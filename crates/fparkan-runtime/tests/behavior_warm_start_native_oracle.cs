using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;

internal static class BehaviorWarmStartOracle {
    private const uint LoadWithAlteredSearchPath = 0x00000008;
    private const string ExpectedBehaviorSha256 = "ddfbdbde07832358fe6e35e01c8dba9b58e7313789498b6049ed51d4ed9baf99";
    private const int WarmStartRva = 0x1adb0;
    private const int GetQueueSlotRva = 0x59110;
    private const int RandomStateRva = 0x63c1c;
    private static uint currentQueueTime;
    private static IntPtr queue;
    private static bool queuePointerMatched;
    private static QueueGetter queueGetter = GetCurrentQueue;
    private static QueueTimeGetter queueTimeGetter = ReadQueueTime;

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr LoadLibraryEx(string fileName, IntPtr file, uint flags);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern bool FreeLibrary(IntPtr module);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern bool VirtualProtect(IntPtr address, UIntPtr size, uint protect, out uint oldProtect);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate IntPtr QueueGetter();
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate uint QueueTimeGetter(IntPtr self);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate void WarmStart(IntPtr self);

    private static IntPtr GetCurrentQueue() { return queue; }
    private static uint ReadQueueTime(IntPtr self) { queuePointerMatched &= self == queue; return currentQueueTime; }

    private static int Main() {
        if (IntPtr.Size != 4) { Console.Error.WriteLine("Build with /platform:x86."); return 2; }
        string path = Environment.GetEnvironmentVariable("FPARKAN_ORIGINAL_BEHAVIOR_DLL");
        if (String.IsNullOrEmpty(path)) path = @"C:\GOG Games\Parkan - Iron Strategy\Behavior.dll";
        if (!File.Exists(path)) { Console.Error.WriteLine("Set FPARKAN_ORIGINAL_BEHAVIOR_DLL to the installed Behavior.dll."); return 3; }
        string hash = Hash(path);
        if (hash != ExpectedBehaviorSha256) { Console.Error.WriteLine("Unexpected Behavior.dll SHA-256: " + hash); return 4; }
        IntPtr module = LoadLibraryEx(path, IntPtr.Zero, LoadWithAlteredSearchPath);
        if (module == IntPtr.Zero) { Console.Error.WriteLine("LoadLibraryEx(Behavior.dll) failed: " + Marshal.GetLastWin32Error()); return 5; }
        IntPtr slot = IntPtr.Add(module, GetQueueSlotRva);
        IntPtr oldQueueGetter = Marshal.ReadIntPtr(slot);
        int oldRandomValue = Marshal.ReadInt32(IntPtr.Add(module, RandomStateRva));
        IntPtr queueVtable = Marshal.AllocHGlobal(0x40);
        queue = Marshal.AllocHGlobal(0x80);
        IntPtr record = Marshal.AllocHGlobal(0x44);
        IntPtr secondRecord = Marshal.AllocHGlobal(0x44);
        IntPtr zeroClockRecord = Marshal.AllocHGlobal(0x44);
        IntPtr wrappingClockRecord = Marshal.AllocHGlobal(0x44);
        uint oldSlotProtection = 0;
        bool slotProtectionChanged = false;
        try {
            Zero(queueVtable, 0x40); Zero(queue, 0x80);
            Zero(record, 0x44); Zero(secondRecord, 0x44); Zero(zeroClockRecord, 0x44); Zero(wrappingClockRecord, 0x44);
            Marshal.WriteIntPtr(queue, 0, queueVtable);
            Marshal.WriteIntPtr(queueVtable, 0x38, Marshal.GetFunctionPointerForDelegate(queueTimeGetter));
            // The synthetic queue controls only GetQueue and the +0x38 clock slot.
            if (!VirtualProtect(slot, new UIntPtr((uint)IntPtr.Size), 0x40, out oldSlotProtection))
                throw new InvalidOperationException("VirtualProtect(GetQueue IAT) failed: " + Marshal.GetLastWin32Error());
            slotProtectionChanged = true;
            Marshal.WriteIntPtr(slot, Marshal.GetFunctionPointerForDelegate(queueGetter));
            WarmStart prepare = (WarmStart)Marshal.GetDelegateForFunctionPointer(IntPtr.Add(module, WarmStartRva), typeof(WarmStart));

            uint rngState = 1;
            Expect(unchecked((uint)oldRandomValue) == rngState, "fresh DLL RNG state starts at the statically observed seed");
            InitializeRecord(record);
            currentQueueTime = 100000;
            queuePointerMatched = true;
            prepare(record);
            Expect(queuePointerMatched, "native clock vslot receives the synthetic queue object");
            uint previousRandomState = rngState;
            rngState = NextRandomState(rngState);
            VerifyFirstPreparation(record, currentQueueTime, previousRandomState, rngState, module);

            uint savedDeadline = ReadWord(record, 0x14);
            for (int offset = 0x30; offset <= 0x40; offset += 4) Marshal.WriteInt32(record, offset, 0x5a5a5a5a);
            currentQueueTime = ReadWord(record, 0x10);
            queuePointerMatched = true;
            prepare(record);
            Expect(queuePointerMatched, "repeat native clock vslot receives the synthetic queue object");
            Expect(ReadWord(record, 0x14) == savedDeadline, "repeat before deadline preserves the deadline");
            Expect(ReadWord(IntPtr.Add(module, RandomStateRva), 0) == rngState, "repeat before deadline consumes no RNG draw");
            for (int offset = 0x30; offset <= 0x40; offset += 4)
                Expect(ReadWord(record, offset) == 0x5a5a5a5a, "nonzero first-time field skips the warm-preparation clear pass");

            InitializeRecord(secondRecord);
            currentQueueTime = 100500;
            queuePointerMatched = true;
            prepare(secondRecord);
            Expect(queuePointerMatched, "second record uses the same synthetic queue");
            previousRandomState = rngState;
            rngState = NextRandomState(rngState);
            VerifyFirstPreparation(secondRecord, currentQueueTime, previousRandomState, rngState, module);

            InitializeRecord(zeroClockRecord);
            currentQueueTime = 0;
            queuePointerMatched = true;
            prepare(zeroClockRecord);
            Expect(queuePointerMatched, "zero-clock record uses the synthetic queue");
            previousRandomState = rngState;
            rngState = NextRandomState(rngState);
            VerifyFirstPreparation(zeroClockRecord, 0, previousRandomState, rngState, module);
            for (int offset = 0x30; offset <= 0x40; offset += 4) Marshal.WriteInt32(zeroClockRecord, offset, 0x6b6b6b6b);
            queuePointerMatched = true;
            prepare(zeroClockRecord);
            Expect(queuePointerMatched, "zero-clock record uses the synthetic queue on repeat");
            previousRandomState = rngState;
            rngState = NextRandomState(rngState);
            VerifyFirstPreparation(zeroClockRecord, 0, previousRandomState, rngState, module);

            InitializeRecord(wrappingClockRecord);
            currentQueueTime = 0xffffff80;
            queuePointerMatched = true;
            prepare(wrappingClockRecord);
            Expect(queuePointerMatched, "wrapping-clock record uses the synthetic queue");
            previousRandomState = rngState;
            rngState = NextRandomState(rngState);
            VerifyFirstPreparation(wrappingClockRecord, currentQueueTime, previousRandomState, rngState, module);
            Expect(ReadWord(wrappingClockRecord, 0x14) < currentQueueTime, "deadline addition wraps as a DWORD near UINT32_MAX");
            Console.WriteLine("original Behavior.dll warm-start oracle passed (RVA 0x1adb0; shared RNG, zero-clock repeat, and DWORD wrap).");
            return 0;
        }
        finally {
            if (slotProtectionChanged) {
                Marshal.WriteIntPtr(slot, oldQueueGetter);
                uint ignored;
                VirtualProtect(slot, new UIntPtr((uint)IntPtr.Size), oldSlotProtection, out ignored);
            }
            Marshal.WriteInt32(IntPtr.Add(module, RandomStateRva), oldRandomValue);
            Marshal.FreeHGlobal(record); Marshal.FreeHGlobal(secondRecord); Marshal.FreeHGlobal(zeroClockRecord); Marshal.FreeHGlobal(wrappingClockRecord);
            Marshal.FreeHGlobal(queue); Marshal.FreeHGlobal(queueVtable);
            GC.KeepAlive(queueGetter); GC.KeepAlive(queueTimeGetter);
            FreeLibrary(module);
        }
    }

    private static uint ReadWord(IntPtr address, int offset) { return unchecked((uint)Marshal.ReadInt32(address, offset)); }
    private static uint NextRandomState(uint state) { return unchecked(state * 0x343fd + 0x269ec3); }
    private static uint ExpectedDeadline(uint queueTime, uint previousRandomState) {
        uint random = (NextRandomState(previousRandomState) >> 16) & 0x7fff;
        uint jitter = ((random & 0xff) * 64) >> 8;
        return unchecked(queueTime + 3 * 64 + jitter);
    }
    private static void InitializeRecord(IntPtr record) {
        Zero(record, 0x44);
        Marshal.WriteInt16(record, 0x18, 3); // Three 64-ms base units.
        Marshal.WriteInt16(record, 0x1a, 1); // One 64-ms jitter range.
        for (int offset = 0x30; offset <= 0x40; offset += 4)
            Marshal.WriteInt32(record, offset, unchecked((int)(0xa0a00000u + (uint)offset)));
    }
    private static void VerifyFirstPreparation(IntPtr record, uint queueTime, uint previousRandomState, uint randomState, IntPtr module) {
        Expect(ReadWord(record, 0x10) == queueTime, "warm preparation stores the queue clock in distributor+0x10");
        Expect(ReadWord(record, 0x14) == ExpectedDeadline(queueTime, previousRandomState), "deadline matches base, configured interval, and native RNG jitter");
        Expect(ReadWord(IntPtr.Add(module, RandomStateRva), 0) == randomState, "shared RNG advances by exactly one LCG draw");
        for (int offset = 0x30; offset <= 0x40; offset += 4)
            Expect(ReadWord(record, offset) == 0, "warm preparation clears distributor field +0x" + offset.ToString("x"));
    }
    private static string Hash(string path) {
        using (SHA256 sha = SHA256.Create()) using (FileStream input = File.OpenRead(path)) {
            byte[] digest = sha.ComputeHash(input); var hex = new StringBuilder();
            foreach (byte value in digest) hex.Append(value.ToString("x2")); return hex.ToString();
        }
    }
    private static void Expect(bool value, string message) { if (!value) throw new InvalidOperationException("original Behavior oracle failed: " + message); }
    private static void Zero(IntPtr buffer, int length) { for (int offset = 0; offset < length; offset++) Marshal.WriteByte(buffer, offset, 0); }
}
