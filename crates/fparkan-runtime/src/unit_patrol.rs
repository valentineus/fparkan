use super::{
    autodemo::{MOBILE_UNIT, PLANT_CLASS},
    autodemo_combat::CombatTarget,
    boarding::WARBOT_CLASS,
    next_crt_rand, EngineError, MissionDocument, MissionObjectDraft, MissionScriptBundle,
    ObjectHandle, TerrainWorld, TransformState, World,
};
use fparkan_script::{Handler15TargetPayload, ScriptDispatchSelector, VarSet};
use fparkan_terrain::{NavigationQuery, RouteRequest, SurfaceQuery};
use fparkan_world::{set_transform, transform_state};
use std::collections::VecDeque;

#[derive(Clone)]
pub(super) struct UnitPatrol {
    center: Option<[f32; 2]>,
    units: Vec<PatrollingUnit>,
    clock_ms: u32,
    rng: u32,
    pub(super) factory_raid_clans: usize,
}

#[derive(Clone)]
struct PatrollingUnit {
    handle: ObjectHandle,
    center: [f32; 2],
    radius: f32,
    points: VecDeque<[f32; 2]>,
    retry_ms: u32,
    attack: Option<ObjectHandle>,
}

pub(super) struct PreparedPatrol {
    pub(super) state: UnitPatrol,
    poses: Vec<(ObjectHandle, TransformState, TransformState)>,
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
        let factories = drafts.iter().filter(|draft| {
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
            if let Some(factory) = factories
                .clone()
                .min_by(|a, b| distance(a).total_cmp(&distance(b)))
            {
                ordered |= self.order(
                    world,
                    handle,
                    [factory.position[0], factory.position[1]],
                    60.0,
                );
            }
        }
        self.factory_raid_clans += usize::from(ordered);
    }

    pub(super) fn scripted() -> Self {
        Self {
            center: None,
            units: Vec::new(),
            clock_ms: 0,
            rng: 1,
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
        self.units
            .retain(|unit| transform_state(world, unit.handle).is_ok());
        if let Some(unit) = self.units.iter_mut().find(|unit| unit.handle == handle) {
            if unit.center != center || unit.radius != radius {
                unit.center = center;
                unit.radius = radius;
                unit.points.clear();
                unit.retry_ms = 0;
                return true;
            }
            return false;
        }
        self.units.push(PatrollingUnit {
            handle,
            center,
            radius,
            points: VecDeque::new(),
            retry_ms: 0,
            attack: None,
        });
        true
    }

    pub(super) fn is_patrolling(&self, handle: ObjectHandle) -> bool {
        self.units.iter().any(|unit| unit.handle == handle)
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
                radius: 150.0,
                points: VecDeque::new(),
                retry_ms: 0,
                attack: None,
            })
            .collect();
        Some(Self {
            center: Some(center),
            units,
            clock_ms: 0,
            rng: 1,
            factory_raid_clans: 0,
        })
    }

    pub(super) fn prepare(
        &self,
        world: &World,
        terrain: &TerrainWorld,
        now_ms: u32,
        targets: &[(ObjectHandle, CombatTarget)],
    ) -> Result<PreparedPatrol, EngineError> {
        let mut state = self.clone();
        let seconds = now_ms.saturating_sub(state.clock_ms) as f32 * 0.001;
        state.clock_ms = state.clock_ms.max(now_ms);
        let mut poses = Vec::new();
        if seconds == 0.0 {
            return Ok(PreparedPatrol { state, poses });
        }
        for unit in &mut state.units {
            let Ok(before) = transform_state(world, unit.handle) else {
                continue;
            };
            let from = before.position.map(f32::from_bits);
            let enemy = targets
                .iter()
                .find(|(handle, _)| *handle == unit.handle)
                .map(|(_, target)| *target);
            if unit.attack != enemy.map(|target| target.handle) {
                unit.points.clear();
                unit.retry_ms = 0;
                unit.attack = enemy.map(|target| target.handle);
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
                } else {
                    choose_route(terrain, from, unit.center, unit.radius, &mut state.rng)?
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
            // shortcut: fixed turn rate and patrol speed until the per-chassis Control step is available.
            let (yaw, turn_seconds) = turn_toward(before, [dx, dy], seconds);
            after.orientation[2] = yaw.to_bits();
            let travel = 9.0 * (seconds - turn_seconds);
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

fn route_points(
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
