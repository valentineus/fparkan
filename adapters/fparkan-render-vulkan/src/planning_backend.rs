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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VulkanPlanningBackendState {
    /// Adapter prepared and able to accept commands.
    Ready,
    /// Adapter is tracking a recoverable runtime surface/depth pipeline fault.
    Degraded,
    /// Adapter has encountered a non-recoverable error.
    Error,
}

impl Default for VulkanPlanningBackendState {
    fn default() -> Self {
        Self::Degraded
    }
}

/// Diagnostics for Vulkan planning backend setup and frame progression.
#[derive(Clone, Debug, PartialEq)]
pub struct VulkanPlanningBackendReport {
    /// Total frames executed.
    pub frames_executed: u64,
    /// Total command submissions.
    pub submissions: u64,
    /// Last command-capture byte size.
    pub last_capture_size: usize,
    /// Number of simulated present calls issued by the planning facade.
    pub simulated_presents: u64,
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
            frames_executed: 0,
            submissions: 0,
            last_capture_size: 0,
            simulated_presents: 0,
            resize_rebuilds: 0,
            request: RenderRequest::conservative(),
            last_frame_submission: None,
        }
    }
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
            state: VulkanPlanningBackendState::Ready,
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
    pub const fn state(&self) -> VulkanPlanningBackendState {
        self.state
    }

    /// Returns backend report.
    #[must_use]
    pub fn report(&self) -> &VulkanPlanningBackendReport {
        &self.report
    }

    fn simulate_present(&mut self) {
        self.report.simulated_presents = self.report.simulated_presents.saturating_add(1);
    }
}

impl RenderBackend for VulkanPlanningBackend {
    fn execute(&mut self, commands: &RenderCommandList) -> Result<FrameOutput, RenderError> {
        if !matches!(
            self.state,
            VulkanPlanningBackendState::Ready | VulkanPlanningBackendState::Degraded
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
