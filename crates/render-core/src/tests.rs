use super::*;
use common::collect_files_recursive;
use msh_core::parse_model_payload;
use nres::Archive;
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

#[test]
fn build_render_mesh_for_real_models() {
    let archives = nres_test_files();
    if archives.is_empty() {
        eprintln!("skipping build_render_mesh_for_real_models: no NRes files in testdata");
        return;
    }

    let mut models_checked = 0usize;
    let mut meshes_non_empty = 0usize;
    let mut bounds_non_empty = 0usize;

    for archive_path in archives {
        let archive = Archive::open_path(&archive_path)
            .unwrap_or_else(|err| panic!("failed to open {}: {err}", archive_path.display()));
        for entry in archive.entries() {
            if !entry.meta.name.to_ascii_lowercase().ends_with(".msh") {
                continue;
            }
            models_checked += 1;
            let payload = archive.read(entry.id).unwrap_or_else(|err| {
                panic!(
                    "failed to read model '{}' from {}: {err}",
                    entry.meta.name,
                    archive_path.display()
                )
            });
            let model = parse_model_payload(payload.as_slice()).unwrap_or_else(|err| {
                panic!(
                    "failed to parse model '{}' from {}: {err}",
                    entry.meta.name,
                    archive_path.display()
                )
            });
            let mesh = build_render_mesh(&model, 0, 0);
            if !mesh.indices.is_empty() {
                meshes_non_empty += 1;
            }
            if compute_bounds_for_mesh(&mesh.vertices).is_some() {
                bounds_non_empty += 1;
            }
            for &index in &mesh.indices {
                assert!(
                    usize::from(index) < mesh.vertices.len(),
                    "index out of bounds for '{}' in {}",
                    entry.meta.name,
                    archive_path.display()
                );
            }
            for vertex in &mesh.vertices {
                assert!(
                    vertex.uv0[0].is_finite() && vertex.uv0[1].is_finite(),
                    "UV must be finite for '{}' in {}",
                    entry.meta.name,
                    archive_path.display()
                );
            }
        }
    }

    assert!(models_checked > 0, "no MSH models found");
    assert!(
        meshes_non_empty > 0,
        "all generated render meshes are empty"
    );
    assert_eq!(
        meshes_non_empty, bounds_non_empty,
        "bounds must be available for every non-empty mesh"
    );
}

#[test]
fn compute_bounds_handles_empty_and_non_empty() {
    assert!(compute_bounds(&[]).is_none());
    let bounds = compute_bounds(&[[1.0, 2.0, 3.0], [-2.0, 5.0, 0.5], [0.0, -1.0, 9.0]])
        .expect("bounds expected");
    assert_eq!(bounds.0, [-2.0, -1.0, 0.5]);
    assert_eq!(bounds.1, [1.0, 5.0, 9.0]);
}

#[test]
fn compute_bounds_for_mesh_handles_empty_and_non_empty() {
    assert!(compute_bounds_for_mesh(&[]).is_none());
    let bounds = compute_bounds_for_mesh(&[
        RenderVertex {
            position: [1.0, 2.0, 3.0],
            uv0: [0.0, 0.0],
        },
        RenderVertex {
            position: [-2.0, 5.0, 0.5],
            uv0: [0.2, 0.3],
        },
        RenderVertex {
            position: [0.0, -1.0, 9.0],
            uv0: [1.0, 1.0],
        },
    ])
    .expect("bounds expected");
    assert_eq!(bounds.0, [-2.0, -1.0, 0.5]);
    assert_eq!(bounds.1, [1.0, 5.0, 9.0]);
}

fn nodes_with_slot_refs(slot_ids: &[Option<u16>]) -> Vec<u8> {
    let mut out = vec![0u8; slot_ids.len().saturating_mul(38)];
    for (node_index, slot_id) in slot_ids.iter().copied().enumerate() {
        let node_off = node_index * 38;
        for i in 0..15 {
            let off = node_off + 8 + i * 2;
            out[off..off + 2].copy_from_slice(&u16::MAX.to_le_bytes());
        }
        if let Some(slot_id) = slot_id {
            out[node_off + 8..node_off + 10].copy_from_slice(&slot_id.to_le_bytes());
        }
    }
    out
}

fn slot(batch_start: u16, batch_count: u16) -> msh_core::Slot {
    msh_core::Slot {
        tri_start: 0,
        tri_count: 0,
        batch_start,
        batch_count,
        aabb_min: [0.0; 3],
        aabb_max: [0.0; 3],
        sphere_center: [0.0; 3],
        sphere_radius: 0.0,
        opaque: [0; 5],
    }
}

fn batch(index_start: u32, index_count: u16, base_vertex: u32) -> msh_core::Batch {
    msh_core::Batch {
        batch_flags: 0,
        material_index: 0,
        opaque4: 0,
        opaque6: 0,
        index_count,
        index_start,
        opaque14: 0,
        base_vertex,
    }
}

#[test]
fn build_render_mesh_handles_empty_slot_model() {
    let model = msh_core::Model {
        node_stride: 38,
        node_count: 1,
        nodes_raw: nodes_with_slot_refs(&[None]),
        slots: Vec::new(),
        positions: vec![[0.0, 0.0, 0.0]],
        normals: None,
        uv0: None,
        indices: Vec::new(),
        batches: Vec::new(),
        node_names: None,
    };

    let mesh = build_render_mesh(&model, 0, 0);
    assert!(mesh.vertices.is_empty());
    assert!(mesh.indices.is_empty());
    assert_eq!(mesh.batch_count, 0);
    assert_eq!(mesh.triangle_count(), 0);
}

#[test]
fn build_render_mesh_supports_multi_node_and_uv_scaling() {
    let model = msh_core::Model {
        node_stride: 38,
        node_count: 2,
        nodes_raw: nodes_with_slot_refs(&[Some(0), Some(1)]),
        slots: vec![slot(0, 1), slot(1, 1)],
        positions: vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [2.0, 0.0, 0.0],
            [3.0, 0.0, 0.0],
            [2.0, 1.0, 0.0],
        ],
        normals: None,
        uv0: Some(vec![
            [1024, -1024],
            [512, 256],
            [0, 0],
            [1024, 1024],
            [2048, 1024],
            [1024, 0],
        ]),
        indices: vec![0, 1, 2, 0, 1, 2],
        batches: vec![batch(0, 3, 0), batch(3, 3, 3)],
        node_names: None,
    };

    let mesh = build_render_mesh(&model, 0, 0);
    assert_eq!(mesh.batch_count, 2);
    assert_eq!(mesh.vertices.len(), 6);
    assert_eq!(mesh.indices, vec![0, 1, 2, 3, 4, 5]);
    assert_eq!(mesh.triangle_count(), 2);
    assert_eq!(mesh.vertices[0].uv0, [1.0, -1.0]);
    assert_eq!(mesh.vertices[1].uv0, [0.5, 0.25]);
    assert_eq!(mesh.vertices[2].uv0, [0.0, 0.0]);
    assert_eq!(mesh.vertices[3].uv0, [1.0, 1.0]);
}

#[test]
fn build_render_mesh_deduplicates_shared_vertices() {
    let model = msh_core::Model {
        node_stride: 38,
        node_count: 1,
        nodes_raw: nodes_with_slot_refs(&[Some(0)]),
        slots: vec![slot(0, 1)],
        positions: vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [1.0, 1.0, 0.0],
        ],
        normals: None,
        uv0: None,
        indices: vec![0, 1, 2, 2, 1, 3],
        batches: vec![batch(0, 6, 0)],
        node_names: None,
    };

    let mesh = build_render_mesh(&model, 0, 0);
    assert_eq!(mesh.vertices.len(), 4);
    assert_eq!(mesh.indices, vec![0, 1, 2, 2, 1, 3]);
    assert_eq!(mesh.triangle_count(), 2);
}
