use crate::{weapon_deployment::WeaponDeployment, Engine, FiringEffectInstance, StartingCockpit};
use fparkan_assets::{
    PreparedComponentControl, PreparedControlEffectRow, PreparedControlEffectSection,
    PreparedControlPoint, PreparedControlTimelineRow,
};

/// A unit's selectable weapon devices, in resource order.
#[derive(Clone, Default)]
pub struct PlayerWeapons {
    weapons: Vec<PlayerWeapon>,
    selected: Option<usize>,
    last_sample_ms: Option<u32>,
    clock_ms: f64,
}

/// A manual-fire device prepared from its CTLD record and effect rows.
#[derive(Clone)]
pub struct PlayerWeapon {
    /// Component containing the barrel's CPT point.
    pub component_index: usize,
    pub(super) control_record_index: usize,
    pub(super) charge: Option<crate::mobile_budget::Charge>,
    pub(super) energy_waits: u64,
    pub(super) autonomous_fire_distance: f32,
    /// Name of the projectile prototype.
    pub projectile: String,
    /// Archive named by the weapon's projectile descriptor.
    pub projectile_archive: String,
    /// Optional type-4 effect started by this weapon.
    pub effect: Option<(String, String)>,
    effect_row: Option<(usize, usize)>,
    effect_node: Option<usize>,
    windup_sound_rows: Vec<(usize, usize)>,
    windup_started: bool,
    deployment: Option<WeaponDeployment>,
    barrels: Vec<Barrel>,
    barrel_index: usize,
    rounds_remaining: Option<u32>,
    shot_hold_ms: f64,
    reload_ms: f64,
    phase: FirePhase,
    phase_start_ms: f64,
    phase_end_ms: f64,
}

#[derive(Clone)]
struct Barrel {
    muzzle: PreparedControlPoint,
    row: PreparedControlTimelineRow,
    rate: f64,
    blend: f64,
    from_blend: f64,
}

#[derive(Clone, PartialEq)]
enum FirePhase {
    Idle,
    Shot,
    Recover,
    Finish,
    Reload,
}

impl PlayerWeapons {
    /// Reads the authoritative Control charge before sampling this weapon bank.
    pub fn read_control_energy(&mut self, engine: &Engine, handle: crate::ObjectHandle) {
        if let Some(mission) = &engine.loaded {
            crate::mobile_budget::read(mission, handle, &mut self.weapons);
        }
    }

    /// Publishes completed shots to Control before its next resource budget.
    pub fn publish_control_energy(&mut self, engine: &mut Engine, handle: crate::ObjectHandle) {
        if let Some(mission) = &mut engine.loaded {
            crate::mobile_budget::publish(mission, handle, &mut self.weapons);
        }
    }

    /// Returns the prepared devices in selection order.
    pub fn weapons(&self) -> &[PlayerWeapon] {
        &self.weapons
    }

    /// Control sound instances whose wind-up began on this sample.
    pub fn started_windup_sounds(
        &self,
        object_index: usize,
    ) -> impl Iterator<Item = FiringEffectInstance> + '_ {
        self.weapons
            .iter()
            .filter(|weapon| weapon.windup_started)
            .flat_map(move |weapon| weapon.windup_sounds(object_index))
    }

    /// Current blends of the model nodes explicitly linked to firing effects.
    pub fn firing_effect_phases(
        &self,
        object_index: usize,
    ) -> impl Iterator<Item = (FiringEffectInstance, f32)> + '_ {
        self.weapons.iter().filter_map(move |weapon| {
            let node = weapon.effect_node?;
            let blend = self
                .weapons
                .iter()
                .rev()
                .filter(|track| track.component_index == weapon.component_index)
                .find_map(|track| {
                    let index = track.barrels.iter().rposition(|barrel| {
                        barrel.row.node_index == node && barrel.row.flags & 4 == 0
                    })?;
                    Some(track.barrel_blend(index, self.clock_ms))
                })?;
            Some((weapon.firing_effect(object_index)?, blend))
        })
    }

    /// Samples the weapon mount and barrel tracks after advancing the bank.
    pub fn apply_firing_pose(&self, component: usize, node_times: &mut [Option<f32>]) {
        for weapon in self
            .weapons
            .iter()
            .filter(|weapon| weapon.component_index == component)
        {
            if let Some(deployment) = &weapon.deployment {
                deployment.apply(node_times);
            }
            for (index, barrel) in weapon.barrels.iter().enumerate() {
                if barrel.row.flags & 4 != 0 {
                    continue;
                }
                let Some(time) = node_times.get_mut(barrel.row.node_index) else {
                    continue;
                };
                let blend = weapon.barrel_blend(index, self.clock_ms);
                *time = Some((1.0 - blend) * barrel.row.frame_a + blend * barrel.row.frame_b);
            }
        }
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
        self.fire_where(clock_ms, move |_, _, _| trigger)
    }

    pub(super) fn fire_where(
        &mut self,
        clock_ms: u32,
        mut trigger: impl FnMut(usize, &PlayerWeapon, &PreparedControlPoint) -> bool,
    ) -> impl Iterator<Item = (usize, &PlayerWeapon, &PreparedControlPoint)> {
        let elapsed_ms = self
            .last_sample_ms
            .replace(clock_ms)
            .map_or(0, |last| clock_ms.wrapping_sub(last));
        self.clock_ms += f64::from(elapsed_ms);
        let now = self.clock_ms;
        let selected = self.selected;
        self.weapons
            .iter_mut()
            .enumerate()
            .filter_map(move |(index, weapon)| {
                let selected = selected.is_none_or(|selected| selected == index);
                let mut start =
                    selected && trigger(index, weapon, &weapon.barrels[weapon.barrel_index].muzzle);
                if let Some(deployment) = &mut weapon.deployment {
                    let cycling = matches!(
                        weapon.phase,
                        FirePhase::Shot | FirePhase::Recover | FirePhase::Finish
                    );
                    start = deployment.sample(elapsed_ms, selected, start, cycling);
                }
                let barrel = weapon.sample_fire(start, now)?;
                Some((index, &*weapon, &weapon.barrels[barrel].muzzle))
            })
    }
}

impl PlayerWeapon {
    fn barrel_blend(&self, index: usize, now: f64) -> f32 {
        let barrel = &self.barrels[index];
        let mut blend = barrel.blend;
        if index == self.barrel_index && matches!(self.phase, FirePhase::Shot | FirePhase::Finish) {
            let duration = self.phase_end_ms - self.phase_start_ms;
            if duration > 0.0 {
                let progress = ((now - self.phase_start_ms) / duration).clamp(0.0, 1.0);
                blend = barrel.from_blend + progress * (blend - barrel.from_blend);
            }
        }
        let blend = blend as f32;
        if barrel.row.flags & 2 != 0 {
            1.0 - blend
        } else {
            blend
        }
    }

    fn sample_fire(&mut self, trigger: bool, now: f64) -> Option<usize> {
        let mut shot = None;
        self.windup_started = false;
        while now > self.phase_end_ms || self.phase == FirePhase::Idle {
            // shortcut: leave a second overdue shot pending until the next simulation sample.
            if self.phase == FirePhase::Shot && shot.is_some() {
                break;
            }
            // An idle device has no scheduled start. Control also rebases
            // zero-length phases to the current sample.
            let start = if self.phase == FirePhase::Idle || self.phase_start_ms == self.phase_end_ms
            {
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
                    if self
                        .charge
                        .as_ref()
                        .is_some_and(|charge| !charge.can_start())
                    {
                        self.energy_waits += 1;
                        self.phase = FirePhase::Idle;
                        self.phase_start_ms = now;
                        self.phase_end_ms = now;
                        break;
                    }
                    self.phase = FirePhase::Shot;
                    self.windup_started = true;
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
                    if let Some(charge) = &mut self.charge {
                        charge.finish_shot();
                    }
                    self.reload_ms
                }
            };
            self.phase_end_ms = start + delay;
        }
        shot
    }

    /// The Control effect instance bound to this weapon's release.
    pub fn firing_effect(&self, object_index: usize) -> Option<FiringEffectInstance> {
        self.effect_row
            .map(|(table_index, row_index)| FiringEffectInstance {
                object_index,
                component_index: self.component_index,
                table_index,
                row_index,
            })
    }

    /// Prepared node-attached sound rows belonging to this device.
    pub fn windup_sounds(
        &self,
        object_index: usize,
    ) -> impl Iterator<Item = FiringEffectInstance> + '_ {
        self.windup_sound_rows
            .iter()
            .map(move |&(table_index, row_index)| FiringEffectInstance {
                object_index,
                component_index: self.component_index,
                table_index,
                row_index,
            })
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
        self.from_blend = self.blend;
        self.blend = blend;
        duration
    }
}

/// Resolves starting weapons from the hero's mounted CTLD and CPT resources.
pub fn starting_player_weapons(engine: &Engine, cockpit: StartingCockpit) -> PlayerWeapons {
    let Some(mission) = &engine.loaded else {
        return PlayerWeapons::default();
    };
    let assets = &mission.mission_assets;
    let Some(control) = assets
        .object_component_controls
        .get(cockpit.object_index)
        .and_then(|controls| controls.get(cockpit.component_index))
        .and_then(Option::as_ref)
    else {
        return PlayerWeapons::default();
    };
    let mut weapons = prepare_weapons(cockpit.component_index, control);
    let model = assets
        .visuals_for_object(cockpit.object_index)
        .get(cockpit.component_index)
        .and_then(|id| assets.visual_by_id(*id))
        .and_then(|visual| assets.model_by_id(visual.model_id?));
    if let Some(model) = model {
        for weapon in &mut weapons {
            weapon.deployment = WeaponDeployment::prepare(
                control,
                &model.validated,
                weapon
                    .barrels
                    .iter()
                    .map(|barrel| barrel.muzzle.raw_signed_field_04 as u32),
            );
        }
    }
    PlayerWeapons {
        weapons,
        ..PlayerWeapons::default()
    }
}

/// Resolves weapon devices for placed and prepared production warbots.
pub fn prepared_warbot_weapons(
    engine: &Engine,
) -> impl Iterator<Item = (usize, PlayerWeapons)> + '_ {
    engine.loaded.iter().flat_map(|mission| {
        mission
            .object_drafts
            .iter()
            .zip(&mission.mission_assets.object_component_controls)
            .enumerate()
            .filter(|(_, (draft, _))| {
                draft.unit_dat_header_flags == Some(crate::boarding::WARBOT_CLASS)
            })
            .map(|(index, (_, controls))| (index, prepare_unit_weapons(controls)))
    })
}

fn prepare_weapons(
    component_index: usize,
    control: &PreparedComponentControl,
) -> Vec<PlayerWeapon> {
    let Some(points) = &control.point_list else {
        return Vec::new();
    };
    control
        .typed_records
        .iter()
        .enumerate()
        .filter(|(_, record)| record.tag == 2)
        .filter_map(|(control_record_index, record)| {
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
                        || (muzzle.raw_signed_field_08 != -1
                            && muzzle.raw_signed_field_04 != muzzle.raw_signed_field_08)
                        || !rate.is_finite()
                        || rate <= 0.0
                        || row.flags & 1 != 0
                        || !(0.0..=1.0).contains(&row.initial_blend)
                    {
                        return None;
                    }
                    Some(Barrel {
                        muzzle: muzzle.clone(),
                        row: *row,
                        rate: f64::from(rate),
                        blend: f64::from(row.initial_blend),
                        from_blend: f64::from(row.initial_blend),
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
            let effect_row = firing_effect_row(control, row);
            let effect_node = effect_row
                .filter(|effect| {
                    control.node_effect_links.contains(&(
                        effect.table_index,
                        row.node_index as u32,
                        effect.selector,
                    ))
                })
                .map(|_| row.node_index);
            let effect = effect_row.and_then(|row| {
                Some((
                    crate::bounded_ascii_c_string(&row.resource.archive)?,
                    crate::bounded_ascii_c_string(&row.resource.name)?,
                ))
            });
            Some(PlayerWeapon {
                component_index,
                control_record_index,
                charge: None,
                energy_waits: 0,
                autonomous_fire_distance: f32::from_bits(record.fixed_words[19]),
                projectile,
                projectile_archive,
                effect,
                effect_row: effect_row.map(|row| (row.table_index, row.row_index)),
                effect_node,
                windup_sound_rows: control
                    .weapon_windup_sound_rows(record)
                    .map(|row| (row.table_index, row.row_index))
                    .collect(),
                windup_started: false,
                deployment: None,
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

fn firing_effect_row<'a>(
    control: &'a PreparedComponentControl,
    muzzle: &PreparedControlTimelineRow,
) -> Option<&'a PreparedControlEffectRow> {
    let PreparedControlEffectSection::Parsed(rows) = &control.effect_section else {
        return None;
    };
    let group = control.initial_effect_group?;
    if let Some(&(_, _, selector)) = control
        .node_effect_links
        .iter()
        .find(|&&(table, node, _)| table == group && node as usize == muzzle.node_index)
    {
        rows.iter()
            .find(|row| row.opcode == 4 && row.table_index == group && row.selector == selector)
    } else {
        // Separate gun meshes can have one firing record and an effect whose
        // first control point is the same muzzle. Ambiguous rows stay unresolved.
        if control
            .typed_records
            .iter()
            .filter(|record| record.tag == 2)
            .nth(1)
            .is_some()
        {
            return None;
        }
        let mut candidates = rows.iter().filter(|row| {
            row.opcode == 4
                && row.table_index == group
                && row.raw_condition == [0; 3]
                && row.point_offsets[0] == muzzle.raw_parameters[1]
        });
        let row = candidates.next()?;
        if candidates.next().is_some() {
            return None;
        }
        Some(row)
    }
}

pub(super) fn prepare_unit_weapons(controls: &[Option<PreparedComponentControl>]) -> PlayerWeapons {
    PlayerWeapons {
        weapons: controls
            .iter()
            .enumerate()
            .filter_map(|(index, control)| Some((index, control.as_ref()?)))
            .flat_map(|(index, control)| prepare_weapons(index, control))
            .collect(),
        ..PlayerWeapons::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_constructor_deploys_cannon_before_firing() {
        use crate::{
            create, initial_component_node_times, load_gameplay_mission, loaded_mission_assets,
            starting_cockpit, EngineServices, MissionMode, MissionRequest, MissionStartupInput,
        };
        use fparkan_animation::AnimationTime;
        use fparkan_msh::node38_sampled_hierarchy_at_times;
        use fparkan_vfs::DirectoryVfs;
        use std::sync::Arc;

        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed root");
        let mut engine = create(EngineServices::new(Arc::new(DirectoryVfs::new(root)))).unwrap();
        load_gameplay_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/CAMPAIGN/CAMPAIGN.00/Mission.02/data.tma".into(),
                mode: MissionMode::SingleMission,
            },
            MissionStartupInput {
                queue_time_ms: 0,
                research_creation_gate: false,
            },
            None,
        )
        .unwrap();
        let hero = starting_cockpit(&engine).unwrap();
        let mut weapons = starting_player_weapons(&engine, hero);
        assert!(weapons
            .weapons()
            .iter()
            .all(|weapon| weapon.deployment.is_some()));
        let slot = weapons
            .weapons()
            .iter()
            .position(|weapon| weapon.projectile == "bb_h_01")
            .unwrap();
        let effect = weapons.weapons()[slot]
            .firing_effect(hero.object_index)
            .unwrap();
        weapons.select(slot + 1);
        let assets = loaded_mission_assets(&engine).unwrap();
        let visual = assets
            .visual_by_id(assets.visuals_for_object(hero.object_index)[hero.component_index])
            .unwrap();
        let model = &assets
            .model_by_id(visual.model_id.unwrap())
            .unwrap()
            .validated;
        let control = assets.object_component_controls[hero.object_index][hero.component_index]
            .as_ref()
            .unwrap();
        let mut times: Vec<_> = initial_component_node_times(model.node_count, &control.bindings)
            .unwrap()
            .into_iter()
            .map(|time| time.map(|time| time.0))
            .collect();
        assert!(weapons.fire(true, 0).next().is_none());
        assert!(weapons.fire(false, 225).next().is_none());
        weapons.apply_firing_pose(hero.component_index, &mut times);
        assert_eq!(times[11], Some(44.7));
        assert_eq!(times[7], Some(42.0));
        assert!(weapons.fire(false, 450).next().is_none());
        assert!(weapons.fire(false, 499).next().is_none());
        assert!(weapons
            .started_windup_sounds(hero.object_index)
            .next()
            .is_none());
        for (now, expected) in [
            (500, 0.0),
            (563, 0.252),
            (625, 0.5),
            (626, 0.5),
            (688, 0.748),
            (752, 0.0),
        ] {
            let shot = weapons
                .fire(false, now)
                .next()
                .map(|(slot, _, muzzle)| (slot, muzzle.clone()));
            assert_eq!(shot.is_some(), now == 626);
            if let Some((fired_slot, muzzle)) = shot {
                assert_eq!(fired_slot, slot);
                weapons.apply_firing_pose(hero.component_index, &mut times);
                assert_eq!(times[11], Some(48.0));
                let sampled: Vec<_> = times.iter().map(|time| time.map(AnimationTime)).collect();
                let hierarchy = node38_sampled_hierarchy_at_times(model, &sampled).unwrap();
                let barrel = hierarchy.poses[muzzle.raw_signed_field_04 as usize];
                let origin = barrel.transform_point(muzzle.position);
                let forward = barrel.transform_point(std::array::from_fn(|axis| {
                    muzzle.position[axis] + muzzle.direction[axis]
                }));
                assert!(origin[1] > 1.0 && origin[2] > 0.1, "{origin:?}");
                assert!(forward[1] - origin[1] > 0.99, "{origin:?} -> {forward:?}");
            }
            let phase = weapons
                .firing_effect_phases(hero.object_index)
                .find_map(|(key, phase)| (key == effect).then_some(phase))
                .expect("the authored effect is linked to the cannon node");
            assert!((phase - expected).abs() < 0.001, "{now} ms: {phase}");
        }
        assert!(weapons.fire(false, 753).next().is_none());
        assert!(weapons.fire(false, 978).next().is_none());
        weapons.apply_firing_pose(hero.component_index, &mut times);
        assert_eq!(times[11], Some(45.3));
        assert!(weapons.fire(false, 1253).next().is_none());
        weapons.apply_firing_pose(hero.component_index, &mut times);
        assert_eq!(times[11], Some(42.0));
    }

    #[test]
    fn shot_finishes_after_trigger_release_and_spends_ammo_and_charge() {
        let mut weapons = PlayerWeapons {
            weapons: vec![PlayerWeapon {
                component_index: 0,
                control_record_index: 0,
                charge: None,
                energy_waits: 0,
                autonomous_fire_distance: 0.0,
                projectile: String::from("test"),
                projectile_archive: String::from("test.rlb"),
                effect: None,
                effect_row: Some((2, 3)),
                effect_node: Some(0),
                windup_sound_rows: vec![(0, 1)],
                windup_started: false,
                deployment: None,
                barrels: vec![Barrel {
                    muzzle: PreparedControlPoint {
                        raw_flags: 0,
                        raw_signed_field_04: 0,
                        raw_signed_field_08: 0,
                        position: [0.0; 3],
                        direction: [0.0, 1.0, 0.0],
                        name: [0; 32],
                    },
                    row: PreparedControlTimelineRow {
                        row_index: 0,
                        node_index: 0,
                        frame_a: 4.0,
                        frame_b: 8.0,
                        initial_blend: 0.0,
                        raw_parameters: [0; 4],
                        flags: 0,
                    },
                    rate: 50.0,
                    blend: 0.0,
                    from_blend: 0.0,
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
        weapons.weapons[0].charge = Some(crate::mobile_budget::Charge {
            capacity: 10.0,
            cost: 4.0,
            amount: 8.0,
        });
        let mut node_times = [None];
        assert!(weapons.fire(true, 0).next().is_none());
        assert_eq!(
            weapons.started_windup_sounds(7).collect::<Vec<_>>(),
            [FiringEffectInstance {
                object_index: 7,
                component_index: 0,
                table_index: 0,
                row_index: 1,
            }]
        );
        assert!(weapons.fire(false, 5).next().is_none());
        assert!(weapons.started_windup_sounds(7).next().is_none());
        weapons.apply_firing_pose(0, &mut node_times);
        assert_eq!(node_times, [Some(5.0)]);
        let effect = weapons.weapons()[0].firing_effect(7).unwrap();
        assert_eq!(weapons.firing_effect_phases(7).next(), Some((effect, 0.25)));
        assert!(weapons.fire(false, 10).next().is_none());
        weapons.apply_firing_pose(0, &mut node_times);
        assert_eq!(node_times, [Some(6.0)]);
        assert_eq!(weapons.firing_effect_phases(7).next(), Some((effect, 0.5)));
        let (slot, weapon, _) = weapons.fire(false, 11).next().unwrap();
        assert_eq!(slot, 0);
        assert_eq!(
            weapon.firing_effect(7),
            Some(FiringEffectInstance {
                object_index: 7,
                component_index: 0,
                table_index: 2,
                row_index: 3,
            })
        );
        assert!(weapons.fire(false, 16).next().is_none());
        weapons.apply_firing_pose(0, &mut node_times);
        assert_eq!(node_times, [Some(7.0)]);
        assert_eq!(weapons.firing_effect_phases(7).next(), Some((effect, 0.75)));
        assert_eq!(weapons.weapons()[0].rounds_remaining(), Some(1));
        assert_eq!(weapons.weapons[0].charge.as_ref().unwrap().amount, 8.0);
        assert!(weapons.fire(false, 22).next().is_none());
        weapons.apply_firing_pose(0, &mut node_times);
        assert_eq!(node_times, [Some(4.0)]);
        assert_eq!(weapons.firing_effect_phases(7).next(), Some((effect, 0.0)));
        assert_eq!(weapons.weapons()[0].rounds_remaining(), Some(0));
        assert_eq!(weapons.weapons[0].charge.as_ref().unwrap().amount, 4.0);
        assert!(weapons.fire(true, 100).next().is_none());
        weapons.weapons[0].rounds_remaining = None;
        weapons.weapons[0].charge.as_mut().unwrap().amount = 0.0;
        assert!(weapons.fire(true, 150).next().is_none());
        assert!(weapons.weapons[0].energy_waits > 0);
        weapons.weapons[0].charge.as_mut().unwrap().amount = 4.0;
        assert!(weapons.fire(true, 10_000).next().is_none());
        assert!(weapons.fire(true, 10_011).next().is_some());
    }
}
