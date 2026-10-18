#![forbid(unsafe_code)]
//! Asset manager ports and transactional preparation models.

use fparkan_material::{
    decode_mat0, decode_wear, resolve_material, MaterialFallback, ResolvedMaterial, WearTable,
    MAT0_KIND, WEAR_KIND,
};
pub use fparkan_material::{
    sample_material_normalized_phase, sample_material_phase, Mat0Document, MaterialCoefficients,
    MaterialError, MaterialPhase, MaterialPhaseSample,
};
use fparkan_mission_format::{decode_tma, decode_tma_land_path, ClanBody};
pub use fparkan_mission_format::{LpString, MissionDocument, MissionError, TmaProfile};
use fparkan_msh::{decode_msh, validate_msh, ModelAsset, MshError};
use fparkan_nres::{decode as decode_nres, ReadProfile};
pub use fparkan_nres::{NresDocument, NresError};
use fparkan_path::{normalize_relative, NormalizedPath, PathError, PathPolicy, ResourceName};
use fparkan_prototype::{
    decode_bas_geometry, BasGeometry, EffectivePrototype, PrototypeGeometry, PrototypeGraph,
    PrototypeGraphEdge, PrototypeGraphFailure, PrototypeGraphNodeKind, PrototypeGraphProvenance,
    PrototypeGraphReport, PrototypeGraphRequiredness,
};
use fparkan_resource::{CachedResourceRepository, ResourceError, ResourceKey, ResourceRepository};
pub use fparkan_terrain::{TerrainError, TerrainWorld};
use fparkan_terrain_format::{decode_build_dat, decode_land_map, decode_land_msh};
pub use fparkan_terrain_format::{BuildCategory, TerrainFormatError};
use fparkan_texm::{decode_mip_rgba8, decode_texm, TexmDocument, TexmError};
pub use fparkan_texm::{PageRect, RgbaImage};
use fparkan_vfs::DirectoryVfs;
use std::collections::{hash_map::Entry, HashMap};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const TEXTURES_ARCHIVE: &str = "textures.lib";
const LIGHTMAP_ARCHIVE: &str = "lightmap.lib";

/// Material coefficients decoded from one selected MAT0 phase.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedMaterialPhase {
    /// Raw NRes MAT0 entry flags retained from the archive directory.
    pub attr1: u32,
    /// Native material pipeline category `(attr1 >> 2) & 0xF`.
    pub pipeline_category: u8,
    /// Native material feature bit `attr1 & 0x40`.
    pub native_flag_40: bool,
    /// Phase parameters 4..6, normalized directional light RGB coefficient.
    pub directional_rgb: [f32; 3],
    /// Phase parameters 0..2, normalized additive RGB coefficient.
    pub additive_rgb: [f32; 3],
    /// Phase parameter 3, stored as a percentage.
    pub diffuse_alpha: f32,
    /// Phase parameters 8..10, normalized specular RGB coefficient.
    pub specular_rgb: [f32; 3],
    /// Phase parameter 16, integer specular power.
    pub power: u8,
    /// Signed phase parameter 17 selecting a TEXM Page rectangle, or -1.
    pub page_index: i8,
    /// Normalized atlas transform `[offset_x, offset_y, scale_x, scale_y]`.
    pub page_uv_transform: [f32; 4],
}

type WearValidationCache =
    HashMap<(NormalizedPath, Vec<u8>), Result<fparkan_material::WearTable, String>>;

/// Extracts the native pipeline category from a MAT0 NRes entry's `attr1`.
#[must_use]
pub const fn material_pipeline_category(attr1: u32) -> u8 {
    ((attr1 >> 2) & 0x0F) as u8
}

/// Returns whether a MAT0 NRes entry carries native feature bit `0x40`.
#[must_use]
pub const fn material_native_flag_40(attr1: u32) -> bool {
    (attr1 & 0x40) != 0
}

/// Canonical terrain archive paths derived from a mission land reference.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MissionTerrainPaths {
    /// Landscape mesh archive path.
    pub land_msh: NormalizedPath,
    /// Landscape map archive path.
    pub land_map: NormalizedPath,
}

/// Raw compiled-script base selected by one TMA clan.
///
/// The path remains raw legacy bytes until the runtime applies its path policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MissionScriptBundleBase {
    /// Index in the mission clan array.
    pub clan_index: usize,
    /// First clan resource path, without a required `.scr` suffix.
    pub path_raw: Vec<u8>,
}

/// Terrain loading errors that include runtime world construction failures.
#[derive(Debug)]
pub enum TerrainPreparationError {
    /// Format error while decoding terrain documents.
    Decode(TerrainFormatError),
    /// Runtime terrain constructor failed.
    Runtime(TerrainError),
}

impl std::fmt::Display for TerrainPreparationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Decode(source) => write!(f, "{source}"),
            Self::Runtime(source) => write!(f, "{source}"),
        }
    }
}

impl std::error::Error for TerrainPreparationError {}

impl From<TerrainFormatError> for TerrainPreparationError {
    fn from(source: TerrainFormatError) -> Self {
        Self::Decode(source)
    }
}

impl From<TerrainError> for TerrainPreparationError {
    fn from(source: TerrainError) -> Self {
        Self::Runtime(source)
    }
}

/// Decodes a mission file bytes payload with a typed profile.
///
/// # Errors
///
/// Returns [`MissionError`] when the mission payload is malformed for the
/// selected profile.
pub fn decode_mission_payload(
    bytes: Arc<[u8]>,
    profile: TmaProfile,
) -> Result<MissionDocument, MissionError> {
    decode_tma(bytes, profile)
}

/// Reads only the mission land path from raw TMA bytes.
///
/// # Errors
///
/// Returns [`MissionError`] when the mission header or land path record cannot
/// be decoded.
pub fn decode_mission_land_path(
    bytes: &[u8],
    profile: TmaProfile,
) -> Result<LpString, MissionError> {
    decode_tma_land_path(bytes, profile)
}

/// Returns non-empty script/formula bases selected by TMA clans.
///
/// The first resource has the same position in the standard and spatial clan
/// layouts. Its consumer-specific meaning is intentionally left to runtime.
#[must_use]
pub fn mission_script_bundle_bases(mission: &MissionDocument) -> Vec<MissionScriptBundleBase> {
    mission
        .clans
        .iter()
        .enumerate()
        .filter_map(|(clan_index, clan)| {
            let path_raw = match &clan.body {
                ClanBody::Standard { first_resource, .. } => first_resource.path.raw.clone(),
                ClanBody::Spatial { first_resource, .. } => first_resource.raw.clone(),
            };
            (!path_raw.is_empty()).then_some(MissionScriptBundleBase {
                clan_index,
                path_raw,
            })
        })
        .collect()
}

/// Builds canonical mission terrain paths from the mission `Land` reference.
///
/// # Errors
///
/// Returns [`PathError`] when the mission land reference is not a strict
/// relative legacy path.
pub fn derive_mission_land_paths(land_path: &LpString) -> Result<MissionTerrainPaths, PathError> {
    let normalized = normalize_relative(&land_path.raw, PathPolicy::StrictLegacy)?;
    let Some((parent, _stem)) = normalized.as_str().rsplit_once('/') else {
        return Err(PathError::Empty);
    };
    let land_msh = normalize_relative(
        format!("{parent}/Land.msh").as_bytes(),
        PathPolicy::StrictLegacy,
    )?;
    let land_map = normalize_relative(
        format!("{parent}/Land.map").as_bytes(),
        PathPolicy::StrictLegacy,
    )?;
    Ok(MissionTerrainPaths { land_msh, land_map })
}

/// Decodes compatible `NRes` payload for terrain/document loading.
///
/// # Errors
///
/// Returns [`NresError`] when the payload is not a compatible `NRes` archive.
pub fn decode_nres_payload(
    bytes: Arc<[u8]>,
) -> Result<fparkan_nres::NresDocument, fparkan_nres::NresError> {
    decode_nres(bytes, ReadProfile::Compatible)
}

/// Loads every authored RGBA8 mip and the selected phase coefficients from a
/// map-local standalone WEAR table.
pub fn load_standalone_wear_material_texture_rgba8_and_phase_from_root(
    root: &Path,
    wear_path: &Path,
    material_index: u16,
) -> Result<(Vec<RgbaImage>, PreparedMaterialPhase), String> {
    load_standalone_wear_material_texture_rgba8_and_phase_at_index_from_root(
        root,
        wear_path,
        material_index,
        0,
    )
}

/// Loads an authored TEXM mip chain and MAT0 coefficients for one phase.
/// Empty phase texture names are treated as intentionally untextured.
pub fn load_standalone_wear_material_texture_rgba8_and_phase_at_index_from_root(
    root: &Path,
    wear_path: &Path,
    material_index: u16,
    phase_index: u16,
) -> Result<(Vec<RgbaImage>, PreparedMaterialPhase), String> {
    let mut loader = StandaloneWearMaterialLoader::new(root);
    loader.load(wear_path, material_index, phase_index)
}

/// Reusable standalone WEAR/MAT0/TEXM loader for map-local preview paths.
///
/// The native preview can request many rows from `Land1.wea` and `Land2.wea`.
/// Keeping one repository and one decoded table per path avoids repeating the
/// archive fingerprint, WEAR parse, and material lookup work for every row.
pub struct StandaloneWearMaterialLoader {
    repository: CachedResourceRepository,
    wears: HashMap<PathBuf, WearTable>,
    materials: HashMap<(PathBuf, u16), ResolvedMaterial>,
    loaded: HashMap<(PathBuf, u16, u16), (Vec<RgbaImage>, PreparedMaterialPhase)>,
}

impl StandaloneWearMaterialLoader {
    /// Creates a loader over an immutable install root.
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self {
            repository: CachedResourceRepository::new(Arc::new(DirectoryVfs::new(root))),
            wears: HashMap::new(),
            materials: HashMap::new(),
            loaded: HashMap::new(),
        }
    }

    /// Loads one material row and phase from a map-local WEAR table.
    ///
    /// # Errors
    ///
    /// Returns a formatted error when the table, material, or texture cannot
    /// be decoded.
    pub fn load(
        &mut self,
        wear_path: &Path,
        material_index: u16,
        phase_index: u16,
    ) -> Result<(Vec<RgbaImage>, PreparedMaterialPhase), String> {
        let cache_key = wear_path.to_path_buf();
        if !self.wears.contains_key(&cache_key) {
            let wear = decode_wear(
                &std::fs::read(wear_path)
                    .map_err(|err| format!("{}: {err}", wear_path.display()))?,
            )
            .map_err(|err| err.to_string())?;
            self.wears.insert(cache_key.clone(), wear);
        }
        let material = self.resolve_material(&cache_key, material_index)?;
        let loaded_key = (cache_key.clone(), material_index, phase_index);
        if let Some(result) = self.loaded.get(&loaded_key) {
            return Ok(result.clone());
        }
        let result = load_resolved_material_texture_rgba8_and_phase(
            &self.repository,
            &material,
            phase_index,
        )?;
        self.loaded.insert(loaded_key, result.clone());
        Ok(result)
    }

    /// Returns the decoded MAT0 document for one map-local WEAR row.
    ///
    /// The document is cloned at this boundary so callers can retain a
    /// frame-sampling binding while the loader continues to cache resources.
    ///
    /// # Errors
    ///
    /// Returns a formatted error when the WEAR table or material cannot be
    /// resolved.
    pub fn material_document(
        &mut self,
        wear_path: &Path,
        material_index: u16,
    ) -> Result<Mat0Document, String> {
        let cache_key = wear_path.to_path_buf();
        self.ensure_wear(&cache_key)?;
        self.resolve_material(&cache_key, material_index)
            .map(|material| material.document)
    }

    fn ensure_wear(&mut self, cache_key: &Path) -> Result<(), String> {
        if self.wears.contains_key(cache_key) {
            return Ok(());
        }
        let wear = decode_wear(
            &std::fs::read(cache_key).map_err(|err| format!("{}: {err}", cache_key.display()))?,
        )
        .map_err(|err| err.to_string())?;
        self.wears.insert(cache_key.to_path_buf(), wear);
        Ok(())
    }

    fn resolve_material(
        &mut self,
        cache_key: &Path,
        material_index: u16,
    ) -> Result<ResolvedMaterial, String> {
        let material_key = (cache_key.to_path_buf(), material_index);
        if let Some(material) = self.materials.get(&material_key) {
            return Ok(material.clone());
        }
        let wear = self
            .wears
            .get(cache_key)
            .ok_or_else(|| "standalone WEAR cache insertion failed".to_string())?;
        let material = resolve_material(&self.repository, wear, material_index)
            .map_err(|err| err.to_string())?;
        self.materials.insert(material_key, material.clone());
        Ok(material)
    }
}

/// Loads named map-local materials together with their complete MAT0
/// documents.  This is the runtime form used by sky/environment bindings;
/// the older helper remains available for callers that only need phase zero.
pub fn load_standalone_wear_named_material_textures_rgba8_and_phases_with_documents_from_root(
    root: &Path,
    wear_path: &Path,
    phase_index: u16,
) -> Result<Vec<(Vec<u8>, Vec<RgbaImage>, PreparedMaterialPhase, Mat0Document)>, String> {
    let wear = decode_wear(
        &std::fs::read(wear_path).map_err(|err| format!("{}: {err}", wear_path.display()))?,
    )
    .map_err(|err| err.to_string())?;
    let repository = CachedResourceRepository::new(Arc::new(DirectoryVfs::new(root)));
    wear.entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let material_index =
                u16::try_from(index).map_err(|_| "WEAR material index exceeds u16".to_string())?;
            let material = resolve_material(&repository, &wear, material_index)
                .map_err(|err| err.to_string())?;
            let (mips, phase) = load_resolved_material_texture_rgba8_and_phase(
                &repository,
                &material,
                phase_index,
            )?;
            Ok((entry.material.0.clone(), mips, phase, material.document))
        })
        .collect()
}

/// Loads every named MAT0 entry in a map-local WEAR table through one cached
/// resource repository.  Sky material rows use this path: their names are
/// material.lib entries, not direct textures.lib TEXM names.
pub fn load_standalone_wear_named_material_textures_rgba8_and_phases_from_root(
    root: &Path,
    wear_path: &Path,
    phase_index: u16,
) -> Result<Vec<(Vec<u8>, Vec<RgbaImage>, PreparedMaterialPhase)>, String> {
    let wear = decode_wear(
        &std::fs::read(wear_path).map_err(|err| format!("{}: {err}", wear_path.display()))?,
    )
    .map_err(|err| err.to_string())?;
    let repository = CachedResourceRepository::new(Arc::new(DirectoryVfs::new(root)));
    wear.entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let material_index =
                u16::try_from(index).map_err(|_| "WEAR material index exceeds u16".to_string())?;
            let material = resolve_material(&repository, &wear, material_index)
                .map_err(|err| err.to_string())?;
            let (mips, phase) = load_resolved_material_texture_rgba8_and_phase(
                &repository,
                &material,
                phase_index,
            )?;
            Ok((entry.material.0.clone(), mips, phase))
        })
        .collect()
}

fn load_resolved_material_texture_rgba8_and_phase<R: ResourceRepository>(
    repository: &R,
    material: &ResolvedMaterial,
    phase_index: u16,
) -> Result<(Vec<RgbaImage>, PreparedMaterialPhase), String> {
    let phase = material
        .document
        .phases
        .get(usize::from(phase_index))
        .or_else(|| material.document.phases.first())
        .ok_or_else(|| "MAT0 declares no material phases".to_string())?;
    let texture_bytes = phase
        .texture_raw
        .iter()
        .copied()
        .take_while(|byte| *byte != 0)
        .collect::<Vec<_>>();
    let document = if texture_bytes.is_empty() {
        None
    } else {
        let texture = ResourceName(texture_bytes);
        let key = ResourceKey {
            archive: parse_path(TEXTURES_ARCHIVE).map_err(|err| err.to_string())?,
            name: texture.clone(),
            type_id: None,
        };
        let bytes = read_optional_key(repository, &key, Some("texture"))
            .map_err(|err| err.to_string())?
            .ok_or_else(|| format!("missing texture {texture:?}"))?;
        Some(decode_texm(bytes).map_err(|err| err.to_string())?)
    };
    let page_index = i8::from_ne_bytes([phase.parameters[17]]);
    let page_uv = match document.as_ref() {
        Some(document) => page_uv_transform(
            document.width(),
            document.height(),
            &document.page_rects(),
            page_index,
        )?,
        None => [0.0, 0.0, 1.0, 1.0],
    };
    let phase = PreparedMaterialPhase {
        attr1: material.attr1,
        pipeline_category: material_pipeline_category(material.attr1),
        native_flag_40: material_native_flag_40(material.attr1),
        directional_rgb: [
            f32::from(phase.parameters[4]) / 255.0,
            f32::from(phase.parameters[5]) / 255.0,
            f32::from(phase.parameters[6]) / 255.0,
        ],
        additive_rgb: [
            f32::from(phase.parameters[0]) / 255.0,
            f32::from(phase.parameters[1]) / 255.0,
            f32::from(phase.parameters[2]) / 255.0,
        ],
        diffuse_alpha: f32::from(phase.parameters[3]) * 0.01,
        specular_rgb: [
            f32::from(phase.parameters[8]) / 255.0,
            f32::from(phase.parameters[9]) / 255.0,
            f32::from(phase.parameters[10]) / 255.0,
        ],
        power: phase.parameters[16],
        page_index,
        page_uv_transform: page_uv,
    };
    let mips = match document {
        Some(document) => (0..document.mip_count())
            .map(|level| decode_mip_rgba8(&document, u32::try_from(level).unwrap_or(u32::MAX)))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| err.to_string())?,
        None => vec![RgbaImage {
            width: 1,
            height: 1,
            rgba8: vec![255, 255, 255, 255],
        }],
    };
    Ok((mips, phase))
}

/// Loads a named `material.lib` MAT0 entry and its selected texture phase.
/// This is used by FX resources whose opcode carries a direct material
/// reference instead of a WEAR-table index.
pub fn load_material_name_texture_rgba8_and_phase_from_root(
    root: &Path,
    material_name: &str,
    phase_index: u16,
) -> Result<(Vec<RgbaImage>, PreparedMaterialPhase), String> {
    load_material_name_texture_rgba8_and_phase_with_document_from_root(
        root,
        material_name,
        phase_index,
    )
    .map(|(mips, phase, _document)| (mips, phase))
}

/// Loads a named `material.lib` entry, its selected texture phase, and the
/// complete MAT0 document used by the runtime animation sampler.
pub fn load_material_name_texture_rgba8_and_phase_with_document_from_root(
    root: &Path,
    material_name: &str,
    phase_index: u16,
) -> Result<(Vec<RgbaImage>, PreparedMaterialPhase, Mat0Document), String> {
    load_material_archive_name_texture_rgba8_and_phase_with_document_from_root(
        root,
        "material.lib",
        material_name,
        phase_index,
    )
}

/// Loads a named MAT0 entry from an FX-selected archive, its selected texture
/// phase, and the complete document used by the runtime animation sampler.
pub fn load_material_archive_name_texture_rgba8_and_phase_with_document_from_root(
    root: &Path,
    archive_name: &str,
    material_name: &str,
    phase_index: u16,
) -> Result<(Vec<RgbaImage>, PreparedMaterialPhase, Mat0Document), String> {
    if archive_name.is_empty() {
        return Err("material archive name is empty".to_string());
    }
    if material_name.is_empty() {
        return Err("material name is empty".to_string());
    }
    let repository = CachedResourceRepository::new(Arc::new(DirectoryVfs::new(root)));
    load_material_archive_name_texture_rgba8_and_phase_from_repository(
        &repository,
        archive_name,
        material_name,
        phase_index,
    )
}

fn load_material_archive_name_texture_rgba8_and_phase_from_repository<R: ResourceRepository>(
    repository: &R,
    archive_name: &str,
    material_name: &str,
    phase_index: u16,
) -> Result<(Vec<RgbaImage>, PreparedMaterialPhase, Mat0Document), String> {
    let archive = parse_path(archive_name).map_err(|err| err.to_string())?;
    let archive_id = repository
        .open_archive(&archive)
        .map_err(|err| err.to_string())?;
    let name = ResourceName(material_name.as_bytes().to_vec());
    let entry = repository
        .find(archive_id, &name)
        .map_err(|err| err.to_string())?
        .ok_or_else(|| format!("missing material {archive_name}/{material_name}"))?;
    let info = repository
        .entry_info(entry)
        .map_err(|err| err.to_string())?;
    if info.key.type_id != Some(MAT0_KIND) {
        return Err(format!(
            "material {archive_name}/{material_name} is not a MAT0 entry"
        ));
    }
    let document = decode_mat0(
        repository
            .read(entry)
            .map_err(|err| err.to_string())?
            .as_slice(),
        info.attr2,
    )
    .map_err(|err| err.to_string())?;
    let material = ResolvedMaterial {
        name: info.key.name,
        fallback: MaterialFallback::Exact,
        attr1: info.attr1,
        document,
    };
    let document = material.document.clone();
    let (mips, phase) =
        load_resolved_material_texture_rgba8_and_phase(repository, &material, phase_index)?;
    Ok((mips, phase, document))
}

/// Reads one named entry from a legacy resource archive through the same
/// cached repository used by the material loaders.
pub fn load_resource_entry_bytes_from_root(
    root: &Path,
    archive_name: &str,
    entry_name: &str,
) -> Result<Arc<[u8]>, String> {
    if archive_name.is_empty() || entry_name.is_empty() {
        return Err("resource archive and entry names must be non-empty".to_string());
    }
    let repository = CachedResourceRepository::new(Arc::new(DirectoryVfs::new(root)));
    let archive = parse_path(archive_name).map_err(|err| err.to_string())?;
    let archive_id = repository
        .open_archive(&archive)
        .map_err(|err| err.to_string())?;
    let entry = repository
        .find(archive_id, &ResourceName(entry_name.as_bytes().to_vec()))
        .map_err(|err| err.to_string())?
        .ok_or_else(|| format!("missing resource {archive_name}/{entry_name}"))?;
    Ok(Arc::from(
        repository
            .read(entry)
            .map_err(|err| err.to_string())?
            .into_owned()
            .into_boxed_slice(),
    ))
}

/// Loads every authored mip of a named entry from the normal texture archive.
///
/// Environment material tables (`sky.wea`) contain direct texture entry names
/// rather than WEAR indirections, so they use this small archive helper.
pub fn load_texture_rgba8_mips_from_root(
    root: &Path,
    texture_name: &str,
) -> Result<Vec<RgbaImage>, String> {
    if texture_name.is_empty() {
        return Err("texture name is empty".to_string());
    }
    let repository = CachedResourceRepository::new(Arc::new(DirectoryVfs::new(root)));
    let archive = ResourceKey {
        archive: parse_path(TEXTURES_ARCHIVE).map_err(|err| err.to_string())?,
        name: ResourceName(texture_name.as_bytes().to_vec()),
        type_id: None,
    };
    let archive_id = repository
        .open_archive(&archive.archive)
        .map_err(|err| err.to_string())?;
    let entry = repository
        .find(archive_id, &archive.name)
        .map_err(|err| err.to_string())?
        .ok_or_else(|| format!("missing texture {texture_name:?}"))?;
    let bytes = repository.read(entry).map_err(|err| err.to_string())?;
    let document = decode_texm(Arc::from(bytes.as_slice().to_vec().into_boxed_slice()))
        .map_err(|err| err.to_string())?;
    (0..document.mip_count())
        .map(|level| decode_mip_rgba8(&document, u32::try_from(level).unwrap_or(u32::MAX)))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| err.to_string())
}

/// Returns the native normalized atlas transform for a signed MAT0 page index.
///
/// The transform is `[offset_x, offset_y, scale_x, scale_y]` and is applied as
/// `atlas_uv = offset + page_local_uv * scale`. A negative selector addresses
/// the complete texture.
pub fn page_uv_transform(
    width: u32,
    height: u32,
    page_rects: &[PageRect],
    page_index: i8,
) -> Result<[f32; 4], String> {
    if width == 0 || height == 0 {
        return Err("TEXM page transform has zero texture extent".to_string());
    }
    if page_index < 0 {
        return Ok([0.0, 0.0, 1.0, 1.0]);
    }
    let rect = page_rects
        .get(usize::from(page_index as u8))
        .ok_or_else(|| {
            format!(
                "MAT0 page selector {page_index} exceeds TEXM Page rect count {}",
                page_rects.len()
            )
        })?;
    if rect.x < 0 || rect.y < 0 || rect.w <= 0 || rect.h <= 0 {
        return Err(format!(
            "TEXM Page rect is invalid: x={} y={} w={} h={}",
            rect.x, rect.y, rect.w, rect.h
        ));
    }
    if i64::from(rect.x) + i64::from(rect.w) > i64::from(width)
        || i64::from(rect.y) + i64::from(rect.h) > i64::from(height)
    {
        return Err(format!(
            "TEXM Page rect exceeds base extent {width}x{height}: x={} y={} w={} h={}",
            rect.x, rect.y, rect.w, rect.h
        ));
    }
    Ok([
        f32::from(rect.x) / width as f32,
        f32::from(rect.y) / height as f32,
        f32::from(rect.w) / width as f32,
        f32::from(rect.h) / height as f32,
    ])
}

/// Crops decoded TEXM mips to a signed MAT0 page selector.
///
/// A selector of `-1` means that the texture has no page selection and is
/// returned unchanged. Non-negative selectors address the TEXM `Page` table;
/// rectangles are scaled with floor origin and ceil end for each mip level.
/// The returned images use page-local 0..1 UVs, matching the native atlas
/// sampling contract.
pub fn crop_rgba8_mips_to_page(
    mips: Vec<RgbaImage>,
    page_rects: &[PageRect],
    page_index: i8,
) -> Result<Vec<RgbaImage>, String> {
    if page_index < 0 {
        return Ok(mips);
    }
    let (base_width_u32, base_height_u32) = mips
        .first()
        .map(|base| (base.width, base.height))
        .ok_or_else(|| "cannot select a page from an empty TEXM mip chain".to_string())?;
    let rect = page_rects
        .get(usize::from(page_index as u8))
        .ok_or_else(|| {
            format!(
                "MAT0 page selector {page_index} exceeds TEXM Page rect count {}",
                page_rects.len()
            )
        })?;
    if rect.x < 0 || rect.y < 0 || rect.w <= 0 || rect.h <= 0 {
        return Err(format!(
            "TEXM Page rect is invalid: x={} y={} w={} h={}",
            rect.x, rect.y, rect.w, rect.h
        ));
    }
    let base_x_end = i64::from(rect.x) + i64::from(rect.w);
    let base_y_end = i64::from(rect.y) + i64::from(rect.h);
    if base_x_end > i64::from(base_width_u32) || base_y_end > i64::from(base_height_u32) {
        return Err(format!(
            "TEXM Page rect exceeds base extent {}x{}: x={} y={} w={} h={}",
            base_width_u32, base_height_u32, rect.x, rect.y, rect.w, rect.h
        ));
    }
    mips.into_iter()
        .enumerate()
        .map(|(level, image)| {
            let scale_width = i64::from(image.width);
            let scale_height = i64::from(image.height);
            let base_width = i64::from(base_width_u32);
            let base_height = i64::from(base_height_u32);
            let x0 = floor_mul_div(i64::from(rect.x), scale_width, base_width);
            let x1 = ceil_mul_div(base_x_end, scale_width, base_width);
            let y0 = floor_mul_div(i64::from(rect.y), scale_height, base_height);
            let y1 = ceil_mul_div(base_y_end, scale_height, base_height);
            let x0 = u32::try_from(x0)
                .map_err(|_| format!("TEXM Page mip {level} x origin is invalid"))?;
            let y0 = u32::try_from(y0)
                .map_err(|_| format!("TEXM Page mip {level} y origin is invalid"))?;
            let x1 =
                u32::try_from(x1).map_err(|_| format!("TEXM Page mip {level} x end is invalid"))?;
            let y1 =
                u32::try_from(y1).map_err(|_| format!("TEXM Page mip {level} y end is invalid"))?;
            if x0 >= x1 || y0 >= y1 || x1 > image.width || y1 > image.height {
                return Err(format!(
                    "TEXM Page mip {level} scaled rect is outside {}x{}: {x0},{y0}..{x1},{y1}",
                    image.width, image.height
                ));
            }
            let width = x1 - x0;
            let height = y1 - y0;
            let row_bytes = usize::try_from(width)
                .ok()
                .and_then(|width| width.checked_mul(4))
                .ok_or_else(|| format!("TEXM Page mip {level} row size overflows"))?;
            let capacity = usize::try_from(width)
                .ok()
                .and_then(|width| {
                    usize::try_from(height)
                        .ok()
                        .and_then(|height| width.checked_mul(height))
                })
                .and_then(|pixels| pixels.checked_mul(4))
                .ok_or_else(|| format!("TEXM Page mip {level} size overflows"))?;
            let mut rgba8 = Vec::with_capacity(capacity);
            let source_stride = usize::try_from(image.width)
                .ok()
                .and_then(|width| width.checked_mul(4))
                .ok_or_else(|| format!("TEXM Page mip {level} source stride overflows"))?;
            let start_x = usize::try_from(x0)
                .ok()
                .and_then(|x| x.checked_mul(4))
                .ok_or_else(|| format!("TEXM Page mip {level} x offset overflows"))?;
            for row in y0..y1 {
                let row_start = usize::try_from(row)
                    .ok()
                    .and_then(|row| row.checked_mul(source_stride))
                    .and_then(|offset| offset.checked_add(start_x))
                    .ok_or_else(|| format!("TEXM Page mip {level} row offset overflows"))?;
                let row_end = row_start
                    .checked_add(row_bytes)
                    .ok_or_else(|| format!("TEXM Page mip {level} row end overflows"))?;
                let source = image
                    .rgba8
                    .get(row_start..row_end)
                    .ok_or_else(|| format!("TEXM Page mip {level} row exceeds decoded pixels"))?;
                rgba8.extend_from_slice(source);
            }
            Ok(RgbaImage {
                width,
                height,
                rgba8,
            })
        })
        .collect()
}

fn floor_mul_div(value: i64, scale: i64, base: i64) -> i64 {
    let product = value.saturating_mul(scale);
    let quotient = product / base;
    if product < 0 && product % base != 0 {
        quotient - 1
    } else {
        quotient
    }
}

fn ceil_mul_div(value: i64, scale: i64, base: i64) -> i64 {
    let product = value.saturating_mul(scale);
    let quotient = product / base;
    if product > 0 && product % base != 0 {
        quotient + 1
    } else {
        quotient
    }
}

/// Decodes terrain documents and builds immutable terrain state.
///
/// # Errors
///
/// Returns [`TerrainPreparationError`] when terrain documents are malformed or
/// cannot be converted into runtime terrain state.
pub fn prepare_terrain_world(
    land_msh_nres: &fparkan_nres::NresDocument,
    land_map_nres: &fparkan_nres::NresDocument,
    build_dat: &[u8],
) -> Result<(TerrainWorld, Vec<BuildCategory>), TerrainPreparationError> {
    let land_msh = decode_land_msh(land_msh_nres)?;
    let land_map = decode_land_map(land_map_nres)?;
    let build_categories = decode_build_dat(build_dat)?;
    let world = TerrainWorld::from_land_assets(&land_msh, &land_map)?;
    Ok((world, build_categories))
}

/// Stable typed identifier for a prepared asset.
#[derive(Debug)]
pub struct AssetId<T> {
    raw: u64,
    marker: PhantomData<T>,
}

impl<T> Clone for AssetId<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for AssetId<T> {}

impl<T> PartialEq for AssetId<T> {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}

impl<T> Eq for AssetId<T> {}

impl<T> Hash for AssetId<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.raw.hash(state);
    }
}

impl<T> AssetId<T> {
    /// Creates an asset id from a stable raw value.
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self {
            raw,
            marker: PhantomData,
        }
    }

    /// Returns the stable raw id.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.raw
    }
}

/// CPU-side data needed before a visual can be handed to a renderer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedVisual {
    /// Stable id derived from the prototype geometry key.
    pub id: AssetId<PreparedVisual>,
    /// Optional mesh resource backing the visual.
    pub mesh: Option<ResourceKey>,
    /// Source registry entry type (`EXTO`/`INTO`/etc.) when preserved.
    pub source_type: Option<u32>,
    /// Prepared model backing the visual, when geometry is present.
    pub model_id: Option<AssetId<PreparedModel>>,
    /// Prepared WEAR table backing the visual, when geometry is present.
    pub wear_id: Option<AssetId<PreparedWear>>,
    /// Number of validated model nodes.
    pub model_nodes: usize,
    /// Number of validated material slots on the model.
    pub model_slots: usize,
    /// Number of validated render batches.
    pub model_batches: usize,
    /// Number of WEAR material slots resolved through MAT0.
    pub material_count: usize,
    /// Typed material IDs available from the resolved visual.
    pub material_ids: Vec<AssetId<PreparedMaterial>>,
    /// Typed texture IDs available from the resolved visual.
    pub texture_ids: Vec<AssetId<PreparedTexture>>,
    /// Typed lightmap IDs available from the resolved visual.
    pub lightmap_ids: Vec<AssetId<PreparedTexture>>,
    /// Number of texture phase requests decoded as TEXM.
    pub texture_count: usize,
    /// Number of lightmap requests decoded as TEXM.
    pub lightmap_count: usize,
}

/// Component-local animation bindings loaded from an explicitly referenced
/// CTLD resource.
///
/// This data is kept beside mission component instances rather than on the
/// shared visual/model cache, because two instances of one mesh may use
/// different control resources.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedComponentControl {
    /// CTLD resource that supplied these bindings.
    pub source: ResourceKey,
    /// Number of nodes in the component model this control targets.
    pub model_node_count: usize,
    /// Enabled source rows in their original order, including duplicate nodes.
    pub bindings: Vec<PreparedControlNodeBinding>,
    /// Every raw 36-byte animation row in source order. The typed controller
    /// map refers to these original row indices, including disabled rows.
    pub timeline_rows: Vec<PreparedControlTimelineRow>,
    /// Parsed typed records retained for the native controller evaluator.
    pub typed_records: Vec<PreparedControlTypedRecord>,
    /// Explicit NDP sidecar and its validated rows, when resolved.
    pub node_data: Option<PreparedControlNodeDataTable>,
    /// Parsed native effect rows in this component's control resource.
    pub effect_section: PreparedControlEffectSection,
    /// Component-local points selected for parsed effect rows, when present.
    pub point_list: Option<PreparedControlPointList>,
}

/// Component-local BAS geometry loaded from an explicitly referenced BAS
/// resource. This remains aligned with each component instance, not the shared
/// model cache.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedComponentBas {
    /// BAS resource that supplied this geometry.
    pub source: ResourceKey,
    /// Decoded BAS point sections.
    pub geometry: BasGeometry,
}

/// Raw and decoded fields from one 36-byte CTLD animation row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedControlTimelineRow {
    /// Original row index, used by typed-record map values.
    pub row_index: usize,
    /// Target node index in the component model.
    pub node_index: usize,
    /// First animation endpoint.
    pub frame_a: f32,
    /// Second animation endpoint.
    pub frame_b: f32,
    /// Initial blend written by native Control during setup.
    pub initial_blend: f32,
    /// Native auxiliary words at row offsets +0x10, +0x14, +0x18, and +0x1C.
    pub raw_parameters: [u32; 4],
    /// Raw native row flags at +0x20.
    pub flags: u32,
}

/// One parsed 0xB0-byte typed-record body and its following value/string data.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedControlTypedRecord {
    /// Native record tag at body offset 0.
    pub tag: u32,
    /// All 44 raw little-endian words from the fixed 0xB0-byte body.
    pub fixed_words: [u32; 44],
    /// Raw values following the fixed body; tag-specific parsing is deferred.
    pub values: Vec<u32>,
    /// Raw bytes following the value array, excluding the required trailing NUL.
    pub string_bytes: Vec<u8>,
    /// Native tag-21 query provider resolved from the explicit NDP sidecar.
    /// Other tags use `NotApplicable`.
    pub gate_provider: PreparedControlGateProvider,
}

/// Initial tag-21 query provider resolution for one typed controller record.
#[derive(Clone, Debug, PartialEq)]
pub enum PreparedControlGateProvider {
    /// The record is not a tag-21 controller.
    NotApplicable,
    /// Its raw key selects a validated NDP row. Native Control initializes
    /// this type-1 query value to 1.0 before runtime state changes.
    Resolved {
        key: u32,
        ndp_source: ResourceKey,
        row_count: usize,
        initial_type1_value: f32,
    },
    /// No NDP reference was present, or its referenced archive/entry is absent.
    MissingResource {
        key: u32,
        ndp_source: Option<ResourceKey>,
    },
    /// The key is not a row in the validated NDP table.
    KeyOutOfRange {
        key: u32,
        ndp_source: ResourceKey,
        row_count: usize,
    },
}

/// Parsed NDP sidecar used as the native Control query-provider table.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedControlNodeDataTable {
    /// Explicit NDP resource paired with the component prototype.
    pub source: ResourceKey,
    /// Provider rows in their native table order.
    pub rows: Vec<PreparedControlNodeDataRow>,
}

/// One validated 76-byte NDP sidecar row.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedControlNodeDataRow {
    /// Raw native flag bits.
    pub raw_flags: u32,
    /// Maximum volume value at row offset +4.
    pub max_volume: f32,
    /// Mobility-per-volume value at row offset +8.
    pub mobility_per_volume: f32,
    /// Effect resource pair at row offset +12.
    pub effect: PreparedControlResourcePair,
}

/// Decoded point records selected by a component's Control configuration.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedControlPointList {
    /// CPT resource paired with the component control resource.
    pub source: ResourceKey,
    /// Point records in their original component-local order.
    pub points: Vec<PreparedControlPoint>,
}

/// One 36-byte native CPT point record and its parallel fixed-width name.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedControlPoint {
    /// Raw native flag bits.
    pub raw_flags: u32,
    /// Raw signed 32-bit word at byte offset +4 in the native record.
    pub raw_signed_field_04: i32,
    /// Raw signed 32-bit word at byte offset +8 in the native record.
    pub raw_signed_field_08: i32,
    /// Three stored coordinate values at byte offset +0x0C.
    pub position: [f32; 3],
    /// Three stored direction values at byte offset +0x18.
    pub direction: [f32; 3],
    /// Parallel raw 32-byte name entry, including NUL padding.
    pub name: [u8; 32],
}

/// Status of the trailing typed/effect data in a CTLD resource.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PreparedControlEffectSection {
    /// The resource advertises no trailing typed or effect data.
    Absent,
    /// The resource contains a layout this reader does not yet understand.
    /// Existing node bindings remain available in this case.
    Unsupported {
        /// Index of the first unparsed typed record, when one was present.
        typed_record_index: Option<usize>,
        /// Native tag on the first unparsed typed record, when available.
        typed_record_tag: Option<u32>,
    },
    /// Effect rows whose native fixed-row layout is understood.
    Parsed(Vec<PreparedControlEffectRow>),
}

/// One native type-4 effect row, with point indices kept relative to the
/// component-local CPT list selected by its control message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedControlEffectRow {
    /// Index of the enclosing fixed-size row table.
    pub table_index: usize,
    /// Index within the enclosing fixed-size row table.
    pub row_index: usize,
    /// The first three raw condition words consumed by the native checker.
    pub raw_condition: [u32; 3],
    /// Component-local offsets into the selected point list.
    pub point_offsets: [u32; 3],
    /// The raw native effect selector, including `u32::MAX` auto-selector.
    pub selector: u32,
    /// Raw fixed-width archive/resource strings from the row.
    pub resource: PreparedControlResourcePair,
}

/// Fixed-width resource strings carried by a native Control effect row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedControlResourcePair {
    /// Raw 32-byte archive field, including any trailing NUL padding.
    pub archive: [u8; 32],
    /// Raw 32-byte resource field, including any trailing NUL padding.
    pub name: [u8; 32],
}

/// One enabled CTLD row targeting a node in a component model.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedControlNodeBinding {
    /// Target node index in the component model.
    pub node_index: usize,
    /// First source animation endpoint, kept as an f32 value.
    pub frame_a: f32,
    /// Second source animation endpoint, kept as an f32 value.
    pub frame_b: f32,
    /// Initial source blend value.
    pub initial_blend: f32,
    /// Raw native row flags.
    pub flags: u32,
}

/// Per-component instance state for native tag-21 control animation.
///
/// Keep one value per model component instance; controls on shared models can
/// have independent clocks and gate samples.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ControlInstanceState {
    initialized: bool,
    node_times: Vec<Option<f32>>,
    tag21_tracks: Vec<ControlTag21InstanceState>,
}

#[derive(Clone, Debug, PartialEq)]
struct ControlTag21InstanceState {
    phase: f32,
    segment_start_ms: f32,
    segment_end_ms: f32,
    reciprocal_duration: f32,
    initialized: bool,
    maps: Vec<ControlTag21MapInstanceState>,
}

#[derive(Clone, Debug, PartialEq)]
struct ControlTag21MapInstanceState {
    row_index: usize,
    previous: f32,
    next: f32,
    current: f32,
}

impl Default for ControlTag21InstanceState {
    fn default() -> Self {
        Self {
            phase: 0.0,
            segment_start_ms: 0.0,
            segment_end_ms: 0.0,
            reciprocal_duration: 1.0,
            initialized: false,
            maps: Vec::new(),
        }
    }
}

/// Input or metadata prevented a safe native control sample.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlSamplingError {
    /// A state object was reused with a different model node count.
    StateNodeCountMismatch { expected: usize, actual: usize },
    /// A state object was reused with a different number of tag-21 records.
    StateTimelineCountMismatch { expected: usize, actual: usize },
    /// The caller did not provide one query-provider value per tag-21 record.
    GateProviderCountMismatch { expected: usize, actual: usize },
    /// A tag-21 record has no corresponding NDP provider resource.
    MissingGateProvider { record_index: usize, key: u32 },
    /// A tag-21 query key does not select a row in its NDP table.
    GateProviderKeyOutOfRange {
        record_index: usize,
        key: u32,
        row_count: usize,
    },
    /// A resolved provider lacks a finite current type-1 value.
    MissingGateProviderValue { record_index: usize, key: u32 },
    /// The record uses a tag-21 mode whose native selector is not yet decoded.
    UnsupportedTag21Mode { record_index: usize, packed: u32 },
    /// A tag-21 record did not retain a valid source-row map index.
    InvalidTag21MapIndex {
        record_index: usize,
        map_index: usize,
    },
    /// A mapped source row reuses the previous typed row's current value.
    ///
    /// This native flag is currently rejected so callers can use their static
    /// fallback instead of silently sampling the wrong row value.
    UnsupportedPreviousRowMapping { row_index: usize },
    /// A mapped raw source row has an invalid rate at +0x18.
    InvalidTransitionRate { row_index: usize },
    /// A mapped raw source row uses external variable selectors at +0x10/+0x14.
    UnsupportedVariableMapping { row_index: usize },
    /// The controller computation produced a non-finite sample.
    NonFiniteSample { record_index: usize },
}

/// Samples per-node animation times using the decoded Control timeline.
///
/// `native_clock_ms` is the absolute u32 millisecond clock passed to native
/// Control::Update. `gate_values` carries one provider type-1 float per tag-21
/// record, in source order. Native query 0x21820 returns 1 when that float is
/// zero, and the tag-21 update stops when that query returns 1; thus each
/// record updates when its provider value is nonzero. `None` values are
/// rejected explicitly. Values are not blend values. The state must be private
/// to one component instance and retained between calls. Rows and typed maps
/// are processed in source order, so later rows targeting one node win as in
/// native Control.
pub fn sample_control_node_times(
    control: &PreparedComponentControl,
    native_clock_ms: u32,
    gate_values: &[Option<f32>],
    state: &mut ControlInstanceState,
) -> Result<Vec<Option<f32>>, ControlSamplingError> {
    let node_count = control.model_node_count;
    let tag21_records: Vec<&PreparedControlTypedRecord> = control
        .typed_records
        .iter()
        .filter(|record| record.tag == 21)
        .collect();
    if gate_values.len() != tag21_records.len() {
        return Err(ControlSamplingError::GateProviderCountMismatch {
            expected: tag21_records.len(),
            actual: gate_values.len(),
        });
    }

    if state.initialized {
        if state.node_times.len() != node_count {
            return Err(ControlSamplingError::StateNodeCountMismatch {
                expected: node_count,
                actual: state.node_times.len(),
            });
        }
        if state.tag21_tracks.len() != tag21_records.len() {
            return Err(ControlSamplingError::StateTimelineCountMismatch {
                expected: tag21_records.len(),
                actual: state.tag21_tracks.len(),
            });
        }
    } else {
        state.node_times = vec![Some(0.0); node_count];
        for binding in &control.bindings {
            if binding.flags & 4 == 0 && binding.node_index < node_count {
                let blend = native_control_blend(binding.initial_blend, binding.flags);
                state.node_times[binding.node_index] =
                    Some((1.0 - blend) * binding.frame_a + blend * binding.frame_b);
            }
        }
        state.tag21_tracks = tag21_records
            .iter()
            .enumerate()
            .map(|(record_index, record)| {
                let mut track = ControlTag21InstanceState::default();
                for map_index in &record.values {
                    let row_index = usize::try_from(*map_index).unwrap_or(usize::MAX);
                    let Some(row) = control.timeline_rows.get(row_index) else {
                        return Err(ControlSamplingError::InvalidTag21MapIndex {
                            record_index,
                            map_index: row_index,
                        });
                    };
                    track.maps.push(ControlTag21MapInstanceState {
                        row_index,
                        previous: row.initial_blend,
                        next: row.initial_blend,
                        current: row.initial_blend,
                    });
                }
                Ok(track)
            })
            .collect::<Result<Vec<_>, _>>()?;
        state.initialized = true;
    }

    let now_ms = native_clock_ms as f32;
    for (record_index, record) in tag21_records.iter().enumerate() {
        let packed = record.fixed_words[2];
        let raw_flags = record.fixed_words[6];
        let controller_flags = if raw_flags == u32::MAX { 5 } else { raw_flags };
        if packed != 0 || controller_flags != 5 {
            return Err(ControlSamplingError::UnsupportedTag21Mode {
                record_index,
                packed,
            });
        }
        let coefficient = f32::from_bits(record.fixed_words[9]);
        let unused_coefficient = f32::from_bits(record.fixed_words[10]);
        if !coefficient.is_finite() || !unused_coefficient.is_finite() {
            return Err(ControlSamplingError::NonFiniteSample { record_index });
        }

        let key = record.fixed_words[1];
        match &record.gate_provider {
            PreparedControlGateProvider::Resolved {
                key: provider_key,
                row_count,
                ..
            } if *provider_key == key && usize::try_from(key).is_ok_and(|key| key < *row_count) => {
            }
            PreparedControlGateProvider::KeyOutOfRange {
                key: provider_key,
                row_count,
                ..
            } if *provider_key == key => {
                return Err(ControlSamplingError::GateProviderKeyOutOfRange {
                    record_index,
                    key,
                    row_count: *row_count,
                });
            }
            PreparedControlGateProvider::MissingResource {
                key: provider_key, ..
            } if *provider_key == key => {
                return Err(ControlSamplingError::MissingGateProvider { record_index, key });
            }
            _ => return Err(ControlSamplingError::MissingGateProvider { record_index, key }),
        }
        let provider_value = gate_values[record_index]
            .ok_or(ControlSamplingError::MissingGateProviderValue { record_index, key })?;
        if !provider_value.is_finite() {
            return Err(ControlSamplingError::MissingGateProviderValue { record_index, key });
        }

        let track = &mut state.tag21_tracks[record_index];
        let mut catch_up_steps = 0_usize;
        while !track.initialized || now_ms > track.segment_end_ms {
            if track.initialized {
                // 2D260 calls 21A30 before opening the next interval. Since
                // this loop runs only after `clock > end`, that sample has
                // reached every typed map's next value.
                for mapping in &mut track.maps {
                    mapping.current = mapping.next;
                }
            }
            if provider_value == 0.0 {
                // A closed native gate keeps the current value and rebases the
                // next transition to this absolute sample time.
                for mapping in &mut track.maps {
                    mapping.previous = mapping.current;
                    mapping.next = mapping.current;
                }
                track.segment_start_ms = now_ms;
                track.segment_end_ms = now_ms;
                track.reciprocal_duration = 1.0;
                track.initialized = true;
                break;
            }

            let direction = match controller_flags & 3 {
                1 => 1.0,
                2 => -1.0,
                _ => 0.0,
            };
            let mut next_phase = track.phase + coefficient * 0.45 * direction;
            if controller_flags & 0x0C == 4 {
                next_phase = wrap_control_value_once(next_phase);
            } else if controller_flags & 0x0C == 8 {
                next_phase = next_phase.clamp(0.0, 1.0);
            }
            let mut duration_ms = 0.0_f32;
            for mapping in &mut track.maps {
                let row = &control.timeline_rows[mapping.row_index];
                if row.flags & 0x40 != 0 {
                    return Err(ControlSamplingError::UnsupportedPreviousRowMapping {
                        row_index: mapping.row_index,
                    });
                }
                if row.raw_parameters[0] != u32::MAX || row.raw_parameters[1] != u32::MAX {
                    return Err(ControlSamplingError::UnsupportedVariableMapping {
                        row_index: mapping.row_index,
                    });
                }
                let rate = f32::from_bits(row.raw_parameters[2]);
                if !rate.is_finite() || rate <= 0.0 {
                    return Err(ControlSamplingError::InvalidTransitionRate {
                        row_index: mapping.row_index,
                    });
                }
                mapping.previous = mapping.current;
                mapping.next = next_phase;
                let direct_distance = (mapping.next - mapping.previous).abs();
                let distance = if row.flags & 1 != 0 {
                    shortest_wrapped_distance(mapping.previous, mapping.next)
                } else {
                    direct_distance
                };
                let transition_speed = coefficient.abs() * rate;
                let candidate_duration = if transition_speed > 0.0 {
                    1000.0 * distance / transition_speed
                } else {
                    100.0
                };
                if !candidate_duration.is_finite() {
                    return Err(ControlSamplingError::NonFiniteSample { record_index });
                }
                duration_ms = duration_ms.max(candidate_duration);
            }

            // Native Control only substitutes 100 ms when the proposed
            // interval is shorter than 1 ms; ordinary short transitions stay
            // short (for example 22.5 ms at rate 20).
            if duration_ms < 1.0 {
                duration_ms = 100.0;
            }
            track.segment_start_ms = track.segment_end_ms;
            track.phase = next_phase;
            track.segment_end_ms = track.segment_start_ms + duration_ms;
            let actual_duration = track.segment_end_ms - track.segment_start_ms;
            track.reciprocal_duration = if actual_duration > 0.0 {
                1.0 / actual_duration
            } else {
                1.0
            };
            track.initialized = true;
            catch_up_steps += 1;
            if catch_up_steps > 100_000 {
                // shortcut: cap catch-up at 100,000 transitions; replace with a bounded
                // fast-forward if valid long clock stalls must reproduce every transition.
                return Err(ControlSamplingError::NonFiniteSample { record_index });
            }
        }

        let duration = track.segment_end_ms - track.segment_start_ms;
        let alpha = if duration > 0.0 {
            ((now_ms - track.segment_start_ms) * track.reciprocal_duration).clamp(0.0, 1.0)
        } else {
            1.0
        };
        for mapping in &mut track.maps {
            let row = &control.timeline_rows[mapping.row_index];
            if row.flags & 4 != 0 || row.node_index >= node_count {
                continue;
            }
            let mut delta = mapping.next - mapping.previous;
            if row.flags & 1 != 0 {
                if delta > 0.5 {
                    delta -= 1.0;
                } else if delta < -0.5 {
                    delta += 1.0;
                }
                mapping.current = wrap_control_value_once(mapping.previous + delta * alpha);
            } else {
                mapping.current = (mapping.previous + delta * alpha).clamp(0.0, 1.0);
            }
            if !mapping.current.is_finite() {
                return Err(ControlSamplingError::NonFiniteSample { record_index });
            }
            let blend = native_control_blend(mapping.current, row.flags);
            let time = (1.0 - blend) * row.frame_a + blend * row.frame_b;
            if !time.is_finite() || time < 0.0 {
                return Err(ControlSamplingError::NonFiniteSample { record_index });
            }
            state.node_times[row.node_index] = Some(time);
        }
    }

    Ok(state.node_times.clone())
}

fn shortest_wrapped_distance(previous: f32, next: f32) -> f32 {
    let direct = (next - previous).abs();
    direct.min((1.0 - direct).abs())
}

fn wrap_control_value_once(mut value: f32) -> f32 {
    if value > 1.0 {
        value -= 1.0;
    }
    if value < 0.0 {
        value += 1.0;
    }
    value
}

fn native_control_blend(mut blend: f32, flags: u32) -> f32 {
    if flags & 1 != 0 {
        if blend > 1.0 {
            blend -= 1.0;
        }
        if blend < 0.0 {
            blend += 1.0;
        }
    } else {
        blend = blend.clamp(0.0, 1.0);
    }
    if flags & 2 != 0 {
        blend = 1.0 - blend;
    }
    blend
}

/// CPU-side validated model ready for a renderer upload path.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedModel {
    /// Stable id derived from the visual source.
    pub id: AssetId<PreparedModel>,
    /// Source mesh resource.
    pub source: ResourceKey,
    /// Fully validated model payload.
    pub validated: ModelAsset,
    /// Mesh dependencies that led to this prepared model.
    pub mesh_dependencies: Vec<ResourceKey>,
}

/// CPU-side WEAR table resolved for a visual.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedWear {
    /// Stable id derived from the visual source.
    pub id: AssetId<PreparedWear>,
    /// Source WEAR resource.
    pub source: ResourceKey,
    /// Decoded WEAR table.
    pub table: WearTable,
}

/// CPU-side data needed before a material can be handed to a renderer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedMaterial {
    /// Stable id derived from the visual and material selector.
    pub id: AssetId<PreparedMaterial>,
    /// Source MAT0 resource.
    pub source: ResourceKey,
    /// Parsed material key retained for compatibility with older callers.
    pub name: ResourceName,
    /// Raw NRes MAT0 entry flags retained for runtime pipeline selection.
    pub attr1: u32,
    /// Native material pipeline category `(attr1 >> 2) & 0xF`.
    pub pipeline_category: u8,
    /// Native material feature bit `attr1 & 0x40`.
    pub native_flag_40: bool,
    /// Decoded MAT0 payload.
    pub mat0: Mat0Document,
    /// Texture requests declared by MAT0 phases.
    pub texture_requests: Vec<ResourceName>,
    /// Lightmap requests associated with the owning WEAR table.
    pub lightmap_requests: Vec<ResourceName>,
}

/// Texture usage role inside a prepared visual.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreparedTextureUsage {
    /// Standard diffuse/albedo texture.
    Diffuse,
    /// Lightmap texture.
    Lightmap,
}

/// CPU-side TEXM texture ready for a renderer upload path.
#[derive(Clone, Debug)]
pub struct PreparedTexture {
    /// Stable id derived from the texture source and usage.
    pub id: AssetId<PreparedTexture>,
    /// Source TEXM resource.
    pub source: ResourceKey,
    /// Decoded TEXM payload.
    pub texm: TexmDocument,
    /// Usage role in the prepared visual.
    pub usage: PreparedTextureUsage,
}

impl PreparedTexture {
    /// Decodes one prepared texture mip into the renderer upload representation.
    ///
    /// # Errors
    ///
    /// Returns the original TEXM decode error when the requested mip is invalid
    /// or malformed.
    pub fn decode_mip_rgba8(&self, level: u32) -> Result<RgbaImage, TexmError> {
        decode_mip_rgba8(&self.texm, level)
    }
}

impl PreparedVisual {
    /// Returns the primary material id, if any.
    #[must_use]
    pub fn primary_material_id(&self) -> Option<AssetId<PreparedMaterial>> {
        self.material_ids.first().copied()
    }
}

/// Immutable prepared mission assets for rendering and game setup.
#[derive(Clone, Debug, Default)]
pub struct MissionAssets {
    /// Mesh-backed models prepared for reachable visuals.
    pub models: Vec<PreparedModel>,
    /// WEAR tables prepared for reachable visuals.
    pub wears: Vec<PreparedWear>,
    /// MAT0 materials prepared for reachable visuals.
    pub materials: Vec<PreparedMaterial>,
    /// TEXM textures prepared for reachable visuals.
    pub textures: Vec<PreparedTexture>,
    /// Visuals prepared for all reachable prototype requests.
    pub visuals: Vec<PreparedVisual>,
    /// Visual ids available for each mission object index.
    pub object_visuals: Vec<Vec<AssetId<PreparedVisual>>>,
    /// Optional component-local controls aligned with each object's visual
    /// component list. `None` means no CTLD pose override is available because
    /// there is no explicit reference or its referenced archive/entry is absent.
    pub object_component_controls: Vec<Vec<Option<PreparedComponentControl>>>,
    /// Optional component-local BAS geometry aligned with each object's visual
    /// component list. `None` means there is no explicit BAS reference or its
    /// archive/entry is absent.
    pub object_component_bas: Vec<Vec<Option<PreparedComponentBas>>>,
}

impl MissionAssets {
    /// Returns how many visuals were prepared.
    #[must_use]
    pub fn visual_count(&self) -> usize {
        self.visuals.len()
    }

    /// Returns all visuals for a mission object index.
    #[must_use]
    pub fn visuals_for_object(&self, object_index: usize) -> &[AssetId<PreparedVisual>] {
        self.object_visuals
            .get(object_index)
            .map_or(&[], |values| values.as_slice())
    }

    /// Returns the first visual for a mission object index.
    #[must_use]
    pub fn visual_for_object(&self, object_index: usize) -> Option<AssetId<PreparedVisual>> {
        self.visuals_for_object(object_index).first().copied()
    }

    /// Finds a visual by prepared id.
    #[must_use]
    pub fn visual_by_id(&self, id: AssetId<PreparedVisual>) -> Option<&PreparedVisual> {
        self.visuals.iter().find(|visual| visual.id == id)
    }

    /// Finds a prepared model by id.
    #[must_use]
    pub fn model_by_id(&self, id: AssetId<PreparedModel>) -> Option<&PreparedModel> {
        self.models.iter().find(|model| model.id == id)
    }

    /// Finds a prepared material by id.
    #[must_use]
    pub fn material_by_id(&self, id: AssetId<PreparedMaterial>) -> Option<&PreparedMaterial> {
        self.materials.iter().find(|material| material.id == id)
    }

    /// Finds a prepared texture by id.
    #[must_use]
    pub fn texture_by_id(&self, id: AssetId<PreparedTexture>) -> Option<&PreparedTexture> {
        self.textures.iter().find(|texture| texture.id == id)
    }

    /// Converts mission assets into a coarse mission plan.
    #[must_use]
    pub fn to_plan(&self) -> MissionAssetPlan {
        MissionAssetPlan {
            visual_count: self.visuals.len(),
            model_count: self.models.len(),
            material_count: self.materials.len(),
            texture_count: self
                .textures
                .iter()
                .filter(|texture| texture.usage == PreparedTextureUsage::Diffuse)
                .count(),
            lightmap_count: self
                .textures
                .iter()
                .filter(|texture| texture.usage == PreparedTextureUsage::Lightmap)
                .count(),
        }
    }
}

/// A transactional mission asset preparation plan.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MissionAssetPlan {
    /// Number of visual prototypes in the plan.
    pub visual_count: usize,
    /// Number of mesh-backed visuals.
    pub model_count: usize,
    /// Number of material slot requests.
    pub material_count: usize,
    /// Number of texture phase requests.
    pub texture_count: usize,
    /// Number of lightmap requests.
    pub lightmap_count: usize,
}

/// Bounded CPU-side asset preparation limits enforced before renderer upload.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AssetPreparationLimits {
    /// Maximum number of unique prepared models.
    pub max_models: Option<usize>,
    /// Maximum number of unique prepared WEAR tables.
    pub max_wears: Option<usize>,
    /// Maximum number of unique prepared materials.
    pub max_materials: Option<usize>,
    /// Maximum number of unique prepared textures, including lightmaps.
    pub max_textures: Option<usize>,
    /// Maximum sum of unique texture base-level pixels.
    pub max_texture_pixels: Option<u64>,
}

/// Summary emitted by bounded asset preparation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AssetPreparationReport {
    /// Number of unique prepared models.
    pub model_count: usize,
    /// Number of unique prepared WEAR tables.
    pub wear_count: usize,
    /// Number of unique prepared materials.
    pub material_count: usize,
    /// Number of unique prepared textures, including lightmaps.
    pub texture_count: usize,
    /// Sum of unique texture base-level pixels.
    pub texture_pixels: u64,
}

/// Errors raised while preparing CPU-side assets.
#[derive(Debug)]
pub enum AssetError {
    /// A required cross-resource dependency was not found.
    MissingDependency(String),
    /// A prototype did not describe a usable visual.
    InvalidPrototype(String),
    /// Asset preparation exceeded explicit limits.
    BudgetExceeded(String),
    /// A repository operation failed.
    Resource {
        /// Human context for the operation.
        context: String,
        /// Concrete repository source error.
        source: Box<ResourceError>,
    },
    /// MSH parsing or validation failed.
    Msh(MshError),
    /// WEAR/MAT0 parsing or resolution failed.
    Material(MaterialError),
    /// TEXM parsing failed.
    Texture(TexmError),
    /// `NRes` decoding failed.
    Nres(NresError),
}

impl fmt::Display for AssetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingDependency(value) => write!(f, "missing dependency: {value}"),
            Self::InvalidPrototype(value) => write!(f, "invalid prototype: {value}"),
            Self::BudgetExceeded(value) => write!(f, "asset budget exceeded: {value}"),
            Self::Resource { context, source } => {
                if context.is_empty() {
                    write!(f, "resource error: {source}")
                } else {
                    write!(f, "resource error ({context}): {source}")
                }
            }
            Self::Msh(source) => write!(f, "msh error: {source}"),
            Self::Material(source) => write!(f, "material error: {source}"),
            Self::Texture(source) => write!(f, "texture error: {source}"),
            Self::Nres(source) => write!(f, "nres error: {source}"),
        }
    }
}

impl std::error::Error for AssetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Resource { source, .. } => Some(source.as_ref()),
            Self::Msh(source) => Some(source),
            Self::Material(source) => Some(source),
            Self::Texture(source) => Some(source),
            Self::Nres(source) => Some(source),
            Self::MissingDependency(_) | Self::InvalidPrototype(_) | Self::BudgetExceeded(_) => {
                None
            }
        }
    }
}

/// Port implemented by typed asset loaders.
pub trait AssetLoader<T> {
    /// Loads an asset for the given resource key.
    ///
    /// # Errors
    ///
    /// Returns [`AssetError`] when the resource cannot be resolved or decoded.
    fn load(&self, key: &ResourceKey) -> Result<Arc<T>, AssetError>;
}

/// Minimal asset manager façade over an immutable resource repository.
#[derive(Debug)]
pub struct AssetManager<R> {
    repository: R,
}

impl<R> AssetManager<R> {
    /// Creates a manager backed by the given repository.
    #[must_use]
    pub const fn new(repository: R) -> Self {
        Self { repository }
    }

    /// Returns the backing repository.
    #[must_use]
    pub const fn repository(&self) -> &R {
        &self.repository
    }
}

impl<R: ResourceRepository> AssetManager<R> {
    /// Prepares one prototype visual using the manager repository.
    ///
    /// # Errors
    ///
    /// Returns [`AssetError`] if any model, material, texture, or lightmap
    /// dependency is missing or malformed.
    pub fn prepare_visual(&self, proto: &EffectivePrototype) -> Result<PreparedVisual, AssetError> {
        prepare_visual_with_repository(&self.repository, proto)
    }

    /// Builds mission assets from resolved prototypes.
    ///
    /// # Errors
    ///
    /// Returns [`AssetError`] if any visual dependency is missing or malformed.
    pub fn prepare_mission_assets(
        &self,
        root_prototype_spans: &[std::ops::Range<usize>],
        prototypes: &[EffectivePrototype],
    ) -> Result<MissionAssets, AssetError> {
        prepare_mission_assets_with_repository(&self.repository, root_prototype_spans, prototypes)
    }

    /// Builds mission assets together with a bounded preparation report.
    ///
    /// # Errors
    ///
    /// Returns [`AssetError`] if any visual dependency is missing or malformed,
    /// or when explicit limits are exceeded.
    pub fn prepare_mission_assets_profiled(
        &self,
        root_prototype_spans: &[std::ops::Range<usize>],
        prototypes: &[EffectivePrototype],
        limits: AssetPreparationLimits,
    ) -> Result<(MissionAssets, AssetPreparationReport), AssetError> {
        prepare_mission_assets_profiled_with_repository(
            &self.repository,
            root_prototype_spans,
            prototypes,
            limits,
        )
    }

    /// Builds a mission plan by preparing each resolved prototype.
    ///
    /// # Errors
    ///
    /// Returns [`AssetError`] if any visual dependency is missing or malformed.
    pub fn build_mission_asset_plan(
        &self,
        prototypes: &[EffectivePrototype],
    ) -> Result<MissionAssetPlan, AssetError> {
        build_mission_asset_plan_with_repository(&self.repository, prototypes)
    }
}

/// Produces a count-only plan from a prototype graph.
#[must_use]
pub fn build_mission_asset_plan(graph: &PrototypeGraph) -> MissionAssetPlan {
    let visual_count = graph
        .nodes
        .iter()
        .filter(|node| node.kind == PrototypeGraphNodeKind::Prototype)
        .count();
    MissionAssetPlan {
        visual_count,
        ..MissionAssetPlan::default()
    }
}

/// Builds a fully validated CPU-side mission asset plan.
///
/// # Errors
///
/// Returns [`AssetError`] if any reachable visual dependency is missing or
/// malformed.
pub fn build_mission_asset_plan_with_repository<R: ResourceRepository>(
    repository: &R,
    prototypes: &[EffectivePrototype],
) -> Result<MissionAssetPlan, AssetError> {
    let full_span = 0..prototypes.len();
    let mission_assets = prepare_mission_assets_with_repository(
        repository,
        std::slice::from_ref(&full_span),
        prototypes,
    )?;
    Ok(mission_assets.to_plan())
}

/// Builds immutable mission assets from resolved prototypes.
///
/// # Errors
///
/// Returns [`AssetError`] if any visual dependency is missing or malformed.
pub fn prepare_mission_assets_with_repository<R: ResourceRepository>(
    repository: &R,
    root_prototype_spans: &[std::ops::Range<usize>],
    prototypes: &[EffectivePrototype],
) -> Result<MissionAssets, AssetError> {
    Ok(prepare_mission_assets_profiled_with_repository(
        repository,
        root_prototype_spans,
        prototypes,
        AssetPreparationLimits::default(),
    )?
    .0)
}

/// Builds mission assets while enforcing explicit preparation limits.
///
/// # Errors
///
/// Returns [`AssetError`] if any visual dependency is missing or malformed, or
/// when explicit limits are exceeded.
pub fn prepare_mission_assets_profiled_with_repository<R: ResourceRepository>(
    repository: &R,
    root_prototype_spans: &[std::ops::Range<usize>],
    prototypes: &[EffectivePrototype],
    limits: AssetPreparationLimits,
) -> Result<(MissionAssets, AssetPreparationReport), AssetError> {
    prepare_mission_assets_with_repository_internal(
        repository,
        root_prototype_spans,
        prototypes,
        AssetIdentityPolicy::default(),
        limits,
    )
}

// This orchestration intentionally keeps preparation, deduplication, and limit enforcement
// together so all fallible asset construction remains transactional.

fn prepare_mission_assets_with_repository_internal<R: ResourceRepository>(
    repository: &R,
    root_prototype_spans: &[std::ops::Range<usize>],
    prototypes: &[EffectivePrototype],
    identity_policy: AssetIdentityPolicy,
    limits: AssetPreparationLimits,
) -> Result<(MissionAssets, AssetPreparationReport), AssetError> {
    if prototypes.is_empty() {
        return Ok((MissionAssets::default(), AssetPreparationReport::default()));
    }
    let mut visual_index_by_id: HashMap<AssetId<PreparedVisual>, PreparedVisualSignature> =
        HashMap::new();
    let mut model_index_by_id: HashMap<AssetId<PreparedModel>, PreparedModelSignature> =
        HashMap::new();
    let mut wear_index_by_id: HashMap<AssetId<PreparedWear>, PreparedWearSignature> =
        HashMap::new();
    let mut material_index_by_id: HashMap<AssetId<PreparedMaterial>, PreparedMaterialSignature> =
        HashMap::new();
    let mut texture_index_by_id: HashMap<AssetId<PreparedTexture>, PreparedTextureSignature> =
        HashMap::new();
    let mut models = Vec::new();
    let mut wears = Vec::new();
    let mut materials = Vec::new();
    let mut textures = Vec::new();
    let mut visuals = Vec::new();
    let mut prototype_visual_ids = Vec::with_capacity(prototypes.len());
    let mut preparation_cache = PreparationCache::default();

    for proto in prototypes {
        let visual_id = AssetId::new((identity_policy.visual)(proto));
        let signature = prepared_visual_signature(proto);
        match visual_index_by_id.get(&visual_id) {
            Some(existing) if existing != &signature => {
                return Err(AssetError::InvalidPrototype(
                    "stable visual id collision between unrelated prototypes".to_string(),
                ));
            }
            Some(_) => {}
            None => {
                visual_index_by_id.insert(visual_id, signature);
                let bundle = prepare_visual_with_repository_internal(
                    repository,
                    proto,
                    identity_policy,
                    &mut preparation_cache,
                )?;
                if bundle.visual.id != visual_id {
                    // Defensive check. stable IDs are deterministic for the same inputs.
                    return Err(AssetError::InvalidPrototype(
                        "prepared visual id changed during preparation".to_string(),
                    ));
                }
                if let Some(model) = bundle.model {
                    if insert_asset_signature(
                        &mut model_index_by_id,
                        model.id,
                        prepared_model_signature(&model),
                        "model",
                    )? {
                        models.push(model);
                    }
                }
                if let Some(wear) = bundle.wear {
                    if insert_asset_signature(
                        &mut wear_index_by_id,
                        wear.id,
                        prepared_wear_signature(&wear),
                        "wear",
                    )? {
                        wears.push(wear);
                    }
                }
                for material in bundle.materials {
                    if insert_asset_signature(
                        &mut material_index_by_id,
                        material.id,
                        prepared_material_signature(&material),
                        "material",
                    )? {
                        materials.push(material);
                    }
                }
                for texture in bundle.textures {
                    if insert_asset_signature(
                        &mut texture_index_by_id,
                        texture.id,
                        prepared_texture_signature(&texture),
                        "texture",
                    )? {
                        textures.push(texture);
                    }
                }
                visuals.push(bundle.visual);
            }
        }
        prototype_visual_ids.push(visual_id);
    }

    let mut prototype_component_controls = Vec::with_capacity(prototypes.len());
    let mut prototype_component_bas = Vec::with_capacity(prototypes.len());
    for (prototype_index, prototype) in prototypes.iter().enumerate() {
        let bas = match prototype.bas_resource.clone() {
            Some(source) => {
                prepare_component_bas_from_resource(repository, source, prototype_index)?
            }
            None => None,
        };
        prototype_component_bas.push(bas);
        let Some(source) = prototype.control_resource.clone() else {
            prototype_component_controls.push(None);
            continue;
        };
        let PrototypeGeometry::Mesh(mesh_key) = &prototype.geometry else {
            prototype_component_controls.push(None);
            continue;
        };
        let model = models
            .iter()
            .find(|model| model.source == *mesh_key)
            .ok_or_else(|| {
                AssetError::InvalidPrototype(format!(
                    "prototype {} references control resource {}:{} but its prepared model is missing",
                    String::from_utf8_lossy(&prototype.key.0.0),
                    source.archive.as_str(),
                    String::from_utf8_lossy(&source.name.0),
                ))
            })?;
        let control = prepare_component_control_from_resource(
            repository,
            source,
            prototype.point_resource.clone(),
            prototype.node_data_resource.clone(),
            model.validated.node_count,
            prototype_index,
        )?;
        prototype_component_controls.push(control);
    }

    let mut object_visuals = Vec::with_capacity(root_prototype_spans.len());
    let mut object_component_controls = Vec::with_capacity(root_prototype_spans.len());
    let mut object_component_bas = Vec::with_capacity(root_prototype_spans.len());
    for (root_index, span) in root_prototype_spans.iter().enumerate() {
        if span.start > span.end || span.end > prototype_visual_ids.len() {
            return Err(AssetError::InvalidPrototype(format!(
                "invalid prototype span for mission object {root_index}: {span:?}"
            )));
        }
        let mut ids = Vec::new();
        let mut controls = Vec::new();
        let mut bases = Vec::new();
        for index in span.clone() {
            let visual_id = prototype_visual_ids[index];
            // A unit DAT is an instance tree.  Reusing one prepared visual
            // must not collapse two component instances (for example the two
            // identical cannons on w_m_wlk2).
            ids.push(visual_id);
            controls.push(prototype_component_controls[index].clone());
            bases.push(prototype_component_bas[index].clone());
        }
        object_visuals.push(ids);
        object_component_controls.push(controls);
        object_component_bas.push(bases);
    }

    let assets = MissionAssets {
        models,
        wears,
        materials,
        textures,
        visuals,
        object_visuals,
        object_component_controls,
        object_component_bas,
    };
    let report = AssetPreparationReport {
        model_count: assets.models.len(),
        wear_count: assets.wears.len(),
        material_count: assets.materials.len(),
        texture_count: assets.textures.len(),
        texture_pixels: assets
            .textures
            .iter()
            .map(|texture| u64::from(texture.texm.width()) * u64::from(texture.texm.height()))
            .sum(),
    };
    enforce_asset_preparation_limits(&report, limits)?;
    Ok((assets, report))
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PreparedVisualSignature {
    Mesh {
        archive: Vec<u8>,
        name: Vec<u8>,
        type_id: Option<u32>,
        source_type: Option<u32>,
        dependency_count: usize,
    },
    NonGeometric {
        source_type: Option<u32>,
        dependency_count: usize,
    },
}

#[derive(Clone, Copy)]
struct AssetIdentityPolicy {
    visual: fn(&EffectivePrototype) -> u64,
    model: fn(&EffectivePrototype) -> u64,
    wear: fn(&EffectivePrototype) -> u64,
    material: fn(&EffectivePrototype, u16, &ResourceName) -> u64,
    texture: fn(&ResourceKey, PreparedTextureUsage) -> u64,
}

impl Default for AssetIdentityPolicy {
    fn default() -> Self {
        Self {
            visual: stable_visual_id,
            model: stable_model_id,
            wear: stable_wear_id,
            material: stable_material_id,
            texture: stable_texture_id,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ResourceSignature {
    archive: Vec<u8>,
    name: Vec<u8>,
    type_id: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PreparedModelSignature {
    source: ResourceSignature,
    node_count: usize,
    slot_count: usize,
    batch_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PreparedWearSignature {
    source: ResourceSignature,
    material_slots: usize,
    lightmap_slots: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PreparedMaterialSignature {
    source: ResourceSignature,
    attr1: u32,
    pipeline_category: u8,
    native_flag_40: bool,
    texture_requests: Vec<Vec<u8>>,
    lightmap_requests: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PreparedTextureSignature {
    source: ResourceSignature,
    usage: PreparedTextureUsage,
    width: u32,
    height: u32,
    mip_count: usize,
}

fn prepared_visual_signature(proto: &EffectivePrototype) -> PreparedVisualSignature {
    match &proto.geometry {
        PrototypeGeometry::Mesh(key) => PreparedVisualSignature::Mesh {
            archive: key.archive.identity_bytes().to_vec(),
            name: key.name.0.clone(),
            type_id: key.type_id,
            source_type: proto.source_type,
            dependency_count: proto.dependencies.len(),
        },
        PrototypeGeometry::NonGeometric => PreparedVisualSignature::NonGeometric {
            source_type: proto.source_type,
            dependency_count: proto.dependencies.len(),
        },
    }
}

fn resource_signature(key: &ResourceKey) -> ResourceSignature {
    ResourceSignature {
        archive: key.archive.identity_bytes().to_vec(),
        name: key.name.0.clone(),
        type_id: key.type_id,
    }
}

fn prepared_model_signature(model: &PreparedModel) -> PreparedModelSignature {
    PreparedModelSignature {
        source: resource_signature(&model.source),
        node_count: model.validated.node_count,
        slot_count: model.validated.slots.len(),
        batch_count: model.validated.batches.len(),
    }
}

fn prepared_wear_signature(wear: &PreparedWear) -> PreparedWearSignature {
    PreparedWearSignature {
        source: resource_signature(&wear.source),
        material_slots: wear.table.entries.len(),
        lightmap_slots: wear.table.lightmaps.len(),
    }
}

fn prepared_material_signature(material: &PreparedMaterial) -> PreparedMaterialSignature {
    PreparedMaterialSignature {
        source: resource_signature(&material.source),
        attr1: material.attr1,
        pipeline_category: material.pipeline_category,
        native_flag_40: material.native_flag_40,
        texture_requests: material
            .texture_requests
            .iter()
            .map(|name| name.0.clone())
            .collect(),
        lightmap_requests: material
            .lightmap_requests
            .iter()
            .map(|name| name.0.clone())
            .collect(),
    }
}

fn prepared_texture_signature(texture: &PreparedTexture) -> PreparedTextureSignature {
    PreparedTextureSignature {
        source: resource_signature(&texture.source),
        usage: texture.usage,
        width: texture.texm.width(),
        height: texture.texm.height(),
        mip_count: texture.texm.mip_count(),
    }
}

fn insert_asset_signature<T, S>(
    signatures: &mut HashMap<AssetId<T>, S>,
    id: AssetId<T>,
    signature: S,
    label: &str,
) -> Result<bool, AssetError>
where
    S: Eq,
{
    match signatures.entry(id) {
        Entry::Occupied(existing) => {
            if existing.get() != &signature {
                return Err(AssetError::InvalidPrototype(format!(
                    "stable {label} id collision between unrelated assets"
                )));
            }
            Ok(false)
        }
        Entry::Vacant(entry) => {
            entry.insert(signature);
            Ok(true)
        }
    }
}

fn enforce_asset_preparation_limits(
    report: &AssetPreparationReport,
    limits: AssetPreparationLimits,
) -> Result<(), AssetError> {
    if let Some(limit) = limits.max_models {
        if report.model_count > limit {
            return Err(AssetError::BudgetExceeded(format!(
                "models={} exceeds limit={limit}",
                report.model_count
            )));
        }
    }
    if let Some(limit) = limits.max_wears {
        if report.wear_count > limit {
            return Err(AssetError::BudgetExceeded(format!(
                "wears={} exceeds limit={limit}",
                report.wear_count
            )));
        }
    }
    if let Some(limit) = limits.max_materials {
        if report.material_count > limit {
            return Err(AssetError::BudgetExceeded(format!(
                "materials={} exceeds limit={limit}",
                report.material_count
            )));
        }
    }
    if let Some(limit) = limits.max_textures {
        if report.texture_count > limit {
            return Err(AssetError::BudgetExceeded(format!(
                "textures={} exceeds limit={limit}",
                report.texture_count
            )));
        }
    }
    if let Some(limit) = limits.max_texture_pixels {
        if report.texture_pixels > limit {
            return Err(AssetError::BudgetExceeded(format!(
                "texture_pixels={} exceeds limit={limit}",
                report.texture_pixels
            )));
        }
    }
    Ok(())
}

/// Extends a prototype dependency report with visual dependency failures.
///
/// This function validates WEAR/material/TEXM/LIGHTMAP resolution for each resolved
/// prototype without constructing full immutable assets.
///
/// # Panics
///
/// Panics only if the hard-coded, host-compatible `material.lib` path stops satisfying
/// the path policy, which indicates a programming error in this module.
// The graph expansion is deliberately a single transactional traversal so every generated
// edge receives the correct parent provenance and node/edge sequence.

pub fn extend_graph_report_with_visual_dependencies<R: ResourceRepository>(
    repository: &R,
    report: &mut PrototypeGraphReport,
    graph: &mut PrototypeGraph,
    prototypes: &[EffectivePrototype],
) {
    if graph.visual_dependencies_expanded {
        return;
    }

    let material_archive = parse_path("material.lib")
        .expect("static material archive path must satisfy host-compatible normalization");
    let mut next_node = graph
        .nodes
        .iter()
        .map(|node| node.id.0)
        .max()
        .map_or(0, |value| value.saturating_add(1));
    let mut next_edge = graph
        .edges
        .iter()
        .map(|edge| edge.id.0)
        .max()
        .map_or(0, |value| value.saturating_add(1));
    let mut diffuse_texture_validation = HashMap::new();
    let mut lightmap_texture_validation = HashMap::new();
    let mut material_validation = HashMap::new();
    let mut wear_validation = HashMap::new();
    // Capture all base-graph anchors before this traversal appends visual
    // resources and edges. Looking them up again from the growing graph would
    // make each later prototype scan an ever larger collection.
    let prototype_mesh_anchors: Vec<_> = (0..prototypes.len())
        .map(|prototype_index| {
            let prototype_node_id = prototype_node_id(graph, prototype_index)?;
            let mesh_node_id = prototype_mesh_node_id(graph, prototype_node_id)?;
            Some((mesh_node_id, mesh_edge_id(graph, prototype_node_id)))
        })
        .collect();

    for (prototype_index, prototype) in prototypes.iter().enumerate() {
        let PrototypeGeometry::Mesh(mesh) = &prototype.geometry else {
            continue;
        };
        report.wear_request_count += 1;
        let Some((mesh_node_id, mesh_parent_edge)) = prototype_mesh_anchors
            .get(prototype_index)
            .copied()
            .flatten()
        else {
            continue;
        };
        let root_index = root_index_for_prototype(graph, prototype_index);
        let unit_component_index =
            mesh_parent_edge.and_then(|edge| unit_component_index_for_edge(graph, edge));

        match resolve_wear_table_cached(repository, mesh, &mut wear_validation) {
            Ok(table) => {
                report.wear_resolved_count += 1;
                let wear_key = match wear_resource_key(mesh) {
                    Ok(key) => key,
                    Err(message) => {
                        push_visual_failure(
                            report,
                            graph,
                            prototype_index,
                            mesh.name.0.clone(),
                            PrototypeGraphEdge::MeshToWear,
                            PrototypeGraphRequiredness::Required,
                            &message.to_string(),
                        );
                        continue;
                    }
                };
                let wear_node_id = push_graph_resource_node(
                    graph,
                    PrototypeGraphNodeKind::WearResource,
                    wear_key.clone(),
                    &mut next_node,
                );
                let wear_edge_id = push_graph_edge(
                    graph,
                    mesh_node_id,
                    wear_node_id,
                    fparkan_prototype::PrototypeGraphEdgeKind::MeshToWear,
                    PrototypeGraphRequiredness::Required,
                    Some(provenance_for_resource(
                        root_index,
                        mesh_parent_edge,
                        unit_component_index,
                        &wear_key,
                    )),
                    &mut next_edge,
                );
                report.material_slot_count += table.entries.len();
                for (material_index, _entry) in table.entries.iter().enumerate() {
                    let Ok(material_index) = u16::try_from(material_index) else {
                        push_visual_failure(
                            report,
                            graph,
                            prototype_index,
                            mesh.name.0.clone(),
                            PrototypeGraphEdge::WearToMaterial,
                            PrototypeGraphRequiredness::Required,
                            "material index does not fit archive format",
                        );
                        continue;
                    };
                    match resolve_material_validation_cached(
                        repository,
                        &table,
                        material_index,
                        &mut material_validation,
                    ) {
                        Ok(material) => {
                            report.material_resolved_count += 1;
                            let material_key = ResourceKey {
                                archive: material_archive.clone(),
                                name: material.name.clone(),
                                type_id: Some(MAT0_KIND),
                            };
                            let material_node_id = push_graph_resource_node(
                                graph,
                                PrototypeGraphNodeKind::MaterialResource,
                                material_key.clone(),
                                &mut next_node,
                            );
                            let material_edge_id = push_graph_edge(
                                graph,
                                wear_node_id,
                                material_node_id,
                                fparkan_prototype::PrototypeGraphEdgeKind::WearToMaterial,
                                PrototypeGraphRequiredness::Required,
                                Some(provenance_for_resource(
                                    root_index,
                                    Some(wear_edge_id),
                                    unit_component_index,
                                    &material_key,
                                )),
                                &mut next_edge,
                            );
                            for texture in material.document.texture_requests() {
                                report.texture_request_count += 1;
                                match resolve_texm_validation_cached(
                                    repository,
                                    &texture,
                                    TEXTURES_ARCHIVE,
                                    "texture",
                                    &mut diffuse_texture_validation,
                                ) {
                                    Ok(()) => {
                                        report.texture_resolved_count += 1;
                                        if let Ok(texture_key) =
                                            texm_resource_key(TEXTURES_ARCHIVE, &texture)
                                        {
                                            let texture_node_id = push_graph_resource_node(
                                                graph,
                                                PrototypeGraphNodeKind::TextureResource,
                                                texture_key.clone(),
                                                &mut next_node,
                                            );
                                            push_graph_edge(
                                                graph,
                                                material_node_id,
                                                texture_node_id,
                                                fparkan_prototype::PrototypeGraphEdgeKind::MaterialToTexture,
                                                PrototypeGraphRequiredness::Required,
                                                Some(provenance_for_resource(
                                                    root_index,
                                                    Some(material_edge_id),
                                                    unit_component_index,
                                                    &texture_key,
                                                )),
                                                &mut next_edge,
                                            );
                                        }
                                    }
                                    Err(message) => {
                                        push_visual_failure(
                                            report,
                                            graph,
                                            prototype_index,
                                            texture.0,
                                            PrototypeGraphEdge::MaterialToTexture,
                                            PrototypeGraphRequiredness::Required,
                                            &message,
                                        );
                                    }
                                }
                            }
                        }
                        Err(message) => push_visual_failure(
                            report,
                            graph,
                            prototype_index,
                            mesh.name.0.clone(),
                            PrototypeGraphEdge::WearToMaterial,
                            PrototypeGraphRequiredness::Required,
                            &message.clone(),
                        ),
                    }
                }
                for lightmap in &table.lightmaps {
                    report.lightmap_request_count += 1;
                    match resolve_texm_validation_cached(
                        repository,
                        &lightmap.lightmap,
                        LIGHTMAP_ARCHIVE,
                        "lightmap",
                        &mut lightmap_texture_validation,
                    ) {
                        Ok(()) => {
                            report.lightmap_resolved_count += 1;
                            if let Ok(lightmap_key) =
                                texm_resource_key(LIGHTMAP_ARCHIVE, &lightmap.lightmap)
                            {
                                let lightmap_node_id = push_graph_resource_node(
                                    graph,
                                    PrototypeGraphNodeKind::LightmapResource,
                                    lightmap_key.clone(),
                                    &mut next_node,
                                );
                                push_graph_edge(
                                    graph,
                                    wear_node_id,
                                    lightmap_node_id,
                                    fparkan_prototype::PrototypeGraphEdgeKind::WearToLightmap,
                                    PrototypeGraphRequiredness::Required,
                                    Some(provenance_for_resource(
                                        root_index,
                                        Some(wear_edge_id),
                                        unit_component_index,
                                        &lightmap_key,
                                    )),
                                    &mut next_edge,
                                );
                            }
                        }
                        Err(message) => {
                            push_visual_failure(
                                report,
                                graph,
                                prototype_index,
                                lightmap.lightmap.0.clone(),
                                PrototypeGraphEdge::WearToLightmap,
                                PrototypeGraphRequiredness::Required,
                                &message,
                            );
                        }
                    }
                }
            }
            Err(message) => push_visual_failure(
                report,
                graph,
                prototype_index,
                mesh.name.0.clone(),
                PrototypeGraphEdge::MeshToWear,
                PrototypeGraphRequiredness::Required,
                &message,
            ),
        }
    }
    graph.visual_dependencies_expanded = true;
}

fn push_graph_resource_node(
    graph: &mut PrototypeGraph,
    kind: PrototypeGraphNodeKind,
    resource: ResourceKey,
    next_node: &mut u32,
) -> fparkan_prototype::PrototypeGraphNodeId {
    let id = fparkan_prototype::PrototypeGraphNodeId(*next_node);
    *next_node = (*next_node).saturating_add(1);
    graph
        .nodes
        .push(fparkan_prototype::PrototypeGraphNode::resource(
            kind, resource, id,
        ));
    id
}

fn push_graph_edge(
    graph: &mut PrototypeGraph,
    from: fparkan_prototype::PrototypeGraphNodeId,
    to: fparkan_prototype::PrototypeGraphNodeId,
    kind: fparkan_prototype::PrototypeGraphEdgeKind,
    requiredness: PrototypeGraphRequiredness,
    provenance: Option<PrototypeGraphProvenance>,
    next_edge: &mut u32,
) -> fparkan_prototype::PrototypeGraphEdgeId {
    let id = fparkan_prototype::PrototypeGraphEdgeId(*next_edge);
    *next_edge = (*next_edge).saturating_add(1);
    graph
        .edges
        .push(fparkan_prototype::PrototypeGraphEdgeInstance {
            id,
            from,
            to,
            kind,
            requiredness,
            provenance,
        });
    id
}

fn prototype_mesh_node_id(
    graph: &PrototypeGraph,
    prototype_node: fparkan_prototype::PrototypeGraphNodeId,
) -> Option<fparkan_prototype::PrototypeGraphNodeId> {
    graph
        .edges
        .iter()
        .find(|edge| {
            edge.from == prototype_node
                && matches!(
                    edge.kind,
                    fparkan_prototype::PrototypeGraphEdgeKind::PrototypeToMesh
                )
        })
        .map(|edge| edge.to)
}

fn provenance_for_resource(
    root_index: usize,
    parent_edge: Option<fparkan_prototype::PrototypeGraphEdgeId>,
    unit_component_index: Option<usize>,
    resource: &ResourceKey,
) -> PrototypeGraphProvenance {
    PrototypeGraphProvenance {
        root_index,
        parent_edge,
        unit_component_index,
        archive: Some(resource.archive.display_lossy().to_string()),
        resource: Some(resource.name.0.clone()),
        span: None,
    }
}

fn wear_resource_key(mesh: &ResourceKey) -> Result<ResourceKey, AssetError> {
    Ok(ResourceKey {
        archive: mesh.archive.clone(),
        name: sibling_name(mesh, "wea")?,
        type_id: Some(WEAR_KIND),
    })
}

fn texm_resource_key(archive: &str, name: &ResourceName) -> Result<ResourceKey, AssetError> {
    Ok(ResourceKey {
        archive: parse_path(archive)?,
        name: name.clone(),
        type_id: None,
    })
}

/// Validates a prototype visual without resolving cross-resource dependencies.
///
/// This is useful for tests and API callers that only need a stable visual id.
///
/// # Errors
///
/// Returns [`AssetError`] when the prototype geometry is malformed.
pub fn prepare_visual(proto: &EffectivePrototype) -> Result<PreparedVisual, AssetError> {
    let id = stable_visual_id(proto);
    let mesh = match &proto.geometry {
        PrototypeGeometry::Mesh(key) => Some(key.clone()),
        PrototypeGeometry::NonGeometric => None,
    };

    Ok(PreparedVisual {
        id: AssetId::new(id),
        mesh,
        source_type: proto.source_type,
        model_id: None,
        wear_id: None,
        model_nodes: 0,
        model_slots: 0,
        model_batches: 0,
        material_count: 0,
        material_ids: Vec::new(),
        texture_ids: Vec::new(),
        lightmap_ids: Vec::new(),
        texture_count: 0,
        lightmap_count: 0,
    })
}

/// Prepares one visual and validates all CPU-side resource dependencies.
///
/// # Errors
///
/// Returns [`AssetError`] if the model, WEAR table, MAT0 materials, texture
/// phases, or lightmaps cannot be resolved and decoded.
pub fn prepare_visual_with_repository<R: ResourceRepository>(
    repository: &R,
    proto: &EffectivePrototype,
) -> Result<PreparedVisual, AssetError> {
    let mut preparation_cache = PreparationCache::default();
    Ok(prepare_visual_with_repository_internal(
        repository,
        proto,
        AssetIdentityPolicy::default(),
        &mut preparation_cache,
    )?
    .visual)
}

struct PreparedVisualBundle {
    visual: PreparedVisual,
    model: Option<PreparedModel>,
    wear: Option<PreparedWear>,
    materials: Vec<PreparedMaterial>,
    textures: Vec<PreparedTexture>,
}

#[derive(Default)]
struct PreparationCache {
    models: Vec<(ResourceKey, ModelAsset)>,
    wears: Vec<(ResourceKey, WearTable)>,
    diffuse: HashMap<Vec<u8>, PreparedTexture>,
    lightmaps: HashMap<Vec<u8>, PreparedTexture>,
    materials: HashMap<Vec<u8>, ResolvedMaterial>,
}

// Keeps the complete model -> WEAR -> material -> texture transaction in one fallible boundary.

fn prepare_visual_with_repository_internal<R: ResourceRepository>(
    repository: &R,
    proto: &EffectivePrototype,
    identity_policy: AssetIdentityPolicy,
    preparation_cache: &mut PreparationCache,
) -> Result<PreparedVisualBundle, AssetError> {
    let PrototypeGeometry::Mesh(mesh_key) = &proto.geometry else {
        return Ok(PreparedVisualBundle {
            visual: prepare_visual(proto)?,
            model: None,
            wear: None,
            materials: Vec::new(),
            textures: Vec::new(),
        });
    };

    let model = prepare_model_cached(repository, mesh_key, preparation_cache)?;
    let model_id = AssetId::new((identity_policy.model)(proto));
    let prepared_model = PreparedModel {
        id: model_id,
        source: mesh_key.clone(),
        validated: model.clone(),
        mesh_dependencies: proto.dependencies.clone(),
    };

    let wear_name = sibling_name(mesh_key, "wea")?;
    let wear_key = ResourceKey {
        archive: mesh_key.archive.clone(),
        name: wear_name,
        type_id: Some(WEAR_KIND),
    };
    let wear = prepare_wear_cached(repository, &wear_key, preparation_cache)?;
    let wear_id = AssetId::new((identity_policy.wear)(proto));
    let prepared_wear = PreparedWear {
        id: wear_id,
        source: wear_key.clone(),
        table: wear.clone(),
    };

    let mut material_count = 0;
    let mut material_ids = Vec::with_capacity(wear.entries.len());
    let mut prepared_materials = Vec::with_capacity(wear.entries.len());
    let mut prepared_textures = Vec::new();
    let mut texture_ids = Vec::new();
    let mut lightmap_ids = Vec::new();
    let mut texture_count = 0;
    let mut lightmap_count = 0;
    let lightmap_requests: Vec<_> = wear
        .lightmaps
        .iter()
        .map(|lightmap| lightmap.lightmap.clone())
        .collect();
    for material_index in 0..wear.entries.len() {
        let material_index = u16::try_from(material_index).map_err(|_| {
            AssetError::InvalidPrototype("material index does not fit archive format".to_string())
        })?;
        let material =
            resolve_material_cached(repository, &wear, material_index, preparation_cache)?;
        material_count += 1;
        let material_id = AssetId::new((identity_policy.material)(
            proto,
            material_index,
            &material.name,
        ));
        material_ids.push(material_id);
        let material_key = ResourceKey {
            archive: parse_path("material.lib")?,
            name: material.name.clone(),
            type_id: Some(MAT0_KIND),
        };
        let texture_requests = material.document.texture_requests();
        prepared_materials.push(PreparedMaterial {
            id: material_id,
            source: material_key,
            name: material.name.clone(),
            attr1: material.attr1,
            pipeline_category: material_pipeline_category(material.attr1),
            native_flag_40: material_native_flag_40(material.attr1),
            mat0: material.document.clone(),
            texture_requests: texture_requests.clone(),
            lightmap_requests: lightmap_requests.clone(),
        });

        for texture in texture_requests {
            let prepared_texture = prepare_texture_cached(
                repository,
                &texture,
                PreparedTextureUsage::Diffuse,
                identity_policy,
                preparation_cache,
            )?;
            texture_ids.push(prepared_texture.id);
            prepared_textures.push(prepared_texture);
            texture_count += 1;
        }
    }

    for lightmap in &wear.lightmaps {
        let prepared_lightmap = prepare_texture_cached(
            repository,
            &lightmap.lightmap,
            PreparedTextureUsage::Lightmap,
            identity_policy,
            preparation_cache,
        )?;
        lightmap_ids.push(prepared_lightmap.id);
        prepared_textures.push(prepared_lightmap);
        lightmap_count += 1;
    }

    Ok(PreparedVisualBundle {
        visual: PreparedVisual {
            id: AssetId::new((identity_policy.visual)(proto)),
            mesh: Some(mesh_key.clone()),
            source_type: proto.source_type,
            model_id: Some(model_id),
            wear_id: Some(wear_id),
            model_nodes: model.node_count,
            model_slots: model.slots.len(),
            model_batches: model.batches.len(),
            material_count,
            material_ids,
            texture_ids,
            lightmap_ids,
            texture_count,
            lightmap_count,
        },
        model: Some(prepared_model),
        wear: Some(prepared_wear),
        materials: prepared_materials,
        textures: prepared_textures,
    })
}

fn read_key<R: ResourceRepository>(
    repository: &R,
    key: &ResourceKey,
    label: Option<&str>,
) -> Result<Arc<[u8]>, AssetError> {
    let label = label.unwrap_or("asset");
    let archive = repository
        .open_archive_unchanged(&key.archive)
        .map_err(|err| map_resource_error(label, key, err))?;
    let handle = repository
        .find(archive, &key.name)
        .map_err(|err| map_resource_error(label, key, err))?
        .ok_or_else(|| AssetError::MissingDependency(format!("{label}: {key:?}")))?;
    let bytes = repository
        .read(handle)
        .map_err(|err| map_resource_error(label, key, err))?;
    Ok(Arc::from(bytes.into_owned()))
}

fn prepare_model_cached<R: ResourceRepository>(
    repository: &R,
    key: &ResourceKey,
    cache: &mut PreparationCache,
) -> Result<ModelAsset, AssetError> {
    if let Some((_, model)) = cache
        .models
        .iter()
        .find(|(cached_key, _)| cached_key == key)
    {
        return Ok(model.clone());
    }
    let nres = decode_nres(
        read_key(repository, key, Some("mesh"))?,
        ReadProfile::Compatible,
    )
    .map_err(AssetError::Nres)?;
    let document = decode_msh(&nres).map_err(AssetError::Msh)?;
    let model = validate_msh(&document).map_err(AssetError::Msh)?;
    cache.models.push((key.clone(), model.clone()));
    Ok(model)
}

fn prepare_wear_cached<R: ResourceRepository>(
    repository: &R,
    key: &ResourceKey,
    cache: &mut PreparationCache,
) -> Result<WearTable, AssetError> {
    if let Some((_, wear)) = cache.wears.iter().find(|(cached_key, _)| cached_key == key) {
        return Ok(wear.clone());
    }
    let wear =
        decode_wear(&read_key(repository, key, Some("wear"))?).map_err(AssetError::Material)?;
    cache.wears.push((key.clone(), wear.clone()));
    Ok(wear)
}

fn map_resource_error(label: &str, key: &ResourceKey, source: ResourceError) -> AssetError {
    AssetError::Resource {
        context: format!(
            "{label}: archive={} entry={}",
            key.archive.as_str(),
            String::from_utf8_lossy(&key.name.0),
        ),
        source: Box::new(source),
    }
}

fn resolve_wear_table<R: ResourceRepository>(
    repository: &R,
    mesh: &ResourceKey,
) -> Result<fparkan_material::WearTable, AssetError> {
    let archive = repository
        .open_archive_unchanged(&mesh.archive)
        .map_err(|err| map_resource_error("wear", mesh, err))?;
    let wear_name = sibling_name(mesh, "wea")?;
    let handle = repository
        .find(archive, &wear_name)
        .map_err(|err| {
            map_resource_error(
                "wear",
                &ResourceKey {
                    archive: mesh.archive.clone(),
                    name: wear_name.clone(),
                    type_id: Some(WEAR_KIND),
                },
                err,
            )
        })?
        .ok_or_else(|| {
            AssetError::MissingDependency(format!(
                "missing WEAR entry {}",
                String::from_utf8_lossy(&wear_name.0)
            ))
        })?;
    let info = repository.entry_info(handle).map_err(|err| {
        map_resource_error(
            "wear",
            &ResourceKey {
                archive: mesh.archive.clone(),
                name: wear_name.clone(),
                type_id: Some(WEAR_KIND),
            },
            err,
        )
    })?;
    if info.key.type_id != Some(WEAR_KIND) {
        return Err(AssetError::InvalidPrototype(format!(
            "entry {} is not WEAR",
            String::from_utf8_lossy(&wear_name.0)
        )));
    }
    let bytes = repository
        .read(handle)
        .map_err(|err| {
            map_resource_error(
                "wear",
                &ResourceKey {
                    archive: mesh.archive.clone(),
                    name: wear_name.clone(),
                    type_id: Some(WEAR_KIND),
                },
                err,
            )
        })?
        .into_owned();
    decode_wear(&bytes).map_err(AssetError::Material)
}

fn resolve_wear_table_cached<R: ResourceRepository>(
    repository: &R,
    mesh: &ResourceKey,
    cache: &mut WearValidationCache,
) -> Result<fparkan_material::WearTable, String> {
    let wear_key = wear_resource_key(mesh).map_err(|error| error.to_string())?;
    let cache_key = (wear_key.archive.clone(), wear_key.name.0.clone());
    if let Some(result) = cache.get(&cache_key) {
        return result.clone();
    }
    let result = resolve_wear_table(repository, mesh).map_err(|error| error.to_string());
    cache.insert(cache_key, result.clone());
    result
}

fn push_visual_failure(
    report: &mut PrototypeGraphReport,
    graph: &PrototypeGraph,
    prototype_index: usize,
    resource_raw: Vec<u8>,
    edge: PrototypeGraphEdge,
    requiredness: PrototypeGraphRequiredness,
    message: &str,
) {
    let root_index = root_index_for_prototype(graph, prototype_index);
    let parent_edge = parent_edge_for_failure(graph, prototype_index, edge);
    let unit_component_index =
        parent_edge.and_then(|value| unit_component_index_for_edge(graph, value));
    let dependency = mesh_dependency_resource(graph, prototype_index);
    report.failures.push(PrototypeGraphFailure {
        root_index,
        resource_raw: resource_raw.clone(),
        edge,
        message: message.to_string(),
        requiredness,
        provenance: Some(PrototypeGraphProvenance {
            root_index,
            parent_edge,
            unit_component_index,
            archive: dependency.map(|resource| resource.archive.as_str().to_string()),
            resource: Some(resource_raw),
            span: None,
        }),
    });
}

fn root_index_for_prototype(graph: &PrototypeGraph, prototype_index: usize) -> usize {
    for (root_index, span) in graph.root_prototype_request_spans.iter().enumerate() {
        if span.start <= prototype_index && prototype_index < span.end {
            return root_index;
        }
    }
    0
}

fn unit_component_index_for_edge(
    graph: &PrototypeGraph,
    edge_id: fparkan_prototype::PrototypeGraphEdgeId,
) -> Option<usize> {
    graph
        .edges
        .iter()
        .find(|edge| edge.id == edge_id)
        .and_then(|edge| edge.provenance.as_ref())
        .and_then(|provenance| provenance.unit_component_index)
}

fn parent_edge_for_failure(
    graph: &PrototypeGraph,
    prototype_index: usize,
    edge: PrototypeGraphEdge,
) -> Option<fparkan_prototype::PrototypeGraphEdgeId> {
    let prototype_node_id = prototype_node_id(graph, prototype_index)?;
    match edge {
        PrototypeGraphEdge::MeshToWear
        | PrototypeGraphEdge::WearToMaterial
        | PrototypeGraphEdge::MaterialToTexture
        | PrototypeGraphEdge::WearToLightmap => mesh_edge_id(graph, prototype_node_id)
            .or_else(|| root_edge_id(graph, prototype_node_id)),
        _ => root_edge_id(graph, prototype_node_id),
    }
}

fn prototype_node_id(
    graph: &PrototypeGraph,
    prototype_index: usize,
) -> Option<fparkan_prototype::PrototypeGraphNodeId> {
    graph
        .nodes
        .iter()
        .filter(|node| node.kind == PrototypeGraphNodeKind::Prototype)
        .nth(prototype_index)
        .map(|node| node.id)
}

fn root_edge_id(
    graph: &PrototypeGraph,
    prototype_node: fparkan_prototype::PrototypeGraphNodeId,
) -> Option<fparkan_prototype::PrototypeGraphEdgeId> {
    graph
        .edges
        .iter()
        .find(|edge| {
            edge.to == prototype_node
                && matches!(
                    edge.kind,
                    fparkan_prototype::PrototypeGraphEdgeKind::MissionToRoot
                        | fparkan_prototype::PrototypeGraphEdgeKind::UnitDatToComponent
                )
        })
        .map(|edge| edge.id)
}

fn mesh_edge_id(
    graph: &PrototypeGraph,
    prototype_node: fparkan_prototype::PrototypeGraphNodeId,
) -> Option<fparkan_prototype::PrototypeGraphEdgeId> {
    graph
        .edges
        .iter()
        .find(|edge| {
            edge.from == prototype_node
                && matches!(
                    edge.kind,
                    fparkan_prototype::PrototypeGraphEdgeKind::PrototypeToMesh
                )
        })
        .map(|edge| edge.id)
}

fn mesh_dependency_resource(
    graph: &PrototypeGraph,
    prototype_index: usize,
) -> Option<&fparkan_resource::ResourceKey> {
    let prototype_node = prototype_node_id(graph, prototype_index)?;
    let mesh_node = graph
        .edges
        .iter()
        .find(|edge| {
            edge.from == prototype_node
                && matches!(
                    edge.kind,
                    fparkan_prototype::PrototypeGraphEdgeKind::PrototypeToMesh
                )
        })?
        .to;
    graph
        .nodes
        .iter()
        .find(|node| node.id == mesh_node)
        .and_then(|node| node.resource.as_ref())
}

fn resolve_texm_validation_cached<R: ResourceRepository>(
    repository: &R,
    name: &ResourceName,
    archive: &str,
    label: &'static str,
    cache: &mut HashMap<Vec<u8>, Result<(), String>>,
) -> Result<(), String> {
    if let Some(result) = cache.get(&name.0) {
        return result.clone();
    }
    let result = resolve_texm(repository, name, archive, label).map_err(|error| error.to_string());
    cache.insert(name.0.clone(), result.clone());
    result
}

fn resolve_material_validation_cached<R: ResourceRepository>(
    repository: &R,
    table: &WearTable,
    index: u16,
    cache: &mut HashMap<Vec<u8>, Result<ResolvedMaterial, String>>,
) -> Result<ResolvedMaterial, String> {
    let request = table
        .entries
        .get(usize::from(index))
        .ok_or(MaterialError::WearIndexOutOfBounds {
            index,
            count: table.entries.len(),
        })
        .map_err(|error| error.to_string())?
        .material
        .0
        .clone();
    if let Some(result) = cache.get(&request) {
        return result.clone();
    }
    let result = resolve_material(repository, table, index).map_err(|error| error.to_string());
    cache.insert(request, result.clone());
    result
}

#[cfg(test)]
fn resolve_texture<R: ResourceRepository>(
    repository: &R,
    name: &ResourceName,
) -> Result<(), AssetError> {
    resolve_texm(repository, name, TEXTURES_ARCHIVE, "texture")
}

#[cfg(test)]
fn resolve_lightmap<R: ResourceRepository>(
    repository: &R,
    name: &ResourceName,
) -> Result<(), AssetError> {
    resolve_texm(repository, name, LIGHTMAP_ARCHIVE, "lightmap")
}

fn prepare_texture<R: ResourceRepository>(
    repository: &R,
    name: &ResourceName,
    usage: PreparedTextureUsage,
    identity_policy: AssetIdentityPolicy,
) -> Result<PreparedTexture, AssetError> {
    let (archive, label) = match usage {
        PreparedTextureUsage::Diffuse => (TEXTURES_ARCHIVE, "texture"),
        PreparedTextureUsage::Lightmap => (LIGHTMAP_ARCHIVE, "lightmap"),
    };
    let key = ResourceKey {
        archive: parse_path(archive)?,
        name: name.clone(),
        type_id: None,
    };
    let Some(bytes) = read_optional_key(repository, &key, Some(label))? else {
        return Err(AssetError::MissingDependency(format!("{label} {name:?}")));
    };
    let texm = decode_texm(bytes).map_err(AssetError::Texture)?;
    Ok(PreparedTexture {
        id: AssetId::new((identity_policy.texture)(&key, usage)),
        source: key,
        texm,
        usage,
    })
}

fn prepare_texture_cached<R: ResourceRepository>(
    repository: &R,
    name: &ResourceName,
    usage: PreparedTextureUsage,
    identity_policy: AssetIdentityPolicy,
    cache: &mut PreparationCache,
) -> Result<PreparedTexture, AssetError> {
    let entries = match usage {
        PreparedTextureUsage::Diffuse => &mut cache.diffuse,
        PreparedTextureUsage::Lightmap => &mut cache.lightmaps,
    };
    if let Some(texture) = entries.get(&name.0) {
        return Ok(texture.clone());
    }
    let texture = prepare_texture(repository, name, usage, identity_policy)?;
    entries.insert(name.0.clone(), texture.clone());
    Ok(texture)
}

fn resolve_material_cached<R: ResourceRepository>(
    repository: &R,
    table: &WearTable,
    index: u16,
    cache: &mut PreparationCache,
) -> Result<ResolvedMaterial, AssetError> {
    let request = table
        .entries
        .get(usize::from(index))
        .ok_or(MaterialError::WearIndexOutOfBounds {
            index,
            count: table.entries.len(),
        })
        .map_err(AssetError::Material)?
        .material
        .0
        .clone();
    if let Some(material) = cache.materials.get(&request) {
        return Ok(material.clone());
    }
    let material = resolve_material(repository, table, index).map_err(AssetError::Material)?;
    cache.materials.insert(request, material.clone());
    Ok(material)
}

fn resolve_texm<R: ResourceRepository>(
    repository: &R,
    name: &ResourceName,
    archive: &str,
    label: &'static str,
) -> Result<(), AssetError> {
    let key = ResourceKey {
        archive: parse_path(archive)?,
        name: name.clone(),
        type_id: None,
    };
    let Some(bytes) = read_optional_key(repository, &key, Some(label))? else {
        return Err(AssetError::MissingDependency(format!("{label} {name:?}")));
    };
    decode_texm(bytes).map(|_| ()).map_err(AssetError::Texture)
}

fn read_optional_key<R: ResourceRepository>(
    repository: &R,
    key: &ResourceKey,
    label: Option<&str>,
) -> Result<Option<Arc<[u8]>>, AssetError> {
    let archive = match repository.open_archive_unchanged(&key.archive) {
        Ok(archive) => archive,
        Err(ResourceError::MissingArchive { .. } | ResourceError::MissingEntry) => return Ok(None),
        Err(err) => {
            let label = label.unwrap_or("asset");
            return Err(map_resource_error(label, key, err));
        }
    };
    let Some(handle) = repository.find(archive, &key.name).map_err(|err| {
        let label = label.unwrap_or("asset");
        map_resource_error(label, key, err)
    })?
    else {
        return Ok(None);
    };
    let bytes = repository.read(handle).map_err(|err| {
        let label = label.unwrap_or("asset");
        map_resource_error(label, key, err)
    })?;
    Ok(Some(Arc::from(bytes.into_owned())))
}

fn prepare_component_control_from_resource<R: ResourceRepository>(
    repository: &R,
    source: ResourceKey,
    point_source: Option<ResourceKey>,
    node_data_source: Option<ResourceKey>,
    model_node_count: usize,
    prototype_index: usize,
) -> Result<Option<PreparedComponentControl>, AssetError> {
    let Some(payload) = read_optional_key(repository, &source, Some("component control"))? else {
        return Ok(None);
    };
    let mut control =
        decode_component_control_payload(&payload, source, model_node_count, prototype_index)?;
    let node_data_payload = match node_data_source.as_ref() {
        Some(source) => read_optional_key(repository, source, Some("component node data"))?,
        None => None,
    };
    control.node_data = match (node_data_source.as_ref(), node_data_payload) {
        (Some(source), Some(payload)) => Some(decode_component_node_data_table(
            &payload,
            source.clone(),
            prototype_index,
        )?),
        _ => None,
    };
    resolve_control_gate_providers(&mut control, node_data_source.as_ref());
    let has_effect_rows = matches!(
        &control.effect_section,
        PreparedControlEffectSection::Parsed(rows) if !rows.is_empty()
    );
    let Some(point_source) = point_source.filter(|_| has_effect_rows) else {
        return Ok(Some(control));
    };
    let Some(point_payload) =
        read_optional_key(repository, &point_source, Some("component point list"))?
    else {
        return Ok(Some(control));
    };
    control.point_list = Some(decode_component_point_list(
        &point_payload,
        point_source,
        prototype_index,
    )?);
    Ok(Some(control))
}

fn prepare_component_bas_from_resource<R: ResourceRepository>(
    repository: &R,
    source: ResourceKey,
    prototype_index: usize,
) -> Result<Option<PreparedComponentBas>, AssetError> {
    let Some(payload) = read_optional_key(repository, &source, Some("component BAS geometry"))?
    else {
        return Ok(None);
    };
    let geometry = decode_bas_geometry(&payload).map_err(|error| {
        AssetError::InvalidPrototype(format!(
            "prototype {prototype_index} BAS geometry {}:{}: {error}",
            source.archive.as_str(),
            String::from_utf8_lossy(&source.name.0),
        ))
    })?;
    Ok(Some(PreparedComponentBas { source, geometry }))
}

fn decode_component_node_data_table(
    payload: &[u8],
    source: ResourceKey,
    prototype_index: usize,
) -> Result<PreparedControlNodeDataTable, AssetError> {
    const HEADER_BYTES: usize = 4;
    const ROW_BYTES: usize = 76;
    const ROW_LIMIT: usize = 65_536;

    let invalid = |message: &str| {
        AssetError::InvalidPrototype(format!(
            "prototype {prototype_index} node data {}:{}: {message}",
            source.archive.as_str(),
            String::from_utf8_lossy(&source.name.0),
        ))
    };
    let row_count =
        usize::try_from(control_u32_at(payload, 0).ok_or_else(|| invalid("missing row count"))?)
            .map_err(|_| invalid("row count overflows usize"))?;
    if row_count > ROW_LIMIT {
        return Err(invalid("row count exceeds the supported limit"));
    }
    let table_bytes = row_count
        .checked_mul(ROW_BYTES)
        .ok_or_else(|| invalid("row table byte length overflows"))?;
    let expected_len = HEADER_BYTES
        .checked_add(table_bytes)
        .ok_or_else(|| invalid("row table end offset overflows"))?;
    if payload.len() != expected_len {
        return Err(invalid(
            "payload length does not match its declared 76-byte row count",
        ));
    }

    let mut rows = Vec::with_capacity(row_count);
    for row_index in 0..row_count {
        let start = HEADER_BYTES + row_index * ROW_BYTES;
        let word = |offset: usize| {
            control_u32_at(payload, start + offset).expect("validated 76-byte NDP row")
        };
        let mut archive = [0; 32];
        let mut name = [0; 32];
        archive.copy_from_slice(&payload[start + 12..start + 44]);
        name.copy_from_slice(&payload[start + 44..start + 76]);
        rows.push(PreparedControlNodeDataRow {
            raw_flags: word(0),
            max_volume: f32::from_bits(word(4)),
            mobility_per_volume: f32::from_bits(word(8)),
            effect: PreparedControlResourcePair { archive, name },
        });
    }
    Ok(PreparedControlNodeDataTable { source, rows })
}

fn resolve_control_gate_providers(
    control: &mut PreparedComponentControl,
    node_data_source: Option<&ResourceKey>,
) {
    const INITIAL_TYPE1_VALUE: f32 = 1.0;
    let row_count = control.node_data.as_ref().map(|table| table.rows.len());
    for record in control
        .typed_records
        .iter_mut()
        .filter(|record| record.tag == 21)
    {
        let key = record.fixed_words[1];
        record.gate_provider = match (control.node_data.as_ref(), row_count) {
            (Some(table), Some(count)) if usize::try_from(key).is_ok_and(|key| key < count) => {
                PreparedControlGateProvider::Resolved {
                    key,
                    ndp_source: table.source.clone(),
                    row_count: count,
                    initial_type1_value: INITIAL_TYPE1_VALUE,
                }
            }
            (Some(table), Some(count)) => PreparedControlGateProvider::KeyOutOfRange {
                key,
                ndp_source: table.source.clone(),
                row_count: count,
            },
            _ => PreparedControlGateProvider::MissingResource {
                key,
                ndp_source: node_data_source.cloned(),
            },
        };
    }
}

fn decode_component_point_list(
    payload: &[u8],
    source: ResourceKey,
    prototype_index: usize,
) -> Result<PreparedControlPointList, AssetError> {
    const HEADER_BYTES: usize = 4;
    const POINT_BYTES: usize = 36;
    const NAME_BYTES: usize = 32;
    const POINT_LIMIT: usize = 65_536;

    let invalid = |message: &str| {
        AssetError::InvalidPrototype(format!(
            "prototype {prototype_index} point list {}:{}: {message}",
            source.archive.as_str(),
            String::from_utf8_lossy(&source.name.0),
        ))
    };
    let point_count =
        usize::try_from(control_u32_at(payload, 0).ok_or_else(|| invalid("missing point count"))?)
            .map_err(|_| invalid("point count overflows usize"))?;
    if point_count > POINT_LIMIT {
        return Err(invalid("point count exceeds the supported limit"));
    }
    let point_bytes = point_count
        .checked_mul(POINT_BYTES)
        .ok_or_else(|| invalid("point table byte length overflows"))?;
    let names_start = HEADER_BYTES
        .checked_add(point_bytes)
        .ok_or_else(|| invalid("point name table offset overflows"))?;
    let names_bytes = point_count
        .checked_mul(NAME_BYTES)
        .ok_or_else(|| invalid("point name table byte length overflows"))?;
    let expected_len = names_start
        .checked_add(names_bytes)
        .ok_or_else(|| invalid("point list end offset overflows"))?;
    if payload.len() != expected_len {
        return Err(invalid(
            "payload length does not match its point and name counts",
        ));
    }

    let mut points = Vec::with_capacity(point_count);
    for index in 0..point_count {
        let point_start = HEADER_BYTES + index * POINT_BYTES;
        let position = control_vec3_at(payload, point_start + 12)
            .expect("complete CPT point record was checked");
        let direction = control_vec3_at(payload, point_start + 24)
            .expect("complete CPT point record was checked");
        if position
            .into_iter()
            .chain(direction)
            .any(|value| !value.is_finite())
        {
            return Err(invalid(
                "point position or direction contains a non-finite value",
            ));
        }
        let name_start = names_start + index * NAME_BYTES;
        points.push(PreparedControlPoint {
            raw_flags: control_u32_at(payload, point_start).expect("checked point flags"),
            raw_signed_field_04: control_i32_at(payload, point_start + 4)
                .expect("checked signed point word at +4"),
            raw_signed_field_08: control_i32_at(payload, point_start + 8)
                .expect("checked signed point word at +8"),
            position,
            direction,
            name: payload[name_start..name_start + NAME_BYTES]
                .try_into()
                .expect("fixed point name is 32 bytes"),
        });
    }
    Ok(PreparedControlPointList { source, points })
}

fn control_vec3_at(payload: &[u8], offset: usize) -> Option<[f32; 3]> {
    Some([
        control_f32_at(payload, offset)?,
        control_f32_at(payload, offset.checked_add(4)?)?,
        control_f32_at(payload, offset.checked_add(8)?)?,
    ])
}

fn control_f32_at(payload: &[u8], offset: usize) -> Option<f32> {
    Some(f32::from_bits(control_u32_at(payload, offset)?))
}

fn control_i32_at(payload: &[u8], offset: usize) -> Option<i32> {
    Some(i32::from_le_bytes(
        control_u32_at(payload, offset)?.to_le_bytes(),
    ))
}

const CONTROL_TYPED_COMMON_PARSER_TAGS: &[u32] = &[1, 2, 5, 8, 9, 12, 13, 15, 19, 21, 25, 26];
const CONTROL_TYPED_RECORD_FIXED_BYTES: usize = 0xB0;
const CONTROL_TYPED_RECORD_COUNT_OFFSET: usize = 0xAC;
const CONTROL_TYPED_RECORD_LIMIT: usize = 4096;
const CONTROL_TYPED_VALUE_LIMIT: usize = 65_536;
const CONTROL_TYPED_STRING_LIMIT: usize = 4096;
const CONTROL_EFFECT_GROUP_LIMIT: usize = 4096;
const CONTROL_EFFECT_ROW_LIMIT: usize = 65_536;
const CONTROL_EFFECT_GROUP_HEADER_BYTES: usize = 0x54;
const CONTROL_EFFECT_ROW_BYTES: usize = 100;

struct DecodedComponentEffectSection {
    section: PreparedControlEffectSection,
    typed_records: Vec<PreparedControlTypedRecord>,
}

fn decode_component_effect_section<F>(
    payload: &[u8],
    mut cursor: usize,
    typed_record_count: usize,
    effect_group_count: usize,
    invalid: &F,
) -> Result<DecodedComponentEffectSection, AssetError>
where
    F: Fn(&str) -> AssetError,
{
    if typed_record_count == 0 && effect_group_count == 0 {
        return Ok(DecodedComponentEffectSection {
            section: if cursor == payload.len() {
                PreparedControlEffectSection::Absent
            } else {
                PreparedControlEffectSection::Unsupported {
                    typed_record_index: None,
                    typed_record_tag: None,
                }
            },
            typed_records: Vec::new(),
        });
    }
    if typed_record_count > CONTROL_TYPED_RECORD_LIMIT {
        return Err(invalid("typed record count exceeds the supported limit"));
    }
    if effect_group_count > CONTROL_EFFECT_GROUP_LIMIT {
        return Err(invalid("effect group count exceeds the supported limit"));
    }

    let mut typed_records = Vec::with_capacity(typed_record_count);
    for record_index in 0..typed_record_count {
        let tag = control_u32_at(payload, cursor)
            .ok_or_else(|| invalid("typed record is missing its tag"))?;
        if !CONTROL_TYPED_COMMON_PARSER_TAGS.contains(&tag) {
            return Ok(DecodedComponentEffectSection {
                section: PreparedControlEffectSection::Unsupported {
                    typed_record_index: Some(record_index),
                    typed_record_tag: Some(tag),
                },
                typed_records,
            });
        }

        let fixed_end = cursor
            .checked_add(CONTROL_TYPED_RECORD_FIXED_BYTES)
            .and_then(|offset| offset.checked_add(4))
            .ok_or_else(|| invalid("typed record header offset overflows"))?;
        if fixed_end > payload.len() {
            return Err(invalid("typed record header extends past the payload"));
        }
        let value_count = usize::try_from(
            control_u32_at(payload, cursor + CONTROL_TYPED_RECORD_COUNT_OFFSET)
                .expect("typed record header was checked"),
        )
        .map_err(|_| invalid("typed record value count overflows usize"))?;
        if value_count > CONTROL_TYPED_VALUE_LIMIT {
            return Err(invalid(
                "typed record value count exceeds the supported limit",
            ));
        }
        let values_bytes = value_count
            .checked_mul(4)
            .ok_or_else(|| invalid("typed record value byte length overflows"))?;
        let length_offset = cursor
            .checked_add(CONTROL_TYPED_RECORD_FIXED_BYTES)
            .and_then(|offset| offset.checked_add(values_bytes))
            .ok_or_else(|| invalid("typed record tail offset overflows"))?;
        let string_length = control_u32_at(payload, length_offset)
            .ok_or_else(|| invalid("typed record length word extends past the payload"))?;
        let after_length = length_offset
            .checked_add(4)
            .ok_or_else(|| invalid("typed record end offset overflows"))?;

        // Native parsing returns immediately for length zero. For UINT32_MAX,
        // its signed length+1 wraps to zero, also consuming no string bytes.
        let (string_end, string_content_end) = if string_length == 0 || string_length == u32::MAX {
            (after_length, after_length)
        } else {
            let string_bytes = usize::try_from(string_length)
                .ok()
                .and_then(|length| length.checked_add(1))
                .ok_or_else(|| invalid("typed record string length overflows"))?;
            if string_bytes > CONTROL_TYPED_STRING_LIMIT {
                return Err(invalid("typed record string exceeds the supported limit"));
            }
            let string_end = after_length
                .checked_add(string_bytes)
                .ok_or_else(|| invalid("typed record string end overflows"))?;
            if string_end > payload.len() {
                return Err(invalid("typed record string extends past the payload"));
            }
            if payload[string_end - 1] != 0 {
                return Err(invalid("typed record string is missing its trailing NUL"));
            }
            (string_end, string_end - 1)
        };
        let mut fixed_words = [0_u32; 44];
        for (word_index, word) in fixed_words.iter_mut().enumerate() {
            *word = control_u32_at(payload, cursor + word_index * 4)
                .expect("typed record fixed header was checked");
        }
        let values = (0..value_count)
            .map(|index| {
                control_u32_at(
                    payload,
                    cursor + CONTROL_TYPED_RECORD_FIXED_BYTES + index * 4,
                )
                .expect("typed record values were checked")
            })
            .collect();
        typed_records.push(PreparedControlTypedRecord {
            tag,
            fixed_words,
            values,
            string_bytes: payload[after_length..string_content_end].to_vec(),
            gate_provider: if tag == 21 {
                PreparedControlGateProvider::MissingResource {
                    key: fixed_words[1],
                    ndp_source: None,
                }
            } else {
                PreparedControlGateProvider::NotApplicable
            },
        });
        cursor = string_end;
    }

    if effect_group_count == 0 {
        return Ok(DecodedComponentEffectSection {
            section: if cursor == payload.len() {
                PreparedControlEffectSection::Parsed(Vec::new())
            } else {
                PreparedControlEffectSection::Unsupported {
                    typed_record_index: None,
                    typed_record_tag: None,
                }
            },
            typed_records,
        });
    }

    cursor = cursor
        .checked_add(CONTROL_EFFECT_GROUP_HEADER_BYTES)
        .ok_or_else(|| invalid("effect group header end overflows"))?;
    if cursor > payload.len() {
        return Err(invalid("effect group header extends past the payload"));
    }

    let mut effect_rows = Vec::new();
    for table_index in 0..effect_group_count {
        let row_count = usize::try_from(
            control_u32_at(payload, cursor)
                .ok_or_else(|| invalid("effect table is missing its row count"))?,
        )
        .map_err(|_| invalid("effect row count overflows usize"))?;
        if row_count > CONTROL_EFFECT_ROW_LIMIT {
            return Err(invalid("effect row count exceeds the supported limit"));
        }
        let rows_start = cursor
            .checked_add(4)
            .ok_or_else(|| invalid("effect row start overflows"))?;
        let row_bytes = row_count
            .checked_mul(CONTROL_EFFECT_ROW_BYTES)
            .ok_or_else(|| invalid("effect row byte length overflows"))?;
        let rows_end = rows_start
            .checked_add(row_bytes)
            .ok_or_else(|| invalid("effect table end overflows"))?;
        if rows_end > payload.len() {
            return Err(invalid("complete effect table extends past the payload"));
        }

        for row_index in 0..row_count {
            let row_start = rows_start + row_index * CONTROL_EFFECT_ROW_BYTES;
            let row = &payload[row_start..row_start + CONTROL_EFFECT_ROW_BYTES];
            if control_u32_at(row, 0x0C) != Some(4) {
                continue;
            }
            let raw_condition = [
                control_u32_at(row, 0).expect("fixed effect row has condition word 0"),
                control_u32_at(row, 4).expect("fixed effect row has condition word 1"),
                control_u32_at(row, 8).expect("fixed effect row has condition word 2"),
            ];
            let point_offsets = [
                control_u32_at(row, 0x10).expect("fixed effect row has point offset 0"),
                control_u32_at(row, 0x14).expect("fixed effect row has point offset 1"),
                control_u32_at(row, 0x18).expect("fixed effect row has point offset 2"),
            ];
            effect_rows.push(PreparedControlEffectRow {
                table_index,
                row_index,
                raw_condition,
                point_offsets,
                selector: control_u32_at(row, 0x1C).expect("fixed effect row has selector"),
                resource: PreparedControlResourcePair {
                    archive: row[0x24..0x44]
                        .try_into()
                        .expect("fixed archive field is 32 bytes"),
                    name: row[0x44..0x64]
                        .try_into()
                        .expect("fixed resource field is 32 bytes"),
                },
            });
        }
        cursor = rows_end;
    }

    if cursor != payload.len() {
        return Ok(DecodedComponentEffectSection {
            section: PreparedControlEffectSection::Unsupported {
                typed_record_index: None,
                typed_record_tag: None,
            },
            typed_records,
        });
    }
    Ok(DecodedComponentEffectSection {
        section: PreparedControlEffectSection::Parsed(effect_rows),
        typed_records,
    })
}

fn control_u32_at(payload: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    let bytes = payload.get(offset..end)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

fn decode_component_control_payload(
    payload: &[u8],
    source: ResourceKey,
    model_node_count: usize,
    prototype_index: usize,
) -> Result<PreparedComponentControl, AssetError> {
    const HEADER_BYTES: usize = 5 * 4;
    const FIXED_SECTION_BYTES: usize = 108;
    const SECTION_FIXED_BYTES: usize = 156;
    const SECTION_NODE_BYTES: usize = 16;
    const ROW_BYTES: usize = 36;

    let invalid = |message: &str| {
        AssetError::InvalidPrototype(format!(
            "prototype {prototype_index} control {}:{}: {message}",
            source.archive.as_str(),
            String::from_utf8_lossy(&source.name.0),
        ))
    };
    if payload.len() < HEADER_BYTES {
        return Err(invalid("payload is shorter than its five-word header"));
    }
    let mut header = [0_u32; 5];
    for (index, word) in header.iter_mut().enumerate() {
        let offset = index * 4;
        let bytes: [u8; 4] = payload[offset..offset + 4]
            .try_into()
            .expect("header slice is exactly four bytes");
        *word = u32::from_le_bytes(bytes);
    }

    let section_count =
        usize::try_from(header[0]).map_err(|_| invalid("section count overflows usize"))?;
    let per_section_nodes =
        usize::try_from(header[1]).map_err(|_| invalid("section node count overflows usize"))?;
    let row_count =
        usize::try_from(header[2]).map_err(|_| invalid("binding count overflows usize"))?;
    let typed_record_count =
        usize::try_from(header[3]).map_err(|_| invalid("typed record count overflows usize"))?;
    let effect_group_count =
        usize::try_from(header[4]).map_err(|_| invalid("effect group count overflows usize"))?;
    let per_section_node_bytes = SECTION_NODE_BYTES
        .checked_mul(per_section_nodes)
        .ok_or_else(|| invalid("section node bytes overflow"))?;
    let section_bytes = section_count
        .checked_mul(
            SECTION_FIXED_BYTES
                .checked_add(per_section_node_bytes)
                .ok_or_else(|| invalid("section size overflows"))?,
        )
        .ok_or_else(|| invalid("section table bytes overflow"))?;
    let matrix_bytes = section_count
        .checked_mul(section_count)
        .and_then(|count| count.checked_mul(4))
        .ok_or_else(|| invalid("section matrix bytes overflow"))?;
    let row_start = HEADER_BYTES
        .checked_add(FIXED_SECTION_BYTES)
        .and_then(|offset| offset.checked_add(section_bytes))
        .and_then(|offset| offset.checked_add(matrix_bytes))
        .ok_or_else(|| invalid("binding table offset overflows"))?;
    let row_bytes = row_count
        .checked_mul(ROW_BYTES)
        .ok_or_else(|| invalid("binding table bytes overflow"))?;
    let row_end = row_start
        .checked_add(row_bytes)
        .ok_or_else(|| invalid("binding table end overflows"))?;
    if row_end > payload.len() {
        return Err(invalid("complete binding table extends past the payload"));
    }

    let mut bindings = Vec::with_capacity(row_count);
    let mut timeline_rows = Vec::with_capacity(row_count);
    for row_index in 0..row_count {
        let row_start = row_start + row_index * ROW_BYTES;
        let row = &payload[row_start..row_start + ROW_BYTES];
        let flags = u32::from_le_bytes(row[0x20..0x24].try_into().expect("row flags"));
        let node_index = u32::from_le_bytes(row[0..4].try_into().expect("row node index")) as usize;
        let frame_a = f32::from_bits(u32::from_le_bytes(row[4..8].try_into().expect("frame A")));
        let frame_b = f32::from_bits(u32::from_le_bytes(row[8..12].try_into().expect("frame B")));
        let initial_blend = f32::from_bits(u32::from_le_bytes(
            row[12..16].try_into().expect("initial blend"),
        ));
        let raw_parameters = [
            u32::from_le_bytes(row[0x10..0x14].try_into().expect("row parameter +0x10")),
            u32::from_le_bytes(row[0x14..0x18].try_into().expect("row parameter +0x14")),
            u32::from_le_bytes(row[0x18..0x1C].try_into().expect("row parameter +0x18")),
            u32::from_le_bytes(row[0x1C..0x20].try_into().expect("row parameter +0x1C")),
        ];
        timeline_rows.push(PreparedControlTimelineRow {
            row_index,
            node_index,
            frame_a,
            frame_b,
            initial_blend,
            raw_parameters,
            flags,
        });
        if flags & 4 != 0 {
            continue;
        }
        if node_index >= model_node_count {
            return Err(invalid("enabled binding node index is outside the model"));
        }
        if !frame_a.is_finite()
            || !frame_b.is_finite()
            || !initial_blend.is_finite()
            || frame_a < 0.0
            || frame_b < 0.0
        {
            return Err(invalid(
                "enabled binding contains a non-finite or negative value",
            ));
        }
        bindings.push(PreparedControlNodeBinding {
            node_index,
            frame_a,
            frame_b,
            initial_blend,
            flags,
        });
    }

    let decoded_effect = decode_component_effect_section(
        payload,
        row_end,
        typed_record_count,
        effect_group_count,
        &invalid,
    )?;

    Ok(PreparedComponentControl {
        source,
        model_node_count,
        bindings,
        timeline_rows,
        typed_records: decoded_effect.typed_records,
        node_data: None,
        effect_section: decoded_effect.section,
        point_list: None,
    })
}

fn sibling_name(key: &ResourceKey, extension: &str) -> Result<ResourceName, AssetError> {
    let dot = key
        .name
        .0
        .iter()
        .rposition(|byte| *byte == b'.')
        .ok_or_else(|| {
            AssetError::InvalidPrototype(format!("resource name has no extension: {:?}", key.name))
        })?;
    let mut name = key.name.0[..dot].to_vec();
    name.push(b'.');
    name.extend_from_slice(extension.as_bytes());
    Ok(ResourceName(name))
}

fn stable_visual_id(proto: &EffectivePrototype) -> u64 {
    let mut hasher = StableHasher::default();
    proto.source_type.hash(&mut hasher);
    match &proto.geometry {
        PrototypeGeometry::Mesh(key) => {
            1_u8.hash(&mut hasher);
            key.archive.identity_bytes().hash(&mut hasher);
            key.name.0.hash(&mut hasher);
            key.type_id.hash(&mut hasher);
        }
        PrototypeGeometry::NonGeometric => {
            0_u8.hash(&mut hasher);
        }
    }
    hasher.finish()
}

fn stable_model_id(proto: &EffectivePrototype) -> u64 {
    stable_visual_id(proto)
}

fn stable_wear_id(proto: &EffectivePrototype) -> u64 {
    let mut hasher = StableHasher::default();
    stable_visual_id(proto).hash(&mut hasher);
    b"wear".hash(&mut hasher);
    hasher.finish()
}

fn stable_material_id(
    proto: &EffectivePrototype,
    material_index: u16,
    material_name: &ResourceName,
) -> u64 {
    let mut hasher = StableHasher::default();
    stable_visual_id(proto).hash(&mut hasher);
    material_index.hash(&mut hasher);
    material_name.0.hash(&mut hasher);
    hasher.finish()
}

fn stable_texture_id(key: &ResourceKey, usage: PreparedTextureUsage) -> u64 {
    let mut hasher = StableHasher::default();
    key.archive.identity_bytes().hash(&mut hasher);
    key.name.0.hash(&mut hasher);
    match usage {
        PreparedTextureUsage::Diffuse => 0_u8.hash(&mut hasher),
        PreparedTextureUsage::Lightmap => 1_u8.hash(&mut hasher),
    }
    hasher.finish()
}

fn parse_path(value: &str) -> Result<NormalizedPath, AssetError> {
    normalize_relative(value.as_bytes(), PathPolicy::HostCompatible)
        .map_err(|err| AssetError::InvalidPrototype(format!("{err}")))
}

#[derive(Default)]
struct StableHasher(u64);

impl Hasher for StableHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut value = if self.0 == 0 {
            0xcbf2_9ce4_8422_2325
        } else {
            self.0
        };
        for byte in bytes {
            value ^= u64::from(*byte);
            value = value.wrapping_mul(0x0000_0100_0000_01b3);
        }
        self.0 = value;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_prototype::build_prototype_graph;
    use fparkan_resource::{resource_name, CachedResourceRepository};
    use fparkan_vfs::{DirectoryVfs, MemoryVfs, Vfs};
    use std::path::PathBuf;

    // Preserve the authored CTLD fixture bits; this is not the `f32` value of tau.
    const AUTHORED_TAG21_RAW_PARAMETER_3: f32 = f32::from_bits(0x40C8_F5C3);

    fn tag21_fullcycle_control() -> PreparedComponentControl {
        let source = ResourceKey {
            archive: parse_path("parts.rlb").expect("archive"),
            name: resource_name(b"u_all_def_a_01.ctl"),
            type_id: None,
        };
        let mut payload = component_control_payload(&[(1, 0.0, 4.0, 0.0, 1)]);
        let row_start = 5 * 4 + 108;
        payload[row_start + 0x10..row_start + 0x14].copy_from_slice(&u32::MAX.to_le_bytes());
        payload[row_start + 0x14..row_start + 0x18].copy_from_slice(&u32::MAX.to_le_bytes());
        payload[row_start + 0x18..row_start + 0x1C]
            .copy_from_slice(&0.5_f32.to_bits().to_le_bytes());
        payload[row_start + 0x1C..row_start + 0x20]
            .copy_from_slice(&AUTHORED_TAG21_RAW_PARAMETER_3.to_bits().to_le_bytes());
        payload[12..16].copy_from_slice(&1_u32.to_le_bytes());

        let typed_start = payload.len();
        let mut fixed = [0_u8; CONTROL_TYPED_RECORD_FIXED_BYTES];
        fixed[0..4].copy_from_slice(&21_u32.to_le_bytes());
        fixed[8..12].copy_from_slice(&0_u32.to_le_bytes());
        fixed[0x18..0x1C].copy_from_slice(&u32::MAX.to_le_bytes());
        fixed[0x24..0x28].copy_from_slice(&1.0_f32.to_bits().to_le_bytes());
        fixed[0x28..0x2C].copy_from_slice(&1.0_f32.to_bits().to_le_bytes());
        fixed[CONTROL_TYPED_RECORD_COUNT_OFFSET..CONTROL_TYPED_RECORD_COUNT_OFFSET + 4]
            .copy_from_slice(&1_u32.to_le_bytes());
        payload.extend_from_slice(&fixed);
        payload.extend_from_slice(&0_u32.to_le_bytes());
        payload.extend_from_slice(&0_u32.to_le_bytes());
        assert_eq!(
            typed_start + CONTROL_TYPED_RECORD_FIXED_BYTES + 8,
            payload.len()
        );

        let mut control =
            decode_component_control_payload(&payload, source, 2, 0).expect("tag21 CTLD");
        control.typed_records[0].gate_provider = PreparedControlGateProvider::Resolved {
            key: 0,
            ndp_source: ResourceKey {
                archive: parse_path("parts.rlb").expect("NDP archive"),
                name: resource_name(b"u_all_def_a_01.ndp"),
                type_id: None,
            },
            row_count: 1,
            initial_type1_value: 1.0,
        };
        control
    }

    #[test]
    fn tag21_sampler_replays_authored_fullcycle_clock_and_gate() {
        let control = tag21_fullcycle_control();
        assert_eq!(control.model_node_count, 2);
        assert_eq!(control.typed_records.len(), 1);
        assert_eq!(control.typed_records[0].tag, 21);
        assert_eq!(control.typed_records[0].values, vec![0]);
        assert_eq!(
            f32::from_bits(control.timeline_rows[0].raw_parameters[2]),
            0.5
        );
        assert_eq!(
            f32::from_bits(control.timeline_rows[0].raw_parameters[3]),
            AUTHORED_TAG21_RAW_PARAMETER_3
        );

        // Native Control receives 1.0 from the first default NDP provider row.
        // Since 0x21820 tests for zero and its caller stops on return value 1,
        // a nonzero provider value opens the tag-21 update path.
        let default_provider_value = 1.0_f32;
        let gate_values = [Some(default_provider_value)];
        let mut state = ControlInstanceState::default();
        let at_zero = sample_control_node_times(&control, 0, &gate_values, &mut state)
            .expect("initial tag21 sample");
        assert_eq!(at_zero, vec![Some(0.0), Some(0.0)]);

        let at_quarter_step = sample_control_node_times(&control, 225, &gate_values, &mut state)
            .expect("quarter-step tag21 sample");
        assert!((at_quarter_step[1].expect("node 1") - 0.45).abs() < 1.0e-5);

        let at_first_transition =
            sample_control_node_times(&control, 900, &gate_values, &mut state)
                .expect("first tag21 transition");
        assert!((at_first_transition[1].expect("node 1") - 1.8).abs() < 1.0e-5);

        let at_second_transition =
            sample_control_node_times(&control, 1800, &gate_values, &mut state)
                .expect("second tag21 transition");
        assert!((at_second_transition[1].expect("node 1") - 3.6).abs() < 1.0e-5);

        let at_wrap = sample_control_node_times(&control, 2700, &gate_values, &mut state)
            .expect("wrapped tag21 transition");
        assert!((at_wrap[1].expect("node 1") - 1.4).abs() < 1.0e-5);

        let at_fullcycle = sample_control_node_times(&control, 18_000, &gate_values, &mut state)
            .expect("fullcycle tag21 transition");
        assert!(
            (at_fullcycle[1].expect("node 1") - 4.0).abs() < 1.0e-4,
            "full-cycle node time was {at_fullcycle:?}"
        );
        let zero_provider_value = 0.0_f32;
        let gate_closed = [Some(zero_provider_value)];
        let mut closed_gate_state = ControlInstanceState::default();
        for clock in [0, 100, 500, 1000] {
            let sample =
                sample_control_node_times(&control, clock, &gate_closed, &mut closed_gate_state)
                    .expect("closed native gate holds the initial blend");
            assert_eq!(sample[1], Some(0.0));
        }
        let reopened_provider_value = 1.0_f32;
        let reopened_gate = [Some(reopened_provider_value)];
        let opened_gate =
            sample_control_node_times(&control, 1001, &reopened_gate, &mut closed_gate_state)
                .expect("open native gate starts a transition");
        assert!(opened_gate[1].expect("node 1") > 0.0);
    }

    #[test]
    fn tag21_sampler_preserves_fast_rates_and_rejects_previous_row_mapping() {
        let mut fast = tag21_fullcycle_control();
        fast.timeline_rows[0].raw_parameters[2] = 20.0_f32.to_bits();
        let mut fast_state = ControlInstanceState::default();
        let at_half_transition =
            sample_control_node_times(&fast, 11, &[Some(1.0)], &mut fast_state)
                .expect("fast sample");
        assert!((at_half_transition[1].expect("node 1") - 0.88).abs() < 1.0e-5);
        let at_transition_end = sample_control_node_times(&fast, 23, &[Some(1.0)], &mut fast_state)
            .expect("fast transition");
        assert!((at_transition_end[1].expect("node 1") - 1.84).abs() < 1.0e-5);

        let mut direct = tag21_fullcycle_control();
        direct.timeline_rows[0].flags = 0;
        direct.typed_records[0].fixed_words[9] = 2.0_f32.to_bits();
        let direct_sample = sample_control_node_times(
            &direct,
            100,
            &[Some(1.0)],
            &mut ControlInstanceState::default(),
        )
        .expect("non-wrapping transition");
        assert!((direct_sample[1].expect("node 1") - 0.4).abs() < 1.0e-5);

        let mut previous_row = tag21_fullcycle_control();
        previous_row.timeline_rows[0].flags = 0x40;
        let error = sample_control_node_times(
            &previous_row,
            0,
            &[Some(1.0)],
            &mut ControlInstanceState::default(),
        )
        .expect_err("previous-row mapping must not silently use the wrong blend");
        assert_eq!(
            error,
            ControlSamplingError::UnsupportedPreviousRowMapping { row_index: 0 }
        );
    }

    #[test]
    fn component_control_rows_preserve_f32_values_and_source_order() {
        let source = ResourceKey {
            archive: parse_path("controls.rlb").expect("archive"),
            name: resource_name(b"parts.ctl"),
            type_id: None,
        };
        let payload = component_control_payload(&[
            (99, f32::NAN, -1.0, -1.0, 4),
            (1, 1.25, 2.5, 0.0, 0),
            (1, 2.0, 0.0, 0.75, 1),
        ]);

        let control =
            decode_component_control_payload(&payload, source, 2, 7).expect("valid enabled rows");

        assert_eq!(control.bindings.len(), 2);
        assert_eq!(control.bindings[0].node_index, 1);
        assert_eq!(control.bindings[0].frame_a, 1.25);
        assert_eq!(control.bindings[0].frame_b, 2.5);
        assert_eq!(control.bindings[0].initial_blend, 0.0);
        assert_eq!(control.bindings[1].node_index, 1);
        assert_eq!(control.bindings[1].frame_a, 2.0);
        assert_eq!(control.bindings[1].flags, 1);
    }

    #[test]
    fn component_control_rejects_bad_bounds_but_keeps_sampler_inputs_raw() {
        let source = ResourceKey {
            archive: parse_path("controls.rlb").expect("archive"),
            name: resource_name(b"parts.ctl"),
            type_id: None,
        };
        let mut overflow = vec![0; 20];
        overflow[0..4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode_component_control_payload(&overflow, source.clone(), 2, 0).is_err());

        let out_of_range_node = component_control_payload(&[(2, 0.0, 1.0, 0.0, 0)]);
        assert!(
            decode_component_control_payload(&out_of_range_node, source.clone(), 2, 0,).is_err()
        );

        let fallback_input = component_control_payload(&[(1, 0.0, 4.0, -0.5, 0)]);
        let control = decode_component_control_payload(&fallback_input, source.clone(), 2, 0)
            .expect("sampler decides out-of-map fallback and blend normalization");
        assert_eq!(control.bindings[0].frame_b, 4.0);
        assert_eq!(control.bindings[0].initial_blend, -0.5);

        let non_finite = component_control_payload(&[(1, f32::NAN, 1.0, 0.0, 0)]);
        assert!(decode_component_control_payload(&non_finite, source, 2, 0).is_err());
    }

    #[test]
    fn component_control_unknown_effect_tag_preserves_node_bindings() {
        let source = ResourceKey {
            archive: parse_path("controls.rlb").expect("archive"),
            name: resource_name(b"parts.ctl"),
            type_id: None,
        };
        let mut payload = component_control_payload(&[(1, 0.0, 1.0, 0.25, 0)]);
        payload[12..16].copy_from_slice(&1_u32.to_le_bytes());
        payload[16..20].copy_from_slice(&1_u32.to_le_bytes());
        payload.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes());

        let control = decode_component_control_payload(&payload, source.clone(), 2, 3)
            .expect("unknown tag is retained");

        assert_eq!(control.bindings.len(), 1);
        assert_eq!(control.bindings[0].node_index, 1);
        assert_eq!(control.bindings[0].frame_b, 1.0);
        assert_eq!(
            control.effect_section,
            PreparedControlEffectSection::Unsupported {
                typed_record_index: Some(0),
                typed_record_tag: Some(0xDEAD_BEEF),
            }
        );
    }

    #[test]
    fn component_control_zero_length_typed_record_ends_before_group_header() {
        let source = ResourceKey {
            archive: parse_path("controls.rlb").expect("archive"),
            name: resource_name(b"parts.ctl"),
            type_id: None,
        };
        let mut payload = component_control_payload(&[(1, 0.0, 1.0, 0.25, 0)]);
        payload[12..16].copy_from_slice(&1_u32.to_le_bytes());
        payload[16..20].copy_from_slice(&1_u32.to_le_bytes());
        append_typed_record(&mut payload, 21, 0);
        let group_header_start = payload.len();
        payload.resize(group_header_start + CONTROL_EFFECT_GROUP_HEADER_BYTES, 0);
        payload.extend_from_slice(&0_u32.to_le_bytes());

        let control = decode_component_control_payload(&payload, source.clone(), 2, 3)
            .expect("zero-length record has no trailing string byte");

        assert_eq!(control.bindings.len(), 1);
        assert_eq!(
            control.effect_section,
            PreparedControlEffectSection::Parsed(Vec::new())
        );
    }

    #[test]
    fn component_control_tags2_and5_share_the_typed_cursor_parser() {
        let source = ResourceKey {
            archive: parse_path("controls.rlb").expect("archive"),
            name: resource_name(b"parts.ctl"),
            type_id: None,
        };
        let mut payload = component_control_payload(&[(1, 0.0, 1.0, 0.25, 0)]);
        payload[12..16].copy_from_slice(&3_u32.to_le_bytes());
        append_typed_record(&mut payload, 2, 0);
        append_typed_record(&mut payload, 5, 2);
        payload.extend_from_slice(b"fx\0");
        append_typed_record(&mut payload, 21, 0);

        let control = decode_component_control_payload(&payload, source, 2, 0)
            .expect("tags 2 and 5 share the native typed cursor parser");

        assert_eq!(
            control
                .typed_records
                .iter()
                .map(|record| record.tag)
                .collect::<Vec<_>>(),
            [2, 5, 21],
            "tag 21 must begin at the cursor returned after tag 5's string"
        );
        assert_eq!(control.typed_records[1].string_bytes, b"fx");
        assert_eq!(
            control.effect_section,
            PreparedControlEffectSection::Parsed(Vec::new())
        );
    }

    #[test]
    fn component_control_malformed_recognized_typed_tail_is_rejected() {
        let source = ResourceKey {
            archive: parse_path("controls.rlb").expect("archive"),
            name: resource_name(b"parts.ctl"),
            type_id: None,
        };
        let mut payload = component_control_payload(&[(1, 0.0, 1.0, 0.25, 0)]);
        payload[12..16].copy_from_slice(&1_u32.to_le_bytes());
        append_typed_record(&mut payload, 21, 2);

        assert!(decode_component_control_payload(&payload, source, 2, 3).is_err());
    }

    #[test]
    fn component_control_type4_rows_preserve_fields_and_skip_other_types() {
        let source = ResourceKey {
            archive: parse_path("controls.rlb").expect("archive"),
            name: resource_name(b"parts.ctl"),
            type_id: None,
        };
        let mut payload = component_control_payload(&[(1, 0.0, 1.0, 0.25, 0)]);
        payload[12..16].copy_from_slice(&1_u32.to_le_bytes());
        payload[16..20].copy_from_slice(&2_u32.to_le_bytes());
        append_typed_record(&mut payload, 21, 0);
        payload.resize(payload.len() + CONTROL_EFFECT_GROUP_HEADER_BYTES, 0);
        append_effect_table(
            &mut payload,
            &[
                effect_row(
                    3,
                    [1, 2, 3],
                    [10, 0, 0],
                    8000,
                    "effects.rlb",
                    "door_open_02",
                ),
                effect_row(
                    4,
                    [0, 7, 9],
                    [104, 105, 106],
                    17,
                    "effects.rlb",
                    "f_smalllight_y",
                ),
            ],
        );
        append_effect_table(
            &mut payload,
            &[effect_row(
                4,
                [13, 14, 15],
                [85, 85, 85],
                26,
                "effects.rlb",
                "f_gener_ball",
            )],
        );

        let control = decode_component_control_payload(&payload, source.clone(), 2, 3)
            .expect("complete type-4 effect tables");

        assert_eq!(
            control.effect_section,
            PreparedControlEffectSection::Parsed(vec![
                PreparedControlEffectRow {
                    table_index: 0,
                    row_index: 1,
                    raw_condition: [0, 7, 9],
                    point_offsets: [104, 105, 106],
                    selector: 17,
                    resource: PreparedControlResourcePair {
                        archive: fixed_name("effects.rlb"),
                        name: fixed_name("f_smalllight_y"),
                    },
                },
                PreparedControlEffectRow {
                    table_index: 1,
                    row_index: 0,
                    raw_condition: [13, 14, 15],
                    point_offsets: [85, 85, 85],
                    selector: 26,
                    resource: PreparedControlResourcePair {
                        archive: fixed_name("effects.rlb"),
                        name: fixed_name("f_gener_ball"),
                    },
                },
            ])
        );
    }

    #[test]
    fn tag21_gate_provider_comes_from_validated_explicit_ndp() {
        let source = ResourceKey {
            archive: parse_path("parts.rlb").expect("CTL archive"),
            name: resource_name(b"locator.ctl"),
            type_id: None,
        };
        let ndp_source = ResourceKey {
            archive: parse_path("parts.rlb").expect("NDP archive"),
            name: resource_name(b"locator.ndp"),
            type_id: None,
        };
        let control_payload = tag21_control_payload(0);
        let ndp_payload = node_data_payload(1);
        let repository = repository_with_archives(&[(
            "parts.rlb",
            &[
                (b"locator.ctl", &control_payload),
                (b"locator.ndp", &ndp_payload),
            ],
        )]);
        let prepared = prepare_component_control_from_resource(
            &repository,
            source.clone(),
            None,
            Some(ndp_source.clone()),
            2,
            0,
        )
        .expect("prepare CTL and NDP")
        .expect("CTL exists");
        let table = prepared.node_data.as_ref().expect("NDP table attached");
        assert_eq!(table.source, ndp_source);
        assert_eq!(table.rows.len(), 1);
        assert_eq!(table.rows[0].max_volume, 6000.0);
        assert_eq!(table.rows[0].mobility_per_volume, 500.0);
        assert!(matches!(
            prepared.typed_records[0].gate_provider,
            PreparedControlGateProvider::Resolved {
                key: 0,
                row_count: 1,
                initial_type1_value: 1.0,
                ..
            }
        ));

        let mut state = ControlInstanceState::default();
        sample_control_node_times(&prepared, 225, &[Some(1.0)], &mut state)
            .expect("resolved nonzero provider opens the transition");

        let missing_repository =
            repository_with_archives(&[("parts.rlb", &[(b"locator.ctl", &control_payload)])]);
        let missing = prepare_component_control_from_resource(
            &missing_repository,
            source.clone(),
            None,
            Some(ResourceKey {
                archive: parse_path("parts.rlb").expect("archive"),
                name: resource_name(b"missing.ndp"),
                type_id: None,
            }),
            2,
            0,
        )
        .expect("missing optional NDP is retained as metadata")
        .expect("CTL exists");
        assert!(matches!(
            sample_control_node_times(&missing, 0, &[None], &mut ControlInstanceState::default()),
            Err(ControlSamplingError::MissingGateProvider { key: 0, .. })
        ));

        let out_of_range_payload = tag21_control_payload(1);
        let out_of_range_repository = repository_with_archives(&[(
            "parts.rlb",
            &[
                (b"locator.ctl", &out_of_range_payload),
                (b"locator.ndp", &ndp_payload),
            ],
        )]);
        let out_of_range = prepare_component_control_from_resource(
            &out_of_range_repository,
            source,
            None,
            Some(ResourceKey {
                archive: parse_path("parts.rlb").expect("archive"),
                name: resource_name(b"locator.ndp"),
                type_id: None,
            }),
            2,
            0,
        )
        .expect("out-of-range key remains inspectable")
        .expect("CTL exists");
        assert!(matches!(
            sample_control_node_times(
                &out_of_range,
                0,
                &[None],
                &mut ControlInstanceState::default(),
            ),
            Err(ControlSamplingError::GateProviderKeyOutOfRange {
                key: 1,
                row_count: 1,
                ..
            })
        ));

        let mut malformed_ndp = ndp_payload;
        malformed_ndp.push(0);
        assert!(decode_component_node_data_table(&malformed_ndp, ndp_source, 0).is_err());
    }

    #[test]
    fn component_control_loads_explicit_cpt_points_for_effect_offsets() {
        let mut control = component_control_payload(&[(1, 0.0, 1.0, 0.25, 0)]);
        control[16..20].copy_from_slice(&1_u32.to_le_bytes());
        control.resize(control.len() + CONTROL_EFFECT_GROUP_HEADER_BYTES, 0);
        append_effect_table(
            &mut control,
            &[
                effect_row(
                    4,
                    [0; 3],
                    [104, 105, 106],
                    17,
                    "effects.rlb",
                    "f_smalllight_y",
                ),
                effect_row(
                    4,
                    [0; 3],
                    [119, 120, 121],
                    3300,
                    "effects.rlb",
                    "smoke_fr_02",
                ),
            ],
        );
        let mut cpt = cpt_payload(122);
        write_cpt_point(
            &mut cpt,
            104,
            0,
            23,
            23,
            [-12.964_036, 12.991_658, -21.119_995],
            [0.0, 0.0, 1.0],
            "Sign_Recharge1",
        );
        write_cpt_point(
            &mut cpt,
            119,
            0x8000_0000,
            24,
            24,
            [0.0, 0.166_994_02, 32.7812],
            [0.0, 0.0, 1.0],
            "Smoke_w",
        );
        let repository = repository_with_archives(&[
            ("controls.rlb", &[(b"parts.ctl", &control)]),
            ("points.rlb", &[(b"attachments.cpt", &cpt)]),
        ]);
        let source = ResourceKey {
            archive: parse_path("controls.rlb").expect("control archive"),
            name: resource_name(b"parts.ctl"),
            type_id: None,
        };
        let point_source = ResourceKey {
            archive: parse_path("points.rlb").expect("point archive"),
            name: resource_name(b"attachments.cpt"),
            type_id: None,
        };

        let control = prepare_component_control_from_resource(
            &repository,
            source,
            Some(point_source),
            None,
            2,
            0,
        )
        .expect("loaded CTL and its explicit CPT")
        .expect("CTL entry");
        let point_list = control.point_list.as_ref().expect("point list is attached");

        assert_eq!(point_list.source.archive.as_str(), "points.rlb");
        assert_eq!(point_list.source.name, resource_name(b"attachments.cpt"));
        assert_eq!(point_list.points.len(), 122);
        let PreparedControlEffectSection::Parsed(rows) = &control.effect_section else {
            panic!("effect rows parsed");
        };
        assert_eq!(rows[0].point_offsets, [104, 105, 106]);
        assert_eq!(rows[1].point_offsets, [119, 120, 121]);
        assert_eq!(point_list.points[104].raw_signed_field_04, 23);
        assert_eq!(point_list.points[104].raw_signed_field_08, 23);
        assert_eq!(
            point_list.points[104].position,
            [-12.964_036, 12.991_658, -21.119_995]
        );
        assert_eq!(point_list.points[104].name, fixed_name("Sign_Recharge1"));
        assert_eq!(point_list.points[119].raw_flags, 0x8000_0000);
        assert_eq!(point_list.points[119].name, fixed_name("Smoke_w"));
    }

    #[test]
    fn component_point_list_rejects_bad_bounds_and_non_finite_points() {
        let source = ResourceKey {
            archive: parse_path("controls.rlb").expect("archive"),
            name: resource_name(b"parts.cpt"),
            type_id: None,
        };
        let valid = cpt_payload(1);
        assert!(decode_component_point_list(&valid[..valid.len() - 1], source.clone(), 0).is_err());
        let mut trailing = valid.clone();
        trailing.push(0);
        assert!(decode_component_point_list(&trailing, source.clone(), 0).is_err());

        let mut non_finite = valid;
        non_finite[16..20].copy_from_slice(&f32::INFINITY.to_bits().to_le_bytes());
        assert!(decode_component_point_list(&non_finite, source, 0).is_err());
    }

    #[test]
    fn mission_assets_keep_explicit_bas_geometry_at_component_index() {
        let mut bas = Vec::new();
        bas.extend_from_slice(&1_u32.to_le_bytes()); // first-section records
        bas.extend_from_slice(&1_u32.to_le_bytes()); // declared count
        for point in [[1.0_f32, 2.0, 3.0], [4.0, 5.0, 6.0]] {
            for value in point {
                bas.extend_from_slice(&value.to_le_bytes());
            }
        }
        bas.extend_from_slice(&0x1122_3344_u32.to_le_bytes());
        bas.extend_from_slice(&0x5566_7788_u32.to_le_bytes());
        bas.extend_from_slice(&1_u32.to_le_bytes()); // second-section records
        bas.extend_from_slice(&1_u32.to_le_bytes());
        for point in [[7.0_f32, 8.0, 9.0], [10.0, 11.0, 12.0]] {
            for value in point {
                bas.extend_from_slice(&value.to_le_bytes());
            }
        }
        let repository = repository_with_archives(&[("fortif.rlb", &[(b"fr_b_plant.bas", &bas)])]);
        let source = ResourceKey {
            archive: parse_path("fortif.rlb").expect("archive"),
            name: resource_name(b"fr_b_plant.bas"),
            type_id: None,
        };
        let without_bas = EffectivePrototype {
            key: fparkan_prototype::PrototypeKey(resource_name(b"unrelated")),
            geometry: PrototypeGeometry::NonGeometric,
            source: fparkan_prototype::PrototypeSource::ObjectsRegistry,
            source_type: None,
            dependencies: Vec::new(),
            control_resource: None,
            point_resource: None,
            node_data_resource: None,
            bas_resource: None,
        };
        let prototype = EffectivePrototype {
            key: fparkan_prototype::PrototypeKey(resource_name(b"fr_b_plant")),
            geometry: PrototypeGeometry::NonGeometric,
            source: fparkan_prototype::PrototypeSource::ObjectsRegistry,
            source_type: None,
            dependencies: Vec::new(),
            control_resource: None,
            point_resource: None,
            node_data_resource: None,
            bas_resource: Some(source.clone()),
        };

        let assets =
            prepare_mission_assets_with_repository(&repository, &[0..2], &[without_bas, prototype])
                .expect("mission BAS sidecar");
        assert_eq!(assets.object_component_bas[0].len(), 2);
        assert!(assets.object_component_bas[0][0].is_none());
        let prepared = assets.object_component_bas[0][1]
            .as_ref()
            .expect("BAS aligned with component one");
        assert_eq!(prepared.source, source);
        assert_eq!(prepared.geometry.first[0].points[1], [4.0, 5.0, 6.0]);
        assert_eq!(prepared.geometry.first[0].opaque_words_a, [0x1122_3344]);
        assert_eq!(prepared.geometry.second[0].points[1], [10.0, 11.0, 12.0]);
    }

    #[test]
    fn malformed_explicit_bas_geometry_is_reported_at_preparation_boundary() {
        let bad_payload = u32::MAX.to_le_bytes();
        let repository =
            repository_with_archives(&[("fortif.rlb", &[(b"broken.bas", &bad_payload)])]);
        let source = ResourceKey {
            archive: parse_path("fortif.rlb").expect("archive"),
            name: resource_name(b"broken.bas"),
            type_id: None,
        };

        let error = prepare_component_bas_from_resource(&repository, source, 3)
            .expect_err("malformed BAS counts are rejected");
        assert!(matches!(
            error,
            AssetError::InvalidPrototype(message)
                if message.contains("prototype 3 BAS geometry fortif.rlb:broken.bas")
                    && message.contains("limit")
        ));
    }

    #[test]
    fn component_control_preserves_unresolved_point_offset_sentinels() {
        let mut control = component_control_payload(&[(1, 0.0, 1.0, 0.25, 0)]);
        control[16..20].copy_from_slice(&1_u32.to_le_bytes());
        control.resize(control.len() + CONTROL_EFFECT_GROUP_HEADER_BYTES, 0);
        append_effect_table(
            &mut control,
            &[effect_row(
                4,
                [0; 3],
                [u32::MAX; 3],
                17,
                "effects.rlb",
                "f_smalllight_y",
            )],
        );
        let cpt = cpt_payload(1);
        let repository = repository_with_archives(&[
            ("controls.rlb", &[(b"parts.ctl", &control)]),
            ("points.rlb", &[(b"attachments.cpt", &cpt)]),
        ]);
        let source = ResourceKey {
            archive: parse_path("controls.rlb").expect("control archive"),
            name: resource_name(b"parts.ctl"),
            type_id: None,
        };
        let point_source = ResourceKey {
            archive: parse_path("points.rlb").expect("point archive"),
            name: resource_name(b"attachments.cpt"),
            type_id: None,
        };

        let control = prepare_component_control_from_resource(
            &repository,
            source,
            Some(point_source),
            None,
            2,
            0,
        )
        .expect("point-offset sentinel is retained without guessed bounds")
        .expect("CTL entry");
        let PreparedControlEffectSection::Parsed(rows) = control.effect_section else {
            panic!("effect rows parsed");
        };
        assert_eq!(rows[0].point_offsets, [u32::MAX; 3]);
        assert_eq!(
            control
                .point_list
                .expect("explicit CPT loaded")
                .points
                .len(),
            1
        );
    }

    #[test]
    fn component_control_keeps_missing_explicit_cpt_unavailable() {
        let mut control = component_control_payload(&[(1, 0.0, 1.0, 0.25, 0)]);
        control[16..20].copy_from_slice(&1_u32.to_le_bytes());
        control.resize(control.len() + CONTROL_EFFECT_GROUP_HEADER_BYTES, 0);
        append_effect_table(
            &mut control,
            &[effect_row(
                4,
                [0; 3],
                [0; 3],
                17,
                "effects.rlb",
                "f_smalllight_y",
            )],
        );
        let repository = repository_with_archives(&[("controls.rlb", &[(b"parts.ctl", &control)])]);
        let source = ResourceKey {
            archive: parse_path("controls.rlb").expect("control archive"),
            name: resource_name(b"parts.ctl"),
            type_id: None,
        };
        let missing_point_source = ResourceKey {
            archive: parse_path("points.rlb").expect("point archive"),
            name: resource_name(b"attachments.cpt"),
            type_id: None,
        };

        let control = prepare_component_control_from_resource(
            &repository,
            source,
            Some(missing_point_source),
            None,
            2,
            0,
        )
        .expect("an absent optional CPT leaves rows inspectable")
        .expect("CTL entry");

        assert_eq!(control.point_list, None);
        assert!(matches!(
            control.effect_section,
            PreparedControlEffectSection::Parsed(rows) if rows.len() == 1
        ));
    }

    #[test]
    fn component_control_max_length_uses_wrapped_empty_tail() {
        let source = ResourceKey {
            archive: parse_path("controls.rlb").expect("archive"),
            name: resource_name(b"parts.ctl"),
            type_id: None,
        };
        let mut payload = component_control_payload(&[(1, 0.0, 1.0, 0.25, 0)]);
        payload[12..16].copy_from_slice(&1_u32.to_le_bytes());
        payload[16..20].copy_from_slice(&1_u32.to_le_bytes());
        append_typed_record(&mut payload, 21, u32::MAX);
        payload.resize(payload.len() + CONTROL_EFFECT_GROUP_HEADER_BYTES, 0);
        append_effect_table(&mut payload, &[]);

        let control = decode_component_control_payload(&payload, source, 2, 3)
            .expect("wrapped empty string tail");

        assert_eq!(control.bindings.len(), 1);
        assert_eq!(
            control.effect_section,
            PreparedControlEffectSection::Parsed(Vec::new())
        );
    }

    fn component_control_payload(rows: &[(u32, f32, f32, f32, u32)]) -> Vec<u8> {
        const ROW_START: usize = 5 * 4 + 108;
        const ROW_BYTES: usize = 36;
        let mut payload = vec![0; ROW_START + rows.len() * ROW_BYTES];
        payload[8..12].copy_from_slice(&u32::try_from(rows.len()).expect("count").to_le_bytes());
        for (index, (node, frame_a, frame_b, blend, flags)) in rows.iter().enumerate() {
            let row = ROW_START + index * ROW_BYTES;
            payload[row..row + 4].copy_from_slice(&node.to_le_bytes());
            payload[row + 4..row + 8].copy_from_slice(&frame_a.to_bits().to_le_bytes());
            payload[row + 8..row + 12].copy_from_slice(&frame_b.to_bits().to_le_bytes());
            payload[row + 12..row + 16].copy_from_slice(&blend.to_bits().to_le_bytes());
            payload[row + 0x20..row + 0x24].copy_from_slice(&flags.to_le_bytes());
        }
        payload
    }

    fn tag21_control_payload(provider_key: u32) -> Vec<u8> {
        let mut payload = component_control_payload(&[(1, 0.0, 4.0, 0.0, 1)]);
        let row_start = 5 * 4 + 108;
        payload[row_start + 0x10..row_start + 0x14].copy_from_slice(&u32::MAX.to_le_bytes());
        payload[row_start + 0x14..row_start + 0x18].copy_from_slice(&u32::MAX.to_le_bytes());
        payload[row_start + 0x18..row_start + 0x1C]
            .copy_from_slice(&0.5_f32.to_bits().to_le_bytes());
        payload[row_start + 0x1C..row_start + 0x20]
            .copy_from_slice(&AUTHORED_TAG21_RAW_PARAMETER_3.to_bits().to_le_bytes());
        payload[12..16].copy_from_slice(&1_u32.to_le_bytes());

        let mut fixed = [0_u8; CONTROL_TYPED_RECORD_FIXED_BYTES];
        fixed[0..4].copy_from_slice(&21_u32.to_le_bytes());
        fixed[4..8].copy_from_slice(&provider_key.to_le_bytes());
        fixed[0x18..0x1C].copy_from_slice(&u32::MAX.to_le_bytes());
        fixed[0x24..0x28].copy_from_slice(&1.0_f32.to_bits().to_le_bytes());
        fixed[0x28..0x2C].copy_from_slice(&1.0_f32.to_bits().to_le_bytes());
        fixed[CONTROL_TYPED_RECORD_COUNT_OFFSET..CONTROL_TYPED_RECORD_COUNT_OFFSET + 4]
            .copy_from_slice(&1_u32.to_le_bytes());
        payload.extend_from_slice(&fixed);
        payload.extend_from_slice(&0_u32.to_le_bytes());
        payload.extend_from_slice(&0_u32.to_le_bytes());
        payload
    }

    fn node_data_payload(count: u32) -> Vec<u8> {
        const ROW_BYTES: usize = 76;
        let mut payload = vec![0; 4 + usize::try_from(count).expect("count") * ROW_BYTES];
        payload[0..4].copy_from_slice(&count.to_le_bytes());
        for row_index in 0..usize::try_from(count).expect("count") {
            let start = 4 + row_index * ROW_BYTES;
            payload[start..start + 4].copy_from_slice(&0_u32.to_le_bytes());
            payload[start + 4..start + 8].copy_from_slice(&6000.0_f32.to_bits().to_le_bytes());
            payload[start + 8..start + 12].copy_from_slice(&500.0_f32.to_bits().to_le_bytes());
            write_fixed_name(&mut payload[start + 12..start + 44], "system.rlb");
            write_fixed_name(&mut payload[start + 44..start + 76], "explode_frt_m.exp");
        }
        payload
    }

    fn append_typed_record(payload: &mut Vec<u8>, tag: u32, string_length: u32) {
        let record_start = payload.len();
        payload.resize(record_start + CONTROL_TYPED_RECORD_FIXED_BYTES + 4, 0);
        payload[record_start..record_start + 4].copy_from_slice(&tag.to_le_bytes());
        payload[record_start + CONTROL_TYPED_RECORD_COUNT_OFFSET
            ..record_start + CONTROL_TYPED_RECORD_COUNT_OFFSET + 4]
            .copy_from_slice(&0_u32.to_le_bytes());
        payload[record_start + CONTROL_TYPED_RECORD_FIXED_BYTES
            ..record_start + CONTROL_TYPED_RECORD_FIXED_BYTES + 4]
            .copy_from_slice(&string_length.to_le_bytes());
    }

    fn append_effect_table(payload: &mut Vec<u8>, rows: &[[u8; CONTROL_EFFECT_ROW_BYTES]]) {
        payload.extend_from_slice(&u32::try_from(rows.len()).expect("row count").to_le_bytes());
        for row in rows {
            payload.extend_from_slice(row);
        }
    }

    fn effect_row(
        kind: u32,
        condition: [u32; 3],
        point_offsets: [u32; 3],
        selector: u32,
        archive: &str,
        name: &str,
    ) -> [u8; CONTROL_EFFECT_ROW_BYTES] {
        let mut row = [0_u8; CONTROL_EFFECT_ROW_BYTES];
        for (index, word) in condition.into_iter().enumerate() {
            let offset = index * 4;
            row[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
        }
        row[0x0C..0x10].copy_from_slice(&kind.to_le_bytes());
        for (index, word) in point_offsets.into_iter().enumerate() {
            let offset = 0x10 + index * 4;
            row[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
        }
        row[0x1C..0x20].copy_from_slice(&selector.to_le_bytes());
        write_fixed_name(&mut row[0x24..0x44], archive);
        write_fixed_name(&mut row[0x44..0x64], name);
        row
    }

    fn fixed_name(value: &str) -> [u8; 32] {
        let mut result = [0_u8; 32];
        write_fixed_name(&mut result, value);
        result
    }

    fn write_fixed_name(target: &mut [u8], value: &str) {
        target[..value.len()].copy_from_slice(value.as_bytes());
    }

    fn cpt_payload(point_count: usize) -> Vec<u8> {
        let mut payload = vec![0; 4 + point_count * 36 + point_count * 32];
        payload[..4].copy_from_slice(
            &u32::try_from(point_count)
                .expect("point count")
                .to_le_bytes(),
        );
        payload
    }

    fn write_cpt_point(
        payload: &mut [u8],
        index: usize,
        raw_flags: u32,
        signed_field_04: i32,
        signed_field_08: i32,
        position: [f32; 3],
        direction: [f32; 3],
        name: &str,
    ) {
        let count = control_u32_at(payload, 0).expect("point count") as usize;
        assert!(index < count);
        let point_start = 4 + index * 36;
        payload[point_start..point_start + 4].copy_from_slice(&raw_flags.to_le_bytes());
        payload[point_start + 4..point_start + 8].copy_from_slice(&signed_field_04.to_le_bytes());
        payload[point_start + 8..point_start + 12].copy_from_slice(&signed_field_08.to_le_bytes());
        for (offset, value) in position.into_iter().chain(direction).enumerate() {
            let start = point_start + 12 + offset * 4;
            payload[start..start + 4].copy_from_slice(&value.to_bits().to_le_bytes());
        }
        let names_start = 4 + count * 36;
        write_fixed_name(
            &mut payload[names_start + index * 32..names_start + (index + 1) * 32],
            name,
        );
    }

    #[test]
    fn page_uv_transform_matches_normalized_native_atlas_record() {
        assert_eq!(
            page_uv_transform(
                8,
                4,
                &[PageRect {
                    x: 2,
                    w: 4,
                    y: 1,
                    h: 2,
                }],
                0,
            ),
            Ok([0.25, 0.25, 0.5, 0.5])
        );
        assert_eq!(page_uv_transform(8, 4, &[], -1), Ok([0.0, 0.0, 1.0, 1.0]));
    }

    #[test]
    fn page_uv_transform_rejects_missing_or_out_of_bounds_page() {
        let rects = [PageRect {
            x: 7,
            w: 2,
            y: 0,
            h: 1,
        }];
        assert!(page_uv_transform(8, 4, &[], 0).is_err());
        assert!(page_uv_transform(8, 4, &rects, 0).is_err());
    }

    #[test]
    fn wear_validation_cache_replays_archive_qualified_failure() {
        let repository = repository_with_archives(&[]);
        let mesh = ResourceKey {
            archive: parse_path("static.rlb").expect("archive"),
            name: resource_name(b"tree.msh"),
            type_id: Some(0x4853_454D),
        };
        let mut cache = WearValidationCache::new();
        cache.insert(
            (mesh.archive.clone(), b"tree.wea".to_vec()),
            Err("cached WEAR failure".to_string()),
        );

        assert_eq!(
            resolve_wear_table_cached(&repository, &mesh, &mut cache),
            Err("cached WEAR failure".to_string())
        );
    }

    #[test]
    fn count_only_plan_uses_graph_requests() {
        let graph = PrototypeGraph::default();

        let plan = build_mission_asset_plan(&graph);

        assert_eq!(plan.visual_count, 0);
        assert_eq!(plan.model_count, 0);
    }

    #[test]
    fn texture_resolver_does_not_fallback_to_lightmap_archive() {
        let texm = texm_payload();
        let repo = repository_with_archives(&[(
            LIGHTMAP_ARCHIVE,
            &[(b"TEX_ONLY".as_slice(), texm.as_slice())],
        )]);

        let err = resolve_texture(&repo, &resource_name(b"TEX_ONLY")).expect_err("missing texture");

        assert!(matches!(err, AssetError::MissingDependency(_)));
    }

    #[test]
    fn lightmap_resolver_does_not_fallback_to_texture_archive() {
        let texm = texm_payload();
        let repo = repository_with_archives(&[(
            TEXTURES_ARCHIVE,
            &[(b"LM_ONLY".as_slice(), texm.as_slice())],
        )]);

        let err =
            resolve_lightmap(&repo, &resource_name(b"LM_ONLY")).expect_err("missing lightmap");

        assert!(matches!(err, AssetError::MissingDependency(_)));
    }

    #[test]
    fn texture_resolver_does_not_continue_after_malformed_texture() {
        let malformed = b"not texm".as_slice();
        let texm = texm_payload();
        let repo = repository_with_archives(&[
            (TEXTURES_ARCHIVE, &[(b"BAD".as_slice(), malformed)]),
            (LIGHTMAP_ARCHIVE, &[(b"BAD".as_slice(), texm.as_slice())]),
        ]);

        let err = resolve_texture(&repo, &resource_name(b"BAD")).expect_err("malformed texture");

        assert!(matches!(err, AssetError::Texture(_)));
    }

    #[test]
    fn stable_visual_id_uses_archive_identity_bytes() {
        let first = EffectivePrototype {
            key: fparkan_prototype::PrototypeKey(resource_name(b"mesh")),
            geometry: PrototypeGeometry::Mesh(ResourceKey {
                archive: normalize_relative(b"DATA/\xFF.lib", PathPolicy::HostCompatible)
                    .expect("archive"),
                name: resource_name(b"mesh.msh"),
                type_id: Some(0x4853_454D),
            }),
            source: fparkan_prototype::PrototypeSource::DirectArchive,
            source_type: None,
            dependencies: Vec::new(),
            control_resource: None,
            point_resource: None,
            node_data_resource: None,
            bas_resource: None,
        };
        let second = EffectivePrototype {
            key: fparkan_prototype::PrototypeKey(resource_name(b"mesh")),
            geometry: PrototypeGeometry::Mesh(ResourceKey {
                archive: normalize_relative(b"DATA/\xFE.lib", PathPolicy::HostCompatible)
                    .expect("archive"),
                name: resource_name(b"mesh.msh"),
                type_id: Some(0x4853_454D),
            }),
            source: fparkan_prototype::PrototypeSource::DirectArchive,
            source_type: None,
            dependencies: Vec::new(),
            control_resource: None,
            point_resource: None,
            node_data_resource: None,
            bas_resource: None,
        };

        assert_ne!(stable_visual_id(&first), stable_visual_id(&second));
        assert_ne!(
            prepared_visual_signature(&first),
            prepared_visual_signature(&second)
        );
    }

    #[test]
    fn graph_materializes_visual_dependency_nodes_and_edges() {
        let mesh_key = ResourceKey {
            archive: parse_path("static.rlb").expect("archive"),
            name: resource_name(b"tree.msh"),
            type_id: Some(0x4853_454D),
        };
        let mat0 = mat0_with_texture(b"TEX_A");
        let texm = texm_payload();
        let lightmap_texm = texm_payload();
        let repo = repository_with_archives_meta(&[
            (
                "static.rlb",
                &[TestNresEntry {
                    name: b"tree.wea",
                    payload: b"1\n0 MAT_A\n\nLIGHTMAPS\n1\n0 LM_A\n",
                    type_id: WEAR_KIND,
                    attr2: 0,
                }],
            ),
            (
                "material.lib",
                &[TestNresEntry {
                    name: b"MAT_A",
                    payload: &mat0,
                    type_id: MAT0_KIND,
                    attr2: 0,
                }],
            ),
            (
                TEXTURES_ARCHIVE,
                &[TestNresEntry {
                    name: b"TEX_A",
                    payload: &texm,
                    type_id: 0,
                    attr2: 0,
                }],
            ),
            (
                LIGHTMAP_ARCHIVE,
                &[TestNresEntry {
                    name: b"LM_A",
                    payload: &lightmap_texm,
                    type_id: 0,
                    attr2: 0,
                }],
            ),
        ]);
        let prototype = EffectivePrototype {
            key: fparkan_prototype::PrototypeKey(resource_name(b"tree")),
            geometry: PrototypeGeometry::Mesh(mesh_key.clone()),
            source: fparkan_prototype::PrototypeSource::DirectArchive,
            source_type: None,
            dependencies: vec![mesh_key.clone()],
            control_resource: None,
            point_resource: None,
            node_data_resource: None,
            bas_resource: None,
        };
        let mut graph = prototype_graph_for_mesh(&prototype);
        let mut report = PrototypeGraphReport {
            root_count: 1,
            direct_reference_count: 1,
            resolved_count: 1,
            mesh_dependency_count: 1,
            ..PrototypeGraphReport::default()
        };

        extend_graph_report_with_visual_dependencies(
            &repo,
            &mut report,
            &mut graph,
            std::slice::from_ref(&prototype),
        );

        assert!(graph
            .nodes
            .iter()
            .any(|node| node.kind == PrototypeGraphNodeKind::WearResource));
        assert!(graph
            .nodes
            .iter()
            .any(|node| node.kind == PrototypeGraphNodeKind::MaterialResource));
        assert!(graph
            .nodes
            .iter()
            .any(|node| node.kind == PrototypeGraphNodeKind::TextureResource));
        assert!(graph
            .nodes
            .iter()
            .any(|node| node.kind == PrototypeGraphNodeKind::LightmapResource));
        assert!(graph
            .edges
            .iter()
            .any(|edge| edge.kind == fparkan_prototype::PrototypeGraphEdgeKind::MeshToWear));
        assert!(graph
            .edges
            .iter()
            .any(|edge| edge.kind == fparkan_prototype::PrototypeGraphEdgeKind::WearToMaterial));
        assert!(graph
            .edges
            .iter()
            .any(|edge| edge.kind == fparkan_prototype::PrototypeGraphEdgeKind::MaterialToTexture));
        assert!(graph
            .edges
            .iter()
            .any(|edge| edge.kind == fparkan_prototype::PrototypeGraphEdgeKind::WearToLightmap));
        assert_eq!(report.wear_request_count, 1);
        assert_eq!(report.wear_resolved_count, 1);
        assert_eq!(report.material_slot_count, 1);
        assert_eq!(report.material_resolved_count, 1);
        assert_eq!(report.texture_request_count, 1);
        assert_eq!(report.texture_resolved_count, 1);
        assert_eq!(report.lightmap_request_count, 1);
        assert_eq!(report.lightmap_resolved_count, 1);
        assert!(report.failures.is_empty());
    }

    #[test]
    fn graph_visual_dependency_edges_preserve_root_and_parent_provenance() {
        let mesh_key = ResourceKey {
            archive: parse_path("static.rlb").expect("archive"),
            name: resource_name(b"tree.msh"),
            type_id: Some(0x4853_454D),
        };
        let mat0 = mat0_with_texture(b"TEX_A");
        let texm = texm_payload();
        let lightmap_texm = texm_payload();
        let repo = repository_with_archives_meta(&[
            (
                "static.rlb",
                &[TestNresEntry {
                    name: b"tree.wea",
                    payload: b"1\n0 MAT_A\n\nLIGHTMAPS\n1\n0 LM_A\n",
                    type_id: WEAR_KIND,
                    attr2: 0,
                }],
            ),
            (
                "material.lib",
                &[TestNresEntry {
                    name: b"MAT_A",
                    payload: &mat0,
                    type_id: MAT0_KIND,
                    attr2: 0,
                }],
            ),
            (
                TEXTURES_ARCHIVE,
                &[TestNresEntry {
                    name: b"TEX_A",
                    payload: &texm,
                    type_id: 0,
                    attr2: 0,
                }],
            ),
            (
                LIGHTMAP_ARCHIVE,
                &[TestNresEntry {
                    name: b"LM_A",
                    payload: &lightmap_texm,
                    type_id: 0,
                    attr2: 0,
                }],
            ),
        ]);
        let prototype = EffectivePrototype {
            key: fparkan_prototype::PrototypeKey(resource_name(b"tree")),
            geometry: PrototypeGeometry::Mesh(mesh_key),
            source: fparkan_prototype::PrototypeSource::DirectArchive,
            source_type: None,
            dependencies: Vec::new(),
            control_resource: None,
            point_resource: None,
            node_data_resource: None,
            bas_resource: None,
        };
        let mut graph = prototype_graph_for_mesh(&prototype);
        graph
            .edges
            .iter_mut()
            .find(|edge| edge.kind == fparkan_prototype::PrototypeGraphEdgeKind::PrototypeToMesh)
            .expect("mesh edge")
            .provenance = Some(PrototypeGraphProvenance {
            root_index: 0,
            parent_edge: Some(fparkan_prototype::PrototypeGraphEdgeId(0)),
            unit_component_index: Some(3),
            archive: Some("static.rlb".to_string()),
            resource: Some(b"tree.msh".to_vec()),
            span: None,
        });
        let mut report = PrototypeGraphReport {
            root_count: 1,
            direct_reference_count: 1,
            resolved_count: 1,
            mesh_dependency_count: 1,
            ..PrototypeGraphReport::default()
        };

        extend_graph_report_with_visual_dependencies(
            &repo,
            &mut report,
            &mut graph,
            std::slice::from_ref(&prototype),
        );

        let wear_edge = graph
            .edges
            .iter()
            .find(|edge| edge.kind == fparkan_prototype::PrototypeGraphEdgeKind::MeshToWear)
            .expect("wear edge");
        let material_edge = graph
            .edges
            .iter()
            .find(|edge| edge.kind == fparkan_prototype::PrototypeGraphEdgeKind::WearToMaterial)
            .expect("material edge");
        let texture_edge = graph
            .edges
            .iter()
            .find(|edge| edge.kind == fparkan_prototype::PrototypeGraphEdgeKind::MaterialToTexture)
            .expect("texture edge");
        let lightmap_edge = graph
            .edges
            .iter()
            .find(|edge| edge.kind == fparkan_prototype::PrototypeGraphEdgeKind::WearToLightmap)
            .expect("lightmap edge");

        assert_eq!(
            wear_edge
                .provenance
                .as_ref()
                .expect("wear provenance")
                .parent_edge,
            Some(fparkan_prototype::PrototypeGraphEdgeId(1))
        );
        assert_eq!(
            material_edge
                .provenance
                .as_ref()
                .expect("material provenance")
                .parent_edge,
            Some(wear_edge.id)
        );
        assert_eq!(
            texture_edge
                .provenance
                .as_ref()
                .expect("texture provenance")
                .parent_edge,
            Some(material_edge.id)
        );
        assert_eq!(
            lightmap_edge
                .provenance
                .as_ref()
                .expect("lightmap provenance")
                .parent_edge,
            Some(wear_edge.id)
        );
        assert!(graph
            .edges
            .iter()
            .filter_map(|edge| edge.provenance.as_ref())
            .all(|provenance| provenance.root_index == 0));
        for edge in [wear_edge, material_edge, texture_edge, lightmap_edge] {
            assert_eq!(
                edge.provenance
                    .as_ref()
                    .and_then(|provenance| provenance.unit_component_index),
                Some(3)
            );
        }
    }

    #[test]
    fn graph_visual_dependency_expansion_is_idempotent() {
        let mesh_key = ResourceKey {
            archive: parse_path("static.rlb").expect("archive"),
            name: resource_name(b"tree.msh"),
            type_id: Some(0x4853_454D),
        };
        let mat0 = mat0_with_texture(b"TEX_A");
        let texm = texm_payload();
        let lightmap_texm = texm_payload();
        let repo = repository_with_archives_meta(&[
            (
                "static.rlb",
                &[TestNresEntry {
                    name: b"tree.wea",
                    payload: b"1\n0 MAT_A\n\nLIGHTMAPS\n1\n0 LM_A\n",
                    type_id: WEAR_KIND,
                    attr2: 0,
                }],
            ),
            (
                "material.lib",
                &[TestNresEntry {
                    name: b"MAT_A",
                    payload: &mat0,
                    type_id: MAT0_KIND,
                    attr2: 0,
                }],
            ),
            (
                TEXTURES_ARCHIVE,
                &[TestNresEntry {
                    name: b"TEX_A",
                    payload: &texm,
                    type_id: 0,
                    attr2: 0,
                }],
            ),
            (
                LIGHTMAP_ARCHIVE,
                &[TestNresEntry {
                    name: b"LM_A",
                    payload: &lightmap_texm,
                    type_id: 0,
                    attr2: 0,
                }],
            ),
        ]);
        let prototype = EffectivePrototype {
            key: fparkan_prototype::PrototypeKey(resource_name(b"tree")),
            geometry: PrototypeGeometry::Mesh(mesh_key),
            source: fparkan_prototype::PrototypeSource::DirectArchive,
            source_type: None,
            dependencies: Vec::new(),
            control_resource: None,
            point_resource: None,
            node_data_resource: None,
            bas_resource: None,
        };
        let mut graph = prototype_graph_for_mesh(&prototype);
        let mut report = PrototypeGraphReport {
            root_count: 1,
            direct_reference_count: 1,
            resolved_count: 1,
            mesh_dependency_count: 1,
            ..PrototypeGraphReport::default()
        };

        extend_graph_report_with_visual_dependencies(
            &repo,
            &mut report,
            &mut graph,
            std::slice::from_ref(&prototype),
        );
        let first = (graph.nodes.clone(), graph.edges.clone(), report.clone());

        extend_graph_report_with_visual_dependencies(
            &repo,
            &mut report,
            &mut graph,
            std::slice::from_ref(&prototype),
        );

        assert!(graph.visual_dependencies_expanded);
        assert_eq!(graph.nodes, first.0);
        assert_eq!(graph.edges, first.1);
        assert_eq!(report, first.2);
    }

    #[test]
    fn prepare_mission_assets_keep_instance_controls_beside_deduplicated_visuals() {
        let mesh_key = ResourceKey {
            archive: parse_path("static.rlb").expect("archive"),
            name: resource_name(b"tree.msh"),
            type_id: Some(0x4853_454D),
        };
        let mut node = vec![0; 38];
        node[8..].fill(0xff);
        let msh = minimal_model_archive_with_nodes(&node);
        let mat0 = mat0_with_texture(b"TEX_A");
        let texm = texm_payload();
        let lightmap_texm = texm_payload();
        let mut first_control = component_control_payload(&[(0, 1.25, 2.5, 0.25, 0)]);
        first_control[16..20].copy_from_slice(&1_u32.to_le_bytes());
        first_control.resize(first_control.len() + CONTROL_EFFECT_GROUP_HEADER_BYTES, 0);
        append_effect_table(
            &mut first_control,
            &[effect_row(
                4,
                [0; 3],
                [0; 3],
                17,
                "effects.rlb",
                "f_smalllight_y",
            )],
        );
        let second_control = component_control_payload(&[(0, 2.5, 3.75, 0.75, 0)]);
        let mut points = cpt_payload(1);
        write_cpt_point(
            &mut points,
            0,
            0x8000_0000,
            -23,
            -24,
            [2.0, 3.0, 4.0],
            [0.0, 0.0, 1.0],
            "LightAnchor",
        );
        let repo = repository_with_archives_meta(&[
            (
                "static.rlb",
                &[
                    TestNresEntry {
                        name: b"tree.msh",
                        payload: &msh,
                        type_id: 0x4853_454D,
                        attr2: 0,
                    },
                    TestNresEntry {
                        name: b"tree.wea",
                        payload: b"1\n0 MAT_A\n\nLIGHTMAPS\n1\n0 LM_A\n",
                        type_id: WEAR_KIND,
                        attr2: 0,
                    },
                ],
            ),
            (
                "material.lib",
                &[TestNresEntry {
                    name: b"MAT_A",
                    payload: &mat0,
                    type_id: MAT0_KIND,
                    attr2: 0,
                }],
            ),
            (
                TEXTURES_ARCHIVE,
                &[TestNresEntry {
                    name: b"TEX_A",
                    payload: &texm,
                    type_id: 0,
                    attr2: 0,
                }],
            ),
            (
                LIGHTMAP_ARCHIVE,
                &[TestNresEntry {
                    name: b"LM_A",
                    payload: &lightmap_texm,
                    type_id: 0,
                    attr2: 0,
                }],
            ),
            (
                "controls.rlb",
                &[
                    TestNresEntry {
                        name: b"first.ctl",
                        payload: &first_control,
                        type_id: 0,
                        attr2: 0,
                    },
                    TestNresEntry {
                        name: b"second.ctl",
                        payload: &second_control,
                        type_id: 0,
                        attr2: 0,
                    },
                ],
            ),
            (
                "points.rlb",
                &[TestNresEntry {
                    name: b"attachments.cpt",
                    payload: &points,
                    type_id: 0,
                    attr2: 0,
                }],
            ),
        ]);
        let prototypes = [
            EffectivePrototype {
                key: fparkan_prototype::PrototypeKey(resource_name(b"tree-a")),
                geometry: PrototypeGeometry::Mesh(mesh_key.clone()),
                source: fparkan_prototype::PrototypeSource::DirectArchive,
                source_type: None,
                dependencies: vec![mesh_key.clone()],
                control_resource: Some(ResourceKey {
                    archive: parse_path("controls.rlb").expect("control archive"),
                    name: resource_name(b"first.ctl"),
                    type_id: None,
                }),
                point_resource: Some(ResourceKey {
                    archive: parse_path("points.rlb").expect("point archive"),
                    name: resource_name(b"attachments.cpt"),
                    type_id: None,
                }),
                node_data_resource: None,
                bas_resource: None,
            },
            EffectivePrototype {
                key: fparkan_prototype::PrototypeKey(resource_name(b"tree-b")),
                geometry: PrototypeGeometry::Mesh(mesh_key.clone()),
                source: fparkan_prototype::PrototypeSource::DirectArchive,
                source_type: None,
                dependencies: vec![mesh_key.clone()],
                control_resource: Some(ResourceKey {
                    archive: parse_path("controls.rlb").expect("control archive"),
                    name: resource_name(b"second.ctl"),
                    type_id: None,
                }),
                point_resource: None,
                node_data_resource: None,
                bas_resource: None,
            },
            EffectivePrototype {
                key: fparkan_prototype::PrototypeKey(resource_name(b"tree-c")),
                geometry: PrototypeGeometry::Mesh(mesh_key.clone()),
                source: fparkan_prototype::PrototypeSource::DirectArchive,
                source_type: None,
                dependencies: vec![mesh_key],
                control_resource: Some(ResourceKey {
                    archive: parse_path("controls.rlb").expect("control archive"),
                    name: resource_name(b"missing.ctl"),
                    type_id: None,
                }),
                point_resource: None,
                node_data_resource: None,
                bas_resource: None,
            },
        ];

        let assets = prepare_mission_assets_with_repository(&repo, &[0..3], &prototypes)
            .expect("prepared mission assets");

        assert_eq!(assets.models.len(), 1);
        assert_eq!(assets.wears.len(), 1);
        assert_eq!(assets.materials.len(), 1);
        assert_eq!(assets.textures.len(), 2);
        assert_eq!(assets.visuals.len(), 1);
        assert_eq!(
            assets.object_visuals,
            vec![vec![
                assets.visuals[0].id,
                assets.visuals[0].id,
                assets.visuals[0].id
            ]]
        );
        assert_eq!(assets.object_component_controls.len(), 1);
        let controls = &assets.object_component_controls[0];
        assert_eq!(controls.len(), 3);
        assert_eq!(
            controls[0]
                .as_ref()
                .expect("first component control")
                .source
                .name,
            resource_name(b"first.ctl")
        );
        let first = controls[0].as_ref().expect("first component control");
        let points = first.point_list.as_ref().expect("explicit CPT was loaded");
        assert_eq!(points.source.archive.as_str(), "points.rlb");
        assert_eq!(points.source.name, resource_name(b"attachments.cpt"));
        assert_eq!(points.points.len(), 1);
        assert_eq!(points.points[0].raw_signed_field_04, -23);
        assert_eq!(points.points[0].raw_signed_field_08, -24);
        assert_eq!(points.points[0].name, fixed_name("LightAnchor"));
        assert_eq!(
            controls[1]
                .as_ref()
                .expect("second component control")
                .source
                .name,
            resource_name(b"second.ctl")
        );
        assert!(
            controls[2].is_none(),
            "missing explicit CTL means no override"
        );
        assert_eq!(
            controls[0].as_ref().expect("first control").bindings,
            vec![PreparedControlNodeBinding {
                node_index: 0,
                frame_a: 1.25,
                frame_b: 2.5,
                initial_blend: 0.25,
                flags: 0,
            }]
        );
        assert_eq!(
            controls[1].as_ref().expect("second control").bindings,
            vec![PreparedControlNodeBinding {
                node_index: 0,
                frame_a: 2.5,
                frame_b: 3.75,
                initial_blend: 0.75,
                flags: 0,
            }]
        );

        let visual = &assets.visuals[0];
        assert_eq!(visual.model_id, Some(assets.models[0].id));
        assert_eq!(visual.wear_id, Some(assets.wears[0].id));
        assert_eq!(visual.material_ids, vec![assets.materials[0].id]);
        assert_eq!(visual.texture_ids.len(), 1);
        assert_eq!(visual.lightmap_ids.len(), 1);
    }

    #[test]
    fn non_geometric_explicit_control_does_not_break_visual_preparation() {
        let repository = repository_with_archives(&[]);
        let prototype = EffectivePrototype {
            key: fparkan_prototype::PrototypeKey(resource_name(b"marker")),
            geometry: PrototypeGeometry::NonGeometric,
            source: fparkan_prototype::PrototypeSource::ObjectsRegistry,
            source_type: None,
            dependencies: Vec::new(),
            control_resource: Some(ResourceKey {
                archive: parse_path("controls.rlb").expect("control archive"),
                name: resource_name(b"marker.ctl"),
                type_id: None,
            }),
            point_resource: None,
            node_data_resource: None,
            bas_resource: None,
        };

        let assets = prepare_mission_assets_with_repository(&repository, &[0..1], &[prototype])
            .expect("non-geometric visual remains valid");

        assert_eq!(assets.object_visuals.len(), 1);
        assert_eq!(assets.object_component_controls, vec![vec![None]]);
    }

    #[test]
    fn texture_preparation_cache_reuses_exact_request_within_one_mission() {
        let texm = texm_payload();
        let repo = repository_with_archives_meta(&[(
            TEXTURES_ARCHIVE,
            &[TestNresEntry {
                name: b"TEX_A",
                payload: &texm,
                type_id: 0,
                attr2: 0,
            }],
        )]);
        let name = resource_name(b"TEX_A");
        let mut cache = PreparationCache::default();

        let first = prepare_texture_cached(
            &repo,
            &name,
            PreparedTextureUsage::Diffuse,
            AssetIdentityPolicy::default(),
            &mut cache,
        )
        .expect("first texture");
        let second = prepare_texture_cached(
            &repo,
            &name,
            PreparedTextureUsage::Diffuse,
            AssetIdentityPolicy::default(),
            &mut cache,
        )
        .expect("cached texture");

        assert_eq!(first.id, second.id);
        assert_eq!(cache.diffuse.len(), 1);
        assert!(cache.lightmaps.is_empty());
    }

    #[test]
    fn material_preparation_cache_preserves_exact_wear_request() {
        let mat0 = mat0_with_texture(b"TEX_A");
        let repo = repository_with_archives_meta(&[(
            "material.lib",
            &[TestNresEntry {
                name: b"MAT_A",
                payload: &mat0,
                type_id: MAT0_KIND,
                attr2: 0,
            }],
        )]);
        let wear = decode_wear(b"1\n0 MAT_A\n").expect("wear");
        let mut cache = PreparationCache::default();

        let first = resolve_material_cached(&repo, &wear, 0, &mut cache).expect("first material");
        let second = resolve_material_cached(&repo, &wear, 0, &mut cache).expect("cached material");

        assert_eq!(first, second);
        assert_eq!(cache.materials.len(), 1);
    }

    #[test]
    fn forced_model_id_collision_is_rejected() {
        assert_forced_collision(
            AssetIdentityPolicy {
                model: |_| 7,
                ..AssetIdentityPolicy::default()
            },
            "stable model id collision",
        );
    }

    #[test]
    fn forced_wear_id_collision_is_rejected() {
        assert_forced_collision(
            AssetIdentityPolicy {
                wear: |_| 11,
                ..AssetIdentityPolicy::default()
            },
            "stable wear id collision",
        );
    }

    #[test]
    fn forced_material_id_collision_is_rejected() {
        assert_forced_collision(
            AssetIdentityPolicy {
                material: |_, _, _| 13,
                ..AssetIdentityPolicy::default()
            },
            "stable material id collision",
        );
    }

    #[test]
    fn forced_texture_id_collision_is_rejected() {
        assert_forced_collision(
            AssetIdentityPolicy {
                texture: |_, _| 17,
                ..AssetIdentityPolicy::default()
            },
            "stable texture id collision",
        );
    }

    #[test]
    fn profiled_asset_preparation_reports_unique_asset_counts() {
        let (repo, prototypes) = collision_fixture();

        let (assets, report) = prepare_mission_assets_profiled_with_repository(
            &repo,
            &[0..1, 1..2],
            &prototypes,
            AssetPreparationLimits::default(),
        )
        .expect("profiled assets");

        assert_eq!(report.model_count, assets.models.len());
        assert_eq!(report.wear_count, assets.wears.len());
        assert_eq!(report.material_count, assets.materials.len());
        assert_eq!(report.texture_count, assets.textures.len());
        assert!(report.texture_pixels > 0);
    }

    #[test]
    fn asset_preparation_limits_reject_texture_pixel_budget() {
        let (repo, prototypes) = collision_fixture();

        let err = prepare_mission_assets_profiled_with_repository(
            &repo,
            &[0..1, 1..2],
            &prototypes,
            AssetPreparationLimits {
                max_texture_pixels: Some(1),
                ..AssetPreparationLimits::default()
            },
        )
        .expect_err("budget should fail");

        assert!(matches!(err, AssetError::BudgetExceeded(_)));
        assert!(err.to_string().contains("texture_pixels"));
    }

    #[test]
    fn graph_report_uses_strict_texture_archive_policy() {
        let mesh_key = ResourceKey {
            archive: parse_path("static.rlb").expect("archive"),
            name: resource_name(b"tree.msh"),
            type_id: Some(0x4853_454D),
        };
        let mat0 = mat0_with_texture(b"TEX_A");
        let texm = texm_payload();
        let repo = repository_with_archives_meta(&[
            (
                "static.rlb",
                &[TestNresEntry {
                    name: b"tree.wea",
                    payload: b"1\n0 MAT_A\n",
                    type_id: WEAR_KIND,
                    attr2: 0,
                }],
            ),
            (
                "material.lib",
                &[TestNresEntry {
                    name: b"MAT_A",
                    payload: &mat0,
                    type_id: MAT0_KIND,
                    attr2: 0,
                }],
            ),
            (
                LIGHTMAP_ARCHIVE,
                &[TestNresEntry {
                    name: b"TEX_A",
                    payload: &texm,
                    type_id: 0,
                    attr2: 0,
                }],
            ),
        ]);
        let prototype = EffectivePrototype {
            key: fparkan_prototype::PrototypeKey(resource_name(b"tree")),
            geometry: PrototypeGeometry::Mesh(mesh_key.clone()),
            source: fparkan_prototype::PrototypeSource::DirectArchive,
            source_type: None,
            dependencies: vec![mesh_key.clone()],
            control_resource: None,
            point_resource: None,
            node_data_resource: None,
            bas_resource: None,
        };
        let mut graph = prototype_graph_for_mesh(&prototype);
        let mut report = PrototypeGraphReport {
            root_count: 1,
            direct_reference_count: 1,
            resolved_count: 1,
            mesh_dependency_count: 1,
            ..PrototypeGraphReport::default()
        };

        extend_graph_report_with_visual_dependencies(
            &repo,
            &mut report,
            &mut graph,
            std::slice::from_ref(&prototype),
        );

        assert_eq!(report.texture_request_count, 1);
        assert_eq!(report.texture_resolved_count, 0);
        assert_eq!(report.failures.len(), 1);
        assert_eq!(
            report.failures[0].edge,
            PrototypeGraphEdge::MaterialToTexture
        );
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn prepares_real_unit_asset_plan() {
        let root = fixture_root("IS");
        let vfs: Arc<dyn Vfs> = Arc::new(DirectoryVfs::new(&root));
        let repository = CachedResourceRepository::new(Arc::clone(&vfs));
        let roots = [resource_name(b"UNITS/AUTO/swlklas.dat")];

        let (graph, prototypes) =
            build_prototype_graph(&repository, vfs.as_ref(), &roots).expect("prototype graph");
        let count_only = build_mission_asset_plan(&graph);
        let plan = build_mission_asset_plan_with_repository(&repository, &prototypes)
            .expect("asset preparation");

        assert_eq!(count_only.visual_count, 12);
        assert_eq!(prototypes.len(), 12);
        assert_eq!(plan.visual_count, 11);
        assert_eq!(plan.model_count, 11);
        assert_eq!(plan.material_count, 62);
        assert_eq!(plan.texture_count, 77);
        assert_eq!(plan.lightmap_count, 0);
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn repository_plan_deduplicates_duplicate_visuals_but_graph_preserves_requests() {
        let root = fixture_root("IS");
        let vfs: Arc<dyn Vfs> = Arc::new(DirectoryVfs::new(&root));
        let repository = CachedResourceRepository::new(Arc::clone(&vfs));
        let roots = [
            resource_name(b"UNITS/AUTO/swlklas.dat"),
            resource_name(b"UNITS/AUTO/swlklas.dat"),
        ];

        let (graph, prototypes) =
            build_prototype_graph(&repository, vfs.as_ref(), &roots).expect("prototype graph");
        let count_only = build_mission_asset_plan(&graph);
        let plan = build_mission_asset_plan_with_repository(&repository, &prototypes)
            .expect("asset preparation");

        assert_eq!(graph.roots.len(), 2);
        assert_eq!(count_only.visual_count, 24);
        assert_eq!(prototypes.len(), 24);
        assert_eq!(plan.visual_count, 11);
        assert_eq!(plan.model_count, 11);
        assert_eq!(plan.material_count, 62);
        assert_eq!(plan.texture_count, 77);
    }

    fn assert_forced_collision(policy: AssetIdentityPolicy, expected: &str) {
        let (repo, prototypes) = collision_fixture();

        let err = prepare_mission_assets_with_repository_internal(
            &repo,
            &[0..1, 1..2],
            &prototypes,
            policy,
            AssetPreparationLimits::default(),
        )
        .expect_err("collision should fail");

        assert!(err.to_string().contains(expected), "{err}");
    }

    fn fixture_root(part: &str) -> PathBuf {
        let variable = match part {
            "IS" => "FPARKAN_CORPUS_PART1_ROOT",
            "IS2" => "FPARKAN_CORPUS_PART2_ROOT",
            _ => panic!("unknown licensed corpus part: {part}"),
        };
        let root = std::env::var_os(variable)
            .map(PathBuf::from)
            .unwrap_or_else(|| panic!("{variable} is required for licensed corpus tests"));
        assert!(
            root.is_dir(),
            "licensed corpus root is missing: {}",
            root.display()
        );
        root
    }

    fn collision_fixture() -> (CachedResourceRepository, Vec<EffectivePrototype>) {
        let msh = minimal_model_archive();
        let mat0_a = mat0_with_texture(b"TEX_A");
        let mat0_b = mat0_with_texture(b"TEX_B");
        let texm_a = texm_payload();
        let texm_b = texm_payload();
        let lightmap_a = texm_payload();
        let lightmap_b = texm_payload();
        let repo = repository_with_archives_meta(&[
            (
                "static.rlb",
                &[
                    TestNresEntry {
                        name: b"tree_a.msh",
                        payload: &msh,
                        type_id: 0x4853_454D,
                        attr2: 0,
                    },
                    TestNresEntry {
                        name: b"tree_a.wea",
                        payload: b"1\n0 MAT_A\n\nLIGHTMAPS\n1\n0 LM_A\n",
                        type_id: WEAR_KIND,
                        attr2: 0,
                    },
                    TestNresEntry {
                        name: b"tree_b.msh",
                        payload: &msh,
                        type_id: 0x4853_454D,
                        attr2: 0,
                    },
                    TestNresEntry {
                        name: b"tree_b.wea",
                        payload: b"1\n0 MAT_B\n\nLIGHTMAPS\n1\n0 LM_B\n",
                        type_id: WEAR_KIND,
                        attr2: 0,
                    },
                ],
            ),
            (
                "material.lib",
                &[
                    TestNresEntry {
                        name: b"MAT_A",
                        payload: &mat0_a,
                        type_id: MAT0_KIND,
                        attr2: 0,
                    },
                    TestNresEntry {
                        name: b"MAT_B",
                        payload: &mat0_b,
                        type_id: MAT0_KIND,
                        attr2: 0,
                    },
                ],
            ),
            (
                TEXTURES_ARCHIVE,
                &[
                    TestNresEntry {
                        name: b"TEX_A",
                        payload: &texm_a,
                        type_id: 0,
                        attr2: 0,
                    },
                    TestNresEntry {
                        name: b"TEX_B",
                        payload: &texm_b,
                        type_id: 0,
                        attr2: 0,
                    },
                ],
            ),
            (
                LIGHTMAP_ARCHIVE,
                &[
                    TestNresEntry {
                        name: b"LM_A",
                        payload: &lightmap_a,
                        type_id: 0,
                        attr2: 0,
                    },
                    TestNresEntry {
                        name: b"LM_B",
                        payload: &lightmap_b,
                        type_id: 0,
                        attr2: 0,
                    },
                ],
            ),
        ]);
        let prototypes = vec![
            EffectivePrototype {
                key: fparkan_prototype::PrototypeKey(resource_name(b"tree_a")),
                geometry: PrototypeGeometry::Mesh(ResourceKey {
                    archive: parse_path("static.rlb").expect("archive"),
                    name: resource_name(b"tree_a.msh"),
                    type_id: Some(0x4853_454D),
                }),
                source: fparkan_prototype::PrototypeSource::DirectArchive,
                source_type: None,
                dependencies: Vec::new(),
                control_resource: None,
                point_resource: None,
                node_data_resource: None,
                bas_resource: None,
            },
            EffectivePrototype {
                key: fparkan_prototype::PrototypeKey(resource_name(b"tree_b")),
                geometry: PrototypeGeometry::Mesh(ResourceKey {
                    archive: parse_path("static.rlb").expect("archive"),
                    name: resource_name(b"tree_b.msh"),
                    type_id: Some(0x4853_454D),
                }),
                source: fparkan_prototype::PrototypeSource::DirectArchive,
                source_type: None,
                dependencies: Vec::new(),
                control_resource: None,
                point_resource: None,
                node_data_resource: None,
                bas_resource: None,
            },
        ];
        (repo, prototypes)
    }

    fn repository_with_archives(
        archives: &[(&str, &[(&[u8], &[u8])])],
    ) -> CachedResourceRepository {
        let mut vfs = MemoryVfs::default();
        for (archive, entries) in archives {
            let path = parse_path(archive).expect("archive path");
            vfs.insert(path, Arc::from(build_nres(entries).into_boxed_slice()));
        }
        CachedResourceRepository::new(Arc::new(vfs))
    }

    #[derive(Clone, Copy)]
    struct TestNresEntry<'a> {
        name: &'a [u8],
        payload: &'a [u8],
        type_id: u32,
        attr2: u32,
    }

    fn repository_with_archives_meta(
        archives: &[(&str, &[TestNresEntry<'_>])],
    ) -> CachedResourceRepository {
        let mut vfs = MemoryVfs::default();
        for (archive, entries) in archives {
            let path = parse_path(archive).expect("archive path");
            vfs.insert(
                path,
                Arc::from(build_nres_with_meta(entries).into_boxed_slice()),
            );
        }
        CachedResourceRepository::new(Arc::new(vfs))
    }

    #[test]
    fn fx_material_loader_uses_the_archive_named_by_the_fx_command() {
        let fx_mat0 = mat0_with_texture(b"FX_LAMP_TEX");
        let default_mat0 = mat0_with_texture(b"DEFAULT_TEX");
        let texm = texm_payload_with_page_rect();
        let repo = repository_with_archives_meta(&[
            (
                "effects.rlb",
                &[TestNresEntry {
                    name: b"lamp",
                    payload: &fx_mat0,
                    type_id: MAT0_KIND,
                    attr2: 0,
                }],
            ),
            (
                "material.lib",
                &[TestNresEntry {
                    name: b"lamp",
                    payload: &default_mat0,
                    type_id: MAT0_KIND,
                    attr2: 0,
                }],
            ),
            (
                TEXTURES_ARCHIVE,
                &[
                    TestNresEntry {
                        name: b"FX_LAMP_TEX",
                        payload: &texm,
                        type_id: 0,
                        attr2: 0,
                    },
                    TestNresEntry {
                        name: b"DEFAULT_TEX",
                        payload: &texm,
                        type_id: 0,
                        attr2: 0,
                    },
                ],
            ),
        ]);

        let (_, _, document) = load_material_archive_name_texture_rgba8_and_phase_from_repository(
            &repo,
            "effects.rlb",
            "lamp",
            0,
        )
        .expect("FX-selected archive has a valid MAT0");
        assert_eq!(
            document.primary_texture(),
            Some(resource_name(b"FX_LAMP_TEX"))
        );
    }

    fn texm_payload() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&0x6d78_6554_u32.to_le_bytes());
        out.extend_from_slice(&1_u32.to_le_bytes());
        out.extend_from_slice(&1_u32.to_le_bytes());
        out.extend_from_slice(&1_u32.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&565_u32.to_le_bytes());
        out.extend_from_slice(&0xffff_u16.to_le_bytes());
        out
    }

    fn texm_payload_with_page_rect() -> Vec<u8> {
        let mut out = texm_payload();
        out.extend_from_slice(&0x6567_6150_u32.to_le_bytes());
        out.extend_from_slice(&1_u32.to_le_bytes());
        out.extend_from_slice(&0_i16.to_le_bytes());
        out.extend_from_slice(&1_i16.to_le_bytes());
        out.extend_from_slice(&0_i16.to_le_bytes());
        out.extend_from_slice(&1_i16.to_le_bytes());
        out
    }

    fn mat0_with_texture(texture: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0; 4 + 34];
        bytes[0..2].copy_from_slice(&1_u16.to_le_bytes());
        let len = texture.len().min(16);
        bytes[22..22 + len].copy_from_slice(&texture[..len]);
        bytes
    }

    fn minimal_model_archive() -> Vec<u8> {
        minimal_model_archive_with_nodes(&[])
    }

    fn minimal_model_archive_with_nodes(nodes: &[u8]) -> Vec<u8> {
        struct MshEntry<'a> {
            type_id: u32,
            attr3: u32,
            name: &'a [u8],
            payload: &'a [u8],
        }

        let entries = [
            MshEntry {
                type_id: 1,
                attr3: 38,
                name: b"Res1",
                payload: nodes,
            },
            MshEntry {
                type_id: 2,
                attr3: 0,
                name: b"Res2",
                payload: &[0; 0x8c],
            },
            MshEntry {
                type_id: 3,
                attr3: 0,
                name: b"Res3",
                payload: &[],
            },
            MshEntry {
                type_id: 6,
                attr3: 0,
                name: b"Res6",
                payload: &[],
            },
            MshEntry {
                type_id: 13,
                attr3: 0,
                name: b"Res13",
                payload: &[],
            },
        ];

        let mut out = vec![0; 16];
        let mut offsets = Vec::with_capacity(entries.len());
        for entry in &entries {
            offsets.push(u32::try_from(out.len()).expect("offset"));
            out.extend_from_slice(entry.payload);
            let padding = (8 - (out.len() % 8)) % 8;
            out.resize(out.len() + padding, 0);
        }
        let mut order: Vec<usize> = (0..entries.len()).collect();
        order.sort_by(|left, right| entries[*left].name.cmp(entries[*right].name));
        for (idx, entry) in entries.iter().enumerate() {
            push_u32(&mut out, entry.type_id);
            push_u32(&mut out, 0);
            push_u32(&mut out, 0);
            push_u32(
                &mut out,
                u32::try_from(entry.payload.len()).expect("payload"),
            );
            push_u32(&mut out, entry.attr3);
            let mut name_raw = [0; 36];
            let len = name_raw.len().saturating_sub(1).min(entry.name.len());
            name_raw[..len].copy_from_slice(&entry.name[..len]);
            out.extend_from_slice(&name_raw);
            push_u32(&mut out, offsets[idx]);
            push_u32(&mut out, u32::try_from(order[idx]).expect("sort index"));
        }
        out[0..4].copy_from_slice(b"NRes");
        out[4..8].copy_from_slice(&0x100_u32.to_le_bytes());
        out[8..12].copy_from_slice(&u32::try_from(entries.len()).expect("count").to_le_bytes());
        let total_size = u32::try_from(out.len()).expect("total size");
        out[12..16].copy_from_slice(&total_size.to_le_bytes());
        out
    }

    fn prototype_graph_for_mesh(prototype: &EffectivePrototype) -> PrototypeGraph {
        let root_node = fparkan_prototype::PrototypeGraphNode::root(
            fparkan_prototype::PrototypeKey(prototype.key.0.clone()),
            false,
            fparkan_prototype::PrototypeGraphNodeId(0),
        );
        let prototype_node = fparkan_prototype::PrototypeGraphNode::prototype(
            prototype.key.clone(),
            fparkan_prototype::PrototypeGraphNodeId(1),
        );
        let mesh_key = match &prototype.geometry {
            PrototypeGeometry::Mesh(mesh) => mesh.clone(),
            PrototypeGeometry::NonGeometric => panic!("mesh prototype expected"),
        };
        let mesh_node = fparkan_prototype::PrototypeGraphNode::mesh(
            mesh_key,
            fparkan_prototype::PrototypeGraphNodeId(2),
        );
        PrototypeGraph {
            roots: vec![prototype.key.clone()],
            prototype_requests: vec![prototype.key.clone()],
            root_prototype_request_spans: std::iter::once(0..1).collect(),
            root_unit_components: vec![Vec::new()],
            root_unit_dat_header_flags: vec![None],
            visual_dependencies_expanded: false,
            nodes: vec![root_node, prototype_node, mesh_node],
            edges: vec![
                fparkan_prototype::PrototypeGraphEdgeInstance {
                    id: fparkan_prototype::PrototypeGraphEdgeId(0),
                    from: fparkan_prototype::PrototypeGraphNodeId(0),
                    to: fparkan_prototype::PrototypeGraphNodeId(1),
                    kind: fparkan_prototype::PrototypeGraphEdgeKind::MissionToRoot,
                    requiredness: PrototypeGraphRequiredness::Required,
                    provenance: None,
                },
                fparkan_prototype::PrototypeGraphEdgeInstance {
                    id: fparkan_prototype::PrototypeGraphEdgeId(1),
                    from: fparkan_prototype::PrototypeGraphNodeId(1),
                    to: fparkan_prototype::PrototypeGraphNodeId(2),
                    kind: fparkan_prototype::PrototypeGraphEdgeKind::PrototypeToMesh,
                    requiredness: PrototypeGraphRequiredness::Required,
                    provenance: None,
                },
            ],
        }
    }

    fn build_nres(entries: &[(&[u8], &[u8])]) -> Vec<u8> {
        let mut out = vec![0; 16];
        let mut offsets = Vec::with_capacity(entries.len());
        for (_, payload) in entries {
            offsets.push(u32::try_from(out.len()).expect("offset"));
            out.extend_from_slice(payload);
            let padding = (8 - (out.len() % 8)) % 8;
            out.resize(out.len() + padding, 0);
        }
        let mut order: Vec<usize> = (0..entries.len()).collect();
        order.sort_by(|left, right| entries[*left].0.cmp(entries[*right].0));
        for (idx, (name, payload)) in entries.iter().enumerate() {
            push_u32(&mut out, 0);
            push_u32(&mut out, 0);
            push_u32(&mut out, 0);
            push_u32(&mut out, u32::try_from(payload.len()).expect("payload"));
            push_u32(&mut out, 0);
            let mut name_raw = [0; 36];
            let len = name_raw.len().saturating_sub(1).min(name.len());
            name_raw[..len].copy_from_slice(&name[..len]);
            out.extend_from_slice(&name_raw);
            push_u32(&mut out, offsets[idx]);
            push_u32(&mut out, u32::try_from(order[idx]).expect("sort index"));
        }
        out[0..4].copy_from_slice(b"NRes");
        out[4..8].copy_from_slice(&0x100_u32.to_le_bytes());
        out[8..12].copy_from_slice(&u32::try_from(entries.len()).expect("count").to_le_bytes());
        let total_size = u32::try_from(out.len()).expect("total size");
        out[12..16].copy_from_slice(&total_size.to_le_bytes());
        out
    }

    fn build_nres_with_meta(entries: &[TestNresEntry<'_>]) -> Vec<u8> {
        let mut out = vec![0; 16];
        let mut offsets = Vec::with_capacity(entries.len());
        for entry in entries {
            offsets.push(u32::try_from(out.len()).expect("offset"));
            out.extend_from_slice(entry.payload);
            let padding = (8 - (out.len() % 8)) % 8;
            out.resize(out.len() + padding, 0);
        }
        let mut order: Vec<usize> = (0..entries.len()).collect();
        order.sort_by(|left, right| entries[*left].name.cmp(entries[*right].name));
        for (idx, entry) in entries.iter().enumerate() {
            push_u32(&mut out, entry.type_id);
            push_u32(&mut out, 0);
            push_u32(&mut out, entry.attr2);
            push_u32(
                &mut out,
                u32::try_from(entry.payload.len()).expect("payload"),
            );
            push_u32(&mut out, 0);
            let mut name_raw = [0; 36];
            let len = name_raw.len().saturating_sub(1).min(entry.name.len());
            name_raw[..len].copy_from_slice(&entry.name[..len]);
            out.extend_from_slice(&name_raw);
            push_u32(&mut out, offsets[idx]);
            push_u32(&mut out, u32::try_from(order[idx]).expect("sort index"));
        }
        out[0..4].copy_from_slice(b"NRes");
        out[4..8].copy_from_slice(&0x100_u32.to_le_bytes());
        out[8..12].copy_from_slice(&u32::try_from(entries.len()).expect("count").to_le_bytes());
        let total_size = u32::try_from(out.len()).expect("total size");
        out[12..16].copy_from_slice(&total_size.to_le_bytes());
        out
    }

    fn push_u32(out: &mut Vec<u8>, value: u32) {
        out.extend_from_slice(&value.to_le_bytes());
    }
}
