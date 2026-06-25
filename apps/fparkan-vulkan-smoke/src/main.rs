#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::expect_used,
        clippy::needless_raw_string_hashes,
        clippy::panic,
        clippy::unwrap_used
    )
)]
#![allow(clippy::print_stderr, clippy::print_stdout)]
//! Native Vulkan smoke runner entrypoint.

use fparkan_platform_winit::{window_native_handles, WinitWindowPlan};
use fparkan_render_vulkan::{
    VulkanSmokeFrameOutcome, VulkanSmokeRenderer, VulkanSmokeRendererCreateInfo,
};
use serde::Serialize;
use std::path::PathBuf;
use std::process::Command;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowId};

const SCHEMA_VERSION: &str = "fparkan-native-smoke-v1";
const DEFAULT_TARGET_FRAMES: u32 = 300;
const DEFAULT_RESIZE_FRAME: u32 = 120;
const DEFAULT_RESIZE_WIDTH: u32 = 960;
const DEFAULT_RESIZE_HEIGHT: u32 = 540;

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let code = match run(&args) {
        Ok(output) => {
            println!("{output}");
            0
        }
        Err(err) => {
            eprintln!("{err}");
            2
        }
    };
    std::process::exit(code);
}

fn run(args: &[String]) -> Result<String, String> {
    let options = SmokeOptions::parse(args)?;
    let event_loop = EventLoop::new().map_err(|err| format!("winit event loop: {err}"))?;
    let mut app = SmokeApp::new(options);
    event_loop
        .run_app(&mut app)
        .map_err(|err| format!("winit event loop: {err}"))?;
    app.finish()
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SmokeOptions {
    out: PathBuf,
    frames: u32,
    resize_frame: u32,
}

impl SmokeOptions {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut out = None;
        let mut frames = DEFAULT_TARGET_FRAMES;
        let mut resize_frame = DEFAULT_RESIZE_FRAME;
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--out" => {
                    out = Some(
                        iter.next()
                            .map(PathBuf::from)
                            .ok_or_else(|| "--out requires a path".to_string())?,
                    );
                }
                "--frames" => {
                    frames = iter
                        .next()
                        .ok_or_else(|| "--frames requires a value".to_string())?
                        .parse()
                        .map_err(|_| "--frames must be an integer".to_string())?;
                }
                "--resize-frame" => {
                    resize_frame = iter
                        .next()
                        .ok_or_else(|| "--resize-frame requires a value".to_string())?
                        .parse()
                        .map_err(|_| "--resize-frame must be an integer".to_string())?;
                }
                _ => return Err(format!("unknown native smoke option: {arg}")),
            }
        }
        let out = out.ok_or_else(|| "missing --out".to_string())?;
        if frames < DEFAULT_TARGET_FRAMES {
            return Err(format!(
                "native smoke requires --frames >= {DEFAULT_TARGET_FRAMES}"
            ));
        }
        Ok(Self {
            out,
            frames,
            resize_frame,
        })
    }
}

struct SmokeApp {
    options: SmokeOptions,
    window_id: Option<WindowId>,
    window: Option<Window>,
    renderer: Option<VulkanSmokeRenderer>,
    error: Option<String>,
    output: Option<String>,
    frames_presented: u32,
    resize_count: u32,
    resize_requested: bool,
    last_size: Option<(u32, u32)>,
}

impl SmokeApp {
    const fn new(options: SmokeOptions) -> Self {
        Self {
            options,
            window_id: None,
            window: None,
            renderer: None,
            error: None,
            output: None,
            frames_presented: 0,
            resize_count: 0,
            resize_requested: false,
            last_size: None,
        }
    }

    fn finish(self) -> Result<String, String> {
        if let Some(error) = self.error {
            return Err(error);
        }
        self.output
            .ok_or_else(|| "native smoke exited before producing a report".to_string())
    }

    fn schedule_next_redraw(&self) {
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    fn complete(&mut self, event_loop: &ActiveEventLoop) {
        let Some(renderer) = self.renderer.as_ref() else {
            self.error = Some("native smoke renderer was not initialized".to_string());
            event_loop.exit();
            return;
        };
        let validation = renderer.validation_report();
        if self.frames_presented < self.options.frames {
            self.error = Some("native smoke did not reach the required frame count".to_string());
            event_loop.exit();
            return;
        }
        if self.resize_count == 0 || renderer.swapchain_recreate_count() == 0 {
            self.error = Some(
                "native smoke requires at least one measured resize and swapchain recreation"
                    .to_string(),
            );
            event_loop.exit();
            return;
        }
        if validation.warning_count != 0 || validation.error_count != 0 {
            self.error = Some(format!(
                "native smoke validation must stay clean (warnings={}, errors={})",
                validation.warning_count, validation.error_count
            ));
            event_loop.exit();
            return;
        }
        let report = match render_smoke_report_json(
            &self.options,
            renderer,
            self.frames_presented,
            self.resize_count,
            validation.warning_count,
            validation.error_count,
            &validation.vuids,
        ) {
            Ok(report) => report,
            Err(err) => {
                self.error = Some(err);
                event_loop.exit();
                return;
            }
        };
        if let Some(parent) = self.options.out.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                self.error = Some(format!("{}: {err}", parent.display()));
                event_loop.exit();
                return;
            }
        }
        if let Err(err) = std::fs::write(&self.options.out, &report) {
            self.error = Some(format!("{}: {err}", self.options.out.display()));
            event_loop.exit();
            return;
        }
        self.output = Some(report);
        event_loop.exit();
    }

    fn request_controlled_resize(&mut self) {
        if self.resize_requested {
            return;
        }
        let Some(window) = self.window.as_ref() else {
            return;
        };
        self.resize_requested = true;
        let requested = PhysicalSize::new(DEFAULT_RESIZE_WIDTH, DEFAULT_RESIZE_HEIGHT);
        let _ = window.request_inner_size(requested);
    }
}

impl ApplicationHandler for SmokeApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let plan = match WinitWindowPlan::smoke().validate() {
            Ok(plan) => plan,
            Err(err) => {
                self.error = Some(err.to_string());
                event_loop.exit();
                return;
            }
        };
        let attributes = Window::default_attributes()
            .with_title("FParkan Vulkan smoke")
            .with_inner_size(PhysicalSize::new(plan.width, plan.height));
        let window = match event_loop.create_window(attributes) {
            Ok(window) => window,
            Err(err) => {
                self.error = Some(format!("winit window: {err}"));
                event_loop.exit();
                return;
            }
        };
        let Some(native_handles) = window_native_handles(&window) else {
            self.error = Some("winit window does not expose native handles".to_string());
            event_loop.exit();
            return;
        };
        let size = window.inner_size();
        let renderer = match VulkanSmokeRenderer::new(&VulkanSmokeRendererCreateInfo {
            application_name: "fparkan-vulkan-smoke".to_string(),
            native_handles,
            drawable_extent: (size.width.max(1), size.height.max(1)),
            enable_validation: true,
        }) {
            Ok(renderer) => renderer,
            Err(err) => {
                self.error = Some(err.to_string());
                event_loop.exit();
                return;
            }
        };
        self.last_size = Some((size.width, size.height));
        self.window_id = Some(window.id());
        self.renderer = Some(renderer);
        self.window = Some(window);
        self.schedule_next_redraw();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if Some(window_id) != self.window_id {
            return;
        }
        match event {
            WindowEvent::CloseRequested => {
                if self.output.is_none() {
                    self.error = Some("native smoke window closed before completion".to_string());
                }
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                if self
                    .last_size
                    .is_some_and(|last| last != (size.width, size.height))
                {
                    self.resize_count = self.resize_count.saturating_add(1);
                }
                self.last_size = Some((size.width, size.height));
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.request_resize((size.width, size.height));
                }
            }
            WindowEvent::RedrawRequested => {
                let Some(renderer) = self.renderer.as_mut() else {
                    self.error = Some("native smoke renderer was not initialized".to_string());
                    event_loop.exit();
                    return;
                };
                match renderer.draw_frame() {
                    Ok(VulkanSmokeFrameOutcome::Presented) => {
                        self.frames_presented = self.frames_presented.saturating_add(1);
                    }
                    Ok(
                        VulkanSmokeFrameOutcome::Recreated | VulkanSmokeFrameOutcome::ZeroExtent,
                    ) => {}
                    Err(err) => {
                        self.error = Some(err.to_string());
                        event_loop.exit();
                        return;
                    }
                }
                let recreate_count = renderer.swapchain_recreate_count();
                let should_request_resize =
                    !self.resize_requested && self.frames_presented >= self.options.resize_frame;
                let should_complete = self.frames_presented >= self.options.frames
                    && self.resize_count > 0
                    && recreate_count > 0;
                let _ = renderer;
                if should_request_resize {
                    self.request_controlled_resize();
                }
                if should_complete {
                    self.complete(event_loop);
                } else {
                    self.schedule_next_redraw();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if self.output.is_none() && self.error.is_none() {
            self.schedule_next_redraw();
        }
    }
}

#[derive(Serialize)]
struct SmokeReport<'a> {
    schema_version: &'static str,
    commit_sha: String,
    git_dirty: bool,
    runner_identity: String,
    rust_toolchain: String,
    target_triple: String,
    platform: &'static str,
    status: &'static str,
    frames: u32,
    resize_count: u32,
    swapchain_recreate_count: u32,
    validation_warning_count: u32,
    validation_error_count: u32,
    validation_vuids: &'a [String],
    requested_frames: u32,
    shader_manifest_hash: &'a str,
    vulkan_loader_status: &'static str,
    vulkan_instance_status: &'static str,
    window_status: &'static str,
    vulkan_surface_status: &'static str,
    vulkan_device_status: &'static str,
    vulkan_device_name: &'a str,
    vulkan_logical_device_status: &'static str,
    vulkan_logical_device_graphics_queue_family: u32,
    vulkan_logical_device_present_queue_family: u32,
    vulkan_logical_device_enabled_extension_count: u32,
    vulkan_swapchain_status: &'static str,
    vulkan_swapchain_width: u32,
    vulkan_swapchain_height: u32,
    vulkan_swapchain_image_count: u32,
    vulkan_portability_enumeration: bool,
}

fn render_smoke_report_json(
    options: &SmokeOptions,
    renderer: &VulkanSmokeRenderer,
    frames_presented: u32,
    resize_count: u32,
    validation_warning_count: u32,
    validation_error_count: u32,
    validation_vuids: &[String],
) -> Result<String, String> {
    let report = renderer.report();
    let smoke_report = SmokeReport {
        schema_version: SCHEMA_VERSION,
        commit_sha: current_git_commit_sha(),
        git_dirty: current_git_dirty(),
        runner_identity: measured_runner_identity(),
        rust_toolchain: current_rustc_release(),
        target_triple: current_rustc_host_triple(),
        platform: actual_platform(),
        status: "passed",
        frames: frames_presented,
        resize_count,
        swapchain_recreate_count: renderer.swapchain_recreate_count(),
        validation_warning_count,
        validation_error_count,
        validation_vuids,
        requested_frames: options.frames,
        shader_manifest_hash: &report.shader_manifest_hash,
        vulkan_loader_status: "available",
        vulkan_instance_status: "created",
        window_status: "created",
        vulkan_surface_status: "created",
        vulkan_device_status: "selected",
        vulkan_device_name: &report.device_name,
        vulkan_logical_device_status: "created",
        vulkan_logical_device_graphics_queue_family: report.graphics_queue_family,
        vulkan_logical_device_present_queue_family: report.present_queue_family,
        vulkan_logical_device_enabled_extension_count: report.enabled_extension_count,
        vulkan_swapchain_status: "created",
        vulkan_swapchain_width: report.swapchain_extent.0,
        vulkan_swapchain_height: report.swapchain_extent.1,
        vulkan_swapchain_image_count: report.swapchain_image_count,
        vulkan_portability_enumeration: report.portability_enumeration,
    };
    serde_json::to_string_pretty(&smoke_report)
        .map(|json| format!("{json}\n"))
        .map_err(|err| format!("native smoke report serialization failed: {err}"))
}

fn actual_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "macos",
        "linux" => "linux",
        "windows" => "windows",
        other => other,
    }
}

fn current_git_commit_sha() -> String {
    Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| value.len() == 40 && value.chars().all(|ch| ch.is_ascii_hexdigit()))
        .unwrap_or_else(|| "unknown".to_string())
}

fn current_git_dirty() -> bool {
    Command::new("git")
        .args(["status", "--short"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .is_some_and(|output| !output.trim().is_empty())
}

fn current_rustc_release() -> String {
    Command::new("rustc")
        .arg("-Vv")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|output| {
            output
                .lines()
                .find_map(|line| line.strip_prefix("release: ").map(ToString::to_string))
        })
        .unwrap_or_else(|| "unknown".to_string())
}

fn measured_runner_identity() -> String {
    if std::env::var_os("GITHUB_ACTIONS").is_some() {
        let run_id = std::env::var("GITHUB_RUN_ID").unwrap_or_else(|_| "unknown-run".to_string());
        let job = std::env::var("GITHUB_JOB").unwrap_or_else(|_| "unknown-job".to_string());
        format!("github-actions/{run_id}/{job}")
    } else if std::env::var_os("CI").is_some() {
        let job = std::env::var("CI_JOB_NAME")
            .or_else(|_| std::env::var("BUILD_ID"))
            .unwrap_or_else(|_| "generic-ci".to_string());
        format!("ci/{job}")
    } else {
        format!("local/{}", std::env::consts::OS)
    }
}

fn current_rustc_host_triple() -> String {
    Command::new("rustc")
        .arg("-Vv")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|output| {
            output
                .lines()
                .find_map(|line| line.strip_prefix("host: ").map(ToString::to_string))
        })
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_required_args() {
        let parsed = SmokeOptions::parse(&["--out".to_string(), "target/report.json".to_string()]);

        assert_eq!(
            parsed,
            Ok(SmokeOptions {
                out: PathBuf::from("target/report.json"),
                frames: DEFAULT_TARGET_FRAMES,
                resize_frame: DEFAULT_RESIZE_FRAME,
            })
        );
    }

    #[test]
    fn rejects_too_few_frames() {
        let parsed = SmokeOptions::parse(&[
            "--out".to_string(),
            "target/report.json".to_string(),
            "--frames".to_string(),
            "299".to_string(),
        ]);

        assert_eq!(
            parsed,
            Err("native smoke requires --frames >= 300".to_string())
        );
    }

    #[test]
    fn rejects_deprecated_self_assertion_flags() {
        for flag in [
            "--status",
            "--platform",
            "--validation-error-count",
            "--resize-count",
            "--swapchain-recreate-count",
        ] {
            let parsed = SmokeOptions::parse(&[
                "--out".to_string(),
                "target/report.json".to_string(),
                flag.to_string(),
                "value".to_string(),
            ]);

            assert_eq!(parsed, Err(format!("unknown native smoke option: {flag}")));
        }
    }

    #[test]
    fn smoke_report_json_contains_expected_fields() {
        let json = serde_json::to_string_pretty(&SmokeReport {
            schema_version: SCHEMA_VERSION,
            commit_sha: "0123456789abcdef0123456789abcdef01234567".to_string(),
            git_dirty: false,
            runner_identity: "github-actions/12345/stage0-macos".to_string(),
            rust_toolchain: "1.87.0".to_string(),
            target_triple: "aarch64-apple-darwin".to_string(),
            platform: "macos",
            status: "passed",
            frames: 300,
            resize_count: 1,
            swapchain_recreate_count: 1,
            validation_warning_count: 0,
            validation_error_count: 0,
            validation_vuids: &["VUID-A".to_string(), "VUID-B".to_string()],
            requested_frames: 300,
            shader_manifest_hash: "deadbeef",
            vulkan_loader_status: "available",
            vulkan_instance_status: "created",
            window_status: "created",
            vulkan_surface_status: "created",
            vulkan_device_status: "selected",
            vulkan_device_name: "Apple GPU",
            vulkan_logical_device_status: "created",
            vulkan_logical_device_graphics_queue_family: 0,
            vulkan_logical_device_present_queue_family: 0,
            vulkan_logical_device_enabled_extension_count: 2,
            vulkan_swapchain_status: "created",
            vulkan_swapchain_width: 960,
            vulkan_swapchain_height: 540,
            vulkan_swapchain_image_count: 3,
            vulkan_portability_enumeration: true,
        })
        .expect("smoke report should serialize");

        assert!(json.contains("\"schema_version\": \"fparkan-native-smoke-v1\""));
        assert!(json.contains("\"validation_vuids\": ["));
        assert!(json.contains("\"vulkan_device_name\": \"Apple GPU\""));
    }
}
