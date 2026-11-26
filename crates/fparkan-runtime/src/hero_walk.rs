//! Translational velocity and slope braking for the walking mode of Control.

use crate::manual_flight::ramp_axis;

#[derive(Clone, Copy)]
pub(super) struct WalkingDrive {
    pub(super) velocity: [f32; 2],
    pub(super) limits: [f32; 2],
    acceleration: [f32; 2],
    slope: WalkingSlope,
}

impl WalkingDrive {
    pub(super) fn from_profile(words: &[u32; 27], velocity: [f32; 2]) -> Option<Self> {
        let slope = WalkingSlope::from_profile(words)?;
        let acceleration = [0, 1].map(|word| f32::from_bits(words[word]));
        let limits = [6, 7].map(|word| f32::from_bits(words[word]));
        acceleration
            .into_iter()
            .chain(limits)
            .all(|rate| rate.is_finite() && rate >= 0.0)
            .then(|| Self {
                velocity: std::array::from_fn(|axis| {
                    velocity[axis].clamp(-limits[axis], limits[axis])
                }),
                limits,
                acceleration,
                slope,
            })
    }

    pub(super) fn advance(self, axes: [f32; 2], seconds: f32) -> ([f32; 2], [f32; 2]) {
        let [(vx, across), (vy, along)] = std::array::from_fn(|axis| {
            ramp_axis(
                self.velocity[axis],
                axes[axis].clamp(-1.0, 1.0) * self.limits[axis],
                self.acceleration[axis],
                seconds,
            )
        });
        ([vx, vy], [across, along])
    }

    pub(super) fn brake(
        self,
        velocity: [f32; 2],
        normal: [f32; 3],
        yaw: f32,
        seconds: f32,
    ) -> Option<([f32; 2], [f32; 2])> {
        let [across, along] = velocity;
        let (sin, cos) = yaw.sin_cos();
        let world_velocity = [cos * across - sin * along, sin * across + cos * along];
        let factor = self.slope.speed_factor(normal, world_velocity);
        if factor == 1.0 {
            return None;
        }
        let velocity = std::array::from_fn(|axis| {
            ramp_axis(
                velocity[axis],
                velocity[axis] * factor,
                self.acceleration[axis] * 1.5,
                seconds,
            )
            .0
        });
        // The scene uses short geometric sweeps. Bridge the surviving speeds
        // at the ends of that sweep while the native contact solver is open.
        let distance = if factor == 0.0 {
            // The surface cannot carry the hero uphill at this inclination.
            // Keep braking the stored velocity while the ground blocks travel.
            [0.0; 2]
        } else {
            std::array::from_fn(|axis| (self.velocity[axis] + velocity[axis]) * (0.5 * seconds))
        };
        Some((velocity, distance))
    }
}

#[derive(Clone, Copy)]
struct WalkingSlope {
    minimum_up: f32,
}

impl WalkingSlope {
    fn from_profile(words: &[u32; 27]) -> Option<Self> {
        let angle = f32::from_bits(words[23]);
        let minimum_up = angle.cos();
        (words[21] == 2 && angle > 0.0 && angle <= std::f32::consts::FRAC_PI_2 && minimum_up < 1.0)
            .then_some(Self { minimum_up })
    }

    fn speed_factor(self, normal: [f32; 3], direction: [f32; 2]) -> f32 {
        // Control+0x153c0 applies the limit on an ascent and along the contour.
        let along_normal = direction[0] * normal[0] + direction[1] * normal[1];
        if along_normal > 0.0 {
            return 1.0;
        }
        (2.0 * (normal[2] - self.minimum_up) / (1.0 - self.minimum_up)).clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hero_profile_limits_steep_ascent_and_contour() {
        let mut words = [0; 27];
        words[21] = 2;
        words[23] = 0.6_f32.to_bits();
        let slope = WalkingSlope::from_profile(&words).unwrap();
        let up_normal = |angle: f32| [0.0, -angle.sin(), angle.cos()];

        assert_eq!(slope.speed_factor(up_normal(0.3), [0.0, 1.0]), 1.0);
        let uphill = slope.speed_factor(up_normal(0.5), [0.0, 1.0]);
        assert!((14.0 * uphill - 8.375_571).abs() < 0.000_01);
        assert_eq!(slope.speed_factor(up_normal(0.7), [0.0, 1.0]), 0.0);
        assert_eq!(slope.speed_factor(up_normal(0.7), [1.0, 0.0]), 0.0);
        assert_eq!(slope.speed_factor(up_normal(0.7), [0.0, -1.0]), 1.0);
    }

    #[test]
    fn profile_walk_keeps_speed_across_contact_and_input_release() {
        let mut words = [0; 27];
        words[0] = 180.0_f32.to_bits();
        words[1] = 140.0_f32.to_bits();
        words[6] = 1.0_f32.to_bits();
        words[7] = 14.0_f32.to_bits();
        words[21] = 2;
        words[23] = 0.6_f32.to_bits();
        let mut drive = WalkingDrive::from_profile(&words, [0.0; 2]).unwrap();
        let (sin, cos) = 0.7_f32.sin_cos();
        let normal = [0.0, -sin, cos];
        for (axes, seconds, steep, expected) in [
            ([1.0, 1.0], 0.02, false, [1.0, 2.8]),
            ([1.0, 1.0], 0.02, false, [1.0, 5.6]),
            ([0.0, 0.0], 0.02, false, [0.0, 2.8]),
            ([0.0, 0.0], 0.02, false, [0.0, 0.0]),
            ([0.0, 1.0], 0.10, false, [0.0, 14.0]),
            ([0.0, 1.0], 0.02, true, [0.0, 9.8]),
            ([0.0, 1.0], 0.02, true, [0.0, 8.4]),
            ([0.0, 0.0], 0.02, true, [0.0, 1.4]),
            ([0.0, 0.0], 0.02, true, [0.0, 0.0]),
            ([0.0, 1.0], 0.02, true, [0.0, 0.0]),
            ([0.0, -1.0], 0.02, true, [0.0, -2.8]),
        ] {
            let (mut velocity, _) = drive.advance(axes, seconds);
            if steep {
                if let Some((braked, _)) = drive.brake(velocity, normal, 0.0, seconds) {
                    velocity = braked;
                }
            }
            assert!(
                velocity
                    .into_iter()
                    .zip(expected)
                    .all(|(v, expected)| (v - expected).abs() < 0.000_01),
                "{velocity:?} expected {expected:?}"
            );
            drive.velocity = velocity;
        }
    }
}
