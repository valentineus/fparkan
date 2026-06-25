//! Build-time shader tool metadata for Vulkan reports.

use std::env;
use std::path::Path;
use std::process::Command;

const SHADER_COMPILER_NAME: &str = "glslangValidator";
const SPIRV_VALIDATOR_NAME: &str = "spirv-val";

fn main() {
    println!("cargo:rerun-if-env-changed=PATH");
    println!("cargo:rerun-if-env-changed=FPARKAN_GLSLANG_VALIDATOR");
    println!("cargo:rerun-if-env-changed=FPARKAN_SPIRV_VAL");

    emit_tool_metadata(
        "FPARKAN_BUILD_SHADER_COMPILER",
        &tool_path("FPARKAN_GLSLANG_VALIDATOR", SHADER_COMPILER_NAME),
    );
    emit_tool_metadata(
        "FPARKAN_BUILD_SPIRV_VALIDATOR",
        &tool_path("FPARKAN_SPIRV_VAL", SPIRV_VALIDATOR_NAME),
    );
}

fn tool_path(env_var: &str, fallback: &str) -> String {
    env::var(env_var).unwrap_or_else(|_| fallback.to_string())
}

fn emit_tool_metadata(prefix: &str, tool: &str) {
    let Some(path) = resolve_tool(tool) else {
        return;
    };
    println!("cargo:rerun-if-changed={}", path.display());
    let Some(version) = tool_version(&path) else {
        return;
    };
    let Some(binary_sha256) = tool_sha256(&path) else {
        return;
    };
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(tool)
        .to_string();
    println!("cargo:rustc-env={prefix}_NAME={name}");
    println!("cargo:rustc-env={prefix}_VERSION={version}");
    println!("cargo:rustc-env={prefix}_SHA256={binary_sha256}");
}

fn resolve_tool(tool: &str) -> Option<std::path::PathBuf> {
    let candidate = Path::new(tool);
    if candidate.components().count() > 1 {
        return candidate.is_file().then(|| candidate.to_path_buf());
    }
    let output = Command::new("which").arg(tool).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8(output.stdout).ok()?;
    let path = path.trim();
    (!path.is_empty()).then(|| path.into())
}

fn tool_version(path: &Path) -> Option<String> {
    let output = Command::new(path).arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    stdout
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(str::trim)
        .map(|line| {
            line.strip_prefix("Glslang Version: ")
                .unwrap_or(line)
                .to_string()
        })
}

fn tool_sha256(path: &Path) -> Option<String> {
    let output = Command::new("shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    stdout.split_whitespace().next().map(ToString::to_string)
}
