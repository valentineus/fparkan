use crate::{
    control_device_provider_value_from_status, EffectiveControlDevice,
    MissionControlDeviceTableState, MissionControlDeviceTableStatus,
    MissionControlNodeProviderTableStatus, ObjectHandle,
};

pub(super) fn unit_strength(
    tables: &[MissionControlDeviceTableState],
    handle: ObjectHandle,
) -> f32 {
    let Some(table) = tables.iter().find(|table| table.handle == handle) else {
        return 0.0;
    };
    let capacity = table.node_provider_amount_bits.map_or(0.0, f32::from_bits);
    let MissionControlDeviceTableStatus::Available(devices) = &table.status else {
        return rating(capacity, 0.0);
    };
    // shortcut: assume full design ammunition and use the live Control amount
    // until selector 0x36 scaling and live weapon counters are wired.
    rating(capacity, weapon_power(devices, &table.node_provider_status))
}

pub(super) fn weapon_power(
    devices: &[EffectiveControlDevice],
    providers: &MissionControlNodeProviderTableStatus,
) -> f32 {
    let mut grouped = false;
    let mut weapon_power = 0.0;
    for device in devices {
        match device.descriptor.tag {
            1 => grouped = true,
            2 if grouped && device.factory_tag == 2 => {
                let (Some(coefficients), Some(volume)) =
                    (device.coefficient_bits, device.device174_bits)
                else {
                    continue;
                };
                if control_device_provider_value_from_status(providers, device)
                    .is_ok_and(|health| health == 0.0)
                {
                    continue;
                }
                let rounds = f32::from_bits(coefficients[0]).round_ties_even() as i32 as u32;
                let volume = f32::from_bits(volume);
                if rounds > 0 && volume.is_finite() {
                    weapon_power += (volume / rounds as f32) * rounds as f32;
                }
            }
            _ => {}
        }
    }
    weapon_power
}

pub(super) fn rating(capacity: f32, weapon_power: f32) -> f32 {
    ((weapon_power + 0.8_f32) * capacity) * 0.00001_f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strength_scales_active_weapon_power_by_remaining_capacity() {
        assert_eq!(rating(200_000.0, 3.2), 8.0);
        assert_eq!(rating(100_000.0, 3.2), 4.0);
        assert_eq!(rating(0.0, 3.2), 0.0);
    }
}
