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

use ash::{
    khr::{surface, swapchain},
    vk,
};
use fparkan_binary::{sha256, sha256_hex};
use fparkan_platform::{NativeWindowHandles, RenderRequest};
use fparkan_render::{
    canonical_capture, validate_command_list, FrameOutput, RenderBackend, RenderCommand,
    RenderCommandList, RenderError,
};
use std::collections::BTreeSet;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Minimum Vulkan API version accepted by the Stage 0 backend.
pub const MIN_VULKAN_API_VERSION: u32 = vk::API_VERSION_1_1;
const KHR_SWAPCHAIN_EXTENSION: &str = "VK_KHR_swapchain";
const KHR_PORTABILITY_SUBSET_EXTENSION: &str = "VK_KHR_portability_subset";
const KHR_PORTABILITY_ENUMERATION_EXTENSION: &str = "VK_KHR_portability_enumeration";
const EXT_DEBUG_UTILS_EXTENSION: &str = "VK_EXT_debug_utils";
const VALIDATION_LAYER_NAME: &str = "VK_LAYER_KHRONOS_validation";
const SPIRV_MAGIC: u32 = 0x0723_0203;
const SPIRV_VERSION_1_0: u32 = 0x0001_0000;
const TRIANGLE_VERTEX_SHADER_WORDS: &[u32] = &[
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
const TRIANGLE_FRAGMENT_SHADER_WORDS: &[u32] = &[
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

const SHADER_MANIFEST_SCHEMA: u32 = 2;
const SHADER_TARGET_ENV: &str = "vulkan1.0";
const SHADER_COMPILER_NAME: &str = "glslangValidator";
const SHADER_COMPILER_VERSION: &str = "11:16.3.0";
const SHADER_COMPILER_BINARY_SHA256: &str =
    "9bcd69d830b350aaa6e2254915ff74e46070e217b67f38daad27c1fc1f22910f";
const SPIRV_VALIDATOR_NAME: &str = "spirv-val";
const SPIRV_VALIDATOR_VERSION: &str = "SPIRV-Tools v2026.2 unknown hash, 2026-04-29T17:02:58+00:00";
const SPIRV_VALIDATOR_BINARY_SHA256: &str =
    "f6d5b96ff19f073f3af0c0bcfa0c18702d288d3ec598efc242d01cd104d8354f";
const TRIANGLE_VERTEX_SOURCE_PATH: &str = "adapters/fparkan-render-vulkan/shaders/triangle.vert";
const TRIANGLE_VERTEX_SOURCE_SHA256: &str =
    "1e57f14d193fc61457c0749081c452ad25669998913107df12f3ccc3c33e0341";
const TRIANGLE_VERTEX_SPIRV_PATH: &str = "adapters/fparkan-render-vulkan/shaders/triangle.vert.spv";
const TRIANGLE_VERTEX_COMPILE_COMMAND: &str = "glslangValidator -V -S vert -e main adapters/fparkan-render-vulkan/shaders/triangle.vert -o adapters/fparkan-render-vulkan/shaders/triangle.vert.spv";
const TRIANGLE_VERTEX_VALIDATE_COMMAND: &str =
    "spirv-val --target-env vulkan1.0 adapters/fparkan-render-vulkan/shaders/triangle.vert.spv";
const TRIANGLE_FRAGMENT_SOURCE_PATH: &str = "adapters/fparkan-render-vulkan/shaders/triangle.frag";
const TRIANGLE_FRAGMENT_SOURCE_SHA256: &str =
    "f19e74d001d07fb537d4b0f9e621f9b8bc40eeb68816130220853abea6bd4445";
const TRIANGLE_FRAGMENT_SPIRV_PATH: &str =
    "adapters/fparkan-render-vulkan/shaders/triangle.frag.spv";
const TRIANGLE_FRAGMENT_COMPILE_COMMAND: &str = "glslangValidator -V -S frag -e main adapters/fparkan-render-vulkan/shaders/triangle.frag -o adapters/fparkan-render-vulkan/shaders/triangle.frag.spv";
const TRIANGLE_FRAGMENT_VALIDATE_COMMAND: &str =
    "spirv-val --target-env vulkan1.0 adapters/fparkan-render-vulkan/shaders/triangle.frag.spv";

/// Shader tool metadata pinned in the Stage 0 manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VulkanShaderToolManifest {
    /// Tool executable name.
    pub name: &'static str,
    /// Tool version string.
    pub version: &'static str,
    /// Tool binary SHA-256.
    pub binary_sha256: &'static str,
}

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
    /// Checked-in GLSL source path.
    pub source_path: &'static str,
    /// Checked-in GLSL source SHA-256.
    pub source_sha256: &'static str,
    /// Checked-in SPIR-V module path.
    pub spirv_path: &'static str,
    /// Exact offline compile command used for the checked-in SPIR-V artifact.
    pub compile_command: &'static str,
    /// Exact offline validation command used for the checked-in SPIR-V artifact.
    pub validate_command: &'static str,
    /// SPIR-V words.
    pub words: &'static [u32],
}

/// Shader manifest validation report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VulkanShaderManifestReport {
    /// Report schema version.
    pub schema: u32,
    /// Explicit Vulkan target environment for the checked-in SPIR-V.
    pub target_env: &'static str,
    /// Pinned compiler metadata.
    pub compiler: VulkanShaderToolManifest,
    /// Pinned validator metadata.
    pub validator: VulkanShaderToolManifest,
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
    /// Checked-in GLSL source path.
    pub source_path: &'static str,
    /// Checked-in GLSL source SHA-256.
    pub source_sha256: &'static str,
    /// Checked-in SPIR-V module path.
    pub spirv_path: &'static str,
    /// SPIR-V word count.
    pub word_count: usize,
    /// SPIR-V byte hash.
    pub sha256: String,
    /// Descriptor set count.
    pub descriptor_sets: u32,
    /// Push constant byte count.
    pub push_constant_bytes: u32,
    /// Exact offline compile command used for the checked-in SPIR-V artifact.
    pub compile_command: &'static str,
    /// Exact offline validation command used for the checked-in SPIR-V artifact.
    pub validate_command: &'static str,
    /// Stable hash of the reflected interface contract for this module.
    pub interface_hash: String,
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
        /// Raw Vulkan result text.
        result: String,
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
                write!(f, "{context}: {result}")
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

struct VulkanValidationShared {
    warning_count: AtomicU32,
    error_count: AtomicU32,
    vuids: Mutex<BTreeSet<String>>,
}

impl Default for VulkanValidationShared {
    fn default() -> Self {
        Self {
            warning_count: AtomicU32::new(0),
            error_count: AtomicU32::new(0),
            vuids: Mutex::new(BTreeSet::new()),
        }
    }
}

struct VulkanValidationMessenger {
    loader: ash::ext::debug_utils::Instance,
    messenger: vk::DebugUtilsMessengerEXT,
    shared: Box<VulkanValidationShared>,
}

impl VulkanValidationMessenger {
    fn report(&self) -> VulkanValidationReport {
        let vuids = self
            .shared
            .vuids
            .lock()
            .map(|values| values.iter().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        VulkanValidationReport {
            warning_count: self.shared.warning_count.load(Ordering::Relaxed),
            error_count: self.shared.error_count.load(Ordering::Relaxed),
            vuids,
        }
    }
}

impl Drop for VulkanValidationMessenger {
    fn drop(&mut self) {
        // SAFETY: The messenger belongs to this instance-level loader and is destroyed once.
        unsafe {
            self.loader
                .destroy_debug_utils_messenger(self.messenger, None);
        };
    }
}

unsafe extern "system" fn vulkan_validation_callback(
    message_severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    _message_types: vk::DebugUtilsMessageTypeFlagsEXT,
    callback_data: *const vk::DebugUtilsMessengerCallbackDataEXT<'_>,
    user_data: *mut std::ffi::c_void,
) -> vk::Bool32 {
    // SAFETY: The debug messenger stores a stable pointer to `VulkanValidationShared` for the messenger lifetime.
    let Some(shared) = (unsafe { (user_data as *const VulkanValidationShared).as_ref() }) else {
        return vk::FALSE;
    };
    if message_severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::ERROR) {
        shared.error_count.fetch_add(1, Ordering::Relaxed);
    } else if message_severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::WARNING) {
        shared.warning_count.fetch_add(1, Ordering::Relaxed);
    }
    // SAFETY: Vulkan invokes the callback with either a null pointer or a valid callback-data payload.
    let Some(callback_data) = (unsafe { callback_data.as_ref() }) else {
        return vk::FALSE;
    };
    if let Some(vuid) = (!callback_data.p_message_id_name.is_null()).then(|| {
        // SAFETY: `p_message_id_name` is a Vulkan-owned NUL-terminated string for the callback duration.
        unsafe { CStr::from_ptr(callback_data.p_message_id_name) }
            .to_string_lossy()
            .into_owned()
    }) {
        if vuid.starts_with("VUID-") {
            if let Ok(mut vuids) = shared.vuids.lock() {
                vuids.insert(vuid);
            }
        }
    }
    vk::FALSE
}

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
            result: format!("{error:?}"),
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
                    result: format!("{error:?}"),
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
                result: format!("{error:?}"),
            })?;
        }
        self.images_in_flight[image_index_usize] = in_flight_fence;
        // SAFETY: The fence belongs to this frame context and is not in use after the wait above.
        unsafe { self.device_ref()?.device().reset_fences(&[in_flight_fence]) }.map_err(
            |error| VulkanSmokeRendererError::VulkanOperation {
                context: "vkResetFences",
                result: format!("{error:?}"),
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
            result: format!("{error:?}"),
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
                    result: format!("{error:?}"),
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
                result: format!("{error:?}"),
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
            result: format!("{error:?}"),
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
            result: format!("{error:?}"),
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
            result: format!("{error:?}"),
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
            result: format!("{error:?}"),
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
    }
}

impl Drop for VulkanSmokeRenderer {
    fn drop(&mut self) {
        self.destroy_swapchain_resources();
        if let Some(device) = self.device.as_ref() {
            if let Some(buffer) = self.index_buffer.take() {
                // SAFETY: Buffer and memory belong to this device and are destroyed once.
                unsafe {
                    device.device().destroy_buffer(buffer.buffer, None);
                    device.device().free_memory(buffer.memory, None);
                }
            }
            if let Some(buffer) = self.vertex_buffer.take() {
                // SAFETY: Buffer and memory belong to this device and are destroyed once.
                unsafe {
                    device.device().destroy_buffer(buffer.buffer, None);
                    device.device().free_memory(buffer.memory, None);
                }
            }
            // SAFETY: The command pool belongs to this device and is destroyed once after buffers are freed.
            unsafe {
                device
                    .device()
                    .destroy_command_pool(self.command_pool, None);
            };
            // SAFETY: The logical device remains live until the renderer completes teardown.
            let _ = unsafe { device.device().device_wait_idle() };
        }
        self.swapchain.take();
        self.device.take();
        self.surface.take();
        self.validation.take();
        self.instance.take();
    }
}

fn create_validation_messenger(
    instance: &VulkanInstanceProbe,
) -> Result<VulkanValidationMessenger, VulkanSmokeRendererError> {
    let shared = Box::new(VulkanValidationShared::default());
    let loader = ash::ext::debug_utils::Instance::new(&instance.entry, &instance.instance);
    let create_info = vk::DebugUtilsMessengerCreateInfoEXT::default()
        .message_severity(
            vk::DebugUtilsMessageSeverityFlagsEXT::WARNING
                | vk::DebugUtilsMessageSeverityFlagsEXT::ERROR,
        )
        .message_type(
            vk::DebugUtilsMessageTypeFlagsEXT::GENERAL
                | vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION
                | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE,
        )
        .pfn_user_callback(Some(vulkan_validation_callback))
        .user_data((&raw const *shared).cast_mut().cast());
    let messenger =
        // SAFETY: The create info points at a stable boxed user-data allocation for the messenger lifetime.
        unsafe { loader.create_debug_utils_messenger(&create_info, None) }.map_err(|error| {
            VulkanSmokeRendererError::VulkanOperation {
                context: "vkCreateDebugUtilsMessengerEXT",
                result: format!("{error:?}"),
            }
        })?;
    Ok(VulkanValidationMessenger {
        loader,
        messenger,
        shared,
    })
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
            result: format!("{error:?}"),
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
            result: format!("{error:?}"),
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
                result: format!("{error:?}"),
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
            result: format!("{error:?}"),
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
            result: format!("{error:?}"),
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
        result: format!("{error:?}"),
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
            result: format!("{error:?}"),
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
            result: format!("{error:?}"),
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
            result: format!("{error:?}"),
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
            result: format!("{error:?}"),
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
            result: format!("{error:?}"),
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
            result: format!("{error:?}"),
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
            result: format!("{error:?}"),
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
                result: format!("{error:?}"),
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
                        result: format!("{error:?}"),
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
                    result: format!("{error:?}"),
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
        result: String,
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
                    "Vulkan logical device creation failed for {device}: {result}"
                )
            }
        }
    }
}

impl std::error::Error for VulkanLogicalDeviceError {}

/// Vulkan swapchain creation error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VulkanSwapchainProbeError {
    /// Surface capability query failed.
    SurfaceCapabilitiesFailed {
        /// Vulkan result.
        result: String,
    },
    /// Swapchain creation failed.
    CreateFailed {
        /// Vulkan result.
        result: String,
    },
    /// Swapchain image query failed.
    ImagesFailed {
        /// Vulkan result.
        result: String,
    },
}

impl std::fmt::Display for VulkanSwapchainProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SurfaceCapabilitiesFailed { result } => {
                write!(f, "Vulkan surface capabilities query failed: {result}")
            }
            Self::CreateFailed { result } => {
                write!(f, "Vulkan swapchain creation failed: {result}")
            }
            Self::ImagesFailed { result } => {
                write!(f, "Vulkan swapchain image query failed: {result}")
            }
        }
    }
}

impl std::error::Error for VulkanSwapchainProbeError {}

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
        result: format!("{error:?}"),
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
    .map_err(
        |error| VulkanSwapchainProbeError::SurfaceCapabilitiesFailed {
            result: format!("{error:?}"),
        },
    )?;
    let surface_formats =
        live_surface_formats(surface, device.physical_device, &device.report.device_name).map_err(
            |error| VulkanSwapchainProbeError::CreateFailed {
                result: error.to_string(),
            },
        )?;
    let present_modes =
        live_present_modes(surface, device.physical_device, &device.report.device_name).map_err(
            |error| VulkanSwapchainProbeError::CreateFailed {
                result: error.to_string(),
            },
        )?;
    let capabilities =
        live_surface_capabilities(surface, device.physical_device, &device.report.device_name)
            .map_err(
                |error| VulkanSwapchainProbeError::SurfaceCapabilitiesFailed {
                    result: error.to_string(),
                },
            )?;
    let plan = plan_vulkan_swapchain(&VulkanSwapchainRequest {
        drawable_extent,
        formats: surface_formats,
        present_modes,
        capabilities,
        preferred_present_mode: vk::PresentModeKHR::MAILBOX.as_raw(),
    })
    .map_err(|error| VulkanSwapchainProbeError::CreateFailed {
        result: error.to_string(),
    })?;
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
    let swapchain = unsafe { loader.create_swapchain(&create_info, None) }.map_err(|error| {
        VulkanSwapchainProbeError::CreateFailed {
            result: format!("{error:?}"),
        }
    })?;
    // SAFETY: The swapchain was created above and the returned image handles are owned by it.
    let images = match unsafe { loader.get_swapchain_images(swapchain) } {
        Ok(images) => images,
        Err(error) => {
            // SAFETY: The swapchain was created above on this loader/device pair and is destroyed on setup failure.
            unsafe { loader.destroy_swapchain(swapchain, None) };
            return Err(VulkanSwapchainProbeError::ImagesFailed {
                result: format!("{error:?}"),
            });
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
            VulkanRuntimeCapabilityError::EnumerateDevicesFailed {
                result: format!("{error:?}"),
            }
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
    /// Validation layers were requested but unavailable.
    MissingValidationLayer,
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
            Self::MissingValidationLayer => {
                write!(
                    f,
                    "Vulkan validation layer VK_LAYER_KHRONOS_validation is unavailable"
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
                result: format!("{error:?}"),
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
            source_path: TRIANGLE_VERTEX_SOURCE_PATH,
            source_sha256: TRIANGLE_VERTEX_SOURCE_SHA256,
            spirv_path: TRIANGLE_VERTEX_SPIRV_PATH,
            compile_command: TRIANGLE_VERTEX_COMPILE_COMMAND,
            validate_command: TRIANGLE_VERTEX_VALIDATE_COMMAND,
            words: TRIANGLE_VERTEX_SHADER_WORDS,
        },
        VulkanShaderModuleManifest {
            name: "triangle.frag",
            stage: VulkanShaderStage::Fragment,
            entry_point: "main",
            descriptor_sets: 0,
            push_constant_bytes: 0,
            source_path: TRIANGLE_FRAGMENT_SOURCE_PATH,
            source_sha256: TRIANGLE_FRAGMENT_SOURCE_SHA256,
            spirv_path: TRIANGLE_FRAGMENT_SPIRV_PATH,
            compile_command: TRIANGLE_FRAGMENT_COMPILE_COMMAND,
            validate_command: TRIANGLE_FRAGMENT_VALIDATE_COMMAND,
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
            source_path: module.source_path,
            source_sha256: module.source_sha256,
            spirv_path: module.spirv_path,
            word_count: module.words.len(),
            sha256: sha256_hex(&sha256(&bytes)),
            descriptor_sets: module.descriptor_sets,
            push_constant_bytes: module.push_constant_bytes,
            compile_command: module.compile_command,
            validate_command: module.validate_command,
            interface_hash: shader_interface_hash(module),
        });
    }
    let normalized = render_shader_manifest_without_hash_json(&reports);
    Ok(VulkanShaderManifestReport {
        schema: SHADER_MANIFEST_SCHEMA,
        target_env: SHADER_TARGET_ENV,
        compiler: VulkanShaderToolManifest {
            name: SHADER_COMPILER_NAME,
            version: SHADER_COMPILER_VERSION,
            binary_sha256: SHADER_COMPILER_BINARY_SHA256,
        },
        validator: VulkanShaderToolManifest {
            name: SPIRV_VALIDATOR_NAME,
            version: SPIRV_VALIDATOR_VERSION,
            binary_sha256: SPIRV_VALIDATOR_BINARY_SHA256,
        },
        modules: reports,
        manifest_hash: sha256_hex(&sha256(normalized.as_bytes())),
    })
}

fn shader_interface_hash(module: &VulkanShaderModuleManifest) -> String {
    let mut normalized = String::new();
    normalized.push_str("{\"stage\":\"");
    normalized.push_str(module.stage.as_str());
    normalized.push_str("\",\"entry_point\":");
    push_json_string(&mut normalized, module.entry_point);
    normalized.push_str(",\"descriptor_sets\":");
    normalized.push_str(&module.descriptor_sets.to_string());
    normalized.push_str(",\"push_constant_bytes\":");
    normalized.push_str(&module.push_constant_bytes.to_string());
    normalized.push('}');
    sha256_hex(&sha256(normalized.as_bytes()))
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
    let mut out = render_shader_manifest_without_hash_json(&report.modules);
    out.push_str(",\"manifest_hash\":");
    push_json_string(&mut out, &report.manifest_hash);
    out.push('}');
    out
}

fn render_shader_manifest_without_hash_json(modules: &[VulkanShaderModuleReport]) -> String {
    let mut out = String::new();
    out.push_str("{\"schema\":");
    out.push_str(&SHADER_MANIFEST_SCHEMA.to_string());
    out.push_str(",\"target_env\":");
    push_json_string(&mut out, SHADER_TARGET_ENV);
    out.push_str(",\"compiler\":");
    out.push_str(&render_shader_tool_json(&VulkanShaderToolManifest {
        name: SHADER_COMPILER_NAME,
        version: SHADER_COMPILER_VERSION,
        binary_sha256: SHADER_COMPILER_BINARY_SHA256,
    }));
    out.push_str(",\"validator\":");
    out.push_str(&render_shader_tool_json(&VulkanShaderToolManifest {
        name: SPIRV_VALIDATOR_NAME,
        version: SPIRV_VALIDATOR_VERSION,
        binary_sha256: SPIRV_VALIDATOR_BINARY_SHA256,
    }));
    out.push_str(",\"modules\":");
    out.push_str(&render_shader_modules_json(modules));
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
        out.push_str(",\"source_path\":");
        push_json_string(&mut out, module.source_path);
        out.push_str(",\"source_sha256\":");
        push_json_string(&mut out, module.source_sha256);
        out.push_str(",\"spirv_path\":");
        push_json_string(&mut out, module.spirv_path);
        out.push_str(",\"word_count\":");
        out.push_str(&module.word_count.to_string());
        out.push_str(",\"sha256\":");
        push_json_string(&mut out, &module.sha256);
        out.push_str(",\"descriptor_sets\":");
        out.push_str(&module.descriptor_sets.to_string());
        out.push_str(",\"push_constant_bytes\":");
        out.push_str(&module.push_constant_bytes.to_string());
        out.push_str(",\"compile_command\":");
        push_json_string(&mut out, module.compile_command);
        out.push_str(",\"validate_command\":");
        push_json_string(&mut out, module.validate_command);
        out.push_str(",\"interface_hash\":");
        push_json_string(&mut out, &module.interface_hash);
        out.push('}');
    }
    out.push(']');
    out
}

fn render_shader_tool_json(tool: &VulkanShaderToolManifest) -> String {
    let mut out = String::new();
    out.push_str("{\"name\":");
    push_json_string(&mut out, tool.name);
    out.push_str(",\"version\":");
    push_json_string(&mut out, tool.version);
    out.push_str(",\"binary_sha256\":");
    push_json_string(&mut out, tool.binary_sha256);
    out.push('}');
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
    let mut last_error = None;
    for device in devices {
        let report = match validate_device(device) {
            Ok(report) => report,
            Err(err) => {
                last_error = Some(err);
                continue;
            }
        };
        match &best {
            Some(existing) if compare_reports(&report, existing) != std::cmp::Ordering::Greater => {
            }
            _ => best = Some(report),
        }
    }
    best.ok_or_else(|| last_error.unwrap_or(VulkanCapabilityError::NoPhysicalDevice))
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

fn select_composite_alpha(supported: vk::CompositeAlphaFlagsKHR) -> vk::CompositeAlphaFlagsKHR {
    if supported.contains(vk::CompositeAlphaFlagsKHR::OPAQUE) {
        vk::CompositeAlphaFlagsKHR::OPAQUE
    } else if supported.contains(vk::CompositeAlphaFlagsKHR::PRE_MULTIPLIED) {
        vk::CompositeAlphaFlagsKHR::PRE_MULTIPLIED
    } else if supported.contains(vk::CompositeAlphaFlagsKHR::POST_MULTIPLIED) {
        vk::CompositeAlphaFlagsKHR::POST_MULTIPLIED
    } else {
        vk::CompositeAlphaFlagsKHR::INHERIT
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
    let (graphics_queue_family, present_queue_family) = select_queue_families(device)?;

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

fn select_queue_families(
    device: &VulkanPhysicalDeviceRecord,
) -> Result<(u32, u32), VulkanCapabilityError> {
    if let Some(unified) = device
        .queue_families
        .iter()
        .find(|family| family.graphics && family.present)
    {
        return Ok((unified.index, unified.index));
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
    Ok((graphics_queue_family, present_queue_family))
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

/// Diagnostics for Vulkan planning backend setup and frame progression.
#[derive(Clone, Debug, PartialEq)]
pub struct VulkanPlanningBackendReport {
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

impl Default for VulkanPlanningBackendReport {
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

/// Vulkan planning backend façade used by the game entrypoint.
#[derive(Debug)]
pub struct VulkanPlanningBackend {
    state: VulkanBackendState,
    report: VulkanPlanningBackendReport,
    swapchain_plan: VulkanSwapchainPlan,
}

impl Default for VulkanPlanningBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl VulkanPlanningBackend {
    /// Creates a new Vulkan planning backend façade.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: VulkanBackendState::Ready,
            report: VulkanPlanningBackendReport::default(),
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
    pub fn report(&self) -> &VulkanPlanningBackendReport {
        &self.report
    }

    fn simulate_present(&mut self) {
        self.report.presents = self.report.presents.saturating_add(1);
    }
}

impl RenderBackend for VulkanPlanningBackend {
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
