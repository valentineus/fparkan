#![forbid(unsafe_code)]
//! Bounded little-endian binary cursor and checked layout helpers.

use std::fmt;

/// Parser limits shared by binary formats.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Maximum file bytes.
    pub max_file_bytes: u64,
    /// Maximum entries.
    pub max_entries: u32,
    /// Maximum string bytes.
    pub max_string_bytes: u32,
    /// Maximum array items.
    pub max_array_items: u32,
    /// Maximum recursion depth.
    pub max_recursion_depth: u16,
    /// Maximum decoded bytes.
    pub max_decoded_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_file_bytes: 256 * 1024 * 1024,
            max_entries: 1_000_000,
            max_string_bytes: 64 * 1024,
            max_array_items: 1_000_000,
            max_recursion_depth: 64,
            max_decoded_bytes: 512 * 1024 * 1024,
        }
    }
}

/// Decode error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DecodeError {
    /// Input ended before requested bytes.
    UnexpectedEof {
        /// Offset where read was attempted.
        offset: u64,
        /// Required byte count.
        needed: u64,
        /// Remaining byte count.
        remaining: u64,
    },
    /// Arithmetic overflow.
    IntegerOverflow,
    /// Count exceeds limit.
    LimitExceeded {
        /// Declared count.
        count: u64,
        /// Configured limit.
        limit: u64,
    },
    /// Cursor did not end at EOF.
    TrailingBytes {
        /// Offset where EOF was expected.
        offset: u64,
        /// Remaining byte count.
        remaining: u64,
    },
    /// Invalid data.
    Invalid(&'static str),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEof {
                offset,
                needed,
                remaining,
            } => write!(
                f,
                "unexpected EOF at {offset}: need {needed}, have {remaining}"
            ),
            Self::IntegerOverflow => write!(f, "integer overflow"),
            Self::LimitExceeded { count, limit } => {
                write!(f, "count {count} exceeds limit {limit}")
            }
            Self::TrailingBytes { offset, remaining } => {
                write!(f, "trailing bytes at {offset}: {remaining}")
            }
            Self::Invalid(reason) => write!(f, "invalid data: {reason}"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Cursor checkpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Checkpoint(pub u64);

/// Bounded cursor.
#[derive(Clone, Debug)]
pub struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    /// Creates a cursor.
    #[must_use]
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    /// Current offset.
    #[must_use]
    pub fn offset(&self) -> u64 {
        self.offset as u64
    }

    /// Remaining bytes.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    /// Creates a checkpoint.
    #[must_use]
    pub fn checkpoint(&self) -> Checkpoint {
        Checkpoint(self.offset())
    }

    /// Reads exact bytes.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError::IntegerOverflow`] if the requested end offset
    /// overflows, or [`DecodeError::UnexpectedEof`] if there are not enough
    /// bytes remaining.
    pub fn read_exact(&mut self, len: usize) -> Result<&'a [u8], DecodeError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or(DecodeError::IntegerOverflow)?;
        if end > self.bytes.len() {
            return Err(DecodeError::UnexpectedEof {
                offset: self.offset(),
                needed: len as u64,
                remaining: self.remaining() as u64,
            });
        }
        let out = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(out)
    }

    /// Reads a little-endian u16.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError`] if two bytes cannot be read.
    pub fn read_u16_le(&mut self) -> Result<u16, DecodeError> {
        let b = self.read_exact(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    /// Reads a little-endian u32.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError`] if four bytes cannot be read.
    pub fn read_u32_le(&mut self) -> Result<u32, DecodeError> {
        let b = self.read_exact(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Reads a little-endian i32.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError`] if four bytes cannot be read.
    pub fn read_i32_le(&mut self) -> Result<i32, DecodeError> {
        let b = self.read_exact(4)?;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Reads a little-endian f32.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError`] if four bytes cannot be read.
    pub fn read_f32_le(&mut self) -> Result<f32, DecodeError> {
        Ok(f32::from_bits(self.read_u32_le()?))
    }

    /// Requires exact EOF.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError::TrailingBytes`] when unread bytes remain.
    pub fn require_eof(&self) -> Result<(), DecodeError> {
        if self.remaining() == 0 {
            Ok(())
        } else {
            Err(DecodeError::TrailingBytes {
                offset: self.offset(),
                remaining: self.remaining() as u64,
            })
        }
    }
}

/// Validates `count * stride <= remaining` and returns bytes as usize.
///
/// # Errors
///
/// Returns [`DecodeError::IntegerOverflow`] on arithmetic or conversion
/// overflow, or [`DecodeError::UnexpectedEof`] when the declared byte count is
/// larger than the remaining bounded input.
pub fn checked_count_bytes(count: u64, stride: u64, remaining: u64) -> Result<usize, DecodeError> {
    let bytes = count
        .checked_mul(stride)
        .ok_or(DecodeError::IntegerOverflow)?;
    if bytes > remaining {
        return Err(DecodeError::UnexpectedEof {
            offset: 0,
            needed: bytes,
            remaining,
        });
    }
    usize::try_from(bytes).map_err(|_| DecodeError::IntegerOverflow)
}

/// Validates a declared allocation size before constructing the allocation.
///
/// # Errors
///
/// Returns [`DecodeError::LimitExceeded`] when `declared` is larger than
/// `limit`, or [`DecodeError::IntegerOverflow`] when the accepted size cannot
/// be represented by the host `usize`.
pub fn checked_allocation_len(declared: u64, limit: u64) -> Result<usize, DecodeError> {
    if declared > limit {
        return Err(DecodeError::LimitExceeded {
            count: declared,
            limit,
        });
    }
    usize::try_from(declared).map_err(|_| DecodeError::IntegerOverflow)
}

/// Reads length-prefixed bytes.
///
/// # Errors
///
/// Returns [`DecodeError`] if the length cannot be read, exceeds `max`, or the
/// declared payload is truncated.
pub fn read_lp_bytes(cursor: &mut Cursor<'_>, max: u32) -> Result<Vec<u8>, DecodeError> {
    let len = cursor.read_u32_le()?;
    if len > max {
        return Err(DecodeError::LimitExceeded {
            count: u64::from(len),
            limit: u64::from(max),
        });
    }
    let len = checked_allocation_len(u64::from(len), u64::from(max))?;
    Ok(cursor.read_exact(len)?.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_count_stride_overflow() {
        assert_eq!(
            checked_count_bytes(u64::MAX, 2, u64::MAX),
            Err(DecodeError::IntegerOverflow)
        );
    }

    #[test]
    fn exact_eof_reports_trailing() {
        let mut cursor = Cursor::new(&[1, 2]);
        assert_eq!(cursor.read_exact(1).expect("byte"), &[1]);
        assert!(matches!(
            cursor.require_eof(),
            Err(DecodeError::TrailingBytes { .. })
        ));
    }

    #[test]
    fn rejects_oversized_declared_allocation_before_read() {
        assert_eq!(
            checked_allocation_len(1025, 1024),
            Err(DecodeError::LimitExceeded {
                count: 1025,
                limit: 1024
            })
        );

        let bytes = 2048u32.to_le_bytes();
        let mut cursor = Cursor::new(&bytes);
        assert_eq!(
            read_lp_bytes(&mut cursor, 1024),
            Err(DecodeError::LimitExceeded {
                count: 2048,
                limit: 1024
            })
        );
        assert_eq!(cursor.offset(), 4);
    }
}
