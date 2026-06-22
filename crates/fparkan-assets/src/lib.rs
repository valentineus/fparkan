#![forbid(unsafe_code)]
//! Asset manager ports and transactional preparation models.

use fparkan_material::{decode_wear, resolve_material, WEAR_KIND};
use fparkan_msh::{decode_msh, validate_msh};
use fparkan_nres::{decode as decode_nres, ReadProfile};
use fparkan_path::{normalize_relative, NormalizedPath, PathPolicy, ResourceName};
use fparkan_prototype::{EffectivePrototype, PrototypeGeometry, PrototypeGraph};
use fparkan_resource::{ResourceError, ResourceKey, ResourceRepository};
use fparkan_texm::decode_texm;
use std::collections::BTreeSet;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::sync::Arc;

const TEXTURES_ARCHIVE: &str = "textures.lib";
const LIGHTMAP_ARCHIVE: &str = "lightmap.lib";

/// Stable typed identifier for a prepared asset.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct AssetId<T> {
    raw: u64,
    marker: PhantomData<T>,
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
    /// Number of texture phase requests decoded as TEXM.
    pub texture_count: usize,
    /// Number of lightmap requests decoded as TEXM.
    pub lightmap_count: usize,
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AssetError {
    /// A required cross-resource dependency was not found.
    MissingDependency(String),
    /// A prototype did not describe a usable visual.
    InvalidPrototype(String),
    /// A repository operation failed.
    Resource(String),
    /// MSH parsing or validation failed.
    Msh(String),
    /// WEAR/MAT0 parsing or resolution failed.
    Material(String),
    /// TEXM parsing failed.
    Texture(String),
}

impl fmt::Display for AssetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingDependency(value) => write!(f, "missing dependency: {value}"),
            Self::InvalidPrototype(value) => write!(f, "invalid prototype: {value}"),
            Self::Resource(value) => write!(f, "resource error: {value}"),
            Self::Msh(value) => write!(f, "msh error: {value}"),
            Self::Material(value) => write!(f, "material error: {value}"),
            Self::Texture(value) => write!(f, "texture error: {value}"),
        }
    }
}

impl std::error::Error for AssetError {}

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

    /// Builds a mission plan by preparing each resolved prototype.
    ///
    /// # Errors
    ///
    /// Returns [`AssetError`] if any visual dependency is missing or malformed.
    pub fn build_mission_asset_plan<'a>(
        &self,
        prototypes: impl IntoIterator<Item = &'a EffectivePrototype>,
    ) -> Result<MissionAssetPlan, AssetError> {
        build_mission_asset_plan_with_repository(&self.repository, prototypes)
    }
}

/// Produces a count-only plan from a prototype graph.
#[must_use]
pub fn build_mission_asset_plan(graph: &PrototypeGraph) -> MissionAssetPlan {
    MissionAssetPlan {
        visual_count: graph.prototype_requests.len(),
        ..MissionAssetPlan::default()
    }
}

/// Builds a fully validated CPU-side mission asset plan.
///
/// # Errors
///
/// Returns [`AssetError`] if any reachable visual dependency is missing or
/// malformed.
pub fn build_mission_asset_plan_with_repository<'a, R: ResourceRepository>(
    repository: &R,
    prototypes: impl IntoIterator<Item = &'a EffectivePrototype>,
) -> Result<MissionAssetPlan, AssetError> {
    let mut plan = MissionAssetPlan::default();
    let mut prepared_visuals = BTreeSet::new();

    for proto in prototypes {
        let visual_id = stable_visual_id(proto);
        if !prepared_visuals.insert(visual_id) {
            continue;
        }
        let visual = prepare_visual_with_repository(repository, proto)?;
        plan.visual_count += 1;
        if visual.mesh.is_some() {
            plan.model_count += 1;
        }
        plan.material_count += visual.material_count;
        plan.texture_count += visual.texture_count;
        plan.lightmap_count += visual.lightmap_count;
    }

    Ok(plan)
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
    let PrototypeGeometry::Mesh(mesh_key) = &proto.geometry else {
        return prepare_visual(proto);
    };

    let nres = decode_nres(
        read_key(repository, mesh_key, Some("mesh"))?,
        ReadProfile::Compatible,
    )
    .map_err(|err| AssetError::Msh(err.to_string()))?;
    let msh_document = decode_msh(&nres).map_err(|err| AssetError::Msh(err.to_string()))?;
    let model = validate_msh(&msh_document).map_err(|err| AssetError::Msh(err.to_string()))?;

    let wear_name = sibling_name(mesh_key, "wea")?;
    let wear_key = ResourceKey {
        archive: mesh_key.archive.clone(),
        name: wear_name,
        type_id: Some(WEAR_KIND),
    };
    let wear = decode_wear(&read_key(repository, &wear_key, Some("wear"))?)
        .map_err(|err| AssetError::Material(err.to_string()))?;

    let mut material_count = 0;
    let mut texture_count = 0;
    let mut lightmap_count = 0;
    for material_index in 0..wear.entries.len() {
        let material_index = u16::try_from(material_index).map_err(|_| {
            AssetError::Material("material index does not fit archive format".to_string())
        })?;
        let material = resolve_material(repository, &wear, material_index)
            .map_err(|err| AssetError::Material(err.to_string()))?;
        material_count += 1;

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
        texture_count,
        lightmap_count,
    })
}

fn read_key<R: ResourceRepository>(
    repository: &R,
    key: &ResourceKey,
    label: Option<&str>,
) -> Result<Arc<[u8]>, AssetError> {
    let handle = repository
        .open_archive(&key.archive)
        .map_err(|err| AssetError::Resource(format!("{label:?} {key:?}: {err}")))
        .and_then(|archive| {
            repository
                .find(archive, &key.name)
                .map_err(|err| AssetError::Resource(format!("{label:?} {key:?}: {err}")))
        })?
        .ok_or_else(|| AssetError::MissingDependency(format!("{label:?} {key:?}")))?;
    let bytes = repository
        .read(handle)
        .map_err(|err| AssetError::Resource(format!("{label:?} {key:?}: {err}")))?;
    Ok(Arc::from(bytes.into_owned()))
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
    decode_texm(bytes)
        .map(|_| ())
        .map_err(|err| AssetError::Texture(err.to_string()))
}

fn read_optional_key<R: ResourceRepository>(
    repository: &R,
    key: &ResourceKey,
    label: Option<&str>,
) -> Result<Option<Arc<[u8]>>, AssetError> {
    let archive = match repository.open_archive(&key.archive) {
        Ok(archive) => archive,
        Err(ResourceError::MissingArchive | ResourceError::MissingEntry) => return Ok(None),
        Err(err) => return Err(AssetError::Resource(format!("{label:?} {key:?}: {err}"))),
    };
    let Some(handle) = repository
        .find(archive, &key.name)
        .map_err(|err| AssetError::Resource(format!("{label:?} {key:?}: {err}")))?
    else {
        return Ok(None);
    };
    let bytes = repository
        .read(handle)
        .map_err(|err| AssetError::Resource(format!("{label:?} {key:?}: {err}")))?;
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
