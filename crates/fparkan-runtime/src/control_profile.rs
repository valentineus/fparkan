//! Mutable Control motion limits after provider amounts change.

use crate::{
    control_fca0_clamp_to_profile_cap, control_fca0_float, control_fca0_product,
    control_fca0_ratio, control_fca0_sum, query_control_device_selector, EffectiveControlDevice,
    MissionControlClassProjection, MissionControlDeviceTableState, MissionControlDeviceTableStatus,
    MissionControlNodeProviderTableStatus, MissionControlOwnerProfileFca0UnavailableReason,
    MissionControlOwnerProfileLoadProjection,
    MissionControlOwnerProfileLoadProjectionUnavailableReason,
};
use std::{collections::BTreeMap, sync::Arc};

/// Native owner fields +0x1a8, +0x354 and +0x358, retained across dirty updates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControlProfileScalars {
    pub scalar_bits: u32,
    pub mean_a_bits: u32,
    pub mean_b_bits: u32,
}

impl Default for ControlProfileScalars {
    fn default() -> Self {
        Self {
            scalar_bits: 1.0_f32.to_bits(),
            mean_a_bits: 1.0_f32.to_bits(),
            mean_b_bits: 1.0_f32.to_bits(),
        }
    }
}

/// Resource-clock checks, dirty profile publications and unresolved updates.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ControlProfileReport {
    pub ticks: BTreeMap<u32, u64>,
    /// Object -> (updates, previous forward-limit bits, latest forward-limit bits).
    pub updates: BTreeMap<u32, (u64, u32, u32)>,
    pub stubs: BTreeMap<(u32, String), u64>,
}

pub(super) fn project_initial(
    native_class: Option<MissionControlClassProjection>,
    original_words: &[u32; 27],
    current_words: &[u32; 27],
    fac0_projection: &Result<
        MissionControlOwnerProfileLoadProjection,
        MissionControlOwnerProfileLoadProjectionUnavailableReason,
    >,
    devices: &[EffectiveControlDevice],
    provider_status: &MissionControlNodeProviderTableStatus,
) -> Result<[u32; 27], MissionControlOwnerProfileFca0UnavailableReason> {
    project(
        native_class,
        original_words,
        current_words,
        fac0_projection,
        devices,
        provider_status,
        ControlProfileScalars::default(),
    )
}

fn node_means(
    mut scalars: ControlProfileScalars,
    rows: impl IntoIterator<Item = (u32, f32)>,
    low_total_amount: bool,
) -> Result<ControlProfileScalars, &'static str> {
    let mut sums = [0.0_f32; 3];
    let mut counts = [0.0_f32; 3];
    for (flags, value) in rows {
        if flags & 0x70 == 0 {
            continue;
        }
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err("node_ratio");
        }
        for (group, mask) in [0x70, 0x20, 0x40].into_iter().enumerate() {
            if flags & mask != 0 {
                sums[group] += value;
                counts[group] += 1.0;
            }
        }
    }
    // Control+0x12a40 enters its damage/alarm path at this boundary.
    if counts[0] > 1.0 {
        if sums[0] <= counts[0] * 0.5 - 1.0 {
            return Err("critical_nodes_alarm");
        }
    } else if low_total_amount {
        return Err("provider_amount_alarm");
    }
    if counts[1] > 0.0 {
        scalars.mean_a_bits = (sums[1] / counts[1]).to_bits();
    }
    if counts[2] > 0.0 {
        scalars.mean_b_bits = (sums[2] / counts[2]).to_bits();
    }
    Ok(scalars)
}

/// The non-alarm dirty branch of Control+0x12a40, before the budget pass.
pub(super) fn refresh(table: &mut MissionControlDeviceTableState) -> Result<bool, String> {
    if !table.node_provider_dirty {
        return Ok(false);
    }
    let profile = table.owner_profile.as_ref().ok_or("owner_profile")?;
    let rows = match &table.node_provider_status {
        MissionControlNodeProviderTableStatus::Available(rows) => rows,
        MissionControlNodeProviderTableStatus::Unavailable(_) => {
            return Err("provider_table".into())
        }
    };
    let amount = table
        .node_provider_amount_bits
        .map(f32::from_bits)
        .ok_or("provider_amount")?;
    let maximum = table
        .node_provider_maximum_amount_bits
        .map(f32::from_bits)
        .ok_or("provider_maximum")?;
    let class = table.native_control_class.ok_or("owner_class")?;
    let threshold = maximum * if class.class_id == 10 { 0.3 } else { 0.2 };
    if !amount.is_finite() || !maximum.is_finite() || maximum <= 0.0 {
        return Err("provider_amount_range".into());
    }
    let scalars = node_means(
        profile.scalars,
        rows.iter().map(|row| {
            (
                row.source_row.raw_flags,
                f32::from_bits(row.current_type1_value_bits),
            )
        }),
        amount < threshold,
    )?;
    let devices = match &table.status {
        MissionControlDeviceTableStatus::Available(devices) => devices,
        MissionControlDeviceTableStatus::Unavailable(_) => return Err("device_table".into()),
    };
    let words = project(
        table.native_control_class,
        &profile.original_words,
        &profile.current_words,
        &profile.load_projection,
        devices,
        &table.node_provider_status,
        scalars,
    )
    .map_err(|reason| format!("{reason:?}"))?;
    let profile = table.owner_profile.as_mut().expect("checked profile");
    profile.current_words = words;
    profile.scalars = scalars;
    table.node_provider_dirty = false;
    Ok(true)
}

pub(super) fn project(
    native_class: Option<MissionControlClassProjection>,
    original_words: &[u32; 27],
    current_words: &[u32; 27],
    fac0_projection: &Result<
        MissionControlOwnerProfileLoadProjection,
        MissionControlOwnerProfileLoadProjectionUnavailableReason,
    >,
    devices: &[EffectiveControlDevice],
    provider_status: &MissionControlNodeProviderTableStatus,
    scalars: ControlProfileScalars,
) -> Result<[u32; 27], MissionControlOwnerProfileFca0UnavailableReason> {
    use MissionControlOwnerProfileFca0UnavailableReason as Unavailable;

    if native_class.is_some_and(|class| class.class_id == 9) {
        return Ok(*current_words);
    }
    if native_class.is_none() {
        return Err(Unavailable::MissingNativeControlClass);
    }
    if let Err(reason) = fac0_projection {
        return Err(Unavailable::Fac0Unavailable(reason.clone()));
    }

    let mut e00_sum = 0.0_f32;
    for device_index in (0..devices.len()).rev() {
        if devices[device_index].descriptor.tag != 5 {
            continue;
        }
        if let Some(value) = query_control_device_selector(&devices[device_index], provider_status)
            .map_err(|error| Unavailable::QueryUnavailable(Arc::new(error)))?
        {
            e00_sum = control_fca0_sum(e00_sum, value, "FCA0 reverse E00 sum")?;
        }
    }

    scale_words(original_words, current_words, scalars, e00_sum)
}

fn scale_words(
    original_words: &[u32; 27],
    current_words: &[u32; 27],
    scalars: ControlProfileScalars,
    e00_sum: f32,
) -> Result<[u32; 27], MissionControlOwnerProfileFca0UnavailableReason> {
    let owner_scalar =
        control_fca0_float(f32::from_bits(scalars.scalar_bits), "FCA0 owner scalar")?;
    let owner_mean = control_fca0_sum(
        f32::from_bits(scalars.mean_a_bits),
        f32::from_bits(scalars.mean_b_bits),
        "FCA0 owner scalar mean",
    )?;
    let owner_mean = control_fca0_product(owner_mean, 0.5, "FCA0 owner scalar mean")?;
    let weight = control_fca0_product(owner_mean, e00_sum, "FCA0 weighted E00 sum")?;
    let ratio = control_fca0_ratio(
        control_fca0_float(f32::from_bits(current_words[26]), "current profile word 26")?,
        control_fca0_float(
            f32::from_bits(original_words[26]),
            "original profile word 26",
        )?,
        "FCA0 profile word 26 ratio",
    )?;
    let factor_a = control_fca0_product(
        control_fca0_sum(ratio, 1.0, "FCA0 factor A")?,
        0.5,
        "FCA0 factor A",
    )?;
    let factor_b = control_fca0_sum(
        control_fca0_product(ratio, 0.75, "FCA0 factor B")?,
        0.25,
        "FCA0 factor B",
    )?;

    let mut current_words = *current_words;
    for index in 6..9 {
        let original = control_fca0_float(
            f32::from_bits(original_words[index]),
            "original profile words 6..8",
        )?;
        let owner_scaled =
            control_fca0_product(owner_scalar, original, "FCA0 first profile triple")?;
        let weighted = control_fca0_product(owner_scaled, weight, "FCA0 first profile triple")?;
        let factored = control_fca0_product(weighted, factor_a, "FCA0 first profile triple")?;
        current_words[index] =
            control_fca0_clamp_to_profile_cap(factored, original, "FCA0 first profile triple")?
                .to_bits();
    }
    for index in 9..12 {
        let original = control_fca0_float(
            f32::from_bits(original_words[index]),
            "original profile words 9..11",
        )?;
        let weighted = control_fca0_product(weight, original, "FCA0 second profile triple")?;
        let factored = control_fca0_product(weighted, factor_b, "FCA0 second profile triple")?;
        current_words[index] =
            control_fca0_sum(factored, factored, "FCA0 doubled second profile triple")?.to_bits();
    }
    for index in 0..3 {
        let original = control_fca0_float(
            f32::from_bits(original_words[index]),
            "original profile words 0..2",
        )?;
        current_words[index] =
            control_fca0_sum(original, original, "FCA0 doubled profile words")?.to_bits();
    }
    Ok(current_words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_dirty_update_uses_means_and_retains_an_absent_group() {
        // GOG Control.dll RVA 0x12a40 -> 0xfca0, x87 CW 0x007f.
        let mut scalars = ControlProfileScalars::default();
        let mut original = [0; 27];
        original[6..9].copy_from_slice(&[10.0_f32, 20.0, 30.0].map(f32::to_bits));
        original[26] = 4.0_f32.to_bits();
        for (rows, a, b, limit) in [
            (
                [(0x20, 0.25), (0x40, 0.75)],
                0x3e800000,
                0x3f400000,
                0x40f00000,
            ),
            (
                [(0x20, 1.0), (0x40, 0.5)],
                0x3f800000,
                0x3f000000,
                0x41340000,
            ),
            (
                [(0x10, 0.5), (0x40, 1.0)],
                0x3f800000,
                0x3f800000,
                0x41700000,
            ),
        ] {
            scalars = node_means(scalars, rows, false).unwrap();
            assert_eq!((scalars.mean_a_bits, scalars.mean_b_bits), (a, b));
            let words = scale_words(&original, &original, scalars, 0.75).unwrap();
            assert_eq!(words[7], limit);
        }
    }

    #[test]
    fn node_means_count_each_membership_and_reject_the_alarm_branch() {
        let means = node_means(
            ControlProfileScalars::default(),
            [(0x60, 0.5), (0x20, 1.0), (0, 0.0)],
            false,
        )
        .unwrap();
        assert_eq!(f32::from_bits(means.mean_a_bits), 0.75);
        assert_eq!(f32::from_bits(means.mean_b_bits), 0.5);
        assert!(node_means(
            ControlProfileScalars::default(),
            [(0x20, 0.0), (0x40, 0.0)],
            false
        )
        .is_err());
        // Native 0x12a40 bypasses the aggregate threshold when >1 critical
        // rows pass their own health test: 10 total / 100 max still updates.
        assert!(node_means(
            ControlProfileScalars::default(),
            [(0x20, 1.0), (0x40, 1.0)],
            true
        )
        .is_ok());
        assert_eq!(
            node_means(ControlProfileScalars::default(), [(0x20, 1.0)], true),
            Err("provider_amount_alarm")
        );
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_stock_damage_changes_patrol_limit_on_the_resource_clock() {
        use crate::{
            apply_projectile_hit, create, frame, load_gameplay_mission, EngineServices,
            MissionMode, MissionRequest, MissionStartupInput,
        };
        use fparkan_vfs::DirectoryVfs;

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
        frame(&mut engine, 0).unwrap();
        let table = &engine.loaded.as_ref().unwrap().control_device_tables[0];
        let handle = table.handle;
        let before = f32::from_bits(table.owner_profile.as_ref().unwrap().current_words[7]);
        let MissionControlNodeProviderTableStatus::Available(rows) = &table.node_provider_status
        else {
            panic!("stock provider rows");
        };
        let damage = f32::from_bits(rows[0].current_amount_bits) * 0.25;
        assert!(apply_projectile_hit(&mut engine, handle, damage)
            .unwrap()
            .is_some());
        frame(&mut engine, 200).unwrap();
        let table = &engine.loaded.as_ref().unwrap().control_device_tables[0];
        assert_eq!(
            f32::from_bits(table.owner_profile.as_ref().unwrap().current_words[7]),
            before
        );
        frame(&mut engine, 300).unwrap();
        frame(&mut engine, 350).unwrap();
        let mission = engine.loaded.as_ref().unwrap();
        let profile = mission.control_device_tables[0]
            .owner_profile
            .as_ref()
            .unwrap();
        let after = f32::from_bits(profile.current_words[7]);
        assert!(
            (after - before * 0.75).abs() < 0.00001,
            "{before} -> {after}"
        );
        let report = &mission.control_resources.profiles;
        assert_eq!(report.updates[&0].0, 1);
        assert!(report.stubs.is_empty(), "{:?}", report.stubs);
        let patrol = mission.unit_patrol.as_ref().unwrap();
        assert_eq!(patrol.speed_report.samples[&0].2, (after * 0.8).to_bits());
    }
}
