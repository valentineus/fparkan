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
    let mut payload = Vec::new();
    payload.extend_from_slice(&TEXM_MAGIC.to_le_bytes());
    payload.extend_from_slice(&1u32.to_le_bytes()); // width
    payload.extend_from_slice(&1u32.to_le_bytes()); // height
    payload.extend_from_slice(&1u32.to_le_bytes()); // mip_count
    payload.extend_from_slice(&0u32.to_le_bytes()); // flags4
    payload.extend_from_slice(&0u32.to_le_bytes()); // flags5
    payload.extend_from_slice(&0u32.to_le_bytes()); // unk6
    payload.extend_from_slice(&8888u32.to_le_bytes()); // format
    payload.extend_from_slice(&[1, 2, 3, 4]); // one pixel

    let parsed = parse_texm(&payload).expect("failed to parse minimal texm");
    assert_eq!(parsed.header.width, 1);
    assert_eq!(parsed.header.height, 1);
    assert_eq!(parsed.mip_levels.len(), 1);
    assert!(parsed.page_rects.is_empty());
}

#[test]
fn texm_parse_indexed_with_page_chunk() {
    let mut payload = Vec::new();
    payload.extend_from_slice(&TEXM_MAGIC.to_le_bytes());
    payload.extend_from_slice(&2u32.to_le_bytes()); // width
    payload.extend_from_slice(&2u32.to_le_bytes()); // height
    payload.extend_from_slice(&1u32.to_le_bytes()); // mip_count
    payload.extend_from_slice(&0u32.to_le_bytes()); // flags4
    payload.extend_from_slice(&0u32.to_le_bytes()); // flags5
    payload.extend_from_slice(&0u32.to_le_bytes()); // unk6
    payload.extend_from_slice(&0u32.to_le_bytes()); // format indexed8
    payload.extend_from_slice(&[0u8; 1024]); // palette
    payload.extend_from_slice(&[1, 2, 3, 4]); // pixels
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
