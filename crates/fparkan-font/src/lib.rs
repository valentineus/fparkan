#![forbid(unsafe_code)]
//! Safe reader for the fixed Tfnt metrics and its embedded Texm image.

use fparkan_texm::{TexmDocument, TexmError};
use std::sync::Arc;

const PREFIX_LEN: usize = 0x14;
const GLYPH_TABLE_END: usize = 0x1014;
const GLYPH_COUNT: usize = 256;

/// Confirmed Tfnt metrics and the preserved opaque word at offset `0x0c`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TfntHeader {
    /// Lower bound used by the native fixed-width drawing path.
    pub fixed_width_floor: i32,
    /// Vertical extent used by native clipping and drawing.
    pub vertical_extent: i32,
    /// Raw bits copied from offset `0x0c`; its semantic name is unknown.
    pub word_0c_bits: u32,
    /// Per-input-byte spacing used by native text measurement.
    pub character_spacing: i32,
}

/// One 16-byte glyph record; the first three words remain opaque.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TfntGlyph {
    /// First opaque DWORD.
    pub word0: u32,
    /// Second opaque DWORD.
    pub word1: u32,
    /// Third opaque DWORD.
    pub word2: u32,
    /// Signed advance at record offset `0x0c`.
    pub advance: i32,
}

/// Decoded Tfnt prefix, fixed glyph table, and embedded Texm document.
#[derive(Clone, Debug)]
pub struct TfntDocument {
    /// Header metrics.
    pub header: TfntHeader,
    /// Records indexed by the native unsigned input byte, `0..=255`.
    pub glyphs: [TfntGlyph; GLYPH_COUNT],
    /// Texture document parsed by the shared Texm reader.
    pub texm: TexmDocument,
}

/// Tfnt decode error.
#[derive(Debug)]
pub enum TfntError {
    /// The safe file reader requires the corpus signature `Tfnt`.
    InvalidMagic { got: [u8; 4] },
    /// The fixed 20-byte prefix is incomplete.
    TruncatedHeader { actual: usize, required: usize },
    /// The fixed 256-record table is incomplete.
    TruncatedGlyphTable { actual: usize, required: usize },
    /// The embedded texture could not be decoded.
    Texm(TexmError),
}

impl std::fmt::Display for TfntError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidMagic { got } => write!(f, "invalid Tfnt magic: {got:02X?}"),
            Self::TruncatedHeader { actual, required } => {
                write!(f, "Tfnt header truncated: {actual} bytes, need {required}")
            }
            Self::TruncatedGlyphTable { actual, required } => {
                write!(
                    f,
                    "Tfnt glyph table truncated: {actual} bytes, need {required}"
                )
            }
            Self::Texm(source) => write!(f, "embedded Texm: {source}"),
        }
    }
}

impl std::error::Error for TfntError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Texm(source) => Some(source),
            Self::InvalidMagic { .. }
            | Self::TruncatedHeader { .. }
            | Self::TruncatedGlyphTable { .. } => None,
        }
    }
}

/// Decodes a Tfnt file and its embedded Texm texture.
///
/// # Errors
///
/// Returns [`TfntError`] for a bad signature, an incomplete fixed header or
/// glyph table, or an invalid embedded Texm document.
pub fn decode_tfnt(bytes: Arc<[u8]>) -> Result<TfntDocument, TfntError> {
    if bytes.len() < PREFIX_LEN {
        return Err(TfntError::TruncatedHeader {
            actual: bytes.len(),
            required: PREFIX_LEN,
        });
    }

    let mut magic = [0; 4];
    magic.copy_from_slice(&bytes[..4]);
    if magic != *b"Tfnt" {
        return Err(TfntError::InvalidMagic { got: magic });
    }

    if bytes.len() < GLYPH_TABLE_END {
        return Err(TfntError::TruncatedGlyphTable {
            actual: bytes.len(),
            required: GLYPH_TABLE_END,
        });
    }

    let header = TfntHeader {
        fixed_width_floor: read_i32_le(&bytes, 0x04),
        vertical_extent: read_i32_le(&bytes, 0x08),
        word_0c_bits: read_u32_le(&bytes, 0x0c),
        character_spacing: read_i32_le(&bytes, 0x10),
    };
    let mut glyphs = [TfntGlyph::default(); GLYPH_COUNT];
    for (glyph, record) in glyphs
        .iter_mut()
        .zip(bytes[PREFIX_LEN..GLYPH_TABLE_END].chunks_exact(16))
    {
        *glyph = TfntGlyph {
            word0: read_u32_le(record, 0),
            word1: read_u32_le(record, 4),
            word2: read_u32_le(record, 8),
            advance: read_i32_le(record, 12),
        };
    }

    let texm =
        fparkan_texm::decode_texm(Arc::from(&bytes[GLYPH_TABLE_END..])).map_err(TfntError::Texm)?;

    Ok(TfntDocument {
        header,
        glyphs,
        texm,
    })
}

fn read_u32_le(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("fixed range was bounds-checked"),
    )
}

fn read_i32_le(bytes: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("fixed range was bounds-checked"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_nres::{decode as decode_nres, ReadProfile};
    use fparkan_texm::PixelFormat;
    use std::path::PathBuf;

    #[test]
    fn reads_distinct_header_values_and_preserves_nan_bits() {
        let mut bytes = tfnt_prefix();
        put_u32(&mut bytes, 0x04, (-17_i32) as u32);
        put_u32(&mut bytes, 0x08, 513);
        put_u32(&mut bytes, 0x0c, 0x7fc1_2345);
        put_u32(&mut bytes, 0x10, (-3_i32) as u32);
        bytes.extend_from_slice(&indexed_texm(2, 1, &[0xA5, 0x5A]));

        let document = decode_tfnt(Arc::from(bytes.into_boxed_slice())).expect("Tfnt");

        assert_eq!(document.header.fixed_width_floor, -17);
        assert_eq!(document.header.vertical_extent, 513);
        assert_eq!(document.header.word_0c_bits, 0x7fc1_2345);
        assert_eq!(document.header.character_spacing, -3);
    }

    #[test]
    fn preserves_distinct_glyph_records_including_byte_255_and_signed_advances() {
        let mut bytes = tfnt_prefix();
        write_glyph(
            &mut bytes,
            7,
            [0x7fc1_2345, 0x89ab_cdef, 0x0102_0304, (-23_i32) as u32],
        );
        write_glyph(
            &mut bytes,
            255,
            [0x8000_0000, 0x7654_3210, 0xffff_ffff, i32::MIN as u32],
        );
        bytes.extend_from_slice(&indexed_texm(2, 1, &[1, 2]));

        let document = decode_tfnt(Arc::from(bytes.into_boxed_slice())).expect("Tfnt");

        assert_eq!(
            document.glyphs[7],
            TfntGlyph {
                word0: 0x7fc1_2345,
                word1: 0x89ab_cdef,
                word2: 0x0102_0304,
                advance: -23,
            }
        );
        assert_eq!(document.glyphs[255].word0, 0x8000_0000);
        assert_eq!(document.glyphs[255].word1, 0x7654_3210);
        assert_eq!(document.glyphs[255].word2, u32::MAX);
        assert_eq!(document.glyphs[255].advance, i32::MIN);
    }

    #[test]
    fn delegates_embedded_texture_to_texm_reader() {
        let mut bytes = tfnt_prefix();
        bytes.extend_from_slice(&indexed_texm(2, 1, &[0xA5, 0x5A]));

        let document = decode_tfnt(Arc::from(bytes.into_boxed_slice())).expect("Tfnt");

        assert_eq!(document.texm.width(), 2);
        assert_eq!(document.texm.height(), 1);
        assert_eq!(document.texm.mip_count(), 1);
        assert_eq!(document.texm.format(), PixelFormat::Indexed8);
        assert_eq!(document.texm.mip_level(0).expect("mip").bytes, [0xA5, 0x5A]);
    }

    #[test]
    fn rejects_wrong_magic_and_short_prefix() {
        let mut wrong_magic = tfnt_prefix();
        wrong_magic[..4].copy_from_slice(b"BAD!");
        assert!(matches!(
            decode_tfnt(Arc::from(wrong_magic.into_boxed_slice())),
            Err(TfntError::InvalidMagic { .. })
        ));

        let mut short_prefix = tfnt_prefix();
        short_prefix.truncate(PREFIX_LEN - 1);
        assert!(matches!(
            decode_tfnt(Arc::from(short_prefix.into_boxed_slice())),
            Err(TfntError::TruncatedHeader { .. })
        ));
    }

    #[test]
    fn rejects_short_fixed_glyph_table() {
        let mut bytes = tfnt_prefix();
        bytes.truncate(GLYPH_TABLE_END - 1);

        assert!(matches!(
            decode_tfnt(Arc::from(bytes.into_boxed_slice())),
            Err(TfntError::TruncatedGlyphTable { .. })
        ));
    }

    #[test]
    fn reports_invalid_and_truncated_embedded_texm_without_panicking() {
        let mut invalid = tfnt_prefix();
        invalid.extend_from_slice(b"Not a Texm payload, but long enough to reach its reader.");
        assert!(matches!(
            decode_tfnt(Arc::from(invalid.into_boxed_slice())),
            Err(TfntError::Texm(_))
        ));

        let mut truncated = tfnt_prefix();
        truncated.extend_from_slice(b"Texm");
        assert!(matches!(
            decode_tfnt(Arc::from(truncated.into_boxed_slice())),
            Err(TfntError::Texm(_))
        ));
    }

    #[test]
    #[ignore = "requires licensed GOG corpus"]
    fn decodes_original_gog_tfnt_resources() {
        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| panic!("FPARKAN_CORPUS_PART1_ROOT is required for this test"));
        assert!(
            root.is_dir(),
            "licensed corpus root is missing: {}",
            root.display()
        );

        let font_bytes = std::fs::read(root.join("ui/font.lib")).expect("read ui/font.lib");
        let fonts = decode_nres(
            Arc::from(font_bytes.into_boxed_slice()),
            ReadProfile::Strict,
        )
        .expect("decode ui/font.lib");
        let expected = [
            ("gf_1024.tft", 256, 256),
            ("tf_1024.tft", 256, 256),
            ("mf_1024.tft", 256, 256),
            ("gf_800.tft", 128, 128),
            ("tf_800.tft", 128, 128),
            ("mf_800.tft", 256, 256),
            ("gf_640.tft", 128, 128),
            ("tf_640.tft", 128, 128),
            ("mf_640.tft", 256, 256),
        ];
        assert_eq!(fonts.entry_count(), expected.len());
        for (name, width, height) in expected {
            let id = fonts
                .find(name)
                .unwrap_or_else(|| panic!("missing NRes font {name}"));
            let entry = fonts.entry(id).expect("font entry");
            assert_eq!(entry.meta().type_id, u32::from_le_bytes(*b"FONT"), "{name}");
            let document = decode_tfnt(Arc::from(fonts.payload(id).expect("font payload")))
                .unwrap_or_else(|err| panic!("{name}: {err}"));
            assert_eq!(document.texm.width(), width, "{name}");
            assert_eq!(document.texm.height(), height, "{name}");
            assert_eq!(document.texm.format(), PixelFormat::Indexed8, "{name}");
            assert_eq!(document.texm.mip_count(), 1, "{name}");
            assert_eq!(
                document.texm.mip_level(0).expect("mip").bytes.len(),
                width as usize * height as usize,
                "{name}"
            );
            if name == "gf_640.tft" {
                assert_eq!(
                    document.header,
                    TfntHeader {
                        fixed_width_floor: 8,
                        vertical_extent: 7,
                        word_0c_bits: 0x3d80_0000,
                        character_spacing: 1,
                    }
                );
                assert_eq!(
                    document.glyphs[65],
                    TfntGlyph {
                        word0: 0x3c00_0000,
                        word1: 0x3d80_0000,
                        word2: 0x3e80_0000,
                        advance: 6,
                    }
                );
                assert_eq!(document.glyphs[255].advance, 5);
            }
        }

        let sys_bytes = std::fs::read(root.join("sys.lib")).expect("read sys.lib");
        let sys = decode_nres(Arc::from(sys_bytes.into_boxed_slice()), ReadProfile::Strict)
            .expect("decode sys.lib");
        let id = sys.find("ARIALTEX.TFT").expect("ARIALTEX.TFT entry");
        let entry = sys.entry(id).expect("ARIALTEX.TFT entry");
        assert_eq!(entry.meta().type_id, u32::from_le_bytes(*b"FONT"));
        let document = decode_tfnt(Arc::from(sys.payload(id).expect("ARIALTEX.TFT payload")))
            .expect("decode ARIALTEX.TFT");
        assert_eq!(document.texm.width(), 128);
        assert_eq!(document.texm.height(), 128);
        assert_eq!(document.texm.format(), PixelFormat::Indexed8);
        assert_eq!(document.texm.mip_count(), 1);
        assert_eq!(
            document.texm.mip_level(0).expect("mip").bytes.len(),
            128 * 128
        );
        assert_eq!(
            document.header,
            TfntHeader {
                fixed_width_floor: 8,
                vertical_extent: 7,
                word_0c_bits: 0x3d80_0000,
                character_spacing: 1,
            }
        );
        assert_eq!(document.glyphs[65].advance, 6);
        assert_eq!(document.glyphs[255].advance, 4);
    }

    fn tfnt_prefix() -> Vec<u8> {
        let mut bytes = vec![0; GLYPH_TABLE_END];
        bytes[..4].copy_from_slice(b"Tfnt");
        bytes
    }

    fn indexed_texm(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(32 + 1024 + pixels.len());
        for word in [0x6d78_6554, width, height, 1, 0x20, 0x0100_0000, 0, 0] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        bytes.extend_from_slice(&[0; 1024]);
        bytes.extend_from_slice(pixels);
        bytes
    }

    fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn write_glyph(bytes: &mut [u8], index: usize, words: [u32; 4]) {
        let offset = PREFIX_LEN + index * 16;
        for (word_index, value) in words.into_iter().enumerate() {
            put_u32(bytes, offset + word_index * 4, value);
        }
    }
}
