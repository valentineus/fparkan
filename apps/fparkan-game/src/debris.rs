//! Reuse the dead unit's already uploaded component ranges as loose meshes.

use super::DynamicComponentGeometry;
use fparkan_animation::Pose;
use fparkan_assets::TerrainWorld;
use fparkan_render_vulkan::{VulkanStaticMesh, VulkanStaticVertex};
use fparkan_runtime::DebrisMotion;
use fparkan_terrain::SurfaceQuery;

pub(super) struct MeshFragment {
    center: [f32; 3],
    motion: DebrisMotion,
    vertex_start: usize,
    vertices: Vec<VulkanStaticVertex>,
    pub(super) ranges: Vec<usize>,
}

pub(super) fn capture(
    object_index: usize,
    hull: [f32; 3],
    geometry: &[Vec<Option<DynamicComponentGeometry>>],
    mesh: &VulkanStaticMesh,
    terrain: &TerrainWorld,
) -> Vec<MeshFragment> {
    let Some(components) = geometry.get(object_index) else {
        return Vec::new();
    };
    let Ok(owner_scope) = u32::try_from(object_index + 1) else {
        return Vec::new();
    };
    components
        .iter()
        .skip(1)
        .flatten()
        .filter_map(|component| {
            let window = component.vertex_start..component.vertex_start + component.vertex_count;
            let vertices = mesh.vertices.get(window.clone())?;
            let ranges: Vec<_> = mesh
                .draw_ranges
                .iter()
                .enumerate()
                .filter_map(|(i, range)| {
                    let vertex = *mesh.indices.get(range.first_index as usize)? as usize;
                    (range.owner_scope == owner_scope && window.contains(&vertex)).then_some(i)
                })
                .collect();
            if ranges.is_empty() {
                return None;
            }
            let mut min = [f32::INFINITY; 3];
            let mut max = [f32::NEG_INFINITY; 3];
            for vertex in vertices {
                for i in 0..3 {
                    min[i] = min[i].min(vertex.position[i]);
                    max[i] = max[i].max(vertex.position[i]);
                }
            }
            let center = std::array::from_fn(|i| (min[i] + max[i]) * 0.5);
            let radius = (0..3)
                .map(|i| (max[i] - center[i]).powi(2))
                .sum::<f32>()
                .sqrt();
            let floor = terrain
                .height_at([center[0], center[1]])
                .ok()
                .flatten()
                .unwrap_or(hull[2]);
            let motion = DebrisMotion::new(center, hull, radius, floor + max[2] - center[2])?;
            Some(MeshFragment {
                center,
                motion,
                vertex_start: component.vertex_start,
                vertices: vertices.to_vec(),
                ranges,
            })
        })
        .collect()
}

impl MeshFragment {
    pub(super) fn draw(&self, elapsed: f32, mesh: &mut VulkanStaticMesh, terrain: &TerrainWorld) {
        let pose = self.motion.pose(elapsed);
        let [x, y, _] = pose.translation;
        let rotate = Pose {
            translation: [0.0; 3],
            ..pose
        };
        let vertices = &mut mesh.vertices[self.vertex_start..][..self.vertices.len()];
        for (dst, src) in vertices.iter_mut().zip(&self.vertices) {
            *dst = *src;
            dst.position =
                pose.transform_point(std::array::from_fn(|i| src.position[i] - self.center[i]));
            dst.normal = rotate.transform_point(src.normal);
        }
        if let Ok(Some(floor)) = terrain.height_at([x, y]) {
            let bottom = vertices
                .iter()
                .map(|vertex| vertex.position[2])
                .fold(f32::INFINITY, f32::min);
            let lift = (floor - bottom).max(0.0);
            for vertex in vertices {
                vertex.position[2] += lift;
            }
        }
    }
}
