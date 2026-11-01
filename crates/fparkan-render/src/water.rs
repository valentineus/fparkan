//! The GOG Terrain EMBM wave map and its world-space animation clock.

/// Side length of the native signed Du/Dv texture.
pub const EMBM_BUMP_SIZE: u32 = 128;
/// Native default wave tiling across the bounding rectangle of reflective faces.
pub const EMBM_BUMP_TILE: f32 = 100.0;
/// Native diagonal bump matrix, without luminance modulation.
pub const EMBM_COEFFICIENTS: [f32; 2] = [0.01, 0.01];

/// Generates the signed bytes written by Terrain.dll RVA 0x2BF50 with CW 0x007F.
#[must_use]
pub fn legacy_embm_bump_map() -> Vec<[i8; 2]> {
    let mut texels = Vec::with_capacity((EMBM_BUMP_SIZE * EMBM_BUMP_SIZE) as usize);
    for y in 0..EMBM_BUMP_SIZE {
        for x in 0..EMBM_BUMP_SIZE {
            // The native constant is slightly smaller than mathematical 4*pi.
            let angle = ((y as f32 / EMBM_BUMP_SIZE as f32 - 0.5)
                + (x as f32 / EMBM_BUMP_SIZE as f32 - 0.5))
                * f32::from_bits(0x4149_0fd0);
            texels.push([
                (angle.cos() * 64.0).round_ties_even() as i8,
                (angle.sin() * 64.0).round_ties_even() as i8,
            ]);
        }
    }
    texels
}

/// Queue clock modulo the native ten-second wave scroll period.
#[must_use]
pub fn legacy_embm_phase(clock_ms: u32) -> f32 {
    (clock_ms % 10_000) as f32 / 10_000.0
}

/// Native reflection coordinates: U reverses world Y and V reverses world X.
#[must_use]
pub fn legacy_embm_uv(world_xy: [f32; 2], bounds: [f32; 4]) -> Option<[f32; 2]> {
    let span = [bounds[2] - bounds[0], bounds[3] - bounds[1]];
    if !world_xy
        .into_iter()
        .chain(bounds)
        .chain(span)
        .all(f32::is_finite)
        || span.iter().any(|&value| value <= 0.0)
    {
        return None;
    }
    Some([
        1.0 - (world_xy[1] - bounds[1]) / span[1],
        1.0 - (world_xy[0] - bounds[0]) / span[0],
    ])
}

/// The EMBM camera looks up from the mirrored eye, with its window on the water rectangle.
/// Terrain.dll+4EFF0 supplies `[min_y-eye_y, min_x-eye_x, span_y, span_x, eye_z-height]`.
/// The ordinary perspective's depth mapping is retained; orthographic inputs use the caller's fallback.
#[must_use]
pub fn legacy_embm_reflection_clip(
    main_clip: [f32; 16],
    eye: [f32; 3],
    height: f32,
    bounds: [f32; 4],
) -> Option<[f32; 16]> {
    let span = [bounds[2] - bounds[0], bounds[3] - bounds[1]];
    let distance = eye[2] - height;
    if !main_clip
        .into_iter()
        .chain(eye)
        .chain([height])
        .chain(bounds)
        .chain(span)
        .all(f32::is_finite)
        || distance <= 0.0
        || span.into_iter().any(|v| v <= 0.0)
    {
        return None;
    }
    let w = [main_clip[3], main_clip[7], main_clip[11]];
    let squared = w.into_iter().map(|v| v * v).sum::<f32>();
    if !squared.is_finite() || squared <= f32::EPSILON {
        return None;
    }
    let depth_a = (main_clip[2] * w[0] + main_clip[6] * w[1] + main_clip[10] * w[2]) / squared;
    // shortcut: retain the main depth range until the native shifted depth matrix is verified.
    let depth_b = (main_clip[14] - depth_a * main_clip[15]) / squared.sqrt();
    if !depth_a.is_finite() || !depth_b.is_finite() || depth_a < 1.0 || depth_b >= 0.0 {
        return None;
    }
    let z = 2.0 * height - eye[2];
    let x_scale = -2.0 * distance / span[1];
    let y_scale = -2.0 * distance / span[0];
    let x_shift = 1.0 + 2.0 * (bounds[1] - eye[1]) / span[1];
    let y_shift = 1.0 + 2.0 * (bounds[0] - eye[0]) / span[0];
    let matrix = [
        0.0,
        y_scale,
        0.0,
        0.0,
        x_scale,
        0.0,
        0.0,
        0.0,
        x_shift,
        y_shift,
        depth_a,
        1.0,
        -x_scale * eye[1] - x_shift * z,
        -y_scale * eye[0] - y_shift * z,
        depth_b - depth_a * z,
        -z,
    ];
    matrix.into_iter().all(f32::is_finite).then_some(matrix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shifted_camera_maps_native_water_corners_and_retains_perspective_depth() {
        let projection = crate::LegacyD3d7Projection {
            viewport: [0, 0, 1280, 1024],
            near_plane: 0.5,
            far_plane: 700.0,
            field_of_view_radians: 1.3,
        }
        .try_direct3d7_projection_row_major()
        .unwrap();
        let eye = [1200.0, 800.0, 185.0];
        let mut main = [0.0; 16];
        main[2] = projection[10];
        main[3] = projection[11];
        main[14] = -eye[0] * projection[10] + projection[14];
        main[15] = -eye[0] * projection[11];
        let bounds = [1000.0, 500.0, 1500.0, 1200.0];
        let matrix = legacy_embm_reflection_clip(main, eye, 150.0, bounds).unwrap();
        let project = |point: [f32; 3]| -> [f32; 4] {
            std::array::from_fn(|column| {
                point[0] * matrix[column]
                    + point[1] * matrix[4 + column]
                    + point[2] * matrix[8 + column]
                    + matrix[12 + column]
            })
        };
        for point in [
            [1000.0, 500.0, 150.0],
            [1500.0, 1200.0, 150.0],
            [1125.0, 1025.0, 150.0],
        ] {
            let clip = project(point);
            let expected = legacy_embm_uv([point[0], point[1]], bounds).unwrap();
            for axis in 0..2 {
                assert!((clip[axis] / clip[3] * 0.5 + 0.5 - expected[axis]).abs() < 0.00001);
            }
            assert_eq!(clip[3], 35.0);
        }
        let near = project([1200.0, 800.0, 115.5]);
        assert!((near[2] / near[3]).abs() < 0.001);
        let far = project([1200.0, 800.0, 815.0]);
        assert!((far[2] / far[3] - 1.0).abs() < 0.00001);
        assert!(legacy_embm_reflection_clip([0.0; 16], eye, 150.0, bounds).is_none());
        assert!(legacy_embm_reflection_clip(main, eye, 185.0, bounds).is_none());
        assert!(legacy_embm_reflection_clip(main, eye, 150.0, [1.0; 4]).is_none());
    }

    #[test]
    fn wave_bytes_match_native_generation_and_clock_wraps_without_float_time_loss() {
        let texels = legacy_embm_bump_map();
        assert_eq!(texels.len(), 128 * 128);
        assert_eq!(
            &texels[..16],
            &[
                [64, 0],
                [64, 6],
                [63, 12],
                [61, 19],
                [59, 24],
                [56, 30],
                [53, 36],
                [49, 41],
                [45, 45],
                [41, 49],
                [36, 53],
                [30, 56],
                [24, 59],
                [19, 61],
                [12, 63],
                [6, 64],
            ]
        );
        assert_eq!(texels[16], [0, 64]);
        assert_eq!(texels[32], [-64, 0]);
        assert_eq!(texels[48], [0, -64]);
        let native_hash = texels
            .iter()
            .flatten()
            .fold(0xcbf29ce484222325u64, |hash, &byte| {
                (hash ^ u64::from(byte as u8)).wrapping_mul(0x100000001b3)
            });
        assert_eq!(native_hash, 0x0b92dc6a9e482625);
        assert_eq!(legacy_embm_phase(0), 0.0);
        assert_eq!(legacy_embm_phase(5_000), 0.5);
        assert_eq!(legacy_embm_phase(10_000), 0.0);
        assert_eq!(legacy_embm_phase(u32::MAX), 0.7295);
    }

    #[test]
    fn mapping_uses_the_reflective_rectangle_and_keeps_axis_order() {
        let bounds = [100.0, 200.0, 300.0, 600.0];
        assert_eq!(legacy_embm_uv([100.0, 200.0], bounds), Some([1.0, 1.0]));
        assert_eq!(legacy_embm_uv([300.0, 600.0], bounds), Some([0.0, 0.0]));
        assert_eq!(legacy_embm_uv([150.0, 500.0], bounds), Some([0.25, 0.75]));
        assert_eq!(legacy_embm_uv([0.0; 2], [1.0; 4]), None);
        assert_eq!(legacy_embm_uv([f32::NAN, 0.0], bounds), None);
    }
}
