using System;
using System.Collections.Generic;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;

internal static class HostCallbackOracle {
    private const uint LoadWithAlteredSearchPath = 0x00000008;
    private const uint MemCommitReserve = 0x00003000;
    private const uint PageExecuteReadWrite = 0x40;
    private const int CallbackRva = 0x611d0;
    private const int GameGlobalRva = 0x10a5f8;
    private const int SoundServerIatRva = 0xe42cc;
    private const string ExpectedSha256 = "d1dc4ea8535e2069b0a05c9e8be2724dd438ba44cca7a83007d8b2e50f9e35fa";

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr LoadLibraryEx(string fileName, IntPtr file, uint flags);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern bool FreeLibrary(IntPtr module);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern IntPtr VirtualAlloc(IntPtr address, UIntPtr size, uint allocationType, uint protect);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern bool VirtualFree(IntPtr address, UIntPtr size, uint freeType);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern bool VirtualProtect(IntPtr address, UIntPtr size, uint newProtect, out uint oldProtect);

    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    private delegate int HostCallback(int gameMode, int command, int payload);

    private static int Main() {
        if (IntPtr.Size != 4) { Console.Error.WriteLine("Build with /platform:x86."); return 2; }
        string dllPath = Environment.GetEnvironmentVariable("FPARKAN_ORIGINAL_IRON3D_DLL");
        if (String.IsNullOrEmpty(dllPath) || !File.Exists(dllPath)) {
            Console.Error.WriteLine("Set FPARKAN_ORIGINAL_IRON3D_DLL to the original GOG iron3d.dll."); return 3;
        }
        string hash;
        using (SHA256 sha = SHA256.Create()) using (FileStream input = File.OpenRead(dllPath)) {
            byte[] digest = sha.ComputeHash(input); StringBuilder hex = new StringBuilder();
            foreach (byte value in digest) hex.Append(value.ToString("x2")); hash = hex.ToString();
        }
        if (hash != ExpectedSha256) { Console.Error.WriteLine("Unexpected iron3d.dll SHA-256: " + hash); return 4; }
        IntPtr module = LoadLibraryEx(dllPath, IntPtr.Zero, LoadWithAlteredSearchPath);
        if (module == IntPtr.Zero) { Console.Error.WriteLine("LoadLibraryEx failed: " + Marshal.GetLastWin32Error()); return 5; }

        IntPtr game = IntPtr.Zero, app = IntPtr.Zero, manager = IntPtr.Zero, sound = IntPtr.Zero, vtable = IntPtr.Zero;
        IntPtr callStub = IntPtr.Zero, returnStub = IntPtr.Zero;
        IntPtr gameGlobal = IntPtr.Zero; int originalGameGlobal = 0; bool gameGlobalPatched = false;
        uint oldProtect = 0; bool iatPatched = false; int originalSoundPointer = 0;
        try {
            game = Marshal.AllocHGlobal(0x200); app = Marshal.AllocHGlobal(0x200); manager = Marshal.AllocHGlobal(0x40);
            sound = Marshal.AllocHGlobal(4); vtable = Marshal.AllocHGlobal(0x14);
            IntPtr observations = Marshal.AllocHGlobal(56);
            try {
                Zero(game, 0x200); Zero(app, 0x200); Zero(manager, 0x40); Zero(sound, 4); Zero(vtable, 0x14); Zero(observations, 56);
                Marshal.WriteInt32(game, 4, app.ToInt32()); Marshal.WriteInt32(game, 0x2c, manager.ToInt32());
                Marshal.WriteInt32(manager, 0x10, IntPtr.Zero.ToInt32());
                Marshal.WriteInt32(sound, vtable.ToInt32());
                IntPtr codePage = VirtualAlloc(IntPtr.Zero, new UIntPtr(0x1000), MemCommitReserve, PageExecuteReadWrite);
                if (codePage == IntPtr.Zero) throw new InvalidOperationException("VirtualAlloc failed: " + Marshal.GetLastWin32Error());
                callStub = codePage;
                byte[] soundMethod = BuildSoundMethod(observations, game, app);
                Marshal.Copy(soundMethod, 0, callStub, soundMethod.Length);
                Marshal.WriteInt32(vtable, 0x10, callStub.ToInt32());
                returnStub = IntPtr.Add(codePage, 0x100);
                byte[] returnCode = { 0xb8, 0, 0, 0, 0, 0xc3 };
                WriteImm32(returnCode, 1, sound.ToInt32()); Marshal.Copy(returnCode, 0, returnStub, returnCode.Length);

                IntPtr iat = IntPtr.Add(module, SoundServerIatRva);
                originalSoundPointer = Marshal.ReadInt32(iat);
                if (!VirtualProtect(iat, new UIntPtr(4), PageExecuteReadWrite, out oldProtect))
                    throw new InvalidOperationException("VirtualProtect(IAT) failed: " + Marshal.GetLastWin32Error());
                Marshal.WriteInt32(iat, returnStub.ToInt32());
                iatPatched = true;
                uint ignored;
                if (!VirtualProtect(iat, new UIntPtr(4), oldProtect, out ignored))
                    throw new InvalidOperationException("VirtualProtect(IAT restore protection) failed: " + Marshal.GetLastWin32Error());

                gameGlobal = IntPtr.Add(module, GameGlobalRva);
                originalGameGlobal = Marshal.ReadInt32(gameGlobal);
                Marshal.WriteInt32(gameGlobal, game.ToInt32());
                gameGlobalPatched = true;
                var callback = (HostCallback)Marshal.GetDelegateForFunctionPointer(IntPtr.Add(module, CallbackRva), typeof(HostCallback));
                RunAllowedFailure(callback, game, app, observations);
                RunFailureRepeat(callback, game, app, observations);
                RunOppositeCompletion(callback, game, app, observations);
                RunCompletionRepeat(callback, game, app, observations);
                RunFailureAfterCompletion(callback, game, app, observations);
                RunAutoDemoGate(callback, game, app, observations);
                RunNonZeroGameMode(callback, game, app, observations);
                RunUnknownPayload(callback, game, app, observations);
                Console.WriteLine("native callback oracle passed; audio endpoint is an isolated no-op recorder");
                return 0;
            }
            finally { Marshal.FreeHGlobal(observations); }
        }
        finally {
            if (gameGlobalPatched) Marshal.WriteInt32(gameGlobal, originalGameGlobal);
            if (iatPatched) {
                IntPtr iat = IntPtr.Add(module, SoundServerIatRva); uint restore;
                if (!VirtualProtect(iat, new UIntPtr(4), PageExecuteReadWrite, out restore))
                    throw new InvalidOperationException("VirtualProtect(IAT cleanup) failed: " + Marshal.GetLastWin32Error());
                Marshal.WriteInt32(iat, originalSoundPointer);
                uint ignored;
                if (!VirtualProtect(iat, new UIntPtr(4), restore, out ignored))
                    throw new InvalidOperationException("VirtualProtect(IAT cleanup restore protection) failed: " + Marshal.GetLastWin32Error());
            }
            if (callStub != IntPtr.Zero) VirtualFree(callStub, UIntPtr.Zero, 0x8000);
            if (vtable != IntPtr.Zero) Marshal.FreeHGlobal(vtable);
            if (sound != IntPtr.Zero) Marshal.FreeHGlobal(sound);
            if (manager != IntPtr.Zero) Marshal.FreeHGlobal(manager);
            if (app != IntPtr.Zero) Marshal.FreeHGlobal(app);
            FreeLibrary(module);
            if (game != IntPtr.Zero) Marshal.FreeHGlobal(game);
        }
    }

    private static byte[] BuildSoundMethod(IntPtr observations, IntPtr game, IntPtr app) {
        var code = new List<byte>();
        code.AddRange(new byte[] { 0x50, 0x52, 0x51, 0x56, 0x57 });
        code.Add(0xa1); AddImm32(code, observations.ToInt32()); code.Add(0x40); code.Add(0xa3); AddImm32(code, observations.ToInt32());
        code.AddRange(new byte[] { 0x8b, 0x74, 0x24, 0x18, 0x8b, 0x06, 0x8b, 0x56, 0x04, 0x2b, 0xd0, 0x89, 0x15 }); AddImm32(code, observations.ToInt32() + 4);
        code.AddRange(new byte[] { 0x83, 0xfa, 0x17, 0x76, 0x09, 0xc6, 0x05 }); AddImm32(code, observations.ToInt32() + 48);
        code.AddRange(new byte[] { 0x01, 0xeb, 0x0f, 0xbf }); AddImm32(code, observations.ToInt32() + 12);
        code.AddRange(new byte[] { 0x8b, 0xca, 0x8b, 0xf0, 0xfc, 0xf3, 0xa4, 0xc6, 0x07, 0x00 });
        code.AddRange(new byte[] { 0x8b, 0x44, 0x24, 0x1c, 0xa3 }); AddImm32(code, observations.ToInt32() + 8);
        code.Add(0xb8); AddImm32(code, game.ToInt32());
        code.AddRange(new byte[] { 0x8b, 0x40, 0x08, 0xa3 }); AddImm32(code, observations.ToInt32() + 40);
        code.Add(0xb8); AddImm32(code, app.ToInt32());
        code.AddRange(new byte[] { 0x0f, 0xb6, 0x80, 0x57, 0x01, 0x00, 0x00, 0xa3 }); AddImm32(code, observations.ToInt32() + 44);
        code.AddRange(new byte[] { 0x5f, 0x5e, 0x59, 0x5a, 0x58, 0xc2, 0x08, 0x00 });
        return code.ToArray();
    }

    private static void RunAllowedFailure(HostCallback callback, IntPtr game, IntPtr app, IntPtr obs) {
        Setup(game, app, obs, 0, 5, 0x7f); int result = callback(0, 0, 0);
        Expect(result == 1 && Marshal.ReadInt32(game, 8) == 1 && Marshal.ReadByte(app, 0x157) == 0, "failure state writes");
        Expect(Marshal.ReadInt32(obs, 0) == 1 && Marshal.ReadInt32(obs, 40) == 5 && Marshal.ReadInt32(obs, 44) == 0x7f, "failure sound before writes");
        Console.WriteLine("fail: status=1 mission_flag=0 sound={0}", SoundId(obs));
        Expect(SoundId(obs) == "VOICE_MISSION_FAIL" && Marshal.ReadInt32(obs, 8) == 0, "failure voice id and argument");
    }
    private static void RunFailureRepeat(HostCallback callback, IntPtr game, IntPtr app, IntPtr obs) {
        Marshal.WriteByte(app, 0x157, 0x7f);
        int result = callback(0, 0, 0);
        Expect(result == 1 && Marshal.ReadInt32(game, 8) == 1 && Marshal.ReadByte(app, 0x157) == 0x7f && Marshal.ReadInt32(obs, 0) == 1, "repeat failure preserves mismatched mission flag");
    }
    private static void RunOppositeCompletion(HostCallback callback, IntPtr game, IntPtr app, IntPtr obs) {
        int result = callback(0, 0, 1); Expect(result == 1 && Marshal.ReadInt32(game, 8) == 0 && Marshal.ReadByte(app, 0x157) == 1 && Marshal.ReadInt32(obs, 0) == 2, "completion after failure");
        Expect(Marshal.ReadInt32(obs, 40) == 1 && Marshal.ReadInt32(obs, 44) == 0x7f && SoundId(obs) == "VOICE_MISSION_COMPLETE" && Marshal.ReadInt32(obs, 8) == 0, "completion sound before writes");
    }
    private static void RunCompletionRepeat(HostCallback callback, IntPtr game, IntPtr app, IntPtr obs) {
        Marshal.WriteByte(app, 0x157, 0x7f);
        int result = callback(0, 0, 1);
        Expect(result == 1 && Marshal.ReadInt32(game, 8) == 0 && Marshal.ReadByte(app, 0x157) == 0x7f && Marshal.ReadInt32(obs, 0) == 2, "repeat completion preserves mismatched mission flag");
    }
    private static void RunFailureAfterCompletion(HostCallback callback, IntPtr game, IntPtr app, IntPtr obs) {
        int result = callback(0, 0, 0);
        Expect(result == 1 && Marshal.ReadInt32(game, 8) == 1 && Marshal.ReadByte(app, 0x157) == 0 && Marshal.ReadInt32(obs, 0) == 3, "failure after completion");
        Expect(Marshal.ReadInt32(obs, 40) == 0 && Marshal.ReadInt32(obs, 44) == 0x7f && SoundId(obs) == "VOICE_MISSION_FAIL" && Marshal.ReadInt32(obs, 8) == 0, "reverse transition sound before writes");
        Console.WriteLine("fail-after-complete: status=1 mission_flag=0 sound={0}", SoundId(obs));
    }
    private static void RunAutoDemoGate(HostCallback callback, IntPtr game, IntPtr app, IntPtr obs) {
        Setup(game, app, obs, 1, 5, 0x7f); callback(0, 0, 0);
        Expect(Marshal.ReadInt32(game, 8) == 5 && Marshal.ReadByte(app, 0x157) == 0x7f && Marshal.ReadInt32(obs, 0) == 0, "AutoDemo gate suppresses callback");
    }
    private static void RunNonZeroGameMode(HostCallback callback, IntPtr game, IntPtr app, IntPtr obs) {
        Setup(game, app, obs, 0, 5, 0x7f); callback(1, 0, 0);
        Expect(Marshal.ReadInt32(game, 8) == 5 && Marshal.ReadByte(app, 0x157) == 0x7f && Marshal.ReadInt32(obs, 0) == 0, "nonzero callback mode suppressed");
    }
    private static void RunUnknownPayload(HostCallback callback, IntPtr game, IntPtr app, IntPtr obs) {
        Setup(game, app, obs, 0, 5, 0x7f); callback(0, 0, 2);
        Expect(Marshal.ReadInt32(game, 8) == 5 && Marshal.ReadByte(app, 0x157) == 0x7f && Marshal.ReadInt32(obs, 0) == 0, "unknown payload suppressed");
    }
    private static void Setup(IntPtr game, IntPtr app, IntPtr obs, byte gate, int status, byte missionFlag) {
        Marshal.WriteByte(game, 0xe5, gate); Marshal.WriteInt32(game, 8, status); Marshal.WriteByte(app, 0x157, missionFlag);
        Zero(obs, 56);
    }
    private static void Expect(bool value, string description) { if (!value) throw new InvalidOperationException("native oracle assertion failed: " + description); }
    private static string SoundId(IntPtr observations) {
        int length = Marshal.ReadInt32(observations, 4);
        if (Marshal.ReadByte(observations, 48) != 0) throw new InvalidOperationException("native sound identifier exceeded recorder capacity");
        if (length < 0 || length > 23) throw new InvalidOperationException("unexpected sound identifier length: " + length);
        byte[] bytes = new byte[length]; Marshal.Copy(IntPtr.Add(observations, 12), bytes, 0, length);
        return Encoding.ASCII.GetString(bytes);
    }
    private static void Zero(IntPtr buffer, int length) { for (int offset = 0; offset < length; offset++) Marshal.WriteByte(buffer, offset, 0); }
    private static void AddImm32(List<byte> code, int value) { byte[] bytes = BitConverter.GetBytes(value); code.AddRange(bytes); }
    private static void WriteImm32(byte[] bytes, int offset, int value) { byte[] immediate = BitConverter.GetBytes(value); Buffer.BlockCopy(immediate, 0, bytes, offset, 4); }
}
