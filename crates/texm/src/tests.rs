use super::*;
use nres::Archive;
use std::fs;
use std::path::{Path, PathBuf};

fn collect_files_recursive(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files_recursive(&path, out);
        } else if path.is_file() {
            out.push(path);
        }
    }
}

fn nres_test_files() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("testdata");
    let mut files = Vec::new();
    collect_files_recursive(&root, &mut files);
    files.sort();
    files
        .into_iter()
        .filter(|path| {
            fs::read(path)
                .map(|bytes| bytes.get(0..4) == Some(b"NRes"))
                .unwrap_or(false)
        })
        .collect()
}

fn build_texm_payload(
    width: u32,
    height: u32,
    format_raw: u32,
    flags5: u32,
    palette: Option<[u8; 1024]>,
    mip_levels: &[&[u8]],
) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&TEXM_MAGIC.to_le_bytes());
    payload.extend_from_slice(&width.to_le_bytes());
    payload.extend_from_slice(&height.to_le_bytes());
    payload.extend_from_slice(
        &u32::try_from(mip_levels.len())
            .expect("mip level count overflow in test")
            .to_le_bytes(),
    );
    payload.extend_from_slice(&0u32.to_le_bytes()); // flags4
    payload.extend_from_slice(&flags5.to_le_bytes());
    payload.extend_from_slice(&0u32.to_le_bytes()); // unk6
    payload.extend_from_slice(&format_raw.to_le_bytes());
    if let Some(palette) = palette {
        payload.extend_from_slice(&palette);
    }
    for level in mip_levels {
        payload.extend_from_slice(level);
    }
    payload
}

#[test]
fn texm_parse_all_game_textures() {
    let archives = nres_test_files();
    if archives.is_empty() {
        eprintln!("skipping texm_parse_all_game_textures: no NRes files in testdata");
        return;
    }

    let mut texm_total = 0usize;
    let mut texm_with_page = 0usize;
    for archive_path in archives {
        let archive = Archive::open_path(&archive_path)
            .unwrap_or_else(|err| panic!("failed to open {}: {err}", archive_path.display()));

        for entry in archive.entries() {
            if entry.meta.kind != TEXM_MAGIC {
                continue;
            }
            texm_total += 1;
            let payload = archive.read(entry.id).unwrap_or_else(|err| {
                panic!(
                    "failed to read Texm entry '{}' in {}: {err}",
                    entry.meta.name,
                    archive_path.display()
                )
            });
            let texture = parse_texm(payload.as_slice()).unwrap_or_else(|err| {
                panic!(
                    "failed to parse Texm '{}' in {}: {err}",
                    entry.meta.name,
                    archive_path.display()
                )
            });
            if !texture.page_rects.is_empty() {
                texm_with_page += 1;
            }

            assert!(
                texture.core_size() <= payload.as_slice().len(),
                "core size must be within payload for '{}' in {}",
                entry.meta.name,
                archive_path.display()
            );
            assert_eq!(
                usize::try_from(texture.header.mip_count).ok(),
                Some(texture.mip_levels.len()),
                "mip count mismatch for '{}' in {}",
                entry.meta.name,
                archive_path.display()
            );
        }
    }

    assert!(texm_total > 0, "no Texm textures found");
    assert!(
        texm_with_page > 0,
        "expected at least one Texm texture with Page chunk"
    );
}

#[test]
fn texm_parse_minimal_argb8888_no_page() {
    let payload = build_texm_payload(1, 1, 8888, 0, None, &[&[1, 2, 3, 4]]);

    let parsed = parse_texm(&payload).expect("failed to parse minimal texm");
    assert_eq!(parsed.header.width, 1);
    assert_eq!(parsed.header.height, 1);
    assert_eq!(parsed.mip_levels.len(), 1);
    assert!(parsed.page_rects.is_empty());
}

#[test]
fn texm_decode_minimal_argb8888_no_page() {
    let payload = build_texm_payload(1, 1, 8888, 0, None, &[&[0x40, 0x11, 0x22, 0x33]]);
    let parsed = parse_texm(&payload).expect("failed to parse minimal texm");
    let decoded = decode_mip_rgba8(&parsed, &payload, 0).expect("failed to decode mip");
    assert_eq!(decoded.width, 1);
    assert_eq!(decoded.height, 1);
    assert_eq!(decoded.rgba8, vec![0x11, 0x22, 0x33, 0x40]);
}

#[test]
fn texm_decode_rgb565() {
    let word = 0xFFE0u16; // r=31 g=63 b=0
    let payload = build_texm_payload(1, 1, 565, 0, None, &[&word.to_le_bytes()]);
    let parsed = parse_texm(&payload).expect("failed to parse rgb565 texm");
    let decoded = decode_mip_rgba8(&parsed, &payload, 0).expect("failed to decode rgb565 texm");
    assert_eq!(decoded.rgba8, vec![255, 255, 0, 255]);
}

#[test]
fn texm_decode_rgb556() {
    let word = 0xF800u16; // r=31 g=0 b=0
    let payload = build_texm_payload(1, 1, 556, 0, None, &[&word.to_le_bytes()]);
    let parsed = parse_texm(&payload).expect("failed to parse rgb556 texm");
    let decoded = decode_mip_rgba8(&parsed, &payload, 0).expect("failed to decode rgb556 texm");
    assert_eq!(decoded.rgba8, vec![255, 0, 0, 255]);
}

#[test]
fn texm_decode_argb4444() {
    let word = 0xF12Eu16; // a=F r=1 g=2 b=E
    let payload = build_texm_payload(1, 1, 4444, 0, None, &[&word.to_le_bytes()]);
    let parsed = parse_texm(&payload).expect("failed to parse argb4444 texm");
    let decoded = decode_mip_rgba8(&parsed, &payload, 0).expect("failed to decode argb4444 texm");
    assert_eq!(decoded.rgba8, vec![17, 34, 238, 255]);
}

#[test]
fn texm_decode_luminance_alpha88() {
    let word = 0x7F40u16; // luminance=0x7F alpha=0x40
    let payload = build_texm_payload(1, 1, 88, 0, None, &[&word.to_le_bytes()]);
    let parsed = parse_texm(&payload).expect("failed to parse la88 texm");
    let decoded = decode_mip_rgba8(&parsed, &payload, 0).expect("failed to decode la88 texm");
    assert_eq!(decoded.rgba8, vec![0x7F, 0x7F, 0x7F, 0x40]);
}

#[test]
fn texm_decode_rgb888x() {
    let payload = build_texm_payload(1, 1, 888, 0, None, &[&[0x11, 0x22, 0x33, 0x99]]);
    let parsed = parse_texm(&payload).expect("failed to parse rgb888 texm");
    let decoded = decode_mip_rgba8(&parsed, &payload, 0).expect("failed to decode rgb888 texm");
    assert_eq!(decoded.rgba8, vec![0x11, 0x22, 0x33, 255]);
}

#[test]
fn texm_parse_indexed_with_page_chunk() {
    let mut palette = [0u8; 1024];
    palette[4..8].copy_from_slice(&[10, 20, 30, 255]);
    let mut payload = build_texm_payload(2, 2, 0, 0, Some(palette), &[&[1, 1, 1, 1]]);
    payload.extend_from_slice(&PAGE_MAGIC.to_le_bytes());
    payload.extend_from_slice(&1u32.to_le_bytes()); // rect_count
    payload.extend_from_slice(&0i16.to_le_bytes()); // x
    payload.extend_from_slice(&2i16.to_le_bytes()); // w
    payload.extend_from_slice(&0i16.to_le_bytes()); // y
    payload.extend_from_slice(&2i16.to_le_bytes()); // h

    let parsed = parse_texm(&payload).expect("failed to parse indexed texm");
    assert!(parsed.palette.is_some());
    assert_eq!(parsed.page_rects.len(), 1);
    assert_eq!(
        parsed.page_rects[0],
        PageRect {
            x: 0,
            w: 2,
            y: 0,
            h: 2
        }
    );
}

#[test]
fn texm_decode_indexed_with_palette_last_entry() {
    let mut palette = [0u8; 1024];
    palette[4..8].copy_from_slice(&[10, 20, 30, 255]); // index 1
    palette[8..12].copy_from_slice(&[40, 50, 60, 200]); // index 2
    palette[1020..1024].copy_from_slice(&[1, 2, 3, 4]); // index 255 (last)
    let payload = build_texm_payload(3, 1, 0, 0, Some(palette), &[&[1u8, 2u8, 255u8]]);

    let parsed = parse_texm(&payload).expect("failed to parse indexed texm");
    let decoded = decode_mip_rgba8(&parsed, &payload, 0).expect("failed to decode indexed texm");
    assert_eq!(decoded.width, 3);
    assert_eq!(decoded.height, 1);
    assert_eq!(
        decoded.rgba8,
        vec![10, 20, 30, 255, 40, 50, 60, 200, 1, 2, 3, 4]
    );
}

#[test]
fn texm_parse_multi_mip_offsets() {
    let mip0 = [0x10u8; 32]; // 4*2*4
    let mip1 = [0x20u8; 8]; // 2*1*4
    let mip2 = [0x30u8; 4]; // 1*1*4
    let payload = build_texm_payload(4, 2, 8888, 0, None, &[&mip0, &mip1, &mip2]);

    let parsed = parse_texm(&payload).expect("failed to parse multi-mip texm");
    assert_eq!(parsed.header.mip_count, 3);
    assert_eq!(parsed.mip_levels.len(), 3);
    assert_eq!(
        parsed.mip_levels,
        vec![
            MipLevel {
                width: 4,
                height: 2,
                offset: 32,
                size: 32
            },
            MipLevel {
                width: 2,
                height: 1,
                offset: 64,
                size: 8
            },
            MipLevel {
                width: 1,
                height: 1,
                offset: 72,
                size: 4
            },
        ]
    );
}

#[test]
fn texm_preserves_flags5_for_mip_skip_metadata() {
    let payload = build_texm_payload(1, 1, 8888, 0x0000_00A5, None, &[&[0, 0, 0, 0]]);
    let parsed = parse_texm(&payload).expect("failed to parse texm");
    assert_eq!(parsed.header.flags5, 0x0000_00A5);
}

#[test]
fn texm_errors_for_invalid_header_values() {
    let mut bad_magic = build_texm_payload(1, 1, 8888, 0, None, &[&[0, 0, 0, 0]]);
    bad_magic[0..4].copy_from_slice(&0u32.to_le_bytes());
    assert!(matches!(
        parse_texm(&bad_magic),
        Err(Error::InvalidMagic { .. })
    ));

    let zero_dims = build_texm_payload(0, 1, 8888, 0, None, &[&[]]);
    assert!(matches!(
        parse_texm(&zero_dims),
        Err(Error::InvalidDimensions { .. })
    ));

    let mut bad_mips = build_texm_payload(1, 1, 8888, 0, None, &[&[0, 0, 0, 0]]);
    bad_mips[12..16].copy_from_slice(&0u32.to_le_bytes());
    assert!(matches!(
        parse_texm(&bad_mips),
        Err(Error::InvalidMipCount { .. })
    ));

    let bad_format = build_texm_payload(1, 1, 12345, 0, None, &[&[0, 0, 0, 0]]);
    assert!(matches!(
        parse_texm(&bad_format),
        Err(Error::UnknownFormat { .. })
    ));
}

#[test]
fn texm_errors_for_page_chunk_and_mip_bounds() {
    let mut bad_page = build_texm_payload(1, 1, 8888, 0, None, &[&[0, 0, 0, 0]]);
    bad_page.extend_from_slice(b"X");
    assert!(matches!(
        parse_texm(&bad_page),
        Err(Error::InvalidPageSize { .. })
    ));

    let payload = build_texm_payload(1, 1, 8888, 0, None, &[&[1, 2, 3, 4]]);
    let parsed = parse_texm(&payload).expect("failed to parse valid texm");
    assert!(matches!(
        decode_mip_rgba8(&parsed, &payload, 7),
        Err(Error::MipIndexOutOfRange { .. })
    ));

    let truncated = &payload[..payload.len() - 1];
    assert!(matches!(
        decode_mip_rgba8(&parsed, truncated, 0),
        Err(Error::MipDataOutOfBounds { .. })
    ));
}
