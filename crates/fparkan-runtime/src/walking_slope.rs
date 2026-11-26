//! Ground speed envelope for the walking mode of Control.

#[derive(Clone, Copy)]
pub(super) struct WalkingSlope {
    minimum_up: f32,
}

impl WalkingSlope {
    pub(super) fn from_profile(words: &[u32; 27]) -> Option<Self> {
        let angle = f32::from_bits(words[23]);
        let minimum_up = angle.cos();
        (words[21] == 2 && angle > 0.0 && angle <= std::f32::consts::FRAC_PI_2 && minimum_up < 1.0)
            .then_some(Self { minimum_up })
    }

    pub(super) fn speed_factor(self, normal: [f32; 3], direction: [f32; 2]) -> f32 {
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
}
