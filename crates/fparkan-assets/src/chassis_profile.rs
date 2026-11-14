use fparkan_binary::{checked_count_bytes, read_lp_bytes, Cursor, DecodeError};

/// Reads the bound `CanFly` boolean from a native chassis VAR profile.
///
/// # Errors
/// Returns an error if the profile framing is invalid.
pub fn decode_chassis_can_fly(bytes: &[u8]) -> Result<Option<bool>, DecodeError> {
    let mut cursor = Cursor::new(bytes);
    let count = cursor.read_u32_le()?;
    checked_count_bytes(u64::from(count), 28, cursor.remaining() as u64)?;
    let mut can_fly = None;
    for _ in 0..count {
        let size = cursor.read_u32_le()?;
        let kind = cursor.read_u32_le()?;
        cursor.read_u32_le()?;
        let name = read_lp_bytes(&mut cursor, 64 * 1024)?;
        read_lp_bytes(&mut cursor, 64 * 1024)?;
        cursor.read_exact(size as usize)?;
        if can_fly.is_none()
            && name.split(|byte| *byte == 0).next() == Some(b"CanFly")
            && size == 4
            && kind == 2
        {
            can_fly = Some(cursor.read_u32_le()? == 1);
        } else {
            cursor.read_exact(size as usize)?;
        }
        cursor.read_exact(8)?;
    }
    cursor.require_eof()?;
    Ok(can_fly)
}

#[cfg(test)]
mod tests {
    use super::decode_chassis_can_fly;

    #[test]
    fn uses_the_bound_boolean_instead_of_its_default() {
        let mut profile = Vec::new();
        for word in [1_u32, 4, 2, 0, 6] {
            profile.extend(word.to_le_bytes());
        }
        profile.extend(b"CanFly");
        for word in [0_u32, 0, 1, 0, 1] {
            profile.extend(word.to_le_bytes());
        }
        assert_eq!(decode_chassis_can_fly(&profile), Ok(Some(true)));
        profile.truncate(profile.len() - 1);
        assert!(decode_chassis_can_fly(&profile).is_err());
    }
}
