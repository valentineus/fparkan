#![forbid(unsafe_code)]
//! Dev-only synthetic builders and fake ports.

use fparkan_render::{FrameOutput, RenderBackend, RenderCommandList, RenderError};

/// Fake clock.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FakeClock {
    /// Current tick.
    pub tick: u64,
}

/// Recording backend.
#[derive(Clone, Debug, Default)]
pub struct RecordingRenderBackend {
    /// Recorded command lists.
    pub captures: Vec<RenderCommandList>,
}

impl RenderBackend for RecordingRenderBackend {
    fn execute(&mut self, commands: &RenderCommandList) -> Result<FrameOutput, RenderError> {
        self.captures.push(commands.clone());
        Ok(FrameOutput)
    }
}
