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

pub(super) fn decode_pair(
    bytes: &[u8],
    names: [&'static str; 2],
) -> Result<[u32; 2], BehaviorProfileVarError> {
    let mut cursor = Cursor::new(bytes);
    let count = cursor.read_u32_le()?;
    checked_count_bytes(u64::from(count), 28, cursor.remaining() as u64)?;
    let mut found = [None, None];
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
    Ok([
        found[0].ok_or(BehaviorProfileVarError::MissingField { name: names[0] })?,
        found[1].ok_or(BehaviorProfileVarError::MissingField { name: names[1] })?,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn work_rates_use_the_applied_second_blob() {
        let mut bytes = 2_u32.to_le_bytes().to_vec();
        for (name, first, second) in [("Use_Power", 7.0_f32, 3.0_f32), ("Use_Ore", 9.0, 1.0)] {
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
        assert_eq!(
            decode_behavior_work_rates(&bytes).unwrap(),
            BehaviorWorkRates {
                power_bits: 3.0_f32.to_bits(),
                ore_bits: 1.0_f32.to_bits()
            }
        );
    }
}
