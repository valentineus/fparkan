#![allow(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        clippy::cast_precision_loss,
        clippy::expect_used,
        clippy::float_cmp,
        clippy::identity_op,
        clippy::too_many_lines,
        clippy::uninlined_format_args,
        clippy::map_unwrap_or,
        clippy::needless_raw_string_hashes,
        clippy::semicolon_if_nothing_returned,
        clippy::type_complexity,
        clippy::panic,
        clippy::unwrap_used
    )
)]
#![deny(unsafe_op_in_unsafe_fn)]
//! Vulkan adapter facade and migration-ready backend surface contract.
//!
//! This module intentionally keeps backend-agnostic command validation in the
//! shared render crate while exposing deterministic lifecycle telemetry used by
//! Stage 0 acceptance evidence.
//!
//! This crate is the declared low-level Vulkan boundary.

mod instance;
mod resources;
mod runtime;
mod surface;
mod validation;

pub use self::instance::{
    create_vulkan_instance_probe, plan_vulkan_instance, probe_vulkan_loader,
    render_instance_plan_json, render_loader_probe_report_json, vulkan_entry_symbol_name,
    VulkanInstanceConfig, VulkanInstanceError, VulkanInstancePlan, VulkanInstanceProbe,
    VulkanLoaderError, VulkanLoaderProbeReport,
};
#[cfg(test)]
use self::instance::{cstring_vec, ensure_instance_extensions_available};
use self::resources::{
    color_subresource_range, create_command_pool, create_frame_sync, create_swapchain_resources,
    create_triangle_index_buffer, create_triangle_vertex_buffer, destroy_allocated_buffer,
    destroy_swapchain_resources, VulkanAllocatedBuffer, VulkanFrameSync, VulkanSwapchainResources,
};
pub use self::runtime::{
    create_vulkan_logical_device_probe, create_vulkan_swapchain_probe,
    create_vulkan_swapchain_probe_for_extent, probe_vulkan_runtime_capabilities,
    VulkanLogicalDeviceError, VulkanLogicalDeviceProbe, VulkanLogicalDeviceReport,
    VulkanRuntimeCapabilityError, VulkanRuntimeCapabilityProbe, VulkanSwapchainProbe,
    VulkanSwapchainProbeError, VulkanSwapchainReport,
};
#[cfg(test)]
use self::surface::extension_name;
pub use self::surface::{
    create_vulkan_surface_probe, plan_vulkan_surface, render_surface_plan_json, VulkanSurfaceError,
    VulkanSurfacePlan, VulkanSurfaceProbe,
};
use self::validation::{create_validation_messenger, VulkanValidationMessenger};
use crate::shader_manifest::{
    triangle_shader_manifest, validate_shader_manifest, VulkanShaderManifestError,
};
use ash::vk;
use fparkan_platform::NativeWindowHandles;
/// Minimum Vulkan API version accepted by the Stage 0 backend.
pub const MIN_VULKAN_API_VERSION: u32 = vk::API_VERSION_1_1;
const KHR_PORTABILITY_ENUMERATION_EXTENSION: &str = "VK_KHR_portability_enumeration";
const EXT_DEBUG_UTILS_EXTENSION: &str = "VK_EXT_debug_utils";
const VALIDATION_LAYER_NAME: &str = "VK_LAYER_KHRONOS_validation";
pub(crate) const SPIRV_MAGIC: u32 = 0x0723_0203;
pub(crate) const SPIRV_VERSION_1_0: u32 = 0x0001_0000;
pub(crate) const TRIANGLE_VERTEX_SHADER_WORDS: &[u32] = &[
    SPIRV_MAGIC,
    SPIRV_VERSION_1_0,
    0x0008_000b,
    0x0000_0021,
    0x0000_0000,
    0x0002_0011,
    0x0000_0001,
    0x0006_000b,
    0x0000_0001,
    0x4c53_4c47,
    0x6474_732e,
    0x3035_342e,
    0x0000_0000,
    0x0003_000e,
    0x0000_0000,
    0x0000_0001,
    0x0009_000f,
    0x0000_0000,
    0x0000_0004,
    0x6e69_616d,
    0x0000_0000,
    0x0000_0009,
    0x0000_000b,
    0x0000_0013,
    0x0000_0018,
    0x0003_0003,
    0x0000_0002,
    0x0000_01c2,
    0x0004_0005,
    0x0000_0004,
    0x6e69_616d,
    0x0000_0000,
    0x0005_0005,
    0x0000_0009,
    0x5f74_756f,
    0x6f6c_6f63,
    0x0000_0072,
    0x0005_0005,
    0x0000_000b,
    0x635f_6e69,
    0x726f_6c6f,
    0x0000_0000,
    0x0006_0005,
    0x0000_0011,
    0x505f_6c67,
    0x6556_7265,
    0x7865_7472,
    0x0000_0000,
    0x0006_0006,
    0x0000_0011,
    0x0000_0000,
    0x505f_6c67,
    0x7469_736f,
    0x006e_6f69,
    0x0007_0006,
    0x0000_0011,
    0x0000_0001,
    0x505f_6c67,
    0x746e_696f,
    0x657a_6953,
    0x0000_0000,
    0x0007_0006,
    0x0000_0011,
    0x0000_0002,
    0x435f_6c67,
    0x4470_696c,
    0x6174_7369,
    0x0065_636e,
    0x0007_0006,
    0x0000_0011,
    0x0000_0003,
    0x435f_6c67,
    0x446c_6c75,
    0x6174_7369,
    0x0065_636e,
    0x0003_0005,
    0x0000_0013,
    0x0000_0000,
    0x0005_0005,
    0x0000_0018,
    0x705f_6e69,
    0x7469_736f,
    0x006e_6f69,
    0x0004_0047,
    0x0000_0009,
    0x0000_001e,
    0x0000_0000,
    0x0004_0047,
    0x0000_000b,
    0x0000_001e,
    0x0000_0001,
    0x0003_0047,
    0x0000_0011,
    0x0000_0002,
    0x0005_0048,
    0x0000_0011,
    0x0000_0000,
    0x0000_000b,
    0x0000_0000,
    0x0005_0048,
    0x0000_0011,
    0x0000_0001,
    0x0000_000b,
    0x0000_0001,
    0x0005_0048,
    0x0000_0011,
    0x0000_0002,
    0x0000_000b,
    0x0000_0003,
    0x0005_0048,
    0x0000_0011,
    0x0000_0003,
    0x0000_000b,
    0x0000_0004,
    0x0004_0047,
    0x0000_0018,
    0x0000_001e,
    0x0000_0000,
    0x0002_0013,
    0x0000_0002,
    0x0003_0021,
    0x0000_0003,
    0x0000_0002,
    0x0003_0016,
    0x0000_0006,
    0x0000_0020,
    0x0004_0017,
    0x0000_0007,
    0x0000_0006,
    0x0000_0003,
    0x0004_0020,
    0x0000_0008,
    0x0000_0003,
    0x0000_0007,
    0x0004_003b,
    0x0000_0008,
    0x0000_0009,
    0x0000_0003,
    0x0004_0020,
    0x0000_000a,
    0x0000_0001,
    0x0000_0007,
    0x0004_003b,
    0x0000_000a,
    0x0000_000b,
    0x0000_0001,
    0x0004_0017,
    0x0000_000d,
    0x0000_0006,
    0x0000_0004,
    0x0004_0015,
    0x0000_000e,
    0x0000_0020,
    0x0000_0000,
    0x0004_002b,
    0x0000_000e,
    0x0000_000f,
    0x0000_0001,
    0x0004_001c,
    0x0000_0010,
    0x0000_0006,
    0x0000_000f,
    0x0006_001e,
    0x0000_0011,
    0x0000_000d,
    0x0000_0006,
    0x0000_0010,
    0x0000_0010,
    0x0004_0020,
    0x0000_0012,
    0x0000_0003,
    0x0000_0011,
    0x0004_003b,
    0x0000_0012,
    0x0000_0013,
    0x0000_0003,
    0x0004_0015,
    0x0000_0014,
    0x0000_0020,
    0x0000_0001,
    0x0004_002b,
    0x0000_0014,
    0x0000_0015,
    0x0000_0000,
    0x0004_0017,
    0x0000_0016,
    0x0000_0006,
    0x0000_0002,
    0x0004_0020,
    0x0000_0017,
    0x0000_0001,
    0x0000_0016,
    0x0004_003b,
    0x0000_0017,
    0x0000_0018,
    0x0000_0001,
    0x0004_002b,
    0x0000_0006,
    0x0000_001a,
    0x0000_0000,
    0x0004_002b,
    0x0000_0006,
    0x0000_001b,
    0x3f80_0000,
    0x0004_0020,
    0x0000_001f,
    0x0000_0003,
    0x0000_000d,
    0x0005_0036,
    0x0000_0002,
    0x0000_0004,
    0x0000_0000,
    0x0000_0003,
    0x0002_00f8,
    0x0000_0005,
    0x0004_003d,
    0x0000_0007,
    0x0000_000c,
    0x0000_000b,
    0x0003_003e,
    0x0000_0009,
    0x0000_000c,
    0x0004_003d,
    0x0000_0016,
    0x0000_0019,
    0x0000_0018,
    0x0005_0051,
    0x0000_0006,
    0x0000_001c,
    0x0000_0019,
    0x0000_0000,
    0x0005_0051,
    0x0000_0006,
    0x0000_001d,
    0x0000_0019,
    0x0000_0001,
    0x0007_0050,
    0x0000_000d,
    0x0000_001e,
    0x0000_001c,
    0x0000_001d,
    0x0000_001a,
    0x0000_001b,
    0x0005_0041,
    0x0000_001f,
    0x0000_0020,
    0x0000_0013,
    0x0000_0015,
    0x0003_003e,
    0x0000_0020,
    0x0000_001e,
    0x0001_00fd,
    0x0001_0038,
];
pub(crate) const TRIANGLE_FRAGMENT_SHADER_WORDS: &[u32] = &[
    SPIRV_MAGIC,
    SPIRV_VERSION_1_0,
    0x0008_000b,
    0x0000_0013,
    0x0000_0000,
    0x0002_0011,
    0x0000_0001,
    0x0006_000b,
    0x0000_0001,
    0x4c53_4c47,
    0x6474_732e,
    0x3035_342e,
    0x0000_0000,
    0x0003_000e,
    0x0000_0000,
    0x0000_0001,
    0x0007_000f,
    0x0000_0004,
    0x0000_0004,
    0x6e69_616d,
    0x0000_0000,
    0x0000_0009,
    0x0000_000c,
    0x0003_0010,
    0x0000_0004,
    0x0000_0007,
    0x0003_0003,
    0x0000_0002,
    0x0000_01c2,
    0x0004_0005,
    0x0000_0004,
    0x6e69_616d,
    0x0000_0000,
    0x0005_0005,
    0x0000_0009,
    0x5f74_756f,
    0x6f6c_6f63,
    0x0000_0072,
    0x0005_0005,
    0x0000_000c,
    0x635f_6e69,
    0x726f_6c6f,
    0x0000_0000,
    0x0004_0047,
    0x0000_0009,
    0x0000_001e,
    0x0000_0000,
    0x0004_0047,
    0x0000_000c,
    0x0000_001e,
    0x0000_0000,
    0x0002_0013,
    0x0000_0002,
    0x0003_0021,
    0x0000_0003,
    0x0000_0002,
    0x0003_0016,
    0x0000_0006,
    0x0000_0020,
    0x0004_0017,
    0x0000_0007,
    0x0000_0006,
    0x0000_0004,
    0x0004_0020,
    0x0000_0008,
    0x0000_0003,
    0x0000_0007,
    0x0004_003b,
    0x0000_0008,
    0x0000_0009,
    0x0000_0003,
    0x0004_0017,
    0x0000_000a,
    0x0000_0006,
    0x0000_0003,
    0x0004_0020,
    0x0000_000b,
    0x0000_0001,
    0x0000_000a,
    0x0004_003b,
    0x0000_000b,
    0x0000_000c,
    0x0000_0001,
    0x0004_002b,
    0x0000_0006,
    0x0000_000e,
    0x3f80_0000,
    0x0005_0036,
    0x0000_0002,
    0x0000_0004,
    0x0000_0000,
    0x0000_0003,
    0x0002_00f8,
    0x0000_0005,
    0x0004_003d,
    0x0000_000a,
    0x0000_000d,
    0x0000_000c,
    0x0005_0051,
    0x0000_0006,
    0x0000_000f,
    0x0000_000d,
    0x0000_0000,
    0x0005_0051,
    0x0000_0006,
    0x0000_0010,
    0x0000_000d,
    0x0000_0001,
    0x0005_0051,
    0x0000_0006,
    0x0000_0011,
    0x0000_000d,
    0x0000_0002,
    0x0007_0050,
    0x0000_0007,
    0x0000_0012,
    0x0000_000f,
    0x0000_0010,
    0x0000_0011,
    0x0000_000e,
    0x0003_003e,
    0x0000_0009,
    0x0000_0012,
    0x0001_00fd,
    0x0001_0038,
];

/// Creates a live native Vulkan renderer for the Stage 0 smoke loop.
#[derive(Clone, Debug)]
pub struct VulkanSmokeRendererCreateInfo {
    /// Application name reported to the Vulkan loader.
    pub application_name: String,
    /// Native window/display handles borrowed from a live window.
    pub native_handles: NativeWindowHandles,
    /// Initial drawable extent.
    pub drawable_extent: (u32, u32),
    /// Whether validation layers must be enabled.
    pub enable_validation: bool,
}

/// Stable smoke renderer bootstrap report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VulkanSmokeRendererReport {
    /// Checked-in shader manifest hash used by the renderer.
    pub shader_manifest_hash: String,
    /// Whether portability enumeration was enabled at instance creation.
    pub portability_enumeration: bool,
    /// Selected device name.
    pub device_name: String,
    /// Graphics queue-family index.
    pub graphics_queue_family: u32,
    /// Present queue-family index.
    pub present_queue_family: u32,
    /// Enabled logical-device extension count.
    pub enabled_extension_count: u32,
    /// Current swapchain extent.
    pub swapchain_extent: (u32, u32),
    /// Current swapchain image count.
    pub swapchain_image_count: u32,
}

/// Measured validation counters from the live smoke loop.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VulkanValidationReport {
    /// Validation warnings observed by the debug messenger.
    pub warning_count: u32,
    /// Validation errors observed by the debug messenger.
    pub error_count: u32,
    /// Stable sorted VUID list.
    pub vuids: Vec<String>,
}

/// Result of one rendered smoke frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VulkanSmokeFrameOutcome {
    /// A frame was submitted and presented.
    Presented,
    /// Rendering was skipped because the swapchain had to be recreated.
    Recreated,
    /// Rendering was skipped because the drawable extent is zero.
    ZeroExtent,
}

/// Live smoke renderer error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VulkanSmokeRendererError {
    /// Instance bootstrap failed.
    Instance(VulkanInstanceError),
    /// Surface bootstrap failed.
    Surface(VulkanSurfaceError),
    /// Logical-device bootstrap failed.
    LogicalDevice(VulkanLogicalDeviceError),
    /// Swapchain bootstrap failed.
    Swapchain(VulkanSwapchainProbeError),
    /// Shader manifest validation failed.
    ShaderManifest(VulkanShaderManifestError),
    /// Vulkan operation failed.
    VulkanOperation {
        /// Operation context.
        context: &'static str,
        /// Raw Vulkan result code.
        result: vk::Result,
    },
    /// No suitable memory type exists for the required properties.
    MissingMemoryType {
        /// Operation context.
        context: &'static str,
    },
    /// Internal smoke renderer state was unexpectedly absent.
    InvariantViolation {
        /// Missing state context.
        context: &'static str,
    },
}

impl std::fmt::Display for VulkanSmokeRendererError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Instance(error) => write!(f, "{error}"),
            Self::Surface(error) => write!(f, "{error}"),
            Self::LogicalDevice(error) => write!(f, "{error}"),
            Self::Swapchain(error) => write!(f, "{error}"),
            Self::ShaderManifest(error) => write!(f, "{error}"),
            Self::VulkanOperation { context, result } => {
                write!(f, "{context}: {result:?}")
            }
            Self::MissingMemoryType { context } => {
                write!(f, "{context}: no compatible Vulkan memory type")
            }
            Self::InvariantViolation { context } => {
                write!(f, "renderer invariant violated: {context}")
            }
        }
    }
}

impl std::error::Error for VulkanSmokeRendererError {}

/// Live Stage 0 Vulkan triangle renderer used by the smoke app.
pub struct VulkanSmokeRenderer {
    instance: Option<VulkanInstanceProbe>,
    validation: Option<VulkanValidationMessenger>,
    surface: Option<VulkanSurfaceProbe>,
    device: Option<VulkanLogicalDeviceProbe>,
    swapchain: Option<VulkanSwapchainProbe>,
    command_pool: vk::CommandPool,
    swapchain_resources: Option<VulkanSwapchainResources>,
    vertex_buffer: Option<VulkanAllocatedBuffer>,
    index_buffer: Option<VulkanAllocatedBuffer>,
    frame_sync: Vec<VulkanFrameSync>,
    images_in_flight: Vec<vk::Fence>,
    current_frame: usize,
    pending_extent: Option<(u32, u32)>,
    swapchain_recreate_count: u32,
    report: VulkanSmokeRendererReport,
}

impl VulkanSmokeRenderer {
    /// Creates a live Vulkan smoke renderer bound to a live native window.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanSmokeRendererError`] when Vulkan bootstrap, pipeline creation,
    /// memory allocation, or synchronization resource creation fails.
    pub fn new(
        create_info: &VulkanSmokeRendererCreateInfo,
    ) -> Result<Self, VulkanSmokeRendererError> {
        let shader_manifest = validate_shader_manifest(&triangle_shader_manifest())
            .map_err(VulkanSmokeRendererError::ShaderManifest)?;
        let surface_plan = plan_vulkan_surface(Some(create_info.native_handles))
            .map_err(VulkanSmokeRendererError::Surface)?;
        let mut instance_config = VulkanInstanceConfig::smoke(&create_info.application_name);
        instance_config
            .required_extensions
            .clone_from(&surface_plan.required_instance_extensions);
        instance_config.enable_validation = create_info.enable_validation;
        let instance = create_vulkan_instance_probe(&instance_config)
            .map_err(VulkanSmokeRendererError::Instance)?;
        let validation = if create_info.enable_validation {
            Some(create_validation_messenger(&instance)?)
        } else {
            None
        };
        let surface = create_vulkan_surface_probe(&instance, Some(create_info.native_handles))
            .map_err(VulkanSmokeRendererError::Surface)?;
        let device =
            create_vulkan_logical_device_probe(&instance, &surface, create_info.drawable_extent)
                .map_err(VulkanSmokeRendererError::LogicalDevice)?;
        let swapchain = create_vulkan_swapchain_probe_for_extent(
            &instance,
            &surface,
            &device,
            create_info.drawable_extent,
            vk::SwapchainKHR::null(),
        )
        .map_err(VulkanSmokeRendererError::Swapchain)?;
        let command_pool = create_command_pool(&device)?;
        let vertex_buffer = match create_triangle_vertex_buffer(&instance, &device) {
            Ok(buffer) => buffer,
            Err(error) => {
                // SAFETY: The command pool belongs to this live logical device and is destroyed on setup failure.
                unsafe { device.device().destroy_command_pool(command_pool, None) };
                return Err(error);
            }
        };
        let index_buffer = match create_triangle_index_buffer(&instance, &device) {
            Ok(buffer) => buffer,
            Err(error) => {
                // SAFETY: The command pool belongs to this live logical device and is destroyed on setup failure.
                unsafe { device.device().destroy_command_pool(command_pool, None) };
                destroy_allocated_buffer(&device, &vertex_buffer);
                return Err(error);
            }
        };
        let mut renderer = Self {
            instance: Some(instance),
            validation,
            surface: Some(surface),
            device: Some(device),
            swapchain: Some(swapchain),
            command_pool,
            swapchain_resources: None,
            vertex_buffer: Some(vertex_buffer),
            index_buffer: Some(index_buffer),
            frame_sync: Vec::new(),
            images_in_flight: Vec::new(),
            current_frame: 0,
            pending_extent: None,
            swapchain_recreate_count: 0,
            report: VulkanSmokeRendererReport {
                shader_manifest_hash: shader_manifest.manifest_hash.clone(),
                portability_enumeration: instance_config.enable_portability_enumeration,
                device_name: String::new(),
                graphics_queue_family: 0,
                present_queue_family: 0,
                enabled_extension_count: 0,
                swapchain_extent: (0, 0),
                swapchain_image_count: 0,
            },
        };
        renderer.rebuild_swapchain_resources(false)?;
        let device_ref = renderer.device_ref()?;
        let swapchain_ref = renderer.swapchain_ref()?;
        renderer.report = VulkanSmokeRendererReport {
            shader_manifest_hash: shader_manifest.manifest_hash,
            portability_enumeration: renderer
                .instance
                .as_ref()
                .is_some_and(|instance| instance.report.create_flags != 0),
            device_name: device_ref.report.device_name.clone(),
            graphics_queue_family: device_ref.report.graphics_queue_family,
            present_queue_family: device_ref.report.present_queue_family,
            enabled_extension_count: device_ref
                .report
                .enabled_extensions
                .len()
                .try_into()
                .unwrap_or(u32::MAX),
            swapchain_extent: swapchain_ref.report.plan.extent,
            swapchain_image_count: swapchain_ref.report.image_count,
        };
        Ok(renderer)
    }

    /// Returns the current bootstrap report.
    #[must_use]
    pub const fn report(&self) -> &VulkanSmokeRendererReport {
        &self.report
    }

    /// Returns measured validation counters and VUIDs.
    #[must_use]
    pub fn validation_report(&self) -> VulkanValidationReport {
        self.validation.as_ref().map_or(
            VulkanValidationReport {
                warning_count: 0,
                error_count: 0,
                vuids: Vec::new(),
            },
            VulkanValidationMessenger::report,
        )
    }

    /// Returns the measured swapchain recreation count.
    #[must_use]
    pub const fn swapchain_recreate_count(&self) -> u32 {
        self.swapchain_recreate_count
    }

    /// Requests swapchain recreation for a new drawable extent.
    pub fn request_resize(&mut self, extent: (u32, u32)) {
        self.pending_extent = Some(extent);
    }

    fn device_ref(&self) -> Result<&VulkanLogicalDeviceProbe, VulkanSmokeRendererError> {
        self.device
            .as_ref()
            .ok_or(VulkanSmokeRendererError::InvariantViolation {
                context: "logical device",
            })
    }

    fn swapchain_ref(&self) -> Result<&VulkanSwapchainProbe, VulkanSmokeRendererError> {
        self.swapchain
            .as_ref()
            .ok_or(VulkanSmokeRendererError::InvariantViolation {
                context: "swapchain",
            })
    }

    fn instance_ref(&self) -> Result<&VulkanInstanceProbe, VulkanSmokeRendererError> {
        self.instance
            .as_ref()
            .ok_or(VulkanSmokeRendererError::InvariantViolation {
                context: "instance",
            })
    }

    fn surface_ref(&self) -> Result<&VulkanSurfaceProbe, VulkanSmokeRendererError> {
        self.surface
            .as_ref()
            .ok_or(VulkanSmokeRendererError::InvariantViolation { context: "surface" })
    }

    fn resources_ref(&self) -> Result<&VulkanSwapchainResources, VulkanSmokeRendererError> {
        self.swapchain_resources
            .as_ref()
            .ok_or(VulkanSmokeRendererError::InvariantViolation {
                context: "swapchain resources",
            })
    }

    fn vertex_buffer_ref(&self) -> Result<&VulkanAllocatedBuffer, VulkanSmokeRendererError> {
        self.vertex_buffer
            .as_ref()
            .ok_or(VulkanSmokeRendererError::InvariantViolation {
                context: "vertex buffer",
            })
    }

    fn index_buffer_ref(&self) -> Result<&VulkanAllocatedBuffer, VulkanSmokeRendererError> {
        self.index_buffer
            .as_ref()
            .ok_or(VulkanSmokeRendererError::InvariantViolation {
                context: "index buffer",
            })
    }

    /// Draws and presents one indexed-triangle frame.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanSmokeRendererError`] when synchronization, command recording,
    /// submission, or presentation fails.
    #[allow(clippy::too_many_lines)]
    pub fn draw_frame(&mut self) -> Result<VulkanSmokeFrameOutcome, VulkanSmokeRendererError> {
        if let Some(extent) = self.pending_extent.take() {
            if extent.0 == 0 || extent.1 == 0 {
                self.pending_extent = Some(extent);
                return Ok(VulkanSmokeFrameOutcome::ZeroExtent);
            }
            self.recreate_swapchain(extent)?;
            return Ok(VulkanSmokeFrameOutcome::Recreated);
        }

        let sync = &self.frame_sync[self.current_frame];
        let image_available = sync.image_available;
        let render_finished = sync.render_finished;
        let in_flight_fence = sync.fence;
        // SAFETY: The fence belongs to this live logical device and is waited from one thread.
        unsafe {
            self.device_ref()?
                .device()
                .wait_for_fences(&[in_flight_fence], true, 1_000_000_000)
        }
        .map_err(|error| VulkanSmokeRendererError::VulkanOperation {
            context: "vkWaitForFences",
            result: error,
        })?;
        // SAFETY: The swapchain, semaphore and fence inputs are live for the duration of the acquire call.
        let acquire = unsafe {
            self.swapchain_ref()?.loader().acquire_next_image(
                self.swapchain_ref()?.swapchain(),
                1_000_000_000,
                image_available,
                vk::Fence::null(),
            )
        };
        let (image_index, acquire_suboptimal) = match acquire {
            Ok(result) => result,
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                self.recreate_swapchain(self.report.swapchain_extent)?;
                return Ok(VulkanSmokeFrameOutcome::Recreated);
            }
            Err(error) => {
                return Err(VulkanSmokeRendererError::VulkanOperation {
                    context: "vkAcquireNextImageKHR",
                    result: error,
                });
            }
        };
        let image_index_usize = usize::try_from(image_index).unwrap_or(0);
        let image_fence = self.images_in_flight[image_index_usize];
        if image_fence != vk::Fence::null() {
            // SAFETY: The fence belongs to this renderer and can be waited independently.
            unsafe {
                self.device_ref()?
                    .device()
                    .wait_for_fences(&[image_fence], true, 1_000_000_000)
            }
            .map_err(|error| VulkanSmokeRendererError::VulkanOperation {
                context: "vkWaitForFences(image)",
                result: error,
            })?;
        }
        self.images_in_flight[image_index_usize] = in_flight_fence;
        // SAFETY: The fence belongs to this frame context and is not in use after the wait above.
        unsafe { self.device_ref()?.device().reset_fences(&[in_flight_fence]) }.map_err(
            |error| VulkanSmokeRendererError::VulkanOperation {
                context: "vkResetFences",
                result: error,
            },
        )?;

        self.record_command_buffer(image_index_usize)?;
        let wait_semaphores = [image_available];
        let wait_stages = [vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT];
        let command_buffers = [self.resources_ref()?.command_buffers[image_index_usize]];
        let signal_semaphores = [render_finished];
        let submit_info = [vk::SubmitInfo::default()
            .wait_semaphores(&wait_semaphores)
            .wait_dst_stage_mask(&wait_stages)
            .command_buffers(&command_buffers)
            .signal_semaphores(&signal_semaphores)];
        // SAFETY: Submission references live queue, sync objects and recorded command buffer.
        unsafe {
            self.device_ref()?.device().queue_submit(
                self.device_ref()?.graphics_queue(),
                &submit_info,
                in_flight_fence,
            )
        }
        .map_err(|error| VulkanSmokeRendererError::VulkanOperation {
            context: "vkQueueSubmit",
            result: error,
        })?;

        let present_wait = [render_finished];
        let swapchains = [self.swapchain_ref()?.swapchain()];
        let image_indices = [image_index];
        let present_info = vk::PresentInfoKHR::default()
            .wait_semaphores(&present_wait)
            .swapchains(&swapchains)
            .image_indices(&image_indices);
        // SAFETY: Presentation uses the rendered image index and a semaphore signaled by queue submission.
        let present_suboptimal = match unsafe {
            self.swapchain_ref()?
                .loader()
                .queue_present(self.device_ref()?.present_queue(), &present_info)
        } {
            Ok(suboptimal) => suboptimal,
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                self.recreate_swapchain(self.report.swapchain_extent)?;
                return Ok(VulkanSmokeFrameOutcome::Recreated);
            }
            Err(error) => {
                return Err(VulkanSmokeRendererError::VulkanOperation {
                    context: "vkQueuePresentKHR",
                    result: error,
                });
            }
        };

        self.current_frame = (self.current_frame + 1) % self.frame_sync.len().max(1);
        if acquire_suboptimal || present_suboptimal {
            self.recreate_swapchain(self.report.swapchain_extent)?;
            Ok(VulkanSmokeFrameOutcome::Recreated)
        } else {
            Ok(VulkanSmokeFrameOutcome::Presented)
        }
    }

    fn recreate_swapchain(&mut self, extent: (u32, u32)) -> Result<(), VulkanSmokeRendererError> {
        let device = self.device_ref()?;
        // SAFETY: The logical device remains live and idling at swapchain recreation boundaries.
        unsafe { device.device().device_wait_idle() }.map_err(|error| {
            VulkanSmokeRendererError::VulkanOperation {
                context: "vkDeviceWaitIdle",
                result: error,
            }
        })?;
        self.pending_extent = None;
        self.rebuild_swapchain(extent)?;
        self.swapchain_recreate_count = self.swapchain_recreate_count.saturating_add(1);
        Ok(())
    }

    fn rebuild_swapchain(&mut self, extent: (u32, u32)) -> Result<(), VulkanSmokeRendererError> {
        self.destroy_swapchain_resources();
        let instance = self.instance_ref()?;
        let surface = self.surface_ref()?;
        let device = self.device_ref()?;
        let old_swapchain = self
            .swapchain
            .as_ref()
            .map_or(vk::SwapchainKHR::null(), VulkanSwapchainProbe::swapchain);
        let new_swapchain = create_vulkan_swapchain_probe_for_extent(
            instance,
            surface,
            device,
            extent,
            old_swapchain,
        )
        .map_err(VulkanSmokeRendererError::Swapchain)?;
        self.swapchain = Some(new_swapchain);
        self.rebuild_swapchain_resources(true)?;
        Ok(())
    }

    fn rebuild_swapchain_resources(
        &mut self,
        reuse_command_pool: bool,
    ) -> Result<(), VulkanSmokeRendererError> {
        let resources = {
            let device = self.device_ref()?;
            let swapchain = self.swapchain_ref()?;
            create_swapchain_resources(
                device,
                swapchain,
                self.command_pool,
                self.vertex_buffer_ref()?,
                self.index_buffer_ref()?,
                reuse_command_pool,
            )?
        };
        let frame_sync = {
            let device = self.device_ref()?;
            create_frame_sync(device)?
        };
        let swapchain_extent = self.swapchain_ref()?.report.plan.extent;
        let swapchain_image_count = self.swapchain_ref()?.report.image_count;
        self.images_in_flight = vec![vk::Fence::null(); resources.image_views.len()];
        self.frame_sync = frame_sync;
        self.report.swapchain_extent = swapchain_extent;
        self.report.swapchain_image_count = swapchain_image_count;
        self.swapchain_resources = Some(resources);
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn record_command_buffer(
        &mut self,
        image_index: usize,
    ) -> Result<(), VulkanSmokeRendererError> {
        let device = self.device_ref()?;
        let swapchain = self.swapchain_ref()?;
        let resources = self.resources_ref()?;
        let command_buffer = resources.command_buffers[image_index];
        // SAFETY: The command buffer belongs to the resettable pool owned by this renderer.
        unsafe {
            device
                .device()
                .reset_command_buffer(command_buffer, vk::CommandBufferResetFlags::empty())
        }
        .map_err(|error| VulkanSmokeRendererError::VulkanOperation {
            context: "vkResetCommandBuffer",
            result: error,
        })?;
        let begin_info = vk::CommandBufferBeginInfo::default();
        // SAFETY: The command buffer is in the initial state after reset and recorded on one thread.
        unsafe {
            device
                .device()
                .begin_command_buffer(command_buffer, &begin_info)
        }
        .map_err(|error| VulkanSmokeRendererError::VulkanOperation {
            context: "vkBeginCommandBuffer",
            result: error,
        })?;

        let pre_barrier = vk::ImageMemoryBarrier::default()
            .old_layout(vk::ImageLayout::PRESENT_SRC_KHR)
            .new_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .subresource_range(color_subresource_range())
            .src_access_mask(vk::AccessFlags::empty())
            .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE);
        // SAFETY: The swapchain is live and queried only to resolve the current image handles.
        let swapchain_images = unsafe {
            swapchain
                .loader()
                .get_swapchain_images(swapchain.swapchain())
        }
        .map_err(|error| VulkanSmokeRendererError::VulkanOperation {
            context: "vkGetSwapchainImagesKHR",
            result: error,
        })?;
        let pre_barrier = pre_barrier.image(swapchain_images[image_index]);
        // SAFETY: The barriers operate on the acquired swapchain image owned by this command buffer submission.
        unsafe {
            device.device().cmd_pipeline_barrier(
                command_buffer,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[pre_barrier],
            );
        }

        let clear_values = [vk::ClearValue {
            color: vk::ClearColorValue {
                float32: [0.05, 0.08, 0.11, 1.0],
            },
        }];
        let render_area = vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: vk::Extent2D {
                width: swapchain.report.plan.extent.0,
                height: swapchain.report.plan.extent.1,
            },
        };
        let render_pass_info = vk::RenderPassBeginInfo::default()
            .render_pass(resources.render_pass)
            .framebuffer(resources.framebuffers[image_index])
            .render_area(render_area)
            .clear_values(&clear_values);
        // SAFETY: All commands target live frame resources owned by this renderer.
        unsafe {
            device.device().cmd_begin_render_pass(
                command_buffer,
                &render_pass_info,
                vk::SubpassContents::INLINE,
            );
            device.device().cmd_bind_pipeline(
                command_buffer,
                vk::PipelineBindPoint::GRAPHICS,
                resources.pipeline,
            );
            let vertex_buffers = [self.vertex_buffer_ref()?.buffer];
            let offsets = [0_u64];
            device
                .device()
                .cmd_bind_vertex_buffers(command_buffer, 0, &vertex_buffers, &offsets);
            device.device().cmd_bind_index_buffer(
                command_buffer,
                self.index_buffer_ref()?.buffer,
                0,
                vk::IndexType::UINT16,
            );
            device
                .device()
                .cmd_draw_indexed(command_buffer, 3, 1, 0, 0, 0);
            device.device().cmd_end_render_pass(command_buffer);
        }

        let post_barrier = vk::ImageMemoryBarrier::default()
            .old_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .new_layout(vk::ImageLayout::PRESENT_SRC_KHR)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(swapchain_images[image_index])
            .subresource_range(color_subresource_range())
            .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
            .dst_access_mask(vk::AccessFlags::empty());
        // SAFETY: The post-render barrier transitions the same live swapchain image into present layout.
        unsafe {
            device.device().cmd_pipeline_barrier(
                command_buffer,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[post_barrier],
            );
            device.device().end_command_buffer(command_buffer)
        }
        .map_err(|error| VulkanSmokeRendererError::VulkanOperation {
            context: "vkEndCommandBuffer",
            result: error,
        })?;
        Ok(())
    }

    fn destroy_swapchain_resources(&mut self) {
        let Some(device) = self.device.as_ref() else {
            return;
        };
        for sync in self.frame_sync.drain(..) {
            // SAFETY: These sync objects belong to this device and are destroyed once.
            unsafe {
                device
                    .device()
                    .destroy_semaphore(sync.image_available, None);
                device
                    .device()
                    .destroy_semaphore(sync.render_finished, None);
                device.device().destroy_fence(sync.fence, None);
            }
        }
        if let Some(resources) = self.swapchain_resources.take() {
            destroy_swapchain_resources(device, self.command_pool, resources);
        }
        self.images_in_flight.clear();
        self.current_frame = 0;
    }

    fn teardown(&mut self) {
        if let Some(device) = self.device.as_ref() {
            // SAFETY: The logical device remains live until teardown finishes and idling prevents in-flight work from touching swapchain, buffers, sync objects or the command pool after destruction starts.
            let _ = unsafe { device.device().device_wait_idle() };
        }
        self.destroy_swapchain_resources();
        if let Some(device) = self.device.as_ref() {
            if let Some(buffer) = self.index_buffer.take() {
                // SAFETY: Buffer and memory belong to this device and are destroyed once after the device has been idled and frame work has been torn down.
                unsafe {
                    device.device().destroy_buffer(buffer.buffer, None);
                    device.device().free_memory(buffer.memory, None);
                }
            }
            if let Some(buffer) = self.vertex_buffer.take() {
                // SAFETY: Buffer and memory belong to this device and are destroyed once after the device has been idled and frame work has been torn down.
                unsafe {
                    device.device().destroy_buffer(buffer.buffer, None);
                    device.device().free_memory(buffer.memory, None);
                }
            }
            // SAFETY: The command pool belongs to this device and is destroyed once after the device is idle and all command buffers allocated from it were freed above.
            unsafe {
                device
                    .device()
                    .destroy_command_pool(self.command_pool, None);
            };
        }
        // Drop child Vulkan owners explicitly before their parents instead of relying on field order.
        self.swapchain.take();
        self.device.take();
        self.surface.take();
        self.validation.take();
        self.instance.take();
    }
}

impl Drop for VulkanSmokeRenderer {
    fn drop(&mut self) {
        self.teardown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{KHR_PORTABILITY_SUBSET_EXTENSION, KHR_SWAPCHAIN_EXTENSION};
    use crate::shader_manifest::{
        SHADER_COMPILER_BINARY_SHA256, SHADER_COMPILER_NAME, SHADER_COMPILER_VERSION,
        SHADER_MANIFEST_SCHEMA, SHADER_TARGET_ENV, SPIRV_MAGIC, SPIRV_VALIDATOR_BINARY_SHA256,
        SPIRV_VALIDATOR_NAME, SPIRV_VALIDATOR_VERSION, SPIRV_VERSION_1_0,
        TRIANGLE_VERTEX_COMPILE_COMMAND, TRIANGLE_VERTEX_SOURCE_PATH,
        TRIANGLE_VERTEX_SOURCE_SHA256, TRIANGLE_VERTEX_SPIRV_PATH,
        TRIANGLE_VERTEX_VALIDATE_COMMAND,
    };
    use crate::*;
    use fparkan_platform::RenderRequest;
    use fparkan_render::{
        DrawCommand, DrawId, GpuMaterialId, GpuMeshId, IndexRange, RenderCommand, RenderPhase,
    };
    use fparkan_render::{RenderBackend, RenderError};

    #[test]
    fn planning_backend_tracks_render_request_and_simulated_present() -> Result<(), RenderError> {
        let mut backend = VulkanPlanningBackend::new();
        let request = RenderRequest::conservative();
        backend.set_render_request(request);
        assert_eq!(backend.render_request(), request);
        assert_eq!(backend.report().resize_rebuilds, 1);

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
        assert_eq!(backend.state(), VulkanPlanningBackendState::Ready);
        assert_eq!(backend.report().frames_executed, 1);
        assert_eq!(backend.report().submissions, 1);
        assert_eq!(backend.report().simulated_presents, 1);
        assert!(backend.report().last_capture_size > 0);
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

        let report =
            select_physical_device(&[rejected, accepted]).expect("selected fallback device");

        assert_eq!(report.device_name, "Accepted");
        assert_eq!(report.graphics_queue_family, 2);
        assert_eq!(report.present_queue_family, 2);
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
    fn capability_report_json_is_stable() {
        let report = select_physical_device(&[device(
            "GPU \"A\"",
            VulkanDeviceType::DiscreteGpu,
            3,
            true,
            false,
        )])
        .expect("selected device");

        assert_eq!(
            render_capability_report_json(&report),
            "{\"schema\":1,\"vulkan_api\":\"1.1.0\",\"device_name\":\"GPU \\\"A\\\"\",\"score\":1101,\"graphics_queue_family\":3,\"present_queue_family\":3,\"portability_subset\":false,\"enabled_extensions\":[\"VK_KHR_swapchain\"]}"
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
        let report =
            validate_shader_manifest(&triangle_shader_manifest()).expect("shader manifest");

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
        assert_eq!(report.modules[0].word_count, 253);
        assert_eq!(
            report.modules[0].sha256,
            "9023b1cc856c98ecd21755596c4e9d1e62cc63e1787f8c43ada2101544e8d0d1"
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
            "6efe2c9716ae845c471ecbaac2c83e56a17a37dc017dd63f0a05f0d9161f44ba"
        );
        assert_eq!(
            report.manifest_hash,
            "725529e9449fa53017e7df75f3f14c76d53479a5a7617d55ec78280b3059bc44"
        );
    }

    #[test]
    fn shader_manifest_report_json_is_stable() {
        let report =
            validate_shader_manifest(&triangle_shader_manifest()).expect("shader manifest");
        let json = render_shader_manifest_report_json(&report);

        assert!(json.contains(SHADER_COMPILER_NAME));
        assert!(json.contains(SPIRV_VALIDATOR_NAME));
        assert!(json.contains(TRIANGLE_VERTEX_SOURCE_PATH));
        assert!(json.contains(TRIANGLE_VERTEX_COMPILE_COMMAND));
    }

    #[test]
    fn checked_in_shader_manifest_matches_generated_report() {
        let report =
            validate_shader_manifest(&triangle_shader_manifest()).expect("shader manifest");
        assert_eq!(
            render_shader_manifest_report_json(&report),
            include_str!("../shaders/manifest.json").trim()
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
}
