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

impl MaterialPhase {
    /// Decodes the native floating-point material coefficients.
    #[must_use]
    pub fn coefficients(&self) -> MaterialCoefficients {
        let normalized = |index: usize| f32::from(self.parameters[index]) / 255.0;
        MaterialCoefficients {
            additive_rgb: [normalized(0), normalized(1), normalized(2)],
            opacity: f32::from(self.parameters[3]) * 0.01,
            directional_rgb: [normalized(4), normalized(5), normalized(6)],
            directional_extra: normalized(7),
            specular_rgb: [normalized(8), normalized(9), normalized(10)],
            specular_extra: normalized(11),
            extra_rgb: [normalized(12), normalized(13), normalized(14)],
            extra_alpha: normalized(15),
            power: self.parameters[16],
            page_index: i8::from_ne_bytes([self.parameters[17]]),
        }
    }
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

/// Native MAT0 animation mode encoded by `header_raw & 7`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MaterialAnimationMode {
    /// Repeat the timeline from its first key.
    Loop,
    /// Repeat the timeline in alternating forward and reverse directions.
    PingPong,
    /// Hold the final key after the timeline duration.
    Clamp,
    /// Choose a fresh time from the caller-provided random value per query.
    RandomPerQuery,
    /// Preserve an unknown native mode without making the sampler panic.
    Unknown(u8),
}

impl MaterialAnimationMode {
    /// Decodes the low three mode bits from a native block header.
    #[must_use]
    pub const fn from_header(header_raw: u32) -> Self {
        match (header_raw & 7) as u8 {
            0 => Self::Loop,
            1 => Self::PingPong,
            2 => Self::Clamp,
            3 => Self::RandomPerQuery,
            other => Self::Unknown(other),
        }
    }
}

impl MaterialAnimationBlock {
    /// Returns the native timeline mode from this block's header.
    #[must_use]
    pub const fn mode(&self) -> MaterialAnimationMode {
        MaterialAnimationMode::from_header(self.header_raw)
    }

    /// Returns the native interpolation bit mask from `header_raw >> 3`.
    #[must_use]
    pub const fn interpolation_mask(&self) -> u32 {
        self.header_raw >> 3
    }

    /// Returns the final key end time in milliseconds, if a key exists.
    #[must_use]
    pub fn duration_ms(&self) -> Option<u32> {
        self.keys.last().map(|key| u32::from(key.k1))
    }
}

/// Coefficients produced by the native MAT0 phase loader.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaterialCoefficients {
    /// Parameters 0..2, normalized additive RGB.
    pub additive_rgb: [f32; 3],
    /// Parameter 3, stored as a percentage (`raw * 0.01`).
    pub opacity: f32,
    /// Parameters 4..6, normalized directional RGB.
    pub directional_rgb: [f32; 3],
    /// Parameter 7, normalized and held when the animation mask is bit 2.
    pub directional_extra: f32,
    /// Parameters 8..10, normalized specular RGB.
    pub specular_rgb: [f32; 3],
    /// Parameter 11, normalized and held when the animation mask is bit 4.
    pub specular_extra: f32,
    /// Parameters 12..14, normalized extra RGB.
    pub extra_rgb: [f32; 3],
    /// Parameter 15, normalized and held when the animation mask is bit 8.
    pub extra_alpha: f32,
    /// Parameter 16, an integer material power value.
    pub power: u8,
    /// Parameter 17, a signed TEXM page selector.
    pub page_index: i8,
}

/// Sampled native material phase.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaterialPhaseSample {
    /// Selected phase index.
    pub phase_index: usize,
    /// Native floating-point coefficients after optional interpolation.
    pub coefficients: MaterialCoefficients,
    /// Texture name bytes from the selected phase; never interpolated.
    pub texture_raw: [u8; 16],
}

/// Resolved material.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedMaterial {
    /// Resolved material name.
    pub name: ResourceName,
    /// Fallback path.
    pub fallback: MaterialFallback,
    /// Raw NRes `attr1`, retained for renderer blend/category selection.
    pub attr1: u32,
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
    /// A normalized MAT0 sample phase is not finite.
    InvalidNormalizedPhase,
    /// An animation block has no positive duration for a multi-key timeline.
    InvalidAnimationDuration {
        /// Selected animation block.
        block_index: usize,
    },
    /// An animation key points outside the decoded phase table.
    InvalidAnimationPhaseIndex {
        /// Selected animation block.
        block_index: usize,
        /// Key index within the block.
        key_index: usize,
        /// Raw phase index from the key.
        phase_index: u16,
        /// Number of decoded phases.
        phase_count: usize,
    },
    /// Animation key end times are not strictly increasing.
    InvalidAnimationKeyTimes {
        /// Selected animation block.
        block_index: usize,
        /// Key index whose end time is invalid.
        key_index: usize,
    },
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
            Self::InvalidNormalizedPhase => {
                write!(f, "normalized MAT0 phase must be finite")
            }
            Self::InvalidAnimationDuration { block_index } => {
                write!(f, "MAT0 animation block {block_index} has zero duration")
            }
            Self::InvalidAnimationPhaseIndex {
                block_index,
                key_index,
                phase_index,
                phase_count,
            } => write!(
                f,
                "MAT0 animation block {block_index} key {key_index} phase {phase_index} outside {phase_count} phases"
            ),
            Self::InvalidAnimationKeyTimes {
                block_index,
                key_index,
            } => write!(
                f,
                "MAT0 animation block {block_index} key {key_index} has non-increasing end time"
            ),
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

    let available_lines = lines.clone().count();
    let mut entries = Vec::with_capacity(count.min(available_lines));
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

/// Samples one native MAT0 animation block.
///
/// `animation_block_index` selects a block from the already-resolved material
/// table. The packed WEAR handle (table and row) is resolved by the caller;
/// its high half is not an animation-block selector. An out-of-range block
/// uses block zero, matching `GetPhase`. `clock_ms` is the paused-compensated
/// `timeGetTime` value and `wear_row_start_ms` is the start timestamp captured
/// for the owning WEA row; the sampler subtracts the latter with wrapping
/// `u32` arithmetic. For mode [`MaterialAnimationMode::RandomPerQuery`],
/// `random_value` is the caller's fresh native-style random value for this
/// query. The sampler does not own an RNG.
///
/// # Errors
///
/// Returns an error for an empty material, a malformed multi-key duration,
/// non-increasing key end times, or a key that references no phase.
pub fn sample_material_phase(
    document: &Mat0Document,
    animation_block_index: usize,
    clock_ms: u32,
    wear_row_start_ms: u32,
    random_value: u32,
) -> Result<MaterialPhaseSample, MaterialError> {
    let Some(first_phase) = document.phases.first() else {
        return Err(MaterialError::EmptyMaterial);
    };

    let Some((selected_block_index, block)) = document
        .animation_blocks
        .get(animation_block_index)
        .map(|block| (animation_block_index, block))
        .or_else(|| document.animation_blocks.first().map(|block| (0, block)))
    else {
        return Ok(sample_static_phase(0, first_phase));
    };

    if block.keys.is_empty() {
        return Ok(sample_static_phase(0, first_phase));
    }

    let keys = &block.keys;
    let resolve_phase = |key_index: usize, key: &MaterialKey| {
        let phase_index = usize::from(key.k0);
        document
            .phases
            .get(phase_index)
            .map(|phase| (phase_index, phase))
            .ok_or(MaterialError::InvalidAnimationPhaseIndex {
                block_index: selected_block_index,
                key_index,
                phase_index: key.k0,
                phase_count: document.phases.len(),
            })
    };

    if keys.len() == 1 {
        let (phase_index, phase) = resolve_phase(0, &keys[0])?;
        return Ok(sample_static_phase(phase_index, phase));
    }

    let duration_ms = u32::from(keys[keys.len() - 1].k1);
    if duration_ms == 0 {
        return Err(MaterialError::InvalidAnimationDuration {
            block_index: selected_block_index,
        });
    }
    validate_key_times(keys, selected_block_index)?;
    for (key_index, key) in keys.iter().enumerate() {
        resolve_phase(key_index, key)?;
    }

    let elapsed_ms = clock_ms.wrapping_sub(wear_row_start_ms);
    let mode = block.mode();
    let (local_time_ms, forced_last_key) = match mode {
        MaterialAnimationMode::Loop | MaterialAnimationMode::Unknown(_) => {
            (elapsed_ms % duration_ms, false)
        }
        MaterialAnimationMode::PingPong => {
            let cycle = elapsed_ms / duration_ms;
            let remainder = elapsed_ms % duration_ms;
            let local_time = if cycle & 1 == 0 {
                remainder
            } else {
                duration_ms - remainder
            };
            (local_time, false)
        }
        MaterialAnimationMode::Clamp => {
            if elapsed_ms >= duration_ms {
                // Native clamp selects the last key and leaves the local
                // remainder at zero before entering the common interpolation
                // path, preserving its unsigned endpoint arithmetic.
                (0, true)
            } else {
                (elapsed_ms, false)
            }
        }
        MaterialAnimationMode::RandomPerQuery => (random_value % duration_ms, false),
    };

    let (key_index, previous_end_ms) = if forced_last_key {
        (keys.len() - 1, u32::from(keys[keys.len() - 2].k1))
    } else {
        select_key(keys, local_time_ms)
    };
    let key = &keys[key_index];
    let next_key_index = (key_index + 1) % keys.len();
    let next_key = &keys[next_key_index];
    let (phase_index, phase) = resolve_phase(key_index, key)?;
    let (_, next_phase) = resolve_phase(next_key_index, next_key)?;
    let span_ms = u32::from(key.k1).wrapping_sub(previous_end_ms);
    if span_ms == 0 {
        return Err(MaterialError::InvalidAnimationKeyTimes {
            block_index: selected_block_index,
            key_index,
        });
    }
    // The original routine subtracts unsigned dwords before converting both
    // values to floating point. Clamp's post-duration endpoint intentionally
    // retains this wraparound behavior.
    let fraction = local_time_ms.wrapping_sub(previous_end_ms) as f32 / span_ms as f32;
    let coefficients =
        interpolate_coefficients(phase, next_phase, block.interpolation_mask(), fraction);
    Ok(MaterialPhaseSample {
        phase_index,
        coefficients,
        texture_raw: phase.texture_raw,
    })
}

/// Samples a MAT0 animation block from the normalized phase used by native
/// material effects.
///
/// Unlike [`sample_material_phase`], this path does not interpret the block's
/// animation mode or derive a local time from a clock. It multiplies the
/// normalized phase by the final key end time, truncates to an integer tick,
/// searches key intervals from the end, and interpolates the selected phase
/// with the following phase using the block's native interpolation mask.
/// Finite phases outside `0..=1` use the native fallback phase `0.5`;
/// non-finite inputs are rejected. As with [`sample_material_phase`], a
/// missing block index falls back to block zero and an unanimated document
/// samples its first phase.
///
/// # Errors
///
/// Returns an error for a non-finite phase, an empty material, a malformed
/// multi-key duration, non-increasing key end times, or a key that references
/// no phase.
pub fn sample_material_normalized_phase(
    document: &Mat0Document,
    animation_block_index: usize,
    normalized_phase: f32,
) -> Result<MaterialPhaseSample, MaterialError> {
    if !normalized_phase.is_finite() {
        return Err(MaterialError::InvalidNormalizedPhase);
    }

    let normalized_phase = if (0.0..=1.0).contains(&normalized_phase) {
        normalized_phase
    } else {
        0.5
    };

    let Some(first_phase) = document.phases.first() else {
        return Err(MaterialError::EmptyMaterial);
    };

    let Some((selected_block_index, block)) = document
        .animation_blocks
        .get(animation_block_index)
        .map(|block| (animation_block_index, block))
        .or_else(|| document.animation_blocks.first().map(|block| (0, block)))
    else {
        return Ok(sample_static_phase(0, first_phase));
    };

    if block.keys.is_empty() {
        return Ok(sample_static_phase(0, first_phase));
    }

    let keys = &block.keys;
    let resolve_phase = |key_index: usize, key: &MaterialKey| {
        let phase_index = usize::from(key.k0);
        document
            .phases
            .get(phase_index)
            .map(|phase| (phase_index, phase))
            .ok_or(MaterialError::InvalidAnimationPhaseIndex {
                block_index: selected_block_index,
                key_index,
                phase_index: key.k0,
                phase_count: document.phases.len(),
            })
    };

    if keys.len() == 1 {
        let (phase_index, phase) = resolve_phase(0, &keys[0])?;
        return Ok(sample_static_phase(phase_index, phase));
    }

    let duration = u32::from(keys[keys.len() - 1].k1);
    if duration == 0 {
        return Err(MaterialError::InvalidAnimationDuration {
            block_index: selected_block_index,
        });
    }
    validate_key_times(keys, selected_block_index)?;
    for (key_index, key) in keys.iter().enumerate() {
        resolve_phase(key_index, key)?;
    }

    // f64 preserves the exact u16-duration by f32-phase product, matching the
    // native x87 extended product before truncation toward zero.
    let tick = ((duration as f64) * f64::from(normalized_phase)).trunc() as u32;
    let (key_index, previous_end) = select_key_backwards(keys, tick);
    let key = &keys[key_index];
    let next_key_index = (key_index + 1) % keys.len();
    let next_key = &keys[next_key_index];
    let (phase_index, phase) = resolve_phase(key_index, key)?;
    let (_, next_phase) = resolve_phase(next_key_index, next_key)?;
    let span = u32::from(key.k1) - previous_end;
    if span == 0 {
        return Err(MaterialError::InvalidAnimationKeyTimes {
            block_index: selected_block_index,
            key_index,
        });
    }
    // Keep the native endpoint behavior: phase 1 produces `tick == duration`,
    // falls back to key zero, and may extrapolate beyond its first interval.
    let fraction = tick.wrapping_sub(previous_end) as f32 / span as f32;
    let coefficients =
        interpolate_coefficients(phase, next_phase, block.interpolation_mask(), fraction);
    Ok(MaterialPhaseSample {
        phase_index,
        coefficients,
        texture_raw: phase.texture_raw,
    })
}

fn sample_static_phase(phase_index: usize, phase: &MaterialPhase) -> MaterialPhaseSample {
    MaterialPhaseSample {
        phase_index,
        coefficients: phase.coefficients(),
        texture_raw: phase.texture_raw,
    }
}

fn validate_key_times(keys: &[MaterialKey], block_index: usize) -> Result<(), MaterialError> {
    let mut previous_end = 0_u32;
    for (key_index, key) in keys.iter().enumerate() {
        let end = u32::from(key.k1);
        if end <= previous_end {
            return Err(MaterialError::InvalidAnimationKeyTimes {
                block_index,
                key_index,
            });
        }
        previous_end = end;
    }
    Ok(())
}

fn select_key(keys: &[MaterialKey], local_time_ms: u32) -> (usize, u32) {
    let mut previous_end = 0_u32;
    for (key_index, key) in keys.iter().enumerate() {
        let end = u32::from(key.k1);
        if local_time_ms < end {
            return (key_index, previous_end);
        }
        previous_end = end;
    }
    // The native backward search falls through to key zero when the local
    // value is exactly the duration (the ping-pong turnaround endpoint).
    if local_time_ms >= u32::from(keys[keys.len() - 1].k1) {
        (0, 0)
    } else {
        (keys.len() - 1, previous_end)
    }
}

fn select_key_backwards(keys: &[MaterialKey], tick: u32) -> (usize, u32) {
    let mut key_index = keys.len() - 1;
    while key_index > 0 {
        let start = u32::from(keys[key_index - 1].k1);
        let end = u32::from(keys[key_index].k1);
        if start <= tick && tick < end {
            return (key_index, start);
        }
        key_index -= 1;
    }
    (0, 0)
}

fn interpolate_coefficients(
    left: &MaterialPhase,
    right: &MaterialPhase,
    mask: u32,
    fraction: f32,
) -> MaterialCoefficients {
    // World3D 0x3030 receives source and destination wrappers one dword before
    // each D3DMATERIAL7 body. Its +4/+14/+24/+34 offsets therefore select
    // parameters 4..6, 0..2, 8..10, and 12..14; +0x20 selects parameter 3.
    // The other alpha/extra fields, power, and page remain from the left phase.
    let left = left.coefficients();
    let right = right.coefficients();
    let mut output = left;
    if mask & 1 != 0 {
        output.additive_rgb = lerp3(left.additive_rgb, right.additive_rgb, fraction);
    }
    if mask & 2 != 0 {
        output.directional_rgb = lerp3(left.directional_rgb, right.directional_rgb, fraction);
    }
    if mask & 4 != 0 {
        output.specular_rgb = lerp3(left.specular_rgb, right.specular_rgb, fraction);
    }
    if mask & 8 != 0 {
        output.extra_rgb = lerp3(left.extra_rgb, right.extra_rgb, fraction);
    }
    if mask & 16 != 0 {
        output.opacity = lerp(left.opacity, right.opacity, fraction);
    }
    output
}

fn lerp(left: f32, right: f32, fraction: f32) -> f32 {
    left + (right - left) * fraction
}

fn lerp3(left: [f32; 3], right: [f32; 3], fraction: f32) -> [f32; 3] {
    [
        lerp(left[0], right[0], fraction),
        lerp(left[1], right[1], fraction),
        lerp(left[2], right[2], fraction),
    ]
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
        attr1: info.attr1,
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
        attr1: info.attr1,
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
    let available_lines = lines.len().saturating_sub(cursor);
    let mut lightmaps = Vec::with_capacity(count.min(available_lines));
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
    use std::collections::{BTreeMap, BTreeSet};
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
    fn wear_and_lightmap_counts_do_not_reserve_beyond_available_lines() {
        let wear = format!("{}\n", usize::MAX);
        assert!(matches!(
            decode_wear(wear.as_bytes()),
            Err(MaterialError::MissingWearRow {
                index: 0,
                count: usize::MAX
            })
        ));

        let lightmaps = format!("1\n0 MAT\n\nLIGHTMAPS\n{}\n", usize::MAX);
        assert!(matches!(
            decode_wear(lightmaps.as_bytes()),
            Err(MaterialError::MissingLightmapRow {
                index: 0,
                count: usize::MAX
            })
        ));
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
            material_entry_with_attr1(b"MAT_A", &mat0_with_texture(b"TEX_A"), 0x1234_5678),
            material_entry(b"DEFAULT", &mat0_with_texture(b"TEX_DEFAULT")),
        ]);
        let table = decode_wear(b"1\n0 MAT_A\n").expect("wear");

        let resolved = resolve_material(&repo, &table, 0).expect("resolved");

        assert_eq!(resolved.name.0, b"MAT_A");
        assert_eq!(resolved.fallback, MaterialFallback::Exact);
        assert_eq!(resolved.attr1, 0x1234_5678);
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
    fn material_animation_header_decodes_native_mode_and_mask() {
        let block = MaterialAnimationBlock {
            header_raw: (0x1f_u32 << 3) | 3,
            keys: Vec::new(),
            bytes: Vec::new(),
        };

        assert_eq!(block.mode(), MaterialAnimationMode::RandomPerQuery);
        assert_eq!(block.interpolation_mask(), 0x1f);
        assert_eq!(
            MaterialAnimationMode::from_header(4),
            MaterialAnimationMode::Unknown(4)
        );
    }

    #[test]
    fn material_phase_coefficients_decode_native_parameter_slots() {
        let mut parameters = [0_u8; 18];
        parameters.copy_from_slice(&[
            0, 64, 128, 25, 10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 9, 0xfe,
        ]);
        let phase = MaterialPhase {
            parameters,
            texture_raw: [0; 16],
        };

        let coefficients = phase.coefficients();
        assert_eq!(
            coefficients.additive_rgb,
            [0.0, 64.0 / 255.0, 128.0 / 255.0]
        );
        assert!((coefficients.opacity - 0.25).abs() < f32::EPSILON);
        assert_eq!(
            coefficients.directional_rgb,
            [10.0 / 255.0, 20.0 / 255.0, 30.0 / 255.0]
        );
        assert_eq!(coefficients.directional_extra, 40.0 / 255.0);
        assert_eq!(
            coefficients.specular_rgb,
            [50.0 / 255.0, 60.0 / 255.0, 70.0 / 255.0]
        );
        assert_eq!(coefficients.specular_extra, 80.0 / 255.0);
        assert_eq!(
            coefficients.extra_rgb,
            [90.0 / 255.0, 100.0 / 255.0, 110.0 / 255.0]
        );
        assert_eq!(coefficients.extra_alpha, 120.0 / 255.0);
        assert_eq!(coefficients.power, 9);
        assert_eq!(coefficients.page_index, -2);
    }

    #[test]
    fn material_loop_interpolates_native_groups_and_selects_current_texture() {
        let left = phase_with_parameters(
            [
                0, 10, 20, 25, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 130, 140, 7, 1,
            ],
            b"LEFT",
        );
        let right = phase_with_parameters(
            [
                255, 110, 220, 75, 230, 240, 250, 160, 170, 180, 190, 200, 210, 220, 230, 240, 9, 2,
            ],
            b"RIGHT",
        );
        let document = animation_document(
            0x1f_u32 << 3,
            vec![left, right],
            vec![
                MaterialKey {
                    k0: 0,
                    k1: 100,
                    k2: 0,
                },
                MaterialKey {
                    k0: 1,
                    k1: 200,
                    k2: 0,
                },
            ],
        );

        let sample = sample_material_phase(&document, 0, 150, 0, 0).expect("sample");

        assert_eq!(sample.phase_index, 1);
        assert_eq!(&sample.texture_raw[..5], b"RIGHT");
        assert!((sample.coefficients.additive_rgb[0] - (127.5 / 255.0)).abs() < 1e-6);
        assert!((sample.coefficients.opacity - 0.5).abs() < 1e-6);
        assert!((sample.coefficients.directional_rgb[1] - (140.0 / 255.0)).abs() < 1e-6);
        assert_eq!(sample.coefficients.directional_extra, 160.0 / 255.0);
        assert!((sample.coefficients.specular_rgb[2] - (140.0 / 255.0)).abs() < 1e-6);
        assert_eq!(sample.coefficients.specular_extra, 200.0 / 255.0);
        assert!((sample.coefficients.extra_rgb[0] - (160.0 / 255.0)).abs() < 1e-6);
        assert_eq!(sample.coefficients.extra_alpha, 240.0 / 255.0);
        assert_eq!(sample.coefficients.power, 9);
        assert_eq!(sample.coefficients.page_index, 2);

        let boundary = sample_material_phase(&document, 0, 100, 0, 0).expect("boundary");
        assert_eq!(boundary.phase_index, 1);
        assert_eq!(&boundary.texture_raw[..5], b"RIGHT");
        assert_eq!(boundary.coefficients.additive_rgb[0], 1.0);

        let wrapped = sample_material_phase(&document, 0, 200, 0, 0).expect("wrapped");
        assert_eq!(wrapped.phase_index, 0);
        assert_eq!(&wrapped.texture_raw[..4], b"LEFT");
    }

    #[test]
    fn material_animation_interpolation_masks_match_native_rebased_offsets() {
        let left = phase_with_parameters(
            [
                10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 130, 140, 150, 160, 170, 180,
            ],
            b"LEFT",
        );
        let right = phase_with_parameters(
            [
                70, 80, 90, 100, 110, 120, 130, 140, 150, 160, 170, 180, 190, 200, 210, 220, 230,
                240,
            ],
            b"RIGHT",
        );
        let rgb = |first: u8| [first, first + 10, first + 20].map(|value| f32::from(value) / 255.0);
        let cases = [
            (0, rgb(10), rgb(50), rgb(90), rgb(130), 0.4),
            (1, rgb(40), rgb(50), rgb(90), rgb(130), 0.4),
            (2, rgb(10), rgb(80), rgb(90), rgb(130), 0.4),
            (4, rgb(10), rgb(50), rgb(120), rgb(130), 0.4),
            (8, rgb(10), rgb(50), rgb(90), rgb(160), 0.4),
            (16, rgb(10), rgb(50), rgb(90), rgb(130), 0.7),
            (0x1f, rgb(40), rgb(80), rgb(120), rgb(160), 0.7),
        ];

        for (mask, additive, directional, specular, extra, opacity) in cases {
            let output = interpolate_coefficients(&left, &right, mask, 0.5);
            for (actual, expected) in [
                (output.additive_rgb, additive),
                (output.directional_rgb, directional),
                (output.specular_rgb, specular),
                (output.extra_rgb, extra),
            ] {
                for channel in 0..3 {
                    assert!(
                        (actual[channel] - expected[channel]).abs() < 1e-6,
                        "mask {mask:#x}, channel {channel}: got {}, expected {}",
                        actual[channel],
                        expected[channel]
                    );
                }
            }

            assert!((output.opacity - opacity).abs() < 1e-6, "mask {mask:#x}");
            assert_eq!(output.directional_extra, 80.0 / 255.0);
            assert_eq!(output.specular_extra, 120.0 / 255.0);
            assert_eq!(output.extra_alpha, 160.0 / 255.0);
            assert_eq!(output.power, 170);
            assert_eq!(output.page_index, -76);
        }
    }

    #[test]
    fn normalized_material_phase_uses_truncated_tick_and_native_fallback_endpoint() {
        let phases = vec![
            phase_with_parameters([0; 18], b"LEFT"),
            phase_with_parameters([255; 18], b"RIGHT"),
        ];
        let keys = vec![
            MaterialKey {
                k0: 0,
                k1: 100,
                k2: 0,
            },
            MaterialKey {
                k0: 1,
                k1: 200,
                k2: 0,
            },
        ];
        // RandomPerQuery must not override the explicit normalized phase.
        let document = animation_document((0x1f_u32 << 3) | 3, phases, keys);

        let early = sample_material_normalized_phase(&document, 0, 0.25).expect("early");
        assert_eq!(early.phase_index, 0);
        assert_eq!(&early.texture_raw[..4], b"LEFT");
        assert!((early.coefficients.additive_rgb[0] - 0.5).abs() < 1e-6);

        // 200 * 0.504 = 100.8; truncation yields tick 100, at the start of key 1.
        let truncated = sample_material_normalized_phase(&document, 0, 0.504).expect("tick");
        assert_eq!(truncated.phase_index, 1);
        assert_eq!(&truncated.texture_raw[..5], b"RIGHT");
        assert_eq!(truncated.coefficients.additive_rgb[0], 1.0);

        // Native backward search falls through to key zero at the final tick.
        let endpoint = sample_material_normalized_phase(&document, 0, 1.0).expect("endpoint");
        assert_eq!(endpoint.phase_index, 0);
        assert_eq!(&endpoint.texture_raw[..4], b"LEFT");
        assert!((endpoint.coefficients.additive_rgb[0] - 2.0).abs() < 1e-6);
    }

    #[test]
    fn normalized_material_phase_preserves_precision_before_native_truncation() {
        let document = animation_document(
            1 << 3,
            vec![
                phase_with_parameters([0; 18], b"LEFT"),
                phase_with_parameters([255; 18], b"RIGHT"),
            ],
            vec![
                MaterialKey {
                    k0: 0,
                    k1: 32_765,
                    k2: 0,
                },
                MaterialKey {
                    k0: 1,
                    k1: 32_767,
                    k2: 0,
                },
            ],
        );

        // The exact product is below 32_767; f32 multiplication rounds it up.
        let sample = sample_material_normalized_phase(&document, 0, f32::from_bits(0x3f7f_ffff))
            .expect("near endpoint");
        assert_eq!(sample.phase_index, 1);
        assert_eq!(&sample.texture_raw[..5], b"RIGHT");
    }

    #[test]
    fn normalized_material_phase_uses_half_for_finite_out_of_range_values() {
        let document = animation_document(
            1 << 3,
            vec![
                phase_with_parameters([0; 18], b"LEFT"),
                phase_with_parameters([255; 18], b"RIGHT"),
            ],
            vec![
                MaterialKey {
                    k0: 0,
                    k1: 100,
                    k2: 0,
                },
                MaterialKey {
                    k0: 1,
                    k1: 200,
                    k2: 0,
                },
            ],
        );

        let midpoint = sample_material_normalized_phase(&document, 0, 0.5).expect("midpoint");
        let below = sample_material_normalized_phase(&document, 0, -0.1).expect("below");
        let above = sample_material_normalized_phase(&document, 0, 1.1).expect("above");
        assert_eq!(below, midpoint);
        assert_eq!(above, midpoint);
    }

    #[test]
    fn normalized_material_phase_rejects_non_finite_values() {
        let document = animation_document(
            0,
            vec![phase_with_parameters([0; 18], b"ONLY")],
            vec![MaterialKey {
                k0: 0,
                k1: 0,
                k2: 0,
            }],
        );

        assert!(matches!(
            sample_material_normalized_phase(&document, 0, f32::NAN),
            Err(MaterialError::InvalidNormalizedPhase)
        ));
        assert!(matches!(
            sample_material_normalized_phase(&document, 0, f32::INFINITY),
            Err(MaterialError::InvalidNormalizedPhase)
        ));
    }

    #[test]
    fn material_ping_pong_reverses_local_time_and_clamp_holds_last_phase() {
        let phases = vec![
            phase_with_parameters([0; 18], b"A"),
            phase_with_parameters([255; 18], b"B"),
        ];
        let keys = vec![
            MaterialKey {
                k0: 0,
                k1: 100,
                k2: 0,
            },
            MaterialKey {
                k0: 1,
                k1: 200,
                k2: 0,
            },
        ];
        let ping_pong = animation_document((0x1_u32) | (1 << 3), phases.clone(), keys.clone());
        assert_eq!(
            sample_material_phase(&ping_pong, 0, 100, 0, 0)
                .expect("turn")
                .phase_index,
            1
        );
        assert_eq!(
            sample_material_phase(&ping_pong, 0, 150, 0, 0)
                .expect("reverse")
                .phase_index,
            1
        );
        assert_eq!(
            sample_material_phase(&ping_pong, 0, 200, 0, 0)
                .expect("restart")
                .phase_index,
            0
        );
        let turnaround = sample_material_phase(&ping_pong, 0, 200, 0, 0).expect("turnaround");
        assert!((turnaround.coefficients.additive_rgb[0] - 2.0).abs() < 1e-6);

        let clamp = animation_document((2_u32) | (1 << 3), phases, keys);
        let sample = sample_material_phase(&clamp, 0, 500, 0, 0).expect("clamp");
        assert_eq!(sample.phase_index, 1);
        assert!(sample.coefficients.additive_rgb[0] < 0.0);
    }

    #[test]
    fn material_random_mode_uses_caller_value_and_invalid_block_uses_block_zero() {
        let phases = vec![
            phase_with_parameters([0; 18], b"A"),
            phase_with_parameters([255; 18], b"B"),
        ];
        let keys = vec![
            MaterialKey {
                k0: 0,
                k1: 100,
                k2: 0,
            },
            MaterialKey {
                k0: 1,
                k1: 200,
                k2: 0,
            },
        ];
        let document = Mat0Document {
            version: 0,
            animation_block_count: 2,
            phases,
            prefix: Vec::new(),
            header_opaque: [2, 0],
            animation_blocks: vec![
                MaterialAnimationBlock {
                    header_raw: 3,
                    keys: keys.clone(),
                    bytes: Vec::new(),
                },
                MaterialAnimationBlock {
                    header_raw: 2,
                    keys,
                    bytes: Vec::new(),
                },
            ],
        };

        let sample = sample_material_phase(&document, 0, 0, 0, 150).expect("random");
        assert_eq!(sample.phase_index, 1);
        let fallback = sample_material_phase(&document, 99, 500, 0, 0).expect("fallback");
        assert_eq!(fallback.phase_index, 0);
    }

    #[test]
    fn material_static_and_single_key_sampling_uses_phase_zero_or_key_phase() {
        let phases = vec![
            phase_with_parameters([1; 18], b"A"),
            phase_with_parameters([2; 18], b"B"),
        ];
        let static_document = Mat0Document {
            version: 0,
            animation_block_count: 0,
            phases: phases.clone(),
            prefix: Vec::new(),
            header_opaque: [0, 0],
            animation_blocks: Vec::new(),
        };
        assert_eq!(
            sample_material_phase(&static_document, 0, 123, 50, 0)
                .expect("static")
                .phase_index,
            0
        );

        let single_document = animation_document(
            0,
            phases,
            vec![MaterialKey {
                k0: 1,
                k1: 0,
                k2: 0,
            }],
        );
        let sample = sample_material_phase(&single_document, 0, 123, 50, 0).expect("single");
        assert_eq!(sample.phase_index, 1);
        assert_eq!(&sample.texture_raw[..1], b"B");
    }

    #[test]
    fn material_sampling_rejects_bad_timeline_data() {
        let phase = phase_with_parameters([0; 18], b"A");
        let invalid_ref = animation_document(
            0,
            vec![phase.clone()],
            vec![
                MaterialKey {
                    k0: 0,
                    k1: 10,
                    k2: 0,
                },
                MaterialKey {
                    k0: 4,
                    k1: 20,
                    k2: 0,
                },
            ],
        );
        assert!(matches!(
            sample_material_phase(&invalid_ref, 0, 0, 0, 0),
            Err(MaterialError::InvalidAnimationPhaseIndex { .. })
        ));
        assert!(matches!(
            sample_material_normalized_phase(&invalid_ref, 0, 0.5),
            Err(MaterialError::InvalidAnimationPhaseIndex { .. })
        ));

        let invalid_times = animation_document(
            0,
            vec![phase.clone()],
            vec![
                MaterialKey {
                    k0: 0,
                    k1: 20,
                    k2: 0,
                },
                MaterialKey {
                    k0: 0,
                    k1: 10,
                    k2: 0,
                },
            ],
        );
        assert!(matches!(
            sample_material_phase(&invalid_times, 0, 0, 0, 0),
            Err(MaterialError::InvalidAnimationKeyTimes { .. })
        ));
        assert!(matches!(
            sample_material_normalized_phase(&invalid_times, 0, 0.5),
            Err(MaterialError::InvalidAnimationKeyTimes { .. })
        ));

        let zero_duration = animation_document(
            0,
            vec![phase],
            vec![
                MaterialKey {
                    k0: 0,
                    k1: 0,
                    k2: 0,
                },
                MaterialKey {
                    k0: 0,
                    k1: 0,
                    k2: 0,
                },
            ],
        );
        assert!(matches!(
            sample_material_phase(&zero_duration, 0, 0, 0, 0),
            Err(MaterialError::InvalidAnimationDuration { .. })
        ));
        assert!(matches!(
            sample_material_normalized_phase(&zero_duration, 0, 0.5),
            Err(MaterialError::InvalidAnimationDuration { .. })
        ));
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_corpus_mat0_and_wear_parse() {
        for (corpus, expected_mat0, expected_archive_wear, expected_standalone_wear) in [
            ("IS", 905_usize, 439_usize, 95_usize),
            ("IS2", 1127_usize, 515_usize, 95_usize),
        ] {
            let root = corpus_root(corpus);
            let mut mat0_count = 0usize;
            let mut archive_wear_count = 0usize;
            let mut standalone_wear_count = 0usize;
            let mut animation_modes = BTreeMap::<u8, usize>::new();
            let mut interpolation_masks = BTreeSet::<u32>::new();
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
                            let document =
                                decode_mat0(payload, entry.meta().attr2).unwrap_or_else(|err| {
                                    panic!("{corpus} {path:?} {:?}: {err}", entry.name_bytes())
                                });
                            for block in &document.animation_blocks {
                                *animation_modes
                                    .entry((block.header_raw & 7) as u8)
                                    .or_default() += 1;
                                interpolation_masks.insert(block.interpolation_mask());
                            }
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
            eprintln!(
                "{corpus} MAT0 animation inventory: modes={animation_modes:?}, masks={interpolation_masks:?}"
            );
        }
    }

    fn corpus_root(name: &str) -> PathBuf {
        let variable = match name {
            "IS" => "FPARKAN_CORPUS_PART1_ROOT",
            "IS2" => "FPARKAN_CORPUS_PART2_ROOT",
            _ => panic!("unknown licensed corpus part: {name}"),
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
        attr1: u32,
        attr2: u32,
        payload: &'a [u8],
    }

    fn material_entry<'a>(name: &'a [u8], payload: &'a [u8]) -> TestMaterialEntry<'a> {
        TestMaterialEntry {
            name,
            type_id: MAT0_KIND,
            attr1: 0,
            attr2: 0,
            payload,
        }
    }

    fn material_entry_with_attr1<'a>(
        name: &'a [u8],
        payload: &'a [u8],
        attr1: u32,
    ) -> TestMaterialEntry<'a> {
        TestMaterialEntry {
            name,
            type_id: MAT0_KIND,
            attr1,
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

    fn phase_with_parameters(parameters: [u8; 18], texture: &[u8]) -> MaterialPhase {
        let mut texture_raw = [0_u8; 16];
        let len = texture.len().min(texture_raw.len());
        texture_raw[..len].copy_from_slice(&texture[..len]);
        MaterialPhase {
            parameters,
            texture_raw,
        }
    }

    fn animation_document(
        header_raw: u32,
        phases: Vec<MaterialPhase>,
        keys: Vec<MaterialKey>,
    ) -> Mat0Document {
        Mat0Document {
            version: 0,
            animation_block_count: 1,
            phases,
            prefix: Vec::new(),
            header_opaque: [1, 0],
            animation_blocks: vec![MaterialAnimationBlock {
                header_raw,
                keys,
                bytes: Vec::new(),
            }],
        }
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
            push_u32(&mut out, entry.attr1);
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
