/// XOR cipher state for RsLi format
pub struct XorState {
    lo: u8,
    hi: u8,
}

impl XorState {
    /// Create new XOR state from 16-bit key
    pub fn new(key16: u16) -> Self {
        Self {
            lo: (key16 & 0xFF) as u8,
            hi: ((key16 >> 8) & 0xFF) as u8,
        }
    }

    /// Decrypt a single byte and update state
    pub fn decrypt_byte(&mut self, encrypted: u8) -> u8 {
        self.lo = self.hi ^ self.lo.wrapping_shl(1);
        let decrypted = encrypted ^ self.lo;
        self.hi = self.lo ^ (self.hi >> 1);
        decrypted
    }
}

/// Decrypt entire buffer with XOR stream cipher
pub fn xor_stream(data: &[u8], key16: u16) -> Vec<u8> {
    let mut state = XorState::new(key16);
    data.iter().map(|&b| state.decrypt_byte(b)).collect()
}
