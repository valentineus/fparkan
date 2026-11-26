//! Prepare and populate the shared projectile body and trail draw pools.

use super::{
    append_static_preview_component, cross3, fixed_effect_field, normalize3, projectile_sound,
    projectile_trail, shot_sound, static_preview_component_materials, PreparedEffectMaterial,
    StaticPreviewScene,
};
use fparkan_assets::{
    load_resource_entry_bytes_from_root, PreparedControlEffectRow, PreparedControlEffectSection,
};
use fparkan_fx::{ambient_sound::Op2SoundCommand, decode_fxid, FxDocument};
use fparkan_render::LegacyIron3dEulerTransform;
use fparkan_render_vulkan::{
    project_msh_to_static_mesh_in_world_space_with_node_fallback_poses, VulkanStaticMesh,
    VulkanStaticVertex,
};
use fparkan_runtime::{
    autodemo_projectiles, autonomous_projectile_segments, load_player_projectile, Engine,
    PlayerProjectile, ProjectileAssets,
};
use fparkan_world::ObjectHandle;
use std::{collections::HashMap, ops::Range, path::Path};

pub(super) struct ProjectileDraw {
    pub(super) speed: f32,
    pub(super) lifetime_ms: u32,
    pub(super) volume: Option<f32>,
    pub(super) launch_sounds: Vec<Op2SoundCommand>,
    pub(super) engine_sounds: Vec<projectile_sound::EngineSound>,
    pub(super) local_vertices: Vec<VulkanStaticVertex>,
    pub(super) instances: Vec<ProjectileDrawSlot>,
}

pub(super) struct ProjectileDrawSlot {
    pub(super) flight: Option<PlayerProjectile>,
    pub(super) sound_born_ms: Option<u32>,
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
    let effects = initial_effects(root, &projectile, name);
    let layers = projectile_trail::prepare(root, &effects, name, preview, trail_materials);
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
            sound_born_ms: None,
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
        launch_sounds: shot_sound::projectile_launch(effects.iter().map(|effect| &effect.document)),
        engine_sounds: projectile_sound::prepare(&projectile, &effects),
        local_vertices: geometry.vertices,
        instances,
    }))
}

pub(super) struct ProjectileEffect<'a> {
    pub(super) row: &'a PreparedControlEffectRow,
    pub(super) resource: String,
    pub(super) document: FxDocument,
}

pub(super) fn initial_effects<'a>(
    root: &Path,
    projectile: &'a ProjectileAssets,
    name: &str,
) -> Vec<ProjectileEffect<'a>> {
    let Some(control) = projectile.assets.object_component_controls[0][0].as_ref() else {
        return Vec::new();
    };
    let PreparedControlEffectSection::Parsed(rows) = &control.effect_section else {
        return Vec::new();
    };
    rows.iter()
        .filter(|row| {
            row.opcode == 4
                && Some(row.table_index) == control.initial_effect_group
                && row.raw_condition == [0; 3]
        })
        .filter_map(|row| {
            let archive = fixed_effect_field(&row.resource.archive);
            let fx = fixed_effect_field(&row.resource.name);
            let effect = load_resource_entry_bytes_from_root(root, &archive, &fx)
                .map_err(|error| error.to_string())
                .and_then(|bytes| decode_fxid(bytes).map_err(|error| error.to_string()));
            match effect {
                Ok(document) => Some(ProjectileEffect {
                    row,
                    resource: format!("{archive}/{fx}"),
                    document,
                }),
                Err(_) => {
                    eprintln!("sim_coverage projectile_trail_resource_stub=1 projectile={name} effect={archive}/{fx}");
                    None
                }
            }
        })
        .collect()
}

pub(super) fn flight_basis(forward: [f32; 3]) -> [[f32; 3]; 3] {
    let right = normalize3(cross3(forward, [0.0, 0.0, 1.0])).unwrap_or([1.0, 0.0, 0.0]);
    [right, forward, cross3(right, forward)]
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
