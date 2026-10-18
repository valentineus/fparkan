#![allow(unsafe_code)]

use ash::vk;
use fparkan_render::{LegacyDepthMode, LegacyPipelineState};

use super::smoke_types::{resolve_draw_texture_indices, VulkanStaticMaterial};
use super::update_material_texture_descriptor;
use super::{
    create_command_pool, create_frame_sync, create_index_buffer, create_static_mesh_index_buffer,
    create_static_mesh_vertex_buffer, create_static_texture_image, create_swapchain_resources,
    create_validation_messenger, create_vertex_buffer, create_vulkan_instance_probe,
    create_vulkan_logical_device_probe_for_request, create_vulkan_surface_probe,
    create_vulkan_swapchain_probe_for_extent, destroy_allocated_buffer, destroy_allocated_image,
    destroy_swapchain_resources, ensure_point_light_capacity, plan_vulkan_surface,
    readback_buffer_bytes, update_static_texture_image, write_frame_uniform,
    write_material_specular_buffer, write_point_light_buffer, write_static_mesh_index_buffer,
    write_static_mesh_vertex_buffer, VulkanAllocatedBuffer, VulkanInstanceConfig,
    VulkanInstanceProbe, VulkanLogicalDeviceProbe, VulkanReadbackArtifact, VulkanSmokeFrameOutcome,
    VulkanSmokeRenderer, VulkanSmokeRendererCreateInfo, VulkanSmokeRendererError,
    VulkanSmokeRendererReport, VulkanSmokeShutdownReport, VulkanSurfaceProbe, VulkanSwapchainProbe,
    VulkanSwapchainResources, VulkanValidationMessenger, VulkanValidationReport,
};
use crate::policy::KHR_PORTABILITY_SUBSET_EXTENSION;
use crate::shader::validate_triangle_shaders;

#[cfg(test)]
fn take_runtime_owners_in_dependency_order<Instance, Validation, Surface, Device, Swapchain>(
    instance: &mut Option<Instance>,
    validation: &mut Option<Validation>,
    surface: &mut Option<Surface>,
    device: &mut Option<Device>,
    swapchain: &mut Option<Swapchain>,
) {
    swapchain.take();
    device.take();
    surface.take();
    validation.take();
    instance.take();
}

fn take_runtime_children_with_validation_snapshot<
    Surface,
    Device,
    Swapchain,
    Validation,
    Snapshot,
    Capture,
>(
    surface: &mut Option<Surface>,
    device: &mut Option<Device>,
    swapchain: &mut Option<Swapchain>,
    validation: Option<&Validation>,
    capture: Capture,
) -> Option<Snapshot>
where
    Capture: FnOnce(&Validation) -> Snapshot,
{
    swapchain.take();
    device.take();
    surface.take();
    validation.map(capture)
}

struct RollbackOnDrop<T, F>
where
    F: FnOnce(T),
{
    value: Option<T>,
    rollback: Option<F>,
}

impl<T, F> RollbackOnDrop<T, F>
where
    F: FnOnce(T),
{
    fn new(value: T, rollback: F) -> Self {
        Self {
            value: Some(value),
            rollback: Some(rollback),
        }
    }

    fn commit(mut self) -> T {
        self.rollback.take();
        match self.value.take() {
            Some(value) => value,
            None => unreachable!("rollback guard must hold a value until commit"),
        }
    }
}

impl<T, F> Drop for RollbackOnDrop<T, F>
where
    F: FnOnce(T),
{
    fn drop(&mut self) {
        if let (Some(value), Some(rollback)) = (self.value.take(), self.rollback.take()) {
            rollback(value);
        }
    }
}

impl VulkanSmokeRenderer {
    /// Creates a live Vulkan smoke renderer bound to a live native window.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanSmokeRendererError`] when Vulkan bootstrap, pipeline creation,
    /// memory allocation, or synchronization resource creation fails.

    pub fn new(
        create_info: &VulkanSmokeRendererCreateInfo,
    ) -> Result<Self, VulkanSmokeRendererError> {
        create_info
            .mesh
            .validate()
            .map_err(|context| VulkanSmokeRendererError::InvalidStaticMesh { context })?;
        if !create_info.camera.is_finite() {
            return Err(VulkanSmokeRendererError::InvalidStaticCamera {
                context: "non-finite clip_from_world matrix",
            });
        }
        let draw_texture_indices =
            resolve_draw_texture_indices(&create_info.mesh.draw_ranges, &create_info.materials)
                .map_err(|context| VulkanSmokeRendererError::InvalidStaticMesh { context })?;
        let bootstrap_progress = create_info.bootstrap_progress.as_ref();
        validate_triangle_shaders().map_err(VulkanSmokeRendererError::Shader)?;
        let surface_plan = plan_vulkan_surface(Some(create_info.native_handles))
            .map_err(VulkanSmokeRendererError::Surface)?;
        let mut instance_config = VulkanInstanceConfig::smoke(&create_info.application_name);
        instance_config
            .required_extensions
            .clone_from(&surface_plan.required_instance_extensions);
        instance_config.enable_validation = create_info.enable_validation;
        let instance = create_vulkan_instance_probe(&instance_config)
            .map_err(VulkanSmokeRendererError::Instance)?;
        if let Some(progress) = bootstrap_progress {
            progress.mark_loader_available();
            progress.mark_instance_created();
        }
        let validation = if create_info.enable_validation {
            Some(create_validation_messenger(&instance)?)
        } else {
            None
        };
        let surface = create_vulkan_surface_probe(&instance, Some(create_info.native_handles))
            .map_err(VulkanSmokeRendererError::Surface)?;
        if let Some(progress) = bootstrap_progress {
            progress.mark_surface_created();
        }
        let device = create_vulkan_logical_device_probe_for_request(
            &instance,
            &surface,
            create_info.drawable_extent,
            create_info.render_request,
        )
        .map_err(VulkanSmokeRendererError::LogicalDevice)?;
        if let Some(progress) = bootstrap_progress {
            progress.mark_logical_device_created();
        }
        let swapchain = create_vulkan_swapchain_probe_for_extent(
            &instance,
            &surface,
            &device,
            create_info.drawable_extent,
            vk::SwapchainKHR::null(),
        )
        .map_err(VulkanSmokeRendererError::Swapchain)?;
        if let Some(progress) = bootstrap_progress {
            progress.mark_swapchain_created();
        }
        let command_pool = create_command_pool(&device)?;
        let vertex_buffer =
            match create_static_mesh_vertex_buffer(&instance, &device, &create_info.mesh) {
                Ok(buffer) => buffer,
                Err(error) => {
                    // SAFETY: The command pool belongs to this live logical device and is destroyed on setup failure.
                    unsafe { device.device().destroy_command_pool(command_pool, None) };
                    return Err(error);
                }
            };
        let index_buffer =
            match create_static_mesh_index_buffer(&instance, &device, &create_info.mesh) {
                Ok(buffer) => buffer,
                Err(error) => {
                    // SAFETY: The command pool belongs to this live logical device and is destroyed on setup failure.
                    unsafe { device.device().destroy_command_pool(command_pool, None) };
                    destroy_allocated_buffer(&device, &vertex_buffer);
                    return Err(error);
                }
            };
        // Keep the compatibility triangle path valid: a white base texel,
        // neutral detail texels and a transparent white overlay make the
        // additional terrain stages mathematically transparent.
        let fallback_texture = super::VulkanStaticTexture {
            width: 1,
            height: 1,
            rgba8: vec![255, 255, 255, 255],
            mip_levels: Vec::new(),
        };
        let fallback_detail_texture = super::VulkanStaticTexture {
            width: 1,
            height: 1,
            rgba8: vec![128, 128, 128, 255],
            mip_levels: Vec::new(),
        };
        let fallback_overlay_texture = super::VulkanStaticTexture {
            width: 1,
            height: 1,
            rgba8: vec![255, 255, 255, 0],
            mip_levels: Vec::new(),
        };
        // Fallback stages are immutable compatibility images. Keep one
        // allocation for each fallback kind and map every logical material
        // slot to it. Authored images remain one-per-slot because dynamic
        // phase uploads must never couple two materials.
        let texture_sources = if create_info.materials.is_empty() {
            vec![
                (&fallback_texture, Some(0_usize)),
                (&fallback_detail_texture, Some(1_usize)),
                (&fallback_overlay_texture, Some(2_usize)),
                (&fallback_detail_texture, Some(1_usize)),
            ]
        } else {
            let mut sources = Vec::with_capacity(create_info.materials.len() * 4);
            for material in &create_info.materials {
                sources.push((&material.texture, None));
                sources.push((
                    material
                        .detail_texture
                        .as_ref()
                        .map_or(&fallback_detail_texture, |texture| texture),
                    material.detail_texture.is_none().then_some(1_usize),
                ));
                sources.push((
                    material
                        .overlay_texture
                        .as_ref()
                        .map_or(&fallback_overlay_texture, |texture| texture),
                    material.overlay_texture.is_none().then_some(2_usize),
                ));
                sources.push((
                    material
                        .overlay_detail_texture
                        .as_ref()
                        .map_or(&fallback_detail_texture, |texture| texture),
                    material.overlay_detail_texture.is_none().then_some(1_usize),
                ));
            }
            sources
        };
        let mut textures = Vec::with_capacity(texture_sources.len());
        let mut texture_slot_indices = Vec::with_capacity(texture_sources.len());
        let mut fallback_image_indices = [None; 3];
        for (texture_source, fallback_kind) in texture_sources {
            let texture_index = if let Some(fallback_kind) = fallback_kind {
                if let Some(texture_index) = fallback_image_indices[fallback_kind] {
                    texture_index
                } else {
                    let image = match create_static_texture_image(
                        &instance,
                        &device,
                        command_pool,
                        texture_source,
                    ) {
                        Ok(image) => image,
                        Err(error) => {
                            for texture in &textures {
                                destroy_allocated_image(&device, texture);
                            }
                            // SAFETY: These resources belong to this live device and are rolled back before it drops.
                            unsafe { device.device().destroy_command_pool(command_pool, None) };
                            destroy_allocated_buffer(&device, &index_buffer);
                            destroy_allocated_buffer(&device, &vertex_buffer);
                            return Err(error);
                        }
                    };
                    let texture_index = textures.len();
                    textures.push(image);
                    fallback_image_indices[fallback_kind] = Some(texture_index);
                    texture_index
                }
            } else {
                let image = match create_static_texture_image(
                    &instance,
                    &device,
                    command_pool,
                    texture_source,
                ) {
                    Ok(image) => image,
                    Err(error) => {
                        for texture in &textures {
                            destroy_allocated_image(&device, texture);
                        }
                        // SAFETY: These resources belong to this live device and are rolled back before it drops.
                        unsafe { device.device().destroy_command_pool(command_pool, None) };
                        destroy_allocated_buffer(&device, &index_buffer);
                        destroy_allocated_buffer(&device, &vertex_buffer);
                        return Err(error);
                    }
                };
                let texture_index = textures.len();
                textures.push(image);
                texture_index
            };
            texture_slot_indices.push(texture_index);
        }
        let material_alphas = if create_info.materials.is_empty() {
            vec![[1.0, 0.0]]
        } else {
            create_info
                .materials
                .iter()
                .map(|material| {
                    [
                        material.diffuse_alpha,
                        material.overlay_diffuse_alpha.unwrap_or(0.0),
                    ]
                })
                .collect()
        };
        let material_lighting = if create_info.materials.is_empty() {
            vec![[[1.0; 3], [0.0; 3], [1.0; 3], [0.0; 3]]]
        } else {
            create_info
                .materials
                .iter()
                .map(|material| {
                    [
                        material.directional_rgb,
                        material.additive_rgb,
                        material.overlay_directional_rgb.unwrap_or([1.0; 3]),
                        material.overlay_additive_rgb.unwrap_or([0.0; 3]),
                    ]
                })
                .collect()
        };
        let material_uv_transforms = if create_info.materials.is_empty() {
            vec![[[0.0, 0.0, 1.0, 1.0]; 4]]
        } else {
            create_info
                .materials
                .iter()
                .map(|material| {
                    [
                        material.uv_transform,
                        material.detail_uv_transform,
                        material
                            .overlay_uv_transform
                            .unwrap_or([0.0, 0.0, 1.0, 1.0]),
                        material
                            .overlay_detail_uv_transform
                            .unwrap_or([0.0, 0.0, 1.0, 1.0]),
                    ]
                })
                .collect()
        };
        let material_unlit = if create_info.materials.is_empty() {
            vec![0.0]
        } else {
            create_info
                .materials
                .iter()
                .map(VulkanStaticMaterial::encoded_material_mode)
                .collect()
        };
        let material_specular = if create_info.materials.is_empty() {
            vec![[[0.0; 4]; 2]]
        } else {
            create_info
                .materials
                .iter()
                .map(|material| {
                    [
                        [
                            material.specular_rgb[0],
                            material.specular_rgb[1],
                            material.specular_rgb[2],
                            f32::from(material.specular_power),
                        ],
                        [
                            material.overlay_specular_rgb.unwrap_or([0.0; 3])[0],
                            material.overlay_specular_rgb.unwrap_or([0.0; 3])[1],
                            material.overlay_specular_rgb.unwrap_or([0.0; 3])[2],
                            f32::from(material.overlay_specular_power.unwrap_or(0)),
                        ],
                    ]
                })
                .collect()
        };
        let material_selectors = if create_info.materials.is_empty() {
            vec![0]
        } else {
            create_info
                .materials
                .iter()
                .map(|material| material.material_index)
                .collect()
        };
        let mut renderer = Self {
            instance: Some(instance),
            validation,
            surface: Some(surface),
            device: Some(device),
            swapchain: Some(swapchain),
            command_pool,
            swapchain_resources: None,
            vertex_buffer: Some(vertex_buffer),
            index_buffer: Some(index_buffer),
            texture_slot_indices,
            textures,
            vertex_count: create_info.mesh.vertices.len(),
            index_capacity: create_info.mesh.indices.len(),
            draw_range_capacities: create_info
                .mesh
                .draw_ranges
                .iter()
                .map(|range| range.index_count)
                .collect(),
            draw_ranges: create_info.mesh.draw_ranges.clone(),
            draw_texture_indices,
            draw_order: (0..create_info.mesh.draw_ranges.len()).collect(),
            draw_range_sort_keys: vec![None; create_info.mesh.draw_ranges.len()],
            draw_order_dirty: false,
            draw_range_world_queue: vec![None; create_info.mesh.draw_ranges.len()],
            world_transparent_background_ranges: Vec::new(),
            draw_range_base_pipeline_states: create_info
                .mesh
                .draw_ranges
                .iter()
                .map(|range| range.pipeline_state)
                .collect(),
            material_alphas,
            material_lighting,
            material_uv_transforms,
            material_unlit,
            material_specular,
            material_selectors,
            material_specular_dirty: true,
            readback_enabled: false,
            camera: create_info.camera,
            frame_uniforms: super::VulkanFrameUniforms::from_camera(create_info.camera),
            point_lights: Vec::new(),
            point_lights_explicit: false,
            frame_sync: Vec::new(),
            images_in_flight: Vec::new(),
            current_frame: 0,
            last_readback_image_index: None,
            depth_request: create_info.render_request.depth,
            pending_extent: None,
            swapchain_recreate_count: 0,
            report: VulkanSmokeRendererReport {
                portability_enumeration: instance_config.enable_portability_enumeration,
                portability_subset_enabled: false,
                device_name: String::new(),
                graphics_queue_family: 0,
                present_queue_family: 0,
                enabled_extension_count: 0,
                swapchain_extent: (0, 0),
                swapchain_image_count: 0,
                swapchain_image_format: 0,
                swapchain_image_usage: 0,
                readback_copy_count: 0,
                readback_byte_count: 0,
                readback_fnv1a64: 0,
            },
        };
        renderer.rebuild_swapchain_resources(false)?;
        let device_ref = renderer.device_ref()?;
        let swapchain_ref = renderer.swapchain_ref()?;
        renderer.report = VulkanSmokeRendererReport {
            portability_enumeration: renderer
                .instance
                .as_ref()
                .is_some_and(|instance| instance.report.create_flags != 0),
            portability_subset_enabled: device_ref
                .report
                .enabled_extensions
                .iter()
                .any(|extension| extension == KHR_PORTABILITY_SUBSET_EXTENSION),
            device_name: device_ref.report.device_name.clone(),
            graphics_queue_family: device_ref.report.graphics_queue_family,
            present_queue_family: device_ref.report.present_queue_family,
            enabled_extension_count: device_ref
                .report
                .enabled_extensions
                .len()
                .try_into()
                .unwrap_or(u32::MAX),
            swapchain_extent: swapchain_ref.report.plan.extent,
            swapchain_image_count: swapchain_ref.report.image_count,
            swapchain_image_format: swapchain_ref.report.plan.format.format,
            swapchain_image_usage: swapchain_ref.report.plan.image_usage,
            readback_copy_count: 0,
            readback_byte_count: 0,
            readback_fnv1a64: 0,
        };
        Ok(renderer)
    }

    /// Returns the current bootstrap report.
    #[must_use]
    pub const fn report(&self) -> &VulkanSmokeRendererReport {
        &self.report
    }

    /// Returns measured validation counters and VUIDs.
    #[must_use]
    pub fn validation_report(&self) -> VulkanValidationReport {
        self.validation.as_ref().map_or(
            VulkanValidationReport {
                warning_count: 0,
                error_count: 0,
                vuids: Vec::new(),
            },
            VulkanValidationMessenger::report,
        )
    }

    /// Returns the measured swapchain recreation count.
    #[must_use]
    pub const fn swapchain_recreate_count(&self) -> u32 {
        self.swapchain_recreate_count
    }

    /// Returns the camera that will be uploaded for the next recorded frame.
    #[must_use]
    pub const fn camera(&self) -> super::VulkanStaticCamera {
        self.camera
    }

    /// Returns the frame state uploaded before the next recorded frame.
    #[must_use]
    pub const fn frame_uniforms(&self) -> super::VulkanFrameUniforms {
        self.frame_uniforms
    }

    /// Replaces the camera for subsequent frames without recreating GPU resources.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanSmokeRendererError::InvalidStaticCamera`] when a matrix
    /// element is non-finite. The caller must update only between
    /// [`Self::draw_frame`] calls on the renderer-owning thread.
    pub fn set_camera(
        &mut self,
        camera: super::VulkanStaticCamera,
    ) -> Result<(), VulkanSmokeRendererError> {
        if !camera.is_finite() {
            return Err(VulkanSmokeRendererError::InvalidStaticCamera {
                context: "non-finite clip_from_world matrix",
            });
        }
        self.camera = camera;
        self.frame_uniforms.clip_from_world = camera.clip_from_world;
        Ok(())
    }

    /// Replaces lighting, fog, camera-position and matrix state for
    /// subsequent frames without recreating GPU resources.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanSmokeRendererError::InvalidStaticCamera`] when any
    /// submitted value is non-finite. The caller must update only between
    /// [`Self::draw_frame`] calls on the renderer-owning thread.
    pub fn set_frame_uniforms(
        &mut self,
        frame_uniforms: super::VulkanFrameUniforms,
    ) -> Result<(), VulkanSmokeRendererError> {
        if !frame_uniforms.is_finite() {
            return Err(VulkanSmokeRendererError::InvalidStaticCamera {
                context: "non-finite frame uniform",
            });
        }
        self.camera.clip_from_world = frame_uniforms.clip_from_world;
        self.frame_uniforms = frame_uniforms;
        Ok(())
    }

    /// Replaces the complete dynamic point-light set for subsequent frames.
    ///
    /// The storage buffer grows on demand and rejects counts beyond the
    /// device's `maxStorageBufferRange`; it never truncates scene lights.
    /// Values are uploaded to the current fence-safe frame slot by
    /// [`Self::draw_frame`].
    pub fn set_point_lights(
        &mut self,
        point_lights: &[super::VulkanPointLight],
    ) -> Result<(), VulkanSmokeRendererError> {
        let max_count = self.resources_ref()?.max_point_light_count;
        if point_lights.len() > max_count {
            return Err(VulkanSmokeRendererError::InvalidPointLights {
                context: "point-light count exceeds maxStorageBufferRange",
            });
        }
        if point_lights.iter().any(|light| {
            !light
                .position
                .into_iter()
                .chain(light.rgb)
                .chain([light.range])
                .chain(light.coefficients)
                .all(f32::is_finite)
        }) {
            return Err(VulkanSmokeRendererError::InvalidPointLights {
                context: "non-finite point-light field",
            });
        }
        self.point_lights.clear();
        self.point_lights.extend_from_slice(point_lights);
        self.point_lights_explicit = true;
        Ok(())
    }

    /// Enables the optional swapchain image copy used by screenshot/readback
    /// callers. Runtime frames leave it disabled to avoid a transfer every
    /// frame.
    pub fn set_readback_enabled(&mut self, enabled: bool) {
        self.readback_enabled = enabled;
        if !enabled {
            self.last_readback_image_index = None;
        }
    }

    /// Replaces the shared vertex allocation between frames.
    ///
    /// The initial mesh owns enough vertices for the scene's dynamic slots;
    /// callers update those slots in place as camera-relative environment
    /// geometry moves.  The device is idled before the host-visible write so
    /// no submitted command can observe a partial update.
    pub fn update_vertices(
        &mut self,
        vertices: &[super::VulkanStaticVertex],
    ) -> Result<(), VulkanSmokeRendererError> {
        if vertices.len() != self.vertex_count {
            return Err(VulkanSmokeRendererError::InvalidStaticMesh {
                context: "updated static mesh vertex count differs from allocation",
            });
        }
        // SAFETY: The renderer owns the queue and all submitted work; waiting
        // for idle makes the shared vertex allocation quiescent.
        unsafe { self.device_ref()?.device().device_wait_idle() }.map_err(|result| {
            VulkanSmokeRendererError::VulkanOperation {
                context: "vkDeviceWaitIdle(static mesh vertex update)",
                result,
            }
        })?;
        write_static_mesh_vertex_buffer(self.device_ref()?, self.vertex_buffer_ref()?, vertices)
    }

    /// Uploads the current shared mesh while retaining dynamic draw state.
    ///
    /// Environment geometry and projected shadow geometry share the original
    /// vertex/index allocations. `geometry_ranges` carries only the ranges
    /// whose index segment changed; material selection, blend/depth state,
    /// transparency classification, and active counts owned by other setters
    /// remain untouched. The allocations grow to the largest submitted frame
    /// and never grow for a frame that fits the retained capacity.
    ///
    /// A range with `index_count == 0` is valid and disables that range for the
    /// frame. The mesh itself still needs at least one vertex and one index so
    /// Vulkan never receives a zero-sized allocation.
    pub fn update_mesh_geometry(
        &mut self,
        vertices: &[super::VulkanStaticVertex],
        indices: &[u32],
        geometry_ranges: &[super::VulkanDynamicDrawRange],
    ) -> Result<(), VulkanSmokeRendererError> {
        validate_geometry_upload(
            vertices,
            indices,
            &self.draw_ranges,
            geometry_ranges,
            self.vertex_count,
            self.index_capacity,
        )?;

        let vertex_grew = vertices.len() > self.vertex_count;
        let index_grew = indices.len() > self.index_capacity;

        // SAFETY: The renderer owns the queue and all submitted work. Waiting
        // before either writing or replacing a shared allocation keeps old
        // command buffers from observing a partial upload.
        unsafe { self.device_ref()?.device().device_wait_idle() }.map_err(|result| {
            VulkanSmokeRendererError::VulkanOperation {
                context: "vkDeviceWaitIdle(dynamic mesh update)",
                result,
            }
        })?;

        let (new_vertex_buffer, new_index_buffer) = if vertex_grew || index_grew {
            let instance = self.instance_ref()?;
            let device = self.device_ref()?;
            let new_vertex_buffer = if vertex_grew {
                Some(create_vertex_buffer(instance, device, vertices)?)
            } else {
                None
            };
            let new_index_buffer = if index_grew {
                match create_index_buffer(instance, device, indices) {
                    Ok(buffer) => Some(buffer),
                    Err(error) => {
                        if let Some(buffer) = new_vertex_buffer.as_ref() {
                            destroy_allocated_buffer(device, buffer);
                        }
                        return Err(error);
                    }
                }
            } else {
                None
            };
            if !vertex_grew {
                if let Err(error) =
                    write_static_mesh_vertex_buffer(device, self.vertex_buffer_ref()?, vertices)
                {
                    if let Some(buffer) = new_vertex_buffer.as_ref() {
                        destroy_allocated_buffer(device, buffer);
                    }
                    if let Some(buffer) = new_index_buffer.as_ref() {
                        destroy_allocated_buffer(device, buffer);
                    }
                    return Err(error);
                }
            }
            if !index_grew {
                if let Err(error) =
                    write_static_mesh_index_buffer(device, self.index_buffer_ref()?, indices)
                {
                    if let Some(buffer) = new_vertex_buffer.as_ref() {
                        destroy_allocated_buffer(device, buffer);
                    }
                    if let Some(buffer) = new_index_buffer.as_ref() {
                        destroy_allocated_buffer(device, buffer);
                    }
                    return Err(error);
                }
            }
            (new_vertex_buffer, new_index_buffer)
        } else {
            let device = self.device_ref()?;
            write_static_mesh_vertex_buffer(device, self.vertex_buffer_ref()?, vertices)?;
            write_static_mesh_index_buffer(device, self.index_buffer_ref()?, indices)?;
            (None, None)
        };

        if let Some(buffer) = new_vertex_buffer {
            let old = self.vertex_buffer.replace(buffer);
            if let Some(old) = old {
                destroy_allocated_buffer(self.device_ref()?, &old);
            }
            self.vertex_count = vertices.len();
        }
        if let Some(buffer) = new_index_buffer {
            let old = self.index_buffer.replace(buffer);
            if let Some(old) = old {
                destroy_allocated_buffer(self.device_ref()?, &old);
            }
            self.index_capacity = indices.len();
        }

        apply_geometry_range_updates(
            &mut self.draw_ranges,
            &mut self.draw_range_capacities,
            geometry_ranges,
        );
        Ok(())
    }

    /// Replaces one material texture in place while retaining its descriptor
    /// set and sampler.  This is used by dynamic fixed-size atlases such as
    /// the native 256 by 256 projected-shadow texture.
    ///
    /// The updated image must have the same dimensions and generated/authored
    /// mip count as the texture allocated during renderer creation.  The
    /// renderer idles its device before the transfer, so callers may invoke
    /// this between frame updates without taking ownership of Vulkan handles.
    pub fn update_material_texture(
        &mut self,
        material_index: usize,
        texture_slot: usize,
        texture: &super::VulkanStaticTexture,
    ) -> Result<(), VulkanSmokeRendererError> {
        if texture_slot >= 4 {
            return Err(VulkanSmokeRendererError::InvalidStaticTexture {
                context: "dynamic material texture slot is out of bounds",
            });
        }
        let logical_texture_index = material_index
            .checked_mul(4)
            .and_then(|index| index.checked_add(texture_slot))
            .ok_or(VulkanSmokeRendererError::InvalidStaticTexture {
                context: "dynamic material texture index overflows address space",
            })?;
        let texture_index = self
            .texture_slot_indices
            .get(logical_texture_index)
            .copied()
            .ok_or(VulkanSmokeRendererError::InvalidStaticTexture {
                context: "dynamic material texture index is out of bounds",
            })?;
        if self.textures.get(texture_index).is_none() {
            return Err(VulkanSmokeRendererError::InvalidStaticTexture {
                context: "dynamic material texture index is out of bounds",
            });
        }
        {
            let device = self.device_ref()?;
            // SAFETY: every submission made by this renderer uses this device; the
            // idle wait makes all descriptor image reads quiescent before upload.
            unsafe { device.device().device_wait_idle() }.map_err(|result| {
                VulkanSmokeRendererError::VulkanOperation {
                    context: "vkDeviceWaitIdle(dynamic texture update)",
                    result,
                }
            })?;
        }
        let shared_texture = self
            .texture_slot_indices
            .iter()
            .filter(|mapped_index| **mapped_index == texture_index)
            .count()
            > 1;
        if shared_texture {
            // A fallback image can serve many logical slots. Detach the
            // requested slot before uploading so this public update method
            // keeps its one-material/one-slot semantics.
            let image = {
                let instance = self.instance_ref()?;
                let device = self.device_ref()?;
                create_static_texture_image(instance, device, self.command_pool, texture)?
            };
            let new_texture_index = self.textures.len();
            self.textures.push(image);
            self.texture_slot_indices[logical_texture_index] = new_texture_index;
            let device = self.device_ref()?;
            let resources = self.resources_ref()?;
            update_material_texture_descriptor(
                device,
                resources,
                material_index,
                texture_slot,
                &self.textures[new_texture_index],
            )
        } else {
            let instance = self.instance_ref()?;
            let device = self.device_ref()?;
            let image = self.textures.get(texture_index).ok_or(
                VulkanSmokeRendererError::InvalidStaticTexture {
                    context: "dynamic material texture index is out of bounds",
                },
            )?;
            update_static_texture_image(instance, device, self.command_pool, image, texture)
        }
    }

    /// Changes one draw range's active index count while retaining its
    /// pipeline and material selector.  This lets a fixed-capacity weather
    /// allocation draw only the particles emitted in the current frame.
    pub fn set_draw_range_index_count(
        &mut self,
        range_index: usize,
        index_count: u32,
    ) -> Result<(), VulkanSmokeRendererError> {
        if index_count % 3 != 0 {
            return Err(VulkanSmokeRendererError::InvalidStaticMesh {
                context: "dynamic draw range index count is not a triangle count",
            });
        }
        let capacity = self.draw_range_capacities.get(range_index).copied().ok_or(
            VulkanSmokeRendererError::InvalidStaticMesh {
                context: "dynamic draw range index is out of bounds",
            },
        )?;
        if index_count > capacity {
            return Err(VulkanSmokeRendererError::InvalidStaticMesh {
                context: "dynamic draw range exceeds its index allocation",
            });
        }
        let range = self.draw_ranges.get_mut(range_index).ok_or(
            VulkanSmokeRendererError::InvalidStaticMesh {
                context: "dynamic draw range index is out of bounds",
            },
        )?;
        range.index_count = index_count;
        Ok(())
    }

    /// Sets the native per-range alpha reference in legacy 0..=255 units.
    ///
    /// Zero remains an explicit value; use the native-default constant when
    /// matching the renderer's initial state. The update is dynamic and does
    /// not recreate a graphics pipeline.
    pub fn set_draw_range_alpha_test_reference(
        &mut self,
        range_index: usize,
        alpha_test_reference: u8,
    ) -> Result<(), VulkanSmokeRendererError> {
        let range = self.draw_ranges.get_mut(range_index).ok_or(
            VulkanSmokeRendererError::InvalidStaticMesh {
                context: "dynamic draw range index is out of bounds",
            },
        )?;
        range.alpha_test_reference = alpha_test_reference;
        Ok(())
    }

    /// Applies the active MAT0 phase's blend and alpha-test category to one
    /// range while retaining its authored depth and cull state.
    pub fn set_draw_range_material_category(
        &mut self,
        range_index: usize,
        category: u8,
    ) -> Result<(), VulkanSmokeRendererError> {
        let base_state = *self
            .draw_range_base_pipeline_states
            .get(range_index)
            .ok_or(VulkanSmokeRendererError::InvalidStaticMesh {
                context: "dynamic draw range index is out of bounds",
            })?;
        let world_queue = self
            .draw_range_world_queue
            .get(range_index)
            .copied()
            .ok_or(VulkanSmokeRendererError::InvalidStaticMesh {
                context: "dynamic draw range index is out of bounds",
            })?;
        // Fixed sky, sprite, weather, and shadow ranges own explicit states.
        // MAT0 categories apply only after a range has entered the world queue.
        let (next_base, next_state) =
            world_material_pipeline_states(base_state, category, world_queue).ok_or(
                VulkanSmokeRendererError::InvalidStaticMesh {
                    context: "MAT0 pipeline category is outside the native 0..=4 table",
                },
            )?;
        let range = self.draw_ranges.get_mut(range_index).ok_or(
            VulkanSmokeRendererError::InvalidStaticMesh {
                context: "dynamic draw range index is out of bounds",
            },
        )?;
        self.draw_range_base_pipeline_states[range_index] = next_base;
        let pipeline_changed = range.pipeline_state != next_state;
        range.pipeline_state = next_state;
        if pipeline_changed {
            self.rebuild_pipeline_resources()?;
        }
        Ok(())
    }

    /// Places nebula, dome, and cloud ranges between opaque and transparent
    /// world draws. The list is explicit so unrelated fixed passes keep their
    /// established order.
    pub fn set_world_transparent_background_ranges(
        &mut self,
        range_indices: &[usize],
    ) -> Result<(), VulkanSmokeRendererError> {
        let mut seen = vec![false; self.draw_ranges.len()];
        for &range_index in range_indices {
            let Some(slot) = seen.get_mut(range_index) else {
                return Err(VulkanSmokeRendererError::InvalidStaticMesh {
                    context: "world background range index is out of bounds",
                });
            };
            if *slot || self.draw_range_world_queue[range_index].is_some() {
                return Err(VulkanSmokeRendererError::InvalidStaticMesh {
                    context: "world background range is duplicated or already queued",
                });
            }
            *slot = true;
        }
        self.world_transparent_background_ranges = range_indices.to_vec();
        self.draw_order_dirty = true;
        Ok(())
    }

    /// Marks a draw range as belonging to the native transparent queue.
    ///
    /// The flag is deliberately independent of the blend category: native
    /// materials can blend while retaining depth writes, and some alpha
    /// materials are depth-tested opaque.  When enabled, a range that uses a
    /// depth attachment switches to depth-test/read-only.  The original depth
    /// state is retained so disabling the flag restores the caller's state.
    ///
    /// The transparent flag only participates in the world draw order. Without
    /// explicit backgrounds, opaque and transparent ranges replace the
    /// existing world slots. With explicit backgrounds, opaque ranges compact
    /// into those slots and the sorted transparent ranges are inserted after
    /// the last background; unrelated fixed ranges keep relative order but
    /// may shift slots.
    pub fn set_draw_range_transparency(
        &mut self,
        range_index: usize,
        transparent: bool,
    ) -> Result<(), VulkanSmokeRendererError> {
        if self
            .world_transparent_background_ranges
            .contains(&range_index)
        {
            return Err(VulkanSmokeRendererError::InvalidStaticMesh {
                context: "world background range cannot join the world queue",
            });
        }
        let base_state = *self
            .draw_range_base_pipeline_states
            .get(range_index)
            .ok_or(VulkanSmokeRendererError::InvalidStaticMesh {
                context: "dynamic draw range index is out of bounds",
            })?;
        let range = self.draw_ranges.get_mut(range_index).ok_or(
            VulkanSmokeRendererError::InvalidStaticMesh {
                context: "dynamic draw range index is out of bounds",
            },
        )?;
        let next_state = apply_world_transparency(base_state, transparent);
        let pipeline_changed = range.pipeline_state != next_state;
        range.pipeline_state = next_state;
        // Calling this setter opts the range into the world queue.  Fixed
        // screen/sky/weather ranges remain `None` because their pass order is
        // intentionally owned by the caller.
        self.draw_range_world_queue[range_index] = Some(transparent);
        self.draw_order_dirty = true;
        if pipeline_changed {
            self.rebuild_pipeline_resources()?;
        }
        Ok(())
    }

    /// Supplies the native camera-sort key returned by the draw-item camera
    /// callback.  The native callback computes Euclidean distance from the
    /// camera to the first world vertex of the whole indexed batch.
    ///
    /// The recovered comparator inserts in descending floating-point camera
    /// key order.  Keeping the key separate from the range handle mirrors the
    /// native layer's callback result and its `[item pointer, key]` entries.
    pub fn set_draw_range_sort_key(
        &mut self,
        range_index: usize,
        sort_key: f32,
    ) -> Result<(), VulkanSmokeRendererError> {
        if !sort_key.is_finite() {
            return Err(VulkanSmokeRendererError::InvalidStaticMesh {
                context: "native draw sort key must be finite",
            });
        }
        let key = self.draw_range_sort_keys.get_mut(range_index).ok_or(
            VulkanSmokeRendererError::InvalidStaticMesh {
                context: "dynamic draw range index is out of bounds",
            },
        )?;
        *key = Some(sort_key);
        self.draw_order_dirty = true;
        Ok(())
    }

    /// Returns the current stable range-handle order used by command
    /// recording.  Handles are indices into the original `draw_ranges` vector.
    #[must_use]
    pub fn draw_order(&self) -> &[usize] {
        &self.draw_order
    }

    /// Changes the diffuse and overlay alpha values carried by one material's
    /// push constants.  The descriptor images remain unchanged.
    pub fn set_material_alphas(
        &mut self,
        material_index: usize,
        diffuse_alpha: f32,
        overlay_alpha: f32,
    ) -> Result<(), VulkanSmokeRendererError> {
        if !diffuse_alpha.is_finite()
            || !overlay_alpha.is_finite()
            || !(0.0..=1.0).contains(&diffuse_alpha)
            || !(0.0..=1.0).contains(&overlay_alpha)
        {
            return Err(VulkanSmokeRendererError::InvalidStaticMesh {
                context: "dynamic material alpha must be finite and 0..1",
            });
        }
        let alphas = self.material_alphas.get_mut(material_index).ok_or(
            VulkanSmokeRendererError::InvalidStaticMesh {
                context: "dynamic material index is out of bounds",
            },
        )?;
        *alphas = [diffuse_alpha, overlay_alpha];
        Ok(())
    }

    /// Changes the base directional and additive lighting coefficients of one
    /// material while retaining its descriptor images and pipeline state.
    pub fn set_material_lighting(
        &mut self,
        material_index: usize,
        directional_rgb: [f32; 3],
        additive_rgb: [f32; 3],
    ) -> Result<(), VulkanSmokeRendererError> {
        self.set_material_lighting_with_overlay(
            material_index,
            directional_rgb,
            additive_rgb,
            None,
            None,
        )
    }

    /// Changes base and optional overlay directional/additive coefficients
    /// while retaining descriptor images and pipeline state.
    pub fn set_material_lighting_with_overlay(
        &mut self,
        material_index: usize,
        directional_rgb: [f32; 3],
        additive_rgb: [f32; 3],
        overlay_directional_rgb: Option<[f32; 3]>,
        overlay_additive_rgb: Option<[f32; 3]>,
    ) -> Result<(), VulkanSmokeRendererError> {
        if directional_rgb
            .into_iter()
            .chain(additive_rgb)
            .chain(overlay_directional_rgb.into_iter().flatten())
            .chain(overlay_additive_rgb.into_iter().flatten())
            .any(|value| !value.is_finite())
        {
            return Err(VulkanSmokeRendererError::InvalidStaticMesh {
                context: "dynamic material lighting must be finite",
            });
        }
        let lighting = self.material_lighting.get_mut(material_index).ok_or(
            VulkanSmokeRendererError::InvalidStaticMesh {
                context: "dynamic material index is out of bounds",
            },
        )?;
        lighting[0] = directional_rgb;
        lighting[1] = additive_rgb;
        lighting[2] = overlay_directional_rgb.unwrap_or([1.0; 3]);
        lighting[3] = overlay_additive_rgb.unwrap_or([0.0; 3]);
        Ok(())
    }

    /// Replaces the four Page transforms carried by one material's push
    /// constants.  The transform order is base, detail, overlay, and overlay
    /// detail, matching the material shader's texture bindings.
    ///
    /// The update is kept CPU-side and is consumed by the next recorded draw;
    /// no descriptor or image resource is recreated.  This is intentionally a
    /// complete replacement so an animated MAT0 phase cannot leave one stale
    /// Page rectangle behind when its texture set is unchanged.
    pub fn set_material_uv_transforms(
        &mut self,
        material_index: usize,
        uv_transforms: [[f32; 4]; 4],
    ) -> Result<(), VulkanSmokeRendererError> {
        if uv_transforms
            .into_iter()
            .flatten()
            .any(|value| !value.is_finite())
        {
            return Err(VulkanSmokeRendererError::InvalidStaticMesh {
                context: "dynamic material UV transform must be finite",
            });
        }
        let transforms = self.material_uv_transforms.get_mut(material_index).ok_or(
            VulkanSmokeRendererError::InvalidStaticMesh {
                context: "dynamic material index is out of bounds",
            },
        )?;
        *transforms = uv_transforms;
        Ok(())
    }

    /// Replaces the base and overlay specular records used by the vertex
    /// Gouraud lighting path.  RGB values remain normalized; powers retain
    /// the native MAT0 byte and are encoded as the fourth float in each GPU
    /// record.
    pub fn set_material_specular(
        &mut self,
        material_index: usize,
        specular_rgb: [f32; 3],
        power: u8,
        overlay_specular_rgb: Option<[f32; 3]>,
        overlay_power: Option<u8>,
    ) -> Result<(), VulkanSmokeRendererError> {
        if specular_rgb
            .into_iter()
            .chain(overlay_specular_rgb.into_iter().flatten())
            .any(|value| !value.is_finite())
        {
            return Err(VulkanSmokeRendererError::InvalidStaticMesh {
                context: "dynamic material specular RGB must be finite",
            });
        }
        let specular = self.material_specular.get_mut(material_index).ok_or(
            VulkanSmokeRendererError::InvalidStaticMesh {
                context: "dynamic material index is out of bounds",
            },
        )?;
        let overlay_rgb = overlay_specular_rgb.unwrap_or([0.0; 3]);
        let next = [
            [
                specular_rgb[0],
                specular_rgb[1],
                specular_rgb[2],
                f32::from(power),
            ],
            [
                overlay_rgb[0],
                overlay_rgb[1],
                overlay_rgb[2],
                f32::from(overlay_power.unwrap_or(0)),
            ],
        ];
        if *specular == next {
            return Ok(());
        }
        *specular = next;
        self.material_specular_dirty = true;
        Ok(())
    }

    /// Rebinds one draw range to a positional material slot without
    /// reallocating any vertex or descriptor resources.
    pub fn set_draw_range_material(
        &mut self,
        range_index: usize,
        material_index: usize,
    ) -> Result<(), VulkanSmokeRendererError> {
        let selector = *self.material_selectors.get(material_index).ok_or(
            VulkanSmokeRendererError::InvalidStaticMesh {
                context: "dynamic material index is out of bounds",
            },
        )?;
        let range = self.draw_ranges.get_mut(range_index).ok_or(
            VulkanSmokeRendererError::InvalidStaticMesh {
                context: "dynamic draw range index is out of bounds",
            },
        )?;
        range.material_index = selector;
        let texture_index = self.draw_texture_indices.get_mut(range_index).ok_or(
            VulkanSmokeRendererError::InvalidStaticMesh {
                context: "dynamic draw range index is out of bounds",
            },
        )?;
        *texture_index = material_index;
        Ok(())
    }

    /// Explicitly idles and tears down the renderer while the native window is still alive.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanSmokeRendererError`] when the renderer cannot reach a
    /// stable idle point before teardown begins.
    pub fn shutdown(mut self) -> Result<VulkanSmokeShutdownReport, VulkanSmokeRendererError> {
        self.shutdown_inner()
    }

    /// Requests swapchain recreation for a new drawable extent.
    pub fn request_resize(&mut self, extent: (u32, u32)) {
        self.pending_extent = Some(extent);
    }

    fn device_ref(&self) -> Result<&VulkanLogicalDeviceProbe, VulkanSmokeRendererError> {
        self.device
            .as_ref()
            .ok_or(VulkanSmokeRendererError::InvariantViolation {
                context: "logical device",
            })
    }

    fn swapchain_ref(&self) -> Result<&VulkanSwapchainProbe, VulkanSmokeRendererError> {
        self.swapchain
            .as_ref()
            .ok_or(VulkanSmokeRendererError::InvariantViolation {
                context: "swapchain",
            })
    }

    fn instance_ref(&self) -> Result<&VulkanInstanceProbe, VulkanSmokeRendererError> {
        self.instance
            .as_ref()
            .ok_or(VulkanSmokeRendererError::InvariantViolation {
                context: "instance",
            })
    }

    fn textures_ref(&self) -> Result<&[super::VulkanAllocatedImage], VulkanSmokeRendererError> {
        if self.textures.is_empty() {
            Err(VulkanSmokeRendererError::InvariantViolation {
                context: "static material textures",
            })
        } else {
            Ok(&self.textures)
        }
    }

    fn surface_ref(&self) -> Result<&VulkanSurfaceProbe, VulkanSmokeRendererError> {
        self.surface
            .as_ref()
            .ok_or(VulkanSmokeRendererError::InvariantViolation { context: "surface" })
    }

    fn resources_ref(&self) -> Result<&VulkanSwapchainResources, VulkanSmokeRendererError> {
        self.swapchain_resources
            .as_ref()
            .ok_or(VulkanSmokeRendererError::InvariantViolation {
                context: "swapchain resources",
            })
    }

    fn vertex_buffer_ref(&self) -> Result<&VulkanAllocatedBuffer, VulkanSmokeRendererError> {
        self.vertex_buffer
            .as_ref()
            .ok_or(VulkanSmokeRendererError::InvariantViolation {
                context: "vertex buffer",
            })
    }

    fn index_buffer_ref(&self) -> Result<&VulkanAllocatedBuffer, VulkanSmokeRendererError> {
        self.index_buffer
            .as_ref()
            .ok_or(VulkanSmokeRendererError::InvariantViolation {
                context: "index buffer",
            })
    }

    /// Draws and presents one indexed-triangle frame.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanSmokeRendererError`] when synchronization, command recording,
    /// submission, or presentation fails.

    pub fn draw_frame(&mut self) -> Result<VulkanSmokeFrameOutcome, VulkanSmokeRendererError> {
        if let Some(extent) = self.pending_extent.take() {
            if extent.0 == 0 || extent.1 == 0 {
                self.pending_extent = Some(extent);
                return Ok(VulkanSmokeFrameOutcome::ZeroExtent);
            }
            self.recreate_swapchain(extent)?;
            return Ok(VulkanSmokeFrameOutcome::Recreated);
        }

        let sync = self.frame_sync.get(self.current_frame).ok_or(
            VulkanSmokeRendererError::InvariantViolation {
                context: "frame synchronization state",
            },
        )?;
        let image_available = sync.image_available;
        let in_flight_fence = sync.fence;
        // SAFETY: The fence belongs to this live logical device and is waited from one thread.
        unsafe {
            self.device_ref()?
                .device()
                .wait_for_fences(&[in_flight_fence], true, 1_000_000_000)
        }
        .map_err(|error| VulkanSmokeRendererError::VulkanOperation {
            context: "vkWaitForFences",
            result: error,
        })?;
        let legacy_point_light = self.frame_uniforms.point_light;
        let point_light_count = if self.point_lights_explicit {
            self.point_lights.len()
        } else {
            usize::from(legacy_point_light.active)
        };
        let needs_point_light_growth =
            point_light_count > self.resources_ref()?.point_light_capacity;
        if needs_point_light_growth {
            // Updating every material's binding 6 and replacing the shared
            // SSBO requires all prior command buffers to finish.
            // SAFETY: The renderer owns the logical device and no submission
            // can observe the replacement after this idle point.
            unsafe { self.device_ref()?.device().device_wait_idle() }.map_err(|result| {
                VulkanSmokeRendererError::VulkanOperation {
                    context: "vkDeviceWaitIdle(point-light buffer growth)",
                    result,
                }
            })?;
            let instance =
                self.instance
                    .as_ref()
                    .ok_or(VulkanSmokeRendererError::InvariantViolation {
                        context: "Vulkan instance during point-light growth",
                    })?;
            let device =
                self.device
                    .as_ref()
                    .ok_or(VulkanSmokeRendererError::InvariantViolation {
                        context: "logical device during point-light growth",
                    })?;
            let resources = self.swapchain_resources.as_mut().ok_or(
                VulkanSmokeRendererError::InvariantViolation {
                    context: "swapchain resources during point-light growth",
                },
            )?;
            ensure_point_light_capacity(instance, device, resources, point_light_count)?;
        }
        if self.material_specular_dirty {
            // The storage buffer is shared by all in-flight frames.  Wait for
            // every prior use before replacing the records so a setter cannot
            // race a vertex shader from the previous frame.
            // SAFETY: The renderer owns the logical device and no submission
            // can observe the host write after this idle point.
            unsafe { self.device_ref()?.device().device_wait_idle() }.map_err(|result| {
                VulkanSmokeRendererError::VulkanOperation {
                    context: "vkDeviceWaitIdle(material specular update)",
                    result,
                }
            })?;
            let resources = self.resources_ref()?;
            write_material_specular_buffer(
                self.device_ref()?,
                &resources.material_specular_buffer,
                resources.material_specular_stride,
                &self.material_specular,
            )?;
            self.material_specular_dirty = false;
        }
        // SAFETY: The swapchain, semaphore and fence inputs are live for the duration of the acquire call.
        let acquire = unsafe {
            self.swapchain_ref()?.loader().acquire_next_image(
                self.swapchain_ref()?.swapchain(),
                1_000_000_000,
                image_available,
                vk::Fence::null(),
            )
        };
        let (image_index, acquire_suboptimal) = match acquire {
            Ok(result) => result,
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                self.recreate_swapchain(self.report.swapchain_extent)?;
                return Ok(VulkanSmokeFrameOutcome::Recreated);
            }
            Err(error) => {
                return Err(VulkanSmokeRendererError::VulkanOperation {
                    context: "vkAcquireNextImageKHR",
                    result: error,
                });
            }
        };
        let image_index_usize = usize::try_from(image_index).unwrap_or(0);
        let image_fence = self.images_in_flight[image_index_usize];
        if image_fence != vk::Fence::null() {
            // SAFETY: The fence belongs to this renderer and can be waited independently.
            unsafe {
                self.device_ref()?
                    .device()
                    .wait_for_fences(&[image_fence], true, 1_000_000_000)
            }
            .map_err(|error| VulkanSmokeRendererError::VulkanOperation {
                context: "vkWaitForFences(image)",
                result: error,
            })?;
        }
        self.images_in_flight[image_index_usize] = in_flight_fence;
        let frame_bytes = self.frame_uniforms.to_ne_bytes();
        {
            let explicit_point_lights = self.point_lights_explicit;
            let legacy_point_light = self.frame_uniforms.point_light;
            let point_lights = if explicit_point_lights {
                self.point_lights.as_slice()
            } else if legacy_point_light.active {
                std::slice::from_ref(&legacy_point_light)
            } else {
                &[]
            };
            let device =
                self.device
                    .as_ref()
                    .ok_or(VulkanSmokeRendererError::InvariantViolation {
                        context: "logical device during frame upload",
                    })?;
            let resources = self.swapchain_resources.as_ref().ok_or(
                VulkanSmokeRendererError::InvariantViolation {
                    context: "swapchain resources during frame upload",
                },
            )?;
            write_frame_uniform(
                device,
                &resources.frame_uniform_buffer,
                resources.frame_uniform_stride,
                self.current_frame,
                &frame_bytes,
            )?;
            write_point_light_buffer(
                device,
                &resources.point_light_buffer,
                resources.point_light_stride,
                self.current_frame,
                point_lights,
            )?;
        }
        let render_finished = self
            .resources_ref()?
            .render_finished
            .get(image_index_usize)
            .copied()
            .ok_or(VulkanSmokeRendererError::InvariantViolation {
                context: "render-finished semaphore for swapchain image",
            })?;
        self.record_command_buffer(image_index_usize)?;
        // Reset only after recording succeeds. A recording error leaves the
        // signaled fence available for the next recovery attempt.
        // SAFETY: The fence belongs to this frame context and is not in use after the wait above.
        unsafe { self.device_ref()?.device().reset_fences(&[in_flight_fence]) }.map_err(
            |error| VulkanSmokeRendererError::VulkanOperation {
                context: "vkResetFences",
                result: error,
            },
        )?;
        let wait_semaphores = [image_available];
        let wait_stages = [vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT];
        let command_buffers = [self.resources_ref()?.command_buffers[image_index_usize]];
        let signal_semaphores = [render_finished];
        let submit_info = [vk::SubmitInfo::default()
            .wait_semaphores(&wait_semaphores)
            .wait_dst_stage_mask(&wait_stages)
            .command_buffers(&command_buffers)
            .signal_semaphores(&signal_semaphores)];
        // SAFETY: Submission references live queue, sync objects and recorded command buffer.
        unsafe {
            self.device_ref()?.device().queue_submit(
                self.device_ref()?.graphics_queue(),
                &submit_info,
                in_flight_fence,
            )
        }
        .map_err(|error| VulkanSmokeRendererError::VulkanOperation {
            context: "vkQueueSubmit",
            result: error,
        })?;
        if self.readback_enabled && !self.resources_ref()?.readback_buffers.is_empty() {
            self.report.readback_copy_count = self.report.readback_copy_count.saturating_add(1);
            self.last_readback_image_index = Some(image_index_usize);
        }

        let present_wait = [render_finished];
        let swapchains = [self.swapchain_ref()?.swapchain()];
        let image_indices = [image_index];
        let present_info = vk::PresentInfoKHR::default()
            .wait_semaphores(&present_wait)
            .swapchains(&swapchains)
            .image_indices(&image_indices);
        // SAFETY: Presentation uses the rendered image index and a semaphore signaled by queue submission.
        let present_suboptimal = match unsafe {
            self.swapchain_ref()?
                .loader()
                .queue_present(self.device_ref()?.present_queue(), &present_info)
        } {
            Ok(suboptimal) => suboptimal,
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                self.recreate_swapchain(self.report.swapchain_extent)?;
                return Ok(VulkanSmokeFrameOutcome::Recreated);
            }
            Err(error) => {
                return Err(VulkanSmokeRendererError::VulkanOperation {
                    context: "vkQueuePresentKHR",
                    result: error,
                });
            }
        };

        self.current_frame = (self.current_frame + 1) % self.frame_sync.len().max(1);
        if acquire_suboptimal || present_suboptimal {
            self.recreate_swapchain(self.report.swapchain_extent)?;
            Ok(VulkanSmokeFrameOutcome::Recreated)
        } else {
            Ok(VulkanSmokeFrameOutcome::Presented)
        }
    }

    fn recreate_swapchain(&mut self, extent: (u32, u32)) -> Result<(), VulkanSmokeRendererError> {
        let device = self.device_ref()?;
        // SAFETY: The logical device remains live and idling at swapchain recreation boundaries.
        unsafe { device.device().device_wait_idle() }.map_err(|error| {
            VulkanSmokeRendererError::VulkanOperation {
                context: "vkDeviceWaitIdle",
                result: error,
            }
        })?;
        self.pending_extent = None;
        self.rebuild_swapchain(extent)?;
        self.swapchain_recreate_count = self.swapchain_recreate_count.saturating_add(1);
        Ok(())
    }

    fn rebuild_swapchain(&mut self, extent: (u32, u32)) -> Result<(), VulkanSmokeRendererError> {
        self.destroy_swapchain_resources();
        let instance = self.instance_ref()?;
        let surface = self.surface_ref()?;
        let device = self.device_ref()?;
        let old_swapchain = self
            .swapchain
            .as_ref()
            .map_or(vk::SwapchainKHR::null(), VulkanSwapchainProbe::swapchain);
        let new_swapchain = create_vulkan_swapchain_probe_for_extent(
            instance,
            surface,
            device,
            extent,
            old_swapchain,
        )
        .map_err(VulkanSmokeRendererError::Swapchain)?;
        self.swapchain = Some(new_swapchain);
        self.rebuild_swapchain_resources(true)?;
        Ok(())
    }

    fn rebuild_swapchain_resources(
        &mut self,
        reuse_command_pool: bool,
    ) -> Result<(), VulkanSmokeRendererError> {
        let device = self.device_ref()?;
        let swapchain = self.swapchain_ref()?;
        let resources = RollbackOnDrop::new(
            create_swapchain_resources(
                self.instance_ref()?,
                device,
                swapchain,
                self.command_pool,
                self.vertex_buffer_ref()?,
                self.index_buffer_ref()?,
                self.textures_ref()?,
                &self.texture_slot_indices,
                &self.draw_ranges,
                self.depth_request,
                reuse_command_pool,
            )?,
            |resources| destroy_swapchain_resources(device, self.command_pool, resources),
        );
        let frame_sync = create_frame_sync(device)?;
        let swapchain_extent = self.swapchain_ref()?.report.plan.extent;
        let swapchain_image_count = self.swapchain_ref()?.report.image_count;
        let swapchain_image_usage = self.swapchain_ref()?.report.plan.image_usage;
        let resources = resources.commit();
        self.images_in_flight = vec![vk::Fence::null(); resources.image_views.len()];
        self.frame_sync = frame_sync;
        self.last_readback_image_index = None;
        self.material_specular_dirty = true;
        self.report.swapchain_extent = swapchain_extent;
        self.report.swapchain_image_count = swapchain_image_count;
        self.report.swapchain_image_format = self.swapchain_ref()?.report.plan.format.format;
        self.report.swapchain_image_usage = swapchain_image_usage;
        self.swapchain_resources = Some(resources);
        Ok(())
    }

    /// Rebuilds only the swapchain-owned pipeline bundle after a structural
    /// range-state change.  Draw-range setters can be called while a renderer
    /// is live, so all prior submissions are completed before handles are
    /// destroyed and recreated.
    fn rebuild_pipeline_resources(&mut self) -> Result<(), VulkanSmokeRendererError> {
        if self.swapchain_resources.is_none() {
            return Ok(());
        }
        let device = self.device_ref()?;
        // SAFETY: The renderer owns every submission on this device.  Waiting
        // here makes the old pipelines, framebuffers, and command buffers
        // quiescent before their destruction.
        unsafe { device.device().device_wait_idle() }.map_err(|result| {
            VulkanSmokeRendererError::VulkanOperation {
                context: "vkDeviceWaitIdle(draw range pipeline update)",
                result,
            }
        })?;
        self.destroy_swapchain_resources();
        self.rebuild_swapchain_resources(true)
    }

    /// Recomputes the stable command-recording order. Without explicit
    /// backgrounds, opaque world ranges followed by the sorted transparent
    /// world ranges replace the existing world slots. With explicit
    /// backgrounds, opaque ranges compact into the world slots and sorted
    /// transparent ranges are inserted after the last background; unrelated
    /// fixed ranges keep relative order but may shift slots. The transparent
    /// queue matches the native descending key comparator.
    fn rebuild_draw_order(&mut self) {
        let range_count = self.draw_ranges.len();
        if self.draw_order.len() != range_count {
            self.draw_order = (0..range_count).collect();
        }
        self.draw_order = sorted_world_draw_order(
            range_count,
            &self.draw_ranges,
            &self.draw_range_world_queue,
            &self.draw_range_sort_keys,
            &self.world_transparent_background_ranges,
        );
    }

    fn record_command_buffer(
        &mut self,
        image_index: usize,
    ) -> Result<(), VulkanSmokeRendererError> {
        if self.draw_order_dirty {
            self.rebuild_draw_order();
            self.draw_order_dirty = false;
        }
        let device = self.device_ref()?;
        let swapchain = self.swapchain_ref()?;
        let resources = self.resources_ref()?;
        let command_buffer = resources.command_buffers[image_index];
        // SAFETY: The command buffer belongs to the resettable pool owned by this renderer.
        unsafe {
            device
                .device()
                .reset_command_buffer(command_buffer, vk::CommandBufferResetFlags::empty())
        }
        .map_err(|error| VulkanSmokeRendererError::VulkanOperation {
            context: "vkResetCommandBuffer",
            result: error,
        })?;
        let begin_info = vk::CommandBufferBeginInfo::default();
        // SAFETY: The command buffer is in the initial state after reset and recorded on one thread.
        unsafe {
            device
                .device()
                .begin_command_buffer(command_buffer, &begin_info)
        }
        .map_err(|error| VulkanSmokeRendererError::VulkanOperation {
            context: "vkBeginCommandBuffer",
            result: error,
        })?;

        let clear_values = [
            vk::ClearValue {
                color: vk::ClearColorValue {
                    float32: [0.05, 0.08, 0.11, 1.0],
                },
            },
            vk::ClearValue {
                depth_stencil: vk::ClearDepthStencilValue {
                    depth: 1.0,
                    stencil: 0,
                },
            },
        ];
        let render_area = vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: vk::Extent2D {
                width: swapchain.report.plan.extent.0,
                height: swapchain.report.plan.extent.1,
            },
        };
        let render_pass_info = vk::RenderPassBeginInfo::default()
            .render_pass(resources.render_pass)
            .framebuffer(resources.framebuffers[image_index])
            .render_area(render_area)
            .clear_values(&clear_values);
        // SAFETY: All commands target live frame resources owned by this renderer.
        unsafe {
            device.device().cmd_begin_render_pass(
                command_buffer,
                &render_pass_info,
                vk::SubpassContents::INLINE,
            );
            let vertex_buffers = [self.vertex_buffer_ref()?.buffer];
            let offsets = [0_u64];
            device
                .device()
                .cmd_bind_vertex_buffers(command_buffer, 0, &vertex_buffers, &offsets);
            device.device().cmd_bind_index_buffer(
                command_buffer,
                self.index_buffer_ref()?.buffer,
                0,
                vk::IndexType::UINT32,
            );
            for &range_index in &self.draw_order {
                let range = self.draw_ranges.get(range_index).ok_or(
                    VulkanSmokeRendererError::InvariantViolation {
                        context: "stable draw order range handle",
                    },
                )?;
                let texture_index = *self.draw_texture_indices.get(range_index).ok_or(
                    VulkanSmokeRendererError::InvariantViolation {
                        context: "stable draw texture range handle",
                    },
                )?;
                let pipeline = resources.pipelines.get(&range.pipeline_key()).ok_or(
                    VulkanSmokeRendererError::InvariantViolation {
                        context: "static draw pipeline variant",
                    },
                )?;
                device.device().cmd_bind_pipeline(
                    command_buffer,
                    vk::PipelineBindPoint::GRAPHICS,
                    *pipeline,
                );
                let frame_offset = u32::try_from(
                    resources
                        .frame_uniform_stride
                        .saturating_mul(u64::try_from(self.current_frame).unwrap_or(u64::MAX)),
                )
                .unwrap_or(u32::MAX);
                let material_offset = u32::try_from(
                    resources
                        .material_specular_stride
                        .saturating_mul(u64::try_from(texture_index).unwrap_or(u64::MAX)),
                )
                .unwrap_or(u32::MAX);
                let point_light_offset = u32::try_from(
                    resources
                        .point_light_stride
                        .saturating_mul(u64::try_from(self.current_frame).unwrap_or(u64::MAX)),
                )
                .unwrap_or(u32::MAX);
                let descriptor_set = resources.descriptor_sets.get(texture_index).ok_or(
                    VulkanSmokeRendererError::InvariantViolation {
                        context: "static material descriptor set",
                    },
                )?;
                device.device().cmd_bind_descriptor_sets(
                    command_buffer,
                    vk::PipelineBindPoint::GRAPHICS,
                    resources.pipeline_layout,
                    0,
                    std::slice::from_ref(descriptor_set),
                    &[frame_offset, material_offset, point_light_offset],
                );
                let alpha_cutoff = range.alpha_test_cutoff().to_ne_bytes();
                device.device().cmd_push_constants(
                    command_buffer,
                    resources.pipeline_layout,
                    vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                    0,
                    &alpha_cutoff,
                );
                let material_alpha = self.material_alphas.get(texture_index).ok_or(
                    VulkanSmokeRendererError::InvariantViolation {
                        context: "static material alpha",
                    },
                )?;
                let material_alphas = [
                    material_alpha[0].to_ne_bytes(),
                    material_alpha[1].to_ne_bytes(),
                ]
                .concat();
                device.device().cmd_push_constants(
                    command_buffer,
                    resources.pipeline_layout,
                    vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                    4,
                    &material_alphas,
                );
                let material_lighting = self.material_lighting.get(texture_index).ok_or(
                    VulkanSmokeRendererError::InvariantViolation {
                        context: "static material lighting",
                    },
                )?;
                let material_lighting = material_lighting
                    .iter()
                    .flatten()
                    .flat_map(|value| value.to_ne_bytes())
                    .collect::<Vec<_>>();
                device.device().cmd_push_constants(
                    command_buffer,
                    resources.pipeline_layout,
                    vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                    12,
                    &material_lighting,
                );
                let material_uv_transforms = self.material_uv_transforms.get(texture_index).ok_or(
                    VulkanSmokeRendererError::InvariantViolation {
                        context: "static material UV transforms",
                    },
                )?;
                let material_uv_transforms = material_uv_transforms
                    .iter()
                    .flatten()
                    .flat_map(|value| value.to_ne_bytes())
                    .collect::<Vec<_>>();
                device.device().cmd_push_constants(
                    command_buffer,
                    resources.pipeline_layout,
                    vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                    60,
                    &material_uv_transforms,
                );
                let unlit = self.material_unlit.get(texture_index).ok_or(
                    VulkanSmokeRendererError::InvariantViolation {
                        context: "static material unlit flag",
                    },
                )?;
                let unlit = [unlit.to_ne_bytes()];
                device.device().cmd_push_constants(
                    command_buffer,
                    resources.pipeline_layout,
                    vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                    124,
                    &unlit.concat(),
                );
                device.device().cmd_draw_indexed(
                    command_buffer,
                    range.index_count,
                    1,
                    range.first_index,
                    0,
                    0,
                );
            }
            device.device().cmd_end_render_pass(command_buffer);
            if self.readback_enabled {
                if let (Some(image), Some(readback)) = (
                    resources.images.get(image_index),
                    resources.readback_buffers.get(image_index),
                ) {
                    let range = super::color_subresource_range(1);
                    let to_transfer = vk::ImageMemoryBarrier::default()
                        .old_layout(vk::ImageLayout::PRESENT_SRC_KHR)
                        .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                        .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
                        .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                        .image(*image)
                        .subresource_range(range);
                    let back_to_present = vk::ImageMemoryBarrier::default()
                        .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                        .new_layout(vk::ImageLayout::PRESENT_SRC_KHR)
                        .src_access_mask(vk::AccessFlags::TRANSFER_READ)
                        .dst_access_mask(vk::AccessFlags::empty())
                        .image(*image)
                        .subresource_range(range);
                    let region = vk::BufferImageCopy::default()
                        .image_subresource(
                            vk::ImageSubresourceLayers::default()
                                .aspect_mask(vk::ImageAspectFlags::COLOR)
                                .mip_level(0)
                                .base_array_layer(0)
                                .layer_count(1),
                        )
                        .image_extent(vk::Extent3D {
                            width: swapchain.report.plan.extent.0,
                            height: swapchain.report.plan.extent.1,
                            depth: 1,
                        });
                    device.device().cmd_pipeline_barrier(
                        command_buffer,
                        vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                        vk::PipelineStageFlags::TRANSFER,
                        vk::DependencyFlags::empty(),
                        &[],
                        &[],
                        &[to_transfer],
                    );
                    device.device().cmd_copy_image_to_buffer(
                        command_buffer,
                        *image,
                        vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                        readback.buffer,
                        &[region],
                    );
                    device.device().cmd_pipeline_barrier(
                        command_buffer,
                        vk::PipelineStageFlags::TRANSFER,
                        vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                        vk::DependencyFlags::empty(),
                        &[],
                        &[],
                        &[back_to_present],
                    );
                }
            }
        }

        // SAFETY: The render pass owns the attachment layout transitions for this clear-and-present path.
        unsafe { device.device().end_command_buffer(command_buffer) }.map_err(|error| {
            VulkanSmokeRendererError::VulkanOperation {
                context: "vkEndCommandBuffer",
                result: error,
            }
        })?;
        Ok(())
    }

    fn destroy_swapchain_resources(&mut self) {
        let Some(device) = self.device.as_ref() else {
            return;
        };
        for sync in self.frame_sync.drain(..) {
            // SAFETY: These sync objects belong to this device and are destroyed once.
            unsafe {
                device
                    .device()
                    .destroy_semaphore(sync.image_available, None);
                device.device().destroy_fence(sync.fence, None);
            }
        }
        if let Some(resources) = self.swapchain_resources.take() {
            destroy_swapchain_resources(device, self.command_pool, resources);
        }
        self.images_in_flight.clear();
        self.current_frame = 0;
    }

    fn destroy_device_owned_resources(&mut self) {
        self.destroy_swapchain_resources();
        if let Some(device) = self.device.as_ref() {
            for texture in self.textures.drain(..) {
                destroy_allocated_image(device, &texture);
            }
            if let Some(buffer) = self.index_buffer.take() {
                // SAFETY: Buffer and memory belong to this device and are destroyed once after the device has been idled and frame work has been torn down.
                unsafe {
                    device.device().destroy_buffer(buffer.buffer, None);
                    device.device().free_memory(buffer.memory, None);
                }
            }
            if let Some(buffer) = self.vertex_buffer.take() {
                // SAFETY: Buffer and memory belong to this device and are destroyed once after the device has been idled and frame work has been torn down.
                unsafe {
                    device.device().destroy_buffer(buffer.buffer, None);
                    device.device().free_memory(buffer.memory, None);
                }
            }
            // SAFETY: The command pool belongs to this device and is destroyed once after the device is idle and all command buffers allocated from it were freed above.
            unsafe {
                device
                    .device()
                    .destroy_command_pool(self.command_pool, None);
            };
        }
        self.pending_extent = None;
    }

    fn shutdown_inner(&mut self) -> Result<VulkanSmokeShutdownReport, VulkanSmokeRendererError> {
        let completed_readback = if let Some(device) = self.device.as_ref() {
            // SAFETY: The logical device remains live until teardown finishes and idling prevents in-flight work from touching swapchain, buffers, sync objects or the command pool after destruction starts.
            unsafe { device.device().device_wait_idle() }.map_err(|error| {
                VulkanSmokeRendererError::VulkanOperation {
                    context: "vkDeviceWaitIdle",
                    result: error,
                }
            })?;
            self.completed_readback(device)?
        } else {
            None
        };
        let readback_artifact = completed_readback.map(|bytes| {
            self.report.readback_byte_count = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            self.report.readback_fnv1a64 = fnv1a64(&bytes);
            VulkanReadbackArtifact {
                format: self
                    .swapchain_ref()
                    .map_or(0, |swapchain| swapchain.report.plan.format.format),
                width: self.report.swapchain_extent.0,
                height: self.report.swapchain_extent.1,
                bytes,
            }
        });
        self.destroy_device_owned_resources();
        let validation = take_runtime_children_with_validation_snapshot(
            &mut self.surface,
            &mut self.device,
            &mut self.swapchain,
            self.validation.as_ref(),
            VulkanValidationMessenger::report,
        )
        .unwrap_or_default();
        self.validation.take();
        self.instance.take();
        Ok(VulkanSmokeShutdownReport {
            renderer_report: self.report.clone(),
            readback_artifact,
            swapchain_recreate_count: self.swapchain_recreate_count,
            validation,
        })
    }

    fn completed_readback(
        &self,
        device: &VulkanLogicalDeviceProbe,
    ) -> Result<Option<Vec<u8>>, VulkanSmokeRendererError> {
        let Some(resources) = self.swapchain_resources.as_ref() else {
            return Ok(None);
        };
        let byte_len = usize::try_from(self.report.swapchain_extent.0)
            .ok()
            .and_then(|width| {
                usize::try_from(self.report.swapchain_extent.1)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or(VulkanSmokeRendererError::InvalidStaticMesh {
                context: "completed readback byte length",
            })?;
        if !self.readback_enabled || resources.readback_buffers.is_empty() {
            return Ok(None);
        }
        let Some(image_index) = completed_readback_buffer_index(
            self.last_readback_image_index,
            resources.readback_buffers.len(),
        )?
        else {
            return Ok(None);
        };
        let buffer = resources.readback_buffers.get(image_index).ok_or(
            VulkanSmokeRendererError::InvariantViolation {
                context: "last readback image index",
            },
        )?;
        Ok(Some(readback_buffer_bytes(device, buffer, byte_len)?))
    }

    fn teardown(&mut self) {
        if let Some(device) = self.device.as_ref() {
            // SAFETY: The logical device remains live until teardown finishes and idling prevents in-flight work from touching swapchain, buffers, sync objects or the command pool after destruction starts.
            let _ = unsafe { device.device().device_wait_idle() };
        }
        self.destroy_device_owned_resources();
        let _ = take_runtime_children_with_validation_snapshot(
            &mut self.surface,
            &mut self.device,
            &mut self.swapchain,
            self.validation.as_ref(),
            VulkanValidationMessenger::report,
        );
        self.validation.take();
        self.instance.take();
    }
}

fn completed_readback_buffer_index(
    last_image_index: Option<usize>,
    buffer_count: usize,
) -> Result<Option<usize>, VulkanSmokeRendererError> {
    match last_image_index {
        None => Ok(None),
        Some(index) if index < buffer_count => Ok(Some(index)),
        Some(_) => Err(VulkanSmokeRendererError::InvariantViolation {
            context: "last readback image index",
        }),
    }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

impl Drop for VulkanSmokeRenderer {
    fn drop(&mut self) {
        self.teardown();
    }
}

fn validate_geometry_upload(
    vertices: &[super::VulkanStaticVertex],
    indices: &[u32],
    current_ranges: &[super::VulkanStaticDrawRange],
    geometry_ranges: &[super::VulkanDynamicDrawRange],
    retained_vertex_capacity: usize,
    retained_index_capacity: usize,
) -> Result<(), VulkanSmokeRendererError> {
    if vertices.is_empty() || indices.is_empty() {
        return Err(VulkanSmokeRendererError::InvalidStaticMesh {
            context: "dynamic mesh upload cannot have zero-sized geometry",
        });
    }
    if !indices.len().is_multiple_of(3) {
        return Err(VulkanSmokeRendererError::InvalidStaticMesh {
            context: "dynamic mesh index upload is not a triangle list",
        });
    }
    if vertices.iter().any(|vertex| {
        !vertex
            .position
            .iter()
            .chain(vertex.color.iter())
            .chain(vertex.normal.iter())
            .chain(vertex.uv.iter())
            .chain(vertex.detail_uv.iter())
            .chain(std::iter::once(&vertex.overlay_alpha))
            .all(|value| value.is_finite())
            || !(0.0..=1.0).contains(&vertex.overlay_alpha)
    }) {
        return Err(VulkanSmokeRendererError::InvalidStaticMesh {
            context: "dynamic mesh vertex attributes must be finite and overlay alpha must be 0..1",
        });
    }
    if indices.iter().any(|&index| {
        usize::try_from(index)
            .ok()
            .is_none_or(|index| index >= vertices.len())
    }) {
        return Err(VulkanSmokeRendererError::InvalidStaticMesh {
            context: "dynamic mesh index exceeds vertex count",
        });
    }
    if retained_vertex_capacity == 0 || retained_index_capacity == 0 {
        return Err(VulkanSmokeRendererError::InvariantViolation {
            context: "dynamic mesh retained allocation is empty",
        });
    }

    let mut next_ranges = current_ranges.to_vec();
    let mut changed = vec![false; current_ranges.len()];
    for geometry in geometry_ranges {
        if geometry.range_index >= current_ranges.len() {
            return Err(VulkanSmokeRendererError::InvalidStaticMesh {
                context: "dynamic mesh range index is out of bounds",
            });
        }
        if changed[geometry.range_index] {
            return Err(VulkanSmokeRendererError::InvalidStaticMesh {
                context: "dynamic mesh range is updated more than once",
            });
        }
        changed[geometry.range_index] = true;
        if geometry.first_index % 3 != 0 || geometry.index_count % 3 != 0 {
            return Err(VulkanSmokeRendererError::InvalidStaticMesh {
                context: "dynamic mesh range is not triangle aligned",
            });
        }
        let first = usize::try_from(geometry.first_index).map_err(|_| {
            VulkanSmokeRendererError::InvalidStaticMesh {
                context: "dynamic mesh range first index exceeds address space",
            }
        })?;
        let count = usize::try_from(geometry.index_count).map_err(|_| {
            VulkanSmokeRendererError::InvalidStaticMesh {
                context: "dynamic mesh range index count exceeds address space",
            }
        })?;
        let end = first
            .checked_add(count)
            .ok_or(VulkanSmokeRendererError::InvalidStaticMesh {
                context: "dynamic mesh range exceeds address space",
            })?;
        if end > indices.len() {
            return Err(VulkanSmokeRendererError::InvalidStaticMesh {
                context: "dynamic mesh range exceeds uploaded indices",
            });
        }
        next_ranges[geometry.range_index].first_index = geometry.first_index;
        next_ranges[geometry.range_index].index_count = geometry.index_count;
    }

    for range in next_ranges {
        if range.first_index % 3 != 0 || range.index_count % 3 != 0 {
            return Err(VulkanSmokeRendererError::InvalidStaticMesh {
                context: "retained dynamic mesh range is not triangle aligned",
            });
        }
        let first = usize::try_from(range.first_index).map_err(|_| {
            VulkanSmokeRendererError::InvalidStaticMesh {
                context: "retained dynamic mesh range first index exceeds address space",
            }
        })?;
        let count = usize::try_from(range.index_count).map_err(|_| {
            VulkanSmokeRendererError::InvalidStaticMesh {
                context: "retained dynamic mesh range index count exceeds address space",
            }
        })?;
        let end = first
            .checked_add(count)
            .ok_or(VulkanSmokeRendererError::InvalidStaticMesh {
                context: "retained dynamic mesh range exceeds address space",
            })?;
        if end > indices.len() {
            return Err(VulkanSmokeRendererError::InvalidStaticMesh {
                context: "retained dynamic mesh range exceeds uploaded indices",
            });
        }
    }
    Ok(())
}

fn apply_geometry_range_updates(
    ranges: &mut [super::VulkanStaticDrawRange],
    capacities: &mut [u32],
    geometry_ranges: &[super::VulkanDynamicDrawRange],
) {
    for geometry in geometry_ranges {
        // `validate_geometry_upload` has already checked every handle. Keep
        // this helper infallible so state is changed only after the Vulkan
        // upload succeeds.
        let range = &mut ranges[geometry.range_index];
        range.first_index = geometry.first_index;
        range.index_count = geometry.index_count;
        // A moved segment no longer owns the old tail. Keeping the previous
        // range capacity would let a later count setter read indices from a
        // different segment. The shared GPU allocation itself remains
        // retained; this is the logical capacity of this range's segment.
        capacities[geometry.range_index] = geometry.index_count;
    }
}

fn apply_world_transparency(
    base_state: LegacyPipelineState,
    transparent: bool,
) -> LegacyPipelineState {
    if transparent && base_state.depth != LegacyDepthMode::Disabled {
        LegacyPipelineState {
            depth: LegacyDepthMode::TestReadOnly,
            ..base_state
        }
    } else {
        base_state
    }
}

fn world_material_pipeline_states(
    original: LegacyPipelineState,
    category: u8,
    world_queue: Option<bool>,
) -> Option<(LegacyPipelineState, LegacyPipelineState)> {
    let categorized = original.with_material_category(category)?;
    let Some(transparent) = world_queue else {
        return Some((original, original));
    };
    let base = categorized;
    Some((base, apply_world_transparency(base, transparent)))
}

/// Replays the native queue construction from the original stable range
/// order. Without explicit background ranges, world opaque ranges replace the
/// existing world slots and world transparent ranges follow in descending
/// camera distance order. With explicit backgrounds, opaque ranges are
/// compacted into world positions and sorted transparent ranges are inserted
/// after the last active background; fixed passes keep their relative order but
/// can shift slots. Zero-index-count ranges are inactive and are omitted.
fn sorted_world_draw_order(
    range_count: usize,
    ranges: &[super::VulkanStaticDrawRange],
    world_queue: &[Option<bool>],
    keys: &[Option<f32>],
    transparent_background_ranges: &[usize],
) -> Vec<usize> {
    let mut next = (0..range_count).collect::<Vec<_>>();
    let mut world_opaque = Vec::new();
    let mut world_transparent = Vec::new();
    for range_index in 0..range_count {
        if ranges
            .get(range_index)
            .is_none_or(|range| range.index_count == 0)
        {
            continue;
        }
        match world_queue.get(range_index).copied().flatten() {
            Some(false) => world_opaque.push(range_index),
            Some(true) => world_transparent.push(range_index),
            None => {}
        }
    }

    // Native equal-key insertion is a midpoint insertion rather than a
    // documented stable sort. Replay that insertion shape from original range
    // order instead of repeatedly sorting the previous frame's order.
    let mut ordered_transparent = Vec::with_capacity(world_transparent.len());
    for range_index in world_transparent {
        let new_key = keys.get(range_index).and_then(|key| *key);
        let mut low = 0usize;
        let mut high = ordered_transparent.len().saturating_sub(1);
        while high > low {
            let middle = low + (high - low) / 2;
            let existing_key = keys.get(ordered_transparent[middle]).and_then(|key| *key);
            match (new_key, existing_key) {
                (Some(new_key), Some(existing_key)) if new_key > existing_key => high = middle,
                (Some(new_key), Some(existing_key)) if new_key < existing_key => low = middle + 1,
                (Some(_), Some(_)) => {
                    low = middle;
                    break;
                }
                (Some(_), None) => high = middle,
                (None, Some(_)) => low = middle + 1,
                (None, None) => {
                    low = middle;
                    break;
                }
            }
        }
        if !ordered_transparent.is_empty() && high == low {
            let existing_key = keys.get(ordered_transparent[low]).and_then(|key| *key);
            let insert_after = match (new_key, existing_key) {
                (Some(new_key), Some(existing_key)) => new_key < existing_key,
                (None, Some(_)) => true,
                _ => false,
            };
            if insert_after {
                low += 1;
            }
        }
        ordered_transparent.insert(low, range_index);
    }

    if transparent_background_ranges.is_empty() {
        let mut world_order = world_opaque;
        world_order.extend(ordered_transparent);
        let mut world_cursor = 0usize;
        for slot in &mut next {
            if world_queue.get(*slot).copied().flatten().is_some()
                && ranges
                    .get(*slot)
                    .is_some_and(|range| range.index_count != 0)
            {
                if let Some(&replacement) = world_order.get(world_cursor) {
                    *slot = replacement;
                    world_cursor += 1;
                }
            }
        }
        return next;
    }

    // Fixed sky backgrounds are not world-queue members. Remove the world
    // transparent slots, compact opaque ranges into the world slots, then
    // insert the sorted transparent queue after the explicit background group.
    let mut opaque_cursor = 0usize;
    let mut next_without_transparency = Vec::with_capacity(range_count);
    for slot in 0..range_count {
        let active_world = ranges.get(slot).is_some_and(|range| range.index_count != 0);
        match world_queue.get(slot).copied().flatten() {
            Some(true) if active_world => {}
            Some(false) if active_world => {
                if let Some(&replacement) = world_opaque.get(opaque_cursor) {
                    next_without_transparency.push(replacement);
                    opaque_cursor += 1;
                } else {
                    next_without_transparency.push(slot);
                }
            }
            _ => next_without_transparency.push(slot),
        }
    }
    let insertion = transparent_background_ranges
        .iter()
        .filter_map(|background| {
            next_without_transparency
                .iter()
                .position(|range| range == background)
        })
        .max()
        .map(|index| index + 1);
    if let Some(insertion) = insertion {
        next_without_transparency.splice(insertion..insertion, ordered_transparent);
    }
    next_without_transparency
}

#[cfg(test)]
mod tests {
    use super::super::{VulkanDynamicDrawRange, VulkanStaticDrawRange, VulkanStaticMesh};
    use super::{
        apply_geometry_range_updates, apply_world_transparency, completed_readback_buffer_index,
        sorted_world_draw_order, take_runtime_children_with_validation_snapshot,
        take_runtime_owners_in_dependency_order, validate_geometry_upload,
        world_material_pipeline_states, RollbackOnDrop,
    };
    use fparkan_render::{LegacyBlendMode, LegacyCullMode, LegacyDepthMode, LegacyPipelineState};
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum TeardownStep {
        Snapshot,
        Instance,
        Validation,
        Surface,
        Device,
        Swapchain,
    }

    struct DropTracker {
        step: TeardownStep,
        log: Rc<RefCell<Vec<TeardownStep>>>,
    }

    impl Drop for DropTracker {
        fn drop(&mut self) {
            self.log.borrow_mut().push(self.step);
        }
    }

    fn tracker(step: TeardownStep, log: &Rc<RefCell<Vec<TeardownStep>>>) -> DropTracker {
        DropTracker {
            step,
            log: Rc::clone(log),
        }
    }

    fn record_teardown_steps(present_steps: &[TeardownStep]) -> Vec<TeardownStep> {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut instance = present_steps
            .contains(&TeardownStep::Instance)
            .then(|| tracker(TeardownStep::Instance, &log));
        let mut validation = present_steps
            .contains(&TeardownStep::Validation)
            .then(|| tracker(TeardownStep::Validation, &log));
        let mut surface = present_steps
            .contains(&TeardownStep::Surface)
            .then(|| tracker(TeardownStep::Surface, &log));
        let mut device = present_steps
            .contains(&TeardownStep::Device)
            .then(|| tracker(TeardownStep::Device, &log));
        let mut swapchain = present_steps
            .contains(&TeardownStep::Swapchain)
            .then(|| tracker(TeardownStep::Swapchain, &log));

        take_runtime_owners_in_dependency_order(
            &mut instance,
            &mut validation,
            &mut surface,
            &mut device,
            &mut swapchain,
        );
        Rc::into_inner(log)
            .expect("all drop trackers released")
            .into_inner()
    }

    #[test]
    fn runtime_owners_drop_in_explicit_dependency_order() {
        assert_eq!(
            record_teardown_steps(&[
                TeardownStep::Instance,
                TeardownStep::Validation,
                TeardownStep::Surface,
                TeardownStep::Device,
                TeardownStep::Swapchain,
            ]),
            vec![
                TeardownStep::Swapchain,
                TeardownStep::Device,
                TeardownStep::Surface,
                TeardownStep::Validation,
                TeardownStep::Instance,
            ]
        );
    }

    #[test]
    fn completed_readback_selects_only_the_last_submitted_image() {
        assert_eq!(completed_readback_buffer_index(None, 2), Ok(None));
        assert_eq!(completed_readback_buffer_index(Some(1), 2), Ok(Some(1)));
        assert!(completed_readback_buffer_index(Some(2), 2).is_err());
    }

    #[test]
    fn geometry_updates_preserve_other_range_runtime_state() {
        let base_state = LegacyPipelineState {
            blend: LegacyBlendMode::SourceAlpha,
            depth: LegacyDepthMode::TestReadOnly,
            ..LegacyPipelineState::default()
        };
        let mut ranges = vec![
            VulkanStaticDrawRange {
                first_index: 0,
                index_count: 3,
                material_index: 7,
                lightmap_index: 4,
                batch_flags: 0x108,
                pipeline_state: base_state,
                alpha_test_reference: 17,
            },
            VulkanStaticDrawRange {
                first_index: 3,
                index_count: 3,
                material_index: 9,
                lightmap_index: u8::MAX,
                batch_flags: 0,
                pipeline_state: LegacyPipelineState::default(),
                alpha_test_reference: 0,
            },
        ];
        let original = ranges[0];
        let mut capacities = vec![3, 3];
        apply_geometry_range_updates(
            &mut ranges,
            &mut capacities,
            &[VulkanDynamicDrawRange {
                range_index: 1,
                first_index: 3,
                index_count: 9,
            }],
        );
        assert_eq!(ranges[0], original);
        assert_eq!(capacities, vec![3, 9]);
        assert_eq!(ranges[1].first_index, 3);
        assert_eq!(ranges[1].index_count, 9);

        apply_geometry_range_updates(
            &mut ranges,
            &mut capacities,
            &[VulkanDynamicDrawRange {
                range_index: 1,
                first_index: 3,
                index_count: 3,
            }],
        );
        assert_eq!(ranges[0], original);
        assert_eq!(capacities, vec![3, 3]);
        assert_eq!(ranges[1].index_count, 3);
    }

    #[test]
    fn geometry_update_can_disable_empty_shadow_range_without_zero_sized_upload() {
        let mesh = VulkanStaticMesh::smoke_triangle();
        let geometry = [VulkanDynamicDrawRange {
            range_index: 0,
            first_index: 3,
            index_count: 0,
        }];
        assert_eq!(
            validate_geometry_upload(
                &mesh.vertices,
                &mesh.indices,
                &mesh.draw_ranges,
                &geometry,
                3,
                3,
            ),
            Ok(())
        );
    }

    #[test]
    fn runtime_owners_drop_remaining_children_after_partial_init_failures() {
        let cases = [
            (vec![TeardownStep::Instance], vec![TeardownStep::Instance]),
            (
                vec![TeardownStep::Instance, TeardownStep::Validation],
                vec![TeardownStep::Validation, TeardownStep::Instance],
            ),
            (
                vec![
                    TeardownStep::Instance,
                    TeardownStep::Validation,
                    TeardownStep::Surface,
                ],
                vec![
                    TeardownStep::Surface,
                    TeardownStep::Validation,
                    TeardownStep::Instance,
                ],
            ),
            (
                vec![
                    TeardownStep::Instance,
                    TeardownStep::Validation,
                    TeardownStep::Surface,
                    TeardownStep::Device,
                ],
                vec![
                    TeardownStep::Device,
                    TeardownStep::Surface,
                    TeardownStep::Validation,
                    TeardownStep::Instance,
                ],
            ),
            (
                vec![
                    TeardownStep::Instance,
                    TeardownStep::Surface,
                    TeardownStep::Device,
                    TeardownStep::Swapchain,
                ],
                vec![
                    TeardownStep::Swapchain,
                    TeardownStep::Device,
                    TeardownStep::Surface,
                    TeardownStep::Instance,
                ],
            ),
        ];

        for (present_steps, expected) in cases {
            assert_eq!(record_teardown_steps(&present_steps), expected);
        }
    }

    #[test]
    fn final_validation_snapshot_is_captured_after_surface_device_swapchain_drop() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut instance = Some(tracker(TeardownStep::Instance, &log));
        let mut validation = Some(tracker(TeardownStep::Validation, &log));
        let mut surface = Some(tracker(TeardownStep::Surface, &log));
        let mut device = Some(tracker(TeardownStep::Device, &log));
        let mut swapchain = Some(tracker(TeardownStep::Swapchain, &log));

        let snapshot = take_runtime_children_with_validation_snapshot(
            &mut surface,
            &mut device,
            &mut swapchain,
            validation.as_ref(),
            |_| {
                log.borrow_mut().push(TeardownStep::Snapshot);
                TeardownStep::Validation
            },
        );
        validation.take();
        instance.take();

        assert_eq!(snapshot, Some(TeardownStep::Validation));
        assert_eq!(
            Rc::into_inner(log)
                .expect("all drop trackers released")
                .into_inner(),
            vec![
                TeardownStep::Swapchain,
                TeardownStep::Device,
                TeardownStep::Surface,
                TeardownStep::Snapshot,
                TeardownStep::Validation,
                TeardownStep::Instance,
            ]
        );
    }

    #[test]
    fn rollback_guard_runs_cleanup_when_later_step_fails() {
        let log = Rc::new(RefCell::new(Vec::new()));
        {
            let _guard = RollbackOnDrop::new(tracker(TeardownStep::Swapchain, &log), |tracker| {
                drop(tracker)
            });
        }

        assert_eq!(log.borrow().as_slice(), &[TeardownStep::Swapchain]);
    }

    #[test]
    fn rollback_guard_skips_cleanup_after_commit() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let tracker = RollbackOnDrop::new(tracker(TeardownStep::Swapchain, &log), |tracker| {
            drop(tracker)
        })
        .commit();
        assert!(log.borrow().is_empty());

        drop(tracker);

        assert_eq!(log.borrow().as_slice(), &[TeardownStep::Swapchain]);
    }

    #[test]
    fn native_transparent_order_is_descending_and_keeps_fixed_pass_slots() {
        let ranges = (0..5)
            .map(|_| super::super::VulkanStaticDrawRange {
                first_index: 0,
                index_count: 3,
                material_index: 0,
                lightmap_index: u8::MAX,
                batch_flags: 0,
                pipeline_state: fparkan_render::LegacyPipelineState::default(),
                alpha_test_reference: 0,
            })
            .collect::<Vec<_>>();
        let order = sorted_world_draw_order(
            5,
            &ranges,
            &[None, Some(true), None, Some(true), Some(false)],
            &[None, Some(2.0), None, Some(9.0), Some(-3.0)],
            &[],
        );
        assert_eq!(order, vec![0, 4, 2, 3, 1]);
    }

    #[test]
    fn inactive_world_range_is_not_reused_as_a_sort_slot() {
        let ranges = vec![
            super::super::VulkanStaticDrawRange {
                first_index: 0,
                index_count: 3,
                material_index: 0,
                lightmap_index: u8::MAX,
                batch_flags: 0,
                pipeline_state: fparkan_render::LegacyPipelineState::default(),
                alpha_test_reference: 0,
            },
            super::super::VulkanStaticDrawRange {
                first_index: 3,
                index_count: 0,
                material_index: 0,
                lightmap_index: u8::MAX,
                batch_flags: 0,
                pipeline_state: fparkan_render::LegacyPipelineState::default(),
                alpha_test_reference: 0,
            },
            super::super::VulkanStaticDrawRange {
                first_index: 3,
                index_count: 3,
                material_index: 0,
                lightmap_index: u8::MAX,
                batch_flags: 0,
                pipeline_state: fparkan_render::LegacyPipelineState::default(),
                alpha_test_reference: 0,
            },
        ];
        let order = sorted_world_draw_order(
            3,
            &ranges,
            &[None, Some(true), Some(true)],
            &[None, Some(100.0), Some(1.0)],
            &[],
        );
        assert_eq!(order, vec![0, 1, 2]);
    }

    #[test]
    fn native_equal_key_insertion_uses_inclusive_midpoint() {
        let ranges = (0..4)
            .map(|index| super::super::VulkanStaticDrawRange {
                first_index: index * 3,
                index_count: 3,
                material_index: 0,
                lightmap_index: u8::MAX,
                batch_flags: 0,
                pipeline_state: fparkan_render::LegacyPipelineState::default(),
                alpha_test_reference: 0,
            })
            .collect::<Vec<_>>();
        let order = sorted_world_draw_order(4, &ranges, &[Some(true); 4], &[Some(1.0); 4], &[]);
        assert_eq!(order, vec![2, 3, 1, 0]);
    }

    #[test]
    fn tree_canopy_queue_follows_sky_and_precedes_foreground_effects() {
        const SCREEN_GRADIENT: usize = 0;
        const TERRAIN: usize = 1;
        const CANOPY_NEAR: usize = 2;
        const BUILDING: usize = 3;
        const CANOPY_FAR: usize = 4;
        const SHADOW: usize = 5;
        const NEBULA: usize = 6;
        const DOME: usize = 7;
        const CLOUDS: usize = 8;
        const SUN: usize = 9;
        const WEATHER: usize = 10;
        let ranges = (0..11)
            .map(|index| VulkanStaticDrawRange {
                first_index: index * 3,
                index_count: 3,
                material_index: 0,
                lightmap_index: u8::MAX,
                batch_flags: 0,
                pipeline_state: LegacyPipelineState::default(),
                alpha_test_reference: 0,
            })
            .collect::<Vec<_>>();
        let mut world_queue = vec![None; ranges.len()];
        world_queue[TERRAIN] = Some(false);
        world_queue[CANOPY_NEAR] = Some(true);
        world_queue[BUILDING] = Some(false);
        world_queue[CANOPY_FAR] = Some(true);
        let mut sort_keys = vec![None; ranges.len()];
        sort_keys[CANOPY_NEAR] = Some(2.0);
        sort_keys[CANOPY_FAR] = Some(9.0);
        let order = sorted_world_draw_order(
            ranges.len(),
            &ranges,
            &world_queue,
            &sort_keys,
            &[NEBULA, DOME, CLOUDS],
        );
        assert_eq!(
            order,
            vec![
                SCREEN_GRADIENT,
                TERRAIN,
                BUILDING,
                SHADOW,
                NEBULA,
                DOME,
                CLOUDS,
                CANOPY_FAR,
                CANOPY_NEAR,
                SUN,
                WEATHER,
            ]
        );
    }

    #[test]
    fn material_category_and_transparency_preserve_world_state_in_either_order() {
        let original = LegacyPipelineState {
            blend: LegacyBlendMode::Opaque,
            depth: LegacyDepthMode::TestWrite,
            cull: LegacyCullMode::FrontFace,
            alpha_test: false,
        };
        let (base, category_first) =
            world_material_pipeline_states(original, 1, Some(true)).expect("category 1");
        let transparency_first = apply_world_transparency(original, true)
            .with_material_category(1)
            .expect("category 1 after transparency");
        assert_eq!(category_first, transparency_first);
        assert_eq!(base.blend, LegacyBlendMode::SourceAlpha);
        assert!(base.alpha_test);
        assert_eq!(base.depth, LegacyDepthMode::TestWrite);
        assert_eq!(base.cull, LegacyCullMode::FrontFace);
        assert_eq!(category_first.depth, LegacyDepthMode::TestReadOnly);
        assert_eq!(category_first.cull, LegacyCullMode::FrontFace);

        let fixed_state = LegacyPipelineState {
            blend: LegacyBlendMode::Additive,
            depth: LegacyDepthMode::Disabled,
            cull: LegacyCullMode::Disabled,
            alpha_test: false,
        };
        assert_eq!(
            world_material_pipeline_states(fixed_state, 1, None),
            Some((fixed_state, fixed_state))
        );
        assert!(world_material_pipeline_states(original, 5, Some(false)).is_none());
        assert!(world_material_pipeline_states(fixed_state, 5, None).is_none());
    }
}
