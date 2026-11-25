use super::{
    checked_control_float, checked_control_ratio, checked_control_sum,
    ControlNodeProviderPublication, ControlResourceAccessError, MissionControlDeviceTableState,
    MissionControlNodeProviderTableStatus,
};

pub(super) fn apply_control_node_delta(
    table: &mut MissionControlDeviceTableState,
    key: u32,
    delta: f32,
) -> Result<Option<ControlNodeProviderPublication>, ControlResourceAccessError> {
    apply_control_node_delta_inner(table, key, delta, false)
}

pub(super) fn apply_control_node_distribution_delta(
    table: &mut MissionControlDeviceTableState,
    key: u32,
    delta: f32,
) -> Result<Option<ControlNodeProviderPublication>, ControlResourceAccessError> {
    apply_control_node_delta_inner(table, key, delta, true)
}

fn apply_control_node_delta_inner(
    table: &mut MissionControlDeviceTableState,
    key: u32,
    delta: f32,
    accumulate_delta: bool,
) -> Result<Option<ControlNodeProviderPublication>, ControlResourceAccessError> {
    let rows = match &mut table.node_provider_status {
        MissionControlNodeProviderTableStatus::Available(rows) => rows,
        MissionControlNodeProviderTableStatus::Unavailable(reason) => {
            return Err(ControlResourceAccessError::NodeProviderTableUnavailable(
                reason.clone(),
            ))
        }
    };
    let row_count = rows.len();
    let row = usize::try_from(key)
        .ok()
        .and_then(|index| rows.get_mut(index))
        .ok_or(ControlResourceAccessError::NodeProviderKeyOutOfRange { key, row_count })?;
    checked_control_float(0, "provider delta", delta)?;
    if delta == 0.0 {
        return Ok(None);
    }
    let previous = checked_control_float(
        0,
        "provider amount",
        f32::from_bits(row.current_amount_bits),
    )?;
    let maximum = checked_control_float(0, "provider maximum", row.source_row.max_volume)?;
    if maximum < 0.0 || previous < 0.0 || previous > maximum {
        return Err(ControlResourceAccessError::InvalidNodeProviderAmount { key });
    }
    let amount =
        checked_control_sum(0, "provider amount plus delta", previous, delta)?.clamp(0.0, maximum);
    if amount == previous {
        row.current_amount_bits = amount.to_bits();
        return Ok(None);
    }
    let flags = row
        .current_flags
        .ok_or(ControlResourceAccessError::UnsupportedField {
            factory_tag: 0,
            field: "provider flags",
        })?;
    let depletion = amount == 0.0 && (key == 0 || flags & 1 != 0);
    // Control+0x11085 marks class 3 without the mobile death callbacks or root cascade.
    if depletion
        && !table.node_provider_depletion_marked
        && !table
            .native_control_class
            .is_some_and(|class| class.class_id == 3)
    {
        return Err(ControlResourceAccessError::UnsupportedNodeLifecycle { key });
    }
    let total_bits =
        table
            .node_provider_amount_bits
            .ok_or(ControlResourceAccessError::UnsupportedField {
                factory_tag: 0,
                field: "provider aggregate",
            })?;
    let total = checked_control_float(0, "provider aggregate", f32::from_bits(total_bits))?;
    if total < 0.0 {
        return Err(ControlResourceAccessError::InvalidNodeProviderAmount { key });
    }
    let change = checked_control_sum(0, "provider amount change", amount, -previous)?;
    let total = checked_control_sum(0, "provider aggregate", total, change)?.max(0.0);
    let ratio = checked_control_ratio("provider ratio", amount, maximum)?;
    let accumulated_delta = if accumulate_delta {
        let previous = checked_control_float(
            0,
            "provider accumulated delta",
            f32::from_bits(row.current_accumulated_delta_bits),
        )?;
        Some(checked_control_sum(
            0,
            "provider accumulated delta",
            change,
            previous,
        )?)
    } else {
        None
    };
    let mesh = row
        .mesh_state
        .as_mut()
        .ok_or(ControlResourceAccessError::UnsupportedField {
            factory_tag: 0,
            field: "model node state",
        })?;
    row.current_amount_bits = amount.to_bits();
    if let Some(accumulated_delta) = accumulated_delta {
        row.current_accumulated_delta_bits = accumulated_delta.to_bits();
    }
    row.current_type1_value_bits = ratio.to_bits();
    mesh.type1_value_bits = ratio.to_bits();
    row.current_flags = Some(if amount == 0.0 {
        flags | 0x10
    } else {
        flags & !0x10
    });
    table.node_provider_amount_bits = Some(total.to_bits());
    table.node_provider_dirty = true;
    table.node_provider_depletion_marked |= depletion;
    Ok(Some(ControlNodeProviderPublication {
        key,
        type1_value_bits: ratio.to_bits(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        tests::{synthetic_control_device_table_with_providers, synthetic_node_provider_row},
        MissionControlClassProjection, MissionControlClassSource, MissionControlDeviceTableStatus,
    };

    fn table() -> MissionControlDeviceTableState {
        let mut root = synthetic_node_provider_row(0, 0, 1.0_f32.to_bits());
        root.current_flags = Some(0);
        let mut child = synthetic_node_provider_row(0, 1, 1.0_f32.to_bits());
        child.source_row.max_volume = 40.0;
        child.initial_amount_bits = 40.0_f32.to_bits();
        child.current_amount_bits = 40.0_f32.to_bits();
        child.processed_amount_bits = 40.0_f32.to_bits();
        child.current_flags = Some(1);
        for row in [&mut root, &mut child] {
            row.mesh_state = Some(crate::MissionControlNodeMeshState {
                type1_value_bits: 1.0_f32.to_bits(),
                visible_stage: 0,
            });
        }
        let mut table = synthetic_control_device_table_with_providers(
            MissionControlDeviceTableStatus::Available(Vec::new()),
            MissionControlNodeProviderTableStatus::Available(vec![root, child]),
        );
        table.native_control_class = Some(MissionControlClassProjection {
            class_id: 3,
            source: MissionControlClassSource::UnitDatHighBit {
                header_flags: 0x8000_0010,
            },
        });
        table
    }

    #[test]
    fn static_depletion_matches_native_root_and_critical_child_without_cascading() {
        for (key, delta, root, child, total) in [
            (0, -150.0, 0, 0x4220_0000, 0x4220_0000),
            (1, -60.0, 0x42c8_0000, 0, 0x42c8_0000),
        ] {
            let mut table = table();
            let publication = apply_control_node_delta(&mut table, key, delta)
                .unwrap()
                .unwrap();
            assert_eq!(publication.type1_value_bits, 0);
            assert_eq!(table.node_provider_amount_bits, Some(total));
            assert!(table.node_provider_depletion_marked);
            let MissionControlNodeProviderTableStatus::Available(rows) =
                &table.node_provider_status
            else {
                panic!()
            };
            assert_eq!(rows[0].current_amount_bits, root);
            assert_eq!(rows[1].current_amount_bits, child);
            assert_ne!(rows[key as usize].current_flags.unwrap() & 0x10, 0);
            apply_control_node_delta(&mut table, key, 10.0).unwrap();
            assert!(
                table.node_provider_depletion_marked,
                "native marker survives repair"
            );
        }
    }

    #[test]
    fn mobile_death_callbacks_remain_explicit_until_the_marker_is_already_set() {
        let mut table = table();
        table.native_control_class = Some(MissionControlClassProjection {
            class_id: 4,
            source: MissionControlClassSource::RootPrototypeFourCc {
                source_type: crate::CONTROL_CLASS_SOURCE_BTLU,
            },
        });
        let before = table.clone();
        assert_eq!(
            apply_control_node_delta(&mut table, 0, -150.0),
            Err(ControlResourceAccessError::UnsupportedNodeLifecycle { key: 0 })
        );
        assert_eq!(table, before);
        table.node_provider_depletion_marked = true;
        apply_control_node_delta(&mut table, 0, -150.0).unwrap();
        assert_eq!(table.node_provider_amount_bits, Some(0x4220_0000));
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_scripted_raid_depletes_the_stock_bunker_root() {
        use crate::*;
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
        let report = simulate_mission_report(&mut engine, 900_000).unwrap();
        let table = &loaded_mission_control_device_tables(&engine).unwrap()[0];
        assert!(table.node_provider_depletion_marked);
        let row = crate::control_node_provider_row(table, 0).unwrap();
        assert_eq!(row.current_amount_bits, 0);
        assert_eq!(row.mesh_state.unwrap().type1_value_bits, 0);
        assert_ne!(row.current_flags.unwrap() & 0x10, 0);
        assert!(
            fparkan_world::transform_state(&engine.world, table.handle).is_ok(),
            "static depletion does not immediately delete the building"
        );
        assert!(!report
            .combat
            .unavailable
            .keys()
            .any(|(_, reason)| *reason == "damage_error"));
    }
}
