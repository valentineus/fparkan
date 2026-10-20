using System;
using System.Collections.Generic;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;

// Calls the original x86 Behavior.dll distributor tick against synthetic vtables.
// Only the imported GetSystemArealMap IAT slot is replaced; the tick instructions
// and its internal helpers remain the installed DLL's original code.
internal static class BehaviorDistributorTickOracle {
    private const uint LoadWithAlteredSearchPath = 0x00000008;
    private const uint MemCommitReserve = 0x00003000;
    private const uint PageExecuteReadWrite = 0x40;
    private const uint MemRelease = 0x00008000;
    private const uint PageReadWrite = 0x04;
    private const string ExpectedBehaviorSha256 = "ddfbdbde07832358fe6e35e01c8dba9b58e7313789498b6049ed51d4ed9baf99";
    private const int TickRva = 0x19e80;
    private const int GetSystemArealMapIatRva = 0x59008;
    private const uint ResourceProperty = 0x02000100;
    private static Fixture active;

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr LoadLibraryEx(string fileName, IntPtr file, uint flags);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern bool FreeLibrary(IntPtr module);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern IntPtr VirtualAlloc(IntPtr address, UIntPtr size, uint allocationType, uint protect);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern bool VirtualFree(IntPtr address, UIntPtr size, uint freeType);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern bool VirtualProtect(IntPtr address, UIntPtr size, uint protect, out uint oldProtect);

    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate IntPtr AreaMapGetter();
    [UnmanagedFunctionPointer(CallingConvention.ThisCall)] private delegate void TickDelegate(IntPtr self, float deltaSeconds);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate IntPtr MapLookup(IntPtr self, uint key);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate int ObjectState(IntPtr self);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate uint ObjectClass(IntPtr self);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate int ObjectQueryInterface(IntPtr self, uint iid, IntPtr result);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate IntPtr ObjectRate(IntPtr self, uint selector);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate float ObjectGetProperty(IntPtr self, uint property, IntPtr result);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate void ObjectSetProperty(IntPtr self, uint property, float value, IntPtr result);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate int InterfaceGetProperty(IntPtr self, uint selector, IntPtr result);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate int InterfaceSetProperty(IntPtr self, uint selector, IntPtr value);

    private static readonly AreaMapGetter AreaMapGetterCallback = GetAreaMap;
    private static readonly MapLookup MapLookupCallback = Lookup;
    private static readonly ObjectState ObjectStateCallback = GetObjectState;
    private static readonly ObjectClass ObjectClassCallback = GetObjectClass;
    private static readonly ObjectQueryInterface ObjectQueryCallback = QueryObject;
    private static readonly ObjectRate ObjectRateCallback = GetRate;
    private static readonly ObjectGetProperty ObjectGetPropertyCallback = GetProperty;
    private static readonly ObjectSetProperty ObjectSetPropertyCallback = SetProperty;
    private static readonly InterfaceGetProperty InterfaceGetCallback = GetInterfaceProperty;
    private static readonly InterfaceSetProperty InterfaceSetCallback = SetInterfaceProperty;
    private static IntPtr fastcallThunks;
    private static IntPtr queryObjectThunk;
    private static IntPtr getInterfacePropertyThunk;
    private static IntPtr setInterfacePropertyThunk;

    private sealed class NativeObject {
        public IntPtr Self;
        public IntPtr Vtable;
        public IntPtr Interface16;
        public IntPtr Interface204;
        public IntPtr Interface16Vtable;
        public IntPtr Interface204Vtable;
        public IntPtr Rate1005;
        public IntPtr Rate1002;
        public uint Key;
        public uint Class;
        public int State;
        public float Property;
        public int PropertyMetadata;
        public float PowerRate;
        public float OreRate;
        public float[] OreRateSequence;
        public int PowerRateReads;
        public int OreRateReads;
        public float InterfaceSelector1;
        public float InterfaceSelector2;
        public int Selector1Reads;
        public int Selector2Reads;
        public int Writes;
        public readonly List<string> Calls = new List<string>();
    }

    private sealed class Fixture : IDisposable {
        public IntPtr Distributor;
        public IntPtr Entries;
        public IntPtr Manager;
        public IntPtr ManagerVtable;
        public readonly List<IntPtr> Allocations = new List<IntPtr>();
        public readonly Dictionary<uint, NativeObject> ByKey = new Dictionary<uint, NativeObject>();
        public readonly List<string> Calls = new List<string>();
        public readonly List<string> Trace = new List<string>();
        public int LookupCount;
        public NativeObject AddObject(uint key, uint classCode, float powerRate, float oreRate, float property, int state) {
            NativeObject item = new NativeObject();
            item.Key = key;
            item.Class = classCode;
            item.PowerRate = powerRate;
            item.OreRate = oreRate;
            item.Property = property;
            item.State = state;
            item.InterfaceSelector1 = 1.0f;
            item.InterfaceSelector2 = 1.0f;
            item.Vtable = Allocate(0x70);
            item.Self = Allocate(0x80);
            item.Interface16Vtable = Allocate(0x20);
            item.Interface204Vtable = Allocate(0x20);
            item.Interface16 = Allocate(IntPtr.Size);
            item.Interface204 = Allocate(IntPtr.Size);
            item.Rate1005 = Allocate(sizeof(float));
            item.Rate1002 = Allocate(sizeof(float));
            Marshal.WriteIntPtr(item.Self, 0, item.Vtable);
            Marshal.WriteIntPtr(item.Interface16, 0, item.Interface16Vtable);
            Marshal.WriteIntPtr(item.Interface204, 0, item.Interface204Vtable);
            Marshal.WriteIntPtr(item.Vtable, 0x00, queryObjectThunk);
            Marshal.WriteIntPtr(item.Vtable, 0x38, Marshal.GetFunctionPointerForDelegate(ObjectClassCallback));
            Marshal.WriteIntPtr(item.Vtable, 0x44, Marshal.GetFunctionPointerForDelegate(ObjectStateCallback));
            Marshal.WriteIntPtr(item.Vtable, 0x48, Marshal.GetFunctionPointerForDelegate(ObjectSetPropertyCallback));
            Marshal.WriteIntPtr(item.Vtable, 0x4c, Marshal.GetFunctionPointerForDelegate(ObjectGetPropertyCallback));
            Marshal.WriteIntPtr(item.Vtable, 0x68, Marshal.GetFunctionPointerForDelegate(ObjectRateCallback));
            Marshal.WriteIntPtr(item.Interface16Vtable, 0x10, getInterfacePropertyThunk);
            Marshal.WriteIntPtr(item.Interface16Vtable, 0x14, setInterfacePropertyThunk);
            Marshal.WriteIntPtr(item.Interface204Vtable, 0x10, getInterfacePropertyThunk);
            Marshal.WriteIntPtr(item.Interface204Vtable, 0x14, setInterfacePropertyThunk);
            Marshal.WriteInt32(item.Rate1005, 0, BitConverter.ToInt32(BitConverter.GetBytes(powerRate), 0));
            Marshal.WriteInt32(item.Rate1002, 0, BitConverter.ToInt32(BitConverter.GetBytes(oreRate), 0));
            ByKey.Add(key, item);
            return item;
        }
        public void MakeDistributor(params uint[] keys) {
            Distributor = Allocate(0x44);
            Entries = Allocate(Math.Max(1, keys.Length) * 0x1c);
            Marshal.WriteInt32(Distributor, 0x20, keys.Length);
            Marshal.WriteIntPtr(Distributor, 0x28, Entries);
            for (int i = 0; i < keys.Length; i++) {
                IntPtr entry = IntPtr.Add(Entries, i * 0x1c);
                Marshal.WriteInt32(entry, 0x00, unchecked((int)keys[i]));
                Marshal.WriteInt32(entry, 0x04, BitConverter.ToInt32(BitConverter.GetBytes(1.0f), 0));
            }
        }
        private IntPtr Allocate(int size) {
            IntPtr value = Marshal.AllocHGlobal(size);
            Zero(value, size);
            Allocations.Add(value);
            return value;
        }
        public void Dispose() {
            active = null;
            for (int i = Allocations.Count - 1; i >= 0; i--) Marshal.FreeHGlobal(Allocations[i]);
            Allocations.Clear();
        }
    }

    private static IntPtr GetAreaMap() { return active.Manager; }
    private static IntPtr Lookup(IntPtr self, uint key) {
        active.Calls.Add("lookup:" + key.ToString("x8"));
        active.LookupCount++;
        active.Trace.Add("lookup:" + key.ToString("x8"));
        NativeObject item;
        return active.ByKey.TryGetValue(key, out item) ? item.Self : IntPtr.Zero;
    }
    private static NativeObject Resolve(IntPtr self) {
        foreach (NativeObject item in active.ByKey.Values) if (item.Self == self) return item;
        throw new InvalidOperationException("unexpected object pointer 0x" + self.ToString("x"));
    }
    private static NativeObject ResolveInterface(IntPtr self, out uint iid) {
        foreach (NativeObject item in active.ByKey.Values) {
            if (item.Interface16 == self) { iid = 0x16; return item; }
            if (item.Interface204 == self) { iid = 0x204; return item; }
        }
        iid = 0;
        throw new InvalidOperationException("unexpected interface pointer 0x" + self.ToString("x"));
    }
    private static int GetObjectState(IntPtr self) {
        NativeObject item = Resolve(self);
        active.Calls.Add("state:" + item.Key.ToString("x8"));
        active.Trace.Add("state:" + item.Key.ToString("x8"));
        return item.State;
    }
    private static uint GetObjectClass(IntPtr self) {
        NativeObject item = Resolve(self);
        active.Calls.Add("class:" + item.Key.ToString("x8") + ":" + item.Class.ToString("x8"));
        active.Trace.Add("class:" + item.Key.ToString("x8") + ":" + item.Class.ToString("x8"));
        return item.Class;
    }
    private static int QueryObject(IntPtr self, uint iid, IntPtr result) {
        NativeObject item = Resolve(self);
        item.Calls.Add("query:" + iid.ToString("x"));
        active.Trace.Add("query:" + item.Key.ToString("x8") + ":" + iid.ToString("x"));
        if (iid == 0x16) Marshal.WriteIntPtr(result, item.Interface16);
        else if (iid == 0x204) Marshal.WriteIntPtr(result, item.Interface204);
        else return 0;
        return 1;
    }
    private static IntPtr GetRate(IntPtr self, uint selector) {
        NativeObject item = Resolve(self);
        if (selector == 0x1005) {
            float value = item.PowerRate;
            item.PowerRateReads++;
            Marshal.WriteInt32(item.Rate1005, BitConverter.ToInt32(BitConverter.GetBytes(value), 0));
            item.Calls.Add("rate:1005:" + Float(value)); active.Trace.Add("rate:" + item.Key.ToString("x8") + ":1005:" + Float(value));
            return item.Rate1005;
        }
        if (selector == 0x1002) {
            float value = SequenceValue(item.OreRateSequence, item.OreRateReads++, item.OreRate);
            Marshal.WriteInt32(item.Rate1002, BitConverter.ToInt32(BitConverter.GetBytes(value), 0));
            item.Calls.Add("rate:1002:" + Float(value)); active.Trace.Add("rate:" + item.Key.ToString("x8") + ":1002:" + Float(value));
            return item.Rate1002;
        }
        item.Calls.Add("rate:" + selector.ToString("x"));
        active.Trace.Add("rate:" + item.Key.ToString("x8") + ":" + selector.ToString("x"));
        return IntPtr.Zero;
    }
    private static float GetProperty(IntPtr self, uint property, IntPtr result) {
        NativeObject item = Resolve(self);
        item.Calls.Add("get:" + property.ToString("x8"));
        active.Trace.Add("get:" + item.Key.ToString("x8") + ":" + property.ToString("x8"));
        if (result != IntPtr.Zero) Marshal.WriteInt32(result, item.PropertyMetadata);
        return item.Property;
    }
    private static void SetProperty(IntPtr self, uint property, float value, IntPtr outPointer) {
        NativeObject item = Resolve(self);
        item.Calls.Add("set:" + property.ToString("x8") + ":" + value.ToString("R", System.Globalization.CultureInfo.InvariantCulture) + ":out=" + outPointer.ToString("x"));
        active.Trace.Add("set:" + item.Key.ToString("x8") + ":" + property.ToString("x8") + ":" + value.ToString("R", System.Globalization.CultureInfo.InvariantCulture));
        if (property == ResourceProperty) { item.Property = value; item.Writes++; }
        if (property == ResourceProperty && outPointer != IntPtr.Zero) item.PropertyMetadata = Marshal.ReadInt32(outPointer);
    }
    private static int GetInterfaceProperty(IntPtr self, uint selector, IntPtr result) {
        uint iid;
        NativeObject item = ResolveInterface(self, out iid);
        float value;
        if (selector == 1) {
            item.Selector1Reads++;
            value = item.InterfaceSelector1;
            item.Calls.Add("iface-get:" + iid.ToString("x") + ":1:" + value.ToString("R", System.Globalization.CultureInfo.InvariantCulture));
            active.Trace.Add("iface-get:" + item.Key.ToString("x8") + ":" + iid.ToString("x") + ":1:" + value.ToString("R", System.Globalization.CultureInfo.InvariantCulture));
        } else if (selector == 2) {
            item.Selector2Reads++;
            value = item.InterfaceSelector2;
            item.Calls.Add("iface-get:" + iid.ToString("x") + ":2:" + value.ToString("R", System.Globalization.CultureInfo.InvariantCulture));
            active.Trace.Add("iface-get:" + item.Key.ToString("x8") + ":" + iid.ToString("x") + ":2:" + value.ToString("R", System.Globalization.CultureInfo.InvariantCulture));
        } else {
            item.Calls.Add("iface-get:" + iid.ToString("x") + ":" + selector.ToString("x"));
            active.Trace.Add("iface-get:" + item.Key.ToString("x8") + ":" + iid.ToString("x") + ":" + selector.ToString("x"));
            value = 0.0f;
        }
        if (result != IntPtr.Zero) Marshal.WriteInt32(result, 0, BitConverter.ToInt32(BitConverter.GetBytes(value), 0));
        return 1;
    }
    private static int SetInterfaceProperty(IntPtr self, uint selector, IntPtr value) {
        uint iid;
        NativeObject item = ResolveInterface(self, out iid);
        float number = value == IntPtr.Zero ? 0.0f : BitConverter.ToSingle(BitConverter.GetBytes(Marshal.ReadInt32(value)), 0);
        if (selector == 1) item.InterfaceSelector1 = number;
        else if (selector == 2) item.InterfaceSelector2 = number;
        item.Calls.Add("iface-set:" + iid.ToString("x") + ":" + selector.ToString("x") + ":" + number.ToString("R", System.Globalization.CultureInfo.InvariantCulture));
        active.Trace.Add("iface-set:" + item.Key.ToString("x8") + ":" + iid.ToString("x") + ":" + selector.ToString("x") + ":" + number.ToString("R", System.Globalization.CultureInfo.InvariantCulture));
        return 1;
    }
    private static float SequenceValue(float[] sequence, int index, float fallback) {
        if (sequence == null || sequence.Length == 0) return fallback;
        return sequence[Math.Min(index, sequence.Length - 1)];
    }

    private static int Main() {
        if (IntPtr.Size != 4) { Console.Error.WriteLine("Build with /platform:x86."); return 2; }
        string path = Environment.GetEnvironmentVariable("FPARKAN_ORIGINAL_BEHAVIOR_DLL");
        if (String.IsNullOrEmpty(path)) path = @"C:\GOG Games\Parkan - Iron Strategy\Behavior.dll";
        if (!File.Exists(path)) { Console.Error.WriteLine("Set FPARKAN_ORIGINAL_BEHAVIOR_DLL to the installed Behavior.dll."); return 3; }
        string hash = Hash(path);
        if (hash != ExpectedBehaviorSha256) { Console.Error.WriteLine("Unexpected Behavior.dll SHA-256: " + hash); return 4; }
        IntPtr module = LoadLibraryEx(path, IntPtr.Zero, LoadWithAlteredSearchPath);
        if (module == IntPtr.Zero) { Console.Error.WriteLine("LoadLibraryEx(Behavior.dll) failed: " + Marshal.GetLastWin32Error()); return 5; }
        IntPtr import = IntPtr.Add(module, GetSystemArealMapIatRva);
        IntPtr originalImport = Marshal.ReadIntPtr(import);
        uint oldProtection = 0;
        bool protectionChanged = false;
        bool thunksAllocated = false;
        try {
            fastcallThunks = VirtualAlloc(IntPtr.Zero, new UIntPtr(0x1000), MemCommitReserve, PageExecuteReadWrite);
            if (fastcallThunks == IntPtr.Zero) throw new InvalidOperationException("VirtualAlloc(fastcall callback thunks) failed: " + Marshal.GetLastWin32Error());
            thunksAllocated = true;
            queryObjectThunk = WriteFastcallToStdcallThunk(0x00, Marshal.GetFunctionPointerForDelegate(ObjectQueryCallback));
            getInterfacePropertyThunk = WriteFastcallToStdcallThunk(0x40, Marshal.GetFunctionPointerForDelegate(InterfaceGetCallback));
            setInterfacePropertyThunk = WriteFastcallToStdcallThunk(0x80, Marshal.GetFunctionPointerForDelegate(InterfaceSetCallback));
            if (!VirtualProtect(import, new UIntPtr((uint)IntPtr.Size), PageReadWrite, out oldProtection))
                throw new InvalidOperationException("VirtualProtect(GetSystemArealMap IAT) failed: " + Marshal.GetLastWin32Error());
            protectionChanged = true;
            Marshal.WriteIntPtr(import, Marshal.GetFunctionPointerForDelegate(AreaMapGetterCallback));
            RunSmokeCases(module);
            Console.WriteLine("original Behavior.dll distributor tick oracle passed (RVA 0x19e80; only GetSystemArealMap IAT substituted; this does not compare Rust parity).");
            return 0;
        }
        catch (Exception error) {
            Console.Error.WriteLine(error.ToString());
            return 6;
        }
        finally {
            if (protectionChanged) {
                Marshal.WriteIntPtr(import, originalImport);
                uint ignored;
                VirtualProtect(import, new UIntPtr((uint)IntPtr.Size), oldProtection, out ignored);
            }
            GC.KeepAlive(AreaMapGetterCallback); GC.KeepAlive(MapLookupCallback);
            GC.KeepAlive(ObjectStateCallback); GC.KeepAlive(ObjectClassCallback);
            GC.KeepAlive(ObjectQueryCallback); GC.KeepAlive(ObjectRateCallback);
            GC.KeepAlive(ObjectGetPropertyCallback); GC.KeepAlive(ObjectSetPropertyCallback);
            GC.KeepAlive(InterfaceGetCallback); GC.KeepAlive(InterfaceSetCallback);
            if (thunksAllocated && !VirtualFree(fastcallThunks, UIntPtr.Zero, MemRelease))
                throw new InvalidOperationException("VirtualFree(fastcall callback thunks) failed: " + Marshal.GetLastWin32Error());
            FreeLibrary(module);
        }
    }

    private static void RunSmokeCases(IntPtr module) {
        TickDelegate tick = (TickDelegate)Marshal.GetDelegateForFunctionPointer(IntPtr.Add(module, TickRva), typeof(TickDelegate));
        using (Fixture empty = NewFixture()) {
            empty.MakeDistributor(); active = empty; tick(empty.Distributor, 0.0f);
            Expect(empty.Calls.Count == 0, "zero delta performs no manager lookups");
            Console.WriteLine("zero-delta: lookups=0");
        }
        RunObjectCase(tick, 0x80000002, 0.5f, 2.0f, 0.0f, 0, 0.0f, "power-80000002");
        RunObjectCase(tick, 0x80010000, 0.5f, 2.0f, 0.0f, 0, 0.0f, "power-80010000");
        RunObjectCase(tick, 0x80020000, 0.5f, 2.0f, 0.0f, 0, 0.0f, "power-80020000");
        RunObjectCase(tick, 0x80040000, 0.5f, 2.0f, 0.0f, 0, 0.0f, "power-80040000");
        RunObjectCase(tick, 0x80000004, 0.5f, 0.0f, 3.0f, 0, 2.0f, "ore-80000004");
        RunObjectCase(tick, 0x80000008, 0.5f, 0.0f, 3.0f, 0, 2.0f, "ore-80000008");
        RunMixedOreCase(tick, "mixed-ore-producer-recipient", 1.0f, 0.5f);
        RunMixedOreCase(tick, "bounded-ore-store", 0.25f, 0.5f);
        RunWeightedOreCase(tick);
        RunMixedPowerCase(tick, "mixed-power-sufficient", 4.0f, 1.0f, 0.5f, 0.75f);
        RunMixedPowerCase(tick, "mixed-power-shortage", 0.125f, 1.0f, 0.5f, 0.75f);
        RunMixedPowerCase(tick, "mixed-power-recipient-reserve", 4.0f, 8.0f, 0.5f, 0.75f);
        RunMissingCase(tick);
        RunSkippedCase(tick);
    }

    private static Fixture NewFixture() {
        Fixture fixture = new Fixture();
        fixture.ManagerVtable = Marshal.AllocHGlobal(0x60); fixture.Allocations.Add(fixture.ManagerVtable); Zero(fixture.ManagerVtable, 0x60);
        fixture.Manager = Marshal.AllocHGlobal(0x80); fixture.Allocations.Add(fixture.Manager); Zero(fixture.Manager, 0x80);
        Marshal.WriteIntPtr(fixture.Manager, 0, fixture.ManagerVtable);
        Marshal.WriteIntPtr(fixture.ManagerVtable, 0x54, Marshal.GetFunctionPointerForDelegate(MapLookupCallback));
        return fixture;
    }

    private static void RunObjectCase(TickDelegate tick, uint classCode, float delta, float powerRate, float oreRate, int state, float property, string name) {
        using (Fixture fixture = NewFixture()) {
            NativeObject item = fixture.AddObject(0x100 + classCode, classCode, powerRate, oreRate, property, state);
            fixture.MakeDistributor(item.Key); active = fixture;
            tick(fixture.Distributor, delta);
            uint[] fields = ReadEntry(fixture.Entries);
            Console.WriteLine(name + ": trace=" + String.Join(",", fixture.Trace.ToArray()) +
                "; entry=[" + Hex(fields[0]) + "," + Float(fields[1]) + "," + Hex(fields[2]) + "," + Float(fields[3]) + "," + Hex(fields[4]) + "," + Float(fields[5]) + "," + Hex(fields[6]) +
                "]; totals=[" + Float(ReadFloat(fixture.Distributor,0x30)) + "," + Float(ReadFloat(fixture.Distributor,0x34)) + "," + Float(ReadFloat(fixture.Distributor,0x38)) + "," + Float(ReadFloat(fixture.Distributor,0x3c)) + "," + Float(ReadFloat(fixture.Distributor,0x40)) +
                "]; property=" + Float(item.Property) + " writes=" + item.Writes + " managerLookups=" + fixture.LookupCount);
            Expect(fixture.LookupCount == 2, name + " is looked up in both native passes");
            if (classCode == 0x80000002 || classCode == 0x80010000 || classCode == 0x80020000 || classCode == 0x80040000) {
                Expect(ReadFloat(fixture.Distributor, 0x30) == 1.0f && ReadFloat(fixture.Distributor, 0x34) == 0.0f &&
                    ReadFloat(fixture.Distributor, 0x38) == 0.0f && ReadFloat(fixture.Distributor, 0x3c) == 0.0f &&
                    ReadFloat(fixture.Distributor, 0x40) == 0.0f && item.Property == property && item.Writes == 2 &&
                    item.PowerRateReads == 1, name + " native power-rate branch totals and writes");
                Expect(fields[1] == FloatBits(1.0f) && fields[3] == FloatBits(0.0f) && fields[4] == 0 &&
                    fields[5] == FloatBits(0.0f) && fields[6] == 0, name + " native power row fields");
                ExpectOrderedCalls(fixture.Trace, "rate:" + item.Key.ToString("x8") + ":1005:2",
                    "lookup:" + item.Key.ToString("x8"), "iface-set:" + item.Key.ToString("x8") + ":204:1:1");
            } else if (classCode == 0x80000004 || classCode == 0x80000008) {
                Expect(ReadFloat(fixture.Distributor, 0x30) == 0.0f && ReadFloat(fixture.Distributor, 0x34) == 1.5f &&
                    ReadFloat(fixture.Distributor, 0x38) == 2.0f && ReadFloat(fixture.Distributor, 0x3c) == 0.0f &&
                    ReadFloat(fixture.Distributor, 0x40) == 0.0f && item.Property == property && item.Writes == 1 &&
                    item.OreRateReads == 2, name + " native ore-rate branch totals and writes");
                Expect(fields[1] == FloatBits(1.0f) && fields[3] == 0 && fields[4] == 0 &&
                    fields[5] == 0 && fields[6] == 0, name + " native ore row fields");
                ExpectOrderedCalls(fixture.Trace, "rate:" + item.Key.ToString("x8") + ":1002:3",
                    "lookup:" + item.Key.ToString("x8"), "rate:" + item.Key.ToString("x8") + ":1002:3",
                    "set:" + item.Key.ToString("x8") + ":02000100:2");
            }
        }
    }

    private static void RunMissingCase(TickDelegate tick) {
        using (Fixture fixture = NewFixture()) {
            fixture.MakeDistributor(0xdeadbeef); active = fixture; tick(fixture.Distributor, 1.0f);
            Expect(fixture.LookupCount == 2, "one missing list item is looked up once in each native pass");
            Console.WriteLine("missing-map-entry: managerLookups=" + fixture.LookupCount + " trace=" + String.Join(",", fixture.Trace.ToArray()));
        }
    }

    private static void RunMixedOreCase(TickDelegate tick, string label, float producerProperty, float recipientWeight) {
        using (Fixture fixture = NewFixture()) {
            NativeObject producer = fixture.AddObject(0x41, 0x80000004, 0.0f, 4.0f, producerProperty, 0);
            producer.OreRateSequence = new float[] { 4.0f, 1.0f };
            producer.PropertyMetadata = FloatBits(10.0f);
            NativeObject recipient = fixture.AddObject(0x42, 0x80000001, 0.0f, 0.0f, 6.0f, 0);
            recipient.PropertyMetadata = FloatBits(12.0f);
            recipient.InterfaceSelector1 = 0.5f;
            recipient.InterfaceSelector2 = 0.75f;
            fixture.MakeDistributor(producer.Key, recipient.Key);
            Marshal.WriteInt32(fixture.Entries, 0x04, BitConverter.ToInt32(BitConverter.GetBytes(1.0f), 0));
            Marshal.WriteInt32(fixture.Entries, 0x1c + 0x04, BitConverter.ToInt32(BitConverter.GetBytes(recipientWeight), 0));
            active = fixture;
            tick(fixture.Distributor, 1.0f);
            Console.WriteLine(label + ": trace=" + String.Join(",", fixture.Trace.ToArray()) +
                "; totals=[" + Float(ReadFloat(fixture.Distributor, 0x30)) + "," + Float(ReadFloat(fixture.Distributor, 0x34)) + "," + Float(ReadFloat(fixture.Distributor, 0x38)) + "," + Float(ReadFloat(fixture.Distributor, 0x3c)) + "," + Float(ReadFloat(fixture.Distributor, 0x40)) +
                "]; entries=[" + FormatEntry(fixture.Entries) + ";" + FormatEntry(IntPtr.Add(fixture.Entries, 0x1c)) +
                "]; producer(property=" + Float(producer.Property) + ",metadata=" + producer.PropertyMetadata + ",writes=" + producer.Writes + ",1002reads=" + producer.OreRateReads + "); recipient(property=" + Float(recipient.Property) + ",metadata=" + recipient.PropertyMetadata + ",writes=" + recipient.Writes + "); managerLookups=" + fixture.LookupCount);
            if (producerProperty == 1.0f && recipientWeight == 0.5f) {
                Expect(ReadFloat(fixture.Distributor, 0x30) == 0.0f && ReadFloat(fixture.Distributor, 0x34) == 1.0f &&
                    ReadFloat(fixture.Distributor, 0x38) == 1.0f && ReadFloat(fixture.Distributor, 0x3c) == 0.375f &&
                    ReadFloat(fixture.Distributor, 0x40) == 6.0f, "mixed vector native totals");
                Expect(producer.Property == 0.0f && recipient.Property == 7.0f && producer.OreRateReads == 2 &&
                    producer.PropertyMetadata == FloatBits(10.0f) && recipient.PropertyMetadata == FloatBits(12.0f) &&
                    producer.Writes == 1 && recipient.Writes == 1 && fixture.LookupCount == 4, "mixed vector native stores and two-pass selector reads");
                uint[] recipientEntry = ReadEntry(IntPtr.Add(fixture.Entries, 0x1c));
                Expect(recipientEntry[3] == FloatBits(0.375f) && recipientEntry[5] == FloatBits(6.0f) &&
                    recipientEntry[6] == 0x3f800000, "mixed vector per-item native fields");
                ExpectLookupOrder(fixture, "00000041", "00000042", "00000041", "00000042");
                ExpectOrderedCalls(fixture.Trace,
                    "rate:00000041:1002:4",
                    "lookup:00000042",
                    "rate:00000041:1002:1",
                    "set:00000041:02000100:0",
                    "lookup:00000042",
                    "set:00000042:02000100:7");
            } else if (producerProperty == 0.25f && recipientWeight == 0.5f) {
                Expect(ReadFloat(fixture.Distributor, 0x30) == 0.0f && ReadFloat(fixture.Distributor, 0x34) == 0.25f &&
                    ReadFloat(fixture.Distributor, 0x38) == 0.25f && ReadFloat(fixture.Distributor, 0x3c) == 0.375f &&
                    ReadFloat(fixture.Distributor, 0x40) == 6.0f, "bounded ore native totals");
                Expect(producer.Property == 0.0f && recipient.Property == 6.25f && producer.OreRateReads == 2 &&
                    producer.PropertyMetadata == FloatBits(10.0f) && recipient.PropertyMetadata == FloatBits(12.0f) &&
                    producer.Writes == 1 && recipient.Writes == 1 && fixture.LookupCount == 4,
                    "bounded ore stores are limited by producer stock");
                uint[] recipientEntry = ReadEntry(IntPtr.Add(fixture.Entries, 0x1c));
                Expect(recipientEntry[3] == FloatBits(0.375f) && recipientEntry[5] == FloatBits(6.0f) &&
                    recipientEntry[6] == FloatBits(0.25f), "bounded ore per-item native fields");
                ExpectLookupOrder(fixture, "00000041", "00000042", "00000041", "00000042");
                ExpectOrderedCalls(fixture.Trace,
                    "rate:00000041:1002:4",
                    "lookup:00000042",
                    "rate:00000041:1002:1",
                    "set:00000041:02000100:0",
                    "lookup:00000042",
                    "set:00000042:02000100:6.25");
            }
        }
    }

    private static void RunWeightedOreCase(TickDelegate tick) {
        using (Fixture fixture = NewFixture()) {
            NativeObject producer = fixture.AddObject(0x43, 0x80000004, 0.0f, 4.0f, 1.0f, 0);
            producer.OreRateSequence = new float[] { 4.0f, 1.0f };
            producer.PropertyMetadata = FloatBits(10.0f);
            NativeObject zeroWeight = fixture.AddObject(0x44, 0x80000001, 0.0f, 0.0f, 6.0f, 0);
            zeroWeight.PropertyMetadata = FloatBits(12.0f);
            NativeObject partialWeight = fixture.AddObject(0x45, 0x80000001, 0.0f, 0.0f, 8.0f, 0);
            partialWeight.PropertyMetadata = FloatBits(16.0f);
            partialWeight.InterfaceSelector1 = 0.25f;
            partialWeight.InterfaceSelector2 = 0.5f;
            fixture.MakeDistributor(producer.Key, zeroWeight.Key, partialWeight.Key);
            Marshal.WriteInt32(fixture.Entries, 0x04, FloatBits(1.0f));
            Marshal.WriteInt32(fixture.Entries, 0x1c + 0x04, FloatBits(0.0f));
            Marshal.WriteInt32(fixture.Entries, 0x38 + 0x04, FloatBits(0.25f));
            active = fixture;
            tick(fixture.Distributor, 1.0f);
            Console.WriteLine("weighted-ore-recipient: trace=" + String.Join(",", fixture.Trace.ToArray()) +
                "; totals=[" + Float(ReadFloat(fixture.Distributor, 0x30)) + "," + Float(ReadFloat(fixture.Distributor, 0x34)) + "," + Float(ReadFloat(fixture.Distributor, 0x38)) + "," + Float(ReadFloat(fixture.Distributor, 0x3c)) + "," + Float(ReadFloat(fixture.Distributor, 0x40)) +
                "]; entries=[" + FormatEntry(fixture.Entries) + ";" + FormatEntry(IntPtr.Add(fixture.Entries,0x1c)) + ";" + FormatEntry(IntPtr.Add(fixture.Entries,0x38)) +
                "]; properties=[" + Float(producer.Property) + "," + Float(zeroWeight.Property) + "," + Float(partialWeight.Property) + "]; reads1002=" + producer.OreRateReads + " lookups=" + fixture.LookupCount);
            Expect(ReadFloat(fixture.Distributor, 0x30) == 0.0f && ReadFloat(fixture.Distributor, 0x34) == 1.0f &&
                ReadFloat(fixture.Distributor, 0x38) == 1.0f && ReadFloat(fixture.Distributor, 0x3c) == 0.375f &&
                ReadFloat(fixture.Distributor, 0x40) == 14.0f, "weighted ore native totals");
            Expect(producer.Property == 0.0f && zeroWeight.Property == 6.0f && partialWeight.Property == 9.0f &&
                producer.OreRateReads == 2 && producer.Writes == 1 && zeroWeight.Writes == 1 && partialWeight.Writes == 1 &&
                fixture.LookupCount == 6, "weighted ore changes only the positive-weight recipient and preserves total stock");
            uint[] zeroEntry = ReadEntry(IntPtr.Add(fixture.Entries, 0x1c));
            uint[] partialEntry = ReadEntry(IntPtr.Add(fixture.Entries, 0x38));
            Expect(zeroEntry[1] == FloatBits(0.0f) && zeroEntry[3] == FloatBits(0.0f) &&
                zeroEntry[5] == FloatBits(6.0f) && partialEntry[1] == FloatBits(0.25f) &&
                partialEntry[3] == FloatBits(0.375f) && partialEntry[5] == FloatBits(8.0f) &&
                partialEntry[6] == FloatBits(1.0f), "weighted ore per-item native fields");
            ExpectLookupOrder(fixture, "00000043", "00000044", "00000045", "00000043", "00000044", "00000045");
            ExpectOrderedCalls(fixture.Trace,
                "rate:00000043:1002:4",
                "lookup:00000044",
                "lookup:00000045",
                "rate:00000043:1002:1",
                "set:00000043:02000100:0",
                "set:00000044:02000100:6",
                "set:00000045:02000100:9");
        }
    }

    private static void RunMixedPowerCase(TickDelegate tick, string label, float powerRate, float recipientProperty, float selector1, float selector2) {
        using (Fixture fixture = NewFixture()) {
            NativeObject producer = fixture.AddObject(0x61, 0x80000002, powerRate, 0.0f, 1.0f, 0);
            producer.PropertyMetadata = FloatBits(10.0f);
            NativeObject recipient = fixture.AddObject(0x62, 0x80000001, 0.0f, 0.0f, recipientProperty, 0);
            recipient.PropertyMetadata = FloatBits(10.0f);
            recipient.InterfaceSelector1 = selector1;
            recipient.InterfaceSelector2 = selector2;
            fixture.MakeDistributor(producer.Key, recipient.Key);
            Marshal.WriteInt32(fixture.Entries, 0x04, FloatBits(1.0f));
            Marshal.WriteInt32(fixture.Entries, 0x1c + 0x04, FloatBits(0.5f));
            active = fixture;
            tick(fixture.Distributor, 1.0f);
            Console.WriteLine(label + ": trace=" + String.Join(",", fixture.Trace.ToArray()) +
                "; totals=[" + Float(ReadFloat(fixture.Distributor, 0x30)) + "," + Float(ReadFloat(fixture.Distributor, 0x34)) + "," + Float(ReadFloat(fixture.Distributor, 0x38)) + "," + Float(ReadFloat(fixture.Distributor, 0x3c)) + "," + Float(ReadFloat(fixture.Distributor, 0x40)) +
                "]; entries=[" + FormatEntry(fixture.Entries) + ";" + FormatEntry(IntPtr.Add(fixture.Entries, 0x1c)) +
                "]; producer(property=" + Float(producer.Property) + ",metadata=" + producer.PropertyMetadata + ",writes=" + producer.Writes + "); recipient(property=" + Float(recipient.Property) + ",metadata=" + recipient.PropertyMetadata + ",writes=" + recipient.Writes + "); managerLookups=" + fixture.LookupCount);
            bool shortage = label == "mixed-power-shortage";
            bool reserve = label == "mixed-power-recipient-reserve";
            float expectedAvailable = powerRate;
            float expectedRecipient = recipientProperty;
            float expectedSelector = shortage ? 2.0f / 3.0f : 1.0f;
            Expect(ReadFloat(fixture.Distributor, 0x30) == expectedAvailable &&
                ReadFloat(fixture.Distributor, 0x34) == 0.0f && ReadFloat(fixture.Distributor, 0x38) == 0.0f &&
                ReadFloat(fixture.Distributor, 0x3c) == 0.375f &&
                ReadFloat(fixture.Distributor, 0x40) == (reserve ? 11.0f : 18.0f), label + " native power totals");
            Expect(producer.Property == 1.0f && recipient.Property == expectedRecipient &&
                producer.Writes == 1 && recipient.Writes == 1 && producer.PowerRateReads == 1 &&
                fixture.LookupCount == 4 && recipient.InterfaceSelector1 == expectedSelector,
                label + " native resource and interface fields");
            uint[] sourceEntry = ReadEntry(fixture.Entries);
            uint[] recipientEntry = ReadEntry(IntPtr.Add(fixture.Entries, 0x1c));
            Expect(sourceEntry[1] == FloatBits(1.0f) && sourceEntry[5] == FloatBits(9.0f) &&
                recipientEntry[1] == FloatBits(0.5f) && recipientEntry[3] == FloatBits(0.375f) &&
                recipientEntry[4] == FloatBits(shortage ? 0.125f : 0.375f) &&
                recipientEntry[5] == FloatBits(reserve ? 2.0f : 9.0f), label + " native per-entry values");
            Expect(FloatBits(recipient.InterfaceSelector1) == (shortage ? unchecked((int)0x3f2aaaab) : FloatBits(1.0f)),
                label + " native interface selector bits");
            ExpectLookupOrder(fixture, "00000061", "00000062", "00000061", "00000062");
            ExpectOrderedCalls(fixture.Trace,
                "rate:00000061:1005:" + Float(powerRate),
                "lookup:00000062",
                "lookup:00000061",
                "iface-set:00000061:204:1:1",
                "set:00000061:02000100:1",
                "lookup:00000062",
                "iface-set:00000062:204:1:" + Float(expectedSelector),
                "set:00000062:02000100:" + Float(expectedRecipient));
        }
    }

    private static void RunSkippedCase(TickDelegate tick) {
        using (Fixture fixture = NewFixture()) {
            NativeObject item = fixture.AddObject(0x55, 0x80000002, 1.0f, 0.0f, 0.0f, 0xfffe);
            fixture.MakeDistributor(item.Key); active = fixture; tick(fixture.Distributor, 1.0f);
            string expectedTrace = "lookup:00000055,state:00000055,lookup:00000055,state:00000055";
            Expect(String.Join(",", fixture.Trace.ToArray()) == expectedTrace,
                "0xfffe state is looked up in both passes and skips class and interface queries");
            Expect(item.Calls.Count == 0, "0xfffe state skips all object vtable slots beyond state");
            Console.WriteLine("state-fffe: trace=" + String.Join(",", fixture.Trace.ToArray()) + "; managerLookups=" + fixture.LookupCount);
        }
    }

    private static uint[] ReadEntry(IntPtr entry) {
        uint[] words = new uint[7];
        for (int i = 0; i < words.Length; i++) words[i] = unchecked((uint)Marshal.ReadInt32(entry, i * 4));
        return words;
    }
    private static string FormatEntry(IntPtr entry) {
        uint[] words = ReadEntry(entry);
        return Hex(words[0]) + ",w1=" + Float(words[1]) + ",w2=" + Hex(words[2]) + ",w3=" + Float(words[3]) + ",w4=" + Hex(words[4]) + ",w5=" + Float(words[5]) + ",w6=" + Hex(words[6]);
    }
    private static IntPtr WriteFastcallToStdcallThunk(int offset, IntPtr stdcallTarget) {
        // Adapt ECX=self, EDX=selector/IID, stack arg3 to a managed stdcall (self, selector, arg3).
        byte[] code = { 0xff, 0x74, 0x24, 0x04, 0x52, 0x51, 0xb8, 0, 0, 0, 0, 0xff, 0xd0, 0xc2, 0x04, 0x00 };
        WriteImm32(code, 7, stdcallTarget.ToInt32());
        IntPtr thunk = IntPtr.Add(fastcallThunks, offset);
        Marshal.Copy(code, 0, thunk, code.Length);
        return thunk;
    }
    private static void WriteImm32(byte[] code, int offset, int value) {
        byte[] bytes = BitConverter.GetBytes(value);
        Buffer.BlockCopy(bytes, 0, code, offset, 4);
    }
    private static float ReadFloat(IntPtr address, int offset) { return BitConverter.ToSingle(BitConverter.GetBytes(Marshal.ReadInt32(address, offset)), 0); }
    private static string Float(uint bits) { return Float(BitConverter.ToSingle(BitConverter.GetBytes(unchecked((int)bits)), 0)); }
    private static int FloatBits(float value) { return BitConverter.ToInt32(BitConverter.GetBytes(value), 0); }
    private static string Float(float value) { return value.ToString("R", System.Globalization.CultureInfo.InvariantCulture); }
    private static string Hex(uint value) { return "0x" + value.ToString("x8"); }
    private static string Hash(string path) {
        using (SHA256 sha = SHA256.Create()) using (FileStream input = File.OpenRead(path)) {
            byte[] digest = sha.ComputeHash(input); StringBuilder hex = new StringBuilder();
            foreach (byte value in digest) hex.Append(value.ToString("x2")); return hex.ToString();
        }
    }
    private static void Expect(bool value, string message) { if (!value) throw new InvalidOperationException("original Behavior tick oracle failed: " + message); }
    private static void ExpectLookupOrder(Fixture fixture, params string[] keys) {
        List<string> actual = new List<string>();
        foreach (string item in fixture.Trace) if (item.StartsWith("lookup:", StringComparison.Ordinal)) actual.Add(item.Substring(7));
        Expect(String.Join(",", actual.ToArray()) == String.Join(",", keys), "native lookup order [" + String.Join(",", actual.ToArray()) + "]");
    }
    private static void ExpectOrderedCalls(List<string> trace, params string[] expected) {
        int previous = -1;
        foreach (string call in expected) {
            int index = trace.IndexOf(call, previous + 1);
            Expect(index >= 0, "native call order missing " + call);
            previous = index;
        }
    }
    private static void Zero(IntPtr buffer, int length) { for (int offset = 0; offset < length; offset++) Marshal.WriteByte(buffer, offset, 0); }
}
