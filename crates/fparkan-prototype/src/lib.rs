#![forbid(unsafe_code)]
//! Prototype registry and unit DAT primitives.

use encoding_rs::WINDOWS_1251;
use fparkan_binary::{checked_count_bytes, Cursor, DecodeError};
use fparkan_material::{decode_wear, resolve_material, WEAR_KIND};
use fparkan_msh::{decode_msh, validate_msh, MshError};
use fparkan_nres::ReadProfile;
use fparkan_path::{normalize_relative, NormalizedPath, PathPolicy, ResourceName};
use fparkan_resource::{
    archive_path, resource_name, ResourceError, ResourceKey, ResourceRepository,
};
use fparkan_texm::decode_texm;
use fparkan_vfs::{Vfs, VfsError};
use std::sync::Arc;

const MESH_KIND: u32 = 0x4853_454D;
const UNIT_DAT_MIN_SIZE: usize = 0x48;
const UNIT_DAT_MAGIC: u32 = 0x0000_F0F1;
const PROTOTYPE_INHERITANCE_DEPTH_LIMIT: usize = 32;

/// Prototype key.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PrototypeKey(pub ResourceName);

/// 64-byte object reference record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectRefRecord {
    /// Archive raw bytes.
    pub archive_raw: [u8; 32],
    /// Resource raw bytes.
    pub resource_raw: [u8; 32],
}

/// Unit DAT document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnitDat {
    /// Opaque eight-byte header before component records.
    pub header_opaque: [u8; 8],
    /// Component records.
    pub records: Vec<UnitComponentRecord>,
}

/// Unit DAT binding used by mission object references.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnitDatBinding {
    /// Flags.
    pub flags: u32,
    /// Archive raw bytes.
    pub archive_raw: [u8; 32],
    /// Model key raw bytes.
    pub model_raw: [u8; 32],
}

/// Unit DAT component.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnitComponentRecord {
    /// Archive raw bytes.
    pub archive_raw: [u8; 32],
    /// Resource raw bytes.
    pub resource_raw: [u8; 32],
    /// Component kind.
    pub kind: u32,
    /// Parent or link.
    pub parent_or_link: i32,
    /// Description raw bytes.
    pub description_raw: [u8; 32],
    /// Opaque tail.
    pub tail0: u32,
    /// Opaque tail.
    pub tail1: u32,
}

/// Prototype geometry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PrototypeGeometry {
    /// Mesh resource.
    Mesh(ResourceKey),
    /// Valid non-geometric prototype.
    NonGeometric,
}

/// Effective prototype.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectivePrototype {
    /// Key.
    pub key: PrototypeKey,
    /// Geometry.
    pub geometry: PrototypeGeometry,
    /// Resolution source.
    pub source: PrototypeSource,
    /// Resource dependencies discovered while resolving this prototype.
    pub dependencies: Vec<ResourceKey>,
}

/// Prototype resolution source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrototypeSource {
    /// Direct archive/key lookup.
    DirectArchive,
    /// `objects.rlb` registry lookup.
    ObjectsRegistry,
    /// Unit DAT binding.
    UnitDat,
}

/// Prototype graph.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PrototypeGraph {
    /// Requested keys.
    pub roots: Vec<PrototypeKey>,
    /// Effective prototype requests after unit DAT expansion.
    pub prototype_requests: Vec<PrototypeKey>,
}

/// Mission prototype dependency graph report.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PrototypeGraphReport {
    /// Requested mission roots.
    pub root_count: usize,
    /// Roots that point at unit DAT files.
    pub unit_reference_count: usize,
    /// Roots that point directly at prototype keys.
    pub direct_reference_count: usize,
    /// Component records reached from unit DAT files.
    pub unit_component_count: usize,
    /// Prototype requests that resolved to an effective prototype.
    pub resolved_count: usize,
    /// Mesh dependencies reached by resolved prototypes.
    pub mesh_dependency_count: usize,
    /// WEAR requests derived from reached mesh dependencies.
    pub wear_request_count: usize,
    /// WEAR entries successfully decoded.
    pub wear_resolved_count: usize,
    /// Material slots requested by decoded WEAR tables.
    pub material_slot_count: usize,
    /// MAT0 material entries successfully decoded.
    pub material_resolved_count: usize,
    /// Texture requests derived from MAT0 texture phases.
    pub texture_request_count: usize,
    /// Texm texture entries successfully decoded.
    pub texture_resolved_count: usize,
    /// Lightmap requests declared by decoded WEAR tables.
    pub lightmap_request_count: usize,
    /// Lightmap Texm entries successfully decoded.
    pub lightmap_resolved_count: usize,
    /// Graph failures tied to mission root edges.
    pub failures: Vec<PrototypeGraphFailure>,
}

impl PrototypeGraphReport {
    /// Returns true when all reachable mission roots resolved.
    #[must_use]
    pub fn is_success(&self) -> bool {
        self.failures.is_empty()
            && self.resolved_count == self.direct_reference_count + self.unit_component_count
    }
}

/// Prototype graph failure tied to a root edge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrototypeGraphFailure {
    /// Root index in the requested mission order.
    pub root_index: usize,
    /// Raw mission resource bytes.
    pub resource_raw: Vec<u8>,
    /// Edge that failed.
    pub edge: PrototypeGraphEdge,
    /// Failure detail.
    pub message: String,
}

/// Prototype graph edge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrototypeGraphEdge {
    /// Mission object to unit DAT binding.
    MissionToUnitDat,
    /// Mission object to `objects.rlb` registry.
    MissionToObjectsRegistry,
    /// Unit DAT component to prototype key.
    UnitDatToComponent,
    /// Resolved prototype to mesh archive/resource.
    PrototypeToMesh,
    /// Mesh resource to matching WEAR table.
    MeshToWear,
    /// WEAR material slot to MAT0.
    WearToMaterial,
    /// MAT0 phase to Texm.
    MaterialToTexture,
    /// WEAR lightmap slot to lightmap Texm.
    WearToLightmap,
}

/// Prototype error.
#[derive(Debug)]
pub enum PrototypeError {
    /// Decode error.
    Decode(DecodeError),
    /// Invalid size.
    InvalidSize,
    /// Invalid unit DAT magic.
    InvalidUnitDatMagic(u32),
    /// Invalid path.
    InvalidPath(String),
    /// VFS error.
    Vfs(String),
    /// Resource repository error.
    Resource(String),
    /// Referenced mesh is present but invalid.
    InvalidMesh(String),
}

impl From<DecodeError> for PrototypeError {
    fn from(value: DecodeError) -> Self {
        Self::Decode(value)
    }
}

impl From<ResourceError> for PrototypeError {
    fn from(value: ResourceError) -> Self {
        Self::Resource(value.to_string())
    }
}

impl From<MshError> for PrototypeError {
    fn from(value: MshError) -> Self {
        Self::InvalidMesh(value.to_string())
    }
}

impl From<VfsError> for PrototypeError {
    fn from(value: VfsError) -> Self {
        Self::Vfs(value.to_string())
    }
}

impl std::fmt::Display for PrototypeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for PrototypeError {}

/// Decodes an `objects.rlb` registry entry as 64-byte records.
///
/// # Errors
///
/// Returns [`PrototypeError::InvalidSize`] when the payload is not composed of
/// whole 64-byte records.
pub fn decode_registry_entry(payload: &[u8]) -> Result<Vec<ObjectRefRecord>, PrototypeError> {
    if !payload.len().is_multiple_of(64) {
        return Err(PrototypeError::InvalidSize);
    }
    let mut out = Vec::with_capacity(payload.len() / 64);
    for chunk in payload.chunks_exact(64) {
        let mut archive_raw = [0; 32];
        let mut resource_raw = [0; 32];
        archive_raw.copy_from_slice(&chunk[..32]);
        resource_raw.copy_from_slice(&chunk[32..64]);
        out.push(ObjectRefRecord {
            archive_raw,
            resource_raw,
        });
    }
    Ok(out)
}

/// Decodes unit DAT as an eight-byte header followed by `N * 112` bytes.
///
/// # Errors
///
/// Returns [`PrototypeError`] when the payload is too small or contains a
/// partial component record.
pub fn decode_unit_dat(payload: &[u8]) -> Result<UnitDat, PrototypeError> {
    if payload.len() < 8 {
        return Err(PrototypeError::InvalidSize);
    }
    let mut header_opaque = [0; 8];
    header_opaque.copy_from_slice(&payload[..8]);
    let remaining = payload.len().saturating_sub(8) as u64;
    if !remaining.is_multiple_of(112) {
        return Err(PrototypeError::InvalidSize);
    }
    let record_count = remaining / 112;
    let bytes = checked_count_bytes(record_count, 112, remaining)?;
    if bytes as u64 != remaining {
        return Err(PrototypeError::InvalidSize);
    }
    let mut cursor = Cursor::new(&payload[8..]);
    let mut records = Vec::with_capacity(
        usize::try_from(record_count).map_err(|_| DecodeError::IntegerOverflow)?,
    );
    for _ in 0..record_count {
        let mut archive_raw = [0; 32];
        let mut resource_raw = [0; 32];
        let mut description_raw = [0; 32];
        archive_raw.copy_from_slice(cursor.read_exact(32)?);
        resource_raw.copy_from_slice(cursor.read_exact(32)?);
        let kind = cursor.read_u32_le()?;
        let parent_or_link = cursor.read_i32_le()?;
        description_raw.copy_from_slice(cursor.read_exact(32)?);
        let tail0 = cursor.read_u32_le()?;
        let tail1 = cursor.read_u32_le()?;
        records.push(UnitComponentRecord {
            archive_raw,
            resource_raw,
            kind,
            parent_or_link,
            description_raw,
            tail0,
            tail1,
        });
    }
    cursor.require_eof()?;
    Ok(UnitDat {
        header_opaque,
        records,
    })
}

/// Decodes a mission unit DAT binding.
///
/// # Errors
///
/// Returns [`PrototypeError`] when the DAT file is too small, has the wrong
/// magic, or does not contain both archive and model keys.
pub fn decode_unit_dat_binding(payload: &[u8]) -> Result<UnitDatBinding, PrototypeError> {
    if payload.len() < UNIT_DAT_MIN_SIZE {
        return Err(PrototypeError::InvalidSize);
    }
    let magic = u32::from_le_bytes(
        payload[0..4]
            .try_into()
            .map_err(|_| PrototypeError::InvalidSize)?,
    );
    if magic != UNIT_DAT_MAGIC {
        return Err(PrototypeError::InvalidUnitDatMagic(magic));
    }
    let flags = u32::from_le_bytes(
        payload[4..8]
            .try_into()
            .map_err(|_| PrototypeError::InvalidSize)?,
    );
    let mut archive_raw = [0; 32];
    let mut model_raw = [0; 32];
    archive_raw.copy_from_slice(&payload[0x08..0x28]);
    model_raw.copy_from_slice(&payload[0x28..0x48]);
    if cstr_bytes(&archive_raw).is_empty() || cstr_bytes(&model_raw).is_empty() {
        return Err(PrototypeError::InvalidSize);
    }
    Ok(UnitDatBinding {
        flags,
        archive_raw,
        model_raw,
    })
}

/// Resolves one prototype request through unit DAT, `objects.rlb`, and direct mesh lookup.
///
/// # Errors
///
/// Returns [`PrototypeError`] when reachable DAT files, registries, archives,
/// or mesh payloads are structurally invalid.
pub fn resolve_prototype(
    repository: &dyn ResourceRepository,
    vfs: &dyn Vfs,
    resource: &ResourceName,
) -> Result<Option<EffectivePrototype>, PrototypeError> {
    if has_extension_bytes(&resource.0, b"dat") {
        return resolve_unit_dat_first_component(repository, vfs, resource);
    }

    resolve_direct_prototype(repository, resource)
}

fn resolve_direct_prototype(
    repository: &dyn ResourceRepository,
    resource: &ResourceName,
) -> Result<Option<EffectivePrototype>, PrototypeError> {
    let objects =
        archive_path(b"objects.rlb").map_err(|err| PrototypeError::InvalidPath(err.to_string()))?;
    resolve_archive_model(
        repository,
        &objects,
        resource,
        PrototypeSource::ObjectsRegistry,
    )
}

struct ResolvedPrototypeRequests {
    expected_count: usize,
    prototypes: Vec<EffectivePrototype>,
}

fn resolve_prototype_requests(
    repository: &dyn ResourceRepository,
    vfs: &dyn Vfs,
    resource: &ResourceName,
) -> Result<ResolvedPrototypeRequests, PrototypeError> {
    if has_extension_bytes(&resource.0, b"dat") {
        return resolve_unit_dat_prototype_requests(repository, vfs, resource);
    }

    let prototype = resolve_direct_prototype(repository, resource)?;
    Ok(ResolvedPrototypeRequests {
        expected_count: 1,
        prototypes: prototype.into_iter().collect(),
    })
}

fn resolve_unit_dat_first_component(
    repository: &dyn ResourceRepository,
    vfs: &dyn Vfs,
    resource: &ResourceName,
) -> Result<Option<EffectivePrototype>, PrototypeError> {
    let expansion = resolve_unit_dat_prototype_requests(repository, vfs, resource)?;
    Ok(expansion.prototypes.into_iter().next())
}

fn resolve_unit_dat_prototype_requests(
    repository: &dyn ResourceRepository,
    vfs: &dyn Vfs,
    resource: &ResourceName,
) -> Result<ResolvedPrototypeRequests, PrototypeError> {
    let dat_path = normalized_path_from_name(resource)?;
    let bytes = match vfs.read(&dat_path) {
        Ok(bytes) => bytes,
        Err(VfsError::NotFound(_)) => {
            return Ok(ResolvedPrototypeRequests {
                expected_count: 0,
                prototypes: Vec::new(),
            });
        }
        Err(err) => return Err(err.into()),
    };

    if let Ok(unit) = decode_unit_dat(&bytes) {
        if !unit.records.is_empty() {
            let mut prototypes = Vec::with_capacity(unit.records.len());
            for record in &unit.records {
                let prototype = resolve_unit_component(repository, record)?.ok_or_else(|| {
                    PrototypeError::Resource(format!(
                        "unit component {} did not resolve",
                        String::from_utf8_lossy(cstr_bytes(&record.resource_raw))
                    ))
                })?;
                prototypes.push(prototype);
            }
            return Ok(ResolvedPrototypeRequests {
                expected_count: unit.records.len(),
                prototypes,
            });
        }
    }

    let binding = decode_unit_dat_binding(&bytes)?;
    let archive =
        normalized_path_from_name(&ResourceName(cstr_bytes(&binding.archive_raw).to_vec()))?;
    let model = ResourceName(cstr_bytes(&binding.model_raw).to_vec());
    let prototype = resolve_archive_model(repository, &archive, &model, PrototypeSource::UnitDat)?;
    Ok(ResolvedPrototypeRequests {
        expected_count: 1,
        prototypes: prototype.into_iter().collect(),
    })
}

fn resolve_unit_component(
    repository: &dyn ResourceRepository,
    record: &UnitComponentRecord,
) -> Result<Option<EffectivePrototype>, PrototypeError> {
    let archive =
        normalized_path_from_name(&ResourceName(cstr_bytes(&record.archive_raw).to_vec()))?;
    let resource = ResourceName(cstr_bytes(&record.resource_raw).to_vec());
    if resource.0.is_empty() {
        return Ok(None);
    }
    resolve_archive_model(repository, &archive, &resource, PrototypeSource::UnitDat)
}

/// Resolves many roots and records every resolved root in a graph.
///
/// # Errors
///
/// Returns [`PrototypeError`] when any reachable root fails with a structural
/// error.
pub fn build_prototype_graph(
    repository: &dyn ResourceRepository,
    vfs: &dyn Vfs,
    roots: &[ResourceName],
) -> Result<(PrototypeGraph, Vec<EffectivePrototype>), PrototypeError> {
    let mut graph = PrototypeGraph::default();
    let mut resolved = Vec::new();
    for root in roots {
        let key = PrototypeKey(root.clone());
        graph.roots.push(key);
        let expansion = resolve_prototype_requests(repository, vfs, root)?;
        for prototype in expansion.prototypes {
            graph.prototype_requests.push(prototype.key.clone());
            resolved.push(prototype);
        }
    }
    Ok((graph, resolved))
}

/// Resolves many mission roots and records edge-specific graph failures.
///
/// This function reports per-root failures in [`PrototypeGraphReport`] instead
/// of returning early.
pub fn build_prototype_graph_report(
    repository: &dyn ResourceRepository,
    vfs: &dyn Vfs,
    roots: &[ResourceName],
) -> (
    PrototypeGraph,
    Vec<EffectivePrototype>,
    PrototypeGraphReport,
) {
    let mut graph = PrototypeGraph::default();
    let mut resolved = Vec::new();
    let mut report = PrototypeGraphReport {
        root_count: roots.len(),
        ..PrototypeGraphReport::default()
    };

    for (root_index, root) in roots.iter().enumerate() {
        graph.roots.push(PrototypeKey(root.clone()));
        let edge = if has_extension_bytes(&root.0, b"dat") {
            report.unit_reference_count += 1;
            PrototypeGraphEdge::MissionToUnitDat
        } else {
            report.direct_reference_count += 1;
            PrototypeGraphEdge::MissionToObjectsRegistry
        };

        match resolve_prototype_requests(repository, vfs, root) {
            Ok(expansion) => {
                let expected = expansion.expected_count;
                if edge == PrototypeGraphEdge::MissionToUnitDat {
                    report.unit_component_count += expected;
                }
                let actual = expansion.prototypes.len();
                for prototype in expansion.prototypes {
                    graph.prototype_requests.push(prototype.key.clone());
                    report.resolved_count += 1;
                    report.mesh_dependency_count += prototype.dependencies.len();
                    resolved.push(prototype);
                }
                if actual < expected {
                    report.failures.push(PrototypeGraphFailure {
                        root_index,
                        resource_raw: root.0.clone(),
                        edge,
                        message: "resource did not resolve to an effective prototype".to_string(),
                    });
                }
            }
            Err(err) => report.failures.push(PrototypeGraphFailure {
                root_index,
                resource_raw: root.0.clone(),
                edge: graph_error_edge(edge, &err),
                message: err.to_string(),
            }),
        }
    }

    (graph, resolved, report)
}

/// Extends a graph report by validating visual dependencies for each resolved
/// prototype.
pub fn extend_graph_report_with_visual_dependencies(
    repository: &dyn ResourceRepository,
    report: &mut PrototypeGraphReport,
    prototypes: &[EffectivePrototype],
) {
    let texture_archive = archive_path(b"textures.lib").ok();
    let lightmap_archive = archive_path(b"lightmap.lib").ok();
    for (prototype_index, prototype) in prototypes.iter().enumerate() {
        let PrototypeGeometry::Mesh(mesh) = &prototype.geometry else {
            continue;
        };
        report.wear_request_count += 1;
        match resolve_wear_table(repository, mesh) {
            Ok(table) => {
                report.wear_resolved_count += 1;
                report.material_slot_count += table.entries.len();
                for (material_index, _entry) in table.entries.iter().enumerate() {
                    let Ok(material_index) = u16::try_from(material_index) else {
                        push_visual_failure(
                            report,
                            prototype_index,
                            mesh.name.0.clone(),
                            PrototypeGraphEdge::WearToMaterial,
                            "material index does not fit WEAR selector",
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
                                    Err(message) => push_visual_failure(
                                        report,
                                        prototype_index,
                                        texture.0,
                                        PrototypeGraphEdge::MaterialToTexture,
                                        &message,
                                    ),
                                }
                            }
                        }
                        Err(err) => push_visual_failure(
                            report,
                            prototype_index,
                            mesh.name.0.clone(),
                            PrototypeGraphEdge::WearToMaterial,
                            &err.to_string(),
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
                        Err(message) => push_visual_failure(
                            report,
                            prototype_index,
                            lightmap.lightmap.0.clone(),
                            PrototypeGraphEdge::WearToLightmap,
                            &message,
                        ),
                    }
                }
            }
            Err(message) => push_visual_failure(
                report,
                prototype_index,
                mesh.name.0.clone(),
                PrototypeGraphEdge::MeshToWear,
                &message,
            ),
        }
    }
}

fn resolve_wear_table(
    repository: &dyn ResourceRepository,
    mesh: &ResourceKey,
) -> Result<fparkan_material::WearTable, String> {
    let archive = repository
        .open_archive(&mesh.archive)
        .map_err(|err| err.to_string())?;
    let wear_name = derive_wear_name(&mesh.name)
        .ok_or_else(|| "cannot derive WEAR name from mesh resource".to_string())?;
    let handle = repository
        .find(archive, &wear_name)
        .map_err(|err| err.to_string())?
        .ok_or_else(|| {
            format!(
                "missing WEAR entry {}",
                String::from_utf8_lossy(&wear_name.0)
            )
        })?;
    let info = repository
        .entry_info(handle)
        .map_err(|err| err.to_string())?;
    if info.key.type_id != Some(WEAR_KIND) {
        return Err(format!(
            "entry {} is not WEAR",
            String::from_utf8_lossy(&wear_name.0)
        ));
    }
    let bytes = repository
        .read(handle)
        .map_err(|err| err.to_string())?
        .into_owned();
    decode_wear(&bytes).map_err(|err| err.to_string())
}

fn resolve_texm_from_candidates<'a>(
    repository: &dyn ResourceRepository,
    texture: &ResourceName,
    candidates: impl IntoIterator<Item = Option<&'a NormalizedPath>>,
) -> Result<(), String> {
    let mut missing_archive = false;
    for path in candidates.into_iter().flatten() {
        let archive = match repository.open_archive(path) {
            Ok(archive) => archive,
            Err(ResourceError::MissingArchive) => {
                missing_archive = true;
                continue;
            }
            Err(err) => return Err(err.to_string()),
        };
        let Some(handle) = repository
            .find(archive, texture)
            .map_err(|err| err.to_string())?
        else {
            continue;
        };
        let bytes = repository
            .read(handle)
            .map_err(|err| err.to_string())?
            .into_owned();
        decode_texm(Arc::from(bytes.into_boxed_slice())).map_err(|err| err.to_string())?;
        return Ok(());
    }
    if missing_archive {
        Err(format!(
            "texture archive missing for {}",
            String::from_utf8_lossy(&texture.0)
        ))
    } else {
        Err(format!(
            "missing texture {}",
            String::from_utf8_lossy(&texture.0)
        ))
    }
}

fn push_visual_failure(
    report: &mut PrototypeGraphReport,
    prototype_index: usize,
    resource_raw: Vec<u8>,
    edge: PrototypeGraphEdge,
    message: &str,
) {
    report.failures.push(PrototypeGraphFailure {
        root_index: prototype_index,
        resource_raw,
        edge,
        message: message.to_string(),
    });
}

fn derive_wear_name(model_name: &ResourceName) -> Option<ResourceName> {
    let stem = file_stem_bytes(&model_name.0);
    if stem.is_empty() {
        return None;
    }
    let mut out = stem.to_vec();
    out.extend_from_slice(b".wea");
    Some(ResourceName(out))
}

fn graph_error_edge(edge: PrototypeGraphEdge, err: &PrototypeError) -> PrototypeGraphEdge {
    match err {
        PrototypeError::InvalidMesh(_) => PrototypeGraphEdge::PrototypeToMesh,
        PrototypeError::Decode(_)
        | PrototypeError::InvalidSize
        | PrototypeError::InvalidUnitDatMagic(_)
        | PrototypeError::InvalidPath(_)
        | PrototypeError::Vfs(_)
        | PrototypeError::Resource(_) => edge,
    }
}

fn resolve_archive_model(
    repository: &dyn ResourceRepository,
    archive: &NormalizedPath,
    model_key: &ResourceName,
    source: PrototypeSource,
) -> Result<Option<EffectivePrototype>, PrototypeError> {
    if archive.as_str().eq_ignore_ascii_case("objects.rlb") {
        if let Some(prototype) = resolve_objects_registry_model(repository, archive, model_key)? {
            return Ok(Some(prototype));
        }
    }

    let Some(mesh) = find_mesh_resource(repository, archive, model_key)? else {
        return Ok(None);
    };
    Ok(Some(effective(model_key.clone(), mesh, source)))
}

fn resolve_objects_registry_model(
    repository: &dyn ResourceRepository,
    registry_archive: &NormalizedPath,
    object_key: &ResourceName,
) -> Result<Option<EffectivePrototype>, PrototypeError> {
    let Some(refs) =
        collect_registry_refs(repository, registry_archive, object_key, &mut Vec::new(), 0)?
    else {
        return Ok(None);
    };

    let mut missing_mesh_refs = Vec::new();
    for item in refs.iter().filter(|item| is_explicit_mesh_ref(item)) {
        if let Some(prototype) =
            resolve_object_ref_model(repository, object_key, item, cstr_bytes(&item.resource_raw))?
        {
            return Ok(Some(prototype));
        }
        missing_mesh_refs.push(describe_object_ref(item));
    }
    if !missing_mesh_refs.is_empty() {
        return Err(PrototypeError::Resource(format!(
            "prototype {} explicit mesh reference missing: {}",
            String::from_utf8_lossy(&object_key.0),
            missing_mesh_refs.join(" -> ")
        )));
    }

    Ok(Some(EffectivePrototype {
        key: PrototypeKey(object_key.clone()),
        geometry: PrototypeGeometry::NonGeometric,
        source: PrototypeSource::ObjectsRegistry,
        dependencies: Vec::new(),
    }))
}

fn collect_registry_refs(
    repository: &dyn ResourceRepository,
    registry_archive: &NormalizedPath,
    object_key: &ResourceName,
    stack: &mut Vec<ResourceName>,
    depth: usize,
) -> Result<Option<Vec<ObjectRefRecord>>, PrototypeError> {
    if depth > PROTOTYPE_INHERITANCE_DEPTH_LIMIT {
        return Err(PrototypeError::Resource(format!(
            "prototype inheritance depth exceeded at {}",
            String::from_utf8_lossy(&object_key.0)
        )));
    }
    if stack
        .iter()
        .any(|item| eq_ignore_ascii_case(&item.0, &object_key.0))
    {
        return Err(PrototypeError::Resource(format!(
            "prototype inheritance cycle at {}",
            String::from_utf8_lossy(&object_key.0)
        )));
    }
    let archive_id = match repository.open_archive(registry_archive) {
        Ok(id) => id,
        Err(ResourceError::MissingArchive) => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    let Some((registry_entry, _matched_name)) =
        find_any_candidate(repository, archive_id, &mesh_name_candidates(&object_key.0))?
    else {
        return Ok(None);
    };
    let payload = repository.read(registry_entry)?.into_owned();
    let refs = decode_registry_entry(&payload)?;
    let mut effective_refs = Vec::new();
    stack.push(object_key.clone());
    for item in refs {
        if archive_name_is(&item.archive_raw, b"objects.rlb") {
            let parent_key = ResourceName(cstr_bytes(&item.resource_raw).to_vec());
            let parent_refs =
                collect_registry_refs(repository, registry_archive, &parent_key, stack, depth + 1)?
                    .ok_or_else(|| {
                        PrototypeError::Resource(format!(
                            "missing parent prototype {}",
                            String::from_utf8_lossy(&parent_key.0)
                        ))
                    })?;
            effective_refs.extend(parent_refs);
        } else {
            effective_refs.push(item);
        }
    }
    stack.pop();

    Ok(Some(effective_refs))
}

fn resolve_object_ref_model(
    repository: &dyn ResourceRepository,
    requested: &ResourceName,
    item: &ObjectRefRecord,
    model_name: &[u8],
) -> Result<Option<EffectivePrototype>, PrototypeError> {
    let archive = normalized_path_from_name(&ResourceName(cstr_bytes(&item.archive_raw).to_vec()))?;
    let Some(mesh) = find_mesh_resource(repository, &archive, &ResourceName(model_name.to_vec()))?
    else {
        return Ok(None);
    };
    Ok(Some(effective(
        requested.clone(),
        mesh,
        PrototypeSource::ObjectsRegistry,
    )))
}

fn is_explicit_mesh_ref(item: &ObjectRefRecord) -> bool {
    has_extension_bytes(cstr_bytes(&item.resource_raw), b"msh")
}

fn describe_object_ref(item: &ObjectRefRecord) -> String {
    format!(
        "{}:{}",
        String::from_utf8_lossy(cstr_bytes(&item.archive_raw)),
        String::from_utf8_lossy(cstr_bytes(&item.resource_raw))
    )
}

fn find_mesh_resource(
    repository: &dyn ResourceRepository,
    archive: &NormalizedPath,
    model_key: &ResourceName,
) -> Result<Option<ResourceKey>, PrototypeError> {
    let archive_id = match repository.open_archive(archive) {
        Ok(id) => id,
        Err(ResourceError::MissingArchive) => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    let candidates = mesh_name_candidates(&model_key.0);
    let Some((handle, matched_name)) = find_any_candidate(repository, archive_id, &candidates)?
    else {
        return Ok(None);
    };
    validate_mesh_payload(repository.read(handle)?.into_owned())?;
    Ok(Some(ResourceKey {
        archive: archive.clone(),
        name: resource_name(matched_name),
        type_id: Some(MESH_KIND),
    }))
}

fn validate_mesh_payload(payload: Vec<u8>) -> Result<(), PrototypeError> {
    let nested = fparkan_nres::decode(
        Arc::from(payload.into_boxed_slice()),
        ReadProfile::Compatible,
    )
    .map_err(|err| PrototypeError::InvalidMesh(err.to_string()))?;
    let document = decode_msh(&nested)?;
    validate_msh(&document)?;
    Ok(())
}

fn find_any_candidate(
    repository: &dyn ResourceRepository,
    archive_id: fparkan_resource::ArchiveId,
    candidates: &[Vec<u8>],
) -> Result<Option<(fparkan_resource::EntryHandle, Vec<u8>)>, PrototypeError> {
    for candidate in candidates {
        if let Some(handle) = repository.find(archive_id, &resource_name(candidate))? {
            return Ok(Some((handle, candidate.clone())));
        }
    }
    Ok(None)
}

fn effective(
    requested: ResourceName,
    mesh: ResourceKey,
    source: PrototypeSource,
) -> EffectivePrototype {
    EffectivePrototype {
        key: PrototypeKey(requested),
        geometry: PrototypeGeometry::Mesh(mesh.clone()),
        source,
        dependencies: vec![mesh],
    }
}

fn mesh_name_candidates(name: &[u8]) -> Vec<Vec<u8>> {
    let trimmed = trim_ascii(name);
    if trimmed.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    push_unique_bytes(&mut out, trimmed.to_vec());
    if has_extension_bytes(trimmed, b"msh") {
        let stem = file_stem_bytes(trimmed);
        if !stem.is_empty() {
            push_unique_bytes(&mut out, stem.to_vec());
        }
    } else {
        let mut with_suffix = trimmed.to_vec();
        with_suffix.extend_from_slice(b".msh");
        push_unique_bytes(&mut out, with_suffix);
    }
    out
}

fn push_unique_bytes(items: &mut Vec<Vec<u8>>, value: Vec<u8>) {
    if !items.iter().any(|item| eq_ignore_ascii_case(item, &value)) {
        items.push(value);
    }
}

fn normalized_path_from_name(name: &ResourceName) -> Result<NormalizedPath, PrototypeError> {
    let text = legacy_path_text(cstr_bytes(&name.0));
    normalize_relative(text.as_bytes(), PathPolicy::StrictLegacy)
        .map_err(|err| PrototypeError::InvalidPath(err.to_string()))
}

fn legacy_path_text(raw: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(raw) {
        text.to_string()
    } else {
        let (decoded, _, _) = WINDOWS_1251.decode(raw);
        decoded.into_owned()
    }
}

fn cstr_bytes(raw: &[u8]) -> &[u8] {
    let len = raw.iter().position(|byte| *byte == 0).unwrap_or(raw.len());
    trim_ascii(&raw[..len])
}

fn archive_name_is(raw: &[u8], expected: &[u8]) -> bool {
    cstr_bytes(raw).eq_ignore_ascii_case(expected)
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let mut start = 0usize;
    let mut end = bytes.len();
    while start < end && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    &bytes[start..end]
}

fn has_extension_bytes(name: &[u8], ext: &[u8]) -> bool {
    let Some(pos) = name.iter().rposition(|byte| *byte == b'.') else {
        return false;
    };
    eq_ignore_ascii_case(&name[pos + 1..], ext)
}

fn file_stem_bytes(name: &[u8]) -> &[u8] {
    let file_name = name
        .iter()
        .rposition(|byte| *byte == b'/' || *byte == b'\\')
        .map_or(name, |pos| &name[pos + 1..]);
    let Some(dot) = file_name.iter().rposition(|byte| *byte == b'.') else {
        return file_name;
    };
    &file_name[..dot]
}

fn eq_ignore_ascii_case(left: &[u8], right: &[u8]) -> bool {
    left.eq_ignore_ascii_case(right)
}

/// Decodes FX/prototype bytes by preserving them for future typed support.
#[must_use]
pub fn preserve_payload(payload: &[u8]) -> Arc<[u8]> {
    Arc::from(payload.to_vec().into_boxed_slice())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_resource::{archive_path as resource_archive_path, CachedResourceRepository};
    use fparkan_vfs::{DirectoryVfs, MemoryVfs};
    use std::path::Path;

    #[test]
    fn registry_requires_record_multiple() {
        assert!(decode_registry_entry(&[0; 63]).is_err());
        assert_eq!(decode_registry_entry(&[0; 64]).expect("record").len(), 1);
    }

    #[test]
    fn registry_zero_records_payload_is_empty() {
        let records = decode_registry_entry(&[]).expect("empty registry");

        assert!(records.is_empty());
    }

    #[test]
    fn registry_preserves_bounded_name_tails_and_order() {
        let mut bytes = Vec::new();
        let mut first = [0u8; 64];
        first[..9].copy_from_slice(b"arch\0tail");
        first[32..40].copy_from_slice(b"res\0tail");
        bytes.extend_from_slice(&first);
        let mut second = [0u8; 64];
        second[..10].copy_from_slice(b"other.rlb\0");
        second[32..43].copy_from_slice(b"second.msh\0");
        bytes.extend_from_slice(&second);

        let records = decode_registry_entry(&bytes).expect("registry records");

        assert_eq!(records.len(), 2);
        assert_eq!(&records[0].archive_raw[..9], b"arch\0tail");
        assert_eq!(&records[0].resource_raw[..8], b"res\0tail");
        assert_eq!(cstr_bytes(&records[0].archive_raw), b"arch");
        assert_eq!(cstr_bytes(&records[1].resource_raw), b"second.msh");
    }

    #[test]
    fn unit_zero_records_uses_exact_size() {
        let bytes = [0_u8; 8];
        let unit = decode_unit_dat(&bytes).expect("unit");
        assert!(unit.records.is_empty());
    }

    #[test]
    fn unit_dat_one_record_uses_exact_size_formula() {
        let bytes = build_unit_dat(&[(b"objects.rlb".as_slice(), b"component".as_slice())]);
        let unit = decode_unit_dat(&bytes).expect("unit");

        assert_eq!(bytes.len(), 8 + 112);
        assert_eq!(unit.records.len(), 1);
        assert_eq!(cstr_bytes(&unit.records[0].archive_raw), b"objects.rlb");
        assert_eq!(cstr_bytes(&unit.records[0].resource_raw), b"component");
    }

    #[test]
    fn unit_dat_rejects_truncated_record() {
        let mut bytes = build_unit_dat(&[(b"objects.rlb".as_slice(), b"component".as_slice())]);
        bytes.pop();

        assert!(matches!(
            decode_unit_dat(&bytes),
            Err(PrototypeError::InvalidSize)
        ));
    }

    #[test]
    fn unit_dat_preserves_header_description_tail_and_parent_link() {
        let mut bytes = build_unit_dat(&[(b"objects.rlb".as_slice(), b"component".as_slice())]);
        bytes[0..8].copy_from_slice(&[0xF1, 0xF0, 1, 2, 3, 4, 5, 6]);
        bytes[8 + 68..8 + 72].copy_from_slice(&(-7_i32).to_le_bytes());
        let description = b"desc\0tail";
        bytes[8 + 72..8 + 72 + description.len()].copy_from_slice(description);
        bytes[8 + 104..8 + 108].copy_from_slice(&0x1122_3344_u32.to_le_bytes());
        bytes[8 + 108..8 + 112].copy_from_slice(&0x5566_7788_u32.to_le_bytes());

        let unit = decode_unit_dat(&bytes).expect("unit");
        let record = &unit.records[0];
        assert_eq!(unit.header_opaque, [0xF1, 0xF0, 1, 2, 3, 4, 5, 6]);
        assert_eq!(record.parent_or_link, -7);
        assert_eq!(&record.description_raw[..description.len()], description);
        assert_eq!(record.tail0, 0x1122_3344);
        assert_eq!(record.tail1, 0x5566_7788);
    }

    #[test]
    fn unit_dat_accepts_full_description_without_nul() {
        let mut bytes = build_unit_dat(&[(b"objects.rlb".as_slice(), b"component".as_slice())]);
        bytes[8 + 72..8 + 104].copy_from_slice(b"12345678901234567890123456789012");

        let unit = decode_unit_dat(&bytes).expect("unit");

        assert_eq!(
            &unit.records[0].description_raw,
            b"12345678901234567890123456789012"
        );
    }

    #[test]
    fn unit_dat_preserves_positive_parent_link() {
        let mut bytes = build_unit_dat(&[(b"objects.rlb".as_slice(), b"component".as_slice())]);
        bytes[8 + 68..8 + 72].copy_from_slice(&12_i32.to_le_bytes());

        let unit = decode_unit_dat(&bytes).expect("unit");

        assert_eq!(unit.records[0].parent_or_link, 12);
    }

    #[test]
    fn resolves_synthetic_objects_registry_model() {
        let mut vfs = MemoryVfs::default();
        let objects_path = resource_archive_path(b"objects.rlb").expect("objects path");
        let static_path = resource_archive_path(b"static.rlb").expect("static path");
        let mesh = minimal_msh_payload();
        vfs.insert(
            objects_path,
            Arc::from(
                build_nres(&[(
                    b"s_tree_04".as_slice(),
                    build_object_refs(&[(b"static.rlb".as_slice(), b"s_tree_0_04.msh".as_slice())])
                        .as_slice(),
                )])
                .into_boxed_slice(),
            ),
        );
        vfs.insert(
            static_path,
            Arc::from(
                build_nres(&[(b"s_tree_0_04.msh".as_slice(), mesh.as_slice())]).into_boxed_slice(),
            ),
        );
        let vfs = Arc::new(vfs);
        let repo = CachedResourceRepository::new(vfs.clone());
        let resolved = resolve_prototype(&repo, vfs.as_ref(), &resource_name(b"s_tree_04"))
            .expect("resolve")
            .expect("prototype");

        assert_eq!(resolved.source, PrototypeSource::ObjectsRegistry);
        let PrototypeGeometry::Mesh(mesh) = resolved.geometry else {
            panic!("expected mesh");
        };
        assert_eq!(mesh.archive.as_str(), "static.rlb");
        assert!(mesh.name.0.eq_ignore_ascii_case(b"s_tree_0_04.msh"));
    }

    #[test]
    fn graph_report_records_resolved_roots_and_failures() {
        let mut vfs = MemoryVfs::default();
        let objects_path = resource_archive_path(b"objects.rlb").expect("objects path");
        let static_path = resource_archive_path(b"static.rlb").expect("static path");
        let mesh = minimal_msh_payload();
        vfs.insert(
            objects_path,
            Arc::from(
                build_nres(&[(
                    b"s_tree_04".as_slice(),
                    build_object_refs(&[(b"static.rlb".as_slice(), b"s_tree_0_04.msh".as_slice())])
                        .as_slice(),
                )])
                .into_boxed_slice(),
            ),
        );
        vfs.insert(
            static_path,
            Arc::from(
                build_nres(&[(b"s_tree_0_04.msh".as_slice(), mesh.as_slice())]).into_boxed_slice(),
            ),
        );
        let vfs = Arc::new(vfs);
        let repo = CachedResourceRepository::new(vfs.clone());
        let roots = [resource_name(b"s_tree_04"), resource_name(b"missing_key")];
        let (graph, resolved, report) = build_prototype_graph_report(&repo, vfs.as_ref(), &roots);

        assert_eq!(graph.roots.len(), 2);
        assert_eq!(resolved.len(), 1);
        assert_eq!(report.root_count, 2);
        assert_eq!(report.direct_reference_count, 2);
        assert_eq!(report.unit_reference_count, 0);
        assert_eq!(report.resolved_count, 1);
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].root_index, 1);
        assert_eq!(
            report.failures[0].edge,
            PrototypeGraphEdge::MissionToObjectsRegistry
        );
        assert!(!report.is_success());
    }

    #[test]
    fn resolves_synthetic_unit_dat_binding() {
        let mut vfs = MemoryVfs::default();
        let dat_path = resource_archive_path(b"UNITS/AUTO/unit.dat").expect("dat path");
        let archive_path = resource_archive_path(b"units.rlb").expect("archive path");
        let mesh = minimal_msh_payload();
        vfs.insert(
            dat_path,
            Arc::from(build_unit_dat_binding(b"units.rlb", b"unit_model").into_boxed_slice()),
        );
        vfs.insert(
            archive_path,
            Arc::from(
                build_nres(&[(b"unit_model.msh".as_slice(), mesh.as_slice())]).into_boxed_slice(),
            ),
        );
        let vfs = Arc::new(vfs);
        let repo = CachedResourceRepository::new(vfs.clone());
        let resolved =
            resolve_prototype(&repo, vfs.as_ref(), &resource_name(b"UNITS/AUTO/unit.dat"))
                .expect("resolve")
                .expect("prototype");

        assert_eq!(resolved.source, PrototypeSource::UnitDat);
        let PrototypeGeometry::Mesh(mesh) = resolved.geometry else {
            panic!("expected mesh");
        };
        assert_eq!(mesh.archive.as_str(), "units.rlb");
        assert!(mesh.name.0.eq_ignore_ascii_case(b"unit_model.msh"));
    }

    #[test]
    fn unit_dat_expands_components_in_order() {
        let mut vfs = MemoryVfs::default();
        let dat_path = resource_archive_path(b"UNITS/AUTO/compound.dat").expect("dat path");
        let objects_path = resource_archive_path(b"objects.rlb").expect("objects path");
        let static_path = resource_archive_path(b"static.rlb").expect("static path");
        let mesh = minimal_msh_payload();
        vfs.insert(
            dat_path,
            Arc::from(
                build_unit_dat(&[
                    (b"objects.rlb".as_slice(), b"component_a".as_slice()),
                    (b"objects.rlb".as_slice(), b"component_b".as_slice()),
                ])
                .into_boxed_slice(),
            ),
        );
        vfs.insert(
            objects_path,
            Arc::from(
                build_nres(&[
                    (
                        b"component_a".as_slice(),
                        build_object_refs(&[(
                            b"static.rlb".as_slice(),
                            b"component_a.msh".as_slice(),
                        )])
                        .as_slice(),
                    ),
                    (
                        b"component_b".as_slice(),
                        build_object_refs(&[(
                            b"static.rlb".as_slice(),
                            b"component_b.msh".as_slice(),
                        )])
                        .as_slice(),
                    ),
                ])
                .into_boxed_slice(),
            ),
        );
        vfs.insert(
            static_path,
            Arc::from(
                build_nres(&[
                    (b"component_a.msh".as_slice(), mesh.as_slice()),
                    (b"component_b.msh".as_slice(), mesh.as_slice()),
                ])
                .into_boxed_slice(),
            ),
        );
        let vfs = Arc::new(vfs);
        let repo = CachedResourceRepository::new(vfs.clone());
        let roots = [resource_name(b"UNITS/AUTO/compound.dat")];
        let (graph, resolved, report) = build_prototype_graph_report(&repo, vfs.as_ref(), &roots);

        assert_eq!(graph.roots.len(), 1);
        assert_eq!(graph.prototype_requests.len(), 2);
        assert_eq!(graph.prototype_requests[0].0 .0, b"component_a");
        assert_eq!(graph.prototype_requests[1].0 .0, b"component_b");
        assert_eq!(resolved.len(), 2);
        assert_eq!(report.unit_reference_count, 1);
        assert_eq!(report.unit_component_count, 2);
        assert_eq!(report.resolved_count, 2);
        assert!(report.is_success());
    }

    #[test]
    fn objects_registry_inheritance_merges_parent_then_local_refs() {
        let mut vfs = MemoryVfs::default();
        let objects_path = resource_archive_path(b"objects.rlb").expect("objects path");
        let static_path = resource_archive_path(b"static.rlb").expect("static path");
        let fortif_path = resource_archive_path(b"fortif.rlb").expect("fortif path");
        let mesh = minimal_msh_payload();
        vfs.insert(
            objects_path,
            Arc::from(
                build_nres(&[
                    (
                        b"parent_proto".as_slice(),
                        build_object_refs(&[(
                            b"static.rlb".as_slice(),
                            b"parent_proto.msh".as_slice(),
                        )])
                        .as_slice(),
                    ),
                    (
                        b"child_proto".as_slice(),
                        build_object_refs(&[
                            (b"objects.rlb".as_slice(), b"parent_proto".as_slice()),
                            (b"fortif.rlb".as_slice(), b"child_proto.bas".as_slice()),
                        ])
                        .as_slice(),
                    ),
                ])
                .into_boxed_slice(),
            ),
        );
        vfs.insert(
            static_path,
            Arc::from(
                build_nres(&[(b"parent_proto.msh".as_slice(), mesh.as_slice())]).into_boxed_slice(),
            ),
        );
        vfs.insert(
            fortif_path,
            Arc::from(build_nres(&[(b"child_proto.bas".as_slice(), b"base")]).into_boxed_slice()),
        );
        let vfs = Arc::new(vfs);
        let repo = CachedResourceRepository::new(vfs.clone());
        let resolved = resolve_prototype(&repo, vfs.as_ref(), &resource_name(b"child_proto"))
            .expect("resolve")
            .expect("prototype");

        let PrototypeGeometry::Mesh(mesh) = resolved.geometry else {
            panic!("expected inherited mesh");
        };
        assert_eq!(mesh.archive.as_str(), "static.rlb");
        assert!(mesh.name.0.eq_ignore_ascii_case(b"parent_proto.msh"));
    }

    #[test]
    fn objects_registry_inheritance_resolves_multiple_levels() {
        let mut vfs = MemoryVfs::default();
        let objects_path = resource_archive_path(b"objects.rlb").expect("objects path");
        let static_path = resource_archive_path(b"static.rlb").expect("static path");
        let mesh = minimal_msh_payload();
        vfs.insert(
            objects_path,
            Arc::from(
                build_nres(&[
                    (
                        b"grandparent".as_slice(),
                        build_object_refs(&[(
                            b"static.rlb".as_slice(),
                            b"grandparent.msh".as_slice(),
                        )])
                        .as_slice(),
                    ),
                    (
                        b"parent".as_slice(),
                        build_object_refs(&[(
                            b"objects.rlb".as_slice(),
                            b"grandparent".as_slice(),
                        )])
                        .as_slice(),
                    ),
                    (
                        b"child".as_slice(),
                        build_object_refs(&[(b"objects.rlb".as_slice(), b"parent".as_slice())])
                            .as_slice(),
                    ),
                ])
                .into_boxed_slice(),
            ),
        );
        vfs.insert(
            static_path,
            Arc::from(
                build_nres(&[(b"grandparent.msh".as_slice(), mesh.as_slice())]).into_boxed_slice(),
            ),
        );
        let vfs = Arc::new(vfs);
        let repo = CachedResourceRepository::new(vfs.clone());
        let resolved = resolve_prototype(&repo, vfs.as_ref(), &resource_name(b"child"))
            .expect("resolve")
            .expect("prototype");

        let PrototypeGeometry::Mesh(mesh) = resolved.geometry else {
            panic!("expected inherited mesh");
        };
        assert!(mesh.name.0.eq_ignore_ascii_case(b"grandparent.msh"));
    }

    #[test]
    fn base_only_registry_entry_is_nongeometric() {
        let mut vfs = MemoryVfs::default();
        let objects_path = resource_archive_path(b"objects.rlb").expect("objects path");
        let fortif_path = resource_archive_path(b"fortif.rlb").expect("fortif path");
        vfs.insert(
            objects_path,
            Arc::from(
                build_nres(&[(
                    b"base_only".as_slice(),
                    build_object_refs(&[(b"fortif.rlb".as_slice(), b"base_only.bas".as_slice())])
                        .as_slice(),
                )])
                .into_boxed_slice(),
            ),
        );
        vfs.insert(
            fortif_path,
            Arc::from(build_nres(&[(b"base_only.bas".as_slice(), b"base")]).into_boxed_slice()),
        );
        let vfs = Arc::new(vfs);
        let repo = CachedResourceRepository::new(vfs.clone());
        let resolved = resolve_prototype(&repo, vfs.as_ref(), &resource_name(b"base_only"))
            .expect("resolve")
            .expect("prototype");

        assert_eq!(resolved.geometry, PrototypeGeometry::NonGeometric);
        assert!(resolved.dependencies.is_empty());
    }

    #[test]
    fn objects_registry_inheritance_rejects_direct_cycle() {
        let mut vfs = MemoryVfs::default();
        let objects_path = resource_archive_path(b"objects.rlb").expect("objects path");
        vfs.insert(
            objects_path,
            Arc::from(
                build_nres(&[(
                    b"self_cycle".as_slice(),
                    build_object_refs(&[(b"objects.rlb".as_slice(), b"self_cycle".as_slice())])
                        .as_slice(),
                )])
                .into_boxed_slice(),
            ),
        );
        let vfs = Arc::new(vfs);
        let repo = CachedResourceRepository::new(vfs.clone());
        let err = resolve_prototype(&repo, vfs.as_ref(), &resource_name(b"self_cycle"))
            .expect_err("cycle");

        assert!(err.to_string().contains("cycle"));
    }

    #[test]
    fn objects_registry_inheritance_rejects_indirect_cycle() {
        let mut vfs = MemoryVfs::default();
        let objects_path = resource_archive_path(b"objects.rlb").expect("objects path");
        vfs.insert(
            objects_path,
            Arc::from(
                build_nres(&[
                    (
                        b"cycle_a".as_slice(),
                        build_object_refs(&[(b"objects.rlb".as_slice(), b"cycle_b".as_slice())])
                            .as_slice(),
                    ),
                    (
                        b"cycle_b".as_slice(),
                        build_object_refs(&[(b"objects.rlb".as_slice(), b"cycle_a".as_slice())])
                            .as_slice(),
                    ),
                ])
                .into_boxed_slice(),
            ),
        );
        let vfs = Arc::new(vfs);
        let repo = CachedResourceRepository::new(vfs.clone());
        let err =
            resolve_prototype(&repo, vfs.as_ref(), &resource_name(b"cycle_a")).expect_err("cycle");

        assert!(err.to_string().contains("cycle"));
    }

    #[test]
    fn invalid_referenced_msh_is_error() {
        let mut vfs = MemoryVfs::default();
        let objects_path = resource_archive_path(b"objects.rlb").expect("objects path");
        let static_path = resource_archive_path(b"static.rlb").expect("static path");
        vfs.insert(
            objects_path,
            Arc::from(
                build_nres(&[(
                    b"bad_tree".as_slice(),
                    build_object_refs(&[(b"static.rlb".as_slice(), b"bad_tree.msh".as_slice())])
                        .as_slice(),
                )])
                .into_boxed_slice(),
            ),
        );
        vfs.insert(
            static_path,
            Arc::from(
                build_nres(&[(b"bad_tree.msh".as_slice(), b"not an nres".as_slice())])
                    .into_boxed_slice(),
            ),
        );
        let vfs = Arc::new(vfs);
        let repo = CachedResourceRepository::new(vfs.clone());

        let err = resolve_prototype(&repo, vfs.as_ref(), &resource_name(b"bad_tree"))
            .expect_err("invalid mesh");

        assert!(matches!(err, PrototypeError::InvalidMesh(_)));
    }

    #[test]
    fn missing_referenced_archive_reports_root_chain() {
        let mut vfs = MemoryVfs::default();
        let objects_path = resource_archive_path(b"objects.rlb").expect("objects path");
        vfs.insert(
            objects_path,
            Arc::from(
                build_nres(&[(
                    b"broken".as_slice(),
                    build_object_refs(&[(b"missing.rlb".as_slice(), b"broken.msh".as_slice())])
                        .as_slice(),
                )])
                .into_boxed_slice(),
            ),
        );
        let vfs = Arc::new(vfs);
        let repo = CachedResourceRepository::new(vfs.clone());

        let (_graph, _resolved, report) =
            build_prototype_graph_report(&repo, vfs.as_ref(), &[resource_name(b"broken")]);

        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].resource_raw, b"broken");
        assert_eq!(
            report.failures[0].edge,
            PrototypeGraphEdge::MissionToObjectsRegistry
        );
        assert!(report.failures[0].message.contains("broken"));
        assert!(report.failures[0]
            .message
            .contains("missing.rlb:broken.msh"));
    }

    #[test]
    fn missing_referenced_resource_reports_root_chain() {
        let mut vfs = MemoryVfs::default();
        let objects_path = resource_archive_path(b"objects.rlb").expect("objects path");
        let static_path = resource_archive_path(b"static.rlb").expect("static path");
        vfs.insert(
            objects_path,
            Arc::from(
                build_nres(&[(
                    b"broken".as_slice(),
                    build_object_refs(&[(b"static.rlb".as_slice(), b"missing.msh".as_slice())])
                        .as_slice(),
                )])
                .into_boxed_slice(),
            ),
        );
        vfs.insert(static_path, Arc::from(build_nres(&[]).into_boxed_slice()));
        let vfs = Arc::new(vfs);
        let repo = CachedResourceRepository::new(vfs.clone());

        let (_graph, _resolved, report) =
            build_prototype_graph_report(&repo, vfs.as_ref(), &[resource_name(b"broken")]);

        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].resource_raw, b"broken");
        assert!(report.failures[0]
            .message
            .contains("static.rlb:missing.msh"));
    }

    #[test]
    fn first_existing_explicit_msh_is_selected_in_order() {
        let mut vfs = MemoryVfs::default();
        let objects_path = resource_archive_path(b"objects.rlb").expect("objects path");
        let static_path = resource_archive_path(b"static.rlb").expect("static path");
        let mesh = minimal_msh_payload();
        vfs.insert(
            objects_path,
            Arc::from(
                build_nres(&[(
                    b"ordered".as_slice(),
                    build_object_refs(&[
                        (b"static.rlb".as_slice(), b"missing.msh".as_slice()),
                        (b"static.rlb".as_slice(), b"ordered.msh".as_slice()),
                    ])
                    .as_slice(),
                )])
                .into_boxed_slice(),
            ),
        );
        vfs.insert(
            static_path,
            Arc::from(
                build_nres(&[(b"ordered.msh".as_slice(), mesh.as_slice())]).into_boxed_slice(),
            ),
        );
        let vfs = Arc::new(vfs);
        let repo = CachedResourceRepository::new(vfs.clone());

        let resolved = resolve_prototype(&repo, vfs.as_ref(), &resource_name(b"ordered"))
            .expect("ordered resolve")
            .expect("prototype");

        let PrototypeGeometry::Mesh(mesh) = resolved.geometry else {
            panic!("expected mesh");
        };
        assert!(mesh.name.0.eq_ignore_ascii_case(b"ordered.msh"));
    }

    #[test]
    fn objects_registry_inheritance_rejects_depth_limit() {
        let mut names = Vec::new();
        let mut payloads = Vec::new();
        for index in 0..34usize {
            names.push(format!("proto_{index}").into_bytes());
            payloads.push(build_object_refs(&[(
                b"objects.rlb".as_slice(),
                format!("proto_{}", index + 1).as_bytes(),
            )]));
        }
        let entries = names
            .iter()
            .zip(payloads.iter())
            .map(|(name, payload)| (name.as_slice(), payload.as_slice()))
            .collect::<Vec<_>>();
        let mut vfs = MemoryVfs::default();
        let objects_path = resource_archive_path(b"objects.rlb").expect("objects path");
        vfs.insert(
            objects_path,
            Arc::from(build_nres(&entries).into_boxed_slice()),
        );
        let vfs = Arc::new(vfs);
        let repo = CachedResourceRepository::new(vfs.clone());

        let err =
            resolve_prototype(&repo, vfs.as_ref(), &resource_name(b"proto_0")).expect_err("depth");

        assert!(err.to_string().contains("depth exceeded"));
    }

    #[test]
    fn generated_acyclic_prototype_graph_resolves_deterministically() {
        let first = generated_acyclic_graph(&[0, 1, 2, 3, 4, 5]);
        let second = generated_acyclic_graph(&[5, 4, 3, 2, 1, 0]);

        assert_eq!(first.0, second.0);
        assert_eq!(first.1, second.1);
        assert_eq!(first.2, second.2);
    }

    #[test]
    fn arbitrary_unit_and_registry_bytes_are_bounded_and_panic_free() {
        for len in 0..256usize {
            let bytes = vec![0xA5; len];
            let unit = std::panic::catch_unwind(|| decode_unit_dat(&bytes));
            let registry = std::panic::catch_unwind(|| decode_registry_entry(&bytes));

            assert!(unit.is_ok());
            assert!(registry.is_ok());
        }
    }

    #[test]
    fn resolver_cache_invalidates_when_archive_fingerprint_changes() {
        let root = temp_dir("resolver-cache");
        let objects_path = resource_archive_path(b"objects.rlb").expect("objects path");
        let static_path = resource_archive_path(b"static.rlb").expect("static path");
        std::fs::write(
            root.join(objects_path.as_str()),
            build_nres(&[(
                b"dynamic".as_slice(),
                build_object_refs(&[(b"static.rlb".as_slice(), b"dynamic.msh".as_slice())])
                    .as_slice(),
            )]),
        )
        .expect("objects.rlb");
        std::fs::write(
            root.join(static_path.as_str()),
            build_nres(&[(b"dynamic.msh".as_slice(), b"not an nres".as_slice())]),
        )
        .expect("initial static.rlb");
        let vfs = Arc::new(DirectoryVfs::new(&root));
        let repo = CachedResourceRepository::new(vfs.clone());

        let err = resolve_prototype(&repo, vfs.as_ref(), &resource_name(b"dynamic"))
            .expect_err("invalid initial mesh");
        assert!(matches!(err, PrototypeError::InvalidMesh(_)));

        std::fs::write(
            root.join(static_path.as_str()),
            build_nres(&[(b"dynamic.msh".as_slice(), minimal_msh_payload().as_slice())]),
        )
        .expect("updated static.rlb");
        let resolved = resolve_prototype(&repo, vfs.as_ref(), &resource_name(b"dynamic"))
            .expect("updated resolve")
            .expect("prototype");

        let PrototypeGeometry::Mesh(mesh) = resolved.geometry else {
            panic!("expected mesh");
        };
        assert!(mesh.name.0.eq_ignore_ascii_case(b"dynamic.msh"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn resolves_known_part1_registry_cases() {
        let root = corpus_root("IS").expect("part 1 root");
        let vfs = Arc::new(DirectoryVfs::new(&root));
        let repo = CachedResourceRepository::new(vfs.clone());
        let cases = [
            (b"r_h_01".as_slice(), "bases.rlb", b"r_h_01.msh".as_slice()),
            (
                b"s_tree_04".as_slice(),
                "static.rlb",
                b"s_tree_0_04.msh".as_slice(),
            ),
            (
                b"fr_m_brige".as_slice(),
                "fortif.rlb",
                b"fr_m_brige.msh".as_slice(),
            ),
        ];

        for (key, archive, model) in cases {
            let resolved = resolve_prototype(&repo, vfs.as_ref(), &resource_name(key))
                .unwrap_or_else(|err| panic!("failed to resolve {:?}: {err}", key))
                .unwrap_or_else(|| panic!("missing prototype for {:?}", key));
            let PrototypeGeometry::Mesh(mesh) = resolved.geometry else {
                panic!("expected mesh");
            };
            assert_eq!(mesh.archive.as_str().to_ascii_lowercase(), archive);
            assert!(mesh.name.0.eq_ignore_ascii_case(model));
        }
    }

    #[test]
    fn resolves_some_registry_entries_in_both_corpora() {
        for corpus in ["IS", "IS2"] {
            let root = corpus_root(corpus).expect("corpus root");
            let objects = std::fs::read(root.join("objects.rlb")).expect("objects.rlb");
            let document = fparkan_nres::decode(
                Arc::from(objects.into_boxed_slice()),
                fparkan_nres::ReadProfile::Compatible,
            )
            .expect("objects.rlb document");
            let vfs = Arc::new(DirectoryVfs::new(&root));
            let repo = CachedResourceRepository::new(vfs.clone());
            let mut resolved = 0usize;

            for entry in document.entries().iter().take(64) {
                if resolve_prototype(&repo, vfs.as_ref(), &resource_name(entry.name_bytes()))
                    .unwrap_or_else(|err| panic!("{corpus} {:?}: {err}", entry.name_bytes()))
                    .is_some()
                {
                    resolved += 1;
                }
            }

            assert!(resolved > 0, "{corpus}: no registry entries resolved");
        }
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_corpora_unit_dat_parse_counts() {
        let cases = [("IS", 425, 5_219), ("IS2", 676, 8_145)];
        for (corpus, expected_files, expected_records) in cases {
            let root = corpus_root(corpus).expect("corpus root");
            let mut dat_paths = Vec::new();
            collect_unit_dat_files(&root, &mut dat_paths);
            dat_paths.sort();
            let mut records = 0usize;
            for path in &dat_paths {
                let bytes = std::fs::read(path).expect("unit DAT");
                let unit = decode_unit_dat(&bytes).expect("unit DAT decode");
                for record in &unit.records {
                    assert!(
                        archive_name_is(&record.archive_raw, b"objects.rlb"),
                        "{}: unexpected component archive {:?}",
                        path.display(),
                        cstr_bytes(&record.archive_raw)
                    );
                    assert_eq!(
                        record.kind,
                        1,
                        "{}: unexpected component kind",
                        path.display()
                    );
                }
                records += unit.records.len();
            }
            assert_eq!(dat_paths.len(), expected_files, "{corpus} unit DAT files");
            assert_eq!(records, expected_records, "{corpus} unit DAT records");
        }
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_corpora_registry_payloads_are_record_aligned() {
        for corpus in ["IS", "IS2"] {
            let root = corpus_root(corpus).expect("corpus root");
            let objects = std::fs::read(root.join("objects.rlb")).expect("objects.rlb");
            let document = fparkan_nres::decode(
                Arc::from(objects.into_boxed_slice()),
                fparkan_nres::ReadProfile::Compatible,
            )
            .expect("objects.rlb document");

            assert!(document.entry_count() > 0, "{corpus}: empty objects.rlb");
            for entry in document.entries() {
                let payload = document.payload(entry.id()).expect("registry payload");
                assert!(
                    payload.len().is_multiple_of(64),
                    "{corpus}: registry payload for {:?} is not 64-byte aligned",
                    entry.name_bytes()
                );
                decode_registry_entry(payload).expect("registry payload decode");
            }
        }
    }

    fn collect_unit_dat_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        let mut children: Vec<_> = std::fs::read_dir(dir)
            .expect("read dir")
            .map(|entry| entry.expect("entry").path())
            .collect();
        children.sort();
        for child in children {
            if child.is_dir() {
                collect_unit_dat_files(&child, out);
            } else if child
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("dat"))
                && child.components().any(|component| {
                    component
                        .as_os_str()
                        .to_str()
                        .is_some_and(|text| text.eq_ignore_ascii_case("UNITS"))
                })
            {
                out.push(child);
            }
        }
    }

    fn corpus_root(name: &str) -> Option<std::path::PathBuf> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("testdata")
            .join(name);
        root.is_dir().then_some(root)
    }

    fn generated_acyclic_graph(
        order: &[usize],
    ) -> (
        PrototypeGraph,
        Vec<EffectivePrototype>,
        PrototypeGraphReport,
    ) {
        let names = (0..6usize)
            .map(|index| format!("node_{index}").into_bytes())
            .collect::<Vec<_>>();
        let payloads = (0..6usize)
            .map(|index| {
                if index == 0 {
                    build_object_refs(&[(b"static.rlb".as_slice(), b"node_0.msh".as_slice())])
                } else {
                    build_object_refs(&[(
                        b"objects.rlb".as_slice(),
                        format!("node_{}", index - 1).as_bytes(),
                    )])
                }
            })
            .collect::<Vec<_>>();
        let entries = order
            .iter()
            .map(|index| (names[*index].as_slice(), payloads[*index].as_slice()))
            .collect::<Vec<_>>();
        let mut vfs = MemoryVfs::default();
        let objects_path = resource_archive_path(b"objects.rlb").expect("objects path");
        let static_path = resource_archive_path(b"static.rlb").expect("static path");
        vfs.insert(
            objects_path,
            Arc::from(build_nres(&entries).into_boxed_slice()),
        );
        vfs.insert(
            static_path,
            Arc::from(
                build_nres(&[(b"node_0.msh".as_slice(), minimal_msh_payload().as_slice())])
                    .into_boxed_slice(),
            ),
        );
        let vfs = Arc::new(vfs);
        let repo = CachedResourceRepository::new(vfs.clone());
        build_prototype_graph_report(
            &repo,
            vfs.as_ref(),
            &[resource_name(b"node_5"), resource_name(b"node_3")],
        )
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "fparkan-prototype-{name}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("temp dir");
        path
    }

    fn build_unit_dat_binding(archive: &[u8], model: &[u8]) -> Vec<u8> {
        let mut out = vec![0; UNIT_DAT_MIN_SIZE];
        out[0..4].copy_from_slice(&UNIT_DAT_MAGIC.to_le_bytes());
        copy_cstr(&mut out[0x08..0x28], archive);
        copy_cstr(&mut out[0x28..0x48], model);
        out
    }

    fn build_unit_dat(components: &[(&[u8], &[u8])]) -> Vec<u8> {
        let mut out = vec![0; 8];
        out[0..4].copy_from_slice(&UNIT_DAT_MAGIC.to_le_bytes());
        for (index, (archive, resource)) in components.iter().enumerate() {
            let mut record = [0; 112];
            copy_cstr(&mut record[0..32], archive);
            copy_cstr(&mut record[32..64], resource);
            record[64..68].copy_from_slice(&1_u32.to_le_bytes());
            record[68..72].copy_from_slice(
                &i32::try_from(index)
                    .map_or(-1, |value| value.saturating_sub(1))
                    .to_le_bytes(),
            );
            copy_cstr(&mut record[72..104], b"component");
            out.extend_from_slice(&record);
        }
        out
    }

    fn build_object_refs(items: &[(&[u8], &[u8])]) -> Vec<u8> {
        let mut out = Vec::with_capacity(items.len() * 64);
        for (archive, resource) in items {
            let mut chunk = [0; 64];
            copy_cstr(&mut chunk[..32], archive);
            copy_cstr(&mut chunk[32..], resource);
            out.extend_from_slice(&chunk);
        }
        out
    }

    fn build_nres(entries: &[(&[u8], &[u8])]) -> Vec<u8> {
        let entries = entries
            .iter()
            .map(|(name, payload)| TestEntry {
                type_id: 0,
                attr3: 0,
                name,
                payload,
            })
            .collect::<Vec<_>>();
        build_nres_typed(&entries)
    }

    fn minimal_msh_payload() -> Vec<u8> {
        build_nres_typed(&[
            TestEntry {
                type_id: 1,
                attr3: 38,
                name: b"Res1",
                payload: &[],
            },
            TestEntry {
                type_id: 2,
                attr3: 0,
                name: b"Res2",
                payload: &[0; 0x8c],
            },
            TestEntry {
                type_id: 3,
                attr3: 0,
                name: b"Res3",
                payload: &[],
            },
            TestEntry {
                type_id: 6,
                attr3: 0,
                name: b"Res6",
                payload: &[],
            },
            TestEntry {
                type_id: 13,
                attr3: 0,
                name: b"Res13",
                payload: &[],
            },
        ])
    }

    struct TestEntry<'a> {
        type_id: u32,
        attr3: u32,
        name: &'a [u8],
        payload: &'a [u8],
    }

    fn build_nres_typed(entries: &[TestEntry<'_>]) -> Vec<u8> {
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
            push_u32(&mut out, 0);
            push_u32(
                &mut out,
                u32::try_from(entry.payload.len()).expect("payload"),
            );
            push_u32(&mut out, entry.attr3);
            let mut name_raw = [0; 36];
            copy_cstr(&mut name_raw, entry.name);
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

    fn copy_cstr(dst: &mut [u8], src: &[u8]) {
        let len = dst.len().saturating_sub(1).min(src.len());
        dst[..len].copy_from_slice(&src[..len]);
    }

    fn push_u32(out: &mut Vec<u8>, value: u32) {
        out.extend_from_slice(&value.to_le_bytes());
    }
}
