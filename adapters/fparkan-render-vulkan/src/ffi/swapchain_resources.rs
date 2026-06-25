#![allow(unsafe_code)]

use ash::vk;

use super::{
    color_subresource_range, VulkanAllocatedBuffer, VulkanLogicalDeviceProbe,
    VulkanSmokeRendererError, VulkanSwapchainProbe, TRIANGLE_FRAGMENT_SHADER_WORDS,
    TRIANGLE_VERTEX_SHADER_WORDS,
};

pub(super) struct VulkanSwapchainResources {
    pub(super) image_views: Vec<vk::ImageView>,
    pub(super) render_pass: vk::RenderPass,
    pub(super) pipeline_layout: vk::PipelineLayout,
    pub(super) pipeline: vk::Pipeline,
    pub(super) framebuffers: Vec<vk::Framebuffer>,
    pub(super) command_buffers: Vec<vk::CommandBuffer>,
}

struct PartialSwapchainResources {
    image_views: Vec<vk::ImageView>,
    render_pass: Option<vk::RenderPass>,
    pipeline_layout: Option<vk::PipelineLayout>,
    pipeline: Option<vk::Pipeline>,
    framebuffers: Vec<vk::Framebuffer>,
    command_buffers: Vec<vk::CommandBuffer>,
}

pub(super) fn create_swapchain_resources(
    device: &VulkanLogicalDeviceProbe,
    swapchain: &VulkanSwapchainProbe,
    command_pool: vk::CommandPool,
    vertex_buffer: &VulkanAllocatedBuffer,
    index_buffer: &VulkanAllocatedBuffer,
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
        render_pass: None,
        pipeline_layout: None,
        pipeline: None,
        framebuffers: Vec::new(),
        command_buffers: Vec::new(),
    };
    let (render_pass, pipeline_layout, pipeline) = match create_swapchain_pipeline_bundle(
        device,
        swapchain.report.plan.format.format,
        swapchain.report.plan.extent,
    ) {
        Ok(bundle) => bundle,
        Err(error) => {
            destroy_partial_swapchain_resources(device, command_pool, partial);
            return Err(error);
        }
    };
    partial.render_pass = Some(render_pass);
    partial.pipeline_layout = Some(pipeline_layout);
    partial.pipeline = Some(pipeline);
    let framebuffers = match create_swapchain_framebuffers(
        device,
        render_pass,
        &partial.image_views,
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
    Ok(VulkanSwapchainResources {
        image_views: partial.image_views,
        render_pass,
        pipeline_layout,
        pipeline,
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
    device: &VulkanLogicalDeviceProbe,
    format: i32,
    extent: (u32, u32),
) -> Result<(vk::RenderPass, vk::PipelineLayout, vk::Pipeline), VulkanSmokeRendererError> {
    let render_pass = create_render_pass(device, format)?;
    let pipeline_layout = create_pipeline_layout(device).inspect_err(|_| {
        // SAFETY: The render pass was created above on this live logical device and is destroyed on setup failure.
        unsafe { device.device().destroy_render_pass(render_pass, None) };
    })?;
    let pipeline = create_graphics_pipeline(device, render_pass, pipeline_layout, extent)
        .inspect_err(|_| {
            // SAFETY: These objects were created above on this live logical device and are destroyed on setup failure.
            unsafe {
                device
                    .device()
                    .destroy_pipeline_layout(pipeline_layout, None);
                device.device().destroy_render_pass(render_pass, None);
            }
        })?;
    Ok((render_pass, pipeline_layout, pipeline))
}

fn create_swapchain_framebuffers(
    device: &VulkanLogicalDeviceProbe,
    render_pass: vk::RenderPass,
    image_views: &[vk::ImageView],
    extent: (u32, u32),
) -> Result<Vec<vk::Framebuffer>, VulkanSmokeRendererError> {
    let mut framebuffers = Vec::with_capacity(image_views.len());
    for image_view in image_views.iter().copied() {
        match create_framebuffer(device, render_pass, image_view, extent) {
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
    let color_attachments = [color_attachment_ref];
    let subpass = vk::SubpassDescription::default()
        .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
        .color_attachments(&color_attachments);
    let dependency = vk::SubpassDependency::default()
        .src_subpass(vk::SUBPASS_EXTERNAL)
        .dst_subpass(0)
        .src_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
        .dst_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
        .src_access_mask(vk::AccessFlags::empty())
        .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE);
    let attachments = [color_attachment];
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
) -> Result<vk::PipelineLayout, VulkanSmokeRendererError> {
    let create_info = vk::PipelineLayoutCreateInfo::default();
    // SAFETY: The pipeline layout has no descriptor sets or push constants in Stage 0 smoke.
    unsafe { device.device().create_pipeline_layout(&create_info, None) }.map_err(|error| {
        VulkanSmokeRendererError::VulkanOperation {
            context: "vkCreatePipelineLayout",
            result: error,
        }
    })
}

fn extent_component_to_f32(value: u32) -> f32 {
    value as f32
}

fn create_graphics_pipeline(
    device: &VulkanLogicalDeviceProbe,
    render_pass: vk::RenderPass,
    pipeline_layout: vk::PipelineLayout,
    extent: (u32, u32),
) -> Result<vk::Pipeline, VulkanSmokeRendererError> {
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
        .stride(u32::try_from(5 * std::mem::size_of::<f32>()).unwrap_or(u32::MAX))
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
        .cull_mode(vk::CullModeFlags::BACK)
        .front_face(vk::FrontFace::CLOCKWISE)
        .depth_bias_enable(false);
    let multisample_state = vk::PipelineMultisampleStateCreateInfo::default()
        .sample_shading_enable(false)
        .rasterization_samples(vk::SampleCountFlags::TYPE_1);
    let color_blend_attachment = vk::PipelineColorBlendAttachmentState::default()
        .color_write_mask(
            vk::ColorComponentFlags::R
                | vk::ColorComponentFlags::G
                | vk::ColorComponentFlags::B
                | vk::ColorComponentFlags::A,
        )
        .blend_enable(false);
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
    extent: (u32, u32),
) -> Result<vk::Framebuffer, VulkanSmokeRendererError> {
    let attachments = [image_view];
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
        device.device().destroy_pipeline(resources.pipeline, None);
        device
            .device()
            .destroy_pipeline_layout(resources.pipeline_layout, None);
        device
            .device()
            .destroy_render_pass(resources.render_pass, None);
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
        if let Some(pipeline) = partial.pipeline {
            device.device().destroy_pipeline(pipeline, None);
        }
        if let Some(pipeline_layout) = partial.pipeline_layout {
            device
                .device()
                .destroy_pipeline_layout(pipeline_layout, None);
        }
        if let Some(render_pass) = partial.render_pass {
            device.device().destroy_render_pass(render_pass, None);
        }
        for image_view in partial.image_views {
            device.device().destroy_image_view(image_view, None);
        }
    }
}
