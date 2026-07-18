use super::*;
use crate::policy::{KHR_PORTABILITY_SUBSET_EXTENSION, KHR_SWAPCHAIN_EXTENSION};
use crate::shader_manifest::{
    SHADER_COMPILER_BINARY_SHA256, SHADER_COMPILER_NAME, SHADER_COMPILER_VERSION,
    SHADER_MANIFEST_SCHEMA, SHADER_TARGET_ENV, SPIRV_MAGIC, SPIRV_VALIDATOR_BINARY_SHA256,
    SPIRV_VALIDATOR_NAME, SPIRV_VALIDATOR_VERSION, SPIRV_VERSION_1_0,
    TRIANGLE_VERTEX_COMPILE_COMMAND, TRIANGLE_VERTEX_SOURCE_PATH, TRIANGLE_VERTEX_SOURCE_SHA256,
    TRIANGLE_VERTEX_SPIRV_PATH, TRIANGLE_VERTEX_VALIDATE_COMMAND,
};
use crate::*;
use fparkan_platform::{DepthStencilSupport, RenderRequest};
use fparkan_render::{
    DrawCommand, DrawId, GpuMaterialId, GpuMeshId, IndexRange, RenderCommand, RenderPhase,
};
use fparkan_render::{RenderBackend, RenderError};

#[test]
fn planning_backend_tracks_render_request_and_simulated_present() -> Result<(), RenderError> {
    let mut backend = VulkanPlanningBackend::new();
    let request = RenderRequest {
        presentation: fparkan_platform::PresentationMode::Immediate,
        ..RenderRequest::conservative()
    };
    backend.set_render_request(request);
    assert_eq!(backend.render_request(), request);
    assert_eq!(backend.report().request.current_request, request);
    assert_eq!(backend.report().request.request_updates, 1);

    let commands = fparkan_render::RenderCommandList {
        commands: vec![
            RenderCommand::BeginFrame,
            RenderCommand::Draw(DrawCommand {
                id: DrawId(11),
                phase: RenderPhase::Opaque,
                object_id: None,
                mesh: GpuMeshId(1),
                material: GpuMaterialId(2),
                transform: [1.0; 16],
                range: IndexRange { start: 0, count: 3 },
                stable_order: 7,
            }),
            RenderCommand::EndFrame,
        ],
    };

    backend.execute(&commands)?;
    assert_eq!(backend.state(), VulkanPlanningBackendState::Configured);
    assert_eq!(backend.report().execution.planned_frames, 1);
    assert_eq!(backend.report().execution.submission_plans, 1);
    assert_eq!(backend.report().execution.simulated_presents, 1);
    assert!(backend.report().execution.last_capture_size > 0);
    assert_eq!(
        backend.report().last_frame_submission,
        Some(VulkanFrameSubmissionPlan {
            schema: 1,
            frames_in_flight: 2,
            command_buffers: 2,
            semaphores_per_frame: 2,
            fences_per_frame: 1,
            draw_count: 1,
            indexed_vertex_count: 3,
        })
    );
    Ok(())
}

#[test]
fn frame_submission_plan_json_is_stable() -> Result<(), RenderError> {
    let commands = fparkan_render::RenderCommandList {
        commands: vec![
            RenderCommand::BeginFrame,
            RenderCommand::Draw(DrawCommand {
                id: DrawId(11),
                phase: RenderPhase::Opaque,
                object_id: None,
                mesh: GpuMeshId(1),
                material: GpuMaterialId(2),
                transform: [1.0; 16],
                range: IndexRange { start: 0, count: 3 },
                stable_order: 7,
            }),
            RenderCommand::EndFrame,
        ],
    };
    let swapchain = VulkanSwapchainPlan {
        schema: 1,
        extent: (1, 1),
        format: VulkanSurfaceFormat {
            format: vk::Format::B8G8R8A8_SRGB.as_raw(),
            color_space: vk::ColorSpaceKHR::SRGB_NONLINEAR.as_raw(),
        },
        present_mode: vk::PresentModeKHR::FIFO.as_raw(),
        image_count: 3,
    };

    let plan = plan_vulkan_frame_submission(&swapchain, &commands)?;

    assert_eq!(plan.frames_in_flight, 2);
    assert_eq!(plan.command_buffers, 3);
    assert_eq!(plan.draw_count, 1);
    assert_eq!(plan.indexed_vertex_count, 3);
    assert_eq!(
        render_frame_submission_plan_json(&plan),
        "{\"schema\":1,\"frames_in_flight\":2,\"command_buffers\":3,\"semaphores_per_frame\":2,\"fences_per_frame\":1,\"draw_count\":1,\"indexed_vertex_count\":3}"
    );
    Ok(())
}

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
fn capability_report_json_is_stable() {
    let mut rejected = device("Rejected", VulkanDeviceType::IntegratedGpu, 0, true, false);
    rejected.present_modes.clear();
    let report = select_physical_device(&[
        rejected,
        device("GPU \"A\"", VulkanDeviceType::DiscreteGpu, 3, true, false),
    ])
    .expect("selected device");

    assert_eq!(
        render_capability_report_json(&report),
        "{\"schema\":1,\"vulkan_api\":\"1.1.0\",\"device_name\":\"GPU \\\"A\\\"\",\"score\":1101,\"graphics_queue_family\":3,\"present_queue_family\":3,\"portability_subset\":false,\"enabled_extensions\":[\"VK_KHR_swapchain\"],\"informational_capabilities\":{\"sampled_color_formats\":[50],\"sampled_depth_formats\":[126],\"limits\":{\"max_image_dimension_2d\":4096,\"max_sampler_allocation_count\":4096,\"max_per_stage_descriptor_samplers\":16,\"max_bound_descriptor_sets\":4}},\"rejected_devices\":[{\"device_name\":\"Rejected\",\"reason_code\":\"missing_present_mode\",\"reason\":\"Vulkan device Rejected has no supported present mode\"}]}"
    );
}

#[test]
fn loader_probe_report_json_is_stable() {
    assert_eq!(
        vulkan_entry_symbol_name().to_bytes(),
        b"vkGetInstanceProcAddr"
    );
    assert_eq!(
        render_loader_probe_report_json(&VulkanLoaderProbeReport {
            schema: 1,
            loader_available: true,
            instance_api_version: vk::API_VERSION_1_2,
        }),
        "{\"schema\":1,\"loader_available\":true,\"instance_api\":\"1.2.0\"}"
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

    assert_eq!(
        render_instance_plan_json(&plan),
        "{\"schema\":1,\"create_flags\":1,\"validation_requested\":true,\"enabled_extensions\":[\"VK_EXT_debug_utils\",\"VK_KHR_portability_enumeration\",\"VK_KHR_surface\"]}"
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
fn surface_plan_json_is_stable() {
    assert_eq!(
        render_surface_plan_json(&VulkanSurfacePlan {
            schema: 1,
            required_instance_extensions: vec![
                "VK_KHR_surface".to_string(),
                "VK_EXT_metal_surface".to_string(),
            ],
        }),
        "{\"schema\":1,\"required_instance_extensions\":[\"VK_KHR_surface\",\"VK_EXT_metal_surface\"]}"
    );
}

#[test]
fn static_surface_extension_name_is_decoded() {
    let name = extension_name(ash::khr::surface::NAME.as_ptr()).expect("extension name");

    assert_eq!(name, "VK_KHR_surface");
}

#[test]
fn swapchain_plan_prefers_srgb_mailbox_and_clamps_extent() {
    let plan = plan_vulkan_swapchain(&swapchain_request()).expect("swapchain plan");

    assert_eq!(
        plan.format,
        VulkanSurfaceFormat {
            format: vk::Format::B8G8R8A8_SRGB.as_raw(),
            color_space: vk::ColorSpaceKHR::SRGB_NONLINEAR.as_raw(),
        }
    );
    assert_eq!(plan.present_mode, vk::PresentModeKHR::MAILBOX.as_raw());
    assert_eq!(plan.extent, (1024, 720));
    assert_eq!(plan.image_count, 3);
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
            format: vk::Format::B8G8R8A8_SRGB.as_raw(),
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
fn swapchain_plan_json_and_recreation_reports_are_stable() {
    let plan = plan_vulkan_swapchain(&swapchain_request()).expect("swapchain plan");
    assert_eq!(
        render_swapchain_plan_json(&plan),
        "{\"schema\":1,\"extent\":[1024,720],\"format\":50,\"color_space\":0,\"present_mode\":1,\"image_count\":3}"
    );

    let report = swapchain_recreation_report(
        VulkanSwapchainRecreationReason::OutOfDate,
        (1024, 720),
        (1280, 720),
    );
    assert_eq!(
        render_swapchain_recreation_report_json(&report),
        "{\"schema\":1,\"reason\":\"out_of_date\",\"previous_extent\":[1024,720],\"next_extent\":[1280,720]}"
    );
}

#[test]
fn triangle_shader_manifest_hashes_are_stable() {
    let report = validate_shader_manifest(&triangle_shader_manifest()).expect("shader manifest");

    assert_eq!(report.schema, SHADER_MANIFEST_SCHEMA);
    assert_eq!(report.target_env, SHADER_TARGET_ENV);
    assert_eq!(
        report.compiler,
        VulkanShaderToolManifest {
            name: SHADER_COMPILER_NAME,
            version: SHADER_COMPILER_VERSION,
            binary_sha256: SHADER_COMPILER_BINARY_SHA256,
        }
    );
    assert_eq!(
        report.validator,
        VulkanShaderToolManifest {
            name: SPIRV_VALIDATOR_NAME,
            version: SPIRV_VALIDATOR_VERSION,
            binary_sha256: SPIRV_VALIDATOR_BINARY_SHA256,
        }
    );
    assert_eq!(report.modules.len(), 2);
    assert_eq!(report.modules[0].name, "triangle.vert");
    assert_eq!(report.modules[0].stage, VulkanShaderStage::Vertex);
    assert_eq!(report.modules[0].source_path, TRIANGLE_VERTEX_SOURCE_PATH);
    assert_eq!(
        report.modules[0].source_sha256,
        TRIANGLE_VERTEX_SOURCE_SHA256
    );
    assert_eq!(report.modules[0].spirv_path, TRIANGLE_VERTEX_SPIRV_PATH);
    assert_eq!(report.modules[0].word_count, 290);
    assert_eq!(
        report.modules[0].sha256,
        "1984662f78873b70135ec444ccd86d86fa66dfca3f615d19ddaf0cda1587d4cf"
    );
    assert_eq!(report.modules[0].descriptor_sets, 0);
    assert_eq!(report.modules[0].push_constant_bytes, 0);
    assert_eq!(
        report.modules[0].compile_command,
        TRIANGLE_VERTEX_COMPILE_COMMAND
    );
    assert_eq!(
        report.modules[0].validate_command,
        TRIANGLE_VERTEX_VALIDATE_COMMAND
    );
    assert!(!report.modules[0].interface_hash.is_empty());
    assert_eq!(
        report.modules[1].sha256,
        "49dae3e1c46d5d23cccf3b161c36ea0b3a606e89c2289dbfed3e4fe991eb8556"
    );
    assert_eq!(
        report.manifest_hash,
        "038ecdb57832ac2d45a1ca6da5ec058b34f4f31b7170aa68fa612ac9d0ae7565"
    );
}

#[test]
fn shader_manifest_report_json_is_stable() {
    let report = validate_shader_manifest(&triangle_shader_manifest()).expect("shader manifest");
    let json = render_shader_manifest_report_json(&report);

    assert!(json.contains(SHADER_COMPILER_NAME));
    assert!(json.contains(SPIRV_VALIDATOR_NAME));
    assert!(json.contains(TRIANGLE_VERTEX_SOURCE_PATH));
    assert!(json.contains(TRIANGLE_VERTEX_COMPILE_COMMAND));
}

#[test]
fn checked_in_shader_manifest_matches_generated_report() {
    let report = validate_shader_manifest(&triangle_shader_manifest()).expect("shader manifest");
    assert_eq!(
        render_shader_manifest_report_json(&report),
        include_str!("../../shaders/manifest.json").trim()
    );
}

#[test]
fn shader_manifest_rejects_invalid_spirv_containers() {
    let mut module = triangle_shader_manifest().remove(0);
    module.words = &[0xFFFF_FFFF, SPIRV_VERSION_1_0, 0, 1, 0];
    assert_eq!(
        validate_shader_manifest(&[module]),
        Err(VulkanShaderManifestError::InvalidMagic {
            name: "triangle.vert",
            found: 0xFFFF_FFFF,
        })
    );

    let mut module = triangle_shader_manifest().remove(0);
    module.words = &[SPIRV_MAGIC, 0, 0, 1, 0];
    assert_eq!(
        validate_shader_manifest(&[module]),
        Err(VulkanShaderManifestError::UnsupportedVersion {
            name: "triangle.vert",
            found: 0,
        })
    );

    let mut module = triangle_shader_manifest().remove(0);
    module.words = &[SPIRV_MAGIC, SPIRV_VERSION_1_0, 0, 0, 0];
    assert_eq!(
        validate_shader_manifest(&[module]),
        Err(VulkanShaderManifestError::InvalidBound {
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
