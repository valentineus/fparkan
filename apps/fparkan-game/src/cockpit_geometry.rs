use super::*;
use fparkan_render_vulkan::project_cockpit_msh;
use std::ops::Range;

#[derive(Default)]
pub(super) struct CockpitGeometry {
    bindings: HashMap<(usize, usize), Binding>,
    active: Option<((usize, usize), RawCameraTransform)>,
}

struct Binding {
    vertices: Range<usize>,
    draws: Range<usize>,
    projection: [f32; 3],
}

impl CockpitGeometry {
    pub(super) fn prepare(
        engine: &Engine,
        preview: &mut StaticPreviewScene,
    ) -> Result<Self, String> {
        let Some(assets) = fparkan_runtime::loaded_mission_assets(engine) else {
            return Ok(Self::default());
        };
        let mut geometry = Self::default();
        for (object, controls) in assets.object_component_controls.iter().enumerate() {
            for (component, control) in controls.iter().enumerate() {
                let Some(control) = control else { continue };
                let Some(node) = control
                    .point_list
                    .iter()
                    .flat_map(|list| &list.points)
                    .find(|point| {
                        point.name.split(|&byte| byte == 0).next() == Some(b"CameraCenter")
                    })
                else {
                    continue;
                };
                let Some(record) = control.typed_records.iter().find(|record| {
                    record.tag == 4
                        && Ok(record.fixed_words[1]) == u32::try_from(node.raw_signed_field_04)
                }) else {
                    continue;
                };
                let Some(model) = preview.dynamic_component_geometry[object][component].as_ref()
                else {
                    continue;
                };
                let Some(poses) = preview.component_pose_buffers[object][component].as_ref() else {
                    continue;
                };
                let mount = preview.component_mount_poses[object][component];
                let mut projected = project_cockpit_msh(
                    &model.model,
                    model.root_transform,
                    model.owner_scale,
                    poses,
                    mount,
                )
                .map_err(|error| format!("project cockpit model {object}:{component}: {error}"))?;
                if projected.draw_ranges.is_empty() {
                    continue;
                }
                let handedness = native_msh_handedness(model.root_transform, model.owner_scale);
                let owner_scope = u32::try_from(object + 1)
                    .map_err(|_| "cockpit light owner exceeds GPU scope range".to_string())?;
                for range in &mut projected.draw_ranges {
                    range.owner_scope = owner_scope;
                    range.pipeline_state.cull = native_msh_cull_mode(range.batch_flags, handedness);
                }
                let visual = assets
                    .visual_by_id(assets.visuals_for_object(object)[component])
                    .ok_or_else(|| format!("missing cockpit visual {object}:{component}"))?;
                let selectors = static_preview_component_materials(
                    assets,
                    visual,
                    &projected,
                    None,
                    &mut preview.materials,
                    &mut preview.material_animations,
                )?;
                let lightmaps = static_preview_lightmap_textures(assets, visual)?;
                let vertex_start = preview.mesh.vertices.len();
                let draw_start = preview.mesh.draw_ranges.len();
                append_static_preview_component_with_lightmaps(
                    &mut preview.mesh,
                    projected,
                    &selectors,
                    Some(&mut preview.materials),
                    &lightmaps,
                    Some(&mut preview.material_animations),
                )?;
                let draws = draw_start..preview.mesh.draw_ranges.len();
                for range in draws.clone() {
                    preview.native_handedness_by_range.insert(range, handedness);
                    if model.model.type16_present {
                        preview.type16_present_ranges.insert(range);
                    }
                }
                geometry.bindings.insert(
                    (object, component),
                    Binding {
                        vertices: vertex_start..preview.mesh.vertices.len(),
                        draws,
                        projection: std::array::from_fn(|axis| {
                            f32::from_bits(record.fixed_words[11 + axis])
                        }),
                    },
                );
            }
        }
        Ok(geometry)
    }

    pub(super) fn select(&mut self, cockpit: StartingCockpit, camera: RawCameraTransform) {
        let key = (cockpit.object_index, cockpit.component_index);
        self.active = self.bindings.contains_key(&key).then_some((key, camera));
    }

    pub(super) fn clear(&mut self) {
        self.active = None;
    }

    pub(super) fn update(
        &self,
        geometry: &[Vec<Option<DynamicComponentGeometry>>],
        pose_buffers: &[Vec<Option<NodePoseBuffer>>],
        mount_poses: &[Vec<Option<VulkanNodePose>>],
        vertices: &mut [VulkanStaticVertex],
        placements: &HashMap<usize, SampledPlacement>,
    ) -> Result<(), String> {
        let Some(((object, component), _)) = self.active else {
            return Ok(());
        };
        let binding = &self.bindings[&(object, component)];
        let model = geometry[object][component].as_ref().expect("cockpit model");
        let poses = pose_buffers[object][component]
            .as_ref()
            .expect("cockpit poses");
        let mount = mount_poses[object][component];
        let (transform, scale) = placements
            .get(&object)
            .map_or((model.root_transform, model.owner_scale), |sample| {
                (sample.pose.transform, sample.pose.scale)
            });
        let projected = project_cockpit_msh(&model.model, transform, scale, poses, mount)
            .map_err(|error| format!("update cockpit model {object}:{component}: {error}"))?;
        let destination = &mut vertices[binding.vertices.clone()];
        if destination.len() != projected.vertices.len() {
            return Err(format!(
                "cockpit model layout changed at {object}:{component}"
            ));
        }
        destination.copy_from_slice(&projected.vertices);
        Ok(())
    }

    pub(super) fn apply(
        &self,
        renderer: &mut VulkanSmokeRenderer,
        viewport: [f32; 2],
    ) -> Result<(), String> {
        let active = self.active.and_then(|(key, camera)| {
            let binding = &self.bindings[&key];
            let [near_plane, far_plane, field_of_view_radians] = binding.projection;
            let projection = LegacyD3d7Projection {
                viewport: [0, 0, viewport[0] as i32, viewport[1] as i32],
                near_plane,
                far_plane,
                field_of_view_radians,
            };
            VulkanStaticCamera::from_legacy_d3d7(camera, projection)
                .map(|camera| (binding.draws.clone(), camera))
        });
        let active_key = active.as_ref().and(self.active).map(|(key, _)| key);
        renderer
            .set_cockpit_camera(active)
            .map_err(|error| format!("set cockpit projection: {error}"))?;
        for (key, binding) in &self.bindings {
            if active_key == Some(*key) {
                continue;
            }
            for draw in binding.draws.clone() {
                renderer
                    .set_draw_range_index_count(draw, 0)
                    .map_err(|error| format!("hide inactive cockpit mesh: {error}"))?;
            }
        }
        Ok(())
    }
}
