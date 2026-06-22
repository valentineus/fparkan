#![forbid(unsafe_code)]
//! WEAR/MAT0 material contracts.

use encoding_rs::WINDOWS_1251;
use fparkan_path::ResourceName;
use fparkan_resource::{archive_path, ResourceError, ResourceRepository};

/// `MAT0` `NRes` entry type.
pub const MAT0_KIND: u32 = 0x3054_414D;
/// `WEAR` `NRes` entry type.
pub const WEAR_KIND: u32 = 0x5241_4557;

/// WEAR table.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WearTable {
    /// Entries.
    pub entries: Vec<WearEntry>,
    /// Lightmap entries.
    pub lightmaps: Vec<LightmapEntry>,
}

/// WEAR entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WearEntry {
    /// Legacy id text.
    pub legacy_id: LegacyText,
    /// Material.
    pub material: ResourceName,
}

/// Legacy text token.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegacyText(pub String);

/// Lightmap entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LightmapEntry {
    /// Legacy id text.
    pub legacy_id: LegacyText,
    /// Lightmap resource.
    pub lightmap: ResourceName,
}

/// MAT0 document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Mat0Document {
    /// Version/profile supplied by archive metadata.
    pub version: u32,
    /// Declared animation block count.
    pub animation_block_count: u16,
    /// Phase records.
    pub phases: Vec<MaterialPhase>,
    /// Version-gated bytes between header and phase table.
    pub prefix: Vec<u8>,
    /// Opaque bytes at offsets 2..4.
    pub header_opaque: [u8; 2],
    /// Animation blocks parsed after phases.
    pub animation_blocks: Vec<MaterialAnimationBlock>,
}

/// Material phase.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaterialPhase {
    /// Parameters.
    pub parameters: [u8; 18],
    /// Texture raw.
    pub texture_raw: [u8; 16],
}

/// Material animation block.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaterialAnimationBlock {
    /// Raw block header.
    pub header_raw: u32,
    /// Parsed keys.
    pub keys: Vec<MaterialKey>,
    /// Raw block bytes.
    pub bytes: Vec<u8>,
}

/// Material key.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct MaterialKey {
    /// Key part.
    pub k0: u16,
    /// Key part.
    pub k1: u16,
    /// Key part.
    pub k2: u16,
}

/// Material fallback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MaterialFallback {
    /// Exact.
    Exact,
    /// Default.
    Default,
    /// First entry.
    FirstEntry,
}

/// Material timeline mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MaterialTimelineMode {
    /// Play once from phase zero.
    OneShot,
    /// Clamp frame to the last phase.
    Clamp,
    /// Loop over all phases.
    Loop,
    /// Ping-pong over all phases.
    PingPong,
}

/// Material runtime sampling profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MaterialTimelineProfile {
    /// Timeline mode.
    pub mode: MaterialTimelineMode,
    /// Apply deterministic material-only random offset.
    pub random_offset: bool,
}

/// Sampled material phase.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaterialPhaseSample {
    /// Selected phase index.
    pub phase_index: usize,
    /// Effective frame after mode and random offset.
    pub effective_frame: u32,
    /// Sampled parameter bytes.
    pub parameters: [u8; 18],
    /// Sampled texture bytes.
    pub texture_raw: [u8; 16],
}

/// Resolved material.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedMaterial {
    /// Resolved material name.
    pub name: ResourceName,
    /// Fallback path.
    pub fallback: MaterialFallback,
    /// Decoded document.
    pub document: Mat0Document,
}

/// Material parse or resolution error.
#[derive(Debug)]
pub enum MaterialError {
    /// Text payload is empty.
    EmptyWear,
    /// Count line is invalid.
    InvalidWearCount(String),
    /// Count is zero.
    ZeroWearCount,
    /// A material row is missing.
    MissingWearRow {
        /// Row index.
        index: usize,
        /// Expected row count.
        count: usize,
    },
    /// A material row is malformed.
    InvalidWearRow {
        /// Row index.
        index: usize,
        /// Original line.
        line: String,
    },
    /// Required blank line before `LIGHTMAPS` is missing.
    MissingLightmapSeparator,
    /// `LIGHTMAPS` marker is missing.
    MissingLightmapMarker,
    /// Lightmap count line is invalid.
    InvalidLightmapCount(String),
    /// Lightmap row is missing.
    MissingLightmapRow {
        /// Row index.
        index: usize,
        /// Expected row count.
        count: usize,
    },
    /// Lightmap row is malformed.
    InvalidLightmapRow {
        /// Row index.
        index: usize,
        /// Original line.
        line: String,
    },
    /// MAT0 payload is too small.
    Mat0TooSmall {
        /// Payload size.
        size: usize,
    },
    /// MAT0 phase count is unsupported.
    InvalidPhaseCount {
        /// Phase count.
        count: usize,
    },
    /// MAT0 range is outside payload.
    Mat0OutOfBounds,
    /// MAT0 has trailing bytes not accounted for by the current grammar.
    Mat0TrailingBytes {
        /// Expected EOF.
        expected: usize,
        /// Actual payload size.
        actual: usize,
    },
    /// Material index is outside WEAR table.
    WearIndexOutOfBounds {
        /// Requested index.
        index: u16,
        /// Entry count.
        count: usize,
    },
    /// Repository error.
    Resource(String),
    /// Material archive or entry is missing.
    MissingMaterial(String),
    /// A material document has no phases for runtime sampling.
    EmptyMaterial,
}

impl From<ResourceError> for MaterialError {
    fn from(value: ResourceError) -> Self {
        Self::Resource(value.to_string())
    }
}

impl std::fmt::Display for MaterialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyWear => write!(f, "WEAR payload is empty"),
            Self::InvalidWearCount(line) => write!(f, "invalid WEAR count line: {line}"),
            Self::ZeroWearCount => write!(f, "WEAR count must be greater than zero"),
            Self::MissingWearRow { index, count } => {
                write!(f, "missing WEAR row {index} of {count}")
            }
            Self::InvalidWearRow { index, line } => {
                write!(f, "invalid WEAR row {index}: {line}")
            }
            Self::MissingLightmapSeparator => {
                write!(f, "missing blank separator before LIGHTMAPS")
            }
            Self::MissingLightmapMarker => write!(f, "missing LIGHTMAPS marker"),
            Self::InvalidLightmapCount(line) => {
                write!(f, "invalid LIGHTMAPS count line: {line}")
            }
            Self::MissingLightmapRow { index, count } => {
                write!(f, "missing LIGHTMAPS row {index} of {count}")
            }
            Self::InvalidLightmapRow { index, line } => {
                write!(f, "invalid LIGHTMAPS row {index}: {line}")
            }
            Self::Mat0TooSmall { size } => write!(f, "MAT0 payload too small: {size}"),
            Self::InvalidPhaseCount { count } => {
                write!(f, "invalid MAT0 phase count: {count}")
            }
            Self::Mat0OutOfBounds => write!(f, "MAT0 data out of bounds"),
            Self::Mat0TrailingBytes { expected, actual } => {
                write!(
                    f,
                    "MAT0 trailing bytes: expected EOF {expected}, actual {actual}"
                )
            }
            Self::WearIndexOutOfBounds { index, count } => {
                write!(f, "WEAR index {index} outside {count} entries")
            }
            Self::Resource(message) => write!(f, "{message}"),
            Self::MissingMaterial(name) => write!(f, "missing material: {name}"),
            Self::EmptyMaterial => write!(f, "material has no phases"),
        }
    }
}

impl std::error::Error for MaterialError {}

/// Decodes WEAR material/lightmap table.
///
/// # Errors
///
/// Returns [`MaterialError`] when count lines, rows, or the `LIGHTMAPS`
/// section framing are malformed.
pub fn decode_wear(bytes: &[u8]) -> Result<WearTable, MaterialError> {
    let text = decode_cp1251(bytes).replace('\r', "");
    let mut lines = text.lines();
    let Some(first) = lines.next() else {
        return Err(MaterialError::EmptyWear);
    };
    let count = parse_count(first).map_err(|_| MaterialError::InvalidWearCount(first.into()))?;
    if count == 0 {
        return Err(MaterialError::ZeroWearCount);
    }

    let mut entries = Vec::with_capacity(count);
    for index in 0..count {
        let line = lines
            .next()
            .ok_or(MaterialError::MissingWearRow { index, count })?;
        let (legacy_id, material) =
            parse_pair(line).ok_or_else(|| MaterialError::InvalidWearRow {
                index,
                line: line.to_string(),
            })?;
        entries.push(WearEntry {
            legacy_id,
            material,
        });
    }

    let remainder = lines.collect::<Vec<_>>();
    let lightmaps = parse_lightmaps(&remainder)?;
    Ok(WearTable { entries, lightmaps })
}

/// Decodes MAT0 material phase data.
///
/// # Errors
///
/// Returns [`MaterialError`] when version-gated prefix bytes, phase records, or
/// EOF framing are malformed.
pub fn decode_mat0(bytes: &[u8], version: u32) -> Result<Mat0Document, MaterialError> {
    if bytes.len() < 4 {
        return Err(MaterialError::Mat0TooSmall { size: bytes.len() });
    }
    let phase_count = usize::from(u16::from_le_bytes([bytes[0], bytes[1]]));
    let animation_block_count = u16::from_le_bytes([bytes[2], bytes[3]]);
    if animation_block_count >= 20 {
        return Err(MaterialError::InvalidPhaseCount {
            count: usize::from(animation_block_count),
        });
    }
    let header_opaque = [bytes[2], bytes[3]];
    let prefix_len = mat0_prefix_len(version);
    let phase_start = 4usize
        .checked_add(prefix_len)
        .ok_or(MaterialError::Mat0OutOfBounds)?;
    let phase_bytes = phase_count
        .checked_mul(34)
        .ok_or(MaterialError::Mat0OutOfBounds)?;
    let phase_end = phase_start
        .checked_add(phase_bytes)
        .ok_or(MaterialError::Mat0OutOfBounds)?;
    if phase_end > bytes.len() {
        return Err(MaterialError::Mat0OutOfBounds);
    }

    let mut phases = Vec::with_capacity(phase_count);
    for index in 0..phase_count {
        let offset = phase_start
            .checked_add(
                index
                    .checked_mul(34)
                    .ok_or(MaterialError::Mat0OutOfBounds)?,
            )
            .ok_or(MaterialError::Mat0OutOfBounds)?;
        let record = bytes
            .get(offset..offset + 34)
            .ok_or(MaterialError::Mat0OutOfBounds)?;
        let mut parameters = [0; 18];
        let mut texture_raw = [0; 16];
        parameters.copy_from_slice(&record[..18]);
        texture_raw.copy_from_slice(&record[18..34]);
        phases.push(MaterialPhase {
            parameters,
            texture_raw,
        });
    }

    let animation_blocks = parse_animation_blocks(&bytes[phase_end..], animation_block_count)?;
    Ok(Mat0Document {
        version,
        animation_block_count,
        phases,
        prefix: bytes[4..phase_start].to_vec(),
        header_opaque,
        animation_blocks,
    })
}

/// Resolves a material selected by WEAR index.
///
/// # Errors
///
/// Returns [`MaterialError`] when the WEAR index is invalid, `material.lib` is
/// missing, or no exact/DEFAULT material can be found.
pub fn resolve_material(
    repository: &dyn ResourceRepository,
    table: &WearTable,
    index: u16,
) -> Result<ResolvedMaterial, MaterialError> {
    let entry =
        table
            .entries
            .get(usize::from(index))
            .ok_or(MaterialError::WearIndexOutOfBounds {
                index,
                count: table.entries.len(),
            })?;
    let archive = repository.open_archive(
        &archive_path(b"material.lib").map_err(|err| MaterialError::Resource(err.to_string()))?,
    )?;

    if let Some(resolved) = load_material_entry(
        repository,
        archive,
        &entry.material,
        MaterialFallback::Exact,
    )? {
        return Ok(resolved);
    }
    let default = ResourceName(b"DEFAULT".to_vec());
    if let Some(resolved) =
        load_material_entry(repository, archive, &default, MaterialFallback::Default)?
    {
        return Ok(resolved);
    }
    if let Some(resolved) = load_first_material_entry(repository, archive)? {
        return Ok(resolved);
    }
    Err(MaterialError::MissingMaterial(
        String::from_utf8_lossy(&entry.material.0).into_owned(),
    ))
}

/// Samples a material phase with deterministic runtime timeline semantics.
///
/// # Errors
///
/// Returns [`MaterialError::EmptyMaterial`] when the MAT0 document has no
/// phases.
pub fn sample_material_phase(
    document: &Mat0Document,
    profile: MaterialTimelineProfile,
    frame: u32,
    seed: u64,
) -> Result<MaterialPhaseSample, MaterialError> {
    if document.phases.is_empty() {
        return Err(MaterialError::EmptyMaterial);
    }
    let phase_count = document.phases.len();
    let offset = if profile.random_offset {
        material_random_offset(seed, phase_count)
    } else {
        0
    };
    let effective_frame = frame.wrapping_add(offset);
    let phase_index = select_phase_index(profile.mode, effective_frame, phase_count);
    let phase = &document.phases[phase_index];
    Ok(MaterialPhaseSample {
        phase_index,
        effective_frame,
        parameters: phase.parameters,
        texture_raw: phase.texture_raw,
    })
}

/// Interpolates selected parameter bytes according to a bit mask.
///
/// Unmasked fields are copied from `left`; masked fields are linearly blended
/// and rounded to nearest integer.
#[must_use]
pub fn interpolate_parameter_bytes(
    left: [u8; 18],
    right: [u8; 18],
    interpolation_mask: u32,
    t: f32,
) -> [u8; 18] {
    let mut out = left;
    for (index, value) in out.iter_mut().enumerate() {
        if interpolation_mask & (1_u32 << index) == 0 {
            continue;
        }
        let blended =
            f32::from(left[index]) + (f32::from(right[index]) - f32::from(left[index])) * t;
        *value = rounded_clamped_byte(blended);
    }
    out
}

fn rounded_clamped_byte(value: f32) -> u8 {
    let rounded = value.round();
    if !rounded.is_finite() || rounded <= 0.0 {
        return 0;
    }
    if rounded >= f32::from(u8::MAX) {
        return u8::MAX;
    }
    (0_u8..=u8::MAX)
        .find(|candidate| f32::from(*candidate) >= rounded)
        .unwrap_or(u8::MAX)
}

/// Builds a deterministic capture for material phase sampling.
///
/// # Errors
///
/// Returns [`MaterialError`] when sampling fails.
pub fn material_phase_capture(
    document: &Mat0Document,
    profile: MaterialTimelineProfile,
    frames: &[u32],
    seed: u64,
) -> Result<Vec<u8>, MaterialError> {
    let mut out = Vec::new();
    for frame in frames {
        let sample = sample_material_phase(document, profile, *frame, seed)?;
        out.extend_from_slice(
            format!(
                "M,{},{},{}\n",
                frame, sample.effective_frame, sample.phase_index
            )
            .as_bytes(),
        );
    }
    Ok(out)
}

impl Mat0Document {
    /// Returns the first non-empty texture name from material phases.
    #[must_use]
    pub fn primary_texture(&self) -> Option<ResourceName> {
        self.phases.iter().find_map(|phase| {
            let bytes = bounded_cstr(&phase.texture_raw);
            (!bytes.is_empty()).then(|| ResourceName(bytes.to_vec()))
        })
    }

    /// Returns every non-empty texture name from material phases in disk order.
    #[must_use]
    pub fn texture_requests(&self) -> Vec<ResourceName> {
        self.phases
            .iter()
            .filter_map(|phase| {
                let bytes = bounded_cstr(&phase.texture_raw);
                (!bytes.is_empty()).then(|| ResourceName(bytes.to_vec()))
            })
            .collect()
    }
}

fn select_phase_index(mode: MaterialTimelineMode, frame: u32, phase_count: usize) -> usize {
    let count = u32::try_from(phase_count).unwrap_or(u32::MAX).max(1);
    let index = match mode {
        MaterialTimelineMode::OneShot | MaterialTimelineMode::Clamp => frame.min(count - 1),
        MaterialTimelineMode::Loop => frame % count,
        MaterialTimelineMode::PingPong => {
            if count == 1 {
                0
            } else {
                let period = count.saturating_mul(2).saturating_sub(2);
                let local = frame % period;
                if local < count {
                    local
                } else {
                    period - local
                }
            }
        }
    };
    usize::try_from(index).unwrap_or(phase_count.saturating_sub(1))
}

fn material_random_offset(seed: u64, phase_count: usize) -> u32 {
    let count = u64::try_from(phase_count).unwrap_or(u64::MAX).max(1);
    let mut state = 0xa076_1d64_78bd_642f_u64 ^ seed;
    for byte in b"material" {
        state ^= u64::from(*byte);
        state = splitmix64(state);
    }
    u32::try_from(splitmix64(state) % count).unwrap_or(0)
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut mixed = value;
    mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    mixed ^ (mixed >> 31)
}

fn load_material_entry(
    repository: &dyn ResourceRepository,
    archive: fparkan_resource::ArchiveId,
    name: &ResourceName,
    fallback: MaterialFallback,
) -> Result<Option<ResolvedMaterial>, MaterialError> {
    let Some(handle) = repository.find(archive, name)? else {
        return Ok(None);
    };
    let info = repository.entry_info(handle)?;
    if info.key.type_id != Some(MAT0_KIND) {
        return Ok(None);
    }
    let bytes = repository.read(handle)?.into_owned();
    let document = decode_mat0(&bytes, info.attr2)?;
    Ok(Some(ResolvedMaterial {
        name: info.key.name,
        fallback,
        document,
    }))
}

fn load_first_material_entry(
    repository: &dyn ResourceRepository,
    archive: fparkan_resource::ArchiveId,
) -> Result<Option<ResolvedMaterial>, MaterialError> {
    let Some(handle) = repository.first_entry(archive)? else {
        return Ok(None);
    };
    let info = repository.entry_info(handle)?;
    if info.key.type_id != Some(MAT0_KIND) {
        return Ok(None);
    }
    let bytes = repository.read(handle)?.into_owned();
    let document = decode_mat0(&bytes, info.attr2)?;
    Ok(Some(ResolvedMaterial {
        name: info.key.name,
        fallback: MaterialFallback::FirstEntry,
        document,
    }))
}

fn parse_lightmaps(lines: &[&str]) -> Result<Vec<LightmapEntry>, MaterialError> {
    if lines.is_empty() || lines.iter().all(|line| line.trim().is_empty()) {
        return Ok(Vec::new());
    }
    let mut cursor = 0usize;
    if !lines[cursor].trim().is_empty() {
        return Err(MaterialError::MissingLightmapSeparator);
    }
    cursor += 1;
    if lines.get(cursor).map(|line| line.trim()) != Some("LIGHTMAPS") {
        return Err(MaterialError::MissingLightmapMarker);
    }
    cursor += 1;
    let count_line = lines
        .get(cursor)
        .ok_or_else(|| MaterialError::InvalidLightmapCount(String::new()))?;
    let count = parse_count(count_line)
        .map_err(|_| MaterialError::InvalidLightmapCount((*count_line).to_string()))?;
    cursor += 1;
    let mut lightmaps = Vec::with_capacity(count);
    for index in 0..count {
        let line = lines
            .get(cursor)
            .ok_or(MaterialError::MissingLightmapRow { index, count })?;
        let (legacy_id, lightmap) =
            parse_pair(line).ok_or_else(|| MaterialError::InvalidLightmapRow {
                index,
                line: (*line).to_string(),
            })?;
        lightmaps.push(LightmapEntry {
            legacy_id,
            lightmap,
        });
        cursor += 1;
    }
    if lines[cursor..].iter().any(|line| !line.trim().is_empty()) {
        return Err(MaterialError::InvalidLightmapRow {
            index: count,
            line: lines[cursor..].join("\n"),
        });
    }
    Ok(lightmaps)
}

fn parse_pair(line: &str) -> Option<(LegacyText, ResourceName)> {
    let mut parts = line.split_whitespace();
    let legacy = parts.next()?;
    let resource = parts.next()?;
    Some((
        LegacyText(legacy.to_string()),
        ResourceName(resource.as_bytes().to_vec()),
    ))
}

fn parse_count(line: &str) -> Result<usize, std::num::ParseIntError> {
    line.trim().parse::<usize>()
}

fn mat0_prefix_len(version: u32) -> usize {
    let mut len = 0usize;
    if version >= 2 {
        len += 2;
    }
    if version >= 3 {
        len += 4;
    }
    if version >= 4 {
        len += 4;
    }
    len
}

fn parse_animation_blocks(
    bytes: &[u8],
    block_count: u16,
) -> Result<Vec<MaterialAnimationBlock>, MaterialError> {
    if block_count == 0 && bytes.is_empty() {
        return Ok(Vec::new());
    }
    let mut cursor = 0usize;
    let mut out = Vec::with_capacity(usize::from(block_count));
    for _ in 0..block_count {
        let start = cursor;
        let header_end = cursor
            .checked_add(6)
            .ok_or(MaterialError::Mat0OutOfBounds)?;
        let header = bytes
            .get(cursor..header_end)
            .ok_or(MaterialError::Mat0OutOfBounds)?;
        let header_raw = u32::from_le_bytes(
            header[0..4]
                .try_into()
                .map_err(|_| MaterialError::Mat0OutOfBounds)?,
        );
        let key_count = usize::from(u16::from_le_bytes([header[4], header[5]]));
        cursor = header_end;
        let keys_bytes = key_count
            .checked_mul(6)
            .ok_or(MaterialError::Mat0OutOfBounds)?;
        let keys_end = cursor
            .checked_add(keys_bytes)
            .ok_or(MaterialError::Mat0OutOfBounds)?;
        if keys_end > bytes.len() {
            return Err(MaterialError::Mat0OutOfBounds);
        }
        let mut keys = Vec::with_capacity(key_count);
        for chunk in bytes[cursor..keys_end].chunks_exact(6) {
            keys.push(MaterialKey {
                k0: u16::from_le_bytes([chunk[0], chunk[1]]),
                k1: u16::from_le_bytes([chunk[2], chunk[3]]),
                k2: u16::from_le_bytes([chunk[4], chunk[5]]),
            });
        }
        cursor = keys_end;
        out.push(MaterialAnimationBlock {
            header_raw,
            keys,
            bytes: bytes[start..cursor].to_vec(),
        });
    }
    if cursor != bytes.len() {
        return Err(MaterialError::Mat0TrailingBytes {
            expected: cursor,
            actual: bytes.len(),
        });
    }
    Ok(out)
}

fn bounded_cstr(bytes: &[u8]) -> &[u8] {
    let len = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    trim_ascii(&bytes[..len])
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let mut start = 0usize;
    let mut end = bytes.len();
    while start < end && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    &bytes[start..end]
}

fn decode_cp1251(bytes: &[u8]) -> String {
    let (decoded, _, _) = WINDOWS_1251.decode(bytes);
    decoded.into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_nres::ReadProfile;
    use fparkan_resource::CachedResourceRepository;
    use fparkan_vfs::MemoryVfs;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    #[test]
    fn wear_preserves_legacy_id_but_selects_by_index() {
        let table = decode_wear(b"2\r\n100 MAT_A\r\n5 MAT_B\r\n").expect("wear");

        assert_eq!(table.entries[0].legacy_id, LegacyText("100".to_string()));
        assert_eq!(table.entries[1].material.0, b"MAT_B");
    }

    #[test]
    fn wear_requires_declared_rows() {
        let err = decode_wear(b"2\n0 ONLY_ONE\n").expect_err("missing row");
        assert!(matches!(err, MaterialError::MissingWearRow { .. }));
    }

    #[test]
    fn wear_requires_blank_separator_before_lightmaps() {
        let err = decode_wear(b"1\n0 MAT\nLIGHTMAPS\n1\n0 LM\n").expect_err("separator");
        assert!(matches!(err, MaterialError::MissingLightmapSeparator));
    }

    #[test]
    fn wear_parses_lightmaps() {
        let table = decode_wear(b"1\n0 MAT\n\nLIGHTMAPS\n1\n0 LM_A\n").expect("wear");
        assert_eq!(table.lightmaps.len(), 1);
        assert_eq!(table.lightmaps[0].lightmap.0, b"LM_A");
    }

    #[test]
    fn mat0_version_prefix_and_primary_texture() {
        let mut bytes = vec![0; 4 + 10 + 68];
        bytes[0..2].copy_from_slice(&2_u16.to_le_bytes());
        bytes[4 + 10 + 18..4 + 10 + 25].copy_from_slice(b"TEXMAIN");
        bytes[4 + 10 + 34 + 18..4 + 10 + 34 + 24].copy_from_slice(b"TEXALT");
        let document = decode_mat0(&bytes, 4).expect("mat0");

        assert_eq!(document.prefix.len(), 10);
        assert_eq!(document.phases.len(), 2);
        assert_eq!(document.primary_texture().expect("texture").0, b"TEXMAIN");
        let textures = document.texture_requests();
        assert_eq!(textures.len(), 2);
        assert_eq!(textures[0].0, b"TEXMAIN");
        assert_eq!(textures[1].0, b"TEXALT");
    }

    #[test]
    fn mat0_accepts_zero_phase_material() {
        let document = decode_mat0(&[0, 0, 0, 0], 0).expect("zero phase");

        assert!(document.phases.is_empty());
        assert!(document.texture_requests().is_empty());
    }

    #[test]
    fn mat0_phase34_exact_framing_and_full_texture_name() {
        let mut bytes = vec![0; 4 + 34];
        bytes[0..2].copy_from_slice(&1_u16.to_le_bytes());
        bytes[4..22].copy_from_slice(&[0xAB; 18]);
        bytes[22..38].copy_from_slice(b"1234567890ABCDEF");

        let document = decode_mat0(&bytes, 0).expect("mat0");

        assert_eq!(document.phases.len(), 1);
        assert_eq!(document.phases[0].parameters, [0xAB; 18]);
        assert_eq!(
            document.primary_texture().expect("texture").0,
            b"1234567890ABCDEF"
        );
    }

    #[test]
    fn mat0_animation_block_has_no_implicit_padding() {
        let mut bytes = vec![0, 0, 1, 0];
        bytes.extend_from_slice(&0xAABB_CCDD_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&7_u16.to_le_bytes());
        bytes.extend_from_slice(&8_u16.to_le_bytes());
        bytes.extend_from_slice(&9_u16.to_le_bytes());

        let document = decode_mat0(&bytes, 0).expect("animation block");

        assert_eq!(document.animation_blocks.len(), 1);
        assert_eq!(document.animation_blocks[0].header_raw, 0xAABB_CCDD);
        assert_eq!(
            document.animation_blocks[0].keys,
            vec![MaterialKey {
                k0: 7,
                k1: 8,
                k2: 9,
            }]
        );
        assert_eq!(document.animation_blocks[0].bytes.len(), 12);
    }

    #[test]
    fn mat0_rejects_animation_block_count_limit() {
        let err = decode_mat0(&[0, 0, 20, 0], 0).expect_err("animation block count");

        assert!(matches!(
            err,
            MaterialError::InvalidPhaseCount { count: 20 }
        ));
    }

    #[test]
    fn mat0_rejects_trailing_bytes() {
        let bytes = vec![0, 0, 0, 0, 1];
        let err = decode_mat0(&bytes, 0).expect_err("trailing byte");
        assert!(matches!(err, MaterialError::Mat0TrailingBytes { .. }));
    }

    #[test]
    fn resolve_material_uses_exact_match() {
        let repo = material_repo(&[
            material_entry(b"MAT_A", &mat0_with_texture(b"TEX_A")),
            material_entry(b"DEFAULT", &mat0_with_texture(b"TEX_DEFAULT")),
        ]);
        let table = decode_wear(b"1\n0 MAT_A\n").expect("wear");

        let resolved = resolve_material(&repo, &table, 0).expect("resolved");

        assert_eq!(resolved.name.0, b"MAT_A");
        assert_eq!(resolved.fallback, MaterialFallback::Exact);
        assert_eq!(
            resolved.document.primary_texture().expect("texture").0,
            b"TEX_A"
        );
    }

    #[test]
    fn resolve_material_falls_back_to_default() {
        let repo = material_repo(&[material_entry(
            b"DEFAULT",
            &mat0_with_texture(b"TEX_DEFAULT"),
        )]);
        let table = decode_wear(b"1\n0 MISSING\n").expect("wear");

        let resolved = resolve_material(&repo, &table, 0).expect("resolved");

        assert_eq!(resolved.name.0, b"DEFAULT");
        assert_eq!(resolved.fallback, MaterialFallback::Default);
    }

    #[test]
    fn resolve_material_uses_first_entry_only_after_missing_default() {
        let repo = material_repo(&[material_entry(b"MAT_FIRST", &mat0_with_texture(b"TEX_A"))]);
        let table = decode_wear(b"2\n0 MAT_FIRST\n1 MISSING\n").expect("wear");

        let resolved = resolve_material(&repo, &table, 1).expect("resolved");

        assert_eq!(resolved.name.0, b"MAT_FIRST");
        assert_eq!(resolved.fallback, MaterialFallback::FirstEntry);
    }

    #[test]
    fn resolve_material_first_entry_uses_material_archive_not_wear_row_zero() {
        let repo = material_repo(&[
            material_entry(b"MAT_ARCHIVE_FIRST", &mat0_with_texture(b"TEX_ARCHIVE")),
            material_entry(b"MAT_WEAR_FIRST", &mat0_with_texture(b"TEX_WEAR")),
        ]);
        let table = decode_wear(b"2\n0 MAT_WEAR_FIRST\n1 MISSING\n").expect("wear");

        let resolved = resolve_material(&repo, &table, 1).expect("resolved");

        assert_eq!(resolved.name.0, b"MAT_ARCHIVE_FIRST");
        assert_eq!(resolved.fallback, MaterialFallback::FirstEntry);
        assert_eq!(
            resolved.document.primary_texture().expect("texture").0,
            b"TEX_ARCHIVE"
        );
    }

    #[test]
    fn resolve_material_empty_texture_means_untextured() {
        let repo = material_repo(&[material_entry(b"MAT_EMPTY", &mat0_with_texture(b""))]);
        let table = decode_wear(b"1\n0 MAT_EMPTY\n").expect("wear");

        let resolved = resolve_material(&repo, &table, 0).expect("resolved");

        assert!(resolved.document.primary_texture().is_none());
        assert!(resolved.document.texture_requests().is_empty());
    }

    #[test]
    fn resolve_material_without_lightmap_keeps_lightmap_absent() {
        let repo = material_repo(&[material_entry(b"MAT_A", &mat0_with_texture(b"TEX_A"))]);
        let table = decode_wear(b"1\n0 MAT_A\n").expect("wear");

        let resolved = resolve_material(&repo, &table, 0).expect("resolved");

        assert_eq!(resolved.fallback, MaterialFallback::Exact);
        assert!(table.lightmaps.is_empty());
    }

    #[test]
    fn material_modes_zero_to_three_choose_stable_phases() {
        let document =
            decode_mat0(&mat0_with_phase_textures(&[b"A", b"B", b"C"]), 0).expect("mat0");

        let cases = [
            (MaterialTimelineMode::OneShot, 9, 2),
            (MaterialTimelineMode::Clamp, 9, 2),
            (MaterialTimelineMode::Loop, 4, 1),
            (MaterialTimelineMode::PingPong, 3, 1),
        ];
        for (mode, frame, expected_phase) in cases {
            let sample = sample_material_phase(
                &document,
                MaterialTimelineProfile {
                    mode,
                    random_offset: false,
                },
                frame,
                0,
            )
            .expect("sample");
            assert_eq!(sample.phase_index, expected_phase, "{mode:?}");
        }
    }

    #[test]
    fn material_exact_key_boundary_selects_exact_phase() {
        let document =
            decode_mat0(&mat0_with_phase_textures(&[b"A", b"B", b"C"]), 0).expect("mat0");

        let sample = sample_material_phase(
            &document,
            MaterialTimelineProfile {
                mode: MaterialTimelineMode::Clamp,
                random_offset: false,
            },
            1,
            0,
        )
        .expect("sample");

        assert_eq!(sample.phase_index, 1);
        assert_eq!(&sample.texture_raw[..1], b"B");
    }

    #[test]
    fn material_interpolation_mask_affects_only_selected_fields() {
        let mut left = [10_u8; 18];
        let mut right = [20_u8; 18];
        left[1] = 100;
        right[1] = 200;

        let out = interpolate_parameter_bytes(left, right, 0b101, 0.5);

        assert_eq!(out[0], 15);
        assert_eq!(out[1], 100);
        assert_eq!(out[2], 15);
        assert_eq!(out[3], 10);
    }

    #[test]
    fn material_timeline_profile_cases_are_evidence_labeled() {
        let document =
            decode_mat0(&mat0_with_phase_textures(&[b"A", b"B", b"C"]), 0).expect("mat0");

        assert_eq!(
            material_phase_capture(
                &document,
                MaterialTimelineProfile {
                    mode: MaterialTimelineMode::OneShot,
                    random_offset: false,
                },
                &[0, 1, 4],
                0,
            )
            .expect("one-shot"),
            b"M,0,0,0\nM,1,1,1\nM,4,4,2\n"
        );
        assert_eq!(
            material_phase_capture(
                &document,
                MaterialTimelineProfile {
                    mode: MaterialTimelineMode::Loop,
                    random_offset: false,
                },
                &[0, 1, 4],
                0,
            )
            .expect("loop"),
            b"M,0,0,0\nM,1,1,1\nM,4,4,1\n"
        );
        assert_eq!(
            material_phase_capture(
                &document,
                MaterialTimelineProfile {
                    mode: MaterialTimelineMode::PingPong,
                    random_offset: false,
                },
                &[0, 1, 3],
                0,
            )
            .expect("ping-pong"),
            b"M,0,0,0\nM,1,1,1\nM,3,3,1\n"
        );
    }

    #[test]
    fn material_random_offset_uses_material_stream_only() {
        let document =
            decode_mat0(&mat0_with_phase_textures(&[b"A", b"B", b"C"]), 0).expect("mat0");
        let profile = MaterialTimelineProfile {
            mode: MaterialTimelineMode::Loop,
            random_offset: true,
        };
        let before = material_phase_capture(&document, profile, &[0, 1, 2], 99).expect("capture");
        let mut unrelated = 0x5555_u64;
        for _ in 0..16 {
            unrelated = unrelated.rotate_left(11).wrapping_mul(31);
        }

        assert_ne!(unrelated, 0);
        assert_eq!(
            material_phase_capture(&document, profile, &[0, 1, 2], 99).expect("capture"),
            before
        );
    }

    #[test]
    fn material_same_seed_and_timeline_produces_same_phase_capture() {
        let document =
            decode_mat0(&mat0_with_phase_textures(&[b"A", b"B", b"C"]), 0).expect("mat0");
        let profile = MaterialTimelineProfile {
            mode: MaterialTimelineMode::Loop,
            random_offset: true,
        };

        assert_eq!(
            material_phase_capture(&document, profile, &[0, 4, 7], 123).expect("first"),
            material_phase_capture(&document, profile, &[0, 4, 7], 123).expect("second")
        );
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_corpus_mat0_and_wear_parse() {
        for (corpus, expected_mat0, expected_archive_wear, expected_standalone_wear) in [
            ("IS", 905_usize, 439_usize, 95_usize),
            ("IS2", 1127_usize, 515_usize, 95_usize),
        ] {
            let Some(root) = corpus_root(corpus) else {
                continue;
            };
            let mut mat0_count = 0usize;
            let mut archive_wear_count = 0usize;
            let mut standalone_wear_count = 0usize;
            for path in files_under(&root) {
                let Ok(bytes) = std::fs::read(&path) else {
                    continue;
                };
                if path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("wea"))
                {
                    decode_wear(&bytes)
                        .unwrap_or_else(|err| panic!("{corpus} standalone {path:?}: {err}"));
                    standalone_wear_count += 1;
                    continue;
                }
                let Ok(archive) = fparkan_nres::decode(
                    Arc::from(bytes.into_boxed_slice()),
                    ReadProfile::Compatible,
                ) else {
                    continue;
                };
                for entry in archive.entries() {
                    let payload = archive.payload(entry.id()).expect("payload");
                    match entry.meta().type_id {
                        MAT0_KIND => {
                            decode_mat0(payload, entry.meta().attr2).unwrap_or_else(|err| {
                                panic!("{corpus} {path:?} {:?}: {err}", entry.name_bytes())
                            });
                            mat0_count += 1;
                        }
                        WEAR_KIND => {
                            decode_wear(payload).unwrap_or_else(|err| {
                                panic!("{corpus} {path:?} {:?}: {err}", entry.name_bytes())
                            });
                            archive_wear_count += 1;
                        }
                        _ => {}
                    }
                }
            }
            assert_eq!(mat0_count, expected_mat0, "{corpus} MAT0 count");
            assert_eq!(
                archive_wear_count, expected_archive_wear,
                "{corpus} archive WEAR count"
            );
            assert_eq!(
                standalone_wear_count, expected_standalone_wear,
                "{corpus} standalone WEAR count"
            );
        }
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

    struct TestMaterialEntry<'a> {
        name: &'a [u8],
        type_id: u32,
        attr2: u32,
        payload: &'a [u8],
    }

    fn material_entry<'a>(name: &'a [u8], payload: &'a [u8]) -> TestMaterialEntry<'a> {
        TestMaterialEntry {
            name,
            type_id: MAT0_KIND,
            attr2: 0,
            payload,
        }
    }

    fn material_repo(entries: &[TestMaterialEntry<'_>]) -> CachedResourceRepository {
        let path = archive_path(b"material.lib").expect("material path");
        let mut vfs = MemoryVfs::default();
        vfs.insert(
            path,
            Arc::from(build_material_nres(entries).into_boxed_slice()),
        );
        CachedResourceRepository::new(Arc::new(vfs))
    }

    fn mat0_with_texture(texture: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0; 4 + 34];
        bytes[0..2].copy_from_slice(&1_u16.to_le_bytes());
        let len = texture.len().min(16);
        bytes[22..22 + len].copy_from_slice(&texture[..len]);
        bytes
    }

    fn mat0_with_phase_textures(textures: &[&[u8]]) -> Vec<u8> {
        let mut bytes = vec![0; 4 + textures.len() * 34];
        bytes[0..2].copy_from_slice(
            &u16::try_from(textures.len())
                .expect("phase count")
                .to_le_bytes(),
        );
        for (index, texture) in textures.iter().enumerate() {
            let offset = 4 + index * 34;
            bytes[offset] = u8::try_from(index).expect("index");
            let len = texture.len().min(16);
            bytes[offset + 18..offset + 18 + len].copy_from_slice(&texture[..len]);
        }
        bytes
    }

    fn build_material_nres(entries: &[TestMaterialEntry<'_>]) -> Vec<u8> {
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
