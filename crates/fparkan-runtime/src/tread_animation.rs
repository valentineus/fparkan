//! Advance a track's authored material cycle from actual hull travel.

use crate::{ObjectHandle, TransformState};

/// Playback position independent of the scene material clock.
#[derive(Clone)]
pub struct TreadAnimation {
    reference_speed: f32,
    travel_ms: f64,
    previous: Option<(ObjectHandle, [f32; 2])>,
}

impl TreadAnimation {
    /// Uses the chassis's unloaded Control forward limit as its nominal rate.
    pub fn new(reference_speed: f32) -> Option<Self> {
        (reference_speed.is_finite() && reference_speed > 0.0).then_some(Self {
            reference_speed,
            travel_ms: 0.0,
            previous: None,
        })
    }

    /// Integrates signed translation along the hull. A retained position
    /// leaves the phase unchanged, and a replacement starts a new cycle.
    pub fn sample(&mut self, handle: ObjectHandle, pose: TransformState) {
        let [x, y, _] = pose.position.map(f32::from_bits);
        match self.previous.replace((handle, [x, y])) {
            Some((owner, [old_x, old_y])) if owner == handle => {
                let yaw = f32::from_bits(pose.orientation[2]);
                let forward = (old_x - x) * yaw.sin() + (y - old_y) * yaw.cos();
                let speed = f32::from_bits(pose.scale[1]).abs() * self.reference_speed;
                let travel_ms = f64::from(forward) * 1_000.0 / f64::from(speed);
                if travel_ms.is_finite() {
                    self.travel_ms += travel_ms;
                }
            }
            _ => self.travel_ms = 0.0,
        }
    }

    /// Maps the current travel to the requested nonzero MAT0 cycle duration.
    pub fn phase(&self, period_ms: std::num::NonZeroU16) -> f32 {
        let period = f64::from(period_ms.get());
        (self.travel_ms.rem_euclid(period) / period) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn material_phase_uses_signed_travel_and_stops_with_hull() {
        let mut animation = TreadAnimation::new(10.0).unwrap();
        let period = std::num::NonZeroU16::new(200).unwrap();
        let handle = ObjectHandle {
            slot: 0,
            generation: 1,
        };
        let pose = |y| TransformState {
            position: [0.0, y, 0.0].map(f32::to_bits),
            scale: [1.0, 2.0, 1.0].map(f32::to_bits),
            ..TransformState::default()
        };
        animation.sample(handle, pose(0.0));
        animation.sample(handle, pose(1.0));
        assert_eq!(animation.phase(period), 0.25);
        animation.sample(handle, pose(1.0));
        assert_eq!(animation.phase(period), 0.25);
        animation.sample(handle, pose(0.0));
        assert_eq!(animation.phase(period), 0.0);
        animation.sample(handle, pose(-1.0));
        assert_eq!(animation.phase(period), 0.75);
        animation.sample(
            ObjectHandle {
                generation: 2,
                ..handle
            },
            pose(-2.0),
        );
        assert_eq!(animation.phase(period), 0.0);
    }
}
