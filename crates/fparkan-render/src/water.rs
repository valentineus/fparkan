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

#[cfg(test)]
mod tests {
    use super::*;

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
