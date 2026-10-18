//! Shared, native-shaped output for FX commands that render one four-vertex
//! effect primitive.
//!
//! This module keeps the small affine and frame contracts separate from the
//! environment-specific lightning emitter. Command evaluators only belong
//! here once their native inputs and output fields are established.

use crate::environment::EffectResource;
use crate::{FxCommand, FxOpcode};

/// Canonical native four-vertex topology in helper-local coordinates.
///
/// The consumer's triangle list is `[0, 1, 2, 2, 3, 0]`.
const EFFECT_QUAD_CORNERS: [[f32; 3]; 4] = [
    [-0.5, 0.5, 0.0],
    [-0.5, -0.5, 0.0],
    [0.5, -0.5, 0.0],
    [0.5, 0.5, 0.0],
];

/// Native full-texture quad coordinates, with the 0.99 edge inset.
pub(crate) const EFFECT_QUAD_UV: [[f32; 2]; 4] =
    [[0.0, 0.99], [0.0, 0.0], [0.99, 0.0], [0.99, 0.99]];

/// One evaluated native-style FX quad.
#[derive(Clone, Debug, PartialEq)]
pub struct EffectQuadFrame {
    /// Source command index in the FXID.
    pub command_index: usize,
    /// Source command opcode.
    pub opcode: FxOpcode,
    /// MAT0 resource selected by the command.
    pub material: EffectResource,
    /// World-space vertices in native corner order.
    pub vertices: [[f32; 3]; 4],
    /// Native texture coordinates in matching corner order.
    pub uv: [[f32; 2]; 4],
    /// Per-vertex RGBA value supplied by the native command evaluator.
    ///
    /// The alpha is the native FX opacity value. Native render assignment
    /// replaces MAT0 phase alpha with this value rather than multiplying it.
    pub color: [f32; 4],
    /// Wrapped command phase supplied to the material's MAT0 animation.
    pub material_phase: f32,
    /// Effect age in seconds.
    pub elapsed_seconds: f32,
    /// Normalized command time over its native active interval.
    pub normalized_time: f32,
    /// Whether the native command is active at `elapsed_seconds`.
    pub active: bool,
    /// Category-independent native draw-flag base selected by Op9 body C4.
    ///
    /// The native renderer combines this base with render-context category
    /// bits and a trailing command flag. Those inputs are not part of this
    /// geometry evaluator's interface yet, so this stores only the proven
    /// selector mapping (C4 0/1/2 -> 3/5/6).
    pub native_flag_base: Option<u32>,
}

/// One evaluated indexed mesh emitted by a native FX command.
///
/// Position and UV arrays use the native element order; `indices` expands
/// those elements into triangles for renderers that consume indexed lists.
#[derive(Clone, Debug, PartialEq)]
pub struct EffectMeshFrame {
    /// Source command index in the FXID.
    pub command_index: usize,
    /// Source command opcode.
    pub opcode: FxOpcode,
    /// MAT0 resource selected by the command.
    pub material: EffectResource,
    /// World-space mesh positions in native element order.
    pub vertices: Vec<[f32; 3]>,
    /// Texture coordinates paired with `vertices`.
    pub uv: Vec<[f32; 2]>,
    /// Triangle-list indices into `vertices` and `uv`.
    pub indices: Vec<u16>,
    /// Per-vertex RGBA value supplied by the native command evaluator.
    pub color: [f32; 4],
    /// Wrapped command phase supplied to the material's MAT0 animation.
    pub material_phase: f32,
    /// Effect age in seconds.
    pub elapsed_seconds: f32,
    /// Normalized command time over its native active interval.
    pub normalized_time: f32,
    /// Whether the native command is active at `elapsed_seconds`.
    pub active: bool,
    /// Category-independent selector base from Op9 body+C4.
    pub native_flag_base: u32,
    /// Native category selected by the top two bits of the renderer flags.
    pub native_category: u8,
}

const F_GENER_BALL_RESOURCE_OFFSET: usize = 0x84;
const F_GENER_BALL_RESOURCE_BYTES: usize = 64;

struct FGenerBallSample {
    opcode: FxOpcode,
    material: EffectResource,
    matrix: [[f32; 4]; 4],
    color: [f32; 4],
    material_phase: f32,
    elapsed_seconds: f32,
    normalized_time: f32,
    active: bool,
    native_flag_base: Option<u32>,
}

/// Evaluates a supported opcode-4 `f_gener_ball` billboard quad.
///
/// `world_from_effect` is the row-major native parent matrix after owner
/// transforms and the FX header scale have been applied. `effect_phase` is the
/// normalized overall effect age; `elapsed_seconds` is command age in seconds.
/// `camera_world` is the render-context query point consumed by the native
/// opcode-4 path. Opcode 9 emits generated hemisphere geometry and must use
/// [`evaluate_f_gener_ball_mesh`] instead.
///
/// Opcode 4 body mode 0 is supported when its distance denominator at
/// body+0xC4 is positive. Other mode/body combinations return `None` until
/// their native branches are verified.
#[must_use]
pub fn evaluate_f_gener_ball(
    command: &FxCommand,
    command_index: usize,
    world_from_effect: &[[f32; 4]; 4],
    effect_phase: f32,
    elapsed_seconds: f32,
    camera_world: [f32; 3],
) -> Option<EffectQuadFrame> {
    if command.opcode != FxOpcode::Op4 {
        return None;
    }
    let sample = evaluate_f_gener_ball_sample(
        command,
        world_from_effect,
        effect_phase,
        elapsed_seconds,
        camera_world,
    )?;
    let vertices = transform_native_effect_quad(&sample.matrix)?;

    Some(EffectQuadFrame {
        command_index,
        opcode: sample.opcode,
        material: sample.material,
        vertices,
        uv: EFFECT_QUAD_UV,
        color: sample.color,
        elapsed_seconds: sample.elapsed_seconds,
        normalized_time: sample.normalized_time,
        material_phase: sample.material_phase,
        active: sample.active,
        native_flag_base: sample.native_flag_base,
    })
}

/// Evaluates an Op9 `f_gener_ball` command as the native generated hemisphere.
///
/// `native_category` is `(renderer_flags >> 30)`, the native category index
/// consumed by Terrain's geometry selector. The three C4 selectors map to
/// generated-mesh modes 0, 1, and 2. Body+0xC8 selects the native UV path:
/// zero uses per-vertex projected XY coordinates; nonzero uses Terrain's
/// static quad/triangle UV layouts. Op4 remains a billboard quad and is not
/// accepted here.
#[must_use]
pub fn evaluate_f_gener_ball_mesh(
    command: &FxCommand,
    command_index: usize,
    world_from_effect: &[[f32; 4]; 4],
    effect_phase: f32,
    elapsed_seconds: f32,
    camera_world: [f32; 3],
    native_category: u8,
) -> Option<EffectMeshFrame> {
    if command.opcode != FxOpcode::Op9 || native_category > 3 {
        return None;
    }
    let sample = evaluate_f_gener_ball_sample(
        command,
        world_from_effect,
        effect_phase,
        elapsed_seconds,
        camera_world,
    )?;
    let native_flag_base = sample.native_flag_base?;
    let static_uv = read_u32(&command.raw_body, 0xC8)? != 0;
    let (vertices, uv, indices) =
        generated_op9_hemisphere(native_flag_base, native_category, static_uv)?;
    let vertices = vertices
        .into_iter()
        .map(|vertex| transform_native_effect_point(&sample.matrix, vertex))
        .collect::<Option<Vec<_>>>()?;

    Some(EffectMeshFrame {
        command_index,
        opcode: sample.opcode,
        material: sample.material,
        vertices,
        uv,
        indices,
        color: sample.color,
        material_phase: sample.material_phase,
        elapsed_seconds: sample.elapsed_seconds,
        normalized_time: sample.normalized_time,
        active: sample.active,
        native_flag_base,
        native_category,
    })
}

fn evaluate_f_gener_ball_sample(
    command: &FxCommand,
    world_from_effect: &[[f32; 4]; 4],
    effect_phase: f32,
    elapsed_seconds: f32,
    camera_world: [f32; 3],
) -> Option<FGenerBallSample> {
    if !world_from_effect
        .iter()
        .flatten()
        .all(|value| value.is_finite())
        || !effect_phase.is_finite()
        || !elapsed_seconds.is_finite()
    {
        return None;
    }
    if !matches!(command.opcode, FxOpcode::Op9 | FxOpcode::Op4) {
        return None;
    }

    let body = &command.raw_body;
    let mode = read_u32(body, 0x00)?;
    let (billboard, distance_denominator, native_flag_base) = match command.opcode {
        FxOpcode::Op9 if mode == 2 => {
            let selector = read_u32(body, 0xC4)?;
            let flag_base = match selector {
                0 => 3,
                1 => 5,
                2 => 6,
                _ => return None,
            };
            (false, None, Some(flag_base))
        }
        FxOpcode::Op4 if mode == 0 => {
            let denominator = read_f32(body, 0xC4)?;
            if !denominator.is_finite() || denominator <= 0.0 {
                return None;
            }
            (true, Some(denominator), None)
        }
        _ => return None,
    };

    let resource = read_effect_resource(body)?;
    let start = read_f32(body, 0x1C)?;
    let end = read_f32(body, 0x20)?;
    let phase_start_raw = read_f32(body, 0x04)?;
    let phase_end = read_f32(body, 0x08)?;
    let phase_exponent = read_f32(body, 0x0C)?;
    let opacity_start = read_f32(body, 0x10)?;
    let opacity_end = read_f32(body, 0x14)?;
    let opacity_exponent = read_f32(body, 0x18)?;
    if ![
        start,
        end,
        phase_start_raw,
        phase_end,
        phase_exponent,
        opacity_start,
        opacity_end,
        opacity_exponent,
    ]
    .iter()
    .all(|value| value.is_finite())
        || start > end
    {
        return None;
    }

    let interval_time = command_interval_phase(effect_phase, start, end);
    let animation_time = if let Some(denominator) = distance_denominator {
        if !camera_world.iter().all(|value| value.is_finite()) {
            return None;
        }
        let camera_local = native_effect_local_point(world_from_effect, camera_world)?;
        let distance = vector_length(camera_local)?;
        (interval_time * distance / denominator).min(1.0)
    } else {
        interval_time
    };
    if !animation_time.is_finite() {
        return None;
    }

    let position_start = read_vec3(body, 0x24)?;
    let position_end = read_vec3(body, 0x30)?;
    let position_exponents = read_vec3(body, 0x3C)?;
    let axis_start = read_vec3(body, 0x48)?;
    let axis_end = read_vec3(body, 0x54)?;
    let scale_start = read_vec3(body, 0x60)?;
    let scale_end = read_vec3(body, 0x6C)?;
    // Native pushes these exponent fields in reverse stack order; their
    // resulting XYZ pairing is body+0x80, +0x7C, +0x78.
    let scale_exponents = [
        read_f32(body, 0x80)?,
        read_f32(body, 0x7C)?,
        read_f32(body, 0x78)?,
    ];

    let position = curve_vector(
        position_start,
        position_end,
        position_exponents,
        animation_time,
    )?;
    let axis = lerp_vector(axis_start, axis_end, animation_time);
    let scale = curve_vector(scale_start, scale_end, scale_exponents, animation_time)?;
    let opacity_curve_time = animation_time.powf(opacity_exponent);
    let opacity =
        (opacity_start + (opacity_end - opacity_start) * opacity_curve_time).clamp(0.0, 1.0);

    let material_timebase = if phase_start_raw < 0.0 {
        elapsed_seconds
    } else {
        interval_time
    };
    let phase_start = phase_start_raw.max(0.0);
    let phase = phase_start + (phase_end - phase_start) * material_timebase.powf(phase_exponent);
    let material_phase = phase.rem_euclid(1.0);
    if !opacity.is_finite() || !material_phase.is_finite() {
        return None;
    }

    let local_basis = if billboard {
        let camera_local = native_effect_local_point(world_from_effect, camera_world)?;
        let view = subtract_vector(camera_local, position);
        native_camera_facing_basis(view, position)?
    } else {
        native_d110_basis(axis, position)?
    };
    let matrix = compose_native_effect_matrix(world_from_effect, &local_basis, position, scale)?;
    let active = effect_phase >= start && effect_phase <= end;

    Some(FGenerBallSample {
        opcode: command.opcode,
        material: resource,
        matrix,
        color: [1.0, 1.0, 1.0, opacity],
        elapsed_seconds,
        normalized_time: animation_time,
        material_phase,
        active,
        native_flag_base,
    })
}

// Terrain::18520's native mode/category tables, indexed by
// `mode * 4 + (renderer_flags >> 30)`. These values are assigned from the
// Terrain globals at initialization and drive both 18D40 draw loops.
const OP9_SECTOR_COUNTS: [[usize; 4]; 3] = [[8, 6, 5, 4], [16, 12, 10, 8], [24, 20, 16, 10]];
const OP9_RING_COUNTS: [[usize; 4]; 3] = [[3, 3, 2, 1], [6, 5, 4, 3], [9, 8, 6, 4]];

fn generated_op9_hemisphere(
    native_flag_base: u32,
    native_category: u8,
    static_uv: bool,
) -> Option<(Vec<[f32; 3]>, Vec<[f32; 2]>, Vec<u16>)> {
    let mode = match native_flag_base {
        3 => 0,
        5 => 1,
        6 => 2,
        _ => return None,
    };
    let category = usize::from(native_category);
    if category >= 4 {
        return None;
    }

    let sectors = OP9_SECTOR_COUNTS[mode][category];
    let rings = OP9_RING_COUNTS[mode][category];
    let quad_count = sectors.checked_mul(rings.checked_sub(1)?)?;
    let vertex_count = quad_count
        .checked_mul(4)?
        .checked_add(sectors.checked_mul(3)?)?;
    let index_count = quad_count
        .checked_mul(6)?
        .checked_add(sectors.checked_mul(3)?)?;
    let mut vertices = Vec::with_capacity(vertex_count);
    let mut uv = Vec::with_capacity(vertex_count);
    let mut indices = Vec::with_capacity(index_count);
    let latitude_step = std::f32::consts::FRAC_PI_2 / rings as f32;

    // 18310 stores quads sector-major, then ring-major. Each cell's order is
    // top/current-azimuth, top/next-azimuth, bottom/next, bottom/current.
    for sector in 0..sectors {
        let phi0 = std::f32::consts::TAU * sector as f32 / sectors as f32;
        let phi1 = std::f32::consts::TAU * (sector + 1) as f32 / sectors as f32;
        for ring in 0..rings.saturating_sub(1) {
            let theta0 = latitude_step * (ring + 1) as f32;
            let theta1 = latitude_step * (ring + 2) as f32;
            let element_start = u16::try_from(vertices.len()).ok()?;
            let element = [
                hemisphere_point(theta0, phi0),
                hemisphere_point(theta0, phi1),
                hemisphere_point(theta1, phi1),
                hemisphere_point(theta1, phi0),
            ];
            for (corner, point) in element.into_iter().enumerate() {
                uv.push(if static_uv {
                    EFFECT_QUAD_UV[corner]
                } else {
                    [(point[0] + 1.0) * 0.5, (point[1] + 1.0) * 0.5]
                });
                vertices.push(point);
            }
            indices.extend_from_slice(&[
                element_start,
                element_start + 1,
                element_start + 2,
                element_start + 2,
                element_start + 3,
                element_start,
            ]);
        }
    }

    // 18190 appends the cap fan after all quads, ordered pole, next sector,
    // current sector. The UVs are the same native XY projection as 18310.
    for sector in 0..sectors {
        let phi0 = std::f32::consts::TAU * sector as f32 / sectors as f32;
        let phi1 = std::f32::consts::TAU * (sector + 1) as f32 / sectors as f32;
        let theta = latitude_step;
        let element_start = u16::try_from(vertices.len()).ok()?;
        let element = [
            [0.0, 0.0, 1.0],
            hemisphere_point(theta, phi1),
            hemisphere_point(theta, phi0),
        ];
        let static_triangle_uv = [[0.5, 0.99], [0.0, 0.0], [0.99, 0.0]];
        for (corner, point) in element.into_iter().enumerate() {
            uv.push(if static_uv {
                static_triangle_uv[corner]
            } else {
                [(point[0] + 1.0) * 0.5, (point[1] + 1.0) * 0.5]
            });
            vertices.push(point);
        }
        indices.extend_from_slice(&[element_start, element_start + 1, element_start + 2]);
    }

    (vertices.len() == vertex_count && indices.len() == index_count)
        .then_some((vertices, uv, indices))
}

fn hemisphere_point(theta: f32, phi: f32) -> [f32; 3] {
    let radial = theta.sin();
    [radial * phi.sin(), radial * phi.cos(), theta.cos()]
}

pub(crate) fn native_camera_facing_basis(
    view: [f32; 3],
    position: [f32; 3],
) -> Option<[[f32; 4]; 4]> {
    if !view
        .iter()
        .chain(position.iter())
        .all(|value| value.is_finite())
    {
        return None;
    }
    let length = vector_length(view)?;
    if length == 0.0 {
        // The native 93F4 branch clears its active byte and returns false when
        // camera and animated position coincide.
        return None;
    }
    let view = [view[0] / length, view[1] / length, view[2] / length];
    let mut u = if view[0] == 0.0 {
        [1.0, 0.0, 0.0]
    } else if view[1] == 0.0 {
        [0.0, 1.0, 0.0]
    } else {
        [-view[1], view[0], 0.0]
    };
    if view[2] != 0.0 && view[0] != 0.0 && view[1] != 0.0 {
        let u_length = vector_length(u)?;
        if u_length == 0.0 {
            return None;
        }
        for component in &mut u {
            *component /= u_length;
        }
    }
    let v = cross(view, u);
    Some([
        [u[0], v[0], view[0], position[0]],
        [u[1], v[1], view[1], position[1]],
        [u[2], v[2], view[2], position[2]],
        [0.0, 0.0, 0.0, 1.0],
    ])
}

fn multiply_affine(left: &[[f32; 4]; 4], right: &[[f32; 4]; 4]) -> Option<[[f32; 4]; 4]> {
    let result = std::array::from_fn(|row| {
        std::array::from_fn(|column| {
            (0..4)
                .map(|inner| left[row][inner] * right[inner][column])
                .sum::<f32>()
        })
    });
    result
        .iter()
        .flatten()
        .all(|value| value.is_finite())
        .then_some(result)
}

fn read_effect_resource(body: &[u8]) -> Option<EffectResource> {
    let bytes = body.get(
        F_GENER_BALL_RESOURCE_OFFSET..F_GENER_BALL_RESOURCE_OFFSET + F_GENER_BALL_RESOURCE_BYTES,
    )?;
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

fn command_interval_phase(phase: f32, start: f32, end: f32) -> f32 {
    if phase < start {
        0.0
    } else if end == start {
        1.0
    } else if phase >= end {
        1.0
    } else {
        ((phase - start) / (end - start)).clamp(0.0, 1.0)
    }
}

/// Builds the native D110 Type2 local basis in row-major matrix form.
///
/// The X/Y degeneracies use native constant fallbacks. For a general axis U
/// is normalized only when its Z component is nonzero; V is `axis × U`, and
/// the axis vector itself remains unnormalized.
#[must_use]
pub(crate) fn native_d110_basis(axis: [f32; 3], position: [f32; 3]) -> Option<[[f32; 4]; 4]> {
    if !axis
        .iter()
        .chain(position.iter())
        .all(|value| value.is_finite())
    {
        return None;
    }

    let mut u = if axis[0] == 0.0 {
        [1.0, 0.0, 0.0]
    } else if axis[1] == 0.0 {
        [0.0, 1.0, 0.0]
    } else {
        [-axis[1], axis[0], 0.0]
    };
    if axis[2] != 0.0 && axis[0] != 0.0 && axis[1] != 0.0 {
        let length = vector_length(u)?;
        if length == 0.0 {
            return None;
        }
        for value in &mut u {
            *value /= length;
        }
    }

    let v = cross(axis, u);
    let result = [
        [u[0], v[0], axis[0], position[0]],
        [u[1], v[1], axis[1], position[1]],
        [u[2], v[2], axis[2], position[2]],
        [0.0, 0.0, 0.0, 1.0],
    ];
    result
        .iter()
        .flatten()
        .all(|value| value.is_finite())
        .then_some(result)
}

fn curve_vector(start: [f32; 3], end: [f32; 3], exponent: [f32; 3], time: f32) -> Option<[f32; 3]> {
    let result = std::array::from_fn(|axis| {
        start[axis] + (end[axis] - start[axis]) * time.powf(exponent[axis])
    });
    result
        .iter()
        .all(|value| value.is_finite())
        .then_some(result)
}

fn lerp_vector(start: [f32; 3], end: [f32; 3], time: f32) -> [f32; 3] {
    std::array::from_fn(|axis| start[axis] + (end[axis] - start[axis]) * time)
}

fn vector_length(vector: [f32; 3]) -> Option<f32> {
    let length = (vector[0] * vector[0] + vector[1] * vector[1] + vector[2] * vector[2]).sqrt();
    length.is_finite().then_some(length)
}

fn subtract_vector(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

fn cross(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

/// Applies the native affine point transform used by the quad helper.
///
/// The matrix is row-major. Each output component is the dot product of one
/// of the first three rows with `[x, y, z, 1]`.
#[must_use]
pub(crate) fn transform_native_effect_point(
    matrix: &[[f32; 4]; 4],
    point: [f32; 3],
) -> Option<[f32; 3]> {
    if !matrix.iter().flatten().all(|value| value.is_finite())
        || !point.iter().all(|value| value.is_finite())
    {
        return None;
    }
    let result = std::array::from_fn(|row| {
        matrix[row][0] * point[0]
            + matrix[row][1] * point[1]
            + matrix[row][2] * point[2]
            + matrix[row][3]
    });
    result
        .iter()
        .all(|value| value.is_finite())
        .then_some(result)
}

/// Transforms the native canonical quad through the final helper matrix.
#[must_use]
pub(crate) fn transform_native_effect_quad(matrix: &[[f32; 4]; 4]) -> Option<[[f32; 3]; 4]> {
    let vertices = EFFECT_QUAD_CORNERS.map(|corner| transform_native_effect_point(matrix, corner));
    if vertices.iter().any(Option::is_none) {
        return None;
    }
    Some(vertices.map(Option::unwrap))
}

/// Applies the native `g_FastProc + 0x68` point operation.
///
/// This is the native component-wise projection: subtract matrix translation,
/// dot the delta with each basis column, and divide by that column's squared
/// length. It is the `g_FastProc + 0x68` contract used by effect callers.
#[must_use]
pub(crate) fn native_effect_local_point(
    matrix: &[[f32; 4]; 4],
    point: [f32; 3],
) -> Option<[f32; 3]> {
    if !matrix.iter().flatten().all(|value| value.is_finite())
        || !point.iter().all(|value| value.is_finite())
    {
        return None;
    }
    let delta = [
        point[0] - matrix[0][3],
        point[1] - matrix[1][3],
        point[2] - matrix[2][3],
    ];
    let mut result = [0.0; 3];
    for axis in 0..3 {
        let basis = [matrix[0][axis], matrix[1][axis], matrix[2][axis]];
        let squared_length = basis
            .iter()
            .map(|component| component * component)
            .sum::<f32>();
        if !squared_length.is_finite() || squared_length == 0.0 {
            return None;
        }
        result[axis] =
            (basis[0] * delta[0] + basis[1] * delta[1] + basis[2] * delta[2]) / squared_length;
    }
    result
        .iter()
        .all(|value| value.is_finite())
        .then_some(result)
}

/// Composes a native FX local basis and translation with the effect parent.
///
/// This matches the shared `93D0` finalization: scale the parent basis columns
/// by the animated scale, multiply by the local basis, then replace the
/// translation with the unscaled parent transform of the animated position.
#[must_use]
pub(crate) fn compose_native_effect_matrix(
    world_from_effect: &[[f32; 4]; 4],
    local_basis: &[[f32; 4]; 4],
    position: [f32; 3],
    scale: [f32; 3],
) -> Option<[[f32; 4]; 4]> {
    if !world_from_effect
        .iter()
        .flatten()
        .all(|value| value.is_finite())
        || !local_basis.iter().flatten().all(|value| value.is_finite())
        || !position
            .iter()
            .chain(scale.iter())
            .all(|value| value.is_finite())
    {
        return None;
    }
    let mut scaled_parent = *world_from_effect;
    for row in 0..3 {
        for column in 0..3 {
            scaled_parent[row][column] *= scale[column];
        }
    }
    let mut result = multiply_affine(&scaled_parent, local_basis)?;
    let world_position = transform_native_effect_point(world_from_effect, position)?;
    for axis in 0..3 {
        result[axis][3] = world_position[axis];
    }
    result[3] = [0.0, 0.0, 0.0, 1.0];
    result
        .iter()
        .flatten()
        .all(|value| value.is_finite())
        .then_some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn d110_builds_general_axis_basis_and_preserves_native_axis_length() {
        let matrix = native_d110_basis([1.0, 2.0, 2.0], [3.0, 4.0, 5.0]).unwrap();
        let inv_sqrt_5 = 1.0 / 5.0_f32.sqrt();
        assert_eq!(matrix[0], [-2.0 * inv_sqrt_5, -2.0 * inv_sqrt_5, 1.0, 3.0]);
        assert_eq!(matrix[1], [inv_sqrt_5, -4.0 * inv_sqrt_5, 2.0, 4.0]);
        assert_eq!(matrix[2], [0.0, 5.0 / 5.0_f32.sqrt(), 2.0, 5.0]);
    }

    #[test]
    fn d110_uses_native_x_then_y_degenerate_fallbacks() {
        let x_fallback = native_d110_basis([0.0, 1.0, 0.0], [0.0; 3]).unwrap();
        assert_eq!(x_fallback[0][0..3], [1.0, 0.0, 0.0]);
        assert_eq!(x_fallback[1][0..3], [0.0, 0.0, 1.0]);

        let y_fallback = native_d110_basis([1.0, 0.0, 0.0], [0.0; 3]).unwrap();
        assert_eq!(y_fallback[0][0..3], [0.0, 0.0, 1.0]);
        assert_eq!(y_fallback[1][0..3], [1.0, 0.0, 0.0]);
    }

    #[test]
    fn native_point_operation_projects_each_translated_basis_column() {
        // Deliberately non-symmetric, translated, and non-orthogonal. The
        // native helper projects independently onto each column and divides
        // by that column's squared length; this is not a full matrix inverse.
        let matrix = [
            [2.0, 3.0, 5.0, 7.0],
            [11.0, 13.0, 17.0, 19.0],
            [23.0, 29.0, 31.0, 37.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        assert_eq!(
            native_effect_local_point(&matrix, [41.0, 43.0, 47.0]),
            Some([562.0 / 654.0, 704.0 / 1019.0, 888.0 / 1275.0])
        );

        let nonuniform_translated = [
            [2.0, 0.0, 0.0, 1.0],
            [0.0, 3.0, 0.0, 2.0],
            [0.0, 0.0, 4.0, 3.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        assert_eq!(
            native_effect_local_point(&nonuniform_translated, [6.0, 8.0, 8.0]),
            Some([2.5, 2.0, 1.25])
        );

        let tiny_basis = [
            [0.0001, 0.0, 0.0, 0.0],
            [0.0, 2.0, 0.0, 0.0],
            [0.0, 0.0, 3.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        assert_eq!(
            native_effect_local_point(&tiny_basis, [0.0002, 2.0, 3.0]),
            Some([2.0, 1.0, 1.0])
        );
    }

    #[test]
    fn native_quad_uses_canonical_corner_order_and_affine_rows() {
        let matrix = [
            [2.0, 3.0, 5.0, 7.0],
            [11.0, 13.0, 17.0, 19.0],
            [23.0, 29.0, 31.0, 37.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        assert_eq!(
            transform_native_effect_quad(&matrix),
            Some([
                [7.5, 20.0, 40.0],
                [4.5, 7.0, 11.0],
                [6.5, 18.0, 34.0],
                [9.5, 31.0, 63.0],
            ])
        );
        assert_eq!(
            EFFECT_QUAD_UV,
            [[0.0, 0.99], [0.0, 0.0], [0.99, 0.0], [0.99, 0.99]]
        );
    }

    #[test]
    fn affine_helpers_reject_non_finite_inputs_and_results() {
        let mut matrix = [[0.0; 4]; 4];
        matrix[3][3] = 1.0;
        assert_eq!(
            transform_native_effect_point(&matrix, [f32::NAN, 0.0, 0.0]),
            None
        );
        matrix[0][0] = f32::INFINITY;
        assert_eq!(transform_native_effect_quad(&matrix), None);
    }

    #[test]
    fn f_gener_ball_snapshot_emits_generated_op9_meshes_and_an_op4_quad() {
        // Provenance: decoded `effects.rlb/f_gener_ball` snapshot in
        // target/research-playground/generator-orb/fx-bodies.txt. The source
        // archive SHA-256 was
        // 3DAF49A8A4DA2FBC181F86822E125E47EB18F431BB5DF10EC53AC2EDC29C0A89.
        // Keep only the known command fields here; no licensed archive bytes
        // or unneeded Op1 semantics are copied into the test.
        let commands = f_gener_ball_snapshot_commands();
        assert_eq!(commands.len(), 4);
        assert_eq!(
            commands
                .iter()
                .map(|command| command.opcode)
                .collect::<Vec<_>>(),
            [FxOpcode::Op9, FxOpcode::Op9, FxOpcode::Op4, FxOpcode::Op1]
        );

        // The decoded FX header carries a uniform 0.75 parent scale. That
        // scale belongs to the input matrix, not the command body.
        let parent = uniform_scale_matrix(0.75);
        let camera = [0.0, 0.0, 500.0 / 0.75];
        let op9_meshes = commands[..2]
            .iter()
            .enumerate()
            .map(|(index, command)| {
                evaluate_f_gener_ball_mesh(command, index, &parent, 0.875, 0.3, camera, 0)
                    .expect("native Op9 mode emits a generated hemisphere")
            })
            .collect::<Vec<_>>();
        assert_eq!(op9_meshes.len(), 2);
        assert_eq!(op9_meshes[0].command_index, 0);
        assert_eq!(op9_meshes[1].command_index, 1);

        for frame in &op9_meshes {
            assert_eq!(
                frame.material,
                EffectResource {
                    archive: "material.lib".to_owned(),
                    name: "laser_y_hit".to_owned(),
                }
            );
            assert_eq!(frame.normalized_time, 0.5);
            assert!((frame.color[3] - 0.5).abs() < 1e-6);
            assert!((frame.material_phase - 0.3).abs() < 1e-6);
            assert!(frame.active);
            assert_eq!(frame.native_flag_base, 3);
            assert_eq!(frame.native_category, 0);
            assert_eq!(frame.vertices.len(), 88);
            assert_eq!(frame.uv.len(), 88);
            assert_eq!(frame.indices.len(), 120);
        }
        assert_ne!(op9_meshes[0].vertices, op9_meshes[1].vertices);
        assert!(evaluate_f_gener_ball(&commands[0], 0, &parent, 0.875, 0.3, camera).is_none());

        let frame = evaluate_f_gener_ball(&commands[2], 2, &parent, 0.875, 0.3, camera)
            .expect("Op4 remains a billboard quad");
        assert_eq!(
            frame.material,
            EffectResource {
                archive: "material.lib".to_owned(),
                name: "glow_y".to_owned(),
            }
        );
        // FastProc projects through the parent basis and divides by its
        // squared length, so the uniform 0.75 parent scale maps the adjusted
        // world camera to local distance 500 / (0.75 * 0.75) before Op4 rescales time.
        assert!((frame.normalized_time - 8.0 / 9.0).abs() < 1e-6);
        assert!((frame.color[3] - 17.0 / 18.0).abs() < 1e-6);
        assert!((frame.material_phase - 0.7).abs() < 1e-6);
        assert!(frame.active);
        assert_vec3_close(frame.vertices[0], [-3.008_333_4, 3.008_333_4, 0.0]);
    }

    #[test]
    fn f_gener_ball_samples_position_scale_and_opacity_curves() {
        let mut command = f_gener_ball_command(FxOpcode::Op9);
        let body = Arc::make_mut(&mut command.raw_body);
        put_vec3(body, 0x30, [2.0, 4.0, 8.0]);
        put_vec3(body, 0x3C, [1.0, 2.0, 3.0]);
        put_vec3(body, 0x48, [0.0, 1.0, 0.0]);
        put_vec3(body, 0x54, [0.0, 0.0, 1.0]);
        put_vec3(body, 0x60, [1.0, 2.0, 3.0]);
        put_vec3(body, 0x6C, [3.0, 6.0, 11.0]);
        // Native XYZ exponent order is body+0x80, +0x7C, +0x78.
        put_vec3(body, 0x78, [3.0, 2.0, 1.0]);
        put_f32(body, 0x10, 0.2);
        put_f32(body, 0x14, 1.0);
        put_f32(body, 0x18, 2.0);

        let identity = identity_matrix();
        let sample = evaluate_f_gener_ball_sample(&command, &identity, 0.875, 0.35, [0.0; 3])
            .expect("curve fields are valid");
        assert_eq!(sample.normalized_time, 0.5);
        assert_vec3_close(
            transform_native_effect_point(&sample.matrix, [0.0; 3]).unwrap(),
            [1.0, 1.0, 1.0],
        );
        for (column, expected_length) in [2.0, 2.5, 2.5].into_iter().enumerate() {
            let basis = [
                sample.matrix[0][column],
                sample.matrix[1][column],
                sample.matrix[2][column],
            ];
            assert!((vector_length(basis).unwrap() - expected_length).abs() < 1e-6);
        }
        assert!((sample.color[3] - 0.4).abs() < 1e-6);
    }

    #[test]
    fn f_gener_ball_op4_uses_local_camera_distance_and_billboard_basis() {
        let command = f_gener_ball_snapshot_commands().remove(2);
        let identity = identity_matrix();
        let frame = evaluate_f_gener_ball(&command, 2, &identity, 0.875, 0.3, [0.0, 0.0, 500.0])
            .expect("supported Op4 fixture command");

        assert_eq!(frame.command_index, 2);
        assert_eq!(
            frame.material,
            EffectResource {
                archive: "material.lib".to_owned(),
                name: "glow_y".to_owned(),
            }
        );
        assert_eq!(frame.normalized_time, 0.5);
        assert!((frame.material_phase - 0.7).abs() < 1e-6);
        assert_eq!(frame.color, [1.0, 1.0, 1.0, 0.75]);
        assert!(frame.active);
        let expected_extent = 4.6 * 0.5;
        assert_eq!(
            frame.vertices,
            [
                [-expected_extent, expected_extent, 0.0],
                [-expected_extent, -expected_extent, 0.0],
                [expected_extent, -expected_extent, 0.0],
                [expected_extent, expected_extent, 0.0],
            ]
        );

        let mut far_camera = [0.0, 0.0, 1000.0];
        let capped = evaluate_f_gener_ball(&command, 2, &identity, 0.875, 0.3, far_camera)
            .expect("camera distance caps Op4 progress");
        assert_eq!(capped.normalized_time, 1.0);
        far_camera[2] = 0.0;
        assert!(evaluate_f_gener_ball(&command, 2, &identity, 0.875, 0.3, far_camera).is_none());

        let nonuniform_parent = [
            [2.0, 0.0, 0.0, 1.0],
            [0.0, 3.0, 0.0, 2.0],
            [0.0, 0.0, 4.0, 3.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let camera_world = [6.0, 8.0, 8.0];
        let camera_local = native_effect_local_point(&nonuniform_parent, camera_world).unwrap();
        let expected_time = 0.5 * vector_length(camera_local).unwrap() / 500.0;
        let transformed =
            evaluate_f_gener_ball(&command, 2, &nonuniform_parent, 0.875, 0.3, camera_world)
                .expect("Op4 uses the native column-projection camera query");
        assert!((transformed.normalized_time - expected_time).abs() < 1e-6);
    }

    #[test]
    fn f_gener_ball_phase_uses_interval_age_before_op4_camera_rescaling() {
        let mut command = f_gener_ball_command(FxOpcode::Op4);
        let body = Arc::make_mut(&mut command.raw_body);
        put_f32(body, 0x04, 0.5);
        let identity = identity_matrix();

        // The command interval maps effect phase 0.875 into age 0.5. Op4's
        // 250/500 render-distance factor reduces track time to 0.25, while
        // 105F0/108F0 has already computed MAT0 phase from the unscaled 0.5.
        let frame = evaluate_f_gener_ball(&command, 2, &identity, 0.875, 0.3, [0.0, 0.0, 250.0])
            .expect("valid Op4 command");
        assert_eq!(frame.normalized_time, 0.25);
        assert_eq!(frame.material_phase, 0.75);
    }

    #[test]
    fn f_gener_ball_rejects_unverified_modes_and_non_finite_state() {
        let mut command = f_gener_ball_command(FxOpcode::Op9);
        let identity = identity_matrix();
        Arc::make_mut(&mut command.raw_body)[..4].copy_from_slice(&1u32.to_le_bytes());
        assert!(
            evaluate_f_gener_ball_mesh(&command, 0, &identity, 0.875, 0.35, [0.0; 3], 0).is_none()
        );

        let mut invalid_selector = f_gener_ball_command(FxOpcode::Op9);
        put_u32(Arc::make_mut(&mut invalid_selector.raw_body), 0xC4, 3);
        assert!(evaluate_f_gener_ball_mesh(
            &invalid_selector,
            0,
            &identity,
            0.875,
            0.35,
            [0.0; 3],
            0
        )
        .is_none());

        let command = f_gener_ball_command(FxOpcode::Op9);
        let mut invalid_parent = identity;
        invalid_parent[1][2] = f32::NAN;
        assert!(
            evaluate_f_gener_ball_mesh(&command, 0, &invalid_parent, 0.875, 0.35, [0.0; 3], 0)
                .is_none()
        );
    }

    #[test]
    fn op9_c4_selector_maps_to_native_draw_flag_base() {
        let identity = identity_matrix();
        for (selector, expected) in [(0, 3), (1, 5), (2, 6)] {
            let mut command = f_gener_ball_command(FxOpcode::Op9);
            put_u32(Arc::make_mut(&mut command.raw_body), 0xC4, selector);
            let frame = evaluate_f_gener_ball_mesh(
                &command,
                0,
                &identity,
                0.875,
                0.35,
                [0.0, 0.0, 500.0],
                0,
            )
            .expect("native Op9 C4 selector 0..=2");
            assert_eq!(frame.native_flag_base, expected);
            assert_eq!(frame.native_category, 0);
            let (sectors, rings) = match selector {
                0 => (8, 3),
                1 => (16, 6),
                2 => (24, 9),
                _ => unreachable!(),
            };
            let quads = sectors * (rings - 1);
            assert_eq!(frame.vertices.len(), quads * 4 + sectors * 3);
            assert_eq!(frame.indices.len(), quads * 6 + sectors * 3);
        }
    }

    #[test]
    fn op9_mesh_uses_native_mode_category_counts_and_projected_uvs() {
        let identity = identity_matrix();
        let command = f_gener_ball_command(FxOpcode::Op9);
        let expected = [
            [(8_usize, 3_usize), (6, 3), (5, 2), (4, 1)],
            [(16, 6), (12, 5), (10, 4), (8, 3)],
            [(24, 9), (20, 8), (16, 6), (10, 4)],
        ];

        for (selector, mode) in [(0, 0), (1, 1), (2, 2)] {
            let mut command = command.clone();
            put_u32(Arc::make_mut(&mut command.raw_body), 0xC4, selector);
            for category in 0..4_u8 {
                let mesh = evaluate_f_gener_ball_mesh(
                    &command,
                    0,
                    &identity,
                    0.875,
                    0.35,
                    [0.0, 0.0, 500.0],
                    category,
                )
                .expect("all native mode/category combinations have a mesh");
                let (sectors, rings) = expected[mode][usize::from(category)];
                let quads = sectors * (rings - 1);
                assert_eq!(mesh.vertices.len(), quads * 4 + sectors * 3);
                assert_eq!(mesh.uv.len(), mesh.vertices.len());
                assert_eq!(mesh.indices.len(), quads * 6 + sectors * 3);
                assert!(mesh
                    .vertices
                    .iter()
                    .flatten()
                    .all(|value| value.is_finite()));
                assert!(mesh.uv.iter().flatten().all(|value| value.is_finite()));
                assert!(mesh
                    .indices
                    .iter()
                    .all(|index| usize::from(*index) < mesh.vertices.len()));
            }
        }

        let mut mode0_command = command.clone();
        put_u32(Arc::make_mut(&mut mode0_command.raw_body), 0xC4, 0);
        put_vec3(
            Arc::make_mut(&mut mode0_command.raw_body),
            0x48,
            [0.0, 0.0, 1.0],
        );
        put_vec3(
            Arc::make_mut(&mut mode0_command.raw_body),
            0x54,
            [0.0, 0.0, 1.0],
        );
        let mesh = evaluate_f_gener_ball_mesh(
            &mode0_command,
            0,
            &identity,
            0.875,
            0.35,
            [0.0, 0.0, 500.0],
            0,
        )
        .expect("last selector remains valid");
        // Native triangle cap starts after all sector-major quads. Sector 0
        // is pole, next azimuth, current azimuth with XY-projected UVs.
        let cap_start = (8 * (3 - 1)) * 4;
        assert_vec3_close(mesh.vertices[cap_start], [0.0, 0.0, 0.4]);
        assert_eq!(mesh.uv[cap_start], [0.5, 0.5]);
        let next = mesh.vertices[cap_start + 1];
        assert!((next[0] - 0.10606602).abs() < 1e-6);
        assert!((next[1] - 0.14142136).abs() < 1e-6);
        assert!((next[2] - 0.34641016).abs() < 1e-6);
        let cap_index_start = (8 * (3 - 1)) * 6;
        assert_eq!(mesh.indices[cap_index_start], cap_start as u16);
        assert_eq!(mesh.indices[cap_index_start + 1], cap_start as u16 + 1);
        assert_eq!(mesh.indices[cap_index_start + 2], cap_start as u16 + 2);

        let mut static_uv_command = mode0_command.clone();
        put_u32(Arc::make_mut(&mut static_uv_command.raw_body), 0xC8, 1);
        let static_uv_mesh = evaluate_f_gener_ball_mesh(
            &static_uv_command,
            0,
            &identity,
            0.875,
            0.35,
            [0.0, 0.0, 500.0],
            0,
        )
        .expect("nonzero native UV flag selects static UV arrays");
        assert_eq!(&static_uv_mesh.uv[..4], &EFFECT_QUAD_UV);
        assert_eq!(
            &static_uv_mesh.uv[cap_start..cap_start + 3],
            &[[0.5, 0.99], [0.0, 0.0], [0.99, 0.0]]
        );
        assert!(evaluate_f_gener_ball_mesh(
            &command,
            0,
            &identity,
            0.875,
            0.35,
            [0.0, 0.0, 500.0],
            4,
        )
        .is_none());
    }

    #[test]
    fn op9_generated_mesh_triangles_keep_native_outward_winding() {
        for (mode, flag_base) in [(0, 3), (1, 5), (2, 6)] {
            for category in 0..4_u8 {
                let (vertices, _, indices) =
                    generated_op9_hemisphere(flag_base, category, false).unwrap();
                assert!(!vertices.is_empty());
                assert_eq!(indices.len() % 3, 0);
                for triangle in indices.chunks_exact(3) {
                    let a = vertices[usize::from(triangle[0])];
                    let b = vertices[usize::from(triangle[1])];
                    let c = vertices[usize::from(triangle[2])];
                    let normal = cross(subtract_vector(b, a), subtract_vector(c, a));
                    let centroid = [
                        (a[0] + b[0] + c[0]) / 3.0,
                        (a[1] + b[1] + c[1]) / 3.0,
                        (a[2] + b[2] + c[2]) / 3.0,
                    ];
                    let outward =
                        normal[0] * centroid[0] + normal[1] * centroid[1] + normal[2] * centroid[2];
                    assert!(
                        outward > 0.0,
                        "mode {mode}, category {category}, triangle {triangle:?}"
                    );
                }
            }
        }
    }

    #[test]
    #[ignore = "requires the original GOG installation"]
    fn original_gog_recharge_and_tree_plate_op9_modes_evaluate() {
        let root = std::env::var_os("PARKAN_GOG_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(r"C:\GOG Games\Parkan - Iron Strategy"));
        let archive_bytes: Arc<[u8]> = std::fs::read(root.join("effects.rlb"))
            .expect("read effects.rlb")
            .into();
        let archive = fparkan_nres::decode(archive_bytes, fparkan_nres::ReadProfile::Compatible)
            .expect("decode effects.rlb");
        let identity = identity_matrix();

        for name in ["f_recharge_r", "tree_plate_58"] {
            let entry_id = archive
                .find(name)
                .unwrap_or_else(|| panic!("missing {name}"));
            let document = crate::decode_fxid(Arc::from(
                archive
                    .payload(entry_id)
                    .expect("FXID payload")
                    .to_vec()
                    .into_boxed_slice(),
            ))
            .unwrap_or_else(|error| panic!("decode {name}: {error}"));
            let mut seen = false;
            for (index, command) in document.commands().iter().enumerate() {
                if command.opcode != FxOpcode::Op9
                    || read_u32(&command.raw_body, 0)
                        .unwrap_or_else(|| panic!("{name} malformed Op9 mode"))
                        != 2
                {
                    continue;
                }
                let selector = read_u32(&command.raw_body, 0xC4)
                    .unwrap_or_else(|| panic!("{name} Op9 missing C4 selector"));
                if !(0..=2).contains(&selector) {
                    continue;
                }
                let phase_start = read_f32(&command.raw_body, 0x1C)
                    .unwrap_or_else(|| panic!("{name} Op9 missing phase start"));
                let phase_end = read_f32(&command.raw_body, 0x20)
                    .unwrap_or_else(|| panic!("{name} Op9 missing phase end"));
                let frame = evaluate_f_gener_ball_mesh(
                    command,
                    index,
                    &identity,
                    (phase_start + phase_end) * 0.5,
                    0.0,
                    [0.0, 0.0, 500.0],
                    0,
                )
                .unwrap_or_else(|| panic!("{name} Op9 selector {selector} did not evaluate"));
                assert_eq!(frame.native_flag_base, [3, 5, 6][selector as usize]);
                let (sectors, rings) = match selector {
                    0 => (8, 3),
                    1 => (16, 6),
                    2 => (24, 9),
                    _ => unreachable!(),
                };
                assert_eq!(
                    frame.vertices.len(),
                    sectors * (rings - 1) * 4 + sectors * 3
                );
                seen = true;
            }
            assert!(seen, "{name} has no supported Op9 mode");
        }
    }

    fn f_gener_ball_snapshot_commands() -> Vec<FxCommand> {
        let mut first_op9 = f_gener_ball_command(FxOpcode::Op9);
        let mut second_op9 = f_gener_ball_command(FxOpcode::Op9);
        put_vec3(
            Arc::make_mut(&mut second_op9.raw_body),
            0x48,
            [1.0, 0.0, 0.0],
        );
        put_vec3(
            Arc::make_mut(&mut second_op9.raw_body),
            0x54,
            [1.0, 0.0, 0.0],
        );
        // Both checked Op9 commands have opacity 0 -> 1 and opposite X axes.
        put_f32(Arc::make_mut(&mut first_op9.raw_body), 0x10, 0.0);
        put_f32(Arc::make_mut(&mut second_op9.raw_body), 0x10, 0.0);

        let mut op4 = f_gener_ball_command(FxOpcode::Op4);
        put_f32(Arc::make_mut(&mut op4.raw_body), 0x10, 0.5);

        // The checked decoder confirms only that the fourth row is Op1. Its
        // raw body is intentionally absent: this evaluator does not claim its
        // layout or behavior.
        let op1 = FxCommand {
            word: 1,
            opcode: FxOpcode::Op1,
            native_flag: false,
            raw_body: Arc::from([]),
            resource_refs: Vec::new(),
        };
        vec![first_op9, second_op9, op4, op1]
    }

    fn f_gener_ball_command(opcode: FxOpcode) -> FxCommand {
        let is_op9 = opcode == FxOpcode::Op9;
        let mut body = vec![0u8; if is_op9 { 0xCC } else { 0xC8 }];
        let mode = if is_op9 { 2 } else { 0 };
        put_u32(&mut body, 0x00, mode);
        put_f32(&mut body, 0x04, -1.0);
        put_f32(&mut body, 0x08, if is_op9 { 11.0 } else { 9.0 });
        put_f32(&mut body, 0x0C, 1.0);
        put_f32(&mut body, 0x10, 0.0);
        put_f32(&mut body, 0x14, 1.0);
        put_f32(&mut body, 0x18, 1.0);
        put_f32(&mut body, 0x1C, 0.75);
        put_f32(&mut body, 0x20, 1.0);
        put_vec3(&mut body, 0x24, [0.0; 3]);
        put_vec3(&mut body, 0x30, [0.0; 3]);
        put_vec3(&mut body, 0x3C, [1.0; 3]);
        let axis = if is_op9 {
            [-1.0, 0.0, 0.0]
        } else {
            [1.0, 0.0, 0.0]
        };
        put_vec3(&mut body, 0x48, axis);
        put_vec3(&mut body, 0x54, axis);
        let scale_start = if is_op9 { [0.3, 0.4, 0.4] } else { [0.2; 3] };
        let scale_end = if is_op9 { [0.3, 0.4, 0.4] } else { [9.0; 3] };
        put_vec3(&mut body, 0x60, scale_start);
        put_vec3(&mut body, 0x6C, scale_end);
        put_vec3(&mut body, 0x78, [1.0; 3]);
        put_name(&mut body, 0x84, "material.lib");
        put_name(
            &mut body,
            0xA4,
            if is_op9 { "laser_y_hit" } else { "glow_y" },
        );
        if is_op9 {
            put_u32(&mut body, 0xC4, 0);
        } else {
            put_f32(&mut body, 0xC4, 500.0);
        }
        FxCommand {
            word: if is_op9 { 9 } else { 4 },
            opcode,
            native_flag: false,
            raw_body: Arc::from(body),
            resource_refs: Vec::new(),
        }
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

    fn put_name(body: &mut [u8], offset: usize, value: &str) {
        body[offset..offset + value.len()].copy_from_slice(value.as_bytes());
    }

    fn identity_matrix() -> [[f32; 4]; 4] {
        [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ]
    }

    fn uniform_scale_matrix(scale: f32) -> [[f32; 4]; 4] {
        [
            [scale, 0.0, 0.0, 0.0],
            [0.0, scale, 0.0, 0.0],
            [0.0, 0.0, scale, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ]
    }

    fn assert_vec3_close(actual: [f32; 3], expected: [f32; 3]) {
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert!((actual - expected).abs() < 1e-6, "{actual} != {expected}");
        }
    }
}
