use super::*;
use common::collect_files_recursive;
use nres::Archive;
use proptest::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};

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

#[derive(Clone)]
struct SyntheticEntry {
    kind: u32,
    name: String,
    attr1: u32,
    attr2: u32,
    attr3: u32,
    data: Vec<u8>,
}

fn build_nested_nres(entries: &[SyntheticEntry]) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(b"NRes");
    payload.extend_from_slice(&0x100u32.to_le_bytes());
    payload.extend_from_slice(
        &u32::try_from(entries.len())
            .expect("entry count overflow in test")
            .to_le_bytes(),
    );
    payload.extend_from_slice(&0u32.to_le_bytes()); // total_size placeholder

    let mut resource_offsets = Vec::with_capacity(entries.len());
    for entry in entries {
        resource_offsets.push(u32::try_from(payload.len()).expect("offset overflow in test"));
        payload.extend_from_slice(&entry.data);
        while !payload.len().is_multiple_of(8) {
            payload.push(0);
        }
    }

    for (index, entry) in entries.iter().enumerate() {
        payload.extend_from_slice(&entry.kind.to_le_bytes());
        payload.extend_from_slice(&entry.attr1.to_le_bytes());
        payload.extend_from_slice(&entry.attr2.to_le_bytes());
        payload.extend_from_slice(
            &u32::try_from(entry.data.len())
                .expect("size overflow in test")
                .to_le_bytes(),
        );
        payload.extend_from_slice(&entry.attr3.to_le_bytes());

        let mut name_raw = [0u8; 36];
        let name_bytes = entry.name.as_bytes();
        assert!(name_bytes.len() <= 35, "name too long for synthetic test");
        name_raw[..name_bytes.len()].copy_from_slice(name_bytes);
        payload.extend_from_slice(&name_raw);

        payload.extend_from_slice(&resource_offsets[index].to_le_bytes());
        payload.extend_from_slice(&(index as u32).to_le_bytes());
    }

    let total_size = u32::try_from(payload.len()).expect("size overflow in test");
    payload[12..16].copy_from_slice(&total_size.to_le_bytes());
    payload
}

fn synthetic_entry(kind: u32, name: &str, attr3: u32, data: Vec<u8>) -> SyntheticEntry {
    SyntheticEntry {
        kind,
        name: name.to_string(),
        attr1: 1,
        attr2: 0,
        attr3,
        data,
    }
}

fn res1_stride38_nodes(node_count: usize, node0_slot00: Option<u16>) -> Vec<u8> {
    let mut out = vec![0u8; node_count.saturating_mul(38)];
    for node in 0..node_count {
        let node_off = node * 38;
        for i in 0..15 {
            let off = node_off + 8 + i * 2;
            out[off..off + 2].copy_from_slice(&u16::MAX.to_le_bytes());
        }
    }
    if let Some(slot) = node0_slot00 {
        out[8..10].copy_from_slice(&slot.to_le_bytes());
    }
    out
}

fn res1_stride24_nodes(node_count: usize) -> Vec<u8> {
    vec![0u8; node_count.saturating_mul(24)]
}

fn res2_single_slot(batch_start: u16, batch_count: u16) -> Vec<u8> {
    let mut res2 = vec![0u8; 0x8C + 68];
    res2[0x8C..0x8C + 2].copy_from_slice(&0u16.to_le_bytes()); // tri_start
    res2[0x8C + 2..0x8C + 4].copy_from_slice(&0u16.to_le_bytes()); // tri_count
    res2[0x8C + 4..0x8C + 6].copy_from_slice(&batch_start.to_le_bytes()); // batch_start
    res2[0x8C + 6..0x8C + 8].copy_from_slice(&batch_count.to_le_bytes()); // batch_count
    res2
}

fn res3_triangle_positions() -> Vec<u8> {
    [0f32, 0f32, 0f32, 1f32, 0f32, 0f32, 0f32, 1f32, 0f32]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect()
}

fn res4_normals() -> Vec<u8> {
    vec![127u8, 0u8, 128u8, 0u8]
}

fn res5_uv0() -> Vec<u8> {
    [1024i16, -1024i16]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect()
}

fn res6_triangle_indices() -> Vec<u8> {
    [0u16, 1u16, 2u16]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect()
}

fn res13_single_batch(index_start: u32, index_count: u16) -> Vec<u8> {
    let mut batch = vec![0u8; 20];
    batch[0..2].copy_from_slice(&0u16.to_le_bytes());
    batch[2..4].copy_from_slice(&0u16.to_le_bytes());
    batch[8..10].copy_from_slice(&index_count.to_le_bytes());
    batch[10..14].copy_from_slice(&index_start.to_le_bytes());
    batch[16..20].copy_from_slice(&0u32.to_le_bytes());
    batch
}

fn res10_names_raw(names: &[Option<&[u8]>]) -> Vec<u8> {
    let mut out = Vec::new();
    for name in names {
        match name {
            Some(name) => {
                out.extend_from_slice(
                    &u32::try_from(name.len())
                        .expect("name size overflow in test")
                        .to_le_bytes(),
                );
                out.extend_from_slice(name);
                out.push(0);
            }
            None => out.extend_from_slice(&0u32.to_le_bytes()),
        }
    }
    out
}

fn res10_names(names: &[Option<&str>]) -> Vec<u8> {
    let raw: Vec<Option<&[u8]>> = names.iter().map(|name| name.map(str::as_bytes)).collect();
    res10_names_raw(&raw)
}

fn base_synthetic_entries() -> Vec<SyntheticEntry> {
    vec![
        synthetic_entry(RES1_NODE_TABLE, "Res1", 38, res1_stride38_nodes(1, Some(0))),
        synthetic_entry(RES2_SLOTS, "Res2", 68, res2_single_slot(0, 1)),
        synthetic_entry(RES3_POSITIONS, "Res3", 12, res3_triangle_positions()),
        synthetic_entry(RES6_INDICES, "Res6", 2, res6_triangle_indices()),
        synthetic_entry(RES13_BATCHES, "Res13", 20, res13_single_batch(0, 3)),
    ]
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
    let payload = build_nested_nres(&base_synthetic_entries());
    let model = parse_model_payload(&payload).expect("failed to parse synthetic model");
    assert_eq!(model.node_count, 1);
    assert_eq!(model.positions.len(), 3);
    assert_eq!(model.indices.len(), 3);
    assert_eq!(model.batches.len(), 1);
    assert_eq!(model.slot_index(0, 0, 0), Some(0));
}

#[test]
fn parse_synthetic_stride24_variant() {
    let mut entries = base_synthetic_entries();
    entries[0] = synthetic_entry(RES1_NODE_TABLE, "Res1", 24, res1_stride24_nodes(1));
    let payload = build_nested_nres(&entries);

    let model = parse_model_payload(&payload).expect("failed to parse stride24 model");
    assert_eq!(model.node_stride, 24);
    assert_eq!(model.node_count, 1);
    assert_eq!(model.slot_index(0, 0, 0), None);
}

#[test]
fn parse_synthetic_model_with_optional_res4_res5_res10() {
    let mut entries = base_synthetic_entries();
    entries.push(synthetic_entry(RES4_NORMALS, "Res4", 4, res4_normals()));
    entries.push(synthetic_entry(RES5_UV0, "Res5", 4, res5_uv0()));
    entries.push(synthetic_entry(
        RES10_NAMES,
        "Res10",
        1,
        res10_names(&[Some("Hull"), None]),
    ));
    entries[0] = synthetic_entry(RES1_NODE_TABLE, "Res1", 38, res1_stride38_nodes(2, Some(0)));
    let payload = build_nested_nres(&entries);

    let model = parse_model_payload(&payload).expect("failed to parse model with optional data");
    assert_eq!(model.node_count, 2);
    assert_eq!(model.normals.as_ref().map(Vec::len), Some(1));
    assert_eq!(model.uv0.as_ref().map(Vec::len), Some(1));
    assert_eq!(model.node_names, Some(vec![Some("Hull".to_string()), None]));
}

#[test]
fn parse_res10_names_decodes_cp1251() {
    let mut entries = base_synthetic_entries();
    entries[0] = synthetic_entry(RES1_NODE_TABLE, "Res1", 38, res1_stride38_nodes(1, Some(0)));
    entries.push(synthetic_entry(
        RES10_NAMES,
        "Res10",
        1,
        res10_names_raw(&[Some(&[0xC0])]),
    ));
    let payload = build_nested_nres(&entries);

    let model = parse_model_payload(&payload).expect("failed to parse model with cp1251 name");
    assert_eq!(model.node_names, Some(vec![Some("А".to_string())]));
}

#[test]
fn parse_fails_when_required_resource_missing() {
    let mut entries = base_synthetic_entries();
    entries.retain(|entry| entry.kind != RES13_BATCHES);
    let payload = build_nested_nres(&entries);

    assert!(matches!(
        parse_model_payload(&payload),
        Err(Error::MissingResource {
            kind: RES13_BATCHES,
            label: "Res13"
        })
    ));
}

#[test]
fn parse_fails_for_invalid_res2_size() {
    let mut entries = base_synthetic_entries();
    entries[1] = synthetic_entry(RES2_SLOTS, "Res2", 68, vec![0u8; 0x8B]);
    let payload = build_nested_nres(&entries);

    assert!(matches!(
        parse_model_payload(&payload),
        Err(Error::InvalidRes2Size { .. })
    ));
}

#[test]
fn parse_fails_for_unsupported_node_stride() {
    let mut entries = base_synthetic_entries();
    entries[0] = synthetic_entry(RES1_NODE_TABLE, "Res1", 30, vec![0u8; 30]);
    let payload = build_nested_nres(&entries);

    assert!(matches!(
        parse_model_payload(&payload),
        Err(Error::UnsupportedNodeStride { stride: 30 })
    ));
}

#[test]
fn parse_fails_for_invalid_optional_resource_size() {
    let mut entries = base_synthetic_entries();
    entries.push(synthetic_entry(RES4_NORMALS, "Res4", 4, vec![1, 2, 3]));
    let payload = build_nested_nres(&entries);

    assert!(matches!(
        parse_model_payload(&payload),
        Err(Error::InvalidResourceSize { label: "Res4", .. })
    ));
}

#[test]
fn parse_fails_for_slot_batch_range_out_of_bounds() {
    let mut entries = base_synthetic_entries();
    entries[1] = synthetic_entry(RES2_SLOTS, "Res2", 68, res2_single_slot(0, 2));
    let payload = build_nested_nres(&entries);

    assert!(matches!(
        parse_model_payload(&payload),
        Err(Error::IndexOutOfBounds {
            label: "Res2.batch_range",
            ..
        })
    ));
}

#[test]
fn parse_fails_for_batch_index_range_out_of_bounds() {
    let mut entries = base_synthetic_entries();
    entries[4] = synthetic_entry(RES13_BATCHES, "Res13", 20, res13_single_batch(1, 3));
    let payload = build_nested_nres(&entries);

    assert!(matches!(
        parse_model_payload(&payload),
        Err(Error::IndexOutOfBounds {
            label: "Res13.index_range",
            ..
        })
    ));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn parse_model_payload_never_panics_on_random_bytes(data in proptest::collection::vec(any::<u8>(), 0..8192)) {
        let _ = parse_model_payload(&data);
    }
}
