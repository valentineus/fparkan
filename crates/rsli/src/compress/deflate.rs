use crate::error::Error;
use crate::Result;
use flate2::read::{DeflateDecoder, ZlibDecoder};
use std::io::Read;

/// Decode Deflate or Zlib compressed data
pub fn decode_deflate(packed: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut decoder = DeflateDecoder::new(packed);
    if decoder.read_to_end(&mut out).is_ok() {
        return Ok(out);
    }

    out.clear();
    let mut zlib = ZlibDecoder::new(packed);
    zlib.read_to_end(&mut out)
        .map_err(|_| Error::DecompressionFailed("deflate"))?;
    Ok(out)
}
