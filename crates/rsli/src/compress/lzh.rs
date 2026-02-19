use super::xor::XorState;
use crate::error::Error;
use crate::Result;

pub(crate) const LZH_N: usize = 4096;
pub(crate) const LZH_F: usize = 60;
pub(crate) const LZH_THRESHOLD: usize = 2;
pub(crate) const LZH_N_CHAR: usize = 256 - LZH_THRESHOLD + LZH_F;
pub(crate) const LZH_T: usize = LZH_N_CHAR * 2 - 1;
pub(crate) const LZH_R: usize = LZH_T - 1;
pub(crate) const LZH_MAX_FREQ: u16 = 0x8000;

/// LZSS-Huffman decompression with optional on-the-fly XOR decryption.
pub fn lzss_huffman_decompress(
    data: &[u8],
    expected_size: usize,
    xor_key: Option<u16>,
) -> Result<Vec<u8>> {
    let mut decoder = LzhDecoder::new(data, xor_key);
    decoder.decode(expected_size)
}

struct LzhDecoder<'a> {
    bit_reader: BitReader<'a>,
    text: [u8; LZH_N],
    freq: [u16; LZH_T + 1],
    parent: [usize; LZH_T + LZH_N_CHAR],
    son: [usize; LZH_T],
    d_code: [u8; 256],
    d_len: [u8; 256],
    ring_pos: usize,
}

impl<'a> LzhDecoder<'a> {
    fn new(data: &'a [u8], xor_key: Option<u16>) -> Self {
        let mut decoder = Self {
            bit_reader: BitReader::new(data, xor_key),
            text: [0x20u8; LZH_N],
            freq: [0u16; LZH_T + 1],
            parent: [0usize; LZH_T + LZH_N_CHAR],
            son: [0usize; LZH_T],
            d_code: [0u8; 256],
            d_len: [0u8; 256],
            ring_pos: LZH_N - LZH_F,
        };
        decoder.init_tables();
        decoder.start_huff();
        decoder
    }

    fn decode(&mut self, expected_size: usize) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(expected_size);

        while out.len() < expected_size {
            let c = self.decode_char()?;
            if c < 256 {
                let byte = c as u8;
                out.push(byte);
                self.text[self.ring_pos] = byte;
                self.ring_pos = (self.ring_pos + 1) & (LZH_N - 1);
            } else {
                let mut offset = self.decode_position()?;
                offset = (self.ring_pos.wrapping_sub(offset).wrapping_sub(1)) & (LZH_N - 1);
                let mut length = c.saturating_sub(253);

                while length > 0 && out.len() < expected_size {
                    let byte = self.text[offset];
                    out.push(byte);
                    self.text[self.ring_pos] = byte;
                    self.ring_pos = (self.ring_pos + 1) & (LZH_N - 1);
                    offset = (offset + 1) & (LZH_N - 1);
                    length -= 1;
                }
            }
        }

        if out.len() != expected_size {
            return Err(Error::DecompressionFailed("lzss-huffman"));
        }
        Ok(out)
    }

    fn init_tables(&mut self) {
        let d_code_group_counts = [1usize, 3, 8, 12, 24, 16];
        let d_len_group_counts = [32usize, 48, 64, 48, 48, 16];

        let mut group_index = 0u8;
        let mut idx = 0usize;
        let mut run = 32usize;
        for count in d_code_group_counts {
            for _ in 0..count {
                for _ in 0..run {
                    self.d_code[idx] = group_index;
                    idx += 1;
                }
                group_index = group_index.wrapping_add(1);
            }
            run >>= 1;
        }

        let mut len = 3u8;
        idx = 0;
        for count in d_len_group_counts {
            for _ in 0..count {
                self.d_len[idx] = len;
                idx += 1;
            }
            len = len.saturating_add(1);
        }
    }

    fn start_huff(&mut self) {
        for i in 0..LZH_N_CHAR {
            self.freq[i] = 1;
            self.son[i] = i + LZH_T;
            self.parent[i + LZH_T] = i;
        }

        let mut i = 0usize;
        let mut j = LZH_N_CHAR;
        while j <= LZH_R {
            self.freq[j] = self.freq[i].saturating_add(self.freq[i + 1]);
            self.son[j] = i;
            self.parent[i] = j;
            self.parent[i + 1] = j;
            i += 2;
            j += 1;
        }

        self.freq[LZH_T] = u16::MAX;
        self.parent[LZH_R] = 0;
    }

    fn decode_char(&mut self) -> Result<usize> {
        let mut node = self.son[LZH_R];
        while node < LZH_T {
            let bit = usize::from(self.bit_reader.read_bit()?);
            let branch = node
                .checked_add(bit)
                .ok_or(Error::DecompressionFailed("lzss-huffman tree overflow"))?;
            node = *self.son.get(branch).ok_or(Error::DecompressionFailed(
                "lzss-huffman tree out of bounds",
            ))?;
        }

        let c = node - LZH_T;
        self.update(c);
        Ok(c)
    }

    fn decode_position(&mut self) -> Result<usize> {
        let i = self.bit_reader.read_bits(8)? as usize;
        let mut c = usize::from(self.d_code[i]) << 6;
        let mut j = usize::from(self.d_len[i]).saturating_sub(2);

        while j > 0 {
            j -= 1;
            c |= usize::from(self.bit_reader.read_bit()?) << j;
        }

        Ok(c | (i & 0x3F))
    }

    fn update(&mut self, c: usize) {
        if self.freq[LZH_R] == LZH_MAX_FREQ {
            self.reconstruct();
        }

        let mut current = self.parent[c + LZH_T];
        loop {
            self.freq[current] = self.freq[current].saturating_add(1);
            let freq = self.freq[current];

            if current + 1 < self.freq.len() && freq > self.freq[current + 1] {
                let mut swap_idx = current + 1;
                while swap_idx + 1 < self.freq.len() && freq > self.freq[swap_idx + 1] {
                    swap_idx += 1;
                }

                self.freq.swap(current, swap_idx);

                let left = self.son[current];
                let right = self.son[swap_idx];
                self.son[current] = right;
                self.son[swap_idx] = left;

                self.parent[left] = swap_idx;
                if left < LZH_T {
                    self.parent[left + 1] = swap_idx;
                }

                self.parent[right] = current;
                if right < LZH_T {
                    self.parent[right + 1] = current;
                }

                current = swap_idx;
            }

            current = self.parent[current];
            if current == 0 {
                break;
            }
        }
    }

    fn reconstruct(&mut self) {
        let mut j = 0usize;
        for i in 0..LZH_T {
            if self.son[i] >= LZH_T {
                self.freq[j] = (self.freq[i].saturating_add(1)) / 2;
                self.son[j] = self.son[i];
                j += 1;
            }
        }

        let mut i = 0usize;
        let mut current = LZH_N_CHAR;
        while current < LZH_T {
            let sum = self.freq[i].saturating_add(self.freq[i + 1]);
            self.freq[current] = sum;

            let mut insert_at = current;
            while insert_at > 0 && sum < self.freq[insert_at - 1] {
                insert_at -= 1;
            }

            for move_idx in (insert_at..current).rev() {
                self.freq[move_idx + 1] = self.freq[move_idx];
                self.son[move_idx + 1] = self.son[move_idx];
            }

            self.freq[insert_at] = sum;
            self.son[insert_at] = i;

            i += 2;
            current += 1;
        }

        for idx in 0..LZH_T {
            let node = self.son[idx];
            self.parent[node] = idx;
            if node < LZH_T {
                self.parent[node + 1] = idx;
            }
        }

        self.freq[LZH_T] = u16::MAX;
        self.parent[LZH_R] = 0;
    }
}

struct BitReader<'a> {
    data: &'a [u8],
    byte_pos: usize,
    bit_mask: u8,
    current_byte: u8,
    xor_state: Option<XorState>,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8], xor_key: Option<u16>) -> Self {
        Self {
            data,
            byte_pos: 0,
            bit_mask: 0x80,
            current_byte: 0,
            xor_state: xor_key.map(XorState::new),
        }
    }

    fn read_bit(&mut self) -> Result<u8> {
        if self.bit_mask == 0x80 {
            let Some(mut byte) = self.data.get(self.byte_pos).copied() else {
                return Err(Error::DecompressionFailed("lzss-huffman: unexpected EOF"));
            };
            if let Some(state) = &mut self.xor_state {
                byte = state.decrypt_byte(byte);
            }
            self.current_byte = byte;
        }

        let bit = if (self.current_byte & self.bit_mask) != 0 {
            1
        } else {
            0
        };
        self.bit_mask >>= 1;
        if self.bit_mask == 0 {
            self.bit_mask = 0x80;
            self.byte_pos = self.byte_pos.saturating_add(1);
        }
        Ok(bit)
    }

    fn read_bits(&mut self, bits: usize) -> Result<u32> {
        let mut value = 0u32;
        for _ in 0..bits {
            value = (value << 1) | u32::from(self.read_bit()?);
        }
        Ok(value)
    }
}
