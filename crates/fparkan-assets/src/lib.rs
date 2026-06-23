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
//! Asset manager ports and transactional preparation models.

use fparkan_material::{decode_wear, resolve_material, MaterialError, WEAR_KIND};
use fparkan_mission_format::{decode_tma, decode_tma_land_path};
pub use fparkan_mission_format::{LpString, MissionDocument, MissionError, TmaProfile};
use fparkan_msh::{decode_msh, validate_msh, MshError};
use fparkan_nres::{decode as decode_nres, ReadProfile};
pub use fparkan_nres::{NresDocument, NresError};
use fparkan_path::{normalize_relative, NormalizedPath, PathError, PathPolicy, ResourceName};
use fparkan_prototype::{
    EffectivePrototype, PrototypeGeometry, PrototypeGraph, PrototypeGraphEdge,
    PrototypeGraphFailure, PrototypeGraphNodeKind, PrototypeGraphProvenance, PrototypeGraphReport,
    PrototypeGraphRequiredness,
};
use fparkan_resource::{ResourceError, ResourceKey, ResourceRepository};
pub use fparkan_terrain::{TerrainError, TerrainWorld};
use fparkan_terrain_format::{decode_build_dat, decode_land_map, decode_land_msh};
pub use fparkan_terrain_format::{BuildCategory, TerrainFormatError};
use fparkan_texm::{decode_texm, TexmError};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::sync::Arc;

const TEXTURES_ARCHIVE: &str = "textures.lib";
const LIGHTMAP_ARCHIVE: &str = "lightmap.lib";

/// Canonical terrain archive paths derived from a mission land reference.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MissionTerrainPaths {
    /// Landscape mesh archive path.
    pub land_msh: NormalizedPath,
    /// Landscape map archive path.
    pub land_map: NormalizedPath,
}

/// Terrain loading errors that include runtime world construction failures.
#[derive(Debug)]
pub enum TerrainPreparationError {
    /// Format error while decoding terrain documents.
    Decode(TerrainFormatError),
    /// Runtime terrain constructor failed.
    Runtime(TerrainError),
}

impl std::fmt::Display for TerrainPreparationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Decode(source) => write!(f, "{source}"),
            Self::Runtime(source) => write!(f, "{source}"),
        }
    }
}

impl std::error::Error for TerrainPreparationError {}

impl From<TerrainFormatError> for TerrainPreparationError {
    fn from(source: TerrainFormatError) -> Self {
        Self::Decode(source)
    }
}

impl From<TerrainError> for TerrainPreparationError {
    fn from(source: TerrainError) -> Self {
        Self::Runtime(source)
    }
}

/// Decodes a mission file bytes payload with a typed profile.
///
/// # Errors
///
/// Returns [`MissionError`] when the mission payload is malformed for the
/// selected profile.
pub fn decode_mission_payload(
    bytes: Arc<[u8]>,
    profile: TmaProfile,
) -> Result<MissionDocument, MissionError> {
    decode_tma(bytes, profile)
}

/// Reads only the mission land path from raw TMA bytes.
///
/// # Errors
///
/// Returns [`MissionError`] when the mission header or land path record cannot
/// be decoded.
pub fn decode_mission_land_path(
    bytes: &[u8],
    profile: TmaProfile,
) -> Result<LpString, MissionError> {
    decode_tma_land_path(bytes, profile)
}

/// Builds canonical mission terrain paths from the mission `Land` reference.
///
/// # Errors
///
/// Returns [`PathError`] when the mission land reference is not a strict
/// relative legacy path.
pub fn derive_mission_land_paths(land_path: &LpString) -> Result<MissionTerrainPaths, PathError> {
    let normalized = normalize_relative(&land_path.raw, PathPolicy::StrictLegacy)?;
    let Some((parent, _stem)) = normalized.as_str().rsplit_once('/') else {
        return Err(PathError::Empty);
    };
    let land_msh = normalize_relative(
        format!("{parent}/Land.msh").as_bytes(),
        PathPolicy::StrictLegacy,
    )?;
    let land_map = normalize_relative(
        format!("{parent}/Land.map").as_bytes(),
        PathPolicy::StrictLegacy,
    )?;
    Ok(MissionTerrainPaths { land_msh, land_map })
}

/// Decodes compatible `NRes` payload for terrain/document loading.
///
/// # Errors
///
/// Returns [`NresError`] when the payload is not a compatible `NRes` archive.
pub fn decode_nres_payload(
    bytes: Arc<[u8]>,
) -> Result<fparkan_nres::NresDocument, fparkan_nres::NresError> {
    decode_nres(bytes, ReadProfile::Compatible)
}

/// Decodes terrain documents and builds immutable terrain state.
///
/// # Errors
///
/// Returns [`TerrainPreparationError`] when terrain documents are malformed or
/// cannot be converted into runtime terrain state.
pub fn prepare_terrain_world(
    land_msh_nres: &fparkan_nres::NresDocument,
    land_map_nres: &fparkan_nres::NresDocument,
    build_dat: &[u8],
) -> Result<(TerrainWorld, Vec<BuildCategory>), TerrainPreparationError> {
    let land_msh = decode_land_msh(land_msh_nres)?;
    let land_map = decode_land_map(land_map_nres)?;
    let build_categories = decode_build_dat(build_dat)?;
    let world = TerrainWorld::from_land_assets(&land_msh, &land_map)?;
    Ok((world, build_categories))
}

/// Stable typed identifier for a prepared asset.
#[derive(Debug)]
pub struct AssetId<T> {
    raw: u64,
    marker: PhantomData<T>,
}

impl<T> Clone for AssetId<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for AssetId<T> {}

impl<T> PartialEq for AssetId<T> {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}

impl<T> Eq for AssetId<T> {}

impl<T> Hash for AssetId<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.raw.hash(state);
    }
}

impl<T> AssetId<T> {
    /// Creates an asset id from a stable raw value.
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self {
            raw,
            marker: PhantomData,
        }
    }

    /// Returns the stable raw id.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.raw
    }
}

/// CPU-side data needed before a visual can be handed to a renderer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedVisual {
    /// Stable id derived from the prototype geometry key.
    pub id: AssetId<PreparedVisual>,
    /// Optional mesh resource backing the visual.
    pub mesh: Option<ResourceKey>,
    /// Number of validated model nodes.
    pub model_nodes: usize,
    /// Number of validated material slots on the model.
    pub model_slots: usize,
    /// Number of validated render batches.
    pub model_batches: usize,
    /// Number of WEAR material slots resolved through MAT0.
    pub material_count: usize,
    /// Typed material IDs available from the resolved visual.
    pub material_ids: Vec<AssetId<PreparedMaterial>>,
    /// Number of texture phase requests decoded as TEXM.
    pub texture_count: usize,
    /// Number of lightmap requests decoded as TEXM.
    pub lightmap_count: usize,
}

/// CPU-side data needed before a material can be handed to a renderer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedMaterial {
    /// Stable id derived from the visual and material selector.
    pub id: AssetId<PreparedMaterial>,
    /// Parsed material key.
    pub name: ResourceName,
}

impl PreparedVisual {
    /// Returns the primary material id, if any.
    #[must_use]
    pub fn primary_material_id(&self) -> Option<AssetId<PreparedMaterial>> {
        self.material_ids.first().copied()
    }
}

/// Immutable prepared mission assets for rendering and game setup.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MissionAssets {
    /// Visuals prepared for all reachable prototype requests.
    pub visuals: Vec<PreparedVisual>,
    /// Visual ids available for each mission object index.
    pub object_visuals: Vec<Vec<AssetId<PreparedVisual>>>,
}

impl MissionAssets {
    /// Returns how many visuals were prepared.
    #[must_use]
    pub fn visual_count(&self) -> usize {
        self.visuals.len()
    }

    /// Returns all visuals for a mission object index.
    #[must_use]
    pub fn visuals_for_object(&self, object_index: usize) -> &[AssetId<PreparedVisual>] {
        self.object_visuals
            .get(object_index)
            .map_or(&[], |values| values.as_slice())
    }

    /// Returns the first visual for a mission object index.
    #[must_use]
    pub fn visual_for_object(&self, object_index: usize) -> Option<AssetId<PreparedVisual>> {
        self.visuals_for_object(object_index).first().copied()
    }

    /// Finds a visual by prepared id.
    #[must_use]
    pub fn visual_by_id(&self, id: AssetId<PreparedVisual>) -> Option<&PreparedVisual> {
        self.visuals.iter().find(|visual| visual.id == id)
    }

    /// Converts mission assets into a coarse mission plan.
    #[must_use]
    pub fn to_plan(&self) -> MissionAssetPlan {
        let visual_count = self.visuals.len();
        let model_count = self
            .visuals
            .iter()
            .filter(|visual| visual.mesh.is_some())
            .count();
        let material_count = self
            .visuals
            .iter()
            .map(|visual| visual.material_count)
            .sum();
        let texture_count = self.visuals.iter().map(|visual| visual.texture_count).sum();
        let lightmap_count = self
            .visuals
            .iter()
            .map(|visual| visual.lightmap_count)
            .sum();
        MissionAssetPlan {
            visual_count,
            model_count,
            material_count,
            texture_count,
            lightmap_count,
        }
    }
}

/// A transactional mission asset preparation plan.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MissionAssetPlan {
    /// Number of visual prototypes in the plan.
    pub visual_count: usize,
    /// Number of mesh-backed visuals.
    pub model_count: usize,
    /// Number of material slot requests.
    pub material_count: usize,
    /// Number of texture phase requests.
    pub texture_count: usize,
    /// Number of lightmap requests.
    pub lightmap_count: usize,
}

/// Coarse CPU-side asset budgets.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AssetBudgets {
    /// Bytes parsed from source resource payloads.
    pub parsed_bytes: u64,
}

/// Errors raised while preparing CPU-side assets.
#[derive(Debug)]
pub enum AssetError {
    /// A required cross-resource dependency was not found.
    MissingDependency(String),
    /// A prototype did not describe a usable visual.
    InvalidPrototype(String),
    /// A repository operation failed.
    Resource {
        /// Human context for the operation.
        context: String,
        /// Concrete repository source error.
        source: Box<ResourceError>,
    },
    /// MSH parsing or validation failed.
    Msh(MshError),
    /// WEAR/MAT0 parsing or resolution failed.
    Material(MaterialError),
    /// TEXM parsing failed.
    Texture(TexmError),
    /// `NRes` decoding failed.
    Nres(NresError),
}

impl fmt::Display for AssetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingDependency(value) => write!(f, "missing dependency: {value}"),
            Self::InvalidPrototype(value) => write!(f, "invalid prototype: {value}"),
            Self::Resource { context, source } => {
                if context.is_empty() {
                    write!(f, "resource error: {source}")
                } else {
                    write!(f, "resource error ({context}): {source}")
                }
            }
            Self::Msh(source) => write!(f, "msh error: {source}"),
            Self::Material(source) => write!(f, "material error: {source}"),
            Self::Texture(source) => write!(f, "texture error: {source}"),
            Self::Nres(source) => write!(f, "nres error: {source}"),
        }
    }
}

impl std::error::Error for AssetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Resource { source, .. } => Some(source.as_ref()),
            Self::Msh(source) => Some(source),
            Self::Material(source) => Some(source),
            Self::Texture(source) => Some(source),
            Self::Nres(source) => Some(source),
            Self::MissingDependency(_) | Self::InvalidPrototype(_) => None,
        }
    }
}

/// Port implemented by typed asset loaders.
pub trait AssetLoader<T> {
    /// Loads an asset for the given resource key.
    ///
    /// # Errors
    ///
    /// Returns [`AssetError`] when the resource cannot be resolved or decoded.
    fn load(&self, key: &ResourceKey) -> Result<Arc<T>, AssetError>;
}

/// Minimal asset manager façade over an immutable resource repository.
#[derive(Debug)]
pub struct AssetManager<R> {
    repository: R,
}

impl<R> AssetManager<R> {
    /// Creates a manager backed by the given repository.
    #[must_use]
    pub const fn new(repository: R) -> Self {
        Self { repository }
    }

    /// Returns the backing repository.
    #[must_use]
    pub const fn repository(&self) -> &R {
        &self.repository
    }
}

impl<R: ResourceRepository> AssetManager<R> {
    /// Prepares one prototype visual using the manager repository.
    ///
    /// # Errors
    ///
    /// Returns [`AssetError`] if any model, material, texture, or lightmap
    /// dependency is missing or malformed.
    pub fn prepare_visual(&self, proto: &EffectivePrototype) -> Result<PreparedVisual, AssetError> {
        prepare_visual_with_repository(&self.repository, proto)
    }

    /// Builds mission assets from resolved prototypes.
    ///
    /// # Errors
    ///
    /// Returns [`AssetError`] if any visual dependency is missing or malformed.
    pub fn prepare_mission_assets(
        &self,
        root_prototype_spans: &[std::ops::Range<usize>],
        prototypes: &[EffectivePrototype],
    ) -> Result<MissionAssets, AssetError> {
        prepare_mission_assets_with_repository(&self.repository, root_prototype_spans, prototypes)
    }

    /// Builds a mission plan by preparing each resolved prototype.
    ///
    /// # Errors
    ///
    /// Returns [`AssetError`] if any visual dependency is missing or malformed.
    pub fn build_mission_asset_plan(
        &self,
        prototypes: &[EffectivePrototype],
    ) -> Result<MissionAssetPlan, AssetError> {
        build_mission_asset_plan_with_repository(&self.repository, prototypes)
    }
}

/// Produces a count-only plan from a prototype graph.
#[must_use]
pub fn build_mission_asset_plan(graph: &PrototypeGraph) -> MissionAssetPlan {
    let visual_count = graph
        .nodes
        .iter()
        .filter(|node| node.kind == PrototypeGraphNodeKind::Prototype)
        .count();
    MissionAssetPlan {
        visual_count,
        ..MissionAssetPlan::default()
    }
}

/// Builds a fully validated CPU-side mission asset plan.
///
/// # Errors
///
/// Returns [`AssetError`] if any reachable visual dependency is missing or
/// malformed.
pub fn build_mission_asset_plan_with_repository<R: ResourceRepository>(
    repository: &R,
    prototypes: &[EffectivePrototype],
) -> Result<MissionAssetPlan, AssetError> {
    let full_span = 0..prototypes.len();
    let mission_assets = prepare_mission_assets_with_repository(
        repository,
        std::slice::from_ref(&full_span),
        prototypes,
    )?;
    Ok(mission_assets.to_plan())
}

/// Builds immutable mission assets from resolved prototypes.
///
/// # Errors
///
/// Returns [`AssetError`] if any visual dependency is missing or malformed.
pub fn prepare_mission_assets_with_repository<R: ResourceRepository>(
    repository: &R,
    root_prototype_spans: &[std::ops::Range<usize>],
    prototypes: &[EffectivePrototype],
) -> Result<MissionAssets, AssetError> {
    if prototypes.is_empty() {
        return Ok(MissionAssets::default());
    }
    let mut visual_index_by_id: HashMap<AssetId<PreparedVisual>, PreparedVisualSignature> =
        HashMap::new();
    let mut material_signature_by_id: HashMap<AssetId<PreparedMaterial>, Vec<u8>> = HashMap::new();
    let mut visuals = Vec::new();
    let mut prototype_visual_ids = Vec::with_capacity(prototypes.len());

    for proto in prototypes {
        let visual_id = AssetId::new(stable_visual_id(proto));
        let signature = prepared_visual_signature(proto);
        match visual_index_by_id.get(&visual_id) {
            Some(existing) if existing != &signature => {
                return Err(AssetError::InvalidPrototype(
                    "stable visual id collision between unrelated prototypes".to_string(),
                ));
            }
            Some(_) => {}
            None => {
                visual_index_by_id.insert(visual_id, signature);
                let visual = prepare_visual_with_repository_internal(
                    repository,
                    proto,
                    Some(&mut material_signature_by_id),
                )?;
                if visual.id != visual_id {
                    // Defensive check. stable IDs are deterministic for the same inputs.
                    return Err(AssetError::InvalidPrototype(
                        "prepared visual id changed during preparation".to_string(),
                    ));
                }
                visuals.push(visual);
            }
        }
        prototype_visual_ids.push(visual_id);
    }

    let mut object_visuals = Vec::with_capacity(root_prototype_spans.len());
    for (root_index, span) in root_prototype_spans.iter().enumerate() {
        if span.start > span.end || span.end > prototype_visual_ids.len() {
            return Err(AssetError::InvalidPrototype(format!(
                "invalid prototype span for mission object {root_index}: {span:?}"
            )));
        }
        let mut ids = Vec::new();
        let mut dedup = HashSet::new();
        for index in span.clone() {
            let visual_id = prototype_visual_ids[index];
            if dedup.insert(visual_id) {
                ids.push(visual_id);
            }
        }
        object_visuals.push(ids);
    }

    Ok(MissionAssets {
        visuals,
        object_visuals,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PreparedVisualSignature {
    Mesh {
        archive: String,
        name: Vec<u8>,
        type_id: Option<u32>,
        dependency_count: usize,
    },
    NonGeometric {
        dependency_count: usize,
    },
}

fn prepared_visual_signature(proto: &EffectivePrototype) -> PreparedVisualSignature {
    match &proto.geometry {
        PrototypeGeometry::Mesh(key) => PreparedVisualSignature::Mesh {
            archive: key.archive.as_str().to_string(),
            name: key.name.0.clone(),
            type_id: key.type_id,
            dependency_count: proto.dependencies.len(),
        },
        PrototypeGeometry::NonGeometric => PreparedVisualSignature::NonGeometric {
            dependency_count: proto.dependencies.len(),
        },
    }
}

/// Extends a prototype dependency report with visual dependency failures.
///
/// This function validates WEAR/material/TEXM/LIGHTMAP resolution for each resolved
/// prototype without constructing full immutable assets.
pub fn extend_graph_report_with_visual_dependencies<R: ResourceRepository>(
    repository: &R,
    report: &mut PrototypeGraphReport,
    graph: &PrototypeGraph,
    prototypes: &[EffectivePrototype],
) {
    let texture_archive = parse_path(TEXTURES_ARCHIVE).ok();
    let lightmap_archive = parse_path(LIGHTMAP_ARCHIVE).ok();

    for (prototype_index, prototype) in prototypes.iter().enumerate() {
        let PrototypeGeometry::Mesh(mesh) = &prototype.geometry else {
            continue;
        };
        report.mesh_dependency_count += prototype.dependencies.len();
        report.wear_request_count += 1;

        match resolve_wear_table(repository, mesh) {
            Ok(table) => {
                report.wear_resolved_count += 1;
                report.material_slot_count += table.entries.len();
                for (material_index, _entry) in table.entries.iter().enumerate() {
                    let Ok(material_index) = u16::try_from(material_index) else {
                        push_visual_failure(
                            report,
                            graph,
                            prototype_index,
                            mesh.name.0.clone(),
                            PrototypeGraphEdge::WearToMaterial,
                            PrototypeGraphRequiredness::Required,
                            "material index does not fit archive format",
                        );
                        continue;
                    };
                    match resolve_material(repository, &table, material_index) {
                        Ok(material) => {
                            report.material_resolved_count += 1;
                            for texture in material.document.texture_requests() {
                                report.texture_request_count += 1;
                                match resolve_texm_from_candidates(
                                    repository,
                                    &texture,
                                    [texture_archive.as_ref(), lightmap_archive.as_ref()],
                                ) {
                                    Ok(()) => report.texture_resolved_count += 1,
                                    Err(message) => {
                                        let message = message.to_string();
                                        push_visual_failure(
                                            report,
                                            graph,
                                            prototype_index,
                                            texture.0,
                                            PrototypeGraphEdge::MaterialToTexture,
                                            PrototypeGraphRequiredness::Required,
                                            &message,
                                        );
                                    }
                                }
                            }
                        }
                        Err(message) => push_visual_failure(
                            report,
                            graph,
                            prototype_index,
                            mesh.name.0.clone(),
                            PrototypeGraphEdge::WearToMaterial,
                            PrototypeGraphRequiredness::Required,
                            &message.to_string(),
                        ),
                    }
                }
                for lightmap in &table.lightmaps {
                    report.lightmap_request_count += 1;
                    match resolve_texm_from_candidates(
                        repository,
                        &lightmap.lightmap,
                        [lightmap_archive.as_ref(), texture_archive.as_ref()],
                    ) {
                        Ok(()) => report.lightmap_resolved_count += 1,
                        Err(message) => {
                            let message = message.to_string();
                            push_visual_failure(
                                report,
                                graph,
                                prototype_index,
                                lightmap.lightmap.0.clone(),
                                PrototypeGraphEdge::WearToLightmap,
                                PrototypeGraphRequiredness::Required,
                                &message,
                            );
                        }
                    }
                }
            }
            Err(message) => push_visual_failure(
                report,
                graph,
                prototype_index,
                mesh.name.0.clone(),
                PrototypeGraphEdge::MeshToWear,
                PrototypeGraphRequiredness::Required,
                &message.to_string(),
            ),
        }
    }
}

/// Validates a prototype visual without resolving cross-resource dependencies.
///
/// This is useful for tests and API callers that only need a stable visual id.
///
/// # Errors
///
/// Returns [`AssetError`] when the prototype geometry is malformed.
pub fn prepare_visual(proto: &EffectivePrototype) -> Result<PreparedVisual, AssetError> {
    let id = stable_visual_id(proto);
    let mesh = match &proto.geometry {
        PrototypeGeometry::Mesh(key) => Some(key.clone()),
        PrototypeGeometry::NonGeometric => None,
    };

    Ok(PreparedVisual {
        id: AssetId::new(id),
        mesh,
        model_nodes: 0,
        model_slots: 0,
        model_batches: 0,
        material_count: 0,
        material_ids: Vec::new(),
        texture_count: 0,
        lightmap_count: 0,
    })
}

/// Prepares one visual and validates all CPU-side resource dependencies.
///
/// # Errors
///
/// Returns [`AssetError`] if the model, WEAR table, MAT0 materials, texture
/// phases, or lightmaps cannot be resolved and decoded.
pub fn prepare_visual_with_repository<R: ResourceRepository>(
    repository: &R,
    proto: &EffectivePrototype,
) -> Result<PreparedVisual, AssetError> {
    prepare_visual_with_repository_internal(repository, proto, None)
}

fn prepare_visual_with_repository_internal<R: ResourceRepository>(
    repository: &R,
    proto: &EffectivePrototype,
    mut material_signature_by_id: Option<&mut HashMap<AssetId<PreparedMaterial>, Vec<u8>>>,
) -> Result<PreparedVisual, AssetError> {
    let PrototypeGeometry::Mesh(mesh_key) = &proto.geometry else {
        return prepare_visual(proto);
    };

    let nres = decode_nres(
        read_key(repository, mesh_key, Some("mesh"))?,
        ReadProfile::Compatible,
    )
    .map_err(AssetError::Nres)?;
    let msh_document = decode_msh(&nres).map_err(AssetError::Msh)?;
    let model = validate_msh(&msh_document).map_err(AssetError::Msh)?;

    let wear_name = sibling_name(mesh_key, "wea")?;
    let wear_key = ResourceKey {
        archive: mesh_key.archive.clone(),
        name: wear_name,
        type_id: Some(WEAR_KIND),
    };
    let wear = decode_wear(&read_key(repository, &wear_key, Some("wear"))?)
        .map_err(AssetError::Material)?;

    let mut material_count = 0;
    let mut material_ids = Vec::with_capacity(wear.entries.len());
    let mut texture_count = 0;
    let mut lightmap_count = 0;
    for material_index in 0..wear.entries.len() {
        let material_index = u16::try_from(material_index).map_err(|_| {
            AssetError::InvalidPrototype("material index does not fit archive format".to_string())
        })?;
        let material =
            resolve_material(repository, &wear, material_index).map_err(AssetError::Material)?;
        material_count += 1;
        let material_id = AssetId::new(stable_material_id(proto, material_index, &material.name));
        material_ids.push(material_id);
        if let Some(registry) = material_signature_by_id.as_deref_mut() {
            match registry.get(&material_id) {
                Some(existing_name) => {
                    if existing_name != &material.name.0 {
                        return Err(AssetError::InvalidPrototype(
                            "stable material id collision between unrelated materials".to_string(),
                        ));
                    }
                }
                None => {
                    registry.insert(material_id, material.name.0.clone());
                }
            }
        }

        for texture in material.document.texture_requests() {
            resolve_texture(repository, &texture)?;
            texture_count += 1;
        }
    }

    for lightmap in &wear.lightmaps {
        resolve_lightmap(repository, &lightmap.lightmap)?;
        lightmap_count += 1;
    }

    Ok(PreparedVisual {
        id: AssetId::new(stable_visual_id(proto)),
        mesh: Some(mesh_key.clone()),
        model_nodes: model.node_count,
        model_slots: model.slots.len(),
        model_batches: model.batches.len(),
        material_count,
        material_ids,
        texture_count,
        lightmap_count,
    })
}

fn read_key<R: ResourceRepository>(
    repository: &R,
    key: &ResourceKey,
    label: Option<&str>,
) -> Result<Arc<[u8]>, AssetError> {
    let label = label.unwrap_or("asset");
    let archive = repository
        .open_archive(&key.archive)
        .map_err(|err| map_resource_error(label, key, err))?;
    let handle = repository
        .find(archive, &key.name)
        .map_err(|err| map_resource_error(label, key, err))?
        .ok_or_else(|| AssetError::MissingDependency(format!("{label}: {key:?}")))?;
    let bytes = repository
        .read(handle)
        .map_err(|err| map_resource_error(label, key, err))?;
    Ok(Arc::from(bytes.into_owned()))
}

fn map_resource_error(label: &str, key: &ResourceKey, source: ResourceError) -> AssetError {
    AssetError::Resource {
        context: format!(
            "{label}: archive={} entry={}",
            key.archive.as_str(),
            String::from_utf8_lossy(&key.name.0),
        ),
        source: Box::new(source),
    }
}

fn resolve_wear_table<R: ResourceRepository>(
    repository: &R,
    mesh: &ResourceKey,
) -> Result<fparkan_material::WearTable, AssetError> {
    let archive = repository
        .open_archive(&mesh.archive)
        .map_err(|err| map_resource_error("wear", mesh, err))?;
    let wear_name = sibling_name(mesh, "wea")?;
    let handle = repository
        .find(archive, &wear_name)
        .map_err(|err| {
            map_resource_error(
                "wear",
                &ResourceKey {
                    archive: mesh.archive.clone(),
                    name: wear_name.clone(),
                    type_id: Some(WEAR_KIND),
                },
                err,
            )
        })?
        .ok_or_else(|| {
            AssetError::MissingDependency(format!(
                "missing WEAR entry {}",
                String::from_utf8_lossy(&wear_name.0)
            ))
        })?;
    let info = repository.entry_info(handle).map_err(|err| {
        map_resource_error(
            "wear",
            &ResourceKey {
                archive: mesh.archive.clone(),
                name: wear_name.clone(),
                type_id: Some(WEAR_KIND),
            },
            err,
        )
    })?;
    if info.key.type_id != Some(WEAR_KIND) {
        return Err(AssetError::InvalidPrototype(format!(
            "entry {} is not WEAR",
            String::from_utf8_lossy(&wear_name.0)
        )));
    }
    let bytes = repository
        .read(handle)
        .map_err(|err| {
            map_resource_error(
                "wear",
                &ResourceKey {
                    archive: mesh.archive.clone(),
                    name: wear_name.clone(),
                    type_id: Some(WEAR_KIND),
                },
                err,
            )
        })?
        .into_owned();
    decode_wear(&bytes).map_err(AssetError::Material)
}

fn resolve_texm_from_candidates<'a, R: ResourceRepository>(
    repository: &R,
    texture: &ResourceName,
    candidates: impl IntoIterator<Item = Option<&'a NormalizedPath>>,
) -> Result<(), AssetError> {
    let mut missing_archive = false;
    for path in candidates.into_iter().flatten() {
        let key = ResourceKey {
            archive: path.to_owned(),
            name: texture.clone(),
            type_id: None,
        };
        let archive = match repository.open_archive(path) {
            Ok(archive) => archive,
            Err(ResourceError::MissingArchive) => {
                missing_archive = true;
                continue;
            }
            Err(err) => return Err(map_resource_error("texm", &key, err)),
        };
        let Some(handle) = repository
            .find(archive, texture)
            .map_err(|err| map_resource_error("texm", &key, err))?
        else {
            continue;
        };
        let bytes = repository
            .read(handle)
            .map_err(|err| map_resource_error("texm", &key, err))?
            .into_owned();
        decode_texm(Arc::from(bytes)).map_err(AssetError::Texture)?;
        return Ok(());
    }
    if missing_archive {
        Err(AssetError::MissingDependency(format!(
            "texm archive missing for {}",
            String::from_utf8_lossy(&texture.0)
        )))
    } else {
        Err(AssetError::MissingDependency(format!(
            "missing texm {}",
            String::from_utf8_lossy(&texture.0)
        )))
    }
}

fn push_visual_failure(
    report: &mut PrototypeGraphReport,
    graph: &PrototypeGraph,
    prototype_index: usize,
    resource_raw: Vec<u8>,
    edge: PrototypeGraphEdge,
    requiredness: PrototypeGraphRequiredness,
    message: &str,
) {
    let root_index = root_index_for_prototype(graph, prototype_index);
    let parent_edge = parent_edge_for_failure(graph, prototype_index, edge);
    let dependency = mesh_dependency_resource(graph, prototype_index);
    report.failures.push(PrototypeGraphFailure {
        root_index,
        resource_raw: resource_raw.clone(),
        edge,
        message: message.to_string(),
        requiredness,
        provenance: Some(PrototypeGraphProvenance {
            root_index,
            parent_edge,
            archive: dependency.map(|resource| resource.archive.as_str().to_string()),
            resource: Some(resource_raw),
            span: None,
        }),
    });
}

fn root_index_for_prototype(graph: &PrototypeGraph, prototype_index: usize) -> usize {
    for (root_index, span) in graph.root_prototype_request_spans.iter().enumerate() {
        if span.start <= prototype_index && prototype_index < span.end {
            return root_index;
        }
    }
    0
}

fn parent_edge_for_failure(
    graph: &PrototypeGraph,
    prototype_index: usize,
    edge: PrototypeGraphEdge,
) -> Option<fparkan_prototype::PrototypeGraphEdgeId> {
    let prototype_node_id = prototype_node_id(graph, prototype_index)?;
    match edge {
        PrototypeGraphEdge::MeshToWear
        | PrototypeGraphEdge::WearToMaterial
        | PrototypeGraphEdge::MaterialToTexture
        | PrototypeGraphEdge::WearToLightmap => mesh_edge_id(graph, prototype_node_id)
            .or_else(|| root_edge_id(graph, prototype_node_id)),
        _ => root_edge_id(graph, prototype_node_id),
    }
}

fn prototype_node_id(
    graph: &PrototypeGraph,
    prototype_index: usize,
) -> Option<fparkan_prototype::PrototypeGraphNodeId> {
    graph
        .nodes
        .iter()
        .filter(|node| node.kind == PrototypeGraphNodeKind::Prototype)
        .nth(prototype_index)
        .map(|node| node.id)
}

fn root_edge_id(
    graph: &PrototypeGraph,
    prototype_node: fparkan_prototype::PrototypeGraphNodeId,
) -> Option<fparkan_prototype::PrototypeGraphEdgeId> {
    graph
        .edges
        .iter()
        .find(|edge| {
            edge.to == prototype_node
                && matches!(
                    edge.kind,
                    fparkan_prototype::PrototypeGraphEdgeKind::MissionToRoot
                        | fparkan_prototype::PrototypeGraphEdgeKind::UnitDatToComponent
                )
        })
        .map(|edge| edge.id)
}

fn mesh_edge_id(
    graph: &PrototypeGraph,
    prototype_node: fparkan_prototype::PrototypeGraphNodeId,
) -> Option<fparkan_prototype::PrototypeGraphEdgeId> {
    graph
        .edges
        .iter()
        .find(|edge| {
            edge.from == prototype_node
                && matches!(
                    edge.kind,
                    fparkan_prototype::PrototypeGraphEdgeKind::PrototypeToMesh
                )
        })
        .map(|edge| edge.id)
}

fn mesh_dependency_resource(
    graph: &PrototypeGraph,
    prototype_index: usize,
) -> Option<&fparkan_resource::ResourceKey> {
    let prototype_node = prototype_node_id(graph, prototype_index)?;
    let mesh_node = graph
        .edges
        .iter()
        .find(|edge| {
            edge.from == prototype_node
                && matches!(
                    edge.kind,
                    fparkan_prototype::PrototypeGraphEdgeKind::PrototypeToMesh
                )
        })?
        .to;
    graph
        .nodes
        .iter()
        .find(|node| node.id == mesh_node)
        .and_then(|node| node.resource.as_ref())
}

fn resolve_texture<R: ResourceRepository>(
    repository: &R,
    name: &ResourceName,
) -> Result<(), AssetError> {
    resolve_texm(repository, name, TEXTURES_ARCHIVE, "texture")
}

fn resolve_lightmap<R: ResourceRepository>(
    repository: &R,
    name: &ResourceName,
) -> Result<(), AssetError> {
    resolve_texm(repository, name, LIGHTMAP_ARCHIVE, "lightmap")
}

fn resolve_texm<R: ResourceRepository>(
    repository: &R,
    name: &ResourceName,
    archive: &str,
    label: &'static str,
) -> Result<(), AssetError> {
    let key = ResourceKey {
        archive: parse_path(archive)?,
        name: name.clone(),
        type_id: None,
    };
    let Some(bytes) = read_optional_key(repository, &key, Some(label))? else {
        return Err(AssetError::MissingDependency(format!("{label} {name:?}")));
    };
    decode_texm(bytes).map(|_| ()).map_err(AssetError::Texture)
}

fn read_optional_key<R: ResourceRepository>(
    repository: &R,
    key: &ResourceKey,
    label: Option<&str>,
) -> Result<Option<Arc<[u8]>>, AssetError> {
    let archive = match repository.open_archive(&key.archive) {
        Ok(archive) => archive,
        Err(ResourceError::MissingArchive | ResourceError::MissingEntry) => return Ok(None),
        Err(err) => {
            let label = label.unwrap_or("asset");
            return Err(map_resource_error(label, key, err));
        }
    };
    let Some(handle) = repository.find(archive, &key.name).map_err(|err| {
        let label = label.unwrap_or("asset");
        map_resource_error(label, key, err)
    })?
    else {
        return Ok(None);
    };
    let bytes = repository.read(handle).map_err(|err| {
        let label = label.unwrap_or("asset");
        map_resource_error(label, key, err)
    })?;
    Ok(Some(Arc::from(bytes.into_owned())))
}

fn sibling_name(key: &ResourceKey, extension: &str) -> Result<ResourceName, AssetError> {
    let dot = key
        .name
        .0
        .iter()
        .rposition(|byte| *byte == b'.')
        .ok_or_else(|| {
            AssetError::InvalidPrototype(format!("resource name has no extension: {:?}", key.name))
        })?;
    let mut name = key.name.0[..dot].to_vec();
    name.push(b'.');
    name.extend_from_slice(extension.as_bytes());
    Ok(ResourceName(name))
}

fn stable_visual_id(proto: &EffectivePrototype) -> u64 {
    let mut hasher = StableHasher::default();
    match &proto.geometry {
        PrototypeGeometry::Mesh(key) => {
            1_u8.hash(&mut hasher);
            key.archive.as_str().hash(&mut hasher);
            key.name.0.hash(&mut hasher);
            key.type_id.hash(&mut hasher);
        }
        PrototypeGeometry::NonGeometric => {
            0_u8.hash(&mut hasher);
        }
    }
    hasher.finish()
}

fn stable_material_id(
    proto: &EffectivePrototype,
    material_index: u16,
    material_name: &ResourceName,
) -> u64 {
    let mut hasher = StableHasher::default();
    stable_visual_id(proto).hash(&mut hasher);
    material_index.hash(&mut hasher);
    material_name.0.hash(&mut hasher);
    hasher.finish()
}

fn parse_path(value: &str) -> Result<NormalizedPath, AssetError> {
    normalize_relative(value.as_bytes(), PathPolicy::HostCompatible)
        .map_err(|err| AssetError::InvalidPrototype(format!("{err}")))
}

#[derive(Default)]
struct StableHasher(u64);

impl Hasher for StableHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut value = if self.0 == 0 {
            0xcbf2_9ce4_8422_2325
        } else {
            self.0
        };
        for byte in bytes {
            value ^= u64::from(*byte);
            value = value.wrapping_mul(0x0000_0100_0000_01b3);
        }
        self.0 = value;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_prototype::build_prototype_graph;
    use fparkan_resource::{resource_name, CachedResourceRepository};
    use fparkan_vfs::{DirectoryVfs, MemoryVfs, Vfs};
    use std::path::PathBuf;

    #[test]
    fn count_only_plan_uses_graph_requests() {
        let graph = PrototypeGraph::default();

        let plan = build_mission_asset_plan(&graph);

        assert_eq!(plan.visual_count, 0);
        assert_eq!(plan.model_count, 0);
    }

    #[test]
    fn texture_resolver_does_not_fallback_to_lightmap_archive() {
        let texm = texm_payload();
        let repo = repository_with_archives(&[(
            LIGHTMAP_ARCHIVE,
            &[(b"TEX_ONLY".as_slice(), texm.as_slice())],
        )]);

        let err = resolve_texture(&repo, &resource_name(b"TEX_ONLY")).expect_err("missing texture");

        assert!(matches!(err, AssetError::MissingDependency(_)));
    }

    #[test]
    fn lightmap_resolver_does_not_fallback_to_texture_archive() {
        let texm = texm_payload();
        let repo = repository_with_archives(&[(
            TEXTURES_ARCHIVE,
            &[(b"LM_ONLY".as_slice(), texm.as_slice())],
        )]);

        let err =
            resolve_lightmap(&repo, &resource_name(b"LM_ONLY")).expect_err("missing lightmap");

        assert!(matches!(err, AssetError::MissingDependency(_)));
    }

    #[test]
    fn texture_resolver_does_not_continue_after_malformed_texture() {
        let malformed = b"not texm".as_slice();
        let texm = texm_payload();
        let repo = repository_with_archives(&[
            (TEXTURES_ARCHIVE, &[(b"BAD".as_slice(), malformed)]),
            (LIGHTMAP_ARCHIVE, &[(b"BAD".as_slice(), texm.as_slice())]),
        ]);

        let err = resolve_texture(&repo, &resource_name(b"BAD")).expect_err("malformed texture");

        assert!(matches!(err, AssetError::Texture(_)));
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn prepares_real_unit_asset_plan() {
        let root = fixture_root("IS");
        let vfs: Arc<dyn Vfs> = Arc::new(DirectoryVfs::new(&root));
        let repository = CachedResourceRepository::new(Arc::clone(&vfs));
        let roots = [resource_name(b"UNITS/AUTO/swlklas.dat")];

        let (graph, prototypes) =
            build_prototype_graph(&repository, vfs.as_ref(), &roots).expect("prototype graph");
        let count_only = build_mission_asset_plan(&graph);
        let plan = build_mission_asset_plan_with_repository(&repository, &prototypes)
            .expect("asset preparation");

        assert_eq!(count_only.visual_count, 12);
        assert_eq!(prototypes.len(), 12);
        assert_eq!(plan.visual_count, 11);
        assert_eq!(plan.model_count, 11);
        assert_eq!(plan.material_count, 62);
        assert_eq!(plan.texture_count, 77);
        assert_eq!(plan.lightmap_count, 0);
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn repository_plan_deduplicates_duplicate_visuals_but_graph_preserves_requests() {
        let root = fixture_root("IS");
        let vfs: Arc<dyn Vfs> = Arc::new(DirectoryVfs::new(&root));
        let repository = CachedResourceRepository::new(Arc::clone(&vfs));
        let roots = [
            resource_name(b"UNITS/AUTO/swlklas.dat"),
            resource_name(b"UNITS/AUTO/swlklas.dat"),
        ];

        let (graph, prototypes) =
            build_prototype_graph(&repository, vfs.as_ref(), &roots).expect("prototype graph");
        let count_only = build_mission_asset_plan(&graph);
        let plan = build_mission_asset_plan_with_repository(&repository, &prototypes)
            .expect("asset preparation");

        assert_eq!(graph.roots.len(), 2);
        assert_eq!(count_only.visual_count, 24);
        assert_eq!(prototypes.len(), 24);
        assert_eq!(plan.visual_count, 11);
        assert_eq!(plan.model_count, 11);
        assert_eq!(plan.material_count, 62);
        assert_eq!(plan.texture_count, 77);
    }

    fn fixture_root(part: &str) -> PathBuf {
        let variable = match part {
            "IS" => "FPARKAN_CORPUS_PART1_ROOT",
            "IS2" => "FPARKAN_CORPUS_PART2_ROOT",
            _ => panic!("unknown licensed corpus part: {part}"),
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

    fn repository_with_archives(
        archives: &[(&str, &[(&[u8], &[u8])])],
    ) -> CachedResourceRepository {
        let mut vfs = MemoryVfs::default();
        for (archive, entries) in archives {
            let path = parse_path(archive).expect("archive path");
            vfs.insert(path, Arc::from(build_nres(entries).into_boxed_slice()));
        }
        CachedResourceRepository::new(Arc::new(vfs))
    }

    fn texm_payload() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&0x6d78_6554_u32.to_le_bytes());
        out.extend_from_slice(&1_u32.to_le_bytes());
        out.extend_from_slice(&1_u32.to_le_bytes());
        out.extend_from_slice(&1_u32.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&565_u32.to_le_bytes());
        out.extend_from_slice(&0xffff_u16.to_le_bytes());
        out
    }

    fn build_nres(entries: &[(&[u8], &[u8])]) -> Vec<u8> {
        let mut out = vec![0; 16];
        let mut offsets = Vec::with_capacity(entries.len());
        for (_, payload) in entries {
            offsets.push(u32::try_from(out.len()).expect("offset"));
            out.extend_from_slice(payload);
            let padding = (8 - (out.len() % 8)) % 8;
            out.resize(out.len() + padding, 0);
        }
        let mut order: Vec<usize> = (0..entries.len()).collect();
        order.sort_by(|left, right| entries[*left].0.cmp(entries[*right].0));
        for (idx, (name, payload)) in entries.iter().enumerate() {
            push_u32(&mut out, 0);
            push_u32(&mut out, 0);
            push_u32(&mut out, 0);
            push_u32(&mut out, u32::try_from(payload.len()).expect("payload"));
            push_u32(&mut out, 0);
            let mut name_raw = [0; 36];
            let len = name_raw.len().saturating_sub(1).min(name.len());
            name_raw[..len].copy_from_slice(&name[..len]);
            out.extend_from_slice(&name_raw);
            push_u32(&mut out, offsets[idx]);
            push_u32(&mut out, u32::try_from(order[idx]).expect("sort index"));
        }
        out[0..4].copy_from_slice(b"NRes");
        out[4..8].copy_from_slice(&0x100_u32.to_le_bytes());
        out[8..12].copy_from_slice(&u32::try_from(entries.len()).expect("count").to_le_bytes());
        let total_size = u32::try_from(out.len()).expect("total size");
        out[12..16].copy_from_slice(&total_size.to_le_bytes());
        out
    }

    fn push_u32(out: &mut Vec<u8>, value: u32) {
        out.extend_from_slice(&value.to_le_bytes());
    }
}
