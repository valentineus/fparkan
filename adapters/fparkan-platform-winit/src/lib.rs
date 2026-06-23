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
use winit::event::{Event, MouseButton, WindowEvent};
use winit::platform::scancode::PhysicalKeyExtScancode;
use winit::window::Window;

static NEXT_WINDOW_HANDLE_ID: AtomicU64 = AtomicU64::new(1);

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

/// Minimal window view over a `winit` window.
#[derive(Clone, Debug)]
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
