//! Format-to-GPU geometry bridge for the initial static asset renderer.

use crate::{VulkanStaticDrawRange, VulkanStaticMesh, VulkanStaticVertex};
use fparkan_msh::ModelAsset;
use fparkan_render::LegacyPipelineState;

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

impl std::fmt::Display for VulkanAssetMeshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyGeometry => write!(f, "MSH contains no triangle geometry"),
            Self::NonFinitePosition => write!(f, "MSH contains a non-finite position"),
            Self::DegenerateViewExtent => write!(f, "MSH has a degenerate XZ view extent"),
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
/// projects the conventional `Iron3D` XZ ground plane into the current XY shader
/// input, and scales the used bounds uniformly into the visible clip rectangle.
///
/// # Errors
///
/// Returns [`VulkanAssetMeshError`] if the source cannot be represented by the
/// current 16-bit, triangle-list viewer input.
pub fn project_msh_to_static_mesh(
    model: &ModelAsset,
) -> Result<VulkanStaticMesh, VulkanAssetMeshError> {
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
            indices.push(u16::try_from(index).map_err(|_| VulkanAssetMeshError::IndexOutOfRange)?);
        }
        draw_ranges.push(VulkanStaticDrawRange {
            first_index,
            index_count: u32::from(batch.index_count),
            material_index: batch.material_index,
            pipeline_state: LegacyPipelineState::default(),
        });
    }
    if indices.is_empty() {
        return Err(VulkanAssetMeshError::EmptyGeometry);
    }

    let mut min_x = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut min_z = f32::INFINITY;
    let mut max_z = f32::NEG_INFINITY;
    for &index in &indices {
        let position = model
            .positions
            .get(usize::from(index))
            .ok_or(VulkanAssetMeshError::IndexOutOfRange)?;
        if !position.iter().all(|value| value.is_finite()) {
            return Err(VulkanAssetMeshError::NonFinitePosition);
        }
        min_x = min_x.min(position[0]);
        max_x = max_x.max(position[0]);
        min_z = min_z.min(position[2]);
        max_z = max_z.max(position[2]);
    }
    let extent = (max_x - min_x).max(max_z - min_z);
    if !extent.is_finite() || extent <= f32::EPSILON {
        return Err(VulkanAssetMeshError::DegenerateViewExtent);
    }
    let center_x = (min_x + max_x) * 0.5;
    let center_z = (min_z + max_z) * 0.5;
    let scale = 1.6 / extent;
    let vertices = model
        .positions
        .iter()
        .enumerate()
        .map(|(index, position)| VulkanStaticVertex {
            position: [
                (position[0] - center_x) * scale,
                (position[2] - center_z) * scale,
            ],
            color: [0.82, 0.72, 0.31],
            // Iron3D stores Res5 UV0 as signed fixed point with 1/1024 units.
            // Models that omit this optional stream retain the static viewer's
            // XZ planar fallback instead of receiving fabricated raw UV values.
            uv: model.uv0.as_ref().and_then(|uv0| uv0.get(index)).map_or(
                [
                    (position[0] - center_x) / extent + 0.5,
                    (position[2] - center_z) / extent + 0.5,
                ],
                |uv| [f32::from(uv[0]) / 1024.0, f32::from(uv[1]) / 1024.0],
            ),
        })
        .collect();

    Ok(VulkanStaticMesh {
        vertices,
        indices,
        draw_ranges,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_msh::{Batch, ModelAsset};

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
    fn projects_xz_geometry_and_applies_base_vertex() {
        let mesh = project_msh_to_static_mesh(&model(
            vec![
                [99.0, 0.0, 99.0],
                [-2.0, 4.0, -1.0],
                [2.0, 8.0, -1.0],
                [-2.0, 1.0, 3.0],
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
            }]
        );
        assert_eq!(mesh.vertices[1].position, [-0.8, -0.8]);
        assert_eq!(mesh.vertices[2].position, [0.8, -0.8]);
        assert_eq!(mesh.vertices[3].position, [-0.8, 0.8]);
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
                },
                VulkanStaticDrawRange {
                    first_index: 3,
                    index_count: 3,
                    material_index: 7,
                    pipeline_state: LegacyPipelineState::default(),
                },
            ]
        );
    }
}
