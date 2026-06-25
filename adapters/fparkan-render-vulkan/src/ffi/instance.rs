#![allow(unsafe_code)]

use ash::vk;
use serde::Serialize;
use std::collections::BTreeSet;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;

use super::{
    EXT_DEBUG_UTILS_EXTENSION, KHR_PORTABILITY_ENUMERATION_EXTENSION, MIN_VULKAN_API_VERSION,
    VALIDATION_LAYER_NAME,
};
use crate::policy::{format_api_version, serialize_json_or_fallback};

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
    pub(super) entry: ash::Entry,
    pub(super) instance: ash::Instance,
    /// Deterministic instance creation report.
    pub report: VulkanInstancePlan,
}

impl Drop for VulkanInstanceProbe {
    fn drop(&mut self) {
        // SAFETY: The `Instance` was created by this probe and is destroyed once during drop.
        unsafe { self.instance.destroy_instance(None) };
    }
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
    /// A required instance extension is unavailable from the loader.
    MissingInstanceExtension {
        /// Required extension name.
        extension: String,
    },
    /// Validation layers were requested but unavailable.
    MissingValidationLayer,
    /// Instance creation failed.
    CreateFailed {
        /// Vulkan result.
        result: vk::Result,
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
            Self::MissingInstanceExtension { extension } => {
                write!(f, "Vulkan instance extension {extension} is unavailable")
            }
            Self::MissingValidationLayer => {
                write!(
                    f,
                    "Vulkan validation layer VK_LAYER_KHRONOS_validation is unavailable"
                )
            }
            Self::CreateFailed { result } => {
                write!(f, "Vulkan instance creation failed: {result:?}")
            }
        }
    }
}

impl std::error::Error for VulkanInstanceError {}

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

/// Builds the deterministic instance creation plan without touching the loader.
#[must_use]
pub fn plan_vulkan_instance(config: &VulkanInstanceConfig) -> VulkanInstancePlan {
    let mut enabled_extensions = config.required_extensions.clone();
    if config.enable_validation
        && !enabled_extensions
            .iter()
            .any(|extension| extension == EXT_DEBUG_UTILS_EXTENSION)
    {
        enabled_extensions.push(EXT_DEBUG_UTILS_EXTENSION.to_string());
    }
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
    let available_extensions = available_instance_extensions(&entry)?;
    ensure_instance_extensions_available(&plan.enabled_extensions, &available_extensions)?;
    let extension_names = cstring_vec(&plan.enabled_extensions)?;
    let extension_ptrs = cstring_ptrs(&extension_names);
    let layer_names = validation_layer_cstrings(&entry, config.enable_validation)?;
    let layer_ptrs = cstring_ptrs(&layer_names);
    let app_info = vk::ApplicationInfo::default()
        .application_name(&app_name)
        .application_version(0)
        .engine_name(engine_name)
        .engine_version(0)
        .api_version(MIN_VULKAN_API_VERSION);
    let create_info = vk::InstanceCreateInfo::default()
        .application_info(&app_info)
        .enabled_extension_names(&extension_ptrs)
        .enabled_layer_names(&layer_ptrs)
        .flags(vk::InstanceCreateFlags::from_raw(plan.create_flags));
    // SAFETY: `create_info` points to stack-owned Vulkan create data that lives for the call.
    let instance = unsafe { entry.create_instance(&create_info, None) }
        .map_err(|error| VulkanInstanceError::CreateFailed { result: error })?;
    Ok(VulkanInstanceProbe {
        entry,
        instance,
        report: plan,
    })
}

/// Renders a deterministic JSON Vulkan instance plan.
#[must_use]
pub fn render_instance_plan_json(plan: &VulkanInstancePlan) -> String {
    #[derive(Serialize)]
    struct InstancePlanJson<'a> {
        schema: u32,
        create_flags: u32,
        validation_requested: bool,
        enabled_extensions: &'a [String],
    }

    serialize_json_or_fallback(
        &InstancePlanJson {
            schema: plan.schema,
            create_flags: plan.create_flags,
            validation_requested: plan.validation_requested,
            enabled_extensions: &plan.enabled_extensions,
        },
        "{\"schema\":0,\"create_flags\":0,\"validation_requested\":false,\"enabled_extensions\":[]}",
    )
}

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
    #[derive(Serialize)]
    struct LoaderProbeReportJson {
        schema: u32,
        loader_available: bool,
        instance_api: String,
    }

    serialize_json_or_fallback(
        &LoaderProbeReportJson {
            schema: report.schema,
            loader_available: report.loader_available,
            instance_api: format_api_version(report.instance_api_version),
        },
        "{\"schema\":0,\"loader_available\":false,\"instance_api\":\"0.0.0\"}",
    )
}

fn available_instance_extensions(entry: &ash::Entry) -> Result<Vec<String>, VulkanInstanceError> {
    let available_extensions =
        // SAFETY: Enumerating instance extensions reads loader-owned immutable metadata.
        unsafe { entry.enumerate_instance_extension_properties(None) }.map_err(|error| {
            VulkanInstanceError::CreateFailed {
                result: error,
            }
        })?;
    available_extensions
        .into_iter()
        .map(|extension| {
            // SAFETY: Vulkan extension names are fixed-size NUL-terminated strings from the loader.
            Ok(unsafe { CStr::from_ptr(extension.extension_name.as_ptr()) }
                .to_string_lossy()
                .into_owned())
        })
        .collect()
}

pub(super) fn ensure_instance_extensions_available(
    required_extensions: &[String],
    available_extensions: &[String],
) -> Result<(), VulkanInstanceError> {
    let available = available_extensions
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    for extension in required_extensions {
        if !available.contains(extension.as_str()) {
            return Err(VulkanInstanceError::MissingInstanceExtension {
                extension: extension.clone(),
            });
        }
    }
    Ok(())
}

fn validation_layer_cstrings(
    entry: &ash::Entry,
    enable_validation: bool,
) -> Result<Vec<CString>, VulkanInstanceError> {
    if !enable_validation {
        return Ok(Vec::new());
    }
    let available_layers =
        // SAFETY: Enumerating instance layers reads loader-owned immutable metadata.
        unsafe { entry.enumerate_instance_layer_properties() }.map_err(|error| {
            VulkanInstanceError::CreateFailed {
                result: error,
            }
        })?;
    let validation_available = available_layers.iter().any(|layer| {
        // SAFETY: Vulkan layer names are fixed-size NUL-terminated strings from the loader.
        unsafe { CStr::from_ptr(layer.layer_name.as_ptr()) }
            .to_string_lossy()
            .as_ref()
            == VALIDATION_LAYER_NAME
    });
    if !validation_available {
        return Err(VulkanInstanceError::MissingValidationLayer);
    }
    Ok(vec![CString::new(VALIDATION_LAYER_NAME).map_err(|_| {
        VulkanInstanceError::InvalidApplicationName
    })?])
}

pub(super) fn cstring_vec(values: &[String]) -> Result<Vec<CString>, VulkanInstanceError> {
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
