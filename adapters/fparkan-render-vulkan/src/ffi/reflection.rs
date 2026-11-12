#![allow(unsafe_code)]

use super::{
    VulkanAllocatedImage, VulkanDepthAttachment, VulkanInstanceProbe, VulkanLogicalDeviceProbe,
    VulkanSmokeRendererError,
};
use ash::vk;

fn reflection_copy_format(features: [vk::FormatFeatureFlags; 2]) -> Option<vk::Format> {
    let required =
        vk::FormatFeatureFlags::SAMPLED_IMAGE | vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_LINEAR;
    [
        vk::Format::R5G6B5_UNORM_PACK16,
        vk::Format::A1R5G5B5_UNORM_PACK16,
    ]
    .into_iter()
    .zip(features)
    .find(|(_, available)| available.contains(required))
    .map(|(format, _)| format)
}

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

pub(super) fn reflected_terrain_pipeline_state(
    state: fparkan_render::LegacyPipelineState,
) -> fparkan_render::LegacyPipelineState {
    // Native reflected Land packets retain clockwise faces; water keeps its underside.
    fparkan_render::LegacyPipelineState {
        cull: fparkan_render::LegacyCullMode::BackFace,
        ..state
    }
}

pub(super) fn reflected_draw_pipeline_state(
    range: &super::VulkanStaticDrawRange,
    shifted_reflection: bool,
) -> fparkan_render::LegacyPipelineState {
    let state = reflected_pipeline_state(range.pipeline_state);
    if range.terrain_reflection {
        fparkan_render::water::legacy_reflected_water_pipeline(state)
    } else if range.terrain_surface && shifted_reflection {
        reflected_terrain_pipeline_state(range.pipeline_state)
    } else {
        state
    }
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

pub(super) fn mesh_reflection_bounds(mesh: &super::VulkanStaticMesh) -> Option<[f32; 4]> {
    let mut bounds = [
        f32::INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
    ];
    for range in mesh
        .draw_ranges
        .iter()
        .filter(|range| range.terrain_reflection)
    {
        for &index in &mesh.indices
            [range.first_index as usize..range.first_index as usize + range.index_count as usize]
        {
            let position = mesh.vertices[index as usize].position;
            bounds[0] = bounds[0].min(position[0]);
            bounds[1] = bounds[1].min(position[1]);
            bounds[2] = bounds[2].max(position[0]);
            bounds[3] = bounds[3].max(position[1]);
        }
    }
    (bounds.iter().all(|v| v.is_finite()) && bounds[2] > bounds[0] && bounds[3] > bounds[1])
        .then_some(bounds)
}

/// Owned before pipeline creation; a dummy set prevents attachment feedback in the mirror pass.
pub(super) struct ReflectionResources {
    device: ash::Device,
    pub layout: vk::DescriptorSetLayout,
    pool: vk::DescriptorPool,
    sampler: vk::Sampler,
    bump_sampler: vk::Sampler,
    pub sets: [vk::DescriptorSet; 2],
    dummy: Option<VulkanAllocatedImage>,
    bump: Option<VulkanAllocatedImage>,
    pub color: Option<VulkanAllocatedImage>,
    pub render_pass: vk::RenderPass,
    depth: Option<VulkanDepthAttachment>,
    pub framebuffer: vk::Framebuffer,
    pub copy_material_mode: i32,
}

impl ReflectionResources {
    pub fn new(
        instance: &VulkanInstanceProbe,
        device: &VulkanLogicalDeviceProbe,
        command_pool: vk::CommandPool,
        enabled: bool,
    ) -> Result<Self, VulkanSmokeRendererError> {
        let mut owned = Self {
            device: device.device().clone(),
            layout: vk::DescriptorSetLayout::null(),
            pool: vk::DescriptorPool::null(),
            sampler: vk::Sampler::null(),
            bump_sampler: vk::Sampler::null(),
            sets: [vk::DescriptorSet::null(); 2],
            dummy: None,
            bump: None,
            color: None,
            render_pass: vk::RenderPass::null(),
            depth: None,
            framebuffer: vk::Framebuffer::null(),
            copy_material_mode: 0,
        };
        let bindings: Vec<_> = (0..2)
            .map(|binding| {
                vk::DescriptorSetLayoutBinding::default()
                    .binding(binding)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::FRAGMENT)
            })
            .collect();
        let layout_info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
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
            .descriptor_count(4);
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
        if enabled {
            let sampler = sampler
                .address_mode_u(vk::SamplerAddressMode::REPEAT)
                .address_mode_v(vk::SamplerAddressMode::REPEAT)
                .address_mode_w(vk::SamplerAddressMode::REPEAT);
            // SAFETY: The immutable bump map uses supported linear filtering and a single mip level.
            owned.bump_sampler =
                unsafe { owned.device.create_sampler(&sampler, None) }.map_err(|result| {
                    VulkanSmokeRendererError::VulkanOperation {
                        context: "vkCreateSampler(EMBM)",
                        result,
                    }
                })?;
            let rgba8 = fparkan_render::water::legacy_embm_bump_map()
                .into_iter()
                .flat_map(|duv| {
                    [
                        (i16::from(duv[0]) + 128) as u8,
                        (i16::from(duv[1]) + 128) as u8,
                        0,
                        255,
                    ]
                })
                .collect();
            owned.bump = Some(super::create_static_texture_image(
                instance,
                device,
                command_pool,
                &super::VulkanStaticTexture {
                    width: fparkan_render::water::EMBM_BUMP_SIZE,
                    height: fparkan_render::water::EMBM_BUMP_SIZE,
                    rgba8,
                    mip_levels: Vec::new(),
                },
            )?);
        }
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
        depth_request: fparkan_platform::DepthStencilSupport,
    ) -> Result<(), VulkanSmokeRendererError> {
        let side = fparkan_render::water::legacy_embm_reflection_size([extent.0, extent.1]);
        let extent = (side, side);
        let formats = [
            vk::Format::R5G6B5_UNORM_PACK16,
            vk::Format::A1R5G5B5_UNORM_PACK16,
        ];
        let features = formats.map(|format| {
            // SAFETY: This physical device belongs to the instance; the query returns a value copy.
            unsafe {
                instance
                    .instance
                    .get_physical_device_format_properties(device.physical_device(), format)
            }
            .optimal_tiling_features
        });
        let native_format = reflection_copy_format(features);
        if native_format.is_none() {
            eprintln!("render_coverage reflection_16bit_color_unsupported=1 (RGB565 and RGB555 lack texture/filter support; unquantized RGB8 copy fallback)");
        }
        self.copy_material_mode = match native_format {
            Some(vk::Format::R5G6B5_UNORM_PACK16) => 512,
            Some(vk::Format::A1R5G5B5_UNORM_PACK16) => 1024,
            _ => 0,
        };
        // Native's 32-bit path blends RGB8 before GDI copies the finished image into the 16-bit texture.
        let format = vk::Format::R8G8B8A8_UNORM;
        eprintln!(
            "render_reflection color_format={} copy_format={}",
            format.as_raw(),
            native_format.unwrap_or(format).as_raw()
        );
        eprintln!("render_reflection target_extent={side}x{side}");
        let info = vk::ImageCreateInfo::default()
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
            .usage(vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::SAMPLED)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        // SAFETY: The device is live; this single-mip RGB8 target has a nonzero side in 2..=1024.
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
            .format(format)
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
        self.render_pass = super::swapchain_resources::create_render_pass(
            device,
            format.as_raw(),
            self.depth.as_ref().expect("depth exists").format,
        )?;
        let attachments = [view, self.depth.as_ref().expect("depth exists").image.view];
        let info = vk::FramebufferCreateInfo::default()
            .render_pass(self.render_pass)
            .attachments(&attachments)
            .width(extent.0)
            .height(extent.1)
            .layers(1);
        // SAFETY: The target formats and extent match the owned reflection render pass.
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
        let bump = self
            .bump
            .as_ref()
            .or(self.dummy.as_ref())
            .expect("dummy image exists");
        let images = [
            image,
            vk::DescriptorImageInfo::default()
                .sampler(if self.bump.is_some() {
                    self.bump_sampler
                } else {
                    self.sampler
                })
                .image_view(bump.view)
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL),
        ];
        let writes: Vec<_> = images
            .iter()
            .enumerate()
            .map(|(binding, image)| {
                vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(binding as u32)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .image_info(std::slice::from_ref(image))
            })
            .collect();
        // SAFETY: Sets are initialized before submission; the dummy remains valid for every pass.
        unsafe { self.device.update_descriptor_sets(&writes, &[]) };
    }
}

impl Drop for ReflectionResources {
    fn drop(&mut self) {
        // SAFETY: The parent waits for submissions and drops this owner before the render pass/device.
        // Null handles also cover partial construction, including failures before memory binding.
        unsafe {
            self.device.destroy_framebuffer(self.framebuffer, None);
            self.device.destroy_render_pass(self.render_pass, None);
            self.device.destroy_descriptor_pool(self.pool, None);
            self.device.destroy_sampler(self.sampler, None);
            self.device.destroy_sampler(self.bump_sampler, None);
            self.device.destroy_descriptor_set_layout(self.layout, None);
            for image in self
                .color
                .iter()
                .chain(self.depth.iter().map(|depth| &depth.image))
                .chain(self.dummy.iter())
                .chain(self.bump.iter())
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
    fn reflected_land_keeps_native_clockwise_faces_without_culling_water_undersides() {
        use fparkan_render::LegacyCullMode;
        // Actual reflected Land packet and shifted projection; all 984 non-water
        // triangles in the native frame have this clockwise viewport winding.
        let points = [
            [912.06555_f32, 286.18677, 334.75375],
            [920.8868, 295.9891, 329.7298],
            [914.6643, 300.5556, 325.31824],
        ];
        let projected = points.map(|[x, y, z]| {
            let w = z - 115.0;
            [
                (-70.0 / 457.30994 * (y - 965.28656) + (1.0 - 2.0 * 312.45984 / 457.30994) * w) / w,
                (-70.0 / 281.6764 * (x - 1270.1804) + (1.0 - 2.0 * 126.905396 / 281.6764) * w) / w,
            ]
        });
        let [a, b, c] = projected;
        assert!((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]) > 0.0);
        let mut range = VulkanStaticMesh::smoke_triangle().draw_ranges[0];
        range.terrain_surface = true;
        assert_eq!(
            reflected_draw_pipeline_state(&range, true).cull,
            LegacyCullMode::BackFace
        );
        assert_eq!(
            reflected_draw_pipeline_state(&range, false),
            reflected_pipeline_state(range.pipeline_state)
        );
        range.terrain_reflection = true;
        assert_eq!(
            reflected_draw_pipeline_state(&range, true).cull,
            LegacyCullMode::Disabled
        );
        range.terrain_surface = false;
        range.terrain_reflection = false;
        assert_eq!(
            reflected_draw_pipeline_state(&range, true),
            reflected_pipeline_state(range.pipeline_state)
        );
    }

    #[test]
    fn reflection_copy_prefers_filterable_565_then_555_without_attachment_support() {
        let required = vk::FormatFeatureFlags::SAMPLED_IMAGE
            | vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_LINEAR;
        assert_eq!(
            reflection_copy_format([required; 2]),
            Some(vk::Format::R5G6B5_UNORM_PACK16)
        );
        for missing in [
            vk::FormatFeatureFlags::SAMPLED_IMAGE,
            vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_LINEAR,
        ] {
            assert_eq!(
                reflection_copy_format([required & !missing, required]),
                Some(vk::Format::A1R5G5B5_UNORM_PACK16)
            );
            assert_eq!(reflection_copy_format([required & !missing; 2]), None);
        }
    }

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
                owner_scope: 0,
                first_index: 0,
                index_count: 3,
                material_index: 0,
                lightmap_index: 255,
                batch_flags: 0,
                terrain_surface: false,
                terrain_base_only: false,
                terrain_reflection: true,
                pipeline_state: fparkan_render::LegacyPipelineState::default(),
                alpha_test_reference: 1,
            }],
        };
        mesh.vertices[1].position = [10.0, 0.0, 150.0];
        mesh.vertices[2].position = [5.0, 20.0, 150.0];
        let mut unused = mesh.vertices[0];
        unused.position = [-10_000.0, -10_000.0, 150.0];
        mesh.vertices.push(unused);
        assert_eq!(mesh_reflection_bounds(&mesh), Some([0.0, 0.0, 10.0, 20.0]));
        assert_eq!(mesh_reflection_planes(&mesh), vec![Some(150.0)]);
        mesh.vertices[2].position[2] = 151.0;
        assert_eq!(mesh_reflection_planes(&mesh), vec![None]);
        mesh.vertices[2].position[2] = 150.0;
        mesh.draw_ranges[0].terrain_reflection = false;
        assert_eq!(mesh_reflection_planes(&mesh), vec![None]);
        assert_eq!(mesh_reflection_bounds(&mesh), None);
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
