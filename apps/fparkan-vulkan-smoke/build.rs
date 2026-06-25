//! Build-time provenance for native smoke artifacts.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=TARGET");
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");
    println!("cargo:rerun-if-env-changed=SOURCE_VERSION");
    println!("cargo:rerun-if-env-changed=BUILD_VCS_NUMBER");

    if let Ok(target) = env::var("TARGET") {
        println!("cargo:rustc-env=FPARKAN_BUILD_TARGET_TRIPLE={target}");
    }

    let workspace_root =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest dir")).join("../..");
    if let Some(git_dir) = git_dir(&workspace_root) {
        emit_git_rerun_hints(&git_dir);
    }

    if let Some(commit_sha) = env_commit_sha().or_else(|| git_head_commit_sha(&workspace_root)) {
        println!("cargo:rustc-env=FPARKAN_BUILD_COMMIT_SHA={commit_sha}");
    }
}

fn env_commit_sha() -> Option<String> {
    ["GITHUB_SHA", "SOURCE_VERSION", "BUILD_VCS_NUMBER"]
        .into_iter()
        .filter_map(|name| env::var(name).ok())
        .find(|value| is_commit_sha(value))
}

fn git_head_commit_sha(workspace_root: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["-C"])
        .arg(workspace_root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    let value = value.trim().to_string();
    is_commit_sha(&value).then_some(value)
}

fn git_dir(workspace_root: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .args(["-C"])
        .arg(workspace_root)
        .args(["rev-parse", "--git-dir"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| workspace_root.join(value))
}

fn emit_git_rerun_hints(git_dir: &Path) {
    let head = git_dir.join("HEAD");
    println!("cargo:rerun-if-changed={}", head.display());
    println!(
        "cargo:rerun-if-changed={}",
        git_dir.join("packed-refs").display()
    );
    let Some(reference) = std::fs::read_to_string(&head).ok().and_then(|value| {
        value
            .strip_prefix("ref: ")
            .map(str::trim)
            .map(ToOwned::to_owned)
    }) else {
        return;
    };
    println!(
        "cargo:rerun-if-changed={}",
        git_dir.join(reference).display()
    );
}

fn is_commit_sha(value: &str) -> bool {
    value.len() == 40 && value.chars().all(|ch| ch.is_ascii_hexdigit())
}
