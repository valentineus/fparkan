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
