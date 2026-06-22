#![forbid(unsafe_code)]
//! Stage-3 MSH asset contract.

use encoding_rs::WINDOWS_1251;
use fparkan_nres::{EntryMeta, NresDocument, NresError};

/// Node table stream.
pub const STREAM_NODE_TABLE: u32 = 1;
/// Slot stream.
pub const STREAM_SLOTS: u32 = 2;
/// Position stream.
pub const STREAM_POSITIONS: u32 = 3;
/// Normal stream.
pub const STREAM_NORMALS: u32 = 4;
/// Texture coordinate stream.
pub const STREAM_UV0: u32 = 5;
/// Triangle index stream.
pub const STREAM_INDICES: u32 = 6;
/// Animation key stream.
pub const STREAM_ANIMATION_KEYS: u32 = 8;
/// Node names stream.
pub const STREAM_NAMES: u32 = 10;
/// Batch stream.
pub const STREAM_BATCHES: u32 = 13;
/// Animation frame map stream.
pub const STREAM_ANIMATION_FRAME_MAP: u32 = 19;

const REQUIRED_STREAMS: &[(u32, &str)] = &[
    (STREAM_NODE_TABLE, "Res1"),
    (STREAM_SLOTS, "Res2"),
    (STREAM_POSITIONS, "Res3"),
    (STREAM_INDICES, "Res6"),
    (STREAM_BATCHES, "Res13"),
];

/// MSH document backed by a lossless nested `NRes` archive.
#[derive(Clone, Debug)]
pub struct MshDocument {
    nres: NresDocument,
    streams: Vec<StreamDescriptor>,
}

/// Stream descriptor in original archive order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamDescriptor {
    /// Stream type identifier.
    pub type_id: u32,
    /// Opaque stream attributes.
    pub attributes: EntryAttributes,
    /// Raw stream name bytes before the first NUL terminator.
    pub name: Vec<u8>,
    /// Payload size in bytes.
    pub size: u32,
}

/// Opaque `NRes` entry attributes preserved for roundtrip.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EntryAttributes {
    /// Opaque attribute 1.
    pub attr1: u32,
    /// Opaque attribute 2.
    pub attr2: u32,
    /// Opaque attribute 3.
    pub attr3: u32,
}

/// MSH variant id.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MshVariantId(pub u32);

/// Validated model asset.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelAsset {
    /// Original node stride.
    pub node_stride: usize,
    /// Number of nodes.
    pub node_count: usize,
    /// Raw node table.
    pub nodes_raw: Vec<u8>,
    /// Slot table.
    pub slots: Vec<Slot>,
    /// Vertex positions.
    pub positions: Vec<[f32; 3]>,
    /// Optional normals.
    pub normals: Option<Vec<[i8; 4]>>,
    /// Optional texture coordinates.
    pub uv0: Option<Vec<[i16; 2]>>,
    /// Triangle indices.
    pub indices: Vec<u16>,
    /// Draw batches.
    pub batches: Vec<Batch>,
    /// Optional decoded node names.
    pub node_names: Option<Vec<Option<String>>>,
}

/// Node id.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodeId(pub u32);

/// Slot id.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SlotId(pub u32);

/// Raw node view.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Node {
    /// Raw node bytes.
    pub raw: Vec<u8>,
}

/// Slot descriptor.
#[derive(Clone, Debug, PartialEq)]
pub struct Slot {
    /// First triangle descriptor.
    pub tri_start: u16,
    /// Triangle descriptor count.
    pub tri_count: u16,
    /// First batch index.
    pub batch_start: u16,
    /// Batch count.
    pub batch_count: u16,
    /// AABB minimum.
    pub aabb_min: [f32; 3],
    /// AABB maximum.
    pub aabb_max: [f32; 3],
    /// Bounding sphere center.
    pub sphere_center: [f32; 3],
    /// Bounding sphere radius.
    pub sphere_radius: f32,
    /// Opaque slot tail.
    pub opaque: [u32; 5],
}

/// Draw batch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Batch {
    /// Batch flags.
    pub batch_flags: u16,
    /// Material index.
    pub material_index: u16,
    /// Opaque field.
    pub opaque4: u16,
    /// Opaque field.
    pub opaque6: u16,
    /// Index count.
    pub index_count: u16,
    /// First index offset.
    pub index_start: u32,
    /// Opaque field.
    pub opaque14: u16,
    /// Base vertex.
    pub base_vertex: u32,
}

/// Preserved triangle descriptor stream marker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TriangleDescriptor;

/// Vertex stream view.
#[derive(Clone, Debug, PartialEq)]
pub struct VertexStreams {
    /// Vertex positions.
    pub positions: Vec<[f32; 3]>,
    /// Optional normals.
    pub normals: Option<Vec<[i8; 4]>>,
    /// Optional texture coordinates.
    pub uv0: Option<Vec<[i16; 2]>>,
}

/// Preserved non-core stream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreservedStream {
    /// Stream type id.
    pub type_id: u32,
    /// Stream attributes.
    pub attributes: EntryAttributes,
    /// Original payload bytes.
    pub bytes: std::sync::Arc<[u8]>,
}

/// LOD id.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Lod(pub u8);

/// Group id.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Group(pub u8);

/// MSH decode or validation error.
#[derive(Debug)]
pub enum MshError {
    /// Nested `NRes` error.
    Nres(NresError),
    /// Required stream is absent.
    MissingStream {
        /// Stream type id.
        type_id: u32,
        /// Human-readable stream label.
        label: &'static str,
    },
    /// Required stream appears more than once.
    DuplicateStream {
        /// Stream type id.
        type_id: u32,
        /// Human-readable stream label.
        label: &'static str,
    },
    /// Legacy compatibility backend rejected the geometry.
    InvalidGeometry(String),
    /// Slot id is outside the validated model.
    SlotOutOfBounds {
        /// Requested slot id.
        slot: u32,
        /// Slot count.
        slot_count: usize,
    },
    /// Batch range is outside the validated model.
    BatchRangeOutOfBounds {
        /// First requested batch.
        start: usize,
        /// Exclusive end.
        end: usize,
        /// Batch count.
        batch_count: usize,
    },
    /// Batch references a vertex outside position stream.
    VertexIndexOutOfBounds {
        /// Batch index.
        batch: usize,
        /// Resolved vertex index.
        vertex: u64,
        /// Position count.
        position_count: usize,
    },
    /// Non-finite or inverted bounds.
    InvalidBounds {
        /// Slot index.
        slot: usize,
    },
}

impl From<NresError> for MshError {
    fn from(value: NresError) -> Self {
        Self::Nres(value)
    }
}

impl std::fmt::Display for MshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Nres(source) => write!(f, "{source}"),
            Self::MissingStream { type_id, label } => {
                write!(f, "missing MSH stream {label} ({type_id})")
            }
            Self::DuplicateStream { type_id, label } => {
                write!(f, "duplicate MSH stream {label} ({type_id})")
            }
            Self::InvalidGeometry(message) => write!(f, "{message}"),
            Self::SlotOutOfBounds { slot, slot_count } => {
                write!(f, "slot {slot} is outside slot table of {slot_count}")
            }
            Self::BatchRangeOutOfBounds {
                start,
                end,
                batch_count,
            } => write!(
                f,
                "batch range {start}..{end} is outside batch table of {batch_count}"
            ),
            Self::VertexIndexOutOfBounds {
                batch,
                vertex,
                position_count,
            } => write!(
                f,
                "batch {batch} references vertex {vertex}, position_count={position_count}"
            ),
            Self::InvalidBounds { slot } => write!(f, "slot {slot} has invalid bounds"),
        }
    }
}

impl std::error::Error for MshError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Nres(source) => Some(source),
            Self::MissingStream { .. }
            | Self::DuplicateStream { .. }
            | Self::InvalidGeometry(_)
            | Self::SlotOutOfBounds { .. }
            | Self::BatchRangeOutOfBounds { .. }
            | Self::VertexIndexOutOfBounds { .. }
            | Self::InvalidBounds { .. } => None,
        }
    }
}

/// Decodes a nested MSH `NRes` document.
///
/// # Errors
///
/// Returns [`MshError`] when required streams are absent or duplicated.
pub fn decode_msh(document: &NresDocument) -> Result<MshDocument, MshError> {
    for (type_id, label) in REQUIRED_STREAMS {
        let count = document
            .entries()
            .iter()
            .filter(|entry| entry.meta().type_id == *type_id)
            .count();
        if count == 0 {
            return Err(MshError::MissingStream {
                type_id: *type_id,
                label,
            });
        }
        if count > 1 {
            return Err(MshError::DuplicateStream {
                type_id: *type_id,
                label,
            });
        }
    }

    let streams = document
        .entries()
        .iter()
        .map(|entry| stream_descriptor(entry.meta(), entry.name_bytes()))
        .collect();

    Ok(MshDocument {
        nres: document.clone(),
        streams,
    })
}

/// Validates static geometry and returns a backend-neutral model asset.
///
/// # Errors
///
/// Returns [`MshError`] when stream sizes, slot ranges, batch ranges, bounds,
/// or indexed vertices are invalid.
pub fn validate_msh(document: &MshDocument) -> Result<ModelAsset, MshError> {
    let model = parse_model_document(&document.nres)?;
    validate_bounds(&model)?;
    validate_vertex_indices(&model)?;
    Ok(model)
}

/// Returns the selected slot for a node/lod/group tuple.
#[must_use]
pub fn selected_slot(model: &ModelAsset, node: NodeId, lod: Lod, group: Group) -> Option<SlotId> {
    if model.node_stride != 38 || lod.0 >= 3 || group.0 >= 5 {
        return None;
    }
    let node_index = usize::try_from(node.0).ok()?;
    if node_index >= model.node_count {
        return None;
    }
    let node_off = node_index.checked_mul(model.node_stride)?;
    let slot_off = node_off
        .checked_add(8)?
        .checked_add((usize::from(lod.0) * 5 + usize::from(group.0)) * 2)?;
    let raw = read_u16(&model.nodes_raw, slot_off)?;
    if raw == u16::MAX {
        return None;
    }
    let slot = usize::from(raw);
    (slot < model.slots.len()).then_some(SlotId(u32::from(raw)))
}

/// Returns draw batches for a validated slot.
///
/// # Errors
///
/// Returns [`MshError`] when the slot id or its batch range is invalid.
pub fn draw_batches(model: &ModelAsset, slot: SlotId) -> Result<&[Batch], MshError> {
    let slot_index = usize::try_from(slot.0).map_err(|_| MshError::SlotOutOfBounds {
        slot: slot.0,
        slot_count: model.slots.len(),
    })?;
    let slot_ref = model
        .slots
        .get(slot_index)
        .ok_or(MshError::SlotOutOfBounds {
            slot: slot.0,
            slot_count: model.slots.len(),
        })?;
    let start = usize::from(slot_ref.batch_start);
    let end = start.checked_add(usize::from(slot_ref.batch_count)).ok_or(
        MshError::BatchRangeOutOfBounds {
            start,
            end: usize::MAX,
            batch_count: model.batches.len(),
        },
    )?;
    model
        .batches
        .get(start..end)
        .ok_or(MshError::BatchRangeOutOfBounds {
            start,
            end,
            batch_count: model.batches.len(),
        })
}

impl MshDocument {
    /// Returns original stream descriptors.
    #[must_use]
    pub fn streams(&self) -> &[StreamDescriptor] {
        &self.streams
    }

    /// Returns the recognized MSH variant id.
    #[must_use]
    pub fn variant_id(&self) -> MshVariantId {
        if self
            .streams
            .iter()
            .any(|stream| stream.name.eq_ignore_ascii_case(b"MTCHECK"))
        {
            MshVariantId(1)
        } else {
            MshVariantId(0)
        }
    }

    /// Returns preserved non-core streams.
    ///
    /// # Errors
    ///
    /// Returns [`MshError`] when the underlying `NRes` payload lookup fails.
    pub fn preserved_streams(&self) -> Result<Vec<PreservedStream>, MshError> {
        let mut preserved = Vec::new();
        for entry in self.nres.entries() {
            let type_id = entry.meta().type_id;
            if REQUIRED_STREAMS
                .iter()
                .any(|(required, _)| *required == type_id)
            {
                continue;
            }
            preserved.push(PreservedStream {
                type_id,
                attributes: attributes(entry.meta()),
                bytes: std::sync::Arc::from(
                    self.nres.payload(entry.id())?.to_vec().into_boxed_slice(),
                ),
            });
        }
        Ok(preserved)
    }
}

fn stream_descriptor(meta: &EntryMeta, name: &[u8]) -> StreamDescriptor {
    StreamDescriptor {
        type_id: meta.type_id,
        attributes: attributes(meta),
        name: name.to_vec(),
        size: meta.data_size,
    }
}

fn attributes(meta: &EntryMeta) -> EntryAttributes {
    EntryAttributes {
        attr1: meta.attr1,
        attr2: meta.attr2,
        attr3: meta.attr3,
    }
}

fn parse_model_document(document: &NresDocument) -> Result<ModelAsset, MshError> {
    let nodes_stream = read_required_stream(document, STREAM_NODE_TABLE, "Res1")?;
    let slots_stream = read_required_stream(document, STREAM_SLOTS, "Res2")?;
    let positions_stream = read_required_stream(document, STREAM_POSITIONS, "Res3")?;
    let indices_stream = read_required_stream(document, STREAM_INDICES, "Res6")?;
    let batches_stream = read_required_stream(document, STREAM_BATCHES, "Res13")?;

    let node_stride = usize::try_from(nodes_stream.attributes.attr3)
        .map_err(|_| MshError::InvalidGeometry("MSH node stride does not fit usize".to_string()))?;
    if node_stride != 38 && node_stride != 24 {
        return Err(MshError::InvalidGeometry(format!(
            "unsupported MSH node stride: {node_stride}"
        )));
    }
    if !nodes_stream.bytes.len().is_multiple_of(node_stride) {
        return Err(invalid_resource_size(
            "Res1",
            nodes_stream.bytes.len(),
            node_stride,
        ));
    }
    let node_count = nodes_stream.bytes.len() / node_stride;

    let slots = parse_slots(&slots_stream.bytes)?;
    let positions = parse_positions(&positions_stream.bytes)?;
    let indices = parse_u16_array(&indices_stream.bytes, "Res6")?;
    let batches = parse_batches(&batches_stream.bytes)?;
    validate_slot_batch_ranges(&slots, batches.len())?;
    validate_batch_index_ranges(&batches, indices.len())?;

    let normals = read_optional_stream(document, STREAM_NORMALS)?
        .map(|raw| parse_i8x4_array(&raw.bytes, "Res4"))
        .transpose()?;
    let uv0 = read_optional_stream(document, STREAM_UV0)?
        .map(|raw| parse_i16x2_array(&raw.bytes, "Res5"))
        .transpose()?;
    let node_names = read_optional_stream(document, STREAM_NAMES)?
        .map(|raw| parse_res10_names(&raw.bytes, node_count))
        .transpose()?;

    Ok(ModelAsset {
        node_stride,
        node_count,
        nodes_raw: nodes_stream.bytes,
        slots,
        positions,
        normals,
        uv0,
        indices,
        batches,
        node_names,
    })
}

struct RawStream {
    attributes: EntryAttributes,
    bytes: Vec<u8>,
}

fn read_required_stream(
    document: &NresDocument,
    type_id: u32,
    label: &'static str,
) -> Result<RawStream, MshError> {
    let entry = document
        .entries()
        .iter()
        .find(|entry| entry.meta().type_id == type_id)
        .ok_or(MshError::MissingStream { type_id, label })?;
    Ok(RawStream {
        attributes: attributes(entry.meta()),
        bytes: document.payload(entry.id())?.to_vec(),
    })
}

fn read_optional_stream(
    document: &NresDocument,
    type_id: u32,
) -> Result<Option<RawStream>, MshError> {
    let Some(entry) = document
        .entries()
        .iter()
        .find(|entry| entry.meta().type_id == type_id)
    else {
        return Ok(None);
    };
    Ok(Some(RawStream {
        attributes: attributes(entry.meta()),
        bytes: document.payload(entry.id())?.to_vec(),
    }))
}

fn parse_slots(data: &[u8]) -> Result<Vec<Slot>, MshError> {
    if data.len() < 0x8C {
        return Err(MshError::InvalidGeometry(format!(
            "invalid Res2 size: {}",
            data.len()
        )));
    }
    let slot_bytes = data
        .len()
        .checked_sub(0x8C)
        .ok_or_else(|| MshError::InvalidGeometry("integer overflow".to_string()))?;
    if !slot_bytes.is_multiple_of(68) {
        return Err(invalid_resource_size("Res2.slots", slot_bytes, 68));
    }
    let count = slot_bytes / 68;
    let mut slots = Vec::with_capacity(count);
    for index in 0..count {
        let offset = 0x8Cusize
            .checked_add(
                index
                    .checked_mul(68)
                    .ok_or_else(|| MshError::InvalidGeometry("integer overflow".to_string()))?,
            )
            .ok_or_else(|| MshError::InvalidGeometry("integer overflow".to_string()))?;
        slots.push(Slot {
            tri_start: read_u16_required(data, offset)?,
            tri_count: read_u16_required(data, offset + 2)?,
            batch_start: read_u16_required(data, offset + 4)?,
            batch_count: read_u16_required(data, offset + 6)?,
            aabb_min: [
                read_f32(data, offset + 8)?,
                read_f32(data, offset + 12)?,
                read_f32(data, offset + 16)?,
            ],
            aabb_max: [
                read_f32(data, offset + 20)?,
                read_f32(data, offset + 24)?,
                read_f32(data, offset + 28)?,
            ],
            sphere_center: [
                read_f32(data, offset + 32)?,
                read_f32(data, offset + 36)?,
                read_f32(data, offset + 40)?,
            ],
            sphere_radius: read_f32(data, offset + 44)?,
            opaque: [
                read_u32(data, offset + 48)?,
                read_u32(data, offset + 52)?,
                read_u32(data, offset + 56)?,
                read_u32(data, offset + 60)?,
                read_u32(data, offset + 64)?,
            ],
        });
    }
    Ok(slots)
}

fn parse_positions(data: &[u8]) -> Result<Vec<[f32; 3]>, MshError> {
    if !data.len().is_multiple_of(12) {
        return Err(invalid_resource_size("Res3", data.len(), 12));
    }
    let mut out = Vec::with_capacity(data.len() / 12);
    for offset in (0..data.len()).step_by(12) {
        out.push([
            read_f32(data, offset)?,
            read_f32(data, offset + 4)?,
            read_f32(data, offset + 8)?,
        ]);
    }
    Ok(out)
}

fn parse_batches(data: &[u8]) -> Result<Vec<Batch>, MshError> {
    if !data.len().is_multiple_of(20) {
        return Err(invalid_resource_size("Res13", data.len(), 20));
    }
    let mut out = Vec::with_capacity(data.len() / 20);
    for offset in (0..data.len()).step_by(20) {
        out.push(Batch {
            batch_flags: read_u16_required(data, offset)?,
            material_index: read_u16_required(data, offset + 2)?,
            opaque4: read_u16_required(data, offset + 4)?,
            opaque6: read_u16_required(data, offset + 6)?,
            index_count: read_u16_required(data, offset + 8)?,
            index_start: read_u32(data, offset + 10)?,
            opaque14: read_u16_required(data, offset + 14)?,
            base_vertex: read_u32(data, offset + 16)?,
        });
    }
    Ok(out)
}

fn parse_u16_array(data: &[u8], label: &'static str) -> Result<Vec<u16>, MshError> {
    if !data.len().is_multiple_of(2) {
        return Err(invalid_resource_size(label, data.len(), 2));
    }
    let mut out = Vec::with_capacity(data.len() / 2);
    for offset in (0..data.len()).step_by(2) {
        out.push(read_u16_required(data, offset)?);
    }
    Ok(out)
}

fn parse_i8x4_array(data: &[u8], label: &'static str) -> Result<Vec<[i8; 4]>, MshError> {
    if !data.len().is_multiple_of(4) {
        return Err(invalid_resource_size(label, data.len(), 4));
    }
    let mut out = Vec::with_capacity(data.len() / 4);
    for offset in (0..data.len()).step_by(4) {
        out.push([
            read_i8(data, offset)?,
            read_i8(data, offset + 1)?,
            read_i8(data, offset + 2)?,
            read_i8(data, offset + 3)?,
        ]);
    }
    Ok(out)
}

fn parse_i16x2_array(data: &[u8], label: &'static str) -> Result<Vec<[i16; 2]>, MshError> {
    if !data.len().is_multiple_of(4) {
        return Err(invalid_resource_size(label, data.len(), 4));
    }
    let mut out = Vec::with_capacity(data.len() / 4);
    for offset in (0..data.len()).step_by(4) {
        out.push([read_i16(data, offset)?, read_i16(data, offset + 2)?]);
    }
    Ok(out)
}

fn parse_res10_names(data: &[u8], node_count: usize) -> Result<Vec<Option<String>>, MshError> {
    let mut out = Vec::with_capacity(node_count);
    let mut offset = 0usize;
    for _ in 0..node_count {
        let len = usize::try_from(read_u32(data, offset)?)
            .map_err(|_| MshError::InvalidGeometry("integer overflow".to_string()))?;
        offset = offset
            .checked_add(4)
            .ok_or_else(|| MshError::InvalidGeometry("integer overflow".to_string()))?;
        if len == 0 {
            out.push(None);
            continue;
        }
        let need = len
            .checked_add(1)
            .ok_or_else(|| MshError::InvalidGeometry("integer overflow".to_string()))?;
        let end = offset
            .checked_add(need)
            .ok_or_else(|| MshError::InvalidGeometry("integer overflow".to_string()))?;
        let slice = data
            .get(offset..end)
            .ok_or_else(|| invalid_resource_size("Res10", data.len(), 1))?;
        let text = if slice.last().copied() == Some(0) {
            &slice[..slice.len().saturating_sub(1)]
        } else {
            slice
        };
        let (decoded, _, _) = WINDOWS_1251.decode(text);
        out.push(Some(decoded.into_owned()));
        offset = end;
    }
    Ok(out)
}

fn validate_slot_batch_ranges(slots: &[Slot], batch_count: usize) -> Result<(), MshError> {
    for slot in slots {
        let start = usize::from(slot.batch_start);
        let end = start
            .checked_add(usize::from(slot.batch_count))
            .ok_or_else(|| MshError::InvalidGeometry("integer overflow".to_string()))?;
        if end > batch_count {
            return Err(MshError::BatchRangeOutOfBounds {
                start,
                end,
                batch_count,
            });
        }
    }
    Ok(())
}

fn validate_batch_index_ranges(batches: &[Batch], index_count: usize) -> Result<(), MshError> {
    for (batch_index, batch) in batches.iter().enumerate() {
        let start = usize::try_from(batch.index_start)
            .map_err(|_| MshError::InvalidGeometry("integer overflow".to_string()))?;
        let end = start
            .checked_add(usize::from(batch.index_count))
            .ok_or_else(|| MshError::InvalidGeometry("integer overflow".to_string()))?;
        if end > index_count {
            return Err(MshError::VertexIndexOutOfBounds {
                batch: batch_index,
                vertex: u64::try_from(end).unwrap_or(u64::MAX),
                position_count: index_count,
            });
        }
    }
    Ok(())
}

fn invalid_resource_size(label: &'static str, size: usize, stride: usize) -> MshError {
    MshError::InvalidGeometry(format!(
        "invalid {label} size: size={size}, stride={stride}"
    ))
}

fn validate_bounds(model: &ModelAsset) -> Result<(), MshError> {
    for (index, slot) in model.slots.iter().enumerate() {
        let ordered = slot
            .aabb_min
            .iter()
            .zip(slot.aabb_max.iter())
            .all(|(min, max)| min.is_finite() && max.is_finite() && min <= max);
        let sphere = slot.sphere_center.iter().all(|value| value.is_finite())
            && slot.sphere_radius.is_finite()
            && slot.sphere_radius >= 0.0;
        if !ordered || !sphere {
            return Err(MshError::InvalidBounds { slot: index });
        }
    }
    Ok(())
}

fn validate_vertex_indices(model: &ModelAsset) -> Result<(), MshError> {
    let position_count =
        u64::try_from(model.positions.len()).map_err(|_| MshError::VertexIndexOutOfBounds {
            batch: usize::MAX,
            vertex: u64::MAX,
            position_count: model.positions.len(),
        })?;
    for (batch_index, batch) in model.batches.iter().enumerate() {
        let start =
            usize::try_from(batch.index_start).map_err(|_| MshError::VertexIndexOutOfBounds {
                batch: batch_index,
                vertex: u64::MAX,
                position_count: model.positions.len(),
            })?;
        let end = start.checked_add(usize::from(batch.index_count)).ok_or(
            MshError::VertexIndexOutOfBounds {
                batch: batch_index,
                vertex: u64::MAX,
                position_count: model.positions.len(),
            },
        )?;
        for raw in &model.indices[start..end] {
            let vertex = u64::from(batch.base_vertex) + u64::from(*raw);
            if vertex >= position_count {
                return Err(MshError::VertexIndexOutOfBounds {
                    batch: batch_index,
                    vertex,
                    position_count: model.positions.len(),
                });
            }
        }
    }
    Ok(())
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let raw = bytes.get(offset..offset.checked_add(2)?)?;
    let arr: [u8; 2] = raw.try_into().ok()?;
    Some(u16::from_le_bytes(arr))
}

fn read_u16_required(bytes: &[u8], offset: usize) -> Result<u16, MshError> {
    let raw = bytes
        .get(offset..offset.saturating_add(2))
        .ok_or_else(|| MshError::InvalidGeometry("integer overflow".to_string()))?;
    let arr: [u8; 2] = raw
        .try_into()
        .map_err(|_| MshError::InvalidGeometry("integer overflow".to_string()))?;
    Ok(u16::from_le_bytes(arr))
}

fn read_i16(bytes: &[u8], offset: usize) -> Result<i16, MshError> {
    let raw = bytes
        .get(offset..offset.saturating_add(2))
        .ok_or_else(|| MshError::InvalidGeometry("integer overflow".to_string()))?;
    let arr: [u8; 2] = raw
        .try_into()
        .map_err(|_| MshError::InvalidGeometry("integer overflow".to_string()))?;
    Ok(i16::from_le_bytes(arr))
}

fn read_i8(bytes: &[u8], offset: usize) -> Result<i8, MshError> {
    let byte = bytes
        .get(offset)
        .copied()
        .ok_or_else(|| MshError::InvalidGeometry("integer overflow".to_string()))?;
    Ok(i8::from_le_bytes([byte]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, MshError> {
    let raw = bytes
        .get(offset..offset.saturating_add(4))
        .ok_or_else(|| MshError::InvalidGeometry("integer overflow".to_string()))?;
    let arr: [u8; 4] = raw
        .try_into()
        .map_err(|_| MshError::InvalidGeometry("integer overflow".to_string()))?;
    Ok(u32::from_le_bytes(arr))
}

fn read_f32(bytes: &[u8], offset: usize) -> Result<f32, MshError> {
    Ok(f32::from_bits(read_u32(bytes, offset)?))
}

/// Returns migration status.
#[must_use]
pub fn migration_facade_ready() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_animation::{
        canonical_timed_pose_capture, AnimKey24, AnimationTime, TimedPoseKey, TimedPoseTrack,
    };
    use fparkan_nres::ReadProfile;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    #[test]
    fn validates_minimal_msh_document() {
        let document = decode_nested(&minimal_msh_bytes()).expect("nested NRes");
        let msh = decode_msh(&document).expect("msh document");
        let model = validate_msh(&msh).expect("model");

        assert_eq!(model.node_stride, 38);
        assert_eq!(model.node_count, 0);
        assert!(model.slots.is_empty());
    }

    #[test]
    fn missing_required_stream_is_error() {
        let document = decode_nested(&build_nres(&[
            stream(STREAM_NODE_TABLE, 38, b"Res1", &[]),
            stream(STREAM_SLOTS, 0, b"Res2", &[0; 0x8c]),
        ]))
        .expect("nested NRes");

        let err = decode_msh(&document).expect_err("missing stream");
        assert!(matches!(
            err,
            MshError::MissingStream {
                type_id: STREAM_POSITIONS,
                ..
            }
        ));
    }

    #[test]
    fn base_vertex_plus_index_must_reference_position() {
        let mut indices = Vec::new();
        indices.extend_from_slice(&1_u16.to_le_bytes());
        let mut batch = Vec::new();
        push_u16(&mut batch, 0);
        push_u16(&mut batch, 0);
        push_u16(&mut batch, 0);
        push_u16(&mut batch, 0);
        push_u16(&mut batch, 1);
        push_u32(&mut batch, 0);
        push_u16(&mut batch, 0);
        push_u32(&mut batch, 0);
        let bytes = build_nres(&[
            stream(STREAM_NODE_TABLE, 38, b"Res1", &[]),
            stream(STREAM_SLOTS, 0, b"Res2", &[0; 0x8c]),
            stream(STREAM_POSITIONS, 0, b"Res3", &[]),
            stream(STREAM_INDICES, 0, b"Res6", &indices),
            stream(STREAM_BATCHES, 0, b"Res13", &batch),
        ]);
        let document = decode_nested(&bytes).expect("nested NRes");
        let msh = decode_msh(&document).expect("msh document");

        let err = validate_msh(&msh).expect_err("invalid vertex");
        assert!(matches!(err, MshError::VertexIndexOutOfBounds { .. }));
    }

    #[test]
    fn canonical_stream_set_is_independent_of_entry_order() {
        let slots = slots_payload(&[]);
        let ordered = decode_nested(&build_nres(&[
            stream(STREAM_NODE_TABLE, 38, b"Res1", &[]),
            stream(STREAM_SLOTS, 0, b"Res2", &slots),
            stream(STREAM_POSITIONS, 0, b"Res3", &[]),
            stream(STREAM_INDICES, 0, b"Res6", &[]),
            stream(STREAM_BATCHES, 0, b"Res13", &[]),
        ]))
        .expect("ordered");
        let reversed = decode_nested(&build_nres(&[
            stream(STREAM_BATCHES, 0, b"Res13", &[]),
            stream(STREAM_INDICES, 0, b"Res6", &[]),
            stream(STREAM_POSITIONS, 0, b"Res3", &[]),
            stream(STREAM_SLOTS, 0, b"Res2", &slots),
            stream(STREAM_NODE_TABLE, 38, b"Res1", &[]),
        ]))
        .expect("reversed");

        assert_eq!(
            validate_msh(&decode_msh(&ordered).expect("ordered msh")).expect("ordered model"),
            validate_msh(&decode_msh(&reversed).expect("reversed msh")).expect("reversed model")
        );
    }

    #[test]
    fn duplicate_required_stream_type_is_error() {
        let slots = slots_payload(&[]);
        let document = decode_nested(&build_nres(&[
            stream(STREAM_NODE_TABLE, 38, b"Res1", &[]),
            stream(STREAM_NODE_TABLE, 38, b"Res1Dup", &[]),
            stream(STREAM_SLOTS, 0, b"Res2", &slots),
            stream(STREAM_POSITIONS, 0, b"Res3", &[]),
            stream(STREAM_INDICES, 0, b"Res6", &[]),
            stream(STREAM_BATCHES, 0, b"Res13", &[]),
        ]))
        .expect("nested NRes");

        assert!(matches!(
            decode_msh(&document),
            Err(MshError::DuplicateStream {
                type_id: STREAM_NODE_TABLE,
                ..
            })
        ));
    }

    #[test]
    fn node38_stride_is_exact() {
        let slots = slots_payload(&[]);
        let valid_node = node38([u16::MAX; 15]);
        let valid = decode_nested(&build_nres(&[
            stream(STREAM_NODE_TABLE, 38, b"Res1", &valid_node),
            stream(STREAM_SLOTS, 0, b"Res2", &slots),
            stream(STREAM_POSITIONS, 0, b"Res3", &[]),
            stream(STREAM_INDICES, 0, b"Res6", &[]),
            stream(STREAM_BATCHES, 0, b"Res13", &[]),
        ]))
        .expect("valid");
        let model = validate_msh(&decode_msh(&valid).expect("msh")).expect("model");
        assert_eq!(model.node_stride, 38);
        assert_eq!(model.node_count, 1);

        let invalid = decode_nested(&build_nres(&[
            stream(STREAM_NODE_TABLE, 38, b"Res1", &valid_node[..37]),
            stream(STREAM_SLOTS, 0, b"Res2", &slots),
            stream(STREAM_POSITIONS, 0, b"Res3", &[]),
            stream(STREAM_INDICES, 0, b"Res6", &[]),
            stream(STREAM_BATCHES, 0, b"Res13", &[]),
        ]))
        .expect("invalid");
        let err = validate_msh(&decode_msh(&invalid).expect("msh")).expect_err("stride");
        assert!(matches!(err, MshError::InvalidGeometry(_)));
    }

    #[test]
    fn node38_uses_three_by_five_slot_mapping_and_absent_marker() {
        let mut mapping = [u16::MAX; 15];
        mapping[0] = 0;
        mapping[7] = 1;
        let node = node38(mapping);
        let slots = slots_payload(&[
            slot_record(0, 0, [0.0, 0.0, 0.0], [1.0, 1.0, 1.0], 1.0),
            slot_record(0, 0, [0.0, 0.0, 0.0], [2.0, 2.0, 2.0], 1.0),
        ]);
        let document = decode_nested(&build_nres(&[
            stream(STREAM_NODE_TABLE, 38, b"Res1", &node),
            stream(STREAM_SLOTS, 0, b"Res2", &slots),
            stream(STREAM_POSITIONS, 0, b"Res3", &[]),
            stream(STREAM_INDICES, 0, b"Res6", &[]),
            stream(STREAM_BATCHES, 0, b"Res13", &[]),
        ]))
        .expect("nested");
        let model = validate_msh(&decode_msh(&document).expect("msh")).expect("model");

        assert_eq!(
            selected_slot(&model, NodeId(0), Lod(0), Group(0)),
            Some(SlotId(0))
        );
        assert_eq!(
            selected_slot(&model, NodeId(0), Lod(1), Group(2)),
            Some(SlotId(1))
        );
        assert_eq!(selected_slot(&model, NodeId(0), Lod(2), Group(4)), None);
    }

    #[test]
    fn type2_header_and_slot_tail_framing_are_exact() {
        let too_small = decode_nested(&build_nres(&[
            stream(STREAM_NODE_TABLE, 38, b"Res1", &[]),
            stream(STREAM_SLOTS, 0, b"Res2", &[0; 0x8b]),
            stream(STREAM_POSITIONS, 0, b"Res3", &[]),
            stream(STREAM_INDICES, 0, b"Res6", &[]),
            stream(STREAM_BATCHES, 0, b"Res13", &[]),
        ]))
        .expect("nested");
        let err = validate_msh(&decode_msh(&too_small).expect("msh")).expect_err("header");
        assert!(matches!(err, MshError::InvalidGeometry(_)));

        let not_divisible = vec![0; 0x8c + 67];
        let bad_tail = decode_nested(&build_nres(&[
            stream(STREAM_NODE_TABLE, 38, b"Res1", &[]),
            stream(STREAM_SLOTS, 0, b"Res2", &not_divisible),
            stream(STREAM_POSITIONS, 0, b"Res3", &[]),
            stream(STREAM_INDICES, 0, b"Res6", &[]),
            stream(STREAM_BATCHES, 0, b"Res13", &[]),
        ]))
        .expect("nested");
        let err = validate_msh(&decode_msh(&bad_tail).expect("msh")).expect_err("tail");
        assert!(matches!(err, MshError::InvalidGeometry(_)));
    }

    #[test]
    fn slot_batch_range_out_of_bounds_is_error() {
        let slots = slots_payload(&[slot_record(1, 1, [0.0, 0.0, 0.0], [1.0, 1.0, 1.0], 1.0)]);
        let document = decode_nested(&build_nres(&[
            stream(STREAM_NODE_TABLE, 38, b"Res1", &[]),
            stream(STREAM_SLOTS, 0, b"Res2", &slots),
            stream(STREAM_POSITIONS, 0, b"Res3", &[]),
            stream(STREAM_INDICES, 0, b"Res6", &[]),
            stream(STREAM_BATCHES, 0, b"Res13", &[]),
        ]))
        .expect("nested");

        assert!(matches!(
            validate_msh(&decode_msh(&document).expect("msh")),
            Err(MshError::BatchRangeOutOfBounds {
                start: 1,
                end: 2,
                batch_count: 0
            })
        ));
    }

    #[test]
    fn vertex_stream_strides_are_exact() {
        for (stream_type, name, payload) in [
            (STREAM_POSITIONS, b"Res3".as_slice(), vec![0; 11]),
            (STREAM_NORMALS, b"Res4".as_slice(), vec![0; 3]),
            (STREAM_UV0, b"Res5".as_slice(), vec![0; 3]),
            (STREAM_INDICES, b"Res6".as_slice(), vec![0; 1]),
        ] {
            let slots = slots_payload(&[]);
            let mut entries = vec![
                stream(STREAM_NODE_TABLE, 38, b"Res1", &[]),
                stream(STREAM_SLOTS, 0, b"Res2", &slots),
                stream(STREAM_POSITIONS, 0, b"Res3", &[]),
                stream(STREAM_INDICES, 0, b"Res6", &[]),
                stream(STREAM_BATCHES, 0, b"Res13", &[]),
            ];
            if stream_type == STREAM_POSITIONS {
                entries[2] = stream(stream_type, 0, name, &payload);
            } else if stream_type == STREAM_INDICES {
                entries[3] = stream(stream_type, 0, name, &payload);
            } else {
                entries.push(stream(stream_type, 0, name, &payload));
            }
            let document = decode_nested(&build_nres(&entries)).expect("nested");
            let err = validate_msh(&decode_msh(&document).expect("msh")).expect_err("stride");
            assert!(matches!(err, MshError::InvalidGeometry(_)));
        }
    }

    #[test]
    fn batch20_uses_unaligned_field_offsets() {
        let positions = positions_payload(&[[0.0, 0.0, 0.0]]);
        let mut indices = Vec::new();
        push_u16(&mut indices, 0);
        let batch = batch_record(0x1100, 0x2200, 0x3300, 0x4400, 1, 0, 0x5500, 0);
        let document = decode_nested(&build_nres(&[
            stream(STREAM_NODE_TABLE, 38, b"Res1", &[]),
            stream(STREAM_SLOTS, 0, b"Res2", &slots_payload(&[])),
            stream(STREAM_POSITIONS, 0, b"Res3", &positions),
            stream(STREAM_INDICES, 0, b"Res6", &indices),
            stream(STREAM_BATCHES, 0, b"Res13", &batch),
        ]))
        .expect("nested");
        let model = validate_msh(&decode_msh(&document).expect("msh")).expect("model");

        assert_eq!(model.batches[0].batch_flags, 0x1100);
        assert_eq!(model.batches[0].material_index, 0x2200);
        assert_eq!(model.batches[0].opaque4, 0x3300);
        assert_eq!(model.batches[0].opaque6, 0x4400);
        assert_eq!(model.batches[0].index_count, 1);
        assert_eq!(model.batches[0].index_start, 0);
        assert_eq!(model.batches[0].opaque14, 0x5500);
        assert_eq!(model.batches[0].base_vertex, 0);
    }

    #[test]
    fn auxiliary_and_extended_streams_are_preserved() {
        let aux = [1, 2, 3, 4];
        let ext18 = [5, 6];
        let ext20 = [7, 8, 9];
        let document = decode_nested(&build_nres(&[
            stream(STREAM_NODE_TABLE, 38, b"Res1", &[]),
            stream(STREAM_SLOTS, 0, b"Res2", &slots_payload(&[])),
            stream(STREAM_POSITIONS, 0, b"Res3", &[]),
            stream(STREAM_INDICES, 0, b"Res6", &[]),
            stream(STREAM_BATCHES, 0, b"Res13", &[]),
            stream(99, 0, b"Aux", &aux),
            stream(18, 0, b"Res18", &ext18),
            stream(20, 0, b"Res20", &ext20),
        ]))
        .expect("nested");
        let msh = decode_msh(&document).expect("msh");
        let preserved = msh.preserved_streams().expect("preserved");

        assert_eq!(
            preserved
                .iter()
                .map(|stream| (stream.type_id, stream.bytes.as_ref().to_vec()))
                .collect::<Vec<_>>(),
            vec![
                (99, aux.to_vec()),
                (18, ext18.to_vec()),
                (20, ext20.to_vec())
            ]
        );
    }

    #[test]
    fn mtcheck_variant_is_preserved_and_recognized() {
        let marker = [0x4D, 0x54];
        let document = decode_nested(&build_nres(&[
            stream(STREAM_NODE_TABLE, 38, b"Res1", &[]),
            stream(STREAM_SLOTS, 0, b"Res2", &slots_payload(&[])),
            stream(STREAM_POSITIONS, 0, b"Res3", &[]),
            stream(STREAM_INDICES, 0, b"Res6", &[]),
            stream(STREAM_BATCHES, 0, b"Res13", &[]),
            stream(21, 0, b"MTCHECK", &marker),
        ]))
        .expect("nested");
        let msh = decode_msh(&document).expect("msh");

        assert_eq!(msh.variant_id(), MshVariantId(1));
        assert_eq!(msh.streams().last().expect("marker").name, b"MTCHECK");
    }

    #[test]
    fn invalid_bounds_are_rejected() {
        let slots = slots_payload(&[slot_record(0, 0, [2.0, 0.0, 0.0], [1.0, 1.0, 1.0], 1.0)]);
        let document = decode_nested(&build_nres(&[
            stream(STREAM_NODE_TABLE, 38, b"Res1", &[]),
            stream(STREAM_SLOTS, 0, b"Res2", &slots),
            stream(STREAM_POSITIONS, 0, b"Res3", &[]),
            stream(STREAM_INDICES, 0, b"Res6", &[]),
            stream(STREAM_BATCHES, 0, b"Res13", &[]),
        ]))
        .expect("nested");

        assert!(matches!(
            validate_msh(&decode_msh(&document).expect("msh")),
            Err(MshError::InvalidBounds { slot: 0 })
        ));
    }

    #[test]
    fn arbitrary_nested_payloads_are_bounded_and_panic_free() {
        for payload in [
            Vec::new(),
            vec![0; 16],
            build_nres(&[stream(STREAM_NODE_TABLE, 38, b"Res1", &[1, 2, 3])]),
            build_nres(&[
                stream(STREAM_NODE_TABLE, 38, b"Res1", &[]),
                stream(STREAM_SLOTS, 0, b"Res2", &[0; 0x8b]),
                stream(STREAM_POSITIONS, 0, b"Res3", &[1]),
                stream(STREAM_INDICES, 0, b"Res6", &[2]),
                stream(STREAM_BATCHES, 0, b"Res13", &[3]),
            ]),
        ] {
            if let Ok(document) = decode_nested(&payload) {
                let _ = decode_msh(&document).and_then(|msh| validate_msh(&msh).map(|_| ()));
            }
        }
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_corpus_msh_assets_validate() {
        for (corpus, expected) in [("IS", 435_usize), ("IS2", 511_usize)] {
            let root = corpus_root(corpus);
            let mut count = 0usize;
            for path in files_under(&root) {
                let Ok(bytes) = std::fs::read(&path) else {
                    continue;
                };
                let Ok(archive) = fparkan_nres::decode(
                    Arc::from(bytes.into_boxed_slice()),
                    ReadProfile::Compatible,
                ) else {
                    continue;
                };
                for entry in archive
                    .entries()
                    .iter()
                    .filter(|entry| has_msh_extension(entry.name_bytes()))
                {
                    let payload = archive.payload(entry.id()).expect("payload");
                    let nested = fparkan_nres::decode(
                        Arc::from(payload.to_vec().into_boxed_slice()),
                        ReadProfile::Compatible,
                    )
                    .unwrap_or_else(|err| {
                        panic!("{corpus} {path:?} {:?}: {err}", entry.name_bytes())
                    });
                    let msh = decode_msh(&nested).unwrap_or_else(|err| {
                        panic!("{corpus} {path:?} {:?}: {err}", entry.name_bytes())
                    });
                    validate_msh(&msh).unwrap_or_else(|err| {
                        panic!("{corpus} {path:?} {:?}: {err}", entry.name_bytes())
                    });
                    count += 1;
                }
            }
            assert_eq!(count, expected, "{corpus} MSH count");
        }
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_corpus_animation_streams_sample_approved_pose_captures() {
        for (
            corpus,
            expected_models,
            expected_animated_models,
            expected_node_samples,
            expected_hash,
        ) in [
            (
                "IS",
                435_usize,
                157_usize,
                3_469_usize,
                7_119_731_908_371_799_613_u64,
            ),
            (
                "IS2",
                511_usize,
                200_usize,
                5_233_usize,
                13_040_438_305_408_523_893_u64,
            ),
        ] {
            let root = corpus_root(corpus);
            let mut models = 0usize;
            let mut animated_models = 0usize;
            let mut node_samples = 0usize;
            let mut hash = FNV_OFFSET;
            for path in files_under(&root) {
                let Ok(bytes) = std::fs::read(&path) else {
                    continue;
                };
                let Ok(archive) = fparkan_nres::decode(
                    Arc::from(bytes.into_boxed_slice()),
                    ReadProfile::Compatible,
                ) else {
                    continue;
                };
                for entry in archive
                    .entries()
                    .iter()
                    .filter(|entry| has_msh_extension(entry.name_bytes()))
                {
                    let payload = archive.payload(entry.id()).expect("payload");
                    let nested = fparkan_nres::decode(
                        Arc::from(payload.to_vec().into_boxed_slice()),
                        ReadProfile::Compatible,
                    )
                    .unwrap_or_else(|err| {
                        panic!("{corpus} {path:?} {:?}: {err}", entry.name_bytes())
                    });
                    let msh = decode_msh(&nested).unwrap_or_else(|err| {
                        panic!("{corpus} {path:?} {:?}: {err}", entry.name_bytes())
                    });
                    let model = validate_msh(&msh).unwrap_or_else(|err| {
                        panic!("{corpus} {path:?} {:?}: {err}", entry.name_bytes())
                    });
                    let preserved = msh.preserved_streams().unwrap_or_else(|err| {
                        panic!("{corpus} {path:?} {:?}: {err}", entry.name_bytes())
                    });
                    let keys_stream = preserved
                        .iter()
                        .find(|stream| stream.type_id == STREAM_ANIMATION_KEYS)
                        .unwrap_or_else(|| {
                            panic!("{corpus} {path:?} {:?}: missing type 8", entry.name_bytes())
                        });
                    let frame_map_stream = preserved
                        .iter()
                        .find(|stream| stream.type_id == STREAM_ANIMATION_FRAME_MAP)
                        .unwrap_or_else(|| {
                            panic!(
                                "{corpus} {path:?} {:?}: missing type 19",
                                entry.name_bytes()
                            )
                        });
                    if !keys_stream.bytes.len().is_multiple_of(24)
                        || !frame_map_stream.bytes.len().is_multiple_of(2)
                    {
                        panic!(
                            "{corpus} {path:?} {:?}: invalid animation stream size",
                            entry.name_bytes()
                        );
                    }

                    let keys = decode_anim_keys(&keys_stream.bytes).unwrap_or_else(|err| {
                        panic!("{corpus} {path:?} {:?}: type 8: {err}", entry.name_bytes())
                    });
                    let frame_map = decode_frame_map_words(&frame_map_stream.bytes);
                    let frame_count = usize::try_from(frame_map_stream.attributes.attr2)
                        .expect("frame count fits usize");
                    models += 1;
                    hash_bytes(&mut hash, entry.name_bytes());
                    hash_usize(&mut hash, keys.len());
                    hash_usize(&mut hash, frame_map.len());

                    let mut model_is_animated = false;
                    if model.node_stride == 38 {
                        for node_index in 0..model.node_count {
                            let offset = node_index * model.node_stride;
                            let anim_map_start =
                                read_u16(&model.nodes_raw, offset + 4).expect("anim map");
                            let fallback_key =
                                read_u16(&model.nodes_raw, offset + 6).expect("fallback key");
                            let fallback_index = usize::from(fallback_key);
                            assert!(
                                fallback_index < keys.len(),
                                "{corpus} {path:?} {:?}: fallback key out of range",
                                entry.name_bytes()
                            );
                            let sample_frames = representative_frames(frame_count, anim_map_start);
                            if anim_map_start != u16::MAX {
                                let start = usize::from(anim_map_start);
                                assert!(
                                    start
                                        .checked_add(frame_count)
                                        .is_some_and(|end| end <= frame_map.len()),
                                    "{corpus} {path:?} {:?}: frame map range out of bounds",
                                    entry.name_bytes()
                                );
                                model_is_animated = true;
                            }
                            for frame in sample_frames {
                                let pose = sample_node_pose(
                                    &keys,
                                    &frame_map,
                                    frame_count,
                                    anim_map_start,
                                    fallback_index,
                                    frame,
                                )
                                .unwrap_or_else(|err| {
                                    let selected = selected_animation_key(
                                        &frame_map,
                                        frame_count,
                                        anim_map_start,
                                        fallback_index,
                                        frame,
                                    );
                                    let selected_key = &keys[selected];
                                    let next_key = keys.get(selected.saturating_add(1));
                                    let fallback_key = &keys[fallback_index];
                                    panic!(
                                        "{corpus} {path:?} {:?}: node {node_index} frame {frame}: {err}; map_start={anim_map_start} fallback={fallback_index} selected={selected:?} frame_count={frame_count} selected_time={:?} selected_rot={:?} next={:?} fallback_time={:?} fallback_rot={:?}",
                                        entry.name_bytes(),
                                        selected_key.time,
                                        selected_key.pose.rotation,
                                        next_key.map(|key| (key.time, key.pose.rotation)),
                                        fallback_key.time,
                                        fallback_key.pose.rotation
                                    )
                                });
                                let track = TimedPoseTrack::new(
                                    pose,
                                    vec![TimedPoseKey {
                                        time: AnimationTime(frame as f32),
                                        pose,
                                    }],
                                )
                                .expect("single pose track");
                                let capture = canonical_timed_pose_capture(
                                    &track,
                                    &[AnimationTime(frame as f32)],
                                )
                                .expect("pose capture");
                                hash_usize(&mut hash, node_index);
                                hash_usize(&mut hash, frame);
                                hash_bytes(&mut hash, &capture);
                                node_samples += 1;
                            }
                        }
                    }
                    if model_is_animated {
                        animated_models += 1;
                    }
                }
            }

            assert_eq!(
                models, expected_models,
                "{corpus} animated stream model count"
            );
            assert_eq!(
                animated_models, expected_animated_models,
                "{corpus} animated model count"
            );
            assert_eq!(node_samples, expected_node_samples, "{corpus} node samples");
            assert_eq!(hash, expected_hash, "{corpus} animation capture hash");
        }
    }

    fn decode_anim_keys(bytes: &[u8]) -> Result<Vec<AnimKey24>, fparkan_animation::AnimationError> {
        bytes.chunks_exact(24).map(AnimKey24::decode).collect()
    }

    fn decode_frame_map_words(bytes: &[u8]) -> Vec<u16> {
        bytes
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect()
    }

    fn representative_frames(frame_count: usize, anim_map_start: u16) -> Vec<usize> {
        if anim_map_start == u16::MAX || frame_count == 0 {
            return vec![0];
        }
        let mut frames = vec![0, frame_count / 2, frame_count - 1];
        frames.sort_unstable();
        frames.dedup();
        frames
    }

    fn sample_node_pose(
        keys: &[AnimKey24],
        frame_map: &[u16],
        frame_count: usize,
        anim_map_start: u16,
        fallback_index: usize,
        frame: usize,
    ) -> Result<fparkan_animation::Pose, fparkan_animation::AnimationError> {
        let key_index = selected_animation_key(
            frame_map,
            frame_count,
            anim_map_start,
            fallback_index,
            frame,
        );
        sample_key_pair(keys, key_index, fallback_index, frame)
    }

    fn selected_animation_key(
        frame_map: &[u16],
        frame_count: usize,
        anim_map_start: u16,
        fallback_index: usize,
        frame: usize,
    ) -> usize {
        if anim_map_start == u16::MAX || frame >= frame_count {
            return fallback_index;
        }
        let mapped = frame_map[usize::from(anim_map_start) + frame];
        if usize::from(mapped) >= fallback_index {
            fallback_index
        } else {
            usize::from(mapped)
        }
    }

    fn sample_key_pair(
        keys: &[AnimKey24],
        key_index: usize,
        fallback_index: usize,
        frame: usize,
    ) -> Result<fparkan_animation::Pose, fparkan_animation::AnimationError> {
        if key_index == fallback_index {
            return Ok(keys[fallback_index].sampling_pose());
        }
        let next_index = key_index.saturating_add(1);
        if next_index >= keys.len() || keys[next_index].time.0 <= keys[key_index].time.0 {
            return Ok(keys[key_index].sampling_pose());
        }
        let track = TimedPoseTrack::new(
            keys[key_index].sampling_pose(),
            vec![
                TimedPoseKey {
                    time: keys[key_index].time,
                    pose: keys[key_index].sampling_pose(),
                },
                TimedPoseKey {
                    time: keys[next_index].time,
                    pose: keys[next_index].sampling_pose(),
                },
            ],
        )?;
        track.sample(AnimationTime(frame as f32))
    }

    fn decode_nested(bytes: &[u8]) -> Result<NresDocument, NresError> {
        fparkan_nres::decode(
            Arc::from(bytes.to_vec().into_boxed_slice()),
            ReadProfile::Compatible,
        )
    }

    fn minimal_msh_bytes() -> Vec<u8> {
        build_nres(&[
            stream(STREAM_NODE_TABLE, 38, b"Res1", &[]),
            stream(STREAM_SLOTS, 0, b"Res2", &slots_payload(&[])),
            stream(STREAM_POSITIONS, 0, b"Res3", &[]),
            stream(STREAM_INDICES, 0, b"Res6", &[]),
            stream(STREAM_BATCHES, 0, b"Res13", &[]),
        ])
    }

    fn stream<'a>(type_id: u32, attr3: u32, name: &'a [u8], payload: &'a [u8]) -> TestEntry<'a> {
        TestEntry {
            type_id,
            attr3,
            name,
            payload,
        }
    }

    struct TestEntry<'a> {
        type_id: u32,
        attr3: u32,
        name: &'a [u8],
        payload: &'a [u8],
    }

    fn build_nres(entries: &[TestEntry<'_>]) -> Vec<u8> {
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

    fn push_u16(out: &mut Vec<u8>, value: u16) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn push_u32(out: &mut Vec<u8>, value: u32) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn push_f32(out: &mut Vec<u8>, value: f32) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn node38(slots: [u16; 15]) -> Vec<u8> {
        let mut out = vec![0; 8];
        for slot in slots {
            push_u16(&mut out, slot);
        }
        out
    }

    fn slots_payload(records: &[Vec<u8>]) -> Vec<u8> {
        let mut out = vec![0; 0x8c];
        for record in records {
            assert_eq!(record.len(), 68);
            out.extend_from_slice(record);
        }
        out
    }

    fn slot_record(
        batch_start: u16,
        batch_count: u16,
        aabb_min: [f32; 3],
        aabb_max: [f32; 3],
        sphere_radius: f32,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        push_u16(&mut out, 0);
        push_u16(&mut out, 0);
        push_u16(&mut out, batch_start);
        push_u16(&mut out, batch_count);
        for value in aabb_min {
            push_f32(&mut out, value);
        }
        for value in aabb_max {
            push_f32(&mut out, value);
        }
        for value in [0.0, 0.0, 0.0] {
            push_f32(&mut out, value);
        }
        push_f32(&mut out, sphere_radius);
        for _ in 0..5 {
            push_u32(&mut out, 0);
        }
        out
    }

    fn positions_payload(values: &[[f32; 3]]) -> Vec<u8> {
        let mut out = Vec::new();
        for position in values {
            for value in position {
                push_f32(&mut out, *value);
            }
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn batch_record(
        batch_flags: u16,
        material_index: u16,
        opaque4: u16,
        opaque6: u16,
        index_count: u16,
        index_start: u32,
        opaque14: u16,
        base_vertex: u32,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        push_u16(&mut out, batch_flags);
        push_u16(&mut out, material_index);
        push_u16(&mut out, opaque4);
        push_u16(&mut out, opaque6);
        push_u16(&mut out, index_count);
        push_u32(&mut out, index_start);
        push_u16(&mut out, opaque14);
        push_u32(&mut out, base_vertex);
        out
    }

    fn copy_cstr(dst: &mut [u8], src: &[u8]) {
        let len = dst.len().saturating_sub(1).min(src.len());
        dst[..len].copy_from_slice(&src[..len]);
    }

    fn has_msh_extension(name: &[u8]) -> bool {
        name.len() >= 4 && name[name.len() - 4..].eq_ignore_ascii_case(b".msh")
    }

    fn corpus_root(name: &str) -> PathBuf {
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

    fn files_under(root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(path) = stack.pop() {
            let Ok(read_dir) = std::fs::read_dir(path) else {
                continue;
            };
            for entry in read_dir.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    out.push(path);
                }
            }
        }
        out.sort();
        out
    }

    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
        for byte in bytes {
            *hash ^= u64::from(*byte);
            *hash = hash.wrapping_mul(FNV_PRIME);
        }
    }

    fn hash_usize(hash: &mut u64, value: usize) {
        hash_bytes(hash, &value.to_le_bytes());
    }
}
