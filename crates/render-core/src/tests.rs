use super::*;
use msh_core::parse_model_payload;
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
            if !mesh.vertices.is_empty() {
                meshes_non_empty += 1;
            }
            if compute_bounds_for_mesh(&mesh.vertices).is_some() {
                bounds_non_empty += 1;
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
