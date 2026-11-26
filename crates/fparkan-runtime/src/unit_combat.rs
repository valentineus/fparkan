use super::{
    apply_projectile_impact,
    autodemo::MOBILE_UNIT,
    combat_report::{CombatObject, CombatReport},
    default_weapon_target::{self, DefaultWeaponTarget},
    player_weapon::prepare_unit_weapons,
    projectile::{load_projectile, segment_sphere_fraction, unit_sphere, ANIMAL_UNIT_CLASS},
    projectile_terrain_hit, projectile_unit_hit,
    turret_aim::{hull_local_point, TurretAim},
    unit_patrol::UnitPatrol,
    weapon_pose::WeaponPoses,
    Engine, MissionAssets, MissionDocument, MissionObjectDraft, ObjectHandle, PlayerProjectile,
    PlayerWeapons, ProjectileImpact,
};
use fparkan_vfs::Vfs;
use fparkan_world::{identity_metadata, transform_state};
use std::sync::Arc;

// Behavior's default Patrol_Attack_Range and Attack_MaxFireDistance.
const ATTACK_RANGE: f32 = 400.0;
const ATTACK_STOP_RANGE: f32 = 200.0;

/// An autonomous projectile type, in render-pool order.
pub struct AutoDemoProjectileType {
    /// Prototype archive.
    pub archive: String,
    /// Prototype name.
    pub name: String,
    /// Maximum concurrent flights for the prepared weapons.
    pub capacity: u32,
    speed: f32,
    lifetime_ms: u32,
    volume: Option<f32>,
    autonomous_fire_distance: f32,
}

/// The prepared scene instance associated with a weapon's firing effect.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FiringEffectInstance {
    /// Index of the unit's prepared object draft.
    pub object_index: usize,
    /// Index of the component owning the effect.
    pub component_index: usize,
    /// Control effect table.
    pub table_index: usize,
    /// Effect row within that table.
    pub row_index: usize,
}

/// One autonomous combat event on the mission clock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AutoDemoCombatEvent {
    /// The wind-up started a node-attached weapon sound.
    WindupSound(FiringEffectInstance),
    /// A weapon released a projectile.
    Shot {
        /// Owner of the weapon.
        shooter: ObjectHandle,
        /// Object the weapon was aimed at.
        target: ObjectHandle,
        /// Projectile type in render-pool order.
        projectile: usize,
        /// Optional weapon effect in shared resource order.
        effect: Option<usize>,
        /// Prepared component effect activated by this release.
        effect_instance: Option<FiringEffectInstance>,
        /// World-space muzzle position as raw IEEE-754 coordinates.
        muzzle_bits: [u32; 3],
    },
    /// A projectile exhausted a unit's health.
    Destroyed {
        /// Owner of the weapon.
        shooter: ObjectHandle,
        /// Unit removed from the world.
        target: ObjectHandle,
    },
    /// An autonomous shot changed a building part's health.
    BuildingHit {
        /// Owner of the weapon.
        shooter: ObjectHandle,
        /// Building struck by the projectile.
        target: ObjectHandle,
        /// Provider row associated with the struck part.
        node: u32,
        /// Remaining health of the part as raw f32 bits.
        remaining_bits: u32,
    },
}

pub(super) struct AutoDemoCombat {
    units: Vec<Unit>,
    reserve: Vec<Unit>,
    hostile: Vec<Vec<bool>>,
    guard_clans: Vec<bool>,
    projectiles: Vec<AutoDemoProjectileType>,
    weapon_effects: Vec<(String, String)>,
    flights: Vec<Flight>,
    finished_segments: Vec<(usize, [[f32; 3]; 2])>,
    pose_failures: super::WeaponPoseReport,
    pub(super) report: CombatReport,
    pub(super) unavailable: usize,
}

#[derive(Clone, Copy)]
pub(super) struct CombatTarget {
    pub(super) handle: ObjectHandle,
    pub(super) center: [f32; 3],
    radius: f32,
    pub(super) in_range: bool,
}

#[derive(Clone)]
struct Unit {
    handle: ObjectHandle,
    index: usize,
    class: u32,
    original_id: u32,
    weapons: PlayerWeapons,
    slots: Vec<Option<usize>>,
    effects: Vec<Option<usize>>,
    poses: WeaponPoses,
    idle_target: DefaultWeaponTarget,
}

impl Unit {
    fn uses_default_targeting(&self, patrol: Option<&UnitPatrol>) -> bool {
        self.class == super::boarding::WARBOT_CLASS
            && patrol.is_none_or(|patrol| !patrol.is_patrolling(self.handle))
    }
}

struct Flight {
    shooter: ObjectHandle,
    shooter_identity: CombatObject,
    projectile: usize,
    pose: PlayerProjectile,
    segment: Option<[[f32; 3]; 2]>,
}

impl AutoDemoCombat {
    pub(super) fn new(
        vfs: &Arc<dyn Vfs>,
        mission: &MissionDocument,
        drafts: &[MissionObjectDraft],
        handles: &[ObjectHandle],
        assets: &MissionAssets,
        reserve_start: usize,
        guard_clans: Vec<bool>,
    ) -> Self {
        let hostile = mission
            .clans
            .iter()
            .map(|clan| {
                mission
                    .clans
                    .iter()
                    .map(|other| {
                        clan.relations.iter().any(|relation| {
                            relation.relation_value == 0
                                && relation.other_clan_name.raw == other.name.raw
                        })
                    })
                    .collect()
            })
            .collect();
        let mut battle = Self {
            units: Vec::new(),
            reserve: Vec::new(),
            hostile,
            guard_clans,
            projectiles: Vec::new(),
            weapon_effects: Vec::new(),
            flights: Vec::new(),
            finished_segments: Vec::new(),
            pose_failures: super::WeaponPoseReport::default(),
            report: CombatReport::default(),
            unavailable: 0,
        };
        for (index, (draft, &handle)) in drafts.iter().zip(handles).enumerate() {
            let class = draft.unit_dat_header_flags.unwrap_or(0);
            if class & MOBILE_UNIT == 0 && class != ANIMAL_UNIT_CLASS {
                continue;
            }
            let object = CombatObject {
                slot: handle.slot,
                generation: handle.generation,
                original_id: draft.original_id.map(|id| id.0),
            };
            let weapons = prepare_unit_weapons(
                assets
                    .object_component_controls
                    .get(index)
                    .map_or(&[], Vec::as_slice),
            );
            if class == ANIMAL_UNIT_CLASS && weapons.weapons().is_empty() {
                continue;
            }
            let slots = weapons
                .weapons()
                .iter()
                .map(|weapon| {
                    let archive = &weapon.projectile_archive;
                    let name = &weapon.projectile;
                    let slot = battle
                        .projectiles
                        .iter()
                        .position(|entry| {
                            entry.archive.eq_ignore_ascii_case(archive)
                                && entry.name.eq_ignore_ascii_case(name)
                        })
                        .or_else(|| {
                            let Ok(data) = load_projectile(vfs, archive, name) else {
                                battle.unavailable += 1;
                                battle.report.unavailable(object, "projectile_load", 0, 1);
                                return None;
                            };
                            battle.projectiles.push(AutoDemoProjectileType {
                                archive: archive.clone(),
                                name: name.clone(),
                                capacity: 0,
                                speed: data.speed,
                                lifetime_ms: data.lifetime_ms,
                                volume: data.volume,
                                autonomous_fire_distance: data.autonomous_fire_distance,
                            });
                            Some(battle.projectiles.len() - 1)
                        })?;
                    let entry = &mut battle.projectiles[slot];
                    entry.capacity = entry
                        .capacity
                        .saturating_add(entry.lifetime_ms.div_ceil(weapon.interval_ms()));
                    Some(slot)
                })
                .collect();
            let effects = weapons
                .weapons()
                .iter()
                .map(|weapon| {
                    let (archive, name) = weapon.effect.as_ref()?;
                    let index = battle
                        .weapon_effects
                        .iter()
                        .position(|(source_archive, source_name)| {
                            source_archive.eq_ignore_ascii_case(archive)
                                && source_name.eq_ignore_ascii_case(name)
                        })
                        .unwrap_or_else(|| {
                            let index = battle.weapon_effects.len();
                            battle.weapon_effects.push((archive.clone(), name.clone()));
                            index
                        });
                    Some(index)
                })
                .collect();
            battle.unavailable += usize::from(weapons.weapons().is_empty());
            if weapons.weapons().is_empty() {
                battle.report.unavailable(object, "empty_weapon_bank", 0, 1);
            }
            let units = if index < reserve_start {
                &mut battle.units
            } else {
                &mut battle.reserve
            };
            units.push(Unit {
                handle,
                index,
                class,
                weapons,
                slots,
                effects,
                poses: WeaponPoses::new(draft, assets, index),
                idle_target: DefaultWeaponTarget::default(),
                original_id: draft.original_id.map_or(handle.slot, |id| id.0),
            });
        }
        battle
    }

    pub(super) fn activate(
        &mut self,
        world: &fparkan_world::World,
        index: usize,
        handle: ObjectHandle,
    ) {
        self.units
            .retain(|unit| transform_state(world, unit.handle).is_ok());
        if let Some(template) = self.reserve.iter().find(|unit| unit.index == index) {
            self.units.push(Unit {
                handle,
                original_id: identity_metadata(world, handle)
                    .ok()
                    .and_then(|identity| identity.original_id)
                    .map_or(handle.slot, |id| id.0),
                ..template.clone()
            });
        }
    }

    pub(super) fn types(&self) -> &[AutoDemoProjectileType] {
        &self.projectiles
    }

    pub(super) fn weapon_effects(&self) -> &[(String, String)] {
        &self.weapon_effects
    }

    pub(super) fn guard_clan_count(&self) -> usize {
        self.guard_clans.iter().filter(|&&enabled| enabled).count()
    }

    pub(super) fn pose_report(&self) -> &super::WeaponPoseReport {
        &self.pose_failures
    }

    pub(super) fn turret_aim(&self, handle: ObjectHandle, component: usize) -> Option<TurretAim> {
        self.units
            .iter()
            .find(|unit| unit.handle == handle)?
            .poses
            .aim(component)
    }

    pub(super) fn flights(&self) -> impl Iterator<Item = (usize, PlayerProjectile)> + '_ {
        self.flights
            .iter()
            .map(|flight| (flight.projectile, flight.pose))
    }

    pub(super) fn segments(&self) -> impl Iterator<Item = (usize, [[f32; 3]; 2])> + '_ {
        self.flights
            .iter()
            .filter_map(|flight| Some((flight.projectile, flight.segment?)))
            .chain(self.finished_segments.iter().copied())
    }

    pub(super) fn targets(
        &self,
        world: &fparkan_world::World,
        assets: &MissionAssets,
        patrol: Option<&UnitPatrol>,
        controls: &[super::MissionControlDeviceTableState],
    ) -> Vec<(ObjectHandle, CombatTarget)> {
        self.select_targets(world, assets, patrol, controls, None)
    }

    fn select_targets(
        &self,
        world: &fparkan_world::World,
        assets: &MissionAssets,
        patrol: Option<&UnitPatrol>,
        controls: &[super::MissionControlDeviceTableState],
        fire_clock_ms: Option<u32>,
    ) -> Vec<(ObjectHandle, CombatTarget)> {
        let use_weapon_range = fire_clock_ms.is_some();
        let live: Vec<_> = self
            .units
            .iter()
            .filter_map(|unit| {
                let owner = identity_metadata(world, unit.handle).ok()?.owner_id?;
                let state = transform_state(world, unit.handle).ok()?;
                let (center, radius) = unit_sphere(assets, unit.index, state)?;
                Some((
                    unit,
                    usize::from(owner.0),
                    center,
                    radius,
                    state.position.map(f32::from_bits),
                ))
            })
            .collect();
        live.iter()
            .filter_map(|&(unit, clan, from, _, position)| {
                let handle = unit.handle;
                if patrol.is_some_and(|patrol| {
                    patrol.is_shut_down(handle)
                        || (!use_weapon_range && patrol.is_moving_to_destination(handle))
                        || patrol.is_leaving_factory(handle)
                }) {
                    return None;
                }
                if let Some((target, index)) =
                    patrol.and_then(|patrol| patrol.attack_target(handle))
                {
                    if super::attack_target::complete(world, controls, target) {
                        return None;
                    }
                    // shortcut: aim at the target bounds until the original Attack task is wired.
                    let state = transform_state(world, target).ok()?;
                    let (center, radius) = unit_sphere(assets, index, state)?;
                    let distance = (center[0] - from[0]).hypot(center[1] - from[1]);
                    return Some((
                        handle,
                        CombatTarget {
                            handle: target,
                            center,
                            radius,
                            in_range: distance <= ATTACK_STOP_RANGE,
                        },
                    ));
                }
                // The patrol area bounds pursuit. A Control weapon can fire
                // across that boundary while the hull follows its order.
                let limit = if use_weapon_range {
                    None
                } else {
                    patrol.and_then(|patrol| patrol.attack_limit(world, handle))
                };
                if limit.is_some_and(|limit| !limit.allows_attack(position)) {
                    return None;
                }
                let idle_clock = fire_clock_ms.filter(|_| unit.uses_default_targeting(patrol));
                let search_distance = if use_weapon_range && idle_clock.is_none() {
                    self.maximum_firing_distance(unit)
                } else {
                    ATTACK_RANGE
                };
                let (enemy, center, radius, _) = live
                    .iter()
                    .filter_map(|&(enemy, other, to, radius, target_position)| {
                        let enemy = enemy.handle;
                        if enemy == handle
                            || limit
                                .is_some_and(|limit| !limit.allows_target(enemy, target_position))
                            || self.hostile.get(clan)?.get(other) != Some(&true)
                        {
                            return None;
                        }
                        let rank = if let Some(now) = idle_clock {
                            if unit.idle_target.searches_at(now) {
                                -default_weapon_target::score(position, target_position)?
                            } else {
                                (unit.idle_target.selected == Some(enemy)).then_some(0.0)?
                            }
                        } else {
                            let distance = (to[0] - from[0]).hypot(to[1] - from[1]);
                            (distance <= search_distance).then_some(distance)?
                        };
                        Some((enemy, to, radius, rank))
                    })
                    .min_by(|a, b| a.3.total_cmp(&b.3))?;
                Some((
                    handle,
                    CombatTarget {
                        handle: enemy,
                        center,
                        radius,
                        in_range: (center[0] - from[0]).hypot(center[1] - from[1])
                            <= ATTACK_STOP_RANGE,
                    },
                ))
            })
            .collect()
    }

    fn maximum_firing_distance(&self, unit: &Unit) -> f32 {
        unit.weapons
            .weapons()
            .iter()
            .zip(&unit.slots)
            .filter_map(|(weapon, slot)| {
                Some(firing_distance(
                    weapon.autonomous_fire_distance,
                    self.projectiles[(*slot)?].autonomous_fire_distance,
                ))
            })
            .fold(0.0, f32::max)
    }

    pub(super) fn tick(
        &mut self,
        engine: &mut Engine,
        now: u32,
    ) -> (Vec<AutoDemoCombatEvent>, usize) {
        let mut events = Vec::new();
        let mut unavailable = 0;
        self.finished_segments.clear();
        self.flights.retain_mut(|flight| {
            let mut contact = None;
            let mut travelled = None;
            let Some(mission) = &engine.loaded else {
                return false;
            };
            let stays = flight.pose.advance(now, |segment| {
                let ground = projectile_terrain_hit(&mission.terrain, segment)?;
                contact = projectile_unit_hit(engine, flight.shooter, segment)
                    .filter(|hit| ground.is_none_or(|fraction| hit.fraction < fraction));
                let fraction = contact.map(|hit| hit.fraction).or(ground).unwrap_or(1.0);
                travelled = Some(clip_segment(segment, fraction));
                Ok(ground.is_some() || contact.is_some())
            });
            if let Some(hit) =
                contact.filter(|hit| hit.node != Some(crate::ProjectileNode::CaptureBarrier))
            {
                let data = &self.projectiles[flight.projectile];
                // Capture the target before applying damage, which may remove it.
                let target = CombatObject::read(&engine.world, hit.handle);
                match data
                    .volume
                    .map(|volume| apply_projectile_impact(engine, hit, volume))
                {
                    Some(Ok(ProjectileImpact::ShieldAbsorbed)) => {}
                    Some(Ok(ProjectileImpact::Damage { removed: true, .. })) => {
                        self.report.destroyed(flight.shooter_identity, target, now);
                        events.push(AutoDemoCombatEvent::Destroyed {
                            shooter: flight.shooter,
                            target: hit.handle,
                        });
                    }
                    Some(Ok(ProjectileImpact::Damage {
                        remaining,
                        removed: false,
                    })) => {
                        if let Some(node) = hit.building_node() {
                            events.push(AutoDemoCombatEvent::BuildingHit {
                                shooter: flight.shooter,
                                target: hit.handle,
                                node,
                                remaining_bits: remaining.to_bits(),
                            });
                        }
                    }
                    result => {
                        unavailable += 1;
                        let reason = match &result {
                            None => "projectile_volume",
                            Some(Ok(_)) => "damage_unavailable",
                            Some(Err(_)) => "damage_error",
                        };
                        if self.report.unavailable(flight.shooter_identity, reason, now, 1) {
                            eprintln!("unit_combat_contact object={} target={target} target_id={:?} node={:?} result={result:?}", flight.shooter_identity, target.original_id, hit.node);
                        }
                    }
                }
            }
            match stays {
                Ok(true) => {
                    flight.segment = travelled;
                    true
                }
                Ok(false) => {
                    if let Some(segment) = travelled {
                        self.finished_segments.push((flight.projectile, segment));
                    }
                    false
                }
                Err(_) => {
                    unavailable += 1;
                    self.report
                        .unavailable(flight.shooter_identity, "projectile_advance", now, 1);
                    false
                }
            }
        });
        let Some(mission) = &engine.loaded else {
            return (events, unavailable);
        };
        let targets = self.select_targets(
            &engine.world,
            &mission.mission_assets,
            mission.unit_patrol.as_ref(),
            &mission.control_device_tables,
            Some(now),
        );
        for unit in &mut self.units {
            unit.weapons.read_control_energy(engine, unit.handle);
            let Ok(state) = transform_state(&engine.world, unit.handle) else {
                continue;
            };
            let shooter = CombatObject::read(&engine.world, unit.handle);
            if unit.weapons.weapons().is_empty() {
                continue;
            }
            // shortcut: armed AI units defend their position while their Init is incomplete.
            let owner = identity_metadata(&engine.world, unit.handle)
                .ok()
                .and_then(|identity| identity.owner_id);
            let guard = owner
                .is_some_and(|owner| self.guard_clans.get(usize::from(owner.0)) == Some(&true));
            let manually_controlled = mission
                .boarding
                .is_some_and(|boarding| boarding.cockpit.handle == unit.handle);
            // shortcut: free warbots defend from their current position until the
            // default Behavior task and its pursuit rules are wired.
            let defends_from_position = matches!(
                unit.class,
                super::boarding::WARBOT_CLASS | ANIMAL_UNIT_CLASS
            );
            let eligible = !manually_controlled
                && (mission.script_host_mode != super::MissionMode::SingleMission
                    || guard
                    || defends_from_position
                    || mission
                        .unit_patrol
                        .as_ref()
                        .is_some_and(|patrol| patrol.is_patrolling(unit.handle)));
            let selected = targets
                .iter()
                .find(|(handle, _)| *handle == unit.handle)
                .map(|(_, target)| *target);
            let patrol = mission.unit_patrol.as_ref();
            if eligible
                && unit.uses_default_targeting(patrol)
                && !patrol.is_some_and(|patrol| {
                    patrol.is_shut_down(unit.handle) || patrol.is_leaving_factory(unit.handle)
                })
            {
                unit.idle_target.update(
                    now,
                    selected.map(|target| target.handle),
                    &mut engine.behavior_rng_state,
                );
            } else {
                unit.idle_target = DefaultWeaponTarget::default();
            }
            let aim = eligible
                .then(|| {
                    selected
                        .map(|target| (target.handle, target.center, target.radius))
                        .or_else(|| {
                            let (factory, index) =
                                mission.unit_patrol.as_ref()?.raid_factory(unit.handle)?;
                            let target_owner =
                                identity_metadata(&engine.world, factory).ok()?.owner_id?;
                            if self
                                .hostile
                                .get(usize::from(owner?.0))?
                                .get(usize::from(target_owner.0))
                                != Some(&true)
                            {
                                return None;
                            }
                            let target = transform_state(&engine.world, factory).ok()?;
                            let (to, _) = unit_sphere(&mission.mission_assets, index, target)?;
                            // shortcut: the native attack task's selected building node is unresolved.
                            Some((factory, to, 0.0))
                        })
                })
                .flatten();
            unit.poses.sample(
                &mission.mission_assets,
                unit.index,
                unit.original_id,
                now,
                aim.and_then(|(_, target, _)| hull_local_point(state, target)),
                &mut self.pose_failures,
            );
            if !eligible {
                continue;
            }
            let mut missing = 0;
            let mut due_missing = 0;
            let poses = &unit.poses;
            let clear = |from, to, radius| {
                projectile_terrain_hit(&mission.terrain, [from, to]).is_ok_and(|ground| {
                    if radius == 0.0 {
                        return ground.is_none();
                    }
                    segment_sphere_fraction([from, to], to, radius)
                        .is_some_and(|contact| ground.is_none_or(|ground| contact < ground))
                })
            };
            let hull_position = state.position.map(f32::from_bits);
            for (slot, weapon, barrel) in unit.weapons.fire_where(now, |slot, weapon, barrel| {
                let Some((_, to, radius)) = aim else {
                    return false;
                };
                let Some(projectile) = unit.slots[slot] else {
                    return false;
                };
                let distance = firing_distance(
                    weapon.autonomous_fire_distance,
                    self.projectiles[projectile].autonomous_fire_distance,
                );
                if !within_firing_distance(hull_position, to, distance) {
                    return false;
                }
                let Some(from) = poses.muzzle(state, weapon.component_index, barrel) else {
                    missing += 1;
                    return false;
                };
                clear(from, to, radius)
            }) {
                let Some((target, to, radius)) = aim else {
                    continue;
                };
                let Some(projectile) = unit.slots[slot] else {
                    unavailable += 1;
                    self.report.unavailable(shooter, "projectile_slot", now, 1);
                    continue;
                };
                let Some(from) = poses.muzzle(state, weapon.component_index, barrel) else {
                    due_missing += 1;
                    continue;
                };
                if !clear(from, to, radius) {
                    continue;
                }
                let data = &self.projectiles[projectile];
                let direction = std::array::from_fn(|axis| to[axis] - from[axis]);
                if let Some(pose) =
                    PlayerProjectile::new(from, direction, data.speed, data.lifetime_ms, now)
                {
                    self.flights.push(Flight {
                        shooter: unit.handle,
                        shooter_identity: shooter,
                        projectile,
                        pose,
                        segment: None,
                    });
                    self.report
                        .shot(shooter, CombatObject::read(&engine.world, target), now);
                    events.push(AutoDemoCombatEvent::Shot {
                        shooter: unit.handle,
                        target,
                        projectile,
                        effect: unit.effects[slot],
                        effect_instance: weapon.firing_effect(unit.index),
                        muzzle_bits: from.map(f32::to_bits),
                    });
                } else {
                    unavailable += 1;
                    self.report
                        .unavailable(shooter, "projectile_launch", now, 1);
                }
            }
            events.extend(
                unit.weapons
                    .started_windup_sounds(unit.index)
                    .map(AutoDemoCombatEvent::WindupSound),
            );
            missing += due_missing;
            if missing > 0 {
                let count = self
                    .pose_failures
                    .missing
                    .entry(unit.original_id)
                    .or_default();
                if *count == 0 {
                    eprintln!(
                        "unit_weapon_pose_stub object={} count={missing}",
                        unit.original_id
                    );
                }
                *count += missing;
                self.report
                    .unavailable(shooter, "weapon_pose", now, missing);
                unavailable += missing as usize;
            }
        }
        for unit in &mut self.units {
            unit.weapons.publish_control_energy(engine, unit.handle);
        }
        (events, unavailable)
    }
}

fn firing_distance(weapon: f32, projectile: f32) -> f32 {
    // Control+0x297e0 overrides the weapon's selector 8 with a positive
    // value from the projectile's owner profile.
    if projectile > 0.0 {
        projectile
    } else {
        weapon.max(0.0)
    }
}

fn within_firing_distance(from: [f32; 3], to: [f32; 3], range: f32) -> bool {
    if range <= 0.0 || !range.is_finite() {
        return false;
    }
    let distance_sq: f32 = (0..3).map(|axis| (to[axis] - from[axis]).powi(2)).sum();
    distance_sq <= range * range
}

fn clip_segment([start, end]: [[f32; 3]; 2], fraction: f32) -> [[f32; 3]; 2] {
    if fraction == 1.0 {
        [start, end]
    } else {
        [
            start,
            std::array::from_fn(|i| start[i] + (end[i] - start[i]) * fraction),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autonomous_weapon_checks_projectile_range_in_three_dimensions() {
        assert_eq!(firing_distance(100.0, 350.0), 350.0);
        assert_eq!(firing_distance(100.0, 0.0), 100.0);
        assert_eq!(firing_distance(-1.0, 0.0), 0.0);
        assert!(within_firing_distance([0.0; 3], [300.0, 400.0, 0.0], 500.0));
        assert!(!within_firing_distance(
            [0.0; 3],
            [300.0, 400.0, 15.0],
            500.0
        ));
    }

    #[test]
    fn visible_segment_ends_at_world_contact() {
        let segment = [[10.0, 20.0, 8.0], [18.0, 24.0, 0.0]];
        assert_eq!(
            clip_segment(segment, 0.25),
            [[10.0, 20.0, 8.0], [12.0, 21.0, 6.0]]
        );
        assert_eq!(clip_segment(segment, 1.0), segment);
    }
    use crate::{
        apply_projectile_hit, control_node_provider_row, create, frame, load_gameplay_mission,
        loaded_mission_assets, loaded_mission_control_device_tables, loaded_mission_script_outcome,
        mission_factory_raid_count, mission_guard_fallback_count, mission_object_handle,
        player_status, projectile, starting_cockpit, EngineServices, MissionMode, MissionNotice,
        MissionRequest, MissionScriptOutcome, MissionStartupInput,
    };
    use fparkan_vfs::DirectoryVfs;
    use fparkan_world::{set_transform, OriginalObjectId, OwnerId};

    fn stock_two_fortresses() -> Engine {
        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed corpus");
        let mut engine = create(EngineServices::new(Arc::new(DirectoryVfs::new(root)))).unwrap();
        load_gameplay_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/Single.01/data.tma".into(),
                mode: MissionMode::SingleMission,
            },
            MissionStartupInput {
                queue_time_ms: 0,
                research_creation_gate: false,
            },
            None,
        )
        .unwrap();
        engine
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_constructor_animal_fires_at_a_nearby_hero() {
        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed corpus");
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
        assert!(frame(&mut engine, 0)
            .unwrap()
            .autodemo_combat_events
            .is_empty());

        let hero = starting_cockpit(&engine).unwrap().handle;
        let animal = mission_object_handle(&engine, OriginalObjectId(2)).unwrap();
        let [x, y, z] = transform_state(&engine.world, animal)
            .unwrap()
            .position
            .map(f32::from_bits);
        let mut pose = transform_state(&engine.world, hero).unwrap();
        pose.position = [x + 24.0, y, z + 6.0].map(f32::to_bits);
        set_transform(&mut engine.world, hero, pose).unwrap();
        let shield_charge = |engine: &Engine| {
            let table = engine
                .loaded
                .as_ref()?
                .control_device_tables
                .iter()
                .find(|table| table.handle == hero)?;
            let crate::MissionControlDeviceTableStatus::Available(devices) = &table.status else {
                return None;
            };
            let charge = devices
                .iter()
                .rfind(|device| device.factory_tag == 9)?
                .class9_values_bits?;
            Some(charge.map(f32::from_bits))
        };
        let before_shield = shield_charge(&engine);
        let mut fired = false;
        for now in (50..=1_500).step_by(50) {
            let result = frame(&mut engine, now).unwrap();
            fired |= result.autodemo_combat_events.iter().any(|event| {
                let AutoDemoCombatEvent::Shot {
                    shooter,
                    target,
                    projectile,
                    ..
                } = *event
                else {
                    return false;
                };
                shooter == animal
                    && target == hero
                    && engine.autodemo_combat.as_ref().unwrap().projectiles[projectile].name
                        == "ba_a_03"
            });
            let shield_was_hit =
                shield_charge(&engine)
                    .zip(before_shield)
                    .is_some_and(|(after, before)| {
                        after
                            .into_iter()
                            .zip(before)
                            .any(|(after, before)| after < before)
                    });
            let hull_was_hit = player_status(&engine, hero)
                .and_then(|status| status.health_percent)
                .is_some_and(|percent| percent < 100);
            if fired && (shield_was_hit || hull_was_hit) {
                return;
            }
        }
        panic!("the stock animal must hit the nearby hero (fired={fired})");
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_autodemo_combat_preserves_muzzles_and_event_identities() {
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
        let mut observed_shots = 0;
        let mut observed_aimed_turret = false;
        let mut observed_finished_segment = false;
        let mut observed_weapon_effect = false;
        let mut observed_opening_shot = false;
        crate::collect_unit_destruction_effects(&mut engine);
        let mut observed_unavailable = engine.autodemo_combat.as_ref().unwrap().unavailable as u64;
        for now in (0..=120_000).step_by(50) {
            let frame = frame(&mut engine, now).unwrap();
            let destruction_effects = crate::take_unit_destruction_effects(&mut engine);
            assert!(crate::take_unit_destruction_effects(&mut engine).is_empty());
            observed_unavailable += frame.autodemo_combat_unavailable as u64;
            let shots = frame
                .autodemo_combat_events
                .iter()
                .filter(|event| matches!(event, AutoDemoCombatEvent::Shot { .. }))
                .count();
            observed_shots += shots as u64;
            let combat = engine.autodemo_combat.as_ref().unwrap();
            observed_finished_segment |= !combat.finished_segments.is_empty();
            for flight in &combat.flights {
                if let Some([_, end]) = flight.segment {
                    assert_eq!(end, flight.pose.position);
                }
            }
            assert_eq!(
                combat
                    .report
                    .shots
                    .values()
                    .map(|total| total.count)
                    .sum::<u64>(),
                observed_shots
            );
            assert_eq!(
                combat
                    .report
                    .unavailable
                    .values()
                    .map(|total| total.count)
                    .sum::<u64>(),
                observed_unavailable
            );
            assert!(combat.flights.len() >= shots);
            for (event, flight) in frame
                .autodemo_combat_events
                .iter()
                .filter(|event| matches!(event, AutoDemoCombatEvent::Shot { .. }))
                .rev()
                .zip(combat.flights.iter().rev())
            {
                let AutoDemoCombatEvent::Shot {
                    shooter,
                    target,
                    projectile,
                    effect,
                    effect_instance,
                    muzzle_bits,
                    ..
                } = *event
                else {
                    unreachable!()
                };
                assert_eq!(flight.shooter, shooter);
                assert_eq!(flight.pose.position.map(f32::to_bits), muzzle_bits);
                observed_opening_shot |= now <= 2_000
                    && CombatObject::read(&engine.world, shooter).original_id == Some(3)
                    && CombatObject::read(&engine.world, target).original_id == Some(0)
                    && combat.projectiles[projectile].name == "bl_m_01";
                if let Some(effect) = effect {
                    assert!(effect < combat.weapon_effects().len());
                    let instance = effect_instance.expect("shot names its prepared effect row");
                    let assets = &engine.loaded.as_ref().unwrap().mission_assets;
                    let control = assets.object_component_controls[instance.object_index]
                        [instance.component_index]
                        .as_ref()
                        .unwrap();
                    let fparkan_assets::PreparedControlEffectSection::Parsed(rows) =
                        &control.effect_section
                    else {
                        panic!("effect table is unprepared")
                    };
                    assert!(rows
                        .iter()
                        .any(|row| row.table_index == instance.table_index
                            && row.row_index == instance.row_index));
                    observed_weapon_effect = true;
                }
                let shooter = combat
                    .units
                    .iter()
                    .find(|unit| unit.handle == flight.shooter)
                    .unwrap();
                let state = transform_state(&engine.world, shooter.handle).unwrap();
                observed_aimed_turret |= shooter.poses.has_aim();
                let (center, _) = unit_sphere(
                    &engine.loaded.as_ref().unwrap().mission_assets,
                    shooter.index,
                    state,
                )
                .unwrap();
                let separation: f32 = flight
                    .pose
                    .position
                    .into_iter()
                    .zip(center)
                    .map(|(a, b)| (a - b) * (a - b))
                    .sum();
                assert!(separation > 0.01, "projectile still starts at hull center");
            }
            assert!(combat.pose_report().missing.is_empty());
            if let Some(AutoDemoCombatEvent::Destroyed { target, .. }) = frame
                .autodemo_combat_events
                .iter()
                .find(|event| matches!(event, AutoDemoCombatEvent::Destroyed { .. }))
            {
                assert!(identity_metadata(&engine.world, *target).is_err());
                let ((_, retired), total) = combat
                    .report
                    .destroyed
                    .iter()
                    .find(|((_, object), _)| {
                        object.slot == target.slot && object.generation == target.generation
                    })
                    .expect("removed target is still reported");
                let drafts = &engine.loaded.as_ref().unwrap().object_drafts;
                assert!(destruction_effects.iter().any(|event| {
                    event.archive == "system.rlb"
                        && event.name == "explode_rbr_b.exp"
                        && matches!(event.mesh, Some(crate::DestroyedMesh::Mounted { object_index })
                            if drafts[object_index].original_id.map(|id| id.0) == retired.original_id)
                        && event
                            .position
                            .iter()
                            .all(|coordinate| coordinate.is_finite())
                }));
                assert_eq!(total.first_ms, now);
                assert_eq!(total.count, 1);
                assert!(observed_shots > 0);
                assert!(
                    observed_opening_shot,
                    "the stock laser can reach the opposite flank"
                );
                assert!(observed_aimed_turret, "no stock turret tracked its target");
                assert!(
                    observed_finished_segment,
                    "no segment survived its projectile's removal"
                );
                assert!(observed_weapon_effect, "no shot carried its weapon effect");
                return;
            }
        }
        panic!("stock AutoDemo did not destroy a unit");
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_stock_clan_uses_script_orders_without_a_startup_raid() {
        let mut engine = stock_two_fortresses();
        assert_eq!(mission_guard_fallback_count(&engine), 0);
        assert_eq!(mission_factory_raid_count(&engine), 0);
        let waiting = mission_object_handle(&engine, OriginalObjectId(13)).unwrap();
        assert_eq!(
            identity_metadata(&engine.world, waiting).unwrap().owner_id,
            Some(OwnerId(1))
        );
        let before = transform_state(&engine.world, waiting).unwrap();
        frame(&mut engine, 0).unwrap();
        assert_eq!(transform_state(&engine.world, waiting).unwrap(), before);
        let mut produced = false;
        let mut scripted_patrol_seen = false;
        for now in (100..=90_000).step_by(100) {
            let result = frame(&mut engine, now).unwrap();
            produced |= result.produced_unit.is_some_and(|unit| unit.clan == 1);
            scripted_patrol_seen |=
                engine
                    .autodemo_combat
                    .as_ref()
                    .unwrap()
                    .units
                    .iter()
                    .any(|unit| {
                        if identity_metadata(&engine.world, unit.handle)
                            .ok()
                            .and_then(|identity| identity.owner_id)
                            != Some(OwnerId(1))
                        {
                            return false;
                        }
                        engine
                            .loaded
                            .as_ref()
                            .unwrap()
                            .unit_patrol
                            .as_ref()
                            .is_some_and(|patrol| patrol.is_patrolling(unit.handle))
                    });
        }
        let loaded = engine.loaded.as_ref().unwrap();
        assert!(loaded.clan_ai_events.stops().next().is_none());
        assert!(loaded
            .clan_ai_events
            .group_orders()
            .any(|(clan, _, count)| clan == 1 && count > 0));
        assert!(
            scripted_patrol_seen,
            "AI clan never received a scripted patrol"
        );
        assert!(produced, "scripted AI factory did not produce a robot");
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_stock_capturer_presses_through_defender_fire_and_takes_generator() {
        let mut engine = stock_two_fortresses();
        let defender = mission_object_handle(&engine, OriginalObjectId(17)).unwrap();
        let capturer = mission_object_handle(&engine, OriginalObjectId(11)).unwrap();
        let generator = mission_object_handle(&engine, OriginalObjectId(1)).unwrap();
        let original_owner = identity_metadata(&engine.world, generator)
            .unwrap()
            .owner_id;
        assert_ne!(original_owner, Some(OwnerId(1)));
        let before = transform_state(&engine.world, defender).unwrap();
        assert_eq!(mission_guard_fallback_count(&engine), 0);
        assert!(frame(&mut engine, 0)
            .unwrap()
            .autodemo_combat_events
            .is_empty());

        let mut capturer_fired_while_moving = false;
        let mut defender_fired = false;
        for tick in 1..=60 * 210 {
            let before_capturer = transform_state(&engine.world, capturer).ok();
            let result = frame(&mut engine, tick * 1000 / 60).unwrap();
            for event in result.autodemo_combat_events {
                if let AutoDemoCombatEvent::Shot {
                    shooter, target, ..
                } = event
                {
                    if shooter == capturer {
                        let patrol = engine
                            .loaded
                            .as_ref()
                            .unwrap()
                            .unit_patrol
                            .as_ref()
                            .unwrap();
                        let after = transform_state(&engine.world, capturer).unwrap();
                        capturer_fired_while_moving |= patrol.is_moving_to_destination(capturer)
                            && before_capturer
                                .is_some_and(|before| before.position != after.position);
                    }
                    if shooter == defender {
                        assert_eq!(
                            identity_metadata(&engine.world, target).unwrap().owner_id,
                            Some(OwnerId(1))
                        );
                        assert_eq!(transform_state(&engine.world, defender).unwrap(), before);
                        defender_fired = true;
                    }
                }
            }
            if identity_metadata(&engine.world, generator)
                .unwrap()
                .owner_id
                == Some(OwnerId(1))
            {
                assert!(capturer_fired_while_moving && defender_fired);
                let position = transform_state(&engine.world, capturer).unwrap().position;
                let roof = transform_state(&engine.world, generator).unwrap().position;
                assert!(f32::from_bits(position[2]) < f32::from_bits(roof[2]));
                return;
            }
        }
        panic!("the first capturer did not take the generator (moving={capturer_fired_while_moving}, defender={defender_fired})");
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_stock_helicopter_starts_both_laser_sound_rows() {
        let mut engine = stock_two_fortresses();
        let helicopter = mission_object_handle(&engine, OriginalObjectId(11)).unwrap();
        let mut first_shot = None;
        let mut first_windup_by_component = [None; 2];

        for tick in 0..=60 * 28 {
            let now = tick * 1000 / 60;
            let result = frame(&mut engine, now).unwrap();
            for event in result.autodemo_combat_events {
                match event {
                    AutoDemoCombatEvent::WindupSound(effect)
                        if effect.object_index == 11
                            && matches!(effect.component_index, 10 | 11) =>
                    {
                        assert_eq!((effect.table_index, effect.row_index), (0, 1));
                        first_windup_by_component[effect.component_index - 10].get_or_insert(now);
                    }
                    AutoDemoCombatEvent::Shot { shooter, .. } if shooter == helicopter => {
                        first_shot.get_or_insert(now);
                    }
                    _ => {}
                }
            }
            if first_shot.is_some() && first_windup_by_component.iter().all(Option::is_some) {
                break;
            }
        }

        let first_shot = first_shot.expect("stock helicopter fires");
        let [Some(left), Some(right)] = first_windup_by_component else {
            panic!("both authored gun sounds need a wind-up, got {first_windup_by_component:?}");
        };
        assert!(left.min(right) <= first_shot);
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_scripted_patrol_fires_without_a_guard_fallback() {
        let mut engine = stock_two_fortresses();
        assert_eq!(mission_guard_fallback_count(&engine), 0);
        assert!(frame(&mut engine, 0)
            .unwrap()
            .autodemo_combat_events
            .is_empty());
        let hero = starting_cockpit(&engine).unwrap();
        let status = player_status(&engine, hero.handle).unwrap();
        assert_eq!(status.health_percent, Some(100));
        let table = loaded_mission_control_device_tables(&engine)
            .unwrap()
            .iter()
            .find(|table| table.handle == hero.handle)
            .unwrap();
        let root = control_node_provider_row(table, 0).unwrap();
        let quarter = f32::from_bits(root.initial_amount_bits) * 0.25;
        let changed = apply_projectile_hit(&mut engine, hero.handle, quarter)
            .unwrap()
            .expect("hero damage provider");
        assert!(!changed.1);
        assert_eq!(
            player_status(&engine, hero.handle).unwrap().health_percent,
            Some(75)
        );
        // Debug poses isolate the projectile/terrain regression. Stock-start
        // movement and script assignment are checked by the companion test.
        let enemy = mission_object_handle(&engine, OriginalObjectId(14)).unwrap();
        let mut enemy_pose = transform_state(&engine.world, enemy).unwrap();
        enemy_pose.position = [1504.2733_f32, 915.2701, 67.87986].map(f32::to_bits);
        set_transform(&mut engine.world, enemy, enemy_pose).unwrap();
        let mut pose = transform_state(&engine.world, hero.handle).unwrap();
        pose.position = [1504.273_f32, 917.2702, 64.927925].map(f32::to_bits);
        set_transform(&mut engine.world, hero.handle, pose).unwrap();
        engine
            .loaded
            .as_mut()
            .unwrap()
            .unit_patrol
            .get_or_insert_with(UnitPatrol::scripted)
            .order(&engine.world, enemy, [1504.2733, 915.2701], 60.0);
        let mission = engine.loaded.as_ref().unwrap();
        let (from, _) = projectile::unit_sphere(&mission.mission_assets, 14, enemy_pose).unwrap();
        let (to, _) =
            projectile::unit_sphere(&mission.mission_assets, hero.object_index, pose).unwrap();
        let ray = [from, to];
        let ground = projectile_terrain_hit(&mission.terrain, ray)
            .unwrap()
            .unwrap();
        let contact = projectile_unit_hit(&engine, enemy, ray).unwrap();
        assert_eq!(contact.handle, hero.handle);
        assert!(contact.fraction < ground);
        let mut shots = Vec::new();
        let mut failures = 0;
        for now in (100..=6_000).step_by(50) {
            let step = frame(&mut engine, now).unwrap();
            shots.extend(step.autodemo_combat_events);
            failures += step.mission_event.map_or(0, |event| {
                event
                    .notices
                    .iter()
                    .filter(|&&notice| notice == MissionNotice::Failed)
                    .count()
            });
        }
        let assets = loaded_mission_assets(&engine).unwrap();
        let weapons = prepare_unit_weapons(&assets.object_component_controls[14]);
        assert!(
            shots.iter().any(|event| matches!(event,
            AutoDemoCombatEvent::Shot { shooter, target, .. }
                if *shooter == enemy && *target == hero.handle)),
            "{shots:?}, enemy weapons: {}",
            weapons.weapons().len()
        );
        assert!(!shots.iter().any(|event| matches!(event,
            AutoDemoCombatEvent::Shot { shooter, .. } if *shooter == hero.handle)));
        assert!(transform_state(&engine.world, hero.handle).is_err());
        assert_eq!(player_status(&engine, hero.handle), None);
        assert_eq!(failures, 1);
        assert_eq!(
            loaded_mission_script_outcome(&engine),
            Some(MissionScriptOutcome::Failed)
        );
    }
}
