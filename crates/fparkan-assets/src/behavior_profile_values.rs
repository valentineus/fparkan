use crate::BehaviorProfileVarError;
use fparkan_binary::{checked_count_bytes, read_lp_bytes, Cursor, DecodeError};

/// Work rates used by native factory and research tasks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BehaviorWorkRates {
    /// Applied `Use_Power` profile value.
    pub power_bits: u32,
    /// Applied `Use_Ore` profile value.
    pub ore_bits: u32,
}

/// Reads the applied power and ore work rates from a Behavior VAR profile.
///
/// # Errors
/// Rejects malformed framing, missing fields and selected non-float records.
pub fn decode_behavior_work_rates(
    bytes: &[u8],
) -> Result<BehaviorWorkRates, BehaviorProfileVarError> {
    let [power_bits, ore_bits] = decode_pair(bytes, ["Use_Power", "Use_Ore"])?;
    Ok(BehaviorWorkRates {
        power_bits,
        ore_bits,
    })
}

/// Reads the applied Go coefficient, retaining the native default when absent.
///
/// # Errors
/// Rejects malformed framing and a selected record that is not a float.
pub fn decode_behavior_go_speed_percent(bytes: &[u8]) -> Result<u32, BehaviorProfileVarError> {
    let [value] = decode_values(bytes, ["Go_SpeedPercent"])?;
    Ok(value.unwrap_or(1.0_f32.to_bits()))
}

pub(super) fn decode_pair(
    bytes: &[u8],
    names: [&'static str; 2],
) -> Result<[u32; 2], BehaviorProfileVarError> {
    let [first, second] = decode_values(bytes, names)?;
    Ok([
        first.ok_or(BehaviorProfileVarError::MissingField { name: names[0] })?,
        second.ok_or(BehaviorProfileVarError::MissingField { name: names[1] })?,
    ])
}

fn decode_values<const N: usize>(
    bytes: &[u8],
    names: [&'static str; N],
) -> Result<[Option<u32>; N], BehaviorProfileVarError> {
    let mut cursor = Cursor::new(bytes);
    let count = cursor.read_u32_le()?;
    checked_count_bytes(u64::from(count), 28, cursor.remaining() as u64)?;
    let mut found = [None; N];
    for _ in 0..count {
        let size = cursor.read_u32_le()?;
        let type_index = cursor.read_u32_le()?;
        cursor.read_u32_le()?;
        let raw_name = read_lp_bytes(&mut cursor, 64 * 1024)?;
        let name = raw_name.split(|&byte| byte == 0).next().unwrap_or_default();
        let target = names
            .iter()
            .position(|expected| expected.as_bytes() == name);
        read_lp_bytes(&mut cursor, 64 * 1024)?;
        let length = usize::try_from(size).map_err(|_| DecodeError::IntegerOverflow)?;
        cursor.read_exact(length)?;
        if let Some(index) = target.filter(|&index| found[index].is_none()) {
            if size != 4 || type_index != 3 {
                return Err(BehaviorProfileVarError::InvalidFloatField {
                    name: names[index],
                    size,
                    type_index,
                });
            }
            found[index] = Some(u32::from_le_bytes(
                cursor.read_exact(length)?.try_into().unwrap(),
            ));
        } else {
            cursor.read_exact(length)?;
        }
        cursor.read_exact(8)?;
    }
    cursor.require_eof()?;
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(fields: &[(&str, f32, f32)]) -> Vec<u8> {
        let mut bytes = (fields.len() as u32).to_le_bytes().to_vec();
        for &(name, first, second) in fields {
            for word in [4_u32, 3, 0] {
                bytes.extend(word.to_le_bytes());
            }
            for text in [name, "float"] {
                bytes.extend((text.len() as u32 + 1).to_le_bytes());
                bytes.extend(text.as_bytes());
                bytes.push(0);
            }
            bytes.extend(first.to_le_bytes());
            bytes.extend(second.to_le_bytes());
            bytes.extend([0; 8]);
        }
        bytes
    }

    #[test]
    fn work_rates_use_the_applied_second_blob() {
        let bytes = profile(&[("Use_Power", 7.0, 3.0), ("Use_Ore", 9.0, 1.0)]);
        assert_eq!(
            decode_behavior_work_rates(&bytes).unwrap(),
            BehaviorWorkRates {
                power_bits: 3.0_f32.to_bits(),
                ore_bits: 1.0_f32.to_bits()
            }
        );
    }

    #[test]
    fn go_speed_keeps_native_default_or_uses_the_first_rows_applied_value() {
        assert_eq!(
            decode_behavior_go_speed_percent(&profile(&[])),
            Ok(1.0_f32.to_bits())
        );
        let bytes = profile(&[
            ("Go_SpeedPercent", 0.8, 0.5),
            ("Go_SpeedPercent", 1.0, 0.25),
        ]);
        assert_eq!(
            decode_behavior_go_speed_percent(&bytes),
            Ok(0.5_f32.to_bits())
        );
    }
}
