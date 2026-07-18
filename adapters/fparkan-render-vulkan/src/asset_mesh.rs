//! Format-to-GPU geometry bridge for the initial static asset renderer.

use crate::{VulkanStaticDrawRange, VulkanStaticMesh, VulkanStaticVertex};
use fparkan_msh::ModelAsset;
use fparkan_render::{LegacyIron3dEulerTransform, LegacyPipelineState};
use fparkan_terrain_format::LandMeshDocument;

/// Legacy `Land.msh` stored-height to mission-world-height scale.
const LEGACY_LAND_HEIGHT_SCALE: f32 = 1.0 / 32.0;

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
                color: [0.82, 0.72, 0.31],
                // Iron3D stores Res5 UV0 as signed fixed point with 1/1024 units.
                // Models that omit this optional stream retain the static viewer's
                // XY planar fallback instead of receiving fabricated raw UV values.
                uv: model
                    .uv0
                    .as_ref()
                    .and_then(|uv0| uv0.get(index))
                    .map_or(frame.planar_uv(transformed), |uv| {
                        [f32::from(uv[0]) / 1024.0, f32::from(uv[1]) / 1024.0]
                    }),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(VulkanStaticMesh {
        vertices,
        indices,
        draw_ranges,
    })
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
            let position = transform
                .try_transform_scaled_point(*position, scale)
                .ok_or(VulkanAssetMeshError::NonFinitePosition)?;
            Ok(VulkanStaticVertex {
                position,
                color: [0.82, 0.72, 0.31],
                uv: model
                    .uv0
                    .as_ref()
                    .and_then(|uv0| uv0.get(index))
                    .map_or([0.0, 0.0], |uv| {
                        [f32::from(uv[0]) / 1024.0, f32::from(uv[1]) / 1024.0]
                    }),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(VulkanStaticMesh {
        vertices,
        indices,
        draw_ranges,
    })
}

/// Projects validated `Land.msh` terrain geometry into the static Vulkan path.
///
/// This bridge preserves the source triangle order from `TerrainFace28` and
/// consumes its validated position and UV0 streams. It is deliberately a
/// geometry-only terrain slice: source slot/material selection, camera
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
                color: [0.31, 0.58, 0.27],
                uv: terrain
                    .uv0
                    .get(index)
                    .map_or(frame.planar_uv(*position), |uv| {
                        [f32::from(uv[0]) / 1024.0, f32::from(uv[1]) / 1024.0]
                    }),
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
                color: [0.31, 0.58, 0.27],
                uv: terrain.uv0.get(index).map_or([0.0, 0.0], |uv| {
                    [f32::from(uv[0]) / 1024.0, f32::from(uv[1]) / 1024.0]
                }),
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

fn static_terrain_draw_ranges(
    terrain: &LandMeshDocument,
) -> Result<Vec<VulkanStaticDrawRange>, VulkanAssetMeshError> {
    Ok(vec![VulkanStaticDrawRange {
        first_index: 0,
        index_count: u32::try_from(terrain.faces.len())
            .map_err(|_| VulkanAssetMeshError::IndexOutOfRange)?
            .checked_mul(3)
            .ok_or(VulkanAssetMeshError::IndexOutOfRange)?,
        material_index: 0,
        pipeline_state: LegacyPipelineState::default(),
        alpha_test_reference: 0,
    }])
}

fn static_model_indices_and_ranges(
    model: &ModelAsset,
) -> Result<(Vec<u32>, Vec<VulkanStaticDrawRange>), VulkanAssetMeshError> {
    let mut indices = Vec::new();
    let mut draw_ranges = Vec::with_capacity(model.batches.len());
    for batch in &model.batches {
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
            material_index: batch.material_index,
            pipeline_state: LegacyPipelineState::default(),
            alpha_test_reference: 0,
        });
    }
    if indices.is_empty() {
        return Err(VulkanAssetMeshError::EmptyGeometry);
    }
    Ok((indices, draw_ranges))
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
    use fparkan_msh::{Batch, ModelAsset};
    use fparkan_terrain_format::{FullSurfaceMask, TerrainFace28, TerrainSlotTable};

    fn model(positions: Vec<[f32; 3]>, indices: Vec<u16>, batches: Vec<Batch>) -> ModelAsset {
        ModelAsset {
            node_stride: 0,
            node_count: 0,
            nodes_raw: Vec::new(),
            slots: Vec::new(),
            positions,
            normals: None,
            uv0: None,
            indices,
            batches,
            node_names: None,
        }
    }

    fn batch(index_start: u32, index_count: u16, base_vertex: u32) -> Batch {
        Batch {
            batch_flags: 0,
            material_index: 0,
            opaque4: 0,
            opaque6: 0,
            index_count,
            index_start,
            opaque14: 0,
            base_vertex,
        }
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
                pipeline_state: LegacyPipelineState::default(),
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
        source.uv0 = Some(vec![[1024, -512], [0, 2048], [-1024, 512]]);

        let mesh = project_msh_to_static_mesh(&source).expect("representable MSH");

        assert_eq!(mesh.vertices[0].uv, [1.0, -0.5]);
        assert_eq!(mesh.vertices[1].uv, [0.0, 2.0]);
        assert_eq!(mesh.vertices[2].uv, [-1.0, 0.5]);
    }

    #[test]
    fn world_space_msh_preserves_z_and_applies_only_decoded_translation_scale() {
        let mut source = model(
            vec![[2.0, -3.0, 4.0], [0.0, 1.0, 2.0], [-1.0, 2.0, 3.0]],
            vec![0, 1, 2],
            vec![batch(0, 3, 0)],
        );
        source.uv0 = Some(vec![[1024, -512], [0, 2048], [-1024, 512]]);

        let mesh = project_msh_to_static_mesh_in_world_space(
            &source,
            [100.0, 200.0, 300.0],
            [2.0, -1.0, 0.5],
        )
        .expect("representable source-space MSH");

        assert_eq!(mesh.indices, vec![0, 1, 2]);
        assert_eq!(mesh.vertices[0].position, [104.0, 203.0, 302.0]);
        assert_eq!(mesh.vertices[1].position, [100.0, 199.0, 301.0]);
        assert_eq!(mesh.vertices[0].uv, [1.0, -0.5]);
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
                    pipeline_state: LegacyPipelineState::default(),
                    alpha_test_reference: 0,
                },
                VulkanStaticDrawRange {
                    first_index: 3,
                    index_count: 3,
                    material_index: 7,
                    pipeline_state: LegacyPipelineState::default(),
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
            uv0: vec![[1024, -512], [0, 2048], [-1024, 512], [512, 0]],
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
        assert_eq!(mesh.vertices[0].uv, [1.0, -0.5]);
        assert_eq!(mesh.vertices[2].uv, [-1.0, 0.5]);
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
            uv0: vec![[1024, -512], [0, 2048], [-1024, 512]],
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
        assert_eq!(mesh.vertices[0].uv, [1.0, -0.5]);
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
        TerrainFace28 {
            flags: FullSurfaceMask(0),
            material_tag: 0,
            aux_tag: 0,
            vertices,
            neighbors: [None; 3],
            tail_raw: [0; 8],
            raw: [0; 28],
        }
    }
}
