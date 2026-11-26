use super::{
    ambient_effect_pipeline_state, append_environment_range, environment_vertex,
    prepare_effect_material, projectile_render::ProjectileEffect, quad_indices,
    EnvironmentGpuRange, PreparedEffectMaterial, StaticPreviewScene,
};
use fparkan_fx::{op5::TrailLayer, FxOpcode};
use std::{collections::HashMap, path::Path};

pub(super) struct ProjectileTrail {
    layer: TrailLayer,
    pub(super) range: EnvironmentGpuRange,
    pub(super) visible: bool,
}

pub(super) fn prepare(
    root: &Path,
    effects: &[ProjectileEffect<'_>],
    name: &str,
    preview: &mut StaticPreviewScene,
    cache: &mut HashMap<String, PreparedEffectMaterial>,
) -> Vec<(TrailLayer, usize, fparkan_render::LegacyPipelineState)> {
    let mut layers = Vec::new();
    for ProjectileEffect {
        resource: effect,
        document,
        ..
    } in effects
    {
        let header = document.header();
        let [scale, sy, sz] = header.scale;
        for command in document
            .commands()
            .iter()
            .filter(|command| command.opcode == FxOpcode::Op5)
        {
            let prepared = (|| {
                if header.time_mode != 0 || scale != sy || scale != sz {
                    return None;
                }
                let layer = TrailLayer::from_command(command, scale)?;
                let reference = command.resource_refs.first()?;
                let material = prepare_effect_material(
                    root,
                    std::str::from_utf8(reference.archive_name()).ok()?,
                    std::str::from_utf8(reference.resource_name()).ok()?,
                    &mut preview.materials,
                    cache,
                )
                .ok()?;
                Some((
                    layer,
                    *material.phase_material_indices.first()?,
                    ambient_effect_pipeline_state(material.initial_pipeline_category)?,
                ))
            })();
            if let Some(prepared) = prepared {
                layers.push(prepared);
            } else {
                eprintln!("sim_coverage projectile_trail_command_stub=1 projectile={name} effect={effect}");
            }
        }
    }
    if !layers.is_empty() {
        eprintln!(
            "sim_coverage projectile_trail_approx={} projectile={name}",
            layers.len()
        );
    }
    layers
}

impl ProjectileTrail {
    pub(super) fn allocate(
        mesh: &mut super::VulkanStaticMesh,
        (layer, material, pipeline): (TrailLayer, usize, fparkan_render::LegacyPipelineState),
    ) -> Result<Self, String> {
        let range = append_environment_range(
            mesh,
            material,
            vec![environment_vertex([0.0; 3], [1.0; 3], [0.0; 2]); 4],
            quad_indices(1)?,
            pipeline,
        )?;
        Ok(Self {
            layer,
            range,
            visible: false,
        })
    }

    pub(super) fn sample(
        &mut self,
        vertices: &mut [super::VulkanStaticVertex],
        segment: Option<[[f32; 3]; 2]>,
        camera_position: [f32; 3],
        camera_forward: [f32; 3],
    ) {
        self.visible = false;
        let Some([start, end]) = segment else { return };
        let direction = std::array::from_fn(|i| (start[i] + end[i]) * 0.5 - camera_position[i]);
        let Some((points, repeats)) = self
            .layer
            .ribbon([start, end], direction)
            .or_else(|| self.layer.ribbon([start, end], camera_forward))
        else {
            return;
        };
        // shortcut: coalesce the native repeated segments into one textured ribbon.
        let uvs = [[0.0, 0.0], [0.0, 1.0], [repeats, 0.0], [repeats, 1.0]];
        let start = self.range.vertex_start;
        for (vertex, (point, uv)) in vertices[start..start + 4]
            .iter_mut()
            .zip(points.into_iter().zip(uvs))
        {
            *vertex = environment_vertex(point, [1.0; 3], uv);
        }
        self.visible = true;
    }
}
