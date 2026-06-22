#![forbid(unsafe_code)]
//! Bounded little-endian binary cursor and checked layout helpers.

use std::fmt;

/// SHA-256 digest bytes.
pub type Sha256Digest = [u8; 32];

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

/// Computes a SHA-256 content digest without external dependencies.
#[must_use]
pub fn sha256(bytes: &[u8]) -> Sha256Digest {
    const K: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];
    let mut h = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];

    let bit_len = (bytes.len() as u64).wrapping_mul(8);
    let mut chunks = bytes.chunks_exact(64);
    for chunk in &mut chunks {
        compress_sha256_chunk(&mut h, chunk, &K);
    }

    let tail = chunks.remainder();
    let mut block = [0u8; 128];
    block[..tail.len()].copy_from_slice(tail);
    block[tail.len()] = 0x80;
    let padded_len = if tail.len() < 56 { 64 } else { 128 };
    block[padded_len - 8..padded_len].copy_from_slice(&bit_len.to_be_bytes());
    for chunk in block[..padded_len].chunks_exact(64) {
        compress_sha256_chunk(&mut h, chunk, &K);
    }

    let mut out = [0u8; 32];
    for (idx, word) in h.iter().enumerate() {
        out[idx * 4..idx * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// Renders a SHA-256 digest as lowercase hexadecimal.
#[must_use]
pub fn sha256_hex(digest: &Sha256Digest) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}

#[allow(clippy::many_single_char_names)]
fn compress_sha256_chunk(h: &mut [u32; 8], chunk: &[u8], k: &[u32; 64]) {
    let mut w = [0u32; 64];
    for (idx, word) in w.iter_mut().take(16).enumerate() {
        let base = idx * 4;
        *word = u32::from_be_bytes([
            chunk[base],
            chunk[base + 1],
            chunk[base + 2],
            chunk[base + 3],
        ]);
    }
    for idx in 16..64 {
        let s0 = w[idx - 15].rotate_right(7) ^ w[idx - 15].rotate_right(18) ^ (w[idx - 15] >> 3);
        let s1 = w[idx - 2].rotate_right(17) ^ w[idx - 2].rotate_right(19) ^ (w[idx - 2] >> 10);
        w[idx] = w[idx - 16]
            .wrapping_add(s0)
            .wrapping_add(w[idx - 7])
            .wrapping_add(s1);
    }

    let mut a = h[0];
    let mut b = h[1];
    let mut c = h[2];
    let mut d = h[3];
    let mut e = h[4];
    let mut f = h[5];
    let mut g = h[6];
    let mut hh = h[7];

    for idx in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ ((!e) & g);
        let temp1 = hh
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(k[idx])
            .wrapping_add(w[idx]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let temp2 = s0.wrapping_add(maj);

        hh = g;
        g = f;
        f = e;
        e = d.wrapping_add(temp1);
        d = c;
        c = b;
        b = a;
        a = temp1.wrapping_add(temp2);
    }

    h[0] = h[0].wrapping_add(a);
    h[1] = h[1].wrapping_add(b);
    h[2] = h[2].wrapping_add(c);
    h[3] = h[3].wrapping_add(d);
    h[4] = h[4].wrapping_add(e);
    h[5] = h[5].wrapping_add(f);
    h[6] = h[6].wrapping_add(g);
    h[7] = h[7].wrapping_add(hh);
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

    #[test]
    fn sha256_matches_known_vectors() {
        assert_eq!(
            sha256_hex(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
