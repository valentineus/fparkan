use super::{transform_state, Engine, EngineError, StartingCockpit, SurfaceQuery, TransformState};
use crate::TerrainWorld;
use fparkan_resource::CachedResourceRepository;
use fparkan_terrain::FullSurfaceMask;

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
const WALK_COLLISION_INTERVAL: f32 = 0.02;

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
    flying: bool,
    max_floor_clearance: f32,
    previous_terrain_height: Option<f32>,
    terrain_height: Option<f32>,
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
        next: Option<(f32, FullSurfaceMask)>,
    ) -> Self {
        self.previous_terrain_height = previous;
        self.terrain_height = next.map(|(height, _)| height);
        self.surface = next.map_or(WalkingSurface::Ground, |(_, mask)| {
            WalkingSurface::terrain(mask)
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
        if self.flying {
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
            flying: false,
            max_floor_clearance: FLOOR_CLEARANCE,
            previous_terrain_height: None,
            terrain_height: Some(11.5),
            surface: super::WalkingSurface::Ground,
            sweep: Some([[3.0, 6.0, 12.5], [4.0, 6.0, 15.5]]),
        };
        assert_eq!(
            step.floor_probe(),
            Some([[4.0, 6.0, 12.85], [4.0, 6.0, 10.0]])
        );
        assert_eq!(step.terrain_exit_floor(), Some(11.5));
        let lowered = step.with_building_floor(11.5, Some(12.0));
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
            flying: false,
            max_floor_clearance: 1.25,
            previous_terrain_height: Some(10.0),
            terrain_height: Some(10.0),
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
            flying: false,
            max_floor_clearance: FLOOR_CLEARANCE,
            previous_terrain_height: None,
            terrain_height: None,
            surface: super::WalkingSurface::Ground,
            sweep: Some([[3.0, 6.0, 10.52], [4.0, 6.0, 10.52]]),
        }
        .with_scene_terrain(
            Some(10.0),
            Some((10.0, super::FullSurfaceMask(super::WATER_SURFACE))),
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
    resolve: Option<ResolveMotion<'_>>,
) -> Result<Option<PlayerMotionStep>, EngineError> {
    let (Some(mission), Ok(pose)) = (
        &engine.loaded,
        transform_state(&engine.world, cockpit.handle),
    ) else {
        return Ok(None);
    };
    let flying = mission
        .boarding
        .is_some_and(|boarding| boarding.cockpit.handle == cockpit.handle && boarding.can_fly);
    if input.movement == [0.0; 2]
        && (!flying || input.climb == 0.0)
        && input.mouse_x == 0.0
        && input.mouse_y == 0.0
    {
        return Ok(None);
    }
    let pitch = f32::from_bits(mission.player_pitch_bits);
    if !input.delta_seconds.is_finite() || input.delta_seconds < 0.0 {
        return Err(EngineError::Movement("invalid player step".into()));
    }
    // shortcut: use the outdoor spawn clearance until the native walking collider is wired.
    let mut max_floor_clearance = FLOOR_CLEARANCE;
    if !flying && input.movement != [0.0; 2] {
        if let Some(draft) = mission.object_drafts.get(cockpit.object_index) {
            let [x, y, z] = draft.position;
            if let Some(ground) = mission
                .terrain
                .height_at([x, y])
                .map_err(|err| EngineError::Movement(err.to_string()))?
            {
                max_floor_clearance = (z - ground).clamp(FLOOR_CLEARANCE, STEP_DOWN);
            }
        }
    }
    let Some(resolve) = resolve else {
        let mut step = plan_motion(&mission.terrain, pose, pitch, flying, input)?;
        step.max_floor_clearance = max_floor_clearance;
        return Ok(Some(step));
    };
    let count = if !flying && input.movement != [0.0; 2] {
        // shortcut: bound movement after a long stall while the scene uses the render clock.
        input.delta_seconds = input.delta_seconds.min(0.5);
        (input.delta_seconds / WALK_COLLISION_INTERVAL).ceil() as u32
    } else {
        1
    }
    .max(1);
    input.delta_seconds /= count as f32;
    let mut step = plan_motion(&mission.terrain, pose, pitch, flying, input)?;
    let mut previous = pose;
    let mut previous_surface = WalkingSurface::Ground;
    input.mouse_x = 0.0;
    input.mouse_y = 0.0;
    for index in 0..count {
        if index != 0 {
            step = plan_motion(
                &mission.terrain,
                previous,
                f32::from_bits(step.pitch_bits),
                flying,
                input,
            )?;
        }
        step.max_floor_clearance = max_floor_clearance;
        let (resolved, blocked) = resolve(step)?;
        step = resolved;
        if blocked {
            step.pose.position = previous.position;
            step.sweep = None;
            step.surface = previous_surface;
            break;
        }
        if drowns(engine, cockpit, step) {
            break;
        }
        previous = step.pose;
        previous_surface = step.surface;
    }
    Ok(Some(step))
}

fn plan_motion(
    terrain: &TerrainWorld,
    mut pose: TransformState,
    pitch: f32,
    flying: bool,
    input: PlayerMotion,
) -> Result<PlayerMotionStep, EngineError> {
    let from = pose.position.map(f32::from_bits);
    let yaw = f32::from_bits(pose.orientation[2]);
    if !input
        .movement
        .into_iter()
        .chain([
            input.mouse_x,
            input.mouse_y,
            input.climb,
            input.delta_seconds,
            yaw,
            pitch,
        ])
        .chain(from)
        .all(f32::is_finite)
        || input.delta_seconds < 0.0
    {
        return Err(EngineError::Movement("invalid player step".into()));
    }
    // shortcut: use constant walk speed and mouse gain until native Control
    // acceleration and mouse sensitivity are measured.
    let yaw = if input.mouse_x != 0.0 {
        let yaw = (yaw - input.mouse_x.clamp(-1_000.0, 1_000.0) * 0.003)
            .rem_euclid(std::f32::consts::TAU);
        pose.orientation[2] = yaw.to_bits();
        yaw
    } else {
        yaw
    };
    let [right, forward] = input.movement.map(|axis| axis.clamp(-1.0, 1.0));
    // shortcut: constant flight rates until thrust, clearance and inertia are measured.
    let speed = if flying { 40.0 } else { 8.0 };
    let distance = speed * input.delta_seconds / right.hypot(forward).max(1.0);
    let (sin, cos) = yaw.sin_cos();
    let xy = [
        from[0] + (cos * right - sin * forward) * distance,
        from[1] + (sin * right + cos * forward) * distance,
    ];
    let climb = if flying {
        input.climb.clamp(-1.0, 1.0) * (16.0 * input.delta_seconds)
    } else {
        0.0
    };
    let altitude = from[2] + climb;
    if !xy.iter().all(|axis| axis.is_finite()) || !altitude.is_finite() {
        return Err(EngineError::Movement(
            "player step exceeds finite coordinates".into(),
        ));
    }
    let mut sweep = None;
    let mut previous_terrain_height = None;
    let mut terrain_height = None;
    let mut surface = WalkingSurface::Ground;
    if flying && (xy != [from[0], from[1]] || climb != 0.0) {
        let old_xy = [from[0], from[1]];
        let floor = |xy| {
            terrain
                .height_at(xy)
                .map_err(|err| EngineError::Movement(err.to_string()))
        };
        if let Some(old_height) = floor(old_xy)? {
            let bottom = old_height + 0.02;
            // A grounded machine can taxi. In the air, climb above an obstacle before crossing it.
            let grounded = from[2] <= bottom && climb <= 0.0;
            let floor_at_target = floor(xy)?.map(|height| height + 0.02);
            let (accepted_xy, height) = match floor_at_target {
                Some(floor) if grounded || floor <= altitude => (xy, floor),
                _ => (old_xy, bottom),
            };
            let [x, y] = accepted_xy;
            let to = [x, y, altitude.max(height)];
            if to != from {
                pose.position = to.map(f32::to_bits);
                sweep = Some([
                    [from[0], from[1], from[2] + 0.5],
                    [to[0], to[1], to[2] + 0.5],
                ]);
            }
        }
    } else if xy != [from[0], from[1]] {
        if let (Some(old_ground), Some((new_ground, mask))) = (
            terrain
                .height_at([from[0], from[1]])
                .map_err(|err| EngineError::Movement(err.to_string()))?,
            terrain
                .support_at(xy)
                .map_err(|err| EngineError::Movement(err.to_string()))?,
        ) {
            let to = [xy[0], xy[1], new_ground + (from[2] - old_ground)];
            previous_terrain_height = Some(old_ground);
            terrain_height = Some(new_ground);
            surface = WalkingSurface::terrain(mask);
            pose.position = to.map(f32::to_bits);
            // shortcut: one sweep at body height until the physical collider
            // and floor transitions are recovered.
            sweep = Some([
                [from[0], from[1], from[2] + 0.5],
                [to[0], to[1], to[2] + 0.5],
            ]);
        }
    }
    // shortcut: use a bounded camera aim until the native turret travel and
    // sensitivity are recovered. The body remains aligned to the terrain.
    let pitch = (pitch - input.mouse_y * 0.005).clamp(-1.0, 1.0);
    Ok(PlayerMotionStep {
        pose,
        pitch_bits: pitch.to_bits(),
        flying,
        max_floor_clearance: FLOOR_CLEARANCE,
        previous_terrain_height,
        terrain_height,
        surface,
        sweep,
    })
}
