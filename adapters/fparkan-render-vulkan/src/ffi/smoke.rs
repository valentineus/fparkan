#![allow(unsafe_code)]

use ash::vk;

use super::{
    create_command_pool, create_frame_sync, create_static_mesh_index_buffer,
    create_static_mesh_vertex_buffer, create_static_texture_image, create_swapchain_resources,
    create_validation_messenger, create_vulkan_instance_probe,
    create_vulkan_logical_device_probe_for_request, create_vulkan_surface_probe,
    create_vulkan_swapchain_probe_for_extent, destroy_allocated_buffer, destroy_allocated_image,
    destroy_swapchain_resources, plan_vulkan_surface, VulkanAllocatedBuffer, VulkanInstanceConfig,
    VulkanInstanceProbe, VulkanLogicalDeviceProbe, VulkanSmokeFrameOutcome, VulkanSmokeRenderer,
    VulkanSmokeRendererCreateInfo, VulkanSmokeRendererError, VulkanSmokeRendererReport,
    VulkanSmokeShutdownReport, VulkanSurfaceProbe, VulkanSwapchainProbe, VulkanSwapchainResources,
    VulkanValidationMessenger, VulkanValidationReport,
};
use crate::policy::KHR_PORTABILITY_SUBSET_EXTENSION;
use crate::shader_manifest::{triangle_shader_manifest, validate_shader_manifest};

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
    #[allow(clippy::too_many_lines)]
    pub fn new(
        create_info: &VulkanSmokeRendererCreateInfo,
    ) -> Result<Self, VulkanSmokeRendererError> {
        create_info
            .mesh
            .validate()
            .map_err(|context| VulkanSmokeRendererError::InvalidStaticMesh { context })?;
        if let Some(texture) = &create_info.texture {
            texture
                .validate()
                .map_err(|context| VulkanSmokeRendererError::InvalidStaticTexture { context })?;
        }
        let bootstrap_progress = create_info.bootstrap_progress.as_ref();
        let shader_manifest = validate_shader_manifest(&triangle_shader_manifest())
            .map_err(VulkanSmokeRendererError::ShaderManifest)?;
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
        // Keep the compatibility triangle path valid: it samples a white texel
        // when no original TEXM was supplied.
        let fallback_texture = super::VulkanStaticTexture {
            width: 1,
            height: 1,
            rgba8: vec![255, 255, 255, 255],
        };
        let texture_source = create_info.texture.as_ref().unwrap_or(&fallback_texture);
        let texture =
            match create_static_texture_image(&instance, &device, command_pool, texture_source) {
                Ok(image) => Some(image),
                Err(error) => {
                    // SAFETY: These resources belong to this live device and are rolled back before it drops.
                    unsafe { device.device().destroy_command_pool(command_pool, None) };
                    destroy_allocated_buffer(&device, &index_buffer);
                    destroy_allocated_buffer(&device, &vertex_buffer);
                    return Err(error);
                }
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
            texture,
            index_count: u32::try_from(create_info.mesh.indices.len()).unwrap_or(u32::MAX),
            frame_sync: Vec::new(),
            images_in_flight: Vec::new(),
            current_frame: 0,
            pending_extent: None,
            swapchain_recreate_count: 0,
            report: VulkanSmokeRendererReport {
                shader_manifest_hash: shader_manifest.manifest_hash.clone(),
                portability_enumeration: instance_config.enable_portability_enumeration,
                portability_subset_enabled: false,
                device_name: String::new(),
                graphics_queue_family: 0,
                present_queue_family: 0,
                enabled_extension_count: 0,
                swapchain_extent: (0, 0),
                swapchain_image_count: 0,
            },
        };
        renderer.rebuild_swapchain_resources(false)?;
        let device_ref = renderer.device_ref()?;
        let swapchain_ref = renderer.swapchain_ref()?;
        renderer.report = VulkanSmokeRendererReport {
            shader_manifest_hash: shader_manifest.manifest_hash,
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

    fn texture_ref(&self) -> Result<&super::VulkanAllocatedImage, VulkanSmokeRendererError> {
        self.texture
            .as_ref()
            .ok_or(VulkanSmokeRendererError::InvariantViolation {
                context: "static material texture",
            })
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
    #[allow(clippy::too_many_lines)]
    pub fn draw_frame(&mut self) -> Result<VulkanSmokeFrameOutcome, VulkanSmokeRendererError> {
        if let Some(extent) = self.pending_extent.take() {
            if extent.0 == 0 || extent.1 == 0 {
                self.pending_extent = Some(extent);
                return Ok(VulkanSmokeFrameOutcome::ZeroExtent);
            }
            self.recreate_swapchain(extent)?;
            return Ok(VulkanSmokeFrameOutcome::Recreated);
        }

        let sync = &self.frame_sync[self.current_frame];
        let image_available = sync.image_available;
        let render_finished = sync.render_finished;
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
        // SAFETY: The fence belongs to this frame context and is not in use after the wait above.
        unsafe { self.device_ref()?.device().reset_fences(&[in_flight_fence]) }.map_err(
            |error| VulkanSmokeRendererError::VulkanOperation {
                context: "vkResetFences",
                result: error,
            },
        )?;

        self.record_command_buffer(image_index_usize)?;
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
                device,
                swapchain,
                self.command_pool,
                self.vertex_buffer_ref()?,
                self.index_buffer_ref()?,
                self.texture_ref()?,
                reuse_command_pool,
            )?,
            |resources| destroy_swapchain_resources(device, self.command_pool, resources),
        );
        let frame_sync = create_frame_sync(device)?;
        let swapchain_extent = self.swapchain_ref()?.report.plan.extent;
        let swapchain_image_count = self.swapchain_ref()?.report.image_count;
        let resources = resources.commit();
        self.images_in_flight = vec![vk::Fence::null(); resources.image_views.len()];
        self.frame_sync = frame_sync;
        self.report.swapchain_extent = swapchain_extent;
        self.report.swapchain_image_count = swapchain_image_count;
        self.swapchain_resources = Some(resources);
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn record_command_buffer(
        &mut self,
        image_index: usize,
    ) -> Result<(), VulkanSmokeRendererError> {
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

        let clear_values = [vk::ClearValue {
            color: vk::ClearColorValue {
                float32: [0.05, 0.08, 0.11, 1.0],
            },
        }];
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
            device.device().cmd_bind_pipeline(
                command_buffer,
                vk::PipelineBindPoint::GRAPHICS,
                resources.pipeline,
            );
            device.device().cmd_bind_descriptor_sets(
                command_buffer,
                vk::PipelineBindPoint::GRAPHICS,
                resources.pipeline_layout,
                0,
                &[resources.descriptor_set],
                &[],
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
                vk::IndexType::UINT16,
            );
            device
                .device()
                .cmd_draw_indexed(command_buffer, self.index_count, 1, 0, 0, 0);
            device.device().cmd_end_render_pass(command_buffer);
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
                device
                    .device()
                    .destroy_semaphore(sync.render_finished, None);
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
            if let Some(texture) = self.texture.take() {
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
        if let Some(device) = self.device.as_ref() {
            // SAFETY: The logical device remains live until teardown finishes and idling prevents in-flight work from touching swapchain, buffers, sync objects or the command pool after destruction starts.
            unsafe { device.device().device_wait_idle() }.map_err(|error| {
                VulkanSmokeRendererError::VulkanOperation {
                    context: "vkDeviceWaitIdle",
                    result: error,
                }
            })?;
        }
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
            swapchain_recreate_count: self.swapchain_recreate_count,
            validation,
        })
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

impl Drop for VulkanSmokeRenderer {
    fn drop(&mut self) {
        self.teardown();
    }
}

#[cfg(test)]
mod tests {
    use super::{
        take_runtime_children_with_validation_snapshot, take_runtime_owners_in_dependency_order,
        RollbackOnDrop,
    };
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
}
