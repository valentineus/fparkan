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

use fparkan_platform::{NativeWindowHandles, WindowPort};
use fparkan_platform_winit::{probe_smoke_window, WinitWindowPlan};
use fparkan_render_vulkan::{
    create_vulkan_instance_probe, create_vulkan_logical_device_probe, create_vulkan_surface_probe,
    create_vulkan_swapchain_probe, probe_vulkan_loader, triangle_shader_manifest,
    validate_shader_manifest, VulkanInstanceConfig, VulkanInstanceProbe, VulkanLogicalDeviceProbe,
    VulkanSwapchainProbe,
};
use std::path::PathBuf;
use std::process::Command;

const SCHEMA_VERSION: &str = "fparkan-native-smoke-v1";
const RUST_TOOLCHAIN: &str = "1.87.0";

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
    let bootstrap = VulkanBootstrapProbe::run(&options);
    validate_smoke_options(&options, &bootstrap)?;
    let report = render_smoke_report_json(&options, &bootstrap)?;
    if let Some(parent) = options.out.parent() {
        std::fs::create_dir_all(parent).map_err(|err| format!("{}: {err}", parent.display()))?;
    }
    std::fs::write(&options.out, &report)
        .map_err(|err| format!("{}: {err}", options.out.display()))?;
    Ok(report)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SmokeOptions {
    platform: SmokePlatform,
    out: PathBuf,
    status: SmokeStatus,
    frames: u32,
    resize_count: u32,
    swapchain_recreate_count: u32,
    validation_error_count: Option<u32>,
    probes: ProbeOptions,
    reason: Option<String>,
}

impl SmokeOptions {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut platform = None;
        let mut out = None;
        let mut status = SmokeStatus::Blocked;
        let mut frames = 0;
        let mut resize_count = 0;
        let mut swapchain_recreate_count = 0;
        let mut validation_error_count = None;
        let mut probes = ProbeOptions::default();
        let mut reason = None;
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--platform" => {
                    let value = iter
                        .next()
                        .ok_or_else(|| "--platform requires a value".to_string())?;
                    platform = Some(SmokePlatform::parse(value)?);
                }
                "--out" => {
                    let value = iter
                        .next()
                        .ok_or_else(|| "--out requires a path".to_string())?;
                    out = Some(PathBuf::from(value));
                }
                "--status" => {
                    let value = iter
                        .next()
                        .ok_or_else(|| "--status requires a value".to_string())?;
                    status = SmokeStatus::parse(value)?;
                }
                "--frames" => {
                    let value = iter
                        .next()
                        .ok_or_else(|| "--frames requires a value".to_string())?;
                    frames = parse_u32("--frames", value)?;
                }
                "--resize-count" => {
                    let value = iter
                        .next()
                        .ok_or_else(|| "--resize-count requires a value".to_string())?;
                    resize_count = parse_u32("--resize-count", value)?;
                }
                "--swapchain-recreate-count" => {
                    let value = iter
                        .next()
                        .ok_or_else(|| "--swapchain-recreate-count requires a value".to_string())?;
                    swapchain_recreate_count = parse_u32("--swapchain-recreate-count", value)?;
                }
                "--validation-error-count" => {
                    let value = iter
                        .next()
                        .ok_or_else(|| "--validation-error-count requires a value".to_string())?;
                    validation_error_count = Some(parse_u32("--validation-error-count", value)?);
                }
                "--probe-loader" => {
                    probes.vulkan = probes.vulkan.max(VulkanProbeDepth::Loader);
                }
                "--probe-instance" => {
                    probes.vulkan = probes.vulkan.max(VulkanProbeDepth::Instance);
                }
                "--probe-window" => {
                    probes.window = true;
                }
                "--probe-surface" => {
                    probes.vulkan = probes.vulkan.max(VulkanProbeDepth::Surface);
                    probes.window = true;
                }
                "--reason" => {
                    let value = iter
                        .next()
                        .ok_or_else(|| "--reason requires a value".to_string())?;
                    reason = Some(value.to_string());
                }
                _ => return Err(format!("unknown native smoke option: {arg}")),
            }
        }
        Ok(Self {
            platform: platform.ok_or_else(|| "missing --platform".to_string())?,
            out: out.ok_or_else(|| "missing --out".to_string())?,
            status,
            frames,
            resize_count,
            swapchain_recreate_count,
            validation_error_count,
            probes,
            reason,
        })
    }
}

fn parse_u32(name: &str, value: &str) -> Result<u32, String> {
    value
        .parse::<u32>()
        .map_err(|_| format!("invalid {name} value: {value}"))
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ProbeOptions {
    vulkan: VulkanProbeDepth,
    window: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
enum VulkanProbeDepth {
    #[default]
    None,
    Loader,
    Instance,
    Surface,
}

impl VulkanProbeDepth {
    const fn includes_loader(self) -> bool {
        matches!(self, Self::Loader | Self::Instance | Self::Surface)
    }

    const fn includes_instance(self) -> bool {
        matches!(self, Self::Instance | Self::Surface)
    }

    const fn includes_surface(self) -> bool {
        matches!(self, Self::Surface)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct VulkanBootstrapProbe {
    loader_status: VulkanLoaderStatus,
    instance_api: Option<String>,
    loader_error: Option<String>,
    instance_status: VulkanInstanceStatus,
    instance_error: Option<String>,
    portability_enumeration: bool,
    window_status: WinitWindowStatus,
    window_width: Option<u32>,
    window_height: Option<u32>,
    window_error: Option<String>,
    surface_status: VulkanSurfaceStatus,
    surface_error: Option<String>,
    device_status: VulkanDeviceStatus,
    device_name: Option<String>,
    device_error: Option<String>,
    logical_device_status: VulkanLogicalDeviceStatus,
    logical_device_graphics_queue_family: Option<u32>,
    logical_device_present_queue_family: Option<u32>,
    logical_device_enabled_extension_count: Option<u32>,
    logical_device_error: Option<String>,
    swapchain_status: VulkanSwapchainStatus,
    swapchain_width: Option<u32>,
    swapchain_height: Option<u32>,
    swapchain_image_count: Option<u32>,
    swapchain_error: Option<String>,
}

impl VulkanBootstrapProbe {
    fn run(options: &SmokeOptions) -> Self {
        if !options.probes.vulkan.includes_loader() {
            return Self::skipped();
        }

        let mut probe = Self::probe_loader();
        let window_handles = probe.probe_window(options);
        let instance = probe.probe_instance(options);
        probe.probe_surface(options, instance.as_ref(), window_handles);
        probe
    }

    const fn skipped() -> Self {
        Self {
            loader_status: VulkanLoaderStatus::Skipped,
            instance_api: None,
            loader_error: None,
            instance_status: VulkanInstanceStatus::Skipped,
            instance_error: None,
            portability_enumeration: false,
            window_status: WinitWindowStatus::Skipped,
            window_width: None,
            window_height: None,
            window_error: None,
            surface_status: VulkanSurfaceStatus::Skipped,
            surface_error: None,
            device_status: VulkanDeviceStatus::Skipped,
            device_name: None,
            device_error: None,
            logical_device_status: VulkanLogicalDeviceStatus::Skipped,
            logical_device_graphics_queue_family: None,
            logical_device_present_queue_family: None,
            logical_device_enabled_extension_count: None,
            logical_device_error: None,
            swapchain_status: VulkanSwapchainStatus::Skipped,
            swapchain_width: None,
            swapchain_height: None,
            swapchain_image_count: None,
            swapchain_error: None,
        }
    }

    fn probe_loader() -> Self {
        match probe_vulkan_loader() {
            Ok(report) => Self {
                loader_status: VulkanLoaderStatus::Available,
                instance_api: Some(format_api_version(report.instance_api_version)),
                loader_error: None,
                instance_status: VulkanInstanceStatus::Skipped,
                instance_error: None,
                portability_enumeration: false,
                window_status: WinitWindowStatus::Skipped,
                window_width: None,
                window_height: None,
                window_error: None,
                surface_status: VulkanSurfaceStatus::Skipped,
                surface_error: None,
                device_status: VulkanDeviceStatus::Skipped,
                device_name: None,
                device_error: None,
                logical_device_status: VulkanLogicalDeviceStatus::Skipped,
                logical_device_graphics_queue_family: None,
                logical_device_present_queue_family: None,
                logical_device_enabled_extension_count: None,
                logical_device_error: None,
                swapchain_status: VulkanSwapchainStatus::Skipped,
                swapchain_width: None,
                swapchain_height: None,
                swapchain_image_count: None,
                swapchain_error: None,
            },
            Err(err) => Self {
                loader_status: VulkanLoaderStatus::Unavailable,
                instance_api: None,
                loader_error: Some(err.to_string()),
                instance_status: VulkanInstanceStatus::Skipped,
                instance_error: None,
                portability_enumeration: false,
                window_status: WinitWindowStatus::Skipped,
                window_width: None,
                window_height: None,
                window_error: None,
                surface_status: VulkanSurfaceStatus::Skipped,
                surface_error: None,
                device_status: VulkanDeviceStatus::Skipped,
                device_name: None,
                device_error: None,
                logical_device_status: VulkanLogicalDeviceStatus::Skipped,
                logical_device_graphics_queue_family: None,
                logical_device_present_queue_family: None,
                logical_device_enabled_extension_count: None,
                logical_device_error: None,
                swapchain_status: VulkanSwapchainStatus::Skipped,
                swapchain_width: None,
                swapchain_height: None,
                swapchain_image_count: None,
                swapchain_error: None,
            },
        }
    }

    fn probe_window(&mut self, options: &SmokeOptions) -> Option<NativeWindowHandles> {
        if options.probes.vulkan.includes_surface() {
            match probe_smoke_window() {
                Ok(window) => {
                    self.window_status = WinitWindowStatus::Created;
                    self.window_width = Some(window.window.drawable_size().width);
                    self.window_height = Some(window.window.drawable_size().height);
                    window.native_handles()
                }
                Err(err) => {
                    self.window_status = WinitWindowStatus::Failed;
                    self.window_error = Some(err.to_string());
                    None
                }
            }
        } else if options.probes.window {
            match WinitWindowPlan::smoke().validate() {
                Ok(plan) => {
                    self.window_status = WinitWindowStatus::Planned;
                    self.window_width = Some(plan.width);
                    self.window_height = Some(plan.height);
                }
                Err(err) => {
                    self.window_status = WinitWindowStatus::Failed;
                    self.window_error = Some(err.to_string());
                }
            }
            None
        } else {
            None
        }
    }

    fn probe_instance(&mut self, options: &SmokeOptions) -> Option<VulkanInstanceProbe> {
        if options.probes.vulkan.includes_instance()
            && self.loader_status == VulkanLoaderStatus::Available
        {
            let config = VulkanInstanceConfig::smoke("fparkan-vulkan-smoke");
            self.portability_enumeration = config.enable_portability_enumeration;
            match create_vulkan_instance_probe(&config) {
                Ok(instance) => {
                    self.instance_status = VulkanInstanceStatus::Created;
                    self.portability_enumeration = instance.report.create_flags != 0;
                    return Some(instance);
                }
                Err(err) => {
                    self.instance_status = VulkanInstanceStatus::Failed;
                    self.instance_error = Some(err.to_string());
                }
            }
        }
        None
    }

    fn probe_surface(
        &mut self,
        options: &SmokeOptions,
        instance: Option<&VulkanInstanceProbe>,
        window_handles: Option<NativeWindowHandles>,
    ) {
        if options.probes.vulkan.includes_surface()
            && self.instance_status == VulkanInstanceStatus::Created
        {
            match instance
                .ok_or_else(|| "Vulkan instance probe was not retained".to_string())
                .and_then(|instance| {
                    create_vulkan_surface_probe(instance, window_handles)
                        .map_err(|err| err.to_string())
                }) {
                Ok(surface) => {
                    self.surface_status = VulkanSurfaceStatus::Created;
                    self.probe_runtime_capabilities(instance, &surface);
                }
                Err(err) => {
                    self.surface_status = VulkanSurfaceStatus::Failed;
                    self.surface_error = Some(err);
                }
            }
        }
    }

    fn probe_runtime_capabilities(
        &mut self,
        instance: Option<&VulkanInstanceProbe>,
        surface: &fparkan_render_vulkan::VulkanSurfaceProbe,
    ) {
        let Some(instance) = instance else {
            self.device_status = VulkanDeviceStatus::Failed;
            self.device_error = Some("Vulkan instance probe was not retained".to_string());
            self.logical_device_status = VulkanLogicalDeviceStatus::Skipped;
            self.swapchain_status = VulkanSwapchainStatus::Skipped;
            return;
        };
        match create_vulkan_logical_device_probe(
            instance,
            surface,
            (
                self.window_width.unwrap_or(1).max(1),
                self.window_height.unwrap_or(1).max(1),
            ),
        ) {
            Ok(device) => match create_vulkan_swapchain_probe(instance, surface, &device) {
                Ok(swapchain) => self.record_swapchain_probe(&device, &swapchain),
                Err(err) => {
                    self.record_logical_device_probe(&device);
                    self.swapchain_status = VulkanSwapchainStatus::Failed;
                    self.swapchain_error = Some(err.to_string());
                }
            },
            Err(err) => {
                self.device_status = VulkanDeviceStatus::Failed;
                self.device_error = Some(err.to_string());
                self.logical_device_status = VulkanLogicalDeviceStatus::Failed;
                self.logical_device_error = Some(err.to_string());
                self.swapchain_status = VulkanSwapchainStatus::Failed;
                self.swapchain_error = Some(err.to_string());
            }
        }
    }

    fn record_logical_device_probe(&mut self, device: &VulkanLogicalDeviceProbe) {
        self.device_status = VulkanDeviceStatus::Selected;
        self.device_name = Some(device.runtime.capability.device_name.clone());
        self.logical_device_status = VulkanLogicalDeviceStatus::Created;
        self.logical_device_graphics_queue_family = Some(device.report.graphics_queue_family);
        self.logical_device_present_queue_family = Some(device.report.present_queue_family);
        self.logical_device_enabled_extension_count = Some(
            device
                .report
                .enabled_extensions
                .len()
                .try_into()
                .unwrap_or(u32::MAX),
        );
        self.swapchain_width = Some(device.runtime.swapchain.extent.0);
        self.swapchain_height = Some(device.runtime.swapchain.extent.1);
        self.swapchain_image_count = Some(device.runtime.swapchain.image_count);
    }

    fn record_swapchain_probe(
        &mut self,
        device: &VulkanLogicalDeviceProbe,
        swapchain: &VulkanSwapchainProbe,
    ) {
        self.record_logical_device_probe(device);
        self.swapchain_status = VulkanSwapchainStatus::Created;
        self.swapchain_width = Some(swapchain.report.plan.extent.0);
        self.swapchain_height = Some(swapchain.report.plan.extent.1);
        self.swapchain_image_count = Some(swapchain.report.image_count);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VulkanLoaderStatus {
    Skipped,
    Available,
    Unavailable,
}

impl VulkanLoaderStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Skipped => "skipped",
            Self::Available => "available",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VulkanInstanceStatus {
    Skipped,
    Created,
    Failed,
}

impl VulkanInstanceStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Skipped => "skipped",
            Self::Created => "created",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WinitWindowStatus {
    Skipped,
    Planned,
    Created,
    Failed,
}

impl WinitWindowStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Skipped => "skipped",
            Self::Planned => "planned",
            Self::Created => "created",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VulkanSurfaceStatus {
    Skipped,
    Created,
    Failed,
}

impl VulkanSurfaceStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Skipped => "skipped",
            Self::Created => "created",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VulkanDeviceStatus {
    Skipped,
    Selected,
    Failed,
}

impl VulkanDeviceStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Skipped => "skipped",
            Self::Selected => "selected",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VulkanLogicalDeviceStatus {
    Skipped,
    Created,
    Failed,
}

impl VulkanLogicalDeviceStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Skipped => "skipped",
            Self::Created => "created",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VulkanSwapchainStatus {
    Skipped,
    Created,
    Failed,
}

impl VulkanSwapchainStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Skipped => "skipped",
            Self::Created => "created",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SmokePlatform {
    Windows,
    Linux,
    Macos,
}

impl SmokePlatform {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "windows" => Ok(Self::Windows),
            "linux" => Ok(Self::Linux),
            "macos" => Ok(Self::Macos),
            _ => Err(format!("unknown native smoke platform: {value}")),
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Linux => "linux",
            Self::Macos => "macos",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SmokeStatus {
    Blocked,
    Passed,
}

impl SmokeStatus {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "blocked" => Ok(Self::Blocked),
            "passed" => Ok(Self::Passed),
            _ => Err(format!("unknown native smoke status: {value}")),
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::Passed => "passed",
        }
    }
}

fn validate_smoke_options(
    options: &SmokeOptions,
    bootstrap: &VulkanBootstrapProbe,
) -> Result<(), String> {
    match options.status {
        SmokeStatus::Blocked => {
            if options
                .reason
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
            {
                return Err("blocked native smoke report requires --reason".to_string());
            }
        }
        SmokeStatus::Passed => {
            if options.frames < 300 {
                return Err("passed native smoke report requires --frames >= 300".to_string());
            }
            if options.resize_count == 0 {
                return Err("passed native smoke report requires --resize-count >= 1".to_string());
            }
            if options.swapchain_recreate_count == 0 {
                return Err(
                    "passed native smoke report requires --swapchain-recreate-count >= 1"
                        .to_string(),
                );
            }
            if options.validation_error_count != Some(0) {
                return Err(
                    "passed native smoke report requires --validation-error-count 0".to_string(),
                );
            }
            if bootstrap.loader_status != VulkanLoaderStatus::Available {
                return Err(
                    "passed native smoke report requires successful --probe-loader".to_string(),
                );
            }
            if bootstrap.instance_status != VulkanInstanceStatus::Created {
                return Err(
                    "passed native smoke report requires successful --probe-instance".to_string(),
                );
            }
            if bootstrap.window_status != WinitWindowStatus::Created {
                return Err(
                    "passed native smoke report requires successful --probe-window".to_string(),
                );
            }
            if bootstrap.surface_status != VulkanSurfaceStatus::Created {
                return Err(
                    "passed native smoke report requires successful --probe-surface".to_string(),
                );
            }
            if bootstrap.device_status != VulkanDeviceStatus::Selected {
                return Err(
                    "passed native smoke report requires selected Vulkan device".to_string()
                );
            }
            if bootstrap.logical_device_status != VulkanLogicalDeviceStatus::Created {
                return Err(
                    "passed native smoke report requires created Vulkan logical device".to_string(),
                );
            }
            if bootstrap.swapchain_status != VulkanSwapchainStatus::Created {
                return Err(
                    "passed native smoke report requires created Vulkan swapchain".to_string(),
                );
            }
        }
    }
    Ok(())
}

fn render_smoke_report_json(
    options: &SmokeOptions,
    bootstrap: &VulkanBootstrapProbe,
) -> Result<String, String> {
    let shader_manifest = validate_shader_manifest(&triangle_shader_manifest())
        .map_err(|err| format!("shader manifest: {err}"))?;
    let mut fields = base_smoke_report_fields(options, &shader_manifest.manifest_hash);
    fields.extend(vulkan_bootstrap_fields(bootstrap));
    fields.push(("reason", optional_string(options.reason.as_deref())));
    Ok(render_json_object(&fields))
}

fn base_smoke_report_fields(
    options: &SmokeOptions,
    shader_manifest_hash: &str,
) -> Vec<(&'static str, String)> {
    vec![
        ("schema_version", json_string(SCHEMA_VERSION)),
        ("commit_sha", json_string(&current_git_commit_sha())),
        ("rust_toolchain", json_string(RUST_TOOLCHAIN)),
        ("target_triple", json_string(&current_rustc_host_triple())),
        ("platform", json_string(options.platform.as_str())),
        ("status", json_string(options.status.as_str())),
        ("frames", options.frames.to_string()),
        ("resize_count", options.resize_count.to_string()),
        (
            "swapchain_recreate_count",
            options.swapchain_recreate_count.to_string(),
        ),
        (
            "validation_error_count",
            optional_u32(options.validation_error_count),
        ),
        ("shader_manifest_hash", json_string(shader_manifest_hash)),
    ]
}

fn vulkan_bootstrap_fields(bootstrap: &VulkanBootstrapProbe) -> Vec<(&'static str, String)> {
    vec![
        (
            "vulkan_loader_status",
            json_string(bootstrap.loader_status.as_str()),
        ),
        (
            "vulkan_instance_api",
            optional_string(bootstrap.instance_api.as_deref()),
        ),
        (
            "vulkan_loader_error",
            optional_string(bootstrap.loader_error.as_deref()),
        ),
        (
            "vulkan_instance_status",
            json_string(bootstrap.instance_status.as_str()),
        ),
        (
            "vulkan_instance_error",
            optional_string(bootstrap.instance_error.as_deref()),
        ),
        (
            "vulkan_portability_enumeration",
            bool_json(bootstrap.portability_enumeration),
        ),
        (
            "window_status",
            json_string(bootstrap.window_status.as_str()),
        ),
        ("window_width", optional_u32(bootstrap.window_width)),
        ("window_height", optional_u32(bootstrap.window_height)),
        (
            "window_error",
            optional_string(bootstrap.window_error.as_deref()),
        ),
        (
            "vulkan_surface_status",
            json_string(bootstrap.surface_status.as_str()),
        ),
        (
            "vulkan_surface_error",
            optional_string(bootstrap.surface_error.as_deref()),
        ),
        (
            "vulkan_device_status",
            json_string(bootstrap.device_status.as_str()),
        ),
        (
            "vulkan_device_name",
            optional_string(bootstrap.device_name.as_deref()),
        ),
        (
            "vulkan_device_error",
            optional_string(bootstrap.device_error.as_deref()),
        ),
        (
            "vulkan_logical_device_status",
            json_string(bootstrap.logical_device_status.as_str()),
        ),
        (
            "vulkan_logical_device_graphics_queue_family",
            optional_u32(bootstrap.logical_device_graphics_queue_family),
        ),
        (
            "vulkan_logical_device_present_queue_family",
            optional_u32(bootstrap.logical_device_present_queue_family),
        ),
        (
            "vulkan_logical_device_enabled_extension_count",
            optional_u32(bootstrap.logical_device_enabled_extension_count),
        ),
        (
            "vulkan_logical_device_error",
            optional_string(bootstrap.logical_device_error.as_deref()),
        ),
        (
            "vulkan_swapchain_status",
            json_string(bootstrap.swapchain_status.as_str()),
        ),
        (
            "vulkan_swapchain_width",
            optional_u32(bootstrap.swapchain_width),
        ),
        (
            "vulkan_swapchain_height",
            optional_u32(bootstrap.swapchain_height),
        ),
        (
            "vulkan_swapchain_image_count",
            optional_u32(bootstrap.swapchain_image_count),
        ),
        (
            "vulkan_swapchain_error",
            optional_string(bootstrap.swapchain_error.as_deref()),
        ),
    ]
}

fn render_json_object(fields: &[(&str, String)]) -> String {
    let mut out = String::from("{\n");
    for (index, (name, value)) in fields.iter().enumerate() {
        out.push_str("  ");
        out.push_str(&json_string(name));
        out.push_str(": ");
        out.push_str(value);
        if index + 1 < fields.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("}\n");
    out
}

fn optional_string(value: Option<&str>) -> String {
    value.map_or_else(|| "null".to_string(), json_string)
}

fn optional_u32(value: Option<u32>) -> String {
    value.map_or_else(|| "null".to_string(), |value| value.to_string())
}

fn bool_json(value: bool) -> String {
    if value { "true" } else { "false" }.to_string()
}

fn format_api_version(version: u32) -> String {
    let major = version >> 22;
    let minor = (version >> 12) & 0x03ff;
    let patch = version & 0x0fff;
    format!("{major}.{minor}.{patch}")
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

fn current_rustc_host_triple() -> String {
    Command::new("rustc")
        .arg("-vV")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|output| {
            output
                .lines()
                .find_map(|line| line.strip_prefix("host: ").map(ToString::to_string))
        })
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

fn json_string(value: &str) -> String {
    format!("\"{}\"", json_escape(value))
}

fn json_escape(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", ch as u32);
            }
            ch => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    fn probe_fixture() -> VulkanBootstrapProbe {
        VulkanBootstrapProbe {
            loader_status: VulkanLoaderStatus::Available,
            instance_api: Some("1.3.0".to_string()),
            loader_error: None,
            instance_status: VulkanInstanceStatus::Created,
            instance_error: None,
            portability_enumeration: false,
            window_status: WinitWindowStatus::Created,
            window_width: Some(1280),
            window_height: Some(720),
            window_error: None,
            surface_status: VulkanSurfaceStatus::Created,
            surface_error: None,
            device_status: VulkanDeviceStatus::Selected,
            device_name: Some("Stage 0 GPU".to_string()),
            device_error: None,
            logical_device_status: VulkanLogicalDeviceStatus::Created,
            logical_device_graphics_queue_family: Some(0),
            logical_device_present_queue_family: Some(0),
            logical_device_enabled_extension_count: Some(1),
            logical_device_error: None,
            swapchain_status: VulkanSwapchainStatus::Created,
            swapchain_width: Some(1280),
            swapchain_height: Some(720),
            swapchain_image_count: Some(3),
            swapchain_error: None,
        }
    }

    #[test]
    fn parses_blocked_smoke_args() -> Result<(), String> {
        let options = SmokeOptions::parse(&strings(&[
            "--platform",
            "linux",
            "--out",
            "target/native.json",
            "--status",
            "blocked",
            "--probe-loader",
            "--reason",
            "runner unavailable",
        ]))?;

        assert_eq!(options.platform, SmokePlatform::Linux);
        assert_eq!(options.status, SmokeStatus::Blocked);
        assert_eq!(options.probes.vulkan, VulkanProbeDepth::Loader);
        assert_eq!(options.reason.as_deref(), Some("runner unavailable"));
        validate_smoke_options(
            &options,
            &VulkanBootstrapProbe {
                loader_status: VulkanLoaderStatus::Unavailable,
                instance_api: None,
                loader_error: Some("Vulkan loader is unavailable".to_string()),
                instance_status: VulkanInstanceStatus::Skipped,
                instance_error: None,
                portability_enumeration: false,
                window_status: WinitWindowStatus::Skipped,
                window_width: None,
                window_height: None,
                window_error: None,
                surface_status: VulkanSurfaceStatus::Skipped,
                surface_error: None,
                ..probe_fixture()
            },
        )
    }

    #[test]
    fn rejects_false_pass_without_full_evidence() {
        let options = SmokeOptions::parse(&strings(&[
            "--platform",
            "linux",
            "--out",
            "target/native.json",
            "--status",
            "passed",
            "--frames",
            "299",
            "--resize-count",
            "1",
            "--swapchain-recreate-count",
            "1",
            "--validation-error-count",
            "0",
        ]))
        .expect("options");

        assert_eq!(
            validate_smoke_options(
                &options,
                &VulkanBootstrapProbe {
                    loader_status: VulkanLoaderStatus::Available,
                    instance_api: Some("1.3.0".to_string()),
                    loader_error: None,
                    instance_status: VulkanInstanceStatus::Created,
                    instance_error: None,
                    portability_enumeration: false,
                    window_status: WinitWindowStatus::Created,
                    window_width: Some(1280),
                    window_height: Some(720),
                    window_error: None,
                    surface_status: VulkanSurfaceStatus::Created,
                    surface_error: None,
                    ..probe_fixture()
                },
            ),
            Err("passed native smoke report requires --frames >= 300".to_string())
        );
    }

    #[test]
    fn rejects_passed_without_loader_probe() {
        let options = SmokeOptions::parse(&strings(&[
            "--platform",
            "linux",
            "--out",
            "target/native.json",
            "--status",
            "passed",
            "--frames",
            "300",
            "--resize-count",
            "1",
            "--swapchain-recreate-count",
            "1",
            "--validation-error-count",
            "0",
        ]))
        .expect("options");

        assert_eq!(
            validate_smoke_options(
                &options,
                &VulkanBootstrapProbe {
                    loader_status: VulkanLoaderStatus::Skipped,
                    instance_api: None,
                    loader_error: None,
                    instance_status: VulkanInstanceStatus::Skipped,
                    instance_error: None,
                    portability_enumeration: false,
                    window_status: WinitWindowStatus::Skipped,
                    window_width: None,
                    window_height: None,
                    window_error: None,
                    surface_status: VulkanSurfaceStatus::Skipped,
                    surface_error: None,
                    ..probe_fixture()
                },
            ),
            Err("passed native smoke report requires successful --probe-loader".to_string())
        );
    }

    #[test]
    fn rejects_passed_without_swapchain_recreation() {
        let options = SmokeOptions::parse(&strings(&[
            "--platform",
            "linux",
            "--out",
            "target/native.json",
            "--status",
            "passed",
            "--frames",
            "300",
            "--resize-count",
            "1",
            "--validation-error-count",
            "0",
            "--probe-surface",
        ]))
        .expect("options");

        assert_eq!(
            validate_smoke_options(
                &options,
                &VulkanBootstrapProbe {
                    loader_status: VulkanLoaderStatus::Available,
                    instance_api: Some("1.3.0".to_string()),
                    loader_error: None,
                    instance_status: VulkanInstanceStatus::Created,
                    instance_error: None,
                    portability_enumeration: false,
                    window_status: WinitWindowStatus::Created,
                    window_width: Some(1280),
                    window_height: Some(720),
                    window_error: None,
                    surface_status: VulkanSurfaceStatus::Created,
                    surface_error: None,
                    ..probe_fixture()
                },
            ),
            Err("passed native smoke report requires --swapchain-recreate-count >= 1".to_string())
        );
    }

    #[test]
    fn rejects_passed_without_instance_probe() {
        let options = SmokeOptions::parse(&strings(&[
            "--platform",
            "linux",
            "--out",
            "target/native.json",
            "--status",
            "passed",
            "--frames",
            "300",
            "--resize-count",
            "1",
            "--swapchain-recreate-count",
            "1",
            "--validation-error-count",
            "0",
            "--probe-loader",
        ]))
        .expect("options");

        assert_eq!(
            validate_smoke_options(
                &options,
                &VulkanBootstrapProbe {
                    loader_status: VulkanLoaderStatus::Available,
                    instance_api: Some("1.3.0".to_string()),
                    loader_error: None,
                    instance_status: VulkanInstanceStatus::Skipped,
                    instance_error: None,
                    portability_enumeration: false,
                    window_status: WinitWindowStatus::Skipped,
                    window_width: None,
                    window_height: None,
                    window_error: None,
                    surface_status: VulkanSurfaceStatus::Skipped,
                    surface_error: None,
                    ..probe_fixture()
                },
            ),
            Err("passed native smoke report requires successful --probe-instance".to_string())
        );
    }

    #[test]
    fn rejects_passed_without_window_probe() {
        let options = SmokeOptions::parse(&strings(&[
            "--platform",
            "linux",
            "--out",
            "target/native.json",
            "--status",
            "passed",
            "--frames",
            "300",
            "--resize-count",
            "1",
            "--swapchain-recreate-count",
            "1",
            "--validation-error-count",
            "0",
            "--probe-instance",
        ]))
        .expect("options");

        assert_eq!(
            validate_smoke_options(
                &options,
                &VulkanBootstrapProbe {
                    loader_status: VulkanLoaderStatus::Available,
                    instance_api: Some("1.3.0".to_string()),
                    loader_error: None,
                    instance_status: VulkanInstanceStatus::Created,
                    instance_error: None,
                    portability_enumeration: false,
                    window_status: WinitWindowStatus::Skipped,
                    window_width: None,
                    window_height: None,
                    window_error: None,
                    surface_status: VulkanSurfaceStatus::Created,
                    surface_error: None,
                    ..probe_fixture()
                },
            ),
            Err("passed native smoke report requires successful --probe-window".to_string())
        );
    }

    #[test]
    fn rejects_passed_without_surface_probe() {
        let options = SmokeOptions::parse(&strings(&[
            "--platform",
            "linux",
            "--out",
            "target/native.json",
            "--status",
            "passed",
            "--frames",
            "300",
            "--resize-count",
            "1",
            "--swapchain-recreate-count",
            "1",
            "--validation-error-count",
            "0",
            "--probe-window",
            "--probe-instance",
        ]))
        .expect("options");

        assert_eq!(
            validate_smoke_options(
                &options,
                &VulkanBootstrapProbe {
                    loader_status: VulkanLoaderStatus::Available,
                    instance_api: Some("1.3.0".to_string()),
                    loader_error: None,
                    instance_status: VulkanInstanceStatus::Created,
                    instance_error: None,
                    portability_enumeration: false,
                    window_status: WinitWindowStatus::Created,
                    window_width: Some(1280),
                    window_height: Some(720),
                    window_error: None,
                    surface_status: VulkanSurfaceStatus::Skipped,
                    surface_error: None,
                    ..probe_fixture()
                },
            ),
            Err("passed native smoke report requires successful --probe-surface".to_string())
        );
    }

    #[test]
    fn rejects_passed_with_failed_surface() {
        let options = SmokeOptions::parse(&strings(&[
            "--platform",
            "linux",
            "--out",
            "target/native.json",
            "--status",
            "passed",
            "--frames",
            "300",
            "--resize-count",
            "1",
            "--swapchain-recreate-count",
            "1",
            "--validation-error-count",
            "0",
            "--probe-surface",
        ]))
        .expect("options");

        assert_eq!(
            validate_smoke_options(
                &options,
                &VulkanBootstrapProbe {
                    loader_status: VulkanLoaderStatus::Available,
                    instance_api: Some("1.3.0".to_string()),
                    loader_error: None,
                    instance_status: VulkanInstanceStatus::Created,
                    instance_error: None,
                    portability_enumeration: false,
                    window_status: WinitWindowStatus::Created,
                    window_width: Some(1280),
                    window_height: Some(720),
                    window_error: None,
                    surface_status: VulkanSurfaceStatus::Failed,
                    surface_error: Some("Vulkan surface creation failed".to_string()),
                    ..probe_fixture()
                },
            ),
            Err("passed native smoke report requires successful --probe-surface".to_string())
        );
    }

    #[test]
    fn rejects_passed_without_selected_device() {
        let options = SmokeOptions::parse(&strings(&[
            "--platform",
            "linux",
            "--out",
            "target/native.json",
            "--status",
            "passed",
            "--frames",
            "300",
            "--resize-count",
            "1",
            "--swapchain-recreate-count",
            "1",
            "--validation-error-count",
            "0",
            "--probe-surface",
        ]))
        .expect("options");

        assert_eq!(
            validate_smoke_options(
                &options,
                &VulkanBootstrapProbe {
                    device_status: VulkanDeviceStatus::Failed,
                    device_name: None,
                    device_error: Some("no Vulkan physical device available".to_string()),
                    ..probe_fixture()
                },
            ),
            Err("passed native smoke report requires selected Vulkan device".to_string())
        );
    }

    #[test]
    fn rejects_passed_without_created_swapchain() {
        let options = SmokeOptions::parse(&strings(&[
            "--platform",
            "linux",
            "--out",
            "target/native.json",
            "--status",
            "passed",
            "--frames",
            "300",
            "--resize-count",
            "1",
            "--swapchain-recreate-count",
            "1",
            "--validation-error-count",
            "0",
            "--probe-surface",
        ]))
        .expect("options");

        assert_eq!(
            validate_smoke_options(
                &options,
                &VulkanBootstrapProbe {
                    swapchain_status: VulkanSwapchainStatus::Failed,
                    swapchain_error: Some("Vulkan swapchain creation failed".to_string()),
                    ..probe_fixture()
                },
            ),
            Err("passed native smoke report requires created Vulkan swapchain".to_string())
        );
    }

    #[test]
    fn rejects_passed_without_created_logical_device() {
        let options = SmokeOptions::parse(&strings(&[
            "--platform",
            "linux",
            "--out",
            "target/native.json",
            "--status",
            "passed",
            "--frames",
            "300",
            "--resize-count",
            "1",
            "--swapchain-recreate-count",
            "1",
            "--validation-error-count",
            "0",
            "--probe-surface",
        ]))
        .expect("options");

        assert_eq!(
            validate_smoke_options(
                &options,
                &VulkanBootstrapProbe {
                    logical_device_status: VulkanLogicalDeviceStatus::Failed,
                    logical_device_error: Some("Vulkan logical device creation failed".to_string()),
                    ..probe_fixture()
                },
            ),
            Err("passed native smoke report requires created Vulkan logical device".to_string())
        );
    }

    #[test]
    fn blocked_report_includes_shader_manifest_and_bootstrap_status() -> Result<(), String> {
        let options = SmokeOptions::parse(&strings(&[
            "--platform",
            "macos",
            "--out",
            "target/native.json",
            "--status",
            "blocked",
            "--reason",
            "runner unavailable",
        ]))?;

        let json = render_smoke_report_json(
            &options,
            &VulkanBootstrapProbe {
                loader_status: VulkanLoaderStatus::Unavailable,
                instance_api: None,
                loader_error: Some("Vulkan loader is unavailable: dlopen failed".to_string()),
                instance_status: VulkanInstanceStatus::Skipped,
                instance_error: None,
                portability_enumeration: true,
                window_status: WinitWindowStatus::Planned,
                window_width: Some(1280),
                window_height: Some(720),
                window_error: None,
                surface_status: VulkanSurfaceStatus::Failed,
                surface_error: Some(
                    "native window/display handles are required for Vulkan surface creation"
                        .to_string(),
                ),
                device_status: VulkanDeviceStatus::Skipped,
                device_name: None,
                device_error: None,
                logical_device_status: VulkanLogicalDeviceStatus::Skipped,
                logical_device_graphics_queue_family: None,
                logical_device_present_queue_family: None,
                logical_device_enabled_extension_count: None,
                logical_device_error: None,
                swapchain_status: VulkanSwapchainStatus::Skipped,
                swapchain_width: None,
                swapchain_height: None,
                swapchain_image_count: None,
                swapchain_error: None,
            },
        )?;

        assert!(json.contains("\"schema_version\": \"fparkan-native-smoke-v1\""));
        assert!(json.contains("\"target_triple\": \""));
        assert!(json.contains("\"platform\": \"macos\""));
        assert!(json.contains("\"status\": \"blocked\""));
        assert!(json.contains("\"swapchain_recreate_count\": 0"));
        assert!(json.contains("\"shader_manifest_hash\": \""));
        assert!(json.contains("\"vulkan_loader_status\": \"unavailable\""));
        assert!(json.contains("\"vulkan_instance_api\": null"));
        assert!(json
            .contains("\"vulkan_loader_error\": \"Vulkan loader is unavailable: dlopen failed\""));
        assert!(json.contains("\"vulkan_instance_status\": \"skipped\""));
        assert!(json.contains("\"vulkan_instance_error\": null"));
        assert!(json.contains("\"vulkan_portability_enumeration\": true"));
        assert!(json.contains("\"window_status\": \"planned\""));
        assert!(json.contains("\"window_width\": 1280"));
        assert!(json.contains("\"window_height\": 720"));
        assert!(json.contains("\"window_error\": null"));
        assert!(json.contains("\"vulkan_surface_status\": \"failed\""));
        assert!(json.contains(
            "\"vulkan_surface_error\": \"native window/display handles are required for Vulkan surface creation\""
        ));
        assert!(json.contains("\"vulkan_device_status\": \"skipped\""));
        assert!(json.contains("\"vulkan_device_name\": null"));
        assert!(json.contains("\"vulkan_logical_device_status\": \"skipped\""));
        assert!(json.contains("\"vulkan_logical_device_graphics_queue_family\": null"));
        assert!(json.contains("\"vulkan_swapchain_status\": \"skipped\""));
        assert!(json.contains("\"vulkan_swapchain_width\": null"));
        assert!(json.contains("\"reason\": \"runner unavailable\""));
        Ok(())
    }

    #[test]
    fn parses_instance_probe_as_loader_probe() -> Result<(), String> {
        let options = SmokeOptions::parse(&strings(&[
            "--platform",
            "linux",
            "--out",
            "target/native.json",
            "--probe-instance",
            "--reason",
            "runner unavailable",
        ]))?;

        assert_eq!(options.probes.vulkan, VulkanProbeDepth::Instance);
        assert!(!options.probes.window);
        Ok(())
    }

    #[test]
    fn parses_window_probe_without_vulkan_probes() -> Result<(), String> {
        let options = SmokeOptions::parse(&strings(&[
            "--platform",
            "linux",
            "--out",
            "target/native.json",
            "--probe-window",
            "--reason",
            "runner unavailable",
        ]))?;

        assert_eq!(options.probes.vulkan, VulkanProbeDepth::None);
        assert!(options.probes.window);
        Ok(())
    }

    #[test]
    fn parses_surface_probe_as_instance_probe() -> Result<(), String> {
        let options = SmokeOptions::parse(&strings(&[
            "--platform",
            "linux",
            "--out",
            "target/native.json",
            "--probe-surface",
            "--reason",
            "runner unavailable",
        ]))?;

        assert_eq!(options.probes.vulkan, VulkanProbeDepth::Surface);
        assert!(options.probes.window);
        Ok(())
    }

    #[test]
    fn formats_vulkan_api_version() {
        assert_eq!(format_api_version((1 << 22) | (3 << 12) | 280), "1.3.280");
    }

    #[test]
    fn reports_rustc_host_triple() {
        assert!(!current_rustc_host_triple().trim().is_empty());
    }
}
