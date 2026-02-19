use nres::Archive;
use std::fmt;
use std::path::Path;

pub const TERRAIN_UV_SCALE: f32 = 1024.0;

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Nres(nres::error::Error),
    MissingChunk(&'static str),
    InvalidChunkSize {
        label: &'static str,
        size: usize,
        stride: usize,
    },
    VertexCountOverflow {
        count: usize,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Nres(err) => write!(f, "{err}"),
            Self::MissingChunk(label) => write!(f, "missing required terrain chunk: {label}"),
            Self::InvalidChunkSize {
                label,
                size,
                stride,
            } => write!(
                f,
                "invalid chunk size for {label}: {size} (must be divisible by {stride})"
            ),
            Self::VertexCountOverflow { count } => {
                write!(f, "terrain vertex count {count} exceeds u16 range")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Nres(err) => Some(err),
            _ => None,
        }
    }
}

impl From<nres::error::Error> for Error {
    fn from(value: nres::error::Error) -> Self {
        Self::Nres(value)
    }
}

#[derive(Clone, Debug)]
pub struct TerrainMesh {
    pub positions: Vec<[f32; 3]>,
    pub uv0: Vec<[f32; 2]>,
    pub faces: Vec<TerrainFace>,
}

#[derive(Copy, Clone, Debug)]
pub struct TerrainFace {
    pub indices: [u16; 3],
    pub flags: u32,
    pub material_tag: u16,
    pub aux_tag: u16,
}

#[derive(Clone, Debug)]
pub struct TerrainRenderMesh {
    pub vertices: Vec<TerrainRenderVertex>,
    pub indices: Vec<u16>,
    pub face_count_raw: usize,
    pub face_count_kept: usize,
    pub face_count_dropped_invalid: usize,
}

#[derive(Copy, Clone, Debug)]
pub struct TerrainRenderVertex {
    pub position: [f32; 3],
    pub uv0: [f32; 2],
}

pub fn load_land_mesh(path: impl AsRef<Path>) -> Result<TerrainMesh> {
    let archive = Archive::open_path(path.as_ref())?;

    let positions_entry = archive
        .entries()
        .find(|entry| entry.meta.kind == 3)
        .ok_or(Error::MissingChunk("type=3 (positions)"))?;
    let uv_entry = archive.entries().find(|entry| entry.meta.kind == 5);
    let faces_entry = archive
        .entries()
        .find(|entry| entry.meta.kind == 21)
        .ok_or(Error::MissingChunk("type=21 (faces)"))?;

    let positions_payload = archive.read(positions_entry.id)?.into_owned();
    if positions_payload.len() % 12 != 0 {
        return Err(Error::InvalidChunkSize {
            label: "type=3 (positions)",
            size: positions_payload.len(),
            stride: 12,
        });
    }

    let mut positions = Vec::with_capacity(positions_payload.len() / 12);
    for chunk in positions_payload.chunks_exact(12) {
        let x = f32::from_le_bytes(chunk[0..4].try_into().unwrap_or([0; 4]));
        let y = f32::from_le_bytes(chunk[4..8].try_into().unwrap_or([0; 4]));
        let z = f32::from_le_bytes(chunk[8..12].try_into().unwrap_or([0; 4]));
        positions.push([x, y, z]);
    }

    let mut uv0 = vec![[0.0f32, 0.0f32]; positions.len()];
    if let Some(uv_entry) = uv_entry {
        let uv_payload = archive.read(uv_entry.id)?.into_owned();
        if uv_payload.len() % 4 != 0 {
            return Err(Error::InvalidChunkSize {
                label: "type=5 (uv)",
                size: uv_payload.len(),
                stride: 4,
            });
        }
        let uv_count = uv_payload.len() / 4;
        for idx in 0..uv_count.min(uv0.len()) {
            let off = idx * 4;
            let u = i16::from_le_bytes([uv_payload[off], uv_payload[off + 1]]) as f32;
            let v = i16::from_le_bytes([uv_payload[off + 2], uv_payload[off + 3]]) as f32;
            uv0[idx] = [u / TERRAIN_UV_SCALE, v / TERRAIN_UV_SCALE];
        }
    }

    let face_payload = archive.read(faces_entry.id)?.into_owned();
    if face_payload.len() % 28 != 0 {
        return Err(Error::InvalidChunkSize {
            label: "type=21 (faces)",
            size: face_payload.len(),
            stride: 28,
        });
    }

    let mut faces = Vec::with_capacity(face_payload.len() / 28);
    for chunk in face_payload.chunks_exact(28) {
        let flags = u32::from_le_bytes(chunk[0..4].try_into().unwrap_or([0; 4]));
        let material_tag = u16::from_le_bytes(chunk[4..6].try_into().unwrap_or([0; 2]));
        let aux_tag = u16::from_le_bytes(chunk[6..8].try_into().unwrap_or([0; 2]));
        let i0 = u16::from_le_bytes(chunk[8..10].try_into().unwrap_or([0; 2]));
        let i1 = u16::from_le_bytes(chunk[10..12].try_into().unwrap_or([0; 2]));
        let i2 = u16::from_le_bytes(chunk[12..14].try_into().unwrap_or([0; 2]));
        if usize::from(i0) >= positions.len()
            || usize::from(i1) >= positions.len()
            || usize::from(i2) >= positions.len()
        {
            continue;
        }
        faces.push(TerrainFace {
            indices: [i0, i1, i2],
            flags,
            material_tag,
            aux_tag,
        });
    }

    Ok(TerrainMesh {
        positions,
        uv0,
        faces,
    })
}

pub fn build_render_mesh(mesh: &TerrainMesh) -> Result<TerrainRenderMesh> {
    if mesh.positions.len() > usize::from(u16::MAX) + 1 {
        return Err(Error::VertexCountOverflow {
            count: mesh.positions.len(),
        });
    }

    let vertices = mesh
        .positions
        .iter()
        .enumerate()
        .map(|(idx, &position)| TerrainRenderVertex {
            position,
            uv0: mesh.uv0.get(idx).copied().unwrap_or([0.0, 0.0]),
        })
        .collect::<Vec<_>>();

    let mut indices = Vec::with_capacity(mesh.faces.len() * 3);
    for face in &mesh.faces {
        indices.extend_from_slice(&face.indices);
    }

    Ok(TerrainRenderMesh {
        vertices,
        indices,
        face_count_raw: mesh.faces.len(),
        face_count_kept: mesh.faces.len(),
        face_count_dropped_invalid: 0,
    })
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
    fn loads_known_land_mesh() {
        let Some(root) = game_root() else {
            eprintln!("skipping: game root missing");
            return;
        };

        let land = root
            .join("DATA")
            .join("MAPS")
            .join("Tut_1")
            .join("Land.msh");
        if !land.is_file() {
            eprintln!("skipping missing sample {}", land.display());
            return;
        }

        let mesh = load_land_mesh(&land)
            .unwrap_or_else(|err| panic!("failed to parse {}: {err}", land.display()));
        assert!(mesh.positions.len() > 1000);
        assert!(mesh.faces.len() > 1000);

        let render = build_render_mesh(&mesh).expect("failed to build render mesh");
        assert_eq!(render.vertices.len(), mesh.positions.len());
        assert_eq!(render.indices.len(), mesh.faces.len() * 3);
    }

    #[test]
    fn loads_all_retail_land_meshes() {
        let Some(root) = game_root() else {
            eprintln!("skipping: game root missing");
            return;
        };

        let maps_root = root.join("DATA").join("MAPS");
        let mut files = Vec::new();
        collect_files_recursive(&maps_root, &mut files);
        files.sort();

        let mut parsed = 0usize;
        for path in files {
            if !path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.eq_ignore_ascii_case("Land.msh"))
            {
                continue;
            }
            let mesh = load_land_mesh(&path)
                .unwrap_or_else(|err| panic!("failed to parse {}: {err}", path.display()));
            assert!(
                !mesh.positions.is_empty() && !mesh.faces.is_empty(),
                "{} parsed but empty",
                path.display()
            );
            parsed += 1;
        }

        assert!(parsed > 0, "no Land.msh files parsed");
    }
}
