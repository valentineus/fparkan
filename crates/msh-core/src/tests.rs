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

fn is_msh_name(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".msh")
}

#[test]
fn parse_all_game_msh_models() {
    let archives = nres_test_files();
    if archives.is_empty() {
        eprintln!("skipping parse_all_game_msh_models: no NRes files in testdata");
        return;
    }

    let mut model_count = 0usize;
    let mut renderable_count = 0usize;
    let mut legacy_stride24_count = 0usize;

    for archive_path in archives {
        let archive = Archive::open_path(&archive_path)
            .unwrap_or_else(|err| panic!("failed to open {}: {err}", archive_path.display()));

        for entry in archive.entries() {
            if !is_msh_name(&entry.meta.name) {
                continue;
            }
            model_count += 1;
            let payload = archive.read(entry.id).unwrap_or_else(|err| {
                panic!(
                    "failed to read model '{}' in {}: {err}",
                    entry.meta.name,
                    archive_path.display()
                )
            });
            let model = parse_model_payload(payload.as_slice()).unwrap_or_else(|err| {
                panic!(
                    "failed to parse model '{}' in {}: {err}",
                    entry.meta.name,
                    archive_path.display()
                )
            });

            if model.node_stride == 24 {
                legacy_stride24_count += 1;
            }

            for node_index in 0..model.node_count {
                for lod in 0..3 {
                    for group in 0..5 {
                        if let Some(slot_idx) = model.slot_index(node_index, lod, group) {
                            assert!(
                                slot_idx < model.slots.len(),
                                "slot index out of bounds in '{}' ({})",
                                entry.meta.name,
                                archive_path.display()
                            );
                        }
                    }
                }
            }

            let mut has_renderable_batch = false;
            for node_index in 0..model.node_count {
                let Some(slot_idx) = model.slot_index(node_index, 0, 0) else {
                    continue;
                };
                let slot = &model.slots[slot_idx];
                let batch_end =
                    usize::from(slot.batch_start).saturating_add(usize::from(slot.batch_count));
                if batch_end > model.batches.len() {
                    continue;
                }
                for batch in &model.batches[usize::from(slot.batch_start)..batch_end] {
                    let index_start = usize::try_from(batch.index_start).unwrap_or(usize::MAX);
                    let index_count = usize::from(batch.index_count);
                    let end = index_start.saturating_add(index_count);
                    if end <= model.indices.len() && index_count >= 3 {
                        has_renderable_batch = true;
                        break;
                    }
                }
                if has_renderable_batch {
                    break;
                }
            }
            if has_renderable_batch {
                renderable_count += 1;
            }
        }
    }

    assert!(model_count > 0, "no .msh entries found");
    assert!(
        renderable_count > 0,
        "no renderable models (lod0/group0) were detected"
    );
    assert!(
        legacy_stride24_count <= model_count,
        "internal test accounting error"
    );
}

#[test]
fn parse_minimal_synthetic_model() {
    // Nested NRes with required resources only.
    let mut payload = Vec::new();
    payload.extend_from_slice(b"NRes");
    payload.extend_from_slice(&0x100u32.to_le_bytes());
    payload.extend_from_slice(&5u32.to_le_bytes()); // entry_count
    payload.extend_from_slice(&0u32.to_le_bytes()); // total_size placeholder

    let mut resource_offsets = Vec::new();
    let mut resource_sizes = Vec::new();
    let mut resource_types = Vec::new();
    let mut resource_attr3 = Vec::new();
    let mut resource_names = Vec::new();

    let add_resource = |payload: &mut Vec<u8>,
                        offsets: &mut Vec<u32>,
                        sizes: &mut Vec<u32>,
                        types: &mut Vec<u32>,
                        attr3: &mut Vec<u32>,
                        names: &mut Vec<String>,
                        kind: u32,
                        name: &str,
                        data: &[u8],
                        attr3_val: u32| {
        offsets.push(u32::try_from(payload.len()).expect("offset overflow"));
        payload.extend_from_slice(data);
        while !payload.len().is_multiple_of(8) {
            payload.push(0);
        }
        sizes.push(u32::try_from(data.len()).expect("size overflow"));
        types.push(kind);
        attr3.push(attr3_val);
        names.push(name.to_string());
    };

    let node = {
        let mut b = vec![0u8; 38];
        // slot[0][0] = 0
        b[8..10].copy_from_slice(&0u16.to_le_bytes());
        for i in 1..15 {
            let off = 8 + i * 2;
            b[off..off + 2].copy_from_slice(&u16::MAX.to_le_bytes());
        }
        b
    };
    let mut res2 = vec![0u8; 0x8C + 68];
    res2[0x8C..0x8C + 2].copy_from_slice(&0u16.to_le_bytes()); // tri_start
    res2[0x8C + 2..0x8C + 4].copy_from_slice(&0u16.to_le_bytes()); // tri_count
    res2[0x8C + 4..0x8C + 6].copy_from_slice(&0u16.to_le_bytes()); // batch_start
    res2[0x8C + 6..0x8C + 8].copy_from_slice(&1u16.to_le_bytes()); // batch_count
    let positions = [0f32, 0f32, 0f32, 1f32, 0f32, 0f32, 0f32, 1f32, 0f32]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect::<Vec<_>>();
    let indices = [0u16, 1, 2]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect::<Vec<_>>();
    let batch = {
        let mut b = vec![0u8; 20];
        b[0..2].copy_from_slice(&0u16.to_le_bytes());
        b[2..4].copy_from_slice(&0u16.to_le_bytes());
        b[8..10].copy_from_slice(&3u16.to_le_bytes()); // index_count
        b[10..14].copy_from_slice(&0u32.to_le_bytes()); // index_start
        b[16..20].copy_from_slice(&0u32.to_le_bytes()); // base_vertex
        b
    };

    add_resource(
        &mut payload,
        &mut resource_offsets,
        &mut resource_sizes,
        &mut resource_types,
        &mut resource_attr3,
        &mut resource_names,
        RES1_NODE_TABLE,
        "Res1",
        &node,
        38,
    );
    add_resource(
        &mut payload,
        &mut resource_offsets,
        &mut resource_sizes,
        &mut resource_types,
        &mut resource_attr3,
        &mut resource_names,
        RES2_SLOTS,
        "Res2",
        &res2,
        68,
    );
    add_resource(
        &mut payload,
        &mut resource_offsets,
        &mut resource_sizes,
        &mut resource_types,
        &mut resource_attr3,
        &mut resource_names,
        RES3_POSITIONS,
        "Res3",
        &positions,
        12,
    );
    add_resource(
        &mut payload,
        &mut resource_offsets,
        &mut resource_sizes,
        &mut resource_types,
        &mut resource_attr3,
        &mut resource_names,
        RES6_INDICES,
        "Res6",
        &indices,
        2,
    );
    add_resource(
        &mut payload,
        &mut resource_offsets,
        &mut resource_sizes,
        &mut resource_types,
        &mut resource_attr3,
        &mut resource_names,
        RES13_BATCHES,
        "Res13",
        &batch,
        20,
    );

    let directory_offset = payload.len();
    for i in 0..resource_types.len() {
        payload.extend_from_slice(&resource_types[i].to_le_bytes());
        payload.extend_from_slice(&1u32.to_le_bytes()); // attr1
        payload.extend_from_slice(&0u32.to_le_bytes()); // attr2
        payload.extend_from_slice(&resource_sizes[i].to_le_bytes());
        payload.extend_from_slice(&resource_attr3[i].to_le_bytes());
        let mut name_raw = [0u8; 36];
        let bytes = resource_names[i].as_bytes();
        name_raw[..bytes.len()].copy_from_slice(bytes);
        payload.extend_from_slice(&name_raw);
        payload.extend_from_slice(&resource_offsets[i].to_le_bytes());
        payload.extend_from_slice(&(i as u32).to_le_bytes()); // sort index
    }
    let total_size = u32::try_from(payload.len()).expect("size overflow");
    payload[12..16].copy_from_slice(&total_size.to_le_bytes());
    assert_eq!(
        directory_offset + resource_types.len() * 64,
        payload.len(),
        "synthetic nested NRes layout invalid"
    );

    let model = parse_model_payload(&payload).expect("failed to parse synthetic model");
    assert_eq!(model.node_count, 1);
    assert_eq!(model.positions.len(), 3);
    assert_eq!(model.indices.len(), 3);
    assert_eq!(model.batches.len(), 1);
    assert_eq!(model.slot_index(0, 0, 0), Some(0));
}
