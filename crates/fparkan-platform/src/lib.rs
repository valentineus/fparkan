#![forbid(unsafe_code)]
//! Platform ports for clocks, event sources and window descriptors.

/// Monotonic instant measured in milliseconds since process start.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MonotonicInstant(pub u64);

/// Platform clock.
pub trait MonotonicClock {
    /// Current instant.
    fn now(&self) -> MonotonicInstant;
}

/// Platform event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlatformEvent {
    /// Window/application requested to quit.
    QuitRequested,
    /// Window focus changed.
    FocusChanged { focused: bool },
    /// Window resize or move to a new drawable size.
    Resize { width: u32, height: u32 },
    /// Device pixel ratio changed.
    DpiChanged { scale: f64 },
    /// Window minimized/hidden.
    Minimized { minimized: bool },
    /// Window occlusion state changed.
    Occluded { occluded: bool },
    /// Window is being suspended.
    Suspended,
    /// Window resumed from suspend.
    Resumed,
    /// Keyboard/scancode input.
    KeyboardInput {
        /// Platform scancode.
        scancode: u32,
        /// Pressed state.
        pressed: bool,
    },
    /// Mouse button input.
    MouseInput {
        /// Mouse button code.
        button: u16,
        /// Pressed state.
        pressed: bool,
        /// X position in window coordinates.
        x: f64,
        /// Y position in window coordinates.
        y: f64,
    },
    /// Mouse cursor movement.
    CursorMoved {
        /// Cursor x.
        x: f64,
        /// Cursor y.
        y: f64,
    },
}

/// Platform error with optional source detail.
#[derive(Debug)]
pub enum PlatformError {
    /// Backend/backend-specific failure.
    Backend {
        /// Operation or subsystem.
        context: &'static str,
        /// Human-readable details.
        message: String,
    },
}

impl std::fmt::Display for PlatformError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Backend { context, message } => {
                write!(f, "{context}: {message}")
            }
        }
    }
}

impl std::error::Error for PlatformError {}

/// Event source contract for polling platform events.
pub trait EventSource {
    /// Polls events.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError`] when the backend cannot collect events.
    fn poll(&mut self, out: &mut Vec<PlatformEvent>) -> Result<(), PlatformError>;
}

/// Physical window size.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhysicalSize {
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
}

/// Window identity as a stable opaque handle token.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct WindowHandle {
    /// Opaque integer token.
    pub id: u64,
}

/// Window presentation and lifecycle port.
///
/// Presentation is not owned by the window abstraction. Render adapters
/// own swapchain and present lifecycle.
pub trait WindowPort {
    /// Current drawable size.
    fn drawable_size(&self) -> PhysicalSize;
    /// DPI scale for this window.
    fn dpi_scale(&self) -> f64;
    /// Whether the window is focused.
    fn has_focus(&self) -> bool;
    /// Whether the window is minimized.
    fn is_minimized(&self) -> bool;
    /// Whether the window is occluded.
    fn is_occluded(&self) -> bool;
    /// Opaque window identity.
    fn handle(&self) -> WindowHandle;
}

/// Render backend request contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderRequest {
    /// Preferred color-space profile.
    pub color_space: ColorSpace,
    /// Preferred presentation mode.
    pub presentation: PresentationMode,
    /// Requested depth/stencil format.
    pub depth: DepthStencilSupport,
}

/// Color-space profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColorSpace {
    /// sRGB nonlinear.
    Srgb,
    /// Linear color-space.
    Linear,
}

/// Presentation mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PresentationMode {
    /// VSync.
    Fifo,
    /// No VSync.
    Immediate,
    /// Triple-buffer mailbox fallback.
    Mailbox,
}

/// Depth/stencil support profile requested by the composition root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DepthStencilSupport {
    /// Depth bits.
    pub depth_bits: u8,
    /// Stencil bits.
    pub stencil_bits: u8,
}

impl RenderRequest {
    /// Returns a conservative default request.
    #[must_use]
    pub const fn conservative() -> Self {
        Self {
            color_space: ColorSpace::Srgb,
            presentation: PresentationMode::Fifo,
            depth: DepthStencilSupport {
                depth_bits: 24,
                stencil_bits: 8,
            },
        }
    }
}
