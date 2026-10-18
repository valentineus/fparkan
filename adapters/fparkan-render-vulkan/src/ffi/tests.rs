use super::*;
use crate::ffi::{SPIRV_MAGIC, SPIRV_VERSION_1_0};
use crate::policy::{KHR_PORTABILITY_SUBSET_EXTENSION, KHR_SWAPCHAIN_EXTENSION};
use crate::shader::{validate_spirv_container, validate_triangle_shaders, VulkanShaderError};
use crate::*;
use fparkan_platform::{DepthStencilSupport, RenderRequest};

#[test]
fn device_scoring_is_deterministic_and_prefers_discrete_unified_queue() {
    let devices = vec![
        device("SwiftShader", VulkanDeviceType::Cpu, 0, true, false),
        device("Discrete", VulkanDeviceType::DiscreteGpu, 1, true, false),
        device(
            "Integrated",
            VulkanDeviceType::IntegratedGpu,
            2,
            true,
            false,
        ),
    ];

    let report = select_physical_device(&devices).expect("selected device");

    assert_eq!(report.device_name, "Discrete");
    assert_eq!(report.graphics_queue_family, 1);
    assert_eq!(report.present_queue_family, 1);
    assert!(!report.portability_subset);
    assert_eq!(report.enabled_extensions, vec![KHR_SWAPCHAIN_EXTENSION]);
}

#[test]
fn device_selection_skips_rejected_candidates_before_accepting_valid_gpu() {
    let mut rejected = device("Rejected", VulkanDeviceType::DiscreteGpu, 0, true, false);
    rejected.queue_families[0].present = false;
    let accepted = device("Accepted", VulkanDeviceType::IntegratedGpu, 2, true, false);

    let report = select_physical_device(&[rejected, accepted]).expect("selected fallback device");

    assert_eq!(report.device_name, "Accepted");
    assert_eq!(report.graphics_queue_family, 2);
    assert_eq!(report.present_queue_family, 2);
    assert_eq!(
        report.rejected_devices,
        vec![VulkanRejectedDeviceReport {
            device_name: "Rejected".to_string(),
            reason_code: "no_present_queue",
            reason: "Vulkan device Rejected has no present queue".to_string(),
        }]
    );
}

#[test]
fn queue_family_selection_prefers_lowest_index_unified_family() {
    let mut candidate = device(
        "Unified later in list",
        VulkanDeviceType::DiscreteGpu,
        7,
        true,
        false,
    );
    candidate.queue_families = vec![
        VulkanQueueFamily {
            index: 9,
            graphics: true,
            present: true,
        },
        VulkanQueueFamily {
            index: 3,
            graphics: true,
            present: true,
        },
        VulkanQueueFamily {
            index: 1,
            graphics: true,
            present: false,
        },
    ];

    let report = select_physical_device(&[candidate]).expect("selected unified queue");

    assert_eq!(report.graphics_queue_family, 3);
    assert_eq!(report.present_queue_family, 3);
}

#[test]
fn portability_subset_is_reported_and_enabled_when_exposed() {
    let report = select_physical_device(&[device(
        "MoltenVK",
        VulkanDeviceType::IntegratedGpu,
        0,
        true,
        true,
    )])
    .expect("selected device");

    assert!(report.portability_subset);
    assert_eq!(
        report.enabled_extensions,
        vec![
            KHR_SWAPCHAIN_EXTENSION.to_string(),
            KHR_PORTABILITY_SUBSET_EXTENSION.to_string()
        ]
    );
}

#[test]
fn missing_loader_candidates_are_reported() {
    assert_eq!(
        select_physical_device(&[]),
        Err(VulkanCapabilityError::NoPhysicalDevice)
    );
}

#[test]
fn rejects_low_api_version() {
    let mut candidate = device("Old GPU", VulkanDeviceType::DiscreteGpu, 0, true, false);
    candidate.api_version = vk::API_VERSION_1_0;

    assert!(matches!(
        select_physical_device(&[candidate]),
        Err(VulkanCapabilityError::ApiVersionTooLow { .. })
    ));
}

#[test]
fn rejects_missing_graphics_present_swapchain_and_format() {
    let mut no_graphics = device("No graphics", VulkanDeviceType::DiscreteGpu, 0, true, false);
    no_graphics.queue_families[0].graphics = false;
    assert!(matches!(
        select_physical_device(&[no_graphics]),
        Err(VulkanCapabilityError::NoGraphicsQueue { .. })
    ));

    let mut no_present = device("No present", VulkanDeviceType::DiscreteGpu, 0, true, false);
    no_present.queue_families[0].present = false;
    assert!(matches!(
        select_physical_device(&[no_present]),
        Err(VulkanCapabilityError::NoPresentQueue { .. })
    ));

    let no_swapchain = device(
        "No swapchain",
        VulkanDeviceType::DiscreteGpu,
        0,
        false,
        false,
    );
    assert!(matches!(
        select_physical_device(&[no_swapchain]),
        Err(VulkanCapabilityError::MissingSwapchainExtension { .. })
    ));

    let mut no_format = device("No format", VulkanDeviceType::DiscreteGpu, 0, true, false);
    no_format.surface_formats.clear();
    assert!(matches!(
        select_physical_device(&[no_format]),
        Err(VulkanCapabilityError::MissingSurfaceFormat { .. })
    ));

    let mut no_present_mode = device(
        "No present mode",
        VulkanDeviceType::DiscreteGpu,
        0,
        true,
        false,
    );
    no_present_mode.present_modes.clear();
    assert!(matches!(
        select_physical_device(&[no_present_mode]),
        Err(VulkanCapabilityError::MissingPresentMode { .. })
    ));

    let mut no_color_attachment = device(
        "No color attachment",
        VulkanDeviceType::DiscreteGpu,
        0,
        true,
        false,
    );
    no_color_attachment
        .surface_capabilities
        .supported_usage_flags = vk::ImageUsageFlags::TRANSFER_DST.as_raw();
    assert!(matches!(
        select_physical_device(&[no_color_attachment]),
        Err(VulkanCapabilityError::MissingColorAttachmentUsage { .. })
    ));
}

#[test]
fn capability_gate_rejects_devices_without_requested_depth_stencil_support() {
    let mut no_depth = device("No depth", VulkanDeviceType::DiscreteGpu, 0, true, false);
    no_depth.supported_depth_stencil_formats = vec![vk::Format::D32_SFLOAT.as_raw()];

    assert!(matches!(
        select_physical_device(&[no_depth]),
        Err(VulkanCapabilityError::MissingDepthStencilFormat { .. })
    ));
}

#[test]
fn capability_gate_respects_request_specific_depth_profiles() {
    let mut no_stencil = device("No stencil", VulkanDeviceType::DiscreteGpu, 0, true, false);
    no_stencil.supported_depth_stencil_formats = vec![vk::Format::D32_SFLOAT.as_raw()];
    let relaxed_request = RenderRequest {
        depth: DepthStencilSupport {
            depth_bits: 32,
            stencil_bits: 0,
        },
        ..RenderRequest::conservative()
    };

    let report = select_physical_device_for_request(&[no_stencil], relaxed_request)
        .expect("selected device for depth-only request");

    assert_eq!(report.device_name, "No stencil");
    assert!(report.rejected_devices.is_empty());
}

#[test]
fn depth_attachment_selection_is_canonical_and_request_scoped() {
    let supported = [
        vk::Format::D32_SFLOAT_S8_UINT.as_raw(),
        vk::Format::D24_UNORM_S8_UINT.as_raw(),
        vk::Format::D32_SFLOAT.as_raw(),
    ];
    assert_eq!(
        select_depth_stencil_attachment_format(
            &supported,
            DepthStencilSupport {
                depth_bits: 24,
                stencil_bits: 8,
            },
        ),
        Some(vk::Format::D24_UNORM_S8_UINT.as_raw())
    );
    assert_eq!(
        select_depth_stencil_attachment_format(
            &supported,
            DepthStencilSupport {
                depth_bits: 32,
                stencil_bits: 0,
            },
        ),
        Some(vk::Format::D32_SFLOAT.as_raw())
    );
    assert_eq!(
        select_depth_stencil_attachment_format(
            &supported,
            DepthStencilSupport {
                depth_bits: 0,
                stencil_bits: 0,
            },
        ),
        None
    );
}

#[test]
fn capability_report_preserves_informational_sampled_formats_and_limits() {
    let report = select_physical_device(&[device(
        "Telemetry GPU",
        VulkanDeviceType::DiscreteGpu,
        0,
        true,
        false,
    )])
    .expect("selected device");

    assert_eq!(
        report.informational_capabilities.sampled_color_formats,
        vec![vk::Format::B8G8R8A8_SRGB.as_raw()]
    );
    assert_eq!(
        report.informational_capabilities.sampled_depth_formats,
        vec![vk::Format::D32_SFLOAT.as_raw()]
    );
    assert_eq!(
        report.informational_capabilities.limits,
        VulkanDeviceLimits {
            max_image_dimension_2d: 4096,
            max_sampler_allocation_count: 4096,
            max_per_stage_descriptor_samplers: 16,
            max_bound_descriptor_sets: 4,
        }
    );
}

#[test]
fn loader_error_display_is_actionable() {
    assert_eq!(
        VulkanLoaderError::Unavailable {
            message: "dlopen failed".to_string(),
        }
        .to_string(),
        "Vulkan loader is unavailable: dlopen failed"
    );
}

#[test]
fn instance_plan_is_sorted_deduplicated_and_portability_aware() {
    let plan = plan_vulkan_instance(&VulkanInstanceConfig {
        application_name: "FParkan".to_string(),
        required_extensions: vec![
            "VK_KHR_surface".to_string(),
            KHR_PORTABILITY_ENUMERATION_EXTENSION.to_string(),
            "VK_KHR_surface".to_string(),
        ],
        enable_portability_enumeration: true,
        enable_validation: true,
    });

    assert_eq!(plan.create_flags, 1);
    assert!(plan.validation_requested);
    assert_eq!(
        plan.enabled_extensions,
        vec![
            "VK_EXT_debug_utils".to_string(),
            KHR_PORTABILITY_ENUMERATION_EXTENSION.to_string(),
            "VK_KHR_surface".to_string(),
        ]
    );
}

#[test]
fn instance_plan_adds_portability_extension_when_requested() {
    let plan = plan_vulkan_instance(&VulkanInstanceConfig {
        application_name: "FParkan".to_string(),
        required_extensions: vec!["VK_KHR_surface".to_string()],
        enable_portability_enumeration: true,
        enable_validation: false,
    });

    assert_eq!(
        plan.enabled_extensions,
        vec![
            KHR_PORTABILITY_ENUMERATION_EXTENSION.to_string(),
            "VK_KHR_surface".to_string()
        ]
    );
    assert_eq!(plan.create_flags, 1);
}

#[test]
fn invalid_instance_extension_name_is_reported_before_loader_use() {
    assert_eq!(
        cstring_vec(&["bad\0extension".to_string()]),
        Err(VulkanInstanceError::InvalidExtensionName {
            extension: "bad\0extension".to_string()
        })
    );
}

#[test]
fn missing_instance_extension_is_reported_before_create_instance() {
    assert_eq!(
        ensure_instance_extensions_available(
            &[
                "VK_EXT_debug_utils".to_string(),
                "VK_KHR_surface".to_string(),
            ],
            &["VK_KHR_surface".to_string()],
        ),
        Err(VulkanInstanceError::MissingInstanceExtension {
            extension: "VK_EXT_debug_utils".to_string(),
        })
    );
}

#[test]
fn surface_plan_requires_native_handles() {
    assert_eq!(
        plan_vulkan_surface(None),
        Err(VulkanSurfaceError::MissingNativeHandles)
    );
    assert_eq!(
        VulkanSurfaceError::MissingNativeHandles.to_string(),
        "native window/display handles are required for Vulkan surface creation"
    );
}

#[test]
fn static_surface_extension_name_is_decoded() {
    let name = extension_name(ash::khr::surface::NAME.as_ptr()).expect("extension name");

    assert_eq!(name, "VK_KHR_surface");
}

#[test]
fn swapchain_plan_prefers_unorm_mailbox_and_clamps_extent() {
    let plan = plan_vulkan_swapchain(&swapchain_request()).expect("swapchain plan");

    assert_eq!(
        plan.format,
        VulkanSurfaceFormat {
            format: vk::Format::R8G8B8A8_UNORM.as_raw(),
            color_space: vk::ColorSpaceKHR::SRGB_NONLINEAR.as_raw(),
        }
    );
    assert_eq!(plan.present_mode, vk::PresentModeKHR::MAILBOX.as_raw());
    assert_eq!(plan.extent, (1024, 720));
    assert_eq!(plan.image_count, 3);
    assert_eq!(
        plan.image_usage,
        vk::ImageUsageFlags::COLOR_ATTACHMENT.as_raw()
    );
}

#[test]
fn swapchain_plan_enables_transfer_source_when_surface_supports_it() {
    let mut request = swapchain_request();
    request.capabilities.supported_usage_flags |= vk::ImageUsageFlags::TRANSFER_SRC.as_raw();

    let plan = plan_vulkan_swapchain(&request).expect("swapchain plan");

    assert_eq!(
        plan.image_usage,
        (vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC).as_raw()
    );
}

#[test]
fn swapchain_plan_uses_fifo_and_current_extent_fallbacks() {
    let mut request = swapchain_request();
    request.preferred_present_mode = vk::PresentModeKHR::IMMEDIATE.as_raw();
    request.present_modes = vec![vk::PresentModeKHR::FIFO.as_raw()];
    request.capabilities.current_extent = Some((800, 600));

    let plan = plan_vulkan_swapchain(&request).expect("swapchain plan");

    assert_eq!(plan.present_mode, vk::PresentModeKHR::FIFO.as_raw());
    assert_eq!(plan.extent, (800, 600));
}

#[test]
fn swapchain_plan_accepts_undefined_surface_format_by_picking_stage0_default() {
    let mut request = swapchain_request();
    request.formats = vec![VulkanSurfaceFormat {
        format: vk::Format::UNDEFINED.as_raw(),
        color_space: vk::ColorSpaceKHR::SRGB_NONLINEAR.as_raw(),
    }];

    let plan = plan_vulkan_swapchain(&request).expect("swapchain plan");

    assert_eq!(
        plan.format,
        VulkanSurfaceFormat {
            format: vk::Format::R8G8B8A8_UNORM.as_raw(),
            color_space: vk::ColorSpaceKHR::SRGB_NONLINEAR.as_raw(),
        }
    );
}

#[test]
fn swapchain_plan_rejects_missing_surface_data_and_empty_extent() {
    let mut request = swapchain_request();
    request.formats.clear();
    assert_eq!(
        plan_vulkan_swapchain(&request),
        Err(VulkanSwapchainError::MissingSurfaceFormat)
    );

    let mut request = swapchain_request();
    request.present_modes.clear();
    assert_eq!(
        plan_vulkan_swapchain(&request),
        Err(VulkanSwapchainError::MissingPresentMode)
    );

    let mut request = swapchain_request();
    request.capabilities.current_extent = Some((0, 600));
    assert_eq!(
        plan_vulkan_swapchain(&request),
        Err(VulkanSwapchainError::EmptyExtent)
    );
}

#[test]
fn checked_in_shaders_have_valid_spirv_containers() {
    assert_eq!(validate_triangle_shaders(), Ok(()));
}

#[test]
fn fragment_shader_dispatches_all_four_material_modes() {
    let fragment = include_str!("../../shaders/triangle.frag");
    assert!(fragment.contains("int mode = int(round(material.material_mode));"));
    assert!(fragment.contains("mode == 0"));
    assert!(fragment.contains("mode == 1"));
    assert!(fragment.contains("mode == 2"));
    assert!(fragment.contains("mode == 3"));
    assert!(!fragment.contains("material_mode > 1.5"));
    assert!(!fragment.contains("material_mode > 2.5"));
}

#[test]
fn point_light_storage_payload_is_counted_and_packed_as_three_vec4s() {
    let lights = [VulkanPointLight {
        position: [1.0, 2.0, 3.0],
        rgb: [4.0, 5.0, 6.0],
        range: 7.0,
        coefficients: [8.0, 9.0, 10.0],
        active: true,
    }];
    let bytes = super::resources::encode_point_lights(&lights).expect("point-light payload");
    assert_eq!(
        bytes.len(),
        VULKAN_POINT_LIGHT_HEADER_BYTES + VULKAN_POINT_LIGHT_RECORD_BYTES
    );
    assert_eq!(u32::from_ne_bytes(bytes[0..4].try_into().unwrap()), 1);
    let values = (0..12)
        .map(|index| {
            let offset = VULKAN_POINT_LIGHT_HEADER_BYTES + index * std::mem::size_of::<f32>();
            f32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap())
        })
        .collect::<Vec<_>>();
    assert_eq!(
        values,
        vec![1.0, 2.0, 3.0, 7.0, 4.0, 5.0, 6.0, 1.0, 8.0, 9.0, 10.0, 1.0]
    );

    let many = vec![VulkanPointLight::default(); 33];
    let bytes = super::resources::encode_point_lights(&many).expect("uncapped point-light payload");
    assert_eq!(u32::from_ne_bytes(bytes[0..4].try_into().unwrap()), 33);
    assert_eq!(
        bytes.len(),
        VULKAN_POINT_LIGHT_HEADER_BYTES + 33 * VULKAN_POINT_LIGHT_RECORD_BYTES
    );
}

#[test]
fn vertex_shader_sums_the_runtime_point_light_array() {
    let vertex = include_str!("../../shaders/triangle.vert");
    assert!(
        vertex.contains("layout(std430, set = 0, binding = 6) readonly buffer PointLightBuffer")
    );
    assert!(vertex.contains("for (uint index = 0u; index < point_light_buffer.header.x; ++index)"));
    assert!(!vertex.contains("if (point_rgb_active.w"));
}

#[test]
fn native_specular_reference_uses_squared_power_and_total_overflow_curve() {
    fn reference_power(cosine: f32, power: u8) -> f32 {
        if power == 0 {
            return 0.0;
        }
        let mut result = cosine;
        for _ in 0..power.saturating_sub(1) {
            result *= result;
        }
        result
    }
    fn reference_curve(value: f32) -> f32 {
        if value <= 1.0 {
            0.8 * value
        } else if value <= 3.0 {
            0.1 * value + 0.7
        } else {
            1.0
        }
    }

    assert_eq!(reference_power(0.5, 0), 0.0);
    assert!((reference_power(0.5, 1) - 0.5).abs() < 1e-6);
    assert!((reference_power(0.5, 2) - 0.25).abs() < 1e-6);
    assert!((reference_power(0.5, 3) - 0.0625).abs() < 1e-6);
    let compressed_diffuse = 1.5;
    let material_specular = 0.75;
    assert!((reference_curve(material_specular + (compressed_diffuse - 1.0)) - 0.825).abs() < 1e-6);

    let vertex = include_str!("../../shaders/triangle.vert");
    assert!(vertex.contains("result *= result;"));
    assert!(
        vertex.contains("vec3 total_specular = specular + max(compressed - vec3(1.0), vec3(0.0));")
    );
    assert!(vertex.contains("if (power <= 0.0 || cosine <= 0.0)"));
}

#[test]
fn shader_computes_radial_fog_per_vertex_and_interpolates_it() {
    let vertex = include_str!("../../shaders/triangle.vert");
    let fragment = include_str!("../../shaders/triangle.frag");
    assert!(vertex.contains("out_fog_factor = screen_tl ? 0.0 : native_fog_factor(in_position);"));
    assert!(fragment.contains("layout(location = 4) in float in_fog_factor;"));
    assert!(!fragment.contains("distance(in_world_position"));
    assert!(!fragment.contains("float fog_factor()"));
}

#[test]
fn shader_container_rejects_invalid_spirv() {
    assert_eq!(
        validate_spirv_container("triangle.vert", &[0xFFFF_FFFF, SPIRV_VERSION_1_0, 0, 1, 0]),
        Err(VulkanShaderError::InvalidMagic {
            name: "triangle.vert",
            found: 0xFFFF_FFFF,
        })
    );

    assert_eq!(
        validate_spirv_container("triangle.vert", &[SPIRV_MAGIC, 0, 0, 1, 0]),
        Err(VulkanShaderError::UnsupportedVersion {
            name: "triangle.vert",
            found: 0,
        })
    );

    assert_eq!(
        validate_spirv_container("triangle.vert", &[SPIRV_MAGIC, SPIRV_VERSION_1_0, 0, 0, 0]),
        Err(VulkanShaderError::InvalidBound {
            name: "triangle.vert",
        })
    );
}

fn device(
    name: &str,
    device_type: VulkanDeviceType,
    queue_index: u32,
    swapchain: bool,
    portability_subset: bool,
) -> VulkanPhysicalDeviceRecord {
    let mut extensions = Vec::new();
    if swapchain {
        extensions.push(KHR_SWAPCHAIN_EXTENSION.to_string());
    }
    if portability_subset {
        extensions.push(KHR_PORTABILITY_SUBSET_EXTENSION.to_string());
    }
    VulkanPhysicalDeviceRecord {
        name: name.to_string(),
        api_version: MIN_VULKAN_API_VERSION,
        device_type,
        extensions,
        queue_families: vec![VulkanQueueFamily {
            index: queue_index,
            graphics: true,
            present: true,
        }],
        surface_formats: vec![VulkanSurfaceFormat {
            format: vk::Format::B8G8R8A8_SRGB.as_raw(),
            color_space: vk::ColorSpaceKHR::SRGB_NONLINEAR.as_raw(),
        }],
        present_modes: vec![
            vk::PresentModeKHR::FIFO.as_raw(),
            vk::PresentModeKHR::MAILBOX.as_raw(),
        ],
        surface_capabilities: default_surface_capabilities(),
        supported_depth_stencil_formats: vec![
            vk::Format::D24_UNORM_S8_UINT.as_raw(),
            vk::Format::D32_SFLOAT_S8_UINT.as_raw(),
            vk::Format::D32_SFLOAT.as_raw(),
        ],
        sampled_image_formats: vec![
            vk::Format::B8G8R8A8_SRGB.as_raw(),
            vk::Format::D32_SFLOAT.as_raw(),
        ],
        limits: VulkanDeviceLimits {
            max_image_dimension_2d: 4096,
            max_sampler_allocation_count: 4096,
            max_per_stage_descriptor_samplers: 16,
            max_bound_descriptor_sets: 4,
        },
    }
}

fn swapchain_request() -> VulkanSwapchainRequest {
    VulkanSwapchainRequest {
        drawable_extent: (1280, 720),
        formats: vec![
            VulkanSurfaceFormat {
                format: vk::Format::R8G8B8A8_UNORM.as_raw(),
                color_space: vk::ColorSpaceKHR::SRGB_NONLINEAR.as_raw(),
            },
            VulkanSurfaceFormat {
                format: vk::Format::B8G8R8A8_SRGB.as_raw(),
                color_space: vk::ColorSpaceKHR::SRGB_NONLINEAR.as_raw(),
            },
        ],
        present_modes: vec![
            vk::PresentModeKHR::FIFO.as_raw(),
            vk::PresentModeKHR::MAILBOX.as_raw(),
        ],
        capabilities: default_surface_capabilities(),
        preferred_present_mode: vk::PresentModeKHR::MAILBOX.as_raw(),
    }
}

fn default_surface_capabilities() -> VulkanSwapchainSurfaceCapabilities {
    VulkanSwapchainSurfaceCapabilities {
        current_extent: None,
        min_extent: (320, 240),
        max_extent: (1024, 768),
        min_image_count: 2,
        max_image_count: 3,
        supported_usage_flags: vk::ImageUsageFlags::COLOR_ATTACHMENT.as_raw(),
    }
}
