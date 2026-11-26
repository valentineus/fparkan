use super::{native_control_blend, PreparedComponentControl, PreparedControlTimelineRow};

/// Returns the vertical timeline named by the first Control turret record.
#[must_use]
pub fn turret_elevation_row(
    control: &PreparedComponentControl,
) -> Option<&PreparedControlTimelineRow> {
    let index = control
        .typed_records
        .iter()
        .find(|record| record.tag == 1)?
        .values
        .get(1)?;
    control.timeline_rows.get(*index as usize)
}

/// Samples the Control turret's vertical row and rows linked to its elevation.
///
/// The input is a signed angle from the authored neutral pose. The caller
/// supplies the current component's sampled node times so unrelated tracks
/// retain their state.
pub fn apply_manual_turret_elevation(
    control: &PreparedComponentControl,
    radians: f32,
    node_times: &mut [Option<f32>],
) {
    let Some(elevation) = turret_elevation_row(control) else {
        return;
    };
    let span = f32::from_bits(elevation.raw_parameters[3]);
    if radians == 0.0 || !radians.is_finite() || !span.is_finite() || span <= 0.0 {
        return;
    }
    // shortcut: share this blend with linked tracks until weapon-specific
    // corrections are measured.
    let blend = (elevation.initial_blend + radians / span).clamp(0.0, 1.0);
    for row in &control.timeline_rows {
        let linked = row.flags & (0x40 | 0x2000 | 0x08) == 0x08;
        if row.flags & 4 != 0 || !(row.row_index == elevation.row_index || linked) {
            continue;
        }
        let Some(sample) = node_times.get_mut(row.node_index) else {
            continue;
        };
        let blend = native_control_blend(blend, row.flags);
        *sample = Some((1.0 - blend) * row.frame_a + blend * row.frame_b);
    }
}
