use ash::vk;
use fparkan_platform::NativeWindowHandles;

use super::{
    VulkanAllocatedBuffer, VulkanFrameSync, VulkanInstanceError, VulkanInstanceProbe,
    VulkanLogicalDeviceError, VulkanLogicalDeviceProbe, VulkanSurfaceError, VulkanSurfaceProbe,
    VulkanSwapchainProbe, VulkanSwapchainProbeError, VulkanSwapchainResources,
    VulkanValidationMessenger,
};
use crate::shader_manifest::VulkanShaderManifestError;

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
    /// Whether the logical device enabled `VK_KHR_portability_subset`.
    pub portability_subset_enabled: bool,
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
    pub(super) instance: Option<VulkanInstanceProbe>,
    pub(super) validation: Option<VulkanValidationMessenger>,
    pub(super) surface: Option<VulkanSurfaceProbe>,
    pub(super) device: Option<VulkanLogicalDeviceProbe>,
    pub(super) swapchain: Option<VulkanSwapchainProbe>,
    pub(super) command_pool: vk::CommandPool,
    pub(super) swapchain_resources: Option<VulkanSwapchainResources>,
    pub(super) vertex_buffer: Option<VulkanAllocatedBuffer>,
    pub(super) index_buffer: Option<VulkanAllocatedBuffer>,
    pub(super) frame_sync: Vec<VulkanFrameSync>,
    pub(super) images_in_flight: Vec<vk::Fence>,
    pub(super) current_frame: usize,
    pub(super) pending_extent: Option<(u32, u32)>,
    pub(super) swapchain_recreate_count: u32,
    pub(super) report: VulkanSmokeRendererReport,
}
