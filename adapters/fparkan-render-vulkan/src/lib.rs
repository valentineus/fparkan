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

use ash::{khr::surface, vk};
use fparkan_binary::{sha256, sha256_hex};
use fparkan_platform::{NativeWindowHandles, RenderRequest};
use fparkan_render::{
    canonical_capture, validate_command_list, FrameOutput, RenderBackend, RenderCommand,
    RenderCommandList, RenderError,
};
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::time::{SystemTime, UNIX_EPOCH};

/// Minimum Vulkan API version accepted by the Stage 0 backend.
pub const MIN_VULKAN_API_VERSION: u32 = vk::API_VERSION_1_1;
const KHR_SWAPCHAIN_EXTENSION: &str = "VK_KHR_swapchain";
const KHR_PORTABILITY_SUBSET_EXTENSION: &str = "VK_KHR_portability_subset";
const KHR_PORTABILITY_ENUMERATION_EXTENSION: &str = "VK_KHR_portability_enumeration";
const SPIRV_MAGIC: u32 = 0x0723_0203;
const SPIRV_VERSION_1_0: u32 = 0x0001_0000;
const TRIANGLE_VERTEX_SHADER_WORDS: &[u32] = &[
    SPIRV_MAGIC,
    SPIRV_VERSION_1_0,
    0,
    8,
    0,
    0x0002_0011,
    1,
    0x0006_000F,
    0,
    4,
    0x6E69_616D,
    0,
];
const TRIANGLE_FRAGMENT_SHADER_WORDS: &[u32] = &[
    SPIRV_MAGIC,
    SPIRV_VERSION_1_0,
    0,
    8,
    0,
    0x0002_0011,
    1,
    0x0006_000F,
    4,
    4,
    0x6E69_616D,
    0,
];

/// Shader compiler/toolchain identifiers pinned in the Stage 0 manifest.
pub const SHADER_COMPILER_ID: &str = "shaderc-offline-stage0@pinned-manifest";
/// SPIR-V validator identifier pinned in the Stage 0 manifest.
pub const SPIRV_VALIDATOR_ID: &str = "spirv-val-stage0@pinned-manifest";

/// Vulkan shader stage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VulkanShaderStage {
    /// Vertex stage.
    Vertex,
    /// Fragment stage.
    Fragment,
}

impl VulkanShaderStage {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Vertex => "vertex",
            Self::Fragment => "fragment",
        }
    }
}

/// Offline SPIR-V shader manifest entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VulkanShaderModuleManifest {
    /// Logical shader name.
    pub name: &'static str,
    /// Shader stage.
    pub stage: VulkanShaderStage,
    /// SPIR-V entry point.
    pub entry_point: &'static str,
    /// Descriptor set count.
    pub descriptor_sets: u32,
    /// Push constant byte count.
    pub push_constant_bytes: u32,
    /// SPIR-V words.
    pub words: &'static [u32],
}

/// Shader manifest validation report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VulkanShaderManifestReport {
    /// Report schema version.
    pub schema: u32,
    /// Pinned compiler identifier.
    pub compiler: &'static str,
    /// Pinned validator identifier.
    pub validator: &'static str,
    /// Shader module reports.
    pub modules: Vec<VulkanShaderModuleReport>,
    /// Hash of the normalized shader manifest.
    pub manifest_hash: String,
}

/// Shader module validation report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VulkanShaderModuleReport {
    /// Logical shader name.
    pub name: &'static str,
    /// Shader stage.
    pub stage: VulkanShaderStage,
    /// SPIR-V entry point.
    pub entry_point: &'static str,
    /// SPIR-V word count.
    pub word_count: usize,
    /// SPIR-V byte hash.
    pub sha256: String,
}

/// Shader manifest validation error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VulkanShaderManifestError {
    /// SPIR-V module is too short to contain a header.
    TooShort {
        /// Shader name.
        name: &'static str,
    },
    /// SPIR-V module has an invalid magic word.
    InvalidMagic {
        /// Shader name.
        name: &'static str,
        /// Found magic word.
        found: u32,
    },
    /// SPIR-V module version is below 1.0.
    UnsupportedVersion {
        /// Shader name.
        name: &'static str,
        /// Found version word.
        found: u32,
    },
    /// SPIR-V module declares an invalid bound.
    InvalidBound {
        /// Shader name.
        name: &'static str,
    },
}

impl std::fmt::Display for VulkanShaderManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooShort { name } => write!(f, "shader {name} SPIR-V module is too short"),
            Self::InvalidMagic { name, found } => {
                write!(f, "shader {name} has invalid SPIR-V magic 0x{found:08x}")
            }
            Self::UnsupportedVersion { name, found } => write!(
                f,
                "shader {name} has unsupported SPIR-V version 0x{found:08x}"
            ),
            Self::InvalidBound { name } => write!(f, "shader {name} has invalid SPIR-V bound"),
        }
    }
}

impl std::error::Error for VulkanShaderManifestError {}

/// Vulkan instance bootstrap configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VulkanInstanceConfig {
    /// Application name reported to the loader.
    pub application_name: String,
    /// Required instance extensions, usually including surface extensions.
    pub required_extensions: Vec<String>,
    /// Whether `VK_KHR_portability_enumeration` and its create flag are enabled.
    pub enable_portability_enumeration: bool,
    /// Whether validation layers are requested.
    pub enable_validation: bool,
}

impl VulkanInstanceConfig {
    /// Returns a conservative instance configuration for smoke probes.
    #[must_use]
    pub fn smoke(application_name: impl Into<String>) -> Self {
        Self {
            application_name: application_name.into(),
            required_extensions: Vec::new(),
            enable_portability_enumeration: cfg!(target_os = "macos"),
            enable_validation: false,
        }
    }
}

/// Deterministic Vulkan instance creation plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VulkanInstancePlan {
    /// Report schema version.
    pub schema: u32,
    /// Instance extensions requested at creation time.
    pub enabled_extensions: Vec<String>,
    /// Raw Vulkan instance creation flags.
    pub create_flags: u32,
    /// Whether validation was requested.
    pub validation_requested: bool,
}

/// Created Vulkan instance probe.
pub struct VulkanInstanceProbe {
    entry: ash::Entry,
    instance: ash::Instance,
    /// Deterministic instance creation report.
    pub report: VulkanInstancePlan,
}

impl Drop for VulkanInstanceProbe {
    fn drop(&mut self) {
        // SAFETY: The `Instance` was created by this probe and is destroyed once during drop.
        unsafe { self.instance.destroy_instance(None) };
    }
}

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
        result: String,
    },
    /// A required extension pointer was not valid UTF-8.
    InvalidExtensionName,
    /// Surface creation failed.
    CreateFailed {
        /// Vulkan result.
        result: String,
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
                "failed to enumerate required Vulkan surface extensions: {result}"
            ),
            Self::InvalidExtensionName => {
                write!(f, "Vulkan surface extension name is not valid UTF-8")
            }
            Self::CreateFailed { result } => write!(f, "Vulkan surface creation failed: {result}"),
        }
    }
}

impl std::error::Error for VulkanSurfaceError {}

/// Created Vulkan surface probe.
pub struct VulkanSurfaceProbe {
    loader: surface::Instance,
    surface: vk::SurfaceKHR,
    /// Deterministic surface creation report.
    pub report: VulkanSurfacePlan,
}

impl Drop for VulkanSurfaceProbe {
    fn drop(&mut self) {
        // SAFETY: The `SurfaceKHR` was created by this probe and is destroyed once during drop.
        unsafe { self.loader.destroy_surface(self.surface, None) };
    }
}

/// Live Vulkan device/surface capability probe.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VulkanRuntimeCapabilityProbe {
    /// Selected device/queue capability report.
    pub capability: VulkanCapabilityReport,
    /// Swapchain plan built from the selected device and live surface capabilities.
    pub swapchain: VulkanSwapchainPlan,
}

/// Live Vulkan device/surface capability probe error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VulkanRuntimeCapabilityError {
    /// Physical device enumeration failed.
    EnumerateDevicesFailed {
        /// Vulkan result.
        result: String,
    },
    /// Device extension enumeration failed.
    EnumerateDeviceExtensionsFailed {
        /// Device name or index context.
        device: String,
        /// Vulkan result.
        result: String,
    },
    /// Queue-family present support query failed.
    PresentSupportFailed {
        /// Device name.
        device: String,
        /// Queue-family index.
        queue_family: u32,
        /// Vulkan result.
        result: String,
    },
    /// Surface format query failed.
    SurfaceFormatsFailed {
        /// Device name.
        device: String,
        /// Vulkan result.
        result: String,
    },
    /// Surface capability query failed.
    SurfaceCapabilitiesFailed {
        /// Device name.
        device: String,
        /// Vulkan result.
        result: String,
    },
    /// Present mode query failed.
    PresentModesFailed {
        /// Device name.
        device: String,
        /// Vulkan result.
        result: String,
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
                write!(f, "Vulkan physical device enumeration failed: {result}")
            }
            Self::EnumerateDeviceExtensionsFailed { device, result } => write!(
                f,
                "Vulkan device {device} extension enumeration failed: {result}"
            ),
            Self::PresentSupportFailed {
                device,
                queue_family,
                result,
            } => write!(
                f,
                "Vulkan device {device} queue family {queue_family} present support query failed: {result}"
            ),
            Self::SurfaceFormatsFailed { device, result } => write!(
                f,
                "Vulkan device {device} surface format query failed: {result}"
            ),
            Self::SurfaceCapabilitiesFailed { device, result } => write!(
                f,
                "Vulkan device {device} surface capabilities query failed: {result}"
            ),
            Self::PresentModesFailed { device, result } => write!(
                f,
                "Vulkan device {device} present mode query failed: {result}"
            ),
            Self::Capability(error) => write!(f, "{error}"),
            Self::Swapchain(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for VulkanRuntimeCapabilityError {}

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
    let required = ash_window::enumerate_required_extensions(handles.display).map_err(|error| {
        VulkanSurfaceError::RequiredExtensionsFailed {
            result: format!("{error:?}"),
        }
    })?;
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
    .map_err(|error| VulkanSurfaceError::CreateFailed {
        result: format!("{error:?}"),
    })?;
    Ok(VulkanSurfaceProbe {
        loader: surface::Instance::new(&instance.entry, &instance.instance),
        surface,
        report,
    })
}

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
    let devices = {
        // SAFETY: The Vulkan instance is live for this query and no handles are retained.
        unsafe { instance.instance.enumerate_physical_devices() }.map_err(|error| {
            VulkanRuntimeCapabilityError::EnumerateDevicesFailed {
                result: format!("{error:?}"),
            }
        })?
    };
    let mut best: Option<LiveDeviceCandidate> = None;
    for (index, device) in devices.iter().copied().enumerate() {
        let candidate = live_device_candidate(instance, surface, device, index)?;
        match &best {
            Some(existing)
                if compare_reports(&candidate.capability, &existing.capability)
                    != std::cmp::Ordering::Greater => {}
            _ => best = Some(candidate),
        }
    }
    let best = best.ok_or(VulkanRuntimeCapabilityError::Capability(
        VulkanCapabilityError::NoPhysicalDevice,
    ))?;
    let swapchain = plan_vulkan_swapchain(&VulkanSwapchainRequest {
        drawable_extent,
        formats: best.surface_formats,
        present_modes: best.present_modes,
        capabilities: best.surface_capabilities,
        preferred_present_mode: vk::PresentModeKHR::MAILBOX.as_raw(),
    })
    .map_err(VulkanRuntimeCapabilityError::Swapchain)?;
    Ok(VulkanRuntimeCapabilityProbe {
        capability: best.capability,
        swapchain,
    })
}

struct LiveDeviceCandidate {
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
                result: format!("{error:?}"),
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
    };
    let capability = validate_device(&record).map_err(VulkanRuntimeCapabilityError::Capability)?;
    Ok(LiveDeviceCandidate {
        capability,
        surface_formats,
        present_modes,
        surface_capabilities,
    })
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
            result: format!("{error:?}"),
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
        result: format!("{error:?}"),
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
        result: format!("{error:?}"),
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
            result: format!("{error:?}"),
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
    })
}

/// Renders a deterministic JSON Vulkan surface plan.
#[must_use]
pub fn render_surface_plan_json(plan: &VulkanSurfacePlan) -> String {
    let mut out = String::new();
    out.push_str("{\"schema\":");
    out.push_str(&plan.schema.to_string());
    out.push_str(",\"required_instance_extensions\":[");
    for (index, extension) in plan.required_instance_extensions.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        push_json_string(&mut out, extension);
    }
    out.push_str("]}");
    out
}

fn extension_name(extension: *const c_char) -> Result<String, VulkanSurfaceError> {
    // SAFETY: `ash-window` returns extension pointers to static NUL-terminated Vulkan names.
    let name = unsafe { CStr::from_ptr(extension) };
    name.to_str()
        .map(str::to_string)
        .map_err(|_| VulkanSurfaceError::InvalidExtensionName)
}

/// Vulkan instance bootstrap error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VulkanInstanceError {
    /// The Vulkan loader could not be opened.
    Loader(VulkanLoaderError),
    /// Application name contained an interior NUL byte.
    InvalidApplicationName,
    /// An extension name contained an interior NUL byte.
    InvalidExtensionName {
        /// Invalid extension name.
        extension: String,
    },
    /// Instance creation failed.
    CreateFailed {
        /// Vulkan result.
        result: String,
    },
}

impl std::fmt::Display for VulkanInstanceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Loader(error) => write!(f, "{error}"),
            Self::InvalidApplicationName => {
                write!(f, "Vulkan application name contains an interior NUL byte")
            }
            Self::InvalidExtensionName { extension } => {
                write!(
                    f,
                    "Vulkan instance extension name contains an interior NUL byte: {extension:?}"
                )
            }
            Self::CreateFailed { result } => write!(f, "Vulkan instance creation failed: {result}"),
        }
    }
}

impl std::error::Error for VulkanInstanceError {}

/// Builds the deterministic instance creation plan without touching the loader.
#[must_use]
pub fn plan_vulkan_instance(config: &VulkanInstanceConfig) -> VulkanInstancePlan {
    let mut enabled_extensions = config.required_extensions.clone();
    if config.enable_portability_enumeration
        && !enabled_extensions
            .iter()
            .any(|extension| extension == KHR_PORTABILITY_ENUMERATION_EXTENSION)
    {
        enabled_extensions.push(KHR_PORTABILITY_ENUMERATION_EXTENSION.to_string());
    }
    enabled_extensions.sort();
    enabled_extensions.dedup();
    VulkanInstancePlan {
        schema: 1,
        enabled_extensions,
        create_flags: if config.enable_portability_enumeration {
            vk::InstanceCreateFlags::ENUMERATE_PORTABILITY_KHR.as_raw()
        } else {
            0
        },
        validation_requested: config.enable_validation,
    }
}

/// Creates a Vulkan instance probe from the supplied configuration.
///
/// # Errors
///
/// Returns [`VulkanInstanceError`] when the loader is unavailable, names are not
/// valid C strings, or `vkCreateInstance` fails.
pub fn create_vulkan_instance_probe(
    config: &VulkanInstanceConfig,
) -> Result<VulkanInstanceProbe, VulkanInstanceError> {
    // SAFETY: Loading the entry only resolves loader symbols; no raw Vulkan handles escape.
    let entry = unsafe { ash::Entry::load() }.map_err(|error| {
        VulkanInstanceError::Loader(VulkanLoaderError::Unavailable {
            message: error.to_string(),
        })
    })?;
    let app_name = CString::new(config.application_name.clone())
        .map_err(|_| VulkanInstanceError::InvalidApplicationName)?;
    let engine_name = c"fparkan";
    let plan = plan_vulkan_instance(config);
    let extension_names = cstring_vec(&plan.enabled_extensions)?;
    let extension_ptrs = cstring_ptrs(&extension_names);
    let app_info = vk::ApplicationInfo::default()
        .application_name(&app_name)
        .application_version(0)
        .engine_name(engine_name)
        .engine_version(0)
        .api_version(MIN_VULKAN_API_VERSION);
    let create_info = vk::InstanceCreateInfo::default()
        .application_info(&app_info)
        .enabled_extension_names(&extension_ptrs)
        .flags(vk::InstanceCreateFlags::from_raw(plan.create_flags));
    // SAFETY: `create_info` points to stack-owned Vulkan create data that lives for the call.
    let instance = unsafe { entry.create_instance(&create_info, None) }.map_err(|error| {
        VulkanInstanceError::CreateFailed {
            result: format!("{error:?}"),
        }
    })?;
    Ok(VulkanInstanceProbe {
        entry,
        instance,
        report: plan,
    })
}

/// Renders a deterministic JSON Vulkan instance plan.
#[must_use]
pub fn render_instance_plan_json(plan: &VulkanInstancePlan) -> String {
    let mut out = String::new();
    out.push_str("{\"schema\":");
    out.push_str(&plan.schema.to_string());
    out.push_str(",\"create_flags\":");
    out.push_str(&plan.create_flags.to_string());
    out.push_str(",\"validation_requested\":");
    out.push_str(if plan.validation_requested {
        "true"
    } else {
        "false"
    });
    out.push_str(",\"enabled_extensions\":[");
    for (index, extension) in plan.enabled_extensions.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        push_json_string(&mut out, extension);
    }
    out.push_str("]}");
    out
}

fn cstring_vec(values: &[String]) -> Result<Vec<CString>, VulkanInstanceError> {
    values
        .iter()
        .map(|extension| {
            CString::new(extension.as_str()).map_err(|_| {
                VulkanInstanceError::InvalidExtensionName {
                    extension: extension.clone(),
                }
            })
        })
        .collect()
}

fn cstring_ptrs(values: &[CString]) -> Vec<*const c_char> {
    values.iter().map(|value| value.as_ptr()).collect()
}

/// Deterministic Vulkan loader probe report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VulkanLoaderProbeReport {
    /// Report schema version.
    pub schema: u32,
    /// Whether the Vulkan loader was opened successfully.
    pub loader_available: bool,
    /// Reported loader instance API version.
    pub instance_api_version: u32,
}

/// Vulkan loader bootstrap error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VulkanLoaderError {
    /// The Vulkan loader library could not be opened.
    Unavailable {
        /// Loader error text.
        message: String,
    },
}

impl std::fmt::Display for VulkanLoaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable { message } => {
                write!(f, "Vulkan loader is unavailable: {message}")
            }
        }
    }
}

impl std::error::Error for VulkanLoaderError {}

/// Opens the Vulkan loader and reports the supported instance API version.
///
/// # Errors
///
/// Returns [`VulkanLoaderError`] when no Vulkan loader library can be opened on
/// the host.
pub fn probe_vulkan_loader() -> Result<VulkanLoaderProbeReport, VulkanLoaderError> {
    // SAFETY: Loading the entry only resolves loader symbols; no raw Vulkan handles escape.
    let entry = unsafe { ash::Entry::load() }.map_err(|error| VulkanLoaderError::Unavailable {
        message: error.to_string(),
    })?;
    // SAFETY: The resolved entry only queries the loader-supported instance API version.
    let version = unsafe { entry.try_enumerate_instance_version() }
        .map_err(|error| VulkanLoaderError::Unavailable {
            message: error.to_string(),
        })?
        .unwrap_or(vk::API_VERSION_1_0);
    Ok(VulkanLoaderProbeReport {
        schema: 1,
        loader_available: true,
        instance_api_version: version,
    })
}

/// Returns the static Vulkan entry name used by loader probes.
#[must_use]
pub fn vulkan_entry_symbol_name() -> &'static CStr {
    c"vkGetInstanceProcAddr"
}

/// Renders a deterministic JSON Vulkan loader report.
#[must_use]
pub fn render_loader_probe_report_json(report: &VulkanLoaderProbeReport) -> String {
    let mut out = String::new();
    out.push_str("{\"schema\":");
    out.push_str(&report.schema.to_string());
    out.push_str(",\"loader_available\":");
    out.push_str(if report.loader_available {
        "true"
    } else {
        "false"
    });
    out.push_str(",\"instance_api\":\"");
    out.push_str(&format_api_version(report.instance_api_version));
    out.push_str("\"}");
    out
}

/// Returns the built-in Stage 0 indexed-triangle shader manifest.
#[must_use]
pub fn triangle_shader_manifest() -> Vec<VulkanShaderModuleManifest> {
    vec![
        VulkanShaderModuleManifest {
            name: "triangle.vert",
            stage: VulkanShaderStage::Vertex,
            entry_point: "main",
            descriptor_sets: 0,
            push_constant_bytes: 0,
            words: TRIANGLE_VERTEX_SHADER_WORDS,
        },
        VulkanShaderModuleManifest {
            name: "triangle.frag",
            stage: VulkanShaderStage::Fragment,
            entry_point: "main",
            descriptor_sets: 0,
            push_constant_bytes: 0,
            words: TRIANGLE_FRAGMENT_SHADER_WORDS,
        },
    ]
}

/// Validates shader SPIR-V containers and renders a deterministic report.
///
/// # Errors
///
/// Returns [`VulkanShaderManifestError`] when a module fails Stage 0 SPIR-V
/// container validation.
pub fn validate_shader_manifest(
    modules: &[VulkanShaderModuleManifest],
) -> Result<VulkanShaderManifestReport, VulkanShaderManifestError> {
    let mut reports = Vec::with_capacity(modules.len());
    for module in modules {
        validate_spirv_container(module)?;
        let bytes = spirv_words_to_bytes(module.words);
        reports.push(VulkanShaderModuleReport {
            name: module.name,
            stage: module.stage,
            entry_point: module.entry_point,
            word_count: module.words.len(),
            sha256: sha256_hex(&sha256(&bytes)),
        });
    }
    let normalized = render_shader_modules_json(&reports);
    Ok(VulkanShaderManifestReport {
        schema: 1,
        compiler: SHADER_COMPILER_ID,
        validator: SPIRV_VALIDATOR_ID,
        modules: reports,
        manifest_hash: sha256_hex(&sha256(normalized.as_bytes())),
    })
}

fn validate_spirv_container(
    module: &VulkanShaderModuleManifest,
) -> Result<(), VulkanShaderManifestError> {
    if module.words.len() < 5 {
        return Err(VulkanShaderManifestError::TooShort { name: module.name });
    }
    if module.words[0] != SPIRV_MAGIC {
        return Err(VulkanShaderManifestError::InvalidMagic {
            name: module.name,
            found: module.words[0],
        });
    }
    if module.words[1] < SPIRV_VERSION_1_0 {
        return Err(VulkanShaderManifestError::UnsupportedVersion {
            name: module.name,
            found: module.words[1],
        });
    }
    if module.words[3] == 0 {
        return Err(VulkanShaderManifestError::InvalidBound { name: module.name });
    }
    Ok(())
}

fn spirv_words_to_bytes(words: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(words.len() * 4);
    for word in words {
        out.extend_from_slice(&word.to_le_bytes());
    }
    out
}

/// Renders a deterministic JSON shader manifest report.
#[must_use]
pub fn render_shader_manifest_report_json(report: &VulkanShaderManifestReport) -> String {
    let mut out = String::new();
    out.push_str("{\"schema\":");
    out.push_str(&report.schema.to_string());
    out.push_str(",\"compiler\":");
    push_json_string(&mut out, report.compiler);
    out.push_str(",\"validator\":");
    push_json_string(&mut out, report.validator);
    out.push_str(",\"modules\":");
    out.push_str(&render_shader_modules_json(&report.modules));
    out.push_str(",\"manifest_hash\":");
    push_json_string(&mut out, &report.manifest_hash);
    out.push('}');
    out
}

fn render_shader_modules_json(modules: &[VulkanShaderModuleReport]) -> String {
    let mut out = String::new();
    out.push('[');
    for (index, module) in modules.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str("{\"name\":");
        push_json_string(&mut out, module.name);
        out.push_str(",\"stage\":\"");
        out.push_str(module.stage.as_str());
        out.push_str("\",\"entry_point\":");
        push_json_string(&mut out, module.entry_point);
        out.push_str(",\"word_count\":");
        out.push_str(&module.word_count.to_string());
        out.push_str(",\"sha256\":");
        push_json_string(&mut out, &module.sha256);
        out.push('}');
    }
    out.push(']');
    out
}

/// Vulkan backend migration readiness.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VulkanBackendState {
    /// Adapter prepared and able to accept commands.
    Ready,
    /// Adapter is tracking a recoverable runtime surface/depth pipeline fault.
    Degraded,
    /// Adapter has encountered a non-recoverable error.
    Error,
}

impl Default for VulkanBackendState {
    fn default() -> Self {
        Self::Degraded
    }
}

/// Synthetic physical-device type used by deterministic capability scoring.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VulkanDeviceType {
    /// Discrete GPU.
    DiscreteGpu,
    /// Integrated GPU.
    IntegratedGpu,
    /// CPU or software Vulkan implementation.
    Cpu,
    /// Other or unknown implementation.
    Other,
}

impl VulkanDeviceType {
    const fn score_bonus(self) -> i32 {
        match self {
            Self::DiscreteGpu => 1_000,
            Self::IntegratedGpu => 700,
            Self::Cpu => 100,
            Self::Other => 10,
        }
    }
}

/// Queue-family capabilities needed by the Stage 0 renderer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VulkanQueueFamily {
    /// Stable queue-family index.
    pub index: u32,
    /// Whether the family supports graphics commands.
    pub graphics: bool,
    /// Whether the family supports presentation for the target surface.
    pub present: bool,
}

/// Surface format capability needed by the Stage 0 swapchain policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VulkanSurfaceFormat {
    /// Vulkan format numeric value.
    pub format: i32,
    /// Vulkan color-space numeric value.
    pub color_space: i32,
}

/// Surface capabilities needed by the Stage 0 swapchain policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VulkanSwapchainSurfaceCapabilities {
    /// Current surface extent, when dictated by the platform.
    pub current_extent: Option<(u32, u32)>,
    /// Minimum supported swapchain extent.
    pub min_extent: (u32, u32),
    /// Maximum supported swapchain extent.
    pub max_extent: (u32, u32),
    /// Minimum supported image count.
    pub min_image_count: u32,
    /// Maximum supported image count, or 0 when unbounded.
    pub max_image_count: u32,
}

/// Deterministic swapchain planning input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VulkanSwapchainRequest {
    /// Requested drawable extent.
    pub drawable_extent: (u32, u32),
    /// Available surface formats.
    pub formats: Vec<VulkanSurfaceFormat>,
    /// Available present modes as raw Vulkan values.
    pub present_modes: Vec<i32>,
    /// Surface capabilities.
    pub capabilities: VulkanSwapchainSurfaceCapabilities,
    /// Preferred present mode.
    pub preferred_present_mode: i32,
}

/// Deterministic swapchain plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VulkanSwapchainPlan {
    /// Report schema version.
    pub schema: u32,
    /// Selected swapchain extent.
    pub extent: (u32, u32),
    /// Selected surface format.
    pub format: VulkanSurfaceFormat,
    /// Selected present mode raw Vulkan value.
    pub present_mode: i32,
    /// Selected image count.
    pub image_count: u32,
}

/// Swapchain planning error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VulkanSwapchainError {
    /// No surface format was available.
    MissingSurfaceFormat,
    /// No present mode was available.
    MissingPresentMode,
    /// Requested or current extent is empty.
    EmptyExtent,
}

impl std::fmt::Display for VulkanSwapchainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingSurfaceFormat => write!(f, "Vulkan swapchain has no surface format"),
            Self::MissingPresentMode => write!(f, "Vulkan swapchain has no present mode"),
            Self::EmptyExtent => write!(f, "Vulkan swapchain extent must be non-zero"),
        }
    }
}

impl std::error::Error for VulkanSwapchainError {}

/// Swapchain recreation reason.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VulkanSwapchainRecreationReason {
    /// Drawable extent changed.
    Resize,
    /// Vulkan reported `VK_ERROR_OUT_OF_DATE_KHR`.
    OutOfDate,
    /// Vulkan reported `VK_SUBOPTIMAL_KHR`.
    Suboptimal,
}

/// Deterministic swapchain recreation report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VulkanSwapchainRecreationReport {
    /// Report schema version.
    pub schema: u32,
    /// Recreation reason.
    pub reason: VulkanSwapchainRecreationReason,
    /// Previous extent.
    pub previous_extent: (u32, u32),
    /// Next extent.
    pub next_extent: (u32, u32),
}

/// Deterministic frame submission plan for command buffers and sync objects.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VulkanFrameSubmissionPlan {
    /// Report schema version.
    pub schema: u32,
    /// Frames allowed in flight.
    pub frames_in_flight: u32,
    /// Swapchain-backed primary command buffers.
    pub command_buffers: u32,
    /// Binary semaphores allocated per frame.
    pub semaphores_per_frame: u32,
    /// Fences allocated per frame.
    pub fences_per_frame: u32,
    /// Draw commands encoded into the frame.
    pub draw_count: u32,
    /// Total indexed vertices submitted by draw commands.
    pub indexed_vertex_count: u32,
}

/// Synthetic physical-device capabilities used by negative tests and reports.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VulkanPhysicalDeviceRecord {
    /// Human-readable device name.
    pub name: String,
    /// Reported Vulkan API version.
    pub api_version: u32,
    /// Device class.
    pub device_type: VulkanDeviceType,
    /// Supported device-extension names.
    pub extensions: Vec<String>,
    /// Queue-family capabilities.
    pub queue_families: Vec<VulkanQueueFamily>,
    /// Surface formats accepted by the target surface.
    pub surface_formats: Vec<VulkanSurfaceFormat>,
}

impl VulkanPhysicalDeviceRecord {
    /// Returns whether the device supports an extension name.
    #[must_use]
    pub fn supports_extension(&self, extension: &str) -> bool {
        self.extensions
            .iter()
            .any(|candidate| candidate == extension)
    }
}

/// Selected device and queue capability report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VulkanCapabilityReport {
    /// Report schema version.
    pub schema: u32,
    /// Selected device name.
    pub device_name: String,
    /// Selected Vulkan API version.
    pub vulkan_api_version: u32,
    /// Deterministic score used for device selection.
    pub score: i32,
    /// Graphics queue family index.
    pub graphics_queue_family: u32,
    /// Present queue family index.
    pub present_queue_family: u32,
    /// Whether portability subset is enabled for the selected device.
    pub portability_subset: bool,
    /// Enabled device extensions.
    pub enabled_extensions: Vec<String>,
}

/// Vulkan capability selection error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VulkanCapabilityError {
    /// No physical devices were available.
    NoPhysicalDevice,
    /// Device API version is lower than the Stage 0 minimum.
    ApiVersionTooLow {
        /// Required Vulkan API version.
        required: u32,
        /// Reported Vulkan API version.
        found: u32,
    },
    /// Required graphics queue is unavailable.
    NoGraphicsQueue {
        /// Device name that failed validation.
        device: String,
    },
    /// Required present queue is unavailable.
    NoPresentQueue {
        /// Device name that failed validation.
        device: String,
    },
    /// Swapchain device extension is unavailable.
    MissingSwapchainExtension {
        /// Device name that failed validation.
        device: String,
    },
    /// No compatible surface format exists.
    MissingSurfaceFormat {
        /// Device name that failed validation.
        device: String,
    },
}

impl std::fmt::Display for VulkanCapabilityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoPhysicalDevice => write!(f, "no Vulkan physical device available"),
            Self::ApiVersionTooLow { required, found } => write!(
                f,
                "Vulkan API version too low: required {}, found {}",
                format_api_version(*required),
                format_api_version(*found)
            ),
            Self::NoGraphicsQueue { device } => {
                write!(f, "Vulkan device {device} has no graphics queue")
            }
            Self::NoPresentQueue { device } => {
                write!(f, "Vulkan device {device} has no present queue")
            }
            Self::MissingSwapchainExtension { device } => {
                write!(f, "Vulkan device {device} lacks {KHR_SWAPCHAIN_EXTENSION}")
            }
            Self::MissingSurfaceFormat { device } => {
                write!(f, "Vulkan device {device} has no compatible surface format")
            }
        }
    }
}

impl std::error::Error for VulkanCapabilityError {}

/// Selects a Vulkan physical device using deterministic Stage 0 policy.
///
/// # Errors
///
/// Returns [`VulkanCapabilityError`] when no candidate satisfies the minimum
/// API version, queue, swapchain-extension and surface-format requirements.
pub fn select_physical_device(
    devices: &[VulkanPhysicalDeviceRecord],
) -> Result<VulkanCapabilityReport, VulkanCapabilityError> {
    if devices.is_empty() {
        return Err(VulkanCapabilityError::NoPhysicalDevice);
    }

    let mut best = None;
    for device in devices {
        let report = validate_device(device)?;
        match &best {
            Some(existing) if compare_reports(&report, existing) != std::cmp::Ordering::Greater => {
            }
            _ => best = Some(report),
        }
    }
    best.ok_or(VulkanCapabilityError::NoPhysicalDevice)
}

/// Builds a deterministic swapchain plan from surface capabilities.
///
/// # Errors
///
/// Returns [`VulkanSwapchainError`] when formats, present modes or extent are
/// unusable.
pub fn plan_vulkan_swapchain(
    request: &VulkanSwapchainRequest,
) -> Result<VulkanSwapchainPlan, VulkanSwapchainError> {
    let format = select_surface_format(&request.formats)?;
    let present_mode = select_present_mode(&request.present_modes, request.preferred_present_mode)?;
    let extent = select_swapchain_extent(request)?;
    if extent.0 == 0 || extent.1 == 0 {
        return Err(VulkanSwapchainError::EmptyExtent);
    }
    Ok(VulkanSwapchainPlan {
        schema: 1,
        extent,
        format,
        present_mode,
        image_count: select_image_count(request.capabilities),
    })
}

fn select_surface_format(
    formats: &[VulkanSurfaceFormat],
) -> Result<VulkanSurfaceFormat, VulkanSwapchainError> {
    formats
        .iter()
        .copied()
        .find(|format| {
            format.format == vk::Format::B8G8R8A8_SRGB.as_raw()
                && format.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR.as_raw()
        })
        .or_else(|| formats.first().copied())
        .ok_or(VulkanSwapchainError::MissingSurfaceFormat)
}

fn select_present_mode(present_modes: &[i32], preferred: i32) -> Result<i32, VulkanSwapchainError> {
    if present_modes.contains(&preferred) {
        Ok(preferred)
    } else if present_modes.contains(&vk::PresentModeKHR::FIFO.as_raw()) {
        Ok(vk::PresentModeKHR::FIFO.as_raw())
    } else {
        present_modes
            .first()
            .copied()
            .ok_or(VulkanSwapchainError::MissingPresentMode)
    }
}

fn select_swapchain_extent(
    request: &VulkanSwapchainRequest,
) -> Result<(u32, u32), VulkanSwapchainError> {
    if let Some(extent) = request.capabilities.current_extent {
        return if extent.0 == 0 || extent.1 == 0 {
            Err(VulkanSwapchainError::EmptyExtent)
        } else {
            Ok(extent)
        };
    }
    let width = request.drawable_extent.0.clamp(
        request.capabilities.min_extent.0,
        request.capabilities.max_extent.0,
    );
    let height = request.drawable_extent.1.clamp(
        request.capabilities.min_extent.1,
        request.capabilities.max_extent.1,
    );
    Ok((width, height))
}

fn select_image_count(capabilities: VulkanSwapchainSurfaceCapabilities) -> u32 {
    let requested = capabilities.min_image_count.saturating_add(1).max(2);
    if capabilities.max_image_count == 0 {
        requested
    } else {
        requested.min(capabilities.max_image_count)
    }
}

/// Builds a deterministic swapchain recreation report.
#[must_use]
pub const fn swapchain_recreation_report(
    reason: VulkanSwapchainRecreationReason,
    previous_extent: (u32, u32),
    next_extent: (u32, u32),
) -> VulkanSwapchainRecreationReport {
    VulkanSwapchainRecreationReport {
        schema: 1,
        reason,
        previous_extent,
        next_extent,
    }
}

/// Builds a deterministic frame submission plan for a validated command list.
///
/// Stage 0 keeps this as a pure planning boundary so command-pool, command-buffer
/// and synchronization policy can be tested without requiring a native surface.
///
/// # Errors
///
/// Returns [`RenderError`] when the command list has invalid frame framing,
/// ordering, draw ranges, mesh bounds, or non-finite transforms.
pub fn plan_vulkan_frame_submission(
    swapchain: &VulkanSwapchainPlan,
    commands: &RenderCommandList,
) -> Result<VulkanFrameSubmissionPlan, RenderError> {
    validate_command_list(commands)?;
    let mut draw_count = 0_u32;
    let mut indexed_vertex_count = 0_u32;
    for command in &commands.commands {
        if let RenderCommand::Draw(draw) = command {
            draw_count = draw_count.saturating_add(1);
            indexed_vertex_count = indexed_vertex_count.saturating_add(draw.range.count);
        }
    }
    Ok(VulkanFrameSubmissionPlan {
        schema: 1,
        frames_in_flight: swapchain.image_count.clamp(1, 2),
        command_buffers: swapchain.image_count,
        semaphores_per_frame: 2,
        fences_per_frame: 1,
        draw_count,
        indexed_vertex_count,
    })
}

fn validate_device(
    device: &VulkanPhysicalDeviceRecord,
) -> Result<VulkanCapabilityReport, VulkanCapabilityError> {
    if device.api_version < MIN_VULKAN_API_VERSION {
        return Err(VulkanCapabilityError::ApiVersionTooLow {
            required: MIN_VULKAN_API_VERSION,
            found: device.api_version,
        });
    }
    if !device.supports_extension(KHR_SWAPCHAIN_EXTENSION) {
        return Err(VulkanCapabilityError::MissingSwapchainExtension {
            device: device.name.clone(),
        });
    }
    if device.surface_formats.is_empty() {
        return Err(VulkanCapabilityError::MissingSurfaceFormat {
            device: device.name.clone(),
        });
    }
    let graphics_queue_family = device
        .queue_families
        .iter()
        .find(|family| family.graphics)
        .ok_or_else(|| VulkanCapabilityError::NoGraphicsQueue {
            device: device.name.clone(),
        })?
        .index;
    let present_queue_family = device
        .queue_families
        .iter()
        .find(|family| family.present)
        .ok_or_else(|| VulkanCapabilityError::NoPresentQueue {
            device: device.name.clone(),
        })?
        .index;

    let portability_subset = device.supports_extension(KHR_PORTABILITY_SUBSET_EXTENSION);
    let mut enabled_extensions = vec![KHR_SWAPCHAIN_EXTENSION.to_string()];
    if portability_subset {
        enabled_extensions.push(KHR_PORTABILITY_SUBSET_EXTENSION.to_string());
    }

    Ok(VulkanCapabilityReport {
        schema: 1,
        device_name: device.name.clone(),
        vulkan_api_version: device.api_version,
        score: score_device(device, graphics_queue_family, present_queue_family),
        graphics_queue_family,
        present_queue_family,
        portability_subset,
        enabled_extensions,
    })
}

fn score_device(
    device: &VulkanPhysicalDeviceRecord,
    graphics_queue_family: u32,
    present_queue_family: u32,
) -> i32 {
    let unified_queue_bonus = if graphics_queue_family == present_queue_family {
        100
    } else {
        0
    };
    let portability_penalty = if device.supports_extension(KHR_PORTABILITY_SUBSET_EXTENSION) {
        -50
    } else {
        0
    };
    device.device_type.score_bonus()
        + unified_queue_bonus
        + portability_penalty
        + i32::try_from(device.surface_formats.len()).unwrap_or(i32::MAX)
}

fn compare_reports(
    left: &VulkanCapabilityReport,
    right: &VulkanCapabilityReport,
) -> std::cmp::Ordering {
    left.score
        .cmp(&right.score)
        .then_with(|| right.device_name.cmp(&left.device_name))
}

/// Renders a deterministic JSON capability report.
#[must_use]
pub fn render_capability_report_json(report: &VulkanCapabilityReport) -> String {
    let mut out = String::new();
    out.push_str("{\"schema\":");
    out.push_str(&report.schema.to_string());
    out.push_str(",\"vulkan_api\":\"");
    out.push_str(&format_api_version(report.vulkan_api_version));
    out.push_str("\",\"device_name\":");
    push_json_string(&mut out, &report.device_name);
    out.push_str(",\"score\":");
    out.push_str(&report.score.to_string());
    out.push_str(",\"graphics_queue_family\":");
    out.push_str(&report.graphics_queue_family.to_string());
    out.push_str(",\"present_queue_family\":");
    out.push_str(&report.present_queue_family.to_string());
    out.push_str(",\"portability_subset\":");
    out.push_str(if report.portability_subset {
        "true"
    } else {
        "false"
    });
    out.push_str(",\"enabled_extensions\":[");
    for (index, extension) in report.enabled_extensions.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        push_json_string(&mut out, extension);
    }
    out.push_str("]}");
    out
}

/// Renders a deterministic JSON swapchain plan.
#[must_use]
pub fn render_swapchain_plan_json(plan: &VulkanSwapchainPlan) -> String {
    let mut out = String::new();
    out.push_str("{\"schema\":");
    out.push_str(&plan.schema.to_string());
    out.push_str(",\"extent\":[");
    out.push_str(&plan.extent.0.to_string());
    out.push(',');
    out.push_str(&plan.extent.1.to_string());
    out.push_str("],\"format\":");
    out.push_str(&plan.format.format.to_string());
    out.push_str(",\"color_space\":");
    out.push_str(&plan.format.color_space.to_string());
    out.push_str(",\"present_mode\":");
    out.push_str(&plan.present_mode.to_string());
    out.push_str(",\"image_count\":");
    out.push_str(&plan.image_count.to_string());
    out.push('}');
    out
}

/// Renders a deterministic JSON swapchain recreation report.
#[must_use]
pub fn render_swapchain_recreation_report_json(report: &VulkanSwapchainRecreationReport) -> String {
    let mut out = String::new();
    out.push_str("{\"schema\":");
    out.push_str(&report.schema.to_string());
    out.push_str(",\"reason\":\"");
    out.push_str(match report.reason {
        VulkanSwapchainRecreationReason::Resize => "resize",
        VulkanSwapchainRecreationReason::OutOfDate => "out_of_date",
        VulkanSwapchainRecreationReason::Suboptimal => "suboptimal",
    });
    out.push_str("\",\"previous_extent\":[");
    out.push_str(&report.previous_extent.0.to_string());
    out.push(',');
    out.push_str(&report.previous_extent.1.to_string());
    out.push_str("],\"next_extent\":[");
    out.push_str(&report.next_extent.0.to_string());
    out.push(',');
    out.push_str(&report.next_extent.1.to_string());
    out.push_str("]}");
    out
}

/// Renders a deterministic JSON frame submission plan.
#[must_use]
pub fn render_frame_submission_plan_json(plan: &VulkanFrameSubmissionPlan) -> String {
    let mut out = String::new();
    out.push_str("{\"schema\":");
    out.push_str(&plan.schema.to_string());
    out.push_str(",\"frames_in_flight\":");
    out.push_str(&plan.frames_in_flight.to_string());
    out.push_str(",\"command_buffers\":");
    out.push_str(&plan.command_buffers.to_string());
    out.push_str(",\"semaphores_per_frame\":");
    out.push_str(&plan.semaphores_per_frame.to_string());
    out.push_str(",\"fences_per_frame\":");
    out.push_str(&plan.fences_per_frame.to_string());
    out.push_str(",\"draw_count\":");
    out.push_str(&plan.draw_count.to_string());
    out.push_str(",\"indexed_vertex_count\":");
    out.push_str(&plan.indexed_vertex_count.to_string());
    out.push('}');
    out
}

fn format_api_version(version: u32) -> String {
    format!(
        "{}.{}.{}",
        vk::api_version_major(version),
        vk::api_version_minor(version),
        vk::api_version_patch(version)
    )
}

fn push_json_string(out: &mut String, value: &str) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Diagnostics for Vulkan backend setup and frame progression.
#[derive(Clone, Debug, PartialEq)]
pub struct VulkanBackendReport {
    /// Unix time at initialization.
    pub initialized_at: u64,
    /// Total frames executed.
    pub frames_executed: u64,
    /// Total command submissions.
    pub submissions: u64,
    /// Last command-capture byte size.
    pub last_capture_size: usize,
    /// Number of simulated present calls.
    pub presents: u64,
    /// Number of resize-driven surface plan refreshes.
    pub resize_rebuilds: u64,
    /// Last render request observed.
    pub request: RenderRequest,
    /// Last deterministic frame submission plan.
    pub last_frame_submission: Option<VulkanFrameSubmissionPlan>,
}

impl Default for VulkanBackendReport {
    fn default() -> Self {
        Self {
            initialized_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_secs()),
            frames_executed: 0,
            submissions: 0,
            last_capture_size: 0,
            presents: 0,
            resize_rebuilds: 0,
            request: RenderRequest::conservative(),
            last_frame_submission: None,
        }
    }
}

/// Vulkan backend façade used by the game entrypoint.
#[derive(Debug)]
pub struct VulkanBackend {
    state: VulkanBackendState,
    report: VulkanBackendReport,
    swapchain_plan: VulkanSwapchainPlan,
}

impl Default for VulkanBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl VulkanBackend {
    /// Creates a new Vulkan-backed backend façade.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: VulkanBackendState::Ready,
            report: VulkanBackendReport::default(),
            swapchain_plan: default_stage0_swapchain_plan(),
        }
    }

    /// Replaces active surface/profile request.
    pub fn set_render_request(&mut self, request: RenderRequest) {
        self.report.request = request;
        self.report.resize_rebuilds = self.report.resize_rebuilds.saturating_add(1);
    }

    /// Returns active render request policy.
    #[must_use]
    pub const fn render_request(&self) -> RenderRequest {
        self.report.request
    }

    /// Replaces active swapchain plan used for frame submission planning.
    pub fn set_swapchain_plan(&mut self, plan: VulkanSwapchainPlan) {
        self.swapchain_plan = plan;
    }

    /// Returns active swapchain plan.
    #[must_use]
    pub const fn swapchain_plan(&self) -> &VulkanSwapchainPlan {
        &self.swapchain_plan
    }

    /// Returns adapter state.
    #[must_use]
    pub const fn state(&self) -> VulkanBackendState {
        self.state
    }

    /// Returns backend report.
    #[must_use]
    pub fn report(&self) -> &VulkanBackendReport {
        &self.report
    }

    fn simulate_present(&mut self) {
        self.report.presents = self.report.presents.saturating_add(1);
    }
}

impl RenderBackend for VulkanBackend {
    fn execute(&mut self, commands: &RenderCommandList) -> Result<FrameOutput, RenderError> {
        if !matches!(
            self.state,
            VulkanBackendState::Ready | VulkanBackendState::Degraded
        ) {
            return Err(RenderError::InvalidRange);
        }
        let capture = canonical_capture(commands)?;
        let frame_plan = plan_vulkan_frame_submission(&self.swapchain_plan, commands)?;
        self.report.frames_executed = self.report.frames_executed.saturating_add(1);
        self.report.submissions = self.report.submissions.saturating_add(1);
        self.report.last_capture_size = capture.len();
        self.report.last_frame_submission = Some(frame_plan);
        self.simulate_present();
        Ok(FrameOutput)
    }
}

fn default_stage0_swapchain_plan() -> VulkanSwapchainPlan {
    VulkanSwapchainPlan {
        schema: 1,
        extent: (1, 1),
        format: VulkanSurfaceFormat {
            format: vk::Format::B8G8R8A8_SRGB.as_raw(),
            color_space: vk::ColorSpaceKHR::SRGB_NONLINEAR.as_raw(),
        },
        present_mode: vk::PresentModeKHR::FIFO.as_raw(),
        image_count: 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_render::{
        DrawCommand, DrawId, GpuMaterialId, GpuMeshId, IndexRange, RenderCommand, RenderPhase,
    };

    #[test]
    fn backend_tracks_render_request_and_presents() -> Result<(), RenderError> {
        let mut backend = VulkanBackend::new();
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
        assert_eq!(backend.state(), VulkanBackendState::Ready);
        assert_eq!(backend.report().frames_executed, 1);
        assert_eq!(backend.report().submissions, 1);
        assert_eq!(backend.report().presents, 1);
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
            image_count: 3,
            ..default_stage0_swapchain_plan()
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
            "{\"schema\":1,\"create_flags\":1,\"validation_requested\":true,\"enabled_extensions\":[\"VK_KHR_portability_enumeration\",\"VK_KHR_surface\"]}"
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

        assert_eq!(report.modules.len(), 2);
        assert_eq!(report.modules[0].name, "triangle.vert");
        assert_eq!(report.modules[0].stage, VulkanShaderStage::Vertex);
        assert_eq!(report.modules[0].word_count, 12);
        assert_eq!(
            report.modules[0].sha256,
            "f0dc7b3388e59e94a0e1d5d82c97f103d47ab703145fdf44acb3b7cdf0d6087f"
        );
        assert_eq!(
            report.modules[1].sha256,
            "bd5e45e96505076efea674c38214e0ee479030d239b52bdc8ffe9835674d14d5"
        );
        assert_eq!(
            report.manifest_hash,
            "dd293e4ff08ffca1c037900d08b0ffd415db39f238b4fcdde46468fa049b679c"
        );
    }

    #[test]
    fn shader_manifest_report_json_is_stable() {
        let report =
            validate_shader_manifest(&triangle_shader_manifest()).expect("shader manifest");

        assert!(render_shader_manifest_report_json(&report).contains(SHADER_COMPILER_ID));
        assert!(render_shader_manifest_report_json(&report).contains(SPIRV_VALIDATOR_ID));
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
            capabilities: VulkanSwapchainSurfaceCapabilities {
                current_extent: None,
                min_extent: (320, 240),
                max_extent: (1024, 768),
                min_image_count: 2,
                max_image_count: 3,
            },
            preferred_present_mode: vk::PresentModeKHR::MAILBOX.as_raw(),
        }
    }
}
