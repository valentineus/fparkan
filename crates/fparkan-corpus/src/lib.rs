#![forbid(unsafe_code)]
//! Licensed corpus discovery and aggregate reports.

use fparkan_path::{ascii_lookup_key, normalize_relative, PathPolicy};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

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
    /// File size in bytes.
    pub size: u64,
    /// Stable content fingerprint.
    pub hash: u64,
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
    pub fingerprint: u64,
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
}

impl fmt::Display for CorpusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::InvalidRoot(path) => write!(f, "invalid corpus root: {}", path.display()),
            Self::InvalidPath(path) => write!(f, "invalid corpus path: {path}"),
        }
    }
}

impl std::error::Error for CorpusError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::InvalidRoot(_) | Self::InvalidPath(_) => None,
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
    files.sort_by(|a, b| a.path.cmp(&b.path));

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
        let rel_text = rel
            .to_str()
            .ok_or_else(|| CorpusError::InvalidPath(path.display().to_string()))?;
        let normalized = normalize_relative(rel_text.as_bytes(), PathPolicy::HostCompatible)
            .map_err(|_| CorpusError::InvalidPath(rel_text.to_string()))?;
        let bytes = fs::read(&path).map_err(|source| CorpusError::Io {
            path: path.clone(),
            source,
        })?;
        out.push(ManifestEntry {
            path: normalized.as_str().to_string(),
            size: metadata.len(),
            hash: stable_hash(&bytes),
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
            .entry(ascii_lookup_key(file.path.as_bytes()).0)
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
#[must_use]
pub fn report(root: &Path, manifest: &CorpusManifest) -> CorpusReport {
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

    for entry in &manifest.files {
        let lower = entry.path.to_ascii_lowercase();
        if lower.ends_with("data.tma") {
            bump(&mut metrics, "tma_files", 1);
        }
        if lower.ends_with("land.msh") {
            bump(&mut metrics, "land_msh_files", 1);
        }
        if lower.ends_with("land.map") {
            bump(&mut metrics, "land_map_files", 1);
        }
        if has_extension(&lower, "dat")
            && (lower.starts_with("units/") || lower.contains("/units/"))
        {
            bump(&mut metrics, "unit_dat_files", 1);
        }

        let path = root.join(&entry.path);
        if let Ok(bytes) = fs::read(path) {
            if bytes.starts_with(b"NRes") {
                bump(&mut metrics, "nres_files", 1);
                if let Some(entries) = inspect_nres_entries(&bytes) {
                    bump(&mut metrics, "nres_entries", entries.len() as u64);
                    for entry in entries {
                        let name = entry.name.to_ascii_lowercase();
                        if has_extension(&name, "msh") {
                            bump(&mut metrics, "msh_entries", 1);
                        }
                        match entry.kind {
                            0x3054_414D => {
                                bump(&mut metrics, "mat0_entries", 1);
                            }
                            0x6D78_6554 => {
                                bump(&mut metrics, "texm_entries", 1);
                            }
                            0x4449_5846 => {
                                bump(&mut metrics, "fxid_entries", 1);
                            }
                            0x5241_4557 => {
                                bump(&mut metrics, "wear_entries", 1);
                            }
                            _ => {}
                        }
                    }
                }
            } else if bytes.starts_with(b"NL") {
                bump(&mut metrics, "rsli_files", 1);
            }
        }
    }

    CorpusReport {
        schema: 1,
        kind: manifest.kind,
        files: manifest.files.len(),
        bytes: manifest.files.iter().map(|f| f.size).sum(),
        metrics,
        casefold_collisions: manifest.casefold_collisions.len(),
        fingerprint: fingerprint(manifest),
    }
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

#[derive(Clone, Debug)]
struct NresEntryBrief {
    kind: u32,
    name: String,
}

fn inspect_nres_entries(bytes: &[u8]) -> Option<Vec<NresEntryBrief>> {
    if bytes.len() < 16 || !bytes.starts_with(b"NRes") {
        return None;
    }
    let count = i32::from_le_bytes(bytes.get(8..12)?.try_into().ok()?);
    if count < 0 {
        return None;
    }
    let count = usize::try_from(count).ok()?;
    let directory_len = count.checked_mul(64)?;
    let directory_offset = bytes.len().checked_sub(directory_len)?;
    let mut names = Vec::with_capacity(count);
    for index in 0..count {
        let base = directory_offset.checked_add(index.checked_mul(64)?)?;
        let kind = u32::from_le_bytes(bytes.get(base..base + 4)?.try_into().ok()?);
        let raw = bytes.get(base + 20..base + 56)?;
        let len = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
        names.push(NresEntryBrief {
            kind,
            name: String::from_utf8_lossy(&raw[..len]).to_string(),
        });
    }
    Some(names)
}

/// Computes stable manifest fingerprint.
#[must_use]
pub fn fingerprint(manifest: &CorpusManifest) -> u64 {
    let mut state = 0xcbf2_9ce4_8422_2325;
    for file in &manifest.files {
        hash_into(&mut state, file.path.as_bytes());
        hash_into(&mut state, &file.size.to_le_bytes());
        hash_into(&mut state, &file.hash.to_le_bytes());
    }
    state
}

fn stable_hash(bytes: &[u8]) -> u64 {
    let mut state = 0xcbf2_9ce4_8422_2325;
    hash_into(&mut state, bytes);
    state
}

fn hash_into(state: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *state ^= u64::from(*byte);
        *state = state.wrapping_mul(0x0000_0100_0000_01b3);
    }
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
        "{{\"schema_version\":\"fparkan-corpus-report-v1\",\"schema\":{},\"kind\":\"{:?}\",\"files\":{},\"bytes\":{},\"casefold_collisions\":{},\"fingerprint\":\"{:016x}\",\"metrics\":{{",
        report.schema,
        report.kind,
        report.files,
        report.bytes,
        report.casefold_collisions,
        report.fingerprint
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
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("testdata")
            .join("IS");
        if !root.is_dir() {
            return;
        }
        let manifest = discover(&root, DiscoverOptions::default()).expect("manifest");
        let report = report(&root, &manifest);
        assert!(report.files > 0);
        assert!(report.metrics["nres_files"] > 0);
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_part1_manifest_profile_and_counts_match_baseline() {
        let root = testdata_root("IS");
        let manifest = discover(&root, DiscoverOptions::default()).expect("part 1 manifest");
        let report = report(&root, &manifest);

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
        let report = report(&root, &manifest);

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
            files: vec![ManifestEntry {
                path: "secret/payload.bin".to_string(),
                size: 4,
                hash: stable_hash(b"DATA"),
            }],
            casefold_collisions: Vec::new(),
        };
        let report = report(Path::new("."), &manifest);
        let json = render_report_json(&report);

        assert!(json.contains("\"schema_version\":\"fparkan-corpus-report-v1\""));
        assert!(json.contains("\"fingerprint\":"));
        assert!(json.contains("\"metrics\":"));
        assert!(!json.contains("secret/payload.bin"));
        assert!(!json.contains("DATA"));
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
                ManifestEntry {
                    path: "Textures/Foo.TEX".to_string(),
                    size: 1,
                    hash: 1,
                },
                ManifestEntry {
                    path: "textures/foo.tex".to_string(),
                    size: 1,
                    hash: 2,
                },
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
            files: vec![ManifestEntry {
                path: "a".to_string(),
                size: 1,
                hash: 1,
            }],
            casefold_collisions: Vec::new(),
        };
        let a = fingerprint(&manifest);
        manifest.files[0].hash = 2;
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
            fingerprint: 0,
        };
        write_report_atomic(&tmp, &report).expect("write");
        assert!(tmp.is_file());
        let _ = fs::remove_file(tmp);
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
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("testdata")
            .join(part)
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
