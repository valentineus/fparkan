#![forbid(unsafe_code)]
#![allow(clippy::print_stderr, clippy::print_stdout)]
//! Repository automation for `FParkan`.

use fparkan_corpus::{discover, render_report_json, report, DiscoverOptions};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let code = match run(&args) {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("{err}");
            2
        }
    };
    std::process::exit(code);
}

fn run(args: &[String]) -> Result<(), String> {
    match args {
        [cmd] if cmd == "ci" => {
            run_rustfmt_check(Path::new("."))?;
            run_policy(Path::new("."))?;
            cargo(&["test", "--workspace", "--locked", "--offline"])?;
            clippy_rustup(&["--workspace", "--locked", "--offline"])?;
            Ok(())
        }
        [cmd] if cmd == "policy" => run_policy(Path::new(".")),
        [cmd, subcmd, rest @ ..] if cmd == "acceptance" && subcmd == "report" => {
            let options = parse_acceptance_options(rest)?;
            run_acceptance_report(&options)
        }
        [cmd, subcmd, rest @ ..] if cmd == "acceptance" && subcmd == "audit" => {
            let options = parse_audit_options(rest)?;
            run_acceptance_audit(&options)
        }
        [cmd, rest @ ..] if cmd == "package" => {
            let options = parse_package_options(rest)?;
            run_package(&options)
        }
        [cmd, suite, rest @ ..] if cmd == "test" && suite == "synthetic" => {
            let options = parse_test_options(rest, PathBuf::from("testdata"))?;
            run_stage_tests(options.stage, TestSuite::Synthetic)
        }
        [cmd, suite, rest @ ..] if cmd == "test" && suite == "licensed" => {
            let options = parse_test_options(rest, PathBuf::from("testdata"))?;
            validate_licensed_root(&options.root)?;
            run_stage_tests(options.stage, TestSuite::Licensed)
        }
        [cmd, subcmd, rest @ ..] if cmd == "corpus" && subcmd == "baseline" => {
            let root = parse_root(rest)?;
            let manifest =
                discover(&root, DiscoverOptions::default()).map_err(|e| e.to_string())?;
            let report = report(&root, &manifest);
            println!("{}", render_report_json(&report));
            Ok(())
        }
        _ => Err(
            "usage: cargo xtask ci | policy | acceptance report --suite synthetic|licensed [--stage 0..5|all] [--root testdata] [--out <path>] | acceptance audit [--roadmap <path>] [--coverage <path>] [--out <path>] [--strict] | package --target <triple> --app viewer|game|headless|cli | test synthetic|licensed [--stage 0..5|all] [--root testdata] | corpus baseline --root <path>"
                .to_string(),
        ),
    }
}

fn cargo(args: &[&str]) -> Result<(), String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .args(args)
        .status()
        .map_err(|err| format!("failed to run cargo: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("cargo exited with {status}"))
    }
}

fn cargo_owned(args: &[String]) -> Result<(), String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .args(args)
        .status()
        .map_err(|err| format!("failed to run cargo: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("cargo exited with {status}"))
    }
}

fn clippy_rustup(args: &[&str]) -> Result<(), String> {
    let rustup = std::env::var_os("RUSTUP").unwrap_or_else(|| "rustup".into());
    let status = Command::new(rustup)
        .args(["run", "stable", "cargo-clippy"])
        .args(args)
        .status()
        .map_err(|err| format!("failed to run cargo-clippy through rustup: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("cargo-clippy exited with {status}"))
    }
}

fn run_rustfmt_check(root: &Path) -> Result<(), String> {
    let mut files = Vec::new();
    collect_rust_files(root, &mut files)?;
    if files.is_empty() {
        return Ok(());
    }

    let rustup = std::env::var_os("RUSTUP").unwrap_or_else(|| "rustup".into());
    let status = Command::new(rustup)
        .args(["run", "stable", "rustfmt", "--check"])
        .args(files)
        .status()
        .map_err(|err| format!("failed to run rustfmt: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("rustfmt exited with {status}"))
    }
}

fn collect_rust_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|err| format!("{}: {err}", dir.display()))?;
        let path = entry.path();
        if should_skip_policy_path(&path) {
            continue;
        }
        let file_type = entry
            .file_type()
            .map_err(|err| format!("{}: {err}", path.display()))?;
        if file_type.is_dir() {
            collect_rust_files(&path, out)?;
        } else if file_type.is_file()
            && path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext == "rs")
        {
            out.push(path);
        }
    }
    Ok(())
}

fn validate_licensed_root(root: &Path) -> Result<(), String> {
    for part in ["IS", "IS2"] {
        let part_root = root.join(part);
        if !part_root.is_dir() {
            return Err(format!(
                "licensed corpus part is missing: {}",
                part_root.display()
            ));
        }
    }
    Ok(())
}

fn parse_root(args: &[String]) -> Result<PathBuf, String> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--root" {
            return iter
                .next()
                .map(PathBuf::from)
                .ok_or_else(|| "--root requires a path".to_string());
        }
    }
    Err("missing --root".to_string())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PackageOptions {
    target: String,
    app: AppPackage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AppPackage {
    Cli,
    Game,
    Headless,
    Viewer,
}

impl AppPackage {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "cli" => Ok(Self::Cli),
            "game" => Ok(Self::Game),
            "headless" => Ok(Self::Headless),
            "viewer" => Ok(Self::Viewer),
            _ => Err(format!("unknown app: {value}")),
        }
    }

    fn package(self) -> &'static str {
        match self {
            Self::Cli => "fparkan-cli",
            Self::Game => "fparkan-game",
            Self::Headless => "fparkan-headless",
            Self::Viewer => "fparkan-viewer",
        }
    }
}

fn parse_package_options(args: &[String]) -> Result<PackageOptions, String> {
    let mut target = None;
    let mut app = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--target" => {
                target = Some(
                    iter.next()
                        .cloned()
                        .ok_or_else(|| "--target requires a value".to_string())?,
                );
            }
            "--app" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--app requires a value".to_string())?;
                app = Some(AppPackage::parse(value)?);
            }
            _ => return Err(format!("unknown package option: {arg}")),
        }
    }
    Ok(PackageOptions {
        target: target.ok_or_else(|| "missing --target".to_string())?,
        app: app.ok_or_else(|| "missing --app".to_string())?,
    })
}

fn run_package(options: &PackageOptions) -> Result<(), String> {
    cargo_owned(&[
        "build".to_string(),
        "-p".to_string(),
        options.app.package().to_string(),
        "--release".to_string(),
        "--locked".to_string(),
        "--offline".to_string(),
        "--target".to_string(),
        options.target.clone(),
    ])
}

fn run_policy(root: &Path) -> Result<(), String> {
    let mut failures = Vec::new();
    scan_policy_dir(root, &mut failures)?;
    validate_cargo_metadata(root, &mut failures)?;
    validate_lockfile(root, &mut failures);
    validate_workspace_license(root, &mut failures)?;
    validate_dependency_boundaries(root, &mut failures)?;
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!("workspace policy failed:\n{}", failures.join("\n")))
    }
}

fn validate_cargo_metadata(root: &Path, failures: &mut Vec<String>) -> Result<(), String> {
    let manifest = root.join("Cargo.toml");
    if !manifest.exists() {
        return Ok(());
    }
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--offline",
            "--locked",
            "--no-deps",
            "--manifest-path",
        ])
        .arg(&manifest)
        .output()
        .map_err(|err| format!("failed to run cargo metadata: {err}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        failures.push(format!(
            "{}: cargo metadata failed: {}",
            manifest.display(),
            stderr.trim()
        ));
    }
    Ok(())
}

fn validate_lockfile(root: &Path, failures: &mut Vec<String>) {
    let lockfile = root.join("Cargo.lock");
    if !lockfile.is_file() {
        failures.push(format!(
            "{}: workspace lockfile is required for locked/offline builds",
            lockfile.display()
        ));
    }
}

fn validate_workspace_license(root: &Path, failures: &mut Vec<String>) -> Result<(), String> {
    let manifest = root.join("Cargo.toml");
    let license = fs::read_to_string(root.join("LICENSE.txt"))
        .map_err(|err| format!("{}: {err}", root.join("LICENSE.txt").display()))?;
    let expected = if license.contains("GNU GENERAL PUBLIC LICENSE")
        && license.contains("Version 2, June 1991")
    {
        "GPL-2.0-only"
    } else {
        failures.push(format!(
            "{}: unsupported repository license text",
            root.join("LICENSE.txt").display()
        ));
        return Ok(());
    };

    let mut manifests = Vec::new();
    collect_cargo_manifests(root, &mut manifests)?;
    manifests.push(manifest);
    manifests.sort();
    manifests.dedup();

    for manifest in manifests {
        let text = fs::read_to_string(&manifest)
            .map_err(|err| format!("{}: {err}", manifest.display()))?;
        let explicit_license = parse_manifest_license(&text);
        let is_root = manifest == root.join("Cargo.toml");
        if is_root {
            if explicit_license.as_deref() != Some(expected) {
                failures.push(format!(
                    "{}: workspace.package license must be {expected}",
                    manifest.display()
                ));
            }
        } else if let Some(license) = explicit_license {
            if license != expected {
                failures.push(format!(
                    "{}: package license {license} does not match repository license {expected}",
                    manifest.display()
                ));
            }
        }
    }
    Ok(())
}

fn validate_dependency_boundaries(root: &Path, failures: &mut Vec<String>) -> Result<(), String> {
    let mut manifests = Vec::new();
    collect_cargo_manifests(root, &mut manifests)?;
    for manifest in manifests {
        let text = fs::read_to_string(&manifest)
            .map_err(|err| format!("{}: {err}", manifest.display()))?;
        let Some(package) = parse_package_name(&text) else {
            continue;
        };
        let dependencies = parse_manifest_dependencies(&text);
        if is_domain_manifest(root, &manifest) {
            for dependency in &dependencies {
                if is_forbidden_domain_dependency(dependency) {
                    failures.push(format!(
                        "{}: domain package {package} depends on forbidden GUI/adapter package {dependency}",
                        manifest.display()
                    ));
                }
            }
        }
        if package == "fparkan-headless" {
            for dependency in &dependencies {
                if matches!(
                    dependency.as_str(),
                    "fparkan-platform-sdl" | "fparkan-render-gl"
                ) {
                    failures.push(format!(
                        "{}: fparkan-headless depends on forbidden platform/render adapter {dependency}",
                        manifest.display()
                    ));
                }
            }
        }
    }
    Ok(())
}

fn collect_cargo_manifests(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|err| format!("{}: {err}", dir.display()))?;
        let path = entry.path();
        if should_skip_policy_path(&path) {
            continue;
        }
        let file_type = entry
            .file_type()
            .map_err(|err| format!("{}: {err}", path.display()))?;
        if file_type.is_dir() {
            collect_cargo_manifests(&path, out)?;
        } else if file_type.is_file()
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name == "Cargo.toml")
        {
            out.push(path);
        }
    }
    Ok(())
}

fn parse_manifest_license(manifest: &str) -> Option<String> {
    let mut in_package = false;
    let mut in_workspace_package = false;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_package = trimmed == "[package]";
            in_workspace_package = trimmed == "[workspace.package]";
            continue;
        }
        if (in_package || in_workspace_package) && trimmed.starts_with("license") {
            return parse_toml_string_value(trimmed);
        }
    }
    None
}

fn parse_package_name(manifest: &str) -> Option<String> {
    let mut in_package = false;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_package = trimmed == "[package]";
            continue;
        }
        if in_package && trimmed.starts_with("name") {
            return parse_toml_string_value(trimmed);
        }
    }
    None
}

fn parse_manifest_dependencies(manifest: &str) -> BTreeSet<String> {
    let mut dependencies = BTreeSet::new();
    let mut in_dependency_section = false;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_dependency_section = matches!(
                trimmed,
                "[dependencies]" | "[dev-dependencies]" | "[build-dependencies]"
            );
            continue;
        }
        if !in_dependency_section || trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((name, _)) = trimmed.split_once('=') else {
            continue;
        };
        let dependency = name.trim().trim_matches('"');
        if !dependency.is_empty() {
            dependencies.insert(dependency.to_string());
        }
    }
    dependencies
}

fn parse_toml_string_value(line: &str) -> Option<String> {
    let (_, value) = line.split_once('=')?;
    let value = value.trim();
    if !(value.starts_with('"') && value.ends_with('"')) {
        return None;
    }
    Some(value.trim_matches('"').to_string())
}

fn is_domain_manifest(root: &Path, manifest: &Path) -> bool {
    let relative = manifest.strip_prefix(root).unwrap_or(manifest);
    relative
        .components()
        .next()
        .is_some_and(|component| component.as_os_str() == "crates")
}

fn is_forbidden_domain_dependency(dependency: &str) -> bool {
    matches!(
        dependency,
        "fparkan-platform-sdl"
            | "fparkan-render-gl"
            | "fparkan-cli"
            | "fparkan-game"
            | "fparkan-headless"
            | "fparkan-viewer"
            | "sdl2"
            | "gl"
            | "glow"
            | "glium"
            | "glutin"
            | "winit"
    )
}

fn scan_policy_dir(dir: &Path, failures: &mut Vec<String>) -> Result<(), String> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => return Err(format!("{}: {err}", dir.display())),
    };
    for entry in entries {
        let entry = entry.map_err(|err| format!("{}: {err}", dir.display()))?;
        let path = entry.path();
        if should_skip_policy_path(&path) {
            continue;
        }
        let file_type = entry
            .file_type()
            .map_err(|err| format!("{}: {err}", path.display()))?;
        if file_type.is_dir() {
            if is_forbidden_generic_crate_dir(&path) {
                failures.push(format!(
                    "{}: package under crates/ must use the fparkan-* prefix",
                    path.display()
                ));
            }
            scan_policy_dir(&path, failures)?;
        } else if file_type.is_file() {
            scan_repository_file_policy(&path, failures)?;
            if is_policy_source(&path) {
                scan_policy_file(&path, failures)?;
            }
        }
    }
    Ok(())
}

fn should_skip_policy_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            matches!(
                name,
                ".git" | "target" | "testdata" | ".idea" | ".vscode" | ".DS_Store"
            )
        })
}

fn is_policy_source(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext, "rs" | "toml"))
}

fn is_forbidden_generic_crate_dir(path: &Path) -> bool {
    path.parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "crates")
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| !name.starts_with("fparkan-"))
}

fn scan_repository_file_policy(path: &Path, failures: &mut Vec<String>) -> Result<(), String> {
    if path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext == "py")
    {
        failures.push(format!(
            "{}: Python source file is forbidden",
            path.display()
        ));
    }

    let bytes = fs::read(path).map_err(|err| format!("{}: {err}", path.display()))?;
    if bytes.starts_with(b"#!") {
        let first_line = bytes
            .split(|byte| *byte == b'\n')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if first_line
            .windows("python".len())
            .any(|window| window == b"python")
        {
            failures.push(format!("{}: Python shebang is forbidden", path.display()));
        }
    }
    if is_workflow_file(path) {
        let text = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
        if text.contains("python") {
            failures.push(format!("{}: Python CI step is forbidden", path.display()));
        }
    }
    Ok(())
}

fn is_workflow_file(path: &Path) -> bool {
    let mut previous = None;
    for component in path.components() {
        let name = component.as_os_str().to_string_lossy();
        if previous.as_deref() == Some(".github") && name == "workflows" {
            return true;
        }
        previous = Some(name.into_owned());
    }
    false
}

fn scan_policy_file(path: &Path, failures: &mut Vec<String>) -> Result<(), String> {
    let text = fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))?;
    let lower = text.to_ascii_lowercase();
    if lower.contains(concat!("app.", "notion.com")) || lower.contains(concat!("385e", "79f2")) {
        failures.push(format!(
            "{}: external knowledge-base reference in source",
            path.display()
        ));
    }
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") || trimmed.starts_with("//!") || trimmed.starts_with("///") {
            continue;
        }
        if contains_unsafe_construct(trimmed) {
            failures.push(format!(
                "{}:{}: unsafe construct in workspace source",
                path.display(),
                index + 1
            ));
        }
    }
    Ok(())
}

fn contains_unsafe_construct(line: &str) -> bool {
    line.contains(concat!("un", "safe {"))
        || line.contains(concat!("un", "safe fn"))
        || line.contains(concat!("un", "safe impl"))
        || line.contains(concat!("extern ", "\"C\""))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Stage {
    All,
    Number(u8),
}

const ALL_WORKSPACE_PACKAGES: &[&str] = &[
    "fparkan-animation",
    "fparkan-assets",
    "fparkan-binary",
    "fparkan-corpus",
    "fparkan-diagnostics",
    "fparkan-fx",
    "fparkan-material",
    "fparkan-mission-format",
    "fparkan-msh",
    "fparkan-nres",
    "fparkan-path",
    "fparkan-platform",
    "fparkan-prototype",
    "fparkan-render",
    "fparkan-resource",
    "fparkan-rsli",
    "fparkan-runtime",
    "fparkan-terrain",
    "fparkan-terrain-format",
    "fparkan-test-support",
    "fparkan-texm",
    "fparkan-vfs",
    "fparkan-world",
    "fparkan-platform-sdl",
    "fparkan-render-gl",
    "fparkan-cli",
    "fparkan-game",
    "fparkan-headless",
    "fparkan-viewer",
    "xtask",
];

impl Stage {
    fn parse(value: &str) -> Result<Self, String> {
        if value == "all" {
            return Ok(Self::All);
        }
        let stage = value
            .parse::<u8>()
            .map_err(|_| format!("invalid stage: {value}"))?;
        if stage <= 5 {
            Ok(Self::Number(stage))
        } else {
            Err(format!("stage out of range: {stage}"))
        }
    }
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::All => f.write_str("all"),
            Self::Number(stage) => write!(f, "{stage}"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TestOptions {
    stage: Stage,
    root: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TestSuite {
    Licensed,
    Synthetic,
}

impl TestSuite {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "licensed" => Ok(Self::Licensed),
            "synthetic" => Ok(Self::Synthetic),
            _ => Err(format!("unknown suite: {value}")),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Licensed => "licensed",
            Self::Synthetic => "synthetic",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AcceptanceOptions {
    suite: TestSuite,
    stage: Stage,
    root: PathBuf,
    out: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AuditOptions {
    roadmap: PathBuf,
    coverage: PathBuf,
    out: PathBuf,
    strict: bool,
}

fn parse_test_options(args: &[String], default_root: PathBuf) -> Result<TestOptions, String> {
    let mut options = TestOptions {
        stage: Stage::All,
        root: default_root,
    };
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--stage" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--stage requires a value".to_string())?;
                options.stage = Stage::parse(value)?;
            }
            "--root" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--root requires a path".to_string())?;
                options.root = PathBuf::from(value);
            }
            _ => return Err(format!("unknown test option: {arg}")),
        }
    }
    Ok(options)
}

fn parse_acceptance_options(args: &[String]) -> Result<AcceptanceOptions, String> {
    let mut suite = None;
    let mut stage = Stage::All;
    let mut root = PathBuf::from("testdata");
    let mut out = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--suite" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--suite requires a value".to_string())?;
                suite = Some(TestSuite::parse(value)?);
            }
            "--stage" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--stage requires a value".to_string())?;
                stage = Stage::parse(value)?;
            }
            "--root" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--root requires a path".to_string())?;
                root = PathBuf::from(value);
            }
            "--out" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--out requires a path".to_string())?;
                out = Some(PathBuf::from(value));
            }
            _ => return Err(format!("unknown acceptance option: {arg}")),
        }
    }

    let suite = suite.ok_or_else(|| "missing --suite".to_string())?;
    let out = out.unwrap_or_else(|| {
        PathBuf::from("target")
            .join("fparkan")
            .join("reports")
            .join("acceptance")
            .join(format!("{}-stage-{}.json", suite.as_str(), stage))
    });
    Ok(AcceptanceOptions {
        suite,
        stage,
        root,
        out,
    })
}

fn parse_audit_options(args: &[String]) -> Result<AuditOptions, String> {
    let mut roadmap = PathBuf::from("FPARKAN_ARCHITECTURE_ROADMAP_STAGES_0_5.md");
    let mut coverage = PathBuf::from("fixtures/acceptance/coverage.tsv");
    let mut out = PathBuf::from("target")
        .join("fparkan")
        .join("reports")
        .join("acceptance")
        .join("coverage-audit.json");
    let mut strict = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--roadmap" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--roadmap requires a path".to_string())?;
                roadmap = PathBuf::from(value);
            }
            "--coverage" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--coverage requires a path".to_string())?;
                coverage = PathBuf::from(value);
            }
            "--out" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--out requires a path".to_string())?;
                out = PathBuf::from(value);
            }
            "--strict" => strict = true,
            _ => return Err(format!("unknown audit option: {arg}")),
        }
    }
    Ok(AuditOptions {
        roadmap,
        coverage,
        out,
        strict,
    })
}

fn run_acceptance_audit(options: &AuditOptions) -> Result<(), String> {
    let roadmap_text = fs::read_to_string(&options.roadmap)
        .map_err(|err| format!("{}: {err}", options.roadmap.display()))?;
    let required = extract_acceptance_ids(&roadmap_text);
    let coverage = read_coverage_manifest(&options.coverage)?;
    let audit = build_acceptance_audit(&required, &coverage);
    if let Some(parent) = options.out.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("{}: {err}", parent.display()))?;
    }
    fs::write(&options.out, render_audit_json(&audit))
        .map_err(|err| format!("{}: {err}", options.out.display()))?;
    println!("{}", options.out.display());
    let unverified = audit.unverified();
    if options.strict && (!unverified.is_empty() || !audit.unknown_coverage.is_empty()) {
        Err(format!(
            "acceptance coverage incomplete: {} unverified, {} unknown",
            unverified.len(),
            audit.unknown_coverage.len()
        ))
    } else {
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CoverageEntry {
    status: CoverageStatus,
    evidence: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CoverageStatus {
    Covered,
    Partial,
    Blocked,
    Omitted,
}

impl CoverageStatus {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "covered" => Ok(Self::Covered),
            "partial" => Ok(Self::Partial),
            "blocked" => Ok(Self::Blocked),
            "omitted" => Ok(Self::Omitted),
            _ => Err(format!("unknown coverage status: {value}")),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AcceptanceAudit {
    required_total: usize,
    covered: Vec<String>,
    partial: Vec<String>,
    blocked: Vec<String>,
    omitted: Vec<String>,
    missing: Vec<String>,
    unknown_coverage: Vec<String>,
    coverage_evidence: BTreeMap<String, String>,
    by_stage: BTreeMap<String, usize>,
}

impl AcceptanceAudit {
    fn unverified(&self) -> Vec<String> {
        self.partial
            .iter()
            .chain(&self.blocked)
            .chain(&self.missing)
            .cloned()
            .collect()
    }
}

fn extract_acceptance_ids(text: &str) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for segment in text.split('`') {
        if is_acceptance_id(segment) {
            ids.insert(segment.to_string());
        }
    }
    ids
}

fn is_acceptance_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 9
        && matches!(bytes[0], b'S' | b'L')
        && matches!(bytes[1], b'0'..=b'5')
        && bytes[2] == b'-'
        && bytes.iter().all(|byte| {
            byte.is_ascii_uppercase() || byte.is_ascii_digit() || *byte == b'-' || *byte == b'_'
        })
}

fn read_coverage_manifest(path: &Path) -> Result<BTreeMap<String, CoverageEntry>, String> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let text = fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))?;
    let mut entries = BTreeMap::new();
    for (line_number, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut fields = trimmed.splitn(3, '\t');
        let id = fields
            .next()
            .ok_or_else(|| format!("{}:{}: missing id", path.display(), line_number + 1))?;
        let status = fields
            .next()
            .ok_or_else(|| format!("{}:{}: missing status", path.display(), line_number + 1))?;
        let evidence = fields
            .next()
            .ok_or_else(|| format!("{}:{}: missing evidence", path.display(), line_number + 1))?;
        if evidence.trim().is_empty() {
            return Err(format!(
                "{}:{}: empty evidence",
                path.display(),
                line_number + 1
            ));
        }
        if !is_acceptance_id(id) {
            return Err(format!(
                "{}:{}: invalid acceptance id: {id}",
                path.display(),
                line_number + 1
            ));
        }
        entries.insert(
            id.to_string(),
            CoverageEntry {
                status: CoverageStatus::parse(status)?,
                evidence: evidence.to_string(),
            },
        );
    }
    Ok(entries)
}

fn build_acceptance_audit(
    required: &BTreeSet<String>,
    coverage: &BTreeMap<String, CoverageEntry>,
) -> AcceptanceAudit {
    let mut covered = Vec::new();
    let mut partial = Vec::new();
    let mut blocked = Vec::new();
    let mut omitted = Vec::new();
    let mut missing = Vec::new();
    let mut by_stage = BTreeMap::new();
    let mut coverage_evidence = BTreeMap::new();

    for id in required {
        let stage = id
            .get(0..2)
            .map_or_else(|| "??".to_string(), ToString::to_string);
        *by_stage.entry(stage).or_insert(0) += 1;
        match coverage.get(id).map(|entry| entry.status) {
            Some(CoverageStatus::Covered) => covered.push(id.clone()),
            Some(CoverageStatus::Partial) => partial.push(id.clone()),
            Some(CoverageStatus::Blocked) => blocked.push(id.clone()),
            Some(CoverageStatus::Omitted) => omitted.push(id.clone()),
            None => missing.push(id.clone()),
        }
        if let Some(entry) = coverage.get(id) {
            coverage_evidence.insert(id.clone(), entry.evidence.clone());
        }
    }

    let unknown_coverage = coverage
        .keys()
        .filter(|id| !required.contains(*id))
        .cloned()
        .collect();

    AcceptanceAudit {
        required_total: required.len(),
        covered,
        partial,
        blocked,
        omitted,
        missing,
        unknown_coverage,
        coverage_evidence,
        by_stage,
    }
}

fn render_audit_json(audit: &AcceptanceAudit) -> String {
    let unverified = audit.unverified();
    format!(
        concat!(
            "{{\n",
            "  \"schema_version\": \"fparkan-acceptance-coverage-v1\",\n",
            "  \"required_total\": {},\n",
            "  \"covered_total\": {},\n",
            "  \"partial_total\": {},\n",
            "  \"blocked_total\": {},\n",
            "  \"omitted_total\": {},\n",
            "  \"missing_total\": {},\n",
            "  \"unverified_total\": {},\n",
            "  \"unknown_coverage_total\": {},\n",
            "  \"by_stage\": {},\n",
            "  \"covered\": {},\n",
            "  \"partial\": {},\n",
            "  \"blocked\": {},\n",
            "  \"omitted\": {},\n",
            "  \"missing\": {},\n",
            "  \"unknown_coverage\": {},\n",
            "  \"coverage_evidence\": {}\n",
            "}}\n"
        ),
        audit.required_total,
        audit.covered.len(),
        audit.partial.len(),
        audit.blocked.len(),
        audit.omitted.len(),
        audit.missing.len(),
        unverified.len(),
        audit.unknown_coverage.len(),
        render_string_usize_map(&audit.by_stage),
        render_string_array(&audit.covered),
        render_string_array(&audit.partial),
        render_string_array(&audit.blocked),
        render_string_array(&audit.omitted),
        render_string_array(&audit.missing),
        render_string_array(&audit.unknown_coverage),
        render_string_string_map(&audit.coverage_evidence)
    )
}

fn render_string_usize_map(values: &BTreeMap<String, usize>) -> String {
    let pairs = values
        .iter()
        .map(|(key, value)| format!("\"{}\": {}", json_escape(key), value))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{{{pairs}}}")
}

fn render_string_string_map(values: &BTreeMap<String, String>) -> String {
    let pairs = values
        .iter()
        .map(|(key, value)| format!("\"{}\": \"{}\"", json_escape(key), json_escape(value)))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{{{pairs}}}")
}

fn render_string_array(values: &[String]) -> String {
    let items = values
        .iter()
        .map(|value| format!("\"{}\"", json_escape(value)))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{items}]")
}

fn json_escape(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => {
                let _ = write!(out, "\\u{:04x}", ch as u32);
            }
            ch => out.push(ch),
        }
    }
    out
}

fn run_acceptance_report(options: &AcceptanceOptions) -> Result<(), String> {
    if options.suite == TestSuite::Licensed {
        validate_licensed_root(&options.root)?;
    }
    run_stage_tests(options.stage, options.suite)?;

    if let Some(parent) = options.out.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("{}: {err}", parent.display()))?;
    }
    let report = render_acceptance_report(options);
    fs::write(&options.out, report).map_err(|err| format!("{}: {err}", options.out.display()))?;
    println!("{}", options.out.display());
    Ok(())
}

fn render_acceptance_report(options: &AcceptanceOptions) -> String {
    let packages = stage_report_packages(options.stage)
        .into_iter()
        .map(|package| format!("    \"{package}\""))
        .collect::<Vec<_>>()
        .join(",\n");
    let corpus = if options.suite == TestSuite::Licensed {
        "\n  \"licensed_corpus\": {\n    \"root\": \"redacted\",\n    \"parts\": [\"IS\", \"IS2\"]\n  },"
    } else {
        ""
    };
    format!(
        concat!(
            "{{\n",
            "  \"schema_version\": \"fparkan-acceptance-report-v1\",\n",
            "  \"suite\": \"{}\",\n",
            "  \"stage\": \"{}\",\n",
            "  \"status\": \"passed\",",
            "{}\n",
            "  \"packages\": [\n",
            "{}\n",
            "  ]\n",
            "}}\n"
        ),
        options.suite.as_str(),
        options.stage,
        corpus,
        packages
    )
}

fn stage_report_packages(stage: Stage) -> Vec<&'static str> {
    match stage {
        Stage::All => ALL_WORKSPACE_PACKAGES.to_vec(),
        Stage::Number(number) => stage_packages(number).unwrap_or(&[]).to_vec(),
    }
}

fn run_stage_tests(stage: Stage, suite: TestSuite) -> Result<(), String> {
    let mut suffix = Vec::new();
    if suite == TestSuite::Licensed {
        suffix.extend(["--", "--ignored"]);
    }
    match stage {
        Stage::All => {
            let mut args = vec!["test", "--workspace", "--locked", "--offline"];
            args.extend(suffix);
            cargo(&args)
        }
        Stage::Number(number) => {
            for package in stage_packages(number)? {
                let mut args = vec!["test", "-p", package, "--locked", "--offline"];
                args.extend(suffix.iter().copied());
                cargo(&args)?;
            }
            Ok(())
        }
    }
}

fn stage_packages(stage: u8) -> Result<&'static [&'static str], String> {
    match stage {
        0 => Ok(&[
            "fparkan-corpus",
            "fparkan-diagnostics",
            "fparkan-test-support",
        ]),
        1 => Ok(&[
            "fparkan-binary",
            "fparkan-path",
            "fparkan-nres",
            "fparkan-rsli",
            "fparkan-resource",
            "fparkan-vfs",
        ]),
        2 => Ok(&["fparkan-prototype"]),
        3 => Ok(&[
            "fparkan-msh",
            "fparkan-material",
            "fparkan-texm",
            "fparkan-assets",
            "fparkan-render",
            "fparkan-viewer",
        ]),
        4 => Ok(&["fparkan-animation", "fparkan-fx"]),
        5 => Ok(&[
            "fparkan-terrain-format",
            "fparkan-terrain",
            "fparkan-mission-format",
            "fparkan-world",
            "fparkan-runtime",
            "fparkan-headless",
            "fparkan-game",
        ]),
        _ => Err(format!("stage out of range: {stage}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn parses_stage_and_root_options() {
        let args = strings(&["--stage", "3", "--root", "fixtures"]);
        let parsed = parse_test_options(&args, PathBuf::from("testdata"));

        assert_eq!(
            parsed,
            Ok(TestOptions {
                stage: Stage::Number(3),
                root: PathBuf::from("fixtures"),
            })
        );
    }

    #[test]
    fn parses_acceptance_report_options() {
        let parsed = parse_acceptance_options(&strings(&[
            "--suite",
            "licensed",
            "--stage",
            "5",
            "--root",
            "testdata",
            "--out",
            "target/report.json",
        ]));

        assert_eq!(
            parsed,
            Ok(AcceptanceOptions {
                suite: TestSuite::Licensed,
                stage: Stage::Number(5),
                root: PathBuf::from("testdata"),
                out: PathBuf::from("target/report.json"),
            })
        );
    }

    #[test]
    fn acceptance_report_redacts_licensed_root() {
        let options = AcceptanceOptions {
            suite: TestSuite::Licensed,
            stage: Stage::Number(0),
            root: PathBuf::from("/private/game"),
            out: PathBuf::from("target/report.json"),
        };
        let report = render_acceptance_report(&options);

        assert!(report.contains("\"root\": \"redacted\""));
        assert!(!report.contains("/private/game"));
        assert!(report.contains("\"fparkan-corpus\""));
    }

    #[test]
    fn extracts_acceptance_ids_from_backticks_only() {
        let ids =
            extract_acceptance_ids("`S0-ARCH-001` text S0-ARCH-002 `L5-P1-MISSION-001` `bad`");

        assert!(ids.contains("S0-ARCH-001"));
        assert!(ids.contains("L5-P1-MISSION-001"));
        assert!(!ids.contains("S0-ARCH-002"));
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn builds_acceptance_audit_counts() {
        let required = ["S0-ARCH-001", "S0-ARCH-002", "L3-DEVICE-001", "L5-RG40-001"]
            .into_iter()
            .map(str::to_string)
            .collect::<BTreeSet<_>>();
        let coverage = [
            (
                "S0-ARCH-001".to_string(),
                CoverageEntry {
                    status: CoverageStatus::Covered,
                    evidence: "cargo xtask policy".to_string(),
                },
            ),
            (
                "L3-DEVICE-001".to_string(),
                CoverageEntry {
                    status: CoverageStatus::Omitted,
                    evidence: "outside macos scope".to_string(),
                },
            ),
            (
                "L5-RG40-001".to_string(),
                CoverageEntry {
                    status: CoverageStatus::Blocked,
                    evidence: "device not attached".to_string(),
                },
            ),
            (
                "S9-UNKNOWN-001".to_string(),
                CoverageEntry {
                    status: CoverageStatus::Partial,
                    evidence: "bad id".to_string(),
                },
            ),
        ]
        .into_iter()
        .collect::<BTreeMap<_, _>>();

        let audit = build_acceptance_audit(&required, &coverage);

        assert_eq!(audit.covered, ["S0-ARCH-001"]);
        assert_eq!(audit.blocked, ["L5-RG40-001"]);
        assert_eq!(audit.omitted, ["L3-DEVICE-001"]);
        assert_eq!(audit.missing, ["S0-ARCH-002"]);
        assert_eq!(audit.unknown_coverage, ["S9-UNKNOWN-001"]);
        assert_eq!(audit.by_stage.get("S0"), Some(&2));
    }

    #[test]
    fn audit_json_escapes_evidence() {
        let mut audit = AcceptanceAudit {
            required_total: 1,
            covered: vec!["S0-ARCH-001".to_string()],
            partial: Vec::new(),
            blocked: Vec::new(),
            omitted: Vec::new(),
            missing: Vec::new(),
            unknown_coverage: Vec::new(),
            coverage_evidence: BTreeMap::new(),
            by_stage: BTreeMap::new(),
        };
        audit
            .coverage_evidence
            .insert("S0-ARCH-001".to_string(), "quoted \"value\"".to_string());

        let json = render_audit_json(&audit);

        assert!(json.contains("quoted \\\"value\\\""));
    }

    #[test]
    fn defaults_to_all_stage_and_testdata_root() {
        let args = Vec::new();
        let parsed = parse_test_options(&args, PathBuf::from("testdata"));

        assert_eq!(
            parsed,
            Ok(TestOptions {
                stage: Stage::All,
                root: PathBuf::from("testdata"),
            })
        );
    }

    #[test]
    fn rejects_unknown_stage() {
        assert_eq!(Stage::parse("6"), Err("stage out of range: 6".to_string()));
        assert_eq!(
            Stage::parse("assets"),
            Err("invalid stage: assets".to_string())
        );
    }

    #[test]
    fn maps_stage_packages() {
        assert!(stage_packages(3).is_ok_and(|packages| packages.contains(&"fparkan-assets")));
        assert!(stage_packages(3).is_ok_and(|packages| packages.contains(&"fparkan-viewer")));
        assert!(stage_packages(5).is_ok_and(|packages| packages.contains(&"fparkan-runtime")));
        assert!(stage_packages(5).is_ok_and(|packages| packages.contains(&"fparkan-game")));
        assert_eq!(stage_packages(9), Err("stage out of range: 9".to_string()));
    }

    #[test]
    fn parses_manifest_dependencies_for_arch_policy() {
        let manifest = r#"
[package]
name = "fparkan-example"

[dependencies]
fparkan-render = { path = "../fparkan-render" }
"quoted-dep" = "1"

[dev-dependencies]
fparkan-render-gl = { path = "../../adapters/fparkan-render-gl" }
"#;

        assert_eq!(
            parse_package_name(manifest),
            Some("fparkan-example".to_string())
        );
        let deps = parse_manifest_dependencies(manifest);
        assert!(deps.contains("fparkan-render"));
        assert!(deps.contains("quoted-dep"));
        assert!(deps.contains("fparkan-render-gl"));
    }

    #[test]
    fn detects_forbidden_domain_dependencies() {
        assert!(is_forbidden_domain_dependency("fparkan-render-gl"));
        assert!(is_forbidden_domain_dependency("sdl2"));
        assert!(!is_forbidden_domain_dependency("fparkan-render"));
        assert!(!is_forbidden_domain_dependency("fparkan-platform"));
    }

    #[test]
    fn parses_package_options() {
        assert_eq!(
            parse_package_options(&strings(&[
                "--target",
                "aarch64-apple-darwin",
                "--app",
                "viewer"
            ])),
            Ok(PackageOptions {
                target: "aarch64-apple-darwin".to_string(),
                app: AppPackage::Viewer,
            })
        );
        assert_eq!(
            parse_package_options(&strings(&["--target", "x", "--app", "bad"])),
            Err("unknown app: bad".to_string())
        );
    }

    #[test]
    fn app_packages_map_to_cargo_packages() {
        assert_eq!(AppPackage::Cli.package(), "fparkan-cli");
        assert_eq!(AppPackage::Game.package(), "fparkan-game");
        assert_eq!(AppPackage::Headless.package(), "fparkan-headless");
        assert_eq!(AppPackage::Viewer.package(), "fparkan-viewer");
    }

    #[test]
    fn policy_source_detection_is_scoped_to_code_files() {
        assert!(is_policy_source(Path::new("src/main.rs")));
        assert!(is_policy_source(Path::new("Cargo.toml")));
        assert!(!is_policy_source(Path::new("README.md")));
        assert!(should_skip_policy_path(Path::new("target")));
        assert!(should_skip_policy_path(Path::new("testdata")));
        assert!(!should_skip_policy_path(Path::new("crates/experimental")));
        assert!(!should_skip_policy_path(Path::new("crates/fparkan-render")));
    }

    #[test]
    fn unsafe_construct_detector_ignores_lints_and_comments() {
        assert!(contains_unsafe_construct(concat!(
            "un",
            "safe fn call() {}"
        )));
        assert!(contains_unsafe_construct(concat!(
            "let value = un",
            "safe { call() };"
        )));
        assert!(!contains_unsafe_construct("#![forbid(unsafe_code)]"));
    }
}
