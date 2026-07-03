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

use fparkan_material::{
    decode_wear, resolve_material, Mat0Document, MaterialError, WearTable, MAT0_KIND, WEAR_KIND,
};
use fparkan_mission_format::{decode_tma, decode_tma_land_path};
pub use fparkan_mission_format::{LpString, MissionDocument, MissionError, TmaProfile};
use fparkan_msh::{decode_msh, validate_msh, ModelAsset, MshError};
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
use fparkan_texm::{decode_texm, TexmDocument, TexmError};
use std::collections::{hash_map::Entry, HashMap, HashSet};
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
    /// Prepared model backing the visual, when geometry is present.
    pub model_id: Option<AssetId<PreparedModel>>,
    /// Prepared WEAR table backing the visual, when geometry is present.
    pub wear_id: Option<AssetId<PreparedWear>>,
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
    /// Typed texture IDs available from the resolved visual.
    pub texture_ids: Vec<AssetId<PreparedTexture>>,
    /// Typed lightmap IDs available from the resolved visual.
    pub lightmap_ids: Vec<AssetId<PreparedTexture>>,
    /// Number of texture phase requests decoded as TEXM.
    pub texture_count: usize,
    /// Number of lightmap requests decoded as TEXM.
    pub lightmap_count: usize,
}

/// CPU-side validated model ready for a renderer upload path.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedModel {
    /// Stable id derived from the visual source.
    pub id: AssetId<PreparedModel>,
    /// Source mesh resource.
    pub source: ResourceKey,
    /// Fully validated model payload.
    pub validated: ModelAsset,
    /// Mesh dependencies that led to this prepared model.
    pub mesh_dependencies: Vec<ResourceKey>,
}

/// CPU-side WEAR table resolved for a visual.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedWear {
    /// Stable id derived from the visual source.
    pub id: AssetId<PreparedWear>,
    /// Source WEAR resource.
    pub source: ResourceKey,
    /// Decoded WEAR table.
    pub table: WearTable,
}

/// CPU-side data needed before a material can be handed to a renderer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedMaterial {
    /// Stable id derived from the visual and material selector.
    pub id: AssetId<PreparedMaterial>,
    /// Source MAT0 resource.
    pub source: ResourceKey,
    /// Parsed material key retained for compatibility with older callers.
    pub name: ResourceName,
    /// Decoded MAT0 payload.
    pub mat0: Mat0Document,
    /// Texture requests declared by MAT0 phases.
    pub texture_requests: Vec<ResourceName>,
    /// Lightmap requests associated with the owning WEAR table.
    pub lightmap_requests: Vec<ResourceName>,
}

/// Texture usage role inside a prepared visual.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreparedTextureUsage {
    /// Standard diffuse/albedo texture.
    Diffuse,
    /// Lightmap texture.
    Lightmap,
}

/// CPU-side TEXM texture ready for a renderer upload path.
#[derive(Clone, Debug)]
pub struct PreparedTexture {
    /// Stable id derived from the texture source and usage.
    pub id: AssetId<PreparedTexture>,
    /// Source TEXM resource.
    pub source: ResourceKey,
    /// Decoded TEXM payload.
    pub texm: TexmDocument,
    /// Usage role in the prepared visual.
    pub usage: PreparedTextureUsage,
}

impl PreparedVisual {
    /// Returns the primary material id, if any.
    #[must_use]
    pub fn primary_material_id(&self) -> Option<AssetId<PreparedMaterial>> {
        self.material_ids.first().copied()
    }
}

/// Immutable prepared mission assets for rendering and game setup.
#[derive(Clone, Debug, Default)]
pub struct MissionAssets {
    /// Mesh-backed models prepared for reachable visuals.
    pub models: Vec<PreparedModel>,
    /// WEAR tables prepared for reachable visuals.
    pub wears: Vec<PreparedWear>,
    /// MAT0 materials prepared for reachable visuals.
    pub materials: Vec<PreparedMaterial>,
    /// TEXM textures prepared for reachable visuals.
    pub textures: Vec<PreparedTexture>,
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

    /// Finds a prepared model by id.
    #[must_use]
    pub fn model_by_id(&self, id: AssetId<PreparedModel>) -> Option<&PreparedModel> {
        self.models.iter().find(|model| model.id == id)
    }

    /// Finds a prepared material by id.
    #[must_use]
    pub fn material_by_id(&self, id: AssetId<PreparedMaterial>) -> Option<&PreparedMaterial> {
        self.materials.iter().find(|material| material.id == id)
    }

    /// Finds a prepared texture by id.
    #[must_use]
    pub fn texture_by_id(&self, id: AssetId<PreparedTexture>) -> Option<&PreparedTexture> {
        self.textures.iter().find(|texture| texture.id == id)
    }

    /// Converts mission assets into a coarse mission plan.
    #[must_use]
    pub fn to_plan(&self) -> MissionAssetPlan {
        MissionAssetPlan {
            visual_count: self.visuals.len(),
            model_count: self.models.len(),
            material_count: self.materials.len(),
            texture_count: self
                .textures
                .iter()
                .filter(|texture| texture.usage == PreparedTextureUsage::Diffuse)
                .count(),
            lightmap_count: self
                .textures
                .iter()
                .filter(|texture| texture.usage == PreparedTextureUsage::Lightmap)
                .count(),
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

/// Bounded CPU-side asset preparation limits enforced before renderer upload.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AssetPreparationLimits {
    /// Maximum number of unique prepared models.
    pub max_models: Option<usize>,
    /// Maximum number of unique prepared WEAR tables.
    pub max_wears: Option<usize>,
    /// Maximum number of unique prepared materials.
    pub max_materials: Option<usize>,
    /// Maximum number of unique prepared textures, including lightmaps.
    pub max_textures: Option<usize>,
    /// Maximum sum of unique texture base-level pixels.
    pub max_texture_pixels: Option<u64>,
}

/// Summary emitted by bounded asset preparation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AssetPreparationReport {
    /// Number of unique prepared models.
    pub model_count: usize,
    /// Number of unique prepared WEAR tables.
    pub wear_count: usize,
    /// Number of unique prepared materials.
    pub material_count: usize,
    /// Number of unique prepared textures, including lightmaps.
    pub texture_count: usize,
    /// Sum of unique texture base-level pixels.
    pub texture_pixels: u64,
}

/// Errors raised while preparing CPU-side assets.
#[derive(Debug)]
pub enum AssetError {
    /// A required cross-resource dependency was not found.
    MissingDependency(String),
    /// A prototype did not describe a usable visual.
    InvalidPrototype(String),
    /// Asset preparation exceeded explicit limits.
    BudgetExceeded(String),
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
            Self::BudgetExceeded(value) => write!(f, "asset budget exceeded: {value}"),
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
            Self::MissingDependency(_) | Self::InvalidPrototype(_) | Self::BudgetExceeded(_) => {
                None
            }
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

    /// Builds mission assets together with a bounded preparation report.
    ///
    /// # Errors
    ///
    /// Returns [`AssetError`] if any visual dependency is missing or malformed,
    /// or when explicit limits are exceeded.
    pub fn prepare_mission_assets_profiled(
        &self,
        root_prototype_spans: &[std::ops::Range<usize>],
        prototypes: &[EffectivePrototype],
        limits: AssetPreparationLimits,
    ) -> Result<(MissionAssets, AssetPreparationReport), AssetError> {
        prepare_mission_assets_profiled_with_repository(
            &self.repository,
            root_prototype_spans,
            prototypes,
            limits,
        )
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
    Ok(
        prepare_mission_assets_profiled_with_repository(
            repository,
            root_prototype_spans,
            prototypes,
            AssetPreparationLimits::default(),
        )?
        .0,
    )
}

/// Builds mission assets while enforcing explicit preparation limits.
///
/// # Errors
///
/// Returns [`AssetError`] if any visual dependency is missing or malformed, or
/// when explicit limits are exceeded.
pub fn prepare_mission_assets_profiled_with_repository<R: ResourceRepository>(
    repository: &R,
    root_prototype_spans: &[std::ops::Range<usize>],
    prototypes: &[EffectivePrototype],
    limits: AssetPreparationLimits,
) -> Result<(MissionAssets, AssetPreparationReport), AssetError> {
    prepare_mission_assets_with_repository_internal(
        repository,
        root_prototype_spans,
        prototypes,
        AssetIdentityPolicy::default(),
        limits,
    )
}

fn prepare_mission_assets_with_repository_internal<R: ResourceRepository>(
    repository: &R,
    root_prototype_spans: &[std::ops::Range<usize>],
    prototypes: &[EffectivePrototype],
    identity_policy: AssetIdentityPolicy,
    limits: AssetPreparationLimits,
) -> Result<(MissionAssets, AssetPreparationReport), AssetError> {
    if prototypes.is_empty() {
        return Ok((MissionAssets::default(), AssetPreparationReport::default()));
    }
    let mut visual_index_by_id: HashMap<AssetId<PreparedVisual>, PreparedVisualSignature> =
        HashMap::new();
    let mut model_index_by_id: HashMap<AssetId<PreparedModel>, PreparedModelSignature> =
        HashMap::new();
    let mut wear_index_by_id: HashMap<AssetId<PreparedWear>, PreparedWearSignature> =
        HashMap::new();
    let mut material_index_by_id: HashMap<AssetId<PreparedMaterial>, PreparedMaterialSignature> =
        HashMap::new();
    let mut texture_index_by_id: HashMap<AssetId<PreparedTexture>, PreparedTextureSignature> =
        HashMap::new();
    let mut models = Vec::new();
    let mut wears = Vec::new();
    let mut materials = Vec::new();
    let mut textures = Vec::new();
    let mut visuals = Vec::new();
    let mut prototype_visual_ids = Vec::with_capacity(prototypes.len());

    for proto in prototypes {
        let visual_id = AssetId::new((identity_policy.visual_id)(proto));
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
                let bundle =
                    prepare_visual_with_repository_internal(repository, proto, identity_policy)?;
                if bundle.visual.id != visual_id {
                    // Defensive check. stable IDs are deterministic for the same inputs.
                    return Err(AssetError::InvalidPrototype(
                        "prepared visual id changed during preparation".to_string(),
                    ));
                }
                if let Some(model) = bundle.model {
                    if insert_asset_signature(
                        &mut model_index_by_id,
                        model.id,
                        prepared_model_signature(&model),
                        "model",
                    )? {
                        models.push(model);
                    }
                }
                if let Some(wear) = bundle.wear {
                    if insert_asset_signature(
                        &mut wear_index_by_id,
                        wear.id,
                        prepared_wear_signature(&wear),
                        "wear",
                    )? {
                        wears.push(wear);
                    }
                }
                for material in bundle.materials {
                    if insert_asset_signature(
                        &mut material_index_by_id,
                        material.id,
                        prepared_material_signature(&material),
                        "material",
                    )? {
                        materials.push(material);
                    }
                }
                for texture in bundle.textures {
                    if insert_asset_signature(
                        &mut texture_index_by_id,
                        texture.id,
                        prepared_texture_signature(&texture),
                        "texture",
                    )? {
                        textures.push(texture);
                    }
                }
                visuals.push(bundle.visual);
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

    let assets = MissionAssets {
        models,
        wears,
        materials,
        textures,
        visuals,
        object_visuals,
    };
    let report = AssetPreparationReport {
        model_count: assets.models.len(),
        wear_count: assets.wears.len(),
        material_count: assets.materials.len(),
        texture_count: assets.textures.len(),
        texture_pixels: assets
            .textures
            .iter()
            .map(|texture| u64::from(texture.texm.width()) * u64::from(texture.texm.height()))
            .sum(),
    };
    enforce_asset_preparation_limits(&report, limits)?;
    Ok((assets, report))
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PreparedVisualSignature {
    Mesh {
        archive: Vec<u8>,
        name: Vec<u8>,
        type_id: Option<u32>,
        dependency_count: usize,
    },
    NonGeometric {
        dependency_count: usize,
    },
}

#[derive(Clone, Copy)]
struct AssetIdentityPolicy {
    visual_id: fn(&EffectivePrototype) -> u64,
    model_id: fn(&EffectivePrototype) -> u64,
    wear_id: fn(&EffectivePrototype) -> u64,
    material_id: fn(&EffectivePrototype, u16, &ResourceName) -> u64,
    texture_id: fn(&ResourceKey, PreparedTextureUsage) -> u64,
}

impl Default for AssetIdentityPolicy {
    fn default() -> Self {
        Self {
            visual_id: stable_visual_id,
            model_id: stable_model_id,
            wear_id: stable_wear_id,
            material_id: stable_material_id,
            texture_id: stable_texture_id,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ResourceSignature {
    archive: Vec<u8>,
    name: Vec<u8>,
    type_id: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PreparedModelSignature {
    source: ResourceSignature,
    node_count: usize,
    slot_count: usize,
    batch_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PreparedWearSignature {
    source: ResourceSignature,
    material_slots: usize,
    lightmap_slots: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PreparedMaterialSignature {
    source: ResourceSignature,
    texture_requests: Vec<Vec<u8>>,
    lightmap_requests: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PreparedTextureSignature {
    source: ResourceSignature,
    usage: PreparedTextureUsage,
    width: u32,
    height: u32,
    mip_count: usize,
}

fn prepared_visual_signature(proto: &EffectivePrototype) -> PreparedVisualSignature {
    match &proto.geometry {
        PrototypeGeometry::Mesh(key) => PreparedVisualSignature::Mesh {
            archive: key.archive.identity_bytes().to_vec(),
            name: key.name.0.clone(),
            type_id: key.type_id,
            dependency_count: proto.dependencies.len(),
        },
        PrototypeGeometry::NonGeometric => PreparedVisualSignature::NonGeometric {
            dependency_count: proto.dependencies.len(),
        },
    }
}

fn resource_signature(key: &ResourceKey) -> ResourceSignature {
    ResourceSignature {
        archive: key.archive.identity_bytes().to_vec(),
        name: key.name.0.clone(),
        type_id: key.type_id,
    }
}

fn prepared_model_signature(model: &PreparedModel) -> PreparedModelSignature {
    PreparedModelSignature {
        source: resource_signature(&model.source),
        node_count: model.validated.node_count,
        slot_count: model.validated.slots.len(),
        batch_count: model.validated.batches.len(),
    }
}

fn prepared_wear_signature(wear: &PreparedWear) -> PreparedWearSignature {
    PreparedWearSignature {
        source: resource_signature(&wear.source),
        material_slots: wear.table.entries.len(),
        lightmap_slots: wear.table.lightmaps.len(),
    }
}

fn prepared_material_signature(material: &PreparedMaterial) -> PreparedMaterialSignature {
    PreparedMaterialSignature {
        source: resource_signature(&material.source),
        texture_requests: material
            .texture_requests
            .iter()
            .map(|name| name.0.clone())
            .collect(),
        lightmap_requests: material
            .lightmap_requests
            .iter()
            .map(|name| name.0.clone())
            .collect(),
    }
}

fn prepared_texture_signature(texture: &PreparedTexture) -> PreparedTextureSignature {
    PreparedTextureSignature {
        source: resource_signature(&texture.source),
        usage: texture.usage,
        width: texture.texm.width(),
        height: texture.texm.height(),
        mip_count: texture.texm.mip_count(),
    }
}

fn insert_asset_signature<T, S>(
    signatures: &mut HashMap<AssetId<T>, S>,
    id: AssetId<T>,
    signature: S,
    label: &str,
) -> Result<bool, AssetError>
where
    S: Eq,
{
    match signatures.entry(id) {
        Entry::Occupied(existing) => {
            if existing.get() != &signature {
                return Err(AssetError::InvalidPrototype(format!(
                    "stable {label} id collision between unrelated assets"
                )));
            }
            Ok(false)
        }
        Entry::Vacant(entry) => {
            entry.insert(signature);
            Ok(true)
        }
    }
}

fn enforce_asset_preparation_limits(
    report: &AssetPreparationReport,
    limits: AssetPreparationLimits,
) -> Result<(), AssetError> {
    if let Some(limit) = limits.max_models {
        if report.model_count > limit {
            return Err(AssetError::BudgetExceeded(format!(
                "models={} exceeds limit={limit}",
                report.model_count
            )));
        }
    }
    if let Some(limit) = limits.max_wears {
        if report.wear_count > limit {
            return Err(AssetError::BudgetExceeded(format!(
                "wears={} exceeds limit={limit}",
                report.wear_count
            )));
        }
    }
    if let Some(limit) = limits.max_materials {
        if report.material_count > limit {
            return Err(AssetError::BudgetExceeded(format!(
                "materials={} exceeds limit={limit}",
                report.material_count
            )));
        }
    }
    if let Some(limit) = limits.max_textures {
        if report.texture_count > limit {
            return Err(AssetError::BudgetExceeded(format!(
                "textures={} exceeds limit={limit}",
                report.texture_count
            )));
        }
    }
    if let Some(limit) = limits.max_texture_pixels {
        if report.texture_pixels > limit {
            return Err(AssetError::BudgetExceeded(format!(
                "texture_pixels={} exceeds limit={limit}",
                report.texture_pixels
            )));
        }
    }
    Ok(())
}

/// Extends a prototype dependency report with visual dependency failures.
///
/// This function validates WEAR/material/TEXM/LIGHTMAP resolution for each resolved
/// prototype without constructing full immutable assets.
pub fn extend_graph_report_with_visual_dependencies<R: ResourceRepository>(
    repository: &R,
    report: &mut PrototypeGraphReport,
    graph: &mut PrototypeGraph,
    prototypes: &[EffectivePrototype],
) {
    if graph.visual_dependencies_expanded {
        return;
    }
    let material_archive = parse_path("material.lib")
        .expect("static material archive path must satisfy host-compatible normalization");
    let mut next_node = graph
        .nodes
        .iter()
        .map(|node| node.id.0)
        .max()
        .map_or(0, |value| value.saturating_add(1));
    let mut next_edge = graph
        .edges
        .iter()
        .map(|edge| edge.id.0)
        .max()
        .map_or(0, |value| value.saturating_add(1));

    for (prototype_index, prototype) in prototypes.iter().enumerate() {
        let PrototypeGeometry::Mesh(mesh) = &prototype.geometry else {
            continue;
        };
        report.wear_request_count += 1;
        let Some(prototype_node_id) = prototype_node_id(graph, prototype_index) else {
            continue;
        };
        let Some(mesh_node_id) = prototype_mesh_node_id(graph, prototype_node_id) else {
            continue;
        };
        let mesh_parent_edge = mesh_edge_id(graph, prototype_node_id);
        let root_index = root_index_for_prototype(graph, prototype_index);

        match resolve_wear_table(repository, mesh) {
            Ok(table) => {
                report.wear_resolved_count += 1;
                let wear_key = match wear_resource_key(mesh) {
                    Ok(key) => key,
                    Err(message) => {
                        push_visual_failure(
                            report,
                            graph,
                            prototype_index,
                            mesh.name.0.clone(),
                            PrototypeGraphEdge::MeshToWear,
                            PrototypeGraphRequiredness::Required,
                            &message.to_string(),
                        );
                        continue;
                    }
                };
                let wear_node_id = push_graph_resource_node(
                    graph,
                    PrototypeGraphNodeKind::WearResource,
                    wear_key.clone(),
                    &mut next_node,
                );
                let wear_edge_id = push_graph_edge(
                    graph,
                    mesh_node_id,
                    wear_node_id,
                    fparkan_prototype::PrototypeGraphEdgeKind::MeshToWear,
                    PrototypeGraphRequiredness::Required,
                    Some(provenance_for_resource(
                        root_index,
                        mesh_parent_edge,
                        &wear_key,
                    )),
                    &mut next_edge,
                );
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
                            let material_key = ResourceKey {
                                archive: material_archive.clone(),
                                name: material.name.clone(),
                                type_id: Some(MAT0_KIND),
                            };
                            let material_node_id = push_graph_resource_node(
                                graph,
                                PrototypeGraphNodeKind::MaterialResource,
                                material_key.clone(),
                                &mut next_node,
                            );
                            let material_edge_id = push_graph_edge(
                                graph,
                                wear_node_id,
                                material_node_id,
                                fparkan_prototype::PrototypeGraphEdgeKind::WearToMaterial,
                                PrototypeGraphRequiredness::Required,
                                Some(provenance_for_resource(
                                    root_index,
                                    Some(wear_edge_id),
                                    &material_key,
                                )),
                                &mut next_edge,
                            );
                            for texture in material.document.texture_requests() {
                                report.texture_request_count += 1;
                                match resolve_texture(repository, &texture) {
                                    Ok(()) => {
                                        report.texture_resolved_count += 1;
                                        if let Ok(texture_key) =
                                            texm_resource_key(TEXTURES_ARCHIVE, &texture)
                                        {
                                            let texture_node_id = push_graph_resource_node(
                                                graph,
                                                PrototypeGraphNodeKind::TextureResource,
                                                texture_key.clone(),
                                                &mut next_node,
                                            );
                                            push_graph_edge(
                                                graph,
                                                material_node_id,
                                                texture_node_id,
                                                fparkan_prototype::PrototypeGraphEdgeKind::MaterialToTexture,
                                                PrototypeGraphRequiredness::Required,
                                                Some(provenance_for_resource(
                                                    root_index,
                                                    Some(material_edge_id),
                                                    &texture_key,
                                                )),
                                                &mut next_edge,
                                            );
                                        }
                                    }
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
                    match resolve_lightmap(repository, &lightmap.lightmap) {
                        Ok(()) => {
                            report.lightmap_resolved_count += 1;
                            if let Ok(lightmap_key) =
                                texm_resource_key(LIGHTMAP_ARCHIVE, &lightmap.lightmap)
                            {
                                let lightmap_node_id = push_graph_resource_node(
                                    graph,
                                    PrototypeGraphNodeKind::LightmapResource,
                                    lightmap_key.clone(),
                                    &mut next_node,
                                );
                                push_graph_edge(
                                    graph,
                                    wear_node_id,
                                    lightmap_node_id,
                                    fparkan_prototype::PrototypeGraphEdgeKind::WearToLightmap,
                                    PrototypeGraphRequiredness::Required,
                                    Some(provenance_for_resource(
                                        root_index,
                                        Some(wear_edge_id),
                                        &lightmap_key,
                                    )),
                                    &mut next_edge,
                                );
                            }
                        }
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
    graph.visual_dependencies_expanded = true;
}

fn push_graph_resource_node(
    graph: &mut PrototypeGraph,
    kind: PrototypeGraphNodeKind,
    resource: ResourceKey,
    next_node: &mut u32,
) -> fparkan_prototype::PrototypeGraphNodeId {
    let id = fparkan_prototype::PrototypeGraphNodeId(*next_node);
    *next_node = (*next_node).saturating_add(1);
    graph
        .nodes
        .push(fparkan_prototype::PrototypeGraphNode::resource(
            kind, resource, id,
        ));
    id
}

fn push_graph_edge(
    graph: &mut PrototypeGraph,
    from: fparkan_prototype::PrototypeGraphNodeId,
    to: fparkan_prototype::PrototypeGraphNodeId,
    kind: fparkan_prototype::PrototypeGraphEdgeKind,
    requiredness: PrototypeGraphRequiredness,
    provenance: Option<PrototypeGraphProvenance>,
    next_edge: &mut u32,
) -> fparkan_prototype::PrototypeGraphEdgeId {
    let id = fparkan_prototype::PrototypeGraphEdgeId(*next_edge);
    *next_edge = (*next_edge).saturating_add(1);
    graph
        .edges
        .push(fparkan_prototype::PrototypeGraphEdgeInstance {
            id,
            from,
            to,
            kind,
            requiredness,
            provenance,
        });
    id
}

fn prototype_mesh_node_id(
    graph: &PrototypeGraph,
    prototype_node: fparkan_prototype::PrototypeGraphNodeId,
) -> Option<fparkan_prototype::PrototypeGraphNodeId> {
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
        .map(|edge| edge.to)
}

fn provenance_for_resource(
    root_index: usize,
    parent_edge: Option<fparkan_prototype::PrototypeGraphEdgeId>,
    resource: &ResourceKey,
) -> PrototypeGraphProvenance {
    PrototypeGraphProvenance {
        root_index,
        parent_edge,
        archive: Some(resource.archive.display_lossy().to_string()),
        resource: Some(resource.name.0.clone()),
        span: None,
    }
}

fn wear_resource_key(mesh: &ResourceKey) -> Result<ResourceKey, AssetError> {
    Ok(ResourceKey {
        archive: mesh.archive.clone(),
        name: sibling_name(mesh, "wea")?,
        type_id: Some(WEAR_KIND),
    })
}

fn texm_resource_key(archive: &str, name: &ResourceName) -> Result<ResourceKey, AssetError> {
    Ok(ResourceKey {
        archive: parse_path(archive)?,
        name: name.clone(),
        type_id: None,
    })
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
        model_id: None,
        wear_id: None,
        model_nodes: 0,
        model_slots: 0,
        model_batches: 0,
        material_count: 0,
        material_ids: Vec::new(),
        texture_ids: Vec::new(),
        lightmap_ids: Vec::new(),
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
    Ok(
        prepare_visual_with_repository_internal(repository, proto, AssetIdentityPolicy::default())?
            .visual,
    )
}

struct PreparedVisualBundle {
    visual: PreparedVisual,
    model: Option<PreparedModel>,
    wear: Option<PreparedWear>,
    materials: Vec<PreparedMaterial>,
    textures: Vec<PreparedTexture>,
}

fn prepare_visual_with_repository_internal<R: ResourceRepository>(
    repository: &R,
    proto: &EffectivePrototype,
    identity_policy: AssetIdentityPolicy,
) -> Result<PreparedVisualBundle, AssetError> {
    let PrototypeGeometry::Mesh(mesh_key) = &proto.geometry else {
        return Ok(PreparedVisualBundle {
            visual: prepare_visual(proto)?,
            model: None,
            wear: None,
            materials: Vec::new(),
            textures: Vec::new(),
        });
    };

    let nres = decode_nres(
        read_key(repository, mesh_key, Some("mesh"))?,
        ReadProfile::Compatible,
    )
    .map_err(AssetError::Nres)?;
    let msh_document = decode_msh(&nres).map_err(AssetError::Msh)?;
    let model = validate_msh(&msh_document).map_err(AssetError::Msh)?;
    let model_id = AssetId::new((identity_policy.model_id)(proto));
    let prepared_model = PreparedModel {
        id: model_id,
        source: mesh_key.clone(),
        validated: model.clone(),
        mesh_dependencies: proto.dependencies.clone(),
    };

    let wear_name = sibling_name(mesh_key, "wea")?;
    let wear_key = ResourceKey {
        archive: mesh_key.archive.clone(),
        name: wear_name,
        type_id: Some(WEAR_KIND),
    };
    let wear = decode_wear(&read_key(repository, &wear_key, Some("wear"))?)
        .map_err(AssetError::Material)?;
    let wear_id = AssetId::new((identity_policy.wear_id)(proto));
    let prepared_wear = PreparedWear {
        id: wear_id,
        source: wear_key.clone(),
        table: wear.clone(),
    };

    let mut material_count = 0;
    let mut material_ids = Vec::with_capacity(wear.entries.len());
    let mut prepared_materials = Vec::with_capacity(wear.entries.len());
    let mut prepared_textures = Vec::new();
    let mut texture_ids = Vec::new();
    let mut lightmap_ids = Vec::new();
    let mut texture_count = 0;
    let mut lightmap_count = 0;
    let lightmap_requests: Vec<_> = wear
        .lightmaps
        .iter()
        .map(|lightmap| lightmap.lightmap.clone())
        .collect();
    for material_index in 0..wear.entries.len() {
        let material_index = u16::try_from(material_index).map_err(|_| {
            AssetError::InvalidPrototype("material index does not fit archive format".to_string())
        })?;
        let material =
            resolve_material(repository, &wear, material_index).map_err(AssetError::Material)?;
        material_count += 1;
        let material_id = AssetId::new((identity_policy.material_id)(
            proto,
            material_index,
            &material.name,
        ));
        material_ids.push(material_id);
        let material_key = ResourceKey {
            archive: parse_path("material.lib")?,
            name: material.name.clone(),
            type_id: Some(MAT0_KIND),
        };
        let texture_requests = material.document.texture_requests();
        prepared_materials.push(PreparedMaterial {
            id: material_id,
            source: material_key,
            name: material.name.clone(),
            mat0: material.document.clone(),
            texture_requests: texture_requests.clone(),
            lightmap_requests: lightmap_requests.clone(),
        });

        for texture in texture_requests {
            let prepared_texture = prepare_texture(
                repository,
                &texture,
                PreparedTextureUsage::Diffuse,
                identity_policy,
            )?;
            texture_ids.push(prepared_texture.id);
            prepared_textures.push(prepared_texture);
            texture_count += 1;
        }
    }

    for lightmap in &wear.lightmaps {
        let prepared_lightmap = prepare_texture(
            repository,
            &lightmap.lightmap,
            PreparedTextureUsage::Lightmap,
            identity_policy,
        )?;
        lightmap_ids.push(prepared_lightmap.id);
        prepared_textures.push(prepared_lightmap);
        lightmap_count += 1;
    }

    Ok(PreparedVisualBundle {
        visual: PreparedVisual {
            id: AssetId::new((identity_policy.visual_id)(proto)),
            mesh: Some(mesh_key.clone()),
            model_id: Some(model_id),
            wear_id: Some(wear_id),
            model_nodes: model.node_count,
            model_slots: model.slots.len(),
            model_batches: model.batches.len(),
            material_count,
            material_ids,
            texture_ids,
            lightmap_ids,
            texture_count,
            lightmap_count,
        },
        model: Some(prepared_model),
        wear: Some(prepared_wear),
        materials: prepared_materials,
        textures: prepared_textures,
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

fn prepare_texture<R: ResourceRepository>(
    repository: &R,
    name: &ResourceName,
    usage: PreparedTextureUsage,
    identity_policy: AssetIdentityPolicy,
) -> Result<PreparedTexture, AssetError> {
    let (archive, label) = match usage {
        PreparedTextureUsage::Diffuse => (TEXTURES_ARCHIVE, "texture"),
        PreparedTextureUsage::Lightmap => (LIGHTMAP_ARCHIVE, "lightmap"),
    };
    let key = ResourceKey {
        archive: parse_path(archive)?,
        name: name.clone(),
        type_id: None,
    };
    let Some(bytes) = read_optional_key(repository, &key, Some(label))? else {
        return Err(AssetError::MissingDependency(format!("{label} {name:?}")));
    };
    let texm = decode_texm(bytes).map_err(AssetError::Texture)?;
    Ok(PreparedTexture {
        id: AssetId::new((identity_policy.texture_id)(&key, usage)),
        source: key,
        texm,
        usage,
    })
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
        Err(ResourceError::MissingArchive { .. } | ResourceError::MissingEntry) => return Ok(None),
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
            key.archive.identity_bytes().hash(&mut hasher);
            key.name.0.hash(&mut hasher);
            key.type_id.hash(&mut hasher);
        }
        PrototypeGeometry::NonGeometric => {
            0_u8.hash(&mut hasher);
        }
    }
    hasher.finish()
}

fn stable_model_id(proto: &EffectivePrototype) -> u64 {
    stable_visual_id(proto)
}

fn stable_wear_id(proto: &EffectivePrototype) -> u64 {
    let mut hasher = StableHasher::default();
    stable_visual_id(proto).hash(&mut hasher);
    b"wear".hash(&mut hasher);
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

fn stable_texture_id(key: &ResourceKey, usage: PreparedTextureUsage) -> u64 {
    let mut hasher = StableHasher::default();
    key.archive.identity_bytes().hash(&mut hasher);
    key.name.0.hash(&mut hasher);
    match usage {
        PreparedTextureUsage::Diffuse => 0_u8.hash(&mut hasher),
        PreparedTextureUsage::Lightmap => 1_u8.hash(&mut hasher),
    }
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
    fn stable_visual_id_uses_archive_identity_bytes() {
        let first = EffectivePrototype {
            key: fparkan_prototype::PrototypeKey(resource_name(b"mesh")),
            geometry: PrototypeGeometry::Mesh(ResourceKey {
                archive: normalize_relative(b"DATA/\xFF.lib", PathPolicy::HostCompatible)
                    .expect("archive"),
                name: resource_name(b"mesh.msh"),
                type_id: Some(0x4853_454D),
            }),
            source: fparkan_prototype::PrototypeSource::DirectArchive,
            dependencies: Vec::new(),
        };
        let second = EffectivePrototype {
            key: fparkan_prototype::PrototypeKey(resource_name(b"mesh")),
            geometry: PrototypeGeometry::Mesh(ResourceKey {
                archive: normalize_relative(b"DATA/\xFE.lib", PathPolicy::HostCompatible)
                    .expect("archive"),
                name: resource_name(b"mesh.msh"),
                type_id: Some(0x4853_454D),
            }),
            source: fparkan_prototype::PrototypeSource::DirectArchive,
            dependencies: Vec::new(),
        };

        assert_ne!(stable_visual_id(&first), stable_visual_id(&second));
        assert_ne!(
            prepared_visual_signature(&first),
            prepared_visual_signature(&second)
        );
    }

    #[test]
    fn graph_materializes_visual_dependency_nodes_and_edges() {
        let mesh_key = ResourceKey {
            archive: parse_path("static.rlb").expect("archive"),
            name: resource_name(b"tree.msh"),
            type_id: Some(0x4853_454D),
        };
        let mat0 = mat0_with_texture(b"TEX_A");
        let texm = texm_payload();
        let lightmap_texm = texm_payload();
        let repo = repository_with_archives_meta(&[
            (
                "static.rlb",
                &[TestNresEntry {
                    name: b"tree.wea",
                    payload: b"1\n0 MAT_A\n\nLIGHTMAPS\n1\n0 LM_A\n",
                    type_id: WEAR_KIND,
                    attr2: 0,
                }],
            ),
            (
                "material.lib",
                &[TestNresEntry {
                    name: b"MAT_A",
                    payload: &mat0,
                    type_id: MAT0_KIND,
                    attr2: 0,
                }],
            ),
            (
                TEXTURES_ARCHIVE,
                &[TestNresEntry {
                    name: b"TEX_A",
                    payload: &texm,
                    type_id: 0,
                    attr2: 0,
                }],
            ),
            (
                LIGHTMAP_ARCHIVE,
                &[TestNresEntry {
                    name: b"LM_A",
                    payload: &lightmap_texm,
                    type_id: 0,
                    attr2: 0,
                }],
            ),
        ]);
        let prototype = EffectivePrototype {
            key: fparkan_prototype::PrototypeKey(resource_name(b"tree")),
            geometry: PrototypeGeometry::Mesh(mesh_key.clone()),
            source: fparkan_prototype::PrototypeSource::DirectArchive,
            dependencies: vec![mesh_key.clone()],
        };
        let mut graph = prototype_graph_for_mesh(&prototype);
        let mut report = PrototypeGraphReport {
            root_count: 1,
            direct_reference_count: 1,
            resolved_count: 1,
            mesh_dependency_count: 1,
            ..PrototypeGraphReport::default()
        };

        extend_graph_report_with_visual_dependencies(
            &repo,
            &mut report,
            &mut graph,
            std::slice::from_ref(&prototype),
        );

        assert!(graph
            .nodes
            .iter()
            .any(|node| node.kind == PrototypeGraphNodeKind::WearResource));
        assert!(graph
            .nodes
            .iter()
            .any(|node| node.kind == PrototypeGraphNodeKind::MaterialResource));
        assert!(graph
            .nodes
            .iter()
            .any(|node| node.kind == PrototypeGraphNodeKind::TextureResource));
        assert!(graph
            .nodes
            .iter()
            .any(|node| node.kind == PrototypeGraphNodeKind::LightmapResource));
        assert!(graph
            .edges
            .iter()
            .any(|edge| edge.kind == fparkan_prototype::PrototypeGraphEdgeKind::MeshToWear));
        assert!(graph
            .edges
            .iter()
            .any(|edge| edge.kind == fparkan_prototype::PrototypeGraphEdgeKind::WearToMaterial));
        assert!(graph
            .edges
            .iter()
            .any(|edge| edge.kind == fparkan_prototype::PrototypeGraphEdgeKind::MaterialToTexture));
        assert!(graph
            .edges
            .iter()
            .any(|edge| edge.kind == fparkan_prototype::PrototypeGraphEdgeKind::WearToLightmap));
        assert_eq!(report.wear_request_count, 1);
        assert_eq!(report.wear_resolved_count, 1);
        assert_eq!(report.material_slot_count, 1);
        assert_eq!(report.material_resolved_count, 1);
        assert_eq!(report.texture_request_count, 1);
        assert_eq!(report.texture_resolved_count, 1);
        assert_eq!(report.lightmap_request_count, 1);
        assert_eq!(report.lightmap_resolved_count, 1);
        assert!(report.failures.is_empty());
    }

    #[test]
    fn graph_visual_dependency_edges_preserve_root_and_parent_provenance() {
        let mesh_key = ResourceKey {
            archive: parse_path("static.rlb").expect("archive"),
            name: resource_name(b"tree.msh"),
            type_id: Some(0x4853_454D),
        };
        let mat0 = mat0_with_texture(b"TEX_A");
        let texm = texm_payload();
        let lightmap_texm = texm_payload();
        let repo = repository_with_archives_meta(&[
            (
                "static.rlb",
                &[TestNresEntry {
                    name: b"tree.wea",
                    payload: b"1\n0 MAT_A\n\nLIGHTMAPS\n1\n0 LM_A\n",
                    type_id: WEAR_KIND,
                    attr2: 0,
                }],
            ),
            (
                "material.lib",
                &[TestNresEntry {
                    name: b"MAT_A",
                    payload: &mat0,
                    type_id: MAT0_KIND,
                    attr2: 0,
                }],
            ),
            (
                TEXTURES_ARCHIVE,
                &[TestNresEntry {
                    name: b"TEX_A",
                    payload: &texm,
                    type_id: 0,
                    attr2: 0,
                }],
            ),
            (
                LIGHTMAP_ARCHIVE,
                &[TestNresEntry {
                    name: b"LM_A",
                    payload: &lightmap_texm,
                    type_id: 0,
                    attr2: 0,
                }],
            ),
        ]);
        let prototype = EffectivePrototype {
            key: fparkan_prototype::PrototypeKey(resource_name(b"tree")),
            geometry: PrototypeGeometry::Mesh(mesh_key),
            source: fparkan_prototype::PrototypeSource::DirectArchive,
            dependencies: Vec::new(),
        };
        let mut graph = prototype_graph_for_mesh(&prototype);
        let mut report = PrototypeGraphReport {
            root_count: 1,
            direct_reference_count: 1,
            resolved_count: 1,
            mesh_dependency_count: 1,
            ..PrototypeGraphReport::default()
        };

        extend_graph_report_with_visual_dependencies(
            &repo,
            &mut report,
            &mut graph,
            std::slice::from_ref(&prototype),
        );

        let wear_edge = graph
            .edges
            .iter()
            .find(|edge| edge.kind == fparkan_prototype::PrototypeGraphEdgeKind::MeshToWear)
            .expect("wear edge");
        let material_edge = graph
            .edges
            .iter()
            .find(|edge| edge.kind == fparkan_prototype::PrototypeGraphEdgeKind::WearToMaterial)
            .expect("material edge");
        let texture_edge = graph
            .edges
            .iter()
            .find(|edge| edge.kind == fparkan_prototype::PrototypeGraphEdgeKind::MaterialToTexture)
            .expect("texture edge");
        let lightmap_edge = graph
            .edges
            .iter()
            .find(|edge| edge.kind == fparkan_prototype::PrototypeGraphEdgeKind::WearToLightmap)
            .expect("lightmap edge");

        assert_eq!(
            wear_edge
                .provenance
                .as_ref()
                .expect("wear provenance")
                .parent_edge,
            Some(fparkan_prototype::PrototypeGraphEdgeId(1))
        );
        assert_eq!(
            material_edge
                .provenance
                .as_ref()
                .expect("material provenance")
                .parent_edge,
            Some(wear_edge.id)
        );
        assert_eq!(
            texture_edge
                .provenance
                .as_ref()
                .expect("texture provenance")
                .parent_edge,
            Some(material_edge.id)
        );
        assert_eq!(
            lightmap_edge
                .provenance
                .as_ref()
                .expect("lightmap provenance")
                .parent_edge,
            Some(wear_edge.id)
        );
        assert!(graph
            .edges
            .iter()
            .filter_map(|edge| edge.provenance.as_ref())
            .all(|provenance| provenance.root_index == 0));
    }

    #[test]
    fn graph_visual_dependency_expansion_is_idempotent() {
        let mesh_key = ResourceKey {
            archive: parse_path("static.rlb").expect("archive"),
            name: resource_name(b"tree.msh"),
            type_id: Some(0x4853_454D),
        };
        let mat0 = mat0_with_texture(b"TEX_A");
        let texm = texm_payload();
        let lightmap_texm = texm_payload();
        let repo = repository_with_archives_meta(&[
            (
                "static.rlb",
                &[TestNresEntry {
                    name: b"tree.wea",
                    payload: b"1\n0 MAT_A\n\nLIGHTMAPS\n1\n0 LM_A\n",
                    type_id: WEAR_KIND,
                    attr2: 0,
                }],
            ),
            (
                "material.lib",
                &[TestNresEntry {
                    name: b"MAT_A",
                    payload: &mat0,
                    type_id: MAT0_KIND,
                    attr2: 0,
                }],
            ),
            (
                TEXTURES_ARCHIVE,
                &[TestNresEntry {
                    name: b"TEX_A",
                    payload: &texm,
                    type_id: 0,
                    attr2: 0,
                }],
            ),
            (
                LIGHTMAP_ARCHIVE,
                &[TestNresEntry {
                    name: b"LM_A",
                    payload: &lightmap_texm,
                    type_id: 0,
                    attr2: 0,
                }],
            ),
        ]);
        let prototype = EffectivePrototype {
            key: fparkan_prototype::PrototypeKey(resource_name(b"tree")),
            geometry: PrototypeGeometry::Mesh(mesh_key),
            source: fparkan_prototype::PrototypeSource::DirectArchive,
            dependencies: Vec::new(),
        };
        let mut graph = prototype_graph_for_mesh(&prototype);
        let mut report = PrototypeGraphReport {
            root_count: 1,
            direct_reference_count: 1,
            resolved_count: 1,
            mesh_dependency_count: 1,
            ..PrototypeGraphReport::default()
        };

        extend_graph_report_with_visual_dependencies(
            &repo,
            &mut report,
            &mut graph,
            std::slice::from_ref(&prototype),
        );
        let first = (graph.nodes.clone(), graph.edges.clone(), report.clone());

        extend_graph_report_with_visual_dependencies(
            &repo,
            &mut report,
            &mut graph,
            std::slice::from_ref(&prototype),
        );

        assert!(graph.visual_dependencies_expanded);
        assert_eq!(graph.nodes, first.0);
        assert_eq!(graph.edges, first.1);
        assert_eq!(report, first.2);
    }

    #[test]
    fn prepare_single_visual_mission_assets_materialize_model_wear_material_and_texture_payloads() {
        let mesh_key = ResourceKey {
            archive: parse_path("static.rlb").expect("archive"),
            name: resource_name(b"tree.msh"),
            type_id: Some(0x4853_454D),
        };
        let msh = minimal_model_archive();
        let mat0 = mat0_with_texture(b"TEX_A");
        let texm = texm_payload();
        let lightmap_texm = texm_payload();
        let repo = repository_with_archives_meta(&[
            (
                "static.rlb",
                &[
                    TestNresEntry {
                        name: b"tree.msh",
                        payload: &msh,
                        type_id: 0x4853_454D,
                        attr2: 0,
                    },
                    TestNresEntry {
                        name: b"tree.wea",
                        payload: b"1\n0 MAT_A\n\nLIGHTMAPS\n1\n0 LM_A\n",
                        type_id: WEAR_KIND,
                        attr2: 0,
                    },
                ],
            ),
            (
                "material.lib",
                &[TestNresEntry {
                    name: b"MAT_A",
                    payload: &mat0,
                    type_id: MAT0_KIND,
                    attr2: 0,
                }],
            ),
            (
                TEXTURES_ARCHIVE,
                &[TestNresEntry {
                    name: b"TEX_A",
                    payload: &texm,
                    type_id: 0,
                    attr2: 0,
                }],
            ),
            (
                LIGHTMAP_ARCHIVE,
                &[TestNresEntry {
                    name: b"LM_A",
                    payload: &lightmap_texm,
                    type_id: 0,
                    attr2: 0,
                }],
            ),
        ]);
        let prototype = EffectivePrototype {
            key: fparkan_prototype::PrototypeKey(resource_name(b"tree")),
            geometry: PrototypeGeometry::Mesh(mesh_key),
            source: fparkan_prototype::PrototypeSource::DirectArchive,
            dependencies: Vec::new(),
        };

        let assets = prepare_mission_assets_with_repository(
            &repo,
            std::slice::from_ref(&(0..1)),
            std::slice::from_ref(&prototype),
        )
        .expect("prepared mission assets");

        assert_eq!(assets.models.len(), 1);
        assert_eq!(assets.wears.len(), 1);
        assert_eq!(assets.materials.len(), 1);
        assert_eq!(assets.textures.len(), 2);
        assert_eq!(assets.visuals.len(), 1);
        assert_eq!(assets.object_visuals, vec![vec![assets.visuals[0].id]]);

        let visual = &assets.visuals[0];
        assert_eq!(visual.model_id, Some(assets.models[0].id));
        assert_eq!(visual.wear_id, Some(assets.wears[0].id));
        assert_eq!(visual.material_ids, vec![assets.materials[0].id]);
        assert_eq!(visual.texture_ids.len(), 1);
        assert_eq!(visual.lightmap_ids.len(), 1);
    }

    #[test]
    fn forced_model_id_collision_is_rejected() {
        assert_forced_collision(
            AssetIdentityPolicy {
                model_id: |_| 7,
                ..AssetIdentityPolicy::default()
            },
            "stable model id collision",
        );
    }

    #[test]
    fn forced_wear_id_collision_is_rejected() {
        assert_forced_collision(
            AssetIdentityPolicy {
                wear_id: |_| 11,
                ..AssetIdentityPolicy::default()
            },
            "stable wear id collision",
        );
    }

    #[test]
    fn forced_material_id_collision_is_rejected() {
        assert_forced_collision(
            AssetIdentityPolicy {
                material_id: |_, _, _| 13,
                ..AssetIdentityPolicy::default()
            },
            "stable material id collision",
        );
    }

    #[test]
    fn forced_texture_id_collision_is_rejected() {
        assert_forced_collision(
            AssetIdentityPolicy {
                texture_id: |_, _| 17,
                ..AssetIdentityPolicy::default()
            },
            "stable texture id collision",
        );
    }

    #[test]
    fn profiled_asset_preparation_reports_unique_asset_counts() {
        let (repo, prototypes) = collision_fixture();

        let (assets, report) = prepare_mission_assets_profiled_with_repository(
            &repo,
            &[0..1, 1..2],
            &prototypes,
            AssetPreparationLimits::default(),
        )
        .expect("profiled assets");

        assert_eq!(report.model_count, assets.models.len());
        assert_eq!(report.wear_count, assets.wears.len());
        assert_eq!(report.material_count, assets.materials.len());
        assert_eq!(report.texture_count, assets.textures.len());
        assert!(report.texture_pixels > 0);
    }

    #[test]
    fn asset_preparation_limits_reject_texture_pixel_budget() {
        let (repo, prototypes) = collision_fixture();

        let err = prepare_mission_assets_profiled_with_repository(
            &repo,
            &[0..1, 1..2],
            &prototypes,
            AssetPreparationLimits {
                max_texture_pixels: Some(1),
                ..AssetPreparationLimits::default()
            },
        )
        .expect_err("budget should fail");

        assert!(matches!(err, AssetError::BudgetExceeded(_)));
        assert!(err.to_string().contains("texture_pixels"));
    }

    #[test]
    fn graph_report_uses_strict_texture_archive_policy() {
        let mesh_key = ResourceKey {
            archive: parse_path("static.rlb").expect("archive"),
            name: resource_name(b"tree.msh"),
            type_id: Some(0x4853_454D),
        };
        let mat0 = mat0_with_texture(b"TEX_A");
        let texm = texm_payload();
        let repo = repository_with_archives_meta(&[
            (
                "static.rlb",
                &[TestNresEntry {
                    name: b"tree.wea",
                    payload: b"1\n0 MAT_A\n",
                    type_id: WEAR_KIND,
                    attr2: 0,
                }],
            ),
            (
                "material.lib",
                &[TestNresEntry {
                    name: b"MAT_A",
                    payload: &mat0,
                    type_id: MAT0_KIND,
                    attr2: 0,
                }],
            ),
            (
                LIGHTMAP_ARCHIVE,
                &[TestNresEntry {
                    name: b"TEX_A",
                    payload: &texm,
                    type_id: 0,
                    attr2: 0,
                }],
            ),
        ]);
        let prototype = EffectivePrototype {
            key: fparkan_prototype::PrototypeKey(resource_name(b"tree")),
            geometry: PrototypeGeometry::Mesh(mesh_key.clone()),
            source: fparkan_prototype::PrototypeSource::DirectArchive,
            dependencies: vec![mesh_key.clone()],
        };
        let mut graph = prototype_graph_for_mesh(&prototype);
        let mut report = PrototypeGraphReport {
            root_count: 1,
            direct_reference_count: 1,
            resolved_count: 1,
            mesh_dependency_count: 1,
            ..PrototypeGraphReport::default()
        };

        extend_graph_report_with_visual_dependencies(
            &repo,
            &mut report,
            &mut graph,
            std::slice::from_ref(&prototype),
        );

        assert_eq!(report.texture_request_count, 1);
        assert_eq!(report.texture_resolved_count, 0);
        assert_eq!(report.failures.len(), 1);
        assert_eq!(
            report.failures[0].edge,
            PrototypeGraphEdge::MaterialToTexture
        );
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

    fn assert_forced_collision(policy: AssetIdentityPolicy, expected: &str) {
        let (repo, prototypes) = collision_fixture();

        let err = prepare_mission_assets_with_repository_internal(
            &repo,
            &[0..1, 1..2],
            &prototypes,
            policy,
            AssetPreparationLimits::default(),
        )
        .expect_err("collision should fail");

        assert!(err.to_string().contains(expected), "{err}");
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

    fn collision_fixture() -> (CachedResourceRepository, Vec<EffectivePrototype>) {
        let msh = minimal_model_archive();
        let mat0_a = mat0_with_texture(b"TEX_A");
        let mat0_b = mat0_with_texture(b"TEX_B");
        let texm_a = texm_payload();
        let texm_b = texm_payload();
        let lightmap_a = texm_payload();
        let lightmap_b = texm_payload();
        let repo = repository_with_archives_meta(&[
            (
                "static.rlb",
                &[
                    TestNresEntry {
                        name: b"tree_a.msh",
                        payload: &msh,
                        type_id: 0x4853_454D,
                        attr2: 0,
                    },
                    TestNresEntry {
                        name: b"tree_a.wea",
                        payload: b"1\n0 MAT_A\n\nLIGHTMAPS\n1\n0 LM_A\n",
                        type_id: WEAR_KIND,
                        attr2: 0,
                    },
                    TestNresEntry {
                        name: b"tree_b.msh",
                        payload: &msh,
                        type_id: 0x4853_454D,
                        attr2: 0,
                    },
                    TestNresEntry {
                        name: b"tree_b.wea",
                        payload: b"1\n0 MAT_B\n\nLIGHTMAPS\n1\n0 LM_B\n",
                        type_id: WEAR_KIND,
                        attr2: 0,
                    },
                ],
            ),
            (
                "material.lib",
                &[
                    TestNresEntry {
                        name: b"MAT_A",
                        payload: &mat0_a,
                        type_id: MAT0_KIND,
                        attr2: 0,
                    },
                    TestNresEntry {
                        name: b"MAT_B",
                        payload: &mat0_b,
                        type_id: MAT0_KIND,
                        attr2: 0,
                    },
                ],
            ),
            (
                TEXTURES_ARCHIVE,
                &[
                    TestNresEntry {
                        name: b"TEX_A",
                        payload: &texm_a,
                        type_id: 0,
                        attr2: 0,
                    },
                    TestNresEntry {
                        name: b"TEX_B",
                        payload: &texm_b,
                        type_id: 0,
                        attr2: 0,
                    },
                ],
            ),
            (
                LIGHTMAP_ARCHIVE,
                &[
                    TestNresEntry {
                        name: b"LM_A",
                        payload: &lightmap_a,
                        type_id: 0,
                        attr2: 0,
                    },
                    TestNresEntry {
                        name: b"LM_B",
                        payload: &lightmap_b,
                        type_id: 0,
                        attr2: 0,
                    },
                ],
            ),
        ]);
        let prototypes = vec![
            EffectivePrototype {
                key: fparkan_prototype::PrototypeKey(resource_name(b"tree_a")),
                geometry: PrototypeGeometry::Mesh(ResourceKey {
                    archive: parse_path("static.rlb").expect("archive"),
                    name: resource_name(b"tree_a.msh"),
                    type_id: Some(0x4853_454D),
                }),
                source: fparkan_prototype::PrototypeSource::DirectArchive,
                dependencies: Vec::new(),
            },
            EffectivePrototype {
                key: fparkan_prototype::PrototypeKey(resource_name(b"tree_b")),
                geometry: PrototypeGeometry::Mesh(ResourceKey {
                    archive: parse_path("static.rlb").expect("archive"),
                    name: resource_name(b"tree_b.msh"),
                    type_id: Some(0x4853_454D),
                }),
                source: fparkan_prototype::PrototypeSource::DirectArchive,
                dependencies: Vec::new(),
            },
        ];
        (repo, prototypes)
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

    #[derive(Clone, Copy)]
    struct TestNresEntry<'a> {
        name: &'a [u8],
        payload: &'a [u8],
        type_id: u32,
        attr2: u32,
    }

    fn repository_with_archives_meta(
        archives: &[(&str, &[TestNresEntry<'_>])],
    ) -> CachedResourceRepository {
        let mut vfs = MemoryVfs::default();
        for (archive, entries) in archives {
            let path = parse_path(archive).expect("archive path");
            vfs.insert(
                path,
                Arc::from(build_nres_with_meta(entries).into_boxed_slice()),
            );
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

    fn mat0_with_texture(texture: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0; 4 + 34];
        bytes[0..2].copy_from_slice(&1_u16.to_le_bytes());
        let len = texture.len().min(16);
        bytes[22..22 + len].copy_from_slice(&texture[..len]);
        bytes
    }

    fn minimal_model_archive() -> Vec<u8> {
        struct MshEntry<'a> {
            type_id: u32,
            attr3: u32,
            name: &'a [u8],
            payload: &'a [u8],
        }

        let entries = [
            MshEntry {
                type_id: 1,
                attr3: 38,
                name: b"Res1",
                payload: &[],
            },
            MshEntry {
                type_id: 2,
                attr3: 0,
                name: b"Res2",
                payload: &[0; 0x8c],
            },
            MshEntry {
                type_id: 3,
                attr3: 0,
                name: b"Res3",
                payload: &[],
            },
            MshEntry {
                type_id: 6,
                attr3: 0,
                name: b"Res6",
                payload: &[],
            },
            MshEntry {
                type_id: 13,
                attr3: 0,
                name: b"Res13",
                payload: &[],
            },
        ];

        let mut out = vec![0; 16];
        let mut offsets = Vec::with_capacity(entries.len());
        for entry in &entries {
            offsets.push(u32::try_from(out.len()).expect("offset"));
            out.extend_from_slice(entry.payload);
            let padding = (8 - (out.len() % 8)) % 8;
            out.resize(out.len() + padding, 0);
        }
        let mut order: Vec<usize> = (0..entries.len()).collect();
        order.sort_by(|left, right| entries[*left].name.cmp(entries[*right].name));
        for (idx, entry) in entries.iter().enumerate() {
            push_u32(&mut out, entry.type_id);
            push_u32(&mut out, 0);
            push_u32(&mut out, 0);
            push_u32(
                &mut out,
                u32::try_from(entry.payload.len()).expect("payload"),
            );
            push_u32(&mut out, entry.attr3);
            let mut name_raw = [0; 36];
            let len = name_raw.len().saturating_sub(1).min(entry.name.len());
            name_raw[..len].copy_from_slice(&entry.name[..len]);
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

    fn prototype_graph_for_mesh(prototype: &EffectivePrototype) -> PrototypeGraph {
        let root_node = fparkan_prototype::PrototypeGraphNode::root(
            fparkan_prototype::PrototypeKey(prototype.key.0.clone()),
            false,
            fparkan_prototype::PrototypeGraphNodeId(0),
        );
        let prototype_node = fparkan_prototype::PrototypeGraphNode::prototype(
            prototype.key.clone(),
            fparkan_prototype::PrototypeGraphNodeId(1),
        );
        let mesh_key = match &prototype.geometry {
            PrototypeGeometry::Mesh(mesh) => mesh.clone(),
            PrototypeGeometry::NonGeometric => panic!("mesh prototype expected"),
        };
        let mesh_node = fparkan_prototype::PrototypeGraphNode::mesh(
            mesh_key,
            fparkan_prototype::PrototypeGraphNodeId(2),
        );
        PrototypeGraph {
            roots: vec![prototype.key.clone()],
            prototype_requests: vec![prototype.key.clone()],
            root_prototype_request_spans: std::iter::once(0..1).collect(),
            visual_dependencies_expanded: false,
            nodes: vec![root_node, prototype_node, mesh_node],
            edges: vec![
                fparkan_prototype::PrototypeGraphEdgeInstance {
                    id: fparkan_prototype::PrototypeGraphEdgeId(0),
                    from: fparkan_prototype::PrototypeGraphNodeId(0),
                    to: fparkan_prototype::PrototypeGraphNodeId(1),
                    kind: fparkan_prototype::PrototypeGraphEdgeKind::MissionToRoot,
                    requiredness: PrototypeGraphRequiredness::Required,
                    provenance: None,
                },
                fparkan_prototype::PrototypeGraphEdgeInstance {
                    id: fparkan_prototype::PrototypeGraphEdgeId(1),
                    from: fparkan_prototype::PrototypeGraphNodeId(1),
                    to: fparkan_prototype::PrototypeGraphNodeId(2),
                    kind: fparkan_prototype::PrototypeGraphEdgeKind::PrototypeToMesh,
                    requiredness: PrototypeGraphRequiredness::Required,
                    provenance: None,
                },
            ],
        }
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

    fn build_nres_with_meta(entries: &[TestNresEntry<'_>]) -> Vec<u8> {
        let mut out = vec![0; 16];
        let mut offsets = Vec::with_capacity(entries.len());
        for entry in entries {
            offsets.push(u32::try_from(out.len()).expect("offset"));
            out.extend_from_slice(entry.payload);
            let padding = (8 - (out.len() % 8)) % 8;
            out.resize(out.len() + padding, 0);
        }
        let mut order: Vec<usize> = (0..entries.len()).collect();
        order.sort_by(|left, right| entries[*left].name.cmp(entries[*right].name));
        for (idx, entry) in entries.iter().enumerate() {
            push_u32(&mut out, entry.type_id);
            push_u32(&mut out, 0);
            push_u32(&mut out, entry.attr2);
            push_u32(
                &mut out,
                u32::try_from(entry.payload.len()).expect("payload"),
            );
            push_u32(&mut out, 0);
            let mut name_raw = [0; 36];
            let len = name_raw.len().saturating_sub(1).min(entry.name.len());
            name_raw[..len].copy_from_slice(&entry.name[..len]);
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
