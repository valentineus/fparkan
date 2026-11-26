//! Ballistic presentation of a detached mesh assembly.

use fparkan_animation::Pose;

const GRAVITY: f32 = 9.81;

/// A visual fragment travelling from its last attached center.
pub struct DebrisMotion {
    center: [f32; 3],
    outward: [f32; 2],
    launch_speed: f32,
    angular_speed: f32,
    landing_seconds: f32,
}

impl DebrisMotion {
    /// Launches a fragment outward with enough energy to lift it one radius.
    pub fn new(center: [f32; 3], hull: [f32; 3], radius: f32, floor_center: f32) -> Option<Self> {
        if !center
            .into_iter()
            .chain(hull)
            .chain([radius, floor_center])
            .all(f32::is_finite)
            || radius <= 0.0
        {
            return None;
        }
        let [x, y] = [center[0] - hull[0], center[1] - hull[1]];
        let length = x.hypot(y);
        let outward = if length > f32::EPSILON {
            [x / length, y / length]
        } else {
            [0.0, 1.0]
        };
        let launch_speed = (2.0 * GRAVITY * radius).sqrt();
        let height = (center[2] - floor_center).max(0.0);
        let landing_seconds =
            (launch_speed + (launch_speed.powi(2) + 2.0 * GRAVITY * height).sqrt()) / GRAVITY;
        Some(Self {
            center,
            outward,
            launch_speed,
            angular_speed: launch_speed / radius,
            landing_seconds,
        })
    }

    /// Returns the new center and rotation. Rotation and travel stop at landing.
    pub fn pose(&self, elapsed_seconds: f32) -> Pose {
        let time = elapsed_seconds.clamp(0.0, self.landing_seconds);
        let travel = self.launch_speed * time;
        let [x, y] = self.outward;
        let (sin, cos) = (0.5 * self.angular_speed * time).sin_cos();
        Pose {
            translation: [
                self.center[0] + x * travel,
                self.center[1] + y * travel,
                self.center[2] + travel - 0.5 * GRAVITY * time.powi(2),
            ],
            rotation: [y * sin, -x * sin, 0.0, cos],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragment_begins_attached_and_stops_on_landing() {
        let center = [2.0, 5.0, 4.0];
        let motion = DebrisMotion::new(center, [0.0, 5.0, 0.0], 1.0, 1.0).unwrap();
        assert_eq!(
            motion.pose(0.0),
            Pose {
                translation: center,
                ..Pose::default()
            }
        );
        assert!(motion.pose(0.2).translation[0] > center[0]);
        assert!(motion.pose(0.2).translation[2] > center[2]);
        assert_eq!(motion.pose(4.0), motion.pose(6.0));
        assert!((motion.pose(4.0).translation[2] - 1.0).abs() < 0.00001);
    }
}
