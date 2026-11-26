//! Apply the directional Control field before hull resistance.

use crate::{
    control_device_budget_coefficient, control_device_selector_zero, fixed_words_ascii_c_string,
    projectile::unit_sphere, weapon_pose::WeaponPoses, Engine, MissionControlDeviceTableStatus,
    ProjectileContact,
};
use fparkan_render::LegacyIron3dEulerTransform;
use fparkan_world::transform_state;

/// One shield pulse triggered on the side that intercepted a projectile.
pub struct ShieldImpact {
    /// The owning object draft, which determines the effect instance pool.
    pub object_index: usize,
    /// Authored archive and resource name of the protection device.
    pub source: (String, String),
    /// World matrix centered on the shield and directed toward the contact.
    pub world_from_effect: [[f32; 4]; 4],
}

pub(super) struct ShieldImpacts {
    pulses: Vec<ShieldImpact>,
    local_spheres: Vec<Option<([f32; 3], f32)>>,
}

/// Enables collection of transient shield pulses for the presentation layer.
pub fn collect_shield_impacts(engine: &mut Engine) {
    let local_spheres = engine.loaded.as_ref().map_or_else(Vec::new, |mission| {
        let assets = &mission.mission_assets;
        mission
            .object_drafts
            .iter()
            .enumerate()
            .map(|(index, draft)| {
                if draft.unit_dat_header_flags != Some(crate::boarding::WARBOT_CLASS) {
                    return None;
                }
                let controls = assets.object_component_controls.get(index)?;
                if !controls
                    .iter()
                    .flatten()
                    .flat_map(|control| &control.typed_records)
                    .any(|record| record.tag == 9)
                {
                    return None;
                }
                let poses = WeaponPoses::new(draft, assets, index);
                enclosing_sphere(&poses.initial_model_spheres(assets, index))
            })
            .collect()
    });
    engine.shield_impacts = Some(ShieldImpacts {
        pulses: Vec::new(),
        local_spheres,
    });
}

/// Drains shield pulses accumulated since the last presentation update.
pub fn take_shield_impacts(engine: &mut Engine) -> Vec<ShieldImpact> {
    engine
        .shield_impacts
        .as_mut()
        .map_or_else(Vec::new, |impacts| std::mem::take(&mut impacts.pulses))
}

/// Finds authored shield effects for placed units and prepared spawn slots.
pub fn shield_effect_sources(
    engine: &Engine,
) -> impl Iterator<Item = (usize, (String, String))> + '_ {
    engine.loaded.iter().flat_map(|mission| {
        mission
            .mission_assets
            .object_component_controls
            .iter()
            .enumerate()
            .flat_map(|(object, controls)| {
                controls.iter().flatten().flat_map(move |control| {
                    control
                        .typed_records
                        .iter()
                        .filter(|record| record.tag == 9)
                        .filter_map(move |record| {
                            Some((object, shield_effect_source(&record.fixed_words)?))
                        })
                })
            })
    })
}

fn shield_effect_source(words: &[u32; 44]) -> Option<(String, String)> {
    let archive = fixed_words_ascii_c_string(&words[27..35])?;
    let name = fixed_words_ascii_c_string(&words[35..43])?;
    (!archive.is_empty() && !name.is_empty()).then_some((archive, name))
}

pub(super) fn absorb(engine: &mut Engine, hit: ProjectileContact, volume: f32) -> f32 {
    if !volume.is_finite() || volume <= 0.0 {
        return volume;
    }
    let Some(mission) = engine.loaded.as_mut() else {
        return volume;
    };
    let Some(table) = mission
        .control_device_tables
        .iter_mut()
        .find(|table| table.handle == hit.handle)
    else {
        return volume;
    };
    let MissionControlDeviceTableStatus::Available(devices) = &mut table.status else {
        return volume;
    };
    let Some(strength) = devices.iter().rfind(|device| device.factory_tag == 21) else {
        return volume;
    };
    let Some(index) = table.original_id.map(|id| id.0 as usize) else {
        return volume;
    };
    let Ok(pose) = transform_state(&engine.world, hit.handle) else {
        return volume;
    };
    let Some((center, radius)) = unit_sphere(&mission.mission_assets, index, pose) else {
        return volume;
    };
    let Some(basis) = (LegacyIron3dEulerTransform {
        translation: [0.0; 3],
        orientation_radians: pose.orientation.map(f32::from_bits),
    })
    .try_row_major() else {
        return volume;
    };
    let offset = std::array::from_fn::<_, 3, _>(|axis| hit.position[axis] - center[axis]);
    let local = std::array::from_fn(|axis| {
        offset[0] * basis[axis] + offset[1] * basis[4 + axis] + offset[2] * basis[8 + axis]
    });
    let Some(sector) = contact_sector(local) else {
        return volume;
    };
    let Ok(multiplier) = control_device_budget_coefficient(strength, sector) else {
        return volume;
    };
    let Some(shield) = devices.iter_mut().rfind(|device| device.factory_tag == 9) else {
        return volume;
    };
    let Ok(base) = control_device_selector_zero(shield) else {
        return volume;
    };
    let (Some(values), Some(deltas)) = (
        &mut shield.class9_values_bits,
        &mut shield.class9_deltas_bits,
    ) else {
        return volume;
    };
    let fraction = f32::from_bits(values[sector]);
    let capacity = base * fraction * multiplier;
    if !capacity.is_finite() || capacity <= 0.0 {
        return volume;
    }
    let (remaining, strength_left) = intercept(volume, capacity);
    // The projectile callback writes the remaining power through Control+0x2ca80.
    let delta = (strength_left / multiplier - base * fraction) / base;
    let updated = (fraction + delta).clamp(0.0, 1.0);
    values[sector] = updated.to_bits();
    deltas[sector] = (f32::from_bits(deltas[sector]) + (updated - fraction)).to_bits();
    if let Some(impacts) = engine.shield_impacts.as_mut() {
        if let Some(source) = shield_effect_source(&shield.descriptor.fixed_words) {
            let display_sphere = || {
                let (center, radius) = *impacts.local_spheres.get(index)?.as_ref()?;
                let scale = pose.scale.map(f32::from_bits);
                let transform = LegacyIron3dEulerTransform {
                    translation: pose.position.map(f32::from_bits),
                    orientation_radians: pose.orientation.map(f32::from_bits),
                };
                Some((
                    transform.try_transform_scaled_point(center, scale)?,
                    radius * scale.into_iter().map(f32::abs).fold(0.0, f32::max),
                ))
            };
            let (display_center, display_radius) = display_sphere().unwrap_or((center, radius));
            // Keep the direction of the actual contact on the collision
            // sphere until the field has its own collision surface.
            impacts.pulses.push(ShieldImpact {
                object_index: index,
                source,
                world_from_effect: effect_transform(display_center, offset, display_radius),
            });
        }
    }
    remaining
}

fn enclosing_sphere(parts: &[([f32; 3], f32)]) -> Option<([f32; 3], f32)> {
    // AniMesh+0x9510 weights model centers by the authored radii. A second
    // pass then puts even the most distant model sphere inside the result.
    let weight: f32 = parts.iter().map(|(_, radius)| radius).sum();
    if weight <= 0.0 {
        return None;
    }
    let center = std::array::from_fn(|axis| {
        parts
            .iter()
            .map(|(point, radius)| point[axis] * radius)
            .sum::<f32>()
            / weight
    });
    let radius = parts
        .iter()
        .map(|(point, radius)| {
            let distance_sq: f32 = (0..3)
                .map(|axis| (center[axis] - point[axis]).powi(2))
                .sum();
            distance_sq.sqrt() + radius
        })
        .fold(0.0, f32::max);
    Some((center, radius))
}

fn effect_transform(center: [f32; 3], [x, y, z]: [f32; 3], radius: f32) -> [[f32; 4]; 4] {
    // Control+0x2c844 builds X from the incident side of the sphere, Y in the
    // horizontal plane, and Z as their cross product.
    let horizontal = x.hypot(y);
    let length = horizontal.hypot(z);
    let [nx, ny, nz] = [x / length, y / length, z / length];
    let [tx, ty] = if horizontal > 0.0 {
        [-y / horizontal, x / horizontal]
    } else {
        [1.0, 0.0]
    };
    [
        [radius * nx, radius * tx, -radius * nz * ty, center[0]],
        [radius * ny, radius * ty, radius * nz * tx, center[1]],
        [radius * nz, 0.0, radius * (nx * ty - ny * tx), center[2]],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn contact_sector([x, y, z]: [f32; 3]) -> Option<usize> {
    if ![x, y, z].into_iter().all(f32::is_finite) || [x, y, z] == [0.0; 3] {
        return None;
    }
    Some(if y.abs() >= x.abs().max(z.abs()) {
        usize::from(y < 0.0)
    } else if x.abs() >= z.abs() {
        2 + usize::from(x < 0.0)
    } else {
        4 + usize::from(z < 0.0)
    })
}

fn intercept(volume: f32, capacity: f32) -> (f32, f32) {
    ((volume - capacity).max(0.0), (capacity - volume).max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enclosing_sphere_uses_radii_to_weight_the_center() {
        let parts = [([0.0; 3], 2.0), ([6.0, 0.0, 0.0], 1.0)];
        assert_eq!(enclosing_sphere(&parts), Some(([2.0, 0.0, 0.0], 5.0)));
        assert_eq!(enclosing_sphere(&[]), None);
    }

    #[test]
    fn shield_spends_strength_before_dealing_hull_damage() {
        assert_eq!(intercept(899.0, 3040.0), (0.0, 2141.0));
        assert_eq!(intercept(220.0, 235.0), (0.0, 15.0));
        assert_eq!(intercept(235.0, 235.0), (0.0, 0.0));
        assert_eq!(intercept(260.0, 235.0), (25.0, 0.0));
        assert_eq!(intercept(260.0, 0.0), (260.0, 0.0));
        assert_eq!(contact_sector([1.0, 1.0, 1.0]), Some(0));
        assert_eq!(contact_sector([1.0, -2.0, 1.0]), Some(1));
        assert_eq!(contact_sector([-2.0, 0.2, 1.0]), Some(3));
        assert_eq!(contact_sector([-1.0, 0.2, -2.0]), Some(5));
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_autodemo_shield_blocks_and_then_exposes_one_sector() {
        use crate::{
            apply_projectile_contact, create, load_gameplay_mission, EngineServices, MissionMode,
            MissionRequest, MissionStartupInput,
        };
        use fparkan_vfs::DirectoryVfs;
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
        let mission = engine.loaded.as_ref().unwrap();
        let (contact, capacity, health) = mission
            .control_device_tables
            .iter()
            .find_map(|table| {
                let MissionControlDeviceTableStatus::Available(devices) = &table.status else {
                    return None;
                };
                let shield = devices.iter().rfind(|device| device.factory_tag == 9)?;
                let strength = devices.iter().rfind(|device| device.factory_tag == 21)?;
                let capacity = control_device_selector_zero(shield).ok()?
                    * f32::from_bits(shield.class9_values_bits?[0])
                    * control_device_budget_coefficient(strength, 0).ok()?;
                if capacity <= 0.0 {
                    return None;
                }
                let pose = transform_state(&engine.world, table.handle).ok()?;
                let (center, radius) =
                    unit_sphere(&mission.mission_assets, table.original_id?.0 as usize, pose)?;
                let basis = LegacyIron3dEulerTransform {
                    translation: center,
                    orientation_radians: pose.orientation.map(f32::from_bits),
                };
                let position = basis.try_transform_scaled_point([0.0, radius, 0.0], [1.0; 3])?;
                let hit = ProjectileContact {
                    handle: table.handle,
                    fraction: 0.5,
                    position,
                    node: None,
                };
                Some((hit, capacity, table.node_provider_amount_bits?))
            })
            .expect("stock shield with a charged front sector");
        collect_shield_impacts(&mut engine);
        let (center, radius) = engine.shield_impacts.as_ref().unwrap().local_spheres[0]
            .expect("stock AutoDemo warbot enclosure");
        assert!(center[2] > 0.0 && radius > 6.0);
        assert!(
            apply_projectile_contact(&mut engine, contact, capacity * 0.5)
                .unwrap()
                .is_none()
        );
        let impacts = take_shield_impacts(&mut engine);
        let [impact] = impacts.as_slice() else {
            panic!("charged sector starts one shield effect");
        };
        assert!(shield_effect_sources(&engine)
            .any(|(object, source)| object == impact.object_index && source == impact.source));
        assert!(impact
            .world_from_effect
            .into_iter()
            .flatten()
            .all(f32::is_finite));
        let table = engine
            .loaded
            .as_ref()
            .unwrap()
            .control_device_tables
            .iter()
            .find(|table| table.handle == contact.handle)
            .unwrap();
        assert_eq!(table.node_provider_amount_bits, Some(health));
        assert!((absorb(&mut engine, contact, capacity * 0.5 + 20.0) - 20.0).abs() < 0.001);
        assert_eq!(take_shield_impacts(&mut engine).len(), 1);
        assert_eq!(absorb(&mut engine, contact, capacity * 0.5), capacity * 0.5);
        assert!(take_shield_impacts(&mut engine).is_empty());
    }
}
