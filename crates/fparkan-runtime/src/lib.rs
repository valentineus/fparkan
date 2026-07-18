#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        clippy::cast_precision_loss,
        clippy::expect_used,
        clippy::float_cmp,
        clippy::identity_op,
        clippy::too_many_lines,
        clippy::uninlined_format_args,
        clippy::map_unwrap_or,
        clippy::needless_raw_string_hashes,
        clippy::semicolon_if_nothing_returned,
        clippy::type_complexity,
        clippy::panic,
        clippy::unwrap_used
    )
)]
//! Runtime orchestration for headless and rendered modes.

use fparkan_assets::{
    decode_mission_land_path, decode_mission_payload, decode_nres_payload,
    derive_mission_land_paths, extend_graph_report_with_visual_dependencies_with_progress,
    mission_script_bundle_bases, prepare_terrain_world, AssetError as AssetPreparationError,
    AssetId, AssetManager, AssetPreparationPhase, BuildCategory, MissionAssetPlan, MissionDocument,
    MissionError, MissionTerrainPaths, NresError, PreparedVisual, TerrainFormatError,
    TerrainPreparationError, TerrainWorld, TmaProfile, VisualDependencyPhase,
};
use fparkan_path::{normalize_relative, NormalizedPath, PathError, PathPolicy};
use fparkan_prototype::{
    build_prototype_graph_report, PrototypeGraph, PrototypeGraphFailure, PrototypeGraphReport,
    UnitComponentRecord,
};
use fparkan_resource::{resource_name, CachedResourceRepository, ResourceRepository};
use fparkan_script::{
    Handler19DwordWrite, Handler19InitInput, ScriptDispatchSelector, ScriptPackage, VarSet,
};
use fparkan_terrain::SurfaceQuery;
use fparkan_vfs::{Vfs, VfsError};
use fparkan_world::{
    construct_object, handle_by_original_id, new as new_world, register_object, set_transform,
    step, transform_state, InputSnapshot, ObjectDraft, OriginalObjectId, TransformState, World,
    WorldConfig, WorldSnapshot,
};
use std::num::NonZeroUsize;
use std::sync::Arc;

pub use fparkan_assets::MissionAssets;

const MISSION_DECODED_PAYLOAD_CACHE_ENTRIES: usize = 256;
const FALLBACK_SCRIPT_VARSET_PATH: &str = "MISSIONS/SCRIPTS/varset.var";

/// Engine mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineMode {
    /// Headless.
    Headless,
    /// Rendered.
    Rendered,
}

/// Scheduler phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchedulerPhase {
    /// Collect platform events.
    CollectPlatformEvents,
    /// Build input snapshot.
    BuildInputSnapshot,
    /// Advance clock.
    AdvanceGameClock,
    /// Calculate world queue.
    CalculateWorldQueue,
    /// Apply deferred operations.
    ApplyDeferredOperations,
    /// Update animation/effects.
    UpdateAnimationAndEffects,
    /// Publish render snapshot.
    PublishRenderSnapshot,
    /// Render world.
    RenderWorld,
    /// End frame callbacks.
    EndFrameCallbacks,
    /// Maintenance.
    Maintenance,
}

/// Engine config.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EngineConfig {
    /// Mode.
    pub mode: EngineMode,
}

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

/// Mission request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MissionRequest {
    /// Mission key/path.
    pub key: String,
}

/// Mission loading phase captured for diagnostics and acceptance tests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MissionLoadPhase {
    /// Resolve services and mission request context.
    Context,
    /// Decode and validate TMA.
    Tma,
    /// Decode and validate terrain map assets.
    Map,
    /// Expand object roots into a prototype graph.
    Graph,
    /// Expand model-backed visual dependencies of the prototype graph.
    GraphVisuals,
    /// Resolve WEAR tables while expanding visual dependencies.
    GraphVisualWears,
    /// Resolve MAT0 documents while expanding visual dependencies.
    GraphVisualMaterials,
    /// Progress marker emitted after each 64 MAT0 validation requests.
    GraphVisualMaterialRequests {
        /// Number of MAT0 validation requests started.
        request_count: usize,
        /// Graph nodes materialized before the request.
        graph_node_count: usize,
        /// Graph edges materialized before the request.
        graph_edge_count: usize,
        /// Distinct WEAR validation keys retained before the request.
        wear_cache_entries: usize,
        /// Distinct MAT0 validation keys retained before the request.
        material_cache_entries: usize,
        /// Distinct TEXM validation keys retained before the request.
        texture_cache_entries: usize,
    },
    /// Validate TEXM documents while expanding visual dependencies.
    GraphVisualTextures,
    /// Progress marker emitted after each 64 TEXM validation requests.
    GraphVisualTextureRequests {
        /// Number of TEXM validation requests started.
        request_count: usize,
        /// Graph nodes materialized before the request.
        graph_node_count: usize,
        /// Graph edges materialized before the request.
        graph_edge_count: usize,
        /// Distinct WEAR validation keys retained before the request.
        wear_cache_entries: usize,
        /// Distinct MAT0 validation keys retained before the request.
        material_cache_entries: usize,
        /// Distinct TEXM validation keys retained before the request.
        texture_cache_entries: usize,
    },
    /// Prepare all reachable visual/resource dependencies.
    Assets,
    /// Decode and validate MSH model meshes.
    AssetModelMeshes,
    /// Decode WEAR material tables.
    AssetWearTables,
    /// Resolve MAT0 material documents.
    AssetMaterials,
    /// Decode diffuse textures and baked lightmaps.
    AssetTextures,
    /// Construct all object drafts before registration.
    Construct,
    /// Register constructed objects.
    Register,
}

/// Raw placed transform preserved by the mission loader.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedTransformProfile {
    /// Object index in TMA order.
    pub object_index: usize,
    /// Raw position vector.
    pub position: [f32; 3],
    /// Raw orientation vector. No Euler order is inferred here.
    pub orientation_raw: [f32; 3],
    /// Raw scale vector.
    pub scale: [f32; 3],
}

/// Mission loading trace.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MissionLoadTrace {
    /// Observed phases in execution order.
    pub phases: Vec<MissionLoadPhase>,
    /// Number of object drafts constructed before the first registration.
    pub drafts_before_registration: usize,
    /// Number of objects registered.
    pub registered_objects: usize,
    /// Raw transform profile for placed objects.
    pub transforms: Vec<PlacedTransformProfile>,
}

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
    /// Raw identity/clan word from the mission.
    pub identity_or_clan_raw: u32,
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
    /// Raw event word preserved from the compiled `Init` event.
    pub event_word: u32,
    /// Exact three DWORD writes made by each proven `Handler(19)` instruction.
    pub writes: Vec<Handler19DwordWrite>,
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
}

/// Frame result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameResult {
    /// Snapshot.
    pub snapshot: WorldSnapshot,
    /// Scheduler phases executed for this frame.
    pub trace: FrameTrace,
}

/// Scheduler trace for a completed frame.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FrameTrace {
    /// Frame phases in execution order.
    pub phases: Vec<SchedulerPhase>,
}

/// Engine.
pub struct Engine {
    config: EngineConfig,
    services: EngineServices,
    world: World,
    loaded: Option<LoadedMissionState>,
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
    script_bundles: Vec<MissionScriptBundle>,
    script_varset: Option<MissionScriptVarSet>,
    script_init_states: Vec<MissionScriptInitState>,
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
    /// Scheduler phase order was violated.
    SchedulerPhaseOrder {
        /// Previous phase.
        previous: SchedulerPhase,
        /// Current phase.
        current: SchedulerPhase,
    },
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
            Self::SchedulerPhaseOrder { previous, current } => write!(
                f,
                "scheduler phase order regressed from {previous:?} to {current:?}"
            ),
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
            Self::Terrain(source) => Some(source),
            Self::World(source) => Some(source),
            Self::AssetPreparation { source, .. } => Some(source),
            Self::MissingVfs
            | Self::Script { .. }
            | Self::VarSet { .. }
            | Self::ScriptInit { .. }
            | Self::Movement(_)
            | Self::PrototypeGraph { .. }
            | Self::SchedulerPhaseOrder { .. }
            | Self::RegistrationTeardown { .. } => None,
        }
    }
}

/// Creates engine.
///
/// # Errors
///
/// Currently this constructor is infallible, but it returns
/// [`EngineError`] to keep the composition-root API stable as services become
/// mandatory.
pub fn create(config: EngineConfig, services: EngineServices) -> Result<Engine, EngineError> {
    Ok(Engine {
        config,
        services,
        world: new_world(WorldConfig),
        loaded: None,
    })
}

/// Loads mission transactionally.
///
/// # Errors
///
/// Returns [`EngineError`] when VFS services are missing, mission paths are
/// invalid, required files cannot be read, disk formats fail validation, terrain
/// runtime data cannot be built, prototype graph roots do not resolve, or
/// object registration fails.
pub fn load_mission(
    engine: &mut Engine,
    request: MissionRequest,
) -> Result<LoadedMission, EngineError> {
    load_mission_with_trace(engine, request).map(|(loaded, _trace)| loaded)
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
    load_mission_static_preview_with_progress(engine, request, |_| {})
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
    load_mission_static_preview_roots_with_progress(engine, request, root_count, |_| {})
}

/// Loads selected static-preview roots while synchronously reporting phases.
///
/// # Errors
///
/// Returns [`EngineError`] under the same conditions as
/// [`load_mission_static_preview_roots`].
pub fn load_mission_static_preview_roots_with_progress(
    engine: &mut Engine,
    request: MissionRequest,
    root_count: NonZeroUsize,
    mut on_phase: impl FnMut(MissionLoadPhase),
) -> Result<LoadedMission, EngineError> {
    load_mission_with_options_and_progress(
        engine,
        request,
        MissionLoadOptions {
            asset_scope: MissionAssetScope::PreviewRoots(root_count),
            ..MissionLoadOptions::default()
        },
        Some(&mut on_phase),
    )
    .map(|(loaded, _trace)| loaded)
}

/// Loads a static preview while synchronously reporting entered loading phases.
///
/// This has the same bounded asset scope as [`load_mission_static_preview`].
///
/// # Errors
///
/// Returns [`EngineError`] under the same conditions as
/// [`load_mission_static_preview`].
pub fn load_mission_static_preview_with_progress(
    engine: &mut Engine,
    request: MissionRequest,
    mut on_phase: impl FnMut(MissionLoadPhase),
) -> Result<LoadedMission, EngineError> {
    load_mission_with_options_and_progress(
        engine,
        request,
        MissionLoadOptions {
            asset_scope: MissionAssetScope::FirstMeshPreview,
            ..MissionLoadOptions::default()
        },
        Some(&mut on_phase),
    )
    .map(|(loaded, _trace)| loaded)
}

/// Loads mission transactionally and returns a diagnostic trace.
///
/// # Errors
///
/// Returns [`EngineError`] under the same conditions as [`load_mission`].
pub fn load_mission_with_trace(
    engine: &mut Engine,
    request: MissionRequest,
) -> Result<(LoadedMission, MissionLoadTrace), EngineError> {
    load_mission_with_options_and_progress(engine, request, MissionLoadOptions::default(), None)
}

/// Loads a mission while synchronously reporting each entered loading phase.
///
/// The observer runs immediately after the phase is recorded in the returned
/// trace. It is intended for diagnostic progress reporting; it does not alter
/// loader ordering, validation, or transaction behavior.
///
/// # Errors
///
/// Returns [`EngineError`] under the same conditions as [`load_mission`].
pub fn load_mission_with_progress(
    engine: &mut Engine,
    request: MissionRequest,
    mut on_phase: impl FnMut(MissionLoadPhase),
) -> Result<LoadedMission, EngineError> {
    load_mission_with_options_and_progress(
        engine,
        request,
        MissionLoadOptions::default(),
        Some(&mut on_phase),
    )
    .map(|(loaded, _trace)| loaded)
}

#[allow(clippy::too_many_lines)]
fn load_mission_with_options_and_progress(
    engine: &mut Engine,
    request: MissionRequest,
    options: MissionLoadOptions,
    mut on_phase: Option<&mut dyn FnMut(MissionLoadPhase)>,
) -> Result<(LoadedMission, MissionLoadTrace), EngineError> {
    let mut trace = MissionLoadTrace::default();
    record_load_phase(&mut trace, &mut on_phase, MissionLoadPhase::Context);
    let vfs = engine.services.vfs.clone().ok_or(EngineError::MissingVfs)?;
    let mission_path = normalize_engine_path("mission", &request.key)?;
    let mission_bytes = read_vfs(&vfs, &mission_path)?;

    record_load_phase(&mut trace, &mut on_phase, MissionLoadPhase::Map);
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
    record_load_phase(&mut trace, &mut on_phase, MissionLoadPhase::Tma);
    let mission = decode_mission_payload(mission_bytes, TmaProfile::Strict).map_err(|source| {
        EngineError::Mission {
            path: mission_path.as_str().to_string(),
            source,
        }
    })?;
    let loaded_scripts = load_mission_scripts(&vfs, &mission)?;
    trace.transforms = mission
        .objects
        .iter()
        .enumerate()
        .map(|(object_index, object)| PlacedTransformProfile {
            object_index,
            position: object.position,
            orientation_raw: object.orientation,
            scale: object.scale,
        })
        .collect();
    record_load_phase(&mut trace, &mut on_phase, MissionLoadPhase::Graph);
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
    record_load_phase(&mut trace, &mut on_phase, MissionLoadPhase::GraphVisuals);
    let mut last_graph_visual_phase = None;
    extend_graph_report_with_visual_dependencies_with_progress(
        &repository,
        &mut prototype_report,
        &mut prototype_graph,
        &resolved_prototypes,
        |progress| {
            let runtime_phase = match progress.phase {
                VisualDependencyPhase::Wear => MissionLoadPhase::GraphVisualWears,
                VisualDependencyPhase::Material => MissionLoadPhase::GraphVisualMaterials,
                VisualDependencyPhase::Texture => MissionLoadPhase::GraphVisualTextures,
            };
            if last_graph_visual_phase != Some(runtime_phase) {
                record_load_phase(&mut trace, &mut on_phase, runtime_phase);
                last_graph_visual_phase = Some(runtime_phase);
            }
            if progress.request_count > 1 {
                let request_phase = match progress.phase {
                    VisualDependencyPhase::Wear => None,
                    VisualDependencyPhase::Material => {
                        Some(MissionLoadPhase::GraphVisualMaterialRequests {
                            request_count: progress.request_count,
                            graph_node_count: progress.graph_node_count,
                            graph_edge_count: progress.graph_edge_count,
                            wear_cache_entries: progress.cache_entries.wear_entries,
                            material_cache_entries: progress.cache_entries.material_entries,
                            texture_cache_entries: progress.cache_entries.texture_entries,
                        })
                    }
                    VisualDependencyPhase::Texture => {
                        Some(MissionLoadPhase::GraphVisualTextureRequests {
                            request_count: progress.request_count,
                            graph_node_count: progress.graph_node_count,
                            graph_edge_count: progress.graph_edge_count,
                            wear_cache_entries: progress.cache_entries.wear_entries,
                            material_cache_entries: progress.cache_entries.material_entries,
                            texture_cache_entries: progress.cache_entries.texture_entries,
                        })
                    }
                };
                if let Some(request_phase) = request_phase {
                    record_load_phase(&mut trace, &mut on_phase, request_phase);
                }
            }
        },
    );
    if !prototype_report.is_success() {
        return Err(EngineError::PrototypeGraph {
            failures: prototype_report.failures.clone(),
        });
    }
    record_load_phase(&mut trace, &mut on_phase, MissionLoadPhase::Assets);
    let asset_manager = AssetManager::new(repository);
    let mut last_asset_phase = None;
    let mut observe_asset_phase = |phase| {
        let runtime_phase = match phase {
            AssetPreparationPhase::ModelMesh => MissionLoadPhase::AssetModelMeshes,
            AssetPreparationPhase::WearTable => MissionLoadPhase::AssetWearTables,
            AssetPreparationPhase::Materials => MissionLoadPhase::AssetMaterials,
            AssetPreparationPhase::Textures => MissionLoadPhase::AssetTextures,
        };
        if last_asset_phase != Some(runtime_phase) {
            record_load_phase(&mut trace, &mut on_phase, runtime_phase);
            last_asset_phase = Some(runtime_phase);
        }
    };
    let mission_assets = match options.asset_scope {
        MissionAssetScope::Full => asset_manager.prepare_mission_assets_with_progress(
            &prototype_graph.root_prototype_request_spans,
            &resolved_prototypes,
            &mut observe_asset_phase,
        ),
        MissionAssetScope::FirstMeshPreview => prepare_first_preview_assets(
            &asset_manager,
            &prototype_graph.root_prototype_request_spans,
            &resolved_prototypes,
            &mut observe_asset_phase,
        ),
        MissionAssetScope::PreviewRoots(root_count) => asset_manager
            .prepare_mission_assets_with_progress(
                &prototype_graph.root_prototype_request_spans[..root_count
                    .get()
                    .min(prototype_graph.root_prototype_request_spans.len())],
                &resolved_prototypes,
                &mut observe_asset_phase,
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
            identity_or_clan_raw: object.identity_or_clan_raw,
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
    record_load_phase(&mut trace, &mut on_phase, MissionLoadPhase::Construct);
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
    trace.drafts_before_registration = handles.len();
    record_load_phase(&mut trace, &mut on_phase, MissionLoadPhase::Register);
    for handle in &handles {
        if options.fail_after_registered_objects == Some(trace.registered_objects) {
            let report = fparkan_world::shutdown(new_runtime_world);
            return Err(EngineError::RegistrationTeardown {
                registered_objects: trace.registered_objects,
                released_objects: report.released_objects.len(),
                managers_released: report.managers_released,
            });
        }
        register_object(&mut new_runtime_world, *handle)?;
        trace.registered_objects += 1;
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
    };

    engine.world = new_runtime_world;
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
        script_bundles: loaded_scripts.bundles,
        script_varset: loaded_scripts.varset,
        script_init_states: loaded_scripts.init_states,
    });
    Ok((summary, trace))
}

fn record_load_phase(
    trace: &mut MissionLoadTrace,
    on_phase: &mut Option<&mut dyn FnMut(MissionLoadPhase)>,
    phase: MissionLoadPhase,
) {
    trace.phases.push(phase);
    if let Some(observer) = on_phase.as_deref_mut() {
        observer(phase);
    }
}

fn prepare_first_preview_assets<R: ResourceRepository>(
    asset_manager: &AssetManager<R>,
    root_spans: &[std::ops::Range<usize>],
    prototypes: &[fparkan_prototype::EffectivePrototype],
    on_phase: &mut dyn FnMut(AssetPreparationPhase),
) -> Result<MissionAssets, AssetPreparationError> {
    for span in root_spans {
        let assets = asset_manager.prepare_mission_assets_with_progress(
            std::slice::from_ref(span),
            prototypes,
            &mut *on_phase,
        )?;
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
    run_frame(engine, input, SchedulerPresentation::Headless)
}

/// Steps rendered mode.
///
/// # Errors
///
/// Returns [`EngineError`] when the world step fails.
pub fn frame(engine: &mut Engine) -> Result<FrameResult, EngineError> {
    match engine.config.mode {
        EngineMode::Headless => step_headless(engine, InputSnapshot),
        EngineMode::Rendered => run_frame(engine, InputSnapshot, SchedulerPresentation::Rendered),
    }
}

/// Shuts down engine.
///
/// # Errors
///
/// Currently shutdown is infallible, but the `Result` preserves the lifecycle
/// API for future service teardown failures.
pub fn shutdown(_engine: Engine) -> Result<(), EngineError> {
    Ok(())
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SchedulerPresentation {
    Headless,
    Rendered,
}

#[derive(Clone, Debug, Default)]
struct Scheduler {
    phase: Option<SchedulerPhase>,
    trace: FrameTrace,
}

impl Scheduler {
    fn enter(&mut self, phase: SchedulerPhase) -> Result<(), EngineError> {
        if let Some(previous) = self.phase {
            if scheduler_phase_index(phase) <= scheduler_phase_index(previous) {
                return Err(EngineError::SchedulerPhaseOrder {
                    previous,
                    current: phase,
                });
            }
        }
        self.phase = Some(phase);
        self.trace.phases.push(phase);
        Ok(())
    }

    fn finish(self) -> FrameTrace {
        self.trace
    }
}

fn run_frame(
    engine: &mut Engine,
    input: InputSnapshot,
    presentation: SchedulerPresentation,
) -> Result<FrameResult, EngineError> {
    let mut scheduler = Scheduler::default();
    scheduler.enter(SchedulerPhase::CollectPlatformEvents)?;
    scheduler.enter(SchedulerPhase::BuildInputSnapshot)?;
    scheduler.enter(SchedulerPhase::AdvanceGameClock)?;
    scheduler.enter(SchedulerPhase::CalculateWorldQueue)?;
    let snapshot = step(&mut engine.world, &input)?;
    scheduler.enter(SchedulerPhase::ApplyDeferredOperations)?;
    scheduler.enter(SchedulerPhase::UpdateAnimationAndEffects)?;
    if presentation == SchedulerPresentation::Rendered {
        scheduler.enter(SchedulerPhase::PublishRenderSnapshot)?;
        scheduler.enter(SchedulerPhase::RenderWorld)?;
    }
    scheduler.enter(SchedulerPhase::EndFrameCallbacks)?;
    scheduler.enter(SchedulerPhase::Maintenance)?;
    Ok(FrameResult {
        snapshot,
        trace: scheduler.finish(),
    })
}

fn scheduler_phase_index(phase: SchedulerPhase) -> u8 {
    match phase {
        SchedulerPhase::CollectPlatformEvents => 0,
        SchedulerPhase::BuildInputSnapshot => 1,
        SchedulerPhase::AdvanceGameClock => 2,
        SchedulerPhase::CalculateWorldQueue => 3,
        SchedulerPhase::ApplyDeferredOperations => 4,
        SchedulerPhase::UpdateAnimationAndEffects => 5,
        SchedulerPhase::PublishRenderSnapshot => 6,
        SchedulerPhase::RenderWorld => 7,
        SchedulerPhase::EndFrameCallbacks => 8,
        SchedulerPhase::Maintenance => 9,
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
        });
    }
    let varset = first_base_path
        .as_deref()
        .map(|base_path| load_script_varset(vfs, base_path))
        .transpose()?;
    let init_states = match &varset {
        Some(varset) => resolve_handler19_init_states(
            &mission
                .clans
                .iter()
                .map(|clan| clan.anchor)
                .collect::<Vec<_>>(),
            &bundles,
            varset,
        )?,
        None => Vec::new(),
    };
    Ok(LoadedMissionScripts {
        bundles,
        varset,
        init_states,
    })
}

fn resolve_handler19_init_states(
    clan_anchors: &[[f32; 2]],
    bundles: &[MissionScriptBundle],
    varset: &MissionScriptVarSet,
) -> Result<Vec<MissionScriptInitState>, EngineError> {
    let mut states = Vec::new();
    for bundle in bundles {
        let Some(anchor) = clan_anchors.get(bundle.clan_index) else {
            return Err(EngineError::ScriptInit {
                clan_index: bundle.clan_index,
                message: "script bundle clan index is outside the decoded mission".to_string(),
            });
        };
        let input = Handler19InitInput {
            first_x87_word: handler19_x87_truncated_u32(anchor[0], bundle.clan_index, "x")?,
            second_x87_word: handler19_x87_truncated_u32(anchor[1], bundle.clan_index, "y")?,
            third_word: u32::try_from(bundle.clan_index).map_err(|_| EngineError::ScriptInit {
                clan_index: bundle.clan_index,
                message: "clan index does not fit the recovered DWORD ClanID field".to_string(),
            })?,
        };
        for event in &bundle.package.events {
            if event.name_raw.as_slice() != b"Init\0" {
                continue;
            }
            let writes: Vec<Handler19DwordWrite> = event
                .instructions
                .iter()
                .filter(|instruction| {
                    instruction.dispatch_selector() == ScriptDispatchSelector::Handler(19)
                })
                .map(|instruction| {
                    varset
                        .declarations
                        .resolve_handler19(instruction, input)
                        .map_err(|source| EngineError::ScriptInit {
                            clan_index: bundle.clan_index,
                            message: source.to_string(),
                        })
                })
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .flatten()
                .collect();
            if !writes.is_empty() {
                states.push(MissionScriptInitState {
                    clan_index: bundle.clan_index,
                    event_word: event.event_word,
                    writes,
                });
            }
        }
    }
    Ok(states)
}

fn handler19_x87_truncated_u32(
    value: f32,
    clan_index: usize,
    axis: &'static str,
) -> Result<u32, EngineError> {
    if !value.is_finite() || !(0.0..=10_000.0).contains(&value) {
        return Err(EngineError::ScriptInit {
            clan_index,
            message: format!(
                "ClanBase{axis}={value:?} is outside the recovered CreateSuperAI base range"
            ),
        });
    }
    // ai.dll's __ftol saves the x87 control word, sets rounding-control bits
    // to truncate, performs `fistp qword`, and restores the control word.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let word = value.trunc() as u32;
    Ok(word)
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
    use fparkan_vfs::{DirectoryVfs, MemoryVfs, VfsEntry, VfsMetadata};
    use std::path::{Path, PathBuf};

    #[test]
    fn load_mission_requires_vfs_and_keeps_world_unchanged_on_error() {
        let mut engine = create(
            EngineConfig {
                mode: EngineMode::Headless,
            },
            EngineServices::default(),
        )
        .expect("engine");
        let before = step_headless(&mut engine, InputSnapshot).expect("step");
        let err = load_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/Autodemo.00/data.tma".to_string(),
            },
        )
        .expect_err("missing VFS");
        assert!(matches!(err, EngineError::MissingVfs));
        let after = step_headless(&mut engine, InputSnapshot).expect("step");
        assert_eq!(before.snapshot.objects, after.snapshot.objects);
    }

    #[test]
    fn load_progress_reports_context_before_missing_vfs_error() {
        let mut engine = create(
            EngineConfig {
                mode: EngineMode::Headless,
            },
            EngineServices::default(),
        )
        .expect("engine");
        let mut phases = Vec::new();

        let err = load_mission_with_progress(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/Autodemo.00/data.tma".to_string(),
            },
            |phase| phases.push(phase),
        )
        .expect_err("missing VFS");

        assert!(matches!(err, EngineError::MissingVfs));
        assert_eq!(phases, vec![MissionLoadPhase::Context]);
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
        let bundles = [MissionScriptBundle {
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
                        header_words: [19, 0, 0, 0, 0, 3, 0],
                        references: vec![0, 1, 2],
                    }],
                }],
                trailing_bytes: Vec::new(),
                raw: Arc::from(&b""[..]),
            },
        }];

        let states =
            resolve_handler19_init_states(&[[500.0, 752.0], [728.0, 449.0]], &bundles, &varset)
                .expect("exact Init execution");
        assert_eq!(
            states,
            vec![MissionScriptInitState {
                clan_index: 1,
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
    }

    #[test]
    fn handler_nineteen_init_rejects_anchor_outside_recovered_base_range() {
        let error = handler19_x87_truncated_u32(-0.5, 0, "x").expect_err("negative anchor");
        assert!(matches!(
            error,
            EngineError::ScriptInit { clan_index: 0, message }
                if message.contains("outside the recovered CreateSuperAI base range")
        ));
    }

    #[test]
    fn headless_scheduler_trace_skips_presentation_phases() {
        let mut engine = create(
            EngineConfig {
                mode: EngineMode::Headless,
            },
            EngineServices::default(),
        )
        .expect("engine");

        let result = frame(&mut engine).expect("frame");

        assert_eq!(result.snapshot.tick.0, 1);
        assert_eq!(
            result.trace.phases,
            vec![
                SchedulerPhase::CollectPlatformEvents,
                SchedulerPhase::BuildInputSnapshot,
                SchedulerPhase::AdvanceGameClock,
                SchedulerPhase::CalculateWorldQueue,
                SchedulerPhase::ApplyDeferredOperations,
                SchedulerPhase::UpdateAnimationAndEffects,
                SchedulerPhase::EndFrameCallbacks,
                SchedulerPhase::Maintenance,
            ]
        );
    }

    #[test]
    fn rendered_scheduler_trace_includes_presentation_after_simulation() {
        let mut engine = create(
            EngineConfig {
                mode: EngineMode::Rendered,
            },
            EngineServices::default(),
        )
        .expect("engine");

        let result = frame(&mut engine).expect("frame");

        assert_eq!(
            result.trace.phases,
            vec![
                SchedulerPhase::CollectPlatformEvents,
                SchedulerPhase::BuildInputSnapshot,
                SchedulerPhase::AdvanceGameClock,
                SchedulerPhase::CalculateWorldQueue,
                SchedulerPhase::ApplyDeferredOperations,
                SchedulerPhase::UpdateAnimationAndEffects,
                SchedulerPhase::PublishRenderSnapshot,
                SchedulerPhase::RenderWorld,
                SchedulerPhase::EndFrameCallbacks,
                SchedulerPhase::Maintenance,
            ]
        );
    }

    #[test]
    fn scheduler_rejects_phase_regressions() {
        let mut scheduler = Scheduler::default();
        scheduler
            .enter(SchedulerPhase::BuildInputSnapshot)
            .expect("enter build input");

        assert!(matches!(
            scheduler.enter(SchedulerPhase::CollectPlatformEvents),
            Err(EngineError::SchedulerPhaseOrder {
                previous: SchedulerPhase::BuildInputSnapshot,
                current: SchedulerPhase::CollectPlatformEvents,
            })
        ));
    }

    #[test]
    fn reference_movement_rejects_non_finite_or_non_positive_input() {
        let mut engine = create(
            EngineConfig {
                mode: EngineMode::Headless,
            },
            EngineServices::default(),
        )
        .expect("engine");

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
    fn load_trace_records_preparation_before_registration_and_raw_transforms() {
        let root = licensed_root("IS");
        let vfs: Arc<dyn Vfs> = Arc::new(DirectoryVfs::new(&root));
        let mut engine = create(
            EngineConfig {
                mode: EngineMode::Headless,
            },
            EngineServices::new(vfs),
        )
        .expect("engine");

        let (loaded, trace) = load_mission_with_trace(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/Autodemo.00/data.tma".to_string(),
            },
        )
        .expect("load mission with trace");

        assert_eq!(
            trace.phases,
            vec![
                MissionLoadPhase::Context,
                MissionLoadPhase::Map,
                MissionLoadPhase::Tma,
                MissionLoadPhase::Graph,
                MissionLoadPhase::GraphVisuals,
                MissionLoadPhase::Assets,
                MissionLoadPhase::Construct,
                MissionLoadPhase::Register,
            ]
        );
        assert_eq!(trace.drafts_before_registration, loaded.object_count);
        assert_eq!(trace.registered_objects, loaded.object_count);
        assert_eq!(trace.transforms.len(), loaded.object_count);
        assert!(trace.transforms.iter().all(|transform| transform
            .orientation_raw
            .iter()
            .all(|component| component.is_finite())));
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn reference_movement_snaps_a_live_mission_object_to_terrain() {
        let root = licensed_root("IS");
        let vfs: Arc<dyn Vfs> = Arc::new(DirectoryVfs::new(&root));
        let mut engine = create(
            EngineConfig {
                mode: EngineMode::Headless,
            },
            EngineServices::new(vfs),
        )
        .expect("engine");
        load_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/Autodemo.00/data.tma".to_string(),
            },
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
                },
            ),
            (
                DenyRule::Suffix("objects.rlb"),
                MissionRequest {
                    key: "MISSIONS/CAMPAIGN/CAMPAIGN.00/Mission.01/data.tma".to_string(),
                },
            ),
        ] {
            let vfs: Arc<dyn Vfs> = Arc::new(DenyVfs {
                inner: DirectoryVfs::new(&root),
                denied,
            });
            let mut engine = create(
                EngineConfig {
                    mode: EngineMode::Headless,
                },
                EngineServices::new(vfs),
            )
            .expect("engine");
            let before = step_headless(&mut engine, InputSnapshot).expect("before");
            let err = load_mission(&mut engine, mission).expect_err("load error");
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
        let mut engine = create(
            EngineConfig {
                mode: EngineMode::Headless,
            },
            EngineServices::new(vfs),
        )
        .expect("engine");
        let before = step_headless(&mut engine, InputSnapshot).expect("before");

        let err = load_mission_with_options_and_progress(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/CAMPAIGN/CAMPAIGN.00/Mission.01/data.tma".to_string(),
            },
            MissionLoadOptions {
                fail_after_registered_objects: Some(1),
                ..MissionLoadOptions::default()
            },
            None,
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
            let mut engine = create(
                EngineConfig {
                    mode: EngineMode::Headless,
                },
                EngineServices::new(vfs),
            )
            .expect("engine");
            let loaded = load_mission(
                &mut engine,
                MissionRequest {
                    key: case.1.to_string(),
                },
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
    fn licensed_corpora_load_all_mission_foundations() {
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
        let mut engine = create(
            EngineConfig {
                mode: EngineMode::Headless,
            },
            EngineServices::new(vfs),
        )
        .expect("engine");
        let loaded = load_mission(
            &mut engine,
            MissionRequest {
                key: case.mission.to_string(),
            },
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
            let mut engine = create(
                EngineConfig {
                    mode: EngineMode::Headless,
                },
                EngineServices::new(vfs.clone()),
            )
            .expect("engine");
            let loaded = load_mission(&mut engine, MissionRequest { key: mission })
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

    #[derive(Clone, Copy)]
    enum DenyRule {
        Suffix(&'static str),
    }

    struct DenyVfs {
        inner: DirectoryVfs,
        denied: DenyRule,
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
