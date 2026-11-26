//! Camera motion and footfalls of the walking hero.

use crate::{Engine, LoadedMissionState, StartingCockpit};
use fparkan_world::{transform_state, World};

const WALK_CYCLE_MS: f32 = 448.0;
const CAMERA_LIFT: f32 = 0.15;

#[derive(Default)]
pub(super) struct PlayerGait {
    previous: Option<(u32, [u32; 2])>,
    phase: f32,
    lift: f32,
    pub(super) footfall: bool,
}

impl PlayerGait {
    fn advance(&mut self, clock_ms: u32, xy: [u32; 2]) {
        self.footfall = false;
        let Some((previous_ms, previous_xy)) = self.previous.replace((clock_ms, xy)) else {
            return;
        };
        let elapsed_ms = clock_ms.wrapping_sub(previous_ms).min(500) as f32;
        if previous_xy == xy {
            let decay = CAMERA_LIFT * elapsed_ms / (WALK_CYCLE_MS / 4.0);
            self.lift = self.lift.signum() * (self.lift.abs() - decay).max(0.0);
            if self.lift == 0.0 {
                self.phase = 0.0;
            }
            return;
        }
        // shortcut: a sine matches the observed main component of the walk;
        // the native camera track and its response to movement speed are open.
        let next = self.phase + elapsed_ms / WALK_CYCLE_MS;
        self.footfall = (next - 0.75).floor() > (self.phase - 0.75).floor();
        self.phase = next.fract();
        self.lift = CAMERA_LIFT * (self.phase * std::f32::consts::TAU).sin();
    }
}

/// A foot contacting the accepted surface during the latest walking step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlayerFootstep {
    /// Position of the walker at ground contact, as IEEE-754 words.
    pub position_bits: [u32; 3],
    /// Whether building geometry supports the walker.
    pub building_floor: bool,
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
        assert!(!gait.footfall);
        gait.advance(448, [3, 0]);
        assert_eq!(gait.lift, -CAMERA_LIFT);
        assert!(gait.footfall);
        gait.advance(504, [3, 0]);
        assert_eq!(gait.lift, -CAMERA_LIFT * 0.5);
        assert!(!gait.footfall);
        gait.advance(616, [3, 0]);
        assert_eq!(gait.lift, 0.0);
    }
}
