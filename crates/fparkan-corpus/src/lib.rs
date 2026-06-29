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
//! Licensed corpus discovery and aggregate reports.

use fparkan_binary::{sha256, sha256_hex, Sha256Digest};
use fparkan_fx::{decode_fxid, FXID_KIND};
use fparkan_material::{decode_mat0, decode_wear, MAT0_KIND, WEAR_KIND};
use fparkan_mission_format::{decode_tma, TmaProfile};
use fparkan_msh::{decode_msh, validate_msh};
use fparkan_nres::NresDocument;
use fparkan_path::{ascii_lookup_key, normalize_relative, PathPolicy};
use fparkan_prototype::{decode_unit_dat, decode_unit_dat_binding};
use fparkan_rsli::{decode as decode_rsli, ReadProfile};
use fparkan_terrain_format::{decode_land_map, decode_land_msh};
use fparkan_texm::decode_texm;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const TEXM_KIND: u32 = 0x6d78_6554;

/// Corpus kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CorpusKind {
    /// Demo corpus.
    Demo,
    /// Part 1 full game.
    Part1,
    /// Part 2 full game.
    Part2,
    /// Unknown local directory.
    Unknown,
}

/// Corpus root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CorpusRoot(pub PathBuf);

/// Discovery options.
#[derive(Clone, Copy, Debug, Default)]
pub struct DiscoverOptions {
    /// Whether symlinks may be traversed.
    pub follow_symlinks: bool,
}

/// File manifest entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManifestEntry {
    /// Normalized relative path.
    pub path: String,
    /// Byte-exact relative host path used for reopening corpus files.
    pub host_rel_path: PathBuf,
    /// File size in bytes.
    pub size: u64,
    /// SHA-256 content fingerprint.
    pub hash: Sha256Digest,
}

/// Corpus manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CorpusManifest {
    /// Kind.
    pub kind: CorpusKind,
    /// Sorted files.
    pub files: Vec<ManifestEntry>,
    /// Casefold collisions.
    pub casefold_collisions: Vec<Vec<String>>,
}

/// Aggregate report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CorpusReport {
    /// Schema version.
    pub schema: u32,
    /// Kind.
    pub kind: CorpusKind,
    /// Total files.
    pub files: usize,
    /// Total bytes.
    pub bytes: u64,
    /// Metrics.
    pub metrics: BTreeMap<String, u64>,
    /// Casefold collision count.
    pub casefold_collisions: usize,
    /// Manifest fingerprint.
    pub fingerprint: Sha256Digest,
    /// Per-file status records.
    pub records: Vec<CorpusFileRecord>,
    /// Number of files with report errors.
    pub failures: usize,
}

/// Per-file report status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CorpusFileStatus {
    /// File was inspected successfully.
    Ok,
    /// File was inspected but produced a non-fatal warning.
    Warning,
    /// File could not be inspected.
    Error,
}

/// Per-file report record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CorpusFileRecord {
    /// Normalized relative path.
    pub path: String,
    /// Inspection status.
    pub status: CorpusFileStatus,
    /// Detected file variant.
    pub variant: String,
    /// Optional status message.
    pub message: Option<String>,
}

/// Corpus error.
#[derive(Debug)]
pub enum CorpusError {
    /// I/O failure.
    Io {
        /// Path where I/O failed.
        path: PathBuf,
        /// Source error.
        source: std::io::Error,
    },
    /// Invalid root.
    InvalidRoot(PathBuf),
    /// Invalid path.
    InvalidPath(String),
    /// Aggregate report failure.
    Report {
        /// Path where reporting failed.
        path: String,
        /// Failure message.
        message: String,
    },
}

impl fmt::Display for CorpusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::InvalidRoot(path) => write!(f, "invalid corpus root: {}", path.display()),
            Self::InvalidPath(path) => write!(f, "invalid corpus path: {path}"),
            Self::Report { path, message } => write!(f, "{path}: {message}"),
        }
    }
}

impl std::error::Error for CorpusError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::InvalidRoot(_) | Self::InvalidPath(_) | Self::Report { .. } => None,
        }
    }
}

/// Discovers a corpus under a root directory.
///
/// # Errors
///
/// Returns [`CorpusError`] if the root is invalid, traversal encounters an I/O
/// error, or a discovered path cannot be represented by the legacy path policy.
pub fn discover(root: &Path, options: DiscoverOptions) -> Result<CorpusManifest, CorpusError> {
    if !root.is_dir() {
        return Err(CorpusError::InvalidRoot(root.to_path_buf()));
    }
    let mut files = Vec::new();
    walk(root, root, options, &mut files)?;
    files.sort_by(|a, b| a.host_rel_path.cmp(&b.host_rel_path));

    let kind = classify(root, &files);
    let casefold_collisions = detect_casefold_collisions(&files);
    Ok(CorpusManifest {
        kind,
        files,
        casefold_collisions,
    })
}

fn walk(
    root: &Path,
    dir: &Path,
    options: DiscoverOptions,
    out: &mut Vec<ManifestEntry>,
) -> Result<(), CorpusError> {
    let read_dir = fs::read_dir(dir).map_err(|source| CorpusError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    let mut entries = Vec::new();
    for entry in read_dir {
        let entry = entry.map_err(|source| CorpusError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        entries.push(entry.path());
    }
    entries.sort();
    for path in entries {
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with('.'))
        {
            continue;
        }
        let metadata = fs::symlink_metadata(&path).map_err(|source| CorpusError::Io {
            path: path.clone(),
            source,
        })?;
        if metadata.file_type().is_symlink() && !options.follow_symlinks {
            continue;
        }
        if metadata.is_dir() {
            walk(root, &path, options, out)?;
            continue;
        }
        if !metadata.is_file() {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .map_err(|_| CorpusError::InvalidPath(path.display().to_string()))?;
        #[cfg(unix)]
        let rel_bytes = rel.as_os_str().as_bytes();
        #[cfg(not(unix))]
        let rel_bytes = rel
            .to_str()
            .ok_or_else(|| CorpusError::InvalidPath(path.display().to_string()))?
            .as_bytes();
        let normalized = normalize_relative(rel_bytes, PathPolicy::HostCompatible)
            .map_err(|_| CorpusError::InvalidPath(path.display().to_string()))?;
        let bytes = fs::read(&path).map_err(|source| CorpusError::Io {
            path: path.clone(),
            source,
        })?;
        out.push(ManifestEntry {
            path: normalized.display_lossy().to_string(),
            host_rel_path: rel.to_path_buf(),
            size: metadata.len(),
            hash: sha256(&bytes),
        });
    }
    Ok(())
}

fn classify(root: &Path, files: &[ManifestEntry]) -> CorpusKind {
    let name = root
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or_default()
        .to_ascii_uppercase();
    if name == "IS" {
        CorpusKind::Part1
    } else if name == "IS2" {
        CorpusKind::Part2
    } else if files
        .iter()
        .any(|f| f.path.eq_ignore_ascii_case("iron_3d.exe"))
    {
        CorpusKind::Part1
    } else {
        CorpusKind::Unknown
    }
}

fn detect_casefold_collisions(files: &[ManifestEntry]) -> Vec<Vec<String>> {
    let mut grouped: BTreeMap<Vec<u8>, BTreeSet<String>> = BTreeMap::new();
    for file in files {
        grouped
            .entry(ascii_lookup_key(path_identity_bytes(&file.host_rel_path)).0)
            .or_default()
            .insert(file.path.clone());
    }
    grouped
        .into_values()
        .filter(|paths| paths.len() > 1)
        .map(|paths| paths.into_iter().collect())
        .collect()
}

/// Builds aggregate report.
///
/// # Errors
///
/// Returns [`CorpusError`] when the aggregate report cannot be constructed.
/// Per-file inspection failures are represented in [`CorpusReport::records`]
/// and counted in [`CorpusReport::failures`].
pub fn report(root: &Path, manifest: &CorpusManifest) -> Result<CorpusReport, CorpusError> {
    let mut metrics = empty_report_metrics();
    let mut records = Vec::with_capacity(manifest.files.len());
    let mut failures = 0usize;

    for entry in &manifest.files {
        let record = inspect_report_file(root, entry, &mut metrics);
        if record.status == CorpusFileStatus::Error {
            failures = failures.saturating_add(1);
        }
        records.push(record);
    }

    Ok(CorpusReport {
        schema: 1,
        kind: manifest.kind,
        files: manifest.files.len(),
        bytes: manifest.files.iter().map(|f| f.size).sum(),
        metrics,
        casefold_collisions: manifest.casefold_collisions.len(),
        fingerprint: fingerprint(manifest),
        records,
        failures,
    })
}

fn empty_report_metrics() -> BTreeMap<String, u64> {
    let mut metrics = BTreeMap::new();
    metrics.insert("nres_files".to_string(), 0);
    metrics.insert("nres_entries".to_string(), 0);
    metrics.insert("rsli_files".to_string(), 0);
    metrics.insert("tma_files".to_string(), 0);
    metrics.insert("land_msh_files".to_string(), 0);
    metrics.insert("land_map_files".to_string(), 0);
    metrics.insert("unit_dat_files".to_string(), 0);
    metrics.insert("msh_entries".to_string(), 0);
    metrics.insert("mat0_entries".to_string(), 0);
    metrics.insert("texm_entries".to_string(), 0);
    metrics.insert("fxid_entries".to_string(), 0);
    metrics.insert("wear_entries".to_string(), 0);
    metrics
}

fn inspect_report_file(
    root: &Path,
    entry: &ManifestEntry,
    metrics: &mut BTreeMap<String, u64>,
) -> CorpusFileRecord {
    let lower = entry.path.to_ascii_lowercase();
    let mut variant = inspect_path_metrics(&lower, metrics);
    let path = root.join(&entry.host_rel_path);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(source) => {
            return CorpusFileRecord {
                path: entry.path.clone(),
                status: CorpusFileStatus::Error,
                variant,
                message: Some(source.to_string()),
            };
        }
    };
    if bytes.starts_with(b"NRes") {
        if variant == "file" {
            variant = "nres".to_string();
        }
        bump(metrics, "nres_files", 1);
        if let Err(message) = inspect_nres_metrics(&bytes, metrics) {
            return CorpusFileRecord {
                path: entry.path.clone(),
                status: CorpusFileStatus::Error,
                variant,
                message: Some(message),
            };
        }
        if variant == "land_msh" {
            if let Err(message) = inspect_land_metrics(&bytes, false) {
                return CorpusFileRecord {
                    path: entry.path.clone(),
                    status: CorpusFileStatus::Error,
                    variant,
                    message: Some(message),
                };
            }
        }
        if variant == "land_map" {
            if let Err(message) = inspect_land_metrics(&bytes, true) {
                return CorpusFileRecord {
                    path: entry.path.clone(),
                    status: CorpusFileStatus::Error,
                    variant,
                    message: Some(message),
                };
            }
        }
    } else if bytes.starts_with(b"NL") {
        variant = "rsli".to_string();
        bump(metrics, "rsli_files", 1);
        if let Err(message) = inspect_rsli_metrics(&bytes) {
            return CorpusFileRecord {
                path: entry.path.clone(),
                status: CorpusFileStatus::Error,
                variant,
                message: Some(message),
            };
        }
    } else if lower.ends_with("data.tma") {
        if let Err(message) = inspect_tma_metrics(&bytes) {
            return CorpusFileRecord {
                path: entry.path.clone(),
                status: CorpusFileStatus::Error,
                variant: "tma".to_string(),
                message: Some(message),
            };
        }
    } else if has_extension(&lower, "dat")
        && (lower.starts_with("units/") || lower.contains("/units/"))
    {
        variant = "unit_dat".to_string();
        if let Err(message) = inspect_unit_dat_metrics(&bytes) {
            return CorpusFileRecord {
                path: entry.path.clone(),
                status: CorpusFileStatus::Error,
                variant,
                message: Some(message),
            };
        }
    }
    CorpusFileRecord {
        path: entry.path.clone(),
        status: CorpusFileStatus::Ok,
        variant,
        message: None,
    }
}

fn path_identity_bytes(path: &Path) -> &[u8] {
    #[cfg(unix)]
    {
        path.as_os_str().as_bytes()
    }
    #[cfg(not(unix))]
    {
        path.to_str().unwrap_or_default().as_bytes()
    }
}

fn inspect_path_metrics(lower: &str, metrics: &mut BTreeMap<String, u64>) -> String {
    let mut variant = "file";
    if lower.ends_with("data.tma") {
        bump(metrics, "tma_files", 1);
        variant = "tma";
    }
    if lower.ends_with("land.msh") {
        bump(metrics, "land_msh_files", 1);
        variant = "land_msh";
    }
    if lower.ends_with("land.map") {
        bump(metrics, "land_map_files", 1);
        variant = "land_map";
    }
    if has_extension(lower, "dat") && (lower.starts_with("units/") || lower.contains("/units/")) {
        bump(metrics, "unit_dat_files", 1);
        variant = "unit_dat";
    }
    variant.to_string()
}

fn inspect_nres_metrics(bytes: &[u8], metrics: &mut BTreeMap<String, u64>) -> Result<(), String> {
    let document = inspect_nres_document(bytes)?;
    bump(metrics, "nres_entries", document.entries().len() as u64);
    for entry in document.entries() {
        let name = String::from_utf8_lossy(entry.name_bytes()).to_ascii_lowercase();
        if has_extension(&name, "msh") {
            bump(metrics, "msh_entries", 1);
            validate_nres_msh_payload(&document, entry)?;
        }
        match entry.meta().type_id {
            MAT0_KIND => {
                bump(metrics, "mat0_entries", 1);
                validate_nres_mat0_payload(&document, entry)?;
            }
            TEXM_KIND => {
                bump(metrics, "texm_entries", 1);
                validate_nres_texm_payload(&document, entry)?;
            }
            FXID_KIND => {
                bump(metrics, "fxid_entries", 1);
                validate_nres_fxid_payload(&document, entry)?;
            }
            WEAR_KIND => {
                bump(metrics, "wear_entries", 1);
                validate_nres_wear_payload(&document, entry)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_nres_msh_payload(
    document: &NresDocument,
    entry: &fparkan_nres::NresEntry,
) -> Result<(), String> {
    let payload = document
        .payload(entry.id())
        .map_err(|err| err.to_string())?;
    let nested = fparkan_nres::decode(
        Arc::from(payload.to_vec().into_boxed_slice()),
        fparkan_nres::ReadProfile::Compatible,
    )
    .map_err(|err| err.to_string())?;
    let model = decode_msh(&nested).map_err(|err| err.to_string())?;
    validate_msh(&model).map_err(|err| err.to_string())?;
    Ok(())
}

fn validate_nres_mat0_payload(
    document: &NresDocument,
    entry: &fparkan_nres::NresEntry,
) -> Result<(), String> {
    let payload = document
        .payload(entry.id())
        .map_err(|err| err.to_string())?;
    decode_mat0(payload, entry.meta().attr2).map_err(|err| err.to_string())?;
    Ok(())
}

fn validate_nres_wear_payload(
    document: &NresDocument,
    entry: &fparkan_nres::NresEntry,
) -> Result<(), String> {
    let payload = document
        .payload(entry.id())
        .map_err(|err| err.to_string())?;
    decode_wear(payload).map_err(|err| err.to_string())?;
    Ok(())
}

fn validate_nres_texm_payload(
    document: &NresDocument,
    entry: &fparkan_nres::NresEntry,
) -> Result<(), String> {
    let payload = document
        .payload(entry.id())
        .map_err(|err| err.to_string())?;
    decode_texm(Arc::from(payload.to_vec().into_boxed_slice())).map_err(|err| err.to_string())?;
    Ok(())
}

fn validate_nres_fxid_payload(
    document: &NresDocument,
    entry: &fparkan_nres::NresEntry,
) -> Result<(), String> {
    let payload = document
        .payload(entry.id())
        .map_err(|err| err.to_string())?;
    decode_fxid(Arc::from(payload.to_vec().into_boxed_slice())).map_err(|err| err.to_string())?;
    Ok(())
}

fn inspect_rsli_metrics(bytes: &[u8]) -> Result<(), String> {
    let _ = decode_rsli(
        Arc::from(bytes.to_vec().into_boxed_slice()),
        ReadProfile::Compatible,
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}

fn inspect_tma_metrics(bytes: &[u8]) -> Result<(), String> {
    let _ = decode_tma(
        Arc::from(bytes.to_vec().into_boxed_slice()),
        TmaProfile::Strict,
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}

fn inspect_unit_dat_metrics(bytes: &[u8]) -> Result<(), String> {
    if decode_unit_dat(bytes).is_err() && decode_unit_dat_binding(bytes).is_err() {
        return Err("failed to parse unit.dat payload as unit or binding format".to_string());
    }
    Ok(())
}

fn inspect_land_metrics(bytes: &[u8], is_map: bool) -> Result<(), String> {
    let document = inspect_nres_document(bytes)?;
    if is_map {
        decode_land_map(&document).map_err(|err| err.to_string())?;
    } else {
        decode_land_msh(&document).map_err(|err| err.to_string())?;
    }
    Ok(())
}

fn inspect_nres_document(bytes: &[u8]) -> Result<NresDocument, String> {
    fparkan_nres::decode(
        Arc::from(bytes.to_vec().into_boxed_slice()),
        fparkan_nres::ReadProfile::Compatible,
    )
    .map_err(|err| err.to_string())
}

fn bump(metrics: &mut BTreeMap<String, u64>, key: &str, delta: u64) {
    if let Some(value) = metrics.get_mut(key) {
        *value = value.saturating_add(delta);
    }
}

fn has_extension(path: &str, expected: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case(expected))
}

/// Computes stable manifest fingerprint.
#[must_use]
pub fn fingerprint(manifest: &CorpusManifest) -> Sha256Digest {
    let mut bytes = Vec::new();
    for file in &manifest.files {
        bytes.extend_from_slice(file.path.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(&file.size.to_le_bytes());
        bytes.extend_from_slice(&file.hash);
    }
    sha256(&bytes)
}

/// Writes report atomically.
///
/// # Errors
///
/// Returns [`CorpusError`] if the parent directory, temporary file, write, or
/// final rename operation fails.
pub fn write_report_atomic(path: &Path, report: &CorpusReport) -> Result<(), CorpusError> {
    let tmp = path.with_extension("tmp");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| CorpusError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let mut file = fs::File::create(&tmp).map_err(|source| CorpusError::Io {
        path: tmp.clone(),
        source,
    })?;
    file.write_all(render_report_json(report).as_bytes())
        .map_err(|source| CorpusError::Io {
            path: tmp.clone(),
            source,
        })?;
    file.sync_all().map_err(|source| CorpusError::Io {
        path: tmp.clone(),
        source,
    })?;
    fs::rename(&tmp, path).map_err(|source| CorpusError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

/// Renders report JSON.
#[must_use]
pub fn render_report_json(report: &CorpusReport) -> String {
    let mut out = format!(
        "{{\"schema_version\":\"fparkan-corpus-report-v1\",\"schema\":{},\"kind\":\"{:?}\",\"files\":{},\"bytes\":{},\"casefold_collisions\":{},\"fingerprint\":\"{}\",\"failures\":{},\"record_count\":{},\"metrics\":{{",
        report.schema,
        report.kind,
        report.files,
        report.bytes,
        report.casefold_collisions,
        sha256_hex(&report.fingerprint),
        report.failures,
        report.records.len()
    );
    for (idx, (key, value)) in report.metrics.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(key);
        out.push_str("\":");
        out.push_str(&value.to_string());
    }
    out.push_str("}}");
    out.push('}');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_path::join_under;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    #[ignore = "requires licensed corpus"]
    fn report_for_testdata_roots() {
        let root = licensed_root("IS");
        let manifest = discover(&root, DiscoverOptions::default()).expect("manifest");
        let report = report(&root, &manifest).expect("report");
        assert!(report.files > 0);
        assert!(report.metrics["nres_files"] > 0);
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_part1_manifest_profile_and_counts_match_baseline() {
        let root = testdata_root("IS");
        let manifest = discover(&root, DiscoverOptions::default()).expect("part 1 manifest");
        let report = report(&root, &manifest).expect("report");

        assert_eq!(manifest.kind, CorpusKind::Part1);
        assert_eq!(report.files, 1_017);
        assert_eq!(report.metrics["nres_files"], 120);
        assert_eq!(report.metrics["rsli_files"], 2);
        assert_eq!(report.metrics["tma_files"], 29);
        assert_eq!(report.metrics["land_msh_files"], 33);
        assert_eq!(report.metrics["land_map_files"], 33);
        assert_eq!(report.metrics["unit_dat_files"], 425);
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_part2_manifest_profile_and_counts_match_baseline() {
        let root = testdata_root("IS2");
        let manifest = discover(&root, DiscoverOptions::default()).expect("part 2 manifest");
        let report = report(&root, &manifest).expect("report");

        assert_eq!(manifest.kind, CorpusKind::Part2);
        assert_eq!(report.files, 1_302);
        assert_eq!(report.metrics["nres_files"], 134);
        assert_eq!(report.metrics["rsli_files"], 2);
        assert_eq!(report.metrics["tma_files"], 31);
        assert_eq!(report.metrics["land_msh_files"], 32);
        assert_eq!(report.metrics["land_map_files"], 32);
        assert_eq!(report.metrics["unit_dat_files"], 676);
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_part1_has_no_casefold_relative_path_collisions() {
        let root = testdata_root("IS");
        let manifest = discover(&root, DiscoverOptions::default()).expect("part 1 manifest");

        assert!(manifest.casefold_collisions.is_empty());
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_part2_has_no_casefold_relative_path_collisions() {
        let root = testdata_root("IS2");
        let manifest = discover(&root, DiscoverOptions::default()).expect("part 2 manifest");

        assert!(manifest.casefold_collisions.is_empty());
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_part1_paths_stay_under_root() {
        assert_discovered_paths_stay_under_root("IS");
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_part2_paths_stay_under_root() {
        assert_discovered_paths_stay_under_root("IS2");
    }

    #[test]
    fn report_json_contains_metrics_and_hashes_not_paths_or_payloads() {
        let manifest = CorpusManifest {
            kind: CorpusKind::Part1,
            files: vec![manifest_entry("secret/payload.bin", 4, sha256(b"DATA"))],
            casefold_collisions: Vec::new(),
        };
        let report = report(Path::new("."), &manifest).expect("report");
        let json = render_report_json(&report);

        assert!(json.contains("\"schema_version\":\"fparkan-corpus-report-v1\""));
        assert!(json.contains("\"fingerprint\":"));
        assert!(json.contains("\"failures\":1"));
        assert!(json.contains("\"record_count\":1"));
        assert!(json.contains("\"metrics\":"));
        assert!(!json.contains("secret/payload.bin"));
        assert!(!json.contains("DATA"));
    }

    #[test]
    fn report_records_missing_manifest_files_as_failures() {
        let root = temp_dir("report-missing");
        let manifest = CorpusManifest {
            kind: CorpusKind::Unknown,
            files: vec![manifest_entry("missing.lib", 1, sha256(b"missing"))],
            casefold_collisions: Vec::new(),
        };

        let report = report(&root, &manifest).expect("report");

        assert_eq!(report.failures, 1);
        assert_eq!(report.records.len(), 1);
        assert_eq!(report.records[0].path, "missing.lib");
        assert_eq!(report.records[0].status, CorpusFileStatus::Error);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn report_records_malformed_nres_as_failure() {
        let root = temp_dir("report-bad-nres");
        fs::write(root.join("bad.lib"), b"NRes").expect("bad nres");
        let manifest = CorpusManifest {
            kind: CorpusKind::Unknown,
            files: vec![manifest_entry("bad.lib", 4, sha256(b"NRes"))],
            casefold_collisions: Vec::new(),
        };

        let report = report(&root, &manifest).expect("report");

        assert_eq!(report.failures, 1);
        assert_eq!(report.records[0].status, CorpusFileStatus::Error);
        assert_eq!(report.records[0].variant, "nres");
        assert!(report.records[0]
            .message
            .as_deref()
            .is_some_and(|message| message.contains("NRes")));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn report_uses_production_nres_parser_for_entry_metrics() {
        let root = temp_dir("report-nres");
        let archive = build_nres(&[
            TestNresEntry {
                name: "mesh.msh",
                type_id: 0,
                payload: b"mesh",
            },
            TestNresEntry {
                name: "mat.bin",
                type_id: 0x3054_414D,
                payload: b"mat0",
            },
            TestNresEntry {
                name: "texture.bin",
                type_id: 0x6D78_6554,
                payload: b"texm",
            },
        ]);
        fs::write(root.join("archive.lib"), &archive).expect("archive");
        let manifest = CorpusManifest {
            kind: CorpusKind::Unknown,
            files: vec![manifest_entry(
                "archive.lib",
                u64::try_from(archive.len()).expect("archive size"),
                sha256(&archive),
            )],
            casefold_collisions: Vec::new(),
        };

        let report = report(&root, &manifest).expect("report");

        assert_eq!(report.failures, 1);
        assert_eq!(report.records.len(), 1);
        assert_eq!(report.records[0].status, CorpusFileStatus::Error);
        assert_eq!(report.records[0].variant, "nres");
        assert_eq!(report.metrics["nres_files"], 1);
        assert_eq!(report.metrics["nres_entries"], 3);
        assert_eq!(report.metrics["msh_entries"], 1);
        assert_eq!(report.metrics["mat0_entries"], 0);
        assert_eq!(report.metrics["texm_entries"], 0);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn report_land_map_paths_use_production_land_parser() {
        let root = temp_dir("report-land-map");
        fs::create_dir_all(root.join("WORLD/MAP")).expect("land map dir");
        fs::write(root.join("WORLD/MAP/land.map"), build_nres(&[])).expect("land map");
        let manifest = CorpusManifest {
            kind: CorpusKind::Unknown,
            files: vec![manifest_entry("WORLD/MAP/land.map", 16, sha256(b"land.map"))],
            casefold_collisions: Vec::new(),
        };

        let report = report(&root, &manifest).expect("report");

        assert_eq!(report.failures, 1);
        assert_eq!(report.records[0].status, CorpusFileStatus::Error);
        assert_eq!(report.records[0].variant, "land_map");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn report_land_msh_paths_use_production_land_parser() {
        let root = temp_dir("report-land-msh");
        fs::create_dir_all(root.join("WORLD/MAP")).expect("land msh dir");
        fs::write(root.join("WORLD/MAP/land.msh"), build_nres(&[])).expect("land msh");
        let manifest = CorpusManifest {
            kind: CorpusKind::Unknown,
            files: vec![manifest_entry("WORLD/MAP/land.msh", 16, sha256(b"land.msh"))],
            casefold_collisions: Vec::new(),
        };

        let report = report(&root, &manifest).expect("report");

        assert_eq!(report.failures, 1);
        assert_eq!(report.records[0].status, CorpusFileStatus::Error);
        assert_eq!(report.records[0].variant, "land_msh");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn report_tma_paths_use_production_tma_parser() {
        let root = temp_dir("report-tma");
        fs::create_dir_all(root.join("MISSIONS/test")).expect("tma dir");
        fs::write(root.join("MISSIONS/test/data.tma"), b"malformed tma").expect("tma");
        let manifest = CorpusManifest {
            kind: CorpusKind::Unknown,
            files: vec![manifest_entry(
                "MISSIONS/test/data.tma",
                12,
                sha256(b"malformed tma"),
            )],
            casefold_collisions: Vec::new(),
        };

        let report = report(&root, &manifest).expect("report");

        assert_eq!(report.failures, 1);
        assert_eq!(report.records[0].status, CorpusFileStatus::Error);
        assert_eq!(report.records[0].variant, "tma");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn report_unit_dat_paths_use_production_unit_parser() {
        let root = temp_dir("report-unit");
        fs::create_dir_all(root.join("units")).expect("unit dir");
        fs::write(root.join("units/unit.dat"), vec![0u8; 120]).expect("unit");
        let manifest = CorpusManifest {
            kind: CorpusKind::Unknown,
            files: vec![manifest_entry("units/unit.dat", 120, sha256(&[0u8; 120]))],
            casefold_collisions: Vec::new(),
        };

        let report = report(&root, &manifest).expect("report");

        assert_eq!(report.failures, 0);
        assert_eq!(report.records[0].status, CorpusFileStatus::Ok);
        assert_eq!(report.records[0].variant, "unit_dat");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn report_rsli_paths_use_production_rsli_parser() {
        let root = temp_dir("report-rsli");
        fs::write(root.join("patch.nl"), b"NL malformed").expect("rsli");
        let manifest = CorpusManifest {
            kind: CorpusKind::Unknown,
            files: vec![manifest_entry("patch.nl", 12, sha256(b"NL malformed"))],
            casefold_collisions: Vec::new(),
        };

        let report = report(&root, &manifest).expect("report");

        assert_eq!(report.failures, 1);
        assert_eq!(report.records[0].status, CorpusFileStatus::Error);
        assert_eq!(report.records[0].variant, "rsli");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn deterministic_traversal_is_creation_order_independent() {
        let first = temp_dir("order-first");
        let second = temp_dir("order-second");
        fs::create_dir_all(first.join("nested")).expect("first nested");
        fs::create_dir_all(second.join("nested")).expect("second nested");

        fs::write(first.join("b.bin"), b"b").expect("first b");
        fs::write(first.join("nested").join("a.bin"), b"a").expect("first a");
        fs::write(second.join("nested").join("a.bin"), b"a").expect("second a");
        fs::write(second.join("b.bin"), b"b").expect("second b");

        let first_manifest = discover(&first, DiscoverOptions::default()).expect("first manifest");
        let second_manifest =
            discover(&second, DiscoverOptions::default()).expect("second manifest");

        assert_eq!(first_manifest.files, second_manifest.files);
        let _ = fs::remove_dir_all(first);
        let _ = fs::remove_dir_all(second);
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_directory_produces_error() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_dir("unreadable");
        let child = root.join("locked");
        fs::create_dir_all(&child).expect("locked dir");
        fs::set_permissions(&child, fs::Permissions::from_mode(0o000)).expect("lock dir");

        let result = discover(&root, DiscoverOptions::default());

        fs::set_permissions(&child, fs::Permissions::from_mode(0o700)).expect("unlock dir");
        let _ = fs::remove_dir_all(root);
        assert!(matches!(result, Err(CorpusError::Io { path, .. }) if path.ends_with("locked")));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_loop_is_not_traversed_by_default() {
        use std::os::unix::fs::symlink;

        let root = temp_dir("symlink-loop");
        fs::write(root.join("real.bin"), b"real").expect("real file");
        symlink(&root, root.join("loop")).expect("loop symlink");

        let manifest = discover(&root, DiscoverOptions::default()).expect("manifest");

        assert_eq!(manifest.files.len(), 1);
        assert_eq!(manifest.files[0].path, "real.bin");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn casefold_collisions_are_registered() {
        let manifest = CorpusManifest {
            kind: CorpusKind::Unknown,
            files: vec![
                manifest_entry("Textures/Foo.TEX", 1, sha256(b"first")),
                manifest_entry("textures/foo.tex", 1, sha256(b"second")),
            ],
            casefold_collisions: Vec::new(),
        };

        let collisions = detect_casefold_collisions(&manifest.files);

        assert_eq!(
            collisions,
            vec![vec![
                "Textures/Foo.TEX".to_string(),
                "textures/foo.tex".to_string()
            ]]
        );
    }

    #[test]
    fn fingerprint_changes() {
        let mut manifest = CorpusManifest {
            kind: CorpusKind::Unknown,
            files: vec![manifest_entry("a", 1, sha256(b"before"))],
            casefold_collisions: Vec::new(),
        };
        let a = fingerprint(&manifest);
        manifest.files[0].hash = sha256(b"after");
        assert_ne!(a, fingerprint(&manifest));
    }

    #[test]
    fn atomic_report_write() {
        let tmp = std::env::temp_dir().join(format!(
            "fparkan-report-{}.json",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let report = CorpusReport {
            schema: 1,
            kind: CorpusKind::Unknown,
            files: 0,
            bytes: 0,
            metrics: BTreeMap::new(),
            casefold_collisions: 0,
            fingerprint: sha256(b"empty-report"),
            records: Vec::new(),
            failures: 0,
        };
        write_report_atomic(&tmp, &report).expect("write");
        assert!(tmp.is_file());
        let _ = fs::remove_file(tmp);
    }

    #[cfg(unix)]
    #[test]
    fn discover_supports_non_utf8_host_paths() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let root = temp_dir("non-utf8");
        let file_name = OsString::from_vec(vec![0xFF, b'.', b'b', b'i', b'n']);
        let file_path = root.join(&file_name);
        if let Err(err) = fs::write(&file_path, b"raw") {
            assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
            let _ = fs::remove_dir_all(root);
            return;
        }

        let manifest = discover(&root, DiscoverOptions::default()).expect("manifest");

        assert_eq!(manifest.files.len(), 1);
        assert_eq!(manifest.files[0].path, "\u{FFFD}.bin");
        assert_eq!(manifest.files[0].host_rel_path, PathBuf::from(&file_name));
        let _ = fs::remove_dir_all(root);
    }

    struct TestNresEntry<'a> {
        name: &'a str,
        type_id: u32,
        payload: &'a [u8],
    }

    fn build_nres(entries: &[TestNresEntry<'_>]) -> Vec<u8> {
        let mut out = vec![0; 16];
        let mut offsets = Vec::with_capacity(entries.len());
        for entry in entries {
            offsets.push(u32::try_from(out.len()).expect("offset"));
            out.extend_from_slice(entry.payload);
            let padding = (8 - (out.len() % 8)) % 8;
            out.resize(out.len() + padding, 0);
        }
        let mut order: Vec<usize> = (0..entries.len()).collect();
        order.sort_by(|left, right| {
            entries[*left]
                .name
                .as_bytes()
                .cmp(entries[*right].name.as_bytes())
        });
        for (index, entry) in entries.iter().enumerate() {
            push_u32(&mut out, entry.type_id);
            push_u32(&mut out, 0);
            push_u32(&mut out, 0);
            push_u32(
                &mut out,
                u32::try_from(entry.payload.len()).expect("payload size"),
            );
            push_u32(&mut out, 0);
            let mut name = [0; 36];
            let name_bytes = entry.name.as_bytes();
            name[..name_bytes.len()].copy_from_slice(name_bytes);
            out.extend_from_slice(&name);
            push_u32(&mut out, offsets[index]);
            push_u32(&mut out, u32::try_from(order[index]).expect("sort index"));
        }
        out[0..4].copy_from_slice(b"NRes");
        out[4..8].copy_from_slice(&0x100_u32.to_le_bytes());
        out[8..12].copy_from_slice(&u32::try_from(entries.len()).expect("count").to_le_bytes());
        let total_size = u32::try_from(out.len()).expect("total size");
        out[12..16].copy_from_slice(&total_size.to_le_bytes());
        out
    }

    fn manifest_entry(path: &str, size: u64, hash: Sha256Digest) -> ManifestEntry {
        ManifestEntry {
            path: path.to_string(),
            host_rel_path: PathBuf::from(path),
            size,
            hash,
        }
    }

    fn push_u32(out: &mut Vec<u8>, value: u32) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "fparkan-corpus-{name}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&path).expect("temp dir");
        path
    }

    fn testdata_root(part: &str) -> PathBuf {
        licensed_root(part)
    }

    fn licensed_root(part: &str) -> PathBuf {
        let variable = match part {
            "IS" => "FPARKAN_CORPUS_PART1_ROOT",
            "IS2" => "FPARKAN_CORPUS_PART2_ROOT",
            _ => panic!("unknown licensed corpus part: {part}"),
        };
        let root = std::env::var_os(variable)
            .map(PathBuf::from)
            .unwrap_or_else(|| panic!("{variable} is required for licensed corpus tests"));
        assert!(
            root.is_dir(),
            "licensed corpus root is missing: {}",
            root.display()
        );
        root
    }

    fn assert_discovered_paths_stay_under_root(part: &str) {
        let root = testdata_root(part);
        let manifest = discover(&root, DiscoverOptions::default()).expect("licensed manifest");

        for entry in &manifest.files {
            let normalized = normalize_relative(entry.path.as_bytes(), PathPolicy::HostCompatible)
                .expect("discovered path should re-normalize");
            let joined = join_under(&root, &normalized).expect("discovered path should join");
            assert!(
                joined.starts_with(&root),
                "discovered path escaped root: {}",
                entry.path
            );
        }
    }
}
