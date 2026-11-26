//! Reuse the dead unit's already uploaded component ranges as loose meshes.

use super::{damage_visibility::node_draw_ranges, DynamicComponentGeometry};
use fparkan_animation::Pose;
use fparkan_assets::TerrainWorld;
use fparkan_render_vulkan::{VulkanStaticMesh, VulkanStaticVertex};
use fparkan_runtime::{DebrisMotion, DestroyedMesh};
use fparkan_terrain::SurfaceQuery;
use std::ops::Range;

pub(super) struct MeshFragment {
    center: [f32; 3],
    motion: DebrisMotion,
    vertex_start: usize,
    vertices: Vec<VulkanStaticVertex>,
    pub(super) ranges: Vec<usize>,
}

pub(super) fn capture(
    destroyed: DestroyedMesh,
    origin: [f32; 3],
    geometry: &[Vec<Option<DynamicComponentGeometry>>],
    mesh: &VulkanStaticMesh,
    terrain: &TerrainWorld,
) -> Vec<MeshFragment> {
    match destroyed {
        DestroyedMesh::Mounted { object_index } => {
            capture_mounted(object_index, origin, geometry, mesh, terrain)
        }
        DestroyedMesh::ChassisNode {
            object_index,
            node,
            hull_origin,
        } => {
            let root = geometry
                .get(object_index)
                .and_then(|parts| parts.first())
                .and_then(Option::as_ref);
            root.and_then(|root| capture_node(object_index, node, hull_origin, root, mesh, terrain))
                .into_iter()
                .collect()
        }
    }
}

fn capture_mounted(
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
            let ranges: Vec<_> = mesh
                .draw_ranges
                .iter()
                .enumerate()
                .filter_map(|(i, range)| {
                    let vertex = *mesh.indices.get(range.first_index as usize)? as usize;
                    (range.owner_scope == owner_scope && window.contains(&vertex)).then_some(i)
                })
                .collect();
            capture_fragment(mesh, window, ranges, hull, terrain)
        })
        .collect()
}

fn capture_node(
    object: usize,
    node: usize,
    hull: [f32; 3],
    root: &DynamicComponentGeometry,
    mesh: &VulkanStaticMesh,
    terrain: &TerrainWorld,
) -> Option<MeshFragment> {
    let ranges = node_draw_ranges(mesh, root, object, node)?;
    if ranges.windows(2).any(|pair| pair[1] != pair[0] + 1) {
        return None;
    }
    let first = mesh.draw_ranges.get(*ranges.first()?)?;
    let last = mesh.draw_ranges.get(*ranges.last()?)?;
    let index_end = last.first_index.checked_add(last.index_count)?;
    let indices = mesh
        .indices
        .get(first.first_index as usize..index_end as usize)?;
    let vertex_start = *indices.first()? as usize;
    let vertex_end = vertex_start.checked_add(indices.len())?;
    let window = vertex_start..vertex_end;
    // The Node38 projector expands the selected batches in triangle order.
    if !indices
        .iter()
        .map(|&index| index as usize)
        .eq(window.clone())
    {
        return None;
    }
    capture_fragment(mesh, window, ranges, hull, terrain)
}

fn capture_fragment(
    mesh: &VulkanStaticMesh,
    window: Range<usize>,
    ranges: Vec<usize>,
    hull: [f32; 3],
    terrain: &TerrainWorld,
) -> Option<MeshFragment> {
    if ranges.is_empty() {
        return None;
    }
    let vertex_start = window.start;
    let vertices = mesh.vertices.get(window)?;
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
        vertex_start,
        vertices: vertices.to_vec(),
        ranges,
    })
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
