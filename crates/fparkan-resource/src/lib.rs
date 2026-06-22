#![forbid(unsafe_code)]
//! Resource identity and repository ports.

use fparkan_binary::Sha256Digest;
use fparkan_path::{normalize_relative, NormalizedPath, PathPolicy, ResourceName};
use fparkan_vfs::{Vfs, VfsError};
use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::{Arc, Mutex};

/// Resource key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceKey {
    /// Archive path.
    pub archive: NormalizedPath,
    /// Entry name.
    pub name: ResourceName,
    /// Optional type id.
    pub type_id: Option<u32>,
}

/// Resource entry metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceEntryInfo {
    /// Stable resource key.
    pub key: ResourceKey,
    /// Archive entry attribute 1.
    pub attr1: u32,
    /// Archive entry attribute 2.
    pub attr2: u32,
    /// Archive entry attribute 3.
    pub attr3: u32,
}

/// Archive identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ArchiveId(pub u64);

/// Entry handle.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EntryHandle {
    /// Archive.
    pub archive: ArchiveId,
    /// Archive generation at the time the entry was resolved.
    pub generation: u64,
    /// Local entry index.
    pub local: u32,
}

/// Archive kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArchiveKind {
    /// `NRes` archive.
    Nres,
    /// `RsLi` archive.
    Rsli,
}

/// Resource bytes.
#[derive(Clone, Debug)]
pub enum ResourceBytes {
    /// Shared byte owner.
    Shared(Arc<[u8]>),
    /// Slice in owner.
    Slice {
        /// Shared owner bytes.
        owner: Arc<[u8]>,
        /// Slice range.
        range: Range<usize>,
    },
}

impl ResourceBytes {
    /// Returns a byte slice.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        match self {
            Self::Shared(bytes) => bytes,
            Self::Slice { owner, range } => &owner[range.clone()],
        }
    }

    /// Returns byte length.
    #[must_use]
    pub fn len(&self) -> usize {
        self.as_slice().len()
    }

    /// Returns whether the resource is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns owned bytes.
    #[must_use]
    pub fn into_owned(self) -> Vec<u8> {
        match self {
            Self::Shared(bytes) => bytes.to_vec(),
            Self::Slice { owner, range } => owner[range].to_vec(),
        }
    }
}

/// Resource error.
#[derive(Debug)]
pub enum ResourceError {
    /// Missing archive.
    MissingArchive,
    /// Missing entry.
    MissingEntry,
    /// Stale or invalid handle.
    InvalidHandle,
    /// Handle belongs to an older archive generation.
    StaleHandle,
    /// Format error.
    Format(String),
    /// Entry-specific read error.
    EntryRead {
        /// Resource key.
        key: ResourceKey,
        /// Source error text.
        source: String,
    },
    /// Repository state lock was poisoned.
    Poisoned,
}

impl std::fmt::Display for ResourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingArchive => write!(f, "archive was not found"),
            Self::MissingEntry => write!(f, "resource entry was not found in the archive"),
            Self::InvalidHandle => write!(
                f,
                "resource handle does not reference an open archive entry"
            ),
            Self::StaleHandle => {
                write!(f, "resource handle belongs to an older archive generation")
            }
            Self::Format(message) => write!(f, "resource archive format error: {message}"),
            Self::EntryRead { key, source } => {
                write!(
                    f,
                    "failed to read resource {}:{} from {}: {}",
                    key.type_id
                        .map_or_else(|| "-".to_string(), |type_id| type_id.to_string()),
                    String::from_utf8_lossy(&key.name.0),
                    key.archive.as_str(),
                    source
                )
            }
            Self::Poisoned => write!(f, "resource repository state lock was poisoned"),
        }
    }
}

impl std::error::Error for ResourceError {}

/// Repository port.
pub trait ResourceRepository {
    /// Opens archive.
    ///
    /// # Errors
    ///
    /// Returns [`ResourceError`] when the archive is missing, unsupported, or
    /// malformed.
    fn open_archive(&self, path: &NormalizedPath) -> Result<ArchiveId, ResourceError>;
    /// Finds entry.
    ///
    /// # Errors
    ///
    /// Returns [`ResourceError`] when `archive` is not a valid opened archive.
    fn find(
        &self,
        archive: ArchiveId,
        name: &ResourceName,
    ) -> Result<Option<EntryHandle>, ResourceError>;
    /// Returns the first entry in archive directory order.
    ///
    /// # Errors
    ///
    /// Returns [`ResourceError`] when `archive` is not a valid opened archive.
    fn first_entry(&self, archive: ArchiveId) -> Result<Option<EntryHandle>, ResourceError>;
    /// Reads bytes.
    ///
    /// # Errors
    ///
    /// Returns [`ResourceError`] when `entry` is stale, invalid, or cannot be
    /// decoded.
    fn read(&self, entry: EntryHandle) -> Result<ResourceBytes, ResourceError>;
    /// Reads entry metadata.
    ///
    /// # Errors
    ///
    /// Returns [`ResourceError`] when `entry` is stale or invalid.
    fn entry_info(&self, entry: EntryHandle) -> Result<ResourceEntryInfo, ResourceError>;
}

/// Cached archive repository over a [`Vfs`].
pub struct CachedResourceRepository {
    vfs: Arc<dyn Vfs>,
    state: Mutex<RepositoryState>,
}

/// Decoded payload cache limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PayloadCacheLimits {
    /// Maximum cached decoded payload entries.
    pub max_entries: usize,
    /// Maximum cached decoded payload bytes.
    pub max_bytes: usize,
}

impl Default for PayloadCacheLimits {
    fn default() -> Self {
        Self {
            max_entries: 64,
            max_bytes: 64 * 1024 * 1024,
        }
    }
}

#[derive(Default)]
struct RepositoryState {
    paths: BTreeMap<String, ArchiveId>,
    archives: Vec<ArchiveSlot>,
    payload_cache: DecodedPayloadCache,
}

struct ArchiveSlot {
    path: NormalizedPath,
    fingerprint: Sha256Digest,
    generation: u64,
    kind: ArchiveKind,
    document: Arc<ArchiveDocument>,
}

enum ArchiveDocument {
    Nres(fparkan_nres::NresDocument),
    Rsli(fparkan_rsli::RsliDocument),
}

struct PayloadDecodeTask {
    document: Arc<ArchiveDocument>,
    key: ResourceKey,
}

#[derive(Debug, Default)]
struct DecodedPayloadCache {
    max_entries: usize,
    max_bytes: usize,
    current_bytes: usize,
    generation: u64,
    entries: BTreeMap<EntryHandle, PayloadCacheEntry>,
}

#[derive(Clone, Debug)]
struct PayloadCacheEntry {
    bytes: Arc<[u8]>,
    last_access: u64,
}

impl CachedResourceRepository {
    /// Creates a cached repository.
    #[must_use]
    pub fn new(vfs: Arc<dyn Vfs>) -> Self {
        Self::with_payload_cache_limits(vfs, PayloadCacheLimits::default())
    }

    /// Creates a cached repository with a decoded payload entry budget.
    #[must_use]
    pub fn with_payload_cache_budget(vfs: Arc<dyn Vfs>, max_payload_entries: usize) -> Self {
        Self::with_payload_cache_limits(
            vfs,
            PayloadCacheLimits {
                max_entries: max_payload_entries,
                ..PayloadCacheLimits::default()
            },
        )
    }

    /// Creates a cached repository with decoded payload entry and byte budgets.
    #[must_use]
    pub fn with_payload_cache_limits(vfs: Arc<dyn Vfs>, limits: PayloadCacheLimits) -> Self {
        Self {
            vfs,
            state: Mutex::new(RepositoryState {
                payload_cache: DecodedPayloadCache::new(limits),
                ..RepositoryState::default()
            }),
        }
    }

    /// Returns the archive kind for an opened archive.
    ///
    /// # Errors
    ///
    /// Returns [`ResourceError::InvalidHandle`] when `archive` is not present.
    pub fn archive_kind(&self, archive: ArchiveId) -> Result<ArchiveKind, ResourceError> {
        let state = self.state.lock().map_err(|_| ResourceError::Poisoned)?;
        Ok(state.archive(archive)?.kind)
    }

    /// Returns the archive path for an opened archive.
    ///
    /// # Errors
    ///
    /// Returns [`ResourceError::InvalidHandle`] when `archive` is not present.
    pub fn archive_path(&self, archive: ArchiveId) -> Result<NormalizedPath, ResourceError> {
        let state = self.state.lock().map_err(|_| ResourceError::Poisoned)?;
        Ok(state.archive(archive)?.path.clone())
    }
}

impl ResourceRepository for CachedResourceRepository {
    fn open_archive(&self, path: &NormalizedPath) -> Result<ArchiveId, ResourceError> {
        let metadata = self.vfs.metadata(path).map_err(resource_error_from_vfs)?;
        let fingerprint = metadata.fingerprint;
        if let Some(id) = self.cached_id(path, fingerprint)? {
            return Ok(id);
        }

        let bytes = self.vfs.read(path).map_err(resource_error_from_vfs)?;
        let mut slot = decode_archive(path.clone(), bytes, fingerprint)?;
        let mut state = self.state.lock().map_err(|_| ResourceError::Poisoned)?;
        if let Some(id) = state.paths.get(path.as_str()).copied() {
            if state.archive(id)?.fingerprint == fingerprint {
                return Ok(id);
            }
            slot.generation = state.archive(id)?.generation.saturating_add(1);
            *state.archive_mut(id)? = slot;
            state.payload_cache.remove_archive(id);
            return Ok(id);
        }
        let id = ArchiveId(u64::try_from(state.archives.len()).map_err(|_| {
            ResourceError::Format("too many open archives for handle space".to_string())
        })?);
        state.paths.insert(path.as_str().to_string(), id);
        state.archives.push(slot);
        Ok(id)
    }

    fn find(
        &self,
        archive: ArchiveId,
        name: &ResourceName,
    ) -> Result<Option<EntryHandle>, ResourceError> {
        let state = self.state.lock().map_err(|_| ResourceError::Poisoned)?;
        let slot = state.archive(archive)?;
        let local = match slot.document.as_ref() {
            ArchiveDocument::Nres(document) => document.find_bytes(&name.0).map(|id| id.0),
            ArchiveDocument::Rsli(document) => document.find_bytes(&name.0).map(|id| id.0),
        };
        Ok(local.map(|local| EntryHandle {
            archive,
            generation: slot.generation,
            local,
        }))
    }

    fn first_entry(&self, archive: ArchiveId) -> Result<Option<EntryHandle>, ResourceError> {
        let state = self.state.lock().map_err(|_| ResourceError::Poisoned)?;
        let slot = state.archive(archive)?;
        let local = match slot.document.as_ref() {
            ArchiveDocument::Nres(document) => document.entries().first().map(|entry| entry.id().0),
            ArchiveDocument::Rsli(document) => document.entry(fparkan_rsli::EntryId(0)).map(|_| 0),
        };
        Ok(local.map(|local| EntryHandle {
            archive,
            generation: slot.generation,
            local,
        }))
    }

    fn read(&self, entry: EntryHandle) -> Result<ResourceBytes, ResourceError> {
        let task = {
            let mut state = self.state.lock().map_err(|_| ResourceError::Poisoned)?;
            if let Some(bytes) = state.payload_cache.get(entry) {
                return Ok(ResourceBytes::Shared(bytes));
            }
            state.payload_decode_task(entry)?
        };
        let payload =
            task.document
                .read_payload(entry.local)
                .map_err(|source| ResourceError::EntryRead {
                    key: task.key,
                    source,
                })?;
        let shared = Arc::from(payload.into_boxed_slice());

        let mut state = self.state.lock().map_err(|_| ResourceError::Poisoned)?;
        if let Some(bytes) = state.payload_cache.get(entry) {
            return Ok(ResourceBytes::Shared(bytes));
        }
        state.entry_archive(entry)?;
        state.payload_cache.insert(entry, Arc::clone(&shared));
        Ok(ResourceBytes::Shared(shared))
    }

    fn entry_info(&self, entry: EntryHandle) -> Result<ResourceEntryInfo, ResourceError> {
        let state = self.state.lock().map_err(|_| ResourceError::Poisoned)?;
        let slot = state.entry_archive(entry)?;
        match slot.document.as_ref() {
            ArchiveDocument::Nres(document) => {
                let local =
                    usize::try_from(entry.local).map_err(|_| ResourceError::InvalidHandle)?;
                let entry = document
                    .entries()
                    .get(local)
                    .ok_or(ResourceError::InvalidHandle)?;
                let meta = entry.meta();
                Ok(ResourceEntryInfo {
                    key: ResourceKey {
                        archive: slot.path.clone(),
                        name: ResourceName(entry.name_bytes().to_vec()),
                        type_id: Some(meta.type_id),
                    },
                    attr1: meta.attr1,
                    attr2: meta.attr2,
                    attr3: meta.attr3,
                })
            }
            ArchiveDocument::Rsli(document) => {
                let meta = document
                    .entry(fparkan_rsli::EntryId(entry.local))
                    .ok_or(ResourceError::InvalidHandle)?;
                Ok(ResourceEntryInfo {
                    key: ResourceKey {
                        archive: slot.path.clone(),
                        name: ResourceName(meta.name_raw.to_vec()),
                        type_id: None,
                    },
                    attr1: u32::try_from(meta.flags).unwrap_or_default(),
                    attr2: 0,
                    attr3: 0,
                })
            }
        }
    }
}

impl CachedResourceRepository {
    fn cached_id(
        &self,
        path: &NormalizedPath,
        fingerprint: Sha256Digest,
    ) -> Result<Option<ArchiveId>, ResourceError> {
        let state = self.state.lock().map_err(|_| ResourceError::Poisoned)?;
        let Some(id) = state.paths.get(path.as_str()).copied() else {
            return Ok(None);
        };
        if state.archive(id)?.fingerprint == fingerprint {
            Ok(Some(id))
        } else {
            Ok(None)
        }
    }
}

impl DecodedPayloadCache {
    fn new(limits: PayloadCacheLimits) -> Self {
        Self {
            max_entries: limits.max_entries,
            max_bytes: limits.max_bytes,
            current_bytes: 0,
            generation: 0,
            entries: BTreeMap::new(),
        }
    }

    fn get(&mut self, handle: EntryHandle) -> Option<Arc<[u8]>> {
        let entry = self.entries.get_mut(&handle)?;
        self.generation = self.generation.saturating_add(1);
        entry.last_access = self.generation;
        Some(Arc::clone(&entry.bytes))
    }

    fn insert(&mut self, handle: EntryHandle, bytes: Arc<[u8]>) {
        let len = bytes.len();
        if self.max_entries == 0 || len > self.max_bytes {
            return;
        }
        self.generation = self.generation.saturating_add(1);
        if let Some(previous) = self.entries.insert(
            handle,
            PayloadCacheEntry {
                bytes,
                last_access: self.generation,
            },
        ) {
            self.current_bytes = self.current_bytes.saturating_sub(previous.bytes.len());
        }
        self.current_bytes = self.current_bytes.saturating_add(len);
        self.evict_until_within_budget();
    }

    fn remove_archive(&mut self, archive: ArchiveId) {
        let mut removed_bytes = 0usize;
        self.entries.retain(|handle, entry| {
            if handle.archive == archive {
                removed_bytes = removed_bytes.saturating_add(entry.bytes.len());
                false
            } else {
                true
            }
        });
        self.current_bytes = self.current_bytes.saturating_sub(removed_bytes);
    }

    fn evict_until_within_budget(&mut self) {
        while self.entries.len() > self.max_entries || self.current_bytes > self.max_bytes {
            let Some(victim) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_access)
                .map(|(handle, _)| *handle)
            else {
                break;
            };
            if let Some(removed) = self.entries.remove(&victim) {
                self.current_bytes = self.current_bytes.saturating_sub(removed.bytes.len());
            }
        }
    }
}

impl RepositoryState {
    fn archive(&self, id: ArchiveId) -> Result<&ArchiveSlot, ResourceError> {
        let index = usize::try_from(id.0).map_err(|_| ResourceError::InvalidHandle)?;
        self.archives.get(index).ok_or(ResourceError::InvalidHandle)
    }

    fn archive_mut(&mut self, id: ArchiveId) -> Result<&mut ArchiveSlot, ResourceError> {
        let index = usize::try_from(id.0).map_err(|_| ResourceError::InvalidHandle)?;
        self.archives
            .get_mut(index)
            .ok_or(ResourceError::InvalidHandle)
    }

    fn entry_archive(&self, entry: EntryHandle) -> Result<&ArchiveSlot, ResourceError> {
        let slot = self.archive(entry.archive)?;
        if slot.generation != entry.generation {
            return Err(ResourceError::StaleHandle);
        }
        Ok(slot)
    }

    fn payload_decode_task(&self, entry: EntryHandle) -> Result<PayloadDecodeTask, ResourceError> {
        let slot = self.entry_archive(entry)?;
        Ok(PayloadDecodeTask {
            document: Arc::clone(&slot.document),
            key: slot.entry_key(entry.local)?,
        })
    }
}

impl ArchiveSlot {
    fn entry_key(&self, local: u32) -> Result<ResourceKey, ResourceError> {
        match self.document.as_ref() {
            ArchiveDocument::Nres(document) => {
                let local = usize::try_from(local).map_err(|_| ResourceError::InvalidHandle)?;
                let entry = document
                    .entries()
                    .get(local)
                    .ok_or(ResourceError::InvalidHandle)?;
                Ok(ResourceKey {
                    archive: self.path.clone(),
                    name: ResourceName(entry.name_bytes().to_vec()),
                    type_id: Some(entry.meta().type_id),
                })
            }
            ArchiveDocument::Rsli(document) => {
                let meta = document
                    .entry(fparkan_rsli::EntryId(local))
                    .ok_or(ResourceError::InvalidHandle)?;
                Ok(ResourceKey {
                    archive: self.path.clone(),
                    name: ResourceName(c_name_bytes(&meta.name_raw).to_vec()),
                    type_id: None,
                })
            }
        }
    }
}

impl ArchiveDocument {
    fn read_payload(&self, local: u32) -> Result<Vec<u8>, String> {
        match self {
            ArchiveDocument::Nres(document) => document
                .payload(fparkan_nres::EntryId(local))
                .map(<[u8]>::to_vec)
                .map_err(|err| err.to_string()),
            ArchiveDocument::Rsli(document) => document
                .load(fparkan_rsli::EntryId(local))
                .map_err(|err| err.to_string()),
        }
    }
}

fn decode_archive(
    path: NormalizedPath,
    bytes: Arc<[u8]>,
    fingerprint: Sha256Digest,
) -> Result<ArchiveSlot, ResourceError> {
    if bytes.starts_with(b"NRes") {
        let document = fparkan_nres::decode(bytes, fparkan_nres::ReadProfile::Compatible)
            .map_err(|err| ResourceError::Format(err.to_string()))?;
        return Ok(ArchiveSlot {
            path,
            fingerprint,
            generation: 0,
            kind: ArchiveKind::Nres,
            document: Arc::new(ArchiveDocument::Nres(document)),
        });
    }
    if bytes.get(0..4) == Some(b"NL\0\x01") {
        let document = fparkan_rsli::decode(bytes, fparkan_rsli::ReadProfile::Compatible)
            .map_err(|err| ResourceError::Format(err.to_string()))?;
        return Ok(ArchiveSlot {
            path,
            fingerprint,
            generation: 0,
            kind: ArchiveKind::Rsli,
            document: Arc::new(ArchiveDocument::Rsli(document)),
        });
    }
    Err(ResourceError::Format(
        "unsupported archive magic for resource repository".to_string(),
    ))
}

fn resource_error_from_vfs(err: VfsError) -> ResourceError {
    match err {
        VfsError::NotFound(_) => ResourceError::MissingArchive,
        VfsError::Ambiguous(path) => ResourceError::Format(format!("ambiguous VFS path: {path}")),
        VfsError::Io(source) => ResourceError::Format(source.to_string()),
        VfsError::Path => ResourceError::Format("invalid VFS path".to_string()),
    }
}

/// Builds a resource name from raw bytes.
#[must_use]
pub fn resource_name(raw: impl AsRef<[u8]>) -> ResourceName {
    ResourceName(raw.as_ref().to_vec())
}

/// Normalizes an archive path for resource lookup.
///
/// # Errors
///
/// Returns [`ResourceError::Format`] when the path is not a valid relative
/// resource path.
pub fn archive_path(raw: impl AsRef<[u8]>) -> Result<NormalizedPath, ResourceError> {
    normalize_relative(raw.as_ref(), PathPolicy::StrictLegacy)
        .map_err(|err| ResourceError::Format(err.to_string()))
}

fn c_name_bytes(raw: &[u8; 12]) -> &[u8] {
    let len = raw.iter().position(|byte| *byte == 0).unwrap_or(raw.len());
    &raw[..len]
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_vfs::{DirectoryVfs, MemoryVfs};
    use std::path::PathBuf;

    #[test]
    fn cached_repository_reads_synthetic_nres() {
        let path = archive_path(b"archives/test.lib").expect("path");
        let bytes = build_nres(&[("Alpha.TXT", b"alpha".as_slice()), ("beta.bin", b"beta")]);
        let mut vfs = MemoryVfs::default();
        vfs.insert(path.clone(), Arc::from(bytes.into_boxed_slice()));
        let repo = CachedResourceRepository::new(Arc::new(vfs));

        let first = repo.open_archive(&path).expect("open archive");
        let second = repo.open_archive(&path).expect("open archive again");
        assert_eq!(first, second);
        assert_eq!(repo.archive_kind(first).expect("kind"), ArchiveKind::Nres);

        let handle = repo
            .find(first, &resource_name(b"alpha.txt"))
            .expect("find")
            .expect("entry");
        assert_eq!(repo.read(handle).expect("read").as_slice(), b"alpha");
        let info = repo.entry_info(handle).expect("entry info");
        assert_eq!(info.key.archive, path);
        assert!(info.key.name.0.eq_ignore_ascii_case(b"Alpha.TXT"));
        assert!(matches!(
            repo.read(EntryHandle {
                archive: ArchiveId(99),
                generation: 0,
                local: 0
            }),
            Err(ResourceError::InvalidHandle)
        ));
    }

    #[test]
    fn entry_handles_are_archive_qualified() {
        let first_path = archive_path(b"first.lib").expect("first path");
        let second_path = archive_path(b"second.lib").expect("second path");
        let mut vfs = MemoryVfs::default();
        vfs.insert(
            first_path.clone(),
            Arc::from(build_nres(&[("same.bin", b"first".as_slice())]).into_boxed_slice()),
        );
        vfs.insert(
            second_path.clone(),
            Arc::from(build_nres(&[("same.bin", b"second".as_slice())]).into_boxed_slice()),
        );
        let repo = CachedResourceRepository::new(Arc::new(vfs));

        let first_archive = repo.open_archive(&first_path).expect("first archive");
        let second_archive = repo.open_archive(&second_path).expect("second archive");
        let first_handle = repo
            .find(first_archive, &resource_name(b"same.bin"))
            .expect("first find")
            .expect("first handle");
        let second_handle = repo
            .find(second_archive, &resource_name(b"same.bin"))
            .expect("second find")
            .expect("second handle");

        assert_ne!(first_handle, second_handle);
        assert_eq!(first_handle.archive, first_archive);
        assert_eq!(second_handle.archive, second_archive);
        assert_eq!(
            repo.read(first_handle).expect("first read").as_slice(),
            b"first"
        );
        assert_eq!(
            repo.read(second_handle).expect("second read").as_slice(),
            b"second"
        );
    }

    #[test]
    fn archive_cache_and_decoded_payload_cache_evict_independently() {
        let path = archive_path(b"cache/test.lib").expect("path");
        let bytes = build_nres(&[("a.bin", b"a".as_slice()), ("b.bin", b"b".as_slice())]);
        let mut vfs = MemoryVfs::default();
        vfs.insert(path.clone(), Arc::from(bytes.into_boxed_slice()));
        let repo = CachedResourceRepository::with_payload_cache_budget(Arc::new(vfs), 1);

        let archive = repo.open_archive(&path).expect("open archive");
        let first = repo
            .find(archive, &resource_name(b"a.bin"))
            .expect("find a")
            .expect("a");
        let second = repo
            .find(archive, &resource_name(b"b.bin"))
            .expect("find b")
            .expect("b");
        assert_eq!(repo.read(first).expect("read a").as_slice(), b"a");
        assert_eq!(repo.read(second).expect("read b").as_slice(), b"b");

        let state = repo.state.lock().expect("state");
        assert_eq!(state.archives.len(), 1);
        assert_eq!(state.payload_cache.entries.len(), 1);
        assert_eq!(state.paths.get(path.as_str()).copied(), Some(archive));
        drop(state);

        assert_eq!(repo.open_archive(&path).expect("cached archive"), archive);
        assert_eq!(
            repo.read(first).expect("reread evicted payload").as_slice(),
            b"a"
        );
    }

    #[test]
    fn decoded_payload_cache_evicts_by_byte_budget() {
        let path = archive_path(b"cache/bytes.lib").expect("path");
        let bytes = build_nres(&[
            ("a.bin", b"1234".as_slice()),
            ("b.bin", b"5678".as_slice()),
            ("c.bin", b"90".as_slice()),
        ]);
        let mut vfs = MemoryVfs::default();
        vfs.insert(path.clone(), Arc::from(bytes.into_boxed_slice()));
        let repo = CachedResourceRepository::with_payload_cache_limits(
            Arc::new(vfs),
            PayloadCacheLimits {
                max_entries: 64,
                max_bytes: 6,
            },
        );

        let archive = repo.open_archive(&path).expect("open archive");
        let first = repo
            .find(archive, &resource_name(b"a.bin"))
            .expect("find a")
            .expect("a");
        let second = repo
            .find(archive, &resource_name(b"b.bin"))
            .expect("find b")
            .expect("b");
        let third = repo
            .find(archive, &resource_name(b"c.bin"))
            .expect("find c")
            .expect("c");

        assert_eq!(repo.read(first).expect("read a").as_slice(), b"1234");
        assert_eq!(repo.read(second).expect("read b").as_slice(), b"5678");
        assert_eq!(repo.read(third).expect("read c").as_slice(), b"90");

        let state = repo.state.lock().expect("state");
        assert_eq!(state.payload_cache.current_bytes, 6);
        assert_eq!(state.payload_cache.entries.len(), 2);
        assert!(!state.payload_cache.entries.contains_key(&first));
        assert!(state.payload_cache.entries.contains_key(&second));
        assert!(state.payload_cache.entries.contains_key(&third));
    }

    #[test]
    fn decoded_payload_cache_does_not_store_payload_larger_than_budget() {
        let path = archive_path(b"cache/oversized.lib").expect("path");
        let bytes = build_nres(&[("big.bin", b"1234567".as_slice())]);
        let mut vfs = MemoryVfs::default();
        vfs.insert(path.clone(), Arc::from(bytes.into_boxed_slice()));
        let repo = CachedResourceRepository::with_payload_cache_limits(
            Arc::new(vfs),
            PayloadCacheLimits {
                max_entries: 64,
                max_bytes: 6,
            },
        );

        let archive = repo.open_archive(&path).expect("open archive");
        let handle = repo
            .find(archive, &resource_name(b"big.bin"))
            .expect("find big")
            .expect("big");

        assert_eq!(repo.read(handle).expect("read big").as_slice(), b"1234567");

        let state = repo.state.lock().expect("state");
        assert_eq!(state.payload_cache.current_bytes, 0);
        assert!(state.payload_cache.entries.is_empty());
    }

    #[test]
    fn archive_cache_invalidates_when_vfs_bytes_change() {
        let root = temp_dir("archive-invalidate");
        let path = archive_path(b"cache/test.lib").expect("path");
        let host_path = root.join(path.as_str());
        std::fs::create_dir_all(host_path.parent().expect("parent")).expect("cache dir");
        std::fs::write(&host_path, build_nres(&[("a.bin", b"before".as_slice())]))
            .expect("initial archive");
        let repo = CachedResourceRepository::new(Arc::new(DirectoryVfs::new(&root)));

        let archive = repo.open_archive(&path).expect("open initial archive");
        let first = repo
            .find(archive, &resource_name(b"a.bin"))
            .expect("find initial")
            .expect("initial handle");
        assert_eq!(
            repo.read(first).expect("read initial").as_slice(),
            b"before"
        );

        std::fs::write(&host_path, build_nres(&[("a.bin", b"after!".as_slice())]))
            .expect("updated archive");
        let reopened = repo.open_archive(&path).expect("open updated archive");
        let second = repo
            .find(reopened, &resource_name(b"a.bin"))
            .expect("find updated")
            .expect("updated handle");

        assert_eq!(reopened, archive);
        assert_ne!(first, second);
        assert!(matches!(repo.read(first), Err(ResourceError::StaleHandle)));
        assert_eq!(
            repo.read(second).expect("read updated").as_slice(),
            b"after!"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn entry_read_error_carries_archive_path_and_entry_name() {
        let path = archive_path(b"bad/rsli.lib").expect("path");
        let mut vfs = MemoryVfs::default();
        vfs.insert(
            path.clone(),
            Arc::from(build_rsli_unknown_method(b"BROKEN.TEX", b"x").into_boxed_slice()),
        );
        let repo = CachedResourceRepository::new(Arc::new(vfs));
        let archive = repo.open_archive(&path).expect("open bad archive");
        let handle = repo
            .find(archive, &resource_name(b"BROKEN.TEX"))
            .expect("find bad entry")
            .expect("bad handle");

        let err = repo.read(handle).expect_err("read should fail");

        match err {
            ResourceError::EntryRead { key, source } => {
                assert_eq!(key.archive, path);
                assert_eq!(key.name.0, b"BROKEN.TEX");
                assert!(source.contains("unsupported packing method"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn resource_error_display_is_actionable() {
        let path = archive_path(b"bad/rsli.lib").expect("path");
        let err = ResourceError::EntryRead {
            key: ResourceKey {
                archive: path,
                name: resource_name(b"BROKEN.TEX"),
                type_id: None,
            },
            source: "unsupported packing method 0x1e0".to_string(),
        };

        assert_eq!(
            err.to_string(),
            "failed to read resource -:BROKEN.TEX from bad/rsli.lib: unsupported packing method 0x1e0"
        );
        assert_eq!(
            ResourceError::StaleHandle.to_string(),
            "resource handle belongs to an older archive generation"
        );
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_corpora_repository_reads_nres_and_rsli() {
        licensed_repository_gate("IS").expect("part 1 repository gate");
        licensed_repository_gate("IS2").expect("part 2 repository gate");
    }

    fn licensed_repository_gate(corpus: &str) -> Result<(), String> {
        let variable = match corpus {
            "IS" => "FPARKAN_CORPUS_PART1_ROOT",
            "IS2" => "FPARKAN_CORPUS_PART2_ROOT",
            _ => return Err(format!("unknown licensed corpus part: {corpus}")),
        };
        let root = std::env::var_os(variable)
            .map(PathBuf::from)
            .ok_or_else(|| format!("{variable} is required for licensed corpus tests"))?;
        if !root.is_dir() {
            return Err(format!(
                "licensed corpus root is missing: {}",
                root.display()
            ));
        }
        let repo = CachedResourceRepository::new(Arc::new(DirectoryVfs::new(&root)));

        let material_path = archive_path(b"Material.lib").map_err(|err| err.to_string())?;
        let material_bytes =
            std::fs::read(root.join(material_path.as_str())).map_err(|err| err.to_string())?;
        let material_doc = fparkan_nres::decode(
            Arc::from(material_bytes.clone().into_boxed_slice()),
            fparkan_nres::ReadProfile::Compatible,
        )
        .map_err(|err| err.to_string())?;
        let material_entry = material_doc
            .entries()
            .first()
            .ok_or_else(|| "Material.lib has no entries".to_string())?;

        let material_archive = repo
            .open_archive(&material_path)
            .map_err(|err| err.to_string())?;
        let material_handle = repo
            .find(
                material_archive,
                &resource_name(material_entry.name_bytes()),
            )
            .map_err(|err| err.to_string())?
            .ok_or_else(|| "Material.lib first entry not found".to_string())?;
        let material_payload = repo
            .read(material_handle)
            .map_err(|err| err.to_string())?
            .into_owned();
        let expected_material = material_doc
            .payload(material_entry.id())
            .map_err(|err| err.to_string())?;
        if material_payload != expected_material {
            return Err("Material.lib payload mismatch".to_string());
        }

        let font_path = archive_path(b"gamefont.rlb").map_err(|err| err.to_string())?;
        let font_bytes =
            std::fs::read(root.join(font_path.as_str())).map_err(|err| err.to_string())?;
        let font_doc = fparkan_rsli::decode(
            Arc::from(font_bytes.into_boxed_slice()),
            fparkan_rsli::ReadProfile::Compatible,
        )
        .map_err(|err| err.to_string())?;
        let font_entry = font_doc
            .entries()
            .first()
            .ok_or_else(|| "gamefont.rlb has no entries".to_string())?;
        let font_archive = repo
            .open_archive(&font_path)
            .map_err(|err| err.to_string())?;
        let font_handle = repo
            .find(font_archive, &resource_name(font_entry.name_raw))
            .map_err(|err| err.to_string())?
            .ok_or_else(|| "gamefont.rlb first entry not found".to_string())?;
        let font_payload = repo
            .read(font_handle)
            .map_err(|err| err.to_string())?
            .into_owned();
        let expected_font = font_doc
            .load(fparkan_rsli::EntryId(0))
            .map_err(|err| err.to_string())?;
        if font_payload != expected_font {
            return Err("gamefont.rlb payload mismatch".to_string());
        }
        Ok(())
    }

    fn build_nres(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = vec![0; 16];
        let mut offsets = Vec::with_capacity(entries.len());
        for (_, payload) in entries {
            offsets.push(u32::try_from(out.len()).expect("offset"));
            out.extend_from_slice(payload);
            let padding = (8 - (out.len() % 8)) % 8;
            out.resize(out.len() + padding, 0);
        }
        let mut order: Vec<usize> = (0..entries.len()).collect();
        order.sort_by(|left, right| {
            entries[*left]
                .0
                .as_bytes()
                .cmp(entries[*right].0.as_bytes())
        });
        for (idx, (name, payload)) in entries.iter().enumerate() {
            push_u32(&mut out, 0);
            push_u32(&mut out, 0);
            push_u32(&mut out, 0);
            push_u32(
                &mut out,
                u32::try_from(payload.len()).expect("payload size"),
            );
            push_u32(&mut out, 0);
            let mut name_raw = [0; 36];
            name_raw[..name.len()].copy_from_slice(name.as_bytes());
            out.extend_from_slice(&name_raw);
            push_u32(&mut out, offsets[idx]);
            push_u32(&mut out, u32::try_from(order[idx]).expect("sort index"));
        }
        out[0..4].copy_from_slice(b"NRes");
        out[4..8].copy_from_slice(&0x100_u32.to_le_bytes());
        out[8..12].copy_from_slice(&u32::try_from(entries.len()).expect("count").to_le_bytes());
        let total_size = u32::try_from(out.len()).expect("total size");
        out[12..16].copy_from_slice(&total_size.to_le_bytes());
        out
    }

    fn push_u32(out: &mut Vec<u8>, value: u32) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "fparkan-resource-{name}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("temp dir");
        path
    }

    fn build_rsli_unknown_method(name: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut header = [0u8; 32];
        header[0..4].copy_from_slice(b"NL\0\x01");
        header[4..6].copy_from_slice(&1i16.to_le_bytes());
        header[14..16].copy_from_slice(&0xABBAu16.to_le_bytes());
        header[20..24].copy_from_slice(&0x1234u32.to_le_bytes());

        let mut row = [0u8; 32];
        let name_len = name.len().min(12);
        row[0..name_len].copy_from_slice(&name[..name_len]);
        row[16..18].copy_from_slice(&0x1E0i16.to_le_bytes());
        row[20..24].copy_from_slice(
            &u32::try_from(payload.len())
                .expect("rsli unpacked size")
                .to_le_bytes(),
        );
        row[24..28].copy_from_slice(&64u32.to_le_bytes());
        row[28..32].copy_from_slice(
            &u32::try_from(payload.len())
                .expect("rsli packed size")
                .to_le_bytes(),
        );

        let mut out = Vec::new();
        out.extend_from_slice(&header);
        out.extend_from_slice(&test_xor_stream(&row, 0x1234));
        out.extend_from_slice(payload);
        out
    }

    fn test_xor_stream(data: &[u8], key16: u16) -> Vec<u8> {
        let mut lo = u8::try_from(key16 & 0xFF).expect("lo");
        let mut hi = u8::try_from((key16 >> 8) & 0xFF).expect("hi");
        data.iter()
            .map(|byte| {
                lo = hi ^ lo.wrapping_shl(1);
                let transformed = byte ^ lo;
                hi = lo ^ (hi >> 1);
                transformed
            })
            .collect()
    }
}
