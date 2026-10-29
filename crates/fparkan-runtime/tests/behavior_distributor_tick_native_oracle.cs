using System;
using System.Collections.Generic;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;

// Calls selected original x86 Behavior.dll, ArealMap.dll, AniMesh.dll, and Control.dll paths
// against synthetic storage. Only host callbacks, GetSystemArealMap/GetQueue IAT
// slots, and bounded vector backing are substituted; tested module code remains installed.
internal static class BehaviorDistributorTickOracle {
    private const uint LoadWithAlteredSearchPath = 0x00000008;
    private const uint MemCommitReserve = 0x00003000;
    private const uint PageExecuteReadWrite = 0x40;
    private const uint MemRelease = 0x00008000;
    private const uint PageReadWrite = 0x04;
    private const string ExpectedBehaviorSha256 = "ddfbdbde07832358fe6e35e01c8dba9b58e7313789498b6049ed51d4ed9baf99";
    private const string ExpectedControlSha256 = "bcb86318089028548a178b03a21accdce15df0cc85ddd91674cf3c2f55012c6e";
    private const string ExpectedArealMapSha256 = "a25f9254ebb339d70e2e07c8121950191e9584ca0858d8e9c2e2b6c671edf6ea";
    private const string ExpectedAniMeshSha256 = "65659bf67f20e5be77d67a5b2f59968f61b94293143807fa59ca451c560c4f7e";
    private const int TickRva = 0x19e80;
    private const int DistributorScheduleRva = 0x1adb0;
    private const int AddObjectRva = 0x1aac0;
    private const int NormalizeRva = 0x1ac90;
    private const int IndexedPropertySetRva = 0x0b470;
    private const int IndexedPropertyGetRva = 0x0b5b0;
    private const int OreInitializationRva = 0x08790;
    private const int BehaviorProfileMapRva = 0x08a80;
    private const int BehaviorVarBlockConstructorRva = 0x22d90;
    private const int BehaviorVarFieldRegisterRva = 0x22e20;
    private const int GetSystemArealMapIatRva = 0x59008;
    private const int GetQueueIatRva = 0x59110;
    private const int ControlResourceDeviceGetterRva = 0x21d00;
    private const int ControlResourceAggregateQueryRva = 0x2b410;
    private const int ControlResourceAggregateSetRva = 0x2ba30;
    private const int ControlProviderInterfaceVtableRva = 0x3d254;
    private const int ControlProviderType1GetterRva = 0x0dc40;
    private const int ControlProviderRowConstructorRva = 0x0b240;
    private const int ControlOwnerConstructorRva = 0x06bf0;
    private const int ControlProviderInitialStateRva = 0x0f940;
    private const int ControlProviderAmountUpdateRva = 0x10f30;
    private const int ControlProviderAmountDistributeRva = 0x10ba0;
    private const int ControlProviderStagePropagationRva = 0x11130;
    private const int ControlProviderStageUpdateRva = 0x11220;
    private const int ControlContactRateUpdateRva = 0x12a40;
    private const int ControlTimedUpdateRva = 0x0bcf0;
    private const int ArealMapUnitDatComponentBuilderRva = 0x14b90;
    private const int AniMeshNodeMetadataQueryRva = 0x051f0;
    private const int AniMeshNodeStageCountRva = 0x05840;
    private const int AniMeshNodeStageSetRva = 0x05810;
    private const int ControlResourceDeviceConstructorRva = 0x22980;
    private const int ControlResourceComponentOverrideRva = 0x2d890;
    private const int ControlResourceCommonSetterRva = 0x21d50;
    private const int ControlResourceTag9ConstructorRva = 0x25460;
    private const int ControlResourceTag15ConstructorRva = 0x22ae0;
    private const int ControlResourceTag25ConstructorRva = 0x20800;
    private const int ControlResourceTag15RateRva = 0x22b20;
    private const int ControlResourceTag15UpdateRva = 0x22bb0;
    private const int ControlResourceBudgetUpdateRva = 0x2d340;
    private const int ControlResourceBudgetCategoryRva = 0x2c3a0;
    private const int ControlResourceIntoRva = 0x2d890;
    private const int ControlResourceAvailabilityRva = 0x10b10;
    private const int ControlResourceTag15GateRva = 0x21820;
    private const int ControlResourceTag9VtableRva = 0x3c938;
    private const int ControlResourceTag15VtableRva = 0x3c648;
    private const int ControlResourceTag25VtableRva = 0x3c448;
    private const int ControlResourceTag9StringPairRva = 0x25600;
    private const int ControlResourceNoOpStringPairRva = 0x0bcd0;
    private const int ControlResourceTag19VtableRva = 0x3c608;
    private const int ControlResourceTag19StringPairNoOpRva = 0x0bcd0;
    private const int BehaviorVarTableVtableRva = 0x591b0;
    private const int BehaviorVarArrayLoadRva = 0x49fa0;
    private const int BehaviorVarFieldBindRva = 0x49090;
    private const int BehaviorVarApplyRva = 0x49170;
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
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate void NativeBehaviorSchedule(IntPtr self);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] private delegate IntPtr QueueGetter();
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate uint QueueClockGetter(IntPtr self);
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
    [UnmanagedFunctionPointer(CallingConvention.ThisCall)] private delegate int NativeBehaviorVarArrayLoad(IntPtr self, IntPtr bytes, IntPtr cursorOut);
    [UnmanagedFunctionPointer(CallingConvention.ThisCall)] private delegate void NativeBehaviorVarFieldBind(IntPtr self, IntPtr typeName, IntPtr fieldName, IntPtr destination);
    [UnmanagedFunctionPointer(CallingConvention.ThisCall)] private delegate void NativeBehaviorVarApply(IntPtr self);
    [UnmanagedFunctionPointer(CallingConvention.ThisCall)] private delegate void NativeBehaviorProfileMap(IntPtr self);
    [UnmanagedFunctionPointer(CallingConvention.ThisCall)] private delegate void NativeBehaviorVarBlockConstructor(IntPtr self);
    [UnmanagedFunctionPointer(CallingConvention.ThisCall)] private delegate void NativeBehaviorVarFieldRegister(IntPtr self, IntPtr table);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate void NativeBehaviorProfileLoadCallback(IntPtr owner, uint selector, IntPtr archive, IntPtr entry);
    [UnmanagedFunctionPointer(CallingConvention.ThisCall)] private delegate float NativeControlResourceDeviceGet(IntPtr self, uint selector);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate void NativeFastcallComponentBuilder(IntPtr owner, IntPtr node, uint parentId);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate int NativeArealMapEvent(IntPtr owner, uint message, uint subcode, IntPtr descriptor);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate int NativeAniMeshNodeMetadataQuery(IntPtr owner, uint nodeIndex, uint selector, IntPtr result);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate uint NativeAniMeshNodeStageCount(IntPtr owner, uint nodeIndex);
    [UnmanagedFunctionPointer(CallingConvention.ThisCall)] private delegate IntPtr NativeControlResourceDeviceConstructor(IntPtr self);
    [UnmanagedFunctionPointer(CallingConvention.ThisCall)] private delegate void NativeControlResourceBudgetUpdate(IntPtr self, float deltaSeconds);
    [UnmanagedFunctionPointer(CallingConvention.ThisCall)] private delegate float NativeControlResourceDeviceRate(IntPtr self, float deltaSeconds);
    [UnmanagedFunctionPointer(CallingConvention.ThisCall)] private delegate int NativeControlResourceDeviceGate(IntPtr self);
    [UnmanagedFunctionPointer(CallingConvention.ThisCall)] private delegate float NativeControlResourceAvailability(IntPtr self, byte flags);
    [UnmanagedFunctionPointer(CallingConvention.ThisCall)] private delegate IntPtr NativeControlDeviceFactory(IntPtr self, uint componentId, uint keyOffset, IntPtr record);
    [UnmanagedFunctionPointer(CallingConvention.ThisCall)] private delegate IntPtr NativeControlResourceComponentOverride(IntPtr self, int deviceIndex, IntPtr typedRecord);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate int NativeControlAggregateQuery(IntPtr self, uint selector, IntPtr result);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate int NativeControlAggregateSet(IntPtr self, uint selector, IntPtr value);
    [UnmanagedFunctionPointer(CallingConvention.ThisCall)] private delegate void NativeAniMeshNodeConstructor(IntPtr self, IntPtr model, IntPtr source, uint component, IntPtr meshData);
    [UnmanagedFunctionPointer(CallingConvention.ThisCall)] private delegate void NativeControlProviderAmountUpdate(IntPtr self, int nodeIndex, float delta, uint animationFlag);
    [UnmanagedFunctionPointer(CallingConvention.ThisCall)] private delegate void NativeControlProviderAmountDistribute(IntPtr self, float delta, uint mode, uint flags);
    [UnmanagedFunctionPointer(CallingConvention.ThisCall)] private delegate void NativeControlContactRateUpdate(IntPtr self, float deltaSeconds);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate void NativeControlEvent(IntPtr self, uint eventCode, uint payload);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate void NativeControlScaleQuery(IntPtr self, IntPtr result);
    [UnmanagedFunctionPointer(CallingConvention.ThisCall)] private delegate void NativeControlTimedUpdate(IntPtr self, float inputTime);
    [UnmanagedFunctionPointer(CallingConvention.ThisCall)] private delegate void NativeControlProviderRowConstructor(IntPtr self, IntPtr sourceNdpRow);
    [UnmanagedFunctionPointer(CallingConvention.ThisCall)] private delegate IntPtr NativeControlOwnerConstructor(IntPtr self, IntPtr host, uint classCode);
    [UnmanagedFunctionPointer(CallingConvention.ThisCall)] private delegate void NativeControlProviderInitialState(IntPtr self);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate void NativeControlNodeUpdateCallback(IntPtr self, uint nodeIndex, uint selector, uint valueBits);
    [UnmanagedFunctionPointer(CallingConvention.ThisCall)] private delegate void NativeControlProviderStageUpdate(IntPtr self, int nodeIndex, uint updateEffects);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate void NativeAniMeshStageTransitionCallback(IntPtr self, uint nodeIndex, uint visibleStage);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] private delegate uint NativeAniMeshNodeLifecycleCallback(IntPtr self, uint nodeIndex, uint selector, uint mask);

    private static readonly AreaMapGetter AreaMapGetterCallback = GetAreaMap;
    private static readonly QueueGetter QueueGetterCallback = GetQueue;
    private static readonly QueueClockGetter QueueClockGetterCallback = GetQueueTime;
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
    private static readonly NativeArealMapEvent ArealMapEventCallback = OnArealMapEvent;
    private static readonly NativeControlNodeUpdateCallback ControlNodeUpdateCallback = OnControlNodeUpdate;
    private static readonly NativeAniMeshStageTransitionCallback AniMeshStageTransitionCallback = OnAniMeshStageTransition;
    private static readonly NativeAniMeshNodeLifecycleCallback AniMeshNodeLifecycleCallback = OnAniMeshNodeLifecycle;
    private static readonly NativeAniMeshNodeMetadataQuery AniMeshNodeMetadataQueryCallback = OnAniMeshNodeMetadataQuery;
    private static ArealMapFixture activeArealMap;
    private static int controlNodeUpdateCount;
    private static uint controlNodeUpdateIndex;
    private static uint controlNodeUpdateSelector;
    private static uint controlNodeUpdateValueBits;
    private static readonly List<uint> controlNodeUpdateIndices = new List<uint>();
    private static readonly List<uint> controlNodeUpdateFlags = new List<uint>();
    private static readonly List<string> aniMeshStageTransitions = new List<string>();
    private static readonly List<string> aniMeshNodeLifecycleCallbacks = new List<string>();
    private static readonly List<string> aniMeshNodeMetadataQueries = new List<string>();
    private static readonly List<string> controlProviderCallbackTrace = new List<string>();
    private static NativeAniMeshStageTransitionCallback activeNativeAniMeshStageTransition;
    private static NativeAniMeshNodeLifecycleCallback activeNativeAniMeshNodeLifecycle;
    private static NativeAniMeshNodeMetadataQuery activeNativeAniMeshNodeMetadataQuery;
    private static IntPtr activeControlProviderRows;
    private static IntPtr activeAniMeshInterface;
    private static bool aniMeshCallbackOwnerMismatch;
    private static bool controlNodeUpdateOwnerMismatch;
    private static bool nativeAniMeshStageCallbackMissing;
    private static IntPtr fastcallThunks;
    private static IntPtr queryObjectThunk;
    private static IntPtr getInterfacePropertyThunk;
    private static IntPtr setInterfacePropertyThunk;
    private static IntPtr vectorInsertThunk;
    private static IntPtr x87ControlWordCell;
    private static X87ControlWordReader x87ControlWordReader;
    private static X87ControlWordWriter x87ControlWordWriter;
    private static readonly NativeBehaviorProfileLoadCallback BehaviorProfileLoadCallback = CaptureBehaviorProfileLoad;
    private static readonly List<BehaviorProfileSelection> behaviorProfileSelections = new List<BehaviorProfileSelection>();
    private static Fixture activeQueueFixture;

    private sealed class BehaviorProfileSelection {
        public IntPtr Owner;
        public uint Selector;
        public string Archive;
        public string Entry;
    }

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

    private sealed class NativeProviderTable {
        public IntPtr Owner;
        public IntPtr Rows;
    }

    private sealed class Fixture : IDisposable {
        public IntPtr Distributor;
        public IntPtr Entries;
        public IntPtr VectorVtable;
        public IntPtr Manager;
        public IntPtr ManagerVtable;
        public IntPtr Queue;
        public IntPtr QueueVtable;
        public uint QueueTime;
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
            Marshal.WriteInt16(Distributor, 0x18, 3);
            Marshal.WriteInt16(Distributor, 0x1a, 1);
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
        public void MakeQueue(uint time) {
            QueueTime = time;
            QueueVtable = Allocate(0x40);
            Queue = Allocate(0x20);
            Marshal.WriteIntPtr(Queue, 0, QueueVtable);
            Marshal.WriteIntPtr(QueueVtable, 0x38, Marshal.GetFunctionPointerForDelegate(QueueClockGetterCallback));
        }
        public void Dispose() {
            active = null;
            if (activeQueueFixture == this) activeQueueFixture = null;
            for (int i = Allocations.Count - 1; i >= 0; i--) Marshal.FreeHGlobal(Allocations[i]);
            Allocations.Clear();
        }
    }

    private sealed class ComponentNode {
        public readonly string ArchiveName;
        public readonly string ResourceName;
        public readonly uint Kind;
        public readonly uint LocalIndex;
        public readonly ComponentNode[] Children;
        public ComponentNode(string archiveName, string resourceName, uint kind, uint localIndex, params ComponentNode[] children) {
            ArchiveName = archiveName;
            ResourceName = resourceName;
            Kind = kind;
            LocalIndex = localIndex;
            Children = children ?? new ComponentNode[0];
        }
    }

    private sealed class CapturedComponentDescriptor {
        public string ArchiveName;
        public string ResourceName;
        public uint ParentId;
        public uint LocalIndex;
        public uint AssignedIdBeforeCallback;
        public uint AssignedId;
        public uint Message;
        public uint Subcode;
    }

    private sealed class ArealMapFixture : IDisposable {
        public IntPtr Owner;
        public IntPtr OwnerVtable;
        public readonly List<IntPtr> Allocations = new List<IntPtr>();
        public readonly List<CapturedComponentDescriptor> Descriptors = new List<CapturedComponentDescriptor>();
        public readonly uint[] AssignedIds = new uint[] { 41, 113, 127, 179 };
        public string CallbackError;
        public IntPtr Allocate(int size) {
            IntPtr value = Marshal.AllocHGlobal(size);
            Zero(value, size);
            Allocations.Add(value);
            return value;
        }
        public void Dispose() {
            activeArealMap = null;
            for (int i = Allocations.Count - 1; i >= 0; i--) Marshal.FreeHGlobal(Allocations[i]);
            Allocations.Clear();
        }
    }

    private static IntPtr GetAreaMap() { return active.Manager; }
    private static IntPtr GetQueue() {
        if (activeQueueFixture == null || activeQueueFixture.Queue == IntPtr.Zero)
            throw new InvalidOperationException("native scheduler requested a queue without a fixture");
        return activeQueueFixture.Queue;
    }
    private static uint GetQueueTime(IntPtr self) {
        if (activeQueueFixture == null || self != activeQueueFixture.Queue)
            throw new InvalidOperationException("native scheduler queried an unexpected queue");
        return activeQueueFixture.QueueTime;
    }
    private static int OnArealMapEvent(IntPtr owner, uint message, uint subcode, IntPtr descriptor) {
        ArealMapFixture fixture = activeArealMap;
        if (fixture == null) return 0;
        if (owner != fixture.Owner) fixture.CallbackError = "ArealMap callback received an unexpected owner";
        if (descriptor == IntPtr.Zero) {
            fixture.CallbackError = "ArealMap callback received a null descriptor";
            return 0;
        }
        int ordinal = fixture.Descriptors.Count;
        uint assignedId = ordinal < fixture.AssignedIds.Length ? fixture.AssignedIds[ordinal] : 0xfffffff0u;
        CapturedComponentDescriptor captured = new CapturedComponentDescriptor();
        captured.ArchiveName = Marshal.PtrToStringAnsi(descriptor);
        captured.ResourceName = Marshal.PtrToStringAnsi(IntPtr.Add(descriptor, 0x20));
        captured.ParentId = unchecked((uint)Marshal.ReadInt32(descriptor, 0x40));
        captured.LocalIndex = unchecked((uint)Marshal.ReadInt32(descriptor, 0x44));
        captured.AssignedIdBeforeCallback = unchecked((uint)Marshal.ReadInt32(descriptor, 0x48));
        captured.AssignedId = assignedId;
        captured.Message = message;
        captured.Subcode = subcode;
        fixture.Descriptors.Add(captured);
        Marshal.WriteInt32(descriptor, 0x48, unchecked((int)assignedId));
        return 1;
    }

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
    private static void OnControlNodeUpdate(IntPtr self, uint nodeIndex, uint selector, uint valueBits) {
        if (activeAniMeshInterface != IntPtr.Zero && self != activeAniMeshInterface)
            controlNodeUpdateOwnerMismatch = true;
        controlNodeUpdateCount++;
        controlNodeUpdateIndex = nodeIndex;
        controlNodeUpdateSelector = selector;
        controlNodeUpdateValueBits = valueBits;
        controlNodeUpdateIndices.Add(nodeIndex);
        controlProviderCallbackTrace.Add("amount:" + nodeIndex.ToString());
        if (activeControlProviderRows != IntPtr.Zero)
            controlNodeUpdateFlags.Add(unchecked((uint)Marshal.ReadInt32(activeControlProviderRows, checked((int)nodeIndex * 0x2c + 0x28))));
    }
    private static void OnAniMeshStageTransition(IntPtr self, uint nodeIndex, uint visibleStage) {
        controlProviderCallbackTrace.Add("stage:" + nodeIndex.ToString() + ":" + visibleStage.ToString());
        aniMeshStageTransitions.Add(nodeIndex.ToString() + ":" + visibleStage.ToString());
        if (activeAniMeshInterface != IntPtr.Zero && self != activeAniMeshInterface)
            aniMeshCallbackOwnerMismatch = true;
        if (activeNativeAniMeshStageTransition == null) nativeAniMeshStageCallbackMissing = true;
        else activeNativeAniMeshStageTransition(self, nodeIndex, visibleStage);
    }
    private static uint OnAniMeshNodeLifecycle(IntPtr self, uint nodeIndex, uint selector, uint mask) {
        aniMeshNodeLifecycleCallbacks.Add(nodeIndex.ToString() + ":" + selector.ToString("x") + ":" + mask.ToString("x"));
        controlProviderCallbackTrace.Add("lifecycle:" + nodeIndex.ToString() + ":" + selector.ToString("x") + ":" + mask.ToString("x"));
        if (activeAniMeshInterface != IntPtr.Zero && self != activeAniMeshInterface)
            aniMeshCallbackOwnerMismatch = true;
        if (activeNativeAniMeshNodeLifecycle == null) {
            nativeAniMeshStageCallbackMissing = true;
            return 0;
        }
        return activeNativeAniMeshNodeLifecycle(self, nodeIndex, selector, mask);
    }
    private static int OnAniMeshNodeMetadataQuery(IntPtr self, uint nodeIndex, uint selector, IntPtr result) {
        int nativeResult = activeNativeAniMeshNodeMetadataQuery(self, nodeIndex, selector, result);
        int success = (nativeResult & 0xff) != 0 ? 1 : 0;
        if (activeAniMeshInterface != IntPtr.Zero && self != activeAniMeshInterface)
            aniMeshCallbackOwnerMismatch = true;
        IntPtr value = success != 0 && result != IntPtr.Zero ? Marshal.ReadIntPtr(result) : IntPtr.Zero;
        aniMeshNodeMetadataQueries.Add(nodeIndex.ToString() + ":" + selector.ToString("x") + ":" +
            (value == IntPtr.Zero ? "none" : Hex(unchecked((uint)Marshal.ReadInt32(value)))));
        return success;
    }
    private static float SequenceValue(float[] sequence, int index, float fallback) {
        if (sequence == null || sequence.Length == 0) return fallback;
        return sequence[Math.Min(index, sequence.Length - 1)];
    }

    private static int Main() {
        if (IntPtr.Size != 4) { Console.Error.WriteLine("Build with /platform:x86."); return 2; }
        return RunWithIsolatedWorkingDirectory();
    }

    private static int RunWithIsolatedWorkingDirectory() {
        string originalDirectory = Environment.CurrentDirectory;
        string tempRoot = Path.GetFullPath(Path.GetTempPath()).TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar);
        string workDirectory = Path.Combine(tempRoot, "fparkan-native-oracle-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(workDirectory);
        try {
            // ArealMap.dll runs its attach initializer on load; keep any relative log output out of the workspace and game directory.
            Environment.CurrentDirectory = workDirectory;
            return RunOracle();
        } finally {
            Environment.CurrentDirectory = originalDirectory;
            string fullWorkDirectory = Path.GetFullPath(workDirectory);
            if (!String.Equals(fullWorkDirectory, workDirectory, StringComparison.OrdinalIgnoreCase) ||
                !String.Equals(Path.GetDirectoryName(fullWorkDirectory), tempRoot, StringComparison.OrdinalIgnoreCase) ||
                !Path.GetFileName(fullWorkDirectory).StartsWith("fparkan-native-oracle-", StringComparison.Ordinal))
                throw new InvalidOperationException("refusing to remove an unexpected native oracle working directory");
            if (Directory.Exists(fullWorkDirectory)) Directory.Delete(fullWorkDirectory, true);
        }
    }

    private static int RunOracle() {
        string controlPath = Environment.GetEnvironmentVariable("FPARKAN_ORIGINAL_CONTROL_DLL");
        if (String.IsNullOrEmpty(controlPath)) controlPath = @"C:\GOG Games\Parkan - Iron Strategy\Control.dll";
        if (!File.Exists(controlPath)) { Console.Error.WriteLine("Set FPARKAN_ORIGINAL_CONTROL_DLL to the installed Control.dll."); return 7; }
        string controlHash = Hash(controlPath);
        if (controlHash != ExpectedControlSha256) { Console.Error.WriteLine("Unexpected Control.dll SHA-256: " + controlHash); return 8; }
        string arealMapPath = Environment.GetEnvironmentVariable("FPARKAN_ORIGINAL_AREALMAP_DLL");
        if (String.IsNullOrEmpty(arealMapPath)) arealMapPath = @"C:\GOG Games\Parkan - Iron Strategy\ArealMap.dll";
        if (!File.Exists(arealMapPath)) { Console.Error.WriteLine("Set FPARKAN_ORIGINAL_AREALMAP_DLL to the installed ArealMap.dll."); return 10; }
        string arealMapHash = Hash(arealMapPath);
        if (arealMapHash != ExpectedArealMapSha256) { Console.Error.WriteLine("Unexpected ArealMap.dll SHA-256: " + arealMapHash); return 11; }
        string aniMeshPath = Environment.GetEnvironmentVariable("FPARKAN_ORIGINAL_ANIMESH_DLL");
        if (String.IsNullOrEmpty(aniMeshPath)) aniMeshPath = @"C:\GOG Games\Parkan - Iron Strategy\AniMesh.dll";
        if (!File.Exists(aniMeshPath)) { Console.Error.WriteLine("Set FPARKAN_ORIGINAL_ANIMESH_DLL to the installed AniMesh.dll."); return 13; }
        string aniMeshHash = Hash(aniMeshPath);
        if (aniMeshHash != ExpectedAniMeshSha256) { Console.Error.WriteLine("Unexpected AniMesh.dll SHA-256: " + aniMeshHash); return 14; }
        IntPtr controlModule = LoadLibraryEx(controlPath, IntPtr.Zero, LoadWithAlteredSearchPath);
        if (controlModule == IntPtr.Zero) { Console.Error.WriteLine("LoadLibraryEx(Control.dll) failed: " + Marshal.GetLastWin32Error()); return 9; }
        string path = Environment.GetEnvironmentVariable("FPARKAN_ORIGINAL_BEHAVIOR_DLL");
        if (String.IsNullOrEmpty(path)) path = @"C:\GOG Games\Parkan - Iron Strategy\Behavior.dll";
        if (!File.Exists(path)) { Console.Error.WriteLine("Set FPARKAN_ORIGINAL_BEHAVIOR_DLL to the installed Behavior.dll."); FreeLibrary(controlModule); return 3; }
        string hash = Hash(path);
        if (hash != ExpectedBehaviorSha256) { Console.Error.WriteLine("Unexpected Behavior.dll SHA-256: " + hash); FreeLibrary(controlModule); return 4; }
        IntPtr module = LoadLibraryEx(path, IntPtr.Zero, LoadWithAlteredSearchPath);
        if (module == IntPtr.Zero) { Console.Error.WriteLine("LoadLibraryEx(Behavior.dll) failed: " + Marshal.GetLastWin32Error()); FreeLibrary(controlModule); return 5; }
        IntPtr arealMapModule = LoadLibraryEx(arealMapPath, IntPtr.Zero, LoadWithAlteredSearchPath);
        if (arealMapModule == IntPtr.Zero) {
            Console.Error.WriteLine("LoadLibraryEx(ArealMap.dll) failed: " + Marshal.GetLastWin32Error());
            FreeLibrary(module); FreeLibrary(controlModule); return 12;
        }
        IntPtr aniMeshModule = LoadLibraryEx(aniMeshPath, IntPtr.Zero, LoadWithAlteredSearchPath);
        if (aniMeshModule == IntPtr.Zero) {
            Console.Error.WriteLine("LoadLibraryEx(AniMesh.dll) failed: " + Marshal.GetLastWin32Error());
            FreeLibrary(arealMapModule); FreeLibrary(module); FreeLibrary(controlModule); return 15;
        }
        IntPtr import = IntPtr.Add(module, GetSystemArealMapIatRva);
        IntPtr queueImport = IntPtr.Add(module, GetQueueIatRva);
        IntPtr originalImport = Marshal.ReadIntPtr(import);
        IntPtr originalQueueImport = Marshal.ReadIntPtr(queueImport);
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
            Marshal.WriteIntPtr(queueImport, Marshal.GetFunctionPointerForDelegate(QueueGetterCallback));
            RunSmokeCases(module, controlModule, arealMapModule, aniMeshModule);
            Console.WriteLine("original native Behavior/ArealMap/Control oracle passed (Behavior Add 0x1aac0, normalize 0x1ac90, scheduler 0x1adb0, tick 0x19e80; ArealMap UnitDAT projection 0x14b90; Control INTO override 0x2d890, provider distribution 0x10ba0, class-4 update 0x12a40 and bounded class-10 active clock 0xbcf0; synthetic host callbacks, GetSystemArealMap/GetQueue IAT slots, and bounded vector insertion are substituted; Control provider selector vtable is original; no full Control update or Rust parity is claimed).");
            return 0;
        }
        catch (Exception error) {
            Console.Error.WriteLine(error.ToString());
            return 6;
        }
        finally {
            if (protectionChanged) {
                Marshal.WriteIntPtr(import, originalImport);
                Marshal.WriteIntPtr(queueImport, originalQueueImport);
                uint ignored;
                VirtualProtect(import, new UIntPtr((uint)IntPtr.Size), oldProtection, out ignored);
            }
            GC.KeepAlive(AreaMapGetterCallback); GC.KeepAlive(QueueGetterCallback); GC.KeepAlive(QueueClockGetterCallback);
            GC.KeepAlive(VectorInsertCallback); GC.KeepAlive(x87ControlWordReader); GC.KeepAlive(x87ControlWordWriter); GC.KeepAlive(MapLookupCallback);
            GC.KeepAlive(ObjectStateCallback); GC.KeepAlive(ObjectClassCallback);
            GC.KeepAlive(ObjectQueryCallback); GC.KeepAlive(ObjectRateCallback);
            GC.KeepAlive(ObjectGetPropertyCallback); GC.KeepAlive(ObjectSetPropertyCallback);
            GC.KeepAlive(InterfaceGetCallback); GC.KeepAlive(InterfaceSetCallback);
            GC.KeepAlive(ControlNodeUpdateCallback);
            GC.KeepAlive(ArealMapEventCallback);
            if (thunksAllocated && !VirtualFree(fastcallThunks, UIntPtr.Zero, MemRelease))
                throw new InvalidOperationException("VirtualFree(fastcall callback thunks) failed: " + Marshal.GetLastWin32Error());
            if (x87ControlWordCell != IntPtr.Zero) { Marshal.FreeHGlobal(x87ControlWordCell); x87ControlWordCell = IntPtr.Zero; }
            FreeLibrary(arealMapModule);
            FreeLibrary(aniMeshModule);
            FreeLibrary(module);
            FreeLibrary(controlModule);
        }
    }

    private static void RunSmokeCases(IntPtr module, IntPtr controlModule, IntPtr arealMapModule, IntPtr aniMeshModule) {
        ushort savedControlWord = CurrentX87ControlWord();
        Console.WriteLine("x87-control-word-before=0x" + savedControlWord.ToString("x4") + " (x86 oracle process)");
        try {
            SetX87ControlWord(0x007f);
            Console.WriteLine("native-add-normalize-x87-control-word=0x" + CurrentX87ControlWord().ToString("x4"));
            RunNativeAddCases(module);
            RunNativeNormalizeCases(module);
            RunNativeArealMapComponentBuilderCase(arealMapModule);
            RunNativeAniMeshScaleLoadSnapshot(aniMeshModule);
            RunNativeAniMeshNodeMetadataCases(aniMeshModule);
            RunNativeControlProviderInitialStateCase(controlModule, aniMeshModule);
            RunNativeControlIntoOverrideCase(controlModule);
            RunNativeControlResourceDeviceCase(controlModule);
            RunNativeControlProviderLiveStateCase(controlModule, aniMeshModule);
            RunNativeControlProviderStageArithmeticCase(controlModule, aniMeshModule);
            RunNativeControlProviderHierarchyPropagationCase(controlModule, aniMeshModule);
            RunNativeControlProviderVisibilityCase(controlModule, aniMeshModule);
            RunNativeControlProviderDistributionCases(controlModule);
            RunNativeControlContactRateUpdateCase(controlModule);
            RunNativeControlTimedUpdateBaselineCases(controlModule);
            RunNativeControlAggregateResourceCases(controlModule);
            RunNativeControlResourceBudgetTag15Case(controlModule);
            RunNativeControlActivationCase(controlModule);
            RunNativeBehaviorProfileMapCases(module);
            RunNativeBehaviorVarSyntheticCase(module);
            RunNativeBehaviorRealProfileCases(module);
        } finally {
            SetX87ControlWord(savedControlWord);
            Console.WriteLine("x87-control-word-restored=0x" + CurrentX87ControlWord().ToString("x4"));
        }
        RunNativeIndexedPropertyCases(module);
        RunNativeOreInitializationCases(module);
        ushort tickControlWord = CurrentX87ControlWord();
        try {
            SetX87ControlWord(0x007f);
            Expect(CurrentX87ControlWord() == 0x007f, "Behavior resource-distributor tick uses the measured game x87 control word");
            Console.WriteLine("native-behavior-tick-x87-control-word=0x" + CurrentX87ControlWord().ToString("x4"));
            RunNativeBehaviorTickCases(module);
            RunNativeBehaviorSchedulerCases(module);
        } finally {
            SetX87ControlWord(tickControlWord);
        }
    }

    private static void RunNativeBehaviorTickCases(IntPtr module) {
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
        RunOreAllocationBranch(tick, "ore-allocation-middle", 0.25f, 2.0f, 0.25f, 0.625f, 1.75f, 0.5f);
        RunOreAllocationBranch(tick, "ore-allocation-surplus", 2.0f, 3.0f, 0.25f, 0.625f, 2.625f, 0.625f);
        RunOreAllocationBranch(tick, "ore-allocation-equality", 0.375f, 2.0f, 0.25f, 0.625f, 1.625f, 0.625f);
        RunNativeOreClampCases(tick);
        RunWeightedOreCase(tick);
        RunMixedPowerCase(tick, "mixed-power-sufficient", 4.0f, 1.0f, 0.5f, 0.75f);
        RunMixedPowerCase(tick, "mixed-power-shortage", 0.125f, 1.0f, 0.5f, 0.75f);
        RunMixedPowerCase(tick, "mixed-power-recipient-reserve", 4.0f, 8.0f, 0.5f, 0.75f);
        RunMissingCase(tick);
        RunSkippedCase(tick);
    }

    private static void RunNativeBehaviorSchedulerCases(IntPtr module) {
        NativeBehaviorSchedule schedule = (NativeBehaviorSchedule)Marshal.GetDelegateForFunctionPointer(
            IntPtr.Add(module, DistributorScheduleRva), typeof(NativeBehaviorSchedule));
        uint[] elapsedVectors = { 1, 1001, 16777217, 16777219, 0xffffffff };
        uint[] expectedBits = { 0x3a83126f, 0x3f8020c5, 0x46831270, 0x46831271, 0x4a83126f };
        for (int i = 0; i < elapsedVectors.Length; i++) {
            uint elapsed = elapsedVectors[i];
            uint now = unchecked(1u + elapsed);
            RunNativeSchedulerSample(schedule, now, 1, 0, expectedBits[i], "elapsed-" + elapsed.ToString());
        }

        using (Fixture fixture = NewFixture()) {
            NativeObject producer = fixture.AddObject(0x75, 0x80000002, 1.0f, 0.0f, 0.0f, 0);
            fixture.MakeDistributor(producer.Key);
            fixture.MakeQueue(10);
            active = fixture;
            activeQueueFixture = fixture;
            Marshal.WriteInt32(fixture.Distributor, 0x10, 1);
            Marshal.WriteInt32(fixture.Distributor, 0x14, 10);
            Marshal.WriteInt32(fixture.Distributor, 0x30, unchecked((int)0x41200000));
            schedule(fixture.Distributor);
            Expect(fixture.LookupCount == 0 && unchecked((uint)Marshal.ReadInt32(fixture.Distributor, 0x30)) == 0x41200000,
                "scheduler equality with the deadline performs no resource tick or totals write");

            fixture.QueueTime = 11;
            schedule(fixture.Distributor);
            uint deadlineTotal = unchecked((uint)Marshal.ReadInt32(fixture.Distributor, 0x30));
            Expect(fixture.LookupCount == 2 && deadlineTotal == 0x3c23d70b,
                "scheduler ticks only after the deadline is strictly passed (lookups=" + fixture.LookupCount +
                ", total=" + Hex(deadlineTotal) + ")");
            Console.WriteLine("scheduler-deadline: equality=skip; now=11 delta=0.01; lookups=" + fixture.LookupCount);
        }

        RunNativeSchedulerSample(schedule, 0, 0xffffffff, 0, 0x3a83126f, "wrapped-elapsed-1");
        GC.KeepAlive(schedule);
    }

    private static void RunNativeSchedulerSample(
        NativeBehaviorSchedule schedule, uint now, uint firstTime, uint deadline, uint expectedTotalBits, string label) {
        using (Fixture fixture = NewFixture()) {
            NativeObject producer = fixture.AddObject(0x74, 0x80000002, 1.0f, 0.0f, 0.0f, 0);
            fixture.MakeDistributor(producer.Key);
            fixture.MakeQueue(now);
            active = fixture;
            activeQueueFixture = fixture;
            Marshal.WriteInt32(fixture.Distributor, 0x10, unchecked((int)firstTime));
            Marshal.WriteInt32(fixture.Distributor, 0x14, unchecked((int)deadline));
            schedule(fixture.Distributor);
            uint actual = unchecked((uint)Marshal.ReadInt32(fixture.Distributor, 0x30));
            Expect(actual == expectedTotalBits,
                label + " native qword-to-f32 scheduler delta expected " + Hex(expectedTotalBits) + " got " + Hex(actual));
            Console.WriteLine("native-scheduler-" + label + ": now=" + now.ToString("x8") + " first=" + firstTime.ToString("x8") +
                " total=" + Hex(actual) + " lookups=" + fixture.LookupCount);
        }
    }

    private static void RunNativeControlResourceDeviceCase(IntPtr module) {
        Expect(CurrentX87ControlWord() == 0x007f, "Control selector 0x300 uses x87 control word 0x007f");
        const uint key = 2;
        const int rowCount = 22;
        const int providerRow = (int)key * 0x2c;
        IntPtr providerOwner = Marshal.AllocHGlobal(0x560);
        IntPtr providerRows = Marshal.AllocHGlobal(rowCount * 0x2c);
        IntPtr device = IntPtr.Zero;
        try {
            Zero(providerOwner, 0x560);
            Zero(providerRows, rowCount * 0x2c);
            IntPtr providerInterface = IntPtr.Add(providerOwner, 4);
            IntPtr providerVtable = IntPtr.Add(module, ControlProviderInterfaceVtableRva);
            Marshal.WriteIntPtr(providerInterface, providerVtable);
            Expect(
                Marshal.ReadIntPtr(providerVtable, 0x0c) == IntPtr.Add(module, ControlProviderType1GetterRva),
                "Control provider interface vtable uses the native type-1 getter");
            Marshal.WriteIntPtr(providerOwner, 0x55c, providerRows);

            device = Marshal.AllocHGlobal(0x94);
            Zero(device, 0x94);
            Marshal.WriteInt32(device, 0x04, unchecked((int)key));
            Marshal.WriteIntPtr(device, 0x44, providerInterface);

            NativeControlResourceDeviceGet getter = (NativeControlResourceDeviceGet)
                Marshal.GetDelegateForFunctionPointer(
                    IntPtr.Add(module, ControlResourceDeviceGetterRva),
                    typeof(NativeControlResourceDeviceGet));
            ExpectControlResourceDeviceResult(getter, device, providerRows, providerRow,
                0x3fe6982f, 0x3d25ae4a, 0x40715539, 0x3e8cb02e, "order-sensitive normal");
            ExpectControlResourceDeviceResult(getter, device, providerRows, providerRow,
                0x7f7fffff, 0x40000000, 0x3e800000, 0x7effffff, "extended intermediate overflow then finite");
            ExpectControlResourceDeviceResult(getter, device, providerRows, providerRow,
                0x00800000, 0x00800000, 0x7f7fffff, 0x017fffff, "extended intermediate underflow then finite");
            ExpectControlResourceDeviceResult(getter, device, providerRows, providerRow,
                0x00800000, 0x3f800000, 0x3f000000, 0x00400000, "subnormal final result");
            ExpectControlResourceDeviceResult(getter, device, providerRows, providerRow,
                0xc0000000, 0x3f000000, 0x40800000, 0xc0800000, "negative factors");
            ExpectControlResourceDeviceResult(getter, device, providerRows, providerRow,
                0x80000000, 0x40000000, 0x40400000, 0x80000000, "negative zero");
            Expect(CurrentX87ControlWord() == 0x007f, "Control selector 0x300 retains x87 control word 0x007f");
            Console.WriteLine("native-control-tag26-selector-300: controlHash=" + ExpectedControlSha256 +
                " cw=0x" + CurrentX87ControlWord().ToString("x4") + " key=2 rows=22");
            GC.KeepAlive(getter);
        } finally {
            if (device != IntPtr.Zero) Marshal.FreeHGlobal(device);
            Marshal.FreeHGlobal(providerRows);
            Marshal.FreeHGlobal(providerOwner);
        }
    }

    private static void RunNativeControlProviderLiveStateCase(IntPtr module, IntPtr aniMeshModule) {
        NativeControlProviderRowConstructor constructProviderRow = (NativeControlProviderRowConstructor)
            Marshal.GetDelegateForFunctionPointer(IntPtr.Add(module, ControlProviderRowConstructorRva),
                typeof(NativeControlProviderRowConstructor));
        NativeControlProviderAmountUpdate update = (NativeControlProviderAmountUpdate)
            Marshal.GetDelegateForFunctionPointer(IntPtr.Add(module, ControlProviderAmountUpdateRva),
                typeof(NativeControlProviderAmountUpdate));
        NativeControlResourceDeviceGet getter = (NativeControlResourceDeviceGet)
            Marshal.GetDelegateForFunctionPointer(IntPtr.Add(module, ControlResourceDeviceGetterRva),
                typeof(NativeControlResourceDeviceGet));

        using (Fixture fixture = NewFixture()) {
            IntPtr owner = fixture.Allocate(0x664);
            IntPtr rows = fixture.Allocate(2 * 0x2c);
            IntPtr ndpDescriptor = fixture.Allocate(0x4c);
            IntPtr initializedRow = fixture.Allocate(0x2c);
            constructProviderRow(initializedRow, ndpDescriptor);
            uint[] initialWords = new uint[11];
            for (int i = 0; i < initialWords.Length; i++)
                initialWords[i] = unchecked((uint)Marshal.ReadInt32(initializedRow, i * 4));
            uint[] expectedInitialWords = {
                unchecked((uint)ndpDescriptor.ToInt32()), 0xffffffff, 0x3f800000,
                0x3f800000, 0, 0x3f800000, 0, 1, 0, 0xffffffff, 0
            };
            Expect(String.Join(",", initialWords) == String.Join(",", expectedInitialWords),
                "Control native NDP provider row constructor initializes all eleven words and type-1 current to 1.0");

            IntPtr providerInterface = IntPtr.Add(owner, 4);
            IntPtr providerVtable = IntPtr.Add(module, ControlProviderInterfaceVtableRva);
            IntPtr callbackObject = fixture.Allocate(0x1b0);
            IntPtr meshRows = fixture.Allocate(2 * 0x130);
            NativeAniMeshNodeConstructor constructNode = (NativeAniMeshNodeConstructor)Marshal.GetDelegateForFunctionPointer(
                IntPtr.Add(aniMeshModule, 0x123e0), typeof(NativeAniMeshNodeConstructor));
            for (int node = 0; node < 2; node++) {
                constructNode(IntPtr.Add(meshRows, node * 0x130), IntPtr.Zero, IntPtr.Zero, 0, IntPtr.Zero);
                Expect(Marshal.ReadInt32(meshRows, node * 0x130 + 0x124) == FloatBits(1.0f) &&
                    Marshal.ReadInt32(meshRows, node * 0x130 + 0x1c) == 0,
                    "original AniMesh node constructor initializes provider value and visible stage");
            }
            Marshal.WriteIntPtr(callbackObject, 0x1a8, meshRows);
            Marshal.WriteInt32(callbackObject, 0x1ac, 2);
            Marshal.WriteInt32(meshRows, 0x124, FloatBits(-7.0f));
            Marshal.WriteInt32(meshRows, 0x130 + 0x1c, 77);
            NativeControlNodeUpdateCallback setNodeValue = (NativeControlNodeUpdateCallback)Marshal.GetDelegateForFunctionPointer(
                IntPtr.Add(aniMeshModule, 0x5750), typeof(NativeControlNodeUpdateCallback));
            NativeControlNodeUpdateCallback forwardNodeValue = delegate(IntPtr self, uint node, uint selector, uint bits) {
                OnControlNodeUpdate(self, node, selector, bits);
                setNodeValue(self, node, selector, bits);
            };
            setNodeValue(callbackObject, 1, 0x201, unchecked((uint)FloatBits(99.0f)));
            Expect(ReadFloat(meshRows, 0x130 + 0x124) == 1.0f,
                "AniMesh node-value setter ignores selectors other than 0x200");
            IntPtr callbackVtable = fixture.Allocate(0x38);
            IntPtr device = fixture.Allocate(0x94);
            IntPtr row = IntPtr.Add(rows, 0x2c); // Keep the root row untouched.

            Marshal.WriteIntPtr(providerInterface, providerVtable);
            Marshal.WriteIntPtr(owner, 0x55c, rows);
            Marshal.WriteInt32(owner, 0x548, FloatBits(1.0f));
            Marshal.WriteInt32(owner, 0x590, FloatBits(40.0f));
            Marshal.WriteInt32(owner, 0x660, FloatBits(1.0f));

            Marshal.WriteIntPtr(row, 0x00, ndpDescriptor);
            Marshal.WriteInt32(ndpDescriptor, 0x04, FloatBits(100.0f)); // Native selector 3 maximum.
            Marshal.WriteInt32(row, 0x04, -1); // Root row has no parent node.
            Marshal.WriteInt32(row, 0x08, FloatBits(40.0f));
            Marshal.WriteInt32(row, 0x0c, FloatBits(40.0f));
            Marshal.WriteInt32(row, 0x14, FloatBits(0.4f));

            Marshal.WriteIntPtr(callbackObject, 0, callbackVtable);
            Marshal.WriteIntPtr(callbackVtable, 0x34, Marshal.GetFunctionPointerForDelegate(forwardNodeValue));
            Marshal.WriteIntPtr(owner, 0x20, callbackObject);

            Marshal.WriteInt32(device, 0x04, 1);
            Marshal.WriteInt32(device, 0x4c, FloatBits(3.0f));
            Marshal.WriteInt32(device, 0x54, FloatBits(2.0f));
            Marshal.WriteIntPtr(device, 0x44, providerInterface);

            controlNodeUpdateCount = 0;
            update(owner, 1, 20.0f, 0);
            Expect(ReadFloat(row, 0x08) == 60.0f && ReadFloat(row, 0x14) == 0.6f &&
                ReadFloat(owner, 0x590) == 60.0f,
                "Control native amount update clamps the node amount, updates the normalized provider value, and keeps the aggregate amount in sync");
            Expect(ReadFloat(meshRows, 0x130 + 0x124) == 0.6f && ReadFloat(meshRows, 0x124) == -7.0f &&
                Marshal.ReadInt32(meshRows, 0x130 + 0x1c) == 77,
                "Control amount update reaches original AniMesh setter and changes only its target node value");
            Expect(controlNodeUpdateCount == 1 && controlNodeUpdateIndex == 1 &&
                controlNodeUpdateSelector == 0x200 && controlNodeUpdateValueBits == unchecked((uint)FloatBits(0.6f)),
                "Control native amount update publishes the normalized provider state through the existing node callback");
            Expect(unchecked((uint)FloatBits(getter(device, 0x300))) ==
                unchecked((uint)FloatBits(0.6f * 2.0f * 3.0f)),
                "Control tag-26 selector 0x300 observes the updated NDP provider value through the native provider interface");

            update(owner, 1, -10.0f, 0);
            Expect(ReadFloat(row, 0x08) == 50.0f && ReadFloat(row, 0x14) == 0.5f &&
                unchecked((uint)FloatBits(getter(device, 0x300))) == 0x40400000,
                "Control native amount update is reversible and tag-26 selector 0x300 tracks the changed provider row");

            update(owner, 1, 1000.0f, 0);
            Expect(ReadFloat(row, 0x08) == 100.0f && ReadFloat(row, 0x14) == 1.0f &&
                unchecked((uint)FloatBits(getter(device, 0x300))) == 0x40c00000,
                "Control native amount update clamps the provider value at its native selector-3 maximum");

            update(owner, 1, -200.0f, 0);
            Expect(ReadFloat(row, 0x08) == 0.0f && ReadFloat(row, 0x14) == 0.0f &&
                (unchecked((uint)Marshal.ReadInt32(row, 0x28)) & 0x10) != 0 &&
                unchecked((uint)FloatBits(getter(device, 0x300))) == 0,
                "Control native depletion sets the exhausted flag and selector 0x300 returns the live zero provider value");
            Expect(controlNodeUpdateCount == 4 && controlNodeUpdateIndex == 1 &&
                controlNodeUpdateSelector == 0x200 && controlNodeUpdateValueBits == 0,
                "Control native node callback receives every normalized provider update through exhaustion");

            update(owner, 1, 10.0f, 0);
            Expect(ReadFloat(row, 0x08) == 10.0f && ReadFloat(row, 0x14) == 0.1f &&
                ReadFloat(row, 0x0c) == 40.0f && ReadFloat(owner, 0x590) == 10.0f &&
                (Marshal.ReadInt32(row, 0x28) & 0x10) == 0 && controlNodeUpdateCount == 5,
                "direct provider update revives a leaf without advancing its processed amount");

            Marshal.WriteInt32(row, 8, unchecked((int)0x80000000));
            Marshal.WriteInt32(row, 0x14, FloatBits(0.25f));
            Marshal.WriteInt32(owner, 0x590, FloatBits(20.0f));
            update(owner, 1, -1.0f, 0);
            Expect(Marshal.ReadInt32(row, 8) == 0 && ReadFloat(row, 0x14) == 0.25f &&
                ReadFloat(row, 0x0c) == 40.0f && ReadFloat(owner, 0x590) == 20.0f &&
                controlNodeUpdateCount == 5 && (Marshal.ReadInt32(row, 0x28) & 0x10) == 0 &&
                ReadFloat(meshRows, 0x130 + 0x124) == 0.1f,
                "clamping signed zero writes amount bits but leaves ratio, aggregate, flags and publication unchanged");

            Console.WriteLine("native-control-live-ndp-provider: controlHash=" + ExpectedControlSha256 +
                " rowCtorRva=0xb240 initialType1=1 rowStride=0x2c node=1 max=100 amount=0..100 tag26Selector300=live");
            Console.WriteLine("native-control-to-animesh-node-value: AniMeshCtor=0x123e0 setter=0x5750 rowStride=0x130 value=+0x124" +
                " selector=0x200 other-node/stage=unchanged signed-zero=no-publication");
            GC.KeepAlive(constructNode);
            GC.KeepAlive(setNodeValue);
            GC.KeepAlive(forwardNodeValue);
            GC.KeepAlive(constructProviderRow);
            GC.KeepAlive(update);
            GC.KeepAlive(getter);
        }
    }

    private static void RunNativeControlProviderStageArithmeticCase(IntPtr module, IntPtr aniMeshModule) {
        NativeControlProviderStageUpdate updateStage = (NativeControlProviderStageUpdate)
            Marshal.GetDelegateForFunctionPointer(IntPtr.Add(module, ControlProviderStageUpdateRva), typeof(NativeControlProviderStageUpdate));
        activeNativeAniMeshStageTransition = (NativeAniMeshStageTransitionCallback)
            Marshal.GetDelegateForFunctionPointer(IntPtr.Add(aniMeshModule, AniMeshNodeStageSetRva),
                typeof(NativeAniMeshStageTransitionCallback));
        activeNativeAniMeshNodeLifecycle = (NativeAniMeshNodeLifecycleCallback)
            Marshal.GetDelegateForFunctionPointer(IntPtr.Add(aniMeshModule, 0x05500),
                typeof(NativeAniMeshNodeLifecycleCallback));
        using (Fixture fixture = new Fixture()) {
            const int rowCount = 11;
            IntPtr owner = fixture.Allocate(0x664);
            IntPtr rows = fixture.Allocate(rowCount * 0x2c);
            IntPtr aniInterface = fixture.Allocate(0x1b0);
            IntPtr aniVtable = fixture.Allocate(0x50);
            IntPtr meshRows = fixture.Allocate(rowCount * 0x130);
            Marshal.WriteIntPtr(aniInterface, 0, aniVtable);
            Marshal.WriteIntPtr(aniInterface, 0x1a8, meshRows);
            Marshal.WriteInt32(aniInterface, 0x1ac, rowCount);
            for (int index = 0; index < rowCount; index++)
                Marshal.WriteInt32(meshRows, index * 0x130 + 0x1c, -99);
            Marshal.WriteIntPtr(aniVtable, 0x44, Marshal.GetFunctionPointerForDelegate(AniMeshStageTransitionCallback));
            Marshal.WriteIntPtr(aniVtable, 0x20, Marshal.GetFunctionPointerForDelegate(AniMeshNodeLifecycleCallback));
            Marshal.WriteIntPtr(owner, 0x20, aniInterface);
            Marshal.WriteIntPtr(owner, 0x55c, rows);
            Marshal.WriteInt32(owner, 0x560, rowCount);

            // Parent ratio .25 yields stage 3 of 4. Bit 4 inherits that ratio;
            // bit 8 inherits the parent's stage and clamps it to the child's count.
            WriteProviderStageRow(rows, 0, -1, 40.0f, 91.0f, 0.25f, 0, 4, 0x02);
            WriteProviderStageRow(rows, 1, 0, 12.0f, 81.0f, 0.875f, 0, 5, 0x06);
            WriteProviderStageRow(rows, 2, 0, 7.0f, 71.0f, 0.9f, 0, 2, 0x0a);
            WriteProviderStageRow(rows, 3, 1, 3.0f, 31.0f, 0.1f, 0, 6, 0x0e);
            // These independent rows distinguish the native ceiling operation
            // from floor/round for fractional stage products.
            WriteProviderStageRow(rows, 4, -1, 2.0f, 92.0f, 0.1f, 0, 3, 0x02);
            WriteProviderStageRow(rows, 5, -1, 3.0f, 93.0f, 0.4f, 0, 3, 0x02);
            WriteProviderStageRow(rows, 6, -1, 4.0f, 94.0f, 0.5f, 0, 3, 0x02);
            WriteProviderStageRow(rows, 7, -1, 5.0f, 95.0f, 1.0f, 1, 3, 0x102);
            WriteProviderStageRow(rows, 8, -1, 6.0f, 96.0f, 0.0f, 0, 3, 0x02);
            WriteProviderStageRow(rows, 9, -1, 10.0f, 90.0f, 0.5f, 0, 2, 0x02);
            WriteProviderStageRow(rows, 10, -1, 11.0f, 90.0f, 0.75f, 0, 16777217, 0x02);

            aniMeshStageTransitions.Clear();
            aniMeshNodeLifecycleCallbacks.Clear();
            controlProviderCallbackTrace.Clear();
            aniMeshCallbackOwnerMismatch = false;
            controlNodeUpdateOwnerMismatch = false;
            nativeAniMeshStageCallbackMissing = false;
            activeAniMeshInterface = aniInterface;
            updateStage(owner, 0, 0); // param2=false suppresses effect creation.
            updateStage(owner, 1, 0);
            updateStage(owner, 2, 0);
            updateStage(owner, 3, 0);
            for (int index = 4; index < rowCount; index++) updateStage(owner, index, 0);

            uint[] expectedStages = { 3, 3, 2, 3, 2, 1, 1, 0, 3, 1, 4194304 };
            uint[] expectedVisibleStages = { 3, 3, 1, 3, 2, 1, 1, 0, 2, 1, 4194304 };
            uint[] expectedAmounts = { 0x42200000, 0x41400000, 0x40e00000, 0x40400000,
                0x40000000, 0x40400000, 0x40800000, 0x40a00000, 0x40c00000, 0x41200000, 0x41300000 };
            for (int index = 0; index < rowCount; index++) {
                IntPtr row = IntPtr.Add(rows, index * 0x2c);
                IntPtr meshRow = IntPtr.Add(meshRows, index * 0x130);
                Expect(unchecked((uint)Marshal.ReadInt32(row, 0x18)) == expectedStages[index],
                    "Control 0x11220 computes inherited stage for row " + index);
                Expect(unchecked((uint)Marshal.ReadInt32(meshRow, 0x1c)) == expectedVisibleStages[index],
                    "AniMesh RVA 0x5810 receives the bounded visual stage for row " + index);
                Expect(unchecked((uint)Marshal.ReadInt32(row, 0x08)) == expectedAmounts[index] &&
                    unchecked((uint)Marshal.ReadInt32(row, 0x0c)) == expectedAmounts[index],
                    "Control 0x11220 retains amount and updates its processed snapshot for row " + index);
                Expect((unchecked((uint)Marshal.ReadInt32(row, 0x28)) & 0x200) != 0,
                    "Control 0x11220 records a stage transition for row " + index);
            }
            Expect(unchecked((uint)Marshal.ReadInt32(rows, 0x14)) == 0x3e800000 &&
                unchecked((uint)Marshal.ReadInt32(rows, 0x2c + 0x14)) == 0x3e800000 &&
                unchecked((uint)Marshal.ReadInt32(rows, 3 * 0x2c + 0x14)) == 0x3e800000,
                "Control flag 4 copies the parent type-1 ratio before stage calculation");
            Expect(ReadFloat(meshRows, 0x130 + 0x124) == 0.0f && ReadFloat(rows, 0x2c + 0x14) == 0.25f,
                "inheriting a parent provider ratio does not rewrite the separate AniMesh node value");
            Expect(String.Join(",", aniMeshStageTransitions.ToArray()) == "0:3,1:3,2:1,3:3,4:2,5:1,6:1,7:0,8:2,9:1,10:4194304" &&
                String.Join(",", aniMeshNodeLifecycleCallbacks.ToArray()) == "7:101:4" && !aniMeshCallbackOwnerMismatch &&
                !controlNodeUpdateOwnerMismatch && !nativeAniMeshStageCallbackMissing,
                "Control stage and slot-0x20 callbacks reach the original AniMesh RVA 0x5810 in direct update order");
            Expect(String.Join(",", controlProviderCallbackTrace.ToArray()) ==
                "stage:0:3,stage:1:3,stage:2:1,stage:3:3,stage:4:2,stage:5:1,stage:6:1,lifecycle:7:101:4,stage:7:0,stage:8:2,stage:9:1,stage:10:4194304",
                "AniMesh slot 0x20 receives its native node, selector, and mask arguments before stage publication");

            int transitionsBeforeNoOp = aniMeshStageTransitions.Count;
            IntPtr unchanged = IntPtr.Add(rows, 6 * 0x2c);
            Marshal.WriteInt32(unchanged, 0x0c, FloatBits(999.0f));
            Marshal.WriteInt32(meshRows, 6 * 0x130 + 0x1c, 77);
            updateStage(owner, 6, 1);
            Expect(ReadFloat(unchanged, 0x0c) == ReadFloat(unchanged, 8) &&
                Marshal.ReadInt32(meshRows, 6 * 0x130 + 0x1c) == 77 &&
                aniMeshStageTransitions.Count == transitionsBeforeNoOp,
                "unchanged stage refreshes processed amount without stage or effects callbacks");
            Marshal.WriteInt32(unchanged, 0x28, 0x52);
            Marshal.WriteInt32(unchanged, 0x0c, FloatBits(999.0f));
            updateStage(owner, 6, 0);
            Expect(ReadFloat(unchanged, 0x0c) == 999.0f &&
                aniMeshStageTransitions.Count == transitionsBeforeNoOp,
                "depleted protected node returns without processing its amount");

            Console.WriteLine("native-control-parent-stage-arithmetic: controlHash=" + ExpectedControlSha256 +
                " rva=0x11220 bit4=ratio bit8=parent-stage ceil=[0.1,0.4,0.5,1,0]=>[2,1,1,0,3]" +
                " callbacks=[0:3,1:3,2:1,3:3,4:2,5:1,6:1,lifecycle7:101:4,7:0,8:2,9:1,10:4194304]");
            activeAniMeshInterface = IntPtr.Zero;
            activeNativeAniMeshStageTransition = null;
            activeNativeAniMeshNodeLifecycle = null;
            GC.KeepAlive(updateStage);
        }
    }

    private static void RunNativeControlProviderHierarchyPropagationCase(IntPtr module, IntPtr aniMeshModule) {
        NativeControlProviderStageUpdate propagate = (NativeControlProviderStageUpdate)
            Marshal.GetDelegateForFunctionPointer(IntPtr.Add(module, ControlProviderStagePropagationRva), typeof(NativeControlProviderStageUpdate));
        activeNativeAniMeshStageTransition = (NativeAniMeshStageTransitionCallback)
            Marshal.GetDelegateForFunctionPointer(IntPtr.Add(aniMeshModule, AniMeshNodeStageSetRva),
                typeof(NativeAniMeshStageTransitionCallback));
        activeNativeAniMeshNodeLifecycle = (NativeAniMeshNodeLifecycleCallback)
            Marshal.GetDelegateForFunctionPointer(IntPtr.Add(aniMeshModule, 0x05500),
                typeof(NativeAniMeshNodeLifecycleCallback));
        using (Fixture fixture = new Fixture()) {
            NativeProviderTable table = CreateNativeProviderTable(fixture, module,
                new float[] { 100.0f, 20.0f, 10.0f, 30.0f, 14.0f },
                new float[] { 25.0f, 10.0f, 5.0f, 12.0f, 7.0f },
                new byte[] { 0x02, 0x02, 0x02, 0x02, 0x02 });
            const int rowCount = 5;
            int[] parents = { -1, 0, 1, 0, 0 };
            uint[] stages = { 4, 2, 3, 2, 2 };
            uint[] flags = { 0x02, 0x42, 0x02, 0x102, 0x02 };
            for (int index = 0; index < rowCount; index++) {
                IntPtr row = IntPtr.Add(table.Rows, index * 0x2c);
                WriteProviderStageRow(table.Rows, index, parents[index], ReadFloat(row, 0x08),
                    123.0f + index, ReadFloat(row, 0x14), 0, stages[index], flags[index]);
            }

            IntPtr aniInterface = fixture.Allocate(0x1b0);
            IntPtr aniVtable = fixture.Allocate(0x50);
            IntPtr meshRows = fixture.Allocate(rowCount * 0x130);
            Marshal.WriteIntPtr(aniInterface, 0, aniVtable);
            Marshal.WriteIntPtr(aniInterface, 0x1a8, meshRows);
            Marshal.WriteInt32(aniInterface, 0x1ac, rowCount);
            for (int index = 0; index < rowCount; index++)
                Marshal.WriteInt32(meshRows, index * 0x130 + 0x1c, -99);
            Marshal.WriteIntPtr(aniVtable, 0x20, Marshal.GetFunctionPointerForDelegate(AniMeshNodeLifecycleCallback));
            Marshal.WriteIntPtr(aniVtable, 0x34, Marshal.GetFunctionPointerForDelegate(ControlNodeUpdateCallback));
            Marshal.WriteIntPtr(aniVtable, 0x44, Marshal.GetFunctionPointerForDelegate(AniMeshStageTransitionCallback));
            Marshal.WriteIntPtr(table.Owner, 0x20, aniInterface);

            controlNodeUpdateCount = 0;
            controlNodeUpdateIndices.Clear();
            controlNodeUpdateFlags.Clear();
            aniMeshStageTransitions.Clear();
            aniMeshNodeLifecycleCallbacks.Clear();
            controlProviderCallbackTrace.Clear();
            aniMeshCallbackOwnerMismatch = false;
            controlNodeUpdateOwnerMismatch = false;
            nativeAniMeshStageCallbackMissing = false;
            activeControlProviderRows = table.Rows;
            activeAniMeshInterface = aniInterface;

            propagate(table.Owner, 0, 0);

            IntPtr parent = table.Rows;
            IntPtr protected40 = IntPtr.Add(table.Rows, 0x2c);
            IntPtr grandchild = IntPtr.Add(table.Rows, 2 * 0x2c);
            IntPtr protected100 = IntPtr.Add(table.Rows, 3 * 0x2c);
            IntPtr sibling = IntPtr.Add(table.Rows, 4 * 0x2c);
            IntPtr parentMeshRow = meshRows;
            IntPtr bit40MeshRow = IntPtr.Add(meshRows, 0x130);
            IntPtr grandchildMeshRow = IntPtr.Add(meshRows, 2 * 0x130);
            IntPtr bit100MeshRow = IntPtr.Add(meshRows, 3 * 0x130);
            IntPtr siblingMeshRow = IntPtr.Add(meshRows, 4 * 0x130);
            Expect(ReadFloat(parent, 0x08) == 25.0f && ReadFloat(parent, 0x0c) == 25.0f &&
                unchecked((uint)Marshal.ReadInt32(parent, 0x18)) == 3,
                "Control 0x11130 updates the parent stage and processed amount first");
            Expect(ReadFloat(protected40, 0x08) == 0.0f && ReadFloat(protected40, 0x14) == 0.0f &&
                unchecked((uint)Marshal.ReadInt32(protected40, 0x18)) == 0 &&
                (unchecked((uint)Marshal.ReadInt32(protected40, 0x28)) & 0x50) == 0x50,
                "Protected bit 0x40 does not stop child depletion, while its depleted-stage early return remains visible");
            Expect(ReadFloat(grandchild, 0x08) == 0.0f && ReadFloat(grandchild, 0x0c) == 125.0f &&
                unchecked((uint)Marshal.ReadInt32(grandchild, 0x18)) == 0 &&
                (unchecked((uint)Marshal.ReadInt32(grandchild, 0x28)) & 0x90) == 0x90,
                "Control 0x11130 drains a protected child's descendant but its inherited 0x80 flag preserves the depleted stage snapshot");
            Expect(ReadFloat(protected100, 0x08) == 0.0f && ReadFloat(protected100, 0x0c) == 0.0f &&
                unchecked((uint)Marshal.ReadInt32(protected100, 0x18)) == 2,
                "Protected bit 0x100 does not stop child depletion or its later stage update");
            Expect(ReadFloat(sibling, 0x08) == 0.0f && ReadFloat(sibling, 0x0c) == 0.0f &&
                unchecked((uint)Marshal.ReadInt32(sibling, 0x18)) == 2 &&
                (unchecked((uint)Marshal.ReadInt32(sibling, 0x28)) & 0x100) != 0,
                "An unprotected sibling retains the native damaged flag while updating its stage");
            Expect(unchecked((uint)Marshal.ReadInt32(parentMeshRow, 0x1c)) == 3 &&
                unchecked((uint)Marshal.ReadInt32(bit40MeshRow, 0x1c)) == unchecked((uint)-99) &&
                unchecked((uint)Marshal.ReadInt32(grandchildMeshRow, 0x1c)) == unchecked((uint)-99) &&
                unchecked((uint)Marshal.ReadInt32(bit100MeshRow, 0x1c)) == 1 &&
                unchecked((uint)Marshal.ReadInt32(siblingMeshRow, 0x1c)) == 1,
                "ANI slot 0x44 reaches native AniMesh RVA 0x5810 for changed visible stages only");
            Expect(controlNodeUpdateCount == 4 &&
                String.Join(",", controlNodeUpdateIndices.ConvertAll(index => index.ToString()).ToArray()) == "1,2,3,4" &&
                String.Join(",", controlNodeUpdateFlags.ConvertAll(value => value.ToString("x")).ToArray()) == "42,82,102,102" &&
                !controlNodeUpdateOwnerMismatch,
                "Control 0x11130 traverses descendants depth-first, checking callback owners and protected flags");
            Expect(String.Join(",", aniMeshStageTransitions.ToArray()) == "0:3,3:1,4:1" &&
                aniMeshNodeLifecycleCallbacks.Count == 0 &&
                !aniMeshCallbackOwnerMismatch && !nativeAniMeshStageCallbackMissing,
                "Control descendant update preserves the native callback order and stage bounds");
            Expect(String.Join(",", controlProviderCallbackTrace.ToArray()) ==
                "stage:0:3,amount:1,amount:2,amount:3,stage:3:1,amount:4,stage:4:1",
                "Control 0x11130 keeps amount, lifecycle, and visual-stage callbacks in native depth-first order");
            Expect(ReadFloat(table.Owner, 0x590) == 25.0f,
                "Control 0x10f30 keeps the aggregate amount in sync after recursive child depletion");

            Console.WriteLine("native-control-parent-propagation: controlHash=" + ExpectedControlSha256 +
                " rva=0x11130 childOrder=[1,2,3,4] protected40/100=depleted bit40-stage-return=preserved" +
                " callbacks=" + String.Join(",", controlProviderCallbackTrace.ToArray()));
            activeControlProviderRows = IntPtr.Zero;
            activeAniMeshInterface = IntPtr.Zero;
            activeNativeAniMeshStageTransition = null;
            activeNativeAniMeshNodeLifecycle = null;
            GC.KeepAlive(propagate);
        }
    }

    private static void RunNativeControlProviderVisibilityCase(IntPtr controlModule, IntPtr aniMeshModule) {
        NativeControlProviderStageUpdate updateStage = (NativeControlProviderStageUpdate)
            Marshal.GetDelegateForFunctionPointer(IntPtr.Add(controlModule, ControlProviderStageUpdateRva),
                typeof(NativeControlProviderStageUpdate));
        NativeAniMeshNodeMetadataQuery queryNode = (NativeAniMeshNodeMetadataQuery)
            Marshal.GetDelegateForFunctionPointer(IntPtr.Add(aniMeshModule, AniMeshNodeMetadataQueryRva),
                typeof(NativeAniMeshNodeMetadataQuery));
        NativeAniMeshNodeLifecycleCallback updateNodeLifecycle = (NativeAniMeshNodeLifecycleCallback)
            Marshal.GetDelegateForFunctionPointer(IntPtr.Add(aniMeshModule, 0x05500),
                typeof(NativeAniMeshNodeLifecycleCallback));
        NativeAniMeshStageTransitionCallback setNodeStage = (NativeAniMeshStageTransitionCallback)
            Marshal.GetDelegateForFunctionPointer(IntPtr.Add(aniMeshModule, AniMeshNodeStageSetRva),
                typeof(NativeAniMeshStageTransitionCallback));

        using (Fixture fixture = NewFixture()) {
            NativeProviderTable table = CreateNativeProviderTable(fixture, controlModule,
                new float[] { 13.0f }, new float[] { 13.0f }, new byte[] { 0x00 });
            IntPtr descriptor = Marshal.ReadIntPtr(table.Rows);
            Marshal.WriteInt32(descriptor, 0x08, FloatBits(0.03125f)); // source mobility per unit volume
            Marshal.WriteInt32(table.Owner, 0x50, 9); // class 9 makes FUN_1000fca0 return after the visibility change
            Marshal.WriteInt32(table.Owner, 0x530, FloatBits(100.0f));
            Marshal.WriteInt32(table.Owner, 0x534, FloatBits(200.0f));
            Marshal.WriteInt32(table.Owner, 0x538, FloatBits(300.0f));
            Marshal.WriteInt32(table.Owner, 0x4d8, FloatBits(40.0f));
            WriteProviderStageRow(table.Rows, 0, -1, 13.0f, 13.0f, 0.0f, 0, 3, 0x00);
            Marshal.WriteInt32(table.Rows, 0x20, FloatBits(13.0f));

            IntPtr aniInterface = fixture.Allocate(0x1b0);
            IntPtr aniVtable = fixture.Allocate(0x50);
            IntPtr meshRows = fixture.Allocate(0x130);
            IntPtr nodeData = fixture.Allocate(0x26);
            IntPtr slotData = fixture.Allocate(2 * 0x44);
            IntPtr meshData = fixture.Allocate(3 * IntPtr.Size);
            for (int word = 0; word < 19; word++) Marshal.WriteInt16(nodeData, word * 2, -1);
            for (int stage = 0; stage < 3; stage++)
                Marshal.WriteInt16(nodeData, 0x08 + stage * 5 * sizeof(ushort), 0);
            // State 0,1,2 each expose slot 0; 0x11220 publishes visible stage 2 before 0x11920 queries it.
            Marshal.WriteIntPtr(meshData, 0, nodeData);
            Marshal.WriteIntPtr(meshData, 2 * IntPtr.Size, slotData);
            Marshal.WriteIntPtr(aniInterface, 0, aniVtable);
            Marshal.WriteIntPtr(aniInterface, 0x1a8, meshRows);
            Marshal.WriteInt32(aniInterface, 0x1ac, 1);
            Marshal.WriteInt32(aniInterface, 0x158, FloatBits(2.0f));
            Marshal.WriteInt32(aniInterface, 0x15c, FloatBits(3.0f));
            Marshal.WriteInt32(aniInterface, 0x160, FloatBits(5.0f));
            Marshal.WriteInt32(meshRows, 0x00, 0); // ANI model-instance ID used by FUN_10005780/57c0
            Marshal.WriteInt32(meshRows, 0x1c, 0); // current visible stage selects Node38 slot state 0
            Marshal.WriteIntPtr(meshRows, 0x12c, meshData);
            Marshal.WriteInt32(slotData, 0x30, FloatBits(7.0f));
            Marshal.WriteInt32(slotData, 0x34, FloatBits(11.0f));
            Marshal.WriteInt32(slotData, 0x44 + 0x30, FloatBits(17.0f));
            Marshal.WriteInt32(slotData, 0x44 + 0x34, FloatBits(19.0f));

            // Use the original ANI vtable entries exercised by FUN_10011920:
            // node selector F/10, instance lookup/count, lifecycle flags, and visible stage.
            activeNativeAniMeshNodeMetadataQuery = queryNode;
            Marshal.WriteIntPtr(aniVtable, 0x0c, Marshal.GetFunctionPointerForDelegate(AniMeshNodeMetadataQueryCallback));
            activeNativeAniMeshNodeLifecycle = updateNodeLifecycle;
            Marshal.WriteIntPtr(aniVtable, 0x20, Marshal.GetFunctionPointerForDelegate(AniMeshNodeLifecycleCallback));
            Marshal.WriteIntPtr(aniVtable, 0x38, IntPtr.Add(aniMeshModule, 0x05780));
            Marshal.WriteIntPtr(aniVtable, 0x3c, IntPtr.Add(aniMeshModule, 0x057c0));
            activeNativeAniMeshStageTransition = setNodeStage;
            Marshal.WriteIntPtr(aniVtable, 0x44, Marshal.GetFunctionPointerForDelegate(AniMeshStageTransitionCallback));
            Marshal.WriteIntPtr(table.Owner, 0x20, aniInterface);
            activeAniMeshInterface = aniInterface;

            IntPtr firstValueOut = fixture.Allocate(IntPtr.Size);
            IntPtr secondValueOut = fixture.Allocate(IntPtr.Size);
            Expect(NativeBoolean(queryNode(aniInterface, 0, 0x0f, firstValueOut)) == 1,
                "AniMesh selector F reads the raw slot state through the original getter");
            IntPtr firstValue = Marshal.ReadIntPtr(firstValueOut);
            Expect(ReadFloat(firstValue, 0) == 42.0f,
                "AniMesh selector F scales slot+0x30 by both XY instance factors");
            Expect(NativeBoolean(queryNode(aniInterface, 0, 0x10, secondValueOut)) == 1,
                "AniMesh selector 10 reads the second raw slot field through the original getter");
            IntPtr secondValue = Marshal.ReadIntPtr(secondValueOut);
            Expect(firstValue == secondValue && ReadFloat(firstValue, 0) == 330.0f,
                "AniMesh selectors F and 10 return the same scratch cell; the second query overwrites the first result");

            activeControlProviderRows = table.Rows;
            aniMeshNodeMetadataQueries.Clear();
            aniMeshNodeLifecycleCallbacks.Clear();
            aniMeshStageTransitions.Clear();
            controlProviderCallbackTrace.Clear();
            aniMeshCallbackOwnerMismatch = false;
            nativeAniMeshStageCallbackMissing = false;
            // At ratio zero, the native ceiling stage rule selects the depleted sentinel
            // and reaches 0x11920's hide branch. Ratio one reopens that node.
            updateStage(table.Owner, 0, 0);
            IntPtr row = table.Rows;
            IntPtr meshRow = meshRows;
            Expect(unchecked((uint)Marshal.ReadInt32(row, 0x28)) == 0x220 &&
                unchecked((uint)Marshal.ReadInt32(row, 0x18)) == 3 &&
                unchecked((uint)Marshal.ReadInt32(meshRow, 0x14)) == 1 &&
                unchecked((uint)Marshal.ReadInt32(meshRow, 0x1c)) == 2,
                "Control RVA 0x11920 hides an exhausted provider in both Control and the original AniMesh node state");
            Expect(String.Join(",", aniMeshNodeMetadataQueries.ToArray()) == "0:f:0x42280000,0:10:0x43a50000" &&
                !aniMeshCallbackOwnerMismatch && !nativeAniMeshStageCallbackMissing,
                "Control RVA 0x11920 queries the shared AniMesh scratch cell before applying its aggregate changes");
            Console.WriteLine("native-control-provider-hide-values: row20=" + Float(ReadFloat(row, 0x20)) +
                " totals=[" + Float(ReadFloat(table.Owner, 0x530)) + "," + Float(ReadFloat(table.Owner, 0x534)) +
                "," + Float(ReadFloat(table.Owner, 0x538)) + "," + Float(ReadFloat(table.Owner, 0x4d8)) +
                "] queries=[" + String.Join(",", aniMeshNodeMetadataQueries.ToArray()) + "]");
            Expect(ReadFloat(table.Owner, 0x530) == -230.0f && ReadFloat(table.Owner, 0x534) == -130.0f &&
                ReadFloat(table.Owner, 0x538) == 287.0f && FloatBits(ReadFloat(table.Owner, 0x4d8)) == FloatBits(42.6875f),
                "Control RVA 0x11920 removes the aliased AniMesh value from aggregates and applies the volume adjustment");

            Marshal.WriteInt32(row, 0x14, FloatBits(1.0f));
            updateStage(table.Owner, 0, 0);
            Expect(unchecked((uint)Marshal.ReadInt32(row, 0x28)) == 0x200 &&
                unchecked((uint)Marshal.ReadInt32(row, 0x18)) == 0 &&
                unchecked((uint)Marshal.ReadInt32(meshRow, 0x14)) == 0 &&
                unchecked((uint)Marshal.ReadInt32(meshRow, 0x1c)) == 0,
                "Control RVA 0x11920 shows the reopened provider and clears both Control and AniMesh visibility flags");
            Expect(ReadFloat(table.Owner, 0x530) == 100.0f && ReadFloat(table.Owner, 0x534) == 200.0f &&
                ReadFloat(table.Owner, 0x538) == 300.0f && FloatBits(ReadFloat(table.Owner, 0x4d8)) == FloatBits(40.0f),
                "Control RVA 0x11920 restores provider aggregates on the paired show transition");
            Expect(String.Join(",", aniMeshNodeMetadataQueries.ToArray()) ==
                "0:f:0x42280000,0:10:0x43a50000,0:f:0x42280000,0:10:0x43a50000" &&
                String.Join(",", aniMeshNodeLifecycleCallbacks.ToArray()) == "0:100:1,0:101:5" &&
                String.Join(",", aniMeshStageTransitions.ToArray()) == "0:2,0:0",
                "Native hide/show repeats the same F/10 queries and original AniMesh callbacks in order");

            // The same native getter follows a new visible state: state 2 now selects
            // the second MSH slot while state 0 still selects the first.
            Marshal.WriteInt16(nodeData, 0x08 + 2 * 5 * sizeof(ushort), 1);
            Marshal.WriteInt32(row, 0x14, FloatBits(0.0f));
            updateStage(table.Owner, 0, 0);
            Expect(String.Join(",", aniMeshNodeMetadataQueries.ToArray()) ==
                "0:f:0x42280000,0:10:0x43a50000,0:f:0x42280000,0:10:0x43a50000," +
                "0:f:0x42cc0000,0:10:0x440e8000" &&
                ReadFloat(table.Owner, 0x530) == -470.0f && ReadFloat(table.Owner, 0x534) == -370.0f &&
                ReadFloat(table.Owner, 0x538) == 287.0f && FloatBits(ReadFloat(table.Owner, 0x4d8)) == FloatBits(35.1875f),
                "Control hide query follows visible state 2 and applies its different native mesh-slot value");
            Marshal.WriteInt32(row, 0x14, FloatBits(1.0f));
            updateStage(table.Owner, 0, 0);
            Expect(String.Join(",", aniMeshNodeMetadataQueries.ToArray()) ==
                "0:f:0x42280000,0:10:0x43a50000,0:f:0x42280000,0:10:0x43a50000," +
                "0:f:0x42cc0000,0:10:0x440e8000,0:f:0x42280000,0:10:0x43a50000" &&
                ReadFloat(table.Owner, 0x530) == -140.0f && ReadFloat(table.Owner, 0x534) == -40.0f &&
                ReadFloat(table.Owner, 0x538) == 300.0f && FloatBits(ReadFloat(table.Owner, 0x4d8)) == FloatBits(32.5f),
                "Control show query returns to visible state 0 rather than reversing the prior state-2 contribution");
            Expect(String.Join(",", controlProviderCallbackTrace.ToArray()) ==
                "stage:0:2,lifecycle:0:100:1,stage:0:0,lifecycle:0:101:5," +
                "stage:0:2,lifecycle:0:100:1,stage:0:0,lifecycle:0:101:5",
                "Control and AniMesh visibility transitions preserve native callback order and arguments");
            Expect(!aniMeshCallbackOwnerMismatch && !nativeAniMeshStageCallbackMissing,
                "Both visibility cycles preserve AniMesh callback owners and original function results");

            Console.WriteLine("native-control-provider-visibility: controlHash=" + ExpectedControlSha256 +
                " aniMeshHash=" + ExpectedAniMeshSha256 + " rva=0x11920 selectorF/10=same-scratch-cell " +
                "visible-state-values=[42,330]=>[102,570] hide/show=stage-dependent");
            activeControlProviderRows = IntPtr.Zero;
            activeAniMeshInterface = IntPtr.Zero;
            activeNativeAniMeshNodeMetadataQuery = null;
            activeNativeAniMeshStageTransition = null;
            activeNativeAniMeshNodeLifecycle = null;
            GC.KeepAlive(updateStage);
            GC.KeepAlive(queryNode);
            GC.KeepAlive(updateNodeLifecycle);
            GC.KeepAlive(setNodeStage);
            GC.KeepAlive(AniMeshNodeLifecycleCallback);
            GC.KeepAlive(AniMeshStageTransitionCallback);
        }
    }

    private static void WriteProviderStageRow(IntPtr rows, int index, int parentIndex,
        float amount, float processed, float normalized, uint currentStage, uint stageCount, uint flags) {
        IntPtr row = IntPtr.Add(rows, index * 0x2c);
        Marshal.WriteInt32(row, 0x04, parentIndex);
        Marshal.WriteInt32(row, 0x08, FloatBits(amount));
        Marshal.WriteInt32(row, 0x0c, FloatBits(processed));
        Marshal.WriteInt32(row, 0x14, FloatBits(normalized));
        Marshal.WriteInt32(row, 0x18, unchecked((int)currentStage));
        Marshal.WriteInt32(row, 0x1c, unchecked((int)stageCount));
        Marshal.WriteInt32(row, 0x28, unchecked((int)flags));
    }

    private static NativeProviderTable CreateNativeProviderTable(Fixture fixture, IntPtr module,
        float[] maxima, float[] amounts, byte[] rowFlags) {
        Expect(maxima.Length == amounts.Length && maxima.Length == rowFlags.Length,
            "native provider fixture arrays have matching lengths");
        NativeControlProviderRowConstructor construct = (NativeControlProviderRowConstructor)
            Marshal.GetDelegateForFunctionPointer(IntPtr.Add(module, ControlProviderRowConstructorRva),
                typeof(NativeControlProviderRowConstructor));
        NativeProviderTable table = new NativeProviderTable();
        table.Owner = fixture.Allocate(0x664);
        table.Rows = fixture.Allocate(maxima.Length * 0x2c);
        IntPtr providerInterface = IntPtr.Add(table.Owner, 4);
        IntPtr providerVtable = IntPtr.Add(module, ControlProviderInterfaceVtableRva);
        Marshal.WriteIntPtr(providerInterface, providerVtable);
        Expect(Marshal.ReadIntPtr(providerVtable, 0x0c) == IntPtr.Add(module, ControlProviderType1GetterRva),
            "native provider fixture uses the installed selector interface vtable");
        Marshal.WriteIntPtr(table.Owner, 0x55c, table.Rows);
        Marshal.WriteInt32(table.Owner, 0x560, maxima.Length);
        Marshal.WriteInt32(table.Owner, 0x548, FloatBits(1.0f));
        Marshal.WriteInt32(table.Owner, 0x660, FloatBits(1.0f));
        Marshal.WriteInt32(table.Owner, 0x550, 0xfffe); // Exclude the separate root-node depletion lifecycle.
        Marshal.WriteInt32(table.Owner, 0x594, 0);
        Marshal.WriteInt32(table.Owner, 0x598, 0);
        Marshal.WriteInt32(table.Owner, 0x618, 0);
        Marshal.WriteInt32(table.Owner, 0x386, 0);

        float totalMaximum = 0.0f;
        float totalAmount = 0.0f;
        for (int i = 0; i < maxima.Length; i++) {
            IntPtr descriptor = fixture.Allocate(0x80);
            IntPtr row = IntPtr.Add(table.Rows, i * 0x2c);
            Marshal.WriteInt32(descriptor, 0x04, FloatBits(maxima[i]));
            construct(row, descriptor);
            Marshal.WriteInt32(row, 0x04, -1); // No component parent or stage chain.
            Marshal.WriteInt32(row, 0x08, FloatBits(amounts[i]));
            Marshal.WriteInt32(row, 0x10, 0);
            Marshal.WriteInt32(row, 0x14, FloatBits(amounts[i] / maxima[i]));
            Marshal.WriteInt32(row, 0x18, 0);
            Marshal.WriteInt32(row, 0x1c, 0);
            Marshal.WriteInt32(row, 0x24, -1);
            Marshal.WriteByte(row, 0x28, rowFlags[i]);
            totalMaximum += maxima[i];
            totalAmount += amounts[i];
        }
        Marshal.WriteInt32(table.Owner, 0x58c, FloatBits(totalMaximum));
        Marshal.WriteInt32(table.Owner, 0x590, FloatBits(totalAmount));

        IntPtr callbackObject = fixture.Allocate(IntPtr.Size);
        IntPtr callbackVtable = fixture.Allocate(0x38);
        Marshal.WriteIntPtr(callbackObject, 0, callbackVtable);
        Marshal.WriteIntPtr(callbackVtable, 0x34, Marshal.GetFunctionPointerForDelegate(ControlNodeUpdateCallback));
        Marshal.WriteIntPtr(table.Owner, 0x20, callbackObject);
        return table;
    }

    private static float ReadNativeProviderAmount(NativeProviderTable table, int index) {
        return ReadFloat(table.Rows, index * 0x2c + 0x08);
    }

    private static byte ReadNativeProviderFlags(NativeProviderTable table, int index) {
        return Marshal.ReadByte(table.Rows, index * 0x2c + 0x28);
    }

    private static void RunNativeControlProviderDistributionCases(IntPtr module) {
        NativeControlProviderAmountDistribute distribute = (NativeControlProviderAmountDistribute)
            Marshal.GetDelegateForFunctionPointer(IntPtr.Add(module, ControlProviderAmountDistributeRva),
                typeof(NativeControlProviderAmountDistribute));

        using (Fixture fixture = NewFixture()) {
            NativeProviderTable table = CreateNativeProviderTable(fixture, module,
                new float[] { 30.0f, 60.0f, 50.0f }, new float[] { 10.0f, 20.0f, 0.0f },
                new byte[] { 0, 0, 0 });
            controlNodeUpdateCount = 0;
            controlNodeUpdateIndices.Clear();
            distribute(table.Owner, 22.0f, 0, 0);
            Expect(ReadNativeProviderAmount(table, 0) == 14.0f &&
                ReadNativeProviderAmount(table, 1) == 28.0f &&
                ReadNativeProviderAmount(table, 2) == 10.0f,
                "Control native proportional amount distribution weights each row by its remaining capacity");
            Expect(controlNodeUpdateCount == 3 && controlNodeUpdateIndex == 2 &&
                controlNodeUpdateSelector == 0x200,
                "Control proportional distribution publishes each row update through the existing native node callback");
            Expect(String.Join(",", controlNodeUpdateIndices.ConvertAll(index => index.ToString()).ToArray()) == "0,1,2",
                "Control proportional distribution visits rows in ascending order");
        }

        using (Fixture fixture = NewFixture()) {
            NativeProviderTable table = CreateNativeProviderTable(fixture, module,
                new float[] { 30.0f, 60.0f, 50.0f }, new float[] { 10.0f, 20.0f, 30.0f },
                new byte[] { 0, 0, 0 });
            controlNodeUpdateIndices.Clear();
            distribute(table.Owner, -35.0f, 1, 0);
            Expect(ReadNativeProviderAmount(table, 0) == 10.0f &&
                ReadNativeProviderAmount(table, 1) == 15.0f &&
                ReadNativeProviderAmount(table, 2) == 0.0f,
                "Control native sequential withdrawal consumes rows in reverse order");
            Expect(String.Join(",", controlNodeUpdateIndices.ConvertAll(index => index.ToString()).ToArray()) == "2,1",
                "Control sequential withdrawal calls the node updater from the last row backwards");
        }

        using (Fixture fixture = NewFixture()) {
            NativeProviderTable table = CreateNativeProviderTable(fixture, module,
                new float[] { 10.0f, 10.0f }, new float[] { 9.0f, 0.0f },
                new byte[] { 0, 0x10 });
            distribute(table.Owner, 10.0f, 1, 0);
            Expect(ReadNativeProviderAmount(table, 0) == 10.0f &&
                ReadNativeProviderAmount(table, 1) == 0.0f &&
                (ReadNativeProviderFlags(table, 1) & 0x10) != 0,
                "Control sequential distribution clips at active capacity and skips exhausted rows without revive");
            distribute(table.Owner, -20.0f, 1, 0);
            Expect(ReadNativeProviderAmount(table, 0) == 0.0f &&
                (ReadNativeProviderFlags(table, 0) & 0x10) != 0,
                "Control native depletion marks the row exhausted");
            distribute(table.Owner, 15.0f, 1, 1);
            Expect(ReadNativeProviderAmount(table, 0) == 10.0f &&
                ReadNativeProviderAmount(table, 1) == 5.0f &&
                (ReadNativeProviderFlags(table, 0) & 0x10) == 0 &&
                (ReadNativeProviderFlags(table, 1) & 0x10) == 0,
                "Control native revive flag admits exhausted rows and clears their exhausted flags when refilled");
        }

        using (Fixture fixture = NewFixture()) {
            NativeProviderTable table = CreateNativeProviderTable(fixture, module,
                new float[] { 33554432.0f, 10.0f }, new float[] { 16777216.0f, 2.0f },
                new byte[] { 0, 0 });
            controlNodeUpdateIndices.Clear();
            distribute(table.Owner, -1.0f, 0, 0);
            uint firstBits = unchecked((uint)FloatBits(ReadNativeProviderAmount(table, 0)));
            uint secondBits = unchecked((uint)FloatBits(ReadNativeProviderAmount(table, 1)));
            uint totalBits = unchecked((uint)FloatBits(ReadFloat(table.Owner, 0x590)));
            Expect(firstBits == unchecked((uint)FloatBits(16777214.0f)) &&
                secondBits == 0x3ffffffe && totalBits == unchecked((uint)FloatBits(16777216.0f)),
                "Control proportional distribution preserves the native x87 add-then-subtract rounding at a 2^24 cancellation boundary; actual=[" +
                Hex(firstBits) + "," + Hex(secondBits) + "] total=" + Hex(totalBits));
        }

        Console.WriteLine("native-control-provider-distribution: controlHash=" + ExpectedControlSha256 +
            " rva=0x10ba0 providerVtable=0x3d254 selector3=0xdc40 proportional=[14,28,10]" +
            " sequentialReverse=[10,15,0] cap/exhausted/revive=verified cancellation=2^24");
        GC.KeepAlive(distribute);
    }

    private static void RunNativeControlContactRateUpdateCase(IntPtr module) {
        NativeControlContactRateUpdate update = (NativeControlContactRateUpdate)
            Marshal.GetDelegateForFunctionPointer(IntPtr.Add(module, ControlContactRateUpdateRva),
                typeof(NativeControlContactRateUpdate));
        using (Fixture fixture = NewFixture()) {
            NativeProviderTable table = CreateNativeProviderTable(fixture, module,
                new float[] { 100.0f }, new float[] { 10.000001f }, new byte[] { 0 });
            IntPtr profile = fixture.Allocate(0x6c);
            Marshal.WriteInt32(table.Owner, 0x50, 4);
            Marshal.WriteInt32(table.Owner, 0x1a4, FloatBits(1.0f));
            Marshal.WriteIntPtr(table.Owner, 0x46c, profile);
            Marshal.WriteInt32(profile, 0x68, FloatBits(1.0f));
            Marshal.WriteInt32(table.Owner, 0x4d8, FloatBits(1.0f));
            Marshal.WriteInt32(table.Owner, 0x354, FloatBits(1.0f));
            Marshal.WriteInt32(table.Owner, 0x358, FloatBits(1.0f));
            Marshal.WriteInt32(table.Owner, 0x1a8, FloatBits(1.0f));
            controlNodeUpdateCount = 0;

            update(table.Owner, 2.0f);
            Expect(FloatBits(ReadNativeProviderAmount(table, 0)) == FloatBits(8.000001f) &&
                FloatBits(ReadFloat(table.Rows, 0x10)) == FloatBits(-2.0f) &&
                ReadFloat(table.Owner, 0x590) == 8.000001f,
                "Control class-4 contact-rate update applies negative rate times elapsed seconds and publishes animation amount");
            const int expectedNormalizedBits = unchecked((int)0x3da3d70c);
            Expect(Marshal.ReadByte(table.Owner, 0x598) == 0 &&
                FloatBits(ReadFloat(table.Owner, 0x588)) == expectedNormalizedBits &&
                controlNodeUpdateCount == 1 && controlNodeUpdateValueBits == unchecked((uint)expectedNormalizedBits),
                "Control class-4 contact-rate update clears its dirty state after computing the aggregate and provider callback");
            Console.WriteLine("native-control-contact-rate-update: controlHash=" + ExpectedControlSha256 +
                " rva=0x12a40 class=4 dt=2 rate=1 current=10.000001->8.000001 animation=-2 normalizedBits=0x" +
                unchecked((uint)FloatBits(ReadFloat(table.Owner, 0x588))).ToString("x8") + " dirty=cleared");
        }
        GC.KeepAlive(update);
    }

    private static void RunNativeControlTimedUpdateBaselineCases(IntPtr module) {
        NativeControlTimedUpdate update = (NativeControlTimedUpdate)
            Marshal.GetDelegateForFunctionPointer(IntPtr.Add(module, ControlTimedUpdateRva),
                typeof(NativeControlTimedUpdate));
        using (Fixture fixture = NewFixture()) {
            IntPtr owner = fixture.Allocate(0x664);
            controlNodeUpdateCount = 0;
            update(owner, 0.5f);
            Expect(controlNodeUpdateCount == 0 && ReadFloat(owner, 0xe8) == 0.5f &&
                ReadFloat(owner, 0xe4) == 0.0f && ReadFloat(owner, 0xec) == 0.0f &&
                ReadFloat(owner, 0xd8) == 0.0f && ReadFloat(owner, 0xdc) == 0.0f,
                "Control timed update records the sub-threshold delta without entering its active callback path");
            update(owner, 1.0f);
            Expect(controlNodeUpdateCount == 0 && ReadFloat(owner, 0xe8) == 1.0f &&
                ReadFloat(owner, 0xe4) == 1.0f && ReadFloat(owner, 0xec) == 1.0f &&
                ReadFloat(owner, 0xd8) == 1.0f && ReadFloat(owner, 0xdc) == 1.0f &&
                Marshal.ReadByte(owner, 0xe0) == 0,
                "Control timed update accepts the one-unit boundary and synchronizes its baseline while e0 remains clear");
            update(owner, 2.0f);
            Expect(controlNodeUpdateCount == 0 && ReadFloat(owner, 0xe8) == 1.0f &&
                ReadFloat(owner, 0xe4) == 2.0f && ReadFloat(owner, 0xec) == 2.0f &&
                ReadFloat(owner, 0xd8) == 2.0f && ReadFloat(owner, 0xdc) == 2.0f &&
                Marshal.ReadByte(owner, 0xe0) == 0,
                "Control timed update keeps synchronizing accepted times without starting the active branch while e0 is clear");
            Console.WriteLine("native-control-timed-update-baseline: controlHash=" + ExpectedControlSha256 +
                " rva=0xbcf0 sub-threshold=0.5 accepted=[1,2] e4/ec/d8/dc synchronized owner.e0=0 callback=none");
        }
        GC.KeepAlive(update);
    }

    private static void RunNativeControlAggregateResourceCases(IntPtr module) {
        NativeControlAggregateQuery query = (NativeControlAggregateQuery)Marshal.GetDelegateForFunctionPointer(
            WriteStdCallToFastcallThunk(0x1c0, IntPtr.Add(module, ControlResourceAggregateQueryRva)),
            typeof(NativeControlAggregateQuery));
        NativeControlAggregateSet set = (NativeControlAggregateSet)Marshal.GetDelegateForFunctionPointer(
            WriteStdCallToFastcallThunk(0x200, IntPtr.Add(module, ControlResourceAggregateSetRva)),
            typeof(NativeControlAggregateSet));
        using (Fixture fixture = NewFixture()) {
            IntPtr owner = fixture.Allocate(0x5b8);
            IntPtr devices = fixture.Allocate(4 * IntPtr.Size);
            IntPtr result = fixture.Allocate(sizeof(uint));
            IntPtr value = fixture.Allocate(sizeof(uint));

            IntPtr tag9Current19 = AddNativeControlAggregateDevice(fixture, module, 9, 19,
                0x120, ControlResourceTag9ConstructorRva, ControlResourceTag9VtableRva, 2.0f, 3.0f, 0.5f);
            IntPtr tag19Current9 = AddNativeControlAggregateDevice(fixture, module, 19, 9,
                0x94, ControlResourceDeviceConstructorRva, ControlResourceTag19VtableRva, 100.0f, 77.0f, 1.0f);
            IntPtr tag15Current19 = AddNativeControlAggregateDevice(fixture, module, 15, 19,
                0x98, ControlResourceTag15ConstructorRva, ControlResourceTag15VtableRva, 4.0f, 0.5f, 1.0f);
            IntPtr tag19Current19 = AddNativeControlAggregateDevice(fixture, module, 19, 19,
                0x94, ControlResourceDeviceConstructorRva, ControlResourceTag19VtableRva, 1.0f, 4.0f, 1.0f);

            Marshal.WriteIntPtr(devices, 0, tag9Current19);
            Marshal.WriteIntPtr(devices, IntPtr.Size, tag19Current9);
            Marshal.WriteIntPtr(devices, 2 * IntPtr.Size, tag15Current19);
            Marshal.WriteIntPtr(devices, 3 * IntPtr.Size, tag19Current19);
            Marshal.WriteIntPtr(owner, 0x5b0, devices);
            Marshal.WriteInt32(owner, 0x5b4, 4);

            Expect(NativeBoolean(query(owner, 1, result)) == 1 && unchecked((uint)Marshal.ReadInt32(result)) == 0x40000000,
                "Control aggregate selector 1 includes descriptor-19 entries and dispatches their original factory vtables");
            Expect(NativeBoolean(query(owner, 2, result)) == 1 && unchecked((uint)Marshal.ReadInt32(result)) == 0x40c00000,
                "Control aggregate selector 2 sums base values for descriptor-19 entries");

            Marshal.WriteInt32(value, 0, FloatBits(7.5f));
            Expect(NativeBoolean(set(owner, 1, value)) == 1,
                "Control aggregate setter selector 1 reports success for its recognized selector");
            Expect(unchecked((uint)Marshal.ReadInt32(tag9Current19, 0x4c)) == 0x40f00000 &&
                unchecked((uint)Marshal.ReadInt32(tag15Current19, 0x4c)) == 0x40f00000 &&
                unchecked((uint)Marshal.ReadInt32(tag19Current19, 0x4c)) == 0x40f00000 &&
                unchecked((uint)Marshal.ReadInt32(tag19Current9, 0x4c)) == FloatBits(77.0f),
                "Control setter selector 1 updates matching positive devices regardless of factory class, and skips current descriptor tag 9");
            Expect(NativeBoolean(query(owner, 1, result)) == 1 && unchecked((uint)Marshal.ReadInt32(result)) == 0x410c0000,
                "Control aggregate selector 1 reflects updated per-device multipliers");

            // Factory tag 9's virtual selector 0 includes +0x11c. A zero base keeps
            // this selected descriptor out of the native setter's positive-only write.
            Marshal.WriteInt32(tag9Current19, 0x11c, FloatBits(0.0f));
            Marshal.WriteInt32(value, 0, FloatBits(9.0f));
            Expect(NativeBoolean(set(owner, 1, value)) == 1 &&
                unchecked((uint)Marshal.ReadInt32(tag9Current19, 0x4c)) == 0x40f00000,
                "Control setter selector 1 leaves the multiplier unchanged when the original factory getter returns zero");

            Marshal.WriteInt32(owner, 0x5b4, 0);
            Marshal.WriteInt32(result, 0, unchecked((int)0x3f123456));
            Expect(NativeBoolean(query(owner, 1, result)) == 0 && unchecked((uint)Marshal.ReadInt32(result)) == 0x3f123456,
                "Control aggregate selector 1 reports no result and leaves output untouched when the table is empty");
            Expect(NativeBoolean(query(owner, 2, result)) == 0 && unchecked((uint)Marshal.ReadInt32(result)) == 0x3f123456,
                "Control aggregate selector 2 reports no result and leaves output untouched when the table is empty");
            Expect(NativeBoolean(set(owner, 1, value)) == 1,
                "Control aggregate setter selector 1 reports success when the table is empty");

            // The aggregate accumulators spill back to float precision every row.
            Marshal.WriteInt32(owner, 0x5b4, 3);
            Marshal.WriteInt32(Marshal.ReadIntPtr(tag19Current9, 0x48), 0, 19);
            Marshal.WriteInt32(tag9Current19, 0x54, FloatBits(1.0f));
            Marshal.WriteInt32(tag9Current19, 0x4c, FloatBits(16777216.0f));
            Marshal.WriteInt32(tag9Current19, 0x11c, FloatBits(1.0f));
            Marshal.WriteInt32(tag19Current9, 0x54, FloatBits(1.0f));
            Marshal.WriteInt32(tag19Current9, 0x4c, FloatBits(1.0f));
            Marshal.WriteInt32(tag15Current19, 0x54, FloatBits(1.0f));
            Marshal.WriteInt32(tag15Current19, 0x4c, FloatBits(-16777216.0f));
            Expect(NativeBoolean(query(owner, 1, result)) == 1 && Marshal.ReadInt32(result) == 0,
                "Control aggregate selector 1 rounds each accumulator to float after each row");

            Marshal.WriteInt32(owner, 0x5b4, 1);
            Marshal.WriteInt32(tag9Current19, 0x54, FloatBits(-1.0f));
            Marshal.WriteInt32(tag9Current19, 0x4c, FloatBits(2.0f));
            Marshal.WriteInt32(result, 0, unchecked((int)0x3f123456));
            Expect(NativeBoolean(query(owner, 1, result)) == 1 && Marshal.ReadInt32(result) == FloatBits(1.0f),
                "Control aggregate selector 1 returns one immediately when a selected base value is negative");
            Expect(NativeBoolean(query(owner, 2, result)) == 1 && Marshal.ReadInt32(result) == FloatBits(-1.0f),
                "Control aggregate selector 2 returns a selected negative base value immediately");

            Console.WriteLine("native-control-global-resource-table: controlHash=" + ExpectedControlSha256 +
                " getterRva=0x2b410 setterRva=0x2ba30 selectors=1,2 descriptorFilter=19" +
                " factory19-descriptor9=excluded factory9-descriptor19-scale=0.5" +
                " factory15-descriptor19=included initial=[2,6] multiplier=7.5 post-set-getter1=8.75" +
                " empty=[false/unchanged,false/unchanged,set=true] rounding=0 negative=[1,-1]");
            GC.KeepAlive(query);
            GC.KeepAlive(set);
        }
    }

    private static IntPtr AddNativeControlAggregateDevice(Fixture fixture, IntPtr module,
        int factoryTag, int currentDescriptorTag, int objectSize, int constructorRva, int vtableRva,
        float coefficient, float multiplier, float class9Scale) {
        IntPtr device = fixture.Allocate(objectSize);
        NativeControlResourceDeviceConstructor construct = (NativeControlResourceDeviceConstructor)Marshal.GetDelegateForFunctionPointer(
            IntPtr.Add(module, constructorRva), typeof(NativeControlResourceDeviceConstructor));
        IntPtr expectedVtable = IntPtr.Add(module, vtableRva);
        Expect(construct(device) == device && Marshal.ReadIntPtr(device) == expectedVtable,
            "native factory tag " + factoryTag + " constructor installs its original class vtable");
        IntPtr descriptor = fixture.Allocate(sizeof(uint));
        Marshal.WriteInt32(descriptor, 0, currentDescriptorTag);
        Marshal.WriteIntPtr(device, 0x48, descriptor);
        Marshal.WriteInt32(device, 0x4c, FloatBits(multiplier));
        Marshal.WriteInt32(device, 0x54, FloatBits(coefficient));
        if (factoryTag == 9) Marshal.WriteInt32(device, 0x11c, FloatBits(class9Scale));
        return device;
    }

    private static void RunNativeControlResourceBudgetTag15Case(IntPtr module) {
        NativeControlOwnerConstructor constructOwner = (NativeControlOwnerConstructor)Marshal.GetDelegateForFunctionPointer(
            IntPtr.Add(module, ControlOwnerConstructorRva), typeof(NativeControlOwnerConstructor));
        NativeControlProviderRowConstructor constructProviderRow = (NativeControlProviderRowConstructor)
            Marshal.GetDelegateForFunctionPointer(IntPtr.Add(module, ControlProviderRowConstructorRva),
                typeof(NativeControlProviderRowConstructor));
        NativeControlResourceDeviceConstructor constructTag15 = (NativeControlResourceDeviceConstructor)
            Marshal.GetDelegateForFunctionPointer(IntPtr.Add(module, ControlResourceTag15ConstructorRva),
                typeof(NativeControlResourceDeviceConstructor));
        NativeControlResourceComponentOverride applyInto = (NativeControlResourceComponentOverride)
            Marshal.GetDelegateForFunctionPointer(IntPtr.Add(module, ControlResourceIntoRva),
                typeof(NativeControlResourceComponentOverride));
        NativeControlResourceDeviceRate rate = (NativeControlResourceDeviceRate)
            Marshal.GetDelegateForFunctionPointer(IntPtr.Add(module, ControlResourceTag15RateRva),
                typeof(NativeControlResourceDeviceRate));
        NativeControlResourceDeviceGate gate = (NativeControlResourceDeviceGate)
            Marshal.GetDelegateForFunctionPointer(IntPtr.Add(module, ControlResourceTag15GateRva),
                typeof(NativeControlResourceDeviceGate));
        NativeControlResourceDeviceGet getter = (NativeControlResourceDeviceGet)
            Marshal.GetDelegateForFunctionPointer(IntPtr.Add(module, ControlResourceDeviceGetterRva),
                typeof(NativeControlResourceDeviceGet));
        NativeControlResourceAvailability availability = (NativeControlResourceAvailability)
            Marshal.GetDelegateForFunctionPointer(IntPtr.Add(module, ControlResourceAvailabilityRva),
                typeof(NativeControlResourceAvailability));
        NativeControlResourceBudgetUpdate update = (NativeControlResourceBudgetUpdate)
            Marshal.GetDelegateForFunctionPointer(IntPtr.Add(module, ControlResourceBudgetUpdateRva),
                typeof(NativeControlResourceBudgetUpdate));

        using (Fixture fixture = NewFixture()) {
            IntPtr owner = fixture.Allocate(0x664);
            Expect(constructOwner(owner, IntPtr.Zero, 4) == owner, "native budget owner constructor");
            IntPtr providerRow = fixture.Allocate(0x2c);
            IntPtr ndpDescriptor = fixture.Allocate(0x4c);
            Marshal.WriteInt32(ndpDescriptor, 4, FloatBits(100.0f));
            constructProviderRow(providerRow, ndpDescriptor);
            Marshal.WriteInt32(providerRow, 4, -1); // No parent or stage lifecycle in this budget vector.
            Marshal.WriteInt32(providerRow, 0x24, -1);
            Marshal.WriteIntPtr(owner, 0x55c, providerRow);
            Marshal.WriteInt32(owner, 0x560, 1);
            Marshal.WriteInt32(owner, 0x65c, FloatBits(1.0f)); // Explicit maximum multiplier read by native selector 3.
            Marshal.WriteInt32(owner, 0x58c, FloatBits(100.0f));
            IntPtr ani = fixture.Allocate(4);
            IntPtr aniVtable = fixture.Allocate(0x38);
            Marshal.WriteIntPtr(ani, aniVtable);
            Marshal.WriteIntPtr(aniVtable, 0x34, Marshal.GetFunctionPointerForDelegate(ControlNodeUpdateCallback));
            Marshal.WriteIntPtr(owner, 0x20, ani);

            IntPtr device = fixture.Allocate(0x98);
            IntPtr typedRecord = fixture.Allocate(0xb4);
            Expect(constructTag15(device) == device, "native tag-15 constructor");
            IntPtr tag15Vtable = Marshal.ReadIntPtr(device);
            Expect(tag15Vtable == IntPtr.Add(module, ControlResourceTag15VtableRva) &&
                Marshal.ReadIntPtr(tag15Vtable, 0x14) == IntPtr.Add(module, ControlResourceTag15RateRva) &&
                Marshal.ReadIntPtr(tag15Vtable, 0x18) == IntPtr.Add(module, ControlResourceTag15UpdateRva),
                "budget fixture keeps original tag-15 rate and update methods");
            Marshal.WriteIntPtr(device, 0x3c, owner);
            Marshal.WriteIntPtr(device, 0x44, IntPtr.Add(owner, 4));
            Marshal.WriteInt32(typedRecord, 0, 15);
            Marshal.WriteInt32(typedRecord, 0x18, 1);
            Marshal.WriteInt32(typedRecord, 0x2c, FloatBits(1.0f));
            Marshal.WriteInt32(typedRecord, 0x30, FloatBits(1.0f));

            // Mock only the generator: rate is read from +8; update records dt, scale,
            // provider amount at callback time, and call count. All tag-15 code stays native.
            IntPtr generator = fixture.Allocate(0x4c);
            IntPtr generatorRecord = fixture.Allocate(4);
            IntPtr generatorVtable = fixture.Allocate(0x1c);
            Marshal.WriteInt32(generatorRecord, 14); // Native category 1, processed after category 0.
            Marshal.WriteIntPtr(generator, generatorVtable);
            Marshal.WriteIntPtr(generator, 0x48, generatorRecord);
            byte[] rateCode = { 0xd9, 0x41, 0x08, 0xc2, 0x04, 0x00 }; // fld [ecx+8]; ret 4
            IntPtr mockRate = IntPtr.Add(fastcallThunks, 0x280);
            Marshal.Copy(rateCode, 0, mockRate, rateCode.Length);
            byte[] updateCode = { 0x8b,0x44,0x24,0x04, 0x89,0x41,0x10,
                0x8b,0x44,0x24,0x08, 0x89,0x41,0x14, 0xa1,0,0,0,0,
                0x89,0x41,0x18, 0xff,0x41,0x1c, 0xc2,0x08,0x00 };
            WriteImm32(updateCode, 15, IntPtr.Add(providerRow, 8).ToInt32());
            IntPtr mockUpdate = IntPtr.Add(fastcallThunks, 0x2c0);
            Marshal.Copy(updateCode, 0, mockUpdate, updateCode.Length);
            Marshal.WriteIntPtr(generatorVtable, 0x14, mockRate);
            Marshal.WriteIntPtr(generatorVtable, 0x18, mockUpdate);
            IntPtr devices = fixture.Allocate(8);
            Marshal.WriteIntPtr(devices, 0, generator); // Reverse storage order makes priority observable.
            Marshal.WriteIntPtr(devices, 4, device);
            Marshal.WriteIntPtr(owner, 0x5bc, devices);
            Marshal.WriteInt32(owner, 0x5c0, 2);
            Expect(applyInto(owner, 1, typedRecord) == IntPtr.Add(typedRecord, 0xb4) &&
                Marshal.ReadIntPtr(device, 0x48) == typedRecord, "native tag-15 descriptor initializer");
            Expect(Marshal.ReadIntPtr(Marshal.ReadIntPtr(owner, 0x0c), 0x24) ==
                IntPtr.Add(module, ControlResourceBudgetCategoryRva), "native descriptor category dispatch");

            float[] budgets = { 0.1f, 0.5f };
            int[] expectedAmounts = { unchecked((int)0x42206666), unchecked((int)0x4220cccd) };
            int[] expectedRatios = { unchecked((int)0x3ecd4fdf), unchecked((int)0x3ecdd2f2) };
            for (int i = 0; i < budgets.Length; i++) {
                Marshal.WriteInt32(generator, 8, FloatBits(budgets[i]));
                Marshal.WriteInt32(generator, 0x1c, 0);
                Marshal.WriteInt32(providerRow, 8, FloatBits(40.0f));
                Marshal.WriteInt32(providerRow, 0x0c, FloatBits(40.0f));
                Marshal.WriteInt32(providerRow, 0x14, FloatBits(0.4f));
                Marshal.WriteInt32(owner, 0x590, FloatBits(40.0f));
                const float dt = 0.5f;
                Expect(gate(device) == 0 && FloatBits(getter(device, 0x100)) == FloatBits(0.4f) &&
                    FloatBits(availability(owner, 0)) == FloatBits(60.0f), "consistent native provider inputs");
                Expect(FloatBits(rate(device, dt)) == FloatBits(-0.2f), "native tag-15 demand at non-unity dt");
                controlNodeUpdateCount = 0;
                update(owner, dt);
                Console.WriteLine("native-control-resource-budget-tag15: controlHash=" + ExpectedControlSha256 +
                    " budgetRva=0x2d340 updateRva=0x22bb0 dt=" + Float(dt) + " budget=" + Float(budgets[i]) +
                    " scale=" + Float(ReadFloat(device, 0x4c)) + " amount=" + Hex(unchecked((uint)Marshal.ReadInt32(providerRow, 8))) +
                    " ratio=" + Hex(unchecked((uint)Marshal.ReadInt32(providerRow, 0x14))) +
                    " total=" + Hex(unchecked((uint)Marshal.ReadInt32(owner, 0x590))) +
                    " processed=" + Hex(unchecked((uint)Marshal.ReadInt32(providerRow, 0x0c))));
                Expect(Marshal.ReadInt32(providerRow, 8) == expectedAmounts[i] &&
                    Marshal.ReadInt32(providerRow, 0x14) == expectedRatios[i] &&
                    Marshal.ReadInt32(owner, 0x590) == expectedAmounts[i] &&
                    Marshal.ReadInt32(providerRow, 0x0c) == expectedAmounts[i], "native budget exact provider result");
                Expect(controlNodeUpdateCount == 1 && controlNodeUpdateIndex == 0 &&
                    controlNodeUpdateSelector == 0x200 && controlNodeUpdateValueBits == unchecked((uint)expectedRatios[i]),
                    "native budget publishes updated provider ratio exactly once");
                Expect(FloatBits(ReadFloat(device, 0x4c)) == FloatBits(i == 0 ? 0.5f : 1.0f), "native budget scale");
                Expect(Marshal.ReadInt32(generator, 0x1c) == 1 &&
                    Marshal.ReadInt32(generator, 0x10) == FloatBits(dt) &&
                    Marshal.ReadInt32(generator, 0x14) == FloatBits((budgets[i] - (i == 0 ? 0.1f : 0.2f) - budgets[i]) / -budgets[i]) &&
                    Marshal.ReadInt32(generator, 0x18) == expectedAmounts[i],
                    "native category priority updates tag 15 before the earlier stored generator");
            }
            NativeControlResourceDeviceConstructor constructGenerator = (NativeControlResourceDeviceConstructor)
                Marshal.GetDelegateForFunctionPointer(IntPtr.Add(module, 0x22980), typeof(NativeControlResourceDeviceConstructor));
            IntPtr nativeGenerator = fixture.Allocate(0x94);
            IntPtr nativeGeneratorRecord = fixture.Allocate(0xb4);
            constructGenerator(nativeGenerator);
            IntPtr nativeGeneratorVtable = Marshal.ReadIntPtr(nativeGenerator);
            Expect(nativeGeneratorVtable == IntPtr.Add(module, 0x3c608) &&
                Marshal.ReadIntPtr(nativeGeneratorVtable, 0x14) == IntPtr.Add(module, 0x229a0) &&
                Marshal.ReadIntPtr(nativeGeneratorVtable, 0x18) == IntPtr.Add(module, 0x22a60),
                "native tag-19 generator keeps its original rate and update methods");
            Marshal.WriteIntPtr(nativeGenerator, 0x3c, owner);
            Marshal.WriteIntPtr(nativeGenerator, 0x44, IntPtr.Add(owner, 4));
            Marshal.WriteInt32(nativeGeneratorRecord, 0, 19);
            Marshal.WriteInt32(nativeGeneratorRecord, 0x18, -1); // Preserve constructor's enabled word 0x20.
            Marshal.WriteInt32(nativeGeneratorRecord, 0x20, FloatBits(0.5f));
            Marshal.WriteIntPtr(devices, 0, nativeGenerator);
            for (int capped = 0; capped < 2; capped++) {
                Marshal.WriteInt32(nativeGeneratorRecord, 0x2c, FloatBits(capped == 0 ? 1.0f : 0.125f));
                Expect(applyInto(owner, 0, nativeGeneratorRecord) == IntPtr.Add(nativeGeneratorRecord, 0xb4),
                    "native tag-19 common record initializer");
                Expect(Marshal.ReadInt32(nativeGenerator, 0x50) == 0x20, "tag-19 constructor enable word is preserved");
                Marshal.WriteInt32(nativeGenerator, 0x4c, FloatBits(1.0f));
                Marshal.WriteInt32(providerRow, 8, FloatBits(40.0f));
                Marshal.WriteInt32(providerRow, 0x0c, FloatBits(40.0f));
                Marshal.WriteInt32(providerRow, 0x14, FloatBits(0.4f));
                Marshal.WriteInt32(owner, 0x590, FloatBits(40.0f));
                NativeControlResourceDeviceRate generatorRate = (NativeControlResourceDeviceRate)
                    Marshal.GetDelegateForFunctionPointer(IntPtr.Add(module, 0x229a0), typeof(NativeControlResourceDeviceRate));
                float produced = generatorRate(nativeGenerator, 0.5f);
                Expect(FloatBits(produced) == FloatBits(capped == 0 ? 0.1f : 0.05f),
                    "real generator rate applies provider ratio and optional coefficient cap");
                update(owner, 0.5f);
                uint amount = unchecked((uint)Marshal.ReadInt32(providerRow, 8));
                uint fraction = unchecked((uint)Marshal.ReadInt32(providerRow, 0x14));
                uint remaining = unchecked((uint)Marshal.ReadInt32(nativeGenerator, 0x4c));
                Console.WriteLine("native-control-real-generator-budget: ctor=0x22980 rate=0x229a0 update=0x22a60" +
                    " cap=" + capped + " produced=" + Float(produced) + " amount=" + Hex(amount) +
                    " ratio=" + Hex(fraction) + " remaining=" + Hex(remaining));
                Expect(amount == (capped == 0 ? 0x42206666u : 0x42203333u) &&
                    fraction == (capped == 0 ? 0x3ecd4fdfu : 0x3ecd0e56u) &&
                    remaining == unchecked((uint)FloatBits(capped == 0 ? 0.75f : 0.0f)),
                    "real generator and consumer update exact provider and reserve values in one budget pass");
                GC.KeepAlive(generatorRate);
            }
            Marshal.WriteIntPtr(devices, 0, generator);
            GC.KeepAlive(constructGenerator);

            // Bounded active class-10 host: no motion object, no deferred packets.
            // These host getters are mocks; the clock, deadline and budget code are native.
            IntPtr host = fixture.Allocate(4);
            IntPtr hostVtable = fixture.Allocate(0x94);
            IntPtr motion = fixture.Allocate(4);
            IntPtr motionVtable = fixture.Allocate(0x34);
            Marshal.WriteIntPtr(host, hostVtable);
            Marshal.WriteIntPtr(motion, motionVtable);
            byte[] getMotion = { 0xb8,0,0,0,0, 0xc2,0x04,0x00 };
            WriteImm32(getMotion, 1, motion.ToInt32());
            Marshal.Copy(getMotion, 0, IntPtr.Add(fastcallThunks, 0x300), getMotion.Length);
            byte[] hostPresent = { 0xb8,1,0,0,0, 0xc2,0x04,0x00 };
            Marshal.Copy(hostPresent, 0, IntPtr.Add(fastcallThunks, 0x320), hostPresent.Length);
            byte[] noMotion = { 0xb8,0xff,0xff,0xff,0xff, 0xc3 };
            Marshal.Copy(noMotion, 0, IntPtr.Add(fastcallThunks, 0x340), noMotion.Length);
            Marshal.WriteIntPtr(hostVtable, 0x90, IntPtr.Add(fastcallThunks, 0x300));
            Marshal.WriteIntPtr(hostVtable, 0x0c, IntPtr.Add(fastcallThunks, 0x320));
            Marshal.WriteIntPtr(motionVtable, 0x30, IntPtr.Add(fastcallThunks, 0x340));
            Marshal.WriteIntPtr(owner, 0x40, host);
            Marshal.WriteIntPtr(owner, 0x1c, host);
            Marshal.WriteInt32(owner, 0x50, 10);
            Marshal.WriteByte(owner, 0xe0, 1);
            Marshal.WriteInt32(owner, 0xe4, 0);
            Marshal.WriteInt32(owner, 0xec, 0);
            Marshal.WriteByte(owner, 0x598, 0);
            Marshal.WriteInt32(owner, 0x588, 0);
            Marshal.WriteInt32(providerRow, 8, FloatBits(40.0f));
            Marshal.WriteInt32(providerRow, 0x0c, FloatBits(40.0f));
            Marshal.WriteInt32(providerRow, 0x14, FloatBits(0.4f));
            Marshal.WriteInt32(owner, 0x590, FloatBits(40.0f));
            Marshal.WriteInt32(generator, 8, FloatBits(0.1f));
            Marshal.WriteInt32(generator, 0x1c, 0);
            NativeControlTimedUpdate timedUpdate = (NativeControlTimedUpdate)Marshal.GetDelegateForFunctionPointer(
                IntPtr.Add(module, ControlTimedUpdateRva), typeof(NativeControlTimedUpdate));
            IntPtr period = IntPtr.Add(module, 0x42480);
            IntPtr randomState = IntPtr.Add(module, 0x43228);
            int savedPeriod = Marshal.ReadInt32(period);
            int savedRandom = Marshal.ReadInt32(randomState);
            Expect(savedPeriod == FloatBits(250.0f), "native Control settings constructor default resource period");
            try {
                Marshal.WriteInt32(period, FloatBits(500.0f));
                Marshal.WriteInt32(randomState, 0); // Fixed point: random offset is -period/8.
                timedUpdate(owner, 0.5f);
                Expect(ReadFloat(owner, 0xe4) == 0.0f && ReadFloat(owner, 0xe8) == 0.5f,
                    "active clock skips less than one millisecond");
                timedUpdate(owner, 436.0f);
                Expect(ReadFloat(owner, 0xe4) == 436.0f && ReadFloat(owner, 0xec) == 0.0f &&
                    Marshal.ReadInt32(generator, 0x1c) == 0, "active clock skips before resource deadline");
                timedUpdate(owner, 437.5f);
                Expect(ReadFloat(owner, 0xe4) == 437.5f && ReadFloat(owner, 0xec) == 0.0f &&
                    Marshal.ReadInt32(generator, 0x1c) == 0 && ReadFloat(providerRow, 8) == 40.0f,
                    "active resource clock skips exact randomized deadline equality");
                timedUpdate(owner, 500.0f);
                Expect(ReadFloat(owner, 0xec) == 500.0f && ReadFloat(owner, 0xe8) == 62.5f &&
                    Marshal.ReadInt32(generator, 0x10) == FloatBits(0.5f) &&
                    Marshal.ReadInt32(generator, 0x1c) == 1 &&
                    Marshal.ReadInt32(providerRow, 8) == expectedAmounts[0] &&
                    Marshal.ReadInt32(providerRow, 0x14) == expectedRatios[0],
                    "active clock converts elapsed since last resource cycle to seconds and executes native budget");
                Expect(Marshal.ReadByte(owner, 0x598) == 1 && ReadFloat(owner, 0x588) == 0.0f,
                    "budget dirties amount after the preceding contact/aggregate update");
                Console.WriteLine("native-control-active-clock: class=10 e0=1 period=500 seed=0 sub-ms=skip" +
                    " equality=437.5/skip now=500 e8=62.5 resourceDt=0.5 amount=0x42206666 ec=500");

                // Isolate cadence after the budget vector: no device or stage callbacks run.
                Marshal.WriteInt32(owner, 0x5c0, 0);
                Marshal.WriteByte(owner, 0x598, 0);
                Marshal.WriteInt32(owner, 0xe4, 0);
                Marshal.WriteInt32(owner, 0xec, 0);
                Marshal.WriteInt32(randomState, unchecked((int)0x12345678));
                timedUpdate(owner, 0.5f);
                Expect(unchecked((uint)Marshal.ReadInt32(randomState)) == 0x12345678 &&
                    ReadFloat(owner, 0xe4) == 0.0f, "sub-millisecond active calls do not draw random jitter");
                uint[] expectedRandom = { 0xb7debec4, 0x91b9ca56, 0x4dc90515, 0x610747e3 };
                float[] expectedResourceClock = { 0, 0, 502, 502 };
                for (int i = 0; i < expectedRandom.Length; i++) {
                    float now = 500.0f + i;
                    timedUpdate(owner, now);
                    Expect(unchecked((uint)Marshal.ReadInt32(randomState)) == expectedRandom[i] &&
                        ReadFloat(owner, 0xec) == expectedResourceClock[i] &&
                        ReadFloat(owner, 0xe4) == now,
                        "active clock draws once per accepted call and tests a fresh randomized deadline");
                    Console.WriteLine("native-control-randomized-clock: now=" + Float(now) +
                        " rng=" + Hex(expectedRandom[i]) + " resourceClock=" + Float(expectedResourceClock[i]));
                }
                Marshal.WriteInt32(owner, 0xe4, 0);
                Marshal.WriteInt32(owner, 0xec, 0);
                Marshal.WriteInt32(randomState, unchecked((int)0xffff0000));
                timedUpdate(owner, 500.0f);
                Expect(unchecked((uint)Marshal.ReadInt32(randomState)) == 0x8000ffff &&
                    ReadFloat(owner, 0xec) == 0.0f,
                    "nonzero random state can produce exact deadline equality, which still skips");
                timedUpdate(owner, 501.0f);
                Expect(unchecked((uint)Marshal.ReadInt32(randomState)) == 0x3ffe7ffe &&
                    ReadFloat(owner, 0xec) == 501.0f, "next accepted call redraws jitter after equality");
                Marshal.WriteInt32(owner, 0xd4, 2);
                timedUpdate(owner, 502.0f);
                Console.WriteLine("native-control-forced-clock: rng=" + Hex(unchecked((uint)Marshal.ReadInt32(randomState))) +
                    " resourceClock=" + Float(ReadFloat(owner, 0xec)) + " counter=" + Marshal.ReadInt32(owner, 0xd4));
                Expect(unchecked((uint)Marshal.ReadInt32(randomState)) == 0xdffdc002 &&
                    ReadFloat(owner, 0xec) == 502.0f && Marshal.ReadInt32(owner, 0xd4) == 1,
                    "forced resource cycle still consumes RNG and decrements its counter");
                Marshal.WriteByte(owner, 0xe0, 0);
                timedUpdate(owner, 503.0f);
                Expect(unchecked((uint)Marshal.ReadInt32(randomState)) == 0xdffdc002 &&
                    ReadFloat(owner, 0xec) == 503.0f && Marshal.ReadInt32(owner, 0xd4) == 1,
                    "inactive clock synchronizes resource time without drawing RNG or consuming the forced counter");
                Console.WriteLine("native-control-randomized-clock-gates: equality=skip redraw=due forced=rng+decrement inactive=no-rng/no-decrement");
            } finally {
                Marshal.WriteInt32(period, savedPeriod);
                Marshal.WriteInt32(randomState, savedRandom);
            }
            GC.KeepAlive(timedUpdate);

        }
    }

    private static void RunNativeControlActivationCase(IntPtr module) {
        NativeControlOwnerConstructor construct = (NativeControlOwnerConstructor)Marshal.GetDelegateForFunctionPointer(
            IntPtr.Add(module, 0x31490), typeof(NativeControlOwnerConstructor));
        using (Fixture fixture = NewFixture()) {
            IntPtr owner = fixture.Allocate(0x670);
            Expect(construct(owner, IntPtr.Zero, 4) == owner, "native derived Control constructor");
            IntPtr host = fixture.Allocate(4);
            IntPtr hostVtable = fixture.Allocate(0x24);
            IntPtr ani = fixture.Allocate(4);
            IntPtr aniVtable = fixture.Allocate(0x84);
            IntPtr scale = fixture.Allocate(4);
            IntPtr scaleVtable = fixture.Allocate(0x3c);
            IntPtr matrix = fixture.Allocate(64);
            for (int i = 0; i < 16; i++) Marshal.WriteInt32(matrix, i * 4, FloatBits(i % 5 == 0 ? 1.0f : 0.0f));
            Marshal.WriteIntPtr(host, hostVtable);
            Marshal.WriteIntPtr(ani, aniVtable);
            Marshal.WriteIntPtr(scale, scaleVtable);
            List<string> trace = new List<string>();
            bool wrongArguments = false;
            InterfaceGetProperty transform = delegate(IntPtr self, uint selector, IntPtr result) {
                wrongArguments |= self != host || selector != 2;
                trace.Add("transform"); Marshal.WriteIntPtr(result, matrix); return 1;
            };
            NativeControlScaleQuery queryScale = delegate(IntPtr self, IntPtr result) {
                wrongArguments |= self != scale; trace.Add("scale");
                for (int i = 0; i < 3; i++) Marshal.WriteInt32(result, i * 4, FloatBits(1.0f));
            };
            // Explicit zero-geometry host mocks: capture parameters and supply outputs.
            InterfaceGetProperty geometryState = delegate(IntPtr self, uint first, IntPtr second) {
                wrongArguments |= self != ani; trace.Add("geometry-state");
                Marshal.WriteInt32(new IntPtr(unchecked((int)first)), 0);
                Marshal.WriteInt32(second, 0); return 1;
            };
            InterfaceGetProperty geometrySet = delegate(IntPtr self, uint node, IntPtr value) {
                wrongArguments |= self != ani || node != 0;
                trace.Add("geometry-set:" + Hex(unchecked((uint)value.ToInt32()))); return 1;
            };
            InterfaceGetProperty geometryPoint = delegate(IntPtr self, uint node, IntPtr result) {
                wrongArguments |= self != ani || node != 0; trace.Add("geometry-point");
                for (int i = 0; i < 3; i++) Marshal.WriteInt32(result, i * 4, 0); return 1;
            };
            Marshal.WriteIntPtr(hostVtable, 0x20, Marshal.GetFunctionPointerForDelegate(transform));
            Marshal.WriteIntPtr(scaleVtable, 0x38, Marshal.GetFunctionPointerForDelegate(queryScale));
            Marshal.WriteIntPtr(aniVtable, 0x68, Marshal.GetFunctionPointerForDelegate(geometryState));
            Marshal.WriteIntPtr(aniVtable, 0x64, Marshal.GetFunctionPointerForDelegate(geometrySet));
            Marshal.WriteIntPtr(aniVtable, 0x80, Marshal.GetFunctionPointerForDelegate(geometryPoint));
            Marshal.WriteIntPtr(owner, 0x1c, host);
            Marshal.WriteIntPtr(owner, 0x20, ani);
            Marshal.WriteIntPtr(owner, 0x24, scale);
            IntPtr eventVtable = Marshal.ReadIntPtr(owner, 4);
            Expect(Marshal.ReadIntPtr(eventVtable, 8) == IntPtr.Add(module, 0x31ae0), "native activation event interface");
            NativeControlEvent activate = (NativeControlEvent)Marshal.GetDelegateForFunctionPointer(
                WriteStdCallToFastcallThunk(0x380, Marshal.ReadIntPtr(eventVtable, 8)), typeof(NativeControlEvent));
            foreach (uint clock in new uint[] { 1000, 33554433 }) {
                trace.Clear();
                activate(IntPtr.Add(owner, 4), 4, clock);
                float absolute = (float)clock;
                float baseline = absolute - 100.0f;
                Expect(Marshal.ReadByte(owner, 0xe0) == 1 &&
                    ReadFloat(owner, 0xe4) == baseline && ReadFloat(owner, 0xe8) == baseline &&
                    ReadFloat(owner, 0xec) == baseline && ReadFloat(owner, 0xd8) == absolute && ReadFloat(owner, 0xdc) == absolute,
                    "native event 4 activates the object and initializes absolute clock baselines");
                Expect(!wrongArguments && String.Join(",", trace.ToArray()) ==
                    "transform,scale,geometry-state,geometry-set:0x00000000,geometry-point,geometry-state,geometry-set:0x3f800000",
                    "native activation zero-geometry host callback order and arguments");
                Expect(Marshal.ReadInt32(owner, 0x370) == 1, "only first activation queues the initial command");
                Console.WriteLine("native-control-activation: eventRva=0x31ae0 handlerRva=0x314f0 class=4 clock=" + clock +
                    " absolute=" + Float(absolute) + " e4/e8/ec=" + Float(baseline) + " e0=1 pending=1 callbacks=" + String.Join(",", trace.ToArray()));
            }
            GC.KeepAlive(transform); GC.KeepAlive(queryScale); GC.KeepAlive(geometryState);
            GC.KeepAlive(geometrySet); GC.KeepAlive(geometryPoint); GC.KeepAlive(activate);
        }
        GC.KeepAlive(construct);
    }

    private static void RunNativeArealMapComponentBuilderCase(IntPtr module) {
        using (ArealMapFixture fixture = new ArealMapFixture()) {
            fixture.OwnerVtable = fixture.Allocate(0x40);
            fixture.Owner = fixture.Allocate(4);
            Marshal.WriteIntPtr(fixture.Owner, 0, fixture.OwnerVtable);
            Marshal.WriteIntPtr(fixture.OwnerVtable, 0x34, Marshal.GetFunctionPointerForDelegate(ArealMapEventCallback));

            ComponentNode tree = new ComponentNode("objects.rlb", "i_pws_f_df", 1, 9,
                new ComponentNode("objects.rlb", "nested-slot", 1, 5,
                    new ComponentNode("objects.rlb", "nested-child", 1, 12)),
                new ComponentNode("objects.rlb", "zero-kind-parent", 0, 77,
                    new ComponentNode("objects.rlb", "skipped-descendant", 1, 88)),
                new ComponentNode("objects.rlb", "later-sibling", 1, 3));
            IntPtr rootNode = fixture.Allocate(0x78);
            PopulateUnitDatNode(fixture, rootNode, tree);

            NativeFastcallComponentBuilder build = (NativeFastcallComponentBuilder)Marshal.GetDelegateForFunctionPointer(
                WriteStdCallToFastcallThunk(0x180, IntPtr.Add(module, ArealMapUnitDatComponentBuilderRva)),
                typeof(NativeFastcallComponentBuilder));
            activeArealMap = fixture;
            build(fixture.Owner, rootNode, 0);

            Expect(fixture.CallbackError == null, "ArealMap descriptor callback owner and pointer");
            Expect(fixture.Descriptors.Count == 4, "raw kind zero skips its node and descendants while later siblings continue");
            string[] expectedResources = new string[] { "i_pws_f_df", "nested-slot", "nested-child", "later-sibling" };
            uint[] expectedParents = new uint[] { 0, 41, 113, 41 };
            uint[] expectedIndices = new uint[] { 9, 5, 12, 3 };
            uint[] expectedAssignedIds = new uint[] { 41, 113, 127, 179 };
            for (int i = 0; i < fixture.Descriptors.Count; i++) {
                CapturedComponentDescriptor descriptor = fixture.Descriptors[i];
                Expect(descriptor.ArchiveName == "objects.rlb" && descriptor.ResourceName == expectedResources[i] &&
                    descriptor.ParentId == expectedParents[i] && descriptor.LocalIndex == expectedIndices[i] &&
                    descriptor.AssignedIdBeforeCallback == 0xffffffff && descriptor.AssignedId == expectedAssignedIds[i] &&
                    descriptor.Message == 6 && descriptor.Subcode == 0x80000020,
                    "ArealMap descriptor " + i + " fields and callback-assigned parent ID");
            }
            Console.WriteLine("native-arealmap-unitdat-components: events=" + fixture.Descriptors.Count + " archive=objects.rlb" +
                " parentIds=[" + String.Join(",", Array.ConvertAll(fixture.Descriptors.ToArray(), d => d.ParentId.ToString())) +
                "] localIndices=[" + String.Join(",", Array.ConvertAll(fixture.Descriptors.ToArray(), d => d.LocalIndex.ToString())) +
                "]; kind0-subtree=skipped sparse-assigned-ids=propagated");
            GC.KeepAlive(build);
        }
    }

    private static void RunNativeAniMeshScaleLoadSnapshot(IntPtr module) {
        uint[] actual = new uint[] {
            unchecked((uint)Marshal.ReadInt32(IntPtr.Add(module, 0x26068))),
            unchecked((uint)Marshal.ReadInt32(IntPtr.Add(module, 0x2606c))),
            unchecked((uint)Marshal.ReadInt32(IntPtr.Add(module, 0x26070)))
        };
        Expect(actual[0] == 0x3f800000 && actual[1] == 0x3f800000 && actual[2] == 0x3f800000,
            "AniMesh module load initializes all three runtime scale globals to 1.0");
        Console.WriteLine("native-anim-mesh-scale-load-snapshot: aniMeshHash=" + ExpectedAniMeshSha256 +
            " globals=[" + String.Join(",", Array.ConvertAll(actual, Hex)) + "]");
    }

    private static void RunNativeAniMeshNodeMetadataCases(IntPtr module) {
        NativeAniMeshNodeMetadataQuery query = (NativeAniMeshNodeMetadataQuery)Marshal.GetDelegateForFunctionPointer(
            IntPtr.Add(module, AniMeshNodeMetadataQueryRva), typeof(NativeAniMeshNodeMetadataQuery));
        NativeAniMeshNodeStageCount stageCount = (NativeAniMeshNodeStageCount)Marshal.GetDelegateForFunctionPointer(
            IntPtr.Add(module, AniMeshNodeStageCountRva), typeof(NativeAniMeshNodeStageCount));

        using (Fixture fixture = new Fixture()) {
            IntPtr owner = fixture.Allocate(0x1b0);
            IntPtr meshRows = fixture.Allocate(3 * 0x130);
            IntPtr nodeData = fixture.Allocate(3 * 0x26);
            IntPtr slotData = fixture.Allocate(3 * 0x44);
            IntPtr meshData = fixture.Allocate(3 * IntPtr.Size);
            IntPtr result = fixture.Allocate(IntPtr.Size);
            Marshal.WriteIntPtr(meshData, 0, nodeData);
            Marshal.WriteIntPtr(meshData, 2 * IntPtr.Size, slotData);
            Marshal.WriteIntPtr(owner, 0x1a8, meshRows);
            Marshal.WriteInt32(owner, 0x1ac, 3);

            ushort[] headerFlags = new ushort[] { 0x8201, 0x0180, 0x0400 };
            int[] rawParentIndices = new int[] { 3, 0, 1 };
            int[] runtimeParentIndices = new int[] { -1, 22, 7 };
            for (int nodeIndex = 0; nodeIndex < 3; nodeIndex++) {
                IntPtr rawNode = IntPtr.Add(nodeData, nodeIndex * 0x26);
                for (int word = 0; word < 19; word++) Marshal.WriteInt16(rawNode, word * 2, -1);
                Marshal.WriteInt16(rawNode, 0, unchecked((short)headerFlags[nodeIndex]));
                Marshal.WriteInt16(rawNode, 2, unchecked((short)rawParentIndices[nodeIndex]));

                IntPtr meshRow = IntPtr.Add(meshRows, nodeIndex * 0x130);
                Marshal.WriteInt32(meshRow, 8, nodeIndex);
                Marshal.WriteInt32(meshRow, 0x18, runtimeParentIndices[nodeIndex]);
                Marshal.WriteIntPtr(meshRow, 0x12c, meshData);
            }

            // Node 0: state0 present, state1 absent, state2 present. Native
            // GetStateCount stops at the first missing state, so it returns 1.
            WriteNode38Slot(nodeData, 0, 0, 0);
            WriteNode38Slot(nodeData, 0, 2, 2);
            // Node 1: the prefix is empty even though later states are present.
            WriteNode38Slot(nodeData, 1, 1, 1);
            WriteNode38Slot(nodeData, 1, 2, 2);
            // Node 2: all three consecutive state slots are present.
            WriteNode38Slot(nodeData, 2, 0, 0);
            WriteNode38Slot(nodeData, 2, 1, 1);
            WriteNode38Slot(nodeData, 2, 2, 2);

            for (uint nodeIndex = 0; nodeIndex < 3; nodeIndex++) {
                Marshal.WriteIntPtr(result, IntPtr.Zero);
                Expect(NativeBoolean(query(owner, nodeIndex, 0x0d, result)) == 1,
                    "AniMesh selector D returns runtime parent metadata");
                IntPtr parent = Marshal.ReadIntPtr(result);
                Expect(runtimeParentIndices[nodeIndex] != rawParentIndices[nodeIndex],
                    "AniMesh runtime parent fixture differs from the raw Node38 parent");
                Expect(Marshal.ReadInt32(parent) == runtimeParentIndices[nodeIndex],
                    "AniMesh selector D copies the indexed global runtime parent, not the raw Node38 link");

                Marshal.WriteIntPtr(result, IntPtr.Zero);
                Expect(NativeBoolean(query(owner, nodeIndex, 0x0e, result)) == 1,
                    "AniMesh selector E returns the Node38 header word");
                IntPtr flags = Marshal.ReadIntPtr(result);
                Expect(unchecked((uint)Marshal.ReadInt32(flags)) == headerFlags[nodeIndex],
                    "AniMesh selector E points to the indexed Node38 hdr0");
            }

            uint[] expectedStageCounts = new uint[] { 1, 0, 3 };
            for (uint nodeIndex = 0; nodeIndex < expectedStageCounts.Length; nodeIndex++) {
                Expect(stageCount(owner, nodeIndex) == expectedStageCounts[nodeIndex],
                    "AniMesh stage count is the consecutive state0-2 prefix");
            }
            Console.WriteLine("native-anim-mesh-control-node-metadata: aniMeshHash=" + ExpectedAniMeshSha256 +
                " D=runtime-parent E=Node38-hdr0 stage-prefix=[1,0,3]");
        }
        GC.KeepAlive(query);
        GC.KeepAlive(stageCount);
    }

    private static void RunNativeControlProviderInitialStateCase(IntPtr controlModule, IntPtr aniMeshModule) {
        const int rowCount = 4;
        NativeControlOwnerConstructor constructOwner = (NativeControlOwnerConstructor)Marshal.GetDelegateForFunctionPointer(
            IntPtr.Add(controlModule, ControlOwnerConstructorRva), typeof(NativeControlOwnerConstructor));
        NativeControlProviderRowConstructor constructProviderRow = (NativeControlProviderRowConstructor)
            Marshal.GetDelegateForFunctionPointer(IntPtr.Add(controlModule, ControlProviderRowConstructorRva),
                typeof(NativeControlProviderRowConstructor));
        NativeControlProviderInitialState initialize = (NativeControlProviderInitialState)
            Marshal.GetDelegateForFunctionPointer(IntPtr.Add(controlModule, ControlProviderInitialStateRva),
                typeof(NativeControlProviderInitialState));
        using (Fixture fixture = NewFixture()) {
            IntPtr owner = fixture.Allocate(0x664);
            Expect(constructOwner(owner, IntPtr.Zero, 4) == owner,
                "Control constructor initializes a synthetic class-4 owner without a host object");
            Expect(unchecked((uint)Marshal.ReadInt32(owner, 0x548)) == 0x3f800000 &&
                unchecked((uint)Marshal.ReadInt32(owner, 0x660)) == 0x3f800000,
                "Control constructor starts both provider getter scale factors at 1.0");

            IntPtr aniOwner = fixture.Allocate(0x1b0);
            IntPtr aniVtable = fixture.Allocate(0x4c);
            IntPtr meshRows = fixture.Allocate(rowCount * 0x130);
            IntPtr nodeData = fixture.Allocate(rowCount * 0x26);
            IntPtr slotData = fixture.Allocate(rowCount * 0x44);
            IntPtr meshData = fixture.Allocate(3 * IntPtr.Size);
            IntPtr providerRows = fixture.Allocate(rowCount * 0x2c);
            Marshal.WriteIntPtr(meshData, 0, nodeData);
            Marshal.WriteIntPtr(meshData, 2 * IntPtr.Size, slotData);
            Marshal.WriteIntPtr(aniOwner, 0, aniVtable);
            Marshal.WriteIntPtr(aniOwner, 0x1a8, meshRows);
            Marshal.WriteInt32(aniOwner, 0x1ac, rowCount);
            Marshal.WriteIntPtr(aniVtable, 0x0c, IntPtr.Add(aniMeshModule, AniMeshNodeMetadataQueryRva));
            Marshal.WriteIntPtr(aniVtable, 0x34, Marshal.GetFunctionPointerForDelegate(ControlNodeUpdateCallback));
            Marshal.WriteIntPtr(aniVtable, 0x48, IntPtr.Add(aniMeshModule, AniMeshNodeStageCountRva));

            ushort[] headers = { 0x8201, 0x0180, 0x0400, 0x0001 };
            int[] rawParents = { 3, 0, 1, 0 };
            int[] runtimeParents = { -1, 22, 7, 11 };
            float[] sourceMaxima = { 30.0f, 60.0f, 50.0f, 0.0f };
            for (int nodeIndex = 0; nodeIndex < rowCount; nodeIndex++) {
                IntPtr rawNode = IntPtr.Add(nodeData, nodeIndex * 0x26);
                for (int word = 0; word < 19; word++) Marshal.WriteInt16(rawNode, word * 2, -1);
                Marshal.WriteInt16(rawNode, 0, unchecked((short)headers[nodeIndex]));
                Marshal.WriteInt16(rawNode, 2, unchecked((short)rawParents[nodeIndex]));

                IntPtr meshRow = IntPtr.Add(meshRows, nodeIndex * 0x130);
                Marshal.WriteInt32(meshRow, 8, nodeIndex);
                Marshal.WriteInt32(meshRow, 0x18, runtimeParents[nodeIndex]);
                Marshal.WriteIntPtr(meshRow, 0x12c, meshData);

                IntPtr descriptor = fixture.Allocate(0x4c);
                Marshal.WriteInt32(descriptor, 0x04, FloatBits(sourceMaxima[nodeIndex]));
                constructProviderRow(IntPtr.Add(providerRows, nodeIndex * 0x2c), descriptor);
            }
            // Stage counts are the consecutive state-0/1/2 prefix; Control
            // changes an empty prefix to one for its initial provider row.
            WriteNode38Slot(nodeData, 0, 0, 0);
            WriteNode38Slot(nodeData, 0, 2, 2);
            WriteNode38Slot(nodeData, 1, 1, 1);
            WriteNode38Slot(nodeData, 1, 2, 2);
            WriteNode38Slot(nodeData, 2, 0, 0);
            WriteNode38Slot(nodeData, 2, 1, 1);
            WriteNode38Slot(nodeData, 2, 2, 2);

            Marshal.WriteIntPtr(owner, 0x20, aniOwner);
            Marshal.WriteIntPtr(owner, 0x55c, providerRows);
            Marshal.WriteInt32(owner, 0x560, rowCount);
            IntPtr providerVtable = Marshal.ReadIntPtr(IntPtr.Add(owner, 4));
            Expect(providerVtable != IntPtr.Zero &&
                Marshal.ReadIntPtr(providerVtable, 0x0c) == IntPtr.Add(controlModule, ControlProviderType1GetterRva),
                "Control constructor installs the original selector-3 provider vtable");

            controlNodeUpdateCount = 0;
            controlNodeUpdateIndices.Clear();
            initialize(owner);
            Expect(Marshal.ReadByte(owner, 0x598) == 1,
                "Control constructor starts the provider aggregate dirty and f940 preserves it");
            uint[] expectedFlags = { 0x09, 0x02, 0x04, 0x08 };
            uint[] expectedStages = { 1, 1, 3, 1 };
            for (int nodeIndex = 0; nodeIndex < rowCount; nodeIndex++) {
                IntPtr row = IntPtr.Add(providerRows, nodeIndex * 0x2c);
                Console.WriteLine("native-control-f940-row=" + nodeIndex + " parent=" + Marshal.ReadInt32(row, 0x04) +
                    " amount=" + Hex(unchecked((uint)Marshal.ReadInt32(row, 0x08))) +
                    " processed=" + Hex(unchecked((uint)Marshal.ReadInt32(row, 0x0c))) +
                    " normalized=" + Hex(unchecked((uint)Marshal.ReadInt32(row, 0x14))) +
                    " stages=" + Hex(unchecked((uint)Marshal.ReadInt32(row, 0x1c))) +
                    " flags=" + Hex(unchecked((uint)Marshal.ReadInt32(row, 0x28))));
                uint expectedAmount = unchecked((uint)FloatBits(sourceMaxima[nodeIndex]));
                Expect(Marshal.ReadInt32(row, 0x04) == runtimeParents[nodeIndex] &&
                    unchecked((uint)Marshal.ReadInt32(row, 0x08)) == expectedAmount &&
                    unchecked((uint)Marshal.ReadInt32(row, 0x0c)) == expectedAmount &&
                    unchecked((uint)Marshal.ReadInt32(row, 0x14)) == 0x3f800000 &&
                    unchecked((uint)Marshal.ReadInt32(row, 0x1c)) == expectedStages[nodeIndex] &&
                    unchecked((uint)Marshal.ReadInt32(row, 0x28)) == expectedFlags[nodeIndex],
                    "Control f940 initializes row " + nodeIndex + " from AniMesh metadata and selector-3 amount");
            }
            Expect(unchecked((uint)Marshal.ReadInt32(owner, 0x590)) == 0x430c0000 &&
                unchecked((uint)Marshal.ReadInt32(owner, 0x58c)) == 0x430c0000,
                "Control f940 stores the sum of initial row amounts in both provider aggregates");
            Expect(controlNodeUpdateCount == rowCount &&
                String.Join(",", controlNodeUpdateIndices.ConvertAll(index => index.ToString()).ToArray()) == "0,1,2,3" &&
                controlNodeUpdateSelector == 0x200 && controlNodeUpdateValueBits == 0x3f800000,
                "Control f940 notifies the host once per row in native append order");

            // Selector 3 is the shared maximum getter. A non-unity owner scale
            // exercises its native multiplication before the same initializer.
            Marshal.WriteInt32(owner, 0x660, FloatBits(1.5f));
            Marshal.WriteInt32(owner, 0x548, FloatBits(0.5f));
            controlNodeUpdateCount = 0;
            controlNodeUpdateIndices.Clear();
            initialize(owner);
            uint[] scaledAmounts = { 0x41b40000, 0x42340000, 0x42160000, 0x00000000 };
            for (int nodeIndex = 0; nodeIndex < rowCount; nodeIndex++) {
                IntPtr row = IntPtr.Add(providerRows, nodeIndex * 0x2c);
                Expect(unchecked((uint)Marshal.ReadInt32(row, 0x08)) == scaledAmounts[nodeIndex] &&
                    unchecked((uint)Marshal.ReadInt32(row, 0x0c)) == scaledAmounts[nodeIndex],
                    "Control f940 selector-3 scaled amount row " + nodeIndex);
            }
            Expect(unchecked((uint)Marshal.ReadInt32(owner, 0x590)) == 0x42d20000 &&
                unchecked((uint)Marshal.ReadInt32(owner, 0x58c)) == 0x42d20000,
                "Control f940 aggregates its non-unity selector-3 results");
            Expect(controlNodeUpdateCount == rowCount &&
                String.Join(",", controlNodeUpdateIndices.ConvertAll(index => index.ToString()).ToArray()) == "0,1,2,3",
                "Control f940 retains callback order for non-unity amounts");

            Console.WriteLine("native-control-provider-initial-state: controlHash=" + ExpectedControlSha256 +
                " rva=0xf940 selector3=0xdc40 rows=4 unity=[30,60,50,0] nonunity=[22.5,45,37.5,0]" +
                " current=processed=sourceMax*owner660*owner548 ratio=1 parents=[-1,22,7,11]");
            GC.KeepAlive(constructOwner);
            GC.KeepAlive(constructProviderRow);
            GC.KeepAlive(initialize);
        }
    }

    private static void WriteNode38Slot(IntPtr nodeData, int nodeIndex, int state, ushort slotIndex) {
        int wordIndex = nodeIndex * 19 + 4 + state * 5;
        Marshal.WriteInt16(nodeData, wordIndex * 2, unchecked((short)slotIndex));
    }

    private static void RunNativeControlIntoOverrideCase(IntPtr module) {
        NativeControlResourceComponentOverride applyOverride = (NativeControlResourceComponentOverride)Marshal.GetDelegateForFunctionPointer(
            IntPtr.Add(module, ControlResourceComponentOverrideRva), typeof(NativeControlResourceComponentOverride));
        RunNativeControlOverrideReceiverCase(module, 19, 0x94, ControlResourceDeviceConstructorRva,
            ControlResourceTag19VtableRva, ControlResourceTag19StringPairNoOpRva, applyOverride);
        RunNativeControlOverrideReceiverCase(module, 9, 0x120, ControlResourceTag9ConstructorRva,
            ControlResourceTag9VtableRva, ControlResourceTag9StringPairRva, applyOverride);
        RunNativeControlOverrideReceiverCase(module, 15, 0x98, ControlResourceTag15ConstructorRva,
            ControlResourceTag15VtableRva, ControlResourceNoOpStringPairRva, applyOverride);
        RunNativeControlOverrideReceiverCase(module, 25, 0xa4, ControlResourceTag25ConstructorRva,
            ControlResourceTag25VtableRva, ControlResourceNoOpStringPairRva, applyOverride);
        RunNativeControlOverrideReceiverCase(module, 4, 0xdc, 0x233d0,
            0x3c720, ControlResourceNoOpStringPairRva, applyOverride);
        RunNativeControlOverrideReceiverCase(module, 10, 0x94, 0x26470,
            0x3ca20, ControlResourceNoOpStringPairRva, applyOverride);
        RunNativeControlOverrideReceiverCase(module, 27, 0x94, 0x218b0,
            0x3c528, ControlResourceNoOpStringPairRva, applyOverride);
        RunNativeControlOverrideReceiverCase(module, 5, 0xa4, 0x20800,
            0x3cdf8, ControlResourceNoOpStringPairRva, applyOverride);
        RunNativeControlOverrideReceiverCase(module, 8, 0xbc, 0x24310,
            0x3c800, ControlResourceNoOpStringPairRva, applyOverride);
        RunNativeControlOverrideReceiverCase(module, 21, 0xa4, 0x20800,
            0x3c448, ControlResourceNoOpStringPairRva, applyOverride);
        RunNativeControlOverrideReceiverCase(module, 2, 0x184, 0x294c0,
            0x3cbd8, 0x29940, applyOverride);
        RunNativeControlOverrideReceiverCase(module, 3, 0xa4, 0x20800,
            0x3c448, ControlResourceNoOpStringPairRva, applyOverride);
        GC.KeepAlive(applyOverride);
    }

    private static void RunNativeControlOverrideReceiverCase(IntPtr module, int tag, int objectSize,
        int constructorRva, int vtableRva, int stringPairRva,
        NativeControlResourceComponentOverride applyOverride) {
        NativeControlResourceDeviceConstructor construct = (NativeControlResourceDeviceConstructor)Marshal.GetDelegateForFunctionPointer(
            IntPtr.Add(module, constructorRva), typeof(NativeControlResourceDeviceConstructor));
        IntPtr expectedVtable = IntPtr.Add(module, vtableRva);
        Expect(Marshal.ReadIntPtr(expectedVtable, 0x1c) == IntPtr.Add(module, tag == 2 ? 0x29650 : ControlResourceCommonSetterRva),
            "tag-" + tag + " vtable reader resolves to its native implementation");
        Expect(Marshal.ReadIntPtr(expectedVtable, 0x24) == IntPtr.Add(module, stringPairRva),
            "tag-" + tag + " vtable string-pair slot matches its native callback");
        using (Fixture fixture = NewFixture()) {
            IntPtr owner = fixture.Allocate(0x5c8);
            IntPtr deviceArray = fixture.Allocate(2 * IntPtr.Size);
            IntPtr targetDevice = (tag == 3 || tag == 5) ? IntPtr.Zero : fixture.Allocate(objectSize);
            bool variableTail = tag == 2 || tag == 3 || tag == 4 || tag == 5 || tag == 8 || tag == 10 || tag == 21 || tag == 27;
            IntPtr typedRecord = fixture.Allocate(variableTail ? 0xc0 : 0xb4);
            if (tag == 3 || tag == 5) {
                // Tag 3 follows the default factory; case 5 also assigns a derived vtable.
                Marshal.WriteIntPtr(owner, 0x5bc, deviceArray);
                Marshal.WriteInt32(owner, 0x5c4, 2);
                Marshal.WriteInt32(typedRecord, 0, tag);
                Marshal.WriteInt32(typedRecord, 0x18, -1);
                NativeControlDeviceFactory create = (NativeControlDeviceFactory)Marshal.GetDelegateForFunctionPointer(
                    IntPtr.Add(module, 0x2d4b0), typeof(NativeControlDeviceFactory));
                Expect(create(owner, 0, 0, typedRecord) == IntPtr.Add(typedRecord, 0xb4), "native tag-" + tag + " factory cursor");
                targetDevice = Marshal.ReadIntPtr(deviceArray);
                GC.KeepAlive(create);
            } else {
                Expect(construct(targetDevice) == targetDevice,
                    "native tag-" + tag + " constructor returns its exact-size device object");
            }
            if (variableTail) {
                int expectedWord = tag == 4 ? 0x200 : (tag == 3 || tag == 5 || tag == 8 || tag == 21) ? 5 : tag == 10 ? 0x2000 : 0;
                Expect(Marshal.ReadInt32(targetDevice, 0x50) == expectedWord &&
                    Marshal.ReadInt32(targetDevice, 0x4c) == FloatBits(1.0f),
                    "native extended factory constructor defaults");
            }
            Marshal.WriteIntPtr(deviceArray, 0, targetDevice);
            Marshal.WriteIntPtr(owner, 0x5bc, deviceArray);
            Marshal.WriteInt32(owner, 0x5c0, 1);

            Marshal.WriteInt32(targetDevice, 0x04, unchecked((int)0x22222222));
            Marshal.WriteInt32(targetDevice, 0x08, unchecked((int)0x12345678)); // component ID
            Marshal.WriteInt32(targetDevice, 0x4c, unchecked((int)0x40200000)); // multiplier 2.5
            for (int i = 0; i < 16; i++) {
                Marshal.WriteInt32(targetDevice, 0x54 + i * 4, unchecked((int)(0xb2000000u + (uint)i)));
                Marshal.WriteInt32(typedRecord, 0x2c + i * 4, unchecked((int)(tag == 2 && i < 2 ? (uint)FloatBits(i == 0 ? 3.5f : 1.25f) : 0xc3000000u + (uint)i)));
            }
            Marshal.WriteInt32(typedRecord, 0x00, 19);
            Marshal.WriteInt32(typedRecord, 0x04, unchecked((int)0x77777777)); // differs from the existing device key
            Marshal.WriteInt32(typedRecord, 0x18, -1); // preserve the current class field
            Marshal.WriteInt32(typedRecord, 0xac, 0); // no map rows or auxiliary string tail
            if (tag == 9) {
                // This synthetic receiver has no FX host and the descriptor has no map rows.
                Marshal.WriteIntPtr(targetDevice, 0x3c, IntPtr.Zero);
            }
            if (variableTail) {
                Marshal.WriteInt32(typedRecord, 0xac, 2);
                Marshal.WriteInt32(typedRecord, 0xb0, 7);
                Marshal.WriteInt32(typedRecord, 0xb4, 9);
                Marshal.WriteInt32(typedRecord, 0xb8, 2);
                Marshal.WriteByte(typedRecord, 0xbc, (byte)'f');
                Marshal.WriteByte(typedRecord, 0xbd, (byte)'x');
                Marshal.WriteByte(typedRecord, 0xbe, 0);
            }
            int priorWord50 = Marshal.ReadInt32(targetDevice, 0x50);

            IntPtr result = applyOverride(owner, 0, typedRecord);
            Expect(result == IntPtr.Add(typedRecord, variableTail ? 0xbf : 0xb4),
                "Control indexed override completes one tag-" + tag + " typed record");
            Expect(Marshal.ReadInt32(owner, 0x5c0) == 1 && Marshal.ReadIntPtr(owner, 0x5bc) == deviceArray &&
                Marshal.ReadIntPtr(deviceArray, 0) == targetDevice,
                "Control INTO override keeps the existing tag-" + tag + " device slot and count");
            Expect(Marshal.ReadIntPtr(targetDevice, 0) == expectedVtable &&
                unchecked((uint)Marshal.ReadInt32(targetDevice, 0x04)) == 0x22222222 &&
                unchecked((uint)Marshal.ReadInt32(targetDevice, 0x08)) == 0x12345678 &&
                unchecked((uint)Marshal.ReadInt32(targetDevice, 0x4c)) == 0x40200000 &&
                Marshal.ReadInt32(targetDevice, 0x50) == priorWord50 &&
                Marshal.ReadIntPtr(targetDevice, 0x48) == typedRecord,
                "selected tag-" + tag + " device retains class, existing key, component ID, multiplier, word +0x50, and records the replacement descriptor");
            if (tag == 9)
                Expect(Marshal.ReadIntPtr(targetDevice, 0x3c) == IntPtr.Zero,
                    "tag-9 string callback runs with no FX host on the synthetic receiver");
            for (int i = 0; i < 16; i++) {
                Expect(unchecked((uint)Marshal.ReadInt32(targetDevice, 0x54 + i * 4)) == (tag == 2 && i < 2 ? (uint)FloatBits(i == 0 ? 3.5f : 1.25f) : 0xc3000000u + (uint)i),
                    "Control replaces selected tag-" + tag + " device coefficient " + i);
            }
            if (variableTail) {
                IntPtr map = Marshal.ReadIntPtr(targetDevice, 0x1c);
                Expect(Marshal.ReadInt32(targetDevice, 0x20) == 2 && Marshal.ReadInt32(map) == 7 &&
                    Marshal.ReadInt32(map, 0x14) == 9, "native common reader retains both variable map rows");
            }
            if (tag == 2) {
                IntPtr rowState = Marshal.ReadIntPtr(targetDevice, 0x12c);
                Expect(Marshal.ReadInt32(targetDevice, 0x120) == FloatBits(1.25f) &&
                    Marshal.ReadInt32(targetDevice, 0x124) == 4 &&
                    Marshal.ReadInt32(rowState) == 0 && Marshal.ReadInt32(rowState, 4) == 0,
                    "tag-2 wrapper caches selector 1, rounds selector 0 with x87, and zeros map state");
                Marshal.WriteInt32(rowState, 1);
                Marshal.WriteInt32(rowState, 4, 2);
                Expect(applyOverride(owner, 0, typedRecord) == result &&
                    Marshal.ReadInt32(targetDevice, 0x20) == 4,
                    "tag-2 repeated override retains common cursor and appends map rows");
                rowState = Marshal.ReadIntPtr(targetDevice, 0x12c);
                for (int i = 0; i < 4; i++)
                    Expect(Marshal.ReadInt32(rowState, i * 4) == 0,
                        "tag-2 repeated override clears state for every accumulated map row");
                Console.WriteLine("native-control-tag2-wrapper: selector1=1.25 selector0=3.5 integer=4 rows=4 state=zero");
            }
            Console.WriteLine("native-control-tag" + tag + "-into-override: size=0x" + objectSize.ToString("x") +
                " index=0 count=1 key=22222222 component=12345678 multiplier=" +
                Float(unchecked((uint)Marshal.ReadInt32(targetDevice, 0x4c))) +
                " original-class=preserved descriptor=attached coefficients=replaced cursor=" +
                (variableTail ? "0xbf map=[7,9] string=fx" : "0xb4"));
            GC.KeepAlive(construct);
        }
    }

    private static void PopulateUnitDatNode(ArealMapFixture fixture, IntPtr destination, ComponentNode node) {
        Zero(destination, 0x78);
        Marshal.WriteInt32(destination, 0x04, node.Children.Length);
        Marshal.WriteInt32(destination, 0x4c, unchecked((int)node.Kind));
        Marshal.WriteInt32(destination, 0x50, unchecked((int)node.LocalIndex));
        WriteFixedAnsi(destination, 0x0c, node.ArchiveName, 0x20);
        WriteFixedAnsi(destination, 0x2c, node.ResourceName, 0x20);
        if (node.Children.Length == 0) return;
        IntPtr children = fixture.Allocate(node.Children.Length * 0x78);
        Marshal.WriteIntPtr(destination, 0x08, children);
        for (int i = 0; i < node.Children.Length; i++)
            PopulateUnitDatNode(fixture, IntPtr.Add(children, i * 0x78), node.Children[i]);
    }

    private static void WriteFixedAnsi(IntPtr destination, int offset, string value, int capacity) {
        byte[] bytes = Encoding.ASCII.GetBytes(value + "\0");
        if (bytes.Length > capacity) throw new InvalidOperationException("synthetic UnitDAT string exceeds native field");
        Marshal.Copy(bytes, 0, IntPtr.Add(destination, offset), bytes.Length);
    }

    private static void ExpectControlResourceDeviceResult(
        NativeControlResourceDeviceGet getter,
        IntPtr device,
        IntPtr providerRows,
        int providerRow,
        uint providerBits,
        uint baseBits,
        uint multiplierBits,
        uint expectedBits,
        string label) {
        Marshal.WriteInt32(providerRows, providerRow + 0x14, unchecked((int)providerBits));
        Marshal.WriteInt32(device, 0x54, unchecked((int)baseBits));
        Marshal.WriteInt32(device, 0x4c, unchecked((int)multiplierBits));
        uint actualBits = unchecked((uint)FloatBits(getter(device, 0x300)));
        Expect(actualBits == expectedBits,
            "Control selector 0x300 " + label + " expected " + Hex(expectedBits) + " but got " + Hex(actualBits));
        Console.WriteLine("native-control-tag26-selector-300-" + label + ": result=" + Hex(actualBits));
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

    private static void CaptureBehaviorProfileLoad(IntPtr owner, uint selector, IntPtr archive, IntPtr entry) {
        behaviorProfileSelections.Add(new BehaviorProfileSelection {
            Owner = owner,
            Selector = selector,
            Archive = Marshal.PtrToStringAnsi(archive),
            Entry = Marshal.PtrToStringAnsi(entry)
        });
    }

    private static void RunNativeBehaviorProfileMapCases(IntPtr module) {
        NativeBehaviorProfileMap map = (NativeBehaviorProfileMap)Marshal.GetDelegateForFunctionPointer(
            IntPtr.Add(module, BehaviorProfileMapRva), typeof(NativeBehaviorProfileMap));
        string[][] cases = new string[][] {
            new string[] { "80000004", "prof_mine.var" },
            new string[] { "80000008", "prof_storage.var" },
            new string[] { "80000002", "prof_generator.var" },
            new string[] { "80000010", "prof_plant.var" },
            new string[] { "80000040", "prof_angar.var" },
            new string[] { "80000200", "prof_mtp.var" },
            new string[] { "80000400", "prof_institute.var" },
            new string[] { "80010000", "prof_bunker.var" },
            new string[] { "80020000", "prof_bunker.var" },
            new string[] { "80040000", "prof_bunker.var" },
            new string[] { "80100000", "prof_tower.var" },
            new string[] { "80200000", "prof_tower.var" },
            new string[] { "01002000", "prof_trn.var" },
            new string[] { "01004000", "prof_bld.var" },
            new string[] { "01008000", "prof_war.var" },
            new string[] { "01010000", "prof_hq.var" },
            new string[] { "01020000", "prof_hero.var" },
            new string[] { "20000000", "prof_animal.var" }
        };
        using (Fixture fixture = NewFixture()) {
            IntPtr owner = fixture.Allocate(0x0b00);
            IntPtr vtable = fixture.Allocate(0x68);
            Marshal.WriteIntPtr(owner, vtable);
            Marshal.WriteIntPtr(vtable, 0x64, Marshal.GetFunctionPointerForDelegate(BehaviorProfileLoadCallback));
            foreach (string[] test in cases) {
                behaviorProfileSelections.Clear();
                Marshal.WriteInt32(owner, 0x0afc, unchecked((int)Convert.ToUInt32(test[0], 16)));
                map(owner);
                Expect(behaviorProfileSelections.Count == 1, "profile mapper callback count for class 0x" + test[0]);
                BehaviorProfileSelection selection = behaviorProfileSelections[0];
                Expect(selection.Owner == owner && selection.Selector == 2 && selection.Archive == "behpsp.res" &&
                    selection.Entry == test[1], "profile mapper result for class 0x" + test[0]);
            }
            string[] noMatch = new string[] { "deadbeef", "8000000c" };
            foreach (string test in noMatch) {
                behaviorProfileSelections.Clear();
                Marshal.WriteInt32(owner, 0x0afc, unchecked((int)Convert.ToUInt32(test, 16)));
                map(owner);
                Expect(behaviorProfileSelections.Count == 0, "profile mapper leaves unknown/combined class 0x" + test + " unconfigured");
            }
        }
        Console.WriteLine("native-behavior-profile-map: mapped-classes=" + cases.Length + " (including three bunker and two tower codes); unknown-and-combined=no callback");
    }

    private static void RunNativeBehaviorVarSyntheticCase(IntPtr module) {
        NativeBehaviorVarArrayLoad load = (NativeBehaviorVarArrayLoad)Marshal.GetDelegateForFunctionPointer(
            IntPtr.Add(module, BehaviorVarArrayLoadRva), typeof(NativeBehaviorVarArrayLoad));
        NativeBehaviorVarFieldBind bind = (NativeBehaviorVarFieldBind)Marshal.GetDelegateForFunctionPointer(
            IntPtr.Add(module, BehaviorVarFieldBindRva), typeof(NativeBehaviorVarFieldBind));
        NativeBehaviorVarApply apply = (NativeBehaviorVarApply)Marshal.GetDelegateForFunctionPointer(
            IntPtr.Add(module, BehaviorVarApplyRva), typeof(NativeBehaviorVarApply));
        byte[] bytes = SerializeOneBehaviorVarField("Transfer_Ore_OffBoard", "float", 0x3f000000, 0x41200000);
        using (Fixture fixture = NewFixture()) {
            IntPtr table = fixture.Allocate(0x18);
            IntPtr target = fixture.Allocate(sizeof(uint));
            IntPtr input = fixture.Allocate(bytes.Length);
            IntPtr cursorOut = fixture.Allocate(IntPtr.Size);
            IntPtr fieldName = fixture.Allocate(32);
            IntPtr typeName = fixture.Allocate(8);
            CopyBytes(input, bytes);
            WriteFixedAnsi(fieldName, 0, "Transfer_Ore_OffBoard", 32);
            WriteFixedAnsi(typeName, 0, "float", 8);
            Marshal.WriteIntPtr(table, IntPtr.Add(module, BehaviorVarTableVtableRva));
            Marshal.WriteInt32(input, 0, 1);

            Expect(load(table, input, cursorOut) == 1, "native .var parser accepts the synthetic one-row profile");
            Expect(Marshal.ReadInt32(table, 4) == 1 && Marshal.ReadIntPtr(cursorOut) == IntPtr.Add(input, bytes.Length),
                "native .var parser consumes the full synthetic profile");
            bind(table, typeName, fieldName, target);
            apply(table);
            Expect(unchecked((uint)Marshal.ReadInt32(target)) == 0x41200000,
                "native .var apply uses the second value blob after binding");
            Console.WriteLine("native-behavior-var-synthetic: first=0x3f000000 second=0x41200000 applied=0x" +
                unchecked((uint)Marshal.ReadInt32(target)).ToString("x8"));
        }
    }

    private static void RunNativeBehaviorRealProfileCases(IntPtr module) {
        string archivePath = Environment.GetEnvironmentVariable("FPARKAN_ORIGINAL_BEHAVIOR_PROFILE_RES");
        if (String.IsNullOrEmpty(archivePath)) archivePath = @"C:\GOG Games\Parkan - Iron Strategy\behpsp.res";
        if (!File.Exists(archivePath)) throw new FileNotFoundException(
            "Set FPARKAN_ORIGINAL_BEHAVIOR_PROFILE_RES to the installed behpsp.res.", archivePath);
        byte[] archiveBytes = File.ReadAllBytes(archivePath);
        NativeBehaviorProfileMap map = (NativeBehaviorProfileMap)Marshal.GetDelegateForFunctionPointer(
            IntPtr.Add(module, BehaviorProfileMapRva), typeof(NativeBehaviorProfileMap));
        NativeBehaviorVarArrayLoad load = (NativeBehaviorVarArrayLoad)Marshal.GetDelegateForFunctionPointer(
            IntPtr.Add(module, BehaviorVarArrayLoadRva), typeof(NativeBehaviorVarArrayLoad));
        NativeBehaviorVarBlockConstructor construct = (NativeBehaviorVarBlockConstructor)Marshal.GetDelegateForFunctionPointer(
            IntPtr.Add(module, BehaviorVarBlockConstructorRva), typeof(NativeBehaviorVarBlockConstructor));
        NativeBehaviorVarFieldRegister register = (NativeBehaviorVarFieldRegister)Marshal.GetDelegateForFunctionPointer(
            IntPtr.Add(module, BehaviorVarFieldRegisterRva), typeof(NativeBehaviorVarFieldRegister));
        NativeBehaviorVarApply apply = (NativeBehaviorVarApply)Marshal.GetDelegateForFunctionPointer(
            IntPtr.Add(module, BehaviorVarApplyRva), typeof(NativeBehaviorVarApply));

        string selectedEntry = SelectBehaviorProfile(map, 0x80000004);
        Expect(selectedEntry == "prof_mine.var", "mine profile is selected by native class mapper");
        byte[] profile = ReadNresEntry(archiveBytes, selectedEntry);
        Expect(ReadUInt32(profile, 0) == 27, "native mine profile contains all 27 rows");
        uint expectedOreOffBoard = FindBehaviorVarSecondBlob(profile, "Transfer_Ore_OffBoard");
        uint expectedPowerOut = FindBehaviorVarSecondBlob(profile, "Transfer_Power_Out");

        using (Fixture fixture = NewFixture()) {
            IntPtr table = fixture.Allocate(0x18);
            IntPtr rawProfile = fixture.Allocate(profile.Length);
            IntPtr cursorOut = fixture.Allocate(IntPtr.Size);
            IntPtr block = fixture.Allocate(0x80);
            CopyBytes(rawProfile, profile);
            Marshal.WriteIntPtr(table, IntPtr.Add(module, BehaviorVarTableVtableRva));

            Expect(load(table, rawProfile, cursorOut) == 1, "native parser accepts the licensed mine profile");
            Expect(Marshal.ReadInt32(table, 4) == 27 && Marshal.ReadIntPtr(cursorOut) == IntPtr.Add(rawProfile, profile.Length),
                "native parser consumes all rows in the licensed mine profile");
            construct(block);
            Expect(unchecked((uint)Marshal.ReadInt32(block, 0x58)) == 0x40a00000 &&
                unchecked((uint)Marshal.ReadInt32(block, 0x60)) == 0x42c80000,
                "native variable block starts with transfer defaults 5 and 100");
            register(block, table);
            apply(table);
            Expect(unchecked((uint)Marshal.ReadInt32(block, 0x58)) == expectedOreOffBoard &&
                unchecked((uint)Marshal.ReadInt32(block, 0x60)) == expectedPowerOut,
                "native profile bind/apply copies second blobs into Transfer_Ore_OffBoard and Transfer_Power_Out");
            Console.WriteLine("native-behavior-real-profile: class=80000004 entry=" + selectedEntry + " rows=27 Transfer_Ore_OffBoard=" +
                Hex(expectedOreOffBoard) + " Transfer_Power_Out=" + Hex(expectedPowerOut) + "; native-bind-and-apply=verified");
        }
    }

    private static string SelectBehaviorProfile(NativeBehaviorProfileMap map, uint classCode) {
        using (Fixture fixture = NewFixture()) {
            IntPtr owner = fixture.Allocate(0x0b00);
            IntPtr vtable = fixture.Allocate(0x68);
            Marshal.WriteIntPtr(owner, vtable);
            Marshal.WriteIntPtr(vtable, 0x64, Marshal.GetFunctionPointerForDelegate(BehaviorProfileLoadCallback));
            behaviorProfileSelections.Clear();
            Marshal.WriteInt32(owner, 0x0afc, unchecked((int)classCode));
            map(owner);
            Expect(behaviorProfileSelections.Count == 1, "native mapper returned a single profile for class 0x" + classCode.ToString("x8"));
            BehaviorProfileSelection selection = behaviorProfileSelections[0];
            Expect(selection.Owner == owner && selection.Selector == 2 && selection.Archive == "behpsp.res",
                "native mapper selected the behavior profile archive and selector");
            return selection.Entry;
        }
    }

    private static byte[] SerializeOneBehaviorVarField(string fieldName, string typeName, uint firstBits, uint secondBits) {
        byte[] field = Encoding.ASCII.GetBytes(fieldName);
        byte[] type = Encoding.ASCII.GetBytes(typeName);
        using (MemoryStream stream = new MemoryStream()) using (BinaryWriter writer = new BinaryWriter(stream)) {
            writer.Write((uint)1); // row count
            writer.Write((uint)4); // value size
            writer.Write((uint)3); // float type index
            writer.Write((uint)0); // flags
            writer.Write((uint)field.Length); writer.Write(field);
            writer.Write((uint)type.Length); writer.Write(type);
            writer.Write(firstBits); writer.Write(secondBits);
            writer.Write((uint)0); writer.Write((uint)0); // trailing row metadata
            return stream.ToArray();
        }
    }

    private static byte[] ReadNresEntry(byte[] archive, string name) {
        if (archive.Length < 16) throw new InvalidDataException("NRes archive is shorter than its header");
        uint count = ReadUInt32(archive, 8);
        if (count > Int32.MaxValue / 64) throw new InvalidDataException("NRes directory count is too large");
        int directory = checked(archive.Length - (int)count * 64);
        if (directory < 16) throw new InvalidDataException("NRes directory falls inside the header");
        for (int index = 0; index < (int)count; index++) {
            int record = checked(directory + index * 64);
            int nameEnd = record + 20;
            while (nameEnd < record + 56 && archive[nameEnd] != 0) nameEnd++;
            string entryName = Encoding.ASCII.GetString(archive, record + 20, nameEnd - (record + 20));
            if (!String.Equals(entryName, name, StringComparison.Ordinal)) continue;
            uint size = ReadUInt32(archive, record + 12);
            uint dataOffset = ReadUInt32(archive, record + 56);
            if (size > Int32.MaxValue || dataOffset > Int32.MaxValue || dataOffset + size > archive.Length)
                throw new InvalidDataException("NRes entry range is outside the archive");
            byte[] data = new byte[(int)size];
            Buffer.BlockCopy(archive, (int)dataOffset, data, 0, data.Length);
            return data;
        }
        throw new InvalidDataException("NRes archive does not contain entry " + name);
    }

    private static uint FindBehaviorVarSecondBlob(byte[] profile, string fieldName) {
        if (profile.Length < 4) throw new InvalidDataException("Behavior profile is shorter than its row count");
        uint count = ReadUInt32(profile, 0);
        if (count > 1024) throw new InvalidDataException("Behavior profile row count is unreasonable");
        int cursor = 4;
        for (int index = 0; index < (int)count; index++) {
            EnsureRange(profile, cursor, 12);
            uint size = ReadUInt32(profile, cursor);
            if (size > Int32.MaxValue) throw new InvalidDataException("Behavior variable size is too large");
            uint type = ReadUInt32(profile, cursor + 4);
            cursor += 12;
            uint fieldLength = ReadUInt32(profile, cursor);
            if (fieldLength > Int32.MaxValue) throw new InvalidDataException("Behavior field name is too large");
            cursor += 4; EnsureRange(profile, cursor, (int)fieldLength);
            string name = Encoding.ASCII.GetString(profile, cursor, (int)fieldLength);
            cursor += (int)fieldLength;
            uint typeLength = ReadUInt32(profile, cursor);
            if (typeLength > Int32.MaxValue) throw new InvalidDataException("Behavior variable type name is too large");
            cursor += 4; EnsureRange(profile, cursor, (int)typeLength);
            cursor += (int)typeLength;
            EnsureRange(profile, cursor, checked((int)size * 2 + 8));
            int secondBlob = checked(cursor + (int)size);
            if (String.Equals(name, fieldName, StringComparison.Ordinal)) {
                if (type != 3 || size != 4) throw new InvalidDataException("Expected float profile field " + fieldName);
                return ReadUInt32(profile, secondBlob);
            }
            cursor = checked(cursor + (int)size * 2 + 8);
        }
        throw new InvalidDataException("Behavior profile does not contain field " + fieldName);
    }

    private static uint ReadUInt32(byte[] data, int offset) {
        EnsureRange(data, offset, 4);
        return unchecked((uint)(data[offset] | (data[offset + 1] << 8) | (data[offset + 2] << 16) | (data[offset + 3] << 24)));
    }

    private static void EnsureRange(byte[] data, int offset, int size) {
        if (offset < 0 || size < 0 || offset > data.Length - size) throw new InvalidDataException("Behavior profile range is outside its input");
    }

    private static void CopyBytes(IntPtr destination, byte[] source) { Marshal.Copy(source, 0, destination, source.Length); }

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

    private static void RunOreAllocationBranch(
        TickDelegate tick, string label, float rate, float stock, float recipientCurrent, float recipientMaximum,
        float expectedProducerRemaining, float expectedRecipientResult) {
        using (Fixture fixture = NewFixture()) {
            NativeObject producer = fixture.AddObject(0x46, 0x80000004, 0.0f, rate, stock, 0);
            producer.PropertyMetadata = FloatBits(10.0f);
            NativeObject recipient = fixture.AddObject(0x47, 0x80000001, 0.0f, 0.0f, recipientCurrent, 0);
            recipient.PropertyMetadata = FloatBits(recipientMaximum);
            recipient.InterfaceSelector1 = 0.5f;
            recipient.InterfaceSelector2 = 0.0f;
            fixture.MakeDistributor(producer.Key, recipient.Key);
            Marshal.WriteInt32(fixture.Entries, 0x04, FloatBits(1.0f));
            Marshal.WriteInt32(fixture.Entries, 0x1c + 0x04, FloatBits(0.5f));
            active = fixture;
            tick(fixture.Distributor, 1.0f);
            Console.WriteLine(label + ": totals=[" + Float(ReadFloat(fixture.Distributor, 0x30)) + "," +
                Float(ReadFloat(fixture.Distributor, 0x34)) + "," + Float(ReadFloat(fixture.Distributor, 0x38)) + "," +
                Float(ReadFloat(fixture.Distributor, 0x3c)) + "," + Float(ReadFloat(fixture.Distributor, 0x40)) +
                "]; stored=[" + Float(producer.Property) + "," + Float(recipient.Property) + "] reads=" + producer.OreRateReads);
            Expect(ReadFloat(fixture.Distributor, 0x34) == Math.Min(rate, stock) &&
                ReadFloat(fixture.Distributor, 0x40) == recipientMaximum - recipientCurrent &&
                producer.Property == expectedProducerRemaining && recipient.Property == expectedRecipientResult &&
                producer.OreRateReads == 2 && producer.Writes == 1 && recipient.Writes == 1 && fixture.LookupCount == 4,
                label + " native allocation branch totals and two-pass reread");
        }
    }

    private static void RunNativeOreClampCases(TickDelegate tick) {
        float[][] states = {
            new float[] { 0.995f, 1.0f, 1.0f },
            new float[] { 1.25f, 1.0f, 1.0f },
            new float[] { 0.0f, 0.01f, 0.0f }
        };
        foreach (float[] state in states) {
            float current = state[0];
            float maximum = state[1];
            float expected = state[2];
            using (Fixture fixture = NewFixture()) {
                NativeObject item = fixture.AddObject(0x48, 0x80000001, 0.0f, 0.0f, current, 0);
                item.PropertyMetadata = FloatBits(maximum);
                item.InterfaceSelector2 = 0.0f;
                fixture.MakeDistributor(item.Key);
                active = fixture;
                tick(fixture.Distributor, 1.0f);
                Expect(item.Property == expected && item.Writes > 0 && fixture.LookupCount == 2,
                    "native first-pass ore clamp for CurrentOre=" + Float(current));
                Console.WriteLine("native-ore-clamp: current=" + Float(current) + " maximum=" + Float(maximum) + " result=" +
                    Float(item.Property) + " writes=" + item.Writes);
            }
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
    private static IntPtr WriteStdCallToFastcallThunk(int offset, IntPtr fastcallTarget) {
        // Adapt stdcall(owner, node, parent ID) to the native __fastcall entry.
        byte[] code = { 0x8b, 0x4c, 0x24, 0x04, 0x8b, 0x54, 0x24, 0x08, 0xff, 0x74, 0x24, 0x0c,
            0xb8, 0, 0, 0, 0, 0xff, 0xd0, 0xc2, 0x0c, 0x00 };
        WriteImm32(code, 13, fastcallTarget.ToInt32());
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
    private static int NativeBoolean(int eax) { return eax & 0xff; }
    private static void Expect(bool value, string message) { if (!value) throw new InvalidOperationException("original native oracle failed: " + message); }
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
