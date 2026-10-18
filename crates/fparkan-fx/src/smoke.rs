//! Deterministic evaluator for verified native opcode-8 particle pools.

use crate::environment::EffectResource;
use crate::fx_visual::{
    compose_native_effect_matrix, native_camera_facing_basis, native_effect_local_point,
    transform_native_effect_quad, EffectQuadFrame, EFFECT_QUAD_UV,
};
use crate::{FxCommand, FxOpcode};
use std::sync::Arc;

const RESOURCE_OFFSET: usize = 0xB4;
const RESOURCE_BYTES: usize = 64;
const MIN_BODY_SIZE: usize = RESOURCE_OFFSET + RESOURCE_BYTES;
const MAX_POOL_SIZE: usize = 4096;
const MAX_CATCH_UP: usize = 100_000;
const U16_TO_UNIT: f32 = 1.0 / 65_536.0;

/// Bounds for the native per-particle detail threshold.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Op8DetailThresholdRange {
    /// Lower native threshold bound.
    min: f32,
    /// Upper native threshold bound.
    max: f32,
}

impl Op8DetailThresholdRange {
    /// Returns a range only when both endpoints are finite and ordered.
    #[must_use]
    pub fn new(min: f32, max: f32) -> Option<Self> {
        (min.is_finite() && max.is_finite() && min <= max).then_some(Self { min, max })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NativeLfsr16Pair {
    state: u32,
}

impl NativeLfsr16Pair {
    fn from_word(state: u32) -> Self {
        Self { state }
    }

    fn next_u16(&mut self) -> u16 {
        crate::advance_native_fx_random_state(&mut self.state)
    }

    fn next_unit(&mut self) -> f32 {
        f32::from(self.next_u16()) * U16_TO_UNIT
    }

    fn next_signed(&mut self, width: f32) -> f32 {
        (self.next_unit() - 0.5) * width
    }
}

#[derive(Clone, Copy, Debug)]
struct Op8Track {
    start: [f32; 3],
    end: [f32; 3],
    exponent: [f32; 3],
}

#[derive(Clone, Debug)]
struct ParticleSlot {
    born_at: f32,
    threshold: f32,
    random_phase: f32,
    owner_matrix: [[f32; 4]; 4],
    position: Op8Track,
    scale: Op8Track,
}

/// Mutable state for one Op8 command. Keep one state per command instance.
#[derive(Clone, Debug)]
pub struct SmokeEmitterState {
    initial_emitter_rng: NativeLfsr16Pair,
    initial_signed_rng: NativeLfsr16Pair,
    initial_material_rng: NativeLfsr16Pair,
    emitter_rng: NativeLfsr16Pair,
    signed_rng: NativeLfsr16Pair,
    material_rng: NativeLfsr16Pair,
    body: Option<Arc<[u8]>>,
    emitter_position: Option<Op8Track>,
    stored_anchor: Option<[f32; 3]>,
    cursor: usize,
    last_spawn: f32,
    interval: f32,
    slots: Vec<Option<ParticleSlot>>,
}

impl SmokeEmitterState {
    /// Creates an evaluator with explicit emitter, signed-range, and material seeds.
    #[must_use]
    pub fn new(emitter_seed: u32, signed_seed: u32, material_seed: u32) -> Self {
        let initial_emitter_rng = NativeLfsr16Pair::from_word(emitter_seed);
        let initial_signed_rng = NativeLfsr16Pair::from_word(signed_seed);
        let initial_material_rng = NativeLfsr16Pair::from_word(material_seed);
        Self {
            initial_emitter_rng,
            initial_signed_rng,
            initial_material_rng,
            emitter_rng: initial_emitter_rng,
            signed_rng: initial_signed_rng,
            material_rng: initial_material_rng,
            body: None,
            emitter_position: None,
            stored_anchor: None,
            cursor: 0,
            last_spawn: 0.0,
            interval: 0.0,
            slots: Vec::new(),
        }
    }

    /// Resets the three streams and the native pool state.
    fn reset(&mut self) {
        self.emitter_rng = self.initial_emitter_rng;
        self.signed_rng = self.initial_signed_rng;
        self.material_rng = self.initial_material_rng;
        self.body = None;
        self.emitter_position = None;
        self.stored_anchor = None;
        self.cursor = 0;
        self.last_spawn = 0.0;
        self.interval = 0.0;
        self.slots.clear();
    }

    /// Number of slots retained by the native ring.
    #[must_use]
    #[cfg(test)]
    fn pool_len(&self) -> usize {
        self.slots.len()
    }

    /// Native ring cursor. A scheduled event advances this index before access.
    #[must_use]
    #[cfg(test)]
    fn next_slot(&self) -> usize {
        self.cursor
    }

    fn material_threshold(&mut self, range: Op8DetailThresholdRange) -> f32 {
        range.min + self.material_rng.next_unit() * (range.max - range.min)
    }
}

/// Tests the native inclusive detail gate.
#[must_use]
fn detail_class_passes_threshold(detail_class: f32, threshold: f32) -> bool {
    detail_class.is_finite() && threshold.is_finite() && detail_class <= threshold
}

/// Evaluates the native Op8 pool and returns one frame per slot in ring order.
///
/// The body is parsed from its verified shared offsets, so material names and
/// pool sizes can vary. The caller supplies the native update clock, normalized
/// effect phase, detail threshold bounds, owner transform, camera position,
/// and explicit RNG state. The first in-window call captures the owner pose
/// and emits nothing, matching the native update path. Each emitted particle
/// keeps its interpolated birth anchor and a motion-oriented slot basis.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn evaluate_op8_emitter(
    command: &FxCommand,
    command_index: usize,
    world_from_effect: &[[f32; 4]; 4],
    camera_world: [f32; 3],
    update_clock: f32,
    effect_phase: f32,
    detail_class: u32,
    detail_threshold_range: Op8DetailThresholdRange,
    state: &mut SmokeEmitterState,
) -> Option<Vec<EffectQuadFrame>> {
    if command.opcode != FxOpcode::Op8
        || !world_from_effect
            .iter()
            .flatten()
            .all(|value| value.is_finite())
        || !camera_world.iter().all(|value| value.is_finite())
        || !update_clock.is_finite()
        || !effect_phase.is_finite()
        || !detail_threshold_range.min.is_finite()
        || !detail_threshold_range.max.is_finite()
        || detail_threshold_range.min > detail_threshold_range.max
        || !(detail_threshold_range.max - detail_threshold_range.min).is_finite()
    {
        return None;
    }

    let params = Op8Params::parse(command)?;
    if state
        .body
        .as_deref()
        .is_some_and(|body| body != command.raw_body.as_ref())
    {
        state.reset();
    }
    state.body = Some(Arc::clone(&command.raw_body));

    if state.slots.len() != params.pool_count {
        if !state.slots.is_empty() {
            state.reset();
            state.body = Some(Arc::clone(&command.raw_body));
        }
        state.slots.resize_with(params.pool_count, || None);
        state.cursor = 0;
    }
    if state.emitter_position.is_none() {
        state.emitter_position = Some(sample_emitter_position(&params, state)?);
    }

    let in_window = effect_phase >= params.window_start && effect_phase <= params.window_end;
    let interval_phase =
        normalized_window_phase(effect_phase, params.window_start, params.window_end);
    let due_interval =
        params.interval_start + (params.interval_end - params.interval_start) * interval_phase;
    let schedule_interval =
        params.interval_start + (params.interval_end - params.interval_start) * effect_phase;
    if !due_interval.is_finite() || due_interval <= 0.0 {
        return None;
    }
    let emitter_offset = sample_track(state.emitter_position?, interval_phase)?;
    let current_anchor = apply_emitter_offset(world_from_effect, emitter_offset)?;

    if !in_window && !state.slots.iter().any(Option::is_some) {
        // The native update returns early once its live-particle count is zero.
        // A later in-window update captures a fresh owner anchor before emitting.
        state.stored_anchor = None;
        state.last_spawn = 0.0;
        state.interval = due_interval;
    } else if state.last_spawn == 0.0 {
        // Native uses a zero schedule base as its initialization sentinel.
        // If the first update clock is zero, the next update captures again.
        state.stored_anchor = Some(matrix_translation(&current_anchor));
        state.last_spawn = update_clock;
        state.interval = due_interval;
    } else {
        // Native clamps only the schedule base on a time rewind. Existing
        // particles and all three random streams remain intact.
        if state.last_spawn > update_clock {
            state.last_spawn = update_clock;
        }
        let catch_up = catch_up_count(
            state.last_spawn,
            update_clock,
            due_interval,
            schedule_interval,
        )?;
        let schedule_origin = state.last_spawn;
        let mut stored_anchor = state.stored_anchor?;
        let velocity = if catch_up == 0 {
            [0.0; 3]
        } else {
            owner_anchor_velocity(
                &current_anchor,
                stored_anchor,
                update_clock,
                schedule_origin,
            )?
        };
        for _ in 0..catch_up {
            let schedule_base = state.last_spawn;
            let born_at = schedule_base + due_interval;
            let next_schedule_base = schedule_base + schedule_interval;
            if !born_at.is_finite() {
                return None;
            }
            if !next_schedule_base.is_finite() || next_schedule_base <= schedule_base {
                return None;
            }
            let (owner_matrix, next_anchor) =
                advance_owner_anchor(&current_anchor, stored_anchor, velocity, due_interval)?;
            if in_window {
                spawn_particle(
                    &params,
                    detail_threshold_range,
                    born_at,
                    owner_matrix,
                    state,
                )?;
            } else {
                retire_particle(params.pool_count, state);
            }
            stored_anchor = next_anchor;
            state.stored_anchor = Some(next_anchor);
            state.last_spawn = next_schedule_base;
        }
        state.interval = due_interval;
    }

    let interval_period = state.interval * params.pool_count as f32;
    if !interval_period.is_finite() || interval_period <= 0.0 {
        return None;
    }
    let mut frames = Vec::with_capacity(params.pool_count);

    for (slot_index, slot) in state.slots.iter().enumerate() {
        let Some(particle) = slot else {
            frames.push(empty_frame(command_index, &params.resource));
            continue;
        };
        let mut raw_phase = (update_clock - state.last_spawn) / interval_period
            + (state.cursor as f32 - slot_index as f32) / params.pool_count as f32;
        if raw_phase < 0.0 {
            raw_phase += 1.0;
        }
        if !raw_phase.is_finite() {
            return None;
        }

        let position = sample_track(particle.position, raw_phase)?;
        let scale = sample_track(particle.scale, raw_phase)?;
        let opacity = params.opacity_start
            + (params.opacity_end - params.opacity_start) * raw_phase.powf(params.opacity_exponent);
        let material_phase = if params.material_phase_exponent < 0.0 {
            particle.random_phase
        } else {
            raw_phase.powf(params.material_phase_exponent)
        };
        if !opacity.is_finite() || !material_phase.is_finite() {
            return None;
        }

        let local_position = position;
        let detail_passes = detail_class_passes_threshold(detail_class as f32, particle.threshold);
        let camera_local = native_effect_local_point(&particle.owner_matrix, camera_world)?;
        let view = subtract3(camera_local, local_position);
        let Some(local_basis) = native_camera_facing_basis(view, local_position) else {
            frames.push(EffectQuadFrame {
                command_index,
                opcode: FxOpcode::Op8,
                material: params.resource.clone(),
                vertices: [[0.0; 3]; 4],
                uv: EFFECT_QUAD_UV,
                color: [1.0, 1.0, 1.0, 0.0],
                material_phase,
                elapsed_seconds: (update_clock - particle.born_at).max(0.0),
                normalized_time: raw_phase,
                active: false,
                native_flag_base: None,
            });
            continue;
        };
        let matrix = compose_native_effect_matrix(
            &particle.owner_matrix,
            &local_basis,
            local_position,
            scale,
        )?;
        let vertices = transform_native_effect_quad(&matrix)?;
        let visible = detail_passes;
        frames.push(EffectQuadFrame {
            command_index,
            opcode: FxOpcode::Op8,
            material: params.resource.clone(),
            vertices,
            uv: EFFECT_QUAD_UV,
            color: [1.0, 1.0, 1.0, if visible { opacity } else { 0.0 }],
            material_phase,
            elapsed_seconds: (update_clock - particle.born_at).max(0.0),
            normalized_time: raw_phase,
            active: visible,
            native_flag_base: None,
        });
    }
    Some(frames)
}

#[derive(Clone, Debug)]
struct Op8Params {
    resource: EffectResource,
    pool_count: usize,
    window_start: f32,
    window_end: f32,
    interval_start: f32,
    interval_end: f32,
    material_phase_exponent: f32,
    opacity_start: f32,
    opacity_end: f32,
    opacity_exponent: f32,
    emitter_position_start: [f32; 3],
    emitter_position_end_center: [f32; 3],
    emitter_position_width: [f32; 3],
    emitter_position_exponent: [f32; 3],
    position_start: [f32; 3],
    position_end_center: [f32; 3],
    position_width: [f32; 3],
    position_exponent: [f32; 3],
    scale_start: [f32; 3],
    scale_end_center: [f32; 3],
    scale_width: [f32; 3],
    scale_exponent: [f32; 3],
}

impl Op8Params {
    fn parse(command: &FxCommand) -> Option<Self> {
        let body = &command.raw_body;
        if body.len() < MIN_BODY_SIZE {
            return None;
        }
        let pool_count = read_u32(body, 0x20)? as usize;
        let window_start = read_f32(body, 0x0C)?;
        let window_end = read_f32(body, 0x10)?;
        let interval_start = read_f32(body, 0x14)?;
        let interval_end = read_f32(body, 0x18)?;
        let material_phase_exponent = read_f32(body, 0x1C)?;
        let opacity_start = read_f32(body, 0x00)?;
        let opacity_end = read_f32(body, 0x04)?;
        let opacity_exponent = read_f32(body, 0x08)?;
        let emitter_position_start = read_vec3(body, 0x24)?;
        let emitter_position_end_center = read_vec3(body, 0x30)?;
        let emitter_position_width = read_vec3(body, 0x3C)?;
        let emitter_position_exponent = read_vec3(body, 0x48)?;
        let position_start = read_vec3(body, 0x54)?;
        let position_end_center = read_vec3(body, 0x60)?;
        let position_width = read_vec3(body, 0x6C)?;
        let position_exponent = read_vec3(body, 0x78)?;
        let scale_start = read_vec3(body, 0x84)?;
        let scale_end_center = read_vec3(body, 0x90)?;
        let scale_width = read_vec3(body, 0x9C)?;
        let scale_exponent = read_vec3(body, 0xA8)?;
        if pool_count == 0
            || pool_count > MAX_POOL_SIZE
            || ![
                window_start,
                window_end,
                interval_start,
                interval_end,
                material_phase_exponent,
                opacity_start,
                opacity_end,
                opacity_exponent,
            ]
            .iter()
            .all(|value| value.is_finite())
            || window_start > window_end
            || interval_start <= 0.0
            || interval_end <= 0.0
            || !emitter_position_start
                .iter()
                .chain(emitter_position_end_center.iter())
                .chain(emitter_position_width.iter())
                .chain(emitter_position_exponent.iter())
                .chain(position_start.iter())
                .chain(position_end_center.iter())
                .chain(position_width.iter())
                .chain(position_exponent.iter())
                .chain(scale_start.iter())
                .chain(scale_end_center.iter())
                .chain(scale_width.iter())
                .chain(scale_exponent.iter())
                .all(|value| value.is_finite())
        {
            return None;
        }

        Some(Self {
            resource: read_resource(body)?,
            pool_count,
            window_start,
            window_end,
            interval_start,
            interval_end,
            material_phase_exponent,
            opacity_start,
            opacity_end,
            opacity_exponent,
            emitter_position_start,
            emitter_position_end_center,
            emitter_position_width,
            emitter_position_exponent,
            position_start,
            position_end_center,
            position_width,
            position_exponent,
            scale_start,
            scale_end_center,
            scale_width,
            scale_exponent,
        })
    }
}

fn normalized_window_phase(phase: f32, start: f32, end: f32) -> f32 {
    let phase = (phase - start) / (end - start);
    if !(phase >= 0.0) {
        0.0
    } else if phase > 1.0 {
        1.0
    } else {
        phase
    }
}

fn catch_up_count(
    last_spawn: f32,
    update_clock: f32,
    due_interval: f32,
    schedule_interval: f32,
) -> Option<usize> {
    if !last_spawn.is_finite() || !update_clock.is_finite() || !due_interval.is_finite() {
        return None;
    }
    if due_interval <= 0.0 {
        return None;
    }

    let mut scheduled = last_spawn;
    let mut count = 0;
    loop {
        let due_at = scheduled + due_interval;
        if !due_at.is_finite() {
            return None;
        }
        if !(due_at < update_clock) {
            return Some(count);
        }
        if count == MAX_CATCH_UP || !schedule_interval.is_finite() || schedule_interval <= 0.0 {
            return None;
        }
        let next_base = scheduled + schedule_interval;
        if !next_base.is_finite() || next_base <= scheduled {
            return None;
        }
        scheduled = next_base;
        count += 1;
    }
}

fn sample_emitter_position(params: &Op8Params, state: &mut SmokeEmitterState) -> Option<Op8Track> {
    // Native 11343..113A4 draws Z from the uniform stream first and centers
    // it around zero, then draws centered Y and X from the signed-range stream.
    let mut end = params.emitter_position_end_center;
    end[2] += state.emitter_rng.next_unit() * params.emitter_position_width[2]
        - params.emitter_position_width[2] * 0.5;
    end[1] += state
        .signed_rng
        .next_signed(params.emitter_position_width[1]);
    end[0] += state
        .signed_rng
        .next_signed(params.emitter_position_width[0]);
    end.iter()
        .all(|value| value.is_finite())
        .then_some(Op8Track {
            start: params.emitter_position_start,
            end,
            exponent: params.emitter_position_exponent,
        })
}

fn apply_emitter_offset(owner: &[[f32; 4]; 4], offset: [f32; 3]) -> Option<[[f32; 4]; 4]> {
    let mut anchor = *owner;
    for axis in 0..3 {
        anchor[axis][3] += offset[axis];
    }
    anchor
        .iter()
        .flatten()
        .all(|value| value.is_finite())
        .then_some(anchor)
}

fn spawn_particle(
    params: &Op8Params,
    threshold_range: Op8DetailThresholdRange,
    born_at: f32,
    owner_matrix: [[f32; 4]; 4],
    state: &mut SmokeEmitterState,
) -> Option<()> {
    let threshold = state.material_threshold(threshold_range);
    let random_phase = state.emitter_rng.next_unit();
    let position_z = params.position_end_center[2]
        + state.emitter_rng.next_unit() * params.position_width[2]
        - params.position_width[2] * 0.5;
    let position_y =
        params.position_end_center[1] + state.signed_rng.next_signed(params.position_width[1]);
    let position_x =
        params.position_end_center[0] + state.signed_rng.next_signed(params.position_width[0]);
    let scale_z = params.scale_end_center[2] + state.signed_rng.next_signed(params.scale_width[2]);
    let scale_y = params.scale_end_center[1] + state.signed_rng.next_signed(params.scale_width[1]);
    let scale_x = params.scale_end_center[0] + state.signed_rng.next_signed(params.scale_width[0]);
    let position_end = [position_x, position_y, position_z];
    let scale_end = [scale_x, scale_y, scale_z];
    if !threshold.is_finite()
        || !position_end.iter().all(|value| value.is_finite())
        || !scale_end.iter().all(|value| value.is_finite())
        || !born_at.is_finite()
    {
        return None;
    }
    // The native ring increments its cursor before using the slot.
    state.cursor = (state.cursor + 1) % params.pool_count;
    state.slots[state.cursor] = Some(ParticleSlot {
        born_at,
        threshold,
        random_phase,
        owner_matrix,
        position: Op8Track {
            start: params.position_start,
            end: position_end,
            exponent: params.position_exponent,
        },
        scale: Op8Track {
            start: params.scale_start,
            end: scale_end,
            exponent: params.scale_exponent,
        },
    });
    Some(())
}

fn retire_particle(pool_count: usize, state: &mut SmokeEmitterState) {
    // Out-of-window ticks follow the same cursor cadence, but mark the slot
    // reached by that tick inactive instead of emitting a replacement.
    state.cursor = (state.cursor + 1) % pool_count;
    state.slots[state.cursor] = None;
}

fn matrix_translation(matrix: &[[f32; 4]; 4]) -> [f32; 3] {
    [matrix[0][3], matrix[1][3], matrix[2][3]]
}

fn owner_anchor_velocity(
    current: &[[f32; 4]; 4],
    stored_anchor: [f32; 3],
    update_clock: f32,
    schedule_base: f32,
) -> Option<[f32; 3]> {
    let elapsed = update_clock - schedule_base;
    if !elapsed.is_finite() || elapsed <= 0.0 {
        return None;
    }
    let velocity = std::array::from_fn(|axis| (current[axis][3] - stored_anchor[axis]) / elapsed);
    velocity
        .iter()
        .all(|value| value.is_finite())
        .then_some(velocity)
}

fn advance_owner_anchor(
    current: &[[f32; 4]; 4],
    stored_anchor: [f32; 3],
    velocity: [f32; 3],
    due_interval: f32,
) -> Option<([[f32; 4]; 4], [f32; 3])> {
    let mut velocity = velocity;
    if vector_length_squared(velocity)? == 0.0 {
        // Native replaces the zero velocity vector with the current owner's X
        // column before both stepping the anchor and building the slot frame.
        velocity = [current[0][0], current[1][0], current[2][0]];
    }
    let translation =
        std::array::from_fn(|axis| stored_anchor[axis] + velocity[axis] * due_interval);
    let tangent = normalize_vector(velocity)?;

    let owner_up = [current[0][2], current[1][2], current[2][2]];
    let side_candidate = cross(owner_up, tangent);
    let side = if vector_length_squared(side_candidate)? == 0.0 {
        native_fallback_side(tangent)?
    } else {
        normalize_vector(side_candidate)?
    };
    let up = cross(tangent, side);
    let matrix = [
        [tangent[0], side[0], up[0], translation[0]],
        [tangent[1], side[1], up[1], translation[1]],
        [tangent[2], side[2], up[2], translation[2]],
        [0.0, 0.0, 0.0, 1.0],
    ];
    (matrix.iter().flatten().all(|value| value.is_finite())
        && translation.iter().all(|value| value.is_finite()))
    .then_some((matrix, translation))
}

fn vector_length_squared(vector: [f32; 3]) -> Option<f32> {
    let length_squared = vector
        .iter()
        .map(|component| component * component)
        .sum::<f32>();
    length_squared.is_finite().then_some(length_squared)
}

fn normalize_vector(vector: [f32; 3]) -> Option<[f32; 3]> {
    let length_squared = vector_length_squared(vector)?;
    if length_squared == 0.0 {
        return None;
    }
    let inverse_length = length_squared.sqrt().recip();
    let normalized = vector.map(|component| component * inverse_length);
    normalized
        .iter()
        .all(|component| component.is_finite())
        .then_some(normalized)
}

fn native_fallback_side(tangent: [f32; 3]) -> Option<[f32; 3]> {
    // Match 38D0: choose X when tangent X is zero, otherwise Y when tangent
    // Y is zero, otherwise use (-Y, X, 0) and normalize only when Z is nonzero.
    let mut side = if tangent[0] == 0.0 {
        [1.0, 0.0, 0.0]
    } else if tangent[1] == 0.0 {
        [0.0, 1.0, 0.0]
    } else {
        [-tangent[1], tangent[0], 0.0]
    };
    if tangent[0] != 0.0 && tangent[1] != 0.0 && tangent[2] != 0.0 {
        side = normalize_vector(side)?;
    }
    Some(side)
}

fn cross(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

fn sample_track(track: Op8Track, phase: f32) -> Option<[f32; 3]> {
    let sample = std::array::from_fn(|axis| {
        track.start[axis] + (track.end[axis] - track.start[axis]) * phase.powf(track.exponent[axis])
    });
    sample
        .iter()
        .all(|value| value.is_finite())
        .then_some(sample)
}

fn empty_frame(command_index: usize, resource: &EffectResource) -> EffectQuadFrame {
    EffectQuadFrame {
        command_index,
        opcode: FxOpcode::Op8,
        material: resource.clone(),
        vertices: [[0.0; 3]; 4],
        uv: EFFECT_QUAD_UV,
        color: [1.0, 1.0, 1.0, 0.0],
        material_phase: 0.0,
        elapsed_seconds: 0.0,
        normalized_time: 0.0,
        active: false,
        native_flag_base: None,
    }
}

fn read_resource(body: &[u8]) -> Option<EffectResource> {
    let bytes = body.get(RESOURCE_OFFSET..RESOURCE_OFFSET + RESOURCE_BYTES)?;
    Some(EffectResource {
        archive: read_name(&bytes[..32])?,
        name: read_name(&bytes[32..])?,
    })
}

fn read_name(bytes: &[u8]) -> Option<String> {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    let name = std::str::from_utf8(&bytes[..end]).ok()?.trim();
    (!name.is_empty()).then(|| name.to_owned())
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

fn read_vec3(bytes: &[u8], offset: usize) -> Option<[f32; 3]> {
    Some([
        read_f32(bytes, offset)?,
        read_f32(bytes, offset + 4)?,
        read_f32(bytes, offset + 8)?,
    ])
}

fn subtract3(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|axis| left[axis] - right[axis])
}

#[cfg(test)]
mod tests {
    use super::{
        detail_class_passes_threshold, evaluate_op8_emitter, Op8DetailThresholdRange,
        SmokeEmitterState,
    };
    use crate::{fx_visual::EffectQuadFrame, FxCommand, FxOpcode};
    use std::sync::Arc;

    const IDENTITY: [[f32; 4]; 4] = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];

    fn command(resource: &str, pool_count: u32, native_flag: bool) -> FxCommand {
        let mut body = vec![0_u8; 0xF4];
        put_f32(&mut body, 0x00, 1.0);
        put_f32(&mut body, 0x04, 0.0);
        put_f32(&mut body, 0x08, 1.0);
        put_f32(&mut body, 0x0C, 0.0);
        put_f32(&mut body, 0x10, 1.0);
        put_f32(&mut body, 0x14, 0.08);
        put_f32(&mut body, 0x18, 0.08);
        put_f32(&mut body, 0x1C, 1.0);
        put_u32(&mut body, 0x20, pool_count);
        put_vec3(&mut body, 0x24, [0.0; 3]);
        put_vec3(&mut body, 0x30, [0.0; 3]);
        put_vec3(&mut body, 0x3C, [0.0; 3]);
        put_vec3(&mut body, 0x48, [1.0; 3]);
        put_vec3(&mut body, 0x54, [0.0; 3]);
        put_vec3(&mut body, 0x60, [0.0, 0.0, 50.0]);
        put_vec3(&mut body, 0x6C, [10.0, 30.0, 30.0]);
        put_vec3(&mut body, 0x78, [1.0; 3]);
        put_vec3(&mut body, 0x84, [10.0; 3]);
        put_vec3(&mut body, 0x90, [30.0; 3]);
        put_vec3(&mut body, 0x9C, [0.0, 1.0, 1.0]);
        put_vec3(&mut body, 0xA8, [1.0; 3]);
        put_name(&mut body, 0xB4, "material.lib");
        put_name(&mut body, 0xD4, resource);
        FxCommand {
            word: if native_flag { 0x108 } else { 8 },
            opcode: FxOpcode::Op8,
            native_flag,
            raw_body: Arc::from(body),
            resource_refs: Vec::new(),
        }
    }

    fn evaluate(
        command: &FxCommand,
        clock: f32,
        phase: f32,
        detail_class: u32,
        state: &mut SmokeEmitterState,
    ) -> Vec<EffectQuadFrame> {
        evaluate_with_owner(command, clock, phase, detail_class, &IDENTITY, state)
    }

    fn evaluate_with_owner(
        command: &FxCommand,
        clock: f32,
        phase: f32,
        detail_class: u32,
        owner: &[[f32; 4]; 4],
        state: &mut SmokeEmitterState,
    ) -> Vec<EffectQuadFrame> {
        evaluate_op8_emitter(
            command,
            0,
            owner,
            [0.0, 0.0, 500.0],
            clock,
            phase,
            detail_class,
            Op8DetailThresholdRange::new(1.0, 1.0).unwrap(),
            state,
        )
        .unwrap()
    }

    fn put_u32(body: &mut [u8], offset: usize, value: u32) {
        body[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_f32(body: &mut [u8], offset: usize, value: f32) {
        body[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_vec3(body: &mut [u8], offset: usize, value: [f32; 3]) {
        for (axis, component) in value.into_iter().enumerate() {
            put_f32(body, offset + axis * 4, component);
        }
    }

    fn put_name(body: &mut [u8], offset: usize, value: &str) {
        body[offset..offset + value.len()].copy_from_slice(value.as_bytes());
    }

    fn centroid(frame: &EffectQuadFrame) -> [f32; 3] {
        std::array::from_fn(|axis| {
            frame
                .vertices
                .iter()
                .map(|vertex| vertex[axis])
                .sum::<f32>()
                / 4.0
        })
    }

    fn distance(left: [f32; 3], right: [f32; 3]) -> f32 {
        let delta = std::array::from_fn::<_, 3, _>(|axis| left[axis] - right[axis]);
        (delta[0] * delta[0] + delta[1] * delta[1] + delta[2] * delta[2]).sqrt()
    }

    #[test]
    fn first_update_strict_interval_and_catch_up_match_native_schedule() {
        let command = command("fire_smoke", 30, false);
        let mut state = SmokeEmitterState::new(13, 13, 13);
        assert!(evaluate(&command, 0.0, 0.0, 0, &mut state)
            .iter()
            .all(|frame| !frame.active));
        assert_eq!(state.next_slot(), 0);
        assert_eq!(state.last_spawn, 0.0);
        assert_eq!(
            evaluate(&command, 0.08, 0.0, 0, &mut state)
                .iter()
                .filter(|frame| frame.active)
                .count(),
            0
        );
        assert_eq!(state.last_spawn, 0.08);
        assert_eq!(
            evaluate(&command, 0.16, 0.0, 0, &mut state)
                .iter()
                .filter(|frame| frame.active)
                .count(),
            0
        );
        assert_eq!(
            evaluate(&command, 0.160_001, 0.0, 0, &mut state)
                .iter()
                .filter(|frame| frame.active)
                .count(),
            1
        );
        assert_eq!(
            evaluate(&command, 0.320_001, 0.0, 0, &mut state)
                .iter()
                .filter(|frame| frame.active)
                .count(),
            3
        );
        assert_eq!(state.next_slot(), 3);
    }

    #[test]
    fn b_sphere_start_uses_normalized_due_interval_and_raw_schedule_step() {
        let mut command = command("B_Sphere_Start", 6, true);
        let body = Arc::make_mut(&mut command.raw_body);
        // These values are from effects.rlb / B_Sphere_Start command 14.
        put_f32(body, 0x0C, 0.269);
        put_f32(body, 0x10, 0.885);
        put_f32(body, 0x14, 0.05);
        put_f32(body, 0x18, 0.01);
        let mut state = SmokeEmitterState::new(17, 29, 43);

        evaluate(&command, 0.01, 0.5, 0, &mut state);
        evaluate(&command, 0.09, 0.5, 0, &mut state);

        // At phase .5, native normalizes to .375 and tests events at .035,
        // then .065 relative to the captured .01 base. After each event it
        // advances the schedule base by the raw-phase interval .03.
        assert!((state.last_spawn - 0.07).abs() < 1e-6);
        assert_eq!(state.next_slot(), 2);
        assert!((state.slots[1].as_ref().unwrap().born_at - 0.045).abs() < 1e-6);
        assert!((state.slots[2].as_ref().unwrap().born_at - 0.075).abs() < 1e-6);
    }

    #[test]
    fn equal_window_phase_matches_native_unordered_and_clamp_comparisons() {
        assert_eq!(super::normalized_window_phase(0.269, 0.269, 0.269), 0.0);
        assert_eq!(super::normalized_window_phase(0.268, 0.269, 0.269), 0.0);
        assert_eq!(super::normalized_window_phase(0.270, 0.269, 0.269), 1.0);
    }

    #[test]
    fn clock_rewind_clamps_only_schedule_base() {
        let command = command("smoke_yg", 6, true);
        let mut state = SmokeEmitterState::new(17, 29, 43);
        evaluate(&command, 0.01, 0.5, 0, &mut state);
        evaluate(&command, 0.090_001, 0.5, 0, &mut state);

        let cursor = state.cursor;
        let born_at = state.slots[1].as_ref().unwrap().born_at;
        let emitter_rng = state.emitter_rng;
        let signed_rng = state.signed_rng;
        let material_rng = state.material_rng;
        assert!((state.last_spawn - 0.09).abs() < 1e-6);

        evaluate(&command, 0.04, 0.5, 0, &mut state);

        assert_eq!(state.last_spawn, 0.04);
        assert_eq!(state.cursor, cursor);
        assert_eq!(state.slots[1].as_ref().unwrap().born_at, born_at);
        assert_eq!(state.emitter_rng, emitter_rng);
        assert_eq!(state.signed_rng, signed_rng);
        assert_eq!(state.material_rng, material_rng);
    }

    #[test]
    fn catch_up_requires_advancing_base_and_honors_validation_bound() {
        assert_eq!(super::catch_up_count(0.0, 1.0, 0.5, 0.25), Some(2));
        assert!(super::catch_up_count(0.0, 200_001.0, 1.0, 1.0).is_none());
        assert!(super::catch_up_count(16_777_216.0, 16_777_220.0, 0.01, 0.01).is_none());
    }

    #[test]
    fn moving_owner_advances_from_the_last_emitted_anchor() {
        let command = command("smoke_yg", 6, true);
        let owner_start = IDENTITY;
        let mut owner_end = [
            [0.0, -1.0, 0.0, 0.0],
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let mut owner_later = IDENTITY;
        owner_end[0][3] = 10.0;
        owner_later[0][3] = 20.0;
        let mut state = SmokeEmitterState::new(17, 17, 17);

        evaluate_with_owner(&command, 0.01, 0.0, 0, &owner_start, &mut state);
        evaluate_with_owner(&command, 0.05, 0.5, 0, &owner_end, &mut state);
        assert_eq!(state.stored_anchor.unwrap(), [0.0, 0.0, 0.0]);

        // No scheduled event has happened, so the stored anchor stays at the
        // initialization pose. Native divides the full displacement by the
        // time from that anchor's schedule base when an event becomes due.
        evaluate_with_owner(&command, 0.110_001, 0.5, 0, &owner_end, &mut state);
        let slot = state.slots[1].as_ref().unwrap();
        assert!((slot.owner_matrix[0][3] - 8.0).abs() < 1e-4);
        assert_eq!(
            state.stored_anchor.unwrap(),
            [slot.owner_matrix[0][3], 0.0, 0.0]
        );
        // The native slot frame uses motion direction and current owner Z.
        // The yawed owner with X-directed motion yields an identity basis.
        assert_eq!(
            slot.owner_matrix[0],
            [1.0, 0.0, 0.0, slot.owner_matrix[0][3]]
        );
        assert_eq!(slot.owner_matrix[1], [0.0, 1.0, 0.0, 0.0]);
        assert_eq!(slot.owner_matrix[2], [0.0, 0.0, 1.0, 0.0]);

        let frames = evaluate_with_owner(&command, 0.12, 0.6, 0, &owner_later, &mut state);
        let slot = state.slots[1].as_ref().unwrap();
        let local_position = super::sample_track(slot.position, frames[1].normalized_time).unwrap();
        // A no-event update leaves the previously emitted anchor unchanged.
        assert!((slot.owner_matrix[0][3] - 8.0).abs() < 1e-4);
        assert!((state.stored_anchor.unwrap()[0] - 8.0).abs() < 1e-4);
        let center_x = centroid(&frames[1])[0];
        assert!((center_x - (slot.owner_matrix[0][3] + local_position[0])).abs() < 1e-4);
        assert!((center_x - (owner_later[0][3] + local_position[0])).abs() > 10.0);

        let frames = evaluate_with_owner(&command, 0.250_001, 0.7, 0, &owner_later, &mut state);
        assert!((state.slots[2].as_ref().unwrap().born_at - 0.17).abs() < 1e-6);
        assert!((state.slots[3].as_ref().unwrap().born_at - 0.25).abs() < 1e-6);
        // Both catch-up particles use the single velocity computed from the
        // stored X=8 anchor and the original schedule base, stepping by .08.
        assert!((state.slots[2].as_ref().unwrap().owner_matrix[0][3] - 14.0).abs() < 1e-3);
        assert!((state.slots[3].as_ref().unwrap().owner_matrix[0][3] - 20.0).abs() < 1e-3);
        assert!(frames[2].active && frames[3].active);
    }

    #[test]
    fn moving_owner_uses_native_zero_motion_and_parallel_axis_fallbacks() {
        let command = command("smoke_yg", 6, true);
        let previous = IDENTITY;
        let mut current = IDENTITY;
        // Tiny nonzero movement must remain a velocity, not take the exact
        // zero-motion branch. A parallel owner-up axis then exercises 38D0.
        current[0][3] = 0.000_01;
        current[0][2] = 1.0;
        current[2][2] = 0.0;
        current[0][0] = 0.0;
        current[1][0] = 1.0;
        current[2][0] = 0.0;
        let mut state = SmokeEmitterState::new(19, 19, 19);
        evaluate_with_owner(&command, 0.01, 0.0, 0, &previous, &mut state);
        evaluate_with_owner(&command, 0.090_001, 0.5, 0, &current, &mut state);
        let matrix = state.slots[1].as_ref().unwrap().owner_matrix;
        assert_eq!([matrix[0][0], matrix[1][0], matrix[2][0]], [1.0, 0.0, 0.0]);
        assert_eq!([matrix[0][1], matrix[1][1], matrix[2][1]], [0.0, 1.0, 0.0]);
        assert_eq!([matrix[0][2], matrix[1][2], matrix[2][2]], [0.0, 0.0, 1.0]);

        // Exactly zero movement uses the current owner X column, matching the
        // separate 11B83 branch before the 38D0 parallel-axis fallback.
        let mut static_owner = IDENTITY;
        static_owner[0][0] = 0.0;
        static_owner[1][0] = 1.0;
        static_owner[2][0] = 0.0;
        let mut stationary = SmokeEmitterState::new(23, 23, 23);
        evaluate_with_owner(&command, 0.01, 0.0, 0, &previous, &mut stationary);
        evaluate_with_owner(&command, 0.090_001, 0.5, 0, &static_owner, &mut stationary);
        let matrix = stationary.slots[1].as_ref().unwrap().owner_matrix;
        assert_eq!([matrix[0][0], matrix[1][0], matrix[2][0]], [0.0, 1.0, 0.0]);
        assert_eq!([matrix[0][1], matrix[1][1], matrix[2][1]], [-1.0, 0.0, 0.0]);
        assert_eq!([matrix[0][2], matrix[1][2], matrix[2][2]], [0.0, 0.0, 1.0]);
        assert!((matrix[1][3] - 0.08).abs() < 1e-6);
        assert_eq!(stationary.stored_anchor.unwrap(), [0.0, 0.08, 0.0]);
    }

    #[test]
    fn particles_drain_through_the_ring_and_keep_fading_after_the_window() {
        let mut command = command("smoke_yg", 6, true);
        put_f32(Arc::make_mut(&mut command.raw_body), 0x10, 0.5);
        let mut state = SmokeEmitterState::new(31, 31, 31);

        evaluate(&command, 0.01, 0.25, 0, &mut state);
        assert_eq!(
            evaluate(&command, 0.090_001, 0.25, 0, &mut state)
                .iter()
                .filter(|frame| frame.active)
                .count(),
            1
        );

        let tail = evaluate(&command, 0.170_001, 0.75, 0, &mut state);
        assert_eq!(tail.iter().filter(|frame| frame.active).count(), 1);
        assert!(tail[1].color[3] > 0.0 && tail[1].color[3] < 1.0);

        // Out-of-window events still advance the native ring. This particle
        // remains alive until the cursor reaches its slot on the sixth tick.
        let before_wrap = evaluate(&command, 0.490_001, 0.75, 0, &mut state);
        assert_eq!(before_wrap.iter().filter(|frame| frame.active).count(), 1);
        assert!(before_wrap[1].active);
        assert!(before_wrap[1].color[3] < tail[1].color[3]);

        let drained = evaluate(&command, 0.570_001, 0.75, 0, &mut state);
        assert!(drained.iter().all(|frame| !frame.active));
        assert!(state.slots.iter().all(Option::is_none));
    }

    #[test]
    fn generic_six_slot_smoke_yg_body_samples_tracks_opacity_and_phase() {
        let mut command = command("smoke_yg", 6, true);
        let body = Arc::make_mut(&mut command.raw_body);
        put_f32(body, 0x00, 0.25);
        put_f32(body, 0x04, 0.75);
        put_f32(body, 0x08, 2.0);
        put_f32(body, 0x1C, -1.0);
        put_f32(body, 0x14, 0.08);
        put_f32(body, 0x18, 0.08);
        put_f32(body, 0x44, 0.0);
        put_vec3(body, 0x48, [1.0; 3]);
        put_vec3(body, 0x24, [0.0, 0.0, 3.0]);
        put_vec3(body, 0x30, [0.0, 0.0, 3.0]);
        put_vec3(body, 0x54, [0.0, 0.0, 3.0]);
        put_vec3(body, 0x60, [0.0, 0.0, 13.0]);
        put_vec3(body, 0x6C, [0.0, 0.0, 6.0]);
        put_vec3(body, 0x78, [1.0, 1.0, 0.5]);
        put_vec3(body, 0x84, [0.5, 0.75, 1.0]);
        put_vec3(body, 0x90, [1.5, 1.75, 2.0]);
        put_vec3(body, 0x9C, [0.0; 3]);
        put_vec3(body, 0xA8, [1.0, 2.0, 3.0]);

        let mut state = SmokeEmitterState::new(17, 29, 43);
        let first = evaluate(&command, 0.01, 0.0, 1, &mut state);
        assert_eq!(first.len(), 6);
        assert!(first.iter().all(|frame| !frame.active));
        let frames = evaluate(&command, 0.090_001, 0.5, 1, &mut state);
        let frame = &frames[1];
        let particle = state.slots[1].as_ref().unwrap();
        let phase: f32 = (0.090_001 - 0.09) / (0.08 * 6.0);
        let position: [f32; 3] = std::array::from_fn(|axis| {
            particle.position.start[axis]
                + (particle.position.end[axis] - particle.position.start[axis])
                    * phase.powf(particle.position.exponent[axis])
        });
        let scale: [f32; 3] = std::array::from_fn(|axis| {
            particle.scale.start[axis]
                + (particle.scale.end[axis] - particle.scale.start[axis])
                    * phase.powf(particle.scale.exponent[axis])
        });
        let expected_center =
            crate::fx_visual::transform_native_effect_point(&particle.owner_matrix, position)
                .unwrap();
        let expected_opacity = 0.25 + (0.75 - 0.25) * phase.powf(2.0);
        let camera_local =
            crate::fx_visual::native_effect_local_point(&particle.owner_matrix, [0.0, 0.0, 500.0])
                .unwrap();
        let view = std::array::from_fn(|axis| camera_local[axis] - position[axis]);
        let local_basis = super::native_camera_facing_basis(view, position).unwrap();
        let expected_edge_length = |basis_column| {
            let edge: [f32; 3] = std::array::from_fn(|row| {
                (0..3)
                    .map(|axis| {
                        particle.owner_matrix[row][axis]
                            * scale[axis]
                            * local_basis[axis][basis_column]
                    })
                    .sum::<f32>()
            });
            (edge[0] * edge[0] + edge[1] * edge[1] + edge[2] * edge[2]).sqrt()
        };

        assert!(frame.active);
        assert_eq!(frame.material.name, "smoke_yg");
        assert!((frame.normalized_time - phase).abs() < 1e-6);
        assert!((frame.material_phase - particle.random_phase).abs() < 1e-6);
        assert!((frame.color[3] - expected_opacity).abs() < 1e-6);
        for (actual, expected) in centroid(frame).into_iter().zip(expected_center) {
            assert!(
                (actual - expected).abs() < 1e-4,
                "actual={actual}, expected={expected}"
            );
        }
        assert!(
            (distance(frame.vertices[0], frame.vertices[3]) - expected_edge_length(0)).abs() < 1e-4
        );
        assert!(
            (distance(frame.vertices[0], frame.vertices[1]) - expected_edge_length(1)).abs() < 1e-4
        );
    }

    #[test]
    fn mission02_breath_anl_03_op8_fixture_uses_shared_body_fields() {
        let mut command = command("smoke_yg", 6, true);
        let body = Arc::make_mut(&mut command.raw_body);
        put_f32(body, 0x44, 0.0);
        put_vec3(body, 0x54, [0.0, 0.0, 1.0]);
        put_vec3(body, 0x60, [0.0, 0.0, 3.0]);
        put_vec3(body, 0x6C, [3.0, 2.0, 1.0]);
        put_vec3(body, 0x78, [1.0; 3]);
        put_vec3(body, 0x84, [4.0; 3]);
        put_vec3(body, 0x90, [5.0; 3]);
        put_vec3(body, 0x9C, [1.0; 3]);
        put_vec3(body, 0xA8, [1.0; 3]);

        let mut state = SmokeEmitterState::new(17, 29, 43);
        evaluate(&command, 0.01, 0.0, 1, &mut state);
        let frames = evaluate(&command, 0.090_001, 0.5, 1, &mut state);
        let frame = &frames[1];
        let particle = state.slots[1].as_ref().unwrap();
        let phase: f32 = (0.090_001 - 0.09) / (0.08 * 6.0);
        let position = super::sample_track(particle.position, phase).unwrap();

        assert!(frame.active);
        assert_eq!(frame.material.name, "smoke_yg");
        assert!((frame.normalized_time - phase).abs() < 1e-6);
        assert!((frame.material_phase - phase).abs() < 1e-6);
        assert!((frame.color[3] - (1.0 - phase)).abs() < 1e-6);
        let expected_center =
            crate::fx_visual::transform_native_effect_point(&particle.owner_matrix, position)
                .unwrap();
        for (actual, expected) in centroid(frame).into_iter().zip(expected_center) {
            assert!((actual - expected).abs() < 1e-4);
        }
    }

    #[test]
    fn generic_body_uses_dynamic_resource_pool_and_wraps_ring() {
        let command = command("cloud_B2_ADD", 6, false);
        let mut left = SmokeEmitterState::new(77, 77, 77);
        let mut right = SmokeEmitterState::new(77, 77, 77);
        evaluate(&command, 0.01, 0.0, 0, &mut left);
        evaluate(&command, 0.01, 0.0, 0, &mut right);
        let left_frames = evaluate(&command, 0.570_001, 0.0, 0, &mut left);
        let right_frames = evaluate(&command, 0.570_001, 0.0, 0, &mut right);
        assert_eq!(left.pool_len(), 6);
        assert_eq!(left.next_slot(), 1);
        assert_eq!(left_frames, right_frames);
        assert_eq!(left_frames[0].material.name, "cloud_B2_ADD");
    }

    #[test]
    fn emitter_position_track_moves_birth_anchor_from_start_to_randomized_end() {
        let mut command = command("smoke_yg", 6, true);
        let body = Arc::make_mut(&mut command.raw_body);
        put_vec3(body, 0x24, [1.0, 2.0, 3.0]);
        put_vec3(body, 0x30, [11.0, 22.0, 33.0]);
        put_vec3(body, 0x3C, [4.0, 6.0, 8.0]);
        put_vec3(body, 0x48, [1.0, 2.0, 3.0]);

        let uniform_seed = 0x0001_0000;
        let signed_seed = 0x0002_0000;
        let mut state = SmokeEmitterState::new(uniform_seed, signed_seed, 1);
        evaluate(&command, 0.01, 0.0, 0, &mut state);
        let track = state.emitter_position.unwrap();
        let signed = |word: f32, width: f32| (word / 65_536.0 - 0.5) * width;
        let expected_end = [
            11.0 + signed(6.0, 4.0),
            22.0 + signed(3.0, 6.0),
            33.0 + 1.0 / 65_536.0 * 8.0 - 4.0,
        ];
        assert_eq!(track.start, [1.0, 2.0, 3.0]);
        for (actual, expected) in track.end.into_iter().zip(expected_end) {
            assert!((actual - expected).abs() < 1e-6);
        }
        let mut expected_uniform_rng = super::NativeLfsr16Pair::from_word(uniform_seed);
        let mut expected_signed_rng = super::NativeLfsr16Pair::from_word(signed_seed);
        expected_uniform_rng.next_unit();
        expected_signed_rng.next_signed(6.0);
        expected_signed_rng.next_signed(4.0);
        assert_eq!(state.emitter_rng, expected_uniform_rng);
        assert_eq!(state.signed_rng, expected_signed_rng);

        // The midpoint uses each component's own exponent, and the sampled
        // offset is added to the owner translation before the slot is born.
        let midpoint = super::sample_track(track, 0.5).unwrap();
        let mut state = SmokeEmitterState::new(uniform_seed, signed_seed, 1);
        evaluate(&command, 0.01, 0.0, 0, &mut state);
        let mut owner = IDENTITY;
        owner[0][3] = 5.0;
        let frames = evaluate_with_owner(&command, 0.090_001, 0.5, 0, &owner, &mut state);
        let particle = state.slots[1].as_ref().unwrap();
        let expected_position_z = 50.0 + 4.0 / 65_536.0 * 30.0 - 15.0;
        assert!((particle.position.end[2] - expected_position_z).abs() < 1e-6);
        // The spawn is almost at the current update, whose owner anchor is
        // the trajectory sample at phase 0.5.
        for axis in 0..3 {
            assert!(
                (particle.owner_matrix[axis][3] - (owner[axis][3] + midpoint[axis])).abs() < 1e-3
            );
        }
        let local_position =
            super::sample_track(particle.position, frames[1].normalized_time).unwrap();
        let expected_center =
            crate::fx_visual::transform_native_effect_point(&particle.owner_matrix, local_position)
                .unwrap();
        for (actual, expected) in centroid(&frames[1]).into_iter().zip(expected_center) {
            assert!((actual - expected).abs() < 1e-3);
        }
    }

    #[test]
    fn detail_gate_is_inclusive_and_rejects_the_next_float() {
        assert!(detail_class_passes_threshold(3.0, 3.0));
        let next_float = f32::from_bits(3.0_f32.to_bits() + 1);
        assert!(!detail_class_passes_threshold(next_float, 3.0));
        assert!(detail_class_passes_threshold(0.0, 0.0));
    }

    #[test]
    fn rejects_malformed_or_oversized_op8_bodies_before_state_changes() {
        let command = FxCommand {
            word: 8,
            opcode: FxOpcode::Op8,
            native_flag: false,
            raw_body: Arc::from(vec![0_u8; 32]),
            resource_refs: Vec::new(),
        };
        let mut state = SmokeEmitterState::new(13, 13, 13);
        assert!(evaluate_op8_emitter(
            &command,
            0,
            &IDENTITY,
            [0.0, 0.0, 500.0],
            1.0,
            0.0,
            0,
            Op8DetailThresholdRange::new(0.0, 3.0).unwrap(),
            &mut state,
        )
        .is_none());
        assert_eq!(state.pool_len(), 0);
        assert_eq!(state.next_slot(), 0);
    }
}
