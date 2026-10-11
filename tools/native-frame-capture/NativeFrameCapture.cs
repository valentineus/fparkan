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
    private const uint FogConsumerRva = 0x2F230;
    private const uint FogConsumerVtableRva = 0x6527C;
    private const uint FogConsumerKnownCallerRva = 0x18A6A;
    private const uint FogGeometryVtableRva = 0x64E48;
    private const uint FogGeometryDistanceMethodRva = 0x246A0;
    private const uint PresentBoundaryRva = 0x6E1B;
    private const uint RelocatedGlobalRva = 0x79518C;
    private const uint ExternalCameraVtableRva = 0x665B4;
    private const uint MissionFactoryRva = 0xA1FE1;
    private const uint MissionVtableCallRva = 0xA1FF0;
    private const uint MissionImportThunkRva = 0xCD01C;
    private const int MissionProbeAttemptLimit = 8;
    private const int FrameCaptureTimeoutSeconds = 300;
    private const int NativeFogSampleLimit = 512;
    private static readonly byte[] FogConsumerFileSignature = new byte[]
        { 0xD9, 0x42, 0x14, 0xD8, 0x15, 0x7C, 0x41, 0x06, 0x10 };
    private static readonly byte[] FogConsumerLivePrefix = new byte[] { 0xD9, 0x42, 0x14 };

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
        public NativeFogReadback NativeFog;
        public readonly List<NativeFogReadback> NativeFogPairedSamples = new List<NativeFogReadback>();
        public readonly List<NativeFogReadback> NativeFogPendingSamples = new List<NativeFogReadback>();
        public uint NativeFogObservedCalls;
        public uint NativeFogOverflowCalls;
        public uint NativeFogPairedSampleCount;
        public uint NativeFogRejectedCalls;
        public uint NativeFogIgnoredOtherThreadCalls;
        public uint NativeFogIgnoredMatrixCalls;
        public bool NativeFogArmPending;
        public bool NativeFogSamplingComplete;
        public bool NativeFogSamplingStopped;
        public bool NativeFogSampleLimitExceeded;
        public string NativeFogConflictReason;
        public string NativeFogFailureReason;
        public string NativeFogStatus;
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

    private sealed class NativeFogReadback
    {
        public uint SampleThreadId;
        public uint CameraThreadIdAtSample;
        public uint CameraGenerationAtSample;
        public uint CameraPointerAtSample;
        public string CameraMatrixSha256AtSample;
        public uint SinkPointer;
        public uint SinkVtable;
        public uint Slot10Target;
        public byte RawFlags;
        public uint[] SinkRgbBits;
        public uint StartBits;
        public uint EndBits;
        public string TerrainModuleSha256;
        public bool TerrainModuleHashVerified;
        public bool BoundarySignatureVerified;
        public uint ReturnAddress;
        public uint DistanceArgumentBits;
        public uint BypassArgumentRawDword;
        public byte BypassArgumentEffectiveByte;
        public uint OutputColorArgument;
        public uint OutputAlphaArgument;
        public uint SourceColorPointer;
        public uint[] SourceColorBits;
        public uint GeometryEbpCandidate;
        public uint GeometryVtable;
        public uint GeometrySlot14Target;
        public ushort GeometryTypePlus0x6E;
        public bool GeometryContractVerified;
        public string ReturnAddressTerrainRva;
        public uint MatchedRenderGeneration;
        public uint MatchedRenderThreadId;
        public bool SameGenerationVerified;
        public bool CameraMatrixMatchesProjection;
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

    [DllImport("kernel32.dll")]
    private static extern uint GetCurrentThreadId();

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

    private static bool VerifyNativeFogConsumerBoundary(IntPtr process, string terrainPath,
        uint terrainBase, out byte firstByte, out string evidence)
    {
        firstByte = 0;
        byte[] fileBytes = ReadPeRva(terrainPath, FogConsumerRva, FogConsumerFileSignature.Length);
        bool fileMatch = ByteArraysEqual(fileBytes, FogConsumerFileSignature);
        byte[] livePrefix = new byte[FogConsumerLivePrefix.Length];
        uint address = unchecked(terrainBase + FogConsumerRva);
        bool liveReadable = ReadExact(process, address, livePrefix);
        bool liveMatch = liveReadable && ByteArraysEqual(livePrefix, FogConsumerLivePrefix);
        if (liveReadable) firstByte = livePrefix[0];
        evidence = "base=0x" + terrainBase.ToString("X8") + " RVA=0x2F230 fileBytes="
            + BitConverter.ToString(fileBytes) + " livePrefix="
            + (liveReadable ? BitConverter.ToString(livePrefix) : "unreadable")
            + " relocationAwareLivePrefixBytes=" + FogConsumerLivePrefix.Length;
        return fileMatch && liveMatch && firstByte == FogConsumerLivePrefix[0];
    }

    private static byte[] ReadPeRva(string path, uint rva, int length)
    {
        if (String.IsNullOrEmpty(path) || length <= 0)
            throw new ArgumentException("A PE path and positive read length are required.");
        byte[] image = File.ReadAllBytes(path);
        if (image.Length < 0x100 || BitConverter.ToUInt16(image, 0) != 0x5A4D)
            throw new InvalidDataException("Invalid PE image: " + path);
        int nt = BitConverter.ToInt32(image, 0x3C);
        if (nt < 0 || nt + 0x18 > image.Length || BitConverter.ToUInt32(image, nt) != 0x00004550)
            throw new InvalidDataException("Invalid PE header: " + path);
        ushort sectionCount = BitConverter.ToUInt16(image, nt + 6);
        ushort optionalSize = BitConverter.ToUInt16(image, nt + 20);
        int sectionOffset = checked(nt + 24 + optionalSize);
        for (int i = 0; i < sectionCount; i++)
        {
            int section = checked(sectionOffset + i * 40);
            if (section < 0 || section + 40 > image.Length) break;
            uint virtualSize = BitConverter.ToUInt32(image, section + 8);
            uint virtualAddress = BitConverter.ToUInt32(image, section + 12);
            uint rawSize = BitConverter.ToUInt32(image, section + 16);
            uint rawPointer = BitConverter.ToUInt32(image, section + 20);
            ulong span = Math.Max(virtualSize, rawSize);
            if (rva < virtualAddress || (ulong)rva + (uint)length > (ulong)virtualAddress + span)
                continue;
            ulong raw = (ulong)rawPointer + (rva - virtualAddress);
            if (raw + (uint)length > (ulong)image.Length)
                break;
            byte[] result = new byte[length];
            Buffer.BlockCopy(image, (int)raw, result, 0, length);
            return result;
        }
        throw new InvalidDataException("RVA 0x" + rva.ToString("X") + " is not in a raw PE section: " + path);
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

    private static void ResetNativeFogForGeneration(FrameSnapshot frame)
    {
        frame.NativeFog = null;
        frame.NativeFogPairedSamples.Clear();
        frame.NativeFogPendingSamples.Clear();
        frame.NativeFogObservedCalls = 0;
        frame.NativeFogOverflowCalls = 0;
        frame.NativeFogPairedSampleCount = 0;
        frame.NativeFogRejectedCalls = 0;
        frame.NativeFogIgnoredOtherThreadCalls = 0;
        frame.NativeFogIgnoredMatrixCalls = 0;
        frame.NativeFogArmPending = false;
        frame.NativeFogSamplingComplete = false;
        frame.NativeFogSamplingStopped = false;
        frame.NativeFogSampleLimitExceeded = false;
        frame.NativeFogConflictReason = null;
        frame.NativeFogFailureReason = null;
        frame.NativeFogStatus = "waiting-for-render-sample";
    }

    private static bool CanSampleNativeFogForFrame(FrameSnapshot frame, uint threadId)
    {
        if (frame == null || frame.Camera == null || !frame.Camera.LayoutVerified
            || frame.Camera.Camera == 0 || frame.CameraThreadId != threadId || frame.CameraGeneration == 0)
            return false;
        if (!frame.SelectedCameraRequested) return true;
        SelectedCameraState selected = frame.SelectedCamera;
        return selected != null && selected.Applied && selected.CameraPointer == frame.Camera.Camera
            && selected.ThreadId == threadId && selected.Generation == frame.CameraGeneration
            && selected.Input != null && selected.Input.MatrixBytes != null
            && ByteArraysEqual(frame.Camera.EntryMatrixBytes, selected.Input.MatrixBytes)
            && !selected.PixelAttributionInvalidated;
    }

    private static void EnsureFogConsumerBreakpoint(IntPtr process, uint terrainBase,
        byte expectedByte, FrameSnapshot frame, uint threadId, ref BreakpointInfo fogConsumerBreakpoint)
    {
        if (!CanSampleNativeFogForFrame(frame, threadId))
        {
            Log("NATIVE_FOG_ARM_SKIPPED without verified camera identity for thread=" + threadId);
            return;
        }
        uint address = unchecked(terrainBase + FogConsumerRva);
        if (fogConsumerBreakpoint == null)
        {
            fogConsumerBreakpoint = ArmBreakpoint(process, "Terrain.native-fog-consumer", address, expectedByte);
            Log("NATIVE_FOG_SAMPLING_ARMED generation=" + frame.CameraGeneration + " tid=" + threadId
                + " camera=0x" + frame.Camera.Camera.ToString("X8")
                + " matrixSha256=" + frame.Camera.EntryMatrixSha256);
            return;
        }
        if (fogConsumerBreakpoint.Address != address || fogConsumerBreakpoint.OriginalByte != expectedByte)
            throw new InvalidOperationException("The verified Terrain native fog consumer boundary changed during this capture.");
        if (fogConsumerBreakpoint.Armed) return;
        byte[] current = new byte[1];
        if (!Read(process, address, current) || current[0] != fogConsumerBreakpoint.OriginalByte)
            throw new InvalidOperationException("Terrain native fog consumer signature changed before re-arming.");
        if (!WriteByte(process, address, 0xCC))
            throw new InvalidOperationException("Could not re-arm Terrain native fog consumer boundary.");
        fogConsumerBreakpoint.Armed = true;
        Log("BREAKPOINT_REARMED Terrain.native-fog-consumer address=0x" + address.ToString("X8")
            + " generation=" + frame.CameraGeneration + " tid=" + threadId);
    }

    private static NativeFogReadback CaptureNativeFogConsumerSample(IntPtr process, byte[] context,
        uint threadId, uint terrainBase, string terrainModuleSha256, FrameSnapshot frame)
    {
        if (!CanSampleNativeFogForFrame(frame, threadId))
            throw new InvalidOperationException("The fog consumer hit did not match an active verified camera generation.");
        uint eip = BitConverter.ToUInt32(context, ContextEipOffset);
        uint esp = BitConverter.ToUInt32(context, ContextEspOffset);
        uint sink = BitConverter.ToUInt32(context, ContextEcxOffset);
        uint sourceColor = BitConverter.ToUInt32(context, ContextEdxOffset);
        uint geometryEbp = BitConverter.ToUInt32(context, ContextEbpOffset);
        if (eip != unchecked(terrainBase + FogConsumerRva + 1)
            || sink == 0 || sink > UInt32.MaxValue - 0x63
            || sourceColor == 0 || sourceColor > UInt32.MaxValue - 0x24
            || esp > UInt32.MaxValue - 0x14)
            throw new InvalidOperationException("Fog consumer registers or pointer ranges failed bounded checks.");

        uint sinkVtable = ReadU32Exact(process, sink);
        uint expectedVtable = unchecked(terrainBase + FogConsumerVtableRva);
        if (sinkVtable != expectedVtable)
            throw new InvalidOperationException("Fog consumer sink vptr did not match Terrain+0x6527C.");
        uint slot10Target = ReadU32Exact(process, unchecked(sinkVtable + 0x10));
        uint expectedTarget = unchecked(terrainBase + FogConsumerRva);
        if (slot10Target != expectedTarget)
            throw new InvalidOperationException("Fog consumer slot+0x10 did not match Terrain+0x2F230.");

        byte[] args = new byte[0x14];
        byte[] sourceColorBits = new byte[0x10];
        byte[] sinkFlags = new byte[1];
        byte[] sinkRgb = new byte[0x0C];
        if (!ReadExact(process, esp, args)
            || !ReadExact(process, unchecked(sourceColor + 0x14), sourceColorBits)
            || !ReadExact(process, unchecked(sink + 0x3C), sinkFlags)
            || !ReadExact(process, unchecked(sink + 0x48), sinkRgb))
            throw new InvalidOperationException("Fog consumer stack, color, flags, or RGB evidence was unreadable.");

        uint returnAddress = BitConverter.ToUInt32(args, 0);
        bool knownGeometryCaller = IsKnownNativeFogGeometryCaller(terrainBase, returnAddress);
        uint geometryVtable = 0;
        uint geometrySlot14Target = 0;
        ushort geometryTypeValue = 0;
        bool geometryContractVerified = false;
        if (knownGeometryCaller && geometryEbp != 0 && geometryEbp <= UInt32.MaxValue - 0x70)
        {
            byte[] geometryVtableBytes = new byte[4];
            if (ReadExact(process, geometryEbp, geometryVtableBytes))
            {
                uint candidateVtable = BitConverter.ToUInt32(geometryVtableBytes, 0);
                byte[] geometrySlot14Bytes = new byte[4];
                if (candidateVtable != 0 && candidateVtable <= UInt32.MaxValue - 0x14
                    && ReadExact(process, unchecked(candidateVtable + 0x14), geometrySlot14Bytes))
                {
                    uint candidateSlot14 = BitConverter.ToUInt32(geometrySlot14Bytes, 0);
                    geometrySlot14Target = candidateSlot14;
                    if (IsVerifiedNativeFogGeometryLayout(terrainBase, candidateVtable, candidateSlot14))
                    {
                        byte[] geometryType = new byte[2];
                        if (ReadExact(process, unchecked(geometryEbp + 0x6E), geometryType))
                        {
                            geometryVtable = candidateVtable;
                            geometryTypeValue = BitConverter.ToUInt16(geometryType, 0);
                            geometryContractVerified = true;
                        }
                    }
                }
            }
        }

        uint camera = frame.Camera.Camera;
        if (camera > UInt32.MaxValue - 0x60)
            throw new InvalidOperationException("Fog consumer camera pointer range overflowed x86 address space.");
        string cameraMatrixSha256 = HashBytes(ReadCameraMatrix(process, camera));

        NativeFogReadback result = new NativeFogReadback();
        result.SampleThreadId = threadId;
        result.CameraThreadIdAtSample = frame.CameraThreadId;
        result.CameraGenerationAtSample = frame.CameraGeneration;
        result.CameraPointerAtSample = camera;
        result.CameraMatrixSha256AtSample = cameraMatrixSha256;
        result.SinkPointer = sink;
        result.SinkVtable = sinkVtable;
        result.Slot10Target = slot10Target;
        result.RawFlags = sinkFlags[0];
        result.SinkRgbBits = new uint[3];
        for (int i = 0; i < result.SinkRgbBits.Length; i++)
            result.SinkRgbBits[i] = BitConverter.ToUInt32(sinkRgb, i * 4);
        result.StartBits = ReadU32Exact(process, unchecked(sink + 0x5C));
        result.EndBits = ReadU32Exact(process, unchecked(sink + 0x60));
        result.TerrainModuleSha256 = terrainModuleSha256;
        result.TerrainModuleHashVerified = String.Equals(terrainModuleSha256,
            ExpectedSha256[TerrainName], StringComparison.OrdinalIgnoreCase);
        result.BoundarySignatureVerified = true;
        result.ReturnAddress = returnAddress;
        result.DistanceArgumentBits = BitConverter.ToUInt32(args, 4);
        result.BypassArgumentRawDword = BitConverter.ToUInt32(args, 8);
        result.BypassArgumentEffectiveByte = NativeFogBypassEffectiveByte(result.BypassArgumentRawDword);
        result.OutputColorArgument = BitConverter.ToUInt32(args, 0x0C);
        result.OutputAlphaArgument = BitConverter.ToUInt32(args, 0x10);
        result.SourceColorPointer = sourceColor;
        result.SourceColorBits = new uint[4];
        for (int i = 0; i < result.SourceColorBits.Length; i++)
            result.SourceColorBits[i] = BitConverter.ToUInt32(sourceColorBits, i * 4);
        result.GeometryEbpCandidate = geometryEbp;
        result.GeometryVtable = geometryVtable;
        result.GeometrySlot14Target = geometryContractVerified ? geometrySlot14Target : 0;
        result.GeometryTypePlus0x6E = geometryTypeValue;
        result.GeometryContractVerified = geometryContractVerified;
        if (knownGeometryCaller)
            result.ReturnAddressTerrainRva = "Terrain.dll+0x18A6A";
        for (int i = 0; i < result.SinkRgbBits.Length; i++)
            if (!IsFinite(BitConverter.ToSingle(BitConverter.GetBytes(result.SinkRgbBits[i]), 0)))
                throw new InvalidOperationException("Native fog sink RGB contains a non-finite value.");
        if (!IsFinite(BitConverter.ToSingle(BitConverter.GetBytes(result.StartBits), 0))
            || !IsFinite(BitConverter.ToSingle(BitConverter.GetBytes(result.EndBits), 0)))
            throw new InvalidOperationException("Native fog range contains a non-finite value.");
        return result;
    }

    private static bool IsKnownNativeFogGeometryCaller(uint terrainBase, uint returnAddress)
    {
        return terrainBase != 0 && returnAddress == unchecked(terrainBase + FogConsumerKnownCallerRva);
    }

    private static bool IsVerifiedNativeFogGeometryLayout(uint terrainBase,
        uint geometryVtable, uint geometrySlot14Target)
    {
        return terrainBase != 0
            && geometryVtable == unchecked(terrainBase + FogGeometryVtableRva)
            && geometrySlot14Target == unchecked(terrainBase + FogGeometryDistanceMethodRva);
    }

    private static byte NativeFogBypassEffectiveByte(uint rawDword)
    {
        return unchecked((byte)(rawDword & 0xFF));
    }

    private static bool CanPairNativeFogSample(FrameSnapshot frame, NativeFogReadback sample, uint threadId)
    {
        return sample != null && sample.TerrainModuleHashVerified && sample.BoundarySignatureVerified
            && CanArmPresentForFrame(frame, threadId)
            && sample.SampleThreadId == threadId && sample.CameraThreadIdAtSample == threadId
            && sample.CameraGenerationAtSample == frame.CameraGeneration
            && sample.CameraPointerAtSample == frame.Camera.Camera
            && !String.IsNullOrEmpty(sample.CameraMatrixSha256AtSample)
            && String.Equals(sample.CameraMatrixSha256AtSample, frame.Camera.ProjectionMatrixSha256,
                StringComparison.OrdinalIgnoreCase);
    }

    private static void PairPendingNativeFogSamples(FrameSnapshot frame, uint threadId)
    {
        if (frame == null || frame.NativeFogPendingSamples.Count == 0
            || !CanArmPresentForFrame(frame, threadId)) return;
        for (int i = 0; i < frame.NativeFogPendingSamples.Count; i++)
        {
            NativeFogReadback sample = frame.NativeFogPendingSamples[i];
            if (CanPairNativeFogSample(frame, sample, threadId))
                AddPairedNativeFogSample(frame, sample, threadId);
            else
            {
                frame.NativeFogIgnoredMatrixCalls++;
                frame.NativeFogSamplingStopped = true;
                frame.NativeFogFailureReason = "A same-thread fog sample did not match the verified camera and projection matrix.";
            }
        }
        frame.NativeFogPendingSamples.Clear();
    }

    private static void RecordNativeFogSample(FrameSnapshot frame, NativeFogReadback sample)
    {
        if (frame == null || sample == null) throw new ArgumentNullException();
        if (frame.Projection != null && frame.ProjectionGeneration == frame.CameraGeneration
            && frame.ProjectionThreadId == sample.SampleThreadId)
        {
            if (CanPairNativeFogSample(frame, sample, sample.SampleThreadId))
                AddPairedNativeFogSample(frame, sample, sample.SampleThreadId);
            else
            {
                frame.NativeFogIgnoredMatrixCalls++;
                frame.NativeFogSamplingStopped = true;
                frame.NativeFogFailureReason = "A same-thread fog sample did not match the verified camera and projection matrix.";
            }
            return;
        }
        if (frame.NativeFogPendingSamples.Count >= NativeFogSampleLimit)
        {
            frame.NativeFogSampleLimitExceeded = true;
            frame.NativeFogSamplingStopped = true;
            frame.NativeFogFailureReason = "The bounded pre-projection fog sample limit was reached.";
            return;
        }
        frame.NativeFogPendingSamples.Add(sample);
    }

    private static bool TryCountNativeFogCall(FrameSnapshot frame)
    {
        if (frame == null) throw new ArgumentNullException("frame");
        if (frame.NativeFogObservedCalls >= NativeFogSampleLimit)
        {
            frame.NativeFogOverflowCalls++;
            frame.NativeFogSampleLimitExceeded = true;
            frame.NativeFogSamplingStopped = true;
            if (frame.NativeFogFailureReason == null)
                frame.NativeFogFailureReason = "The bounded 512-call native fog capture limit was reached.";
            return false;
        }
        frame.NativeFogObservedCalls++;
        return true;
    }

    private static void StopNativeFogAtCallLimit(FrameSnapshot frame)
    {
        if (frame == null || frame.NativeFogObservedCalls < NativeFogSampleLimit) return;
        frame.NativeFogSampleLimitExceeded = true;
        frame.NativeFogSamplingStopped = true;
        if (frame.NativeFogFailureReason == null)
            frame.NativeFogFailureReason = "The bounded 512-call native fog capture limit was reached.";
    }

    private static void AddPairedNativeFogSample(FrameSnapshot frame,
        NativeFogReadback sample, uint threadId)
    {
        sample.SameGenerationVerified = true;
        sample.CameraMatrixMatchesProjection = true;
        sample.MatchedRenderGeneration = frame.CameraGeneration;
        sample.MatchedRenderThreadId = threadId;
        if (frame.NativeFog == null) frame.NativeFog = sample;
        else
        {
            string conflict = NativeFogSettingsConflict(frame.NativeFog, sample);
            if (conflict != null && frame.NativeFogConflictReason == null)
            {
                frame.NativeFogConflictReason = conflict;
                frame.NativeFogSamplingStopped = true;
                frame.NativeFogStatus = "conflict";
            }
        }
        frame.NativeFogPairedSamples.Add(sample);
        frame.NativeFogPairedSampleCount++;
    }

    private static string NativeFogSettingsConflict(NativeFogReadback first, NativeFogReadback next)
    {
        if (first == null || next == null) return "a paired fog sample was missing";
        if (first.SinkPointer != next.SinkPointer || first.SinkVtable != next.SinkVtable
            || first.Slot10Target != next.Slot10Target)
            return "sink identity changed across paired fog calls";
        if (first.RawFlags != next.RawFlags)
            return "effective sink flags changed across paired fog calls";
        if (!UInt32ArraysEqual(first.SinkRgbBits, next.SinkRgbBits))
            return "effective sink RGB changed across paired fog calls";
        if (first.StartBits != next.StartBits || first.EndBits != next.EndBits)
            return "effective fog range changed across paired fog calls";
        return null;
    }

    private static bool UInt32ArraysEqual(uint[] left, uint[] right)
    {
        if (left == null || right == null || left.Length != right.Length) return false;
        for (int i = 0; i < left.Length; i++) if (left[i] != right[i]) return false;
        return true;
    }

    private static void FinalizeNativeFogAtPresent(FrameSnapshot frame, uint threadId)
    {
        if (frame == null) return;
        PairPendingNativeFogSamples(frame, threadId);
        frame.NativeFogSamplingComplete = true;
        if (frame.NativeFogSampleLimitExceeded)
        {
            frame.NativeFogStatus = "incomplete";
            if (frame.NativeFogFailureReason == null)
                frame.NativeFogFailureReason = "The bounded fog sample limit was reached before Present.";
        }
        else if (frame.NativeFogConflictReason != null) frame.NativeFogStatus = "conflict";
        else if (frame.NativeFogFailureReason != null) frame.NativeFogStatus = "incomplete";
        else if (!CanArmPresentForFrame(frame, threadId) || frame.NativeFogPairedSampleCount == 0)
        {
            frame.NativeFogStatus = "unpaired";
            if (frame.NativeFogFailureReason == null)
                frame.NativeFogFailureReason = frame.NativeFogObservedCalls == 0
                    ? "No Terrain+0x2F230 call was observed between this camera entry and Present."
                    : "No fog consumer sample matched the camera pointer, matrix hash, thread, generation, and verified projection.";
        }
        else if (frame.NativeFogSamplingStopped)
        {
            frame.NativeFogStatus = "incomplete";
            if (frame.NativeFogFailureReason == null)
                frame.NativeFogFailureReason = "Fog sampling stopped before the verified Present boundary.";
        }
        else frame.NativeFogStatus = "paired";
    }

    private static bool IsVerifiedNativeFogForFrame(FrameSnapshot frame, uint threadId)
    {
        NativeFogReadback fog = frame == null ? null : frame.NativeFog;
        return frame != null && frame.NativeFogSamplingComplete
            && String.Equals(frame.NativeFogStatus, "paired", StringComparison.Ordinal)
            && frame.NativeFogConflictReason == null && frame.NativeFogFailureReason == null
            && !frame.NativeFogSamplingStopped && !frame.NativeFogSampleLimitExceeded
            && frame.NativeFogPairedSampleCount > 0 && fog != null
            && (uint)frame.NativeFogPairedSamples.Count == frame.NativeFogPairedSampleCount
            && fog.SameGenerationVerified && fog.CameraMatrixMatchesProjection
            && String.Equals(fog.TerrainModuleSha256, ExpectedSha256[TerrainName], StringComparison.OrdinalIgnoreCase)
            && CanPairNativeFogSample(frame, fog, threadId);
    }

    private static string JsonFloatFromBits(uint bits)
    {
        float value = BitConverter.ToSingle(BitConverter.GetBytes(bits), 0);
        return IsFinite(value) ? value.ToString("R", CultureInfo.InvariantCulture) : "null";
    }

    private static string JsonHexU32(uint value)
    {
        return JsonString("0x" + value.ToString("X8"));
    }

    private static void AppendNativeFogJson(StringBuilder json, FrameSnapshot frame, uint threadId)
    {
        bool verified = IsVerifiedNativeFogForFrame(frame, threadId);
        string status = frame == null || String.IsNullOrEmpty(frame.NativeFogStatus)
            ? "not-sampled" : frame.NativeFogStatus;
        string reason = frame == null ? null
            : (frame.NativeFogConflictReason ?? frame.NativeFogFailureReason);
        json.AppendLine("  \"native_fog_status\": " + JsonString(status) + ",");
        json.AppendLine("  \"native_fog_status_reason\": " + (String.IsNullOrEmpty(reason) ? "null" : JsonString(reason)) + ",");
        ulong observedCalls = frame == null ? 0UL : (ulong)frame.NativeFogObservedCalls + frame.NativeFogOverflowCalls;
        json.AppendLine("  \"native_fog_observed_calls\": " + observedCalls.ToString(CultureInfo.InvariantCulture) + ",");
        json.AppendLine("  \"native_fog_processed_calls\": " + (frame == null ? "0" : frame.NativeFogObservedCalls.ToString(CultureInfo.InvariantCulture)) + ",");
        json.AppendLine("  \"native_fog_overflow_calls\": " + (frame == null ? "0" : frame.NativeFogOverflowCalls.ToString(CultureInfo.InvariantCulture)) + ",");
        json.AppendLine("  \"native_fog_paired_sample_count\": " + (frame == null ? "0" : frame.NativeFogPairedSampleCount.ToString(CultureInfo.InvariantCulture)) + ",");
        json.AppendLine("  \"native_fog_rejected_calls\": " + (frame == null ? "0" : frame.NativeFogRejectedCalls.ToString(CultureInfo.InvariantCulture)) + ",");
        json.AppendLine("  \"native_fog_ignored_other_thread_calls\": " + (frame == null ? "0" : frame.NativeFogIgnoredOtherThreadCalls.ToString(CultureInfo.InvariantCulture)) + ",");
        json.AppendLine("  \"native_fog_ignored_matrix_calls\": " + (frame == null ? "0" : frame.NativeFogIgnoredMatrixCalls.ToString(CultureInfo.InvariantCulture)) + ",");
        json.AppendLine("  \"native_fog_sample_limit\": " + NativeFogSampleLimit.ToString(CultureInfo.InvariantCulture) + ",");
        if (!verified)
        {
            json.AppendLine("  \"native_fog\": null,");
            return;
        }

        NativeFogReadback fog = frame.NativeFog;
        json.AppendLine("  \"native_fog\": {");
        json.AppendLine("    \"sample_thread_id\": " + fog.SampleThreadId.ToString(CultureInfo.InvariantCulture) + ",");
        json.AppendLine("    \"camera_generation_at_sample\": " + fog.CameraGenerationAtSample.ToString(CultureInfo.InvariantCulture) + ",");
        json.AppendLine("    \"matched_render_generation\": " + fog.MatchedRenderGeneration.ToString(CultureInfo.InvariantCulture) + ",");
        json.AppendLine("    \"matched_render_thread_id\": " + fog.MatchedRenderThreadId.ToString(CultureInfo.InvariantCulture) + ",");
        json.AppendLine("    \"same_generation_verified\": " + (fog.SameGenerationVerified ? "true" : "false") + ",");
        json.AppendLine("    \"camera_matrix_matches_projection\": " + (fog.CameraMatrixMatchesProjection ? "true" : "false") + ",");
        json.AppendLine("    \"camera_argument\": " + JsonHexU32(fog.CameraPointerAtSample) + ",");
        json.AppendLine("    \"camera_matrix_sha256_at_sample\": " + JsonString(fog.CameraMatrixSha256AtSample) + ",");
        json.AppendLine("    \"terrain_module_sha256\": " + JsonString(fog.TerrainModuleSha256) + ",");
        json.AppendLine("    \"terrain_sha256_verified\": " + (fog.TerrainModuleHashVerified ? "true" : "false") + ",");
        json.AppendLine("    \"boundary_signature_verified\": " + (fog.BoundarySignatureVerified ? "true" : "false") + ",");
        json.AppendLine("    \"sink_pointer\": " + JsonHexU32(fog.SinkPointer) + ",");
        json.AppendLine("    \"sink_vtable\": " + JsonHexU32(fog.SinkVtable) + ",");
        json.AppendLine("    \"slot_10_target\": " + JsonHexU32(fog.Slot10Target) + ",");
        json.AppendLine("    \"effective\": {");
        json.AppendLine("      \"flags_byte\": " + JsonString("0x" + fog.RawFlags.ToString("X2")) + ",");
        json.Append("      \"sink_rgb_bits\": [");
        for (int i = 0; i < fog.SinkRgbBits.Length; i++)
        {
            if (i != 0) json.Append(", ");
            json.Append(JsonHexU32(fog.SinkRgbBits[i]));
        }
        json.AppendLine("],");
        json.Append("      \"sink_rgb\": [");
        for (int i = 0; i < fog.SinkRgbBits.Length; i++)
        {
            if (i != 0) json.Append(", ");
            json.Append(JsonFloatFromBits(fog.SinkRgbBits[i]));
        }
        json.AppendLine("],");
        json.AppendLine("      \"start_bits\": " + JsonHexU32(fog.StartBits) + ",");
        json.AppendLine("      \"start\": " + JsonFloatFromBits(fog.StartBits) + ",");
        json.AppendLine("      \"end_bits\": " + JsonHexU32(fog.EndBits) + ",");
        json.AppendLine("      \"end\": " + JsonFloatFromBits(fog.EndBits));
        json.AppendLine("    },");
        json.AppendLine("    \"draw_evidence\": [");
        for (int i = 0; i < frame.NativeFogPairedSamples.Count; i++)
        {
            NativeFogReadback draw = frame.NativeFogPairedSamples[i];
            if (i != 0) json.AppendLine(",");
            json.AppendLine("      {");
            json.AppendLine("        \"return_address\": " + JsonHexU32(draw.ReturnAddress) + ",");
            json.AppendLine("        \"return_address_terrain_rva\": " + (draw.ReturnAddressTerrainRva == null ? "null" : JsonString(draw.ReturnAddressTerrainRva)) + ",");
            json.AppendLine("        \"distance_argument_bits\": " + JsonHexU32(draw.DistanceArgumentBits) + ",");
            json.AppendLine("        \"distance_argument\": " + JsonFloatFromBits(draw.DistanceArgumentBits) + ",");
            json.AppendLine("        \"bypass_argument_raw_dword\": " + JsonHexU32(draw.BypassArgumentRawDword) + ",");
            json.AppendLine("        \"bypass_argument_effective_byte\": " + JsonString("0x" + draw.BypassArgumentEffectiveByte.ToString("X2")) + ",");
            json.AppendLine("        \"bypass_effective\": " + (draw.BypassArgumentEffectiveByte != 0 ? "true" : "false") + ",");
            json.AppendLine("        \"output_color_argument\": " + JsonHexU32(draw.OutputColorArgument) + ",");
            json.AppendLine("        \"output_alpha_argument\": " + JsonHexU32(draw.OutputAlphaArgument) + ",");
            json.AppendLine("        \"source_color_pointer\": " + JsonHexU32(draw.SourceColorPointer) + ",");
            json.Append("        \"source_color_bits\": [");
            for (int channel = 0; channel < draw.SourceColorBits.Length; channel++)
            {
                if (channel != 0) json.Append(", ");
                json.Append(JsonHexU32(draw.SourceColorBits[channel]));
            }
            json.AppendLine("],");
            json.Append("        \"source_color\": [");
            for (int channel = 0; channel < draw.SourceColorBits.Length; channel++)
            {
                if (channel != 0) json.Append(", ");
                json.Append(JsonFloatFromBits(draw.SourceColorBits[channel]));
            }
            json.AppendLine("],");
            bool geometryVerified = draw.GeometryContractVerified
                && String.Equals(draw.ReturnAddressTerrainRva, "Terrain.dll+0x18A6A", StringComparison.Ordinal)
                && draw.GeometryVtable != 0 && draw.GeometrySlot14Target != 0;
            if (!geometryVerified)
                json.AppendLine("        \"distance_geometry\": null");
            else
            {
                json.AppendLine("        \"distance_geometry\": {");
                json.AppendLine("          \"object\": " + JsonHexU32(draw.GeometryEbpCandidate) + ",");
                json.AppendLine("          \"vtable\": " + JsonHexU32(draw.GeometryVtable) + ",");
                json.AppendLine("          \"slot_14_target\": " + JsonHexU32(draw.GeometrySlot14Target) + ",");
                json.AppendLine("          \"type_plus_0x6e\": " + draw.GeometryTypePlus0x6E.ToString(CultureInfo.InvariantCulture) + ",");
                json.AppendLine("          \"contract_verified\": true");
                json.AppendLine("        }");
            }
            json.Append("      }");
        }
        json.AppendLine();
        json.AppendLine("    ]");
        json.AppendLine("  },");
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
        AppendNativeFogJson(json, frame, frame.ProjectionThreadId);
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
        SelfCheckNativeFogPairing();
        SelfCheckRenderInvocationGates();
        SelfCheckDebugEventHandleOwnership();
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
        Console.WriteLine("self-check: x86 CONTEXT layout, relocation RVA, legacy camera JSON compatibility, native fog pairing/settings consistency, geometry caller/layout gates, low-byte bypass, nullable fog JSON, 512-call bound, camera/projection gates, wrapping atmosphere phase arithmetic/clock pairing, and recovery states OK");
        return SelfCheckReadback();
    }

    private static void SelfCheckNativeFogPairing()
    {
        if (FogConsumerRva != 0x2F230 || FogConsumerVtableRva != 0x6527C
            || FogConsumerKnownCallerRva != 0x18A6A || NativeFogSampleLimit != 512
            || FogGeometryVtableRva != 0x64E48 || FogGeometryDistanceMethodRva != 0x246A0
            || !ByteArraysEqual(FogConsumerFileSignature,
                new byte[] { 0xD9, 0x42, 0x14, 0xD8, 0x15, 0x7C, 0x41, 0x06, 0x10 })
            || !ByteArraysEqual(FogConsumerLivePrefix, new byte[] { 0xD9, 0x42, 0x14 }))
            throw new InvalidOperationException("Self-check failed: pinned Terrain native-fog boundary signature.");
        bool exactGeometryCallerAccepted = IsKnownNativeFogGeometryCaller(0x10000000, 0x10018A6A);
        bool otherGeometryCallerRejected = !IsKnownNativeFogGeometryCaller(0x10000000, 0x10018A6B);
        bool exactGeometryLayoutAccepted = IsVerifiedNativeFogGeometryLayout(0x10000000,
            0x10064E48, 0x100246A0);
        bool changedGeometryLayoutRejected = !IsVerifiedNativeFogGeometryLayout(0x10000000,
            0x10064E48, 0x100246A4);
        bool nativeBypassUsesLowByte = NativeFogBypassEffectiveByte(0x00000101) == 1
            && NativeFogBypassEffectiveByte(0x00000100) == 0;

        FrameSnapshot frame = new FrameSnapshot
        {
            CameraGeneration = 7,
            CameraThreadId = 11,
            Camera = new CameraReadback
            {
                Camera = 0x200000,
                LayoutVerified = true,
                MatrixFinite = true,
                CurrentAtProjectionVerified = true,
                EntryMatrixSha256 = "MATRIX-A",
                ProjectionMatrixSha256 = "MATRIX-A"
            },
            ProjectionGeneration = 7,
            ProjectionThreadId = 11,
            Projection = new ProjectionReadback { Verified = true, Mode = 1 }
        };
        NativeFogReadback first = new NativeFogReadback
        {
            SampleThreadId = 11,
            CameraThreadIdAtSample = 11,
            CameraGenerationAtSample = 7,
            CameraPointerAtSample = 0x200000,
            CameraMatrixSha256AtSample = "MATRIX-A",
            SinkPointer = 0x300000,
            SinkVtable = 0x1006527C,
            Slot10Target = 0x1002F230,
            RawFlags = 0x0C,
            SinkRgbBits = new uint[] { 0x3E70F0F2, 0x3E68E8EA, 0x3EA2A2A3 },
            StartBits = 0,
            EndBits = 0x442F8000,
            TerrainModuleSha256 = ExpectedSha256[TerrainName],
            TerrainModuleHashVerified = true,
            BoundarySignatureVerified = true
        };
        bool matched = CanPairNativeFogSample(frame, first, 11);
        bool wrongThreadRejected = !CanPairNativeFogSample(frame,
            new NativeFogReadback
            {
                SampleThreadId = 12, CameraThreadIdAtSample = 12, CameraGenerationAtSample = 7,
                CameraPointerAtSample = 0x200000, CameraMatrixSha256AtSample = "MATRIX-A",
                TerrainModuleHashVerified = true, BoundarySignatureVerified = true
            }, 11);
        bool wrongGenerationRejected = !CanPairNativeFogSample(frame,
            new NativeFogReadback
            {
                SampleThreadId = 11, CameraThreadIdAtSample = 11, CameraGenerationAtSample = 8,
                CameraPointerAtSample = 0x200000, CameraMatrixSha256AtSample = "MATRIX-A",
                TerrainModuleHashVerified = true, BoundarySignatureVerified = true
            }, 11);
        bool wrongCameraRejected = !CanPairNativeFogSample(frame,
            new NativeFogReadback
            {
                SampleThreadId = 11, CameraThreadIdAtSample = 11, CameraGenerationAtSample = 7,
                CameraPointerAtSample = 0x200004, CameraMatrixSha256AtSample = "MATRIX-A",
                TerrainModuleHashVerified = true, BoundarySignatureVerified = true
            }, 11);
        bool wrongMatrixRejected = !CanPairNativeFogSample(frame,
            new NativeFogReadback
            {
                SampleThreadId = 11, CameraThreadIdAtSample = 11, CameraGenerationAtSample = 7,
                CameraPointerAtSample = 0x200000, CameraMatrixSha256AtSample = "MATRIX-B",
                TerrainModuleHashVerified = true, BoundarySignatureVerified = true
            }, 11);
        FrameSnapshot badProjection = new FrameSnapshot
        {
            CameraGeneration = 7, CameraThreadId = 11, Camera = frame.Camera,
            ProjectionGeneration = 7, ProjectionThreadId = 11,
            Projection = new ProjectionReadback { Verified = false, Mode = 1 }
        };
        bool invalidProjectionRejected = !CanPairNativeFogSample(badProjection, first, 11);

        NativeFogReadback perDrawVariation = new NativeFogReadback
        {
            SinkPointer = first.SinkPointer, SinkVtable = first.SinkVtable, Slot10Target = first.Slot10Target,
            RawFlags = first.RawFlags, SinkRgbBits = (uint[])first.SinkRgbBits.Clone(),
            StartBits = first.StartBits, EndBits = first.EndBits,
            ReturnAddress = first.ReturnAddress + 4, DistanceArgumentBits = first.DistanceArgumentBits + 1,
            SourceColorPointer = 0x400000, SourceColorBits = new uint[] { 1, 2, 3, 4 },
            GeometryEbpCandidate = 0x500000, GeometryVtable = 0x10064E48,
            GeometryTypePlus0x6E = 1, GeometryContractVerified = true
        };
        bool perDrawEvidenceIgnoredForConsistency = NativeFogSettingsConflict(first, perDrawVariation) == null;
        NativeFogReadback changedSettings = new NativeFogReadback
        {
            SinkPointer = first.SinkPointer, SinkVtable = first.SinkVtable, Slot10Target = first.Slot10Target,
            RawFlags = (byte)(first.RawFlags ^ 1), SinkRgbBits = (uint[])first.SinkRgbBits.Clone(),
            StartBits = first.StartBits, EndBits = first.EndBits
        };
        bool settingsConflictDetected = NativeFogSettingsConflict(first, changedSettings) != null;
        FrameSnapshot accumulated = new FrameSnapshot { CameraGeneration = 7, CameraThreadId = 11 };
        NativeFogReadback pairedFirst = new NativeFogReadback
        {
            SinkPointer = first.SinkPointer, SinkVtable = first.SinkVtable, Slot10Target = first.Slot10Target,
            RawFlags = first.RawFlags, SinkRgbBits = (uint[])first.SinkRgbBits.Clone(),
            StartBits = first.StartBits, EndBits = first.EndBits
        };
        NativeFogReadback pairedSecond = new NativeFogReadback
        {
            SinkPointer = perDrawVariation.SinkPointer, SinkVtable = perDrawVariation.SinkVtable,
            Slot10Target = perDrawVariation.Slot10Target, RawFlags = perDrawVariation.RawFlags,
            SinkRgbBits = (uint[])perDrawVariation.SinkRgbBits.Clone(),
            StartBits = perDrawVariation.StartBits, EndBits = perDrawVariation.EndBits
        };
        AddPairedNativeFogSample(accumulated, pairedFirst, 11);
        AddPairedNativeFogSample(accumulated, pairedSecond, 11);
        bool repeatedDrawsConsistent = accumulated.NativeFogPairedSampleCount == 2
            && accumulated.NativeFogConflictReason == null;
        AddPairedNativeFogSample(accumulated, changedSettings, 11);
        bool repeatedDrawConflictRejected = accumulated.NativeFogPairedSampleCount == 3
            && accumulated.NativeFogConflictReason != null && accumulated.NativeFogSamplingStopped;

        if (!exactGeometryCallerAccepted || !otherGeometryCallerRejected
            || !exactGeometryLayoutAccepted || !changedGeometryLayoutRejected || !nativeBypassUsesLowByte
            || !matched || !wrongThreadRejected || !wrongGenerationRejected || !wrongCameraRejected
            || !wrongMatrixRejected || !invalidProjectionRejected || !perDrawEvidenceIgnoredForConsistency
            || !settingsConflictDetected || !repeatedDrawsConsistent || !repeatedDrawConflictRejected)
            throw new InvalidOperationException("Self-check failed: native fog sample pairing or consistency gate.");
        SelfCheckNativeFogJsonAndLimit(frame);
    }

    private static Dictionary<string, object> ParseNativeFogJsonFragmentForSelfCheck(FrameSnapshot frame)
    {
        StringBuilder fragment = new StringBuilder();
        AppendNativeFogJson(fragment, frame, frame.ProjectionThreadId);
        string content = fragment.ToString().Trim();
        if (content.Length == 0 || content[content.Length - 1] != ',')
            throw new InvalidOperationException("Self-check failed: native fog JSON fragment did not end with a property separator.");
        string json = "{" + content.Substring(0, content.Length - 1) + "}";
        object parsed = new System.Web.Script.Serialization.JavaScriptSerializer().DeserializeObject(json);
        Dictionary<string, object> root = parsed as Dictionary<string, object>;
        if (root == null) throw new InvalidOperationException("Self-check failed: native fog JSON fragment was not an object.");
        return root;
    }

    private static void SelfCheckNativeFogJsonAndLimit(FrameSnapshot baseFrame)
    {
        NativeFogReadback sample = new NativeFogReadback
        {
            SampleThreadId = 11,
            CameraThreadIdAtSample = 11,
            CameraGenerationAtSample = 7,
            CameraPointerAtSample = 0x200000,
            CameraMatrixSha256AtSample = "MATRIX-A",
            SinkPointer = 0x300000,
            SinkVtable = 0x1006527C,
            Slot10Target = 0x1002F230,
            RawFlags = 0x0C,
            SinkRgbBits = new uint[] { 0x3E70F0F2, 0x3E68E8EA, 0x3EA2A2A3 },
            StartBits = 0,
            EndBits = 0x442F8000,
            TerrainModuleSha256 = ExpectedSha256[TerrainName],
            TerrainModuleHashVerified = true,
            BoundarySignatureVerified = true,
            ReturnAddress = 0x10018A6A,
            ReturnAddressTerrainRva = "Terrain.dll+0x18A6A",
            DistanceArgumentBits = 0x42CAC000,
            BypassArgumentRawDword = 0x00000100,
            BypassArgumentEffectiveByte = 0,
            OutputColorArgument = 0x400000,
            OutputAlphaArgument = 0x400004,
            SourceColorPointer = 0x410000,
            SourceColorBits = new uint[] { 0x3F800000, 0x3F800000, 0x3F000000, 0x3F800000 },
            GeometryEbpCandidate = 0x500000,
            GeometryVtable = 0x10064E48,
            GeometrySlot14Target = 0x100246A0,
            GeometryTypePlus0x6E = 1,
            GeometryContractVerified = true,
            MatchedRenderGeneration = 7,
            MatchedRenderThreadId = 11,
            SameGenerationVerified = true,
            CameraMatrixMatchesProjection = true
        };
        FrameSnapshot paired = new FrameSnapshot
        {
            CameraGeneration = baseFrame.CameraGeneration,
            CameraThreadId = baseFrame.CameraThreadId,
            Camera = baseFrame.Camera,
            ProjectionGeneration = baseFrame.ProjectionGeneration,
            ProjectionThreadId = baseFrame.ProjectionThreadId,
            Projection = baseFrame.Projection,
            NativeFog = sample,
            NativeFogSamplingComplete = true,
            NativeFogStatus = "paired",
            NativeFogPairedSampleCount = 1
        };
        paired.NativeFogPairedSamples.Add(sample);
        Dictionary<string, object> pairedJson = ParseNativeFogJsonFragmentForSelfCheck(paired);
        object fogValue;
        Dictionary<string, object> fogJson = pairedJson.TryGetValue("native_fog", out fogValue)
            ? fogValue as Dictionary<string, object> : null;
        object drawValue;
        object[] draws = fogJson != null && fogJson.TryGetValue("draw_evidence", out drawValue)
            ? drawValue as object[] : null;
        Dictionary<string, object> drawJson = draws != null && draws.Length == 1
            ? draws[0] as Dictionary<string, object> : null;
        object geometryValue;
        Dictionary<string, object> geometryJson = drawJson != null
            && drawJson.TryGetValue("distance_geometry", out geometryValue)
            ? geometryValue as Dictionary<string, object> : null;
        bool exactCallerGeometrySerialized = geometryJson != null
            && geometryJson.ContainsKey("slot_14_target") && geometryJson.ContainsKey("type_plus_0x6e");
        sample.ReturnAddress = 0x10012345;
        sample.ReturnAddressTerrainRva = null;
        sample.GeometryContractVerified = false;
        sample.GeometryVtable = 0;
        sample.GeometrySlot14Target = 0;
        Dictionary<string, object> unknownCallerJson = ParseNativeFogJsonFragmentForSelfCheck(paired);
        Dictionary<string, object> unknownFogJson = unknownCallerJson["native_fog"] as Dictionary<string, object>;
        object unknownDrawValue;
        object[] unknownDraws = unknownFogJson != null && unknownFogJson.TryGetValue("draw_evidence", out unknownDrawValue)
            ? unknownDrawValue as object[] : null;
        Dictionary<string, object> unknownDrawJson = unknownDraws != null && unknownDraws.Length == 1
            ? unknownDraws[0] as Dictionary<string, object> : null;
        object unknownGeometryValue;
        bool unknownCallerHasNoInterpretedGeometry = unknownDrawJson != null
            && unknownDrawJson.TryGetValue("distance_geometry", out unknownGeometryValue) && unknownGeometryValue == null;

        FrameSnapshot conflict = new FrameSnapshot
        {
            CameraGeneration = paired.CameraGeneration,
            CameraThreadId = paired.CameraThreadId,
            Camera = paired.Camera,
            ProjectionGeneration = paired.ProjectionGeneration,
            ProjectionThreadId = paired.ProjectionThreadId,
            Projection = paired.Projection,
            NativeFog = sample,
            NativeFogSamplingComplete = true,
            NativeFogSamplingStopped = true,
            NativeFogStatus = "conflict",
            NativeFogConflictReason = "test settings conflict",
            NativeFogPairedSampleCount = 2
        };
        object conflictFog;
        bool conflictIsNull = ParseNativeFogJsonFragmentForSelfCheck(conflict)
            .TryGetValue("native_fog", out conflictFog) && conflictFog == null;
        FrameSnapshot unpaired = new FrameSnapshot
        {
            NativeFogSamplingComplete = true,
            NativeFogStatus = "unpaired"
        };
        object unpairedFog;
        bool unpairedIsNull = ParseNativeFogJsonFragmentForSelfCheck(unpaired)
            .TryGetValue("native_fog", out unpairedFog) && unpairedFog == null;

        FrameSnapshot bounded = new FrameSnapshot();
        NativeFogReadback pending = new NativeFogReadback();
        for (int i = 0; i < NativeFogSampleLimit; i++) RecordNativeFogSample(bounded, pending);
        RecordNativeFogSample(bounded, pending);
        bool limitIsBounded = bounded.NativeFogPendingSamples.Count == NativeFogSampleLimit
            && bounded.NativeFogSampleLimitExceeded && bounded.NativeFogSamplingStopped;
        FrameSnapshot boundedCalls = new FrameSnapshot();
        bool allInRangeCallsAccepted = true;
        for (int i = 0; i < NativeFogSampleLimit; i++)
            if (!TryCountNativeFogCall(boundedCalls)) allInRangeCallsAccepted = false;
        StopNativeFogAtCallLimit(boundedCalls);
        bool overflowRejected = !TryCountNativeFogCall(boundedCalls);
        bool eventCallLimitIsBounded = allInRangeCallsAccepted
            && boundedCalls.NativeFogObservedCalls == NativeFogSampleLimit
            && boundedCalls.NativeFogOverflowCalls == 1
            && boundedCalls.NativeFogSampleLimitExceeded && boundedCalls.NativeFogSamplingStopped
            && overflowRejected;
        if (!exactCallerGeometrySerialized || !unknownCallerHasNoInterpretedGeometry
            || !conflictIsNull || !unpairedIsNull
            || !limitIsBounded || !eventCallLimitIsBounded)
            throw new InvalidOperationException("Self-check failed: nullable fog JSON, unknown caller gate, or bounded sample list.");
    }

    private static void SelfCheckDebugEventHandleOwnership()
    {
        BreakpointInfo owned = new BreakpointInfo { Address = 0x12345678, Armed = true };
        uint ownedStatus = FrameDebugEventContinueStatus(ExceptionDebugEvent, 0x80000003,
            owned.Address, new BreakpointInfo[] { owned });
        uint unrelatedStatus = FrameDebugEventContinueStatus(ExceptionDebugEvent, 0x80000003,
            0x12345679, new BreakpointInfo[] { owned });
        if (!ShouldExplicitlyCloseDebugEventFileHandle(CreateProcessDebugEvent)
            || ShouldExplicitlyCloseDebugEventFileHandle(CreateThreadDebugEvent)
            || !ShouldExplicitlyCloseDebugEventFileHandle(LoadDllDebugEvent)
            || ShouldExplicitlyCloseDebugEventFileHandle(ExitThreadDebugEvent)
            || ShouldExplicitlyCloseDebugEventFileHandle(ExitProcessDebugEvent)
            || ownedStatus != DbgContinue || unrelatedStatus != DbgExceptionNotHandled
            || FrameDebugEventContinueStatus(CreateThreadDebugEvent, 0, 0, null) != DbgContinue)
            throw new InvalidOperationException("Debug-event handle ownership self-check failed.");
    }

    // Win32 debug-event process/thread handles are system-owned and closed on
    // the matching EXIT event's ContinueDebugEvent. Only image-file handles
    // are explicitly closed by the debugger. See:
    // https://learn.microsoft.com/en-us/windows/win32/debug/writing-the-debugger-s-main-loop
    // https://learn.microsoft.com/en-us/windows/win32/api/debugapi/nf-debugapi-continuedebugevent
    private static bool ShouldExplicitlyCloseDebugEventFileHandle(uint eventCode)
    {
        return eventCode == CreateProcessDebugEvent || eventCode == LoadDllDebugEvent;
    }

    private static void CloseExplicitDebugEventFileHandle(IntPtr eventBuffer, uint eventCode)
    {
        if (!ShouldExplicitlyCloseDebugEventFileHandle(eventCode)) return;
        IntPtr handle = Marshal.ReadIntPtr(eventBuffer, 12);
        if (handle != IntPtr.Zero && !CloseHandle(handle))
            Log("DEBUG_EVENT_FILE_HANDLE_CLOSE_FAILED code=" + eventCode + " offset=12"
                + " error=" + Marshal.GetLastWin32Error());
    }

    private static uint FrameDebugEventContinueStatus(uint eventCode, uint exceptionCode,
        uint exceptionAddress, BreakpointInfo[] breakpoints)
    {
        if (eventCode != ExceptionDebugEvent) return DbgContinue;
        if (exceptionCode == 0x80000003 && breakpoints != null)
        {
            for (int i = 0; i < breakpoints.Length; i++)
                if (breakpoints[i] != null && breakpoints[i].Armed
                    && breakpoints[i].Address == exceptionAddress)
                    return DbgContinue;
        }
        return DbgExceptionNotHandled;
    }

    private static bool VerifyFrameTerminationIdentity(Process identity, uint processId,
        string expectedPath, long expectedStartTicks, out string evidence)
    {
        evidence = "identity_unavailable";
        try
        {
            if (identity == null) return false;
            identity.Refresh();
            if (identity.HasExited) { evidence = "already_exited"; return false; }
            string actualPath = Path.GetFullPath(identity.MainModule.FileName);
            long actualTicks = identity.StartTime.ToUniversalTime().Ticks;
            if ((uint)identity.Id != processId || identity.ProcessName != "iron_3d"
                || actualTicks != expectedStartTicks
                || !String.Equals(actualPath, expectedPath, StringComparison.OrdinalIgnoreCase))
            {
                evidence = "pid=" + identity.Id + ";path=" + actualPath + ";startTicks=" + actualTicks;
                return false;
            }
            string hash = HashFile(actualPath);
            if (!String.Equals(hash, ExpectedSha256[GameExe], StringComparison.OrdinalIgnoreCase))
            {
                evidence = "pid=" + processId + ";path=" + actualPath + ";startTicks=" + actualTicks + ";sha256=" + hash;
                return false;
            }
            evidence = "pid=" + processId + ";path=" + actualPath + ";startTicks=" + actualTicks + ";sha256=" + hash;
            return true;
        }
        catch (Exception error)
        {
            evidence = error.GetType().Name + ":" + error.Message;
            return false;
        }
    }

    private static bool DrainFrameTerminationEvents(IntPtr process, IntPtr eventBuffer, uint processId,
        ref bool eventOutstanding, ref uint outstandingPid, ref uint outstandingTid,
        ref uint outstandingCode, ref uint outstandingContinueStatus, ref bool outstandingFileHandleClosed,
        BreakpointInfo[] breakpoints, out string evidence)
    {
        DateTime deadline = DateTime.UtcNow.AddSeconds(5);
        bool targetExitEventContinued = false;
        uint drainThreadId = GetCurrentThreadId();
        if (eventOutstanding)
        {
            targetExitEventContinued = outstandingCode == ExitProcessDebugEvent && outstandingPid == processId;
            if (!outstandingFileHandleClosed)
            {
                CloseExplicitDebugEventFileHandle(eventBuffer, outstandingCode);
                outstandingFileHandleClosed = true;
            }
            if (!ContinueDebugEvent(outstandingPid, outstandingTid, outstandingContinueStatus))
            {
                int error = Marshal.GetLastWin32Error();
                evidence = "continue_outstanding_failed;code=" + outstandingCode + ";pid=" + outstandingPid
                    + ";tid=" + outstandingTid + ";status=0x" + outstandingContinueStatus.ToString("X8")
                    + ";osThreadId=" + GetCurrentThreadId() + ";error=" + error;
                return false;
            }
            Log("FRAME_TERMINATION_OUTSTANDING_EVENT_CONTINUED code=" + outstandingCode
                + " pid=" + outstandingPid + " tid=" + outstandingTid
                + " status=0x" + outstandingContinueStatus.ToString("X8")
                + " osThreadId=" + GetCurrentThreadId() + " targetExitEvent=" + targetExitEventContinued);
            eventOutstanding = false;
            outstandingPid = outstandingTid = outstandingCode = 0;
            outstandingContinueStatus = DbgContinue;
            outstandingFileHandleClosed = false;
        }

        while (!targetExitEventContinued && DateTime.UtcNow < deadline)
        {
            uint waitThreadId = GetCurrentThreadId();
            int remaining = (int)Math.Max(1, Math.Min(250, (deadline - DateTime.UtcNow).TotalMilliseconds));
            if (!WaitForDebugEvent(eventBuffer, (uint)remaining))
            {
                int error = Marshal.GetLastWin32Error();
                if (error == 121 || error == 258) continue;
                evidence = "wait_for_exit_event_failed;osThreadId=" + GetCurrentThreadId() + ";error=" + error;
                return false;
            }

            uint code = unchecked((uint)Marshal.ReadInt32(eventBuffer, 0));
            uint pid = unchecked((uint)Marshal.ReadInt32(eventBuffer, 4));
            uint tid = unchecked((uint)Marshal.ReadInt32(eventBuffer, 8));
            uint exceptionCode = code == ExceptionDebugEvent
                ? unchecked((uint)Marshal.ReadInt32(eventBuffer, 12)) : 0;
            uint exceptionAddress = code == ExceptionDebugEvent
                ? unchecked((uint)Marshal.ReadInt32(eventBuffer, 24)) : 0;
            uint status = FrameDebugEventContinueStatus(code, exceptionCode, exceptionAddress, breakpoints);
            eventOutstanding = true;
            outstandingPid = pid;
            outstandingTid = tid;
            outstandingCode = code;
            outstandingContinueStatus = status;
            outstandingFileHandleClosed = false;
            Log("FRAME_TERMINATION_DEBUG_EVENT code=" + code + " pid=" + pid + " tid=" + tid
                + " waitOsThreadId=" + waitThreadId + " continueStatus=0x" + status.ToString("X8")
                + (code == ExceptionDebugEvent ? " exception=0x" + exceptionCode.ToString("X8")
                    + " address=0x" + exceptionAddress.ToString("X8") : ""));
            CloseExplicitDebugEventFileHandle(eventBuffer, code);
            outstandingFileHandleClosed = true;
            uint continueThreadId = GetCurrentThreadId();
            if (!ContinueDebugEvent(pid, tid, status))
            {
                int error = Marshal.GetLastWin32Error();
                evidence = "continue_exit_drain_event_failed;code=" + code + ";pid=" + pid + ";tid=" + tid
                    + ";status=0x" + status.ToString("X8") + ";osThreadId=" + continueThreadId + ";error=" + error;
                return false;
            }
            Log("FRAME_TERMINATION_DEBUG_EVENT_CONTINUED code=" + code + " pid=" + pid + " tid=" + tid
                + " status=0x" + status.ToString("X8") + " osThreadId=" + continueThreadId);
            eventOutstanding = false;
            outstandingPid = outstandingTid = outstandingCode = 0;
            outstandingContinueStatus = DbgContinue;
            outstandingFileHandleClosed = false;
            if (code == ExitProcessDebugEvent && pid == processId) targetExitEventContinued = true;
        }

        if (!targetExitEventContinued)
        {
            evidence = "target_exit_process_event_not_drained_within_5s;osThreadId=" + drainThreadId;
            return false;
        }
        uint waitResult = WaitForSingleObject(process, 2000);
        uint exitCode;
        bool exitRead = GetExitCodeProcess(process, out exitCode);
        if (waitResult != 0)
        {
            evidence = "exit_event_continued_but_process_not_signaled;waitResult=0x" + waitResult.ToString("X8")
                + ";exitCode=" + (exitRead ? "0x" + exitCode.ToString("X8") : "unreadable")
                + ";osThreadId=" + GetCurrentThreadId();
            return false;
        }
        evidence = "targetExitEventContinued=true;waitResult=0x" + waitResult.ToString("X8")
            + ";exitCode=" + (exitRead ? "0x" + exitCode.ToString("X8") : "unreadable")
            + ";osThreadId=" + GetCurrentThreadId();
        return exitRead;
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
        byte fogConsumerOriginalByte = 0;
        bool fogConsumerBoundaryVerified = false;
        bool attached = false;
        bool detached = false;
        bool breakpointsRestored = false;
        bool fatal = false;
        bool processExited = false;
        bool processExitUnconfirmed = false;
        int detachError = 0;
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
        BreakpointInfo fogConsumerBreakpoint = null;
        BreakpointInfo presentBreakpoint = null;
        BreakpointInfo missionBreakpoint = null;
        int missionProbeAttempts = 0;
        bool missionProbeFinished = false;
        PendingStep pendingStep = null;
        RemoteReadbackSession remote = null;
        RemoteCameraSetterSession cameraSetter = null;
        bool eventOutstanding = false, outstandingFileHandleClosed = false;
        uint outstandingPid = 0, outstandingTid = 0, outstandingCode = 0, outstandingContinueStatus = DbgContinue;
        uint attachOsThreadId = 0, lastWaitOsThreadId = 0, lastContinueOsThreadId = 0;
        uint debugEventsSeen = 0, debugEventsContinued = 0, lastDebugEventCode = 0;
        uint debugCreateThreadEvents = 0, debugExitThreadEvents = 0, debugLoadDllEvents = 0;
        uint debugExceptionEvents = 0, debugSingleStepEvents = 0, lastDebugEventPid = 0, lastDebugEventTid = 0;
        const uint ProcessAccessForFrameCapture = 0x0001 | 0x0008 | 0x0010 | 0x0020 | 0x0400 | 0x00100000;
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
            process = OpenProcess(ProcessAccessForFrameCapture, false, processId);
            if (process == IntPtr.Zero) throw new InvalidOperationException("OpenProcess failed: " + Marshal.GetLastWin32Error());
            eventBuffer = Marshal.AllocHGlobal(128);
            attachOsThreadId = GetCurrentThreadId();
            Log("DEBUG_ATTACH_REQUEST osThreadId=" + attachOsThreadId + " pid=" + processId
                + " processAccess=0x" + ProcessAccessForFrameCapture.ToString("X8")
                + " openProcessBeforeAttach=true threadContextAccess=0x" + (ThreadGetContext | ThreadSetContext).ToString("X8"));
            if (!DebugActiveProcess(processId))
                throw new InvalidOperationException("DebugActiveProcess failed: " + Marshal.GetLastWin32Error());
            attached = true;
            Log("DEBUG_ATTACH_RESULT success=true osThreadId=" + GetCurrentThreadId() + " pid=" + processId);
            if (identityProcess.HasExited || identityProcess.StartTime.ToUniversalTime().Ticks != expectedStartTicks
                || !String.Equals(Path.GetFullPath(identityProcess.MainModule.FileName), expectedPath,
                    StringComparison.OrdinalIgnoreCase))
                throw new InvalidOperationException("The retained process identity changed during debugger attach.");
            if (!DebugSetProcessKillOnExit(false))
                throw new InvalidOperationException("DebugSetProcessKillOnExit(false) failed: " + Marshal.GetLastWin32Error());
            Log("DEBUG_KILL_ON_EXIT_DISABLED osThreadId=" + GetCurrentThreadId());
            Log("DEBUG_ACCESS processMask=0x" + ProcessAccessForFrameCapture.ToString("X8")
                + " threadContextMask=0x" + (ThreadGetContext | ThreadSetContext).ToString("X8")
                + " remoteThreadCreation=false");
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
                lastWaitOsThreadId = GetCurrentThreadId();
                if (!WaitForDebugEvent(eventBuffer, 1000))
                {
                    int waitError = Marshal.GetLastWin32Error();
                    if (waitError == 121 || waitError == 258) continue;
                    throw new InvalidOperationException("WaitForDebugEvent failed: " + waitError);
                }

                uint eventCode = unchecked((uint)Marshal.ReadInt32(eventBuffer, 0));
                uint eventPid = unchecked((uint)Marshal.ReadInt32(eventBuffer, 4));
                uint eventTid = unchecked((uint)Marshal.ReadInt32(eventBuffer, 8));
                uint eventExceptionCode = eventCode == ExceptionDebugEvent
                    ? unchecked((uint)Marshal.ReadInt32(eventBuffer, 12)) : 0;
                uint eventExceptionAddress = eventCode == ExceptionDebugEvent
                    ? unchecked((uint)Marshal.ReadInt32(eventBuffer, 24)) : 0;
                eventOutstanding = true;
                outstandingPid = eventPid;
                outstandingTid = eventTid;
                outstandingCode = eventCode;
                outstandingContinueStatus = DbgContinue;
                outstandingFileHandleClosed = false;
                debugEventsSeen++;
                lastDebugEventCode = eventCode;
                lastDebugEventPid = eventPid;
                lastDebugEventTid = eventTid;
                if (eventCode == CreateThreadDebugEvent) debugCreateThreadEvents++;
                else if (eventCode == ExitThreadDebugEvent) debugExitThreadEvents++;
                else if (eventCode == LoadDllDebugEvent) debugLoadDllEvents++;
                else if (eventCode == ExceptionDebugEvent)
                {
                    debugExceptionEvents++;
                    if (eventExceptionCode == 0x80000004) debugSingleStepEvents++;
                }
                if (eventCode != ExceptionDebugEvent || eventExceptionCode != 0x80000004)
                    Log("DEBUG_EVENT sequence=" + debugEventsSeen + " code=" + eventCode
                        + " pid=" + eventPid + " tid=" + eventTid
                        + " exception=0x" + eventExceptionCode.ToString("X8")
                        + " address=0x" + eventExceptionAddress.ToString("X8")
                        + " waitOsThreadId=" + lastWaitOsThreadId);
                uint continueStatus = DbgContinue;
                bool shouldStop = false;
                BreakpointInfo hitBreakpoint = null;
                byte[] stoppedContext = null;
                try
                {
                    if (eventCode == CreateProcessDebugEvent)
                    {
                        IntPtr imageFile = Marshal.ReadIntPtr(eventBuffer, 12);
                        string imagePath = PathForHandle(imageFile);
                        if (imagePath.Length != 0 && !String.Equals(imagePath, NormalizePath(expectedPath), StringComparison.OrdinalIgnoreCase))
                            throw new InvalidOperationException("CREATE_PROCESS image path mismatch: " + imagePath);
                        CloseExplicitDebugEventFileHandle(eventBuffer, eventCode);
                        outstandingFileHandleClosed = true;
                        Log("CREATE_PROCESS path=" + imagePath
                            + " debuggerOwnedProcessInitialThreadHandles=retainedForSystemExitCleanup");
                    }
                    else if (eventCode == CreateThreadDebugEvent)
                    {
                        Log("CREATE_THREAD tid=" + eventTid + " debuggerOwnedThreadHandle=retainedForSystemExitCleanup");
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
                            string fogConsumerEvidence;
                            if (!VerifyNativeFogConsumerBoundary(process, imagePath, terrainBase,
                                out fogConsumerOriginalByte, out fogConsumerEvidence))
                                throw new InvalidOperationException("Terrain native fog consumer boundary signature failed: "
                                    + fogConsumerEvidence);
                            fogConsumerBoundaryVerified = true;
                            Log("Terrain verified base=0x" + terrainBase.ToString("X8") + " sha256=" + hash
                                + " " + atmosphereEvidence + " nativeFogConsumer=" + fogConsumerEvidence);
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
                        finally
                        {
                            CloseExplicitDebugEventFileHandle(eventBuffer, eventCode);
                            outstandingFileHandleClosed = true;
                        }
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
                                        if (fogConsumerBoundaryVerified)
                                            EnsureFogConsumerBreakpoint(process, terrainBase, fogConsumerOriginalByte,
                                                frame, eventTid, ref fogConsumerBreakpoint);
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
                            else if (fogConsumerBreakpoint != null && fogConsumerBreakpoint.Armed && exceptionAddress == fogConsumerBreakpoint.Address)
                                hitBreakpoint = fogConsumerBreakpoint;
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
                                        if (fogConsumerBreakpoint != null && fogConsumerBreakpoint.Armed
                                            && !RestoreBreakpointWhileStopped(process, fogConsumerBreakpoint))
                                            throw new InvalidOperationException("Could not disarm the previous generation's native fog boundary.");
                                        frame.CameraGeneration++;
                                        ResetNativeFogForGeneration(frame);
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

                                        if (fogConsumerBoundaryVerified && frame.Camera != null && frame.Camera.LayoutVerified)
                                            EnsureFogConsumerBreakpoint(process, terrainBase, fogConsumerOriginalByte,
                                                frame, eventTid, ref fogConsumerBreakpoint);
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
                                            PairPendingNativeFogSamples(frame, eventTid);
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
                                else if (hitBreakpoint == fogConsumerBreakpoint)
                                {
                                    if (!TryCountNativeFogCall(frame))
                                    {
                                        Log("NATIVE_FOG_SAMPLE_LIMIT_REACHED tid=" + eventTid
                                            + " processed=" + frame.NativeFogObservedCalls + " max=" + NativeFogSampleLimit);
                                    }
                                    else
                                    {
                                        if (frame.NativeFogSamplingStopped)
                                        {
                                            Log("NATIVE_FOG_SAMPLE_SKIPPED_AFTER_STOP tid=" + eventTid
                                                + " generation=" + frame.CameraGeneration);
                                        }
                                        else if (eventTid != frame.CameraThreadId)
                                        {
                                            frame.NativeFogIgnoredOtherThreadCalls++;
                                            Log("NATIVE_FOG_SAMPLE_IGNORED_OTHER_THREAD tid=" + eventTid
                                                + " renderTid=" + frame.CameraThreadId
                                                + " generation=" + frame.CameraGeneration);
                                        }
                                        else if (!CanSampleNativeFogForFrame(frame, eventTid))
                                        {
                                            frame.NativeFogRejectedCalls++;
                                            frame.NativeFogSamplingStopped = true;
                                            frame.NativeFogFailureReason = "The fog consumer call no longer matched the active verified camera generation.";
                                            Log("NATIVE_FOG_SAMPLE_REJECTED tid=" + eventTid
                                                + " generation=" + frame.CameraGeneration + " camera/projection gate failed");
                                        }
                                        else
                                        {
                                            try
                                            {
                                                NativeFogReadback sample = CaptureNativeFogConsumerSample(process,
                                                    stoppedContext, eventTid, terrainBase, terrainModuleSha256, frame);
                                                RecordNativeFogSample(frame, sample);
                                                Log("NATIVE_FOG_SAMPLE tid=" + eventTid
                                                    + " generation=" + sample.CameraGenerationAtSample
                                                    + " sink=0x" + sample.SinkPointer.ToString("X8")
                                                    + " flags=0x" + sample.RawFlags.ToString("X2")
                                                    + " startBits=0x" + sample.StartBits.ToString("X8")
                                                    + " endBits=0x" + sample.EndBits.ToString("X8")
                                                    + " return=0x" + sample.ReturnAddress.ToString("X8")
                                                    + " distanceBits=0x" + sample.DistanceArgumentBits.ToString("X8")
                                                    + " bypassRaw=0x" + sample.BypassArgumentRawDword.ToString("X8")
                                                    + " bypassByte=0x" + sample.BypassArgumentEffectiveByte.ToString("X2")
                                                    + " paired=" + sample.SameGenerationVerified
                                                    + " geometryVerified=" + sample.GeometryContractVerified);
                                            }
                                            catch (Exception fogError)
                                            {
                                                frame.NativeFogRejectedCalls++;
                                                frame.NativeFogSamplingStopped = true;
                                                frame.NativeFogFailureReason = "A native fog consumer sample failed validation: " + fogError.Message;
                                                Log("NATIVE_FOG_SAMPLE_REJECTED tid=" + eventTid + " " + fogError.Message);
                                            }
                                        }
                                        StopNativeFogAtCallLimit(frame);
                                        if (frame.NativeFogSampleLimitExceeded)
                                            Log("NATIVE_FOG_SAMPLE_LIMIT_REACHED tid=" + eventTid
                                                + " processed=" + frame.NativeFogObservedCalls + " max=" + NativeFogSampleLimit);
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
                                    FinalizeNativeFogAtPresent(frame, eventTid);
                                    if (!RestoreBreakpointWhileStopped(process, fogConsumerBreakpoint))
                                        throw new InvalidOperationException("Could not restore the native fog consumer breakpoint before Present readback.");
                                    Log("NATIVE_FOG_FINAL status=" + frame.NativeFogStatus
                                        + " observed=" + frame.NativeFogObservedCalls
                                        + " paired=" + frame.NativeFogPairedSampleCount
                                        + " rejected=" + frame.NativeFogRejectedCalls
                                        + " otherThread=" + frame.NativeFogIgnoredOtherThreadCalls
                                        + " matrixMismatch=" + frame.NativeFogIgnoredMatrixCalls
                                        + " reason=" + (frame.NativeFogConflictReason ?? frame.NativeFogFailureReason ?? "none"));
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
                                    if (hitBreakpoint == fogConsumerBreakpoint)
                                        rearmAfterStep = !frame.NativeFogSamplingStopped
                                            && !frame.NativeFogSampleLimitExceeded;
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
                        cameraBreakpoint, projectionBreakpoint, returnBreakpoint, atmosphereBreakpoint,
                        fogConsumerBreakpoint, presentBreakpoint, missionBreakpoint))
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
                        || fogConsumerBreakpoint != null && fogConsumerBreakpoint.Armed
                        || presentBreakpoint != null && presentBreakpoint.Armed
                        || missionBreakpoint != null && missionBreakpoint.Armed))
                {
                    if (!RestoreArmedBreakpointsWhileStopped(process,
                        cameraBreakpoint, projectionBreakpoint, returnBreakpoint, atmosphereBreakpoint,
                        fogConsumerBreakpoint, presentBreakpoint, missionBreakpoint))
                    {
                        Log("FATAL_STOP_BREAKPOINT_RESTORE_FAILED; terminating own verified scratch process");
                        TerminateProcess(process, 0xE008);
                        fatal = true;
                    }
                }

                outstandingContinueStatus = continueStatus;
                lastContinueOsThreadId = GetCurrentThreadId();
                if (!ContinueDebugEvent(eventPid, eventTid, continueStatus))
                {
                    fatal = true;
                    int continueError = Marshal.GetLastWin32Error();
                    Log("FATAL ContinueDebugEvent failed=" + continueError + " code=" + eventCode
                        + " pid=" + eventPid + " tid=" + eventTid + " status=0x" + continueStatus.ToString("X8")
                        + " osThreadId=" + lastContinueOsThreadId);
                    if (process != IntPtr.Zero && !TerminateProcess(process, 0xE009))
                        Log("FATAL ContinueDebugEvent recovery terminate failed=" + Marshal.GetLastWin32Error());
                    break;
                }
                eventOutstanding = false;
                outstandingPid = outstandingTid = outstandingCode = 0;
                outstandingContinueStatus = DbgContinue;
                outstandingFileHandleClosed = false;
                debugEventsContinued++;
                if (eventCode != ExceptionDebugEvent || eventExceptionCode != 0x80000004)
                    Log("DEBUG_CONTINUE_OK sequence=" + debugEventsContinued + " code=" + eventCode
                        + " pid=" + eventPid + " tid=" + eventTid + " status=0x" + continueStatus.ToString("X8")
                        + " osThreadId=" + lastContinueOsThreadId);
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
                || cameraMutationUnrestored || eventOutstanding
                || cameraBreakpoint != null && cameraBreakpoint.Armed
                || projectionBreakpoint != null && projectionBreakpoint.Armed
                || returnBreakpoint != null && returnBreakpoint.Armed
                || atmosphereBreakpoint != null && atmosphereBreakpoint.Armed
                || fogConsumerBreakpoint != null && fogConsumerBreakpoint.Armed
                || presentBreakpoint != null && presentBreakpoint.Armed
                || missionBreakpoint != null && missionBreakpoint.Armed))
            {
                string state = remote != null ? "REMOTE_STUB_IN_FLIGHT" : (cameraSetter != null ? "CAMERA_SETTER_IN_FLIGHT"
                    : (cameraMutationUnrestored ? "SELECTED_CAMERA_MUTATION_NOT_RESTORED"
                    : (pendingStep != null ? "SINGLE_STEP_IN_FLIGHT" : "ARMED_BREAKPOINTS_WITHOUT_STOPPED_EVENT")));
                string identityEvidence;
                if (!VerifyFrameTerminationIdentity(identityProcess, processId, expectedPath,
                    expectedStartTicks, out identityEvidence))
                {
                    processExitUnconfirmed = true;
                    fatal = true;
                    Log("FINAL_TERMINATION_REFUSED identity=" + identityEvidence + " state=" + state);
                }
                else
                {
                    Log("FINAL_" + state + "; terminating verified scratch before detach identity=" + identityEvidence);
                    if (!TerminateProcess(process, 0xE006))
                        Log("FINAL_TERMINATE_FAILED error=" + Marshal.GetLastWin32Error());
                    string drainEvidence;
                    if (DrainFrameTerminationEvents(process, eventBuffer, processId,
                        ref eventOutstanding, ref outstandingPid, ref outstandingTid, ref outstandingCode,
                        ref outstandingContinueStatus, ref outstandingFileHandleClosed,
                        new BreakpointInfo[] { cameraBreakpoint, projectionBreakpoint, returnBreakpoint,
                            atmosphereBreakpoint, fogConsumerBreakpoint, presentBreakpoint, missionBreakpoint },
                        out drainEvidence))
                    {
                        processExited = true;
                        Log("FINAL_TERMINATION_DRAIN_CONFIRMED " + drainEvidence);
                    }
                    else
                    {
                        fatal = true;
                        processExitUnconfirmed = true;
                        Log("FATAL_FINAL_TERMINATION_DRAIN_FAILED " + drainEvidence);
                    }
                }
            }
            if (attached && !processExitUnconfirmed)
            {
                Log("DEBUG_DETACH_REQUEST osThreadId=" + GetCurrentThreadId()
                    + " attachOsThreadId=" + attachOsThreadId + " processId=" + processId
                    + " eventOutstanding=" + eventOutstanding + " outstandingCode=" + outstandingCode
                    + " outstandingPid=" + outstandingPid + " outstandingTid=" + outstandingTid
                    + " outstandingContinueStatus=0x" + outstandingContinueStatus.ToString("X8")
                    + " waitOsThreadId=" + lastWaitOsThreadId + " lastContinueOsThreadId=" + lastContinueOsThreadId
                    + " lastEventCode=" + lastDebugEventCode + " lastEventPid=" + lastDebugEventPid
                    + " lastEventTid=" + lastDebugEventTid + " eventsSeen=" + debugEventsSeen
                    + " eventsContinued=" + debugEventsContinued + " createThreads=" + debugCreateThreadEvents
                    + " exitThreads=" + debugExitThreadEvents + " dllLoads=" + debugLoadDllEvents
                    + " exceptions=" + debugExceptionEvents + " singleSteps=" + debugSingleStepEvents);
                detached = DebugActiveProcessStop(processId);
                if (detached) Log("DEBUG_DETACHED osThreadId=" + GetCurrentThreadId());
                else
                {
                    fatal = true;
                    detachError = Marshal.GetLastWin32Error();
                    Log("FATAL DebugActiveProcessStop failed=" + detachError
                        + " osThreadId=" + GetCurrentThreadId() + " eventOutstanding=" + eventOutstanding
                        + " eventsSeen=" + debugEventsSeen + " eventsContinued=" + debugEventsContinued
                        + " lastEventCode=" + lastDebugEventCode + " lastEventPid=" + lastDebugEventPid
                        + " lastEventTid=" + lastDebugEventTid + " lastWaitOsThreadId=" + lastWaitOsThreadId
                        + " lastContinueOsThreadId=" + lastContinueOsThreadId);
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
                            string terminationEvidence;
                            if (!VerifyFrameTerminationIdentity(identityProcess, processId, expectedPath,
                                expectedStartTicks, out terminationEvidence))
                            {
                                processExitUnconfirmed = true;
                                Log("DETACH_FAILURE_TERMINATION_REFUSED identity=" + terminationEvidence);
                            }
                            else
                            {
                                Log("DETACH_FAILURE_TARGET_STILL_ACTIVE; terminating verified scratch identity=" + terminationEvidence);
                                if (!TerminateProcess(process, 0xE00A))
                                    Log("DETACH_FAILURE_TERMINATE_FAILED error=" + Marshal.GetLastWin32Error());
                                string drainEvidence;
                                if (DrainFrameTerminationEvents(process, eventBuffer, processId,
                                    ref eventOutstanding, ref outstandingPid, ref outstandingTid, ref outstandingCode,
                                    ref outstandingContinueStatus, ref outstandingFileHandleClosed,
                                    new BreakpointInfo[] { cameraBreakpoint, projectionBreakpoint, returnBreakpoint,
                                        atmosphereBreakpoint, fogConsumerBreakpoint, presentBreakpoint, missionBreakpoint },
                                    out drainEvidence))
                                {
                                    processExited = true;
                                    Log("DETACH_FAILURE_TERMINATION_DRAIN_CONFIRMED " + drainEvidence);
                                    uint detachThreadId = GetCurrentThreadId();
                                    bool detachedAfterExit = DebugActiveProcessStop(processId);
                                    int detachAfterExitError = detachedAfterExit ? 0 : Marshal.GetLastWin32Error();
                                    Log("DEBUG_DETACH_AFTER_TERMINATION success=" + detachedAfterExit
                                        + " osThreadId=" + detachThreadId + " error=" + detachAfterExitError);
                                    detached = detachedAfterExit;
                                }
                                else
                                {
                                    processExitUnconfirmed = true;
                                    Log("DETACH_FAILURE_TERMINATION_DRAIN_FAILED " + drainEvidence);
                                }
                            }
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
                projectionBreakpoint, returnBreakpoint, atmosphereBreakpoint,
                fogConsumerBreakpoint, presentBreakpoint, missionBreakpoint);
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
        BreakpointInfo atmosphereBreakpoint, BreakpointInfo fogConsumerBreakpoint,
        BreakpointInfo presentBreakpoint, BreakpointInfo missionBreakpoint)
    {
        return RestoreBreakpointWhileStopped(process, cameraBreakpoint)
            && RestoreBreakpointWhileStopped(process, projectionBreakpoint)
            && RestoreBreakpointWhileStopped(process, returnBreakpoint)
            && RestoreBreakpointWhileStopped(process, atmosphereBreakpoint)
            && RestoreBreakpointWhileStopped(process, fogConsumerBreakpoint)
            && RestoreBreakpointWhileStopped(process, presentBreakpoint)
            && RestoreBreakpointWhileStopped(process, missionBreakpoint);
    }

    private static bool AreBreakpointsRestored(BreakpointInfo cameraBreakpoint,
        BreakpointInfo projectionBreakpoint, BreakpointInfo returnBreakpoint,
        BreakpointInfo atmosphereBreakpoint, BreakpointInfo fogConsumerBreakpoint,
        BreakpointInfo presentBreakpoint,
        BreakpointInfo missionBreakpoint)
    {
        return (cameraBreakpoint == null || !cameraBreakpoint.Armed)
            && (projectionBreakpoint == null || !projectionBreakpoint.Armed)
            && (returnBreakpoint == null || !returnBreakpoint.Armed)
            && (atmosphereBreakpoint == null || !atmosphereBreakpoint.Armed)
            && (fogConsumerBreakpoint == null || !fogConsumerBreakpoint.Armed)
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
