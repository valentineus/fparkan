//! Geometry for the swept, two-position layers of an FXID Op5 trail.

use crate::{FxCommand, FxOpcode};

/// One constant-width trail layer, as used by the training lasers.
#[derive(Clone, Copy, Debug)]
pub struct TrailLayer {
    pub width: f32,
    pub texture_length: f32,
}

impl TrailLayer {
    /// Decodes the constant-width, full-phase form of the command.
    pub fn from_command(command: &FxCommand, uniform_scale: f32) -> Option<Self> {
        let read = |offset| {
            command
                .raw_body
                .get(offset..offset + 4)
                .and_then(|bytes| bytes.try_into().ok())
                .map(f32::from_le_bytes)
        };
        if command.opcode != FxOpcode::Op5
            || command.native_flag
            || read(0)? != 1.0
            || read(4)? != 0.0
            || read(8)? != 0.0
            || read(12)? != 1.0
            || read(20)? != read(24)?
            || read(36)? != -1.0
        {
            return None;
        }
        let width = read(20)? * uniform_scale;
        let texture_length = read(28)?;
        (width.is_finite() && width > 0.0 && texture_length.is_finite() && texture_length > 0.0)
            .then_some(Self {
                width,
                texture_length,
            })
    }

    /// Builds a ribbon between the two sampled positions in the camera plane.
    pub fn ribbon(
        self,
        [start, end]: [[f32; 3]; 2],
        camera_forward: [f32; 3],
    ) -> Option<([[f32; 3]; 4], f32)> {
        let [x, y, z]: [f32; 3] = std::array::from_fn(|i| end[i] - start[i]);
        let [cx, cy, cz] = camera_forward;
        let side = [cy * z - cz * y, cz * x - cx * z, cx * y - cy * x];
        let side_length = side[0].hypot(side[1]).hypot(side[2]);
        let length = x.hypot(y).hypot(z);
        if !side_length.is_finite() || side_length <= f32::EPSILON || !length.is_finite() {
            return None;
        }
        let offset = side.map(|value| value * (0.5 * self.width / side_length));
        Some((
            [
                std::array::from_fn(|i| start[i] - offset[i]),
                std::array::from_fn(|i| start[i] + offset[i]),
                std::array::from_fn(|i| end[i] - offset[i]),
                std::array::from_fn(|i| end[i] + offset[i]),
            ],
            length / self.texture_length,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::TrailLayer;

    #[test]
    fn ribbon_keeps_the_sampled_endpoints_and_width() {
        let layer = TrailLayer {
            width: 0.5,
            texture_length: 10.0,
        };
        let (points, repeats) = layer
            .ribbon([[0.0; 3], [20.0, 0.0, 0.0]], [0.0, 1.0, 0.0])
            .unwrap();
        assert_eq!(
            points,
            [
                [0.0, 0.0, 0.25],
                [0.0, 0.0, -0.25],
                [20.0, 0.0, 0.25],
                [20.0, 0.0, -0.25]
            ]
        );
        assert_eq!(repeats, 2.0);
        assert!(layer.ribbon([[0.0; 3]; 2], [1.0, 0.0, 0.0]).is_none());
    }
}
