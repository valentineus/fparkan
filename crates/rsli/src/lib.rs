pub mod error;

use crate::error::Error;
use common::{OutputBuffer, ResourceData};
use flate2::read::{DeflateDecoder, ZlibDecoder};
use std::cmp::Ordering;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Clone, Debug)]
pub struct OpenOptions {
    pub allow_ao_trailer: bool,
    pub allow_deflate_eof_plus_one: bool,
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self {
            allow_ao_trailer: true,
            allow_deflate_eof_plus_one: true,
        }
    }
}

#[derive(Debug)]
pub struct Library {
    bytes: Arc<[u8]>,
    entries: Vec<EntryRecord>,
    #[cfg(test)]
    header_raw: [u8; 32],
    #[cfg(test)]
    table_plain_original: Vec<u8>,
    #[cfg(test)]
    xor_seed: u32,
    #[cfg(test)]
    source_size: usize,
    #[cfg(test)]
    trailer_raw: Option<[u8; 6]>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct EntryId(pub u32);

#[derive(Clone, Debug)]
pub struct EntryMeta {
    pub name: String,
    pub flags: i32,
    pub method: PackMethod,
    pub data_offset: u64,
    pub packed_size: u32,
    pub unpacked_size: u32,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PackMethod {
    None,
    XorOnly,
    Lzss,
    XorLzss,
    LzssHuffman,
    XorLzssHuffman,
    Deflate,
    Unknown(u32),
}

#[derive(Copy, Clone, Debug)]
pub struct EntryRef<'a> {
    pub id: EntryId,
    pub meta: &'a EntryMeta,
}

pub struct PackedResource {
    pub meta: EntryMeta,
    pub packed: Vec<u8>,
}

#[derive(Clone, Debug)]
struct EntryRecord {
    meta: EntryMeta,
    name_raw: [u8; 12],
    sort_to_original: i16,
    key16: u16,
    #[cfg(test)]
    data_offset_raw: u32,
    packed_size_declared: u32,
    packed_size_available: usize,
    effective_offset: usize,
}

impl Library {
    pub fn open_path(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_path_with(path, OpenOptions::default())
    }

    pub fn open_path_with(path: impl AsRef<Path>, opts: OpenOptions) -> Result<Self> {
        let bytes = fs::read(path.as_ref())?;
        let arc: Arc<[u8]> = Arc::from(bytes.into_boxed_slice());
        parse_library(arc, opts)
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub fn entries(&self) -> impl Iterator<Item = EntryRef<'_>> {
        self.entries
            .iter()
            .enumerate()
            .map(|(idx, entry)| EntryRef {
                id: EntryId(u32::try_from(idx).expect("entry count validated at parse")),
                meta: &entry.meta,
            })
    }

    pub fn find(&self, name: &str) -> Option<EntryId> {
        if self.entries.is_empty() {
            return None;
        }

        const MAX_INLINE_NAME: usize = 12;

        // Fast path: use stack allocation for short ASCII names (95% of cases)
        if name.len() <= MAX_INLINE_NAME && name.is_ascii() {
            let mut buf = [0u8; MAX_INLINE_NAME];
            for (i, &b) in name.as_bytes().iter().enumerate() {
                buf[i] = b.to_ascii_uppercase();
            }
            return self.find_impl(&buf[..name.len()]);
        }

        // Slow path: heap allocation for long or non-ASCII names
        let query = name.to_ascii_uppercase();
        self.find_impl(query.as_bytes())
    }

    fn find_impl(&self, query_bytes: &[u8]) -> Option<EntryId> {
        // Binary search
        let mut low = 0usize;
        let mut high = self.entries.len();
        while low < high {
            let mid = low + (high - low) / 2;
            let idx = self.entries[mid].sort_to_original;
            if idx < 0 {
                break;
            }
            let idx = usize::try_from(idx).ok()?;
            if idx >= self.entries.len() {
                break;
            }

            let cmp = cmp_c_string(query_bytes, c_name_bytes(&self.entries[idx].name_raw));
            match cmp {
                Ordering::Less => high = mid,
                Ordering::Greater => low = mid + 1,
                Ordering::Equal => {
                    return Some(EntryId(
                        u32::try_from(idx).expect("entry count validated at parse"),
                    ))
                }
            }
        }

        // Linear fallback search
        self.entries.iter().enumerate().find_map(|(idx, entry)| {
            if cmp_c_string(query_bytes, c_name_bytes(&entry.name_raw)) == Ordering::Equal {
                Some(EntryId(
                    u32::try_from(idx).expect("entry count validated at parse"),
                ))
            } else {
                None
            }
        })
    }

    pub fn get(&self, id: EntryId) -> Option<EntryRef<'_>> {
        let idx = usize::try_from(id.0).ok()?;
        let entry = self.entries.get(idx)?;
        Some(EntryRef {
            id,
            meta: &entry.meta,
        })
    }

    pub fn load(&self, id: EntryId) -> Result<Vec<u8>> {
        let entry = self.entry_by_id(id)?;
        let packed = self.packed_slice(entry)?;
        decode_payload(
            packed,
            entry.meta.method,
            entry.key16,
            entry.meta.unpacked_size,
        )
    }

    pub fn load_into(&self, id: EntryId, out: &mut dyn OutputBuffer) -> Result<usize> {
        let decoded = self.load(id)?;
        out.write_exact(&decoded)?;
        Ok(decoded.len())
    }

    pub fn load_packed(&self, id: EntryId) -> Result<PackedResource> {
        let entry = self.entry_by_id(id)?;
        let packed = self.packed_slice(entry)?.to_vec();
        Ok(PackedResource {
            meta: entry.meta.clone(),
            packed,
        })
    }

    pub fn unpack(&self, packed: &PackedResource) -> Result<Vec<u8>> {
        let key16 = self.resolve_key_for_meta(&packed.meta).unwrap_or(0);

        let method = packed.meta.method;
        if needs_xor_key(method) && self.resolve_key_for_meta(&packed.meta).is_none() {
            return Err(Error::CorruptEntryTable(
                "cannot resolve XOR key for packed resource",
            ));
        }

        decode_payload(&packed.packed, method, key16, packed.meta.unpacked_size)
    }

    pub fn load_fast(&self, id: EntryId) -> Result<ResourceData<'_>> {
        let entry = self.entry_by_id(id)?;
        if entry.meta.method == PackMethod::None {
            let packed = self.packed_slice(entry)?;
            let size =
                usize::try_from(entry.meta.unpacked_size).map_err(|_| Error::IntegerOverflow)?;
            if packed.len() < size {
                return Err(Error::OutputSizeMismatch {
                    expected: entry.meta.unpacked_size,
                    got: u32::try_from(packed.len()).unwrap_or(u32::MAX),
                });
            }
            return Ok(ResourceData::Borrowed(&packed[..size]));
        }
        Ok(ResourceData::Owned(self.load(id)?))
    }

    fn entry_by_id(&self, id: EntryId) -> Result<&EntryRecord> {
        let idx = usize::try_from(id.0).map_err(|_| Error::IntegerOverflow)?;
        self.entries
            .get(idx)
            .ok_or_else(|| Error::EntryIdOutOfRange {
                id: id.0,
                entry_count: self.entries.len().try_into().unwrap_or(u32::MAX),
            })
    }

    fn packed_slice<'a>(&'a self, entry: &EntryRecord) -> Result<&'a [u8]> {
        let start = entry.effective_offset;
        let end = start
            .checked_add(entry.packed_size_available)
            .ok_or(Error::IntegerOverflow)?;
        self.bytes
            .get(start..end)
            .ok_or(Error::EntryDataOutOfBounds {
                id: 0,
                offset: u64::try_from(start).unwrap_or(u64::MAX),
                size: entry.packed_size_declared,
                file_len: u64::try_from(self.bytes.len()).unwrap_or(u64::MAX),
            })
    }

    fn resolve_key_for_meta(&self, meta: &EntryMeta) -> Option<u16> {
        self.entries
            .iter()
            .find(|entry| {
                entry.meta.name == meta.name
                    && entry.meta.flags == meta.flags
                    && entry.meta.data_offset == meta.data_offset
                    && entry.meta.packed_size == meta.packed_size
                    && entry.meta.unpacked_size == meta.unpacked_size
                    && entry.meta.method == meta.method
            })
            .map(|entry| entry.key16)
    }

    #[cfg(test)]
    fn rebuild_from_parsed_metadata(&self) -> Result<Vec<u8>> {
        let trailer_len = usize::from(self.trailer_raw.is_some()) * 6;
        let pre_trailer_size = self
            .source_size
            .checked_sub(trailer_len)
            .ok_or(Error::IntegerOverflow)?;

        let count = self.entries.len();
        let table_len = count.checked_mul(32).ok_or(Error::IntegerOverflow)?;
        let table_end = 32usize
            .checked_add(table_len)
            .ok_or(Error::IntegerOverflow)?;
        if pre_trailer_size < table_end {
            return Err(Error::EntryTableOutOfBounds {
                table_offset: 32,
                table_len: u64::try_from(table_len).map_err(|_| Error::IntegerOverflow)?,
                file_len: u64::try_from(pre_trailer_size).map_err(|_| Error::IntegerOverflow)?,
            });
        }

        let mut out = vec![0u8; pre_trailer_size];
        out[0..32].copy_from_slice(&self.header_raw);
        let encrypted_table =
            xor_stream(&self.table_plain_original, (self.xor_seed & 0xFFFF) as u16);
        out[32..table_end].copy_from_slice(&encrypted_table);

        let mut occupied = vec![false; pre_trailer_size];
        for byte in occupied.iter_mut().take(table_end) {
            *byte = true;
        }

        for (idx, entry) in self.entries.iter().enumerate() {
            let packed = self
                .load_packed(EntryId(
                    u32::try_from(idx).expect("entry count validated at parse"),
                ))?
                .packed;
            let start =
                usize::try_from(entry.data_offset_raw).map_err(|_| Error::IntegerOverflow)?;
            for (offset, byte) in packed.iter().copied().enumerate() {
                let pos = start.checked_add(offset).ok_or(Error::IntegerOverflow)?;
                if pos >= out.len() {
                    return Err(Error::PackedSizePastEof {
                        id: u32::try_from(idx).expect("entry count validated at parse"),
                        offset: u64::from(entry.data_offset_raw),
                        packed_size: entry.packed_size_declared,
                        file_len: u64::try_from(out.len()).map_err(|_| Error::IntegerOverflow)?,
                    });
                }
                if occupied[pos] && out[pos] != byte {
                    return Err(Error::CorruptEntryTable("packed payload overlap conflict"));
                }
                out[pos] = byte;
                occupied[pos] = true;
            }
        }

        if let Some(trailer) = self.trailer_raw {
            out.extend_from_slice(&trailer);
        }
        Ok(out)
    }
}

fn parse_library(bytes: Arc<[u8]>, opts: OpenOptions) -> Result<Library> {
    if bytes.len() < 32 {
        return Err(Error::EntryTableOutOfBounds {
            table_offset: 32,
            table_len: 0,
            file_len: u64::try_from(bytes.len()).map_err(|_| Error::IntegerOverflow)?,
        });
    }

    let mut header_raw = [0u8; 32];
    header_raw.copy_from_slice(&bytes[0..32]);

    if &bytes[0..2] != b"NL" {
        let mut got = [0u8; 2];
        got.copy_from_slice(&bytes[0..2]);
        return Err(Error::InvalidMagic { got });
    }
    if bytes[3] != 0x01 {
        return Err(Error::UnsupportedVersion { got: bytes[3] });
    }

    let entry_count = i16::from_le_bytes([bytes[4], bytes[5]]);
    if entry_count < 0 {
        return Err(Error::InvalidEntryCount { got: entry_count });
    }
    let count = usize::try_from(entry_count).map_err(|_| Error::IntegerOverflow)?;

    // Validate entry_count fits in u32 (required for EntryId)
    if count > u32::MAX as usize {
        return Err(Error::TooManyEntries { got: count });
    }

    let xor_seed = u32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);

    let table_len = count.checked_mul(32).ok_or(Error::IntegerOverflow)?;
    let table_offset = 32usize;
    let table_end = table_offset
        .checked_add(table_len)
        .ok_or(Error::IntegerOverflow)?;
    if table_end > bytes.len() {
        return Err(Error::EntryTableOutOfBounds {
            table_offset: u64::try_from(table_offset).map_err(|_| Error::IntegerOverflow)?,
            table_len: u64::try_from(table_len).map_err(|_| Error::IntegerOverflow)?,
            file_len: u64::try_from(bytes.len()).map_err(|_| Error::IntegerOverflow)?,
        });
    }

    let table_enc = &bytes[table_offset..table_end];
    let table_plain_original = xor_stream(table_enc, (xor_seed & 0xFFFF) as u16);
    if table_plain_original.len() != table_len {
        return Err(Error::EntryTableDecryptFailed);
    }

    let (overlay, trailer_raw) = parse_ao_trailer(&bytes, opts.allow_ao_trailer)?;
    #[cfg(not(test))]
    let _ = trailer_raw;

    let mut entries = Vec::with_capacity(count);
    for idx in 0..count {
        let row = &table_plain_original[idx * 32..(idx + 1) * 32];

        let mut name_raw = [0u8; 12];
        name_raw.copy_from_slice(&row[0..12]);

        let flags_signed = i16::from_le_bytes([row[16], row[17]]);
        let sort_to_original = i16::from_le_bytes([row[18], row[19]]);
        let unpacked_size = u32::from_le_bytes([row[20], row[21], row[22], row[23]]);
        let data_offset_raw = u32::from_le_bytes([row[24], row[25], row[26], row[27]]);
        let packed_size_declared = u32::from_le_bytes([row[28], row[29], row[30], row[31]]);

        let method_raw = (flags_signed as u16 as u32) & 0x1E0;
        let method = parse_method(method_raw);

        let effective_offset_u64 = u64::from(data_offset_raw)
            .checked_add(u64::from(overlay))
            .ok_or(Error::IntegerOverflow)?;
        let effective_offset =
            usize::try_from(effective_offset_u64).map_err(|_| Error::IntegerOverflow)?;

        let packed_size_usize =
            usize::try_from(packed_size_declared).map_err(|_| Error::IntegerOverflow)?;
        let mut packed_size_available = packed_size_usize;

        let end = effective_offset_u64
            .checked_add(u64::from(packed_size_declared))
            .ok_or(Error::IntegerOverflow)?;
        let file_len_u64 = u64::try_from(bytes.len()).map_err(|_| Error::IntegerOverflow)?;

        if end > file_len_u64 {
            if method_raw == 0x100 && end == file_len_u64 + 1 {
                if opts.allow_deflate_eof_plus_one {
                    packed_size_available = packed_size_available
                        .checked_sub(1)
                        .ok_or(Error::IntegerOverflow)?;
                } else {
                    return Err(Error::DeflateEofPlusOneQuirkRejected {
                        id: u32::try_from(idx).expect("entry count validated at parse"),
                    });
                }
            } else {
                return Err(Error::PackedSizePastEof {
                    id: u32::try_from(idx).expect("entry count validated at parse"),
                    offset: effective_offset_u64,
                    packed_size: packed_size_declared,
                    file_len: file_len_u64,
                });
            }
        }

        let available_end = effective_offset
            .checked_add(packed_size_available)
            .ok_or(Error::IntegerOverflow)?;
        if available_end > bytes.len() {
            return Err(Error::EntryDataOutOfBounds {
                id: u32::try_from(idx).expect("entry count validated at parse"),
                offset: effective_offset_u64,
                size: packed_size_declared,
                file_len: file_len_u64,
            });
        }

        let name = decode_name(c_name_bytes(&name_raw));

        entries.push(EntryRecord {
            meta: EntryMeta {
                name,
                flags: i32::from(flags_signed),
                method,
                data_offset: effective_offset_u64,
                packed_size: packed_size_declared,
                unpacked_size,
            },
            name_raw,
            sort_to_original,
            key16: sort_to_original as u16,
            #[cfg(test)]
            data_offset_raw,
            packed_size_declared,
            packed_size_available,
            effective_offset,
        });
    }

    let presorted_flag = u16::from_le_bytes([bytes[14], bytes[15]]);
    if presorted_flag == 0xABBA {
        for entry in &entries {
            let idx = i32::from(entry.sort_to_original);
            if idx < 0 || usize::try_from(idx).map_err(|_| Error::IntegerOverflow)? >= count {
                return Err(Error::CorruptEntryTable(
                    "sort_to_original is not a valid permutation index",
                ));
            }
        }
    } else {
        let mut sorted: Vec<usize> = (0..count).collect();
        sorted.sort_by(|a, b| {
            cmp_c_string(
                c_name_bytes(&entries[*a].name_raw),
                c_name_bytes(&entries[*b].name_raw),
            )
        });
        for (idx, entry) in entries.iter_mut().enumerate() {
            entry.sort_to_original =
                i16::try_from(sorted[idx]).map_err(|_| Error::IntegerOverflow)?;
            entry.key16 = entry.sort_to_original as u16;
        }
    }

    #[cfg(test)]
    let source_size = bytes.len();

    Ok(Library {
        bytes,
        entries,
        #[cfg(test)]
        header_raw,
        #[cfg(test)]
        table_plain_original,
        #[cfg(test)]
        xor_seed,
        #[cfg(test)]
        source_size,
        #[cfg(test)]
        trailer_raw,
    })
}

fn parse_ao_trailer(bytes: &[u8], allow: bool) -> Result<(u32, Option<[u8; 6]>)> {
    if !allow || bytes.len() < 6 {
        return Ok((0, None));
    }

    if &bytes[bytes.len() - 6..bytes.len() - 4] != b"AO" {
        return Ok((0, None));
    }

    let mut trailer = [0u8; 6];
    trailer.copy_from_slice(&bytes[bytes.len() - 6..]);
    let overlay = u32::from_le_bytes([trailer[2], trailer[3], trailer[4], trailer[5]]);

    if u64::from(overlay) > u64::try_from(bytes.len()).map_err(|_| Error::IntegerOverflow)? {
        return Err(Error::MediaOverlayOutOfBounds {
            overlay,
            file_len: u64::try_from(bytes.len()).map_err(|_| Error::IntegerOverflow)?,
        });
    }

    Ok((overlay, Some(trailer)))
}

fn parse_method(raw: u32) -> PackMethod {
    match raw {
        0x000 => PackMethod::None,
        0x020 => PackMethod::XorOnly,
        0x040 => PackMethod::Lzss,
        0x060 => PackMethod::XorLzss,
        0x080 => PackMethod::LzssHuffman,
        0x0A0 => PackMethod::XorLzssHuffman,
        0x100 => PackMethod::Deflate,
        other => PackMethod::Unknown(other),
    }
}

fn decode_payload(
    packed: &[u8],
    method: PackMethod,
    key16: u16,
    unpacked_size: u32,
) -> Result<Vec<u8>> {
    let expected = usize::try_from(unpacked_size).map_err(|_| Error::IntegerOverflow)?;

    let out = match method {
        PackMethod::None => {
            if packed.len() < expected {
                return Err(Error::OutputSizeMismatch {
                    expected: unpacked_size,
                    got: u32::try_from(packed.len()).unwrap_or(u32::MAX),
                });
            }
            packed[..expected].to_vec()
        }
        PackMethod::XorOnly => {
            if packed.len() < expected {
                return Err(Error::OutputSizeMismatch {
                    expected: unpacked_size,
                    got: u32::try_from(packed.len()).unwrap_or(u32::MAX),
                });
            }
            xor_stream(&packed[..expected], key16)
        }
        PackMethod::Lzss => lzss_decompress_simple(packed, expected, None)?,
        PackMethod::XorLzss => {
            // Optimized: XOR on-the-fly during decompression instead of creating temp buffer
            lzss_decompress_simple(packed, expected, Some(key16))?
        }
        PackMethod::LzssHuffman => lzss_huffman_decompress(packed, expected, None)?,
        PackMethod::XorLzssHuffman => {
            // Optimized: XOR on-the-fly during decompression
            lzss_huffman_decompress(packed, expected, Some(key16))?
        }
        PackMethod::Deflate => decode_deflate(packed)?,
        PackMethod::Unknown(raw) => return Err(Error::UnsupportedMethod { raw }),
    };

    if out.len() != expected {
        return Err(Error::OutputSizeMismatch {
            expected: unpacked_size,
            got: u32::try_from(out.len()).unwrap_or(u32::MAX),
        });
    }

    Ok(out)
}

fn decode_deflate(packed: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut decoder = DeflateDecoder::new(packed);
    if decoder.read_to_end(&mut out).is_ok() {
        return Ok(out);
    }

    out.clear();
    let mut zlib = ZlibDecoder::new(packed);
    zlib.read_to_end(&mut out)
        .map_err(|_| Error::DecompressionFailed("deflate"))?;
    Ok(out)
}

struct XorState {
    lo: u8,
    hi: u8,
}

impl XorState {
    fn new(key16: u16) -> Self {
        Self {
            lo: (key16 & 0xFF) as u8,
            hi: ((key16 >> 8) & 0xFF) as u8,
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
    data.iter().map(|&b| state.decrypt_byte(b)).collect()
}

fn lzss_decompress_simple(
    data: &[u8],
    expected_size: usize,
    xor_key: Option<u16>,
) -> Result<Vec<u8>> {
    let mut ring = [0x20u8; 0x1000];
    let mut ring_pos = 0xFEEusize;
    let mut out = Vec::with_capacity(expected_size);
    let mut in_pos = 0usize;

    let mut control = 0u8;
    let mut bits_left = 0u8;

    // XOR state for on-the-fly decryption
    let mut xor_state = xor_key.map(XorState::new);

    // Helper to read byte with optional XOR decryption
    let read_byte = |pos: usize, state: &mut Option<XorState>| -> Option<u8> {
        let encrypted = data.get(pos).copied()?;
        Some(if let Some(ref mut s) = state {
            s.decrypt_byte(encrypted)
        } else {
            encrypted
        })
    };

    while out.len() < expected_size {
        if bits_left == 0 {
            let byte = read_byte(in_pos, &mut xor_state)
                .ok_or(Error::DecompressionFailed("lzss-simple: unexpected EOF"))?;
            control = byte;
            in_pos += 1;
            bits_left = 8;
        }

        if (control & 1) != 0 {
            let byte = read_byte(in_pos, &mut xor_state)
                .ok_or(Error::DecompressionFailed("lzss-simple: unexpected EOF"))?;
            in_pos += 1;

            out.push(byte);
            ring[ring_pos] = byte;
            ring_pos = (ring_pos + 1) & 0x0FFF;
        } else {
            let low = read_byte(in_pos, &mut xor_state)
                .ok_or(Error::DecompressionFailed("lzss-simple: unexpected EOF"))?;
            let high = read_byte(in_pos + 1, &mut xor_state)
                .ok_or(Error::DecompressionFailed("lzss-simple: unexpected EOF"))?;
            in_pos += 2;

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

    if out.len() != expected_size {
        return Err(Error::DecompressionFailed("lzss-simple"));
    }

    Ok(out)
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
) -> Result<Vec<u8>> {
    // TODO: Full optimization for Huffman variant (rare in practice)
    // For now, fallback to separate XOR step for Huffman
    if let Some(key) = xor_key {
        let decrypted = xor_stream(data, key);
        let mut decoder = LzhDecoder::new(&decrypted);
        decoder.decode(expected_size)
    } else {
        let mut decoder = LzhDecoder::new(data);
        decoder.decode(expected_size)
    }
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
    fn new(data: &'a [u8]) -> Self {
        let mut decoder = Self {
            bit_reader: BitReader::new(data),
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

    fn decode(&mut self, expected_size: usize) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(expected_size);

        while out.len() < expected_size {
            let c = self.decode_char();
            if c < 256 {
                let byte = c as u8;
                out.push(byte);
                self.text[self.ring_pos] = byte;
                self.ring_pos = (self.ring_pos + 1) & (LZH_N - 1);
            } else {
                let mut offset = self.decode_position();
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

        if out.len() != expected_size {
            return Err(Error::DecompressionFailed("lzss-huffman"));
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

    fn decode_char(&mut self) -> usize {
        let mut node = self.son[LZH_R];
        while node < LZH_T {
            let bit = usize::from(self.bit_reader.read_bit_or_zero());
            node = self.son[node + bit];
        }

        let c = node - LZH_T;
        self.update(c);
        c
    }

    fn decode_position(&mut self) -> usize {
        let i = self.bit_reader.read_bits_or_zero(8) as usize;
        let mut c = usize::from(self.d_code[i]) << 6;
        let mut j = usize::from(self.d_len[i]).saturating_sub(2);

        while j > 0 {
            j -= 1;
            c |= usize::from(self.bit_reader.read_bit_or_zero()) << j;
        }

        c | (i & 0x3F)
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
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte_pos: 0,
            bit_mask: 0x80,
        }
    }

    fn read_bit_or_zero(&mut self) -> u8 {
        let Some(byte) = self.data.get(self.byte_pos).copied() else {
            return 0;
        };

        let bit = if (byte & self.bit_mask) != 0 { 1 } else { 0 };
        self.bit_mask >>= 1;
        if self.bit_mask == 0 {
            self.bit_mask = 0x80;
            self.byte_pos = self.byte_pos.saturating_add(1);
        }
        bit
    }

    fn read_bits_or_zero(&mut self, bits: usize) -> u32 {
        let mut value = 0u32;
        for _ in 0..bits {
            value = (value << 1) | u32::from(self.read_bit_or_zero());
        }
        value
    }
}

fn decode_name(name: &[u8]) -> String {
    name.iter().map(|b| char::from(*b)).collect()
}

fn c_name_bytes(raw: &[u8; 12]) -> &[u8] {
    let len = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    &raw[..len]
}

fn cmp_c_string(a: &[u8], b: &[u8]) -> Ordering {
    let min_len = a.len().min(b.len());
    let mut idx = 0usize;
    while idx < min_len {
        if a[idx] != b[idx] {
            return a[idx].cmp(&b[idx]);
        }
        idx += 1;
    }
    a.len().cmp(&b.len())
}

fn needs_xor_key(method: PackMethod) -> bool {
    matches!(
        method,
        PackMethod::XorOnly | PackMethod::XorLzss | PackMethod::XorLzssHuffman
    )
}

#[cfg(test)]
mod tests;
