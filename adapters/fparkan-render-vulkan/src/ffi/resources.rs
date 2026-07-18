#![allow(unsafe_code)]

use ash::vk;

use super::{
    VulkanInstanceProbe, VulkanLogicalDeviceProbe, VulkanSmokeRendererError, VulkanStaticMesh,
};

pub(super) struct VulkanAllocatedBuffer {
    pub(super) buffer: vk::Buffer,
    pub(super) memory: vk::DeviceMemory,
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
    let mut bytes = Vec::with_capacity(mesh.vertices.len() * 5 * std::mem::size_of::<f32>());
    for vertex in &mesh.vertices {
        for value in vertex.position.into_iter().chain(vertex.color) {
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

pub(super) fn color_subresource_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .base_mip_level(0)
        .level_count(1)
        .base_array_layer(0)
        .layer_count(1)
}
