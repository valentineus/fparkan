#![forbid(unsafe_code)]
//! Platform ports for clocks, input, events, windows, and graphics requests.

/// Monotonic instant.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MonotonicInstant(pub u64);

/// Monotonic clock.
pub trait MonotonicClock {
    /// Current instant.
    fn now(&self) -> MonotonicInstant;
}

/// Platform event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlatformEvent {
    /// Quit requested.
    Quit,
}

/// Platform error.
#[derive(Debug)]
pub enum PlatformError {
    /// Backend failed.
    Backend,
}

impl std::fmt::Display for PlatformError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for PlatformError {}

/// Event source.
pub trait EventSource {
    /// Polls events.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError`] when the backend cannot collect events.
    fn poll(&mut self, out: &mut Vec<PlatformEvent>) -> Result<(), PlatformError>;
}

/// Physical size.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhysicalSize {
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
}

/// Window port.
pub trait WindowPort {
    /// Drawable size.
    fn drawable_size(&self) -> PhysicalSize;
    /// Presents.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError`] when the backend cannot present the current
    /// frame.
    fn present(&mut self) -> Result<(), PlatformError>;
}

/// Graphics profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphicsProfile {
    /// Desktop core.
    DesktopCore,
    /// Embedded profile.
    Embedded,
}

/// Version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Version {
    /// Major.
    pub major: u8,
    /// Minor.
    pub minor: u8,
}

/// Graphics context request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphicsContextRequest {
    /// Profile.
    pub profile: GraphicsProfile,
    /// Version.
    pub version: Version,
}
