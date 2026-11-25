use super::{
    apply_projectile_contact,
    autodemo::MOBILE_UNIT,
    player_weapon::prepare_unit_weapons,
    projectile::{load_projectile, segment_sphere_fraction, unit_sphere},
    projectile_terrain_hit, projectile_unit_hit,
    unit_patrol::UnitPatrol,
    weapon_pose::WeaponPoses,
    Engine, MissionAssets, MissionDocument, MissionObjectDraft, ObjectHandle, PlayerProjectile,
    PlayerWeapons,
};
use fparkan_vfs::Vfs;
use fparkan_world::{identity_metadata, transform_state};
use std::sync::Arc;

// Behavior's default Patrol_Attack_Range and Attack_MaxFireDistance.
const ATTACK_RANGE: f32 = 400.0;
const FIRE_RANGE: f32 = 200.0;

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
}

/// One autonomous combat event on the mission clock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AutoDemoCombatEvent {
    /// A weapon released a projectile.
    Shot {
        /// Owner of the weapon.
        shooter: ObjectHandle,
        /// Object the weapon was aimed at.
        target: ObjectHandle,
        /// Projectile type in render-pool order.
        projectile: usize,
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
    flights: Vec<Flight>,
    pose_failures: super::WeaponPoseReport,
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
    original_id: u32,
    weapons: PlayerWeapons,
    slots: Vec<Option<usize>>,
    poses: WeaponPoses,
}

struct Flight {
    shooter: ObjectHandle,
    projectile: usize,
    pose: PlayerProjectile,
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
            flights: Vec::new(),
            pose_failures: super::WeaponPoseReport::default(),
            unavailable: 0,
        };
        for (index, (draft, &handle)) in drafts.iter().zip(handles).enumerate() {
            if !draft
                .unit_dat_header_flags
                .is_some_and(|class| class & MOBILE_UNIT != 0)
            {
                continue;
            }
            let weapons = prepare_unit_weapons(
                assets
                    .object_component_controls
                    .get(index)
                    .map_or(&[], Vec::as_slice),
            );
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
                                return None;
                            };
                            battle.projectiles.push(AutoDemoProjectileType {
                                archive: archive.clone(),
                                name: name.clone(),
                                capacity: 0,
                                speed: data.speed,
                                lifetime_ms: data.lifetime_ms,
                                volume: data.volume,
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
            battle.unavailable += usize::from(weapons.weapons().is_empty());
            let units = if index < reserve_start {
                &mut battle.units
            } else {
                &mut battle.reserve
            };
            units.push(Unit {
                handle,
                index,
                weapons,
                slots,
                poses: WeaponPoses::new(draft, assets, index),
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

    pub(super) fn guard_clan_count(&self) -> usize {
        self.guard_clans.iter().filter(|&&enabled| enabled).count()
    }

    pub(super) fn pose_report(&self) -> &super::WeaponPoseReport {
        &self.pose_failures
    }

    pub(super) fn flights(&self) -> impl Iterator<Item = (usize, PlayerProjectile)> + '_ {
        self.flights
            .iter()
            .map(|flight| (flight.projectile, flight.pose))
    }

    pub(super) fn targets(
        &self,
        world: &fparkan_world::World,
        assets: &MissionAssets,
        patrol: Option<&UnitPatrol>,
    ) -> Vec<(ObjectHandle, CombatTarget)> {
        let live: Vec<_> = self
            .units
            .iter()
            .filter_map(|unit| {
                let owner = identity_metadata(world, unit.handle).ok()?.owner_id?;
                let state = transform_state(world, unit.handle).ok()?;
                let (center, radius) = unit_sphere(assets, unit.index, state)?;
                Some((
                    unit.handle,
                    usize::from(owner.0),
                    center,
                    radius,
                    state.position.map(f32::from_bits),
                ))
            })
            .collect();
        live.iter()
            .filter_map(|&(handle, clan, from, _, position)| {
                if patrol.is_some_and(|patrol| {
                    patrol.is_shut_down(handle) || patrol.is_moving_to_destination(handle)
                }) {
                    return None;
                }
                if let Some((target, index)) =
                    patrol.and_then(|patrol| patrol.attack_target(handle))
                {
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
                            in_range: distance <= FIRE_RANGE,
                        },
                    ));
                }
                let limit = patrol.and_then(|patrol| patrol.attack_limit(world, handle));
                if limit.is_some_and(|limit| !limit.allows_attack(position)) {
                    return None;
                }
                let (enemy, center, radius, distance) = live
                    .iter()
                    .filter_map(|&(enemy, other, to, radius, target_position)| {
                        let distance = (to[0] - from[0]).hypot(to[1] - from[1]);
                        (enemy != handle
                            && distance <= ATTACK_RANGE
                            && limit
                                .is_none_or(|limit| limit.allows_target(enemy, target_position))
                            && self.hostile.get(clan)?.get(other) == Some(&true))
                        .then_some((enemy, to, radius, distance))
                    })
                    .min_by(|a, b| a.3.total_cmp(&b.3))?;
                Some((
                    handle,
                    CombatTarget {
                        handle: enemy,
                        center,
                        radius,
                        in_range: distance <= FIRE_RANGE,
                    },
                ))
            })
            .collect()
    }

    pub(super) fn tick(
        &mut self,
        engine: &mut Engine,
        now: u32,
    ) -> (Vec<AutoDemoCombatEvent>, usize) {
        let mut events = Vec::new();
        let mut unavailable = 0;
        self.flights.retain_mut(|flight| {
            let mut contact = None;
            let Some(mission) = &engine.loaded else {
                return false;
            };
            let stays = flight.pose.advance(now, |segment| {
                let ground = projectile_terrain_hit(&mission.terrain, segment)?;
                contact = projectile_unit_hit(engine, flight.shooter, segment)
                    .filter(|hit| ground.is_none_or(|fraction| hit.fraction < fraction));
                Ok(ground.is_some() || contact.is_some())
            });
            if let Some(hit) = contact {
                let data = &self.projectiles[flight.projectile];
                match data
                    .volume
                    .map(|volume| apply_projectile_contact(engine, hit, volume))
                {
                    Some(Ok(Some((_, true)))) => events.push(AutoDemoCombatEvent::Destroyed {
                        shooter: flight.shooter,
                        target: hit.handle,
                    }),
                    Some(Ok(Some((remaining, false)))) => {
                        if let Some(node) = hit.building_node {
                            events.push(AutoDemoCombatEvent::BuildingHit {
                                shooter: flight.shooter,
                                target: hit.handle,
                                node,
                                remaining_bits: remaining.to_bits(),
                            });
                        }
                    }
                    _ => unavailable += 1,
                }
            }
            match stays {
                Ok(stays) => stays,
                Err(_) => {
                    unavailable += 1;
                    false
                }
            }
        });
        let Some(mission) = &engine.loaded else {
            return (events, unavailable);
        };
        let targets = self.targets(
            &engine.world,
            &mission.mission_assets,
            mission.unit_patrol.as_ref(),
        );
        for unit in &mut self.units {
            let Ok(state) = transform_state(&engine.world, unit.handle) else {
                continue;
            };
            if unit.weapons.weapons().is_empty() {
                continue;
            }
            unit.poses.sample(
                &mission.mission_assets,
                unit.index,
                unit.original_id,
                now,
                &mut self.pose_failures,
            );
            // shortcut: armed AI units defend their position while their Init is incomplete.
            let owner = identity_metadata(&engine.world, unit.handle)
                .ok()
                .and_then(|identity| identity.owner_id);
            let guard = owner
                .is_some_and(|owner| self.guard_clans.get(usize::from(owner.0)) == Some(&true));
            if mission.script_host_mode == super::MissionMode::SingleMission
                && !guard
                && !mission
                    .unit_patrol
                    .as_ref()
                    .is_some_and(|patrol| patrol.is_patrolling(unit.handle))
            {
                continue;
            }
            let aim = targets
                .iter()
                .find(|(handle, target)| *handle == unit.handle && target.in_range)
                .map(|(_, target)| (target.handle, target.center, target.radius))
                .or_else(|| {
                    let (factory, index) =
                        mission.unit_patrol.as_ref()?.raid_factory(unit.handle)?;
                    let target_owner = identity_metadata(&engine.world, factory).ok()?.owner_id?;
                    if self
                        .hostile
                        .get(usize::from(owner?.0))?
                        .get(usize::from(target_owner.0))
                        != Some(&true)
                    {
                        return None;
                    }
                    let state = transform_state(&engine.world, unit.handle).ok()?;
                    let (from, _) = unit_sphere(&mission.mission_assets, unit.index, state)?;
                    let target = transform_state(&engine.world, factory).ok()?;
                    let (to, _) = unit_sphere(&mission.mission_assets, index, target)?;
                    if (to[0] - from[0]).hypot(to[1] - from[1]) > FIRE_RANGE {
                        return None;
                    }
                    // shortcut: the native attack task's selected building node is unresolved.
                    Some((factory, to, 0.0))
                });
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
            for (slot, weapon, barrel) in unit.weapons.fire_where(now, |weapon, barrel| {
                let Some((_, to, radius)) = aim else {
                    return false;
                };
                let Some(from) = poses.muzzle(state, weapon.component_index, barrel) else {
                    missing += 1;
                    return false;
                };
                clear(from, to, radius)
            }) {
                let (Some((target, to, radius)), Some(projectile)) = (aim, unit.slots[slot]) else {
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
                        projectile,
                        pose,
                    });
                    events.push(AutoDemoCombatEvent::Shot {
                        shooter: unit.handle,
                        target,
                        projectile,
                    });
                }
            }
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
                unavailable += missing as usize;
            }
        }
        (events, unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn licensed_autodemo_shots_start_outside_the_hull_center() {
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
        for now in (0..=120_000).step_by(50) {
            let frame = frame(&mut engine, now).unwrap();
            let shots = frame
                .autodemo_combat_events
                .iter()
                .filter(|event| matches!(event, AutoDemoCombatEvent::Shot { .. }))
                .count();
            if shots == 0 {
                continue;
            }
            let combat = engine.autodemo_combat.as_ref().unwrap();
            assert!(combat.flights.len() >= shots);
            for flight in combat.flights.iter().rev().take(shots) {
                let shooter = combat
                    .units
                    .iter()
                    .find(|unit| unit.handle == flight.shooter)
                    .unwrap();
                let state = transform_state(&engine.world, shooter.handle).unwrap();
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
            return;
        }
        panic!("stock AutoDemo did not fire");
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
