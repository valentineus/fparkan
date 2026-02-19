use encoding_rs::WINDOWS_1251;
use std::fmt;
use std::fs;
use std::path::Path;

const MIN_SIZE: usize = 0x48;
const MAGIC: u32 = 0x0000_F0F1;

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    TooSmall { got: usize },
    InvalidMagic { got: u32 },
    MissingArchiveName,
    MissingModelKey,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "{err}"),
            Self::TooSmall { got } => write!(f, "unit .dat is too small: {got} bytes"),
            Self::InvalidMagic { got } => write!(f, "invalid .dat magic: 0x{got:08X}"),
            Self::MissingArchiveName => write!(f, "unit .dat has empty archive name"),
            Self::MissingModelKey => write!(f, "unit .dat has empty model key"),
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
pub struct UnitDat {
    pub magic: u32,
    pub flags: u32,
    pub archive_name: String,
    pub model_key: String,
}

pub fn parse_path(path: impl AsRef<Path>) -> Result<UnitDat> {
    let bytes = fs::read(path.as_ref())?;
    parse_bytes(&bytes)
}

pub fn parse_bytes(bytes: &[u8]) -> Result<UnitDat> {
    if bytes.len() < MIN_SIZE {
        return Err(Error::TooSmall { got: bytes.len() });
    }

    let magic = read_u32(bytes, 0).ok_or(Error::TooSmall { got: bytes.len() })?;
    if magic != MAGIC {
        return Err(Error::InvalidMagic { got: magic });
    }

    let flags = read_u32(bytes, 4).ok_or(Error::TooSmall { got: bytes.len() })?;
    let archive_name = decode_c_string_fixed(&bytes[0x08..0x28]);
    if archive_name.is_empty() {
        return Err(Error::MissingArchiveName);
    }

    let model_key = decode_c_string_fixed(&bytes[0x28..0x48]);
    if model_key.is_empty() {
        return Err(Error::MissingModelKey);
    }

    Ok(UnitDat {
        magic,
        flags,
        archive_name,
        model_key,
    })
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    let chunk = bytes.get(offset..end)?;
    Some(u32::from_le_bytes(chunk.try_into().ok()?))
}

fn decode_c_string_fixed(bytes: &[u8]) -> String {
    let used = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let (decoded, _, _) = WINDOWS_1251.decode(&bytes[..used]);
    decoded.trim().to_string()
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
    fn parses_known_dat_files() {
        let Some(root) = game_root() else {
            eprintln!("skipping: game root missing");
            return;
        };

        let samples = [
            root.join("UNITS/UNITS/HERO/tut1_p.dat"),
            root.join("UNITS/UNITS/BATTLE/l_targ.dat"),
            root.join("UNITS/BUILDS/BRIDGE/m_bridge.dat"),
        ];

        for path in samples {
            if !path.is_file() {
                eprintln!("skipping missing sample {}", path.display());
                continue;
            }
            let dat = parse_path(&path)
                .unwrap_or_else(|err| panic!("failed to parse {}: {err}", path.display()));
            assert_eq!(dat.magic, MAGIC);
            assert!(dat.archive_name.to_ascii_lowercase().ends_with(".rlb"));
            assert!(dat.model_key.contains('_'));
        }
    }

    #[test]
    fn parses_retail_dat_corpus() {
        let Some(root) = game_root() else {
            eprintln!("skipping: game root missing");
            return;
        };

        let units_root = root.join("UNITS");
        let mut files = Vec::new();
        collect_files_recursive(&units_root, &mut files);
        files.sort();

        let mut parsed = 0usize;
        for path in files {
            if !path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("dat"))
            {
                continue;
            }
            let dat = parse_path(&path)
                .unwrap_or_else(|err| panic!("failed to parse {}: {err}", path.display()));
            assert!(
                !dat.archive_name.is_empty(),
                "{} empty archive",
                path.display()
            );
            assert!(
                !dat.model_key.is_empty(),
                "{} empty model key",
                path.display()
            );
            parsed += 1;
        }

        assert!(parsed > 0, "no .dat files parsed");
    }
}
