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
//! Minimal `winit`-backed platform adapter shim.

use fparkan_platform::{
    EventSource, MonotonicClock, MonotonicInstant, NativeWindowHandles, PhysicalSize,
    PlatformError, PlatformEvent, RenderRequest, WindowHandle, WindowPort,
};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize as WinitPhysicalSize;
use winit::event::{Event, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::platform::scancode::PhysicalKeyExtScancode;
use winit::window::{Window, WindowId};

static NEXT_WINDOW_HANDLE_ID: AtomicU64 = AtomicU64::new(1);
const DEFAULT_SMOKE_WIDTH: u32 = 1280;
const DEFAULT_SMOKE_HEIGHT: u32 = 720;

fn next_window_id() -> u64 {
    NEXT_WINDOW_HANDLE_ID.fetch_add(1, Ordering::Relaxed)
}

/// Simple monotonic clock for windowing abstractions.
#[derive(Clone, Copy, Debug)]
pub struct WinitClock;

impl MonotonicClock for WinitClock {
    fn now(&self) -> MonotonicInstant {
        let duration = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        MonotonicInstant(duration.as_millis().try_into().unwrap_or(u64::MAX))
    }
}

/// Event source backed by pre-buffered platform events.
#[derive(Clone, Debug, Default)]
pub struct WinitEventSource {
    queue: VecDeque<PlatformEvent>,
}

impl WinitEventSource {
    /// Creates an empty source.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            queue: VecDeque::new(),
        }
    }

    /// Pushes a synthetic event (used by tests and smoke stubs).
    pub fn push(&mut self, event: PlatformEvent) {
        self.queue.push_back(event);
    }

    /// Pushes a mapped native window event.
    pub fn push_window_event(&mut self, event: &WindowEvent) {
        match event {
            WindowEvent::KeyboardInput { event, .. } => {
                self.queue.push_back(PlatformEvent::KeyboardInput {
                    scancode: event.physical_key.to_scancode().unwrap_or(0),
                    pressed: event.state.is_pressed(),
                });
            }
            WindowEvent::MouseInput { state, button, .. } => {
                self.queue.push_back(PlatformEvent::MouseInput {
                    button: mouse_button_code(*button),
                    pressed: state.is_pressed(),
                    x: 0.0,
                    y: 0.0,
                });
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.queue.push_back(PlatformEvent::CursorMoved {
                    x: position.x,
                    y: position.y,
                });
            }
            WindowEvent::Resized(size) => {
                self.queue.push_back(PlatformEvent::Resize {
                    width: size.width,
                    height: size.height,
                });
            }
            WindowEvent::Focused(focused) => {
                self.queue
                    .push_back(PlatformEvent::FocusChanged { focused: *focused });
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.queue.push_back(PlatformEvent::DpiChanged {
                    scale: *scale_factor,
                });
            }
            WindowEvent::CloseRequested => {
                self.queue.push_back(PlatformEvent::QuitRequested);
            }
            _ => {}
        }
    }

    /// Pushes events from an event loop event.
    pub fn push_event<T>(&mut self, event: &Event<T>) {
        if let Event::WindowEvent { event, .. } = event {
            self.push_window_event(event);
        }
    }
}

fn mouse_button_code(button: MouseButton) -> u16 {
    match button {
        MouseButton::Left => 0,
        MouseButton::Right => 1,
        MouseButton::Middle => 2,
        MouseButton::Back => 3,
        MouseButton::Forward => 4,
        MouseButton::Other(index) => 100 + index,
    }
}

impl EventSource for WinitEventSource {
    fn poll(&mut self, out: &mut Vec<PlatformEvent>) -> Result<(), PlatformError> {
        while let Some(event) = self.queue.pop_front() {
            out.push(event);
        }
        Ok(())
    }
}

/// Window creation plan for native smoke entrypoints.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WinitWindowPlan {
    /// Requested drawable width in physical pixels.
    pub width: u32,
    /// Requested drawable height in physical pixels.
    pub height: u32,
    /// Whether native window/display handles are required by the caller.
    pub requires_native_handles: bool,
}

impl WinitWindowPlan {
    /// Returns the Stage 0 native smoke window plan.
    #[must_use]
    pub const fn smoke() -> Self {
        Self {
            width: DEFAULT_SMOKE_WIDTH,
            height: DEFAULT_SMOKE_HEIGHT,
            requires_native_handles: true,
        }
    }

    /// Validates the window plan before a native event loop is entered.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError`] when the drawable extent is zero.
    pub fn validate(self) -> Result<Self, PlatformError> {
        if self.width == 0 || self.height == 0 {
            return Err(PlatformError::Backend {
                context: "winit window plan",
                message: "drawable extent must be non-zero".to_string(),
            });
        }
        Ok(self)
    }
}

/// Native smoke window creation result.
#[derive(Clone, Copy, Debug)]
pub struct WinitSmokeWindowProbe {
    /// Validated creation plan.
    pub plan: WinitWindowPlan,
    /// Captured window descriptor.
    pub window: WinitWindow,
}

impl WinitSmokeWindowProbe {
    /// Returns raw native handles captured from the native window.
    #[must_use]
    pub fn native_handles(&self) -> Option<NativeWindowHandles> {
        self.window.native_handles()
    }
}

/// Creates a native smoke window, captures raw handles, then exits the event loop.
///
/// # Errors
///
/// Returns [`PlatformError`] when the plan is invalid, the event loop/window
/// cannot be created, or raw native handles are unavailable.
pub fn probe_smoke_window() -> Result<WinitSmokeWindowProbe, PlatformError> {
    let plan = WinitWindowPlan::smoke().validate()?;
    let event_loop = EventLoop::new().map_err(|err| PlatformError::Backend {
        context: "winit event loop",
        message: err.to_string(),
    })?;
    let mut app = SmokeWindowApp::new(plan);
    event_loop
        .run_app(&mut app)
        .map_err(|err| PlatformError::Backend {
            context: "winit event loop",
            message: err.to_string(),
        })?;
    app.into_probe()
}

struct SmokeWindowApp {
    plan: WinitWindowPlan,
    window: Option<WinitWindow>,
    error: Option<String>,
}

impl SmokeWindowApp {
    const fn new(plan: WinitWindowPlan) -> Self {
        Self {
            plan,
            window: None,
            error: None,
        }
    }

    fn into_probe(self) -> Result<WinitSmokeWindowProbe, PlatformError> {
        if let Some(message) = self.error {
            return Err(PlatformError::Backend {
                context: "winit smoke window",
                message,
            });
        }
        let window = self.window.ok_or_else(|| PlatformError::Backend {
            context: "winit smoke window",
            message: "event loop exited before creating a window".to_string(),
        })?;
        if self.plan.requires_native_handles && window.native_handles().is_none() {
            return Err(PlatformError::Backend {
                context: "winit smoke window",
                message: "native window/display handles are unavailable".to_string(),
            });
        }
        Ok(WinitSmokeWindowProbe {
            plan: self.plan,
            window,
        })
    }
}

impl ApplicationHandler for SmokeWindowApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() || self.error.is_some() {
            event_loop.exit();
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("FParkan Vulkan smoke")
            .with_inner_size(WinitPhysicalSize::new(self.plan.width, self.plan.height));
        match event_loop.create_window(attributes) {
            Ok(window) => {
                self.window = Some(WinitWindow::from_window(&window));
            }
            Err(err) => {
                self.error = Some(err.to_string());
            }
        }
        event_loop.exit();
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        _event: WindowEvent,
    ) {
    }
}

/// Minimal window view over a `winit` window.
#[derive(Clone, Copy, Debug)]
pub struct WinitWindow {
    handle: WindowHandle,
    width: u32,
    height: u32,
    scale: f64,
    focused: bool,
    minimized: bool,
    occluded: bool,
    native_handles: Option<NativeWindowHandles>,
}

impl WinitWindow {
    /// Builds a stable descriptor from a `winit` window.
    #[must_use]
    pub fn from_window(window: &Window) -> Self {
        let scale = window.scale_factor();
        let size = window.inner_size();
        Self {
            handle: WindowHandle {
                id: next_window_id(),
            },
            width: size.width,
            height: size.height,
            scale,
            focused: true,
            minimized: false,
            occluded: false,
            native_handles: native_handles(window),
        }
    }

    /// Returns conservative defaults if a native window is not available yet.
    #[must_use]
    pub fn synthetic(width: u32, height: u32) -> Self {
        Self {
            handle: WindowHandle {
                id: next_window_id(),
            },
            width,
            height,
            scale: 1.0,
            focused: true,
            minimized: false,
            occluded: false,
            native_handles: None,
        }
    }

    /// Returns requested default render profile for integration points.
    #[must_use]
    pub const fn default_render_request() -> RenderRequest {
        RenderRequest::conservative()
    }
}

impl WindowPort for WinitWindow {
    fn drawable_size(&self) -> PhysicalSize {
        PhysicalSize {
            width: self.width,
            height: self.height,
        }
    }

    fn dpi_scale(&self) -> f64 {
        self.scale
    }

    fn has_focus(&self) -> bool {
        self.focused
    }

    fn is_minimized(&self) -> bool {
        self.minimized
    }

    fn is_occluded(&self) -> bool {
        self.occluded
    }

    fn handle(&self) -> WindowHandle {
        self.handle
    }

    fn native_handles(&self) -> Option<NativeWindowHandles> {
        self.native_handles
    }
}

fn native_handles(window: &Window) -> Option<NativeWindowHandles> {
    let display = window.display_handle().ok()?.as_raw();
    let window = window.window_handle().ok()?.as_raw();
    Some(NativeWindowHandles { display, window })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_source_buffers_synthetic_events() -> Result<(), PlatformError> {
        let mut source = WinitEventSource::new();
        source.push(PlatformEvent::Resumed);
        source.push(PlatformEvent::QuitRequested);
        let mut events = Vec::new();
        source.poll(&mut events)?;
        assert_eq!(
            events,
            vec![PlatformEvent::Resumed, PlatformEvent::QuitRequested]
        );
        Ok(())
    }

    #[test]
    fn window_port_reports_default_request_profile() {
        let window = WinitWindow::synthetic(640, 360);
        let request = WinitWindow::default_render_request();
        assert_eq!(
            request.presentation,
            fparkan_platform::PresentationMode::Fifo
        );
        assert_eq!(
            window.drawable_size(),
            PhysicalSize {
                width: 640,
                height: 360
            }
        );
        assert!(window.native_handles().is_none());
    }

    #[test]
    fn smoke_window_plan_requires_native_handles_and_nonzero_extent() -> Result<(), PlatformError> {
        let plan = WinitWindowPlan::smoke().validate()?;

        assert_eq!(plan.width, DEFAULT_SMOKE_WIDTH);
        assert_eq!(plan.height, DEFAULT_SMOKE_HEIGHT);
        assert!(plan.requires_native_handles);
        Ok(())
    }

    #[test]
    fn smoke_window_plan_rejects_zero_extent() {
        let plan = WinitWindowPlan {
            width: 0,
            height: DEFAULT_SMOKE_HEIGHT,
            requires_native_handles: true,
        };

        assert!(matches!(
            plan.validate(),
            Err(PlatformError::Backend {
                context: "winit window plan",
                ..
            })
        ));
    }

    #[test]
    fn smoke_window_app_requires_created_native_window() {
        let app = SmokeWindowApp::new(WinitWindowPlan::smoke());

        assert!(matches!(
            app.into_probe(),
            Err(PlatformError::Backend {
                context: "winit smoke window",
                ..
            })
        ));
    }

    #[test]
    fn smoke_window_app_rejects_synthetic_window_without_native_handles() {
        let mut app = SmokeWindowApp::new(WinitWindowPlan::smoke());
        app.window = Some(WinitWindow::synthetic(
            DEFAULT_SMOKE_WIDTH,
            DEFAULT_SMOKE_HEIGHT,
        ));

        assert!(matches!(
            app.into_probe(),
            Err(PlatformError::Backend {
                context: "winit smoke window",
                ..
            })
        ));
    }

    #[test]
    fn window_events_push_expected_platform_events() {
        let mut source = WinitEventSource::new();
        let size = winit::dpi::PhysicalSize::new(1024u32, 768u32);

        source.push_window_event(&WindowEvent::Resized(size));
        source.push_window_event(&WindowEvent::Focused(false));
        source.push_window_event(&WindowEvent::CloseRequested);

        let mut events = Vec::new();
        source
            .poll(&mut events)
            .expect("platform event pump should never fail");

        assert!(events.contains(&PlatformEvent::Resize {
            width: 1024,
            height: 768,
        }));
        assert!(events.contains(&PlatformEvent::FocusChanged { focused: false }));
        assert!(events.contains(&PlatformEvent::QuitRequested));
    }
}

// SAFETY: no unsafe usage in this crate.
