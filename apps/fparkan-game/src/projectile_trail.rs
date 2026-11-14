use super::{
    ambient_effect_pipeline_state, append_environment_range, environment_vertex,
    fixed_effect_field, prepare_effect_material, quad_indices, EnvironmentGpuRange,
    PreparedEffectMaterial, StaticPreviewScene,
};
use fparkan_assets::{load_resource_entry_bytes_from_root, PreparedControlEffectSection};
use fparkan_fx::{decode_fxid, op5::TrailLayer, FxOpcode};
use fparkan_runtime::ProjectileAssets;
use std::{collections::HashMap, path::Path};

pub(super) struct ProjectileTrail {
    layer: TrailLayer,
    pub(super) range: EnvironmentGpuRange,
    pub(super) visible: bool,
}

pub(super) fn prepare(
    root: &Path,
    projectile: &ProjectileAssets,
    name: &str,
    preview: &mut StaticPreviewScene,
    cache: &mut HashMap<String, PreparedEffectMaterial>,
) -> Vec<(TrailLayer, usize, fparkan_render::LegacyPipelineState)> {
    let Some(control) = projectile.assets.object_component_controls[0][0].as_ref() else {
        return Vec::new();
    };
    let PreparedControlEffectSection::Parsed(rows) = &control.effect_section else {
        return Vec::new();
    };
    let mut layers = Vec::new();
    for row in rows.iter().filter(|row| {
        Some(row.table_index) == control.initial_effect_group && row.raw_condition == [0; 3]
    }) {
        let archive = fixed_effect_field(&row.resource.archive);
        let fx = fixed_effect_field(&row.resource.name);
        let Ok(document) = load_resource_entry_bytes_from_root(root, &archive, &fx)
            .map_err(|e| e.to_string())
            .and_then(|bytes| decode_fxid(bytes).map_err(|e| e.to_string()))
        else {
            eprintln!("sim_coverage projectile_trail_resource_stub=1 projectile={name} effect={archive}/{fx}");
            continue;
        };
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
                eprintln!("sim_coverage projectile_trail_command_stub=1 projectile={name} effect={archive}/{fx}");
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
