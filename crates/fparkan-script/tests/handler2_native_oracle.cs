using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;

internal static class Handler2NativeOracle {
    private const uint LoadWithAlteredSearchPath = 0x00000008;
    private const int RefreshRecordRva = 0x5070;
    private const string ExpectedSha256 = "c19cd6cd31cd2896f8c4ba0f0e700dd3408aec8182daaf4863d98a7d233a2abc";

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr LoadLibraryEx(string fileName, IntPtr file, uint flags);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool FreeLibrary(IntPtr module);

    [UnmanagedFunctionPointer(CallingConvention.ThisCall)]
    private delegate void RefreshRecord(IntPtr record);

    private static int Main() {
        if (IntPtr.Size != 4) {
            Console.Error.WriteLine("Build this oracle with an x86 target; ai.dll is 32-bit.");
            return 2;
        }

        string dllPath = Environment.GetEnvironmentVariable("FPARKAN_ORIGINAL_AI_DLL");
        if (String.IsNullOrEmpty(dllPath) || !File.Exists(dllPath)) {
            Console.Error.WriteLine("Set FPARKAN_ORIGINAL_AI_DLL to the original GOG ai.dll.");
            return 3;
        }

        string actualSha256;
        using (SHA256 sha256 = SHA256.Create())
        using (FileStream input = File.OpenRead(dllPath)) {
            byte[] digest = sha256.ComputeHash(input);
            StringBuilder hex = new StringBuilder(digest.Length * 2);
            foreach (byte value in digest) hex.Append(value.ToString("x2"));
            actualSha256 = hex.ToString();
        }
        if (actualSha256 != ExpectedSha256) {
            Console.Error.WriteLine("Unexpected ai.dll SHA-256: " + actualSha256);
            return 4;
        }

        IntPtr module = LoadLibraryEx(dllPath, IntPtr.Zero, LoadWithAlteredSearchPath);
        if (module == IntPtr.Zero) {
            Console.Error.WriteLine("LoadLibraryEx failed: " + Marshal.GetLastWin32Error());
            return 5;
        }

        try {
            var refresh = (RefreshRecord)Marshal.GetDelegateForFunctionPointer(
                IntPtr.Add(module, RefreshRecordRva), typeof(RefreshRecord));
            IntPtr record = Marshal.AllocHGlobal(100);
            try {
                for (int offset = 0; offset < 100; offset += 4) Marshal.WriteInt32(record, offset, 0);
                Marshal.WriteInt32(record, 0x24, 4);
                Marshal.WriteInt32(record, 0x28, 4);
                AssertRefresh(refresh, record, 8);
                AssertRefresh(refresh, record, 12);

                Marshal.WriteInt32(record, 0x24, unchecked((int)UInt32.MaxValue));
                Marshal.WriteInt32(record, 0x28, unchecked((int)UInt32.MaxValue));
                AssertRefresh(refresh, record, UInt32.MaxValue - 1);
                AssertRefresh(refresh, record, UInt32.MaxValue - 2);
            }
            finally { Marshal.FreeHGlobal(record); }
        }
        finally { FreeLibrary(module); }
        return 0;
    }

    private static void AssertRefresh(RefreshRecord refresh, IntPtr record, uint expectedCounter) {
        uint savedIncrement = unchecked((uint)Marshal.ReadInt32(record, 0x28));
        refresh(record);
        uint actualCounter = unchecked((uint)Marshal.ReadInt32(record, 0x24));
        uint actualIncrement = unchecked((uint)Marshal.ReadInt32(record, 0x28));
        Console.WriteLine("record[+0x24]=0x{0:X8} record[+0x28]=0x{1:X8}",
            actualCounter, actualIncrement);
        if (actualCounter != expectedCounter || actualIncrement != savedIncrement) {
            throw new InvalidOperationException(String.Format(
                "Expected +0x24=0x{0:X8}, +0x28 unchanged at 0x{1:X8}.",
                expectedCounter, savedIncrement));
        }
    }
}
