#![allow(unsafe_code)]

use ash::vk;
use fparkan_platform::DepthStencilSupport;

use super::{
    VulkanInstanceProbe, VulkanLogicalDeviceProbe, VulkanSmokeRendererError, VulkanStaticMesh,
    VulkanStaticTexture,
};
use crate::select_depth_stencil_attachment_format;

pub(super) struct VulkanAllocatedBuffer {
    pub(super) buffer: vk::Buffer,
    pub(super) memory: vk::DeviceMemory,
}

pub(super) struct VulkanAllocatedImage {
    pub(super) image: vk::Image,
    pub(super) memory: vk::DeviceMemory,
    pub(super) view: vk::ImageView,
}

/// Depth/stencil image and the exact format selected for its render pass.
pub(super) struct VulkanDepthAttachment {
    pub(super) image: VulkanAllocatedImage,
    pub(super) format: vk::Format,
}

#[allow(clippy::too_many_lines)]
pub(super) fn create_depth_attachment(
    instance: &VulkanInstanceProbe,
    device: &VulkanLogicalDeviceProbe,
    extent: (u32, u32),
    request: DepthStencilSupport,
) -> Result<VulkanDepthAttachment, VulkanSmokeRendererError> {
    let supported_formats = depth_attachment_formats(instance, device);
    let format = select_depth_stencil_attachment_format(&supported_formats, request)
        .map(vk::Format::from_raw)
        .ok_or(VulkanSmokeRendererError::InvalidStaticMesh {
            context: "logical device has no selected depth attachment format",
        })?;
    let image_info = vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(format)
        .extent(vk::Extent3D {
            width: extent.0,
            height: extent.1,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::OPTIMAL)
        .usage(vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED);
    // SAFETY: The creation info is stack-owned and uses the live non-zero swapchain extent.
    let image = unsafe { device.device().create_image(&image_info, None) }.map_err(|result| {
        VulkanSmokeRendererError::VulkanOperation {
            context: "vkCreateImage(depth attachment)",
            result,
        }
    })?;
    // SAFETY: The newly created image belongs to this device.
    let requirements = unsafe { device.device().get_image_memory_requirements(image) };
    let Some(memory_type_index) = find_memory_type(
        instance,
        device.physical_device(),
        requirements.memory_type_bits,
        vk::MemoryPropertyFlags::DEVICE_LOCAL,
    ) else {
        // SAFETY: The unbound image is rolled back on its owning device.
        unsafe { device.device().destroy_image(image, None) };
        return Err(VulkanSmokeRendererError::MissingMemoryType {
            context: "depth attachment",
        });
    };
    let allocation = vk::MemoryAllocateInfo::default()
        .allocation_size(requirements.size)
        .memory_type_index(memory_type_index);
    // SAFETY: Allocation parameters are derived from this image's requirements.
    let allocation_result = unsafe { device.device().allocate_memory(&allocation, None) };
    let memory = match allocation_result {
        Ok(memory) => memory,
        Err(result) => {
            // SAFETY: The unbound image is rolled back on allocation failure.
            unsafe { device.device().destroy_image(image, None) };
            return Err(VulkanSmokeRendererError::VulkanOperation {
                context: "vkAllocateMemory(depth attachment)",
                result,
            });
        }
    };
    // SAFETY: The allocation satisfies this image's queried requirements.
    if let Err(result) = unsafe { device.device().bind_image_memory(image, memory, 0) } {
        // SAFETY: Both newly created resources are rolled back together.
        unsafe {
            device.device().destroy_image(image, None);
            device.device().free_memory(memory, None);
        }
        return Err(VulkanSmokeRendererError::VulkanOperation {
            context: "vkBindImageMemory(depth attachment)",
            result,
        });
    }
    let range = vk::ImageSubresourceRange::default()
        .aspect_mask(depth_aspect(format))
        .base_mip_level(0)
        .level_count(1)
        .base_array_layer(0)
        .layer_count(1);
    let view_info = vk::ImageViewCreateInfo::default()
        .image(image)
        .view_type(vk::ImageViewType::TYPE_2D)
        .format(format)
        .subresource_range(range);
    // SAFETY: The image is bound and the depth/stencil aspect matches its selected format.
    let view_result = unsafe { device.device().create_image_view(&view_info, None) };
    let view = match view_result {
        Ok(view) => view,
        Err(result) => {
            // SAFETY: Both newly created resources are rolled back together.
            unsafe {
                device.device().destroy_image(image, None);
                device.device().free_memory(memory, None);
            }
            return Err(VulkanSmokeRendererError::VulkanOperation {
                context: "vkCreateImageView(depth attachment)",
                result,
            });
        }
    };
    Ok(VulkanDepthAttachment {
        image: VulkanAllocatedImage {
            image,
            memory,
            view,
        },
        format,
    })
}

pub(super) fn destroy_depth_attachment(
    device: &VulkanLogicalDeviceProbe,
    attachment: &VulkanDepthAttachment,
) {
    destroy_allocated_image(device, &attachment.image);
}

fn depth_attachment_formats(
    instance: &VulkanInstanceProbe,
    device: &VulkanLogicalDeviceProbe,
) -> Vec<i32> {
    [
        vk::Format::D16_UNORM,
        vk::Format::X8_D24_UNORM_PACK32,
        vk::Format::D32_SFLOAT,
        vk::Format::D16_UNORM_S8_UINT,
        vk::Format::D24_UNORM_S8_UINT,
        vk::Format::D32_SFLOAT_S8_UINT,
    ]
    .into_iter()
    .filter(|format| {
        // SAFETY: The physical device belongs to this instance and the query returns a value copy.
        unsafe {
            instance
                .instance
                .get_physical_device_format_properties(device.physical_device(), *format)
        }
        .optimal_tiling_features
        .contains(vk::FormatFeatureFlags::DEPTH_STENCIL_ATTACHMENT)
    })
    .map(vk::Format::as_raw)
    .collect()
}

fn depth_aspect(format: vk::Format) -> vk::ImageAspectFlags {
    match format {
        vk::Format::D16_UNORM_S8_UINT
        | vk::Format::D24_UNORM_S8_UINT
        | vk::Format::D32_SFLOAT_S8_UINT => {
            vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL
        }
        _ => vk::ImageAspectFlags::DEPTH,
    }
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

pub(super) fn create_static_mesh_vertex_buffer(
    instance: &VulkanInstanceProbe,
    device: &VulkanLogicalDeviceProbe,
    mesh: &VulkanStaticMesh,
) -> Result<VulkanAllocatedBuffer, VulkanSmokeRendererError> {
    let mut bytes = Vec::with_capacity(mesh.vertices.len() * 7 * std::mem::size_of::<f32>());
    for vertex in &mesh.vertices {
        for value in vertex
            .position
            .into_iter()
            .chain(vertex.color)
            .chain(vertex.uv)
        {
            bytes.extend_from_slice(&value.to_ne_bytes());
        }
    }
    create_host_visible_buffer(
        instance,
        device,
        &bytes,
        vk::BufferUsageFlags::VERTEX_BUFFER,
        "static mesh vertex buffer",
    )
}

pub(super) fn create_static_mesh_index_buffer(
    instance: &VulkanInstanceProbe,
    device: &VulkanLogicalDeviceProbe,
    mesh: &VulkanStaticMesh,
) -> Result<VulkanAllocatedBuffer, VulkanSmokeRendererError> {
    let mut bytes = Vec::with_capacity(mesh.indices.len() * std::mem::size_of::<u16>());
    for &index in &mesh.indices {
        bytes.extend_from_slice(&index.to_ne_bytes());
    }
    create_host_visible_buffer(
        instance,
        device,
        &bytes,
        vk::BufferUsageFlags::INDEX_BUFFER,
        "static mesh index buffer",
    )
}

#[allow(clippy::too_many_lines)]
pub(super) fn create_static_texture_image(
    instance: &VulkanInstanceProbe,
    device: &VulkanLogicalDeviceProbe,
    command_pool: vk::CommandPool,
    texture: &VulkanStaticTexture,
) -> Result<VulkanAllocatedImage, VulkanSmokeRendererError> {
    let staging = create_host_visible_buffer(
        instance,
        device,
        &texture.rgba8,
        vk::BufferUsageFlags::TRANSFER_SRC,
        "static texture staging buffer",
    )?;
    let image_info = vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(vk::Format::R8G8B8A8_UNORM)
        .extent(vk::Extent3D {
            width: texture.width,
            height: texture.height,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::OPTIMAL)
        .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED);
    // SAFETY: The create info only contains stack-owned values and a validated non-zero extent.
    let image = unsafe { device.device().create_image(&image_info, None) }.map_err(|result| {
        destroy_allocated_buffer(device, &staging);
        VulkanSmokeRendererError::VulkanOperation {
            context: "vkCreateImage(static texture)",
            result,
        }
    })?;
    // SAFETY: The image belongs to this device and is queried immediately after creation.
    let requirements = unsafe { device.device().get_image_memory_requirements(image) };
    let Some(memory_type_index) = find_memory_type(
        instance,
        device.physical_device(),
        requirements.memory_type_bits,
        vk::MemoryPropertyFlags::DEVICE_LOCAL,
    ) else {
        // SAFETY: Both resources were created on this device and are being rolled back once.
        unsafe { device.device().destroy_image(image, None) };
        destroy_allocated_buffer(device, &staging);
        return Err(VulkanSmokeRendererError::MissingMemoryType {
            context: "static texture image",
        });
    };
    let allocate_info = vk::MemoryAllocateInfo::default()
        .allocation_size(requirements.size)
        .memory_type_index(memory_type_index);
    // SAFETY: The allocation matches the image memory requirements queried above.
    let memory = {
        // SAFETY: The allocation matches the image memory requirements queried above.
        unsafe { device.device().allocate_memory(&allocate_info, None) }
    }
    .map_err(|result| {
        // SAFETY: Both resources were created on this device and are being rolled back once.
        unsafe { device.device().destroy_image(image, None) };
        destroy_allocated_buffer(device, &staging);
        VulkanSmokeRendererError::VulkanOperation {
            context: "vkAllocateMemory(static texture)",
            result,
        }
    })?;
    // SAFETY: The allocation matches this image's queried requirements.
    if let Err(result) = unsafe { device.device().bind_image_memory(image, memory, 0) } {
        // SAFETY: Both resources were created on this device and are being rolled back once.
        unsafe {
            device.device().destroy_image(image, None);
            device.device().free_memory(memory, None);
        };
        destroy_allocated_buffer(device, &staging);
        return Err(VulkanSmokeRendererError::VulkanOperation {
            context: "vkBindImageMemory(static texture)",
            result,
        });
    }
    if let Err(error) = upload_static_texture(
        device,
        command_pool,
        staging.buffer,
        image,
        texture.width,
        texture.height,
    ) {
        // SAFETY: Both resources were created on this device and are being rolled back once.
        unsafe {
            device.device().destroy_image(image, None);
            device.device().free_memory(memory, None);
        };
        destroy_allocated_buffer(device, &staging);
        return Err(error);
    }
    destroy_allocated_buffer(device, &staging);
    let view_info = vk::ImageViewCreateInfo::default()
        .image(image)
        .view_type(vk::ImageViewType::TYPE_2D)
        .format(vk::Format::R8G8B8A8_UNORM)
        .subresource_range(color_subresource_range());
    // SAFETY: The image is live, initialized and has the stated color subresource.
    let view = {
        // SAFETY: The image is live, initialized and has the stated color subresource.
        unsafe { device.device().create_image_view(&view_info, None) }
    }
    .map_err(|result| {
        // SAFETY: Both resources were created on this device and are being rolled back once.
        unsafe {
            device.device().destroy_image(image, None);
            device.device().free_memory(memory, None);
        };
        VulkanSmokeRendererError::VulkanOperation {
            context: "vkCreateImageView(static texture)",
            result,
        }
    })?;
    Ok(VulkanAllocatedImage {
        image,
        memory,
        view,
    })
}

#[allow(clippy::too_many_lines)]
fn upload_static_texture(
    device: &VulkanLogicalDeviceProbe,
    command_pool: vk::CommandPool,
    staging_buffer: vk::Buffer,
    image: vk::Image,
    width: u32,
    height: u32,
) -> Result<(), VulkanSmokeRendererError> {
    let allocate_info = vk::CommandBufferAllocateInfo::default()
        .command_pool(command_pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(1);
    // SAFETY: The command pool is live and owned by the current logical device.
    let command_buffer = unsafe { device.device().allocate_command_buffers(&allocate_info) }
        .map_err(|result| VulkanSmokeRendererError::VulkanOperation {
            context: "vkAllocateCommandBuffers(texture upload)",
            result,
        })?[0];
    let begin =
        vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
    // SAFETY: The command buffer is freshly allocated from the current pool.
    if let Err(result) = unsafe { device.device().begin_command_buffer(command_buffer, &begin) } {
        // SAFETY: The command buffer belongs to this pool and is released on failure.
        unsafe {
            device
                .device()
                .free_command_buffers(command_pool, &[command_buffer]);
        };
        return Err(VulkanSmokeRendererError::VulkanOperation {
            context: "vkBeginCommandBuffer(texture upload)",
            result,
        });
    }
    let range = color_subresource_range();
    let to_transfer = vk::ImageMemoryBarrier::default()
        .old_layout(vk::ImageLayout::UNDEFINED)
        .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
        .src_access_mask(vk::AccessFlags::empty())
        .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
        .image(image)
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
            width,
            height,
            depth: 1,
        });
    let to_sampled = vk::ImageMemoryBarrier::default()
        .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
        .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
        .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
        .dst_access_mask(vk::AccessFlags::SHADER_READ)
        .image(image)
        .subresource_range(range);
    // SAFETY: The commands operate on live, exclusively owned resources and the stated color subresource.
    unsafe {
        device.device().cmd_pipeline_barrier(
            command_buffer,
            vk::PipelineStageFlags::TOP_OF_PIPE,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[to_transfer],
        );
        device.device().cmd_copy_buffer_to_image(
            command_buffer,
            staging_buffer,
            image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &[region],
        );
        device.device().cmd_pipeline_barrier(
            command_buffer,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::FRAGMENT_SHADER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[to_sampled],
        );
    }
    // SAFETY: Command recording is complete on the current command buffer.
    if let Err(result) = unsafe { device.device().end_command_buffer(command_buffer) } {
        // SAFETY: The command buffer belongs to this pool and is released on failure.
        unsafe {
            device
                .device()
                .free_command_buffers(command_pool, &[command_buffer]);
        };
        return Err(VulkanSmokeRendererError::VulkanOperation {
            context: "vkEndCommandBuffer(texture upload)",
            result,
        });
    }
    let command_buffers = [command_buffer];
    let submit = [vk::SubmitInfo::default().command_buffers(&command_buffers)];
    // SAFETY: The graphics queue and submitted command buffer are live for the duration of the wait.
    let submit_result = unsafe {
        device
            .device()
            .queue_submit(device.graphics_queue(), &submit, vk::Fence::null())
    };
    let result = submit_result.and_then(|()| {
        // SAFETY: The graphics queue remains live until the synchronous idle wait completes.
        unsafe { device.device().queue_wait_idle(device.graphics_queue()) }
    });
    // SAFETY: Submission completed or failed synchronously and the command buffer is no longer needed.
    unsafe {
        device
            .device()
            .free_command_buffers(command_pool, &[command_buffer]);
    };
    result.map_err(|result| VulkanSmokeRendererError::VulkanOperation {
        context: "vkQueueSubmit/WaitIdle(texture upload)",
        result,
    })
}

pub(super) fn destroy_allocated_image(
    device: &VulkanLogicalDeviceProbe,
    image: &VulkanAllocatedImage,
) {
    // SAFETY: The image, view and allocation belong to this device and are destroyed once after idle.
    unsafe {
        device.device().destroy_image_view(image.view, None);
        device.device().destroy_image(image.image, None);
        device.device().free_memory(image.memory, None);
    };
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

/// Allocates a host-coherent transfer destination for a swapchain image copy.
pub(super) fn create_readback_buffer(
    instance: &VulkanInstanceProbe,
    device: &VulkanLogicalDeviceProbe,
    byte_len: usize,
) -> Result<VulkanAllocatedBuffer, VulkanSmokeRendererError> {
    create_host_visible_buffer(
        instance,
        device,
        &vec![0; byte_len],
        vk::BufferUsageFlags::TRANSFER_DST,
        "vkCreateBuffer(readback)",
    )
}

/// Copies a completed host-coherent readback allocation into CPU-owned bytes.
pub(super) fn readback_buffer_bytes(
    device: &VulkanLogicalDeviceProbe,
    buffer: &VulkanAllocatedBuffer,
    byte_len: usize,
) -> Result<Vec<u8>, VulkanSmokeRendererError> {
    // SAFETY: The caller waits for device idle before mapping this host-visible allocation.
    let mapped = unsafe {
        device.device().map_memory(
            buffer.memory,
            0,
            u64::try_from(byte_len).unwrap_or(u64::MAX),
            vk::MemoryMapFlags::empty(),
        )
    }
    .map_err(|result| VulkanSmokeRendererError::VulkanOperation {
        context: "vkMapMemory(readback)",
        result,
    })?;
    let mut bytes = vec![0; byte_len];
    // SAFETY: Both ranges contain exactly byte_len initialized bytes and do not overlap.
    unsafe {
        std::ptr::copy_nonoverlapping(mapped.cast::<u8>(), bytes.as_mut_ptr(), byte_len);
        device.device().unmap_memory(buffer.memory);
    }
    Ok(bytes)
}

pub(super) fn color_subresource_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .base_mip_level(0)
        .level_count(1)
        .base_array_layer(0)
        .layer_count(1)
}
