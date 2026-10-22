#![forbid(unsafe_code)]
//! Runtime orchestration for mission loading and world stepping.

use fparkan_assets::{
    decode_mission_land_path, decode_mission_payload, decode_nres_payload,
    derive_mission_land_paths, extend_graph_report_with_visual_dependencies,
    mission_script_bundle_bases, prepare_terrain_world, AssetError as AssetPreparationError,
    AssetId, AssetManager, BuildCategory, MissionAssetPlan, MissionDocument, MissionError,
    MissionTerrainPaths, NresError, PreparedComponentControl, PreparedControlEffectSection,
    PreparedControlTypedRecord, PreparedVisual, TerrainFormatError, TerrainPreparationError,
    TerrainWorld, TmaProfile,
};
use fparkan_path::{normalize_relative, NormalizedPath, PathError, PathPolicy};
use fparkan_prototype::{
    build_prototype_graph_report, unit_component_tree, PrototypeGraph, PrototypeGraphFailure,
    PrototypeGraphReport, UnitComponentRecord, UnitComponentTreeNode, PROTOTYPE_TYPE_EXTO,
    UNIT_DAT_BAS_PLACEMENT_FLAG,
};
use fparkan_resource::{resource_name, CachedResourceRepository, ResourceKey, ResourceRepository};
use fparkan_script::{
    Handler19DwordWrite, Handler19InitInput, Handler19InitResolution, Handler19ResolveError,
    ScriptInstruction, ScriptPackage, VarSet, VarSetDefault, VmHostCallbackCommand,
};
use fparkan_terrain::SurfaceQuery;
use fparkan_vfs::{Vfs, VfsError};
use fparkan_world::{
    construct_object, handle_by_original_id, new as new_world, register_object, set_transform,
    step, transform_state, InputSnapshot, ObjectDraft, ObjectHandle, OriginalObjectId,
    TransformState, World, WorldConfig, WorldSnapshot,
};
use std::num::NonZeroUsize;
use std::sync::Arc;

pub use fparkan_assets::MissionAssets;

const MISSION_DECODED_PAYLOAD_CACHE_ENTRIES: usize = 256;
const FALLBACK_SCRIPT_VARSET_PATH: &str = "MISSIONS/SCRIPTS/varset.var";
const INITIAL_BEHAVIOR_RNG_STATE: u32 = 1;

/// Injectable engine services used by composition roots.
#[derive(Clone, Default)]
pub struct EngineServices {
    /// Resource filesystem.
    pub vfs: Option<Arc<dyn Vfs>>,
}

impl EngineServices {
    /// Creates services with a VFS.
    #[must_use]
    pub fn new(vfs: Arc<dyn Vfs>) -> Self {
        Self { vfs: Some(vfs) }
    }
}

/// Native mission mode used by the recovered host callback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MissionMode {
    /// Native application mode 1, used by the selected `missions/single.*` path.
    SingleMission,
    /// Native application mode 3, selected by the main-menu idle timer.
    AutoDemo,
}

/// Mission request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MissionRequest {
    /// Mission key/path.
    pub key: String,
    /// Explicit host mode; this is not inferred from the mission path.
    pub mode: MissionMode,
}

const INITIAL_SCRIPT_HOST_STATUS: u32 = 5;

/// Ordered mission property preserved for later runtime stages.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MissionObjectProperty {
    /// Raw property value words.
    pub raw_value: [u32; 4],
    /// Raw property name bytes.
    pub name_raw: Vec<u8>,
}

/// Mission-derived object draft preserved for Stage 3 viewer/player work.
#[derive(Clone, Debug, PartialEq)]
pub struct MissionObjectDraft {
    /// Original mission object id.
    pub original_id: Option<OriginalObjectId>,
    /// Raw mission resource reference.
    pub resource_name_raw: Vec<u8>,
    /// Raw word immediately after the mission resource reference. The native
    /// runtime uses it as an AI-selection index.
    pub raw_after_resource: u32,
    /// Raw identity/clan word from the mission.
    pub identity_or_clan_raw: u32,
    /// Raw word after the mission object's instance name. Native BAS
    /// placement uses its zero/nonzero state only when the Unit DAT header
    /// gate is set.
    pub raw_after_name: u32,
    /// Second word of the root Unit DAT header, when it was decoded. A direct
    /// prototype root has no Unit DAT header.
    pub unit_dat_header_flags: Option<u32>,
    /// Raw position vector.
    pub position: [f32; 3],
    /// Raw orientation vector.
    pub orientation_raw: [f32; 3],
    /// Raw scale vector.
    pub scale: [f32; 3],
    /// Prepared visuals reachable from this mission object.
    pub visual_ids: Vec<AssetId<PreparedVisual>>,
    /// Ordered mission properties.
    pub properties: Vec<MissionObjectProperty>,
    /// Ordered raw Unit DAT components that selected this mission root.
    ///
    /// The records preserve source provenance for future controller loading;
    /// their fields intentionally have no inferred Control/physics semantics.
    pub unit_components: Vec<UnitComponentRecord>,
}

/// A mission object registered with one clan's native Behavior distributor.
#[derive(Clone, Debug, PartialEq)]
pub struct BehaviorRegisteredObject {
    /// Stable identity from the source TMA object order.
    pub original_id: OriginalObjectId,
    /// Corresponding live Rust world object.
    pub handle: ObjectHandle,
    /// Raw DWORD that selects the `GetSuperAI` table slot.
    pub selector: u32,
    /// Class word recovered from the root Unit DAT header.
    pub class_flags: u32,
    /// Native share field at record offset `+0x04`.
    pub share: f32,
    /// Raw DWORDs at offsets `+0x08..+0x18`; the first controls whether
    /// normalization treats this record as fixed, and the remaining words are
    /// not yet assigned semantics. A normal native Add initializes all five to 0.
    pub native_tail: [u32; 5],
}

/// Native `CurrentOre`/`MaximumOre` state for one full Unit DAT mission object.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BehaviorOreState {
    /// Stable identity from the source TMA object order.
    pub original_id: OriginalObjectId,
    /// Corresponding live Rust world object.
    pub handle: ObjectHandle,
    /// Class word recovered from the root Unit DAT header.
    pub class_flags: u32,
    /// Raw bits of the `CurrentOre` float property.
    pub current_ore_bits: u32,
    /// Raw bits of the read-only `MaximumOre` float property.
    pub maximum_ore_bits: u32,
}

/// Origin of a Control descriptor retained in the effective table projection.
#[derive(Clone, Debug, PartialEq)]
pub struct ControlDeviceRecordProvenance {
    /// Unit DAT component index that supplied the descriptor.
    pub unit_component_index: usize,
    /// Zero-based record index in that component's CTLD typed-record list.
    pub typed_record_index: usize,
    /// CTLD resource that supplied the descriptor.
    pub source: ResourceKey,
}

/// Common fields known for one row in a host object's Control device table.
///
/// The row index is its position in the containing `Vec`. This stores the
/// confirmed base fields and latest raw descriptor, not tag-specific callbacks.
#[derive(Clone, Debug, PartialEq)]
pub struct EffectiveControlDevice {
    /// Factory tag of the original device instance; INTO does not replace it.
    pub factory_tag: u32,
    /// Native component ID that first appended this row.
    pub component_id: u32,
    /// Current common key field. INTO preserves the receiver's key.
    pub key: u32,
    /// Sixteen raw DWORD coefficients copied by the common setter.
    pub coefficient_bits: Option<[u32; 16]>,
    /// Multiplier bits when the factory default is confirmed.
    pub multiplier_bits: Option<u32>,
    /// Current native word at `+0x50` when it is known.
    pub native_word_50_bits: Option<u32>,
    /// Current raw descriptor, including typed values and string bytes.
    pub descriptor: PreparedControlTypedRecord,
    /// Source location that created the original factory instance.
    pub factory_provenance: ControlDeviceRecordProvenance,
    /// Source location of the descriptor currently applied to the row.
    pub descriptor_provenance: ControlDeviceRecordProvenance,
}

/// Why the base Control device table could not be projected for an object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MissionControlDeviceTableUnavailableReason {
    /// The mission root has no decoded Unit DAT component tree.
    NoUnitDatTree,
    /// Unit DAT child counts do not form a valid preorder tree.
    InvalidUnitComponentTree,
    /// Prototype, visual, and component data are not aligned.
    ComponentAlignmentMismatch,
    /// No component explicitly references a Control resource.
    NoControlResource,
    /// An explicit Control resource could not be prepared.
    ControlResourceUnavailable { unit_component_index: usize },
    /// A Control resource belongs to a non-geometric prototype.
    NonGeometricControlResource { unit_component_index: usize },
    /// The parser stopped before the component's typed record list completed.
    IncompleteTypedRecordList {
        unit_component_index: usize,
        first_unparsed_record: usize,
    },
    /// An INTO descriptor has no first typed record for its one native override call.
    MissingOverrideRecord { unit_component_index: usize },
    /// Native wrapping produced a non-sentinel index outside the current table.
    OverrideIndexOutOfRange {
        unit_component_index: usize,
        target_index: u32,
    },
    /// The receiver tag does not have a verified common override method.
    UnsupportedOverrideReceiver { factory_tag: u32 },
    /// The EXTO key needs a global model-node mapping that is not available.
    UnsupportedExToNodeMapping { unit_component_index: usize },
    /// An unknown component kind prevents calculating EXTO global node offsets.
    UnknownComponentKindForExTo {
        unit_component_index: usize,
        source_type: Option<u32>,
    },
}

/// Availability of the confirmed common-field Control table projection.
#[derive(Clone, Debug, PartialEq)]
pub enum MissionControlDeviceTableStatus {
    /// Rows in native global-table order.
    Available(Vec<EffectiveControlDevice>),
    /// No complete table is exposed when required source state is unsupported.
    Unavailable(MissionControlDeviceTableUnavailableReason),
}

/// Control device table projection attached to one loaded mission object.
#[derive(Clone, Debug, PartialEq)]
pub struct MissionControlDeviceTableState {
    /// Stable identity from the source TMA object order.
    pub original_id: Option<OriginalObjectId>,
    /// Corresponding live Rust world object.
    pub handle: ObjectHandle,
    /// Complete base projection, or an explicit unsupported boundary.
    pub status: MissionControlDeviceTableStatus,
}

/// Resource distributor state associated with one TMA clan's constructed AI.
#[derive(Clone, Debug, PartialEq)]
pub struct BehaviorDistributorState {
    /// TMA clan index; it is not an index into the filtered script bundle list.
    pub clan_index: usize,
    /// Whether a successfully loaded script package published this AI to
    /// `GetSuperAI` in the standard host path.
    pub super_ai_visible: bool,
    /// Objects admitted by the currently recovered standard-load registration path.
    pub registered_objects: Vec<BehaviorRegisteredObject>,
}

const LAST_SUPER_AI_TABLE_INDEX: u32 = 63;
const BEHAVIOR_REGISTRATION_KEY_FLAG: u32 = 0x8000_0000;
#[cfg(test)]
const PROTOTYPE_TYPE_INTO: u32 = u32::from_le_bytes(*b"INTO");

/// Native terrain adjustment selected for a mission object's BAS geometry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BasementPlacementMode {
    /// Flatten the BAS inner boundary to mean outer terrain height and adjust
    /// the object root transform by the native placement delta.
    FlattenInnerAndAdjustRoot,
    /// Keep authored BAS inner heights and leave the object root transform as
    /// supplied by the mission.
    PreserveAuthoredInnerHeight,
}

impl MissionObjectDraft {
    /// Returns the BAS mode selected by the native Unit DAT gate and mission
    /// word, independent of resource path naming.
    #[must_use]
    pub fn basement_placement_mode(&self) -> Option<BasementPlacementMode> {
        let header_flags = self.unit_dat_header_flags?;
        if header_flags & UNIT_DAT_BAS_PLACEMENT_FLAG == 0 {
            return None;
        }
        Some(if self.raw_after_name == 0 {
            BasementPlacementMode::FlattenInnerAndAdjustRoot
        } else {
            BasementPlacementMode::PreserveAuthoredInnerHeight
        })
    }

    /// Returns this mission root's position after applying the native BAS
    /// transform adjustment once. Native code subtracts
    /// `authored_inner_z - mean_outer_z` from matrix element 11. Component
    /// local transforms should continue to be composed beneath this adjusted
    /// root position without applying the delta again.
    #[must_use]
    pub fn root_position_with_basement_delta(&self, placement_delta: f32) -> [f32; 3] {
        let mut position = self.position;
        if self.basement_placement_mode() == Some(BasementPlacementMode::FlattenInnerAndAdjustRoot)
        {
            position[2] -= placement_delta;
        }
        position
    }
}

/// A compiled script bundle selected by one mission clan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MissionScriptBundle {
    /// TMA clan index selecting this bundle.
    pub clan_index: usize,
    /// Lossy display form of the raw bundle base path from the clan resource.
    pub base_path: String,
    /// Resolved compiled `.scr` path.
    pub script_path: String,
    /// Losslessly decoded compiled package; only proven `Handler(19)` Init
    /// writes are resolved and retained by runtime.
    pub package: ScriptPackage,
    /// Outcome of the restricted first-`Init` resolution for this bundle.
    pub init_execution: MissionScriptInitExecution,
    /// Recovered per-AI Behavior state computed by a full mission startup.
    /// Static previews leave this empty because they do not run script startup.
    pub behavior_warm_start: Option<BehaviorWarmStartState>,
}

/// Initial Behavior distributor fields recovered from the original warm-start path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BehaviorWarmStartState {
    /// Queue-clock value copied to distributor offset `+0x10` when it is zero.
    pub first_time_ms: u32,
    /// Initial scheduled time written to distributor offset `+0x14`.
    pub next_deadline_ms: u32,
    /// Opaque distributor words at offsets `+0x30..=+0x40`, cleared on first start.
    pub cleared_words: [u32; 5],
}

impl BehaviorWarmStartState {
    fn start(queue_time_ms: u32, rng_state: &mut u32) -> Self {
        const BASE_INTERVAL_MS: u32 = 3 * 64;
        const JITTER_RANGE_MS: u32 = 64;

        *rng_state = (*rng_state).wrapping_mul(0x343fd).wrapping_add(0x269ec3);
        let random = (*rng_state >> 16) & 0x7fff;
        let jitter_ms = ((random & 0xff) * JITTER_RANGE_MS) >> 8;

        Self {
            first_time_ms: queue_time_ms,
            next_deadline_ms: queue_time_ms.wrapping_add(BASE_INTERVAL_MS + jitter_ms),
            cleared_words: [0; 5],
        }
    }
}

/// Numeric script defaults selected for the mission's first script bundle.
///
/// GOG `ai.dll` first tries `<bundle-base>.var`, then falls back to the shared
/// `MISSIONS/SCRIPTS/varset.var`. This stores the successfully selected source
/// and supplies the typed targets for the proven `Handler(19)` Init writes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MissionScriptVarSet {
    /// Resolved `.var` path selected by the original fallback order.
    pub path: String,
    /// Whether the shared `MISSIONS/SCRIPTS/varset.var` fallback was used.
    pub used_fallback: bool,
    /// Typed numeric declaration defaults in source order.
    pub declarations: VarSet,
}

/// One resolved, corpus-proven `Init` result for a mission clan.
///
/// This is intentionally limited to `Handler(19)`: an `Init` event's other
/// selectors are retained in [`MissionScriptBundle::package`] but are not
/// guessed as executable behavior.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MissionScriptInitState {
    /// TMA clan index that owns the `SuperAI` instance and varset.
    pub clan_index: usize,
    /// Zero-based first case-sensitive C-string `Init` event selected by native lookup.
    pub event_index: usize,
    /// Raw event word preserved from the compiled `Init` event.
    pub event_word: u32,
    /// Exact three DWORD writes made by each proven `Handler(19)` instruction.
    pub writes: Vec<Handler19DwordWrite>,
}

/// Execution status of the first event whose case-sensitive C-string name is `Init`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MissionScriptInitExecution {
    /// Native name lookup found no event named `Init` and made no handler call.
    Missing,
    /// The selected event completed within the currently recovered action subset.
    Executed {
        /// Zero-based event table index selected by native lookup.
        event_index: usize,
    },
    /// A preview retained the decoded package but stopped before the first
    /// instruction outside the recovered `Handler(19)` action subset.
    Unsupported {
        /// Zero-based event table index selected by native lookup.
        event_index: usize,
        /// Zero-based instruction index where restricted resolution stopped.
        instruction_index: usize,
        /// Typed selector, control, or result-save shape that is not modeled.
        error: Handler19ResolveError,
    },
}

/// Per-clan mutable-value model of one instantiated mission varset.
///
/// Declarations remain shared source metadata; these values are the runtime
/// cells that later recovered handlers will read and write.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MissionScriptVarSetState {
    /// TMA clan index owning this instantiated varset.
    pub clan_index: usize,
    /// Values in the original varset declaration order.
    pub values: Vec<VarSetDefault>,
}

/// Rust-side outcome selected by the recovered `Iron3D` host callback.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MissionScriptOutcome {
    /// No host callback outcome has been recorded; this does not mean that the
    /// native `IGame` has entered its running state.
    #[default]
    Active,
    /// Rust outcome corresponding to completion payload `1`.
    Completed,
    /// Rust outcome corresponding to failure payload `0`.
    Failed,
}

/// Result of applying one recovered script host callback command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MissionScriptHostEffect {
    /// The input did not match a currently modeled command.
    Unhandled,
    /// The Rust outcome changed to completed.
    MissionCompleted,
    /// The Rust outcome changed to failed.
    MissionFailed,
    /// The native status guard rejected a repeated callback without changing state.
    NoChange,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct MissionLoadOptions {
    fail_after_registered_objects: Option<usize>,
    asset_scope: MissionAssetScope,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct LoadedMissionScripts {
    bundles: Vec<MissionScriptBundle>,
    varset: Option<MissionScriptVarSet>,
    init_states: Vec<MissionScriptInitState>,
    varset_states: Vec<MissionScriptVarSetState>,
}

/// Selects how much of the resolved mission asset graph to prepare.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum MissionAssetScope {
    /// Prepare every reachable asset; required by normal runtime loading.
    #[default]
    Full,
    /// Prepare only the first mission root for a static preview.
    FirstMeshPreview,
    /// Prepare a non-zero prefix of mission roots for a static preview.
    PreviewRoots(NonZeroUsize),
}

/// Loaded mission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedMission {
    /// Mission key.
    pub key: String,
    /// Decoded mission path count.
    pub path_count: usize,
    /// Decoded clan count.
    pub clan_count: usize,
    /// Decoded placed object count.
    pub object_count: usize,
    /// Decoded extra record count.
    pub extra_count: usize,
    /// `Land.msh` path.
    pub land_msh_path: String,
    /// `Land.map` path.
    pub land_map_path: String,
    /// Build category count.
    pub build_category_count: usize,
    /// Runtime navigation area count.
    pub areal_count: usize,
    /// Runtime surface triangle count.
    pub surface_count: usize,
    /// Registered world object count.
    pub registered_objects: usize,
    /// Mission resource roots that point to unit DAT files.
    pub graph_unit_reference_count: usize,
    /// Mission resource roots that point directly to prototype keys.
    pub graph_direct_reference_count: usize,
    /// Component records reached from unit DAT roots.
    pub graph_unit_component_count: usize,
    /// Mission prototype graph root count.
    pub graph_root_count: usize,
    /// Total materialized graph node count after visual dependency expansion.
    pub graph_node_count: usize,
    /// Total materialized graph edge count after visual dependency expansion.
    pub graph_edge_count: usize,
    /// Mission asset plan visual count after dependency preparation.
    pub asset_visual_count: usize,
    /// Expanded prototype requests resolved to effective prototypes.
    pub graph_resolved_count: usize,
    /// Reached mesh dependency count.
    pub graph_mesh_dependency_count: usize,
    /// Graph failure count.
    pub graph_failure_count: usize,
    /// WEAR requests derived from graph meshes.
    pub graph_wear_request_count: usize,
    /// WEAR entries decoded.
    pub graph_wear_resolved_count: usize,
    /// WEAR material slots requested.
    pub graph_material_slot_count: usize,
    /// MAT0 entries decoded.
    pub graph_material_resolved_count: usize,
    /// Texture requests derived from MAT0 phases.
    pub graph_texture_request_count: usize,
    /// Texm texture entries decoded.
    pub graph_texture_resolved_count: usize,
    /// Lightmap requests declared by WEAR tables.
    pub graph_lightmap_request_count: usize,
    /// Lightmap Texm entries decoded.
    pub graph_lightmap_resolved_count: usize,
    /// Mission asset plan mesh-backed count after dependency preparation.
    pub asset_model_count: usize,
    /// Mission asset plan material count after dependency preparation.
    pub asset_material_count: usize,
    /// Mission asset plan texture count after dependency preparation.
    pub asset_texture_count: usize,
    /// Mission asset plan lightmap count after dependency preparation.
    pub asset_lightmap_count: usize,
    /// Script bundles selected by mission clans.
    pub script_bundle_count: usize,
    /// Total named events in loaded script packages.
    pub script_event_count: usize,
    /// Numeric script defaults loaded through the original bundle/fallback rule.
    pub script_varset_declaration_count: usize,
    /// `Handler(19)` Init results resolved for mission clans.
    pub script_init_state_count: usize,
    /// Instantiated per-clan varsets whose Init writes have been applied.
    pub script_varset_state_count: usize,
}

/// Frame result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameResult {
    /// Snapshot.
    pub snapshot: WorldSnapshot,
}

/// Engine.
pub struct Engine {
    services: EngineServices,
    world: World,
    loaded: Option<LoadedMissionState>,
    behavior_rng_state: u32,
}

struct LoadedMissionState {
    summary: LoadedMission,
    mission: MissionDocument,
    terrain: TerrainWorld,
    build_categories: Vec<BuildCategory>,
    prototype_graph: PrototypeGraph,
    prototype_report: PrototypeGraphReport,
    mission_assets: MissionAssets,
    asset_plan: MissionAssetPlan,
    object_drafts: Vec<MissionObjectDraft>,
    behavior_distributors: Vec<BehaviorDistributorState>,
    behavior_ore_states: Vec<BehaviorOreState>,
    control_device_tables: Vec<MissionControlDeviceTableState>,
    script_bundles: Vec<MissionScriptBundle>,
    script_varset: Option<MissionScriptVarSet>,
    script_init_states: Vec<MissionScriptInitState>,
    script_varset_states: Vec<MissionScriptVarSetState>,
    script_host_mode: MissionMode,
    script_host_status: u32,
    script_outcome: MissionScriptOutcome,
}

/// Engine error.
#[derive(Debug)]
pub enum EngineError {
    /// Engine was created without a resource VFS.
    MissingVfs,
    /// Invalid resource path.
    Path {
        /// Path role.
        role: &'static str,
        /// Raw value.
        value: String,
        /// Source error.
        source: PathError,
    },
    /// VFS error.
    Vfs {
        /// Resource path.
        path: String,
        /// Source error.
        source: VfsError,
    },
    /// Compiled script package could not be decoded.
    Script {
        /// Script path.
        path: String,
        /// Decode diagnostic.
        message: String,
    },
    /// Script defaults could not be decoded.
    VarSet {
        /// Script-default path.
        path: String,
        /// Parse diagnostic.
        message: String,
    },
    /// A recovered `Handler(19)` Init input or target was not representable.
    ScriptInit {
        /// Owning TMA clan index.
        clan_index: usize,
        /// Proven-contract diagnostic.
        message: String,
    },
    /// A full mission load reached an unsupported instruction in its first Init event.
    ScriptInitUnsupported {
        /// Owning TMA clan index.
        clan_index: usize,
        /// Resolved `.scr` package path.
        path: String,
        /// Zero-based first exact Init event index.
        event_index: usize,
        /// Zero-based instruction index within that event.
        instruction_index: usize,
        /// Typed unsupported instruction shape.
        source: Handler19ResolveError,
    },
    /// A recovered script handler could not resolve against runtime state.
    ScriptRuntime {
        /// Owning TMA clan index.
        clan_index: usize,
        /// Handler-specific diagnostic.
        message: String,
    },
    /// `NRes` decode error.
    Nres {
        /// Resource path.
        path: String,
        /// Source error.
        source: NresError,
    },
    /// Mission decode error.
    Mission {
        /// Resource path.
        path: String,
        /// Source error.
        source: MissionError,
    },
    /// Terrain disk format error.
    TerrainFormat {
        /// Resource path.
        path: String,
        /// Source error.
        source: TerrainFormatError,
    },
    /// Terrain runtime build error.
    Terrain(fparkan_assets::TerrainError),
    /// Prototype graph errors.
    PrototypeGraph {
        /// Root failures.
        failures: Vec<PrototypeGraphFailure>,
    },
    /// Asset preparation errors.
    AssetPreparation {
        /// Mission key.
        mission: String,
        /// Source error.
        source: AssetPreparationError,
    },
    /// World error.
    World(fparkan_world::WorldError),
    /// Reference movement input or terrain query failed.
    Movement(String),
    /// Staged mission world was torn down after a registration-phase failure.
    RegistrationTeardown {
        /// Registered objects before the forced failure.
        registered_objects: usize,
        /// Objects released by normal world shutdown.
        released_objects: usize,
        /// Managers were released after objects.
        managers_released: bool,
    },
}

/// Advances one object toward an explicit XY target and snaps it to terrain.
///
/// This is a deterministic reference controller, not recovered original AI.
///
/// # Errors
///
/// Returns an error when the input is invalid, the mission/object is absent,
/// the source transform is non-finite, or the requested point has no terrain
/// surface.
pub fn advance_reference_movement(
    engine: &mut Engine,
    original_id: OriginalObjectId,
    target_xy: [f32; 2],
    max_step: f32,
) -> Result<bool, EngineError> {
    if !target_xy
        .iter()
        .chain(std::iter::once(&max_step))
        .all(|v| v.is_finite())
        || max_step <= 0.0
    {
        return Err(EngineError::Movement(
            "target and max_step must be finite; max_step must be positive".to_string(),
        ));
    }
    let terrain = engine
        .loaded
        .as_ref()
        .ok_or_else(|| EngineError::Movement("mission terrain is unavailable".to_string()))?
        .terrain
        .clone();
    let handle = handle_by_original_id(&engine.world, original_id).ok_or_else(|| {
        EngineError::Movement(format!("original object {} is unavailable", original_id.0))
    })?;
    let mut transform = transform_state(&engine.world, handle)?;
    let position = transform.position.map(f32::from_bits);
    if !position.iter().all(|v| v.is_finite()) {
        return Err(EngineError::Movement(
            "object transform is non-finite".to_string(),
        ));
    }
    let dx = target_xy[0] - position[0];
    let dy = target_xy[1] - position[1];
    let distance = dx.hypot(dy);
    let reached = distance <= max_step;
    let ratio = if reached { 1.0 } else { max_step / distance };
    let x = position[0] + dx * ratio;
    let y = position[1] + dy * ratio;
    let z = terrain
        .height_at([x, y])
        .map_err(|err| EngineError::Movement(err.to_string()))?
        .ok_or_else(|| {
            EngineError::Movement("movement target lies outside terrain surface".to_string())
        })?;
    transform.position = [x.to_bits(), y.to_bits(), z.to_bits()];
    set_transform(&mut engine.world, handle, transform)?;
    Ok(reached)
}

impl From<fparkan_world::WorldError> for EngineError {
    fn from(value: fparkan_world::WorldError) -> Self {
        Self::World(value)
    }
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingVfs => write!(f, "mission loading requires a VFS service"),
            Self::Path {
                role,
                value,
                source,
            } => {
                write!(f, "invalid {role} path '{value}': {source}")
            }
            Self::Vfs { path, source } => write!(f, "{path}: {source}"),
            Self::Script { path, message } => write!(f, "{path}: script decode failed: {message}"),
            Self::VarSet { path, message } => write!(f, "{path}: varset decode failed: {message}"),
            Self::ScriptInit {
                clan_index,
                message,
            } => write!(f, "clan {clan_index}: script Init failed: {message}"),
            Self::ScriptInitUnsupported {
                clan_index,
                path,
                event_index,
                instruction_index,
                source,
            } => write!(
                f,
                "{path}: clan {clan_index} Init event {event_index} instruction {instruction_index} is unsupported: {source}"
            ),
            Self::ScriptRuntime {
                clan_index,
                message,
            } => write!(f, "clan {clan_index}: script runtime failed: {message}"),
            Self::Nres { path, source } => write!(f, "{path}: {source}"),
            Self::Mission { path, source } => write!(f, "{path}: {source}"),
            Self::TerrainFormat { path, source } => write!(f, "{path}: {source}"),
            Self::Terrain(source) => write!(f, "{source}"),
            Self::PrototypeGraph { failures } => {
                write!(f, "mission prototype graph has {} failures", failures.len())
            }
            Self::AssetPreparation { mission, source } => {
                write!(f, "{mission}: asset preparation failed: {source}")
            }
            Self::World(source) => write!(f, "{source}"),
            Self::Movement(message) => write!(f, "reference movement: {message}"),
            Self::RegistrationTeardown {
                registered_objects,
                released_objects,
                managers_released,
            } => write!(
                f,
                "mission registration failed after {registered_objects} objects; teardown released {released_objects}, managers_released={managers_released}"
            ),
        }
    }
}

impl std::error::Error for EngineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Path { source, .. } => Some(source),
            Self::Vfs { source, .. } => Some(source),
            Self::Nres { source, .. } => Some(source),
            Self::Mission { source, .. } => Some(source),
            Self::TerrainFormat { source, .. } => Some(source),
            Self::ScriptInitUnsupported { source, .. } => Some(source),
            Self::Terrain(source) => Some(source),
            Self::World(source) => Some(source),
            Self::AssetPreparation { source, .. } => Some(source),
            Self::MissingVfs
            | Self::Script { .. }
            | Self::VarSet { .. }
            | Self::ScriptInit { .. }
            | Self::ScriptRuntime { .. }
            | Self::Movement(_)
            | Self::PrototypeGraph { .. }
            | Self::RegistrationTeardown { .. } => None,
        }
    }
}

/// Creates an engine with the supplied services.
pub fn create(services: EngineServices) -> Result<Engine, EngineError> {
    Ok(Engine {
        services,
        world: new_world(WorldConfig),
        loaded: None,
        behavior_rng_state: INITIAL_BEHAVIOR_RNG_STATE,
    })
}

/// Loads a mission transactionally and runs the recovered initial script subset.
///
/// `queue_time_ms` is the caller-supplied 32-bit queue-clock sample used to
/// initialize the first Behavior deadline for each script bundle. This runtime
/// entry point does not acquire the native queue clock or reproduce the full
/// SuperAI host lifecycle.
///
/// # Errors
///
/// Returns [`EngineError`] if VFS services are missing, mission paths are
/// invalid, required files cannot be read, disk formats fail validation, terrain
/// runtime data cannot be built, prototype graph roots do not resolve, object
/// registration fails, or a full mission's first `Init` contains an unsupported
/// action form.
pub fn load_mission(
    engine: &mut Engine,
    request: MissionRequest,
    queue_time_ms: u32,
) -> Result<LoadedMission, EngineError> {
    load_mission_with_options(
        engine,
        request,
        MissionLoadOptions::default(),
        Some(queue_time_ms),
    )
}

/// Loads a mission using the bounded static-preview asset scope.
///
/// This mode preserves map, TMA, graph, construction and registration work,
/// but visits only the first mission root. It is intended solely for the
/// opt-in static Vulkan preview and must not be used for normal gameplay,
/// where all reachable assets remain required.
///
/// # Errors
///
/// Returns [`EngineError`] under the same decoding, graph, terrain and world
/// conditions as [`load_mission`], plus asset errors for the roots examined by
/// the preview scope.
pub fn load_mission_static_preview(
    engine: &mut Engine,
    request: MissionRequest,
) -> Result<LoadedMission, EngineError> {
    load_mission_with_options(
        engine,
        request,
        MissionLoadOptions {
            asset_scope: MissionAssetScope::FirstMeshPreview,
            ..MissionLoadOptions::default()
        },
        None,
    )
}

/// Loads a static preview for the first requested non-zero count of mission roots.
///
/// Normal gameplay still uses [`load_mission`] and always resolves every root.
///
/// # Errors
///
/// Returns [`EngineError`] under the same conditions as
/// [`load_mission_static_preview`].
pub fn load_mission_static_preview_roots(
    engine: &mut Engine,
    request: MissionRequest,
    root_count: NonZeroUsize,
) -> Result<LoadedMission, EngineError> {
    load_mission_with_options(
        engine,
        request,
        MissionLoadOptions {
            asset_scope: MissionAssetScope::PreviewRoots(root_count),
            ..MissionLoadOptions::default()
        },
        None,
    )
}

fn load_mission_with_options(
    engine: &mut Engine,
    request: MissionRequest,
    options: MissionLoadOptions,
    queue_time_ms: Option<u32>,
) -> Result<LoadedMission, EngineError> {
    let vfs = engine.services.vfs.clone().ok_or(EngineError::MissingVfs)?;
    let mission_path = normalize_engine_path("mission", &request.key)?;
    let mission_bytes = read_vfs(&vfs, &mission_path)?;

    let land_path =
        decode_mission_land_path(&mission_bytes, TmaProfile::Strict).map_err(|source| {
            EngineError::Mission {
                path: mission_path.as_str().to_string(),
                source,
            }
        })?;
    let MissionTerrainPaths {
        land_msh: land_msh_path,
        land_map: land_map_path,
    } = derive_mission_land_paths(&land_path).map_err(|source| EngineError::Path {
        role: "mission land",
        value: mission_path.as_str().to_string(),
        source,
    })?;
    let land_msh_nres = decode_nres_payload(read_vfs(&vfs, &land_msh_path)?).map_err(|source| {
        EngineError::Nres {
            path: land_msh_path.as_str().to_string(),
            source,
        }
    })?;
    let land_map_nres = decode_nres_payload(read_vfs(&vfs, &land_map_path)?).map_err(|source| {
        EngineError::Nres {
            path: land_map_path.as_str().to_string(),
            source,
        }
    })?;
    let build_dat_path = normalize_engine_path("BuildDat", "BuildDat.lst")?;
    let build_dat = read_vfs(&vfs, &build_dat_path)?;
    let (terrain, build_categories) =
        prepare_terrain_world(&land_msh_nres, &land_map_nres, &build_dat).map_err(|source| {
            match source {
                TerrainPreparationError::Decode(source) => EngineError::TerrainFormat {
                    path: build_dat_path.as_str().to_string(),
                    source,
                },
                TerrainPreparationError::Runtime(source) => EngineError::Terrain(source),
            }
        })?;
    let mission = decode_mission_payload(mission_bytes, TmaProfile::Strict).map_err(|source| {
        EngineError::Mission {
            path: mission_path.as_str().to_string(),
            source,
        }
    })?;
    let mut loaded_scripts = load_mission_scripts(&vfs, &mission, options.asset_scope)?;
    let repository = CachedResourceRepository::with_payload_cache_budget(
        vfs.clone(),
        MISSION_DECODED_PAYLOAD_CACHE_ENTRIES,
    );
    let graph_roots: Vec<_> = mission
        .objects
        .iter()
        .map(|object| resource_name(&object.resource_name.raw))
        .collect();
    let scoped_graph_roots = match options.asset_scope {
        MissionAssetScope::Full => graph_roots.as_slice(),
        MissionAssetScope::FirstMeshPreview => graph_roots.get(..1).unwrap_or_default(),
        MissionAssetScope::PreviewRoots(root_count) => graph_roots
            .get(..root_count.get().min(graph_roots.len()))
            .unwrap_or_default(),
    };
    let (mut prototype_graph, resolved_prototypes, mut prototype_report) =
        build_prototype_graph_report(&repository, vfs.as_ref(), scoped_graph_roots);
    extend_graph_report_with_visual_dependencies(
        &repository,
        &mut prototype_report,
        &mut prototype_graph,
        &resolved_prototypes,
    );
    if !prototype_report.is_success() {
        return Err(EngineError::PrototypeGraph {
            failures: prototype_report.failures.clone(),
        });
    }
    let asset_manager = AssetManager::new(repository);
    let mission_assets = match options.asset_scope {
        MissionAssetScope::Full => asset_manager.prepare_mission_assets(
            &prototype_graph.root_prototype_request_spans,
            &resolved_prototypes,
        ),
        MissionAssetScope::FirstMeshPreview => prepare_first_preview_assets(
            &asset_manager,
            &prototype_graph.root_prototype_request_spans,
            &resolved_prototypes,
        ),
        MissionAssetScope::PreviewRoots(root_count) => asset_manager.prepare_mission_assets(
            &prototype_graph.root_prototype_request_spans[..root_count
                .get()
                .min(prototype_graph.root_prototype_request_spans.len())],
            &resolved_prototypes,
        ),
    }
    .map_err(|source| EngineError::AssetPreparation {
        mission: request.key.clone(),
        source,
    })?;
    let mission_asset_plan = mission_assets.to_plan();
    let object_drafts: Vec<_> = mission
        .objects
        .iter()
        .enumerate()
        .map(|(index, object)| MissionObjectDraft {
            original_id: u32::try_from(index).ok().map(OriginalObjectId),
            resource_name_raw: object.resource_raw.clone(),
            raw_after_resource: object.raw_after_resource,
            identity_or_clan_raw: object.identity_or_clan_raw,
            raw_after_name: object.raw_after_name,
            unit_dat_header_flags: prototype_graph
                .root_unit_dat_header_flags
                .get(index)
                .copied()
                .flatten(),
            position: object.position,
            orientation_raw: object.orientation,
            scale: object.scale,
            visual_ids: mission_assets.visuals_for_object(index).to_vec(),
            properties: object
                .properties
                .iter()
                .map(|property| MissionObjectProperty {
                    raw_value: property.raw_value,
                    name_raw: property.name_raw.clone(),
                })
                .collect(),
            unit_components: prototype_graph
                .root_unit_components
                .get(index)
                .cloned()
                .unwrap_or_default(),
        })
        .collect();
    let mut new_runtime_world = new_world(WorldConfig);
    let mut handles = Vec::with_capacity(mission.objects.len());
    for (index, object) in mission.objects.iter().enumerate() {
        let original_id = u32::try_from(index).ok().map(OriginalObjectId);
        let handle = construct_object(&mut new_runtime_world, ObjectDraft { original_id })?;
        set_transform(
            &mut new_runtime_world,
            handle,
            TransformState {
                position: object.position.map(f32::to_bits),
                orientation: object.orientation.map(f32::to_bits),
                scale: object.scale.map(f32::to_bits),
            },
        )?;
        handles.push(handle);
    }
    let mut registered_objects = 0;
    for handle in &handles {
        if options.fail_after_registered_objects == Some(registered_objects) {
            let report = fparkan_world::shutdown(new_runtime_world);
            return Err(EngineError::RegistrationTeardown {
                registered_objects,
                released_objects: report.released_objects.len(),
                managers_released: report.managers_released,
            });
        }
        register_object(&mut new_runtime_world, *handle)?;
        registered_objects += 1;
    }

    let behavior_distributors = if options.asset_scope == MissionAssetScope::Full {
        let visible_clan_indices = loaded_scripts
            .bundles
            .iter()
            .map(|bundle| bundle.clan_index)
            .filter(|clan_index| {
                *clan_index < mission.clans.len()
                    && *clan_index <= LAST_SUPER_AI_TABLE_INDEX as usize
            })
            .collect::<Vec<_>>();
        let mut distributors = mission
            .clans
            .iter()
            .enumerate()
            .map(|(clan_index, _)| BehaviorDistributorState {
                clan_index,
                super_ai_visible: visible_clan_indices.contains(&clan_index),
                registered_objects: Vec::new(),
            })
            .collect::<Vec<_>>();

        for (draft, handle) in object_drafts.iter().zip(&handles) {
            let Some(class_flags) = draft.unit_dat_header_flags else {
                continue;
            };
            if !has_behavior_registration_key(class_flags) {
                continue;
            }
            let Some(clan_index) = behavior_clan_for_selector(
                draft.raw_after_resource,
                mission.clans.len(),
                &visible_clan_indices,
            ) else {
                continue;
            };
            let Some(original_id) = draft.original_id else {
                continue;
            };
            add_behavior_registered_object(
                &mut distributors[clan_index],
                BehaviorRegisteredObject {
                    original_id,
                    handle: *handle,
                    selector: draft.raw_after_resource,
                    class_flags,
                    share: 0.0,
                    native_tail: [0; 5],
                },
            );
        }
        distributors
    } else {
        Vec::new()
    };

    let behavior_ore_states = if options.asset_scope == MissionAssetScope::Full {
        // Property callbacks also run for Unit DAT objects that do not join a distributor.
        object_drafts
            .iter()
            .zip(&handles)
            .filter_map(|(draft, handle)| {
                Some(initialize_behavior_ore_state(
                    draft.original_id?,
                    *handle,
                    draft.unit_dat_header_flags?,
                    &draft.properties,
                ))
            })
            .collect()
    } else {
        Vec::new()
    };

    let control_device_tables = if options.asset_scope == MissionAssetScope::Full {
        object_drafts
            .iter()
            .zip(&handles)
            .enumerate()
            .map(
                |(object_index, (draft, handle))| MissionControlDeviceTableState {
                    original_id: draft.original_id,
                    handle: *handle,
                    status: build_control_device_table(
                        object_index,
                        draft,
                        &mission_assets,
                        prototype_graph
                            .root_prototype_request_spans
                            .get(object_index),
                        &resolved_prototypes,
                    )
                    .map_or_else(
                        MissionControlDeviceTableStatus::Unavailable,
                        MissionControlDeviceTableStatus::Available,
                    ),
                },
            )
            .collect()
    } else {
        Vec::new()
    };

    let mut next_behavior_rng_state = engine.behavior_rng_state;
    if options.asset_scope == MissionAssetScope::Full {
        let queue_time_ms = queue_time_ms.expect("full mission loading requires a queue time");
        if let Err(error) = start_mission_scripts(
            &mission,
            &mut loaded_scripts,
            queue_time_ms,
            &mut next_behavior_rng_state,
        ) {
            let _ = fparkan_world::shutdown(new_runtime_world);
            return Err(error);
        }
    }

    let summary = LoadedMission {
        key: request.key,
        path_count: mission.paths.len(),
        clan_count: mission.clans.len(),
        object_count: mission.objects.len(),
        extra_count: mission.extras.len(),
        land_msh_path: land_msh_path.as_str().to_string(),
        land_map_path: land_map_path.as_str().to_string(),
        build_category_count: build_categories.len(),
        areal_count: terrain.areal_count(),
        surface_count: terrain.surface_count(),
        registered_objects: handles.len(),
        graph_unit_reference_count: prototype_report.unit_reference_count,
        graph_direct_reference_count: prototype_report.direct_reference_count,
        graph_unit_component_count: prototype_report.unit_component_count,
        graph_root_count: prototype_report.root_count,
        graph_node_count: prototype_graph.nodes.len(),
        graph_edge_count: prototype_graph.edges.len(),
        asset_visual_count: mission_asset_plan.visual_count,
        graph_resolved_count: prototype_report.resolved_count,
        graph_mesh_dependency_count: prototype_report.mesh_dependency_count,
        graph_failure_count: prototype_report.failures.len(),
        graph_wear_request_count: prototype_report.wear_request_count,
        graph_wear_resolved_count: prototype_report.wear_resolved_count,
        graph_material_slot_count: prototype_report.material_slot_count,
        graph_material_resolved_count: prototype_report.material_resolved_count,
        graph_texture_request_count: prototype_report.texture_request_count,
        graph_texture_resolved_count: prototype_report.texture_resolved_count,
        graph_lightmap_request_count: prototype_report.lightmap_request_count,
        graph_lightmap_resolved_count: prototype_report.lightmap_resolved_count,
        asset_model_count: mission_asset_plan.model_count,
        asset_material_count: mission_asset_plan.material_count,
        asset_texture_count: mission_asset_plan.texture_count,
        asset_lightmap_count: mission_asset_plan.lightmap_count,
        script_bundle_count: loaded_scripts.bundles.len(),
        script_event_count: loaded_scripts
            .bundles
            .iter()
            .map(|bundle| bundle.package.events.len())
            .sum(),
        script_varset_declaration_count: loaded_scripts
            .varset
            .as_ref()
            .map_or(0, |varset| varset.declarations.declarations.len()),
        script_init_state_count: loaded_scripts.init_states.len(),
        script_varset_state_count: loaded_scripts.varset_states.len(),
    };

    engine.world = new_runtime_world;
    engine.behavior_rng_state = next_behavior_rng_state;
    engine.loaded = Some(LoadedMissionState {
        summary: summary.clone(),
        mission,
        terrain,
        build_categories,
        prototype_graph,
        prototype_report,
        mission_assets,
        asset_plan: mission_asset_plan,
        object_drafts,
        behavior_distributors,
        behavior_ore_states,
        control_device_tables,
        script_bundles: loaded_scripts.bundles,
        script_varset: loaded_scripts.varset,
        script_init_states: loaded_scripts.init_states,
        script_varset_states: loaded_scripts.varset_states,
        script_host_mode: request.mode,
        // Fresh IGame construction seeds this field with 5; this is not the
        // later status-4 transition performed after native game initialization.
        script_host_status: INITIAL_SCRIPT_HOST_STATUS,
        script_outcome: MissionScriptOutcome::Active,
    });
    Ok(summary)
}

fn prepare_first_preview_assets<R: ResourceRepository>(
    asset_manager: &AssetManager<R>,
    root_spans: &[std::ops::Range<usize>],
    prototypes: &[fparkan_prototype::EffectivePrototype],
) -> Result<MissionAssets, AssetPreparationError> {
    for span in root_spans {
        let assets =
            asset_manager.prepare_mission_assets(std::slice::from_ref(span), prototypes)?;
        if !assets.models.is_empty() {
            return Ok(assets);
        }
    }
    Ok(MissionAssets::default())
}

/// Steps headless mode.
///
/// # Errors
///
/// Returns [`EngineError`] when the world step fails.
pub fn step_headless(
    engine: &mut Engine,
    input: InputSnapshot,
) -> Result<FrameResult, EngineError> {
    Ok(FrameResult {
        snapshot: step(&mut engine.world, &input)?,
    })
}

/// Steps rendered mode.
///
/// # Errors
///
/// Returns [`EngineError`] when the world step fails.
pub fn frame(engine: &mut Engine) -> Result<FrameResult, EngineError> {
    step_headless(engine, InputSnapshot)
}

/// Returns the loaded mission summary.
#[must_use]
pub fn loaded_mission(engine: &Engine) -> Option<&LoadedMission> {
    engine.loaded.as_ref().map(|state| &state.summary)
}

/// Returns the decoded mission document for the loaded mission.
#[must_use]
pub fn loaded_mission_document(engine: &Engine) -> Option<&MissionDocument> {
    engine.loaded.as_ref().map(|state| &state.mission)
}

/// Returns compiled script bundles selected by TMA clan resources.
#[must_use]
pub fn loaded_mission_script_bundles(engine: &Engine) -> Option<&[MissionScriptBundle]> {
    engine
        .loaded
        .as_ref()
        .map(|state| state.script_bundles.as_slice())
}

/// Returns the numeric script defaults selected by the original `.var` fallback rule.
#[must_use]
pub fn loaded_mission_script_varset(engine: &Engine) -> Option<&MissionScriptVarSet> {
    engine
        .loaded
        .as_ref()
        .and_then(|state| state.script_varset.as_ref())
}

/// Returns the proven script `Init` writes resolved for the loaded mission.
///
/// The list has one entry for each clan whose `Init` event contains recovered
/// `Handler(19)` instructions. It is not a general script event executor.
#[must_use]
pub fn loaded_mission_script_init_states(engine: &Engine) -> Option<&[MissionScriptInitState]> {
    engine
        .loaded
        .as_ref()
        .map(|state| state.script_init_states.as_slice())
}

/// Returns instantiated per-clan varsets after proven Init writes are applied.
#[must_use]
pub fn loaded_mission_script_varset_states(engine: &Engine) -> Option<&[MissionScriptVarSetState]> {
    engine
        .loaded
        .as_ref()
        .map(|state| state.script_varset_states.as_slice())
}

/// Resolves the proven opaque callback command for one `Handler(30)` record.
///
/// Unlike the loader-only resolver, operands are read from the selected
/// clan's instantiated varset cells. Dispatching the returned host callback
/// remains separate from this function. The runtime currently models the
/// callback's recovered mode and status guards, but not the full game startup
/// transition or frame/event execution that reaches it.
///
/// # Errors
///
/// Returns an explicit error when no mission, declaration table, or clan state
/// is available, or if the instruction violates the proven handler contract.
pub fn resolve_loaded_handler30(
    engine: &Engine,
    clan_index: usize,
    instruction: &ScriptInstruction,
) -> Result<VmHostCallbackCommand, EngineError> {
    let state = engine
        .loaded
        .as_ref()
        .ok_or_else(|| EngineError::ScriptRuntime {
            clan_index,
            message: "mission script state is unavailable".to_string(),
        })?;
    let declarations = state
        .script_varset
        .as_ref()
        .ok_or_else(|| EngineError::ScriptRuntime {
            clan_index,
            message: "mission varset declarations are unavailable".to_string(),
        })?;
    let values = state
        .script_varset_states
        .iter()
        .find(|varset| varset.clan_index == clan_index)
        .ok_or_else(|| EngineError::ScriptRuntime {
            clan_index,
            message: "instantiated mission varset is unavailable".to_string(),
        })?;
    resolve_handler30_from_varset_state(declarations, values, instruction)
}

/// Returns the currently recovered mission outcome, if a mission is loaded.
#[must_use]
pub fn loaded_mission_script_outcome(engine: &Engine) -> Option<MissionScriptOutcome> {
    engine.loaded.as_ref().map(|state| state.script_outcome)
}

/// Applies the recovered `Iron3D` host-callback guards and records its outcome.
///
/// Native mission mode `3` closes the callback gate. Otherwise the callback
/// must have `(mode=0, command=0)` and payload `0` or `1`. Payload `0` writes
/// status `1` and records failure only when the current status is not `1`;
/// payload `1` writes status `0` and records completion only when the current
/// status is not `0`. A duplicate returns [`MissionScriptHostEffect::NoChange`]
/// without repairing any other state. Loaded mission state seeds the raw
/// status from the native constructor value `5`; it does not model the later
/// game-initialization transition to status `4`.
///
/// # Errors
///
/// Returns an error if no mission is loaded.
pub fn apply_loaded_script_host_callback(
    engine: &mut Engine,
    command: VmHostCallbackCommand,
) -> Result<MissionScriptHostEffect, EngineError> {
    let state = engine
        .loaded
        .as_mut()
        .ok_or_else(|| EngineError::ScriptRuntime {
            clan_index: 0,
            message: "mission script host state is unavailable".to_string(),
        })?;
    Ok(apply_recovered_script_host_callback(
        state.script_host_mode,
        &mut state.script_host_status,
        &mut state.script_outcome,
        command,
    ))
}

fn apply_recovered_script_host_callback(
    native_mode: MissionMode,
    status: &mut u32,
    outcome: &mut MissionScriptOutcome,
    command: VmHostCallbackCommand,
) -> MissionScriptHostEffect {
    if native_mode == MissionMode::AutoDemo || command.mode != 0 || command.first != 0 {
        return MissionScriptHostEffect::Unhandled;
    }
    match command.second {
        0 if *status != 1 => {
            *status = 1;
            *outcome = MissionScriptOutcome::Failed;
            MissionScriptHostEffect::MissionFailed
        }
        0 => MissionScriptHostEffect::NoChange,
        1 if *status != 0 => {
            *status = 0;
            *outcome = MissionScriptOutcome::Completed;
            MissionScriptHostEffect::MissionCompleted
        }
        1 => MissionScriptHostEffect::NoChange,
        _ => MissionScriptHostEffect::Unhandled,
    }
}

fn resolve_handler30_from_varset_state(
    declarations: &MissionScriptVarSet,
    values: &MissionScriptVarSetState,
    instruction: &ScriptInstruction,
) -> Result<VmHostCallbackCommand, EngineError> {
    declarations
        .declarations
        .resolve_handler30_with_values(instruction, &values.values)
        .map_err(|source| EngineError::ScriptRuntime {
            clan_index: values.clan_index,
            message: source.to_string(),
        })
}

/// Returns terrain runtime data for the loaded mission.
#[must_use]
pub fn loaded_terrain(engine: &Engine) -> Option<&TerrainWorld> {
    engine.loaded.as_ref().map(|state| &state.terrain)
}

/// Returns decoded build categories for the loaded game root.
#[must_use]
pub fn loaded_build_categories(engine: &Engine) -> Option<&[BuildCategory]> {
    engine
        .loaded
        .as_ref()
        .map(|state| state.build_categories.as_slice())
}

/// Returns the loaded prototype graph.
#[must_use]
pub fn loaded_prototype_graph(engine: &Engine) -> Option<&PrototypeGraph> {
    engine.loaded.as_ref().map(|state| &state.prototype_graph)
}

/// Returns the loaded prototype graph report.
#[must_use]
pub fn loaded_prototype_graph_report(engine: &Engine) -> Option<&PrototypeGraphReport> {
    engine.loaded.as_ref().map(|state| &state.prototype_report)
}

/// Returns the prepared mission asset plan for the loaded mission.
#[must_use]
pub fn loaded_mission_asset_plan(engine: &Engine) -> Option<&MissionAssetPlan> {
    engine.loaded.as_ref().map(|state| &state.asset_plan)
}

/// Returns prepared mission assets for the loaded mission.
#[must_use]
pub fn loaded_mission_assets(engine: &Engine) -> Option<&MissionAssets> {
    engine.loaded.as_ref().map(|state| &state.mission_assets)
}

/// Returns mission-derived object drafts preserved for later runtime stages.
#[must_use]
pub fn loaded_mission_object_drafts(engine: &Engine) -> Option<&[MissionObjectDraft]> {
    engine
        .loaded
        .as_ref()
        .map(|state| state.object_drafts.as_slice())
}

/// Returns the clan-indexed Behavior distributors and their registered objects.
#[must_use]
pub fn loaded_mission_behavior_distributors(
    engine: &Engine,
) -> Option<&[BehaviorDistributorState]> {
    engine
        .loaded
        .as_ref()
        .map(|state| state.behavior_distributors.as_slice())
}

/// Returns native ore properties attached to full Unit DAT mission objects.
#[must_use]
pub fn loaded_mission_behavior_ore_states(engine: &Engine) -> Option<&[BehaviorOreState]> {
    engine
        .loaded
        .as_ref()
        .map(|state| state.behavior_ore_states.as_slice())
}

/// Returns the base Control device table projections for loaded mission objects.
#[must_use]
pub fn loaded_mission_control_device_tables(
    engine: &Engine,
) -> Option<&[MissionControlDeviceTableState]> {
    engine
        .loaded
        .as_ref()
        .map(|state| state.control_device_tables.as_slice())
}

fn behavior_clan_for_selector(
    selector: u32,
    clan_count: usize,
    super_ai_clan_indices: &[usize],
) -> Option<usize> {
    if selector > LAST_SUPER_AI_TABLE_INDEX {
        return None;
    }
    let clan_index = usize::try_from(selector).ok()?;
    (clan_index < clan_count && super_ai_clan_indices.contains(&clan_index)).then_some(clan_index)
}

fn has_behavior_registration_key(class_flags: u32) -> bool {
    class_flags & BEHAVIOR_REGISTRATION_KEY_FLAG != 0
}

fn native_behavior_ore_defaults(class_flags: u32) -> (u32, u32) {
    match class_flags {
        0x8000_0004 => (0, 0x43fa_0000),
        0x8000_0008 => (0, 0x457a_0000),
        0x0100_2000 => (0, 0x44fa_0000),
        0x0100_4000 => (0x44fa_0000, 0x44fa_0000),
        _ => (0, 0),
    }
}

fn initialize_behavior_ore_state(
    original_id: OriginalObjectId,
    handle: ObjectHandle,
    class_flags: u32,
    properties: &[MissionObjectProperty],
) -> BehaviorOreState {
    let (mut current_ore_bits, maximum_ore_bits) = native_behavior_ore_defaults(class_flags);
    if let Some(property) = properties
        .iter()
        .find(|property| property.name_raw.as_slice() == b"CurrentOre")
    {
        current_ore_bits = property.raw_value[1];
    }
    BehaviorOreState {
        original_id,
        handle,
        class_flags,
        current_ore_bits,
        maximum_ore_bits,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ControlTableTarget {
    Append,
    Override(u32),
}

fn native_control_target_action(target: u32) -> ControlTableTarget {
    if target == u32::MAX {
        ControlTableTarget::Append
    } else {
        ControlTableTarget::Override(target)
    }
}

fn native_component_target_index(
    source_type: Option<u32>,
    devices: &[EffectiveControlDevice],
    parent_component_id: u32,
    raw_local_index: i32,
) -> u32 {
    if source_type == Some(PROTOTYPE_TYPE_EXTO) {
        // AniMesh writes EXTO's mount node to descriptor C4; C8 remains -1,
        // the Control append sentinel.
        u32::MAX
    } else {
        native_into_table_target_index(devices, parent_component_id, raw_local_index)
    }
}

fn native_into_table_target_index(
    devices: &[EffectiveControlDevice],
    parent_component_id: u32,
    raw_local_index: i32,
) -> u32 {
    let first_parent_device = devices
        .iter()
        .position(|device| device.component_id == parent_component_id)
        .and_then(|index| u32::try_from(index).ok())
        .unwrap_or(u32::MAX);
    first_parent_device.wrapping_add(raw_local_index as u32)
}

fn native_into_append_key(raw_key: u32) -> u32 {
    if raw_key == 0 {
        u32::MAX
    } else {
        raw_key
    }
}

fn native_ex_to_append_key(
    raw_key: u32,
    component_id: u32,
    mount_node: Option<u32>,
    first_global_node: Option<u32>,
) -> Option<u32> {
    if raw_key == 0 {
        return mount_node;
    }
    if component_id == 0 {
        return Some(raw_key);
    }
    let mount_node = mount_node?;
    if mount_node == u32::MAX {
        return Some(raw_key);
    }
    let first_global_node = first_global_node.unwrap_or(u32::MAX);
    Some(raw_key.wrapping_add(first_global_node).wrapping_sub(1))
}

fn active_unit_component_order(
    records: &[UnitComponentRecord],
    tree: &[UnitComponentTreeNode],
) -> Vec<usize> {
    if records.is_empty() || tree.is_empty() {
        return Vec::new();
    }
    let mut order = vec![0];
    let mut pending = tree[0].children.iter().rev().copied().collect::<Vec<_>>();
    while let Some(node_index) = pending.pop() {
        let Some(node) = tree.get(node_index) else {
            continue;
        };
        let component_index = node.record_index;
        let Some(component) = records.get(component_index) else {
            continue;
        };
        if component.kind == 0 {
            // The native raw-kind guard returns before visiting this subtree.
            continue;
        }
        order.push(component_index);
        pending.extend(node.children.iter().rev().copied());
    }
    order
}

fn control_device_record_provenance(
    component_index: usize,
    record_index: usize,
    control: &PreparedComponentControl,
) -> ControlDeviceRecordProvenance {
    ControlDeviceRecordProvenance {
        unit_component_index: component_index,
        typed_record_index: record_index,
        source: control.source.clone(),
    }
}

fn append_effective_control_device(
    devices: &mut Vec<EffectiveControlDevice>,
    component_id: u32,
    key: u32,
    component_index: usize,
    record_index: usize,
    control: &PreparedComponentControl,
    descriptor: &PreparedControlTypedRecord,
) {
    let provenance = control_device_record_provenance(component_index, record_index, control);
    let mut coefficient_words = [0; 16];
    coefficient_words.copy_from_slice(&descriptor.fixed_words[11..27]);
    let coefficient_bits =
        control_factory_uses_common_setter(descriptor.tag).then_some(coefficient_words);
    let (multiplier_bits, default_word_50) = native_control_factory_defaults(descriptor.tag);
    let native_word_50_bits = if descriptor.fixed_words[6] == u32::MAX {
        default_word_50
    } else {
        Some(descriptor.fixed_words[6])
    };
    devices.push(EffectiveControlDevice {
        factory_tag: descriptor.tag,
        component_id,
        key,
        coefficient_bits,
        multiplier_bits,
        native_word_50_bits,
        descriptor: descriptor.clone(),
        factory_provenance: provenance.clone(),
        descriptor_provenance: provenance,
    });
}

fn apply_effective_control_override(
    device: &mut EffectiveControlDevice,
    descriptor: &PreparedControlTypedRecord,
    component_index: usize,
    record_index: usize,
    control: &PreparedComponentControl,
) {
    let mut coefficient_words = [0; 16];
    coefficient_words.copy_from_slice(&descriptor.fixed_words[11..27]);
    device.coefficient_bits = Some(coefficient_words);
    if descriptor.fixed_words[6] != u32::MAX {
        device.native_word_50_bits = Some(descriptor.fixed_words[6]);
    }
    device.descriptor = descriptor.clone();
    device.descriptor_provenance =
        control_device_record_provenance(component_index, record_index, control);
}

fn control_factory_uses_common_setter(factory_tag: u32) -> bool {
    matches!(
        factory_tag,
        1 | 2 | 4 | 5 | 8 | 9 | 10 | 15 | 17 | 19 | 25 | 26 | 30
    )
}

fn native_control_factory_defaults(factory_tag: u32) -> (Option<u32>, Option<u32>) {
    let flags = match factory_tag {
        1 | 4 => 0x200,
        5 | 8 | 26 => 0x5,
        10 => 0x2000,
        17 | 19 => 0x20,
        _ => return (None, None),
    };
    (Some(0x3f80_0000), Some(flags))
}

fn build_control_device_table(
    object_index: usize,
    draft: &MissionObjectDraft,
    assets: &MissionAssets,
    prototype_span: Option<&std::ops::Range<usize>>,
    prototypes: &[fparkan_prototype::EffectivePrototype],
) -> Result<Vec<EffectiveControlDevice>, MissionControlDeviceTableUnavailableReason> {
    use MissionControlDeviceTableUnavailableReason as Unavailable;

    draft
        .unit_dat_header_flags
        .ok_or(Unavailable::NoUnitDatTree)?;
    if draft.unit_components.is_empty() {
        return Err(Unavailable::InvalidUnitComponentTree);
    }
    let tree = match unit_component_tree(&draft.unit_components) {
        Ok(tree) if !tree.is_empty() => tree,
        _ => return Err(Unavailable::InvalidUnitComponentTree),
    };
    let span = prototype_span.ok_or(Unavailable::ComponentAlignmentMismatch)?;
    let component_controls = assets
        .object_component_controls
        .get(object_index)
        .ok_or(Unavailable::ComponentAlignmentMismatch)?;
    let object_visuals = assets
        .object_visuals
        .get(object_index)
        .ok_or(Unavailable::ComponentAlignmentMismatch)?;
    if span.start > span.end
        || span.end > prototypes.len()
        || span.end - span.start != draft.unit_components.len()
        || component_controls.len() != draft.unit_components.len()
        || object_visuals.len() != draft.unit_components.len()
    {
        return Err(Unavailable::ComponentAlignmentMismatch);
    }

    let prototype_for = |component_index: usize| {
        span.start
            .checked_add(component_index)
            .and_then(|index| prototypes.get(index))
    };
    let active_components = active_unit_component_order(&draft.unit_components, &tree);
    if active_components.first() != Some(&0) {
        return Err(Unavailable::InvalidUnitComponentTree);
    }

    let has_ex_to = active_components.iter().any(|component_index| {
        prototype_for(*component_index)
            .is_some_and(|prototype| prototype.source_type == Some(PROTOTYPE_TYPE_EXTO))
    });
    if has_ex_to {
        for component_index in active_components
            .iter()
            .copied()
            .filter(|index| *index != 0)
        {
            let prototype =
                prototype_for(component_index).ok_or(Unavailable::ComponentAlignmentMismatch)?;
            if prototype.source_type.is_none() {
                return Err(Unavailable::UnknownComponentKindForExTo {
                    unit_component_index: component_index,
                    source_type: None,
                });
            }
        }
    }

    let mut component_ids = vec![None; draft.unit_components.len()];
    component_ids[0] = Some(0_u32);
    let mut next_component_id = 1_u32;
    for component_index in active_components.iter().copied().skip(1) {
        component_ids[component_index] = Some(next_component_id);
        let next = next_component_id
            .checked_add(1)
            .ok_or(Unavailable::ComponentAlignmentMismatch)?;
        next_component_id = next;
    }

    let mut first_global_nodes = vec![None; draft.unit_components.len()];
    if has_ex_to {
        let root_prototype = prototype_for(0).ok_or(Unavailable::ComponentAlignmentMismatch)?;
        let root_is_mesh = matches!(
            root_prototype.geometry,
            fparkan_prototype::PrototypeGeometry::Mesh(_)
        );
        let root_node_count = object_visuals
            .first()
            .and_then(|visual_id| assets.visual_by_id(*visual_id))
            .map(|visual| visual.model_nodes)
            .ok_or(Unavailable::UnsupportedExToNodeMapping {
                unit_component_index: 0,
            })?;
        if !root_is_mesh || root_node_count == 0 {
            return Err(Unavailable::UnsupportedExToNodeMapping {
                unit_component_index: 0,
            });
        }
        let mut global_node_cursor = u32::try_from(root_node_count).map_err(|_| {
            Unavailable::UnsupportedExToNodeMapping {
                unit_component_index: 0,
            }
        })?;
        first_global_nodes[0] = Some(0_u32);
        for component_index in active_components.iter().copied().skip(1) {
            let prototype =
                prototype_for(component_index).ok_or(Unavailable::ComponentAlignmentMismatch)?;
            match prototype.source_type {
                Some(PROTOTYPE_TYPE_EXTO) => {
                    if !matches!(
                        prototype.geometry,
                        fparkan_prototype::PrototypeGeometry::Mesh(_)
                    ) {
                        return Err(Unavailable::UnsupportedExToNodeMapping {
                            unit_component_index: component_index,
                        });
                    }
                    let Some(node_count) = object_visuals
                        .get(component_index)
                        .and_then(|visual_id| assets.visual_by_id(*visual_id))
                        .map(|visual| visual.model_nodes)
                    else {
                        return Err(Unavailable::UnsupportedExToNodeMapping {
                            unit_component_index: component_index,
                        });
                    };
                    if node_count == 0 {
                        return Err(Unavailable::UnsupportedExToNodeMapping {
                            unit_component_index: component_index,
                        });
                    }
                    if node_count > 1 {
                        first_global_nodes[component_index] = Some(global_node_cursor);
                    }
                    let contribution = u32::try_from(node_count - 1).map_err(|_| {
                        Unavailable::UnsupportedExToNodeMapping {
                            unit_component_index: component_index,
                        }
                    })?;
                    let next_cursor =
                        global_node_cursor
                            .checked_add(contribution)
                            .ok_or_else(|| Unavailable::UnsupportedExToNodeMapping {
                                unit_component_index: component_index,
                            })?;
                    global_node_cursor = next_cursor;
                }
                Some(_) => {}
                None => {
                    return Err(Unavailable::UnknownComponentKindForExTo {
                        unit_component_index: component_index,
                        source_type: None,
                    });
                }
            }
        }
    }

    let mut devices = Vec::new();
    let mut has_control_resource = false;
    for component_index in active_components {
        let prototype =
            prototype_for(component_index).ok_or(Unavailable::ComponentAlignmentMismatch)?;
        if prototype.control_resource.is_none() {
            continue;
        }
        has_control_resource = true;
        if matches!(
            prototype.geometry,
            fparkan_prototype::PrototypeGeometry::NonGeometric
        ) {
            return Err(Unavailable::NonGeometricControlResource {
                unit_component_index: component_index,
            });
        }
        let control = component_controls[component_index].as_ref().ok_or(
            Unavailable::ControlResourceUnavailable {
                unit_component_index: component_index,
            },
        )?;
        if let PreparedControlEffectSection::Unsupported {
            typed_record_index: Some(first_unparsed_record),
            ..
        } = &control.effect_section
        {
            return Err(Unavailable::IncompleteTypedRecordList {
                unit_component_index: component_index,
                first_unparsed_record: *first_unparsed_record,
            });
        }

        let component_id =
            component_ids[component_index].ok_or(Unavailable::ComponentAlignmentMismatch)?;
        let is_root = component_index == 0;
        let is_ex_to = prototype.source_type == Some(PROTOTYPE_TYPE_EXTO);
        if is_root {
            for (record_index, descriptor) in control.typed_records.iter().enumerate() {
                append_effective_control_device(
                    &mut devices,
                    component_id,
                    descriptor.fixed_words[1],
                    component_index,
                    record_index,
                    control,
                    descriptor,
                );
            }
            continue;
        }

        let raw_local_index = draft.unit_components[component_index].parent_or_link;
        let parent_component_id = tree
            .get(component_index)
            .and_then(|node| node.parent_index)
            .and_then(|parent_index| component_ids.get(parent_index).copied().flatten())
            .unwrap_or(u32::MAX);
        let native_target = native_component_target_index(
            prototype.source_type,
            &devices,
            parent_component_id,
            raw_local_index,
        );
        match native_control_target_action(native_target) {
            ControlTableTarget::Append => {
                let mount_node = if is_ex_to {
                    tree.get(component_index)
                        .and_then(|node| node.parent_index)
                        .and_then(|parent_index| {
                            let parent_id = component_ids.get(parent_index).copied().flatten()?;
                            let parent_first = first_global_nodes
                                .get(parent_index)
                                .copied()
                                .flatten()
                                .unwrap_or(u32::MAX);
                            Some(
                                parent_first
                                    .wrapping_add(raw_local_index as u32)
                                    .wrapping_sub(u32::from(parent_id != 0)),
                            )
                        })
                } else {
                    None
                };
                let first_global_node = first_global_nodes.get(component_index).copied().flatten();
                for (record_index, descriptor) in control.typed_records.iter().enumerate() {
                    let raw_key = descriptor.fixed_words[1];
                    let key = if is_ex_to {
                        native_ex_to_append_key(
                            raw_key,
                            component_id,
                            mount_node,
                            first_global_node,
                        )
                        .ok_or(
                            Unavailable::UnsupportedExToNodeMapping {
                                unit_component_index: component_index,
                            },
                        )?
                    } else {
                        native_into_append_key(raw_key)
                    };
                    append_effective_control_device(
                        &mut devices,
                        component_id,
                        key,
                        component_index,
                        record_index,
                        control,
                        descriptor,
                    );
                }
            }
            ControlTableTarget::Override(target_index) => {
                let descriptor =
                    control
                        .typed_records
                        .first()
                        .ok_or(Unavailable::MissingOverrideRecord {
                            unit_component_index: component_index,
                        })?;
                let target_index_usize = usize::try_from(target_index).map_err(|_| {
                    Unavailable::OverrideIndexOutOfRange {
                        unit_component_index: component_index,
                        target_index,
                    }
                })?;
                let device = devices.get_mut(target_index_usize).ok_or(
                    Unavailable::OverrideIndexOutOfRange {
                        unit_component_index: component_index,
                        target_index,
                    },
                )?;
                if !matches!(device.factory_tag, 9 | 15 | 19 | 25) {
                    return Err(Unavailable::UnsupportedOverrideReceiver {
                        factory_tag: device.factory_tag,
                    });
                }
                apply_effective_control_override(device, descriptor, component_index, 0, control);
            }
        }
    }
    if !has_control_resource {
        return Err(Unavailable::NoControlResource);
    }
    Ok(devices)
}

fn add_behavior_registered_object(
    distributor: &mut BehaviorDistributorState,
    mut object: BehaviorRegisteredObject,
) {
    let existing_index = distributor
        .registered_objects
        .iter()
        .position(|entry| entry.handle == object.handle);
    let count = distributor.registered_objects.len() + usize::from(existing_index.is_none());

    // Native Add/normalize ran with x87 control word 0x007f. Keep the list-order
    // arithmetic in f32 so each operation has the observed 24-bit precision.
    let share = 1.0_f32 / count as f32;
    let handle = object.handle;
    if let Some(index) = existing_index {
        distributor.registered_objects[index].share = share;
    } else {
        object.share = share;
        object.native_tail = [0; 5];
        distributor.registered_objects.push(object);
    }

    normalize_behavior_distributor(distributor, handle);
}

fn normalize_behavior_distributor(
    distributor: &mut BehaviorDistributorState,
    fixed_handle: ObjectHandle,
) {
    let mut fixed_sum = 0.0_f32;
    let mut free_sum = 0.0_f32;
    let mut adjustable_count = 0_usize;

    for object in &distributor.registered_objects {
        if object.handle == fixed_handle || object.native_tail[0] != 0 {
            fixed_sum = fixed_sum + object.share;
        } else {
            free_sum = free_sum + object.share;
            adjustable_count += 1;
        }
    }

    if adjustable_count == 0 {
        return;
    }

    if free_sum <= 0.0 {
        let share = (1.0_f32 - fixed_sum) / adjustable_count as f32;
        for object in &mut distributor.registered_objects {
            if object.handle != fixed_handle && object.native_tail[0] == 0 {
                object.share = share;
            }
        }
    } else {
        let factor = (1.0_f32 - fixed_sum) / free_sum;
        for object in &mut distributor.registered_objects {
            if object.handle != fixed_handle && object.native_tail[0] == 0 {
                object.share = object.share * factor;
            }
        }
    }
}

fn normalize_engine_path(role: &'static str, value: &str) -> Result<NormalizedPath, EngineError> {
    normalize_relative(value.as_bytes(), PathPolicy::StrictLegacy).map_err(|source| {
        EngineError::Path {
            role,
            value: value.to_string(),
            source,
        }
    })
}

fn load_mission_scripts(
    vfs: &Arc<dyn Vfs>,
    mission: &MissionDocument,
    asset_scope: MissionAssetScope,
) -> Result<LoadedMissionScripts, EngineError> {
    let script_bases = mission_script_bundle_bases(mission);
    let mut bundles = Vec::with_capacity(script_bases.len());
    let mut first_base_path = None;
    for script_base in script_bases {
        let base_path = String::from_utf8_lossy(&script_base.path_raw).into_owned();
        let script_path = if base_path.to_ascii_lowercase().ends_with(".scr") {
            base_path.clone()
        } else {
            format!("{base_path}.scr")
        };
        let normalized = normalize_engine_path("mission script", &script_path)?;
        let bytes = read_vfs(vfs, &normalized)?;
        let package = fparkan_script::decode(&bytes).map_err(|source| EngineError::Script {
            path: normalized.as_str().to_string(),
            message: source.to_string(),
        })?;
        if first_base_path.is_none() {
            first_base_path = Some(base_path.clone());
        }
        bundles.push(MissionScriptBundle {
            clan_index: script_base.clan_index,
            base_path,
            script_path: normalized.as_str().to_string(),
            package,
            init_execution: MissionScriptInitExecution::Missing,
            behavior_warm_start: None,
        });
    }
    let varset = first_base_path
        .as_deref()
        .map(|base_path| load_script_varset(vfs, base_path))
        .transpose()?;
    let (init_states, varset_states) = if asset_scope == MissionAssetScope::Full {
        (Vec::new(), Vec::new())
    } else if let Some(varset) = &varset {
        let init_states = resolve_handler19_init_states(
            &mission
                .clans
                .iter()
                .map(|clan| clan.anchor)
                .collect::<Vec<_>>(),
            &mut bundles,
            varset,
            asset_scope,
        )?;
        let varset_states = materialize_script_varset_states(&bundles, varset, &init_states)?;
        (init_states, varset_states)
    } else {
        (Vec::new(), Vec::new())
    };
    Ok(LoadedMissionScripts {
        bundles,
        varset,
        init_states,
        varset_states,
    })
}

fn start_mission_scripts(
    mission: &MissionDocument,
    scripts: &mut LoadedMissionScripts,
    queue_time_ms: u32,
    behavior_rng_state: &mut u32,
) -> Result<(), EngineError> {
    // The recovered slot refreshes one Behavior instance before dispatching
    // that same instance's Init event. The Rust loader waits until its new
    // world is registered before it starts this per-bundle sequence.
    let clan_anchors = mission
        .clans
        .iter()
        .map(|clan| clan.anchor)
        .collect::<Vec<_>>();
    let mut init_states = Vec::new();
    for bundle in &mut scripts.bundles {
        bundle.behavior_warm_start = Some(BehaviorWarmStartState::start(
            queue_time_ms,
            behavior_rng_state,
        ));

        if let Some(varset) = scripts.varset.as_ref() {
            init_states.extend(resolve_handler19_init_states(
                &clan_anchors,
                std::slice::from_mut(bundle),
                varset,
                MissionAssetScope::Full,
            )?);
        }
    }
    scripts.init_states = init_states;
    scripts.varset_states = match scripts.varset.as_ref() {
        Some(varset) => {
            materialize_script_varset_states(&scripts.bundles, varset, &scripts.init_states)?
        }
        None => Vec::new(),
    };
    Ok(())
}

fn materialize_script_varset_states(
    bundles: &[MissionScriptBundle],
    varset: &MissionScriptVarSet,
    init_states: &[MissionScriptInitState],
) -> Result<Vec<MissionScriptVarSetState>, EngineError> {
    let defaults: Vec<_> = varset
        .declarations
        .declarations
        .iter()
        .map(|declaration| declaration.default_value)
        .collect();
    bundles
        .iter()
        .map(|bundle| {
            let mut values = defaults.clone();
            for init in init_states
                .iter()
                .filter(|init| init.clan_index == bundle.clan_index)
            {
                for write in &init.writes {
                    let Some(value) = values.get_mut(write.index as usize) else {
                        return Err(EngineError::ScriptInit {
                            clan_index: bundle.clan_index,
                            message: format!(
                                "Handler(19) write index {} is outside the instantiated varset",
                                write.index
                            ),
                        });
                    };
                    *value = VarSetDefault::Dword(write.value);
                }
            }
            Ok(MissionScriptVarSetState {
                clan_index: bundle.clan_index,
                values,
            })
        })
        .collect()
}

fn resolve_handler19_init_states(
    clan_anchors: &[[f32; 2]],
    bundles: &mut [MissionScriptBundle],
    varset: &MissionScriptVarSet,
    asset_scope: MissionAssetScope,
) -> Result<Vec<MissionScriptInitState>, EngineError> {
    let mut states = Vec::new();
    for bundle in bundles {
        if !bundle
            .package
            .events
            .iter()
            .any(|event| event.name_raw.split(|byte| *byte == 0).next() == Some(&b"Init"[..]))
        {
            bundle.init_execution = MissionScriptInitExecution::Missing;
            continue;
        }
        let Some(anchor) = clan_anchors.get(bundle.clan_index) else {
            return Err(EngineError::ScriptInit {
                clan_index: bundle.clan_index,
                message: "script bundle clan index is outside the decoded mission".to_string(),
            });
        };
        let input = Handler19InitInput {
            first_x87_word: handler19_anchor_word(anchor[0], bundle.clan_index, "x")?,
            second_x87_word: handler19_anchor_word(anchor[1], bundle.clan_index, "y")?,
            third_word: u32::try_from(bundle.clan_index).map_err(|_| EngineError::ScriptInit {
                clan_index: bundle.clan_index,
                message: "clan index does not fit the recovered DWORD ClanID field".to_string(),
            })?,
        };
        match varset
            .declarations
            .resolve_handler19_init(&bundle.package, input)
            .map_err(|source| EngineError::ScriptInit {
                clan_index: bundle.clan_index,
                message: source.to_string(),
            })? {
            Handler19InitResolution::Missing => {
                bundle.init_execution = MissionScriptInitExecution::Missing;
            }
            Handler19InitResolution::Executed {
                event_index,
                event_word,
                writes,
            } => {
                bundle.init_execution = MissionScriptInitExecution::Executed { event_index };
                if !writes.is_empty() {
                    states.push(MissionScriptInitState {
                        clan_index: bundle.clan_index,
                        event_index,
                        event_word,
                        writes,
                    });
                }
            }
            Handler19InitResolution::Unsupported {
                event_index,
                instruction_index,
                error,
            } if asset_scope != MissionAssetScope::Full => {
                bundle.init_execution = MissionScriptInitExecution::Unsupported {
                    event_index,
                    instruction_index,
                    error,
                };
            }
            Handler19InitResolution::Unsupported {
                event_index,
                instruction_index,
                error,
            } => {
                return Err(EngineError::ScriptInitUnsupported {
                    clan_index: bundle.clan_index,
                    path: bundle.script_path.clone(),
                    event_index,
                    instruction_index,
                    source: error,
                });
            }
        }
    }
    Ok(states)
}

fn handler19_anchor_word(
    value: f32,
    clan_index: usize,
    axis: &'static str,
) -> Result<u32, EngineError> {
    const SIGNED_QWORD_LIMIT: f32 = 9_223_372_036_854_775_808.0;

    let rounded = value.round_ties_even();
    if !value.is_finite() || rounded < -SIGNED_QWORD_LIMIT || rounded >= SIGNED_QWORD_LIMIT {
        return Err(EngineError::ScriptInit {
            clan_index,
            message: format!(
                "ClanBase{axis}={value:?} is outside the modeled x87 FISTP qword range"
            ),
        });
    }

    // Iron3D FISTP qword keeps its low DWORD; AI FUN_10005c70 then round-trips
    // that unsigned word through f32 before Handler(19)'s truncating __ftol.
    let creation_word = (rounded as i64) as u32;
    let ai_field = creation_word as f32;
    let handler_qword = ai_field.trunc() as i64;
    Ok(handler_qword as u32)
}

fn load_script_varset(
    vfs: &Arc<dyn Vfs>,
    base_path: &str,
) -> Result<MissionScriptVarSet, EngineError> {
    let base_path = base_path
        .strip_suffix(".scr")
        .or_else(|| base_path.strip_suffix(".SCR"))
        .unwrap_or(base_path);
    let primary_path = normalize_engine_path("mission script varset", &format!("{base_path}.var"))?;
    let (path, bytes, used_fallback) = match vfs.read(&primary_path) {
        Ok(bytes) => (primary_path, bytes, false),
        Err(VfsError::NotFound(_)) => {
            let fallback_path =
                normalize_engine_path("fallback script varset", FALLBACK_SCRIPT_VARSET_PATH)?;
            let bytes = read_vfs(vfs, &fallback_path)?;
            (fallback_path, bytes, true)
        }
        Err(source) => {
            return Err(EngineError::Vfs {
                path: primary_path.as_str().to_string(),
                source,
            });
        }
    };
    let declarations =
        fparkan_script::parse_varset(&bytes).map_err(|source| EngineError::VarSet {
            path: path.as_str().to_string(),
            message: source.to_string(),
        })?;
    Ok(MissionScriptVarSet {
        path: path.as_str().to_string(),
        used_fallback,
        declarations,
    })
}

fn read_vfs(vfs: &Arc<dyn Vfs>, path: &NormalizedPath) -> Result<Arc<[u8]>, EngineError> {
    vfs.read(path).map_err(|source| EngineError::Vfs {
        path: path.as_str().to_string(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_script::ScriptDispatchSelector;
    use fparkan_vfs::{DirectoryVfs, MemoryVfs, OverlayVfs, VfsEntry, VfsMetadata};
    use std::path::{Path, PathBuf};

    fn basement_draft(header_flags: Option<u32>, raw_after_name: u32) -> MissionObjectDraft {
        MissionObjectDraft {
            original_id: Some(OriginalObjectId(7)),
            resource_name_raw: b"BUILDS/example.dat".to_vec(),
            raw_after_resource: 0,
            identity_or_clan_raw: 0,
            raw_after_name,
            unit_dat_header_flags: header_flags,
            position: [12.0, 34.0, 100.0],
            orientation_raw: [0.0; 3],
            scale: [1.0; 3],
            visual_ids: Vec::new(),
            properties: Vec::new(),
            unit_components: Vec::new(),
        }
    }

    fn synthetic_resource_key(index: usize) -> ResourceKey {
        ResourceKey {
            archive: normalize_engine_path("test", &format!("synthetic/{index}.rlb"))
                .expect("synthetic archive path"),
            name: fparkan_path::ResourceName(format!("entry{index}").into_bytes()),
            type_id: None,
        }
    }

    fn synthetic_control_record(
        tag: u32,
        key: u32,
        first_coefficient: u32,
        native_word_50: Option<u32>,
    ) -> PreparedControlTypedRecord {
        let mut fixed_words = [0; 44];
        fixed_words[1] = key;
        fixed_words[6] = native_word_50.unwrap_or(u32::MAX);
        fixed_words[11] = first_coefficient;
        PreparedControlTypedRecord {
            tag,
            fixed_words,
            values: Vec::new(),
            string_bytes: Vec::new(),
            gate_provider: fparkan_assets::PreparedControlGateProvider::NotApplicable,
        }
    }

    fn synthetic_component_control(
        index: usize,
        typed_records: Vec<PreparedControlTypedRecord>,
        effect_section: PreparedControlEffectSection,
    ) -> PreparedComponentControl {
        PreparedComponentControl {
            source: synthetic_resource_key(100 + index),
            model_node_count: 0,
            bindings: Vec::new(),
            timeline_rows: Vec::new(),
            typed_records,
            node_data: None,
            effect_section,
            initial_effect_group: None,
            point_list: None,
        }
    }

    fn synthetic_control_fixture(
        source_types: &[Option<u32>],
        kinds: &[u32],
        child_counts: &[u32],
        parent_or_links: &[i32],
        model_nodes: &[usize],
        controls: Vec<Option<PreparedComponentControl>>,
    ) -> (
        MissionObjectDraft,
        MissionAssets,
        Vec<fparkan_prototype::EffectivePrototype>,
    ) {
        let component_count = source_types.len();
        assert_eq!(kinds.len(), component_count);
        assert_eq!(child_counts.len(), component_count);
        assert_eq!(parent_or_links.len(), component_count);
        assert_eq!(model_nodes.len(), component_count);
        assert_eq!(controls.len(), component_count);

        let unit_components = (0..component_count)
            .map(|index| UnitComponentRecord {
                archive_raw: [0; 32],
                resource_raw: [0; 32],
                kind: kinds[index],
                parent_or_link: parent_or_links[index],
                description_raw: [0; 32],
                tail0: 0,
                immediate_child_count: child_counts[index],
            })
            .collect::<Vec<_>>();
        let mut assets = MissionAssets::default();
        let mut prototypes = Vec::with_capacity(component_count);
        let mut visuals = Vec::with_capacity(component_count);
        let mut visual_ids = Vec::with_capacity(component_count);
        for index in 0..component_count {
            let mesh = synthetic_resource_key(200 + index);
            let visual_id = AssetId::new(index as u64);
            visual_ids.push(visual_id);
            visuals.push(PreparedVisual {
                id: visual_id,
                mesh: Some(mesh.clone()),
                source_type: source_types[index],
                model_id: None,
                wear_id: None,
                model_nodes: model_nodes[index],
                model_slots: 0,
                model_batches: 0,
                material_count: 0,
                material_ids: Vec::new(),
                texture_ids: Vec::new(),
                lightmap_ids: Vec::new(),
                texture_count: 0,
                lightmap_count: 0,
            });
            prototypes.push(fparkan_prototype::EffectivePrototype {
                key: fparkan_prototype::PrototypeKey(fparkan_path::ResourceName(
                    format!("prototype{index}").into_bytes(),
                )),
                geometry: fparkan_prototype::PrototypeGeometry::Mesh(mesh),
                source: fparkan_prototype::PrototypeSource::ObjectsRegistry,
                source_type: source_types[index],
                dependencies: Vec::new(),
                control_resource: controls[index]
                    .as_ref()
                    .map(|control| control.source.clone()),
                point_resource: None,
                node_data_resource: None,
                bas_resource: None,
            });
        }
        assets.visuals = visuals;
        assets.object_visuals = vec![visual_ids.clone()];
        assets.object_component_controls = vec![controls];
        let draft = MissionObjectDraft {
            original_id: Some(OriginalObjectId(0)),
            resource_name_raw: b"TEST/ROOT.DAT".to_vec(),
            raw_after_resource: 0,
            identity_or_clan_raw: 0,
            raw_after_name: 0,
            unit_dat_header_flags: Some(0),
            position: [0.0; 3],
            orientation_raw: [0.0; 3],
            scale: [1.0; 3],
            visual_ids,
            properties: Vec::new(),
            unit_components,
        };
        (draft, assets, prototypes)
    }

    fn synthetic_control_table(
        source_types: &[Option<u32>],
        kinds: &[u32],
        child_counts: &[u32],
        parent_or_links: &[i32],
        model_nodes: &[usize],
        controls: Vec<Option<PreparedComponentControl>>,
    ) -> MissionControlDeviceTableStatus {
        let (draft, assets, prototypes) = synthetic_control_fixture(
            source_types,
            kinds,
            child_counts,
            parent_or_links,
            model_nodes,
            controls,
        );
        build_control_device_table(
            0,
            &draft,
            &assets,
            Some(&(0..prototypes.len())),
            &prototypes,
        )
        .map_or_else(
            MissionControlDeviceTableStatus::Unavailable,
            MissionControlDeviceTableStatus::Available,
        )
    }

    #[test]
    fn native_basement_mode_uses_unit_header_gate_not_builds_path() {
        assert_eq!(
            basement_draft(Some(UNIT_DAT_BAS_PLACEMENT_FLAG), 0).basement_placement_mode(),
            Some(BasementPlacementMode::FlattenInnerAndAdjustRoot)
        );
        assert_eq!(
            basement_draft(Some(UNIT_DAT_BAS_PLACEMENT_FLAG), 1).basement_placement_mode(),
            Some(BasementPlacementMode::PreserveAuthoredInnerHeight)
        );
        assert_eq!(basement_draft(Some(0), 0).basement_placement_mode(), None);
        assert_eq!(basement_draft(None, 1).basement_placement_mode(), None);
    }

    #[test]
    fn native_basement_delta_adjusts_only_flattened_root_once() {
        let flatten = basement_draft(Some(UNIT_DAT_BAS_PLACEMENT_FLAG), 0);
        let root = flatten.root_position_with_basement_delta(3.25);
        assert_eq!(root, [12.0, 34.0, 96.75]);
        // A component remains local to the root; it receives the root shift
        // through composition instead of applying the delta itself.
        let child_local_z = 8.0;
        assert_eq!(root[2] + child_local_z, 104.75);

        let preserve = basement_draft(Some(UNIT_DAT_BAS_PLACEMENT_FLAG), 0x40);
        assert_eq!(
            preserve.root_position_with_basement_delta(3.25),
            preserve.position
        );
        let no_gate = basement_draft(Some(0), 0);
        assert_eq!(
            no_gate.root_position_with_basement_delta(3.25),
            no_gate.position
        );
    }

    #[test]
    fn behavior_selector_requires_an_exact_visible_clan_slot() {
        let sparse_slots = [1, 63];
        assert_eq!(behavior_clan_for_selector(1, 64, &sparse_slots), Some(1));
        assert_eq!(behavior_clan_for_selector(63, 64, &[63]), Some(63));
        assert_eq!(behavior_clan_for_selector(0, 64, &[1, 63]), None);
        assert_eq!(behavior_clan_for_selector(64, 65, &[64]), None);
        assert_eq!(behavior_clan_for_selector(0xfffe, 65_535, &[0xfffe]), None);
        assert_eq!(behavior_clan_for_selector(63, 63, &[63]), None);
    }

    #[test]
    fn behavior_registration_uses_the_class_high_bit() {
        assert!(has_behavior_registration_key(0x8000_0010));
        assert!(has_behavior_registration_key(0x8000_0002));
        assert!(has_behavior_registration_key(0x8000_0123));
        assert!(!has_behavior_registration_key(0x0000_0123));
        assert!(!has_behavior_registration_key(0x7fff_ffff));
    }

    #[test]
    fn behavior_warm_start_uses_native_clock_and_shared_rng_vectors() {
        let mut rng_state = 1;
        let first = BehaviorWarmStartState::start(100_000, &mut rng_state);
        assert_eq!(first.first_time_ms, 100_000);
        assert_eq!(first.next_deadline_ms, 100_202);
        assert_eq!(first.cleared_words, [0; 5]);
        assert_eq!(rng_state, 0x0029_e2c0);

        let second = BehaviorWarmStartState::start(100_500, &mut rng_state);
        assert_eq!(second.first_time_ms, 100_500);
        assert_eq!(second.next_deadline_ms, 100_700);
        assert_eq!(second.cleared_words, [0; 5]);
        assert_eq!(rng_state, 0xc823_f683);

        let zero = BehaviorWarmStartState::start(0, &mut rng_state);
        assert_eq!(zero.first_time_ms, 0);
        assert_eq!(zero.next_deadline_ms, 239);
        assert_eq!(rng_state, 0x18be_873a);

        // A second fresh AI with a zero queue clock receives its own LCG draw.
        let second_zero_clock_start = BehaviorWarmStartState::start(0, &mut rng_state);
        assert_eq!(second_zero_clock_start.first_time_ms, 0);
        assert_eq!(second_zero_clock_start.next_deadline_ms, 225);
        assert_eq!(rng_state, 0xe784_7115);

        let wrapping = BehaviorWarmStartState::start(0xffff_ff80, &mut rng_state);
        assert_eq!(wrapping.next_deadline_ms, 120);
        assert_eq!(rng_state, 0xcae1_df84);
    }

    #[test]
    fn mission_script_start_stops_before_warming_later_bundles() {
        fn push_u32(bytes: &mut Vec<u8>, value: u32) {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        fn push_lp(bytes: &mut Vec<u8>, value: &[u8]) {
            push_u32(bytes, u32::try_from(value.len()).expect("length fits u32"));
            bytes.extend_from_slice(value);
        }

        let mut tma = Vec::new();
        push_u32(&mut tma, 1); // TMA format version.
        push_u32(&mut tma, 0); // Paths.
        push_u32(&mut tma, 6); // Clan section version.
        push_u32(&mut tma, 2); // Two clans provide Handler(19) anchors.
        for (x, y) in [(500.0_f32, 752.0_f32), (728.0_f32, 449.0_f32)] {
            push_lp(&mut tma, b"");
            push_u32(&mut tma, u32::MAX); // Raw clan id -1.
            tma.extend_from_slice(&x.to_le_bytes());
            tma.extend_from_slice(&y.to_le_bytes());
            push_u32(&mut tma, 1); // Standard clan body.
            push_lp(&mut tma, b"");
            push_u32(&mut tma, 0);
            push_lp(&mut tma, b"");
            push_u32(&mut tma, 0);
            push_u32(&mut tma, 0); // Relations.
        }
        push_u32(&mut tma, 10); // Object section version.
        push_u32(&mut tma, 0); // Objects.
        push_lp(&mut tma, b""); // Landscape path.
        push_u32(&mut tma, 0); // Mission flag.
        push_lp(&mut tma, b""); // Description.
        push_u32(&mut tma, 1); // Extra section version.
        push_u32(&mut tma, 0); // Extras.
        let mission = decode_mission_payload(Arc::from(tma), TmaProfile::Strict)
            .expect("two-clan startup fixture");

        let varset = MissionScriptVarSet {
            path: FALLBACK_SCRIPT_VARSET_PATH.to_string(),
            used_fallback: true,
            declarations: fparkan_script::parse_varset(
                b"VAR( DWORD, ClanBaseX, 0)\nVAR( DWORD, ClanBaseY, 0)\nVAR( DWORD, ClanID, 0)\n",
            )
            .expect("varset"),
        };
        let bundle = |clan_index, selector, references: Vec<u32>| MissionScriptBundle {
            clan_index,
            base_path: "MISSIONS/SCRIPTS/default".to_string(),
            script_path: "MISSIONS/SCRIPTS/default.scr".to_string(),
            package: ScriptPackage {
                opcode_handler_count: 73,
                events: vec![fparkan_script::ScriptEvent {
                    name_len: 4,
                    name_raw: b"Init\0".to_vec(),
                    event_word: 0,
                    instructions: vec![ScriptInstruction {
                        header_words: [selector, u32::MAX, u32::MAX, 0, 0, 3, 0],
                        references,
                    }],
                }],
                trailing_bytes: Vec::new(),
                raw: Arc::from(&b""[..]),
            },
            init_execution: MissionScriptInitExecution::Missing,
            behavior_warm_start: None,
        };
        let mut scripts = LoadedMissionScripts {
            bundles: vec![bundle(0, 18, Vec::new()), bundle(1, 19, vec![0, 1, 2])],
            varset: Some(varset),
            ..LoadedMissionScripts::default()
        };
        let mut rng_state = INITIAL_BEHAVIOR_RNG_STATE;

        let error = start_mission_scripts(&mission, &mut scripts, 100_000, &mut rng_state)
            .expect_err("unsupported first Init stops startup before the second bundle");

        assert!(matches!(
            error,
            EngineError::ScriptInitUnsupported {
                source: Handler19ResolveError::UnsupportedSelector {
                    found: fparkan_script::ScriptDispatchSelector::Handler(18),
                },
                ..
            }
        ));
        assert_eq!(
            scripts.bundles[0].behavior_warm_start,
            Some(BehaviorWarmStartState {
                first_time_ms: 100_000,
                next_deadline_ms: 100_202,
                cleared_words: [0; 5],
            })
        );
        assert!(scripts.bundles[1].behavior_warm_start.is_none());
        assert_eq!(rng_state, 0x0029_e2c0);
    }

    #[test]
    fn load_mission_requires_vfs_and_keeps_world_unchanged_on_error() {
        let mut engine = create(EngineServices::default()).expect("engine");
        let before = step_headless(&mut engine, InputSnapshot).expect("step");
        let err = load_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/Autodemo.00/data.tma".to_string(),
                mode: MissionMode::SingleMission,
            },
            0,
        )
        .expect_err("missing VFS");
        assert!(matches!(err, EngineError::MissingVfs));
        let after = step_headless(&mut engine, InputSnapshot).expect("step");
        assert_eq!(before.snapshot.objects, after.snapshot.objects);
    }

    #[test]
    fn into_table_target_uses_native_wrapping_and_append_sentinel() {
        let no_parent_devices = [];
        let local_one = native_into_table_target_index(&no_parent_devices, 9, 1);
        assert_eq!(local_one, 0);
        assert_eq!(
            native_control_target_action(local_one),
            ControlTableTarget::Override(0)
        );

        let local_zero = native_into_table_target_index(&no_parent_devices, 9, 0);
        assert_eq!(local_zero, u32::MAX);
        assert_eq!(
            native_control_target_action(local_zero),
            ControlTableTarget::Append
        );
    }

    #[test]
    fn into_missing_parent_append_maps_only_zero_key_to_native_minus_one() {
        assert_eq!(native_into_append_key(0), u32::MAX);
        assert_eq!(native_into_append_key(0x1234), 0x1234);
    }

    #[test]
    fn exto_key_mapping_obeys_native_component_and_mount_sentinels() {
        assert_eq!(native_ex_to_append_key(2, 0, None, Some(20)), Some(2));
        assert_eq!(
            native_ex_to_append_key(2, 1, Some(u32::MAX), Some(20)),
            Some(2)
        );
        assert_eq!(
            native_ex_to_append_key(0, 1, Some(u32::MAX), None),
            Some(u32::MAX)
        );
        assert_eq!(native_ex_to_append_key(2, 1, Some(4), Some(10)), Some(11));
        assert_eq!(native_ex_to_append_key(2, 1, Some(4), None), Some(0));
        assert_eq!(native_ex_to_append_key(1, 5, Some(9), Some(22)), Some(22));
    }

    #[test]
    fn synthetic_ex_to_builder_appends_remapped_rows_and_skips_inactive_subtree() {
        let controls = vec![
            Some(synthetic_component_control(
                0,
                vec![synthetic_control_record(19, 7, 0x10, None)],
                PreparedControlEffectSection::Parsed(Vec::new()),
            )),
            Some(synthetic_component_control(
                1,
                vec![
                    synthetic_control_record(19, 0, 0x20, None),
                    synthetic_control_record(19, 2, 0x21, None),
                ],
                PreparedControlEffectSection::Parsed(Vec::new()),
            )),
            Some(synthetic_component_control(
                2,
                vec![
                    synthetic_control_record(19, 0, 0x30, None),
                    synthetic_control_record(19, 2, 0x31, None),
                ],
                PreparedControlEffectSection::Parsed(Vec::new()),
            )),
            Some(synthetic_component_control(
                3,
                vec![synthetic_control_record(19, 9, 0x40, None)],
                PreparedControlEffectSection::Parsed(Vec::new()),
            )),
            Some(synthetic_component_control(
                4,
                vec![synthetic_control_record(19, 10, 0x50, None)],
                PreparedControlEffectSection::Parsed(Vec::new()),
            )),
            Some(synthetic_component_control(
                5,
                vec![synthetic_control_record(19, 2, 0x60, None)],
                PreparedControlEffectSection::Parsed(Vec::new()),
            )),
        ];
        let status = synthetic_control_table(
            &[
                Some(PROTOTYPE_TYPE_EXTO),
                Some(PROTOTYPE_TYPE_EXTO),
                Some(PROTOTYPE_TYPE_EXTO),
                None,
                None,
                Some(PROTOTYPE_TYPE_EXTO),
            ],
            &[1, 1, 1, 0, 1, 1],
            &[3, 1, 0, 1, 0, 0],
            &[0, 3, 2, 0, 0, 4],
            &[5, 3, 2, 1, 1, 2],
            controls,
        );
        let MissionControlDeviceTableStatus::Available(entries) = status else {
            panic!("synthetic EXTO table should be available: {status:?}");
        };
        assert_eq!(entries.len(), 6);
        // Root EXTO keys remain raw. Child keys map to mount/global nodes.
        assert_eq!(
            entries.iter().map(|entry| entry.key).collect::<Vec<_>>(),
            [7, 3, 6, 6, 8, 9]
        );
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.component_id)
                .collect::<Vec<_>>(),
            [0, 1, 1, 2, 2, 3]
        );
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.descriptor_provenance.unit_component_index)
                .collect::<Vec<_>>(),
            [0, 1, 1, 2, 2, 5]
        );
        assert_ne!(
            entries[5].component_id as usize,
            entries[5].descriptor_provenance.unit_component_index
        );
    }

    #[test]
    fn synthetic_into_builder_appends_all_rows_and_overrides_only_first_record() {
        let append = synthetic_control_table(
            &[None, Some(PROTOTYPE_TYPE_INTO)],
            &[1, 1],
            &[1, 0],
            &[0, 0],
            &[1, 0],
            vec![
                None,
                Some(synthetic_component_control(
                    1,
                    vec![
                        synthetic_control_record(19, 0, 0x10, None),
                        synthetic_control_record(19, 2, 0x20, None),
                    ],
                    PreparedControlEffectSection::Parsed(Vec::new()),
                )),
            ],
        );
        let MissionControlDeviceTableStatus::Available(appended) = append else {
            panic!("synthetic INTO sentinel should append: {append:?}");
        };
        assert_eq!(appended.len(), 2);
        assert_eq!(
            appended.iter().map(|entry| entry.key).collect::<Vec<_>>(),
            [u32::MAX, 2]
        );
        assert_eq!(
            appended
                .iter()
                .map(|entry| entry.descriptor_provenance.typed_record_index)
                .collect::<Vec<_>>(),
            [0, 1]
        );

        let override_table = synthetic_control_table(
            &[None, Some(PROTOTYPE_TYPE_INTO), Some(PROTOTYPE_TYPE_INTO)],
            &[1, 1, 1],
            &[2, 0, 0],
            &[0, 0, 1],
            &[1, 0, 0],
            vec![
                Some(synthetic_component_control(
                    0,
                    vec![
                        synthetic_control_record(19, 11, 0x10, None),
                        synthetic_control_record(19, 12, 0x20, None),
                    ],
                    PreparedControlEffectSection::Parsed(Vec::new()),
                )),
                Some(synthetic_component_control(
                    1,
                    vec![
                        synthetic_control_record(21, 99, 0xA1, Some(0x77)),
                        synthetic_control_record(25, 88, 0xA2, Some(0x88)),
                    ],
                    PreparedControlEffectSection::Parsed(Vec::new()),
                )),
                Some(synthetic_component_control(
                    2,
                    vec![synthetic_control_record(21, 77, 0xB1, None)],
                    PreparedControlEffectSection::Parsed(Vec::new()),
                )),
            ],
        );
        let MissionControlDeviceTableStatus::Available(overridden) = override_table else {
            panic!("synthetic INTO overrides should be available: {override_table:?}");
        };
        assert_eq!(overridden.len(), 2);
        assert_eq!(
            overridden
                .iter()
                .map(|entry| (entry.factory_tag, entry.key, entry.component_id))
                .collect::<Vec<_>>(),
            [(19, 11, 0), (19, 12, 0)]
        );
        assert_eq!(
            overridden
                .iter()
                .map(|entry| entry.coefficient_bits.map(|words| words[0]))
                .collect::<Vec<_>>(),
            [Some(0xA1), Some(0xB1)]
        );
        assert_eq!(
            overridden
                .iter()
                .map(|entry| entry.native_word_50_bits)
                .collect::<Vec<_>>(),
            [Some(0x77), Some(0x20)]
        );
        assert_eq!(
            overridden
                .iter()
                .map(|entry| entry.multiplier_bits)
                .collect::<Vec<_>>(),
            [Some(0x3F80_0000); 2]
        );
        assert_eq!(
            overridden
                .iter()
                .map(|entry| entry.descriptor_provenance.unit_component_index)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(
            overridden
                .iter()
                .map(|entry| entry.descriptor_provenance.typed_record_index)
                .collect::<Vec<_>>(),
            [0, 0]
        );
        assert_eq!(overridden[0].descriptor.tag, 21);

        let mut live_device = overridden[0].clone();
        live_device.multiplier_bits = Some(0x4020_0000); // Live native multiplier 2.5.
        let replacement = synthetic_control_record(25, 100, 0xC1, None);
        let replacement_control = synthetic_component_control(
            3,
            vec![replacement.clone()],
            PreparedControlEffectSection::Parsed(Vec::new()),
        );
        apply_effective_control_override(
            &mut live_device,
            &replacement,
            3,
            0,
            &replacement_control,
        );
        assert_eq!(live_device.factory_tag, 19);
        assert_eq!(live_device.key, 11);
        assert_eq!(live_device.component_id, 0);
        assert_eq!(live_device.multiplier_bits, Some(0x4020_0000));
        assert_eq!(
            live_device.coefficient_bits,
            Some([0xC1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
        );
    }

    #[test]
    fn synthetic_incomplete_typed_list_does_not_expose_partial_table() {
        let status = synthetic_control_table(
            &[None],
            &[1],
            &[0],
            &[0],
            &[1],
            vec![Some(synthetic_component_control(
                0,
                vec![synthetic_control_record(19, 1, 0x10, None)],
                PreparedControlEffectSection::Unsupported {
                    typed_record_index: Some(1),
                    typed_record_tag: Some(0xDEAD),
                },
            ))],
        );
        assert_eq!(
            status,
            MissionControlDeviceTableStatus::Unavailable(
                MissionControlDeviceTableUnavailableReason::IncompleteTypedRecordList {
                    unit_component_index: 0,
                    first_unparsed_record: 1,
                }
            )
        );
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn full_load_builds_effective_control_device_table_for_mine_unit_dat() {
        fn push_u32(bytes: &mut Vec<u8>, value: u32) {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        fn push_lp(bytes: &mut Vec<u8>, value: &[u8]) {
            push_u32(bytes, u32::try_from(value.len()).expect("length fits u32"));
            bytes.extend_from_slice(value);
        }
        fn single_unit_tma(land_path: &[u8]) -> Vec<u8> {
            let mut bytes = Vec::new();
            push_u32(&mut bytes, 1); // TMA format version.
            push_u32(&mut bytes, 0); // Paths.
            push_u32(&mut bytes, 6); // Clan section version.
            push_u32(&mut bytes, 0); // Clans.
            push_u32(&mut bytes, 10); // Object section version.
            push_u32(&mut bytes, 1); // One Unit DAT object.
            push_u32(&mut bytes, 1); // Raw object kind.
            push_u32(&mut bytes, 0x8000_0002); // Strict object class/flags word.
            push_lp(&mut bytes, b"UNITS/BUILDS/MINE/min_l_n1.dat");
            push_u32(&mut bytes, 0); // Raw word after resource.
            push_u32(&mut bytes, 0); // Identity/clan.
            for value in [0.0_f32, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0] {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            push_lp(&mut bytes, b"mine-control-table-test");
            push_u32(&mut bytes, 0); // Raw word after name.
            push_u32(&mut bytes, 0); // Link 0.
            push_u32(&mut bytes, 0); // Link 1.
            push_u32(&mut bytes, 1); // Property schema version.
            push_u32(&mut bytes, 0); // Properties.
            push_lp(&mut bytes, land_path);
            push_u32(&mut bytes, 0); // Mission flag.
            push_lp(&mut bytes, b""); // Description.
            push_u32(&mut bytes, 1); // Extra section version.
            push_u32(&mut bytes, 0); // Extras.
            bytes
        }

        let root = licensed_root("IS");
        let source_tma = std::fs::read(root.join("MISSIONS/Autodemo.00/data.tma"))
            .expect("licensed source mission");
        let land_path = decode_mission_land_path(&source_tma, TmaProfile::Strict)
            .expect("source mission land path")
            .raw;
        let mut overlay = MemoryVfs::default();
        overlay.insert(
            normalize_engine_path("test mission", "MISSIONS/TEST/MINE/data.tma")
                .expect("mission path"),
            Arc::from(single_unit_tma(&land_path)),
        );
        let vfs = Arc::new(OverlayVfs::from_layers(vec![
            Arc::new(overlay),
            Arc::new(DirectoryVfs::new(&root)),
        ]));
        let mut engine = create(EngineServices::new(vfs)).expect("engine");
        let loaded = load_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/TEST/MINE/data.tma".to_string(),
                mode: MissionMode::SingleMission,
            },
            0,
        )
        .expect("full normal mission load");
        assert_eq!(loaded.object_count, 1);

        let draft = &loaded_mission_object_drafts(&engine).expect("object drafts")[0];
        assert_eq!(draft.unit_components.len(), 6);
        assert_eq!(draft.unit_components[0].immediate_child_count, 5);
        let tables = loaded_mission_control_device_tables(&engine).expect("Control tables");
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].original_id, Some(OriginalObjectId(0)));
        let MissionControlDeviceTableStatus::Available(entries) = &tables[0].status else {
            panic!(
                "licensed mine table should be available: {:?}",
                tables[0].status
            );
        };
        assert_eq!(entries.len(), 11);
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.factory_tag)
                .collect::<Vec<_>>(),
            [19, 19, 9, 15, 12, 12, 13, 13, 26, 25, 21]
        );
        assert_eq!(
            entries.iter().map(|entry| entry.key).collect::<Vec<_>>(),
            [2, 2, 1, 1, 18, 21, 15, 16, 2, 1, 22]
        );
        assert_eq!(
            (
                entries[0].coefficient_bits.map(|words| words[0]),
                entries[1].coefficient_bits.map(|words| words[0])
            ),
            (Some(0x4100_0000), Some(0x4100_0000))
        );
        assert_eq!(
            (entries[0].multiplier_bits, entries[0].native_word_50_bits),
            (Some(0x3f80_0000), Some(0x20))
        );
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.descriptor_provenance.unit_component_index)
                .collect::<Vec<_>>(),
            [1, 2, 3, 4, 0, 0, 0, 0, 0, 0, 5]
        );
        assert_eq!(entries[8].descriptor_provenance.typed_record_index, 8);
        assert_eq!(
            (
                entries[9].factory_tag,
                entries[9].component_id,
                entries[9].key
            ),
            (25, 0, 1)
        );
        assert_eq!(entries[9].descriptor.tag, 25);
        assert_eq!(
            (
                entries[10].factory_tag,
                entries[10].component_id,
                entries[10].key
            ),
            (21, 5, 22)
        );
        assert_eq!(entries[10].descriptor.tag, 21);
    }

    #[test]
    fn script_varset_prefers_bundle_adjacent_file_then_uses_shared_fallback() {
        let mut personal_files = MemoryVfs::default();
        personal_files.insert(
            normalize_engine_path("test", "MISSIONS/SCRIPTS/custom.var").expect("path"),
            Arc::from(&b"VAR( DWORD, personal, 7)\n"[..]),
        );
        personal_files.insert(
            normalize_engine_path("test", FALLBACK_SCRIPT_VARSET_PATH).expect("path"),
            Arc::from(&b"VAR( DWORD, fallback, 8)\n"[..]),
        );
        let personal_vfs: Arc<dyn Vfs> = Arc::new(personal_files);
        let personal =
            load_script_varset(&personal_vfs, "MISSIONS/SCRIPTS/custom").expect("personal varset");
        assert_eq!(personal.path, "MISSIONS/SCRIPTS/custom.var");
        assert!(!personal.used_fallback);
        assert_eq!(personal.declarations.declarations[0].name, "personal");

        let mut fallback_files = MemoryVfs::default();
        fallback_files.insert(
            normalize_engine_path("test", FALLBACK_SCRIPT_VARSET_PATH).expect("path"),
            Arc::from(&b"VAR( float, fallback, 0.5)\n"[..]),
        );
        let fallback_vfs: Arc<dyn Vfs> = Arc::new(fallback_files);
        let fallback =
            load_script_varset(&fallback_vfs, "MISSIONS/SCRIPTS/custom").expect("fallback varset");
        assert_eq!(fallback.path, FALLBACK_SCRIPT_VARSET_PATH);
        assert!(fallback.used_fallback);
        assert_eq!(fallback.declarations.declarations[0].name, "fallback");
    }

    #[test]
    fn malformed_personal_script_varset_does_not_silently_use_fallback() {
        let mut files = MemoryVfs::default();
        files.insert(
            normalize_engine_path("test", "MISSIONS/SCRIPTS/custom.var").expect("path"),
            Arc::from(&b"VAR( DWORD, broken, nope)\n"[..]),
        );
        files.insert(
            normalize_engine_path("test", FALLBACK_SCRIPT_VARSET_PATH).expect("path"),
            Arc::from(&b"VAR( DWORD, fallback, 8)\n"[..]),
        );
        let vfs: Arc<dyn Vfs> = Arc::new(files);
        assert!(matches!(
            load_script_varset(&vfs, "MISSIONS/SCRIPTS/custom"),
            Err(EngineError::VarSet { path, .. }) if path == "MISSIONS/SCRIPTS/custom.var"
        ));
    }

    #[test]
    fn handler_nineteen_init_executes_from_clan_anchor_for_each_bundle() {
        let declarations = fparkan_script::parse_varset(
            b"VAR( DWORD, ClanBaseX, 950)\nVAR( DWORD, ClanBaseY, 1000)\nVAR( DWORD, ClanID, 0)\n",
        )
        .expect("varset");
        let varset = MissionScriptVarSet {
            path: FALLBACK_SCRIPT_VARSET_PATH.to_string(),
            used_fallback: true,
            declarations,
        };
        let mut bundles = [MissionScriptBundle {
            clan_index: 1,
            base_path: "MISSIONS/SCRIPTS/default".to_string(),
            script_path: "MISSIONS/SCRIPTS/default.scr".to_string(),
            package: ScriptPackage {
                opcode_handler_count: 73,
                events: vec![fparkan_script::ScriptEvent {
                    name_len: 4,
                    name_raw: b"Init\0".to_vec(),
                    event_word: 0x1234_5678,
                    instructions: vec![fparkan_script::ScriptInstruction {
                        header_words: [19, u32::MAX, u32::MAX, 0, 0, 3, 0],
                        references: vec![0, 1, 2],
                    }],
                }],
                trailing_bytes: Vec::new(),
                raw: Arc::from(&b""[..]),
            },
            init_execution: MissionScriptInitExecution::Missing,
            behavior_warm_start: None,
        }];

        let states = resolve_handler19_init_states(
            &[[500.0, 752.0], [728.0, 449.0]],
            &mut bundles,
            &varset,
            MissionAssetScope::Full,
        )
        .expect("exact Init execution");
        assert_eq!(
            states,
            vec![MissionScriptInitState {
                clan_index: 1,
                event_index: 0,
                event_word: 0x1234_5678,
                writes: vec![
                    Handler19DwordWrite {
                        index: 0,
                        value: 728
                    },
                    Handler19DwordWrite {
                        index: 1,
                        value: 449
                    },
                    Handler19DwordWrite { index: 2, value: 1 },
                ],
            }]
        );
        let runtime_varsets = materialize_script_varset_states(&bundles, &varset, &states)
            .expect("materialized per-clan varset");
        assert_eq!(
            runtime_varsets,
            vec![MissionScriptVarSetState {
                clan_index: 1,
                values: vec![
                    VarSetDefault::Dword(728),
                    VarSetDefault::Dword(449),
                    VarSetDefault::Dword(1),
                ],
            }]
        );
        let callback = resolve_handler30_from_varset_state(
            &varset,
            &runtime_varsets[0],
            &ScriptInstruction {
                header_words: [30, 0, 0, 0, 0, 2, 0],
                references: vec![0, 2],
            },
        )
        .expect("runtime Handler(30) callback");
        assert_eq!(
            callback,
            VmHostCallbackCommand {
                mode: 0,
                first: 728,
                second: 1,
            }
        );
    }

    #[test]
    fn handler_nineteen_preview_keeps_unsupported_package_without_partial_writes() {
        let varset = MissionScriptVarSet {
            path: FALLBACK_SCRIPT_VARSET_PATH.to_string(),
            used_fallback: true,
            declarations: fparkan_script::parse_varset(
                b"VAR( DWORD, x, 950)\nVAR( DWORD, y, 1000)\nVAR( DWORD, id, 0)\n",
            )
            .expect("varset"),
        };
        let package = ScriptPackage {
            opcode_handler_count: 73,
            events: vec![fparkan_script::ScriptEvent {
                name_len: 4,
                name_raw: b"Init\0".to_vec(),
                event_word: 0x1234,
                instructions: vec![
                    fparkan_script::ScriptInstruction {
                        header_words: [19, u32::MAX, u32::MAX, 0, 0, 3, 0],
                        references: vec![0, 1, 2],
                    },
                    fparkan_script::ScriptInstruction {
                        header_words: [18, u32::MAX, u32::MAX, 0, 0, 0, 0],
                        references: Vec::new(),
                    },
                ],
            }],
            trailing_bytes: Vec::new(),
            raw: Arc::from(&b""[..]),
        };
        let bundle = || MissionScriptBundle {
            clan_index: 0,
            base_path: "MISSIONS/SCRIPTS/default".to_string(),
            script_path: "MISSIONS/SCRIPTS/default.scr".to_string(),
            package: package.clone(),
            init_execution: MissionScriptInitExecution::Missing,
            behavior_warm_start: None,
        };
        let anchors = [[500.0, 752.0]];
        let mut preview_bundles = vec![bundle()];
        let preview_states = resolve_handler19_init_states(
            &anchors,
            &mut preview_bundles,
            &varset,
            MissionAssetScope::FirstMeshPreview,
        )
        .expect("static preview retains unsupported script");
        assert!(preview_states.is_empty(), "no partial writes are retained");
        assert_eq!(
            preview_bundles[0].init_execution,
            MissionScriptInitExecution::Unsupported {
                event_index: 0,
                instruction_index: 1,
                error: Handler19ResolveError::UnsupportedSelector {
                    found: ScriptDispatchSelector::Handler(18),
                },
            }
        );
        assert_eq!(
            materialize_script_varset_states(&preview_bundles, &varset, &preview_states)
                .expect("preview keeps default varset values"),
            vec![MissionScriptVarSetState {
                clan_index: 0,
                values: vec![
                    VarSetDefault::Dword(950),
                    VarSetDefault::Dword(1000),
                    VarSetDefault::Dword(0),
                ],
            }]
        );
        assert_eq!(preview_bundles[0].package, package);

        let mut full_bundles = vec![bundle()];
        assert!(matches!(
            resolve_handler19_init_states(
                &anchors,
                &mut full_bundles,
                &varset,
                MissionAssetScope::Full,
            ),
            Err(EngineError::ScriptInitUnsupported {
                clan_index: 0,
                event_index: 0,
                instruction_index: 1,
                source: Handler19ResolveError::UnsupportedSelector {
                    found: ScriptDispatchSelector::Handler(18),
                },
                ..
            })
        ));

        let mut malformed_package = package.clone();
        malformed_package.events[0].instructions[0]
            .references
            .clear();
        let mut malformed_bundle = vec![MissionScriptBundle {
            package: malformed_package,
            ..bundle()
        }];
        assert!(matches!(
            resolve_handler19_init_states(
                &anchors,
                &mut malformed_bundle,
                &varset,
                MissionAssetScope::FirstMeshPreview,
            ),
            Err(EngineError::ScriptInit { message, .. })
                if message.contains("Handler(19) is missing reference 0")
        ));
    }

    #[test]
    fn handler_nineteen_init_matches_original_x87_rounding_and_dword_path() {
        for (value, expected) in [
            (500.2857, 500),
            (751.8217, 752),
            (500.5, 500),
            (501.5, 502),
            (-0.5, 0),
            (-0.4, 0),
            (-0.6, 0),
            (10_000.4, 10_000),
            (10_000.5, 10_000),
            (10_000.6, 10_001),
            (10_001.0, 10_001),
            (4_294_967_296.0, 0),
            (u32::MAX as f32, 0),
            (4_294_967_808.0, 512),
        ] {
            assert_eq!(
                handler19_anchor_word(value, 0, "x").expect("supported anchor"),
                expected,
                "anchor {value:?}"
            );
        }
        assert_eq!(u32::MAX as f32, 4_294_967_296.0);
    }

    #[test]
    fn handler_nineteen_init_rejects_unmodeled_x87_qword_conversions() {
        let qword_limit = 9_223_372_036_854_775_808.0_f32;
        assert_eq!(
            handler19_anchor_word(-qword_limit, 0, "x").expect("valid signed qword minimum"),
            0,
            "the signed qword minimum is representable before low-DWORD conversion"
        );
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, qword_limit] {
            let error =
                handler19_anchor_word(value, 0, "x").expect_err("unmodeled FISTP qword input");
            assert!(matches!(
                error,
                EngineError::ScriptInit { clan_index: 0, message }
                    if message.contains("outside the modeled x87 FISTP qword range")
            ));
        }
    }

    #[test]
    fn handler_thirty_host_callback_obeys_native_mode_and_status_guards() {
        fn command(second: u32) -> VmHostCallbackCommand {
            VmHostCallbackCommand {
                mode: 0,
                first: 0,
                second,
            }
        }

        let mut status = 5;
        let mut outcome = MissionScriptOutcome::Active;
        assert_eq!(
            apply_recovered_script_host_callback(
                MissionMode::SingleMission,
                &mut status,
                &mut outcome,
                command(0),
            ),
            MissionScriptHostEffect::MissionFailed
        );
        assert_eq!((status, outcome), (1, MissionScriptOutcome::Failed));
        outcome = MissionScriptOutcome::Completed;
        assert_eq!(
            apply_recovered_script_host_callback(
                MissionMode::SingleMission,
                &mut status,
                &mut outcome,
                command(0),
            ),
            MissionScriptHostEffect::NoChange
        );
        assert_eq!((status, outcome), (1, MissionScriptOutcome::Completed));
        outcome = MissionScriptOutcome::Failed;

        assert_eq!(
            apply_recovered_script_host_callback(
                MissionMode::SingleMission,
                &mut status,
                &mut outcome,
                command(1),
            ),
            MissionScriptHostEffect::MissionCompleted
        );
        assert_eq!((status, outcome), (0, MissionScriptOutcome::Completed));
        assert_eq!(
            apply_recovered_script_host_callback(
                MissionMode::SingleMission,
                &mut status,
                &mut outcome,
                command(1),
            ),
            MissionScriptHostEffect::NoChange
        );
        assert_eq!((status, outcome), (0, MissionScriptOutcome::Completed));

        assert_eq!(
            apply_recovered_script_host_callback(
                MissionMode::SingleMission,
                &mut status,
                &mut outcome,
                command(0),
            ),
            MissionScriptHostEffect::MissionFailed
        );
        assert_eq!((status, outcome), (1, MissionScriptOutcome::Failed));

        status = 4;
        outcome = MissionScriptOutcome::Active;
        assert_eq!(
            apply_recovered_script_host_callback(
                MissionMode::SingleMission,
                &mut status,
                &mut outcome,
                command(1),
            ),
            MissionScriptHostEffect::MissionCompleted
        );
        assert_eq!((status, outcome), (0, MissionScriptOutcome::Completed));

        status = 5;
        outcome = MissionScriptOutcome::Active;
        assert_eq!(
            apply_recovered_script_host_callback(
                MissionMode::AutoDemo,
                &mut status,
                &mut outcome,
                command(0),
            ),
            MissionScriptHostEffect::Unhandled
        );
        assert_eq!((status, outcome), (5, MissionScriptOutcome::Active));

        for command in [
            VmHostCallbackCommand {
                mode: 1,
                first: 0,
                second: 0,
            },
            VmHostCallbackCommand {
                mode: 0,
                first: 3,
                second: 0,
            },
            VmHostCallbackCommand {
                mode: 0,
                first: 0,
                second: 42,
            },
        ] {
            assert_eq!(
                apply_recovered_script_host_callback(
                    MissionMode::SingleMission,
                    &mut status,
                    &mut outcome,
                    command,
                ),
                MissionScriptHostEffect::Unhandled
            );
            assert_eq!((status, outcome), (5, MissionScriptOutcome::Active));
        }
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_autodemo_load_resolves_native_handler19_init_for_both_clans() {
        let root = licensed_root("IS");
        let vfs: Arc<dyn Vfs> = Arc::new(DirectoryVfs::new(root));
        let mut engine = create(EngineServices::new(vfs)).expect("engine");
        let loaded = load_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/Autodemo.00/data.tma".to_string(),
                mode: MissionMode::AutoDemo,
            },
            100_000,
        )
        .expect("load licensed AutoDemo");

        let mission = loaded_mission_document(&engine).expect("mission document");
        let drafts = loaded_mission_object_drafts(&engine).expect("object drafts");
        assert_eq!(drafts.len(), mission.objects.len());
        assert!(mission
            .objects
            .iter()
            .any(|object| object.raw_after_resource != object.identity_or_clan_raw));
        for (object, draft) in mission.objects.iter().zip(drafts) {
            assert_eq!(draft.raw_after_resource, object.raw_after_resource);
            assert_eq!(draft.identity_or_clan_raw, object.identity_or_clan_raw);
        }

        let distributors =
            loaded_mission_behavior_distributors(&engine).expect("Behavior distributors");
        assert_eq!(
            distributors
                .iter()
                .map(|distributor| (distributor.clan_index, distributor.super_ai_visible))
                .collect::<Vec<_>>(),
            vec![(0, true), (1, true)]
        );
        let ore_states = loaded_mission_behavior_ore_states(&engine).expect("Behavior ore state");
        let unit_dat_drafts = drafts
            .iter()
            .filter(|draft| draft.unit_dat_header_flags.is_some())
            .collect::<Vec<_>>();
        assert_eq!(ore_states.len(), unit_dat_drafts.len());
        assert_eq!(ore_states.len(), 8);
        for state in ore_states {
            let draft = unit_dat_drafts
                .iter()
                .find(|draft| draft.original_id == Some(state.original_id))
                .expect("ore state has a Unit DAT source");
            assert_eq!(
                state.handle,
                handle_by_original_id(&engine.world, state.original_id).unwrap()
            );
            assert_eq!(state.class_flags, draft.unit_dat_header_flags.unwrap());
            assert_eq!(state.current_ore_bits, 0);
            assert_eq!(state.maximum_ore_bits, 0);
        }
        let registered = distributors
            .iter()
            .flat_map(|distributor| {
                distributor
                    .registered_objects
                    .iter()
                    .map(move |object| (distributor.clan_index, object))
            })
            .collect::<Vec<_>>();
        assert_eq!(
            registered
                .iter()
                .map(|(clan_index, object)| {
                    let draft = drafts
                        .iter()
                        .find(|draft| draft.original_id == Some(object.original_id))
                        .expect("registered object draft");
                    (
                        *clan_index,
                        object.original_id,
                        draft.resource_name_raw.as_slice(),
                        object.selector,
                        object.class_flags,
                        object.share.to_bits(),
                        object.native_tail,
                    )
                })
                .collect::<Vec<_>>(),
            vec![
                (
                    0,
                    OriginalObjectId(4),
                    &b"UNITS\\BUILDS\\PLANT\\wor_l_n1.dat"[..],
                    0,
                    0x8000_0010,
                    0x3f00_0000,
                    [0; 5],
                ),
                (
                    0,
                    OriginalObjectId(6),
                    &b"UNITS\\BUILDS\\GENER\\gener01.dat"[..],
                    0,
                    0x8000_0002,
                    0x3f00_0000,
                    [0; 5],
                ),
                (
                    1,
                    OriginalObjectId(5),
                    &b"UNITS\\BUILDS\\PLANT\\wor_l_n1.dat"[..],
                    1,
                    0x8000_0010,
                    0x3f00_0000,
                    [0; 5],
                ),
                (
                    1,
                    OriginalObjectId(7),
                    &b"UNITS\\BUILDS\\GENER\\gener01.dat"[..],
                    1,
                    0x8000_0002,
                    0x3f00_0000,
                    [0; 5],
                ),
            ]
        );
        assert_eq!(registered.len(), 4);
        assert!(ore_states.iter().any(|state| {
            state.class_flags == 0x0100_8000
                && !distributors.iter().any(|distributor| {
                    distributor
                        .registered_objects
                        .iter()
                        .any(|object| object.handle == state.handle)
                })
        }));
        for (_, object) in registered {
            assert_eq!(
                handle_by_original_id(&engine.world, object.original_id),
                Some(object.handle)
            );
        }

        assert_eq!(
            mission
                .clans
                .iter()
                .map(|clan| clan.anchor)
                .collect::<Vec<_>>(),
            vec![[500.2857, 751.8217], [728.0, 449.0]]
        );
        assert_eq!(loaded.script_bundle_count, 2);
        assert_eq!(loaded.script_init_state_count, 2);
        assert_eq!(loaded.script_varset_state_count, 2);
        assert!(loaded_mission_script_bundles(&engine)
            .expect("script bundles")
            .iter()
            .all(|bundle| bundle.init_execution
                == MissionScriptInitExecution::Executed { event_index: 0 }));
        assert_eq!(engine.behavior_rng_state, 0xc823_f683);
        assert_eq!(
            loaded_mission_script_bundles(&engine)
                .expect("warmed script bundles")
                .iter()
                .map(|bundle| bundle
                    .behavior_warm_start
                    .expect("full startup warms the AI"))
                .map(|warm_start| warm_start.next_deadline_ms)
                .collect::<Vec<_>>(),
            vec![100_202, 100_200]
        );

        let varsets = loaded_mission_script_varset_states(&engine).expect("per-clan varsets");
        let clan_zero = varsets
            .iter()
            .find(|state| state.clan_index == 0)
            .expect("first clan varset");
        let clan_one = varsets
            .iter()
            .find(|state| state.clan_index == 1)
            .expect("second clan varset");
        assert_eq!(
            &clan_zero.values[224..227],
            &[
                VarSetDefault::Dword(500),
                VarSetDefault::Dword(752),
                VarSetDefault::Dword(0),
            ]
        );
        assert_eq!(
            &clan_one.values[224..227],
            &[
                VarSetDefault::Dword(728),
                VarSetDefault::Dword(449),
                VarSetDefault::Dword(1),
            ]
        );
    }

    fn behavior_test_object(
        slot: u32,
        share: f32,
        native_tail: [u32; 5],
    ) -> BehaviorRegisteredObject {
        BehaviorRegisteredObject {
            original_id: OriginalObjectId(slot),
            handle: ObjectHandle {
                generation: 0,
                slot,
            },
            selector: 0,
            class_flags: 0,
            share,
            native_tail,
        }
    }

    #[test]
    fn behavior_ore_initialization_uses_first_exact_current_value_and_read_only_maximum() {
        let handle = ObjectHandle {
            generation: 7,
            slot: 19,
        };
        let state = initialize_behavior_ore_state(
            OriginalObjectId(19),
            handle,
            0x0100_2000,
            &[
                MissionObjectProperty {
                    name_raw: b"currentore".to_vec(),
                    raw_value: [0x1111_1111, 0xdead_beef, 0, 0],
                },
                MissionObjectProperty {
                    name_raw: b"CurrentOre".to_vec(),
                    raw_value: [0x3f80_0000, 0x7fc1_2345, 0, 0],
                },
                MissionObjectProperty {
                    name_raw: b"CurrentOre".to_vec(),
                    raw_value: [0, 0x4400_0000, 0, 0],
                },
                MissionObjectProperty {
                    name_raw: b"MaximumOre".to_vec(),
                    raw_value: [0, 0x4123_4567, 0, 0],
                },
            ],
        );

        assert_eq!(state.original_id, OriginalObjectId(19));
        assert_eq!(state.handle, handle);
        assert_eq!(state.class_flags, 0x0100_2000);
        assert_eq!(state.current_ore_bits, 0x7fc1_2345);
        assert_eq!(state.maximum_ore_bits, 0x44fa_0000);
    }

    #[test]
    fn behavior_ore_defaults_follow_native_class_dispatch() {
        assert_eq!(native_behavior_ore_defaults(0x8000_0004), (0, 0x43fa_0000));
        assert_eq!(native_behavior_ore_defaults(0x8000_0008), (0, 0x457a_0000));
        assert_eq!(native_behavior_ore_defaults(0x0100_2000), (0, 0x44fa_0000));
        assert_eq!(
            native_behavior_ore_defaults(0x0100_4000),
            (0x44fa_0000, 0x44fa_0000)
        );
        assert_eq!(native_behavior_ore_defaults(0x0100_8000), (0, 0));
    }

    #[test]
    fn behavior_distributor_add_uses_handle_and_preserves_duplicate_tail() {
        let mut distributor = BehaviorDistributorState {
            clan_index: 0,
            super_ai_visible: true,
            registered_objects: Vec::new(),
        };

        add_behavior_registered_object(&mut distributor, behavior_test_object(4, 0.0, [0; 5]));
        add_behavior_registered_object(&mut distributor, behavior_test_object(6, 0.0, [0; 5]));
        assert_eq!(
            distributor
                .registered_objects
                .iter()
                .map(|object| object.share.to_bits())
                .collect::<Vec<_>>(),
            vec![0x3f00_0000, 0x3f00_0000]
        );

        distributor.registered_objects[0].share = 0.2;
        distributor.registered_objects[0].native_tail = [0xa1, 0xa2, 0xa3, 0xa4, 0xa5];
        add_behavior_registered_object(&mut distributor, behavior_test_object(4, 0.0, [0; 5]));
        assert_eq!(
            distributor.registered_objects[0].share.to_bits(),
            0x3f00_0000
        );
        assert_eq!(
            distributor.registered_objects[0].native_tail,
            [0xa1, 0xa2, 0xa3, 0xa4, 0xa5]
        );

        add_behavior_registered_object(&mut distributor, behavior_test_object(8, 0.0, [0; 5]));
        assert_eq!(
            distributor
                .registered_objects
                .iter()
                .map(|object| object.share.to_bits())
                .collect::<Vec<_>>(),
            vec![0x3f00_0000, 0x3e2a_aaa8, 0x3eaa_aaab]
        );
        assert_eq!(
            distributor
                .registered_objects
                .iter()
                .map(|object| object.native_tail)
                .collect::<Vec<_>>(),
            vec![[0xa1, 0xa2, 0xa3, 0xa4, 0xa5], [0; 5], [0; 5]]
        );
    }

    #[test]
    fn behavior_distributor_normalize_matches_native_24_bit_vectors() {
        let mut pinned = BehaviorDistributorState {
            clan_index: 0,
            super_ai_visible: true,
            registered_objects: vec![
                behavior_test_object(0x11, 0.2, [0; 5]),
                behavior_test_object(0x22, 0.3, [1, 0x11, 0x22, 0x33, 0x44]),
                behavior_test_object(0x33, 0.3, [0; 5]),
            ],
        };
        normalize_behavior_distributor(
            &mut pinned,
            ObjectHandle {
                generation: 0,
                slot: 0x11,
            },
        );
        assert_eq!(
            pinned
                .registered_objects
                .iter()
                .map(|object| object.share.to_bits())
                .collect::<Vec<_>>(),
            vec![0x3e4c_cccd, 0x3e99_999a, 0x3f00_0000]
        );

        let mut zero_sum = BehaviorDistributorState {
            clan_index: 0,
            super_ai_visible: true,
            registered_objects: vec![
                behavior_test_object(0x41, 0.25, [0; 5]),
                behavior_test_object(0x42, 0.0, [0; 5]),
                behavior_test_object(0x43, 0.0, [0; 5]),
            ],
        };
        normalize_behavior_distributor(
            &mut zero_sum,
            ObjectHandle {
                generation: 0,
                slot: 0x41,
            },
        );
        assert_eq!(
            zero_sum
                .registered_objects
                .iter()
                .map(|object| object.share.to_bits())
                .collect::<Vec<_>>(),
            vec![0x3e80_0000, 0x3ec0_0000, 0x3ec0_0000]
        );

        let mut near_rounding = BehaviorDistributorState {
            clan_index: 0,
            super_ai_visible: true,
            registered_objects: vec![
                behavior_test_object(0x51, 0.5, [0; 5]),
                behavior_test_object(0x52, 1.0, [0; 5]),
                behavior_test_object(0x53, f32::from_bits(0x3380_0000), [0; 5]),
            ],
        };
        normalize_behavior_distributor(
            &mut near_rounding,
            ObjectHandle {
                generation: 0,
                slot: 0x51,
            },
        );
        assert_eq!(
            near_rounding
                .registered_objects
                .iter()
                .map(|object| object.share.to_bits())
                .collect::<Vec<_>>(),
            vec![0x3f00_0000, 0x3f00_0000, 0x3300_0000]
        );
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_campaign_preview_keeps_unsupported_init_without_partial_writes() {
        let root = licensed_root("IS");
        let vfs: Arc<dyn Vfs> = Arc::new(DirectoryVfs::new(root));
        let mut engine = create(EngineServices::new(vfs)).expect("engine");
        let loaded = load_mission_static_preview_roots(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/CAMPAIGN/CAMPAIGN.00/Mission.01/data.tma".to_string(),
                mode: MissionMode::SingleMission,
            },
            NonZeroUsize::new(1).expect("non-zero root count"),
        )
        .expect("load licensed campaign preview");

        let bundles = loaded_mission_script_bundles(&engine).expect("script bundles");
        assert!(loaded_mission_behavior_distributors(&engine)
            .expect("preview Behavior distributors")
            .is_empty());
        assert!(loaded_mission_behavior_ore_states(&engine)
            .expect("preview Behavior ore state")
            .is_empty());
        assert!(bundles
            .iter()
            .all(|bundle| bundle.behavior_warm_start.is_none()));
        assert_eq!(engine.behavior_rng_state, INITIAL_BEHAVIOR_RNG_STATE);
        let unsupported = bundles
            .iter()
            .find(|bundle| {
                matches!(
                    &bundle.init_execution,
                    MissionScriptInitExecution::Unsupported { .. }
                )
            })
            .expect("campaign package with an unsupported Init instruction");
        let clan_index = unsupported.clan_index;
        let states = loaded_mission_script_init_states(&engine).expect("Init writes");
        assert!(states.iter().all(|state| state.clan_index != clan_index));
        let varset = loaded_mission_script_varset_states(&engine)
            .expect("per-clan varsets")
            .iter()
            .find(|state| state.clan_index == clan_index)
            .expect("unsupported clan varset");
        assert_eq!(
            &varset.values[224..227],
            &[
                VarSetDefault::Dword(950),
                VarSetDefault::Dword(1000),
                VarSetDefault::Dword(0),
            ]
        );
        assert!(loaded.script_bundle_count >= 1);
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn loaded_host_callback_uses_requested_mode_and_constructor_status() {
        let root = licensed_root("IS");
        let vfs: Arc<dyn Vfs> = Arc::new(DirectoryVfs::new(root));
        let key = "MISSIONS/Autodemo.00/data.tma".to_string();
        let mut engine = create(EngineServices::new(vfs)).expect("engine");

        load_mission_static_preview_roots(
            &mut engine,
            MissionRequest {
                key: key.clone(),
                mode: MissionMode::SingleMission,
            },
            NonZeroUsize::new(1).expect("non-zero root count"),
        )
        .expect("load single-mission preview");
        let state = engine.loaded.as_mut().expect("loaded state");
        assert_eq!(state.script_host_status, INITIAL_SCRIPT_HOST_STATUS);
        assert_eq!(state.script_outcome, MissionScriptOutcome::Active);
        assert_eq!(
            apply_loaded_script_host_callback(
                &mut engine,
                VmHostCallbackCommand {
                    mode: 0,
                    first: 0,
                    second: 0,
                },
            )
            .expect("apply failure callback"),
            MissionScriptHostEffect::MissionFailed
        );
        assert_eq!(engine.loaded.as_ref().unwrap().script_host_status, 1);
        assert_eq!(
            loaded_mission_script_outcome(&engine),
            Some(MissionScriptOutcome::Failed)
        );

        engine.loaded.as_mut().unwrap().script_outcome = MissionScriptOutcome::Completed;
        assert_eq!(
            apply_loaded_script_host_callback(
                &mut engine,
                VmHostCallbackCommand {
                    mode: 0,
                    first: 0,
                    second: 0,
                },
            )
            .expect("apply repeated failure callback"),
            MissionScriptHostEffect::NoChange
        );
        assert_eq!(engine.loaded.as_ref().unwrap().script_host_status, 1);
        assert_eq!(
            loaded_mission_script_outcome(&engine),
            Some(MissionScriptOutcome::Completed)
        );

        load_mission_static_preview_roots(
            &mut engine,
            MissionRequest {
                key,
                mode: MissionMode::AutoDemo,
            },
            NonZeroUsize::new(1).expect("non-zero root count"),
        )
        .expect("reload AutoDemo preview");
        assert_eq!(engine.loaded.as_ref().unwrap().script_host_status, 5);
        assert_eq!(
            apply_loaded_script_host_callback(
                &mut engine,
                VmHostCallbackCommand {
                    mode: 0,
                    first: 0,
                    second: 1,
                },
            )
            .expect("apply AutoDemo callback"),
            MissionScriptHostEffect::Unhandled
        );
        assert_eq!(engine.loaded.as_ref().unwrap().script_host_status, 5);
        assert_eq!(
            loaded_mission_script_outcome(&engine),
            Some(MissionScriptOutcome::Active)
        );
    }

    #[test]
    fn reference_movement_rejects_non_finite_or_non_positive_input() {
        let mut engine = create(EngineServices::default()).expect("engine");

        let err =
            advance_reference_movement(&mut engine, OriginalObjectId(0), [f32::NAN, 0.0], 1.0)
                .expect_err("non-finite target must fail");
        assert_eq!(
            err.to_string(),
            "reference movement: target and max_step must be finite; max_step must be positive"
        );

        assert!(matches!(
            advance_reference_movement(&mut engine, OriginalObjectId(0), [0.0, 0.0], 0.0),
            Err(EngineError::Movement(_))
        ));
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn reference_movement_snaps_a_live_mission_object_to_terrain() {
        let root = licensed_root("IS");
        let vfs: Arc<dyn Vfs> = Arc::new(DirectoryVfs::new(&root));
        let mut engine = create(EngineServices::new(vfs)).expect("engine");
        load_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/Autodemo.00/data.tma".to_string(),
                mode: MissionMode::SingleMission,
            },
            0,
        )
        .expect("load mission");

        let (original_id, start_xy, target_xy, expected_xy, expected_height) = {
            let loaded = engine.loaded.as_ref().expect("loaded mission");
            loaded
                .object_drafts
                .iter()
                .find_map(|draft| {
                    let original_id = draft.original_id?;
                    let start_xy = [draft.position[0], draft.position[1]];
                    [[1.0, 0.0], [-1.0, 0.0], [0.0, 1.0], [0.0, -1.0]]
                        .into_iter()
                        .find_map(|offset| {
                            let target_xy = [start_xy[0] + offset[0], start_xy[1] + offset[1]];
                            let expected_xy =
                                [start_xy[0] + offset[0] * 0.5, start_xy[1] + offset[1] * 0.5];
                            let height = loaded.terrain.height_at(expected_xy).ok()??;
                            loaded
                                .terrain
                                .height_at(target_xy)
                                .ok()??
                                .is_finite()
                                .then_some((original_id, start_xy, target_xy, expected_xy, height))
                        })
                })
                .expect("mission object with a movable terrain-neighbour target")
        };

        assert!(
            !advance_reference_movement(&mut engine, original_id, target_xy, 0.5)
                .expect("bounded reference movement")
        );
        let handle = handle_by_original_id(&engine.world, original_id).expect("world object");
        let transform = transform_state(&engine.world, handle).expect("world transform");
        assert_eq!(
            transform.position,
            [
                expected_xy[0].to_bits(),
                expected_xy[1].to_bits(),
                expected_height.to_bits()
            ]
        );
        assert_ne!(start_xy, expected_xy);
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn missing_map_and_missing_reachable_resource_fail_before_registration() {
        let root = licensed_root("IS");
        for (denied, mission) in [
            (
                DenyRule::Suffix("Land.map"),
                MissionRequest {
                    key: "MISSIONS/Autodemo.00/data.tma".to_string(),
                    mode: MissionMode::SingleMission,
                },
            ),
            (
                DenyRule::Suffix("objects.rlb"),
                MissionRequest {
                    key: "MISSIONS/CAMPAIGN/CAMPAIGN.00/Mission.01/data.tma".to_string(),
                    mode: MissionMode::SingleMission,
                },
            ),
        ] {
            let vfs: Arc<dyn Vfs> = Arc::new(DenyVfs {
                inner: DirectoryVfs::new(&root),
                denied,
            });
            let mut engine = create(EngineServices::new(vfs)).expect("engine");
            let before = step_headless(&mut engine, InputSnapshot).expect("before");
            let err = load_mission(&mut engine, mission, 0).expect_err("load error");
            match denied {
                DenyRule::Suffix("Land.map") => assert!(matches!(err, EngineError::Vfs { .. })),
                DenyRule::Suffix("objects.rlb") => {
                    assert!(matches!(err, EngineError::PrototypeGraph { .. }))
                }
                DenyRule::Suffix(unexpected) => panic!("unexpected deny rule {unexpected}"),
            }
            assert!(loaded_mission(&engine).is_none());
            let after = step_headless(&mut engine, InputSnapshot).expect("after");
            assert_eq!(before.snapshot.objects, after.snapshot.objects);
        }
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn registration_phase_failure_uses_normal_teardown_and_keeps_engine_world() {
        let root = licensed_root("IS");
        let vfs: Arc<dyn Vfs> = Arc::new(DirectoryVfs::new(root));
        let mut engine = create(EngineServices::new(vfs)).expect("engine");
        let before = step_headless(&mut engine, InputSnapshot).expect("before");

        let err = load_mission_with_options(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/CAMPAIGN/CAMPAIGN.00/Mission.01/data.tma".to_string(),
                mode: MissionMode::SingleMission,
            },
            MissionLoadOptions {
                fail_after_registered_objects: Some(1),
                ..MissionLoadOptions::default()
            },
            Some(0),
        )
        .expect_err("forced registration failure");

        assert!(matches!(
            err,
            EngineError::RegistrationTeardown {
                registered_objects: 1,
                released_objects: 1,
                managers_released: true,
            }
        ));
        assert!(loaded_mission(&engine).is_none());
        let after = step_headless(&mut engine, InputSnapshot).expect("after");
        assert_eq!(before.snapshot.objects, after.snapshot.objects);
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn full_startup_init_failure_keeps_the_previous_world_and_rng() {
        let root = licensed_root("IS");
        let original_script = std::fs::read(root.join("MISSIONS/SCRIPTS/default.scr"))
            .expect("read licensed default.scr");
        let mut package = fparkan_script::decode(&original_script).expect("decode default.scr");
        let init_index = package
            .events
            .iter()
            .position(|event| event.name_raw.split(|byte| *byte == 0).next() == Some(&b"Init"[..]))
            .expect("Init event");
        assert_eq!(
            package.events[init_index].instructions[0].header_words[0],
            19
        );
        package.events[init_index].instructions[0].header_words[0] = 18;
        let replacement = Arc::<[u8]>::from(encode_script_package(&package));

        let vfs = Arc::new(SwappingScriptVfs {
            inner: DirectoryVfs::new(&root),
            replacement,
            enabled: std::sync::atomic::AtomicBool::new(false),
        });
        let services_vfs: Arc<dyn Vfs> = vfs.clone();
        let mut engine = create(EngineServices::new(services_vfs)).expect("engine");
        let mission = || MissionRequest {
            key: "MISSIONS/Autodemo.00/data.tma".to_string(),
            mode: MissionMode::AutoDemo,
        };
        load_mission(&mut engine, mission(), 100_000).expect("initial full startup");

        let previous_loaded = loaded_mission(&engine).expect("previous mission").clone();
        let previous_bundles = loaded_mission_script_bundles(&engine)
            .expect("previous bundles")
            .to_vec();
        let previous_distributors = loaded_mission_behavior_distributors(&engine)
            .expect("previous Behavior distributors")
            .to_vec();
        let previous_rng = engine.behavior_rng_state;
        let before = step_headless(&mut engine, InputSnapshot)
            .expect("step previous world")
            .snapshot
            .objects;

        vfs.enabled.store(true, std::sync::atomic::Ordering::SeqCst);
        let error = load_mission(&mut engine, mission(), 100_500)
            .expect_err("unsupported Init must fail after new-world registration");
        assert!(matches!(
            error,
            EngineError::ScriptInitUnsupported {
                source: Handler19ResolveError::UnsupportedSelector {
                    found: ScriptDispatchSelector::Handler(18),
                },
                ..
            }
        ));
        assert_eq!(loaded_mission(&engine), Some(&previous_loaded));
        assert_eq!(
            loaded_mission_script_bundles(&engine),
            Some(previous_bundles.as_slice())
        );
        assert_eq!(
            loaded_mission_behavior_distributors(&engine),
            Some(previous_distributors.as_slice())
        );
        assert_eq!(engine.behavior_rng_state, previous_rng);
        let after = step_headless(&mut engine, InputSnapshot)
            .expect("step retained world")
            .snapshot
            .objects;
        assert_eq!(before, after);
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn selected_is_and_is2_missions_execute_10000_deterministic_ticks() {
        for case in [
            HeadlessCase {
                root: "IS",
                mission: "MISSIONS/CAMPAIGN/CAMPAIGN.00/Mission.01/data.tma",
                object_count: 33,
                expected_hash: [
                    0xc7, 0xb0, 0x6e, 0x0a, 0x31, 0x1f, 0x5d, 0x8c, 0xde, 0x64, 0xa5, 0x33, 0x1f,
                    0x2c, 0xd0, 0x2c, 0x21, 0x44, 0x2f, 0x34, 0x5d, 0x16, 0xe8, 0x94, 0xaf, 0xa2,
                    0x2b, 0xa9, 0xd4, 0x24, 0xd2, 0xf9,
                ],
            },
            HeadlessCase {
                root: "IS2",
                mission: "MISSIONS/Campaign/CAMPAIGN.00/Mission.02/data.tma",
                object_count: 10,
                expected_hash: [
                    0x3c, 0xe5, 0xa6, 0x39, 0x47, 0x86, 0x76, 0xe1, 0xb2, 0x1a, 0x8e, 0x96, 0x3d,
                    0x60, 0x6e, 0xc6, 0x8c, 0xe2, 0x28, 0x4f, 0x57, 0xd9, 0xe1, 0xe4, 0xb5, 0x95,
                    0xdf, 0x88, 0xd3, 0x2f, 0x4a, 0x4d,
                ],
            },
        ] {
            let first = run_headless_case(case);
            let second = run_headless_case(case);
            assert_eq!(first, second);
            assert_eq!(first.tick.0, 10_000);
            assert_eq!(first.objects.len(), case.object_count);
            assert_eq!(first.hash.0, case.expected_hash);
        }
    }

    #[test]
    #[ignore = "requires local testdata corpus"]
    fn selected_is_and_is2_missions_preserve_runtime_object_drafts() {
        for case in [
            ("IS", "MISSIONS/Autodemo.00/data.tma"),
            ("IS2", "MISSIONS/Autodemo.00/data.tma"),
        ] {
            let root = local_testdata_root(case.0);
            let vfs: Arc<dyn Vfs> = Arc::new(DirectoryVfs::new(root));
            let mut engine = create(EngineServices::new(vfs)).expect("engine");
            let loaded = load_mission(
                &mut engine,
                MissionRequest {
                    key: case.1.to_string(),
                    mode: MissionMode::SingleMission,
                },
                0,
            )
            .expect("load mission");
            let drafts = loaded_mission_object_drafts(&engine).expect("object drafts");
            let assets = loaded_mission_assets(&engine).expect("mission assets");
            let graph = loaded_prototype_graph(&engine).expect("prototype graph");

            assert_eq!(drafts.len(), loaded.object_count);
            assert!(drafts.iter().any(|draft| !draft.visual_ids.is_empty()));
            for (index, draft) in drafts.iter().enumerate() {
                assert_eq!(
                    draft.original_id,
                    u32::try_from(index).ok().map(OriginalObjectId)
                );
                assert_eq!(draft.visual_ids, assets.visuals_for_object(index));
                assert_eq!(draft.unit_components, graph.root_unit_components[index]);
                assert!(draft.position.iter().all(|component| component.is_finite()));
                assert!(draft
                    .orientation_raw
                    .iter()
                    .all(|component| component.is_finite()));
                assert!(draft.scale.iter().all(|component| component.is_finite()));
            }
        }
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_corpus_part1_loads_all_mission_foundations() {
        let part1 = load_all(&licensed_root("IS"));
        assert_eq!(part1.missions, 29);
        assert_eq!(part1.paths, 34);
        assert_eq!(part1.clans, 101);
        assert_eq!(part1.objects, 864);
        assert_eq!(part1.extras, 28);
        assert_eq!(part1.unit_references, 463);
        assert_eq!(part1.direct_references, 401);
        assert_eq!(part1.unit_components, 4_300);
        assert_eq!(part1.prototype_requests, 4_701);
        assert_eq!(part1.material_slots, 36_954);
        assert_eq!(part1.texture_requests, 48_806);
        assert_eq!(part1.lightmap_requests, 139);
        assert_eq!(part1.graph_failures, 0);
        assert_eq!(part1.wear_requests, part1.prototype_requests);
        assert_eq!(part1.wear_requests, part1.wear_resolved);
        assert_eq!(part1.material_slots, part1.material_resolved);
        assert_eq!(part1.texture_requests, part1.texture_resolved);
        assert_eq!(part1.lightmap_requests, part1.lightmap_resolved);
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_corpus_part2_loads_all_mission_foundations() {
        let part2 = load_all(&licensed_root("IS2"));
        assert_eq!(part2.missions, 31);
        assert_eq!(part2.paths, 61);
        assert_eq!(part2.clans, 91);
        assert_eq!(part2.objects, 885);
        assert_eq!(part2.extras, 41);
        assert_eq!(part2.unit_references, 561);
        assert_eq!(part2.direct_references, 324);
        assert_eq!(part2.unit_components, 5_521);
        assert_eq!(part2.prototype_requests, 5_845);
        assert_eq!(part2.material_slots, 50_888);
        assert_eq!(part2.texture_requests, 68_603);
        assert_eq!(part2.lightmap_requests, 214);
        assert_eq!(part2.graph_failures, 0);
        assert_eq!(part2.wear_requests, part2.prototype_requests);
        assert_eq!(part2.wear_requests, part2.wear_resolved);
        assert_eq!(part2.material_slots, part2.material_resolved);
        assert_eq!(part2.texture_requests, part2.texture_resolved);
        assert_eq!(part2.lightmap_requests, part2.lightmap_resolved);
    }

    #[derive(Default)]
    struct LoadTotals {
        missions: usize,
        paths: usize,
        clans: usize,
        objects: usize,
        extras: usize,
        unit_references: usize,
        direct_references: usize,
        unit_components: usize,
        prototype_requests: usize,
        wear_requests: usize,
        wear_resolved: usize,
        material_slots: usize,
        material_resolved: usize,
        texture_requests: usize,
        texture_resolved: usize,
        lightmap_requests: usize,
        lightmap_resolved: usize,
        graph_failures: usize,
    }

    #[derive(Clone, Copy)]
    struct HeadlessCase {
        root: &'static str,
        mission: &'static str,
        object_count: usize,
        expected_hash: [u8; 32],
    }

    fn run_headless_case(case: HeadlessCase) -> WorldSnapshot {
        let root = licensed_root(case.root);
        let vfs: Arc<dyn Vfs> = Arc::new(DirectoryVfs::new(root));
        let mut engine = create(EngineServices::new(vfs)).expect("engine");
        let loaded = load_mission(
            &mut engine,
            MissionRequest {
                key: case.mission.to_string(),
                mode: MissionMode::SingleMission,
            },
            0,
        )
        .expect("load selected mission");
        assert_eq!(loaded.object_count, case.object_count);

        let mut snapshot = None;
        for _ in 0..10_000 {
            snapshot = Some(
                step_headless(&mut engine, InputSnapshot)
                    .expect("selected mission deterministic tick")
                    .snapshot,
            );
        }
        snapshot.expect("at least one tick")
    }

    fn load_all(root: &Path) -> LoadTotals {
        assert!(root.is_dir(), "missing licensed corpus {}", root.display());
        let mut missions = mission_paths(root);
        missions.sort();
        let vfs: Arc<dyn Vfs> = Arc::new(DirectoryVfs::new(root));
        let mut totals = LoadTotals::default();
        for mission in missions {
            let mut engine = create(EngineServices::new(vfs.clone())).expect("engine");
            let loaded = load_mission(
                &mut engine,
                MissionRequest {
                    key: mission,
                    mode: MissionMode::SingleMission,
                },
                0,
            )
            .expect("load mission foundation");
            assert_eq!(loaded.object_count, loaded.registered_objects);
            assert_eq!(loaded.object_count, loaded.graph_root_count);
            assert_eq!(
                loaded.graph_direct_reference_count + loaded.graph_unit_component_count,
                loaded.graph_resolved_count
            );
            assert_eq!(loaded.graph_failure_count, 0);
            assert_eq!(
                loaded.graph_wear_request_count,
                loaded.graph_wear_resolved_count
            );
            assert_eq!(
                loaded.graph_material_slot_count,
                loaded.graph_material_resolved_count
            );
            assert_eq!(
                loaded.graph_texture_request_count,
                loaded.graph_texture_resolved_count
            );
            assert_eq!(
                loaded.graph_lightmap_request_count,
                loaded.graph_lightmap_resolved_count
            );
            assert_eq!(loaded.build_category_count, 12);
            assert!(loaded.areal_count > 0);
            assert!(loaded.surface_count > 0);
            totals.missions += 1;
            totals.paths += loaded.path_count;
            totals.clans += loaded.clan_count;
            totals.objects += loaded.object_count;
            totals.extras += loaded.extra_count;
            totals.unit_references += loaded.graph_unit_reference_count;
            totals.direct_references += loaded.graph_direct_reference_count;
            totals.unit_components += loaded.graph_unit_component_count;
            totals.prototype_requests += loaded.graph_resolved_count;
            totals.wear_requests += loaded.graph_wear_request_count;
            totals.wear_resolved += loaded.graph_wear_resolved_count;
            totals.material_slots += loaded.graph_material_slot_count;
            totals.material_resolved += loaded.graph_material_resolved_count;
            totals.texture_requests += loaded.graph_texture_request_count;
            totals.texture_resolved += loaded.graph_texture_resolved_count;
            totals.lightmap_requests += loaded.graph_lightmap_request_count;
            totals.lightmap_resolved += loaded.graph_lightmap_resolved_count;
            totals.graph_failures += loaded.graph_failure_count;
        }
        totals
    }

    fn mission_paths(root: &Path) -> Vec<String> {
        let mut out = Vec::new();
        collect_missions(root, root, &mut out);
        out
    }

    fn collect_missions(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let mut children: Vec<PathBuf> = std::fs::read_dir(dir)
            .expect("read dir")
            .map(|entry| entry.expect("entry").path())
            .collect();
        children.sort();
        for child in children {
            if child.is_dir() {
                collect_missions(root, &child, out);
            } else if child
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case("data.tma"))
            {
                let rel = child.strip_prefix(root).expect("relative");
                let rel = rel.to_str().expect("utf8 path").replace('\\', "/");
                out.push(rel);
            }
        }
    }

    fn licensed_root(name: &str) -> PathBuf {
        let variable = match name {
            "IS" => "FPARKAN_CORPUS_PART1_ROOT",
            "IS2" => "FPARKAN_CORPUS_PART2_ROOT",
            _ => panic!("unknown licensed corpus part: {name}"),
        };
        let root = std::env::var_os(variable)
            .map(PathBuf::from)
            .unwrap_or_else(|| panic!("{variable} is required for licensed corpus tests"));
        assert!(
            root.is_dir(),
            "licensed corpus root is missing: {}",
            root.display()
        );
        root
    }

    fn local_testdata_root(name: &str) -> PathBuf {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../testdata")
            .join(name);
        assert!(
            root.is_dir(),
            "local testdata root is missing: {}",
            root.display()
        );
        root
    }

    fn encode_script_package(package: &ScriptPackage) -> Vec<u8> {
        fn push_u32(bytes: &mut Vec<u8>, value: u32) {
            bytes.extend_from_slice(&value.to_le_bytes());
        }

        let mut bytes = Vec::new();
        push_u32(&mut bytes, package.opcode_handler_count);
        push_u32(
            &mut bytes,
            u32::try_from(package.events.len()).expect("event count fits u32"),
        );
        for event in &package.events {
            push_u32(&mut bytes, event.name_len);
            bytes.extend_from_slice(&event.name_raw);
            push_u32(&mut bytes, event.event_word);
            push_u32(
                &mut bytes,
                u32::try_from(event.instructions.len()).expect("instruction count fits u32"),
            );
            for instruction in &event.instructions {
                for word in &instruction.header_words[..6] {
                    push_u32(&mut bytes, *word);
                }
                for reference in &instruction.references {
                    push_u32(&mut bytes, *reference);
                }
                push_u32(&mut bytes, instruction.header_words[6]);
            }
        }
        bytes.extend_from_slice(&package.trailing_bytes);
        bytes
    }

    #[derive(Clone, Copy)]
    enum DenyRule {
        Suffix(&'static str),
    }

    struct DenyVfs {
        inner: DirectoryVfs,
        denied: DenyRule,
    }

    struct SwappingScriptVfs {
        inner: DirectoryVfs,
        replacement: Arc<[u8]>,
        enabled: std::sync::atomic::AtomicBool,
    }

    impl SwappingScriptVfs {
        fn use_replacement(&self, path: &NormalizedPath) -> bool {
            self.enabled.load(std::sync::atomic::Ordering::SeqCst)
                && path
                    .as_str()
                    .eq_ignore_ascii_case("MISSIONS/SCRIPTS/default.scr")
        }
    }

    impl Vfs for SwappingScriptVfs {
        fn metadata(&self, path: &NormalizedPath) -> Result<VfsMetadata, VfsError> {
            if self.use_replacement(path) {
                return Ok(VfsMetadata {
                    len: self.replacement.len() as u64,
                    fingerprint: [0x18; 32],
                });
            }
            self.inner.metadata(path)
        }

        fn read(&self, path: &NormalizedPath) -> Result<Arc<[u8]>, VfsError> {
            if self.use_replacement(path) {
                return Ok(self.replacement.clone());
            }
            self.inner.read(path)
        }

        fn list(&self, prefix: &NormalizedPath) -> Result<Vec<VfsEntry>, VfsError> {
            self.inner.list(prefix)
        }
    }

    impl DenyVfs {
        fn denied(&self, path: &NormalizedPath) -> bool {
            match self.denied {
                DenyRule::Suffix(suffix) => path
                    .as_str()
                    .to_ascii_uppercase()
                    .ends_with(&suffix.to_ascii_uppercase()),
            }
        }
    }

    impl Vfs for DenyVfs {
        fn metadata(&self, path: &NormalizedPath) -> Result<VfsMetadata, VfsError> {
            if self.denied(path) {
                return Err(VfsError::NotFound(path.as_str().to_string()));
            }
            self.inner.metadata(path)
        }

        fn read(&self, path: &NormalizedPath) -> Result<Arc<[u8]>, VfsError> {
            if self.denied(path) {
                return Err(VfsError::NotFound(path.as_str().to_string()));
            }
            self.inner.read(path)
        }

        fn list(&self, prefix: &NormalizedPath) -> Result<Vec<VfsEntry>, VfsError> {
            self.inner.list(prefix).map(|entries| {
                entries
                    .into_iter()
                    .filter(|entry| !self.denied(&entry.path))
                    .collect()
            })
        }
    }
}
