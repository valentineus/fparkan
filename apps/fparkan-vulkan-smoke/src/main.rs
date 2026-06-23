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

use fparkan_render_vulkan::{triangle_shader_manifest, validate_shader_manifest};
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
    validate_smoke_options(&options)?;
    let report = render_smoke_report_json(&options)?;
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
    validation_error_count: Option<u32>,
    reason: Option<String>,
}

impl SmokeOptions {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut platform = None;
        let mut out = None;
        let mut status = SmokeStatus::Blocked;
        let mut frames = 0;
        let mut resize_count = 0;
        let mut validation_error_count = None;
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
                "--validation-error-count" => {
                    let value = iter
                        .next()
                        .ok_or_else(|| "--validation-error-count requires a value".to_string())?;
                    validation_error_count = Some(parse_u32("--validation-error-count", value)?);
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
            validation_error_count,
            reason,
        })
    }
}

fn parse_u32(name: &str, value: &str) -> Result<u32, String> {
    value
        .parse::<u32>()
        .map_err(|_| format!("invalid {name} value: {value}"))
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

fn validate_smoke_options(options: &SmokeOptions) -> Result<(), String> {
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
            if options.validation_error_count != Some(0) {
                return Err(
                    "passed native smoke report requires --validation-error-count 0".to_string(),
                );
            }
        }
    }
    Ok(())
}

fn render_smoke_report_json(options: &SmokeOptions) -> Result<String, String> {
    let shader_manifest = validate_shader_manifest(&triangle_shader_manifest())
        .map_err(|err| format!("shader manifest: {err}"))?;
    let validation_error_count = options
        .validation_error_count
        .map_or_else(|| "null".to_string(), |value| value.to_string());
    let reason = options
        .reason
        .as_ref()
        .map_or_else(|| "null".to_string(), |value| json_string(value));
    Ok(format!(
        concat!(
            "{{\n",
            "  \"schema_version\": \"{}\",\n",
            "  \"commit_sha\": \"{}\",\n",
            "  \"rust_toolchain\": \"{}\",\n",
            "  \"platform\": \"{}\",\n",
            "  \"status\": \"{}\",\n",
            "  \"frames\": {},\n",
            "  \"resize_count\": {},\n",
            "  \"validation_error_count\": {},\n",
            "  \"shader_manifest_hash\": \"{}\",\n",
            "  \"reason\": {}\n",
            "}}\n"
        ),
        SCHEMA_VERSION,
        json_escape(&current_git_commit_sha()),
        RUST_TOOLCHAIN,
        options.platform.as_str(),
        options.status.as_str(),
        options.frames,
        options.resize_count,
        validation_error_count,
        json_escape(&shader_manifest.manifest_hash),
        reason
    ))
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

    #[test]
    fn parses_blocked_smoke_args() -> Result<(), String> {
        let options = SmokeOptions::parse(&strings(&[
            "--platform",
            "linux",
            "--out",
            "target/native.json",
            "--status",
            "blocked",
            "--reason",
            "runner unavailable",
        ]))?;

        assert_eq!(options.platform, SmokePlatform::Linux);
        assert_eq!(options.status, SmokeStatus::Blocked);
        assert_eq!(options.reason.as_deref(), Some("runner unavailable"));
        validate_smoke_options(&options)
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
            "--validation-error-count",
            "0",
        ]))
        .expect("options");

        assert_eq!(
            validate_smoke_options(&options),
            Err("passed native smoke report requires --frames >= 300".to_string())
        );
    }

    #[test]
    fn blocked_report_includes_shader_manifest_hash() -> Result<(), String> {
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

        let json = render_smoke_report_json(&options)?;

        assert!(json.contains("\"schema_version\": \"fparkan-native-smoke-v1\""));
        assert!(json.contains("\"platform\": \"macos\""));
        assert!(json.contains("\"status\": \"blocked\""));
        assert!(json.contains("\"shader_manifest_hash\": \""));
        assert!(json.contains("\"reason\": \"runner unavailable\""));
        Ok(())
    }
}
