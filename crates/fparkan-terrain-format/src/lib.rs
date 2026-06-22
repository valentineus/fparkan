#![forbid(unsafe_code)]
//! Terrain disk format primitives.

use fparkan_binary::{checked_count_bytes, Cursor, DecodeError};
use fparkan_nres::{EntryId, EntryMeta, NresDocument, NresError};

const TYPE_AREAL_MAP: u32 = 12;
const TYPE_NODES: u32 = 1;
const TYPE_SLOTS: u32 = 2;
const TYPE_POSITIONS: u32 = 3;
const TYPE_NORMALS: u32 = 4;
const TYPE_UV0: u32 = 5;
const TYPE_ACCELERATOR: u32 = 11;
const TYPE_AUX14: u32 = 14;
const TYPE_AUX18: u32 = 18;
const TYPE_FACES: u32 = 21;
const REQUIRED_TYPES: [u32; 9] = [
    TYPE_NODES,
    TYPE_SLOTS,
    TYPE_POSITIONS,
    TYPE_NORMALS,
    TYPE_UV0,
    TYPE_AUX18,
    TYPE_AUX14,
    TYPE_ACCELERATOR,
    TYPE_FACES,
];
const AREAL_PREFIX_SIZE: usize = 56;
const SLOT_HEADER_SIZE: usize = 0x8c;
const SLOT_STRIDE: usize = 68;
const GRID_HIT_COUNT_BITS: u32 = 10;
const GRID_POOL_OFFSET_MASK: u32 = (1 << 22) - 1;

/// Full surface mask.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FullSurfaceMask(pub u32);

/// Compact surface mask.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompactSurfaceMask(pub u16);

/// Material class mask.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MaterialClassMask(pub u8);

/// Terrain face with 28-byte source layout.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerrainFace28 {
    /// Full 32-bit surface mask/flags from bytes 0..4.
    pub flags: FullSurfaceMask,
    /// Opaque tag at bytes 4..6.
    pub material_tag: u16,
    /// Opaque tag at bytes 6..8.
    pub aux_tag: u16,
    /// Vertex indices at bytes 8..14.
    pub vertices: [u16; 3],
    /// Neighbor face indices at bytes 14..20.
    pub neighbors: [Option<u16>; 3],
    /// Preserved bytes 20..28.
    pub tail_raw: [u8; 8],
    /// Preserved raw bytes.
    pub raw: [u8; 28],
}

/// Terrain stream descriptor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerrainStream {
    /// Stream type id.
    pub type_id: u32,
    /// Entry attributes.
    pub attributes: TerrainStreamAttributes,
    /// Payload size.
    pub size: u32,
}

/// Opaque stream attributes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TerrainStreamAttributes {
    /// Attribute 1.
    pub attr1: u32,
    /// Attribute 2.
    pub attr2: u32,
    /// Attribute 3.
    pub attr3: u32,
}

/// Slot table metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerrainSlotTable {
    /// Raw 0x8c-byte header.
    pub header_raw: Vec<u8>,
    /// Slot records.
    pub slots_raw: Vec<[u8; SLOT_STRIDE]>,
}

/// Land mesh document.
#[derive(Clone, Debug, PartialEq)]
pub struct LandMeshDocument {
    /// Stream descriptors in archive order.
    pub streams: Vec<TerrainStream>,
    /// Raw node/slot mapping bytes.
    pub nodes_raw: Vec<u8>,
    /// Slot table.
    pub slots: TerrainSlotTable,
    /// Positions from type 3.
    pub positions: Vec<[f32; 3]>,
    /// Packed normals from type 4.
    pub normals: Vec<[i8; 4]>,
    /// Packed UV from type 5.
    pub uv0: Vec<[i16; 2]>,
    /// Type 11 accelerator words.
    pub accelerator: Vec<[u8; 4]>,
    /// Type 14 auxiliary words.
    pub aux14: Vec<[u8; 4]>,
    /// Type 18 auxiliary words.
    pub aux18: Vec<[u8; 4]>,
    /// Faces.
    pub faces: Vec<TerrainFace28>,
}

/// Decoded `Land.map` document.
#[derive(Clone, Debug, PartialEq)]
pub struct LandMapDocument {
    /// Type 12 entry attributes.
    pub entry: TerrainStream,
    /// Areal count declared by entry attribute 1.
    pub areal_count: u32,
    /// Decoded areals.
    pub areals: Vec<Areal>,
    /// Fast lookup grid.
    pub grid: ArealGrid,
}

/// Logical terrain area.
#[derive(Clone, Debug, PartialEq)]
pub struct Areal {
    /// Preserved 56-byte prefix.
    pub prefix_raw: [u8; AREAL_PREFIX_SIZE],
    /// Anchor position.
    pub anchor: [f32; 3],
    /// Preserved float at prefix offset 12.
    pub reserved_12: f32,
    /// Area metric from the source file.
    pub area_metric: f32,
    /// Area normal.
    pub normal: [f32; 3],
    /// Logic flag.
    pub logic_flag: u32,
    /// Preserved integer at prefix offset 36.
    pub reserved_36: u32,
    /// Area class identifier.
    pub class_id: u32,
    /// Preserved integer at prefix offset 44.
    pub reserved_44: u32,
    /// Boundary vertices.
    pub vertices: Vec<[f32; 3]>,
    /// Edge and polygon links.
    pub links: Vec<EdgeLink>,
    /// Polygon payload blocks.
    pub polygon_blocks: Vec<ArealPolygonBlock>,
}

/// Neighbor link for an areal edge or polygon slot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EdgeLink {
    /// Raw signed area reference.
    pub raw_area_ref: i32,
    /// Raw signed edge reference.
    pub raw_edge_ref: i32,
    /// Referenced area, or `None` for `(-1, -1)`.
    pub area_ref: Option<u32>,
    /// Referenced edge/link slot in the target area, or `None` for `(-1, -1)`.
    pub edge_ref: Option<u32>,
}

/// Preserved polygon block.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArealPolygonBlock {
    /// Leading `n` value.
    pub n: u32,
    /// Raw block following `n`.
    pub body_raw: Vec<u8>,
}

/// Fast area lookup grid.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArealGrid {
    /// Number of cells on X axis.
    pub cells_x: u32,
    /// Number of cells on Y axis.
    pub cells_y: u32,
    /// Per-cell decoded candidates.
    pub cells: Vec<ArealGridCell>,
    /// Concatenated candidate pool used by compact lookup.
    pub candidate_pool: Vec<u32>,
    /// Per-cell compact descriptor: high 10 bits are hit count, low 22 bits are pool offset.
    pub compact_cells: Vec<u32>,
}

/// Candidate list for one areal grid cell.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArealGridCell {
    /// Area identifiers referenced by this cell.
    pub area_ids: Vec<u32>,
}

/// Build category from `BuildDat.lst`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildCategory {
    /// Category name from the section header.
    pub name: String,
    /// Known category mask.
    pub mask: u32,
    /// Unit DAT paths listed in the section.
    pub unit_paths: Vec<String>,
}

/// Terrain format error.
#[derive(Debug)]
pub enum TerrainFormatError {
    /// Binary decode error.
    Decode(DecodeError),
    /// Nested `NRes` error.
    Nres(NresError),
    /// Invalid `Land.map` archive entry count.
    InvalidLandMapEntryCount {
        /// Observed entry count.
        entry_count: usize,
    },
    /// Invalid `Land.map` entry type.
    InvalidLandMapEntryType {
        /// Observed type id.
        type_id: u32,
    },
    /// Missing required stream.
    MissingStream {
        /// Stream type id.
        type_id: u32,
    },
    /// Duplicate required stream.
    DuplicateStream {
        /// Stream type id.
        type_id: u32,
    },
    /// Invalid stream stride.
    InvalidStride {
        /// Stream type id.
        type_id: u32,
        /// Observed stride.
        stride: u32,
        /// Expected stride.
        expected: u32,
    },
    /// Invalid stream size.
    InvalidSize {
        /// Stream type id.
        type_id: u32,
        /// Observed size.
        size: usize,
        /// Expected stride or framing.
        stride: usize,
    },
    /// Stream count does not match payload size.
    CountMismatch {
        /// Stream type id.
        type_id: u32,
        /// Attribute count.
        attr_count: u32,
        /// Payload-derived count.
        payload_count: usize,
    },
    /// Invalid vertex.
    InvalidVertexIndex {
        /// Face index.
        face: usize,
        /// Vertex index.
        vertex: u16,
        /// Position count.
        position_count: usize,
    },
    /// Invalid neighbor.
    InvalidNeighborIndex {
        /// Face index.
        face: usize,
        /// Neighbor index.
        neighbor: u16,
        /// Face count.
        face_count: usize,
    },
    /// Invalid areal link.
    InvalidArealLink {
        /// Source area index.
        area: usize,
        /// Source link index.
        link: usize,
        /// Raw area reference.
        area_ref: i32,
        /// Raw edge reference.
        edge_ref: i32,
    },
    /// Invalid grid dimensions.
    InvalidGridSize {
        /// Cells on X axis.
        cells_x: u32,
        /// Cells on Y axis.
        cells_y: u32,
    },
    /// Invalid area reference in a grid cell.
    InvalidGridAreaRef {
        /// Linear cell index.
        cell: usize,
        /// Referenced area.
        area_ref: u32,
        /// Total area count.
        area_count: usize,
    },
    /// Invalid `BuildDat.lst` text encoding.
    InvalidBuildDatUtf8,
    /// Invalid `BuildDat.lst` section structure.
    InvalidBuildDatStructure {
        /// One-based line number.
        line: usize,
        /// Reason.
        reason: &'static str,
    },
    /// Unknown `BuildDat.lst` category name.
    UnknownBuildCategory {
        /// One-based line number.
        line: usize,
        /// Category name.
        name: String,
    },
    /// Integer overflow.
    IntegerOverflow,
}

impl From<DecodeError> for TerrainFormatError {
    fn from(value: DecodeError) -> Self {
        Self::Decode(value)
    }
}

impl From<NresError> for TerrainFormatError {
    fn from(value: NresError) -> Self {
        Self::Nres(value)
    }
}

impl std::fmt::Display for TerrainFormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Decode(source) => write!(f, "{source}"),
            Self::Nres(source) => write!(f, "{source}"),
            Self::InvalidLandMapEntryCount { entry_count } => {
                write!(f, "Land.map must contain exactly one entry, got {entry_count}")
            }
            Self::InvalidLandMapEntryType { type_id } => {
                write!(f, "Land.map entry type must be 12, got {type_id}")
            }
            Self::MissingStream { type_id } => write!(f, "missing Land.msh stream {type_id}"),
            Self::DuplicateStream { type_id } => write!(f, "duplicate Land.msh stream {type_id}"),
            Self::InvalidStride {
                type_id,
                stride,
                expected,
            } => write!(
                f,
                "invalid Land.msh stream {type_id} stride {stride}, expected {expected}"
            ),
            Self::InvalidSize {
                type_id,
                size,
                stride,
            } => write!(
                f,
                "invalid Land.msh stream {type_id} size {size}, stride/framing {stride}"
            ),
            Self::CountMismatch {
                type_id,
                attr_count,
                payload_count,
            } => write!(
                f,
                "Land.msh stream {type_id} count mismatch: attr={attr_count}, payload={payload_count}"
            ),
            Self::InvalidVertexIndex {
                face,
                vertex,
                position_count,
            } => write!(
                f,
                "Land.msh face {face} vertex {vertex} outside {position_count} positions"
            ),
            Self::InvalidNeighborIndex {
                face,
                neighbor,
                face_count,
            } => write!(
                f,
                "Land.msh face {face} neighbor {neighbor} outside {face_count} faces"
            ),
            Self::InvalidArealLink {
                area,
                link,
                area_ref,
                edge_ref,
            } => write!(
                f,
                "Land.map area {area} link {link} has invalid reference ({area_ref}, {edge_ref})"
            ),
            Self::InvalidGridSize { cells_x, cells_y } => {
                write!(f, "Land.map invalid grid size {cells_x}x{cells_y}")
            }
            Self::InvalidGridAreaRef {
                cell,
                area_ref,
                area_count,
            } => write!(
                f,
                "Land.map grid cell {cell} references area {area_ref} outside {area_count} areas"
            ),
            Self::InvalidBuildDatUtf8 => write!(f, "BuildDat.lst is not valid UTF-8/ASCII text"),
            Self::InvalidBuildDatStructure { line, reason } => {
                write!(f, "invalid BuildDat.lst structure at line {line}: {reason}")
            }
            Self::UnknownBuildCategory { line, name } => {
                write!(f, "unknown BuildDat.lst category '{name}' at line {line}")
            }
            Self::IntegerOverflow => write!(f, "integer overflow"),
        }
    }
}

impl std::error::Error for TerrainFormatError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Decode(source) => Some(source),
            Self::Nres(source) => Some(source),
            Self::InvalidLandMapEntryCount { .. }
            | Self::InvalidLandMapEntryType { .. }
            | Self::MissingStream { .. }
            | Self::DuplicateStream { .. }
            | Self::InvalidStride { .. }
            | Self::InvalidSize { .. }
            | Self::CountMismatch { .. }
            | Self::InvalidVertexIndex { .. }
            | Self::InvalidNeighborIndex { .. }
            | Self::InvalidArealLink { .. }
            | Self::InvalidGridSize { .. }
            | Self::InvalidGridAreaRef { .. }
            | Self::InvalidBuildDatUtf8
            | Self::InvalidBuildDatStructure { .. }
            | Self::UnknownBuildCategory { .. }
            | Self::IntegerOverflow => None,
        }
    }
}

/// Decodes a `Land.msh` `NRes` document.
///
/// # Errors
///
/// Returns [`TerrainFormatError`] when required streams are missing, stream
/// strides/counts do not match, or face vertex/neighbor references are invalid.
pub fn decode_land_msh(nres: &NresDocument) -> Result<LandMeshDocument, TerrainFormatError> {
    for type_id in REQUIRED_TYPES {
        require_single_stream(nres, type_id)?;
    }

    let nodes = stream_payload(nres, TYPE_NODES)?;
    let slots = stream_payload(nres, TYPE_SLOTS)?;
    let positions = stream_payload(nres, TYPE_POSITIONS)?;
    let normals = stream_payload(nres, TYPE_NORMALS)?;
    let uv0 = stream_payload(nres, TYPE_UV0)?;
    let accelerator = stream_payload(nres, TYPE_ACCELERATOR)?;
    let aux14 = stream_payload(nres, TYPE_AUX14)?;
    let aux18 = stream_payload(nres, TYPE_AUX18)?;
    let faces = stream_payload(nres, TYPE_FACES)?;

    validate_stream(nres, TYPE_NODES, 38, nodes.len() / 38)?;
    validate_slots(nres, slots)?;
    let positions = parse_positions(nres, positions)?;
    let normals = parse_i8x4_stream(nres, TYPE_NORMALS, normals)?;
    let uv0 = parse_i16x2_stream(nres, TYPE_UV0, uv0)?;
    let accelerator = parse_word_stream(nres, TYPE_ACCELERATOR, accelerator)?;
    let aux14 = parse_word_stream(nres, TYPE_AUX14, aux14)?;
    let aux18 = parse_word_stream(nres, TYPE_AUX18, aux18)?;
    let faces = parse_faces(nres, faces)?;
    validate_faces(&faces, positions.len())?;

    Ok(LandMeshDocument {
        streams: nres
            .entries()
            .iter()
            .map(|entry| TerrainStream {
                type_id: entry.meta().type_id,
                attributes: attributes(entry.meta()),
                size: entry.meta().data_size,
            })
            .collect(),
        nodes_raw: nodes.to_vec(),
        slots: parse_slot_table(slots),
        positions,
        normals,
        uv0,
        accelerator,
        aux14,
        aux18,
        faces,
    })
}

/// Decodes a `Land.map` `NRes` document.
///
/// # Errors
///
/// Returns [`TerrainFormatError`] when the archive does not contain exactly one
/// type 12 entry, the payload framing is invalid, references are out of range,
/// or the parser does not finish exactly at EOF.
pub fn decode_land_map(nres: &NresDocument) -> Result<LandMapDocument, TerrainFormatError> {
    if nres.entry_count() != 1 {
        return Err(TerrainFormatError::InvalidLandMapEntryCount {
            entry_count: nres.entry_count(),
        });
    }
    let entry = &nres.entries()[0];
    let meta = entry.meta();
    if meta.type_id != TYPE_AREAL_MAP {
        return Err(TerrainFormatError::InvalidLandMapEntryType {
            type_id: meta.type_id,
        });
    }
    let payload = nres.payload(entry.id())?;
    let areal_count =
        usize::try_from(meta.attr1).map_err(|_| TerrainFormatError::IntegerOverflow)?;
    let mut cursor = Cursor::new(payload);
    let mut areals = Vec::with_capacity(areal_count);
    for area_index in 0..areal_count {
        areals.push(parse_areal(&mut cursor, area_index)?);
    }
    validate_areal_links(&areals)?;
    let grid = parse_areal_grid(&mut cursor, areals.len())?;
    cursor.require_eof()?;

    Ok(LandMapDocument {
        entry: TerrainStream {
            type_id: meta.type_id,
            attributes: attributes(meta),
            size: meta.data_size,
        },
        areal_count: meta.attr1,
        areals,
        grid,
    })
}

/// Decodes `Build.dat`.
///
/// # Errors
///
/// Returns [`TerrainFormatError`] when the file contains malformed sections,
/// unknown category names, invalid counts, or invalid quoted unit paths.
pub fn decode_build_dat(bytes: &[u8]) -> Result<Vec<BuildCategory>, TerrainFormatError> {
    let text = std::str::from_utf8(bytes).map_err(|_| TerrainFormatError::InvalidBuildDatUtf8)?;
    let mut categories = Vec::new();
    let mut iter = text.lines().enumerate().peekable();

    while let Some((line_index, raw_line)) = iter.next() {
        let line_no = line_index + 1;
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }

        let (name, count) = parse_build_header(line_no, line)?;
        let mask =
            build_category_mask(name).ok_or_else(|| TerrainFormatError::UnknownBuildCategory {
                line: line_no,
                name: name.to_string(),
            })?;
        let mut unit_paths = Vec::with_capacity(count);
        for _ in 0..count {
            let Some((path_line_index, path_line_raw)) = iter.next() else {
                return Err(TerrainFormatError::InvalidBuildDatStructure {
                    line: line_no,
                    reason: "section ended before declared path count",
                });
            };
            let path_line_no = path_line_index + 1;
            let path_line = path_line_raw.trim();
            unit_paths.push(parse_quoted_path(path_line_no, path_line)?);
        }
        categories.push(BuildCategory {
            name: name.to_string(),
            mask,
            unit_paths,
        });
    }

    Ok(categories)
}

/// Converts full mask to compact mask with explicit bit preservation policy.
#[must_use]
pub fn full_to_compact(mask: FullSurfaceMask) -> CompactSurfaceMask {
    let mut compact = 0u16;
    for (full_bit, compact_bit) in SURFACE_MASK_MAP {
        if mask.0 & full_bit != 0 {
            compact |= compact_bit;
        }
    }
    CompactSurfaceMask(compact)
}

/// Converts compact mask to full mask.
#[must_use]
pub fn compact_to_full(mask: CompactSurfaceMask) -> FullSurfaceMask {
    let mut full = 0u32;
    for (full_bit, compact_bit) in SURFACE_MASK_MAP {
        if mask.0 & compact_bit != 0 {
            full |= full_bit;
        }
    }
    FullSurfaceMask(full)
}

/// Converts full mask to compact material class mask.
#[must_use]
pub fn full_to_material_class(mask: FullSurfaceMask) -> MaterialClassMask {
    let mut compact = 0u8;
    for (full_bit, compact_bit) in MATERIAL_MASK_MAP {
        if mask.0 & full_bit != 0 {
            compact |= compact_bit;
        }
    }
    MaterialClassMask(compact)
}

/// Validates face references.
///
/// # Errors
///
/// Returns [`TerrainFormatError`] when a face references a vertex or neighbor
/// outside the decoded document.
pub fn validate_faces(
    faces: &[TerrainFace28],
    vertex_count: usize,
) -> Result<(), TerrainFormatError> {
    for (face_index, face) in faces.iter().enumerate() {
        for vertex in face.vertices {
            if usize::from(vertex) >= vertex_count {
                return Err(TerrainFormatError::InvalidVertexIndex {
                    face: face_index,
                    vertex,
                    position_count: vertex_count,
                });
            }
        }
        for neighbor in face.neighbors.iter().flatten() {
            if usize::from(*neighbor) >= faces.len() {
                return Err(TerrainFormatError::InvalidNeighborIndex {
                    face: face_index,
                    neighbor: *neighbor,
                    face_count: faces.len(),
                });
            }
        }
    }
    Ok(())
}

const BUILD_CATEGORY_MASKS: &[(&str, u32)] = &[
    ("Bunker_Small", 0x8001_0000),
    ("Bunker_Medium", 0x8002_0000),
    ("Bunker_Large", 0x8004_0000),
    ("Generator", 0x8000_0002),
    ("Mine", 0x8000_0004),
    ("Storage", 0x8000_0008),
    ("Plant", 0x8000_0010),
    ("Hangar", 0x8000_0040),
    ("MainTeleport", 0x8000_0200),
    ("Institute", 0x8000_0400),
    ("Tower_Medium", 0x8010_0000),
    ("Tower_Large", 0x8020_0000),
];

const SURFACE_MASK_MAP: &[(u32, u16)] = &[
    (0x0000_0001, 0x0001),
    (0x0000_0008, 0x0002),
    (0x0000_0010, 0x0004),
    (0x0000_0020, 0x0008),
    (0x0000_1000, 0x0010),
    (0x0000_4000, 0x0020),
    (0x0000_0002, 0x0040),
    (0x0000_0400, 0x0080),
    (0x0000_0800, 0x0100),
    (0x0002_0000, 0x0200),
    (0x0000_2000, 0x0400),
    (0x0000_0200, 0x0800),
    (0x0000_0004, 0x1000),
    (0x0000_0040, 0x2000),
    (0x0020_0000, 0x8000),
];

const MATERIAL_MASK_MAP: &[(u32, u8)] = &[
    (0x0000_0100, 0x01),
    (0x0000_8000, 0x02),
    (0x0001_0000, 0x04),
    (0x0004_0000, 0x08),
    (0x0008_0000, 0x10),
    (0x0000_0080, 0x20),
];

fn parse_build_header(line: usize, text: &str) -> Result<(&str, usize), TerrainFormatError> {
    let mut parts = text.split_ascii_whitespace();
    let name = parts
        .next()
        .ok_or(TerrainFormatError::InvalidBuildDatStructure {
            line,
            reason: "missing category name",
        })?;
    let count_raw = parts
        .next()
        .ok_or(TerrainFormatError::InvalidBuildDatStructure {
            line,
            reason: "missing category count",
        })?;
    if parts.next().is_some() {
        return Err(TerrainFormatError::InvalidBuildDatStructure {
            line,
            reason: "extra fields in category header",
        });
    }
    let count =
        count_raw
            .parse::<usize>()
            .map_err(|_| TerrainFormatError::InvalidBuildDatStructure {
                line,
                reason: "invalid category count",
            })?;
    Ok((name, count))
}

fn parse_quoted_path(line: usize, text: &str) -> Result<String, TerrainFormatError> {
    if text.len() < 2 || !text.starts_with('"') || !text.ends_with('"') {
        return Err(TerrainFormatError::InvalidBuildDatStructure {
            line,
            reason: "unit path must be quoted",
        });
    }
    let path = &text[1..text.len() - 1];
    if path.is_empty() {
        return Err(TerrainFormatError::InvalidBuildDatStructure {
            line,
            reason: "unit path must not be empty",
        });
    }
    if !path.bytes().all(is_build_path_byte) {
        return Err(TerrainFormatError::InvalidBuildDatStructure {
            line,
            reason: "unit path contains invalid byte",
        });
    }
    Ok(path.to_string())
}

fn is_build_path_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'/' | b'\\' | b'-')
}

fn build_category_mask(name: &str) -> Option<u32> {
    BUILD_CATEGORY_MASKS
        .iter()
        .find_map(|(category, mask)| (*category == name).then_some(*mask))
}

fn require_single_stream(nres: &NresDocument, type_id: u32) -> Result<EntryId, TerrainFormatError> {
    let mut found = None;
    for entry in nres
        .entries()
        .iter()
        .filter(|entry| entry.meta().type_id == type_id)
    {
        if found.is_some() {
            return Err(TerrainFormatError::DuplicateStream { type_id });
        }
        found = Some(entry.id());
    }
    found.ok_or(TerrainFormatError::MissingStream { type_id })
}

fn stream_payload(nres: &NresDocument, type_id: u32) -> Result<&[u8], TerrainFormatError> {
    let id = require_single_stream(nres, type_id)?;
    nres.payload(id).map_err(Into::into)
}

fn stream_meta(nres: &NresDocument, type_id: u32) -> Result<&EntryMeta, TerrainFormatError> {
    let id = require_single_stream(nres, type_id)?;
    nres.entry(id)
        .map(fparkan_nres::NresEntry::meta)
        .ok_or(TerrainFormatError::MissingStream { type_id })
}

fn validate_stream(
    nres: &NresDocument,
    type_id: u32,
    stride: usize,
    count: usize,
) -> Result<(), TerrainFormatError> {
    let meta = stream_meta(nres, type_id)?;
    let expected = u32::try_from(stride).map_err(|_| TerrainFormatError::IntegerOverflow)?;
    if meta.attr3 != expected {
        return Err(TerrainFormatError::InvalidStride {
            type_id,
            stride: meta.attr3,
            expected,
        });
    }
    let attr_count =
        usize::try_from(meta.attr1).map_err(|_| TerrainFormatError::IntegerOverflow)?;
    if attr_count != count {
        return Err(TerrainFormatError::CountMismatch {
            type_id,
            attr_count: meta.attr1,
            payload_count: count,
        });
    }
    Ok(())
}

fn validate_slots(nres: &NresDocument, payload: &[u8]) -> Result<(), TerrainFormatError> {
    let meta = stream_meta(nres, TYPE_SLOTS)?;
    if payload.len() < SLOT_HEADER_SIZE {
        return Err(TerrainFormatError::InvalidSize {
            type_id: TYPE_SLOTS,
            size: payload.len(),
            stride: SLOT_HEADER_SIZE,
        });
    }
    let tail = payload.len() - SLOT_HEADER_SIZE;
    if !tail.is_multiple_of(SLOT_STRIDE) {
        return Err(TerrainFormatError::InvalidSize {
            type_id: TYPE_SLOTS,
            size: payload.len(),
            stride: SLOT_STRIDE,
        });
    }
    let slots = tail / SLOT_STRIDE;
    let attr_count =
        usize::try_from(meta.attr1).map_err(|_| TerrainFormatError::IntegerOverflow)?;
    if attr_count != slots {
        return Err(TerrainFormatError::CountMismatch {
            type_id: TYPE_SLOTS,
            attr_count: meta.attr1,
            payload_count: slots,
        });
    }
    Ok(())
}

fn parse_slot_table(payload: &[u8]) -> TerrainSlotTable {
    let mut slots_raw = Vec::new();
    for chunk in payload[SLOT_HEADER_SIZE..].chunks_exact(SLOT_STRIDE) {
        let mut raw = [0; SLOT_STRIDE];
        raw.copy_from_slice(chunk);
        slots_raw.push(raw);
    }
    TerrainSlotTable {
        header_raw: payload[..SLOT_HEADER_SIZE].to_vec(),
        slots_raw,
    }
}

fn parse_positions(
    nres: &NresDocument,
    payload: &[u8],
) -> Result<Vec<[f32; 3]>, TerrainFormatError> {
    if !payload.len().is_multiple_of(12) {
        return Err(TerrainFormatError::InvalidSize {
            type_id: TYPE_POSITIONS,
            size: payload.len(),
            stride: 12,
        });
    }
    let count = payload.len() / 12;
    validate_stream(nres, TYPE_POSITIONS, 12, count)?;
    let mut out = Vec::with_capacity(count);
    for chunk in payload.chunks_exact(12) {
        out.push([
            read_f32(chunk, 0)?,
            read_f32(chunk, 4)?,
            read_f32(chunk, 8)?,
        ]);
    }
    Ok(out)
}

fn parse_i8x4_stream(
    nres: &NresDocument,
    type_id: u32,
    payload: &[u8],
) -> Result<Vec<[i8; 4]>, TerrainFormatError> {
    if !payload.len().is_multiple_of(4) {
        return Err(TerrainFormatError::InvalidSize {
            type_id,
            size: payload.len(),
            stride: 4,
        });
    }
    let count = payload.len() / 4;
    validate_stream(nres, type_id, 4, count)?;
    Ok(payload
        .chunks_exact(4)
        .map(|chunk| {
            [
                i8::from_le_bytes([chunk[0]]),
                i8::from_le_bytes([chunk[1]]),
                i8::from_le_bytes([chunk[2]]),
                i8::from_le_bytes([chunk[3]]),
            ]
        })
        .collect())
}

fn parse_i16x2_stream(
    nres: &NresDocument,
    type_id: u32,
    payload: &[u8],
) -> Result<Vec<[i16; 2]>, TerrainFormatError> {
    if !payload.len().is_multiple_of(4) {
        return Err(TerrainFormatError::InvalidSize {
            type_id,
            size: payload.len(),
            stride: 4,
        });
    }
    let count = payload.len() / 4;
    validate_stream(nres, type_id, 4, count)?;
    let mut out = Vec::with_capacity(count);
    for chunk in payload.chunks_exact(4) {
        out.push([read_i16(chunk, 0)?, read_i16(chunk, 2)?]);
    }
    Ok(out)
}

fn parse_word_stream(
    nres: &NresDocument,
    type_id: u32,
    payload: &[u8],
) -> Result<Vec<[u8; 4]>, TerrainFormatError> {
    if !payload.len().is_multiple_of(4) {
        return Err(TerrainFormatError::InvalidSize {
            type_id,
            size: payload.len(),
            stride: 4,
        });
    }
    let count = payload.len() / 4;
    validate_stream(nres, type_id, 4, count)?;
    Ok(payload
        .chunks_exact(4)
        .map(|chunk| [chunk[0], chunk[1], chunk[2], chunk[3]])
        .collect())
}

fn parse_faces(
    nres: &NresDocument,
    payload: &[u8],
) -> Result<Vec<TerrainFace28>, TerrainFormatError> {
    if !payload.len().is_multiple_of(28) {
        return Err(TerrainFormatError::InvalidSize {
            type_id: TYPE_FACES,
            size: payload.len(),
            stride: 28,
        });
    }
    let count = payload.len() / 28;
    validate_stream(nres, TYPE_FACES, 28, count)?;
    let mut out = Vec::with_capacity(count);
    for chunk in payload.chunks_exact(28) {
        let mut raw = [0; 28];
        raw.copy_from_slice(chunk);
        let mut tail_raw = [0; 8];
        tail_raw.copy_from_slice(&chunk[20..28]);
        out.push(TerrainFace28 {
            flags: FullSurfaceMask(read_u32(chunk, 0)?),
            material_tag: read_u16(chunk, 4)?,
            aux_tag: read_u16(chunk, 6)?,
            vertices: [
                read_u16(chunk, 8)?,
                read_u16(chunk, 10)?,
                read_u16(chunk, 12)?,
            ],
            neighbors: [
                neighbor(read_u16(chunk, 14)?),
                neighbor(read_u16(chunk, 16)?),
                neighbor(read_u16(chunk, 18)?),
            ],
            tail_raw,
            raw,
        });
    }
    Ok(out)
}

fn neighbor(raw: u16) -> Option<u16> {
    (raw != u16::MAX).then_some(raw)
}

fn parse_areal(cursor: &mut Cursor<'_>, _area_index: usize) -> Result<Areal, TerrainFormatError> {
    let prefix = cursor.read_exact(AREAL_PREFIX_SIZE)?;
    let mut prefix_raw = [0; AREAL_PREFIX_SIZE];
    prefix_raw.copy_from_slice(prefix);
    let vertex_count = read_u32(prefix, 48)?;
    let poly_count = read_u32(prefix, 52)?;
    let vertices = parse_areal_vertices(cursor, vertex_count)?;
    let link_count = vertex_count
        .checked_add(
            poly_count
                .checked_mul(3)
                .ok_or(TerrainFormatError::IntegerOverflow)?,
        )
        .ok_or(TerrainFormatError::IntegerOverflow)?;
    let links = parse_edge_links(cursor, link_count)?;
    let polygon_blocks = parse_polygon_blocks(cursor, poly_count)?;

    Ok(Areal {
        prefix_raw,
        anchor: [
            read_f32(prefix, 0)?,
            read_f32(prefix, 4)?,
            read_f32(prefix, 8)?,
        ],
        reserved_12: read_f32(prefix, 12)?,
        area_metric: read_f32(prefix, 16)?,
        normal: [
            read_f32(prefix, 20)?,
            read_f32(prefix, 24)?,
            read_f32(prefix, 28)?,
        ],
        logic_flag: read_u32(prefix, 32)?,
        reserved_36: read_u32(prefix, 36)?,
        class_id: read_u32(prefix, 40)?,
        reserved_44: read_u32(prefix, 44)?,
        vertices,
        links,
        polygon_blocks,
    })
}

fn parse_areal_vertices(
    cursor: &mut Cursor<'_>,
    vertex_count: u32,
) -> Result<Vec<[f32; 3]>, TerrainFormatError> {
    checked_count_bytes(u64::from(vertex_count), 12, cursor.remaining() as u64)?;
    let count = usize::try_from(vertex_count).map_err(|_| TerrainFormatError::IntegerOverflow)?;
    let mut vertices = Vec::with_capacity(count);
    for _ in 0..count {
        vertices.push([
            cursor.read_f32_le()?,
            cursor.read_f32_le()?,
            cursor.read_f32_le()?,
        ]);
    }
    Ok(vertices)
}

fn parse_edge_links(
    cursor: &mut Cursor<'_>,
    link_count: u32,
) -> Result<Vec<EdgeLink>, TerrainFormatError> {
    checked_count_bytes(u64::from(link_count), 8, cursor.remaining() as u64)?;
    let count = usize::try_from(link_count).map_err(|_| TerrainFormatError::IntegerOverflow)?;
    let mut links = Vec::with_capacity(count);
    for _ in 0..count {
        let raw_area_ref = cursor.read_i32_le()?;
        let raw_edge_ref = cursor.read_i32_le()?;
        let (area_ref, edge_ref) = match (raw_area_ref, raw_edge_ref) {
            (-1, -1) => (None, None),
            (area, edge) if area >= 0 && edge >= 0 => {
                let area = u32::try_from(area).map_err(|_| TerrainFormatError::IntegerOverflow)?;
                let edge = u32::try_from(edge).map_err(|_| TerrainFormatError::IntegerOverflow)?;
                (Some(area), Some(edge))
            }
            _ => (None, None),
        };
        links.push(EdgeLink {
            raw_area_ref,
            raw_edge_ref,
            area_ref,
            edge_ref,
        });
    }
    Ok(links)
}

fn parse_polygon_blocks(
    cursor: &mut Cursor<'_>,
    poly_count: u32,
) -> Result<Vec<ArealPolygonBlock>, TerrainFormatError> {
    let count = usize::try_from(poly_count).map_err(|_| TerrainFormatError::IntegerOverflow)?;
    let mut blocks = Vec::with_capacity(count);
    for _ in 0..count {
        let n = cursor.read_u32_le()?;
        let word_count = u64::from(n)
            .checked_mul(3)
            .and_then(|count| count.checked_add(1))
            .ok_or(TerrainFormatError::IntegerOverflow)?;
        let byte_count = checked_count_bytes(word_count, 4, cursor.remaining() as u64)?;
        blocks.push(ArealPolygonBlock {
            n,
            body_raw: cursor.read_exact(byte_count)?.to_vec(),
        });
    }
    Ok(blocks)
}

fn validate_areal_links(areals: &[Areal]) -> Result<(), TerrainFormatError> {
    for (area_index, area) in areals.iter().enumerate() {
        for (link_index, link) in area.links.iter().enumerate() {
            match (link.area_ref, link.edge_ref) {
                (None, None) if link.raw_area_ref == -1 && link.raw_edge_ref == -1 => {}
                (Some(area_ref), Some(edge_ref)) => {
                    let Some(target) = usize::try_from(area_ref)
                        .ok()
                        .and_then(|index| areals.get(index))
                    else {
                        return Err(invalid_areal_link(area_index, link_index, link));
                    };
                    let edge_index = usize::try_from(edge_ref)
                        .map_err(|_| TerrainFormatError::IntegerOverflow)?;
                    if edge_index >= target.links.len() {
                        return Err(invalid_areal_link(area_index, link_index, link));
                    }
                }
                _ => return Err(invalid_areal_link(area_index, link_index, link)),
            }
        }
    }
    Ok(())
}

fn invalid_areal_link(area: usize, link: usize, edge_link: &EdgeLink) -> TerrainFormatError {
    TerrainFormatError::InvalidArealLink {
        area,
        link,
        area_ref: edge_link.raw_area_ref,
        edge_ref: edge_link.raw_edge_ref,
    }
}

fn parse_areal_grid(
    cursor: &mut Cursor<'_>,
    area_count: usize,
) -> Result<ArealGrid, TerrainFormatError> {
    let cells_x = cursor.read_u32_le()?;
    let cells_y = cursor.read_u32_le()?;
    let cell_count = cells_x
        .checked_mul(cells_y)
        .ok_or(TerrainFormatError::IntegerOverflow)?;
    if cell_count == 0 {
        return Err(TerrainFormatError::InvalidGridSize { cells_x, cells_y });
    }
    let cell_count_usize =
        usize::try_from(cell_count).map_err(|_| TerrainFormatError::IntegerOverflow)?;
    let mut cells = Vec::with_capacity(cell_count_usize);
    let mut candidate_pool = Vec::new();
    let mut compact_cells = Vec::with_capacity(cell_count_usize);
    for cell_index in 0..cell_count_usize {
        let hit_count = cursor.read_u16_le()?;
        let pool_offset =
            u32::try_from(candidate_pool.len()).map_err(|_| TerrainFormatError::IntegerOverflow)?;
        if u32::from(hit_count) >= (1 << GRID_HIT_COUNT_BITS) || pool_offset > GRID_POOL_OFFSET_MASK
        {
            return Err(TerrainFormatError::IntegerOverflow);
        }
        let mut area_ids = Vec::with_capacity(usize::from(hit_count));
        for _ in 0..hit_count {
            let area_ref = u32::from(cursor.read_u16_le()?);
            if usize::try_from(area_ref).map_or(true, |index| index >= area_count) {
                return Err(TerrainFormatError::InvalidGridAreaRef {
                    cell: cell_index,
                    area_ref,
                    area_count,
                });
            }
            area_ids.push(area_ref);
            candidate_pool.push(area_ref);
        }
        compact_cells.push((u32::from(hit_count) << 22) | pool_offset);
        cells.push(ArealGridCell { area_ids });
    }
    Ok(ArealGrid {
        cells_x,
        cells_y,
        cells,
        candidate_pool,
        compact_cells,
    })
}

fn attributes(meta: &EntryMeta) -> TerrainStreamAttributes {
    TerrainStreamAttributes {
        attr1: meta.attr1,
        attr2: meta.attr2,
        attr3: meta.attr3,
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, TerrainFormatError> {
    let raw = bytes
        .get(offset..offset + 2)
        .ok_or(TerrainFormatError::IntegerOverflow)?;
    Ok(u16::from_le_bytes([raw[0], raw[1]]))
}

fn read_i16(bytes: &[u8], offset: usize) -> Result<i16, TerrainFormatError> {
    let raw = bytes
        .get(offset..offset + 2)
        .ok_or(TerrainFormatError::IntegerOverflow)?;
    Ok(i16::from_le_bytes([raw[0], raw[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, TerrainFormatError> {
    let raw = bytes
        .get(offset..offset + 4)
        .ok_or(TerrainFormatError::IntegerOverflow)?;
    Ok(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
}

fn read_f32(bytes: &[u8], offset: usize) -> Result<f32, TerrainFormatError> {
    Ok(f32::from_bits(read_u32(bytes, offset)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_nres::ReadProfile;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    static SLOT_HEADER_ZERO: [u8; SLOT_HEADER_SIZE] = [0; SLOT_HEADER_SIZE];
    static STREAM12_ZERO: [u8; 12] = [0; 12];

    #[test]
    fn decodes_minimal_land_msh() {
        let nres =
            decode_nres(&minimal_land_msh(&face([0, 1, 2], [None, None, None]))).expect("nres");
        let document = decode_land_msh(&nres).expect("land mesh");

        assert_eq!(document.positions.len(), 3);
        assert_eq!(document.faces.len(), 1);
        assert_eq!(document.faces[0].vertices, [0, 1, 2]);
        assert_eq!(document.faces[0].neighbors, [None, None, None]);
    }

    #[test]
    fn land_msh_required_streams_are_order_independent_and_stride_checked() {
        let face = face([0, 1, 2], [None, None, None]);
        let positions = minimal_positions_payload();
        let entries = minimal_land_msh_entries(&face, &positions);
        let shuffled = [
            entries[8], entries[2], entries[0], entries[7], entries[4], entries[3], entries[6],
            entries[5], entries[1],
        ];
        let nres = decode_nres(&build_nres(&shuffled)).expect("nres");
        let document = decode_land_msh(&nres).expect("land mesh");
        assert_eq!(document.positions.len(), 3);
        assert_eq!(
            document
                .streams
                .iter()
                .map(|stream| stream.type_id)
                .collect::<Vec<_>>(),
            vec![
                TYPE_FACES,
                TYPE_POSITIONS,
                TYPE_NODES,
                TYPE_ACCELERATOR,
                TYPE_UV0,
                TYPE_NORMALS,
                TYPE_AUX14,
                TYPE_AUX18,
                TYPE_SLOTS,
            ]
        );

        let bad_stride = [
            entries[0],
            entries[1],
            entries[2],
            entry(TYPE_NORMALS, 3, 8, &[0; 12]),
            entries[4],
            entries[5],
            entries[6],
            entries[7],
            entries[8],
        ];
        let nres = decode_nres(&build_nres(&bad_stride)).expect("nres");
        assert!(matches!(
            decode_land_msh(&nres),
            Err(TerrainFormatError::InvalidStride {
                type_id: TYPE_NORMALS,
                ..
            })
        ));
    }

    #[test]
    fn rejects_invalid_vertex_index() {
        let nres =
            decode_nres(&minimal_land_msh(&face([0, 1, 3], [None, None, None]))).expect("nres");
        let err = decode_land_msh(&nres).expect_err("invalid vertex");

        assert!(matches!(
            err,
            TerrainFormatError::InvalidVertexIndex { vertex: 3, .. }
        ));
    }

    #[test]
    fn rejects_invalid_neighbor_index() {
        let nres =
            decode_nres(&minimal_land_msh(&face([0, 1, 2], [Some(1), None, None]))).expect("nres");
        let err = decode_land_msh(&nres).expect_err("invalid neighbor");

        assert!(matches!(
            err,
            TerrainFormatError::InvalidNeighborIndex { neighbor: 1, .. }
        ));
    }

    #[test]
    fn face_layout_preserves_tail_and_all_surface_mask_mappings_are_explicit() {
        let mut raw_face = face([0, 1, 2], [None, None, None]);
        raw_face[20..28].copy_from_slice(b"UNKNOWN!");
        let nres = decode_nres(&minimal_land_msh(&raw_face)).expect("nres");
        let document = decode_land_msh(&nres).expect("land mesh");
        assert_eq!(document.faces[0].tail_raw, *b"UNKNOWN!");
        assert_eq!(document.faces[0].raw, raw_face);

        for (full, compact) in SURFACE_MASK_MAP {
            assert_eq!(
                full_to_compact(FullSurfaceMask(*full)),
                CompactSurfaceMask(*compact)
            );
            assert_eq!(
                compact_to_full(CompactSurfaceMask(*compact)),
                FullSurfaceMask(*full)
            );
        }
        assert_eq!(
            full_to_compact(FullSurfaceMask(0x0000_0008)),
            CompactSurfaceMask(0x0002)
        );
        assert_eq!(
            full_to_compact(FullSurfaceMask(0x0020_0000)),
            CompactSurfaceMask(0x8000)
        );
        assert_eq!(
            compact_to_full(CompactSurfaceMask(0x8000)),
            FullSurfaceMask(0x0020_0000)
        );
        assert_eq!(
            full_to_material_class(FullSurfaceMask(0x0000_8000 | 0x0000_0080)),
            MaterialClassMask(0x22)
        );
    }

    #[test]
    fn decodes_minimal_land_map() {
        let nres = decode_nres(&minimal_land_map([(-1, -1), (-1, -1)], 0)).expect("nres");
        let document = decode_land_map(&nres).expect("land map");

        assert_eq!(document.areal_count, 1);
        assert_eq!(document.areals.len(), 1);
        assert_eq!(document.areals[0].vertices.len(), 2);
        assert_eq!(document.areals[0].links.len(), 2);
        assert_eq!(document.grid.cells_x, 1);
        assert_eq!(document.grid.cells_y, 1);
        assert_eq!(document.grid.cells[0].area_ids, [0]);
        assert_eq!(document.grid.compact_cells, [0x0040_0000]);
    }

    #[test]
    fn land_map_prefix_absent_links_polygon_blocks_grid_size_and_exact_eof() {
        let nres = decode_nres(&minimal_land_map_with_poly(1, true)).expect("nres");
        let document = decode_land_map(&nres).expect("land map");
        assert_eq!(document.areals[0].prefix_raw.len(), AREAL_PREFIX_SIZE);
        assert_eq!(document.areals[0].anchor, [0.0, 0.0, 0.0]);
        assert_eq!(document.areals[0].area_metric, 2.0);
        assert_eq!(document.areals[0].links[0].area_ref, None);
        assert_eq!(document.areals[0].polygon_blocks.len(), 1);
        assert_eq!(document.areals[0].links.len(), 5);
        assert_eq!(document.grid.cells_x, 1);
        assert_eq!(document.grid.cells_y, 1);

        let nres = decode_nres(&minimal_land_map_with_vertex_count(3)).expect("nres");
        assert!(decode_land_map(&nres).is_err());

        let nres = decode_nres(&minimal_land_map_with_poly(1_000_000, true)).expect("nres");
        assert!(decode_land_map(&nres).is_err());

        let nres = decode_nres(&minimal_land_map_with_poly(0, false)).expect("nres");
        assert!(matches!(
            decode_land_map(&nres),
            Err(TerrainFormatError::InvalidGridSize { cells_x: 0, .. })
        ));

        let nres = decode_nres(&minimal_land_map_with_payload_tail()).expect("nres");
        assert!(decode_land_map(&nres).is_err());
    }

    #[test]
    fn rejects_invalid_areal_link() {
        let nres = decode_nres(&minimal_land_map([(1, 0), (-1, -1)], 0)).expect("nres");
        let err = decode_land_map(&nres).expect_err("invalid link");

        assert!(matches!(
            err,
            TerrainFormatError::InvalidArealLink {
                area: 0,
                link: 0,
                area_ref: 1,
                edge_ref: 0
            }
        ));
    }

    #[test]
    fn rejects_invalid_grid_area_ref() {
        let nres = decode_nres(&minimal_land_map([(-1, -1), (-1, -1)], 1)).expect("nres");
        let err = decode_land_map(&nres).expect_err("invalid grid");

        assert!(matches!(
            err,
            TerrainFormatError::InvalidGridAreaRef {
                cell: 0,
                area_ref: 1,
                area_count: 1
            }
        ));
    }

    #[test]
    fn decodes_synthetic_build_dat() {
        let bytes = br#"
// comment
Bunker_Small 2
  "UNITS\BUILDS\BUNKER\sbunk01.dat"
  "UNITS\BUILDS\BUNKER\sbunk02.dat"
Generator 1
  "UNITS\BUILDS\GENER\gener01.dat"
"#;
        let categories = decode_build_dat(bytes).expect("BuildDat");

        assert_eq!(categories.len(), 2);
        assert_eq!(categories[0].name, "Bunker_Small");
        assert_eq!(categories[0].mask, 0x8001_0000);
        assert_eq!(categories[0].unit_paths.len(), 2);
        assert_eq!(categories[1].name, "Generator");
        assert_eq!(categories[1].mask, 0x8000_0002);
    }

    #[test]
    fn rejects_unknown_build_category() {
        let err = decode_build_dat(br#"Unknown 0"#).expect_err("unknown category");

        assert!(matches!(
            err,
            TerrainFormatError::UnknownBuildCategory { line: 1, .. }
        ));
    }

    #[test]
    fn rejects_build_category_count_mismatch() {
        let err = decode_build_dat(
            br#"Bunker_Small 2
  "UNITS\BUILDS\BUNKER\sbunk01.dat"
"#,
        )
        .expect_err("count mismatch");

        assert!(matches!(
            err,
            TerrainFormatError::InvalidBuildDatStructure { line: 1, .. }
        ));
    }

    #[test]
    fn licensed_corpus_land_msh_validate() {
        for (corpus, expected_files, expected_vertices, expected_faces) in [
            ("IS", 33_usize, 299_450_usize, 275_882_usize),
            ("IS2", 32_usize, 188_024_usize, 184_454_usize),
        ] {
            let Some(root) = corpus_root(corpus) else {
                continue;
            };
            let mut files = 0usize;
            let mut vertices = 0usize;
            let mut faces = 0usize;
            for path in files_under(&root) {
                if !path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.eq_ignore_ascii_case("Land.msh"))
                {
                    continue;
                }
                let bytes = std::fs::read(&path).expect("read Land.msh");
                let nres = fparkan_nres::decode(
                    Arc::from(bytes.into_boxed_slice()),
                    ReadProfile::Compatible,
                )
                .unwrap_or_else(|err| panic!("{corpus} {path:?}: {err}"));
                let document =
                    decode_land_msh(&nres).unwrap_or_else(|err| panic!("{corpus} {path:?}: {err}"));
                files += 1;
                vertices += document.positions.len();
                faces += document.faces.len();
                assert_eq!(
                    document
                        .streams
                        .iter()
                        .map(|stream| stream.type_id)
                        .collect::<Vec<_>>(),
                    REQUIRED_TYPES,
                    "{corpus} {path:?} stream order"
                );
            }

            assert_eq!(files, expected_files, "{corpus} Land.msh count");
            assert_eq!(vertices, expected_vertices, "{corpus} vertex count");
            assert_eq!(faces, expected_faces, "{corpus} face count");
        }
    }

    #[test]
    fn licensed_corpus_build_dat_validate() {
        for (corpus, expected_ai_prefix) in [("IS", false), ("IS2", true)] {
            let Some(root) = corpus_root(corpus) else {
                continue;
            };
            let path = root.join("BuildDat.lst");
            let bytes = std::fs::read(&path).expect("read BuildDat.lst");
            let categories =
                decode_build_dat(&bytes).unwrap_or_else(|err| panic!("{corpus} {path:?}: {err}"));

            assert_eq!(categories.len(), BUILD_CATEGORY_MASKS.len(), "{corpus}");
            assert_eq!(
                categories
                    .iter()
                    .map(|category| (category.name.as_str(), category.mask))
                    .collect::<Vec<_>>(),
                BUILD_CATEGORY_MASKS,
                "{corpus} category order/masks"
            );
            assert_eq!(
                categories
                    .iter()
                    .map(|category| category.unit_paths.len())
                    .sum::<usize>(),
                32,
                "{corpus} unit path count"
            );
            assert!(
                categories
                    .iter()
                    .all(
                        |category| category.unit_paths.iter().all(|path| path.starts_with(
                            if expected_ai_prefix {
                                "UNITS\\BUILDS\\AI\\"
                            } else {
                                "UNITS\\BUILDS\\"
                            }
                        ) && path
                            .to_ascii_lowercase()
                            .ends_with(".dat"))
                    ),
                "{corpus} unit path prefixes"
            );
        }
    }

    #[test]
    fn licensed_corpus_land_map_validate() {
        for (corpus, expected_files, expected_areals, expected_vertices, expected_max_hits) in [
            ("IS", 33_usize, 34_662_usize, 197_698_usize, 20_usize),
            ("IS2", 32_usize, 18_984_usize, 114_968_usize, 14_usize),
        ] {
            let Some(root) = corpus_root(corpus) else {
                continue;
            };
            let mut files = 0usize;
            let mut areals = 0usize;
            let mut vertices = 0usize;
            let mut max_hits = 0usize;
            for path in files_under(&root) {
                if !path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.eq_ignore_ascii_case("Land.map"))
                {
                    continue;
                }
                let bytes = std::fs::read(&path).expect("read Land.map");
                let nres = fparkan_nres::decode(
                    Arc::from(bytes.into_boxed_slice()),
                    ReadProfile::Compatible,
                )
                .unwrap_or_else(|err| panic!("{corpus} {path:?}: {err}"));
                let document =
                    decode_land_map(&nres).unwrap_or_else(|err| panic!("{corpus} {path:?}: {err}"));
                files += 1;
                areals += document.areals.len();
                vertices += document
                    .areals
                    .iter()
                    .map(|area| area.vertices.len())
                    .sum::<usize>();
                max_hits = max_hits.max(
                    document
                        .grid
                        .cells
                        .iter()
                        .map(|cell| cell.area_ids.len())
                        .max()
                        .unwrap_or(0),
                );
                assert_eq!(document.grid.cells_x, 128, "{corpus} {path:?} cells_x");
                assert_eq!(document.grid.cells_y, 128, "{corpus} {path:?} cells_y");
                assert!(
                    document
                        .areals
                        .iter()
                        .all(|area| area.polygon_blocks.is_empty()),
                    "{corpus} {path:?} polygon blocks"
                );
            }

            assert_eq!(files, expected_files, "{corpus} Land.map count");
            assert_eq!(areals, expected_areals, "{corpus} areal count");
            assert_eq!(vertices, expected_vertices, "{corpus} areal vertex count");
            assert_eq!(max_hits, expected_max_hits, "{corpus} max grid hits");
        }
    }

    fn decode_nres(bytes: &[u8]) -> Result<NresDocument, fparkan_nres::NresError> {
        fparkan_nres::decode(
            Arc::from(bytes.to_vec().into_boxed_slice()),
            ReadProfile::Compatible,
        )
    }

    fn minimal_land_msh(face: &[u8; 28]) -> Vec<u8> {
        let positions = minimal_positions_payload();
        build_nres(&minimal_land_msh_entries(face, &positions))
    }

    fn minimal_positions_payload() -> Vec<u8> {
        [
            0.0_f32.to_le_bytes(),
            0.0_f32.to_le_bytes(),
            0.0_f32.to_le_bytes(),
            1.0_f32.to_le_bytes(),
            0.0_f32.to_le_bytes(),
            0.0_f32.to_le_bytes(),
            0.0_f32.to_le_bytes(),
            1.0_f32.to_le_bytes(),
            0.0_f32.to_le_bytes(),
        ]
        .concat()
    }

    fn minimal_land_msh_entries<'a>(face: &'a [u8; 28], positions: &'a [u8]) -> [TestEntry<'a>; 9] {
        [
            entry(TYPE_NODES, 0, 38, &[]),
            entry(TYPE_SLOTS, 0, 0, &SLOT_HEADER_ZERO),
            entry(TYPE_POSITIONS, 3, 12, positions),
            entry(TYPE_NORMALS, 3, 4, &STREAM12_ZERO),
            entry(TYPE_UV0, 3, 4, &STREAM12_ZERO),
            entry(TYPE_AUX18, 0, 4, &[]),
            entry(TYPE_AUX14, 0, 4, &[]),
            entry(TYPE_ACCELERATOR, 0, 4, &[]),
            entry(TYPE_FACES, 1, 28, face),
        ]
    }

    fn minimal_land_map(links: [(i32, i32); 2], grid_area_ref: u16) -> Vec<u8> {
        let mut payload = Vec::new();
        push_f32(&mut payload, 0.0);
        push_f32(&mut payload, 0.0);
        push_f32(&mut payload, 0.0);
        push_f32(&mut payload, 0.0);
        push_f32(&mut payload, 2.0);
        push_f32(&mut payload, 0.0);
        push_f32(&mut payload, 1.0);
        push_f32(&mut payload, 0.0);
        push_u32(&mut payload, 0);
        push_u32(&mut payload, 0);
        push_u32(&mut payload, 7);
        push_u32(&mut payload, 0);
        push_u32(&mut payload, 2);
        push_u32(&mut payload, 0);
        push_f32(&mut payload, 0.0);
        push_f32(&mut payload, 0.0);
        push_f32(&mut payload, 0.0);
        push_f32(&mut payload, 1.0);
        push_f32(&mut payload, 0.0);
        push_f32(&mut payload, 0.0);
        for (area_ref, edge_ref) in links {
            push_i32(&mut payload, area_ref);
            push_i32(&mut payload, edge_ref);
        }
        push_u32(&mut payload, 1);
        push_u32(&mut payload, 1);
        push_u16(&mut payload, 1);
        push_u16(&mut payload, grid_area_ref);
        build_nres(&[entry(TYPE_AREAL_MAP, 1, 0, &payload)])
    }

    fn minimal_land_map_with_poly(poly_n: u32, valid_grid: bool) -> Vec<u8> {
        let mut payload = Vec::new();
        push_areal_prefix(&mut payload, 2, 1);
        push_f32(&mut payload, 0.0);
        push_f32(&mut payload, 0.0);
        push_f32(&mut payload, 0.0);
        push_f32(&mut payload, 1.0);
        push_f32(&mut payload, 0.0);
        push_f32(&mut payload, 0.0);
        for _ in 0..5 {
            push_i32(&mut payload, -1);
            push_i32(&mut payload, -1);
        }
        push_u32(&mut payload, poly_n);
        match poly_n {
            0 => payload.extend_from_slice(&[0; 4]),
            1 => payload.extend_from_slice(&[0; 16]),
            _ => {}
        }
        if valid_grid {
            push_u32(&mut payload, 1);
            push_u32(&mut payload, 1);
            push_u16(&mut payload, 1);
            push_u16(&mut payload, 0);
        } else {
            push_u32(&mut payload, 0);
            push_u32(&mut payload, 1);
        }
        build_nres(&[entry(TYPE_AREAL_MAP, 1, 0, &payload)])
    }

    fn minimal_land_map_with_vertex_count(vertex_count: u32) -> Vec<u8> {
        let mut payload = Vec::new();
        push_areal_prefix(&mut payload, vertex_count, 0);
        build_nres(&[entry(TYPE_AREAL_MAP, 1, 0, &payload)])
    }

    fn minimal_land_map_with_payload_tail() -> Vec<u8> {
        let mut payload = Vec::new();
        push_areal_prefix(&mut payload, 2, 0);
        push_f32(&mut payload, 0.0);
        push_f32(&mut payload, 0.0);
        push_f32(&mut payload, 0.0);
        push_f32(&mut payload, 1.0);
        push_f32(&mut payload, 0.0);
        push_f32(&mut payload, 0.0);
        for _ in 0..2 {
            push_i32(&mut payload, -1);
            push_i32(&mut payload, -1);
        }
        push_u32(&mut payload, 1);
        push_u32(&mut payload, 1);
        push_u16(&mut payload, 1);
        push_u16(&mut payload, 0);
        payload.push(0);
        build_nres(&[entry(TYPE_AREAL_MAP, 1, 0, &payload)])
    }

    fn push_areal_prefix(payload: &mut Vec<u8>, vertex_count: u32, poly_count: u32) {
        push_f32(payload, 0.0);
        push_f32(payload, 0.0);
        push_f32(payload, 0.0);
        push_f32(payload, 0.0);
        push_f32(payload, 2.0);
        push_f32(payload, 0.0);
        push_f32(payload, 1.0);
        push_f32(payload, 0.0);
        push_u32(payload, 0);
        push_u32(payload, 0);
        push_u32(payload, 7);
        push_u32(payload, 0);
        push_u32(payload, vertex_count);
        push_u32(payload, poly_count);
    }

    fn face(vertices: [u16; 3], neighbors: [Option<u16>; 3]) -> [u8; 28] {
        let mut out = [0; 28];
        out[8..10].copy_from_slice(&vertices[0].to_le_bytes());
        out[10..12].copy_from_slice(&vertices[1].to_le_bytes());
        out[12..14].copy_from_slice(&vertices[2].to_le_bytes());
        for (idx, neighbor) in neighbors.iter().enumerate() {
            let raw = neighbor.unwrap_or(u16::MAX);
            let offset = 14 + idx * 2;
            out[offset..offset + 2].copy_from_slice(&raw.to_le_bytes());
        }
        out[20..28].copy_from_slice(b"TAILFACE");
        out
    }

    fn entry(type_id: u32, attr1: u32, attr3: u32, payload: &[u8]) -> TestEntry<'_> {
        TestEntry {
            type_id,
            attr1,
            attr3,
            payload,
        }
    }

    #[derive(Clone, Copy)]
    struct TestEntry<'a> {
        type_id: u32,
        attr1: u32,
        attr3: u32,
        payload: &'a [u8],
    }

    fn build_nres(entries: &[TestEntry<'_>]) -> Vec<u8> {
        let mut out = vec![0; 16];
        let mut offsets = Vec::with_capacity(entries.len());
        for entry in entries {
            offsets.push(u32::try_from(out.len()).expect("offset"));
            out.extend_from_slice(entry.payload);
            let padding = (8 - (out.len() % 8)) % 8;
            out.resize(out.len() + padding, 0);
        }
        let order: Vec<usize> = (0..entries.len()).collect();
        for (idx, entry) in entries.iter().enumerate() {
            push_u32(&mut out, entry.type_id);
            push_u32(&mut out, entry.attr1);
            push_u32(&mut out, 0);
            push_u32(
                &mut out,
                u32::try_from(entry.payload.len()).expect("payload"),
            );
            push_u32(&mut out, entry.attr3);
            let mut name_raw = [0; 36];
            let name = format!("Res{}", entry.type_id);
            copy_cstr(&mut name_raw, name.as_bytes());
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

    fn copy_cstr(dst: &mut [u8], src: &[u8]) {
        let len = dst.len().saturating_sub(1).min(src.len());
        dst[..len].copy_from_slice(&src[..len]);
    }

    fn push_u32(out: &mut Vec<u8>, value: u32) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn push_i32(out: &mut Vec<u8>, value: i32) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn push_u16(out: &mut Vec<u8>, value: u16) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn push_f32(out: &mut Vec<u8>, value: f32) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn corpus_root(name: &str) -> Option<PathBuf> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("testdata")
            .join(name);
        root.is_dir().then_some(root)
    }

    fn files_under(root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(path) = stack.pop() {
            let Ok(read_dir) = std::fs::read_dir(path) else {
                continue;
            };
            for entry in read_dir.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    out.push(path);
                }
            }
        }
        out.sort();
        out
    }
}
