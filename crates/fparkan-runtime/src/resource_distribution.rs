//! The two-pass clan resource distributor from Behavior.dll RVA 0x19e80.

use crate::{
    behavior_add_total, behavior_allocation, behavior_checked_property,
    behavior_control_index_for_handle, behavior_difference, behavior_ore_index_for_handle,
    behavior_product, behavior_query_control, behavior_tick_resource_rate, checked_control_float,
    checked_control_ratio, checked_control_sum, set_control_device_table_multiplier,
    BehaviorDistributorState, BehaviorOreState, BehaviorProfileState, BehaviorTickTotals,
    MissionControlDeviceTableState,
};
use fparkan_world::{handle_by_original_id, World};

pub(super) fn advance_behavior_distributor_tick(
    distributor: &mut BehaviorDistributorState,
    delta_seconds: f32,
    world: &World,
    ore_states: &mut [BehaviorOreState],
    profiles: &[BehaviorProfileState],
    control_tables: &mut [MissionControlDeviceTableState],
) -> Result<[u32; 5], String> {
    let mut totals = BehaviorTickTotals::default();

    // Native pass one measures supply and demand, then records per-row demand.
    for object in &mut distributor.registered_objects {
        let Some(live_handle) = handle_by_original_id(world, object.original_id) else {
            object.native_tail[1] = 0;
            object.native_tail[3] = 0;
            continue;
        };
        if live_handle != object.handle {
            return Err(format!(
                "live original object {} maps to {live_handle:?}, expected {:?}",
                object.original_id.0, object.handle
            ));
        }
        if control_tables
            .iter()
            .any(|table| table.handle == live_handle && table.node_provider_depletion_marked)
        {
            continue;
        }
        let share = checked_control_float(object.class_flags, "Behavior share", object.share)
            .map_err(|error| format!("{error:?}"))?;

        let power_source = matches!(
            object.class_flags,
            0x8000_0002 | 0x8001_0000 | 0x8002_0000 | 0x8004_0000
        );
        let power_demand = if power_source {
            let rate = behavior_tick_resource_rate(
                object.class_flags,
                live_handle,
                0x1005,
                profiles,
                control_tables,
            )?;
            let production = behavior_product(delta_seconds, rate, "power production")?;
            behavior_add_total(&mut totals.power_available, production, "power available")?;
            0.0
        } else {
            let total_power = behavior_query_control(control_tables, live_handle, 2)?;
            let power_ratio = behavior_query_control(control_tables, live_handle, 1)?;
            if total_power > 0.0 {
                let used_power = behavior_product(power_ratio, total_power, "used power")?;
                behavior_difference(total_power, used_power, "power demand")?
            } else {
                0.0
            }
        };
        object.native_tail[1] = power_demand.to_bits();
        behavior_add_total(&mut totals.power_demand, power_demand, "power demand")?;
        behavior_add_total(
            &mut totals.power_weighted_demand,
            behavior_product(power_demand, share, "weighted power demand")?,
            "weighted power demand",
        )?;

        let ore_source = matches!(object.class_flags, 0x8000_0004 | 0x8000_0008);
        let ore_index = behavior_ore_index_for_handle(ore_states, live_handle)?;
        let current = behavior_checked_property(&ore_states[ore_index], false)?;
        let ore_demand = if ore_source {
            let rate = behavior_tick_resource_rate(
                object.class_flags,
                live_handle,
                0x1002,
                profiles,
                control_tables,
            )?;
            let production = behavior_product(delta_seconds, rate, "ore production")?;
            let available = if current < production {
                current
            } else {
                production
            };
            behavior_add_total(&mut totals.ore_available, available, "ore available")?;
            behavior_add_total(&mut totals.ore_in_buffer, current, "ore in buffer")?;
            0.0
        } else {
            let maximum = behavior_checked_property(&ore_states[ore_index], true)?;
            let deficit = behavior_difference(maximum, current, "ore deficit")?;
            if deficit >= f32::from_bits(0x3c23_d70a) {
                deficit
            } else {
                ore_states[ore_index].current_ore_bits = maximum.to_bits();
                0.0
            }
        };
        object.native_tail[3] = ore_demand.to_bits();
        behavior_add_total(&mut totals.ore_demand, ore_demand, "ore demand")?;
        behavior_add_total(
            &mut totals.ore_weighted_demand,
            behavior_product(ore_demand, share, "weighted ore demand")?,
            "weighted ore demand",
        )?;
    }

    let power_allocation = behavior_allocation(
        totals.power_available,
        totals.power_demand,
        totals.power_weighted_demand,
    )?;
    let ore_allocation = behavior_allocation(
        totals.ore_available,
        totals.ore_demand,
        totals.ore_weighted_demand,
    )?;

    // Native pass two re-reads callbacks before applying each row in order.
    for object in &mut distributor.registered_objects {
        let Some(live_handle) = handle_by_original_id(world, object.original_id) else {
            object.native_tail[2] = 0;
            object.native_tail[4] = 0;
            continue;
        };
        if live_handle != object.handle {
            return Err(format!(
                "live original object {} maps to {live_handle:?}, expected {:?}",
                object.original_id.0, object.handle
            ));
        }
        if control_tables
            .iter()
            .any(|table| table.handle == live_handle && table.node_provider_depletion_marked)
        {
            continue;
        }
        object.native_tail[2] = 0;
        object.native_tail[4] = 0;

        let power_source = matches!(
            object.class_flags,
            0x8000_0002 | 0x8001_0000 | 0x8002_0000 | 0x8004_0000
        );
        if power_source {
            let table_index = behavior_control_index_for_handle(control_tables, live_handle)?;
            set_control_device_table_multiplier(&mut control_tables[table_index], 1.0)
                .map_err(|error| format!("{error:?}"))?;
        } else {
            let table_index = behavior_control_index_for_handle(control_tables, live_handle)?;
            let total_power = behavior_query_control(control_tables, live_handle, 2)?;
            if total_power > 0.0 {
                let power_ratio = behavior_query_control(control_tables, live_handle, 1)?;
                let used_power = behavior_product(power_ratio, total_power, "used power")?;
                let demand = behavior_difference(total_power, used_power, "power demand")?;
                let share = object.share;
                let priority_share =
                    behavior_product(share, power_allocation.priority, "priority power share")?;
                let residual_share = behavior_product(
                    behavior_difference(1.0, share, "remaining share")?,
                    power_allocation.residual,
                    "residual power share",
                )?;
                let fraction = checked_control_sum(
                    0,
                    "power allocation fraction",
                    residual_share,
                    priority_share,
                )
                .map_err(|error| format!("{error:?}"))?;
                let allocated = behavior_product(demand, fraction, "power allocation")?;
                let updated =
                    checked_control_sum(0, "used plus allocated power", used_power, allocated)
                        .map_err(|error| format!("{error:?}"))?;
                let new_ratio =
                    checked_control_ratio("new Control selector 1", updated, total_power)
                        .map_err(|error| format!("{error:?}"))?;
                set_control_device_table_multiplier(&mut control_tables[table_index], new_ratio)
                    .map_err(|error| format!("{error:?}"))?;
                object.native_tail[2] = allocated.to_bits();
            }
        }

        let ore_source = matches!(object.class_flags, 0x8000_0004 | 0x8000_0008);
        let ore_index = behavior_ore_index_for_handle(ore_states, live_handle)?;
        let current = behavior_checked_property(&ore_states[ore_index], false)?;
        if ore_source {
            let rate = behavior_tick_resource_rate(
                object.class_flags,
                live_handle,
                0x1002,
                profiles,
                control_tables,
            )?;
            let production = behavior_product(delta_seconds, rate, "ore withdrawal rate")?;
            let bounded_production = if current < production {
                current
            } else {
                production
            };
            let taken = behavior_product(
                bounded_production,
                ore_allocation.consumption,
                "ore withdrawal",
            )?;
            ore_states[ore_index].current_ore_bits =
                behavior_difference(current, taken, "remaining ore")?.to_bits();
        } else {
            let maximum = behavior_checked_property(&ore_states[ore_index], true)?;
            let capacity = behavior_difference(maximum, current, "ore capacity")?;
            let share = object.share;
            let priority_share =
                behavior_product(share, ore_allocation.priority, "priority ore share")?;
            let residual_share = behavior_product(
                behavior_difference(1.0, share, "remaining share")?,
                ore_allocation.residual,
                "residual ore share",
            )?;
            let fraction =
                checked_control_sum(0, "ore allocation fraction", residual_share, priority_share)
                    .map_err(|error| format!("{error:?}"))?;
            let allocated = behavior_product(capacity, fraction, "ore allocation")?;
            ore_states[ore_index].current_ore_bits =
                checked_control_sum(0, "current plus allocated ore", current, allocated)
                    .map_err(|error| format!("{error:?}"))?
                    .to_bits();
            object.native_tail[4] = allocated.to_bits();
        }
    }

    Ok([
        totals.power_available.to_bits(),
        totals.ore_available.to_bits(),
        totals.ore_in_buffer.to_bits(),
        totals.power_demand.to_bits(),
        totals.ore_demand.to_bits(),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{behavior_tick_test_control_table, behavior_tick_test_object};
    use fparkan_world::{new, transform_state, WorldConfig};

    #[test]
    fn depleted_objects_keep_their_rows_without_resource_queries_or_writes() {
        // Both source kinds and a consumer bypass every resource callback in both passes.
        for class_flags in [0x8001_0000, 0x8000_0004, 0x0100_8000] {
            let mut world = new(WorldConfig);
            let mut object = behavior_tick_test_object(&mut world, 0x55, class_flags, 0.5);
            object.native_tail = std::array::from_fn(|i| 0x4102_0000 + i as u32 * 0x1_0000);
            let mut distributor = BehaviorDistributorState {
                clan_index: 0,
                super_ai_visible: true,
                registered_objects: vec![object.clone()],
            };
            let before = distributor.clone();
            let mut table = behavior_tick_test_control_table(0x55, object.handle);
            table.node_provider_depletion_marked = true;
            let mut tables = [table];
            let tables_before = tables.clone();
            // No ore or profile interfaces are supplied: the native state guard precedes them.
            let totals = advance_behavior_distributor_tick(
                &mut distributor,
                1.0,
                &world,
                &mut [],
                &[],
                &mut tables,
            )
            .expect("depleted objects do not query resource interfaces");
            assert_eq!(totals, [0; 5]);
            assert_eq!(distributor, before);
            assert_eq!(tables, tables_before);
            assert_eq!(
                handle_by_original_id(&world, object.original_id),
                Some(object.handle)
            );
            assert!(transform_state(&world, object.handle).is_ok());
        }
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_stock_raid_removes_depleted_bunker_power_from_clan_totals() {
        use crate::{
            create, frame, load_gameplay_mission, player_status, EngineServices, MissionMode,
            MissionRequest, MissionStartupInput,
        };
        use fparkan_vfs::DirectoryVfs;
        use std::sync::Arc;

        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed corpus");
        let mut engine = create(EngineServices::new(Arc::new(DirectoryVfs::new(root)))).unwrap();
        load_gameplay_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/CAMPAIGN/CAMPAIGN.04/Mission.02/data.tma".into(),
                mode: MissionMode::SingleMission,
            },
            MissionStartupInput {
                queue_time_ms: 0,
                research_creation_gate: false,
            },
            None,
        )
        .unwrap();
        let loaded = engine.loaded.as_ref().unwrap();
        let bunker = loaded.control_device_tables[0].handle;
        let owner = fparkan_world::identity_metadata(&engine.world, bunker)
            .unwrap()
            .owner_id
            .unwrap();
        let output_rate = f32::from_bits(
            loaded
                .behavior_profiles
                .iter()
                .find(|profile| profile.handle == bunker)
                .unwrap()
                .transfer_power_out_bits,
        );
        assert!(output_rate > 0.0);
        let mut last_tick_ms = 0;
        let mut last_intact_rate = None;
        let mut depletion_time = None;
        for now in (0..=900_000).step_by(50) {
            frame(&mut engine, now).unwrap();
            let loaded = engine.loaded.as_ref().unwrap();
            let table = loaded
                .control_device_tables
                .iter()
                .find(|t| t.handle == bunker)
                .unwrap();
            if table.node_provider_depletion_marked {
                depletion_time.get_or_insert(now);
            }
            let resources = loaded
                .script_bundles
                .iter()
                .find(|bundle| bundle.clan_index == usize::from(owner.0))
                .unwrap()
                .behavior_warm_start
                .unwrap();
            if resources.first_time_ms == last_tick_ms {
                continue;
            }
            let elapsed =
                crate::behavior_elapsed_seconds(resources.first_time_ms.wrapping_sub(last_tick_ms));
            last_tick_ms = resources.first_time_ms;
            let power = player_status(&engine, bunker).unwrap().power.unwrap().0;
            let rate = power / elapsed;
            if !table.node_provider_depletion_marked && power > 0.0 {
                last_intact_rate = Some(rate);
            }
            if depletion_time.is_some_and(|time| resources.first_time_ms > time) {
                let before = last_intact_rate.expect("stock bunker supplied power before the raid");
                assert!(
                    (rate - (before - output_rate)).abs() < 0.00001,
                    "bunker output {output_rate} was not removed: before={before}, after={rate}"
                );
                assert!(transform_state(&engine.world, bunker).is_ok());
                assert!(
                    loaded.behavior_distributors.iter().any(|distributor| {
                        distributor
                            .registered_objects
                            .iter()
                            .any(|object| object.handle == bunker)
                    }),
                    "depletion must retain the distributor entry"
                );
                eprintln!(
                    "stock bunker depleted at {}: clan power rate {before} -> {rate}",
                    depletion_time.unwrap()
                );
                return;
            }
        }
        panic!("stock raid never removed depleted bunker output from clan power");
    }
}
