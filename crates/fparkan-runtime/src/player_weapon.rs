use crate::{Engine, StartingCockpit};
use fparkan_assets::{
    PreparedComponentControl, PreparedControlEffectSection, PreparedControlPoint,
};

/// The starting hero's selectable weapon devices, in resource order.
#[derive(Default)]
pub struct PlayerWeapons {
    weapons: Vec<PlayerWeapon>,
    selected: Option<usize>,
    last_sample_ms: Option<u32>,
    clock_ms: f64,
}

/// A manual-fire device prepared from its CTLD record and effect rows.
pub struct PlayerWeapon {
    /// Name of the projectile prototype.
    pub projectile: String,
    /// Archive named by the weapon's projectile descriptor.
    pub projectile_archive: String,
    /// Optional type-4 effect started by this weapon.
    pub effect: Option<(String, String)>,
    barrels: Vec<Barrel>,
    barrel_index: usize,
    rounds_remaining: Option<u32>,
    shot_hold_ms: f64,
    reload_ms: f64,
    phase: FirePhase,
    phase_start_ms: f64,
    phase_end_ms: f64,
}

struct Barrel {
    muzzle: PreparedControlPoint,
    rate: f64,
    blend: f64,
}

#[derive(PartialEq)]
enum FirePhase {
    Idle,
    Shot,
    Recover,
    Finish,
    Reload,
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
    ) -> impl Iterator<Item = (usize, &PlayerWeapon, &PreparedControlPoint)> {
        if let Some(last) = self.last_sample_ms.replace(clock_ms) {
            self.clock_ms += f64::from(clock_ms.wrapping_sub(last));
        }
        let now = self.clock_ms;
        let selected = self.selected;
        self.weapons
            .iter_mut()
            .enumerate()
            .filter_map(move |(index, weapon)| {
                let start = trigger && selected.is_none_or(|selected| selected == index);
                let barrel = weapon.sample_fire(start, now)?;
                Some((index, &*weapon, &weapon.barrels[barrel].muzzle))
            })
    }
}

impl PlayerWeapon {
    fn sample_fire(&mut self, trigger: bool, now: f64) -> Option<usize> {
        let mut shot = None;
        while now > self.phase_end_ms || self.phase == FirePhase::Idle {
            // shortcut: leave a second overdue shot pending until the next simulation sample.
            if self.phase == FirePhase::Shot && shot.is_some() {
                break;
            }
            // Control rebases a zero-length phase to the current sample.
            let start = if self.phase_start_ms == self.phase_end_ms {
                now
            } else {
                self.phase_end_ms
            };
            self.phase_start_ms = start;
            let barrel = &mut self.barrels[self.barrel_index];
            let delay = match self.phase {
                FirePhase::Idle | FirePhase::Reload => {
                    if !trigger || self.rounds_remaining == Some(0) {
                        self.phase = FirePhase::Idle;
                        break;
                    }
                    self.phase = FirePhase::Shot;
                    barrel.travel_to(0.5)
                }
                FirePhase::Shot => {
                    shot = Some(self.barrel_index);
                    self.phase = FirePhase::Recover;
                    self.shot_hold_ms.max(barrel.travel_to(0.5))
                }
                FirePhase::Recover => {
                    self.phase = FirePhase::Finish;
                    barrel.travel_to(1.0)
                }
                FirePhase::Finish => {
                    barrel.blend = 0.0;
                    self.phase = FirePhase::Reload;
                    self.barrel_index = (self.barrel_index + 1) % self.barrels.len();
                    if let Some(rounds) = &mut self.rounds_remaining {
                        *rounds -= 1;
                    }
                    self.reload_ms
                }
            };
            self.phase_end_ms = start + delay;
        }
        shot
    }

    /// Remaining shots, or `None` for an unlimited weapon.
    pub fn rounds_remaining(&self) -> Option<u32> {
        self.rounds_remaining
    }

    /// Conservative lower bound used to size the projectile draw pool.
    pub fn interval_ms(&self) -> u32 {
        let fastest = self
            .barrels
            .iter()
            .map(|barrel| 500.0 / barrel.rate)
            .fold(f64::INFINITY, f64::min);
        ((2.0 * fastest + self.shot_hold_ms + self.reload_ms) as u32).max(1)
    }
}

impl Barrel {
    fn travel_to(&mut self, blend: f64) -> f64 {
        let duration = (blend - self.blend).abs() * 1000.0 / self.rate;
        self.blend = blend;
        duration
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
            let mut firing_rows = record
                .values
                .iter()
                .filter_map(|&index| control.timeline_rows.get(index as usize))
                .filter(|row| row.flags & 0x40 == 0);
            let row = firing_rows.next()?;
            let barrels = std::iter::once(row)
                .chain(firing_rows)
                .map(|row| {
                    let muzzle = points.points.get(row.raw_parameters[1] as usize)?;
                    let rate = f32::from_bits(row.raw_parameters[2]);
                    if muzzle.raw_signed_field_04 < 0
                        || muzzle.raw_signed_field_04 != muzzle.raw_signed_field_08
                        || !rate.is_finite()
                        || rate <= 0.0
                        || row.flags & 1 != 0
                        || !(0.0..=1.0).contains(&row.initial_blend)
                    {
                        return None;
                    }
                    Some(Barrel {
                        muzzle: muzzle.clone(),
                        rate: f64::from(rate),
                        blend: f64::from(row.initial_blend),
                    })
                })
                .collect::<Option<Vec<_>>>()?;
            let reload_ms = f64::from(f32::from_bits(record.fixed_words[14]));
            let shot_hold_ms = f64::from(f32::from_bits(record.fixed_words[15]));
            if ![reload_ms, shot_hold_ms]
                .into_iter()
                .all(|ms| ms.is_finite() && ms >= 0.0)
            {
                return None;
            }
            let rounds = f32::from_bits(record.fixed_words[11]).round_ties_even();
            let rounds_remaining = if rounds == -1.0 {
                None
            } else if (0.0..2_147_483_648.0).contains(&rounds) {
                Some(rounds as u32)
            } else {
                return None;
            };
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
                effect,
                barrels,
                barrel_index: 0,
                rounds_remaining,
                shot_hold_ms,
                reload_ms,
                phase: FirePhase::Idle,
                phase_start_ms: 0.0,
                phase_end_ms: 0.0,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shot_finishes_after_trigger_release_and_then_spends_ammo() {
        let mut weapons = PlayerWeapons {
            weapons: vec![PlayerWeapon {
                projectile: String::from("test"),
                projectile_archive: String::from("test.rlb"),
                effect: None,
                barrels: vec![Barrel {
                    muzzle: PreparedControlPoint {
                        raw_flags: 0,
                        raw_signed_field_04: 0,
                        raw_signed_field_08: 0,
                        position: [0.0; 3],
                        direction: [0.0, 1.0, 0.0],
                        name: [0; 32],
                    },
                    rate: 50.0,
                    blend: 0.0,
                }],
                barrel_index: 0,
                rounds_remaining: Some(1),
                shot_hold_ms: 0.0,
                reload_ms: 30.0,
                phase: FirePhase::Idle,
                phase_start_ms: 0.0,
                phase_end_ms: 0.0,
            }],
            ..PlayerWeapons::default()
        };
        assert!(weapons.fire(true, 0).next().is_none());
        assert!(weapons.fire(false, 10).next().is_none());
        assert_eq!(weapons.fire(false, 11).next().unwrap().0, 0);
        assert_eq!(weapons.weapons()[0].rounds_remaining(), Some(1));
        assert!(weapons.fire(false, 22).next().is_none());
        assert_eq!(weapons.weapons()[0].rounds_remaining(), Some(0));
        assert!(weapons.fire(true, 100).next().is_none());
    }
}
