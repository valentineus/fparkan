#![allow(unsafe_code)]

use ash::{khr::swapchain, vk};

use super::{
    capabilities::{
        live_present_modes, live_surface_capabilities, live_surface_formats, unique_queue_families,
    },
    VulkanInstanceProbe, VulkanLogicalDeviceProbe, VulkanRuntimeCapabilityError,
    VulkanSurfaceProbe,
};
use crate::policy::{
    plan_vulkan_swapchain, select_composite_alpha, VulkanSwapchainError, VulkanSwapchainPlan,
    VulkanSwapchainRequest,
};

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
                .get_physical_device_surface_capabilities(device.physical_device(), surface.surface)
        }
    }
    .map_err(|error| VulkanSwapchainProbeError::SurfaceCapabilitiesFailed { result: error })?;
    let surface_formats = live_surface_formats(
        surface,
        device.physical_device(),
        &device.report.device_name,
    )
    .map_err(VulkanSwapchainProbeError::Runtime)?;
    let present_modes = live_present_modes(
        surface,
        device.physical_device(),
        &device.report.device_name,
    )
    .map_err(VulkanSwapchainProbeError::Runtime)?;
    let capabilities = live_surface_capabilities(
        surface,
        device.physical_device(),
        &device.report.device_name,
    )
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
    let loader = swapchain::Device::new(&instance.instance, device.device());
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
