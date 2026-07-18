#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::unwrap_used))]
//! Lossless, bounded reader for compiled AI `.scr` packages.
//!
//! This module preserves the layout proven by the GOG `ai.dll` reader.  It
//! deliberately does not assign semantics to instruction words or execute
//! bytecode: that requires handler-specific evidence.

use fparkan_binary::{checked_allocation_len, Cursor, DecodeError, Limits};
use std::sync::Arc;

const INSTRUCTION_HEADER_BYTES: u64 = 28;
const INSTRUCTION_WORDS: usize = 7;
const GOG_HANDLER_COUNT: u32 = 73;

/// A compiled `.scr` package.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScriptPackage {
    /// Number of opcode handlers expected by the package.
    pub opcode_handler_count: u32,
    /// Named events in original file order.
    pub events: Vec<ScriptEvent>,
    /// Bytes not consumed by the recovered framing.
    pub trailing_bytes: Vec<u8>,
    /// Original package bytes.
    pub raw: Arc<[u8]>,
}

/// One named event record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScriptEvent {
    /// Declared byte count excluding the NUL terminator.
    pub name_len: u32,
    /// Name bytes including its mandatory NUL terminator.
    pub name_raw: Vec<u8>,
    /// Opaque event word following the name.
    pub event_word: u32,
    /// Nested instruction records in original file order.
    pub instructions: Vec<ScriptInstruction>,
}

/// A lossless instruction record.
///
/// Seven header words are retained in their on-disk order. The sixth word
/// declares the number of following references; the seventh follows them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScriptInstruction {
    /// Opaque header words in original file order.
    pub header_words: [u32; INSTRUCTION_WORDS],
    /// References stored after header word five and before word six.
    pub references: Vec<u32>,
}

/// The recovered selector for an instruction's installed handler table.
///
/// The GOG AI loader copies 73 function pointers in order. Across all checked
/// GOG packages the first disk word is either one of these indices or the
/// explicit `u32::MAX` sentinel. This is a disassembly contract, not an
/// instruction executor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScriptDispatchSelector {
    /// One of the ordered handlers installed by `ai.dll`.
    Handler(u8),
    /// The on-disk `0xffff_ffff` sentinel.
    Sentinel,
    /// A value not yet observed or accepted by the GOG handler table.
    Unknown(u32),
}

/// Raw inputs resolved by the corpus-reachable `Handler(2)` before it reaches
/// the original event-record scheduler.
///
/// The field names preserve handler slot order, not guessed gameplay meaning.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Handler2RecordInput {
    /// Resolved slot 0 word.
    pub word_0: u32,
    /// Resolved slot 1 scalar.
    pub scalar_1: f32,
    /// Resolved slot 2 word.
    pub word_2: u32,
    /// Resolved slot 3 word.
    pub word_3: u32,
    /// Resolved slot 4 scalar.
    pub scalar_4: f32,
    /// Resolved slot 5 scalar.
    pub scalar_5: f32,
    /// Resolved slot 6 scalar.
    pub scalar_6: f32,
}

/// The exact three-word identity used by the original `Handler(2)` scheduler.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Handler2RecordKey {
    /// First identity word from resolved slot 0.
    pub word_0: u32,
    /// IEEE-754 bits of resolved slot 4.
    pub scalar_4_bits: u32,
    /// IEEE-754 bits of resolved slot 5.
    pub scalar_5_bits: u32,
}

impl From<Handler2RecordInput> for Handler2RecordKey {
    fn from(input: Handler2RecordInput) -> Self {
        Self {
            word_0: input.word_0,
            scalar_4_bits: input.scalar_4.to_bits(),
            scalar_5_bits: input.scalar_5.to_bits(),
        }
    }
}

/// A single backend-neutral event record created by `Handler(2)`.
///
/// This mirrors only the fields whose construction and update rules are
/// statically recovered. Event-name lookup and the downstream consumer remain
/// separate runtime work.
#[derive(Clone, Debug, PartialEq)]
pub struct Handler2Record {
    /// The three-word scheduler identity.
    pub key: Handler2RecordKey,
    /// Resolved slot 1 scalar.
    pub scalar_1: f32,
    /// Initial and per-refresh counter word from resolved slot 2.
    pub counter: u32,
    /// Resolved slot 3 word.
    pub word_3: u32,
    /// Resolved slot 6 scalar.
    pub scalar_6: f32,
}

/// The result of submitting one resolved `Handler(2)` record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Handler2RecordUpdate {
    /// Stable record position in insertion order.
    pub index: usize,
    /// Whether a new record was created.
    pub created: bool,
    /// Whether an existing record took the original refresh path.
    pub refreshed: bool,
}

/// Deterministic model of the original `Handler(2)` event-record collection.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Handler2RecordScheduler {
    records: Vec<Handler2Record>,
}

impl Handler2RecordScheduler {
    /// Returns event records in original insertion order.
    #[must_use]
    pub fn records(&self) -> &[Handler2Record] {
        &self.records
    }

    /// Inserts or refreshes one resolved handler input.
    ///
    /// The original compares the three identity words bit-for-bit. On an
    /// existing key, it refreshes only when `scalar_1` compares unequal; that
    /// update replaces `scalar_1` and `scalar_6`, then adds the record's own
    /// slot-2 counter word with x86 wrapping arithmetic.
    pub fn submit(&mut self, input: Handler2RecordInput) -> Handler2RecordUpdate {
        let key = Handler2RecordKey::from(input);
        if let Some((index, record)) = self
            .records
            .iter_mut()
            .enumerate()
            .find(|(_, record)| record.key == key)
        {
            if !handler_two_scalar_equal(record.scalar_1, input.scalar_1) {
                record.scalar_1 = input.scalar_1;
                record.scalar_6 = input.scalar_6;
                record.counter = record.counter.wrapping_add(input.word_2);
                return Handler2RecordUpdate {
                    index,
                    created: false,
                    refreshed: true,
                };
            }
            return Handler2RecordUpdate {
                index,
                created: false,
                refreshed: false,
            };
        }
        let index = self.records.len();
        self.records.push(Handler2Record {
            key,
            scalar_1: input.scalar_1,
            counter: input.word_2,
            word_3: input.word_3,
            scalar_6: input.scalar_6,
        });
        Handler2RecordUpdate {
            index,
            created: true,
            refreshed: false,
        }
    }
}

fn handler_two_scalar_equal(left: f32, right: f32) -> bool {
    if left.is_nan() || right.is_nan() {
        return false;
    }
    let left_bits = left.to_bits();
    let right_bits = right.to_bits();
    left_bits == right_bits || (is_f32_zero_bits(left_bits) && is_f32_zero_bits(right_bits))
}

fn is_f32_zero_bits(bits: u32) -> bool {
    matches!(bits, 0 | 0x8000_0000)
}

impl ScriptInstruction {
    /// Returns the recovered dispatch selector from the first disk word.
    #[must_use]
    pub fn dispatch_selector(&self) -> ScriptDispatchSelector {
        match self.header_words[0] {
            value if value < GOG_HANDLER_COUNT =>
            {
                #[allow(clippy::cast_possible_truncation)]
                ScriptDispatchSelector::Handler(self.header_words[0] as u8)
            }
            u32::MAX => ScriptDispatchSelector::Sentinel,
            value => ScriptDispatchSelector::Unknown(value),
        }
    }
}

/// Decodes a compiled AI package using default safety limits.
///
/// # Errors
///
/// Returns a bounded [`DecodeError`] on truncated or oversized input.
pub fn decode(bytes: &[u8]) -> Result<ScriptPackage, DecodeError> {
    decode_with_limits(bytes, Limits::default())
}

/// Decodes a compiled AI package using explicit safety limits.
///
/// # Errors
///
/// Returns a bounded [`DecodeError`] on truncated or oversized input.
pub fn decode_with_limits(bytes: &[u8], limits: Limits) -> Result<ScriptPackage, DecodeError> {
    if u64::try_from(bytes.len()).map_err(|_| DecodeError::IntegerOverflow)? > limits.max_file_bytes
    {
        return Err(DecodeError::LimitExceeded {
            count: u64::try_from(bytes.len()).map_err(|_| DecodeError::IntegerOverflow)?,
            limit: limits.max_file_bytes,
        });
    }
    let mut cursor = Cursor::new(bytes);
    let opcode_handler_count = cursor.read_u32_le()?;
    let event_count = cursor.read_u32_le()?;
    checked_allocation_len(u64::from(event_count), u64::from(limits.max_entries))?;
    let mut events =
        Vec::with_capacity(usize::try_from(event_count).map_err(|_| DecodeError::IntegerOverflow)?);
    for _ in 0..event_count {
        events.push(read_event(&mut cursor, limits)?);
    }
    let trailing_bytes = cursor.read_exact(cursor.remaining())?.to_vec();
    Ok(ScriptPackage {
        opcode_handler_count,
        events,
        trailing_bytes,
        raw: Arc::from(bytes),
    })
}

fn read_event(cursor: &mut Cursor<'_>, limits: Limits) -> Result<ScriptEvent, DecodeError> {
    let name_len = cursor.read_u32_le()?;
    let name_bytes = u64::from(name_len)
        .checked_add(1)
        .ok_or(DecodeError::IntegerOverflow)?;
    let name_len_usize = checked_allocation_len(name_bytes, u64::from(limits.max_string_bytes))?;
    let name_raw = cursor.read_exact(name_len_usize)?.to_vec();
    if name_raw.last().copied() != Some(0) {
        return Err(DecodeError::Invalid(
            "script event name is not NUL terminated",
        ));
    }
    let event_word = cursor.read_u32_le()?;
    let instruction_count = cursor.read_u32_le()?;
    checked_allocation_len(u64::from(instruction_count), u64::from(limits.max_entries))?;
    let minimum = u64::from(instruction_count)
        .checked_mul(INSTRUCTION_HEADER_BYTES)
        .ok_or(DecodeError::IntegerOverflow)?;
    if minimum > u64::try_from(cursor.remaining()).map_err(|_| DecodeError::IntegerOverflow)? {
        return Err(DecodeError::UnexpectedEof {
            offset: cursor.offset(),
            needed: minimum,
            remaining: u64::try_from(cursor.remaining())
                .map_err(|_| DecodeError::IntegerOverflow)?,
        });
    }
    let mut instructions = Vec::with_capacity(
        usize::try_from(instruction_count).map_err(|_| DecodeError::IntegerOverflow)?,
    );
    for _ in 0..instruction_count {
        instructions.push(read_instruction(cursor, limits)?);
    }
    Ok(ScriptEvent {
        name_len,
        name_raw,
        event_word,
        instructions,
    })
}

fn read_instruction(
    cursor: &mut Cursor<'_>,
    limits: Limits,
) -> Result<ScriptInstruction, DecodeError> {
    let mut header_words = [0; INSTRUCTION_WORDS];
    for word in &mut header_words[..5] {
        *word = cursor.read_u32_le()?;
    }
    header_words[5] = cursor.read_u32_le()?;
    let reference_count = header_words[5];
    let reference_bytes = u64::from(reference_count)
        .checked_mul(4)
        .ok_or(DecodeError::IntegerOverflow)?;
    if reference_bytes
        > u64::try_from(cursor.remaining()).map_err(|_| DecodeError::IntegerOverflow)?
    {
        return Err(DecodeError::UnexpectedEof {
            offset: cursor.offset(),
            needed: reference_bytes,
            remaining: u64::try_from(cursor.remaining())
                .map_err(|_| DecodeError::IntegerOverflow)?,
        });
    }
    checked_allocation_len(
        u64::from(reference_count),
        u64::from(limits.max_array_items),
    )?;
    let mut references = Vec::with_capacity(
        usize::try_from(reference_count).map_err(|_| DecodeError::IntegerOverflow)?,
    );
    for _ in 0..reference_count {
        references.push(cursor.read_u32_le()?);
    }
    header_words[6] = cursor.read_u32_le()?;
    Ok(ScriptInstruction {
        header_words,
        references,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        decode, decode_with_limits, Handler2RecordInput, Handler2RecordScheduler,
        ScriptDispatchSelector, GOG_HANDLER_COUNT, INSTRUCTION_WORDS,
    };
    use fparkan_binary::{DecodeError, Limits};

    fn handler_two_input(
        scalar_1: f32,
        scalar_4: f32,
        scalar_5: f32,
        scalar_6: f32,
        word_2: u32,
    ) -> Handler2RecordInput {
        Handler2RecordInput {
            word_0: 7,
            scalar_1,
            word_2,
            word_3: 11,
            scalar_4,
            scalar_5,
            scalar_6,
        }
    }

    #[test]
    fn handler_two_scheduler_uses_three_word_bit_identity_and_refresh_contract() {
        let mut scheduler = Handler2RecordScheduler::default();
        let first = handler_two_input(1.5, -0.0, 3.0, 9.0, 4);
        assert_eq!(
            scheduler.submit(first),
            super::Handler2RecordUpdate {
                index: 0,
                created: true,
                refreshed: false,
            }
        );
        assert_eq!(scheduler.records().len(), 1);
        assert_eq!(scheduler.records()[0].counter, 4);

        let unchanged = handler_two_input(1.5, -0.0, 3.0, 12.0, 99);
        assert_eq!(
            scheduler.submit(unchanged),
            super::Handler2RecordUpdate {
                index: 0,
                created: false,
                refreshed: false,
            }
        );
        assert_eq!(scheduler.records()[0].counter, 4);
        assert_eq!(scheduler.records()[0].scalar_6.to_bits(), 9.0_f32.to_bits());

        let refreshed = handler_two_input(2.5, -0.0, 3.0, 12.0, 99);
        assert_eq!(
            scheduler.submit(refreshed),
            super::Handler2RecordUpdate {
                index: 0,
                created: false,
                refreshed: true,
            }
        );
        assert_eq!(scheduler.records()[0].counter, 103);
        assert_eq!(
            scheduler.records()[0].scalar_6.to_bits(),
            12.0_f32.to_bits()
        );

        let positive_zero_key = handler_two_input(2.5, 0.0, 3.0, 12.0, 1);
        assert_eq!(scheduler.submit(positive_zero_key).index, 1);
        assert_eq!(scheduler.records().len(), 2);
    }

    #[test]
    fn handler_two_scheduler_refreshes_nan_and_wraps_counter() {
        let mut scheduler = Handler2RecordScheduler::default();
        scheduler.submit(handler_two_input(f32::NAN, 1.0, 2.0, 3.0, u32::MAX));
        let update = scheduler.submit(handler_two_input(f32::NAN, 1.0, 2.0, 4.0, 2));
        assert_eq!(update.index, 0);
        assert!(update.refreshed);
        assert_eq!(scheduler.records()[0].counter, 1);
        assert_eq!(scheduler.records()[0].scalar_6.to_bits(), 4.0_f32.to_bits());
    }

    #[test]
    fn handler_two_scheduler_treats_signed_zero_value_as_unchanged() {
        let mut scheduler = Handler2RecordScheduler::default();
        scheduler.submit(handler_two_input(-0.0, 1.0, 2.0, 3.0, 5));
        let update = scheduler.submit(handler_two_input(0.0, 1.0, 2.0, 4.0, 9));
        assert!(!update.created);
        assert!(!update.refreshed);
        assert_eq!(scheduler.records()[0].counter, 5);
        assert_eq!(scheduler.records()[0].scalar_6.to_bits(), 3.0_f32.to_bits());
    }

    #[test]
    fn decodes_lossless_event_and_instruction_records() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&73_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.extend_from_slice(&4_u32.to_le_bytes());
        bytes.extend_from_slice(b"Init\0");
        bytes.extend_from_slice(&9_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        for word in [1_u32, 2, 3, 4, 5, 2] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        for reference in [7_u32, 8] {
            bytes.extend_from_slice(&reference.to_le_bytes());
        }
        bytes.extend_from_slice(&6_u32.to_le_bytes());
        bytes.extend_from_slice(&[0xaa, 0xbb]);

        let package = decode(&bytes).expect("valid script package");
        assert_eq!(package.opcode_handler_count, 73);
        assert_eq!(package.events.len(), 1);
        assert_eq!(package.events[0].name_raw, b"Init\0");
        assert_eq!(package.events[0].event_word, 9);
        assert_eq!(
            package.events[0].instructions[0].header_words,
            [1, 2, 3, 4, 5, 2, 6]
        );
        assert_eq!(package.events[0].instructions[0].references, [7, 8]);
        assert_eq!(
            package.events[0].instructions[0].dispatch_selector(),
            ScriptDispatchSelector::Handler(1)
        );
        assert_eq!(package.trailing_bytes, [0xaa, 0xbb]);
    }

    #[test]
    fn rejects_missing_event_nul_and_truncated_references() {
        let mut missing_nul = Vec::new();
        missing_nul.extend_from_slice(&0_u32.to_le_bytes());
        missing_nul.extend_from_slice(&1_u32.to_le_bytes());
        missing_nul.extend_from_slice(&1_u32.to_le_bytes());
        missing_nul.extend_from_slice(b"AB");
        assert_eq!(
            decode(&missing_nul),
            Err(DecodeError::Invalid(
                "script event name is not NUL terminated"
            ))
        );

        let mut truncated = Vec::new();
        truncated.extend_from_slice(&0_u32.to_le_bytes());
        truncated.extend_from_slice(&1_u32.to_le_bytes());
        truncated.extend_from_slice(&0_u32.to_le_bytes());
        truncated.push(0);
        truncated.extend_from_slice(&0_u32.to_le_bytes());
        truncated.extend_from_slice(&1_u32.to_le_bytes());
        for word in [0_u32, 0, 0, 0, 0, 1] {
            truncated.extend_from_slice(&word.to_le_bytes());
        }
        assert!(matches!(
            decode(&truncated),
            Err(DecodeError::UnexpectedEof { .. })
        ));
    }

    #[test]
    fn explicit_limits_bound_event_allocations() {
        let bytes = [0_u8; 8];
        let limits = Limits {
            max_entries: 0,
            ..Limits::default()
        };
        assert!(decode_with_limits(&bytes, limits).is_ok());

        let bytes = [0_u8, 0, 0, 0, 1, 0, 0, 0];
        assert!(matches!(
            decode_with_limits(&bytes, limits),
            Err(DecodeError::LimitExceeded { .. })
        ));
    }

    #[test]
    fn dispatch_selector_preserves_sentinel_and_unobserved_values() {
        let mut instruction = super::ScriptInstruction {
            header_words: [u32::MAX; INSTRUCTION_WORDS],
            references: Vec::new(),
        };
        assert_eq!(
            instruction.dispatch_selector(),
            ScriptDispatchSelector::Sentinel
        );
        instruction.header_words[0] = GOG_HANDLER_COUNT;
        assert_eq!(
            instruction.dispatch_selector(),
            ScriptDispatchSelector::Unknown(GOG_HANDLER_COUNT)
        );
    }
}
