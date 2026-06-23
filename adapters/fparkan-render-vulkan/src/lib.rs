#![forbid(unsafe_code)]
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

use ash::vk;
use fparkan_platform::RenderRequest;
use fparkan_render::{
    canonical_capture, FrameOutput, RenderBackend, RenderCommandList, RenderError,
};
use std::time::{SystemTime, UNIX_EPOCH};

/// Minimum Vulkan API version accepted by the Stage 0 backend.
pub const MIN_VULKAN_API_VERSION: u32 = vk::API_VERSION_1_1;
const KHR_SWAPCHAIN_EXTENSION: &str = "VK_KHR_swapchain";
const KHR_PORTABILITY_SUBSET_EXTENSION: &str = "VK_KHR_portability_subset";

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
        }
    }
}

/// Vulkan backend façade used by the game entrypoint.
#[derive(Debug)]
pub struct VulkanBackend {
    state: VulkanBackendState,
    report: VulkanBackendReport,
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
        self.report.frames_executed = self.report.frames_executed.saturating_add(1);
        self.report.submissions = self.report.submissions.saturating_add(1);
        self.report.last_capture_size = capture.len();
        self.simulate_present();
        Ok(FrameOutput)
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
}
