use msh_core::{parse_model_payload, Model};
use nres::Archive;
use std::path::Path;

#[derive(Debug)]
pub enum Error {
    Nres(nres::error::Error),
    Msh(msh_core::error::Error),
    NoMshEntries,
    ModelNotFound(String),
}

impl From<nres::error::Error> for Error {
    fn from(value: nres::error::Error) -> Self {
        Self::Nres(value)
    }
}

impl From<msh_core::error::Error> for Error {
    fn from(value: msh_core::error::Error) -> Self {
        Self::Msh(value)
    }
}

pub type Result<T> = core::result::Result<T, Error>;

pub fn load_model_from_archive(path: &Path, model_name: Option<&str>) -> Result<Model> {
    let archive = Archive::open_path(path)?;
    let mut msh_entries = Vec::new();
    for entry in archive.entries() {
        if entry.meta.name.to_ascii_lowercase().ends_with(".msh") {
            msh_entries.push((entry.id, entry.meta.name.clone()));
        }
    }
    if msh_entries.is_empty() {
        return Err(Error::NoMshEntries);
    }

    let target_id = if let Some(name) = model_name {
        msh_entries
            .iter()
            .find(|(_, n)| n.eq_ignore_ascii_case(name))
            .map(|(id, _)| *id)
            .ok_or_else(|| Error::ModelNotFound(name.to_string()))?
    } else {
        msh_entries[0].0
    };

    let payload = archive.read(target_id)?;
    Ok(parse_model_payload(payload.as_slice())?)
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn archive_with_msh() -> Option<PathBuf> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("testdata");
        let mut files = Vec::new();
        collect_files_recursive(&root, &mut files);
        files.sort();
        for path in files {
            let Ok(bytes) = fs::read(&path) else {
                continue;
            };
            if bytes.get(0..4) != Some(b"NRes") {
                continue;
            }
            let Ok(archive) = Archive::open_path(&path) else {
                continue;
            };
            if archive
                .entries()
                .any(|entry| entry.meta.name.to_ascii_lowercase().ends_with(".msh"))
            {
                return Some(path);
            }
        }
        None
    }

    #[test]
    fn load_model_from_real_archive() {
        let Some(path) = archive_with_msh() else {
            eprintln!("skipping load_model_from_real_archive: no .msh archives in testdata");
            return;
        };
        let model = load_model_from_archive(&path, None)
            .unwrap_or_else(|err| panic!("failed to load model from {}: {err:?}", path.display()));
        assert!(model.node_count > 0);
        assert!(!model.positions.is_empty());
        assert!(!model.indices.is_empty());
    }
}
