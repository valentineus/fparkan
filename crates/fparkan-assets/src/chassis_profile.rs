use fparkan_binary::{checked_count_bytes, read_lp_bytes, Cursor, DecodeError};

/// Locomotion booleans bound by the chassis profile, when present.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChassisFlags {
    /// The chassis can fly.
    pub can_fly: Option<bool>,
    /// The chassis uses legs to move.
    pub walk_chassis: Option<bool>,
}

/// Reads the bound `CanFly` boolean from a native chassis VAR profile.
///
/// # Errors
/// Returns an error if the profile framing is invalid.
pub fn decode_chassis_can_fly(bytes: &[u8]) -> Result<Option<bool>, DecodeError> {
    Ok(decode_chassis_flags(bytes)?.can_fly)
}

/// Reads applied locomotion flags, keeping absent flags as unknown.
///
/// # Errors
/// Returns an error if the profile framing is invalid.
pub fn decode_chassis_flags(bytes: &[u8]) -> Result<ChassisFlags, DecodeError> {
    let mut cursor = Cursor::new(bytes);
    let count = cursor.read_u32_le()?;
    checked_count_bytes(u64::from(count), 28, cursor.remaining() as u64)?;
    let mut flags = ChassisFlags::default();
    for _ in 0..count {
        let size = cursor.read_u32_le()?;
        let kind = cursor.read_u32_le()?;
        cursor.read_u32_le()?;
        let name = read_lp_bytes(&mut cursor, 64 * 1024)?;
        read_lp_bytes(&mut cursor, 64 * 1024)?;
        cursor.read_exact(size as usize)?;
        if size == 4 && kind == 2 {
            let value = cursor.read_u32_le()? == 1;
            match name.split(|byte| *byte == 0).next().unwrap_or_default() {
                b"CanFly" => {
                    flags.can_fly.get_or_insert(value);
                }
                b"WalkChassis" => {
                    flags.walk_chassis.get_or_insert(value);
                }
                _ => {}
            }
        } else {
            cursor.read_exact(size as usize)?;
        }
        cursor.read_exact(8)?;
    }
    cursor.require_eof()?;
    Ok(flags)
}

#[cfg(test)]
mod tests {
    use super::{decode_chassis_can_fly, decode_chassis_flags, ChassisFlags};

    #[test]
    fn uses_the_bound_boolean_instead_of_its_default() {
        let mut profile = 2_u32.to_le_bytes().to_vec();
        for (name, value) in [("CanFly", 1), ("WalkChassis\0", 0)] {
            for word in [4_u32, 2, 0, name.len() as u32] {
                profile.extend(word.to_le_bytes());
            }
            profile.extend(name.as_bytes());
            for word in [0_u32, 1 - value, value, 0, 1] {
                profile.extend(word.to_le_bytes());
            }
        }
        assert_eq!(decode_chassis_can_fly(&profile), Ok(Some(true)));
        assert_eq!(
            decode_chassis_flags(&profile),
            Ok(ChassisFlags {
                can_fly: Some(true),
                walk_chassis: Some(false),
            })
        );
        profile.truncate(profile.len() - 1);
        assert!(decode_chassis_can_fly(&profile).is_err());
    }
}
