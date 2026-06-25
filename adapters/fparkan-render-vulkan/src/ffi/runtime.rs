#![allow(unsafe_code)]

use ash::vk;
use std::ffi::{CStr, CString};

use super::{VulkanInstanceProbe, VulkanSurfaceProbe};
use crate::policy::{
    compare_reports, plan_vulkan_swapchain, validate_device, VulkanCapabilityError,
    VulkanCapabilityReport, VulkanDeviceType, VulkanPhysicalDeviceRecord, VulkanQueueFamily,
    VulkanSurfaceFormat, VulkanSwapchainError, VulkanSwapchainPlan, VulkanSwapchainRequest,
    VulkanSwapchainSurfaceCapabilities,
};

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

pub(super) fn unique_queue_families(graphics: u32, present: u32) -> Vec<u32> {
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

pub(super) fn live_surface_formats(
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

pub(super) fn live_present_modes(
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

pub(super) fn live_surface_capabilities(
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
