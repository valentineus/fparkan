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
//! Strict and lossless `NRes` archive support.

use fparkan_binary::{checked_allocation_len, Cursor, DecodeError};
use fparkan_path::{ascii_lookup_key, LookupKey};
use std::cmp::Ordering;
use std::fmt;
use std::ops::Range;
use std::sync::Arc;

const HEADER_LEN: usize = 16;
const HEADER_LEN_U32: u32 = 16;
const ENTRY_LEN: usize = 64;
const NAME_LEN: usize = 36;
const VERSION_0100: u32 = 0x100;

/// Read profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadProfile {
    /// Reject malformed lookup tables and directory invariants.
    Strict,
    /// Keep the document readable when the lookup table is invalid.
    Compatible,
}

/// Write profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriteProfile {
    /// Preserve the original byte image or unindexed data-region bytes.
    Lossless,
    /// Repack active payloads and rebuild the lookup table.
    CanonicalCompact,
}

/// Decode-time archive limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodeLimits {
    /// Maximum accepted source archive bytes.
    pub max_input_bytes: u64,
    /// Maximum accepted entry count.
    pub max_entries: u32,
    /// Maximum accepted single payload byte length.
    pub max_decoded_entry_bytes: u64,
    /// Maximum accepted cumulative payload bytes.
    pub max_total_decoded_bytes: u64,
    /// Maximum accepted preserved-region bytes.
    pub max_preserved_bytes: u64,
}

impl Default for DecodeLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 256 * 1024 * 1024,
            max_entries: 1_000_000,
            max_decoded_entry_bytes: 256 * 1024 * 1024,
            max_total_decoded_bytes: 512 * 1024 * 1024,
            max_preserved_bytes: 256 * 1024 * 1024,
        }
    }
}

/// `NRes` archive header.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NresHeader {
    /// Archive format version.
    pub version: u32,
    /// Number of directory entries.
    pub entry_count: u32,
    /// Total byte size declared by the header.
    pub total_size: u32,
    /// Directory byte offset.
    pub directory_offset: u32,
}

/// `NRes` entry identifier in original directory order.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EntryId(pub u32);

/// `NRes` entry metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntryMeta {
    /// Entry type identifier.
    pub type_id: u32,
    /// Opaque attribute 1.
    pub attr1: u32,
    /// Opaque attribute 2.
    pub attr2: u32,
    /// Opaque attribute 3.
    pub attr3: u32,
    /// Decoded byte-for-byte ASCII-style resource name.
    pub name: String,
    /// Payload byte offset.
    pub data_offset: u32,
    /// Payload byte size.
    pub data_size: u32,
    /// Lookup table value stored at this sorted position.
    pub sort_index: u32,
}

/// `NRes` entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NresEntry {
    id: EntryId,
    meta: EntryMeta,
    name_raw: [u8; NAME_LEN],
    data_range: Range<usize>,
}

/// Preserved bytes that are not referenced by any entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreservedRegion {
    /// Byte range in the original archive.
    pub range: Range<u32>,
    /// Whether the whole range consists of zero bytes.
    pub all_zero: bool,
}

/// Parsed `NRes` document.
#[derive(Clone, Debug)]
pub struct NresDocument {
    bytes: Arc<[u8]>,
    header: NresHeader,
    entries: Vec<NresEntry>,
    lookup_order_valid: bool,
    preserved_regions: Vec<PreservedRegion>,
}

/// Editable `NRes` document.
#[derive(Clone, Debug)]
pub struct NresEditor {
    entries: Vec<EditableEntry>,
    layout: Vec<EditableSegment>,
}

#[derive(Clone, Debug)]
struct EditableEntry {
    type_id: u32,
    attr1: u32,
    attr2: u32,
    attr3: u32,
    name_raw: [u8; NAME_LEN],
    payload: Vec<u8>,
}

#[derive(Clone, Debug)]
enum EditableSegment {
    Entry(usize),
    Preserved(Vec<u8>),
}

/// `NRes` parse or write error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NresError {
    /// The input is not an `NRes` archive.
    InvalidMagic {
        /// First four bytes, padded when the file is shorter.
        got: [u8; 4],
    },
    /// Unsupported format version.
    UnsupportedVersion {
        /// Observed version.
        got: u32,
    },
    /// Entry count is negative.
    InvalidEntryCount {
        /// Observed signed count.
        got: i32,
    },
    /// Header size does not match the byte slice length.
    TotalSizeMismatch {
        /// Header value.
        header: u32,
        /// Actual byte length.
        actual: u64,
    },
    /// Directory range is outside the archive.
    DirectoryOutOfBounds {
        /// Computed directory offset.
        offset: u64,
        /// Computed directory length.
        len: u64,
        /// Actual byte length.
        file_len: u64,
    },
    /// Entry payload range is outside the data region.
    EntryDataOutOfBounds {
        /// Entry id.
        id: u32,
        /// Payload offset.
        offset: u32,
        /// Payload size.
        size: u32,
        /// Directory offset.
        directory_offset: u32,
    },
    /// Active payload ranges overlap.
    EntryDataOverlap {
        /// Earlier entry id.
        first: u32,
        /// Later entry id.
        second: u32,
    },
    /// Entry name has no zero terminator inside the fixed field.
    MissingNameTerminator {
        /// Entry id.
        id: u32,
    },
    /// Entry name is empty.
    EmptyName {
        /// Entry id.
        id: u32,
    },
    /// Lookup value points outside the directory.
    SortIndexOutOfRange {
        /// Sorted table position.
        position: u32,
        /// Stored index.
        index: u32,
        /// Entry count.
        entry_count: u32,
    },
    /// Lookup table is not a permutation.
    SortIndexDuplicate {
        /// Duplicated original entry index.
        index: u32,
    },
    /// Lookup table is a permutation but not sorted by ASCII-casefolded names.
    SortOrderMismatch {
        /// Sorted table position.
        position: u32,
    },
    /// Entry id is outside this archive.
    EntryIdOutOfRange {
        /// Entry id.
        id: u32,
        /// Entry count.
        entry_count: u32,
    },
    /// Authoring name is too long for the fixed `NRes` field.
    AuthoringNameTooLong {
        /// Observed byte length.
        len: usize,
        /// Maximum useful byte length before the required NUL terminator.
        max: usize,
    },
    /// Authoring name contains an embedded NUL byte.
    AuthoringNameContainsNul {
        /// Byte offset.
        offset: usize,
    },
    /// Arithmetic overflow or failed bounded read.
    Binary(DecodeError),
}

impl fmt::Display for NresError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMagic { got } => write!(f, "invalid NRes magic: {got:02X?}"),
            Self::UnsupportedVersion { got } => {
                write!(f, "unsupported NRes version: {got:#x}")
            }
            Self::InvalidEntryCount { got } => write!(f, "invalid NRes entry count: {got}"),
            Self::TotalSizeMismatch { header, actual } => {
                write!(f, "NRes total size mismatch: header={header}, actual={actual}")
            }
            Self::DirectoryOutOfBounds {
                offset,
                len,
                file_len,
            } => write!(
                f,
                "NRes directory out of bounds: offset={offset}, len={len}, file={file_len}"
            ),
            Self::EntryDataOutOfBounds {
                id,
                offset,
                size,
                directory_offset,
            } => write!(
                f,
                "NRes entry #{id} data out of bounds: offset={offset}, size={size}, directory={directory_offset}"
            ),
            Self::EntryDataOverlap { first, second } => {
                write!(f, "NRes entries #{first} and #{second} overlap")
            }
            Self::MissingNameTerminator { id } => {
                write!(f, "NRes entry #{id} name has no NUL terminator")
            }
            Self::EmptyName { id } => write!(f, "NRes entry #{id} name is empty"),
            Self::SortIndexOutOfRange {
                position,
                index,
                entry_count,
            } => write!(
                f,
                "NRes sort index out of range at position {position}: {index} >= {entry_count}"
            ),
            Self::SortIndexDuplicate { index } => {
                write!(f, "NRes duplicate sort index: {index}")
            }
            Self::SortOrderMismatch { position } => {
                write!(f, "NRes sort order mismatch at position {position}")
            }
            Self::EntryIdOutOfRange { id, entry_count } => {
                write!(f, "NRes entry id out of range: {id} >= {entry_count}")
            }
            Self::AuthoringNameTooLong { len, max } => {
                write!(f, "NRes authoring name too long: {len} > {max}")
            }
            Self::AuthoringNameContainsNul { offset } => {
                write!(f, "NRes authoring name contains NUL at byte {offset}")
            }
            Self::Binary(source) => write!(f, "{source}"),
        }
    }
}

impl std::error::Error for NresError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Binary(source) => Some(source),
            Self::InvalidMagic { .. }
            | Self::UnsupportedVersion { .. }
            | Self::InvalidEntryCount { .. }
            | Self::TotalSizeMismatch { .. }
            | Self::DirectoryOutOfBounds { .. }
            | Self::EntryDataOutOfBounds { .. }
            | Self::EntryDataOverlap { .. }
            | Self::MissingNameTerminator { .. }
            | Self::EmptyName { .. }
            | Self::SortIndexOutOfRange { .. }
            | Self::SortIndexDuplicate { .. }
            | Self::SortOrderMismatch { .. }
            | Self::EntryIdOutOfRange { .. }
            | Self::AuthoringNameTooLong { .. }
            | Self::AuthoringNameContainsNul { .. } => None,
        }
    }
}

impl From<DecodeError> for NresError {
    fn from(value: DecodeError) -> Self {
        Self::Binary(value)
    }
}

/// Decodes `NRes` bytes.
///
/// # Errors
///
/// Returns [`NresError`] when the header, directory, payload ranges, or strict
/// lookup permutation are malformed for the selected [`ReadProfile`].
pub fn decode(bytes: Arc<[u8]>, profile: ReadProfile) -> Result<NresDocument, NresError> {
    decode_with_limits(bytes, profile, DecodeLimits::default())
}

/// Decodes `NRes` bytes with explicit archive limits.
///
/// # Errors
///
/// Returns [`NresError`] when the input exceeds configured limits, the header,
/// directory, payload ranges, or strict lookup permutation are malformed.
pub fn decode_with_limits(
    bytes: Arc<[u8]>,
    profile: ReadProfile,
    limits: DecodeLimits,
) -> Result<NresDocument, NresError> {
    let header = parse_header(&bytes, limits)?;
    let entries = parse_entries(&bytes, &header, limits)?;
    validate_names(&entries)?;
    validate_payload_ranges(&entries, limits)?;
    let lookup_order_valid = match validate_lookup_order(&entries) {
        Ok(()) => true,
        Err(err) if profile == ReadProfile::Strict => return Err(err),
        Err(_) => false,
    };
    let preserved_regions =
        find_preserved_regions(&bytes, &entries, header.directory_offset, limits)?;
    Ok(NresDocument {
        bytes,
        header,
        entries,
        lookup_order_valid,
        preserved_regions,
    })
}

impl NresDocument {
    /// Returns the archive header.
    #[must_use]
    pub fn header(&self) -> &NresHeader {
        &self.header
    }

    /// Entry count.
    #[must_use]
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Returns all entries in original directory order.
    #[must_use]
    pub fn entries(&self) -> &[NresEntry] {
        &self.entries
    }

    /// Whether the lookup table is valid and sorted.
    #[must_use]
    pub fn lookup_order_valid(&self) -> bool {
        self.lookup_order_valid
    }

    /// Returns preserved ranges outside active payloads.
    #[must_use]
    pub fn preserved_regions(&self) -> &[PreservedRegion] {
        &self.preserved_regions
    }

    /// Whether any unindexed preserved region contains non-zero bytes.
    #[must_use]
    pub fn has_nonzero_preserved_region(&self) -> bool {
        self.preserved_regions.iter().any(|region| !region.all_zero)
    }

    /// Finds an entry by ASCII-case-insensitive name.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<EntryId> {
        self.find_bytes(name.as_bytes())
    }

    /// Finds an entry by ASCII-case-insensitive raw name bytes.
    #[must_use]
    pub fn find_bytes(&self, name: &[u8]) -> Option<EntryId> {
        if self.lookup_order_valid {
            return self.find_by_lookup(name);
        }
        self.entries
            .iter()
            .find(|entry| cmp_ascii_casefold(name, entry.name_bytes()) == Ordering::Equal)
            .map(NresEntry::id)
    }

    /// Returns an entry by id.
    #[must_use]
    pub fn entry(&self, id: EntryId) -> Option<&NresEntry> {
        self.entries.get(usize::try_from(id.0).ok()?)
    }

    /// Returns an entry payload.
    ///
    /// # Errors
    ///
    /// Returns [`NresError::EntryIdOutOfRange`] when `id` is not present in
    /// this document.
    pub fn payload(&self, id: EntryId) -> Result<&[u8], NresError> {
        let entry = self.entry(id).ok_or_else(|| NresError::EntryIdOutOfRange {
            id: id.0,
            entry_count: saturating_u32_len(self.entries.len()),
        })?;
        Ok(&self.bytes[entry.data_range.clone()])
    }

    /// Encodes the document according to the selected write profile.
    #[must_use]
    pub fn encode(&self, profile: WriteProfile) -> Vec<u8> {
        match profile {
            WriteProfile::Lossless => self.bytes.to_vec(),
            WriteProfile::CanonicalCompact => self.encode_canonical_compact(),
        }
    }

    /// Creates an editor initialized from this document.
    ///
    /// # Errors
    ///
    /// Returns [`NresError`] if any source payload cannot be copied by id.
    pub fn editor(&self) -> Result<NresEditor, NresError> {
        NresEditor::from_document(self)
    }

    fn find_by_lookup(&self, needle: &[u8]) -> Option<EntryId> {
        let mut low = 0usize;
        let mut high = self.entries.len();
        while low < high {
            let mid = low + (high - low) / 2;
            let entry_idx = usize::try_from(self.entries[mid].meta.sort_index).ok()?;
            let entry = self.entries.get(entry_idx)?;
            match cmp_ascii_casefold(needle, entry.name_bytes()) {
                Ordering::Less => high = mid,
                Ordering::Greater => low = mid.saturating_add(1),
                Ordering::Equal => {
                    return self
                        .entries
                        .iter()
                        .find(|entry| {
                            cmp_ascii_casefold(needle, entry.name_bytes()) == Ordering::Equal
                        })
                        .map(NresEntry::id);
                }
            }
        }
        None
    }

    fn encode_canonical_compact(&self) -> Vec<u8> {
        let mut out = vec![0; HEADER_LEN];
        let mut offsets = Vec::with_capacity(self.entries.len());
        let mut sizes = Vec::with_capacity(self.entries.len());
        for entry in &self.entries {
            offsets.push(saturating_u32_len(out.len()));
            let payload = &self.bytes[entry.data_range.clone()];
            sizes.push(saturating_u32_len(payload.len()));
            out.extend_from_slice(payload);
            let padding = (8 - (out.len() % 8)) % 8;
            out.resize(out.len() + padding, 0);
        }

        let sort_order = build_sort_order(&self.entries);
        for (index, entry) in self.entries.iter().enumerate() {
            push_u32(&mut out, entry.meta.type_id);
            push_u32(&mut out, entry.meta.attr1);
            push_u32(&mut out, entry.meta.attr2);
            push_u32(&mut out, sizes[index]);
            push_u32(&mut out, entry.meta.attr3);
            out.extend_from_slice(&entry.name_raw);
            push_u32(&mut out, offsets[index]);
            push_u32(&mut out, saturating_u32_len(sort_order[index]));
        }

        let total_size = saturating_u32_len(out.len());
        out[0..4].copy_from_slice(b"NRes");
        out[4..8].copy_from_slice(&VERSION_0100.to_le_bytes());
        out[8..12].copy_from_slice(&saturating_u32_len(self.entries.len()).to_le_bytes());
        out[12..16].copy_from_slice(&total_size.to_le_bytes());
        out
    }
}

impl NresEditor {
    /// Creates an editor from an existing document.
    ///
    /// # Errors
    ///
    /// Returns [`NresError`] if any source payload cannot be copied by id.
    pub fn from_document(document: &NresDocument) -> Result<Self, NresError> {
        let mut entries = Vec::with_capacity(document.entries.len());
        for entry in &document.entries {
            let meta = entry.meta();
            entries.push(EditableEntry {
                type_id: meta.type_id,
                attr1: meta.attr1,
                attr2: meta.attr2,
                attr3: meta.attr3,
                name_raw: entry.name_raw,
                payload: document.payload(entry.id())?.to_vec(),
            });
        }
        let layout = build_edit_layout(document)?;
        Ok(Self { entries, layout })
    }

    /// Replaces an entry payload.
    ///
    /// # Errors
    ///
    /// Returns [`NresError::EntryIdOutOfRange`] when `id` is not present.
    pub fn set_payload(
        &mut self,
        id: EntryId,
        payload: impl Into<Vec<u8>>,
    ) -> Result<(), NresError> {
        let entry = self.entry_mut(id)?;
        entry.payload = payload.into();
        Ok(())
    }

    /// Renames an entry.
    ///
    /// # Errors
    ///
    /// Returns [`NresError::EntryIdOutOfRange`] when `id` is not present, or
    /// a name authoring error when `name` cannot be stored in the fixed field.
    pub fn rename(&mut self, id: EntryId, name: impl AsRef<[u8]>) -> Result<(), NresError> {
        let name_raw = authoring_name_raw(name.as_ref())?;
        let entry = self.entry_mut(id)?;
        entry.name_raw = name_raw;
        Ok(())
    }

    /// Encodes the edited document while preserving unindexed bytes.
    ///
    /// # Errors
    ///
    /// Returns [`NresError`] when offsets or sizes exceed the on-disk `u32`
    /// representation.
    pub fn encode(&self) -> Result<Vec<u8>, NresError> {
        self.encode_with_profile(WriteProfile::Lossless)
    }

    /// Encodes the edited document with an explicit write profile.
    ///
    /// # Errors
    ///
    /// Returns [`NresError`] when offsets or sizes exceed the on-disk `u32`
    /// representation.
    pub fn encode_with_profile(&self, profile: WriteProfile) -> Result<Vec<u8>, NresError> {
        match profile {
            WriteProfile::Lossless => self.encode_preserving_layout(),
            WriteProfile::CanonicalCompact => self.encode_canonical_compact(),
        }
    }

    fn encode_preserving_layout(&self) -> Result<Vec<u8>, NresError> {
        let mut out = vec![0; HEADER_LEN];
        let mut offsets = vec![0; self.entries.len()];
        let mut sizes = vec![0; self.entries.len()];
        for segment in &self.layout {
            match segment {
                EditableSegment::Entry(index) => {
                    let entry = self
                        .entries
                        .get(*index)
                        .ok_or(DecodeError::IntegerOverflow)?;
                    offsets[*index] = checked_u32_len(out.len())?;
                    sizes[*index] = checked_u32_len(entry.payload.len())?;
                    out.extend_from_slice(&entry.payload);
                }
                EditableSegment::Preserved(bytes) => {
                    out.len()
                        .checked_add(bytes.len())
                        .ok_or(DecodeError::IntegerOverflow)?;
                    out.extend_from_slice(bytes);
                }
            }
        }
        write_edit_archive_header_and_directory(&mut out, &self.entries, &offsets, &sizes)?;
        Ok(out)
    }

    fn encode_canonical_compact(&self) -> Result<Vec<u8>, NresError> {
        let mut out = vec![0; HEADER_LEN];
        let mut offsets = Vec::with_capacity(self.entries.len());
        let mut sizes = Vec::with_capacity(self.entries.len());
        for entry in &self.entries {
            offsets.push(checked_u32_len(out.len())?);
            sizes.push(checked_u32_len(entry.payload.len())?);
            out.extend_from_slice(&entry.payload);
            let padding = (8 - (out.len() % 8)) % 8;
            out.resize(
                out.len()
                    .checked_add(padding)
                    .ok_or(DecodeError::IntegerOverflow)?,
                0,
            );
        }

        write_edit_archive_header_and_directory(&mut out, &self.entries, &offsets, &sizes)?;
        Ok(out)
    }

    fn entry_mut(&mut self, id: EntryId) -> Result<&mut EditableEntry, NresError> {
        let entry_count = saturating_u32_len(self.entries.len());
        self.entries
            .get_mut(
                usize::try_from(id.0).map_err(|_| NresError::EntryIdOutOfRange {
                    id: id.0,
                    entry_count,
                })?,
            )
            .ok_or(NresError::EntryIdOutOfRange {
                id: id.0,
                entry_count,
            })
    }
}

impl NresEntry {
    /// Entry id in original directory order.
    #[must_use]
    pub fn id(&self) -> EntryId {
        self.id
    }

    /// Entry metadata.
    #[must_use]
    pub fn meta(&self) -> &EntryMeta {
        &self.meta
    }

    /// Raw fixed-size name field.
    #[must_use]
    pub fn name_raw(&self) -> &[u8; NAME_LEN] {
        &self.name_raw
    }

    /// Active payload range in the original archive.
    #[must_use]
    pub fn data_range(&self) -> Range<usize> {
        self.data_range.clone()
    }

    /// Raw name bytes before the first NUL terminator.
    #[must_use]
    pub fn name_bytes(&self) -> &[u8] {
        let len = name_len(&self.name_raw).unwrap_or(NAME_LEN);
        &self.name_raw[..len]
    }
}

fn parse_header(bytes: &[u8], limits: DecodeLimits) -> Result<NresHeader, NresError> {
    enforce_limit(
        u64::try_from(bytes.len()).map_err(|_| DecodeError::IntegerOverflow)?,
        limits.max_input_bytes,
    )?;
    if bytes.len() < HEADER_LEN {
        let mut got = [0; 4];
        let copy_len = bytes.len().min(4);
        got[..copy_len].copy_from_slice(&bytes[..copy_len]);
        return Err(NresError::InvalidMagic { got });
    }
    if &bytes[..4] != b"NRes" {
        let mut got = [0; 4];
        got.copy_from_slice(&bytes[..4]);
        return Err(NresError::InvalidMagic { got });
    }

    let mut cursor = Cursor::new(bytes);
    let _magic = cursor.read_exact(4)?;
    let version = cursor.read_u32_le()?;
    if version != VERSION_0100 {
        return Err(NresError::UnsupportedVersion { got: version });
    }
    let entry_count_signed = cursor.read_i32_le()?;
    if entry_count_signed < 0 {
        return Err(NresError::InvalidEntryCount {
            got: entry_count_signed,
        });
    }
    let entry_count =
        u32::try_from(entry_count_signed).map_err(|_| DecodeError::IntegerOverflow)?;
    enforce_limit(u64::from(entry_count), u64::from(limits.max_entries))?;
    let total_size = cursor.read_u32_le()?;
    let actual = u64::try_from(bytes.len()).map_err(|_| DecodeError::IntegerOverflow)?;
    if u64::from(total_size) != actual {
        return Err(NresError::TotalSizeMismatch {
            header: total_size,
            actual,
        });
    }
    let directory_len = u64::from(entry_count)
        .checked_mul(ENTRY_LEN as u64)
        .ok_or(DecodeError::IntegerOverflow)?;
    let directory_offset = u64::from(total_size).checked_sub(directory_len).ok_or(
        NresError::DirectoryOutOfBounds {
            offset: 0,
            len: directory_len,
            file_len: actual,
        },
    )?;
    if directory_offset < HEADER_LEN as u64
        || directory_offset
            .checked_add(directory_len)
            .ok_or(DecodeError::IntegerOverflow)?
            != actual
    {
        return Err(NresError::DirectoryOutOfBounds {
            offset: directory_offset,
            len: directory_len,
            file_len: actual,
        });
    }
    Ok(NresHeader {
        version,
        entry_count,
        total_size,
        directory_offset: u32::try_from(directory_offset)
            .map_err(|_| DecodeError::IntegerOverflow)?,
    })
}

fn parse_entries(
    bytes: &[u8],
    header: &NresHeader,
    limits: DecodeLimits,
) -> Result<Vec<NresEntry>, NresError> {
    let capacity = checked_allocation_len(
        u64::from(header.entry_count),
        u64::from(limits.max_entries),
    )?;
    let mut entries = Vec::with_capacity(capacity);
    let directory_offset =
        usize::try_from(header.directory_offset).map_err(|_| DecodeError::IntegerOverflow)?;
    for index in 0..header.entry_count {
        let index_usize = usize::try_from(index).map_err(|_| DecodeError::IntegerOverflow)?;
        let entry_offset = directory_offset
            .checked_add(
                index_usize
                    .checked_mul(ENTRY_LEN)
                    .ok_or(DecodeError::IntegerOverflow)?,
            )
            .ok_or(DecodeError::IntegerOverflow)?;
        entries.push(parse_entry(
            bytes,
            entry_offset,
            index,
            header.directory_offset,
        )?);
    }
    Ok(entries)
}

fn parse_entry(
    bytes: &[u8],
    offset: usize,
    id: u32,
    directory_offset: u32,
) -> Result<NresEntry, NresError> {
    let entry_bytes = bytes
        .get(offset..offset + ENTRY_LEN)
        .ok_or(DecodeError::IntegerOverflow)?;
    let mut cursor = Cursor::new(entry_bytes);
    let type_id = cursor.read_u32_le()?;
    let attr1 = cursor.read_u32_le()?;
    let attr2 = cursor.read_u32_le()?;
    let data_size = cursor.read_u32_le()?;
    let attr3 = cursor.read_u32_le()?;
    let name_slice = cursor.read_exact(NAME_LEN)?;
    let mut name_raw = [0; NAME_LEN];
    name_raw.copy_from_slice(name_slice);
    let Some(name_len) = name_len(&name_raw) else {
        return Err(NresError::MissingNameTerminator { id });
    };
    let name = name_raw[..name_len]
        .iter()
        .map(|byte| char::from(*byte))
        .collect();
    let data_offset = cursor.read_u32_le()?;
    let sort_index = cursor.read_u32_le()?;
    cursor.require_eof()?;

    let data_end = data_offset
        .checked_add(data_size)
        .ok_or(DecodeError::IntegerOverflow)?;
    if data_offset < HEADER_LEN_U32 || data_end > directory_offset {
        return Err(NresError::EntryDataOutOfBounds {
            id,
            offset: data_offset,
            size: data_size,
            directory_offset,
        });
    }

    Ok(NresEntry {
        id: EntryId(id),
        meta: EntryMeta {
            type_id,
            attr1,
            attr2,
            attr3,
            name,
            data_offset,
            data_size,
            sort_index,
        },
        name_raw,
        data_range: usize::try_from(data_offset).map_err(|_| DecodeError::IntegerOverflow)?
            ..usize::try_from(data_end).map_err(|_| DecodeError::IntegerOverflow)?,
    })
}

fn validate_payload_ranges(entries: &[NresEntry], limits: DecodeLimits) -> Result<(), NresError> {
    let mut ranges: Vec<(u32, Range<usize>)> = entries
        .iter()
        .map(|entry| (entry.id.0, entry.data_range.clone()))
        .collect();
    ranges.sort_by(|left, right| {
        left.1
            .start
            .cmp(&right.1.start)
            .then_with(|| left.1.end.cmp(&right.1.end))
    });
    let mut total_payload_bytes = 0_u64;
    for entry in entries {
        let payload_len = u64::from(entry.meta.data_size);
        enforce_limit(payload_len, limits.max_decoded_entry_bytes)?;
        total_payload_bytes = total_payload_bytes
            .checked_add(payload_len)
            .ok_or(DecodeError::IntegerOverflow)?;
        enforce_limit(total_payload_bytes, limits.max_total_decoded_bytes)?;
    }
    for pair in ranges.windows(2) {
        if pair[0].1.end > pair[1].1.start {
            return Err(NresError::EntryDataOverlap {
                first: pair[0].0,
                second: pair[1].0,
            });
        }
    }
    Ok(())
}

fn validate_names(entries: &[NresEntry]) -> Result<(), NresError> {
    for entry in entries {
        if entry.name_bytes().is_empty() {
            return Err(NresError::EmptyName { id: entry.id.0 });
        }
    }
    Ok(())
}

fn validate_lookup_order(entries: &[NresEntry]) -> Result<(), NresError> {
    let entry_count = saturating_u32_len(entries.len());
    let mut seen = vec![false; entries.len()];
    for (position, entry) in entries.iter().enumerate() {
        let index = entry.meta.sort_index;
        if index >= entry_count {
            return Err(NresError::SortIndexOutOfRange {
                position: saturating_u32_len(position),
                index,
                entry_count,
            });
        }
        let index_usize = usize::try_from(index).map_err(|_| DecodeError::IntegerOverflow)?;
        if seen[index_usize] {
            return Err(NresError::SortIndexDuplicate { index });
        }
        seen[index_usize] = true;
    }
    for (position, pair) in entries.windows(2).enumerate() {
        let left_index =
            usize::try_from(pair[0].meta.sort_index).map_err(|_| DecodeError::IntegerOverflow)?;
        let right_index =
            usize::try_from(pair[1].meta.sort_index).map_err(|_| DecodeError::IntegerOverflow)?;
        let left = entries[left_index].name_bytes();
        let right = entries[right_index].name_bytes();
        if cmp_ascii_casefold(left, right) == Ordering::Greater {
            return Err(NresError::SortOrderMismatch {
                position: saturating_u32_len(position.saturating_add(1)),
            });
        }
    }
    Ok(())
}

fn find_preserved_regions(
    bytes: &[u8],
    entries: &[NresEntry],
    directory_offset: u32,
    limits: DecodeLimits,
) -> Result<Vec<PreservedRegion>, NresError> {
    let mut ranges: Vec<Range<usize>> = entries
        .iter()
        .map(|entry| entry.data_range.clone())
        .collect();
    ranges.sort_by(|left, right| {
        left.start
            .cmp(&right.start)
            .then_with(|| left.end.cmp(&right.end))
    });

    let mut cursor = HEADER_LEN;
    let directory_offset =
        usize::try_from(directory_offset).map_err(|_| DecodeError::IntegerOverflow)?;
    let mut preserved = Vec::new();
    let mut preserved_bytes = 0_u64;
    for range in ranges {
        if cursor < range.start {
            preserved_bytes = preserved_bytes
                .checked_add(
                    u64::try_from(range.start - cursor)
                        .map_err(|_| DecodeError::IntegerOverflow)?,
                )
                .ok_or(DecodeError::IntegerOverflow)?;
            enforce_limit(preserved_bytes, limits.max_preserved_bytes)?;
            preserved.push(make_preserved_region(bytes, cursor..range.start)?);
        }
        cursor = cursor.max(range.end);
    }
    if cursor < directory_offset {
        preserved_bytes = preserved_bytes
            .checked_add(
                u64::try_from(directory_offset - cursor)
                    .map_err(|_| DecodeError::IntegerOverflow)?,
            )
            .ok_or(DecodeError::IntegerOverflow)?;
        enforce_limit(preserved_bytes, limits.max_preserved_bytes)?;
        preserved.push(make_preserved_region(bytes, cursor..directory_offset)?);
    }
    Ok(preserved)
}

fn enforce_limit(value: u64, limit: u64) -> Result<(), NresError> {
    if value > limit {
        return Err(DecodeError::LimitExceeded {
            count: value,
            limit,
        }
        .into());
    }
    Ok(())
}

fn make_preserved_region(bytes: &[u8], range: Range<usize>) -> Result<PreservedRegion, NresError> {
    let all_zero = bytes[range.clone()].iter().all(|byte| *byte == 0);
    Ok(PreservedRegion {
        range: u32::try_from(range.start).map_err(|_| DecodeError::IntegerOverflow)?
            ..u32::try_from(range.end).map_err(|_| DecodeError::IntegerOverflow)?,
        all_zero,
    })
}

fn build_sort_order(entries: &[NresEntry]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by(|left, right| {
        cmp_ascii_casefold(entries[*left].name_bytes(), entries[*right].name_bytes())
    });
    order
}

fn build_edit_sort_order(entries: &[EditableEntry]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by(|left, right| {
        cmp_ascii_casefold(
            editable_name_bytes(&entries[*left].name_raw),
            editable_name_bytes(&entries[*right].name_raw),
        )
    });
    order
}

fn build_edit_layout(document: &NresDocument) -> Result<Vec<EditableSegment>, NresError> {
    let mut ranges: Vec<(Range<usize>, usize)> = document
        .entries
        .iter()
        .enumerate()
        .map(|(index, entry)| (entry.data_range.clone(), index))
        .collect();
    ranges.sort_by(|(left, _), (right, _)| {
        left.start
            .cmp(&right.start)
            .then_with(|| left.end.cmp(&right.end))
    });

    let mut cursor = HEADER_LEN;
    let directory_offset = usize::try_from(document.header.directory_offset)
        .map_err(|_| DecodeError::IntegerOverflow)?;
    let mut layout = Vec::new();
    for (range, index) in ranges {
        if cursor < range.start {
            layout.push(EditableSegment::Preserved(
                document.bytes[cursor..range.start].to_vec(),
            ));
        }
        layout.push(EditableSegment::Entry(index));
        cursor = cursor.max(range.end);
    }
    if cursor < directory_offset {
        layout.push(EditableSegment::Preserved(
            document.bytes[cursor..directory_offset].to_vec(),
        ));
    }
    Ok(layout)
}

fn write_edit_archive_header_and_directory(
    out: &mut Vec<u8>,
    entries: &[EditableEntry],
    offsets: &[u32],
    sizes: &[u32],
) -> Result<(), NresError> {
    if offsets.len() != entries.len() || sizes.len() != entries.len() {
        return Err(DecodeError::IntegerOverflow.into());
    }
    let directory_len = ENTRY_LEN
        .checked_mul(entries.len())
        .ok_or(DecodeError::IntegerOverflow)?;
    out.len()
        .checked_add(directory_len)
        .ok_or(DecodeError::IntegerOverflow)?;

    let sort_order = build_edit_sort_order(entries);
    for (index, entry) in entries.iter().enumerate() {
        push_u32(out, entry.type_id);
        push_u32(out, entry.attr1);
        push_u32(out, entry.attr2);
        push_u32(out, sizes[index]);
        push_u32(out, entry.attr3);
        out.extend_from_slice(&entry.name_raw);
        push_u32(out, offsets[index]);
        push_u32(out, checked_u32_len(sort_order[index])?);
    }

    let total_size = checked_u32_len(out.len())?;
    out[0..4].copy_from_slice(b"NRes");
    out[4..8].copy_from_slice(&VERSION_0100.to_le_bytes());
    out[8..12].copy_from_slice(&checked_u32_len(entries.len())?.to_le_bytes());
    out[12..16].copy_from_slice(&total_size.to_le_bytes());
    Ok(())
}

fn editable_name_bytes(raw: &[u8; NAME_LEN]) -> &[u8] {
    let len = name_len(raw).unwrap_or(NAME_LEN);
    &raw[..len]
}

fn cmp_ascii_casefold(left: &[u8], right: &[u8]) -> Ordering {
    let left_key = lookup_key(left);
    let right_key = lookup_key(right);
    left_key.0.cmp(&right_key.0)
}

fn lookup_key(bytes: &[u8]) -> LookupKey {
    ascii_lookup_key(bytes)
}

fn name_len(raw: &[u8; NAME_LEN]) -> Option<usize> {
    raw.iter().position(|byte| *byte == 0)
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn checked_u32_len(len: usize) -> Result<u32, NresError> {
    u32::try_from(len).map_err(|_| NresError::Binary(DecodeError::IntegerOverflow))
}

fn saturating_u32_len(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
}

fn authoring_name_raw(name: &[u8]) -> Result<[u8; NAME_LEN], NresError> {
    if let Some(offset) = name.iter().position(|byte| *byte == 0) {
        return Err(NresError::AuthoringNameContainsNul { offset });
    }
    let max = NAME_LEN - 1;
    if name.len() > max {
        return Err(NresError::AuthoringNameTooLong {
            len: name.len(),
            max,
        });
    }
    let mut raw = [0; NAME_LEN];
    raw[..name.len()].copy_from_slice(name);
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    #[derive(Clone, Copy)]
    struct SyntheticEntry<'a> {
        type_id: u32,
        attr1: u32,
        attr2: u32,
        attr3: u32,
        name: &'a str,
        payload: &'a [u8],
    }

    #[test]
    fn parses_minimal_empty_archive() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"NRes");
        push_u32(&mut bytes, VERSION_0100);
        push_u32(&mut bytes, 0);
        push_u32(&mut bytes, HEADER_LEN_U32);

        let doc = decode(arc(bytes.clone()), ReadProfile::Strict).expect("empty nres");

        assert_eq!(doc.header().entry_count, 0);
        assert_eq!(doc.header().directory_offset, HEADER_LEN_U32);
        assert!(doc.entries().is_empty());
        assert!(doc.preserved_regions().is_empty());
        assert_eq!(doc.encode(WriteProfile::Lossless), bytes);
    }

    #[test]
    fn one_entry_archive_uses_8_byte_alignment() {
        let bytes = build_archive(&[SyntheticEntry {
            type_id: 7,
            attr1: 1,
            attr2: 2,
            attr3: 3,
            name: "one",
            payload: b"x",
        }]);
        let doc = decode(arc(bytes), ReadProfile::Strict).expect("one entry nres");
        let entry = doc.entry(EntryId(0)).expect("entry");

        assert_eq!(doc.entry_count(), 1);
        assert_eq!(entry.data_range().start, HEADER_LEN);
        assert_eq!(entry.data_range().end, HEADER_LEN + 1);
        assert_eq!(doc.header().directory_offset % 8, 0);
        assert_eq!(doc.payload(EntryId(0)).expect("payload"), b"x");
    }

    #[test]
    fn rejects_invalid_magic() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"BAD!");
        push_u32(&mut bytes, VERSION_0100);
        push_u32(&mut bytes, 0);
        push_u32(&mut bytes, HEADER_LEN_U32);

        assert!(matches!(
            decode(arc(bytes), ReadProfile::Strict),
            Err(NresError::InvalidMagic { got }) if got == *b"BAD!"
        ));
    }

    #[test]
    fn rejects_unsupported_version() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"NRes");
        push_u32(&mut bytes, VERSION_0100 + 1);
        push_u32(&mut bytes, 0);
        push_u32(&mut bytes, HEADER_LEN_U32);

        assert!(matches!(
            decode(arc(bytes), ReadProfile::Strict),
            Err(NresError::UnsupportedVersion { got }) if got == VERSION_0100 + 1
        ));
    }

    #[test]
    fn rejects_negative_entry_count() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"NRes");
        push_u32(&mut bytes, VERSION_0100);
        bytes.extend_from_slice(&(-1_i32).to_le_bytes());
        push_u32(&mut bytes, HEADER_LEN_U32);

        assert!(matches!(
            decode(arc(bytes), ReadProfile::Strict),
            Err(NresError::InvalidEntryCount { got }) if got == -1
        ));
    }

    #[test]
    fn rejects_directory_size_before_allocation() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"NRes");
        push_u32(&mut bytes, VERSION_0100);
        push_u32(&mut bytes, i32::MAX.cast_unsigned());
        push_u32(&mut bytes, HEADER_LEN_U32);

        assert!(matches!(
            decode(arc(bytes), ReadProfile::Strict),
            Err(NresError::Binary(DecodeError::LimitExceeded {
                count,
                limit
            })) if count == i32::MAX as u64 && limit == DecodeLimits::default().max_entries as u64
        ));
    }

    #[test]
    fn rejects_total_size_mismatch() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"NRes");
        push_u32(&mut bytes, VERSION_0100);
        push_u32(&mut bytes, 0);
        push_u32(&mut bytes, HEADER_LEN_U32 + 1);

        assert!(matches!(
            decode(arc(bytes), ReadProfile::Strict),
            Err(NresError::TotalSizeMismatch { header, actual })
                if header == HEADER_LEN_U32 + 1 && actual == HEADER_LEN as u64
        ));
    }

    #[test]
    fn rejects_directory_before_header() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"NRes");
        push_u32(&mut bytes, VERSION_0100);
        push_u32(&mut bytes, 1);
        push_u32(&mut bytes, ENTRY_LEN as u32);
        bytes.resize(ENTRY_LEN, 0);

        assert!(matches!(
            decode(arc(bytes), ReadProfile::Strict),
            Err(NresError::DirectoryOutOfBounds { offset, .. }) if offset == 0
        ));
    }

    #[test]
    fn rejects_payload_before_data_region() {
        let mut bytes = build_archive(&[SyntheticEntry {
            type_id: 1,
            attr1: 0,
            attr2: 0,
            attr3: 0,
            name: "one",
            payload: b"x",
        }]);
        let directory_offset = bytes.len() - ENTRY_LEN;
        bytes[directory_offset + 56..directory_offset + 60].copy_from_slice(&15_u32.to_le_bytes());

        assert!(matches!(
            decode(arc(bytes), ReadProfile::Strict),
            Err(NresError::EntryDataOutOfBounds { offset, .. }) if offset == 15
        ));
    }

    #[test]
    fn rejects_payload_crossing_directory() {
        let mut bytes = build_archive(&[SyntheticEntry {
            type_id: 1,
            attr1: 0,
            attr2: 0,
            attr3: 0,
            name: "one",
            payload: b"x",
        }]);
        let directory_offset = bytes.len() - ENTRY_LEN;
        let offset = u32::from_le_bytes(
            bytes[directory_offset + 56..directory_offset + 60]
                .try_into()
                .expect("offset field"),
        );
        let size = u32::try_from(directory_offset).expect("directory offset") - offset + 1;
        bytes[directory_offset + 12..directory_offset + 16].copy_from_slice(&size.to_le_bytes());

        assert!(matches!(
            decode(arc(bytes), ReadProfile::Strict),
            Err(NresError::EntryDataOutOfBounds {
                directory_offset: got_directory,
                ..
            }) if got_directory == u32::try_from(directory_offset).expect("directory offset")
        ));
    }

    #[test]
    fn rejects_name_without_nul_terminator() {
        let mut bytes = build_archive(&[SyntheticEntry {
            type_id: 1,
            attr1: 0,
            attr2: 0,
            attr3: 0,
            name: "one",
            payload: b"x",
        }]);
        let directory_offset = bytes.len() - ENTRY_LEN;
        bytes[directory_offset + 20..directory_offset + 56].fill(b'A');

        assert!(matches!(
            decode(arc(bytes), ReadProfile::Strict),
            Err(NresError::MissingNameTerminator { id }) if id == 0
        ));
    }

    #[test]
    fn preserves_name_bytes_after_nul() {
        let mut bytes = build_archive(&[SyntheticEntry {
            type_id: 1,
            attr1: 0,
            attr2: 0,
            attr3: 0,
            name: "one",
            payload: b"x",
        }]);
        let directory_offset = bytes.len() - ENTRY_LEN;
        bytes[directory_offset + 20..directory_offset + 29].copy_from_slice(b"one\0TAIL!");

        let doc = decode(arc(bytes.clone()), ReadProfile::Strict).expect("nres");
        let entry = doc.entry(EntryId(0)).expect("entry");

        assert_eq!(entry.name_bytes(), b"one");
        assert_eq!(doc.encode(WriteProfile::Lossless), bytes);
        assert_eq!(doc.encode(WriteProfile::CanonicalCompact), bytes);
    }

    #[test]
    fn rejects_sort_index_out_of_range() {
        let mut bytes = build_archive(&[SyntheticEntry {
            type_id: 1,
            attr1: 0,
            attr2: 0,
            attr3: 0,
            name: "one",
            payload: b"x",
        }]);
        let directory_offset = bytes.len() - ENTRY_LEN;
        bytes[directory_offset + 60..directory_offset + 64].copy_from_slice(&1_u32.to_le_bytes());

        assert!(matches!(
            decode(arc(bytes), ReadProfile::Strict),
            Err(NresError::SortIndexOutOfRange {
                position: 0,
                index: 1,
                entry_count: 1,
            })
        ));
    }

    #[test]
    fn rejects_duplicate_sort_mapping() {
        let mut bytes = build_archive(&[
            SyntheticEntry {
                type_id: 1,
                attr1: 0,
                attr2: 0,
                attr3: 0,
                name: "a",
                payload: b"a",
            },
            SyntheticEntry {
                type_id: 2,
                attr1: 0,
                attr2: 0,
                attr3: 0,
                name: "b",
                payload: b"b",
            },
        ]);
        let directory_offset = bytes.len() - ENTRY_LEN * 2;
        bytes[directory_offset + 60..directory_offset + 64].copy_from_slice(&0_u32.to_le_bytes());
        bytes[directory_offset + ENTRY_LEN + 60..directory_offset + ENTRY_LEN + 64]
            .copy_from_slice(&0_u32.to_le_bytes());

        assert!(matches!(
            decode(arc(bytes), ReadProfile::Strict),
            Err(NresError::SortIndexDuplicate { index }) if index == 0
        ));
    }

    #[test]
    fn binary_lookup_returns_original_entry_index() {
        let bytes = build_archive(&[
            SyntheticEntry {
                type_id: 1,
                attr1: 0,
                attr2: 0,
                attr3: 0,
                name: "Zulu",
                payload: b"z",
            },
            SyntheticEntry {
                type_id: 2,
                attr1: 0,
                attr2: 0,
                attr3: 0,
                name: "alpha",
                payload: b"a",
            },
            SyntheticEntry {
                type_id: 3,
                attr1: 0,
                attr2: 0,
                attr3: 0,
                name: "Mike",
                payload: b"m",
            },
        ]);
        let doc = decode(arc(bytes), ReadProfile::Strict).expect("nres");

        assert!(doc.lookup_order_valid());
        assert_eq!(doc.find("alpha"), Some(EntryId(1)));
        assert_eq!(doc.find("Mike"), Some(EntryId(2)));
        assert_eq!(doc.find("Zulu"), Some(EntryId(0)));
    }

    #[test]
    fn compatible_profile_uses_linear_fallback_for_broken_mapping() {
        let mut bytes = build_archive(&[
            SyntheticEntry {
                type_id: 1,
                attr1: 0,
                attr2: 0,
                attr3: 0,
                name: "b",
                payload: b"b",
            },
            SyntheticEntry {
                type_id: 2,
                attr1: 0,
                attr2: 0,
                attr3: 0,
                name: "a",
                payload: b"a",
            },
        ]);
        let directory_offset = bytes.len() - ENTRY_LEN * 2;
        bytes[directory_offset + 60..directory_offset + 64].copy_from_slice(&0_u32.to_le_bytes());
        bytes[directory_offset + ENTRY_LEN + 60..directory_offset + ENTRY_LEN + 64]
            .copy_from_slice(&0_u32.to_le_bytes());

        let doc = decode(arc(bytes), ReadProfile::Compatible).expect("compatible nres");

        assert!(!doc.lookup_order_valid());
        assert_eq!(doc.find("A"), Some(EntryId(1)));
        assert_eq!(doc.payload(EntryId(1)).expect("payload"), b"a");
    }

    #[test]
    fn lookup_is_ascii_case_insensitive() {
        let bytes = build_archive(&[SyntheticEntry {
            type_id: 1,
            attr1: 0,
            attr2: 0,
            attr3: 0,
            name: "MiXeD",
            payload: b"x",
        }]);
        let doc = decode(arc(bytes), ReadProfile::Strict).expect("nres");

        assert_eq!(doc.find("mixed"), Some(EntryId(0)));
        assert_eq!(doc.find("MIXED"), Some(EntryId(0)));
    }

    #[test]
    fn parses_synthetic_archive_and_finds_names() {
        let bytes = build_archive(&[
            SyntheticEntry {
                type_id: 1,
                attr1: 10,
                attr2: 20,
                attr3: 30,
                name: "Zulu",
                payload: b"z",
            },
            SyntheticEntry {
                type_id: 2,
                attr1: 11,
                attr2: 21,
                attr3: 31,
                name: "alpha",
                payload: b"aaaa",
            },
        ]);
        let doc = decode(arc(bytes.clone()), ReadProfile::Strict).expect("synthetic nres");

        assert_eq!(doc.entry_count(), 2);
        assert_eq!(doc.find("ALPHA"), Some(EntryId(1)));
        assert_eq!(doc.find("zulu"), Some(EntryId(0)));
        assert_eq!(
            doc.payload(EntryId(1)).expect("payload"),
            b"aaaa".as_slice()
        );
        assert_eq!(doc.encode(WriteProfile::Lossless), bytes);
        assert_eq!(doc.encode(WriteProfile::CanonicalCompact), bytes);
    }

    #[test]
    fn strict_rejects_unsorted_lookup_table() {
        let mut bytes = build_archive(&[
            SyntheticEntry {
                type_id: 1,
                attr1: 0,
                attr2: 0,
                attr3: 0,
                name: "b",
                payload: b"b",
            },
            SyntheticEntry {
                type_id: 2,
                attr1: 0,
                attr2: 0,
                attr3: 0,
                name: "a",
                payload: b"a",
            },
        ]);
        let directory_offset = usize::try_from(u32::from_le_bytes(
            bytes[12..16].try_into().expect("total size field"),
        ))
        .expect("total size")
            - ENTRY_LEN * 2;
        bytes[directory_offset + 60..directory_offset + 64].copy_from_slice(&0_u32.to_le_bytes());
        bytes[directory_offset + ENTRY_LEN + 60..directory_offset + ENTRY_LEN + 64]
            .copy_from_slice(&1_u32.to_le_bytes());

        assert!(matches!(
            decode(arc(bytes), ReadProfile::Strict),
            Err(NresError::SortOrderMismatch { position: 1 })
        ));
    }

    #[test]
    fn decode_rejects_entry_count_above_limit() {
        let bytes = build_archive(&[
            SyntheticEntry {
                type_id: 1,
                attr1: 0,
                attr2: 0,
                attr3: 0,
                name: "a",
                payload: b"a",
            },
            SyntheticEntry {
                type_id: 2,
                attr1: 0,
                attr2: 0,
                attr3: 0,
                name: "b",
                payload: b"b",
            },
        ]);

        assert!(matches!(
            decode_with_limits(
                arc(bytes),
                ReadProfile::Strict,
                DecodeLimits {
                    max_entries: 1,
                    ..DecodeLimits::default()
                }
            ),
            Err(NresError::Binary(DecodeError::LimitExceeded { count: 2, limit: 1 }))
        ));
    }

    #[test]
    fn decode_rejects_input_bytes_above_limit() {
        let bytes = build_archive(&[SyntheticEntry {
            type_id: 1,
            attr1: 0,
            attr2: 0,
            attr3: 0,
            name: "payload",
            payload: b"data",
        }]);
        let exact_size = u64::try_from(bytes.len()).expect("archive size");

        assert!(matches!(
            decode_with_limits(
                arc(bytes),
                ReadProfile::Strict,
                DecodeLimits {
                    max_input_bytes: exact_size - 1,
                    ..DecodeLimits::default()
                }
            ),
            Err(NresError::Binary(DecodeError::LimitExceeded {
                count,
                limit
            })) if count == exact_size && limit == exact_size - 1
        ));
    }

    #[test]
    fn decode_rejects_preserved_bytes_above_limit() {
        let bytes = build_archive_with_nonzero_prefix_gap(&[SyntheticEntry {
            type_id: 1,
            attr1: 0,
            attr2: 0,
            attr3: 0,
            name: "payload",
            payload: b"data",
        }]);

        assert!(matches!(
            decode_with_limits(
                arc(bytes),
                ReadProfile::Strict,
                DecodeLimits {
                    max_preserved_bytes: 4,
                    ..DecodeLimits::default()
                }
            ),
            Err(NresError::Binary(DecodeError::LimitExceeded { .. }))
        ));
    }

    #[test]
    fn rejects_overlapping_payloads() {
        let mut bytes = build_archive(&[
            SyntheticEntry {
                type_id: 1,
                attr1: 0,
                attr2: 0,
                attr3: 0,
                name: "one",
                payload: b"1111",
            },
            SyntheticEntry {
                type_id: 2,
                attr1: 0,
                attr2: 0,
                attr3: 0,
                name: "two",
                payload: b"2222",
            },
        ]);
        let directory_offset = bytes.len() - ENTRY_LEN * 2;
        let first_offset = u32::from_le_bytes(
            bytes[directory_offset + 56..directory_offset + 60]
                .try_into()
                .expect("offset field"),
        );
        bytes[directory_offset + ENTRY_LEN + 56..directory_offset + ENTRY_LEN + 60]
            .copy_from_slice(&(first_offset + 1).to_le_bytes());

        assert!(matches!(
            decode(arc(bytes), ReadProfile::Strict),
            Err(NresError::EntryDataOverlap { .. })
        ));
    }

    #[test]
    fn preserves_nonzero_unindexed_region() {
        let bytes = build_archive_with_nonzero_prefix_gap(&[SyntheticEntry {
            type_id: 1,
            attr1: 0,
            attr2: 0,
            attr3: 0,
            name: "payload",
            payload: b"data",
        }]);

        let doc = decode(arc(bytes.clone()), ReadProfile::Strict).expect("nres");
        assert!(doc.has_nonzero_preserved_region());
        assert_eq!(doc.encode(WriteProfile::Lossless), bytes);
        assert_ne!(doc.encode(WriteProfile::CanonicalCompact), bytes);
    }

    #[test]
    fn canonical_compact_roundtrip_preserves_entry_semantics() {
        let bytes = build_archive_with_nonzero_prefix_gap(&[
            SyntheticEntry {
                type_id: 7,
                attr1: 10,
                attr2: 20,
                attr3: 30,
                name: "zeta",
                payload: b"zz",
            },
            SyntheticEntry {
                type_id: 9,
                attr1: 11,
                attr2: 21,
                attr3: 31,
                name: "alpha",
                payload: b"aaaa",
            },
        ]);

        let original = decode(arc(bytes), ReadProfile::Strict).expect("original");
        let compact = decode(
            arc(original.encode(WriteProfile::CanonicalCompact)),
            ReadProfile::Strict,
        )
        .expect("compact");

        assert_eq!(compact.entry_count(), original.entry_count());
        assert!(!compact.has_nonzero_preserved_region());
        for original_entry in original.entries() {
            let compact_id = compact
                .find_bytes(original_entry.name_bytes())
                .expect("compact lookup");
            let compact_entry = compact.entry(compact_id).expect("compact entry");
            let original_meta = original_entry.meta();
            let compact_meta = compact_entry.meta();
            assert_eq!(compact_entry.name_bytes(), original_entry.name_bytes());
            assert_eq!(compact_meta.type_id, original_meta.type_id);
            assert_eq!(compact_meta.attr1, original_meta.attr1);
            assert_eq!(compact_meta.attr2, original_meta.attr2);
            assert_eq!(compact_meta.attr3, original_meta.attr3);
            assert_eq!(compact_meta.data_size, original_meta.data_size);
            assert_eq!(
                compact.payload(compact_id).expect("compact payload"),
                original
                    .payload(original_entry.id())
                    .expect("original payload")
            );
        }
    }

    #[test]
    fn editor_payload_update_rewrites_offsets_and_size() {
        let bytes = build_archive(&[
            SyntheticEntry {
                type_id: 1,
                attr1: 10,
                attr2: 20,
                attr3: 30,
                name: "first",
                payload: b"a",
            },
            SyntheticEntry {
                type_id: 2,
                attr1: 11,
                attr2: 21,
                attr3: 31,
                name: "second",
                payload: b"bb",
            },
        ]);
        let original = decode(arc(bytes), ReadProfile::Strict).expect("original");
        let mut editor = original.editor().expect("editor");

        editor
            .set_payload(EntryId(0), b"replacement".to_vec())
            .expect("set payload");
        let edited = decode(
            arc(editor
                .encode_with_profile(WriteProfile::CanonicalCompact)
                .expect("encode")),
            ReadProfile::Strict,
        )
        .expect("edited");
        let first = edited.entry(EntryId(0)).expect("first");
        let second = edited.entry(EntryId(1)).expect("second");

        assert_eq!(
            edited.payload(EntryId(0)).expect("first payload"),
            b"replacement"
        );
        assert_eq!(edited.payload(EntryId(1)).expect("second payload"), b"bb");
        assert_eq!(first.meta().data_size, 11);
        assert_eq!(first.meta().data_offset, HEADER_LEN_U32);
        assert_eq!(second.meta().data_offset % 8, 0);
        assert!(second.meta().data_offset > first.meta().data_offset + first.meta().data_size);
    }

    #[test]
    fn editor_payload_update_preserves_nonzero_unindexed_region_by_default() {
        let bytes = build_archive_with_nonzero_prefix_gap(&[
            SyntheticEntry {
                type_id: 1,
                attr1: 0,
                attr2: 0,
                attr3: 0,
                name: "first",
                payload: b"one",
            },
            SyntheticEntry {
                type_id: 2,
                attr1: 0,
                attr2: 0,
                attr3: 0,
                name: "second",
                payload: b"two",
            },
        ]);
        let original = decode(arc(bytes), ReadProfile::Strict).expect("original");
        let marker = original
            .preserved_regions()
            .iter()
            .find(|region| !region.all_zero)
            .expect("nonzero preserved region")
            .range
            .clone();
        let marker = original.bytes[usize::try_from(marker.start).expect("start")
            ..usize::try_from(marker.end).expect("end")]
            .to_vec();
        let mut editor = original.editor().expect("editor");

        editor
            .set_payload(EntryId(0), b"replacement".to_vec())
            .expect("set payload");
        let edited_bytes = editor.encode().expect("encode");
        let edited = decode(arc(edited_bytes.clone()), ReadProfile::Strict).expect("edited");

        assert_eq!(
            edited.payload(EntryId(0)).expect("first payload"),
            b"replacement"
        );
        assert!(edited.has_nonzero_preserved_region());
        assert!(edited_bytes
            .windows(marker.len())
            .any(|window| window == marker));

        let compact = decode(
            arc(editor
                .encode_with_profile(WriteProfile::CanonicalCompact)
                .expect("compact")),
            ReadProfile::Strict,
        )
        .expect("compact");
        assert!(!compact.has_nonzero_preserved_region());
    }

    #[test]
    fn editor_rename_rebuilds_search_mapping() {
        let bytes = build_archive(&[
            SyntheticEntry {
                type_id: 1,
                attr1: 0,
                attr2: 0,
                attr3: 0,
                name: "zeta",
                payload: b"z",
            },
            SyntheticEntry {
                type_id: 2,
                attr1: 0,
                attr2: 0,
                attr3: 0,
                name: "middle",
                payload: b"m",
            },
        ]);
        let original = decode(arc(bytes), ReadProfile::Strict).expect("original");
        let mut editor = original.editor().expect("editor");

        editor.rename(EntryId(0), b"alpha").expect("rename");
        let edited =
            decode(arc(editor.encode().expect("encode")), ReadProfile::Strict).expect("edited");

        assert!(edited.lookup_order_valid());
        assert_eq!(edited.find("alpha"), Some(EntryId(0)));
        assert_eq!(edited.find("zeta"), None);
        assert_eq!(edited.find("middle"), Some(EntryId(1)));
        assert_eq!(
            edited.entry(EntryId(0)).expect("entry").name_bytes(),
            b"alpha"
        );
    }

    #[test]
    fn editor_rejects_invalid_authoring_names() {
        let bytes = build_archive(&[SyntheticEntry {
            type_id: 1,
            attr1: 0,
            attr2: 0,
            attr3: 0,
            name: "one",
            payload: b"x",
        }]);
        let original = decode(arc(bytes), ReadProfile::Strict).expect("original");
        let mut editor = original.editor().expect("editor");

        assert!(matches!(
            editor.rename(EntryId(0), [b'A'; NAME_LEN]),
            Err(NresError::AuthoringNameTooLong { len, max })
                if len == NAME_LEN && max == NAME_LEN - 1
        ));
        assert!(matches!(
            editor.rename(EntryId(0), b"bad\0name"),
            Err(NresError::AuthoringNameContainsNul { offset }) if offset == 3
        ));

        let encoded = editor.encode().expect("encode");
        let unchanged = decode(arc(encoded), ReadProfile::Strict).expect("unchanged");
        assert_eq!(
            unchanged.entry(EntryId(0)).expect("entry").name_bytes(),
            b"one"
        );
    }

    #[test]
    fn rejects_empty_names_and_resolves_duplicates_to_first_entry() {
        let empty_name = build_archive(&[SyntheticEntry {
            type_id: 1,
            attr1: 0,
            attr2: 0,
            attr3: 0,
            name: "",
            payload: b"x",
        }]);
        assert!(matches!(
            decode(arc(empty_name), ReadProfile::Strict),
            Err(NresError::EmptyName { id: 0 })
        ));

        let duplicate_names = build_archive(&[
            SyntheticEntry {
                type_id: 1,
                attr1: 0,
                attr2: 0,
                attr3: 0,
                name: "duplicate",
                payload: b"a",
            },
            SyntheticEntry {
                type_id: 2,
                attr1: 0,
                attr2: 0,
                attr3: 0,
                name: "DUPLICATE",
                payload: b"b",
            },
        ]);
        let doc = decode(arc(duplicate_names), ReadProfile::Strict).expect("duplicates");
        assert_eq!(doc.find("duplicate"), Some(EntryId(0)));
        assert_eq!(doc.payload(EntryId(0)).expect("first duplicate"), b"a");
        assert_eq!(doc.payload(EntryId(1)).expect("second duplicate"), b"b");
    }

    #[test]
    fn generated_archives_preserve_lossless_and_canonical_semantics() {
        let cases = [
            vec![SyntheticEntry {
                type_id: 1,
                attr1: 10,
                attr2: 20,
                attr3: 30,
                name: "single.bin",
                payload: b"x",
            }],
            vec![
                SyntheticEntry {
                    type_id: 2,
                    attr1: 1,
                    attr2: 2,
                    attr3: 3,
                    name: "zeta.bin",
                    payload: b"zzzz",
                },
                SyntheticEntry {
                    type_id: 3,
                    attr1: 4,
                    attr2: 5,
                    attr3: 6,
                    name: "Alpha.bin",
                    payload: b"a",
                },
                SyntheticEntry {
                    type_id: 4,
                    attr1: 7,
                    attr2: 8,
                    attr3: 9,
                    name: "middle.bin",
                    payload: b"middle",
                },
            ],
        ];

        for entries in cases {
            let bytes = build_archive(&entries);
            let doc = decode(arc(bytes.clone()), ReadProfile::Strict).expect("generated nres");
            assert_eq!(doc.encode(WriteProfile::Lossless), bytes);

            let compact = doc.encode(WriteProfile::CanonicalCompact);
            let compact_doc = decode(arc(compact), ReadProfile::Strict).expect("compact nres");
            assert_eq!(compact_doc.entry_count(), doc.entry_count());
            for original in doc.entries() {
                let compact_id = compact_doc
                    .find_bytes(original.name_bytes())
                    .expect("compact entry");
                let compact_entry = compact_doc.entry(compact_id).expect("compact meta");
                assert_eq!(compact_entry.meta().type_id, original.meta().type_id);
                assert_eq!(compact_entry.meta().attr1, original.meta().attr1);
                assert_eq!(compact_entry.meta().attr2, original.meta().attr2);
                assert_eq!(compact_entry.meta().attr3, original.meta().attr3);
                assert_eq!(
                    compact_doc.payload(compact_id).expect("compact payload"),
                    doc.payload(original.id()).expect("original payload")
                );
            }
        }
    }

    #[test]
    fn generated_editor_updates_roundtrip() {
        for count in 1..5usize {
            let entries = (0..count)
                .map(|idx| SyntheticEntry {
                    type_id: u32::try_from(idx + 1).expect("type id"),
                    attr1: u32::try_from(idx).expect("attr1"),
                    attr2: u32::try_from(idx * 2).expect("attr2"),
                    attr3: u32::try_from(idx * 3).expect("attr3"),
                    name: ["a.bin", "b.bin", "c.bin", "d.bin"][idx],
                    payload: ["a", "bb", "ccc", "dddd"][idx].as_bytes(),
                })
                .collect::<Vec<_>>();
            let doc = decode(arc(build_archive(&entries)), ReadProfile::Strict).expect("nres");
            let mut editor = doc.editor().expect("editor");
            editor
                .set_payload(EntryId(0), format!("replacement-{count}").into_bytes())
                .expect("set payload");
            editor
                .rename(EntryId(0), format!("renamed-{count}.bin").as_bytes())
                .expect("rename");

            let edited =
                decode(arc(editor.encode().expect("encode")), ReadProfile::Strict).expect("edited");
            assert_eq!(edited.entry_count(), count);
            let renamed = edited
                .find(&format!("RENAMED-{count}.BIN"))
                .expect("renamed");
            assert_eq!(renamed, EntryId(0));
            assert_eq!(
                edited.payload(EntryId(0)).expect("payload"),
                format!("replacement-{count}").as_bytes()
            );
        }
    }

    #[test]
    fn arbitrary_small_inputs_do_not_panic_or_overallocate() {
        for len in 0..160usize {
            let mut bytes = vec![0u8; len];
            if len >= 4 {
                bytes[0..4].copy_from_slice(b"NRes");
            }
            if len >= 8 {
                bytes[4..8].copy_from_slice(&VERSION_0100.to_le_bytes());
            }
            if len >= 12 {
                bytes[8..12].copy_from_slice(&u32::try_from(len % 4).expect("count").to_le_bytes());
            }
            if len >= 16 {
                bytes[12..16].copy_from_slice(&u32::try_from(len).expect("len").to_le_bytes());
            }

            let strict =
                std::panic::catch_unwind(|| decode(arc(bytes.clone()), ReadProfile::Strict));
            let compatible =
                std::panic::catch_unwind(|| decode(arc(bytes.clone()), ReadProfile::Compatible));
            assert!(strict.is_ok());
            assert!(compatible.is_ok());
        }
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_corpora_nres_roundtrip_gates() {
        let part1 = corpus_gate("IS", 120, 6_804).expect("part 1 NRes gate");
        let part2 = corpus_gate("IS2", 134, 8_171).expect("part 2 NRes gate");

        assert!(!part1.has_nonzero_preserved_region);
        assert!(
            part2.has_nonzero_preserved_region,
            "part 2 must keep the known non-zero unindexed NRes regression case"
        );
    }

    #[derive(Clone, Copy, Debug, Default)]
    struct CorpusGateResult {
        has_nonzero_preserved_region: bool,
    }

    fn corpus_gate(
        name: &str,
        expected_files: usize,
        expected_entries: usize,
    ) -> Result<CorpusGateResult, String> {
        let variable = match name {
            "IS" => "FPARKAN_CORPUS_PART1_ROOT",
            "IS2" => "FPARKAN_CORPUS_PART2_ROOT",
            _ => return Err(format!("unknown licensed corpus part: {name}")),
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
        let mut files = Vec::new();
        collect_nres_files(&root, &mut files).map_err(|err| err.to_string())?;
        files.sort();

        let mut total_entries = 0usize;
        let mut has_nonzero_preserved_region = false;
        for path in &files {
            let bytes = fs::read(path).map_err(|err| format!("{}: {err}", path.display()))?;
            let doc = decode(arc(bytes.clone()), ReadProfile::Compatible)
                .map_err(|err| format!("{}: {err}", path.display()))?;
            total_entries = total_entries
                .checked_add(doc.entry_count())
                .ok_or_else(|| "entry count overflow".to_string())?;
            if doc.has_nonzero_preserved_region() {
                has_nonzero_preserved_region = true;
            }
            for entry in doc.entries() {
                let id = doc
                    .find_bytes(entry.name_bytes())
                    .ok_or_else(|| format!("lookup failed: {}", path.display()))?;
                let found = doc
                    .entry(id)
                    .ok_or_else(|| format!("lookup returned invalid id: {}", path.display()))?;
                if cmp_ascii_casefold(found.name_bytes(), entry.name_bytes()) != Ordering::Equal {
                    return Err(format!("lookup mismatch: {}", path.display()));
                }
                let _payload = doc
                    .payload(entry.id())
                    .map_err(|err| format!("{}: {err}", path.display()))?;
            }
            if doc.encode(WriteProfile::Lossless) != bytes {
                return Err(format!("lossless roundtrip mismatch: {}", path.display()));
            }
        }

        if files.len() != expected_files {
            return Err(format!(
                "{name}: expected {expected_files} NRes files, got {}",
                files.len()
            ));
        }
        if total_entries != expected_entries {
            return Err(format!(
                "{name}: expected {expected_entries} NRes entries, got {total_entries}"
            ));
        }
        Ok(CorpusGateResult {
            has_nonzero_preserved_region,
        })
    }

    fn collect_nres_files(root: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
        for entry in fs::read_dir(root)? {
            let path = entry?.path();
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with('.'))
            {
                continue;
            }
            if path.is_dir() {
                collect_nres_files(&path, out)?;
                continue;
            }
            if path.is_file() {
                let bytes = fs::read(&path)?;
                if bytes.starts_with(b"NRes") {
                    out.push(path);
                }
            }
        }
        Ok(())
    }

    fn build_archive(entries: &[SyntheticEntry<'_>]) -> Vec<u8> {
        let mut out = vec![0; HEADER_LEN];
        let mut offsets = Vec::with_capacity(entries.len());
        for entry in entries {
            offsets.push(u32::try_from(out.len()).expect("offset"));
            out.extend_from_slice(entry.payload);
            let padding = (8 - (out.len() % 8)) % 8;
            out.resize(out.len() + padding, 0);
        }
        let mut order: Vec<usize> = (0..entries.len()).collect();
        order.sort_by(|left, right| {
            cmp_ascii_casefold(
                entries[*left].name.as_bytes(),
                entries[*right].name.as_bytes(),
            )
        });
        for (index, entry) in entries.iter().enumerate() {
            push_u32(&mut out, entry.type_id);
            push_u32(&mut out, entry.attr1);
            push_u32(&mut out, entry.attr2);
            push_u32(
                &mut out,
                u32::try_from(entry.payload.len()).expect("payload size"),
            );
            push_u32(&mut out, entry.attr3);
            let mut name = [0; NAME_LEN];
            let name_bytes = entry.name.as_bytes();
            name[..name_bytes.len()].copy_from_slice(name_bytes);
            out.extend_from_slice(&name);
            push_u32(&mut out, offsets[index]);
            push_u32(&mut out, u32::try_from(order[index]).expect("sort index"));
        }
        out[0..4].copy_from_slice(b"NRes");
        out[4..8].copy_from_slice(&VERSION_0100.to_le_bytes());
        out[8..12].copy_from_slice(
            &u32::try_from(entries.len())
                .expect("entry count")
                .to_le_bytes(),
        );
        let total_size = u32::try_from(out.len()).expect("total size");
        out[12..16].copy_from_slice(&total_size.to_le_bytes());
        out
    }

    fn build_archive_with_nonzero_prefix_gap(entries: &[SyntheticEntry<'_>]) -> Vec<u8> {
        let mut bytes = build_archive(entries);
        let directory_offset = bytes.len() - ENTRY_LEN * entries.len();
        bytes.splice(HEADER_LEN..HEADER_LEN, [0xAA, 0xBB, 0xCC, 0xDD]);
        let total = u32::try_from(bytes.len()).expect("total size");
        bytes[12..16].copy_from_slice(&total.to_le_bytes());
        for entry_index in 0..entries.len() {
            let field = directory_offset + 4 + entry_index * ENTRY_LEN + 56;
            let offset =
                u32::from_le_bytes(bytes[field..field + 4].try_into().expect("shifted offset"));
            bytes[field..field + 4].copy_from_slice(&(offset + 4).to_le_bytes());
        }
        bytes
    }

    fn arc(bytes: Vec<u8>) -> Arc<[u8]> {
        Arc::from(bytes.into_boxed_slice())
    }
}
