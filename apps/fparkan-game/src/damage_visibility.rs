//! Hide exhausted chassis nodes without changing the uploaded mesh layout.

use super::DynamicComponentGeometry;
use fparkan_animation::NodePoseBuffer;
use fparkan_msh::{draw_batches, selected_slot, Group, Lod, NodeId};
use fparkan_render_vulkan::{VulkanSmokeRenderer, VulkanStaticMesh};
use fparkan_runtime::{depleted_chassis_nodes, Engine};

pub(super) fn apply(
    engine: &Engine,
    geometry: &[Vec<Option<DynamicComponentGeometry>>],
    poses: &[Vec<Option<NodePoseBuffer>>],
    mesh: &VulkanStaticMesh,
    renderer: &mut VulkanSmokeRenderer,
    fragment_is_visible: impl Fn(usize) -> bool,
) -> Result<(), String> {
    for (object, node) in depleted_chassis_nodes(engine) {
        let Some(component) = geometry
            .get(object)
            .and_then(|v| v.first())
            .and_then(Option::as_ref)
        else {
            continue;
        };
        if !poses
            .get(object)
            .and_then(|v| v.first())
            .is_some_and(Option::is_some)
        {
            continue;
        }
        let Some(ranges) = node_draw_ranges(mesh, component, object, node) else {
            continue;
        };
        for range in ranges
            .into_iter()
            .filter(|&range| !fragment_is_visible(range))
        {
            renderer
                .set_draw_range_index_count(range, 0)
                .map_err(|error| format!("hide damaged chassis node {object}:{node}: {error}"))?;
        }
    }
    Ok(())
}

pub(super) fn node_draw_ranges(
    mesh: &VulkanStaticMesh,
    component: &DynamicComponentGeometry,
    object: usize,
    node: usize,
) -> Option<Vec<usize>> {
    if component.model.node_stride != 38 || component.model.animation.is_none() {
        return None;
    }
    let owner = u32::try_from(object.checked_add(1)?).ok()?;
    let end = component.vertex_start.checked_add(component.vertex_count)?;
    let vertices = component.vertex_start..end;
    let mut ranges = mesh.draw_ranges.iter().enumerate().filter(|(_, range)| {
        range.owner_scope == owner
            && mesh
                .indices
                .get(range.first_index as usize)
                .is_some_and(|&vertex| vertices.contains(&(vertex as usize)))
    });
    let model = &component.model;
    let mut matched = Vec::new();
    for index in 0..model.node_count {
        let id = NodeId(u32::try_from(index).ok()?);
        let Some(slot) = selected_slot(model, id, Lod(0), Group(0)) else {
            continue;
        };
        for batch in draw_batches(model, slot)
            .ok()?
            .iter()
            .filter(|batch| batch.index_count != 0)
        {
            let (range_index, range) = ranges.next()?;
            if range.index_count != u32::from(batch.index_count)
                || range.batch_flags != batch.batch_flags
            {
                return None;
            }
            if index == node {
                matched.push(range_index);
            }
        }
    }
    ranges.next().is_none().then_some(matched)
}
