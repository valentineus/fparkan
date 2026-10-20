using System;
using System.IO;
using System.Globalization;
using System.Runtime.InteropServices;
using System.Security.Cryptography;

internal static class Iron3dAnchorOracle {
    private const uint LoadWithAlteredSearchPath = 0x00000008;
    private const uint PageExecuteReadWrite = 0x40;
    private const uint MemCommitReserve = 0x3000;
    private const uint MemRelease = 0x8000;
    private const int CallerRva = 0x392e0;
    private const int GameGlobalRva = 0x10a5f8;
    private const int CreateSuperAiIatRva = 0xe4010;

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr LoadLibraryEx(string path, IntPtr file, uint flags);
    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool FreeLibrary(IntPtr module);
    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern IntPtr VirtualAlloc(IntPtr address, UIntPtr size, uint allocationType, uint protect);
    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool VirtualFree(IntPtr address, UIntPtr size, uint freeType);
    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool VirtualProtect(IntPtr address, UIntPtr size, uint protect, out uint oldProtect);

    [UnmanagedFunctionPointer(CallingConvention.ThisCall)]
    private delegate void CreateAiDelegate(IntPtr record, int clanIndex, uint networkPlayer, int networkFlag);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    private delegate IntPtr CreateSuperAiDelegate(uint p0, uint p1, uint p2, uint clanIndex, uint x, uint y, uint p6, uint p7, uint p8, uint callback);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    private delegate void FpuWordOperation(ref ushort value);

    private static FpuWordOperation ReadControlWord;
    private static uint CapturedX;
    private static uint CapturedY;
    private static ushort CapturedControlWord;
    private static CreateSuperAiDelegate CaptureDelegate;

    private static IntPtr CaptureCreateSuperAi(uint p0, uint p1, uint p2, uint clanIndex, uint x, uint y, uint p6, uint p7, uint p8, uint callback) {
        CapturedX = x;
        CapturedY = y;
        ReadControlWord(ref CapturedControlWord);
        return IntPtr.Zero;
    }

    private static void Require(bool value, string message) {
        if (!value) throw new InvalidOperationException(message);
    }

    private static void Expect(CreateAiDelegate caller, IntPtr record, FpuWordOperation setControlWord, ushort mode, float x, float y, uint expectedX, uint expectedY, string label) {
        ushort requested = mode;
        setControlWord(ref requested);
        Marshal.WriteInt32(record, 4, BitConverter.ToInt32(BitConverter.GetBytes(x), 0));
        Marshal.WriteInt32(record, 8, BitConverter.ToInt32(BitConverter.GetBytes(y), 0));
        CapturedX = 0xdeadbeef;
        CapturedY = 0xdeadbeef;
        CapturedControlWord = 0;
        caller(record, 0, 0, 0);
        string actual = String.Format(CultureInfo.InvariantCulture, "{0}: cw=0x{1:x4} rc={2}, ({3:R}, {4:R}) -> (0x{5:x8}, 0x{6:x8})", label, CapturedControlWord, (CapturedControlWord >> 10) & 3, x, y, CapturedX, CapturedY);
        Console.WriteLine(actual);
        Require(CapturedX == expectedX && CapturedY == expectedY, label + " expected (0x" + expectedX.ToString("x8") + ",0x" + expectedY.ToString("x8") + ")");
        Require(((CapturedControlWord >> 10) & 3) == ((requested >> 10) & 3), label + " x87 RC changed before the native callback");
    }

    private static IntPtr BuildInstruction(byte[] bytes) {
        IntPtr memory = VirtualAlloc(IntPtr.Zero, new UIntPtr(4096), MemCommitReserve, PageExecuteReadWrite);
        if (memory == IntPtr.Zero) throw new InvalidOperationException("VirtualAlloc failed: " + Marshal.GetLastWin32Error());
        Marshal.Copy(bytes, 0, memory, bytes.Length);
        return memory;
    }

    private static void ProtectWrite(IntPtr address, int value, out uint oldProtect) {
        if (!VirtualProtect(address, new UIntPtr(4), PageExecuteReadWrite, out oldProtect)) throw new InvalidOperationException("VirtualProtect failed: " + Marshal.GetLastWin32Error());
        Marshal.WriteInt32(address, value);
    }

    private static void RestoreProtection(IntPtr address, uint oldProtect) {
        uint ignored;
        if (!VirtualProtect(address, new UIntPtr(4), oldProtect, out ignored)) throw new InvalidOperationException("VirtualProtect restore failed: " + Marshal.GetLastWin32Error());
    }

    private static void Main() {
        Require(IntPtr.Size == 4, "This oracle must run as an x86 process");
        string dll = Environment.GetEnvironmentVariable("FPARKAN_ORIGINAL_IRON3D_DLL");
        Require(!String.IsNullOrEmpty(dll) && File.Exists(dll), "Set FPARKAN_ORIGINAL_IRON3D_DLL to the installed iron3d.dll");
        string hash;
        using (SHA256 sha = SHA256.Create()) using (FileStream stream = File.OpenRead(dll)) hash = BitConverter.ToString(sha.ComputeHash(stream)).Replace("-", "").ToLowerInvariant();
        Require(hash == "d1dc4ea8535e2069b0a05c9e8be2724dd438ba44cca7a83007d8b2e50f9e35fa", "unexpected iron3d.dll hash: " + hash);

        IntPtr module = LoadLibraryEx(dll, IntPtr.Zero, LoadWithAlteredSearchPath);
        if (module == IntPtr.Zero) throw new InvalidOperationException("LoadLibraryEx failed: " + Marshal.GetLastWin32Error());
        IntPtr readCode = IntPtr.Zero;
        IntPtr writeCode = IntPtr.Zero;
        IntPtr game = Marshal.AllocHGlobal(0x40);
        IntPtr manager = Marshal.AllocHGlobal(0x40);
        IntPtr record = Marshal.AllocHGlobal(0x68);
        uint oldIatProtection = 0;
        int oldGlobal = Marshal.ReadInt32(IntPtr.Add(module, GameGlobalRva));
        int oldIat = Marshal.ReadInt32(IntPtr.Add(module, CreateSuperAiIatRva));
        bool iatChanged = false;
        bool controlWordCaptured = false;
        ushort originalControlWord = 0;
        FpuWordOperation setControlWord = null;
        try {
            byte[] readBytes = { 0x8b, 0x44, 0x24, 0x04, 0xd9, 0x38, 0xc2, 0x04, 0x00 }; // mov eax,[esp+4]; fnstcw [eax]; ret 4
            byte[] writeBytes = { 0x8b, 0x44, 0x24, 0x04, 0xd9, 0x28, 0xc2, 0x04, 0x00 }; // mov eax,[esp+4]; fldcw [eax]; ret 4
            readCode = BuildInstruction(readBytes);
            writeCode = BuildInstruction(writeBytes);
            ReadControlWord = (FpuWordOperation)Marshal.GetDelegateForFunctionPointer(readCode, typeof(FpuWordOperation));
            setControlWord = (FpuWordOperation)Marshal.GetDelegateForFunctionPointer(writeCode, typeof(FpuWordOperation));
            ReadControlWord(ref originalControlWord);
            controlWordCaptured = true;
            Console.WriteLine("iron3d.dll sha256=" + hash + " loadedBase=0x" + module.ToInt32().ToString("x8") + " defaultCW=0x" + originalControlWord.ToString("x4") + " defaultRC=" + ((originalControlWord >> 10) & 3));

            for (int offset = 0; offset < 0x40; offset += 4) { Marshal.WriteInt32(game, offset, 0); Marshal.WriteInt32(manager, offset, 0); }
            for (int offset = 0; offset < 0x68; offset += 4) Marshal.WriteInt32(record, offset, 0);
            Marshal.WriteInt32(game, 0x1c, manager.ToInt32());
            Marshal.WriteInt32(IntPtr.Add(module, GameGlobalRva), game.ToInt32());

            CaptureDelegate = CaptureCreateSuperAi;
            IntPtr callbackPointer = Marshal.GetFunctionPointerForDelegate(CaptureDelegate);
            IntPtr iat = IntPtr.Add(module, CreateSuperAiIatRva);
            ProtectWrite(iat, callbackPointer.ToInt32(), out oldIatProtection);
            iatChanged = true;

            CreateAiDelegate caller = (CreateAiDelegate)Marshal.GetDelegateForFunctionPointer(IntPtr.Add(module, CallerRva), typeof(CreateAiDelegate));
            ushort nearest = (ushort)(originalControlWord & ~0x0c00);
            ushort down = (ushort)((originalControlWord & ~0x0c00) | 0x0400);
            ushort up = (ushort)((originalControlWord & ~0x0c00) | 0x0800);
            ushort truncate = (ushort)((originalControlWord & ~0x0c00) | 0x0c00);

            Expect(caller, record, setControlWord, nearest, 500.2857f, 751.8217f, 500, 752, "Autodemo anchors / nearest-even");
            Expect(caller, record, setControlWord, nearest, 500.5f, 501.5f, 500, 502, "nearest tie pair");
            Expect(caller, record, setControlWord, down, 500.5f, 501.5f, 500, 501, "down tie pair");
            Expect(caller, record, setControlWord, up, 500.5f, 501.5f, 501, 502, "up tie pair");
            Expect(caller, record, setControlWord, truncate, 500.5f, 501.5f, 500, 501, "truncate tie pair");
            Expect(caller, record, setControlWord, nearest, -0.4f, 10000.4f, 0, 10000, "round-in lower/upper boundary");
            Expect(caller, record, setControlWord, nearest, -0.6f, 10000.6f, 0xffffffff, 10001, "round-out lower/upper boundary");
            Expect(caller, record, setControlWord, nearest, 4294967296.0f, 4294967808.0f, 0, 512, "low-DWORD wrap boundary");
        }
        finally {
            if (iatChanged) {
                IntPtr iat = IntPtr.Add(module, CreateSuperAiIatRva);
                Marshal.WriteInt32(iat, oldIat);
                RestoreProtection(iat, oldIatProtection);
            }
            Marshal.WriteInt32(IntPtr.Add(module, GameGlobalRva), oldGlobal);
            if (controlWordCaptured) setControlWord(ref originalControlWord);
            Marshal.FreeHGlobal(record);
            Marshal.FreeHGlobal(manager);
            Marshal.FreeHGlobal(game);
            if (readCode != IntPtr.Zero) VirtualFree(readCode, UIntPtr.Zero, MemRelease);
            if (writeCode != IntPtr.Zero) VirtualFree(writeCode, UIntPtr.Zero, MemRelease);
            FreeLibrary(module);
        }
    }
}
