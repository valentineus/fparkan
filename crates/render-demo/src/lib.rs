use encoding_rs::WINDOWS_1251;
use msh_core::{parse_model_payload, Model};
use nres::{Archive, EntryRef};
use std::fmt;
use std::path::{Path, PathBuf};
use texm::{decode_mip_rgba8, parse_texm};

const WEAR_KIND: u32 = 0x5241_4557;
const MAT0_KIND: u32 = 0x3054_414D;

#[derive(Debug)]
pub enum Error {
    Nres(nres::error::Error),
    Msh(msh_core::error::Error),
    Texm(texm::error::Error),
    Io(std::io::Error),
    NoMshEntries,
    ModelNotFound(String),
    NoTexmEntries,
    TextureNotFound(String),
    MaterialNotFound(String),
    WearNotFound(String),
    InvalidWear(String),
    InvalidMaterial(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Nres(err) => write!(f, "{err}"),
            Self::Msh(err) => write!(f, "{err}"),
            Self::Texm(err) => write!(f, "{err}"),
            Self::Io(err) => write!(f, "{err}"),
            Self::NoMshEntries => write!(f, "archive does not contain .msh entries"),
            Self::ModelNotFound(name) => write!(f, "model not found: {name}"),
            Self::NoTexmEntries => write!(f, "archive does not contain Texm entries"),
            Self::TextureNotFound(name) => write!(f, "texture not found: {name}"),
            Self::MaterialNotFound(name) => write!(f, "material not found: {name}"),
            Self::WearNotFound(name) => write!(f, "wear entry not found: {name}"),
            Self::InvalidWear(reason) => write!(f, "invalid WEAR payload: {reason}"),
            Self::InvalidMaterial(reason) => write!(f, "invalid MAT0 payload: {reason}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Nres(err) => Some(err),
            Self::Msh(err) => Some(err),
            Self::Texm(err) => Some(err),
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
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

impl From<texm::error::Error> for Error {
    fn from(value: texm::error::Error) -> Self {
        Self::Texm(value)
    }
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Clone, Debug)]
pub struct LoadedModel {
    pub name: String,
    pub model: Model,
}

#[derive(Clone, Debug)]
pub struct LoadedTexture {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub rgba8: Vec<u8>,
}

pub fn load_model_with_name_from_archive(
    path: &Path,
    model_name: Option<&str>,
) -> Result<LoadedModel> {
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

    let target_name = archive
        .get(target_id)
        .map(|entry| entry.meta.name.clone())
        .unwrap_or_else(|| String::from("<unknown>"));
    let payload = archive.read(target_id)?;
    Ok(LoadedModel {
        name: target_name,
        model: parse_model_payload(payload.as_slice())?,
    })
}

pub fn load_model_from_archive(path: &Path, model_name: Option<&str>) -> Result<Model> {
    Ok(load_model_with_name_from_archive(path, model_name)?.model)
}

pub fn load_texture_from_archive(path: &Path, texture_name: Option<&str>) -> Result<LoadedTexture> {
    let archive = Archive::open_path(path)?;
    if let Some(name) = texture_name {
        return load_texture_from_archive_by_name(&archive, name);
    }

    let mut texm_entries = archive
        .entries()
        .filter(|entry| entry.meta.kind == texm::TEXM_MAGIC)
        .collect::<Vec<_>>();
    if texm_entries.is_empty() {
        return Err(Error::NoTexmEntries);
    }
    texm_entries.sort_by(|a, b| {
        a.meta
            .name
            .to_ascii_lowercase()
            .cmp(&b.meta.name.to_ascii_lowercase())
    });
    let first = texm_entries[0];
    decode_texture_entry(&archive, first)
}

pub fn resolve_texture_for_model(
    model_archive_path: &Path,
    model_entry_name: &str,
    texture_name_override: Option<&str>,
    textures_archive_override: Option<&Path>,
    material_archive_override: Option<&Path>,
    wear_entry_override: Option<&str>,
) -> Result<Option<LoadedTexture>> {
    if let Some(name) = texture_name_override {
        return load_texture_by_name_from_candidate_archives(
            name,
            candidate_texture_archives(model_archive_path, textures_archive_override),
        )
        .map(Some);
    }

    let wear_entry_name = if let Some(name) = wear_entry_override {
        name.to_string()
    } else {
        derive_wear_entry_name(model_entry_name).ok_or_else(|| {
            Error::WearNotFound(format!(
                "cannot derive WEAR name from model '{model_entry_name}'"
            ))
        })?
    };

    let model_archive = Archive::open_path(model_archive_path)?;
    let wear_materials = parse_wear_material_names(
        read_entry_by_name_kind(&model_archive, &wear_entry_name, WEAR_KIND)?
            .0
            .as_slice(),
    )?;
    let Some(primary_material) = wear_materials.first() else {
        return Ok(None);
    };

    let material_path = if let Some(path) = material_archive_override {
        path.to_path_buf()
    } else {
        sibling_archive_path(model_archive_path, "material.lib")
            .ok_or_else(|| Error::MaterialNotFound(String::from("material.lib")))?
    };
    let material_archive = Archive::open_path(&material_path)?;
    let material_entry = find_material_entry_with_fallback(&material_archive, primary_material)?;
    let material_payload = material_archive.read(material_entry.id)?.into_owned();
    let texture_name =
        parse_primary_texture_name_from_mat0(&material_payload, material_entry.meta.attr2)?;
    let Some(texture_name) = texture_name else {
        return Ok(None);
    };

    let texture = load_texture_by_name_from_candidate_archives(
        &texture_name,
        candidate_texture_archives(model_archive_path, textures_archive_override),
    )?;
    Ok(Some(texture))
}

fn load_texture_by_name_from_candidate_archives(
    texture_name: &str,
    archives: Vec<PathBuf>,
) -> Result<LoadedTexture> {
    let mut last_not_found = None;
    for archive_path in archives {
        if !archive_path.is_file() {
            continue;
        }
        let archive = Archive::open_path(&archive_path)?;
        match load_texture_from_archive_by_name(&archive, texture_name) {
            Ok(texture) => return Ok(texture),
            Err(Error::TextureNotFound(name)) => {
                last_not_found = Some(name);
            }
            Err(other) => return Err(other),
        }
    }

    Err(Error::TextureNotFound(
        last_not_found.unwrap_or_else(|| texture_name.to_string()),
    ))
}

fn candidate_texture_archives(
    model_archive_path: &Path,
    textures_archive_override: Option<&Path>,
) -> Vec<PathBuf> {
    if let Some(path) = textures_archive_override {
        return vec![path.to_path_buf()];
    }

    let mut out = Vec::new();
    if let Some(path) = sibling_archive_path(model_archive_path, "textures.lib") {
        out.push(path);
    }
    if let Some(path) = sibling_archive_path(model_archive_path, "lightmap.lib") {
        out.push(path);
    }
    out
}

fn sibling_archive_path(model_archive_path: &Path, name: &str) -> Option<PathBuf> {
    let parent = model_archive_path.parent()?;
    Some(parent.join(name))
}

fn derive_wear_entry_name(model_entry_name: &str) -> Option<String> {
    let stem = model_entry_name.rsplit_once('.').map(|(left, _)| left)?;
    Some(format!("{stem}.wea"))
}

fn read_entry_by_name_kind(
    archive: &Archive,
    name: &str,
    expected_kind: u32,
) -> Result<(Vec<u8>, String)> {
    let Some(id) = archive.find(name) else {
        return Err(Error::WearNotFound(name.to_string()));
    };
    let Some(entry) = archive.get(id) else {
        return Err(Error::WearNotFound(name.to_string()));
    };
    if entry.meta.kind != expected_kind {
        return Err(Error::WearNotFound(name.to_string()));
    }
    let payload = archive.read(id)?.into_owned();
    Ok((payload, entry.meta.name.clone()))
}

fn find_material_entry_with_fallback<'a>(
    archive: &'a Archive,
    requested_name: &str,
) -> Result<EntryRef<'a>> {
    if let Some(id) = archive.find(requested_name) {
        if let Some(entry) = archive.get(id) {
            if entry.meta.kind == MAT0_KIND {
                return Ok(entry);
            }
        }
    }

    if let Some(id) = archive.find("DEFAULT") {
        if let Some(entry) = archive.get(id) {
            if entry.meta.kind == MAT0_KIND {
                return Ok(entry);
            }
        }
    }

    let Some(entry) = archive.entries().find(|entry| entry.meta.kind == MAT0_KIND) else {
        return Err(Error::MaterialNotFound(requested_name.to_string()));
    };
    Ok(entry)
}

fn parse_wear_material_names(payload: &[u8]) -> Result<Vec<String>> {
    let text = decode_cp1251(payload).replace('\r', "");
    let mut lines = text.lines();
    let Some(first) = lines.next() else {
        return Err(Error::InvalidWear(String::from("WEAR payload is empty")));
    };
    let count = first
        .trim()
        .parse::<usize>()
        .map_err(|_| Error::InvalidWear(format!("invalid wearCount line: '{first}'")))?;
    if count == 0 {
        return Err(Error::InvalidWear(String::from("wearCount must be > 0")));
    }

    let mut materials = Vec::with_capacity(count);
    for idx in 0..count {
        let Some(line) = lines.next() else {
            return Err(Error::InvalidWear(format!(
                "missing material line {idx} of {count}"
            )));
        };
        let mut parts = line.split_whitespace();
        let _legacy = parts
            .next()
            .ok_or_else(|| Error::InvalidWear(format!("invalid material line {idx}: '{line}'")))?;
        let name = parts
            .next()
            .ok_or_else(|| Error::InvalidWear(format!("invalid material line {idx}: '{line}'")))?;
        materials.push(name.to_string());
    }

    Ok(materials)
}

fn parse_primary_texture_name_from_mat0(payload: &[u8], attr2: u32) -> Result<Option<String>> {
    if payload.len() < 4 {
        return Err(Error::InvalidMaterial(String::from(
            "MAT0 payload is too small for header",
        )));
    }
    let phase_count = u16::from_le_bytes([payload[0], payload[1]]) as usize;
    if phase_count == 0 {
        return Ok(None);
    }

    let mut offset = 4usize;
    if attr2 >= 2 {
        offset = offset
            .checked_add(2)
            .ok_or_else(|| Error::InvalidMaterial(String::from("MAT0 offset overflow")))?;
    }
    if attr2 >= 3 {
        offset = offset
            .checked_add(4)
            .ok_or_else(|| Error::InvalidMaterial(String::from("MAT0 offset overflow")))?;
    }
    if attr2 >= 4 {
        offset = offset
            .checked_add(4)
            .ok_or_else(|| Error::InvalidMaterial(String::from("MAT0 offset overflow")))?;
    }

    for phase in 0..phase_count {
        let phase_off = offset
            .checked_add(phase.checked_mul(34).ok_or_else(|| {
                Error::InvalidMaterial(String::from("MAT0 phase offset overflow"))
            })?)
            .ok_or_else(|| Error::InvalidMaterial(String::from("MAT0 phase offset overflow")))?;
        let phase_end = phase_off
            .checked_add(34)
            .ok_or_else(|| Error::InvalidMaterial(String::from("MAT0 phase offset overflow")))?;
        let Some(rec) = payload.get(phase_off..phase_end) else {
            return Err(Error::InvalidMaterial(format!(
                "MAT0 phase {phase} is out of bounds"
            )));
        };
        let name_raw = &rec[18..34];
        let name_end = name_raw
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(name_raw.len());
        let name = decode_cp1251(&name_raw[..name_end]).trim().to_string();
        if !name.is_empty() {
            return Ok(Some(name));
        }
    }

    Ok(None)
}

fn decode_cp1251(bytes: &[u8]) -> String {
    let (decoded, _, _) = WINDOWS_1251.decode(bytes);
    decoded.into_owned()
}

fn load_texture_from_archive_by_name(archive: &Archive, name: &str) -> Result<LoadedTexture> {
    let Some(id) = archive.find(name) else {
        return Err(Error::TextureNotFound(name.to_string()));
    };
    let Some(entry) = archive.get(id) else {
        return Err(Error::TextureNotFound(name.to_string()));
    };
    if entry.meta.kind != texm::TEXM_MAGIC {
        return Err(Error::TextureNotFound(name.to_string()));
    }
    decode_texture_entry(archive, entry)
}

fn decode_texture_entry(archive: &Archive, entry: EntryRef<'_>) -> Result<LoadedTexture> {
    let payload = archive.read(entry.id)?.into_owned();
    let parsed = parse_texm(&payload)?;
    let decoded = decode_mip_rgba8(&parsed, &payload, 0)?;
    Ok(LoadedTexture {
        name: entry.meta.name.clone(),
        width: decoded.width,
        height: decoded.height,
        rgba8: decoded.rgba8,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::collect_files_recursive;
    use std::fs;
    use std::path::{Path, PathBuf};

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

    fn game_root() -> Option<PathBuf> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("testdata")
            .join("Parkan - Iron Strategy");
        if path.is_dir() {
            Some(path)
        } else {
            None
        }
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

    #[test]
    fn resolve_texture_for_real_model_via_wear_and_material() {
        let Some(root) = game_root() else {
            eprintln!(
                "skipping resolve_texture_for_real_model_via_wear_and_material: no game root"
            );
            return;
        };
        let archive = root.join("animals.rlb");
        if !archive.is_file() {
            eprintln!("skipping resolve_texture_for_real_model_via_wear_and_material: missing animals.rlb");
            return;
        }

        let loaded = load_model_with_name_from_archive(&archive, Some("A_L_01.msh"))
            .unwrap_or_else(|err| {
                panic!(
                    "failed to load model A_L_01.msh from {}: {err:?}",
                    archive.display()
                )
            });
        let texture = resolve_texture_for_model(&archive, &loaded.name, None, None, None, None)
            .unwrap_or_else(|err| panic!("failed to resolve texture for {}: {err:?}", loaded.name))
            .expect("texture must be resolved for A_L_01.msh");
        assert!(texture.width > 0 && texture.height > 0);
        assert_eq!(
            texture.rgba8.len(),
            usize::try_from(texture.width)
                .ok()
                .and_then(|w| usize::try_from(texture.height).ok().map(|h| w * h * 4))
                .unwrap_or(0)
        );
    }

    #[test]
    fn load_first_texture_from_real_archive() {
        let Some(root) = game_root() else {
            eprintln!("skipping load_first_texture_from_real_archive: no game root");
            return;
        };
        let archive = root.join("textures.lib");
        if !archive.is_file() {
            eprintln!("skipping load_first_texture_from_real_archive: missing textures.lib");
            return;
        }
        let texture = load_texture_from_archive(&archive, None).unwrap_or_else(|err| {
            panic!(
                "failed to load first texture from {}: {err:?}",
                archive.display()
            )
        });
        assert!(texture.width > 0 && texture.height > 0);
        assert!(!texture.rgba8.is_empty());
    }

    #[test]
    fn parse_wear_material_names_parses_counted_lines() {
        let payload = b"2\r\n0 MAT_A\r\n1 MAT_B\r\n";
        let materials =
            parse_wear_material_names(payload).expect("failed to parse valid WEAR payload");
        assert_eq!(materials, vec!["MAT_A".to_string(), "MAT_B".to_string()]);
    }

    #[test]
    fn parse_wear_material_names_rejects_invalid_payload() {
        let payload = b"2\n0 ONLY_ONE\n";
        assert!(matches!(
            parse_wear_material_names(payload),
            Err(Error::InvalidWear(_))
        ));
    }

    #[test]
    fn parse_primary_texture_name_from_mat0_respects_attr2_layout() {
        let mut payload = vec![0u8; 4 + 10 + 34];
        payload[0..2].copy_from_slice(&1u16.to_le_bytes()); // phase_count
                                                            // attr2=4 adds 10 bytes before phase table
        let name = b"TEX_MAIN";
        payload[4 + 10 + 18..4 + 10 + 18 + name.len()].copy_from_slice(name);

        let parsed = parse_primary_texture_name_from_mat0(&payload, 4)
            .expect("failed to parse MAT0 payload with attr2=4");
        assert_eq!(parsed, Some("TEX_MAIN".to_string()));
    }

    #[test]
    fn parse_primary_texture_name_from_mat0_decodes_cp1251_bytes() {
        let mut payload = vec![0u8; 4 + 34];
        payload[0..2].copy_from_slice(&1u16.to_le_bytes()); // phase_count
        payload[4 + 18] = 0xC0; // 'А' in CP1251

        let parsed =
            parse_primary_texture_name_from_mat0(&payload, 0).expect("failed to parse MAT0");
        assert_eq!(parsed, Some("А".to_string()));
    }
}
