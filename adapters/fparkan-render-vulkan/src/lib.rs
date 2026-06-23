#![forbid(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]
//! Vulkan adapter facade and migration-ready backend surface contract.
//!
//! This module intentionally keeps backend-agnostic command validation in the
//! shared render crate while exposing deterministic lifecycle telemetry used by
//! Stage 0 acceptance evidence.
//!
//! This crate is the declared low-level Vulkan boundary.

use fparkan_render::{
    canonical_capture, FrameOutput, RenderBackend, RenderCommandList, RenderError,
};
use fparkan_platform::RenderRequest;
use std::time::{SystemTime, UNIX_EPOCH};

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
        if !matches!(self.state, VulkanBackendState::Ready | VulkanBackendState::Degraded) {
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
}
