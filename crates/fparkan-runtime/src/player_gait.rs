//! First-person motion of the walking hero's camera.

use crate::{Engine, LoadedMissionState, StartingCockpit};
use fparkan_world::{transform_state, World};

const WALK_CYCLE_MS: f32 = 448.0;
const CAMERA_LIFT: f32 = 0.15;

#[derive(Default)]
pub(super) struct PlayerGait {
    previous: Option<(u32, [u32; 2])>,
    phase: f32,
    lift: f32,
}

impl PlayerGait {
    fn advance(&mut self, clock_ms: u32, xy: [u32; 2]) {
        let Some((previous_ms, previous_xy)) = self.previous.replace((clock_ms, xy)) else {
            return;
        };
        let elapsed_ms = clock_ms.wrapping_sub(previous_ms).min(500) as f32;
        if previous_xy == xy {
            let decay = (1.0 - elapsed_ms / (WALK_CYCLE_MS / 4.0)).clamp(0.0, 1.0);
            self.lift *= decay;
            if self.lift == 0.0 {
                self.phase = 0.0;
            }
            return;
        }
        // shortcut: a sine matches the observed main component of the walk;
        // the native camera track and its response to movement speed are open.
        self.phase = (self.phase + elapsed_ms / WALK_CYCLE_MS).fract();
        self.lift = CAMERA_LIFT * (self.phase * std::f32::consts::TAU).sin();
    }
}

pub(super) fn advance(mission: &mut LoadedMissionState, world: &World, now: u32) {
    let pose = mission
        .local_hero
        .filter(|_| mission.boarding.is_none())
        .and_then(|hero| transform_state(world, hero).ok());
    if let Some(pose) = pose {
        let [x, y, _] = pose.position;
        mission.player_gait.advance(now, [x, y]);
    } else {
        mission.player_gait = PlayerGait::default();
    }
}

/// Vertical movement of the local hero's camera at the latest simulation tick.
pub fn player_walk_camera_lift(engine: &Engine, cockpit: StartingCockpit) -> f32 {
    engine
        .loaded
        .as_ref()
        .filter(|mission| mission.local_hero == Some(cockpit.handle) && mission.boarding.is_none())
        .map_or(0.0, |mission| mission.player_gait.lift)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_oscillates_only_after_walk_displaces_the_hero() {
        let mut gait = PlayerGait::default();
        gait.advance(0, [0, 0]);
        gait.advance(112, [0, 0]);
        assert_eq!(gait.lift, 0.0);
        gait.advance(224, [1, 0]);
        assert_eq!(gait.lift, CAMERA_LIFT);
        gait.advance(448, [3, 0]);
        assert_eq!(gait.lift, -CAMERA_LIFT);
        gait.advance(504, [3, 0]);
        assert_eq!(gait.lift, -CAMERA_LIFT * 0.5);
        gait.advance(616, [3, 0]);
        assert_eq!(gait.lift, 0.0);
    }
}
