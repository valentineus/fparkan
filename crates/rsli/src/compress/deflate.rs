use crate::error::Error;
use crate::Result;
use flate2::read::DeflateDecoder;
use std::io::Read;

/// Decode raw Deflate (RFC 1951) payload.
pub fn decode_deflate(packed: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut decoder = DeflateDecoder::new(packed);
    decoder
        .read_to_end(&mut out)
        .map_err(|_| Error::DecompressionFailed("deflate"))?;
    Ok(out)
}
