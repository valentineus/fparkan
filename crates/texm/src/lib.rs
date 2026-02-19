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
        w = w.max(1) >> 1;
        h = h.max(1) >> 1;
        if w == 0 {
            w = 1;
        }
        if h == 0 {
            h = 1;
        }
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

#[cfg(test)]
mod tests;
