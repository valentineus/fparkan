pub mod error;

use crate::error::Error;

pub type Result<T> = core::result::Result<T, Error>;

pub const TEXM_MAGIC: u32 = 0x6D78_6554;
pub const PAGE_MAGIC: u32 = 0x6567_6150;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    Indexed8,
    Rgb565,
    Rgb556,
    Argb4444,
    LuminanceAlpha88,
    Rgb888,
    Argb8888,
}

impl PixelFormat {
    pub fn from_raw(raw: u32) -> Option<Self> {
        match raw {
            0 => Some(Self::Indexed8),
            565 => Some(Self::Rgb565),
            556 => Some(Self::Rgb556),
            4444 => Some(Self::Argb4444),
            88 => Some(Self::LuminanceAlpha88),
            888 => Some(Self::Rgb888),
            8888 => Some(Self::Argb8888),
            _ => None,
        }
    }

    pub fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Indexed8 => 1,
            Self::Rgb565 | Self::Rgb556 | Self::Argb4444 | Self::LuminanceAlpha88 => 2,
            // Parkan stores format 888 as 32-bit RGBX in texture payloads.
            Self::Rgb888 | Self::Argb8888 => 4,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Header {
    pub width: u32,
    pub height: u32,
    pub mip_count: u32,
    pub flags4: u32,
    pub flags5: u32,
    pub unk6: u32,
    pub format_raw: u32,
    pub format: PixelFormat,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct MipLevel {
    pub width: u32,
    pub height: u32,
    pub offset: usize,
    pub size: usize,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PageRect {
    pub x: i16,
    pub w: i16,
    pub y: i16,
    pub h: i16,
}

#[derive(Clone, Debug)]
pub struct Texture {
    pub header: Header,
    pub palette: Option<[u8; 1024]>,
    pub mip_levels: Vec<MipLevel>,
    pub page_rects: Vec<PageRect>,
}

impl Texture {
    pub fn core_size(&self) -> usize {
        let mut size = 32usize;
        if self.palette.is_some() {
            size += 1024;
        }
        for level in &self.mip_levels {
            size += level.size;
        }
        size
    }
}

#[derive(Clone, Debug)]
pub struct DecodedMip {
    pub width: u32,
    pub height: u32,
    pub rgba8: Vec<u8>,
}

pub fn parse_texm(payload: &[u8]) -> Result<Texture> {
    if payload.len() < 32 {
        return Err(Error::HeaderTooSmall {
            size: payload.len(),
        });
    }

    let magic = read_u32(payload, 0)?;
    if magic != TEXM_MAGIC {
        return Err(Error::InvalidMagic { got: magic });
    }

    let width = read_u32(payload, 4)?;
    let height = read_u32(payload, 8)?;
    let mip_count = read_u32(payload, 12)?;
    let flags4 = read_u32(payload, 16)?;
    let flags5 = read_u32(payload, 20)?;
    let unk6 = read_u32(payload, 24)?;
    let format_raw = read_u32(payload, 28)?;

    if width == 0 || height == 0 {
        return Err(Error::InvalidDimensions { width, height });
    }
    if mip_count == 0 {
        return Err(Error::InvalidMipCount { mip_count });
    }

    let format =
        PixelFormat::from_raw(format_raw).ok_or(Error::UnknownFormat { format: format_raw })?;
    let bytes_per_pixel = format.bytes_per_pixel();

    let mut offset = 32usize;
    let palette = if format == PixelFormat::Indexed8 {
        let end = offset.checked_add(1024).ok_or(Error::IntegerOverflow)?;
        if end > payload.len() {
            return Err(Error::CoreDataOutOfBounds {
                expected_end: end,
                actual_size: payload.len(),
            });
        }
        let mut pal = [0u8; 1024];
        pal.copy_from_slice(&payload[offset..end]);
        offset = end;
        Some(pal)
    } else {
        None
    };

    let mut mip_levels =
        Vec::with_capacity(usize::try_from(mip_count).map_err(|_| Error::IntegerOverflow)?);
    let mut w = width;
    let mut h = height;
    for _ in 0..mip_count {
        let pixel_count_u64 = u64::from(w)
            .checked_mul(u64::from(h))
            .ok_or(Error::IntegerOverflow)?;
        let level_size_u64 = pixel_count_u64
            .checked_mul(u64::try_from(bytes_per_pixel).map_err(|_| Error::IntegerOverflow)?)
            .ok_or(Error::IntegerOverflow)?;
        let level_size = usize::try_from(level_size_u64).map_err(|_| Error::IntegerOverflow)?;
        let level_offset = offset;
        offset = offset
            .checked_add(level_size)
            .ok_or(Error::IntegerOverflow)?;
        if offset > payload.len() {
            return Err(Error::CoreDataOutOfBounds {
                expected_end: offset,
                actual_size: payload.len(),
            });
        }
        mip_levels.push(MipLevel {
            width: w,
            height: h,
            offset: level_offset,
            size: level_size,
        });
        w = (w >> 1).max(1);
        h = (h >> 1).max(1);
    }

    let page_rects = parse_page_tail(payload, offset)?;

    Ok(Texture {
        header: Header {
            width,
            height,
            mip_count,
            flags4,
            flags5,
            unk6,
            format_raw,
            format,
        },
        palette,
        mip_levels,
        page_rects,
    })
}

pub fn decode_mip_rgba8(texture: &Texture, payload: &[u8], mip_index: usize) -> Result<DecodedMip> {
    let Some(level) = texture.mip_levels.get(mip_index).copied() else {
        return Err(Error::MipIndexOutOfRange {
            requested: mip_index,
            mip_count: texture.mip_levels.len(),
        });
    };

    let end = level
        .offset
        .checked_add(level.size)
        .ok_or(Error::IntegerOverflow)?;
    let Some(level_data) = payload.get(level.offset..end) else {
        return Err(Error::MipDataOutOfBounds {
            offset: level.offset,
            size: level.size,
            payload_size: payload.len(),
        });
    };

    let pixel_count = usize::try_from(level.width)
        .ok()
        .and_then(|w| {
            usize::try_from(level.height)
                .ok()
                .map(|h| w.saturating_mul(h))
        })
        .ok_or(Error::IntegerOverflow)?;
    let mut rgba = vec![0u8; pixel_count.saturating_mul(4)];

    match texture.header.format {
        PixelFormat::Indexed8 => {
            let palette = texture.palette.as_ref().ok_or(Error::IntegerOverflow)?;
            for (i, &index) in level_data.iter().enumerate() {
                if i >= pixel_count {
                    break;
                }
                let poff = usize::from(index).saturating_mul(4);
                // Keep this form to accept the last palette item (index 255).
                if poff + 4 > palette.len() {
                    continue;
                }
                let out = i.saturating_mul(4);
                rgba[out] = palette[poff];
                rgba[out + 1] = palette[poff + 1];
                rgba[out + 2] = palette[poff + 2];
                rgba[out + 3] = palette[poff + 3];
            }
        }
        PixelFormat::Rgb565 => {
            decode_words(level_data, pixel_count, &mut rgba, decode_rgb565);
        }
        PixelFormat::Rgb556 => {
            decode_words(level_data, pixel_count, &mut rgba, decode_rgb556);
        }
        PixelFormat::Argb4444 => {
            decode_words(level_data, pixel_count, &mut rgba, decode_argb4444);
        }
        PixelFormat::LuminanceAlpha88 => {
            decode_words(level_data, pixel_count, &mut rgba, decode_luminance_alpha88);
        }
        PixelFormat::Rgb888 => {
            decode_dwords(level_data, pixel_count, &mut rgba, decode_rgb888x);
        }
        PixelFormat::Argb8888 => {
            decode_dwords(level_data, pixel_count, &mut rgba, decode_argb8888);
        }
    }

    Ok(DecodedMip {
        width: level.width,
        height: level.height,
        rgba8: rgba,
    })
}

fn parse_page_tail(payload: &[u8], core_end: usize) -> Result<Vec<PageRect>> {
    if core_end == payload.len() {
        return Ok(Vec::new());
    }
    if payload.len().saturating_sub(core_end) < 8 {
        return Err(Error::InvalidPageSize {
            expected: 8,
            actual: payload.len().saturating_sub(core_end),
        });
    }
    let magic = read_u32(payload, core_end)?;
    if magic != PAGE_MAGIC {
        return Err(Error::InvalidPageMagic);
    }
    let rect_count = read_u32(payload, core_end + 4)?;
    let rect_count_usize = usize::try_from(rect_count).map_err(|_| Error::IntegerOverflow)?;
    let expected_size = 8usize
        .checked_add(
            rect_count_usize
                .checked_mul(8)
                .ok_or(Error::IntegerOverflow)?,
        )
        .ok_or(Error::IntegerOverflow)?;
    let actual = payload.len().saturating_sub(core_end);
    if expected_size != actual {
        return Err(Error::InvalidPageSize {
            expected: expected_size,
            actual,
        });
    }

    let mut rects = Vec::with_capacity(rect_count_usize);
    for i in 0..rect_count_usize {
        let off = core_end
            .checked_add(8)
            .and_then(|v| v.checked_add(i * 8))
            .ok_or(Error::IntegerOverflow)?;
        rects.push(PageRect {
            x: read_i16(payload, off)?,
            w: read_i16(payload, off + 2)?,
            y: read_i16(payload, off + 4)?,
            h: read_i16(payload, off + 6)?,
        });
    }
    Ok(rects)
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32> {
    let bytes = data.get(offset..offset + 4).ok_or(Error::IntegerOverflow)?;
    let arr: [u8; 4] = bytes.try_into().map_err(|_| Error::IntegerOverflow)?;
    Ok(u32::from_le_bytes(arr))
}

fn read_i16(data: &[u8], offset: usize) -> Result<i16> {
    let bytes = data.get(offset..offset + 2).ok_or(Error::IntegerOverflow)?;
    let arr: [u8; 2] = bytes.try_into().map_err(|_| Error::IntegerOverflow)?;
    Ok(i16::from_le_bytes(arr))
}

fn decode_words(data: &[u8], pixel_count: usize, rgba: &mut [u8], decode: fn(u16) -> [u8; 4]) {
    for i in 0..pixel_count {
        let off = i.saturating_mul(2);
        let Some(bytes) = data.get(off..off + 2) else {
            break;
        };
        let word = u16::from_le_bytes([bytes[0], bytes[1]]);
        let px = decode(word);
        let out = i.saturating_mul(4);
        rgba[out..out + 4].copy_from_slice(&px);
    }
}

fn decode_dwords(data: &[u8], pixel_count: usize, rgba: &mut [u8], decode: fn(u32) -> [u8; 4]) {
    for i in 0..pixel_count {
        let off = i.saturating_mul(4);
        let Some(bytes) = data.get(off..off + 4) else {
            break;
        };
        let dword = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let px = decode(dword);
        let out = i.saturating_mul(4);
        rgba[out..out + 4].copy_from_slice(&px);
    }
}

fn expand5(v: u16) -> u8 {
    ((u32::from(v) * 255 + 15) / 31) as u8
}

fn expand6(v: u16) -> u8 {
    ((u32::from(v) * 255 + 31) / 63) as u8
}

fn expand4(v: u16) -> u8 {
    (u32::from(v) * 17) as u8
}

fn decode_rgb565(word: u16) -> [u8; 4] {
    let r = expand5((word >> 11) & 0x1F);
    let g = expand6((word >> 5) & 0x3F);
    let b = expand5(word & 0x1F);
    [r, g, b, 255]
}

fn decode_rgb556(word: u16) -> [u8; 4] {
    let r = expand5((word >> 11) & 0x1F);
    let g = expand5((word >> 6) & 0x1F);
    let b = expand6(word & 0x3F);
    [r, g, b, 255]
}

fn decode_argb4444(word: u16) -> [u8; 4] {
    let a = expand4((word >> 12) & 0x0F);
    let r = expand4((word >> 8) & 0x0F);
    let g = expand4((word >> 4) & 0x0F);
    let b = expand4(word & 0x0F);
    [r, g, b, a]
}

fn decode_luminance_alpha88(word: u16) -> [u8; 4] {
    let l = ((word >> 8) & 0xFF) as u8;
    let a = (word & 0xFF) as u8;
    [l, l, l, a]
}

fn decode_rgb888x(dword: u32) -> [u8; 4] {
    let r = (dword & 0xFF) as u8;
    let g = ((dword >> 8) & 0xFF) as u8;
    let b = ((dword >> 16) & 0xFF) as u8;
    [r, g, b, 255]
}

fn decode_argb8888(dword: u32) -> [u8; 4] {
    let a = (dword & 0xFF) as u8;
    let r = ((dword >> 8) & 0xFF) as u8;
    let g = ((dword >> 16) & 0xFF) as u8;
    let b = ((dword >> 24) & 0xFF) as u8;
    [r, g, b, a]
}

#[cfg(test)]
mod tests;
