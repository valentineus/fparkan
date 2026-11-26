//! Damage reduction from the target's authored Control protection coefficients.

pub(super) fn resisted_volume(incoming: f32, coefficients: Option<[u32; 2]>) -> f32 {
    let Some([linear, quadratic]) = coefficients.map(|bits| bits.map(f32::from_bits)) else {
        return incoming;
    };
    if !incoming.is_finite() || incoming <= 0.0 || !linear.is_finite() || !quadratic.is_finite() {
        return incoming;
    }
    // Control.dll+0x10218..0x1026c evaluates these operations at 24-bit precision.
    ((incoming * quadratic + linear) * incoming).clamp(0.0, incoming)
}

#[cfg(test)]
mod tests {
    use super::resisted_volume;

    #[test]
    fn protection_matches_live_control_damage() {
        let coefficients = Some([0x3eab_af10, 0x3966_afcd]);
        for (incoming, expected) in [(0x436b_0000, 0x42b5_e63f), (0x435a_aae4, 0x42a7_af9e)] {
            assert_eq!(
                resisted_volume(f32::from_bits(incoming), coefficients).to_bits(),
                expected
            );
        }
        assert_eq!(resisted_volume(390.0, None), 390.0);
        assert_eq!(resisted_volume(390.0, Some([0, 0])), 0.0);
        assert_eq!(resisted_volume(390.0, Some([2.0_f32.to_bits(), 0])), 390.0);
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_autodemo_contact_uses_the_loaded_protection() {
        use crate::{
            apply_projectile_contact, control_node_provider_row, create, load_gameplay_mission,
            loaded_mission_control_device_tables, EngineServices, MissionMode, MissionRequest,
            MissionStartupInput, ProjectileContact,
        };
        use fparkan_vfs::DirectoryVfs;
        use fparkan_world::transform_state;
        use std::sync::Arc;

        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed corpus");
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
        let table = loaded_mission_control_device_tables(&engine)
            .unwrap()
            .iter()
            .find(|table| {
                table.projectile_resistance_bits == Some([0x3eab_af10, 0x3966_afcd])
                    && transform_state(&engine.world, table.handle).is_ok()
            })
            .expect("the stock warbot with the protection measured in Control");
        let root = control_node_provider_row(table, 0).unwrap();
        let remaining = f32::from_bits(root.current_amount_bits) - f32::from_bits(0x42b5_e63f);
        let contact = ProjectileContact {
            handle: table.handle,
            fraction: 0.5,
            position: transform_state(&engine.world, table.handle)
                .unwrap()
                .position
                .map(f32::from_bits),
            building_node: None,
        };
        assert_eq!(
            apply_projectile_contact(&mut engine, contact, 235.0).unwrap(),
            Some((remaining, false))
        );
    }
}
