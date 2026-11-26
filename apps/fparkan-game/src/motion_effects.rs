//! Observe chassis motion for effects driven by owner velocity.

use super::{live_root_pose, MissionRenderRoot};
use fparkan_fx::clock::FxPhaseProvider;
use fparkan_runtime::{loaded_mission_control_device_tables, Engine};
use fparkan_world::{TransformState, WorldSnapshot};
use std::collections::HashMap;
use std::f32::consts::{PI, TAU};

#[derive(Default)]
pub(super) struct MotionEffects {
    sampled_ms: Option<u32>,
    owners: HashMap<usize, FxPhaseProvider>,
}

impl MotionEffects {
    pub(super) fn sample(
        &mut self,
        now_ms: u32,
        roots: &[Option<MissionRenderRoot>],
        previous: Option<&WorldSnapshot>,
        current: &WorldSnapshot,
        engine: &Engine,
    ) {
        let previous_ms = self.sampled_ms.replace(now_ms);
        if previous_ms == Some(now_ms) {
            return;
        }
        self.owners.clear();
        let Some(tables) = loaded_mission_control_device_tables(engine) else {
            return;
        };
        let seconds = previous_ms
            .map(|before| now_ms.saturating_sub(before) as f32 * 0.001)
            .unwrap_or(0.0);
        for (index, root) in roots.iter().enumerate() {
            let Some(root) = root else { continue };
            let Some(current) = live_root_pose(root.handle, current) else {
                continue;
            };
            let Some(profile) = tables
                .iter()
                .find(|table| table.handle == root.handle)
                .and_then(|table| table.owner_profile.as_ref())
            else {
                continue;
            };
            let previous = previous.and_then(|snapshot| live_root_pose(root.handle, snapshot));
            if let Some(values) =
                motion_provider(previous, current, &profile.current_words, seconds)
            {
                self.owners.insert(index, values);
            }
        }
    }

    pub(super) fn provider(&self, index: usize) -> Option<FxPhaseProvider> {
        self.owners.get(&index).copied()
    }
}

fn motion_provider(
    previous: Option<TransformState>,
    current: TransformState,
    profile: &[u32; 27],
    seconds: f32,
) -> Option<FxPhaseProvider> {
    let linear_velocity_limit = std::array::from_fn(|i| f32::from_bits(profile[6 + i]));
    let angular_velocity_limit = std::array::from_fn(|i| f32::from_bits(profile[9 + i]));
    let mut provider = FxPhaseProvider {
        linear_velocity_limit,
        angular_velocity_limit,
        ..FxPhaseProvider::idle()
    };
    if let Some(previous) = previous.filter(|_| seconds > 0.0) {
        let distance =
            |i| f32::from_bits(current.position[i]) - f32::from_bits(previous.position[i]);
        let angle =
            |i| f32::from_bits(current.orientation[i]) - f32::from_bits(previous.orientation[i]);
        provider.linear_velocity = std::array::from_fn(|i| distance(i) / seconds);
        provider.angular_velocity =
            std::array::from_fn(|i| ((angle(i) + PI).rem_euclid(TAU) - PI) / seconds);
    }
    provider
        .linear_velocity
        .into_iter()
        .chain(provider.angular_velocity)
        .chain(linear_velocity_limit)
        .chain(angular_velocity_limit)
        .all(f32::is_finite)
        .then_some(provider)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_fx::clock::FxInstanceClock;

    #[test]
    fn moving_and_turning_drive_the_engine_effect_and_stop_at_rest() {
        let before = TransformState {
            orientation: [0.0, 0.0, PI - 0.1].map(f32::to_bits),
            ..TransformState::default()
        };
        let after = TransformState {
            position: [0.0_f32, 1.0, 0.0].map(f32::to_bits),
            orientation: [0.0, 0.0, -PI + 0.1].map(f32::to_bits),
            ..before
        };
        let mut profile = [0; 27];
        profile[7] = 20.0_f32.to_bits();
        profile[11] = 2.0_f32.to_bits();
        let moving = motion_provider(Some(before), after, &profile, 0.25).unwrap();
        let stopped = motion_provider(Some(after), after, &profile, 0.25).unwrap();
        let new_actor = motion_provider(None, after, &profile, 0.25).unwrap();
        let mut effect = FxInstanceClock::new(15, 0, 0.0, 0.0).unwrap();
        let phase = effect.phase(250.0, false, Some(&moving)).unwrap();
        assert!((phase - 0.4).abs() < 0.0001);
        assert_eq!(effect.phase(500.0, false, Some(&stopped)), Some(0.0));
        assert_eq!(effect.phase(750.0, false, Some(&new_actor)), Some(0.0));
    }
}
