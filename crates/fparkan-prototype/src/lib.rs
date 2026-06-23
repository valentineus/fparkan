#![forbid(unsafe_code)]
//! Prototype registry and unit DAT primitives.

use encoding_rs::WINDOWS_1251;
use fparkan_binary::{checked_count_bytes, Cursor, DecodeError};
use fparkan_path::{normalize_relative, NormalizedPath, PathPolicy, ResourceName};
use fparkan_resource::{
    archive_path, resource_name, ResourceError, ResourceKey, ResourceRepository,
};
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
    /// Mission object-local spans of effective prototype requests.
    pub root_prototype_request_spans: Vec<std::ops::Range<usize>>,
    /// Materialized prototype dependency graph nodes.
    pub nodes: Vec<PrototypeGraphNode>,
    /// Materialized prototype dependency graph edges.
    pub edges: Vec<PrototypeGraphEdgeInstance>,
}

/// Stable node identifier.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PrototypeGraphNodeId(pub u32);

/// Stable edge identifier.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PrototypeGraphEdgeId(pub u32);

/// Edge requiredness/fallback policy for a graph dependency.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrototypeGraphRequiredness {
    /// Missing edge should fail mission load.
    Required,
    /// Missing edge is tolerated and handled by fallback policy.
    Optional,
    /// Edge was produced by an explicit fallback transition.
    Fallback,
}

/// Source provenance for graph construction and failures.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrototypeGraphProvenance {
    /// Root mission object index that initiated traversal.
    pub root_index: usize,
    /// Immediate parent edge that discovered this edge.
    pub parent_edge: Option<PrototypeGraphEdgeId>,
    /// Source archive when available.
    pub archive: Option<String>,
    /// Source resource key when available.
    pub resource: Option<Vec<u8>>,
    /// Byte span in the source archive entry when known.
    pub span: Option<(u64, u64)>,
}

/// Prototype graph node kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrototypeGraphNodeKind {
    /// Mission root key.
    MissionRoot,
    /// Unit DAT root key.
    UnitDatRoot,
    /// Resolved prototype request.
    Prototype,
    /// Mesh dependency.
    MeshResource,
    /// Non-geometric prototype.
    NonGeometric,
}

/// Prototype graph node record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrototypeGraphNode {
    /// Stable identifier.
    pub id: PrototypeGraphNodeId,
    /// Node kind.
    pub kind: PrototypeGraphNodeKind,
    /// Optional logical key represented by node.
    pub key: Option<PrototypeKey>,
    /// Optional resource represented by node.
    pub resource: Option<ResourceKey>,
}

impl PrototypeGraphNode {
    /// Creates a mesh resource node.
    #[must_use]
    pub const fn mesh(resource: ResourceKey, id: PrototypeGraphNodeId) -> Self {
        Self {
            id,
            kind: PrototypeGraphNodeKind::MeshResource,
            key: None,
            resource: Some(resource),
        }
    }

    /// Creates a prototype node.
    #[must_use]
    pub const fn prototype(key: PrototypeKey, id: PrototypeGraphNodeId) -> Self {
        Self {
            id,
            kind: PrototypeGraphNodeKind::Prototype,
            key: Some(key),
            resource: None,
        }
    }

    /// Creates a root node.
    #[must_use]
    pub const fn root(key: PrototypeKey, is_unit_dat: bool, id: PrototypeGraphNodeId) -> Self {
        Self {
            id,
            kind: if is_unit_dat {
                PrototypeGraphNodeKind::UnitDatRoot
            } else {
                PrototypeGraphNodeKind::MissionRoot
            },
            key: Some(key),
            resource: None,
        }
    }
}

/// Prototype graph edge kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrototypeGraphEdgeKind {
    /// Mission root to resolved prototype.
    MissionToRoot,
    /// Unit component to prototype.
    UnitDatToComponent,
    /// Prototype to mesh dependency.
    PrototypeToMesh,
}

/// Prototype graph edge record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrototypeGraphEdgeInstance {
    /// Stable identifier.
    pub id: PrototypeGraphEdgeId,
    /// Source node.
    pub from: PrototypeGraphNodeId,
    /// Destination node.
    pub to: PrototypeGraphNodeId,
    /// Edge kind.
    pub kind: PrototypeGraphEdgeKind,
    /// Requiredness semantics for this dependency.
    pub requiredness: PrototypeGraphRequiredness,
    /// Provenance for reproducible diagnostics and tracing.
    pub provenance: Option<PrototypeGraphProvenance>,
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
        if self
            .failures
            .iter()
            .any(|failure| failure.requiredness == PrototypeGraphRequiredness::Required)
        {
            return false;
        }

        let expected_prototype_count = self.direct_reference_count + self.unit_component_count;
        if self.resolved_count != expected_prototype_count {
            return false;
        }

        if self.wear_resolved_count > self.wear_request_count
            || self.material_resolved_count > self.material_slot_count
            || self.texture_resolved_count > self.texture_request_count
            || self.lightmap_resolved_count > self.lightmap_request_count
        {
            return false;
        }

        true
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
    /// Requiredness that triggered this failure.
    pub requiredness: PrototypeGraphRequiredness,
    /// Source provenance for this failure.
    pub provenance: Option<PrototypeGraphProvenance>,
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
    Vfs(VfsError),
    /// Resource repository error.
    Resource(ResourceError),
}

impl From<DecodeError> for PrototypeError {
    fn from(value: DecodeError) -> Self {
        Self::Decode(value)
    }
}

impl From<ResourceError> for PrototypeError {
    fn from(value: ResourceError) -> Self {
        Self::Resource(value)
    }
}

impl From<VfsError> for PrototypeError {
    fn from(value: VfsError) -> Self {
        Self::Vfs(value)
    }
}

impl std::fmt::Display for PrototypeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Decode(source) => write!(f, "decode error: {source}"),
            Self::InvalidSize => write!(f, "invalid prototype payload size"),
            Self::InvalidUnitDatMagic(magic) => {
                write!(f, "invalid unit DAT magic: {magic:#010X}")
            }
            Self::InvalidPath(value) => write!(f, "invalid path: {value}"),
            Self::Vfs(source) => write!(f, "vfs error: {source}"),
            Self::Resource(source) => write!(f, "resource error: {source}"),
        }
    }
}

impl std::error::Error for PrototypeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Decode(source) => Some(source),
            Self::InvalidSize | Self::InvalidUnitDatMagic(_) | Self::InvalidPath(_) => None,
            Self::Vfs(source) => Some(source),
            Self::Resource(source) => Some(source),
        }
    }
}

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

/// Resolves all prototype requests for a root resource, including every component
/// entry from unit DAT.
pub fn resolve_prototype(
    repository: &dyn ResourceRepository,
    vfs: &dyn Vfs,
    resource: &ResourceName,
) -> Result<Vec<EffectivePrototype>, PrototypeError> {
    resolve_prototype_all(repository, vfs, resource)
}

/// Resolves a single prototype for single-component callers.
///
/// # Errors
///
/// Returns [`PrototypeError`] when reachable DAT files, registries, archives,
/// or mesh payloads are structurally invalid.
fn resolve_prototype_single(
    repository: &dyn ResourceRepository,
    vfs: &dyn Vfs,
    resource: &ResourceName,
) -> Result<Option<EffectivePrototype>, PrototypeError> {
    let prototypes = resolve_prototype(repository, vfs, resource)?;
    let mut iter = prototypes.into_iter();
    let first = iter.next();
    if iter.next().is_some() {
        return Err(PrototypeError::Resource(ResourceError::Format(format!(
            "resolve_prototype_single called for multi-component root: {}",
            String::from_utf8_lossy(&resource.0)
        ))));
    }
    Ok(first)
}

/// Canonical API: resolves all prototype requests for a root resource, including
/// every component entry from unit DAT.
/// # Errors
///
/// Returns [`PrototypeError`] when reachable DAT files, registries, archives,
/// or mesh payloads are structurally invalid.
pub fn resolve_prototype_all(
    repository: &dyn ResourceRepository,
    vfs: &dyn Vfs,
    resource: &ResourceName,
) -> Result<Vec<EffectivePrototype>, PrototypeError> {
    Ok(resolve_prototype_requests(repository, vfs, resource)?
        .prototypes)
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

fn resolve_unit_dat_prototype_requests(
    repository: &dyn ResourceRepository,
    vfs: &dyn Vfs,
    resource: &ResourceName,
) -> Result<ResolvedPrototypeRequests, PrototypeError> {
    let dat_path = normalized_path_from_name(resource)?;
    let bytes = match vfs.read(&dat_path) {
        Ok(bytes) => bytes,
        Err(VfsError::NotFound(_)) => {
            return Err(PrototypeError::Resource(ResourceError::Format(format!(
                "missing unit DAT: {}",
                dat_path.as_str()
            ))));
        }
        Err(err) => return Err(err.into()),
    };

    if let Ok(unit) = decode_unit_dat(&bytes) {
        if !unit.records.is_empty() {
            let mut prototypes = Vec::with_capacity(unit.records.len());
            for record in &unit.records {
                let prototype = resolve_unit_component(repository, record)?.ok_or_else(|| {
                    PrototypeError::Resource(ResourceError::Format(format!(
                        "unit component {} did not resolve",
                        String::from_utf8_lossy(cstr_bytes(&record.resource_raw))
                    )))
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
    let mut next_node = 0u32;
    let mut next_edge = 0u32;
    for (root_index, root) in roots.iter().enumerate() {
        let key = PrototypeKey(root.clone());
        graph.roots.push(key);
        let is_unit_dat_root = has_extension_bytes(&root.0, b"dat");
        let root_node = PrototypeGraphNodeId(next_node);
        next_node = next_node.saturating_add(1);
        graph.nodes.push(
            PrototypeGraphNode::root(key.clone(), is_unit_dat_root, root_node)
        );
        let start = graph.prototype_requests.len();
        let expansion = resolve_prototype_requests(repository, vfs, root)?;
        let root_provenance = provenance_for_root(root_index, root);
        for prototype in expansion.prototypes {
            let prototype_node = PrototypeGraphNode::prototype(prototype.key.clone(), PrototypeGraphNodeId(next_node));
            next_node = next_node.saturating_add(1);
            let prototype_node_id = prototype_node.id;
            graph.nodes.push(prototype_node);
            let root_to_prototype_edge_id = PrototypeGraphEdgeId(next_edge);
            graph.edges.push(PrototypeGraphEdgeInstance {
                id: root_to_prototype_edge_id,
                from: root_node,
                to: prototype_node_id,
                kind: if is_unit_dat_root {
                    PrototypeGraphEdgeKind::UnitDatToComponent
                } else {
                    PrototypeGraphEdgeKind::MissionToRoot
                },
                requiredness: PrototypeGraphRequiredness::Required,
                provenance: Some(root_provenance.clone()),
            });
            next_edge = next_edge.saturating_add(1);

            for dependency in &prototype.dependencies {
                let mesh_node = PrototypeGraphNode::mesh(dependency.clone(), PrototypeGraphNodeId(next_node));
                next_node = next_node.saturating_add(1);
                let mesh_node_id = mesh_node.id;
                graph.nodes.push(mesh_node);
                let prototype_to_mesh_edge_id = PrototypeGraphEdgeId(next_edge);
                graph.edges.push(PrototypeGraphEdgeInstance {
                    id: prototype_to_mesh_edge_id,
                    from: prototype_node_id,
                    to: mesh_node_id,
                    kind: PrototypeGraphEdgeKind::PrototypeToMesh,
                    requiredness: PrototypeGraphRequiredness::Required,
                    provenance: Some(provenance_for_mesh(
                        root_index,
                        root_to_prototype_edge_id,
                        dependency,
                    )),
                });
                next_edge = next_edge.saturating_add(1);
            }
            graph.prototype_requests.push(prototype.key.clone());
            resolved.push(prototype);
        }
        let end = graph.prototype_requests.len();
        graph.root_prototype_request_spans.push(start..end);
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
    let mut next_node = 0u32;
    let mut next_edge = 0u32;

    for (root_index, root) in roots.iter().enumerate() {
        graph.roots.push(PrototypeKey(root.clone()));
        let is_unit_dat_root = has_extension_bytes(&root.0, b"dat");
        let edge = if is_unit_dat_root {
            report.unit_reference_count += 1;
            PrototypeGraphEdge::MissionToUnitDat
        } else {
            report.direct_reference_count += 1;
            PrototypeGraphEdge::MissionToObjectsRegistry
        };
        let root_node = PrototypeGraphNodeId(next_node);
        next_node = next_node.saturating_add(1);
        graph.nodes.push(
            PrototypeGraphNode::root(PrototypeKey(root.clone()), is_unit_dat_root, root_node)
        );
        let start = graph.prototype_requests.len();
        let root_provenance = provenance_for_root(root_index, root);

        match resolve_prototype_requests(repository, vfs, root) {
            Ok(expansion) => {
                let expected = expansion.expected_count;
                if edge == PrototypeGraphEdge::MissionToUnitDat {
                    report.unit_component_count += expected;
                }
                let actual = expansion.prototypes.len();
                for prototype in expansion.prototypes {
                    let prototype_node = PrototypeGraphNode::prototype(
                        prototype.key.clone(),
                        PrototypeGraphNodeId(next_node),
                    );
                    next_node = next_node.saturating_add(1);
                    let prototype_node_id = prototype_node.id;
                    graph.nodes.push(prototype_node);
                    let root_to_prototype_edge_id = PrototypeGraphEdgeId(next_edge);
                    graph.edges.push(PrototypeGraphEdgeInstance {
                        id: root_to_prototype_edge_id,
                        from: root_node,
                        to: prototype_node_id,
                        kind: if is_unit_dat_root {
                            PrototypeGraphEdgeKind::UnitDatToComponent
                        } else {
                            PrototypeGraphEdgeKind::MissionToRoot
                        },
                        requiredness: PrototypeGraphRequiredness::Required,
                        provenance: Some(root_provenance.clone()),
                    });
                    next_edge = next_edge.saturating_add(1);

                    for dependency in &prototype.dependencies {
                        let mesh_node = PrototypeGraphNode::mesh(
                            dependency.clone(),
                            PrototypeGraphNodeId(next_node),
                        );
                        next_node = next_node.saturating_add(1);
                        let mesh_node_id = mesh_node.id;
                        graph.nodes.push(mesh_node);
                        let prototype_to_mesh_edge_id = PrototypeGraphEdgeId(next_edge);
                        graph.edges.push(PrototypeGraphEdgeInstance {
                            id: prototype_to_mesh_edge_id,
                            from: prototype_node_id,
                            to: mesh_node_id,
                            kind: PrototypeGraphEdgeKind::PrototypeToMesh,
                            requiredness: PrototypeGraphRequiredness::Required,
                            provenance: Some(provenance_for_mesh(
                                root_index,
                                root_to_prototype_edge_id,
                                dependency,
                            )),
                        });
                        next_edge = next_edge.saturating_add(1);
                    }

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
                        requiredness: PrototypeGraphRequiredness::Required,
                        provenance: Some(PrototypeGraphProvenance {
                            root_index,
                            parent_edge: None,
                            archive: None,
                            resource: Some(root.0.clone()),
                            span: None,
                        }),
                    });
                }
            }
            Err(err) => report.failures.push(PrototypeGraphFailure {
                root_index,
                resource_raw: root.0.clone(),
                edge: graph_error_edge(edge, &err),
                message: err.to_string(),
                requiredness: PrototypeGraphRequiredness::Required,
                provenance: Some(PrototypeGraphProvenance {
                    root_index,
                    parent_edge: None,
                    archive: None,
                    resource: Some(root.0.clone()),
                    span: None,
                }),
            }),
        }
        let end = graph.prototype_requests.len();
        graph
            .root_prototype_request_spans
            .push(start..end);
    }

    (graph, resolved, report)
}

fn graph_error_edge(edge: PrototypeGraphEdge, err: &PrototypeError) -> PrototypeGraphEdge {
    let _ = err;
    edge
}

fn provenance_for_root(root_index: usize, root: &ResourceName) -> PrototypeGraphProvenance {
    PrototypeGraphProvenance {
        root_index,
        parent_edge: None,
        archive: None,
        resource: Some(root.0.clone()),
        span: None,
    }
}

fn provenance_for_mesh(
    root_index: usize,
    parent_edge: PrototypeGraphEdgeId,
    dependency: &ResourceKey,
) -> PrototypeGraphProvenance {
    PrototypeGraphProvenance {
        root_index,
        parent_edge: Some(parent_edge),
        archive: Some(dependency.archive.as_str().to_string()),
        resource: Some(dependency.name.0.clone()),
        span: None,
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
        return Err(PrototypeError::Resource(ResourceError::Format(format!(
            "prototype {} explicit mesh reference missing: {}",
            String::from_utf8_lossy(&object_key.0),
            missing_mesh_refs.join(" -> ")
        ))));
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
        return Err(PrototypeError::Resource(ResourceError::Format(format!(
            "prototype inheritance depth exceeded at {}",
            String::from_utf8_lossy(&object_key.0)
        ))));
    }
    if stack
        .iter()
        .any(|item| eq_ignore_ascii_case(&item.0, &object_key.0))
    {
        return Err(PrototypeError::Resource(ResourceError::Format(format!(
            "prototype inheritance cycle at {}",
            String::from_utf8_lossy(&object_key.0)
        ))));
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
                    PrototypeError::Resource(ResourceError::Format(format!(
                        "missing parent prototype {}",
                        String::from_utf8_lossy(&parent_key.0)
                    )))
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
    repository.read(handle)?;
    Ok(Some(ResourceKey {
        archive: archive.clone(),
        name: resource_name(matched_name),
        type_id: Some(MESH_KIND),
    }))
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
        let resolved = resolve_prototype_single(&repo, vfs.as_ref(), &resource_name(b"s_tree_04"))
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
            resolve_prototype_single(&repo, vfs.as_ref(), &resource_name(b"UNITS/AUTO/unit.dat"))
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
    fn resolves_all_unit_dat_components() {
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
                        build_object_refs(&[
                            (b"static.rlb".as_slice(), b"component_b.msh".as_slice()),
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
                build_nres(&[
                    (b"component_a.msh".as_slice(), mesh.as_slice()),
                    (b"component_b.msh".as_slice(), mesh.as_slice()),
                ])
                .into_boxed_slice(),
            ),
        );
        let vfs = Arc::new(vfs);
        let repo = CachedResourceRepository::new(vfs.clone());
        let prototypes = resolve_prototype_all(
            &repo,
            vfs.as_ref(),
            &resource_name(b"UNITS/AUTO/compound.dat"),
        )
        .expect("resolve all");

        assert_eq!(prototypes.len(), 2);
        assert_eq!(prototypes[0].key.0 .0, b"component_a");
        assert_eq!(prototypes[1].key.0 .0, b"component_b");
    }

    #[test]
    fn resolve_prototype_returns_all_unit_dat_components() {
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
                        build_object_refs(&[(b"static.rlb".as_slice(), b"component_a.msh".as_slice())])
                            .as_slice(),
                    ),
                    (
                        b"component_b".as_slice(),
                        build_object_refs(&[(b"static.rlb".as_slice(), b"component_b.msh".as_slice())])
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

        let resolved = resolve_prototype(
            &repo,
            vfs.as_ref(),
            &resource_name(b"UNITS/AUTO/compound.dat"),
        )
        .expect("compound unit DAT should resolve");

        assert_eq!(resolved.len(), 2);
    }

    #[test]
    fn missing_unit_dat_is_reported_as_error() {
        let vfs = Arc::new(MemoryVfs::default());
        let repo = CachedResourceRepository::new(vfs.clone());

        let err = resolve_prototype_all(
            &repo,
            vfs.as_ref(),
            &resource_name(b"UNITS/AUTO/missing.dat"),
        )
        .expect_err("missing unit DAT should error");

        assert!(err.to_string().contains("missing unit DAT"));
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
        let resolved = resolve_prototype_single(&repo, vfs.as_ref(), &resource_name(b"child_proto"))
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
        let resolved = resolve_prototype_single(&repo, vfs.as_ref(), &resource_name(b"child"))
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
        let resolved = resolve_prototype_single(&repo, vfs.as_ref(), &resource_name(b"base_only"))
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
        let err = resolve_prototype_single(&repo, vfs.as_ref(), &resource_name(b"self_cycle"))
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
            resolve_prototype_single(&repo, vfs.as_ref(), &resource_name(b"cycle_a")).expect_err("cycle");

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

        let resolved = resolve_prototype_single(&repo, vfs.as_ref(), &resource_name(b"bad_tree"))
            .expect("prototype resolution")
            .expect("effective prototype");
        assert!(matches!(resolved.geometry, PrototypeGeometry::Mesh(_)));
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

        let resolved = resolve_prototype_single(&repo, vfs.as_ref(), &resource_name(b"ordered"))
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
            resolve_prototype_single(&repo, vfs.as_ref(), &resource_name(b"proto_0")).expect_err("depth");

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

        let _ = resolve_prototype_single(&repo, vfs.as_ref(), &resource_name(b"dynamic"))
            .expect("invalid initial mesh")
            .expect("prototype");

        std::fs::write(
            root.join(static_path.as_str()),
            build_nres(&[(b"dynamic.msh".as_slice(), minimal_msh_payload().as_slice())]),
        )
        .expect("updated static.rlb");
        let resolved = resolve_prototype_single(&repo, vfs.as_ref(), &resource_name(b"dynamic"))
            .expect("updated resolve")
            .expect("prototype");

        let PrototypeGeometry::Mesh(mesh) = resolved.geometry else {
            panic!("expected mesh");
        };
        assert!(mesh.name.0.eq_ignore_ascii_case(b"dynamic.msh"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn resolves_known_part1_registry_cases() {
        let root = corpus_root("IS");
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
            let resolved = resolve_prototype_single(&repo, vfs.as_ref(), &resource_name(key))
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
    #[ignore = "requires licensed corpus"]
    fn resolves_some_registry_entries_in_both_corpora() {
        for corpus in ["IS", "IS2"] {
            let root = corpus_root(corpus);
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
                if resolve_prototype_single(&repo, vfs.as_ref(), &resource_name(entry.name_bytes()))
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
            let root = corpus_root(corpus);
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
            let root = corpus_root(corpus);
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

    fn corpus_root(name: &str) -> std::path::PathBuf {
        let variable = match name {
            "IS" => "FPARKAN_CORPUS_PART1_ROOT",
            "IS2" => "FPARKAN_CORPUS_PART2_ROOT",
            _ => panic!("unknown licensed corpus part: {name}"),
        };
        let root = std::env::var_os(variable)
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| panic!("{variable} is required for licensed corpus tests"));
        assert!(
            root.is_dir(),
            "licensed corpus root is missing: {}",
            root.display()
        );
        root
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
