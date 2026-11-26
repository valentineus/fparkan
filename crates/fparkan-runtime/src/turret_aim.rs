//! Aim a mount through the azimuth and elevation tracks supplied by its asset.

use crate::{Engine, ObjectHandle, TransformState};
use fparkan_assets::{
    apply_manual_turret_elevation, PreparedComponentControl, PreparedControlTimelineRow,
};
use fparkan_render::LegacyIron3dEulerTransform;

/// An autonomous turret's selected direction in its mount's coordinates.
#[derive(Clone, Copy, Debug)]
pub struct TurretAim {
    azimuth: f32,
    elevation: f32,
}

impl TurretAim {
    pub(super) fn toward([x, y, z]: [f32; 3]) -> Option<Self> {
        let horizontal = x.hypot(y);
        if !horizontal.is_finite() || !z.is_finite() || horizontal.hypot(z) <= f32::EPSILON {
            return None;
        }
        Some(Self {
            azimuth: (-x).atan2(y),
            elevation: z.atan2(horizontal),
        })
    }
}

/// Last aim used by the weapon sampler for a live autonomous component.
pub fn autonomous_turret_aim(
    engine: &Engine,
    handle: ObjectHandle,
    component: usize,
) -> Option<TurretAim> {
    engine
        .autodemo_combat
        .as_ref()?
        .turret_aim(handle, component)
}

fn azimuth_row(control: &PreparedComponentControl) -> Option<&PreparedControlTimelineRow> {
    let row = control
        .typed_records
        .iter()
        .find(|record| record.tag == 1)?
        .values
        .first()?;
    let row = control.timeline_rows.get(*row as usize)?;
    let span = f32::from_bits(row.raw_parameters[3]);
    (row.flags & 4 == 0 && span.is_finite() && span > 0.0).then_some(row)
}

/// Whether a component supplies a usable turret azimuth track.
pub fn has_turret_aim(control: &PreparedComponentControl) -> bool {
    azimuth_row(control).is_some()
}

/// Sample the same tracks for the rendered turret and the CPU muzzle pose.
pub fn apply_autonomous_turret_pose(
    control: &PreparedComponentControl,
    aim: TurretAim,
    node_times: &mut [Option<f32>],
) {
    let Some(row) = azimuth_row(control) else {
        return;
    };
    let blend = row.initial_blend + aim.azimuth / f32::from_bits(row.raw_parameters[3]);
    if let Some(time) = node_times.get_mut(row.node_index) {
        let mut blend = if row.flags & 1 != 0 {
            blend.rem_euclid(1.0)
        } else {
            blend.clamp(0.0, 1.0)
        };
        if row.flags & 2 != 0 {
            blend = 1.0 - blend;
        }
        let sample = (1.0 - blend) * row.frame_a + blend * row.frame_b;
        if sample.is_finite() && sample >= 0.0 {
            *time = Some(sample);
        }
    }
    apply_manual_turret_elevation(control, aim.elevation, node_times);
}

pub(super) fn hull_local_point(state: TransformState, target: [f32; 3]) -> Option<[f32; 3]> {
    let translation = state.position.map(f32::from_bits);
    let matrix = LegacyIron3dEulerTransform {
        translation,
        orientation_radians: state.orientation.map(f32::from_bits),
    }
    .try_row_major()?;
    let [dx, dy, dz] = std::array::from_fn(|i| target[i] - translation[i]);
    let scale = state.scale.map(f32::from_bits);
    let point = std::array::from_fn(|i| {
        (matrix[i] * dx + matrix[4 + i] * dy + matrix[8 + i] * dz) / scale[i]
    });
    point.iter().all(|value| value.is_finite()).then_some(point)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::{FRAC_PI_2, FRAC_PI_4};

    #[test]
    fn target_uses_hull_rotation_and_scale_before_computing_aim() {
        let hull = TransformState {
            position: [20.0, 30.0, 40.0].map(f32::to_bits),
            orientation: [0.0, 0.0, FRAC_PI_2].map(f32::to_bits),
            scale: [2.0, 3.0, 4.0].map(f32::to_bits),
        };
        let local_target = hull_local_point(hull, [20.0, 32.0, 44.0]).unwrap();
        let aim = TurretAim::toward(local_target).unwrap();
        assert!((aim.azimuth + FRAC_PI_2).abs() < 1e-5);
        assert!((aim.elevation - FRAC_PI_4).abs() < 1e-5);
        assert!(TurretAim::toward([0.0; 3]).is_none());
    }
}
