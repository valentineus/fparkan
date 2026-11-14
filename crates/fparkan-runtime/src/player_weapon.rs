use crate::{Engine, StartingCockpit};
use fparkan_assets::{
    PreparedComponentControl, PreparedControlEffectSection, PreparedControlPoint,
};

/// The starting hero's selectable weapon devices, in resource order.
#[derive(Default)]
pub struct PlayerWeapons {
    weapons: Vec<PlayerWeapon>,
    selected: Option<usize>,
}

/// A manual-fire device prepared from its CTLD record and effect rows.
pub struct PlayerWeapon {
    /// Name of the projectile prototype.
    pub projectile: String,
    /// Archive named by the weapon's projectile descriptor.
    pub projectile_archive: String,
    /// Point used by the weapon animation's first firing row.
    pub muzzle: PreparedControlPoint,
    /// Optional type-4 effect started by this weapon.
    pub effect: Option<(String, String)>,
    interval_ms: u32,
    last_fire_ms: Option<u32>,
}

impl PlayerWeapons {
    /// Returns the prepared devices in selection order.
    pub fn weapons(&self) -> &[PlayerWeapon] {
        &self.weapons
    }

    /// Selects all weapons with zero, or one weapon by its one-based slot.
    pub fn select(&mut self, slot: usize) {
        match slot {
            0 => self.selected = None,
            n if n <= self.weapons.len() => self.selected = Some(n - 1),
            _ => {}
        }
    }

    /// Samples the active trigger on the queue clock and emits due shots.
    /// At most one shot per weapon is produced by a single simulation sample.
    pub fn fire(
        &mut self,
        trigger: bool,
        clock_ms: u32,
    ) -> impl Iterator<Item = (usize, &PlayerWeapon)> {
        let selected = self.selected;
        self.weapons
            .iter_mut()
            .enumerate()
            .filter_map(move |(index, weapon)| {
                if !trigger || selected.is_some_and(|selected| selected != index) {
                    return None;
                }
                if weapon
                    .last_fire_ms
                    .is_some_and(|last| clock_ms.wrapping_sub(last) < weapon.interval_ms)
                {
                    return None;
                }
                weapon.last_fire_ms = Some(clock_ms);
                Some((index, &*weapon))
            })
    }
}

/// Resolves starting weapons from the hero's mounted CTLD and CPT resources.
pub fn starting_player_weapons(engine: &Engine, cockpit: StartingCockpit) -> PlayerWeapons {
    let control = engine
        .loaded
        .as_ref()
        .and_then(|mission| {
            mission
                .mission_assets
                .object_component_controls
                .get(cockpit.object_index)
        })
        .and_then(|controls| controls.get(cockpit.component_index))
        .and_then(Option::as_ref);
    PlayerWeapons {
        weapons: control.map_or_else(Vec::new, prepare_weapons),
        ..PlayerWeapons::default()
    }
}

fn prepare_weapons(control: &PreparedComponentControl) -> Vec<PlayerWeapon> {
    let Some(points) = &control.point_list else {
        return Vec::new();
    };
    control
        .typed_records
        .iter()
        .filter(|record| record.tag == 2)
        .filter_map(|record| {
            let projectile_archive =
                crate::fixed_words_ascii_c_string(&record.fixed_words[27..35])?;
            let projectile = crate::fixed_words_ascii_c_string(&record.fixed_words[35..43])?;
            if projectile_archive.is_empty() || projectile.is_empty() {
                return None;
            }
            let row = control
                .timeline_rows
                .get(*record.values.first()? as usize)?;
            if row.flags & 0x40 != 0 {
                return None;
            }
            let muzzle = points.points.get(row.raw_parameters[1] as usize)?;
            if muzzle.raw_signed_field_04 < 0
                || muzzle.raw_signed_field_04 != muzzle.raw_signed_field_08
            {
                return None;
            }
            let interval_ms =
                f32::from_bits(record.fixed_words[14]) + f32::from_bits(record.fixed_words[15]);
            if !interval_ms.is_finite() || interval_ms < 0.0 {
                return None;
            }
            let effect = match &control.effect_section {
                PreparedControlEffectSection::Parsed(rows) => control
                    .node_effect_links
                    .iter()
                    .find(|&&(group, node, _)| {
                        Some(group) == control.initial_effect_group
                            && node as usize == row.node_index
                    })
                    .and_then(|&(group, _, selector)| {
                        rows.iter()
                            .find(|row| row.table_index == group && row.selector == selector)
                    })
                    .and_then(|row| {
                        Some((
                            crate::bounded_ascii_c_string(&row.resource.archive)?,
                            crate::bounded_ascii_c_string(&row.resource.name)?,
                        ))
                    }),
                _ => None,
            };
            Some(PlayerWeapon {
                projectile,
                projectile_archive,
                muzzle: muzzle.clone(),
                effect,
                // shortcut: begin the next cycle after the two configured
                // waits and a 60 Hz step for each of the four native phases.
                interval_ms: (interval_ms + 4.0 * (1000.0 / 60.0)).ceil() as u32,
                last_fire_ms: None,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_respects_selection_and_independent_weapon_waits() {
        let mut weapons = PlayerWeapons {
            selected: None,
            weapons: [100, 250]
                .map(|interval_ms| PlayerWeapon {
                    projectile: String::from("test"),
                    projectile_archive: String::from("test.rlb"),
                    muzzle: PreparedControlPoint {
                        raw_flags: 0,
                        raw_signed_field_04: 0,
                        raw_signed_field_08: 0,
                        position: [0.0; 3],
                        direction: [0.0, 1.0, 0.0],
                        name: [0; 32],
                    },
                    effect: None,
                    interval_ms,
                    last_fire_ms: None,
                })
                .into(),
        };
        let mut fired = |trigger, t| {
            weapons
                .fire(trigger, t)
                .map(|(slot, _)| slot)
                .collect::<Vec<_>>()
        };
        assert!(fired(false, 0).is_empty());
        assert_eq!(fired(true, 0), [0, 1]);
        assert!(fired(true, 99).is_empty());
        assert_eq!(fired(true, 100), [0]);
        weapons.select(2);
        assert!(weapons.fire(true, 249).next().is_none());
        assert_eq!(weapons.fire(true, 250).next().unwrap().0, 1);
        weapons.select(0);
        assert_eq!(weapons.fire(true, 251).next().unwrap().0, 0);
    }
}
