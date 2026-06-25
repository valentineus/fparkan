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
#[cfg(test)]
use self::surface::extension_name;
pub use self::surface::{
    create_vulkan_surface_probe, plan_vulkan_surface, render_surface_plan_json, VulkanSurfaceError,
    VulkanSurfacePlan, VulkanSurfaceProbe,
};
use self::validation::{create_validation_messenger, VulkanValidationMessenger};
use crate::policy::*;
use crate::shader_manifest::{
    triangle_shader_manifest, validate_shader_manifest, VulkanShaderManifestError,
};
use ash::{khr::swapchain, vk};
use fparkan_platform::NativeWindowHandles;
use std::ffi::{CStr, CString};
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

/// Live Vulkan device/surface capability probe.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VulkanRuntimeCapabilityProbe {
    /// Selected device/queue capability report.
    pub capability: VulkanCapabilityReport,
    /// Swapchain plan built from the selected device and live surface capabilities.
    pub swapchain: VulkanSwapchainPlan,
}

/// Created Vulkan logical device probe.
pub struct VulkanLogicalDeviceProbe {
    device: ash::Device,
    physical_device: vk::PhysicalDevice,
    /// Runtime capability report used for device selection.
    pub runtime: VulkanRuntimeCapabilityProbe,
    /// Deterministic logical device creation report.
    pub report: VulkanLogicalDeviceReport,
}

impl Drop for VulkanLogicalDeviceProbe {
    fn drop(&mut self) {
        // SAFETY: The logical device was created by this probe and is destroyed once during drop.
        unsafe { self.device.destroy_device(None) };
    }
}

impl VulkanLogicalDeviceProbe {
    /// Returns the graphics queue selected by the Stage 0 policy.
    #[must_use]
    pub fn graphics_queue(&self) -> vk::Queue {
        // SAFETY: The queue-family index belongs to this live logical device.
        unsafe {
            self.device
                .get_device_queue(self.report.graphics_queue_family, 0)
        }
    }

    /// Returns the presentation queue selected by the Stage 0 policy.
    #[must_use]
    pub fn present_queue(&self) -> vk::Queue {
        // SAFETY: The queue-family index belongs to this live logical device.
        unsafe {
            self.device
                .get_device_queue(self.report.present_queue_family, 0)
        }
    }

    /// Returns a shared reference to the live logical device.
    #[must_use]
    pub fn device(&self) -> &ash::Device {
        &self.device
    }
}

/// Logical device creation report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VulkanLogicalDeviceReport {
    /// Report schema version.
    pub schema: u32,
    /// Selected physical device name.
    pub device_name: String,
    /// Graphics queue-family index used by the logical device.
    pub graphics_queue_family: u32,
    /// Present queue-family index used by the logical device.
    pub present_queue_family: u32,
    /// Enabled device extensions.
    pub enabled_extensions: Vec<String>,
}

/// Created Vulkan swapchain probe.
pub struct VulkanSwapchainProbe {
    loader: swapchain::Device,
    swapchain: vk::SwapchainKHR,
    /// Deterministic swapchain creation report.
    pub report: VulkanSwapchainReport,
}

impl Drop for VulkanSwapchainProbe {
    fn drop(&mut self) {
        // SAFETY: The swapchain was created by this probe and is destroyed once during drop.
        unsafe { self.loader.destroy_swapchain(self.swapchain, None) };
    }
}

impl VulkanSwapchainProbe {
    /// Returns the live swapchain handle.
    #[must_use]
    pub fn swapchain(&self) -> vk::SwapchainKHR {
        self.swapchain
    }

    /// Returns the swapchain extension loader for this live swapchain.
    #[must_use]
    pub fn loader(&self) -> &swapchain::Device {
        &self.loader
    }
}

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

struct VulkanAllocatedBuffer {
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
}

struct VulkanSwapchainResources {
    image_views: Vec<vk::ImageView>,
    render_pass: vk::RenderPass,
    pipeline_layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    framebuffers: Vec<vk::Framebuffer>,
    command_buffers: Vec<vk::CommandBuffer>,
}

struct PartialSwapchainResources {
    image_views: Vec<vk::ImageView>,
    render_pass: Option<vk::RenderPass>,
    pipeline_layout: Option<vk::PipelineLayout>,
    pipeline: Option<vk::Pipeline>,
    framebuffers: Vec<vk::Framebuffer>,
    command_buffers: Vec<vk::CommandBuffer>,
}

struct VulkanFrameSync {
    image_available: vk::Semaphore,
    render_finished: vk::Semaphore,
    fence: vk::Fence,
}

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

fn create_command_pool(
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

fn create_triangle_vertex_buffer(
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

fn create_triangle_index_buffer(
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
        device.physical_device,
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
        // SAFETY: Allocation uses a memory type index selected from the physical-device requirements above.
        unsafe { device.device().allocate_memory(&allocate_info, None) }.map_err(|error| {
            // SAFETY: The buffer was created above on this logical device and is destroyed on setup failure.
            unsafe { device.device().destroy_buffer(buffer, None) };
            VulkanSmokeRendererError::VulkanOperation {
                context,
                result: error,
            }
        })?;
    // SAFETY: The buffer and allocation belong to the same live logical device.
    unsafe { device.device().bind_buffer_memory(buffer, memory, 0) }.map_err(|error| {
        // SAFETY: The buffer and allocation belong to this logical device and are destroyed on setup failure.
        unsafe {
            device.device().destroy_buffer(buffer, None);
            device.device().free_memory(memory, None);
        }
        VulkanSmokeRendererError::VulkanOperation {
            context,
            result: error,
        }
    })?;
    // SAFETY: The allocation is HOST_VISIBLE, mapped for the full buffer size and unmapped before return.
    let mapped = unsafe {
        device
            .device()
            .map_memory(memory, 0, requirements.size, vk::MemoryMapFlags::empty())
    }
    .map_err(|error| {
        // SAFETY: The buffer and allocation belong to this logical device and are destroyed on setup failure.
        unsafe {
            device.device().destroy_buffer(buffer, None);
            device.device().free_memory(memory, None);
        }
        VulkanSmokeRendererError::VulkanOperation {
            context,
            result: error,
        }
    })?;
    // SAFETY: The mapped pointer is valid for `bytes.len()` bytes and non-overlapping with the source slice.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), mapped.cast::<u8>(), bytes.len());
        device.device().unmap_memory(memory);
    }
    Ok(VulkanAllocatedBuffer { buffer, memory })
}

fn find_memory_type(
    instance: &VulkanInstanceProbe,
    physical_device: vk::PhysicalDevice,
    memory_type_bits: u32,
    required_properties: vk::MemoryPropertyFlags,
) -> Option<u32> {
    // SAFETY: Physical-device memory properties are queried from a live instance-owned physical device.
    let memory_properties = unsafe {
        instance
            .instance
            .get_physical_device_memory_properties(physical_device)
    };
    memory_properties
        .memory_types
        .iter()
        .enumerate()
        .find_map(|(index, memory_type)| {
            let supported = (memory_type_bits & (1_u32 << index)) != 0;
            let has_properties = memory_type.property_flags.contains(required_properties);
            (supported && has_properties).then(|| index.try_into().unwrap_or(u32::MAX))
        })
}

#[allow(clippy::too_many_lines)]
fn create_swapchain_resources(
    device: &VulkanLogicalDeviceProbe,
    swapchain: &VulkanSwapchainProbe,
    command_pool: vk::CommandPool,
    _vertex_buffer: &VulkanAllocatedBuffer,
    _index_buffer: &VulkanAllocatedBuffer,
    _reuse_command_pool: bool,
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
    for image in &images {
        match create_image_view(device, *image, swapchain.report.plan.format.format) {
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
    for image_view in &partial.image_views {
        match create_framebuffer(
            device,
            render_pass,
            *image_view,
            swapchain.report.plan.extent,
        ) {
            Ok(framebuffer) => partial.framebuffers.push(framebuffer),
            Err(error) => {
                destroy_partial_swapchain_resources(device, command_pool, partial);
                return Err(error);
            }
        }
    }
    partial.command_buffers = match allocate_command_buffers(
        device,
        command_pool,
        partial.image_views.len().try_into().unwrap_or(u32::MAX),
    ) {
        Ok(command_buffers) => command_buffers,
        Err(error) => {
            destroy_partial_swapchain_resources(device, command_pool, partial);
            return Err(error);
        }
    };
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

fn create_frame_sync(
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
        let render_finished =
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
            };
        let fence =
            // SAFETY: The fence belongs to this live logical device and is destroyed at teardown.
            match unsafe { device.device().create_fence(&fence_info, None) } {
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

fn destroy_swapchain_resources(
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

fn destroy_allocated_buffer(device: &VulkanLogicalDeviceProbe, buffer: &VulkanAllocatedBuffer) {
    // SAFETY: The buffer and allocation belong to this live logical device and are destroyed once during teardown.
    unsafe {
        device.device().destroy_buffer(buffer.buffer, None);
        device.device().free_memory(buffer.memory, None);
    }
}

fn color_subresource_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .base_mip_level(0)
        .level_count(1)
        .base_array_layer(0)
        .layer_count(1)
}

/// Runtime swapchain creation report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VulkanSwapchainReport {
    /// Report schema version.
    pub schema: u32,
    /// Deterministic swapchain policy used for creation.
    pub plan: VulkanSwapchainPlan,
    /// Number of images returned by `vkGetSwapchainImagesKHR`.
    pub image_count: u32,
}

/// Live Vulkan device/surface capability probe error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VulkanRuntimeCapabilityError {
    /// Physical device enumeration failed.
    EnumerateDevicesFailed {
        /// Vulkan result.
        result: vk::Result,
    },
    /// Device extension enumeration failed.
    EnumerateDeviceExtensionsFailed {
        /// Device name or index context.
        device: String,
        /// Vulkan result.
        result: vk::Result,
    },
    /// Queue-family present support query failed.
    PresentSupportFailed {
        /// Device name.
        device: String,
        /// Queue-family index.
        queue_family: u32,
        /// Vulkan result.
        result: vk::Result,
    },
    /// Surface format query failed.
    SurfaceFormatsFailed {
        /// Device name.
        device: String,
        /// Vulkan result.
        result: vk::Result,
    },
    /// Surface capability query failed.
    SurfaceCapabilitiesFailed {
        /// Device name.
        device: String,
        /// Vulkan result.
        result: vk::Result,
    },
    /// Present mode query failed.
    PresentModesFailed {
        /// Device name.
        device: String,
        /// Vulkan result.
        result: vk::Result,
    },
    /// No device satisfied Stage 0 capability policy.
    Capability(VulkanCapabilityError),
    /// Live surface capabilities could not produce a swapchain plan.
    Swapchain(VulkanSwapchainError),
}

impl std::fmt::Display for VulkanRuntimeCapabilityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EnumerateDevicesFailed { result } => {
                write!(f, "Vulkan physical device enumeration failed: {result:?}")
            }
            Self::EnumerateDeviceExtensionsFailed { device, result } => write!(
                f,
                "Vulkan device {device} extension enumeration failed: {result:?}"
            ),
            Self::PresentSupportFailed {
                device,
                queue_family,
                result,
            } => write!(
                f,
                "Vulkan device {device} queue family {queue_family} present support query failed: {result:?}"
            ),
            Self::SurfaceFormatsFailed { device, result } => write!(
                f,
                "Vulkan device {device} surface format query failed: {result:?}"
            ),
            Self::SurfaceCapabilitiesFailed { device, result } => write!(
                f,
                "Vulkan device {device} surface capabilities query failed: {result:?}"
            ),
            Self::PresentModesFailed { device, result } => write!(
                f,
                "Vulkan device {device} present mode query failed: {result:?}"
            ),
            Self::Capability(error) => write!(f, "{error}"),
            Self::Swapchain(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for VulkanRuntimeCapabilityError {}

/// Vulkan logical device creation error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VulkanLogicalDeviceError {
    /// Runtime capability probing failed.
    Runtime(VulkanRuntimeCapabilityError),
    /// Device extension name contained an interior NUL byte.
    InvalidExtensionName {
        /// Invalid extension name.
        extension: String,
    },
    /// Logical device creation failed.
    CreateFailed {
        /// Selected device name.
        device: String,
        /// Vulkan result.
        result: vk::Result,
    },
}

impl std::fmt::Display for VulkanLogicalDeviceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Runtime(error) => write!(f, "{error}"),
            Self::InvalidExtensionName { extension } => write!(
                f,
                "Vulkan device extension name contains an interior NUL byte: {extension:?}"
            ),
            Self::CreateFailed { device, result } => {
                write!(
                    f,
                    "Vulkan logical device creation failed for {device}: {result:?}"
                )
            }
        }
    }
}

impl std::error::Error for VulkanLogicalDeviceError {}

/// Vulkan swapchain creation error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VulkanSwapchainProbeError {
    /// Live runtime capability probing failed before swapchain creation.
    Runtime(VulkanRuntimeCapabilityError),
    /// Deterministic swapchain planning failed before create.
    Plan(VulkanSwapchainError),
    /// Surface capability query failed.
    SurfaceCapabilitiesFailed {
        /// Vulkan result.
        result: vk::Result,
    },
    /// Swapchain creation failed.
    CreateFailed {
        /// Vulkan result.
        result: vk::Result,
    },
    /// Swapchain image query failed.
    ImagesFailed {
        /// Vulkan result.
        result: vk::Result,
    },
}

impl std::fmt::Display for VulkanSwapchainProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Runtime(error) => write!(f, "{error}"),
            Self::Plan(error) => write!(f, "{error}"),
            Self::SurfaceCapabilitiesFailed { result } => {
                write!(f, "Vulkan surface capabilities query failed: {result:?}")
            }
            Self::CreateFailed { result } => {
                write!(f, "Vulkan swapchain creation failed: {result:?}")
            }
            Self::ImagesFailed { result } => {
                write!(f, "Vulkan swapchain image query failed: {result:?}")
            }
        }
    }
}

impl std::error::Error for VulkanSwapchainProbeError {}

/// Probes live Vulkan device, queue, surface and swapchain capabilities.
///
/// # Errors
///
/// Returns [`VulkanRuntimeCapabilityError`] when device enumeration, surface
/// capability queries, Stage 0 device selection, or swapchain planning fails.
pub fn probe_vulkan_runtime_capabilities(
    instance: &VulkanInstanceProbe,
    surface: &VulkanSurfaceProbe,
    drawable_extent: (u32, u32),
) -> Result<VulkanRuntimeCapabilityProbe, VulkanRuntimeCapabilityError> {
    let selected = select_live_device_candidate(instance, surface, drawable_extent)?;
    Ok(selected.runtime)
}

/// Creates a Vulkan logical device for the selected live surface-capable device.
///
/// # Errors
///
/// Returns [`VulkanLogicalDeviceError`] when runtime capability probing fails,
/// device extension names are invalid, or `vkCreateDevice` fails.
pub fn create_vulkan_logical_device_probe(
    instance: &VulkanInstanceProbe,
    surface: &VulkanSurfaceProbe,
    drawable_extent: (u32, u32),
) -> Result<VulkanLogicalDeviceProbe, VulkanLogicalDeviceError> {
    let selected = select_live_device_candidate(instance, surface, drawable_extent)
        .map_err(VulkanLogicalDeviceError::Runtime)?;
    let capability = &selected.runtime.capability;
    let queue_priorities = [1.0_f32];
    let queue_families = unique_queue_families(
        capability.graphics_queue_family,
        capability.present_queue_family,
    );
    let queue_infos = queue_families
        .iter()
        .map(|queue_family| {
            vk::DeviceQueueCreateInfo::default()
                .queue_family_index(*queue_family)
                .queue_priorities(&queue_priorities)
        })
        .collect::<Vec<_>>();
    let extension_names = device_extension_cstrings(&capability.enabled_extensions)
        .map_err(|extension| VulkanLogicalDeviceError::InvalidExtensionName { extension })?;
    let extension_ptrs = extension_names
        .iter()
        .map(|extension| extension.as_ptr())
        .collect::<Vec<_>>();
    let create_info = vk::DeviceCreateInfo::default()
        .queue_create_infos(&queue_infos)
        .enabled_extension_names(&extension_ptrs);
    // SAFETY: `selected.physical_device` belongs to `instance`; create data lives for the call.
    let device = unsafe {
        instance
            .instance
            .create_device(selected.physical_device, &create_info, None)
    }
    .map_err(|error| VulkanLogicalDeviceError::CreateFailed {
        device: capability.device_name.clone(),
        result: error,
    })?;
    // SAFETY: Queue family indices came from validated live queue families requested above.
    let _graphics_queue = unsafe { device.get_device_queue(capability.graphics_queue_family, 0) };
    // SAFETY: Queue family indices came from validated live queue families requested above.
    let _present_queue = unsafe { device.get_device_queue(capability.present_queue_family, 0) };
    Ok(VulkanLogicalDeviceProbe {
        device,
        physical_device: selected.physical_device,
        report: VulkanLogicalDeviceReport {
            schema: 1,
            device_name: capability.device_name.clone(),
            graphics_queue_family: capability.graphics_queue_family,
            present_queue_family: capability.present_queue_family,
            enabled_extensions: capability.enabled_extensions.clone(),
        },
        runtime: selected.runtime,
    })
}

/// Creates a Vulkan swapchain for the live logical device and surface.
///
/// # Errors
///
/// Returns [`VulkanSwapchainProbeError`] when live surface capability queries,
/// swapchain creation, or swapchain image enumeration fails.
pub fn create_vulkan_swapchain_probe(
    instance: &VulkanInstanceProbe,
    surface: &VulkanSurfaceProbe,
    device: &VulkanLogicalDeviceProbe,
) -> Result<VulkanSwapchainProbe, VulkanSwapchainProbeError> {
    create_vulkan_swapchain_probe_for_extent(
        instance,
        surface,
        device,
        device.runtime.swapchain.extent,
        vk::SwapchainKHR::null(),
    )
}

/// Creates a Vulkan swapchain for the live logical device and surface at a specific extent.
///
/// # Errors
///
/// Returns [`VulkanSwapchainProbeError`] when live surface capability queries,
/// swapchain creation, or swapchain image enumeration fails.
pub fn create_vulkan_swapchain_probe_for_extent(
    instance: &VulkanInstanceProbe,
    surface: &VulkanSurfaceProbe,
    device: &VulkanLogicalDeviceProbe,
    drawable_extent: (u32, u32),
    old_swapchain: vk::SwapchainKHR,
) -> Result<VulkanSwapchainProbe, VulkanSwapchainProbeError> {
    let raw_capabilities = {
        // SAFETY: The physical device and surface are live query inputs and no handles are retained.
        unsafe {
            surface
                .loader
                .get_physical_device_surface_capabilities(device.physical_device, surface.surface)
        }
    }
    .map_err(|error| VulkanSwapchainProbeError::SurfaceCapabilitiesFailed { result: error })?;
    let surface_formats =
        live_surface_formats(surface, device.physical_device, &device.report.device_name)
            .map_err(VulkanSwapchainProbeError::Runtime)?;
    let present_modes =
        live_present_modes(surface, device.physical_device, &device.report.device_name)
            .map_err(VulkanSwapchainProbeError::Runtime)?;
    let capabilities =
        live_surface_capabilities(surface, device.physical_device, &device.report.device_name)
            .map_err(VulkanSwapchainProbeError::Runtime)?;
    let plan = plan_vulkan_swapchain(&VulkanSwapchainRequest {
        drawable_extent,
        formats: surface_formats,
        present_modes,
        capabilities,
        preferred_present_mode: vk::PresentModeKHR::MAILBOX.as_raw(),
    })
    .map_err(VulkanSwapchainProbeError::Plan)?;
    let queue_family_indices = unique_queue_families(
        device.runtime.capability.graphics_queue_family,
        device.runtime.capability.present_queue_family,
    );
    let sharing_mode = if queue_family_indices.len() > 1 {
        vk::SharingMode::CONCURRENT
    } else {
        vk::SharingMode::EXCLUSIVE
    };
    let create_info = vk::SwapchainCreateInfoKHR::default()
        .surface(surface.surface)
        .min_image_count(plan.image_count)
        .image_format(vk::Format::from_raw(plan.format.format))
        .image_color_space(vk::ColorSpaceKHR::from_raw(plan.format.color_space))
        .image_extent(vk::Extent2D {
            width: plan.extent.0,
            height: plan.extent.1,
        })
        .image_array_layers(1)
        .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
        .image_sharing_mode(sharing_mode)
        .queue_family_indices(&queue_family_indices)
        .pre_transform(raw_capabilities.current_transform)
        .composite_alpha(select_composite_alpha(
            raw_capabilities.supported_composite_alpha,
        ))
        .present_mode(vk::PresentModeKHR::from_raw(plan.present_mode))
        .old_swapchain(old_swapchain)
        .clipped(true);
    let loader = swapchain::Device::new(&instance.instance, &device.device);
    // SAFETY: The create info references live instance/device/surface handles for this call.
    let swapchain = unsafe { loader.create_swapchain(&create_info, None) }
        .map_err(|error| VulkanSwapchainProbeError::CreateFailed { result: error })?;
    // SAFETY: The swapchain was created above and the returned image handles are owned by it.
    let images = match unsafe { loader.get_swapchain_images(swapchain) } {
        Ok(images) => images,
        Err(error) => {
            // SAFETY: The swapchain was created above on this loader/device pair and is destroyed on setup failure.
            unsafe { loader.destroy_swapchain(swapchain, None) };
            return Err(VulkanSwapchainProbeError::ImagesFailed { result: error });
        }
    };
    Ok(VulkanSwapchainProbe {
        loader,
        swapchain,
        report: VulkanSwapchainReport {
            schema: 1,
            plan,
            image_count: images.len().try_into().unwrap_or(u32::MAX),
        },
    })
}

fn select_live_device_candidate(
    instance: &VulkanInstanceProbe,
    surface: &VulkanSurfaceProbe,
    drawable_extent: (u32, u32),
) -> Result<SelectedLiveDevice, VulkanRuntimeCapabilityError> {
    let devices = {
        // SAFETY: The Vulkan instance is live for this query and no handles are retained.
        unsafe { instance.instance.enumerate_physical_devices() }.map_err(|error| {
            VulkanRuntimeCapabilityError::EnumerateDevicesFailed { result: error }
        })?
    };
    let mut best: Option<LiveDeviceCandidate> = None;
    let mut last_error = None;
    for (index, device) in devices.iter().copied().enumerate() {
        let candidate = match live_device_candidate(instance, surface, device, index) {
            Ok(candidate) => candidate,
            Err(err) => {
                last_error = Some(err);
                continue;
            }
        };
        match &best {
            Some(existing)
                if compare_reports(&candidate.capability, &existing.capability)
                    != std::cmp::Ordering::Greater => {}
            _ => best = Some(candidate),
        }
    }
    let best = best.ok_or_else(|| {
        last_error.unwrap_or(VulkanRuntimeCapabilityError::Capability(
            VulkanCapabilityError::NoPhysicalDevice,
        ))
    })?;
    let swapchain = plan_vulkan_swapchain(&VulkanSwapchainRequest {
        drawable_extent,
        formats: best.surface_formats,
        present_modes: best.present_modes,
        capabilities: best.surface_capabilities,
        preferred_present_mode: vk::PresentModeKHR::MAILBOX.as_raw(),
    })
    .map_err(VulkanRuntimeCapabilityError::Swapchain)?;
    Ok(SelectedLiveDevice {
        physical_device: best.physical_device,
        runtime: VulkanRuntimeCapabilityProbe {
            capability: best.capability,
            swapchain,
        },
    })
}

struct SelectedLiveDevice {
    physical_device: vk::PhysicalDevice,
    runtime: VulkanRuntimeCapabilityProbe,
}

struct LiveDeviceCandidate {
    physical_device: vk::PhysicalDevice,
    capability: VulkanCapabilityReport,
    surface_formats: Vec<VulkanSurfaceFormat>,
    present_modes: Vec<i32>,
    surface_capabilities: VulkanSwapchainSurfaceCapabilities,
}

fn live_device_candidate(
    instance: &VulkanInstanceProbe,
    surface: &VulkanSurfaceProbe,
    device: vk::PhysicalDevice,
    index: usize,
) -> Result<LiveDeviceCandidate, VulkanRuntimeCapabilityError> {
    let properties = {
        // SAFETY: `device` was returned by this live instance and the result is copied by value.
        unsafe { instance.instance.get_physical_device_properties(device) }
    };
    let name = physical_device_name(&properties, index);
    let queue_properties = {
        // SAFETY: `device` was returned by this live instance and the result is owned by Rust.
        unsafe {
            instance
                .instance
                .get_physical_device_queue_family_properties(device)
        }
    };
    let extensions = live_device_extensions(instance, device, &name)?;
    let surface_formats = live_surface_formats(surface, device, &name)?;
    let present_modes = live_present_modes(surface, device, &name)?;
    let surface_capabilities = live_surface_capabilities(surface, device, &name)?;
    let queue_families = queue_properties
        .iter()
        .enumerate()
        .map(|(queue_index, properties)| {
            let index = u32::try_from(queue_index).unwrap_or(u32::MAX);
            let present = {
                // SAFETY: The physical device, surface and queue-family index are live query inputs.
                unsafe {
                    surface.loader.get_physical_device_surface_support(
                        device,
                        index,
                        surface.surface,
                    )
                }
            }
            .map_err(|error| VulkanRuntimeCapabilityError::PresentSupportFailed {
                device: name.clone(),
                queue_family: index,
                result: error,
            })?;
            Ok(VulkanQueueFamily {
                index,
                graphics: properties.queue_flags.contains(vk::QueueFlags::GRAPHICS),
                present,
            })
        })
        .collect::<Result<Vec<_>, VulkanRuntimeCapabilityError>>()?;
    let record = VulkanPhysicalDeviceRecord {
        name,
        api_version: properties.api_version,
        device_type: match properties.device_type {
            vk::PhysicalDeviceType::DISCRETE_GPU => VulkanDeviceType::DiscreteGpu,
            vk::PhysicalDeviceType::INTEGRATED_GPU => VulkanDeviceType::IntegratedGpu,
            vk::PhysicalDeviceType::CPU => VulkanDeviceType::Cpu,
            _ => VulkanDeviceType::Other,
        },
        extensions,
        queue_families,
        surface_formats: surface_formats.clone(),
        present_modes: present_modes.clone(),
        surface_capabilities,
    };
    let capability = validate_device(&record).map_err(VulkanRuntimeCapabilityError::Capability)?;
    Ok(LiveDeviceCandidate {
        physical_device: device,
        capability,
        surface_formats,
        present_modes,
        surface_capabilities,
    })
}

fn unique_queue_families(graphics: u32, present: u32) -> Vec<u32> {
    if graphics == present {
        vec![graphics]
    } else {
        vec![graphics, present]
    }
}

fn device_extension_cstrings(values: &[String]) -> Result<Vec<CString>, String> {
    values
        .iter()
        .map(|extension| CString::new(extension.as_str()).map_err(|_| extension.clone()))
        .collect()
}

fn physical_device_name(properties: &vk::PhysicalDeviceProperties, index: usize) -> String {
    // SAFETY: Vulkan device names are fixed-size NUL-terminated C strings per the spec.
    let name = unsafe { CStr::from_ptr(properties.device_name.as_ptr()) }
        .to_string_lossy()
        .trim()
        .to_string();
    if name.is_empty() {
        format!("physical-device-{index}")
    } else {
        name
    }
}

fn live_device_extensions(
    instance: &VulkanInstanceProbe,
    device: vk::PhysicalDevice,
    name: &str,
) -> Result<Vec<String>, VulkanRuntimeCapabilityError> {
    let properties = {
        // SAFETY: `device` was returned by this live instance and no borrowed data escapes.
        unsafe {
            instance
                .instance
                .enumerate_device_extension_properties(device)
        }
    }
    .map_err(
        |error| VulkanRuntimeCapabilityError::EnumerateDeviceExtensionsFailed {
            device: name.to_string(),
            result: error,
        },
    )?;
    let mut extensions = properties
        .iter()
        .map(|property| {
            // SAFETY: Vulkan extension names are fixed-size NUL-terminated C strings per the spec.
            unsafe { CStr::from_ptr(property.extension_name.as_ptr()) }
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    extensions.sort();
    extensions.dedup();
    Ok(extensions)
}

fn live_surface_formats(
    surface: &VulkanSurfaceProbe,
    device: vk::PhysicalDevice,
    name: &str,
) -> Result<Vec<VulkanSurfaceFormat>, VulkanRuntimeCapabilityError> {
    let formats = {
        // SAFETY: The physical device and surface are live query inputs and no handles are retained.
        unsafe {
            surface
                .loader
                .get_physical_device_surface_formats(device, surface.surface)
        }
    }
    .map_err(|error| VulkanRuntimeCapabilityError::SurfaceFormatsFailed {
        device: name.to_string(),
        result: error,
    })?;
    Ok(formats
        .into_iter()
        .map(|format| VulkanSurfaceFormat {
            format: format.format.as_raw(),
            color_space: format.color_space.as_raw(),
        })
        .collect())
}

fn live_present_modes(
    surface: &VulkanSurfaceProbe,
    device: vk::PhysicalDevice,
    name: &str,
) -> Result<Vec<i32>, VulkanRuntimeCapabilityError> {
    let modes = {
        // SAFETY: The physical device and surface are live query inputs and no handles are retained.
        unsafe {
            surface
                .loader
                .get_physical_device_surface_present_modes(device, surface.surface)
        }
    }
    .map_err(|error| VulkanRuntimeCapabilityError::PresentModesFailed {
        device: name.to_string(),
        result: error,
    })?;
    Ok(modes.into_iter().map(vk::PresentModeKHR::as_raw).collect())
}

fn live_surface_capabilities(
    surface: &VulkanSurfaceProbe,
    device: vk::PhysicalDevice,
    name: &str,
) -> Result<VulkanSwapchainSurfaceCapabilities, VulkanRuntimeCapabilityError> {
    let capabilities = {
        // SAFETY: The physical device and surface are live query inputs and no handles are retained.
        unsafe {
            surface
                .loader
                .get_physical_device_surface_capabilities(device, surface.surface)
        }
    }
    .map_err(
        |error| VulkanRuntimeCapabilityError::SurfaceCapabilitiesFailed {
            device: name.to_string(),
            result: error,
        },
    )?;
    Ok(VulkanSwapchainSurfaceCapabilities {
        current_extent: if capabilities.current_extent.width == u32::MAX {
            None
        } else {
            Some((
                capabilities.current_extent.width,
                capabilities.current_extent.height,
            ))
        },
        min_extent: (
            capabilities.min_image_extent.width,
            capabilities.min_image_extent.height,
        ),
        max_extent: (
            capabilities.max_image_extent.width,
            capabilities.max_image_extent.height,
        ),
        min_image_count: capabilities.min_image_count,
        max_image_count: capabilities.max_image_count,
        supported_usage_flags: capabilities.supported_usage_flags.as_raw(),
    })
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
