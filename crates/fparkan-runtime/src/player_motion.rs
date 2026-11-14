use super::{transform_state, Engine, EngineError, StartingCockpit, SurfaceQuery, TransformState};
use crate::TerrainWorld;

// shortcut: bound stair steps and drops until the native hero collider is wired.
const STEP_UP: f32 = 0.85;
const STEP_DOWN: f32 = 2.0;

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
    terrain_height: Option<f32>,
    /// Sweep segment when the step translates the hero.
    pub sweep: Option<[[f32; 3]; 2]>,
}

impl PlayerMotionStep {
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

    /// Terrain can take over at the end of a floor within the same step limits.
    #[must_use]
    pub fn terrain_exit_floor(self) -> Option<(f32, [[f32; 3]; 2])> {
        let [top, bottom] = self.floor_probe()?;
        let [start, end] = self.sweep?;
        let height = self.terrain_height?;
        let [x, y, body_z] = start;
        ((bottom[2]..=top[2]).contains(&height) && (end[2] - (height + 0.52)).abs() > 0.05)
            .then_some((height, [[x, y, body_z - 0.45], [x, y, body_z - 0.60]]))
    }

    /// Places the player's feet just above a floor accepted by the scene query.
    #[must_use]
    pub fn with_building_floor(mut self, height: f32) -> Self {
        if let Some(sweep) = &mut self.sweep {
            self.pose.position[2] = (height + 0.02).to_bits();
            sweep[1][2] = height + 0.52;
        }
        self
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
    use super::{PlayerMotionStep, TransformState};

    #[test]
    fn building_floor_probe_uses_proposed_xy_and_previous_height() {
        let step = PlayerMotionStep {
            pose: TransformState {
                position: [4.0_f32, 6.0, 15.0].map(f32::to_bits),
                ..TransformState::default()
            },
            pitch_bits: 0,
            flying: false,
            terrain_height: Some(11.5),
            sweep: Some([[3.0, 6.0, 12.5], [4.0, 6.0, 15.5]]),
        };
        assert_eq!(
            step.floor_probe(),
            Some([[4.0, 6.0, 12.85], [4.0, 6.0, 10.0]])
        );
        assert_eq!(
            step.terrain_exit_floor().map(|(height, _)| height),
            Some(11.5)
        );
        let lowered = step.with_building_floor(11.5);
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
            step.with_building_floor(12.5).floor_sweeps().unwrap()[0],
            [[3.0, 6.0, 12.5], [3.0, 6.0, 13.02]]
        );
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
    let (Some(mission), Ok(pose)) = (
        &engine.loaded,
        transform_state(&engine.world, cockpit.handle),
    ) else {
        return Ok(None);
    };
    let flying = mission.boarding.is_some_and(|boarding| {
        boarding.cockpit.handle == cockpit.handle
            && mission
                .object_drafts
                .get(cockpit.object_index)
                .and_then(|draft| draft.unit_components.first())
                .is_some_and(|chassis| {
                    let key = chassis.resource_raw.split(|&byte| byte == 0).next();
                    // shortcut: use the stock flying chassis until Behavior's CanFly is wired.
                    matches!(
                        key,
                        Some(
                            b"R_T_02"
                                | b"R_L_02"
                                | b"R_L_05"
                                | b"R_L_07"
                                | b"R_M_02"
                                | b"R_B_02"
                                | b"R_B_07"
                                | b"R_B_08"
                        )
                    )
                })
    });
    if input.movement == [0.0; 2]
        && (!flying || input.climb == 0.0)
        && input.mouse_x == 0.0
        && input.mouse_y == 0.0
    {
        return Ok(None);
    }
    plan_motion(
        &mission.terrain,
        pose,
        f32::from_bits(mission.player_pitch_bits),
        flying,
        input,
    )
    .map(Some)
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
    let mut terrain_height = None;
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
        if let (Some(old_ground), Some(new_ground)) = (
            terrain
                .height_at([from[0], from[1]])
                .map_err(|err| EngineError::Movement(err.to_string()))?,
            terrain
                .height_at(xy)
                .map_err(|err| EngineError::Movement(err.to_string()))?,
        ) {
            let to = [xy[0], xy[1], new_ground + (from[2] - old_ground)];
            terrain_height = Some(new_ground);
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
        terrain_height,
        sweep,
    })
}
