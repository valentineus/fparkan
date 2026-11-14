use super::{transform_state, Engine, EngineError, StartingCockpit, SurfaceQuery, TransformState};
use crate::TerrainWorld;

/// Player controls sampled once for a simulation step.
#[derive(Clone, Copy, Debug, Default)]
pub struct PlayerMotion {
    /// Signed strafe and forward input in the range -1 to 1.
    pub movement: [f32; 2],
    /// Relative horizontal mouse motion for this step.
    pub mouse_x: f32,
    /// Time spent on this simulation step, excluding time spent paused.
    pub delta_seconds: f32,
}

/// Proposed hero transform and the body-level sweep used to check buildings.
#[derive(Clone, Copy, Debug)]
pub struct PlayerMotionStep {
    pub(super) pose: TransformState,
    /// Sweep segment when the step translates the hero.
    pub sweep: Option<[[f32; 3]; 2]>,
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
    if input.movement == [0.0; 2] && input.mouse_x == 0.0 {
        return Ok(None);
    }
    let (Some(mission), Ok(pose)) = (
        &engine.loaded,
        transform_state(&engine.world, cockpit.handle),
    ) else {
        return Ok(None);
    };
    plan_motion(&mission.terrain, pose, input).map(Some)
}

fn plan_motion(
    terrain: &TerrainWorld,
    mut pose: TransformState,
    input: PlayerMotion,
) -> Result<PlayerMotionStep, EngineError> {
    let from = pose.position.map(f32::from_bits);
    let yaw = f32::from_bits(pose.orientation[2]);
    if !input
        .movement
        .into_iter()
        .chain([input.mouse_x, input.delta_seconds, yaw])
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
    let distance = 8.0 * input.delta_seconds.min(0.1) / right.hypot(forward).max(1.0);
    let (sin, cos) = yaw.sin_cos();
    let xy = [
        from[0] + (cos * right - sin * forward) * distance,
        from[1] + (sin * right + cos * forward) * distance,
    ];
    let mut sweep = None;
    if xy != [from[0], from[1]] {
        if let (Some(old_ground), Some(new_ground)) = (
            terrain
                .height_at([from[0], from[1]])
                .map_err(|err| EngineError::Movement(err.to_string()))?,
            terrain
                .height_at(xy)
                .map_err(|err| EngineError::Movement(err.to_string()))?,
        ) {
            let to = [xy[0], xy[1], new_ground + (from[2] - old_ground)];
            pose.position = to.map(f32::to_bits);
            // shortcut: one sweep at body height until the physical collider
            // and floor transitions are recovered.
            sweep = Some([
                [from[0], from[1], from[2] + 0.5],
                [to[0], to[1], to[2] + 0.5],
            ]);
        }
    }
    Ok(PlayerMotionStep { pose, sweep })
}
