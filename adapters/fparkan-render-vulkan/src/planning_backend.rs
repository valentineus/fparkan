use ash::vk;
use fparkan_platform::RenderRequest;
use fparkan_render::{
    canonical_capture, FrameOutput, RenderBackend, RenderCommandList, RenderError,
};

use crate::{
    plan_vulkan_frame_submission, VulkanFrameSubmissionPlan, VulkanSurfaceFormat,
    VulkanSwapchainPlan,
};

/// Vulkan backend migration readiness.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum VulkanPlanningBackendState {
    /// Planning facade is configured and able to accept command lists.
    #[default]
    Configured,
    /// Adapter is tracking a recoverable runtime surface/depth pipeline fault.
    Degraded,
    /// Adapter has encountered a non-recoverable error.
    Error,
}

/// Diagnostics for planning-facade request tracking.
#[derive(Clone, Debug, PartialEq)]
pub struct VulkanPlanningRequestReport {
    /// Last render request observed by the planning facade.
    pub current_request: RenderRequest,
    /// Number of meaningful request updates applied to the facade.
    pub request_updates: u64,
}

impl Default for VulkanPlanningRequestReport {
    fn default() -> Self {
        Self {
            current_request: RenderRequest::conservative(),
            request_updates: 0,
        }
    }
}

/// Diagnostics for planning-facade execution telemetry.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VulkanPlanningExecutionReport {
    /// Total frames planned by the facade.
    pub planned_frames: u64,
    /// Total frame-submission plans emitted by the facade.
    pub submission_plans: u64,
    /// Last command-capture byte size.
    pub last_capture_size: usize,
    /// Number of simulated present calls issued by the planning facade.
    pub simulated_presents: u64,
}

/// Diagnostics for Vulkan planning backend setup and frame progression.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VulkanPlanningBackendReport {
    /// Request-tracking telemetry.
    pub request: VulkanPlanningRequestReport,
    /// Execution-planning telemetry.
    pub execution: VulkanPlanningExecutionReport,
    /// Last deterministic frame submission plan.
    pub last_frame_submission: Option<VulkanFrameSubmissionPlan>,
}

/// Vulkan planning backend facade used by the game entrypoint.
#[derive(Debug)]
pub struct VulkanPlanningBackend {
    state: VulkanPlanningBackendState,
    report: VulkanPlanningBackendReport,
    swapchain_plan: VulkanSwapchainPlan,
}

impl Default for VulkanPlanningBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl VulkanPlanningBackend {
    /// Creates a new Vulkan planning backend facade.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: VulkanPlanningBackendState::Configured,
            report: VulkanPlanningBackendReport::default(),
            swapchain_plan: default_stage0_swapchain_plan(),
        }
    }

    /// Replaces active surface/profile request.
    pub fn set_render_request(&mut self, request: RenderRequest) {
        if self.report.request.current_request != request {
            self.report.request.current_request = request;
            self.report.request.request_updates =
                self.report.request.request_updates.saturating_add(1);
        }
    }

    /// Returns active render request policy.
    #[must_use]
    pub const fn render_request(&self) -> RenderRequest {
        self.report.request.current_request
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
    pub const fn state(&self) -> VulkanPlanningBackendState {
        self.state
    }

    /// Returns backend report.
    #[must_use]
    pub fn report(&self) -> &VulkanPlanningBackendReport {
        &self.report
    }

    fn simulate_present(&mut self) {
        self.report.execution.simulated_presents =
            self.report.execution.simulated_presents.saturating_add(1);
    }
}

impl RenderBackend for VulkanPlanningBackend {
    fn execute(&mut self, commands: &RenderCommandList) -> Result<FrameOutput, RenderError> {
        if !matches!(
            self.state,
            VulkanPlanningBackendState::Configured | VulkanPlanningBackendState::Degraded
        ) {
            return Err(RenderError::InvalidRange);
        }
        let capture = canonical_capture(commands)?;
        let frame_plan = plan_vulkan_frame_submission(&self.swapchain_plan, commands)?;
        self.report.execution.planned_frames =
            self.report.execution.planned_frames.saturating_add(1);
        self.report.execution.submission_plans =
            self.report.execution.submission_plans.saturating_add(1);
        self.report.execution.last_capture_size = capture.len();
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
