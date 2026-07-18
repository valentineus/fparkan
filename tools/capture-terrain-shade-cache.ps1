[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [int]$ProcessId,
    [ValidateRange(1, 60)]
    [int]$CaptureAttempts = 6,
    [ValidateRange(0, 1000)]
    [int]$RetryIntervalMilliseconds = 100,
    [ValidateRange(0x00010000, 0x7fff0000)]
    [UInt32]$SearchStart = 0x01000000,
    [ValidateRange(0x00010000, 0x7fff0000)]
    [UInt32]$SearchEnd = 0x20000000
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# This observer deliberately opens only PROCESS_QUERY_INFORMATION | PROCESS_VM_READ.
# It neither sends input nor writes, suspends, injects into, or calls the original game.
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public static class FparkanTerrainShadeProbe {
    public const uint TH32CS_SNAPMODULE = 0x00000008;
    public const uint TH32CS_SNAPMODULE32 = 0x00000010;
    public const uint PROCESS_QUERY_INFORMATION = 0x00000400;
    public const uint PROCESS_VM_READ = 0x00000010;
    public const uint MEM_COMMIT = 0x1000;
    public const uint PAGE_NOACCESS = 0x01;
    public const uint PAGE_GUARD = 0x100;

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

    [StructLayout(LayoutKind.Sequential)]
    public struct MEMORY_BASIC_INFORMATION {
        public IntPtr BaseAddress;
        public IntPtr AllocationBase;
        public uint AllocationProtect;
        public IntPtr RegionSize;
        public uint State;
        public uint Protect;
        public uint Type;
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
        IntPtr process, IntPtr address, [Out] byte[] buffer, IntPtr size, out IntPtr bytesRead);
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern IntPtr VirtualQueryEx(
        IntPtr process, IntPtr address, out MEMORY_BASIC_INFORMATION buffer, IntPtr length);
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern bool CloseHandle(IntPtr handle);

    public static int FindU32(byte[] bytes, uint expected, int start) {
        for (int index = start; index <= bytes.Length - 4; index += 4) {
            if (BitConverter.ToUInt32(bytes, index) == expected) return index;
        }
        return -1;
    }
}
'@

function Get-LastWin32ErrorText {
    $code = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
    "$code ($([ComponentModel.Win32Exception]::new($code).Message))"
}

function Get-TerrainModuleBase {
    param([int]$ProbeProcessId)
    $snapshot = [FparkanTerrainShadeProbe]::CreateToolhelp32Snapshot(
        [FparkanTerrainShadeProbe]::TH32CS_SNAPMODULE -bor [FparkanTerrainShadeProbe]::TH32CS_SNAPMODULE32,
        [uint32]$ProbeProcessId
    )
    if ($snapshot -eq [IntPtr]::Zero -or $snapshot.ToInt64() -eq -1) {
        throw "CreateToolhelp32Snapshot failed: $(Get-LastWin32ErrorText)"
    }
    try {
        $module = [FparkanTerrainShadeProbe+MODULEENTRY32]::new()
        $module.dwSize = [Runtime.InteropServices.Marshal]::SizeOf([type][FparkanTerrainShadeProbe+MODULEENTRY32])
        if (-not [FparkanTerrainShadeProbe]::Module32First($snapshot, [ref]$module)) {
            throw "Module32First failed: $(Get-LastWin32ErrorText)"
        }
        do {
            if ($module.szModule -ieq 'Terrain.dll') {
                return $module.modBaseAddr.ToInt64()
            }
            $module = [FparkanTerrainShadeProbe+MODULEENTRY32]::new()
            $module.dwSize = [Runtime.InteropServices.Marshal]::SizeOf([type][FparkanTerrainShadeProbe+MODULEENTRY32])
        } while ([FparkanTerrainShadeProbe]::Module32Next($snapshot, [ref]$module))
    } finally {
        [void][FparkanTerrainShadeProbe]::CloseHandle($snapshot)
    }
    throw "Terrain.dll is not loaded by process $ProbeProcessId"
}

function Read-Bytes {
    param([IntPtr]$Process, [Int64]$Address, [int]$Length)
    $buffer = [byte[]]::new($Length)
    $read = [IntPtr]::Zero
    if (-not [FparkanTerrainShadeProbe]::ReadProcessMemory(
            $Process, [IntPtr]$Address, $buffer, [IntPtr]$Length, [ref]$read)) {
        return $null
    }
    if ($read.ToInt64() -ne $Length) {
        return $null
    }
    return $buffer
}

function Find-ShadeCache {
    param(
        [IntPtr]$Process,
        [UInt32]$ExpectedVtable,
        [UInt32]$Start,
        [UInt32]$End,
        [Int64]$ExcludedImageStart,
        [Int64]$ExcludedImageEnd
    )
    $mbiSize = [Runtime.InteropServices.Marshal]::SizeOf([type][FparkanTerrainShadeProbe+MEMORY_BASIC_INFORMATION])
    [Int64]$cursor = $Start
    while ($cursor -lt $End) {
        $mbi = [FparkanTerrainShadeProbe+MEMORY_BASIC_INFORMATION]::new()
        $queried = [FparkanTerrainShadeProbe]::VirtualQueryEx(
            $Process, [IntPtr]$cursor, [ref]$mbi, [IntPtr]$mbiSize
        )
        if ($queried -eq [IntPtr]::Zero) { break }
        $base = $mbi.BaseAddress.ToInt64()
        $size = $mbi.RegionSize.ToInt64()
        if ($size -le 0) { break }
        $next = $base + $size
        if ($mbi.State -eq [FparkanTerrainShadeProbe]::MEM_COMMIT -and
            ($mbi.Protect -band [FparkanTerrainShadeProbe]::PAGE_NOACCESS) -eq 0 -and
            ($mbi.Protect -band [FparkanTerrainShadeProbe]::PAGE_GUARD) -eq 0) {
            $regionStart = [Math]::Max($base, [Int64]$Start)
            $regionEnd = [Math]::Min($next, [Int64]$End)
            for ([Int64]$offset = $regionStart; $offset -lt $regionEnd; $offset += 65536) {
                $length = [int][Math]::Min(65536, $regionEnd - $offset)
                $bytes = Read-Bytes $Process $offset $length
                if ($null -eq $bytes) { continue }
                $searchIndex = 0
                while ($searchIndex -le $bytes.Length - 4) {
                    $index = [FparkanTerrainShadeProbe]::FindU32($bytes, $ExpectedVtable, $searchIndex)
                    if ($index -lt 0) { break }
                    $candidate = $offset + $index
                    if ($candidate -lt $ExcludedImageStart -or $candidate -ge $ExcludedImageEnd) {
                        return $candidate
                    }
                    $searchIndex = $index + 4
                }
            }
        }
        $cursor = [Math]::Max($next, $cursor + 4096)
    }
    return $null
}

if ($SearchEnd -le $SearchStart) { throw 'SearchEnd must exceed SearchStart' }
$terrainBase = Get-TerrainModuleBase $ProcessId
$process = [FparkanTerrainShadeProbe]::OpenProcess(
    [FparkanTerrainShadeProbe]::PROCESS_QUERY_INFORMATION -bor [FparkanTerrainShadeProbe]::PROCESS_VM_READ,
    $false,
    [uint32]$ProcessId
)
if ($process -eq [IntPtr]::Zero) { throw "OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ) failed: $(Get-LastWin32ErrorText)" }

try {
    $expectedVtable = [uint32]($terrainBase + 0x643d0)
    $last = 'cache vtable was not present in readable scan range'
    for ($attempt = 1; $attempt -le $CaptureAttempts; $attempt++) {
        # The vtable value itself naturally appears in the Terrain image as
        # relocation data; only a heap-resident object is a cache candidate.
        $cache = Find-ShadeCache $process $expectedVtable $SearchStart $SearchEnd $terrainBase ($terrainBase + 0x100000)
        if ($null -ne $cache) {
            $header = Read-Bytes $process $cache 324
            if ($null -ne $header) {
                [ordered]@{
                    schema = 'fparkan-terrain-shade-cache-v1'
                    process_id = $ProcessId
                    terrain_module_base = ('0x{0:X8}' -f $terrainBase)
                    cache_vtable_rva = '0x643d0'
                    cache_object = ('0x{0:X8}' -f $cache)
                    result_view = ('0x{0:X8}' -f [BitConverter]::ToUInt32($header, 24))
                    entry_table = ('0x{0:X8}' -f [BitConverter]::ToUInt32($header, 316))
                    entry_count = [BitConverter]::ToUInt32($header, 320)
                    scan_attempt = $attempt
                } | ConvertTo-Json -Compress
                exit 0
            }
            $last = "cache candidate 0x$('{0:X8}' -f $cache) became unreadable"
        }
        if ($attempt -lt $CaptureAttempts -and $RetryIntervalMilliseconds -gt 0) {
            Start-Sleep -Milliseconds $RetryIntervalMilliseconds
        }
    }
    throw "No live GetShade cache after $CaptureAttempts scans ($last)"
} finally {
    [void][FparkanTerrainShadeProbe]::CloseHandle($process)
}
