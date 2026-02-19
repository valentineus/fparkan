use encoding_rs::WINDOWS_1251;
use std::fmt;
use std::fs;
use std::path::Path;

const OBJECT_RECORD_FLAGS: u32 = 0x8000_0002;
const FOOTER_MAGIC: &[u8; 4] = b"MtPr";
const MAP_PATH_TOKEN: &[u8; 10] = b"DATA\\MAPS\\";

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    FooterNotFound,
    FooterCorrupt(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "{err}"),
            Self::FooterNotFound => write!(f, "footer magic 'MtPr' not found"),
            Self::FooterCorrupt(reason) => write!(f, "corrupt mission footer: {reason}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Clone, Debug)]
pub struct MissionFile {
    pub footer: MissionFooter,
    pub objects: Vec<MissionObject>,
}

#[derive(Clone, Debug)]
pub struct MissionFooter {
    pub map_path: String,
    pub title: String,
    pub version: u32,
}

#[derive(Clone, Debug)]
pub struct MissionObject {
    pub offset: usize,
    pub group_id: u32,
    pub flags: u32,
    pub resource_name: String,
    pub logical_id: i32,
    pub clan_id: i32,
    pub position: [f32; 3],
    pub orientation: [f32; 3],
    pub scale: [f32; 3],
    pub alias: String,
}

pub fn parse_path(path: impl AsRef<Path>) -> Result<MissionFile> {
    let bytes = fs::read(path.as_ref())?;
    parse_bytes(&bytes)
}

pub fn parse_bytes(bytes: &[u8]) -> Result<MissionFile> {
    let footer = parse_footer(bytes)?;
    let objects = parse_objects(bytes);
    Ok(MissionFile { footer, objects })
}

fn parse_footer(bytes: &[u8]) -> Result<MissionFooter> {
    let map_positions = find_all_map_path_positions(bytes);
    if map_positions.is_empty() {
        return Err(Error::FooterNotFound);
    }

    for map_start in map_positions.into_iter().rev() {
        if map_start < 4 {
            continue;
        }

        let map_end = scan_path_end(bytes, map_start);
        if map_end <= map_start {
            continue;
        }
        let map_len = map_end - map_start;
        let Some(declared_map_len) = read_u32(bytes, map_start - 4).map(|v| v as usize) else {
            continue;
        };
        if declared_map_len != map_len {
            continue;
        }

        let Some(zero_pad) = read_u32(bytes, map_end) else {
            continue;
        };
        if zero_pad != 0 {
            continue;
        }

        let title_len_off = map_end + 4;
        let Some(title_len) = read_u32(bytes, title_len_off).map(|v| v as usize) else {
            continue;
        };
        if title_len == 0 || title_len > 256 {
            continue;
        }
        let title_start = title_len_off + 4;
        let Some(title_end) = title_start.checked_add(title_len) else {
            continue;
        };
        if title_end > bytes.len() {
            continue;
        }

        let map_path = decode_cp1251(&bytes[map_start..map_end]);
        if !map_path.to_ascii_uppercase().contains("DATA\\MAPS\\") {
            continue;
        }
        let title = decode_title(&bytes[title_start..title_end]);
        let version = parse_footer_version(bytes, title_end)?;

        return Ok(MissionFooter {
            map_path,
            title,
            version,
        });
    }

    // Fallback for multiplayer/legacy variants where the footer tail differs,
    // but map path is still present in clear text near EOF.
    let Some(map_start) = bytes
        .windows(MAP_PATH_TOKEN.len())
        .rposition(|window| window == MAP_PATH_TOKEN)
    else {
        return Err(Error::FooterCorrupt("failed to decode map/title envelope"));
    };
    let map_end = scan_path_end(bytes, map_start);
    if map_end <= map_start {
        return Err(Error::FooterCorrupt("failed to decode map/title envelope"));
    }
    let map_path = decode_cp1251(&bytes[map_start..map_end]);
    if !map_path.to_ascii_uppercase().contains("DATA\\MAPS\\") {
        return Err(Error::FooterCorrupt("failed to decode map/title envelope"));
    }

    let mut title = String::new();
    if let Some(title_len) = read_u32(bytes, map_end + 8).map(|v| v as usize) {
        let title_start = map_end + 12;
        let title_end = title_start.saturating_add(title_len);
        if title_len > 0 && title_len <= 256 && title_end <= bytes.len() {
            let raw = &bytes[title_start..title_end];
            if raw.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
                title = decode_title(raw);
            }
        }
    }

    let version = if let Some(magic_off) = bytes
        .windows(FOOTER_MAGIC.len())
        .rposition(|window| window == FOOTER_MAGIC)
    {
        read_u32(bytes, magic_off + 4).unwrap_or(1)
    } else {
        read_u32(bytes, map_end).unwrap_or(1)
    };

    Ok(MissionFooter {
        map_path,
        title,
        version,
    })
}

fn parse_footer_version(bytes: &[u8], after_title_off: usize) -> Result<u32> {
    if after_title_off + 8 <= bytes.len()
        && &bytes[after_title_off..after_title_off + 4] == FOOTER_MAGIC
    {
        let version = read_u32(bytes, after_title_off + 4)
            .ok_or(Error::FooterCorrupt("missing version after MtPr"))?;
        return Ok(version);
    }

    let version = read_u32(bytes, after_title_off)
        .ok_or(Error::FooterCorrupt("missing version after title"))?;
    Ok(version)
}

fn find_all_map_path_positions(bytes: &[u8]) -> Vec<usize> {
    bytes
        .windows(MAP_PATH_TOKEN.len())
        .enumerate()
        .filter_map(|(idx, window)| (window == MAP_PATH_TOKEN).then_some(idx))
        .collect()
}

fn scan_path_end(bytes: &[u8], start: usize) -> usize {
    let mut off = start;
    while off < bytes.len() && is_path_byte(bytes[off]) {
        off += 1;
    }
    off
}

fn is_path_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'/' | b'\\' | b'-' | b' ' | b':')
}

fn parse_objects(bytes: &[u8]) -> Vec<MissionObject> {
    let mut objects = Vec::new();
    let min_record_tail = 48usize;

    for offset in 0..bytes.len().saturating_sub(16) {
        let Some(flags) = read_u32(bytes, offset + 4) else {
            continue;
        };
        if flags != OBJECT_RECORD_FLAGS {
            continue;
        }

        let Some(name_len) = read_u32(bytes, offset + 8).map(|v| v as usize) else {
            continue;
        };
        if !(3..=260).contains(&name_len) {
            continue;
        }

        let name_start = offset + 12;
        let Some(name_end) = name_start.checked_add(name_len) else {
            continue;
        };
        if name_end + min_record_tail > bytes.len() {
            continue;
        }

        let name_raw = &bytes[name_start..name_end];
        if !is_object_name_bytes(name_raw) {
            continue;
        }

        let resource_name = decode_cp1251(name_raw);
        if !looks_like_object_name(&resource_name) {
            continue;
        }

        let Some(group_id) = read_u32(bytes, offset) else {
            continue;
        };
        let Some(logical_id) = read_i32(bytes, name_end) else {
            continue;
        };
        let Some(clan_id) = read_i32(bytes, name_end + 4) else {
            continue;
        };
        let Some(position) = read_vec3(bytes, name_end + 8) else {
            continue;
        };
        let Some(orientation) = read_vec3(bytes, name_end + 20) else {
            continue;
        };
        let Some(scale) = read_vec3(bytes, name_end + 32) else {
            continue;
        };
        if !all_finite(&position) || !all_finite(&orientation) || !all_finite(&scale) {
            continue;
        }

        let alias = parse_alias(bytes, name_end + 44);

        objects.push(MissionObject {
            offset,
            group_id,
            flags,
            resource_name,
            logical_id,
            clan_id,
            position,
            orientation,
            scale,
            alias,
        });
    }

    objects.sort_by_key(|obj| obj.offset);
    objects.dedup_by_key(|obj| obj.offset);
    objects
}

fn parse_alias(bytes: &[u8], alias_len_off: usize) -> String {
    let Some(alias_len) = read_u32(bytes, alias_len_off).map(|v| v as usize) else {
        return String::new();
    };
    if alias_len == 0 || alias_len > 96 {
        return String::new();
    }
    let alias_start = alias_len_off + 4;
    let Some(alias_end) = alias_start.checked_add(alias_len) else {
        return String::new();
    };
    if alias_end > bytes.len() {
        return String::new();
    }
    let alias_raw = &bytes[alias_start..alias_end];
    if !alias_raw
        .iter()
        .all(|&b| b == b'_' || b == b'-' || b == b'.' || b.is_ascii_alphanumeric())
    {
        return String::new();
    }
    decode_cp1251(alias_raw)
}

fn looks_like_object_name(name: &str) -> bool {
    if name.ends_with(".dat") {
        return true;
    }
    name.contains('_')
}

fn is_object_name_bytes(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || matches!(*b, b'_' | b'.' | b'/' | b'\\' | b'-'))
}

fn all_finite(v: &[f32; 3]) -> bool {
    v.iter().all(|c| c.is_finite())
}

fn decode_cp1251(bytes: &[u8]) -> String {
    let (decoded, _, _) = WINDOWS_1251.decode(bytes);
    decoded.into_owned()
}

fn decode_title(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .rposition(|b| *b != 0 && *b != 0xCD)
        .map(|idx| idx + 1)
        .unwrap_or(0);
    decode_cp1251(&bytes[..end]).trim().to_string()
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    let chunk = bytes.get(offset..end)?;
    Some(u32::from_le_bytes(chunk.try_into().ok()?))
}

fn read_i32(bytes: &[u8], offset: usize) -> Option<i32> {
    read_u32(bytes, offset).map(|v| v as i32)
}

fn read_f32(bytes: &[u8], offset: usize) -> Option<f32> {
    let end = offset.checked_add(4)?;
    let chunk = bytes.get(offset..end)?;
    Some(f32::from_le_bytes(chunk.try_into().ok()?))
}

fn read_vec3(bytes: &[u8], offset: usize) -> Option<[f32; 3]> {
    Some([
        read_f32(bytes, offset)?,
        read_f32(bytes, offset + 4)?,
        read_f32(bytes, offset + 8)?,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::collect_files_recursive;
    use std::path::{Path, PathBuf};

    fn game_root() -> Option<PathBuf> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("testdata")
            .join("Parkan - Iron Strategy");
        root.is_dir().then_some(root)
    }

    #[test]
    fn parses_known_mission_footer_and_objects() {
        let Some(root) = game_root() else {
            eprintln!("skipping: game root is missing");
            return;
        };

        let path = root
            .join("MISSIONS")
            .join("CAMPAIGN")
            .join("CAMPAIGN.00")
            .join("Mission.01")
            .join("data.tma");
        if !path.is_file() {
            eprintln!("skipping: sample mission is missing ({})", path.display());
            return;
        }

        let mission = parse_path(&path).expect("parse mission failed");
        assert_eq!(mission.footer.version, 1);
        assert!(
            mission
                .footer
                .map_path
                .eq_ignore_ascii_case("DATA\\MAPS\\Tut_1\\land"),
            "unexpected map path: {}",
            mission.footer.map_path
        );
        assert!(mission.objects.len() >= 20);
        assert!(mission
            .objects
            .iter()
            .any(|obj| obj.resource_name.eq_ignore_ascii_case("s_tree_04")));
        assert!(mission.objects.iter().any(|obj| {
            obj.resource_name
                .eq_ignore_ascii_case("UNITS\\UNITS\\HERO\\tut1_p.dat")
        }));
    }

    #[test]
    fn parses_all_retail_missions() {
        let Some(root) = game_root() else {
            eprintln!("skipping: game root is missing");
            return;
        };

        let mission_root = root.join("MISSIONS");
        let mut files = Vec::new();
        collect_files_recursive(&mission_root, &mut files);
        files.sort();

        let mut mission_count = 0usize;
        for path in files {
            if !path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.eq_ignore_ascii_case("data.tma"))
            {
                continue;
            }

            mission_count += 1;
            let mission = parse_path(&path)
                .unwrap_or_else(|err| panic!("failed to parse {}: {err}", path.display()));
            assert!(
                mission
                    .footer
                    .map_path
                    .to_ascii_uppercase()
                    .contains("DATA\\MAPS\\"),
                "{}: invalid map path '{}'",
                path.display(),
                mission.footer.map_path
            );
            assert!(
                !mission.objects.is_empty(),
                "{}: mission has no parsed object records",
                path.display()
            );
            assert!(
                mission
                    .objects
                    .iter()
                    .all(|obj| obj.position.iter().all(|v| v.is_finite())),
                "{}: mission has non-finite position",
                path.display()
            );
        }

        assert!(mission_count > 0, "no data.tma files found");
    }
}
