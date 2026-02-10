pub mod error;

use crate::error::Error;
use common::{OutputBuffer, ResourceData};
use core::ops::Range;
use std::cmp::Ordering;
use std::fs::{self, OpenOptions as FsOpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Clone, Debug, Default)]
pub struct OpenOptions {
    pub raw_mode: bool,
    pub sequential_hint: bool,
    pub prefetch_pages: bool,
}

#[derive(Clone, Debug, Default)]
pub enum OpenMode {
    #[default]
    ReadOnly,
    ReadWrite,
}

#[derive(Debug)]
pub struct Archive {
    bytes: Arc<[u8]>,
    entries: Vec<EntryRecord>,
    raw_mode: bool,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct EntryId(pub u32);

#[derive(Clone, Debug)]
pub struct EntryMeta {
    pub kind: u32,
    pub attr1: u32,
    pub attr2: u32,
    pub attr3: u32,
    pub name: String,
    pub data_offset: u64,
    pub data_size: u32,
    pub sort_index: u32,
}

#[derive(Copy, Clone, Debug)]
pub struct EntryRef<'a> {
    pub id: EntryId,
    pub meta: &'a EntryMeta,
}

#[derive(Clone, Debug)]
struct EntryRecord {
    meta: EntryMeta,
    name_raw: [u8; 36],
}

impl Archive {
    pub fn open_path(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_path_with(path, OpenMode::ReadOnly, OpenOptions::default())
    }

    pub fn open_path_with(
        path: impl AsRef<Path>,
        _mode: OpenMode,
        opts: OpenOptions,
    ) -> Result<Self> {
        let bytes = fs::read(path.as_ref())?;
        let arc: Arc<[u8]> = Arc::from(bytes.into_boxed_slice());
        Self::open_bytes(arc, opts)
    }

    pub fn open_bytes(bytes: Arc<[u8]>, opts: OpenOptions) -> Result<Self> {
        let (entries, _) = parse_archive(&bytes, opts.raw_mode)?;
        if opts.prefetch_pages {
            prefetch_pages(&bytes);
        }
        Ok(Self {
            bytes,
            entries,
            raw_mode: opts.raw_mode,
        })
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

        if !self.raw_mode {
            let mut low = 0usize;
            let mut high = self.entries.len();
            while low < high {
                let mid = low + (high - low) / 2;
                let Ok(target_idx) = usize::try_from(self.entries[mid].meta.sort_index) else {
                    break;
                };
                if target_idx >= self.entries.len() {
                    break;
                }
                let cmp = cmp_name_case_insensitive(
                    name.as_bytes(),
                    entry_name_bytes(&self.entries[target_idx].name_raw),
                );
                match cmp {
                    Ordering::Less => high = mid,
                    Ordering::Greater => low = mid + 1,
                    Ordering::Equal => {
                        return Some(EntryId(
                            u32::try_from(target_idx).expect("entry count validated at parse"),
                        ))
                    }
                }
            }
        }

        self.entries.iter().enumerate().find_map(|(idx, entry)| {
            if cmp_name_case_insensitive(name.as_bytes(), entry_name_bytes(&entry.name_raw))
                == Ordering::Equal
            {
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

    pub fn read(&self, id: EntryId) -> Result<ResourceData<'_>> {
        let range = self.entry_range(id)?;
        Ok(ResourceData::Borrowed(&self.bytes[range]))
    }

    pub fn read_into(&self, id: EntryId, out: &mut dyn OutputBuffer) -> Result<usize> {
        let range = self.entry_range(id)?;
        out.write_exact(&self.bytes[range.clone()])?;
        Ok(range.len())
    }

    pub fn raw_slice(&self, id: EntryId) -> Result<Option<&[u8]>> {
        let range = self.entry_range(id)?;
        Ok(Some(&self.bytes[range]))
    }

    pub fn edit_path(path: impl AsRef<Path>) -> Result<Editor> {
        let path_buf = path.as_ref().to_path_buf();
        let bytes = fs::read(&path_buf)?;
        let arc: Arc<[u8]> = Arc::from(bytes.into_boxed_slice());
        let (entries, _) = parse_archive(&arc, false)?;
        let mut editable = Vec::with_capacity(entries.len());
        for entry in &entries {
            let range = checked_range(entry.meta.data_offset, entry.meta.data_size, arc.len())?;
            editable.push(EditableEntry {
                meta: entry.meta.clone(),
                name_raw: entry.name_raw,
                data: EntryData::Borrowed(range), // Copy-on-write: only store range
            });
        }
        Ok(Editor {
            path: path_buf,
            source: arc,
            entries: editable,
        })
    }

    fn entry_range(&self, id: EntryId) -> Result<Range<usize>> {
        let idx = usize::try_from(id.0).map_err(|_| Error::IntegerOverflow)?;
        let Some(entry) = self.entries.get(idx) else {
            return Err(Error::EntryIdOutOfRange {
                id: id.0,
                entry_count: self.entries.len().try_into().unwrap_or(u32::MAX),
            });
        };
        checked_range(
            entry.meta.data_offset,
            entry.meta.data_size,
            self.bytes.len(),
        )
    }
}

pub struct Editor {
    path: PathBuf,
    source: Arc<[u8]>,
    entries: Vec<EditableEntry>,
}

#[derive(Clone, Debug)]
enum EntryData {
    Borrowed(Range<usize>),
    Modified(Vec<u8>),
}

#[derive(Clone, Debug)]
struct EditableEntry {
    meta: EntryMeta,
    name_raw: [u8; 36],
    data: EntryData,
}

impl EditableEntry {
    fn data_slice<'a>(&'a self, source: &'a Arc<[u8]>) -> &'a [u8] {
        match &self.data {
            EntryData::Borrowed(range) => &source[range.clone()],
            EntryData::Modified(vec) => vec.as_slice(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct NewEntry<'a> {
    pub kind: u32,
    pub attr1: u32,
    pub attr2: u32,
    pub attr3: u32,
    pub name: &'a str,
    pub data: &'a [u8],
}

impl Editor {
    pub fn entries(&self) -> impl Iterator<Item = EntryRef<'_>> {
        self.entries
            .iter()
            .enumerate()
            .map(|(idx, entry)| EntryRef {
                id: EntryId(u32::try_from(idx).expect("entry count validated at add")),
                meta: &entry.meta,
            })
    }

    pub fn add(&mut self, entry: NewEntry<'_>) -> Result<EntryId> {
        let name_raw = encode_name_field(entry.name)?;
        let id_u32 = u32::try_from(self.entries.len()).map_err(|_| Error::IntegerOverflow)?;
        let data_size = u32::try_from(entry.data.len()).map_err(|_| Error::IntegerOverflow)?;
        self.entries.push(EditableEntry {
            meta: EntryMeta {
                kind: entry.kind,
                attr1: entry.attr1,
                attr2: entry.attr2,
                attr3: entry.attr3,
                name: decode_name(entry_name_bytes(&name_raw)),
                data_offset: 0,
                data_size,
                sort_index: 0,
            },
            name_raw,
            data: EntryData::Modified(entry.data.to_vec()),
        });
        Ok(EntryId(id_u32))
    }

    pub fn replace_data(&mut self, id: EntryId, data: &[u8]) -> Result<()> {
        let idx = usize::try_from(id.0).map_err(|_| Error::IntegerOverflow)?;
        let Some(entry) = self.entries.get_mut(idx) else {
            return Err(Error::EntryIdOutOfRange {
                id: id.0,
                entry_count: self.entries.len().try_into().unwrap_or(u32::MAX),
            });
        };
        entry.meta.data_size = u32::try_from(data.len()).map_err(|_| Error::IntegerOverflow)?;
        // Replace with new data (triggers copy-on-write if borrowed)
        entry.data = EntryData::Modified(data.to_vec());
        Ok(())
    }

    pub fn remove(&mut self, id: EntryId) -> Result<()> {
        let idx = usize::try_from(id.0).map_err(|_| Error::IntegerOverflow)?;
        if idx >= self.entries.len() {
            return Err(Error::EntryIdOutOfRange {
                id: id.0,
                entry_count: self.entries.len().try_into().unwrap_or(u32::MAX),
            });
        }
        self.entries.remove(idx);
        Ok(())
    }

    pub fn commit(mut self) -> Result<()> {
        let count_u32 = u32::try_from(self.entries.len()).map_err(|_| Error::IntegerOverflow)?;

        // Pre-calculate capacity to avoid reallocations
        let total_data_size: usize = self
            .entries
            .iter()
            .map(|e| e.data_slice(&self.source).len())
            .sum();
        let padding_estimate = self.entries.len() * 8; // Max 8 bytes padding per entry
        let directory_size = self.entries.len() * 64; // 64 bytes per entry
        let capacity = 16 + total_data_size + padding_estimate + directory_size;

        let mut out = Vec::with_capacity(capacity);
        out.resize(16, 0); // Header

        // Keep reference to source for copy-on-write
        let source = &self.source;

        for entry in &mut self.entries {
            entry.meta.data_offset =
                u64::try_from(out.len()).map_err(|_| Error::IntegerOverflow)?;

            // Calculate size and get slice separately to avoid borrow conflicts
            let data_len = entry.data_slice(source).len();
            entry.meta.data_size = u32::try_from(data_len).map_err(|_| Error::IntegerOverflow)?;

            // Now get the slice again for writing
            let data_slice = entry.data_slice(source);
            out.extend_from_slice(data_slice);

            let padding = (8 - (out.len() % 8)) % 8;
            if padding > 0 {
                out.resize(out.len() + padding, 0);
            }
        }

        let mut sort_order: Vec<usize> = (0..self.entries.len()).collect();
        sort_order.sort_by(|a, b| {
            cmp_name_case_insensitive(
                entry_name_bytes(&self.entries[*a].name_raw),
                entry_name_bytes(&self.entries[*b].name_raw),
            )
        });

        for (idx, entry) in self.entries.iter_mut().enumerate() {
            entry.meta.sort_index =
                u32::try_from(sort_order[idx]).map_err(|_| Error::IntegerOverflow)?;
        }

        for entry in &self.entries {
            let data_offset_u32 =
                u32::try_from(entry.meta.data_offset).map_err(|_| Error::IntegerOverflow)?;
            push_u32(&mut out, entry.meta.kind);
            push_u32(&mut out, entry.meta.attr1);
            push_u32(&mut out, entry.meta.attr2);
            push_u32(&mut out, entry.meta.data_size);
            push_u32(&mut out, entry.meta.attr3);
            out.extend_from_slice(&entry.name_raw);
            push_u32(&mut out, data_offset_u32);
            push_u32(&mut out, entry.meta.sort_index);
        }

        let total_size_u32 = u32::try_from(out.len()).map_err(|_| Error::IntegerOverflow)?;
        out[0..4].copy_from_slice(b"NRes");
        out[4..8].copy_from_slice(&0x100_u32.to_le_bytes());
        out[8..12].copy_from_slice(&count_u32.to_le_bytes());
        out[12..16].copy_from_slice(&total_size_u32.to_le_bytes());

        write_atomic(&self.path, &out)
    }
}

fn parse_archive(bytes: &[u8], raw_mode: bool) -> Result<(Vec<EntryRecord>, u64)> {
    if raw_mode {
        let data_size = u32::try_from(bytes.len()).map_err(|_| Error::IntegerOverflow)?;
        let entry = EntryRecord {
            meta: EntryMeta {
                kind: 0,
                attr1: 0,
                attr2: 0,
                attr3: 0,
                name: String::from("RAW"),
                data_offset: 0,
                data_size,
                sort_index: 0,
            },
            name_raw: {
                let mut name = [0u8; 36];
                let bytes_name = b"RAW";
                name[..bytes_name.len()].copy_from_slice(bytes_name);
                name
            },
        };
        return Ok((
            vec![entry],
            u64::try_from(bytes.len()).map_err(|_| Error::IntegerOverflow)?,
        ));
    }

    if bytes.len() < 16 {
        let mut got = [0u8; 4];
        let copy_len = bytes.len().min(4);
        got[..copy_len].copy_from_slice(&bytes[..copy_len]);
        return Err(Error::InvalidMagic { got });
    }

    let mut magic = [0u8; 4];
    magic.copy_from_slice(&bytes[0..4]);
    if &magic != b"NRes" {
        return Err(Error::InvalidMagic { got: magic });
    }

    let version = read_u32(bytes, 4)?;
    if version != 0x100 {
        return Err(Error::UnsupportedVersion { got: version });
    }

    let entry_count_i32 = i32::from_le_bytes(
        bytes[8..12]
            .try_into()
            .map_err(|_| Error::IntegerOverflow)?,
    );
    if entry_count_i32 < 0 {
        return Err(Error::InvalidEntryCount {
            got: entry_count_i32,
        });
    }
    let entry_count = usize::try_from(entry_count_i32).map_err(|_| Error::IntegerOverflow)?;

    // Validate entry_count fits in u32 (required for EntryId)
    if entry_count > u32::MAX as usize {
        return Err(Error::TooManyEntries { got: entry_count });
    }

    let total_size = read_u32(bytes, 12)?;
    let actual_size = u64::try_from(bytes.len()).map_err(|_| Error::IntegerOverflow)?;
    if u64::from(total_size) != actual_size {
        return Err(Error::TotalSizeMismatch {
            header: total_size,
            actual: actual_size,
        });
    }

    let directory_len = u64::try_from(entry_count)
        .map_err(|_| Error::IntegerOverflow)?
        .checked_mul(64)
        .ok_or(Error::IntegerOverflow)?;
    let directory_offset =
        u64::from(total_size)
            .checked_sub(directory_len)
            .ok_or(Error::DirectoryOutOfBounds {
                directory_offset: 0,
                directory_len,
                file_len: actual_size,
            })?;

    if directory_offset < 16 || directory_offset + directory_len > actual_size {
        return Err(Error::DirectoryOutOfBounds {
            directory_offset,
            directory_len,
            file_len: actual_size,
        });
    }

    let mut entries = Vec::with_capacity(entry_count);
    for index in 0..entry_count {
        let base = usize::try_from(directory_offset)
            .map_err(|_| Error::IntegerOverflow)?
            .checked_add(index.checked_mul(64).ok_or(Error::IntegerOverflow)?)
            .ok_or(Error::IntegerOverflow)?;

        let kind = read_u32(bytes, base)?;
        let attr1 = read_u32(bytes, base + 4)?;
        let attr2 = read_u32(bytes, base + 8)?;
        let data_size = read_u32(bytes, base + 12)?;
        let attr3 = read_u32(bytes, base + 16)?;

        let mut name_raw = [0u8; 36];
        let name_slice = bytes
            .get(base + 20..base + 56)
            .ok_or(Error::IntegerOverflow)?;
        name_raw.copy_from_slice(name_slice);

        let name_bytes = entry_name_bytes(&name_raw);
        if name_bytes.len() > 35 {
            return Err(Error::NameTooLong {
                got: name_bytes.len(),
                max: 35,
            });
        }

        let data_offset = u64::from(read_u32(bytes, base + 56)?);
        let sort_index = read_u32(bytes, base + 60)?;

        let end = data_offset
            .checked_add(u64::from(data_size))
            .ok_or(Error::IntegerOverflow)?;
        if data_offset < 16 || end > directory_offset {
            return Err(Error::EntryDataOutOfBounds {
                id: u32::try_from(index).map_err(|_| Error::IntegerOverflow)?,
                offset: data_offset,
                size: data_size,
                directory_offset,
            });
        }

        entries.push(EntryRecord {
            meta: EntryMeta {
                kind,
                attr1,
                attr2,
                attr3,
                name: decode_name(name_bytes),
                data_offset,
                data_size,
                sort_index,
            },
            name_raw,
        });
    }

    Ok((entries, directory_offset))
}

fn checked_range(offset: u64, size: u32, bytes_len: usize) -> Result<Range<usize>> {
    let start = usize::try_from(offset).map_err(|_| Error::IntegerOverflow)?;
    let len = usize::try_from(size).map_err(|_| Error::IntegerOverflow)?;
    let end = start.checked_add(len).ok_or(Error::IntegerOverflow)?;
    if end > bytes_len {
        return Err(Error::IntegerOverflow);
    }
    Ok(start..end)
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32> {
    let data = bytes
        .get(offset..offset + 4)
        .ok_or(Error::IntegerOverflow)?;
    let arr: [u8; 4] = data.try_into().map_err(|_| Error::IntegerOverflow)?;
    Ok(u32::from_le_bytes(arr))
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn encode_name_field(name: &str) -> Result<[u8; 36]> {
    let bytes = name.as_bytes();
    if bytes.contains(&0) {
        return Err(Error::NameContainsNul);
    }
    if bytes.len() > 35 {
        return Err(Error::NameTooLong {
            got: bytes.len(),
            max: 35,
        });
    }

    let mut out = [0u8; 36];
    out[..bytes.len()].copy_from_slice(bytes);
    Ok(out)
}

fn entry_name_bytes(raw: &[u8; 36]) -> &[u8] {
    let len = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    &raw[..len]
}

fn decode_name(name: &[u8]) -> String {
    name.iter().map(|b| char::from(*b)).collect()
}

fn cmp_name_case_insensitive(a: &[u8], b: &[u8]) -> Ordering {
    let mut idx = 0usize;
    let min_len = a.len().min(b.len());
    while idx < min_len {
        let left = ascii_lower(a[idx]);
        let right = ascii_lower(b[idx]);
        if left != right {
            return left.cmp(&right);
        }
        idx += 1;
    }
    a.len().cmp(&b.len())
}

fn ascii_lower(value: u8) -> u8 {
    if value.is_ascii_uppercase() {
        value + 32
    } else {
        value
    }
}

fn prefetch_pages(bytes: &[u8]) {
    use std::sync::atomic::{compiler_fence, Ordering};

    let mut cursor = 0usize;
    let mut sink = 0u8;
    while cursor < bytes.len() {
        sink ^= bytes[cursor];
        cursor = cursor.saturating_add(4096);
    }
    compiler_fence(Ordering::SeqCst);
    let _ = sink;
}

fn write_atomic(path: &Path, content: &[u8]) -> Result<()> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("archive");
    let parent = path.parent().unwrap_or_else(|| Path::new("."));

    let mut temp_path = None;
    for attempt in 0..128u32 {
        let name = format!(
            ".{}.tmp.{}.{}.{}",
            file_name,
            std::process::id(),
            unix_time_nanos(),
            attempt
        );
        let candidate = parent.join(name);
        let opened = FsOpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&candidate);
        if let Ok(mut file) = opened {
            file.write_all(content)?;
            file.sync_all()?;
            temp_path = Some((candidate, file));
            break;
        }
    }

    let Some((tmp_path, mut file)) = temp_path else {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "failed to create temporary file for atomic write",
        )));
    };

    file.flush()?;
    drop(file);

    match fs::rename(&tmp_path, path) {
        Ok(()) => Ok(()),
        Err(rename_err) => {
            if path.exists() {
                fs::remove_file(path)?;
                fs::rename(&tmp_path, path)?;
                Ok(())
            } else {
                let _ = fs::remove_file(&tmp_path);
                Err(Error::Io(rename_err))
            }
        }
    }
}

fn unix_time_nanos() -> u128 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_nanos(),
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests;
