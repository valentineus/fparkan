using System;
using System.Collections;
using System.Collections.Generic;
using System.Diagnostics;
using System.Globalization;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;
using System.Web.Script.Serialization;
using System.Drawing;
using System.Drawing.Imaging;

internal static partial class NativeFrameCapture
{
    private const uint NgiRendererGlobalRva = 0x3A460;
    private const uint NgiLastErrorGlobalRva = 0x3A464;
    private const uint NgiDeviceGlobalRva = 0x3A488;
    private const uint NgiRendererVtableRva = 0x315E0;
    private const uint NgiWindowedPresentRva = 0x6E1B;
    private const uint NgiFullscreenPresentRva = 0x6E38;
    private const uint DdLockReadOnly = 0x00000010;
    private const uint DdLockDoNotWait = 0x00004000;
    private const int D3dDeviceGetRenderTargetOffset = 0x24;
    private const int DdSurfaceLockOffset = 0x64;
    private const int DdSurfaceUnlockOffset = 0x80;
    private const int DdSurfaceReleaseOffset = 0x08;
    private const uint DdErrSurfaceBusy = 0x887601AE;
    private const uint DdErrWasStillDrawing = 0x8876021C;
    private const uint DdsdPitch = 0x00000008;
    private const uint DdsdHeight = 0x00000002;
    private const uint DdsdWidth = 0x00000004;
    private const uint DdsdPixelFormat = 0x00001000;
    private const uint DdsdLpSurface = 0x00000800;
    private const uint DdpfFourCc = 0x00000004;
    private const uint DdpfRgb = 0x00000040;
    private const int DdSurfaceDesc2Size = 124;
    private const int CameraInputJsonLimit = 1024 * 1024;
    private const uint ContextAllX86 = 0x0001003F;
    // THREAD_QUERY_INFORMATION from the Windows SDK; required by NtQueryInformationThread.
    private const uint ThreadQueryInformation = 0x00000040;
    private const int ContextEflagsOffset = 192;
    private const int PixelReadbackLimit = 64 * 1024 * 1024;
    private const uint RemoteCodePage = 0x1000;
    private const uint RemoteDataOffset = 0x1000;
    private const uint RemoteAllocationBytes = 0x2000;
    private const uint RemoteStubMaxCallStackBytes = 24;
    private const uint PageGuard = 0x00000100;
    private const uint RemoteDataStatusOffset = 0;
    private const uint RemoteDataGetHrOffset = 4;
    private const uint RemoteDataLockHrOffset = 8;
    private const uint RemoteDataUnlockHrOffset = 12;
    private const uint RemoteDataReleaseCountOffset = 16;
    private const uint RemoteDataSurfaceOffset = 20;
    private const uint RemoteDataDescOffset = 24;
    private const uint CameraSetterMatrixOffset = 64;
    private const uint CameraTransformInterfaceVtableRva = 0x66558;
    private const uint CameraTransformSetterRva = 0x54C70;
    private const uint CameraTransformGetterRva = 0x55110;
    private const uint CameraTransformInvalidatorRva = 0x55280;
    private const uint CameraTransformSetterSlotOffset = 0x1C;
    private const uint CameraTransformGetterSlotOffset = 0x20;
    private const uint CameraTransformInvalidatorSlotOffset = 0x30;
    private const uint CameraSetterWorldMatrixPointerOffset = 128;
    private const uint RenderFunctionStackFrameBytes = 0x70;

    [StructLayout(LayoutKind.Sequential)]
    private struct ClientIdX86
    {
        public IntPtr UniqueProcess;
        public IntPtr UniqueThread;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct ThreadBasicInformationX86
    {
        public int ExitStatus;
        public IntPtr TebBaseAddress;
        public ClientIdX86 ClientId;
        public UIntPtr AffinityMask;
        public int Priority;
        public int BasePriority;
    }

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern IntPtr VirtualAllocEx(IntPtr process, IntPtr address,
        UIntPtr size, uint allocationType, uint protection);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool VirtualFreeEx(IntPtr process, IntPtr address,
        UIntPtr size, uint freeType);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool TerminateProcess(IntPtr process, uint exitCode);

    [DllImport("ntdll.dll")]
    private static extern int NtQueryInformationThread(IntPtr thread, int informationClass,
        out ThreadBasicInformationX86 information, int informationLength, IntPtr returnLength);

    private sealed class SurfaceReadback
    {
        public uint Width;
        public uint Height;
        public int Pitch;
        public uint BitCount;
        public uint RedMask;
        public uint GreenMask;
        public uint BlueMask;
        public uint AlphaMask;
        public uint SurfacePointer;
        public byte[] Rows;
        public string Sha256;
        public bool RgbMasksVerified;
    }

    private sealed class ProjectionReadback
    {
        public int[] Viewport;
        public float Near;
        public float Far;
        public float Fov;
        public byte Mode;
        public uint Renderer;
        public uint RendererVtable;
        public bool Verified;
        public string Failure;
    }

    private sealed class CameraReadback
    {
        public uint Camera;
        public uint Parent;
        public bool Parented;
        public uint Vtable;
        public uint TransformInterface;
        public uint TransformVtable;
        public byte CameraMode;
        public uint EntryEsp;
        public uint ReturnAddress;
        public uint SelectorField;
        public uint[] Words;
        public uint[] EffectiveWorldWords;
        public byte[] EntryMatrixBytes;
        public byte[] ProjectionMatrixBytes;
        public byte[] EntryEffectiveWorldMatrixBytes;
        public byte[] ProjectionEffectiveWorldMatrixBytes;
        public bool CurrentAtProjectionVerified;
        public bool EffectiveWorldAtProjectionVerified;
        public bool WordsChangedAtProjection;
        public bool EffectiveWorldChangedAtProjection;
        public string EntryMatrixSha256;
        public string ProjectionMatrixSha256;
        public string EntryEffectiveWorldMatrixSha256;
        public string ProjectionEffectiveWorldMatrixSha256;
        public bool MatrixFinite;
        public bool EffectiveWorldMatrixFinite;
        public bool LayoutVerified;
        public string Failure;
    }

    private sealed class SelectedCameraInput
    {
        public string Path;
        public bool Synthetic;
        public bool ParentedCamera;
        public bool UsedEffectiveWorldMatrix;
        public byte[] MatrixBytes;
        public uint[] MatrixWords;
        public byte[] Selector0MatrixBytes;
        public uint[] Selector0MatrixWords;
        public string MatrixSha256;
        public int[] Viewport;
        public float Near;
        public float Far;
        public float FieldOfView;
        public byte ProjectionMode;
    }

    private enum CameraSetterPhase { Apply, Restore }

    private sealed class RemoteCameraSetterSession
    {
        public RemoteCode Code;
        public uint Region;
        public uint Camera;
        public uint ThreadId;
        public uint OriginalEsp;
        public byte[] OriginalContext;
        public byte[] CallerStack;
        public uint FunctionEntryEsp;
        public uint FunctionReturnAddress;
        public uint EsiAtBoundary;
        public byte[] FunctionCallerStack;
        public byte[] MatrixBytes;
        public CameraSetterPhase Phase;
        public BreakpointInfo ResumeBreakpoint;
        public bool RearmOnStep;
        public DateTime DeadlineUtc;
    }

    private sealed class RemoteCode
    {
        public uint Base;
        public uint TrapAddress;
        public uint DataBase;
        public byte[] Bytes;
        public int[] CallInstructionOffsets;
        public int MaxCallStackBytes;
    }

    private static IntPtr AllocateAlignedX86Context(out IntPtr allocation)
    {
        allocation = Marshal.AllocHGlobal(X86ContextSize + 15);
        long raw = allocation.ToInt64();
        return new IntPtr((raw + 15L) & ~15L);
    }

    private static byte[] GetFullX86Context(IntPtr thread)
    {
        IntPtr allocation;
        IntPtr context = AllocateAlignedX86Context(out allocation);
        try
        {
            Marshal.Copy(new byte[X86ContextSize], 0, context, X86ContextSize);
            Marshal.WriteInt32(context, 0, unchecked((int)ContextAllX86));
            if (!GetThreadContext(thread, context))
                throw new InvalidOperationException("GetThreadContext(CONTEXT_ALL_X86) failed: " + Marshal.GetLastWin32Error());
            byte[] bytes = new byte[X86ContextSize];
            Marshal.Copy(context, bytes, 0, bytes.Length);
            return bytes;
        }
        finally { Marshal.FreeHGlobal(allocation); }
    }

    private static void SetFullX86Context(IntPtr thread, byte[] bytes)
    {
        if (bytes == null || bytes.Length != X86ContextSize)
            throw new ArgumentException("Expected a complete x86 CONTEXT buffer.", "bytes");
        IntPtr allocation;
        IntPtr context = AllocateAlignedX86Context(out allocation);
        try
        {
            Marshal.Copy(bytes, 0, context, bytes.Length);
            Marshal.WriteInt32(context, 0, unchecked((int)ContextAllX86));
            if (!SetThreadContext(thread, context))
                throw new InvalidOperationException("SetThreadContext(CONTEXT_ALL_X86) failed: " + Marshal.GetLastWin32Error());
        }
        finally { Marshal.FreeHGlobal(allocation); }
    }

    private static byte[] ContextForRemoteCode(byte[] original, uint codeAddress, uint stackPointer)
    {
        if (original == null || original.Length != X86ContextSize)
            throw new ArgumentException("Expected a complete x86 CONTEXT buffer.", "original");
        byte[] result = (byte[])original.Clone();
        Buffer.BlockCopy(BitConverter.GetBytes(unchecked((int)ContextAllX86)), 0, result, 0, 4);
        Buffer.BlockCopy(BitConverter.GetBytes(unchecked((int)codeAddress)), 0, result, ContextEipOffset, 4);
        Buffer.BlockCopy(BitConverter.GetBytes(unchecked((int)stackPointer)), 0, result, ContextEspOffset, 4);
        int flags = BitConverter.ToInt32(result, ContextEflagsOffset) & ~0x100;
        Buffer.BlockCopy(BitConverter.GetBytes(flags), 0, result, ContextEflagsOffset, 4);
        return result;
    }

    private static uint ComputeSafeNativeStackPointer(IntPtr process, IntPtr thread, byte[] originalContext)
    {
        if (originalContext == null || originalContext.Length != X86ContextSize)
            throw new ArgumentException("Expected a complete x86 CONTEXT buffer.", "originalContext");
        ThreadBasicInformationX86 information;
        int status = NtQueryInformationThread(thread, 0, out information,
            Marshal.SizeOf(typeof(ThreadBasicInformationX86)), IntPtr.Zero);
        if (status < 0 || information.TebBaseAddress == IntPtr.Zero)
            throw new InvalidOperationException("NtQueryInformationThread(ThreadBasicInformation) failed: 0x" + status.ToString("X8"));
        uint teb = unchecked((uint)information.TebBaseAddress.ToInt32());
        uint stackBase = ReadU32Exact(process, unchecked(teb + 4));
        uint stackLimit = ReadU32Exact(process, unchecked(teb + 8));
        uint originalEsp = BitConverter.ToUInt32(originalContext, ContextEspOffset);
        if (!HasNativeStackBounds(stackLimit, stackBase, originalEsp, RemoteStubMaxCallStackBytes))
        {
            Log("REMOTE_STACK_BOUNDS_INVALID teb=0x" + teb.ToString("X8")
                + " stackLimit=0x" + stackLimit.ToString("X8") + " originalEsp=0x" + originalEsp.ToString("X8")
                + " stackBase=0x" + stackBase.ToString("X8") + " callerBytes=" + RemoteStubMaxCallStackBytes);
            throw new InvalidOperationException("Native render-thread ESP does not leave the verified caller-push bytes within its committed stack bounds.");
        }
        uint lowestPushByte = originalEsp - RemoteStubMaxCallStackBytes;
        MemoryBasicInformation memory = new MemoryBasicInformation();
        UIntPtr queried = VirtualQueryEx(process, Ptr(lowestPushByte), out memory,
            new UIntPtr((uint)Marshal.SizeOf(typeof(MemoryBasicInformation))));
        uint regionBase = unchecked((uint)memory.BaseAddress.ToInt32());
        uint regionSize = memory.RegionSize.ToUInt32();
        string stackEvidence = "state=0x" + memory.State.ToString("X8")
            + " protect=0x" + memory.Protect.ToString("X8") + " region=0x" + regionBase.ToString("X8")
            + "+0x" + regionSize.ToString("X8");
        bool stackRangeWritable = queried.ToUInt32() != 0 && IsSafeNativeStackRange(stackLimit, stackBase,
            originalEsp, RemoteStubMaxCallStackBytes, regionBase, regionSize, memory.State, memory.Protect);
        Log("REMOTE_STACK_BOUNDS teb=0x" + teb.ToString("X8")
            + " stackLimit=0x" + stackLimit.ToString("X8") + " originalEsp=0x" + originalEsp.ToString("X8")
            + " stackBase=0x" + stackBase.ToString("X8") + " lowestPushByte=0x" + lowestPushByte.ToString("X8")
            + " callerBytes=" + RemoteStubMaxCallStackBytes + " " + stackEvidence + " writable=" + stackRangeWritable);
        if (!stackRangeWritable)
            throw new InvalidOperationException("The full present-thread stub push range is not in one committed writable stack region or touches a guard page.");
        return originalEsp;
    }

    private static bool HasNativeStackBounds(uint stackLimit, uint stackBase, uint originalEsp, uint callerBytes)
    {
        return callerBytes != 0 && originalEsp > stackLimit && originalEsp < stackBase
            && originalEsp >= callerBytes && originalEsp - callerBytes >= stackLimit;
    }

    private static bool IsSafeNativeStackRange(uint stackLimit, uint stackBase, uint originalEsp,
        uint callerBytes, uint regionBase, uint regionSize, uint state, uint protect)
    {
        if (!HasNativeStackBounds(stackLimit, stackBase, originalEsp, callerBytes) || regionSize == 0)
            return false;
        uint lowestPushByte = originalEsp - callerBytes;
        ulong regionEnd = (ulong)regionBase + regionSize;
        uint protection = protect & 0xFF;
        bool writableProtection = protection == 0x04 || protection == 0x08
            || protection == 0x40 || protection == 0x80;
        return state == MemCommit && (protect & PageGuard) == 0 && writableProtection
            && lowestPushByte >= regionBase && (ulong)originalEsp <= regionEnd;
    }

    private sealed class X86CodeBuilder
    {
        private readonly List<byte> _bytes = new List<byte>();
        private readonly Dictionary<string, int> _labels = new Dictionary<string, int>(StringComparer.Ordinal);
        private readonly List<KeyValuePair<int, string>> _relative32 = new List<KeyValuePair<int, string>>();
        private readonly List<int> _callInstructionOffsets = new List<int>();
        private int _int3Count;
        private int _terminalOffset = -1;
        private int _currentPushBytes;
        private int _maxCallStackBytes;

        public int Position { get { return _bytes.Count; } }

        public void Emit(params byte[] bytes)
        {
            _bytes.AddRange(bytes);
        }

        public void EmitU32(uint value)
        {
            _bytes.AddRange(BitConverter.GetBytes(value));
        }

        public void PushReg(byte opcode)
        {
            if (opcode < 0x50 || opcode > 0x57)
                throw new InvalidOperationException("Expected one x86 push-register opcode.");
            _bytes.Add(opcode);
            _currentPushBytes = checked(_currentPushBytes + 4);
        }

        public void PushImm8(byte value)
        {
            _bytes.Add(0x6A);
            _bytes.Add(value);
            _currentPushBytes = checked(_currentPushBytes + 4);
        }

        public void PushImm32(uint value)
        {
            _bytes.Add(0x68);
            EmitU32(value);
            _currentPushBytes = checked(_currentPushBytes + 4);
        }

        public void CallStdCall(int argumentBytes, params byte[] opcode)
        {
            if (argumentBytes < 0 || (argumentBytes & 3) != 0 || _currentPushBytes != argumentBytes
                || opcode == null || opcode.Length == 0)
                throw new InvalidOperationException("x86 stdcall must match the emitted dword arguments.");
            _maxCallStackBytes = Math.Max(_maxCallStackBytes, checked(argumentBytes + 4));
            _callInstructionOffsets.Add(_bytes.Count);
            _bytes.AddRange(opcode);
            // IDirect3DDevice7/IDirectDrawSurface7 vtable methods use STDMETHODCALLTYPE
            // (stdcall), so the callee removes its arguments before returning.
            _currentPushBytes -= argumentBytes;
        }

        public void Mark(string label)
        {
            if (_labels.ContainsKey(label)) throw new InvalidOperationException("Duplicate x86 label: " + label);
            _labels.Add(label, _bytes.Count);
        }

        public void JumpIf(byte condition, string label)
        {
            _bytes.Add(0x0F);
            _bytes.Add((byte)(0x80 | condition));
            int operand = _bytes.Count;
            EmitU32(0);
            _relative32.Add(new KeyValuePair<int, string>(operand, label));
        }

        public void Jump(string label)
        {
            _bytes.Add(0xE9);
            int operand = _bytes.Count;
            EmitU32(0);
            _relative32.Add(new KeyValuePair<int, string>(operand, label));
        }

        public void EmitTerminalInt3()
        {
            _terminalOffset = _bytes.Count;
            _bytes.Add(0xCC);
            _int3Count++;
        }

        public int Int3Count { get { return _int3Count; } }
        public int TerminalOffset { get { return _terminalOffset; } }
        public int BranchCount { get { return _relative32.Count; } }
        public int MaxCallStackBytes { get { return _maxCallStackBytes; } }
        public int[] CallInstructionOffsets { get { return _callInstructionOffsets.ToArray(); } }

        public void AssertTerminalControlFlow(byte[] finishedBytes, int expectedBranchCount)
        {
            if (finishedBytes == null || _int3Count != 1 || _terminalOffset != finishedBytes.Length - 1
                || _terminalOffset < 0 || finishedBytes[_terminalOffset] != 0xCC
                || _relative32.Count != expectedBranchCount || _currentPushBytes != 0)
                throw new InvalidOperationException("Readback stub must have one terminal INT3 and the expected branch count.");

            // Validate only branches registered by Jump/JumpIf. A rel32 operand
            // can itself contain 0xCC bytes, which are not instructions.
            for (int i = 0; i < _relative32.Count; i++)
            {
                KeyValuePair<int, string> branch = _relative32[i];
                int labelOffset;
                if (!_labels.TryGetValue(branch.Value, out labelOffset)
                    || branch.Key < 0 || branch.Key + 4 > finishedBytes.Length)
                    throw new InvalidOperationException("Readback stub has an invalid branch record.");
                int resolvedOffset = checked(branch.Key + 4 + BitConverter.ToInt32(finishedBytes, branch.Key));
                if (resolvedOffset != labelOffset || resolvedOffset < 0 || resolvedOffset > _terminalOffset)
                    throw new InvalidOperationException("Readback stub branch does not resolve to a valid instruction label.");
            }
        }

        public byte[] Finish()
        {
            for (int i = 0; i < _relative32.Count; i++)
            {
                KeyValuePair<int, string> fixup = _relative32[i];
                int destination;
                if (!_labels.TryGetValue(fixup.Value, out destination))
                    throw new InvalidOperationException("Unresolved x86 label: " + fixup.Value);
                int relative = destination - (fixup.Key + 4);
                byte[] bytes = BitConverter.GetBytes(relative);
                for (int j = 0; j < 4; j++) _bytes[fixup.Key + j] = bytes[j];
            }
            return _bytes.ToArray();
        }
    }

    private static RemoteCode BuildReadbackStub(uint allocationBase, uint device, bool cleanup)
    {
        uint data = unchecked(allocationBase + RemoteDataOffset);
        uint statusAddress = unchecked(data + RemoteDataStatusOffset);
        uint getHrAddress = unchecked(data + RemoteDataGetHrOffset);
        uint lockHrAddress = unchecked(data + RemoteDataLockHrOffset);
        uint unlockHrAddress = unchecked(data + RemoteDataUnlockHrOffset);
        uint releaseAddress = unchecked(data + RemoteDataReleaseCountOffset);
        uint surfaceAddress = unchecked(data + RemoteDataSurfaceOffset);
        uint descAddress = unchecked(data + RemoteDataDescOffset);

        X86CodeBuilder code = new X86CodeBuilder();
        if (!cleanup)
        {
            code.Emit(0xBE); code.EmitU32(device); // mov esi, device
            code.Emit(0x8B, 0x06);                 // mov eax, [esi]
            code.Emit(0x8D, 0x3D); code.EmitU32(surfaceAddress); // lea edi, [surface]
            code.PushReg(0x57); code.PushReg(0x56); // push edi; push esi
            code.CallStdCall(8, 0xFF, 0x50, (byte)D3dDeviceGetRenderTargetOffset); // call [eax+GetRenderTarget]
            code.Emit(0xA3); code.EmitU32(getHrAddress); // mov [getHr], eax
            code.Emit(0x85, 0xC0);                 // test eax,eax
            code.JumpIf(0x89, "get_success");     // jns get_success
            code.Emit(0x8B, 0x35); code.EmitU32(surfaceAddress); // mov esi, [surface]
            code.Emit(0x85, 0xF6);                 // test esi,esi
            code.JumpIf(0x84, "get_failed_no_surface");
            code.Emit(0xC7, 0x05); code.EmitU32(statusAddress); code.EmitU32(2);
            code.Jump("release_and_stop");

            code.Mark("get_success");
            code.Emit(0x8B, 0x35); code.EmitU32(surfaceAddress); // mov esi, [surface]
            code.Emit(0x85, 0xF6);                 // test esi,esi
            code.JumpIf(0x84, "get_success_no_surface");
            code.Emit(0x8B, 0x06);                 // mov eax, [esi]
            code.PushImm8(0x00);                  // push NULL event
            code.PushImm32(DdLockReadOnly | DdLockDoNotWait);
            code.Emit(0x8D, 0x3D); code.EmitU32(descAddress); // lea edi,[desc]
            code.PushReg(0x57); code.PushImm8(0x00); code.PushReg(0x56); // desc; NULL rect; this
            code.CallStdCall(20, 0xFF, 0x50, (byte)DdSurfaceLockOffset); // call [eax+Lock]
            code.Emit(0xA3); code.EmitU32(lockHrAddress); // mov [lockHr], eax
            code.Emit(0x85, 0xC0);                 // test eax,eax
            code.JumpIf(0x88, "lock_failed");     // js lock_failed
            code.Emit(0xC7, 0x05); code.EmitU32(statusAddress); code.EmitU32(1);
            code.Jump("stop");                    // lock held; host copies pixels, then cleans up

            code.Mark("lock_failed");
            code.Emit(0xC7, 0x05); code.EmitU32(statusAddress); code.EmitU32(4);
            code.Jump("release_and_stop");

            code.Mark("get_success_no_surface");
            code.Emit(0xC7, 0x05); code.EmitU32(statusAddress); code.EmitU32(3);
            code.Jump("stop");

            code.Mark("get_failed_no_surface");
            code.Emit(0xC7, 0x05); code.EmitU32(statusAddress); code.EmitU32(5);
            code.Jump("stop");

            code.Mark("release_and_stop");
            code.Emit(0x8B, 0x35); code.EmitU32(surfaceAddress);
            code.Emit(0x8B, 0x06);                 // mov eax, [esi]
            code.PushReg(0x56);                    // push this
            code.CallStdCall(4, 0xFF, 0x50, (byte)DdSurfaceReleaseOffset); // call [eax+Release]
            code.Emit(0xA3); code.EmitU32(releaseAddress);
            code.Jump("stop");
        }
        else
        {
            code.Emit(0x8B, 0x35); code.EmitU32(surfaceAddress); // mov esi, [surface]
            code.Emit(0x85, 0xF6);
            code.JumpIf(0x84, "cleanup_no_surface");
            code.Emit(0x8B, 0x06);                 // mov eax, [esi]
            code.PushImm8(0x00);                   // Unlock(NULL)
            code.PushReg(0x56);                    // push this
            code.CallStdCall(8, 0xFF, 0x90, 0x80, 0x00, 0x00, 0x00); // call dword ptr [eax+0x80] (disp32)
            code.Emit(0xA3); code.EmitU32(unlockHrAddress);
            code.Emit(0x8B, 0x06);                 // call Release even if Unlock failed
            code.PushReg(0x56);
            code.CallStdCall(4, 0xFF, 0x50, (byte)DdSurfaceReleaseOffset);
            code.Emit(0xA3); code.EmitU32(releaseAddress);
            code.Emit(0xC7, 0x05); code.EmitU32(statusAddress); code.EmitU32(6);
            code.Jump("stop");
            code.Mark("cleanup_no_surface");
            code.Emit(0xC7, 0x05); code.EmitU32(statusAddress); code.EmitU32(7);
            code.Jump("stop");
        }

        code.Mark("stop");
        code.EmitTerminalInt3();

        byte[] bytes = code.Finish();
        code.AssertTerminalControlFlow(bytes, cleanup ? 3 : 10);
        int expectedStack = cleanup ? 12 : 24;
        if (code.MaxCallStackBytes != expectedStack)
            throw new InvalidOperationException("Unexpected x86 caller stack footprint in the readback stub.");
        if (bytes.Length > RemoteCodePage)
            throw new InvalidOperationException("Remote DirectDraw stub exceeds one code page.");
        RemoteCode result = new RemoteCode();
        result.Base = allocationBase;
        result.DataBase = data;
        result.Bytes = bytes;
        result.CallInstructionOffsets = code.CallInstructionOffsets;
        result.MaxCallStackBytes = code.MaxCallStackBytes;
        result.TrapAddress = unchecked(allocationBase + (uint)code.TerminalOffset);
        return result;
    }

    // Boundary tracing only asks the already-active D3D7 device for its current
    // render target, records the returned interface pointer, and balances the
    // COM AddRef with Release. It deliberately never locks or reads pixels.
    private static RemoteCode BuildSurfaceProbeStub(uint allocationBase, uint device)
    {
        uint data = unchecked(allocationBase + RemoteDataOffset);
        uint statusAddress = unchecked(data + RemoteDataStatusOffset);
        uint getHrAddress = unchecked(data + RemoteDataGetHrOffset);
        uint releaseAddress = unchecked(data + RemoteDataReleaseCountOffset);
        uint surfaceAddress = unchecked(data + RemoteDataSurfaceOffset);
        X86CodeBuilder code = new X86CodeBuilder();
        code.Emit(0xBE); code.EmitU32(device); // mov esi, device
        code.Emit(0x8B, 0x06);                 // mov eax, [esi]
        code.Emit(0x8D, 0x3D); code.EmitU32(surfaceAddress); // lea edi, [surface]
        code.PushReg(0x57); code.PushReg(0x56); // push output; push this
        code.CallStdCall(8, 0xFF, 0x50, (byte)D3dDeviceGetRenderTargetOffset);
        code.Emit(0xA3); code.EmitU32(getHrAddress); // mov [getHr], eax
        code.Emit(0x85, 0xC0);                 // test eax,eax
        code.JumpIf(0x89, "get_succeeded");   // jns
        code.Emit(0xC7, 0x05); code.EmitU32(statusAddress); code.EmitU32(9);
        code.Jump("release_surface");
        code.Mark("get_succeeded");
        code.Emit(0x8B, 0x35); code.EmitU32(surfaceAddress); // mov esi,[surface]
        code.Emit(0x85, 0xF6);                 // test esi,esi
        code.JumpIf(0x84, "no_surface");
        code.Emit(0xC7, 0x05); code.EmitU32(statusAddress); code.EmitU32(8);
        code.Jump("release_surface");
        code.Mark("no_surface");
        code.Emit(0xC7, 0x05); code.EmitU32(statusAddress); code.EmitU32(10);
        code.Jump("stop");
        code.Mark("release_surface");
        code.Emit(0x8B, 0x35); code.EmitU32(surfaceAddress);
        code.Emit(0x85, 0xF6);
        code.JumpIf(0x84, "stop");
        code.Emit(0x8B, 0x06);
        code.PushReg(0x56);
        code.CallStdCall(4, 0xFF, 0x50, (byte)DdSurfaceReleaseOffset);
        code.Emit(0xA3); code.EmitU32(releaseAddress);
        code.Mark("stop");
        code.EmitTerminalInt3();
        byte[] bytes = code.Finish();
        code.AssertTerminalControlFlow(bytes, 6);
        if (code.MaxCallStackBytes != 12 || bytes.Length > RemoteCodePage)
            throw new InvalidOperationException("Surface probe stub has an unexpected stack footprint or size.");
        RemoteCode result = new RemoteCode();
        result.Base = allocationBase;
        result.DataBase = data;
        result.Bytes = bytes;
        result.CallInstructionOffsets = code.CallInstructionOffsets;
        result.MaxCallStackBytes = code.MaxCallStackBytes;
        result.TrapAddress = unchecked(allocationBase + (uint)code.TerminalOffset);
        return result;
    }

    private static byte CameraSetterMode(CameraSetterPhase phase)
    {
        // Native setter mode 2 treats the supplied matrix as world space and
        // converts it relative to camera+0x0C. Restoring a captured +0x20
        // selector-0 value must use local mode 1.
        return phase == CameraSetterPhase.Apply ? (byte)2 : (byte)1;
    }

    private static RemoteCode BuildCameraSetterStub(uint allocationBase, uint transformInterface, byte mode)
    {
        if (mode != 1 && mode != 2)
            throw new ArgumentOutOfRangeException("mode", "Only verified local and world camera setter modes are supported.");
        uint dataBase = unchecked(allocationBase + RemoteDataOffset);
        uint matrixAddress = unchecked(dataBase + CameraSetterMatrixOffset);
        X86CodeBuilder code = new X86CodeBuilder();
        code.Emit(0xBE); code.EmitU32(transformInterface); // mov esi, transform interface
        code.Emit(0x8B, 0x0E);                           // mov ecx, [esi] (verified vtable)
        code.PushImm32(matrixAddress);                   // argument 3: 64-byte transform
        code.PushImm8(mode);                             // argument 2: native transform space
        code.PushReg(0x56);                              // argument 1: this
        code.CallStdCall(12, 0xFF, 0x51, (byte)CameraTransformSetterSlotOffset);
        if (mode == 2)
        {
            // Mode 2 stores a parent-relative local matrix and invalidates the
            // effective-world cache at transformInterface+0x11C (camera+0x120). The selector-2 getter
            // recomputes camera+0x60 before the caller verifies the new pose.
            code.Emit(0x8B, 0x0E);                       // mov ecx, [esi] (verified vtable)
            code.PushImm32(unchecked(dataBase + CameraSetterWorldMatrixPointerOffset));
            code.PushImm8(2);                            // selector 2: effective world matrix
            code.PushReg(0x56);                          // argument 1: transform interface
            code.CallStdCall(12, 0xFF, 0x51, (byte)CameraTransformGetterSlotOffset);
        }
        code.EmitTerminalInt3();
        byte[] bytes = code.Finish();
        code.AssertTerminalControlFlow(bytes, 0);
        if (code.MaxCallStackBytes != 16 || bytes.Length > RemoteCodePage)
            throw new InvalidOperationException("Camera setter stub has an unexpected stack footprint or size.");
        RemoteCode result = new RemoteCode();
        result.Base = allocationBase;
        result.DataBase = dataBase;
        result.Bytes = bytes;
        result.CallInstructionOffsets = code.CallInstructionOffsets;
        result.MaxCallStackBytes = code.MaxCallStackBytes;
        result.TrapAddress = unchecked(allocationBase + (uint)code.TerminalOffset);
        return result;
    }

    private static bool VerifyCameraSetterAbi(IntPtr process, uint terrainBase, uint camera,
        out uint transformInterface, out string evidence)
    {
        transformInterface = unchecked(camera + 4);
        uint primaryVtable = ReadU32(process, camera);
        uint selector = ReadU32(process, unchecked(camera + 0x10));
        byte[] modeBytes = new byte[1];
        bool modeRead = ReadExact(process, unchecked(camera + 0x1A0), modeBytes);
        uint transformVtable = ReadU32(process, transformInterface);
        uint setter = ReadU32(process, unchecked(transformVtable + CameraTransformSetterSlotOffset));
        uint getter = ReadU32(process, unchecked(transformVtable + CameraTransformGetterSlotOffset));
        uint invalidate = ReadU32(process, unchecked(transformVtable + CameraTransformInvalidatorSlotOffset));
        bool valid = terrainBase != 0
            && primaryVtable == unchecked(terrainBase + ExternalCameraVtableRva)
            && transformVtable == unchecked(terrainBase + CameraTransformInterfaceVtableRva)
            && setter == unchecked(terrainBase + CameraTransformSetterRva)
            && getter == unchecked(terrainBase + CameraTransformGetterRva)
            && invalidate == unchecked(terrainBase + CameraTransformInvalidatorRva)
            && selector == 0xFFFFFFFF && modeRead && modeBytes[0] == 0;
        evidence = "camera=0x" + camera.ToString("X8") + " primaryVtable=0x" + primaryVtable.ToString("X8")
            + " transform=0x" + transformInterface.ToString("X8") + " transformVtable=0x" + transformVtable.ToString("X8")
            + " setter=0x" + setter.ToString("X8") + " getter=0x" + getter.ToString("X8")
            + " invalidator=0x" + invalidate.ToString("X8")
            + " selector=0x" + selector.ToString("X8")
            + " mode=" + (modeRead ? modeBytes[0].ToString(CultureInfo.InvariantCulture) : "unreadable")
            + " valid=" + valid;
        return valid;
    }

    private static RemoteCameraSetterSession StartCameraSetter(IntPtr process, uint threadId,
        uint terrainBase, uint camera, byte[] matrixBytes, byte[] originalContext,
        BreakpointInfo resumeBreakpoint, bool rearmOnStep, CameraSetterPhase phase,
        uint functionEntryEsp, uint functionReturnAddress)
    {
        if (matrixBytes == null || matrixBytes.Length != 64 || originalContext == null
            || originalContext.Length != X86ContextSize || resumeBreakpoint == null || !resumeBreakpoint.Armed)
            throw new InvalidOperationException("Camera setter request is incomplete.");
        string abiEvidence;
        uint transformInterface;
        if (!VerifyCameraSetterAbi(process, terrainBase, camera, out transformInterface, out abiEvidence))
            throw new InvalidOperationException("Camera setter ABI refused: " + abiEvidence);
        byte setterMode = CameraSetterMode(phase);
        Log("verified native camera setter ABI " + abiEvidence + " slot=+0x1C this=0x"
            + transformInterface.ToString("X8") + " mode=" + setterMode
            + " matrixSpace=" + (phase == CameraSetterPhase.Apply ? "world" : "local")
            + " matrixBytes=64 ret=0x0C");

        uint expectedEip = unchecked(resumeBreakpoint.Address + 1);
        uint originalEip = BitConverter.ToUInt32(originalContext, ContextEipOffset);
        uint originalEsp = BitConverter.ToUInt32(originalContext, ContextEspOffset);
        if (originalEip != expectedEip || originalEsp > UInt32.MaxValue - 8
            || functionEntryEsp > UInt32.MaxValue - 8)
            throw new InvalidOperationException("Camera setter breakpoint EIP/ESP did not match its verified boundary.");
        byte[] callerStack = new byte[8];
        if (!ReadExact(process, originalEsp, callerStack))
            throw new InvalidOperationException("Could not snapshot the actual native stack words at the setter boundary.");
        byte[] functionCallerStack = new byte[8];
        if (!ReadExact(process, functionEntryEsp, functionCallerStack)
            || !CameraFunctionStackMatches(phase, functionCallerStack, functionReturnAddress, camera))
            throw new InvalidOperationException("The native stdRenderGame return/argument stack did not match the selected setter phase.");
        uint esiAtBoundary = BitConverter.ToUInt32(originalContext, ContextEsiOffset);
        if (phase == CameraSetterPhase.Apply)
        {
            if (originalEsp != functionEntryEsp || BitConverter.ToUInt32(callerStack, 4) != camera)
                throw new InvalidOperationException("The camera-entry setter stack did not contain the original camera argument.");
        }
        else
        {
            if (functionEntryEsp < RenderFunctionStackFrameBytes
                || originalEsp != functionEntryEsp - RenderFunctionStackFrameBytes)
                throw new InvalidOperationException("The restore boundary did not preserve the selected function ESP.");
        }

        IntPtr thread = OpenThread(ThreadGetContext | ThreadSetContext | ThreadQueryInformation, false, threadId);
        if (thread == IntPtr.Zero) throw new InvalidOperationException("OpenThread for camera setter failed: " + Marshal.GetLastWin32Error());
        uint region = 0;
        try
        {
            uint safeEsp = ComputeSafeNativeStackPointer(process, thread, originalContext);
            region = AllocateReadbackRegion(process);
            RemoteCode code = BuildCameraSetterStub(region, transformInterface, setterMode);
            WriteRemoteStub(process, code, false);
            UIntPtr written;
            if (!WriteProcessMemory(process, Ptr(unchecked(code.DataBase + CameraSetterMatrixOffset)), matrixBytes,
                new UIntPtr(64), out written) || written.ToUInt32() != 64)
                throw new InvalidOperationException("Could not copy the validated 64-byte camera matrix to the bounded setter stub.");
            RemoteCameraSetterSession session = new RemoteCameraSetterSession();
            session.Code = code;
            session.Region = region;
            session.Camera = camera;
            session.ThreadId = threadId;
            session.OriginalEsp = originalEsp;
            session.OriginalContext = (byte[])originalContext.Clone();
            session.CallerStack = callerStack;
            session.FunctionEntryEsp = functionEntryEsp;
            session.FunctionReturnAddress = functionReturnAddress;
            session.EsiAtBoundary = esiAtBoundary;
            session.FunctionCallerStack = functionCallerStack;
            session.MatrixBytes = (byte[])matrixBytes.Clone();
            session.Phase = phase;
            session.ResumeBreakpoint = resumeBreakpoint;
            session.RearmOnStep = rearmOnStep;
            session.DeadlineUtc = DateTime.UtcNow.AddSeconds(5);
            SetFullX86Context(thread, ContextForRemoteCode(originalContext, code.Base, safeEsp));
            Log("CAMERA_SETTER_STARTED phase=" + phase + " tid=" + threadId
                + " camera=0x" + camera.ToString("X8") + " stub=0x" + code.Base.ToString("X8")
                + " terminal=0x" + code.TrapAddress.ToString("X8") + " originalEsp=0x" + originalEsp.ToString("X8")
                + " currentStack=" + BitConverter.ToString(callerStack)
                + " functionCallerStack=" + BitConverter.ToString(functionCallerStack)
                + " observedFunctionArgument=0x" + BitConverter.ToUInt32(functionCallerStack, 4).ToString("X8")
                + " esiAtBoundary=0x" + esiAtBoundary.ToString("X8")
                + " matrixSha256=" + HashBytes(matrixBytes));
            return session;
        }
        catch
        {
            if (region != 0) VirtualFreeEx(process, Ptr(region), UIntPtr.Zero, 0x8000);
            throw;
        }
        finally { CloseHandle(thread); }
    }

    private static bool VerifyCameraSetterReturnContext(IntPtr process,
        RemoteCameraSetterSession session, out string evidence)
    {
        evidence = "unverified";
        IntPtr thread = OpenThread(ThreadGetContext, false, session.ThreadId);
        if (thread == IntPtr.Zero)
        {
            evidence = "OpenThread failed=" + Marshal.GetLastWin32Error();
            return false;
        }
        try
        {
            byte[] context = GetFullX86Context(thread);
            uint eip = BitConverter.ToUInt32(context, ContextEipOffset);
            uint esp = BitConverter.ToUInt32(context, ContextEspOffset);
            byte[] callerStack = new byte[session.CallerStack.Length];
            byte[] functionCallerStack = new byte[session.FunctionCallerStack.Length];
            byte[] matrix = new byte[session.MatrixBytes.Length];
            bool stackReadable = ReadExact(process, session.OriginalEsp, callerStack);
            bool functionStackReadable = ReadExact(process, session.FunctionEntryEsp, functionCallerStack);
            bool matrixReadable = ReadExact(process,
                unchecked(session.Code.DataBase + CameraSetterMatrixOffset), matrix);
            bool stackMatches = stackReadable && ByteArraysEqual(callerStack, session.CallerStack);
            bool functionStackMatches = functionStackReadable
                && ByteArraysEqual(functionCallerStack, session.FunctionCallerStack)
                && CameraFunctionStackMatches(session.Phase, functionCallerStack,
                    session.FunctionReturnAddress, session.Camera);
            bool matrixMatches = matrixReadable && ByteArraysEqual(matrix, session.MatrixBytes);
            bool valid = eip == unchecked(session.Code.TrapAddress + 1)
                && esp == session.OriginalEsp && stackMatches && functionStackMatches
                && matrixMatches;
            evidence = "expectedEip=0x" + unchecked(session.Code.TrapAddress + 1).ToString("X8")
                + " actualEip=0x" + eip.ToString("X8") + " expectedEsp=0x" + session.OriginalEsp.ToString("X8")
                + " actualEsp=0x" + esp.ToString("X8") + " callerStackIntact=" + stackMatches
                + " functionCallerStackIntact=" + functionStackMatches
                + " observedFunctionArgument=0x" + BitConverter.ToUInt32(session.FunctionCallerStack, 4).ToString("X8")
                + " esiAtBoundary=0x" + session.EsiAtBoundary.ToString("X8")
                + " matrixDataIntact=" + matrixMatches;
            return valid;
        }
        finally { CloseHandle(thread); }
    }

    private static byte[] ReadCameraMatrix(IntPtr process, uint camera)
    {
        byte[] matrix = new byte[64];
        if (!ReadExact(process, unchecked(camera + 0x20), matrix))
            throw new InvalidOperationException("Could not read back the native camera selector-0 matrix.");
        return matrix;
    }

    private static uint ReadCameraParent(IntPtr process, uint camera)
    {
        if (camera == 0 || camera > UInt32.MaxValue - 0x0C)
            throw new InvalidOperationException("Camera pointer is outside the verified parent-field range.");
        return ReadU32(process, unchecked(camera + 0x0C));
    }

    private static byte[] ReadEffectiveWorldCameraMatrix(IntPtr process, uint camera)
    {
        byte[] matrix = new byte[64];
        if (camera == 0 || camera > UInt32.MaxValue - 0x60
            || !ReadExact(process, unchecked(camera + 0x60), matrix))
            throw new InvalidOperationException("Could not read the native effective-world camera matrix at camera+0x60.");
        return matrix;
    }

    private static uint[] MatrixWords(byte[] matrixBytes)
    {
        if (matrixBytes == null || matrixBytes.Length != 64)
            throw new ArgumentException("A camera matrix must contain exactly 64 bytes.", "matrixBytes");
        uint[] words = new uint[16];
        for (int i = 0; i < words.Length; i++)
            words[i] = BitConverter.ToUInt32(matrixBytes, i * 4);
        return words;
    }

    private static bool CameraMatricesNearlyEqual(byte[] left, byte[] right)
    {
        if (left == null || right == null || left.Length != 64 || right.Length != 64) return false;
        const double absoluteTolerance = 0.001;
        for (int i = 0; i < 16; i++)
        {
            float a = BitConverter.ToSingle(left, i * 4);
            float b = BitConverter.ToSingle(right, i * 4);
            if (!IsFinite(a) || !IsFinite(b) || Math.Abs((double)a - b) > absoluteTolerance)
                return false;
        }
        return true;
    }

    private static bool CameraFunctionStackMatches(CameraSetterPhase phase, byte[] stackWords,
        uint expectedReturnAddress, uint camera)
    {
        if (stackWords == null || stackWords.Length != 8
            || BitConverter.ToUInt32(stackWords, 0) != expectedReturnAddress)
            return false;
        return phase == CameraSetterPhase.Restore
            || BitConverter.ToUInt32(stackWords, 4) == camera;
    }

    private static void SelfCheckRenderInvocationGates()
    {
        const uint threadId = 11;
        const uint eip = 0x10013CE5;
        const uint esp = 0x001AFC28;
        const uint camera = 0x1644A8D8;
        const uint returnAddress = 0x1005F243;
        uint observedArgument;
        bool overwrittenArgumentAccepted = RenderInvocationMatches(threadId, threadId,
            eip, eip, esp, esp, camera, camera, returnAddress, returnAddress, true,
            0x0BADF00D, out observedArgument) && observedArgument == 0x0BADF00D;
        bool wrongThreadRejected = !RenderInvocationMatches(threadId + 1, threadId,
            eip, eip, esp, esp, camera, camera, returnAddress, returnAddress, true,
            0x0BADF00D, out observedArgument);
        bool wrongEsiRejected = !RenderInvocationMatches(threadId, threadId,
            eip, eip, esp, esp, camera + 4, camera, returnAddress, returnAddress, true,
            0x0BADF00D, out observedArgument);
        bool wrongReturnRejected = !RenderInvocationMatches(threadId, threadId,
            eip, eip, esp, esp, camera, camera, returnAddress + 4, returnAddress, false,
            0x0BADF00D, out observedArgument);
        bool epilogueAllowsReassignedEsi = RenderInvocationMatches(threadId, threadId,
            eip, eip, esp, esp, camera + 0x100, camera, returnAddress, returnAddress, false,
            0x0BADF00D, out observedArgument);
        byte[] entryStack = new byte[8];
        Buffer.BlockCopy(BitConverter.GetBytes(returnAddress), 0, entryStack, 0, 4);
        Buffer.BlockCopy(BitConverter.GetBytes(camera), 0, entryStack, 4, 4);
        byte[] overwrittenStack = (byte[])entryStack.Clone();
        Buffer.BlockCopy(BitConverter.GetBytes(0x0BADF00Du), 0, overwrittenStack, 4, 4);
        bool applyRequiresOriginalCameraArgument = CameraFunctionStackMatches(
            CameraSetterPhase.Apply, entryStack, returnAddress, camera)
            && !CameraFunctionStackMatches(CameraSetterPhase.Apply, overwrittenStack, returnAddress, camera);
        bool restorePreservesOverwrittenArgument = CameraFunctionStackMatches(
            CameraSetterPhase.Restore, overwrittenStack, returnAddress, camera)
            && !CameraFunctionStackMatches(CameraSetterPhase.Restore, overwrittenStack, returnAddress + 4, camera);
        if (!overwrittenArgumentAccepted || !wrongThreadRejected || !wrongEsiRejected
            || !epilogueAllowsReassignedEsi
            || !wrongReturnRejected || !applyRequiresOriginalCameraArgument
            || !restorePreservesOverwrittenArgument)
            throw new InvalidOperationException("Self-check failed: selected invocation identity or phase-specific camera stack gate.");
    }

    private static bool IsSelectedRenderInvocation(IntPtr process, byte[] context, uint threadId,
        SelectedCameraState selected, uint expectedBreakpointEip, bool requireEsiCamera,
        out string evidence)
    {
        if (selected == null)
        {
            evidence = "selected invocation state is missing";
            return false;
        }
        if (threadId != selected.ThreadId)
        {
            evidence = "actualTid=" + threadId + " expectedTid=" + selected.ThreadId
                + " camera=0x" + selected.CameraPointer.ToString("X8");
            return false;
        }
        return IsRenderInvocation(process, context, threadId, selected.ThreadId,
            selected.CameraPointer, selected.EntryEsp, selected.FunctionStackEsp,
            selected.ReturnAddress, expectedBreakpointEip, requireEsiCamera, out evidence);
    }

    private static bool IsRenderInvocation(IntPtr process, byte[] context, uint threadId,
        uint expectedThreadId, uint camera, uint entryEsp, uint functionStackEsp,
        uint returnAddress, uint expectedBreakpointEip, bool requireEsiCamera, out string evidence)
    {
        uint actualEip = 0;
        uint actualEsp = 0;
        uint actualEsi = 0;
        if (context != null && context.Length == X86ContextSize)
        {
            actualEip = BitConverter.ToUInt32(context, ContextEipOffset);
            actualEsp = BitConverter.ToUInt32(context, ContextEspOffset);
            actualEsi = BitConverter.ToUInt32(context, ContextEsiOffset);
        }
        byte[] callerStack = new byte[8];
        bool stackReadable = entryEsp <= UInt32.MaxValue - 8
            && ReadExact(process, entryEsp, callerStack);
        uint savedReturn = stackReadable ? BitConverter.ToUInt32(callerStack, 0) : 0;
        uint observedArgument = stackReadable ? BitConverter.ToUInt32(callerStack, 4) : 0;
        uint diagnosticArgument;
        bool valid = RenderInvocationMatches(threadId, expectedThreadId, actualEip,
            expectedBreakpointEip, actualEsp, functionStackEsp, actualEsi, camera,
            savedReturn, returnAddress, requireEsiCamera, observedArgument, out diagnosticArgument) && stackReadable;
        evidence = "actualTid=" + threadId + " expectedTid=" + expectedThreadId
            + " actualEip=0x" + actualEip.ToString("X8") + " expectedEip=0x" + expectedBreakpointEip.ToString("X8")
            + " actualEsp=0x" + actualEsp.ToString("X8") + " expectedFunctionEsp=0x" + functionStackEsp.ToString("X8")
            + " entryEsp=0x" + entryEsp.ToString("X8") + " savedReturn=0x" + savedReturn.ToString("X8")
            + " expectedReturn=0x" + returnAddress.ToString("X8") + " observedEntryArgument=0x" + diagnosticArgument.ToString("X8")
            + " entryArgumentMayBeOverwritten=true esi=0x" + actualEsi.ToString("X8")
            + " esiRequired=" + requireEsiCamera
            + " expectedCamera=0x" + camera.ToString("X8") + " stackReadable=" + stackReadable
            + " match=" + valid;
        return valid;
    }

    private static bool RenderInvocationMatches(uint threadId, uint expectedThreadId,
        uint eip, uint expectedEip, uint esp, uint expectedEsp, uint esi, uint camera,
        uint savedReturn, uint expectedReturn, bool requireEsiCamera,
        uint observedArgument, out uint diagnosticArgument)
    {
        // The camera argument's original stack slot becomes a COM output pointer
        // during World3D's selector-6 query. Keep it in diagnostics, not identity.
        diagnosticArgument = observedArgument;
        return threadId == expectedThreadId && eip == expectedEip && esp == expectedEsp
            && (!requireEsiCamera || esi == camera) && savedReturn == expectedReturn;
    }

    private static RemoteReadbackSession StartRemoteReadback(IntPtr process, uint threadId, uint ngiBase,
        BreakpointInfo present, FrameSnapshot frame, string outputJson)
    {
        uint device;
        uint getRenderTarget;
        string evidence;
        if (!VerifyD3d7GetRenderTarget(process, ngiBase, out device, out getRenderTarget, out evidence))
            throw new InvalidOperationException("D3D7 GetRenderTarget call was refused: " + evidence);
        Log("verified D3D7 GetRenderTarget ABI " + evidence);
        if (frame == null || frame.Projection == null
            || ReadU32(process, unchecked(ngiBase + NgiRendererGlobalRva)) != frame.Projection.Renderer)
            throw new InvalidOperationException("Ngi32 renderer changed since the matching projection snapshot.");
        if (ReadU32(process, unchecked(frame.Projection.Renderer + 0x114)) == 0)
            throw new InvalidOperationException("Ngi32 is not in the verified windowed present path for RVA 0x6E1B.");

        IntPtr thread = OpenThread(ThreadGetContext | ThreadSetContext | ThreadQueryInformation, false, threadId);
        if (thread == IntPtr.Zero) throw new InvalidOperationException("OpenThread for D3D7 readback failed: " + Marshal.GetLastWin32Error());
        uint region = 0;
        try
        {
            byte[] originalContext = GetFullX86Context(thread);
            uint stoppedEip = BitConverter.ToUInt32(originalContext, ContextEipOffset);
            if (stoppedEip != unchecked(present.Address + 1))
                throw new InvalidOperationException("Ngi32 present EIP did not match the armed callsite.");
            uint safeEsp = ComputeSafeNativeStackPointer(process, thread, originalContext);
            uint originalEsp = BitConverter.ToUInt32(originalContext, ContextEspOffset);
            if (originalEsp > UInt32.MaxValue - PresentCallArgumentsBytes)
                throw new InvalidOperationException("Ngi32 present argument block address overflowed.");
            byte[] presentStackArguments = new byte[PresentCallArgumentsBytes];
            if (!ReadExact(process, originalEsp, presentStackArguments))
                throw new InvalidOperationException("Could not read the original Ngi32 present call's 24-byte argument block.");
            region = AllocateReadbackRegion(process);
            RemoteCode acquire = BuildReadbackStub(region, device, false);
            WriteRemoteStub(process, acquire, true);
            RemoteReadbackSession session = new RemoteReadbackSession();
            session.Acquire = acquire;
            session.Region = region;
            session.ThreadId = threadId;
            session.SafeEsp = safeEsp;
            session.OriginalContext = originalContext;
            session.PresentStackArguments = presentStackArguments;
            session.PresentStackArgumentsIntact = true;
            session.ValidatePresentStackArguments = true;
            session.RemoteContextIntact = true;
            session.PresentBreakpoint = present;
            session.Frame = frame;
            session.Generation = frame.CameraGeneration;
            session.OutputJson = outputJson;
            session.OutputPng = Path.ChangeExtension(outputJson, ".png");
            session.DeadlineUtc = DateTime.UtcNow.AddSeconds(5);
            session.Phase = RemoteReadbackPhase.Acquire;
            Log("PRESENT_STACK_ARGS stage=before-com esp=0x" + originalEsp.ToString("X8")
                + " bytes=" + BitConverter.ToString(presentStackArguments));
            Log("REMOTE_ACQUIRE_STARTED tid=" + threadId + " stub=0x" + acquire.Base.ToString("X8")
                + " terminal=0x" + acquire.TrapAddress.ToString("X8") + " safeEsp=0x" + safeEsp.ToString("X8")
                + " generation=" + session.Generation);
            SetFullX86Context(thread, ContextForRemoteCode(originalContext, acquire.Base, safeEsp));
            return session;
        }
        catch
        {
            if (region != 0) VirtualFreeEx(process, Ptr(region), UIntPtr.Zero, 0x8000);
            throw;
        }
        finally { CloseHandle(thread); }
    }

    private static RemoteReadbackSession StartRemoteBoundaryReadback(IntPtr process, uint threadId,
        uint ngiBase, BreakpointInfo boundary, FrameSnapshot frame, string outputJson,
        BreakpointInfo[] allBreakpoints)
    {
        uint device;
        uint getRenderTarget;
        string evidence;
        if (!VerifyD3d7GetRenderTarget(process, ngiBase, out device, out getRenderTarget, out evidence))
            throw new InvalidOperationException("D3D7 EndScene render-target readback was refused: " + evidence);
        uint renderer = ReadU32Exact(process, unchecked(ngiBase + NgiRendererGlobalRva));
        if (frame == null || frame.Projection == null || !frame.Projection.Verified
            || frame.Projection.Renderer != renderer
            || ReadU32Exact(process, renderer) != unchecked(ngiBase + NgiRendererVtableRva))
            throw new InvalidOperationException("EndScene readback renderer changed since the selected projection snapshot.");
        if (ReadU32Exact(process, unchecked(renderer + 0x114)) == 0)
            throw new InvalidOperationException("Ngi32 is not in the verified windowed render-target path.");

        IntPtr thread = OpenThread(ThreadGetContext | ThreadSetContext | ThreadQueryInformation, false, threadId);
        if (thread == IntPtr.Zero)
            throw new InvalidOperationException("OpenThread for selected EndScene readback failed: " + Marshal.GetLastWin32Error());
        uint region = 0;
        try
        {
            byte[] originalContext = GetFullX86Context(thread);
            if (boundary == null || BitConverter.ToUInt32(originalContext, ContextEipOffset)
                != unchecked(boundary.Address + 1))
                throw new InvalidOperationException("Selected EndScene EIP did not match its verified post-call boundary.");
            uint safeEsp = ComputeSafeNativeStackPointer(process, thread, originalContext);
            region = AllocateReadbackRegion(process);
            RemoteCode acquire = BuildReadbackStub(region, device, false);
            WriteRemoteStub(process, acquire, true);
            BreakpointInfo[] suspended;
            if (!SuspendBreakpointsWhileStopped(process, allBreakpoints, out suspended))
                throw new InvalidOperationException("Could not suspend every armed breakpoint before EndScene readback.");
            RemoteReadbackSession session = new RemoteReadbackSession();
            session.Acquire = acquire;
            session.Region = region;
            session.ThreadId = threadId;
            session.SafeEsp = safeEsp;
            session.OriginalContext = originalContext;
            session.PresentStackArgumentsIntact = true;
            session.ValidatePresentStackArguments = false;
            session.RemoteContextIntact = true;
            session.PresentBreakpoint = boundary;
            session.ResumeBreakpoint = boundary;
            session.SuspendedBreakpoints = suspended;
            session.Frame = frame;
            session.Generation = frame.CameraGeneration;
            session.OutputJson = outputJson;
            session.OutputPng = Path.ChangeExtension(outputJson, ".png");
            session.DeadlineUtc = DateTime.UtcNow.AddSeconds(5);
            session.Phase = RemoteReadbackPhase.Acquire;
            session.PixelBoundary = "Ngi32 EndScene after selected World3D projection, before Present";
            Log("BOUNDARY_READBACK_STARTED kind=" + boundary.Name + " tid=" + threadId
                + " device=0x" + device.ToString("X8") + " renderer=0x" + renderer.ToString("X8")
                + " generation=" + session.Generation + " stub=0x" + acquire.Base.ToString("X8")
                + " suspendedBreakpoints=" + suspended.Length + " presentStackArgs=not-applicable");
            SetFullX86Context(thread, ContextForRemoteCode(originalContext, acquire.Base, safeEsp));
            return session;
        }
        catch
        {
            if (region != 0) VirtualFreeEx(process, Ptr(region), UIntPtr.Zero, 0x8000);
            throw;
        }
        finally { CloseHandle(thread); }
    }

    private static bool SuspendBreakpointsWhileStopped(IntPtr process, BreakpointInfo[] candidates,
        out BreakpointInfo[] suspended)
    {
        List<BreakpointInfo> active = new List<BreakpointInfo>();
        if (candidates != null)
            for (int i = 0; i < candidates.Length; i++)
            {
                BreakpointInfo breakpoint = candidates[i];
                if (breakpoint == null || !breakpoint.Armed || active.Contains(breakpoint)) continue;
                active.Add(breakpoint);
                if (!WriteByte(process, breakpoint.Address, breakpoint.OriginalByte))
                {
                    bool rollbackSucceeded = true;
                    foreach (BreakpointInfo suspendedBreakpoint in active)
                    {
                        if (!WriteByte(process, suspendedBreakpoint.Address, 0xCC))
                        {
                            rollbackSucceeded = false;
                            Log("FATAL boundary probe could not roll back breakpoint suspension for "
                                + suspendedBreakpoint.Name + " error=" + Marshal.GetLastWin32Error());
                        }
                        // Leave this true even after a failed write so final cleanup retries
                        // restoring the original byte before any detach.
                        suspendedBreakpoint.Armed = true;
                    }
                    suspended = active.ToArray();
                    Log("BOUNDARY_PROBE_SUSPEND_FAILED name=" + breakpoint.Name
                        + " rollbackSucceeded=" + rollbackSucceeded);
                    return false;
                }
                breakpoint.Armed = false;
                Log("BOUNDARY_PROBE_BREAKPOINT_SUSPENDED " + breakpoint.Name);
            }
        suspended = active.ToArray();
        return true;
    }

    private static void RearmSuspendedBreakpoints(IntPtr process, RemoteReadbackSession session)
    {
        if (session == null || session.SuspendedBreakpoints == null) return;
        foreach (BreakpointInfo breakpoint in session.SuspendedBreakpoints)
        {
            if (breakpoint == null || breakpoint == session.ResumeBreakpoint) continue;
            if (!WriteByte(process, breakpoint.Address, 0xCC))
                throw new InvalidOperationException("Could not re-arm suspended boundary probe breakpoint " + breakpoint.Name);
            breakpoint.Armed = true;
            Log("BOUNDARY_PROBE_BREAKPOINT_REARMED " + breakpoint.Name);
        }
        if (session.ResumeBreakpoint != null) session.ResumeBreakpoint.Armed = true;
    }

    private static RemoteReadbackSession StartRemoteSurfaceProbe(IntPtr process, uint threadId,
        uint ngiBase, BreakpointInfo resumeBreakpoint, FrameSnapshot frame,
        BreakpointInfo[] allBreakpoints)
    {
        uint device;
        uint getRenderTarget;
        string evidence;
        if (!VerifyD3d7GetRenderTarget(process, ngiBase, out device, out getRenderTarget, out evidence))
            throw new InvalidOperationException("D3D7 surface probe refused: " + evidence);
        uint renderer = ReadU32Exact(process, unchecked(ngiBase + NgiRendererGlobalRva));
        if (frame == null || frame.Projection == null || !frame.Projection.Verified
            || frame.Projection.Renderer != renderer
            || ReadU32Exact(process, renderer) != unchecked(ngiBase + NgiRendererVtableRva))
            throw new InvalidOperationException("Surface probe renderer did not match the verified active projection snapshot.");
        Log("BOUNDARY_PROBE_GET_RENDER_TARGET_ABI " + evidence + " renderer=0x" + renderer.ToString("X8")
            + " beginScene=" + (ReadU32Exact(process, unchecked(renderer + 0x118)) >> 8 & 0xFF));

        IntPtr thread = OpenThread(ThreadGetContext | ThreadSetContext | ThreadQueryInformation, false, threadId);
        if (thread == IntPtr.Zero) throw new InvalidOperationException("OpenThread for D3D7 boundary probe failed: " + Marshal.GetLastWin32Error());
        uint region = 0;
        try
        {
            byte[] originalContext = GetFullX86Context(thread);
            if (resumeBreakpoint == null || BitConverter.ToUInt32(originalContext, ContextEipOffset)
                != unchecked(resumeBreakpoint.Address + 1))
                throw new InvalidOperationException("Boundary probe EIP did not match the verified breakpoint.");
            uint safeEsp = ComputeSafeNativeStackPointer(process, thread, originalContext);
            region = AllocateReadbackRegion(process);
            RemoteCode code = BuildSurfaceProbeStub(region, device);
            WriteRemoteStub(process, code, true);
            BreakpointInfo[] suspended;
            if (!SuspendBreakpointsWhileStopped(process, allBreakpoints, out suspended))
                throw new InvalidOperationException("Could not suspend every armed breakpoint before the read-only surface query.");
            RemoteReadbackSession session = new RemoteReadbackSession();
            session.Acquire = code;
            session.Region = region;
            session.ThreadId = threadId;
            session.SafeEsp = safeEsp;
            session.OriginalContext = originalContext;
            session.RemoteContextIntact = true;
            session.PresentBreakpoint = resumeBreakpoint;
            session.ResumeBreakpoint = resumeBreakpoint;
            session.SuspendedBreakpoints = suspended;
            session.SurfaceProbeOnly = true;
            session.Frame = frame;
            session.Generation = frame.CameraGeneration;
            session.DeadlineUtc = DateTime.UtcNow.AddSeconds(5);
            session.Phase = RemoteReadbackPhase.Acquire;
            SetFullX86Context(thread, ContextForRemoteCode(originalContext, code.Base, safeEsp));
            Log("BOUNDARY_PROBE_STARTED kind=" + resumeBreakpoint.Name + " tid=" + threadId
                + " device=0x" + device.ToString("X8") + " renderer=0x" + renderer.ToString("X8")
                + " generation=" + session.Generation + " stub=0x" + code.Base.ToString("X8")
                + " lockCalled=false");
            return session;
        }
        catch
        {
            if (region != 0) VirtualFreeEx(process, Ptr(region), UIntPtr.Zero, 0x8000);
            throw;
        }
        finally { CloseHandle(thread); }
    }

    private static void BeginRemoteCleanup(IntPtr process, RemoteReadbackSession session)
    {
        session.Cleanup = BuildReadbackStub(session.Region, 0, true);
        WriteRemoteStub(process, session.Cleanup, false);
        IntPtr thread = OpenThread(ThreadGetContext | ThreadSetContext, false, session.ThreadId);
        if (thread == IntPtr.Zero) throw new InvalidOperationException("OpenThread for D3D7 Unlock/Release failed: " + Marshal.GetLastWin32Error());
        try
        {
            SetFullX86Context(thread, ContextForRemoteCode(session.OriginalContext, session.Cleanup.Base, session.SafeEsp));
        }
        finally { CloseHandle(thread); }
        session.Phase = RemoteReadbackPhase.Cleanup;
        session.DeadlineUtc = DateTime.UtcNow.AddSeconds(5);
        Log("REMOTE_CLEANUP_STARTED tid=" + session.ThreadId + " stub=0x" + session.Cleanup.Base.ToString("X8")
            + " terminal=0x" + session.Cleanup.TrapAddress.ToString("X8"));
    }

    private static uint AllocateReadbackRegion(IntPtr process)
    {
        const uint MemReserve = 0x2000;
        IntPtr memory = VirtualAllocEx(process, IntPtr.Zero, new UIntPtr(RemoteAllocationBytes),
            MemReserve | MemCommit, 0x04);
        if (memory == IntPtr.Zero) throw new InvalidOperationException("VirtualAllocEx for bounded readback stub failed: " + Marshal.GetLastWin32Error());
        uint address = unchecked((uint)memory.ToInt32());
        if (address == 0 || address + RemoteAllocationBytes < address)
        {
            VirtualFreeEx(process, memory, UIntPtr.Zero, 0x8000);
            throw new InvalidOperationException("VirtualAllocEx returned an invalid x86 address.");
        }
        // Keep the executable stub on its own read/execute page. The result
        // structure stays writable; COM uses the stopped render thread's
        // native stack after a TEB stack-bound check.
        uint oldProtection;
        if (!VirtualProtectEx(process, memory, new UIntPtr(RemoteCodePage), 0x20, out oldProtection))
        {
            VirtualFreeEx(process, memory, UIntPtr.Zero, 0x8000);
            throw new InvalidOperationException("VirtualProtectEx for readback code failed: " + Marshal.GetLastWin32Error());
        }
        return address;
    }

    private static void WriteRemoteStub(IntPtr process, RemoteCode code, bool initializeData)
    {
        uint oldProtection;
        if (!VirtualProtectEx(process, Ptr(code.Base), new UIntPtr(RemoteCodePage), PageExecuteReadWrite, out oldProtection))
            throw new InvalidOperationException("Could not make temporary readback stub writable: " + Marshal.GetLastWin32Error());
        try
        {
            UIntPtr written;
            if (!WriteProcessMemory(process, Ptr(code.Base), code.Bytes,
                new UIntPtr((uint)code.Bytes.Length), out written) || written.ToUInt32() != (uint)code.Bytes.Length)
                throw new InvalidOperationException("Writing temporary readback stub failed: " + Marshal.GetLastWin32Error());
            if (initializeData)
            {
                byte[] descriptor = new byte[DdSurfaceDesc2Size];
                Buffer.BlockCopy(BitConverter.GetBytes((uint)DdSurfaceDesc2Size), 0, descriptor, 0, 4);
                UIntPtr descWritten;
                if (!WriteProcessMemory(process, Ptr(unchecked(code.DataBase + RemoteDataDescOffset)), descriptor,
                    new UIntPtr((uint)descriptor.Length), out descWritten) || descWritten.ToUInt32() != (uint)descriptor.Length)
                    throw new InvalidOperationException("Initializing DDSURFACEDESC2 failed: " + Marshal.GetLastWin32Error());
                byte[] zeros = new byte[RemoteDataDescOffset];
                UIntPtr zerosWritten;
                if (!WriteProcessMemory(process, Ptr(code.DataBase), zeros,
                    new UIntPtr((uint)zeros.Length), out zerosWritten) || zerosWritten.ToUInt32() != (uint)zeros.Length)
                    throw new InvalidOperationException("Initializing readback result block failed: " + Marshal.GetLastWin32Error());
            }
        }
        finally
        {
            uint ignored;
            if (!VirtualProtectEx(process, Ptr(code.Base), new UIntPtr(RemoteCodePage), oldProtection, out ignored))
                throw new InvalidOperationException("Restoring temporary readback code protection failed: " + Marshal.GetLastWin32Error());
            if (!FlushInstructionCache(process, Ptr(code.Base), new UIntPtr((uint)code.Bytes.Length)))
                throw new InvalidOperationException("FlushInstructionCache for readback stub failed: " + Marshal.GetLastWin32Error());
        }
    }

    private static bool ReadExact(IntPtr process, uint address, byte[] bytes)
    {
        UIntPtr read;
        return address != 0 && ReadProcessMemory(process, Ptr(address), bytes,
            new UIntPtr((uint)bytes.Length), out read) && read.ToUInt32() == (uint)bytes.Length;
    }

    private static bool VerifyPresentStackArguments(IntPtr process, RemoteReadbackSession session, string stage)
    {
        uint esp = BitConverter.ToUInt32(session.OriginalContext, ContextEspOffset);
        byte[] current = new byte[PresentCallArgumentsBytes];
        bool readable = esp <= UInt32.MaxValue - PresentCallArgumentsBytes
            && ReadExact(process, esp, current);
        bool unchanged = readable && ByteArraysEqual(session.PresentStackArguments, current);
        Log("PRESENT_STACK_ARGS stage=" + stage + " esp=0x" + esp.ToString("X8")
            + " readable=" + readable + " unchanged=" + unchanged
            + " before=" + (session.PresentStackArguments == null ? "null" : BitConverter.ToString(session.PresentStackArguments))
            + " after=" + (readable ? BitConverter.ToString(current) : "unreadable"));
        return unchanged;
    }

    private static bool VerifyRemoteStubContext(IntPtr process, RemoteReadbackSession session,
        uint expectedEip, string stage)
    {
        IntPtr thread = OpenThread(ThreadGetContext, false, session.ThreadId);
        if (thread == IntPtr.Zero)
        {
            Log("REMOTE_STUB_CONTEXT stage=" + stage + " OpenThreadFailed=" + Marshal.GetLastWin32Error());
            return false;
        }
        try
        {
            byte[] context = GetFullX86Context(thread);
            uint eip = BitConverter.ToUInt32(context, ContextEipOffset);
            uint esp = BitConverter.ToUInt32(context, ContextEspOffset);
            uint expectedEsp = BitConverter.ToUInt32(session.OriginalContext, ContextEspOffset);
            bool matches = eip == expectedEip && esp == expectedEsp;
            Log("REMOTE_STUB_CONTEXT stage=" + stage + " expectedEip=0x" + expectedEip.ToString("X8")
                + " actualEip=0x" + eip.ToString("X8") + " expectedEsp=0x" + expectedEsp.ToString("X8")
                + " actualEsp=0x" + esp.ToString("X8") + " matches=" + matches);
            return matches;
        }
        catch (Exception error)
        {
            Log("REMOTE_STUB_CONTEXT stage=" + stage + " readFailed=" + error.Message);
            return false;
        }
        finally { CloseHandle(thread); }
    }

    private static bool ByteArraysEqual(byte[] left, byte[] right)
    {
        if (left == null || right == null || left.Length != right.Length) return false;
        for (int i = 0; i < left.Length; i++)
            if (left[i] != right[i]) return false;
        return true;
    }

    private static uint ReadU32Exact(IntPtr process, uint address)
    {
        byte[] bytes = new byte[4];
        if (!ReadExact(process, address, bytes)) throw new InvalidOperationException("Unreadable remote readback result at 0x" + address.ToString("X8"));
        return BitConverter.ToUInt32(bytes, 0);
    }

    private static ProjectionReadback ReadProjection(IntPtr process, uint ngiBase)
    {
        ProjectionReadback result = new ProjectionReadback();
        uint rendererPointer = ReadU32(process, unchecked(ngiBase + NgiRendererGlobalRva));
        result.Renderer = rendererPointer;
        if (rendererPointer == 0)
        {
            result.Failure = "Ngi32 renderer global is null";
            return result;
        }
        uint vtable = ReadU32(process, rendererPointer);
        result.RendererVtable = vtable;
        if (vtable != unchecked(ngiBase + NgiRendererVtableRva))
        {
            result.Failure = "Ngi32 renderer vtable mismatch: 0x" + vtable.ToString("X8");
            return result;
        }
        byte[] viewportBytes = new byte[16];
        byte[] nearBytes = new byte[4];
        byte[] farBytes = new byte[4];
        byte[] fovBytes = new byte[4];
        byte[] modeBytes = new byte[1];
        if (!ReadExact(process, unchecked(rendererPointer + 0x18), viewportBytes)
            || !ReadExact(process, unchecked(rendererPointer + 0x38), nearBytes)
            || !ReadExact(process, unchecked(rendererPointer + 0x3C), farBytes)
            || !ReadExact(process, unchecked(rendererPointer + 0x54), fovBytes)
            || !ReadExact(process, unchecked(rendererPointer + 0x118), modeBytes))
        {
            result.Failure = "Ngi32 projection fields are unreadable";
            return result;
        }
        int[] viewport = new int[4];
        for (int i = 0; i < viewport.Length; i++) viewport[i] = BitConverter.ToInt32(viewportBytes, i * 4);
        float nearPlane = BitConverter.ToSingle(nearBytes, 0);
        float farPlane = BitConverter.ToSingle(farBytes, 0);
        float fov = BitConverter.ToSingle(fovBytes, 0);
        result.Viewport = viewport;
        result.Near = nearPlane;
        result.Far = farPlane;
        result.Fov = fov;
        result.Mode = modeBytes[0];
        int width = viewport[2] - viewport[0];
        int height = viewport[3] - viewport[1];
        result.Verified = width > 0 && height > 0 && width <= 8192 && height <= 8192
            && IsFinite(nearPlane) && nearPlane > 0.0f
            && IsFinite(farPlane) && farPlane > nearPlane
            && IsFinite(fov) && fov > 0.0f && fov < 3.141593f;
        if (!result.Verified) result.Failure = "Ngi32 viewport or projection values failed bounded finite/range checks";
        return result;
    }

    private static SurfaceReadback ReadSurfaceRows(IntPtr process, uint descAddress)
    {
        byte[] desc = new byte[DdSurfaceDesc2Size];
        if (!ReadExact(process, descAddress, desc)) throw new InvalidOperationException("Could not read D3D7 surface descriptor.");
        Log("D3D7_LOCK_DDSURFACEDESC2 bytes=" + desc.Length + " raw=" + BitConverter.ToString(desc));
        uint size = BitConverter.ToUInt32(desc, 0);
        uint flags = BitConverter.ToUInt32(desc, 4);
        uint height = BitConverter.ToUInt32(desc, 8);
        uint width = BitConverter.ToUInt32(desc, 12);
        int pitch = BitConverter.ToInt32(desc, 16);
        uint surface = BitConverter.ToUInt32(desc, 36);
        uint pfSize = BitConverter.ToUInt32(desc, 72);
        uint pfFlags = BitConverter.ToUInt32(desc, 76);
        uint fourCc = BitConverter.ToUInt32(desc, 80);
        uint bitCount = BitConverter.ToUInt32(desc, 84);
        uint red = BitConverter.ToUInt32(desc, 88);
        uint green = BitConverter.ToUInt32(desc, 92);
        uint blue = BitConverter.ToUInt32(desc, 96);
        uint alpha = BitConverter.ToUInt32(desc, 100);
        if (size != DdSurfaceDesc2Size || pfSize != 32)
            throw new InvalidOperationException("DDSURFACEDESC2 or DDPIXELFORMAT size was unexpected.");
        if (!HasLockedSurfaceDescriptor(flags, surface))
            throw new InvalidOperationException("D3D7 Lock descriptor flags were incomplete: size=" + size
                + " flags=0x" + flags.ToString("X8") + " width=" + width + " height=" + height
                + " pitch=" + pitch + " surface=0x" + surface.ToString("X8")
                + " pfSize=" + pfSize + " pfFlags=0x" + pfFlags.ToString("X8")
                + " bitCount=" + bitCount + " masks=0x" + red.ToString("X8") + "/0x"
                + green.ToString("X8") + "/0x" + blue.ToString("X8") + "/0x" + alpha.ToString("X8"));
        if ((flags & DdsdLpSurface) == 0)
            Log("D3D7_LOCK_LPSURFACE_FLAG_ABSENT accepted_after_S_OK_and_nonzero_pointer; exact bounded ReadProcessMemory remains required");
        if ((pfFlags & DdpfRgb) == 0 || (pfFlags & DdpfFourCc) != 0)
            throw new InvalidOperationException("D3D7 render target is not an uncompressed RGB surface.");
        if (bitCount != 16 && bitCount != 24 && bitCount != 32)
            throw new InvalidOperationException("Unsupported D3D7 render-target bit depth: " + bitCount);
        if (width == 0 || height == 0 || width > 8192 || height > 8192 || surface == 0)
            throw new InvalidOperationException("D3D7 surface dimensions or pointer are outside the accepted bounds.");
        long rowBytes = ((long)width * bitCount + 7) / 8;
        long pitchAbs = Math.Abs((long)pitch);
        long totalBytes = pitchAbs * height;
        if (pitch == 0 || pitchAbs < rowBytes || totalBytes <= 0 || totalBytes > PixelReadbackLimit)
            throw new InvalidOperationException("D3D7 pitch/byte count is outside the bounded readback limits.");
        long lowest = pitch >= 0 ? surface : (long)surface + (long)(height - 1) * pitch;
        if (lowest <= 0 || lowest + totalBytes > UInt32.MaxValue)
            throw new InvalidOperationException("D3D7 surface address range overflowed x86 address space.");
        byte[] rows = new byte[(int)totalBytes];
        if (!ReadExact(process, unchecked((uint)lowest), rows))
            throw new InvalidOperationException("Could not copy the bounded locked D3D7 surface rows.");
        SurfaceReadback result = new SurfaceReadback();
        result.Width = width;
        result.Height = height;
        result.Pitch = pitch;
        result.BitCount = bitCount;
        result.RedMask = red;
        result.GreenMask = green;
        result.BlueMask = blue;
        result.AlphaMask = alpha;
        result.SurfacePointer = surface;
        result.Rows = rows;
        result.RgbMasksVerified = IsValidChannelMask(red, bitCount, true)
            && IsValidChannelMask(green, bitCount, true)
            && IsValidChannelMask(blue, bitCount, true)
            && IsValidChannelMask(alpha, bitCount, false)
            && (red & green) == 0 && (red & blue) == 0 && (green & blue) == 0
            && (alpha == 0 || ((alpha & red) == 0 && (alpha & green) == 0 && (alpha & blue) == 0));
        if (!result.RgbMasksVerified)
            throw new InvalidOperationException("D3D7 RGB channel masks are missing or overlap.");
        using (SHA256 sha = SHA256.Create())
        {
            byte[] hash = sha.ComputeHash(rows);
            StringBuilder hex = new StringBuilder(hash.Length * 2);
            for (int i = 0; i < hash.Length; i++) hex.Append(hash[i].ToString("X2"));
            result.Sha256 = hex.ToString();
        }
        return result;
    }

    private static bool HasLockedSurfaceDescriptor(uint flags, uint surface)
    {
        const uint required = DdsdHeight | DdsdWidth | DdsdPitch | DdsdPixelFormat;
        // Successful IDirectDrawSurface7::Lock returns the lpSurface address;
        // tolerate drivers that omit only DDSD_LPSURFACE, then require exact bounded
        // ReadProcessMemory for every pixel row before accepting the capture.
        return surface != 0 && (flags & required) == required;
    }

    private static int MaskChannel(uint pixel, uint mask, int fallback)
    {
        if (mask == 0) return fallback;
        int shift = 0;
        while (((mask >> shift) & 1) == 0 && shift < 31) shift++;
        ulong maximum = mask >> shift;
        ulong value = (pixel & mask) >> shift;
        return (int)((value * 255UL + maximum / 2UL) / maximum);
    }

    private static bool IsValidChannelMask(uint mask, uint bitCount, bool required)
    {
        if (mask == 0) return !required;
        if (bitCount < 32 && (mask >> (int)bitCount) != 0) return false;
        int shift = 0;
        while (shift < 32 && ((mask >> shift) & 1) == 0) shift++;
        if (shift >= 32) return false;
        uint normalized = mask >> shift;
        return (normalized & unchecked(normalized + 1u)) == 0;
    }

    private static byte[] DecodeSurfaceToBgra(SurfaceReadback surface)
    {
        if (surface == null || surface.Rows == null || !surface.RgbMasksVerified)
            throw new InvalidOperationException("Surface was not verified before pixel conversion.");
        int width = checked((int)surface.Width);
        int height = checked((int)surface.Height);
        int bytesPerPixel = checked((int)(surface.BitCount / 8));
        int sourcePitch = checked((int)Math.Abs((long)surface.Pitch));
        int rowBytes = checked(width * bytesPerPixel);
        if (width <= 0 || height <= 0 || (surface.BitCount != 16 && surface.BitCount != 24 && surface.BitCount != 32)
            || sourcePitch < rowBytes || surface.Rows.Length < checked(sourcePitch * height))
            throw new InvalidOperationException("Surface pixel buffer shape is inconsistent.");
        byte[] bgra = new byte[checked(width * height * 4)];
        for (int y = 0; y < height; y++)
        {
            int sourceRow = surface.Pitch >= 0 ? y : (height - 1 - y);
            int sourceBase = checked(sourceRow * sourcePitch);
            int destinationBase = checked(y * width * 4);
            for (int x = 0; x < width; x++)
            {
                int offset = sourceBase + x * bytesPerPixel;
                uint packed;
                if (surface.BitCount == 16) packed = BitConverter.ToUInt16(surface.Rows, offset);
                else if (surface.BitCount == 24)
                    packed = (uint)(surface.Rows[offset] | (surface.Rows[offset + 1] << 8) | (surface.Rows[offset + 2] << 16));
                else packed = BitConverter.ToUInt32(surface.Rows, offset);
                int outOffset = destinationBase + x * 4;
                bgra[outOffset] = (byte)MaskChannel(packed, surface.BlueMask, 0);
                bgra[outOffset + 1] = (byte)MaskChannel(packed, surface.GreenMask, 0);
                bgra[outOffset + 2] = (byte)MaskChannel(packed, surface.RedMask, 0);
                // PNG is a screenshot of the composited framebuffer. The
                // render target's alpha bits are not window opacity metadata.
                bgra[outOffset + 3] = 255;
            }
        }
        return bgra;
    }

    private static void AssertPixel(byte[] actual, params byte[] expected)
    {
        if (actual == null || actual.Length != expected.Length)
            throw new InvalidOperationException("Pixel converter self-check returned the wrong byte count.");
        for (int i = 0; i < expected.Length; i++)
            if (actual[i] != expected[i])
                throw new InvalidOperationException("Pixel converter self-check failed at byte " + i + ".");
    }

    private static void SaveSurfacePng(SurfaceReadback surface, string outputPath)
    {
        int width = checked((int)surface.Width);
        int height = checked((int)surface.Height);
        bool created = false;
        try
        {
            using (FileStream png = new FileStream(outputPath, FileMode.CreateNew, FileAccess.Write, FileShare.None))
            {
                created = true;
                using (Bitmap bitmap = new Bitmap(width, height, PixelFormat.Format32bppArgb))
                {
                    Rectangle rectangle = new Rectangle(0, 0, width, height);
                    BitmapData bits = bitmap.LockBits(rectangle, ImageLockMode.WriteOnly, PixelFormat.Format32bppArgb);
                    try
                    {
                        if (bits.Stride < width * 4) throw new InvalidOperationException("PNG staging bitmap stride is too short.");
                        byte[] sourceBgra = DecodeSurfaceToBgra(surface);
                        byte[] rgba = new byte[checked(bits.Stride * height)];
                        int sourceStride = width * 4;
                        for (int y = 0; y < height; y++)
                            Buffer.BlockCopy(sourceBgra, y * sourceStride, rgba, y * bits.Stride, sourceStride);
                        Marshal.Copy(rgba, 0, bits.Scan0, rgba.Length);
                    }
                    finally { bitmap.UnlockBits(bits); }
                    bitmap.Save(png, ImageFormat.Png);
                }
                png.Flush();
            }
        }
        catch
        {
            if (created) try { File.Delete(outputPath); } catch { }
            throw;
        }
    }

    private static void WriteTextCreateNew(string outputPath, string text)
    {
        bool created = false;
        try
        {
            using (FileStream output = new FileStream(outputPath, FileMode.CreateNew, FileAccess.Write, FileShare.None))
            {
                created = true;
                byte[] bytes = new UTF8Encoding(false).GetBytes(text);
                output.Write(bytes, 0, bytes.Length);
                output.Flush();
            }
        }
        catch
        {
            if (created) try { File.Delete(outputPath); } catch { }
            throw;
        }
    }

    private static CameraReadback ReadCameraSnapshot(IntPtr process, uint threadId, uint terrainBase)
    {
        CameraReadback result = new CameraReadback();
        IntPtr thread = OpenThread(ThreadGetContext | ThreadSetContext, false, threadId);
        IntPtr contextAllocation;
        IntPtr context = AllocateAlignedX86Context(out contextAllocation);
        try
        {
            Marshal.Copy(new byte[X86ContextSize], 0, context, X86ContextSize);
            Marshal.WriteInt32(context, 0, unchecked((int)ContextAllX86));
            if (thread == IntPtr.Zero || !GetThreadContext(thread, context))
            {
                result.Failure = "World3D breakpoint context was unreadable: " + Marshal.GetLastWin32Error();
                return result;
            }
            uint esp = unchecked((uint)Marshal.ReadInt32(context, ContextEspOffset));
            uint returnAddress = ReadU32(process, esp);
            uint camera = ReadU32(process, unchecked(esp + 4));
            uint vtable = ReadU32(process, camera);
            uint parent = ReadCameraParent(process, camera);
            uint transformInterface = unchecked(camera + 4);
            uint transformVtable = ReadU32(process, transformInterface);
            uint selectorField = ReadU32(process, unchecked(camera + 0x10));
            byte[] modeBytes = new byte[1];
            byte cameraMode = ReadExact(process, unchecked(camera + 0x1A0), modeBytes) ? modeBytes[0] : (byte)0xFF;
            byte[] matrixBytes = new byte[64];
            byte[] effectiveWorldMatrixBytes = new byte[64];
            if (camera == 0 || !ReadExact(process, unchecked(camera + 0x20), matrixBytes)
                || !ReadExact(process, unchecked(camera + 0x60), effectiveWorldMatrixBytes))
            {
                result.Failure = "World3D camera argument, selector-0 matrix, or effective-world matrix was unreadable";
                return result;
            }
            uint[] words = new uint[16];
            bool matrixFinite = true;
            for (int i = 0; i < words.Length; i++)
            {
                words[i] = BitConverter.ToUInt32(matrixBytes, i * 4);
                if (!IsFinite(BitConverter.ToSingle(matrixBytes, i * 4))) matrixFinite = false;
            }
            uint[] effectiveWorldWords = MatrixWords(effectiveWorldMatrixBytes);
            bool effectiveWorldMatrixFinite = true;
            for (int i = 0; i < effectiveWorldWords.Length; i++)
                if (!IsFinite(BitConverter.ToSingle(effectiveWorldMatrixBytes, i * 4)))
                    effectiveWorldMatrixFinite = false;
            string worldMatrixFailure = null;
            bool effectiveWorldMatrixRigid = effectiveWorldMatrixFinite
                && IsOrthonormalAffineCamera(effectiveWorldMatrixBytes, out worldMatrixFailure);
            if (!effectiveWorldMatrixFinite) worldMatrixFailure = "effective-world matrix contains a nonfinite value";
            uint expectedVtable = unchecked(terrainBase + ExternalCameraVtableRva);
            uint expectedTransformVtable = unchecked(terrainBase + 0x66558);
            uint setterSlot = ReadU32(process, unchecked(transformVtable + 0x1C));
            uint invalidateSlot = ReadU32(process, unchecked(transformVtable + 0x30));
            bool setterAbiVerified = transformVtable == expectedTransformVtable
                && setterSlot == unchecked(terrainBase + 0x54C70)
                && invalidateSlot == unchecked(terrainBase + 0x55280);
            result.Camera = camera;
            result.Parent = parent;
            result.Parented = parent != 0;
            result.Vtable = vtable;
            result.TransformInterface = transformInterface;
            result.TransformVtable = transformVtable;
            result.CameraMode = cameraMode;
            result.EntryEsp = esp;
            result.ReturnAddress = returnAddress;
            result.SelectorField = selectorField;
            result.Words = words;
            result.EffectiveWorldWords = effectiveWorldWords;
            result.EntryMatrixBytes = (byte[])matrixBytes.Clone();
            result.EntryEffectiveWorldMatrixBytes = (byte[])effectiveWorldMatrixBytes.Clone();
            result.EntryMatrixSha256 = HashBytes(matrixBytes);
            result.EntryEffectiveWorldMatrixSha256 = HashBytes(effectiveWorldMatrixBytes);
            result.MatrixFinite = matrixFinite;
            result.EffectiveWorldMatrixFinite = effectiveWorldMatrixRigid;
            result.LayoutVerified = terrainBase != 0 && vtable == expectedVtable
                && setterAbiVerified && cameraMode == 0 && effectiveWorldMatrixRigid
                && selectorField == 0xFFFFFFFF && matrixFinite;
            if (!result.LayoutVerified)
                result.Failure = "external camera primary/transform vtables, setter slots, normal mode, selector field, local matrix, or effective-world matrix check failed"
                    + (effectiveWorldMatrixRigid ? "" : ": " + worldMatrixFailure);
            return result;
        }
        finally
        {
            Marshal.FreeHGlobal(contextAllocation);
            if (thread != IntPtr.Zero) CloseHandle(thread);
        }
    }

    private static Dictionary<string, object> ParseJsonObject(string json)
    {
        if (String.IsNullOrEmpty(json) || json.Length > CameraInputJsonLimit)
            throw new InvalidDataException("Camera JSON is empty or exceeds the 1 MiB input limit.");
        JavaScriptSerializer serializer = new JavaScriptSerializer();
        serializer.MaxJsonLength = CameraInputJsonLimit;
        serializer.RecursionLimit = 32;
        object value;
        try { value = serializer.DeserializeObject(json); }
        catch (Exception error) { throw new InvalidDataException("Camera JSON is malformed.", error); }
        Dictionary<string, object> result = value as Dictionary<string, object>;
        if (result == null) throw new InvalidDataException("Camera JSON must be a top-level object.");
        return result;
    }

    private static object RequiredJsonField(Dictionary<string, object> json, string key)
    {
        object value;
        if (json == null || !json.TryGetValue(key, out value) || value == null)
            throw new InvalidDataException("Camera JSON is missing '" + key + "'.");
        return value;
    }

    private static string JsonStringField(Dictionary<string, object> json, string key)
    {
        string value = RequiredJsonField(json, key) as string;
        if (value == null) throw new InvalidDataException("Camera JSON field '" + key + "' must be a string.");
        return value;
    }

    private static object[] JsonArrayField(Dictionary<string, object> json, string key)
    {
        object value = RequiredJsonField(json, key);
        IList list = value as IList;
        if (list == null) throw new InvalidDataException("Camera JSON field '" + key + "' must be an array.");
        object[] result = new object[list.Count];
        list.CopyTo(result, 0);
        return result;
    }

    private static bool TryJsonDouble(object value, out double result)
    {
        if (value is double) result = (double)value;
        else if (value is decimal) result = (double)(decimal)value;
        else if (value is int) result = (int)value;
        else if (value is long) result = (long)value;
        else if (value is uint) result = (uint)value;
        else if (value is ulong) result = (ulong)value;
        else if (value is float) result = (float)value;
        else { result = 0; return false; }
        return !Double.IsNaN(result) && !Double.IsInfinity(result);
    }

    private static bool TryJsonUInt32(object value, out uint result)
    {
        double number;
        if (!TryJsonDouble(value, out number) || number < 0 || number > UInt32.MaxValue
            || Math.Truncate(number) != number)
        { result = 0; return false; }
        result = (uint)number;
        return true;
    }

    private static bool TryJsonInt32(object value, out int result)
    {
        double number;
        if (!TryJsonDouble(value, out number) || number < Int32.MinValue || number > Int32.MaxValue
            || Math.Truncate(number) != number)
        { result = 0; return false; }
        result = (int)number;
        return true;
    }

    private static bool TryJsonFloat(object value, out float result)
    {
        double number;
        if (!TryJsonDouble(value, out number) || number < -Single.MaxValue || number > Single.MaxValue)
        { result = 0; return false; }
        result = (float)number;
        return IsFinite(result);
    }

    private static bool IsOrthonormalAffineCamera(byte[] matrixBytes, out string failure)
    {
        failure = null;
        if (matrixBytes == null || matrixBytes.Length != 64)
        {
            failure = "camera matrix must contain exactly 64 bytes";
            return false;
        }
        float[] m = new float[16];
        for (int i = 0; i < m.Length; i++)
        {
            m[i] = BitConverter.ToSingle(matrixBytes, i * 4);
            if (!IsFinite(m[i])) { failure = "camera matrix contains a nonfinite value"; return false; }
        }
        const double affineTolerance = 0.0001;
        if (Math.Abs(m[12]) > affineTolerance || Math.Abs(m[13]) > affineTolerance
            || Math.Abs(m[14]) > affineTolerance || Math.Abs(m[15] - 1.0) > affineTolerance)
        {
            failure = "camera matrix last row is not affine [0,0,0,1]";
            return false;
        }
        if (Math.Abs(m[3]) > 1000000 || Math.Abs(m[7]) > 1000000 || Math.Abs(m[11]) > 1000000)
        {
            failure = "camera translation is outside the supported finite range";
            return false;
        }

        // Native selector-0 matrices are row-major affine transforms with a
        // rigid, orthonormal 3x3 basis. Reject shear/scale before the DLL call.
        double[] rowLengthSquared = new double[3];
        for (int row = 0; row < 3; row++)
        {
            int offset = row * 4;
            for (int column = 0; column < 3; column++)
                rowLengthSquared[row] += (double)m[offset + column] * m[offset + column];
            if (Math.Abs(rowLengthSquared[row] - 1.0) > 0.02)
            {
                failure = "camera basis row is not unit length";
                return false;
            }
        }
        for (int firstRow = 0; firstRow < 3; firstRow++)
            for (int secondRow = firstRow + 1; secondRow < 3; secondRow++)
            {
                double dot = 0;
                for (int column = 0; column < 3; column++)
                    dot += (double)m[firstRow * 4 + column] * m[secondRow * 4 + column];
                if (Math.Abs(dot) > 0.01)
                {
                    failure = "camera basis contains shear or nonorthogonal axes";
                    return false;
                }
            }
        double determinant = (double)m[0] * (m[5] * m[10] - m[6] * m[9])
            - (double)m[1] * (m[4] * m[10] - m[6] * m[8])
            + (double)m[2] * (m[4] * m[9] - m[5] * m[8]);
        if (Math.Abs(Math.Abs(determinant) - 1.0) > 0.04)
        {
            failure = "camera basis is singular or not approximately rigid";
            return false;
        }
        return true;
    }

    private static SelectedCameraInput ParseSelectedCameraJson(string json, string sourcePath)
    {
        Dictionary<string, object> root = ParseJsonObject(json);
        object renderInputUsableValue;
        bool renderInputUsable = root.TryGetValue("render_input_usable", out renderInputUsableValue)
            && renderInputUsableValue is bool && (bool)renderInputUsableValue;
        object syntheticCameraValue;
        bool syntheticCamera = root.TryGetValue("synthetic_camera", out syntheticCameraValue)
            && syntheticCameraValue is bool && (bool)syntheticCameraValue;
        bool nativeCapture = String.Equals(JsonStringField(root, "capture_status"),
            "native-frame-captured", StringComparison.Ordinal) && renderInputUsable;
        if (!String.Equals(JsonStringField(root, "schema"), "fparkan-legacy-camera-v1", StringComparison.Ordinal)
            || (!nativeCapture && !syntheticCamera))
            throw new InvalidDataException("Camera input must be a usable native capture or an explicitly marked synthetic camera with a verified projection tuple.");

        SelectedCameraInput input = new SelectedCameraInput();
        input.Path = Path.GetFullPath(sourcePath);
        input.Synthetic = syntheticCamera;
        object parentedValue;
        if (root.TryGetValue("parented_camera", out parentedValue))
        {
            if (!(parentedValue is bool))
                throw new InvalidDataException("parented_camera must be a boolean when present.");
            input.ParentedCamera = (bool)parentedValue;
        }

        object[] localWords = JsonArrayField(root, "selector0_words");
        if (localWords.Length != 16) throw new InvalidDataException("selector0_words must contain exactly 16 uint32 values.");
        input.Selector0MatrixWords = new uint[16];
        input.Selector0MatrixBytes = new byte[64];
        for (int i = 0; i < localWords.Length; i++)
        {
            uint word;
            if (!TryJsonUInt32(localWords[i], out word))
                throw new InvalidDataException("selector0_words contains a value outside uint32 range.");
            input.Selector0MatrixWords[i] = word;
            Buffer.BlockCopy(BitConverter.GetBytes(word), 0, input.Selector0MatrixBytes, i * 4, 4);
        }

        object worldSpaceValue;
        object worldWordsValue;
        bool hasWorldSpace = root.TryGetValue("effective_world_matrix_space", out worldSpaceValue);
        bool hasWorldWords = root.TryGetValue("effective_world_matrix_words", out worldWordsValue);
        if (hasWorldSpace != hasWorldWords)
            throw new InvalidDataException("effective_world_matrix_space and effective_world_matrix_words must be supplied together.");
        if (hasWorldSpace)
        {
            string worldSpace = worldSpaceValue as string;
            if (!String.Equals(worldSpace, "world", StringComparison.Ordinal))
                throw new InvalidDataException("effective_world_matrix_space must be 'world'.");
            IList worldWordList = worldWordsValue as IList;
            if (worldWordList == null || worldWordList.Count != 16)
                throw new InvalidDataException("effective_world_matrix_words must contain exactly 16 uint32 values.");
            input.MatrixWords = new uint[16];
            input.MatrixBytes = new byte[64];
            for (int i = 0; i < 16; i++)
            {
                uint word;
                if (!TryJsonUInt32(worldWordList[i], out word))
                    throw new InvalidDataException("effective_world_matrix_words contains a value outside uint32 range.");
                input.MatrixWords[i] = word;
                Buffer.BlockCopy(BitConverter.GetBytes(word), 0, input.MatrixBytes, i * 4, 4);
            }
            input.UsedEffectiveWorldMatrix = true;
        }
        else
        {
            if (input.ParentedCamera)
                throw new InvalidDataException("A parented camera requires its effective world matrix; selector0_words is local space.");
            input.MatrixWords = (uint[])input.Selector0MatrixWords.Clone();
            input.MatrixBytes = (byte[])input.Selector0MatrixBytes.Clone();
            input.UsedEffectiveWorldMatrix = false;
        }
        string matrixFailure;
        if (!IsOrthonormalAffineCamera(input.MatrixBytes, out matrixFailure))
            throw new InvalidDataException("Camera input rejected: " + matrixFailure + ".");
        input.MatrixSha256 = HashBytes(input.MatrixBytes);

        object[] viewport = JsonArrayField(root, "viewport");
        if (viewport.Length != 4) throw new InvalidDataException("viewport must contain exactly four integers.");
        input.Viewport = new int[4];
        for (int i = 0; i < viewport.Length; i++)
            if (!TryJsonInt32(viewport[i], out input.Viewport[i]))
                throw new InvalidDataException("viewport contains an invalid integer.");
        if (input.Viewport[0] < 0 || input.Viewport[1] < 0
            || input.Viewport[2] <= input.Viewport[0] || input.Viewport[3] <= input.Viewport[1])
            throw new InvalidDataException("viewport bounds must have positive width and height.");

        if (!TryJsonFloat(RequiredJsonField(root, "near_plane"), out input.Near)
            || !TryJsonFloat(RequiredJsonField(root, "far_plane"), out input.Far)
            || !TryJsonFloat(RequiredJsonField(root, "field_of_view_radians"), out input.FieldOfView)
            || !IsFinite(input.Near) || !IsFinite(input.Far) || !IsFinite(input.FieldOfView)
            || input.Near <= 0 || input.Far <= input.Near || input.FieldOfView <= 0.05f || input.FieldOfView >= 3.1f)
            throw new InvalidDataException("Camera projection must have finite, ordered near/far planes and a finite perspective FOV.");
        uint projectionMode;
        if (!TryJsonUInt32(RequiredJsonField(root, "projection_mode_byte"), out projectionMode)
            || projectionMode == 0 || projectionMode > Byte.MaxValue)
            throw new InvalidDataException("Camera input requires a verified perspective projection mode.");
        input.ProjectionMode = (byte)projectionMode;
        return input;
    }

    private static SelectedCameraInput LoadSelectedCameraInput(string path)
    {
        string fullPath = Path.GetFullPath(path);
        if (!File.Exists(fullPath)) throw new FileNotFoundException("Camera input JSON does not exist.", fullPath);
        FileInfo file = new FileInfo(fullPath);
        if (file.Length > CameraInputJsonLimit)
            throw new InvalidDataException("Camera input exceeds the 1 MiB limit.");
        byte[] bytes = File.ReadAllBytes(fullPath);
        if (bytes.Length > CameraInputJsonLimit)
            throw new InvalidDataException("Camera input exceeded the 1 MiB limit while being read.");
        int offset = bytes.Length >= 3 && bytes[0] == 0xEF && bytes[1] == 0xBB && bytes[2] == 0xBF ? 3 : 0;
        string text;
        try { text = new UTF8Encoding(false, true).GetString(bytes, offset, bytes.Length - offset); }
        catch (DecoderFallbackException error) { throw new InvalidDataException("Camera input is not valid UTF-8.", error); }
        return ParseSelectedCameraJson(text, fullPath);
    }

    private static bool ProjectionMatchesCameraInput(ProjectionReadback projection, SelectedCameraInput input)
    {
        if (projection == null || input == null || !projection.Verified || projection.Mode != input.ProjectionMode
            || projection.Viewport == null || projection.Viewport.Length != 4) return false;
        for (int i = 0; i < 4; i++) if (projection.Viewport[i] != input.Viewport[i]) return false;
        return NearlyEqual(projection.Near, input.Near, 0.0001f)
            && NearlyEqual(projection.Far, input.Far, 0.001f)
            && NearlyEqual(projection.Fov, input.FieldOfView, 0.0001f);
    }

    private static bool NearlyEqual(float left, float right, float tolerance)
    {
        return IsFinite(left) && IsFinite(right)
            && Math.Abs((double)left - right) <= tolerance * Math.Max(1.0, Math.Max(Math.Abs((double)left), Math.Abs((double)right)));
    }

    private static void SelfCheckCameraInput()
    {
        string valid = "{\"schema\":\"fparkan-legacy-camera-v1\",\"capture_status\":\"native-frame-captured\","
            + "\"render_input_usable\":true,\"selector0_words\":[1065353216,0,0,1065353216,"
            + "0,1065353216,0,1073741824,0,0,1065353216,1077936128,0,0,0,1065353216],"
            + "\"viewport\":[0,0,1280,1024],\"near_plane\":0.5,\"far_plane\":700,"
            + "\"field_of_view_radians\":1.04,\"projection_mode_byte\":1}";
        SelectedCameraInput parsed = ParseSelectedCameraJson(valid, "input.json");
        if (parsed.MatrixBytes.Length != 64 || parsed.Selector0MatrixBytes.Length != 64
            || parsed.Viewport[2] != 1280 || parsed.MatrixSha256.Length != 64
            || parsed.UsedEffectiveWorldMatrix || parsed.MatrixWords[3] != parsed.Selector0MatrixWords[3])
            throw new InvalidOperationException("Camera JSON parser self-check failed on a valid rigid transform.");
        string parentedWorld = valid.Replace("\"viewport\":",
            "\"parented_camera\":true,\"effective_world_matrix_space\":\"world\","
            + "\"effective_world_matrix_words\":[1065353216,0,0,1092616192,0,1065353216,0,1101004800,"
            + "0,0,1065353216,1106247680,0,0,0,1065353216],\"viewport\":");
        SelectedCameraInput parsedParentedWorld = ParseSelectedCameraJson(parentedWorld, "parented.json");
        if (!parsedParentedWorld.ParentedCamera || !parsedParentedWorld.UsedEffectiveWorldMatrix
            || parsedParentedWorld.MatrixWords[3] != 1092616192
            || parsedParentedWorld.Selector0MatrixWords[3] != 1065353216)
            throw new InvalidOperationException("Camera JSON parser did not prefer the effective world matrix while preserving selector-0 local words.");
        if (CameraMatricesNearlyEqual(parsedParentedWorld.MatrixBytes,
                parsedParentedWorld.Selector0MatrixBytes)
            || parsedParentedWorld.Selector0MatrixWords[3] != 1065353216)
            throw new InvalidOperationException("Camera setter self-check did not keep the original local matrix distinct for restore.");
        bool rejected;
        string parentedWithoutWorld = valid.Replace("\"viewport\":", "\"parented_camera\":true,\"viewport\":");
        rejected = false;
        try { ParseSelectedCameraJson(parentedWithoutWorld, "bad-parented.json"); }
        catch (InvalidDataException) { rejected = true; }
        if (!rejected) throw new InvalidOperationException("Camera JSON parser accepted a parented camera without an effective world matrix.");
        string wrongWorldSpace = parentedWorld.Replace("\"effective_world_matrix_space\":\"world\"",
            "\"effective_world_matrix_space\":\"local\"");
        rejected = false;
        try { ParseSelectedCameraJson(wrongWorldSpace, "bad-space.json"); }
        catch (InvalidDataException) { rejected = true; }
        if (!rejected) throw new InvalidOperationException("Camera JSON parser accepted a non-world effective matrix marker.");
        string synthetic = valid.Replace("\"capture_status\":\"native-frame-captured\",",
                "\"capture_status\":\"synthetic-camera-plan\",\"synthetic_camera\":true,")
            .Replace("\"render_input_usable\":true,", "");
        SelectedCameraInput parsedSynthetic = ParseSelectedCameraJson(synthetic, "synthetic-input.json");
        if (!parsedSynthetic.Synthetic || parsedSynthetic.ProjectionMode != 1)
            throw new InvalidOperationException("Camera JSON parser failed to preserve explicit synthetic-camera provenance.");
        string unmarkedSynthetic = synthetic.Replace("\"synthetic_camera\":true", "\"synthetic_camera\":false");
        rejected = false;
        try { ParseSelectedCameraJson(unmarkedSynthetic, "bad.json"); }
        catch (InvalidDataException) { rejected = true; }
        if (!rejected) throw new InvalidOperationException("Camera JSON parser accepted an unmarked synthetic camera plan.");
        string zeroMatrix = valid.Replace("1065353216,0,0,1065353216,0,1065353216,0,1073741824,0,0,1065353216,1077936128,0,0,0,1065353216",
            "0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0");
        rejected = false;
        try { ParseSelectedCameraJson(zeroMatrix, "bad.json"); }
        catch (InvalidDataException) { rejected = true; }
        if (!rejected) throw new InvalidOperationException("Camera JSON parser accepted a singular all-zero matrix.");
        string nonPerspective = valid.Replace("\"projection_mode_byte\":1", "\"projection_mode_byte\":0");
        rejected = false;
        try { ParseSelectedCameraJson(nonPerspective, "bad.json"); }
        catch (InvalidDataException) { rejected = true; }
        if (!rejected) throw new InvalidOperationException("Camera JSON parser accepted a non-perspective projection.");
        string nonNumericWord = valid.Replace("1065353216,0,0,1065353216", "\"1065353216\",0,0,1065353216");
        rejected = false;
        try { ParseSelectedCameraJson(nonNumericWord, "bad.json"); }
        catch (InvalidDataException) { rejected = true; }
        if (!rejected) throw new InvalidOperationException("Camera JSON parser accepted a quoted matrix word.");
    }

    private static bool RefreshCameraWordsAtProjection(IntPtr process, CameraReadback camera,
        uint terrainBase, uint generation)
    {
        if (camera == null || !camera.LayoutVerified || camera.Camera == 0
            || camera.EntryMatrixBytes == null || camera.EntryMatrixBytes.Length != 64)
        {
            if (camera != null)
            {
                camera.CurrentAtProjectionVerified = false;
                camera.Failure = "camera entry layout was not verified before the projection boundary";
            }
            return false;
        }

        uint vtable = ReadU32(process, camera.Camera);
        uint selectorField = ReadU32(process, unchecked(camera.Camera + 0x10));
        uint parent = ReadCameraParent(process, camera.Camera);
        uint expectedVtable = unchecked(terrainBase + ExternalCameraVtableRva);
        byte[] current = new byte[64];
        byte[] effectiveWorldCurrent = new byte[64];
        if (vtable != expectedVtable || selectorField != 0xFFFFFFFF || parent != camera.Parent
            || !ReadExact(process, unchecked(camera.Camera + 0x20), current)
            || !ReadExact(process, unchecked(camera.Camera + 0x60), effectiveWorldCurrent))
        {
            camera.CurrentAtProjectionVerified = false;
            camera.EffectiveWorldAtProjectionVerified = false;
            camera.Failure = "camera pointer, parent, vtable, selector, local matrix, or world matrix changed before projection";
            Log("CAMERA_MATRIX_AT_PROJECTION_REJECTED generation=" + generation
                + " vtable=0x" + vtable.ToString("X8") + " selector=0x" + selectorField.ToString("X8")
                + " parent=0x" + parent.ToString("X8") + " expectedParent=0x" + camera.Parent.ToString("X8")
                + " expectedVtable=0x" + expectedVtable.ToString("X8"));
            return false;
        }

        uint[] words = new uint[16];
        bool finite = true;
        for (int i = 0; i < words.Length; i++)
        {
            words[i] = BitConverter.ToUInt32(current, i * 4);
            if (!IsFinite(BitConverter.ToSingle(current, i * 4))) finite = false;
        }
        if (!finite)
        {
            camera.CurrentAtProjectionVerified = false;
            camera.EffectiveWorldAtProjectionVerified = false;
            camera.Failure = "camera matrix contains a non-finite value at projection boundary";
            Log("CAMERA_MATRIX_AT_PROJECTION_REJECTED generation=" + generation + " nonFinite=true");
            return false;
        }

        uint[] effectiveWorldWords = MatrixWords(effectiveWorldCurrent);
        bool effectiveWorldFinite = true;
        for (int i = 0; i < effectiveWorldWords.Length; i++)
            if (!IsFinite(BitConverter.ToSingle(effectiveWorldCurrent, i * 4)))
                effectiveWorldFinite = false;
        string worldFailure = null;
        bool effectiveWorldRigid = effectiveWorldFinite
            && IsOrthonormalAffineCamera(effectiveWorldCurrent, out worldFailure);
        if (!effectiveWorldFinite) worldFailure = "effective-world matrix contains a nonfinite value";
        if (!effectiveWorldRigid)
        {
            camera.CurrentAtProjectionVerified = false;
            camera.EffectiveWorldAtProjectionVerified = false;
            camera.Failure = "effective-world matrix failed validation at projection: " + worldFailure;
            Log("CAMERA_WORLD_MATRIX_AT_PROJECTION_REJECTED generation=" + generation + " reason=" + worldFailure);
            return false;
        }

        bool changed = !ByteArraysEqual(camera.EntryMatrixBytes, current);
        bool effectiveWorldChanged = !ByteArraysEqual(camera.EntryEffectiveWorldMatrixBytes, effectiveWorldCurrent);
        camera.Vtable = vtable;
        camera.SelectorField = selectorField;
        camera.Parent = parent;
        camera.Parented = parent != 0;
        camera.Words = words;
        camera.EffectiveWorldWords = effectiveWorldWords;
        camera.ProjectionMatrixBytes = (byte[])current.Clone();
        camera.ProjectionEffectiveWorldMatrixBytes = (byte[])effectiveWorldCurrent.Clone();
        camera.MatrixFinite = true;
        camera.EffectiveWorldMatrixFinite = true;
        camera.CurrentAtProjectionVerified = true;
        camera.EffectiveWorldAtProjectionVerified = true;
        camera.WordsChangedAtProjection = changed;
        camera.EffectiveWorldChangedAtProjection = effectiveWorldChanged;
        camera.ProjectionMatrixSha256 = HashBytes(current);
        camera.ProjectionEffectiveWorldMatrixSha256 = HashBytes(effectiveWorldCurrent);
        Log("CAMERA_MATRIX_AT_PROJECTION generation=" + generation + " verified=true changedSinceEntry=" + changed
            + " entrySha256=" + camera.EntryMatrixSha256 + " projectionSha256=" + camera.ProjectionMatrixSha256);
        Log("CAMERA_WORLD_MATRIX_AT_PROJECTION generation=" + generation + " verified=true"
            + " parent=0x" + parent.ToString("X8") + " changedSinceEntry=" + effectiveWorldChanged
            + " entrySha256=" + camera.EntryEffectiveWorldMatrixSha256
            + " projectionSha256=" + camera.ProjectionEffectiveWorldMatrixSha256);
        return true;
    }

    private static string HashBytes(byte[] bytes)
    {
        using (SHA256 sha = SHA256.Create())
        {
            byte[] hash = sha.ComputeHash(bytes);
            StringBuilder result = new StringBuilder(hash.Length * 2);
            for (int i = 0; i < hash.Length; i++) result.Append(hash[i].ToString("X2"));
            return result.ToString();
        }
    }

    private static int SelfCheckReadback()
    {
        if (IntPtr.Size != 4) throw new InvalidOperationException("This helper must run as x86.");
        uint flags = DdLockReadOnly | DdLockDoNotWait;
        if (DdLockReadOnly != 0x10 || DdLockDoNotWait != 0x4000 || flags != 0x4010 || flags == 0x30)
            throw new InvalidOperationException("DDLOCK flag self-check failed.");
        RemoteCode acquire = BuildReadbackStub(0x10000000, 0x20000000, false);
        RemoteCode cleanup = BuildReadbackStub(0x10000000, 0, true);
        RemoteCode surfaceProbe = BuildSurfaceProbeStub(0x18000000, 0x28000000);
        RemoteCode cameraSetterApply = BuildCameraSetterStub(0x30000000, 0x40000000, 2);
        RemoteCode cameraSetterRestore = BuildCameraSetterStub(0x31000000, 0x40000000, 1);
        int[] cameraApplyCalls = cameraSetterApply.CallInstructionOffsets;
        int[] cameraRestoreCalls = cameraSetterRestore.CallInstructionOffsets;
        int effectiveWorldGetterCall = cameraApplyCalls != null && cameraApplyCalls.Length > 1
            ? cameraApplyCalls[1] : -1;
        bool effectiveWorldGetterArgumentsMatch = effectiveWorldGetterCall >= 8
            && cameraSetterApply.Bytes[effectiveWorldGetterCall - 1] == 0x56
            && cameraSetterApply.Bytes[effectiveWorldGetterCall - 2] == 2
            && cameraSetterApply.Bytes[effectiveWorldGetterCall - 3] == 0x6A
            && cameraSetterApply.Bytes[effectiveWorldGetterCall - 8] == 0x68
            && BitConverter.ToUInt32(cameraSetterApply.Bytes, effectiveWorldGetterCall - 7)
                == 0x30000000 + RemoteDataOffset + CameraSetterWorldMatrixPointerOffset;
        string[] d3d7DeviceVtable = new string[] {
            "QueryInterface", "AddRef", "Release", "GetCaps", "EnumTextureFormats", "BeginScene", "EndScene",
            "GetDirect3D", "SetRenderTarget", "GetRenderTarget", "Clear", "SetTransform", "GetTransform",
            "SetViewport", "MultiplyTransform", "GetViewport", "SetMaterial", "GetMaterial", "SetLight", "GetLight"
        };
        string[] surface7Vtable = new string[] {
            "QueryInterface", "AddRef", "Release", "AddAttachedSurface", "AddOverlayDirtyRect", "Blt", "BltBatch",
            "BltFast", "DeleteAttachedSurface", "EnumAttachedSurfaces", "EnumOverlayZOrders", "Flip", "GetAttachedSurface",
            "GetBltStatus", "GetCaps", "GetClipper", "GetColorKey", "GetDC", "GetFlipStatus", "GetOverlayPosition",
            "GetPalette", "GetPixelFormat", "GetSurfaceDesc", "Initialize", "IsLost", "Lock", "ReleaseDC", "Restore",
            "SetClipper", "SetColorKey", "SetOverlayPosition", "SetPalette", "Unlock", "UpdateOverlay",
            "UpdateOverlayDisplay", "UpdateOverlayZOrder", "GetDDInterface", "PageLock", "PageUnlock", "SetSurfaceDesc",
            "SetPrivateData", "GetPrivateData", "FreePrivateData", "GetUniquenessValue", "ChangeUniquenessValue",
            "SetPriority", "GetPriority", "SetLOD", "GetLOD"
        };
        byte[] originalContext = new byte[X86ContextSize];
        const uint originalEsp = 0x00100100;
        Buffer.BlockCopy(BitConverter.GetBytes(unchecked((int)0x12345678)), 0, originalContext, ContextEipOffset, 4);
        Buffer.BlockCopy(BitConverter.GetBytes(unchecked((int)originalEsp)), 0, originalContext, ContextEspOffset, 4);
        Buffer.BlockCopy(BitConverter.GetBytes(0x00000202), 0, originalContext, ContextEflagsOffset, 4);
        byte[] remoteContext = ContextForRemoteCode(originalContext, 0x10001000, originalEsp);
        byte[] presentArgs = new byte[PresentCallArgumentsBytes];
        for (int i = 0; i < presentArgs.Length; i++) presentArgs[i] = (byte)(i + 1);
        byte[] samePresentArgs = (byte[])presentArgs.Clone();
        byte[] changedPresentArgs = (byte[])presentArgs.Clone();
        changedPresentArgs[PresentCallArgumentsBytes - 1] ^= 1;
        bool validStackRange = IsSafeNativeStackRange(0x00100000, 0x00200000, originalEsp, 24,
            0x00100000, 0x1000, MemCommit, 0x04);
        if (Marshal.SizeOf(typeof(X86ContextLayout)) != X86ContextSize
            || Marshal.OffsetOf(typeof(X86ContextLayout), "Eip").ToInt32() != ContextEipOffset
            || Marshal.OffsetOf(typeof(X86ContextLayout), "Esp").ToInt32() != ContextEspOffset
            || ContextAllX86 != 0x0001003F || acquire.Bytes.Length == 0 || cleanup.Bytes.Length == 0
            || acquire.MaxCallStackBytes != RemoteStubMaxCallStackBytes || cleanup.MaxCallStackBytes != 12
            || cameraSetterApply.Bytes.Length == 0 || cameraSetterApply.MaxCallStackBytes != 16
            || cameraSetterApply.Bytes[12] != 0x6A || cameraSetterApply.Bytes[13] != 2
            || cameraSetterRestore.Bytes[12] != 0x6A || cameraSetterRestore.Bytes[13] != 1
            || CameraSetterMode(CameraSetterPhase.Apply) != 2 || CameraSetterMode(CameraSetterPhase.Restore) != 1
            || cameraApplyCalls == null || cameraApplyCalls.Length != 2
            || cameraRestoreCalls == null || cameraRestoreCalls.Length != 1
            || DecodeIndirectCallDisplacement(cameraSetterApply.Bytes, cameraApplyCalls[0])
                != CameraTransformSetterSlotOffset
            || DecodeIndirectCallDisplacement(cameraSetterApply.Bytes, effectiveWorldGetterCall)
                != CameraTransformGetterSlotOffset
            || DecodeIndirectCallDisplacement(cameraSetterRestore.Bytes, cameraRestoreCalls[0])
                != CameraTransformSetterSlotOffset
            || !effectiveWorldGetterArgumentsMatch
            || cameraSetterApply.TrapAddress - cameraSetterApply.Base != (uint)(effectiveWorldGetterCall + 3)
            || cameraSetterApply.TrapAddress != 0x30000000 + cameraSetterApply.Bytes.Length - 1
            || surfaceProbe.Bytes.Length == 0 || surfaceProbe.MaxCallStackBytes != 12
            || surfaceProbe.CallInstructionOffsets == null || surfaceProbe.CallInstructionOffsets.Length != 2
            || DecodeIndirectCallDisplacement(surfaceProbe.Bytes, surfaceProbe.CallInstructionOffsets[0])
                != D3dDeviceGetRenderTargetOffset
            || DecodeIndirectCallDisplacement(surfaceProbe.Bytes, surfaceProbe.CallInstructionOffsets[1])
                != DdSurfaceReleaseOffset
            || surfaceProbe.TrapAddress != 0x18000000 + surfaceProbe.Bytes.Length - 1
            || VtableOffset(d3d7DeviceVtable, "GetRenderTarget") != D3dDeviceGetRenderTargetOffset
            || VtableOffset(surface7Vtable, "Lock") != DdSurfaceLockOffset
            || VtableOffset(surface7Vtable, "SetOverlayPosition") != 0x78
            || VtableOffset(surface7Vtable, "Unlock") != DdSurfaceUnlockOffset
            || VtableOffset(surface7Vtable, "Release") != DdSurfaceReleaseOffset
            || cleanup.CallInstructionOffsets == null || cleanup.CallInstructionOffsets.Length != 2
            || DecodeIndirectCallDisplacement(cleanup.Bytes, cleanup.CallInstructionOffsets[0]) != DdSurfaceUnlockOffset
            || DecodeIndirectCallDisplacement(cleanup.Bytes, cleanup.CallInstructionOffsets[1]) != DdSurfaceReleaseOffset
            || acquire.CallInstructionOffsets == null || acquire.CallInstructionOffsets.Length != 3
            || DecodeIndirectCallDisplacement(acquire.Bytes, acquire.CallInstructionOffsets[0]) != D3dDeviceGetRenderTargetOffset
            || DecodeIndirectCallDisplacement(acquire.Bytes, acquire.CallInstructionOffsets[1]) != DdSurfaceLockOffset
            || DecodeIndirectCallDisplacement(acquire.Bytes, acquire.CallInstructionOffsets[2]) != DdSurfaceReleaseOffset
            || DecodeIndirectCallDisplacement(new byte[] { 0xFF, 0x50, 0x80 }, 0) != -128
            || acquire.TrapAddress != 0x10000000 + acquire.Bytes.Length - 1
            || cleanup.TrapAddress != 0x10000000 + cleanup.Bytes.Length - 1
            || !validStackRange || !HasNativeStackBounds(0x00100000, 0x00200000, 0x00100018, 24)
            || HasNativeStackBounds(0x00100000, 0x00200000, 0x00100017, 24)
            || HasNativeStackBounds(0x00100000, 0x00200000, 0x00000010, 24)
            || HasNativeStackBounds(0x00100000, 0x00200000, 0x00100100, 0)
            || !IsSafeNativeStackRange(0x00100000, 0x00200000, originalEsp, 24,
                0x001000E8, 0x1000, MemCommit, 0x04)
            || IsSafeNativeStackRange(0x00100000, 0x00200000, originalEsp, 24,
                0x00100000, 0x1000, MemCommit, 0x104)
            || IsSafeNativeStackRange(0x00100000, 0x00200000, originalEsp, 24,
                0x00100000, 0xF0, MemCommit, 0x04)
            || BitConverter.ToUInt32(remoteContext, ContextEspOffset) != originalEsp
            || BitConverter.ToUInt32(remoteContext, ContextEipOffset) != 0x10001000
            || BitConverter.ToUInt32(remoteContext, ContextEflagsOffset) != 0x00000202
            || !ByteArraysEqual(presentArgs, samePresentArgs)
            || ByteArraysEqual(presentArgs, changedPresentArgs)
            || !IsViewportInsideSurface(new int[] { 0, 0, 1280, 1024 }, 1280, 1024)
            || IsViewportInsideSurface(new int[] { -1, 0, 1280, 1024 }, 1280, 1024)
            || IsViewportInsideSurface(new int[] { 0, 0, 1281, 1024 }, 1280, 1024)
            || IsViewportInsideSurface(new int[] { 0, 0, 0, 1024 }, 1280, 1024))
            throw new InvalidOperationException("Remote stub branch/INT3 self-check failed.");
        if (!HasLockedSurfaceDescriptor(0x100F, 0x19510000)
            || HasLockedSurfaceDescriptor(0x1007, 0x19510000)
            || HasLockedSurfaceDescriptor(0x100F, 0))
            throw new InvalidOperationException("D3D7 Lock descriptor compatibility self-check failed.");
        AssertPixel(DecodeSurfaceToBgra(new SurfaceReadback {
            Width = 1, Height = 1, Pitch = 2, BitCount = 16,
            RedMask = 0xF800, GreenMask = 0x07E0, BlueMask = 0x001F,
            Rows = new byte[] { 0x1F, 0xF8 }, RgbMasksVerified = true
        }), 255, 0, 255, 255);
        AssertPixel(DecodeSurfaceToBgra(new SurfaceReadback {
            Width = 1, Height = 1, Pitch = 3, BitCount = 24,
            RedMask = 0x00FF0000, GreenMask = 0x0000FF00, BlueMask = 0x000000FF,
            Rows = new byte[] { 0x11, 0x22, 0x33 }, RgbMasksVerified = true
        }), 0x11, 0x22, 0x33, 255);
        AssertPixel(DecodeSurfaceToBgra(new SurfaceReadback {
            Width = 1, Height = 1, Pitch = 4, BitCount = 32,
            RedMask = 0x00FF0000, GreenMask = 0x0000FF00, BlueMask = 0x000000FF, AlphaMask = 0xFF000000,
            Rows = new byte[] { 0x44, 0x55, 0x66, 0x77 }, RgbMasksVerified = true
        }), 0x44, 0x55, 0x66, 255);
        AssertPixel(DecodeSurfaceToBgra(new SurfaceReadback {
            Width = 1, Height = 2, Pitch = -3, BitCount = 24,
            RedMask = 0x00FF0000, GreenMask = 0x0000FF00, BlueMask = 0x000000FF,
            Rows = new byte[] { 1, 2, 3, 4, 5, 6 }, RgbMasksVerified = true
        }), 4, 5, 6, 255, 1, 2, 3, 255);
        Console.WriteLine("self-check: D3D7 GetRenderTarget and Surface7 Lock/Unlock/Release SDK vtable ordinals OK");
        Console.WriteLine("self-check: boundary surface probe calls GetRenderTarget/Release only; no Lock; 12-byte max stack OK");
        Console.WriteLine("self-check: caller stack footprint (24/12 bytes), original ESP, committed writable range and guard rejection OK");
        Console.WriteLine("self-check: selected-camera mode-2 setter refreshes selector-2 world cache before trap; local restore uses verified setter slot +0x1C; 16-byte maximum caller stack OK");
        Console.WriteLine("self-check: 24-byte present call arguments compare unchanged and detect a changed byte");
        Console.WriteLine("self-check: return context logging verifies remote EIP/ESP before restoring the original present call");
        Console.WriteLine("self-check: viewport bounds fit the captured pixel surface OK");
        Console.WriteLine("self-check: DDLOCK_READONLY|DONOTWAIT=0x4010; success/failure paths include Release; cleanup calls Release after Unlock regardless of result");
        Console.WriteLine("self-check: RGB565, 24-bit, 32-bit opaque PNG conversion and negative-pitch row order OK");
        return 0;
    }

    private static int VtableOffset(string[] methodOrder, string method)
    {
        for (int i = 0; i < methodOrder.Length; i++)
            if (String.Equals(methodOrder[i], method, StringComparison.Ordinal)) return i * 4;
        return -1;
    }

    private static int DecodeIndirectCallDisplacement(byte[] code, int offset)
    {
        if (code == null || offset < 0 || offset + 2 > code.Length || code[offset] != 0xFF
            || ((code[offset + 1] >> 3) & 7) != 2)
            throw new InvalidOperationException("Expected an x86 indirect CALL instruction.");
        int mod = code[offset + 1] >> 6;
        int rm = code[offset + 1] & 7;
        if (mod == 1)
        {
            if (offset + 3 > code.Length) throw new InvalidOperationException("Truncated disp8 indirect CALL.");
            return unchecked((sbyte)code[offset + 2]);
        }
        if (mod == 2 || (mod == 0 && rm == 5))
        {
            if (offset + 6 > code.Length) throw new InvalidOperationException("Truncated disp32 indirect CALL.");
            return BitConverter.ToInt32(code, offset + 2);
        }
        return 0;
    }
}
