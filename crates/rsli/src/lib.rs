pub mod compress;
pub mod error;
pub mod parse;

use crate::compress::{
    decode_deflate, lzss_decompress_simple, lzss_huffman_decompress, xor_stream,
};
use crate::error::Error;
use crate::parse::{c_name_bytes, cmp_c_string, parse_library};
use common::{OutputBuffer, ResourceData};
use std::cmp::Ordering;
use std::fs;
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
    pub(crate) header_raw: [u8; 32],
    #[cfg(test)]
    pub(crate) table_plain_original: Vec<u8>,
    #[cfg(test)]
    pub(crate) xor_seed: u32,
    #[cfg(test)]
    pub(crate) source_size: usize,
    #[cfg(test)]
    pub(crate) trailer_raw: Option<[u8; 6]>,
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
pub(crate) struct EntryRecord {
    pub(crate) meta: EntryMeta,
    pub(crate) name_raw: [u8; 12],
    pub(crate) sort_to_original: i16,
    pub(crate) key16: u16,
    #[cfg(test)]
    pub(crate) data_offset_raw: u32,
    pub(crate) packed_size_declared: u32,
    pub(crate) packed_size_available: usize,
    pub(crate) effective_offset: usize,
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
        let packed = self.packed_slice(id, entry)?;
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
        let packed = self.packed_slice(id, entry)?.to_vec();
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
            let packed = self.packed_slice(id, entry)?;
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

    fn packed_slice<'a>(&'a self, id: EntryId, entry: &EntryRecord) -> Result<&'a [u8]> {
        let start = entry.effective_offset;
        let end = start
            .checked_add(entry.packed_size_available)
            .ok_or(Error::IntegerOverflow)?;
        self.bytes
            .get(start..end)
            .ok_or(Error::EntryDataOutOfBounds {
                id: id.0,
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
    pub(crate) fn rebuild_from_parsed_metadata(&self) -> Result<Vec<u8>> {
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

fn needs_xor_key(method: PackMethod) -> bool {
    matches!(
        method,
        PackMethod::XorOnly | PackMethod::XorLzss | PackMethod::XorLzssHuffman
    )
}

#[cfg(test)]
mod tests;
