using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Globalization;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;

// Small, x86-only, one-shot debugger for the isolated GOG-compatible scratch
// process. It records verified camera/projection fields and a D3D7 frame. Pixel
// readback calls the target's verified D3D7 COM methods on its stopped render
// thread through a bounded temporary stub; it does not load game DLLs.
internal static partial class NativeFrameCapture
{
    private const uint DbgContinue = 0x00010002;
    private const uint DbgExceptionNotHandled = 0x80010001;
    private const uint ExceptionDebugEvent = 1;
    private const uint CreateProcessDebugEvent = 3;
    private const uint CreateThreadDebugEvent = 2;
    private const uint ExitThreadDebugEvent = 4;
    private const uint ExitProcessDebugEvent = 5;
    private const uint LoadDllDebugEvent = 6;
    private const uint PageExecuteReadWrite = 0x40;
    private const uint ThreadGetContext = 0x0008;
    private const uint ThreadSetContext = 0x0010;
    private const int ContextEipOffset = 184;
    private const int ContextEbxOffset = 164;
    private const int ContextEdxOffset = 168;
    private const int ContextEcxOffset = 172;
    private const int ContextEsiOffset = 160;
    private const int ContextEbpOffset = 180;
    private const int ContextEspOffset = 196;
    private const int PresentCallArgumentsBytes = 24;
    private const int X86ContextSize = 716;
    private const uint MemCommit = 0x1000;
    private const uint PageNoAccess = 0x01;
    private const uint PageReadOnly = 0x02;
    private const uint PageReadWrite = 0x04;
    private const uint PageWriteCopy = 0x08;
    private const uint PageExecuteRead = 0x20;
    private const uint PageExecuteWriteCopy = 0x80;
    private const string GameDirectory = @"target\shadow-probe\Parkan - Iron Strategy";
    private const string GameExe = "iron_3d.exe";
    private const string World3DName = "World3D.dll";
    private const string TerrainName = "Terrain.dll";
    private const string Ngi32Name = "Ngi32.dll";
    private const string Iron3dName = "iron3d.dll";
    private const uint RenderGameRva = 0x13BD0;
    private const uint ProjectionSnapshotRva = 0x13CE4;
    private const uint RenderReturnRva = 0x13D7B;
    private const uint AtmospherePhaseRva = 0x421DC;
    private const uint PresentBoundaryRva = 0x6E1B;
    private const uint RelocatedGlobalRva = 0x79518C;
    private const uint ExternalCameraVtableRva = 0x665B4;
    private const uint MissionFactoryRva = 0xA1FE1;
    private const uint MissionVtableCallRva = 0xA1FF0;
    private const uint MissionImportThunkRva = 0xCD01C;
    private const int MissionProbeAttemptLimit = 8;
    private const int FrameCaptureTimeoutSeconds = 300;

    private static readonly Dictionary<string, string> ExpectedSha256 =
        new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase)
        {
            { "iron_3d.exe", "F476AF85C034A4B4F34F49D0806E4DFF397B5DA0EE26D382A7674231144979F7" },
            { "World3D.dll", "17E4A3089B2583A8CF2356C9DB0390B1ABA138356A09130D79B4E7E4791DA61E" },
            { "Ngi32.dll", "BAB9840D94F4E4E74FFC26677724FA896CF4823845504D09A9E025F80016EDF5" },
            { "Terrain.dll", "AF87D1B2E728A0BE73C52BE3B44CC196AB46DA7799F25A15D40F8C9B0B425EAD" },
            { "iron3d.dll", "D1DC4EA8535E2069B0A05C9E8BE2724DD438BA44CCA7A83007D8B2E50F9E35FA" }
        };

    private static string _logPath;

    private sealed class BreakpointInfo
    {
        public string Name;
        public uint Address;
        public byte OriginalByte;
        public bool Armed;
    }

    private sealed class FrameSnapshot
    {
        public uint CameraGeneration;
        public uint CameraThreadId;
        public CameraReadback Camera;
        public uint ProjectionGeneration;
        public uint ProjectionThreadId;
        public ProjectionReadback Projection;
        public uint CameraWorldGameTimeWord;
        public bool CameraWorldGameTimeWordReadable;
        public uint WorldGameTimeWord;
        public bool WorldGameTimeWordReadable;
        public AtmosphereReadback Atmosphere;
        public readonly Dictionary<uint, AtmosphereReadback> AtmosphereByThread = new Dictionary<uint, AtmosphereReadback>();
        public string MissionPath;
        public string MissionEvidence;
        public bool SelectedCameraRequested;
        public SelectedCameraState SelectedCamera;
    }

    private sealed class AtmosphereReadback
    {
        public uint SampleThreadId;
        public uint CameraGenerationAtSample;
        public uint CameraThreadIdAtSample;
        public uint AtmosphereObject;
        public uint RawClock;
        public uint PhaseMilliseconds;
        public uint Origin;
        public uint PeriodMilliseconds;
        public uint RecomputedPhaseMilliseconds;
        public uint WorldGameTimeWord;
        public bool WorldGameTimeWordReadable;
        public string TerrainModuleSha256;
        public bool TerrainModuleHashVerified;
        public bool ArithmeticVerified;
        public bool WorldTimeMatchesRawClock;
        public uint CandidateRenderGeneration;
        public uint CandidateRenderThreadId;
        public uint MatchedRenderGeneration;
        public uint MatchedRenderThreadId;
        public bool SameGenerationVerified;
    }

    private sealed class PendingStep
    {
        public BreakpointInfo Breakpoint;
        public uint ThreadId;
        public DateTime DeadlineUtc;
        public bool RearmOnComplete;
    }

    private sealed class SelectedCameraState
    {
        public SelectedCameraInput Input;
        public uint CandidateCameraPointer;
        public uint CandidateThreadId;
        public uint CandidateGeneration;
        public bool CandidateProjectionVerified;
        public bool Applied;
        public bool ProjectionMatchesInput;
        public bool Restored;
        public bool RestoreVerified;
        public bool MatrixIntactAtReturn;
        public bool PixelAttributionInvalidated;
        public string PixelAttributionFailure;
        public bool ProjectionGateDiagnosticLogged;
        public bool ReturnGateDiagnosticLogged;
        public DateTime RestoreDeadlineUtc;
        public uint CameraPointer;
        public uint ThreadId;
        public uint Generation;
        public uint EntryEsp;
        public uint FunctionStackEsp;
        public uint ReturnAddress;
        public byte[] OriginalMatrixBytes;
        public string OriginalMatrixSha256;
    }

    private enum RemoteReadbackPhase { Acquire, Cleanup }

    private sealed class RemoteReadbackSession
    {
        public RemoteCode Acquire;
        public RemoteCode Cleanup;
        public uint Region;
        public uint ThreadId;
        public uint SafeEsp;
        public byte[] OriginalContext;
        public byte[] PresentStackArguments;
        public bool PresentStackArgumentsIntact;
        public bool RemoteContextIntact;
        public BreakpointInfo PresentBreakpoint;
        public FrameSnapshot Frame;
        public uint Generation;
        public string OutputJson;
        public string OutputPng;
        public DateTime DeadlineUtc;
        public RemoteReadbackPhase Phase;
        public SurfaceReadback Surface;
        public string PixelFailure;
        public uint Status;
        public uint ReleaseResult;
        public uint GetHr;
        public uint LockHr;
        public uint UnlockHr;
    }

    [StructLayout(LayoutKind.Explicit, Size = X86ContextSize)]
    private struct X86ContextLayout
    {
        [FieldOffset(ContextEipOffset)] public uint Eip;
        [FieldOffset(ContextEsiOffset)] public uint Esi;
        [FieldOffset(ContextEbpOffset)] public uint Ebp;
        [FieldOffset(ContextEspOffset)] public uint Esp;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct MemoryBasicInformation
    {
        public IntPtr BaseAddress;
        public IntPtr AllocationBase;
        public uint AllocationProtect;
        public UIntPtr RegionSize;
        public uint State;
        public uint Protect;
        public uint Type;
    }

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool DebugActiveProcess(uint processId);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool DebugActiveProcessStop(uint processId);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool DebugSetProcessKillOnExit(bool killOnExit);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool WaitForDebugEvent(IntPtr debugEvent, uint milliseconds);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool ContinueDebugEvent(uint processId, uint threadId, uint continueStatus);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool ReadProcessMemory(IntPtr process, IntPtr address,
        [Out] byte[] buffer, UIntPtr size, out UIntPtr bytesRead);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool WriteProcessMemory(IntPtr process, IntPtr address,
        byte[] buffer, UIntPtr size, out UIntPtr bytesWritten);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool VirtualProtectEx(IntPtr process, IntPtr address,
        UIntPtr size, uint newProtection, out uint oldProtection);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool FlushInstructionCache(IntPtr process, IntPtr address, UIntPtr size);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern IntPtr OpenThread(uint desiredAccess, bool inheritHandle, uint threadId);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool GetThreadContext(IntPtr thread, IntPtr context);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool SetThreadContext(IntPtr thread, IntPtr context);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern IntPtr OpenProcess(uint desiredAccess, bool inheritHandle, uint processId);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern UIntPtr VirtualQueryEx(IntPtr process, IntPtr address,
        out MemoryBasicInformation information, UIntPtr length);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool CloseHandle(IntPtr handle);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern uint WaitForSingleObject(IntPtr handle, uint milliseconds);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool GetExitCodeProcess(IntPtr process, out uint exitCode);

    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    private static extern uint GetFinalPathNameByHandleW(IntPtr file, StringBuilder path,
        uint pathLength, uint flags);

    [DllImport("psapi.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    private static extern uint GetModuleFileNameExW(IntPtr process, IntPtr module,
        StringBuilder fileName, uint size);

    private static IntPtr Ptr(uint value) { return new IntPtr(unchecked((int)value)); }

    private static void Log(string text)
    {
        if (_logPath != null)
            File.AppendAllText(_logPath, DateTime.UtcNow.ToString("o", CultureInfo.InvariantCulture)
                + " " + text + Environment.NewLine, Encoding.UTF8);
    }

    private static string NormalizePath(string path)
    {
        if (path.StartsWith(@"\\?\UNC\", StringComparison.OrdinalIgnoreCase)) return @"\\" + path.Substring(8);
        if (path.StartsWith(@"\\?\", StringComparison.OrdinalIgnoreCase)) return path.Substring(4);
        return Path.GetFullPath(path);
    }

    private static string FindRepositoryRoot()
    {
        DirectoryInfo directory = new DirectoryInfo(AppDomain.CurrentDomain.BaseDirectory);
        while (directory != null)
        {
            if (File.Exists(Path.Combine(directory.FullName, "Cargo.toml"))
                && Directory.Exists(Path.Combine(directory.FullName, ".git"))) return directory.FullName;
            directory = directory.Parent;
        }
        directory = new DirectoryInfo(Environment.CurrentDirectory);
        while (directory != null)
        {
            if (File.Exists(Path.Combine(directory.FullName, "Cargo.toml"))) return directory.FullName;
            directory = directory.Parent;
        }
        throw new InvalidOperationException("Run from this repository or place the EXE beneath it.");
    }

    private static string HashFile(string path)
    {
        using (SHA256 sha = SHA256.Create())
        using (FileStream stream = File.OpenRead(path))
        {
            byte[] hash = sha.ComputeHash(stream);
            StringBuilder result = new StringBuilder(hash.Length * 2);
            for (int i = 0; i < hash.Length; i++) result.Append(hash[i].ToString("X2"));
            return result.ToString();
        }
    }

    private static string VerifyScratchFiles(string repositoryRoot)
    {
        string directory = Path.GetFullPath(Path.Combine(repositoryRoot, GameDirectory));
        foreach (KeyValuePair<string, string> expected in ExpectedSha256)
        {
            string path = Path.Combine(directory, expected.Key);
            if (!File.Exists(path)) throw new FileNotFoundException("Missing scratch file: " + path, path);
            string observed = HashFile(path);
            if (!String.Equals(observed, expected.Value, StringComparison.OrdinalIgnoreCase))
                throw new InvalidOperationException("SHA-256 mismatch for " + path + ": " + observed);
        }
        return Path.Combine(directory, GameExe);
    }

    private static bool Read(IntPtr process, uint address, byte[] bytes)
    {
        UIntPtr count;
        return address != 0 && ReadProcessMemory(process, Ptr(address), bytes,
            new UIntPtr((uint)bytes.Length), out count) && count.ToUInt32() == (uint)bytes.Length;
    }

    private static uint ReadU32(IntPtr process, uint address)
    {
        byte[] bytes = new byte[4];
        return Read(process, address, bytes) ? BitConverter.ToUInt32(bytes, 0) : 0;
    }

    private static bool IsFinite(float value)
    {
        return !Single.IsNaN(value) && !Single.IsInfinity(value);
    }

    private static bool WriteByte(IntPtr process, uint address, byte value)
    {
        uint oldProtection;
        if (!VirtualProtectEx(process, Ptr(address), new UIntPtr(1), PageExecuteReadWrite, out oldProtection))
            return false;
        UIntPtr written;
        bool result = WriteProcessMemory(process, Ptr(address), new byte[] { value }, new UIntPtr(1), out written)
            && written.ToUInt32() == 1;
        uint ignored;
        VirtualProtectEx(process, Ptr(address), new UIntPtr(1), oldProtection, out ignored);
        FlushInstructionCache(process, Ptr(address), new UIntPtr(1));
        return result;
    }

    private static string PathForHandle(IntPtr file)
    {
        if (file == IntPtr.Zero) return "";
        StringBuilder buffer = new StringBuilder(1024);
        uint length = GetFinalPathNameByHandleW(file, buffer, (uint)buffer.Capacity, 0);
        return length == 0 || length >= buffer.Capacity ? "" : NormalizePath(buffer.ToString());
    }

    private static string ModulePath(IntPtr process, uint moduleBase)
    {
        StringBuilder buffer = new StringBuilder(1024);
        uint length = GetModuleFileNameExW(process, Ptr(moduleBase), buffer, (uint)buffer.Capacity);
        return length == 0 || length >= buffer.Capacity ? "" : NormalizePath(buffer.ToString());
    }

    private static bool VerifyRenderEntry(IntPtr process, uint moduleBase, out byte firstByte, out string evidence)
    {
        firstByte = 0;
        evidence = "";
        byte[] code = new byte[15];
        uint address = unchecked(moduleBase + RenderGameRva);
        if (!Read(process, address, code))
        {
            evidence = "World3D+0x13BD0 unreadable";
            return false;
        }
        uint relocatedOperand = BitConverter.ToUInt32(code, 5);
        bool match = code[0] == 0x83 && code[1] == 0xEC && code[2] == 0x64
            && code[3] == 0xC7 && code[4] == 0x05
            && relocatedOperand == unchecked(moduleBase + RelocatedGlobalRva)
            && code[9] == 0 && code[10] == 0 && code[11] == 0 && code[12] == 0
            && code[13] == 0x53 && code[14] == 0x56;
        evidence = "base=0x" + moduleBase.ToString("X8") + " RVA=0x13BD0 bytes="
            + BitConverter.ToString(code) + " relocOperand=0x" + relocatedOperand.ToString("X8")
            + " expectedOperand=0x" + unchecked(moduleBase + RelocatedGlobalRva).ToString("X8");
        firstByte = code[0];
        return match;
    }

    private static bool VerifyRenderReturnBoundary(IntPtr process, uint moduleBase,
        out byte firstByte, out string evidence)
    {
        firstByte = 0;
        byte[] expected = new byte[] { 0x5F, 0x5E, 0x5B, 0x83, 0xC4, 0x64, 0xC2, 0x04, 0x00 };
        byte[] actual = new byte[expected.Length];
        uint address = unchecked(moduleBase + RenderReturnRva);
        if (!ReadExact(process, address, actual)) { evidence = "return-epilogue bytes unreadable"; return false; }
        if (!ByteArraysEqual(actual, expected))
        {
            evidence = "return-epilogue mismatch at 0x" + address.ToString("X8")
                + " expected=" + BitConverter.ToString(expected) + " actual=" + BitConverter.ToString(actual);
            return false;
        }
        firstByte = actual[0];
        evidence = "verified one-byte pop-edi at World3D+0x13D7B followed by pop esi/pop ebx/add esp,64h/ret 4";
        return true;
    }

    private static bool VerifyD3d7GetRenderTarget(IntPtr process, uint ngiBase,
        out uint device, out uint getRenderTarget, out string evidence)
    {
        device = ReadU32(process, unchecked(ngiBase + 0x3A488));
        getRenderTarget = 0;
        if (device == 0)
        {
            evidence = "Ngi32 D3D7 device global is null";
            return false;
        }
        uint vtable = ReadU32(process, device);
        if (vtable == 0 || !ReadExact(process, unchecked(vtable + 0x24), new byte[4]))
        {
            evidence = "D3D7 device vtable or GetRenderTarget slot is unreadable";
            return false;
        }
        getRenderTarget = ReadU32(process, unchecked(vtable + 0x24));
        if (getRenderTarget == 0)
        {
            evidence = "D3D7 GetRenderTarget slot is null";
            return false;
        }
        MemoryBasicInformation memory;
        UIntPtr queried = VirtualQueryEx(process, Ptr(getRenderTarget), out memory,
            new UIntPtr((uint)Marshal.SizeOf(typeof(MemoryBasicInformation))));
        uint protection = memory.Protect & 0xFF;
        bool executable = memory.State == 0x1000
            && (protection == 0x10 || protection == 0x20 || protection == 0x40 || protection == 0x80);
        evidence = "device=0x" + device.ToString("X8") + " vtable=0x" + vtable.ToString("X8")
            + " GetRenderTarget=0x" + getRenderTarget.ToString("X8") + " pageState=0x"
            + memory.State.ToString("X8") + " protect=0x" + memory.Protect.ToString("X8");
        return queried.ToUInt32() != 0 && executable;
    }

    private static string JsonString(string value)
    {
        StringBuilder result = new StringBuilder();
        result.Append('"');
        for (int i = 0; i < value.Length; i++)
        {
            char c = value[i];
            if (c == '"') result.Append("\\\"");
            else if (c == '\\') result.Append("\\\\");
            else if (c == '\n') result.Append("\\n");
            else if (c == '\r') result.Append("\\r");
            else if (c == '\t') result.Append("\\t");
            else if (c < 0x20) result.Append("\\u").Append(((int)c).ToString("X4"));
            else result.Append(c);
        }
        result.Append('"');
        return result.ToString();
    }

    private static bool VerifyProjectionBoundary(IntPtr process, uint world3dBase, out byte firstByte, out string evidence)
    {
        const uint projectionGlobalRva = 0x795170;
        firstByte = 0;
        byte[] code = new byte[11];
        if (!Read(process, unchecked(world3dBase + ProjectionSnapshotRva), code))
        {
            evidence = "projection boundary unreadable";
            return false;
        }
        uint relocatedGlobal = BitConverter.ToUInt32(code, 2);
        bool match = code[0] == 0x8B && code[1] == 0x0D
            && relocatedGlobal == unchecked(world3dBase + projectionGlobalRva)
            && code[6] == 0x8B && code[7] == 0x11
            && code[8] == 0xFF && code[9] == 0x52 && code[10] == 0x34;
        firstByte = code[0];
        evidence = "base=0x" + world3dBase.ToString("X8") + " RVA=0x13CE4 bytes="
            + BitConverter.ToString(code) + " global=0x" + relocatedGlobal.ToString("X8");
        return match;
    }

    private static bool VerifyAtmospherePhaseBoundary(IntPtr process, uint terrainBase,
        out byte firstByte, out string evidence)
    {
        firstByte = 0;
        byte[] code = new byte[15];
        uint address = unchecked(terrainBase + AtmospherePhaseRva - 12);
        if (!Read(process, address, code))
        {
            evidence = "Terrain+0x421DC phase boundary unreadable";
            return false;
        }
        byte[] expected = new byte[] {
            0x8B, 0xC5,                         // mov eax, ebp (raw clock)
            0x2B, 0xC1,                         // sub eax, ecx (origin from [esi+0x144])
            0x33, 0xD2,                         // xor edx, edx
            0xF7, 0xB6, 0x50, 0x01, 0x00, 0x00, // div dword ptr [esi+0x150] (phase remainder in edx)
            0x57,                               // push edi (breakpoint before phase is consumed)
            0x8B, 0xFA                          // mov edi, edx
        };
        bool match = ByteArraysEqual(code, expected);
        firstByte = code[12];
        evidence = "base=0x" + terrainBase.ToString("X8") + " RVA=0x421DC bytes="
            + BitConverter.ToString(code) + " rawClock=EBP origin=[ESI+0x144]"
            + " period=[ESI+0x150] remainder=EDX";
        return match && firstByte == 0x57;
    }

    private static bool VerifyMissionIdentityBoundary(IntPtr process, uint iron3dBase,
        out byte firstByte, out string evidence)
    {
        firstByte = 0;
        byte[] code = new byte[18];
        if (!Read(process, unchecked(iron3dBase + MissionFactoryRva), code))
        {
            evidence = "iron3d mission factory/call boundary unreadable";
            return false;
        }
        firstByte = code[15];
        int displacement = BitConverter.ToInt32(code, 1);
        uint factoryTarget = unchecked(iron3dBase + MissionFactoryRva + 5 + (uint)displacement);
        bool match = code[0] == 0xE8
            && factoryTarget == unchecked(iron3dBase + MissionImportThunkRva)
            && code[5] == 0x8B && code[6] == 0x54 && code[7] == 0x24 && code[8] == 0x40
            && code[9] == 0x8B && code[10] == 0xD8
            && code[11] == 0x8B && code[12] == 0x0B
            && code[13] == 0x52 && code[14] == 0x53
            && code[15] == 0xFF && code[16] == 0x51 && code[17] == 0x08;
        evidence = "base=0x" + iron3dBase.ToString("X8") + " factoryRva=0xA1FE1 bytes="
            + BitConverter.ToString(code) + " factoryTarget=0x" + factoryTarget.ToString("X8")
            + " vtableCallRva=0xA1FF0";
        return match;
    }

    private static bool IsReadableProtection(uint protection)
    {
        uint basic = protection & 0xFF;
        return basic != PageNoAccess && (protection & PageGuard) == 0
            && (basic == PageReadOnly || basic == PageReadWrite || basic == PageWriteCopy
                || basic == PageExecuteRead || basic == PageExecuteReadWrite || basic == PageExecuteWriteCopy);
    }

    private static bool TryReadReadableRegion(IntPtr process, uint address, int maxBytes,
        out byte[] bytes, out string evidence)
    {
        bytes = null;
        evidence = "unreadable";
        if (address < 0x10000 || maxBytes <= 0) { evidence = "invalid address or length"; return false; }
        MemoryBasicInformation memory;
        UIntPtr queried = VirtualQueryEx(process, Ptr(address), out memory,
            new UIntPtr((uint)Marshal.SizeOf(typeof(MemoryBasicInformation))));
        uint baseAddress = unchecked((uint)memory.BaseAddress.ToInt32());
        uint regionSize = memory.RegionSize.ToUInt32();
        ulong regionEnd = (ulong)baseAddress + regionSize;
        uint protection = memory.Protect;
        if (queried.ToUInt32() == 0 || memory.State != MemCommit || !IsReadableProtection(protection)
            || address < baseAddress || (ulong)address >= regionEnd)
        {
            evidence = "VirtualQueryEx rejected addr=0x" + address.ToString("X8")
                + " state=0x" + memory.State.ToString("X8") + " protect=0x" + protection.ToString("X8");
            return false;
        }
        int count = (int)Math.Min((ulong)maxBytes, regionEnd - address);
        if (count <= 0) { evidence = "empty readable region"; return false; }
        byte[] result = new byte[count];
        if (!Read(process, address, result))
        {
            evidence = "ReadProcessMemory failed for addr=0x" + address.ToString("X8") + " bytes=" + count;
            return false;
        }
        bytes = result;
        evidence = "addr=0x" + address.ToString("X8") + " bytes=" + count
            + " state=0x" + memory.State.ToString("X8") + " protect=0x" + protection.ToString("X8");
        return true;
    }

    private static string DecodeAsciiMissionCandidate(byte[] bytes, int start, int maxLength)
    {
        if (bytes == null || start < 0 || start >= bytes.Length) return null;
        int end = Math.Min(bytes.Length, start + maxLength);
        StringBuilder text = new StringBuilder();
        for (int i = start; i < end; i++)
        {
            byte value = bytes[i];
            if (value == 0) break;
            if (value < 0x20 || value > 0x7E) return null;
            text.Append((char)value);
        }
        string candidate = text.ToString().Trim();
        return IsMissionPathCandidate(candidate) ? candidate : null;
    }

    private static bool IsMissionPathCandidate(string candidate)
    {
        if (String.IsNullOrEmpty(candidate) || candidate.Length > 512) return false;
        string lower = candidate.ToLowerInvariant().Replace('/', '\\');
        return lower.Contains("missions\\") && (lower.Contains(".tma") || lower.Contains(".mis")
            || lower.Contains("autodemo") || lower.Contains("\\data"));
    }

    private static string TryReadMissionPathArgument(IntPtr process, uint argument, out string evidence)
    {
        evidence = "argument pointer is not a verified mission path";
        if (argument < 0x10000) return null;

        byte[] objectBytes;
        string objectEvidence;
        if (!TryReadReadableRegion(process, argument, 64, out objectBytes, out objectEvidence))
        {
            evidence = "argument=" + objectEvidence;
            return null;
        }
        for (int i = 0; i < objectBytes.Length; i++)
        {
            string inline = DecodeAsciiMissionCandidate(objectBytes, i, 512);
            if (inline != null)
            {
                evidence = "mission path found inline in call argument (" + objectEvidence + ")";
                return inline;
            }
        }

        // The callsite passes a string object by pointer. Try the observed word
        // fields as readable C-string pointers without assuming a string ABI.
        int pointerWords = Math.Min(8, objectBytes.Length / 4);
        for (int i = 0; i < pointerWords; i++)
        {
            uint pointer = BitConverter.ToUInt32(objectBytes, i * 4);
            byte[] pointed;
            string pointedEvidence;
            if (!TryReadReadableRegion(process, pointer, 512, out pointed, out pointedEvidence)) continue;
            string candidate = DecodeAsciiMissionCandidate(pointed, 0, 512);
            if (candidate == null) continue;
            evidence = "mission path found through argument word +0x" + (i * 4).ToString("X2")
                + " (" + pointedEvidence + ")";
            return candidate;
        }

        evidence = "argument object=" + objectEvidence + " raw64=" + BitConverter.ToString(objectBytes);
        return null;
    }

    private static bool CaptureMissionIdentityAtCall(IntPtr process, uint iron3dBase,
        byte[] context, uint threadId, out string path, out string evidence)
    {
        path = null;
        evidence = "mission call arguments were not verified";
        if (context == null || context.Length != X86ContextSize
            || BitConverter.ToUInt32(context, ContextEipOffset) != unchecked(iron3dBase + MissionVtableCallRva + 1))
        {
            evidence = "iron3d mission vtable-call breakpoint context mismatch";
            return false;
        }
        uint ebx = BitConverter.ToUInt32(context, ContextEbxOffset);
        uint edx = BitConverter.ToUInt32(context, ContextEdxOffset);
        uint ecx = BitConverter.ToUInt32(context, ContextEcxOffset);
        uint esp = BitConverter.ToUInt32(context, ContextEspOffset);
        byte[] args = new byte[8];
        if (ebx == 0 || edx == 0 || ecx == 0 || !Read(process, esp, args))
        {
            evidence = "mission vtable-call registers or stack arguments unreadable";
            return false;
        }
        uint stackThis = BitConverter.ToUInt32(args, 0);
        uint stackString = BitConverter.ToUInt32(args, 4);
        uint vtable = ReadU32(process, ebx);
        uint method = ReadU32(process, unchecked(vtable + 8));
        MemoryBasicInformation methodMemory;
        UIntPtr methodQuery = VirtualQueryEx(process, Ptr(method), out methodMemory,
            new UIntPtr((uint)Marshal.SizeOf(typeof(MemoryBasicInformation))));
        uint methodProtection = methodMemory.Protect & 0xFF;
        bool executableMethod = methodQuery.ToUInt32() != 0 && methodMemory.State == MemCommit
            && (methodProtection == 0x10 || methodProtection == 0x20
                || methodProtection == 0x40 || methodProtection == 0x80);
        Log("MISSION_VTABLE_CALL tid=" + threadId + " ebx=0x" + ebx.ToString("X8")
            + " edx=0x" + edx.ToString("X8") + " ecx=0x" + ecx.ToString("X8")
            + " vtable=0x" + vtable.ToString("X8") + " slot8=0x" + method.ToString("X8")
            + " executableSlot=" + executableMethod + " stackThis=0x" + stackThis.ToString("X8")
            + " stackArg1=0x" + stackString.ToString("X8"));
        if (ecx != vtable || stackThis != ebx || stackString != edx || !executableMethod)
        {
            evidence = "mission vtable-call registers, stack arguments, or vtable slot failed verification";
            return false;
        }
        path = TryReadMissionPathArgument(process, edx, out evidence);
        if (path == null) evidence = "mission vtable argument path unresolved: " + evidence;
        return path != null;
    }

    private static bool ComputeAtmospherePhaseMilliseconds(uint rawClock, uint origin,
        uint periodMilliseconds, out uint phaseMilliseconds)
    {
        phaseMilliseconds = 0;
        if (periodMilliseconds == 0) return false;
        uint delta = unchecked(rawClock - origin);
        phaseMilliseconds = delta % periodMilliseconds;
        return true;
    }

    private static AtmosphereReadback CaptureAtmospherePhase(IntPtr process, uint terrainBase,
        uint world3dBase, byte[] context, uint threadId, string terrainModuleSha256,
        FrameSnapshot frame)
    {
        if (context == null || context.Length != X86ContextSize
            || BitConverter.ToUInt32(context, ContextEipOffset) != unchecked(terrainBase + AtmospherePhaseRva + 1))
            throw new InvalidOperationException("Terrain atmosphere-phase breakpoint context mismatch.");

        uint atmosphereObject = BitConverter.ToUInt32(context, ContextEsiOffset);
        uint rawClock = BitConverter.ToUInt32(context, ContextEbpOffset);
        uint phaseMilliseconds = BitConverter.ToUInt32(context, ContextEdxOffset);
        if (atmosphereObject < 0x10000 || world3dBase == 0)
            throw new InvalidOperationException("Terrain atmosphere object or World3D game-time source was unavailable.");

        uint origin = ReadU32Exact(process, unchecked(atmosphereObject + 0x144));
        uint periodMilliseconds = ReadU32Exact(process, unchecked(atmosphereObject + 0x150));
        uint worldGameTimeWord = ReadU32Exact(process, unchecked(world3dBase + 0x32A38));
        uint recomputed;
        bool arithmeticVerified = ComputeAtmospherePhaseMilliseconds(rawClock, origin,
            periodMilliseconds, out recomputed) && recomputed == phaseMilliseconds;
        bool worldTimeMatchesRawClock = rawClock == worldGameTimeWord;
        return new AtmosphereReadback
        {
            SampleThreadId = threadId,
            CameraGenerationAtSample = frame == null ? 0 : frame.CameraGeneration,
            CameraThreadIdAtSample = frame == null ? 0 : frame.CameraThreadId,
            AtmosphereObject = atmosphereObject,
            RawClock = rawClock,
            PhaseMilliseconds = phaseMilliseconds,
            Origin = origin,
            PeriodMilliseconds = periodMilliseconds,
            RecomputedPhaseMilliseconds = recomputed,
            WorldGameTimeWord = worldGameTimeWord,
            WorldGameTimeWordReadable = true,
            TerrainModuleSha256 = terrainModuleSha256,
            TerrainModuleHashVerified = String.Equals(terrainModuleSha256,
                ExpectedSha256[TerrainName], StringComparison.OrdinalIgnoreCase),
            ArithmeticVerified = arithmeticVerified,
            WorldTimeMatchesRawClock = worldTimeMatchesRawClock
        };
    }

    private static bool IsVerifiedAtmosphereForFrame(FrameSnapshot frame, uint projectionThreadId)
    {
        if (frame == null || frame.Atmosphere == null) return false;
        AtmosphereReadback atmosphere = frame.Atmosphere;
        return atmosphere.TerrainModuleHashVerified
            && atmosphere.PeriodMilliseconds != 0
            && atmosphere.ArithmeticVerified
            && atmosphere.WorldGameTimeWordReadable
            && atmosphere.WorldTimeMatchesRawClock
            && atmosphere.RawClock == frame.WorldGameTimeWord
            && atmosphere.WorldGameTimeWord == frame.WorldGameTimeWord
            && frame.CameraWorldGameTimeWordReadable
            && frame.WorldGameTimeWordReadable
            && frame.CameraWorldGameTimeWord == frame.WorldGameTimeWord
            && atmosphere.SampleThreadId == frame.CameraThreadId
            && atmosphere.SampleThreadId == frame.ProjectionThreadId
            && frame.ProjectionThreadId == projectionThreadId
            && frame.CameraGeneration != 0
            && frame.CameraGeneration == frame.ProjectionGeneration
            && frame.CameraThreadId == projectionThreadId
            && frame.ProjectionGeneration == atmosphere.MatchedRenderGeneration
            && atmosphere.MatchedRenderThreadId == projectionThreadId
            && frame.Camera != null && frame.Camera.CurrentAtProjectionVerified
            && frame.Projection != null && frame.Projection.Verified;
    }

    private static void PairAtmosphereToProjection(FrameSnapshot frame, uint projectionThreadId)
    {
        frame.Atmosphere = null;
        AtmosphereReadback candidate;
        if (frame.AtmosphereByThread == null
            || !frame.AtmosphereByThread.TryGetValue(projectionThreadId, out candidate))
        {
            Log("ATMOSPHERE_FRAME_PAIR missing_sample tid=" + projectionThreadId
                + " generation=" + frame.CameraGeneration);
            return;
        }
        candidate.CandidateRenderGeneration = frame.CameraGeneration;
        candidate.CandidateRenderThreadId = projectionThreadId;
        candidate.MatchedRenderGeneration = frame.CameraGeneration;
        candidate.MatchedRenderThreadId = projectionThreadId;
        frame.Atmosphere = candidate;
        candidate.SameGenerationVerified = IsVerifiedAtmosphereForFrame(frame, projectionThreadId);
        if (!candidate.SameGenerationVerified)
        {
            candidate.MatchedRenderGeneration = 0;
            candidate.MatchedRenderThreadId = 0;
        }
        Log("ATMOSPHERE_FRAME_PAIR generation=" + frame.CameraGeneration + " tid=" + projectionThreadId
            + " sampleTid=" + candidate.SampleThreadId + " esi=0x" + candidate.AtmosphereObject.ToString("X8")
            + " rawClock=0x" + candidate.RawClock.ToString("X8") + " phaseMs=" + candidate.PhaseMilliseconds
            + " origin=" + candidate.Origin + " periodMs=" + candidate.PeriodMilliseconds
            + " recomputedPhaseMs=" + candidate.RecomputedPhaseMilliseconds
            + " worldGameTime=0x" + candidate.WorldGameTimeWord.ToString("X8")
            + " arithmeticVerified=" + candidate.ArithmeticVerified
            + " rawMatchesWorldTime=" + candidate.WorldTimeMatchesRawClock
            + " entryWorldTime=0x" + frame.CameraWorldGameTimeWord.ToString("X8")
            + " projectionWorldTime=0x" + frame.WorldGameTimeWord.ToString("X8")
            + " sameGenerationVerified=" + candidate.SameGenerationVerified);
    }

    private static bool VerifyPresentBoundary(IntPtr process, uint ngiBase, out byte firstByte, out string evidence)
    {
        firstByte = 0;
        byte[] code = new byte[3];
        if (!Read(process, unchecked(ngiBase + PresentBoundaryRva), code))
        {
            evidence = "Ngi32 present boundary unreadable";
            return false;
        }
        firstByte = code[0];
        bool match = code[0] == 0xFF && code[1] == 0x50 && code[2] == 0x14;
        evidence = "base=0x" + ngiBase.ToString("X8") + " RVA=0x6E1B bytes=" + BitConverter.ToString(code);
        return match;
    }

    private static byte[] ContextAtBreakpoint(byte[] original, uint address, bool singleStep)
    {
        if (original == null || original.Length != X86ContextSize)
            throw new ArgumentException("Expected a full x86 thread context.", "original");
        byte[] result = (byte[])original.Clone();
        Buffer.BlockCopy(BitConverter.GetBytes(unchecked((int)address)), 0, result, ContextEipOffset, 4);
        int flags = BitConverter.ToInt32(result, ContextEflagsOffset);
        flags = singleStep ? (flags | 0x100) : (flags & ~0x100);
        Buffer.BlockCopy(BitConverter.GetBytes(flags), 0, result, ContextEflagsOffset, 4);
        return result;
    }

    private static void BeginSingleStep(IntPtr process, uint threadId, BreakpointInfo breakpoint,
        byte[] stoppedContext, out PendingStep pending, bool rearmOnComplete)
    {
        pending = null;
        if (!breakpoint.Armed) throw new InvalidOperationException(breakpoint.Name + " was not armed at its hit.");
        if (!WriteByte(process, breakpoint.Address, breakpoint.OriginalByte))
            throw new InvalidOperationException("Could not restore " + breakpoint.Name + " before single-step.");
        breakpoint.Armed = false;
        IntPtr thread = OpenThread(ThreadGetContext | ThreadSetContext, false, threadId);
        if (thread == IntPtr.Zero) throw new InvalidOperationException("OpenThread for " + breakpoint.Name + " failed: " + Marshal.GetLastWin32Error());
        try
        {
            SetFullX86Context(thread, ContextAtBreakpoint(stoppedContext, breakpoint.Address, true));
        }
        finally { CloseHandle(thread); }
        pending = new PendingStep { Breakpoint = breakpoint, ThreadId = threadId,
            DeadlineUtc = DateTime.UtcNow.AddSeconds(5), RearmOnComplete = rearmOnComplete };
    }

    private static void FinishSingleStep(IntPtr process, uint threadId, PendingStep pending)
    {
        if (pending == null || pending.ThreadId != threadId)
            throw new InvalidOperationException("Unexpected single-step thread.");
        BreakpointInfo breakpoint = pending.Breakpoint;
        if (pending.RearmOnComplete && !WriteByte(process, breakpoint.Address, 0xCC))
            throw new InvalidOperationException("Could not re-arm " + breakpoint.Name + " after single-step.");
        IntPtr thread = OpenThread(ThreadGetContext | ThreadSetContext, false, threadId);
        if (thread == IntPtr.Zero) throw new InvalidOperationException("OpenThread after single-step failed: " + Marshal.GetLastWin32Error());
        try
        {
            byte[] context = GetFullX86Context(thread);
            int flags = BitConverter.ToInt32(context, ContextEflagsOffset) & ~0x100;
            Buffer.BlockCopy(BitConverter.GetBytes(flags), 0, context, ContextEflagsOffset, 4);
            SetFullX86Context(thread, context);
        }
        finally { CloseHandle(thread); }
        breakpoint.Armed = pending.RearmOnComplete;
    }

    private static void ResumeOriginalPresent(IntPtr process, RemoteReadbackSession session)
    {
        BreakpointInfo breakpoint = session.PresentBreakpoint;
        if (breakpoint.Armed)
        {
            if (!WriteByte(process, breakpoint.Address, breakpoint.OriginalByte))
                throw new InvalidOperationException("Could not restore Ngi32 present call after readback.");
            breakpoint.Armed = false;
        }
        IntPtr thread = OpenThread(ThreadGetContext | ThreadSetContext, false, session.ThreadId);
        if (thread == IntPtr.Zero) throw new InvalidOperationException("OpenThread to resume Ngi32 present failed: " + Marshal.GetLastWin32Error());
        try
        {
            byte[] resumeContext = ContextAtBreakpoint(session.OriginalContext, breakpoint.Address, false);
            SetFullX86Context(thread, resumeContext);
            byte[] restoredContext = GetFullX86Context(thread);
            uint expectedEsp = BitConverter.ToUInt32(session.OriginalContext, ContextEspOffset);
            uint restoredEip = BitConverter.ToUInt32(restoredContext, ContextEipOffset);
            uint restoredEsp = BitConverter.ToUInt32(restoredContext, ContextEspOffset);
            Log("PRESENT_RESUME_CONTEXT expectedEip=0x" + breakpoint.Address.ToString("X8")
                + " actualEip=0x" + restoredEip.ToString("X8")
                + " expectedEsp=0x" + expectedEsp.ToString("X8")
                + " actualEsp=0x" + restoredEsp.ToString("X8"));
            if (restoredEip != breakpoint.Address || restoredEsp != expectedEsp)
                throw new InvalidOperationException("Could not verify restored Ngi32 present EIP/ESP.");
        }
        finally { CloseHandle(thread); }
    }

    private static bool CanStopCapture(bool captured, bool stopAfterAttempt,
        bool remoteInFlight, bool singleStepInFlight, bool cameraRestorePending)
    {
        return (captured || stopAfterAttempt) && !remoteInFlight && !singleStepInFlight
            && !cameraRestorePending;
    }

    private static bool IsMatchingPerspectiveFrame(FrameSnapshot frame, uint threadId)
    {
        return CanArmPresentForFrame(frame, threadId)
            && (!frame.SelectedCameraRequested || (frame.SelectedCamera != null
                && frame.SelectedCamera.Applied && frame.SelectedCamera.ProjectionMatchesInput
                && frame.SelectedCamera.CandidateProjectionVerified
                && frame.SelectedCamera.Restored && frame.SelectedCamera.RestoreVerified
                && frame.SelectedCamera.MatrixIntactAtReturn
                && !frame.SelectedCamera.PixelAttributionInvalidated
                && SelectedMatrixMatchesProjection(frame)));
    }

    private static bool SelectedMatrixMatchesProjection(FrameSnapshot frame)
    {
        if (frame == null || !frame.SelectedCameraRequested) return true;
        return frame.SelectedCamera != null && frame.SelectedCamera.Input != null
            && frame.Camera != null && frame.Camera.CurrentAtProjectionVerified
            && !frame.Camera.WordsChangedAtProjection
            && ByteArraysEqual(frame.Camera.ProjectionMatrixBytes, frame.SelectedCamera.Input.MatrixBytes);
    }

    private static bool CanArmPresentForFrame(FrameSnapshot frame, uint threadId)
    {
        return frame != null && frame.Camera != null && frame.Camera.LayoutVerified && frame.Camera.MatrixFinite
            && frame.Camera.CurrentAtProjectionVerified
            && frame.Projection != null && frame.Projection.Verified && frame.Projection.Mode != 0
            && frame.CameraGeneration != 0 && frame.ProjectionGeneration == frame.CameraGeneration
            && frame.CameraThreadId == threadId && frame.ProjectionThreadId == threadId
            && (!frame.SelectedCameraRequested || (frame.SelectedCamera != null
                && frame.SelectedCamera.Applied && frame.SelectedCamera.ProjectionMatchesInput
                && frame.SelectedCamera.CandidateProjectionVerified
                && !frame.SelectedCamera.PixelAttributionInvalidated
                && SelectedMatrixMatchesProjection(frame)));
    }

    private static bool IsViewportInsideSurface(int[] viewport, uint width, uint height)
    {
        return viewport != null && viewport.Length == 4 && width > 0 && height > 0
            && viewport[0] >= 0 && viewport[1] >= 0
            && viewport[2] > viewport[0] && viewport[3] > viewport[1]
            && (uint)viewport[2] <= width && (uint)viewport[3] <= height;
    }

    private static string UsableFrameJson(string gamePath, uint world3dBase, uint ngiBase,
        FrameSnapshot frame, SurfaceReadback surface, string pngPath, RemoteReadbackSession remote)
    {
        if (frame == null || frame.Camera == null || frame.Projection == null || surface == null)
            throw new InvalidOperationException("A usable camera, projection, and pixel surface are required for the legacy input JSON.");
        if (!IsMatchingPerspectiveFrame(frame, frame.ProjectionThreadId))
            throw new InvalidOperationException("Refusing to emit app-compatible JSON before camera restoration and pixel attribution are verified.");
        CameraReadback camera = frame.Camera;
        ProjectionReadback projection = frame.Projection;
        if (!camera.LayoutVerified || !camera.MatrixFinite || !camera.CurrentAtProjectionVerified
            || !projection.Verified || projection.Mode == 0)
            throw new InvalidOperationException("Refusing to emit app-compatible JSON from an unverified camera or non-perspective projection.");
        if (!IsViewportInsideSurface(projection.Viewport, surface.Width, surface.Height))
            throw new InvalidOperationException("Refusing to emit app-compatible JSON because the captured viewport lies outside the pixel surface.");
        StringBuilder json = new StringBuilder();
        json.AppendLine("{");
        json.AppendLine("  \"schema\": \"fparkan-legacy-camera-v1\",");
        json.AppendLine("  \"capture_status\": \"native-frame-captured\",");
        json.AppendLine("  \"render_input_usable\": true,");
        json.AppendLine("  \"source\": \"GOG World3D stdRenderGame + Ngi32 D3D7 render target\",");
        json.AppendLine("  \"scratch_executable\": " + JsonString(gamePath) + ",");
        json.AppendLine("  \"world3d_module_base\": " + JsonString("0x" + world3dBase.ToString("X8")) + ",");
        json.AppendLine("  \"ngi32_module_base\": " + JsonString("0x" + ngiBase.ToString("X8")) + ",");
        json.AppendLine("  \"frame_generation\": " + frame.CameraGeneration.ToString(CultureInfo.InvariantCulture) + ",");
        json.AppendLine("  \"camera_argument\": " + JsonString("0x" + camera.Camera.ToString("X8")) + ",");
        json.AppendLine("  \"camera_vtable\": " + JsonString("0x" + camera.Vtable.ToString("X8")) + ",");
        json.AppendLine("  \"selector_field_plus_0x10\": " + JsonString("0x" + camera.SelectorField.ToString("X8")) + ",");
        json.AppendLine("  \"camera_words_sampled_at\": \"World3D+0x13CE4\",");
        json.AppendLine("  \"camera_words_changed_since_render_entry\": " + (camera.WordsChangedAtProjection ? "true" : "false") + ",");
        json.AppendLine("  \"camera_words_render_entry_sha256\": " + JsonString(camera.EntryMatrixSha256) + ",");
        json.AppendLine("  \"camera_words_projection_boundary_sha256\": " + JsonString(camera.ProjectionMatrixSha256) + ",");
        json.Append("  \"selector0_words\": [");
        for (int i = 0; i < camera.Words.Length; i++)
        {
            if (i != 0) json.Append(", ");
            json.Append(camera.Words[i].ToString(CultureInfo.InvariantCulture));
        }
        json.AppendLine("],");
        json.AppendLine("  \"viewport\": [" + String.Join(", ", projection.Viewport) + "],");
        json.AppendLine("  \"near_plane\": " + projection.Near.ToString("R", CultureInfo.InvariantCulture) + ",");
        json.AppendLine("  \"far_plane\": " + projection.Far.ToString("R", CultureInfo.InvariantCulture) + ",");
        json.AppendLine("  \"field_of_view_radians\": " + projection.Fov.ToString("R", CultureInfo.InvariantCulture) + ",");
        json.AppendLine("  \"projection_mode_byte\": " + projection.Mode.ToString(CultureInfo.InvariantCulture) + ",");
        if (frame.SelectedCamera == null) json.AppendLine("  \"selected_camera\": null,");
        else
        {
            SelectedCameraState selected = frame.SelectedCamera;
            json.AppendLine("  \"selected_camera\": {");
            json.AppendLine("    \"input_json\": " + JsonString(selected.Input.Path) + ",");
            json.AppendLine("    \"requested_matrix_sha256\": " + JsonString(selected.Input.MatrixSha256) + ",");
            json.AppendLine("    \"preflight_camera_argument\": " + JsonString("0x" + selected.CandidateCameraPointer.ToString("X8")) + ",");
            json.AppendLine("    \"preflight_thread_id\": " + selected.CandidateThreadId.ToString(CultureInfo.InvariantCulture) + ",");
            json.AppendLine("    \"preflight_render_generation\": " + selected.CandidateGeneration.ToString(CultureInfo.InvariantCulture) + ",");
            json.AppendLine("    \"preflight_projection_verified\": " + (selected.CandidateProjectionVerified ? "true" : "false") + ",");
            json.AppendLine("    \"original_matrix_sha256\": " + JsonString(selected.OriginalMatrixSha256) + ",");
            json.AppendLine("    \"camera_argument\": " + JsonString("0x" + selected.CameraPointer.ToString("X8")) + ",");
            json.AppendLine("    \"thread_id\": " + selected.ThreadId.ToString(CultureInfo.InvariantCulture) + ",");
            json.AppendLine("    \"frame_generation\": " + selected.Generation.ToString(CultureInfo.InvariantCulture) + ",");
            json.AppendLine("    \"render_entry_esp\": " + JsonString("0x" + selected.EntryEsp.ToString("X8")) + ",");
            json.AppendLine("    \"render_return_address\": " + JsonString("0x" + selected.ReturnAddress.ToString("X8")) + ",");
            json.AppendLine("    \"render_return_boundary_rva\": \"World3D+0x13D7B\",");
            json.AppendLine("    \"native_setter_applied\": " + (selected.Applied ? "true" : "false") + ",");
            json.AppendLine("    \"projection_matches_input\": " + (selected.ProjectionMatchesInput ? "true" : "false") + ",");
            json.AppendLine("    \"matrix_intact_at_return\": " + (selected.MatrixIntactAtReturn ? "true" : "false") + ",");
            json.AppendLine("    \"native_setter_restore_completed\": " + (selected.Restored ? "true" : "false") + ",");
            json.AppendLine("    \"restore_matrix_verified\": " + (selected.RestoreVerified ? "true" : "false") + ",");
            json.AppendLine("    \"pixel_attribution_invalidated\": " + (selected.PixelAttributionInvalidated ? "true" : "false"));
            json.AppendLine("  },");
        }
        json.AppendLine("  \"mission_path\": " + (String.IsNullOrEmpty(frame.MissionPath) ? "null" : JsonString(frame.MissionPath)) + ",");
        json.AppendLine("  \"mission_identity\": " + (String.IsNullOrEmpty(frame.MissionPath)
            ? "\"unknown\"" : JsonString("path-observed")) + ",");
        json.AppendLine("  \"mission_identity_evidence\": "
            + (String.IsNullOrEmpty(frame.MissionEvidence) ? "null" : JsonString(frame.MissionEvidence)) + ",");
        json.AppendLine("  \"simulation_time_seconds\": null,");
        bool atmosphereVerified = IsVerifiedAtmosphereForFrame(frame, frame.ProjectionThreadId);
        string atmosphereSeconds = atmosphereVerified
            ? (((double)frame.Atmosphere.PhaseMilliseconds) / 1000.0).ToString("R", CultureInfo.InvariantCulture)
            : "null";
        json.AppendLine("  \"atmosphere_seconds\": " + atmosphereSeconds + ",");
        json.AppendLine("  \"atmosphere_phase\": {");
        json.AppendLine("    \"sample_rva\": \"Terrain+0x421DC\",");
        json.AppendLine("    \"terrain_module_sha256\": " + (frame.Atmosphere == null
            || String.IsNullOrEmpty(frame.Atmosphere.TerrainModuleSha256) ? "null"
            : JsonString(frame.Atmosphere.TerrainModuleSha256)) + ",");
        json.AppendLine("    \"terrain_sha256_verified\": "
            + (frame.Atmosphere != null && frame.Atmosphere.TerrainModuleHashVerified ? "true" : "false") + ",");
        json.AppendLine("    \"sample_thread_id\": " + (frame.Atmosphere == null ? "null"
            : frame.Atmosphere.SampleThreadId.ToString(CultureInfo.InvariantCulture)) + ",");
        json.AppendLine("    \"atmosphere_object_esi\": " + (frame.Atmosphere == null ? "null"
            : JsonString("0x" + frame.Atmosphere.AtmosphereObject.ToString("X8"))) + ",");
        json.AppendLine("    \"raw_clock_ebp\": " + (frame.Atmosphere == null ? "null"
            : JsonString("0x" + frame.Atmosphere.RawClock.ToString("X8"))) + ",");
        json.AppendLine("    \"phase_ms_edx\": " + (frame.Atmosphere == null ? "null"
            : frame.Atmosphere.PhaseMilliseconds.ToString(CultureInfo.InvariantCulture)) + ",");
        json.AppendLine("    \"origin_u32_esi_plus_0x144\": " + (frame.Atmosphere == null ? "null"
            : frame.Atmosphere.Origin.ToString(CultureInfo.InvariantCulture)) + ",");
        json.AppendLine("    \"period_ms_esi_plus_0x150\": " + (frame.Atmosphere == null ? "null"
            : frame.Atmosphere.PeriodMilliseconds.ToString(CultureInfo.InvariantCulture)) + ",");
        json.AppendLine("    \"recomputed_phase_ms\": " + (frame.Atmosphere == null ? "null"
            : frame.Atmosphere.RecomputedPhaseMilliseconds.ToString(CultureInfo.InvariantCulture)) + ",");
        json.AppendLine("    \"world_game_time_word_at_sample\": " + (frame.Atmosphere == null ? "null"
            : JsonString("0x" + frame.Atmosphere.WorldGameTimeWord.ToString("X8"))) + ",");
        json.AppendLine("    \"camera_generation_at_sample\": " + (frame.Atmosphere == null ? "null"
            : frame.Atmosphere.CameraGenerationAtSample.ToString(CultureInfo.InvariantCulture)) + ",");
        json.AppendLine("    \"camera_thread_id_at_sample\": " + (frame.Atmosphere == null ? "null"
            : frame.Atmosphere.CameraThreadIdAtSample.ToString(CultureInfo.InvariantCulture)) + ",");
        json.AppendLine("    \"candidate_render_generation\": " + (frame.Atmosphere == null ? "null"
            : frame.Atmosphere.CandidateRenderGeneration.ToString(CultureInfo.InvariantCulture)) + ",");
        json.AppendLine("    \"matched_render_generation\": " + (atmosphereVerified
            ? frame.Atmosphere.MatchedRenderGeneration.ToString(CultureInfo.InvariantCulture) : "null") + ",");
        json.AppendLine("    \"matched_render_thread_id\": " + (atmosphereVerified
            ? frame.Atmosphere.MatchedRenderThreadId.ToString(CultureInfo.InvariantCulture) : "null") + ",");
        json.AppendLine("    \"arithmetic_verified\": "
            + (frame.Atmosphere != null && frame.Atmosphere.ArithmeticVerified ? "true" : "false") + ",");
        json.AppendLine("    \"raw_clock_matches_world_time\": "
            + (frame.Atmosphere != null && frame.Atmosphere.WorldTimeMatchesRawClock ? "true" : "false") + ",");
        json.AppendLine("    \"same_generation_verified\": " + (atmosphereVerified ? "true" : "false"));
        json.AppendLine("  },");
        json.AppendLine("  \"weather_state\": null,");
        json.AppendLine("  \"rng_state\": null,");
        json.AppendLine("  \"raw_world_game_time_word32A38\": " + JsonString("0x" + frame.WorldGameTimeWord.ToString("X8")) + ",");
        json.AppendLine("  \"native_frame_png\": " + JsonString(Path.GetFileName(pngPath)) + ",");
        json.AppendLine("  \"pixel_capture\": {");
        json.AppendLine("    \"width\": " + surface.Width.ToString(CultureInfo.InvariantCulture) + ",");
        json.AppendLine("    \"height\": " + surface.Height.ToString(CultureInfo.InvariantCulture) + ",");
        json.AppendLine("    \"pitch_bytes\": " + surface.Pitch.ToString(CultureInfo.InvariantCulture) + ",");
        json.AppendLine("    \"bit_count\": " + surface.BitCount.ToString(CultureInfo.InvariantCulture) + ",");
        json.AppendLine("    \"red_mask\": " + JsonString("0x" + surface.RedMask.ToString("X8")) + ",");
        json.AppendLine("    \"green_mask\": " + JsonString("0x" + surface.GreenMask.ToString("X8")) + ",");
        json.AppendLine("    \"blue_mask\": " + JsonString("0x" + surface.BlueMask.ToString("X8")) + ",");
        json.AppendLine("    \"alpha_mask\": " + JsonString("0x" + surface.AlphaMask.ToString("X8")) + ",");
        json.AppendLine("    \"rows_sha256\": " + JsonString(surface.Sha256) + ",");
        json.AppendLine("    \"get_render_target_hresult\": " + JsonString("0x" + remote.GetHr.ToString("X8")) + ",");
        json.AppendLine("    \"lock_hresult\": " + JsonString("0x" + remote.LockHr.ToString("X8")) + ",");
        json.AppendLine("    \"unlock_hresult\": " + JsonString("0x" + remote.UnlockHr.ToString("X8")) + ",");
        json.AppendLine("    \"release_result\": " + JsonString("0x" + remote.ReleaseResult.ToString("X8")));
        json.AppendLine("  }");
        json.AppendLine("}");
        return json.ToString();
    }

    private static int SelfCheck()
    {
        if (IntPtr.Size != 4) throw new InvalidOperationException("This helper must be compiled and run as x86.");
        SelfCheckCameraInput();
        SelfCheckRenderInvocationGates();
        uint relocated = 0x10000000u + RelocatedGlobalRva;
        if (relocated != 0x1079518Cu) throw new InvalidOperationException("relocation self-check failed");
        bool offsetsMatch = Marshal.SizeOf(typeof(X86ContextLayout)) == X86ContextSize
            && Marshal.OffsetOf(typeof(X86ContextLayout), "Eip").ToInt32() == ContextEipOffset
            && Marshal.OffsetOf(typeof(X86ContextLayout), "Esi").ToInt32() == ContextEsiOffset
            && Marshal.OffsetOf(typeof(X86ContextLayout), "Ebp").ToInt32() == ContextEbpOffset
            && Marshal.OffsetOf(typeof(X86ContextLayout), "Esp").ToInt32() == ContextEspOffset;
        FrameSnapshot matched = new FrameSnapshot { CameraGeneration = 7, CameraThreadId = 11,
            Camera = new CameraReadback { LayoutVerified = true, MatrixFinite = true, CurrentAtProjectionVerified = true },
            ProjectionGeneration = 7, ProjectionThreadId = 11,
            Projection = new ProjectionReadback { Verified = true, Mode = 1 } };
        byte[] selectedMatrixBytes = new byte[64];
        selectedMatrixBytes[0] = 1;
        byte[] changedSelectedMatrixBytes = (byte[])selectedMatrixBytes.Clone();
        changedSelectedMatrixBytes[0] = 2;
        SelectedCameraInput selectedInput = new SelectedCameraInput { MatrixBytes = selectedMatrixBytes };
        FrameSnapshot selectedMatched = new FrameSnapshot
        {
            CameraGeneration = 8, CameraThreadId = 11,
            Camera = new CameraReadback { LayoutVerified = true, MatrixFinite = true, CurrentAtProjectionVerified = true,
                EntryMatrixBytes = (byte[])selectedMatrixBytes.Clone(), ProjectionMatrixBytes = (byte[])selectedMatrixBytes.Clone() },
            ProjectionGeneration = 8, ProjectionThreadId = 11,
            Projection = new ProjectionReadback { Verified = true, Mode = 1 },
            SelectedCameraRequested = true,
            SelectedCamera = new SelectedCameraState
            {
                Applied = true, ProjectionMatchesInput = true, CandidateProjectionVerified = true,
                Restored = true, RestoreVerified = true, MatrixIntactAtReturn = true, Input = selectedInput
            }
        };
        FrameSnapshot selectedUnrestored = new FrameSnapshot
        {
            CameraGeneration = 8, CameraThreadId = 11,
            Camera = selectedMatched.Camera, ProjectionGeneration = 8, ProjectionThreadId = 11,
            Projection = selectedMatched.Projection, SelectedCameraRequested = true,
            SelectedCamera = new SelectedCameraState
            {
                Applied = true, ProjectionMatchesInput = true, CandidateProjectionVerified = true,
                MatrixIntactAtReturn = true, Input = selectedInput
            }
        };
        FrameSnapshot selectedIntervened = new FrameSnapshot
        {
            CameraGeneration = 8, CameraThreadId = 11,
            Camera = selectedMatched.Camera, ProjectionGeneration = 8, ProjectionThreadId = 11,
            Projection = selectedMatched.Projection, SelectedCameraRequested = true,
            SelectedCamera = new SelectedCameraState
            {
                Applied = true, ProjectionMatchesInput = true, CandidateProjectionVerified = true,
                Restored = true, RestoreVerified = true, MatrixIntactAtReturn = true,
                PixelAttributionInvalidated = true, Input = selectedInput
            }
        };
        FrameSnapshot selectedMatrixChanged = new FrameSnapshot
        {
            CameraGeneration = 8, CameraThreadId = 11,
            Camera = new CameraReadback { LayoutVerified = true, MatrixFinite = true, CurrentAtProjectionVerified = true,
                EntryMatrixBytes = (byte[])selectedMatrixBytes.Clone(), ProjectionMatrixBytes = changedSelectedMatrixBytes },
            ProjectionGeneration = 8, ProjectionThreadId = 11,
            Projection = new ProjectionReadback { Verified = true, Mode = 1 },
            SelectedCameraRequested = true,
            SelectedCamera = new SelectedCameraState
            {
                Applied = true, ProjectionMatchesInput = true, CandidateProjectionVerified = true,
                Restored = true, RestoreVerified = true, MatrixIntactAtReturn = true, Input = selectedInput
            }
        };
        byte[] pendingContext = new byte[X86ContextSize];
        const uint pendingEsp = 0x00100100;
        Buffer.BlockCopy(BitConverter.GetBytes(0x00100100u), 0, pendingContext, ContextEspOffset, 4);
        Buffer.BlockCopy(BitConverter.GetBytes(0xFFFFFFFFu), 0, pendingContext, ContextEflagsOffset, 4);
        byte[] resumedContext = ContextAtBreakpoint(pendingContext, 0x12345678u, false);
        uint wrappedPhase;
        bool atmosphereArithmetic = ComputeAtmospherePhaseMilliseconds(0x00000010u, 0xFFFFFFF0u,
            0x00000100u, out wrappedPhase) && wrappedPhase == 32u;
        uint ignoredPhase;
        bool zeroPeriodRejected = !ComputeAtmospherePhaseMilliseconds(10u, 0u, 0u, out ignoredPhase);
        FrameSnapshot atmosphereMatched = new FrameSnapshot
        {
            CameraGeneration = 7,
            CameraThreadId = 11,
            Camera = new CameraReadback { LayoutVerified = true, MatrixFinite = true, CurrentAtProjectionVerified = true },
            ProjectionGeneration = 7,
            ProjectionThreadId = 11,
            Projection = new ProjectionReadback { Verified = true, Mode = 1 },
            CameraWorldGameTimeWord = 0x10,
            CameraWorldGameTimeWordReadable = true,
            WorldGameTimeWord = 0x10,
            WorldGameTimeWordReadable = true,
            Atmosphere = new AtmosphereReadback
            {
                SampleThreadId = 11,
                CameraGenerationAtSample = 7,
                CameraThreadIdAtSample = 11,
                RawClock = 0x10,
                PhaseMilliseconds = 32,
                Origin = 0xFFFFFFF0,
                PeriodMilliseconds = 0x100,
                RecomputedPhaseMilliseconds = 32,
                WorldGameTimeWord = 0x10,
                WorldGameTimeWordReadable = true,
                TerrainModuleSha256 = ExpectedSha256[TerrainName],
                TerrainModuleHashVerified = true,
                ArithmeticVerified = true,
                WorldTimeMatchesRawClock = true,
                MatchedRenderGeneration = 7,
                MatchedRenderThreadId = 11
            }
        };
        FrameSnapshot atmosphereClockMismatch = new FrameSnapshot
        {
            CameraGeneration = 7,
            CameraThreadId = 11,
            Camera = atmosphereMatched.Camera,
            ProjectionGeneration = 7,
            ProjectionThreadId = 11,
            Projection = atmosphereMatched.Projection,
            CameraWorldGameTimeWord = 0x11,
            CameraWorldGameTimeWordReadable = true,
            WorldGameTimeWord = 0x11,
            WorldGameTimeWordReadable = true,
            Atmosphere = atmosphereMatched.Atmosphere
        };
        bool atmosphereFrameValid = IsVerifiedAtmosphereForFrame(atmosphereMatched, 11);
        bool atmosphereClockMismatchRejected = !IsVerifiedAtmosphereForFrame(atmosphereClockMismatch, 11);
        bool cameraFrameValid = IsMatchingPerspectiveFrame(matched, 11);
        bool wrongCameraThreadRejected = !IsMatchingPerspectiveFrame(matched, 12);
        bool emptyCameraFrameRejected = !IsMatchingPerspectiveFrame(new FrameSnapshot(), 11);
        bool selectedFrameValid = IsMatchingPerspectiveFrame(selectedMatched, 11);
        bool selectedUnrestoredRejected = !IsMatchingPerspectiveFrame(selectedUnrestored, 11);
        bool selectedUnrestoredCaptureEligible = CanArmPresentForFrame(selectedUnrestored, 11);
        bool selectedIntervenedCaptureRejected = !CanArmPresentForFrame(selectedIntervened, 11);
        bool selectedIntervenedRejected = !IsMatchingPerspectiveFrame(selectedIntervened, 11);
        bool selectedChangedMatrixRejected = !CanArmPresentForFrame(selectedMatrixChanged, 11)
            && !IsMatchingPerspectiveFrame(selectedMatrixChanged, 11);
        bool stopAfterCapture = CanStopCapture(true, false, false, false, false);
        bool stopAfterFailure = CanStopCapture(false, true, false, false, false);
        bool waitsForRemote = !CanStopCapture(true, false, true, false, false);
        bool waitsForStep = !CanStopCapture(true, false, false, true, false);
        bool waitsForCameraRestore = !CanStopCapture(true, false, false, false, true);
        bool resumedEipOk = BitConverter.ToUInt32(resumedContext, ContextEipOffset) == 0x12345678u;
        bool resumedEspOk = BitConverter.ToUInt32(resumedContext, ContextEspOffset) == pendingEsp;
        bool resumedTrapCleared = (BitConverter.ToUInt32(resumedContext, ContextEflagsOffset) & 0x100u) == 0;
        if (!offsetsMatch
            || ThreadQueryInformation != 0x00000040
            || ContextEbxOffset != 164 || ContextEdxOffset != 168 || ContextEcxOffset != 172
            || !atmosphereArithmetic || !zeroPeriodRejected
            || !atmosphereFrameValid
            || !atmosphereClockMismatchRejected
            || !cameraFrameValid || !wrongCameraThreadRejected || !emptyCameraFrameRejected
            || !selectedFrameValid || !selectedUnrestoredRejected || !selectedUnrestoredCaptureEligible
            || !selectedIntervenedCaptureRejected || !selectedIntervenedRejected
            || !selectedChangedMatrixRejected
            || !stopAfterCapture || !stopAfterFailure || !waitsForRemote || !waitsForStep || !waitsForCameraRestore
            || !resumedEipOk || !resumedEspOk || !resumedTrapCleared)
            throw new InvalidOperationException("Self-check failed: offsets=" + offsetsMatch
                + " atmosphereArithmetic=" + atmosphereArithmetic + " zeroPeriodRejected=" + zeroPeriodRejected
                + " atmosphereFrameValid=" + atmosphereFrameValid
                + " atmosphereClockMismatchRejected=" + atmosphereClockMismatchRejected
                + " cameraFrameValid=" + cameraFrameValid + " wrongCameraThreadRejected=" + wrongCameraThreadRejected
                + " emptyCameraFrameRejected=" + emptyCameraFrameRejected + " stopAfterCapture=" + stopAfterCapture
                + " selectedFrameValid=" + selectedFrameValid + " selectedUnrestoredRejected=" + selectedUnrestoredRejected
                + " selectedUnrestoredCaptureEligible=" + selectedUnrestoredCaptureEligible
                + " selectedIntervenedCaptureRejected=" + selectedIntervenedCaptureRejected
                + " selectedIntervenedRejected=" + selectedIntervenedRejected
                + " selectedChangedMatrixRejected=" + selectedChangedMatrixRejected
                + " stopAfterFailure=" + stopAfterFailure + " waitsForRemote=" + waitsForRemote
                + " waitsForStep=" + waitsForStep + " waitsForCameraRestore=" + waitsForCameraRestore
                + " resumedEipOk=" + resumedEipOk
                + " resumedEspOk=" + resumedEspOk + " resumedTrapCleared=" + resumedTrapCleared
                + " ThreadQueryInformation=" + ThreadQueryInformation + " Ebx=" + ContextEbxOffset
                + " Edx=" + ContextEdxOffset + " Ecx=" + ContextEcxOffset);
        Console.WriteLine("self-check: x86 CONTEXT layout, relocation RVA, validated camera JSON, post-prologue invocation identity/stack gates, camera/projection gates, wrapping atmosphere phase arithmetic/clock pairing, and recovery states OK");
        return SelfCheckReadback();
    }

    private static int Main(string[] args)
    {
        try
        {
            if (args.Length == 1 && args[0] == "--self-check") return SelfCheck();
            if (args.Length == 2 && args[0] == "--validate-camera-input")
            {
                SelectedCameraInput validated = LoadSelectedCameraInput(args[1]);
                Console.WriteLine("camera input valid: matrixSha256=" + validated.MatrixSha256
                    + " viewport=" + String.Join(",", validated.Viewport)
                    + " near=" + validated.Near.ToString("R", CultureInfo.InvariantCulture)
                    + " far=" + validated.Far.ToString("R", CultureInfo.InvariantCulture)
                    + " fov=" + validated.FieldOfView.ToString("R", CultureInfo.InvariantCulture)
                    + " mode=" + validated.ProjectionMode);
                return 0;
            }
            bool hasCameraInput = args.Length == 6 && args[4] == "--camera-input";
            if ((args.Length != 4 && !hasCameraInput) || args[0] != "--capture")
            {
                Console.Error.WriteLine("       NativeFrameCapture.exe --capture <pid> <expected-start-utc-ticks> <output.json>");
                Console.Error.WriteLine("           [--camera-input <native-frame.json>]");
                Console.Error.WriteLine("       NativeFrameCapture.exe --validate-camera-input <native-frame.json>");
                Console.Error.WriteLine("       NativeFrameCapture.exe --self-check");
                return 2;
            }
            if (IntPtr.Size != 4) throw new InvalidOperationException("Run the x86 build only.");
            uint processId = UInt32.Parse(args[1], CultureInfo.InvariantCulture);
            long expectedStartTicks = Int64.Parse(args[2], CultureInfo.InvariantCulture);
            SelectedCameraInput selectedCameraInput = hasCameraInput ? LoadSelectedCameraInput(args[5]) : null;
            if (selectedCameraInput != null)
                Log("validated selected camera input path=" + selectedCameraInput.Path
                    + " matrixSha256=" + selectedCameraInput.MatrixSha256
                    + " viewport=" + String.Join(",", selectedCameraInput.Viewport)
                    + " near=" + selectedCameraInput.Near.ToString("R", CultureInfo.InvariantCulture)
                    + " far=" + selectedCameraInput.Far.ToString("R", CultureInfo.InvariantCulture)
                    + " fov=" + selectedCameraInput.FieldOfView.ToString("R", CultureInfo.InvariantCulture)
                    + " mode=" + selectedCameraInput.ProjectionMode);
            string repositoryRoot = FindRepositoryRoot();
            string expectedPath = Path.GetFullPath(VerifyScratchFiles(repositoryRoot));
            string outputPath = Path.GetFullPath(args[3]);
            string targetRoot = Path.GetFullPath(Path.Combine(repositoryRoot, "target")) + Path.DirectorySeparatorChar;
            if (!outputPath.StartsWith(targetRoot, StringComparison.OrdinalIgnoreCase))
                throw new InvalidOperationException("Output must be beneath the repository target directory: " + targetRoot);
            if (!String.Equals(Path.GetExtension(outputPath), ".json", StringComparison.OrdinalIgnoreCase))
                throw new InvalidOperationException("Capture output must use a .json extension so PNG/log paths cannot overlap it.");
            string outputPng = Path.ChangeExtension(outputPath, ".png");
            string outputLog = Path.ChangeExtension(outputPath, ".log");
            if (File.Exists(outputPath) || File.Exists(outputPng) || File.Exists(outputLog))
                throw new InvalidOperationException("Output JSON, PNG, or log already exists; choose a fresh basename: " + outputPath);
            Directory.CreateDirectory(Path.GetDirectoryName(outputPath));
            _logPath = outputLog;

            using (Process target = Process.GetProcessById((int)processId))
            {
                IntPtr identityHandle = target.Handle;
                if (identityHandle == IntPtr.Zero) throw new InvalidOperationException("Could not retain the target process identity handle.");
                string actualPath = Path.GetFullPath(target.MainModule.FileName);
                long actualStartTicks = target.StartTime.ToUniversalTime().Ticks;
                if (target.ProcessName != "iron_3d"
                    || !String.Equals(actualPath, expectedPath, StringComparison.OrdinalIgnoreCase))
                    throw new InvalidOperationException("Refusing PID: expected exact scratch image " + expectedPath + ", got " + actualPath);
                if (actualStartTicks != expectedStartTicks)
                    throw new InvalidOperationException("Refusing stale PID; expected UTC start ticks "
                        + expectedStartTicks + ", got " + actualStartTicks);
                Log("identity pid=" + processId + " path=" + actualPath + " startUtc="
                    + target.StartTime.ToUniversalTime().ToString("o", CultureInfo.InvariantCulture)
                    + " startTicks=" + actualStartTicks + " bits=" + (IntPtr.Size * 8));
                foreach (KeyValuePair<string, string> expected in ExpectedSha256)
                    Log("verified sha256 " + expected.Key + "=" + expected.Value);
                return AttachForFrame(processId, expectedPath, expectedStartTicks, outputPath,
                    selectedCameraInput, target, FrameCaptureTimeoutSeconds);
            }
        }
        catch (Exception exception)
        {
            Log("ERROR " + exception);
            Console.Error.WriteLine(exception.Message);
            return 1;
        }
    }

    private static int AttachForFrame(uint processId, string expectedPath, long expectedStartTicks,
        string outputPath, SelectedCameraInput selectedCameraInput, Process identityProcess, int timeoutSeconds)
    {
        const uint ProcessTerminate = 0x0001;
        const uint ProcessVmRead = 0x0010;
        const uint ProcessVmWrite = 0x0020;
        const uint ProcessVmOperation = 0x0008;
        const uint ProcessQueryInformation = 0x0400;
        const uint Synchronize = 0x00100000;
        IntPtr process = IntPtr.Zero;
        IntPtr eventBuffer = IntPtr.Zero;
        uint world3dBase = 0;
        uint terrainBase = 0;
        uint ngiBase = 0;
        byte presentOriginalByte = 0;
        bool presentBoundaryVerified = false;
        bool attached = false;
        bool detached = false;
        bool breakpointsRestored = false;
        bool fatal = false;
        bool processExited = false;
        bool processExitUnconfirmed = false;
        bool captured = false;
        SurfaceReadback completedSurface = null;
        FrameSnapshot completedFrame = null;
        RemoteReadbackSession completedReadback = null;
        bool stopAfterAttempt = false;
        string captureFailure = null;
        DateTime deadline = DateTime.UtcNow.AddSeconds(timeoutSeconds);
        string expectedDirectory = Path.GetDirectoryName(expectedPath);
        string expectedWorld3D = Path.Combine(expectedDirectory, World3DName);
        string expectedTerrain = Path.Combine(expectedDirectory, TerrainName);
        string expectedNgi32 = Path.Combine(expectedDirectory, Ngi32Name);
        string expectedIron3d = Path.Combine(expectedDirectory, Iron3dName);
        uint iron3dBase = 0;
        string terrainModuleSha256 = null;
        BreakpointInfo cameraBreakpoint = null;
        BreakpointInfo projectionBreakpoint = null;
        BreakpointInfo returnBreakpoint = null;
        BreakpointInfo atmosphereBreakpoint = null;
        BreakpointInfo presentBreakpoint = null;
        BreakpointInfo missionBreakpoint = null;
        int missionProbeAttempts = 0;
        bool missionProbeFinished = false;
        PendingStep pendingStep = null;
        RemoteReadbackSession remote = null;
        RemoteCameraSetterSession cameraSetter = null;
        FrameSnapshot frame = new FrameSnapshot();
        frame.SelectedCameraRequested = selectedCameraInput != null;
        SelectedCameraState selectedCamera = selectedCameraInput == null ? null
            : new SelectedCameraState { Input = selectedCameraInput };
        try
        {
            if (identityProcess == null || identityProcess.HasExited
                || identityProcess.StartTime.ToUniversalTime().Ticks != expectedStartTicks
                || !String.Equals(Path.GetFullPath(identityProcess.MainModule.FileName), expectedPath,
                    StringComparison.OrdinalIgnoreCase))
                throw new InvalidOperationException("The retained process identity changed before debugger attach.");
            if (!DebugActiveProcess(processId))
                throw new InvalidOperationException("DebugActiveProcess failed: " + Marshal.GetLastWin32Error());
            attached = true;
            if (identityProcess.HasExited || identityProcess.StartTime.ToUniversalTime().Ticks != expectedStartTicks
                || !String.Equals(Path.GetFullPath(identityProcess.MainModule.FileName), expectedPath,
                    StringComparison.OrdinalIgnoreCase))
                throw new InvalidOperationException("The retained process identity changed during debugger attach.");
            if (!DebugSetProcessKillOnExit(false))
                throw new InvalidOperationException("DebugSetProcessKillOnExit(false) failed: " + Marshal.GetLastWin32Error());
            process = OpenProcess(ProcessTerminate | ProcessVmOperation | ProcessVmRead | ProcessVmWrite
                | ProcessQueryInformation | Synchronize, false, processId);
            if (process == IntPtr.Zero) throw new InvalidOperationException("OpenProcess failed: " + Marshal.GetLastWin32Error());
            eventBuffer = Marshal.AllocHGlobal(128);
            Log("frame capture attached; waiting for verified camera/projection/present boundaries for "
                + timeoutSeconds + " seconds");

            while (!fatal && !CanStopCapture(captured, stopAfterAttempt, remote != null || cameraSetter != null,
                    pendingStep != null, selectedCamera != null && selectedCamera.Applied && !selectedCamera.RestoreVerified)
                && (DateTime.UtcNow < deadline || remote != null || cameraSetter != null || pendingStep != null
                    || selectedCamera != null && selectedCamera.Applied && !selectedCamera.RestoreVerified))
            {
                if (remote != null && DateTime.UtcNow >= remote.DeadlineUtc)
                {
                    Log("REMOTE_COM_STALLED phase=" + remote.Phase + " tid=" + remote.ThreadId
                        + "; terminating only the path/tick/hash-verified scratch process");
                    if (!TerminateProcess(process, 0xE001))
                        Log("REMOTE_COM_TERMINATE_FAILED error=" + Marshal.GetLastWin32Error());
                    fatal = true;
                    continue;
                }
                if (cameraSetter != null && DateTime.UtcNow >= cameraSetter.DeadlineUtc)
                {
                    Log("CAMERA_SETTER_STALLED phase=" + cameraSetter.Phase + " tid=" + cameraSetter.ThreadId
                        + "; terminating only the path/tick/hash-verified scratch process");
                    if (!TerminateProcess(process, 0xE00A))
                        Log("CAMERA_SETTER_TERMINATE_FAILED error=" + Marshal.GetLastWin32Error());
                    fatal = true;
                    continue;
                }
                if (selectedCamera != null && selectedCamera.Applied && !selectedCamera.RestoreVerified
                    && cameraSetter == null && selectedCamera.RestoreDeadlineUtc != DateTime.MinValue
                    && DateTime.UtcNow >= selectedCamera.RestoreDeadlineUtc)
                {
                    Log("SELECTED_CAMERA_RESTORE_BOUND_EXPIRED; terminating only the verified scratch process");
                    if (!TerminateProcess(process, 0xE00B))
                        Log("SELECTED_CAMERA_RESTORE_TERMINATE_FAILED error=" + Marshal.GetLastWin32Error());
                    fatal = true;
                    continue;
                }
                if (pendingStep != null && DateTime.UtcNow >= pendingStep.DeadlineUtc)
                {
                    Log("SINGLE_STEP_STALLED boundary=" + pendingStep.Breakpoint.Name + " tid=" + pendingStep.ThreadId
                        + "; terminating only the path/tick/hash-verified scratch process");
                    if (!TerminateProcess(process, 0xE002))
                        Log("SINGLE_STEP_TERMINATE_FAILED error=" + Marshal.GetLastWin32Error());
                    fatal = true;
                    continue;
                }
                if (!WaitForDebugEvent(eventBuffer, 1000))
                {
                    int waitError = Marshal.GetLastWin32Error();
                    if (waitError == 121 || waitError == 258) continue;
                    throw new InvalidOperationException("WaitForDebugEvent failed: " + waitError);
                }

                uint eventCode = unchecked((uint)Marshal.ReadInt32(eventBuffer, 0));
                uint eventPid = unchecked((uint)Marshal.ReadInt32(eventBuffer, 4));
                uint eventTid = unchecked((uint)Marshal.ReadInt32(eventBuffer, 8));
                uint continueStatus = DbgContinue;
                bool shouldStop = false;
                BreakpointInfo hitBreakpoint = null;
                byte[] stoppedContext = null;
                try
                {
                    if (eventCode == CreateProcessDebugEvent)
                    {
                        IntPtr imageFile = Marshal.ReadIntPtr(eventBuffer, 12);
                        IntPtr processHandle = Marshal.ReadIntPtr(eventBuffer, 16);
                        IntPtr threadHandle = Marshal.ReadIntPtr(eventBuffer, 20);
                        string imagePath = PathForHandle(imageFile);
                        if (imagePath.Length != 0 && !String.Equals(imagePath, NormalizePath(expectedPath), StringComparison.OrdinalIgnoreCase))
                            throw new InvalidOperationException("CREATE_PROCESS image path mismatch: " + imagePath);
                        if (processHandle != IntPtr.Zero) CloseHandle(processHandle);
                        if (threadHandle != IntPtr.Zero) CloseHandle(threadHandle);
                        if (imageFile != IntPtr.Zero) CloseHandle(imageFile);
                        Log("CREATE_PROCESS path=" + imagePath);
                    }
                    else if (eventCode == CreateThreadDebugEvent)
                    {
                        IntPtr threadHandle = Marshal.ReadIntPtr(eventBuffer, 12);
                        if (threadHandle != IntPtr.Zero) CloseHandle(threadHandle);
                        Log("CREATE_THREAD tid=" + eventTid + " handleClosed=" + (threadHandle != IntPtr.Zero));
                    }
                    else if (eventCode == ExitThreadDebugEvent)
                    {
                        uint threadExitCode = unchecked((uint)Marshal.ReadInt32(eventBuffer, 12));
                        Log("EXIT_THREAD tid=" + eventTid + " exitCode=0x" + threadExitCode.ToString("X8"));
                    }
                    else if (eventCode == LoadDllDebugEvent)
                    {
                        IntPtr imageFile = Marshal.ReadIntPtr(eventBuffer, 12);
                        try
                        {
                        uint imageBase = unchecked((uint)Marshal.ReadInt32(eventBuffer, 16));
                        string imagePath = PathForHandle(imageFile);
                        if (imagePath.Length != 0 && String.Equals(imagePath, NormalizePath(expectedTerrain), StringComparison.OrdinalIgnoreCase))
                        {
                            string hash = HashFile(imagePath);
                            if (!String.Equals(hash, ExpectedSha256[TerrainName], StringComparison.OrdinalIgnoreCase))
                                throw new InvalidOperationException("Loaded Terrain hash mismatch: " + hash);
                            terrainBase = imageBase;
                            terrainModuleSha256 = hash;
                            byte atmosphereByte;
                            string atmosphereEvidence;
                            if (!VerifyAtmospherePhaseBoundary(process, terrainBase, out atmosphereByte, out atmosphereEvidence))
                                throw new InvalidOperationException("Terrain atmosphere phase boundary signature failed: " + atmosphereEvidence);
                            atmosphereBreakpoint = ArmBreakpoint(process, "Terrain.atmosphere-phase",
                                unchecked(terrainBase + AtmospherePhaseRva), atmosphereByte);
                            Log("Terrain verified base=0x" + terrainBase.ToString("X8") + " sha256=" + hash
                                + " " + atmosphereEvidence);
                        }
                        if (imagePath.Length != 0 && String.Equals(imagePath, NormalizePath(expectedIron3d), StringComparison.OrdinalIgnoreCase))
                        {
                            string loadedPath = ModulePath(process, imageBase);
                            string hash = HashFile(loadedPath);
                            if (!String.Equals(hash, ExpectedSha256[Iron3dName], StringComparison.OrdinalIgnoreCase))
                                throw new InvalidOperationException("Loaded iron3d hash mismatch: " + hash);
                            byte missionByte;
                            string missionEvidence;
                            if (!VerifyMissionIdentityBoundary(process, imageBase, out missionByte, out missionEvidence))
                                throw new InvalidOperationException("iron3d mission path boundary signature failed: " + missionEvidence);
                            iron3dBase = imageBase;
                            missionBreakpoint = ArmBreakpoint(process, "iron3d.mission-path-vtable-call",
                                unchecked(imageBase + MissionVtableCallRva), missionByte);
                            Log("iron3d verified sha256=" + hash + " " + missionEvidence);
                        }
                        if (imagePath.Length != 0 && String.Equals(imagePath, NormalizePath(expectedWorld3D), StringComparison.OrdinalIgnoreCase))
                        {
                            string loadedPath = ModulePath(process, imageBase);
                            string hash = HashFile(loadedPath);
                            if (!String.Equals(hash, ExpectedSha256[World3DName], StringComparison.OrdinalIgnoreCase))
                                throw new InvalidOperationException("Loaded World3D hash mismatch: " + hash);
                            byte renderByte;
                            byte returnByte = 0;
                            byte projectionByte;
                            string renderEvidence;
                            string returnEvidence = "";
                            string projectionEvidence;
                            if (!VerifyRenderEntry(process, imageBase, out renderByte, out renderEvidence))
                                throw new InvalidOperationException("World3D render-entry signature failed: " + renderEvidence);
                            if (!VerifyProjectionBoundary(process, imageBase, out projectionByte, out projectionEvidence))
                                throw new InvalidOperationException("World3D projection signature failed: " + projectionEvidence);
                            if (selectedCameraInput != null
                                && !VerifyRenderReturnBoundary(process, imageBase, out returnByte, out returnEvidence))
                                throw new InvalidOperationException("World3D selected-camera restore boundary failed: " + returnEvidence);
                            world3dBase = imageBase;
                            cameraBreakpoint = ArmBreakpoint(process, "World3D.stdRenderGame", imageBase + RenderGameRva, renderByte);
                            projectionBreakpoint = ArmBreakpoint(process, "World3D.projection-snapshot", imageBase + ProjectionSnapshotRva, projectionByte);
                            if (selectedCameraInput != null)
                                returnBreakpoint = ArmBreakpoint(process, "World3D.stdRenderGame-restore-return",
                                    imageBase + RenderReturnRva, returnByte);
                            Log("World3D verified sha256=" + hash + " " + renderEvidence + " " + projectionEvidence
                                + (selectedCameraInput == null ? "" : " " + returnEvidence));
                        }
                        if (imagePath.Length != 0 && String.Equals(imagePath, NormalizePath(expectedNgi32), StringComparison.OrdinalIgnoreCase))
                        {
                            string loadedPath = ModulePath(process, imageBase);
                            string hash = HashFile(loadedPath);
                            if (!String.Equals(hash, ExpectedSha256[Ngi32Name], StringComparison.OrdinalIgnoreCase))
                                throw new InvalidOperationException("Loaded Ngi32 hash mismatch: " + hash);
                            byte presentByte;
                            string presentEvidence;
                            if (!VerifyPresentBoundary(process, imageBase, out presentByte, out presentEvidence))
                                throw new InvalidOperationException("Ngi32 present signature failed: " + presentEvidence);
                            ngiBase = imageBase;
                            presentOriginalByte = presentByte;
                            presentBoundaryVerified = true;
                            Log("Ngi32 verified sha256=" + hash + " " + presentEvidence);
                            if (IsMatchingPerspectiveFrame(frame, frame.CameraThreadId))
                                EnsurePresentBreakpoint(process, ngiBase, frame, frame.CameraThreadId,
                                    presentOriginalByte, ref presentBreakpoint);
                        }
                        }
                        finally { if (imageFile != IntPtr.Zero) CloseHandle(imageFile); }
                    }
                    else if (eventCode == ExceptionDebugEvent)
                    {
                        uint exceptionCode = unchecked((uint)Marshal.ReadInt32(eventBuffer, 12));
                        uint exceptionAddress = unchecked((uint)Marshal.ReadInt32(eventBuffer, 24));
                        if (cameraSetter != null)
                        {
                            bool expectedTrap = exceptionCode == 0x80000003
                                && exceptionAddress == cameraSetter.Code.TrapAddress
                                && eventTid == cameraSetter.ThreadId;
                            if (!expectedTrap)
                            {
                                Log("CAMERA_SETTER_UNEXPECTED_EXCEPTION phase=" + cameraSetter.Phase
                                    + " code=0x" + exceptionCode.ToString("X8")
                                    + " address=0x" + exceptionAddress.ToString("X8") + " tid=" + eventTid
                                    + "; terminating the verified scratch process");
                                TerminateProcess(process, 0xE00B);
                                fatal = true;
                            }
                            else
                            {
                                string returnEvidence;
                                bool returnContextOk = VerifyCameraSetterReturnContext(process, cameraSetter, out returnEvidence);
                                byte[] actualMatrix = ReadCameraMatrix(process, cameraSetter.Camera);
                                bool matrixMatches = ByteArraysEqual(actualMatrix, cameraSetter.MatrixBytes);
                                string abiEvidence;
                                uint transformInterface;
                                bool abiStillValid = VerifyCameraSetterAbi(process, terrainBase,
                                    cameraSetter.Camera, out transformInterface, out abiEvidence);
                                Log("CAMERA_SETTER_RETURN phase=" + cameraSetter.Phase
                                    + " contextOk=" + returnContextOk + " matrixMatches=" + matrixMatches
                                    + " abiStillValid=" + abiStillValid + " " + returnEvidence + " " + abiEvidence);
                                if (!returnContextOk || !matrixMatches || !abiStillValid)
                                {
                                    captureFailure = "Native camera setter could not be verified; the isolated scratch process will be stopped.";
                                    Log("CAMERA_SETTER_UNVERIFIED; terminating the verified scratch process before detach");
                                    TerminateProcess(process, 0xE00C);
                                    fatal = true;
                                }
                                else if (cameraSetter.Phase == CameraSetterPhase.Apply)
                                {
                                    CameraReadback appliedCamera = frame.Camera;
                                    if (selectedCamera == null || appliedCamera == null || !appliedCamera.LayoutVerified
                                        || appliedCamera.Camera != selectedCamera.CameraPointer
                                        || !ByteArraysEqual(actualMatrix, selectedCamera.Input.MatrixBytes))
                                    {
                                        captureFailure = "Native setter did not leave the requested selected-camera matrix in place.";
                                        Log("CAMERA_SETTER_APPLY_READBACK_FAILED; terminating the verified scratch process");
                                        TerminateProcess(process, 0xE00D);
                                        fatal = true;
                                    }
                                    else
                                    {
                                        appliedCamera.EntryMatrixBytes = (byte[])selectedCamera.Input.MatrixBytes.Clone();
                                        appliedCamera.EntryMatrixSha256 = selectedCamera.Input.MatrixSha256;
                                        appliedCamera.ProjectionMatrixSha256 = null;
                                        appliedCamera.CurrentAtProjectionVerified = false;
                                        appliedCamera.WordsChangedAtProjection = false;
                                        appliedCamera.Words = (uint[])selectedCamera.Input.MatrixWords.Clone();
                                        appliedCamera.MatrixFinite = true;
                                        selectedCamera.Applied = true;
                                        selectedCamera.RestoreDeadlineUtc = DateTime.UtcNow.AddSeconds(5);
                                        frame.SelectedCamera = selectedCamera;
                                        frame.Camera = appliedCamera;
                                        Log("CAMERA_SETTER_APPLY_VERIFIED generation=" + selectedCamera.Generation
                                            + " camera=0x" + selectedCamera.CameraPointer.ToString("X8")
                                            + " matrixSha256=" + selectedCamera.Input.MatrixSha256
                                            + " originalSha256=" + selectedCamera.OriginalMatrixSha256);
                                        BeginSingleStep(process, eventTid, cameraSetter.ResumeBreakpoint,
                                            cameraSetter.OriginalContext, out pendingStep, cameraSetter.RearmOnStep);
                                        Log("SINGLE_STEP_STARTED boundary=" + cameraSetter.ResumeBreakpoint.Name + " tid=" + eventTid);
                                        if (VirtualFreeEx(process, Ptr(cameraSetter.Region), UIntPtr.Zero, 0x8000))
                                            Log("CAMERA_SETTER_REGION_FREED phase=Apply");
                                        else Log("CAMERA_SETTER_REGION_FREE_FAILED phase=Apply error=" + Marshal.GetLastWin32Error());
                                        cameraSetter = null;
                                    }
                                }
                                else
                                {
                                    if (selectedCamera == null || !ByteArraysEqual(actualMatrix, selectedCamera.OriginalMatrixBytes))
                                    {
                                        captureFailure = "The original native camera matrix was not restored exactly.";
                                        Log("CAMERA_SETTER_RESTORE_READBACK_FAILED; terminating the verified scratch process");
                                        TerminateProcess(process, 0xE00E);
                                        fatal = true;
                                    }
                                    else
                                    {
                                        selectedCamera.Restored = true;
                                        selectedCamera.RestoreVerified = true;
                                        selectedCamera.RestoreDeadlineUtc = DateTime.MinValue;
                                        Log("CAMERA_SETTER_RESTORE_VERIFIED camera=0x" + selectedCamera.CameraPointer.ToString("X8")
                                            + " matrixSha256=" + selectedCamera.OriginalMatrixSha256
                                            + " returnBoundary=World3D+0x13D7B");
                                        BeginSingleStep(process, eventTid, cameraSetter.ResumeBreakpoint,
                                            cameraSetter.OriginalContext, out pendingStep, cameraSetter.RearmOnStep);
                                        Log("SINGLE_STEP_STARTED boundary=" + cameraSetter.ResumeBreakpoint.Name + " tid=" + eventTid);
                                        if (VirtualFreeEx(process, Ptr(cameraSetter.Region), UIntPtr.Zero, 0x8000))
                                            Log("CAMERA_SETTER_REGION_FREED phase=Restore");
                                        else Log("CAMERA_SETTER_REGION_FREE_FAILED phase=Restore error=" + Marshal.GetLastWin32Error());
                                        cameraSetter = null;
                                    }
                                }
                            }
                        }
                        else if (remote != null)
                        {
                            uint expectedTrap = remote.Phase == RemoteReadbackPhase.Acquire
                                ? remote.Acquire.TrapAddress : remote.Cleanup.TrapAddress;
                            if (exceptionCode != 0x80000003 || exceptionAddress != expectedTrap || eventTid != remote.ThreadId)
                            {
                                Log("REMOTE_STUB_UNEXPECTED_EXCEPTION code=0x" + exceptionCode.ToString("X8")
                                    + " address=0x" + exceptionAddress.ToString("X8") + " tid=" + eventTid);
                                TerminateProcess(process, 0xE003);
                                fatal = true;
                            }
                            else if (remote.Phase == RemoteReadbackPhase.Acquire)
                            {
                                uint data = remote.Acquire.DataBase;
                                remote.Status = ReadU32Exact(process, data + RemoteDataStatusOffset);
                                remote.GetHr = ReadU32Exact(process, data + RemoteDataGetHrOffset);
                                remote.LockHr = ReadU32Exact(process, data + RemoteDataLockHrOffset);
                                remote.ReleaseResult = ReadU32Exact(process, data + RemoteDataReleaseCountOffset);
                                Log("REMOTE_ACQUIRE_DONE status=" + remote.Status + " getHR=0x" + remote.GetHr.ToString("X8")
                                    + " lockHR=0x" + remote.LockHr.ToString("X8") + " release=0x" + remote.ReleaseResult.ToString("X8"));
                                remote.PresentStackArgumentsIntact = remote.PresentStackArgumentsIntact
                                    && VerifyPresentStackArguments(process, remote, "after-get-render-target-and-lock");
                                remote.RemoteContextIntact = remote.RemoteContextIntact
                                    && VerifyRemoteStubContext(process, remote, unchecked(remote.Acquire.TrapAddress + 1),
                                        "after-get-render-target-and-lock");
                                if (remote.Status == 1)
                                {
                                    try { remote.Surface = ReadSurfaceRows(process, data + RemoteDataDescOffset); }
                                    catch (Exception pixelError) { remote.PixelFailure = pixelError.Message; }
                                    BeginRemoteCleanup(process, remote);
                                }
                                else if (!remote.PresentStackArgumentsIntact || !remote.RemoteContextIntact)
                                {
                                    captureFailure = "Present call stack arguments changed during D3D7 readback.";
                                    Log("PRESENT_STACK_CORRUPTION_UNRECOVERED; terminating the verified scratch process before detach");
                                    fatal = true;
                                }
                                else
                                {
                                    captureFailure = "D3D7 render-target readback returned status " + remote.Status
                                        + " (GetRenderTarget 0x" + remote.GetHr.ToString("X8")
                                        + ", Lock 0x" + remote.LockHr.ToString("X8") + ").";
                                    Log("REMOTE_CAPTURE_RETRY " + captureFailure);
                                    ResumeOriginalPresent(process, remote);
                                    VirtualFreeEx(process, Ptr(remote.Region), UIntPtr.Zero, 0x8000);
                                    remote = null;
                                }
                            }
                            else
                            {
                                uint data = remote.Cleanup.DataBase;
                                remote.Status = ReadU32Exact(process, data + RemoteDataStatusOffset);
                                remote.UnlockHr = ReadU32Exact(process, data + RemoteDataUnlockHrOffset);
                                remote.ReleaseResult = ReadU32Exact(process, data + RemoteDataReleaseCountOffset);
                                Log("REMOTE_CLEANUP_DONE status=" + remote.Status + " unlockHR=0x" + remote.UnlockHr.ToString("X8")
                                    + " release=0x" + remote.ReleaseResult.ToString("X8"));
                                bool unlocked = remote.Status == 6 && unchecked((int)remote.UnlockHr) >= 0;
                                remote.PresentStackArgumentsIntact = remote.PresentStackArgumentsIntact
                                    && VerifyPresentStackArguments(process, remote, "after-unlock-and-release");
                                remote.RemoteContextIntact = remote.RemoteContextIntact
                                    && VerifyRemoteStubContext(process, remote, unchecked(remote.Cleanup.TrapAddress + 1),
                                        "after-unlock-and-release");
                                bool outputStaged = false;
                                if (!unlocked)
                                {
                                    captureFailure = "D3D7 Unlock failed; the render target may remain locked (status "
                                        + remote.Status + ", HRESULT 0x" + remote.UnlockHr.ToString("X8") + ").";
                                    Log("FRAME_CAPTURE_NOT_USABLE " + (remote.PixelFailure ?? captureFailure));
                                    Log("UNLOCK_FAILURE_UNRECOVERED; terminating the verified scratch process before detach");
                                    fatal = true;
                                    // Keep the session non-null. The final recovery path terminates this
                                    // exact scratch process and waits for exit before it detaches.
                                }
                                else if (!remote.PresentStackArgumentsIntact || !remote.RemoteContextIntact)
                                {
                                    captureFailure = "Present call stack arguments changed during D3D7 readback.";
                                    Log("PRESENT_STACK_CORRUPTION_UNRECOVERED; terminating the verified scratch process before detach");
                                    fatal = true;
                                }
                                else
                                {
                                    if (remote.Surface != null)
                                    {
                                        if (completedSurface != null || completedFrame != null || completedReadback != null)
                                        {
                                            captureFailure = "A second frame readback completed before the staged frame could be safely finalized.";
                                            Log("FRAME_OUTPUT_STAGE_REJECTED " + captureFailure);
                                            stopAfterAttempt = true;
                                        }
                                        else
                                        {
                                            completedSurface = remote.Surface;
                                            completedFrame = remote.Frame;
                                            completedReadback = remote;
                                            outputStaged = true;
                                            Log("FRAME_OUTPUT_STAGED generation=" + remote.Generation
                                                + " size=" + remote.Surface.Width + "x" + remote.Surface.Height
                                                + " bitCount=" + remote.Surface.BitCount + " rowsSha256=" + remote.Surface.Sha256);
                                        }
                                    }
                                    else
                                    {
                                        captureFailure = remote.PixelFailure ?? "D3D7 Lock succeeded, but pixel rows were not available.";
                                        Log("FRAME_CAPTURE_NOT_USABLE " + captureFailure);
                                        stopAfterAttempt = true;
                                    }

                                    ResumeOriginalPresent(process, remote);
                                    if (VirtualFreeEx(process, Ptr(remote.Region), UIntPtr.Zero, 0x8000))
                                        Log("REMOTE_READBACK_REGION_FREED");
                                    else Log("REMOTE_READBACK_REGION_FREE_FAILED error=" + Marshal.GetLastWin32Error());
                                    remote = null;
                                    if (outputStaged)
                                    {
                                        captured = true;
                                        Log("FRAME_CAPTURE_CONTEXT_RESTORED; awaiting camera restore and safe detach before writing outputs");
                                    }
                                }
                            }
                        }
                        else if (exceptionCode == 0x80000004 && pendingStep != null)
                        {
                            FinishSingleStep(process, eventTid, pendingStep);
                            Log("SINGLE_STEP_COMPLETE boundary=" + pendingStep.Breakpoint.Name + " tid=" + eventTid);
                            pendingStep = null;
                        }
                        else if (exceptionCode == 0x80000003)
                        {
                            if (cameraBreakpoint != null && cameraBreakpoint.Armed && exceptionAddress == cameraBreakpoint.Address)
                                hitBreakpoint = cameraBreakpoint;
                            else if (projectionBreakpoint != null && projectionBreakpoint.Armed && exceptionAddress == projectionBreakpoint.Address)
                                hitBreakpoint = projectionBreakpoint;
                            else if (returnBreakpoint != null && returnBreakpoint.Armed && exceptionAddress == returnBreakpoint.Address)
                                hitBreakpoint = returnBreakpoint;
                            else if (atmosphereBreakpoint != null && atmosphereBreakpoint.Armed && exceptionAddress == atmosphereBreakpoint.Address)
                                hitBreakpoint = atmosphereBreakpoint;
                            else if (missionBreakpoint != null && missionBreakpoint.Armed && exceptionAddress == missionBreakpoint.Address)
                                hitBreakpoint = missionBreakpoint;
                            else if (presentBreakpoint != null && presentBreakpoint.Armed && exceptionAddress == presentBreakpoint.Address)
                                hitBreakpoint = presentBreakpoint;

                            if (hitBreakpoint != null)
                            {
                                IntPtr thread = OpenThread(ThreadGetContext | ThreadSetContext, false, eventTid);
                                if (thread == IntPtr.Zero) throw new InvalidOperationException("OpenThread at " + hitBreakpoint.Name + " failed: " + Marshal.GetLastWin32Error());
                                try { stoppedContext = GetFullX86Context(thread); }
                                finally { CloseHandle(thread); }
                                if (unchecked(BitConverter.ToUInt32(stoppedContext, ContextEipOffset)) != unchecked(hitBreakpoint.Address + 1))
                                    throw new InvalidOperationException(hitBreakpoint.Name + " breakpoint EIP did not point past INT3.");

                                if (hitBreakpoint == cameraBreakpoint)
                                {
                                    if (selectedCamera != null && selectedCamera.Applied)
                                    {
                                        if (!selectedCamera.PixelAttributionInvalidated)
                                        {
                                            selectedCamera.PixelAttributionInvalidated = true;
                                            selectedCamera.PixelAttributionFailure = "An additional World3D.stdRenderGame invocation occurred before the next present boundary.";
                                            captureFailure = selectedCamera.PixelAttributionFailure;
                                            stopAfterAttempt = true;
                                            Log("SELECTED_PIXEL_ATTRIBUTION_INVALIDATED tid=" + eventTid
                                                + " esp=0x" + BitConverter.ToUInt32(stoppedContext, ContextEspOffset).ToString("X8")
                                                + " selectedGeneration=" + selectedCamera.Generation
                                                + " restoreVerified=" + selectedCamera.RestoreVerified);
                                        }
                                        Log("CAMERA_ENTRY_SKIPPED_SELECTED_FRAME_PENDING tid=" + eventTid);
                                    }
                                    else
                                    {
                                        frame.CameraGeneration++;
                                        frame.CameraThreadId = eventTid;
                                        frame.CameraWorldGameTimeWordReadable = false;
                                        if (world3dBase != 0)
                                        {
                                            try
                                            {
                                                frame.CameraWorldGameTimeWord = ReadU32Exact(process, unchecked(world3dBase + 0x32A38));
                                                frame.CameraWorldGameTimeWordReadable = true;
                                            }
                                            catch (Exception clockError)
                                            {
                                                Log("CAMERA_WORLD_GAME_TIME_UNREADABLE " + clockError.Message);
                                            }
                                        }
                                        frame.Projection = null;
                                        frame.ProjectionGeneration = 0;
                                        try { frame.Camera = ReadCameraSnapshot(process, eventTid, terrainBase); }
                                        catch (Exception cameraError) { frame.Camera = null; Log("CAMERA_SNAPSHOT_FAILED " + cameraError.Message); }
                                        Log("CAMERA_GENERATION " + frame.CameraGeneration + " tid=" + eventTid
                                            + " pointer=" + (frame.Camera == null ? "unreadable" : "0x" + frame.Camera.Camera.ToString("X8"))
                                            + " layoutVerified=" + (frame.Camera != null && frame.Camera.LayoutVerified)
                                            + " matrixSha256=" + (frame.Camera == null ? "unreadable" : frame.Camera.EntryMatrixSha256)
                                            + " entryEsp=0x" + (frame.Camera == null ? 0 : frame.Camera.EntryEsp).ToString("X8")
                                            + " return=0x" + (frame.Camera == null ? 0 : frame.Camera.ReturnAddress).ToString("X8"));

                                        if (selectedCamera != null && !selectedCamera.Applied && frame.Camera != null
                                            && frame.Camera.LayoutVerified)
                                        {
                                            if (selectedCamera.CandidateProjectionVerified
                                                && frame.Camera.Camera == selectedCamera.CandidateCameraPointer
                                                && eventTid == selectedCamera.CandidateThreadId
                                                && frame.CameraGeneration != selectedCamera.CandidateGeneration)
                                            {
                                                if (frame.Camera.EntryEsp < RenderFunctionStackFrameBytes)
                                                    throw new InvalidOperationException("Selected camera render stack pointer underflowed the verified prologue size.");
                                                selectedCamera.CameraPointer = frame.Camera.Camera;
                                                selectedCamera.ThreadId = eventTid;
                                                selectedCamera.Generation = frame.CameraGeneration;
                                                selectedCamera.EntryEsp = frame.Camera.EntryEsp;
                                                selectedCamera.FunctionStackEsp = frame.Camera.EntryEsp - RenderFunctionStackFrameBytes;
                                                selectedCamera.ReturnAddress = frame.Camera.ReturnAddress;
                                                selectedCamera.OriginalMatrixBytes = (byte[])frame.Camera.EntryMatrixBytes.Clone();
                                                selectedCamera.OriginalMatrixSha256 = frame.Camera.EntryMatrixSha256;
                                                frame.SelectedCamera = selectedCamera;
                                                cameraSetter = StartCameraSetter(process, eventTid, terrainBase,
                                                    selectedCamera.CameraPointer, selectedCamera.Input.MatrixBytes,
                                                    stoppedContext, cameraBreakpoint, true, CameraSetterPhase.Apply,
                                                    selectedCamera.EntryEsp, selectedCamera.ReturnAddress);
                                                Log("SELECTED_CAMERA_APPLY_STARTED generation=" + selectedCamera.Generation
                                                    + " candidateGeneration=" + selectedCamera.CandidateGeneration
                                                    + " camera=0x" + selectedCamera.CameraPointer.ToString("X8")
                                                    + " entryEsp=0x" + selectedCamera.EntryEsp.ToString("X8")
                                                    + " return=0x" + selectedCamera.ReturnAddress.ToString("X8")
                                                    + " originalSha256=" + selectedCamera.OriginalMatrixSha256
                                                    + " requestedSha256=" + selectedCamera.Input.MatrixSha256);
                                            }
                                            else if (!selectedCamera.CandidateProjectionVerified)
                                            {
                                                selectedCamera.CandidateCameraPointer = frame.Camera.Camera;
                                                selectedCamera.CandidateThreadId = eventTid;
                                                selectedCamera.CandidateGeneration = frame.CameraGeneration;
                                                frame.SelectedCamera = selectedCamera;
                                                Log("SELECTED_CAMERA_PREFLIGHT_CANDIDATE generation=" + selectedCamera.CandidateGeneration
                                                    + " camera=0x" + selectedCamera.CandidateCameraPointer.ToString("X8")
                                                    + " tid=" + selectedCamera.CandidateThreadId
                                                    + " matrixSha256=" + frame.Camera.EntryMatrixSha256);
                                            }
                                            else
                                            {
                                                Log("SELECTED_CAMERA_WAITING_FOR_MATCHING_CANDIDATE tid=" + eventTid
                                                    + " camera=0x" + frame.Camera.Camera.ToString("X8")
                                                    + " candidateTid=" + selectedCamera.CandidateThreadId
                                                    + " candidateCamera=0x" + selectedCamera.CandidateCameraPointer.ToString("X8"));
                                            }
                                        }
                                        else if (selectedCamera != null && !selectedCamera.Applied && frame.Camera != null)
                                            Log("SELECTED_CAMERA_CANDIDATE_REJECTED tid=" + eventTid
                                                + " layoutVerified=" + frame.Camera.LayoutVerified
                                                + " matrixSha256=" + frame.Camera.EntryMatrixSha256
                                                + " requestedSha256=" + selectedCamera.Input.MatrixSha256);
                                    }
                                }
                                else if (hitBreakpoint == projectionBreakpoint)
                                {
                                    string candidateProjectionEvidence = "candidate preconditions not met";
                                    string selectedProjectionEvidence = "selected invocation was not checked";
                                    bool selectedCandidateScope = selectedCamera != null && !selectedCamera.Applied
                                        && !selectedCamera.CandidateProjectionVerified
                                        && frame.Camera != null
                                        && frame.Camera.Camera == selectedCamera.CandidateCameraPointer
                                        && frame.CameraGeneration == selectedCamera.CandidateGeneration
                                        && eventTid == selectedCamera.CandidateThreadId
                                        && frame.Camera.EntryEsp >= RenderFunctionStackFrameBytes
                                        && IsRenderInvocation(process, stoppedContext, eventTid,
                                            selectedCamera.CandidateThreadId,
                                            frame.Camera.Camera, frame.Camera.EntryEsp,
                                            frame.Camera.EntryEsp - RenderFunctionStackFrameBytes,
                                            frame.Camera.ReturnAddress, unchecked(projectionBreakpoint.Address + 1),
                                            true,
                                            out candidateProjectionEvidence);
                                    bool selectedProjectionScope = selectedCamera == null
                                        ? !frame.SelectedCameraRequested
                                        : (selectedCamera.Applied
                                            ? IsSelectedRenderInvocation(process, stoppedContext, eventTid,
                                                selectedCamera, unchecked(projectionBreakpoint.Address + 1),
                                                true,
                                                out selectedProjectionEvidence)
                                            : selectedCandidateScope);
                                    if (!selectedProjectionScope)
                                    {
                                        if (selectedCamera != null && !selectedCamera.ProjectionGateDiagnosticLogged)
                                        {
                                            selectedCamera.ProjectionGateDiagnosticLogged = true;
                                            Log("PROJECTION_SKIPPED_OUTSIDE_SELECTED_RENDER_INVOCATION tid=" + eventTid
                                                + " applied=" + selectedCamera.Applied
                                                + " candidateGeneration=" + selectedCamera.CandidateGeneration
                                                + " candidateTid=" + selectedCamera.CandidateThreadId
                                                + " candidateCamera=0x" + selectedCamera.CandidateCameraPointer.ToString("X8")
                                                + " selectedGeneration=" + selectedCamera.Generation
                                                + " selectedTid=" + selectedCamera.ThreadId
                                                + " selectedCamera=0x" + selectedCamera.CameraPointer.ToString("X8")
                                                + " candidateEvidence=" + candidateProjectionEvidence
                                                + " selectedEvidence=" + selectedProjectionEvidence);
                                        }
                                    }
                                    else
                                    {
                                        frame.Projection = null;
                                        frame.ProjectionGeneration = 0;
                                        if (frame.Camera != null && frame.CameraThreadId == eventTid && ngiBase != 0)
                                        {
                                            bool cameraWordsCurrent = RefreshCameraWordsAtProjection(process, frame.Camera,
                                                terrainBase, frame.CameraGeneration);
                                            frame.Projection = ReadProjection(process, ngiBase);
                                            frame.ProjectionThreadId = eventTid;
                                            frame.ProjectionGeneration = frame.CameraGeneration;
                                            frame.WorldGameTimeWordReadable = false;
                                            try
                                            {
                                                frame.WorldGameTimeWord = ReadU32Exact(process, unchecked(world3dBase + 0x32A38));
                                                frame.WorldGameTimeWordReadable = true;
                                            }
                                            catch (Exception clockError)
                                            {
                                                Log("PROJECTION_WORLD_GAME_TIME_UNREADABLE " + clockError.Message);
                                            }
                                            PairAtmosphereToProjection(frame, eventTid);
                                            bool cameraMatrixMatchesInput = selectedCamera == null || !selectedCamera.Applied
                                                || SelectedMatrixMatchesProjection(frame);
                                            bool projectionMatchesInput = selectedCamera == null
                                                || ProjectionMatchesCameraInput(frame.Projection, selectedCamera.Input)
                                                    && cameraMatrixMatchesInput;
                                            if (selectedCamera != null)
                                            {
                                                if (selectedCamera.Applied)
                                                    selectedCamera.ProjectionMatchesInput = projectionMatchesInput;
                                                else
                                                {
                                                    selectedCamera.CandidateProjectionVerified = cameraWordsCurrent
                                                        && frame.Projection.Verified && frame.Projection.Mode != 0
                                                        && projectionMatchesInput;
                                                    Log("SELECTED_CAMERA_PREFLIGHT_PROJECTION verified="
                                                        + selectedCamera.CandidateProjectionVerified
                                                        + " generation=" + selectedCamera.CandidateGeneration
                                                        + " camera=0x" + selectedCamera.CandidateCameraPointer.ToString("X8")
                                                        + " tid=" + selectedCamera.CandidateThreadId);
                                                }
                                                Log("SELECTED_CAMERA_PROJECTION_MATCH " + projectionMatchesInput
                                                    + " cameraMatrixMatchesInput=" + cameraMatrixMatchesInput
                                                    + " inputViewport=" + String.Join(",", selectedCamera.Input.Viewport)
                                                    + " actualViewport=" + (frame.Projection.Viewport == null ? "unreadable" : String.Join(",", frame.Projection.Viewport))
                                                    + " inputNear=" + selectedCamera.Input.Near.ToString("R", CultureInfo.InvariantCulture)
                                                    + " actualNear=" + frame.Projection.Near.ToString("R", CultureInfo.InvariantCulture)
                                                    + " inputFar=" + selectedCamera.Input.Far.ToString("R", CultureInfo.InvariantCulture)
                                                    + " actualFar=" + frame.Projection.Far.ToString("R", CultureInfo.InvariantCulture)
                                                    + " inputFov=" + selectedCamera.Input.FieldOfView.ToString("R", CultureInfo.InvariantCulture)
                                                    + " actualFov=" + frame.Projection.Fov.ToString("R", CultureInfo.InvariantCulture));
                                            }
                                            Log("PROJECTION_GENERATION " + frame.ProjectionGeneration + " tid=" + eventTid
                                                + " renderer=0x" + frame.Projection.Renderer.ToString("X8")
                                                + " viewport=" + (frame.Projection.Viewport == null ? "unreadable" : String.Join(",", frame.Projection.Viewport))
                                                + " mode=" + frame.Projection.Mode + " cameraWordsCurrent=" + cameraWordsCurrent
                                                + " usable=" + (cameraWordsCurrent && frame.Projection.Verified && frame.Projection.Mode != 0
                                                    && projectionMatchesInput)
                                                + " rawWorldTime=0x" + frame.WorldGameTimeWord.ToString("X8"));
                                            if (cameraWordsCurrent && frame.Projection.Verified && frame.Projection.Mode != 0
                                                && projectionMatchesInput && ngiBase != 0 && presentBoundaryVerified)
                                                EnsurePresentBreakpoint(process, ngiBase, frame, eventTid,
                                                    presentOriginalByte, ref presentBreakpoint);
                                        }
                                    }
                                    if (frame.Camera == null || frame.CameraThreadId != eventTid || ngiBase == 0)
                                        Log("PROJECTION_SKIPPED without loaded renderer and same-thread camera snapshot tid=" + eventTid);
                                }
                                else if (hitBreakpoint == returnBreakpoint)
                                {
                                    if (selectedCamera != null && selectedCamera.Applied && !selectedCamera.Restored)
                                    {
                                        string returnEvidence;
                                        // World3D+0x13D47 repurposes ESI for registry traversal before this epilogue.
                                        bool returnInvocationMatches = IsSelectedRenderInvocation(process,
                                            stoppedContext, eventTid, selectedCamera,
                                            unchecked(returnBreakpoint.Address + 1), false, out returnEvidence);
                                        if (!returnInvocationMatches && !selectedCamera.ReturnGateDiagnosticLogged)
                                        {
                                            selectedCamera.ReturnGateDiagnosticLogged = true;
                                            Log("SELECTED_CAMERA_RETURN_GATE_REJECTED " + returnEvidence
                                                + " selectedGeneration=" + selectedCamera.Generation
                                                + " selectedTid=" + selectedCamera.ThreadId
                                                + " selectedCamera=0x" + selectedCamera.CameraPointer.ToString("X8"));
                                        }
                                        if (returnInvocationMatches)
                                        {
                                        try
                                        {
                                            byte[] selectedAtReturn = ReadCameraMatrix(process, selectedCamera.CameraPointer);
                                            selectedCamera.MatrixIntactAtReturn = ByteArraysEqual(selectedAtReturn,
                                                selectedCamera.Input.MatrixBytes);
                                        }
                                        catch (Exception matrixReadError)
                                        {
                                            selectedCamera.MatrixIntactAtReturn = false;
                                            Log("SELECTED_CAMERA_RETURN_MATRIX_READ_FAILED " + matrixReadError.Message);
                                        }
                                        if (!selectedCamera.MatrixIntactAtReturn)
                                        {
                                            selectedCamera.PixelAttributionInvalidated = true;
                                            selectedCamera.PixelAttributionFailure = "Selected camera matrix changed or became unreadable before its verified render return.";
                                            captureFailure = selectedCamera.PixelAttributionFailure;
                                            stopAfterAttempt = true;
                                            Log("SELECTED_CAMERA_CHANGED_BEFORE_RETURN; restoring the original native matrix but rejecting pixels");
                                        }
                                        cameraSetter = StartCameraSetter(process, eventTid, terrainBase,
                                            selectedCamera.CameraPointer, selectedCamera.OriginalMatrixBytes,
                                            stoppedContext, returnBreakpoint, false, CameraSetterPhase.Restore,
                                            selectedCamera.EntryEsp, selectedCamera.ReturnAddress);
                                        Log("SELECTED_CAMERA_RESTORE_STARTED generation=" + selectedCamera.Generation
                                            + " camera=0x" + selectedCamera.CameraPointer.ToString("X8")
                                            + " returnEsp=0x" + BitConverter.ToUInt32(stoppedContext, ContextEspOffset).ToString("X8")
                                            + " matrixIntactAtReturn=" + selectedCamera.MatrixIntactAtReturn);
                                        }
                                    }
                                }
                                else if (hitBreakpoint == atmosphereBreakpoint)
                                {
                                    try
                                    {
                                        AtmosphereReadback sample = CaptureAtmospherePhase(process, terrainBase,
                                            world3dBase, stoppedContext, eventTid, terrainModuleSha256, frame);
                                        frame.AtmosphereByThread[eventTid] = sample;
                                        Log("ATMOSPHERE_PHASE_SAMPLE tid=" + eventTid
                                            + " cameraGenerationAtSample=" + sample.CameraGenerationAtSample
                                            + " cameraThreadAtSample=" + sample.CameraThreadIdAtSample
                                            + " esi=0x" + sample.AtmosphereObject.ToString("X8")
                                            + " ebpRaw=0x" + sample.RawClock.ToString("X8")
                                            + " edxPhaseMs=" + sample.PhaseMilliseconds
                                            + " origin=[esi+0x144]=" + sample.Origin
                                            + " periodMs=[esi+0x150]=" + sample.PeriodMilliseconds
                                            + " recomputedPhaseMs=" + sample.RecomputedPhaseMilliseconds
                                            + " worldGameTime=0x" + sample.WorldGameTimeWord.ToString("X8")
                                            + " arithmeticVerified=" + sample.ArithmeticVerified
                                            + " rawMatchesWorldTime=" + sample.WorldTimeMatchesRawClock
                                            + " terrainHashVerified=" + sample.TerrainModuleHashVerified);
                                    }
                                    catch (Exception atmosphereError)
                                    {
                                        Log("ATMOSPHERE_PHASE_SAMPLE_REJECTED tid=" + eventTid + " " + atmosphereError.Message);
                                    }
                                }
                                else if (hitBreakpoint == missionBreakpoint)
                                {
                                    missionProbeAttempts++;
                                    string missionPath;
                                    string missionEvidence;
                                    bool found = CaptureMissionIdentityAtCall(process, iron3dBase, stoppedContext,
                                        eventTid, out missionPath, out missionEvidence);
                                    if (found)
                                    {
                                        frame.MissionPath = missionPath;
                                        frame.MissionEvidence = missionEvidence;
                                        missionProbeFinished = true;
                                    }
                                    else if (missionProbeAttempts >= MissionProbeAttemptLimit)
                                    {
                                        missionProbeFinished = true;
                                        frame.MissionEvidence = "verified callsite reached " + missionProbeAttempts
                                            + " times, but no validated mission path was found; " + missionEvidence;
                                    }
                                    else frame.MissionEvidence = "verified callsite reached " + missionProbeAttempts
                                        + " times; no validated mission path yet; " + missionEvidence;
                                    Log("MISSION_IDENTITY_PROBE attempt=" + missionProbeAttempts + "/"
                                        + MissionProbeAttemptLimit + " found=" + found
                                        + " path=" + (missionPath ?? "unknown") + " evidence=" + missionEvidence);
                                }
                                else if (hitBreakpoint == presentBreakpoint)
                                {
                                    if (CanArmPresentForFrame(frame, eventTid))
                                    {
                                        try { remote = StartRemoteReadback(process, eventTid, ngiBase, presentBreakpoint, frame, outputPath); }
                                        catch (Exception startError)
                                        {
                                            captureFailure = startError.Message;
                                            Log("REMOTE_CAPTURE_START_FAILED " + startError);
                                            stopAfterAttempt = true;
                                        }
                                    }
                                    else Log("PRESENT_SKIPPED without matching verified perspective camera/projection");
                                }

                                if (remote == null && cameraSetter == null)
                                {
                                    bool rearmAfterStep = hitBreakpoint != presentBreakpoint
                                        && !(hitBreakpoint == missionBreakpoint && missionProbeFinished);
                                    BeginSingleStep(process, eventTid, hitBreakpoint, stoppedContext,
                                        out pendingStep, rearmAfterStep);
                                    Log("SINGLE_STEP_STARTED boundary=" + hitBreakpoint.Name + " tid=" + eventTid);
                                }
                            }
                            else
                            {
                                continueStatus = DbgContinue;
                            }
                        }
                        else
                        {
                            continueStatus = DbgExceptionNotHandled;
                        }
                    }
                    else if (eventCode == ExitProcessDebugEvent)
                    {
                        uint exitCode = unchecked((uint)Marshal.ReadInt32(eventBuffer, 12));
                        Log("PROCESS_EXIT while capturing exitCode=0x" + exitCode.ToString("X8"));
                        processExited = true;
                        remote = null;
                        shouldStop = true;
                    }
                }
                catch (Exception eventError)
                {
                    Log("FRAME_EVENT_ERROR " + eventError);
                    if (remote != null || cameraSetter != null)
                    {
                        TerminateProcess(process, cameraSetter != null ? 0xE00Cu : 0xE004u);
                        fatal = true;
                    }
                    else if (hitBreakpoint != null && stoppedContext != null)
                    {
                        try
                        {
                            if (hitBreakpoint.Armed)
                            {
                                if (!WriteByte(process, hitBreakpoint.Address, hitBreakpoint.OriginalByte))
                                    throw new InvalidOperationException("Could not restore failed boundary byte.");
                                hitBreakpoint.Armed = false;
                            }
                            IntPtr thread = OpenThread(ThreadGetContext | ThreadSetContext, false, eventTid);
                            if (thread == IntPtr.Zero) throw new InvalidOperationException("OpenThread for failed boundary recovery failed.");
                            try { SetFullX86Context(thread, ContextAtBreakpoint(stoppedContext, hitBreakpoint.Address, false)); }
                            finally { CloseHandle(thread); }
                            captureFailure = eventError.Message;
                            stopAfterAttempt = true;
                            Log("FAILED_BOUNDARY_RECOVERED boundary=" + hitBreakpoint.Name);
                        }
                        catch (Exception recoveryError)
                        {
                            Log("FATAL_BOUNDARY_RECOVERY_FAILED " + recoveryError);
                            TerminateProcess(process, 0xE005);
                            fatal = true;
                        }
                    }
                    else
                    {
                        continueStatus = DbgContinue;
                        captureFailure = eventError.Message;
                        stopAfterAttempt = true;
                        Log("FRAME_EVENT_ABORT_RECOVERED eventCode=" + eventCode + " error=" + eventError.Message);
                    }
                }

                if (!captured && !fatal && !processExited && DateTime.UtcNow >= deadline
                    && remote == null && cameraSetter == null && pendingStep == null)
                {
                    captureFailure = captureFailure ?? "No matching native camera/projection/present frame completed before the bounded timeout.";
                    Log("NO_FRAME_CAPTURE bounded timeout at stopped debug event");
                    if (!RestoreArmedBreakpointsWhileStopped(process,
                        cameraBreakpoint, projectionBreakpoint, returnBreakpoint, atmosphereBreakpoint, presentBreakpoint, missionBreakpoint))
                    {
                        Log("FATAL_TIMEOUT_BREAKPOINT_RESTORE_FAILED; terminating own verified scratch process");
                        TerminateProcess(process, 0xE007);
                        fatal = true;
                    }
                    else
                    {
                        stopAfterAttempt = true;
                    }
                }

                if (CanStopCapture(captured, stopAfterAttempt, remote != null || cameraSetter != null,
                    pendingStep != null, selectedCamera != null && selectedCamera.Applied && !selectedCamera.RestoreVerified)
                    && !fatal && !processExited
                    && (cameraBreakpoint != null && cameraBreakpoint.Armed
                        || projectionBreakpoint != null && projectionBreakpoint.Armed
                        || returnBreakpoint != null && returnBreakpoint.Armed
                        || atmosphereBreakpoint != null && atmosphereBreakpoint.Armed
                        || presentBreakpoint != null && presentBreakpoint.Armed
                        || missionBreakpoint != null && missionBreakpoint.Armed))
                {
                    if (!RestoreArmedBreakpointsWhileStopped(process,
                        cameraBreakpoint, projectionBreakpoint, returnBreakpoint, atmosphereBreakpoint, presentBreakpoint, missionBreakpoint))
                    {
                        Log("FATAL_STOP_BREAKPOINT_RESTORE_FAILED; terminating own verified scratch process");
                        TerminateProcess(process, 0xE008);
                        fatal = true;
                    }
                }

                if (!ContinueDebugEvent(eventPid, eventTid, continueStatus))
                {
                    fatal = true;
                    Log("FATAL ContinueDebugEvent failed=" + Marshal.GetLastWin32Error());
                    if (process != IntPtr.Zero && !TerminateProcess(process, 0xE009))
                        Log("FATAL ContinueDebugEvent recovery terminate failed=" + Marshal.GetLastWin32Error());
                    break;
                }
                if (shouldStop || CanStopCapture(captured, stopAfterAttempt, remote != null || cameraSetter != null,
                    pendingStep != null, selectedCamera != null && selectedCamera.Applied && !selectedCamera.RestoreVerified)) break;
            }
            if (!captured && !fatal && !stopAfterAttempt)
            {
                captureFailure = captureFailure ?? "No matching native camera/projection/present frame completed before the bounded timeout.";
                Log("NO_FRAME_CAPTURE bounded timeout");
            }
        }
        finally
        {
            bool cameraMutationUnrestored = selectedCamera != null && selectedCamera.Applied && !selectedCamera.RestoreVerified;
            if (!processExited && process != IntPtr.Zero && (remote != null || cameraSetter != null || pendingStep != null
                || cameraMutationUnrestored
                || cameraBreakpoint != null && cameraBreakpoint.Armed
                || projectionBreakpoint != null && projectionBreakpoint.Armed
                || returnBreakpoint != null && returnBreakpoint.Armed
                || atmosphereBreakpoint != null && atmosphereBreakpoint.Armed
                || presentBreakpoint != null && presentBreakpoint.Armed
                || missionBreakpoint != null && missionBreakpoint.Armed))
            {
                string state = remote != null ? "REMOTE_STUB_IN_FLIGHT" : (cameraSetter != null ? "CAMERA_SETTER_IN_FLIGHT"
                    : (cameraMutationUnrestored ? "SELECTED_CAMERA_MUTATION_NOT_RESTORED"
                    : (pendingStep != null ? "SINGLE_STEP_IN_FLIGHT" : "ARMED_BREAKPOINTS_WITHOUT_STOPPED_EVENT")));
                Log("FINAL_" + state + "; terminating only the path/tick/hash-verified scratch process before detach");
                if (!TerminateProcess(process, 0xE006))
                    Log("FINAL_TERMINATE_FAILED error=" + Marshal.GetLastWin32Error());
                if (WaitForSingleObject(process, 2000) == 0) processExited = true;
                else
                {
                    fatal = true;
                    processExitUnconfirmed = true;
                    Log("FATAL scratch termination was not confirmed before detach");
                }
            }
            if (attached && !processExitUnconfirmed)
            {
                detached = DebugActiveProcessStop(processId);
                if (detached) Log("DEBUG_DETACHED");
                else
                {
                    fatal = true;
                    int detachError = Marshal.GetLastWin32Error();
                    Log("FATAL DebugActiveProcessStop failed=" + detachError);
                    if (process != IntPtr.Zero)
                    {
                        uint waitBeforeRecovery = WaitForSingleObject(process, 0);
                        int waitBeforeError = waitBeforeRecovery == 0xFFFFFFFF ? Marshal.GetLastWin32Error() : 0;
                        uint exitBeforeRecovery;
                        bool exitRead = GetExitCodeProcess(process, out exitBeforeRecovery);
                        int exitBeforeError = exitRead ? 0 : Marshal.GetLastWin32Error();
                        Log("PROCESS_STATE_AFTER_DETACH_FAILURE waitResult=0x" + waitBeforeRecovery.ToString("X8")
                            + " exitCode=" + (exitRead ? "0x" + exitBeforeRecovery.ToString("X8") : "unreadable")
                            + " waitError=" + waitBeforeError + " exitError=" + exitBeforeError);
                        if (waitBeforeRecovery == 0x00000102 && exitRead && exitBeforeRecovery == 0x00000103)
                        {
                            Log("DETACH_FAILURE_TARGET_STILL_ACTIVE; terminating only exact verified scratch after state snapshot");
                            if (!TerminateProcess(process, 0xE00A))
                            {
                                int terminateError = Marshal.GetLastWin32Error();
                                Log("DETACH_FAILURE_TERMINATE_FAILED error=" + terminateError);
                            }
                            uint waitAfterRecovery = WaitForSingleObject(process, 2000);
                            int waitAfterError = waitAfterRecovery == 0xFFFFFFFF ? Marshal.GetLastWin32Error() : 0;
                            uint exitAfterRecovery;
                            bool exitAfterRead = GetExitCodeProcess(process, out exitAfterRecovery);
                            int exitAfterError = exitAfterRead ? 0 : Marshal.GetLastWin32Error();
                            Log("PROCESS_STATE_AFTER_DETACH_FAILURE_RECOVERY waitResult=0x" + waitAfterRecovery.ToString("X8")
                                + " exitCode=" + (exitAfterRead ? "0x" + exitAfterRecovery.ToString("X8") : "unreadable")
                                + " waitError=" + waitAfterError + " exitError=" + exitAfterError);
                            if (waitAfterRecovery == 0) processExited = true;
                            else processExitUnconfirmed = true;
                        }
                    }
                }
            }
            else if (attached)
            {
                Log("FATAL_DEBUG_DETACH_SKIPPED_PROCESS_STILL_RUNNING");
            }
            if (detached && process != IntPtr.Zero)
            {
                uint waitResult = WaitForSingleObject(process, 2000);
                uint exitCode;
                if (GetExitCodeProcess(process, out exitCode))
                    Log((waitResult == 0 ? "PROCESS_EXIT_AFTER_DETACH" : "PROCESS_STATE_AFTER_DETACH")
                        + " waitResult=0x" + waitResult.ToString("X8") + " exitCode=0x" + exitCode.ToString("X8"));
                else Log("PROCESS_STATE_AFTER_DETACH unreadable error=" + Marshal.GetLastWin32Error());
            }
            breakpointsRestored = processExited || AreBreakpointsRestored(cameraBreakpoint,
                projectionBreakpoint, returnBreakpoint, atmosphereBreakpoint, presentBreakpoint, missionBreakpoint);
            Log("FINAL_BREAKPOINT_STATE restored=" + breakpointsRestored + " processExited=" + processExited);
            if (eventBuffer != IntPtr.Zero) Marshal.FreeHGlobal(eventBuffer);
            if (process != IntPtr.Zero) CloseHandle(process);
        }

        if (!detached) throw new InvalidOperationException("Debug detach not confirmed; inspect the frame log before reuse.");
        if (fatal) throw new InvalidOperationException("The native frame capture could not recover safely; inspect the frame log.");
        if (captured)
        {
            bool pngCreated = false;
            bool jsonCreated = false;
            string outputPng = Path.ChangeExtension(outputPath, ".png");
            try
            {
                if (!breakpointsRestored)
                    throw new InvalidOperationException("The staged pixels cannot be published because breakpoint restoration was not confirmed.");
                if (completedSurface == null || completedFrame == null || completedReadback == null)
                    throw new InvalidOperationException("The capture was marked ready without a staged frame and readback record.");
                if (!IsMatchingPerspectiveFrame(completedFrame, completedReadback.ThreadId))
                    throw new InvalidOperationException("The staged pixels lost their verified camera or projection attribution before finalization.");
                string json = UsableFrameJson(expectedPath, world3dBase, ngiBase, completedFrame,
                    completedSurface, outputPng, completedReadback);
                SaveSurfacePng(completedSurface, outputPng);
                pngCreated = true;
                WriteTextCreateNew(outputPath, json);
                jsonCreated = true;
            }
            catch (Exception outputError)
            {
                if (jsonCreated) try { File.Delete(outputPath); } catch { }
                if (pngCreated) try { File.Delete(outputPng); } catch { }
                captureFailure = "Could not safely finalize the detached frame output: " + outputError.Message;
                stopAfterAttempt = true;
                captured = false;
                Log("FRAME_OUTPUT_FAILED " + outputError);
            }
            if (captured)
            {
                Log("FRAME_OUTPUT_COMPLETE generation=" + completedReadback.Generation + " png=" + outputPng
                    + " json=" + outputPath + " size=" + completedSurface.Width + "x" + completedSurface.Height
                    + " bitCount=" + completedSurface.BitCount + " rowsSha256=" + completedSurface.Sha256
                    + " afterContextRestore=true breakpointsRestored=true detached=true");
                Console.WriteLine("native frame captured: " + outputPath + " and " + outputPng);
                return 0;
            }
        }
        if (captureFailure != null) Console.Error.WriteLine(captureFailure);
        return stopAfterAttempt ? 4 : 3;
    }

    private static BreakpointInfo ArmBreakpoint(IntPtr process, string name, uint address, byte expectedByte)
    {
        byte[] current = new byte[1];
        if (!Read(process, address, current) || current[0] != expectedByte)
            throw new InvalidOperationException(name + " signature byte changed before arming at 0x" + address.ToString("X8"));
        BreakpointInfo result = new BreakpointInfo();
        result.Name = name;
        result.Address = address;
        result.OriginalByte = current[0];
        if (!WriteByte(process, address, 0xCC)) throw new InvalidOperationException("Could not arm " + name + " at 0x" + address.ToString("X8"));
        result.Armed = true;
        Log("BREAKPOINT_ARMED " + name + " address=0x" + address.ToString("X8") + " original=0x" + current[0].ToString("X2"));
        return result;
    }

    private static void EnsurePresentBreakpoint(IntPtr process, uint ngiBase, FrameSnapshot frame,
        uint threadId, byte expectedByte, ref BreakpointInfo presentBreakpoint)
    {
        if (!CanArmPresentForFrame(frame, threadId)) return;
        uint address = unchecked(ngiBase + PresentBoundaryRva);
        if (presentBreakpoint == null)
        {
            presentBreakpoint = ArmBreakpoint(process, "Ngi32.windowed-present", address, expectedByte);
            return;
        }
        if (presentBreakpoint.Address != address || presentBreakpoint.OriginalByte != expectedByte)
            throw new InvalidOperationException("The verified Ngi32 present boundary changed during this capture.");
        if (presentBreakpoint.Armed) return;
        byte[] current = new byte[1];
        if (!Read(process, address, current) || current[0] != presentBreakpoint.OriginalByte)
            throw new InvalidOperationException("Ngi32 present signature changed before re-arming.");
        if (!WriteByte(process, address, 0xCC))
            throw new InvalidOperationException("Could not re-arm Ngi32 present boundary.");
        presentBreakpoint.Armed = true;
        Log("BREAKPOINT_REARMED Ngi32.windowed-present address=0x" + address.ToString("X8"));
    }

    private static bool RestoreArmedBreakpointsWhileStopped(IntPtr process,
        BreakpointInfo cameraBreakpoint, BreakpointInfo projectionBreakpoint, BreakpointInfo returnBreakpoint,
        BreakpointInfo atmosphereBreakpoint, BreakpointInfo presentBreakpoint, BreakpointInfo missionBreakpoint)
    {
        return RestoreBreakpointWhileStopped(process, cameraBreakpoint)
            && RestoreBreakpointWhileStopped(process, projectionBreakpoint)
            && RestoreBreakpointWhileStopped(process, returnBreakpoint)
            && RestoreBreakpointWhileStopped(process, atmosphereBreakpoint)
            && RestoreBreakpointWhileStopped(process, presentBreakpoint)
            && RestoreBreakpointWhileStopped(process, missionBreakpoint);
    }

    private static bool AreBreakpointsRestored(BreakpointInfo cameraBreakpoint,
        BreakpointInfo projectionBreakpoint, BreakpointInfo returnBreakpoint,
        BreakpointInfo atmosphereBreakpoint, BreakpointInfo presentBreakpoint,
        BreakpointInfo missionBreakpoint)
    {
        return (cameraBreakpoint == null || !cameraBreakpoint.Armed)
            && (projectionBreakpoint == null || !projectionBreakpoint.Armed)
            && (returnBreakpoint == null || !returnBreakpoint.Armed)
            && (atmosphereBreakpoint == null || !atmosphereBreakpoint.Armed)
            && (presentBreakpoint == null || !presentBreakpoint.Armed)
            && (missionBreakpoint == null || !missionBreakpoint.Armed);
    }

    private static bool RestoreBreakpointWhileStopped(IntPtr process, BreakpointInfo breakpoint)
    {
        if (breakpoint == null || !breakpoint.Armed) return true;
        if (!WriteByte(process, breakpoint.Address, breakpoint.OriginalByte))
        {
            Log("FATAL breakpoint restore failed while target stopped: " + breakpoint.Name
                + " error=" + Marshal.GetLastWin32Error());
            return false;
        }
        breakpoint.Armed = false;
        Log("BREAKPOINT_RESTORED_WHILE_STOPPED " + breakpoint.Name + " address=0x" + breakpoint.Address.ToString("X8"));
        return true;
    }
}
