#![forbid(unsafe_code)]
//! Stage-3 MSH asset contract.

use encoding_rs::WINDOWS_1251;
use fparkan_animation::{
    evaluate_hierarchy, AnimKey24, AnimationTime, NodePoseBuffer, ParentIndex, Pose, TimedPoseKey,
    TimedPoseTrack,
};
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
/// Optional secondary texture coordinate stream used by native lightmap/detail
/// stages.  MSH stores it as the type-18 four-byte packed UV stream.
pub const STREAM_UV1: u32 = 18;
/// Optional type-16 stream. Its presence marks MSH vertex-colour data.
pub const STREAM_TYPE_16: u32 = 16;
/// Triangle index stream.
pub const STREAM_INDICES: u32 = 6;
/// Optional triangle-descriptor stream.
pub const STREAM_TRIANGLE_DESCRIPTORS: u32 = 7;
const TRIANGLE_NORMAL_SCALE: f32 = 1.0 / 32_767.0;
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
    /// Model-level bounding sphere center from the Res2 header.
    pub bounding_sphere_center: [f32; 3],
    /// Model-level bounding sphere radius from the Res2 header.
    pub bounding_sphere_radius: f32,
    /// Slot table.
    pub slots: Vec<Slot>,
    /// Vertex positions.
    pub positions: Vec<[f32; 3]>,
    /// Optional normals.
    pub normals: Option<Vec<[i8; 4]>>,
    /// Optional primary packed texture coordinates from type 5.
    pub uv0: Option<Vec<[u16; 2]>>,
    /// Optional secondary packed texture coordinates from type 18.
    pub uv1: Option<Vec<[u16; 2]>>,
    /// Whether the source MSH contains a type-16 vertex-colour stream.
    ///
    /// The stream payload is preserved separately and is not decoded here.
    pub type16_present: bool,
    /// Triangle indices.
    pub indices: Vec<u16>,
    /// Draw batches.
    pub batches: Vec<Batch>,
    /// Optional type-7 triangle descriptors.
    pub triangle_descriptors: Option<Vec<TriangleDescriptor>>,
    /// Optional decoded node names.
    pub node_names: Option<Vec<Option<String>>>,
    /// Optional decoded node-animation streams.
    pub animation: Option<ModelAnimation>,
}

/// Decoded MSH node-animation streams.
///
/// This preserves the exact type-8/type-19 boundary used by `Node38` without
/// assigning runtime-state ownership to a static asset.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelAnimation {
    /// Type-8 animation keys in source order.
    pub keys: Vec<AnimKey24>,
    /// Type-19 frame-to-key mapping words in source order.
    pub frame_map: Vec<u16>,
    /// Declared frame count from type-19 `attr2`.
    pub frame_count: u32,
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

/// Raw fields of the standard 38-byte node layout that precede slot selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Node38Metadata {
    /// Raw Node38 header/flag word at byte offset zero.
    pub header_flags: u16,
    /// Source-local parent word at byte offset two; `0xFFFF` is the native
    /// sentinel before component rebasing.
    pub parent_or_link_raw: u16,
    /// Offset into the type-19 frame map, or `0xFFFF` for no map.
    pub anim_map_start: u16,
    /// Fallback type-8 key index.
    pub fallback_key: u16,
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
    /// High material selector/slot field at byte offset `+0x02`.
    ///
    /// The ordinary WEAR material selector is the byte at `+0x04`. This
    /// field is retained separately because the native path can override it
    /// with a forced material index.
    pub material_index_hi: u16,
    /// WEAR material selector at byte offset `+0x04`.
    pub material_index: u8,
    /// Lightmap selector at byte offset `+0x05`.
    pub lightmap_index: u8,
    /// Local batch index at byte offset `+0x06`.
    pub local_batch_index: u16,
    /// Index count.
    pub index_count: u16,
    /// First index offset.
    pub index_start: u32,
    /// Vertex count at byte offset `+0x0E`.
    pub vertex_count: u16,
    /// Base vertex.
    pub base_vertex: u32,
}

/// One 16-byte type-7 triangle descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TriangleDescriptor {
    /// Triangle mask flags at byte offset `+0`.
    pub tri_flags: u16,
    /// Opaque 16-bit words from offsets `+2`, `+4`, `+6` and `+14`.
    pub opaque: [u16; 4],
    /// Signed packed plane normal at byte offsets `+8`, `+10` and `+12`.
    pub normal: [i16; 3],
}

/// Include-all and exclude-any masks used by the native MSH query.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SegmentQueryMasks {
    /// Node38 header bits that must all be set.
    pub node_include: u16,
    /// Node38 header bits that must all be clear.
    pub node_exclude: u16,
    /// Batch20 leading-DWORD bits that must all be set.
    pub batch_include: u32,
    /// Batch20 leading-DWORD bits that must all be clear.
    pub batch_exclude: u32,
    /// Type-7 triangle bits that must all be set.
    pub triangle_include: u16,
    /// Type-7 triangle bits that must all be clear.
    pub triangle_exclude: u16,
}

/// One accepted segment hit from the native MSH triangle-query path.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SegmentHit {
    /// Hit position in the same coordinate frame as the input segment.
    pub position: [f32; 3],
    /// Squared distance from the input segment's first endpoint.
    pub distance_squared: f32,
    /// Global Batch20 index.
    pub batch_index: usize,
    /// Global type-7 descriptor index.
    pub triangle_descriptor_index: usize,
    /// Global Res3 position indices of the hit triangle.
    pub vertex_indices: [usize; 3],
}

/// Vertex stream view.
#[derive(Clone, Debug, PartialEq)]
pub struct VertexStreams {
    /// Vertex positions.
    pub positions: Vec<[f32; 3]>,
    /// Optional normals.
    pub normals: Option<Vec<[i8; 4]>>,
    /// Optional primary packed texture coordinates from type 5.
    pub uv0: Option<Vec<[u16; 2]>>,
    /// Optional secondary packed texture coordinates from type 18.
    pub uv1: Option<Vec<[u16; 2]>>,
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

/// Native state id kept under the historical `Lod` API name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Lod(pub u8);

/// Native LOD id kept under the historical `Group` API name.
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

/// Returns the selected slot for a node/native-state/native-LOD tuple.
///
/// The argument names are retained for API compatibility: `Lod` is the
/// three-value native state and `Group` is the five-value native LOD.
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

/// Returns the undecorated metadata of a standard 38-byte node.
#[must_use]
pub fn node38_metadata(model: &ModelAsset, node: NodeId) -> Option<Node38Metadata> {
    if model.node_stride != 38 {
        return None;
    }
    let node_index = usize::try_from(node.0).ok()?;
    if node_index >= model.node_count {
        return None;
    }
    let offset = node_index.checked_mul(model.node_stride)?;
    Some(Node38Metadata {
        header_flags: read_u16(&model.nodes_raw, offset)?,
        parent_or_link_raw: read_u16(&model.nodes_raw, offset + 2)?,
        anim_map_start: read_u16(&model.nodes_raw, offset + 4)?,
        fallback_key: read_u16(&model.nodes_raw, offset + 6)?,
    })
}

/// Returns the fallback key as a native Node38 source-local pose.
///
/// The legacy sampler uses this key when a node has no frame map or its current
/// frame falls outside that map. It is a static-pose input only; callers must
/// assemble the runtime hierarchy separately and apply the component-specific
/// mapping of `parent_or_link_raw`. Its quaternion is converted to match native
/// AniMesh's quaternion-to-matrix convention.
#[must_use]
pub fn node38_fallback_pose(model: &ModelAsset, node: NodeId) -> Option<Pose> {
    let metadata = node38_metadata(model, node)?;
    model
        .animation
        .as_ref()?
        .keys
        .get(usize::from(metadata.fallback_key))
        .map(AnimKey24::sampling_pose)
        .map(node38_native_local_pose)
}

/// Converts a sampled MSH source-local quaternion into the native AniMesh
/// quaternion-matrix convention before Node38 hierarchy composition. Native
/// AniMesh's matrix coefficients are the transpose of the portable `Pose`
/// basis.
fn node38_native_local_pose(mut pose: Pose) -> Pose {
    pose.rotation[0] = -pose.rotation[0];
    pose.rotation[1] = -pose.rotation[1];
    pose.rotation[2] = -pose.rotation[2];
    pose
}

/// Evaluates the static fallback pose hierarchy of a standard `Node38` model.
///
/// `0xFFFF` denotes a root. Every non-root source link must name an earlier
/// node, which preserves the producer's parent-before-child ordering. Models
/// that do not meet this established `Node38` contract return `None` so callers
/// can retain an explicitly unhierarchical diagnostic path.
#[must_use]
pub fn node38_fallback_hierarchy(model: &ModelAsset) -> Option<NodePoseBuffer> {
    if model.node_stride != 38 || model.node_count == 0 {
        return None;
    }
    let mut parents = Vec::with_capacity(model.node_count);
    let mut poses = Vec::with_capacity(model.node_count);
    for index in 0..model.node_count {
        let node = NodeId(u32::try_from(index).ok()?);
        let metadata = node38_metadata(model, node)?;
        let parent = if metadata.parent_or_link_raw == u16::MAX {
            ParentIndex(None)
        } else {
            let parent = usize::from(metadata.parent_or_link_raw);
            (parent < index).then_some(ParentIndex(Some(metadata.parent_or_link_raw)))?
        };
        parents.push(parent);
        poses.push(node38_fallback_pose(model, node)?);
    }
    evaluate_hierarchy(&parents, &poses).ok()
}

/// Evaluates a standard `Node38` hierarchy at one explicitly supplied logical
/// frame, using portable key sampling and native AniMesh local-matrix
/// convention.
///
/// A node without a usable type-19 map, or a requested frame outside the
/// declared map length, retains its exact fallback key. Mapped keys are
/// sampled against their immediate successor using the decoded key times.
/// This is an offline asset-sampling contract; it does not claim ownership of
/// the original runtime's animation clock or x87 numeric profile.
///
/// Returns `None` when the model lacks a complete standard-node animation
/// representation or the map cannot be safely resolved.
#[must_use]
pub fn node38_sampled_hierarchy(model: &ModelAsset, frame: u16) -> Option<NodePoseBuffer> {
    node38_sampled_hierarchy_with_selector(model, |_| {
        Some((u32::from(frame), AnimationTime(f32::from(frame))))
    })
}

/// Evaluates a standard `Node38` hierarchy with an independent floating-point
/// sample time for each node.
///
/// `Some(time)` uses the nearest-even integer to `time - 0.5` as the type-19
/// frame-map selector, then samples the selected key and its immediate
/// successor at the original fractional time. This matches the native
/// sampler's selector calculation while keeping interpolation time fractional.
/// The selector uses Rust's default-nearest, ties-to-even rounding. This is
/// the portable counterpart of the native x87 conversion under its default
/// rounding mode; it does not claim parity under every x87 control-word mode.
/// A missing time uses that node's exact fallback key. Supplied times must be
/// finite and non-negative; the caller owns choosing the per-node times.
///
/// Returns `None` when the model is not a complete standard-node animation,
/// the slice does not match the node count, any supplied time is invalid, or a
/// frame-map entry cannot be safely resolved.
#[must_use]
pub fn node38_sampled_hierarchy_at_times(
    model: &ModelAsset,
    node_times: &[Option<AnimationTime>],
) -> Option<NodePoseBuffer> {
    if node_times.len() != model.node_count
        || node_times
            .iter()
            .flatten()
            .any(|time| !time.0.is_finite() || time.0 < 0.0)
    {
        return None;
    }
    node38_sampled_hierarchy_with_selector(model, |index| {
        let time = node_times[index]?;
        let selector = (time.0 - 0.5).round_ties_even();
        // A finite, non-negative time can still be beyond the representable
        // map range. Saturate to a frame that will resolve to the node's
        // fallback key rather than allowing a float-to-index wraparound.
        let map_frame = if selector < 0.0 || selector >= u32::MAX as f32 {
            u32::MAX
        } else {
            selector as u32
        };
        Some((map_frame, time))
    })
}

fn node38_sampled_hierarchy_with_selector(
    model: &ModelAsset,
    mut selector_for_node: impl FnMut(usize) -> Option<(u32, AnimationTime)>,
) -> Option<NodePoseBuffer> {
    if model.node_stride != 38 || model.node_count == 0 {
        return None;
    }
    let animation = model.animation.as_ref()?;
    let mut parents = Vec::with_capacity(model.node_count);
    let mut poses = Vec::with_capacity(model.node_count);
    for index in 0..model.node_count {
        let node = NodeId(u32::try_from(index).ok()?);
        let metadata = node38_metadata(model, node)?;
        let parent = if metadata.parent_or_link_raw == u16::MAX {
            ParentIndex(None)
        } else {
            let parent = usize::from(metadata.parent_or_link_raw);
            (parent < index).then_some(ParentIndex(Some(metadata.parent_or_link_raw)))?
        };
        let fallback_index = usize::from(metadata.fallback_key);
        let _fallback = animation.keys.get(fallback_index)?;
        let sample = selector_for_node(index);
        let key_index = if let Some((map_frame, _)) = sample {
            if metadata.anim_map_start == u16::MAX
                || map_frame == u32::MAX
                || map_frame >= animation.frame_count
            {
                fallback_index
            } else {
                let mapped_index = usize::from(
                    *animation.frame_map.get(
                        usize::from(metadata.anim_map_start)
                            .checked_add(usize::try_from(map_frame).ok()?)?,
                    )?,
                );
                if mapped_index < fallback_index {
                    mapped_index
                } else {
                    fallback_index
                }
            }
        } else {
            fallback_index
        };
        let sample_time = sample.map_or(AnimationTime(0.0), |(_, time)| time);
        let pose = node38_native_local_pose(sample_node38_key_pair(
            &animation.keys,
            key_index,
            fallback_index,
            sample_time,
        )?);
        parents.push(parent);
        poses.push(pose);
    }
    evaluate_hierarchy(&parents, &poses).ok()
}

fn sample_node38_key_pair(
    keys: &[AnimKey24],
    key_index: usize,
    fallback_index: usize,
    sample_time: AnimationTime,
) -> Option<Pose> {
    let key = *keys.get(key_index)?;
    if key_index == fallback_index {
        return Some(key.sampling_pose());
    }
    let next = *keys.get(key_index.checked_add(1)?)?;
    if next.time.0 <= key.time.0 {
        return Some(key.sampling_pose());
    }
    let track = TimedPoseTrack::new(
        key.sampling_pose(),
        vec![
            TimedPoseKey {
                time: key.time,
                pose: key.sampling_pose(),
            },
            TimedPoseKey {
                time: next.time,
                pose: next.sampling_pose(),
            },
        ],
    )
    .ok()?;
    track.sample(sample_time).ok()
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

/// Queries a segment against one slot using the native Node38, Batch20 and
/// type-7 include-all/exclude-any masks.
///
/// The first endpoint is the native forward direction. Batch bit `0x0002`
/// retries the plane test in reverse only when the forward plane test misses.
/// The returned distance is always measured from the original first endpoint.
///
/// # Errors
///
/// Returns [`MshError`] when type 7 is absent, the model does not use Node38,
/// or the selected slot's descriptor, batch, index or vertex ranges are invalid.
pub fn query_slot_segment(
    model: &ModelAsset,
    node: NodeId,
    slot: SlotId,
    endpoints: [[f32; 3]; 2],
    masks: SegmentQueryMasks,
) -> Result<Option<SegmentHit>, MshError> {
    let descriptors = model
        .triangle_descriptors
        .as_deref()
        .ok_or(MshError::MissingStream {
            type_id: STREAM_TRIANGLE_DESCRIPTORS,
            label: "Res7",
        })?;
    if model.node_stride != 38 {
        return Err(MshError::InvalidGeometry(format!(
            "segment query requires Node38 metadata, got node stride {}",
            model.node_stride
        )));
    }
    if !endpoints.iter().flatten().all(|value| value.is_finite()) {
        return Err(MshError::InvalidGeometry(
            "segment endpoints must be finite".to_string(),
        ));
    }
    let node_flags = node38_metadata(model, node)
        .ok_or_else(|| MshError::InvalidGeometry("invalid Node38 metadata".to_string()))?
        .header_flags;
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
    let batches = draw_batches(model, slot)?;
    let descriptor_start = usize::from(slot_ref.tri_start);
    let descriptor_end = descriptor_start
        .checked_add(usize::from(slot_ref.tri_count))
        .ok_or_else(|| MshError::InvalidGeometry("type-7 range overflow".to_string()))?;
    if descriptor_end > descriptors.len() {
        return Err(MshError::InvalidGeometry(format!(
            "slot {slot_index} type-7 range {descriptor_start}..{descriptor_end} exceeds {} descriptors",
            descriptors.len()
        )));
    }

    let mut face_count = 0usize;
    for (local_batch, batch) in batches.iter().enumerate() {
        if batch.index_count % 3 != 0 {
            return Err(MshError::InvalidGeometry(format!(
                "batch {} index count {} is not triangle-aligned",
                usize::from(slot_ref.batch_start) + local_batch,
                batch.index_count
            )));
        }
        let start = usize::try_from(batch.index_start)
            .map_err(|_| MshError::InvalidGeometry("batch index range overflow".to_string()))?;
        let end = start
            .checked_add(usize::from(batch.index_count))
            .ok_or_else(|| MshError::InvalidGeometry("batch index range overflow".to_string()))?;
        if end > model.indices.len() {
            return Err(MshError::InvalidGeometry(format!(
                "batch {} index range {start}..{end} exceeds {} indices",
                usize::from(slot_ref.batch_start) + local_batch,
                model.indices.len()
            )));
        }
        face_count = face_count
            .checked_add(usize::from(batch.index_count) / 3)
            .ok_or_else(|| MshError::InvalidGeometry("slot triangle count overflow".to_string()))?;
    }
    if face_count != usize::from(slot_ref.tri_count) {
        return Err(MshError::InvalidGeometry(format!(
            "slot {slot_index} declares {} type-7 descriptors but its batches contain {face_count} triangles",
            slot_ref.tri_count
        )));
    }
    if !mask_matches(
        u32::from(node_flags),
        u32::from(masks.node_include),
        u32::from(masks.node_exclude),
    ) {
        return Ok(None);
    }

    let mut best = None;
    let mut descriptor_cursor = descriptor_start;
    for (local_batch, batch) in batches.iter().enumerate() {
        let batch_index = usize::from(slot_ref.batch_start) + local_batch;
        let batch_flags = u32::from(batch.batch_flags) | (u32::from(batch.material_index_hi) << 16);
        let batch_face_count = usize::from(batch.index_count) / 3;
        if mask_matches(batch_flags, masks.batch_include, masks.batch_exclude) {
            for local_face in 0..batch_face_count {
                let triangle_descriptor_index = descriptor_cursor + local_face;
                let descriptor = descriptors[triangle_descriptor_index];
                if !mask_matches(
                    u32::from(descriptor.tri_flags),
                    u32::from(masks.triangle_include),
                    u32::from(masks.triangle_exclude),
                ) {
                    continue;
                }
                let index_start = usize::try_from(batch.index_start)
                    .expect("validated batch index range")
                    + local_face * 3;
                let base_vertex = usize::try_from(batch.base_vertex)
                    .map_err(|_| MshError::InvalidGeometry("base vertex overflow".to_string()))?;
                let mut vertex_indices = [0usize; 3];
                for (triangle_corner, index) in vertex_indices.iter_mut().enumerate() {
                    let raw_index = usize::from(model.indices[index_start + triangle_corner]);
                    *index = base_vertex.checked_add(raw_index).ok_or_else(|| {
                        MshError::InvalidGeometry("resolved vertex index overflow".to_string())
                    })?;
                    if *index >= model.positions.len() {
                        return Err(MshError::VertexIndexOutOfBounds {
                            batch: batch_index,
                            vertex: u64::try_from(*index).unwrap_or(u64::MAX),
                            position_count: model.positions.len(),
                        });
                    }
                }
                let triangle = vertex_indices.map(|index| model.positions[index]);
                if !triangle.iter().flatten().all(|value| value.is_finite()) {
                    return Err(MshError::InvalidGeometry(format!(
                        "triangle descriptor {triangle_descriptor_index} has a non-finite position"
                    )));
                }
                let normal = descriptor
                    .normal
                    .map(|value| f32::from(value) * TRIANGLE_NORMAL_SCALE);
                if normal == [0.0; 3] {
                    continue;
                }
                let forward =
                    native_segment_plane_hit(normal, triangle[0], endpoints[0], endpoints[1])?;
                let position = match forward {
                    Some(position) => Some(position),
                    None if batch.batch_flags & 0x0002 != 0 => {
                        native_segment_plane_hit(normal, triangle[0], endpoints[1], endpoints[0])?
                    }
                    None => None,
                };
                let Some(position) = position else {
                    continue;
                };
                if !native_point_in_triangle(normal, triangle, position)? {
                    continue;
                }
                let offset = [
                    position[0] - endpoints[0][0],
                    position[1] - endpoints[0][1],
                    position[2] - endpoints[0][2],
                ];
                let distance_squared = dot3(offset, offset);
                if !distance_squared.is_finite() {
                    return Err(MshError::InvalidGeometry(
                        "segment hit distance is non-finite".to_string(),
                    ));
                }
                if best.is_none_or(|hit: SegmentHit| distance_squared <= hit.distance_squared) {
                    best = Some(SegmentHit {
                        position,
                        distance_squared,
                        batch_index,
                        triangle_descriptor_index,
                        vertex_indices,
                    });
                }
            }
        }
        descriptor_cursor += batch_face_count;
    }
    Ok(best)
}

fn mask_matches(flags: u32, include: u32, exclude: u32) -> bool {
    flags & include == include && flags & exclude == 0
}

fn native_segment_plane_hit(
    normal: [f32; 3],
    anchor: [f32; 3],
    start: [f32; 3],
    end: [f32; 3],
) -> Result<Option<[f32; 3]>, MshError> {
    let direction = [end[0] - start[0], end[1] - start[1], end[2] - start[2]];
    let denominator = dot3(normal, direction);
    let start_distance = dot3(normal, start) - dot3(normal, anchor);
    let end_distance = start_distance + denominator;
    if !denominator.is_finite() || !start_distance.is_finite() || !end_distance.is_finite() {
        return Err(MshError::InvalidGeometry(
            "segment plane calculation is non-finite".to_string(),
        ));
    }
    if denominator >= 0.0 || start_distance <= 0.0 || end_distance > 0.0 {
        return Ok(None);
    }
    let ratio = start_distance / denominator;
    let position = [
        start[0] - direction[0] * ratio,
        start[1] - direction[1] * ratio,
        start[2] - direction[2] * ratio,
    ];
    if !position.iter().all(|value| value.is_finite()) {
        return Err(MshError::InvalidGeometry(
            "segment intersection is non-finite".to_string(),
        ));
    }
    Ok(Some(position))
}

fn native_point_in_triangle(
    normal: [f32; 3],
    triangle: [[f32; 3]; 3],
    point: [f32; 3],
) -> Result<bool, MshError> {
    let absolute = normal.map(f32::abs);
    let xy_max = if absolute[0] < absolute[1] {
        absolute[1]
    } else {
        absolute[0]
    };
    let dominant_axis = if absolute[2] <= xy_max {
        if absolute[0] < absolute[1] {
            1
        } else {
            0
        }
    } else {
        2
    };
    for index in 0..3 {
        let previous = triangle[(index + 2) % 3];
        let current = triangle[index];
        let relative = [
            point[0] - previous[0],
            point[1] - previous[1],
            point[2] - previous[2],
        ];
        let edge = [
            current[0] - previous[0],
            current[1] - previous[1],
            current[2] - previous[2],
        ];
        let side = match dominant_axis {
            0 => (relative[2] * edge[1] + relative[1] * -edge[2]) * normal[0],
            1 => (-edge[0] * relative[2] + relative[0] * edge[2]) * normal[1],
            _ => (relative[1] * edge[0] - relative[0] * edge[1]) * normal[2],
        };
        if !side.is_finite() {
            return Err(MshError::InvalidGeometry(
                "triangle edge calculation is non-finite".to_string(),
            ));
        }
        if side < 0.0 {
            return Ok(false);
        }
    }
    Ok(true)
}

fn dot3(left: [f32; 3], right: [f32; 3]) -> f32 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
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

    let (bounding_sphere_center, bounding_sphere_radius) =
        parse_res2_bounding_sphere(&slots_stream.bytes)?;
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
        .map(|raw| parse_u16x2_array(&raw.bytes, "Res5"))
        .transpose()?;
    let uv1 = read_optional_stream(document, STREAM_UV1)?
        .map(|raw| parse_secondary_uv_stream(raw, positions.len()))
        .transpose()?;
    let type16_present = read_optional_stream(document, STREAM_TYPE_16)?.is_some();
    let triangle_descriptors = read_optional_triangle_descriptors(document)?
        .map(|raw| parse_triangle_descriptors(&raw.bytes))
        .transpose()?;
    let node_names = read_optional_stream(document, STREAM_NAMES)?
        .map(|raw| parse_res10_names(&raw.bytes, node_count))
        .transpose()?;
    let animation = parse_optional_animation(document)?;

    Ok(ModelAsset {
        node_stride,
        node_count,
        nodes_raw: nodes_stream.bytes,
        bounding_sphere_center,
        bounding_sphere_radius,
        slots,
        positions,
        normals,
        uv0,
        uv1,
        type16_present,
        indices,
        batches,
        triangle_descriptors,
        node_names,
        animation,
    })
}

fn parse_optional_animation(document: &NresDocument) -> Result<Option<ModelAnimation>, MshError> {
    let keys = read_optional_stream(document, STREAM_ANIMATION_KEYS)?;
    let frame_map = read_optional_stream(document, STREAM_ANIMATION_FRAME_MAP)?;
    let (Some(keys), Some(frame_map)) = (keys, frame_map) else {
        return Ok(None);
    };
    if !keys.bytes.len().is_multiple_of(24) {
        return Err(invalid_resource_size("Res8", keys.bytes.len(), 24));
    }
    if !frame_map.bytes.len().is_multiple_of(2) {
        return Err(invalid_resource_size("Res19", frame_map.bytes.len(), 2));
    }
    let keys = keys
        .bytes
        .chunks_exact(24)
        .map(AnimKey24::decode)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| MshError::InvalidGeometry(format!("invalid Res8 animation key: {err}")))?;
    let frame_count = frame_map.attributes.attr2;
    let frame_map = parse_u16_array(&frame_map.bytes, "Res19")?;
    Ok(Some(ModelAnimation {
        keys,
        frame_map,
        frame_count,
    }))
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

fn read_optional_triangle_descriptors(
    document: &NresDocument,
) -> Result<Option<RawStream>, MshError> {
    let mut entries = document
        .entries()
        .iter()
        .filter(|entry| entry.meta().type_id == STREAM_TRIANGLE_DESCRIPTORS);
    let Some(entry) = entries.next() else {
        return Ok(None);
    };
    if entries.next().is_some() {
        return Err(MshError::DuplicateStream {
            type_id: STREAM_TRIANGLE_DESCRIPTORS,
            label: "Res7",
        });
    }
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

fn parse_res2_bounding_sphere(data: &[u8]) -> Result<([f32; 3], f32), MshError> {
    if data.len() < 0x8C {
        return Err(MshError::InvalidGeometry(format!(
            "invalid Res2 size: {}",
            data.len()
        )));
    }
    let center = [
        read_f32(data, 0x60)?,
        read_f32(data, 0x64)?,
        read_f32(data, 0x68)?,
    ];
    let radius = read_f32(data, 0x6C)?;
    if !center.iter().all(|value| value.is_finite()) || !radius.is_finite() || radius < 0.0 {
        return Err(MshError::InvalidGeometry(
            "invalid Res2 bounding sphere".to_string(),
        ));
    }
    Ok((center, radius))
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
            material_index_hi: read_u16_required(data, offset + 2)?,
            material_index: *data
                .get(offset + 4)
                .ok_or_else(|| MshError::InvalidGeometry("batch material selector".to_string()))?,
            lightmap_index: *data
                .get(offset + 5)
                .ok_or_else(|| MshError::InvalidGeometry("batch lightmap selector".to_string()))?,
            local_batch_index: read_u16_required(data, offset + 6)?,
            index_count: read_u16_required(data, offset + 8)?,
            index_start: read_u32(data, offset + 10)?,
            vertex_count: read_u16_required(data, offset + 14)?,
            base_vertex: read_u32(data, offset + 16)?,
        });
    }
    Ok(out)
}

fn parse_triangle_descriptors(data: &[u8]) -> Result<Vec<TriangleDescriptor>, MshError> {
    if !data.len().is_multiple_of(16) {
        return Err(invalid_resource_size("Res7", data.len(), 16));
    }
    let mut out = Vec::with_capacity(data.len() / 16);
    for offset in (0..data.len()).step_by(16) {
        out.push(TriangleDescriptor {
            tri_flags: read_u16_required(data, offset)?,
            opaque: [
                read_u16_required(data, offset + 2)?,
                read_u16_required(data, offset + 4)?,
                read_u16_required(data, offset + 6)?,
                read_u16_required(data, offset + 14)?,
            ],
            normal: [
                read_i16_required(data, offset + 8)?,
                read_i16_required(data, offset + 10)?,
                read_i16_required(data, offset + 12)?,
            ],
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

fn parse_u16x2_array(data: &[u8], label: &'static str) -> Result<Vec<[u16; 2]>, MshError> {
    if !data.len().is_multiple_of(4) {
        return Err(invalid_resource_size(label, data.len(), 4));
    }
    let mut out = Vec::with_capacity(data.len() / 4);
    for offset in (0..data.len()).step_by(4) {
        out.push([
            read_u16_required(data, offset)?,
            read_u16_required(data, offset + 2)?,
        ]);
    }
    Ok(out)
}

fn parse_secondary_uv_stream(
    stream: RawStream,
    position_count: usize,
) -> Result<Vec<[u16; 2]>, MshError> {
    if stream.attributes.attr3 != 4 {
        return Err(MshError::InvalidGeometry(format!(
            "invalid Res18 stride: expected 4, got {}",
            stream.attributes.attr3
        )));
    }
    let values = parse_u16x2_array(&stream.bytes, "Res18")?;
    let declared_count = usize::try_from(stream.attributes.attr1).map_err(|_| {
        MshError::InvalidGeometry("Res18 vertex count does not fit usize".to_string())
    })?;
    if declared_count != values.len() {
        return Err(MshError::InvalidGeometry(format!(
            "Res18 vertex count does not match payload: declared={}, decoded={}",
            declared_count,
            values.len()
        )));
    }
    if values.len() != position_count {
        return Err(MshError::InvalidGeometry(format!(
            "Res18 vertex count does not match Res3: uv1={}, positions={}",
            values.len(),
            position_count
        )));
    }
    Ok(values)
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

fn read_i16_required(bytes: &[u8], offset: usize) -> Result<i16, MshError> {
    Ok(i16::from_le_bytes(
        read_u16_required(bytes, offset)?.to_le_bytes(),
    ))
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
        assert_eq!(model.triangle_descriptors, None);
        assert!(!model.type16_present);
    }

    #[test]
    fn type16_presence_is_preserved_without_decoding_its_payload() {
        let payload = [0xDE, 0xAD, 0xBE, 0xEF];
        let document = decode_nested(&build_nres(&[
            stream(STREAM_NODE_TABLE, 38, b"Res1", &[]),
            stream(STREAM_SLOTS, 0, b"Res2", &slots_payload(&[])),
            stream(STREAM_POSITIONS, 0, b"Res3", &[]),
            stream(STREAM_INDICES, 0, b"Res6", &[]),
            stream(STREAM_BATCHES, 0, b"Res13", &[]),
            stream(STREAM_TYPE_16, 0, b"Res16", &payload),
        ]))
        .expect("nested NRes");
        let model =
            validate_msh(&decode_msh(&document).expect("MSH document")).expect("model asset");

        assert!(model.type16_present);
    }

    #[test]
    fn type7_decodes_optional_records_and_keeps_opaque_words() {
        let mut raw = Vec::new();
        push_u16(&mut raw, 0x3456);
        push_u16(&mut raw, 0x1111);
        push_u16(&mut raw, 0x2222);
        push_u16(&mut raw, 0x3333);
        raw.extend_from_slice(&(-1_i16).to_le_bytes());
        raw.extend_from_slice(&i16::MIN.to_le_bytes());
        raw.extend_from_slice(&i16::MAX.to_le_bytes());
        push_u16(&mut raw, 0x4444);
        let document = decode_nested(&build_nres(&[
            stream(STREAM_NODE_TABLE, 38, b"Res1", &[]),
            stream(STREAM_SLOTS, 0, b"Res2", &slots_payload(&[])),
            stream(STREAM_POSITIONS, 0, b"Res3", &[]),
            stream(STREAM_INDICES, 0, b"Res6", &[]),
            stream(STREAM_BATCHES, 0, b"Res13", &[]),
            stream(STREAM_TRIANGLE_DESCRIPTORS, 0, b"Res7", &raw),
        ]))
        .expect("nested NRes");
        let msh = decode_msh(&document).expect("optional type7 stream");
        let model = validate_msh(&msh).expect("model");

        assert_eq!(
            model.triangle_descriptors,
            Some(vec![TriangleDescriptor {
                tri_flags: 0x3456,
                opaque: [0x1111, 0x2222, 0x3333, 0x4444],
                normal: [-1, i16::MIN, i16::MAX],
            }])
        );
        let preserved = msh.preserved_streams().expect("preserved streams");
        assert_eq!(
            preserved
                .iter()
                .find(|stream| stream.type_id == STREAM_TRIANGLE_DESCRIPTORS)
                .unwrap()
                .bytes
                .as_ref(),
            raw
        );
    }

    #[test]
    fn malformed_or_duplicate_optional_type7_stream_is_an_error() {
        for entries in [
            vec![
                stream(STREAM_NODE_TABLE, 38, b"Res1", &[]),
                stream(STREAM_SLOTS, 0, b"Res2", &slots_payload(&[])),
                stream(STREAM_POSITIONS, 0, b"Res3", &[]),
                stream(STREAM_INDICES, 0, b"Res6", &[]),
                stream(STREAM_BATCHES, 0, b"Res13", &[]),
                stream(STREAM_TRIANGLE_DESCRIPTORS, 0, b"Res7", &[0; 15]),
            ],
            vec![
                stream(STREAM_NODE_TABLE, 38, b"Res1", &[]),
                stream(STREAM_SLOTS, 0, b"Res2", &slots_payload(&[])),
                stream(STREAM_POSITIONS, 0, b"Res3", &[]),
                stream(STREAM_INDICES, 0, b"Res6", &[]),
                stream(STREAM_BATCHES, 0, b"Res13", &[]),
                stream(STREAM_TRIANGLE_DESCRIPTORS, 0, b"Res7a", &[0; 16]),
                stream(STREAM_TRIANGLE_DESCRIPTORS, 0, b"Res7b", &[0; 16]),
            ],
        ] {
            let document = decode_nested(&build_nres(&entries)).expect("nested NRes");
            let msh = decode_msh(&document).expect("required streams");
            assert!(validate_msh(&msh).is_err());
        }
    }

    #[test]
    fn segment_query_uses_native_one_sided_edge_and_endpoint_rules() {
        let model = segment_model(&[(0, 0)], &[0]);
        let hit = query_slot_segment(
            &model,
            NodeId(0),
            SlotId(0),
            [[0.0, 1.0, 0.0], [0.0, -1.0, 0.0]],
            SegmentQueryMasks::default(),
        )
        .expect("forward segment")
        .expect("front-to-back hit");
        assert_eq!(hit.position, [0.0, 0.0, 0.0]);
        assert_eq!(hit.distance_squared, 1.0);
        assert_eq!(hit.vertex_indices, [0, 1, 2]);

        assert!(query_slot_segment(
            &model,
            NodeId(0),
            SlotId(0),
            [[0.0, -1.0, 0.0], [0.0, 1.0, 0.0]],
            SegmentQueryMasks::default(),
        )
        .expect("reverse segment")
        .is_none());
        assert!(query_slot_segment(
            &model,
            NodeId(0),
            SlotId(0),
            [[0.0, 1.0, 0.0], [0.0, 0.0, 0.0]],
            SegmentQueryMasks::default(),
        )
        .expect("endpoint on plane")
        .is_some());
        assert!(query_slot_segment(
            &model,
            NodeId(0),
            SlotId(0),
            [[0.0, 0.0, 0.0], [0.0, -1.0, 0.0]],
            SegmentQueryMasks::default(),
        )
        .expect("start on plane")
        .is_none());
        assert!(query_slot_segment(
            &model,
            NodeId(0),
            SlotId(0),
            [[0.0, 1.0, -1.0], [0.0, -1.0, -1.0]],
            SegmentQueryMasks::default(),
        )
        .expect("segment crossing triangle edge")
        .is_some());

        let two_sided = segment_model(&[(2, 0)], &[0]);
        assert!(query_slot_segment(
            &two_sided,
            NodeId(0),
            SlotId(0),
            [[0.0, -1.0, 0.0], [0.0, 1.0, 0.0]],
            SegmentQueryMasks::default(),
        )
        .expect("two-sided reverse retry")
        .is_some());
    }

    #[test]
    fn segment_query_skips_triangles_with_zero_packed_normals() {
        let mut model = segment_model(&[(0, 0)], &[0]);
        model
            .triangle_descriptors
            .as_mut()
            .expect("type-7 descriptors")[0]
            .normal = [0; 3];

        assert!(query_slot_segment(
            &model,
            NodeId(0),
            SlotId(0),
            [[0.0, 1.0, 0.0], [0.0, -1.0, 0.0]],
            SegmentQueryMasks::default(),
        )
        .expect("zero normal is a missed triangle")
        .is_none());
    }

    #[test]
    fn segment_query_applies_native_masks_and_advances_filtered_descriptors() {
        let mut model = segment_model(&[(0x0200, 1), (0, 1), (0, 1)], &[0, 0x0020, 0x0004]);
        model
            .triangle_descriptors
            .as_mut()
            .expect("type-7 descriptors")
            .insert(
                0,
                TriangleDescriptor {
                    tri_flags: 0,
                    opaque: [0; 4],
                    normal: [0, i16::MAX, 0],
                },
            );
        model.slots[0].tri_start = 1;
        let masks = SegmentQueryMasks {
            node_include: 0x0004,
            node_exclude: 0,
            batch_include: 0x0001_0000,
            batch_exclude: 0x0000_0200,
            triangle_include: 0x0004,
            triangle_exclude: 0x0020,
        };
        let hit = query_slot_segment(
            &model,
            NodeId(0),
            SlotId(0),
            [[0.0, 1.0, 0.0], [0.0, -1.0, 0.0]],
            masks,
        )
        .expect("native masks")
        .expect("third descriptor survives");
        assert_eq!(hit.batch_index, 2);
        assert_eq!(hit.triangle_descriptor_index, 3);
        assert_eq!(hit.vertex_indices, [6, 7, 8]);

        for masks in [
            SegmentQueryMasks {
                node_include: 0x0008,
                ..masks
            },
            SegmentQueryMasks {
                node_exclude: 0x0004,
                ..masks
            },
            SegmentQueryMasks {
                batch_include: 0x0002_0000,
                ..masks
            },
            SegmentQueryMasks {
                triangle_exclude: 0x0004,
                ..masks
            },
        ] {
            assert!(query_slot_segment(
                &model,
                NodeId(0),
                SlotId(0),
                [[0.0, 1.0, 0.0], [0.0, -1.0, 0.0]],
                masks,
            )
            .expect("filtered query")
            .is_none());
        }
    }

    #[test]
    fn segment_query_replaces_equal_distance_hits_with_the_later_candidate() {
        let model = segment_model(&[(0, 0), (0, 0)], &[0, 0]);
        let hit = query_slot_segment(
            &model,
            NodeId(0),
            SlotId(0),
            [[0.0, 1.0, 0.0], [0.0, -1.0, 0.0]],
            SegmentQueryMasks::default(),
        )
        .expect("equal-distance candidates")
        .expect("hit");
        assert_eq!(hit.distance_squared, 1.0);
        assert_eq!(hit.batch_index, 1);
        assert_eq!(hit.triangle_descriptor_index, 1);
    }

    #[test]
    fn native_point_in_triangle_projects_on_each_dominant_normal_axis() {
        for (normal, triangle, edge, outside) in [
            (
                [1.0, 0.0, 0.0],
                [[0.0, -1.0, -1.0], [0.0, 1.0, -1.0], [0.0, 0.0, 1.0]],
                [0.0, 0.0, -1.0],
                [0.0, 2.0, 0.0],
            ),
            (
                [0.0, 1.0, 0.0],
                [[-1.0, 0.0, -1.0], [0.0, 0.0, 1.0], [1.0, 0.0, -1.0]],
                [0.0, 0.0, -1.0],
                [0.0, 0.0, 2.0],
            ),
            (
                [0.0, 0.0, 1.0],
                [[-1.0, -1.0, 0.0], [1.0, -1.0, 0.0], [0.0, 1.0, 0.0]],
                [0.0, -1.0, 0.0],
                [2.0, 0.0, 0.0],
            ),
        ] {
            assert!(native_point_in_triangle(normal, triangle, [0.0; 3]).expect("inside"));
            assert!(native_point_in_triangle(normal, triangle, edge).expect("edge"));
            assert!(!native_point_in_triangle(normal, triangle, outside).expect("outside"));
        }
    }

    #[test]
    fn segment_query_reports_missing_type7_and_invalid_metadata_or_ranges() {
        let mut model = segment_model(&[(0, 0)], &[0]);
        model.triangle_descriptors = None;
        assert!(matches!(
            query_slot_segment(
                &model,
                NodeId(0),
                SlotId(0),
                [[0.0, 1.0, 0.0], [0.0, -1.0, 0.0]],
                SegmentQueryMasks::default(),
            ),
            Err(MshError::MissingStream {
                type_id: STREAM_TRIANGLE_DESCRIPTORS,
                ..
            })
        ));

        let mut model = segment_model(&[(0, 0)], &[0]);
        model.node_stride = 24;
        assert!(matches!(
            query_slot_segment(
                &model,
                NodeId(0),
                SlotId(0),
                [[0.0, 1.0, 0.0], [0.0, -1.0, 0.0]],
                SegmentQueryMasks::default(),
            ),
            Err(MshError::InvalidGeometry(_))
        ));

        let mut model = segment_model(&[(0, 0)], &[0]);
        model.slots[0].tri_start = 1;
        assert!(query_slot_segment(
            &model,
            NodeId(0),
            SlotId(0),
            [[0.0, 1.0, 0.0], [0.0, -1.0, 0.0]],
            SegmentQueryMasks::default(),
        )
        .is_err());

        let mut model = segment_model(&[(0, 0)], &[0]);
        model.slots[0].batch_count = 2;
        assert!(matches!(
            query_slot_segment(
                &model,
                NodeId(0),
                SlotId(0),
                [[0.0, 1.0, 0.0], [0.0, -1.0, 0.0]],
                SegmentQueryMasks::default(),
            ),
            Err(MshError::BatchRangeOutOfBounds { .. })
        ));

        let mut model = segment_model(&[(0, 0)], &[0]);
        model.batches[0].index_count = 4;
        assert!(query_slot_segment(
            &model,
            NodeId(0),
            SlotId(0),
            [[0.0, 1.0, 0.0], [0.0, -1.0, 0.0]],
            SegmentQueryMasks::default(),
        )
        .is_err());

        let mut model = segment_model(&[(0, 0)], &[0]);
        model.batches[0].index_start = 3;
        assert!(query_slot_segment(
            &model,
            NodeId(0),
            SlotId(0),
            [[0.0, 1.0, 0.0], [0.0, -1.0, 0.0]],
            SegmentQueryMasks::default(),
        )
        .is_err());
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
    fn standard_node_exposes_fallback_pose_and_unassigned_link() {
        let mut node = node38([u16::MAX; 15]);
        node[0..2].copy_from_slice(&0x8201_u16.to_le_bytes());
        node[2..4].copy_from_slice(&7_u16.to_le_bytes());
        node[4..6].copy_from_slice(&u16::MAX.to_le_bytes());
        node[6..8].copy_from_slice(&0_u16.to_le_bytes());
        let mut key = Vec::new();
        push_f32(&mut key, 1.0);
        push_f32(&mut key, 2.0);
        push_f32(&mut key, 3.0);
        push_f32(&mut key, 0.0);
        push_u16(&mut key, 0);
        push_u16(&mut key, 0);
        push_u16(&mut key, 0);
        push_u16(&mut key, 32_767);
        let document = decode_nested(&build_nres(&[
            stream(STREAM_NODE_TABLE, 38, b"Res1", &node),
            stream(STREAM_SLOTS, 0, b"Res2", &slots_payload(&[])),
            stream(STREAM_POSITIONS, 0, b"Res3", &[]),
            stream(STREAM_INDICES, 0, b"Res6", &[]),
            stream(STREAM_ANIMATION_KEYS, 0, b"Res8", &key),
            stream(STREAM_BATCHES, 0, b"Res13", &[]),
            stream(STREAM_ANIMATION_FRAME_MAP, 0, b"Res19", &[]),
        ]))
        .expect("nested NRes");
        let model =
            validate_msh(&decode_msh(&document).expect("msh document")).expect("model asset");

        assert_eq!(
            node38_metadata(&model, NodeId(0)),
            Some(Node38Metadata {
                header_flags: 0x8201,
                parent_or_link_raw: 7,
                anim_map_start: u16::MAX,
                fallback_key: 0,
            })
        );
        assert_eq!(
            node38_fallback_pose(&model, NodeId(0)).map(|pose| pose.translation),
            Some([1.0, 2.0, 3.0])
        );
        assert_eq!(
            model
                .animation
                .as_ref()
                .map(|animation| animation.keys.len()),
            Some(1)
        );
    }

    #[test]
    fn standard_nodes_evaluate_fallback_parent_hierarchy() {
        let mut root = node38([u16::MAX; 15]);
        root[2..4].copy_from_slice(&u16::MAX.to_le_bytes());
        root[6..8].copy_from_slice(&0_u16.to_le_bytes());
        let mut child = node38([u16::MAX; 15]);
        child[2..4].copy_from_slice(&0_u16.to_le_bytes());
        child[6..8].copy_from_slice(&1_u16.to_le_bytes());
        let mut nodes = root;
        nodes.extend(child);
        let mut keys = Vec::new();
        // Type 8 stores WXYZ, which `AnimKey24::decode` reorders to XYZW.
        // The native Node38 local-basis conversion happens before hierarchy
        // evaluation, not in the generic animation decoder.
        for (x, y, z, qw, qz) in [
            (1.0, 0.0, 0.0, 23_170_i16, 23_170_i16),
            (2.0, 0.0, 0.0, 32_767_i16, 0_i16),
        ] {
            push_f32(&mut keys, x);
            push_f32(&mut keys, y);
            push_f32(&mut keys, z);
            push_f32(&mut keys, 0.0);
            push_u16(&mut keys, qw.cast_unsigned());
            push_u16(&mut keys, 0);
            push_u16(&mut keys, 0);
            push_u16(&mut keys, qz.cast_unsigned());
        }
        let document = decode_nested(&build_nres(&[
            stream(STREAM_NODE_TABLE, 38, b"Res1", &nodes),
            stream(STREAM_SLOTS, 0, b"Res2", &slots_payload(&[])),
            stream(STREAM_POSITIONS, 0, b"Res3", &[]),
            stream(STREAM_INDICES, 0, b"Res6", &[]),
            stream(STREAM_ANIMATION_KEYS, 0, b"Res8", &keys),
            stream(STREAM_BATCHES, 0, b"Res13", &[]),
            stream(STREAM_ANIMATION_FRAME_MAP, 0, b"Res19", &[]),
        ]))
        .expect("nested NRes");
        let model =
            validate_msh(&decode_msh(&document).expect("msh document")).expect("model asset");
        let hierarchy = node38_fallback_hierarchy(&model).expect("valid node hierarchy");
        assert!((hierarchy.poses[1].translation[0] - 1.0).abs() < 0.001);
        assert!((hierarchy.poses[1].translation[1] + 2.0).abs() < 0.001);
    }

    #[test]
    fn node38_native_x_rotation_moves_child_translation_for_fallback_and_sampled_paths() {
        let mut root = node38([u16::MAX; 15]);
        root[2..4].copy_from_slice(&u16::MAX.to_le_bytes());
        root[4..6].copy_from_slice(&0_u16.to_le_bytes());
        root[6..8].copy_from_slice(&1_u16.to_le_bytes());
        let mut child = node38([u16::MAX; 15]);
        child[2..4].copy_from_slice(&0_u16.to_le_bytes());
        child[4..6].copy_from_slice(&u16::MAX.to_le_bytes());
        child[6..8].copy_from_slice(&2_u16.to_le_bytes());
        let mut nodes = root;
        nodes.extend(child);
        let half_sqrt_two = std::f32::consts::FRAC_1_SQRT_2;
        let root_rotation = [half_sqrt_two, 0.0, 0.0, half_sqrt_two];
        let identity = [0.0, 0.0, 0.0, 1.0];
        let child_translation = [0.0, -1.01209, -0.001792];
        let model = ModelAsset {
            node_stride: 38,
            node_count: 2,
            nodes_raw: nodes,
            bounding_sphere_center: [0.0; 3],
            bounding_sphere_radius: 1.0,
            slots: Vec::new(),
            positions: Vec::new(),
            normals: None,
            uv0: None,
            uv1: None,
            type16_present: false,
            indices: Vec::new(),
            batches: Vec::new(),
            triangle_descriptors: None,
            node_names: None,
            animation: Some(ModelAnimation {
                keys: vec![
                    AnimKey24 {
                        time: AnimationTime(0.0),
                        pose: Pose {
                            translation: [0.0; 3],
                            rotation: root_rotation,
                        },
                    },
                    AnimKey24 {
                        time: AnimationTime(1.0),
                        pose: Pose {
                            translation: [0.0; 3],
                            rotation: root_rotation,
                        },
                    },
                    AnimKey24 {
                        time: AnimationTime(0.0),
                        pose: Pose {
                            translation: child_translation,
                            rotation: identity,
                        },
                    },
                ],
                frame_map: vec![0],
                frame_count: 1,
            }),
        };

        let fallback_root = node38_fallback_pose(&model, NodeId(0)).expect("fallback root pose");
        assert!(fallback_root.rotation[0] < -0.7);
        let fallback = node38_fallback_hierarchy(&model).expect("fallback hierarchy");
        let sampled = node38_sampled_hierarchy(&model, 0).expect("sampled hierarchy");
        for hierarchy in [&fallback, &sampled] {
            let child_translation = hierarchy.poses[1].translation;
            assert!(child_translation[0].abs() < 0.0001);
            assert!((child_translation[1] + 0.001792).abs() < 0.0001);
            assert!((child_translation[2] - 1.01209).abs() < 0.0001);
        }
    }

    #[test]
    fn standard_nodes_sample_mapped_key_before_fallback_key() {
        let mut node = node38([u16::MAX; 15]);
        node[2..4].copy_from_slice(&u16::MAX.to_le_bytes());
        node[4..6].copy_from_slice(&0_u16.to_le_bytes());
        node[6..8].copy_from_slice(&2_u16.to_le_bytes());
        let model = ModelAsset {
            node_stride: 38,
            node_count: 1,
            nodes_raw: node,
            bounding_sphere_center: [0.0; 3],
            bounding_sphere_radius: 1.0,
            slots: Vec::new(),
            positions: Vec::new(),
            normals: None,
            uv0: None,
            uv1: None,
            type16_present: false,
            indices: Vec::new(),
            batches: Vec::new(),
            triangle_descriptors: None,
            node_names: None,
            animation: Some(ModelAnimation {
                keys: vec![
                    AnimKey24 {
                        time: AnimationTime(0.0),
                        pose: Pose {
                            translation: [4.0, 0.0, 0.0],
                            rotation: [0.0, 0.0, 0.0, 1.0],
                        },
                    },
                    AnimKey24 {
                        time: AnimationTime(2.0),
                        pose: Pose {
                            translation: [8.0, 0.0, 0.0],
                            rotation: [0.0, 0.0, 0.0, 1.0],
                        },
                    },
                    AnimKey24 {
                        time: AnimationTime(9.0),
                        pose: Pose::default(),
                    },
                ],
                frame_map: vec![0, 0, 0],
                frame_count: 3,
            }),
        };

        let pose = node38_sampled_hierarchy(&model, 1)
            .expect("mapped hierarchy")
            .poses[0];
        assert!((pose.translation[0] - 6.0).abs() < f32::EPSILON);
        assert_eq!(
            node38_sampled_hierarchy(&model, 9)
                .expect("fallback hierarchy")
                .poses[0],
            Pose::default()
        );
    }

    #[test]
    fn standard_nodes_sample_independent_fractional_times_and_selector_rounding() {
        let mut root = node38([u16::MAX; 15]);
        root[2..4].copy_from_slice(&u16::MAX.to_le_bytes());
        root[4..6].copy_from_slice(&0_u16.to_le_bytes());
        root[6..8].copy_from_slice(&4_u16.to_le_bytes());
        let mut child = node38([u16::MAX; 15]);
        child[2..4].copy_from_slice(&0_u16.to_le_bytes());
        child[4..6].copy_from_slice(&2_u16.to_le_bytes());
        child[6..8].copy_from_slice(&5_u16.to_le_bytes());
        let mut nodes = root;
        nodes.extend(child);
        let model = ModelAsset {
            node_stride: 38,
            node_count: 2,
            nodes_raw: nodes,
            bounding_sphere_center: [0.0; 3],
            bounding_sphere_radius: 1.0,
            slots: Vec::new(),
            positions: Vec::new(),
            normals: None,
            uv0: None,
            uv1: None,
            type16_present: false,
            indices: Vec::new(),
            batches: Vec::new(),
            triangle_descriptors: None,
            node_names: None,
            animation: Some(ModelAnimation {
                keys: vec![
                    AnimKey24 {
                        time: AnimationTime(0.0),
                        pose: Pose {
                            translation: [4.0, 0.0, 0.0],
                            rotation: [0.0, 0.0, 0.0, 1.0],
                        },
                    },
                    AnimKey24 {
                        time: AnimationTime(2.0),
                        pose: Pose {
                            translation: [8.0, 0.0, 0.0],
                            rotation: [0.0, 0.0, 0.0, 1.0],
                        },
                    },
                    AnimKey24 {
                        time: AnimationTime(1.0),
                        pose: Pose {
                            translation: [20.0, 0.0, 0.0],
                            rotation: [0.0, 0.0, 0.0, 1.0],
                        },
                    },
                    AnimKey24 {
                        time: AnimationTime(2.0),
                        pose: Pose {
                            translation: [30.0, 0.0, 0.0],
                            rotation: [0.0, 0.0, 0.0, 1.0],
                        },
                    },
                    AnimKey24 {
                        time: AnimationTime(9.0),
                        pose: Pose {
                            translation: [100.0, 0.0, 0.0],
                            rotation: [0.0, 0.0, 0.0, 1.0],
                        },
                    },
                    AnimKey24 {
                        time: AnimationTime(9.0),
                        pose: Pose {
                            translation: [300.0, 0.0, 0.0],
                            rotation: [0.0, 0.0, 0.0, 1.0],
                        },
                    },
                ],
                frame_map: vec![0, 1, 0, 2],
                frame_count: 4,
            }),
        };

        let hierarchy = node38_sampled_hierarchy_at_times(
            &model,
            &[Some(AnimationTime(1.0)), Some(AnimationTime(1.55))],
        )
        .expect("per-node fractional hierarchy");
        // The root's tie selects map frame zero and interpolates to x=6. The
        // child independently selects frame one and samples its own 1.55 time;
        // its global pose includes the root translation.
        assert!((hierarchy.poses[0].translation[0] - 6.0).abs() < f32::EPSILON);
        assert!((hierarchy.poses[1].translation[0] - 31.5).abs() < 0.001);
        let fallback = node38_sampled_hierarchy_at_times(&model, &[Some(AnimationTime(1.0)), None])
            .expect("per-node fallback hierarchy");
        assert_eq!(fallback.poses[1].translation[0], 306.0);
        assert!(node38_sampled_hierarchy_at_times(&model, &[Some(AnimationTime(1.0))]).is_none());
        for invalid in [-1.0, f32::NAN, f32::INFINITY] {
            assert!(node38_sampled_hierarchy_at_times(
                &model,
                &[Some(AnimationTime(invalid)), None]
            )
            .is_none());
        }
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
    fn type2_header_bounding_sphere_is_preserved_and_rejects_invalid_values() {
        let valid_slots = slots_payload_with_bounds([1.25, -2.5, 3.75], 4.5, &[]);
        let document = decode_nested(&build_nres(&[
            stream(STREAM_NODE_TABLE, 38, b"Res1", &[]),
            stream(STREAM_SLOTS, 0, b"Res2", &valid_slots),
            stream(STREAM_POSITIONS, 0, b"Res3", &[]),
            stream(STREAM_INDICES, 0, b"Res6", &[]),
            stream(STREAM_BATCHES, 0, b"Res13", &[]),
        ]))
        .expect("nested");
        let model = validate_msh(&decode_msh(&document).expect("msh")).expect("model");
        assert_eq!(model.bounding_sphere_center, [1.25, -2.5, 3.75]);
        assert_eq!(model.bounding_sphere_radius, 4.5);

        for (offset, value) in [
            (0x60, f32::NAN),
            (0x68, f32::INFINITY),
            (0x6C, f32::NAN),
            (0x6C, -1.0),
        ] {
            let mut slots = slots_payload(&[]);
            slots[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
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
                Err(MshError::InvalidGeometry(_))
            ));
        }
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
            (STREAM_UV1, b"Res18".as_slice(), vec![0; 3]),
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
    fn type18_secondary_uv_decodes_unsigned_pairs_for_each_source_vertex() {
        let positions = positions_payload(&[[0.0, 0.0, 0.0]; 4]);
        let mut uv1 = Vec::new();
        for pair in [[2_u16, 5_u16], [12, 8], [16, 30], [34, 2]] {
            push_u16(&mut uv1, pair[0]);
            push_u16(&mut uv1, pair[1]);
        }
        let document = decode_nested(&build_nres(&[
            stream(STREAM_NODE_TABLE, 38, b"Res1", &[]),
            stream(STREAM_SLOTS, 0, b"Res2", &slots_payload(&[])),
            stream(STREAM_POSITIONS, 12, b"Res3", &positions),
            stream(STREAM_INDICES, 0, b"Res6", &[]),
            stream(STREAM_BATCHES, 0, b"Res13", &[]),
            stream_with_attr1(STREAM_UV1, 4, 4, b"Res18", &uv1),
        ]))
        .expect("nested");
        let model = validate_msh(&decode_msh(&document).expect("msh")).expect("model");

        assert_eq!(model.positions.len(), 4);
        assert_eq!(model.uv1, Some(vec![[2, 5], [12, 8], [16, 30], [34, 2]]));
    }

    #[test]
    fn type18_secondary_uv_rejects_untrusted_count_or_stride() {
        let positions = positions_payload(&[[0.0, 0.0, 0.0]; 1]);
        for (attr1, attr3, expected) in [(1, 8, "stride"), (2, 4, "count")] {
            let mut uv1 = Vec::new();
            push_u16(&mut uv1, 2);
            push_u16(&mut uv1, 5);
            let document = decode_nested(&build_nres(&[
                stream(STREAM_NODE_TABLE, 38, b"Res1", &[]),
                stream(STREAM_SLOTS, 0, b"Res2", &slots_payload(&[])),
                stream(STREAM_POSITIONS, 12, b"Res3", &positions),
                stream(STREAM_INDICES, 0, b"Res6", &[]),
                stream(STREAM_BATCHES, 0, b"Res13", &[]),
                stream_with_attr1(STREAM_UV1, attr1, attr3, b"Res18", &uv1),
            ]))
            .expect("nested");
            let err = validate_msh(&decode_msh(&document).expect("msh")).expect_err("invalid");
            let message = err.to_string();
            assert!(message.contains(expected), "{message}");
        }
    }

    #[test]
    fn installed_gog_secondary_uv_fixture_is_aligned_and_real() {
        let Some(root) = std::env::var_os("FPARKAN_GAME_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        assert!(
            root.is_dir(),
            "FPARKAN_GAME_ROOT is missing: {}",
            root.display()
        );

        let mut models_with_uv1 = 0usize;
        let mut fixture_found = false;
        for path in files_under(&root) {
            if !path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("rlb"))
            {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let Ok(archive) =
                fparkan_nres::decode(Arc::from(bytes.into_boxed_slice()), ReadProfile::Compatible)
            else {
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
                .unwrap_or_else(|err| panic!("{path:?} {:?}: {err}", entry.name_bytes()));
                let msh = decode_msh(&nested)
                    .unwrap_or_else(|err| panic!("{path:?} {:?}: {err}", entry.name_bytes()));
                let model = validate_msh(&msh)
                    .unwrap_or_else(|err| panic!("{path:?} {:?}: {err}", entry.name_bytes()));
                let Some(uv1) = model.uv1.as_ref() else {
                    continue;
                };
                models_with_uv1 += 1;
                assert_eq!(
                    uv1.len(),
                    model.positions.len(),
                    "{path:?} {:?}",
                    entry.name_bytes()
                );
                let descriptor = msh
                    .streams()
                    .iter()
                    .find(|stream| stream.type_id == STREAM_UV1)
                    .expect("Res18 descriptor");
                assert_eq!(descriptor.attributes.attr1 as usize, uv1.len());
                assert_eq!(descriptor.attributes.attr3, 4);

                if entry.name_bytes().eq_ignore_ascii_case(b"fr_l_gener.msh") {
                    assert_eq!(&uv1[..4], &[[2, 5], [12, 8], [16, 30], [34, 2]]);
                    fixture_found = true;
                }
            }
        }

        assert!(models_with_uv1 > 0, "FPARKAN_GAME_ROOT has no type18 MSH");
        assert!(
            fixture_found,
            "FPARKAN_GAME_ROOT lacks fr_l_gener.msh fixture"
        );
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
        assert_eq!(model.batches[0].material_index_hi, 0x2200);
        assert_eq!(model.batches[0].material_index, 0x00);
        assert_eq!(model.batches[0].lightmap_index, 0x33);
        assert_eq!(model.batches[0].local_batch_index, 0x4400);
        assert_eq!(model.batches[0].index_count, 1);
        assert_eq!(model.batches[0].index_start, 0);
        assert_eq!(model.batches[0].vertex_count, 0x5500);
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
    fn licensed_corpus_part1_msh_assets_validate() {
        validate_licensed_corpus_msh_assets("IS", 435);
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_corpus_part2_msh_assets_validate() {
        validate_licensed_corpus_msh_assets("IS2", 511);
    }

    fn validate_licensed_corpus_msh_assets(corpus: &str, expected: usize) {
        let root = corpus_root(corpus);
        let mut count = 0usize;
        for path in files_under(&root) {
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let Ok(archive) =
                fparkan_nres::decode(Arc::from(bytes.into_boxed_slice()), ReadProfile::Compatible)
            else {
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
                .unwrap_or_else(|err| panic!("{corpus} {path:?} {:?}: {err}", entry.name_bytes()));
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
                    if model.node_stride == 38 {
                        assert!(
                            node38_fallback_hierarchy(&model).is_some(),
                            "{corpus} {path:?} {:?}: Node38 hierarchy is not parent-before-child",
                            entry.name_bytes()
                        );
                    }
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

    #[test]
    #[ignore = "requires licensed corpus"]
    fn factory_wall_door_and_reversed_segments_match_native_query() {
        let root = corpus_root("IS");
        let archive_path = files_under(&root)
            .into_iter()
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.eq_ignore_ascii_case("fortif.rlb"))
            })
            .expect("fortif.rlb in licensed corpus");
        let archive_bytes = std::fs::read(&archive_path).expect("fortif.rlb bytes");
        let archive = fparkan_nres::decode(
            Arc::from(archive_bytes.into_boxed_slice()),
            ReadProfile::Compatible,
        )
        .expect("fortif.rlb archive");
        let entry = archive
            .entries()
            .iter()
            .find(|entry| entry.name_bytes().eq_ignore_ascii_case(b"fr_l_plant.msh"))
            .expect("plant factory MSH");
        let nested = fparkan_nres::decode(
            Arc::from(
                archive
                    .payload(entry.id())
                    .expect("MSH payload")
                    .to_vec()
                    .into_boxed_slice(),
            ),
            ReadProfile::Compatible,
        )
        .expect("nested plant MSH");
        let msh = decode_msh(&nested).expect("plant MSH document");
        let model = validate_msh(&msh).expect("plant MSH model");
        let slot = selected_slot(&model, NodeId(1), Lod(0), Group(0)).expect("Node 1 slot");
        assert_eq!(slot, SlotId(0));
        let hierarchy = node38_fallback_hierarchy(&model).expect("Node38 poses");
        let pose = hierarchy.poses[1];
        assert_eq!(pose.rotation, [0.0, 0.0, 0.0, 1.0]);

        let masks = SegmentQueryMasks {
            node_include: 0,
            node_exclude: 0,
            batch_include: 0,
            batch_exclude: 0x0208,
            triangle_include: 0,
            triangle_exclude: 0x0024,
        };
        let local = |point: [f32; 3]| {
            [
                point[0] - pose.translation[0],
                point[1] - pose.translation[1],
                point[2] - pose.translation[2],
            ]
        };
        let wall = query_slot_segment(
            &model,
            NodeId(1),
            slot,
            [local([-10.5, 22.16, 2.5]), local([-10.5, 19.16, 2.5])],
            masks,
        )
        .expect("wall query")
        .expect("solid wall hit");
        assert_eq!(wall.batch_index, 5);
        assert_eq!(wall.triangle_descriptor_index, 62);
        assert_eq!(wall.distance_squared, 4.0);
        assert!(query_slot_segment(
            &model,
            NodeId(1),
            slot,
            [local([0.5, 22.16, 2.5]), local([0.5, 19.16, 2.5])],
            masks,
        )
        .expect("door query")
        .is_none());
        assert!(query_slot_segment(
            &model,
            NodeId(1),
            slot,
            [local([-10.5, 19.16, 2.5]), local([-10.5, 22.16, 2.5])],
            masks,
        )
        .expect("reversed wall query")
        .is_none());
    }

    fn segment_model(batch_fields: &[(u16, u16)], tri_flags: &[u16]) -> ModelAsset {
        assert_eq!(batch_fields.len(), tri_flags.len());
        let mut nodes_raw = node38([u16::MAX; 15]);
        nodes_raw[0..2].copy_from_slice(&0x0004_u16.to_le_bytes());
        let mut positions = Vec::with_capacity(batch_fields.len() * 3);
        let mut indices = Vec::with_capacity(batch_fields.len() * 3);
        let mut batches = Vec::with_capacity(batch_fields.len());
        for index in 0..batch_fields.len() {
            positions.extend_from_slice(&[[-1.0, 0.0, -1.0], [0.0, 0.0, 1.0], [1.0, 0.0, -1.0]]);
            indices.extend_from_slice(&[0, 1, 2]);
            batches.push(Batch {
                batch_flags: batch_fields[index].0,
                material_index_hi: batch_fields[index].1,
                material_index: 0,
                lightmap_index: 0,
                local_batch_index: u16::try_from(index).expect("local batch index"),
                index_count: 3,
                index_start: u32::try_from(index * 3).expect("index start"),
                vertex_count: 3,
                base_vertex: u32::try_from(index * 3).expect("base vertex"),
            });
        }
        ModelAsset {
            node_stride: 38,
            node_count: 1,
            nodes_raw,
            bounding_sphere_center: [0.0; 3],
            bounding_sphere_radius: 1.0,
            slots: vec![Slot {
                tri_start: 0,
                tri_count: u16::try_from(tri_flags.len()).expect("triangle count"),
                batch_start: 0,
                batch_count: u16::try_from(batch_fields.len()).expect("batch count"),
                aabb_min: [-1.0, 0.0, -1.0],
                aabb_max: [1.0, 0.0, 1.0],
                sphere_center: [0.0; 3],
                sphere_radius: 1.0,
                opaque: [0; 5],
            }],
            positions,
            normals: None,
            uv0: None,
            uv1: None,
            type16_present: false,
            indices,
            batches,
            node_names: None,
            animation: None,
            triangle_descriptors: Some(
                tri_flags
                    .iter()
                    .map(|tri_flags| TriangleDescriptor {
                        tri_flags: *tri_flags,
                        opaque: [0; 4],
                        normal: [0, i16::MAX, 0],
                    })
                    .collect(),
            ),
        }
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
            attr1: 0,
            attr3,
            name,
            payload,
        }
    }

    fn stream_with_attr1<'a>(
        type_id: u32,
        attr1: u32,
        attr3: u32,
        name: &'a [u8],
        payload: &'a [u8],
    ) -> TestEntry<'a> {
        TestEntry {
            type_id,
            attr1,
            attr3,
            name,
            payload,
        }
    }

    struct TestEntry<'a> {
        type_id: u32,
        attr1: u32,
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
            push_u32(&mut out, entry.attr1);
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
        slots_payload_with_bounds([0.0; 3], 1.0, records)
    }

    fn slots_payload_with_bounds(center: [f32; 3], radius: f32, records: &[Vec<u8>]) -> Vec<u8> {
        let mut out = vec![0; 0x8c];
        for (offset, value) in [
            (0x60, center[0]),
            (0x64, center[1]),
            (0x68, center[2]),
            (0x6C, radius),
        ] {
            out[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
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
