#![forbid(unsafe_code)]
//! Virtual filesystem ports for resource loading.

use fparkan_path::{join_under, NormalizedPath};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// VFS metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VfsMetadata {
    /// Byte length.
    pub len: u64,
    /// Stable-enough source fingerprint for cache invalidation.
    pub fingerprint: u64,
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
        let exact = join_under(&self.root, path).map_err(|_| VfsError::Path)?;
        if exact.exists() {
            return Ok(exact);
        }
        resolve_casefolded(&self.root, path.as_str())
    }
}

impl Vfs for DirectoryVfs {
    fn metadata(&self, path: &NormalizedPath) -> Result<VfsMetadata, VfsError> {
        let meta = fs::metadata(self.host_path(path)?).map_err(VfsError::Io)?;
        Ok(metadata_from_fs(&meta))
    }

    fn read(&self, path: &NormalizedPath) -> Result<Arc<[u8]>, VfsError> {
        let bytes = fs::read(self.host_path(path)?).map_err(VfsError::Io)?;
        Ok(Arc::from(bytes.into_boxed_slice()))
    }

    fn list(&self, prefix: &NormalizedPath) -> Result<Vec<VfsEntry>, VfsError> {
        let base = self.host_path(prefix)?;
        let mut entries = Vec::new();
        if base.is_file() {
            let metadata = fs::metadata(&base).map_err(VfsError::Io)?;
            entries.push(VfsEntry {
                path: prefix.clone(),
                metadata: metadata_from_fs(&metadata),
            });
            return Ok(entries);
        }
        list_recursive(&self.root, &base, &mut entries)?;
        entries.sort_by(|a, b| a.path.as_str().cmp(b.path.as_str()));
        Ok(entries)
    }
}

fn resolve_casefolded(root: &Path, normalized: &str) -> Result<PathBuf, VfsError> {
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
    segment: &str,
    mut matches: Vec<PathBuf>,
) -> Result<PathBuf, VfsError> {
    matches.sort();
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

fn list_recursive(root: &Path, dir: &Path, out: &mut Vec<VfsEntry>) -> Result<(), VfsError> {
    let read_dir = fs::read_dir(dir).map_err(VfsError::Io)?;
    let mut children = Vec::new();
    for entry in read_dir {
        let entry = entry.map_err(VfsError::Io)?;
        children.push(entry.path());
    }
    children.sort();
    for child in children {
        let metadata = fs::metadata(&child).map_err(VfsError::Io)?;
        if metadata.is_dir() {
            list_recursive(root, &child, out)?;
            continue;
        }
        if !metadata.is_file() {
            continue;
        }
        let rel = child.strip_prefix(root).map_err(|_| VfsError::Path)?;
        let rel_text = rel.to_str().ok_or(VfsError::Path)?;
        let path = fparkan_path::normalize_relative(
            rel_text.as_bytes(),
            fparkan_path::PathPolicy::HostCompatible,
        )
        .map_err(|_| VfsError::Path)?;
        out.push(VfsEntry {
            path,
            metadata: metadata_from_fs(&metadata),
        });
    }
    Ok(())
}

fn metadata_from_fs(metadata: &fs::Metadata) -> VfsMetadata {
    let mut fingerprint = 0xcbf2_9ce4_8422_2325;
    hash_u64(&mut fingerprint, metadata.len());
    if let Ok(modified) = metadata.modified() {
        if let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH) {
            hash_u64(&mut fingerprint, duration.as_secs());
            hash_u64(&mut fingerprint, u64::from(duration.subsec_nanos()));
        }
    }
    VfsMetadata {
        len: metadata.len(),
        fingerprint,
    }
}

/// In-memory VFS.
#[derive(Clone, Debug, Default)]
pub struct MemoryVfs {
    files: BTreeMap<String, Arc<[u8]>>,
}

impl MemoryVfs {
    /// Inserts a file.
    #[allow(clippy::needless_pass_by_value)]
    pub fn insert(&mut self, path: NormalizedPath, bytes: Arc<[u8]>) {
        self.files.insert(path.as_str().to_string(), bytes);
    }
}

impl Vfs for MemoryVfs {
    fn metadata(&self, path: &NormalizedPath) -> Result<VfsMetadata, VfsError> {
        let bytes = self
            .files
            .get(path.as_str())
            .ok_or_else(|| VfsError::NotFound(path.as_str().to_string()))?;
        Ok(VfsMetadata {
            len: bytes.len() as u64,
            fingerprint: stable_hash(bytes),
        })
    }

    fn read(&self, path: &NormalizedPath) -> Result<Arc<[u8]>, VfsError> {
        self.files
            .get(path.as_str())
            .cloned()
            .ok_or_else(|| VfsError::NotFound(path.as_str().to_string()))
    }

    fn list(&self, prefix: &NormalizedPath) -> Result<Vec<VfsEntry>, VfsError> {
        let mut out = Vec::new();
        for (path, bytes) in &self.files {
            if path
                .as_bytes()
                .get(..prefix.as_str().len())
                .is_some_and(|head| head.eq_ignore_ascii_case(prefix.as_str().as_bytes()))
            {
                let normalized = fparkan_path::normalize_relative(
                    path.as_bytes(),
                    fparkan_path::PathPolicy::StrictLegacy,
                )
                .map_err(|_| VfsError::Path)?;
                out.push(VfsEntry {
                    path: normalized,
                    metadata: VfsMetadata {
                        len: bytes.len() as u64,
                        fingerprint: stable_hash(bytes),
                    },
                });
            }
        }
        Ok(out)
    }
}

fn stable_hash(bytes: &[u8]) -> u64 {
    let mut state = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        state ^= u64::from(*byte);
        state = state.wrapping_mul(0x0000_0100_0000_01b3);
    }
    state
}

fn hash_u64(state: &mut u64, value: u64) {
    for byte in value.to_le_bytes() {
        *state ^= u64::from(byte);
        *state = state.wrapping_mul(0x0000_0100_0000_01b3);
    }
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
        Err(VfsError::NotFound(path.as_str().to_string()))
    }

    fn read(&self, path: &NormalizedPath) -> Result<Arc<[u8]>, VfsError> {
        for layer in &self.layers {
            match layer.read(path) {
                Ok(bytes) => return Ok(bytes),
                Err(VfsError::NotFound(_)) => {}
                Err(err) => return Err(err),
            }
        }
        Err(VfsError::NotFound(path.as_str().to_string()))
    }

    fn list(&self, prefix: &NormalizedPath) -> Result<Vec<VfsEntry>, VfsError> {
        let mut by_key = BTreeMap::new();
        for layer in &self.layers {
            match layer.list(prefix) {
                Ok(entries) => {
                    for entry in entries {
                        let key = entry.path.as_str().to_ascii_uppercase();
                        by_key.entry(key).or_insert(entry);
                    }
                }
                Err(VfsError::NotFound(_)) => {}
                Err(err) => return Err(err),
            }
        }
        let mut entries: Vec<_> = by_key.into_values().collect();
        entries.sort_by(|a, b| a.path.as_str().cmp(b.path.as_str()));
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_path::{normalize_relative, PathPolicy};

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
    fn memory_vfs_uses_exact_lookup() {
        let path = normalize_relative(b"Data/File.bin", PathPolicy::StrictLegacy).expect("path");
        let mut vfs = MemoryVfs::default();
        vfs.insert(path.clone(), Arc::from(b"payload".as_slice()));

        assert_eq!(vfs.metadata(&path).expect("metadata").len, 7);
        assert_eq!(vfs.read(&path).expect("read").as_ref(), b"payload");

        let other_case =
            normalize_relative(b"data/file.bin", PathPolicy::StrictLegacy).expect("path");
        assert!(matches!(vfs.read(&other_case), Err(VfsError::NotFound(_))));
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

    fn unique_test_dir(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("fparkan-vfs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        path
    }
}
