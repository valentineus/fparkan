#![forbid(unsafe_code)]
//! Count-driven mission format primitives.

use encoding_rs::WINDOWS_1251;
use fparkan_binary::{checked_count_bytes, read_lp_bytes, Cursor, DecodeError};
use std::sync::Arc;

const FORMAT_VERSION: u32 = 1;
const CLAN_SECTION_VERSION: u32 = 6;
const OBJECT_SECTION_VERSION: u32 = 10;
const PROPERTY_SCHEMA_VERSION: u32 = 1;
const EXTRA_SECTION_VERSION: u32 = 1;
const OBJECT_CLASS_OR_FLAGS: u32 = 0x8000_0002;
const MAX_PATHS: u32 = 16_384;
const MAX_POINTS: u32 = 1_000_000;
const MAX_CLANS: u32 = 16_384;
const MAX_RELATIONS: u32 = 65_536;
const MAX_SPATIAL_GROUPS: u32 = 65_536;
const MAX_SPATIAL_RECORDS: u32 = 1_000_000;
const MAX_OBJECTS: u32 = 1_000_000;
const MAX_PROPERTIES: u32 = 1_000_000;
const MAX_EXTRAS: u32 = 1_000_000;
const MAX_STRING_BYTES: u32 = 64 * 1024;

/// Mission document.
#[derive(Clone, Debug, PartialEq)]
pub struct MissionDocument {
    /// Top-level format version.
    pub format_version: u32,
    /// Clan section version.
    pub clan_section_version: u32,
    /// Object section version.
    pub object_section_version: u32,
    /// Extra section version.
    pub extra_section_version: u32,
    /// Version words preserved for compact compatibility checks.
    pub versions: Vec<u32>,
    /// Paths.
    pub paths: Vec<MissionPath>,
    /// Clans.
    pub clans: Vec<ClanRecord>,
    /// Placed objects.
    pub objects: Vec<PlacedObject>,
    /// Landscape path.
    pub land_path: LpString,
    /// Mission flag.
    pub mission_flag: u32,
    /// Raw mission description.
    pub description_raw: LpString,
    /// Extras.
    pub extras: Vec<ExtraRecord28>,
    /// Original bytes.
    pub raw: Arc<[u8]>,
}

/// Length-prefixed string with decoded CP1251 helper text.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LpString {
    /// Raw bytes from the file.
    pub raw: Vec<u8>,
    /// Decoded text.
    pub decoded: String,
}

/// Mission path.
#[derive(Clone, Debug, PartialEq)]
pub struct MissionPath {
    /// Path id.
    pub id: i32,
    /// Points.
    pub points: Vec<[f32; 3]>,
}

/// Clan record.
#[derive(Clone, Debug, PartialEq)]
pub struct ClanRecord {
    /// Clan name.
    pub name: LpString,
    /// Raw id, usually `-1` in checked corpora.
    pub raw_id: i32,
    /// Two-dimensional clan anchor.
    pub anchor: [f32; 2],
    /// Mode selector.
    pub mode: u32,
    /// Mode-dependent payload.
    pub body: ClanBody,
    /// Relation table.
    pub relations: Vec<ClanRelation>,
}

/// Clan mode-dependent body.
#[derive(Clone, Debug, PartialEq)]
pub enum ClanBody {
    /// Standard modes 1..=3.
    Standard {
        /// First tagged resource.
        first_resource: TaggedResource,
        /// Second tagged resource.
        second_resource: TaggedResource,
    },
    /// Mode 0 spatial body.
    Spatial {
        /// First untagged resource.
        first_resource: LpString,
        /// Spatial groups.
        spatial_groups: Vec<SpatialGroup>,
        /// Second tagged resource.
        second_resource: TaggedResource,
    },
}

/// Tagged clan resource reference.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaggedResource {
    /// Resource path.
    pub path: LpString,
    /// Raw tag.
    pub tag: i32,
}

/// Mode 0 spatial group.
#[derive(Clone, Debug, PartialEq)]
pub struct SpatialGroup {
    /// Raw spatial records, five floats each.
    pub records: Vec<[f32; 5]>,
}

/// Clan relation entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClanRelation {
    /// Other clan name.
    pub other_clan_name: LpString,
    /// Raw relation value.
    pub relation_value: i32,
}

/// Placed object.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedObject {
    /// Raw object kind.
    pub raw_kind: u32,
    /// Class/flags word.
    pub class_or_flags: u32,
    /// Resource reference.
    pub resource_name: LpString,
    /// Raw resource bytes retained for older callers.
    pub resource_raw: Vec<u8>,
    /// Raw word after resource.
    pub raw_after_resource: u32,
    /// Raw identity/clan word.
    pub identity_or_clan_raw: u32,
    /// Position.
    pub position: [f32; 3],
    /// Orientation.
    pub orientation: [f32; 3],
    /// Scale.
    pub scale: [f32; 3],
    /// Instance name.
    pub instance_name: LpString,
    /// Raw word after instance name.
    pub raw_after_name: u32,
    /// First link word.
    pub link0: i32,
    /// Second link word.
    pub link1: i32,
    /// Property schema version.
    pub property_schema_version: u32,
    /// Ordered properties.
    pub properties: Vec<OrderedProperty>,
}

/// Ordered property.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderedProperty {
    /// Raw words.
    pub raw_value: [u32; 4],
    /// Property name.
    pub name: LpString,
    /// Raw name bytes retained for older callers.
    pub name_raw: Vec<u8>,
}

/// Mission epilogue marker.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MissionEpilogue;

/// 28-byte extra record.
#[derive(Clone, Debug, PartialEq)]
pub struct ExtraRecord28 {
    /// Raw 28-byte record.
    pub raw: [u8; 28],
    /// Position.
    pub position: [f32; 3],
    /// Preserved trailing words.
    pub raw_words: [u32; 4],
}

/// TMA profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TmaProfile {
    /// Strict profile.
    Strict,
}

/// Mission error.
#[derive(Debug)]
pub enum MissionError {
    /// Decode error.
    Decode(DecodeError),
    /// Unsupported branch.
    Unsupported(&'static str),
    /// Invalid section version.
    InvalidVersion {
        /// Section name.
        section: &'static str,
        /// Expected version.
        expected: u32,
        /// Observed version.
        got: u32,
    },
    /// Unknown clan mode.
    UnknownClanMode {
        /// Clan index.
        clan: usize,
        /// Observed mode.
        mode: u32,
    },
    /// Invalid placed object flags.
    InvalidObjectFlags {
        /// Object index.
        object: usize,
        /// Observed flags.
        flags: u32,
    },
    /// Non-finite transform field.
    NonFiniteTransform {
        /// Object index.
        object: usize,
    },
}

impl From<DecodeError> for MissionError {
    fn from(value: DecodeError) -> Self {
        Self::Decode(value)
    }
}

impl std::fmt::Display for MissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Decode(source) => write!(f, "{source}"),
            Self::Unsupported(reason) => write!(f, "unsupported TMA branch: {reason}"),
            Self::InvalidVersion {
                section,
                expected,
                got,
            } => write!(
                f,
                "invalid TMA {section} version {got}, expected {expected}"
            ),
            Self::UnknownClanMode { clan, mode } => {
                write!(f, "unknown TMA clan mode {mode} at clan {clan}")
            }
            Self::InvalidObjectFlags { object, flags } => {
                write!(f, "invalid TMA object {object} flags {flags:#x}")
            }
            Self::NonFiniteTransform { object } => {
                write!(f, "TMA object {object} contains non-finite transform")
            }
        }
    }
}

impl std::error::Error for MissionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Decode(source) => Some(source),
            Self::Unsupported(_)
            | Self::InvalidVersion { .. }
            | Self::UnknownClanMode { .. }
            | Self::InvalidObjectFlags { .. }
            | Self::NonFiniteTransform { .. } => None,
        }
    }
}

/// Decodes an exact, count-driven TMA document.
///
/// # Errors
///
/// Returns [`MissionError`] when a count/length is out of bounds, a known
/// section version does not match strict expectations, a mode-dependent branch
/// is unknown, object transforms are invalid, or the cursor does not end at EOF.
pub fn decode_tma(bytes: Arc<[u8]>, profile: TmaProfile) -> Result<MissionDocument, MissionError> {
    let mut cursor = Cursor::new(&bytes);
    let format_version = cursor.read_u32_le()?;
    require_version("format", format_version, FORMAT_VERSION, profile)?;

    let paths = parse_paths(&mut cursor)?;

    let clan_section_version = cursor.read_u32_le()?;
    require_version(
        "clan section",
        clan_section_version,
        CLAN_SECTION_VERSION,
        profile,
    )?;
    let clans = parse_clans(&mut cursor)?;

    let object_section_version = cursor.read_u32_le()?;
    require_version(
        "object section",
        object_section_version,
        OBJECT_SECTION_VERSION,
        profile,
    )?;
    let objects = parse_objects(&mut cursor, profile)?;

    let land_path = read_lp_string(&mut cursor)?;
    let mission_flag = cursor.read_u32_le()?;
    let description_raw = read_lp_string(&mut cursor)?;

    let extra_section_version = cursor.read_u32_le()?;
    require_version(
        "extra section",
        extra_section_version,
        EXTRA_SECTION_VERSION,
        profile,
    )?;
    let extras = parse_extras(&mut cursor)?;
    cursor.require_eof()?;

    Ok(MissionDocument {
        format_version,
        clan_section_version,
        object_section_version,
        extra_section_version,
        versions: vec![
            format_version,
            clan_section_version,
            object_section_version,
            extra_section_version,
        ],
        paths,
        clans,
        objects,
        land_path,
        mission_flag,
        description_raw,
        extras,
        raw: bytes,
    })
}

/// Decodes only the TMA landscape path needed to load terrain before the full
/// mission document is materialized.
///
/// # Errors
///
/// Returns [`MissionError`] when any section preceding the landscape path is
/// malformed or unsupported.
pub fn decode_tma_land_path(bytes: &[u8], profile: TmaProfile) -> Result<LpString, MissionError> {
    let mut cursor = Cursor::new(bytes);
    let format_version = cursor.read_u32_le()?;
    require_version("format", format_version, FORMAT_VERSION, profile)?;
    let _paths = parse_paths(&mut cursor)?;

    let clan_section_version = cursor.read_u32_le()?;
    require_version(
        "clan section",
        clan_section_version,
        CLAN_SECTION_VERSION,
        profile,
    )?;
    let _clans = parse_clans(&mut cursor)?;

    let object_section_version = cursor.read_u32_le()?;
    require_version(
        "object section",
        object_section_version,
        OBJECT_SECTION_VERSION,
        profile,
    )?;
    let _objects = parse_objects(&mut cursor, profile)?;
    read_lp_string(&mut cursor)
}

fn require_version(
    section: &'static str,
    got: u32,
    expected: u32,
    _profile: TmaProfile,
) -> Result<(), MissionError> {
    if got == expected {
        Ok(())
    } else {
        Err(MissionError::InvalidVersion {
            section,
            expected,
            got,
        })
    }
}

fn parse_paths(cursor: &mut Cursor<'_>) -> Result<Vec<MissionPath>, MissionError> {
    let count = checked_count(cursor.read_u32_le()?, MAX_PATHS)?;
    let mut paths = Vec::with_capacity(count);
    for _ in 0..count {
        let id = cursor.read_i32_le()?;
        let point_count = cursor.read_u32_le()?;
        checked_count_bytes(u64::from(point_count), 12, cursor.remaining() as u64)?;
        let point_count = checked_count(point_count, MAX_POINTS)?;
        let mut points = Vec::with_capacity(point_count);
        for _ in 0..point_count {
            points.push(read_vec3(cursor)?);
        }
        paths.push(MissionPath { id, points });
    }
    Ok(paths)
}

fn parse_clans(cursor: &mut Cursor<'_>) -> Result<Vec<ClanRecord>, MissionError> {
    let count = checked_count(cursor.read_u32_le()?, MAX_CLANS)?;
    let mut clans = Vec::with_capacity(count);
    for clan_index in 0..count {
        let name = read_lp_string(cursor)?;
        let raw_id = cursor.read_i32_le()?;
        let anchor = [cursor.read_f32_le()?, cursor.read_f32_le()?];
        let mode = cursor.read_u32_le()?;
        let (body, relations) = match mode {
            0 => parse_spatial_clan(cursor)?,
            1..=3 => parse_standard_clan(cursor)?,
            _ => {
                return Err(MissionError::UnknownClanMode {
                    clan: clan_index,
                    mode,
                })
            }
        };
        clans.push(ClanRecord {
            name,
            raw_id,
            anchor,
            mode,
            body,
            relations,
        });
    }
    Ok(clans)
}

fn parse_standard_clan(
    cursor: &mut Cursor<'_>,
) -> Result<(ClanBody, Vec<ClanRelation>), MissionError> {
    let first_resource = parse_tagged_resource(cursor)?;
    let second_resource = parse_tagged_resource(cursor)?;
    let relations = parse_relations(cursor)?;
    Ok((
        ClanBody::Standard {
            first_resource,
            second_resource,
        },
        relations,
    ))
}

fn parse_spatial_clan(
    cursor: &mut Cursor<'_>,
) -> Result<(ClanBody, Vec<ClanRelation>), MissionError> {
    let first_resource = read_lp_string(cursor)?;
    let group_count = checked_count(cursor.read_u32_le()?, MAX_SPATIAL_GROUPS)?;
    let mut spatial_groups = Vec::with_capacity(group_count);
    for _ in 0..group_count {
        let record_count = cursor.read_u32_le()?;
        checked_count_bytes(u64::from(record_count), 20, cursor.remaining() as u64)?;
        let record_count = checked_count(record_count, MAX_SPATIAL_RECORDS)?;
        let mut records = Vec::with_capacity(record_count);
        for _ in 0..record_count {
            records.push([
                cursor.read_f32_le()?,
                cursor.read_f32_le()?,
                cursor.read_f32_le()?,
                cursor.read_f32_le()?,
                cursor.read_f32_le()?,
            ]);
        }
        spatial_groups.push(SpatialGroup { records });
    }
    let second_resource = parse_tagged_resource(cursor)?;
    let relations = parse_relations(cursor)?;
    Ok((
        ClanBody::Spatial {
            first_resource,
            spatial_groups,
            second_resource,
        },
        relations,
    ))
}

fn parse_tagged_resource(cursor: &mut Cursor<'_>) -> Result<TaggedResource, MissionError> {
    Ok(TaggedResource {
        path: read_lp_string(cursor)?,
        tag: cursor.read_i32_le()?,
    })
}

fn parse_relations(cursor: &mut Cursor<'_>) -> Result<Vec<ClanRelation>, MissionError> {
    let count = checked_count(cursor.read_u32_le()?, MAX_RELATIONS)?;
    let mut relations = Vec::with_capacity(count);
    for _ in 0..count {
        relations.push(ClanRelation {
            other_clan_name: read_lp_string(cursor)?,
            relation_value: cursor.read_i32_le()?,
        });
    }
    Ok(relations)
}

fn parse_objects(
    cursor: &mut Cursor<'_>,
    profile: TmaProfile,
) -> Result<Vec<PlacedObject>, MissionError> {
    let count = checked_count(cursor.read_u32_le()?, MAX_OBJECTS)?;
    let mut objects = Vec::with_capacity(count);
    for object_index in 0..count {
        let raw_kind = cursor.read_u32_le()?;
        let class_or_flags = cursor.read_u32_le()?;
        if profile == TmaProfile::Strict && class_or_flags != OBJECT_CLASS_OR_FLAGS {
            return Err(MissionError::InvalidObjectFlags {
                object: object_index,
                flags: class_or_flags,
            });
        }
        let resource_name = read_lp_string(cursor)?;
        let resource_raw = resource_name.raw.clone();
        let raw_after_resource = cursor.read_u32_le()?;
        let identity_or_clan_raw = cursor.read_u32_le()?;
        let position = read_vec3(cursor)?;
        let orientation = read_vec3(cursor)?;
        let scale = read_vec3(cursor)?;
        if !all_finite(&position) || !all_finite(&orientation) || !all_finite(&scale) {
            return Err(MissionError::NonFiniteTransform {
                object: object_index,
            });
        }
        let instance_name = read_lp_string(cursor)?;
        let raw_after_name = cursor.read_u32_le()?;
        let link0 = cursor.read_i32_le()?;
        let link1 = cursor.read_i32_le()?;
        let property_schema_version = cursor.read_u32_le()?;
        require_version(
            "property schema",
            property_schema_version,
            PROPERTY_SCHEMA_VERSION,
            profile,
        )?;
        let properties = parse_properties(cursor)?;
        objects.push(PlacedObject {
            raw_kind,
            class_or_flags,
            resource_name,
            resource_raw,
            raw_after_resource,
            identity_or_clan_raw,
            position,
            orientation,
            scale,
            instance_name,
            raw_after_name,
            link0,
            link1,
            property_schema_version,
            properties,
        });
    }
    Ok(objects)
}

fn parse_properties(cursor: &mut Cursor<'_>) -> Result<Vec<OrderedProperty>, MissionError> {
    let count = checked_count(cursor.read_u32_le()?, MAX_PROPERTIES)?;
    let mut properties = Vec::with_capacity(count);
    for _ in 0..count {
        let raw_value = [
            cursor.read_u32_le()?,
            cursor.read_u32_le()?,
            cursor.read_u32_le()?,
            cursor.read_u32_le()?,
        ];
        let name = read_lp_string(cursor)?;
        let name_raw = name.raw.clone();
        properties.push(OrderedProperty {
            raw_value,
            name,
            name_raw,
        });
    }
    Ok(properties)
}

fn parse_extras(cursor: &mut Cursor<'_>) -> Result<Vec<ExtraRecord28>, MissionError> {
    let count = checked_count(cursor.read_u32_le()?, MAX_EXTRAS)?;
    checked_count_bytes(count as u64, 28, cursor.remaining() as u64)?;
    let mut extras = Vec::with_capacity(count);
    for _ in 0..count {
        let chunk = cursor.read_exact(28)?;
        let mut raw = [0; 28];
        raw.copy_from_slice(chunk);
        extras.push(ExtraRecord28 {
            raw,
            position: [
                read_f32_from(chunk, 0)?,
                read_f32_from(chunk, 4)?,
                read_f32_from(chunk, 8)?,
            ],
            raw_words: [
                read_u32_from(chunk, 12)?,
                read_u32_from(chunk, 16)?,
                read_u32_from(chunk, 20)?,
                read_u32_from(chunk, 24)?,
            ],
        });
    }
    Ok(extras)
}

fn read_lp_string(cursor: &mut Cursor<'_>) -> Result<LpString, MissionError> {
    let raw = read_lp_bytes(cursor, MAX_STRING_BYTES)?;
    let (decoded, _, _) = WINDOWS_1251.decode(&raw);
    let decoded = decoded.into_owned();
    Ok(LpString { raw, decoded })
}

fn read_vec3(cursor: &mut Cursor<'_>) -> Result<[f32; 3], MissionError> {
    Ok([
        cursor.read_f32_le()?,
        cursor.read_f32_le()?,
        cursor.read_f32_le()?,
    ])
}

fn all_finite(value: &[f32; 3]) -> bool {
    value.iter().all(|component| component.is_finite())
}

fn checked_count(count: u32, limit: u32) -> Result<usize, MissionError> {
    if count > limit {
        return Err(DecodeError::LimitExceeded {
            count: u64::from(count),
            limit: u64::from(limit),
        }
        .into());
    }
    usize::try_from(count).map_err(|_| DecodeError::IntegerOverflow.into())
}

fn read_u32_from(bytes: &[u8], offset: usize) -> Result<u32, MissionError> {
    let raw = bytes
        .get(offset..offset + 4)
        .ok_or(DecodeError::IntegerOverflow)?;
    Ok(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
}

fn read_f32_from(bytes: &[u8], offset: usize) -> Result<f32, MissionError> {
    Ok(f32::from_bits(read_u32_from(bytes, offset)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn minimal_synthetic_exact_eof() {
        let mut bytes = Vec::new();
        push_u32(&mut bytes, FORMAT_VERSION);
        push_u32(&mut bytes, 0);
        push_u32(&mut bytes, CLAN_SECTION_VERSION);
        push_u32(&mut bytes, 0);
        push_u32(&mut bytes, OBJECT_SECTION_VERSION);
        push_u32(&mut bytes, 0);
        push_lp(&mut bytes, b"DATA\\MAPS\\Tut_1\\land");
        push_u32(&mut bytes, 0);
        push_lp(&mut bytes, b"");
        push_u32(&mut bytes, EXTRA_SECTION_VERSION);
        push_u32(&mut bytes, 0);

        let doc = decode_tma(Arc::from(bytes.into_boxed_slice()), TmaProfile::Strict).expect("tma");
        assert_eq!(
            doc.versions,
            vec![
                FORMAT_VERSION,
                CLAN_SECTION_VERSION,
                OBJECT_SECTION_VERSION,
                EXTRA_SECTION_VERSION
            ]
        );
        assert_eq!(doc.land_path.decoded, "DATA\\MAPS\\Tut_1\\land");
    }

    #[test]
    fn land_path_prefix_decode_matches_full_document() {
        let bytes = minimal_tma_bytes();
        let prefix = decode_tma_land_path(&bytes, TmaProfile::Strict).expect("land path prefix");
        let doc = decode_tma(Arc::from(bytes.into_boxed_slice()), TmaProfile::Strict).expect("tma");

        assert_eq!(prefix, doc.land_path);
    }

    #[test]
    fn lp_string_does_not_consume_implicit_nul() {
        let mut bytes = Vec::new();
        push_u32(&mut bytes, FORMAT_VERSION);
        push_u32(&mut bytes, 0);
        push_u32(&mut bytes, CLAN_SECTION_VERSION);
        push_u32(&mut bytes, 0);
        push_u32(&mut bytes, OBJECT_SECTION_VERSION);
        push_u32(&mut bytes, 0);
        push_lp(&mut bytes, b"A\0B");
        push_u32(&mut bytes, 0x55aa);
        push_lp(&mut bytes, b"");
        push_u32(&mut bytes, EXTRA_SECTION_VERSION);
        push_u32(&mut bytes, 0);

        let doc = decode_tma(Arc::from(bytes.into_boxed_slice()), TmaProfile::Strict).expect("tma");
        assert_eq!(doc.land_path.raw, b"A\0B");
        assert_eq!(doc.mission_flag, 0x55aa);
    }

    #[test]
    fn synthetic_standard_clan_and_object_preserve_ordered_properties() {
        let mut bytes = Vec::new();
        push_u32(&mut bytes, FORMAT_VERSION);
        push_u32(&mut bytes, 1);
        push_i32(&mut bytes, 42);
        push_u32(&mut bytes, 1);
        push_f32(&mut bytes, 1.0);
        push_f32(&mut bytes, 2.0);
        push_f32(&mut bytes, 3.0);
        push_u32(&mut bytes, CLAN_SECTION_VERSION);
        push_u32(&mut bytes, 1);
        push_lp(&mut bytes, b"Alpha");
        push_i32(&mut bytes, -1);
        push_f32(&mut bytes, 10.0);
        push_f32(&mut bytes, 20.0);
        push_u32(&mut bytes, 1);
        push_lp(&mut bytes, b"Scripts\\a");
        push_i32(&mut bytes, 7);
        push_lp(&mut bytes, b"");
        push_i32(&mut bytes, 8);
        push_u32(&mut bytes, 0);
        push_u32(&mut bytes, OBJECT_SECTION_VERSION);
        push_u32(&mut bytes, 1);
        push_u32(&mut bytes, 0);
        push_u32(&mut bytes, OBJECT_CLASS_OR_FLAGS);
        push_lp(&mut bytes, b"s_tree_04");
        push_u32(&mut bytes, 0);
        push_u32(&mut bytes, 0);
        for value in [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0] {
            push_f32(&mut bytes, value);
        }
        push_lp(&mut bytes, b"tree_01");
        push_u32(&mut bytes, 0);
        push_i32(&mut bytes, -1);
        push_i32(&mut bytes, -1);
        push_u32(&mut bytes, PROPERTY_SCHEMA_VERSION);
        push_u32(&mut bytes, 2);
        for name in [b"Life state".as_slice(), b"Life state".as_slice()] {
            push_u32(&mut bytes, 1);
            push_u32(&mut bytes, 2);
            push_u32(&mut bytes, 3);
            push_u32(&mut bytes, 4);
            push_lp(&mut bytes, name);
        }
        push_lp(&mut bytes, b"DATA\\MAPS\\Tut_1\\land");
        push_u32(&mut bytes, 0);
        push_lp(&mut bytes, b"");
        push_u32(&mut bytes, EXTRA_SECTION_VERSION);
        push_u32(&mut bytes, 0);

        let doc = decode_tma(Arc::from(bytes.into_boxed_slice()), TmaProfile::Strict).expect("tma");
        assert_eq!(doc.paths[0].id, 42);
        assert_eq!(doc.clans[0].name.decoded, "Alpha");
        assert_eq!(doc.objects[0].resource_name.decoded, "s_tree_04");
        assert_eq!(doc.objects[0].properties.len(), 2);
        assert_eq!(doc.objects[0].properties[0].raw_value, [1, 2, 3, 4]);
        assert_eq!(doc.objects[0].properties[0].name.decoded, "Life state");
    }

    #[test]
    fn path_ids_retain_nonsequential_order_and_truncated_points_fail() {
        let mut bytes = Vec::new();
        push_u32(&mut bytes, FORMAT_VERSION);
        push_u32(&mut bytes, 3);
        for id in [30, -5, 10] {
            push_i32(&mut bytes, id);
            push_u32(&mut bytes, 0);
        }
        push_empty_tail(&mut bytes);

        let doc = decode_tma(Arc::from(bytes.into_boxed_slice()), TmaProfile::Strict).expect("tma");
        assert_eq!(
            doc.paths.iter().map(|path| path.id).collect::<Vec<_>>(),
            vec![30, -5, 10]
        );

        let mut truncated = Vec::new();
        push_u32(&mut truncated, FORMAT_VERSION);
        push_u32(&mut truncated, 1);
        push_i32(&mut truncated, 1);
        push_u32(&mut truncated, 1);
        assert!(decode_tma(Arc::from(truncated.into_boxed_slice()), TmaProfile::Strict).is_err());
    }

    #[test]
    fn clan_modes_one_to_three_and_spatial_mode_zero_decode() {
        for mode in 1..=3 {
            let mut bytes = Vec::new();
            push_u32(&mut bytes, FORMAT_VERSION);
            push_u32(&mut bytes, 0);
            push_u32(&mut bytes, CLAN_SECTION_VERSION);
            push_u32(&mut bytes, 1);
            push_standard_clan(&mut bytes, mode);
            push_object_section_and_tail(&mut bytes, 0, b"", &[]);

            let doc =
                decode_tma(Arc::from(bytes.into_boxed_slice()), TmaProfile::Strict).expect("tma");
            assert_eq!(doc.clans[0].mode, mode);
            assert!(matches!(doc.clans[0].body, ClanBody::Standard { .. }));
        }

        let mut bytes = Vec::new();
        push_u32(&mut bytes, FORMAT_VERSION);
        push_u32(&mut bytes, 0);
        push_u32(&mut bytes, CLAN_SECTION_VERSION);
        push_u32(&mut bytes, 1);
        push_lp(&mut bytes, b"Spatial");
        push_i32(&mut bytes, -1);
        push_f32(&mut bytes, 0.0);
        push_f32(&mut bytes, 0.0);
        push_u32(&mut bytes, 0);
        push_lp(&mut bytes, b"first");
        push_u32(&mut bytes, 1);
        push_u32(&mut bytes, 1);
        for value in [1.0, 2.0, 3.0, 4.0, 5.0] {
            push_f32(&mut bytes, value);
        }
        push_lp(&mut bytes, b"second");
        push_i32(&mut bytes, 9);
        push_u32(&mut bytes, 0);
        push_object_section_and_tail(&mut bytes, 0, b"", &[]);

        let doc = decode_tma(Arc::from(bytes.into_boxed_slice()), TmaProfile::Strict).expect("tma");
        let ClanBody::Spatial { spatial_groups, .. } = &doc.clans[0].body else {
            panic!("spatial body");
        };
        assert_eq!(spatial_groups[0].records[0], [1.0, 2.0, 3.0, 4.0, 5.0]);
    }

    #[test]
    fn unknown_clan_mode_nonfinite_transform_and_trailing_bytes_are_rejected() {
        let mut unknown_mode = Vec::new();
        push_u32(&mut unknown_mode, FORMAT_VERSION);
        push_u32(&mut unknown_mode, 0);
        push_u32(&mut unknown_mode, CLAN_SECTION_VERSION);
        push_u32(&mut unknown_mode, 1);
        push_lp(&mut unknown_mode, b"Bad");
        push_i32(&mut unknown_mode, -1);
        push_f32(&mut unknown_mode, 0.0);
        push_f32(&mut unknown_mode, 0.0);
        push_u32(&mut unknown_mode, 99);
        let err = decode_tma(
            Arc::from(unknown_mode.into_boxed_slice()),
            TmaProfile::Strict,
        )
        .expect_err("mode");
        assert!(matches!(
            err,
            MissionError::UnknownClanMode { mode: 99, .. }
        ));

        let mut nonfinite = Vec::new();
        push_u32(&mut nonfinite, FORMAT_VERSION);
        push_u32(&mut nonfinite, 0);
        push_u32(&mut nonfinite, CLAN_SECTION_VERSION);
        push_u32(&mut nonfinite, 0);
        push_u32(&mut nonfinite, OBJECT_SECTION_VERSION);
        push_u32(&mut nonfinite, 1);
        push_object(&mut nonfinite, f32::NAN, &[]);
        push_epilogue(&mut nonfinite, b"DATA\\MAPS\\Tut_1\\land", b"", &[]);
        let err = decode_tma(Arc::from(nonfinite.into_boxed_slice()), TmaProfile::Strict)
            .expect_err("nan");
        assert!(matches!(
            err,
            MissionError::NonFiniteTransform { object: 0 }
        ));

        let mut trailing = minimal_tma_bytes();
        trailing.push(0);
        assert!(decode_tma(Arc::from(trailing.into_boxed_slice()), TmaProfile::Strict).is_err());
    }

    #[test]
    fn description_and_extras_are_exact_raw_records() {
        let mut extra = Vec::new();
        for value in 0_u8..28 {
            extra.push(value);
        }
        let mut bytes = Vec::new();
        push_u32(&mut bytes, FORMAT_VERSION);
        push_u32(&mut bytes, 0);
        push_empty_tail_with_description(&mut bytes, b"A\x00B", &[extra.as_slice()]);

        let doc = decode_tma(Arc::from(bytes.into_boxed_slice()), TmaProfile::Strict).expect("tma");
        assert_eq!(doc.description_raw.raw, b"A\x00B");
        assert_eq!(doc.extras.len(), 1);
        assert_eq!(doc.extras[0].raw[27], 27);

        let mut truncated_extra = Vec::new();
        push_u32(&mut truncated_extra, FORMAT_VERSION);
        push_u32(&mut truncated_extra, 0);
        push_empty_tail_with_description(&mut truncated_extra, b"", &[&extra[..27]]);
        assert!(decode_tma(
            Arc::from(truncated_extra.into_boxed_slice()),
            TmaProfile::Strict
        )
        .is_err());
    }

    #[test]
    fn signatures_inside_strings_do_not_create_records_and_truncations_are_bounded() {
        let mut bytes = Vec::new();
        push_u32(&mut bytes, FORMAT_VERSION);
        push_u32(&mut bytes, 0);
        push_empty_tail_with_description(&mut bytes, &[1, 0, 0, 0, 6, 0, 0, 0], &[]);

        let doc = decode_tma(
            Arc::from(bytes.clone().into_boxed_slice()),
            TmaProfile::Strict,
        )
        .expect("tma");
        assert!(doc.paths.is_empty());
        assert_eq!(doc.description_raw.raw, [1, 0, 0, 0, 6, 0, 0, 0]);

        for len in 0..bytes.len() {
            let _ = decode_tma(
                Arc::from(bytes[..len].to_vec().into_boxed_slice()),
                TmaProfile::Strict,
            );
        }
    }

    #[test]
    fn generated_valid_documents_and_arbitrary_inputs_are_bounded() {
        for seed in 0_u32..64 {
            let mut bytes = Vec::new();
            push_u32(&mut bytes, FORMAT_VERSION);
            push_u32(&mut bytes, 1);
            push_i32(&mut bytes, i32::try_from(seed).expect("seed"));
            push_u32(&mut bytes, 1);
            push_f32(&mut bytes, seed as f32);
            push_f32(&mut bytes, 1.0);
            push_f32(&mut bytes, 2.0);
            push_empty_tail_with_description(&mut bytes, &[seed as u8, 0, 1], &[]);

            let doc = decode_tma(
                Arc::from(bytes.clone().into_boxed_slice()),
                TmaProfile::Strict,
            )
            .expect("generated");
            assert_eq!(doc.raw.as_ref(), bytes.as_slice());
            assert_eq!(doc.paths[0].id, i32::try_from(seed).expect("seed"));

            let arbitrary = (0..seed % 31)
                .map(|offset| seed.wrapping_mul(17).wrapping_add(offset) as u8)
                .collect::<Vec<_>>();
            let _ = decode_tma(Arc::from(arbitrary.into_boxed_slice()), TmaProfile::Strict);
        }
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_corpus_tma_validate() {
        for (
            corpus,
            expected_files,
            expected_paths,
            expected_clans,
            expected_objects,
            expected_extras,
        ) in [
            ("IS", 29_usize, 34_usize, 101_usize, 864_usize, 28_usize),
            ("IS2", 31_usize, 61_usize, 91_usize, 885_usize, 41_usize),
        ] {
            let root = corpus_root(corpus);
            let mut files = 0usize;
            let mut paths = 0usize;
            let mut clans = 0usize;
            let mut objects = 0usize;
            let mut extras = 0usize;
            for path in files_under(&root) {
                if !path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.eq_ignore_ascii_case("data.tma"))
                {
                    continue;
                }
                let bytes = std::fs::read(&path).expect("read data.tma");
                let document = decode_tma(Arc::from(bytes.into_boxed_slice()), TmaProfile::Strict)
                    .unwrap_or_else(|err| panic!("{corpus} {path:?}: {err}"));
                files += 1;
                paths += document.paths.len();
                clans += document.clans.len();
                objects += document.objects.len();
                extras += document.extras.len();
                assert_eq!(document.format_version, FORMAT_VERSION, "{corpus} {path:?}");
                assert_eq!(
                    document.clan_section_version, CLAN_SECTION_VERSION,
                    "{corpus} {path:?}"
                );
                assert_eq!(
                    document.object_section_version, OBJECT_SECTION_VERSION,
                    "{corpus} {path:?}"
                );
                assert_eq!(
                    document.extra_section_version, EXTRA_SECTION_VERSION,
                    "{corpus} {path:?}"
                );
                assert!(
                    document
                        .land_path
                        .decoded
                        .to_ascii_uppercase()
                        .contains("DATA\\MAPS\\"),
                    "{corpus} {path:?} land path"
                );
            }

            assert_eq!(files, expected_files, "{corpus} TMA count");
            assert_eq!(paths, expected_paths, "{corpus} path count");
            assert_eq!(clans, expected_clans, "{corpus} clan count");
            assert_eq!(objects, expected_objects, "{corpus} object count");
            assert_eq!(extras, expected_extras, "{corpus} extra count");
        }
    }

    fn push_lp(out: &mut Vec<u8>, bytes: &[u8]) {
        push_u32(out, u32::try_from(bytes.len()).expect("lp len"));
        out.extend_from_slice(bytes);
    }

    fn push_u32(out: &mut Vec<u8>, value: u32) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn push_i32(out: &mut Vec<u8>, value: i32) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn push_f32(out: &mut Vec<u8>, value: f32) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn minimal_tma_bytes() -> Vec<u8> {
        let mut bytes = Vec::new();
        push_u32(&mut bytes, FORMAT_VERSION);
        push_u32(&mut bytes, 0);
        push_empty_tail(&mut bytes);
        bytes
    }

    fn push_empty_tail(out: &mut Vec<u8>) {
        push_empty_tail_with_description(out, b"", &[]);
    }

    fn push_empty_tail_with_description(out: &mut Vec<u8>, description: &[u8], extras: &[&[u8]]) {
        push_u32(out, CLAN_SECTION_VERSION);
        push_u32(out, 0);
        push_object_section_and_tail(out, 0, description, extras);
    }

    fn push_object_section_and_tail(
        out: &mut Vec<u8>,
        object_count: u32,
        description: &[u8],
        extras: &[&[u8]],
    ) {
        push_u32(out, OBJECT_SECTION_VERSION);
        push_u32(out, object_count);
        push_epilogue(out, b"DATA\\MAPS\\Tut_1\\land", description, extras);
    }

    fn push_epilogue(out: &mut Vec<u8>, land_path: &[u8], description: &[u8], extras: &[&[u8]]) {
        push_lp(out, land_path);
        push_u32(out, 0);
        push_lp(out, description);
        push_u32(out, EXTRA_SECTION_VERSION);
        push_u32(out, u32::try_from(extras.len()).expect("extra count"));
        for extra in extras {
            out.extend_from_slice(extra);
        }
    }

    fn push_standard_clan(out: &mut Vec<u8>, mode: u32) {
        push_lp(out, b"Clan");
        push_i32(out, -1);
        push_f32(out, 0.0);
        push_f32(out, 0.0);
        push_u32(out, mode);
        push_lp(out, b"first");
        push_i32(out, 1);
        push_lp(out, b"second");
        push_i32(out, 2);
        push_u32(out, 0);
    }

    fn push_object(out: &mut Vec<u8>, first_position: f32, properties: &[(&[u8], [u32; 4])]) {
        push_u32(out, 0);
        push_u32(out, OBJECT_CLASS_OR_FLAGS);
        push_lp(out, b"s_tree_04");
        push_u32(out, 0);
        push_u32(out, 0);
        for value in [first_position, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0] {
            push_f32(out, value);
        }
        push_lp(out, b"tree_01");
        push_u32(out, 0);
        push_i32(out, -1);
        push_i32(out, -1);
        push_u32(out, PROPERTY_SCHEMA_VERSION);
        push_u32(
            out,
            u32::try_from(properties.len()).expect("property count"),
        );
        for (name, raw) in properties {
            for value in raw {
                push_u32(out, *value);
            }
            push_lp(out, name);
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
}
