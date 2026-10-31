use crate::{decode_nres_payload, NresDocument};
use fparkan_binary::{checked_allocation_len, checked_count_bytes, Cursor, DecodeError, Limits};
use fparkan_nres::{EntryId, NresEntry};
use fparkan_prototype::{unit_component_tree, PrototypeError, UnitComponentTreeNode, UnitDat};
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

const TRF0: u32 = u32::from_le_bytes(*b"TRF0");
const TRF1: u32 = u32::from_le_bytes(*b"TRF1");
const TRF2: u32 = u32::from_le_bytes(*b"TRF2");
const TRF3: u32 = u32::from_le_bytes(*b"TRF3");
const TRF4: u32 = u32::from_le_bytes(*b"TRF4");
const TRF5: u32 = u32::from_le_bytes(*b"TRF5");
const TRF6: u32 = u32::from_le_bytes(*b"TRF6");
const TRF7: u32 = u32::from_le_bytes(*b"TRF7");
const TRF8: u32 = u32::from_le_bytes(*b"TRF8");
const TRF9: u32 = u32::from_le_bytes(*b"TRF9");
const TRFA: u32 = u32::from_le_bytes(*b"TRFA");
const TRFB: u32 = u32::from_le_bytes(*b"TRFB");

const RESEARCH_CHUNKS: [u32; 12] = [
    TRF0, TRF1, TRF2, TRF3, TRF4, TRF5, TRF6, TRF7, TRF8, TRF9, TRFA, TRFB,
];

/// Ore and power outputs from one native CalcFullResearchCost call.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResearchCost {
    /// First native output, labeled ore by the Calc debug string.
    pub ore: f32,
    /// Second native output, labeled power by the Calc debug string.
    pub power: f32,
}

/// One decoded MisLoad research table and its original NRes archive.
#[derive(Debug)]
pub struct ResearchTrfDocument {
    archive: NresDocument,
    records: Vec<[u32; 10]>,
    flags: Vec<u8>,
    aliases: Vec<ResearchAlias>,
    name_pool: Vec<u8>,
    relation_2_3: ResearchRelations,
    relation_4_5: ResearchRelations,
}

#[derive(Clone, Debug)]
struct ResearchAlias {
    name_range: std::ops::Range<usize>,
    row_index: u16,
}

#[derive(Debug)]
struct ResearchRelations {
    table_id: u32,
    group_starts: Vec<usize>,
    indices: Vec<u32>,
}

#[derive(Debug)]
struct Chunk<'a> {
    entry: &'a NresEntry,
    payload: &'a [u8],
}

/// TRF decoding and cost-calculation error.
#[derive(Debug)]
pub enum ResearchError {
    /// The outer NRes archive is malformed.
    Nres(fparkan_nres::NresError),
    /// A bounded TRF field is truncated or overflows.
    Binary(DecodeError),
    /// A native-required TRF chunk is absent.
    MissingChunk { type_id: u32 },
    /// A recognized TRF chunk occurs more than once.
    DuplicateChunk { type_id: u32 },
    /// The native loader requires TRF0 attribute 2 to equal three.
    InvalidTrf0Attribute { got: u32 },
    /// A TRF1 payload is too large for the native zero-initialized flag buffer.
    FlagsTooLong { row_count: usize, actual: usize },
    /// A TRFB pair points outside the TRF6 name pool.
    InvalidNameOffset { pair_index: usize, offset: u16 },
    /// A TRF6 string does not end in NUL before the pool ends.
    UnterminatedName { pair_index: usize, offset: u16 },
    /// A TRF0 row's low 16-bit pair link is outside TRFB.
    InvalidRecordPair {
        record_index: usize,
        pair_index: u16,
        pair_count: usize,
    },
    /// A key or relation resolves to a research row outside TRF0.
    InvalidResearchIndex {
        type_id: u32,
        index: u32,
        row_count: usize,
    },
    /// A relation table has no group at this research row.
    MissingRelationGroup {
        type_id: u32,
        index: usize,
        group_count: usize,
    },
    /// A required non-empty relation stream is absent.
    MissingRelationStream { type_id: u32, required_ids: u64 },
    /// A candidate Unit DAT resource field is not NUL-terminated.
    MissingCandidateKeyTerminator { component_index: usize },
    /// No TRFB key matches an enabled candidate component.
    UnknownResearchKey {
        component_index: usize,
        key: Vec<u8>,
    },
    /// A non-ASCII lookup would require an unverified locale-specific fold.
    UnsupportedNonAsciiLookup {
        query: Vec<u8>,
        alias: Option<Vec<u8>>,
    },
    /// The research table has no rows.
    EmptyResearchTable,
    /// The candidate Unit DAT has no root component.
    EmptyCandidateTree,
    /// The existing Unit DAT tree builder rejected the child-count framing.
    CandidateTree(PrototypeError),
}

impl fmt::Display for ResearchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Nres(source) => write!(formatter, "{source}"),
            Self::Binary(source) => write!(formatter, "{source}"),
            Self::MissingChunk { type_id } => {
                write!(formatter, "research NRes is missing chunk {type_id:#010x}")
            }
            Self::DuplicateChunk { type_id } => {
                write!(formatter, "research NRes has duplicate chunk {type_id:#010x}")
            }
            Self::InvalidTrf0Attribute { got } => {
                write!(formatter, "TRF0 attribute 2 is {got}, expected 3")
            }
            Self::FlagsTooLong { row_count, actual } => write!(
                formatter,
                "TRF1 has {actual} flags, exceeding the {row_count}-byte native buffer"
            ),
            Self::InvalidNameOffset { pair_index, offset } => write!(
                formatter,
                "TRFB pair {pair_index} name offset {offset} is outside TRF6"
            ),
            Self::UnterminatedName { pair_index, offset } => write!(
                formatter,
                "TRFB pair {pair_index} name at TRF6 offset {offset} is not NUL-terminated"
            ),
            Self::InvalidRecordPair {
                record_index,
                pair_index,
                pair_count,
            } => write!(
                formatter,
                "TRF0 row {record_index} links to TRFB pair {pair_index}, but there are {pair_count} pairs"
            ),
            Self::InvalidResearchIndex {
                type_id,
                index,
                row_count,
            } => write!(
                formatter,
                "research index {index} from {type_id:#010x} exceeds TRF0 row count {row_count}"
            ),
            Self::MissingRelationGroup {
                type_id,
                index,
                group_count,
            } => write!(
                formatter,
                "{type_id:#010x} has no relation group for row {index} (group count {group_count})"
            ),
            Self::MissingRelationStream {
                type_id,
                required_ids,
            } => write!(
                formatter,
                "{type_id:#010x} is absent but its count table requires {required_ids} indices"
            ),
            Self::MissingCandidateKeyTerminator { component_index } => write!(
                formatter,
                "candidate component {component_index} has no NUL in its 32-byte resource key"
            ),
            Self::UnknownResearchKey {
                component_index,
                key,
            } => write!(
                formatter,
                "candidate component {component_index} key {:?} is absent from TRFB",
                String::from_utf8_lossy(key)
            ),
            Self::UnsupportedNonAsciiLookup { query, alias } => {
                if let Some(alias) = alias {
                    write!(
                        formatter,
                        "research key comparison between {:?} and {:?} needs unverified non-ASCII case folding",
                        String::from_utf8_lossy(query),
                        String::from_utf8_lossy(alias)
                    )
                } else {
                    write!(
                        formatter,
                        "research key {:?} needs unverified non-ASCII case folding",
                        String::from_utf8_lossy(query)
                    )
                }
            }
            Self::EmptyResearchTable => write!(formatter, "research table has no rows"),
            Self::EmptyCandidateTree => write!(formatter, "candidate Unit DAT has no root component"),
            Self::CandidateTree(source) => write!(formatter, "{source}"),
        }
    }
}

impl std::error::Error for ResearchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Nres(source) => Some(source),
            Self::Binary(source) => Some(source),
            Self::CandidateTree(source) => Some(source),
            Self::MissingChunk { .. }
            | Self::DuplicateChunk { .. }
            | Self::InvalidTrf0Attribute { .. }
            | Self::FlagsTooLong { .. }
            | Self::InvalidNameOffset { .. }
            | Self::UnterminatedName { .. }
            | Self::InvalidRecordPair { .. }
            | Self::InvalidResearchIndex { .. }
            | Self::MissingRelationGroup { .. }
            | Self::MissingRelationStream { .. }
            | Self::MissingCandidateKeyTerminator { .. }
            | Self::UnknownResearchKey { .. }
            | Self::UnsupportedNonAsciiLookup { .. }
            | Self::EmptyResearchTable
            | Self::EmptyCandidateTree => None,
        }
    }
}

impl From<fparkan_nres::NresError> for ResearchError {
    fn from(value: fparkan_nres::NresError) -> Self {
        Self::Nres(value)
    }
}

impl From<DecodeError> for ResearchError {
    fn from(value: DecodeError) -> Self {
        Self::Binary(value)
    }
}

impl From<PrototypeError> for ResearchError {
    fn from(value: PrototypeError) -> Self {
        Self::CandidateTree(value)
    }
}

impl ResearchTrfDocument {
    /// Decodes the native research table chunks from an NRes archive.
    ///
    /// NRes validates the archive framing and payload ranges. This reader then
    /// validates only the TRF bytes needed by its row, key, and relation views.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed NRes framing, a missing required chunk,
    /// an invalid TRF0 guard, or a referenced byte range outside its chunk.
    pub fn decode(bytes: Arc<[u8]>) -> Result<Self, ResearchError> {
        let archive = decode_nres_payload(bytes)?;
        let mut chunk_ids = HashMap::new();
        for entry in archive.entries() {
            let type_id = entry.meta().type_id;
            if RESEARCH_CHUNKS.contains(&type_id) && chunk_ids.insert(type_id, entry.id()).is_some()
            {
                return Err(ResearchError::DuplicateChunk { type_id });
            }
        }

        let trf0 = required_chunk(&archive, &chunk_ids, TRF0)?;
        if trf0.entry.meta().attr2 != 3 {
            return Err(ResearchError::InvalidTrf0Attribute {
                got: trf0.entry.meta().attr2,
            });
        }
        let row_count = trf0.entry.meta().attr1;
        let row_bytes = counted_prefix(trf0.payload, u64::from(row_count), 40)?;
        let records = read_research_rows(row_bytes, row_count)?;

        let trf1 = required_chunk(&archive, &chunk_ids, TRF1)?;
        let flag_count = usize::try_from(row_count).map_err(|_| DecodeError::IntegerOverflow)?;
        if trf1.payload.len() > flag_count {
            return Err(ResearchError::FlagsTooLong {
                row_count: flag_count,
                actual: trf1.payload.len(),
            });
        }
        let mut flags = vec![0; flag_count];
        flags[..trf1.payload.len()].copy_from_slice(trf1.payload);

        let trfb = required_chunk(&archive, &chunk_ids, TRFB)?;
        let alias_count = trfb.entry.meta().attr1;
        let pair_bytes = counted_prefix(trfb.payload, u64::from(alias_count), 4)?;
        let trf6 = required_chunk(&archive, &chunk_ids, TRF6)?;
        let name_pool = trf6.payload.to_vec();
        let aliases = read_aliases(pair_bytes, &name_pool, alias_count)?;

        for (record_index, record) in records.iter().enumerate() {
            // The native reader casts the packed word to u16 before indexing TRFB.
            let pair_index = record[8] as u16;
            if usize::from(pair_index) >= aliases.len() {
                return Err(ResearchError::InvalidRecordPair {
                    record_index,
                    pair_index,
                    pair_count: aliases.len(),
                });
            }
        }

        for type_id in [TRF7, TRF8, TRF9, TRFA] {
            required_chunk(&archive, &chunk_ids, type_id)?;
        }

        let relation_2_3 = read_relations(&archive, &chunk_ids, TRF2, TRF3)?;
        let relation_4_5 = read_relations(&archive, &chunk_ids, TRF4, TRF5)?;

        Ok(Self {
            archive,
            records,
            flags,
            aliases,
            name_pool,
            relation_2_3,
            relation_4_5,
        })
    }

    /// Returns the retained NRes document, including opaque chunks and unused payload tails.
    #[must_use]
    pub fn archive(&self) -> &NresDocument {
        &self.archive
    }

    /// Number of TRF0 rows, taken from that chunk's own attr1.
    #[must_use]
    pub fn row_count(&self) -> usize {
        self.records.len()
    }

    /// Looks up the first ASCII-case-insensitive TRFB alias in source order.
    ///
    /// The shipped research keys are ASCII and use ASCII case-insensitive matching.
    /// Comparisons involving non-ASCII bytes fail explicitly because native
    /// locale-specific folding for those bytes is unknown.
    ///
    /// # Errors
    ///
    /// Returns an error if matching requires unverified non-ASCII folding or
    /// the selected alias contains an out-of-range row id.
    pub fn find_key(&self, key: &[u8]) -> Result<Option<usize>, ResearchError> {
        let key = key.split(|byte| *byte == 0).next().unwrap_or_default();
        if !key.is_ascii() {
            return Err(ResearchError::UnsupportedNonAsciiLookup {
                query: key.to_vec(),
                alias: None,
            });
        }
        for alias in &self.aliases {
            let name = &self.name_pool[alias.name_range.clone()];
            if !name.is_ascii() {
                return Err(ResearchError::UnsupportedNonAsciiLookup {
                    query: key.to_vec(),
                    alias: Some(name.to_vec()),
                });
            }
            if name.eq_ignore_ascii_case(key) {
                let row_index = u32::from(alias.row_index);
                if usize::from(alias.row_index) >= self.records.len() {
                    return Err(ResearchError::InvalidResearchIndex {
                        type_id: TRFB,
                        index: row_index,
                        row_count: self.records.len(),
                    });
                }
                return Ok(Some(usize::from(alias.row_index)));
            }
        }
        Ok(None)
    }

    /// Returns the preserved ten raw TRF0 words for one row.
    #[must_use]
    pub fn row_words(&self, index: usize) -> Option<&[u32; 10]> {
        self.records.get(index)
    }

    /// Returns the original TRF1 byte for one row.
    #[must_use]
    pub fn flag_byte(&self, index: usize) -> Option<u8> {
        self.flags.get(index).copied()
    }

    /// Returns the raw name bytes selected by a row's low-16-bit TRFB link.
    ///
    /// # Errors
    ///
    /// Returns an error when the row is outside TRF0.
    pub fn row_name(&self, index: usize) -> Result<&[u8], ResearchError> {
        let row = self
            .records
            .get(index)
            .ok_or(ResearchError::InvalidResearchIndex {
                type_id: TRF0,
                index: u32::try_from(index).unwrap_or(u32::MAX),
                row_count: self.records.len(),
            })?;
        let pair_index = usize::from(row[8] as u16);
        let alias = self
            .aliases
            .get(pair_index)
            .ok_or(ResearchError::InvalidRecordPair {
                record_index: index,
                pair_index: row[8] as u16,
                pair_count: self.aliases.len(),
            })?;
        Ok(&self.name_pool[alias.name_range.clone()])
    }

    /// Returns the ordered TRF2/TRF3 indices used by CalcFullResearchCost.
    ///
    /// # Errors
    ///
    /// Returns an error if this row has no TRF2 group or its TRF3 stream
    /// contains a row index outside TRF0.
    pub fn relation_2_3(&self, index: usize) -> Result<&[u32], ResearchError> {
        let related = self.relation_2_3.get(index)?;
        validate_relation_indices(related, TRF3, self.records.len())?;
        Ok(related)
    }

    /// Returns the separate ordered TRF4/TRF5 index group.
    ///
    /// # Errors
    ///
    /// Returns an error if this row has no TRF4 group or its TRF5 stream
    /// contains a row index outside TRF0.
    pub fn relation_4_5(&self, index: usize) -> Result<&[u32], ResearchError> {
        let related = self.relation_4_5.get(index)?;
        validate_relation_indices(related, TRF5, self.records.len())?;
        Ok(related)
    }

    fn gates(&self, index: usize) -> Result<(u32, u32, u32), ResearchError> {
        let flag = self
            .flags
            .get(index)
            .copied()
            .ok_or(ResearchError::InvalidResearchIndex {
                type_id: TRF1,
                index: u32::try_from(index).unwrap_or(u32::MAX),
                row_count: self.records.len(),
            })?;
        Ok((
            u32::from((flag >> 1) & 1),
            u32::from(flag & 1),
            u32::from((flag >> 2) & 1),
        ))
    }
}

/// Calculates the ordered ore and power cost for one candidate Unit DAT tree.
///
/// # Errors
///
/// Returns an error for an empty research table, malformed candidate tree,
/// missing or unsupported key comparison, invalid relation group, or invalid
/// row reference.
pub fn calc_full_research_cost(
    candidate: &UnitDat,
    research: &ResearchTrfDocument,
) -> Result<ResearchCost, ResearchError> {
    if research.records.is_empty() {
        return Err(ResearchError::EmptyResearchTable);
    }
    let tree = unit_component_tree(&candidate.records)?;
    if tree.is_empty() {
        return Err(ResearchError::EmptyCandidateTree);
    }

    let mut visited = vec![false; research.records.len()];
    let direct = collect_direct_indices(candidate, &tree, research, &mut visited)?;
    let mut closure = Vec::new();
    for &index in &direct {
        closure.push(index);
        let (_, recurse, _) = research.gates(index)?;
        if recurse == 0 {
            append_relation_closure(index, research, &mut visited, &mut closure)?;
        }
    }

    let mut ore = 0.0_f32;
    let mut power = 0.0_f32;
    for index in closure {
        let (included, _, _) = research.gates(index)?;
        if included == 0 {
            let row = research
                .records
                .get(index)
                .ok_or(ResearchError::InvalidResearchIndex {
                    type_id: TRF0,
                    index: u32::try_from(index).unwrap_or(u32::MAX),
                    row_count: research.records.len(),
                })?;
            ore = ore + f32::from_bits(row[1]);
            power = power + f32::from_bits(row[0]);
        }
    }

    Ok(ResearchCost { ore, power })
}

fn collect_direct_indices(
    candidate: &UnitDat,
    tree: &[UnitComponentTreeNode],
    research: &ResearchTrfDocument,
    visited: &mut [bool],
) -> Result<Vec<usize>, ResearchError> {
    let mut direct = Vec::new();
    let mut stack = vec![0_usize];
    let mut first_error = None;

    while let Some(node_index) = stack.pop() {
        let Some(node) = tree.get(node_index) else {
            return Err(ResearchError::CandidateTree(
                PrototypeError::InvalidUnitHierarchy(format!(
                    "component index {node_index} is outside the reconstructed tree"
                )),
            ));
        };
        let Some(component) = candidate.records.get(node.record_index) else {
            return Err(ResearchError::CandidateTree(
                PrototypeError::InvalidUnitHierarchy(format!(
                    "tree references missing component {}",
                    node.record_index
                )),
            ));
        };

        if component.kind == 0 {
            continue;
        }

        let key_end = component.resource_raw.iter().position(|byte| *byte == 0);
        let key = match key_end {
            Some(end) => &component.resource_raw[..end],
            None => {
                if first_error.is_none() {
                    first_error = Some(ResearchError::MissingCandidateKeyTerminator {
                        component_index: node.record_index,
                    });
                }
                for child in node.children.iter().rev() {
                    stack.push(*child);
                }
                continue;
            }
        };

        match research.find_key(key) {
            Ok(Some(index)) => match research.gates(index) {
                Ok((included, _, _)) => {
                    if included == 0 && !visited[index] {
                        visited[index] = true;
                        direct.push(index);
                    }
                }
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            },
            Ok(None) => {
                if first_error.is_none() {
                    first_error = Some(ResearchError::UnknownResearchKey {
                        component_index: node.record_index,
                        key: key.to_vec(),
                    });
                }
            }
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }

        for child in node.children.iter().rev() {
            stack.push(*child);
        }
    }

    if let Some(error) = first_error {
        return Err(error);
    }
    Ok(direct)
}

#[derive(Clone, Copy)]
struct RelationFrame {
    row_index: usize,
    next_child: usize,
}

fn append_relation_closure(
    root: usize,
    research: &ResearchTrfDocument,
    visited: &mut [bool],
    closure: &mut Vec<usize>,
) -> Result<(), ResearchError> {
    let mut stack = vec![RelationFrame {
        row_index: root,
        next_child: 0,
    }];

    while let Some(frame) = stack.last().copied() {
        let related = research.relation_2_3.get(frame.row_index)?;
        if frame.next_child >= related.len() {
            stack.pop();
            continue;
        }
        let related_index = related[frame.next_child];
        stack
            .last_mut()
            .expect("non-empty relation stack")
            .next_child += 1;

        let index = usize::try_from(related_index).map_err(|_| DecodeError::IntegerOverflow)?;
        if index >= research.records.len() {
            return Err(ResearchError::InvalidResearchIndex {
                type_id: TRF3,
                index: related_index,
                row_count: research.records.len(),
            });
        }
        let (included, recurse, _) = research.gates(index)?;
        if included != 0 || visited[index] {
            continue;
        }

        visited[index] = true;
        closure.push(index);
        if recurse == 0 {
            stack.push(RelationFrame {
                row_index: index,
                next_child: 0,
            });
        }
    }

    Ok(())
}

impl ResearchRelations {
    fn get(&self, index: usize) -> Result<&[u32], ResearchError> {
        let group_count = self.group_starts.len().saturating_sub(1);
        let missing_group = || ResearchError::MissingRelationGroup {
            type_id: self.table_id,
            index,
            group_count,
        };
        let Some(end_index) = index.checked_add(1) else {
            return Err(missing_group());
        };
        let Some(&start) = self.group_starts.get(index) else {
            return Err(missing_group());
        };
        let Some(&end) = self.group_starts.get(end_index) else {
            return Err(missing_group());
        };
        Ok(&self.indices[start..end])
    }
}

fn validate_relation_indices(
    related: &[u32],
    type_id: u32,
    row_count: usize,
) -> Result<(), ResearchError> {
    for &index in related {
        if usize::try_from(index).map_or(true, |value| value >= row_count) {
            return Err(ResearchError::InvalidResearchIndex {
                type_id,
                index,
                row_count,
            });
        }
    }
    Ok(())
}

fn required_chunk<'a>(
    archive: &'a NresDocument,
    chunk_ids: &HashMap<u32, EntryId>,
    type_id: u32,
) -> Result<Chunk<'a>, ResearchError> {
    let id = chunk_ids
        .get(&type_id)
        .copied()
        .ok_or(ResearchError::MissingChunk { type_id })?;
    let entry = archive
        .entry(id)
        .ok_or(fparkan_nres::NresError::EntryIdOutOfRange {
            id: id.0,
            entry_count: archive.header().entry_count,
        })?;
    let payload = archive.payload(id)?;
    Ok(Chunk { entry, payload })
}

fn optional_chunk<'a>(
    archive: &'a NresDocument,
    chunk_ids: &HashMap<u32, EntryId>,
    type_id: u32,
) -> Result<Option<Chunk<'a>>, ResearchError> {
    let Some(id) = chunk_ids.get(&type_id).copied() else {
        return Ok(None);
    };
    let entry = archive
        .entry(id)
        .ok_or(fparkan_nres::NresError::EntryIdOutOfRange {
            id: id.0,
            entry_count: archive.header().entry_count,
        })?;
    let payload = archive.payload(id)?;
    Ok(Some(Chunk { entry, payload }))
}

fn counted_prefix(payload: &[u8], count: u64, stride: u64) -> Result<&[u8], ResearchError> {
    let remaining = u64::try_from(payload.len()).map_err(|_| DecodeError::IntegerOverflow)?;
    let size = checked_count_bytes(count, stride, remaining)?;
    Ok(&payload[..size])
}

fn read_research_rows(bytes: &[u8], row_count: u32) -> Result<Vec<[u32; 10]>, ResearchError> {
    let capacity = checked_allocation_len(
        u64::from(row_count),
        u64::from(Limits::default().max_array_items),
    )?;
    let mut cursor = Cursor::new(bytes);
    let mut records = Vec::with_capacity(capacity);
    for _ in 0..row_count {
        let mut row = [0_u32; 10];
        for word in &mut row {
            *word = cursor.read_u32_le()?;
        }
        records.push(row);
    }
    cursor.require_eof()?;
    Ok(records)
}

fn read_aliases(
    bytes: &[u8],
    name_pool: &[u8],
    alias_count: u32,
) -> Result<Vec<ResearchAlias>, ResearchError> {
    let capacity = checked_allocation_len(
        u64::from(alias_count),
        u64::from(Limits::default().max_array_items),
    )?;
    let mut cursor = Cursor::new(bytes);
    let mut aliases = Vec::with_capacity(capacity);
    for pair_index in 0..capacity {
        let name_offset = cursor.read_u16_le()?;
        let row_index = cursor.read_u16_le()?;
        let offset = usize::from(name_offset);
        let suffix = name_pool
            .get(offset..)
            .ok_or(ResearchError::InvalidNameOffset {
                pair_index,
                offset: name_offset,
            })?;
        let length =
            suffix
                .iter()
                .position(|byte| *byte == 0)
                .ok_or(ResearchError::UnterminatedName {
                    pair_index,
                    offset: name_offset,
                })?;
        aliases.push(ResearchAlias {
            name_range: offset..offset + length,
            row_index,
        });
    }
    cursor.require_eof()?;
    Ok(aliases)
}

fn read_relations(
    archive: &NresDocument,
    chunk_ids: &HashMap<u32, EntryId>,
    count_type: u32,
    index_type: u32,
) -> Result<ResearchRelations, ResearchError> {
    let count_chunk = required_chunk(archive, chunk_ids, count_type)?;
    let count_count = count_chunk.entry.meta().attr1;
    let count_bytes = counted_prefix(count_chunk.payload, u64::from(count_count), 4)?;
    let mut cursor = Cursor::new(count_bytes);
    let max_items = u64::from(Limits::default().max_array_items);
    let group_count = u64::from(count_count);
    let offset_count = group_count
        .checked_add(1)
        .ok_or(DecodeError::IntegerOverflow)?;
    let offset_capacity = checked_allocation_len(
        offset_count,
        max_items
            .checked_add(1)
            .ok_or(DecodeError::IntegerOverflow)?,
    )?;
    let mut group_starts = Vec::with_capacity(offset_capacity);
    group_starts.push(0_usize);
    let mut total_ids = 0_u64;
    for _ in 0..count_count {
        let count = cursor.read_u32_le()?;
        total_ids = total_ids
            .checked_add(u64::from(count))
            .ok_or(DecodeError::IntegerOverflow)?;
        let next = checked_allocation_len(total_ids, max_items)?;
        group_starts.push(next);
    }

    let index_chunk = optional_chunk(archive, chunk_ids, index_type)?;
    let indices = match index_chunk {
        None if total_ids == 0 => Vec::new(),
        None => {
            return Err(ResearchError::MissingRelationStream {
                type_id: index_type,
                required_ids: total_ids,
            })
        }
        Some(chunk) => {
            let index_bytes = counted_prefix(chunk.payload, total_ids, 4)?;
            let mut cursor = Cursor::new(index_bytes);
            let capacity = checked_allocation_len(total_ids, max_items)?;
            let mut indices = Vec::with_capacity(capacity);
            for _ in 0..total_ids {
                indices.push(cursor.read_u32_le()?);
            }
            cursor.require_eof()?;
            indices
        }
    };

    Ok(ResearchRelations {
        table_id: count_type,
        group_starts,
        indices,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoded_array_limits_fail_before_large_allocations() {
        let limit = Limits::default().max_array_items;
        let too_many = limit.checked_add(1).expect("test count");

        assert!(matches!(
            read_research_rows(&[], too_many),
            Err(ResearchError::Binary(DecodeError::LimitExceeded { count, limit: actual_limit }))
                if count == u64::from(too_many) && actual_limit == u64::from(limit)
        ));
        assert!(matches!(
            read_aliases(&[], &[], too_many),
            Err(ResearchError::Binary(DecodeError::LimitExceeded { count, limit: actual_limit }))
                if count == u64::from(too_many) && actual_limit == u64::from(limit)
        ));
    }
}
