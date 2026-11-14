use super::{
    apply_projectile_hit,
    autodemo::MOBILE_UNIT,
    player_weapon::prepare_unit_weapons,
    projectile::{load_projectile, unit_sphere},
    projectile_terrain_hit, projectile_unit_hit, Engine, MissionAssets, MissionDocument,
    MissionObjectDraft, ObjectHandle, PlayerProjectile, PlayerWeapons,
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
}

pub(super) struct AutoDemoCombat {
    units: Vec<Unit>,
    reserve: Vec<Unit>,
    hostile: Vec<Vec<bool>>,
    guard_clans: Vec<bool>,
    projectiles: Vec<AutoDemoProjectileType>,
    flights: Vec<Flight>,
    pub(super) unavailable: usize,
}

#[derive(Clone, Copy)]
pub(super) struct CombatTarget {
    pub(super) handle: ObjectHandle,
    pub(super) center: [f32; 3],
    pub(super) in_range: bool,
}

#[derive(Clone)]
struct Unit {
    handle: ObjectHandle,
    index: usize,
    weapons: PlayerWeapons,
    slots: Vec<Option<usize>>,
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

    pub(super) fn flights(&self) -> impl Iterator<Item = (usize, PlayerProjectile)> + '_ {
        self.flights
            .iter()
            .map(|flight| (flight.projectile, flight.pose))
    }

    pub(super) fn targets(
        &self,
        world: &fparkan_world::World,
        assets: &MissionAssets,
    ) -> Vec<(ObjectHandle, CombatTarget)> {
        let live: Vec<_> = self
            .units
            .iter()
            .filter_map(|unit| {
                let owner = identity_metadata(world, unit.handle).ok()?.owner_id?;
                let state = transform_state(world, unit.handle).ok()?;
                let (center, _) = unit_sphere(assets, unit.index, state)?;
                Some((unit.handle, usize::from(owner.0), center))
            })
            .collect();
        live.iter()
            .filter_map(|&(handle, clan, from)| {
                let (enemy, center, distance) = live
                    .iter()
                    .filter_map(|&(enemy, other, to)| {
                        let distance = (to[0] - from[0]).hypot(to[1] - from[1]);
                        (enemy != handle
                            && distance <= ATTACK_RANGE
                            && self.hostile.get(clan)?.get(other) == Some(&true))
                        .then_some((enemy, to, distance))
                    })
                    .min_by(|a, b| a.2.total_cmp(&b.2))?;
                Some((
                    handle,
                    CombatTarget {
                        handle: enemy,
                        center,
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
                    .map(|volume| apply_projectile_hit(engine, hit.handle, volume))
                {
                    Some(Ok(Some((_, true)))) => events.push(AutoDemoCombatEvent::Destroyed {
                        shooter: flight.shooter,
                        target: hit.handle,
                    }),
                    Some(Ok(Some((_, false)))) => {}
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
        let targets = self.targets(&engine.world, &mission.mission_assets);
        for unit in &mut self.units {
            // shortcut: armed AI units defend their position while their Init is incomplete.
            let guard = identity_metadata(&engine.world, unit.handle)
                .ok()
                .and_then(|identity| identity.owner_id)
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
                .and_then(|(_, target)| {
                    let state = transform_state(&engine.world, unit.handle).ok()?;
                    let (from, _) = unit_sphere(&mission.mission_assets, unit.index, state)?;
                    match projectile_terrain_hit(&mission.terrain, [from, target.center]) {
                        Ok(None) => Some((target.handle, from, target.center)),
                        Ok(Some(_)) => None,
                        Err(_) => {
                            unavailable += 1;
                            None
                        }
                    }
                });
            for (slot, _, _) in unit.weapons.fire(aim.is_some(), now) {
                let (Some((target, from, to)), Some(projectile)) = (aim, unit.slots[slot]) else {
                    continue;
                };
                let data = &self.projectiles[projectile];
                // shortcut: aim from the root hull center until animated AI barrel poses are sampled.
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
        }
        (events, unavailable)
    }
}
