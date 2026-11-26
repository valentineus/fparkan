//! Effect references carried by the version-1 explosion descriptor.

/// Reads the initial effect archive and name from a version-1 EXPL payload.
/// Other versions and malformed references require separate interpretation.
pub fn decode_explosion_effect(bytes: &[u8]) -> Option<(&str, &str)> {
    if bytes.len() != 792 || bytes[..4] != 1_u32.to_le_bytes() {
        return None;
    }
    Some((
        fixed_name(&bytes[0x18..0x38])?,
        fixed_name(&bytes[0x38..0x58])?,
    ))
}

fn fixed_name(bytes: &[u8]) -> Option<&str> {
    let name = bytes.split(|&byte| byte == 0).next()?;
    (name.is_ascii() && !name.is_empty())
        .then(|| std::str::from_utf8(name).ok())
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_version_one_reference_and_rejects_unknown_layouts() {
        let mut bytes = [0; 792];
        bytes[..4].copy_from_slice(&1_u32.to_le_bytes());
        bytes[0x18..0x1d].copy_from_slice(b"a.rlb");
        bytes[0x38..0x3d].copy_from_slice(b"burst");
        assert_eq!(decode_explosion_effect(&bytes), Some(("a.rlb", "burst")));
        assert_eq!(decode_explosion_effect(&bytes[..791]), None);
        bytes[0] = 2;
        assert_eq!(decode_explosion_effect(&bytes), None);
    }
}
