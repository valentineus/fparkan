#![allow(unsafe_code)]

use ash::vk;
use fparkan_platform::RenderRequest;
use std::ffi::CString;

use super::capabilities::{
    select_live_device_candidate_for_request, unique_queue_families, VulkanRuntimeCapabilityError,
    VulkanRuntimeCapabilityProbe,
};
use super::{VulkanInstanceProbe, VulkanSurfaceProbe};

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

    /// Returns the selected physical device handle.
    #[must_use]
    pub fn physical_device(&self) -> vk::PhysicalDevice {
        self.physical_device
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
    create_vulkan_logical_device_probe_for_request(
        instance,
        surface,
        drawable_extent,
        RenderRequest::conservative(),
    )
}

/// Creates a Vulkan logical device for a specific Stage 0 render request.
///
/// # Errors
///
/// Returns [`VulkanLogicalDeviceError`] when runtime capability probing fails,
/// device extension names are invalid, or `vkCreateDevice` fails.
pub fn create_vulkan_logical_device_probe_for_request(
    instance: &VulkanInstanceProbe,
    surface: &VulkanSurfaceProbe,
    drawable_extent: (u32, u32),
    render_request: RenderRequest,
) -> Result<VulkanLogicalDeviceProbe, VulkanLogicalDeviceError> {
    let selected = select_live_device_candidate_for_request(
        instance,
        surface,
        drawable_extent,
        render_request,
    )
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

fn device_extension_cstrings(values: &[String]) -> Result<Vec<CString>, String> {
    values
        .iter()
        .map(|extension| CString::new(extension.as_str()).map_err(|_| extension.clone()))
        .collect()
}
