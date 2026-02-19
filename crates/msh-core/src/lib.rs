pub mod error;

use crate::error::Error;
use std::sync::Arc;

pub type Result<T> = core::result::Result<T, Error>;

pub const RES1_NODE_TABLE: u32 = 1;
pub const RES2_SLOTS: u32 = 2;
pub const RES3_POSITIONS: u32 = 3;
pub const RES4_NORMALS: u32 = 4;
pub const RES5_UV0: u32 = 5;
pub const RES6_INDICES: u32 = 6;
pub const RES10_NAMES: u32 = 10;
pub const RES13_BATCHES: u32 = 13;

#[derive(Clone, Debug)]
pub struct Slot {
    pub tri_start: u16,
    pub tri_count: u16,
    pub batch_start: u16,
    pub batch_count: u16,
    pub aabb_min: [f32; 3],
    pub aabb_max: [f32; 3],
    pub sphere_center: [f32; 3],
    pub sphere_radius: f32,
    pub opaque: [u32; 5],
}

#[derive(Clone, Debug)]
pub struct Batch {
    pub batch_flags: u16,
    pub material_index: u16,
    pub opaque4: u16,
    pub opaque6: u16,
    pub index_count: u16,
    pub index_start: u32,
    pub opaque14: u16,
    pub base_vertex: u32,
}

#[derive(Clone, Debug)]
pub struct Model {
    pub node_stride: usize,
    pub node_count: usize,
    pub nodes_raw: Vec<u8>,
    pub slots: Vec<Slot>,
    pub positions: Vec<[f32; 3]>,
    pub normals: Option<Vec<[i8; 4]>>,
    pub uv0: Option<Vec<[i16; 2]>>,
    pub indices: Vec<u16>,
    pub batches: Vec<Batch>,
    pub node_names: Option<Vec<Option<String>>>,
}

impl Model {
    pub fn slot_index(&self, node_index: usize, lod: usize, group: usize) -> Option<usize> {
        if node_index >= self.node_count || lod >= 3 || group >= 5 {
            return None;
        }
        if self.node_stride != 38 {
            return None;
        }
        let node_off = node_index.checked_mul(self.node_stride)?;
        let matrix_off = node_off.checked_add(8)?;
        let word_off = matrix_off.checked_add((lod * 5 + group) * 2)?;
        let raw = read_u16(&self.nodes_raw, word_off).ok()?;
        if raw == u16::MAX {
            return None;
        }
        let idx = usize::from(raw);
        if idx >= self.slots.len() {
            return None;
        }
        Some(idx)
    }
}

pub fn parse_model_payload(payload: &[u8]) -> Result<Model> {
    let archive = nres::Archive::open_bytes(
        Arc::from(payload.to_vec().into_boxed_slice()),
        nres::OpenOptions::default(),
    )?;

    let res1 = read_required(&archive, RES1_NODE_TABLE, "Res1")?;
    let res2 = read_required(&archive, RES2_SLOTS, "Res2")?;
    let res3 = read_required(&archive, RES3_POSITIONS, "Res3")?;
    let res6 = read_required(&archive, RES6_INDICES, "Res6")?;
    let res13 = read_required(&archive, RES13_BATCHES, "Res13")?;

    let res4 = read_optional(&archive, RES4_NORMALS)?;
    let res5 = read_optional(&archive, RES5_UV0)?;
    let res10 = read_optional(&archive, RES10_NAMES)?;

    let node_stride = usize::try_from(res1.meta.attr3).map_err(|_| Error::IntegerOverflow)?;
    if node_stride != 38 && node_stride != 24 {
        return Err(Error::UnsupportedNodeStride {
            stride: node_stride,
        });
    }
    if res1.bytes.len() % node_stride != 0 {
        return Err(Error::InvalidResourceSize {
            label: "Res1",
            size: res1.bytes.len(),
            stride: node_stride,
        });
    }
    let node_count = res1.bytes.len() / node_stride;

    if res2.bytes.len() < 0x8C {
        return Err(Error::InvalidRes2Size {
            size: res2.bytes.len(),
        });
    }
    let slot_blob = res2
        .bytes
        .len()
        .checked_sub(0x8C)
        .ok_or(Error::IntegerOverflow)?;
    if slot_blob % 68 != 0 {
        return Err(Error::InvalidResourceSize {
            label: "Res2.slots",
            size: slot_blob,
            stride: 68,
        });
    }
    let slot_count = slot_blob / 68;
    let mut slots = Vec::with_capacity(slot_count);
    for i in 0..slot_count {
        let off = 0x8Cusize
            .checked_add(i.checked_mul(68).ok_or(Error::IntegerOverflow)?)
            .ok_or(Error::IntegerOverflow)?;
        slots.push(Slot {
            tri_start: read_u16(&res2.bytes, off)?,
            tri_count: read_u16(&res2.bytes, off + 2)?,
            batch_start: read_u16(&res2.bytes, off + 4)?,
            batch_count: read_u16(&res2.bytes, off + 6)?,
            aabb_min: [
                read_f32(&res2.bytes, off + 8)?,
                read_f32(&res2.bytes, off + 12)?,
                read_f32(&res2.bytes, off + 16)?,
            ],
            aabb_max: [
                read_f32(&res2.bytes, off + 20)?,
                read_f32(&res2.bytes, off + 24)?,
                read_f32(&res2.bytes, off + 28)?,
            ],
            sphere_center: [
                read_f32(&res2.bytes, off + 32)?,
                read_f32(&res2.bytes, off + 36)?,
                read_f32(&res2.bytes, off + 40)?,
            ],
            sphere_radius: read_f32(&res2.bytes, off + 44)?,
            opaque: [
                read_u32(&res2.bytes, off + 48)?,
                read_u32(&res2.bytes, off + 52)?,
                read_u32(&res2.bytes, off + 56)?,
                read_u32(&res2.bytes, off + 60)?,
                read_u32(&res2.bytes, off + 64)?,
            ],
        });
    }

    let positions = parse_positions(&res3.bytes)?;
    let indices = parse_u16_array(&res6.bytes, "Res6")?;
    let batches = parse_batches(&res13.bytes)?;
    validate_slot_batch_ranges(&slots, batches.len())?;
    validate_batch_index_ranges(&batches, indices.len())?;

    let normals = match res4 {
        Some(raw) => Some(parse_i8x4_array(&raw.bytes, "Res4")?),
        None => None,
    };
    let uv0 = match res5 {
        Some(raw) => Some(parse_i16x2_array(&raw.bytes, "Res5")?),
        None => None,
    };
    let node_names = match res10 {
        Some(raw) => Some(parse_res10_names(&raw.bytes, node_count)?),
        None => None,
    };

    Ok(Model {
        node_stride,
        node_count,
        nodes_raw: res1.bytes,
        slots,
        positions,
        normals,
        uv0,
        indices,
        batches,
        node_names,
    })
}

fn validate_slot_batch_ranges(slots: &[Slot], batch_count: usize) -> Result<()> {
    for slot in slots {
        let start = usize::from(slot.batch_start);
        let end = start
            .checked_add(usize::from(slot.batch_count))
            .ok_or(Error::IntegerOverflow)?;
        if end > batch_count {
            return Err(Error::IndexOutOfBounds {
                label: "Res2.batch_range",
                index: end,
                limit: batch_count,
            });
        }
    }
    Ok(())
}

fn validate_batch_index_ranges(batches: &[Batch], index_count: usize) -> Result<()> {
    for batch in batches {
        let start = usize::try_from(batch.index_start).map_err(|_| Error::IntegerOverflow)?;
        let end = start
            .checked_add(usize::from(batch.index_count))
            .ok_or(Error::IntegerOverflow)?;
        if end > index_count {
            return Err(Error::IndexOutOfBounds {
                label: "Res13.index_range",
                index: end,
                limit: index_count,
            });
        }
    }
    Ok(())
}

fn parse_positions(data: &[u8]) -> Result<Vec<[f32; 3]>> {
    if !data.len().is_multiple_of(12) {
        return Err(Error::InvalidResourceSize {
            label: "Res3",
            size: data.len(),
            stride: 12,
        });
    }
    let count = data.len() / 12;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let off = i * 12;
        out.push([
            read_f32(data, off)?,
            read_f32(data, off + 4)?,
            read_f32(data, off + 8)?,
        ]);
    }
    Ok(out)
}

fn parse_batches(data: &[u8]) -> Result<Vec<Batch>> {
    if !data.len().is_multiple_of(20) {
        return Err(Error::InvalidResourceSize {
            label: "Res13",
            size: data.len(),
            stride: 20,
        });
    }
    let count = data.len() / 20;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let off = i * 20;
        out.push(Batch {
            batch_flags: read_u16(data, off)?,
            material_index: read_u16(data, off + 2)?,
            opaque4: read_u16(data, off + 4)?,
            opaque6: read_u16(data, off + 6)?,
            index_count: read_u16(data, off + 8)?,
            index_start: read_u32(data, off + 10)?,
            opaque14: read_u16(data, off + 14)?,
            base_vertex: read_u32(data, off + 16)?,
        });
    }
    Ok(out)
}

fn parse_u16_array(data: &[u8], label: &'static str) -> Result<Vec<u16>> {
    if !data.len().is_multiple_of(2) {
        return Err(Error::InvalidResourceSize {
            label,
            size: data.len(),
            stride: 2,
        });
    }
    let mut out = Vec::with_capacity(data.len() / 2);
    for i in (0..data.len()).step_by(2) {
        out.push(read_u16(data, i)?);
    }
    Ok(out)
}

fn parse_i8x4_array(data: &[u8], label: &'static str) -> Result<Vec<[i8; 4]>> {
    if !data.len().is_multiple_of(4) {
        return Err(Error::InvalidResourceSize {
            label,
            size: data.len(),
            stride: 4,
        });
    }
    let mut out = Vec::with_capacity(data.len() / 4);
    for i in (0..data.len()).step_by(4) {
        out.push([
            read_i8(data, i)?,
            read_i8(data, i + 1)?,
            read_i8(data, i + 2)?,
            read_i8(data, i + 3)?,
        ]);
    }
    Ok(out)
}

fn parse_i16x2_array(data: &[u8], label: &'static str) -> Result<Vec<[i16; 2]>> {
    if !data.len().is_multiple_of(4) {
        return Err(Error::InvalidResourceSize {
            label,
            size: data.len(),
            stride: 4,
        });
    }
    let mut out = Vec::with_capacity(data.len() / 4);
    for i in (0..data.len()).step_by(4) {
        out.push([read_i16(data, i)?, read_i16(data, i + 2)?]);
    }
    Ok(out)
}

fn parse_res10_names(data: &[u8], node_count: usize) -> Result<Vec<Option<String>>> {
    let mut out = Vec::with_capacity(node_count);
    let mut off = 0usize;
    for _ in 0..node_count {
        let len = usize::try_from(read_u32(data, off)?).map_err(|_| Error::IntegerOverflow)?;
        off = off.checked_add(4).ok_or(Error::IntegerOverflow)?;
        if len == 0 {
            out.push(None);
            continue;
        }
        let need = len.checked_add(1).ok_or(Error::IntegerOverflow)?;
        let end = off.checked_add(need).ok_or(Error::IntegerOverflow)?;
        let slice = data.get(off..end).ok_or(Error::InvalidResourceSize {
            label: "Res10",
            size: data.len(),
            stride: 1,
        })?;
        let text = if slice.last().copied() == Some(0) {
            &slice[..slice.len().saturating_sub(1)]
        } else {
            slice
        };
        let decoded = String::from_utf8_lossy(text).to_string();
        out.push(Some(decoded));
        off = end;
    }
    Ok(out)
}

struct RawResource {
    meta: nres::EntryMeta,
    bytes: Vec<u8>,
}

fn read_required(archive: &nres::Archive, kind: u32, label: &'static str) -> Result<RawResource> {
    let id = archive
        .entries()
        .find(|entry| entry.meta.kind == kind)
        .map(|entry| entry.id)
        .ok_or(Error::MissingResource { kind, label })?;
    let entry = archive.get(id).ok_or(Error::IndexOutOfBounds {
        label,
        index: usize::try_from(id.0).map_err(|_| Error::IntegerOverflow)?,
        limit: archive.entry_count(),
    })?;
    let data = archive.read(id)?.into_owned();
    Ok(RawResource {
        meta: entry.meta.clone(),
        bytes: data,
    })
}

fn read_optional(archive: &nres::Archive, kind: u32) -> Result<Option<RawResource>> {
    let Some(id) = archive
        .entries()
        .find(|entry| entry.meta.kind == kind)
        .map(|entry| entry.id)
    else {
        return Ok(None);
    };
    let entry = archive.get(id).ok_or(Error::IndexOutOfBounds {
        label: "optional",
        index: usize::try_from(id.0).map_err(|_| Error::IntegerOverflow)?,
        limit: archive.entry_count(),
    })?;
    let data = archive.read(id)?.into_owned();
    Ok(Some(RawResource {
        meta: entry.meta.clone(),
        bytes: data,
    }))
}

fn read_u16(data: &[u8], offset: usize) -> Result<u16> {
    let bytes = data.get(offset..offset + 2).ok_or(Error::IntegerOverflow)?;
    let arr: [u8; 2] = bytes.try_into().map_err(|_| Error::IntegerOverflow)?;
    Ok(u16::from_le_bytes(arr))
}

fn read_i16(data: &[u8], offset: usize) -> Result<i16> {
    let bytes = data.get(offset..offset + 2).ok_or(Error::IntegerOverflow)?;
    let arr: [u8; 2] = bytes.try_into().map_err(|_| Error::IntegerOverflow)?;
    Ok(i16::from_le_bytes(arr))
}

fn read_i8(data: &[u8], offset: usize) -> Result<i8> {
    let byte = data.get(offset).copied().ok_or(Error::IntegerOverflow)?;
    Ok(i8::from_le_bytes([byte]))
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32> {
    let bytes = data.get(offset..offset + 4).ok_or(Error::IntegerOverflow)?;
    let arr: [u8; 4] = bytes.try_into().map_err(|_| Error::IntegerOverflow)?;
    Ok(u32::from_le_bytes(arr))
}

fn read_f32(data: &[u8], offset: usize) -> Result<f32> {
    Ok(f32::from_bits(read_u32(data, offset)?))
}

#[cfg(test)]
mod tests;
