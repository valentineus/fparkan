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

use fparkan_diagnostics::{
    diagnostic, render_human, Diagnostic, DiagnosticCode, DiagnosticContext, Phase, SourceSpan,
};
use fparkan_material::{decode_wear, resolve_material, MaterialFallback};
use fparkan_msh::{
    decode_msh, node38_metadata, selected_slot, validate_msh, Group, Lod, ModelAsset, NodeId,
};
use fparkan_nres::{decode as decode_nres, NresDocument, ReadProfile};
use fparkan_path::{normalize_relative, PathPolicy};
use fparkan_resource::{archive_path, resource_name, CachedResourceRepository, ResourceRepository};
use fparkan_rsli::decode as decode_rsli;
use fparkan_terrain_format::{decode_land_map, decode_land_msh, LandMeshDocument};
use fparkan_texm::decode_texm;
use fparkan_vfs::{DirectoryVfs, Vfs};
use std::fs;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
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
    /// Original node record stride.
    pub node_stride: usize,
    /// Standard-node metadata in source order, when the model uses `Node38`.
    pub node38: Vec<Node38Inspection>,
    /// Number of decoded type-8 animation keys, when available.
    pub animation_keys: Option<usize>,
    /// Declared type-19 animation frame count, when available.
    pub animation_frame_count: Option<u32>,
}

/// Inspection view of a standard 38-byte model node.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Node38Inspection {
    /// Source node index.
    pub index: usize,
    /// Opaque source field at byte offset two.
    pub parent_or_link_raw: u16,
    /// Type-19 frame-map offset, or `0xFFFF`.
    pub anim_map_start: u16,
    /// Type-8 fallback key index.
    pub fallback_key: u16,
    /// Whether LOD zero/group zero selects a geometry slot.
    pub has_lod0_group0: bool,
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

/// Diffuse TEXM selected through an original WEAR and MAT0 material chain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WearMaterialTexture {
    /// Source WEAR archive.
    pub wear_archive: String,
    /// Source WEAR resource.
    pub wear_resource: String,
    /// Positional WEAR selector used by an MSH batch.
    pub material_index: u16,
    /// Resolved MAT0 resource name after the original fallback chain.
    pub material_name: String,
    /// Fallback route used while resolving the MAT0 resource.
    pub material_fallback: MaterialFallback,
    /// Texture name selected from phase zero of the resolved MAT0 document.
    pub texture_name: String,
    /// Decoded RGBA8 mip zero suitable for the Vulkan upload boundary.
    pub image: fparkan_texm::RgbaImage,
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

/// Axis-aligned position bounds of a decoded `Land.msh`.
#[derive(Clone, Debug, PartialEq)]
pub struct LandMeshBoundsInspection {
    /// Number of source positions covered by the bounds.
    pub positions: usize,
    /// Per-axis inclusive minimum position.
    pub min: [f32; 3],
    /// Per-axis inclusive maximum position.
    pub max: [f32; 3],
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
    inspect_archive_file_diagnostic(path, sample_limit)
        .map_err(|diagnostic| render_human(&diagnostic))
}

/// Inspects a format archive and returns a structured diagnostic on failure.
///
/// # Errors
///
/// Returns a [`Diagnostic`] when the archive cannot be read or decoded.
// Diagnostic is deliberately returned by value as the public structured-error contract.
#[allow(clippy::result_large_err)]
pub fn inspect_archive_file_diagnostic(
    path: &Path,
    sample_limit: usize,
) -> Result<ArchiveInspection, Diagnostic> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path.file_name().ok_or_else(|| {
        diagnostic(
            DiagnosticCode("S1.VFS.PATH"),
            format!("{}: archive path has no file name", path.display()),
        )
        .with_context(DiagnosticContext {
            phase: Some(Phase::Read),
            path: Some(path.display().to_string()),
            ..DiagnosticContext::default()
        })
    })?;
    #[cfg(unix)]
    let raw_name = file_name.as_bytes();
    #[cfg(not(unix))]
    let raw_name = file_name
        .to_str()
        .ok_or_else(|| {
            diagnostic(
                DiagnosticCode("S1.VFS.PATH"),
                format!("{}: archive file name is not valid text", path.display()),
            )
            .with_context(DiagnosticContext {
                phase: Some(Phase::Read),
                path: Some(path.display().to_string()),
                ..DiagnosticContext::default()
            })
        })?
        .as_bytes();
    let normalized = normalize_relative(raw_name, PathPolicy::HostCompatible).map_err(|err| {
        diagnostic(
            DiagnosticCode("S1.VFS.PATH"),
            format!("{}: {err}", path.display()),
        )
        .with_context(DiagnosticContext {
            phase: Some(Phase::Read),
            path: Some(path.display().to_string()),
            ..DiagnosticContext::default()
        })
    })?;
    let vfs = DirectoryVfs::new(parent);
    let bytes = vfs.read(&normalized).map_err(|err| {
        diagnostic(
            DiagnosticCode("S1.VFS.READ"),
            format!("{}: {err}", path.display()),
        )
        .with_context(DiagnosticContext {
            phase: Some(Phase::Read),
            path: Some(path.display().to_string()),
            ..DiagnosticContext::default()
        })
    })?;
    inspect_archive_bytes(&bytes, sample_limit, Some(path))
}

/// Inspects archive bytes and returns a typed summary.
// Keeps the internal diagnostic flow aligned with the public structured-error contract.
#[allow(clippy::result_large_err)]
fn inspect_archive_bytes(
    bytes: &[u8],
    sample_limit: usize,
    source: Option<&Path>,
) -> Result<ArchiveInspection, Diagnostic> {
    if bytes.starts_with(b"NRes") {
        let document = decode_nres(
            Arc::from(bytes.to_vec().into_boxed_slice()),
            ReadProfile::Compatible,
        )
        .map_err(|err| {
            archive_parse_diagnostic("S1.NRES.DECODE", source, bytes, err.to_string())
        })?;
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
        .map_err(|err| {
            archive_parse_diagnostic("S1.RSLI.DECODE", source, bytes, err.to_string())
        })?;
        Ok(ArchiveInspection::Rsli {
            entries: document.entries().len(),
        })
    } else {
        Err(archive_parse_diagnostic(
            "S1.RESOURCE.UNSUPPORTED_ARCHIVE",
            source,
            bytes,
            "unsupported archive magic".to_string(),
        ))
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
    let bytes = read_resource_bytes_diagnostic(root, archive, resource)
        .map_err(|err| render_human(&err))?;
    let document = decode_nres(bytes.clone(), ReadProfile::Compatible).map_err(|err| {
        render_human(&resource_parse_diagnostic(
            "S1.NRES.DECODE",
            archive,
            resource,
            &bytes,
            err.to_string(),
        ))
    })?;
    let msh = decode_msh(&document).map_err(|err| err.to_string())?;
    let validated = validate_msh(&msh).map_err(|err| err.to_string())?;
    let node38 = if validated.node_stride == 38 {
        (0..validated.node_count)
            .filter_map(|index| {
                let node = NodeId(u32::try_from(index).ok()?);
                let metadata = node38_metadata(&validated, node)?;
                Some(Node38Inspection {
                    index,
                    parent_or_link_raw: metadata.parent_or_link_raw,
                    anim_map_start: metadata.anim_map_start,
                    fallback_key: metadata.fallback_key,
                    has_lod0_group0: selected_slot(&validated, node, Lod(0), Group(0)).is_some(),
                })
            })
            .collect()
    } else {
        Vec::new()
    };
    Ok(ModelInspection {
        streams: msh.streams().len(),
        nodes: validated.node_count,
        slots: validated.slots.len(),
        positions: validated.positions.len(),
        indices: validated.indices.len(),
        batches: validated.batches.len(),
        node_stride: validated.node_stride,
        node38,
        animation_keys: validated
            .animation
            .as_ref()
            .map(|animation| animation.keys.len()),
        animation_frame_count: validated
            .animation
            .as_ref()
            .map(|animation| animation.frame_count),
    })
}

/// Loads and validates a model resource through repository-backed lookup.
///
/// # Errors
///
/// Returns a string error when the resource cannot be resolved or parsed as a
/// valid model payload.
pub fn load_model_from_root(
    root: &Path,
    archive: &str,
    resource: &str,
) -> Result<ModelAsset, String> {
    let document = load_model_document_from_root_diagnostic(root, archive, resource)
        .map_err(|err| render_human(&err))?;
    let msh = decode_msh(&document).map_err(|err| err.to_string())?;
    validate_msh(&msh).map_err(|err| err.to_string())
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
    let bytes = read_resource_bytes_diagnostic(root, archive, resource)
        .map_err(|err| render_human(&err))?;
    let document = decode_texm(bytes).map_err(|err| err.to_string())?;
    Ok(TextureInspection {
        width: document.width(),
        height: document.height(),
        format: format!("{:?}", document.format()),
        mips: document.mip_count(),
        pages: document.page_rects().len(),
    })
}

/// Loads a decoded TEXM document through repository-backed lookup.
///
/// # Errors
///
/// Returns a string error when the resource cannot be resolved or parsed as a
/// valid TEXM payload.
pub fn load_texture_from_root(
    root: &Path,
    archive: &str,
    resource: &str,
) -> Result<fparkan_texm::TexmDocument, String> {
    let bytes = read_resource_bytes_diagnostic(root, archive, resource)
        .map_err(|err| render_human(&err))?;
    decode_texm(bytes).map_err(|err| err.to_string())
}

/// Loads and decodes TEXM mip 0 as RGBA8 through repository-backed lookup.
///
/// # Errors
///
/// Returns a string error when the resource cannot be resolved, decoded, or
/// converted to the shared RGBA8 upload representation.
pub fn load_texture_mip0_rgba8_from_root(
    root: &Path,
    archive: &str,
    resource: &str,
) -> Result<fparkan_texm::RgbaImage, String> {
    let document = load_texture_from_root(root, archive, resource)?;
    fparkan_texm::decode_mip_rgba8(&document, 0).map_err(|err| err.to_string())
}

/// Resolves phase-zero diffuse TEXM through `WEAR → MAT0 → Textures.lib`.
///
/// The selector is the positional `Batch20.material_index`, not WEAR's legacy
/// text id. MAT0 fallback remains owned by `fparkan-material`: exact requested
/// entry, then `DEFAULT`, then the first material entry.
///
/// # Errors
///
/// Returns a string error if WEAR/MAT0 resolution, phase selection, or TEXM
/// decoding fails. An empty phase-zero texture name is an intentional
/// untextured material and is reported rather than substituted with a texture.
pub fn load_wear_material_texture_mip0_rgba8_from_root(
    root: &Path,
    wear_archive: &str,
    wear_resource: &str,
    material_index: u16,
) -> Result<WearMaterialTexture, String> {
    let wear_bytes = read_resource_bytes_diagnostic(root, wear_archive, wear_resource)
        .map_err(|err| render_human(&err))?;
    let wear = decode_wear(&wear_bytes).map_err(|err| err.to_string())?;
    let repository = CachedResourceRepository::new(Arc::new(DirectoryVfs::new(root)));
    let material =
        resolve_material(&repository, &wear, material_index).map_err(|err| err.to_string())?;
    let texture = material.document.primary_texture().ok_or_else(|| {
        "MAT0 phase zero declares an intentionally untextured material".to_string()
    })?;
    let texture_name = String::from_utf8_lossy(&texture.0).into_owned();
    let image = load_texture_mip0_rgba8_from_root(root, "Textures.lib", &texture_name)?;
    Ok(WearMaterialTexture {
        wear_archive: wear_archive.to_string(),
        wear_resource: wear_resource.to_string(),
        material_index,
        material_name: String::from_utf8_lossy(&material.name.0).into_owned(),
        material_fallback: material.fallback,
        texture_name,
        image,
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

/// Loads and validates a standalone `Land.msh` file.
///
/// # Errors
///
/// Returns a human-readable error if the file cannot be read, decoded as `NRes`,
/// or decoded as the specialized terrain mesh format.
pub fn load_land_msh_from_path(path: &Path) -> Result<LandMeshDocument, String> {
    let bytes = fs::read(path).map_err(|err| format!("{}: {err}", path.display()))?;
    let document = decode_nres(Arc::from(bytes.into_boxed_slice()), ReadProfile::Compatible)
        .map_err(|err| err.to_string())?;
    decode_land_msh(&document).map_err(|err| err.to_string())
}

/// Inspects the source-coordinate bounds of a standalone `Land.msh` file.
///
/// # Errors
///
/// Returns a string error when the file cannot be read, decoded, or contains
/// no finite source positions.
pub fn inspect_land_msh_bounds_file(path: &Path) -> Result<LandMeshBoundsInspection, String> {
    let mesh = load_land_msh_from_path(path)?;
    inspect_land_msh_bounds(&mesh)
}

/// Computes source-coordinate bounds for an already validated `Land.msh`.
///
/// # Errors
///
/// Returns a string error when the mesh has no finite source positions.
pub fn inspect_land_msh_bounds(
    mesh: &LandMeshDocument,
) -> Result<LandMeshBoundsInspection, String> {
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for position in &mesh.positions {
        if !position.iter().all(|value| value.is_finite()) {
            return Err("Land.msh contains a non-finite source position".to_string());
        }
        for axis in 0..3 {
            min[axis] = min[axis].min(position[axis]);
            max[axis] = max[axis].max(position[axis]);
        }
    }
    if mesh.positions.is_empty() {
        return Err("Land.msh contains no source positions".to_string());
    }
    Ok(LandMeshBoundsInspection {
        positions: mesh.positions.len(),
        min,
        max,
    })
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

// Preserves the shared structured-error type without changing callers to boxed errors.
#[allow(clippy::result_large_err)]
fn read_resource_bytes_diagnostic(
    root: &Path,
    archive: &str,
    name: &str,
) -> Result<Arc<[u8]>, Diagnostic> {
    let repository = CachedResourceRepository::new(Arc::new(DirectoryVfs::new(root)));
    let archive_path = archive_path(archive.as_bytes()).map_err(|err| {
        diagnostic(DiagnosticCode("S1.PATH.ARCHIVE"), err.to_string()).with_context(
            DiagnosticContext {
                phase: Some(Phase::Resolve),
                path: Some(archive.to_string()),
                archive_entry: Some(name.to_string()),
                ..DiagnosticContext::default()
            },
        )
    })?;
    let resource_name = resource_name(name.as_bytes());
    let archive_handle = repository.open_archive(&archive_path).map_err(|err| {
        diagnostic(DiagnosticCode("S1.RESOURCE.OPEN_ARCHIVE"), err.to_string()).with_context(
            DiagnosticContext {
                phase: Some(Phase::Read),
                path: Some(archive.to_string()),
                archive_entry: Some(name.to_string()),
                ..DiagnosticContext::default()
            },
        )
    })?;
    let Some(handle) = repository
        .find(archive_handle, &resource_name)
        .map_err(|err| {
            diagnostic(DiagnosticCode("S1.RESOURCE.FIND"), err.to_string()).with_context(
                DiagnosticContext {
                    phase: Some(Phase::Resolve),
                    path: Some(archive.to_string()),
                    archive_entry: Some(name.to_string()),
                    ..DiagnosticContext::default()
                },
            )
        })?
    else {
        return Err(diagnostic(
            DiagnosticCode("S1.RESOURCE.MISSING_ENTRY"),
            format!(
                "resource not found: {archive}/{}",
                String::from_utf8_lossy(name.as_bytes())
            ),
        )
        .with_context(DiagnosticContext {
            phase: Some(Phase::Resolve),
            path: Some(archive.to_string()),
            archive_entry: Some(name.to_string()),
            ..DiagnosticContext::default()
        }));
    };
    let bytes = repository.read(handle).map_err(|err| {
        diagnostic(DiagnosticCode("S1.RESOURCE.READ"), err.to_string()).with_context(
            DiagnosticContext {
                phase: Some(Phase::Read),
                path: Some(archive.to_string()),
                archive_entry: Some(name.to_string()),
                ..DiagnosticContext::default()
            },
        )
    })?;
    Ok(Arc::from(bytes.into_owned()))
}

// Preserves the shared structured-error type without changing callers to boxed errors.
#[allow(clippy::result_large_err)]
fn load_model_document_from_root_diagnostic(
    root: &Path,
    archive: &str,
    resource: &str,
) -> Result<NresDocument, Diagnostic> {
    let bytes = read_resource_bytes_diagnostic(root, archive, resource)?;
    decode_nres(bytes.clone(), ReadProfile::Compatible).map_err(|err| {
        resource_parse_diagnostic("S1.NRES.DECODE", archive, resource, &bytes, err.to_string())
    })
}

fn archive_parse_diagnostic(
    code: &'static str,
    source: Option<&Path>,
    bytes: &[u8],
    message: String,
) -> Diagnostic {
    diagnostic(DiagnosticCode(code), message).with_context(DiagnosticContext {
        phase: Some(Phase::Parse),
        path: source.map(|path| path.display().to_string()),
        span: Some(SourceSpan {
            offset: 0,
            length: u64::try_from(bytes.len().min(4)).unwrap_or(4),
        }),
        ..DiagnosticContext::default()
    })
}

fn resource_parse_diagnostic(
    code: &'static str,
    archive: &str,
    resource: &str,
    bytes: &[u8],
    message: String,
) -> Diagnostic {
    diagnostic(DiagnosticCode(code), message).with_context(DiagnosticContext {
        phase: Some(Phase::Parse),
        path: Some(archive.to_string()),
        archive_entry: Some(resource.to_string()),
        span: Some(SourceSpan {
            offset: 0,
            length: u64::try_from(bytes.len().min(4)).unwrap_or(4),
        }),
        ..DiagnosticContext::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_terrain_format::TerrainSlotTable;
    use std::io::Write as _;
    use std::path::PathBuf;

    const TEST_NRES_HEADER_LEN: usize = 16;
    const TEST_NRES_NAME_LEN: usize = 36;
    const TEST_NRES_VERSION_0100: u32 = 0x100;

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
    fn archive_diagnostic_preserves_source_path_phase_and_span() {
        let dir = temp_dir("inspect-diagnostic");
        let path = dir.join("broken.nres");
        fs::write(&path, b"NRes").expect("broken nres");

        let diagnostic = inspect_archive_file_diagnostic(&path, 0).expect_err("diagnostic failure");

        assert_eq!(diagnostic.code.0, "S1.NRES.DECODE");
        let expected_path = path.display().to_string();
        assert_eq!(
            diagnostic.context.path.as_deref(),
            Some(expected_path.as_str())
        );
        assert_eq!(diagnostic.context.phase, Some(Phase::Parse));
        assert_eq!(
            diagnostic.context.span,
            Some(SourceSpan {
                offset: 0,
                length: 4
            })
        );
    }

    #[test]
    fn nres_entry_summary_fields_are_readable() {
        let dir = temp_dir("inspect-nres");
        let archive = dir.join("test.nres");
        let payload = Vec::from("NRes\x00\x00\x00\x00");
        fs::write(&archive, &payload).expect("nres");

        let _ = inspect_archive_file(&archive, 2);
    }

    #[test]
    fn model_archive_diagnostic_preserves_archive_entry_context() {
        let dir = temp_dir("inspect-model-diagnostic");
        let archive = dir.join("models.rlb");
        fs::write(&archive, build_single_entry_nres(b"BROKEN.MSH", b"NRes")).expect("archive");

        let diagnostic = load_model_document_from_root_diagnostic(&dir, "models.rlb", "BROKEN.MSH")
            .expect_err("nested diagnostic failure");

        assert_eq!(diagnostic.code.0, "S1.NRES.DECODE");
        assert_eq!(diagnostic.context.phase, Some(Phase::Parse));
        assert_eq!(diagnostic.context.path.as_deref(), Some("models.rlb"));
        assert_eq!(
            diagnostic.context.archive_entry.as_deref(),
            Some("BROKEN.MSH")
        );
        assert_eq!(
            diagnostic.context.span,
            Some(SourceSpan {
                offset: 0,
                length: 4
            })
        );
    }

    #[test]
    fn wear_material_texture_loader_preserves_original_selection_provenance() {
        let dir = temp_dir("wear-material-texture");
        fs::write(
            dir.join("wear.rlb"),
            build_single_entry_nres(b"MODEL.WEA", b"1\n0 MAT\n"),
        )
        .expect("wear archive");
        let mut mat0 = vec![0; 4 + 34];
        mat0[0..2].copy_from_slice(&1_u16.to_le_bytes());
        mat0[22..25].copy_from_slice(b"TEX");
        fs::write(
            dir.join("material.lib"),
            build_single_entry_nres_with_meta(b"MAT", fparkan_material::MAT0_KIND, 0, &mat0),
        )
        .expect("material archive");
        fs::write(
            dir.join("Textures.lib"),
            build_single_entry_nres(b"TEX", &texm_argb8888_pixel([0x40, 0x11, 0x22, 0x33])),
        )
        .expect("texture archive");

        let selected =
            load_wear_material_texture_mip0_rgba8_from_root(&dir, "wear.rlb", "MODEL.WEA", 0)
                .expect("resolved material texture");

        assert_eq!(selected.material_name, "MAT");
        assert_eq!(selected.material_fallback, MaterialFallback::Exact);
        assert_eq!(selected.texture_name, "TEX");
        assert_eq!(selected.image.rgba8, vec![0x11, 0x22, 0x33, 0x40]);
    }

    #[test]
    fn land_mesh_bounds_preserve_each_source_axis() {
        let mesh = LandMeshDocument {
            streams: Vec::new(),
            nodes_raw: Vec::new(),
            slots: TerrainSlotTable {
                header_raw: Vec::new(),
                slots_raw: Vec::new(),
            },
            positions: vec![[4.0, -2.0, 8.0], [-3.0, 6.0, 1.0]],
            normals: Vec::new(),
            uv0: Vec::new(),
            accelerator: Vec::new(),
            aux14: Vec::new(),
            aux18: Vec::new(),
            faces: Vec::new(),
        };

        let bounds = inspect_land_msh_bounds(&mesh).expect("bounds");

        assert_eq!(bounds.positions, 2);
        assert_eq!(bounds.min, [-3.0, -2.0, 1.0]);
        assert_eq!(bounds.max, [4.0, 6.0, 8.0]);
    }

    fn temp_dir(name: &str) -> PathBuf {
        let base = PathBuf::from("/tmp")
            .join("fparkan-inspection-tests")
            .join(name);
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).expect("tmp dir");
        base
    }

    fn build_single_entry_nres(name: &[u8], payload: &[u8]) -> Vec<u8> {
        build_single_entry_nres_with_meta(name, 1, 0, payload)
    }

    fn build_single_entry_nres_with_meta(
        name: &[u8],
        type_id: u32,
        attr2: u32,
        payload: &[u8],
    ) -> Vec<u8> {
        let mut out = vec![0; TEST_NRES_HEADER_LEN];
        let payload_offset = u32::try_from(out.len()).expect("payload offset");
        out.extend_from_slice(payload);
        let padding = (8 - (out.len() % 8)) % 8;
        out.resize(out.len() + padding, 0);

        push_u32(&mut out, type_id);
        push_u32(&mut out, 0);
        push_u32(&mut out, attr2);
        push_u32(&mut out, u32::try_from(payload.len()).expect("payload len"));
        push_u32(&mut out, 0);
        let mut raw_name = [0; TEST_NRES_NAME_LEN];
        raw_name[..name.len()].copy_from_slice(name);
        out.extend_from_slice(&raw_name);
        push_u32(&mut out, payload_offset);
        push_u32(&mut out, 0);

        out[0..4].copy_from_slice(b"NRes");
        out[4..8].copy_from_slice(&TEST_NRES_VERSION_0100.to_le_bytes());
        out[8..12].copy_from_slice(&1_u32.to_le_bytes());
        let total_size = u32::try_from(out.len()).expect("total size");
        out[12..16].copy_from_slice(&total_size.to_le_bytes());
        out
    }

    fn push_u32(out: &mut Vec<u8>, value: u32) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn texm_argb8888_pixel(pixel: [u8; 4]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&0x6D78_6554_u32.to_le_bytes());
        out.extend_from_slice(&1_u32.to_le_bytes());
        out.extend_from_slice(&1_u32.to_le_bytes());
        out.extend_from_slice(&1_u32.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&8888_u32.to_le_bytes());
        out.extend_from_slice(&pixel);
        out
    }
}
