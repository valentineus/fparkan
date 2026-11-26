//! Prepare and populate the shared projectile body and trail draw pools.

use super::{
    append_static_preview_component, projectile_trail, static_preview_component_materials,
    PreparedEffectMaterial, StaticPreviewScene,
};
use fparkan_render::LegacyIron3dEulerTransform;
use fparkan_render_vulkan::{
    project_msh_to_static_mesh_in_world_space_with_node_fallback_poses, VulkanStaticMesh,
    VulkanStaticVertex,
};
use fparkan_runtime::{
    autodemo_projectiles, autonomous_projectile_segments, load_player_projectile, Engine,
    PlayerProjectile,
};
use fparkan_world::ObjectHandle;
use std::{collections::HashMap, ops::Range, path::Path};

pub(super) struct ProjectileDraw {
    pub(super) speed: f32,
    pub(super) lifetime_ms: u32,
    pub(super) volume: Option<f32>,
    pub(super) local_vertices: Vec<VulkanStaticVertex>,
    pub(super) instances: Vec<ProjectileDrawSlot>,
}

pub(super) struct ProjectileDrawSlot {
    pub(super) flight: Option<PlayerProjectile>,
    pub(super) shooter: Option<ObjectHandle>,
    pub(super) vertex_start: usize,
    pub(super) draw_ranges: Range<usize>,
    pub(super) trails: Vec<projectile_trail::ProjectileTrail>,
    pub(super) trail_segment: Option<[[f32; 3]; 2]>,
}

pub(super) fn prepare_projectile_draw(
    root: &Path,
    engine: &Engine,
    archive: &str,
    name: &str,
    capacity: impl FnOnce(u32) -> u32,
    preview: &mut StaticPreviewScene,
    trail_materials: &mut HashMap<String, PreparedEffectMaterial>,
) -> Result<Option<ProjectileDraw>, String> {
    let projectile = load_player_projectile(engine, archive, name)?;
    let assets = &projectile.assets;
    let [id] = assets.visuals_for_object(0) else {
        return Err("projectile has no single visual".into());
    };
    let visual = assets
        .visual_by_id(*id)
        .ok_or("projectile visual unavailable")?;
    let geometry = if let Some(model) = visual.model_id.and_then(|id| assets.model_by_id(id)) {
        project_msh_to_static_mesh_in_world_space_with_node_fallback_poses(
            &model.validated,
            LegacyIron3dEulerTransform {
                translation: [0.0; 3],
                orientation_radians: [0.0; 3],
            },
            [1.0; 3],
        )
        .map_err(|error| error.to_string())?
    } else {
        VulkanStaticMesh {
            vertices: Vec::new(),
            indices: Vec::new(),
            draw_ranges: Vec::new(),
        }
    };
    let layers = projectile_trail::prepare(root, &projectile, name, preview, trail_materials);
    if geometry.draw_ranges.is_empty() && layers.is_empty() {
        return Ok(None);
    }
    let selector_remap = static_preview_component_materials(
        assets,
        visual,
        &geometry,
        None,
        &mut preview.materials,
        &mut preview.material_animations,
    )?;
    let capacity = capacity(projectile.lifetime_ms).min(128);
    let mut instances = Vec::new();
    for _ in 0..capacity {
        let vertex_start = preview.mesh.vertices.len();
        let range_start = preview.mesh.draw_ranges.len();
        append_static_preview_component(
            &mut preview.mesh,
            geometry.clone(),
            &selector_remap,
            Some(&mut preview.material_animations),
        )?;
        let draw_ranges = range_start..preview.mesh.draw_ranges.len();
        let trails = layers
            .iter()
            .map(|&layer| projectile_trail::ProjectileTrail::allocate(&mut preview.mesh, layer))
            .collect::<Result<_, _>>()?;
        instances.push(ProjectileDrawSlot {
            flight: None,
            shooter: None,
            vertex_start,
            draw_ranges,
            trails,
            trail_segment: None,
        });
    }
    Ok(Some(ProjectileDraw {
        speed: projectile.speed,
        lifetime_ms: projectile.lifetime_ms,
        volume: projectile.volume,
        local_vertices: geometry.vertices,
        instances,
    }))
}

pub(super) fn sample_autonomous(draws: &mut [Option<ProjectileDraw>], engine: &Engine) {
    for instance in draws
        .iter_mut()
        .flatten()
        .flat_map(|draw| &mut draw.instances)
    {
        instance.flight = None;
        instance.trail_segment = None;
    }
    for (slot, flight) in autodemo_projectiles(engine) {
        if let Some(draw) = draws.get_mut(slot).and_then(Option::as_mut) {
            if let Some(instance) = draw
                .instances
                .iter_mut()
                .find(|instance| instance.flight.is_none())
            {
                instance.flight = Some(flight);
            }
        }
    }
    // Pack trails independently of bodies. A body can be removed on impact
    // while its last visible segment still belongs to this frame.
    for (slot, segment) in autonomous_projectile_segments(engine) {
        if let Some(draw) = draws.get_mut(slot).and_then(Option::as_mut) {
            if let Some(instance) = draw
                .instances
                .iter_mut()
                .find(|instance| instance.trail_segment.is_none() && !instance.trails.is_empty())
            {
                instance.trail_segment = Some(segment);
            }
        }
    }
}
