#![allow(unsafe_code)]

use ash::vk;
use fparkan_platform::DepthStencilSupport;
use fparkan_render::{
    LegacyBlendMode, LegacyCullMode, LegacyDepthMode, LegacyPipelineState, PipelineKey,
};
use std::collections::BTreeMap;

use super::{
    color_subresource_range, create_depth_attachment, create_material_specular_buffer,
    create_point_light_buffer, destroy_depth_attachment, VulkanAllocatedBuffer,
    VulkanAllocatedImage, VulkanDepthAttachment, VulkanInstanceProbe, VulkanLogicalDeviceProbe,
    VulkanSmokeRendererError, VulkanSwapchainProbe, TRIANGLE_FRAGMENT_SHADER_WORDS,
    TRIANGLE_VERTEX_SHADER_WORDS, VULKAN_FRAME_UNIFORM_BYTES,
    VULKAN_MATERIAL_SPECULAR_RECORD_BYTES,
};

pub(super) struct VulkanSwapchainResources {
    pub(super) image_views: Vec<vk::ImageView>,
    pub(super) images: Vec<vk::Image>,
    pub(super) readback_buffers: Vec<VulkanAllocatedBuffer>,
    /// One depth image per swapchain image so frames can overlap safely.
    pub(super) depth_attachments: Vec<VulkanDepthAttachment>,
    /// A render-finished semaphore dedicated to each swapchain image.
    pub(super) render_finished: Vec<vk::Semaphore>,
    pub(super) render_pass: vk::RenderPass,
    pub(super) pipeline_layout: vk::PipelineLayout,
    pub(super) descriptor_set_layout: vk::DescriptorSetLayout,
    pub(super) descriptor_pool: vk::DescriptorPool,
    pub(super) descriptor_sets: Vec<vk::DescriptorSet>,
    pub(super) sampler: vk::Sampler,
    pub(super) frame_uniform_buffer: VulkanAllocatedBuffer,
    pub(super) frame_uniform_stride: u64,
    pub(super) material_specular_buffer: VulkanAllocatedBuffer,
    pub(super) material_specular_stride: u64,
    pub(super) point_light_buffer: VulkanAllocatedBuffer,
    pub(super) point_light_range: u64,
    pub(super) point_light_stride: u64,
    pub(super) point_light_capacity: usize,
    pub(super) max_point_light_count: usize,
    /// Live graphics pipelines indexed by canonical backend-neutral state.
    pub(super) pipelines: BTreeMap<PipelineKey, vk::Pipeline>,
    pub(super) framebuffers: Vec<vk::Framebuffer>,
    pub(super) command_buffers: Vec<vk::CommandBuffer>,
}

pub(super) fn update_material_texture_descriptor(
    device: &VulkanLogicalDeviceProbe,
    resources: &VulkanSwapchainResources,
    material_index: usize,
    texture_slot: usize,
    texture: &VulkanAllocatedImage,
) -> Result<(), VulkanSmokeRendererError> {
    if texture_slot >= 4 {
        return Err(VulkanSmokeRendererError::InvalidStaticTexture {
            context: "dynamic material texture slot is out of bounds",
        });
    }
    let descriptor_set = resources
        .descriptor_sets
        .get(material_index)
        .copied()
        .ok_or(VulkanSmokeRendererError::InvalidStaticTexture {
            context: "dynamic material descriptor set is out of bounds",
        })?;
    let image_info = vk::DescriptorImageInfo::default()
        .sampler(resources.sampler)
        .image_view(texture.view)
        .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
    let write = vk::WriteDescriptorSet::default()
        .dst_set(descriptor_set)
        .dst_binding(u32::try_from(texture_slot).unwrap_or(u32::MAX))
        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
        .image_info(std::slice::from_ref(&image_info));
    // SAFETY: the renderer idles the device before changing a descriptor that
    // may be referenced by an in-flight command buffer.
    unsafe { device.device().update_descriptor_sets(&[write], &[]) };
    Ok(())
}

/// Grows the dynamic light SSBO when the scene adds lights beyond its current
/// capacity. The caller must idle the device before updating these descriptors.
pub(super) fn ensure_point_light_capacity(
    instance: &VulkanInstanceProbe,
    device: &VulkanLogicalDeviceProbe,
    resources: &mut VulkanSwapchainResources,
    needed: usize,
) -> Result<(), VulkanSmokeRendererError> {
    if needed <= resources.point_light_capacity {
        return Ok(());
    }
    if needed > resources.max_point_light_count {
        return Err(VulkanSmokeRendererError::InvalidPointLights {
            context: "point-light count exceeds maxStorageBufferRange",
        });
    }
    let target = resources
        .point_light_capacity
        .saturating_mul(2)
        .max(1)
        .max(needed)
        .min(resources.max_point_light_count);
    let (new_buffer, new_range, new_stride, max_count) =
        create_point_light_buffer(instance, device, target)?;
    if max_count != resources.max_point_light_count {
        unsafe {
            device.device().destroy_buffer(new_buffer.buffer, None);
            device.device().free_memory(new_buffer.memory, None);
        }
        return Err(VulkanSmokeRendererError::InvariantViolation {
            context: "point-light capacity changed for the active device",
        });
    }
    let info = vk::DescriptorBufferInfo::default()
        .buffer(new_buffer.buffer)
        .offset(0)
        .range(new_range);
    let writes = resources
        .descriptor_sets
        .iter()
        .copied()
        .map(|descriptor_set| {
            vk::WriteDescriptorSet::default()
                .dst_set(descriptor_set)
                .dst_binding(6)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER_DYNAMIC)
                .buffer_info(std::slice::from_ref(&info))
        })
        .collect::<Vec<_>>();
    // SAFETY: The caller waited for device idle before replacing a descriptor
    // that submitted command buffers may reference.
    unsafe { device.device().update_descriptor_sets(&writes, &[]) };
    let old_buffer = std::mem::replace(&mut resources.point_light_buffer, new_buffer);
    resources.point_light_range = new_range;
    resources.point_light_stride = new_stride;
    resources.point_light_capacity = target;
    // SAFETY: All prior submissions have completed and descriptors now point
    // at the replacement allocation.
    unsafe {
        device.device().destroy_buffer(old_buffer.buffer, None);
        device.device().free_memory(old_buffer.memory, None);
    }
    Ok(())
}

struct PartialSwapchainResources {
    image_views: Vec<vk::ImageView>,
    readback_buffers: Vec<VulkanAllocatedBuffer>,
    depth_attachments: Vec<VulkanDepthAttachment>,
    render_finished: Vec<vk::Semaphore>,
    render_pass: Option<vk::RenderPass>,
    pipeline_layout: Option<vk::PipelineLayout>,
    descriptor_set_layout: Option<vk::DescriptorSetLayout>,
    descriptor_pool: Option<vk::DescriptorPool>,
    sampler: Option<vk::Sampler>,
    frame_uniform_buffer: Option<VulkanAllocatedBuffer>,
    frame_uniform_stride: u64,
    material_specular_buffer: Option<VulkanAllocatedBuffer>,
    material_specular_stride: u64,
    point_light_buffer: Option<VulkanAllocatedBuffer>,
    point_light_range: u64,
    point_light_stride: u64,
    point_light_capacity: usize,
    max_point_light_count: usize,
    pipelines: BTreeMap<PipelineKey, vk::Pipeline>,
    framebuffers: Vec<vk::Framebuffer>,
    command_buffers: Vec<vk::CommandBuffer>,
}

pub(super) fn create_swapchain_resources(
    instance: &VulkanInstanceProbe,
    device: &VulkanLogicalDeviceProbe,
    swapchain: &VulkanSwapchainProbe,
    command_pool: vk::CommandPool,
    vertex_buffer: &VulkanAllocatedBuffer,
    index_buffer: &VulkanAllocatedBuffer,
    textures: &[VulkanAllocatedImage],
    texture_slot_indices: &[usize],
    draw_ranges: &[super::VulkanStaticDrawRange],
    depth_request: DepthStencilSupport,
    reuse_command_pool: bool,
) -> Result<VulkanSwapchainResources, VulkanSmokeRendererError> {
    // SAFETY: The swapchain is live and owned by this renderer for the duration of the query.
    let images = unsafe {
        swapchain
            .loader()
            .get_swapchain_images(swapchain.swapchain())
    }
    .map_err(|error| VulkanSmokeRendererError::VulkanOperation {
        context: "vkGetSwapchainImagesKHR",
        result: error,
    })?;
    let mut partial = PartialSwapchainResources {
        image_views: create_swapchain_image_views(
            device,
            &images,
            swapchain.report.plan.format.format,
        )?,
        readback_buffers: Vec::new(),
        depth_attachments: Vec::new(),
        render_finished: Vec::new(),
        render_pass: None,
        pipeline_layout: None,
        descriptor_set_layout: None,
        descriptor_pool: None,
        sampler: None,
        frame_uniform_buffer: None,
        frame_uniform_stride: 0,
        material_specular_buffer: None,
        material_specular_stride: 0,
        point_light_buffer: None,
        point_light_range: 0,
        point_light_stride: 0,
        point_light_capacity: 0,
        max_point_light_count: 0,
        pipelines: BTreeMap::new(),
        framebuffers: Vec::new(),
        command_buffers: Vec::new(),
    };
    let semaphore_info = vk::SemaphoreCreateInfo::default();
    for _ in &images {
        // SAFETY: The semaphore belongs to this live logical device and is
        // destroyed with the swapchain resources.
        match unsafe { device.device().create_semaphore(&semaphore_info, None) } {
            Ok(semaphore) => partial.render_finished.push(semaphore),
            Err(result) => {
                destroy_partial_swapchain_resources(device, command_pool, partial);
                return Err(VulkanSmokeRendererError::VulkanOperation {
                    context: "vkCreateSemaphore(render_finished)",
                    result,
                });
            }
        }
    }
    let (frame_uniform_buffer, frame_uniform_stride) =
        match super::create_frame_uniform_buffer(instance, device) {
            Ok(buffer) => buffer,
            Err(error) => {
                destroy_partial_swapchain_resources(device, command_pool, partial);
                return Err(error);
            }
        };
    partial.frame_uniform_stride = frame_uniform_stride;
    partial.frame_uniform_buffer = Some(frame_uniform_buffer);
    let material_count = texture_slot_indices.len() / 4;
    let (material_specular_buffer, material_specular_stride) =
        match create_material_specular_buffer(instance, device, material_count) {
            Ok(buffer) => buffer,
            Err(error) => {
                destroy_partial_swapchain_resources(device, command_pool, partial);
                return Err(error);
            }
        };
    partial.material_specular_stride = material_specular_stride;
    partial.material_specular_buffer = Some(material_specular_buffer);
    let point_light_capacity = 1;
    let (point_light_buffer, point_light_range, point_light_stride, max_point_light_count) =
        match create_point_light_buffer(instance, device, point_light_capacity) {
            Ok(buffer) => buffer,
            Err(error) => {
                destroy_partial_swapchain_resources(device, command_pool, partial);
                return Err(error);
            }
        };
    partial.point_light_range = point_light_range;
    partial.point_light_stride = point_light_stride;
    partial.point_light_capacity = point_light_capacity;
    partial.max_point_light_count = max_point_light_count;
    partial.point_light_buffer = Some(point_light_buffer);
    let (
        depth_attachment,
        render_pass,
        pipeline_layout,
        pipelines,
        descriptor_set_layout,
        descriptor_pool,
        descriptor_sets,
        sampler,
    ) = match create_swapchain_pipeline_bundle(
        instance,
        device,
        swapchain.report.plan.format.format,
        swapchain.report.plan.extent,
        textures,
        texture_slot_indices,
        draw_ranges,
        depth_request,
        partial
            .frame_uniform_buffer
            .as_ref()
            .expect("frame uniform buffer is present before descriptor creation"),
        partial
            .material_specular_buffer
            .as_ref()
            .expect("material specular buffer is present before descriptor creation"),
        partial
            .point_light_buffer
            .as_ref()
            .expect("point-light buffer is present before descriptor creation"),
        partial.point_light_range,
    ) {
        Ok(bundle) => bundle,
        Err(error) => {
            destroy_partial_swapchain_resources(device, command_pool, partial);
            return Err(error);
        }
    };
    // Keep every handle from the bundle in the partial owner before creating
    // the remaining per-image depth attachments. That loop can fail after the
    // first attachment, and its error path must destroy the whole bundle.
    partial.depth_attachments.push(depth_attachment);
    partial.render_pass = Some(render_pass);
    partial.pipeline_layout = Some(pipeline_layout);
    partial.descriptor_set_layout = Some(descriptor_set_layout);
    partial.descriptor_pool = Some(descriptor_pool);
    partial.sampler = Some(sampler);
    partial.pipelines = pipelines;
    for _ in 1..images.len() {
        match create_depth_attachment(
            instance,
            device,
            swapchain.report.plan.extent,
            depth_request,
        ) {
            Ok(attachment) => partial.depth_attachments.push(attachment),
            Err(error) => {
                destroy_partial_swapchain_resources(device, command_pool, partial);
                return Err(error);
            }
        }
    }
    let framebuffers = match create_swapchain_framebuffers(
        device,
        render_pass,
        &partial.image_views,
        &partial.depth_attachments,
        swapchain.report.plan.extent,
    ) {
        Ok(framebuffers) => framebuffers,
        Err(error) => {
            destroy_partial_swapchain_resources(device, command_pool, partial);
            return Err(error);
        }
    };
    partial.framebuffers = framebuffers;
    if vk::ImageUsageFlags::from_raw(swapchain.report.plan.image_usage)
        .contains(vk::ImageUsageFlags::TRANSFER_SRC)
    {
        let byte_len = usize::try_from(swapchain.report.plan.extent.0)
            .ok()
            .and_then(|width| {
                usize::try_from(swapchain.report.plan.extent.1)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or(VulkanSmokeRendererError::InvalidStaticMesh {
                context: "swapchain readback byte length",
            })?;
        for _ in &images {
            match super::create_readback_buffer(instance, device, byte_len) {
                Ok(buffer) => partial.readback_buffers.push(buffer),
                Err(error) => {
                    destroy_partial_swapchain_resources(device, command_pool, partial);
                    return Err(error);
                }
            }
        }
    }
    reset_reusable_command_pool(device, command_pool, reuse_command_pool)?;
    let command_buffers = match allocate_command_buffers(
        device,
        command_pool,
        u32::try_from(images.len()).unwrap_or(u32::MAX),
    ) {
        Ok(command_buffers) => command_buffers,
        Err(error) => {
            destroy_partial_swapchain_resources(device, command_pool, partial);
            return Err(error);
        }
    };
    partial.command_buffers = command_buffers;
    let _ = (vertex_buffer, index_buffer);
    if partial.depth_attachments.len() != images.len()
        || partial.render_finished.len() != images.len()
    {
        destroy_partial_swapchain_resources(device, command_pool, partial);
        return Err(VulkanSmokeRendererError::InvariantViolation {
            context: "swapchain per-image resource count after setup",
        });
    }
    Ok(VulkanSwapchainResources {
        image_views: partial.image_views,
        images,
        readback_buffers: partial.readback_buffers,
        depth_attachments: partial.depth_attachments,
        render_finished: partial.render_finished,
        render_pass: partial
            .render_pass
            .expect("render pass is present after successful swapchain setup"),
        pipeline_layout: partial
            .pipeline_layout
            .expect("pipeline layout is present after successful swapchain setup"),
        descriptor_set_layout: partial
            .descriptor_set_layout
            .expect("descriptor set layout is present after successful swapchain setup"),
        descriptor_pool: partial
            .descriptor_pool
            .expect("descriptor pool is present after successful swapchain setup"),
        descriptor_sets,
        sampler: partial
            .sampler
            .expect("sampler is present after successful swapchain setup"),
        frame_uniform_buffer: partial
            .frame_uniform_buffer
            .expect("frame uniform buffer is present after successful swapchain setup"),
        frame_uniform_stride: partial.frame_uniform_stride,
        material_specular_buffer: partial
            .material_specular_buffer
            .expect("material specular buffer is present after successful swapchain setup"),
        material_specular_stride: partial.material_specular_stride,
        point_light_buffer: partial
            .point_light_buffer
            .expect("point-light buffer is present after successful swapchain setup"),
        point_light_range: partial.point_light_range,
        point_light_stride: partial.point_light_stride,
        point_light_capacity: partial.point_light_capacity,
        max_point_light_count: partial.max_point_light_count,
        pipelines: partial.pipelines,
        framebuffers: partial.framebuffers,
        command_buffers: partial.command_buffers,
    })
}

fn create_swapchain_image_views(
    device: &VulkanLogicalDeviceProbe,
    images: &[vk::Image],
    format: i32,
) -> Result<Vec<vk::ImageView>, VulkanSmokeRendererError> {
    let mut image_views = Vec::with_capacity(images.len());
    for image in images.iter().copied() {
        image_views.push(create_image_view(device, image, format)?);
    }
    Ok(image_views)
}

fn create_swapchain_pipeline_bundle(
    instance: &VulkanInstanceProbe,
    device: &VulkanLogicalDeviceProbe,
    format: i32,
    extent: (u32, u32),
    textures: &[VulkanAllocatedImage],
    texture_slot_indices: &[usize],
    draw_ranges: &[super::VulkanStaticDrawRange],
    depth_request: DepthStencilSupport,
    frame_uniform_buffer: &VulkanAllocatedBuffer,
    material_specular_buffer: &VulkanAllocatedBuffer,
    point_light_buffer: &VulkanAllocatedBuffer,
    point_light_range: u64,
) -> Result<
    (
        VulkanDepthAttachment,
        vk::RenderPass,
        vk::PipelineLayout,
        BTreeMap<PipelineKey, vk::Pipeline>,
        vk::DescriptorSetLayout,
        vk::DescriptorPool,
        Vec<vk::DescriptorSet>,
        vk::Sampler,
    ),
    VulkanSmokeRendererError,
> {
    let depth_attachment = create_depth_attachment(instance, device, extent, depth_request)?;
    let render_pass = match create_render_pass(device, format, depth_attachment.format) {
        Ok(render_pass) => render_pass,
        Err(error) => {
            destroy_depth_attachment(device, &depth_attachment);
            return Err(error);
        }
    };
    let (descriptor_set_layout, descriptor_pool, descriptor_sets, sampler) =
        create_texture_descriptor_bundle(
            device,
            textures,
            texture_slot_indices,
            frame_uniform_buffer,
            material_specular_buffer,
            point_light_buffer,
            point_light_range,
        )
        .inspect_err(|_| {
            // SAFETY: The render pass was created above on this live logical device and is destroyed on setup failure.
            unsafe { device.device().destroy_render_pass(render_pass, None) };
            destroy_depth_attachment(device, &depth_attachment);
        })?;
    let pipeline_layout =
        create_pipeline_layout(device, descriptor_set_layout).inspect_err(|_| {
            // SAFETY: The descriptor resources and render pass were created above and are rolled back on this device.
            unsafe {
                device.device().destroy_sampler(sampler, None);
                device
                    .device()
                    .destroy_descriptor_pool(descriptor_pool, None);
                device
                    .device()
                    .destroy_descriptor_set_layout(descriptor_set_layout, None);
                device.device().destroy_render_pass(render_pass, None);
            }
            destroy_depth_attachment(device, &depth_attachment);
        })?;
    let pipelines =
        create_graphics_pipeline_cache(device, render_pass, pipeline_layout, extent, draw_ranges)
            .inspect_err(|_| {
            // SAFETY: These objects were created above on this live logical device and are destroyed on setup failure.
            unsafe {
                device
                    .device()
                    .destroy_pipeline_layout(pipeline_layout, None);
                device.device().destroy_sampler(sampler, None);
                device
                    .device()
                    .destroy_descriptor_pool(descriptor_pool, None);
                device
                    .device()
                    .destroy_descriptor_set_layout(descriptor_set_layout, None);
                device.device().destroy_render_pass(render_pass, None);
            }
            destroy_depth_attachment(device, &depth_attachment);
        })?;
    Ok((
        depth_attachment,
        render_pass,
        pipeline_layout,
        pipelines,
        descriptor_set_layout,
        descriptor_pool,
        descriptor_sets,
        sampler,
    ))
}

fn create_swapchain_framebuffers(
    device: &VulkanLogicalDeviceProbe,
    render_pass: vk::RenderPass,
    image_views: &[vk::ImageView],
    depth_attachments: &[VulkanDepthAttachment],
    extent: (u32, u32),
) -> Result<Vec<vk::Framebuffer>, VulkanSmokeRendererError> {
    if image_views.len() != depth_attachments.len() {
        return Err(VulkanSmokeRendererError::InvariantViolation {
            context: "swapchain color/depth attachment count",
        });
    }
    let mut framebuffers = Vec::with_capacity(image_views.len());
    for (image_view, depth_attachment) in image_views.iter().copied().zip(depth_attachments) {
        match create_framebuffer(
            device,
            render_pass,
            image_view,
            depth_attachment.image.view,
            extent,
        ) {
            Ok(framebuffer) => framebuffers.push(framebuffer),
            Err(error) => {
                // SAFETY: These framebuffers were created above on this live logical device and are destroyed on setup failure.
                unsafe {
                    for framebuffer in framebuffers.iter().copied() {
                        device.device().destroy_framebuffer(framebuffer, None);
                    }
                }
                return Err(error);
            }
        }
    }
    Ok(framebuffers)
}

fn reset_reusable_command_pool(
    device: &VulkanLogicalDeviceProbe,
    command_pool: vk::CommandPool,
    reuse_command_pool: bool,
) -> Result<(), VulkanSmokeRendererError> {
    if !reuse_command_pool {
        return Ok(());
    }
    // SAFETY: All command buffers allocated from the live pool are freed before reallocating them.
    unsafe {
        device
            .device()
            .reset_command_pool(command_pool, vk::CommandPoolResetFlags::empty())
    }
    .map_err(|error| VulkanSmokeRendererError::VulkanOperation {
        context: "vkResetCommandPool",
        result: error,
    })
}

fn create_image_view(
    device: &VulkanLogicalDeviceProbe,
    image: vk::Image,
    format: i32,
) -> Result<vk::ImageView, VulkanSmokeRendererError> {
    let create_info = vk::ImageViewCreateInfo::default()
        .image(image)
        .view_type(vk::ImageViewType::TYPE_2D)
        .format(vk::Format::from_raw(format))
        .subresource_range(color_subresource_range(1));
    // SAFETY: The image comes from the live swapchain and the subresource range covers its color aspect.
    unsafe { device.device().create_image_view(&create_info, None) }.map_err(|error| {
        VulkanSmokeRendererError::VulkanOperation {
            context: "vkCreateImageView",
            result: error,
        }
    })
}

fn create_render_pass(
    device: &VulkanLogicalDeviceProbe,
    format: i32,
    depth_format: vk::Format,
) -> Result<vk::RenderPass, VulkanSmokeRendererError> {
    let color_attachment = vk::AttachmentDescription::default()
        .format(vk::Format::from_raw(format))
        .samples(vk::SampleCountFlags::TYPE_1)
        .load_op(vk::AttachmentLoadOp::CLEAR)
        .store_op(vk::AttachmentStoreOp::STORE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .final_layout(vk::ImageLayout::PRESENT_SRC_KHR);
    let color_attachment_ref = vk::AttachmentReference::default()
        .attachment(0)
        .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
    let depth_attachment = vk::AttachmentDescription::default()
        .format(depth_format)
        .samples(vk::SampleCountFlags::TYPE_1)
        .load_op(vk::AttachmentLoadOp::CLEAR)
        .store_op(vk::AttachmentStoreOp::DONT_CARE)
        .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
        .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .final_layout(vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL);
    let depth_attachment_ref = vk::AttachmentReference::default()
        .attachment(1)
        .layout(vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL);
    let color_attachments = [color_attachment_ref];
    let subpass = vk::SubpassDescription::default()
        .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
        .color_attachments(&color_attachments)
        .depth_stencil_attachment(&depth_attachment_ref);
    let dependency = vk::SubpassDependency::default()
        .src_subpass(vk::SUBPASS_EXTERNAL)
        .dst_subpass(0)
        .src_stage_mask(
            vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT
                | vk::PipelineStageFlags::EARLY_FRAGMENT_TESTS
                | vk::PipelineStageFlags::LATE_FRAGMENT_TESTS,
        )
        .dst_stage_mask(
            vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT
                | vk::PipelineStageFlags::EARLY_FRAGMENT_TESTS
                | vk::PipelineStageFlags::LATE_FRAGMENT_TESTS,
        )
        .src_access_mask(vk::AccessFlags::empty())
        .dst_access_mask(
            vk::AccessFlags::COLOR_ATTACHMENT_WRITE
                | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE,
        );
    let attachments = [color_attachment, depth_attachment];
    let subpasses = [subpass];
    let dependencies = [dependency];
    let create_info = vk::RenderPassCreateInfo::default()
        .attachments(&attachments)
        .subpasses(&subpasses)
        .dependencies(&dependencies);
    // SAFETY: The render-pass create info only references stack-owned descriptors.
    unsafe { device.device().create_render_pass(&create_info, None) }.map_err(|error| {
        VulkanSmokeRendererError::VulkanOperation {
            context: "vkCreateRenderPass",
            result: error,
        }
    })
}

fn create_pipeline_layout(
    device: &VulkanLogicalDeviceProbe,
    descriptor_set_layout: vk::DescriptorSetLayout,
) -> Result<vk::PipelineLayout, VulkanSmokeRendererError> {
    let set_layouts = [descriptor_set_layout];
    // Material data occupies one 128-byte push-constant allocation shared by
    // both stages; the vertex stage also reads its specular pair from the
    // dynamic storage buffer at descriptor binding 5.  Camera data is supplied
    // by the dynamic frame UBO at descriptor binding 4.
    // Expose the complete block from offset zero so drivers validate the
    // fragment member's declared offset against the pipeline layout.
    let push_constant_ranges = [vk::PushConstantRange::default()
        .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)
        .offset(0)
        .size(32 * u32::try_from(std::mem::size_of::<f32>()).unwrap_or(u32::MAX))];
    let create_info = vk::PipelineLayoutCreateInfo::default()
        .set_layouts(&set_layouts)
        .push_constant_ranges(&push_constant_ranges);
    // SAFETY: The descriptor-set layout belongs to this live logical device.
    unsafe { device.device().create_pipeline_layout(&create_info, None) }.map_err(|error| {
        VulkanSmokeRendererError::VulkanOperation {
            context: "vkCreatePipelineLayout",
            result: error,
        }
    })
}

fn create_texture_descriptor_bundle(
    device: &VulkanLogicalDeviceProbe,
    textures: &[VulkanAllocatedImage],
    texture_slot_indices: &[usize],
    frame_uniform_buffer: &VulkanAllocatedBuffer,
    material_specular_buffer: &VulkanAllocatedBuffer,
    point_light_buffer: &VulkanAllocatedBuffer,
    point_light_range: u64,
) -> Result<
    (
        vk::DescriptorSetLayout,
        vk::DescriptorPool,
        Vec<vk::DescriptorSet>,
        vk::Sampler,
    ),
    VulkanSmokeRendererError,
> {
    const TEXTURES_PER_MATERIAL: usize = 4;
    const FRAME_BINDING: u32 = 4;
    const MATERIAL_SPECULAR_BINDING: u32 = 5;
    const POINT_LIGHT_BINDING: u32 = 6;
    let mut bindings = (0..TEXTURES_PER_MATERIAL)
        .map(|binding| {
            vk::DescriptorSetLayoutBinding::default()
                .binding(u32::try_from(binding).unwrap_or(u32::MAX))
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT)
        })
        .collect::<Vec<_>>();
    bindings.push(
        vk::DescriptorSetLayoutBinding::default()
            .binding(FRAME_BINDING)
            .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER_DYNAMIC)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT),
    );
    bindings.push(
        vk::DescriptorSetLayoutBinding::default()
            .binding(MATERIAL_SPECULAR_BINDING)
            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER_DYNAMIC)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::VERTEX),
    );
    bindings.push(
        vk::DescriptorSetLayoutBinding::default()
            .binding(POINT_LIGHT_BINDING)
            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER_DYNAMIC)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::VERTEX),
    );
    let layout_info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    // SAFETY: The layout description is stack-owned and references no external memory.
    let layout = unsafe {
        device
            .device()
            .create_descriptor_set_layout(&layout_info, None)
    }
    .map_err(|result| VulkanSmokeRendererError::VulkanOperation {
        context: "vkCreateDescriptorSetLayout",
        result,
    })?;
    let texture_count = u32::try_from(texture_slot_indices.len()).map_err(|_| {
        VulkanSmokeRendererError::InvariantViolation {
            context: "static material texture count exceeds Vulkan descriptor limit",
        }
    })?;
    if texture_count == 0
        || !texture_slot_indices
            .len()
            .is_multiple_of(TEXTURES_PER_MATERIAL)
        || texture_slot_indices
            .iter()
            .any(|texture_index| *texture_index >= textures.len())
    {
        // SAFETY: The layout was created above on this device and is rolled
        // back before reporting malformed material-slot input.
        unsafe { device.device().destroy_descriptor_set_layout(layout, None) };
        return Err(VulkanSmokeRendererError::InvariantViolation {
            context: "static material texture slots are invalid",
        });
    }
    let material_count = texture_count / u32::try_from(TEXTURES_PER_MATERIAL).unwrap_or(1);
    let pool_size = vk::DescriptorPoolSize::default()
        .ty(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
        .descriptor_count(texture_count);
    let frame_pool_size = vk::DescriptorPoolSize::default()
        .ty(vk::DescriptorType::UNIFORM_BUFFER_DYNAMIC)
        .descriptor_count(material_count);
    let storage_pool_size = vk::DescriptorPoolSize::default()
        .ty(vk::DescriptorType::STORAGE_BUFFER_DYNAMIC)
        .descriptor_count(material_count.checked_mul(2).ok_or_else(|| {
            // SAFETY: The layout was created above on this device and is
            // rolled back before reporting an impossible descriptor count.
            unsafe { device.device().destroy_descriptor_set_layout(layout, None) };
            VulkanSmokeRendererError::InvariantViolation {
                context: "dynamic storage descriptor count overflow",
            }
        })?);
    let pool_sizes = [pool_size, frame_pool_size, storage_pool_size];
    let pool_info = vk::DescriptorPoolCreateInfo::default()
        .pool_sizes(&pool_sizes)
        .max_sets(material_count);
    let pool =
        // SAFETY: The pool description is stack-owned and reserves exactly one descriptor.
        unsafe { device.device().create_descriptor_pool(&pool_info, None) }.map_err(|result| {
            // SAFETY: The layout was created above on this device and is rolled back once.
            unsafe { device.device().destroy_descriptor_set_layout(layout, None) };
            VulkanSmokeRendererError::VulkanOperation {
                context: "vkCreateDescriptorPool",
                result,
            }
        })?;
    let layouts = vec![layout; usize::try_from(material_count).unwrap_or(0)];
    let allocate_info = vk::DescriptorSetAllocateInfo::default()
        .descriptor_pool(pool)
        .set_layouts(&layouts);
    // SAFETY: The pool and layout are live on this logical device.
    let descriptor_sets = unsafe { device.device().allocate_descriptor_sets(&allocate_info) }
        .map_err(|result| {
            // SAFETY: Resources were created above on this device and are rolled back once.
            unsafe {
                device.device().destroy_descriptor_pool(pool, None);
                device.device().destroy_descriptor_set_layout(layout, None);
            };
            VulkanSmokeRendererError::VulkanOperation {
                context: "vkAllocateDescriptorSets",
                result,
            }
        })?;
    let sampler_info = vk::SamplerCreateInfo::default()
        .mag_filter(vk::Filter::LINEAR)
        .min_filter(vk::Filter::LINEAR)
        .mipmap_mode(vk::SamplerMipmapMode::LINEAR)
        .address_mode_u(vk::SamplerAddressMode::REPEAT)
        .address_mode_v(vk::SamplerAddressMode::REPEAT)
        .address_mode_w(vk::SamplerAddressMode::REPEAT)
        // The uploaded images carry a complete CPU-generated mip chain; the
        // sampler clamps to the largest legal level for each image.
        .max_lod(16.0);
    let sampler =
        // SAFETY: The sampler create info is stack-owned and has no unsupported optional features.
        unsafe { device.device().create_sampler(&sampler_info, None) }.map_err(|result| {
            // SAFETY: Resources were created above on this device and are rolled back once.
            unsafe {
                device.device().destroy_descriptor_pool(pool, None);
                device.device().destroy_descriptor_set_layout(layout, None);
            };
            VulkanSmokeRendererError::VulkanOperation {
                context: "vkCreateSampler",
                result,
            }
        })?;
    let image_infos = texture_slot_indices
        .iter()
        .map(|texture_index| {
            let texture = &textures[*texture_index];
            vk::DescriptorImageInfo::default()
                .sampler(sampler)
                .image_view(texture.view)
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
        })
        .collect::<Vec<_>>();
    let frame_info = vk::DescriptorBufferInfo::default()
        .buffer(frame_uniform_buffer.buffer)
        .offset(0)
        .range(VULKAN_FRAME_UNIFORM_BYTES as u64);
    let material_specular_info = vk::DescriptorBufferInfo::default()
        .buffer(material_specular_buffer.buffer)
        .offset(0)
        .range(VULKAN_MATERIAL_SPECULAR_RECORD_BYTES as u64);
    let point_light_info = vk::DescriptorBufferInfo::default()
        .buffer(point_light_buffer.buffer)
        .offset(0)
        .range(point_light_range);
    let mut writes = Vec::with_capacity(descriptor_sets.len() * (TEXTURES_PER_MATERIAL + 3));
    for (set_index, descriptor_set) in descriptor_sets.iter().copied().enumerate() {
        for binding in 0..TEXTURES_PER_MATERIAL {
            let image_info = &image_infos[set_index * TEXTURES_PER_MATERIAL + binding];
            writes.push(
                vk::WriteDescriptorSet::default()
                    .dst_set(descriptor_set)
                    .dst_binding(u32::try_from(binding).unwrap_or(u32::MAX))
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .image_info(std::slice::from_ref(image_info)),
            );
        }
        writes.push(
            vk::WriteDescriptorSet::default()
                .dst_set(descriptor_set)
                .dst_binding(FRAME_BINDING)
                .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER_DYNAMIC)
                .buffer_info(std::slice::from_ref(&frame_info)),
        );
        writes.push(
            vk::WriteDescriptorSet::default()
                .dst_set(descriptor_set)
                .dst_binding(MATERIAL_SPECULAR_BINDING)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER_DYNAMIC)
                .buffer_info(std::slice::from_ref(&material_specular_info)),
        );
        writes.push(
            vk::WriteDescriptorSet::default()
                .dst_set(descriptor_set)
                .dst_binding(POINT_LIGHT_BINDING)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER_DYNAMIC)
                .buffer_info(std::slice::from_ref(&point_light_info)),
        );
    }
    // SAFETY: Descriptor sets, sampler and image views are live; every texture upload completed its shader-read transition.
    unsafe { device.device().update_descriptor_sets(&writes, &[]) };
    Ok((layout, pool, descriptor_sets, sampler))
}

fn extent_component_to_f32(value: u32) -> f32 {
    value as f32
}

fn create_graphics_pipeline_cache(
    device: &VulkanLogicalDeviceProbe,
    render_pass: vk::RenderPass,
    pipeline_layout: vk::PipelineLayout,
    extent: (u32, u32),
    draw_ranges: &[super::VulkanStaticDrawRange],
) -> Result<BTreeMap<PipelineKey, vk::Pipeline>, VulkanSmokeRendererError> {
    let mut pipelines = BTreeMap::new();
    for range in draw_ranges {
        let key = range.pipeline_key();
        if pipelines.contains_key(&key) {
            continue;
        }
        let pipeline = match create_graphics_pipeline(
            device,
            render_pass,
            pipeline_layout,
            extent,
            range.pipeline_state,
        ) {
            Ok(pipeline) => pipeline,
            Err(error) => {
                // SAFETY: All previously created pipelines belong to this live device and are rolled back with their bundle.
                unsafe {
                    for pipeline in pipelines.into_values() {
                        device.device().destroy_pipeline(pipeline, None);
                    }
                }
                return Err(error);
            }
        };
        pipelines.insert(key, pipeline);
    }
    Ok(pipelines)
}

fn create_graphics_pipeline(
    device: &VulkanLogicalDeviceProbe,
    render_pass: vk::RenderPass,
    pipeline_layout: vk::PipelineLayout,
    extent: (u32, u32),
    state: LegacyPipelineState,
) -> Result<vk::Pipeline, VulkanSmokeRendererError> {
    let vertex_shader = create_shader_module(device, TRIANGLE_VERTEX_SHADER_WORDS, "vertex")?;
    let fragment_shader =
        match create_shader_module(device, TRIANGLE_FRAGMENT_SHADER_WORDS, "fragment") {
            Ok(module) => module,
            Err(error) => {
                // SAFETY: The shader module was created above on this live logical device and is destroyed on setup failure.
                unsafe { device.device().destroy_shader_module(vertex_shader, None) };
                return Err(error);
            }
        };
    let entry_point = c"main";
    let shader_stages = [
        vk::PipelineShaderStageCreateInfo::default()
            .module(vertex_shader)
            .name(entry_point)
            .stage(vk::ShaderStageFlags::VERTEX),
        vk::PipelineShaderStageCreateInfo::default()
            .module(fragment_shader)
            .name(entry_point)
            .stage(vk::ShaderStageFlags::FRAGMENT),
    ];
    let vertex_binding = vk::VertexInputBindingDescription::default()
        .binding(0)
        .stride(u32::try_from(14 * std::mem::size_of::<f32>()).unwrap_or(u32::MAX))
        .input_rate(vk::VertexInputRate::VERTEX);
    let vertex_attributes = [
        vk::VertexInputAttributeDescription::default()
            .binding(0)
            .location(0)
            .format(vk::Format::R32G32B32_SFLOAT)
            .offset(0),
        vk::VertexInputAttributeDescription::default()
            .binding(0)
            .location(1)
            .format(vk::Format::R32G32B32_SFLOAT)
            .offset(u32::try_from(3 * std::mem::size_of::<f32>()).unwrap_or(u32::MAX)),
        vk::VertexInputAttributeDescription::default()
            .binding(0)
            .location(2)
            .format(vk::Format::R32G32B32_SFLOAT)
            .offset(u32::try_from(6 * std::mem::size_of::<f32>()).unwrap_or(u32::MAX)),
        vk::VertexInputAttributeDescription::default()
            .binding(0)
            .location(3)
            .format(vk::Format::R32G32_SFLOAT)
            .offset(u32::try_from(9 * std::mem::size_of::<f32>()).unwrap_or(u32::MAX)),
        vk::VertexInputAttributeDescription::default()
            .binding(0)
            .location(4)
            .format(vk::Format::R32G32_SFLOAT)
            .offset(u32::try_from(11 * std::mem::size_of::<f32>()).unwrap_or(u32::MAX)),
        vk::VertexInputAttributeDescription::default()
            .binding(0)
            .location(5)
            .format(vk::Format::R32_SFLOAT)
            .offset(u32::try_from(13 * std::mem::size_of::<f32>()).unwrap_or(u32::MAX)),
    ];
    let vertex_bindings = [vertex_binding];
    let vertex_input_state = vk::PipelineVertexInputStateCreateInfo::default()
        .vertex_binding_descriptions(&vertex_bindings)
        .vertex_attribute_descriptions(&vertex_attributes);
    let input_assembly_state = vk::PipelineInputAssemblyStateCreateInfo::default()
        .topology(vk::PrimitiveTopology::TRIANGLE_LIST)
        .primitive_restart_enable(false);
    let viewports = [vk::Viewport {
        x: 0.0,
        y: 0.0,
        width: extent_component_to_f32(extent.0),
        height: extent_component_to_f32(extent.1),
        min_depth: 0.0,
        max_depth: 1.0,
    }];
    let scissors = [vk::Rect2D {
        offset: vk::Offset2D { x: 0, y: 0 },
        extent: vk::Extent2D {
            width: extent.0,
            height: extent.1,
        },
    }];
    let viewport_state = vk::PipelineViewportStateCreateInfo::default()
        .viewports(&viewports)
        .scissors(&scissors);
    let rasterization_state = vk::PipelineRasterizationStateCreateInfo::default()
        .depth_clamp_enable(false)
        .rasterizer_discard_enable(false)
        .polygon_mode(vk::PolygonMode::FILL)
        .line_width(1.0)
        .cull_mode(match state.cull {
            LegacyCullMode::Disabled => vk::CullModeFlags::NONE,
            LegacyCullMode::BackFace => vk::CullModeFlags::BACK,
            LegacyCullMode::FrontFace => vk::CullModeFlags::FRONT,
        })
        .front_face(vk::FrontFace::CLOCKWISE)
        .depth_bias_enable(false);
    let multisample_state = vk::PipelineMultisampleStateCreateInfo::default()
        .sample_shading_enable(false)
        .rasterization_samples(vk::SampleCountFlags::TYPE_1);
    let depth_stencil_state = vk::PipelineDepthStencilStateCreateInfo::default()
        .depth_test_enable(state.depth != LegacyDepthMode::Disabled)
        .depth_write_enable(state.depth == LegacyDepthMode::TestWrite)
        .depth_compare_op(vk::CompareOp::LESS_OR_EQUAL)
        .depth_bounds_test_enable(false)
        .stencil_test_enable(false);
    let (blend_enable, src_color_blend_factor, dst_color_blend_factor) = match state.blend {
        LegacyBlendMode::Opaque => (false, vk::BlendFactor::ONE, vk::BlendFactor::ZERO),
        LegacyBlendMode::SourceAlpha => (
            true,
            vk::BlendFactor::SRC_ALPHA,
            vk::BlendFactor::ONE_MINUS_SRC_ALPHA,
        ),
        LegacyBlendMode::Additive => (true, vk::BlendFactor::SRC_ALPHA, vk::BlendFactor::ONE),
        LegacyBlendMode::ZeroSourceColor => {
            (true, vk::BlendFactor::ZERO, vk::BlendFactor::SRC_COLOR)
        }
        LegacyBlendMode::DestColorSourceColor => {
            (true, vk::BlendFactor::DST_COLOR, vk::BlendFactor::SRC_COLOR)
        }
    };
    let color_blend_attachment = vk::PipelineColorBlendAttachmentState::default()
        .color_write_mask(
            vk::ColorComponentFlags::R
                | vk::ColorComponentFlags::G
                | vk::ColorComponentFlags::B
                | vk::ColorComponentFlags::A,
        )
        .blend_enable(blend_enable)
        .src_color_blend_factor(src_color_blend_factor)
        .dst_color_blend_factor(dst_color_blend_factor)
        .color_blend_op(vk::BlendOp::ADD)
        .src_alpha_blend_factor(src_color_blend_factor)
        .dst_alpha_blend_factor(dst_color_blend_factor)
        .alpha_blend_op(vk::BlendOp::ADD);
    let color_blend_attachments = [color_blend_attachment];
    let color_blend_state = vk::PipelineColorBlendStateCreateInfo::default()
        .logic_op_enable(false)
        .attachments(&color_blend_attachments);
    let create_info = vk::GraphicsPipelineCreateInfo::default()
        .stages(&shader_stages)
        .vertex_input_state(&vertex_input_state)
        .input_assembly_state(&input_assembly_state)
        .viewport_state(&viewport_state)
        .rasterization_state(&rasterization_state)
        .multisample_state(&multisample_state)
        .depth_stencil_state(&depth_stencil_state)
        .color_blend_state(&color_blend_state)
        .layout(pipeline_layout)
        .render_pass(render_pass)
        .subpass(0);
    let create_infos = [create_info];
    // SAFETY: Pipeline creation references only stack-owned descriptions and live render-pass/layout handles.
    let pipeline_result = unsafe {
        device
            .device()
            .create_graphics_pipelines(vk::PipelineCache::null(), &create_infos, None)
    };
    // SAFETY: The shader modules were created above on this live logical device and are no longer needed after pipeline creation.
    unsafe {
        device.device().destroy_shader_module(vertex_shader, None);
        device.device().destroy_shader_module(fragment_shader, None);
    }
    let pipelines =
        pipeline_result.map_err(|(_, error)| VulkanSmokeRendererError::VulkanOperation {
            context: "vkCreateGraphicsPipelines",
            result: error,
        })?;
    Ok(pipelines[0])
}

fn create_shader_module(
    device: &VulkanLogicalDeviceProbe,
    words: &[u32],
    stage: &'static str,
) -> Result<vk::ShaderModule, VulkanSmokeRendererError> {
    let create_info = vk::ShaderModuleCreateInfo::default().code(words);
    // SAFETY: The SPIR-V slice points to static checked-in words and lives for the duration of the call.
    unsafe { device.device().create_shader_module(&create_info, None) }.map_err(|error| {
        VulkanSmokeRendererError::VulkanOperation {
            context: if stage == "vertex" {
                "vkCreateShaderModule(vertex)"
            } else {
                "vkCreateShaderModule(fragment)"
            },
            result: error,
        }
    })
}

fn create_framebuffer(
    device: &VulkanLogicalDeviceProbe,
    render_pass: vk::RenderPass,
    image_view: vk::ImageView,
    depth_view: vk::ImageView,
    extent: (u32, u32),
) -> Result<vk::Framebuffer, VulkanSmokeRendererError> {
    let attachments = [image_view, depth_view];
    let create_info = vk::FramebufferCreateInfo::default()
        .render_pass(render_pass)
        .attachments(&attachments)
        .width(extent.0)
        .height(extent.1)
        .layers(1);
    // SAFETY: The framebuffer references a live image view and render pass owned by this logical device.
    unsafe { device.device().create_framebuffer(&create_info, None) }.map_err(|error| {
        VulkanSmokeRendererError::VulkanOperation {
            context: "vkCreateFramebuffer",
            result: error,
        }
    })
}

fn allocate_command_buffers(
    device: &VulkanLogicalDeviceProbe,
    command_pool: vk::CommandPool,
    count: u32,
) -> Result<Vec<vk::CommandBuffer>, VulkanSmokeRendererError> {
    let allocate_info = vk::CommandBufferAllocateInfo::default()
        .command_pool(command_pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(count);
    // SAFETY: The command pool belongs to this live logical device and the allocation info is stack-owned.
    unsafe { device.device().allocate_command_buffers(&allocate_info) }.map_err(|error| {
        VulkanSmokeRendererError::VulkanOperation {
            context: "vkAllocateCommandBuffers",
            result: error,
        }
    })
}

pub(super) fn destroy_swapchain_resources(
    device: &VulkanLogicalDeviceProbe,
    command_pool: vk::CommandPool,
    resources: VulkanSwapchainResources,
) {
    // SAFETY: All handles belong to this live logical device and are destroyed during renderer teardown/recreation.
    unsafe {
        if !resources.command_buffers.is_empty() {
            device
                .device()
                .free_command_buffers(command_pool, &resources.command_buffers);
        }
        for framebuffer in resources.framebuffers {
            device.device().destroy_framebuffer(framebuffer, None);
        }
        for pipeline in resources.pipelines.into_values() {
            device.device().destroy_pipeline(pipeline, None);
        }
        device
            .device()
            .destroy_pipeline_layout(resources.pipeline_layout, None);
        device.device().destroy_sampler(resources.sampler, None);
        device
            .device()
            .destroy_descriptor_pool(resources.descriptor_pool, None);
        device
            .device()
            .destroy_descriptor_set_layout(resources.descriptor_set_layout, None);
        device
            .device()
            .destroy_render_pass(resources.render_pass, None);
        for depth_attachment in resources.depth_attachments {
            destroy_depth_attachment(device, &depth_attachment);
        }
        for image_view in resources.image_views {
            device.device().destroy_image_view(image_view, None);
        }
        for buffer in resources.readback_buffers {
            device.device().destroy_buffer(buffer.buffer, None);
            device.device().free_memory(buffer.memory, None);
        }
        for semaphore in resources.render_finished {
            device.device().destroy_semaphore(semaphore, None);
        }
        device
            .device()
            .destroy_buffer(resources.frame_uniform_buffer.buffer, None);
        device
            .device()
            .free_memory(resources.frame_uniform_buffer.memory, None);
        device
            .device()
            .destroy_buffer(resources.material_specular_buffer.buffer, None);
        device
            .device()
            .free_memory(resources.material_specular_buffer.memory, None);
        device
            .device()
            .destroy_buffer(resources.point_light_buffer.buffer, None);
        device
            .device()
            .free_memory(resources.point_light_buffer.memory, None);
    }
}

fn destroy_partial_swapchain_resources(
    device: &VulkanLogicalDeviceProbe,
    command_pool: vk::CommandPool,
    partial: PartialSwapchainResources,
) {
    // SAFETY: All handles in the partial bundle belong to this live logical device and are destroyed on setup failure.
    unsafe {
        if !partial.command_buffers.is_empty() {
            device
                .device()
                .free_command_buffers(command_pool, &partial.command_buffers);
        }
        for framebuffer in partial.framebuffers {
            device.device().destroy_framebuffer(framebuffer, None);
        }
        for pipeline in partial.pipelines.into_values() {
            device.device().destroy_pipeline(pipeline, None);
        }
        if let Some(pipeline_layout) = partial.pipeline_layout {
            device
                .device()
                .destroy_pipeline_layout(pipeline_layout, None);
        }
        if let Some(sampler) = partial.sampler {
            device.device().destroy_sampler(sampler, None);
        }
        if let Some(descriptor_pool) = partial.descriptor_pool {
            device
                .device()
                .destroy_descriptor_pool(descriptor_pool, None);
        }
        if let Some(descriptor_set_layout) = partial.descriptor_set_layout {
            device
                .device()
                .destroy_descriptor_set_layout(descriptor_set_layout, None);
        }
        if let Some(render_pass) = partial.render_pass {
            device.device().destroy_render_pass(render_pass, None);
        }
        for depth_attachment in partial.depth_attachments {
            destroy_depth_attachment(device, &depth_attachment);
        }
        for image_view in partial.image_views {
            device.device().destroy_image_view(image_view, None);
        }
        for buffer in partial.readback_buffers {
            device.device().destroy_buffer(buffer.buffer, None);
            device.device().free_memory(buffer.memory, None);
        }
        for semaphore in partial.render_finished {
            device.device().destroy_semaphore(semaphore, None);
        }
        if let Some(buffer) = partial.frame_uniform_buffer {
            device.device().destroy_buffer(buffer.buffer, None);
            device.device().free_memory(buffer.memory, None);
        }
        if let Some(buffer) = partial.material_specular_buffer {
            device.device().destroy_buffer(buffer.buffer, None);
            device.device().free_memory(buffer.memory, None);
        }
        if let Some(buffer) = partial.point_light_buffer {
            device.device().destroy_buffer(buffer.buffer, None);
            device.device().free_memory(buffer.memory, None);
        }
    }
}
