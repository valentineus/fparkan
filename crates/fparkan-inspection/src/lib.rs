#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        clippy::cast_precision_loss,
        clippy::expect_used,
        clippy::float_cmp,
        clippy::identity_op,
        clippy::too_many_lines,
        clippy::uninlined_format_args,
        clippy::map_unwrap_or,
        clippy::needless_raw_string_hashes,
        clippy::semicolon_if_nothing_returned,
        clippy::type_complexity,
        clippy::panic,
        clippy::unwrap_used
    )
)]
//! Shared inspection helpers for format-backed tooling.

use fparkan_msh::{decode_msh, validate_msh};
use fparkan_nres::{decode as decode_nres, NresDocument, ReadProfile};
use fparkan_resource::{archive_path, resource_name, CachedResourceRepository, ResourceRepository};
use fparkan_rsli::decode as decode_rsli;
use fparkan_terrain_format::{decode_land_map, decode_land_msh};
use fparkan_texm::decode_texm;
use fparkan_vfs::DirectoryVfs;
use std::fs;
use std::path::Path;
use std::sync::Arc;

/// Archive inspection variants.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ArchiveInspection {
    /// `NRes` inspection summary.
    Nres {
        /// Archive entry count.
        entries: usize,
        /// Lookup order validity.
        lookup_order_valid: bool,
        /// Entry samples (subject to request limit).
        sample: Vec<NresEntrySummary>,
    },
    /// `RsLi` inspection summary.
    Rsli {
        /// Archive entry count.
        entries: usize,
    },
    /// Unknown/unsupported archive magic.
    Unsupported,
}

/// `NRes` entry summary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NresEntrySummary {
    /// ASCII/legacy resource name.
    pub name: String,
    /// Entry type identifier.
    pub type_id: u32,
    /// Declared entry payload size.
    pub data_size: u32,
}

/// Model inspection payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelInspection {
    /// Terrain stream/document stream count.
    pub streams: usize,
    /// Node count.
    pub nodes: usize,
    /// Slot count.
    pub slots: usize,
    /// Position count.
    pub positions: usize,
    /// Index count.
    pub indices: usize,
    /// Batch count.
    pub batches: usize,
}

/// Texture inspection payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextureInspection {
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
    /// Texture format debug text.
    pub format: String,
    /// Mip level count.
    pub mips: usize,
    /// Total page rectangles.
    pub pages: usize,
}

/// Land map/msh inspection payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MapInspection {
    /// Mapped mesh stream count.
    pub streams: usize,
    /// Slot count.
    pub slots: usize,
    /// Position count.
    pub positions: usize,
    /// Face count.
    pub faces: usize,
    /// Terrain areals.
    pub areals: usize,
    /// Declared areal count from map metadata.
    pub declared_areals: u32,
    /// Map grid width.
    pub grid_width: u32,
    /// Map grid height.
    pub grid_height: u32,
}

/// Supported land file kinds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LandFileKind {
    /// `land.msh` payload.
    LandMsh,
    /// `land.map` payload.
    LandMap,
}

/// Inspects a format archive.
///
/// # Errors
///
/// Returns a string error when the archive cannot be read or decoded.
pub fn inspect_archive_file(path: &Path, sample_limit: usize) -> Result<ArchiveInspection, String> {
    let bytes = fs::read(path).map_err(|err| format!("{}: {err}", path.display()))?;
    inspect_archive_bytes(&bytes, sample_limit, Some(path))
}

/// Inspects archive bytes and returns a typed summary.
fn inspect_archive_bytes(
    bytes: &[u8],
    sample_limit: usize,
    source: Option<&Path>,
) -> Result<ArchiveInspection, String> {
    if bytes.starts_with(b"NRes") {
        let document = decode_nres(
            Arc::from(bytes.to_vec().into_boxed_slice()),
            ReadProfile::Compatible,
        )
        .map_err(|err| err.to_string())?;
        let mut sample = Vec::new();
        for entry in document.entries().iter().take(sample_limit) {
            sample.push(NresEntrySummary {
                name: String::from_utf8_lossy(entry.name_bytes()).to_string(),
                type_id: entry.meta().type_id,
                data_size: entry.meta().data_size,
            });
        }
        Ok(ArchiveInspection::Nres {
            entries: document.entries().len(),
            lookup_order_valid: document.lookup_order_valid(),
            sample,
        })
    } else if bytes.get(0..4) == Some(b"NL\0\x01") {
        let document = decode_rsli(
            Arc::from(bytes.to_vec().into_boxed_slice()),
            fparkan_rsli::ReadProfile::Compatible,
        )
        .map_err(|err| err.to_string())?;
        Ok(ArchiveInspection::Rsli {
            entries: document.entries().len(),
        })
    } else {
        match source {
            Some(path) => Err(format!("{}: unsupported archive magic", path.display())),
            None => Err("unsupported archive magic".to_string()),
        }
    }
}

/// Inspects a model through repository-backed resource lookup.
///
/// # Errors
///
/// Returns a string error when the resource cannot be resolved or parsed as a
/// valid model payload.
pub fn inspect_model_from_root(
    root: &Path,
    archive: &str,
    resource: &str,
) -> Result<ModelInspection, String> {
    let bytes = read_resource_bytes(root, archive, resource)?;
    let document = decode_nres(bytes, ReadProfile::Compatible).map_err(|err| err.to_string())?;
    let msh = decode_msh(&document).map_err(|err| err.to_string())?;
    let validated = validate_msh(&msh).map_err(|err| err.to_string())?;
    Ok(ModelInspection {
        streams: msh.streams().len(),
        nodes: validated.node_count,
        slots: validated.slots.len(),
        positions: validated.positions.len(),
        indices: validated.indices.len(),
        batches: validated.batches.len(),
    })
}

/// Inspects a texture through repository-backed resource lookup.
///
/// # Errors
///
/// Returns a string error when the resource cannot be resolved or parsed as a
/// valid texture payload.
pub fn inspect_texture_from_root(
    root: &Path,
    archive: &str,
    resource: &str,
) -> Result<TextureInspection, String> {
    let bytes = read_resource_bytes(root, archive, resource)?;
    let document = decode_texm(bytes).map_err(|err| err.to_string())?;
    Ok(TextureInspection {
        width: document.width(),
        height: document.height(),
        format: format!("{:?}", document.format()),
        mips: document.mip_count(),
        pages: document.page_rects().len(),
    })
}

/// Inspects a terrain land file by path.
///
/// # Errors
///
/// Returns a string error when the file cannot be read or parsed as the
/// requested terrain payload kind.
pub fn inspect_land_file(path: &Path, kind: LandFileKind) -> Result<MapInspection, String> {
    let bytes = fs::read(path).map_err(|err| format!("{}: {err}", path.display()))?;
    let document = decode_nres(Arc::from(bytes.into_boxed_slice()), ReadProfile::Compatible)
        .map_err(|err| err.to_string())?;
    match kind {
        LandFileKind::LandMsh => inspect_land_msh(&document),
        LandFileKind::LandMap => inspect_land_map(&document),
    }
}

fn inspect_land_msh(document: &NresDocument) -> Result<MapInspection, String> {
    let land_msh = decode_land_msh(document).map_err(|err| err.to_string())?;
    Ok(MapInspection {
        streams: land_msh.streams.len(),
        slots: land_msh.slots.slots_raw.len(),
        positions: land_msh.positions.len(),
        faces: land_msh.faces.len(),
        areals: 0,
        declared_areals: 0,
        grid_width: 0,
        grid_height: 0,
    })
}

fn inspect_land_map(document: &NresDocument) -> Result<MapInspection, String> {
    let land_map = decode_land_map(document).map_err(|err| err.to_string())?;
    Ok(MapInspection {
        streams: 0,
        slots: 0,
        positions: 0,
        faces: 0,
        areals: land_map.areals.len(),
        declared_areals: land_map.areal_count,
        grid_width: land_map.grid.cells_x,
        grid_height: land_map.grid.cells_y,
    })
}

fn read_resource_bytes(root: &Path, archive: &str, name: &str) -> Result<Arc<[u8]>, String> {
    let repository = CachedResourceRepository::new(Arc::new(DirectoryVfs::new(root)));
    let archive_path = archive_path(archive.as_bytes()).map_err(|err| err.to_string())?;
    let resource_name = resource_name(name.as_bytes());
    let archive_handle = repository
        .open_archive(&archive_path)
        .map_err(|err| format!("{err}"))?;
    let Some(handle) = repository
        .find(archive_handle, &resource_name)
        .map_err(|err| format!("{err}"))?
    else {
        return Err(format!(
            "resource not found: {archive}/{}",
            String::from_utf8_lossy(name.as_bytes())
        ));
    };
    let bytes = repository.read(handle).map_err(|err| format!("{err}"))?;
    Ok(Arc::from(bytes.into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::path::PathBuf;

    #[test]
    fn inspect_rsli_rejects_malformed_archive() {
        let dir = temp_dir("inspect");
        let path = dir.join("test.rsli");
        let mut file = fs::File::create(&path).expect("file");
        file.write_all(b"NL\0\x01").expect("magic");
        drop(file);

        let error = inspect_archive_file(&path, 0).expect_err("malformed archive");
        assert!(error.contains("entry table out of bounds"));
    }

    #[test]
    fn nres_entry_summary_fields_are_readable() {
        let dir = temp_dir("inspect-nres");
        let archive = dir.join("test.nres");
        let payload = Vec::from("NRes\x00\x00\x00\x00");
        fs::write(&archive, &payload).expect("nres");

        let _ = inspect_archive_file(&archive, 2);
    }

    fn temp_dir(name: &str) -> PathBuf {
        let base = PathBuf::from("/tmp")
            .join("fparkan-inspection-tests")
            .join(name);
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).expect("tmp dir");
        base
    }
}
