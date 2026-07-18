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
//! Resource identity and repository ports.

use fparkan_binary::{sha256, Sha256Digest};
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
    MissingArchive {
        /// Logical archive path.
        path: NormalizedPath,
    },
    /// Missing entry.
    MissingEntry,
    /// Stale or invalid handle.
    InvalidHandle,
    /// Handle belongs to an older archive generation.
    StaleHandle,
    /// Resource archive path is invalid.
    InvalidPath {
        /// Display form of the rejected path.
        path: String,
        /// Validation or VFS rejection text.
        source: String,
    },
    /// Host lookup matched multiple candidates.
    PathAmbiguous {
        /// Ambiguous host path description.
        path: String,
    },
    /// Backing storage failed while reading an archive.
    Storage {
        /// Logical archive path.
        path: NormalizedPath,
        /// Underlying storage error.
        source: std::io::Error,
    },
    /// Archive magic is unsupported.
    UnsupportedArchive {
        /// Logical archive path.
        path: NormalizedPath,
    },
    /// Archive bytes were found but could not be decoded.
    ArchiveDecode {
        /// Logical archive path.
        path: NormalizedPath,
        /// Decoder failure text.
        source: String,
    },
    /// Format error.
    Format(String),
    /// Entry-specific read error.
    EntryRead {
        /// Resource key.
        key: ResourceKey,
        /// Source error text.
        source: String,
    },
    /// Repository exhausted stable archive handle space.
    HandleSpaceExhausted,
    /// Repository state lock was poisoned.
    Poisoned,
}

impl std::fmt::Display for ResourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingArchive { path } => {
                write!(f, "archive was not found: {}", path.display_lossy())
            }
            Self::MissingEntry => write!(f, "resource entry was not found in the archive"),
            Self::InvalidHandle => write!(
                f,
                "resource handle does not reference an open archive entry"
            ),
            Self::StaleHandle => {
                write!(f, "resource handle belongs to an older archive generation")
            }
            Self::InvalidPath { path, source } => {
                write!(f, "invalid resource archive path {path}: {source}")
            }
            Self::PathAmbiguous { path } => {
                write!(f, "resource archive path is ambiguous: {path}")
            }
            Self::Storage { path, source } => {
                write!(
                    f,
                    "failed to read archive {}: {source}",
                    path.display_lossy()
                )
            }
            Self::UnsupportedArchive { path } => write!(
                f,
                "unsupported archive magic for resource repository: {}",
                path.display_lossy()
            ),
            Self::ArchiveDecode { path, source } => {
                write!(
                    f,
                    "failed to decode archive {}: {source}",
                    path.display_lossy()
                )
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
            Self::HandleSpaceExhausted => {
                write!(f, "too many open archives for handle space")
            }
            Self::Poisoned => write!(f, "resource repository state lock was poisoned"),
        }
    }
}

impl std::error::Error for ResourceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Storage { source, .. } => Some(source),
            _ => None,
        }
    }
}

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

/// Repository-wide archive and payload cache limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RepositoryLimits {
    /// Maximum number of decoded archives retained in memory.
    pub max_open_archives: usize,
    /// Maximum total retained source archive bytes.
    pub max_archive_bytes: usize,
    /// Maximum cached decoded payload entries.
    pub max_decoded_payload_entries: usize,
    /// Maximum cached decoded payload bytes.
    pub max_decoded_payload_bytes: usize,
}

impl Default for RepositoryLimits {
    fn default() -> Self {
        Self {
            max_open_archives: 32,
            max_archive_bytes: 256 * 1024 * 1024,
            max_decoded_payload_entries: 64,
            max_decoded_payload_bytes: 64 * 1024 * 1024,
        }
    }
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
        let limits = RepositoryLimits::default();
        Self {
            max_entries: limits.max_decoded_payload_entries,
            max_bytes: limits.max_decoded_payload_bytes,
        }
    }
}

#[derive(Default)]
struct RepositoryState {
    paths: BTreeMap<Vec<u8>, ArchiveId>,
    archives: Vec<ArchiveSlot>,
    max_open_archives: usize,
    max_archive_bytes: usize,
    current_open_archives: usize,
    current_archive_bytes: usize,
    archive_access_generation: u64,
    payload_cache: DecodedPayloadCache,
}

struct ArchiveSlot {
    path: NormalizedPath,
    fingerprint: Sha256Digest,
    generation: u64,
    kind: ArchiveKind,
    document: Option<Arc<ArchiveDocument>>,
    archive_bytes: usize,
    last_access: u64,
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
        Self::with_limits(vfs, RepositoryLimits::default())
    }

    /// Creates a cached repository with explicit archive and payload budgets.
    #[must_use]
    pub fn with_limits(vfs: Arc<dyn Vfs>, limits: RepositoryLimits) -> Self {
        Self {
            vfs,
            state: Mutex::new(RepositoryState {
                max_open_archives: limits.max_open_archives,
                max_archive_bytes: limits.max_archive_bytes,
                payload_cache: DecodedPayloadCache::new(PayloadCacheLimits {
                    max_entries: limits.max_decoded_payload_entries,
                    max_bytes: limits.max_decoded_payload_bytes,
                }),
                ..RepositoryState::default()
            }),
        }
    }

    /// Creates a cached repository with a decoded payload entry budget.
    #[must_use]
    pub fn with_payload_cache_budget(vfs: Arc<dyn Vfs>, max_payload_entries: usize) -> Self {
        let limits = RepositoryLimits {
            max_decoded_payload_entries: max_payload_entries,
            ..RepositoryLimits::default()
        };
        Self::with_limits(vfs, limits)
    }

    /// Creates a cached repository with decoded payload entry and byte budgets.
    #[must_use]
    pub fn with_payload_cache_limits(vfs: Arc<dyn Vfs>, limits: PayloadCacheLimits) -> Self {
        let repository_limits = RepositoryLimits {
            max_decoded_payload_entries: limits.max_entries,
            max_decoded_payload_bytes: limits.max_bytes,
            ..RepositoryLimits::default()
        };
        Self::with_limits(vfs, repository_limits)
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
        let key = path.identity_bytes().to_vec();
        loop {
            // Read outside the repository lock. The content hash doubles as the
            // cache-validation value, so an unchanged open archive avoids both
            // decode and a second whole-archive metadata read.
            let bytes = self
                .vfs
                .read(path)
                .map_err(|err| resource_error_from_vfs(path, err))?;
            let observed_fingerprint = sha256(&bytes);
            let mut state = self.state.lock().map_err(|_| ResourceError::Poisoned)?;
            if let Some(id) = state.paths.get(&key).copied() {
                let current = state.archive(id)?;
                if current.document.is_some() && current.fingerprint == observed_fingerprint {
                    state.touch_archive(id)?;
                    return Ok(id);
                }
            }

            // A new or changed archive still receives the full decode and a
            // post-decode VFS fingerprint check before it commits to the cache.
            drop(state);
            let mut slot = decode_archive(path.clone(), bytes, observed_fingerprint)?;
            let current_vfs_fingerprint = self
                .vfs
                .metadata(path)
                .map_err(|err| resource_error_from_vfs(path, err))?
                .fingerprint;
            state = self.state.lock().map_err(|_| ResourceError::Poisoned)?;
            if let Some(id) = state.paths.get(&key).copied() {
                let current = state.archive(id)?;
                if current.document.is_some() && current.fingerprint == current_vfs_fingerprint {
                    state.touch_archive(id)?;
                    return Ok(id);
                }
                if current_vfs_fingerprint != observed_fingerprint {
                    continue;
                }
                if current.document.is_some() && current.fingerprint == observed_fingerprint {
                    state.touch_archive(id)?;
                    return Ok(id);
                }
                let current_generation = current.generation;
                let current_fingerprint = current.fingerprint;
                if current_fingerprint == observed_fingerprint {
                    slot.generation = current_generation;
                } else {
                    slot.generation = current_generation.saturating_add(1);
                    state.payload_cache.remove_archive(id);
                }
                state.unload_archive(id)?;
                *state.archive_mut(id)? = slot;
                state.load_archive(id)?;
                state.evict_archives(id)?;
                return Ok(id);
            }
            if current_vfs_fingerprint != observed_fingerprint {
                continue;
            }
            let id = ArchiveId(
                u64::try_from(state.archives.len())
                    .map_err(|_| ResourceError::HandleSpaceExhausted)?,
            );
            state.paths.insert(key.clone(), id);
            state.archives.push(slot);
            state.load_archive(id)?;
            state.evict_archives(id)?;
            return Ok(id);
        }
    }

    fn find(
        &self,
        archive: ArchiveId,
        name: &ResourceName,
    ) -> Result<Option<EntryHandle>, ResourceError> {
        let state = self.state.lock().map_err(|_| ResourceError::Poisoned)?;
        let slot = state.archive(archive)?;
        let document = slot.document.as_ref().ok_or(ResourceError::InvalidHandle)?;
        let local = match document.as_ref() {
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
        let document = slot.document.as_ref().ok_or(ResourceError::InvalidHandle)?;
        let local = match document.as_ref() {
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
        let document = slot.document.as_ref().ok_or(ResourceError::InvalidHandle)?;
        match document.as_ref() {
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
                        name: ResourceName(c_name_bytes(&meta.name_raw).to_vec()),
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
        let document = slot.document.as_ref().ok_or(ResourceError::InvalidHandle)?;
        Ok(PayloadDecodeTask {
            document: Arc::clone(document),
            key: slot.entry_key(entry.local)?,
        })
    }

    fn touch_archive(&mut self, id: ArchiveId) -> Result<(), ResourceError> {
        self.archive_access_generation = self.archive_access_generation.saturating_add(1);
        let access = self.archive_access_generation;
        self.archive_mut(id)?.last_access = access;
        Ok(())
    }

    fn load_archive(&mut self, id: ArchiveId) -> Result<(), ResourceError> {
        let archive_bytes = self.archive(id)?.archive_bytes;
        if self.archive(id)?.document.is_none() {
            return Err(ResourceError::InvalidHandle);
        }
        self.current_open_archives = self.current_open_archives.saturating_add(1);
        self.current_archive_bytes = self.current_archive_bytes.saturating_add(archive_bytes);
        self.touch_archive(id)
    }

    fn unload_archive(&mut self, id: ArchiveId) -> Result<(), ResourceError> {
        let (was_loaded, archive_bytes) = {
            let slot = self.archive(id)?;
            (slot.document.is_some(), slot.archive_bytes)
        };
        if was_loaded {
            self.current_open_archives = self.current_open_archives.saturating_sub(1);
            self.current_archive_bytes = self.current_archive_bytes.saturating_sub(archive_bytes);
            self.payload_cache.remove_archive(id);
            let slot = self.archive_mut(id)?;
            slot.document = None;
            slot.archive_bytes = 0;
            slot.generation = slot.generation.saturating_add(1);
        }
        Ok(())
    }

    fn evict_archives(&mut self, protected: ArchiveId) -> Result<(), ResourceError> {
        while self.current_open_archives > self.max_open_archives
            || self.current_archive_bytes > self.max_archive_bytes
        {
            let Some(victim) = self
                .archives
                .iter()
                .enumerate()
                .filter_map(|(index, slot)| {
                    let id = ArchiveId(u64::try_from(index).ok()?);
                    if id == protected || slot.document.is_none() {
                        return None;
                    }
                    Some((id, slot.last_access))
                })
                .min_by_key(|(_, access)| *access)
                .map(|(id, _)| id)
            else {
                break;
            };
            self.unload_archive(victim)?;
        }
        Ok(())
    }
}

impl ArchiveSlot {
    fn entry_key(&self, local: u32) -> Result<ResourceKey, ResourceError> {
        let document = self.document.as_ref().ok_or(ResourceError::InvalidHandle)?;
        match document.as_ref() {
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
    let archive_bytes = bytes.len();
    if bytes.starts_with(b"NRes") {
        let document =
            fparkan_nres::decode(bytes, fparkan_nres::ReadProfile::Compatible).map_err(|err| {
                ResourceError::ArchiveDecode {
                    path: path.clone(),
                    source: err.to_string(),
                }
            })?;
        return Ok(ArchiveSlot {
            path,
            fingerprint,
            generation: 0,
            kind: ArchiveKind::Nres,
            archive_bytes,
            last_access: 0,
            document: Some(Arc::new(ArchiveDocument::Nres(document))),
        });
    }
    if bytes.get(0..4) == Some(b"NL\0\x01") {
        let document =
            fparkan_rsli::decode(bytes, fparkan_rsli::ReadProfile::Compatible).map_err(|err| {
                ResourceError::ArchiveDecode {
                    path: path.clone(),
                    source: err.to_string(),
                }
            })?;
        return Ok(ArchiveSlot {
            path,
            fingerprint,
            generation: 0,
            kind: ArchiveKind::Rsli,
            archive_bytes,
            last_access: 0,
            document: Some(Arc::new(ArchiveDocument::Rsli(document))),
        });
    }
    Err(ResourceError::UnsupportedArchive { path })
}

fn resource_error_from_vfs(path: &NormalizedPath, err: VfsError) -> ResourceError {
    match err {
        VfsError::NotFound(_) => ResourceError::MissingArchive { path: path.clone() },
        VfsError::Ambiguous(path) => ResourceError::PathAmbiguous { path },
        VfsError::Io(source) => ResourceError::Storage {
            path: path.clone(),
            source,
        },
        VfsError::Path => ResourceError::InvalidPath {
            path: path.display_lossy().to_string(),
            source: "invalid VFS path".to_string(),
        },
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
/// Returns [`ResourceError::InvalidPath`] when the path is not a valid relative
/// resource path.
pub fn archive_path(raw: impl AsRef<[u8]>) -> Result<NormalizedPath, ResourceError> {
    let raw = raw.as_ref();
    normalize_relative(raw, PathPolicy::StrictLegacy).map_err(|err| ResourceError::InvalidPath {
        path: String::from_utf8_lossy(raw).to_string(),
        source: err.to_string(),
    })
}

fn c_name_bytes(raw: &[u8; 12]) -> &[u8] {
    let len = raw.iter().position(|byte| *byte == 0).unwrap_or(raw.len());
    &raw[..len]
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_vfs::{DirectoryVfs, MemoryVfs, Vfs, VfsEntry, VfsError, VfsMetadata};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Condvar;
    use std::thread;

    enum FailingReadMode {
        Ambiguous(&'static str),
        Io,
        Path,
    }

    struct FailingReadVfs {
        mode: FailingReadMode,
    }

    impl Vfs for FailingReadVfs {
        fn metadata(&self, _path: &NormalizedPath) -> Result<VfsMetadata, VfsError> {
            unreachable!("metadata is not used in these tests");
        }

        fn read(&self, _path: &NormalizedPath) -> Result<Arc<[u8]>, VfsError> {
            match self.mode {
                FailingReadMode::Ambiguous(path) => Err(VfsError::Ambiguous(path.to_string())),
                FailingReadMode::Io => Err(VfsError::Io(std::io::Error::other("disk offline"))),
                FailingReadMode::Path => Err(VfsError::Path),
            }
        }

        fn list(&self, _prefix: &NormalizedPath) -> Result<Vec<VfsEntry>, VfsError> {
            unreachable!("list is not used in these tests");
        }
    }

    struct CountingVfs {
        bytes: Arc<[u8]>,
        reads: AtomicUsize,
        metadata_reads: AtomicUsize,
    }

    impl CountingVfs {
        fn new(bytes: Arc<[u8]>) -> Self {
            Self {
                bytes,
                reads: AtomicUsize::new(0),
                metadata_reads: AtomicUsize::new(0),
            }
        }
    }

    impl Vfs for CountingVfs {
        fn metadata(&self, _path: &NormalizedPath) -> Result<VfsMetadata, VfsError> {
            self.metadata_reads.fetch_add(1, Ordering::Relaxed);
            Ok(VfsMetadata {
                len: self.bytes.len() as u64,
                fingerprint: sha256(&self.bytes),
            })
        }

        fn read(&self, _path: &NormalizedPath) -> Result<Arc<[u8]>, VfsError> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            Ok(Arc::clone(&self.bytes))
        }

        fn list(&self, _prefix: &NormalizedPath) -> Result<Vec<VfsEntry>, VfsError> {
            unreachable!("list is not used in these tests");
        }
    }

    struct CoordinatedReadState {
        current: Arc<[u8]>,
        first_read_started: bool,
        release_first_read: bool,
    }

    struct CoordinatedReadVfs {
        state: Mutex<CoordinatedReadState>,
        first_read_gate: Condvar,
    }

    impl CoordinatedReadVfs {
        fn new(initial: Arc<[u8]>) -> Self {
            Self {
                state: Mutex::new(CoordinatedReadState {
                    current: initial,
                    first_read_started: false,
                    release_first_read: false,
                }),
                first_read_gate: Condvar::new(),
            }
        }

        fn wait_for_first_read(&self) {
            let mut state = self.state.lock().expect("state");
            while !state.first_read_started {
                state = self.first_read_gate.wait(state).expect("wait");
            }
        }

        fn replace_current(&self, bytes: Arc<[u8]>) {
            self.state.lock().expect("state").current = bytes;
        }

        fn release_first_read(&self) {
            let mut state = self.state.lock().expect("state");
            state.release_first_read = true;
            self.first_read_gate.notify_all();
        }
    }

    impl Vfs for CoordinatedReadVfs {
        fn metadata(&self, _path: &NormalizedPath) -> Result<VfsMetadata, VfsError> {
            let state = self.state.lock().expect("state");
            Ok(VfsMetadata {
                len: state.current.len() as u64,
                fingerprint: sha256(&state.current),
            })
        }

        fn read(&self, _path: &NormalizedPath) -> Result<Arc<[u8]>, VfsError> {
            let mut state = self.state.lock().expect("state");
            let snapshot = Arc::clone(&state.current);
            if !state.first_read_started {
                state.first_read_started = true;
                self.first_read_gate.notify_all();
                while !state.release_first_read {
                    state = self.first_read_gate.wait(state).expect("wait");
                }
            }
            Ok(snapshot)
        }

        fn list(&self, _prefix: &NormalizedPath) -> Result<Vec<VfsEntry>, VfsError> {
            unreachable!("list is not used in these tests");
        }
    }

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
    fn cached_archive_fast_path_skips_second_metadata_fingerprint() {
        let path = archive_path(b"archives/test.lib").expect("path");
        let bytes = Arc::from(build_nres(&[("one", b"payload")]).into_boxed_slice());
        let vfs = Arc::new(CountingVfs::new(bytes));
        let repo = CachedResourceRepository::new(Arc::clone(&vfs) as Arc<dyn Vfs>);

        let first = repo.open_archive(&path).expect("first open");
        let second = repo.open_archive(&path).expect("cached open");

        assert_eq!(first, second);
        assert_eq!(vfs.reads.load(Ordering::Relaxed), 2);
        assert_eq!(vfs.metadata_reads.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn concurrent_same_archive_open_reuses_archive_id() {
        let path = archive_path(b"archives/test.lib").expect("path");
        let bytes = Arc::from(build_nres(&[("Alpha.TXT", b"alpha".as_slice())]).into_boxed_slice());
        let mut vfs = MemoryVfs::default();
        vfs.insert(path.clone(), bytes);
        let repo = Arc::new(CachedResourceRepository::new(Arc::new(vfs)));
        let first_repo = Arc::clone(&repo);
        let first_path = path.clone();
        let first = thread::spawn(move || first_repo.open_archive(&first_path));
        let second_repo = Arc::clone(&repo);
        let second_path = path.clone();
        let second = thread::spawn(move || second_repo.open_archive(&second_path));

        let first = first.join().expect("first join").expect("first archive");
        let second = second.join().expect("second join").expect("second archive");

        assert_eq!(first, second);
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
        assert_eq!(
            state.paths.get(path.identity_bytes()).copied(),
            Some(archive)
        );
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
        let host_path = root.join(path.as_path());
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
    fn concurrent_replacement_old_decode_cannot_overwrite_new() {
        let path = archive_path(b"cache/concurrent.lib").expect("path");
        let old_bytes =
            Arc::from(build_nres(&[("same.bin", b"old".as_slice())]).into_boxed_slice());
        let new_bytes =
            Arc::from(build_nres(&[("same.bin", b"new".as_slice())]).into_boxed_slice());
        let vfs = Arc::new(CoordinatedReadVfs::new(old_bytes));
        let repo = Arc::new(CachedResourceRepository::new(vfs.clone()));
        let stale_repo = Arc::clone(&repo);
        let stale_path = path.clone();
        let stale_open = thread::spawn(move || stale_repo.open_archive(&stale_path));

        vfs.wait_for_first_read();
        vfs.replace_current(Arc::clone(&new_bytes));
        let current_archive = repo.open_archive(&path).expect("open current archive");
        vfs.release_first_read();
        let raced_archive = stale_open
            .join()
            .expect("join stale thread")
            .expect("stale open");

        assert_eq!(raced_archive, current_archive);
        let handle = repo
            .find(current_archive, &resource_name(b"same.bin"))
            .expect("find current")
            .expect("current handle");
        assert_eq!(repo.read(handle).expect("read current").as_slice(), b"new");
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
    fn missing_archive_error_carries_logical_path() {
        let path = archive_path(b"missing/archive.lib").expect("path");
        let repo = CachedResourceRepository::new(Arc::new(MemoryVfs::default()));

        let err = repo.open_archive(&path).expect_err("missing archive");

        match err {
            ResourceError::MissingArchive { path: missing } => assert_eq!(missing, path),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn open_archive_maps_vfs_errors_to_typed_variants() {
        let path = archive_path(b"broken/archive.lib").expect("path");

        let ambiguous = CachedResourceRepository::new(Arc::new(FailingReadVfs {
            mode: FailingReadMode::Ambiguous("/tmp/root/archive.lib"),
        }));
        match ambiguous
            .open_archive(&path)
            .expect_err("ambiguous archive")
        {
            ResourceError::PathAmbiguous { path } => assert_eq!(path, "/tmp/root/archive.lib"),
            other => panic!("unexpected error: {other:?}"),
        }

        let io = CachedResourceRepository::new(Arc::new(FailingReadVfs {
            mode: FailingReadMode::Io,
        }));
        match io.open_archive(&path).expect_err("storage failure") {
            ResourceError::Storage {
                path: archive,
                source,
            } => {
                assert_eq!(archive, path);
                assert_eq!(source.to_string(), "disk offline");
            }
            other => panic!("unexpected error: {other:?}"),
        }

        let invalid = CachedResourceRepository::new(Arc::new(FailingReadVfs {
            mode: FailingReadMode::Path,
        }));
        match invalid.open_archive(&path).expect_err("invalid path") {
            ResourceError::InvalidPath { path: raw, source } => {
                assert_eq!(raw, "broken/archive.lib");
                assert_eq!(source, "invalid VFS path");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn open_archive_reports_decode_and_magic_errors() {
        let malformed_path = archive_path(b"broken/malformed.lib").expect("malformed path");
        let unsupported_path = archive_path(b"broken/unsupported.lib").expect("unsupported path");
        let mut vfs = MemoryVfs::default();
        vfs.insert(
            malformed_path.clone(),
            Arc::from(b"NRes".to_vec().into_boxed_slice()),
        );
        vfs.insert(
            unsupported_path.clone(),
            Arc::from(b"ABCD".to_vec().into_boxed_slice()),
        );
        let repo = CachedResourceRepository::new(Arc::new(vfs));

        match repo
            .open_archive(&malformed_path)
            .expect_err("malformed archive should fail")
        {
            ResourceError::ArchiveDecode { path, source } => {
                assert_eq!(path, malformed_path);
                assert!(!source.is_empty());
            }
            other => panic!("unexpected error: {other:?}"),
        }

        match repo
            .open_archive(&unsupported_path)
            .expect_err("unsupported archive should fail")
        {
            ResourceError::UnsupportedArchive { path } => assert_eq!(path, unsupported_path),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn lossy_equivalent_archive_paths_remain_distinct() {
        let first_path = archive_path(b"DATA/\xFF.lib").expect("first path");
        let second_path = archive_path(b"DATA/\xFE.lib").expect("second path");
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

        assert_ne!(first_archive, second_archive);
        assert_eq!(
            repo.read(
                repo.find(first_archive, &resource_name(b"same.bin"))
                    .expect("find first")
                    .expect("first handle")
            )
            .expect("read first")
            .as_slice(),
            b"first"
        );
        assert_eq!(
            repo.read(
                repo.find(second_archive, &resource_name(b"same.bin"))
                    .expect("find second")
                    .expect("second handle")
            )
            .expect("read second")
            .as_slice(),
            b"second"
        );
    }

    #[test]
    fn archive_cache_eviction_makes_old_handles_stale() {
        let first_path = archive_path(b"cache/first.lib").expect("first path");
        let second_path = archive_path(b"cache/second.lib").expect("second path");
        let mut vfs = MemoryVfs::default();
        vfs.insert(
            first_path.clone(),
            Arc::from(build_nres(&[("a.bin", b"first".as_slice())]).into_boxed_slice()),
        );
        vfs.insert(
            second_path.clone(),
            Arc::from(build_nres(&[("b.bin", b"second".as_slice())]).into_boxed_slice()),
        );
        let repo = CachedResourceRepository::with_limits(
            Arc::new(vfs),
            RepositoryLimits {
                max_open_archives: 1,
                max_archive_bytes: usize::MAX,
                max_decoded_payload_entries: 64,
                max_decoded_payload_bytes: 64 * 1024 * 1024,
            },
        );

        let first_archive = repo.open_archive(&first_path).expect("open first");
        let first_handle = repo
            .find(first_archive, &resource_name(b"a.bin"))
            .expect("find first")
            .expect("first handle");
        assert_eq!(
            repo.read(first_handle).expect("read first").as_slice(),
            b"first"
        );

        let _second_archive = repo.open_archive(&second_path).expect("open second");
        assert!(matches!(
            repo.read(first_handle),
            Err(ResourceError::StaleHandle)
        ));

        let reopened = repo.open_archive(&first_path).expect("reopen first");
        let refreshed = repo
            .find(reopened, &resource_name(b"a.bin"))
            .expect("find refreshed")
            .expect("refreshed handle");
        assert_eq!(reopened, first_archive);
        assert_ne!(refreshed, first_handle);
        assert_eq!(
            repo.read(refreshed).expect("read refreshed").as_slice(),
            b"first"
        );
    }

    #[test]
    fn archive_cache_evicts_by_byte_budget() {
        let first_path = archive_path(b"cache/first-bytes.lib").expect("first path");
        let second_path = archive_path(b"cache/second-bytes.lib").expect("second path");
        let first_bytes = build_nres(&[("a.bin", b"first".as_slice())]);
        let second_bytes = build_nres(&[("b.bin", b"second".as_slice())]);
        let second_budget = second_bytes.len();
        let mut vfs = MemoryVfs::default();
        vfs.insert(
            first_path.clone(),
            Arc::from(first_bytes.into_boxed_slice()),
        );
        vfs.insert(
            second_path.clone(),
            Arc::from(second_bytes.into_boxed_slice()),
        );
        let repo = CachedResourceRepository::with_limits(
            Arc::new(vfs),
            RepositoryLimits {
                max_open_archives: 2,
                max_archive_bytes: second_budget,
                max_decoded_payload_entries: 64,
                max_decoded_payload_bytes: 64 * 1024 * 1024,
            },
        );

        let first_archive = repo.open_archive(&first_path).expect("open first");
        let first_handle = repo
            .find(first_archive, &resource_name(b"a.bin"))
            .expect("find first")
            .expect("first handle");
        assert_eq!(
            repo.read(first_handle).expect("read first").as_slice(),
            b"first"
        );

        let second_archive = repo.open_archive(&second_path).expect("open second");
        let second_handle = repo
            .find(second_archive, &resource_name(b"b.bin"))
            .expect("find second")
            .expect("second handle");
        assert_eq!(
            repo.read(second_handle).expect("read second").as_slice(),
            b"second"
        );

        assert!(matches!(
            repo.read(first_handle),
            Err(ResourceError::StaleHandle)
        ));
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
        assert_eq!(
            ResourceError::MissingArchive {
                path: archive_path(b"missing.lib").expect("missing path")
            }
            .to_string(),
            "archive was not found: missing.lib"
        );
        assert_eq!(
            ResourceError::PathAmbiguous {
                path: "/tmp/root/MATERIAL.LIB".to_string()
            }
            .to_string(),
            "resource archive path is ambiguous: /tmp/root/MATERIAL.LIB"
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
            std::fs::read(root.join(material_path.as_path())).map_err(|err| err.to_string())?;
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
            std::fs::read(root.join(font_path.as_path())).map_err(|err| err.to_string())?;
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
