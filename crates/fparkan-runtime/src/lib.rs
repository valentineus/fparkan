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
    derive_mission_land_paths, extend_graph_report_with_visual_dependencies, prepare_terrain_world,
    AssetError as AssetPreparationError, AssetId, AssetManager, BuildCategory, MissionAssetPlan,
    MissionDocument, MissionError, MissionTerrainPaths, NresError, PreparedVisual,
    TerrainFormatError, TerrainPreparationError, TerrainWorld, TmaProfile,
};
use fparkan_path::{normalize_relative, NormalizedPath, PathError, PathPolicy};
use fparkan_prototype::{
    build_prototype_graph_report, PrototypeGraph, PrototypeGraphFailure, PrototypeGraphReport,
};
use fparkan_resource::{resource_name, CachedResourceRepository, ResourceRepository};
use fparkan_vfs::{Vfs, VfsError};
use fparkan_world::{
    construct_object, new as new_world, register_object, step, InputSnapshot, ObjectDraft,
    OriginalObjectId, World, WorldConfig, WorldSnapshot,
};
use std::num::NonZeroUsize;
use std::sync::Arc;

pub use fparkan_assets::MissionAssets;

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
    /// Prepare all reachable visual/resource dependencies.
    Assets,
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
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct MissionLoadOptions {
    fail_after_registered_objects: Option<usize>,
    asset_scope: MissionAssetScope,
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
    let repository = CachedResourceRepository::new(vfs.clone());
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
    record_load_phase(&mut trace, &mut on_phase, MissionLoadPhase::Assets);
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
        })
        .collect();
    let mut new_runtime_world = new_world(WorldConfig);
    let mut handles = Vec::with_capacity(mission.objects.len());
    record_load_phase(&mut trace, &mut on_phase, MissionLoadPhase::Construct);
    for (index, _object) in mission.objects.iter().enumerate() {
        let original_id = u32::try_from(index).ok().map(OriginalObjectId);
        let handle = construct_object(&mut new_runtime_world, ObjectDraft { original_id })?;
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

fn read_vfs(vfs: &Arc<dyn Vfs>, path: &NormalizedPath) -> Result<Arc<[u8]>, EngineError> {
    vfs.read(path).map_err(|source| EngineError::Vfs {
        path: path.as_str().to_string(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_vfs::{DirectoryVfs, VfsEntry, VfsMetadata};
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

            assert_eq!(drafts.len(), loaded.object_count);
            assert!(drafts.iter().any(|draft| !draft.visual_ids.is_empty()));
            for (index, draft) in drafts.iter().enumerate() {
                assert_eq!(
                    draft.original_id,
                    u32::try_from(index).ok().map(OriginalObjectId)
                );
                assert_eq!(draft.visual_ids, assets.visuals_for_object(index));
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
