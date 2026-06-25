#![allow(unsafe_code)]

use ash::vk;

use super::{
    VulkanInstanceProbe, VulkanLogicalDeviceProbe, VulkanSmokeRendererError, VulkanSwapchainProbe,
    TRIANGLE_FRAGMENT_SHADER_WORDS, TRIANGLE_VERTEX_SHADER_WORDS,
};

pub(super) struct VulkanAllocatedBuffer {
    pub(super) buffer: vk::Buffer,
    pub(super) memory: vk::DeviceMemory,
}

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

pub(super) struct VulkanFrameSync {
    pub(super) image_available: vk::Semaphore,
    pub(super) render_finished: vk::Semaphore,
    pub(super) fence: vk::Fence,
}

pub(super) fn create_command_pool(
    device: &VulkanLogicalDeviceProbe,
) -> Result<vk::CommandPool, VulkanSmokeRendererError> {
    let create_info = vk::CommandPoolCreateInfo::default()
        .queue_family_index(device.report.graphics_queue_family)
        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
    // SAFETY: The queue-family index belongs to this live logical device.
    unsafe { device.device().create_command_pool(&create_info, None) }.map_err(|error| {
        VulkanSmokeRendererError::VulkanOperation {
            context: "vkCreateCommandPool",
            result: error,
        }
    })
}

pub(super) fn create_triangle_vertex_buffer(
    instance: &VulkanInstanceProbe,
    device: &VulkanLogicalDeviceProbe,
) -> Result<VulkanAllocatedBuffer, VulkanSmokeRendererError> {
    let vertices: [[f32; 5]; 3] = [
        [0.0, -0.55, 1.0, 0.2, 0.2],
        [0.55, 0.55, 0.2, 1.0, 0.2],
        [-0.55, 0.55, 0.2, 0.4, 1.0],
    ];
    let mut bytes = Vec::with_capacity(vertices.len() * 5 * std::mem::size_of::<f32>());
    for vertex in vertices {
        for value in vertex {
            bytes.extend_from_slice(&value.to_ne_bytes());
        }
    }
    create_host_visible_buffer(
        instance,
        device,
        &bytes,
        vk::BufferUsageFlags::VERTEX_BUFFER,
        "triangle vertex buffer",
    )
}

pub(super) fn create_triangle_index_buffer(
    instance: &VulkanInstanceProbe,
    device: &VulkanLogicalDeviceProbe,
) -> Result<VulkanAllocatedBuffer, VulkanSmokeRendererError> {
    let indices = [0_u16, 1_u16, 2_u16];
    let mut bytes = Vec::with_capacity(indices.len() * std::mem::size_of::<u16>());
    for index in indices {
        bytes.extend_from_slice(&index.to_ne_bytes());
    }
    create_host_visible_buffer(
        instance,
        device,
        &bytes,
        vk::BufferUsageFlags::INDEX_BUFFER,
        "triangle index buffer",
    )
}

fn create_host_visible_buffer(
    instance: &VulkanInstanceProbe,
    device: &VulkanLogicalDeviceProbe,
    bytes: &[u8],
    usage: vk::BufferUsageFlags,
    context: &'static str,
) -> Result<VulkanAllocatedBuffer, VulkanSmokeRendererError> {
    let create_info = vk::BufferCreateInfo::default()
        .size(bytes.len().try_into().unwrap_or(u64::MAX))
        .usage(usage)
        .sharing_mode(vk::SharingMode::EXCLUSIVE);
    // SAFETY: The create info is stack-owned and references no external memory.
    let buffer = unsafe { device.device().create_buffer(&create_info, None) }.map_err(|error| {
        VulkanSmokeRendererError::VulkanOperation {
            context,
            result: error,
        }
    })?;
    // SAFETY: The buffer belongs to this device and is queried immediately after creation.
    let requirements = unsafe { device.device().get_buffer_memory_requirements(buffer) };
    let Some(memory_type_index) = find_memory_type(
        instance,
        device.physical_device(),
        requirements.memory_type_bits,
        vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
    ) else {
        // SAFETY: The buffer was created above on this logical device and is destroyed on setup failure.
        unsafe { device.device().destroy_buffer(buffer, None) };
        return Err(VulkanSmokeRendererError::MissingMemoryType { context });
    };
    let allocate_info = vk::MemoryAllocateInfo::default()
        .allocation_size(requirements.size)
        .memory_type_index(memory_type_index);
    let memory =
        // SAFETY: The allocation request matches the queried memory requirements for this buffer.
        unsafe { device.device().allocate_memory(&allocate_info, None) }.map_err(|error| {
            // SAFETY: The buffer was created above on this logical device and is destroyed on setup failure.
            unsafe { device.device().destroy_buffer(buffer, None) };
            VulkanSmokeRendererError::VulkanOperation {
                context,
                result: error,
            }
        })?;
    // SAFETY: The allocation satisfies the queried buffer memory requirements for this device.
    unsafe { device.device().bind_buffer_memory(buffer, memory, 0) }.map_err(|error| {
        // SAFETY: The buffer and allocation were created above on this logical device and are destroyed on setup failure.
        unsafe {
            device.device().destroy_buffer(buffer, None);
            device.device().free_memory(memory, None);
        }
        VulkanSmokeRendererError::VulkanOperation {
            context,
            result: error,
        }
    })?;
    // SAFETY: The mapping range is within the host-visible allocation bound to the buffer.
    let mapped = unsafe {
        device
            .device()
            .map_memory(memory, 0, requirements.size, vk::MemoryMapFlags::empty())
    }
    .map_err(|error| {
        // SAFETY: The buffer and allocation were created above on this logical device and are destroyed on setup failure.
        unsafe {
            device.device().destroy_buffer(buffer, None);
            device.device().free_memory(memory, None);
        }
        VulkanSmokeRendererError::VulkanOperation {
            context,
            result: error,
        }
    })?;
    // SAFETY: The destination points to the mapped allocation and the source slice lives for the copy.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), mapped.cast::<u8>(), bytes.len());
        device.device().unmap_memory(memory);
    }
    Ok(VulkanAllocatedBuffer { buffer, memory })
}

fn find_memory_type(
    instance: &VulkanInstanceProbe,
    physical_device: vk::PhysicalDevice,
    type_bits: u32,
    required: vk::MemoryPropertyFlags,
) -> Option<u32> {
    let properties =
        // SAFETY: The physical device was selected from this live instance and queried by value.
        unsafe {
            instance
                .instance
                .get_physical_device_memory_properties(physical_device)
        };
    let count = usize::try_from(properties.memory_type_count).unwrap_or(0);
    properties.memory_types[..count]
        .iter()
        .enumerate()
        .find_map(|(index, memory_type)| {
            let index_u32 = u32::try_from(index).ok()?;
            let supported = (type_bits & (1_u32 << index_u32)) != 0;
            (supported && memory_type.property_flags.contains(required)).then_some(index_u32)
        })
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
        image_views: Vec::with_capacity(images.len()),
        render_pass: None,
        pipeline_layout: None,
        pipeline: None,
        framebuffers: Vec::with_capacity(images.len()),
        command_buffers: Vec::new(),
    };
    for image in images.iter().copied() {
        match create_image_view(device, image, swapchain.report.plan.format.format) {
            Ok(image_view) => partial.image_views.push(image_view),
            Err(error) => {
                destroy_partial_swapchain_resources(device, command_pool, partial);
                return Err(error);
            }
        }
    }
    let render_pass = match create_render_pass(device, swapchain.report.plan.format.format) {
        Ok(render_pass) => render_pass,
        Err(error) => {
            destroy_partial_swapchain_resources(device, command_pool, partial);
            return Err(error);
        }
    };
    partial.render_pass = Some(render_pass);
    let pipeline_layout = match create_pipeline_layout(device) {
        Ok(pipeline_layout) => pipeline_layout,
        Err(error) => {
            destroy_partial_swapchain_resources(device, command_pool, partial);
            return Err(error);
        }
    };
    partial.pipeline_layout = Some(pipeline_layout);
    let pipeline = match create_graphics_pipeline(
        device,
        render_pass,
        pipeline_layout,
        swapchain.report.plan.extent,
    ) {
        Ok(pipeline) => pipeline,
        Err(error) => {
            destroy_partial_swapchain_resources(device, command_pool, partial);
            return Err(error);
        }
    };
    partial.pipeline = Some(pipeline);
    for image_view in partial.image_views.iter().copied() {
        match create_framebuffer(
            device,
            render_pass,
            image_view,
            swapchain.report.plan.extent,
        ) {
            Ok(framebuffer) => partial.framebuffers.push(framebuffer),
            Err(error) => {
                destroy_partial_swapchain_resources(device, command_pool, partial);
                return Err(error);
            }
        }
    }
    if reuse_command_pool {
        // SAFETY: All command buffers allocated from the live pool are freed before reallocating them.
        unsafe {
            device
                .device()
                .reset_command_pool(command_pool, vk::CommandPoolResetFlags::empty())
        }
        .map_err(|error| VulkanSmokeRendererError::VulkanOperation {
            context: "vkResetCommandPool",
            result: error,
        })?;
    }
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
        .initial_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
        .final_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
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
    // SAFETY: The pipeline layout contains no descriptor sets or push constants.
    unsafe { device.device().create_pipeline_layout(&create_info, None) }.map_err(|error| {
        VulkanSmokeRendererError::VulkanOperation {
            context: "vkCreatePipelineLayout",
            result: error,
        }
    })
}

fn extent_component_to_f32(value: u32) -> f32 {
    u16::try_from(value).map_or(f32::from(u16::MAX), f32::from)
}

#[allow(clippy::too_many_lines)]
fn create_graphics_pipeline(
    device: &VulkanLogicalDeviceProbe,
    render_pass: vk::RenderPass,
    pipeline_layout: vk::PipelineLayout,
    extent: (u32, u32),
) -> Result<vk::Pipeline, VulkanSmokeRendererError> {
    let entry_point = c"main";
    let vertex_module = create_shader_module(device, TRIANGLE_VERTEX_SHADER_WORDS)?;
    let fragment_module = match create_shader_module(device, TRIANGLE_FRAGMENT_SHADER_WORDS) {
        Ok(fragment_module) => fragment_module,
        Err(error) => {
            // SAFETY: The vertex shader module was created above on this logical device and is destroyed on setup failure.
            unsafe { device.device().destroy_shader_module(vertex_module, None) };
            return Err(error);
        }
    };
    let stage_create_infos = [
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::VERTEX)
            .module(vertex_module)
            .name(entry_point),
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::FRAGMENT)
            .module(fragment_module)
            .name(entry_point),
    ];
    let binding_descriptions = [vk::VertexInputBindingDescription {
        binding: 0,
        stride: 20,
        input_rate: vk::VertexInputRate::VERTEX,
    }];
    let attribute_descriptions = [
        vk::VertexInputAttributeDescription {
            location: 0,
            binding: 0,
            format: vk::Format::R32G32_SFLOAT,
            offset: 0,
        },
        vk::VertexInputAttributeDescription {
            location: 1,
            binding: 0,
            format: vk::Format::R32G32B32_SFLOAT,
            offset: 8,
        },
    ];
    let vertex_input_state = vk::PipelineVertexInputStateCreateInfo::default()
        .vertex_binding_descriptions(&binding_descriptions)
        .vertex_attribute_descriptions(&attribute_descriptions);
    let input_assembly_state = vk::PipelineInputAssemblyStateCreateInfo::default()
        .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
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
        .polygon_mode(vk::PolygonMode::FILL)
        .cull_mode(vk::CullModeFlags::BACK)
        .front_face(vk::FrontFace::CLOCKWISE)
        .line_width(1.0);
    let multisample_state = vk::PipelineMultisampleStateCreateInfo::default()
        .rasterization_samples(vk::SampleCountFlags::TYPE_1);
    let color_blend_attachment = [vk::PipelineColorBlendAttachmentState::default()
        .color_write_mask(
            vk::ColorComponentFlags::R
                | vk::ColorComponentFlags::G
                | vk::ColorComponentFlags::B
                | vk::ColorComponentFlags::A,
        )];
    let color_blend_state =
        vk::PipelineColorBlendStateCreateInfo::default().attachments(&color_blend_attachment);
    let create_info = [vk::GraphicsPipelineCreateInfo::default()
        .stages(&stage_create_infos)
        .vertex_input_state(&vertex_input_state)
        .input_assembly_state(&input_assembly_state)
        .viewport_state(&viewport_state)
        .rasterization_state(&rasterization_state)
        .multisample_state(&multisample_state)
        .color_blend_state(&color_blend_state)
        .layout(pipeline_layout)
        .render_pass(render_pass)
        .subpass(0)];
    // SAFETY: The pipeline creation references live shader modules and stack-owned fixed-function descriptors.
    let pipeline_result = unsafe {
        device
            .device()
            .create_graphics_pipelines(vk::PipelineCache::null(), &create_info, None)
    };
    // SAFETY: Shader modules are no longer needed after pipeline creation completes.
    unsafe {
        device.device().destroy_shader_module(vertex_module, None);
        device.device().destroy_shader_module(fragment_module, None);
    }
    let pipeline =
        pipeline_result.map_err(|(_, error)| VulkanSmokeRendererError::VulkanOperation {
            context: "vkCreateGraphicsPipelines",
            result: error,
        })?[0];
    Ok(pipeline)
}

fn create_shader_module(
    device: &VulkanLogicalDeviceProbe,
    words: &[u32],
) -> Result<vk::ShaderModule, VulkanSmokeRendererError> {
    let create_info = vk::ShaderModuleCreateInfo::default().code(words);
    // SAFETY: SPIR-V words are immutable and valid for the duration of the call.
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
    // SAFETY: The framebuffer attachments and render pass remain live for the duration of the call.
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
    // SAFETY: Command buffers are allocated from a live resettable pool owned by this device.
    unsafe { device.device().allocate_command_buffers(&allocate_info) }.map_err(|error| {
        VulkanSmokeRendererError::VulkanOperation {
            context: "vkAllocateCommandBuffers",
            result: error,
        }
    })
}

pub(super) fn create_frame_sync(
    device: &VulkanLogicalDeviceProbe,
) -> Result<Vec<VulkanFrameSync>, VulkanSmokeRendererError> {
    let semaphore_info = vk::SemaphoreCreateInfo::default();
    let fence_info = vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED);
    let mut sync = Vec::with_capacity(2);
    for _ in 0..2 {
        // SAFETY: The sync objects belong to this live logical device and are destroyed at teardown.
        let image_available = unsafe { device.device().create_semaphore(&semaphore_info, None) }
            .map_err(|error| VulkanSmokeRendererError::VulkanOperation {
                context: "vkCreateSemaphore(image_available)",
                result: error,
            })?;
        let render_finished = {
            // SAFETY: The sync objects belong to this live logical device and are destroyed at teardown.
            match unsafe { device.device().create_semaphore(&semaphore_info, None) } {
                Ok(render_finished) => render_finished,
                Err(error) => {
                    destroy_frame_sync_objects(device, &sync);
                    // SAFETY: The semaphore was created above on this logical device and is destroyed on setup failure.
                    unsafe { device.device().destroy_semaphore(image_available, None) };
                    return Err(VulkanSmokeRendererError::VulkanOperation {
                        context: "vkCreateSemaphore(render_finished)",
                        result: error,
                    });
                }
            }
        };
        // SAFETY: The fence belongs to this live logical device and is destroyed at teardown.
        let fence = match unsafe { device.device().create_fence(&fence_info, None) } {
            Ok(fence) => fence,
            Err(error) => {
                destroy_frame_sync_objects(device, &sync);
                // SAFETY: These semaphores were created above on this logical device and are destroyed on setup failure.
                unsafe {
                    device.device().destroy_semaphore(image_available, None);
                    device.device().destroy_semaphore(render_finished, None);
                }
                return Err(VulkanSmokeRendererError::VulkanOperation {
                    context: "vkCreateFence",
                    result: error,
                });
            }
        };
        sync.push(VulkanFrameSync {
            image_available,
            render_finished,
            fence,
        });
    }
    Ok(sync)
}

pub(super) fn destroy_swapchain_resources(
    device: &VulkanLogicalDeviceProbe,
    command_pool: vk::CommandPool,
    resources: VulkanSwapchainResources,
) {
    // SAFETY: All swapchain-dependent objects belong to this device and are destroyed once.
    unsafe {
        device
            .device()
            .free_command_buffers(command_pool, &resources.command_buffers);
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
    resources: PartialSwapchainResources,
) {
    // SAFETY: All handles in this partial resource set were created on this live logical device and are destroyed once.
    unsafe {
        if !resources.command_buffers.is_empty() {
            device
                .device()
                .free_command_buffers(command_pool, &resources.command_buffers);
        }
        for framebuffer in resources.framebuffers {
            device.device().destroy_framebuffer(framebuffer, None);
        }
        if let Some(pipeline) = resources.pipeline {
            device.device().destroy_pipeline(pipeline, None);
        }
        if let Some(pipeline_layout) = resources.pipeline_layout {
            device
                .device()
                .destroy_pipeline_layout(pipeline_layout, None);
        }
        if let Some(render_pass) = resources.render_pass {
            device.device().destroy_render_pass(render_pass, None);
        }
        for image_view in resources.image_views {
            device.device().destroy_image_view(image_view, None);
        }
    }
}

fn destroy_frame_sync_objects(device: &VulkanLogicalDeviceProbe, sync: &[VulkanFrameSync]) {
    for frame_sync in sync {
        // SAFETY: These sync objects belong to this live logical device and are destroyed once during teardown.
        unsafe {
            device
                .device()
                .destroy_semaphore(frame_sync.image_available, None);
            device
                .device()
                .destroy_semaphore(frame_sync.render_finished, None);
            device.device().destroy_fence(frame_sync.fence, None);
        }
    }
}

pub(super) fn destroy_allocated_buffer(
    device: &VulkanLogicalDeviceProbe,
    buffer: &VulkanAllocatedBuffer,
) {
    // SAFETY: The buffer and allocation belong to this live logical device and are destroyed once during teardown.
    unsafe {
        device.device().destroy_buffer(buffer.buffer, None);
        device.device().free_memory(buffer.memory, None);
    }
}

pub(super) fn color_subresource_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .base_mip_level(0)
        .level_count(1)
        .base_array_layer(0)
        .layer_count(1)
}
