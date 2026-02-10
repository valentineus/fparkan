use super::xor::XorState;
use crate::error::Error;
use crate::Result;

/// Simple LZSS decompression with optional on-the-fly XOR decryption
pub fn lzss_decompress_simple(
    data: &[u8],
    expected_size: usize,
    xor_key: Option<u16>,
) -> Result<Vec<u8>> {
    let mut ring = [0x20u8; 0x1000];
    let mut ring_pos = 0xFEEusize;
    let mut out = Vec::with_capacity(expected_size);
    let mut in_pos = 0usize;

    let mut control = 0u8;
    let mut bits_left = 0u8;

    // XOR state for on-the-fly decryption
    let mut xor_state = xor_key.map(XorState::new);

    // Helper to read byte with optional XOR decryption
    let read_byte = |pos: usize, state: &mut Option<XorState>| -> Option<u8> {
        let encrypted = data.get(pos).copied()?;
        Some(if let Some(ref mut s) = state {
            s.decrypt_byte(encrypted)
        } else {
            encrypted
        })
    };

    while out.len() < expected_size {
        if bits_left == 0 {
            let byte = read_byte(in_pos, &mut xor_state)
                .ok_or(Error::DecompressionFailed("lzss-simple: unexpected EOF"))?;
            control = byte;
            in_pos += 1;
            bits_left = 8;
        }

        if (control & 1) != 0 {
            let byte = read_byte(in_pos, &mut xor_state)
                .ok_or(Error::DecompressionFailed("lzss-simple: unexpected EOF"))?;
            in_pos += 1;

            out.push(byte);
            ring[ring_pos] = byte;
            ring_pos = (ring_pos + 1) & 0x0FFF;
        } else {
            let low = read_byte(in_pos, &mut xor_state)
                .ok_or(Error::DecompressionFailed("lzss-simple: unexpected EOF"))?;
            let high = read_byte(in_pos + 1, &mut xor_state)
                .ok_or(Error::DecompressionFailed("lzss-simple: unexpected EOF"))?;
            in_pos += 2;

            let offset = usize::from(low) | (usize::from(high & 0xF0) << 4);
            let length = usize::from((high & 0x0F) + 3);

            for step in 0..length {
                let byte = ring[(offset + step) & 0x0FFF];
                out.push(byte);
                ring[ring_pos] = byte;
                ring_pos = (ring_pos + 1) & 0x0FFF;
                if out.len() >= expected_size {
                    break;
                }
            }
        }

        control >>= 1;
        bits_left -= 1;
    }

    if out.len() != expected_size {
        return Err(Error::DecompressionFailed("lzss-simple"));
    }

    Ok(out)
}
