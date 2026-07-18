[CmdletBinding()]
param([Parameter(Mandatory = $true)][int]$ProcessId)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Read-only observer for ai.dll's `GetSuperAI` singleton array. It never sends
# input, writes memory, suspends, injects, or calls into the original process.
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class FparkanAiInitCapture {
    public const uint TH32CS_SNAPMODULE = 0x00000008;
    public const uint TH32CS_SNAPMODULE32 = 0x00000010;
    public const uint PROCESS_QUERY_INFORMATION = 0x00000400;
    public const uint PROCESS_VM_READ = 0x00000010;
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    public struct MODULEENTRY32 {
        public uint dwSize, th32ModuleID, th32ProcessID, GlblcntUsage, ProccntUsage;
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
    public static extern bool ReadProcessMemory(IntPtr process, IntPtr address,
        [Out] byte[] buffer, IntPtr size, out IntPtr bytesRead);
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern bool CloseHandle(IntPtr handle);
}
'@

function Read-Bytes([IntPtr]$Process, [Int64]$Address, [int]$Length) {
    $bytes = [byte[]]::new($Length); $read = [IntPtr]::Zero
    if (-not [FparkanAiInitCapture]::ReadProcessMemory($Process, [IntPtr]$Address,
            $bytes, [IntPtr]$Length, [ref]$read) -or $read.ToInt64() -ne $Length) {
        throw "ReadProcessMemory failed at 0x$('{0:X8}' -f $Address)"
    }
    $bytes
}

$snapshot = [FparkanAiInitCapture]::CreateToolhelp32Snapshot(
    [FparkanAiInitCapture]::TH32CS_SNAPMODULE -bor [FparkanAiInitCapture]::TH32CS_SNAPMODULE32,
    [uint32]$ProcessId)
$aiBase = $null
$modules = @()
try {
    $entry = [FparkanAiInitCapture+MODULEENTRY32]::new()
    $entry.dwSize = [Runtime.InteropServices.Marshal]::SizeOf([type][FparkanAiInitCapture+MODULEENTRY32])
    if ([FparkanAiInitCapture]::Module32First($snapshot, [ref]$entry)) {
        do {
            $modules += [ordered]@{
                name = $entry.szModule
                base = $entry.modBaseAddr.ToInt64()
                size = [int64]$entry.modBaseSize
            }
            if ($entry.szModule -ieq 'ai.dll') { $aiBase = $entry.modBaseAddr.ToInt64() }
            $entry = [FparkanAiInitCapture+MODULEENTRY32]::new()
            $entry.dwSize = [Runtime.InteropServices.Marshal]::SizeOf([type][FparkanAiInitCapture+MODULEENTRY32])
        } while ([FparkanAiInitCapture]::Module32Next($snapshot, [ref]$entry))
    }
} finally { [void][FparkanAiInitCapture]::CloseHandle($snapshot) }
if ($null -eq $aiBase) { throw "ai.dll is not loaded by process $ProcessId" }

$process = [FparkanAiInitCapture]::OpenProcess(
    [FparkanAiInitCapture]::PROCESS_QUERY_INFORMATION -bor [FparkanAiInitCapture]::PROCESS_VM_READ,
    $false, [uint32]$ProcessId)
if ($process -eq [IntPtr]::Zero) { throw "OpenProcess read-only failed" }
try {
    # CreateSuperAI stores its tenth host-callback argument at DAT_100555e4.
    $callbackBytes = Read-Bytes $process ($aiBase + 0x555e4) 4
    $callback = [BitConverter]::ToUInt32($callbackBytes, 0)
    $callbackModule = $modules | Where-Object {
        $callback -ge $_.base -and [int64]$callback -lt ($_.base + $_.size)
    } | Select-Object -First 1
    # GetSuperAI(i) returns (&DAT_10055398)[i], with ai.dll preferred base 0x10000000.
    $entries = Read-Bytes $process ($aiBase + 0x55398) (64 * 4)
    $samples = for ($index = 0; $index -lt 64; $index++) {
        $pointer = [BitConverter]::ToUInt32($entries, $index * 4)
        if ($pointer -le 0x10000) { continue }
        try {
            $fields = Read-Bytes $process ([int64]$pointer) 0x88
            [ordered]@{
                index = $index
                super_ai = ('0x{0:X8}' -f $pointer)
                word_7c = [BitConverter]::ToUInt32($fields, 0x7c)
                float_80 = [BitConverter]::ToSingle($fields, 0x80)
                float_84 = [BitConverter]::ToSingle($fields, 0x84)
            }
        } catch { }
    }
    [ordered]@{
        schema = 'fparkan-ai-init-v1'
        process_id = $ProcessId
        ai_module_base = ('0x{0:X8}' -f $aiBase)
        handler30_callback = ('0x{0:X8}' -f $callback)
        handler30_callback_module = if ($null -eq $callbackModule) { $null } else { $callbackModule.name }
        handler30_callback_rva = if ($null -eq $callbackModule) { $null } else { ('0x{0:X}' -f ([int64]$callback - $callbackModule.base)) }
        entries = @($samples)
    } |
        ConvertTo-Json -Depth 4 -Compress
} finally { [void][FparkanAiInitCapture]::CloseHandle($process) }
