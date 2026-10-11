//! Format-to-GPU geometry bridge for the initial static asset renderer.

use crate::{VulkanStaticDrawRange, VulkanStaticMesh, VulkanStaticVertex};
use fparkan_animation::{NodePoseBuffer, Pose};
use fparkan_msh::{
    draw_batches, node38_fallback_hierarchy, node38_sampled_hierarchy, selected_slot, Group, Lod,
    ModelAsset, NodeId,
};
use fparkan_render::{LegacyDepthMode, LegacyIron3dEulerTransform, LegacyPipelineState};
use fparkan_terrain_format::LandMeshDocument;

/// Legacy `Land.msh` stored-height to mission-world-height scale.
const LEGACY_LAND_HEIGHT_SCALE: f32 = 1.0 / 32.0;

fn world_pipeline_state() -> LegacyPipelineState {
    LegacyPipelineState {
        depth: LegacyDepthMode::TestWrite,
        ..LegacyPipelineState::default()
    }
}

/// Error returned when a validated MSH cannot enter the current static GPU path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VulkanAssetMeshError {
    /// The model has no triangle batches.
    EmptyGeometry,
    /// A position used by a batch is non-finite.
    NonFinitePosition,
    /// The view-plane extent is zero or otherwise not usable for normalization.
    DegenerateViewExtent,
    /// The indexed model exceeds the current 16-bit Vulkan input contract.
    IndexOutOfRange,
    /// A caller-supplied node pose buffer has a different length than its model.
    NodePoseCountMismatch {
        /// Number of nodes in the model.
        expected: usize,
        /// Number of supplied node poses.
        actual: usize,
    },
}

/// Evaluated world pose of one standard Node38 node in model-local space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VulkanNodePose {
    /// Translation in the model's local coordinate system.
    pub translation: [f32; 3],
    /// Unit quaternion in `[x, y, z, w]` order.
    pub rotation: [f32; 4],
}

/// Shared XY frame for a deliberately top-down diagnostic static scene.
///
/// It is a CPU-side viewer transform, not evidence of the original camera or
/// object transform convention. Keeping it explicit prevents separately
/// normalized terrain and model components from being incorrectly overlaid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VulkanStaticXyFrame {
    center_x: f32,
    center_y: f32,
    extent: f32,
}

impl VulkanStaticXyFrame {
    /// Builds a frame that maps the supplied XY bounds into the static viewer.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanAssetMeshError::DegenerateViewExtent`] for non-finite
    /// or zero-sized bounds.
    pub fn from_bounds(
        min_x: f32,
        max_x: f32,
        min_y: f32,
        max_y: f32,
    ) -> Result<Self, VulkanAssetMeshError> {
        let extent = (max_x - min_x).max(max_y - min_y);
        if !extent.is_finite() || extent <= f32::EPSILON {
            return Err(VulkanAssetMeshError::DegenerateViewExtent);
        }
        Ok(Self {
            center_x: (min_x + max_x) * 0.5,
            center_y: (min_y + max_y) * 0.5,
            extent,
        })
    }

    fn project(self, position: [f32; 3]) -> [f32; 2] {
        let scale = 1.6 / self.extent;
        [
            (position[0] - self.center_x) * scale,
            (position[1] - self.center_y) * scale,
        ]
    }

    fn planar_uv(self, position: [f32; 3]) -> [f32; 2] {
        [
            (position[0] - self.center_x) / self.extent + 0.5,
            (position[1] - self.center_y) / self.extent + 0.5,
        ]
    }
}

impl std::fmt::Display for VulkanAssetMeshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyGeometry => write!(f, "MSH contains no triangle geometry"),
            Self::NonFinitePosition => write!(f, "MSH contains a non-finite position"),
            Self::DegenerateViewExtent => write!(f, "MSH has a degenerate XY view extent"),
            Self::IndexOutOfRange => write!(f, "MSH index exceeds the static Vulkan u16 contract"),
            Self::NodePoseCountMismatch { expected, actual } => write!(
                f,
                "node pose buffer has {actual} poses, but the MSH has {expected} nodes"
            ),
        }
    }
}

impl std::error::Error for VulkanAssetMeshError {}

/// Projects validated MSH geometry into the current static Vulkan clip-space path.
///
/// The original engine's node transforms, camera and material pipeline are not
/// substituted here. This bridge is intentionally a static asset-viewer step:
/// it preserves every batch's `index_start`, `index_count` and `base_vertex`,
/// projects the conventional `Iron3D` XY ground plane into the current XY shader
/// input, and scales the used bounds uniformly into the visible clip rectangle.
///
/// # Errors
///
/// Returns [`VulkanAssetMeshError`] if the source cannot be represented by the
/// current 16-bit, triangle-list viewer input.
pub fn project_msh_to_static_mesh(
    model: &ModelAsset,
) -> Result<VulkanStaticMesh, VulkanAssetMeshError> {
    let (indices, _) = static_model_indices_and_ranges(model)?;
    let (min_x, max_x, min_y, max_y) = static_mesh_xy_bounds(&model.positions, &indices)?;
    let frame = VulkanStaticXyFrame::from_bounds(min_x, max_x, min_y, max_y)?;
    project_msh_to_static_mesh_in_xy_frame(model, frame, [0.0; 3], [1.0; 3])
}

/// Projects MSH geometry with a known translation/scale into a shared XY frame.
///
/// The caller supplies only transforms already decoded from mission data. Raw
/// orientation is intentionally not interpreted until its original convention
/// is established.
///
/// # Errors
///
/// Returns [`VulkanAssetMeshError`] when geometry, transform values or the
/// static Vulkan input contract cannot represent the source model.
pub fn project_msh_to_static_mesh_in_xy_frame(
    model: &ModelAsset,
    frame: VulkanStaticXyFrame,
    translation: [f32; 3],
    scale: [f32; 3],
) -> Result<VulkanStaticMesh, VulkanAssetMeshError> {
    if !translation
        .iter()
        .chain(scale.iter())
        .all(|value| value.is_finite())
    {
        return Err(VulkanAssetMeshError::NonFinitePosition);
    }
    let (indices, draw_ranges) = static_model_indices_and_ranges(model)?;
    let vertices = model
        .positions
        .iter()
        .enumerate()
        .map(|(index, position)| {
            if !position.iter().all(|value| value.is_finite()) {
                return Err(VulkanAssetMeshError::NonFinitePosition);
            }
            let transformed = [
                position[0] * scale[0] + translation[0],
                position[1] * scale[1] + translation[1],
                position[2] * scale[2] + translation[2],
            ];
            Ok(VulkanStaticVertex {
                position: [
                    frame.project(transformed)[0],
                    frame.project(transformed)[1],
                    0.0,
                ],
                color: [1.0, 1.0, 1.0],
                normal: model_normal(model, index),
                // Iron3D stores Res5 UV0 as unsigned fixed point with 1/1024 units.
                // Models that omit this optional stream retain the static viewer's
                // XY planar fallback instead of receiving fabricated raw UV values.
                uv: model
                    .uv0
                    .as_ref()
                    .and_then(|uv0| uv0.get(index))
                    .map_or(frame.planar_uv(transformed), |uv| {
                        [f32::from(uv[0]) / 1024.0, f32::from(uv[1]) / 1024.0]
                    }),
                detail_uv: model_detail_uv(model, index),
                overlay_alpha: 0.0,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(VulkanStaticMesh {
        vertices,
        indices,
        draw_ranges,
    })
}

/// Projects a static MSH preview through the recovered placement transform and
/// then into a shared diagnostic XY frame.
///
/// This is the diagnostic-camera counterpart of
/// [`project_msh_to_static_mesh_in_world_space_with_node_fallback_poses`]. It
/// deliberately keeps the diagnostic camera itself separate from the original
/// D3D7 camera, while ensuring that mission placement uses the same proven
/// `Rz(z) * Ry(y) * Rx(x)` convention in both preview paths.
///
/// # Errors
///
/// Returns [`VulkanAssetMeshError`] when the model cannot be represented by
/// the static bridge or a transformed coordinate is non-finite.
pub fn project_msh_to_static_mesh_in_xy_frame_with_node_fallback_poses(
    model: &ModelAsset,
    frame: VulkanStaticXyFrame,
    transform: LegacyIron3dEulerTransform,
    scale: [f32; 3],
) -> Result<VulkanStaticMesh, VulkanAssetMeshError> {
    let mut mesh = project_msh_to_static_mesh_in_world_space_with_node_fallback_poses(
        model, transform, scale,
    )?;
    for vertex in &mut mesh.vertices {
        if !vertex.position.iter().all(|value| value.is_finite()) {
            return Err(VulkanAssetMeshError::NonFinitePosition);
        }
        let projected = frame.project(vertex.position);
        vertex.position = [projected[0], projected[1], 0.0];
    }
    Ok(mesh)
}

/// Projects a standard `Node38` model at one explicit portable-reference
/// animation frame into a diagnostic XY frame.
///
/// This is the diagnostic-camera counterpart of
/// [`project_msh_to_static_mesh_in_world_space_with_node_sampled_poses`].
///
/// # Errors
///
/// Returns [`VulkanAssetMeshError`] when the source cannot be represented by
/// the static bridge or a transformed coordinate is non-finite.
pub fn project_msh_to_static_mesh_in_xy_frame_with_node_sampled_poses(
    model: &ModelAsset,
    frame: VulkanStaticXyFrame,
    transform: LegacyIron3dEulerTransform,
    scale: [f32; 3],
    animation_frame: u16,
) -> Result<VulkanStaticMesh, VulkanAssetMeshError> {
    let mut mesh = project_msh_to_static_mesh_in_world_space_with_node_sampled_poses(
        model,
        transform,
        scale,
        animation_frame,
    )?;
    for vertex in &mut mesh.vertices {
        if !vertex.position.iter().all(|value| value.is_finite()) {
            return Err(VulkanAssetMeshError::NonFinitePosition);
        }
        let projected = frame.project(vertex.position);
        vertex.position = [projected[0], projected[1], 0.0];
    }
    Ok(mesh)
}

/// Projects MSH geometry into source world coordinates with known translation and scale.
///
/// Unlike [`project_msh_to_static_mesh_in_xy_frame`], this does not normalize
/// or discard Z. It retains the decoded translation/scale for the recovered
/// legacy camera path while intentionally leaving raw object orientation
/// uninterpreted until its original convention is established.
///
/// # Errors
///
/// Returns [`VulkanAssetMeshError`] when geometry, transform values or the
/// static Vulkan input contract cannot represent the source model.
pub fn project_msh_to_static_mesh_in_world_space(
    model: &ModelAsset,
    translation: [f32; 3],
    scale: [f32; 3],
) -> Result<VulkanStaticMesh, VulkanAssetMeshError> {
    project_msh_to_static_mesh_in_world_space_with_transform(
        model,
        LegacyIron3dEulerTransform {
            translation,
            orientation_radians: [0.0; 3],
        },
        scale,
    )
}

/// Projects MSH geometry using the recovered `Iron3D` placement transform.
///
/// This preserves source-world coordinates and applies the proven
/// `Rz(z) * Ry(y) * Rx(x)` rotation after local component-wise scale. It is
/// intended for the opt-in legacy-camera static preview, not yet for animated
/// or gameplay-owned transforms.
///
/// # Errors
///
/// Returns [`VulkanAssetMeshError`] when geometry or transform values cannot
/// be represented by the static Vulkan input contract.
pub fn project_msh_to_static_mesh_in_world_space_with_transform(
    model: &ModelAsset,
    transform: LegacyIron3dEulerTransform,
    scale: [f32; 3],
) -> Result<VulkanStaticMesh, VulkanAssetMeshError> {
    project_msh_to_static_mesh_in_world_space_with_transform_and_root_pose(
        model, transform, scale, None,
    )
}

fn project_msh_to_static_mesh_in_world_space_with_transform_and_root_pose(
    model: &ModelAsset,
    transform: LegacyIron3dEulerTransform,
    scale: [f32; 3],
    root_pose: Option<Pose>,
) -> Result<VulkanStaticMesh, VulkanAssetMeshError> {
    if !transform
        .translation
        .iter()
        .chain(transform.orientation_radians.iter())
        .chain(scale.iter())
        .all(|value| value.is_finite())
    {
        return Err(VulkanAssetMeshError::NonFinitePosition);
    }
    let (indices, draw_ranges) = static_model_indices_and_ranges(model)?;
    let vertices = model
        .positions
        .iter()
        .enumerate()
        .map(|(index, position)| {
            if !position.iter().all(|value| value.is_finite()) {
                return Err(VulkanAssetMeshError::NonFinitePosition);
            }
            let local_position = root_pose.map_or(
                Pose {
                    translation: *position,
                    rotation: [0.0, 0.0, 0.0, 1.0],
                },
                |root| {
                    compose_pose(
                        root,
                        Pose {
                            translation: *position,
                            rotation: [0.0, 0.0, 0.0, 1.0],
                        },
                    )
                },
            );
            let position = transform
                .try_transform_scaled_point(local_position.translation, scale)
                .ok_or(VulkanAssetMeshError::NonFinitePosition)?;
            Ok(VulkanStaticVertex {
                position,
                color: [1.0, 1.0, 1.0],
                normal: transform_normal(
                    transform,
                    root_pose.map_or(model_normal(model, index), |root| {
                        rotate_by_quaternion(model_normal(model, index), root.rotation)
                    }),
                    scale,
                )
                .ok_or(VulkanAssetMeshError::NonFinitePosition)?,
                uv: model
                    .uv0
                    .as_ref()
                    .and_then(|uv0| uv0.get(index))
                    .map_or([0.0, 0.0], |uv| {
                        [f32::from(uv[0]) / 1024.0, f32::from(uv[1]) / 1024.0]
                    }),
                detail_uv: model_detail_uv(model, index),
                overlay_alpha: 0.0,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(VulkanStaticMesh {
        vertices,
        indices,
        draw_ranges,
    })
}

/// Projects a standard `Node38` model using each selected node's decoded
/// fallback pose before its recovered `Iron3D` placement transform.
///
/// The source link at `Node38 + 2` is a parent index: `0xFFFF` marks a root
/// and non-root indices are parent-before-child. The bridge composes the
/// fallback poses through that hierarchy before applying the outer transform.
/// Models without complete standard-node hierarchy data retain the established
/// unposed static bridge.
///
/// # Errors
///
/// Returns [`VulkanAssetMeshError`] when source geometry or transforms cannot
/// be represented by the current static Vulkan input contract.
pub fn project_msh_to_static_mesh_in_world_space_with_node_fallback_poses(
    model: &ModelAsset,
    transform: LegacyIron3dEulerTransform,
    scale: [f32; 3],
) -> Result<VulkanStaticMesh, VulkanAssetMeshError> {
    project_msh_to_static_mesh_in_world_space_with_node_poses(
        model,
        transform,
        scale,
        node38_fallback_hierarchy(model),
        None,
        false,
    )
}

/// Projects a standard `Node38` model at one explicit portable-reference
/// animation frame.
///
/// This uses decoded type-8 keys and the type-19 map but deliberately does
/// not infer the original engine's animation clock or x87 profile. Models
/// without a complete sampled hierarchy retain the established unposed bridge.
///
/// # Errors
///
/// Returns [`VulkanAssetMeshError`] when source geometry or transforms cannot
/// be represented by the current static Vulkan input contract.
pub fn project_msh_to_static_mesh_in_world_space_with_node_sampled_poses(
    model: &ModelAsset,
    transform: LegacyIron3dEulerTransform,
    scale: [f32; 3],
    animation_frame: u16,
) -> Result<VulkanStaticMesh, VulkanAssetMeshError> {
    project_msh_to_static_mesh_in_world_space_with_node_poses(
        model,
        transform,
        scale,
        node38_sampled_hierarchy(model, animation_frame),
        None,
        false,
    )
}

/// Projects a Node38 model using a hierarchy sampled by the caller.
///
/// This keeps geometry in sync with any sockets or bounds that the caller
/// derives from the same pose buffer.
///
/// # Errors
///
/// Returns [`VulkanAssetMeshError`] when source geometry or transforms cannot
/// be represented by the current static Vulkan input contract.
pub fn project_msh_to_static_mesh_in_world_space_with_node_pose_buffer(
    model: &ModelAsset,
    transform: LegacyIron3dEulerTransform,
    scale: [f32; 3],
    node_poses: &NodePoseBuffer,
) -> Result<VulkanStaticMesh, VulkanAssetMeshError> {
    project_msh_to_static_mesh_in_world_space_with_node_poses(
        model,
        transform,
        scale,
        Some(node_poses.clone()),
        None,
        false,
    )
}

/// Projects a mounted Node38 model using a caller-sampled hierarchy and the
/// parent socket pose derived from its own caller-sampled hierarchy.
///
/// # Errors
///
/// Returns [`VulkanAssetMeshError`] when source geometry or transforms cannot
/// be represented by the current static Vulkan input contract.
pub fn project_msh_to_static_mesh_in_world_space_with_node_pose_buffer_and_mount(
    model: &ModelAsset,
    transform: LegacyIron3dEulerTransform,
    scale: [f32; 3],
    node_poses: &NodePoseBuffer,
    mount_pose: VulkanNodePose,
) -> Result<VulkanStaticMesh, VulkanAssetMeshError> {
    project_msh_to_static_mesh_in_world_space_with_node_poses(
        model,
        transform,
        scale,
        Some(node_poses.clone()),
        Some(Pose {
            translation: mount_pose.translation,
            rotation: mount_pose.rotation,
        }),
        true,
    )
}

/// Evaluates one Node38 node pose for a unit mount socket.
#[must_use]
pub fn node38_pose(
    model: &ModelAsset,
    animation_frame: Option<u16>,
    node_index: usize,
) -> Option<VulkanNodePose> {
    node38_pose_with_root_replacement(model, animation_frame, node_index, false)
}

/// Evaluates one Node38 socket relative to a replacement root pose.
///
/// This is used when the model itself is already mounted: node zero belongs to
/// the parent socket and must not be composed a second time.
#[must_use]
pub fn node38_pose_relative_to_root(
    model: &ModelAsset,
    animation_frame: Option<u16>,
    node_index: usize,
) -> Option<VulkanNodePose> {
    node38_pose_with_root_replacement(model, animation_frame, node_index, true)
}

/// Returns one node's evaluated pose from an already sampled hierarchy.
#[must_use]
pub fn node38_pose_from_hierarchy(
    hierarchy: &NodePoseBuffer,
    node_index: usize,
) -> Option<VulkanNodePose> {
    node38_pose_value_from_hierarchy(hierarchy, node_index, false).map(vulkan_node_pose)
}

/// Returns a node's pose relative to a root pose that will be replaced by a
/// parent socket.
#[must_use]
pub fn node38_pose_relative_to_root_from_hierarchy(
    hierarchy: &NodePoseBuffer,
    node_index: usize,
) -> Option<VulkanNodePose> {
    node38_pose_value_from_hierarchy(hierarchy, node_index, true).map(vulkan_node_pose)
}

fn node38_pose_value_from_hierarchy(
    hierarchy: &NodePoseBuffer,
    node_index: usize,
    root_replaced: bool,
) -> Option<Pose> {
    let pose = *hierarchy.poses.get(node_index)?;
    if !root_replaced {
        return Some(pose);
    }
    if node_index == 0 {
        return Some(Pose::default());
    }
    let root = *hierarchy.poses.first()?;
    Some(compose_pose(inverse_pose(root), pose))
}

fn vulkan_node_pose(pose: Pose) -> VulkanNodePose {
    VulkanNodePose {
        translation: pose.translation,
        rotation: pose.rotation,
    }
}

fn node38_pose_with_root_replacement(
    model: &ModelAsset,
    animation_frame: Option<u16>,
    node_index: usize,
    root_replaced: bool,
) -> Option<VulkanNodePose> {
    let hierarchy = animation_frame.map_or_else(
        || node38_fallback_hierarchy(model),
        |frame| node38_sampled_hierarchy(model, frame),
    )?;
    let hierarchy = if root_replaced {
        strip_node38_root_pose(hierarchy)?
    } else {
        hierarchy
    };
    let pose = *hierarchy.poses.get(node_index)?;
    Some(VulkanNodePose {
        translation: pose.translation,
        rotation: pose.rotation,
    })
}

/// Projects a mounted Node38 model while preserving its local hierarchy.
///
/// The native unit loader mounts a child root at the parent's selected local
/// socket. The child root node itself is a socket placeholder and is hidden;
/// nodes `1..` retain their evaluated local hierarchy and are composed with the
/// parent's world socket pose. The mission transform remains applied once.
///
/// # Errors
///
/// Returns [`VulkanAssetMeshError`] when geometry or transforms cannot be
/// represented by the static input contract.
pub fn project_msh_to_static_mesh_in_world_space_with_node_sampled_poses_and_mount(
    model: &ModelAsset,
    transform: LegacyIron3dEulerTransform,
    scale: [f32; 3],
    animation_frame: u16,
    mount_pose: VulkanNodePose,
) -> Result<VulkanStaticMesh, VulkanAssetMeshError> {
    project_msh_to_static_mesh_in_world_space_with_node_poses(
        model,
        transform,
        scale,
        node38_sampled_hierarchy(model, animation_frame),
        Some(Pose {
            translation: mount_pose.translation,
            rotation: mount_pose.rotation,
        }),
        true,
    )
}

/// Projects a mounted Node38 model at its decoded fallback pose.
///
/// # Errors
///
/// Returns [`VulkanAssetMeshError`] when geometry or transforms cannot be
/// represented by the static input contract.
pub fn project_msh_to_static_mesh_in_world_space_with_node_fallback_poses_and_mount(
    model: &ModelAsset,
    transform: LegacyIron3dEulerTransform,
    scale: [f32; 3],
    mount_pose: VulkanNodePose,
) -> Result<VulkanStaticMesh, VulkanAssetMeshError> {
    project_msh_to_static_mesh_in_world_space_with_node_poses(
        model,
        transform,
        scale,
        node38_fallback_hierarchy(model),
        Some(Pose {
            translation: mount_pose.translation,
            rotation: mount_pose.rotation,
        }),
        true,
    )
}

fn project_msh_to_static_mesh_in_world_space_with_node_poses(
    model: &ModelAsset,
    transform: LegacyIron3dEulerTransform,
    scale: [f32; 3],
    hierarchy: Option<fparkan_animation::NodePoseBuffer>,
    mount_pose: Option<Pose>,
    hide_root_node: bool,
) -> Result<VulkanStaticMesh, VulkanAssetMeshError> {
    if let Some(hierarchy) = hierarchy.as_ref() {
        if hierarchy.poses.len() != model.node_count {
            return Err(VulkanAssetMeshError::NodePoseCountMismatch {
                expected: model.node_count,
                actual: hierarchy.poses.len(),
            });
        }
    }
    if !transform
        .translation
        .iter()
        .chain(transform.orientation_radians.iter())
        .chain(scale.iter())
        .all(|value| value.is_finite())
    {
        return Err(VulkanAssetMeshError::NonFinitePosition);
    }
    if model.node_stride != 38 || model.node_count == 0 || model.animation.is_none() {
        return project_msh_to_static_mesh_in_world_space_with_transform_and_root_pose(
            model, transform, scale, mount_pose,
        );
    }
    let hierarchy = if mount_pose.is_some() {
        hierarchy.and_then(strip_node38_root_pose)
    } else {
        hierarchy
    };
    let Some(hierarchy) = hierarchy else {
        return project_msh_to_static_mesh_in_world_space_with_transform_and_root_pose(
            model, transform, scale, mount_pose,
        );
    };

    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let mut draw_ranges = Vec::new();
    for node_index in 0..model.node_count {
        let node =
            NodeId(u32::try_from(node_index).map_err(|_| VulkanAssetMeshError::IndexOutOfRange)?);
        let Some(slot) = selected_slot(model, node, Lod(0), Group(0)) else {
            continue;
        };
        if hide_root_node && node_index == 0 {
            continue;
        }
        let pose = mount_pose.map_or(hierarchy.poses[node_index], |mount| {
            compose_pose(mount, hierarchy.poses[node_index])
        });
        if !pose
            .translation
            .iter()
            .chain(pose.rotation.iter())
            .all(|value| value.is_finite())
        {
            return Err(VulkanAssetMeshError::NonFinitePosition);
        }
        for batch in draw_batches(model, slot).map_err(|_| VulkanAssetMeshError::IndexOutOfRange)? {
            append_node_fallback_batch(
                model,
                batch,
                pose.translation,
                pose.rotation,
                transform,
                scale,
                &mut vertices,
                &mut indices,
                &mut draw_ranges,
            )?;
        }
    }
    if indices.is_empty() {
        // A valid Node38 hierarchy may intentionally select no drawable
        // batches for this component (for example, a mounted attachment
        // whose root is hidden at the chosen LOD/group). Preserve that empty
        // result instead of falling back to every source batch and drawing
        // geometry that the node selection excluded.
        return Ok(VulkanStaticMesh {
            vertices,
            indices,
            draw_ranges,
        });
    }
    Ok(VulkanStaticMesh {
        vertices,
        indices,
        draw_ranges,
    })
}

/// Replaces Node38 node zero with the identity pose while retaining every
/// descendant's pose relative to that root. Mounted components use this
/// because the native loader substitutes their root with the parent socket.
fn strip_node38_root_pose(
    mut hierarchy: fparkan_animation::NodePoseBuffer,
) -> Option<fparkan_animation::NodePoseBuffer> {
    let root = *hierarchy.poses.first()?;
    let inverse_root = inverse_pose(root);
    hierarchy.poses[0] = Pose::default();
    for pose in hierarchy.poses.iter_mut().skip(1) {
        *pose = compose_pose(inverse_root, *pose);
    }
    Some(hierarchy)
}

fn inverse_pose(pose: Pose) -> Pose {
    let [x, y, z, w] = pose.rotation;
    let length_squared = x * x + y * y + z * z + w * w;
    let inverse_rotation = if length_squared.is_finite() && length_squared > f32::EPSILON {
        [
            -x / length_squared,
            -y / length_squared,
            -z / length_squared,
            w / length_squared,
        ]
    } else {
        [0.0, 0.0, 0.0, 1.0]
    };
    let translation = rotate_by_quaternion(
        [
            -pose.translation[0],
            -pose.translation[1],
            -pose.translation[2],
        ],
        inverse_rotation,
    );
    Pose {
        translation,
        rotation: inverse_rotation,
    }
}

fn append_node_fallback_batch(
    model: &ModelAsset,
    batch: &fparkan_msh::Batch,
    translation: [f32; 3],
    rotation: [f32; 4],
    transform: LegacyIron3dEulerTransform,
    scale: [f32; 3],
    vertices: &mut Vec<VulkanStaticVertex>,
    indices: &mut Vec<u32>,
    draw_ranges: &mut Vec<VulkanStaticDrawRange>,
) -> Result<(), VulkanAssetMeshError> {
    // Some authored MSH tables retain empty alternate batches. They do not
    // describe a drawable range and would violate the renderer's contiguous
    // non-empty draw-range contract when a full mission root set is loaded.
    if batch.index_count == 0 {
        return Ok(());
    }
    if !usize::from(batch.index_count).is_multiple_of(3) {
        return Err(VulkanAssetMeshError::IndexOutOfRange);
    }
    let first_index =
        u32::try_from(indices.len()).map_err(|_| VulkanAssetMeshError::IndexOutOfRange)?;
    let start =
        usize::try_from(batch.index_start).map_err(|_| VulkanAssetMeshError::IndexOutOfRange)?;
    let end = start
        .checked_add(usize::from(batch.index_count))
        .ok_or(VulkanAssetMeshError::IndexOutOfRange)?;
    for &raw_index in model
        .indices
        .get(start..end)
        .ok_or(VulkanAssetMeshError::IndexOutOfRange)?
    {
        let source_index = batch
            .base_vertex
            .checked_add(u32::from(raw_index))
            .ok_or(VulkanAssetMeshError::IndexOutOfRange)?;
        let source_index =
            usize::try_from(source_index).map_err(|_| VulkanAssetMeshError::IndexOutOfRange)?;
        let position = *model
            .positions
            .get(source_index)
            .ok_or(VulkanAssetMeshError::IndexOutOfRange)?;
        if !position.iter().all(|value| value.is_finite()) {
            return Err(VulkanAssetMeshError::NonFinitePosition);
        }
        let local_position = rotate_by_quaternion(position, rotation);
        let local_position = [
            local_position[0] + translation[0],
            local_position[1] + translation[1],
            local_position[2] + translation[2],
        ];
        let position = transform
            .try_transform_scaled_point(local_position, scale)
            .ok_or(VulkanAssetMeshError::NonFinitePosition)?;
        let vertex_index =
            u32::try_from(vertices.len()).map_err(|_| VulkanAssetMeshError::IndexOutOfRange)?;
        vertices.push(VulkanStaticVertex {
            position,
            color: [1.0, 1.0, 1.0],
            normal: {
                let normal = rotate_by_quaternion(model_normal(model, source_index), rotation);
                transform_normal(transform, normal, scale)
                    .ok_or(VulkanAssetMeshError::NonFinitePosition)?
            },
            uv: model
                .uv0
                .as_ref()
                .and_then(|uv0| uv0.get(source_index))
                .map_or([0.0, 0.0], |uv| {
                    [f32::from(uv[0]) / 1024.0, f32::from(uv[1]) / 1024.0]
                }),
            detail_uv: model_detail_uv(model, source_index),
            overlay_alpha: 0.0,
        });
        indices.push(vertex_index);
    }
    draw_ranges.push(VulkanStaticDrawRange {
        first_index,
        index_count: u32::from(batch.index_count),
        material_index: u16::from(batch.material_index),
        lightmap_index: batch.lightmap_index,
        batch_flags: batch.batch_flags,
        pipeline_state: world_pipeline_state(),
        alpha_test_reference: 0,
    });
    Ok(())
}

fn compose_pose(parent: Pose, child: Pose) -> Pose {
    let translation = rotate_by_quaternion(child.translation, parent.rotation);
    Pose {
        translation: [
            parent.translation[0] + translation[0],
            parent.translation[1] + translation[1],
            parent.translation[2] + translation[2],
        ],
        rotation: multiply_quaternions(parent.rotation, child.rotation),
    }
}

fn multiply_quaternions(left: [f32; 4], right: [f32; 4]) -> [f32; 4] {
    let [lx, ly, lz, lw] = left;
    let [rx, ry, rz, rw] = right;
    [
        lw * rx + lx * rw + ly * rz - lz * ry,
        lw * ry - lx * rz + ly * rw + lz * rx,
        lw * rz + lx * ry - ly * rx + lz * rw,
        lw * rw - lx * rx - ly * ry - lz * rz,
    ]
}

fn rotate_by_quaternion(position: [f32; 3], rotation: [f32; 4]) -> [f32; 3] {
    let [x, y, z, w] = rotation;
    let tx = 2.0 * (y * position[2] - z * position[1]);
    let ty = 2.0 * (z * position[0] - x * position[2]);
    let tz = 2.0 * (x * position[1] - y * position[0]);
    [
        position[0] + w * tx + (y * tz - z * ty),
        position[1] + w * ty + (z * tx - x * tz),
        position[2] + w * tz + (x * ty - y * tx),
    ]
}

fn model_normal(model: &ModelAsset, index: usize) -> [f32; 3] {
    model
        .normals
        .as_ref()
        .and_then(|normals| normals.get(index))
        .map_or([0.0, 0.0, 1.0], |raw| {
            // Indexed MSH stores signed bytes.  The native decoder divides
            // by 127 without clamping or renormalizing, so the packed length
            // (including -128/127) reaches the shader unchanged.
            [
                f32::from(raw[0]) / 127.0,
                f32::from(raw[1]) / 127.0,
                f32::from(raw[2]) / 127.0,
            ]
        })
}

/// Decodes the optional MSH type-18 secondary UV stream.  Native model and
/// terrain samplers use the same unsigned 16-bit fixed-point 1/1024 scale for
/// both texture stages; an absent stream leaves the neutral detail coordinate.
fn model_detail_uv(model: &ModelAsset, index: usize) -> [f32; 2] {
    model
        .uv1
        .as_ref()
        .and_then(|uv1| uv1.get(index))
        .map_or([0.0, 0.0], |uv| {
            [f32::from(uv[0]) / 1024.0, f32::from(uv[1]) / 1024.0]
        })
}

fn terrain_normal(terrain: &LandMeshDocument, index: usize) -> [f32; 3] {
    terrain.normals.get(index).map_or([0.0, 0.0, 1.0], |raw| {
        // Ngi32's normal decoder sign-extends each source byte and multiplies
        // it by the literal 1/128.  Preserve the packed magnitude here: the
        // shader consumes the native vector directly, and normalizing it
        // changes the lighting response for every authored byte.
        [
            f32::from(raw[0]) * (1.0 / 128.0),
            f32::from(raw[1]) * (1.0 / 128.0),
            f32::from(raw[2]) * (1.0 / 128.0),
        ]
    })
}

fn transform_normal(
    transform: LegacyIron3dEulerTransform,
    normal: [f32; 3],
    scale: [f32; 3],
) -> Option<[f32; 3]> {
    if !normal.iter().all(|value| value.is_finite())
        || !scale
            .iter()
            .all(|value| value.is_finite() && value.abs() > f32::EPSILON)
    {
        return None;
    }
    // Positions use `R * (scale * point)`. Normals therefore use the
    // inverse-transpose linear transform: inverse scale first, then R.
    let inverse_scaled = [
        normal[0] / scale[0],
        normal[1] / scale[1],
        normal[2] / scale[2],
    ];
    let matrix = transform.try_row_major()?;
    let transformed = [
        matrix[0] * inverse_scaled[0]
            + matrix[1] * inverse_scaled[1]
            + matrix[2] * inverse_scaled[2],
        matrix[4] * inverse_scaled[0]
            + matrix[5] * inverse_scaled[1]
            + matrix[6] * inverse_scaled[2],
        matrix[8] * inverse_scaled[0]
            + matrix[9] * inverse_scaled[1]
            + matrix[10] * inverse_scaled[2],
    ];
    let transformed_length = transformed[0].mul_add(
        transformed[0],
        transformed[1].mul_add(transformed[1], transformed[2] * transformed[2]),
    );
    if !transformed_length.is_finite() || transformed_length <= f32::EPSILON {
        return Some([0.0; 3]);
    }
    let source_length = normal[0].mul_add(
        normal[0],
        normal[1].mul_add(normal[1], normal[2] * normal[2]),
    );
    if !source_length.is_finite() || source_length <= f32::EPSILON {
        return Some([0.0; 3]);
    }
    let magnitude = source_length.sqrt();
    let inverse_transformed_length = transformed_length.sqrt().recip();
    Some([
        transformed[0] * inverse_transformed_length * magnitude,
        transformed[1] * inverse_transformed_length * magnitude,
        transformed[2] * inverse_transformed_length * magnitude,
    ])
}

/// Projects validated `Land.msh` terrain geometry into the static Vulkan path.
///
/// This bridge preserves the source triangle order from `TerrainFace28` and
/// consumes its validated position and UV0 streams. Contiguous source faces are
/// retained as separate draw ranges keyed by their exact packed
/// `TerrainFace28.material_tag`, so a caller can resolve the map-local
/// material contract without changing topology. Slot selection, camera
/// transforms, fog and surface-mask shading need their own evidence before
/// they can be modeled as renderer state.
///
/// # Errors
///
/// Returns [`VulkanAssetMeshError`] if the terrain has no triangles, contains
/// non-finite positions, has no usable XY extent, or references data outside
/// the current static Vulkan input contract.
pub fn project_land_msh_to_static_mesh(
    terrain: &LandMeshDocument,
) -> Result<VulkanStaticMesh, VulkanAssetMeshError> {
    let mut indices = Vec::with_capacity(
        terrain
            .faces
            .len()
            .checked_mul(3)
            .ok_or(VulkanAssetMeshError::IndexOutOfRange)?,
    );
    for face in &terrain.faces {
        indices.extend(face.vertices.map(u32::from));
    }
    if indices.is_empty() {
        return Err(VulkanAssetMeshError::EmptyGeometry);
    }

    let (min_x, max_x, min_y, max_y) = static_mesh_xy_bounds(&terrain.positions, &indices)?;
    let frame = VulkanStaticXyFrame::from_bounds(min_x, max_x, min_y, max_y)?;
    project_land_msh_to_static_mesh_in_xy_frame(terrain, frame)
}

/// Projects `Land.msh` terrain geometry into a shared diagnostic XY frame.
///
/// # Errors
///
/// Returns [`VulkanAssetMeshError`] when terrain geometry or the static Vulkan
/// input contract cannot represent the source mesh.
pub fn project_land_msh_to_static_mesh_in_xy_frame(
    terrain: &LandMeshDocument,
    frame: VulkanStaticXyFrame,
) -> Result<VulkanStaticMesh, VulkanAssetMeshError> {
    let mut indices = Vec::with_capacity(
        terrain
            .faces
            .len()
            .checked_mul(3)
            .ok_or(VulkanAssetMeshError::IndexOutOfRange)?,
    );
    for face in &terrain.faces {
        indices.extend(face.vertices.map(u32::from));
    }
    if indices.is_empty() {
        return Err(VulkanAssetMeshError::EmptyGeometry);
    }
    let vertices = terrain
        .positions
        .iter()
        .enumerate()
        .map(|(index, position)| {
            if !position.iter().all(|value| value.is_finite()) {
                return Err(VulkanAssetMeshError::NonFinitePosition);
            }
            Ok(VulkanStaticVertex {
                position: [
                    frame.project(*position)[0],
                    frame.project(*position)[1],
                    0.0,
                ],
                color: [1.0, 1.0, 1.0],
                normal: terrain_normal(terrain, index),
                uv: terrain
                    .uv0
                    .get(index)
                    .map_or(frame.planar_uv(*position), |uv| {
                        [f32::from(uv[0]) / 1024.0, f32::from(uv[1]) / 1024.0]
                    }),
                detail_uv: terrain_detail_uv(terrain, index),
                overlay_alpha: terrain_overlay_alpha(terrain, index),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(VulkanStaticMesh {
        vertices,
        indices,
        draw_ranges: static_terrain_draw_ranges(terrain)?,
    })
}

/// Projects `Land.msh` terrain with its decoded source coordinates intact.
///
/// This is the terrain counterpart of
/// [`project_msh_to_static_mesh_in_world_space`]. It has no viewer transform:
/// camera framing and any coordinate conversion are owned by the recovered
/// legacy camera adapter.
///
/// # Errors
///
/// Returns [`VulkanAssetMeshError`] when terrain geometry or the static Vulkan
/// input contract cannot represent the source mesh.
pub fn project_land_msh_to_static_mesh_in_world_space(
    terrain: &LandMeshDocument,
) -> Result<VulkanStaticMesh, VulkanAssetMeshError> {
    let indices = static_terrain_indices(terrain)?;
    let vertices = terrain
        .positions
        .iter()
        .enumerate()
        .map(|(index, position)| {
            if !position.iter().all(|value| value.is_finite()) {
                return Err(VulkanAssetMeshError::NonFinitePosition);
            }
            Ok(VulkanStaticVertex {
                position: *position,
                color: [1.0, 1.0, 1.0],
                normal: terrain_normal(terrain, index),
                uv: terrain.uv0.get(index).map_or([0.0, 0.0], |uv| {
                    [f32::from(uv[0]) / 1024.0, f32::from(uv[1]) / 1024.0]
                }),
                detail_uv: terrain_detail_uv(terrain, index),
                overlay_alpha: terrain_overlay_alpha(terrain, index),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(VulkanStaticMesh {
        vertices,
        indices,
        draw_ranges: static_terrain_draw_ranges(terrain)?,
    })
}

/// Projects terrain into the mission space consumed by the legacy D3D7 camera.
///
/// `Land.msh` stores horizontal coordinates directly, while its height stream
/// uses 1/32 units. The scale is evidenced by the GOG `AutoDemo` map: raw terrain
/// heights `95.29412..288.23532` become `2.978..9.007`, matching its placed
/// object heights and the live Ngi32 camera's world-space Z coordinate.
///
/// This conversion is intentionally confined to the captured legacy-camera
/// path. [`project_land_msh_to_static_mesh_in_world_space`] remains the raw
/// source-coordinate bridge for format inspection.
///
/// # Errors
///
/// Returns [`VulkanAssetMeshError`] when the terrain cannot enter the static
/// Vulkan input contract.
pub fn project_land_msh_to_static_mesh_in_legacy_world_space(
    terrain: &LandMeshDocument,
) -> Result<VulkanStaticMesh, VulkanAssetMeshError> {
    let mut mesh = project_land_msh_to_static_mesh_in_world_space(terrain)?;
    for vertex in &mut mesh.vertices {
        vertex.position[2] *= LEGACY_LAND_HEIGHT_SCALE;
    }
    Ok(mesh)
}

fn static_terrain_indices(terrain: &LandMeshDocument) -> Result<Vec<u32>, VulkanAssetMeshError> {
    let mut indices = Vec::with_capacity(
        terrain
            .faces
            .len()
            .checked_mul(3)
            .ok_or(VulkanAssetMeshError::IndexOutOfRange)?,
    );
    for face in &terrain.faces {
        indices.extend(face.vertices.map(u32::from));
    }
    if indices.is_empty() {
        return Err(VulkanAssetMeshError::EmptyGeometry);
    }
    Ok(indices)
}

/// Decodes the terrain type-18 repeating detail coordinate. The original
/// stream stores two unsigned 16-bit fixed-point coordinates with the same
/// 1/1024 texture scale used by the texture sampler.
fn terrain_detail_uv(terrain: &LandMeshDocument, index: usize) -> [f32; 2] {
    terrain.aux18.get(index).map_or([0.0, 0.0], |raw| {
        [
            f32::from(u16::from_le_bytes([raw[0], raw[1]])) / 1024.0,
            f32::from(u16::from_le_bytes([raw[2], raw[3]])) / 1024.0,
        ]
    })
}

/// Decodes the terrain type-14 overlay mask as the source float alpha.
/// Invalid or out-of-range values are disabled at the static bridge boundary;
/// valid map data is the continuous 0..1 mask consumed by the original shade
/// path.
fn terrain_overlay_alpha(terrain: &LandMeshDocument, index: usize) -> f32 {
    terrain
        .aux14
        .get(index)
        .map_or(0.0, |raw| f32::from_le_bytes(*raw))
        .is_finite()
        .then_some(
            terrain
                .aux14
                .get(index)
                .map_or(0.0, |raw| f32::from_le_bytes(*raw))
                .clamp(0.0, 1.0),
        )
        .unwrap_or(0.0)
}

fn static_terrain_draw_ranges(
    terrain: &LandMeshDocument,
) -> Result<Vec<VulkanStaticDrawRange>, VulkanAssetMeshError> {
    let mut ranges = Vec::new();
    let mut start_face = 0usize;
    while start_face < terrain.faces.len() {
        let material_index = terrain.faces[start_face].material_tag;
        let mut end_face = start_face + 1;
        while end_face < terrain.faces.len()
            && terrain.faces[end_face].material_tag == material_index
        {
            end_face += 1;
        }
        ranges.push(VulkanStaticDrawRange {
            first_index: u32::try_from(start_face)
                .map_err(|_| VulkanAssetMeshError::IndexOutOfRange)?
                .checked_mul(3)
                .ok_or(VulkanAssetMeshError::IndexOutOfRange)?,
            index_count: u32::try_from(end_face - start_face)
                .map_err(|_| VulkanAssetMeshError::IndexOutOfRange)?
                .checked_mul(3)
                .ok_or(VulkanAssetMeshError::IndexOutOfRange)?,
            material_index,
            lightmap_index: u8::MAX,
            batch_flags: 0,
            pipeline_state: world_pipeline_state(),
            alpha_test_reference: 0,
        });
        start_face = end_face;
    }
    Ok(ranges)
}

fn static_model_indices_and_ranges(
    model: &ModelAsset,
) -> Result<(Vec<u32>, Vec<VulkanStaticDrawRange>), VulkanAssetMeshError> {
    let mut indices = Vec::new();
    let batches = static_model_preview_batches(model)?;
    let mut draw_ranges = Vec::with_capacity(batches.len());
    for batch in batches {
        if batch.index_count == 0 {
            continue;
        }
        if !usize::from(batch.index_count).is_multiple_of(3) {
            return Err(VulkanAssetMeshError::IndexOutOfRange);
        }
        let first_index =
            u32::try_from(indices.len()).map_err(|_| VulkanAssetMeshError::IndexOutOfRange)?;
        let start = usize::try_from(batch.index_start)
            .map_err(|_| VulkanAssetMeshError::IndexOutOfRange)?;
        let end = start
            .checked_add(usize::from(batch.index_count))
            .ok_or(VulkanAssetMeshError::IndexOutOfRange)?;
        for &raw_index in model
            .indices
            .get(start..end)
            .ok_or(VulkanAssetMeshError::IndexOutOfRange)?
        {
            let index = batch
                .base_vertex
                .checked_add(u32::from(raw_index))
                .ok_or(VulkanAssetMeshError::IndexOutOfRange)?;
            indices.push(index);
        }
        draw_ranges.push(VulkanStaticDrawRange {
            first_index,
            index_count: u32::from(batch.index_count),
            material_index: u16::from(batch.material_index),
            lightmap_index: batch.lightmap_index,
            batch_flags: batch.batch_flags,
            pipeline_state: world_pipeline_state(),
            alpha_test_reference: 0,
        });
    }
    if indices.is_empty() {
        return Err(VulkanAssetMeshError::EmptyGeometry);
    }
    Ok((indices, draw_ranges))
}

/// Returns the source batches selected by the initial static model pose.
///
/// A `Node38` has a three-LOD by five-group slot map. The static bridge has no
/// recovered animation or group-selection controller, so it uses the source's
/// first LOD/group entry for every node instead of submitting every batch in the
/// file (which also includes alternate LODs and groups). Models without the
/// standard node layout retain the prior all-batches diagnostic fallback.
fn static_model_preview_batches(
    model: &ModelAsset,
) -> Result<Vec<&fparkan_msh::Batch>, VulkanAssetMeshError> {
    if model.node_stride != 38 || model.node_count == 0 {
        return Ok(model.batches.iter().collect());
    }

    let mut selected = Vec::new();
    for node_index in 0..model.node_count {
        let node =
            NodeId(u32::try_from(node_index).map_err(|_| VulkanAssetMeshError::IndexOutOfRange)?);
        let Some(slot) = selected_slot(model, node, Lod(0), Group(0)) else {
            continue;
        };
        let slot = model
            .slots
            .get(usize::try_from(slot.0).map_err(|_| VulkanAssetMeshError::IndexOutOfRange)?)
            .ok_or(VulkanAssetMeshError::IndexOutOfRange)?;
        let start = usize::from(slot.batch_start);
        let end = start
            .checked_add(usize::from(slot.batch_count))
            .ok_or(VulkanAssetMeshError::IndexOutOfRange)?;
        selected.extend(
            model
                .batches
                .get(start..end)
                .ok_or(VulkanAssetMeshError::IndexOutOfRange)?,
        );
    }

    if selected.is_empty() {
        return Ok(model.batches.iter().collect());
    }
    Ok(selected)
}

fn static_mesh_xy_bounds(
    positions: &[[f32; 3]],
    indices: &[u32],
) -> Result<(f32, f32, f32, f32), VulkanAssetMeshError> {
    let mut min_x = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for &index in indices {
        let position = positions
            .get(usize::try_from(index).map_err(|_| VulkanAssetMeshError::IndexOutOfRange)?)
            .ok_or(VulkanAssetMeshError::IndexOutOfRange)?;
        if !position.iter().all(|value| value.is_finite()) {
            return Err(VulkanAssetMeshError::NonFinitePosition);
        }
        min_x = min_x.min(position[0]);
        max_x = max_x.max(position[0]);
        min_y = min_y.min(position[1]);
        max_y = max_y.max(position[1]);
    }
    Ok((min_x, max_x, min_y, max_y))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_animation::{AnimKey24, AnimationTime, Pose};
    use fparkan_msh::{Batch, ModelAnimation, ModelAsset, Slot};
    use fparkan_terrain_format::{FullSurfaceMask, TerrainFace28, TerrainSlotTable};

    fn model(positions: Vec<[f32; 3]>, indices: Vec<u16>, batches: Vec<Batch>) -> ModelAsset {
        ModelAsset {
            node_stride: 0,
            node_count: 0,
            nodes_raw: Vec::new(),
            bounding_sphere_center: [0.0; 3],
            bounding_sphere_radius: 0.0,
            slots: Vec::new(),
            positions,
            normals: None,
            uv0: None,
            uv1: None,
            indices,
            batches,
            node_names: None,
            animation: None,
        }
    }

    fn batch(index_start: u32, index_count: u16, base_vertex: u32) -> Batch {
        Batch {
            batch_flags: 0,
            material_index_hi: 0,
            material_index: 0,
            lightmap_index: 0,
            local_batch_index: 0,
            index_count,
            index_start,
            vertex_count: 0,
            base_vertex,
        }
    }

    #[test]
    fn static_preview_uses_first_lod_group_slot_for_standard_nodes() {
        let mut node = vec![0_u8; 38];
        node[8..10].copy_from_slice(&1_u16.to_le_bytes());
        let model = ModelAsset {
            node_stride: 38,
            node_count: 1,
            nodes_raw: node,
            bounding_sphere_center: [0.0; 3],
            bounding_sphere_radius: 0.0,
            slots: vec![
                Slot {
                    tri_start: 0,
                    tri_count: 0,
                    batch_start: 0,
                    batch_count: 1,
                    aabb_min: [0.0; 3],
                    aabb_max: [1.0; 3],
                    sphere_center: [0.0; 3],
                    sphere_radius: 1.0,
                    opaque: [0; 5],
                },
                Slot {
                    tri_start: 0,
                    tri_count: 0,
                    batch_start: 1,
                    batch_count: 1,
                    aabb_min: [0.0; 3],
                    aabb_max: [1.0; 3],
                    sphere_center: [0.0; 3],
                    sphere_radius: 1.0,
                    opaque: [0; 5],
                },
            ],
            positions: Vec::new(),
            normals: None,
            uv0: None,
            uv1: None,
            indices: Vec::new(),
            batches: vec![
                batch(0, 0, 0),
                Batch {
                    material_index: 1,
                    ..batch(0, 0, 0)
                },
            ],
            node_names: None,
            animation: None,
        };

        let selected = static_model_preview_batches(&model).expect("valid selected slot");
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].material_index, 1);
    }

    #[test]
    fn projects_xy_geometry_and_applies_base_vertex() {
        let mesh = project_msh_to_static_mesh(&model(
            vec![
                [99.0, 99.0, 0.0],
                [-2.0, -1.0, 4.0],
                [2.0, -1.0, 8.0],
                [-2.0, 3.0, 1.0],
            ],
            vec![0, 1, 2],
            vec![batch(0, 3, 1)],
        ))
        .expect("representable MSH");

        assert_eq!(mesh.indices, vec![1, 2, 3]);
        assert_eq!(
            mesh.draw_ranges,
            vec![VulkanStaticDrawRange {
                first_index: 0,
                index_count: 3,
                material_index: 0,
                lightmap_index: 0,
                batch_flags: 0,
                pipeline_state: world_pipeline_state(),
                alpha_test_reference: 0,
            }]
        );
        assert_eq!(mesh.vertices[1].position, [-0.8, -0.8, 0.0]);
        assert_eq!(mesh.vertices[2].position, [0.8, -0.8, 0.0]);
        assert_eq!(mesh.vertices[3].position, [-0.8, 0.8, 0.0]);
    }

    #[test]
    fn projects_packed_uv0_using_documented_fixed_point_scale() {
        let mut source = model(
            vec![[-2.0, 0.0, -1.0], [2.0, 0.0, -1.0], [-2.0, 0.0, 3.0]],
            vec![0, 1, 2],
            vec![batch(0, 3, 0)],
        );
        source.uv0 = Some(vec![[1024, 65024], [0, 2048], [64512, 512]]);

        let mesh = project_msh_to_static_mesh(&source).expect("representable MSH");

        assert_eq!(mesh.vertices[0].uv, [1.0, 63.5]);
        assert_eq!(mesh.vertices[1].uv, [0.0, 2.0]);
        assert_eq!(mesh.vertices[2].uv, [63.0, 0.5]);
    }

    #[test]
    fn world_space_msh_preserves_z_and_applies_only_decoded_translation_scale() {
        let mut source = model(
            vec![[2.0, -3.0, 4.0], [0.0, 1.0, 2.0], [-1.0, 2.0, 3.0]],
            vec![0, 1, 2],
            vec![batch(0, 3, 0)],
        );
        source.uv0 = Some(vec![[1024, 65024], [0, 2048], [64512, 512]]);

        let mesh = project_msh_to_static_mesh_in_world_space(
            &source,
            [100.0, 200.0, 300.0],
            [2.0, -1.0, 0.5],
        )
        .expect("representable source-space MSH");

        assert_eq!(mesh.indices, vec![0, 1, 2]);
        assert_eq!(mesh.vertices[0].position, [104.0, 203.0, 302.0]);
        assert_eq!(mesh.vertices[1].position, [100.0, 199.0, 301.0]);
        assert_eq!(mesh.vertices[0].uv, [1.0, 63.5]);
    }

    #[test]
    fn world_space_msh_applies_recovered_iron3d_orientation_after_scale() {
        let source = model(
            vec![[2.0, 3.0, 4.0], [0.0, 0.0, 0.0], [1.0, 0.0, 0.0]],
            vec![0, 1, 2],
            vec![batch(0, 3, 0)],
        );

        let mesh = project_msh_to_static_mesh_in_world_space_with_transform(
            &source,
            LegacyIron3dEulerTransform {
                translation: [10.0, 20.0, 30.0],
                orientation_radians: [0.0, 0.0, std::f32::consts::FRAC_PI_2],
            },
            [2.0, 1.0, 0.5],
        )
        .expect("finite recovered transform");

        assert_eq!(mesh.vertices[0].position, [7.0, 24.0, 32.0]);
        assert_eq!(mesh.vertices[1].position, [10.0, 20.0, 30.0]);
        assert_eq!(mesh.vertices[2].position, [10.0, 22.0, 30.0]);
    }

    #[test]
    fn world_space_msh_transforms_normals_with_inverse_scale() {
        let mut source = model(
            vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            vec![0, 1, 2],
            vec![batch(0, 3, 0)],
        );
        source.normals = Some(vec![[127, 127, 0, 0]; 3]);

        let mesh = project_msh_to_static_mesh_in_world_space_with_transform(
            &source,
            LegacyIron3dEulerTransform {
                translation: [0.0; 3],
                orientation_radians: [0.0; 3],
            },
            [2.0, 3.0, 1.0],
        )
        .expect("finite transformed normals");

        let expected_magnitude = 2.0_f32.sqrt() * (127.0 / 127.0);
        assert!((mesh.vertices[0].normal[0] - 0.83205 * expected_magnitude).abs() < 0.0001);
        assert!((mesh.vertices[0].normal[1] - 0.55470 * expected_magnitude).abs() < 0.0001);
        assert_eq!(mesh.vertices[0].normal[2], 0.0);
    }

    #[test]
    fn indexed_msh_normal_bytes_keep_packed_magnitude_through_uniform_scale() {
        let mut source = model(
            vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            vec![0, 1, 2],
            vec![batch(0, 3, 0)],
        );
        source.normals = Some(vec![[-128, 0, 0, 0], [64, 0, 0, 0], [0, 0, 127, 0]]);
        let mesh = project_msh_to_static_mesh_in_world_space_with_transform(
            &source,
            LegacyIron3dEulerTransform {
                translation: [0.0; 3],
                orientation_radians: [0.0, 0.0, std::f32::consts::FRAC_PI_2],
            },
            [2.0; 3],
        )
        .expect("finite packed normals");
        let lengths = mesh
            .vertices
            .iter()
            .map(|vertex| {
                vertex.normal[0]
                    .mul_add(
                        vertex.normal[0],
                        vertex.normal[1]
                            .mul_add(vertex.normal[1], vertex.normal[2] * vertex.normal[2]),
                    )
                    .sqrt()
            })
            .collect::<Vec<_>>();
        assert!((lengths[0] - 128.0 / 127.0).abs() < 0.0001);
        assert!((lengths[1] - 64.0 / 127.0).abs() < 0.0001);
    }

    #[test]
    fn xy_preview_applies_recovered_iron3d_orientation_before_framing() {
        let source = model(
            vec![[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 0.0]],
            vec![0, 1, 2],
            vec![batch(0, 3, 0)],
        );
        let frame =
            VulkanStaticXyFrame::from_bounds(0.0, 4.0, 0.0, 4.0).expect("valid diagnostic frame");

        let mesh = project_msh_to_static_mesh_in_xy_frame_with_node_fallback_poses(
            &source,
            frame,
            LegacyIron3dEulerTransform {
                translation: [2.0, 1.0, 0.0],
                orientation_radians: [0.0, 0.0, std::f32::consts::FRAC_PI_2],
            },
            [1.0; 3],
        )
        .expect("diagnostic static mesh");

        assert!(mesh.vertices[0].position[0].abs() < 0.0001);
        assert!(mesh.vertices[0].position[1].abs() < 0.0001);
        assert_eq!(mesh.vertices[0].position[2], 0.0);
    }

    #[test]
    fn world_space_msh_applies_each_node_fallback_pose_before_root_transform() {
        let mut nodes = vec![0_u8; 76];
        nodes[2..4].copy_from_slice(&u16::MAX.to_le_bytes());
        nodes[8..10].copy_from_slice(&0_u16.to_le_bytes());
        nodes[38 + 2..38 + 4].copy_from_slice(&0_u16.to_le_bytes());
        nodes[38 + 6..38 + 8].copy_from_slice(&1_u16.to_le_bytes());
        nodes[38 + 8..38 + 10].copy_from_slice(&1_u16.to_le_bytes());
        let source = ModelAsset {
            node_stride: 38,
            node_count: 2,
            nodes_raw: nodes,
            bounding_sphere_center: [0.0; 3],
            bounding_sphere_radius: 0.0,
            slots: vec![
                Slot {
                    tri_start: 0,
                    tri_count: 0,
                    batch_start: 0,
                    batch_count: 1,
                    aabb_min: [0.0; 3],
                    aabb_max: [1.0; 3],
                    sphere_center: [0.0; 3],
                    sphere_radius: 1.0,
                    opaque: [0; 5],
                },
                Slot {
                    tri_start: 0,
                    tri_count: 0,
                    batch_start: 1,
                    batch_count: 1,
                    aabb_min: [0.0; 3],
                    aabb_max: [1.0; 3],
                    sphere_center: [0.0; 3],
                    sphere_radius: 1.0,
                    opaque: [0; 5],
                },
            ],
            positions: vec![[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            normals: None,
            uv0: None,
            uv1: None,
            indices: vec![0, 1, 2, 0, 1, 2],
            batches: vec![batch(0, 3, 0), batch(3, 3, 0)],
            node_names: None,
            animation: Some(ModelAnimation {
                keys: vec![
                    AnimKey24 {
                        time: AnimationTime(0.0),
                        pose: Pose::default(),
                    },
                    AnimKey24 {
                        time: AnimationTime(0.0),
                        pose: Pose {
                            translation: [10.0, 0.0, 0.0],
                            rotation: [0.0, 0.0, 0.0, 1.0],
                        },
                    },
                ],
                frame_map: Vec::new(),
                frame_count: 0,
            }),
        };

        let mesh = project_msh_to_static_mesh_in_world_space_with_node_fallback_poses(
            &source,
            LegacyIron3dEulerTransform {
                translation: [100.0, 0.0, 0.0],
                orientation_radians: [0.0; 3],
            },
            [1.0; 3],
        )
        .expect("fallback-pose static mesh");

        assert_eq!(mesh.vertices[0].position, [101.0, 0.0, 0.0]);
        assert_eq!(mesh.vertices[3].position, [111.0, 0.0, 0.0]);
        assert_eq!(mesh.indices, vec![0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn mounted_socket_pose_strips_only_a_replaced_parent_root() {
        let mut nodes = vec![0_u8; 76];
        nodes[2..4].copy_from_slice(&u16::MAX.to_le_bytes());
        nodes[6..8].copy_from_slice(&0_u16.to_le_bytes());
        nodes[38 + 2..38 + 4].copy_from_slice(&0_u16.to_le_bytes());
        nodes[38 + 6..38 + 8].copy_from_slice(&1_u16.to_le_bytes());
        let source = ModelAsset {
            node_stride: 38,
            node_count: 2,
            nodes_raw: nodes,
            bounding_sphere_center: [0.0; 3],
            bounding_sphere_radius: 0.0,
            slots: Vec::new(),
            positions: Vec::new(),
            normals: None,
            uv0: None,
            uv1: None,
            indices: Vec::new(),
            batches: Vec::new(),
            node_names: None,
            animation: Some(ModelAnimation {
                keys: vec![
                    AnimKey24 {
                        time: AnimationTime(0.0),
                        pose: Pose {
                            translation: [5.0, 0.0, 0.0],
                            rotation: [0.0, 0.0, 0.0, 1.0],
                        },
                    },
                    AnimKey24 {
                        time: AnimationTime(0.0),
                        pose: Pose {
                            translation: [2.0, 0.0, 0.0],
                            rotation: [0.0, 0.0, 0.0, 1.0],
                        },
                    },
                ],
                frame_map: Vec::new(),
                frame_count: 0,
            }),
        };

        assert_eq!(
            node38_pose(&source, None, 1)
                .expect("full socket")
                .translation,
            [7.0, 0.0, 0.0]
        );
        assert_eq!(
            node38_pose_relative_to_root(&source, None, 1)
                .expect("relative socket")
                .translation,
            [2.0, 0.0, 0.0]
        );
    }

    #[test]
    fn mounted_child_uses_precomputed_hierarchies_for_socket_and_geometry() {
        let parent_hierarchy = NodePoseBuffer {
            poses: vec![
                Pose {
                    translation: [20.0, 0.0, 0.0],
                    rotation: [0.0, 0.0, 0.0, 1.0],
                },
                Pose {
                    translation: [25.0, 0.0, 0.0],
                    rotation: [0.0, 0.0, 0.0, 1.0],
                },
            ],
        };
        let parent_socket = node38_pose_relative_to_root_from_hierarchy(&parent_hierarchy, 1)
            .expect("sampled parent socket relative to its mounted root");
        assert_eq!(parent_socket.translation, [5.0, 0.0, 0.0]);
        assert_eq!(
            node38_pose_from_hierarchy(&parent_hierarchy, 1)
                .expect("sampled parent socket")
                .translation,
            [25.0, 0.0, 0.0]
        );

        let mut child_root = vec![0_u8; 38];
        child_root[2..4].copy_from_slice(&u16::MAX.to_le_bytes());
        child_root[6..8].copy_from_slice(&0_u16.to_le_bytes());
        let mut child_node = vec![0_u8; 38];
        child_node[2..4].copy_from_slice(&0_u16.to_le_bytes());
        child_node[6..8].copy_from_slice(&1_u16.to_le_bytes());
        child_node[8..10].copy_from_slice(&1_u16.to_le_bytes());
        let mut nodes = child_root;
        nodes.extend(child_node);
        let child = ModelAsset {
            node_stride: 38,
            node_count: 2,
            nodes_raw: nodes,
            bounding_sphere_center: [0.0; 3],
            bounding_sphere_radius: 0.0,
            slots: vec![
                Slot {
                    tri_start: 0,
                    tri_count: 0,
                    batch_start: 0,
                    batch_count: 1,
                    aabb_min: [0.0; 3],
                    aabb_max: [1.0; 3],
                    sphere_center: [0.0; 3],
                    sphere_radius: 1.0,
                    opaque: [0; 5],
                },
                Slot {
                    tri_start: 0,
                    tri_count: 0,
                    batch_start: 1,
                    batch_count: 1,
                    aabb_min: [0.0; 3],
                    aabb_max: [1.0; 3],
                    sphere_center: [0.0; 3],
                    sphere_radius: 1.0,
                    opaque: [0; 5],
                },
            ],
            positions: vec![[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            normals: None,
            uv0: None,
            uv1: None,
            indices: vec![0, 1, 2, 0, 1, 2],
            batches: vec![batch(0, 3, 0), batch(3, 3, 0)],
            node_names: None,
            animation: Some(ModelAnimation {
                keys: vec![AnimKey24 {
                    time: AnimationTime(0.0),
                    pose: Pose::default(),
                }],
                frame_map: Vec::new(),
                frame_count: 0,
            }),
        };
        let child_hierarchy = NodePoseBuffer {
            poses: vec![
                Pose {
                    translation: [10.0, 0.0, 0.0],
                    rotation: [0.0, 0.0, 0.0, 1.0],
                },
                Pose {
                    translation: [14.0, 0.0, 0.0],
                    rotation: [0.0, 0.0, 0.0, 1.0],
                },
            ],
        };
        let child_socket = node38_pose_relative_to_root_from_hierarchy(&child_hierarchy, 1)
            .expect("child node relative to its mounted root");
        assert_eq!(child_socket.translation, [4.0, 0.0, 0.0]);

        // The parent component's world root is at x=100, so its sampled socket
        // at x=5 places the child root at x=105. The child geometry uses the
        // same child hierarchy as the socket path: root x=10 is stripped and
        // node 1 contributes x=4 before the source vertex at x=1.
        let mesh = project_msh_to_static_mesh_in_world_space_with_node_pose_buffer_and_mount(
            &child,
            LegacyIron3dEulerTransform {
                translation: [0.0; 3],
                orientation_radians: [0.0; 3],
            },
            [1.0; 3],
            &child_hierarchy,
            VulkanNodePose {
                translation: [105.0, 0.0, 0.0],
                rotation: [0.0, 0.0, 0.0, 1.0],
            },
        )
        .expect("mounted child geometry from sampled pose buffer");
        assert_eq!(mesh.vertices[0].position, [110.0, 0.0, 0.0]);
        assert_eq!(mesh.indices, vec![0, 1, 2]);
    }

    #[test]
    fn supplied_node_pose_buffer_must_match_model_node_count() {
        let source = model(Vec::new(), Vec::new(), Vec::new());
        let error = project_msh_to_static_mesh_in_world_space_with_node_pose_buffer(
            &source,
            LegacyIron3dEulerTransform {
                translation: [0.0; 3],
                orientation_radians: [0.0; 3],
            },
            [1.0; 3],
            &NodePoseBuffer {
                poses: vec![Pose::default()],
            },
        )
        .expect_err("mismatched node pose count");
        assert_eq!(
            error,
            VulkanAssetMeshError::NodePoseCountMismatch {
                expected: 0,
                actual: 1,
            }
        );
    }

    #[test]
    fn node_pose_projection_preserves_an_intentionally_empty_component() {
        let mut node = vec![0_u8; 38];
        node[2..4].copy_from_slice(&u16::MAX.to_le_bytes());
        node[6..8].copy_from_slice(&0_u16.to_le_bytes());
        node[8..10].copy_from_slice(&u16::MAX.to_le_bytes());
        let source = ModelAsset {
            node_stride: 38,
            node_count: 1,
            nodes_raw: node,
            bounding_sphere_center: [0.0; 3],
            bounding_sphere_radius: 0.0,
            slots: Vec::new(),
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            normals: None,
            uv0: None,
            uv1: None,
            indices: vec![0, 1, 2],
            batches: vec![batch(0, 3, 0)],
            node_names: None,
            animation: Some(ModelAnimation {
                keys: vec![AnimKey24 {
                    time: AnimationTime(0.0),
                    pose: Pose::default(),
                }],
                frame_map: Vec::new(),
                frame_count: 0,
            }),
        };

        let mesh = project_msh_to_static_mesh_in_world_space_with_node_fallback_poses(
            &source,
            LegacyIron3dEulerTransform {
                translation: [10.0, 20.0, 30.0],
                orientation_radians: [0.0; 3],
            },
            [1.0; 3],
        )
        .expect("empty node component remains representable");
        assert!(mesh.vertices.is_empty());
        assert!(mesh.indices.is_empty());
        assert!(mesh.draw_ranges.is_empty());
    }

    #[test]
    fn rotates_point_by_unit_quaternion() {
        let rotated = rotate_by_quaternion(
            [1.0, 0.0, 0.0],
            [
                0.0,
                0.0,
                std::f32::consts::FRAC_1_SQRT_2,
                std::f32::consts::FRAC_1_SQRT_2,
            ],
        );
        assert!((rotated[0]).abs() < 0.0001);
        assert!((rotated[1] - 1.0).abs() < 0.0001);
        assert!((rotated[2]).abs() < 0.0001);
    }

    #[test]
    fn retains_each_source_batch_material_selector() {
        let mut second = batch(3, 3, 0);
        second.material_index = 7;
        let mesh = project_msh_to_static_mesh(&model(
            vec![
                [-2.0, 0.0, -1.0],
                [2.0, 0.0, -1.0],
                [-2.0, 0.0, 3.0],
                [2.0, 0.0, 3.0],
            ],
            vec![0, 1, 2, 1, 3, 2],
            vec![batch(0, 3, 0), second],
        ))
        .expect("representable MSH");

        assert_eq!(
            mesh.draw_ranges,
            vec![
                VulkanStaticDrawRange {
                    first_index: 0,
                    index_count: 3,
                    material_index: 0,
                    lightmap_index: 0,
                    batch_flags: 0,
                    pipeline_state: world_pipeline_state(),
                    alpha_test_reference: 0,
                },
                VulkanStaticDrawRange {
                    first_index: 3,
                    index_count: 3,
                    material_index: 7,
                    lightmap_index: 0,
                    batch_flags: 0,
                    pipeline_state: world_pipeline_state(),
                    alpha_test_reference: 0,
                },
            ]
        );
    }

    #[test]
    fn projects_land_faces_in_source_order_with_packed_uv0() {
        let terrain = LandMeshDocument {
            streams: Vec::new(),
            nodes_raw: Vec::new(),
            slots: TerrainSlotTable {
                header_raw: Vec::new(),
                slots_raw: Vec::new(),
            },
            positions: vec![
                [-2.0, -1.0, 5.0],
                [2.0, -1.0, 3.0],
                [-2.0, 3.0, 9.0],
                [2.0, 3.0, 1.0],
            ],
            normals: Vec::new(),
            uv0: vec![[1024, 65024], [0, 2048], [64512, 512], [512, 0]],
            accelerator: Vec::new(),
            aux14: Vec::new(),
            aux18: Vec::new(),
            faces: vec![terrain_face([0, 1, 2]), terrain_face([1, 3, 2])],
        };

        let mesh = project_land_msh_to_static_mesh(&terrain).expect("representable terrain");

        assert_eq!(mesh.indices, vec![0, 1, 2, 1, 3, 2]);
        assert_eq!(mesh.draw_ranges.len(), 1);
        assert_eq!(mesh.draw_ranges[0].index_count, 6);
        assert_eq!(mesh.vertices[0].position, [-0.8, -0.8, 0.0]);
        assert_eq!(mesh.vertices[3].position, [0.8, 0.8, 0.0]);
        assert_eq!(mesh.vertices[0].uv, [1.0, 63.5]);
        assert_eq!(mesh.vertices[2].uv, [63.0, 0.5]);
    }

    #[test]
    fn terrain_normals_keep_native_one_over_128_scale() {
        let terrain = LandMeshDocument {
            streams: Vec::new(),
            nodes_raw: Vec::new(),
            slots: TerrainSlotTable {
                header_raw: Vec::new(),
                slots_raw: Vec::new(),
            },
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            normals: vec![[127, -128, 64, 0]; 3],
            uv0: vec![[0, 0]; 3],
            accelerator: Vec::new(),
            aux14: Vec::new(),
            aux18: Vec::new(),
            faces: vec![terrain_face([0, 1, 2])],
        };

        let mesh = project_land_msh_to_static_mesh_in_world_space(&terrain)
            .expect("representable terrain normals");
        assert_eq!(mesh.vertices[0].normal, [127.0 / 128.0, -1.0, 0.5]);
    }

    #[test]
    fn world_space_terrain_preserves_source_coordinates_and_faces() {
        let terrain = LandMeshDocument {
            streams: Vec::new(),
            nodes_raw: Vec::new(),
            slots: TerrainSlotTable {
                header_raw: Vec::new(),
                slots_raw: Vec::new(),
            },
            positions: vec![[10.0, 20.0, 30.0], [40.0, 50.0, 60.0], [70.0, 80.0, 90.0]],
            normals: Vec::new(),
            uv0: vec![[1024, 65024], [0, 2048], [64512, 512]],
            accelerator: Vec::new(),
            aux14: Vec::new(),
            aux18: Vec::new(),
            faces: vec![terrain_face([2, 0, 1])],
        };

        let mesh = project_land_msh_to_static_mesh_in_world_space(&terrain)
            .expect("representable source-space terrain");

        assert_eq!(mesh.indices, vec![2, 0, 1]);
        assert_eq!(mesh.vertices[0].position, [10.0, 20.0, 30.0]);
        assert_eq!(mesh.vertices[2].position, [70.0, 80.0, 90.0]);
        assert_eq!(mesh.vertices[0].uv, [1.0, 63.5]);
    }

    #[test]
    fn legacy_world_space_terrain_scales_only_stored_height() {
        let terrain = LandMeshDocument {
            streams: Vec::new(),
            nodes_raw: Vec::new(),
            slots: TerrainSlotTable {
                header_raw: Vec::new(),
                slots_raw: Vec::new(),
            },
            positions: vec![
                [100.0, 200.0, 96.0],
                [300.0, 400.0, 288.0],
                [500.0, 600.0, 192.0],
            ],
            normals: Vec::new(),
            uv0: vec![[0, 0]; 3],
            accelerator: Vec::new(),
            aux14: Vec::new(),
            aux18: Vec::new(),
            faces: vec![terrain_face([0, 1, 2])],
        };

        let mesh = project_land_msh_to_static_mesh_in_legacy_world_space(&terrain)
            .expect("representable terrain");

        assert_eq!(mesh.vertices[0].position, [100.0, 200.0, 3.0]);
        assert_eq!(mesh.vertices[1].position, [300.0, 400.0, 9.0]);
        assert_eq!(mesh.vertices[2].position, [500.0, 600.0, 6.0]);
    }

    fn terrain_face(vertices: [u16; 3]) -> TerrainFace28 {
        terrain_face_with_tag(0, vertices)
    }

    fn terrain_face_with_tag(material_tag: u16, vertices: [u16; 3]) -> TerrainFace28 {
        TerrainFace28 {
            flags: FullSurfaceMask(0),
            material_tag,
            aux_tag: 0,
            vertices,
            neighbors: [None; 3],
            tail_raw: [0; 8],
            raw: [0; 28],
        }
    }

    #[test]
    fn terrain_ranges_preserve_face_order_and_packed_material_tags() {
        let terrain = LandMeshDocument {
            streams: Vec::new(),
            nodes_raw: Vec::new(),
            slots: TerrainSlotTable {
                header_raw: Vec::new(),
                slots_raw: Vec::new(),
            },
            positions: vec![[0.0, 0.0, 0.0]; 3],
            normals: Vec::new(),
            uv0: vec![[0, 0]; 3],
            accelerator: Vec::new(),
            aux14: Vec::new(),
            aux18: Vec::new(),
            faces: vec![
                terrain_face_with_tag(0xff01, [0, 1, 2]),
                terrain_face_with_tag(0xff01, [0, 1, 2]),
                terrain_face_with_tag(0x0102, [0, 1, 2]),
                terrain_face_with_tag(0xff01, [0, 1, 2]),
            ],
        };

        let ranges = static_terrain_draw_ranges(&terrain).expect("representable terrain");

        assert_eq!(ranges.len(), 3);
        assert_eq!(ranges[0].first_index, 0);
        assert_eq!(ranges[0].index_count, 6);
        assert_eq!(ranges[0].material_index, 0xff01);
        assert_eq!(ranges[1].first_index, 6);
        assert_eq!(ranges[1].index_count, 3);
        assert_eq!(ranges[1].material_index, 0x0102);
        assert_eq!(ranges[2].first_index, 9);
        assert_eq!(ranges[2].index_count, 3);
        assert_eq!(ranges[2].material_index, 0xff01);
    }
}
