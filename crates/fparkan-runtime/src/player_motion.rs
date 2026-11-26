use super::{transform_state, Engine, EngineError, StartingCockpit, SurfaceQuery, TransformState};
use crate::hero_look::{turret_pitch, HeroLook};
use crate::manual_flight::{FlightDrive, FlightProfile};
use crate::{hero_walk::WalkingDrive, TerrainWorld};
use fparkan_resource::CachedResourceRepository;
use fparkan_terrain::{FullSurfaceMask, TerrainSupport};

pub(super) fn warbot_can_fly(engine: &crate::Engine, object_index: usize) -> bool {
    let profile = || {
        let draft = engine.loaded.as_ref()?.object_drafts.get(object_index)?;
        let resources = CachedResourceRepository::new(engine.services.vfs.as_ref()?.clone());
        crate::unit_chassis::can_fly(&resources, draft)
    };
    profile().unwrap_or_else(|| {
        eprintln!("sim_coverage player_chassis_profile_stub=1 object={object_index}");
        false
    })
}

pub(super) fn drowns(engine: &Engine, cockpit: StartingCockpit, step: PlayerMotionStep) -> bool {
    step.touches_water()
        && engine.loaded.as_ref().is_some_and(|mission| {
            mission.boarding.is_none() && mission.local_hero == Some(cockpit.handle)
        })
}

// shortcut: bound stair steps and drops until the native hero collider is wired.
const STEP_UP: f32 = 0.85;
const STEP_DOWN: f32 = 2.0;
const FLOOR_CLEARANCE: f32 = 0.02;
const WATER_SURFACE: u32 = 0x0002_0000;
// shortcut: sample the temporary floor probe at most 0.16 units apart until native collision is wired.
const WALK_COLLISION_DISTANCE: f32 = 0.16;
const WALK_SPEED_FALLBACK: f32 = 8.0;

pub(super) fn spawn_floor_clearance(
    terrain: &TerrainWorld,
    [x, y, z]: [f32; 3],
) -> Result<f32, EngineError> {
    let floor = terrain
        .height_at([x, y])
        .map_err(|error| EngineError::Movement(error.to_string()))?;
    Ok(floor.map_or(FLOOR_CLEARANCE, |floor| {
        (z - floor).clamp(FLOOR_CLEARANCE, STEP_DOWN)
    }))
}

/// Player controls sampled once for a simulation step.
#[derive(Clone, Copy, Debug, Default)]
pub struct PlayerMotion {
    /// Signed strafe and forward input in the range -1 to 1.
    pub movement: [f32; 2],
    /// Signed climb input for a flying vehicle, in the range -1 to 1.
    pub climb: f32,
    /// Relative horizontal mouse motion for this step.
    pub mouse_x: f32,
    /// Relative vertical mouse motion for this step, positive down.
    pub mouse_y: f32,
    /// Time spent on this simulation step, excluding time spent paused.
    pub delta_seconds: f32,
}

/// Proposed hero transform and the body-level sweep used to check buildings.
#[derive(Clone, Copy, Debug)]
pub struct PlayerMotionStep {
    pub(super) pose: TransformState,
    pub(super) pitch_bits: u32,
    flight_velocity: Option<[f32; 3]>,
    hero_walk_velocity: Option<[f32; 2]>,
    hero_look: Option<HeroLook>,
    max_floor_clearance: f32,
    previous_terrain_height: Option<f32>,
    terrain_height: Option<f32>,
    terrain_normal: Option<[f32; 3]>,
    surface: WalkingSurface,
    /// Sweep segment when the step translates the hero.
    pub sweep: Option<[[f32; 3]; 2]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WalkingSurface {
    Ground,
    Building,
    Water,
}

impl WalkingSurface {
    fn terrain(mask: FullSurfaceMask) -> Self {
        if mask.0 & WATER_SURFACE != 0 {
            Self::Water
        } else {
            Self::Ground
        }
    }
}

impl PlayerMotionStep {
    /// Uses the scene's terrain after foundation cuts and fills.
    #[must_use]
    pub fn with_scene_terrain(
        mut self,
        previous: Option<f32>,
        next: Option<TerrainSupport>,
    ) -> Self {
        self.previous_terrain_height = previous;
        self.terrain_height = next.map(|plane| plane.height);
        self.terrain_normal = next.map(|plane| plane.normal);
        self.surface = next.map_or(WalkingSurface::Ground, |plane| {
            WalkingSurface::terrain(plane.mask)
        });
        if let Some([from, to]) = self.sweep.as_mut() {
            to[2] = previous
                .zip(self.terrain_height)
                .map_or(from[2], |(previous, next)| next + (from[2] - previous));
            self.pose.position[2] = (to[2] - 0.5).to_bits();
        }
        self
    }

    /// Vertical segment for finding a nearby building floor at the proposed XY.
    #[must_use]
    pub fn floor_probe(self) -> Option<[[f32; 3]; 2]> {
        if self.flight_velocity.is_some() {
            return None;
        }
        let [[_, _, start_z], [x, y, _]] = self.sweep?;
        let feet_z = start_z - 0.5;
        Some([[x, y, feet_z + STEP_UP], [x, y, feet_z - STEP_DOWN]])
    }

    /// Looks below the previous position for the surface carrying the player.
    #[must_use]
    pub fn previous_floor_probe(self) -> Option<[[f32; 3]; 2]> {
        self.floor_probe()?;
        let [[x, y, body_z], _] = self.sweep?;
        let feet_z = body_z - 0.5;
        Some([[x, y, feet_z + 0.05], [x, y, feet_z - STEP_DOWN]])
    }

    /// Terrain can take over at the end of a floor within the same step limits.
    #[must_use]
    pub fn terrain_exit_floor(self) -> Option<f32> {
        let [top, bottom] = self.floor_probe()?;
        let [_, end] = self.sweep?;
        let height = self.terrain_height?;
        ((bottom[2]..=top[2]).contains(&height)
            && (end[2] - (height + FLOOR_CLEARANCE + 0.5)).abs() > 0.05)
            .then_some(height)
    }

    /// Replaces the terrain prediction with the accepted floor, preserving the
    /// player's offset above the surface that carried them on the previous step.
    #[must_use]
    pub fn with_building_floor(mut self, height: f32, previous_mesh: Option<f32>) -> Self {
        let Some([top, bottom]) = self.previous_floor_probe() else {
            return self;
        };
        let previous_surface = previous_mesh
            .into_iter()
            .chain(self.previous_terrain_height)
            .filter(|height| (bottom[2]..=top[2]).contains(height))
            .max_by(f32::total_cmp);
        let sweep = self.sweep.as_mut().expect("floor probe requires a sweep");
        let clearance = previous_surface.map_or(FLOOR_CLEARANCE, |surface| {
            (sweep[0][2] - 0.5 - surface).clamp(FLOOR_CLEARANCE, self.max_floor_clearance)
        });
        self.pose.position[2] = (height + clearance).to_bits();
        sweep[1][2] = height + clearance + 0.5;
        self.surface = WalkingSurface::Building;
        self
    }

    /// Lands on a terrain surface beyond the end of a building floor.
    #[must_use]
    pub fn with_terrain_floor(self, height: f32, previous_mesh: Option<f32>) -> Self {
        Self {
            surface: self.surface,
            ..self.with_building_floor(height, previous_mesh)
        }
    }

    fn terrain_slope(self) -> Option<[f32; 3]> {
        (self.surface == WalkingSurface::Ground && self.sweep.is_some())
            .then_some(self.terrain_normal)
            .flatten()
    }

    pub(super) fn touches_water(self) -> bool {
        self.surface == WalkingSurface::Water
            && self.terrain_height.is_some_and(|height| {
                f32::from_bits(self.pose.position[2]) <= height + self.max_floor_clearance + 0.05
            })
    }

    pub(super) fn on_building_floor(self) -> bool {
        self.surface == WalkingSurface::Building
    }

    /// Sweeps vertically before crossing a step up, and horizontally before a step down.
    #[must_use]
    pub fn floor_sweeps(self) -> Option<[[[f32; 3]; 2]; 2]> {
        let [start, end] = self.sweep?;
        let middle = if end[2] > start[2] {
            [start[0], start[1], end[2]]
        } else {
            [end[0], end[1], start[2]]
        };
        Some([[start, middle], [middle, end]])
    }
}

#[cfg(test)]
mod tests {
    use super::{PlayerMotionStep, TransformState, FLOOR_CLEARANCE};

    #[test]
    fn building_floor_probe_uses_proposed_xy_and_previous_height() {
        let step = PlayerMotionStep {
            pose: TransformState {
                position: [4.0_f32, 6.0, 15.0].map(f32::to_bits),
                ..TransformState::default()
            },
            pitch_bits: 0,
            flight_velocity: None,
            hero_walk_velocity: None,
            hero_look: None,
            max_floor_clearance: FLOOR_CLEARANCE,
            previous_terrain_height: None,
            terrain_height: Some(11.5),
            terrain_normal: Some([0.0, -0.8, 0.6]),
            surface: super::WalkingSurface::Ground,
            sweep: Some([[3.0, 6.0, 12.5], [4.0, 6.0, 15.5]]),
        };
        assert_eq!(
            step.floor_probe(),
            Some([[4.0, 6.0, 12.85], [4.0, 6.0, 10.0]])
        );
        assert_eq!(step.terrain_exit_floor(), Some(11.5));
        let lowered = step.with_building_floor(11.5, Some(12.0));
        assert_eq!(step.terrain_slope(), Some([0.0, -0.8, 0.6]));
        assert_eq!(lowered.terrain_slope(), None);
        assert_eq!(lowered.pose.position.map(f32::from_bits), [4.0, 6.0, 11.52]);
        assert_eq!(lowered.sweep.unwrap()[1], [4.0, 6.0, 12.02]);
        assert_eq!(
            lowered.floor_sweeps().unwrap(),
            [
                [[3.0, 6.0, 12.5], [4.0, 6.0, 12.5]],
                [[4.0, 6.0, 12.5], [4.0, 6.0, 12.02]],
            ]
        );
        assert_eq!(
            step.with_building_floor(12.5, Some(12.0))
                .floor_sweeps()
                .unwrap()[0],
            [[3.0, 6.0, 12.5], [3.0, 6.0, 13.02]]
        );
    }

    #[test]
    fn floor_transition_preserves_offset_from_terrain_and_mesh() {
        let step = PlayerMotionStep {
            pose: TransformState::default(),
            pitch_bits: 0,
            flight_velocity: None,
            hero_walk_velocity: None,
            hero_look: None,
            max_floor_clearance: 1.25,
            previous_terrain_height: Some(10.0),
            terrain_height: Some(10.0),
            terrain_normal: None,
            surface: super::WalkingSurface::Ground,
            sweep: Some([[3.0, 6.0, 11.75], [4.0, 6.0, 11.75]]),
        };
        assert_eq!(
            f32::from_bits(step.with_building_floor(10.25, None).pose.position[2]),
            11.5
        );
        assert_eq!(
            f32::from_bits(step.with_building_floor(10.0, Some(10.25)).pose.position[2]),
            11.0
        );
    }

    #[test]
    fn a_building_floor_over_water_carries_the_hero() {
        let step = PlayerMotionStep {
            pose: TransformState::default(),
            pitch_bits: 0,
            flight_velocity: None,
            hero_walk_velocity: None,
            hero_look: None,
            max_floor_clearance: FLOOR_CLEARANCE,
            previous_terrain_height: None,
            terrain_height: None,
            terrain_normal: None,
            surface: super::WalkingSurface::Ground,
            sweep: Some([[3.0, 6.0, 10.52], [4.0, 6.0, 10.52]]),
        }
        .with_scene_terrain(
            Some(10.0),
            Some(fparkan_terrain::TerrainSupport {
                height: 10.0,
                mask: super::FullSurfaceMask(super::WATER_SURFACE),
                normal: [0.0, 0.0, 1.0],
            }),
        );
        assert!(step.touches_water());
        assert!(!step.with_building_floor(10.0, None).touches_water());
        assert!(step.with_terrain_floor(10.0, None).touches_water());
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_constructor_walk_into_water_fails_immediately() {
        use crate::{
            create, frame, frame_with_player_motion, load_gameplay_mission,
            loaded_mission_script_outcome, plan_player_motion_with_collision, starting_cockpit,
            EngineServices, MissionMode, MissionRequest, MissionScriptOutcome, MissionStartupInput,
            PlayerMotion,
        };
        use fparkan_vfs::DirectoryVfs;
        use std::sync::Arc;

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
        let cockpit = starting_cockpit(&engine).unwrap();
        frame(&mut engine, 0).unwrap();
        for now in (250..=130_000).step_by(250) {
            let state = super::transform_state(&engine.world, cockpit.handle).unwrap();
            let [x, y, _] = state.position.map(f32::from_bits);
            let [dx, dy] = [1158.739 - x, 816.309 - y];
            let distance = dx.hypot(dy);
            assert!(distance > 0.25, "the walk reached the middle of the lake");
            let (sin, cos) = f32::from_bits(state.orientation[2]).sin_cos();
            let step = plan_player_motion_with_collision(
                &engine,
                cockpit,
                PlayerMotion {
                    movement: [
                        (dx * cos + dy * sin) / distance,
                        (dy * cos - dx * sin) / distance,
                    ],
                    delta_seconds: 0.25,
                    ..Default::default()
                },
                |step| Ok((step, false)),
            )
            .unwrap()
            .unwrap();
            let wet = step.touches_water();
            let result = frame_with_player_motion(&mut engine, now, cockpit, step, false).unwrap();
            let alive = result
                .snapshot
                .transforms
                .iter()
                .any(|(handle, _)| *handle == cockpit.handle);
            if wet {
                assert!(!alive, "the hero survived water contact");
                assert_eq!(
                    loaded_mission_script_outcome(&engine),
                    Some(MissionScriptOutcome::Failed)
                );
                return;
            }
            assert!(alive, "the hero died before reaching water");
        }
        panic!("the hero did not enter the lake");
    }
}

/// Proposes a hero step from the world pose and the active terrain.
///
/// # Errors
///
/// Returns an error for invalid motion values or a failed terrain query.
pub fn plan_player_motion(
    engine: &Engine,
    cockpit: StartingCockpit,
    input: PlayerMotion,
) -> Result<Option<PlayerMotionStep>, EngineError> {
    plan_player_steps(engine, cockpit, input, None)
}

/// Resolves walking in short sweeps and returns the last reachable pose for this frame.
///
/// # Errors
/// Returns an error if motion or the scene collision query fails.
pub fn plan_player_motion_with_collision(
    engine: &Engine,
    cockpit: StartingCockpit,
    input: PlayerMotion,
    mut resolve: impl FnMut(PlayerMotionStep) -> Result<(PlayerMotionStep, bool), EngineError>,
) -> Result<Option<PlayerMotionStep>, EngineError> {
    plan_player_steps(engine, cockpit, input, Some(&mut resolve))
}

type ResolveMotion<'a> =
    &'a mut dyn FnMut(PlayerMotionStep) -> Result<(PlayerMotionStep, bool), EngineError>;

fn plan_player_steps(
    engine: &Engine,
    cockpit: StartingCockpit,
    mut input: PlayerMotion,
    mut resolve: Option<ResolveMotion<'_>>,
) -> Result<Option<PlayerMotionStep>, EngineError> {
    let (Some(mission), Ok(pose)) = (
        &engine.loaded,
        transform_state(&engine.world, cockpit.handle),
    ) else {
        return Ok(None);
    };
    let boarded = mission
        .boarding
        .filter(|boarding| boarding.cockpit.handle == cockpit.handle);
    let flight = boarded.filter(|boarding| boarding.can_fly).map(|boarding| {
        let profile = mission
            .control_device_tables
            .iter()
            .find(|table| table.handle == cockpit.handle)
            .and_then(|table| table.owner_profile.as_ref())
            .and_then(|profile| FlightProfile::from_control(&profile.current_words))
            .unwrap_or(FlightProfile::FALLBACK);
        FlightDrive {
            profile,
            velocity: boarding.flight_velocity,
        }
    });
    let flying = flight.is_some();
    let chassis_ground = boarded.and_then(|boarding| boarding.ground);
    let hero_profile = if mission.local_hero == Some(cockpit.handle) {
        mission
            .control_device_tables
            .iter()
            .find(|table| table.handle == cockpit.handle)
            .and_then(|table| table.owner_profile.as_ref())
            .map(|profile| &profile.current_words)
    } else {
        None
    };
    let mut walking = hero_profile
        .and_then(|profile| WalkingDrive::from_profile(profile, mission.hero_walk_velocity));
    let walking_motion =
        input.movement != [0.0; 2] || (walking.is_some() && mission.hero_walk_velocity != [0.0; 2]);
    let hero_control = boarded.is_none() && mission.local_hero == Some(cockpit.handle);
    if !walking_motion
        && flight.is_none_or(|drive| input.climb == 0.0 && drive.velocity == [0.0; 3])
        && input.mouse_x == 0.0
        && input.mouse_y == 0.0
        && (!hero_control || !mission.hero_look.has_state())
    {
        return Ok(None);
    }
    let pitch = f32::from_bits(mission.player_pitch_bits);
    if ![input.delta_seconds, input.mouse_x, input.mouse_y]
        .into_iter()
        .all(f32::is_finite)
        || input.delta_seconds < 0.0
    {
        return Err(EngineError::Movement("invalid player step".into()));
    }
    let mut hero_look = hero_control.then_some(mission.hero_look);
    // shortcut: mounted chassis use fixed gains until their bindings are wired.
    let fallback_pitch = || (pitch - input.mouse_y * 0.005).clamp(-1.0, 1.0);
    let (mut yaw_delta, pitch) = if let Some(look) = hero_look.as_mut() {
        let max_angle = hero_profile
            .map(|words| f32::from_bits(words[11]))
            .filter(|speed| speed.is_finite() && *speed >= 0.0)
            .map_or(f32::INFINITY, |speed| speed * input.delta_seconds);
        let [yaw, blend_delta] = look.advance([input.mouse_x, input.mouse_y], max_angle);
        let next_pitch = || {
            let control = mission
                .mission_assets
                .object_component_controls
                .get(cockpit.object_index)?
                .get(cockpit.component_index)?
                .as_ref()?;
            let row = fparkan_assets::turret_elevation_row(control)?;
            turret_pitch(row, pitch, blend_delta)
        };
        let pitch = if blend_delta != 0.0 {
            next_pitch().unwrap_or_else(fallback_pitch)
        } else {
            pitch
        };
        (yaw, pitch)
    } else {
        (
            -input.mouse_x.clamp(-1_000.0, 1_000.0) * 0.003,
            fallback_pitch(),
        )
    };
    let speeds = if boarded.is_some() && !flying {
        crate::manual_chassis::speed_limits(mission, cockpit.handle)
            .unwrap_or([WALK_SPEED_FALLBACK; 2])
    } else {
        let forward = hero_profile
            .map(|words| f32::from_bits(words[7]))
            .filter(|speed| speed.is_finite() && *speed >= 0.0)
            .unwrap_or(WALK_SPEED_FALLBACK);
        [WALK_SPEED_FALLBACK, forward]
    };
    // shortcut: use the outdoor spawn clearance until the native walking collider is wired.
    let mut max_floor_clearance = FLOOR_CLEARANCE;
    if !flying && walking_motion {
        if let Some(draft) = mission.object_drafts.get(cockpit.object_index) {
            max_floor_clearance = spawn_floor_clearance(&mission.terrain, draft.position)?;
        }
    }
    let count = if resolve.is_some() && !flying && walking_motion {
        // shortcut: bound movement after a long stall while the scene uses the render clock.
        input.delta_seconds = input.delta_seconds.min(0.5);
        let [across, along] = walking.map_or_else(
            || body_velocity(input.movement, speeds),
            |drive| drive.limits,
        );
        (input.delta_seconds * across.hypot(along) / WALK_COLLISION_DISTANCE).ceil() as u32
    } else {
        1
    }
    .max(1);
    input.delta_seconds /= count as f32;
    let mut previous = pose;
    let mut previous_surface = WalkingSurface::Ground;
    for index in 0..count {
        let mut propose = |walk: Option<([f32; 2], [f32; 2])>| {
            let ground_displacement = walk.map_or_else(
                || body_velocity(input.movement, speeds).map(|speed| speed * input.delta_seconds),
                |(_, distance)| distance,
            );
            let mut step = plan_motion(
                &mission.terrain,
                previous,
                pitch,
                yaw_delta,
                flight,
                ground_displacement,
                input,
            )?;
            step.max_floor_clearance = max_floor_clearance;
            step.hero_walk_velocity = walk.map(|(velocity, _)| velocity);
            step.hero_look = hero_look;
            match resolve.as_mut() {
                Some(resolve) => resolve(step),
                None => Ok((step, false)),
            }
        };
        let advance = walking.map(|drive| drive.advance(input.movement, input.delta_seconds));
        let (mut step, mut blocked) = propose(advance)?;
        if let (false, Some(drive), Some(normal), Some((velocity, _))) =
            (blocked, walking, step.terrain_slope(), advance)
        {
            let yaw = f32::from_bits(step.pose.orientation[2]);
            if let Some(braked) = drive.brake(velocity, normal, yaw, input.delta_seconds) {
                (step, blocked) = propose(Some(braked))?;
            }
        }
        if blocked {
            step.pose.position = previous.position;
            step.sweep = None;
            step.surface = previous_surface;
            step.flight_velocity = step.flight_velocity.map(|_| [0.0; 3]);
            step.hero_walk_velocity = step.hero_walk_velocity.map(|_| [0.0; 2]);
            return Ok(Some(step));
        }
        crate::manual_chassis::support_step(chassis_ground, &mission.terrain, previous, &mut step)?;
        if index + 1 == count || drowns(engine, cockpit, step) {
            return Ok(Some(step));
        }
        yaw_delta = 0.0;
        previous = step.pose;
        previous_surface = step.surface;
        if let (Some(drive), Some(velocity)) = (walking.as_mut(), step.hero_walk_velocity) {
            drive.velocity = velocity;
        }
    }
    unreachable!("a movement sample always has at least one step")
}

pub(super) fn commit_motion_state(
    mission: &mut crate::LoadedMissionState,
    cockpit: StartingCockpit,
    step: PlayerMotionStep,
    blocked: bool,
) {
    if mission.local_hero == Some(cockpit.handle) {
        if let Some(velocity) = step.hero_walk_velocity {
            mission.hero_walk_velocity = if blocked { [0.0; 2] } else { velocity };
        }
        if mission.boarding.is_none() {
            if let Some(look) = step.hero_look {
                mission.hero_look = look;
            }
        }
    }
    if let (Some(boarding), Some(velocity)) = (&mut mission.boarding, step.flight_velocity) {
        if boarding.cockpit.handle == cockpit.handle {
            boarding.flight_velocity = if blocked { [0.0; 3] } else { velocity };
        }
    }
}

fn body_velocity(movement: [f32; 2], [side_speed, forward_speed]: [f32; 2]) -> [f32; 2] {
    let [right, forward] = movement.map(|axis| axis.clamp(-1.0, 1.0));
    let length = right.hypot(forward).max(1.0);
    [
        right * side_speed / length,
        forward * forward_speed / length,
    ]
}

fn plan_motion(
    terrain: &TerrainWorld,
    mut pose: TransformState,
    pitch: f32,
    yaw_delta: f32,
    flight: Option<FlightDrive>,
    ground_displacement: [f32; 2],
    input: PlayerMotion,
) -> Result<PlayerMotionStep, EngineError> {
    let from = pose.position.map(f32::from_bits);
    let yaw = f32::from_bits(pose.orientation[2]);
    if !input
        .movement
        .into_iter()
        .chain([input.climb, input.delta_seconds, yaw, yaw_delta, pitch])
        .chain(from)
        .all(f32::is_finite)
        || input.delta_seconds < 0.0
    {
        return Err(EngineError::Movement("invalid player step".into()));
    }
    let yaw = if yaw_delta != 0.0 {
        let yaw = (yaw + yaw_delta).rem_euclid(std::f32::consts::TAU);
        pose.orientation[2] = yaw.to_bits();
        yaw
    } else {
        yaw
    };
    let mut sweep = None;
    let mut flight_velocity = None;
    let mut previous_terrain_height = None;
    let mut terrain_height = None;
    let mut terrain_normal = None;
    let mut surface = WalkingSurface::Ground;
    if let Some(flight) = flight {
        let [across, along] = input.movement;
        let (to, velocity) = flight.step(
            terrain,
            from,
            yaw,
            [across, along, input.climb],
            input.delta_seconds,
        )?;
        flight_velocity = Some(velocity);
        if to != from {
            pose.position = to.map(f32::to_bits);
            sweep = Some([
                [from[0], from[1], from[2] + 0.5],
                [to[0], to[1], to[2] + 0.5],
            ]);
        }
    } else {
        let [across, along] = ground_displacement;
        let (sin, cos) = yaw.sin_cos();
        let xy = [
            from[0] + cos * across - sin * along,
            from[1] + sin * across + cos * along,
        ];
        if !xy.iter().all(|axis| axis.is_finite()) {
            return Err(EngineError::Movement(
                "player step exceeds finite coordinates".into(),
            ));
        }
        if xy != [from[0], from[1]] {
            if let (Some(old_ground), Some(plane)) = (
                terrain
                    .height_at([from[0], from[1]])
                    .map_err(|err| EngineError::Movement(err.to_string()))?,
                terrain
                    .support_plane_at(xy)
                    .map_err(|err| EngineError::Movement(err.to_string()))?,
            ) {
                let to = [xy[0], xy[1], plane.height + (from[2] - old_ground)];
                previous_terrain_height = Some(old_ground);
                terrain_height = Some(plane.height);
                terrain_normal = Some(plane.normal);
                surface = WalkingSurface::terrain(plane.mask);
                pose.position = to.map(f32::to_bits);
                // shortcut: one sweep at body height until the physical collider
                // and floor transitions are recovered.
                sweep = Some([
                    [from[0], from[1], from[2] + 0.5],
                    [to[0], to[1], to[2] + 0.5],
                ]);
            }
        }
    }
    Ok(PlayerMotionStep {
        pose,
        pitch_bits: pitch.to_bits(),
        flight_velocity,
        hero_walk_velocity: None,
        hero_look: None,
        max_floor_clearance: FLOOR_CLEARANCE,
        previous_terrain_height,
        terrain_height,
        terrain_normal,
        surface,
        sweep,
    })
}
