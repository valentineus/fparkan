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
    use super::{decode, decode_with_limits};
    use fparkan_binary::{DecodeError, Limits};

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
}
