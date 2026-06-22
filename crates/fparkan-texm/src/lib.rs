#![forbid(unsafe_code)]
//! Stage-3 Texm texture contract.

use std::sync::Arc;

const TEXM_MAGIC: u32 = 0x6D78_6554;
const PAGE_MAGIC: u32 = 0x6567_6150;

/// Pixel format.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PixelFormat {
    /// Indexed 8.
    Indexed8,
    /// RGB565.
    Rgb565,
    /// RGB556.
    Rgb556,
    /// ARGB4444.
    Argb4444,
    /// Luminance alpha 8:8.
    L8A8,
    /// RGB888 with preserved service byte in disk payload.
    Rgb888x,
    /// ARGB8888.
    Argb8888,
}

/// Texm disk document.
#[derive(Clone, Debug)]
pub struct TexmDocument {
    bytes: Arc<[u8]>,
    texture: Texture,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DiskPixelFormat {
    Indexed8,
    Rgb565,
    Rgb556,
    Argb4444,
    L8A8,
    Rgb888x,
    Argb8888,
}

impl DiskPixelFormat {
    fn from_raw(raw: u32) -> Option<Self> {
        match raw {
            0 => Some(Self::Indexed8),
            565 => Some(Self::Rgb565),
            556 => Some(Self::Rgb556),
            4444 => Some(Self::Argb4444),
            88 => Some(Self::L8A8),
            888 => Some(Self::Rgb888x),
            8888 => Some(Self::Argb8888),
            _ => None,
        }
    }

    fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Indexed8 => 1,
            Self::Rgb565 | Self::Rgb556 | Self::Argb4444 | Self::L8A8 => 2,
            Self::Rgb888x | Self::Argb8888 => 4,
        }
    }
}

#[derive(Clone, Debug)]
struct Header {
    width: u32,
    height: u32,
    format: DiskPixelFormat,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MipLevel {
    width: u32,
    height: u32,
    offset: usize,
    size: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DiskPageRect {
    x: i16,
    w: i16,
    y: i16,
    h: i16,
}

#[derive(Clone, Debug)]
struct Texture {
    header: Header,
    palette: Option<[u8; 1024]>,
    mip_levels: Vec<MipLevel>,
    page_rects: Vec<DiskPageRect>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DecodedMip {
    width: u32,
    height: u32,
    rgba8: Vec<u8>,
}

/// Borrowed mip level view.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MipLevelView<'a> {
    /// Mip level index.
    pub level: u32,
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
    /// Raw disk bytes for this level.
    pub bytes: &'a [u8],
}

/// Page rectangle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PageRect {
    /// X origin.
    pub x: i16,
    /// Width.
    pub w: i16,
    /// Y origin.
    pub y: i16,
    /// Height.
    pub h: i16,
}

/// Page rectangle scaling policy.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PageScalePolicy {
    /// Scale origin with floor and end with ceil, preserving coverage.
    #[default]
    FloorOriginCeilEnd,
}

/// RGBA8 image.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RgbaImage {
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
    /// Packed RGBA8 pixels.
    pub rgba8: Vec<u8>,
}

/// Texture upload plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextureUploadPlan {
    /// Pixel format.
    pub format: PixelFormat,
    /// Original texture width.
    pub width: u32,
    /// Original texture height.
    pub height: u32,
    /// Selected mip levels.
    pub mips: Vec<UploadMip>,
    /// Page rectangles copied from disk metadata.
    pub page_rects: Vec<PageRect>,
}

/// Upload mip description.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UploadMip {
    /// Original mip level index.
    pub level: u32,
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
    /// Byte offset in the original disk document.
    pub offset: usize,
    /// Byte size.
    pub size: usize,
}

/// Mip skip policy.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MipSkipPolicy {
    /// Number of top mip levels to skip.
    pub skip_top_levels: u32,
}

/// Texm decode error.
#[derive(Debug)]
pub enum TexmError {
    /// Legacy parser error.
    Format(String),
    /// Requested mip level is absent.
    MipLevelOutOfRange {
        /// Requested level.
        requested: u32,
        /// Available mip count.
        mip_count: usize,
    },
    /// Mip payload range is outside the document.
    MipDataOutOfBounds {
        /// Byte offset.
        offset: usize,
        /// Byte size.
        size: usize,
        /// Document size.
        document_size: usize,
    },
    /// All mip levels were skipped.
    EmptyUploadPlan,
}

impl std::fmt::Display for TexmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Format(message) => write!(f, "{message}"),
            Self::MipLevelOutOfRange {
                requested,
                mip_count,
            } => write!(
                f,
                "Texm mip level out of range: requested={requested}, mip_count={mip_count}"
            ),
            Self::MipDataOutOfBounds {
                offset,
                size,
                document_size,
            } => write!(
                f,
                "Texm mip bytes out of bounds: offset={offset}, size={size}, document_size={document_size}"
            ),
            Self::EmptyUploadPlan => write!(f, "Texm upload plan contains no mip levels"),
        }
    }
}

impl std::error::Error for TexmError {}

/// Decodes Texm disk bytes.
///
/// # Errors
///
/// Returns [`TexmError`] when the header, format, mip chain, palette, or Page
/// chunk is malformed.
pub fn decode_texm(bytes: Arc<[u8]>) -> Result<TexmDocument, TexmError> {
    let texture = parse_texm(&bytes)?;
    Ok(TexmDocument { bytes, texture })
}

/// Decodes one mip level into RGBA8 using the CPU reference decoder.
///
/// # Errors
///
/// Returns [`TexmError`] when `level` is outside the mip chain or mip bytes are
/// malformed.
pub fn decode_mip_rgba8(document: &TexmDocument, level: u32) -> Result<RgbaImage, TexmError> {
    let decoded = decode_mip_rgba8_internal(
        &document.texture,
        &document.bytes,
        usize::try_from(level).map_err(|_| TexmError::MipLevelOutOfRange {
            requested: level,
            mip_count: document.texture.mip_levels.len(),
        })?,
    )?;
    Ok(RgbaImage {
        width: decoded.width,
        height: decoded.height,
        rgba8: decoded.rgba8,
    })
}

/// Builds an upload plan without mutating the disk document.
///
/// # Errors
///
/// Returns [`TexmError::EmptyUploadPlan`] when the policy skips every mip.
pub fn plan_upload(
    document: &TexmDocument,
    policy: MipSkipPolicy,
) -> Result<TextureUploadPlan, TexmError> {
    let skip = usize::try_from(policy.skip_top_levels).map_err(|_| TexmError::EmptyUploadPlan)?;
    let mips = document
        .texture
        .mip_levels
        .iter()
        .enumerate()
        .skip(skip)
        .map(|(level, mip)| {
            Ok(UploadMip {
                level: u32::try_from(level).map_err(|_| TexmError::EmptyUploadPlan)?,
                width: mip.width,
                height: mip.height,
                offset: mip.offset,
                size: mip.size,
            })
        })
        .collect::<Result<Vec<_>, TexmError>>()?;
    if mips.is_empty() {
        return Err(TexmError::EmptyUploadPlan);
    }
    Ok(TextureUploadPlan {
        format: map_format(document.texture.header.format),
        width: document.texture.header.width,
        height: document.texture.header.height,
        mips,
        page_rects: document
            .texture
            .page_rects
            .iter()
            .copied()
            .map(map_page_rect)
            .collect(),
    })
}

/// Returns Page rectangles scaled to a selected mip level.
///
/// # Errors
///
/// Returns [`TexmError`] when `level` is outside the mip chain or scaled values
/// cannot be represented as `i16`.
pub fn scaled_page_rects(
    document: &TexmDocument,
    level: u32,
    policy: PageScalePolicy,
) -> Result<Vec<PageRect>, TexmError> {
    let mip = document.mip_level(level)?;
    document
        .texture
        .page_rects
        .iter()
        .copied()
        .map(|rect| {
            scale_page_rect(
                document.width(),
                document.height(),
                mip.width,
                mip.height,
                rect,
                policy,
            )
        })
        .collect()
}

impl TexmDocument {
    /// Width.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.texture.header.width
    }

    /// Height.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.texture.header.height
    }

    /// Pixel format.
    #[must_use]
    pub fn format(&self) -> PixelFormat {
        map_format(self.texture.header.format)
    }

    /// Mip count.
    #[must_use]
    pub fn mip_count(&self) -> usize {
        self.texture.mip_levels.len()
    }

    /// Returns a borrowed mip view.
    ///
    /// # Errors
    ///
    /// Returns [`TexmError`] when `level` is outside the mip chain or the stored
    /// range is outside the document.
    pub fn mip_level(&self, level: u32) -> Result<MipLevelView<'_>, TexmError> {
        let requested = usize::try_from(level).map_err(|_| TexmError::MipLevelOutOfRange {
            requested: level,
            mip_count: self.texture.mip_levels.len(),
        })?;
        let mip = self
            .texture
            .mip_levels
            .get(requested)
            .ok_or(TexmError::MipLevelOutOfRange {
                requested: level,
                mip_count: self.texture.mip_levels.len(),
            })?;
        let end = mip
            .offset
            .checked_add(mip.size)
            .ok_or(TexmError::MipDataOutOfBounds {
                offset: mip.offset,
                size: mip.size,
                document_size: self.bytes.len(),
            })?;
        let bytes = self
            .bytes
            .get(mip.offset..end)
            .ok_or(TexmError::MipDataOutOfBounds {
                offset: mip.offset,
                size: mip.size,
                document_size: self.bytes.len(),
            })?;
        Ok(MipLevelView {
            level,
            width: mip.width,
            height: mip.height,
            bytes,
        })
    }

    /// Page rectangles.
    #[must_use]
    pub fn page_rects(&self) -> Vec<PageRect> {
        self.texture
            .page_rects
            .iter()
            .copied()
            .map(map_page_rect)
            .collect()
    }
}

fn map_format(format: DiskPixelFormat) -> PixelFormat {
    match format {
        DiskPixelFormat::Indexed8 => PixelFormat::Indexed8,
        DiskPixelFormat::Rgb565 => PixelFormat::Rgb565,
        DiskPixelFormat::Rgb556 => PixelFormat::Rgb556,
        DiskPixelFormat::Argb4444 => PixelFormat::Argb4444,
        DiskPixelFormat::L8A8 => PixelFormat::L8A8,
        DiskPixelFormat::Rgb888x => PixelFormat::Rgb888x,
        DiskPixelFormat::Argb8888 => PixelFormat::Argb8888,
    }
}

fn map_page_rect(rect: DiskPageRect) -> PageRect {
    PageRect {
        x: rect.x,
        w: rect.w,
        y: rect.y,
        h: rect.h,
    }
}

fn scale_page_rect(
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
    rect: DiskPageRect,
    policy: PageScalePolicy,
) -> Result<PageRect, TexmError> {
    match policy {
        PageScalePolicy::FloorOriginCeilEnd => {
            let x0 = scale_floor(rect.x, target_width, source_width)?;
            let y0 = scale_floor(rect.y, target_height, source_height)?;
            let x1 = scale_ceil(
                rect.x
                    .checked_add(rect.w)
                    .ok_or_else(integer_overflow_error)?,
                target_width,
                source_width,
            )?;
            let y1 = scale_ceil(
                rect.y
                    .checked_add(rect.h)
                    .ok_or_else(integer_overflow_error)?,
                target_height,
                source_height,
            )?;
            Ok(PageRect {
                x: x0,
                w: checked_i16(i32::from(x1) - i32::from(x0))?,
                y: y0,
                h: checked_i16(i32::from(y1) - i32::from(y0))?,
            })
        }
    }
}

fn scale_floor(value: i16, numerator: u32, denominator: u32) -> Result<i16, TexmError> {
    checked_i16(div_floor(
        i64::from(value) * i64::from(numerator),
        i64::from(denominator),
    )?)
}

fn scale_ceil(value: i16, numerator: u32, denominator: u32) -> Result<i16, TexmError> {
    checked_i16(div_ceil(
        i64::from(value) * i64::from(numerator),
        i64::from(denominator),
    )?)
}

fn div_floor(value: i64, divisor: i64) -> Result<i32, TexmError> {
    let result = if value >= 0 {
        value / divisor
    } else {
        -((-value + divisor - 1) / divisor)
    };
    i32::try_from(result).map_err(|_| integer_overflow_error())
}

fn div_ceil(value: i64, divisor: i64) -> Result<i32, TexmError> {
    let result = if value >= 0 {
        (value + divisor - 1) / divisor
    } else {
        -((-value) / divisor)
    };
    i32::try_from(result).map_err(|_| integer_overflow_error())
}

fn checked_i16(value: i32) -> Result<i16, TexmError> {
    i16::try_from(value)
        .map_err(|_| TexmError::Format(format!("scaled Page rect value out of range: {value}")))
}

fn parse_texm(payload: &[u8]) -> Result<Texture, TexmError> {
    if payload.len() < 32 {
        return Err(TexmError::Format(format!(
            "Texm payload too small for header: {}",
            payload.len()
        )));
    }

    let magic = read_u32(payload, 0)?;
    if magic != TEXM_MAGIC {
        return Err(TexmError::Format(format!(
            "invalid Texm magic: 0x{magic:08X}"
        )));
    }

    let width = read_u32(payload, 4)?;
    let height = read_u32(payload, 8)?;
    let mip_count = read_u32(payload, 12)?;
    let format_raw = read_u32(payload, 28)?;

    if width == 0 || height == 0 {
        return Err(TexmError::Format(format!(
            "invalid Texm dimensions: {width}x{height}"
        )));
    }
    if mip_count == 0 {
        return Err(TexmError::Format(format!(
            "invalid Texm mip_count={mip_count}"
        )));
    }

    let format = DiskPixelFormat::from_raw(format_raw)
        .ok_or_else(|| TexmError::Format(format!("unknown Texm format={format_raw}")))?;
    let bytes_per_pixel = format.bytes_per_pixel();

    let mut offset = 32usize;
    let palette = if format == DiskPixelFormat::Indexed8 {
        let end = offset
            .checked_add(1024)
            .ok_or_else(integer_overflow_error)?;
        if end > payload.len() {
            return Err(TexmError::Format(format!(
                "Texm core data out of bounds: expected_end={end}, actual_size={}",
                payload.len()
            )));
        }
        let mut pal = [0u8; 1024];
        pal.copy_from_slice(&payload[offset..end]);
        offset = end;
        Some(pal)
    } else {
        None
    };

    let mut mip_levels =
        Vec::with_capacity(usize::try_from(mip_count).map_err(|_| integer_overflow_error())?);
    let mut w = width;
    let mut h = height;
    for _ in 0..mip_count {
        let pixel_count = u64::from(w)
            .checked_mul(u64::from(h))
            .ok_or_else(integer_overflow_error)?;
        let level_size_u64 = pixel_count
            .checked_mul(u64::try_from(bytes_per_pixel).map_err(|_| integer_overflow_error())?)
            .ok_or_else(integer_overflow_error)?;
        let level_size = usize::try_from(level_size_u64).map_err(|_| integer_overflow_error())?;
        let level_offset = offset;
        offset = offset
            .checked_add(level_size)
            .ok_or_else(integer_overflow_error)?;
        if offset > payload.len() {
            return Err(TexmError::Format(format!(
                "Texm core data out of bounds: expected_end={offset}, actual_size={}",
                payload.len()
            )));
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
            format,
        },
        palette,
        mip_levels,
        page_rects,
    })
}

fn decode_mip_rgba8_internal(
    texture: &Texture,
    payload: &[u8],
    mip_index: usize,
) -> Result<DecodedMip, TexmError> {
    let Some(level) = texture.mip_levels.get(mip_index).copied() else {
        return Err(TexmError::MipLevelOutOfRange {
            requested: u32::try_from(mip_index).unwrap_or(u32::MAX),
            mip_count: texture.mip_levels.len(),
        });
    };

    let end = level
        .offset
        .checked_add(level.size)
        .ok_or(TexmError::MipDataOutOfBounds {
            offset: level.offset,
            size: level.size,
            document_size: payload.len(),
        })?;
    let Some(level_data) = payload.get(level.offset..end) else {
        return Err(TexmError::MipDataOutOfBounds {
            offset: level.offset,
            size: level.size,
            document_size: payload.len(),
        });
    };

    let width = usize::try_from(level.width).map_err(|_| integer_overflow_error())?;
    let height = usize::try_from(level.height).map_err(|_| integer_overflow_error())?;
    let pixel_count = width
        .checked_mul(height)
        .ok_or_else(integer_overflow_error)?;
    let mut rgba = vec![0u8; pixel_count.saturating_mul(4)];

    match texture.header.format {
        DiskPixelFormat::Indexed8 => {
            let palette = texture
                .palette
                .as_ref()
                .ok_or_else(|| TexmError::Format("indexed Texm has no palette".to_string()))?;
            for (index, palette_index) in level_data.iter().copied().enumerate().take(pixel_count) {
                let palette_offset = usize::from(palette_index).saturating_mul(4);
                if palette_offset + 4 > palette.len() {
                    continue;
                }
                let out = index.saturating_mul(4);
                rgba[out] = palette[palette_offset];
                rgba[out + 1] = palette[palette_offset + 1];
                rgba[out + 2] = palette[palette_offset + 2];
                rgba[out + 3] = palette[palette_offset + 3];
            }
        }
        DiskPixelFormat::Rgb565 => decode_words(level_data, pixel_count, &mut rgba, decode_rgb565),
        DiskPixelFormat::Rgb556 => decode_words(level_data, pixel_count, &mut rgba, decode_rgb556),
        DiskPixelFormat::Argb4444 => {
            decode_words(level_data, pixel_count, &mut rgba, decode_argb4444);
        }
        DiskPixelFormat::L8A8 => {
            decode_words(level_data, pixel_count, &mut rgba, decode_luminance_alpha88);
        }
        DiskPixelFormat::Rgb888x => {
            decode_dwords(level_data, pixel_count, &mut rgba, decode_rgb888x);
        }
        DiskPixelFormat::Argb8888 => {
            decode_dwords(level_data, pixel_count, &mut rgba, decode_argb8888);
        }
    }

    Ok(DecodedMip {
        width: level.width,
        height: level.height,
        rgba8: rgba,
    })
}

fn parse_page_tail(payload: &[u8], core_end: usize) -> Result<Vec<DiskPageRect>, TexmError> {
    if core_end == payload.len() {
        return Ok(Vec::new());
    }
    if payload.len().saturating_sub(core_end) < 8 {
        return Err(TexmError::Format(format!(
            "invalid Page chunk size: expected=8, actual={}",
            payload.len().saturating_sub(core_end)
        )));
    }
    let magic = read_u32(payload, core_end)?;
    if magic != PAGE_MAGIC {
        return Err(TexmError::Format(
            "Texm tail exists but Page magic is missing".to_string(),
        ));
    }
    let rect_count = read_u32(payload, core_end + 4)?;
    let rect_count_usize = usize::try_from(rect_count).map_err(|_| integer_overflow_error())?;
    let expected_size = 8usize
        .checked_add(
            rect_count_usize
                .checked_mul(8)
                .ok_or_else(integer_overflow_error)?,
        )
        .ok_or_else(integer_overflow_error)?;
    let actual = payload.len().saturating_sub(core_end);
    if expected_size != actual {
        return Err(TexmError::Format(format!(
            "invalid Page chunk size: expected={expected_size}, actual={actual}"
        )));
    }

    let mut rects = Vec::with_capacity(rect_count_usize);
    for index in 0..rect_count_usize {
        let offset = core_end
            .checked_add(8)
            .and_then(|value| value.checked_add(index * 8))
            .ok_or_else(integer_overflow_error)?;
        rects.push(DiskPageRect {
            x: read_i16(payload, offset)?,
            w: read_i16(payload, offset + 2)?,
            y: read_i16(payload, offset + 4)?,
            h: read_i16(payload, offset + 6)?,
        });
    }
    Ok(rects)
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32, TexmError> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or_else(integer_overflow_error)?;
    let arr: [u8; 4] = bytes.try_into().map_err(|_| integer_overflow_error())?;
    Ok(u32::from_le_bytes(arr))
}

fn read_i16(data: &[u8], offset: usize) -> Result<i16, TexmError> {
    let bytes = data
        .get(offset..offset + 2)
        .ok_or_else(integer_overflow_error)?;
    let arr: [u8; 2] = bytes.try_into().map_err(|_| integer_overflow_error())?;
    Ok(i16::from_le_bytes(arr))
}

fn decode_words(data: &[u8], pixel_count: usize, rgba: &mut [u8], decode: fn(u16) -> [u8; 4]) {
    for index in 0..pixel_count {
        let offset = index.saturating_mul(2);
        let Some(bytes) = data.get(offset..offset + 2) else {
            break;
        };
        let word = u16::from_le_bytes([bytes[0], bytes[1]]);
        let pixel = decode(word);
        let out = index.saturating_mul(4);
        rgba[out..out + 4].copy_from_slice(&pixel);
    }
}

fn decode_dwords(data: &[u8], pixel_count: usize, rgba: &mut [u8], decode: fn(u32) -> [u8; 4]) {
    for index in 0..pixel_count {
        let offset = index.saturating_mul(4);
        let Some(bytes) = data.get(offset..offset + 4) else {
            break;
        };
        let dword = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let pixel = decode(dword);
        let out = index.saturating_mul(4);
        rgba[out..out + 4].copy_from_slice(&pixel);
    }
}

fn expand5(value: u16) -> u8 {
    u8::try_from((u32::from(value) * 255 + 15) / 31).unwrap_or(u8::MAX)
}

fn expand6(value: u16) -> u8 {
    u8::try_from((u32::from(value) * 255 + 31) / 63).unwrap_or(u8::MAX)
}

fn expand4(value: u16) -> u8 {
    u8::try_from(u32::from(value) * 17).unwrap_or(u8::MAX)
}

fn decode_rgb565(word: u16) -> [u8; 4] {
    let red = expand5((word >> 11) & 0x1F);
    let green = expand6((word >> 5) & 0x3F);
    let blue = expand5(word & 0x1F);
    [red, green, blue, 255]
}

fn decode_rgb556(word: u16) -> [u8; 4] {
    let red = expand5((word >> 11) & 0x1F);
    let green = expand5((word >> 6) & 0x1F);
    let blue = expand6(word & 0x3F);
    [red, green, blue, 255]
}

fn decode_argb4444(word: u16) -> [u8; 4] {
    let alpha = expand4((word >> 12) & 0x0F);
    let red = expand4((word >> 8) & 0x0F);
    let green = expand4((word >> 4) & 0x0F);
    let blue = expand4(word & 0x0F);
    [red, green, blue, alpha]
}

fn decode_luminance_alpha88(word: u16) -> [u8; 4] {
    let luminance = u8::try_from((word >> 8) & 0xFF).unwrap_or(u8::MAX);
    let alpha = u8::try_from(word & 0xFF).unwrap_or(u8::MAX);
    [luminance, luminance, luminance, alpha]
}

fn decode_rgb888x(dword: u32) -> [u8; 4] {
    let red = u8::try_from(dword & 0xFF).unwrap_or(u8::MAX);
    let green = u8::try_from((dword >> 8) & 0xFF).unwrap_or(u8::MAX);
    let blue = u8::try_from((dword >> 16) & 0xFF).unwrap_or(u8::MAX);
    [red, green, blue, 255]
}

fn decode_argb8888(dword: u32) -> [u8; 4] {
    let alpha = u8::try_from(dword & 0xFF).unwrap_or(u8::MAX);
    let red = u8::try_from((dword >> 8) & 0xFF).unwrap_or(u8::MAX);
    let green = u8::try_from((dword >> 16) & 0xFF).unwrap_or(u8::MAX);
    let blue = u8::try_from((dword >> 24) & 0xFF).unwrap_or(u8::MAX);
    [red, green, blue, alpha]
}

fn integer_overflow_error() -> TexmError {
    TexmError::Format("integer overflow".to_string())
}

/// Returns migration status.
#[must_use]
pub fn migration_facade_ready() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_nres::ReadProfile;
    use std::path::{Path, PathBuf};

    const TEXM_MAGIC: u32 = 0x6D78_6554;

    #[test]
    fn decodes_all_synthetic_formats() {
        let cases = [
            (0, PixelFormat::Indexed8, indexed_payload()),
            (
                565,
                PixelFormat::Rgb565,
                payload(1, 1, 565, &[&0xFFE0_u16.to_le_bytes()]),
            ),
            (
                556,
                PixelFormat::Rgb556,
                payload(1, 1, 556, &[&0xF800_u16.to_le_bytes()]),
            ),
            (
                4444,
                PixelFormat::Argb4444,
                payload(1, 1, 4444, &[&0xF12E_u16.to_le_bytes()]),
            ),
            (
                88,
                PixelFormat::L8A8,
                payload(1, 1, 88, &[&0x7F40_u16.to_le_bytes()]),
            ),
            (
                888,
                PixelFormat::Rgb888x,
                payload(1, 1, 888, &[&[0x11, 0x22, 0x33, 0x99]]),
            ),
            (
                8888,
                PixelFormat::Argb8888,
                payload(1, 1, 8888, &[&[0x40, 0x11, 0x22, 0x33]]),
            ),
        ];

        for (raw, expected, bytes) in cases {
            let document = decode_texm(Arc::from(bytes.into_boxed_slice()))
                .unwrap_or_else(|err| panic!("format {raw}: {err}"));
            assert_eq!(document.format(), expected);
            assert_eq!(document.mip_count(), 1);
            let rgba =
                decode_mip_rgba8(&document, 0).unwrap_or_else(|err| panic!("format {raw}: {err}"));
            assert_eq!(rgba.width, 1);
            assert_eq!(rgba.height, 1);
            assert_eq!(rgba.rgba8.len(), 4);
        }
    }

    #[test]
    fn rejects_zero_dimensions() {
        let err = decode_texm(Arc::from(
            payload(0, 1, 8888, &[&[0, 0, 0, 0]]).into_boxed_slice(),
        ))
        .expect_err("zero width");
        assert!(matches!(err, TexmError::Format(_)));
    }

    #[test]
    fn non_power_of_two_mip_chain_clamps_each_dimension() {
        let bytes = payload(3, 2, 8888, &[&[0; 3 * 2 * 4], &[1, 2, 3, 4], &[5, 6, 7, 8]]);
        let document = decode_texm(Arc::from(bytes.into_boxed_slice())).expect("document");

        assert_eq!(document.mip_level(0).expect("mip 0").width, 3);
        assert_eq!(document.mip_level(0).expect("mip 0").height, 2);
        assert_eq!(document.mip_level(1).expect("mip 1").width, 1);
        assert_eq!(document.mip_level(1).expect("mip 1").height, 1);
        assert_eq!(document.mip_level(2).expect("mip 2").width, 1);
        assert_eq!(document.mip_level(2).expect("mip 2").height, 1);
    }

    #[test]
    fn rejects_mip_size_arithmetic_overflow_or_oob() {
        let err = decode_texm(Arc::from(
            header(u32::MAX, u32::MAX, 1, 8888).into_boxed_slice(),
        ))
        .expect_err("huge mip");

        assert!(matches!(err, TexmError::Format(_)));
    }

    #[test]
    fn indexed_palette_requires_exact_1024_bytes() {
        let mut bytes = indexed_payload();
        bytes.remove(32 + 1023);

        let err = decode_texm(Arc::from(bytes.into_boxed_slice())).expect_err("short palette");

        assert!(matches!(err, TexmError::Format(_)));
    }

    #[test]
    fn channel_expansion_boundary_values_are_stable() {
        let document = decode_texm(Arc::from(
            payload(2, 1, 565, &[&[0x00, 0x00, 0xFF, 0xFF]]).into_boxed_slice(),
        ))
        .expect("rgb565 document");
        let rgba = decode_mip_rgba8(&document, 0).expect("rgba");

        assert_eq!(rgba.rgba8, vec![0, 0, 0, 255, 255, 255, 255, 255]);
    }

    #[test]
    fn rgb888x_preserves_fourth_disk_byte_but_outputs_opaque_alpha() {
        let document = decode_texm(Arc::from(
            payload(1, 1, 888, &[&[0x11, 0x22, 0x33, 0x99]]).into_boxed_slice(),
        ))
        .expect("rgb888x document");

        assert_eq!(
            document.mip_level(0).expect("mip").bytes,
            &[0x11, 0x22, 0x33, 0x99]
        );
        assert_eq!(
            decode_mip_rgba8(&document, 0).expect("rgba").rgba8,
            vec![0x11, 0x22, 0x33, 0xFF]
        );
    }

    #[test]
    fn page_tail_absent_and_exact_rect_framing() {
        let absent = decode_texm(Arc::from(
            payload(1, 1, 8888, &[&[0, 0, 0, 0]]).into_boxed_slice(),
        ))
        .expect("page absent");
        assert!(absent.page_rects().is_empty());

        let mut bytes = payload(1, 1, 8888, &[&[0, 0, 0, 0]]);
        push_page_tail(&mut bytes, &[(1, 2, 3, 4)]);
        let document = decode_texm(Arc::from(bytes.into_boxed_slice())).expect("page rect");

        assert_eq!(
            document.page_rects(),
            vec![PageRect {
                x: 1,
                w: 2,
                y: 3,
                h: 4,
            }]
        );
    }

    #[test]
    fn invalid_page_magic_size_and_trailing_bytes_are_rejected() {
        let mut missing_magic = payload(1, 1, 8888, &[&[0, 0, 0, 0]]);
        missing_magic.extend_from_slice(b"tail");
        assert!(decode_texm(Arc::from(missing_magic.into_boxed_slice())).is_err());

        let mut wrong_size = payload(1, 1, 8888, &[&[0, 0, 0, 0]]);
        wrong_size.extend_from_slice(&PAGE_MAGIC.to_le_bytes());
        wrong_size.extend_from_slice(&2_u32.to_le_bytes());
        wrong_size.extend_from_slice(&[0; 8]);
        assert!(decode_texm(Arc::from(wrong_size.into_boxed_slice())).is_err());
    }

    #[test]
    fn exposes_mip_views_and_upload_plan_without_mutating_document() {
        let bytes = payload(2, 1, 8888, &[&[1, 2, 3, 4, 5, 6, 7, 8], &[9, 10, 11, 12]]);
        let original = bytes.clone();
        let document = decode_texm(Arc::from(bytes.into_boxed_slice())).expect("document");

        let mip1 = document.mip_level(1).expect("mip 1");
        assert_eq!(mip1.width, 1);
        assert_eq!(mip1.height, 1);
        assert_eq!(mip1.bytes, &[9, 10, 11, 12]);
        let plan = plan_upload(&document, MipSkipPolicy { skip_top_levels: 1 }).expect("plan");
        assert_eq!(plan.mips.len(), 1);
        assert_eq!(plan.mips[0].level, 1);
        assert_eq!(&document.bytes[..], &original[..]);
    }

    #[test]
    fn page_scaling_uses_floor_origin_and_ceil_end_policy() {
        let mut bytes = payload(5, 3, 8888, &[&[0; 5 * 3 * 4], &[0; 2 * 1 * 4]]);
        push_page_tail(&mut bytes, &[(1, 3, 1, 2)]);
        let document = decode_texm(Arc::from(bytes.into_boxed_slice())).expect("document");

        assert_eq!(
            scaled_page_rects(&document, 1, PageScalePolicy::FloorOriginCeilEnd).expect("scaled"),
            vec![PageRect {
                x: 0,
                w: 2,
                y: 0,
                h: 1,
            }]
        );
        assert_eq!(
            plan_upload(&document, MipSkipPolicy { skip_top_levels: 1 })
                .expect("plan")
                .page_rects,
            vec![PageRect {
                x: 1,
                w: 3,
                y: 1,
                h: 2,
            }]
        );
    }

    #[test]
    fn arbitrary_texm_payloads_do_not_panic() {
        for len in 0..128usize {
            let mut bytes = vec![0xCC; len];
            if len >= 4 {
                bytes[0..4].copy_from_slice(&TEXM_MAGIC.to_le_bytes());
            }
            let result = std::panic::catch_unwind(|| {
                let _ = decode_texm(Arc::from(bytes.into_boxed_slice()));
            });
            assert!(result.is_ok());
        }
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_corpus_texm_assets_validate_and_decode_mip0() {
        for (corpus, expected) in [("IS", 518_usize), ("IS2", 631_usize)] {
            let root = corpus_root(corpus);
            let mut count = 0usize;
            for path in files_under(&root) {
                let Ok(bytes) = std::fs::read(&path) else {
                    continue;
                };
                let Ok(archive) = fparkan_nres::decode(
                    Arc::from(bytes.into_boxed_slice()),
                    ReadProfile::Compatible,
                ) else {
                    continue;
                };
                for entry in archive
                    .entries()
                    .iter()
                    .filter(|entry| entry.meta().type_id == TEXM_MAGIC)
                {
                    let payload = archive.payload(entry.id()).expect("payload");
                    let document = decode_texm(Arc::from(payload.to_vec().into_boxed_slice()))
                        .unwrap_or_else(|err| {
                            panic!("{corpus} {path:?} {:?}: {err}", entry.name_bytes())
                        });
                    decode_mip_rgba8(&document, 0).unwrap_or_else(|err| {
                        panic!("{corpus} {path:?} {:?}: {err}", entry.name_bytes())
                    });
                    count += 1;
                }
            }
            assert_eq!(count, expected, "{corpus} Texm count");
        }
    }

    fn indexed_payload() -> Vec<u8> {
        let mut palette = [0_u8; 1024];
        palette[4..8].copy_from_slice(&[10, 20, 30, 255]);
        let mut out = header(1, 1, 1, 0);
        out.extend_from_slice(&palette);
        out.push(1);
        out
    }

    fn payload(width: u32, height: u32, format: u32, mip_levels: &[&[u8]]) -> Vec<u8> {
        let mut out = header(
            width,
            height,
            u32::try_from(mip_levels.len()).expect("mip count"),
            format,
        );
        for level in mip_levels {
            out.extend_from_slice(level);
        }
        out
    }

    fn header(width: u32, height: u32, mip_count: u32, format: u32) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&TEXM_MAGIC.to_le_bytes());
        out.extend_from_slice(&width.to_le_bytes());
        out.extend_from_slice(&height.to_le_bytes());
        out.extend_from_slice(&mip_count.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&format.to_le_bytes());
        out
    }

    fn push_page_tail(out: &mut Vec<u8>, rects: &[(i16, i16, i16, i16)]) {
        out.extend_from_slice(&PAGE_MAGIC.to_le_bytes());
        out.extend_from_slice(
            &u32::try_from(rects.len())
                .expect("rect count")
                .to_le_bytes(),
        );
        for (x, w, y, h) in rects {
            out.extend_from_slice(&x.to_le_bytes());
            out.extend_from_slice(&w.to_le_bytes());
            out.extend_from_slice(&y.to_le_bytes());
            out.extend_from_slice(&h.to_le_bytes());
        }
    }

    fn corpus_root(name: &str) -> PathBuf {
        let variable = match name {
            "IS" => "FPARKAN_CORPUS_PART1_ROOT",
            "IS2" => "FPARKAN_CORPUS_PART2_ROOT",
            _ => panic!("unknown licensed corpus part: {name}"),
        };
        let root = std::env::var_os(variable)
            .map(PathBuf::from)
            .unwrap_or_else(|| panic!("{variable} is required for licensed corpus tests"));
        assert!(
            root.is_dir(),
            "licensed corpus root is missing: {}",
            root.display()
        );
        root
    }

    fn files_under(root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(path) = stack.pop() {
            let Ok(read_dir) = std::fs::read_dir(path) else {
                continue;
            };
            for entry in read_dir.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    out.push(path);
                }
            }
        }
        out.sort();
        out
    }
}
