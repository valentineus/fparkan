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
//! Virtual filesystem ports for resource loading.

use fparkan_binary::{sha256, Sha256Digest};
use fparkan_path::{ascii_lookup_key, join_under, NormalizedPath};
use std::collections::BTreeMap;
use std::fs;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(windows)]
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// VFS metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VfsMetadata {
    /// Byte length.
    pub len: u64,
    /// SHA-256 content fingerprint for cache invalidation.
    pub fingerprint: Sha256Digest,
}

/// VFS entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VfsEntry {
    /// Path.
    pub path: NormalizedPath,
    /// Metadata.
    pub metadata: VfsMetadata,
}

/// VFS error.
#[derive(Debug)]
pub enum VfsError {
    /// Missing entry.
    NotFound(String),
    /// Ambiguous host path.
    Ambiguous(String),
    /// I/O error.
    Io(std::io::Error),
    /// Invalid path.
    Path,
}

impl std::fmt::Display for VfsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(path) => write!(f, "not found: {path}"),
            Self::Ambiguous(path) => write!(f, "ambiguous host path: {path}"),
            Self::Io(err) => write!(f, "{err}"),
            Self::Path => write!(f, "invalid path"),
        }
    }
}

impl std::error::Error for VfsError {}

/// Resource VFS.
pub trait Vfs: Send + Sync {
    /// Reads metadata.
    ///
    /// # Errors
    ///
    /// Returns [`VfsError`] when the path is invalid, missing, or cannot be
    /// inspected by the backing store.
    fn metadata(&self, path: &NormalizedPath) -> Result<VfsMetadata, VfsError>;
    /// Reads bytes.
    ///
    /// # Errors
    ///
    /// Returns [`VfsError`] when the path is invalid, missing, or cannot be
    /// read by the backing store.
    fn read(&self, path: &NormalizedPath) -> Result<Arc<[u8]>, VfsError>;
    /// Lists entries below prefix.
    ///
    /// # Errors
    ///
    /// Returns [`VfsError`] when the prefix is invalid, missing, or cannot be
    /// traversed by the backing store.
    fn list(&self, prefix: &NormalizedPath) -> Result<Vec<VfsEntry>, VfsError>;
}

/// Host directory VFS.
#[derive(Clone, Debug)]
pub struct DirectoryVfs {
    root: PathBuf,
}

impl DirectoryVfs {
    /// Creates a directory VFS.
    #[must_use]
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    fn host_path(&self, path: &NormalizedPath) -> Result<PathBuf, VfsError> {
        join_under(&self.root, path).map_err(|_| VfsError::Path)?;
        resolve_casefolded(&self.root, path)
    }

    fn metadata_from_host_file(&self, path: &Path) -> Result<VfsMetadata, VfsError> {
        let metadata = fs::symlink_metadata(path).map_err(VfsError::Io)?;
        metadata_from_host_file(path, &metadata)
    }
}

impl Vfs for DirectoryVfs {
    fn metadata(&self, path: &NormalizedPath) -> Result<VfsMetadata, VfsError> {
        self.metadata_from_host_file(&self.host_path(path)?)
    }

    fn read(&self, path: &NormalizedPath) -> Result<Arc<[u8]>, VfsError> {
        let host = self.host_path(path)?;
        let pre_metadata = fs::symlink_metadata(&host).map_err(VfsError::Io)?;
        if pre_metadata.file_type().is_symlink() || !pre_metadata.is_file() {
            return Err(VfsError::Path);
        }
        let pre_identity = file_identity(&pre_metadata);
        let pre_len = pre_metadata.len();
        let pre_modified = pre_metadata.modified().ok();
        let bytes = fs::read(&host).map_err(VfsError::Io)?;
        let post_metadata = fs::symlink_metadata(&host).map_err(VfsError::Io)?;
        if post_metadata.file_type().is_symlink()
            || !post_metadata.is_file()
            || post_metadata.len() != pre_len
            || post_metadata.modified().ok() != pre_modified
            || file_identity(&post_metadata) != pre_identity
        {
            return Err(VfsError::Path);
        }
        Ok(Arc::from(bytes.into_boxed_slice()))
    }

    fn list(&self, prefix: &NormalizedPath) -> Result<Vec<VfsEntry>, VfsError> {
        let base = self.host_path(prefix)?;
        let mut entries = Vec::new();
        if base.is_file() {
            let metadata = fs::symlink_metadata(&base).map_err(VfsError::Io)?;
            entries.push(VfsEntry {
                path: prefix.clone(),
                metadata: metadata_from_host_file(&base, &metadata)?,
            });
            return Ok(entries);
        }
        list_recursive(&self.root, &base, &mut entries)?;
        entries.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
        Ok(entries)
    }
}

fn resolve_casefolded(root: &Path, normalized: &NormalizedPath) -> Result<PathBuf, VfsError> {
    #[cfg(unix)]
    {
        return resolve_casefolded_unix(root, normalized);
    }

    #[cfg(not(unix))]
    {
        resolve_casefolded_text(root, normalized.display_lossy())
    }
}

#[cfg(unix)]
fn resolve_casefolded_unix(root: &Path, normalized: &NormalizedPath) -> Result<PathBuf, VfsError> {
    let mut current = root.to_path_buf();
    for segment in normalized.as_bytes().split(|byte| *byte == b'/') {
        current = resolve_casefolded_segment(&current, segment, normalized)?;
    }
    Ok(current)
}

#[cfg(unix)]
fn resolve_casefolded_segment(
    dir: &Path,
    segment: &[u8],
    normalized: &NormalizedPath,
) -> Result<PathBuf, VfsError> {
    let read_dir = fs::read_dir(dir).map_err(VfsError::Io)?;
    let mut matches = Vec::new();
    for entry in read_dir {
        let entry = entry.map_err(VfsError::Io)?;
        let name = entry.file_name();
        if name.as_bytes().eq_ignore_ascii_case(segment) {
            if entry.file_type().map_err(VfsError::Io)?.is_symlink() {
                return Err(VfsError::Path);
            }
            matches.push(entry.path());
        }
    }
    select_casefolded_match(normalized.display_lossy(), dir, segment, matches)
}

#[cfg(not(unix))]
fn resolve_casefolded_text(root: &Path, normalized: &str) -> Result<PathBuf, VfsError> {
    let mut current = root.to_path_buf();
    for segment in normalized.split('/') {
        let read_dir = fs::read_dir(&current).map_err(VfsError::Io)?;
        let mut matches = Vec::new();
        for entry in read_dir {
            let entry = entry.map_err(VfsError::Io)?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if name.eq_ignore_ascii_case(segment) {
                if entry.file_type().map_err(VfsError::Io)?.is_symlink() {
                    return Err(VfsError::Path);
                }
                matches.push(entry.path());
            }
        }
        current = select_casefolded_match(normalized, &current, segment, matches)?;
    }
    Ok(current)
}

fn select_casefolded_match(
    normalized: &str,
    current: &Path,
    segment: impl AsRef<[u8]>,
    mut matches: Vec<PathBuf>,
) -> Result<PathBuf, VfsError> {
    matches.sort();
    let segment = String::from_utf8_lossy(segment.as_ref());
    match matches.len() {
        0 => Err(VfsError::NotFound(normalized.to_string())),
        1 => Ok(matches.remove(0)),
        _ => Err(VfsError::Ambiguous(format!(
            "{}/{}",
            current.display(),
            segment
        ))),
    }
}

fn list_recursive(
    root: &Path,
    dir: &Path,
    out: &mut Vec<VfsEntry>,
) -> Result<(), VfsError> {
    let read_dir = fs::read_dir(dir).map_err(VfsError::Io)?;
    let mut children = Vec::new();
    for entry in read_dir {
        let entry = entry.map_err(VfsError::Io)?;
        children.push(entry.path());
    }
    children.sort();
    for child in children {
        let metadata = fs::symlink_metadata(&child).map_err(VfsError::Io)?;
        if metadata.file_type().is_symlink() {
            return Err(VfsError::Path);
        }
        if metadata.is_dir() {
            list_recursive(root, &child, out)?;
            continue;
        }
        if !metadata.is_file() {
            continue;
        }
        let rel = child.strip_prefix(root).map_err(|_| VfsError::Path)?;
        #[cfg(unix)]
        let rel_bytes = rel.as_os_str().as_bytes();
        #[cfg(not(unix))]
        let rel_bytes = rel.to_str().ok_or(VfsError::Path)?.as_bytes();
        let path = fparkan_path::normalize_relative(
            rel_bytes,
            fparkan_path::PathPolicy::HostCompatible,
        )
        .map_err(|_| VfsError::Path)?;
        out.push(VfsEntry {
            path,
            metadata: metadata_from_host_file(&child, &metadata)?,
        });
    }
    Ok(())
}

fn metadata_from_host_file(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<VfsMetadata, VfsError> {
    if !metadata.is_file() {
        return Err(VfsError::Path);
    }
    let len = metadata.len();
    let bytes = fs::read(path).map_err(VfsError::Io)?;
    let fingerprint = sha256(&bytes);
    Ok(VfsMetadata { len, fingerprint })
}

/// In-memory VFS.
#[derive(Clone, Debug, Default)]
pub struct MemoryVfs {
    files: BTreeMap<Vec<u8>, Arc<[u8]>>,
    lookup: BTreeMap<Vec<u8>, Vec<Vec<u8>>>,
}

impl MemoryVfs {
    /// Inserts a file.
    #[allow(clippy::needless_pass_by_value)]
    pub fn insert(&mut self, path: NormalizedPath, bytes: Arc<[u8]>) {
        let path = path.as_bytes().to_vec();
        self.files.insert(path, bytes);
        self.rebuild_lookup();
    }

    fn rebuild_lookup(&mut self) {
        self.lookup.clear();
        for path in self.files.keys() {
            self.lookup
                .entry(ascii_lookup_key(path).0)
                .or_default()
                .push(path.clone());
        }
        for paths in self.lookup.values_mut() {
            paths.sort();
        }
    }

    fn resolve_path(&self, path: &NormalizedPath) -> Result<&[u8], VfsError> {
        let key = ascii_lookup_key(path.as_bytes()).0;
        let matches = self
            .lookup
            .get(&key)
            .ok_or_else(|| VfsError::NotFound(path.display_lossy().to_string()))?;
        match matches.as_slice() {
            [single] => Ok(single.as_slice()),
            [] => Err(VfsError::NotFound(path.display_lossy().to_string())),
            _ => Err(VfsError::Ambiguous(path.display_lossy().to_string())),
        }
    }
}

#[cfg(unix)]
#[allow(clippy::unnecessary_wraps)]
fn file_identity(metadata: &fs::Metadata) -> Option<u64> {
    Some(metadata.dev().rotate_left(32) ^ metadata.ino())
}

#[cfg(windows)]
#[allow(clippy::unnecessary_wraps)]
fn file_identity(metadata: &fs::Metadata) -> Option<u64> {
    Some(
        (metadata.volume_serial_number() as u64).rotate_left(40)
            ^ ((metadata.file_index_high() as u64) << 32)
            ^ metadata.file_index_low() as u64,
    )
}

#[cfg(not(any(unix, windows)))]
fn file_identity(_metadata: &fs::Metadata) -> Option<u64> {
    None
}

impl Vfs for MemoryVfs {
    fn metadata(&self, path: &NormalizedPath) -> Result<VfsMetadata, VfsError> {
        let resolved = self.resolve_path(path)?;
        let bytes = self
            .files
            .get(resolved)
            .ok_or_else(|| VfsError::NotFound(path.display_lossy().to_string()))?;
        Ok(VfsMetadata {
            len: bytes.len() as u64,
            fingerprint: sha256(bytes),
        })
    }

    fn read(&self, path: &NormalizedPath) -> Result<Arc<[u8]>, VfsError> {
        let resolved = self.resolve_path(path)?;
        self.files
            .get(resolved)
            .cloned()
            .ok_or_else(|| VfsError::NotFound(path.display_lossy().to_string()))
    }

    fn list(&self, prefix: &NormalizedPath) -> Result<Vec<VfsEntry>, VfsError> {
        let mut out = Vec::new();
        for (path, bytes) in &self.files {
            if has_segment_boundary_prefix_bytes(path, prefix.as_bytes()) {
                let normalized =
                    fparkan_path::normalize_relative(path, fparkan_path::PathPolicy::StrictLegacy)
                        .map_err(|_| VfsError::Path)?;
                out.push(VfsEntry {
                    path: normalized,
                    metadata: VfsMetadata {
                        len: bytes.len() as u64,
                        fingerprint: sha256(bytes),
                    },
                });
            }
        }
        Ok(out)
    }
}

fn has_segment_boundary_prefix_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    if haystack.len() < needle.len() {
        return false;
    }
    if haystack.len() == needle.len() {
        return haystack
            .iter()
            .zip(needle.iter())
            .all(|(left, right)| left.eq_ignore_ascii_case(right));
    }
    if haystack[needle.len()] != b'/' {
        return false;
    }
    haystack[..needle.len()]
        .iter()
        .zip(needle.iter())
        .all(|(left, right)| left.eq_ignore_ascii_case(right))
}

/// Layered VFS with deterministic first-layer precedence.
#[derive(Clone, Default)]
pub struct OverlayVfs {
    layers: Vec<Arc<dyn Vfs>>,
}

impl std::fmt::Debug for OverlayVfs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverlayVfs")
            .field("layers", &self.layers.len())
            .finish()
    }
}

impl OverlayVfs {
    /// Creates an empty overlay.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an overlay from ordered layers.
    #[must_use]
    pub fn from_layers(layers: Vec<Arc<dyn Vfs>>) -> Self {
        Self { layers }
    }

    /// Appends a lower-priority layer.
    pub fn push_layer(&mut self, layer: Arc<dyn Vfs>) {
        self.layers.push(layer);
    }
}

impl Vfs for OverlayVfs {
    fn metadata(&self, path: &NormalizedPath) -> Result<VfsMetadata, VfsError> {
        for layer in &self.layers {
            match layer.metadata(path) {
                Ok(metadata) => return Ok(metadata),
                Err(VfsError::NotFound(_)) => {}
                Err(err) => return Err(err),
            }
        }
        Err(VfsError::NotFound(path.display_lossy().to_string()))
    }

    fn read(&self, path: &NormalizedPath) -> Result<Arc<[u8]>, VfsError> {
        for layer in &self.layers {
            match layer.read(path) {
                Ok(bytes) => return Ok(bytes),
                Err(VfsError::NotFound(_)) => {}
                Err(err) => return Err(err),
            }
        }
        Err(VfsError::NotFound(path.display_lossy().to_string()))
    }

    fn list(&self, prefix: &NormalizedPath) -> Result<Vec<VfsEntry>, VfsError> {
        let mut by_key = BTreeMap::new();
        for layer in &self.layers {
            match layer.list(prefix) {
                Ok(entries) => {
                    for entry in entries {
                        let key = ascii_lookup_key(entry.path.as_bytes()).0;
                        by_key.entry(key).or_insert(entry);
                    }
                }
                Err(VfsError::NotFound(_)) => {}
                Err(err) => return Err(err),
            }
        }
        let mut entries: Vec<_> = by_key.into_values().collect();
        entries.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_path::{normalize_relative, PathPolicy};
    #[cfg(unix)]
    use std::ffi::OsString;
    #[cfg(unix)]
    use std::os::unix::ffi::OsStringExt;

    #[test]
    fn directory_vfs_resolves_ascii_casefolded_segments() {
        let root = unique_test_dir("casefold");
        let dir = root.join("data").join("MAPS").join("Tut_1");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("Land.msh"), b"mesh").expect("write");

        let vfs = DirectoryVfs::new(&root);
        let path = normalize_relative(b"DATA/maps/tut_1/land.MSH", PathPolicy::StrictLegacy)
            .expect("path");
        assert_eq!(vfs.read(&path).expect("read").as_ref(), b"mesh");

        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn directory_vfs_reports_casefold_ambiguity_even_for_exact_host_path() {
        let root = unique_test_dir("casefold-ambiguous");
        std::fs::create_dir_all(root.join("Data")).expect("mkdir first");
        std::fs::create_dir_all(root.join("data")).expect("mkdir second");
        std::fs::write(root.join("Data").join("File.bin"), b"first").expect("write first");
        std::fs::write(root.join("data").join("File.bin"), b"second").expect("write second");
        let collision_count = std::fs::read_dir(&root)
            .expect("read root")
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.eq_ignore_ascii_case("data"))
            })
            .count();
        if collision_count < 2 {
            std::fs::remove_dir_all(root).expect("cleanup");
            return;
        }

        let vfs = DirectoryVfs::new(&root);
        let path = normalize_relative(b"Data/File.bin", PathPolicy::StrictLegacy).expect("path");

        assert!(matches!(vfs.read(&path), Err(VfsError::Ambiguous(_))));

        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn directory_vfs_lists_files_below_prefix() {
        let root = unique_test_dir("list");
        std::fs::create_dir_all(root.join("DATA").join("MAPS")).expect("mkdir");
        std::fs::write(root.join("DATA").join("MAPS").join("Land.map"), b"map").expect("write");
        std::fs::write(root.join("BuildDat.lst"), b"build").expect("write");

        let vfs = DirectoryVfs::new(&root);
        let prefix = normalize_relative(b"data", PathPolicy::StrictLegacy).expect("prefix");
        let entries = vfs.list(&prefix).expect("list");
        assert_eq!(entries.len(), 1);
        assert!(entries[0]
            .path
            .as_str()
            .eq_ignore_ascii_case("DATA/MAPS/Land.map"));

        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn memory_vfs_list_prefix_is_boundary_safe() {
        let mut vfs = MemoryVfs::default();
        let exact = normalize_relative(b"DATA/Land.map", PathPolicy::StrictLegacy).expect("path");
        let sibling =
            normalize_relative(b"DATA2/Land.map", PathPolicy::StrictLegacy).expect("path");
        vfs.insert(exact.clone(), Arc::from(b"exact".as_slice()));
        vfs.insert(sibling, Arc::from(b"sibling".as_slice()));

        let prefix = normalize_relative(b"DATA", PathPolicy::StrictLegacy).expect("prefix");
        let entries = vfs.list(&prefix).expect("list");

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path.as_str(), exact.as_str());
    }

    #[test]
    fn directory_vfs_fingerprint_changes_for_same_length_content() {
        let root = unique_test_dir("content-fingerprint");
        std::fs::create_dir_all(root.join("DATA")).expect("mkdir");
        std::fs::write(root.join("DATA").join("File.bin"), b"before").expect("write before");

        let vfs = DirectoryVfs::new(&root);
        let path = normalize_relative(b"DATA/File.bin", PathPolicy::StrictLegacy).expect("path");
        let before = vfs.metadata(&path).expect("before metadata");
        std::fs::write(root.join("DATA").join("File.bin"), b"after!").expect("write after");
        let after = vfs.metadata(&path).expect("after metadata");

        assert_eq!(before.len, after.len);
        assert_ne!(before.fingerprint, after.fingerprint);

        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn directory_vfs_rejects_symlink_escape() {
        let root = unique_test_dir("symlink-escape");
        let outside = unique_test_dir("symlink-outside");
        std::fs::create_dir_all(&root).expect("mkdir root");
        std::fs::create_dir_all(&outside).expect("mkdir outside");
        std::fs::write(outside.join("secret.bin"), b"secret").expect("write outside");
        std::os::unix::fs::symlink(&outside, root.join("DATA")).expect("symlink");

        let vfs = DirectoryVfs::new(&root);
        let path = normalize_relative(b"DATA/secret.bin", PathPolicy::StrictLegacy).expect("path");
        let prefix = normalize_relative(b"DATA", PathPolicy::StrictLegacy).expect("prefix");

        assert!(matches!(vfs.read(&path), Err(VfsError::Path)));
        assert!(matches!(vfs.list(&prefix), Err(VfsError::Path)));

        std::fs::remove_dir_all(root).expect("cleanup root");
        std::fs::remove_dir_all(outside).expect("cleanup outside");
    }

    #[cfg(unix)]
    #[test]
    fn directory_vfs_resolves_non_utf8_host_entries_by_raw_bytes() {
        let root = unique_test_dir("non-utf8");
        let data_dir = root.join("DATA");
        std::fs::create_dir_all(&data_dir).expect("mkdir");
        let file_name = OsString::from_vec(vec![0xFF, b'.', b'b', b'i', b'n']);
        let raw_path = data_dir.join(&file_name);
        if let Err(err) = std::fs::write(&raw_path, b"raw") {
            assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
            std::fs::remove_dir_all(root).expect("cleanup");
            return;
        }

        let vfs = DirectoryVfs::new(&root);
        let path =
            normalize_relative(b"data/\xFF.bin", PathPolicy::HostCompatible).expect("path");

        assert_eq!(vfs.read(&path).expect("read raw path").as_ref(), b"raw");
        let entries = vfs
            .list(&normalize_relative(b"DATA", PathPolicy::StrictLegacy).expect("prefix"))
            .expect("list");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path.identity_bytes(), b"DATA/\xFF.bin");

        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn casefold_selector_reports_ambiguous_segments() {
        let err = select_casefolded_match(
            "data/file.bin",
            Path::new("/game"),
            "data",
            vec![PathBuf::from("/game/Data"), PathBuf::from("/game/DATA")],
        )
        .expect_err("ambiguous path");

        assert!(matches!(err, VfsError::Ambiguous(_)));
    }

    #[test]
    fn memory_vfs_uses_ascii_casefold_lookup() {
        let path = normalize_relative(b"Data/File.bin", PathPolicy::StrictLegacy).expect("path");
        let mut vfs = MemoryVfs::default();
        vfs.insert(path.clone(), Arc::from(b"payload".as_slice()));

        assert_eq!(vfs.metadata(&path).expect("metadata").len, 7);
        assert_eq!(vfs.read(&path).expect("read").as_ref(), b"payload");

        let other_case =
            normalize_relative(b"data/file.bin", PathPolicy::StrictLegacy).expect("path");
        assert_eq!(
            vfs.read(&other_case).expect("casefold read").as_ref(),
            b"payload"
        );
    }

    #[test]
    fn memory_vfs_reports_casefold_ambiguity() {
        let first = normalize_relative(b"Data/File.bin", PathPolicy::StrictLegacy).expect("first");
        let second =
            normalize_relative(b"DATA/file.BIN", PathPolicy::StrictLegacy).expect("second");
        let query = normalize_relative(b"data/file.bin", PathPolicy::StrictLegacy).expect("query");
        let mut vfs = MemoryVfs::default();
        vfs.insert(first, Arc::from(b"first".as_slice()));
        vfs.insert(second, Arc::from(b"second".as_slice()));

        assert!(matches!(vfs.read(&query), Err(VfsError::Ambiguous(_))));
    }

    #[test]
    fn memory_vfs_distinguishes_non_utf8_path_bytes() {
        let mut vfs = MemoryVfs::default();
        let ascii =
            normalize_relative(b"DATA/normal.bin", PathPolicy::HostCompatible).expect("ascii path");
        let binary =
            normalize_relative(b"DATA/\xFF.bin", PathPolicy::HostCompatible).expect("binary path");
        vfs.insert(ascii.clone(), Arc::from(b"ascii".as_slice()));
        vfs.insert(binary.clone(), Arc::from(b"binary".as_slice()));

        let binary_query =
            normalize_relative(b"DATA/\xFF.bin", PathPolicy::HostCompatible).expect("binary query");

        assert_eq!(
            vfs.read(&binary_query).expect("read binary").as_ref(),
            b"binary"
        );
        assert_eq!(vfs.read(&ascii).expect("read ascii").as_ref(), b"ascii");
    }

    #[test]
    fn overlay_vfs_uses_first_matching_layer() {
        let path = normalize_relative(b"DATA/File.bin", PathPolicy::StrictLegacy).expect("path");
        let prefix = normalize_relative(b"DATA", PathPolicy::StrictLegacy).expect("prefix");
        let mut high = MemoryVfs::default();
        let mut low = MemoryVfs::default();
        high.insert(path.clone(), Arc::from(b"high".as_slice()));
        low.insert(path.clone(), Arc::from(b"low".as_slice()));

        let overlay = OverlayVfs::from_layers(vec![Arc::new(high), Arc::new(low)]);

        assert_eq!(overlay.read(&path).expect("read").as_ref(), b"high");
        let entries = overlay.list(&prefix).expect("list");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].metadata.len, 4);
    }

    #[test]
    fn overlay_vfs_keeps_lossy_equivalent_entries_distinct() {
        let prefix = normalize_relative(b"DATA", PathPolicy::StrictLegacy).expect("prefix");
        let mut high = MemoryVfs::default();
        let mut low = MemoryVfs::default();
        high.insert(
            normalize_relative(b"DATA/\xFF.bin", PathPolicy::HostCompatible).expect("high path"),
            Arc::from(b"high".as_slice()),
        );
        low.insert(
            normalize_relative(b"DATA/\xFE.bin", PathPolicy::HostCompatible).expect("low path"),
            Arc::from(b"low".as_slice()),
        );

        let overlay = OverlayVfs::from_layers(vec![Arc::new(high), Arc::new(low)]);
        let entries = overlay.list(&prefix).expect("list");

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path.display_lossy(), entries[1].path.display_lossy());
        assert_ne!(entries[0].path.identity_bytes(), entries[1].path.identity_bytes());
    }

    fn unique_test_dir(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("fparkan-vfs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        path
    }
}
