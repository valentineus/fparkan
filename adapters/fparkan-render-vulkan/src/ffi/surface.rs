#![allow(unsafe_code)]

use ash::{khr::surface, vk};
use fparkan_platform::NativeWindowHandles;
use serde::Serialize;
use std::ffi::CStr;
use std::os::raw::c_char;

use super::VulkanInstanceProbe;
use crate::policy::serialize_json_or_fallback;

/// Deterministic Vulkan surface creation plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VulkanSurfacePlan {
    /// Report schema version.
    pub schema: u32,
    /// Instance extensions required by the native display backend.
    pub required_instance_extensions: Vec<String>,
}

/// Vulkan surface bootstrap error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VulkanSurfaceError {
    /// No native raw window/display handles were available.
    MissingNativeHandles,
    /// Required platform surface extensions could not be enumerated.
    RequiredExtensionsFailed {
        /// Vulkan result.
        result: vk::Result,
    },
    /// A required extension pointer was not valid UTF-8.
    InvalidExtensionName,
    /// Surface creation failed.
    CreateFailed {
        /// Vulkan result.
        result: vk::Result,
    },
}

impl std::fmt::Display for VulkanSurfaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingNativeHandles => {
                write!(
                    f,
                    "native window/display handles are required for Vulkan surface creation"
                )
            }
            Self::RequiredExtensionsFailed { result } => write!(
                f,
                "failed to enumerate required Vulkan surface extensions: {result:?}"
            ),
            Self::InvalidExtensionName => {
                write!(f, "Vulkan surface extension name is not valid UTF-8")
            }
            Self::CreateFailed { result } => {
                write!(f, "Vulkan surface creation failed: {result:?}")
            }
        }
    }
}

impl std::error::Error for VulkanSurfaceError {}

/// Created Vulkan surface probe.
pub struct VulkanSurfaceProbe {
    pub(super) loader: surface::Instance,
    pub(super) surface: vk::SurfaceKHR,
    /// Deterministic surface creation report.
    pub report: VulkanSurfacePlan,
}

impl Drop for VulkanSurfaceProbe {
    fn drop(&mut self) {
        // SAFETY: The `SurfaceKHR` was created by this probe and is destroyed once during drop.
        unsafe { self.loader.destroy_surface(self.surface, None) };
    }
}

/// Builds a deterministic Vulkan surface plan from native window handles.
///
/// # Errors
///
/// Returns [`VulkanSurfaceError`] when no native handles exist or the platform
/// display backend has no Vulkan surface extension mapping.
pub fn plan_vulkan_surface(
    handles: Option<NativeWindowHandles>,
) -> Result<VulkanSurfacePlan, VulkanSurfaceError> {
    let handles = handles.ok_or(VulkanSurfaceError::MissingNativeHandles)?;
    let required = ash_window::enumerate_required_extensions(handles.display)
        .map_err(|error| VulkanSurfaceError::RequiredExtensionsFailed { result: error })?;
    let mut required_instance_extensions = Vec::with_capacity(required.len());
    for extension in required {
        let name = extension_name(*extension)?;
        required_instance_extensions.push(name);
    }
    required_instance_extensions.sort();
    required_instance_extensions.dedup();
    Ok(VulkanSurfacePlan {
        schema: 1,
        required_instance_extensions,
    })
}

/// Creates a Vulkan surface probe from native window handles.
///
/// # Errors
///
/// Returns [`VulkanSurfaceError`] when handles are missing, required extensions
/// cannot be planned, or `vkCreate*SurfaceKHR` fails.
pub fn create_vulkan_surface_probe(
    instance: &VulkanInstanceProbe,
    handles: Option<NativeWindowHandles>,
) -> Result<VulkanSurfaceProbe, VulkanSurfaceError> {
    let handles = handles.ok_or(VulkanSurfaceError::MissingNativeHandles)?;
    let report = plan_vulkan_surface(Some(handles))?;
    // SAFETY: The platform handles are only used to create a child surface owned by this probe.
    let surface = unsafe {
        ash_window::create_surface(
            &instance.entry,
            &instance.instance,
            handles.display,
            handles.window,
            None,
        )
    }
    .map_err(|error| VulkanSurfaceError::CreateFailed { result: error })?;
    Ok(VulkanSurfaceProbe {
        loader: surface::Instance::new(&instance.entry, &instance.instance),
        surface,
        report,
    })
}

/// Renders a deterministic JSON Vulkan surface plan.
#[must_use]
pub fn render_surface_plan_json(plan: &VulkanSurfacePlan) -> String {
    #[derive(Serialize)]
    struct SurfacePlanJson<'a> {
        schema: u32,
        required_instance_extensions: &'a [String],
    }

    serialize_json_or_fallback(
        &SurfacePlanJson {
            schema: plan.schema,
            required_instance_extensions: &plan.required_instance_extensions,
        },
        "{\"schema\":0,\"required_instance_extensions\":[]}",
    )
}

pub(super) fn extension_name(extension: *const c_char) -> Result<String, VulkanSurfaceError> {
    // SAFETY: `ash-window` returns extension pointers to static NUL-terminated Vulkan names.
    let name = unsafe { CStr::from_ptr(extension) };
    name.to_str()
        .map(str::to_string)
        .map_err(|_| VulkanSurfaceError::InvalidExtensionName)
}
