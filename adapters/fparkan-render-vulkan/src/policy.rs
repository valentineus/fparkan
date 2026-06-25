use ash::vk;
use fparkan_platform::{DepthStencilSupport, RenderRequest};
use fparkan_render::{validate_command_list, RenderCommand, RenderCommandList, RenderError};
use serde::Serialize;

const MIN_VULKAN_API_VERSION: u32 = vk::API_VERSION_1_1;
pub(crate) const KHR_SWAPCHAIN_EXTENSION: &str = "VK_KHR_swapchain";
pub(crate) const KHR_PORTABILITY_SUBSET_EXTENSION: &str = "VK_KHR_portability_subset";

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
    /// Supported swapchain image-usage flags as raw Vulkan bits.
    pub supported_usage_flags: u32,
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
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
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
    /// Present modes accepted by the target surface.
    pub present_modes: Vec<i32>,
    /// Surface capabilities accepted by the target surface.
    pub surface_capabilities: VulkanSwapchainSurfaceCapabilities,
    /// Depth/stencil attachment formats supported by the device.
    pub supported_depth_stencil_formats: Vec<i32>,
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
    /// Devices rejected by deterministic Stage 0 capability validation.
    pub rejected_devices: Vec<VulkanRejectedDeviceReport>,
}

/// Deterministic rejection reason for an unsuitable physical device.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct VulkanRejectedDeviceReport {
    /// Human-readable device name.
    pub device_name: String,
    /// Stable machine-readable rejection code.
    pub reason_code: &'static str,
    /// Actionable rejection summary.
    pub reason: String,
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
    /// No present mode is available for the target surface.
    MissingPresentMode {
        /// Device name that failed validation.
        device: String,
    },
    /// Swapchain images cannot be used as color attachments.
    MissingColorAttachmentUsage {
        /// Device name that failed validation.
        device: String,
    },
    /// No compatible depth/stencil attachment format exists for the render request.
    MissingDepthStencilFormat {
        /// Device name that failed validation.
        device: String,
        /// Requested depth/stencil profile.
        requested: DepthStencilSupport,
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
            Self::MissingPresentMode { device } => {
                write!(f, "Vulkan device {device} has no supported present mode")
            }
            Self::MissingColorAttachmentUsage { device } => write!(
                f,
                "Vulkan device {device} surface does not support COLOR_ATTACHMENT usage"
            ),
            Self::MissingDepthStencilFormat { device, requested } => write!(
                f,
                "Vulkan device {device} lacks a depth/stencil attachment format for {}-bit depth and {}-bit stencil",
                requested.depth_bits,
                requested.stencil_bits
            ),
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
    select_physical_device_for_request(devices, &RenderRequest::conservative())
}

/// Selects a Vulkan physical device for a specific Stage 0 render request.
///
/// # Errors
///
/// Returns [`VulkanCapabilityError`] when no candidate satisfies the minimum
/// API version, queue, swapchain-extension, surface-format or depth/stencil
/// requirements for the requested profile.
pub fn select_physical_device_for_request(
    devices: &[VulkanPhysicalDeviceRecord],
    render_request: &RenderRequest,
) -> Result<VulkanCapabilityReport, VulkanCapabilityError> {
    if devices.is_empty() {
        return Err(VulkanCapabilityError::NoPhysicalDevice);
    }

    let mut best = None;
    let mut rejected_devices = Vec::new();
    let mut last_error = None;
    for device in devices {
        let report = match validate_device_for_request(device, render_request) {
            Ok(report) => report,
            Err(err) => {
                rejected_devices.push(rejected_device_report(device, &err));
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
    let mut best =
        best.ok_or_else(|| last_error.unwrap_or(VulkanCapabilityError::NoPhysicalDevice))?;
    best.rejected_devices = rejected_devices;
    Ok(best)
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

/// Renders a deterministic JSON capability report.
#[must_use]
pub fn render_capability_report_json(report: &VulkanCapabilityReport) -> String {
    #[derive(Serialize)]
    struct CapabilityReportJson<'a> {
        schema: u32,
        vulkan_api: String,
        device_name: &'a str,
        score: i32,
        graphics_queue_family: u32,
        present_queue_family: u32,
        portability_subset: bool,
        enabled_extensions: &'a [String],
        rejected_devices: &'a [VulkanRejectedDeviceReport],
    }

    serialize_json_or_fallback(
        &CapabilityReportJson {
            schema: report.schema,
            vulkan_api: format_api_version(report.vulkan_api_version),
            device_name: &report.device_name,
            score: report.score,
            graphics_queue_family: report.graphics_queue_family,
            present_queue_family: report.present_queue_family,
            portability_subset: report.portability_subset,
            enabled_extensions: &report.enabled_extensions,
            rejected_devices: &report.rejected_devices,
        },
        "{\"schema\":0,\"vulkan_api\":\"0.0.0\",\"device_name\":\"unknown\",\"score\":0,\"graphics_queue_family\":0,\"present_queue_family\":0,\"portability_subset\":false,\"enabled_extensions\":[],\"rejected_devices\":[]}",
    )
}

/// Renders a deterministic JSON swapchain plan.
#[must_use]
pub fn render_swapchain_plan_json(plan: &VulkanSwapchainPlan) -> String {
    #[derive(Serialize)]
    struct SwapchainPlanJson {
        schema: u32,
        extent: [u32; 2],
        format: i32,
        color_space: i32,
        present_mode: i32,
        image_count: u32,
    }

    serialize_json_or_fallback(
        &SwapchainPlanJson {
            schema: plan.schema,
            extent: [plan.extent.0, plan.extent.1],
            format: plan.format.format,
            color_space: plan.format.color_space,
            present_mode: plan.present_mode,
            image_count: plan.image_count,
        },
        "{\"schema\":0,\"extent\":[0,0],\"format\":0,\"color_space\":0,\"present_mode\":0,\"image_count\":0}",
    )
}

/// Renders a deterministic JSON swapchain recreation report.
#[must_use]
pub fn render_swapchain_recreation_report_json(report: &VulkanSwapchainRecreationReport) -> String {
    #[derive(Serialize)]
    struct SwapchainRecreationReportJson<'a> {
        schema: u32,
        reason: &'a str,
        previous_extent: [u32; 2],
        next_extent: [u32; 2],
    }

    serialize_json_or_fallback(
        &SwapchainRecreationReportJson {
            schema: report.schema,
            reason: match report.reason {
                VulkanSwapchainRecreationReason::Resize => "resize",
                VulkanSwapchainRecreationReason::OutOfDate => "out_of_date",
                VulkanSwapchainRecreationReason::Suboptimal => "suboptimal",
            },
            previous_extent: [report.previous_extent.0, report.previous_extent.1],
            next_extent: [report.next_extent.0, report.next_extent.1],
        },
        "{\"schema\":0,\"reason\":\"unknown\",\"previous_extent\":[0,0],\"next_extent\":[0,0]}",
    )
}

/// Renders a deterministic JSON frame submission plan.
#[must_use]
pub fn render_frame_submission_plan_json(plan: &VulkanFrameSubmissionPlan) -> String {
    serialize_json_or_fallback(
        plan,
        "{\"schema\":0,\"frames_in_flight\":0,\"command_buffers\":0,\"semaphores_per_frame\":0,\"fences_per_frame\":0,\"draw_count\":0,\"indexed_vertex_count\":0}",
    )
}

pub(crate) fn select_composite_alpha(
    supported: vk::CompositeAlphaFlagsKHR,
) -> vk::CompositeAlphaFlagsKHR {
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

pub(crate) fn serialize_json_or_fallback<T: Serialize>(value: &T, fallback: &str) -> String {
    match serde_json::to_string(value) {
        Ok(json) => json,
        Err(_) => fallback.to_string(),
    }
}

pub(crate) fn format_api_version(version: u32) -> String {
    format!(
        "{}.{}.{}",
        vk::api_version_major(version),
        vk::api_version_minor(version),
        vk::api_version_patch(version)
    )
}

fn select_surface_format(
    formats: &[VulkanSurfaceFormat],
) -> Result<VulkanSurfaceFormat, VulkanSwapchainError> {
    if let Some(format) = undefined_surface_format_override(formats) {
        return Ok(format);
    }
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

fn undefined_surface_format_override(
    formats: &[VulkanSurfaceFormat],
) -> Option<VulkanSurfaceFormat> {
    match formats {
        [format] if format.format == vk::Format::UNDEFINED.as_raw() => Some(VulkanSurfaceFormat {
            format: vk::Format::B8G8R8A8_SRGB.as_raw(),
            color_space: format.color_space,
        }),
        _ => None,
    }
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

pub(crate) fn validate_device_for_request(
    device: &VulkanPhysicalDeviceRecord,
    render_request: &RenderRequest,
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
    if !supports_surface_formats(device) {
        return Err(VulkanCapabilityError::MissingSurfaceFormat {
            device: device.name.clone(),
        });
    }
    if device.present_modes.is_empty() {
        return Err(VulkanCapabilityError::MissingPresentMode {
            device: device.name.clone(),
        });
    }
    if !supports_color_attachment_usage(device.surface_capabilities) {
        return Err(VulkanCapabilityError::MissingColorAttachmentUsage {
            device: device.name.clone(),
        });
    }
    if !supports_depth_stencil_request(device, render_request.depth) {
        return Err(VulkanCapabilityError::MissingDepthStencilFormat {
            device: device.name.clone(),
            requested: render_request.depth,
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
        rejected_devices: Vec::new(),
    })
}

fn rejected_device_report(
    device: &VulkanPhysicalDeviceRecord,
    error: &VulkanCapabilityError,
) -> VulkanRejectedDeviceReport {
    VulkanRejectedDeviceReport {
        device_name: device.name.clone(),
        reason_code: capability_error_code(error),
        reason: error.to_string(),
    }
}

const fn capability_error_code(error: &VulkanCapabilityError) -> &'static str {
    match error {
        VulkanCapabilityError::NoPhysicalDevice => "no_physical_device",
        VulkanCapabilityError::ApiVersionTooLow { .. } => "api_version_too_low",
        VulkanCapabilityError::NoGraphicsQueue { .. } => "no_graphics_queue",
        VulkanCapabilityError::NoPresentQueue { .. } => "no_present_queue",
        VulkanCapabilityError::MissingSwapchainExtension { .. } => "missing_swapchain_extension",
        VulkanCapabilityError::MissingSurfaceFormat { .. } => "missing_surface_format",
        VulkanCapabilityError::MissingPresentMode { .. } => "missing_present_mode",
        VulkanCapabilityError::MissingColorAttachmentUsage { .. } => {
            "missing_color_attachment_usage"
        }
        VulkanCapabilityError::MissingDepthStencilFormat { .. } => "missing_depth_stencil_format",
    }
}

fn select_queue_families(
    device: &VulkanPhysicalDeviceRecord,
) -> Result<(u32, u32), VulkanCapabilityError> {
    if let Some(unified) = device
        .queue_families
        .iter()
        .filter(|family| family.graphics && family.present)
        .min_by_key(|family| family.index)
    {
        return Ok((unified.index, unified.index));
    }

    let graphics_queue_family = device
        .queue_families
        .iter()
        .filter(|family| family.graphics)
        .min_by_key(|family| family.index)
        .ok_or_else(|| VulkanCapabilityError::NoGraphicsQueue {
            device: device.name.clone(),
        })?
        .index;
    let present_queue_family = device
        .queue_families
        .iter()
        .filter(|family| family.present)
        .min_by_key(|family| family.index)
        .ok_or_else(|| VulkanCapabilityError::NoPresentQueue {
            device: device.name.clone(),
        })?
        .index;
    Ok((graphics_queue_family, present_queue_family))
}

fn supports_surface_formats(device: &VulkanPhysicalDeviceRecord) -> bool {
    !device.surface_formats.is_empty()
}

fn supports_color_attachment_usage(capabilities: VulkanSwapchainSurfaceCapabilities) -> bool {
    capabilities.supported_usage_flags & vk::ImageUsageFlags::COLOR_ATTACHMENT.as_raw() != 0
}

fn supports_depth_stencil_request(
    device: &VulkanPhysicalDeviceRecord,
    depth: DepthStencilSupport,
) -> bool {
    if depth.depth_bits == 0 && depth.stencil_bits == 0 {
        return true;
    }
    required_depth_stencil_formats(depth).iter().any(|format| {
        device
            .supported_depth_stencil_formats
            .contains(&format.as_raw())
    })
}

fn required_depth_stencil_formats(depth: DepthStencilSupport) -> &'static [vk::Format] {
    match (depth.depth_bits, depth.stencil_bits) {
        (0, 0) => &[],
        (16, 0) => &[vk::Format::D16_UNORM, vk::Format::D32_SFLOAT],
        (24, 0) => &[vk::Format::X8_D24_UNORM_PACK32, vk::Format::D32_SFLOAT],
        (32, 0) => &[vk::Format::D32_SFLOAT],
        (16, 8) => &[vk::Format::D16_UNORM_S8_UINT, vk::Format::D24_UNORM_S8_UINT],
        (24, 8) => &[
            vk::Format::D24_UNORM_S8_UINT,
            vk::Format::D32_SFLOAT_S8_UINT,
        ],
        (32, 8) => &[vk::Format::D32_SFLOAT_S8_UINT],
        _ => &[],
    }
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

pub(crate) fn compare_reports(
    left: &VulkanCapabilityReport,
    right: &VulkanCapabilityReport,
) -> std::cmp::Ordering {
    left.score
        .cmp(&right.score)
        .then_with(|| right.device_name.cmp(&left.device_name))
}
