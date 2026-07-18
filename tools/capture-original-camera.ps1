[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [int]$ProcessId,
    [ValidateRange(1, 16384)]
    [int]$ViewportWidth = 1024,
    [ValidateRange(1, 16384)]
    [int]$ViewportHeight = 768,
    [ValidateRange(0.001, 1000.0)]
    [double]$NearPlane = 0.5,
    [ValidateRange(0.01, 100000.0)]
    [double]$FarPlane = 700.0,
    [ValidateRange(0.01, 3.13)]
    [double]$FieldOfViewRadians = 1.3,
    [ValidateRange(1, 600)]
    [int]$CaptureAttempts = 60,
    [ValidateRange(0, 1000000.0)]
    [double]$MinimumWorldTranslation = 100.0,
    [ValidateRange(0, 1000)]
    [int]$RetryIntervalMilliseconds = 100
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# This observer deliberately opens only PROCESS_QUERY_INFORMATION | PROCESS_VM_READ.
# It neither sends input nor writes, suspends, injects into, or calls the original game.
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public static class FparkanOriginalCamera {
    public const uint TH32CS_SNAPMODULE = 0x00000008;
    public const uint TH32CS_SNAPMODULE32 = 0x00000010;
    public const uint PROCESS_QUERY_INFORMATION = 0x00000400;
    public const uint PROCESS_VM_READ = 0x00000010;

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    public struct MODULEENTRY32 {
        public uint dwSize;
        public uint th32ModuleID;
        public uint th32ProcessID;
        public uint GlblcntUsage;
        public uint ProccntUsage;
        public IntPtr modBaseAddr;
        public uint modBaseSize;
        public IntPtr hModule;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] public string szModule;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 260)] public string szExePath;
    }

    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern IntPtr CreateToolhelp32Snapshot(uint flags, uint processId);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern bool Module32First(IntPtr snapshot, ref MODULEENTRY32 entry);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern bool Module32Next(IntPtr snapshot, ref MODULEENTRY32 entry);
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern IntPtr OpenProcess(uint access, bool inheritHandle, uint processId);
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern bool ReadProcessMemory(
        IntPtr process,
        IntPtr address,
        [Out] byte[] buffer,
        IntPtr size,
        out IntPtr bytesRead);
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern bool CloseHandle(IntPtr handle);
}
'@

function Get-LastWin32ErrorText {
    $code = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
    "$code ($([ComponentModel.Win32Exception]::new($code).Message))"
}

function Read-OriginalBytes {
    param(
        [IntPtr]$Process,
        [Int64]$Address,
        [int]$Length
    )
    $buffer = [byte[]]::new($Length)
    $read = [IntPtr]::Zero
    if (-not [FparkanOriginalCamera]::ReadProcessMemory(
            $Process,
            [IntPtr]$Address,
            $buffer,
            [IntPtr]$Length,
            [ref]$read)) {
        throw "ReadProcessMemory at 0x$('{0:X8}' -f $Address) failed: $(Get-LastWin32ErrorText)"
    }
    if ($read.ToInt64() -ne $Length) {
        throw "ReadProcessMemory at 0x$('{0:X8}' -f $Address) returned $($read.ToInt64()) of $Length bytes"
    }
    $buffer
}

$snapshot = [FparkanOriginalCamera]::CreateToolhelp32Snapshot(
    [FparkanOriginalCamera]::TH32CS_SNAPMODULE -bor [FparkanOriginalCamera]::TH32CS_SNAPMODULE32,
    [uint32]$ProcessId
)
if ($snapshot -eq [IntPtr]::Zero -or $snapshot.ToInt64() -eq -1) {
    throw "CreateToolhelp32Snapshot failed: $(Get-LastWin32ErrorText)"
}

$terrainBase = $null
try {
    $module = [FparkanOriginalCamera+MODULEENTRY32]::new()
    $module.dwSize = [Runtime.InteropServices.Marshal]::SizeOf([type][FparkanOriginalCamera+MODULEENTRY32])
    if (-not [FparkanOriginalCamera]::Module32First($snapshot, [ref]$module)) {
        throw "Module32First failed: $(Get-LastWin32ErrorText)"
    }
    do {
        if ($module.szModule -ieq 'Terrain.dll') {
            $terrainBase = $module.modBaseAddr.ToInt64()
            break
        }
        $module = [FparkanOriginalCamera+MODULEENTRY32]::new()
        $module.dwSize = [Runtime.InteropServices.Marshal]::SizeOf([type][FparkanOriginalCamera+MODULEENTRY32])
    } while ([FparkanOriginalCamera]::Module32Next($snapshot, [ref]$module))
} finally {
    [void][FparkanOriginalCamera]::CloseHandle($snapshot)
}

if ($null -eq $terrainBase) {
    throw "Terrain.dll is not loaded by process $ProcessId"
}

$process = [FparkanOriginalCamera]::OpenProcess(
    [FparkanOriginalCamera]::PROCESS_QUERY_INFORMATION -bor [FparkanOriginalCamera]::PROCESS_VM_READ,
    $false,
    [uint32]$ProcessId
)
if ($process -eq [IntPtr]::Zero) {
    throw "OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ) failed: $(Get-LastWin32ErrorText)"
}

try {
    # Terrain.dll image RVA 0x7355c -> active 0x1a4-byte outer camera object.
    # Its selector-0 affine block begins at outer + 0x20. Words 3/7/11 are
    # the proven world-space translation components. AutoDemo also briefly
    # selects normalized/reflection-like camera objects; a finite matrix alone
    # is not sufficient evidence that it can project the mission world.
    $expectedOuterVtable = [uint32]($terrainBase + 0x665b4)
    $matrixBytes = $null
    $translation = $null
    $cameraOuter = $null
    $lastObservation = 'camera pointer unavailable'
    for ($attempt = 1; $attempt -le $CaptureAttempts; $attempt++) {
        $candidateOuter = [BitConverter]::ToUInt32(
            (Read-OriginalBytes $process ($terrainBase + 0x7355c) 4),
            0
        )
        if ($candidateOuter -lt 0x10000) {
            $lastObservation = "camera pointer 0x$('{0:X8}' -f $candidateOuter)"
            continue
        }
        $outerVtable = [BitConverter]::ToUInt32(
            (Read-OriginalBytes $process ([int64]$candidateOuter) 4),
            0
        )
        if ($outerVtable -ne $expectedOuterVtable) {
            $lastObservation = "outer vtable 0x$('{0:X8}' -f $outerVtable)"
            continue
        }
        $matrixBytes = Read-OriginalBytes $process ([int64]$candidateOuter + 0x20) 64
        $candidateTranslation = @(
            [BitConverter]::ToSingle($matrixBytes, 12),
            [BitConverter]::ToSingle($matrixBytes, 28),
            [BitConverter]::ToSingle($matrixBytes, 44)
        )
        $nonFinite = @($candidateTranslation | Where-Object {
            [Single]::IsNaN($_) -or [Single]::IsInfinity($_)
        })
        $length = [Math]::Sqrt(
            [double]$candidateTranslation[0] * $candidateTranslation[0] +
            [double]$candidateTranslation[1] * $candidateTranslation[1] +
            [double]$candidateTranslation[2] * $candidateTranslation[2]
        )
        if ($nonFinite.Count -eq 0 -and $length -ge $MinimumWorldTranslation) {
            $cameraOuter = $candidateOuter
            $translation = $candidateTranslation
            break
        }
        $lastObservation = "translation length $length at 0x$('{0:X8}' -f $candidateOuter)"
        if ($attempt -lt $CaptureAttempts -and $RetryIntervalMilliseconds -gt 0) {
            Start-Sleep -Milliseconds $RetryIntervalMilliseconds
        }
    }
    if ($null -eq $translation) {
        throw "No world-space selector-0 transform after $CaptureAttempts samples (minimum translation length $MinimumWorldTranslation; last observation: $lastObservation)"
    }
    $words = for ($index = 0; $index -lt 16; $index++) {
        [BitConverter]::ToUInt32($matrixBytes, $index * 4)
    }
    [ordered]@{
        schema = 'fparkan-legacy-camera-v1'
        process_id = $ProcessId
        terrain_module_base = ('0x{0:X8}' -f $terrainBase)
        terrain_camera_global_rva = '0x7355c'
        terrain_camera_outer = ('0x{0:X8}' -f $cameraOuter)
        selector0_words = @($words)
        selector0_translation = @($translation)
        # LegacyD3d7Projection carries a D3D7 RECT: left, top, right, bottom.
        viewport = @(0, 0, $ViewportWidth, $ViewportHeight)
        near_plane = $NearPlane
        far_plane = $FarPlane
        field_of_view_radians = $FieldOfViewRadians
    } | ConvertTo-Json -Depth 4 -Compress
} finally {
    [void][FparkanOriginalCamera]::CloseHandle($process)
}
