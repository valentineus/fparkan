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
#![allow(clippy::print_stderr, clippy::print_stdout)]
//! Repository automation for `FParkan`.

use cargo_metadata::MetadataCommand;
use fparkan_corpus::{discover, render_report_json, report, DiscoverOptions};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const CORPORA_MANIFEST_ENV: &str = "FPARKAN_CORPORA_MANIFEST";
const PART1_ROOT_ENV: &str = "FPARKAN_CORPUS_PART1_ROOT";
const PART2_ROOT_ENV: &str = "FPARKAN_CORPUS_PART2_ROOT";
const CI_ACCEPTANCE_ROADMAP: &str = "fixtures/acceptance/stage_0_roadmap.md";
const CI_ACCEPTANCE_COVERAGE: &str = "fixtures/acceptance/coverage.tsv";
const CI_ACCEPTANCE_REPORT: &str = "target/fparkan/acceptance/stage-0-audit.json";
const STAGE_PACKAGE_MANIFEST: &str = "fixtures/acceptance/stage_packages.toml";
const SUPPLY_CHAIN_POLICY_CONFIG: &str = "deny.toml";
const REQUIRED_NATIVE_SMOKE_PLATFORMS: &[&str] = &["macos"];
const APPROVED_REGISTRY_SOURCE: &str = "registry+https://github.com/rust-lang/crates.io-index";
const SUPPLY_CHAIN_BANNED_PACKAGES: &[&str] = &["native-tls", "openssl", "openssl-sys"];
const PINNED_RUST_TOOLCHAIN: &str = "1.87.0";
const WORKSPACE_MSRV: &str = "1.87";
const ALLOW_SUPPLY_CHAIN_FALLBACK_ENV: &str = "FPARKAN_ALLOW_SUPPLY_CHAIN_FALLBACK";

fn workspace_root_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")))
        .to_path_buf()
}

fn workspace_relative_path(path: &str) -> PathBuf {
    workspace_root_path().join(path)
}

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
            run_cargo_fmt_check()?;
            run_policy(Path::new("."))?;
            cargo(&["test", "--workspace", "--all-targets", "--all-features", "--locked"])?;
            cargo(&[
                "clippy",
                "--workspace",
                "--all-targets",
                "--all-features",
                "--locked",
                "--",
                "-D",
                "warnings",
            ])?;
            run_cargo_doc()?;
            run_cargo_deny()?;
            run_acceptance_audit(&AuditOptions {
                roadmap: PathBuf::from(CI_ACCEPTANCE_ROADMAP),
                coverage: PathBuf::from(CI_ACCEPTANCE_COVERAGE),
                out: PathBuf::from(CI_ACCEPTANCE_REPORT),
                strict: true,
            })?;
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
        [cmd, subcmd, rest @ ..] if cmd == "native-smoke" && subcmd == "audit" => {
            let options = parse_native_smoke_audit_options(rest)?;
            run_native_smoke_audit(&options)
        }
        [cmd, rest @ ..] if cmd == "package" => {
            let options = parse_package_options(rest)?;
            run_package(&options)
        }
        [cmd, suite, rest @ ..] if cmd == "test" && suite == "synthetic" => {
            let options = parse_test_options(rest, PathBuf::from("testdata"))?;
            run_stage_tests(options.stage, TestSuite::Synthetic, None)
        }
        [cmd, suite, rest @ ..] if cmd == "test" && suite == "licensed" => {
            let options = parse_test_options(rest, PathBuf::from("testdata"))?;
            let roots = load_licensed_roots(options.manifest.as_deref())?;
            run_stage_tests(options.stage, TestSuite::Licensed, Some(&roots))
        }
        [cmd, subcmd, rest @ ..] if cmd == "corpus" && subcmd == "baseline" => {
            let root = parse_root(rest)?;
            let manifest =
                discover(&root, DiscoverOptions::default()).map_err(|e| e.to_string())?;
            let report = report(&root, &manifest).map_err(|e| e.to_string())?;
            println!("{}", render_report_json(&report));
            Ok(())
        }
        _ => Err(
            "usage: cargo xtask ci | policy | acceptance report --suite synthetic|licensed [--stage 0..5|all] [--manifest corpora.toml] [--out <path>] | acceptance audit [--roadmap <path>] [--coverage <path>] [--out <path>] [--strict] | native-smoke audit --dir <path> | package --target <triple> --app viewer|game|headless|cli | test synthetic|licensed [--stage 0..5|all] [--manifest corpora.toml] | corpus baseline --root <path>"
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

fn cargo_with_env(args: &[&str], envs: &[(&str, &Path)]) -> Result<(), String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    command.args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    let status = command
        .status()
        .map_err(|err| format!("failed to run cargo: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("cargo exited with {status}"))
    }
}

fn run_cargo_fmt_check() -> Result<(), String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .args(["fmt", "--all", "--", "--check"])
        .status()
        .map_err(|err| format!("failed to run rustfmt: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("cargo fmt exited with {status}"))
    }
}

fn run_cargo_deny() -> Result<(), String> {
    validate_supply_chain_policy_config(&workspace_relative_path(SUPPLY_CHAIN_POLICY_CONFIG))?;
    let cargo_deny = std::env::var_os("CARGO_DENY").unwrap_or_else(|| "cargo-deny".into());
    let version_output = match Command::new(&cargo_deny).arg("--version").output() {
        Ok(output) => output,
        Err(err) => {
            return handle_cargo_deny_fallback(&format!(
                "failed to run cargo-deny --version: {err}"
            ));
        }
    };
    if !version_output.status.success() {
        return handle_cargo_deny_fallback(&format!(
            "cargo-deny --version exited with {}",
            version_output.status
        ));
    }
    let version_text = String::from_utf8(version_output.stdout)
        .map_err(|err| format!("cargo-deny --version produced invalid UTF-8: {err}"))?;
    if !version_text.contains(PINNED_CARGO_DENY_VERSION) {
        return handle_cargo_deny_fallback(&format!(
            "cargo-deny version mismatch: expected {PINNED_CARGO_DENY_VERSION}, found {}",
            version_text.trim()
        ));
    }

    let status = Command::new(cargo_deny)
        .args([
            "check",
            "--workspace",
            "--all-features",
            "advisories",
            "bans",
            "licenses",
            "sources",
        ])
        .status()
        .map_err(|err| format!("failed to run cargo-deny: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("cargo-deny exited with {status}"))
    }
}

const PINNED_CARGO_DENY_VERSION: &str = "0.19.9";

fn handle_cargo_deny_fallback(reason: &str) -> Result<(), String> {
    if allow_supply_chain_fallback() {
        eprintln!(
            "{reason}; running built-in supply-chain policy fallback because {ALLOW_SUPPLY_CHAIN_FALLBACK_ENV} is set"
        );
        run_builtin_supply_chain_policy(Path::new("."))
    } else if std::env::var_os(ALLOW_SUPPLY_CHAIN_FALLBACK_ENV).is_some() && ci_env_active() {
        Err(format!(
            "{reason}; {ALLOW_SUPPLY_CHAIN_FALLBACK_ENV} is for local developer convenience only and is forbidden when CI is set"
        ))
    } else {
        Err(format!(
            "{reason}; install cargo-deny {PINNED_CARGO_DENY_VERSION} or explicitly opt into the fallback with {ALLOW_SUPPLY_CHAIN_FALLBACK_ENV}=1"
        ))
    }
}

fn validate_supply_chain_policy_config(path: &Path) -> Result<(), String> {
    if path.is_file() {
        Ok(())
    } else {
        Err(format!(
            "reviewed supply-chain policy config is missing: {}",
            path.display()
        ))
    }
}

fn allow_supply_chain_fallback() -> bool {
    std::env::var_os(ALLOW_SUPPLY_CHAIN_FALLBACK_ENV).is_some() && !ci_env_active()
}

fn ci_env_active() -> bool {
    ci_env_value_is_active(std::env::var("CI").ok().as_deref())
}

fn ci_env_value_is_active(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        let trimmed = value.trim();
        !trimmed.is_empty() && trimmed != "0" && !trimmed.eq_ignore_ascii_case("false")
    })
}

fn run_builtin_supply_chain_policy(root: &Path) -> Result<(), String> {
    let mut failures = Vec::new();
    validate_workspace_license(root, &mut failures)?;
    validate_lockfile_supply_chain(root, &mut failures)?;
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "built-in supply-chain policy failed:\n{}",
            failures.join("\n")
        ))
    }
}

fn run_cargo_doc() -> Result<(), String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .args([
            "doc",
            "--workspace",
            "--all-features",
            "--locked",
            "--no-deps",
        ])
        .env(
            "RUSTDOCFLAGS",
            "-D warnings -D rustdoc::broken_intra_doc_links",
        )
        .status()
        .map_err(|err| format!("failed to run cargo doc: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("cargo doc exited with {status}"))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LicensedCorpusRoots {
    part1: PathBuf,
    part2: PathBuf,
}

impl LicensedCorpusRoots {
    fn envs(&self) -> [(&str, &Path); 2] {
        [
            (PART1_ROOT_ENV, self.part1.as_path()),
            (PART2_ROOT_ENV, self.part2.as_path()),
        ]
    }
}

fn load_licensed_roots(manifest: Option<&Path>) -> Result<LicensedCorpusRoots, String> {
    let manifest = manifest
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os(CORPORA_MANIFEST_ENV).map(PathBuf::from))
        .ok_or_else(|| {
            format!(
                "licensed tests require --manifest or {CORPORA_MANIFEST_ENV}=<absolute corpora.toml>"
            )
    })?;
    parse_licensed_manifest(&manifest)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LicensedManifest {
    schema: Option<u8>,
    #[serde(rename = "corpus")]
    corpora: Vec<CorpusEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CorpusEntry {
    id: String,
    kind: CorpusKind,
    root: String,
    expected_profile: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum CorpusKind {
    Part1,
    Part2,
}

fn parse_licensed_manifest(path: &Path) -> Result<LicensedCorpusRoots, String> {
    let text = fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))?;
    let manifest: LicensedManifest = toml::from_str(&text)
        .map_err(|err| format!("failed to parse {}: {err}", path.display()))?;
    if manifest.schema.is_some_and(|version| version != 1) {
        return Err(format!(
            "unsupported corpora manifest schema {} (expected 1)",
            manifest.schema.unwrap_or(1)
        ));
    }

    let mut part1 = None;
    let mut part2 = None;

    for entry in manifest.corpora {
        match entry.kind {
            CorpusKind::Part1 => {
                let root = PathBuf::from(entry.root);
                assign_manifest_root(&mut part1, root, "part1")?;
            }
            CorpusKind::Part2 => {
                let root = PathBuf::from(entry.root);
                assign_manifest_root(&mut part2, root, "part2")?;
            }
        }
        if entry.expected_profile.is_none() {
            return Err(format!(
                "{}: corpus entry '{}' must define expected_profile",
                path.display(),
                entry.id
            ));
        }
    }

    let roots = LicensedCorpusRoots {
        part1: part1
            .ok_or_else(|| "licensed manifest is missing part1 corpus entry".to_string())?,
        part2: part2
            .ok_or_else(|| "licensed manifest is missing part2 corpus entry".to_string())?,
    };
    validate_licensed_part("part1", &roots.part1)?;
    validate_licensed_part("part2", &roots.part2)?;
    Ok(roots)
}

fn assign_manifest_root(
    target: &mut Option<PathBuf>,
    root: PathBuf,
    kind: &str,
) -> Result<(), String> {
    if target.replace(root).is_some() {
        return Err(format!("licensed manifest contains duplicate {kind} root"));
    }
    Ok(())
}

fn validate_licensed_part(kind: &str, root: &Path) -> Result<(), String> {
    if root.is_dir() {
        Ok(())
    } else {
        Err(format!(
            "licensed corpus {kind} root is missing: {}",
            root.display()
        ))
    }
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
    validate_toolchain_policy(root, &mut failures)?;
    scan_policy_dir(root, &mut failures)?;
    validate_cargo_metadata(root, &mut failures)?;
    validate_cargo_metadata_dependency_closures(root, &mut failures)?;
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
    let metadata = workspace_metadata(root)?;
    if metadata.workspace_members.is_empty() {
        failures.push(format!(
            "{}: cargo metadata produced no workspace members",
            manifest.display()
        ));
        return Ok(());
    }
    let stage_manifest_path = root.join(STAGE_PACKAGE_MANIFEST);
    let stage_manifest = load_stage_package_manifest(&stage_manifest_path)?;
    let workspace_packages = metadata
        .workspace_packages()
        .iter()
        .map(|package| package.name.to_string())
        .collect::<BTreeSet<_>>();
    if let Err(err) =
        validate_stage_package_entries(&stage_manifest, &workspace_packages, &stage_manifest_path)
    {
        failures.push(err);
    }
    Ok(())
}

fn validate_cargo_metadata_dependency_closures(
    root: &Path,
    failures: &mut Vec<String>,
) -> Result<(), String> {
    let mut manifests = Vec::new();
    collect_cargo_manifests(root, &mut manifests)?;
    let mut deps_by_package = BTreeMap::new();
    for manifest in manifests {
        let text = fs::read_to_string(&manifest)
            .map_err(|err| format!("{}: {err}", manifest.display()))?;
        let Some(package) = parse_package_name(&text) else {
            continue;
        };
        deps_by_package.insert(package, parse_manifest_dependencies(&text));
    }

    validate_package_closure_excludes("fparkan-headless", &deps_by_package, failures);
    Ok(())
}

fn validate_package_closure_excludes(
    package: &str,
    deps_by_package: &BTreeMap<String, BTreeSet<String>>,
    failures: &mut Vec<String>,
) {
    if !deps_by_package.contains_key(package) {
        failures.push(format!(
            "workspace manifest graph missing package {package}"
        ));
        return;
    }
    let closure = dependency_closure_names(package, deps_by_package);
    if let Some(forbidden) = first_forbidden_platform_bridge_dependency(&closure) {
        failures.push(format!(
            "workspace manifest closure: package {package} depends on forbidden platform/render dependency {forbidden}"
        ));
    }
}

fn dependency_closure_names(
    root: &str,
    deps_by_package: &BTreeMap<String, BTreeSet<String>>,
) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut names = BTreeSet::new();
    let mut stack = deps_by_package
        .get(root)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .collect::<Vec<_>>();
    while let Some(name) = stack.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        names.insert(name.clone());
        if let Some(deps) = deps_by_package.get(&name) {
            stack.extend(deps.iter().cloned());
        }
    }
    names
}

fn validate_toolchain_policy(root: &Path, failures: &mut Vec<String>) -> Result<(), String> {
    let toolchain_path = root.join("rust-toolchain.toml");
    let toolchain_text = fs::read_to_string(&toolchain_path)
        .map_err(|err| format!("{}: {err}", toolchain_path.display()))?;
    let toolchain = toml::from_str::<RustToolchainToml>(&toolchain_text)
        .map_err(|err| format!("{}: invalid TOML: {err}", toolchain_path.display()))?;
    if toolchain.toolchain.channel != PINNED_RUST_TOOLCHAIN {
        failures.push(format!(
            "{}: toolchain channel must be exact {PINNED_RUST_TOOLCHAIN}",
            toolchain_path.display()
        ));
    }
    if !is_exact_rust_patch_version(&toolchain.toolchain.channel) {
        failures.push(format!(
            "{}: toolchain channel must include major.minor.patch, not a moving channel",
            toolchain_path.display()
        ));
    }

    let manifest_path = root.join("Cargo.toml");
    let manifest_text = fs::read_to_string(&manifest_path)
        .map_err(|err| format!("{}: {err}", manifest_path.display()))?;
    let manifest = toml::from_str::<WorkspaceManifestToml>(&manifest_text)
        .map_err(|err| format!("{}: invalid TOML: {err}", manifest_path.display()))?;
    if manifest.workspace.package.rust_version != WORKSPACE_MSRV {
        failures.push(format!(
            "{}: workspace.package.rust-version must be {WORKSPACE_MSRV}",
            manifest_path.display()
        ));
    }
    if !PINNED_RUST_TOOLCHAIN.starts_with(&format!("{}.", manifest.workspace.package.rust_version))
    {
        failures.push(format!(
            "{}: workspace.package.rust-version must match pinned toolchain major.minor",
            manifest_path.display()
        ));
    }
    Ok(())
}

fn is_exact_rust_patch_version(value: &str) -> bool {
    let parts = value.split('.').collect::<Vec<_>>();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|ch| ch.is_ascii_digit()))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RustToolchainToml {
    toolchain: RustToolchainTable,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RustToolchainTable {
    channel: String,
    #[allow(dead_code)]
    components: Option<Vec<String>>,
    #[allow(dead_code)]
    targets: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct WorkspaceManifestToml {
    workspace: WorkspaceTable,
}

#[derive(Debug, Deserialize)]
struct WorkspaceTable {
    package: WorkspacePackageTable,
}

#[derive(Debug, Deserialize)]
struct WorkspacePackageTable {
    #[serde(rename = "rust-version")]
    rust_version: String,
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

fn validate_lockfile_supply_chain(root: &Path, failures: &mut Vec<String>) -> Result<(), String> {
    let lockfile = root.join("Cargo.lock");
    let packages = read_lockfile_packages(&lockfile)?;
    for package in packages {
        if let Some(source) = package.source.as_deref() {
            if source != APPROVED_REGISTRY_SOURCE {
                failures.push(format!(
                    "{}: package {} {} uses unapproved source {source}",
                    lockfile.display(),
                    package.name,
                    package.version
                ));
            }
        }
        if SUPPLY_CHAIN_BANNED_PACKAGES.contains(&package.name.as_str()) {
            failures.push(format!(
                "{}: package {} {} is banned by built-in supply-chain policy",
                lockfile.display(),
                package.name,
                package.version
            ));
        }
    }
    Ok(())
}

fn read_lockfile_packages(lockfile: &Path) -> Result<Vec<CargoLockPackage>, String> {
    let text =
        fs::read_to_string(lockfile).map_err(|err| format!("{}: {err}", lockfile.display()))?;
    let parsed = toml::from_str::<CargoLock>(&text)
        .map_err(|err| format!("{}: invalid Cargo.lock TOML: {err}", lockfile.display()))?;
    Ok(parsed.package)
}

#[derive(Debug, Deserialize)]
struct CargoLock {
    package: Vec<CargoLockPackage>,
}

#[derive(Debug, Deserialize)]
struct CargoLockPackage {
    name: String,
    version: String,
    source: Option<String>,
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
        if is_removed_legacy_adapter_manifest(root, &manifest) {
            failures.push(format!(
                "{}: legacy SDL/OpenGL adapter crate must be removed: {package}",
                manifest.display()
            ));
            continue;
        }
        let dependencies = parse_manifest_dependencies(&text);
        if !is_adapter_like_package(&package) && !is_app_package(&package) {
            for dependency in &dependencies {
                if is_forbidden_gui_dependency(dependency) {
                    failures.push(format!(
                        "{}: package {package} depends on forbidden GUI/adapter package {dependency}",
                        manifest.display()
                    ));
                }
            }
        }
        if is_app_package(&package) {
            if let Some(forbidden) = first_forbidden_parser_dependency(&dependencies) {
                failures.push(format!(
                    "{}: app package {package} depends on parser crate {forbidden}",
                    manifest.display()
                ));
            }
        }
        if package == "fparkan-headless" {
            if let Some(forbidden) = first_forbidden_platform_bridge_dependency(&dependencies) {
                failures.push(format!(
                    "{}: headless package {package} depends on platform/render bridge dependency {forbidden}",
                    manifest.display()
                ));
            }
        }

        if package == "fparkan-runtime" {
            if let Some(forbidden) = first_forbidden_parser_dependency(&dependencies) {
                failures.push(format!(
                    "{}: runtime package {package} depends on parser crate {forbidden}",
                    manifest.display()
                ));
            }
            if let Some(forbidden) = first_forbidden_platform_bridge_dependency(&dependencies) {
                failures.push(format!(
                    "{}: runtime package {package} depends on forbidden platform/driver crate {forbidden}",
                    manifest.display()
                ));
            }
        }

        if package == "fparkan-prototype" {
            if let Some(forbidden) = first_forbidden_visual_dependency(&dependencies) {
                failures.push(format!(
                    "{}: prototype package {package} depends on forbidden visual parser {forbidden}",
                    manifest.display()
                ));
            }
        }
    }
    Ok(())
}

fn is_app_package(package: &str) -> bool {
    matches!(
        package,
        "fparkan-cli"
            | "fparkan-game"
            | "fparkan-headless"
            | "fparkan-vulkan-smoke"
            | "fparkan-viewer"
    )
}

fn is_adapter_like_package(package: &str) -> bool {
    matches!(package, "fparkan-platform-winit" | "fparkan-render-vulkan")
}

fn first_forbidden_parser_dependency(dependencies: &BTreeSet<String>) -> Option<&str> {
    [
        "fparkan-msh",
        "fparkan-nres",
        "fparkan-rsli",
        "fparkan-terrain-format",
        "fparkan-texm",
        "fparkan-mission-format",
        "fparkan-material",
        "fparkan-fx",
    ]
    .iter()
    .find_map(|forbidden| {
        if dependencies.contains(*forbidden) {
            Some(*forbidden)
        } else {
            None
        }
    })
}

fn first_forbidden_visual_dependency(dependencies: &BTreeSet<String>) -> Option<&str> {
    [
        "fparkan-msh",
        "fparkan-material",
        "fparkan-texm",
        "fparkan-fx",
        "fparkan-terrain-format",
    ]
    .iter()
    .find_map(|forbidden| {
        if dependencies.contains(*forbidden) {
            Some(*forbidden)
        } else {
            None
        }
    })
}

fn first_forbidden_platform_bridge_dependency(dependencies: &BTreeSet<String>) -> Option<&str> {
    [
        "fparkan-platform-winit",
        "fparkan-render-vulkan",
        "winit",
        "ash",
        "ash-window",
    ]
    .iter()
    .find_map(|forbidden| {
        if dependencies.contains(*forbidden) {
            Some(*forbidden)
        } else {
            None
        }
    })
}

fn is_forbidden_domain_dependency(dependency: &str) -> bool {
    matches!(
        dependency,
        "fparkan-cli"
            | "fparkan-game"
            | "fparkan-headless"
            | "fparkan-viewer"
            | "fparkan-platform-sdl"
            | "fparkan-render-gl"
            | "sdl2"
            | "gl"
            | "glow"
            | "glium"
            | "glutin"
    )
}

fn is_forbidden_gui_dependency(dependency: &str) -> bool {
    is_forbidden_domain_dependency(dependency) || is_forbidden_platform_dependency(dependency)
}

fn is_forbidden_platform_dependency(dependency: &str) -> bool {
    matches!(
        dependency,
        "fparkan-platform-winit" | "fparkan-render-vulkan" | "winit" | "ash" | "ash-window"
    )
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

fn is_removed_legacy_adapter_manifest(root: &Path, manifest: &Path) -> bool {
    let normalized = manifest.strip_prefix(root).unwrap_or(manifest);
    normalized.starts_with("adapters/fparkan-platform-sdl")
        || normalized.starts_with("adapters/fparkan-render-gl")
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
    let mut previous_line_has_safety_comment = false;
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if is_comment_line(trimmed) {
            previous_line_has_safety_comment = has_safety_comment(trimmed);
            continue;
        }
        if trimmed.is_empty() {
            previous_line_has_safety_comment = false;
            continue;
        }
        if contains_unsafe_construct(trimmed)
            && !is_authorized_unsafe_construct(path, trimmed, previous_line_has_safety_comment)
        {
            failures.push(format!(
                "{}:{}: unsafe construct in workspace source",
                path.display(),
                index + 1
            ));
        }
        previous_line_has_safety_comment = false;
    }
    Ok(())
}

fn contains_unsafe_construct(line: &str) -> bool {
    line.contains(concat!("un", "safe {"))
        || line.contains(concat!("un", "safe fn"))
        || line.contains(concat!("un", "safe impl"))
        || line.contains(concat!("extern ", "\"C\""))
}

fn is_comment_line(line: &str) -> bool {
    line.starts_with("//") || line.starts_with("//!") || line.starts_with("///")
}

fn has_safety_comment(line: &str) -> bool {
    line.contains("SAFETY:")
}

const AUDITED_UNSAFE_SOURCE_FILES: &[&str] = &[
    "adapters/fparkan-render-vulkan/src/ffi.rs",
    "adapters/fparkan-render-vulkan/src/ffi/instance.rs",
    "adapters/fparkan-render-vulkan/src/ffi/resources.rs",
    "adapters/fparkan-render-vulkan/src/ffi/runtime.rs",
    "adapters/fparkan-render-vulkan/src/ffi/surface.rs",
    "adapters/fparkan-render-vulkan/src/ffi/validation.rs",
];

fn is_audited_unsafe_source(path: &Path) -> bool {
    let as_path = path.as_os_str().to_string_lossy();
    AUDITED_UNSAFE_SOURCE_FILES
        .iter()
        .any(|candidate| as_path.ends_with(candidate))
}

fn is_authorized_unsafe_construct(
    path: &Path,
    line: &str,
    previous_line_has_safety_comment: bool,
) -> bool {
    if !is_audited_unsafe_source(path) {
        return false;
    }
    previous_line_has_safety_comment || has_safety_comment(line)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Stage {
    All,
    Number(u8),
}

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
    manifest: Option<PathBuf>,
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
    manifest: Option<PathBuf>,
    out: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AuditOptions {
    roadmap: PathBuf,
    coverage: PathBuf,
    out: PathBuf,
    strict: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct NativeSmokeAuditOptions {
    dir: PathBuf,
}

fn parse_test_options(args: &[String], default_root: PathBuf) -> Result<TestOptions, String> {
    let mut options = TestOptions {
        stage: Stage::All,
        root: default_root,
        manifest: None,
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
            "--manifest" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--manifest requires a path".to_string())?;
                options.manifest = Some(PathBuf::from(value));
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
    let mut manifest = None;
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
            "--manifest" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--manifest requires a path".to_string())?;
                manifest = Some(PathBuf::from(value));
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
        manifest,
        out,
    })
}

fn parse_audit_options(args: &[String]) -> Result<AuditOptions, String> {
    let mut roadmap = workspace_relative_path(CI_ACCEPTANCE_ROADMAP);
    let mut coverage = workspace_relative_path(CI_ACCEPTANCE_COVERAGE);
    let mut out = workspace_root_path()
        .join("target")
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

fn parse_native_smoke_audit_options(args: &[String]) -> Result<NativeSmokeAuditOptions, String> {
    let mut dir = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--dir" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--dir requires a path".to_string())?;
                dir = Some(PathBuf::from(value));
            }
            _ => return Err(format!("unknown native-smoke audit option: {arg}")),
        }
    }
    Ok(NativeSmokeAuditOptions {
        dir: dir.ok_or_else(|| "native-smoke audit requires --dir".to_string())?,
    })
}

fn run_native_smoke_audit(options: &NativeSmokeAuditOptions) -> Result<(), String> {
    let reports = read_native_smoke_reports(&options.dir)?;
    let failures = audit_native_smoke_reports(&reports);
    if failures.is_empty() {
        println!("native smoke artifacts passed: {}", options.dir.display());
        Ok(())
    } else {
        Err(format!(
            "native smoke artifacts incomplete:\n{}",
            failures.join("\n")
        ))
    }
}

fn read_native_smoke_reports(dir: &Path) -> Result<BTreeMap<String, serde_json::Value>, String> {
    let mut reports = BTreeMap::new();
    let entries = fs::read_dir(dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|err| format!("{}: {err}", dir.display()))?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let text = fs::read_to_string(&path).map_err(|err| format!("{}: {err}", path.display()))?;
        let json = serde_json::from_str::<serde_json::Value>(&text)
            .map_err(|err| format!("{}: {err}", path.display()))?;
        let platform = json_string_field(&json, "platform")
            .map_err(|err| format!("{}: {err}", path.display()))?;
        let platform = platform.to_string();
        if reports.insert(platform.clone(), json).is_some() {
            return Err(format!(
                "{}: duplicate native smoke report for platform {platform}",
                path.display()
            ));
        }
    }
    Ok(reports)
}

fn audit_native_smoke_reports(reports: &BTreeMap<String, serde_json::Value>) -> Vec<String> {
    let mut failures = Vec::new();
    let mut commit_shas = BTreeSet::new();
    let mut rust_toolchains = BTreeSet::new();
    for platform in REQUIRED_NATIVE_SMOKE_PLATFORMS {
        let Some(report) = reports.get(*platform) else {
            failures.push(format!("{platform}: missing native smoke report"));
            continue;
        };
        validate_native_smoke_report(platform, report, &mut failures);
        if let Ok(commit_sha) = json_string_field(report, "commit_sha") {
            if commit_sha == "unknown" {
                failures.push(format!("{platform}: commit_sha must not be \"unknown\""));
            } else {
                commit_shas.insert(commit_sha.to_string());
            }
        }
        if let Ok(toolchain) = json_string_field(report, "rust_toolchain") {
            rust_toolchains.insert(toolchain.to_string());
        }
    }
    for platform in reports.keys() {
        if !REQUIRED_NATIVE_SMOKE_PLATFORMS.contains(&platform.as_str()) {
            failures.push(format!("{platform}: unexpected native smoke platform"));
        }
    }
    if commit_shas.len() > 1 {
        failures.push(format!(
            "native smoke reports disagree on commit_sha: {}",
            commit_shas.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    if rust_toolchains.len() > 1 {
        failures.push(format!(
            "native smoke reports disagree on rust_toolchain: {}",
            rust_toolchains.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    failures
}

fn validate_native_smoke_report(
    platform: &str,
    report: &serde_json::Value,
    failures: &mut Vec<String>,
) {
    validate_native_smoke_status_fields(platform, report, failures);
    validate_native_smoke_provenance_fields(platform, report, failures);
    validate_native_smoke_runtime_fields(platform, report, failures);
}

fn validate_native_smoke_status_fields(
    platform: &str,
    report: &serde_json::Value,
    failures: &mut Vec<String>,
) {
    expect_string_field(
        platform,
        report,
        "schema_version",
        "fparkan-native-smoke-v1",
        failures,
    );
    expect_string_field(platform, report, "status", "passed", failures);
    expect_string_field(
        platform,
        report,
        "vulkan_loader_status",
        "available",
        failures,
    );
    expect_string_field(
        platform,
        report,
        "vulkan_instance_status",
        "created",
        failures,
    );
    expect_string_field(platform, report, "window_status", "created", failures);
    expect_string_field(
        platform,
        report,
        "vulkan_surface_status",
        "created",
        failures,
    );
    expect_string_field(
        platform,
        report,
        "vulkan_device_status",
        "selected",
        failures,
    );
    expect_string_field(
        platform,
        report,
        "vulkan_logical_device_status",
        "created",
        failures,
    );
    expect_string_field(
        platform,
        report,
        "vulkan_swapchain_status",
        "created",
        failures,
    );
}

fn validate_native_smoke_provenance_fields(
    platform: &str,
    report: &serde_json::Value,
    failures: &mut Vec<String>,
) {
    expect_commit_sha(platform, report, failures);
    expect_bool_field(platform, report, "git_dirty", failures);
    expect_bool_field_value(platform, report, "git_dirty", false, failures);
    expect_nonempty_string(platform, report, "runner_identity", failures);
    expect_string_field(
        platform,
        report,
        "rust_toolchain",
        &measured_rust_toolchain_version(),
        failures,
    );
    expect_string_field(platform, report, "platform", platform, failures);
    expect_nonempty_string(platform, report, "target_triple", failures);
    expect_target_triple_matches_platform(platform, report, failures);
}

fn validate_native_smoke_runtime_fields(
    platform: &str,
    report: &serde_json::Value,
    failures: &mut Vec<String>,
) {
    expect_u64_at_least(platform, report, "frames", 300, failures);
    expect_u64_at_least(platform, report, "resize_count", 1, failures);
    expect_u64_at_least(platform, report, "swapchain_recreate_count", 1, failures);
    expect_u64_field(platform, report, "validation_warning_count", 0, failures);
    expect_u64_field(platform, report, "validation_error_count", 0, failures);
    expect_nonempty_string(platform, report, "shader_manifest_hash", failures);
    expect_nonempty_string(platform, report, "vulkan_device_name", failures);
    expect_u64_at_least(
        platform,
        report,
        "vulkan_logical_device_enabled_extension_count",
        1,
        failures,
    );
    expect_u64_at_least(
        platform,
        report,
        "vulkan_logical_device_graphics_queue_family",
        0,
        failures,
    );
    expect_u64_at_least(
        platform,
        report,
        "vulkan_logical_device_present_queue_family",
        0,
        failures,
    );
    expect_u64_at_least(platform, report, "vulkan_swapchain_width", 1, failures);
    expect_u64_at_least(platform, report, "vulkan_swapchain_height", 1, failures);
    expect_u64_at_least(
        platform,
        report,
        "vulkan_swapchain_image_count",
        2,
        failures,
    );
}

fn expect_string_field(
    platform: &str,
    report: &serde_json::Value,
    field: &str,
    expected: &str,
    failures: &mut Vec<String>,
) {
    match json_string_field(report, field) {
        Ok(actual) if actual == expected => {}
        Ok(actual) => failures.push(format!(
            "{platform}: {field} expected {expected:?}, found {actual:?}"
        )),
        Err(err) => failures.push(format!("{platform}: {err}")),
    }
}

fn expect_nonempty_string(
    platform: &str,
    report: &serde_json::Value,
    field: &str,
    failures: &mut Vec<String>,
) {
    match json_string_field(report, field) {
        Ok(value) if !value.trim().is_empty() => {}
        Ok(_) => failures.push(format!("{platform}: {field} must be non-empty")),
        Err(err) => failures.push(format!("{platform}: {err}")),
    }
}

fn expect_bool_field(
    platform: &str,
    report: &serde_json::Value,
    field: &str,
    failures: &mut Vec<String>,
) {
    match report.get(field) {
        Some(serde_json::Value::Bool(_)) => {}
        Some(_) => failures.push(format!("{platform}: {field} must be a boolean")),
        None => failures.push(format!("{platform}: missing {field}")),
    }
}

fn expect_bool_field_value(
    platform: &str,
    report: &serde_json::Value,
    field: &str,
    expected: bool,
    failures: &mut Vec<String>,
) {
    match report.get(field) {
        Some(serde_json::Value::Bool(actual)) if *actual == expected => {}
        Some(serde_json::Value::Bool(actual)) => {
            failures.push(format!(
                "{platform}: {field} expected {expected}, found {actual}"
            ));
        }
        Some(_) => failures.push(format!("{platform}: {field} must be a boolean")),
        None => failures.push(format!("{platform}: missing {field}")),
    }
}

fn expect_commit_sha(platform: &str, report: &serde_json::Value, failures: &mut Vec<String>) {
    match report.get("commit_sha") {
        Some(serde_json::Value::String(commit_sha))
            if commit_sha.len() == 40 && commit_sha.chars().all(|ch| ch.is_ascii_hexdigit()) => {}
        Some(serde_json::Value::String(_)) => failures.push(format!(
            "{platform}: commit_sha must be a 40-character lowercase or uppercase hex string"
        )),
        Some(_) => failures.push(format!("{platform}: commit_sha must be a string")),
        None => failures.push(format!("{platform}: missing commit_sha")),
    }
}

fn expect_target_triple_matches_platform(
    platform: &str,
    report: &serde_json::Value,
    failures: &mut Vec<String>,
) {
    let Ok(target_triple) = json_string_field(report, "target_triple") else {
        return;
    };
    let matches_platform = match platform {
        "macos" => target_triple.contains("apple-darwin"),
        "linux" => target_triple.contains("linux"),
        "windows" => target_triple.contains("windows"),
        _ => true,
    };
    if !matches_platform {
        failures.push(format!(
            "{platform}: target_triple {target_triple:?} does not match platform"
        ));
    }
}

fn expect_u64_at_least(
    platform: &str,
    report: &serde_json::Value,
    field: &str,
    minimum: u64,
    failures: &mut Vec<String>,
) {
    match json_u64_field(report, field) {
        Ok(value) if value >= minimum => {}
        Ok(value) => failures.push(format!(
            "{platform}: {field} expected >= {minimum}, found {value}"
        )),
        Err(err) => failures.push(format!("{platform}: {err}")),
    }
}

fn expect_u64_field(
    platform: &str,
    report: &serde_json::Value,
    field: &str,
    expected: u64,
    failures: &mut Vec<String>,
) {
    match json_u64_field(report, field) {
        Ok(value) if value == expected => {}
        Ok(value) => failures.push(format!(
            "{platform}: {field} expected {expected}, found {value}"
        )),
        Err(err) => failures.push(format!("{platform}: {err}")),
    }
}

fn json_string_field<'a>(json: &'a serde_json::Value, field: &str) -> Result<&'a str, String> {
    json.get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("{field} must be a string"))
}

fn json_u64_field(json: &serde_json::Value, field: &str) -> Result<u64, String> {
    json.get(field)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| format!("{field} must be an unsigned integer"))
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
    let rendered = render_audit_json(&audit)?;
    fs::write(&options.out, rendered).map_err(|err| format!("{}: {err}", options.out.display()))?;
    println!("{}", options.out.display());
    let strict_failures = audit.strict_failures();
    if options.strict && (!strict_failures.is_empty() || !audit.unknown_coverage.is_empty()) {
        Err(format!(
            "acceptance coverage incomplete: {} strict failures, {} unknown",
            strict_failures.len(),
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
    commit_sha: String,
    git_dirty: bool,
    runner_identity: String,
    rust_toolchain: String,
    msrv: String,
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
            .chain(&self.omitted)
            .chain(&self.missing)
            .cloned()
            .collect()
    }

    fn strict_failures(&self) -> Vec<String> {
        self.partial
            .iter()
            .chain(&self.blocked)
            .chain(&self.omitted)
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
    let required_scopes = required
        .iter()
        .filter_map(|id| id.get(0..2).map(ToString::to_string))
        .collect::<BTreeSet<_>>();

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
        .filter(|id| {
            !required.contains(*id)
                && id
                    .get(0..2)
                    .is_some_and(|scope| required_scopes.contains(scope))
        })
        .cloned()
        .collect();

    AcceptanceAudit {
        commit_sha: current_git_commit_sha(),
        git_dirty: current_git_dirty(),
        runner_identity: measured_runner_identity(),
        rust_toolchain: measured_rust_toolchain_version(),
        msrv: WORKSPACE_MSRV.to_string(),
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

#[derive(Serialize)]
struct AcceptanceAuditJson<'a> {
    schema_version: &'static str,
    commit_sha: &'a str,
    git_dirty: bool,
    runner_identity: &'a str,
    rust_toolchain: &'a str,
    msrv: &'a str,
    required_total: usize,
    covered_total: usize,
    partial_total: usize,
    blocked_total: usize,
    omitted_total: usize,
    missing_total: usize,
    unverified_total: usize,
    unknown_coverage_total: usize,
    by_stage: &'a BTreeMap<String, usize>,
    covered: &'a [String],
    partial: &'a [String],
    blocked: &'a [String],
    omitted: &'a [String],
    missing: &'a [String],
    unknown_coverage: &'a [String],
    coverage_evidence: &'a BTreeMap<String, String>,
}

fn render_audit_json(audit: &AcceptanceAudit) -> Result<String, String> {
    let unverified = audit.unverified();
    let report = AcceptanceAuditJson {
        schema_version: "fparkan-acceptance-coverage-v1",
        commit_sha: &audit.commit_sha,
        git_dirty: audit.git_dirty,
        runner_identity: &audit.runner_identity,
        rust_toolchain: &audit.rust_toolchain,
        msrv: &audit.msrv,
        required_total: audit.required_total,
        covered_total: audit.covered.len(),
        partial_total: audit.partial.len(),
        blocked_total: audit.blocked.len(),
        omitted_total: audit.omitted.len(),
        missing_total: audit.missing.len(),
        unverified_total: unverified.len(),
        unknown_coverage_total: audit.unknown_coverage.len(),
        by_stage: &audit.by_stage,
        covered: &audit.covered,
        partial: &audit.partial,
        blocked: &audit.blocked,
        omitted: &audit.omitted,
        missing: &audit.missing,
        unknown_coverage: &audit.unknown_coverage,
        coverage_evidence: &audit.coverage_evidence,
    };
    serde_json::to_string_pretty(&report)
        .map(|json| format!("{json}\n"))
        .map_err(|err| format!("acceptance audit serialization failed: {err}"))
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

fn measured_rust_toolchain_version() -> String {
    Command::new("rustc")
        .args(["-Vv"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|stdout| {
            stdout.lines().find_map(|line| {
                line.strip_prefix("release:")
                    .map(str::trim)
                    .map(ToString::to_string)
            })
        })
        .unwrap_or_else(|| PINNED_RUST_TOOLCHAIN.to_string())
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

fn run_acceptance_report(options: &AcceptanceOptions) -> Result<(), String> {
    let roots = if options.suite == TestSuite::Licensed {
        Some(load_licensed_roots(options.manifest.as_deref())?)
    } else {
        None
    };
    run_stage_tests(options.stage, options.suite, roots.as_ref())?;

    if let Some(parent) = options.out.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("{}: {err}", parent.display()))?;
    }
    let report = render_acceptance_report(options)?;
    fs::write(&options.out, report).map_err(|err| format!("{}: {err}", options.out.display()))?;
    println!("{}", options.out.display());
    Ok(())
}

#[derive(Serialize)]
struct AcceptanceLicensedCorpusReport<'a> {
    root: &'a str,
    parts: [&'a str; 2],
}

#[derive(Serialize)]
struct AcceptanceReportJson {
    schema_version: &'static str,
    suite: String,
    stage: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    licensed_corpus: Option<AcceptanceLicensedCorpusReport<'static>>,
    packages: Vec<String>,
}

fn render_acceptance_report(options: &AcceptanceOptions) -> Result<String, String> {
    let report = AcceptanceReportJson {
        schema_version: "fparkan-acceptance-report-v1",
        suite: options.suite.as_str().to_string(),
        stage: options.stage.to_string(),
        status: "passed",
        licensed_corpus: if options.suite == TestSuite::Licensed {
            Some(AcceptanceLicensedCorpusReport {
                root: "redacted",
                parts: ["IS", "IS2"],
            })
        } else {
            None
        },
        packages: stage_report_packages(options.stage).unwrap_or_default(),
    };
    serde_json::to_string_pretty(&report)
        .map(|json| format!("{json}\n"))
        .map_err(|err| format!("acceptance report serialization failed: {err}"))
}

fn stage_report_packages(stage: Stage) -> Result<Vec<String>, String> {
    let workspace_root = workspace_root_path();
    match stage {
        Stage::All => workspace_package_names(&workspace_root)
            .map(|packages| packages.into_iter().collect::<Vec<_>>()),
        Stage::Number(number) => stage_packages(number),
    }
}

fn run_stage_tests(
    stage: Stage,
    suite: TestSuite,
    roots: Option<&LicensedCorpusRoots>,
) -> Result<(), String> {
    let mut suffix = Vec::new();
    if suite == TestSuite::Licensed {
        suffix.extend(["--", "--ignored"]);
    }
    let envs = roots.map(LicensedCorpusRoots::envs);
    match stage {
        Stage::All => {
            let mut args = vec!["test", "--workspace", "--locked", "--offline"];
            args.extend(suffix);
            if let Some(envs) = envs {
                cargo_with_env(&args, &envs)
            } else {
                cargo(&args)
            }
        }
        Stage::Number(number) => {
            for package in stage_packages(number)? {
                let mut args = vec![
                    "test".to_string(),
                    "-p".to_string(),
                    package,
                    "--locked".to_string(),
                    "--offline".to_string(),
                ];
                args.extend(suffix.iter().map(|value| (*value).to_string()));
                if let Some(envs) = envs {
                    let borrowed = args.iter().map(String::as_str).collect::<Vec<_>>();
                    cargo_with_env(&borrowed, &envs)?;
                } else {
                    cargo_owned(&args)?;
                }
            }
            Ok(())
        }
    }
}

fn stage_packages(stage: u8) -> Result<Vec<String>, String> {
    let manifest_path = workspace_relative_path(STAGE_PACKAGE_MANIFEST);
    let manifest = load_stage_package_manifest(&manifest_path)?;
    let packages = manifest
        .stages
        .get(&stage.to_string())
        .cloned()
        .ok_or_else(|| format!("stage out of range: {stage}"))?;
    validate_stage_package_entries(
        &manifest,
        &workspace_package_names(&workspace_root_path())?,
        &manifest_path,
    )?;
    Ok(packages)
}

fn workspace_package_names(root: &Path) -> Result<BTreeSet<String>, String> {
    let metadata = workspace_metadata(root)?;
    Ok(metadata
        .workspace_packages()
        .iter()
        .map(|package| package.name.to_string())
        .collect())
}

fn workspace_metadata(root: &Path) -> Result<cargo_metadata::Metadata, String> {
    let manifest = root.join("Cargo.toml");
    MetadataCommand::new()
        .manifest_path(&manifest)
        .no_deps()
        .other_options(["--offline".to_string(), "--locked".to_string()])
        .exec()
        .map_err(|error| format!("{}: cargo metadata failed: {}", manifest.display(), error))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StagePackageManifest {
    schema: Option<u8>,
    stages: BTreeMap<String, Vec<String>>,
}

fn load_stage_package_manifest(path: &Path) -> Result<StagePackageManifest, String> {
    let text = fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))?;
    let manifest = toml::from_str::<StagePackageManifest>(&text)
        .map_err(|err| format!("failed to parse {}: {err}", path.display()))?;
    if manifest.schema != Some(1) {
        return Err(format!(
            "{}: unsupported stage package manifest schema {:?} (expected 1)",
            path.display(),
            manifest.schema
        ));
    }
    Ok(manifest)
}

fn validate_stage_package_entries(
    manifest: &StagePackageManifest,
    workspace_packages: &BTreeSet<String>,
    path: &Path,
) -> Result<(), String> {
    let required_stages = (0_u8..=5_u8)
        .map(|stage| stage.to_string())
        .collect::<BTreeSet<_>>();
    let declared_stages = manifest.stages.keys().cloned().collect::<BTreeSet<_>>();
    if declared_stages != required_stages {
        return Err(format!(
            "{}: stage package manifest must declare stages 0 through 5 exactly once",
            path.display()
        ));
    }

    let mut assigned = BTreeSet::new();
    for (stage, packages) in &manifest.stages {
        for package in packages {
            if !workspace_packages.contains(package) {
                return Err(format!(
                    "{}: stage {stage} references unknown package {package}",
                    path.display()
                ));
            }
            if !assigned.insert(package.clone()) {
                return Err(format!(
                    "{}: package {package} is assigned to multiple stages",
                    path.display()
                ));
            }
        }
    }

    let missing = workspace_packages
        .difference(&assigned)
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(format!(
            "{}: stage package manifest is missing workspace packages: {}",
            path.display(),
            missing.join(", ")
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        std::env::temp_dir().join(format!("fparkan-xtask-{name}-{suffix}"))
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
                manifest: None,
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
            "--manifest",
            "corpora.toml",
            "--out",
            "target/report.json",
        ]));

        assert_eq!(
            parsed,
            Ok(AcceptanceOptions {
                suite: TestSuite::Licensed,
                stage: Stage::Number(5),
                root: PathBuf::from("testdata"),
                manifest: Some(PathBuf::from("corpora.toml")),
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
            manifest: Some(PathBuf::from("/private/corpora.toml")),
            out: PathBuf::from("target/report.json"),
        };
        let report =
            render_acceptance_report(&options).expect("acceptance report should serialize");

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
                "S0-ARCH-099".to_string(),
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
        assert_eq!(audit.unknown_coverage, ["S0-ARCH-099"]);
        assert_eq!(audit.by_stage.get("S0"), Some(&2));
        assert_eq!(
            audit.strict_failures(),
            strings(&["L5-RG40-001", "L3-DEVICE-001", "S0-ARCH-002"])
        );
    }

    #[test]
    fn audit_json_escapes_evidence() {
        let mut audit = AcceptanceAudit {
            commit_sha: "0123456789abcdef0123456789abcdef01234567".to_string(),
            git_dirty: false,
            runner_identity: "github-actions/12345/stage0-macos".to_string(),
            rust_toolchain: PINNED_RUST_TOOLCHAIN.to_string(),
            msrv: WORKSPACE_MSRV.to_string(),
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

        let json = render_audit_json(&audit).expect("acceptance audit should serialize");

        assert!(json.contains("quoted \\\"value\\\""));
        assert!(json.contains("\"commit_sha\": \"0123456789abcdef0123456789abcdef01234567\""));
        assert!(json.contains("\"git_dirty\": false"));
        assert!(json.contains("\"runner_identity\": \"github-actions/12345/stage0-macos\""));
        assert!(json.contains("\"rust_toolchain\": \"1.87.0\""));
        assert!(json.contains("\"msrv\": \"1.87\""));
    }

    #[test]
    fn native_smoke_audit_accepts_complete_required_platform_pass() {
        let reports = ["macos"]
            .into_iter()
            .map(|platform| {
                let target_triple = match platform {
                    "macos" => "aarch64-apple-darwin",
                    "linux" => "x86_64-unknown-linux-gnu",
                    "windows" => "x86_64-pc-windows-msvc",
                    _ => "unknown-target",
                };
                (
                    platform.to_string(),
                    serde_json::json!({
                        "schema_version": "fparkan-native-smoke-v1",
                        "commit_sha": "0123456789abcdef0123456789abcdef01234567",
                        "git_dirty": false,
                        "runner_identity": "github-actions/12345/stage0-macos",
                        "rust_toolchain": measured_rust_toolchain_version(),
                        "target_triple": target_triple,
                        "platform": platform,
                        "status": "passed",
                        "frames": 300,
                        "resize_count": 1,
                        "swapchain_recreate_count": 1,
                        "validation_warning_count": 0,
                        "validation_error_count": 0,
                        "shader_manifest_hash": "dd293e4ff08ffca1c037900d08b0ffd415db39f238b4fcdde46468fa049b679c",
                        "vulkan_loader_status": "available",
                        "vulkan_instance_status": "created",
                        "window_status": "created",
                        "vulkan_surface_status": "created",
                        "vulkan_device_status": "selected",
                        "vulkan_device_name": format!("{platform} GPU"),
                        "vulkan_logical_device_status": "created",
                        "vulkan_logical_device_graphics_queue_family": 0,
                        "vulkan_logical_device_present_queue_family": 0,
                        "vulkan_logical_device_enabled_extension_count": 1,
                        "vulkan_swapchain_status": "created",
                        "vulkan_swapchain_width": 1280,
                        "vulkan_swapchain_height": 720,
                        "vulkan_swapchain_image_count": 3
                    }),
                )
            })
            .collect::<BTreeMap<_, _>>();

        assert_eq!(audit_native_smoke_reports(&reports), Vec::<String>::new());
    }

    #[test]
    fn native_smoke_audit_rejects_blocked_or_incomplete_reports() {
        let reports = [(
            "macos".to_string(),
            serde_json::json!({
                "schema_version": "fparkan-native-smoke-v1",
                "commit_sha": "unknown",
                "git_dirty": true,
                "runner_identity": "",
                "rust_toolchain": measured_rust_toolchain_version(),
                "target_triple": "x86_64-unknown-linux-gnu",
                "platform": "macos",
                "status": "blocked",
                "frames": 0,
                "resize_count": 0,
                "swapchain_recreate_count": 0,
                "validation_warning_count": null,
                "validation_error_count": null,
                "shader_manifest_hash": "dd293e4ff08ffca1c037900d08b0ffd415db39f238b4fcdde46468fa049b679c",
                "vulkan_loader_status": "unavailable",
                "vulkan_instance_status": "skipped",
                "window_status": "planned",
                "vulkan_surface_status": "skipped",
                "vulkan_device_status": "skipped",
                "vulkan_swapchain_status": "skipped"
            }),
        )]
        .into_iter()
        .collect::<BTreeMap<_, _>>();

        let failures = audit_native_smoke_reports(&reports);

        assert!(
            failures.contains(&"macos: status expected \"passed\", found \"blocked\"".to_string())
        );
        assert!(failures.contains(
            &"macos: commit_sha must be a 40-character lowercase or uppercase hex string"
                .to_string()
        ));
        assert!(failures.contains(&"macos: git_dirty expected false, found true".to_string()));
        assert!(failures.contains(&"macos: runner_identity must be non-empty".to_string()));
        assert!(failures.contains(
            &"macos: target_triple \"x86_64-unknown-linux-gnu\" does not match platform"
                .to_string()
        ));
        assert!(failures.contains(&"macos: frames expected >= 300, found 0".to_string()));
        assert!(failures
            .contains(&"macos: validation_error_count must be an unsigned integer".to_string()));
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
                manifest: None,
            })
        );
    }

    #[test]
    fn parses_licensed_corpora_manifest() -> Result<(), String> {
        let root = temp_dir("manifest");
        let part1 = root.join("IS");
        let part2 = root.join("IS2");
        fs::create_dir_all(&part1).map_err(|err| err.to_string())?;
        fs::create_dir_all(&part2).map_err(|err| err.to_string())?;
        let manifest = root.join("corpora.toml");
        fs::write(
            &manifest,
            format!(
                "schema = 1\n\n[[corpus]]\nid = \"part1-local\"\nkind = \"part1\"\nroot = \"{}\"\nexpected_profile = \"parkan-is-part1\"\n\n[[corpus]]\nid = \"part2-local\"\nkind = \"part2\"\nroot = \"{}\"\nexpected_profile = \"parkan-is-part2\"\n",
                part1.display(),
                part2.display()
            ),
        )
        .map_err(|err| err.to_string())?;

        assert_eq!(
            parse_licensed_manifest(&manifest)?,
            LicensedCorpusRoots { part1, part2 }
        );
        fs::remove_dir_all(root).map_err(|err| err.to_string())?;
        Ok(())
    }

    #[test]
    fn licensed_roots_require_manifest_configuration() {
        let previous = std::env::var_os(CORPORA_MANIFEST_ENV);
        std::env::remove_var(CORPORA_MANIFEST_ENV);

        assert_eq!(
            load_licensed_roots(None),
            Err(format!(
                "licensed tests require --manifest or {CORPORA_MANIFEST_ENV}=<absolute corpora.toml>"
            ))
        );

        if let Some(value) = previous {
            std::env::set_var(CORPORA_MANIFEST_ENV, value);
        }
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
        assert!(stage_packages(0)
            .is_ok_and(|packages| packages.contains(&"fparkan-platform".to_string())));
        assert!(stage_packages(0)
            .is_ok_and(|packages| packages.contains(&"fparkan-vulkan-smoke".to_string())));
        assert!(stage_packages(1)
            .is_ok_and(|packages| packages.contains(&"fparkan-inspection".to_string())));
        assert!(stage_packages(5)
            .is_ok_and(|packages| packages.contains(&"fparkan-runtime".to_string())));
        assert!(
            stage_packages(5).is_ok_and(|packages| packages.contains(&"fparkan-game".to_string()))
        );
        assert_eq!(stage_packages(9), Err("stage out of range: 9".to_string()));
    }

    #[test]
    fn stage_package_manifest_covers_workspace_once() -> Result<(), String> {
        let manifest_path = workspace_relative_path(STAGE_PACKAGE_MANIFEST);
        let manifest = load_stage_package_manifest(&manifest_path)?;
        let workspace_packages = workspace_package_names(&workspace_root_path())?;

        validate_stage_package_entries(&manifest, &workspace_packages, &manifest_path)?;

        Ok(())
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
fparkan-render-vulkan = { path = "../../adapters/fparkan-render-vulkan" }
"#;

        assert_eq!(
            parse_package_name(manifest),
            Some("fparkan-example".to_string())
        );
        let deps = parse_manifest_dependencies(manifest);
        assert!(deps.contains("fparkan-render"));
        assert!(deps.contains("quoted-dep"));
        assert!(deps.contains("fparkan-render-vulkan"));
    }

    #[test]
    fn workspace_manifest_closure_detects_transitive_platform_bridge() {
        let deps_by_package = [
            (
                "fparkan-headless".to_string(),
                ["fparkan-runtime".to_string()].into_iter().collect(),
            ),
            (
                "fparkan-runtime".to_string(),
                ["fparkan-render-vulkan".to_string()].into_iter().collect(),
            ),
            ("fparkan-render-vulkan".to_string(), BTreeSet::new()),
        ]
        .into_iter()
        .collect::<BTreeMap<_, _>>();

        let closure = dependency_closure_names("fparkan-headless", &deps_by_package);

        assert!(closure.contains("fparkan-runtime"));
        assert_eq!(
            first_forbidden_platform_bridge_dependency(&closure),
            Some("fparkan-render-vulkan")
        );
    }

    #[test]
    fn toolchain_policy_rejects_moving_toolchain() -> Result<(), String> {
        let root = temp_dir("toolchain-moving");
        fs::create_dir_all(&root).map_err(|err| err.to_string())?;
        fs::write(
            root.join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"stable\"\n",
        )
        .map_err(|err| err.to_string())?;
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\n[workspace.package]\nrust-version = \"1.87\"\n",
        )
        .map_err(|err| err.to_string())?;

        let mut failures = Vec::new();
        validate_toolchain_policy(&root, &mut failures)?;

        assert_eq!(failures.len(), 2);
        assert!(failures[0].contains("must be exact"));
        assert!(failures[1].contains("major.minor.patch"));
        fs::remove_dir_all(root).map_err(|err| err.to_string())?;
        Ok(())
    }

    #[test]
    fn toolchain_policy_rejects_msrv_mismatch() -> Result<(), String> {
        let root = temp_dir("toolchain-msrv");
        fs::create_dir_all(&root).map_err(|err| err.to_string())?;
        fs::write(
            root.join("rust-toolchain.toml"),
            format!("[toolchain]\nchannel = \"{PINNED_RUST_TOOLCHAIN}\"\n"),
        )
        .map_err(|err| err.to_string())?;
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\n[workspace.package]\nrust-version = \"1.86\"\n",
        )
        .map_err(|err| err.to_string())?;

        let mut failures = Vec::new();
        validate_toolchain_policy(&root, &mut failures)?;

        assert_eq!(failures.len(), 2);
        assert!(failures[0].contains("rust-version must be"));
        assert!(failures[1].contains("must match pinned toolchain"));
        fs::remove_dir_all(root).map_err(|err| err.to_string())?;
        Ok(())
    }

    #[test]
    fn lockfile_supply_chain_rejects_unapproved_sources() -> Result<(), String> {
        let root = temp_dir("lockfile-source");
        fs::create_dir_all(&root).map_err(|err| err.to_string())?;
        fs::write(
            root.join("Cargo.lock"),
            r#"
[[package]]
name = "external"
version = "1.0.0"
source = "git+https://example.invalid/repo"
"#,
        )
        .map_err(|err| err.to_string())?;

        let mut failures = Vec::new();
        validate_lockfile_supply_chain(&root, &mut failures)?;

        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("uses unapproved source"));
        fs::remove_dir_all(root).map_err(|err| err.to_string())?;
        Ok(())
    }

    #[test]
    fn lockfile_supply_chain_rejects_banned_packages() -> Result<(), String> {
        let root = temp_dir("lockfile-ban");
        fs::create_dir_all(&root).map_err(|err| err.to_string())?;
        fs::write(
            root.join("Cargo.lock"),
            format!(
                "[[package]]\nname = \"openssl\"\nversion = \"0.10.0\"\nsource = \"{APPROVED_REGISTRY_SOURCE}\"\n"
            ),
        )
        .map_err(|err| err.to_string())?;

        let mut failures = Vec::new();
        validate_lockfile_supply_chain(&root, &mut failures)?;

        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("is banned"));
        fs::remove_dir_all(root).map_err(|err| err.to_string())?;
        Ok(())
    }

    #[test]
    fn supply_chain_policy_config_must_exist() -> Result<(), String> {
        let root = temp_dir("supply-chain-config");
        fs::create_dir_all(&root).map_err(|err| err.to_string())?;

        let missing = root.join("deny.toml");
        assert!(validate_supply_chain_policy_config(&missing).is_err());

        fs::write(&missing, "[graph]\nall-features = true\n").map_err(|err| err.to_string())?;
        assert_eq!(validate_supply_chain_policy_config(&missing), Ok(()));

        fs::remove_dir_all(root).map_err(|err| err.to_string())?;
        Ok(())
    }

    #[test]
    fn ci_env_truthy_values_are_detected() {
        assert!(ci_env_value_is_active(Some("true")));
        assert!(ci_env_value_is_active(Some("1")));
        assert!(ci_env_value_is_active(Some("yes")));
        assert!(!ci_env_value_is_active(None));
        assert!(!ci_env_value_is_active(Some("")));
        assert!(!ci_env_value_is_active(Some("0")));
        assert!(!ci_env_value_is_active(Some("false")));
        assert!(!ci_env_value_is_active(Some(" FALSE ")));
    }

    #[test]
    fn detects_forbidden_domain_dependencies() {
        assert!(!is_forbidden_domain_dependency("fparkan-render-vulkan"));
        assert!(is_forbidden_domain_dependency("sdl2"));
        assert!(is_forbidden_domain_dependency("fparkan-platform-sdl"));
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
