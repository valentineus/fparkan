//! Mobile Control device budgets and weapon charge snapshots.

use crate::{
    checked_control_float, checked_control_product, checked_control_ratio, checked_control_sum,
    control_budget_device, control_device_budget_active_provider_value,
    control_device_budget_coefficient, control_device_budget_descriptor_float,
    ControlResourceAccessError, EffectiveControlDevice, MissionControlDeviceTableState,
    MissionControlDeviceTableStatus,
};
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub(super) struct Charge {
    pub(super) capacity: f32,
    pub(super) cost: f32,
    pub(super) amount: f32,
}

impl Charge {
    pub(super) fn can_start(&self) -> bool {
        self.capacity <= 0.0 || self.cost <= self.amount
    }

    pub(super) fn finish_shot(&mut self) {
        if self.capacity > 0.0 && self.cost <= self.amount {
            self.amount -= self.cost;
        }
    }
}

/// Mobile budget steps, omitted motion inputs and energy waits, by original ID.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MobileBudgetReport {
    pub ticks: BTreeMap<u32, u64>,
    pub stubs: BTreeMap<(u32, String), u64>,
    pub weapon_waits: BTreeMap<u32, u64>,
}

impl MobileBudgetReport {
    pub(super) fn stub(&mut self, object: u32, reason: String) {
        let count = self.stubs.entry((object, reason.clone())).or_default();
        *count += 1;
        if *count == 1 {
            eprintln!("control_mobile_budget_stub object={object} reason={reason}");
        }
    }
}

fn charge_rate(
    amount: f32,
    capacity: f32,
    dt: f32,
    upkeep: f32,
    active: bool,
) -> Result<f32, ControlResourceAccessError> {
    let deficit = checked_control_sum(2, "weapon charge deficit", amount, -capacity)?;
    if active {
        checked_control_sum(
            2,
            "weapon charge and upkeep",
            deficit,
            -checked_control_product(2, "weapon upkeep interval", dt, upkeep)?,
        )
    } else {
        Ok(deficit)
    }
}

fn charge_update(
    amount: f32,
    capacity: f32,
    dt: f32,
    upkeep: f32,
    active: bool,
    allocation: f32,
) -> Result<(f32, Option<f32>), ControlResourceAccessError> {
    let rate = charge_rate(amount, capacity, dt, upkeep, active)?;
    let mut received = -checked_control_product(2, "weapon allocated charge", rate, allocation)?;
    if active {
        received = checked_control_sum(
            2,
            "weapon received minus upkeep",
            received,
            -checked_control_product(2, "weapon upkeep interval", dt, upkeep)?,
        )?;
    }
    if capacity <= 0.0 {
        return Ok((amount, None));
    }
    let amount = checked_control_sum(2, "weapon updated charge", received, amount)?;
    Ok((
        amount,
        Some(checked_control_ratio(
            "weapon charge ratio",
            amount,
            capacity,
        )?),
    ))
}

fn weapon_amount(device: &EffectiveControlDevice) -> Result<f32, ControlResourceAccessError> {
    let bits = device
        .weapon_charge_bits
        .ok_or(ControlResourceAccessError::UnsupportedField {
            factory_tag: 2,
            field: "weapon charge +0x120",
        })?;
    checked_control_float(2, "weapon charge", f32::from_bits(bits))
}

pub(super) fn weapon_rate(
    table: &MissionControlDeviceTableState,
    device: &EffectiveControlDevice,
    dt: f32,
) -> Result<f32, ControlResourceAccessError> {
    let active = control_device_budget_active_provider_value(table, device)?.is_some();
    charge_rate(
        weapon_amount(device)?,
        control_device_budget_coefficient(device, 1)?,
        dt,
        control_device_budget_descriptor_float(device, 8, "weapon upkeep")?,
        active,
    )
}

pub(super) fn weapon_update(
    table: &mut MissionControlDeviceTableState,
    index: usize,
    dt: f32,
    allocation: f32,
) -> Result<(), ControlResourceAccessError> {
    let device = control_budget_device(table, index)?;
    let active = control_device_budget_active_provider_value(table, device)?.is_some();
    let (amount, ratio) = charge_update(
        weapon_amount(device)?,
        control_device_budget_coefficient(device, 1)?,
        dt,
        control_device_budget_descriptor_float(device, 8, "weapon upkeep")?,
        active,
        allocation,
    )?;
    let MissionControlDeviceTableStatus::Available(devices) = &mut table.status else {
        unreachable!("checked table")
    };
    devices[index].weapon_charge_bits = Some(amount.to_bits());
    if let Some(ratio) = ratio {
        devices[index].multiplier_bits = Some(ratio.to_bits());
    }
    Ok(())
}

pub(super) fn passive_device_rate(
    device: &EffectiveControlDevice,
    dt: f32,
) -> Result<f32, ControlResourceAccessError> {
    if device.native_word_50_bits != Some(0x2000) {
        return Err(ControlResourceAccessError::UnsupportedField {
            factory_tag: 10,
            field: "active mode +0x50",
        });
    }
    Ok(-checked_control_product(
        10,
        "passive device upkeep",
        dt,
        control_device_budget_descriptor_float(device, 8, "passive device rate")?,
    )?)
}

pub(super) fn unmodeled_motion_rate() -> f32 {
    // shortcut: Control motion vectors and +0x154 are not supplied by the
    // temporary movement controller. Resources counts this omission per owner.
    0.0
}

pub(super) fn read(
    mission: &crate::LoadedMissionState,
    handle: crate::ObjectHandle,
    weapons: &mut [crate::player_weapon::PlayerWeapon],
) {
    let table = mission
        .control_device_tables
        .iter()
        .find(|table| table.handle == handle);
    let devices = table.and_then(|table| match &table.status {
        MissionControlDeviceTableStatus::Available(devices) => Some(devices),
        _ => None,
    });
    for weapon in weapons {
        weapon.charge = if mission.control_resources.energy_available(handle) {
            devices
                .and_then(|devices| devices.iter().find(|device| matches_weapon(device, weapon)))
                .and_then(|device| {
                    let coefficients = device.coefficient_bits?;
                    let charge = Charge {
                        capacity: f32::from_bits(coefficients[1]),
                        cost: f32::from_bits(coefficients[2]),
                        amount: f32::from_bits(device.weapon_charge_bits?),
                    };
                    [charge.capacity, charge.cost, charge.amount]
                        .into_iter()
                        .all(|value| value.is_finite() && !value.is_subnormal())
                        .then_some(charge)
                })
        } else {
            None
        };
    }
}

fn matches_weapon(
    device: &EffectiveControlDevice,
    weapon: &crate::player_weapon::PlayerWeapon,
) -> bool {
    device.factory_tag == 2
        && device.factory_provenance.unit_component_index == weapon.component_index
        && device.factory_provenance.typed_record_index == weapon.control_record_index
}

pub(super) fn publish(
    mission: &mut crate::LoadedMissionState,
    handle: crate::ObjectHandle,
    weapons: &mut [crate::player_weapon::PlayerWeapon],
) {
    if weapons.is_empty() {
        return;
    }
    let available = mission.control_resources.energy_available(handle);
    let Some(table) = mission
        .control_device_tables
        .iter_mut()
        .find(|table| table.handle == handle)
    else {
        mission
            .control_resources
            .mobile
            .stub(handle.slot, "weapon_owner_table".into());
        return;
    };
    let object = table.original_id.map_or(handle.slot, |id| id.0);
    let waits: u64 = weapons.iter().map(|weapon| weapon.energy_waits).sum();
    let total = mission
        .control_resources
        .mobile
        .weapon_waits
        .entry(object)
        .or_default();
    let previous = *total;
    *total += waits;
    if previous == 0 && waits > 0 {
        eprintln!("weapon_energy_wait object={object} samples={waits}");
    }
    for weapon in weapons {
        weapon.energy_waits = 0;
        let Some(charge) = &weapon.charge else {
            if available {
                mission.control_resources.mobile.stub(
                    object,
                    format!(
                        "weapon_link_{}_{}",
                        weapon.component_index, weapon.control_record_index
                    ),
                );
            }
            continue;
        };
        if let MissionControlDeviceTableStatus::Available(devices) = &mut table.status {
            if let Some(device) = devices
                .iter_mut()
                .find(|device| matches_weapon(device, weapon))
            {
                device.weapon_charge_bits = Some(charge.amount.to_bits());
                if charge.capacity > 0.0 {
                    device.multiplier_bits = Some((charge.amount / charge.capacity).to_bits());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_weapon_budget_refills_disabled_devices_and_can_go_negative() {
        // GOG Control.dll +0x29a40 / +0x29a90, x87 CW 0x007f.
        for (capacity, amount, dt, upkeep, allocation, active, rate, charge, multiplier) in [
            (
                10.0, 3.0, 0.25, 4.0, 0.5, true, 0xc1000000, 0x40c00000, 0x3f19999a,
            ),
            (
                10.0, 10.0, 0.25, 4.0, 0.5, true, 0xbf800000, 0x41180000, 0x3f733333,
            ),
            (
                10.0, 3.0, 0.25, 4.0, 0.5, false, 0xc0e00000, 0x40d00000, 0x3f266666,
            ),
            (
                10.0, 3.0, 0.25, 4.0, 0.0, true, 0xc1000000, 0x40000000, 0x3e4ccccd,
            ),
            (0.0, 0.0, 0.25, 4.0, 1.0, true, 0xbf800000, 0, 0x3e800000),
            (
                10.0, 0.5, 0.25, 4.0, 0.0, true, 0xc1280000, 0xbf000000, 0xbd4ccccd,
            ),
            (
                19.81, 0.23, 0.375491, 17.258, 0.234267, true, 0xc1d07b56, 0xbe14a841, 0xbbf02203,
            ),
        ] {
            let rate_value = charge_rate(amount, capacity, dt, upkeep, active).unwrap();
            assert_eq!(rate_value.to_bits(), rate);
            let (amount, ratio) =
                charge_update(amount, capacity, dt, upkeep, active, allocation).unwrap();
            assert_eq!(amount.to_bits(), charge);
            assert_eq!(ratio.unwrap_or(0.25).to_bits(), multiplier);
        }
    }

    #[test]
    fn charge_allows_equality_and_is_spent_after_a_completed_shot() {
        let mut charge = Charge {
            capacity: 10.0,
            cost: 4.0,
            amount: 8.0,
        };
        assert!(charge.can_start());
        charge.finish_shot();
        assert!(charge.can_start());
        charge.finish_shot();
        assert!(!charge.can_start());
        let unlimited = Charge {
            capacity: 0.0,
            cost: 4.0,
            amount: 0.0,
        };
        assert!(unlimited.can_start());
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_stock_lasers_wait_for_charge_and_recover_from_the_unit_budget() {
        use crate::{
            create, frame, load_gameplay_mission, EngineServices, MissionMode, MissionRequest,
            MissionStartupInput,
        };
        use fparkan_vfs::DirectoryVfs;
        use std::sync::Arc;
        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").unwrap();
        let mut engine = create(EngineServices::new(Arc::new(DirectoryVfs::new(root)))).unwrap();
        load_gameplay_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/Autodemo.00/data.tma".into(),
                mode: MissionMode::AutoDemo,
            },
            MissionStartupInput {
                queue_time_ms: 0,
                research_creation_gate: false,
            },
            None,
        )
        .unwrap();
        // Isolate a held trigger on an unchanged stock unit; this is not mission acceptance.
        engine.autodemo_combat = None;
        let mission = engine.loaded.as_ref().unwrap();
        let handle = mission.control_device_tables[1].handle;
        let mut weapons = crate::player_weapon::prepare_unit_weapons(
            &mission.mission_assets.object_component_controls[1],
        );
        let mut idle_bank = weapons.clone();
        assert_eq!(weapons.weapons().len(), 2);
        assert!(weapons
            .weapons()
            .iter()
            .all(|weapon| weapon.rounds_remaining().is_none()));
        let mut shots = 0;
        frame(&mut engine, 0).unwrap();
        for now in (50..=120_000).step_by(50) {
            frame(&mut engine, now).unwrap();
            weapons.read_control_energy(&engine, handle);
            shots += weapons.fire(true, now).count();
            weapons.publish_control_energy(&mut engine, handle);
            idle_bank.read_control_energy(&engine, handle);
            idle_bank.publish_control_energy(&mut engine, handle);
        }
        let report = &engine.loaded.as_ref().unwrap().control_resources.mobile;
        assert!(shots > 0);
        assert!(
            report.weapon_waits[&1] > 0,
            "expected depleted laser charge"
        );
        assert!(
            !report.stubs.keys().any(|(object, reason)| *object == 1
                && (reason.starts_with("budget_") || reason.starts_with("weapon_link"))),
            "{:?}",
            report.stubs
        );
        for now in (120_050..=140_000).step_by(50) {
            frame(&mut engine, now).unwrap();
            weapons.read_control_energy(&engine, handle);
            weapons.fire(false, now).count();
            weapons.publish_control_energy(&mut engine, handle);
        }
        let mut resumed = 0;
        for now in (140_050..=142_000).step_by(50) {
            frame(&mut engine, now).unwrap();
            weapons.read_control_energy(&engine, handle);
            resumed += weapons.fire(true, now).count();
            weapons.publish_control_energy(&mut engine, handle);
        }
        assert!(resumed > 0, "laser must resume after regeneration");
    }
}
