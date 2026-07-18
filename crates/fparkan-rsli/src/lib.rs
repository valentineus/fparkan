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
//! Stage-1 `RsLi` archive contract.

use fparkan_binary::DecodeError;
use std::fmt;
use std::io::Read;
use std::sync::Arc;

/// Read profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadProfile {
    /// Reject compatibility quirks.
    Strict,
    /// Accept registered retail compatibility quirks.
    Compatible,
}

/// Detailed read profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RsliReadProfile {
    /// Reject compatibility quirks.
    Strict,
    /// Accept selected retail compatibility quirks.
    Compatible(RsliCompatibilityProfile),
}

impl From<ReadProfile> for RsliReadProfile {
    fn from(value: ReadProfile) -> Self {
        match value {
            ReadProfile::Strict => Self::Strict,
            ReadProfile::Compatible => Self::Compatible(RsliCompatibilityProfile::default()),
        }
    }
}

impl RsliReadProfile {
    /// Strict profile with every compatibility quirk disabled.
    #[must_use]
    pub const fn strict() -> Self {
        Self::Strict
    }

    /// Retail-compatible profile with the default approved quirk set.
    #[must_use]
    pub const fn compatible() -> Self {
        Self::Compatible(RsliCompatibilityProfile::retail())
    }

    /// Retail-compatible profile with a caller-provided quirk set.
    #[must_use]
    pub const fn compatible_with(profile: RsliCompatibilityProfile) -> Self {
        Self::Compatible(profile)
    }
}

/// Write profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriteProfile {
    /// Return the original byte image.
    Lossless,
}

/// Decode and payload loading limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodeLimits {
    /// Maximum accepted source archive bytes.
    pub max_input_bytes: u64,
    /// Maximum accepted entry count.
    pub max_entries: u32,
    /// Maximum accepted packed entry bytes.
    pub max_packed_entry_bytes: u64,
    /// Maximum accepted decoded entry bytes.
    pub max_decoded_entry_bytes: u64,
    /// Maximum accepted cumulative decoded bytes for a single load operation.
    pub max_total_decoded_bytes: u64,
}

impl Default for DecodeLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 256 * 1024 * 1024,
            max_entries: 1_000_000,
            max_packed_entry_bytes: 64 * 1024 * 1024,
            max_decoded_entry_bytes: 128 * 1024 * 1024,
            max_total_decoded_bytes: 128 * 1024 * 1024,
        }
    }
}

/// Error returned when mutable editing is attempted.
#[derive(Debug)]
pub enum RsliMutationError {
    /// Entry id is not present in this editable document.
    EntryNotFound {
        /// Requested entry id.
        id: EntryId,
    },
    /// Entry name does not fit into a 12-byte fixed field.
    AuthoringNameTooLong {
        /// Observed length in bytes.
        len: usize,
        /// Maximum accepted length for an authoring field.
        max: usize,
    },
    /// Entry name contains an explicit NUL byte.
    AuthoringNameContainsNul {
        /// Byte offset within the provided name.
        offset: usize,
    },
    /// Packed payload size overflows the format `u32` field.
    PackedPayloadTooLarge {
        /// Requested packed payload size.
        size: usize,
        /// Format maximum (`u32::MAX`).
        max: usize,
    },
    /// Method cannot be represented by the on-disk flags field.
    UnsupportedMethod {
        /// Requested method.
        method: RsliMethod,
    },
}

impl std::fmt::Display for RsliMutationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EntryNotFound { id } => write!(f, "entry id {id:?} is not present"),
            Self::AuthoringNameTooLong { len, max } => {
                write!(f, "authoring name is too long: {len} > {max}")
            }
            Self::AuthoringNameContainsNul { offset } => {
                write!(f, "authoring name contains embedded NUL at {offset}")
            }
            Self::PackedPayloadTooLarge { size, max } => {
                write!(f, "packed payload is too large: {size} > {max}")
            }
            Self::UnsupportedMethod { method } => {
                write!(f, "unsupported authoring method: {method:?}")
            }
        }
    }
}

impl std::error::Error for RsliMutationError {}

/// Mutable editor for `RsliDocument` that can rebuild lookup tables.
#[derive(Clone, Debug)]
pub struct RsliEditor {
    original_image: Arc<[u8]>,
    header: RsliHeader,
    overlay: u32,
    ao_trailer: Option<[u8; 6]>,
    entries: Vec<EditableEntry>,
    dirty: bool,
}

#[derive(Clone, Debug)]
struct EditableEntry {
    meta: EntryMeta,
    packed: Vec<u8>,
}

/// `RsLi` compatibility switches.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RsliCompatibilityProfile {
    /// Allow the registered `AO` trailer overlay.
    pub allow_ao_trailer: bool,
    /// Allow retail Deflate entries whose declared size is one byte past EOF.
    pub allow_deflate_eof_plus_one: bool,
    /// Rebuild lookup order when a retail presorted table is corrupt.
    pub allow_invalid_presorted_fallback: bool,
}

impl Default for RsliCompatibilityProfile {
    fn default() -> Self {
        Self::retail()
    }
}

impl RsliCompatibilityProfile {
    /// Retail-compatible profile with every approved quirk enabled.
    #[must_use]
    pub const fn retail() -> Self {
        Self {
            allow_ao_trailer: true,
            allow_deflate_eof_plus_one: true,
            allow_invalid_presorted_fallback: true,
        }
    }

    /// Profile with every compatibility quirk disabled.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            allow_ao_trailer: false,
            allow_deflate_eof_plus_one: false,
            allow_invalid_presorted_fallback: false,
        }
    }
}

/// `RsLi` packing method.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RsliMethod {
    /// Stored without packing.
    Stored,
    /// XOR only.
    XorOnly,
    /// Simple LZSS.
    Lzss,
    /// XOR plus simple LZSS.
    XorLzss,
    /// Adaptive LZSS/Huffman method `0x080`.
    AdaptiveLzss,
    /// XOR plus adaptive LZSS/Huffman method `0x0A0`.
    XorAdaptiveLzss,
    /// Raw Deflate.
    RawDeflate,
    /// Unsupported method bits.
    Unknown(u32),
}

/// Entry identifier in original table order.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EntryId(pub u32);

/// Archive header summary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RsliHeader {
    /// Raw 32-byte header.
    pub raw: [u8; 32],
    /// Format version.
    pub version: u8,
    /// Entry count.
    pub entry_count: u16,
    /// Presorted flag from the header.
    pub presorted_flag: u16,
    /// XOR seed used for the entry table.
    pub xor_seed: u32,
}

/// `AO` trailer summary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AoTrailer {
    /// Raw six-byte trailer.
    pub raw: [u8; 6],
    /// Media overlay byte offset.
    pub overlay: u32,
}

/// Entry metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntryMeta {
    /// Decoded byte-for-byte name adapter.
    pub name: String,
    /// Raw fixed-size name field.
    pub name_raw: [u8; 12],
    /// Original flags.
    pub flags: i32,
    /// Packing method.
    pub method: RsliMethod,
    /// Effective payload offset after overlay.
    pub data_offset: u64,
    /// Declared packed size.
    pub packed_size: u32,
    /// Declared unpacked size.
    pub unpacked_size: u32,
    /// Sort table value.
    pub sort_to_original: i16,
    /// Raw data offset stored in the table.
    pub data_offset_raw: u32,
}

/// Parsed `RsLi` document.
#[derive(Debug)]
pub struct RsliDocument {
    bytes: Arc<[u8]>,
    header: RsliHeader,
    ao_trailer: Option<AoTrailer>,
    entries: Vec<EntryMeta>,
    records: Vec<EntryRecord>,
}

/// Packed resource bytes and metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackedResource {
    /// Entry metadata.
    pub meta: EntryMeta,
    /// Packed bytes as stored in the archive.
    pub packed: Vec<u8>,
}

/// `RsLi` parse or decode error.
#[derive(Debug)]
pub enum RsliError {
    /// Invalid magic.
    InvalidMagic {
        /// Observed magic.
        got: [u8; 2],
    },
    /// Reserved header byte has an unexpected value.
    InvalidReserved {
        /// Observed reserved byte.
        got: u8,
    },
    /// Unsupported version.
    UnsupportedVersion {
        /// Observed version.
        got: u8,
    },
    /// Invalid entry count.
    InvalidEntryCount {
        /// Observed signed count.
        got: i16,
    },
    /// Too many entries for stable ids.
    TooManyEntries {
        /// Observed count.
        got: usize,
    },
    /// Entry table is outside the archive.
    EntryTableOutOfBounds {
        /// Table byte offset.
        table_offset: u64,
        /// Table byte length.
        table_len: u64,
        /// Archive byte length.
        file_len: u64,
    },
    /// Entry table is structurally corrupt.
    CorruptEntryTable(&'static str),
    /// Entry id is outside this archive.
    EntryIdOutOfRange {
        /// Entry id.
        id: u32,
        /// Entry count.
        entry_count: u32,
    },
    /// Entry payload is outside the archive.
    EntryDataOutOfBounds {
        /// Entry id.
        id: u32,
        /// Payload offset.
        offset: u64,
        /// Payload declared size.
        size: u32,
        /// Archive byte length.
        file_len: u64,
    },
    /// `AO` media overlay points outside the archive.
    MediaOverlayOutOfBounds {
        /// Overlay byte offset.
        overlay: u32,
        /// Archive byte length.
        file_len: u64,
    },
    /// Registered `AO` overlay is rejected by the selected profile.
    AoTrailerQuirkRejected {
        /// Overlay byte offset.
        overlay: u32,
    },
    /// Unsupported packing method.
    UnsupportedMethod {
        /// Raw method bits.
        raw: u32,
    },
    /// Packed range ends past EOF.
    PackedSizePastEof {
        /// Entry id.
        id: u32,
        /// Payload offset.
        offset: u64,
        /// Declared packed size.
        packed_size: u32,
        /// Archive byte length.
        file_len: u64,
    },
    /// Registered retail quirk is rejected by the selected profile.
    DeflateEofPlusOneQuirkRejected {
        /// Entry id.
        id: u32,
    },
    /// Payload decompression failed.
    DecompressionFailed(&'static str),
    /// Decoded payload size does not match the declared size.
    OutputSizeMismatch {
        /// Expected decoded size.
        expected: u32,
        /// Observed decoded size.
        got: u32,
    },
    /// Integer conversion or arithmetic overflow.
    IntegerOverflow,
    /// Shared bounded decode failure.
    Binary(DecodeError),
}

impl fmt::Display for RsliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMagic { got } => write!(f, "invalid RsLi magic: {got:02X?}"),
            Self::InvalidReserved { got } => write!(f, "invalid RsLi reserved byte: {got:#x}"),
            Self::UnsupportedVersion { got } => write!(f, "unsupported RsLi version: {got:#x}"),
            Self::InvalidEntryCount { got } => write!(f, "invalid entry_count: {got}"),
            Self::TooManyEntries { got } => write!(f, "too many entries: {got} exceeds u32::MAX"),
            Self::EntryTableOutOfBounds {
                table_offset,
                table_len,
                file_len,
            } => write!(
                f,
                "entry table out of bounds: off={table_offset}, len={table_len}, file={file_len}"
            ),
            Self::CorruptEntryTable(message) => write!(f, "corrupt entry table: {message}"),
            Self::EntryIdOutOfRange { id, entry_count } => {
                write!(f, "RsLi entry id out of range: {id} >= {entry_count}")
            }
            Self::EntryDataOutOfBounds {
                id,
                offset,
                size,
                file_len,
            } => write!(
                f,
                "entry data out of bounds: id={id}, off={offset}, size={size}, file={file_len}"
            ),
            Self::MediaOverlayOutOfBounds { overlay, file_len } => {
                write!(
                    f,
                    "media overlay out of bounds: overlay={overlay}, file={file_len}"
                )
            }
            Self::AoTrailerQuirkRejected { overlay } => {
                write!(f, "AO trailer quirk rejected: overlay={overlay}")
            }
            Self::UnsupportedMethod { raw } => write!(f, "unsupported packing method: {raw:#x}"),
            Self::PackedSizePastEof {
                id,
                offset,
                packed_size,
                file_len,
            } => write!(
                f,
                "packed range past EOF: id={id}, off={offset}, size={packed_size}, file={file_len}"
            ),
            Self::DeflateEofPlusOneQuirkRejected { id } => {
                write!(f, "deflate EOF+1 quirk rejected for entry {id}")
            }
            Self::DecompressionFailed(message) => write!(f, "decompression failed: {message}"),
            Self::OutputSizeMismatch { expected, got } => {
                write!(f, "output size mismatch: expected={expected}, got={got}")
            }
            Self::IntegerOverflow => write!(f, "integer overflow"),
            Self::Binary(source) => write!(f, "{source}"),
        }
    }
}

impl std::error::Error for RsliError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Binary(source) => Some(source),
            _ => None,
        }
    }
}

impl From<DecodeError> for RsliError {
    fn from(value: DecodeError) -> Self {
        Self::Binary(value)
    }
}

/// Decodes an `RsLi` document.
///
/// # Errors
///
/// Returns [`RsliError`] when the header, table, payload ranges, registered
/// compatibility quirks, or packed payloads are invalid for the selected
/// profile.
pub fn decode(bytes: Arc<[u8]>, profile: ReadProfile) -> Result<RsliDocument, RsliError> {
    decode_with_limits(bytes, profile, DecodeLimits::default())
}

/// Decodes an `RsLi` document with explicit archive limits.
///
/// # Errors
///
/// Returns [`RsliError`] when the input exceeds configured limits or the
/// archive is malformed for the selected profile.
pub fn decode_with_limits(
    bytes: Arc<[u8]>,
    profile: ReadProfile,
    limits: DecodeLimits,
) -> Result<RsliDocument, RsliError> {
    decode_with_profile_and_limits(bytes, profile.into(), limits)
}

/// Decodes an `RsLi` document with explicit compatibility switches.
///
/// # Errors
///
/// Returns [`RsliError`] when the header, table, payload ranges, registered
/// compatibility quirks, or packed payloads are invalid for the selected
/// profile.
pub fn decode_with_profile(
    bytes: Arc<[u8]>,
    profile: RsliReadProfile,
) -> Result<RsliDocument, RsliError> {
    decode_with_profile_and_limits(bytes, profile, DecodeLimits::default())
}

/// Decodes an `RsLi` document with explicit profile and archive limits.
///
/// # Errors
///
/// Returns [`RsliError`] when the input exceeds configured limits or the
/// archive is malformed for the selected profile.
pub fn decode_with_profile_and_limits(
    bytes: Arc<[u8]>,
    profile: RsliReadProfile,
    limits: DecodeLimits,
) -> Result<RsliDocument, RsliError> {
    let options = match profile {
        RsliReadProfile::Strict => ParseOptions {
            allow_ao_trailer: false,
            allow_deflate_eof_plus_one: false,
            allow_invalid_presorted_fallback: false,
            limits,
        },
        RsliReadProfile::Compatible(profile) => ParseOptions {
            allow_ao_trailer: profile.allow_ao_trailer,
            allow_deflate_eof_plus_one: profile.allow_deflate_eof_plus_one,
            allow_invalid_presorted_fallback: profile.allow_invalid_presorted_fallback,
            limits,
        },
    };
    let ParsedRsli {
        header,
        ao_trailer,
        records,
    } = parse_rsli(&bytes, options)?;
    let entries = records.iter().map(|record| record.meta.clone()).collect();
    Ok(RsliDocument {
        bytes,
        header,
        ao_trailer,
        entries,
        records,
    })
}

impl RsliDocument {
    /// Header summary.
    #[must_use]
    pub fn header(&self) -> &RsliHeader {
        &self.header
    }

    /// Optional `AO` trailer.
    #[must_use]
    pub fn ao_trailer(&self) -> Option<&AoTrailer> {
        self.ao_trailer.as_ref()
    }

    /// Entry count.
    #[must_use]
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Entries in original table order.
    #[must_use]
    pub fn entries(&self) -> &[EntryMeta] {
        &self.entries
    }

    /// Finds an entry by name.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<EntryId> {
        self.find_bytes(name.as_bytes())
    }

    /// Finds an entry by raw ASCII-case-insensitive name bytes.
    #[must_use]
    pub fn find_bytes(&self, name: &[u8]) -> Option<EntryId> {
        let len = name
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(name.len());
        let query = name[..len]
            .iter()
            .map(u8::to_ascii_uppercase)
            .collect::<Vec<_>>();
        self.find_impl(&query)
    }

    /// Returns an entry by id.
    #[must_use]
    pub fn entry(&self, id: EntryId) -> Option<&EntryMeta> {
        self.entries.get(usize::try_from(id.0).ok()?)
    }

    /// Loads and unpacks an entry.
    ///
    /// # Errors
    ///
    /// Returns [`RsliError`] when `id` is invalid or the packed payload cannot
    /// be decoded to the declared size.
    pub fn load(&self, id: EntryId) -> Result<Vec<u8>, RsliError> {
        self.load_with_limits(id, DecodeLimits::default())
    }

    /// Loads and unpacks an entry with explicit decode limits.
    ///
    /// # Errors
    ///
    /// Returns [`RsliError`] when the packed payload exceeds configured
    /// limits, `id` is invalid, or the payload cannot be decoded.
    pub fn load_with_limits(
        &self,
        id: EntryId,
        limits: DecodeLimits,
    ) -> Result<Vec<u8>, RsliError> {
        let record = self.record_by_id(id)?;
        let packed = self.packed_slice(id, record)?;
        decode_payload(
            packed,
            record.meta.method,
            record.key16,
            record.meta.unpacked_size,
            limits,
        )
    }

    /// Returns packed bytes and public metadata.
    ///
    /// # Errors
    ///
    /// Returns [`RsliError`] when `id` is invalid or the packed range is outside
    /// the archive.
    pub fn load_packed(&self, id: EntryId) -> Result<PackedResource, RsliError> {
        let record = self.record_by_id(id)?;
        let packed = self.packed_slice(id, record)?.to_vec();
        Ok(PackedResource {
            meta: record.meta.clone(),
            packed,
        })
    }

    /// Encodes the document according to the selected profile.
    #[must_use]
    pub fn encode(&self, profile: WriteProfile) -> Vec<u8> {
        match profile {
            WriteProfile::Lossless => self.bytes.to_vec(),
        }
    }

    /// Creates a mutable editor from the parsed document.
    ///
    /// # Errors
    ///
    /// Returns [`RsliError`] when source payloads cannot be copied from the
    /// underlying archive image.
    pub fn editor(&self) -> Result<RsliEditor, RsliError> {
        let mut entries = Vec::with_capacity(self.records.len());
        for (id, record) in self.records.iter().enumerate() {
            let entry_id = EntryId(u32::try_from(id).map_err(|_| RsliError::IntegerOverflow)?);
            let packed = self.packed_slice(entry_id, record)?.to_vec();
            entries.push(EditableEntry {
                meta: record.meta.clone(),
                packed,
            });
        }

        Ok(RsliEditor {
            original_image: self.bytes.clone(),
            header: self.header.clone(),
            overlay: self
                .ao_trailer
                .as_ref()
                .map_or(0, |overlay| overlay.overlay),
            ao_trailer: self.ao_trailer.as_ref().map(|overlay| overlay.raw),
            entries,
            dirty: false,
        })
    }
}

impl RsliEditor {
    /// Returns editable entries by original directory id.
    #[must_use]
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Replaces packed payload bytes for an entry.
    ///
    /// `unpacked_size` is stored explicitly for compatibility checks and does
    /// not imply a packing transform.
    ///
    /// # Errors
    ///
    /// Returns [`RsliMutationError`] when the entry id is unknown or the packed
    /// payload is too large for the archive directory format.
    pub fn set_packed_payload(
        &mut self,
        id: EntryId,
        packed: impl Into<Vec<u8>>,
        unpacked_size: u32,
    ) -> Result<(), RsliMutationError> {
        let entry = self.entry_mut(id)?;
        let packed = packed.into();
        entry.meta.packed_size =
            u32::try_from(packed.len()).map_err(|_| RsliMutationError::PackedPayloadTooLarge {
                size: packed.len(),
                max: u32::MAX as usize,
            })?;
        entry.packed = packed;
        entry.meta.unpacked_size = unpacked_size;
        self.dirty = true;
        Ok(())
    }

    /// Replaces entry packing method in-place.
    ///
    /// # Errors
    ///
    /// Returns [`RsliMutationError`] when the entry id is unknown.
    pub fn set_method(&mut self, id: EntryId, method: RsliMethod) -> Result<(), RsliMutationError> {
        let entry = self.entry_mut(id)?;
        entry.meta.flags = flags_with_method(entry.meta.flags, method)?;
        entry.meta.method = method;
        self.dirty = true;
        Ok(())
    }

    /// Replaces entry name in the fixed 12-byte table field.
    ///
    /// # Errors
    ///
    /// Returns [`RsliMutationError`] when the entry id is unknown or the name
    /// cannot be represented in the fixed authoring field.
    pub fn set_name(&mut self, id: EntryId, name: &[u8]) -> Result<(), RsliMutationError> {
        let entry = self.entry_mut(id)?;
        entry.meta.name_raw = authoring_name_raw(name)?;
        entry.meta.name = decode_name(c_name_bytes(&entry.meta.name_raw));
        self.dirty = true;
        Ok(())
    }

    /// Encodes the document according to editor state.
    ///
    /// For untouched documents returns the original image verbatim. On any
    /// mutation this method rebuilds the lookup table and rewrites packed entry
    /// bytes deterministically.
    ///
    /// # Errors
    ///
    /// Returns [`RsliError`] when offsets, sizes or ids exceed in-memory limits.
    pub fn encode(&self) -> Result<Vec<u8>, RsliError> {
        if !self.dirty {
            return Ok(self.original_image.to_vec());
        }
        self.encode_rebuild()
    }

    fn encode_rebuild(&self) -> Result<Vec<u8>, RsliError> {
        let mut output = Vec::with_capacity(self.original_image.len());

        let entry_count =
            u16::try_from(self.entries.len()).map_err(|_| RsliError::IntegerOverflow)?;
        let table_len = self
            .entries
            .len()
            .checked_mul(32)
            .ok_or(RsliError::IntegerOverflow)?;

        let mut header = self.header.raw;
        header[4..6].copy_from_slice(&entry_count.to_le_bytes());
        output.extend_from_slice(&header);

        let mut sorted = (0..self.entries.len()).collect::<Vec<_>>();
        sorted.sort_by(|left, right| {
            cmp_c_string(
                c_name_bytes(&self.entries[*left].meta.name_raw),
                c_name_bytes(&self.entries[*right].meta.name_raw),
            )
        });

        let mut lookup_map = vec![0i16; self.entries.len()];
        for (position, original) in sorted.iter().enumerate() {
            lookup_map[*original] =
                i16::try_from(position).map_err(|_| RsliError::IntegerOverflow)?;
        }

        let mut cursor = 32usize
            .checked_add(table_len)
            .ok_or(RsliError::IntegerOverflow)?;
        let mut table_plain = Vec::with_capacity(table_len);
        for (index, entry) in self.entries.iter().enumerate() {
            let mut row = [0u8; 32];
            let name_len = entry.meta.name_raw.len().min(12);
            row[0..name_len].copy_from_slice(&entry.meta.name_raw[..name_len]);

            row[16..18].copy_from_slice(
                &i16::try_from(entry.meta.flags)
                    .map_err(|_| RsliError::IntegerOverflow)?
                    .to_le_bytes(),
            );
            row[18..20].copy_from_slice(&lookup_map[index].to_le_bytes());
            row[20..24].copy_from_slice(&entry.meta.unpacked_size.to_le_bytes());

            let packed_len =
                u32::try_from(entry.packed.len()).map_err(|_| RsliError::IntegerOverflow)?;
            let cursor_u32 = u32::try_from(cursor).map_err(|_| RsliError::IntegerOverflow)?;
            let offset_raw = if self.overlay == 0 {
                cursor_u32
            } else {
                cursor_u32
                    .checked_sub(self.overlay)
                    .ok_or(RsliError::IntegerOverflow)?
            };

            row[24..28].copy_from_slice(&offset_raw.to_le_bytes());
            row[28..32].copy_from_slice(&packed_len.to_le_bytes());
            table_plain.extend_from_slice(&row);

            output.extend_from_slice(&entry.packed);
            cursor = cursor
                .checked_add(entry.packed.len())
                .ok_or(RsliError::IntegerOverflow)?;
        }

        let seed =
            u16::try_from(self.header.xor_seed & 0xFFFF).map_err(|_| RsliError::IntegerOverflow)?;
        let encrypted = xor_stream(&table_plain, seed);
        output.splice(32..32, encrypted);

        if let Some(overlay) = &self.ao_trailer {
            output.extend_from_slice(overlay);
        }

        Ok(output)
    }

    fn entry_mut(&mut self, id: EntryId) -> Result<&mut EditableEntry, RsliMutationError> {
        self.entries
            .get_mut(usize::try_from(id.0).map_err(|_| RsliMutationError::EntryNotFound { id })?)
            .ok_or(RsliMutationError::EntryNotFound { id })
    }
}

impl RsliDocument {
    fn find_impl(&self, query_bytes: &[u8]) -> Option<EntryId> {
        let mut low = 0usize;
        let mut high = self.records.len();
        while low < high {
            let mid = low + (high - low) / 2;
            let original = self.records.get(mid)?.meta.sort_to_original;
            if original < 0 {
                break;
            }
            let original = usize::try_from(original).ok()?;
            let record = self.records.get(original)?;
            match cmp_c_string(query_bytes, c_name_bytes(&record.meta.name_raw)) {
                std::cmp::Ordering::Less => high = mid,
                std::cmp::Ordering::Greater => low = mid + 1,
                std::cmp::Ordering::Equal => return Some(EntryId(u32::try_from(original).ok()?)),
            }
        }

        self.records.iter().enumerate().find_map(|(idx, record)| {
            if cmp_c_string(query_bytes, c_name_bytes(&record.meta.name_raw))
                == std::cmp::Ordering::Equal
            {
                Some(EntryId(u32::try_from(idx).ok()?))
            } else {
                None
            }
        })
    }

    fn record_by_id(&self, id: EntryId) -> Result<&EntryRecord, RsliError> {
        let idx = usize::try_from(id.0).map_err(|_| RsliError::IntegerOverflow)?;
        self.records
            .get(idx)
            .ok_or_else(|| RsliError::EntryIdOutOfRange {
                id: id.0,
                entry_count: saturating_u32_len(self.records.len()),
            })
    }

    fn packed_slice<'a>(
        &'a self,
        id: EntryId,
        record: &EntryRecord,
    ) -> Result<&'a [u8], RsliError> {
        let end = record
            .effective_offset
            .checked_add(record.packed_size_available)
            .ok_or(RsliError::IntegerOverflow)?;
        self.bytes
            .get(record.effective_offset..end)
            .ok_or(RsliError::EntryDataOutOfBounds {
                id: id.0,
                offset: u64::try_from(record.effective_offset).unwrap_or(u64::MAX),
                size: record.packed_size_declared,
                file_len: u64::try_from(self.bytes.len()).unwrap_or(u64::MAX),
            })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ParseOptions {
    allow_ao_trailer: bool,
    allow_deflate_eof_plus_one: bool,
    allow_invalid_presorted_fallback: bool,
    limits: DecodeLimits,
}

#[derive(Clone, Debug)]
struct ParsedRsli {
    header: RsliHeader,
    ao_trailer: Option<AoTrailer>,
    records: Vec<EntryRecord>,
}

#[derive(Clone, Debug)]
struct EntryRecord {
    meta: EntryMeta,
    key16: u16,
    packed_size_declared: u32,
    packed_size_available: usize,
    effective_offset: usize,
}

#[allow(clippy::too_many_lines)]
fn parse_rsli(bytes: &[u8], options: ParseOptions) -> Result<ParsedRsli, RsliError> {
    enforce_limit(
        u64::try_from(bytes.len()).map_err(|_| RsliError::IntegerOverflow)?,
        options.limits.max_input_bytes,
    )?;
    if bytes.len() < 32 {
        return Err(RsliError::EntryTableOutOfBounds {
            table_offset: 32,
            table_len: 0,
            file_len: u64::try_from(bytes.len()).map_err(|_| RsliError::IntegerOverflow)?,
        });
    }

    let mut header_raw = [0u8; 32];
    header_raw.copy_from_slice(&bytes[0..32]);

    let mut magic = [0u8; 2];
    magic.copy_from_slice(&bytes[0..2]);
    if &magic != b"NL" {
        return Err(RsliError::InvalidMagic { got: magic });
    }
    let reserved = bytes[2];
    if reserved != 0 {
        return Err(RsliError::InvalidReserved { got: reserved });
    }
    let version = bytes[3];
    if version != 0x01 {
        return Err(RsliError::UnsupportedVersion { got: version });
    }

    let entry_count_signed = i16::from_le_bytes([bytes[4], bytes[5]]);
    if entry_count_signed < 0 {
        return Err(RsliError::InvalidEntryCount {
            got: entry_count_signed,
        });
    }
    let count = usize::try_from(entry_count_signed).map_err(|_| RsliError::IntegerOverflow)?;
    if count > usize::try_from(u32::MAX).map_err(|_| RsliError::IntegerOverflow)? {
        return Err(RsliError::TooManyEntries { got: count });
    }
    enforce_limit(
        u64::try_from(count).map_err(|_| RsliError::IntegerOverflow)?,
        u64::from(options.limits.max_entries),
    )?;

    let presorted_flag = u16::from_le_bytes([bytes[14], bytes[15]]);
    let xor_seed = u32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    let header = RsliHeader {
        raw: header_raw,
        version,
        entry_count: u16::try_from(entry_count_signed).map_err(|_| RsliError::IntegerOverflow)?,
        presorted_flag,
        xor_seed,
    };

    let table_len = count.checked_mul(32).ok_or(RsliError::IntegerOverflow)?;
    let table_end = 32usize
        .checked_add(table_len)
        .ok_or(RsliError::IntegerOverflow)?;
    if table_end > bytes.len() {
        return Err(RsliError::EntryTableOutOfBounds {
            table_offset: 32,
            table_len: u64::try_from(table_len).map_err(|_| RsliError::IntegerOverflow)?,
            file_len: u64::try_from(bytes.len()).map_err(|_| RsliError::IntegerOverflow)?,
        });
    }

    let table_plain = xor_stream(&bytes[32..table_end], (xor_seed & 0xFFFF) as u16);
    if table_plain.len() != table_len {
        return Err(RsliError::CorruptEntryTable(
            "entry table decrypt length mismatch",
        ));
    }

    let (overlay, trailer_raw) = parse_ao_trailer(bytes, options.allow_ao_trailer)?;

    let mut records = Vec::with_capacity(count);
    for idx in 0..count {
        let row = &table_plain[idx * 32..(idx + 1) * 32];
        let mut name_raw = [0u8; 12];
        name_raw.copy_from_slice(&row[0..12]);

        let flags_signed = i16::from_le_bytes([row[16], row[17]]);
        let mut sort_to_original = i16::from_le_bytes([row[18], row[19]]);
        let unpacked_size = u32::from_le_bytes([row[20], row[21], row[22], row[23]]);
        let data_offset_raw = u32::from_le_bytes([row[24], row[25], row[26], row[27]]);
        let packed_size_declared = u32::from_le_bytes([row[28], row[29], row[30], row[31]]);
        enforce_limit(
            u64::from(packed_size_declared),
            options.limits.max_packed_entry_bytes,
        )?;
        enforce_limit(
            u64::from(unpacked_size),
            options.limits.max_decoded_entry_bytes,
        )?;
        let method_raw = u32::from(flags_signed.cast_unsigned()) & 0x1E0;
        let method = parse_method(method_raw);

        let effective_offset_u64 = u64::from(data_offset_raw)
            .checked_add(u64::from(overlay))
            .ok_or(RsliError::IntegerOverflow)?;
        let effective_offset =
            usize::try_from(effective_offset_u64).map_err(|_| RsliError::IntegerOverflow)?;
        let mut packed_size_available =
            usize::try_from(packed_size_declared).map_err(|_| RsliError::IntegerOverflow)?;
        let end = effective_offset_u64
            .checked_add(u64::from(packed_size_declared))
            .ok_or(RsliError::IntegerOverflow)?;
        let file_len = u64::try_from(bytes.len()).map_err(|_| RsliError::IntegerOverflow)?;

        if end > file_len {
            if method_raw == 0x100 && end == file_len + 1 {
                if options.allow_deflate_eof_plus_one
                    && is_registered_deflate_eof_plus_one_quirk(&name_raw)
                {
                    packed_size_available = packed_size_available
                        .checked_sub(1)
                        .ok_or(RsliError::IntegerOverflow)?;
                } else {
                    return Err(RsliError::DeflateEofPlusOneQuirkRejected {
                        id: u32::try_from(idx).map_err(|_| RsliError::IntegerOverflow)?,
                    });
                }
            } else {
                return Err(RsliError::PackedSizePastEof {
                    id: u32::try_from(idx).map_err(|_| RsliError::IntegerOverflow)?,
                    offset: effective_offset_u64,
                    packed_size: packed_size_declared,
                    file_len,
                });
            }
        }

        let available_end = effective_offset
            .checked_add(packed_size_available)
            .ok_or(RsliError::IntegerOverflow)?;
        if available_end > bytes.len() {
            return Err(RsliError::EntryDataOutOfBounds {
                id: u32::try_from(idx).map_err(|_| RsliError::IntegerOverflow)?,
                offset: effective_offset_u64,
                size: packed_size_declared,
                file_len,
            });
        }

        if presorted_flag != 0xABBA {
            sort_to_original = 0;
        }

        records.push(EntryRecord {
            meta: EntryMeta {
                name: decode_name(c_name_bytes(&name_raw)),
                name_raw,
                flags: i32::from(flags_signed),
                method,
                data_offset: effective_offset_u64,
                packed_size: packed_size_declared,
                unpacked_size,
                sort_to_original,
                data_offset_raw,
            },
            key16: sort_to_original.cast_unsigned(),
            packed_size_declared,
            packed_size_available,
            effective_offset,
        });
    }

    if presorted_flag == 0xABBA {
        let permutation = validate_permutation(&records);
        let order = validate_lookup_order(&records);
        if permutation.is_err() || order.is_err() {
            if !options.allow_invalid_presorted_fallback {
                permutation?;
                order?;
            }
            rebuild_sorted_mapping(&mut records)?;
        }
    } else {
        rebuild_sorted_mapping(&mut records)?;
    }

    Ok(ParsedRsli {
        header,
        ao_trailer: trailer_raw.map(|raw| AoTrailer { raw, overlay }),
        records,
    })
}

fn rebuild_sorted_mapping(records: &mut [EntryRecord]) -> Result<(), RsliError> {
    let mut sorted: Vec<usize> = (0..records.len()).collect();
    sorted.sort_by(|a, b| {
        cmp_c_string(
            c_name_bytes(&records[*a].meta.name_raw),
            c_name_bytes(&records[*b].meta.name_raw),
        )
    });
    for (idx, record) in records.iter_mut().enumerate() {
        record.meta.sort_to_original =
            i16::try_from(sorted[idx]).map_err(|_| RsliError::IntegerOverflow)?;
        record.key16 = record.meta.sort_to_original.cast_unsigned();
    }
    Ok(())
}

fn parse_ao_trailer(bytes: &[u8], allow: bool) -> Result<(u32, Option<[u8; 6]>), RsliError> {
    if bytes.len() < 6 || &bytes[bytes.len() - 6..bytes.len() - 4] != b"AO" {
        return Ok((0, None));
    }
    let mut raw = [0u8; 6];
    raw.copy_from_slice(&bytes[bytes.len() - 6..]);
    let overlay = u32::from_le_bytes([raw[2], raw[3], raw[4], raw[5]]);
    if u64::from(overlay) > u64::try_from(bytes.len()).map_err(|_| RsliError::IntegerOverflow)? {
        return Err(RsliError::MediaOverlayOutOfBounds {
            overlay,
            file_len: u64::try_from(bytes.len()).map_err(|_| RsliError::IntegerOverflow)?,
        });
    }
    if !allow {
        return Err(RsliError::AoTrailerQuirkRejected { overlay });
    }
    Ok((overlay, Some(raw)))
}

fn validate_permutation(records: &[EntryRecord]) -> Result<(), RsliError> {
    let mut seen = vec![false; records.len()];
    for record in records {
        let idx = i32::from(record.meta.sort_to_original);
        if idx < 0 {
            return Err(RsliError::CorruptEntryTable(
                "sort_to_original is not a valid permutation index",
            ));
        }
        let idx = usize::try_from(idx).map_err(|_| RsliError::IntegerOverflow)?;
        if idx >= records.len() || seen[idx] {
            return Err(RsliError::CorruptEntryTable(
                "sort_to_original is not a permutation",
            ));
        }
        seen[idx] = true;
    }
    if seen.iter().any(|value| !*value) {
        return Err(RsliError::CorruptEntryTable(
            "sort_to_original is not a permutation",
        ));
    }
    Ok(())
}

fn validate_lookup_order(records: &[EntryRecord]) -> Result<(), RsliError> {
    for pair in records.windows(2) {
        let left_original = usize::try_from(i32::from(pair[0].meta.sort_to_original))
            .map_err(|_| RsliError::IntegerOverflow)?;
        let right_original = usize::try_from(i32::from(pair[1].meta.sort_to_original))
            .map_err(|_| RsliError::IntegerOverflow)?;
        let left = records
            .get(left_original)
            .ok_or(RsliError::CorruptEntryTable(
                "sort_to_original is not a permutation",
            ))?;
        let right = records
            .get(right_original)
            .ok_or(RsliError::CorruptEntryTable(
                "sort_to_original is not a permutation",
            ))?;
        if cmp_c_string(
            c_name_bytes(&left.meta.name_raw),
            c_name_bytes(&right.meta.name_raw),
        ) == std::cmp::Ordering::Greater
        {
            return Err(RsliError::CorruptEntryTable(
                "presorted lookup names are not sorted",
            ));
        }
    }
    Ok(())
}

fn parse_method(raw: u32) -> RsliMethod {
    match raw {
        0x000 => RsliMethod::Stored,
        0x020 => RsliMethod::XorOnly,
        0x040 => RsliMethod::Lzss,
        0x060 => RsliMethod::XorLzss,
        0x080 => RsliMethod::AdaptiveLzss,
        0x0A0 => RsliMethod::XorAdaptiveLzss,
        0x100 => RsliMethod::RawDeflate,
        other => RsliMethod::Unknown(other),
    }
}

fn is_registered_deflate_eof_plus_one_quirk(name_raw: &[u8; 12]) -> bool {
    c_name_bytes(name_raw)
        .iter()
        .map(u8::to_ascii_uppercase)
        .eq(b"INTERF8.TEX".iter().copied())
}

fn decode_name(name: &[u8]) -> String {
    name.iter().map(|byte| char::from(*byte)).collect()
}

fn authoring_name_raw(name: &[u8]) -> Result<[u8; 12], RsliMutationError> {
    if name.len() > 12 {
        return Err(RsliMutationError::AuthoringNameTooLong {
            len: name.len(),
            max: 12,
        });
    }
    let mut output = [0u8; 12];
    for (offset, byte) in name.iter().copied().enumerate() {
        if byte == 0 {
            return Err(RsliMutationError::AuthoringNameContainsNul { offset });
        }
        output[offset] = byte;
    }
    Ok(output)
}

fn c_name_bytes(raw: &[u8; 12]) -> &[u8] {
    let len = raw.iter().position(|byte| *byte == 0).unwrap_or(raw.len());
    &raw[..len]
}

fn cmp_c_string(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    let min_len = a.len().min(b.len());
    for idx in 0..min_len {
        if a[idx] != b[idx] {
            return a[idx].cmp(&b[idx]);
        }
    }
    a.len().cmp(&b.len())
}

fn decode_payload(
    packed: &[u8],
    method: RsliMethod,
    key16: u16,
    unpacked_size: u32,
    limits: DecodeLimits,
) -> Result<Vec<u8>, RsliError> {
    enforce_limit(
        u64::try_from(packed.len()).map_err(|_| RsliError::IntegerOverflow)?,
        limits.max_packed_entry_bytes,
    )?;
    enforce_limit(u64::from(unpacked_size), limits.max_decoded_entry_bytes)?;
    enforce_limit(u64::from(unpacked_size), limits.max_total_decoded_bytes)?;
    let expected = usize::try_from(unpacked_size).map_err(|_| RsliError::IntegerOverflow)?;
    let out = match method {
        RsliMethod::Stored => {
            if packed.len() != expected {
                return Err(RsliError::OutputSizeMismatch {
                    expected: unpacked_size,
                    got: u32::try_from(packed.len()).unwrap_or(u32::MAX),
                });
            }
            packed.to_vec()
        }
        RsliMethod::XorOnly => {
            if packed.len() != expected {
                return Err(RsliError::OutputSizeMismatch {
                    expected: unpacked_size,
                    got: u32::try_from(packed.len()).unwrap_or(u32::MAX),
                });
            }
            xor_stream(packed, key16)
        }
        RsliMethod::Lzss => lzss_decompress_simple(packed, expected, None)?,
        RsliMethod::XorLzss => lzss_decompress_simple(packed, expected, Some(key16))?,
        RsliMethod::AdaptiveLzss => lzss_huffman_decompress(packed, expected, None)?,
        RsliMethod::XorAdaptiveLzss => lzss_huffman_decompress(packed, expected, Some(key16))?,
        RsliMethod::RawDeflate => decode_deflate(packed, expected)?,
        RsliMethod::Unknown(raw) => return Err(RsliError::UnsupportedMethod { raw }),
    };
    if out.len() != expected {
        return Err(RsliError::OutputSizeMismatch {
            expected: unpacked_size,
            got: u32::try_from(out.len()).unwrap_or(u32::MAX),
        });
    }
    Ok(out)
}

#[derive(Clone, Copy, Debug)]
struct XorState {
    lo: u8,
    hi: u8,
}

impl XorState {
    fn new(key16: u16) -> Self {
        Self {
            lo: u8::try_from(key16 & 0xFF).unwrap_or(u8::MAX),
            hi: u8::try_from((key16 >> 8) & 0xFF).unwrap_or(u8::MAX),
        }
    }

    fn decrypt_byte(&mut self, encrypted: u8) -> u8 {
        self.lo = self.hi ^ self.lo.wrapping_shl(1);
        let decrypted = encrypted ^ self.lo;
        self.hi = self.lo ^ (self.hi >> 1);
        decrypted
    }
}

fn xor_stream(data: &[u8], key16: u16) -> Vec<u8> {
    let mut state = XorState::new(key16);
    data.iter().map(|byte| state.decrypt_byte(*byte)).collect()
}

fn lzss_decompress_simple(
    data: &[u8],
    expected_size: usize,
    xor_key: Option<u16>,
) -> Result<Vec<u8>, RsliError> {
    let mut ring = [0x20u8; 0x1000];
    let mut ring_pos = 0xFEEusize;
    let mut out = Vec::with_capacity(expected_size);
    let mut in_pos = 0usize;
    let mut control = 0u8;
    let mut bits_left = 0u8;
    let mut xor_state = xor_key.map(XorState::new);

    while out.len() < expected_size {
        if bits_left == 0 {
            control = read_packed_byte(data, in_pos, &mut xor_state).ok_or(
                RsliError::DecompressionFailed("lzss-simple: unexpected EOF"),
            )?;
            in_pos = in_pos.saturating_add(1);
            bits_left = 8;
        }

        if (control & 1) != 0 {
            let byte = read_packed_byte(data, in_pos, &mut xor_state).ok_or(
                RsliError::DecompressionFailed("lzss-simple: unexpected EOF"),
            )?;
            in_pos = in_pos.saturating_add(1);
            out.push(byte);
            ring[ring_pos] = byte;
            ring_pos = (ring_pos + 1) & 0x0FFF;
        } else {
            let low = read_packed_byte(data, in_pos, &mut xor_state).ok_or(
                RsliError::DecompressionFailed("lzss-simple: unexpected EOF"),
            )?;
            let high = read_packed_byte(data, in_pos.saturating_add(1), &mut xor_state).ok_or(
                RsliError::DecompressionFailed("lzss-simple: unexpected EOF"),
            )?;
            in_pos = in_pos.saturating_add(2);
            let offset = usize::from(low) | (usize::from(high & 0xF0) << 4);
            let length = usize::from((high & 0x0F) + 3);
            for step in 0..length {
                let byte = ring[(offset + step) & 0x0FFF];
                out.push(byte);
                ring[ring_pos] = byte;
                ring_pos = (ring_pos + 1) & 0x0FFF;
                if out.len() >= expected_size {
                    break;
                }
            }
        }
        control >>= 1;
        bits_left -= 1;
    }
    Ok(out)
}

fn read_packed_byte(data: &[u8], pos: usize, state: &mut Option<XorState>) -> Option<u8> {
    let encrypted = data.get(pos).copied()?;
    Some(if let Some(state) = state {
        state.decrypt_byte(encrypted)
    } else {
        encrypted
    })
}

fn decode_deflate(packed: &[u8], expected_size: usize) -> Result<Vec<u8>, RsliError> {
    let mut out = Vec::with_capacity(expected_size);
    let mut chunk = [0u8; 4096];
    let mut decoder = flate2::read::DeflateDecoder::new(packed);
    loop {
        let read = decoder
            .read(&mut chunk)
            .map_err(|_| RsliError::DecompressionFailed("deflate"))?;
        if read == 0 {
            break;
        }
        let next_len = out
            .len()
            .checked_add(read)
            .ok_or(RsliError::IntegerOverflow)?;
        if next_len > expected_size {
            return Err(RsliError::OutputSizeMismatch {
                expected: u32::try_from(expected_size).unwrap_or(u32::MAX),
                got: u32::try_from(next_len).unwrap_or(u32::MAX),
            });
        }
        out.extend_from_slice(&chunk[..read]);
    }
    Ok(out)
}

fn method_bits(method: RsliMethod) -> Result<u16, RsliMutationError> {
    match method {
        RsliMethod::Stored => Ok(0x000),
        RsliMethod::XorOnly => Ok(0x020),
        RsliMethod::Lzss => Ok(0x040),
        RsliMethod::XorLzss => Ok(0x060),
        RsliMethod::AdaptiveLzss => Ok(0x080),
        RsliMethod::XorAdaptiveLzss => Ok(0x0A0),
        RsliMethod::RawDeflate => Ok(0x100),
        RsliMethod::Unknown(_) => Err(RsliMutationError::UnsupportedMethod { method }),
    }
}

fn flags_with_method(flags: i32, method: RsliMethod) -> Result<i32, RsliMutationError> {
    let method = i32::from(method_bits(method)?);
    Ok((flags & !0x1E0) | method)
}

fn enforce_limit(value: u64, limit: u64) -> Result<(), RsliError> {
    if value > limit {
        return Err(DecodeError::LimitExceeded {
            count: value,
            limit,
        }
        .into());
    }
    Ok(())
}

const LZH_N: usize = 4096;
const LZH_F: usize = 60;
const LZH_THRESHOLD: usize = 2;
const LZH_N_CHAR: usize = 256 - LZH_THRESHOLD + LZH_F;
const LZH_T: usize = LZH_N_CHAR * 2 - 1;
const LZH_R: usize = LZH_T - 1;
const LZH_MAX_FREQ: u16 = 0x8000;

fn lzss_huffman_decompress(
    data: &[u8],
    expected_size: usize,
    xor_key: Option<u16>,
) -> Result<Vec<u8>, RsliError> {
    let mut decoder = LzhDecoder::new(data, xor_key);
    decoder.decode(expected_size)
}

struct LzhDecoder<'a> {
    bit_reader: BitReader<'a>,
    text: [u8; LZH_N],
    freq: [u16; LZH_T + 1],
    parent: [usize; LZH_T + LZH_N_CHAR],
    son: [usize; LZH_T],
    d_code: [u8; 256],
    d_len: [u8; 256],
    ring_pos: usize,
}

impl<'a> LzhDecoder<'a> {
    fn new(data: &'a [u8], xor_key: Option<u16>) -> Self {
        let mut decoder = Self {
            bit_reader: BitReader::new(data, xor_key),
            text: [0x20u8; LZH_N],
            freq: [0u16; LZH_T + 1],
            parent: [0usize; LZH_T + LZH_N_CHAR],
            son: [0usize; LZH_T],
            d_code: [0u8; 256],
            d_len: [0u8; 256],
            ring_pos: LZH_N - LZH_F,
        };
        decoder.init_tables();
        decoder.start_huff();
        decoder
    }

    fn decode(&mut self, expected_size: usize) -> Result<Vec<u8>, RsliError> {
        let mut out = Vec::with_capacity(expected_size);
        while out.len() < expected_size {
            let c = self.decode_char()?;
            if c < 256 {
                let byte = u8::try_from(c).map_err(|_| RsliError::IntegerOverflow)?;
                out.push(byte);
                self.text[self.ring_pos] = byte;
                self.ring_pos = (self.ring_pos + 1) & (LZH_N - 1);
            } else {
                let mut offset = self.decode_position()?;
                offset = (self.ring_pos.wrapping_sub(offset).wrapping_sub(1)) & (LZH_N - 1);
                let mut length = c.saturating_sub(253);
                while length > 0 && out.len() < expected_size {
                    let byte = self.text[offset];
                    out.push(byte);
                    self.text[self.ring_pos] = byte;
                    self.ring_pos = (self.ring_pos + 1) & (LZH_N - 1);
                    offset = (offset + 1) & (LZH_N - 1);
                    length -= 1;
                }
            }
        }
        Ok(out)
    }

    fn init_tables(&mut self) {
        let d_code_group_counts = [1usize, 3, 8, 12, 24, 16];
        let d_len_group_counts = [32usize, 48, 64, 48, 48, 16];
        let mut group_index = 0u8;
        let mut idx = 0usize;
        let mut run = 32usize;
        for count in d_code_group_counts {
            for _ in 0..count {
                for _ in 0..run {
                    self.d_code[idx] = group_index;
                    idx += 1;
                }
                group_index = group_index.wrapping_add(1);
            }
            run >>= 1;
        }

        let mut len = 3u8;
        idx = 0;
        for count in d_len_group_counts {
            for _ in 0..count {
                self.d_len[idx] = len;
                idx += 1;
            }
            len = len.saturating_add(1);
        }
    }

    fn start_huff(&mut self) {
        for i in 0..LZH_N_CHAR {
            self.freq[i] = 1;
            self.son[i] = i + LZH_T;
            self.parent[i + LZH_T] = i;
        }
        let mut i = 0usize;
        let mut j = LZH_N_CHAR;
        while j <= LZH_R {
            self.freq[j] = self.freq[i].saturating_add(self.freq[i + 1]);
            self.son[j] = i;
            self.parent[i] = j;
            self.parent[i + 1] = j;
            i += 2;
            j += 1;
        }
        self.freq[LZH_T] = u16::MAX;
        self.parent[LZH_R] = 0;
    }

    fn decode_char(&mut self) -> Result<usize, RsliError> {
        let mut node = self.son[LZH_R];
        while node < LZH_T {
            let bit = usize::from(self.bit_reader.read_bit()?);
            let branch = node
                .checked_add(bit)
                .ok_or(RsliError::DecompressionFailed("lzss-huffman tree overflow"))?;
            node = *self.son.get(branch).ok_or(RsliError::DecompressionFailed(
                "lzss-huffman tree out of bounds",
            ))?;
        }
        let c = node - LZH_T;
        self.update(c);
        Ok(c)
    }

    fn decode_position(&mut self) -> Result<usize, RsliError> {
        let i = usize::try_from(self.bit_reader.read_bits(8)?)
            .map_err(|_| RsliError::IntegerOverflow)?;
        let mut c = usize::from(self.d_code[i]) << 6;
        let mut j = usize::from(self.d_len[i]).saturating_sub(2);
        while j > 0 {
            j -= 1;
            c |= usize::from(self.bit_reader.read_bit()?) << j;
        }
        Ok(c | (i & 0x3F))
    }

    fn update(&mut self, c: usize) {
        if self.freq[LZH_R] == LZH_MAX_FREQ {
            self.reconstruct();
        }
        let mut current = self.parent[c + LZH_T];
        loop {
            self.freq[current] = self.freq[current].saturating_add(1);
            let freq = self.freq[current];
            if current + 1 < self.freq.len() && freq > self.freq[current + 1] {
                let mut swap_idx = current + 1;
                while swap_idx + 1 < self.freq.len() && freq > self.freq[swap_idx + 1] {
                    swap_idx += 1;
                }
                self.freq.swap(current, swap_idx);
                let left = self.son[current];
                let right = self.son[swap_idx];
                self.son[current] = right;
                self.son[swap_idx] = left;
                self.parent[left] = swap_idx;
                if left < LZH_T {
                    self.parent[left + 1] = swap_idx;
                }
                self.parent[right] = current;
                if right < LZH_T {
                    self.parent[right + 1] = current;
                }
                current = swap_idx;
            }
            current = self.parent[current];
            if current == 0 {
                break;
            }
        }
    }

    fn reconstruct(&mut self) {
        let mut j = 0usize;
        for i in 0..LZH_T {
            if self.son[i] >= LZH_T {
                self.freq[j] = (self.freq[i].saturating_add(1)) / 2;
                self.son[j] = self.son[i];
                j += 1;
            }
        }
        let mut i = 0usize;
        let mut current = LZH_N_CHAR;
        while current < LZH_T {
            let sum = self.freq[i].saturating_add(self.freq[i + 1]);
            self.freq[current] = sum;
            let mut insert_at = current;
            while insert_at > 0 && sum < self.freq[insert_at - 1] {
                insert_at -= 1;
            }
            for move_idx in (insert_at..current).rev() {
                self.freq[move_idx + 1] = self.freq[move_idx];
                self.son[move_idx + 1] = self.son[move_idx];
            }
            self.freq[insert_at] = sum;
            self.son[insert_at] = i;
            i += 2;
            current += 1;
        }
        for idx in 0..LZH_T {
            let node = self.son[idx];
            self.parent[node] = idx;
            if node < LZH_T {
                self.parent[node + 1] = idx;
            }
        }
        self.freq[LZH_T] = u16::MAX;
        self.parent[LZH_R] = 0;
    }
}

struct BitReader<'a> {
    data: &'a [u8],
    byte_pos: usize,
    bit_mask: u8,
    current_byte: u8,
    xor_state: Option<XorState>,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8], xor_key: Option<u16>) -> Self {
        Self {
            data,
            byte_pos: 0,
            bit_mask: 0x80,
            current_byte: 0,
            xor_state: xor_key.map(XorState::new),
        }
    }

    fn read_bit(&mut self) -> Result<u8, RsliError> {
        if self.bit_mask == 0x80 {
            let Some(mut byte) = self.data.get(self.byte_pos).copied() else {
                return Err(RsliError::DecompressionFailed(
                    "lzss-huffman: unexpected EOF",
                ));
            };
            if let Some(state) = &mut self.xor_state {
                byte = state.decrypt_byte(byte);
            }
            self.current_byte = byte;
        }
        let bit = u8::from((self.current_byte & self.bit_mask) != 0);
        self.bit_mask >>= 1;
        if self.bit_mask == 0 {
            self.bit_mask = 0x80;
            self.byte_pos = self.byte_pos.saturating_add(1);
        }
        Ok(bit)
    }

    fn read_bits(&mut self, bits: usize) -> Result<u32, RsliError> {
        let mut value = 0u32;
        for _ in 0..bits {
            value = (value << 1) | u32::from(self.read_bit()?);
        }
        Ok(value)
    }
}

fn saturating_u32_len(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    #[test]
    fn parses_minimal_empty_library() {
        let bytes = synthetic_rsli(&[], false, 0x1234, None);

        let doc = decode(arc(bytes.clone()), ReadProfile::Strict).expect("minimal RsLi");

        assert_eq!(doc.entry_count(), 0);
        assert_eq!(doc.header().raw[0..4], *b"NL\0\x01");
        assert_eq!(doc.encode(WriteProfile::Lossless), bytes);
    }

    #[test]
    fn rejects_invalid_header_fields() {
        let valid = synthetic_rsli(&[], false, 0, None);

        let mut invalid_magic = valid.clone();
        invalid_magic[0] = b'X';
        assert!(matches!(
            decode(arc(invalid_magic), ReadProfile::Strict),
            Err(RsliError::InvalidMagic { .. })
        ));

        let mut invalid_reserved = valid.clone();
        invalid_reserved[2] = 1;
        assert!(matches!(
            decode(arc(invalid_reserved), ReadProfile::Strict),
            Err(RsliError::InvalidReserved { got: 1 })
        ));

        let mut invalid_version = valid.clone();
        invalid_version[3] = 2;
        assert!(matches!(
            decode(arc(invalid_version), ReadProfile::Strict),
            Err(RsliError::UnsupportedVersion { got: 2 })
        ));

        let mut invalid_count = valid;
        invalid_count[4..6].copy_from_slice(&(-1i16).to_le_bytes());
        assert!(matches!(
            decode(arc(invalid_count), ReadProfile::Strict),
            Err(RsliError::InvalidEntryCount { got: -1 })
        ));
    }

    #[test]
    fn rejects_entry_table_bounds() {
        let mut bytes = synthetic_rsli(&[], false, 0, None);
        bytes[4..6].copy_from_slice(&1i16.to_le_bytes());

        assert!(matches!(
            decode(arc(bytes), ReadProfile::Strict),
            Err(RsliError::EntryTableOutOfBounds { .. })
        ));
    }

    #[test]
    fn table_xor_transform_uses_known_vector() {
        assert_eq!(
            xor_stream(&[0x00, 0x01, 0x02, 0x03], 0x1234),
            [0x7A, 0x86, 0xB2, 0x8C]
        );
    }

    #[test]
    fn table_xor_transform_is_symmetric() {
        let plain = b"entry table bytes".to_vec();
        let encrypted = xor_stream(&plain, 0x3456);

        assert_ne!(encrypted, plain);
        assert_eq!(xor_stream(&encrypted, 0x3456), plain);
    }

    #[test]
    fn table_xor_state_spans_entries() {
        let rows = two_plain_rows_for_transform_test();
        let whole_stream = xor_stream(&rows.concat(), 0x2468);
        let row_reset = rows
            .iter()
            .flat_map(|row| xor_stream(row, 0x2468))
            .collect::<Vec<_>>();

        assert_ne!(whole_stream, row_reset);

        let bytes = synthetic_rsli(
            &[
                SyntheticEntry::stored(b"A", 0, b"a"),
                SyntheticEntry::stored(b"B", 1, b"b"),
            ],
            true,
            0x2468,
            None,
        );
        let doc = decode(arc(bytes), ReadProfile::Strict).expect("continuous table stream");
        assert_eq!(doc.entry_count(), 2);
    }

    #[test]
    fn presorted_mapping_uses_valid_permutation() {
        let bytes = synthetic_rsli(
            &[
                SyntheticEntry::stored(b"B", 1, b"bee"),
                SyntheticEntry::stored(b"A", 0, b"aye"),
            ],
            true,
            0x4321,
            None,
        );

        let doc = decode(arc(bytes), ReadProfile::Strict).expect("valid presorted map");

        assert_eq!(doc.find("A"), Some(EntryId(1)));
        assert_eq!(doc.find("B"), Some(EntryId(0)));
        assert_eq!(doc.load(EntryId(1)).expect("A payload"), b"aye");
    }

    #[test]
    fn compatible_profile_rebuilds_invalid_presorted_mapping() {
        let bytes = synthetic_rsli(
            &[
                SyntheticEntry::stored(b"B", 0, b"bee"),
                SyntheticEntry::stored(b"A", 0, b"aye"),
            ],
            true,
            0x0102,
            None,
        );

        assert!(matches!(
            decode(arc(bytes.clone()), ReadProfile::Strict),
            Err(RsliError::CorruptEntryTable(_))
        ));

        let doc = decode(arc(bytes), ReadProfile::Compatible).expect("compatible fallback");
        assert_eq!(doc.find("A"), Some(EntryId(1)));
        assert_eq!(doc.find("B"), Some(EntryId(0)));
    }

    #[test]
    fn strict_rejects_unsorted_presorted_mapping() {
        let bytes = synthetic_rsli(
            &[
                SyntheticEntry::stored(b"B", 0, b"bee"),
                SyntheticEntry::stored(b"A", 1, b"aye"),
            ],
            true,
            0x0103,
            None,
        );

        assert!(matches!(
            decode(arc(bytes.clone()), ReadProfile::Strict),
            Err(RsliError::CorruptEntryTable(
                "presorted lookup names are not sorted"
            ))
        ));

        let doc = decode(arc(bytes), ReadProfile::Compatible).expect("compatible fallback");
        assert_eq!(doc.find("A"), Some(EntryId(1)));
        assert_eq!(doc.find("B"), Some(EntryId(0)));
    }

    #[test]
    fn explicit_profile_controls_invalid_presorted_fallback() {
        let bytes = synthetic_rsli(
            &[
                SyntheticEntry::stored(b"B", 0, b"bee"),
                SyntheticEntry::stored(b"A", 0, b"aye"),
            ],
            true,
            0x0102,
            None,
        );
        let profile = RsliCompatibilityProfile {
            allow_invalid_presorted_fallback: false,
            ..RsliCompatibilityProfile::retail()
        };

        assert!(matches!(
            decode_with_profile(
                arc(bytes.clone()),
                RsliReadProfile::compatible_with(profile)
            ),
            Err(RsliError::CorruptEntryTable(_))
        ));

        let profile = RsliCompatibilityProfile {
            allow_invalid_presorted_fallback: true,
            ..RsliCompatibilityProfile::none()
        };
        let doc = decode_with_profile(arc(bytes), RsliReadProfile::compatible_with(profile))
            .expect("presorted fallback only");
        assert_eq!(doc.find("A"), Some(EntryId(1)));
    }

    #[test]
    fn stored_method_uses_exact_size() {
        let bytes = synthetic_rsli(
            &[SyntheticEntry::stored(b"A", 0, b"abc")],
            true,
            0x1111,
            None,
        );

        let doc = decode(arc(bytes), ReadProfile::Strict).expect("stored entry");

        assert_eq!(doc.load(EntryId(0)).expect("stored payload"), b"abc");
        assert_eq!(doc.entry(EntryId(0)).expect("stored meta").packed_size, 3);
    }

    #[test]
    fn xor_only_method_uses_entry_key() {
        let plain = b"secret".to_vec();
        let packed = xor_stream(&plain, 1);
        let bytes = synthetic_rsli(
            &[
                SyntheticEntry::with_payload(b"B", 0x020, 1, &plain, packed),
                SyntheticEntry::stored(b"A", 0, b"plain"),
            ],
            true,
            0x2222,
            None,
        );

        let doc = decode(arc(bytes), ReadProfile::Strict).expect("xor entry");

        assert_eq!(doc.load(EntryId(0)).expect("xor payload"), plain);
    }

    #[test]
    fn lzss_method_decodes_literals_references_and_wrap() {
        let bytes = synthetic_rsli(
            &[
                SyntheticEntry::with_payload(
                    b"LIT",
                    0x040,
                    0,
                    b"ABC",
                    vec![0b0000_0111, b'A', b'B', b'C'],
                ),
                SyntheticEntry::with_payload(
                    b"WRAP",
                    0x040,
                    1,
                    b"    ",
                    vec![0b0000_0000, 0xFF, 0xF1],
                ),
            ],
            true,
            0x1212,
            None,
        );

        let doc = decode(arc(bytes), ReadProfile::Strict).expect("lzss archive");

        assert_eq!(doc.load(EntryId(0)).expect("literal lzss"), b"ABC");
        assert_eq!(doc.load(EntryId(1)).expect("wrapped reference"), b"    ");
    }

    #[test]
    fn xor_lzss_method_uses_entry_key() {
        let plain_lzss = vec![0b0000_0111, b'X', b'Y', b'Z'];
        let bytes = synthetic_rsli(
            &[
                SyntheticEntry::with_payload(b"X", 0x060, 1, b"XYZ", xor_stream(&plain_lzss, 1)),
                SyntheticEntry::stored(b"A", 0, b"filler"),
            ],
            true,
            0x3434,
            None,
        );

        let doc = decode(arc(bytes), ReadProfile::Strict).expect("xor lzss archive");

        assert_eq!(doc.load(EntryId(0)).expect("xor lzss"), b"XYZ");
    }

    #[test]
    fn adaptive_lzss_method_decodes_synthetic_vector() {
        let bytes = synthetic_rsli(
            &[SyntheticEntry::with_payload(
                b"A",
                0x080,
                0,
                b"t",
                vec![0x00],
            )],
            true,
            0,
            None,
        );

        let doc = decode(arc(bytes), ReadProfile::Strict).expect("adaptive lzss archive");

        assert_eq!(doc.load(EntryId(0)).expect("adaptive lzss"), b"t");
    }

    #[test]
    fn xor_adaptive_lzss_method_decodes_synthetic_vector() {
        let bytes = synthetic_rsli(
            &[
                SyntheticEntry::with_payload(b"X", 0x0A0, 1, b"t", vec![0x02]),
                SyntheticEntry::stored(b"A", 0, b"filler"),
            ],
            true,
            0x5656,
            None,
        );

        let doc = decode(arc(bytes), ReadProfile::Strict).expect("xor adaptive lzss archive");

        assert_eq!(doc.load(EntryId(0)).expect("xor adaptive lzss"), b"t");
    }

    #[test]
    fn raw_deflate_method_expects_raw_stream_not_zlib_wrapper() {
        let raw_deflate = vec![0x01, 0x03, 0x00, 0xFC, 0xFF, b'r', b'a', b'w'];
        let bytes = synthetic_rsli(
            &[SyntheticEntry::with_payload(
                b"RAW",
                0x100,
                0,
                b"raw",
                raw_deflate,
            )],
            true,
            0,
            None,
        );
        let doc = decode(arc(bytes), ReadProfile::Strict).expect("raw deflate archive");
        assert_eq!(doc.load(EntryId(0)).expect("raw deflate"), b"raw");

        let zlib_wrapped = vec![
            0x78, 0x01, 0x01, 0x03, 0x00, 0xFC, 0xFF, b'r', b'a', b'w', 0x02, 0x92, 0x01, 0x4B,
        ];
        let wrapped = synthetic_rsli(
            &[SyntheticEntry::with_payload(
                b"ZLIB",
                0x100,
                0,
                b"raw",
                zlib_wrapped,
            )],
            true,
            0,
            None,
        );
        let doc = decode(arc(wrapped), ReadProfile::Strict).expect("zlib wrapped archive");
        assert!(matches!(
            doc.load(EntryId(0)),
            Err(RsliError::DecompressionFailed("deflate"))
        ));
    }

    #[test]
    fn named_deflate_eof_plus_one_quirk_accepts_only_approved_entry() {
        let raw_deflate = vec![0x01, 0x03, 0x00, 0xFC, 0xFF, b'r', b'a', b'w'];
        let approved = synthetic_rsli(
            &[SyntheticEntry::with_declared_packed_size(
                b"INTERF8.TEX",
                0x100,
                0,
                b"raw",
                raw_deflate.clone(),
                u32::try_from(raw_deflate.len() + 1).expect("declared size"),
            )],
            true,
            0,
            None,
        );

        assert!(matches!(
            decode(arc(approved.clone()), ReadProfile::Strict),
            Err(RsliError::DeflateEofPlusOneQuirkRejected { id: 0 })
        ));
        assert!(matches!(
            decode_with_profile(
                arc(approved.clone()),
                RsliReadProfile::compatible_with(RsliCompatibilityProfile {
                    allow_deflate_eof_plus_one: false,
                    ..RsliCompatibilityProfile::retail()
                })
            ),
            Err(RsliError::DeflateEofPlusOneQuirkRejected { id: 0 })
        ));
        let doc = decode(arc(approved), ReadProfile::Compatible).expect("approved EOF+1 quirk");
        assert_eq!(doc.load(EntryId(0)).expect("approved payload"), b"raw");

        let unknown = synthetic_rsli(
            &[SyntheticEntry::with_declared_packed_size(
                b"OTHER.TEX",
                0x100,
                0,
                b"raw",
                raw_deflate.clone(),
                u32::try_from(raw_deflate.len() + 1).expect("declared size"),
            )],
            true,
            0,
            None,
        );
        assert!(matches!(
            decode(arc(unknown), ReadProfile::Compatible),
            Err(RsliError::DeflateEofPlusOneQuirkRejected { id: 0 })
        ));

        let plus_two = synthetic_rsli(
            &[SyntheticEntry::with_declared_packed_size(
                b"INTERF8.TEX",
                0x100,
                0,
                b"raw",
                raw_deflate.clone(),
                u32::try_from(raw_deflate.len() + 2).expect("declared size"),
            )],
            true,
            0,
            None,
        );
        assert!(matches!(
            decode(arc(plus_two), ReadProfile::Compatible),
            Err(RsliError::PackedSizePastEof { id: 0, .. })
        ));
    }

    #[test]
    fn unknown_method_is_rejected_on_load() {
        let bytes = synthetic_rsli(
            &[SyntheticEntry::with_payload(
                b"A",
                0x1E0,
                0,
                b"abc",
                b"abc".to_vec(),
            )],
            true,
            0,
            None,
        );
        let doc = decode(arc(bytes), ReadProfile::Strict).expect("unknown method archive");

        assert!(matches!(
            doc.load(EntryId(0)),
            Err(RsliError::UnsupportedMethod { raw: 0x1E0 })
        ));
    }

    #[test]
    fn decoded_size_mismatch_is_rejected() {
        let bytes = synthetic_rsli(
            &[SyntheticEntry::with_payload(
                b"A",
                0x000,
                0,
                b"abc",
                b"ab".to_vec(),
            )],
            true,
            0,
            None,
        );
        let doc = decode(arc(bytes), ReadProfile::Strict).expect("mismatched entry archive");

        assert!(matches!(
            doc.load(EntryId(0)),
            Err(RsliError::OutputSizeMismatch {
                expected: 3,
                got: 2
            })
        ));
    }

    #[test]
    fn ao_overlay_adjusts_effective_offsets() {
        let bytes = synthetic_rsli(
            &[SyntheticEntry::stored(b"A", 0, b"media")],
            true,
            0x3333,
            Some(4),
        );

        let doc = decode(arc(bytes.clone()), ReadProfile::Compatible).expect("AO overlay");
        let meta = doc.entry(EntryId(0)).expect("AO meta");
        assert_eq!(meta.data_offset, 64);
        assert_eq!(meta.data_offset_raw, 60);
        assert_eq!(doc.load(EntryId(0)).expect("AO payload"), b"media");

        assert!(matches!(
            decode_with_profile(
                arc(bytes),
                RsliReadProfile::compatible_with(RsliCompatibilityProfile {
                    allow_ao_trailer: false,
                    ..RsliCompatibilityProfile::retail()
                })
            ),
            Err(RsliError::AoTrailerQuirkRejected { overlay: 4 })
        ));
    }

    #[test]
    fn invalid_ao_overlay_is_rejected() {
        let mut bytes = synthetic_rsli(&[], false, 0, None);
        bytes.extend_from_slice(b"AO");
        bytes.extend_from_slice(&1000u32.to_le_bytes());

        assert!(matches!(
            decode(arc(bytes), ReadProfile::Compatible),
            Err(RsliError::MediaOverlayOutOfBounds { overlay: 1000, .. })
        ));
    }

    #[test]
    fn strict_profile_distinguishes_valid_ao_quirk_from_malformed_ao() {
        let valid = synthetic_rsli(
            &[SyntheticEntry::stored(b"A", 0, b"media")],
            true,
            0x3333,
            Some(4),
        );
        assert!(matches!(
            decode_with_profile(arc(valid), RsliReadProfile::strict()),
            Err(RsliError::AoTrailerQuirkRejected { overlay: 4 })
        ));

        let mut malformed = synthetic_rsli(&[], false, 0, None);
        malformed.extend_from_slice(b"AO");
        malformed.extend_from_slice(&1000u32.to_le_bytes());
        assert!(matches!(
            decode_with_profile(arc(malformed), RsliReadProfile::strict()),
            Err(RsliError::MediaOverlayOutOfBounds { overlay: 1000, .. })
        ));
    }

    #[test]
    fn unknown_header_bytes_are_lossless() {
        let mut bytes = synthetic_rsli(
            &[SyntheticEntry::stored(b"A", 0, b"abc")],
            true,
            0x4444,
            None,
        );
        bytes[6] = 0xA5;
        bytes[24] = 0x5A;

        let doc = decode(arc(bytes.clone()), ReadProfile::Strict).expect("unknown header bytes");

        assert_eq!(doc.header().raw[6], 0xA5);
        assert_eq!(doc.header().raw[24], 0x5A);
        assert_eq!(doc.encode(WriteProfile::Lossless), bytes);
    }

    #[test]
    fn no_op_lossless_roundtrip_preserves_bytes() {
        let bytes = synthetic_rsli(
            &[
                SyntheticEntry::stored(b"A", 0, b"alpha"),
                SyntheticEntry::stored(b"B", 1, b"beta"),
            ],
            true,
            0x5555,
            None,
        );

        let doc = decode(arc(bytes.clone()), ReadProfile::Strict).expect("roundtrip archive");

        assert_eq!(doc.encode(WriteProfile::Lossless), bytes);
    }

    #[test]
    fn editor_roundtrip_without_mutations_is_identity() {
        let bytes = synthetic_rsli(
            &[
                SyntheticEntry::stored(b"A", 0, b"alpha"),
                SyntheticEntry::stored(b"B", 1, b"beta"),
            ],
            true,
            0x7777,
            None,
        );

        let doc = decode(arc(bytes.clone()), ReadProfile::Strict).expect("editable archive");
        let editor = doc.editor().expect("editor");

        assert_eq!(editor.encode().expect("editor encode"), bytes);
    }

    #[test]
    fn editor_can_mutate_names_and_payloads() {
        let bytes = synthetic_rsli(
            &[
                SyntheticEntry::stored(b"A", 0, b"alpha"),
                SyntheticEntry::stored(b"B", 1, b"beta"),
            ],
            true,
            0x7778,
            None,
        );

        let doc = decode(arc(bytes), ReadProfile::Strict).expect("editable archive");
        let mut editor = doc.editor().expect("editor");
        editor.set_name(EntryId(1), b"ZETA").expect("edit name");
        let repacked = deflate_bytes(b"repacked-alpha");
        editor
            .set_packed_payload(EntryId(0), repacked, 14)
            .expect("edit packed payload");
        editor
            .set_method(EntryId(0), RsliMethod::RawDeflate)
            .expect("edit method");

        let rebuilt = editor.encode().expect("editor encode");
        let doc = decode(arc(rebuilt), ReadProfile::Strict).expect("repacked archive");

        let renamed = doc.find("ZETA").expect("renamed entry");
        assert_eq!(doc.load(renamed).expect("renamed payload"), b"beta");
        let original = doc
            .find("A")
            .or_else(|| doc.find("a"))
            .expect("original renamed entry fallback");
        assert_eq!(
            doc.load(original).expect("updated payload"),
            b"repacked-alpha"
        );
        assert_eq!(
            doc.entries()[original.0 as usize].method,
            RsliMethod::RawDeflate
        );
    }

    #[test]
    fn set_method_rejects_unknown_authoring_method() {
        let bytes = synthetic_rsli(
            &[SyntheticEntry::stored(b"A", 0, b"alpha")],
            true,
            0x7780,
            None,
        );
        let doc = decode(arc(bytes), ReadProfile::Strict).expect("editable archive");
        let mut editor = doc.editor().expect("editor");

        assert!(matches!(
            editor.set_method(EntryId(0), RsliMethod::Unknown(0x1E0)),
            Err(RsliMutationError::UnsupportedMethod { .. })
        ));
    }

    #[test]
    fn decode_rejects_entry_count_above_limit() {
        let bytes = synthetic_rsli(
            &[
                SyntheticEntry::stored(b"A", 0, b"alpha"),
                SyntheticEntry::stored(b"B", 1, b"beta"),
            ],
            true,
            0x7781,
            None,
        );

        assert!(matches!(
            decode_with_limits(
                arc(bytes),
                ReadProfile::Strict,
                DecodeLimits {
                    max_entries: 1,
                    ..DecodeLimits::default()
                }
            ),
            Err(RsliError::Binary(DecodeError::LimitExceeded {
                count: 2,
                limit: 1
            }))
        ));
    }

    #[test]
    fn stored_entries_require_exact_packed_size() {
        let bytes = synthetic_rsli(
            &[SyntheticEntry::with_payload(
                b"A",
                0x000,
                0,
                b"ok",
                b"ok!".to_vec(),
            )],
            true,
            0x7782,
            None,
        );
        let doc = decode(arc(bytes), ReadProfile::Strict).expect("stored archive");

        assert!(matches!(
            doc.load(EntryId(0)),
            Err(RsliError::OutputSizeMismatch {
                expected: 2,
                got: 3
            })
        ));
    }

    #[test]
    fn load_rejects_unpacked_size_above_limit_before_allocation() {
        let bytes = synthetic_rsli(
            &[SyntheticEntry::stored(b"A", 0, b"alpha")],
            true,
            0x7783,
            None,
        );
        let doc = decode(arc(bytes), ReadProfile::Strict).expect("stored archive");

        assert!(matches!(
            doc.load_with_limits(
                EntryId(0),
                DecodeLimits {
                    max_decoded_entry_bytes: 4,
                    max_total_decoded_bytes: 4,
                    ..DecodeLimits::default()
                }
            ),
            Err(RsliError::Binary(DecodeError::LimitExceeded {
                count: 5,
                limit: 4
            }))
        ));
    }

    #[test]
    fn editor_rejects_unknown_entry_id_and_invalid_name() {
        let bytes = synthetic_rsli(
            &[SyntheticEntry::stored(b"A", 0, b"alpha")],
            true,
            0x7779,
            None,
        );
        let doc = decode(arc(bytes), ReadProfile::Strict).expect("editable archive");
        let mut editor = doc.editor().expect("editor");

        assert!(matches!(
            editor.set_name(EntryId(10), b"BAD"),
            Err(RsliMutationError::EntryNotFound { id: EntryId(10) })
        ));
        assert!(matches!(
            editor.set_name(EntryId(0), b"TOO_LONG_ENTRY_NAME"),
            Err(RsliMutationError::AuthoringNameTooLong { .. })
        ));
    }

    #[test]
    fn generated_supported_methods_decode_expected_bytes() {
        let cases = [
            (0x000, b"STO".as_slice(), b"ok".as_slice(), b"ok".to_vec()),
            (
                0x020,
                b"XOR".as_slice(),
                b"ok".as_slice(),
                xor_stream(b"ok", 0),
            ),
            (
                0x040,
                b"LZS".as_slice(),
                b"ok".as_slice(),
                vec![0b0000_0011, b'o', b'k'],
            ),
            (
                0x060,
                b"XLZ".as_slice(),
                b"ok".as_slice(),
                xor_stream(&[0b0000_0011, b'o', b'k'], 0),
            ),
            (0x080, b"ADP".as_slice(), b"t".as_slice(), vec![0x00]),
            (
                0x0A0,
                b"XAD".as_slice(),
                b"t".as_slice(),
                xor_stream(&[0x00], 0),
            ),
            (
                0x100,
                b"DEF".as_slice(),
                b"ok".as_slice(),
                vec![0x01, 0x02, 0x00, 0xFD, 0xFF, b'o', b'k'],
            ),
        ];

        for (idx, (method, name, expected, packed)) in cases.iter().enumerate() {
            let bytes = synthetic_rsli(
                &[SyntheticEntry::with_payload(
                    name,
                    *method,
                    0,
                    expected,
                    packed.clone(),
                )],
                true,
                u16::try_from(idx).expect("case index"),
                None,
            );
            let doc = decode(arc(bytes), ReadProfile::Strict).expect("generated method archive");
            assert_eq!(
                doc.load(EntryId(0)).expect("generated method payload"),
                *expected
            );
        }
    }

    #[test]
    fn arbitrary_small_inputs_do_not_panic() {
        for len in 0..128usize {
            let mut bytes = vec![0u8; len];
            if len >= 4 {
                bytes[0..4].copy_from_slice(b"NL\0\x01");
            }
            if len >= 6 {
                bytes[4..6].copy_from_slice(&((len % 8) as i16).to_le_bytes());
            }
            if len >= 24 {
                bytes[20..24].copy_from_slice(&0x1357u32.to_le_bytes());
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
    fn licensed_corpora_rsli_roundtrip_gates() {
        let part1 = corpus_gate("IS", 2).expect("part 1 RsLi gate");
        let part2 = corpus_gate("IS2", 2).expect("part 2 RsLi gate");

        assert!(part1.entries > 0);
        assert!(part2.entries > 0);
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_part1_rsli_method_distribution_baseline() {
        let stats = corpus_gate("IS", 2).expect("part 1 RsLi gate");

        assert_eq!(
            stats.methods,
            RsliMethodCounts {
                stored: 0,
                xor_only: 0,
                lzss: 2,
                xor_lzss: 0,
                adaptive_lzss: 0,
                xor_adaptive_lzss: 0,
                raw_deflate: 24,
                unknown: 0,
            }
        );
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_part2_rsli_method_distribution_baseline() {
        let stats = corpus_gate("IS2", 2).expect("part 2 RsLi gate");

        assert_eq!(
            stats.methods,
            RsliMethodCounts {
                stored: 0,
                xor_only: 0,
                lzss: 2,
                xor_lzss: 0,
                adaptive_lzss: 0,
                xor_adaptive_lzss: 0,
                raw_deflate: 24,
                unknown: 0,
            }
        );
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_corpora_rsli_quirk_is_only_approved_interf8_tex() {
        let part1 = corpus_gate("IS", 2).expect("part 1 RsLi gate");
        let part2 = corpus_gate("IS2", 2).expect("part 2 RsLi gate");

        assert_eq!(
            part1.eof_plus_one_entries,
            vec!["sprites.lib:INTERF8.TEX".to_string()]
        );
        assert_eq!(
            part2.eof_plus_one_entries,
            vec!["sprites.lib:INTERF8.TEX".to_string()]
        );
        assert_strict_profile_only_rejects_approved_quirk("IS");
        assert_strict_profile_only_rejects_approved_quirk("IS2");
    }

    #[derive(Clone, Debug, Default, Eq, PartialEq)]
    struct RsliMethodCounts {
        stored: usize,
        xor_only: usize,
        lzss: usize,
        xor_lzss: usize,
        adaptive_lzss: usize,
        xor_adaptive_lzss: usize,
        raw_deflate: usize,
        unknown: usize,
    }

    impl RsliMethodCounts {
        fn add(&mut self, method: RsliMethod) {
            match method {
                RsliMethod::Stored => self.stored += 1,
                RsliMethod::XorOnly => self.xor_only += 1,
                RsliMethod::Lzss => self.lzss += 1,
                RsliMethod::XorLzss => self.xor_lzss += 1,
                RsliMethod::AdaptiveLzss => self.adaptive_lzss += 1,
                RsliMethod::XorAdaptiveLzss => self.xor_adaptive_lzss += 1,
                RsliMethod::RawDeflate => self.raw_deflate += 1,
                RsliMethod::Unknown(_) => self.unknown += 1,
            }
        }
    }

    #[derive(Clone, Debug, Default, Eq, PartialEq)]
    struct CorpusGateResult {
        entries: usize,
        methods: RsliMethodCounts,
        eof_plus_one_entries: Vec<String>,
    }

    fn corpus_gate(name: &str, expected_files: usize) -> Result<CorpusGateResult, String> {
        let files = corpus_files(name)?;
        if files.len() != expected_files {
            return Err(format!(
                "{name}: expected {expected_files} RsLi files, got {}",
                files.len()
            ));
        }

        let mut entries = 0usize;
        let mut methods = RsliMethodCounts::default();
        let mut eof_plus_one_entries = Vec::new();
        for path in &files {
            let bytes = fs::read(path).map_err(|err| format!("{}: {err}", path.display()))?;
            let doc = decode(arc(bytes.clone()), ReadProfile::Compatible)
                .map_err(|err| format!("{}: {err}", path.display()))?;
            entries = entries
                .checked_add(doc.entry_count())
                .ok_or_else(|| "entry count overflow".to_string())?;
            for (idx, entry) in doc.entries().iter().enumerate() {
                methods.add(entry.method);
                if entry.method == RsliMethod::RawDeflate
                    && entry.data_offset + u64::from(entry.packed_size) == bytes.len() as u64 + 1
                {
                    eof_plus_one_entries.push(format!(
                        "{}:{}",
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("<unknown>"),
                        entry.name
                    ));
                }
                let id = EntryId(u32::try_from(idx).map_err(|_| "entry id overflow")?);
                let found = doc
                    .find(&entry.name)
                    .ok_or_else(|| format!("lookup failed: {}", path.display()))?;
                if found != id {
                    return Err(format!("lookup mismatch: {}", path.display()));
                }
                let unpacked = doc
                    .load(id)
                    .map_err(|err| format!("{} entry #{idx}: {err}", path.display()))?;
                if unpacked.len()
                    != usize::try_from(entry.unpacked_size).map_err(|_| "size overflow")?
                {
                    return Err(format!("unpacked size mismatch: {}", path.display()));
                }
                let packed = doc
                    .load_packed(id)
                    .map_err(|err| format!("{} entry #{idx}: {err}", path.display()))?;
                if packed.packed.is_empty() && entry.packed_size != 0 {
                    return Err(format!(
                        "packed payload unexpectedly empty: {}",
                        path.display()
                    ));
                }
            }
            if doc.encode(WriteProfile::Lossless) != bytes {
                return Err(format!("lossless roundtrip mismatch: {}", path.display()));
            }
        }
        Ok(CorpusGateResult {
            entries,
            methods,
            eof_plus_one_entries,
        })
    }

    fn corpus_files(name: &str) -> Result<Vec<PathBuf>, String> {
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
        collect_rsli_files(&root, &mut files).map_err(|err| err.to_string())?;
        files.sort();
        Ok(files)
    }

    fn assert_strict_profile_only_rejects_approved_quirk(name: &str) {
        for path in corpus_files(name).expect("licensed RsLi files") {
            let bytes = fs::read(&path).expect("licensed RsLi bytes");
            let doc = decode(arc(bytes.clone()), ReadProfile::Compatible)
                .expect("compatible licensed RsLi");
            let mut eof_plus_one_names = Vec::new();
            for entry in doc.entries() {
                if entry.method == RsliMethod::RawDeflate
                    && entry.data_offset + u64::from(entry.packed_size) == bytes.len() as u64 + 1
                {
                    eof_plus_one_names.push(entry.name.clone());
                }
            }

            let strict = decode(arc(bytes), ReadProfile::Strict);
            if eof_plus_one_names.is_empty() {
                assert!(
                    strict.is_ok(),
                    "strict profile should accept {}",
                    path.display()
                );
            } else {
                assert_eq!(eof_plus_one_names, vec!["INTERF8.TEX".to_string()]);
                assert!(
                    matches!(
                        strict,
                        Err(RsliError::DeflateEofPlusOneQuirkRejected { .. })
                    ),
                    "strict profile should only reject the approved EOF+1 quirk in {}",
                    path.display()
                );
            }
        }
    }

    fn collect_rsli_files(root: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
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
                collect_rsli_files(&path, out)?;
                continue;
            }
            if path.is_file() {
                let bytes = fs::read(&path)?;
                if bytes.get(0..4) == Some(b"NL\0\x01") {
                    out.push(path);
                }
            }
        }
        Ok(())
    }

    fn arc(bytes: Vec<u8>) -> Arc<[u8]> {
        Arc::from(bytes.into_boxed_slice())
    }

    #[derive(Clone, Debug)]
    struct SyntheticEntry {
        name: Vec<u8>,
        method_raw: u32,
        sort_to_original: i16,
        unpacked_size: u32,
        declared_packed_size: u32,
        packed: Vec<u8>,
    }

    impl SyntheticEntry {
        fn stored(name: &[u8], sort_to_original: i16, payload: &[u8]) -> Self {
            Self::with_payload(name, 0x000, sort_to_original, payload, payload.to_vec())
        }

        fn with_payload(
            name: &[u8],
            method_raw: u32,
            sort_to_original: i16,
            unpacked: &[u8],
            packed: Vec<u8>,
        ) -> Self {
            let declared_packed_size = u32::try_from(packed.len()).expect("synthetic packed size");
            Self::with_declared_packed_size(
                name,
                method_raw,
                sort_to_original,
                unpacked,
                packed,
                declared_packed_size,
            )
        }

        fn with_declared_packed_size(
            name: &[u8],
            method_raw: u32,
            sort_to_original: i16,
            unpacked: &[u8],
            packed: Vec<u8>,
            declared_packed_size: u32,
        ) -> Self {
            Self {
                name: name.to_vec(),
                method_raw,
                sort_to_original,
                unpacked_size: u32::try_from(unpacked.len()).expect("synthetic unpacked size"),
                declared_packed_size,
                packed,
            }
        }
    }

    fn synthetic_rsli(
        entries: &[SyntheticEntry],
        presorted: bool,
        xor_seed: u16,
        overlay: Option<u32>,
    ) -> Vec<u8> {
        let count = i16::try_from(entries.len()).expect("synthetic entry count");
        let table_len = entries
            .len()
            .checked_mul(32)
            .expect("synthetic table length");
        let payload_offset = 32usize
            .checked_add(table_len)
            .expect("synthetic payload offset");
        let overlay = overlay.unwrap_or(0);

        let mut header = [0u8; 32];
        header[0..4].copy_from_slice(b"NL\0\x01");
        header[4..6].copy_from_slice(&count.to_le_bytes());
        if presorted {
            header[14..16].copy_from_slice(&0xABBAu16.to_le_bytes());
        }
        header[20..24].copy_from_slice(&u32::from(xor_seed).to_le_bytes());

        let mut table_plain = Vec::with_capacity(table_len);
        let mut cursor = payload_offset;
        for entry in entries {
            let mut row = [0u8; 32];
            let name_len = entry.name.len().min(12);
            row[0..name_len].copy_from_slice(&entry.name[..name_len]);
            row[16..18].copy_from_slice(
                &i16::try_from(entry.method_raw)
                    .expect("synthetic method fits")
                    .to_le_bytes(),
            );
            row[18..20].copy_from_slice(&entry.sort_to_original.to_le_bytes());
            row[20..24].copy_from_slice(&entry.unpacked_size.to_le_bytes());
            let raw_offset = u32::try_from(cursor)
                .expect("synthetic offset")
                .checked_sub(overlay)
                .expect("synthetic overlay precedes payload");
            row[24..28].copy_from_slice(&raw_offset.to_le_bytes());
            row[28..32].copy_from_slice(&entry.declared_packed_size.to_le_bytes());
            table_plain.extend_from_slice(&row);
            cursor = cursor
                .checked_add(entry.packed.len())
                .expect("synthetic payload cursor");
        }

        let mut bytes = Vec::with_capacity(cursor + 6);
        bytes.extend_from_slice(&header);
        bytes.extend_from_slice(&xor_stream(&table_plain, xor_seed));
        for entry in entries {
            bytes.extend_from_slice(&entry.packed);
        }
        if overlay != 0 {
            bytes.extend_from_slice(b"AO");
            bytes.extend_from_slice(&overlay.to_le_bytes());
        }
        bytes
    }

    fn deflate_bytes(plain: &[u8]) -> Vec<u8> {
        use std::io::Write;

        let mut encoder =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(plain).expect("deflate write");
        encoder.finish().expect("deflate finish")
    }

    fn two_plain_rows_for_transform_test() -> Vec<[u8; 32]> {
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        a[0] = b'A';
        b[0] = b'B';
        a[18..20].copy_from_slice(&0i16.to_le_bytes());
        b[18..20].copy_from_slice(&1i16.to_le_bytes());
        vec![a, b]
    }
}
