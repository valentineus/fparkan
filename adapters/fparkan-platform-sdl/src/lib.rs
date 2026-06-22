#![forbid(unsafe_code)]
//! SDL platform adapter boundary stubs behind safe `FParkan` ports.

use fparkan_platform::{
    EventSource, GraphicsContextRequest, GraphicsProfile, PhysicalSize, PlatformError,
    PlatformEvent, Version, WindowPort,
};

/// Adapter capabilities compiled into this package.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SdlAdapterCapabilities {
    /// Supported graphics context requests in preference order.
    pub graphics: Vec<GraphicsContextRequest>,
    /// Whether adapter-owned code is free of `unsafe`.
    pub project_owned_unsafe_free: bool,
}

impl Default for SdlAdapterCapabilities {
    fn default() -> Self {
        Self {
            graphics: vec![
                GraphicsContextRequest {
                    profile: GraphicsProfile::DesktopCore,
                    version: Version { major: 3, minor: 3 },
                },
                GraphicsContextRequest {
                    profile: GraphicsProfile::Embedded,
                    version: Version { major: 2, minor: 0 },
                },
            ],
            project_owned_unsafe_free: true,
        }
    }
}

/// Returns whether the project-owned adapter boundary avoids `unsafe`.
#[must_use]
pub fn project_owned_layer_unsafe_free() -> bool {
    SdlAdapterCapabilities::default().project_owned_unsafe_free
}

/// In-memory event source used by adapter smoke tests before a concrete SDL
/// runtime is selected.
#[derive(Clone, Debug, Default)]
pub struct SdlEventSourceStub {
    pending: Vec<PlatformEvent>,
}

impl SdlEventSourceStub {
    /// Creates an event source with deterministic pending events.
    #[must_use]
    pub fn new(pending: Vec<PlatformEvent>) -> Self {
        Self { pending }
    }
}

impl EventSource for SdlEventSourceStub {
    fn poll(&mut self, out: &mut Vec<PlatformEvent>) -> Result<(), PlatformError> {
        out.append(&mut self.pending);
        Ok(())
    }
}

/// Safe window-port stub with SDL-compatible drawable-size semantics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SdlWindowStub {
    size: PhysicalSize,
    presents: u64,
}

impl SdlWindowStub {
    /// Creates a stub window with a fixed drawable size.
    #[must_use]
    pub fn new(size: PhysicalSize) -> Self {
        Self { size, presents: 0 }
    }

    /// Number of successful present calls.
    #[must_use]
    pub fn presents(&self) -> u64 {
        self.presents
    }
}

impl WindowPort for SdlWindowStub {
    fn drawable_size(&self) -> PhysicalSize {
        self.size
    }

    fn present(&mut self) -> Result<(), PlatformError> {
        self.presents = self.presents.saturating_add(1);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_boundary_is_project_owned_unsafe_free() {
        assert!(project_owned_layer_unsafe_free());
        assert_eq!(SdlAdapterCapabilities::default().graphics.len(), 2);
    }

    #[test]
    fn event_source_and_window_ports_are_deterministic() -> Result<(), PlatformError> {
        let mut source = SdlEventSourceStub::new(vec![PlatformEvent::Quit]);
        let mut events = Vec::new();
        source.poll(&mut events)?;
        source.poll(&mut events)?;
        assert_eq!(events, vec![PlatformEvent::Quit]);

        let mut window = SdlWindowStub::new(PhysicalSize {
            width: 320,
            height: 240,
        });
        assert_eq!(window.drawable_size().width, 320);
        window.present()?;
        assert_eq!(window.presents(), 1);
        Ok(())
    }
}
