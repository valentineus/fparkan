using System;
using System.Collections.Generic;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;

// Calls the original x86 Behavior.dll distributor Add, normalize, indexed resource
// property, and tick paths against synthetic storage. Only GetSystemArealMap and the
// bounded vector backing are substituted; Behavior's corresponding instructions
// remain the installed DLL.
internal static class BehaviorDistributorTickOracle {
    private const uint LoadWithAlteredSearchPath = 0x00000008;
    private const uint MemCommitReserve = 0x00003000;
    private const uint PageExecuteReadWrite = 0x40;
    private const uint MemRelease = 0x00008000;
    private const uint PageReadWrite = 0x04;
    private const string ExpectedBehaviorSha256 = "ddfbdbde07832358fe6e35e01c8dba9b58e7313789498b6049ed51d4ed9baf99";
    private const int TickRva = 0x19e80;
    private const int AddObjectRva = 0x1aac0;
    private const int NormalizeRva = 0x1ac90;
    private const int IndexedPropertySetRva = 0x0b470;
    private const int IndexedPropertyGetRva = 0x0b5b0;
    private const int OreInitializationRva = 0x08790;
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
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate void X87ControlWordReader();
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate void X87ControlWordWriter();
    [UnmanagedFunctionPointer(CallingConvention.ThisCall)] private delegate void TickDelegate(IntPtr self, float deltaSeconds);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate void NativeAddObject(IntPtr self, uint key);
    [UnmanagedFunctionPointer(CallingConvention.ThisCall)] private delegate void NativeNormalize(IntPtr self, uint key);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate void VectorInsert(IntPtr self, IntPtr source, uint count, uint position);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate IntPtr MapLookup(IntPtr self, uint key);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate int ObjectState(IntPtr self);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate uint ObjectClass(IntPtr self);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate int ObjectQueryInterface(IntPtr self, uint iid, IntPtr result);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate IntPtr ObjectRate(IntPtr self, uint selector);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate float ObjectGetProperty(IntPtr self, uint property, IntPtr result);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate void ObjectSetProperty(IntPtr self, uint property, float value, IntPtr result);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate int InterfaceGetProperty(IntPtr self, uint selector, IntPtr result);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate int InterfaceSetProperty(IntPtr self, uint selector, IntPtr value);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate void IndexedPropertySet(IntPtr self, uint index, uint valueBits);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate int IndexedPropertyGet(IntPtr self, uint index, IntPtr resultBits);
    [UnmanagedFunctionPointer(CallingConvention.ThisCall)] private delegate void NativeOreInitialization(IntPtr self);

    private static readonly AreaMapGetter AreaMapGetterCallback = GetAreaMap;
    private static readonly VectorInsert VectorInsertCallback = InsertVector;
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
    private static IntPtr vectorInsertThunk;
    private static IntPtr x87ControlWordCell;
    private static X87ControlWordReader x87ControlWordReader;
    private static X87ControlWordWriter x87ControlWordWriter;

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
        public IntPtr VectorVtable;
        public IntPtr Manager;
        public IntPtr ManagerVtable;
        public readonly List<IntPtr> Allocations = new List<IntPtr>();
        public readonly Dictionary<uint, NativeObject> ByKey = new Dictionary<uint, NativeObject>();
        public readonly List<string> Calls = new List<string>();
        public readonly List<string> Trace = new List<string>();
        public int LookupCount;
        public int VectorInsertCalls;
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
            const int capacity = 32;
            if (keys.Length > capacity) throw new InvalidOperationException("synthetic distributor capacity exceeded");
            Entries = Allocate(capacity * 0x1c);
            VectorVtable = Allocate(0x20);
            Marshal.WriteIntPtr(VectorVtable, 0x10, vectorInsertThunk);
            Marshal.WriteIntPtr(Distributor, 0x1c, VectorVtable);
            Marshal.WriteInt32(Distributor, 0x20, keys.Length);
            Marshal.WriteInt32(Distributor, 0x24, capacity);
            Marshal.WriteIntPtr(Distributor, 0x28, Entries);
            for (int i = 0; i < keys.Length; i++) {
                IntPtr entry = IntPtr.Add(Entries, i * 0x1c);
                Marshal.WriteInt32(entry, 0x00, unchecked((int)keys[i]));
                Marshal.WriteInt32(entry, 0x04, BitConverter.ToInt32(BitConverter.GetBytes(1.0f), 0));
            }
        }
        public IntPtr Allocate(int size) {
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
    private static void InsertVector(IntPtr self, IntPtr source, uint count, uint position) {
        int length = Marshal.ReadInt32(self, 4);
        int capacity = Marshal.ReadInt32(self, 8);
        IntPtr destination = Marshal.ReadIntPtr(self, 12);
        Expect(count > 0 && count <= (uint)(capacity - length), "bounded vector insertion capacity");
        Expect(position == 0xffffffff || position == (uint)length, "native append position");
        byte[] entry = new byte[0x1c];
        for (uint index = 0; index < count; index++) {
            Marshal.Copy(IntPtr.Add(source, unchecked((int)(index * 0x1c))), entry, 0, entry.Length);
            Marshal.Copy(entry, 0, IntPtr.Add(destination, (length + (int)index) * 0x1c), entry.Length);
        }
        Marshal.WriteInt32(self, 4, length + (int)count);
        active.VectorInsertCalls++;
        active.Trace.Add("insert:" + count.ToString() + ":" + position.ToString("x8"));
    }
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
            vectorInsertThunk = WriteThisCallThreeArgsToStdcallThunk(0xc0, Marshal.GetFunctionPointerForDelegate(VectorInsertCallback));
            x87ControlWordCell = Marshal.AllocHGlobal(2);
            Marshal.WriteInt16(x87ControlWordCell, 0);
            x87ControlWordReader = (X87ControlWordReader)Marshal.GetDelegateForFunctionPointer(WriteX87ControlWordReader(0x100), typeof(X87ControlWordReader));
            x87ControlWordWriter = (X87ControlWordWriter)Marshal.GetDelegateForFunctionPointer(WriteX87ControlWordWriter(0x120), typeof(X87ControlWordWriter));
            if (!VirtualProtect(import, new UIntPtr((uint)IntPtr.Size), PageReadWrite, out oldProtection))
                throw new InvalidOperationException("VirtualProtect(GetSystemArealMap IAT) failed: " + Marshal.GetLastWin32Error());
            protectionChanged = true;
            Marshal.WriteIntPtr(import, Marshal.GetFunctionPointerForDelegate(AreaMapGetterCallback));
            RunSmokeCases(module);
            Console.WriteLine("original Behavior.dll distributor oracle passed (native Add 0x1aac0, normalize 0x1ac90, tick 0x19e80; only GetSystemArealMap IAT and bounded vector insertion are substituted; this does not compare Rust parity).");
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
            GC.KeepAlive(AreaMapGetterCallback); GC.KeepAlive(VectorInsertCallback); GC.KeepAlive(x87ControlWordReader); GC.KeepAlive(x87ControlWordWriter); GC.KeepAlive(MapLookupCallback);
            GC.KeepAlive(ObjectStateCallback); GC.KeepAlive(ObjectClassCallback);
            GC.KeepAlive(ObjectQueryCallback); GC.KeepAlive(ObjectRateCallback);
            GC.KeepAlive(ObjectGetPropertyCallback); GC.KeepAlive(ObjectSetPropertyCallback);
            GC.KeepAlive(InterfaceGetCallback); GC.KeepAlive(InterfaceSetCallback);
            if (thunksAllocated && !VirtualFree(fastcallThunks, UIntPtr.Zero, MemRelease))
                throw new InvalidOperationException("VirtualFree(fastcall callback thunks) failed: " + Marshal.GetLastWin32Error());
            if (x87ControlWordCell != IntPtr.Zero) { Marshal.FreeHGlobal(x87ControlWordCell); x87ControlWordCell = IntPtr.Zero; }
            FreeLibrary(module);
        }
    }

    private static void RunSmokeCases(IntPtr module) {
        ushort savedControlWord = CurrentX87ControlWord();
        Console.WriteLine("x87-control-word-before=0x" + savedControlWord.ToString("x4") + " (x86 oracle process)");
        try {
            SetX87ControlWord(0x007f);
            Console.WriteLine("native-add-normalize-x87-control-word=0x" + CurrentX87ControlWord().ToString("x4"));
            RunNativeAddCases(module);
            RunNativeNormalizeCases(module);
        } finally {
            SetX87ControlWord(savedControlWord);
            Console.WriteLine("x87-control-word-restored=0x" + CurrentX87ControlWord().ToString("x4"));
        }
        RunNativeIndexedPropertyCases(module);
        RunNativeOreInitializationCases(module);
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

    private static void RunNativeIndexedPropertyCases(IntPtr module) {
        IndexedPropertySet set = (IndexedPropertySet)Marshal.GetDelegateForFunctionPointer(
            IntPtr.Add(module, IndexedPropertySetRva), typeof(IndexedPropertySet));
        IndexedPropertyGet get = (IndexedPropertyGet)Marshal.GetDelegateForFunctionPointer(
            IntPtr.Add(module, IndexedPropertyGetRva), typeof(IndexedPropertyGet));
        using (Fixture fixture = NewFixture()) {
            // The handlers receive the embedded interface at owner+4. Its descriptor
            // vector is at interface+0xa3c and its keyed property store at interface+0x5e4.
            IntPtr owner = fixture.Allocate(0x0b20);
            IntPtr iface = IntPtr.Add(owner, 4);
            IntPtr descriptors = fixture.Allocate(6 * 0x18);
            IntPtr propertyEntry = fixture.Allocate(0x18);
            IntPtr maximumOutput = fixture.Allocate(sizeof(uint));
            IntPtr currentOutput = fixture.Allocate(sizeof(uint));

            Marshal.WriteInt32(iface, 0xa3c, 6);
            Marshal.WriteIntPtr(iface, 0xa44, descriptors);
            Marshal.WriteInt32(IntPtr.Add(descriptors, 4 * 0x18), 4, 4);
            Marshal.WriteInt32(IntPtr.Add(descriptors, 5 * 0x18), 4, 5);

            Marshal.WriteInt32(iface, 0x5e4, 0); // vector vtable is not used by these callbacks
            Marshal.WriteInt32(iface, 0x5e8, 1); // count
            Marshal.WriteInt32(iface, 0x5ec, 1); // capacity
            Marshal.WriteIntPtr(iface, 0x5f0, propertyEntry);
            Marshal.WriteInt32(propertyEntry, 0x00, unchecked((int)ResourceProperty));
            Marshal.WriteInt32(propertyEntry, 0x04, unchecked((int)0x11223344));
            Marshal.WriteInt32(propertyEntry, 0x08, FloatBits(2500.0f)); // auxiliary maximum
            Marshal.WriteInt32(propertyEntry, 0x0c, unchecked((int)0x55667788));
            Marshal.WriteInt32(propertyEntry, 0x10, unchecked((int)0x99aabbcc));
            Marshal.WriteInt32(propertyEntry, 0x14, FloatBits(12.5f)); // primary current amount

            Expect(get(iface, 4, maximumOutput) == 1 && Marshal.ReadInt32(maximumOutput) == FloatBits(2500.0f),
                "descriptor 4 reads keyed auxiliary value as MaximumOre");
            Expect(get(iface, 5, currentOutput) == 1 && Marshal.ReadInt32(currentOutput) == FloatBits(12.5f),
                "descriptor 5 reads keyed primary float as CurrentOre");

            set(iface, 5, unchecked((uint)FloatBits(21.75f)));
            Expect(Marshal.ReadInt32(propertyEntry, 0x14) == FloatBits(21.75f) &&
                Marshal.ReadInt32(propertyEntry, 0x08) == FloatBits(2500.0f),
                "descriptor 5 updates CurrentOre and preserves MaximumOre auxiliary value");
            set(iface, 4, unchecked((uint)FloatBits(999.0f)));
            Expect(Marshal.ReadInt32(propertyEntry, 0x14) == FloatBits(21.75f) &&
                Marshal.ReadInt32(propertyEntry, 0x08) == FloatBits(2500.0f),
                "descriptor 4 is read-only through the indexed setter");
            Expect(get(iface, 4, maximumOutput) == 1 && Marshal.ReadInt32(maximumOutput) == FloatBits(2500.0f) &&
                get(iface, 5, currentOutput) == 1 && Marshal.ReadInt32(currentOutput) == FloatBits(21.75f),
                "indexed resource values remain independently readable after CurrentOre write");

            IntPtr reorderedDescriptors = fixture.Allocate(2 * 0x18);
            Marshal.WriteInt32(iface, 0xa3c, 2);
            Marshal.WriteIntPtr(iface, 0xa44, reorderedDescriptors);
            Marshal.WriteInt32(reorderedDescriptors, 4, 5);
            Marshal.WriteInt32(IntPtr.Add(reorderedDescriptors, 0x18), 4, 4);
            Expect(get(iface, 0, currentOutput) == 1 && Marshal.ReadInt32(currentOutput) == FloatBits(21.75f),
                "descriptor index zero follows its CurrentOre code 5 row");
            Expect(get(iface, 1, maximumOutput) == 1 && Marshal.ReadInt32(maximumOutput) == FloatBits(2500.0f),
                "descriptor index one follows its MaximumOre code 4 row");
            set(iface, 0, unchecked((uint)FloatBits(31.25f)));
            set(iface, 1, unchecked((uint)FloatBits(999.0f)));
            Expect(Marshal.ReadInt32(propertyEntry, 0x14) == FloatBits(31.25f) &&
                Marshal.ReadInt32(propertyEntry, 0x08) == FloatBits(2500.0f),
                "reordered descriptor rows preserve CurrentOre write and MaximumOre read-only behavior");
            Expect(get(iface, 0, currentOutput) == 1 && Marshal.ReadInt32(currentOutput) == FloatBits(31.25f) &&
                get(iface, 1, maximumOutput) == 1 && Marshal.ReadInt32(maximumOutput) == FloatBits(2500.0f),
                "reordered descriptor rows remain readable after their indexed writes");
            Console.WriteLine("native-indexed-resource-property: maximum=" + Float(unchecked((uint)Marshal.ReadInt32(maximumOutput))) +
                " current=" + Float(unchecked((uint)Marshal.ReadInt32(currentOutput))) +
                "; descriptor4-write=no-op descriptor5-write=primary-only; reordered-indices=verified");
        }
    }

    private static void RunNativeOreInitializationCases(IntPtr module) {
        NativeOreInitialization initialize = (NativeOreInitialization)Marshal.GetDelegateForFunctionPointer(
            IntPtr.Add(module, OreInitializationRva), typeof(NativeOreInitialization));
        uint[][] cases = new uint[][] {
            new uint[] { 0x80000004, 0x00000000, 0x43fa0000 },
            new uint[] { 0x80000008, 0x00000000, 0x457a0000 },
            new uint[] { 0x01002000, 0x00000000, 0x44fa0000 },
            new uint[] { 0x01004000, 0x44fa0000, 0x44fa0000 },
            new uint[] { 0x01008000, 0x00000000, 0x00000000 }
        };
        IntPtr vtable = IntPtr.Add(module, 0x59250);
        foreach (uint[] test in cases) {
            using (Fixture fixture = NewFixture()) {
                IntPtr owner = fixture.Allocate(0x0b20);
                IntPtr propertyEntry = fixture.Allocate(0x18);
                Marshal.WriteIntPtr(owner, vtable);
                Marshal.WriteIntPtr(owner, 0x30, owner);
                Marshal.WriteInt32(owner, 0xa04, 2);
                Marshal.WriteInt32(owner, 0xa64, 1); // suppress the wrapper's deferred side effect
                Marshal.WriteInt32(owner, 0xafc, unchecked((int)test[0]));
                Marshal.WriteInt32(owner, 0x70c, FloatBits(500.0f));
                Marshal.WriteInt32(owner, 0x710, FloatBits(4000.0f));
                Marshal.WriteInt32(owner, 0x720, FloatBits(2000.0f));
                Marshal.WriteInt32(propertyEntry, 0x00, unchecked((int)ResourceProperty));
                Marshal.WriteInt32(propertyEntry, 0x08, unchecked((int)0xbf800000)); // native add default auxiliary
                Marshal.WriteInt32(propertyEntry, 0x14, FloatBits(123.0f)); // prove initialization overwrites primary
                Marshal.WriteInt32(owner, 0x5ec, 1); // property count
                Marshal.WriteInt32(owner, 0x5f0, 1); // property capacity
                Marshal.WriteIntPtr(owner, 0x5f4, propertyEntry);

                initialize(owner);

                Expect(unchecked((uint)Marshal.ReadInt32(propertyEntry, 0x14)) == test[1] &&
                    unchecked((uint)Marshal.ReadInt32(propertyEntry, 0x08)) == test[2],
                    "native ore defaults for class 0x" + test[0].ToString("x8"));
            }
        }
        Console.WriteLine("native-behavior-ore-initialization: classes=80000004,80000008,01002000,01004000,01008000; defaults=verified");
    }

    private static ushort CurrentX87ControlWord() {
        if (x87ControlWordReader == null || x87ControlWordCell == IntPtr.Zero)
            throw new InvalidOperationException("x87 control-word reader is not initialized");
        x87ControlWordReader();
        return unchecked((ushort)Marshal.ReadInt16(x87ControlWordCell));
    }

    private static void SetX87ControlWord(ushort value) {
        if (x87ControlWordWriter == null || x87ControlWordCell == IntPtr.Zero)
            throw new InvalidOperationException("x87 control-word writer is not initialized");
        Marshal.WriteInt16(x87ControlWordCell, unchecked((short)value));
        x87ControlWordWriter();
        Expect(CurrentX87ControlWord() == value, "x87 control word 0x" + value.ToString("x4"));
    }

    private static void AddWithGameControl(NativeAddObject add, IntPtr distributor, uint key) {
        Expect(CurrentX87ControlWord() == 0x007f, "game x87 control word before native Add");
        add(distributor, key);
        Expect(CurrentX87ControlWord() == 0x007f, "game x87 control word after native Add");
    }

    private static void NormalizeWithGameControl(NativeNormalize normalize, IntPtr distributor, uint key) {
        Expect(CurrentX87ControlWord() == 0x007f, "game x87 control word before native normalize");
        normalize(distributor, key);
        Expect(CurrentX87ControlWord() == 0x007f, "game x87 control word after native normalize");
    }

    private static void RunNativeAddCases(IntPtr module) {
        NativeAddObject add = (NativeAddObject)Marshal.GetDelegateForFunctionPointer(IntPtr.Add(module, AddObjectRva), typeof(NativeAddObject));
        using (Fixture fixture = NewFixture()) {
            fixture.MakeDistributor();
            active = fixture;
            AddWithGameControl(add, fixture.Distributor, 0x80000004);
            Expect(Marshal.ReadInt32(fixture.Distributor, 0x20) == 1, "Add inserts a new first key");
            ExpectEntry(fixture.Entries, 0, 0x80000004, 0x3f800000, 0, 0, 0, 0, 0);

            AddWithGameControl(add, fixture.Distributor, 0x80000006);
            Expect(Marshal.ReadInt32(fixture.Distributor, 0x20) == 2, "Add appends a second key");
            ExpectEntry(fixture.Entries, 0, 0x80000004, 0x3f000000, 0, 0, 0, 0, 0);
            ExpectEntry(fixture.Entries, 1, 0x80000006, 0x3f000000, 0, 0, 0, 0, 0);

            WriteEntry(fixture.Entries, 0, 0x80000004, 0.2f, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5);
            AddWithGameControl(add, fixture.Distributor, 0x80000004);
            Expect(Marshal.ReadInt32(fixture.Distributor, 0x20) == 2, "Add on an existing key does not append");
            Expect(fixture.VectorInsertCalls == 2, "Add inserts exactly the two new keys");
            ExpectEntry(fixture.Entries, 0, 0x80000004, 0x3f000000, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5);
            ExpectEntry(fixture.Entries, 1, 0x80000006, 0x3f000000, 0, 0, 0, 0, 0);

            AddWithGameControl(add, fixture.Distributor, 0x80000008);
            Expect(Marshal.ReadInt32(fixture.Distributor, 0x20) == 3, "Add appends a third key");
            Expect(fixture.VectorInsertCalls == 3, "Add inserts each new key exactly once");
            ExpectEntry(fixture.Entries, 0, 0x80000004, 0x3f000000, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5);
            uint[] third = ReadEntry(IntPtr.Add(fixture.Entries, 0x38));
            Console.WriteLine("native-add-third-share: " + Float(third[1]) + " bits=" + Hex(third[1]));
            Expect(third[0] == 0x80000008 && third[2] == 0 && third[3] == 0 && third[4] == 0 && third[5] == 0 && third[6] == 0,
                "Add initializes the third record apart from its observed share");
            ExpectEntry(fixture.Entries, 1, 0x80000006, 0x3e2aaaa8, 0, 0, 0, 0, 0);
            ExpectEntry(fixture.Entries, 2, 0x80000008, 0x3eaaaaab, 0, 0, 0, 0, 0);
            Console.WriteLine("native-add: count=3 inserts=" + fixture.VectorInsertCalls + " trace=" + String.Join(",", fixture.Trace.ToArray()) +
                " entries=[" + FormatEntry(fixture.Entries) + ";" + FormatEntry(IntPtr.Add(fixture.Entries, 0x1c)) + ";" +
                FormatEntry(IntPtr.Add(fixture.Entries, 0x38)) + "]");
        }
    }

    private static void RunNativeNormalizeCases(IntPtr module) {
        NativeNormalize normalize = (NativeNormalize)Marshal.GetDelegateForFunctionPointer(IntPtr.Add(module, NormalizeRva), typeof(NativeNormalize));
        using (Fixture fixture = NewFixture()) {
            fixture.MakeDistributor(0x11, 0x22, 0x33);
            active = fixture;
            WriteEntry(fixture.Entries, 0, 0x11, 0.2f, 0, 0xaaaaaaaa, 0xbbbbbbbb, 0xcccccccc, 0xdddddddd);
            WriteEntry(fixture.Entries, 1, 0x22, 0.3f, 1, 0x11111111, 0x22222222, 0x33333333, 0x44444444);
            WriteEntry(fixture.Entries, 2, 0x33, 0.3f, 0, 0x55555555, 0x66666666, 0x77777777, 0x88888888);
            NormalizeWithGameControl(normalize, fixture.Distributor, 0x11);
            ExpectEntry(fixture.Entries, 0, 0x11, FloatBitsUnsigned(0.2f), 0, 0xaaaaaaaa, 0xbbbbbbbb, 0xcccccccc, 0xdddddddd);
            ExpectEntry(fixture.Entries, 1, 0x22, FloatBitsUnsigned(0.3f), 1, 0x11111111, 0x22222222, 0x33333333, 0x44444444);
            ExpectEntry(fixture.Entries, 2, 0x33, 0x3f000000, 0, 0x55555555, 0x66666666, 0x77777777, 0x88888888);
            Console.WriteLine("native-normalize-pinned: shares=[" + Float(ReadEntry(fixture.Entries)[1]) + "," +
                Float(ReadEntry(IntPtr.Add(fixture.Entries, 0x1c))[1]) + "," +
                Float(ReadEntry(IntPtr.Add(fixture.Entries, 0x38))[1]) + "] flags=[0,1,0]");
        }
        using (Fixture fixture = NewFixture()) {
            fixture.MakeDistributor(0x41, 0x42, 0x43);
            active = fixture;
            WriteEntry(fixture.Entries, 0, 0x41, 0.25f, 0, 0, 0, 0, 0);
            WriteEntry(fixture.Entries, 1, 0x42, 0.0f, 0, 0, 0, 0, 0);
            WriteEntry(fixture.Entries, 2, 0x43, 0.0f, 0, 0, 0, 0, 0);
            NormalizeWithGameControl(normalize, fixture.Distributor, 0x41);
            ExpectEntry(fixture.Entries, 0, 0x41, 0x3e800000, 0, 0, 0, 0, 0);
            ExpectEntry(fixture.Entries, 1, 0x42, 0x3ec00000, 0, 0, 0, 0, 0);
            ExpectEntry(fixture.Entries, 2, 0x43, 0x3ec00000, 0, 0, 0, 0, 0);
            Console.WriteLine("native-normalize-zero-free-sum: shares=[0.25,0.375,0.375]");
        }
        using (Fixture fixture = NewFixture()) {
            fixture.MakeDistributor(0x51, 0x52, 0x53);
            active = fixture;
            WriteEntry(fixture.Entries, 0, 0x51, 0.5f, 0, 0, 0, 0, 0);
            WriteEntry(fixture.Entries, 1, 0x52, 1.0f, 0, 0, 0, 0, 0);
            WriteEntry(fixture.Entries, 2, 0x53, 5.9604644775390625e-8f, 0, 0, 0, 0, 0);
            NormalizeWithGameControl(normalize, fixture.Distributor, 0x51);
            uint[] nearFirst = ReadEntry(fixture.Entries);
            uint[] nearSecond = ReadEntry(IntPtr.Add(fixture.Entries, 0x1c));
            uint[] nearThird = ReadEntry(IntPtr.Add(fixture.Entries, 0x38));
            Console.WriteLine("native-normalize-near-rounding: shares=[" + Float(nearFirst[1]) + "," + Float(nearSecond[1]) + "," + Float(nearThird[1]) + "] bits=[" + Hex(nearFirst[1]) + "," + Hex(nearSecond[1]) + "," + Hex(nearThird[1]) + "]");
            ExpectEntry(fixture.Entries, 0, 0x51, 0x3f000000, 0, 0, 0, 0, 0);
            ExpectEntry(fixture.Entries, 1, 0x52, 0x3f000000, 0, 0, 0, 0, 0);
            ExpectEntry(fixture.Entries, 2, 0x53, 0x33000000, 0, 0, 0, 0, 0);
        }
    }

    private static void WriteEntry(IntPtr entries, int index, uint key, float weight, uint pinned,
        uint word3, uint word4, uint word5, uint word6) {
        IntPtr entry = IntPtr.Add(entries, index * 0x1c);
        Marshal.WriteInt32(entry, 0x00, unchecked((int)key));
        Marshal.WriteInt32(entry, 0x04, FloatBits(weight));
        Marshal.WriteInt32(entry, 0x08, unchecked((int)pinned));
        Marshal.WriteInt32(entry, 0x0c, unchecked((int)word3));
        Marshal.WriteInt32(entry, 0x10, unchecked((int)word4));
        Marshal.WriteInt32(entry, 0x14, unchecked((int)word5));
        Marshal.WriteInt32(entry, 0x18, unchecked((int)word6));
    }

    private static void ExpectEntry(IntPtr entries, int index, uint key, uint weight, uint pinned,
        uint word3, uint word4, uint word5, uint word6) {
        uint[] words = ReadEntry(IntPtr.Add(entries, index * 0x1c));
        Expect(words[0] == key && words[1] == weight && words[2] == pinned && words[3] == word3 &&
            words[4] == word4 && words[5] == word5 && words[6] == word6,
            "native entry " + index + " fields [" + String.Join(",", Array.ConvertAll(words, Hex)) + "]");
    }

    private static uint FloatBitsUnsigned(float value) { return unchecked((uint)FloatBits(value)); }

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
    private static IntPtr WriteThisCallThreeArgsToStdcallThunk(int offset, IntPtr stdcallTarget) {
        // Adapt ECX=self plus three stack arguments to stdcall(self, arg1, arg2, arg3).
        byte[] code = { 0xff, 0x74, 0x24, 0x0c, 0xff, 0x74, 0x24, 0x0c, 0xff, 0x74, 0x24, 0x0c,
            0x51, 0xb8, 0, 0, 0, 0, 0xff, 0xd0, 0xc2, 0x0c, 0x00 };
        WriteImm32(code, 14, stdcallTarget.ToInt32());
        IntPtr thunk = IntPtr.Add(fastcallThunks, offset);
        Marshal.Copy(code, 0, thunk, code.Length);
        return thunk;
    }
    private static IntPtr WriteX87ControlWordReader(int offset) {
        byte[] code = { 0xd9, 0x3d, 0, 0, 0, 0, 0xc3 }; // fnstcw [absolute address]; ret
        WriteImm32(code, 2, x87ControlWordCell.ToInt32());
        IntPtr thunk = IntPtr.Add(fastcallThunks, offset);
        Marshal.Copy(code, 0, thunk, code.Length);
        return thunk;
    }
    private static IntPtr WriteX87ControlWordWriter(int offset) {
        byte[] code = { 0xd9, 0x2d, 0, 0, 0, 0, 0xc3 }; // fldcw [absolute address]; ret
        WriteImm32(code, 2, x87ControlWordCell.ToInt32());
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
    private static void Expect(bool value, string message) { if (!value) throw new InvalidOperationException("original Behavior oracle failed: " + message); }
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
