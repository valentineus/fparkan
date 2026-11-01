#![allow(unsafe_code)]

use super::{
    VulkanAllocatedImage, VulkanDepthAttachment, VulkanInstanceProbe, VulkanLogicalDeviceProbe,
    VulkanSmokeRendererError,
};
use ash::vk;

pub(super) fn reflected_pipeline_state(
    mut state: fparkan_render::LegacyPipelineState,
) -> fparkan_render::LegacyPipelineState {
    use fparkan_render::LegacyCullMode;
    state.cull = match state.cull {
        LegacyCullMode::Disabled => LegacyCullMode::Disabled,
        LegacyCullMode::BackFace => LegacyCullMode::FrontFace,
        LegacyCullMode::FrontFace => LegacyCullMode::BackFace,
    };
    state
}

pub(super) fn mesh_reflection_planes(mesh: &super::VulkanStaticMesh) -> Vec<Option<f32>> {
    mesh.draw_ranges
        .iter()
        .map(|range| {
            if !range.terrain_reflection || range.index_count == 0 {
                return None;
            }
            let indices = &mesh.indices[range.first_index as usize
                ..range.first_index as usize + range.index_count as usize];
            let height = mesh.vertices[indices[0] as usize].position[2];
            indices
                .iter()
                .all(|&index| (mesh.vertices[index as usize].position[2] - height).abs() <= 1.0e-4)
                .then_some(height)
        })
        .collect()
}

/// Owned before pipeline creation; a dummy set prevents attachment feedback in the mirror pass.
pub(super) struct ReflectionResources {
    device: ash::Device,
    pub layout: vk::DescriptorSetLayout,
    pool: vk::DescriptorPool,
    sampler: vk::Sampler,
    pub sets: [vk::DescriptorSet; 2],
    dummy: Option<VulkanAllocatedImage>,
    pub color: Option<VulkanAllocatedImage>,
    depth: Option<VulkanDepthAttachment>,
    pub framebuffer: vk::Framebuffer,
}

impl ReflectionResources {
    pub fn new(
        instance: &VulkanInstanceProbe,
        device: &VulkanLogicalDeviceProbe,
        command_pool: vk::CommandPool,
    ) -> Result<Self, VulkanSmokeRendererError> {
        let mut owned = Self {
            device: device.device().clone(),
            layout: vk::DescriptorSetLayout::null(),
            pool: vk::DescriptorPool::null(),
            sampler: vk::Sampler::null(),
            sets: [vk::DescriptorSet::null(); 2],
            dummy: None,
            color: None,
            depth: None,
            framebuffer: vk::Framebuffer::null(),
        };
        let binding = vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::FRAGMENT);
        let layout_info =
            vk::DescriptorSetLayoutCreateInfo::default().bindings(std::slice::from_ref(&binding));
        // SAFETY: The device is live; owned rolls back every handle on later failure.
        owned.layout = unsafe {
            owned
                .device
                .create_descriptor_set_layout(&layout_info, None)
        }
        .map_err(|result| VulkanSmokeRendererError::VulkanOperation {
            context: "vkCreateDescriptorSetLayout(reflection)",
            result,
        })?;
        let size = vk::DescriptorPoolSize::default()
            .ty(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(2);
        let pool_info = vk::DescriptorPoolCreateInfo::default()
            .max_sets(2)
            .pool_sizes(std::slice::from_ref(&size));
        // SAFETY: The pool owns only the two reflection descriptor sets below.
        owned.pool =
            unsafe { owned.device.create_descriptor_pool(&pool_info, None) }.map_err(|result| {
                VulkanSmokeRendererError::VulkanOperation {
                    context: "vkCreateDescriptorPool(reflection)",
                    result,
                }
            })?;
        let layouts = [owned.layout; 2];
        let allocation = vk::DescriptorSetAllocateInfo::default()
            .descriptor_pool(owned.pool)
            .set_layouts(&layouts);
        // SAFETY: The pool has capacity for these two live layouts.
        let sets =
            unsafe { owned.device.allocate_descriptor_sets(&allocation) }.map_err(|result| {
                VulkanSmokeRendererError::VulkanOperation {
                    context: "vkAllocateDescriptorSets(reflection)",
                    result,
                }
            })?;
        owned.sets.copy_from_slice(&sets);
        let sampler = vk::SamplerCreateInfo::default()
            .mag_filter(vk::Filter::LINEAR)
            .min_filter(vk::Filter::LINEAR)
            .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE);
        // SAFETY: No optional device feature is used by this sampler.
        owned.sampler =
            unsafe { owned.device.create_sampler(&sampler, None) }.map_err(|result| {
                VulkanSmokeRendererError::VulkanOperation {
                    context: "vkCreateSampler(reflection)",
                    result,
                }
            })?;
        // Material images can be replaced by animation, so the dummy has its own lifetime.
        owned.dummy = Some(super::create_static_texture_image(
            instance,
            device,
            command_pool,
            &super::VulkanStaticTexture {
                width: 1,
                height: 1,
                rgba8: vec![255; 4],
                mip_levels: Vec::new(),
            },
        )?);
        for set in owned.sets {
            owned.write_image(set, owned.dummy.as_ref().expect("dummy image exists").view);
        }
        Ok(owned)
    }

    pub fn create_target(
        &mut self,
        instance: &VulkanInstanceProbe,
        device: &VulkanLogicalDeviceProbe,
        extent: (u32, u32),
        format: i32,
        depth_request: fparkan_platform::DepthStencilSupport,
        render_pass: vk::RenderPass,
    ) -> Result<(), VulkanSmokeRendererError> {
        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk::Format::from_raw(format))
            .extent(vk::Extent3D {
                width: extent.0,
                height: extent.1,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::SAMPLED)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        // SAFETY: This target uses the validated non-zero swapchain extent and format.
        let image = unsafe { self.device.create_image(&info, None) }.map_err(|result| {
            VulkanSmokeRendererError::VulkanOperation {
                context: "vkCreateImage(reflection)",
                result,
            }
        })?;
        self.color = Some(VulkanAllocatedImage {
            image,
            memory: vk::DeviceMemory::null(),
            view: vk::ImageView::null(),
            width: extent.0,
            height: extent.1,
            mip_levels: 1,
        });
        // SAFETY: The image belongs to this device and has not been bound yet.
        let requirements = unsafe { self.device.get_image_memory_requirements(image) };
        let memory_type_index = super::resources::find_memory_type(
            instance,
            device.physical_device(),
            requirements.memory_type_bits,
            vk::MemoryPropertyFlags::DEVICE_LOCAL,
        )
        .ok_or(VulkanSmokeRendererError::MissingMemoryType {
            context: "reflection target",
        })?;
        let allocation = vk::MemoryAllocateInfo::default()
            .allocation_size(requirements.size)
            .memory_type_index(memory_type_index);
        // SAFETY: The allocation follows this image's memory requirements.
        let memory =
            unsafe { self.device.allocate_memory(&allocation, None) }.map_err(|result| {
                VulkanSmokeRendererError::VulkanOperation {
                    context: "vkAllocateMemory(reflection)",
                    result,
                }
            })?;
        self.color.as_mut().expect("color image exists").memory = memory;
        // SAFETY: The allocation is compatible with the image and offset zero.
        unsafe { self.device.bind_image_memory(image, memory, 0) }.map_err(|result| {
            VulkanSmokeRendererError::VulkanOperation {
                context: "vkBindImageMemory(reflection)",
                result,
            }
        })?;
        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(vk::Format::from_raw(format))
            .subresource_range(super::color_subresource_range(1));
        // SAFETY: The image is bound and the view covers its single color subresource.
        let view =
            unsafe { self.device.create_image_view(&view_info, None) }.map_err(|result| {
                VulkanSmokeRendererError::VulkanOperation {
                    context: "vkCreateImageView(reflection)",
                    result,
                }
            })?;
        self.color.as_mut().expect("color image exists").view = view;
        self.depth = Some(super::create_depth_attachment(
            instance,
            device,
            extent,
            depth_request,
        )?);
        let attachments = [view, self.depth.as_ref().expect("depth exists").image.view];
        let info = vk::FramebufferCreateInfo::default()
            .render_pass(render_pass)
            .attachments(&attachments)
            .width(extent.0)
            .height(extent.1)
            .layers(1);
        // SAFETY: The target formats and extent match the shared render pass.
        self.framebuffer =
            unsafe { self.device.create_framebuffer(&info, None) }.map_err(|result| {
                VulkanSmokeRendererError::VulkanOperation {
                    context: "vkCreateFramebuffer(reflection)",
                    result,
                }
            })?;
        self.write_image(self.sets[1], view);
        Ok(())
    }

    fn write_image(&self, set: vk::DescriptorSet, view: vk::ImageView) {
        let image = vk::DescriptorImageInfo::default()
            .sampler(self.sampler)
            .image_view(view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
        let write = vk::WriteDescriptorSet::default()
            .dst_set(set)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(std::slice::from_ref(&image));
        // SAFETY: Sets are initialized before submission; the dummy remains valid for every pass.
        unsafe { self.device.update_descriptor_sets(&[write], &[]) };
    }
}

impl Drop for ReflectionResources {
    fn drop(&mut self) {
        // SAFETY: The parent waits for submissions and drops this owner before the render pass/device.
        // Null handles also cover partial construction, including failures before memory binding.
        unsafe {
            self.device.destroy_framebuffer(self.framebuffer, None);
            self.device.destroy_descriptor_pool(self.pool, None);
            self.device.destroy_sampler(self.sampler, None);
            self.device.destroy_descriptor_set_layout(self.layout, None);
            for image in self
                .color
                .iter()
                .chain(self.depth.iter().map(|depth| &depth.image))
                .chain(self.dummy.iter())
            {
                self.device.destroy_image_view(image.view, None);
                self.device.destroy_image(image.image, None);
                self.device.free_memory(image.memory, None);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{VulkanStaticDrawRange, VulkanStaticMesh, VulkanStaticVertex};

    #[test]
    fn reflection_requires_a_flat_flagged_surface_and_reverses_only_culling() {
        let mut mesh = VulkanStaticMesh {
            vertices: vec![
                VulkanStaticVertex {
                    position: [0.0, 0.0, 150.0],
                    color: [1.0; 3],
                    normal: [0.0, 0.0, 1.0],
                    uv: [0.0; 2],
                    detail_uv: [0.0; 2],
                    overlay_alpha: 0.0,
                };
                3
            ],
            indices: vec![0, 1, 2],
            draw_ranges: vec![VulkanStaticDrawRange {
                first_index: 0,
                index_count: 3,
                material_index: 0,
                lightmap_index: 255,
                batch_flags: 0,
                terrain_base_only: false,
                terrain_reflection: true,
                pipeline_state: fparkan_render::LegacyPipelineState::default(),
                alpha_test_reference: 1,
            }],
        };
        assert_eq!(mesh_reflection_planes(&mesh), vec![Some(150.0)]);
        mesh.vertices[2].position[2] = 151.0;
        assert_eq!(mesh_reflection_planes(&mesh), vec![None]);
        mesh.vertices[2].position[2] = 150.0;
        mesh.draw_ranges[0].terrain_reflection = false;
        assert_eq!(mesh_reflection_planes(&mesh), vec![None]);
        for cull in [
            fparkan_render::LegacyCullMode::Disabled,
            fparkan_render::LegacyCullMode::BackFace,
            fparkan_render::LegacyCullMode::FrontFace,
        ] {
            let mut state = mesh.draw_ranges[0].pipeline_state;
            state.cull = cull;
            state.alpha_test = true;
            state.depth_bias = fparkan_render::LegacyDepthBias::ProjectedDecal;
            assert_eq!(
                reflected_pipeline_state(reflected_pipeline_state(state)),
                state
            );
            let reflected = reflected_pipeline_state(state);
            assert_eq!(reflected.depth, state.depth);
            assert_eq!(reflected.blend, state.blend);
            assert_eq!(reflected.alpha_test, state.alpha_test);
        }
    }
}
