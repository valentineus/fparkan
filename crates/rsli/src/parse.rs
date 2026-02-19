use crate::compress::xor::xor_stream;
use crate::error::Error;
use crate::{
    AoTrailer, EntryMeta, EntryRecord, Library, LibraryHeader, OpenOptions, PackMethod, Result,
};
use std::cmp::Ordering;
use std::sync::Arc;

pub fn parse_library(bytes: Arc<[u8]>, opts: OpenOptions) -> Result<Library> {
    if bytes.len() < 32 {
        return Err(Error::EntryTableOutOfBounds {
            table_offset: 32,
            table_len: 0,
            file_len: u64::try_from(bytes.len()).map_err(|_| Error::IntegerOverflow)?,
        });
    }

    let mut header_raw = [0u8; 32];
    header_raw.copy_from_slice(&bytes[0..32]);

    let mut magic = [0u8; 2];
    magic.copy_from_slice(&bytes[0..2]);
    if &magic != b"NL" {
        let mut got = [0u8; 2];
        got.copy_from_slice(&bytes[0..2]);
        return Err(Error::InvalidMagic { got });
    }
    let reserved = bytes[2];
    let version = bytes[3];
    if version != 0x01 {
        return Err(Error::UnsupportedVersion { got: version });
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

    let presorted_flag = u16::from_le_bytes([bytes[14], bytes[15]]);
    let xor_seed = u32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    let header = LibraryHeader {
        raw: header_raw,
        magic,
        reserved,
        version,
        entry_count,
        presorted_flag,
        xor_seed,
    };

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

    let mut entries = Vec::with_capacity(count);
    for idx in 0..count {
        let row = &table_plain_original[idx * 32..(idx + 1) * 32];

        let mut name_raw = [0u8; 12];
        name_raw.copy_from_slice(&row[0..12]);
        let mut service_tail = [0u8; 4];
        service_tail.copy_from_slice(&row[12..16]);

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
                        id: u32::try_from(idx).map_err(|_| Error::IntegerOverflow)?,
                    });
                }
            } else {
                return Err(Error::PackedSizePastEof {
                    id: u32::try_from(idx).map_err(|_| Error::IntegerOverflow)?,
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
                id: u32::try_from(idx).map_err(|_| Error::IntegerOverflow)?,
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
            service_tail,
            sort_to_original,
            key16: sort_to_original as u16,
            data_offset_raw,
            packed_size_declared,
            packed_size_available,
            effective_offset,
        });
    }

    if presorted_flag == 0xABBA {
        let mut seen = vec![false; count];
        for entry in &entries {
            let idx = i32::from(entry.sort_to_original);
            if idx < 0 {
                return Err(Error::CorruptEntryTable(
                    "sort_to_original is not a valid permutation index",
                ));
            }
            let idx = usize::try_from(idx).map_err(|_| Error::IntegerOverflow)?;
            if idx >= count {
                return Err(Error::CorruptEntryTable(
                    "sort_to_original is not a valid permutation index",
                ));
            }
            if seen[idx] {
                return Err(Error::CorruptEntryTable(
                    "sort_to_original is not a permutation",
                ));
            }
            seen[idx] = true;
        }
        if seen.iter().any(|value| !*value) {
            return Err(Error::CorruptEntryTable(
                "sort_to_original is not a permutation",
            ));
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
        header,
        ao_trailer: trailer_raw.map(|raw| AoTrailer { raw, overlay }),
        #[cfg(test)]
        table_plain_original,
        #[cfg(test)]
        source_size,
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

pub fn parse_method(raw: u32) -> PackMethod {
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

fn decode_name(name: &[u8]) -> String {
    name.iter().map(|b| char::from(*b)).collect()
}

pub fn c_name_bytes(raw: &[u8; 12]) -> &[u8] {
    let len = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    &raw[..len]
}

pub fn cmp_c_string(a: &[u8], b: &[u8]) -> Ordering {
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
