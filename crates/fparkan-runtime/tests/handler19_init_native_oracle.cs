using System;
using System.Collections.Generic;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;

internal static class Handler19DispatchOracle {
    private const uint LoadWithAlteredSearchPath = 0x00000008;
    private const string ExpectedAiSha256 = "c19cd6cd31cd2896f8c4ba0f0e700dd3408aec8182daaf4863d98a7d233a2abc";
    private const string ExpectedScriptSha256 = "db1c6fde57aff43181619f19e19e1cacf7301ebb859e8cb883db6b4b7181eced";
    private const int EventLookupRva = 0x11f40;
    private const int EventDispatchRva = 0x11f10;
    private const int Handler19Rva = 0x0aa38;
    private const int EventStride = 0x14;
    private const int InstructionStride = 0x20;
    private const int VarSetStride = 0x30;

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr LoadLibraryEx(string fileName, IntPtr file, uint flags);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern bool FreeLibrary(IntPtr module);

    [UnmanagedFunctionPointer(CallingConvention.ThisCall)] private delegate int FindEvent(IntPtr vm, IntPtr name);
    [UnmanagedFunctionPointer(CallingConvention.ThisCall)] private delegate uint DispatchEvent(IntPtr vm, int eventIndex);
    [UnmanagedFunctionPointer(CallingConvention.ThisCall)] private delegate uint ReadVariable(IntPtr record);
    [UnmanagedFunctionPointer(CallingConvention.ThisCall)] private delegate void SetDwordPosition(IntPtr record, uint x, uint y);

    private sealed class Instruction {
        public readonly uint[] Words;
        public readonly uint[] References;
        public Instruction(uint[] words, uint[] references) { Words = words; References = references; }
    }
    private sealed class Event {
        public readonly byte[] Name;
        public readonly uint EventWord;
        public readonly Instruction[] Instructions;
        public Event(byte[] name, uint eventWord, Instruction[] instructions) { Name = name; EventWord = eventWord; Instructions = instructions; }
    }
    private sealed class Package {
        public readonly uint HandlerCount;
        public readonly Event[] Events;
        public Package(uint handlerCount, Event[] events) { HandlerCount = handlerCount; Events = events; }
    }
    private sealed class Instance : IDisposable {
        private readonly List<IntPtr> allocations = new List<IntPtr>();
        public IntPtr Ai;
        public IntPtr EventTable;
        public IntPtr HandlerTable;
        public IntPtr VarRecords;
        public IntPtr Cells;
        public void Dispose() { for (int i = allocations.Count - 1; i >= 0; --i) Marshal.FreeHGlobal(allocations[i]); }
        public IntPtr Allocate(int length) {
            IntPtr p = Marshal.AllocHGlobal(length);
            if (p == IntPtr.Zero) throw new OutOfMemoryException();
            Zero(p, length); allocations.Add(p); return p;
        }
    }

    private static int Main() {
        if (IntPtr.Size != 4) { Console.Error.WriteLine("Build with /platform:x86."); return 2; }
        string aiPath = Environment.GetEnvironmentVariable("FPARKAN_ORIGINAL_AI_DLL");
        string scriptPath = Environment.GetEnvironmentVariable("FPARKAN_ORIGINAL_DEFAULT_SCR");
        if (String.IsNullOrEmpty(aiPath) || !File.Exists(aiPath) || String.IsNullOrEmpty(scriptPath) || !File.Exists(scriptPath)) {
            Console.Error.WriteLine("Set FPARKAN_ORIGINAL_AI_DLL and FPARKAN_ORIGINAL_DEFAULT_SCR to the installed GOG files."); return 3;
        }
        if (Hash(aiPath) != ExpectedAiSha256) { Console.Error.WriteLine("Unexpected ai.dll SHA-256: " + Hash(aiPath)); return 4; }
        if (Hash(scriptPath) != ExpectedScriptSha256) { Console.Error.WriteLine("Unexpected default.scr SHA-256: " + Hash(scriptPath)); return 5; }
        Package package = ReadPackage(scriptPath);
        if (package.HandlerCount != 73 || package.Events.Length != 9) throw new InvalidDataException("Unexpected default.scr header.");
        if (package.Events[0].Name.Length != 5 || Encoding.ASCII.GetString(package.Events[0].Name) != "Init\0" || package.Events[0].Instructions.Length != 1)
            throw new InvalidDataException("Expected a single-instruction Init event at index 0.");
        Instruction init = package.Events[0].Instructions[0];
        if (init.Words[0] != 19 || init.Words[1] != 0xffffffff || init.Words[2] != 0xffffffff || init.References.Length != 3 || init.References[0] != 224 || init.References[1] != 225 || init.References[2] != 226)
            throw new InvalidDataException("Init instruction no longer matches the recovered Handler(19) shape.");

        IntPtr module = LoadLibraryEx(aiPath, IntPtr.Zero, LoadWithAlteredSearchPath);
        if (module == IntPtr.Zero) { Console.Error.WriteLine("LoadLibraryEx(ai.dll) failed: " + Marshal.GetLastWin32Error()); return 6; }
        try {
            FindEvent find = (FindEvent)Marshal.GetDelegateForFunctionPointer(IntPtr.Add(module, EventLookupRva), typeof(FindEvent));
            DispatchEvent dispatch = (DispatchEvent)Marshal.GetDelegateForFunctionPointer(IntPtr.Add(module, EventDispatchRva), typeof(DispatchEvent));
            ReadVariable readVariable = (ReadVariable)Marshal.GetDelegateForFunctionPointer(IntPtr.Add(module, 0x13570), typeof(ReadVariable));
            SetDwordPosition setDwordPosition = (SetDwordPosition)Marshal.GetDelegateForFunctionPointer(IntPtr.Add(module, 0x5c70), typeof(SetDwordPosition));
            VerifyEventLookupVectors(package, find, dispatch, module);
            RunInstance(package, find, dispatch, readVariable, module, 500.2857f, 752.0f, 0, 500, 752);
            RunInstance(package, find, dispatch, readVariable, module, 728.0f, 449.0f, 1, 728, 449);
            RunDwordToHandlerRoundTrip(package, find, dispatch, readVariable, setDwordPosition, module);
            Console.WriteLine("original ai.dll dispatcher passed for default.scr Init instances and DWORD-to-handler round-trip; code bytes were not modified");
            return 0;
        }
        finally { FreeLibrary(module); }
    }

    private static void VerifyEventLookupVectors(Package package, FindEvent find, DispatchEvent dispatch, IntPtr module) {
        var duplicateEvents = new Event[package.Events.Length + 1];
        Array.Copy(package.Events, duplicateEvents, package.Events.Length);
        duplicateEvents[duplicateEvents.Length - 1] = new Event(
            Encoding.ASCII.GetBytes("Init\0"), 0xabcdef01,
            new Instruction[] { new Instruction(new uint[] { 19, 0xffffffff, 0xffffffff, 0, 0, 3, 0 }, new uint[] { 224, 225, 226 }) });
        var duplicatePackage = new Package(package.HandlerCount, duplicateEvents);
        using (Instance instance = BuildInstance(duplicatePackage, 500.2857f, 752.0f, 0, IntPtr.Add(module, Handler19Rva))) {
            Expect(LookupEvent(find, IntPtr.Add(instance.Ai, 8), "Init") == 0, "native lookup chooses first exact duplicate event");
        }

        var embeddedNulEvents = (Event[])package.Events.Clone();
        for (int i = 0; i < embeddedNulEvents.Length; i++) {
            if (Encoding.ASCII.GetString(embeddedNulEvents[i].Name) == "Init\0") {
                embeddedNulEvents[i] = new Event(Encoding.ASCII.GetBytes("Init\0suffix\0"), embeddedNulEvents[i].EventWord, embeddedNulEvents[i].Instructions);
                break;
            }
        }
        var embeddedNulPackage = new Package(package.HandlerCount, embeddedNulEvents);
        using (Instance instance = BuildInstance(embeddedNulPackage, 500.2857f, 752.0f, 0, IntPtr.Add(module, Handler19Rva))) {
            Expect(LookupEvent(find, IntPtr.Add(instance.Ai, 8), "Init") == 0, "native lookup stops at an embedded NUL and ignores trailing name bytes");
        }

        var missingEvents = (Event[])package.Events.Clone();
        for (int i = 0; i < missingEvents.Length; i++) {
            if (Encoding.ASCII.GetString(missingEvents[i].Name) == "Init\0") {
                missingEvents[i] = new Event(Encoding.ASCII.GetBytes("NoInit\0"), missingEvents[i].EventWord, missingEvents[i].Instructions);
            }
        }
        var missingPackage = new Package(package.HandlerCount, missingEvents);
        using (Instance instance = BuildInstance(missingPackage, 500.2857f, 752.0f, 0, IntPtr.Add(module, Handler19Rva))) {
            IntPtr vm = IntPtr.Add(instance.Ai, 8);
            int eventIndex = LookupEvent(find, vm, "Init");
            Expect(eventIndex == -1, "native lookup reports missing exact event");
            Expect((dispatch(vm, eventIndex) & 0xff) == 0, "dispatching missing event performs no callback");
            Expect(Marshal.ReadInt32(instance.Cells, 0) == 0 && Marshal.ReadInt32(instance.Cells, 4) == 0 && Marshal.ReadInt32(instance.Cells, 8) == 0,
                "missing event makes no varset writes");
        }
        Console.WriteLine("original event lookup: first duplicate wins; Init\\0suffix also matches; missing Init returns -1 without writes");
    }

    private static int LookupEvent(FindEvent find, IntPtr vm, string nameText) {
        byte[] nameBytes = Encoding.ASCII.GetBytes(nameText + "\0");
        IntPtr name = Marshal.AllocHGlobal(nameBytes.Length);
        try {
            Marshal.Copy(nameBytes, 0, name, nameBytes.Length);
            return find(vm, name);
        }
        finally { Marshal.FreeHGlobal(name); }
    }

    private static void RunDwordToHandlerRoundTrip(Package package, FindEvent find, DispatchEvent dispatch, ReadVariable readVariable, SetDwordPosition setDwordPosition, IntPtr module) {
        IntPtr position = Marshal.AllocHGlobal(12);
        try {
            Zero(position, 12);
            setDwordPosition(position, UInt32.MaxValue, 10001);
            float x = BitConverter.ToSingle(BitConverter.GetBytes(ReadWord(position, 1)), 0);
            float y = BitConverter.ToSingle(BitConverter.GetBytes(ReadWord(position, 2)), 0);
            Expect(x == 4294967296.0f && y == 10001.0f, "FUN_10005c70 converts DWORD coordinates to the expected single-precision fields");
            Console.WriteLine("FUN_10005c70 input=({0:X8}, {1}) fields=({2:R}, {3:R})", UInt32.MaxValue, 10001, x, y);
            RunInstance(package, find, dispatch, readVariable, module, x, y, 2, 0, 10001);
        }
        finally { Marshal.FreeHGlobal(position); }
    }

    private static void RunInstance(Package package, FindEvent find, DispatchEvent dispatch, ReadVariable readVariable, IntPtr module, float x, float y, uint clanId, uint expectedX, uint expectedY) {
        using (Instance instance = BuildInstance(package, x, y, clanId, IntPtr.Add(module, Handler19Rva))) {
            IntPtr vm = IntPtr.Add(instance.Ai, 8);
            IntPtr name = Marshal.AllocHGlobal(5);
            try {
                Marshal.Copy(Encoding.ASCII.GetBytes("Init\0"), 0, name, 5);
                int eventIndex = find(vm, name);
                Expect(eventIndex == 0, "original name lookup returns Init index 0");
                uint result = dispatch(vm, eventIndex);
                uint first = ReadWord(instance.Cells, 0);
                uint second = ReadWord(instance.Cells, 1);
                uint third = ReadWord(instance.Cells, 2);
                uint getterX = readVariable(IntPtr.Add(instance.VarRecords, 224 * VarSetStride));
                uint getterY = readVariable(IntPtr.Add(instance.VarRecords, 225 * VarSetStride));
                uint getterClan = readVariable(IntPtr.Add(instance.VarRecords, 226 * VarSetStride));
                Console.WriteLine("probe clan={0}: cell_words=({1:X8}, {2:X8}, {3:X8}), native_getter=({4:X8}, {5:X8}, {6:X8})", clanId, first, second, third, getterX, getterY, getterClan);
                Expect((result & 0xff) == 1, "dispatcher reports completion");
                Expect(Marshal.ReadInt32(vm, 0x40) == 0, "current event field +0x48 is Init index");
                Expect(Marshal.ReadInt32(vm, 0x44) == 1, "current instruction field +0x4c advanced once");
                Expect(Marshal.ReadInt32(vm, 0x6c) == 0, "condition stack depth reset to zero");
                Expect(Marshal.ReadInt32(instance.Ai, 0x50) == 0, "handler result slot +0x50 cleared");
                Expect(first == expectedX && second == expectedY && third == clanId, "Handler(19) writes the three referenced DWORD cells in order");
                Expect(getterX == first && getterY == second && getterClan == third, "native DWORD getter reads the values written by Handler(19)");
                Console.WriteLine("clan={0}: Init={1}, instruction={2}, result={3}, stack={4}, result_slot={5}, cell_words=({6:X8}, {7:X8}, {8:X8}), typed_getters=({9:X8}, {10:X8}, {11:X8})",
                    clanId, eventIndex, Marshal.ReadInt32(vm, 0x44), result, Marshal.ReadInt32(vm, 0x6c), Marshal.ReadInt32(instance.Ai, 0x50), first, second, third, getterX, getterY, getterClan);
            }
            finally { Marshal.FreeHGlobal(name); }
        }
    }

    private static Instance BuildInstance(Package package, float x, float y, uint clanId, IntPtr handler19) {
        var instance = new Instance();
        try {
            instance.Ai = instance.Allocate(0x8b0);
            instance.EventTable = instance.Allocate(package.Events.Length * EventStride);
            instance.HandlerTable = instance.Allocate((int)package.HandlerCount * 4);
            instance.VarRecords = instance.Allocate(227 * VarSetStride);
            instance.Cells = instance.Allocate(3 * 4);
            IntPtr vm = IntPtr.Add(instance.Ai, 8);
            Marshal.WriteInt32(vm, 0, package.Events.Length);
            Marshal.WriteInt32(vm, 4, instance.EventTable.ToInt32());
            Marshal.WriteInt32(vm, 8, (int)package.HandlerCount);
            Marshal.WriteInt32(vm, 0x0c, instance.HandlerTable.ToInt32());
            Marshal.WriteInt32(vm, 0x3c, instance.Ai.ToInt32());
            Marshal.WriteInt32(instance.HandlerTable, 19 * 4, handler19.ToInt32());
            for (int eventIndex = 0; eventIndex < package.Events.Length; eventIndex++) {
                Event sourceEvent = package.Events[eventIndex];
                IntPtr name = instance.Allocate(sourceEvent.Name.Length);
                Marshal.Copy(sourceEvent.Name, 0, name, sourceEvent.Name.Length);
                IntPtr records = sourceEvent.Instructions.Length == 0 ? IntPtr.Zero : instance.Allocate(sourceEvent.Instructions.Length * InstructionStride);
                int eventOffset = eventIndex * EventStride;
                Marshal.WriteInt32(instance.EventTable, eventOffset, name.ToInt32());
                Marshal.WriteInt32(instance.EventTable, eventOffset + 4, sourceEvent.Name.Length - 1);
                Marshal.WriteInt32(instance.EventTable, eventOffset + 8, unchecked((int)sourceEvent.EventWord));
                Marshal.WriteInt32(instance.EventTable, eventOffset + 0x0c, sourceEvent.Instructions.Length);
                Marshal.WriteInt32(instance.EventTable, eventOffset + 0x10, records.ToInt32());
                for (int instructionIndex = 0; instructionIndex < sourceEvent.Instructions.Length; instructionIndex++) {
                    Instruction sourceInstruction = sourceEvent.Instructions[instructionIndex];
                    IntPtr refs = sourceInstruction.References.Length == 0 ? IntPtr.Zero : instance.Allocate(sourceInstruction.References.Length * 4);
                    for (int referenceIndex = 0; referenceIndex < sourceInstruction.References.Length; referenceIndex++)
                        Marshal.WriteInt32(refs, referenceIndex * 4, unchecked((int)sourceInstruction.References[referenceIndex]));
                    IntPtr instruction = IntPtr.Add(records, instructionIndex * InstructionStride);
                    for (int word = 0; word < 5; word++) Marshal.WriteInt32(instruction, word * 4, unchecked((int)sourceInstruction.Words[word]));
                    Marshal.WriteInt32(instruction, 0x14, refs.ToInt32());
                    Marshal.WriteInt32(instruction, 0x18, sourceInstruction.References.Length);
                    Marshal.WriteInt32(instruction, 0x1c, unchecked((int)sourceInstruction.Words[6]));
                }
            }
            Marshal.WriteInt32(instance.Ai, 0x7c, unchecked((int)clanId));
            Marshal.WriteInt32(instance.Ai, 0x80, BitConverter.ToInt32(BitConverter.GetBytes(x), 0));
            Marshal.WriteInt32(instance.Ai, 0x84, BitConverter.ToInt32(BitConverter.GetBytes(y), 0));
            // AI+0x18 is the original varset vector object: +4=count and +0xc=record array.
            Marshal.WriteInt32(instance.Ai, 0x1c, 227);
            Marshal.WriteInt32(instance.Ai, 0x24, instance.VarRecords.ToInt32());
            uint[] targets = { 224, 225, 226 };
            for (int i = 0; i < targets.Length; i++) {
                IntPtr record = IntPtr.Add(instance.VarRecords, checked((int)targets[i] * VarSetStride));
                IntPtr cell = IntPtr.Add(instance.Cells, i * 4);
                Marshal.WriteInt32(record, 8, 5); // original parser's DWORD type tag
                Marshal.WriteInt32(record, 0x20, cell.ToInt32());
            }
            return instance;
        }
        catch { instance.Dispose(); throw; }
    }

    private static Package ReadPackage(string path) {
        using (BinaryReader reader = new BinaryReader(File.OpenRead(path), Encoding.ASCII)) {
            uint handlerCount = reader.ReadUInt32();
            uint eventCount = reader.ReadUInt32();
            if (eventCount > 4096 || handlerCount > 4096) throw new InvalidDataException("Unreasonable package header.");
            var events = new Event[eventCount];
            for (int e = 0; e < events.Length; e++) {
                uint nameLength = reader.ReadUInt32();
                if (nameLength > 1024) throw new InvalidDataException("Unreasonable event name length.");
                byte[] name = reader.ReadBytes(checked((int)nameLength + 1));
                if (name.Length != nameLength + 1 || name[name.Length - 1] != 0) throw new InvalidDataException("Bad event name.");
                uint eventWord = reader.ReadUInt32();
                uint instructionCount = reader.ReadUInt32();
                if (instructionCount > 65536) throw new InvalidDataException("Unreasonable event instruction count.");
                var instructions = new Instruction[instructionCount];
                for (int i = 0; i < instructions.Length; i++) {
                    uint[] words = new uint[6];
                    for (int w = 0; w < words.Length; w++) words[w] = reader.ReadUInt32();
                    if (words[5] > 4096) throw new InvalidDataException("Unreasonable reference count.");
                    uint[] refs = new uint[words[5]];
                    for (int r = 0; r < refs.Length; r++) refs[r] = reader.ReadUInt32();
                    uint trailing = reader.ReadUInt32();
                    instructions[i] = new Instruction(new uint[] { words[0], words[1], words[2], words[3], words[4], words[5], trailing }, refs);
                }
                events[e] = new Event(name, eventWord, instructions);
            }
            if (reader.BaseStream.Position != reader.BaseStream.Length) throw new InvalidDataException("Trailing bytes in default.scr.");
            return new Package(handlerCount, events);
        }
    }

    private static uint ReadWord(IntPtr cells, int index) { return unchecked((uint)Marshal.ReadInt32(cells, index * 4)); }
    private static string Hash(string path) {
        using (SHA256 sha = SHA256.Create()) using (FileStream input = File.OpenRead(path)) {
            byte[] digest = sha.ComputeHash(input); var hex = new StringBuilder();
            foreach (byte value in digest) hex.Append(value.ToString("x2")); return hex.ToString();
        }
    }
    private static void Expect(bool value, string message) { if (!value) throw new InvalidOperationException("original dispatcher oracle failed: " + message); }
    private static void Zero(IntPtr buffer, int length) { for (int offset = 0; offset < length; offset++) Marshal.WriteByte(buffer, offset, 0); }
}
