//! Native-shaped evaluation for opcode-7 particle pools.
//!
//! Op7 is a fixed pool of independently randomized position and scale tracks.
//! It has a different lifecycle from the scheduled ring used by Op8.

use crate::environment::EffectResource;
use crate::fx_visual::{
    compose_native_effect_matrix, native_camera_facing_basis, native_effect_local_point,
    transform_native_effect_quad, EffectQuadFrame, EFFECT_QUAD_UV,
};
use crate::{FxCommand, FxOpcode};
use std::sync::Arc;

const RESOURCE_OFFSET: usize = 0x8C;
const RESOURCE_BYTES: usize = 64;
const MIN_BODY_SIZE: usize = RESOURCE_OFFSET + RESOURCE_BYTES;
const MAX_PARTICLES: usize = 4096;
const U16_TO_UNIT: f32 = 1.0 / 65_536.0;

#[derive(Clone, Copy, Debug)]
struct Track {
    start: [f32; 3],
    end: [f32; 3],
    exponent: [f32; 3],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Lfsr16Pair {
    state: u32,
}

impl Lfsr16Pair {
    fn from_word(state: u32) -> Self {
        Self { state }
    }

    fn next_unit(&mut self) -> f32 {
        f32::from(crate::advance_native_fx_random_state(&mut self.state)) * U16_TO_UNIT
    }

    fn next_signed(&mut self, width: f32) -> f32 {
        (self.next_unit() - 0.5) * width
    }
}

#[derive(Clone, Debug)]
struct Particle {
    group: usize,
    helper_mode: u32,
    visibility_cutoff: f32,
    position: Track,
    scale: Track,
}

/// Mutable deterministic state for one Op7 command instance.
///
/// Native Op7 initializes all slots once and then samples their fixed tracks;
/// retain one state per command instance and call [`evaluate_op7_emitter`] on
/// each update. The seed only supplies deterministic stand-ins for the
/// process-global native random streams.
#[derive(Clone, Debug)]
pub struct Op7EmitterState {
    initial_particle_rng: Lfsr16Pair,
    initial_visibility_rng: Lfsr16Pair,
    particle_rng: Lfsr16Pair,
    visibility_rng: Lfsr16Pair,
    body: Option<Arc<[u8]>>,
    visibility_profile: Option<Op7VisibilityProfile>,
    particles: Vec<Particle>,
    last_clock: Option<f32>,
    group_clocks: Vec<f32>,
}

impl Op7EmitterState {
    /// Creates deterministic evaluator state from particle and visibility seeds.
    #[must_use]
    pub fn new(particle_seed: u32, visibility_seed: u32) -> Self {
        let initial_particle_rng = Lfsr16Pair::from_word(particle_seed);
        let initial_visibility_rng = Lfsr16Pair::from_word(visibility_seed);
        Self {
            initial_particle_rng,
            initial_visibility_rng,
            particle_rng: initial_particle_rng,
            visibility_rng: initial_visibility_rng,
            body: None,
            visibility_profile: None,
            particles: Vec::new(),
            last_clock: None,
            group_clocks: Vec::new(),
        }
    }

    /// Clears initialized slots and restores the initial random streams.
    fn reset(&mut self) {
        self.particle_rng = self.initial_particle_rng;
        self.visibility_rng = self.initial_visibility_rng;
        self.body = None;
        self.visibility_profile = None;
        self.particles.clear();
        self.last_clock = None;
        self.group_clocks.clear();
    }
}

/// Native Op7's per-slot visibility range from the effect settings tables.
///
/// `width` and `bias` are the pair selected by the caller's native settings
/// context (`settings_id >> 8`) for helper 0xEC50. The sampled cutoff is
/// `random_unit * width - bias`; this is a detail cutoff, not a time offset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Op7VisibilityProfile {
    /// Width from the native settings table.
    pub width: f32,
    /// Bias from the native settings table.
    pub bias: f32,
}

/// Per-render context consumed by the native Op7 visibility gate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Op7RenderContext {
    /// Ordinal returned by the native render-context detail selector.
    /// The native outer dispatcher bypasses rendering for values >= 8.
    pub detail_level: u32,
    /// Native settings-table pair used to initialize fixed slot cutoffs.
    pub visibility: Op7VisibilityProfile,
    /// Native world scalar returned by Terrain.dll's GetWorld vslot +0x10.
    /// Effect.dll multiplies Op7 body B88 by this value and by 0.5.
    pub world_scalar: f32,
}

#[derive(Clone, Debug)]
struct Params {
    material: EffectResource,
    helper_mode: u32,
    pool_count: usize,
    copies_per_group: usize,
    start: f32,
    end: f32,
    lifetime: f32,
    material_phase_factor: f32,
    vertical_acceleration: f32,
    opacity_start: f32,
    opacity_end: f32,
    opacity_exponent: f32,
    position: Track,
    position_width: [f32; 3],
    scale: Track,
    scale_width: [f32; 3],
}

impl Params {
    fn parse(command: &FxCommand, world_scalar: f32) -> Option<Self> {
        let body = &command.raw_body;
        if body.len() < MIN_BODY_SIZE {
            return None;
        }
        let start = read_f32(body, 0x10)?;
        let end = read_f32(body, 0x14)?;
        let lifetime = read_f32(body, 0x18)?;
        let material_phase_factor = read_f32(body, 0x1C)?;
        let vertical_acceleration = read_f32(body, 0x88)? * world_scalar * 0.5;
        let pool_count = read_u32(body, 0x20)? as usize;
        let copies_per_group = read_u32(body, 0x24)? as usize;
        let opacity_start = read_f32(body, 0x04)?;
        let opacity_end = read_f32(body, 0x08)?;
        let opacity_exponent = read_f32(body, 0x0C)?;
        let position = Track {
            start: read_vec3(body, 0x28)?,
            end: read_vec3(body, 0x34)?,
            exponent: read_vec3(body, 0x4C)?,
        };
        let position_width = read_vec3(body, 0x40)?;
        let scale = Track {
            start: read_vec3(body, 0x58)?,
            end: read_vec3(body, 0x64)?,
            exponent: read_vec3(body, 0x7C)?,
        };
        let scale_width = read_vec3(body, 0x70)?;
        let particle_count = pool_count.checked_mul(copies_per_group)?;
        let mode_field = read_u32(body, 0x00)?;
        let helper_mode = if mode_field == 1 { 3 } else { 0 };
        if start >= end
            || !lifetime.is_finite()
            || lifetime <= 0.0
            || !material_phase_factor.is_finite()
            || material_phase_factor == 0.0
            || !world_scalar.is_finite()
            || !vertical_acceleration.is_finite()
            || pool_count == 0
            || copies_per_group == 0
            || particle_count > MAX_PARTICLES
            || ![start, end, opacity_start, opacity_end, opacity_exponent]
                .iter()
                .all(|value| value.is_finite())
            || !position
                .start
                .iter()
                .chain(position.end.iter())
                .chain(position.exponent.iter())
                .chain(position_width.iter())
                .chain(scale.start.iter())
                .chain(scale.end.iter())
                .chain(scale.exponent.iter())
                .chain(scale_width.iter())
                .all(|value| value.is_finite())
        {
            return None;
        }
        Some(Self {
            material: read_resource(body)?,
            helper_mode,
            pool_count,
            copies_per_group,
            start,
            end,
            lifetime,
            material_phase_factor,
            vertical_acceleration,
            opacity_start,
            opacity_end,
            opacity_exponent,
            position,
            position_width,
            scale,
            scale_width,
        })
    }
}

/// Evaluates an Op7 fixed particle pool at one effect phase.
///
/// `effect_phase` is the native command clock used by the inclusive body
/// window. `world_from_effect` is the owner transform after the FX header
/// transform, and `camera_world` is the render context point used by the
/// particle billboard. Frames are returned only for slots whose staggered
/// lifetime is currently active.
#[must_use]
pub fn evaluate_op7_emitter(
    command: &FxCommand,
    command_index: usize,
    world_from_effect: &[[f32; 4]; 4],
    camera_world: [f32; 3],
    effect_phase: f32,
    elapsed_seconds: f32,
    render_context: Op7RenderContext,
    state: &mut Op7EmitterState,
) -> Option<Vec<EffectQuadFrame>> {
    if command.opcode != FxOpcode::Op7
        || !world_from_effect
            .iter()
            .flatten()
            .all(|value| value.is_finite())
        || !camera_world.iter().all(|value| value.is_finite())
        || !effect_phase.is_finite()
        || !elapsed_seconds.is_finite()
        || !render_context.visibility.width.is_finite()
        || !render_context.visibility.bias.is_finite()
        || !render_context.world_scalar.is_finite()
        || render_context.visibility.width < 0.0
    {
        return None;
    }
    let params = Params::parse(command, render_context.world_scalar)?;
    if state
        .body
        .as_deref()
        .is_some_and(|body| body != command.raw_body.as_ref())
    {
        state.reset();
    }
    if state
        .visibility_profile
        .is_some_and(|profile| profile != render_context.visibility)
    {
        // The native base class samples these cutoffs once during object
        // initialization. A caller changing the settings pair mid-emitter is
        // not the same native state, so reject that frame instead of silently
        // rerolling particle and visibility RNGs.
        return None;
    }
    state.body = Some(Arc::clone(&command.raw_body));
    if state.particles.is_empty() {
        initialize_particles(&params, render_context.visibility, state)?;
        state.visibility_profile = Some(render_context.visibility);
    }

    let frame_delta = elapsed_seconds - state.last_clock.unwrap_or(0.0);
    if !frame_delta.is_finite() {
        return None;
    }
    let in_window = effect_phase >= params.start && effect_phase <= params.end;
    state.last_clock = Some(elapsed_seconds);

    if render_context.detail_level >= 8 || !in_window {
        return Some(Vec::new());
    }
    let window_phase = (effect_phase - params.start) / (params.end - params.start);
    let mut frames = Vec::with_capacity(params.particles_len());
    let mut group_contexts = Vec::with_capacity(params.pool_count);
    for group in 0..params.pool_count {
        let stagger = (1.0 - params.lifetime) * group as f32 / params.pool_count as f32;
        let group_age = (window_phase - stagger) / params.lifetime;
        if !(0.0..=1.0).contains(&group_age) {
            group_contexts.push(None);
            continue;
        }
        let mut owner_matrix = *world_from_effect;
        if params.vertical_acceleration != 0.0 {
            let group_clock = *state.group_clocks.get(group)?;
            let delta_z = group_clock * group_clock * params.vertical_acceleration;
            if !delta_z.is_finite() {
                return None;
            }
            // The native update accumulator updates the emitter's bounds
            // position, while the renderer applies only this group's clock
            // to its copied owner matrix.
            owner_matrix[2][3] -= delta_z;
            let group_clock = state.group_clocks.get_mut(group)?;
            *group_clock += frame_delta;
            if !group_clock.is_finite() {
                return None;
            }
        }
        let camera_local = native_effect_local_point(&owner_matrix, camera_world)?;
        group_contexts.push(Some((owner_matrix, camera_local)));
    }
    for particle in &state.particles {
        // Renderer 01300 converts context+0x60's DWORD ordinal with FILD and
        // skips this slot only when the ordinal compares greater than the
        // native per-slot float in state+0x2c.
        if render_context.detail_level as f32 > particle.visibility_cutoff {
            continue;
        }
        // Native 01000 spaces group starts by (1 - lifetime) / pool_count;
        // the second pool dimension shares its group's start phase.
        let stagger = (1.0 - params.lifetime) * particle.group as f32 / params.pool_count as f32;
        let age = (window_phase - stagger) / params.lifetime;
        if !(0.0..=1.0).contains(&age) {
            continue;
        }
        let Some(position) = sample_track(particle.position, age) else {
            return None;
        };
        let Some(scale) = sample_track(particle.scale, age) else {
            return None;
        };
        let Some((owner_matrix, camera_local)) = group_contexts.get(particle.group)?.as_ref()
        else {
            continue;
        };
        let camera_local = *camera_local;
        let opacity = params.opacity_start
            + (params.opacity_end - params.opacity_start) * age.powf(params.opacity_exponent);
        if !opacity.is_finite() {
            return None;
        }
        let Some(local_basis) =
            native_mode_basis(particle.helper_mode, owner_matrix, camera_local, position)
        else {
            continue;
        };
        let matrix = compose_native_effect_matrix(owner_matrix, &local_basis, position, scale)?;
        let vertices = transform_native_effect_quad(&matrix)?;
        frames.push(EffectQuadFrame {
            command_index,
            opcode: FxOpcode::Op7,
            material: params.material.clone(),
            vertices,
            uv: EFFECT_QUAD_UV,
            color: [1.0, 1.0, 1.0, opacity.clamp(0.0, 1.0)],
            // Native material timing uses the normalized effect-window phase
            // minus the group offset. It does not reuse the lifetime-scaled
            // particle age used by position, scale, and opacity.
            material_phase: native_material_phase(
                window_phase - stagger,
                params.material_phase_factor,
            )?,
            elapsed_seconds,
            normalized_time: age,
            active: true,
            native_flag_base: None,
        });
    }
    Some(frames)
}

impl Params {
    fn particles_len(&self) -> usize {
        self.pool_count * self.copies_per_group
    }
}

fn initialize_particles(
    params: &Params,
    visibility: Op7VisibilityProfile,
    state: &mut Op7EmitterState,
) -> Option<()> {
    state.particles.reserve(params.particles_len());
    state.group_clocks.resize(params.pool_count, 0.0);
    for group in 0..params.pool_count {
        for _ in 0..params.copies_per_group {
            state.particles.push(Particle {
                group,
                helper_mode: params.helper_mode,
                visibility_cutoff: state.visibility_rng.next_unit() * visibility.width
                    - visibility.bias,
                position: randomize_track(
                    params.position,
                    params.position_width,
                    &mut state.particle_rng,
                )?,
                scale: randomize_track(params.scale, params.scale_width, &mut state.particle_rng)?,
            });
        }
    }
    Some(())
}

fn randomize_track(
    mut track: Track,
    width: [f32; 3],
    particle_rng: &mut Lfsr16Pair,
) -> Option<Track> {
    // Native 01720 advances the same process-global state for Z, Y, and X;
    // scale randomization continues that stream after position randomization.
    track.end[2] += particle_rng.next_unit() * width[2] - width[2] * 0.5;
    track.end[1] += particle_rng.next_signed(width[1]);
    track.end[0] += particle_rng.next_signed(width[0]);
    track
        .start
        .iter()
        .chain(track.end.iter())
        .chain(track.exponent.iter())
        .all(|value| value.is_finite())
        .then_some(track)
}

fn native_material_phase(window_phase: f32, factor: f32) -> Option<f32> {
    let phase = if factor > 1.0 {
        (window_phase * factor).fract()
    } else {
        window_phase / factor
    };
    phase
        .is_finite()
        .then_some(if phase >= 1.0 { 0.99 } else { phase })
}

fn sample_track(track: Track, phase: f32) -> Option<[f32; 3]> {
    if !phase.is_finite() || !(0.0..=1.0).contains(&phase) {
        return None;
    }
    let mut result = [0.0; 3];
    for axis in 0..3 {
        let curve = phase.powf(track.exponent[axis]);
        result[axis] = track.start[axis] + (track.end[axis] - track.start[axis]) * curve;
    }
    result
        .iter()
        .all(|value| value.is_finite())
        .then_some(result)
}

fn native_mode_basis(
    helper_mode: u32,
    world_from_effect: &[[f32; 4]; 4],
    camera_local: [f32; 3],
    position: [f32; 3],
) -> Option<[[f32; 4]; 4]> {
    match helper_mode {
        0 => {
            let view = subtract(camera_local, position);
            native_camera_facing_basis(view, position)
        }
        3 => native_mode_three_basis(world_from_effect, camera_local, position),
        _ => None,
    }
}

/// Reproduces Op7's native helper-mode-3 basis selection (Effect.dll 9575,
/// with 038D0 as its degenerate-cross fallback).
fn native_mode_three_basis(
    world_from_effect: &[[f32; 4]; 4],
    camera_local: [f32; 3],
    position: [f32; 3],
) -> Option<[[f32; 4]; 4]> {
    // The slot helper's +0x54 point is initialized to zero by 092D0. Native
    // FastProc+0x68 projects that world point through the owner matrix before
    // using it as the forward-vector origin.
    let origin_local = native_effect_local_point(world_from_effect, [0.0; 3])?;
    let forward = normalize(subtract(position, origin_local))?;
    let to_camera = subtract(camera_local, position);
    let dot = dot(forward, to_camera);
    if !dot.is_finite() || dot.abs() == 1.0 {
        // The x87 comparison at 9667..9683 clears the helper's active byte for
        // the exact-equality or unordered path.
        return None;
    }

    let right = cross(to_camera, forward);
    let local_right = match normalize(right) {
        Some(right) => right,
        None => mode_three_fallback_right(forward)?,
    };
    Some(matrix_from_forward_right(forward, local_right, position))
}

fn mode_three_fallback_right(forward: [f32; 3]) -> Option<[f32; 3]> {
    let right = if forward[0] == 0.0 {
        [0.0, 1.0, 0.0]
    } else if forward[1] == 0.0 {
        [1.0, 0.0, 0.0]
    } else {
        [-forward[1], forward[0], 0.0]
    };
    if forward[2] != 0.0 && forward[0] != 0.0 && forward[1] != 0.0 {
        normalize(right)
    } else {
        Some(right)
    }
}

fn matrix_from_forward_right(
    forward: [f32; 3],
    right: [f32; 3],
    position: [f32; 3],
) -> [[f32; 4]; 4] {
    let up = cross(forward, right);
    [
        [forward[0], right[0], up[0], position[0]],
        [forward[1], right[1], up[1], position[1]],
        [forward[2], right[2], up[2], position[2]],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn subtract(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

fn dot(left: [f32; 3], right: [f32; 3]) -> f32 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

fn cross(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

fn normalize(vector: [f32; 3]) -> Option<[f32; 3]> {
    if !vector.iter().all(|component| component.is_finite()) {
        return None;
    }
    let length = dot(vector, vector).sqrt();
    if !length.is_finite() || length == 0.0 {
        return None;
    }
    Some([vector[0] / length, vector[1] / length, vector[2] / length])
}

fn read_resource(body: &[u8]) -> Option<EffectResource> {
    let bytes = body.get(RESOURCE_OFFSET..RESOURCE_OFFSET + RESOURCE_BYTES)?;
    let archive = read_name(&bytes[..32])?;
    let name = read_name(&bytes[32..])?;
    (!archive.is_empty() && !name.is_empty()).then_some(EffectResource { archive, name })
}

fn read_name(bytes: &[u8]) -> Option<String> {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    Some(std::str::from_utf8(&bytes[..end]).ok()?.trim().to_owned())
}

fn read_u32(body: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        body.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn read_f32(body: &[u8], offset: usize) -> Option<f32> {
    Some(f32::from_le_bytes(
        body.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn read_vec3(body: &[u8], offset: usize) -> Option<[f32; 3]> {
    Some([
        read_f32(body, offset)?,
        read_f32(body, offset + 4)?,
        read_f32(body, offset + 8)?,
    ])
}

#[cfg(test)]
mod tests {
    use super::{
        evaluate_op7_emitter, Lfsr16Pair, Op7EmitterState, Op7RenderContext, Op7VisibilityProfile,
    };
    use crate::{decode_fxid, FxCommand, FxOpcode, FXID_KIND};
    use fparkan_nres::{decode, ReadProfile};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn command() -> FxCommand {
        let mut body = vec![0; 0xCC];
        put_f32(&mut body, 0x04, 1.0);
        put_f32(&mut body, 0x08, 0.0);
        put_f32(&mut body, 0x0C, 1.0);
        put_f32(&mut body, 0x10, 0.05);
        put_f32(&mut body, 0x14, 0.5);
        put_f32(&mut body, 0x18, 0.5);
        put_f32(&mut body, 0x1C, 1.5);
        put_u32(&mut body, 0x20, 6);
        put_u32(&mut body, 0x24, 1);
        put_vec3(&mut body, 0x28, [0.0, 0.0, 0.0]);
        put_vec3(&mut body, 0x34, [1.0, 0.0, 6.0]);
        put_vec3(&mut body, 0x40, [1.0, 1.0, 2.0]);
        put_vec3(&mut body, 0x4C, [1.0; 3]);
        put_vec3(&mut body, 0x58, [2.5; 3]);
        put_vec3(&mut body, 0x64, [2.0; 3]);
        put_vec3(&mut body, 0x70, [1.5; 3]);
        put_vec3(&mut body, 0x7C, [1.0; 3]);
        body[0x8C..0x98].copy_from_slice(b"material.lib");
        body[0xAC..0xB4].copy_from_slice(b"smoke_yg");
        FxCommand {
            word: 0x107,
            opcode: FxOpcode::Op7,
            native_flag: true,
            raw_body: Arc::from(body.into_boxed_slice()),
            resource_refs: Vec::new(),
        }
    }

    fn b88_command(pool_count: u32, lifetime: f32) -> FxCommand {
        let mut command = command();
        let mut body = command.raw_body.to_vec();
        put_f32(&mut body, 0x10, 0.0);
        put_f32(&mut body, 0x14, 1.0);
        put_f32(&mut body, 0x18, lifetime);
        put_u32(&mut body, 0x20, pool_count);
        put_u32(&mut body, 0x24, 1);
        put_vec3(&mut body, 0x28, [0.0; 3]);
        put_vec3(&mut body, 0x34, [0.0; 3]);
        put_vec3(&mut body, 0x40, [0.0; 3]);
        put_vec3(&mut body, 0x58, [1.0; 3]);
        put_vec3(&mut body, 0x64, [1.0; 3]);
        put_vec3(&mut body, 0x70, [0.0; 3]);
        put_f32(&mut body, 0x88, 1.0);
        command.raw_body = Arc::from(body.into_boxed_slice());
        command
    }

    fn render_context(detail_level: u32) -> Op7RenderContext {
        Op7RenderContext {
            detail_level,
            // Synthetic profile used to exercise the native ordinal gate.
            // Production callers pass their resolved runtime settings pair.
            visibility: Op7VisibilityProfile {
                width: 3.0,
                bias: 0.0,
            },
            world_scalar: 10.0,
        }
    }

    fn matrix_center_z(frame: &crate::fx_visual::EffectQuadFrame) -> f32 {
        frame.vertices.iter().map(|vertex| vertex[2]).sum::<f32>() / 4.0
    }

    #[test]
    fn op7_lfsr_matches_native_two_half_transition() {
        let mut rng = Lfsr16Pair { state: 0x1234_5678 };
        assert_eq!(rng.next_unit(), f32::from(0xB7DE_u16) / 65_536.0);
        assert_eq!(rng, Lfsr16Pair { state: 0xB7DE_BEC4 });
        assert_eq!(rng.next_unit(), f32::from(0x91B9_u16) / 65_536.0);
        assert_eq!(rng, Lfsr16Pair { state: 0x91B9_CA56 });
    }

    fn put_u32(body: &mut [u8], offset: usize, value: u32) {
        body[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_f32(body: &mut [u8], offset: usize, value: f32) {
        body[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_vec3(body: &mut [u8], offset: usize, value: [f32; 3]) {
        for (index, component) in value.into_iter().enumerate() {
            put_f32(body, offset + index * 4, component);
        }
    }

    fn centroid(frame: &crate::fx_visual::EffectQuadFrame) -> [f32; 3] {
        std::array::from_fn(|axis| {
            frame
                .vertices
                .iter()
                .map(|vertex| vertex[axis])
                .sum::<f32>()
                / 4.0
        })
    }

    #[test]
    fn op7_samples_randomized_tracks_and_native_group_stagger() {
        let command = command();
        let identity = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let mut state = Op7EmitterState::new(17, 29);

        assert!(
            evaluate_op7_emitter(
                &command,
                0,
                &identity,
                [0.0, 0.0, -20.0],
                0.05,
                0.0,
                render_context(0),
                &mut state,
            )
            .unwrap()
            .len()
                <= 1
        );
        let frames = evaluate_op7_emitter(
            &command,
            0,
            &identity,
            [0.0, 0.0, -20.0],
            0.25,
            0.5,
            render_context(0),
            &mut state,
        )
        .unwrap();
        assert_eq!(frames.len(), 6);
        assert!(frames.iter().all(|frame| frame.opcode == FxOpcode::Op7));
        assert!(frames.iter().all(|frame| frame.material.name == "smoke_yg"));
        let first_age = frames[0].normalized_time;
        assert!((first_age - 0.888_888_9).abs() < 1e-5);
        for (left, right) in frames.iter().zip(frames.iter().skip(1)) {
            assert!((left.normalized_time - right.normalized_time - 1.0 / 6.0).abs() < 1e-5);
        }
        assert!((frames[0].color[3] - (1.0 - first_age)).abs() < 1e-5);
        let window_phase = (0.25_f32 - 0.05) / (0.5 - 0.05);
        let native_material_phase = (window_phase * 1.5).fract();
        assert!((frames[0].material_phase - native_material_phase).abs() < 1e-5);
        // Native material timing uses the window phase (about 0.444), not
        // the lifetime-scaled particle age (about 0.889).
        assert!((frames[0].material_phase - first_age).abs() > 0.1);
        assert!(centroid(&frames[0])[2] > 0.0);
    }

    #[test]
    fn op7_b88_uses_world_scalar_and_moves_owner_and_camera_basis() {
        let command = b88_command(1, 1.0);
        let mut unaccelerated = command.clone();
        Arc::make_mut(&mut unaccelerated.raw_body)[0x88..0x8C]
            .copy_from_slice(&0.0_f32.to_le_bytes());
        let identity = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let camera = [10.0, 2.0, 10.0];

        let mut default_state = Op7EmitterState::new(31, 31);
        let default_context = render_context(0);
        let default_first = evaluate_op7_emitter(
            &command,
            0,
            &identity,
            camera,
            0.25,
            0.2,
            default_context,
            &mut default_state,
        )
        .expect("native default world scalar");
        assert_eq!(default_first.len(), 1);
        assert!(matrix_center_z(&default_first[0]).abs() < 1e-6);
        assert!((default_state.group_clocks[0] - 0.2).abs() < 1e-6);
        let default_frames = evaluate_op7_emitter(
            &command,
            0,
            &identity,
            camera,
            0.3,
            0.4,
            default_context,
            &mut default_state,
        )
        .expect("second update renders with prior group clock only");
        assert!((matrix_center_z(&default_frames[0]) + 0.2).abs() < 1e-6);
        assert!((default_state.group_clocks[0] - 0.4).abs() < 1e-6);

        let mut alternate_state = Op7EmitterState::new(31, 31);
        let mut alternate_context = render_context(0);
        alternate_context.world_scalar = 4.0;
        let alternate_first = evaluate_op7_emitter(
            &command,
            0,
            &identity,
            camera,
            0.25,
            0.2,
            alternate_context,
            &mut alternate_state,
        )
        .expect("alternate world scalar");
        assert!(matrix_center_z(&alternate_first[0]).abs() < 1e-6);
        let alternate_frames = evaluate_op7_emitter(
            &command,
            0,
            &identity,
            camera,
            0.3,
            0.4,
            alternate_context,
            &mut alternate_state,
        )
        .expect("alternate scalar renders with prior group clock only");
        assert!((matrix_center_z(&alternate_frames[0]) + 0.08).abs() < 1e-6);

        let mut unaccelerated_state = Op7EmitterState::new(31, 31);
        let unaccelerated_first = evaluate_op7_emitter(
            &unaccelerated,
            0,
            &identity,
            camera,
            0.25,
            0.2,
            default_context,
            &mut unaccelerated_state,
        )
        .expect("zero B88 command");
        let unaccelerated_frames = evaluate_op7_emitter(
            &unaccelerated,
            0,
            &identity,
            camera,
            0.3,
            0.4,
            default_context,
            &mut unaccelerated_state,
        )
        .expect("zero B88 command on second update");
        assert!(matrix_center_z(&unaccelerated_first[0]).abs() < 1e-6);
        let edge = |frame: &crate::fx_visual::EffectQuadFrame| {
            std::array::from_fn::<_, 3, _>(|axis| frame.vertices[3][axis] - frame.vertices[0][axis])
        };
        assert_ne!(edge(&default_frames[0]), edge(&unaccelerated_frames[0]));
    }

    #[test]
    fn op7_group_clocks_advance_only_for_active_groups_and_remain_separate() {
        let command = b88_command(2, 0.5);
        let identity = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let mut state = Op7EmitterState::new(37, 37);
        let context = render_context(0);

        let first = evaluate_op7_emitter(
            &command,
            0,
            &identity,
            [10.0, 2.0, 10.0],
            0.2,
            0.2,
            context,
            &mut state,
        )
        .expect("first active phase");
        assert_eq!(first.len(), 1);
        assert_eq!(state.group_clocks, [0.2, 0.0]);
        assert!(matrix_center_z(&first[0]).abs() < 1e-6);

        let inactive = evaluate_op7_emitter(
            &command,
            0,
            &identity,
            [10.0, 2.0, 10.0],
            1.2,
            0.6,
            context,
            &mut state,
        )
        .expect("outside the active window");
        assert!(inactive.is_empty());
        assert_eq!(state.group_clocks, [0.2, 0.0]);

        let resumed = evaluate_op7_emitter(
            &command,
            0,
            &identity,
            [10.0, 2.0, 10.0],
            0.4,
            0.7,
            context,
            &mut state,
        )
        .expect("re-enter the active window");
        assert_eq!(resumed.len(), 2);
        assert!((state.group_clocks[0] - 0.3).abs() < 1e-6);
        assert!((state.group_clocks[1] - 0.1).abs() < 1e-6);
        assert!((matrix_center_z(&resumed[0]) + 0.2).abs() < 1e-5);
        assert!(matrix_center_z(&resumed[1]).abs() < 1e-6);
    }

    #[test]
    fn op7_active_group_clocks_advance_before_detail_cutoff() {
        let command = b88_command(2, 0.5);
        let identity = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let mut context = render_context(0);
        context.visibility = Op7VisibilityProfile {
            width: 0.0,
            bias: 1.0,
        };
        let mut state = Op7EmitterState::new(37, 37);
        let frames = evaluate_op7_emitter(
            &command,
            0,
            &identity,
            [10.0, 2.0, 10.0],
            0.3,
            0.3,
            context,
            &mut state,
        )
        .unwrap();

        assert!(frames.is_empty());
        assert_eq!(state.group_clocks, [0.3, 0.3]);
    }

    #[test]
    fn op7_group_phases_match_native_activity_at_zero_and_one() {
        let mut command = command();
        let mut body = command.raw_body.to_vec();
        put_f32(&mut body, 0x10, 0.0);
        put_f32(&mut body, 0x14, 1.0);
        put_u32(&mut body, 0x20, 4);
        command.raw_body = Arc::from(body.into_boxed_slice());
        let identity = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let mut state = Op7EmitterState::new(17, 17);
        let mut context = render_context(0);
        context.visibility = Op7VisibilityProfile {
            width: 0.0,
            bias: -1.0,
        };

        let at_zero = evaluate_op7_emitter(
            &command,
            0,
            &identity,
            [0.0, 0.0, -20.0],
            0.0,
            0.0,
            context,
            &mut state,
        )
        .unwrap();
        assert_eq!(at_zero.len(), 1);
        assert!((at_zero[0].normalized_time - 0.0).abs() < 1e-6);

        let before_last_group = evaluate_op7_emitter(
            &command,
            0,
            &identity,
            [0.0, 0.0, -20.0],
            0.3749,
            0.0,
            context,
            &mut state,
        )
        .unwrap();
        assert_eq!(before_last_group.len(), 3);

        let at_last_group = evaluate_op7_emitter(
            &command,
            0,
            &identity,
            [0.0, 0.0, -20.0],
            0.375,
            0.0,
            context,
            &mut state,
        )
        .unwrap();
        assert_eq!(at_last_group.len(), 4);
        assert!(at_last_group
            .iter()
            .any(|frame| frame.normalized_time.abs() < 1e-6));

        let at_one = evaluate_op7_emitter(
            &command,
            0,
            &identity,
            [0.0, 0.0, -20.0],
            0.5,
            0.0,
            context,
            &mut state,
        )
        .unwrap();
        assert_eq!(at_one.len(), 4);
        assert!((at_one[0].normalized_time - 1.0).abs() < 1e-6);

        let after_first_group = evaluate_op7_emitter(
            &command,
            0,
            &identity,
            [0.0, 0.0, -20.0],
            0.5001,
            0.0,
            context,
            &mut state,
        )
        .unwrap();
        assert_eq!(after_first_group.len(), 3);
        assert!(after_first_group
            .iter()
            .all(|frame| frame.normalized_time <= 1.0));
    }

    #[test]
    fn op7_position_and_scale_randomization_share_the_native_lfsr_stream() {
        let mut command = command();
        let mut body = command.raw_body.to_vec();
        put_u32(&mut body, 0x20, 1);
        command.raw_body = Arc::from(body.into_boxed_slice());
        let identity = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let mut state = Op7EmitterState::new(0x1234_5678, 0xBEEF);
        let _ = evaluate_op7_emitter(
            &command,
            0,
            &identity,
            [0.0, 0.0, -20.0],
            0.25,
            0.5,
            render_context(0),
            &mut state,
        )
        .unwrap();
        let particle = &state.particles[0];
        assert!((particle.position.end[2] - 6.436_462_4).abs() < 1e-6);
        assert!((particle.position.end[1] - 0.069_229_126).abs() < 1e-6);
        assert!((particle.position.end[0] - 0.803_848_27).abs() < 1e-6);
        assert!((particle.scale.end[2] - 1.818_519_6).abs() < 1e-6);
        assert!((particle.scale.end[1] - 2.552_292).abs() < 1e-6);
        assert!((particle.scale.end[0] - 1.887_962_3).abs() < 1e-6);
        assert_eq!(state.particle_rng, super::Lfsr16Pair { state: 0x6CE1_03C0 });
    }

    #[test]
    fn op7_rejects_non_finite_world_scalar() {
        let identity = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let acceleration = b88_command(1, 1.0);
        let mut context = render_context(0);
        context.world_scalar = f32::NAN;
        let mut state = Op7EmitterState::new(11, 11);
        assert!(evaluate_op7_emitter(
            &acceleration,
            0,
            &identity,
            [0.0, 0.0, -20.0],
            0.25,
            2.0,
            context,
            &mut state,
        )
        .is_none());
    }

    #[test]
    fn op7_rejects_truncated_body() {
        let mut command = command();
        command.raw_body = Arc::from(
            command.raw_body[..super::MIN_BODY_SIZE - 1]
                .to_vec()
                .into_boxed_slice(),
        );
        let identity = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let mut state = Op7EmitterState::new(11, 11);
        assert!(evaluate_op7_emitter(
            &command,
            0,
            &identity,
            [0.0, 0.0, -20.0],
            0.25,
            2.0,
            render_context(0),
            &mut state,
        )
        .is_none());
    }

    #[test]
    fn op7_mode_three_deactivates_on_native_exact_dot_one_precheck() {
        let identity = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        // F=(1,0,0), D=(1,1,0): abs(F·D)==1 although D×F is nonzero.
        // Native 9667..9683 deactivates before reaching its cross fallback.
        assert!(
            super::native_mode_three_basis(&identity, [2.0, 1.0, 0.0], [1.0, 0.0, 0.0]).is_none()
        );
    }

    #[test]
    fn op7_mode_three_uses_038d0_fallback_when_camera_cross_is_zero() {
        let identity = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        // F=(0,0,1), D=(0,0,2), so abs(dot)=2 (past the early gate),
        // but D×F is zero. 038D0 chooses +Y as the right axis and F×R as up.
        assert_eq!(
            super::native_mode_three_basis(&identity, [0.0, 0.0, 4.0], [0.0, 0.0, 2.0]),
            Some([
                [0.0, 0.0, -1.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [1.0, 0.0, 0.0, 2.0],
                [0.0, 0.0, 0.0, 1.0],
            ])
        );
    }

    #[test]
    fn op7_mode_three_uses_camera_cross_basis_for_non_degenerate_view() {
        let identity = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let matrix =
            super::native_mode_three_basis(&identity, [0.0, 0.0, -10.0], [1.0, 1.0, 1.0]).unwrap();
        let inv_sqrt_two = std::f32::consts::FRAC_1_SQRT_2;
        assert!((matrix[0][0] - 1.0 / 3.0_f32.sqrt()).abs() < 1e-6);
        assert!((matrix[0][1] - inv_sqrt_two).abs() < 1e-6);
        assert!((matrix[0][2] - 1.0 / (6.0_f32.sqrt() * 1.0)).abs() < 1e-6);
        assert!((matrix[1][0] - 1.0 / 3.0_f32.sqrt()).abs() < 1e-6);
        assert!((matrix[1][1] + inv_sqrt_two).abs() < 1e-6);
        assert!((matrix[1][2] - 1.0 / (6.0_f32.sqrt() * 1.0)).abs() < 1e-6);
        assert!((matrix[2][0] - 1.0 / 3.0_f32.sqrt()).abs() < 1e-6);
        assert!(matrix[2][2] + 2.0 / 6.0_f32.sqrt() < 1e-6);
        assert_eq!([matrix[0][3], matrix[1][3], matrix[2][3]], [1.0, 1.0, 1.0]);
    }

    #[test]
    fn op7_mode_three_projects_the_zero_helper_point_through_owner_matrix() {
        let identity = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let translated = [
            [1.0, 0.0, 0.0, 10.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let local =
            super::native_mode_three_basis(&identity, [0.0, 0.0, -10.0], [1.0, 1.0, 1.0]).unwrap();
        // FastProc+68 maps world origin to owner-local [-10,0,0]. Pairing it
        // with this local slot point and the same owner-local camera yields
        // the same forward vector as the untransformed reference case.
        let translated_local =
            super::native_mode_three_basis(&translated, [0.0, 0.0, -10.0], [-9.0, 1.0, 1.0])
                .unwrap();
        for row in 0..3 {
            assert!((translated_local[row][0] - local[row][0]).abs() < 1e-6);
        }
    }

    #[test]
    fn op7_mode_three_evaluator_renders_supported_slots() {
        let mut command = command();
        let mut body = command.raw_body.to_vec();
        put_u32(&mut body, 0x00, 1);
        put_f32(&mut body, 0x10, 0.0);
        put_f32(&mut body, 0x14, 1.0);
        put_f32(&mut body, 0x18, 1.0);
        put_u32(&mut body, 0x20, 1);
        put_vec3(&mut body, 0x28, [1.0, 1.0, 1.0]);
        put_vec3(&mut body, 0x34, [1.0, 1.0, 1.0]);
        put_vec3(&mut body, 0x40, [0.0; 3]);
        put_vec3(&mut body, 0x4C, [1.0; 3]);
        put_vec3(&mut body, 0x58, [1.0; 3]);
        put_vec3(&mut body, 0x64, [1.0; 3]);
        put_vec3(&mut body, 0x70, [0.0; 3]);
        put_vec3(&mut body, 0x7C, [1.0; 3]);
        command.raw_body = Arc::from(body.into_boxed_slice());
        let identity = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let mut context = render_context(0);
        context.visibility = Op7VisibilityProfile {
            width: 0.0,
            bias: -1.0,
        };
        let mut state = Op7EmitterState::new(9, 9);
        let frames = evaluate_op7_emitter(
            &command,
            0,
            &identity,
            [0.0, 0.0, -10.0],
            0.5,
            0.0,
            context,
            &mut state,
        )
        .unwrap();
        assert_eq!(frames.len(), 1);
        let center = centroid(&frames[0]);
        for (actual, expected) in center.into_iter().zip([1.0, 1.0, 1.0]) {
            assert!((actual - expected).abs() < 1e-6);
        }
    }

    #[test]
    fn op7_billboard_uses_owner_local_camera_point_for_translated_owner() {
        let mut command = command();
        let mut body = command.raw_body.to_vec();
        put_f32(&mut body, 0x10, 0.0);
        put_f32(&mut body, 0x14, 1.0);
        put_f32(&mut body, 0x18, 1.0);
        put_vec3(&mut body, 0x28, [0.0; 3]);
        put_vec3(&mut body, 0x34, [0.0; 3]);
        put_vec3(&mut body, 0x40, [0.0; 3]);
        put_vec3(&mut body, 0x4C, [1.0; 3]);
        put_vec3(&mut body, 0x58, [1.0; 3]);
        put_vec3(&mut body, 0x64, [1.0; 3]);
        put_vec3(&mut body, 0x70, [0.0; 3]);
        put_vec3(&mut body, 0x7C, [1.0; 3]);
        put_u32(&mut body, 0x20, 1);
        command.raw_body = Arc::from(body.into_boxed_slice());

        let translated_owner = [
            [1.0, 0.0, 0.0, 10.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let mut context = render_context(0);
        context.visibility = Op7VisibilityProfile {
            width: 0.0,
            bias: -1.0,
        };
        let frames = evaluate_op7_emitter(
            &command,
            0,
            &translated_owner,
            [10.0, 0.0, -10.0],
            0.5,
            0.5,
            context,
            &mut Op7EmitterState::new(7, 7),
        )
        .unwrap();

        assert_eq!(frames.len(), 1);
        // Native 07D10 transforms context+48 through the inverse owner matrix
        // before 01300 passes it to 93D0. With a translation-only owner that
        // leaves a camera directly behind the particle, so the quad is axis
        // aligned; raw world coordinates would rotate it around the view ray.
        assert_eq!(frames[0].vertices[0], [9.5, -0.5, 0.0]);
        assert_eq!(frames[0].vertices[2], [10.5, 0.5, 0.0]);
    }

    #[test]
    fn op7_is_inactive_outside_its_native_window() {
        let command = command();
        let identity = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let mut state = Op7EmitterState::new(7, 7);
        let frames = evaluate_op7_emitter(
            &command,
            0,
            &identity,
            [0.0, 0.0, -1.0],
            0.51,
            0.8,
            render_context(0),
            &mut state,
        )
        .unwrap();
        assert!(frames.is_empty());
    }

    #[test]
    fn op7_detail_ordinal_filters_fixed_cutoffs_not_particle_age() {
        let command = command();
        let identity = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        // This stream places five of six native cutoffs below level 1 and
        // one above it, so the test observes ordinal filtering directly.
        let mut state = Op7EmitterState::new(17, 0x0156);
        let run = |detail_level, state: &mut Op7EmitterState| {
            evaluate_op7_emitter(
                &command,
                0,
                &identity,
                [0.0, 0.0, -20.0],
                0.25,
                0.5,
                render_context(detail_level),
                state,
            )
            .unwrap()
        };
        let best = run(0, &mut state);
        let lower = run(1, &mut state);
        let lowest = run(2, &mut state);
        assert_eq!(best.len(), 6);
        assert!(lower.len() < best.len());
        assert!(lowest.len() <= lower.len());
        assert!(lower.iter().all(|frame| best
            .iter()
            .any(|full| frame.normalized_time == full.normalized_time)));
    }

    #[test]
    #[ignore = "requires the original GOG installation"]
    fn original_gog_breath_anl_03a_b_c_load_as_op7() {
        let root = std::env::var_os("PARKAN_GOG_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\GOG Games\Parkan - Iron Strategy"));
        let bytes = std::fs::read(root.join("effects.rlb")).expect("read effects.rlb");
        let archive = decode(Arc::from(bytes.into_boxed_slice()), ReadProfile::Compatible)
            .expect("decode effects.rlb");
        let identity = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        for name in ["breath_anl_03a", "breath_anl_03b", "breath_anl_03c"] {
            let id = archive.find(name).expect("FXID entry");
            let entry = archive.entry(id).expect("FXID metadata");
            assert_eq!(entry.meta().type_id, FXID_KIND);
            let bytes = archive.payload(id).expect("FXID payload");
            let document = decode_fxid(Arc::from(bytes.to_vec().into_boxed_slice()))
                .unwrap_or_else(|error| panic!("{name}: {error}"));
            let command = document.commands().first().expect("Op7 command");
            assert_eq!(command.opcode, FxOpcode::Op7, "{name}");
            assert_eq!(
                super::read_u32(&command.raw_body, 0x00),
                Some(0),
                "{name} B0"
            );
            assert_eq!(
                super::read_f32(&command.raw_body, 0x1C),
                Some(1.5),
                "{name} B1C"
            );
            assert_eq!(
                super::read_f32(&command.raw_body, 0x88),
                Some(0.0),
                "{name} B88"
            );
            let mut state = Op7EmitterState::new(7, 7);
            let frames = evaluate_op7_emitter(
                command,
                0,
                &identity,
                [0.0, 0.0, -20.0],
                0.275,
                0.5,
                render_context(0),
                &mut state,
            )
            .unwrap_or_else(|| panic!("{name}: native Op7 fields should parse"));
            assert_eq!(state.particles.len(), 6, "{name} pool slots");
            assert!(frames.len() <= 6, "{name} visible slots");
            assert!(frames.iter().all(|frame| frame.material.name == "smoke_yg"));
        }

        let id = archive.find("breath_anl_03").expect("Op8 FXID entry");
        let bytes = archive.payload(id).expect("Op8 FXID payload");
        let document = decode_fxid(Arc::from(bytes.to_vec().into_boxed_slice()))
            .expect("decode breath_anl_03");
        assert_eq!(document.commands()[0].opcode, FxOpcode::Op8);
    }
}
