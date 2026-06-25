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
use std::time::{Duration, Instant};
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
const DEFAULT_TIMEOUT_SECONDS: u64 = 120;

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
    if let Err(err) = event_loop.run_app(&mut app) {
        app.error = Some(format!("winit event loop: {err}"));
    }
    app.finish()
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SmokeOptions {
    out: PathBuf,
    frames: u32,
    resize_frame: u32,
    timeout_seconds: u64,
}

impl SmokeOptions {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut out = None;
        let mut frames = DEFAULT_TARGET_FRAMES;
        let mut resize_frame = DEFAULT_RESIZE_FRAME;
        let mut timeout_seconds = DEFAULT_TIMEOUT_SECONDS;
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
                "--timeout-seconds" => {
                    timeout_seconds = iter
                        .next()
                        .ok_or_else(|| "--timeout-seconds requires a value".to_string())?
                        .parse()
                        .map_err(|_| "--timeout-seconds must be an integer".to_string())?;
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
        if timeout_seconds == 0 {
            return Err("native smoke requires --timeout-seconds >= 1".to_string());
        }
        Ok(Self {
            out,
            frames,
            resize_frame,
            timeout_seconds,
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
    started_at: Instant,
}

impl SmokeApp {
    fn new(options: SmokeOptions) -> Self {
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
            started_at: Instant::now(),
        }
    }

    fn finish(mut self) -> Result<String, String> {
        if let Some(output) = self.output.take() {
            return Ok(output);
        }
        let error = self
            .error
            .clone()
            .unwrap_or_else(|| "native smoke exited before producing a report".to_string());
        self.write_failure_report(&error)?;
        Err(error)
    }

    fn schedule_next_redraw(&self) {
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    fn write_report(&self, report: &str) -> Result<(), String> {
        if let Some(parent) = self.options.out.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("{}: {err}", parent.display()))?;
        }
        std::fs::write(&self.options.out, report)
            .map_err(|err| format!("{}: {err}", self.options.out.display()))
    }

    fn render_report(
        &self,
        status: &'static str,
        failure_reason: Option<&str>,
    ) -> Result<String, String> {
        let renderer = self.renderer.as_ref();
        let validation = renderer.map_or(
            fparkan_render_vulkan::VulkanValidationReport {
                warning_count: 0,
                error_count: 0,
                vuids: Vec::new(),
            },
            VulkanSmokeRenderer::validation_report,
        );
        let smoke_report = SmokeReport {
            schema_version: SCHEMA_VERSION,
            commit_sha: compiled_commit_sha(),
            git_dirty: compiled_git_dirty(),
            runner_identity: measured_runner_identity(),
            runner_architecture: actual_architecture(),
            rust_toolchain: compiled_rust_toolchain(),
            target_triple: compiled_target_triple(),
            platform: actual_platform(),
            status,
            failure_reason,
            frames: self.frames_presented,
            resize_count: self.resize_count,
            swapchain_recreate_count: renderer
                .map_or(0, VulkanSmokeRenderer::swapchain_recreate_count),
            validation_warning_count: validation.warning_count,
            validation_error_count: validation.error_count,
            validation_vuids: &validation.vuids,
            requested_frames: self.options.frames,
            timeout_seconds: self.options.timeout_seconds,
            shader_manifest_hash: renderer
                .map_or("", |value| value.report().shader_manifest_hash.as_str()),
            vulkan_loader_status: if renderer.is_some() {
                "available"
            } else {
                "failed"
            },
            vulkan_instance_status: if renderer.is_some() {
                "created"
            } else {
                "failed"
            },
            window_status: if self.window.is_some() {
                "created"
            } else {
                "failed"
            },
            vulkan_surface_status: if renderer.is_some() {
                "created"
            } else {
                "failed"
            },
            vulkan_device_status: if renderer.is_some() {
                "selected"
            } else {
                "failed"
            },
            vulkan_device_name: renderer.map_or("", |value| value.report().device_name.as_str()),
            vulkan_logical_device_status: if renderer.is_some() {
                "created"
            } else {
                "failed"
            },
            vulkan_logical_device_graphics_queue_family: renderer
                .map_or(0, |value| value.report().graphics_queue_family),
            vulkan_logical_device_present_queue_family: renderer
                .map_or(0, |value| value.report().present_queue_family),
            vulkan_logical_device_enabled_extension_count: renderer
                .map_or(0, |value| value.report().enabled_extension_count),
            vulkan_swapchain_status: if renderer.is_some() {
                "created"
            } else {
                "failed"
            },
            vulkan_swapchain_width: renderer.map_or(0, |value| value.report().swapchain_extent.0),
            vulkan_swapchain_height: renderer.map_or(0, |value| value.report().swapchain_extent.1),
            vulkan_swapchain_image_count: renderer
                .map_or(0, |value| value.report().swapchain_image_count),
            vulkan_portability_enumeration: renderer
                .is_some_and(|value| value.report().portability_enumeration),
            vulkan_portability_subset_enabled: renderer
                .is_some_and(|value| value.report().portability_subset_enabled),
        };
        serde_json::to_string_pretty(&smoke_report)
            .map(|json| format!("{json}\n"))
            .map_err(|err| format!("native smoke report serialization failed: {err}"))
    }

    fn write_failure_report(&self, failure_reason: &str) -> Result<(), String> {
        let report = self.render_report("failed", Some(failure_reason))?;
        self.write_report(&report)
    }

    fn abort_if_timed_out(&mut self, event_loop: &ActiveEventLoop) -> bool {
        if self.output.is_some() || self.error.is_some() {
            return false;
        }
        if self.started_at.elapsed() <= Duration::from_secs(self.options.timeout_seconds) {
            return false;
        }
        self.error = Some(format!(
            "native smoke timed out after {} seconds",
            self.options.timeout_seconds
        ));
        event_loop.exit();
        true
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
        let _ = renderer;
        let report = match self.render_report("passed", None) {
            Ok(report) => report,
            Err(err) => {
                self.error = Some(err);
                event_loop.exit();
                return;
            }
        };
        if let Err(err) = self.write_report(&report) {
            self.error = Some(err);
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
        if self.abort_if_timed_out(event_loop) {
            return;
        }
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
        if self.abort_if_timed_out(event_loop) {
            return;
        }
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

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.abort_if_timed_out(event_loop) {
            return;
        }
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
    runner_architecture: &'static str,
    rust_toolchain: String,
    target_triple: String,
    platform: &'static str,
    status: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure_reason: Option<&'a str>,
    frames: u32,
    resize_count: u32,
    swapchain_recreate_count: u32,
    validation_warning_count: u32,
    validation_error_count: u32,
    validation_vuids: &'a [String],
    requested_frames: u32,
    timeout_seconds: u64,
    shader_manifest_hash: &'a str,
    vulkan_loader_status: &'a str,
    vulkan_instance_status: &'a str,
    window_status: &'a str,
    vulkan_surface_status: &'a str,
    vulkan_device_status: &'a str,
    vulkan_device_name: &'a str,
    vulkan_logical_device_status: &'a str,
    vulkan_logical_device_graphics_queue_family: u32,
    vulkan_logical_device_present_queue_family: u32,
    vulkan_logical_device_enabled_extension_count: u32,
    vulkan_swapchain_status: &'a str,
    vulkan_swapchain_width: u32,
    vulkan_swapchain_height: u32,
    vulkan_swapchain_image_count: u32,
    vulkan_portability_enumeration: bool,
    vulkan_portability_subset_enabled: bool,
}

fn actual_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "macos",
        "linux" => "linux",
        "windows" => "windows",
        other => other,
    }
}

fn actual_architecture() -> &'static str {
    match std::env::consts::ARCH {
        "arm64" => "aarch64",
        other => other,
    }
}

fn compiled_commit_sha() -> String {
    option_env!("FPARKAN_BUILD_COMMIT_SHA")
        .filter(|value| is_commit_sha(value))
        .map_or_else(runtime_git_commit_sha, ToString::to_string)
}

fn runtime_git_commit_sha() -> String {
    Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| is_commit_sha(value))
        .unwrap_or_else(|| "unknown".to_string())
}

fn compiled_git_dirty() -> bool {
    option_env!("FPARKAN_BUILD_GIT_DIRTY")
        .and_then(parse_bool_env)
        .unwrap_or_else(runtime_git_dirty)
}

fn runtime_git_dirty() -> bool {
    Command::new("git")
        .args(["status", "--short"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .is_some_and(|output| !output.trim().is_empty())
}

fn compiled_rust_toolchain() -> String {
    option_env!("FPARKAN_BUILD_RUST_TOOLCHAIN")
        .filter(|value| !value.trim().is_empty())
        .map_or_else(current_rustc_release, ToString::to_string)
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

fn compiled_target_triple() -> String {
    option_env!("FPARKAN_BUILD_TARGET_TRIPLE")
        .filter(|value| !value.trim().is_empty())
        .map_or_else(current_rustc_host_triple, ToString::to_string)
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

fn is_commit_sha(value: &str) -> bool {
    value.len() == 40 && value.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn parse_bool_env(value: &str) -> Option<bool> {
    match value {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
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
                timeout_seconds: DEFAULT_TIMEOUT_SECONDS,
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
    fn parses_timeout_seconds() {
        let parsed = SmokeOptions::parse(&[
            "--out".to_string(),
            "target/report.json".to_string(),
            "--timeout-seconds".to_string(),
            "45".to_string(),
        ]);

        assert_eq!(
            parsed,
            Ok(SmokeOptions {
                out: PathBuf::from("target/report.json"),
                frames: DEFAULT_TARGET_FRAMES,
                resize_frame: DEFAULT_RESIZE_FRAME,
                timeout_seconds: 45,
            })
        );
    }

    #[test]
    fn rejects_zero_timeout_seconds() {
        let parsed = SmokeOptions::parse(&[
            "--out".to_string(),
            "target/report.json".to_string(),
            "--timeout-seconds".to_string(),
            "0".to_string(),
        ]);

        assert_eq!(
            parsed,
            Err("native smoke requires --timeout-seconds >= 1".to_string())
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
    fn commit_sha_validation_accepts_hex_head() {
        assert!(is_commit_sha("0123456789abcdef0123456789abcdef01234567"));
    }

    #[test]
    fn commit_sha_validation_rejects_non_hex_or_wrong_length() {
        assert!(!is_commit_sha("0123456789abcdef0123456789abcdef0123456"));
        assert!(!is_commit_sha("zz23456789abcdef0123456789abcdef01234567"));
    }

    #[test]
    fn parses_bool_env_values() {
        assert_eq!(parse_bool_env("true"), Some(true));
        assert_eq!(parse_bool_env("false"), Some(false));
        assert_eq!(parse_bool_env("1"), None);
    }

    #[test]
    fn smoke_report_json_contains_expected_fields() {
        let json = serde_json::to_string_pretty(&SmokeReport {
            schema_version: SCHEMA_VERSION,
            commit_sha: "0123456789abcdef0123456789abcdef01234567".to_string(),
            git_dirty: false,
            runner_identity: "github-actions/12345/stage0-macos".to_string(),
            runner_architecture: "aarch64",
            rust_toolchain: "1.87.0".to_string(),
            target_triple: "aarch64-apple-darwin".to_string(),
            platform: "macos",
            status: "passed",
            failure_reason: None,
            frames: 300,
            resize_count: 1,
            swapchain_recreate_count: 1,
            validation_warning_count: 0,
            validation_error_count: 0,
            validation_vuids: &["VUID-A".to_string(), "VUID-B".to_string()],
            requested_frames: 300,
            timeout_seconds: DEFAULT_TIMEOUT_SECONDS,
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
            vulkan_portability_subset_enabled: true,
        })
        .expect("smoke report should serialize");

        assert!(json.contains("\"schema_version\": \"fparkan-native-smoke-v1\""));
        assert!(json.contains("\"validation_vuids\": ["));
        assert!(json.contains("\"vulkan_device_name\": \"Apple GPU\""));
        assert!(json.contains("\"runner_architecture\": \"aarch64\""));
    }

    #[test]
    fn finish_writes_failure_artifact() {
        let root = std::env::temp_dir().join(format!(
            "fparkan-native-smoke-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("temp dir");
        let out = root.join("report.json");

        let result = SmokeApp {
            options: SmokeOptions {
                out: out.clone(),
                frames: DEFAULT_TARGET_FRAMES,
                resize_frame: DEFAULT_RESIZE_FRAME,
                timeout_seconds: 7,
            },
            window_id: None,
            window: None,
            renderer: None,
            error: Some("native smoke timed out after 7 seconds".to_string()),
            output: None,
            frames_presented: 42,
            resize_count: 0,
            resize_requested: false,
            last_size: None,
            started_at: Instant::now(),
        }
        .finish();

        assert_eq!(
            result,
            Err("native smoke timed out after 7 seconds".to_string())
        );

        let json = std::fs::read_to_string(&out).expect("failure report");
        assert!(json.contains("\"status\": \"failed\""));
        assert!(json.contains("\"failure_reason\": \"native smoke timed out after 7 seconds\""));
        assert!(json.contains("\"timeout_seconds\": 7"));

        std::fs::remove_file(out).expect("cleanup report");
        std::fs::remove_dir(root).expect("cleanup dir");
    }
}
