#![allow(unsafe_code)]

use ash::vk;
use fparkan_platform::DepthStencilSupport;
use fparkan_render::{
    LegacyBlendMode, LegacyCullMode, LegacyDepthMode, LegacyPipelineState, PipelineKey,
};
use std::collections::BTreeMap;

use super::{
    color_subresource_range, create_depth_attachment, destroy_depth_attachment,
    VulkanAllocatedBuffer, VulkanAllocatedImage, VulkanDepthAttachment, VulkanInstanceProbe,
    VulkanLogicalDeviceProbe, VulkanSmokeRendererError, VulkanSwapchainProbe,
    TRIANGLE_FRAGMENT_SHADER_WORDS, TRIANGLE_VERTEX_SHADER_WORDS,
};

pub(super) struct VulkanSwapchainResources {
    pub(super) image_views: Vec<vk::ImageView>,
    pub(super) depth_attachment: VulkanDepthAttachment,
    pub(super) render_pass: vk::RenderPass,
    pub(super) pipeline_layout: vk::PipelineLayout,
    pub(super) descriptor_set_layout: vk::DescriptorSetLayout,
    pub(super) descriptor_pool: vk::DescriptorPool,
    pub(super) descriptor_sets: Vec<vk::DescriptorSet>,
    pub(super) sampler: vk::Sampler,
    /// Live graphics pipelines indexed by canonical backend-neutral state.
    pub(super) pipelines: BTreeMap<PipelineKey, vk::Pipeline>,
    pub(super) framebuffers: Vec<vk::Framebuffer>,
    pub(super) command_buffers: Vec<vk::CommandBuffer>,
}

struct PartialSwapchainResources {
    image_views: Vec<vk::ImageView>,
    depth_attachment: Option<VulkanDepthAttachment>,
    render_pass: Option<vk::RenderPass>,
    pipeline_layout: Option<vk::PipelineLayout>,
    descriptor_set_layout: Option<vk::DescriptorSetLayout>,
    descriptor_pool: Option<vk::DescriptorPool>,
    sampler: Option<vk::Sampler>,
    pipelines: BTreeMap<PipelineKey, vk::Pipeline>,
    framebuffers: Vec<vk::Framebuffer>,
    command_buffers: Vec<vk::CommandBuffer>,
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(super) fn create_swapchain_resources(
    instance: &VulkanInstanceProbe,
    device: &VulkanLogicalDeviceProbe,
    swapchain: &VulkanSwapchainProbe,
    command_pool: vk::CommandPool,
    vertex_buffer: &VulkanAllocatedBuffer,
    index_buffer: &VulkanAllocatedBuffer,
    textures: &[VulkanAllocatedImage],
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
        depth_attachment: None,
        render_pass: None,
        pipeline_layout: None,
        descriptor_set_layout: None,
        descriptor_pool: None,
        sampler: None,
        pipelines: BTreeMap::new(),
        framebuffers: Vec::new(),
        command_buffers: Vec::new(),
    };
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
        draw_ranges,
        depth_request,
    ) {
        Ok(bundle) => bundle,
        Err(error) => {
            destroy_partial_swapchain_resources(device, command_pool, partial);
            return Err(error);
        }
    };
    let depth_view = depth_attachment.image.view;
    partial.depth_attachment = Some(depth_attachment);
    partial.render_pass = Some(render_pass);
    partial.pipeline_layout = Some(pipeline_layout);
    partial.descriptor_set_layout = Some(descriptor_set_layout);
    partial.descriptor_pool = Some(descriptor_pool);
    partial.sampler = Some(sampler);
    partial.pipelines = pipelines;
    let framebuffers = match create_swapchain_framebuffers(
        device,
        render_pass,
        &partial.image_views,
        depth_view,
        swapchain.report.plan.extent,
    ) {
        Ok(framebuffers) => framebuffers,
        Err(error) => {
            destroy_partial_swapchain_resources(device, command_pool, partial);
            return Err(error);
        }
    };
    partial.framebuffers = framebuffers;
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
    let depth_attachment =
        partial
            .depth_attachment
            .take()
            .ok_or(VulkanSmokeRendererError::InvariantViolation {
                context: "depth attachment ownership after swapchain setup",
            })?;
    Ok(VulkanSwapchainResources {
        image_views: partial.image_views,
        depth_attachment,
        render_pass,
        pipeline_layout,
        descriptor_set_layout,
        descriptor_pool,
        descriptor_sets,
        sampler,
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

#[allow(clippy::type_complexity)]
fn create_swapchain_pipeline_bundle(
    instance: &VulkanInstanceProbe,
    device: &VulkanLogicalDeviceProbe,
    format: i32,
    extent: (u32, u32),
    textures: &[VulkanAllocatedImage],
    draw_ranges: &[super::VulkanStaticDrawRange],
    depth_request: DepthStencilSupport,
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
        create_texture_descriptor_bundle(device, textures).inspect_err(|_| {
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
    depth_view: vk::ImageView,
    extent: (u32, u32),
) -> Result<Vec<vk::Framebuffer>, VulkanSmokeRendererError> {
    let mut framebuffers = Vec::with_capacity(image_views.len());
    for image_view in image_views.iter().copied() {
        match create_framebuffer(device, render_pass, image_view, depth_view, extent) {
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
        .subresource_range(color_subresource_range());
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
        .src_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
        .dst_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
        .src_access_mask(vk::AccessFlags::empty())
        .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE);
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
    let create_info = vk::PipelineLayoutCreateInfo::default().set_layouts(&set_layouts);
    // SAFETY: The descriptor-set layout belongs to this live logical device.
    unsafe { device.device().create_pipeline_layout(&create_info, None) }.map_err(|error| {
        VulkanSmokeRendererError::VulkanOperation {
            context: "vkCreatePipelineLayout",
            result: error,
        }
    })
}

#[allow(clippy::too_many_lines)]
fn create_texture_descriptor_bundle(
    device: &VulkanLogicalDeviceProbe,
    textures: &[VulkanAllocatedImage],
) -> Result<
    (
        vk::DescriptorSetLayout,
        vk::DescriptorPool,
        Vec<vk::DescriptorSet>,
        vk::Sampler,
    ),
    VulkanSmokeRendererError,
> {
    let binding = vk::DescriptorSetLayoutBinding::default()
        .binding(0)
        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
        .descriptor_count(1)
        .stage_flags(vk::ShaderStageFlags::FRAGMENT);
    let bindings = [binding];
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
    let texture_count = u32::try_from(textures.len()).map_err(|_| {
        VulkanSmokeRendererError::InvariantViolation {
            context: "static material texture count exceeds Vulkan descriptor limit",
        }
    })?;
    if texture_count == 0 {
        return Err(VulkanSmokeRendererError::InvariantViolation {
            context: "static material texture list is empty",
        });
    }
    let pool_size = vk::DescriptorPoolSize::default()
        .ty(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
        .descriptor_count(texture_count);
    let pool_sizes = [pool_size];
    let pool_info = vk::DescriptorPoolCreateInfo::default()
        .pool_sizes(&pool_sizes)
        .max_sets(texture_count);
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
    let layouts = vec![layout; textures.len()];
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
        .max_lod(0.0);
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
    let image_infos = textures
        .iter()
        .map(|texture| {
            vk::DescriptorImageInfo::default()
                .sampler(sampler)
                .image_view(texture.view)
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
        })
        .collect::<Vec<_>>();
    let writes = descriptor_sets
        .iter()
        .copied()
        .zip(image_infos.iter())
        .map(|(descriptor_set, image_info)| {
            vk::WriteDescriptorSet::default()
                .dst_set(descriptor_set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(std::slice::from_ref(image_info))
        })
        .collect::<Vec<_>>();
    // SAFETY: Descriptor sets, sampler and image views are live; every texture upload completed its shader-read transition.
    unsafe { device.device().update_descriptor_sets(&writes, &[]) };
    Ok((layout, pool, descriptor_sets, sampler))
}

#[allow(clippy::cast_precision_loss)]
fn extent_component_to_f32(value: u32) -> f32 {
    value as f32
}

#[allow(clippy::too_many_lines)]
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

#[allow(clippy::too_many_lines)]
fn create_graphics_pipeline(
    device: &VulkanLogicalDeviceProbe,
    render_pass: vk::RenderPass,
    pipeline_layout: vk::PipelineLayout,
    extent: (u32, u32),
    state: LegacyPipelineState,
) -> Result<vk::Pipeline, VulkanSmokeRendererError> {
    if state.alpha_test {
        return Err(VulkanSmokeRendererError::InvalidStaticMesh {
            context:
                "static renderer has no alpha-test shader variant for requested pipeline state",
        });
    }
    let vertex_shader = create_shader_module(device, TRIANGLE_VERTEX_SHADER_WORDS)?;
    let fragment_shader = match create_shader_module(device, TRIANGLE_FRAGMENT_SHADER_WORDS) {
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
        .stride(u32::try_from(7 * std::mem::size_of::<f32>()).unwrap_or(u32::MAX))
        .input_rate(vk::VertexInputRate::VERTEX);
    let vertex_attributes = [
        vk::VertexInputAttributeDescription::default()
            .binding(0)
            .location(0)
            .format(vk::Format::R32G32_SFLOAT)
            .offset(0),
        vk::VertexInputAttributeDescription::default()
            .binding(0)
            .location(1)
            .format(vk::Format::R32G32B32_SFLOAT)
            .offset(u32::try_from(2 * std::mem::size_of::<f32>()).unwrap_or(u32::MAX)),
        vk::VertexInputAttributeDescription::default()
            .binding(0)
            .location(2)
            .format(vk::Format::R32G32_SFLOAT)
            .offset(u32::try_from(5 * std::mem::size_of::<f32>()).unwrap_or(u32::MAX)),
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
    let color_blend_attachment = vk::PipelineColorBlendAttachmentState::default()
        .color_write_mask(
            vk::ColorComponentFlags::R
                | vk::ColorComponentFlags::G
                | vk::ColorComponentFlags::B
                | vk::ColorComponentFlags::A,
        )
        .blend_enable(state.blend == LegacyBlendMode::SourceAlpha)
        .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
        .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
        .color_blend_op(vk::BlendOp::ADD)
        .src_alpha_blend_factor(vk::BlendFactor::ONE)
        .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
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
) -> Result<vk::ShaderModule, VulkanSmokeRendererError> {
    let create_info = vk::ShaderModuleCreateInfo::default().code(words);
    // SAFETY: The SPIR-V slice points to static checked-in words and lives for the duration of the call.
    unsafe { device.device().create_shader_module(&create_info, None) }.map_err(|error| {
        VulkanSmokeRendererError::VulkanOperation {
            context: "vkCreateShaderModule",
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
        destroy_depth_attachment(device, &resources.depth_attachment);
        for image_view in resources.image_views {
            device.device().destroy_image_view(image_view, None);
        }
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
        if let Some(depth_attachment) = partial.depth_attachment {
            destroy_depth_attachment(device, &depth_attachment);
        }
        for image_view in partial.image_views {
            device.device().destroy_image_view(image_view, None);
        }
    }
}
