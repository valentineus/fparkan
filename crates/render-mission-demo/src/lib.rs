use encoding_rs::WINDOWS_1251;
use nres::Archive;
use render_core::{build_render_mesh, RenderMesh};
use render_demo::{load_model_with_name_from_archive, resolve_texture_for_model, LoadedTexture};
use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use terrain_core::TerrainMesh;
use tma::MissionFile;

const MAT0_KIND: u32 = 0x3054_414D;
const MESH_KIND: u32 = 0x4853_454D;
const OBJECT_REF_STRIDE: usize = 64;
const OBJECT_REF_ARCHIVE_BYTES: usize = 32;

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Mission(tma::Error),
    Terrain(terrain_core::Error),
    UnitDat(unitdat::Error),
    RenderDemo(render_demo::Error),
    Nres(nres::error::Error),
    Texm(texm::error::Error),
    InvalidMapPath(String),
    GameRootNotFound(PathBuf),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "{err}"),
            Self::Mission(err) => write!(f, "{err}"),
            Self::Terrain(err) => write!(f, "{err}"),
            Self::UnitDat(err) => write!(f, "{err}"),
            Self::RenderDemo(err) => write!(f, "{err}"),
            Self::Nres(err) => write!(f, "{err}"),
            Self::Texm(err) => write!(f, "{err}"),
            Self::InvalidMapPath(path) => write!(f, "invalid mission map path: {path}"),
            Self::GameRootNotFound(path) => {
                write!(
                    f,
                    "failed to detect game root from mission path {}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Mission(err) => Some(err),
            Self::Terrain(err) => Some(err),
            Self::UnitDat(err) => Some(err),
            Self::RenderDemo(err) => Some(err),
            Self::Nres(err) => Some(err),
            Self::Texm(err) => Some(err),
            Self::InvalidMapPath(_) | Self::GameRootNotFound(_) => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<tma::Error> for Error {
    fn from(value: tma::Error) -> Self {
        Self::Mission(value)
    }
}

impl From<terrain_core::Error> for Error {
    fn from(value: terrain_core::Error) -> Self {
        Self::Terrain(value)
    }
}

impl From<unitdat::Error> for Error {
    fn from(value: unitdat::Error) -> Self {
        Self::UnitDat(value)
    }
}

impl From<render_demo::Error> for Error {
    fn from(value: render_demo::Error) -> Self {
        Self::RenderDemo(value)
    }
}

impl From<nres::error::Error> for Error {
    fn from(value: nres::error::Error) -> Self {
        Self::Nres(value)
    }
}

impl From<texm::error::Error> for Error {
    fn from(value: texm::error::Error) -> Self {
        Self::Texm(value)
    }
}

#[derive(Copy, Clone, Debug)]
pub struct LoadOptions {
    pub load_model_textures: bool,
    pub load_terrain_texture: bool,
}

impl Default for LoadOptions {
    fn default() -> Self {
        Self {
            load_model_textures: true,
            load_terrain_texture: true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct MissionScene {
    pub game_root: PathBuf,
    pub mission_path: PathBuf,
    pub mission: MissionFile,
    pub map_folder_rel: PathBuf,
    pub land_msh_path: PathBuf,
    pub terrain: TerrainMesh,
    pub terrain_texture: Option<LoadedTexture>,
    pub models: Vec<SceneModel>,
    pub skipped_objects: usize,
}

#[derive(Clone, Debug)]
pub struct SceneModel {
    pub archive_path: PathBuf,
    pub model_name: String,
    pub mesh: RenderMesh,
    pub texture: Option<LoadedTexture>,
    pub instances: Vec<ModelInstance>,
}

#[derive(Copy, Clone, Debug)]
pub struct ModelInstance {
    pub position: [f32; 3],
    pub yaw_rad: f32,
    pub scale: [f32; 3],
}

#[derive(Clone, Debug)]
struct ObjectPrototype {
    archive_path: PathBuf,
    model_name: String,
}

#[derive(Clone, Debug)]
struct ObjectRef {
    archive_name: String,
    resource_name: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct ModelKey {
    archive_path: PathBuf,
    model_name: String,
}

pub fn detect_game_root_from_mission_path(mission_path: &Path) -> Option<PathBuf> {
    let mut cursor = mission_path.parent();
    while let Some(dir) = cursor {
        if dir.join("DATA").is_dir() && dir.join("objects.rlb").is_file() {
            return Some(dir.to_path_buf());
        }
        cursor = dir.parent();
    }
    None
}

pub fn load_scene(
    game_root: impl AsRef<Path>,
    mission_path: impl AsRef<Path>,
) -> Result<MissionScene> {
    load_scene_with_options(game_root, mission_path, LoadOptions::default())
}

pub fn load_scene_with_options(
    game_root: impl AsRef<Path>,
    mission_path: impl AsRef<Path>,
    options: LoadOptions,
) -> Result<MissionScene> {
    let game_root = game_root.as_ref().to_path_buf();
    let mission_path = mission_path.as_ref().to_path_buf();

    let mission = tma::parse_path(&mission_path)?;
    let map_folder_rel = map_folder_from_footer(&mission.footer.map_path)?;
    let land_msh_path = game_root.join(&map_folder_rel).join("Land.msh");
    let terrain = terrain_core::load_land_mesh(&land_msh_path)?;
    let terrain_texture = if options.load_terrain_texture {
        resolve_terrain_texture(&game_root, &map_folder_rel)?
    } else {
        None
    };

    let mut grouped_instances: HashMap<ModelKey, Vec<ModelInstance>> = HashMap::new();
    let mut prototype_cache: HashMap<String, Option<ObjectPrototype>> = HashMap::new();
    let mut skipped = 0usize;

    for object in &mission.objects {
        let cache_key = object.resource_name.to_ascii_lowercase();
        let proto = if let Some(cached) = prototype_cache.get(&cache_key) {
            cached.clone()
        } else {
            let resolved = resolve_object_prototype(&game_root, object)?;
            prototype_cache.insert(cache_key, resolved.clone());
            resolved
        };

        let Some(proto) = proto else {
            skipped += 1;
            continue;
        };

        let instance = ModelInstance {
            position: object.position,
            yaw_rad: object.orientation[2],
            scale: normalize_scale(object.scale),
        };

        grouped_instances
            .entry(ModelKey {
                archive_path: proto.archive_path,
                model_name: proto.model_name,
            })
            .or_default()
            .push(instance);
    }

    let mut models = Vec::new();
    for (key, instances) in grouped_instances {
        let loaded =
            match load_model_with_name_from_archive(&key.archive_path, Some(&key.model_name)) {
                Ok(v) => v,
                Err(_) => {
                    skipped += instances.len();
                    continue;
                }
            };

        let mesh = build_render_mesh(&loaded.model, 0, 0);
        if mesh.indices.is_empty() {
            skipped += instances.len();
            continue;
        }

        let texture = if options.load_model_textures {
            resolve_texture_for_model(&key.archive_path, &loaded.name, None, None, None, None)
                .ok()
                .flatten()
        } else {
            None
        };

        models.push(SceneModel {
            archive_path: key.archive_path,
            model_name: loaded.name,
            mesh,
            texture,
            instances,
        });
    }

    models.sort_by(|a, b| a.model_name.cmp(&b.model_name));

    Ok(MissionScene {
        game_root,
        mission_path,
        mission,
        map_folder_rel,
        land_msh_path,
        terrain,
        terrain_texture,
        models,
        skipped_objects: skipped,
    })
}

pub fn compute_scene_bounds(scene: &MissionScene) -> Option<([f32; 3], [f32; 3])> {
    let mut min_v = [f32::INFINITY; 3];
    let mut max_v = [f32::NEG_INFINITY; 3];
    let mut any = false;

    for pos in &scene.terrain.positions {
        merge_bounds(&mut min_v, &mut max_v, *pos);
        any = true;
    }

    for model in &scene.models {
        for instance in &model.instances {
            merge_bounds(&mut min_v, &mut max_v, instance.position);
            any = true;
        }
    }

    any.then_some((min_v, max_v))
}

fn merge_bounds(min_v: &mut [f32; 3], max_v: &mut [f32; 3], p: [f32; 3]) {
    for i in 0..3 {
        if p[i] < min_v[i] {
            min_v[i] = p[i];
        }
        if p[i] > max_v[i] {
            max_v[i] = p[i];
        }
    }
}

fn normalize_scale(scale: [f32; 3]) -> [f32; 3] {
    let mut out = scale;
    for item in &mut out {
        if !item.is_finite() || item.abs() < 0.000_1 {
            *item = 1.0;
        }
    }
    out
}

fn map_folder_from_footer(map_path: &str) -> Result<PathBuf> {
    let mut parts = split_relative_path(map_path);
    if parts.len() < 2 {
        return Err(Error::InvalidMapPath(map_path.to_string()));
    }
    parts.pop(); // remove 'land'

    let mut out = PathBuf::new();
    for part in parts {
        out.push(part);
    }
    Ok(out)
}

fn resolve_object_prototype(
    game_root: &Path,
    object: &tma::MissionObject,
) -> Result<Option<ObjectPrototype>> {
    if object.resource_name.to_ascii_lowercase().ends_with(".dat") {
        let dat_path = game_root.join(pathbuf_from_rel(&object.resource_name));
        if !dat_path.is_file() {
            return Ok(None);
        }

        let parsed = unitdat::parse_path(&dat_path)?;
        let archive_path = game_root.join(pathbuf_from_rel(&parsed.archive_name));
        if !archive_path.is_file() {
            return Ok(None);
        }
        return resolve_archive_model(game_root, &archive_path, &parsed.model_key);
    }

    let archive_path = game_root.join("objects.rlb");
    if !archive_path.is_file() {
        return Ok(None);
    }
    resolve_archive_model(game_root, &archive_path, &object.resource_name)
}

fn resolve_archive_model(
    game_root: &Path,
    archive_path: &Path,
    model_key: &str,
) -> Result<Option<ObjectPrototype>> {
    if !archive_path.is_file() {
        return Ok(None);
    }

    if is_objects_registry_archive(archive_path) {
        if let Some(proto) = resolve_objects_registry_model(game_root, archive_path, model_key)? {
            return Ok(Some(proto));
        }
    }

    let model_name = ensure_msh_suffix(model_key);
    if !archive_has_mesh_entry(archive_path, &model_name)? {
        return Ok(None);
    }

    Ok(Some(ObjectPrototype {
        archive_path: archive_path.to_path_buf(),
        model_name: model_name.to_ascii_lowercase(),
    }))
}

fn is_objects_registry_archive(archive_path: &Path) -> bool {
    archive_path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("objects.rlb"))
}

fn resolve_objects_registry_model(
    game_root: &Path,
    registry_archive_path: &Path,
    object_key: &str,
) -> Result<Option<ObjectPrototype>> {
    let archive = Archive::open_path(registry_archive_path)?;
    let Some(entry_id) = find_registry_entry_id(&archive, object_key) else {
        return Ok(None);
    };

    let payload = archive.read(entry_id)?.into_owned();
    let refs = parse_object_refs(&payload);
    if refs.is_empty() {
        return Ok(None);
    }

    for item in refs
        .iter()
        .filter(|item| has_extension(&item.resource_name, "msh"))
    {
        if let Some(proto) = resolve_object_ref_model(game_root, item, &item.resource_name)? {
            return Ok(Some(proto));
        }
    }

    for item in refs
        .iter()
        .filter(|item| has_extension(&item.resource_name, "bas"))
    {
        let Some(stem) = Path::new(&item.resource_name)
            .file_stem()
            .and_then(|stem| stem.to_str())
        else {
            continue;
        };
        if stem.is_empty() {
            continue;
        }
        let candidate = format!("{stem}.msh");
        if let Some(proto) = resolve_object_ref_model(game_root, item, &candidate)? {
            return Ok(Some(proto));
        }
    }

    Ok(None)
}

fn find_registry_entry_id(archive: &Archive, object_key: &str) -> Option<nres::EntryId> {
    mesh_name_candidates(object_key)
        .into_iter()
        .find_map(|candidate| archive.find(&candidate))
}

fn resolve_object_ref_model(
    game_root: &Path,
    item: &ObjectRef,
    model_name: &str,
) -> Result<Option<ObjectPrototype>> {
    let archive_path = game_root.join(pathbuf_from_rel(&item.archive_name));
    if !archive_path.is_file() {
        return Ok(None);
    }
    if !archive_has_mesh_entry(&archive_path, model_name)? {
        return Ok(None);
    }

    Ok(Some(ObjectPrototype {
        archive_path,
        model_name: model_name.to_ascii_lowercase(),
    }))
}

fn parse_object_refs(payload: &[u8]) -> Vec<ObjectRef> {
    if !payload.len().is_multiple_of(OBJECT_REF_STRIDE) {
        return Vec::new();
    }

    let mut refs = Vec::with_capacity(payload.len() / OBJECT_REF_STRIDE);
    for chunk in payload.chunks_exact(OBJECT_REF_STRIDE) {
        let archive_name = decode_cp1251_cstr(&chunk[..OBJECT_REF_ARCHIVE_BYTES]);
        let resource_name = decode_cp1251_cstr(&chunk[OBJECT_REF_ARCHIVE_BYTES..]);
        if archive_name.is_empty() || resource_name.is_empty() {
            continue;
        }
        refs.push(ObjectRef {
            archive_name,
            resource_name,
        });
    }
    refs
}

fn archive_has_mesh_entry(archive_path: &Path, requested_name: &str) -> Result<bool> {
    let archive = Archive::open_path(archive_path)?;
    Ok(find_mesh_entry_id(&archive, requested_name).is_some())
}

fn find_mesh_entry_id(archive: &Archive, requested_name: &str) -> Option<nres::EntryId> {
    for candidate in mesh_name_candidates(requested_name) {
        let Some(id) = archive.find(&candidate) else {
            continue;
        };
        let Some(entry) = archive.get(id) else {
            continue;
        };
        if entry.meta.kind == MESH_KIND || has_extension(&entry.meta.name, "msh") {
            return Some(id);
        }
    }
    None
}

fn mesh_name_candidates(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return out;
    }

    push_unique_string(&mut out, trimmed.to_string());
    if let Some(stem) = trimmed
        .strip_suffix(".msh")
        .or_else(|| trimmed.strip_suffix(".MSH"))
    {
        if !stem.is_empty() {
            push_unique_string(&mut out, stem.to_string());
        }
    } else {
        push_unique_string(&mut out, format!("{trimmed}.msh"));
    }

    out
}

fn push_unique_string(items: &mut Vec<String>, value: String) {
    if !items.iter().any(|item| item.eq_ignore_ascii_case(&value)) {
        items.push(value);
    }
}

fn ensure_msh_suffix(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.to_ascii_lowercase().ends_with(".msh") {
        trimmed.to_string()
    } else {
        format!("{trimmed}.msh")
    }
}

fn has_extension(name: &str, ext: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case(ext))
}

fn resolve_terrain_texture(
    game_root: &Path,
    map_folder_rel: &Path,
) -> Result<Option<LoadedTexture>> {
    let material_archive_path = game_root.join("material.lib");
    let texture_archive_path = game_root.join("textures.lib");
    if !material_archive_path.is_file() || !texture_archive_path.is_file() {
        return Ok(None);
    }

    for wear_name in ["Land1.wea", "Land2.wea"] {
        let wear_path = game_root.join(map_folder_rel).join(wear_name);
        if !wear_path.is_file() {
            continue;
        }
        let wear_payload = fs::read(&wear_path)?;
        let Some(material_name) = parse_primary_material_from_wear(&wear_payload) else {
            continue;
        };
        let Some(texture_name) =
            resolve_texture_name_from_material_archive(&material_archive_path, &material_name)?
        else {
            continue;
        };
        if let Some(texture) = load_texm_by_name(&texture_archive_path, &texture_name)? {
            return Ok(Some(texture));
        }
    }

    Ok(None)
}

fn parse_primary_material_from_wear(bytes: &[u8]) -> Option<String> {
    let text = decode_cp1251(bytes).replace('\r', "");
    let mut lines = text.lines();
    let count = lines.next()?.trim().parse::<usize>().ok()?;
    if count == 0 {
        return None;
    }

    for line in lines.take(count) {
        let mut parts = line.split_whitespace();
        let _legacy = parts.next()?;
        let name = parts.next()?;
        if !name.is_empty() {
            return Some(name.to_string());
        }
    }
    None
}

fn resolve_texture_name_from_material_archive(
    archive_path: &Path,
    material_name: &str,
) -> Result<Option<String>> {
    let archive = Archive::open_path(archive_path)?;

    let entry = if let Some(id) = archive.find(material_name) {
        archive
            .get(id)
            .filter(|entry| entry.meta.kind == MAT0_KIND)
            .or_else(|| {
                archive
                    .find("DEFAULT")
                    .and_then(|id| archive.get(id))
                    .filter(|entry| entry.meta.kind == MAT0_KIND)
            })
    } else {
        archive
            .find("DEFAULT")
            .and_then(|id| archive.get(id))
            .filter(|entry| entry.meta.kind == MAT0_KIND)
    }
    .or_else(|| archive.entries().find(|entry| entry.meta.kind == MAT0_KIND));

    let Some(entry) = entry else {
        return Ok(None);
    };

    let payload = archive.read(entry.id)?.into_owned();
    parse_primary_texture_name_from_mat0(&payload, entry.meta.attr2)
}

fn parse_primary_texture_name_from_mat0(payload: &[u8], attr2: u32) -> Result<Option<String>> {
    if payload.len() < 4 {
        return Ok(None);
    }

    let phase_count = u16::from_le_bytes([payload[0], payload[1]]) as usize;
    if phase_count == 0 {
        return Ok(None);
    }

    let mut offset = 4usize;
    if attr2 >= 2 {
        offset = offset.saturating_add(2);
    }
    if attr2 >= 3 {
        offset = offset.saturating_add(4);
    }
    if attr2 >= 4 {
        offset = offset.saturating_add(4);
    }

    for phase in 0..phase_count {
        let phase_off = offset.saturating_add(phase.saturating_mul(34));
        let Some(rec) = payload.get(phase_off..phase_off + 34) else {
            break;
        };
        let name_raw = &rec[18..34];
        let end = name_raw
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(name_raw.len());
        let name = decode_cp1251(&name_raw[..end]).trim().to_string();
        if !name.is_empty() {
            return Ok(Some(name));
        }
    }

    Ok(None)
}

fn load_texm_by_name(archive_path: &Path, texture_name: &str) -> Result<Option<LoadedTexture>> {
    let archive = Archive::open_path(archive_path)?;
    let Some(id) = archive.find(texture_name) else {
        return Ok(None);
    };
    let Some(entry) = archive.get(id) else {
        return Ok(None);
    };
    if entry.meta.kind != texm::TEXM_MAGIC {
        return Ok(None);
    }

    let payload = archive.read(id)?.into_owned();
    let parsed = texm::parse_texm(&payload)?;
    let decoded = texm::decode_mip_rgba8(&parsed, &payload, 0)?;

    Ok(Some(LoadedTexture {
        name: entry.meta.name.clone(),
        width: decoded.width,
        height: decoded.height,
        rgba8: decoded.rgba8,
    }))
}

fn split_relative_path(path: &str) -> Vec<&str> {
    path.split(['\\', '/'])
        .filter(|part| !part.is_empty())
        .collect()
}

fn pathbuf_from_rel(path: &str) -> PathBuf {
    let mut out = PathBuf::new();
    for part in split_relative_path(path) {
        out.push(part);
    }
    out
}

fn decode_cp1251_cstr(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let (decoded, _, _) = WINDOWS_1251.decode(&bytes[..end]);
    decoded.trim().to_string()
}

fn decode_cp1251(bytes: &[u8]) -> String {
    let (decoded, _, _) = WINDOWS_1251.decode(bytes);
    decoded.into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn game_root() -> Option<PathBuf> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("testdata")
            .join("Parkan - Iron Strategy");
        root.is_dir().then_some(root)
    }

    #[test]
    fn detects_game_root_from_mission_path() {
        let Some(root) = game_root() else {
            eprintln!("skipping: game root missing");
            return;
        };

        let mission = root
            .join("MISSIONS")
            .join("CAMPAIGN")
            .join("CAMPAIGN.00")
            .join("Mission.01")
            .join("data.tma");
        if !mission.is_file() {
            eprintln!("skipping missing mission sample");
            return;
        }

        let detected = detect_game_root_from_mission_path(&mission)
            .expect("failed to detect game root from mission path");
        assert_eq!(detected, root);
    }

    #[test]
    fn loads_scene_cpu_without_textures() {
        let Some(root) = game_root() else {
            eprintln!("skipping: game root missing");
            return;
        };

        let mission = root
            .join("MISSIONS")
            .join("CAMPAIGN")
            .join("CAMPAIGN.00")
            .join("Mission.01")
            .join("data.tma");
        if !mission.is_file() {
            eprintln!("skipping missing mission sample");
            return;
        }

        let scene = load_scene_with_options(
            &root,
            &mission,
            LoadOptions {
                load_model_textures: false,
                load_terrain_texture: false,
            },
        )
        .unwrap_or_else(|err| panic!("failed to load scene {}: {err}", mission.display()));

        assert!(!scene.terrain.positions.is_empty());
        assert!(!scene.terrain.faces.is_empty());
        assert!(!scene.models.is_empty());

        let instance_count = scene
            .models
            .iter()
            .map(|model| model.instances.len())
            .sum::<usize>();
        assert!(instance_count >= 10);

        let bounds = compute_scene_bounds(&scene).expect("scene bounds should exist");
        assert!(bounds.0[0] <= bounds.1[0]);
        assert!(bounds.0[1] <= bounds.1[1]);
        assert!(bounds.0[2] <= bounds.1[2]);
    }

    #[test]
    fn loads_scene_with_textures() {
        let Some(root) = game_root() else {
            eprintln!("skipping: game root missing");
            return;
        };

        let mission = root
            .join("MISSIONS")
            .join("CAMPAIGN")
            .join("CAMPAIGN.00")
            .join("Mission.01")
            .join("data.tma");
        if !mission.is_file() {
            eprintln!("skipping missing mission sample");
            return;
        }

        let scene = load_scene_with_options(&root, &mission, LoadOptions::default())
            .unwrap_or_else(|err| panic!("failed to load textured scene {}: {err}", mission.display()));

        assert!(!scene.models.is_empty());
        let textured_models = scene.models.iter().filter(|model| model.texture.is_some()).count();
        assert!(textured_models > 0, "no model textures resolved");
        assert!(scene.terrain_texture.is_some(), "terrain texture was not resolved");
    }

    #[test]
    fn resolves_objects_registry_models() {
        let Some(root) = game_root() else {
            eprintln!("skipping: game root missing");
            return;
        };

        let registry = root.join("objects.rlb");
        if !registry.is_file() {
            eprintln!("skipping missing objects.rlb");
            return;
        }

        let cases = [
            ("r_h_01", "bases.rlb", "r_h_01.msh"),
            ("s_tree_04", "static.rlb", "s_tree_0_04.msh"),
            ("fr_m_brige", "fortif.rlb", "fr_m_brige.msh"),
        ];

        for (key, archive_name, model_name) in cases {
            let proto = resolve_objects_registry_model(&root, &registry, key)
                .unwrap_or_else(|err| panic!("failed to resolve '{key}' from objects.rlb: {err}"))
                .unwrap_or_else(|| panic!("missing model resolution for '{key}'"));

            let got_archive = proto
                .archive_path
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.to_ascii_lowercase())
                .unwrap_or_default();
            assert_eq!(got_archive, archive_name.to_ascii_lowercase());
            assert!(
                proto.model_name.eq_ignore_ascii_case(model_name),
                "unexpected model for key '{key}': got '{}', expected '{}'",
                proto.model_name,
                model_name
            );
        }
    }
}
