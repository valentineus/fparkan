#![allow(unsafe_code)]

use ash::vk;
use fparkan_platform::DepthStencilSupport;

use super::{
    VulkanInstanceProbe, VulkanLogicalDeviceProbe, VulkanPointLight, VulkanSmokeRendererError,
    VulkanStaticMesh, VulkanStaticTexture,
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
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) mip_levels: u32,
}

/// Depth/stencil image and the exact format selected for its render pass.
pub(super) struct VulkanDepthAttachment {
    pub(super) image: VulkanAllocatedImage,
    pub(super) format: vk::Format,
}

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
            width: extent.0,
            height: extent.1,
            mip_levels: 1,
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
    let bytes = encode_static_vertices(&mesh.vertices)?;
    create_host_visible_buffer(
        instance,
        device,
        &bytes,
        vk::BufferUsageFlags::VERTEX_BUFFER,
        "static mesh vertex buffer",
    )
}

/// Allocates a vertex buffer for a frame's retained mesh capacity.
pub(super) fn create_vertex_buffer(
    instance: &VulkanInstanceProbe,
    device: &VulkanLogicalDeviceProbe,
    vertices: &[super::VulkanStaticVertex],
) -> Result<VulkanAllocatedBuffer, VulkanSmokeRendererError> {
    let bytes = encode_static_vertices(vertices)?;
    create_host_visible_buffer(
        instance,
        device,
        &bytes,
        vk::BufferUsageFlags::VERTEX_BUFFER,
        "dynamic mesh vertex buffer",
    )
}

/// Replaces the bytes of a host-visible static vertex allocation.
///
/// The live renderer waits for the device before calling this helper, so a
/// single shared allocation can safely carry camera-relative sky and weather
/// geometry between frames.
pub(super) fn write_static_mesh_vertex_buffer(
    device: &VulkanLogicalDeviceProbe,
    buffer: &VulkanAllocatedBuffer,
    vertices: &[super::VulkanStaticVertex],
) -> Result<(), VulkanSmokeRendererError> {
    let bytes = encode_static_vertices(vertices)?;
    write_mapped_buffer(
        device,
        buffer,
        &bytes,
        "vkMapMemory(static mesh vertex update)",
    )
}

fn encode_static_vertices(
    vertices: &[super::VulkanStaticVertex],
) -> Result<Vec<u8>, VulkanSmokeRendererError> {
    let byte_len = vertices
        .len()
        .checked_mul(14)
        .and_then(|count| count.checked_mul(std::mem::size_of::<f32>()))
        .ok_or(VulkanSmokeRendererError::InvalidStaticMesh {
            context: "static mesh vertex data size overflows address space",
        })?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(byte_len)
        .map_err(|_| VulkanSmokeRendererError::InvalidStaticMesh {
            context: "static mesh vertex data exceeds addressable memory",
        })?;
    for vertex in vertices {
        for value in vertex
            .position
            .into_iter()
            .chain(vertex.color)
            .chain(vertex.normal)
            .chain(vertex.uv)
            .chain(vertex.detail_uv)
            .chain([vertex.overlay_alpha])
        {
            bytes.extend_from_slice(&value.to_ne_bytes());
        }
    }
    Ok(bytes)
}

fn write_mapped_buffer(
    device: &VulkanLogicalDeviceProbe,
    buffer: &VulkanAllocatedBuffer,
    bytes: &[u8],
    context: &'static str,
) -> Result<(), VulkanSmokeRendererError> {
    let byte_len =
        u64::try_from(bytes.len()).map_err(|_| VulkanSmokeRendererError::InvalidStaticMesh {
            context: "static mesh upload exceeds address space",
        })?;
    if byte_len == 0 {
        return Err(VulkanSmokeRendererError::InvalidStaticMesh {
            context: "static mesh upload cannot be empty",
        });
    }
    // SAFETY: The caller validates that the allocation is large enough and
    // waits for all queue work that could read it before this write.
    let mapped = unsafe {
        device
            .device()
            .map_memory(buffer.memory, 0, byte_len, vk::MemoryMapFlags::empty())
    }
    .map_err(|result| VulkanSmokeRendererError::VulkanOperation { context, result })?;
    // SAFETY: `mapped` covers exactly the byte slice copied below.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), mapped.cast::<u8>(), bytes.len());
        device.device().unmap_memory(buffer.memory);
    }
    Ok(())
}

pub(super) fn create_static_mesh_index_buffer(
    instance: &VulkanInstanceProbe,
    device: &VulkanLogicalDeviceProbe,
    mesh: &VulkanStaticMesh,
) -> Result<VulkanAllocatedBuffer, VulkanSmokeRendererError> {
    create_index_buffer(instance, device, &mesh.indices)
}

/// Allocates an index buffer for a frame's retained mesh capacity.
pub(super) fn create_index_buffer(
    instance: &VulkanInstanceProbe,
    device: &VulkanLogicalDeviceProbe,
    indices: &[u32],
) -> Result<VulkanAllocatedBuffer, VulkanSmokeRendererError> {
    let bytes = encode_static_indices(indices)?;
    create_host_visible_buffer(
        instance,
        device,
        &bytes,
        vk::BufferUsageFlags::INDEX_BUFFER,
        "dynamic mesh index buffer",
    )
}

/// Replaces the bytes of a host-visible static index allocation.
pub(super) fn write_static_mesh_index_buffer(
    device: &VulkanLogicalDeviceProbe,
    buffer: &VulkanAllocatedBuffer,
    indices: &[u32],
) -> Result<(), VulkanSmokeRendererError> {
    let bytes = encode_static_indices(indices)?;
    write_mapped_buffer(
        device,
        buffer,
        &bytes,
        "vkMapMemory(static mesh index update)",
    )
}

fn encode_static_indices(indices: &[u32]) -> Result<Vec<u8>, VulkanSmokeRendererError> {
    let byte_len = indices
        .len()
        .checked_mul(std::mem::size_of::<u32>())
        .ok_or(VulkanSmokeRendererError::InvalidStaticMesh {
            context: "static mesh index data size overflows address space",
        })?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(byte_len)
        .map_err(|_| VulkanSmokeRendererError::InvalidStaticMesh {
            context: "static mesh index data exceeds addressable memory",
        })?;
    for &index in indices {
        bytes.extend_from_slice(&index.to_ne_bytes());
    }
    Ok(bytes)
}

pub(super) fn create_static_texture_image(
    instance: &VulkanInstanceProbe,
    device: &VulkanLogicalDeviceProbe,
    command_pool: vk::CommandPool,
    texture: &VulkanStaticTexture,
) -> Result<VulkanAllocatedImage, VulkanSmokeRendererError> {
    texture
        .validate()
        .map_err(|context| VulkanSmokeRendererError::InvalidStaticTexture { context })?;
    let (mip_bytes, mip_levels) = build_static_texture_mip_chain(texture);
    let staging = create_host_visible_buffer(
        instance,
        device,
        &mip_bytes,
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
        .mip_levels(mip_levels)
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
        mip_levels,
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
        .subresource_range(color_subresource_range(mip_levels));
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
        width: texture.width,
        height: texture.height,
        mip_levels,
    })
}

/// Allocates two aligned camera slots used by the dynamic frame UBO.
pub(super) fn create_frame_uniform_buffer(
    instance: &VulkanInstanceProbe,
    device: &VulkanLogicalDeviceProbe,
) -> Result<(VulkanAllocatedBuffer, u64), VulkanSmokeRendererError> {
    const FRAME_BYTES: u64 = super::VULKAN_FRAME_UNIFORM_BYTES as u64;
    let limits =
        // SAFETY: The physical device belongs to this live instance and the query returns a value copy.
        unsafe { instance.instance.get_physical_device_properties(device.physical_device()) };
    let alignment = u64::from(limits.limits.min_uniform_buffer_offset_alignment).max(1);
    let stride = FRAME_BYTES.checked_add(alignment - 1).ok_or(
        VulkanSmokeRendererError::InvalidStaticMesh {
            context: "frame uniform stride overflow",
        },
    )? / alignment
        * alignment;
    let size = stride
        .checked_mul(super::VULKAN_FRAME_SLOT_COUNT as u64)
        .ok_or(VulkanSmokeRendererError::InvalidStaticMesh {
            context: "frame uniform buffer size overflow",
        })?;
    let bytes = vec![
        0_u8;
        usize::try_from(size).map_err(|_| {
            VulkanSmokeRendererError::InvalidStaticMesh {
                context: "frame uniform buffer size exceeds address space",
            }
        })?
    ];
    let buffer = create_host_visible_buffer(
        instance,
        device,
        &bytes,
        vk::BufferUsageFlags::UNIFORM_BUFFER,
        "frame uniform buffer",
    )?;
    Ok((buffer, stride))
}

/// Returns the per-frame descriptor range, aligned slot stride, total bytes,
/// and maximum light count accepted by the device's storage-buffer limit.
pub(super) fn point_light_buffer_layout(
    instance: &VulkanInstanceProbe,
    device: &VulkanLogicalDeviceProbe,
    capacity: usize,
) -> Result<(u64, u64, u64, usize), VulkanSmokeRendererError> {
    let limits =
        // SAFETY: The physical device belongs to this live instance and the query returns a value copy.
        unsafe { instance.instance.get_physical_device_properties(device.physical_device()) }
            .limits;
    let max_range = u64::from(limits.max_storage_buffer_range);
    let header_bytes = super::VULKAN_POINT_LIGHT_HEADER_BYTES as u64;
    let record_bytes = super::VULKAN_POINT_LIGHT_RECORD_BYTES as u64;
    if max_range < header_bytes {
        return Err(VulkanSmokeRendererError::InvalidPointLights {
            context: "device storage-buffer range cannot hold the point-light header",
        });
    }
    let max_count = usize::try_from((max_range - header_bytes) / record_bytes).map_err(|_| {
        VulkanSmokeRendererError::InvalidPointLights {
            context: "device point-light capacity exceeds address space",
        }
    })?;
    if capacity > max_count {
        return Err(VulkanSmokeRendererError::InvalidPointLights {
            context: "point-light count exceeds maxStorageBufferRange",
        });
    }
    let capacity_u64 =
        u64::try_from(capacity).map_err(|_| VulkanSmokeRendererError::InvalidPointLights {
            context: "point-light capacity exceeds address space",
        })?;
    let range = header_bytes
        .checked_add(record_bytes.checked_mul(capacity_u64).ok_or(
            VulkanSmokeRendererError::InvalidPointLights {
                context: "point-light descriptor range overflow",
            },
        )?)
        .ok_or(VulkanSmokeRendererError::InvalidPointLights {
            context: "point-light descriptor range overflow",
        })?;
    let alignment = u64::from(limits.min_storage_buffer_offset_alignment).max(1);
    let stride =
        range
            .checked_add(alignment - 1)
            .ok_or(VulkanSmokeRendererError::InvalidPointLights {
                context: "point-light frame stride overflow",
            })?
            / alignment
            * alignment;
    let total = stride
        .checked_mul(u64::try_from(super::VULKAN_FRAME_SLOT_COUNT).unwrap_or(u64::MAX))
        .ok_or(VulkanSmokeRendererError::InvalidPointLights {
            context: "point-light buffer size overflow",
        })?;
    if stride
        .checked_mul(u64::try_from(super::VULKAN_FRAME_SLOT_COUNT - 1).unwrap_or(u64::MAX))
        .is_none_or(|last_offset| last_offset > u64::from(u32::MAX))
    {
        return Err(VulkanSmokeRendererError::InvalidPointLights {
            context: "point-light dynamic offset exceeds Vulkan's 32-bit limit",
        });
    }
    Ok((range, stride, total, max_count))
}

/// Allocates a fence-safe point-light SSBO with one aligned segment per frame.
pub(super) fn create_point_light_buffer(
    instance: &VulkanInstanceProbe,
    device: &VulkanLogicalDeviceProbe,
    capacity: usize,
) -> Result<(VulkanAllocatedBuffer, u64, u64, usize), VulkanSmokeRendererError> {
    let (range, stride, total, max_count) = point_light_buffer_layout(instance, device, capacity)?;
    let bytes =
        vec![
            0_u8;
            usize::try_from(total).map_err(|_| VulkanSmokeRendererError::InvalidPointLights {
                context: "point-light buffer size exceeds address space",
            })?
        ];
    let buffer = create_host_visible_buffer(
        instance,
        device,
        &bytes,
        vk::BufferUsageFlags::STORAGE_BUFFER,
        "point-light storage buffer",
    )?;
    Ok((buffer, range, stride, max_count))
}

pub(super) fn encode_point_lights(
    lights: &[VulkanPointLight],
) -> Result<Vec<u8>, VulkanSmokeRendererError> {
    let count =
        u32::try_from(lights.len()).map_err(|_| VulkanSmokeRendererError::InvalidPointLights {
            context: "point-light count exceeds the storage header",
        })?;
    let total = super::VULKAN_POINT_LIGHT_HEADER_BYTES
        .checked_add(
            super::VULKAN_POINT_LIGHT_RECORD_BYTES
                .checked_mul(lights.len())
                .ok_or(VulkanSmokeRendererError::InvalidPointLights {
                    context: "point-light upload size overflow",
                })?,
        )
        .ok_or(VulkanSmokeRendererError::InvalidPointLights {
            context: "point-light upload size overflow",
        })?;
    let mut bytes = vec![0; total];
    bytes[0..4].copy_from_slice(&count.to_ne_bytes());
    let mut offset = super::VULKAN_POINT_LIGHT_HEADER_BYTES;
    for light in lights {
        for value in light
            .position
            .into_iter()
            .chain([light.range])
            .chain(light.rgb)
            .chain([if light.active { 1.0 } else { 0.0 }])
            .chain(light.coefficients)
            .chain([if light.active { 1.0 } else { 0.0 }])
        {
            bytes[offset..offset + std::mem::size_of::<f32>()]
                .copy_from_slice(&value.to_ne_bytes());
            offset += std::mem::size_of::<f32>();
        }
    }
    debug_assert_eq!(offset, total);
    Ok(bytes)
}

/// Uploads the current light set into the frame slot whose fence has completed.
pub(super) fn write_point_light_buffer(
    device: &VulkanLogicalDeviceProbe,
    buffer: &VulkanAllocatedBuffer,
    stride: u64,
    frame_index: usize,
    lights: &[VulkanPointLight],
) -> Result<(), VulkanSmokeRendererError> {
    let slot =
        u64::try_from(frame_index).map_err(|_| VulkanSmokeRendererError::InvariantViolation {
            context: "point-light frame slot index",
        })?;
    if frame_index >= super::VULKAN_FRAME_SLOT_COUNT {
        return Err(VulkanSmokeRendererError::InvariantViolation {
            context: "point-light frame slot is out of bounds",
        });
    }
    let offset = stride
        .checked_mul(slot)
        .ok_or(VulkanSmokeRendererError::InvariantViolation {
            context: "point-light frame slot offset",
        })?;
    let bytes = encode_point_lights(lights)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > stride {
        return Err(VulkanSmokeRendererError::InvalidPointLights {
            context: "point-light upload exceeds the allocated frame slot",
        });
    }
    let total = stride
        .checked_mul(u64::try_from(super::VULKAN_FRAME_SLOT_COUNT).unwrap_or(u64::MAX))
        .ok_or(VulkanSmokeRendererError::InvariantViolation {
            context: "point-light buffer mapped size",
        })?;
    let offset =
        usize::try_from(offset).map_err(|_| VulkanSmokeRendererError::InvariantViolation {
            context: "point-light frame slot offset exceeds address space",
        })?;
    // Map the whole allocation because the Vulkan minimum map alignment may
    // exceed minStorageBufferOffsetAlignment used by the dynamic frame offset.
    let mapped = unsafe {
        device
            .device()
            .map_memory(buffer.memory, 0, total, vk::MemoryMapFlags::empty())
    }
    .map_err(|result| VulkanSmokeRendererError::VulkanOperation {
        context: "vkMapMemory(point-light buffer)",
        result,
    })?;
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), mapped.cast::<u8>().add(offset), bytes.len());
        device.device().unmap_memory(buffer.memory);
    }
    Ok(())
}

/// Allocates one aligned storage-buffer record for every material descriptor.
///
/// The descriptor uses a dynamic storage-buffer offset.  Keeping the stride
/// aligned to the physical device limit lets the renderer select a material
/// record with the same descriptor set for every draw without baking material
/// indices into the pipeline layout.
pub(super) fn create_material_specular_buffer(
    instance: &VulkanInstanceProbe,
    device: &VulkanLogicalDeviceProbe,
    material_count: usize,
) -> Result<(VulkanAllocatedBuffer, u64), VulkanSmokeRendererError> {
    if material_count == 0 {
        return Err(VulkanSmokeRendererError::InvariantViolation {
            context: "material specular buffer has no material records",
        });
    }
    let limits =
        // SAFETY: The physical device belongs to this live instance and the query returns a value copy.
        unsafe { instance.instance.get_physical_device_properties(device.physical_device()) };
    let alignment = limits.limits.min_storage_buffer_offset_alignment.max(1);
    let record_bytes = super::VULKAN_MATERIAL_SPECULAR_RECORD_BYTES as u64;
    let stride = record_bytes.checked_add(alignment - 1).ok_or(
        VulkanSmokeRendererError::InvalidStaticMesh {
            context: "material specular stride overflow",
        },
    )? / alignment
        * alignment;
    let count =
        u64::try_from(material_count).map_err(|_| VulkanSmokeRendererError::InvalidStaticMesh {
            context: "material specular count exceeds address space",
        })?;
    let size = stride
        .checked_mul(count)
        .ok_or(VulkanSmokeRendererError::InvalidStaticMesh {
            context: "material specular buffer size overflow",
        })?;
    let bytes =
        vec![
            0_u8;
            usize::try_from(size).map_err(|_| VulkanSmokeRendererError::InvalidStaticMesh {
                context: "material specular buffer size exceeds address space",
            })?
        ];
    let buffer = create_host_visible_buffer(
        instance,
        device,
        &bytes,
        vk::BufferUsageFlags::STORAGE_BUFFER,
        "material specular buffer",
    )?;
    Ok((buffer, stride))
}

/// Replaces all material specular records in one host-coherent upload.
pub(super) fn write_material_specular_buffer(
    device: &VulkanLogicalDeviceProbe,
    buffer: &VulkanAllocatedBuffer,
    stride: u64,
    records: &[[[f32; 4]; 2]],
) -> Result<(), VulkanSmokeRendererError> {
    if records.is_empty() {
        return Err(VulkanSmokeRendererError::InvariantViolation {
            context: "material specular upload has no records",
        });
    }
    let record_bytes = super::VULKAN_MATERIAL_SPECULAR_RECORD_BYTES;
    let total_bytes = stride
        .checked_mul(u64::try_from(records.len()).unwrap_or(u64::MAX))
        .ok_or(VulkanSmokeRendererError::InvariantViolation {
            context: "material specular upload size",
        })?;
    let total_bytes_usize =
        usize::try_from(total_bytes).map_err(|_| VulkanSmokeRendererError::InvariantViolation {
            context: "material specular upload size exceeds address space",
        })?;
    let mut bytes = vec![0_u8; total_bytes_usize];
    for (record_index, record) in records.iter().enumerate() {
        let record_offset = usize::try_from(
            stride
                .checked_mul(u64::try_from(record_index).unwrap_or(u64::MAX))
                .ok_or(VulkanSmokeRendererError::InvariantViolation {
                    context: "material specular upload offset",
                })?,
        )
        .map_err(|_| VulkanSmokeRendererError::InvariantViolation {
            context: "material specular upload offset exceeds address space",
        })?;
        let mut offset = record_offset;
        for vector in record {
            for value in vector {
                bytes[offset..offset + std::mem::size_of::<f32>()]
                    .copy_from_slice(&value.to_ne_bytes());
                offset += std::mem::size_of::<f32>();
            }
        }
        debug_assert_eq!(offset - record_offset, record_bytes);
    }
    // SAFETY: The storage buffer is host-visible and the caller has waited for
    // all prior uses before replacing its records.
    let mapped = unsafe {
        device
            .device()
            .map_memory(buffer.memory, 0, total_bytes, vk::MemoryMapFlags::empty())
    }
    .map_err(|result| VulkanSmokeRendererError::VulkanOperation {
        context: "vkMapMemory(material specular buffer)",
        result,
    })?;
    // SAFETY: The mapped range is the complete upload and the destination is
    // valid for the exact byte count copied below.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), mapped.cast::<u8>(), bytes.len());
        device.device().unmap_memory(buffer.memory);
    }
    Ok(())
}

/// Writes one complete frame block into the slot selected by the
/// fence-safe frame index.
pub(super) fn write_frame_uniform(
    device: &VulkanLogicalDeviceProbe,
    buffer: &VulkanAllocatedBuffer,
    stride: u64,
    frame_index: usize,
    bytes: &[u8; super::VULKAN_FRAME_UNIFORM_BYTES],
) -> Result<(), VulkanSmokeRendererError> {
    let slot =
        u64::try_from(frame_index).map_err(|_| VulkanSmokeRendererError::InvariantViolation {
            context: "frame uniform slot index",
        })?;
    let offset = stride
        .checked_mul(slot)
        .ok_or(VulkanSmokeRendererError::InvariantViolation {
            context: "frame uniform slot offset",
        })?;
    // SAFETY: The offset is one of the two allocated aligned slots and the
    // mapped range is exactly one complete frame block.
    let mapped = unsafe {
        device.device().map_memory(
            buffer.memory,
            offset,
            super::VULKAN_FRAME_UNIFORM_BYTES as u64,
            vk::MemoryMapFlags::empty(),
        )
    }
    .map_err(|result| VulkanSmokeRendererError::VulkanOperation {
        context: "vkMapMemory(frame uniform buffer)",
        result,
    })?;
    // SAFETY: The destination is the mapped frame slot and the byte slice lives
    // for this copy.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), mapped.cast::<u8>(), bytes.len());
        device.device().unmap_memory(buffer.memory);
    }
    Ok(())
}

fn upload_static_texture(
    device: &VulkanLogicalDeviceProbe,
    command_pool: vk::CommandPool,
    staging_buffer: vk::Buffer,
    image: vk::Image,
    width: u32,
    height: u32,
    mip_levels: u32,
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
    let range = color_subresource_range(mip_levels);
    let to_transfer = vk::ImageMemoryBarrier::default()
        .old_layout(vk::ImageLayout::UNDEFINED)
        .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
        .src_access_mask(vk::AccessFlags::empty())
        .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
        .image(image)
        .subresource_range(range);
    let mut regions = Vec::with_capacity(usize::try_from(mip_levels).unwrap_or(0));
    let mut buffer_offset = 0_u64;
    let mut mip_width = width;
    let mut mip_height = height;
    for mip_level in 0..mip_levels {
        let mip_bytes = u64::from(mip_width)
            .saturating_mul(u64::from(mip_height))
            .saturating_mul(4);
        regions.push(
            vk::BufferImageCopy::default()
                .buffer_offset(buffer_offset)
                .image_subresource(
                    vk::ImageSubresourceLayers::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .mip_level(mip_level)
                        .base_array_layer(0)
                        .layer_count(1),
                )
                .image_extent(vk::Extent3D {
                    width: mip_width,
                    height: mip_height,
                    depth: 1,
                }),
        );
        buffer_offset = buffer_offset.saturating_add(mip_bytes);
        mip_width = (mip_width / 2).max(1);
        mip_height = (mip_height / 2).max(1);
    }
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
            &regions,
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

/// Replaces the contents of an already sampled texture without changing its
/// image view or descriptor set.  The caller must wait for the renderer's
/// submitted work before invoking this function.
pub(super) fn update_static_texture_image(
    instance: &VulkanInstanceProbe,
    device: &VulkanLogicalDeviceProbe,
    command_pool: vk::CommandPool,
    image: &VulkanAllocatedImage,
    texture: &VulkanStaticTexture,
) -> Result<(), VulkanSmokeRendererError> {
    texture
        .validate()
        .map_err(|context| VulkanSmokeRendererError::InvalidStaticTexture { context })?;
    let (mip_bytes, mip_levels) = build_static_texture_mip_chain(texture);
    if texture.width != image.width
        || texture.height != image.height
        || mip_levels != image.mip_levels
    {
        return Err(VulkanSmokeRendererError::InvalidStaticTexture {
            context: "dynamic texture extent or mip count differs from allocation",
        });
    }
    let staging = create_host_visible_buffer(
        instance,
        device,
        &mip_bytes,
        vk::BufferUsageFlags::TRANSFER_SRC,
        "dynamic texture staging buffer",
    )?;
    let allocate_info = vk::CommandBufferAllocateInfo::default()
        .command_pool(command_pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(1);
    // SAFETY: The command pool is live and owned by the current logical device.
    let command_buffer = unsafe { device.device().allocate_command_buffers(&allocate_info) }
        .map_err(|result| {
            destroy_allocated_buffer(device, &staging);
            VulkanSmokeRendererError::VulkanOperation {
                context: "vkAllocateCommandBuffers(dynamic texture upload)",
                result,
            }
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
        destroy_allocated_buffer(device, &staging);
        return Err(VulkanSmokeRendererError::VulkanOperation {
            context: "vkBeginCommandBuffer(dynamic texture upload)",
            result,
        });
    }
    let range = color_subresource_range(mip_levels);
    let to_transfer = vk::ImageMemoryBarrier::default()
        .old_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
        .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
        .src_access_mask(vk::AccessFlags::SHADER_READ)
        .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
        .image(image.image)
        .subresource_range(range);
    let mut regions = Vec::with_capacity(usize::try_from(mip_levels).unwrap_or(0));
    let mut buffer_offset = 0_u64;
    let mut mip_width = texture.width;
    let mut mip_height = texture.height;
    for mip_level in 0..mip_levels {
        let mip_bytes = u64::from(mip_width)
            .saturating_mul(u64::from(mip_height))
            .saturating_mul(4);
        regions.push(
            vk::BufferImageCopy::default()
                .buffer_offset(buffer_offset)
                .image_subresource(
                    vk::ImageSubresourceLayers::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .mip_level(mip_level)
                        .base_array_layer(0)
                        .layer_count(1),
                )
                .image_extent(vk::Extent3D {
                    width: mip_width,
                    height: mip_height,
                    depth: 1,
                }),
        );
        buffer_offset = buffer_offset.saturating_add(mip_bytes);
        mip_width = (mip_width / 2).max(1);
        mip_height = (mip_height / 2).max(1);
    }
    let to_sampled = vk::ImageMemoryBarrier::default()
        .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
        .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
        .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
        .dst_access_mask(vk::AccessFlags::SHADER_READ)
        .image(image.image)
        .subresource_range(range);
    // SAFETY: The caller has made all prior shader reads idle, and the image
    // remains exclusively owned by this graphics queue.
    unsafe {
        device.device().cmd_pipeline_barrier(
            command_buffer,
            vk::PipelineStageFlags::FRAGMENT_SHADER,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[to_transfer],
        );
        device.device().cmd_copy_buffer_to_image(
            command_buffer,
            staging.buffer,
            image.image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &regions,
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
        destroy_allocated_buffer(device, &staging);
        return Err(VulkanSmokeRendererError::VulkanOperation {
            context: "vkEndCommandBuffer(dynamic texture upload)",
            result,
        });
    }
    let command_buffers = [command_buffer];
    let submit = [vk::SubmitInfo::default().command_buffers(&command_buffers)];
    // SAFETY: The graphics queue and submitted command buffer remain live
    // until this synchronous wait completes.
    let submit_result = unsafe {
        device
            .device()
            .queue_submit(device.graphics_queue(), &submit, vk::Fence::null())
    };
    let result = submit_result
        .and_then(|()| unsafe { device.device().queue_wait_idle(device.graphics_queue()) });
    // SAFETY: Submission completed or failed synchronously.
    unsafe {
        device
            .device()
            .free_command_buffers(command_pool, &[command_buffer]);
    }
    destroy_allocated_buffer(device, &staging);
    result.map_err(|result| VulkanSmokeRendererError::VulkanOperation {
        context: "vkQueueSubmit/WaitIdle(dynamic texture upload)",
        result,
    })
}

/// Builds an RGBA8 mip chain on the CPU so texture upload does not depend on
/// optional linear-blit support for the device's UNORM format.
fn build_static_texture_mip_chain(texture: &VulkanStaticTexture) -> (Vec<u8>, u32) {
    if !texture.mip_levels.is_empty() {
        let mut bytes = Vec::new();
        for mip in &texture.mip_levels {
            bytes.extend_from_slice(&mip.rgba8);
        }
        return (
            bytes,
            u32::try_from(texture.mip_levels.len()).unwrap_or(u32::MAX),
        );
    }
    let mut bytes = texture.rgba8.clone();
    let mut level = texture.rgba8.clone();
    let mut width = texture.width;
    let mut height = texture.height;
    let mut mip_levels = 1_u32;

    while width > 1 || height > 1 {
        let next_width = (width / 2).max(1);
        let next_height = (height / 2).max(1);
        let mut next = vec![
            0_u8;
            usize::try_from(next_width)
                .unwrap_or(0)
                .saturating_mul(usize::try_from(next_height).unwrap_or(0))
                .saturating_mul(4)
        ];
        for y in 0..next_height {
            for x in 0..next_width {
                let dst = (usize::try_from(y).unwrap_or(0)
                    * usize::try_from(next_width).unwrap_or(0)
                    + usize::try_from(x).unwrap_or(0))
                    * 4;
                let x0 = x.saturating_mul(2).min(width.saturating_sub(1));
                let y0 = y.saturating_mul(2).min(height.saturating_sub(1));
                let x1 = x0.saturating_add(1).min(width.saturating_sub(1));
                let y1 = y0.saturating_add(1).min(height.saturating_sub(1));
                let source_width = usize::try_from(width).unwrap_or(0);
                let samples = [
                    (usize::try_from(y0).unwrap_or(0) * source_width
                        + usize::try_from(x0).unwrap_or(0))
                        * 4,
                    (usize::try_from(y0).unwrap_or(0) * source_width
                        + usize::try_from(x1).unwrap_or(0))
                        * 4,
                    (usize::try_from(y1).unwrap_or(0) * source_width
                        + usize::try_from(x0).unwrap_or(0))
                        * 4,
                    (usize::try_from(y1).unwrap_or(0) * source_width
                        + usize::try_from(x1).unwrap_or(0))
                        * 4,
                ];
                for channel in 0..4 {
                    let sum: u16 = samples
                        .iter()
                        .map(|&offset| u16::from(level[offset + channel]))
                        .sum();
                    next[dst + channel] = u8::try_from((sum + 2) / 4).unwrap_or(u8::MAX);
                }
            }
        }
        bytes.extend_from_slice(&next);
        level = next;
        width = next_width;
        height = next_height;
        mip_levels = mip_levels.saturating_add(1);
    }
    (bytes, mip_levels)
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
        let image_available =
            match unsafe { device.device().create_semaphore(&semaphore_info, None) } {
                Ok(image_available) => image_available,
                Err(error) => {
                    destroy_frame_sync_objects(device, &sync);
                    return Err(VulkanSmokeRendererError::VulkanOperation {
                        context: "vkCreateSemaphore(image_available)",
                        result: error,
                    });
                }
            };
        // SAFETY: The fence belongs to this live logical device and is destroyed at teardown.
        let fence = match unsafe { device.device().create_fence(&fence_info, None) } {
            Ok(fence) => fence,
            Err(error) => {
                destroy_frame_sync_objects(device, &sync);
                // SAFETY: The semaphore was created above on this logical device and is destroyed on setup failure.
                unsafe { device.device().destroy_semaphore(image_available, None) };
                return Err(VulkanSmokeRendererError::VulkanOperation {
                    context: "vkCreateFence",
                    result: error,
                });
            }
        };
        sync.push(VulkanFrameSync {
            image_available,
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

pub(super) fn color_subresource_range(level_count: u32) -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .base_mip_level(0)
        .level_count(level_count)
        .base_array_layer(0)
        .layer_count(1)
}
