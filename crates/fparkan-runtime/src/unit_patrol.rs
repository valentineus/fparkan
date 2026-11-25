use super::{
    autodemo::{MOBILE_UNIT, PLANT_CLASS},
    boarding::WARBOT_CLASS,
    next_crt_rand,
    patrol_points::{PatrolPointReport, PatrolPoints},
    patrol_speed::{place_speed, PatrolSpeedReport},
    unit_combat::CombatTarget,
    EngineError, LoadedMissionState, MissionDocument, MissionObjectDraft, MissionScriptBundle,
    ObjectHandle, TerrainWorld, TransformState, World,
};
use fparkan_script::{Handler15TargetPayload, ScriptDispatchSelector, VarSet};
use fparkan_terrain::{NavigationQuery, RouteRequest, SurfaceQuery};
use fparkan_world::{identity_metadata, set_transform, transform_state};
use std::collections::{BTreeMap, HashSet, VecDeque};

mod orders;
pub(super) use orders::supports_order;

#[derive(Clone)]
pub(super) struct UnitPatrol {
    center: Option<[f32; 2]>,
    units: Vec<PatrollingUnit>,
    shut_down: HashSet<ObjectHandle>,
    script_orders: Vec<(ObjectHandle, VecDeque<orders::UnitOrder>)>,
    script_order_counts: BTreeMap<(u32, u32), u64>,
    clock_ms: u32,
    rng: u32,
    pub(super) point_report: PatrolPointReport,
    pub(super) speed_report: PatrolSpeedReport,
    pub(super) factory_raid_clans: usize,
}

#[derive(Clone)]
struct PatrollingUnit {
    handle: ObjectHandle,
    center: [f32; 2],
    center_object: Option<ObjectHandle>,
    destinations: Option<VecDeque<[f32; 2]>>,
    radius: f32,
    points: VecDeque<[f32; 2]>,
    place_points: PatrolPoints,
    retry_ms: u32,
    attack: Option<ObjectHandle>,
    attack_target: Option<(ObjectHandle, usize)>,
    raid_factory: Option<(ObjectHandle, usize)>,
}

pub(super) struct PreparedPatrol {
    pub(super) state: UnitPatrol,
    poses: Vec<(ObjectHandle, TransformState, TransformState)>,
}

#[derive(Clone, Copy)]
pub(super) struct PatrolAttackLimit {
    center: [f32; 2],
    radius: f32,
    current_target: Option<ObjectHandle>,
}

impl PatrolAttackLimit {
    pub(super) fn allows_attack(self, position: [f32; 3]) -> bool {
        distance_to_patrol(self.center, position) <= self.radius + 60.0
    }

    pub(super) fn allows_target(self, target: ObjectHandle, position: [f32; 3]) -> bool {
        self.current_target == Some(target)
            || distance_to_patrol(self.center, position) < self.radius
    }
}

impl UnitPatrol {
    pub(super) fn approximate_factory_raid(
        &mut self,
        world: &World,
        mission: &MissionDocument,
        drafts: &[MissionObjectDraft],
        handles: &[ObjectHandle],
        bundle: &MissionScriptBundle,
        declarations: &VarSet,
    ) {
        let attacks_factory = bundle
            .package
            .events
            .iter()
            .filter(|event| event.name_raw == b"PBM_BUILDING_ATTACK_Start\0")
            .flat_map(|event| &event.instructions)
            .filter(|instruction| {
                instruction.dispatch_selector() == ScriptDispatchSelector::Handler(28)
            })
            .any(|instruction| {
                declarations
                    .resolve_handler15(instruction)
                    .is_ok_and(|order| {
                        order.word_1 == 3
                            && order.word_2 == 3
                            && order.word_3 == PLANT_CLASS
                            && matches!(order.target, Handler15TargetPayload::ByLogicId(_))
                    })
            });
        if !attacks_factory {
            return;
        }

        let clan = &mission.clans[bundle.clan_index];
        let factories = drafts.iter().enumerate().filter(|(_, draft)| {
            draft.unit_dat_header_flags == Some(PLANT_CLASS)
                && mission
                    .clans
                    .get(draft.raw_after_resource as usize)
                    .is_some_and(|owner| {
                        clan.relations.iter().any(|relation| {
                            relation.relation_value == 0
                                && relation.other_clan_name.raw == owner.name.raw
                        })
                    })
        });
        // shortcut: route placed battle robots toward the nearest hostile factory until
        // the problem scheduler can select and dispatch a native attack group.
        let mut ordered = false;
        for (draft, &handle) in drafts.iter().zip(handles).filter(|(draft, _)| {
            draft.raw_after_resource as usize == bundle.clan_index
                && draft.unit_dat_header_flags == Some(WARBOT_CLASS)
        }) {
            let distance = |target: &MissionObjectDraft| {
                (draft.position[0] - target.position[0])
                    .hypot(draft.position[1] - target.position[1])
            };
            if let Some((index, factory)) = factories
                .clone()
                .min_by(|(_, a), (_, b)| distance(a).total_cmp(&distance(b)))
            {
                ordered |= self.order(
                    world,
                    handle,
                    [factory.position[0], factory.position[1]],
                    60.0,
                );
                if let Some(unit) = self.units.iter_mut().find(|unit| unit.handle == handle) {
                    unit.raid_factory = Some((handles[index], index));
                }
            }
        }
        self.factory_raid_clans += usize::from(ordered);
    }

    pub(super) fn scripted() -> Self {
        Self {
            center: None,
            units: Vec::new(),
            shut_down: HashSet::new(),
            script_orders: Vec::new(),
            script_order_counts: BTreeMap::new(),
            clock_ms: 0,
            rng: 1,
            point_report: PatrolPointReport::default(),
            speed_report: PatrolSpeedReport::default(),
            factory_raid_clans: 0,
        }
    }

    pub(super) fn order(
        &mut self,
        world: &World,
        handle: ObjectHandle,
        center: [f32; 2],
        radius: f32,
    ) -> bool {
        self.script_orders.retain(|(unit, _)| *unit != handle);
        self.shut_down.remove(&handle);
        self.units
            .retain(|unit| transform_state(world, unit.handle).is_ok());
        if let Some(unit) = self.units.iter_mut().find(|unit| unit.handle == handle) {
            unit.raid_factory = None;
            let was_attacking = unit.attack_target.take().is_some();
            let was_moving = unit.destinations.take().is_some();
            if unit.center_object.take().is_some()
                || was_moving
                || was_attacking
                || unit.center != center
                || unit.radius != radius
            {
                unit.center = center;
                unit.radius = radius;
                unit.points.clear();
                unit.place_points = PatrolPoints::default();
                unit.retry_ms = 0;
                return true;
            }
            return false;
        }
        self.units.push(PatrollingUnit {
            handle,
            center,
            center_object: None,
            destinations: None,
            radius,
            points: VecDeque::new(),
            place_points: PatrolPoints::default(),
            retry_ms: 0,
            attack: None,
            attack_target: None,
            raid_factory: None,
        });
        true
    }

    pub(super) fn move_to(
        &mut self,
        world: &World,
        handle: ObjectHandle,
        destination: [f32; 2],
        append: bool,
    ) {
        if append {
            if let Some(destinations) = self
                .units
                .iter_mut()
                .find(|unit| unit.handle == handle)
                .and_then(|unit| unit.destinations.as_mut())
            {
                destinations.push_back(destination);
                return;
            }
        }
        self.order(world, handle, destination, 60.0);
        if let Some(unit) = self.units.iter_mut().find(|unit| unit.handle == handle) {
            unit.destinations = Some(VecDeque::from([destination]));
            unit.points.clear();
            unit.retry_ms = 0;
        }
    }

    pub(super) fn order_around_object(
        &mut self,
        world: &World,
        handle: ObjectHandle,
        target: ObjectHandle,
        center: [f32; 2],
        radius: f32,
    ) -> bool {
        if self.units.iter().any(|unit| {
            unit.handle == handle
                && unit.attack_target.is_none()
                && unit.center_object == Some(target)
                && unit.radius == radius
        }) {
            return false;
        }
        self.order(world, handle, center, radius);
        if let Some(unit) = self.units.iter_mut().find(|unit| unit.handle == handle) {
            unit.center_object = Some(target);
            unit.points.clear();
            unit.retry_ms = 0;
        }
        true
    }

    pub(super) fn is_patrolling(&self, handle: ObjectHandle) -> bool {
        self.units.iter().any(|unit| unit.handle == handle)
    }

    pub(super) fn has_object_order(&self, handle: ObjectHandle, target: ObjectHandle) -> bool {
        self.units
            .iter()
            .any(|unit| unit.handle == handle && unit.has_object_order(target))
    }

    pub(super) fn clear_object_order(&mut self, handle: ObjectHandle, target: ObjectHandle) {
        self.units
            .retain(|unit| unit.handle != handle || !unit.has_object_order(target));
    }

    pub(super) fn has_point_order(&self, handle: ObjectHandle, point: [f32; 2]) -> bool {
        self.units
            .iter()
            .any(|unit| unit.handle == handle && unit.patrols_point(point))
    }

    pub(super) fn clear_point_order(&mut self, handle: ObjectHandle, point: [f32; 2]) {
        self.units
            .retain(|unit| unit.handle != handle || !unit.patrols_point(point));
    }

    pub(super) fn shut_down(&mut self, handle: ObjectHandle) -> bool {
        self.script_orders.retain(|(unit, _)| *unit != handle);
        // shortcut: stop autonomous movement and fire until the native shutdown task is modeled.
        self.units.retain(|unit| unit.handle != handle);
        self.shut_down.insert(handle)
    }

    pub(super) fn is_shut_down(&self, handle: ObjectHandle) -> bool {
        self.shut_down.contains(&handle)
    }

    pub(super) fn is_moving_to_destination(&self, handle: ObjectHandle) -> bool {
        self.units
            .iter()
            .any(|unit| unit.handle == handle && unit.destinations.is_some())
    }

    pub(super) fn clear_move(&mut self, handle: ObjectHandle) {
        self.units
            .retain(|unit| unit.handle != handle || unit.destinations.is_none());
    }

    pub(super) fn raid_factory(&self, handle: ObjectHandle) -> Option<(ObjectHandle, usize)> {
        self.units
            .iter()
            .find(|unit| unit.handle == handle)?
            .raid_factory
    }

    pub(super) fn set_attack_target(
        &mut self,
        world: &World,
        handle: ObjectHandle,
        target: ObjectHandle,
        target_index: usize,
        center: [f32; 2],
    ) {
        self.order(world, handle, center, 60.0);
        if let Some(unit) = self.units.iter_mut().find(|unit| unit.handle == handle) {
            unit.center_object = Some(target);
            unit.attack_target = Some((target, target_index));
        }
    }

    pub(super) fn attack_target(&self, handle: ObjectHandle) -> Option<(ObjectHandle, usize)> {
        self.units
            .iter()
            .find(|unit| unit.handle == handle)?
            .attack_target
    }

    pub(super) fn attack_limit(
        &self,
        world: &World,
        handle: ObjectHandle,
    ) -> Option<PatrolAttackLimit> {
        // A raid starts with an explicit attack order. It never receives PatrolPlace limits.
        let unit = self
            .units
            .iter()
            .find(|unit| unit.handle == handle && unit.raid_factory.is_none())?;
        Some(PatrolAttackLimit {
            center: unit.center(world)?,
            radius: unit.radius,
            current_target: unit.attack,
        })
    }

    pub(super) fn activate(&mut self, world: &World, handle: ObjectHandle) {
        if let Some(center) = self.center {
            self.order(world, handle, center, 150.0);
        }
    }

    pub(super) fn new(
        mission: &MissionDocument,
        drafts: &[MissionObjectDraft],
        handles: &[ObjectHandle],
    ) -> Option<Self> {
        // shortcut: use the observed AutoDemo meeting area from the first clan anchor
        // until we can execute the native producer of the patrol order.
        let center = mission.clans.first()?.anchor.map(f32::round);
        let units = drafts
            .iter()
            .zip(handles)
            .filter(|(draft, _)| {
                draft
                    .unit_dat_header_flags
                    .is_some_and(|class| class & MOBILE_UNIT != 0)
                    && (draft.raw_after_resource as usize) < mission.clans.len()
            })
            .map(|(_, &handle)| PatrollingUnit {
                handle,
                center,
                center_object: None,
                destinations: None,
                radius: 150.0,
                points: VecDeque::new(),
                place_points: PatrolPoints::default(),
                retry_ms: 0,
                attack: None,
                attack_target: None,
                raid_factory: None,
            })
            .collect();
        Some(Self {
            center: Some(center),
            units,
            shut_down: HashSet::new(),
            script_orders: Vec::new(),
            script_order_counts: BTreeMap::new(),
            clock_ms: 0,
            rng: 1,
            point_report: PatrolPointReport::default(),
            speed_report: PatrolSpeedReport::default(),
            factory_raid_clans: 0,
        })
    }

    pub(super) fn prepare(
        &self,
        world: &World,
        mission: &LoadedMissionState,
        now_ms: u32,
        targets: &[(ObjectHandle, CombatTarget)],
        crt: &mut u32,
        count_rng: &mut u32,
    ) -> Result<PreparedPatrol, EngineError> {
        let terrain = &mission.terrain;
        let chassis = &mission.unit_can_fly;
        let mut state = self.clone();
        state.advance_script_orders(world, &mission.control_device_tables);
        state.units.retain(|unit| {
            unit.destinations
                .as_ref()
                .is_none_or(|items| !items.is_empty())
                && unit.center(world).is_some()
        });
        let seconds = now_ms.saturating_sub(state.clock_ms) as f32 * 0.001;
        state.clock_ms = state.clock_ms.max(now_ms);
        let mut poses = Vec::new();
        for unit in &mut state.units {
            let Ok(before) = transform_state(world, unit.handle) else {
                continue;
            };
            let from = before.position.map(f32::from_bits);
            if let Some(destinations) = &mut unit.destinations {
                if destinations
                    .front()
                    .is_some_and(|[x, y]| (x - from[0]).hypot(y - from[1]) <= 0.05)
                {
                    destinations.pop_front();
                    unit.points.clear();
                    unit.retry_ms = 0;
                }
                if destinations.is_empty() {
                    continue;
                }
            }
            let center = unit.center(world).expect("retained patrol center");
            if (center[0] - unit.center[0]).hypot(center[1] - unit.center[1]) >= unit.radius {
                unit.center = center;
                unit.points.clear();
                unit.retry_ms = 0;
            }
            let enemy = targets
                .iter()
                .find(|(handle, _)| *handle == unit.handle)
                .map(|(_, target)| *target);
            if unit.attack != enemy.map(|target| target.handle) {
                unit.points.clear();
                unit.retry_ms = 0;
                unit.attack = enemy.map(|target| target.handle);
            }
            let place_patrol = unit.center_object.is_none()
                && unit.destinations.is_none()
                && unit.attack_target.is_none()
                && unit.raid_factory.is_none();
            if place_patrol && (enemy.is_none() || unit.place_points.current().is_none()) {
                let original_id = identity_metadata(world, unit.handle)
                    .ok()
                    .and_then(|identity| identity.original_id)
                    .map_or(unit.handle.slot, |id| id.0);
                let can_fly = chassis.get(&original_id).copied().flatten();
                if let Some(bounds) = terrain.surface_bounds() {
                    let mut checks = 0;
                    let refreshed = unit.place_points.refresh(
                        center,
                        unit.radius,
                        bounds,
                        now_ms,
                        crt,
                        count_rng,
                        |[x, y]| {
                            if can_fly == Some(true) {
                                return Ok(true);
                            }
                            checks += 1;
                            // shortcut: area presence substitutes for native area walkability
                            // until the native ArealMap flags are projected.
                            Ok(terrain
                                .locate_areal([x, y, 0.0])
                                .map_err(EngineError::Terrain)?
                                .is_some())
                        },
                    )?;
                    if refreshed {
                        let totals = state.point_report.batches.entry(original_id).or_default();
                        if totals.0 == 0 {
                            eprintln!("mission_patrol_points_approx object={original_id} (seed, global owner order and ground walkability remain open)");
                        }
                        totals.0 += 1;
                        totals.1 += unit.place_points.len() as u64;
                        totals.2 += checks;
                        if can_fly.is_none() {
                            let count = state
                                .point_report
                                .stubs
                                .entry((original_id, "chassis_can_fly"))
                                .or_default();
                            if *count == 0 {
                                eprintln!("mission_patrol_points_stub object={original_id} reason=chassis_can_fly");
                            }
                            *count += 1;
                        }
                        unit.points.clear();
                        unit.retry_ms = 0;
                    }
                    if unit
                        .place_points
                        .current()
                        .is_some_and(|[x, y]| (x - from[0]).hypot(y - from[1]) <= 0.05)
                    {
                        unit.place_points.advance();
                        unit.points.clear();
                        unit.retry_ms = 0;
                    }
                } else {
                    let count = state
                        .point_report
                        .stubs
                        .entry((original_id, "terrain_bounds"))
                        .or_default();
                    if *count == 0 {
                        eprintln!(
                            "mission_patrol_points_stub object={original_id} reason=terrain_bounds"
                        );
                    }
                    *count += 1;
                }
            }
            if seconds == 0.0 {
                continue;
            }
            if let Some(enemy) = enemy.filter(|enemy| enemy.in_range) {
                let mut after = before;
                let (yaw, _) = turn_toward(
                    before,
                    [enemy.center[0] - from[0], enemy.center[1] - from[1]],
                    seconds,
                );
                after.orientation[2] = yaw.to_bits();
                if after != before {
                    poses.push((unit.handle, before, after));
                }
                continue;
            }
            if (unit.points.is_empty() || enemy.is_some()) && now_ms >= unit.retry_ms {
                unit.points = if let Some(enemy) = enemy {
                    route_points(terrain, from, enemy.center)?.unwrap_or_default()
                } else if let Some(&[x, y]) =
                    unit.destinations.as_ref().and_then(|items| items.front())
                {
                    // shortcut: route to the same XY using the terrain height until native Go arrival is wired.
                    terrain
                        .height_at([x, y])
                        .map_err(EngineError::Terrain)?
                        .map(|z| route_points(terrain, from, [x, y, z]))
                        .transpose()?
                        .flatten()
                        .unwrap_or_default()
                } else if unit.attack_target.is_some() {
                    VecDeque::new()
                } else if let Some([x, y]) = unit.place_points.current().filter(|_| place_patrol) {
                    let route = terrain
                        .height_at([x, y])
                        .map_err(EngineError::Terrain)?
                        .map(|z| route_points(terrain, from, [x, y, z]))
                        .transpose()?
                        .flatten();
                    if route.is_none() {
                        unit.place_points.advance();
                    }
                    route.unwrap_or_default()
                } else {
                    choose_route(terrain, from, center, unit.radius, &mut state.rng)?
                };
                unit.retry_ms = now_ms.saturating_add(1_000);
            }
            let Some(&target) = unit.points.front() else {
                continue;
            };
            let dx = target[0] - from[0];
            let dy = target[1] - from[1];
            let distance = dx.hypot(dy);
            if distance <= 0.05 {
                unit.points.pop_front();
                continue;
            }
            let mut after = before;
            // shortcut: turn and non-place orders still use the temporary controller.
            let (yaw, turn_seconds) = turn_toward(before, [dx, dy], seconds);
            after.orientation[2] = yaw.to_bits();
            let speed = if place_patrol && enemy.is_none() {
                let object = identity_metadata(world, unit.handle)
                    .ok()
                    .and_then(|identity| identity.original_id)
                    .map_or(unit.handle.slot, |id| id.0);
                let table = mission
                    .control_device_tables
                    .iter()
                    .find(|table| table.handle == unit.handle);
                place_speed(table, object, &mut state.speed_report)
            } else {
                9.0
            };
            let travel = speed * (seconds - turn_seconds);
            if travel > 0.0 {
                let fraction = (travel / distance).min(1.0);
                let xy = [from[0] + dx * fraction, from[1] + dy * fraction];
                if let Some(ground) = terrain.height_at(xy).map_err(EngineError::Terrain)? {
                    let old_ground = terrain
                        .height_at([from[0], from[1]])
                        .map_err(EngineError::Terrain)?;
                    let z = ground + old_ground.map_or(0.0, |height| from[2] - height);
                    after.position = [xy[0], xy[1], z].map(f32::to_bits);
                    if fraction == 1.0 {
                        unit.points.pop_front();
                    }
                } else {
                    unit.points.clear();
                }
            }
            if after != before {
                poses.push((unit.handle, before, after));
            }
        }
        Ok(PreparedPatrol { state, poses })
    }
}

#[cfg(test)]
#[test]
fn new_patrol_replaces_a_script_attack_order() {
    let world = fparkan_world::new(fparkan_world::WorldConfig::default());
    let actor = ObjectHandle {
        slot: 0,
        generation: 1,
    };
    let target = ObjectHandle {
        slot: 1,
        generation: 1,
    };
    let mut patrol = UnitPatrol::scripted();
    patrol.set_attack_target(&world, actor, target, 1, [50.0, 60.0]);
    assert_eq!(patrol.attack_target(actor), Some((target, 1)));
    assert!(patrol.order_around_object(&world, actor, target, [50.0, 60.0], 60.0));
    assert_eq!(patrol.attack_target(actor), None);
}

impl PatrollingUnit {
    fn has_object_order(&self, target: ObjectHandle) -> bool {
        self.center_object == Some(target)
            || self
                .attack_target
                .is_some_and(|(handle, _)| handle == target)
    }

    fn patrols_point(&self, point: [f32; 2]) -> bool {
        self.center_object.is_none()
            && self.attack_target.is_none()
            && self.destinations.is_none()
            && self.center == point
    }

    fn center(&self, world: &World) -> Option<[f32; 2]> {
        // shortcut: use the target's transform as the patrol center until native building approaches are wired.
        match self.center_object {
            None => Some(self.center),
            Some(handle) => {
                let [x, y, _] = transform_state(world, handle)
                    .ok()?
                    .position
                    .map(f32::from_bits);
                Some([x, y])
            }
        }
    }
}

#[cfg(test)]
#[test]
fn attack_order_remains_assigned_and_cleanup_preserves_a_new_target() {
    let mut world = fparkan_world::new(fparkan_world::WorldConfig::default());
    let [actor, first, second] = [0; 3].map(|_| {
        let unit = fparkan_world::construct_object(
            &mut world,
            fparkan_world::ObjectDraft { original_id: None },
        )
        .unwrap();
        fparkan_world::register_object(&mut world, unit).unwrap();
        unit
    });
    let mut patrol = UnitPatrol::scripted();
    patrol.set_attack_target(&world, actor, first, 1, [50.0, 60.0]);
    assert!(patrol.has_object_order(actor, first));
    patrol.clear_object_order(actor, second);
    assert_eq!(patrol.attack_target(actor), Some((first, 1)));
    patrol.set_attack_target(&world, actor, second, 2, [70.0, 80.0]);
    patrol.clear_object_order(actor, first);
    assert_eq!(patrol.attack_target(actor), Some((second, 2)));
    patrol.clear_object_order(actor, second);
    assert!(!patrol.is_patrolling(actor));
}

#[cfg(test)]
#[test]
fn point_patrol_cleanup_preserves_reassigned_orders() {
    let mut world = fparkan_world::new(fparkan_world::WorldConfig::default());
    let actor = fparkan_world::construct_object(
        &mut world,
        fparkan_world::ObjectDraft { original_id: None },
    )
    .unwrap();
    fparkan_world::register_object(&mut world, actor).unwrap();
    let mut patrol = UnitPatrol::scripted();
    let point = [1124.0, 783.0];
    patrol.order(&world, actor, point, 60.0);
    assert!(patrol.has_point_order(actor, point));
    patrol.clear_point_order(actor, [1125.0, 783.0]);
    assert!(patrol.has_point_order(actor, point));
    patrol.move_to(&world, actor, point, false);
    assert!(!patrol.has_point_order(actor, point));
    patrol.clear_point_order(actor, point);
    assert!(patrol.is_patrolling(actor));
    patrol.order(&world, actor, point, 90.0);
    patrol.clear_point_order(actor, point);
    assert!(!patrol.is_patrolling(actor));
}

#[cfg(test)]
#[test]
fn shutdown_clears_patrol_until_a_new_order_arrives() {
    let mut world = fparkan_world::new(fparkan_world::WorldConfig::default());
    let handle = fparkan_world::construct_object(
        &mut world,
        fparkan_world::ObjectDraft { original_id: None },
    )
    .unwrap();
    fparkan_world::register_object(&mut world, handle).unwrap();
    let mut patrol = UnitPatrol::scripted();
    assert!(patrol.order(&world, handle, [20.0, 30.0], 60.0));
    assert!(patrol.shut_down(handle));
    assert!(patrol.is_shut_down(handle));
    assert!(!patrol.is_patrolling(handle));
    assert!(patrol.order(&world, handle, [35.0, 45.0], 60.0));
    assert!(!patrol.is_shut_down(handle));
    assert!(patrol.is_patrolling(handle));
}

#[cfg(test)]
#[test]
fn object_patrol_tracks_its_center_and_ignores_repeated_orders() {
    let mut world = fparkan_world::new(fparkan_world::WorldConfig::default());
    let mut make_actor = || {
        let actor = fparkan_world::construct_object(
            &mut world,
            fparkan_world::ObjectDraft { original_id: None },
        )
        .unwrap();
        fparkan_world::register_object(&mut world, actor).unwrap();
        actor
    };
    let actor = make_actor();
    let target = make_actor();
    let mut patrol = UnitPatrol::scripted();
    assert!(patrol.order_around_object(&world, actor, target, [0.0; 2], 60.0));
    assert!(!patrol.order_around_object(&world, actor, target, [0.0; 2], 60.0));
    let mut position = transform_state(&world, target).unwrap();
    position.position = [20.0_f32, 35.0, 0.0].map(f32::to_bits);
    set_transform(&mut world, target, position).unwrap();
    assert_eq!(
        patrol.attack_limit(&world, actor).unwrap().center,
        [20.0, 35.0]
    );
    patrol.order(&world, actor, [0.0; 2], 60.0);
    assert_eq!(patrol.attack_limit(&world, actor).unwrap().center, [0.0; 2]);
}

#[cfg(test)]
#[test]
fn move_orders_append_and_replace_destinations() {
    let mut world = fparkan_world::new(fparkan_world::WorldConfig::default());
    let actor = fparkan_world::construct_object(
        &mut world,
        fparkan_world::ObjectDraft { original_id: None },
    )
    .unwrap();
    fparkan_world::register_object(&mut world, actor).unwrap();
    let mut patrol = UnitPatrol::scripted();
    patrol.move_to(&world, actor, [10.0, 20.0], false);
    patrol.move_to(&world, actor, [30.0, 40.0], true);
    assert_eq!(
        patrol.units[0].destinations,
        Some(VecDeque::from([[10.0, 20.0], [30.0, 40.0]]))
    );
    patrol.move_to(&world, actor, [45.0, 50.0], false);
    assert_eq!(
        patrol.units[0].destinations,
        Some(VecDeque::from([[45.0, 50.0]]))
    );
}

fn distance_to_patrol([x, y]: [f32; 2], [px, py, _]: [f32; 3]) -> f32 {
    // shortcut: use the horizontal distance until patrol orders carry their native Z.
    (px - x).hypot(py - y)
}

#[cfg(test)]
#[test]
fn patrol_limits_new_targets_and_ends_pursuit_at_the_outer_boundary() {
    let tracked = ObjectHandle {
        slot: 1,
        generation: 1,
    };
    let candidate = ObjectHandle {
        slot: 2,
        generation: 1,
    };
    let patrol = PatrolAttackLimit {
        center: [500.0, 752.0],
        radius: 150.0,
        current_target: Some(tracked),
    };
    assert!(patrol.allows_target(candidate, [649.0, 752.0, 0.0]));
    assert!(!patrol.allows_target(candidate, [650.0, 752.0, 0.0]));
    assert!(patrol.allows_target(tracked, [650.0, 752.0, 0.0]));
    assert!(patrol.allows_attack([710.0, 752.0, 0.0]));
    assert!(!patrol.allows_attack([711.0, 752.0, 0.0]));
    assert!(!patrol.allows_attack([650.0, 902.0, 0.0]));
}

fn turn_toward(pose: TransformState, [dx, dy]: [f32; 2], seconds: f32) -> (f32, f32) {
    let yaw = f32::from_bits(pose.orientation[2]);
    let goal_yaw = (-dx).atan2(dy);
    let turn = (goal_yaw - yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
        - std::f32::consts::PI;
    let turn_budget = 2.0 * seconds;
    (
        yaw + turn.clamp(-turn_budget, turn_budget),
        seconds.min(turn.abs() * 0.5),
    )
}

fn choose_route(
    terrain: &TerrainWorld,
    from: [f32; 3],
    center: [f32; 2],
    radius: f32,
    rng: &mut u32,
) -> Result<VecDeque<[f32; 2]>, EngineError> {
    // shortcut: choose one routed patrol point at a time with separate RNG state.
    for _ in 0..16 {
        let [x, y] =
            center.map(|axis| axis + (next_crt_rand(rng) as f32 / 32767.0 * 2.0 - 1.0) * radius);
        let Some(z) = terrain.height_at([x, y]).map_err(EngineError::Terrain)? else {
            continue;
        };
        if let Some(points) = route_points(terrain, from, [x, y, z])? {
            return Ok(points);
        }
    }
    Ok(VecDeque::new())
}

pub(super) fn route_points(
    terrain: &TerrainWorld,
    from: [f32; 3],
    goal: [f32; 3],
) -> Result<Option<VecDeque<[f32; 2]>>, EngineError> {
    let route = terrain
        .route(RouteRequest { start: from, goal })
        .map_err(EngineError::Terrain)?;
    Ok(route.and_then(|route| {
        let mut points = route
            .areas
            .windows(2)
            .map(|pair| terrain.route_crossing(pair[0], pair[1]))
            .collect::<Option<VecDeque<_>>>()?;
        points.push_back([goal[0], goal[1]]);
        Some(points)
    }))
}

impl PreparedPatrol {
    pub(super) fn apply_poses(&self, world: &mut World) -> Result<(), EngineError> {
        // These handles were read during preparation and the world has yet to step.
        for &(handle, _, after) in &self.poses {
            set_transform(world, handle, after)?;
        }
        Ok(())
    }

    pub(super) fn restore_poses(&self, world: &mut World) -> Result<(), EngineError> {
        for &(handle, before, _) in &self.poses {
            set_transform(world, handle, before)?;
        }
        Ok(())
    }
}
