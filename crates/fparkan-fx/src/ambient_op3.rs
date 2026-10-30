//! Native-shaped CPU evaluation for Types 0, 1, and 2 carried by opcode 3.
//!
//! The FX body and the command opcode are separate concepts: this evaluator
//! accepts opcode 3 only when its body discriminator matches the selected
//! evaluator. Types 0/1/2 share the position, scale, material-phase, and
//! opacity tracks; Types 1/2 additionally use an animated axis. Their render
//! basis is selected by the native body discriminator.

use crate::environment::EffectResource;
use crate::fx_visual::{
    compose_native_effect_matrix, native_camera_facing_basis, native_d110_basis,
    native_effect_local_point, transform_native_effect_quad, EffectQuadFrame, EFFECT_QUAD_UV,
};
use crate::{FxCommand, FxOpcode};

const TYPE2_BODY_DISCRIMINATOR: u32 = 2;
const TYPE2_ACTIVE_START_OFFSET: usize = 0x1c;
const TYPE2_ACTIVE_END_OFFSET: usize = 0x20;
const TYPE2_PHASE_START_OFFSET: usize = 0x04;
const TYPE2_PHASE_END_OFFSET: usize = 0x08;
const TYPE2_PHASE_EXPONENT_OFFSET: usize = 0x0c;
const TYPE2_OPACITY_START_OFFSET: usize = 0x10;
const TYPE2_OPACITY_END_OFFSET: usize = 0x14;
const TYPE2_OPACITY_EXPONENT_OFFSET: usize = 0x18;
const TYPE2_POSITION_START_OFFSET: usize = 0x24;
const TYPE2_POSITION_END_OFFSET: usize = 0x30;
#[cfg(test)]
const TYPE2_POSITION_EXPONENT_OFFSET: usize = 0x3c;
const TYPE2_AXIS_START_OFFSET: usize = 0x48;
const TYPE2_AXIS_END_OFFSET: usize = 0x54;
const TYPE2_SCALE_START_OFFSET: usize = 0x60;
const TYPE2_SCALE_END_OFFSET: usize = 0x6c;
const TYPE2_RESOURCE_OFFSET: usize = 0x84;
const TYPE2_RESOURCE_BYTES: usize = 64;
const TYPE0_BODY_DISCRIMINATOR: u32 = 0;
const TYPE1_BODY_DISCRIMINATOR: u32 = 1;

/// Per-frame values sampled from the verified Op3 Type0 body tracks.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Op3Type0Sample {
    /// Animated local translation sampled from body+0x24/+0x30 with exponents
    /// at body+0x3c/+0x40/+0x44.
    position: [f32; 3],
    /// Animated scale sampled from body+0x60/+0x6c with reverse-order
    /// exponents at body+0x78/+0x7c/+0x80.
    scale: [f32; 3],
    /// Native command opacity after its authored curve.
    opacity: f32,
    /// Wrapped texture phase computed by the native Op3 time track.
    material_phase: f32,
}

/// Per-frame values sampled from the verified native Op3 Type2 body tracks.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Op3Type2Sample {
    /// Animated local translation used by native helper 0xC5D0.
    position: [f32; 3],
    /// Animated local axis used by native helper 0xD110.
    axis: [f32; 3],
    /// Animated scale applied to the parent matrix basis.
    scale: [f32; 3],
    /// Opacity multiplier sent through native helper 0x9930 and clamped to
    /// `[0, 1]` for the material descriptor.
    opacity: f32,
    /// Wrapped texture phase computed by the native Op3 time track.
    material_phase: f32,
}

/// Per-frame values sampled from the same track layout in a Type 1 body.
type Op3Type1Sample = Op3Type2Sample;

/// Evaluates one FX opcode 3 / Type 0 fixed camera-facing command.
///
/// Type 0 uses the shared animated position and scale tracks but forms its
/// local basis from the camera vector. The current caller supplies the
/// already-composed Control owner transform and header scale.
/// The supported caller path assumes native render-context index 0, which
/// satisfies Effect.dll's shared `context + 0x60 < 8` gate.
#[must_use]
pub fn evaluate_op3_type0_command(
    command: &FxCommand,
    command_index: usize,
    world_from_effect: &[[f32; 4]; 4],
    elapsed_seconds: f32,
    effect_phase: f32,
    camera_world: [f32; 3],
) -> Option<EffectQuadFrame> {
    let body = &command.raw_body;
    if command.opcode != FxOpcode::Op3
        || read_u32(body, 0)? != TYPE0_BODY_DISCRIMINATOR
        || body.len() < TYPE2_RESOURCE_OFFSET + TYPE2_RESOURCE_BYTES
        || !world_from_effect
            .iter()
            .flatten()
            .all(|value| value.is_finite())
        || !elapsed_seconds.is_finite()
        || !effect_phase.is_finite()
        || !camera_world.iter().all(|value| value.is_finite())
    {
        return None;
    }

    let start = read_f32(body, TYPE2_ACTIVE_START_OFFSET)?;
    let end = read_f32(body, TYPE2_ACTIVE_END_OFFSET)?;
    if !start.is_finite() || !end.is_finite() || start > end {
        return None;
    }

    let resource = read_resource(command)?;
    let normalized_time = interval_phase(effect_phase, start, end);
    let sample = sample_op3_type0_body(body, normalized_time, elapsed_seconds)?;
    let camera_local = native_effect_local_point(world_from_effect, camera_world)?;
    let view = sub3(camera_local, sample.position);
    let local_basis = native_camera_facing_basis(view, sample.position)?;
    let matrix = compose_native_effect_matrix(
        world_from_effect,
        &local_basis,
        sample.position,
        sample.scale,
    )?;
    let vertices = transform_native_effect_quad(&matrix)?;

    Some(EffectQuadFrame {
        command_index,
        opcode: FxOpcode::Op3,
        material: resource,
        vertices,
        uv: EFFECT_QUAD_UV,
        color: [1.0, 1.0, 1.0, sample.opacity],
        elapsed_seconds,
        normalized_time,
        material_phase: sample.material_phase,
        active: effect_phase >= start && effect_phase <= end,
        native_flag_base: None,
    })
}

/// Evaluates one FX opcode 3 / Type 2 command using native helper matrix rules.
///
/// `world_from_effect` is the row-major parent descriptor already composed by
/// the caller through the selected CPT row, mission-root transform, resolved
/// owner pose (including a mounted component pose), and FX header scale. The
/// evaluator does not resolve those inputs itself. The matrix is the parent
/// supplied to native helper 0x93D0, not the final quad matrix.
///
/// `effect_phase` is the header-transformed, command-clamped overall effect
/// phase in `[0, 1]`. The command's inclusive phase window is stored at
/// raw-body offsets `0x1c` and `0x20`.
/// `render_context` mirrors the native 0x106C0 gate (`context + 0x60 < 8`).
/// The function returns `None` for another command/body type, malformed
/// resources, non-finite inputs, or a degenerate non-finite transform.
#[must_use]
pub fn evaluate_op3_type2_command(
    command: &FxCommand,
    command_index: usize,
    world_from_effect: &[[f32; 4]; 4],
    elapsed_seconds: f32,
    effect_phase: f32,
    render_context: u32,
) -> Option<EffectQuadFrame> {
    if command.opcode != FxOpcode::Op3
        || read_u32(&command.raw_body, 0)? != TYPE2_BODY_DISCRIMINATOR
        || command.raw_body.len() < TYPE2_RESOURCE_OFFSET + TYPE2_RESOURCE_BYTES
        || !elapsed_seconds.is_finite()
        || !effect_phase.is_finite()
    {
        return None;
    }

    let start = read_f32(&command.raw_body, TYPE2_ACTIVE_START_OFFSET)?;
    let end = read_f32(&command.raw_body, TYPE2_ACTIVE_END_OFFSET)?;
    if !start.is_finite() || !end.is_finite() || start > end {
        return None;
    }

    let resource = read_resource(command)?;
    let normalized_time = interval_phase(effect_phase, start, end);
    let sample = sample_op3_type2_body(&command.raw_body, normalized_time, elapsed_seconds)?;
    let matrix = native_op3_type2_matrix(world_from_effect, sample)?;
    let vertices = transform_native_effect_quad(&matrix)?;

    Some(EffectQuadFrame {
        command_index,
        opcode: FxOpcode::Op3,
        material: resource,
        vertices,
        uv: EFFECT_QUAD_UV,
        color: [1.0, 1.0, 1.0, sample.opacity],
        elapsed_seconds,
        normalized_time,
        material_phase: sample.material_phase,
        active: render_context < 8 && effect_phase >= start && effect_phase <= end,
        native_flag_base: None,
    })
}

/// Evaluates one FX opcode 3 / Type 1 camera-facing command.
///
/// Type 1 uses the native camera-to-position vector and sampled axis to form
/// its quad basis. The command's body tracks use the same offsets as Type 2.
/// The supported caller path assumes native render-context index 0, which
/// satisfies the Effect.dll `context + 0x60 < 8` gate.
#[must_use]
pub fn evaluate_op3_type1_command(
    command: &FxCommand,
    command_index: usize,
    world_from_effect: &[[f32; 4]; 4],
    elapsed_seconds: f32,
    effect_phase: f32,
    camera_world: [f32; 3],
) -> Option<EffectQuadFrame> {
    let body = &command.raw_body;
    if command.opcode != FxOpcode::Op3
        || read_u32(body, 0)? != TYPE1_BODY_DISCRIMINATOR
        || body.len() < TYPE2_RESOURCE_OFFSET + TYPE2_RESOURCE_BYTES
        || !elapsed_seconds.is_finite()
        || !effect_phase.is_finite()
        || !camera_world.iter().all(|value| value.is_finite())
    {
        return None;
    }

    let start = read_f32(body, TYPE2_ACTIVE_START_OFFSET)?;
    let end = read_f32(body, TYPE2_ACTIVE_END_OFFSET)?;
    if !start.is_finite() || !end.is_finite() || start > end {
        return None;
    }

    let resource = read_resource(command)?;
    let normalized_time = interval_phase(effect_phase, start, end);
    let sample = sample_op3_type1_body(body, normalized_time, elapsed_seconds)?;
    let matrix = native_op3_type1_matrix(world_from_effect, sample, camera_world)?;
    let vertices = transform_native_effect_quad(&matrix)?;

    Some(EffectQuadFrame {
        command_index,
        opcode: FxOpcode::Op3,
        material: resource,
        vertices,
        uv: EFFECT_QUAD_UV,
        color: [1.0, 1.0, 1.0, sample.opacity],
        elapsed_seconds,
        normalized_time,
        material_phase: sample.material_phase,
        active: effect_phase >= start && effect_phase <= end,
        native_flag_base: None,
    })
}

/// Samples the verified Type2 body tracks used by native Op3 update/render.
///
/// `normalized_active_time` is the value produced by the inclusive command
/// interval gate. Position and scale use component-wise powers; axis is a
/// linear start/end track. The separate phase track uses elapsed seconds when
/// its raw start is negative, matching native 0x105F0.
#[must_use]
fn sample_op3_type2_body(
    body: &[u8],
    normalized_active_time: f32,
    elapsed_seconds: f32,
) -> Option<Op3Type2Sample> {
    if read_u32(body, 0)? != TYPE2_BODY_DISCRIMINATOR
        || body.len() < TYPE2_RESOURCE_OFFSET + TYPE2_RESOURCE_BYTES
    {
        return None;
    }

    sample_op3_body_tracks(body, normalized_active_time, elapsed_seconds)
}

/// Samples the verified Op3 Type0 position/scale and common command tracks.
#[must_use]
fn sample_op3_type0_body(
    body: &[u8],
    normalized_active_time: f32,
    elapsed_seconds: f32,
) -> Option<Op3Type0Sample> {
    if read_u32(body, 0)? != TYPE0_BODY_DISCRIMINATOR
        || body.len() < TYPE2_RESOURCE_OFFSET + TYPE2_RESOURCE_BYTES
        || !normalized_active_time.is_finite()
        || !elapsed_seconds.is_finite()
    {
        return None;
    }

    // Native 0x106C0 samples the record's position into helper+0x78 from the
    // body position endpoints and exponents. The body tail at +0x78 is the
    // reverse-ordered scale exponent vector, not a fixed position.
    let position = sample_power_vector(
        body,
        TYPE2_POSITION_START_OFFSET,
        TYPE2_POSITION_END_OFFSET,
        [0x3c, 0x40, 0x44],
        normalized_active_time,
    )?;
    let scale = sample_power_vector(
        body,
        TYPE2_SCALE_START_OFFSET,
        TYPE2_SCALE_END_OFFSET,
        [0x80, 0x7c, 0x78],
        normalized_active_time,
    )?;
    let (material_phase, opacity) =
        sample_op3_material_phase_and_opacity(body, normalized_active_time, elapsed_seconds)?;
    Some(Op3Type0Sample {
        position,
        scale,
        opacity,
        material_phase,
    })
}

/// Samples the verified Op3 Type 1 body tracks.
#[must_use]
fn sample_op3_type1_body(
    body: &[u8],
    normalized_active_time: f32,
    elapsed_seconds: f32,
) -> Option<Op3Type1Sample> {
    if read_u32(body, 0)? != TYPE1_BODY_DISCRIMINATOR
        || body.len() < TYPE2_RESOURCE_OFFSET + TYPE2_RESOURCE_BYTES
    {
        return None;
    }

    sample_op3_body_tracks(body, normalized_active_time, elapsed_seconds)
}

fn sample_op3_body_tracks(
    body: &[u8],
    normalized_active_time: f32,
    elapsed_seconds: f32,
) -> Option<Op3Type2Sample> {
    if !normalized_active_time.is_finite() || !elapsed_seconds.is_finite() {
        return None;
    }

    let position = sample_power_vector(
        body,
        TYPE2_POSITION_START_OFFSET,
        TYPE2_POSITION_END_OFFSET,
        [0x3c, 0x40, 0x44],
        normalized_active_time,
    )?;
    let axis = sample_linear_vector(
        body,
        TYPE2_AXIS_START_OFFSET,
        TYPE2_AXIS_END_OFFSET,
        normalized_active_time,
    )?;
    let scale = sample_power_vector(
        body,
        TYPE2_SCALE_START_OFFSET,
        TYPE2_SCALE_END_OFFSET,
        // 0x106C0 loads Z then Y then X from the packed reverse-order
        // exponent vector at body+0x78.
        [0x80, 0x7c, 0x78],
        normalized_active_time,
    )?;

    let (material_phase, opacity) =
        sample_op3_material_phase_and_opacity(body, normalized_active_time, elapsed_seconds)?;
    if !material_phase.is_finite() || !opacity.is_finite() {
        return None;
    }

    Some(Op3Type2Sample {
        position,
        axis,
        scale,
        opacity,
        material_phase,
    })
}

fn sample_op3_material_phase_and_opacity(
    body: &[u8],
    normalized_active_time: f32,
    elapsed_seconds: f32,
) -> Option<(f32, f32)> {
    let raw_phase_start = read_f32(body, TYPE2_PHASE_START_OFFSET)?;
    let phase_end = read_f32(body, TYPE2_PHASE_END_OFFSET)?;
    let phase_exponent = read_f32(body, TYPE2_PHASE_EXPONENT_OFFSET)?;
    let opacity_start = read_f32(body, TYPE2_OPACITY_START_OFFSET)?;
    let opacity_end = read_f32(body, TYPE2_OPACITY_END_OFFSET)?;
    let opacity_exponent = read_f32(body, TYPE2_OPACITY_EXPONENT_OFFSET)?;
    if ![
        raw_phase_start,
        phase_end,
        phase_exponent,
        opacity_start,
        opacity_end,
        opacity_exponent,
        normalized_active_time,
        elapsed_seconds,
    ]
    .iter()
    .all(|value| value.is_finite())
    {
        return None;
    }

    let phase_start = raw_phase_start.max(0.0);
    let phase_delta = phase_end - phase_start;
    let phase_timebase = if raw_phase_start < 0.0 {
        elapsed_seconds
    } else {
        normalized_active_time
    };
    let material_phase = fract(phase_start + phase_delta * phase_timebase.powf(phase_exponent));
    let opacity = (opacity_start
        + (opacity_end - opacity_start) * normalized_active_time.powf(opacity_exponent))
    .clamp(0.0, 1.0);
    (material_phase.is_finite() && opacity.is_finite()).then_some((material_phase, opacity))
}

/// Builds the native Type2 Op3 quad matrix from its parent descriptor and
/// already-sampled animated vectors.
///
/// This matches 0x93D0's order: scale the parent basis by animated scale,
/// multiply by D110's local basis, and then replace the translation with the
/// unscaled parent transform of the animated position.
#[must_use]
fn native_op3_type2_matrix(
    world_from_effect: &[[f32; 4]; 4],
    sample: Op3Type2Sample,
) -> Option<[[f32; 4]; 4]> {
    if !world_from_effect
        .iter()
        .flatten()
        .all(|value| value.is_finite())
        || !sample_is_finite(sample)
    {
        return None;
    }

    let local = native_d110_basis(sample.axis, sample.position)?;
    compose_native_effect_matrix(world_from_effect, &local, sample.position, sample.scale)
}

/// Builds the native Type 1 Op3 quad matrix from the parent transform, sampled
/// tracks, and camera query point.
#[must_use]
fn native_op3_type1_matrix(
    world_from_effect: &[[f32; 4]; 4],
    sample: Op3Type1Sample,
    camera_world: [f32; 3],
) -> Option<[[f32; 4]; 4]> {
    if !world_from_effect
        .iter()
        .flatten()
        .all(|value| value.is_finite())
        || !camera_world.iter().all(|value| value.is_finite())
        || !sample_is_finite(sample)
    {
        return None;
    }

    let camera_local = native_effect_local_point(world_from_effect, camera_world)?;
    let view = sub3(camera_local, sample.position);
    let local_basis = native_op3_type1_basis(view, sample.axis, sample.position)?;
    compose_native_effect_matrix(
        world_from_effect,
        &local_basis,
        sample.position,
        sample.scale,
    )
}

/// Native Effect.dll Type 1 basis. The parallel gate compares the raw
/// `view · axis` value to exactly ±1 before building the cross-product frame;
/// neither input vector is normalized. The native path normalizes only
/// `view × axis`, then computes `axis × U` without normalizing the result.
/// The authored axis is the quad's X column (Effect.dll RVA 0x97F6..0x983C).
#[must_use]
fn native_op3_type1_basis(
    view: [f32; 3],
    axis: [f32; 3],
    position: [f32; 3],
) -> Option<[[f32; 4]; 4]> {
    if !view
        .iter()
        .chain(axis.iter())
        .chain(position.iter())
        .all(|value| value.is_finite())
    {
        return None;
    }

    let dot = view[0] * axis[0] + view[1] * axis[1] + view[2] * axis[2];
    if !dot.is_finite() || dot.abs() == 1.0 {
        return None;
    }

    let mut u = cross3(view, axis);
    let length = length3(u)?;
    if length == 0.0 {
        return None;
    }
    for value in &mut u {
        *value /= length;
    }
    let w = cross3(axis, u);
    let result = [
        [axis[0], u[0], w[0], position[0]],
        [axis[1], u[1], w[1], position[1]],
        [axis[2], u[2], w[2], position[2]],
        [0.0, 0.0, 0.0, 1.0],
    ];
    result
        .iter()
        .flatten()
        .all(|value| value.is_finite())
        .then_some(result)
}

fn interval_phase(phase: f32, start: f32, end: f32) -> f32 {
    if phase < start {
        return 0.0;
    }
    if phase > end {
        return 1.0;
    }
    if end == start {
        1.0
    } else if phase == start {
        0.0
    } else if phase == end {
        1.0
    } else {
        ((phase - start) / (end - start)).clamp(0.0, 1.0)
    }
}

fn sample_is_finite(sample: Op3Type2Sample) -> bool {
    sample
        .position
        .iter()
        .chain(sample.axis.iter())
        .chain(sample.scale.iter())
        .chain([&sample.opacity, &sample.material_phase])
        .all(|value| value.is_finite())
}

fn sample_linear_vector(body: &[u8], start: usize, end: usize, time: f32) -> Option<[f32; 3]> {
    let start = read_vec3(body, start)?;
    let end = read_vec3(body, end)?;
    if !start
        .iter()
        .chain(end.iter())
        .all(|value| value.is_finite())
        || !time.is_finite()
    {
        return None;
    }
    let values = std::array::from_fn(|axis| start[axis] + (end[axis] - start[axis]) * time);
    values
        .iter()
        .all(|value| value.is_finite())
        .then_some(values)
}

fn sample_power_vector(
    body: &[u8],
    start: usize,
    end: usize,
    exponent_offsets: [usize; 3],
    time: f32,
) -> Option<[f32; 3]> {
    let start = read_vec3(body, start)?;
    let end = read_vec3(body, end)?;
    let exponents = [
        read_f32(body, exponent_offsets[0])?,
        read_f32(body, exponent_offsets[1])?,
        read_f32(body, exponent_offsets[2])?,
    ];
    if !start
        .iter()
        .chain(end.iter())
        .chain(exponents.iter())
        .all(|value| value.is_finite())
        || !time.is_finite()
    {
        return None;
    }
    let values = std::array::from_fn(|axis| {
        let amount = time.powf(exponents[axis]);
        start[axis] + (end[axis] - start[axis]) * amount
    });
    values
        .iter()
        .all(|value| value.is_finite())
        .then_some(values)
}

fn read_vec3(body: &[u8], offset: usize) -> Option<[f32; 3]> {
    Some([
        read_f32(body, offset)?,
        read_f32(body, offset + 4)?,
        read_f32(body, offset + 8)?,
    ])
}

fn cross3(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

fn sub3(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

fn length3(vector: [f32; 3]) -> Option<f32> {
    let length = (vector[0] * vector[0] + vector[1] * vector[1] + vector[2] * vector[2]).sqrt();
    length.is_finite().then_some(length)
}

fn fract(value: f32) -> f32 {
    value - value.floor()
}

fn read_resource(command: &FxCommand) -> Option<EffectResource> {
    let bytes = command
        .raw_body
        .get(TYPE2_RESOURCE_OFFSET..TYPE2_RESOURCE_OFFSET + TYPE2_RESOURCE_BYTES)?;
    let archive = bounded_name(&bytes[..32])?;
    let name = bounded_name(&bytes[32..])?;
    (!archive.is_empty() && !name.is_empty()).then_some(EffectResource { archive, name })
}

fn bounded_name(bytes: &[u8]) -> Option<String> {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    let value = std::str::from_utf8(&bytes[..end]).ok()?.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn read_f32(bytes: &[u8], offset: usize) -> Option<f32> {
    Some(f32::from_le_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn type0_command_samples_position_and_scale_tracks_for_camera_facing_quad() {
        let mut body = vec![0_u8; 196];
        write_u32(&mut body, 0, TYPE0_BODY_DISCRIMINATOR);
        write_f32(&mut body, TYPE2_PHASE_START_OFFSET, -1.0);
        write_f32(&mut body, TYPE2_PHASE_END_OFFSET, 1.0);
        write_f32(&mut body, TYPE2_PHASE_EXPONENT_OFFSET, 1.0);
        write_f32(&mut body, TYPE2_OPACITY_START_OFFSET, 1.0);
        write_f32(&mut body, TYPE2_OPACITY_END_OFFSET, 0.0);
        write_f32(&mut body, TYPE2_OPACITY_EXPONENT_OFFSET, 1.0);
        write_f32(&mut body, TYPE2_ACTIVE_START_OFFSET, 0.0);
        write_f32(&mut body, TYPE2_ACTIVE_END_OFFSET, 1.0);
        write_vec3(&mut body, TYPE2_POSITION_START_OFFSET, [0.0, 0.0, 0.0]);
        write_vec3(&mut body, TYPE2_POSITION_END_OFFSET, [2.0, 4.0, 6.0]);
        write_vec3(&mut body, TYPE2_POSITION_EXPONENT_OFFSET, [1.0; 3]);
        write_vec3(&mut body, TYPE2_SCALE_START_OFFSET, [1.0; 3]);
        write_vec3(&mut body, TYPE2_SCALE_END_OFFSET, [3.0, 4.0, 5.0]);
        write_vec3(&mut body, 0x78, [3.0, 2.0, 1.0]);
        body[TYPE2_RESOURCE_OFFSET..TYPE2_RESOURCE_OFFSET + 11].copy_from_slice(b"effects.rlb");
        body[TYPE2_RESOURCE_OFFSET + 32..TYPE2_RESOURCE_OFFSET + 32 + 3].copy_from_slice(b"sun");

        let command = FxCommand {
            word: 3,
            opcode: FxOpcode::Op3,
            native_flag: false,
            raw_body: Arc::from(body.into_boxed_slice()),
            resource_refs: Vec::new(),
        };
        let identity = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let sample = sample_op3_type0_body(&command.raw_body, 0.5, 0.25).unwrap();
        assert_eq!(sample.position, [1.0, 2.0, 3.0]);
        assert_eq!(sample.scale, [2.0, 1.75, 1.5]);
        assert_eq!(sample.opacity, 0.5);
        assert_eq!(sample.material_phase, 0.25);

        let frame = evaluate_op3_type0_command(&command, 7, &identity, 0.25, 0.5, [1.0, 2.0, 13.0])
            .unwrap();
        assert_eq!(frame.command_index, 7);
        assert_eq!(frame.material.name, "sun");
        assert_eq!(frame.normalized_time, 0.5);
        assert_eq!(frame.material_phase, 0.25);
        assert_eq!(frame.color[3], 0.5);
        assert!(frame.active);
        let center = std::array::from_fn::<_, 3, _>(|axis| {
            frame
                .vertices
                .iter()
                .map(|vertex| vertex[axis])
                .sum::<f32>()
                / 4.0
        });
        assert_eq!(center, [1.0, 2.0, 3.0]);
        assert_ne!(frame.vertices[0], frame.vertices[1]);

        assert!(
            evaluate_op3_type0_command(&command, 7, &identity, 0.25, 1.1, [1.0, 2.0, 13.0],)
                .is_some_and(|frame| !frame.active)
        );
        assert!(
            evaluate_op3_type0_command(&command, 7, &identity, 0.25, 0.5, [1.0, 2.0, 3.0],)
                .is_none()
        );
    }

    #[test]
    fn type2_matrix_scales_parent_basis_then_overrides_translation() {
        let parent = [
            [2.0, 0.0, 0.0, 10.0],
            [0.0, 3.0, 0.0, 20.0],
            [0.0, 0.0, 4.0, 30.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let sample = Op3Type2Sample {
            position: [1.0, 2.0, 3.0],
            axis: [1.0, 2.0, 2.0],
            scale: [2.0, 3.0, 4.0],
            opacity: 1.0,
            material_phase: 0.0,
        };
        let result = native_op3_type2_matrix(&parent, sample).unwrap();
        assert_eq!(result[0][3], 12.0);
        assert_eq!(result[1][3], 26.0);
        assert_eq!(result[2][3], 42.0);
        assert_eq!(result[0][0], -8.0 / 5.0_f32.sqrt());
        assert_eq!(result[1][0], 9.0 / 5.0_f32.sqrt());
        assert_eq!(result[3], [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn type2_command_keeps_inclusive_single_phase_endpoint() {
        let mut body = vec![0_u8; 196];
        body[0..4].copy_from_slice(&TYPE2_BODY_DISCRIMINATOR.to_le_bytes());
        body[TYPE2_ACTIVE_START_OFFSET..TYPE2_ACTIVE_START_OFFSET + 4]
            .copy_from_slice(&1.0_f32.to_le_bytes());
        body[TYPE2_ACTIVE_END_OFFSET..TYPE2_ACTIVE_END_OFFSET + 4]
            .copy_from_slice(&1.0_f32.to_le_bytes());
        body[TYPE2_RESOURCE_OFFSET..TYPE2_RESOURCE_OFFSET + 11].copy_from_slice(b"effects.rlb");
        body[TYPE2_RESOURCE_OFFSET + 32..TYPE2_RESOURCE_OFFSET + 36].copy_from_slice(b"lamp");
        let command = FxCommand {
            word: 3,
            opcode: FxOpcode::Op3,
            native_flag: false,
            raw_body: Arc::from(body.into_boxed_slice()),
            resource_refs: Vec::new(),
        };
        let identity = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let frame = evaluate_op3_type2_command(&command, 0, &identity, 1.0, 1.0, 0).unwrap();
        assert_eq!(
            frame.material,
            EffectResource {
                archive: "effects.rlb".to_owned(),
                name: "lamp".to_owned(),
            }
        );
        assert!(frame.active);
        assert_eq!(frame.normalized_time, 1.0);

        let before = evaluate_op3_type2_command(&command, 0, &identity, 0.99, 0.99, 0).unwrap();
        assert!(!before.active);
    }

    #[test]
    fn samples_verified_type2_position_axis_scale_phase_and_opacity_tracks() {
        let mut body = vec![0_u8; 196];
        write_u32(&mut body, 0, TYPE2_BODY_DISCRIMINATOR);
        write_f32(&mut body, TYPE2_PHASE_START_OFFSET, -1.0);
        write_f32(&mut body, TYPE2_PHASE_END_OFFSET, 9.0);
        write_f32(&mut body, TYPE2_PHASE_EXPONENT_OFFSET, 1.0);
        write_f32(&mut body, TYPE2_OPACITY_START_OFFSET, 2.0);
        write_f32(&mut body, TYPE2_OPACITY_END_OFFSET, -1.0);
        write_f32(&mut body, TYPE2_OPACITY_EXPONENT_OFFSET, 1.0);
        write_vec3(&mut body, TYPE2_POSITION_START_OFFSET, [0.0, 0.0, 0.0]);
        write_vec3(&mut body, TYPE2_POSITION_END_OFFSET, [8.0, 16.0, 24.0]);
        write_vec3(&mut body, TYPE2_POSITION_EXPONENT_OFFSET, [1.0, 2.0, 3.0]);
        write_vec3(&mut body, TYPE2_AXIS_START_OFFSET, [1.0, 2.0, 3.0]);
        write_vec3(&mut body, TYPE2_AXIS_END_OFFSET, [5.0, 6.0, 7.0]);
        write_vec3(&mut body, TYPE2_SCALE_START_OFFSET, [1.0; 3]);
        write_vec3(&mut body, TYPE2_SCALE_END_OFFSET, [3.0; 3]);
        write_f32(&mut body, 0x78, 3.0);
        write_f32(&mut body, 0x7c, 2.0);
        write_f32(&mut body, 0x80, 1.0);

        let sample = sample_op3_type2_body(&body, 0.5, 0.25).unwrap();
        assert_eq!(sample.position, [4.0, 4.0, 3.0]);
        assert_eq!(sample.axis, [3.0, 4.0, 5.0]);
        assert_eq!(sample.scale, [2.0, 1.5, 1.25]);
        assert_eq!(sample.opacity, 0.5);
        assert_eq!(sample.material_phase, 0.25);
    }

    #[test]
    fn type1_basis_uses_raw_exact_dot_gate_and_native_cross_order() {
        // The native comparison is against the raw dot product, so this
        // oblique pair is rejected even though the vectors are not parallel.
        assert_eq!(
            native_op3_type1_basis([1.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0; 3]),
            None
        );
        assert_eq!(
            native_op3_type1_basis([1.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0; 3]),
            None
        );
        assert_eq!(
            native_op3_type1_basis([1.0, 0.0, 0.0], [-1.0, 0.0, 0.0], [0.0; 3]),
            None
        );

        // A non-unit view gives a raw dot of 2 and proceeds to the native
        // cross-product frame instead of being normalized before the gate.
        let frame =
            native_op3_type1_basis([2.0, 1.0, 0.0], [1.0, 0.0, 0.0], [3.0, 4.0, 5.0]).unwrap();
        assert_eq!(frame[0], [1.0, 0.0, 0.0, 3.0]);
        assert_eq!(frame[1], [0.0, 0.0, 1.0, 4.0]);
        assert_eq!(frame[2], [0.0, -1.0, 0.0, 5.0]);

        // The axis is not normalized: W and the axis column keep its scale.
        let axis_scaled =
            native_op3_type1_basis([2.0, 1.0, 0.0], [0.0, 0.0, 2.0], [0.0; 3]).unwrap();
        let inv_sqrt_5 = 1.0 / 5.0_f32.sqrt();
        assert_eq!(axis_scaled[0][1], inv_sqrt_5);
        assert_eq!(axis_scaled[1][1], -2.0 * inv_sqrt_5);
        assert_eq!(axis_scaled[0][2], 4.0 * inv_sqrt_5);
        assert_eq!(axis_scaled[1][2], 2.0 * inv_sqrt_5);
        assert_eq!(axis_scaled[2][0], 2.0);

        assert_eq!(
            native_op3_type1_basis([f32::NAN, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0; 3]),
            None
        );
    }

    #[test]
    fn type1_command_samples_authored_tracks_and_camera_basis() {
        let mut body = vec![0_u8; 196];
        write_u32(&mut body, 0, TYPE1_BODY_DISCRIMINATOR);
        write_f32(&mut body, TYPE2_PHASE_START_OFFSET, -1.0);
        write_f32(&mut body, TYPE2_PHASE_END_OFFSET, 2.0);
        write_f32(&mut body, TYPE2_PHASE_EXPONENT_OFFSET, 1.0);
        write_f32(&mut body, TYPE2_OPACITY_START_OFFSET, 1.0);
        write_f32(&mut body, TYPE2_OPACITY_END_OFFSET, 0.0);
        write_f32(&mut body, TYPE2_OPACITY_EXPONENT_OFFSET, 1.0);
        write_f32(&mut body, TYPE2_ACTIVE_START_OFFSET, 0.0);
        write_f32(&mut body, TYPE2_ACTIVE_END_OFFSET, 1.0);
        write_vec3(&mut body, TYPE2_POSITION_START_OFFSET, [0.0, 0.0, 0.0]);
        write_vec3(&mut body, TYPE2_POSITION_END_OFFSET, [2.0, 0.0, 0.0]);
        write_vec3(&mut body, TYPE2_POSITION_EXPONENT_OFFSET, [1.0; 3]);
        write_vec3(&mut body, TYPE2_AXIS_START_OFFSET, [0.0, 0.0, 1.0]);
        write_vec3(&mut body, TYPE2_AXIS_END_OFFSET, [0.0, 0.0, 1.0]);
        write_vec3(&mut body, TYPE2_SCALE_START_OFFSET, [1.0; 3]);
        write_vec3(&mut body, TYPE2_SCALE_END_OFFSET, [1.0; 3]);
        write_vec3(&mut body, 0x78, [1.0; 3]);
        body[TYPE2_RESOURCE_OFFSET..TYPE2_RESOURCE_OFFSET + 12].copy_from_slice(b"material.lib");
        body[TYPE2_RESOURCE_OFFSET + 32..TYPE2_RESOURCE_OFFSET + 37].copy_from_slice(b"b_p08");

        let command = FxCommand {
            word: 3,
            opcode: FxOpcode::Op3,
            native_flag: false,
            raw_body: Arc::from(body.into_boxed_slice()),
            resource_refs: Vec::new(),
        };
        let identity = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let frame =
            evaluate_op3_type1_command(&command, 4, &identity, 0.25, 0.5, [2.0, 1.0, 3.0]).unwrap();
        assert_eq!(frame.command_index, 4);
        assert_eq!(
            frame.material,
            EffectResource {
                archive: "material.lib".to_owned(),
                name: "b_p08".to_owned(),
            }
        );
        assert!(frame.active);
        assert_eq!(frame.normalized_time, 0.5);
        assert_eq!(frame.color, [1.0, 1.0, 1.0, 0.5]);
        assert_eq!(frame.material_phase, 0.5);
        let authored_axis_edge = std::array::from_fn::<_, 3, _>(|axis| {
            frame.vertices[2][axis] - frame.vertices[1][axis]
        });
        assert_eq!(authored_axis_edge, [0.0, 0.0, 1.0]);
        assert!(evaluate_op3_type1_command(
            &command,
            4,
            &identity,
            0.25,
            0.5,
            [f32::NAN, 1.0, 3.0],
        )
        .is_none());
    }

    #[test]
    #[ignore = "requires the original GOG installation"]
    fn original_deflector_quad_keeps_its_authored_axis_over_time_and_camera_motion() {
        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT")
            .expect("set FPARKAN_CORPUS_PART1_ROOT to the original GOG installation");
        let bytes = std::fs::read(std::path::Path::new(&root).join("effects.rlb"))
            .expect("read effects.rlb");
        let archive = fparkan_nres::decode(bytes.into(), fparkan_nres::ReadProfile::Compatible)
            .expect("decode effects.rlb");
        let entry = archive.find("deflector").expect("deflector FXID");
        let document = crate::decode_fxid(
            archive
                .payload(entry)
                .expect("FXID payload")
                .to_vec()
                .into(),
        )
        .expect("decode deflector");
        let command = &document.commands()[0];
        let parent = [
            [12.0, 0.0, 0.0, 100.0],
            [0.0, 3.0, 0.0, 200.0],
            [0.0, 0.0, 3.0, 20.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        for elapsed in [0.0_f32, 0.125, 0.5, 1.0, 2.5] {
            for camera in [
                [160.0, 230.0, 20.0],
                [160.0, 200.0, 50.0],
                [160.0, 170.0, 5.0],
            ] {
                let frame = evaluate_op3_type1_command(
                    command,
                    0,
                    &parent,
                    elapsed,
                    elapsed.min(1.0),
                    camera,
                )
                .expect("active deflector quad");
                assert!(frame.active);
                assert_eq!(frame.material.name.to_ascii_lowercase(), "defl_b");
                let axis_edge = std::array::from_fn::<_, 3, _>(|axis| {
                    frame.vertices[2][axis] - frame.vertices[1][axis]
                });
                assert_eq!(axis_edge, [12.0, 0.0, 0.0]);
                let center = std::array::from_fn::<_, 3, _>(|axis| {
                    (frame.vertices[0][axis] + frame.vertices[2][axis]) * 0.5
                });
                assert_eq!(center, [106.0, 200.0, 20.0]);
                assert_eq!(frame.material_phase, (elapsed * 7.0).fract());
            }
        }
    }

    #[test]
    #[ignore = "requires the original GOG installation"]
    fn original_tree_ray_58_matches_native_type1_geometry_samples() {
        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT")
            .expect("set FPARKAN_CORPUS_PART1_ROOT to the original GOG installation");
        let bytes = std::fs::read(std::path::Path::new(&root).join("effects.rlb"))
            .expect("read effects.rlb");
        let archive = fparkan_nres::decode(bytes.into(), fparkan_nres::ReadProfile::Compatible)
            .expect("decode effects.rlb");
        let entry = archive.find("tree_ray_58").expect("tree_ray_58 FXID");
        let document = crate::decode_fxid(
            archive
                .payload(entry)
                .expect("FXID payload")
                .to_vec()
                .into(),
        )
        .expect("decode tree_ray_58");
        assert_eq!(document.header().time_mode, 2);
        assert_eq!(document.header().duration_seconds, 3.0);
        assert_eq!(document.commands().len(), 3);

        let parent = [
            [0.999_924_123, 0.012_319_660, 0.0, 386.673_400_879],
            [-0.012_319_660, 0.999_924_123, 0.0, 603.692_321_777],
            [0.0, 0.0, 1.0, 180.414_703_369],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let center = [390.273_132, 603.647_949, 180.414_703];
        // Original GOG Effect.dll RVA 0x93D0, x87 CW 0x007F; Rust-sampled keys are supplied separately.
        let native_samples = [
            (
                0,
                "defl_y_add",
                [0.0, 35.0, 22.0],
                [
                    [386.280, 604.229_431, 179.568_024],
                    [386.266_876, 603.165_039, 181.261_383],
                    [394.266_266, 603.066_467, 181.261_383],
                    [394.279_4, 604.130_859, 179.568_024],
                ],
            ),
            (
                0,
                "defl_y_add",
                [20.0, -30.0, 15.0],
                [
                    [386.279, 604.147_4, 181.307_663],
                    [386.267_883, 603.247_1, 179.521_744],
                    [394.267_273, 603.148_5, 179.521_744],
                    [394.278_381, 604.048_8, 181.307_663],
                ],
            ),
            (
                1,
                "defl_y1_add",
                [0.0, 35.0, 22.0],
                [
                    [386.276_7, 603.963_3, 179.991_364],
                    [386.270_172, 603.431_152, 180.838_043],
                    [394.269_562, 603.332_6, 180.838_043],
                    [394.276_1, 603.864_746, 179.991_364],
                ],
            ),
        ];
        for (command_index, material_name, camera_offset, expected_vertices) in native_samples {
            let camera = std::array::from_fn(|axis| center[axis] + camera_offset[axis]);
            let frame = evaluate_op3_type1_command(
                &document.commands()[command_index],
                command_index,
                &parent,
                0.0,
                0.0,
                camera,
            )
            .expect("active tree ray quad");
            assert_eq!(frame.material.name, material_name);
            for (actual, expected) in frame
                .vertices
                .iter()
                .flatten()
                .zip(expected_vertices.iter().flatten())
            {
                assert!(
                    (actual - expected).abs() < 0.001,
                    "native sample vertex mismatch: {actual} vs {expected}"
                );
            }
        }

        for (command_index, material_name) in [(0, "defl_y_add"), (1, "defl_y1_add")] {
            let command = &document.commands()[command_index];
            assert_eq!(command.opcode, FxOpcode::Op3);
            assert_eq!(
                read_u32(&command.raw_body, 0),
                Some(TYPE1_BODY_DISCRIMINATOR)
            );
            let camera_offsets = [[0.0, 35.0, 22.0], [20.0, -30.0, 15.0], [-25.0, 18.0, 10.0]];
            for elapsed in [0.0_f32, 0.375, 0.75, 1.5, 2.25, 2.999, 3.125] {
                let phase = elapsed.rem_euclid(3.0) / 3.0;
                for camera_offset in camera_offsets {
                    let camera = std::array::from_fn(|axis| center[axis] + camera_offset[axis]);
                    let frame = evaluate_op3_type1_command(
                        command,
                        command_index,
                        &parent,
                        elapsed,
                        phase,
                        camera,
                    )
                    .expect("active tree ray quad");
                    assert_eq!(frame.material.name, material_name);
                    assert!(frame.active);
                    let actual_center: [f32; 3] = std::array::from_fn(|axis| {
                        frame
                            .vertices
                            .iter()
                            .map(|vertex| vertex[axis])
                            .sum::<f32>()
                            * 0.25
                    });
                    for axis in 0..3 {
                        assert!((actual_center[axis] - center[axis]).abs() < 0.001);
                    }
                    let authored_axis_edge: [f32; 3] = std::array::from_fn(|axis| {
                        frame.vertices[2][axis] - frame.vertices[1][axis]
                    });
                    let native_axis_edge = [7.999_393, -0.098_557_28, 0.0];
                    for axis in 0..3 {
                        assert!((authored_axis_edge[axis] - native_axis_edge[axis]).abs() < 0.001);
                    }
                }
            }
        }
    }

    #[test]
    fn rejects_truncated_bodies_and_non_finite_tracks_or_inputs() {
        let identity = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let truncated = FxCommand {
            word: 3,
            opcode: FxOpcode::Op3,
            native_flag: false,
            raw_body: Arc::from(
                vec![0_u8; TYPE2_RESOURCE_OFFSET + TYPE2_RESOURCE_BYTES - 1].into_boxed_slice(),
            ),
            resource_refs: Vec::new(),
        };
        assert!(
            evaluate_op3_type0_command(&truncated, 0, &identity, 0.0, 0.0, [0.0; 3],).is_none()
        );

        let mut type0_body = vec![0_u8; TYPE2_RESOURCE_OFFSET + TYPE2_RESOURCE_BYTES];
        write_u32(&mut type0_body, 0, TYPE0_BODY_DISCRIMINATOR);
        write_f32(
            &mut type0_body,
            TYPE2_POSITION_EXPONENT_OFFSET,
            f32::INFINITY,
        );
        assert!(sample_op3_type0_body(&type0_body, 0.5, 0.25).is_none());

        let mut type2_body = vec![0_u8; TYPE2_RESOURCE_OFFSET + TYPE2_RESOURCE_BYTES];
        write_u32(&mut type2_body, 0, TYPE2_BODY_DISCRIMINATOR);
        let non_finite_elapsed = FxCommand {
            word: 3,
            opcode: FxOpcode::Op3,
            native_flag: false,
            raw_body: Arc::from(type2_body.into_boxed_slice()),
            resource_refs: Vec::new(),
        };
        assert!(evaluate_op3_type2_command(
            &non_finite_elapsed,
            0,
            &identity,
            f32::INFINITY,
            0.5,
            0,
        )
        .is_none());
    }

    fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn write_f32(bytes: &mut [u8], offset: usize, value: f32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn write_vec3(bytes: &mut [u8], offset: usize, value: [f32; 3]) {
        for (axis, component) in value.into_iter().enumerate() {
            write_f32(bytes, offset + axis * 4, component);
        }
    }
}
