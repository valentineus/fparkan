//! CPU-side precipitation and lightning output.
//!
//! The original game keeps atmospheric objects close to the observer.  This
//! module follows that ownership boundary: it advances precipitation particles
//! and native lightning effect lifetimes and returns renderer/audio records
//! for the current frame.  It does not invent a scene graph or hide resource
//! loading behind the simulation.

use crate::sky::SkyMaterials;
use crate::{FxCommand, FxDocument, FxError, FxOpcode};
use std::sync::Arc;

// Terrain.dll writes these velocity vectors into the shared precipitation
// particle state on every update (CSnow at 10045E27 and CRain at 10046555).
// Keep the values together so a renderer cannot accidentally substitute the
// old placeholder speeds.
pub const RAIN_VELOCITY: [f32; 3] = [0.5, 0.0, -60.0];
pub const SNOW_VELOCITY: [f32; 3] = [0.5, 0.0, -4.0];

/// Near plane used by Terrain's precipitation volume constructor at
/// `100444C0`.
pub const PRECIPITATION_NEAR: f32 = 2.0;
/// Far plane used by Terrain's precipitation volume constructor at
/// `100444C0`.
pub const PRECIPITATION_FAR: f32 = 50.0;
/// Half horizontal angle used by Terrain's precipitation volume constructor.
pub const PRECIPITATION_HALF_ANGLE_X: f32 = 1.3 * 0.5;
/// Half vertical angle used by Terrain's precipitation volume constructor.
pub const PRECIPITATION_HALF_ANGLE_Y: f32 = 0.975 * 0.5;
/// Native rain size factor (`Terrain.dll!65E30`).
pub const RAIN_SIZE_FACTOR: f32 = 0.0065;
/// Native snow size factor (`Terrain.dll!65DE8`).
pub const SNOW_SIZE_FACTOR: f32 = 0.0195;
/// Native snow minimum depth fade (`Terrain.dll!65D98`).
pub const SNOW_MIN_DEPTH_FADE: f32 = 0.1;
/// Native precipitation particle-count multiplier (`Terrain.dll!65DE4`).
pub const PARTICLE_COUNT_SCALE: f32 = 1_000.0;

/// Native precipitation spawn volume.  The fields mirror the descriptor
/// built by Terrain at `100444C0`; particles are sampled from this box by
/// `100445B0` using three 16-bit random values divided by 32768.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PrecipitationVolume {
    /// Local-space lower corner `[near, -tan(angle_x)*far, -tan(angle_y)*far]`.
    pub origin: [f32; 3],
    /// Local-space extent `[far-near, 2*tan(angle_x)*far, 2*tan(angle_y)*far]`.
    pub extent: [f32; 3],
    /// Reciprocals stored by the native descriptor for containment tests.
    pub reciprocal_extent: [f32; 3],
}

impl PrecipitationVolume {
    /// Reconstructs the descriptor constants used by Terrain.
    #[must_use]
    pub fn native() -> Self {
        let width = PRECIPITATION_HALF_ANGLE_X.tan() * PRECIPITATION_FAR;
        let height = PRECIPITATION_HALF_ANGLE_Y.tan() * PRECIPITATION_FAR;
        let extent = [
            PRECIPITATION_FAR - PRECIPITATION_NEAR,
            2.0 * width,
            2.0 * height,
        ];
        Self {
            origin: [PRECIPITATION_NEAR, -width, -height],
            extent,
            reciprocal_extent: extent.map(|value| 1.0 / value),
        }
    }

    /// Rebuilds the same depth box for a camera projection.  The native
    /// emitter uses the camera FOV and aspect ratio for this box while the
    /// reference descriptor above keeps the original 1.3 by 0.975 angles.
    #[must_use]
    pub fn for_camera(self, camera: Camera) -> Self {
        let near = self.origin[0];
        let far = near + self.extent[0];
        let half_y = (camera.vertical_fov * 0.5).tan() * far;
        let half_x = (camera.vertical_fov * 0.5).tan() * camera.aspect_ratio * far;
        let extent = [far - near, 2.0 * half_x, 2.0 * half_y];
        Self {
            origin: [near, -half_x, -half_y],
            extent,
            reciprocal_extent: extent.map(|value| 1.0 / value),
        }
    }

    /// Returns an axis-aligned world-space bound around the camera-local
    /// descriptor. The native emitter transforms each sampled local point by
    /// the camera basis before handing it to the particle object.
    #[must_use]
    pub fn bounds(self, camera: Camera) -> PrecipitationBounds {
        let corners: [[f32; 3]; 8] = std::array::from_fn(|index| {
            let local = [
                self.origin[0] + if index & 1 == 0 { 0.0 } else { self.extent[0] },
                self.origin[1] + if index & 2 == 0 { 0.0 } else { self.extent[1] },
                self.origin[2] + if index & 4 == 0 { 0.0 } else { self.extent[2] },
            ];
            camera.world_from_local(local)
        });
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        for corner in corners {
            for axis in 0..3 {
                min[axis] = min[axis].min(corner[axis]);
                max[axis] = max[axis].max(corner[axis]);
            }
        }
        PrecipitationBounds { min, max }
    }

    /// Computes the native reference particle count for a weather intensity.
    /// The camera-aware form below applies the current/reference volume ratio.
    #[must_use]
    pub fn particle_count(self, intensity: f32) -> usize {
        self.particle_count_for(self, 1.0, intensity)
    }

    /// Computes the native count from the current/reference volume ratio and
    /// the object's density scalar.  The original uses FISTP's nearest-even
    /// conversion after multiplying by 1000.
    #[must_use]
    pub fn particle_count_for(self, current: Self, density: f32, intensity: f32) -> usize {
        if !density.is_finite() || !intensity.is_finite() || density <= 0.0 || intensity <= 0.0 {
            return 0;
        }
        let reference_volume = volume_product(self.extent);
        let current_volume = volume_product(current.extent);
        if !reference_volume.is_finite() || reference_volume <= 0.0 || !current_volume.is_finite() {
            return 0;
        }
        let ratio = (current_volume / reference_volume).clamp(0.0, 1.0);
        let value = f64::from(ratio)
            * f64::from(density)
            * f64::from(intensity)
            * f64::from(PARTICLE_COUNT_SCALE);
        if !value.is_finite() || value <= 0.0 {
            0
        } else if value >= usize::MAX as f64 {
            usize::MAX
        } else {
            value.round_ties_even() as usize
        }
    }
}

impl Default for PrecipitationVolume {
    fn default() -> Self {
        Self::native()
    }
}

/// World-space bounds for one observer-local precipitation volume.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PrecipitationBounds {
    /// Inclusive lower corner.
    pub min: [f32; 3],
    /// Inclusive upper corner.
    pub max: [f32; 3],
}

impl PrecipitationBounds {
    /// Checks whether a world-space point lies inside this bound.
    #[must_use]
    pub fn contains(self, point: [f32; 3]) -> bool {
        point
            .into_iter()
            .zip(self.min.into_iter().zip(self.max))
            .all(|(value, (min, max))| value >= min && value <= max)
    }
}

// CLightning::Update (Terrain.dll!10043870) uses a DWORD game tick.
const LIGHTNING_COOLDOWN_MS: u64 = 6_000;
const LIGHTNING_DELAY_MS: f64 = 60_000.0;
const LIGHTNING_INTENSITY_CAP: f32 = 0.95;
// CLightning's effect descriptor stores a 600-unit vertical quad and
// CLightning::CreateEffect (100439D0) adds half of that length to the
// sampled Z coordinate before creating the visual effect.
const LIGHTNING_NATIVE_WIDTH: f32 = 40.0;
const LIGHTNING_NATIVE_LENGTH: f32 = 600.0;
const LIGHTNING_NATIVE_ORIGIN_OFFSET: f32 = 300.0;
/// Effective CLightning descriptor used by the renderer-facing quad.
pub const LIGHTNING_QUAD_DIMENSIONS: [f32; 3] = [
    LIGHTNING_NATIVE_WIDTH,
    LIGHTNING_NATIVE_WIDTH,
    LIGHTNING_NATIVE_LENGTH,
];
const NATIVE_RANDOM_SCALE: f32 = 1.0 / 32_768.0;

/// The CRT linear congruential generator used by Terrain.dll and Effect.dll.
/// Both modules read and update the same process-wide state, so the
/// environment system intentionally owns one stream and passes it through
/// each emitter in update order.
#[derive(Clone, Copy, Debug)]
struct NativeRng {
    state: u32,
}

impl NativeRng {
    fn new(seed: u64) -> Self {
        Self { state: seed as u32 }
    }

    fn next_u15(&mut self) -> u16 {
        self.state = self.state.wrapping_mul(0x343F_D).wrapping_add(0x269E_C);
        ((self.state >> 16) & 0x7FFF) as u16
    }

    fn next_unit(&mut self) -> f32 {
        f32::from(self.next_u15()) * NATIVE_RANDOM_SCALE
    }
}

/// The two precipitation classes present in the original environment data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrecipitationKind {
    /// Rain drops using the `RAIN_DROP` material.
    Rain,
    /// Snow flakes using the `SNOWFLAKE` material.
    Snow,
}

/// A sampled weather state supplied by the atmosphere schedule or a caller.
#[derive(Clone, Debug, PartialEq)]
pub struct WeatherState {
    /// Whether this weather object is currently running.
    pub active: bool,
    /// Non-negative emission strength.  Values above one are capped for the
    /// bounded CPU emitter.
    pub intensity: f32,
    /// RGBA color passed to the renderer.
    pub color: [f32; 4],
    /// Resource names attached to the active interval in `sky.ske`.
    /// Rain uses the first name as its loop sound; lightning uses the FXID
    /// selected by the caller. Keeping these names here avoids silently
    /// substituting a global weather asset.
    pub resources: Vec<String>,
    /// Material row resolved from the current `sky.wea` table.
    pub material: Option<String>,
}

impl WeatherState {
    /// Constructs a weather state.
    #[must_use]
    pub fn new(active: bool, intensity: f32, color: [f32; 4]) -> Self {
        Self {
            active,
            intensity,
            color,
            resources: Vec::new(),
            material: None,
        }
    }

    /// Constructs a weather state with resource names from the atmosphere
    /// key that started its active interval.
    #[must_use]
    pub fn with_resources<I, S>(active: bool, intensity: f32, color: [f32; 4], resources: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            active,
            intensity,
            color,
            resources: resources.into_iter().map(Into::into).collect(),
            material: None,
        }
    }

    /// Constructs an inactive weather state.
    #[must_use]
    pub fn inactive() -> Self {
        Self::new(false, 0.0, [0.0; 4])
    }

    fn sanitized(self) -> Self {
        let color = self.color.map(|component| {
            if component.is_finite() {
                component.clamp(0.0, 1.0)
            } else {
                0.0
            }
        });
        let intensity = if self.intensity.is_finite() {
            self.intensity.clamp(0.0, 1.0)
        } else {
            0.0
        };
        Self {
            active: self.active,
            intensity,
            color,
            resources: self
                .resources
                .into_iter()
                .filter(|resource| !resource.is_empty())
                .collect(),
            material: self.material.filter(|material| !material.is_empty()),
        }
    }
}

impl Default for WeatherState {
    fn default() -> Self {
        Self::inactive()
    }
}

/// Weather input for one environment update.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WeatherInput {
    /// Rain state.
    pub rain: WeatherState,
    /// Snow state.
    pub snow: WeatherState,
    /// Lightning state.
    pub lightning: WeatherState,
}

impl WeatherInput {
    /// Constructs a clear-weather input.
    #[must_use]
    pub fn clear() -> Self {
        Self {
            rain: WeatherState::inactive(),
            snow: WeatherState::inactive(),
            lightning: WeatherState::inactive(),
        }
    }

    /// Converts a sampled `sky.ske` frame into environment input.
    #[must_use]
    pub fn from_atmosphere(frame: &crate::atmosphere::AtmosphereFrame) -> Self {
        Self {
            rain: from_atmosphere_weather(&frame.rain),
            snow: from_atmosphere_weather(&frame.snow),
            lightning: from_atmosphere_weather(&frame.lightning),
        }
    }

    /// Converts a sampled atmosphere frame and the current `sky.wea` table
    /// into environment input. Rows seven and eight are the native snow and
    /// rain material slots; the table is resolved at runtime so missions can
    /// replace their names.
    #[must_use]
    pub fn from_atmosphere_with_materials(
        frame: &crate::atmosphere::AtmosphereFrame,
        materials: &SkyMaterials,
    ) -> Self {
        let mut input = Self::from_atmosphere(frame);
        input.rain.material = materials.material_name(8).map(str::to_owned);
        input.snow.material = materials.material_name(7).map(str::to_owned);
        input
    }

    fn sanitized(self) -> Self {
        Self {
            rain: self.rain.sanitized(),
            snow: self.snow.sanitized(),
            lightning: self.lightning.sanitized(),
        }
    }
}

/// Camera snapshot used to keep finite weather geometry around the observer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    /// World-space camera position.  The renderer uses +Z as up.
    pub position: [f32; 3],
    /// Unit world-space direction of camera-local depth.
    pub forward: [f32; 3],
    /// Unit world-space direction of camera-local horizontal offset.
    pub right: [f32; 3],
    /// Unit world-space direction of camera-local vertical offset.
    pub up: [f32; 3],
    /// Vertical camera field of view in radians.
    pub vertical_fov: f32,
    /// Horizontal-to-vertical projection aspect ratio.
    pub aspect_ratio: f32,
    /// Viewport width and height in pixels used by screen-space particle quads.
    pub viewport: [f32; 2],
}

impl Camera {
    /// Creates a camera snapshot.
    #[must_use]
    pub const fn new(position: [f32; 3]) -> Self {
        Self {
            position,
            forward: [1.0, 0.0, 0.0],
            right: [0.0, 1.0, 0.0],
            up: [0.0, 0.0, 1.0],
            vertical_fov: 0.975,
            // tan(0.65) / tan(0.4875), the original reference box aspect.
            aspect_ratio: 1.4338578,
            viewport: [640.0, 480.0],
        }
    }

    /// Creates a camera snapshot with the orientation used by the native
    /// camera-local precipitation transform.
    #[must_use]
    pub const fn with_basis(
        position: [f32; 3],
        forward: [f32; 3],
        right: [f32; 3],
        up: [f32; 3],
    ) -> Self {
        Self {
            position,
            forward,
            right,
            up,
            vertical_fov: 0.975,
            aspect_ratio: 1.4338578,
            viewport: [640.0, 480.0],
        }
    }

    /// Creates a camera snapshot with orientation and projection data.
    #[must_use]
    pub const fn with_projection(
        position: [f32; 3],
        forward: [f32; 3],
        right: [f32; 3],
        up: [f32; 3],
        vertical_fov: f32,
        aspect_ratio: f32,
    ) -> Self {
        Self {
            position,
            forward,
            right,
            up,
            vertical_fov,
            aspect_ratio,
            viewport: [640.0, 480.0],
        }
    }

    /// Sets the pixel viewport used by the projected particle geometry.
    #[must_use]
    pub const fn with_viewport(self, viewport: [f32; 2]) -> Self {
        Self { viewport, ..self }
    }

    /// Returns the horizontal field of view represented by this projection.
    #[must_use]
    pub fn horizontal_fov(self) -> f32 {
        2.0 * ((self.vertical_fov * 0.5).tan() * self.aspect_ratio).atan()
    }

    /// Returns Terrain's precipitation size query: viewport width divided by
    /// the camera's horizontal FOV in radians.
    #[must_use]
    pub fn precipitation_size_scalar(self) -> f32 {
        let fov = self.horizontal_fov();
        if fov.is_finite() && fov > 0.0 {
            self.viewport[0] / fov
        } else {
            0.0
        }
    }

    /// Transforms a camera-local point into world space.
    #[must_use]
    pub fn world_from_local(self, local: [f32; 3]) -> [f32; 3] {
        [
            self.position[0]
                + self.forward[0] * local[0]
                + self.right[0] * local[1]
                + self.up[0] * local[2],
            self.position[1]
                + self.forward[1] * local[0]
                + self.right[1] * local[1]
                + self.up[1] * local[2],
            self.position[2]
                + self.forward[2] * local[0]
                + self.right[2] * local[1]
                + self.up[2] * local[2],
        ]
    }

    /// Transforms a camera-local vector into world space.
    #[must_use]
    pub fn world_vector(self, local: [f32; 3]) -> [f32; 3] {
        [
            self.forward[0] * local[0] + self.right[0] * local[1] + self.up[0] * local[2],
            self.forward[1] * local[0] + self.right[1] * local[1] + self.up[1] * local[2],
            self.forward[2] * local[0] + self.right[2] * local[1] + self.up[2] * local[2],
        ]
    }

    /// Transforms a world-space point into camera-local coordinates.
    #[must_use]
    pub fn local_from_world(self, world: [f32; 3]) -> [f32; 3] {
        let delta = [
            world[0] - self.position[0],
            world[1] - self.position[1],
            world[2] - self.position[2],
        ];
        [
            dot3(self.forward, delta),
            dot3(self.right, delta),
            dot3(self.up, delta),
        ]
    }

    fn sanitized(self) -> Self {
        let mut camera = self;
        camera.position = camera.position.map(|component| {
            if component.is_finite() {
                component
            } else {
                0.0
            }
        });
        camera.forward = finite_unit(camera.forward).unwrap_or([1.0, 0.0, 0.0]);
        camera.right = finite_unit(camera.right).unwrap_or([0.0, 1.0, 0.0]);
        camera.up = finite_unit(camera.up).unwrap_or([0.0, 0.0, 1.0]);
        if !camera.vertical_fov.is_finite() || camera.vertical_fov <= 0.0 {
            camera.vertical_fov = 0.975;
        }
        camera.vertical_fov = camera.vertical_fov.min(std::f32::consts::PI - 0.001);
        if !camera.aspect_ratio.is_finite() || camera.aspect_ratio <= 0.0 {
            camera.aspect_ratio = 1.4338578;
        }
        for value in &mut camera.viewport {
            if !value.is_finite() || *value <= 0.0 {
                *value = 1.0;
            }
        }
        camera
    }
}

impl Default for Camera {
    fn default() -> Self {
        Self::new([0.0; 3])
    }
}

/// World-space range used by the native lightning position sampler.
///
/// `CLightning::Update` samples X from `[x_min, x_max]`, Y from
/// `[y_min, y_max]`, and copies `z` unchanged.  The atmosphere manager owns
/// these values in the original engine, so the default environment system
/// uses the camera position as a zero-size range until a caller supplies the
/// current bounds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LightningBounds {
    /// Lower and upper X bounds in world coordinates.
    pub x: [f32; 2],
    /// Lower and upper Y bounds in world coordinates.
    pub y: [f32; 2],
    /// Z coordinate copied to the effect origin.
    pub z: f32,
}

impl LightningBounds {
    /// Creates a world-space lightning sampling range.
    #[must_use]
    pub const fn new(x_min: f32, x_max: f32, y_min: f32, y_max: f32, z: f32) -> Self {
        Self {
            x: [x_min, x_max],
            y: [y_min, y_max],
            z,
        }
    }

    fn sanitized(self) -> Option<Self> {
        if self
            .x
            .into_iter()
            .chain(self.y)
            .chain([self.z])
            .all(f32::is_finite)
        {
            Some(self)
        } else {
            None
        }
    }
}

/// Native point-light state produced by an Effect opcode 1 command.
///
/// Effect.dll's opcode 1 handler (`1000F6E0`) updates a LightManager record.
/// The A and B groups are the submitted position and direction, C is the
/// four-component HDR light color, the scalar pair at body offsets 108 and
/// 112 is the light range, and body offsets 120, 124, and 128 are the native
/// attenuation coefficients. These values do not describe lightning mesh
/// geometry or texture coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointLightFrame {
    /// Opcode 1 type word. `env_lightning` uses type 7.
    pub effect_type: u32,
    /// Native LightManager selector selected from the opcode 1 type.
    pub light_selector: u32,
    /// Native LightManager flags selected from the opcode 1 type.
    pub native_flags: u32,
    /// Native update value compared with the opcode 1 start/end fields.
    pub command_time: f32,
    /// Interpolation parameter between the opcode 1 start/end times.
    pub normalized_time: f32,
    /// World-space LightManager position.
    pub position: [f32; 3],
    /// World-space direction submitted alongside the point-light record.
    pub direction: [f32; 3],
    /// HDR RGBA color submitted to LightManager slot +0C.
    pub color: [f32; 4],
    /// Light range submitted to LightManager slot +10.
    pub range: f32,
    /// Native attenuation coefficients `(a0, a1, a2)`.
    pub attenuation: [f32; 3],
    /// The scalar at body offset 116. Its native role is an effect factor,
    /// not alpha or a UV parameter, so it is preserved without reinterpretation.
    pub effect_factor: f32,
    /// Whether the opcode 1 command is active at `command_time`.
    pub active: bool,
}

/// Evaluates a native opcode 1 point-light command at `command_time` and
/// applies a parent basis to its local position and direction. `basis` stores
/// the world-space vectors corresponding to local +X, +Y, and +Z.
/// `command_time` is resolved by the caller; FX header duration and lifecycle
/// remain with that caller. The interpolated range is retained as authored;
/// native Op1 does not clamp it to zero.
#[must_use]
pub fn evaluate_op1_point_light_with_basis(
    command: &FxCommand,
    command_time: f32,
    origin: [f32; 3],
    basis: [[f32; 3]; 3],
) -> Option<PointLightFrame> {
    if command.opcode != FxOpcode::Op1
        || !command_time.is_finite()
        || !origin
            .into_iter()
            .chain(basis.into_iter().flatten())
            .all(f32::is_finite)
    {
        return None;
    }

    let body = command.raw_body.as_ref();
    const FLOAT_OFFSETS: &[usize] = &[
        4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 44, 48, 52, 56, 60, 64, 68, 72, 76, 80, 84, 88, 108,
        112, 116, 120, 124, 128,
    ];
    if body.len() < 132
        || FLOAT_OFFSETS
            .iter()
            .any(|&offset| read_f32(body, offset).is_none_or(|value| !value.is_finite()))
    {
        return None;
    }
    let normalized_time = native_opcode1_interval(body, command_time)?;
    if !normalized_time.is_finite() {
        return None;
    }
    let sample = sample_op1_point_light(body, normalized_time)?;
    let (light_selector, native_flags) = opcode1_light_manager_mapping(sample.effect_type)?;
    if light_selector != 1 {
        return None;
    }
    let transformed_direction = transform3(basis, sample.local_direction);
    let position = add3(origin, transform3(basis, sample.local_position));
    if !position.into_iter().all(f32::is_finite)
        || !transformed_direction.into_iter().all(f32::is_finite)
        || !sample.color.into_iter().all(f32::is_finite)
        || !sample.range.is_finite()
        || !sample.attenuation.into_iter().all(f32::is_finite)
        || !sample.effect_factor.is_finite()
    {
        return None;
    }
    let direction = normalize_op1_direction(transformed_direction);

    Some(PointLightFrame {
        effect_type: sample.effect_type,
        light_selector,
        native_flags,
        command_time,
        normalized_time,
        position,
        direction,
        color: sample.color,
        range: sample.range,
        attenuation: sample.attenuation,
        effect_factor: sample.effect_factor,
        active: true,
    })
}

#[derive(Clone, Copy)]
struct Op1PointLightSample {
    effect_type: u32,
    local_position: [f32; 3],
    local_direction: [f32; 3],
    color: [f32; 4],
    range: f32,
    attenuation: [f32; 3],
    effect_factor: f32,
}

fn sample_op1_point_light(body: &[u8], normalized_time: f32) -> Option<Op1PointLightSample> {
    Some(Op1PointLightSample {
        effect_type: read_u32(body, 0)?,
        local_position: lerp3(read_vec3(body, 12)?, read_vec3(body, 24)?, normalized_time),
        local_direction: lerp3(read_vec3(body, 36)?, read_vec3(body, 48)?, normalized_time),
        color: lerp4(read_vec4(body, 60)?, read_vec4(body, 76)?, normalized_time),
        range: lerp(read_f32(body, 108)?, read_f32(body, 112)?, normalized_time),
        attenuation: [
            read_f32(body, 120)?,
            read_f32(body, 124)?,
            read_f32(body, 128)?,
        ],
        effect_factor: read_f32(body, 116)?,
    })
}

fn normalize_op1_direction(direction: [f32; 3]) -> [f32; 3] {
    // Native FastProc normalization preserves a degenerate vector as zero;
    // this path must not invent a fallback axis.
    let length =
        (direction[0] * direction[0] + direction[1] * direction[1] + direction[2] * direction[2])
            .sqrt();
    if length.is_finite() && length > 0.0 {
        direction.map(|component| component / length)
    } else {
        [0.0; 3]
    }
}

fn lightning_point_light_direction(local_direction: [f32; 3], basis: [[f32; 3]; 3]) -> [f32; 3] {
    // Preserve the pre-existing Lightning consumer fallback, which is
    // separate from the generic native Op1 behavior.
    finite_unit(transform3(basis, local_direction))
        .unwrap_or_else(|| finite_unit(basis[0]).unwrap_or([1.0, 0.0, 0.0]))
}

/// Maps the native opcode 1 type switch to its LightManager record selector
/// and flags. Types 3 and 4 select non-point record classes; generic point
/// lights reject those selectors, while Lightning keeps the full mapping.
fn opcode1_light_manager_mapping(effect_type: u32) -> Option<(u32, u32)> {
    Some(match effect_type {
        1 => (1, 0x8000_0000),
        2 => (1, 0),
        3 => (3, 0),
        4 => (4, 0),
        5 => (1, 0),
        6 => (1, 0xA000_0000),
        7 => (1, 0x2000_0000),
        _ => return None,
    })
}

/// Native quad produced by the opcode 3 lightning visual.
///
/// CLightning creates this effect after moving the sampled position 300
/// units upward. The owner descriptor is `[40, 40, 600]`; opcode 1 does not
/// supply the quad vertices or texture coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LightningQuadFrame {
    /// Four world-space vertices in native perimeter order: bottom plus
    /// horizontal, bottom minus horizontal, top minus horizontal, top plus
    /// horizontal.
    pub vertices: [[f32; 3]; 4],
    /// Native UVs in the same order as `vertices`.
    pub uv: [[f32; 2]; 4],
    /// CLightning's width, height, and effective streak length.
    pub dimensions: [f32; 3],
    /// Opcode 3 animated local position from the native start/delta channels.
    pub local_offset: [f32; 3],
    /// Opcode 3 animated local scale from the native start/end channels.
    pub local_scale: [f32; 3],
    /// Native opcode 3 alpha channel. `env_lightning` fades from one to zero
    /// over its active interval.
    pub opacity: f32,
    /// Effect age supplied to the native opcode 3 evaluator, in seconds.
    pub elapsed_seconds: f32,
    /// Interpolation parameter between the opcode 3 start/end times.
    pub normalized_time: f32,
    /// FXID lifetime in seconds.
    pub duration_seconds: f32,
    /// Whether the opcode 3 command is active at `elapsed_seconds`.
    pub active: bool,
}

/// Screen-space quad data produced by the native rain/snow geometry paths.
/// `head`, `tail`, and `corners` are normalized device coordinates. The native
/// size remains in viewport pixels so a renderer can apply the same query
/// without mixing pixel and NDC units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScreenBillboard {
    /// World-space head point.
    pub world_head: [f32; 3],
    /// World-space tail point.
    pub world_tail: [f32; 3],
    /// Projected head point in NDC.
    pub head: [f32; 2],
    /// Projected tail point in NDC. Snow uses the same point as `head`.
    pub tail: [f32; 2],
    /// Four NDC corners in tail-left, tail-right, head-left, head-right order.
    pub corners: [[f32; 2]; 4],
    /// Texture coordinates for the four corners.
    pub uv: [[f32; 2]; 4],
    /// Native half-size argument in viewport pixels.
    pub half_size: f32,
    /// Camera-local depth of the projected head.
    pub head_depth: f32,
    /// Camera-local depth of the projected tail.
    pub tail_depth: f32,
}

/// Geometry and light output decoded from the FXID.
#[derive(Clone, Debug, PartialEq)]
pub struct LightningBolt {
    /// Material archive and name resolved from opcode 3.
    pub material: EffectResource,
    /// World-space sampled position supplied to CLightning.
    pub position: [f32; 3],
    /// Opcode 3 world-space quad and material state.
    pub quad: LightningQuadFrame,
    /// Current weather intensity used by the event scheduler.
    pub intensity: f32,
    /// Effect age in seconds from its FXID start time.
    pub age_seconds: f32,
    /// FXID lifetime in seconds used by the native evaluator.
    pub duration_seconds: f32,
}

/// Native environment primitives consumed by the renderer.
#[derive(Clone, Debug, PartialEq)]
pub enum EnvironmentPrimitive {
    /// A rain drop or snow flake billboard instance.
    Particle {
        /// Precipitation class.
        kind: PrecipitationKind,
        /// World-space position.
        position: [f32; 3],
        /// World-space velocity.
        velocity: [f32; 3],
        /// Native size argument after applying the rain/snow size factor.
        size: f32,
        /// RGBA color.
        color: [f32; 4],
        /// Material lookup name from `sky.wea`.
        material: String,
        /// Native projected quad geometry in viewport pixels.
        screen: ScreenBillboard,
    },
    /// A lightning bolt with opcode 3 quad data.
    Lightning(LightningBolt),
}

/// Action applied to a sound resource by the caller's audio backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SoundAction {
    /// Start a loop at the event's volume.
    StartLoop,
    /// Change the volume of an already-running loop.
    SetLoopVolume,
    /// Stop a previously started loop.
    StopLoop,
    /// Play one sample once.
    OneShot,
}

/// One sound event generated by the environment simulation.
#[derive(Clone, Debug, PartialEq)]
pub struct SoundEvent {
    /// Archive to resolve through the resource repository. Empty means the
    /// audio owner should use its mission-configured weather library.
    pub archive: String,
    /// Entry name in `archive`.
    pub name: String,
    /// World-space emitter position.
    pub position: [f32; 3],
    /// Linear volume multiplier.
    pub volume: f32,
    /// Native minimum distance in world units. A non-positive value disables
    /// the native range clamp for callers such as atmosphere loops.
    pub min_distance: f32,
    /// Native maximum distance in world units. A non-positive or non-finite
    /// value disables the native range clamp.
    pub max_distance: f32,
    /// Native playback-frequency ratio evaluated at event creation.
    pub frequency_ratio: f32,
    /// Playback action.
    pub action: SoundAction,
}

/// Native opcode-2 sound parameters evaluated for one effect age.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SoundParameters {
    /// DirectSound minimum distance in world units.
    pub min_distance: f32,
    /// DirectSound maximum distance in world units.
    pub max_distance: f32,
    /// Playback frequency ratio. The shipped thunder effect keeps this at 1.
    pub frequency_ratio: f32,
}

impl Default for SoundParameters {
    fn default() -> Self {
        Self {
            min_distance: 0.0,
            max_distance: f32::INFINITY,
            frequency_ratio: 1.0,
        }
    }
}

/// All visible and audible environment output for one frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EnvironmentFrame {
    /// Renderer primitives in stable emitter order.
    pub primitives: Vec<EnvironmentPrimitive>,
    /// Dynamic point lights emitted by native Effect opcode 1 commands.
    pub point_lights: Vec<PointLightFrame>,
    /// Sound transitions and one-shot events.
    pub sounds: Vec<SoundEvent>,
}

/// A validated resource name from the `env_lightning` FXID.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectResource {
    /// Archive name.
    pub archive: String,
    /// Entry name.
    pub name: String,
}

/// The real visual and sound contracts extracted from `env_lightning`.
#[derive(Clone, Debug, PartialEq)]
pub struct LightningEffect {
    /// FXID header lifetime in seconds.
    pub duration_seconds: f32,
    /// Visual material resource from the opcode 3 body.
    pub visual: EffectResource,
    /// Complete opcode-3 body used by the native quad evaluator.
    pub visual_body: Arc<[u8]>,
    /// Complete opcode-1 body for the native Effect evaluator.
    pub opcode1_body: Arc<[u8]>,
    /// Command index containing those visual parameters.
    pub opcode1_command_index: usize,
    /// Sound resource from the opcode 2 body.
    pub sound: EffectResource,
    /// Complete opcode-2 body used to evaluate the native thunder gain and
    /// active interval at the event origin.
    pub sound_body: Arc<[u8]>,
    /// Command index containing the visual material.
    pub visual_command_index: usize,
    /// Command index containing the sound resource.
    pub sound_command_index: usize,
}

impl LightningEffect {
    /// Extracts the visual, point-light, and sound contracts from a decoded
    /// lightning FXID.
    ///
    /// The generic FXID parser intentionally preserves command bodies without
    /// claiming field semantics. Opcode 3 has a material reference at body
    /// offset 132, opcode 1 carries the LightManager record, and opcode 2
    /// has a sound reference at body offsets 80 and 112.
    ///
    /// # Errors
    ///
    /// Returns an error if the document does not contain those commands or
    /// contains truncated command bodies.
    pub fn from_fxid(document: &FxDocument) -> Result<Self, EnvironmentError> {
        let duration_seconds = document.header().duration_seconds;
        if !duration_seconds.is_finite() || duration_seconds <= 0.0 {
            return Err(EnvironmentError::InvalidDuration { duration_seconds });
        }

        let mut visual = None;
        let mut opcode1 = None;
        let mut sound = None;
        for (index, command) in document.commands().iter().enumerate() {
            match command.opcode {
                FxOpcode::Op3 if visual.is_none() => {
                    let resource = read_resource(index, command.opcode, &command.raw_body, 132)?;
                    validate_opcode3_body(index, &command.raw_body)?;
                    visual = Some((index, resource, Arc::clone(&command.raw_body)));
                }
                FxOpcode::Op1 if opcode1.is_none() => {
                    validate_opcode1_body(index, &command.raw_body)?;
                    opcode1 = Some((index, Arc::clone(&command.raw_body)));
                }
                FxOpcode::Op2 if sound.is_none() => {
                    validate_opcode2_body(index, &command.raw_body)?;
                    let resource = read_resource(index, command.opcode, &command.raw_body, 80)?;
                    let name = read_name(&command.raw_body, index, command.opcode, 112)?;
                    sound = Some((
                        index,
                        EffectResource {
                            archive: resource.archive,
                            name,
                        },
                        Arc::clone(&command.raw_body),
                    ));
                }
                _ => {}
            }
        }

        let Some((visual_command_index, visual, visual_body)) = visual else {
            return Err(EnvironmentError::MissingCommand {
                opcode: FxOpcode::Op3,
            });
        };
        let Some((opcode1_command_index, opcode1_body)) = opcode1 else {
            return Err(EnvironmentError::MissingCommand {
                opcode: FxOpcode::Op1,
            });
        };
        let Some((sound_command_index, sound, sound_body)) = sound else {
            return Err(EnvironmentError::MissingCommand {
                opcode: FxOpcode::Op2,
            });
        };
        Ok(Self {
            duration_seconds,
            visual,
            visual_body,
            opcode1_body,
            opcode1_command_index,
            sound,
            sound_body,
            visual_command_index,
            sound_command_index,
        })
    }

    /// Evaluates the native opcode-2 distance and playback parameters at an
    /// effect age. The FX command stores minimum/maximum distance at body
    /// offsets `0x3c`/`0x40` and interpolates the frequency ratio from
    /// `0x44` to `0x48`; body offset `0x4c` remains in the preserved raw
    /// command because its backend meaning is not established.
    #[must_use]
    pub fn sound_parameters(&self, elapsed_seconds: f32) -> Option<SoundParameters> {
        if !elapsed_seconds.is_finite()
            || !self.duration_seconds.is_finite()
            || self.duration_seconds <= 0.0
        {
            return None;
        }
        let (normalized_time, _) = active_interval(
            self.sound_body.as_ref(),
            elapsed_seconds,
            self.duration_seconds,
        )?;
        let min_distance = read_f32(self.sound_body.as_ref(), 60)?;
        let max_distance = read_f32(self.sound_body.as_ref(), 64)?;
        let frequency_ratio = lerp(
            read_f32(self.sound_body.as_ref(), 68)?,
            read_f32(self.sound_body.as_ref(), 72)?,
            normalized_time,
        );
        if !min_distance.is_finite() || !max_distance.is_finite() || !frequency_ratio.is_finite() {
            return None;
        }
        Some(SoundParameters {
            min_distance,
            max_distance,
            frequency_ratio,
        })
    }

    /// Evaluates the native opcode 1 point-light command at `elapsed_seconds`.
    #[must_use]
    pub fn evaluate_point_light(
        &self,
        elapsed_seconds: f32,
        origin: [f32; 3],
    ) -> Option<PointLightFrame> {
        self.evaluate_point_light_with_basis(elapsed_seconds, origin, identity_basis())
    }

    /// Evaluates opcode 1 and applies a parent basis to its LightManager
    /// position and direction. `basis` stores the world-space vectors
    /// corresponding to local +X, +Y, and +Z.
    #[must_use]
    pub fn evaluate_point_light_with_basis(
        &self,
        elapsed_seconds: f32,
        origin: [f32; 3],
        basis: [[f32; 3]; 3],
    ) -> Option<PointLightFrame> {
        if !self.valid_effect_time(elapsed_seconds, origin, basis) {
            return None;
        }

        let body = self.opcode1_body.as_ref();
        let (normalized_time, _) = active_interval(body, elapsed_seconds, self.duration_seconds)?;
        let sample = sample_op1_point_light(body, normalized_time)?;
        let (light_selector, native_flags) =
            opcode1_light_manager_mapping(sample.effect_type).unwrap_or((0, 0));
        let direction = lightning_point_light_direction(sample.local_direction, basis);

        Some(PointLightFrame {
            effect_type: sample.effect_type,
            light_selector,
            native_flags,
            command_time: elapsed_seconds,
            normalized_time,
            position: add3(origin, transform3(basis, sample.local_position)),
            direction,
            color: sample.color.map(sanitize_float),
            range: sanitize_float(sample.range).max(0.0),
            attenuation: sample.attenuation.map(sanitize_float),
            effect_factor: sanitize_float(sample.effect_factor),
            active: true,
        })
    }

    /// Evaluates the native opcode 3 visual quad at `elapsed_seconds`.
    ///
    /// The native owner supplies the effective descriptor `[40, 40, 600]`
    /// and the effect origin after adding 300 units to the sampled Z. Opcode
    /// 3 contributes the material modifiers and active interval; its fixed
    /// full-texture UVs are independent of opcode 1.
    #[must_use]
    pub fn evaluate_quad(
        &self,
        elapsed_seconds: f32,
        origin: [f32; 3],
        camera: Camera,
    ) -> Option<LightningQuadFrame> {
        self.evaluate_quad_with_mirror(elapsed_seconds, origin, camera, false)
    }

    /// Evaluates the native opcode 3 visual quad with the per-bolt mirrored
    /// texture orientation selected by `CLightning::CreateEffect`.
    #[must_use]
    pub fn evaluate_quad_with_mirror(
        &self,
        elapsed_seconds: f32,
        origin: [f32; 3],
        camera: Camera,
        mirrored: bool,
    ) -> Option<LightningQuadFrame> {
        let camera = camera.sanitized();
        if !self.valid_effect_time(elapsed_seconds, origin, identity_basis()) {
            return None;
        }
        let body = self.visual_body.as_ref();
        let (normalized_time, _) = active_interval(body, elapsed_seconds, self.duration_seconds)?;
        let local_offset = evaluate_position(body, normalized_time)?;
        let local_scale = evaluate_scale(body, normalized_time)?;
        // The owner matrix is diagonal. The mirrored path negates its local
        // Y basis; that sign participates in both the inverse camera
        // transform and the forward transform of the billboard frame.
        // Effect.dll first computes `world_center = M_base * local_offset`,
        // then builds `M_base * Diag(local_scale)`. It never applies the
        // animated scale to the inverse camera vector.
        let base_diag = lightning_parent_diagonal(mirrored);
        let mut geometry_diag = base_diag.map(|value| value);
        for (axis, scale) in geometry_diag.iter_mut().zip(local_scale) {
            *axis *= scale;
        }
        let dimensions = [
            LIGHTNING_QUAD_DIMENSIONS[0] * local_scale[0],
            LIGHTNING_QUAD_DIMENSIONS[1] * local_scale[1],
            LIGHTNING_QUAD_DIMENSIONS[2] * local_scale[2],
        ];
        let center = add3(
            origin,
            [
                base_diag[0] * local_offset[0],
                base_diag[1] * local_offset[1],
                base_diag[2] * local_offset[2],
            ],
        );

        // The shipped env_lightning command is type 1. Its native receiver
        // builds a local frame with columns `(axis, U, W)`, where
        // `U = normalize(V x axis)` and `W = axis x U`. The canonical quad
        // uses the first two columns; the third column supplies the
        // camera-facing normal to the native receiver. `V` is intentionally
        // not normalized before the exact parallel-view test.
        let axis = [0.0, 0.0, 1.0];
        let camera_local = [
            (camera.position[0] - origin[0]) / base_diag[0],
            (camera.position[1] - origin[1]) / base_diag[1],
            (camera.position[2] - origin[2]) / base_diag[2],
        ];
        let raw_view = sub3(camera_local, local_offset);
        // Native type 1 compares the raw dot product against exactly +/-1.
        // A near-parallel view therefore follows the cross-product path.
        if dot3(raw_view, axis).abs() == 1.0 {
            return None;
        }
        let horizontal_local = normalized_or_fallback(cross3(raw_view, axis), [1.0, 0.0, 0.0]);
        let _normal_local = cross3(axis, horizontal_local);
        let horizontal = transform_diagonal(geometry_diag, horizontal_local);
        let vertical = transform_diagonal(geometry_diag, axis);
        let bottom = sub3(center, scale3(vertical, 0.5));
        let top = add3(center, scale3(vertical, 0.5));
        let positive_horizontal = scale3(horizontal, 0.5);
        let negative_horizontal = scale3(horizontal, -0.5);
        Some(LightningQuadFrame {
            vertices: [
                add3(bottom, positive_horizontal),
                add3(bottom, negative_horizontal),
                add3(top, negative_horizontal),
                add3(top, positive_horizontal),
            ],
            uv: [[0.0, 0.99], [0.0, 0.0], [0.99, 0.0], [0.99, 0.99]],
            dimensions,
            local_offset: local_offset.map(sanitize_float),
            local_scale: local_scale.map(sanitize_float),
            opacity: sanitize_float(1.0 - normalized_time).clamp(0.0, 1.0),
            elapsed_seconds,
            normalized_time,
            duration_seconds: self.duration_seconds,
            active: true,
        })
    }

    fn valid_effect_time(
        &self,
        elapsed_seconds: f32,
        origin: [f32; 3],
        basis: [[f32; 3]; 3],
    ) -> bool {
        elapsed_seconds.is_finite()
            && self.duration_seconds.is_finite()
            && self.duration_seconds > 0.0
            && elapsed_seconds >= 0.0
            && elapsed_seconds <= self.duration_seconds
            && origin.iter().all(|value| value.is_finite())
            && basis.iter().flatten().all(|value| value.is_finite())
    }
}

/// Decodes and validates the original `env_lightning` FXID payload.
pub fn decode_env_lightning_fxid(bytes: Arc<[u8]>) -> Result<LightningEffect, EnvironmentError> {
    let document = crate::decode_fxid(bytes)?;
    LightningEffect::from_fxid(&document)
}

/// Environment simulation failure.
#[derive(Debug)]
pub enum EnvironmentError {
    /// Generic FXID framing error.
    Fx(FxError),
    /// FXID lifetime is unusable for an active effect.
    InvalidDuration {
        /// Value from the FXID header.
        duration_seconds: f32,
    },
    /// Required command is absent.
    MissingCommand {
        /// Required opcode.
        opcode: FxOpcode,
    },
    /// A command body is too short for the established layout.
    InvalidCommandLayout {
        /// Command index.
        command_index: usize,
        /// Command opcode.
        opcode: FxOpcode,
        /// Required body length.
        required: usize,
        /// Actual body length.
        actual: usize,
    },
    /// A known command points at an unexpected resource.
    UnexpectedResource {
        /// Command index.
        command_index: usize,
        /// Resource field being checked.
        part: &'static str,
        /// Decoded archive name.
        archive: String,
        /// Decoded resource name.
        name: String,
    },
    /// Opcode 1 contains a non-finite visual parameter.
    NonFiniteVisualValue {
        /// Command index.
        command_index: usize,
        /// Value index in the decoded prefix.
        index: usize,
        /// Value read from the command body.
        value: f32,
    },
    /// Opcode 2 contains a non-finite sound parameter.
    NonFiniteSoundValue {
        /// Command index.
        command_index: usize,
        /// Value index in the numeric sound prefix.
        index: usize,
        /// Value read from the command body.
        value: f32,
    },
}

impl From<FxError> for EnvironmentError {
    fn from(value: FxError) -> Self {
        Self::Fx(value)
    }
}

impl std::fmt::Display for EnvironmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fx(error) => write!(f, "{error}"),
            Self::InvalidDuration { duration_seconds } => {
                write!(f, "invalid env_lightning FXID duration {duration_seconds}")
            }
            Self::MissingCommand { opcode } => {
                write!(f, "env_lightning is missing required {opcode:?} command")
            }
            Self::InvalidCommandLayout {
                command_index,
                opcode,
                required,
                actual,
            } => write!(
                f,
                "env_lightning command {command_index} ({opcode:?}) needs {required} body bytes, found {actual}"
            ),
            Self::UnexpectedResource {
                command_index,
                part,
                archive,
                name,
            } => write!(
                f,
                "unexpected env_lightning {part} at command {command_index}: {archive}/{name}"
            ),
            Self::NonFiniteVisualValue {
                command_index,
                index,
                value,
            } => write!(
                f,
                "env_lightning opcode 1 command {command_index} has non-finite visual value {index}: {value}"
            ),
            Self::NonFiniteSoundValue {
                command_index,
                index,
                value,
            } => write!(
                f,
                "env_lightning opcode 2 command {command_index} has non-finite sound value {index}: {value}"
            ),
        }
    }
}

impl std::error::Error for EnvironmentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Fx(error) => Some(error),
            Self::InvalidDuration { .. }
            | Self::MissingCommand { .. }
            | Self::InvalidCommandLayout { .. }
            | Self::UnexpectedResource { .. }
            | Self::NonFiniteVisualValue { .. }
            | Self::NonFiniteSoundValue { .. } => None,
        }
    }
}

/// Stateful CPU environment emitter.
#[derive(Clone, Debug)]
pub struct EnvironmentSystem {
    time_seconds: f32,
    time_millis: u64,
    time_millis_fraction: f64,
    /// Terrain and Effect share the CRT `rand` state. Keep one stream for
    /// particle respawns, lightning scheduling, and lightning placement so
    /// the native call order remains observable across subsystems.
    rng: NativeRng,
    rain: PrecipitationEmitter,
    snow: PrecipitationEmitter,
    materials: SkyMaterials,
    lightning_effect: Option<LightningEffect>,
    lightning_bounds: Option<LightningBounds>,
    lightning: LightningState,
    rain_loop_active: bool,
    rain_loop_resource: Option<String>,
}

impl EnvironmentSystem {
    /// Creates a deterministic environment system.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            time_seconds: 0.0,
            time_millis: 0,
            time_millis_fraction: 0.0,
            rng: NativeRng::new(seed),
            rain: PrecipitationEmitter::new(PrecipitationKind::Rain, PrecipitationVolume::native()),
            snow: PrecipitationEmitter::new(PrecipitationKind::Snow, PrecipitationVolume::native()),
            materials: SkyMaterials::default(),
            lightning_effect: None,
            lightning_bounds: None,
            lightning: LightningState::new(),
            rain_loop_active: false,
            rain_loop_resource: None,
        }
    }

    /// Installs the mission's `sky.wea` material table. The rain and snow
    /// emitters resolve rows eight and seven from this table on every frame.
    pub fn set_sky_materials(&mut self, materials: SkyMaterials) {
        self.materials = materials;
    }

    /// Returns the active precipitation descriptor.
    #[must_use]
    pub fn precipitation_volume(&self) -> PrecipitationVolume {
        self.rain.volume
    }

    /// Installs a validated `env_lightning` effect contract.
    pub fn set_lightning_effect(&mut self, effect: LightningEffect) {
        self.lightning_effect = effect
            .duration_seconds
            .is_finite()
            .then_some(effect)
            .filter(|effect| effect.duration_seconds > 0.0);
        self.lightning.clear_visual();
    }

    /// Removes the installed lightning effect.
    pub fn clear_lightning_effect(&mut self) {
        self.lightning_effect = None;
        self.lightning.clear_visual();
    }

    /// Supplies the current world-space bounds used by the native lightning
    /// sampler. Passing non-finite bounds makes the system fall back to the
    /// camera position for subsequent flashes.
    pub fn set_lightning_bounds(&mut self, bounds: LightningBounds) {
        self.lightning_bounds = bounds.sanitized();
    }

    /// Removes explicit lightning bounds. Subsequent effects use the camera
    /// position as a zero-size range.
    pub fn clear_lightning_bounds(&mut self) {
        self.lightning_bounds = None;
    }

    /// Current simulation time in seconds.
    #[must_use]
    pub fn time_seconds(&self) -> f32 {
        self.time_seconds
    }

    /// Current simulation time in whole native milliseconds.
    #[must_use]
    pub fn time_millis(&self) -> u64 {
        self.time_millis
    }

    /// Advances precipitation and lightning and returns renderer/audio output.
    ///
    /// `dt_seconds` is sanitized to a finite non-negative value.  Particle
    /// positions are retained between frames and wrapped in the camera-local
    /// descriptor using the native floor-based wrap operation, so a renderer
    /// can consume this frame as a direct list of instances.
    #[must_use]
    pub fn update(
        &mut self,
        dt_seconds: f32,
        weather: WeatherInput,
        camera: Camera,
    ) -> EnvironmentFrame {
        let dt_seconds = if dt_seconds.is_finite() {
            dt_seconds.max(0.0)
        } else {
            0.0
        };
        let weather = weather.sanitized();
        let camera = camera.sanitized();
        self.time_seconds = self.time_seconds.max(0.0) + dt_seconds;
        self.advance_millis(dt_seconds);

        let mut frame = EnvironmentFrame::default();
        self.rain.update(
            dt_seconds,
            &weather.rain,
            camera,
            self.materials.material_name(8),
            &mut self.rng,
            &mut frame.primitives,
        );
        self.snow.update(
            dt_seconds,
            &weather.snow,
            camera,
            self.materials.material_name(7),
            &mut self.rng,
            &mut frame.primitives,
        );

        let rain_resource = weather.rain.resources.first().cloned();
        if weather.rain.active && self.rain_loop_active && rain_resource != self.rain_loop_resource
        {
            if let Some(name) = self.rain_loop_resource.clone() {
                let (archive, name) = split_resource(&name);
                frame.sounds.push(SoundEvent {
                    archive,
                    name,
                    position: camera.position,
                    volume: 0.0,
                    min_distance: 0.0,
                    max_distance: f32::INFINITY,
                    frequency_ratio: 1.0,
                    action: SoundAction::StopLoop,
                });
            }
        }
        if weather.rain.active != self.rain_loop_active
            || (weather.rain.active && rain_resource != self.rain_loop_resource)
        {
            let (resource, action) = if weather.rain.active {
                (rain_resource.clone(), SoundAction::StartLoop)
            } else {
                (self.rain_loop_resource.clone(), SoundAction::StopLoop)
            };
            if let Some(resource) = resource {
                let (archive, name) = split_resource(&resource);
                frame.sounds.push(SoundEvent {
                    archive,
                    name,
                    position: camera.position,
                    volume: weather.rain.intensity,
                    min_distance: 0.0,
                    max_distance: f32::INFINITY,
                    frequency_ratio: 1.0,
                    action,
                });
            }
        } else if weather.rain.active {
            // Atmosphere intensity is continuously interpolated between
            // keys.  The native rain object keeps its loop alive while that
            // value changes, so the audio owner must receive the current
            // volume even when the lifecycle state is unchanged.
            if let Some(name) = rain_resource.clone() {
                let (archive, name) = split_resource(&name);
                frame.sounds.push(SoundEvent {
                    archive,
                    name,
                    position: camera.position,
                    volume: weather.rain.intensity,
                    min_distance: 0.0,
                    max_distance: f32::INFINITY,
                    frequency_ratio: 1.0,
                    action: SoundAction::SetLoopVolume,
                });
            }
        }
        self.rain_loop_active = weather.rain.active;
        self.rain_loop_resource = weather.rain.active.then_some(rain_resource).flatten();

        self.lightning.update(
            self.time_millis,
            weather.lightning,
            camera,
            self.lightning_effect.as_ref(),
            self.lightning_bounds,
            &mut self.rng,
            &mut frame.primitives,
            &mut frame.point_lights,
            &mut frame.sounds,
        );

        frame
    }

    /// Advances from an `AtmosphereFrame` sampled by the `sky.ske` parser.
    #[must_use]
    pub fn update_atmosphere(
        &mut self,
        dt_seconds: f32,
        frame: &crate::atmosphere::AtmosphereFrame,
        camera: Camera,
    ) -> EnvironmentFrame {
        self.update(dt_seconds, WeatherInput::from_atmosphere(frame), camera)
    }

    /// Advances from a sampled `sky.ske` frame and its mission `sky.wea`
    /// material table.
    #[must_use]
    pub fn update_atmosphere_with_materials(
        &mut self,
        dt_seconds: f32,
        frame: &crate::atmosphere::AtmosphereFrame,
        materials: &SkyMaterials,
        camera: Camera,
    ) -> EnvironmentFrame {
        self.set_sky_materials(materials.clone());
        self.update(
            dt_seconds,
            WeatherInput::from_atmosphere_with_materials(frame, materials),
            camera,
        )
    }

    fn advance_millis(&mut self, dt_seconds: f32) {
        let millis = self.time_millis_fraction + f64::from(dt_seconds) * 1_000.0;
        if !millis.is_finite() || millis <= 0.0 {
            return;
        }
        let whole = millis.floor();
        self.time_millis = self
            .time_millis
            .saturating_add(if whole >= u64::MAX as f64 {
                u64::MAX
            } else {
                whole as u64
            });
        self.time_millis_fraction = millis - whole;
    }
}

#[derive(Clone, Debug)]
struct PrecipitationEmitter {
    kind: PrecipitationKind,
    volume: PrecipitationVolume,
    particles: Vec<ParticleState>,
}

#[derive(Clone, Copy, Debug)]
struct ParticleState {
    /// World-space head position. Native integration advances this directly
    /// before converting it to camera-local coordinates for wrapping.
    position: [f32; 3],
    /// Previous world-space head position, used as the rain tail.
    previous_position: [f32; 3],
    respawns: u64,
}

impl PrecipitationEmitter {
    fn new(kind: PrecipitationKind, volume: PrecipitationVolume) -> Self {
        Self {
            kind,
            volume,
            particles: Vec::new(),
        }
    }

    fn update(
        &mut self,
        dt_seconds: f32,
        state: &WeatherState,
        camera: Camera,
        default_material: Option<&str>,
        rng: &mut NativeRng,
        out: &mut Vec<EnvironmentPrimitive>,
    ) {
        if !state.active || state.intensity <= 0.0 {
            return;
        }
        let Some(material) = state
            .material
            .as_deref()
            .or(default_material)
            .filter(|material| !material.is_empty())
        else {
            return;
        };
        let current_volume = self.volume.for_camera(camera);
        let desired = self
            .volume
            .particle_count_for(current_volume, 1.0, state.intensity);
        self.particles.truncate(desired);
        while self.particles.len() < desired {
            let index = self.particles.len();
            self.particles.push(ParticleState {
                position: [0.0; 3],
                previous_position: [0.0; 3],
                respawns: 0,
            });
            self.respawn(index, camera, current_volume, rng);
        }

        let velocity = self.velocity();
        let kind = self.kind;
        let size_factor = match kind {
            PrecipitationKind::Rain => RAIN_SIZE_FACTOR,
            PrecipitationKind::Snow => SNOW_SIZE_FACTOR,
        };
        let size = camera.precipitation_size_scalar() * size_factor;
        for index in 0..desired {
            let particle = &mut self.particles[index];
            particle.previous_position = particle.position;
            particle.position[0] += velocity[0] * dt_seconds;
            particle.position[1] += velocity[1] * dt_seconds;
            particle.position[2] += velocity[2] * dt_seconds;
            let local = camera.local_from_world(particle.position);
            let mut wrapped_local = local;
            let mut wrapped = false;
            for axis in 0..3 {
                let offset = local[axis] - current_volume.origin[axis];
                let wrapped_offset = wrap_local(offset, current_volume.extent[axis]);
                wrapped_local[axis] = current_volume.origin[axis] + wrapped_offset;
                wrapped |= (wrapped_offset - offset).abs() > f32::EPSILON;
            }
            if wrapped {
                // A wrapped particle has no continuous temporal tail across
                // the opposite faces of the volume.
                particle.position = camera.world_from_local(wrapped_local);
                particle.previous_position = particle.position;
            }
            let position = particle.position;
            let tail = particle.previous_position;
            out.push(EnvironmentPrimitive::Particle {
                kind,
                position,
                velocity,
                size,
                color: state.color,
                material: material.to_owned(),
                screen: screen_billboard(kind, camera, position, tail, size, index),
            });
        }
    }

    fn respawn(
        &mut self,
        index: usize,
        camera: Camera,
        volume: PrecipitationVolume,
        rng: &mut NativeRng,
    ) {
        let particle = &mut self.particles[index];
        let local = [
            rng.next_unit() * volume.extent[0],
            rng.next_unit() * volume.extent[1],
            rng.next_unit() * volume.extent[2],
        ];
        particle.position = camera.world_from_local(add3(volume.origin, local));
        particle.previous_position = particle.position;
        particle.respawns = particle.respawns.wrapping_add(1);
    }

    fn velocity(&self) -> [f32; 3] {
        match self.kind {
            PrecipitationKind::Rain => RAIN_VELOCITY,
            PrecipitationKind::Snow => SNOW_VELOCITY,
        }
    }
}

#[derive(Clone, Debug)]
struct LightningState {
    phase: u8,
    last_time_ms: u64,
    deadline_ms: u64,
    active: bool,
    visual: Option<LightningRuntime>,
}

#[derive(Clone, Debug)]
struct LightningRuntime {
    effect: LightningEffect,
    position: [f32; 3],
    intensity: f32,
    mirrored: bool,
    started_ms: u64,
}

impl LightningState {
    fn new() -> Self {
        Self {
            // CLightning's constructor stores state 1, which means the first
            // active update schedules a delay immediately.
            phase: 1,
            last_time_ms: 0,
            deadline_ms: 0,
            active: false,
            visual: None,
        }
    }

    fn clear_visual(&mut self) {
        self.visual = None;
    }

    fn update(
        &mut self,
        now_ms: u64,
        weather: WeatherState,
        camera: Camera,
        effect: Option<&LightningEffect>,
        bounds: Option<LightningBounds>,
        rng: &mut NativeRng,
        primitives: &mut Vec<EnvironmentPrimitive>,
        point_lights: &mut Vec<PointLightFrame>,
        sounds: &mut Vec<SoundEvent>,
    ) {
        if !weather.active {
            // The atmosphere destroys and recreates the lightning object on a
            // stop/start transition. Reproduce that lifecycle so an old
            // deadline cannot fire after clear weather.
            self.active = false;
            self.phase = 1;
            self.last_time_ms = 0;
            self.deadline_ms = 0;
            self.visual = None;
            return;
        }
        if !self.active {
            self.active = true;
            self.phase = 1;
            self.last_time_ms = 0;
            self.deadline_ms = 0;
        }

        self.emit_visual(now_ms, weather.color, camera, primitives, point_lights);

        // Keep the state machine's one-transition-per-call behaviour. The
        // original virtual update returns after changing state 0->1 or
        // state 1->2; it does not catch up multiple lightning events after a
        // long frame.
        match self.phase {
            0 => {
                if now_ms.saturating_sub(self.last_time_ms) >= LIGHTNING_COOLDOWN_MS {
                    self.phase = 1;
                }
            }
            1 => {
                if weather.intensity <= 0.0 {
                    return;
                }
                let intensity = weather.intensity.min(LIGHTNING_INTENSITY_CAP);
                let random = rng.next_unit();
                let delay = ((1.0 - f64::from(intensity)) * LIGHTNING_DELAY_MS * f64::from(random))
                    .round_ties_even();
                let delay = if delay.is_finite() && delay >= 0.0 {
                    delay.min(u64::MAX as f64) as u64
                } else {
                    0
                };
                self.deadline_ms = now_ms.saturating_add(delay);
                self.phase = 2;
            }
            2 => {
                if now_ms < self.deadline_ms {
                    return;
                }
                let position = self.random_position(camera, bounds, rng);
                if let Some(effect) = effect {
                    // CLightning consumes one CRT random value for the
                    // effect seed after sampling the X and Y coordinates.
                    // The seed has no renderer-facing field, but it remains
                    // part of the shared native stream.
                    let effect_seed = rng.next_unit();
                    self.visual = Some(LightningRuntime {
                        effect: effect.clone(),
                        position,
                        intensity: weather.intensity,
                        mirrored: effect_seed < 0.5,
                        started_ms: now_ms,
                    });
                    let parameters = effect.sound_parameters(0.0).unwrap_or_default();
                    sounds.push(SoundEvent {
                        archive: effect.sound.archive.clone(),
                        name: effect.sound.name.clone(),
                        position,
                        // The native opcode-2 command owns its frequency and
                        // range; weather intensity controls scheduling, not
                        // the thunder sample's source gain.
                        volume: 1.0,
                        min_distance: parameters.min_distance,
                        max_distance: parameters.max_distance,
                        frequency_ratio: parameters.frequency_ratio,
                        action: SoundAction::OneShot,
                    });
                    self.emit_visual(now_ms, weather.color, camera, primitives, point_lights);
                }
                self.last_time_ms = now_ms;
                self.phase = 0;
            }
            _ => self.phase = 1,
        }
    }

    fn emit_visual(
        &mut self,
        now_ms: u64,
        _color: [f32; 4],
        camera: Camera,
        primitives: &mut Vec<EnvironmentPrimitive>,
        point_lights: &mut Vec<PointLightFrame>,
    ) {
        let Some(visual) = self.visual.as_ref() else {
            return;
        };
        let duration_ms = seconds_to_millis(visual.effect.duration_seconds);
        let elapsed_ms = now_ms.saturating_sub(visual.started_ms);
        if elapsed_ms >= duration_ms.max(1) {
            self.visual = None;
            return;
        }
        // CLightning creates the Effect object 300 world units above the
        // sampled ground point. Its opcode 3 type-1 quad keeps a 600-unit
        // world-Z streak axis and billboards only its horizontal axis.
        let origin = [
            visual.position[0],
            visual.position[1],
            visual.position[2] + LIGHTNING_NATIVE_ORIGIN_OFFSET,
        ];
        let basis = diagonal_basis(lightning_parent_diagonal(visual.mirrored));
        let elapsed_seconds = elapsed_ms as f32 * 0.001;
        if let Some(point_light) =
            visual
                .effect
                .evaluate_point_light_with_basis(elapsed_seconds, origin, basis)
        {
            point_lights.push(point_light);
        }
        if let Some(quad) = visual.effect.evaluate_quad_with_mirror(
            elapsed_seconds,
            origin,
            camera,
            visual.mirrored,
        ) {
            primitives.push(EnvironmentPrimitive::Lightning(LightningBolt {
                material: visual.effect.visual.clone(),
                position: visual.position,
                quad,
                intensity: visual.intensity,
                age_seconds: elapsed_seconds,
                duration_seconds: visual.effect.duration_seconds,
            }));
        }
    }

    fn random_position(
        &mut self,
        camera: Camera,
        bounds: Option<LightningBounds>,
        rng: &mut NativeRng,
    ) -> [f32; 3] {
        let x_random = rng.next_unit();
        let y_random = rng.next_unit();
        let bounds = bounds.unwrap_or_else(|| {
            LightningBounds::new(
                camera.position[0],
                camera.position[0],
                camera.position[1],
                camera.position[1],
                camera.position[2],
            )
        });
        [
            bounds.x[0] + (bounds.x[1] - bounds.x[0]) * x_random,
            bounds.y[0] + (bounds.y[1] - bounds.y[0]) * y_random,
            bounds.z,
        ]
    }
}

fn from_atmosphere_weather(sample: &crate::atmosphere::WeatherSample) -> WeatherState {
    WeatherState::with_resources(
        sample.active,
        sample.intensity,
        sample.color,
        sample.resources.iter().map(|resource| resource.as_str()),
    )
}

fn split_resource(resource: &str) -> (String, String) {
    resource.split_once('/').map_or_else(
        || (String::new(), resource.to_owned()),
        |(archive, name)| (archive.to_owned(), name.to_owned()),
    )
}

fn seconds_to_millis(seconds: f32) -> u64 {
    if !seconds.is_finite() || seconds <= 0.0 {
        return 0;
    }
    let millis = f64::from(seconds) * 1_000.0;
    if millis >= u64::MAX as f64 {
        u64::MAX
    } else {
        millis.round() as u64
    }
}

fn read_resource(
    command_index: usize,
    opcode: FxOpcode,
    body: &[u8],
    archive_offset: usize,
) -> Result<EffectResource, EnvironmentError> {
    let archive = read_name(body, command_index, opcode, archive_offset)?;
    let name = read_name(body, command_index, opcode, archive_offset + 32)?;
    Ok(EffectResource { archive, name })
}

fn validate_opcode1_body(command_index: usize, body: &[u8]) -> Result<(), EnvironmentError> {
    let required = 132;
    body.get(..required)
        .ok_or(EnvironmentError::InvalidCommandLayout {
            command_index,
            opcode: FxOpcode::Op1,
            required,
            actual: body.len(),
        })?;
    for (index, offset) in (4..required).step_by(4).enumerate() {
        let value = read_f32(body, offset).expect("validated opcode 1 layout");
        if !value.is_finite() {
            return Err(EnvironmentError::NonFiniteVisualValue {
                command_index,
                index,
                value,
            });
        }
    }
    Ok(())
}

fn validate_opcode3_body(command_index: usize, body: &[u8]) -> Result<(), EnvironmentError> {
    let required = 132;
    body.get(..required)
        .ok_or(EnvironmentError::InvalidCommandLayout {
            command_index,
            opcode: FxOpcode::Op3,
            required,
            actual: body.len(),
        })?;
    for (index, offset) in (4..required).step_by(4).enumerate() {
        let value = read_f32(body, offset).expect("validated opcode 3 layout");
        if !value.is_finite() {
            return Err(EnvironmentError::NonFiniteVisualValue {
                command_index,
                index,
                value,
            });
        }
    }
    Ok(())
}

fn validate_opcode2_body(command_index: usize, body: &[u8]) -> Result<(), EnvironmentError> {
    let required = 144;
    body.get(..required)
        .ok_or(EnvironmentError::InvalidCommandLayout {
            command_index,
            opcode: FxOpcode::Op2,
            required,
            actual: body.len(),
        })?;
    for (index, offset) in (4..80).step_by(4).enumerate() {
        let value = read_f32(body, offset).expect("validated opcode 2 layout");
        if !value.is_finite() {
            return Err(EnvironmentError::NonFiniteSoundValue {
                command_index,
                index,
                value,
            });
        }
    }
    Ok(())
}

fn active_interval(body: &[u8], elapsed: f32, duration: f32) -> Option<(f32, f32)> {
    let start = read_f32(body, 4)?;
    let end = read_f32(body, 8)?;
    if !start.is_finite()
        || !end.is_finite()
        || !duration.is_finite()
        || duration <= 0.0
        || elapsed < start
        || elapsed > end
    {
        return None;
    }
    let span = end - start;
    let normalized = if span.abs() > f32::EPSILON {
        ((elapsed - start) / span).clamp(0.0, 1.0)
    } else {
        0.0
    };
    Some((normalized, end.min(duration)))
}

fn native_opcode1_interval(body: &[u8], command_time: f32) -> Option<f32> {
    let start = read_f32(body, 4)?;
    let end = read_f32(body, 8)?;
    if !command_time.is_finite()
        || !start.is_finite()
        || !end.is_finite()
        || command_time < start
        || command_time > end
    {
        return None;
    }
    let span = end - start;
    if !span.is_finite() {
        return None;
    }
    // Effect.dll divides by the interval span without an epsilon gate
    // (1000F703-1000F783). A zero-width interval produces 0/0, which cannot
    // yield a finite LightManager sample; keep that command inactive here.
    if span == 0.0 {
        return None;
    }
    let normalized = ((command_time - start) / span).clamp(0.0, 1.0);
    normalized.is_finite().then_some(normalized)
}

fn evaluate_position(body: &[u8], normalized_time: f32) -> Option<[f32; 3]> {
    let start = read_vec3(body, 36)?;
    let delta = read_vec3(body, 48)?;
    let exponent = read_vec3(body, 60)?;
    Some(std::array::from_fn(|axis| {
        let input = normalized_time.powf(exponent[axis]);
        start[axis] + delta[axis] * input
    }))
}

fn evaluate_scale(body: &[u8], normalized_time: f32) -> Option<[f32; 3]> {
    let start = read_vec3(body, 96)?;
    let end = read_vec3(body, 108)?;
    let exponent = read_vec3(body, 120)?;
    Some(std::array::from_fn(|axis| {
        let input = normalized_time.powf(exponent[axis]);
        lerp(start[axis], end[axis], input)
    }))
}

fn read_name(
    body: &[u8],
    command_index: usize,
    opcode: FxOpcode,
    offset: usize,
) -> Result<String, EnvironmentError> {
    let end = offset
        .checked_add(32)
        .ok_or(EnvironmentError::InvalidCommandLayout {
            command_index,
            opcode,
            required: usize::MAX,
            actual: body.len(),
        })?;
    let bytes = body
        .get(offset..end)
        .ok_or(EnvironmentError::InvalidCommandLayout {
            command_index,
            opcode,
            required: end,
            actual: body.len(),
        })?;
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    Ok(String::from_utf8_lossy(&bytes[..end]).into_owned())
}

fn volume_product(extent: [f32; 3]) -> f32 {
    extent[0] * extent[1] * extent[2]
}

fn finite_unit(vector: [f32; 3]) -> Option<[f32; 3]> {
    let length = (vector[0] * vector[0] + vector[1] * vector[1] + vector[2] * vector[2]).sqrt();
    if length.is_finite() && length > f32::EPSILON {
        Some(vector.map(|component| component / length))
    } else {
        None
    }
}

fn wrap_local(value: f32, extent: f32) -> f32 {
    if !value.is_finite() || !extent.is_finite() || extent <= 0.0 {
        return 0.0;
    }
    value - (value / extent).floor() * extent
}

fn add3(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] + right[0], left[1] + right[1], left[2] + right[2]]
}

fn scale3(value: [f32; 3], scale: f32) -> [f32; 3] {
    [value[0] * scale, value[1] * scale, value[2] * scale]
}

fn dot3(left: [f32; 3], right: [f32; 3]) -> f32 {
    left[0].mul_add(right[0], left[1].mul_add(right[1], left[2] * right[2]))
}

fn cross3(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [
        left[1].mul_add(right[2], -(left[2] * right[1])),
        left[2].mul_add(right[0], -(left[0] * right[2])),
        left[0].mul_add(right[1], -(left[1] * right[0])),
    ]
}

fn lightning_parent_diagonal(mirrored: bool) -> [f32; 3] {
    [
        LIGHTNING_QUAD_DIMENSIONS[0],
        if mirrored {
            -LIGHTNING_QUAD_DIMENSIONS[1]
        } else {
            LIGHTNING_QUAD_DIMENSIONS[1]
        },
        LIGHTNING_QUAD_DIMENSIONS[2],
    ]
}

fn normalized_or_fallback(vector: [f32; 3], fallback: [f32; 3]) -> [f32; 3] {
    finite_unit(vector).unwrap_or(fallback)
}

fn transform_diagonal(diagonal: [f32; 3], vector: [f32; 3]) -> [f32; 3] {
    [
        diagonal[0] * vector[0],
        diagonal[1] * vector[1],
        diagonal[2] * vector[2],
    ]
}

fn diagonal_basis(diagonal: [f32; 3]) -> [[f32; 3]; 3] {
    [
        [diagonal[0], 0.0, 0.0],
        [0.0, diagonal[1], 0.0],
        [0.0, 0.0, diagonal[2]],
    ]
}

fn identity_basis() -> [[f32; 3]; 3] {
    [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
}

fn read_u32(body: &[u8], offset: usize) -> Option<u32> {
    let bytes = body.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

fn read_f32(body: &[u8], offset: usize) -> Option<f32> {
    Some(f32::from_bits(read_u32(body, offset)?))
}

fn read_vec3(body: &[u8], offset: usize) -> Option<[f32; 3]> {
    Some([
        read_f32(body, offset)?,
        read_f32(body, offset.checked_add(4)?)?,
        read_f32(body, offset.checked_add(8)?)?,
    ])
}

fn read_vec4(body: &[u8], offset: usize) -> Option<[f32; 4]> {
    Some([
        read_f32(body, offset)?,
        read_f32(body, offset.checked_add(4)?)?,
        read_f32(body, offset.checked_add(8)?)?,
        read_f32(body, offset.checked_add(12)?)?,
    ])
}

fn lerp(start: f32, end: f32, t: f32) -> f32 {
    start + (end - start) * t
}

fn lerp3(start: [f32; 3], end: [f32; 3], t: f32) -> [f32; 3] {
    [
        lerp(start[0], end[0], t),
        lerp(start[1], end[1], t),
        lerp(start[2], end[2], t),
    ]
}

fn lerp4(start: [f32; 4], end: [f32; 4], t: f32) -> [f32; 4] {
    [
        lerp(start[0], end[0], t),
        lerp(start[1], end[1], t),
        lerp(start[2], end[2], t),
        lerp(start[3], end[3], t),
    ]
}

fn transform3(basis: [[f32; 3]; 3], local: [f32; 3]) -> [f32; 3] {
    [
        basis[0][0].mul_add(
            local[0],
            basis[1][0].mul_add(local[1], basis[2][0] * local[2]),
        ),
        basis[0][1].mul_add(
            local[0],
            basis[1][1].mul_add(local[1], basis[2][1] * local[2]),
        ),
        basis[0][2].mul_add(
            local[0],
            basis[1][2].mul_add(local[1], basis[2][2] * local[2]),
        ),
    ]
}

fn sanitize_float(value: f32) -> f32 {
    if value.is_finite() {
        value
    } else {
        0.0
    }
}

fn sub3(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

fn screen_point(camera: Camera, point: [f32; 3]) -> ([f32; 2], f32) {
    let delta = sub3(point, camera.position);
    let depth = dot3(camera.forward, delta);
    let depth_for_projection = depth.max(f32::EPSILON);
    let half_vertical = (camera.vertical_fov * 0.5).tan();
    let half_horizontal = half_vertical * camera.aspect_ratio;
    let ndc_x = dot3(camera.right, delta) / (depth_for_projection * half_horizontal);
    let ndc_y = dot3(camera.up, delta) / (depth_for_projection * half_vertical);
    let x = (ndc_x + 1.0) * 0.5 * camera.viewport[0];
    let y = (1.0 - ndc_y) * 0.5 * camera.viewport[1];
    ([x, y], depth)
}

fn pixel_to_ndc(camera: Camera, point: [f32; 2]) -> [f32; 2] {
    [
        point[0] * 2.0 / camera.viewport[0] - 1.0,
        1.0 - point[1] * 2.0 / camera.viewport[1],
    ]
}

fn screen_billboard(
    kind: PrecipitationKind,
    camera: Camera,
    head_world: [f32; 3],
    tail_world: [f32; 3],
    size: f32,
    index: usize,
) -> ScreenBillboard {
    let (head_pixels, depth) = screen_point(camera, head_world);
    let (tail_pixels, tail_depth) = screen_point(camera, tail_world);
    let near = PRECIPITATION_NEAR;
    let extent_x = PRECIPITATION_FAR - PRECIPITATION_NEAR;
    let depth_fade = (1.0 - (depth - near) / extent_x).max(0.0);
    let half_size = match kind {
        PrecipitationKind::Rain => size * 0.5 * depth_fade,
        PrecipitationKind::Snow => size * 0.5 * depth_fade.max(SNOW_MIN_DEPTH_FADE),
    };
    let (corners_pixels, uv) = match kind {
        PrecipitationKind::Rain => {
            let direction = [
                head_pixels[0] - tail_pixels[0],
                head_pixels[1] - tail_pixels[1],
            ];
            let direction_length =
                (direction[0] * direction[0] + direction[1] * direction[1]).sqrt();
            let perpendicular = if direction_length.is_finite() && direction_length > f32::EPSILON {
                [
                    -direction[1] / direction_length,
                    direction[0] / direction_length,
                ]
            } else {
                [1.0, 0.0]
            };
            let offset = [perpendicular[0] * half_size, perpendicular[1] * half_size];
            (
                [
                    [tail_pixels[0] - offset[0], tail_pixels[1] - offset[1]],
                    [tail_pixels[0] + offset[0], tail_pixels[1] + offset[1]],
                    [head_pixels[0] - offset[0], head_pixels[1] - offset[1]],
                    [head_pixels[0] + offset[0], head_pixels[1] + offset[1]],
                ],
                [[0.0, 1.0], [1.0, 1.0], [0.0, 0.0], [1.0, 0.0]],
            )
        }
        PrecipitationKind::Snow => {
            let signs = [[1.0, -1.0], [1.0, 1.0], [-1.0, -1.0], [-1.0, 1.0]];
            let mirror = index & 0x10 != 0;
            let corners = std::array::from_fn(|corner| {
                let [mut x, mut y] = signs[corner];
                if mirror {
                    std::mem::swap(&mut x, &mut y);
                }
                [
                    head_pixels[0] + x * half_size,
                    head_pixels[1] + y * half_size,
                ]
            });
            let uv = if mirror {
                [[1.0, 0.0], [1.0, 1.0], [0.0, 0.0], [0.0, 1.0]]
            } else {
                [[1.0, 0.0], [1.0, 1.0], [0.0, 0.0], [0.0, 1.0]]
            };
            (corners, uv)
        }
    };
    let head = pixel_to_ndc(camera, head_pixels);
    let tail = pixel_to_ndc(camera, tail_pixels);
    let corners = corners_pixels.map(|corner| pixel_to_ndc(camera, corner));
    ScreenBillboard {
        world_head: head_world,
        world_tail: tail_world,
        head,
        tail,
        corners,
        uv,
        half_size,
        head_depth: depth,
        tail_depth,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_rain_is_camera_local_and_uses_sky_material() {
        let mut system = EnvironmentSystem::new(7);
        let weather = WeatherInput {
            rain: WeatherState::with_resources(true, 0.5, [0.7, 0.8, 1.0, 0.6], ["atm_rain1.wav"]),
            ..WeatherInput::clear()
        };
        let frame = system.update(1.0, weather.clone(), Camera::new([10.0, -2.0, 4.0]));
        let particles = frame
            .primitives
            .iter()
            .filter_map(|primitive| match primitive {
                EnvironmentPrimitive::Particle {
                    kind,
                    position,
                    velocity,
                    material,
                    ..
                } => Some((*kind, *position, *velocity, material.as_str())),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(
            particles.len(),
            system.precipitation_volume().particle_count(0.5)
        );
        assert!(particles
            .iter()
            .all(|(kind, position, velocity, material)| {
                *kind == PrecipitationKind::Rain
                    && *material == system.materials.material_name(8).unwrap()
                    && *velocity == RAIN_VELOCITY
                    && system
                        .precipitation_volume()
                        .bounds(Camera::new([10.0, -2.0, 4.0]))
                        .contains(*position)
            }));
        assert_eq!(frame.sounds.len(), 1);
        assert_eq!(frame.sounds[0].name, "atm_rain1.wav");
        assert_eq!(frame.sounds[0].action, SoundAction::StartLoop);

        let update = system.update(0.1, weather, Camera::new([10.0, -2.0, 4.0]));
        assert_eq!(update.sounds.len(), 1);
        assert_eq!(update.sounds[0].action, SoundAction::SetLoopVolume);
        assert_eq!(update.sounds[0].volume, 0.5);
    }

    #[test]
    fn precipitation_uses_native_volume_ratio_and_nearest_even_count() {
        let reference = PrecipitationVolume::native();
        assert_eq!(reference.particle_count_for(reference, 1.0, 0.5004), 500);
        assert_eq!(reference.particle_count_for(reference, 1.0, 0.5016), 502);

        let narrow_camera = Camera::with_projection(
            [0.0; 3],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            0.4875,
            1.0,
        );
        let current = reference.for_camera(narrow_camera);
        assert!(reference.particle_count_for(current, 1.0, 1.0) < reference.particle_count(1.0));
    }

    #[test]
    fn precipitation_keeps_world_velocity_when_camera_turns_and_wraps() {
        let mut system = EnvironmentSystem::new(3);
        let camera = Camera::new([10.0, 20.0, 30.0]);
        let weather = WeatherInput {
            rain: WeatherState::new(true, 0.01, [1.0; 4]),
            ..WeatherInput::clear()
        };
        let initial = system.update(0.0, weather.clone(), camera);
        let initial_position = initial
            .primitives
            .iter()
            .find_map(|primitive| match primitive {
                EnvironmentPrimitive::Particle { position, .. } => Some(*position),
                _ => None,
            })
            .expect("initial rain particle");
        let turned_camera = Camera::with_basis(
            [10.0, 20.0, 30.0],
            [0.0, 1.0, 0.0],
            [-1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
        );
        let turned = system.update(0.0, weather.clone(), turned_camera);
        let turned_position = turned
            .primitives
            .iter()
            .find_map(|primitive| match primitive {
                EnvironmentPrimitive::Particle { position, .. } => Some(*position),
                _ => None,
            })
            .expect("turned rain particle");
        assert!(sub3(initial_position, turned_position)
            .into_iter()
            .all(|component| component.abs() < 1.0e-5));

        let frame = system.update(10.0, weather, camera);
        let particle = frame
            .primitives
            .iter()
            .find_map(|primitive| match primitive {
                EnvironmentPrimitive::Particle {
                    kind,
                    position,
                    velocity,
                    size,
                    screen,
                    ..
                } => Some((*kind, *position, *velocity, *size, *screen)),
                _ => None,
            })
            .expect("rain particle");
        assert_eq!(particle.0, PrecipitationKind::Rain);
        assert_eq!(particle.2, RAIN_VELOCITY);
        assert_eq!(
            particle.3,
            camera.precipitation_size_scalar() * RAIN_SIZE_FACTOR
        );
        assert!(system
            .precipitation_volume()
            .bounds(camera)
            .contains(particle.1));
        assert!(particle
            .4
            .corners
            .iter()
            .flatten()
            .all(|value| value.is_finite()));
    }

    #[test]
    fn precipitation_size_uses_viewport_width_and_horizontal_fov() {
        let camera = Camera::with_projection(
            [0.0; 3],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            0.975,
            16.0 / 9.0,
        )
        .with_viewport([1920.0, 1080.0]);
        let expected_fov = 2.0 * ((0.975_f32 * 0.5).tan() * (16.0_f32 / 9.0)).atan();
        assert!((camera.horizontal_fov() - expected_fov).abs() < 1.0e-6);
        assert!((camera.precipitation_size_scalar() - 1920.0 / expected_fov).abs() < 1.0e-4);
    }

    #[test]
    fn lightning_fixture_extracts_real_material_and_sound_offsets() {
        let mut bytes = header(3, 0.75);
        // The shipped env_lightning FXID uses opcode words 3, 1, and 2 with
        // bit 8 clear. Bit 8 is a native mode flag, not command enablement.
        let mut visual = command(0x0003, 200);
        visual[4..8].copy_from_slice(&1_u32.to_le_bytes());
        copy_name(&mut visual[4 + 132..4 + 164], b"material.lib");
        copy_name(&mut visual[4 + 164..4 + 196], b"env_lightning");
        bytes.extend_from_slice(&visual);
        let mut opcode1 = command(0x0001, 224);
        opcode1[4..8].copy_from_slice(&7_u32.to_le_bytes());
        opcode1[12..16].copy_from_slice(&0.99_f32.to_le_bytes());
        bytes.extend_from_slice(&opcode1);
        let mut sound = command(0x0002, 148);
        copy_name(&mut sound[4 + 80..4 + 112], b"sounds.lib");
        copy_name(&mut sound[4 + 112..4 + 144], b"atm_light1.wav");
        bytes.extend_from_slice(&sound);

        let effect = decode_env_lightning_fxid(Arc::from(bytes.into_boxed_slice()))
            .expect("env_lightning fixture");
        assert_eq!(effect.duration_seconds, 0.75);
        assert_eq!(
            effect.visual,
            EffectResource {
                archive: "material.lib".to_owned(),
                name: "env_lightning".to_owned()
            }
        );
        assert_eq!(
            effect.sound,
            EffectResource {
                archive: "sounds.lib".to_owned(),
                name: "atm_light1.wav".to_owned()
            }
        );
    }

    #[test]
    fn opcode2_sound_parameters_use_native_distance_and_frequency_offsets() {
        let mut bytes = header(3, 1.0);
        let mut visual = command(0x0003, 200);
        visual[4..8].copy_from_slice(&1_u32.to_le_bytes());
        copy_name(&mut visual[4 + 132..4 + 164], b"material.lib");
        copy_name(&mut visual[4 + 164..4 + 196], b"env_lightning");
        bytes.extend_from_slice(&visual);
        let mut opcode1 = command(0x0001, 224);
        opcode1[4..8].copy_from_slice(&7_u32.to_le_bytes());
        bytes.extend_from_slice(&opcode1);
        let mut sound = command(0x0002, 148);
        let body = &mut sound[4..];
        body[4..8].copy_from_slice(&0.0_f32.to_le_bytes());
        body[8..12].copy_from_slice(&1.0_f32.to_le_bytes());
        body[60..64].copy_from_slice(&100.0_f32.to_le_bytes());
        body[64..68].copy_from_slice(&1_500.0_f32.to_le_bytes());
        body[68..72].copy_from_slice(&0.75_f32.to_le_bytes());
        body[72..76].copy_from_slice(&1.25_f32.to_le_bytes());
        body[76..80].copy_from_slice(&0_u32.to_le_bytes());
        copy_name(&mut body[80..112], b"sounds.lib");
        copy_name(&mut body[112..144], b"atm_light1.wav");
        bytes.extend_from_slice(&sound);

        let effect = decode_env_lightning_fxid(Arc::from(bytes.into_boxed_slice()))
            .expect("env_lightning sound fixture");
        assert_eq!(
            effect.sound_parameters(0.5),
            Some(SoundParameters {
                min_distance: 100.0,
                max_distance: 1_500.0,
                frequency_ratio: 1.0,
            })
        );
    }

    #[test]
    fn opcode1_evaluator_emits_native_point_light_groups_and_maps_parent_axis() {
        let mut bytes = header(3, 0.75);
        let mut visual = command(0x0003, 200);
        visual[4..8].copy_from_slice(&1_u32.to_le_bytes());
        copy_name(&mut visual[4 + 132..4 + 164], b"material.lib");
        copy_name(&mut visual[4 + 164..4 + 196], b"env_lightning");
        visual[4 + 4..4 + 8].copy_from_slice(&(-1.0_f32).to_le_bytes());
        visual[4 + 8..4 + 12].copy_from_slice(&1.0_f32.to_le_bytes());
        for offset in [96_usize, 100, 104, 108, 112, 116, 120, 124, 128] {
            visual[4 + offset..4 + offset + 4].copy_from_slice(&1.0_f32.to_le_bytes());
        }
        bytes.extend_from_slice(&visual);
        let mut opcode1 = command(0x0001, 224);
        let body = &mut opcode1[4..];
        body[0..4].copy_from_slice(&7_u32.to_le_bytes());
        body[4..8].copy_from_slice(&0.0_f32.to_le_bytes());
        body[8..12].copy_from_slice(&1.0_f32.to_le_bytes());
        body[12..24].copy_from_slice(
            &[0.0_f32; 3]
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<_>>(),
        );
        body[24..36].copy_from_slice(
            &[2.0_f32, 4.0, 6.0]
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<_>>(),
        );
        body[36..48].copy_from_slice(
            &[1.0_f32, 0.0, 0.0]
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<_>>(),
        );
        body[48..60].copy_from_slice(
            &[0.0_f32, 1.0, 0.0]
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<_>>(),
        );
        body[60..76].copy_from_slice(
            &[7.0_f32, 7.0, 10.0, 0.0]
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<_>>(),
        );
        body[76..92].copy_from_slice(
            &[0.0_f32, 0.0, 0.0, 0.0]
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<_>>(),
        );
        body[108..112].copy_from_slice(&100.0_f32.to_le_bytes());
        body[112..116].copy_from_slice(&0.0_f32.to_le_bytes());
        body[116..120].copy_from_slice(&0.25_f32.to_le_bytes());
        body[120..124].copy_from_slice(&0.25_f32.to_le_bytes());
        body[124..128].copy_from_slice(&0.75_f32.to_le_bytes());
        body[128..132].copy_from_slice(&0.75_f32.to_le_bytes());
        bytes.extend_from_slice(&opcode1);
        let mut sound = command(0x0002, 148);
        copy_name(&mut sound[4 + 80..4 + 112], b"sounds.lib");
        copy_name(&mut sound[4 + 112..4 + 144], b"atm_light1.wav");
        bytes.extend_from_slice(&sound);

        let effect = decode_env_lightning_fxid(Arc::from(bytes.into_boxed_slice()))
            .expect("env_lightning evaluator fixture");
        let basis = [[0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let frame = effect
            .evaluate_point_light_with_basis(0.5, [10.0, 20.0, 30.0], basis)
            .expect("active opcode 1 sample");
        assert_eq!(frame.effect_type, 7);
        assert_eq!(frame.position, [12.0, 23.0, 31.0]);
        let diagonal = 2.0_f32.sqrt().recip();
        assert!((frame.direction[0] - diagonal).abs() < 1.0e-6);
        assert_eq!(frame.direction[1], 0.0);
        assert!((frame.direction[2] - diagonal).abs() < 1.0e-6);
        assert_eq!(frame.color, [3.5, 3.5, 5.0, 0.0]);
        assert_eq!(frame.range, 50.0);
        assert_eq!(frame.attenuation, [0.25, 0.75, 0.75]);
        assert_eq!(frame.effect_factor, 0.25);
        assert!(frame.active);
        let quad = effect
            .evaluate_quad(0.5, [10.0, 20.0, 330.0], Camera::new([0.0, 0.0, 0.0]))
            .expect("active opcode 3 sample");
        assert_eq!(quad.dimensions, LIGHTNING_QUAD_DIMENSIONS);
        assert_eq!(
            quad.uv,
            [[0.0, 0.99], [0.0, 0.0], [0.99, 0.0], [0.99, 0.99]]
        );
        assert_eq!(quad.opacity, 0.25);
        assert!(quad
            .vertices
            .iter()
            .flatten()
            .all(|value| value.is_finite()));
        assert!(effect.evaluate_point_light(1.01, [0.0; 3]).is_none());
    }

    #[test]
    fn generic_opcode1_uses_native_command_time_not_header_duration_phase() {
        // These type/start/end values come from the shipped f_blinklight_r
        // (header mode 2, duration 2s) and b_sphere_main (mode 0, duration
        // 6s) Op1 records. Their local command clocks must reach their own
        // authored windows; dividing by the FX lifetime would miss both.
        let blinklight = op1_test_command(6, 0.75, 1.0);
        let sphere = op1_test_command(5, 1.0 / 3.0, 1.0);

        let blink_midpoint = evaluate_op1_point_light_identity(&blinklight, 0.875, [0.0; 3])
            .expect("blink light command midpoint");
        let sphere_midpoint = evaluate_op1_point_light_identity(&sphere, 2.0 / 3.0, [0.0; 3])
            .expect("sphere command midpoint");
        assert_eq!(blink_midpoint.effect_type, 6);
        assert_eq!(blink_midpoint.light_selector, 1);
        assert_eq!(blink_midpoint.native_flags, 0xA000_0000);
        assert_eq!(blink_midpoint.command_time, 0.875);
        assert!((blink_midpoint.normalized_time - 0.5).abs() < 1.0e-6);
        assert_eq!(sphere_midpoint.effect_type, 5);
        assert_eq!(sphere_midpoint.light_selector, 1);
        assert_eq!(sphere_midpoint.native_flags, 0);
        assert!((sphere_midpoint.normalized_time - 0.5).abs() < 1.0e-6);

        // The native comparisons accept the endpoint; the effect remains
        // active at `end`, where its interpolation reaches one.
        let blink_end = evaluate_op1_point_light_identity(&blinklight, 1.0, [0.0; 3])
            .expect("inclusive end of native interval");
        assert_eq!(blink_end.normalized_time, 1.0);
        assert!(
            evaluate_op1_point_light_identity(&blinklight, 1.0 + f32::EPSILON, [0.0; 3]).is_none()
        );
        assert!(
            evaluate_op1_point_light_identity(&op1_test_command(3, 0.0, 1.0), 0.5, [0.0; 3])
                .is_none()
        );
        assert!(
            evaluate_op1_point_light_identity(&op1_test_command(4, 0.0, 1.0), 0.5, [0.0; 3])
                .is_none()
        );
    }

    #[test]
    fn generic_opcode1_preserves_narrow_native_intervals_and_rejects_zero_span() {
        let narrow_end = f32::EPSILON * 0.5;
        let narrow = op1_test_command(5, 0.0, narrow_end);
        let sample = evaluate_op1_point_light_identity(&narrow, narrow_end * 0.5, [0.0; 3])
            .expect("non-zero native interval below epsilon");
        assert!((sample.normalized_time - 0.5).abs() < 1.0e-6);

        let zero_width = op1_test_command(5, 0.0, 0.0);
        assert!(evaluate_op1_point_light_identity(&zero_width, 0.0, [0.0; 3]).is_none());
    }

    #[test]
    fn generic_opcode1_preserves_negative_native_range() {
        let mut command = op1_test_command(5, 0.0, 1.0);
        let mut body = command.raw_body.to_vec();
        body[108..112].copy_from_slice(&(-2.0_f32).to_le_bytes());
        body[112..116].copy_from_slice(&(-4.0_f32).to_le_bytes());
        command.raw_body = Arc::from(body.into_boxed_slice());

        let sample = evaluate_op1_point_light_identity(&command, 0.5, [0.0; 3])
            .expect("native Op1 sample with a negative range");
        assert_eq!(sample.range, -3.0);
    }

    #[test]
    fn generic_opcode1_keeps_zero_direction_zero() {
        let mut command = op1_test_command(5, 0.0, 1.0);
        let mut body = command.raw_body.to_vec();
        body[36..60].fill(0);
        command.raw_body = Arc::from(body.into_boxed_slice());

        let sample = evaluate_op1_point_light_identity(&command, 0.5, [0.0; 3])
            .expect("native Op1 sample with a zero direction");
        assert_eq!(sample.direction, [0.0; 3]);
    }

    #[test]
    fn generic_opcode1_validates_only_values_consumed_by_the_handler() {
        let mut command = op1_test_command(7, 0.0, 1.0);
        let mut body = command.raw_body.to_vec();
        body[92..96].copy_from_slice(&f32::NAN.to_le_bytes());
        command.raw_body = Arc::from(body.into_boxed_slice());
        assert!(evaluate_op1_point_light_identity(&command, 0.5, [0.0; 3]).is_some());

        let mut body = command.raw_body.to_vec();
        body[80..84].copy_from_slice(&f32::NAN.to_le_bytes());
        command.raw_body = Arc::from(body.into_boxed_slice());
        assert!(evaluate_op1_point_light_identity(&command, 0.5, [0.0; 3]).is_none());
    }

    #[test]
    fn generic_opcode1_rejects_non_finite_transformed_values() {
        let command = op1_test_command(5, 0.0, 1.0);

        assert!(evaluate_op1_point_light_with_basis(
            &command,
            0.5,
            [f32::MAX, 0.0, 0.0],
            [[f32::MAX, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        )
        .is_none());
    }

    fn op1_test_command(effect_type: u32, start: f32, end: f32) -> FxCommand {
        let mut body = vec![0; 132];
        body[0..4].copy_from_slice(&effect_type.to_le_bytes());
        body[4..8].copy_from_slice(&start.to_le_bytes());
        body[8..12].copy_from_slice(&end.to_le_bytes());
        body[12..24].copy_from_slice(
            &[0.0_f32; 3]
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect::<Vec<_>>(),
        );
        body[24..36].copy_from_slice(
            &[2.0_f32, 4.0, 6.0]
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect::<Vec<_>>(),
        );
        body[36..48].copy_from_slice(
            &[1.0_f32, 0.0, 0.0]
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect::<Vec<_>>(),
        );
        body[48..60].copy_from_slice(
            &[0.0_f32, 1.0, 0.0]
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect::<Vec<_>>(),
        );
        body[60..76].copy_from_slice(
            &[1.0_f32, 1.0, 1.0, 1.0]
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect::<Vec<_>>(),
        );
        body[76..92].copy_from_slice(
            &[3.0_f32, 3.0, 3.0, 1.0]
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect::<Vec<_>>(),
        );
        body[108..112].copy_from_slice(&10.0_f32.to_le_bytes());
        body[112..116].copy_from_slice(&20.0_f32.to_le_bytes());
        body[116..120].copy_from_slice(&0.5_f32.to_le_bytes());
        FxCommand {
            word: 1,
            opcode: FxOpcode::Op1,
            native_flag: false,
            raw_body: Arc::from(body.into_boxed_slice()),
            resource_refs: Vec::new(),
        }
    }

    fn evaluate_op1_point_light_identity(
        command: &FxCommand,
        command_time: f32,
        origin: [f32; 3],
    ) -> Option<PointLightFrame> {
        evaluate_op1_point_light_with_basis(command, command_time, origin, identity_basis())
    }

    #[test]
    fn opcode3_quad_applies_parent_diagonal_before_offset_and_scale() {
        let mut body = vec![0; 196];
        body[0..4].copy_from_slice(&1_u32.to_le_bytes());
        body[4..8].copy_from_slice(&0.0_f32.to_le_bytes());
        body[8..12].copy_from_slice(&1.0_f32.to_le_bytes());
        body[36..48].copy_from_slice(
            &[1.0_f32, 2.0, 0.0]
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect::<Vec<_>>(),
        );
        body[48..60].copy_from_slice(
            &[1.0_f32, 0.0, 0.0]
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect::<Vec<_>>(),
        );
        body[60..72].copy_from_slice(
            &[1.0_f32; 3]
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect::<Vec<_>>(),
        );
        body[96..108].copy_from_slice(
            &[2.0_f32, 3.0, 1.0]
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect::<Vec<_>>(),
        );
        body[108..120].copy_from_slice(
            &[4.0_f32, 5.0, 2.0]
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect::<Vec<_>>(),
        );
        body[120..132].copy_from_slice(
            &[1.0_f32; 3]
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect::<Vec<_>>(),
        );
        let effect = LightningEffect {
            duration_seconds: 1.0,
            visual: EffectResource {
                archive: "material.lib".to_owned(),
                name: "env_lightning".to_owned(),
            },
            visual_body: Arc::from(body.into_boxed_slice()),
            opcode1_body: Arc::from(vec![0; 132].into_boxed_slice()),
            opcode1_command_index: 1,
            sound: EffectResource {
                archive: "sounds.lib".to_owned(),
                name: "atm_light1.wav".to_owned(),
            },
            sound_body: Arc::from(vec![0; 144].into_boxed_slice()),
            visual_command_index: 0,
            sound_command_index: 2,
        };
        let quad = effect
            .evaluate_quad(0.5, [10.0, 20.0, 30.0], Camera::new([130.0, 80.0, 100.0]))
            .expect("synthetic opcode 3 sample");
        assert_eq!(quad.local_offset, [1.5, 2.0, 0.0]);
        assert_eq!(quad.local_scale, [3.0, 4.0, 1.5]);
        assert_eq!(quad.dimensions, [120.0, 160.0, 900.0]);
        let center = quad
            .vertices
            .iter()
            .fold([0.0; 3], |sum, vertex| add3(sum, *vertex))
            .map(|component| component * 0.25);
        assert_eq!(center, [70.0, 100.0, 30.0]);
        assert!((quad.vertices[2][2] - quad.vertices[0][2] - 900.0).abs() < 1.0e-5);

        let parallel_camera = Camera::new([70.0, 100.0, 630.0]);
        assert!(effect
            .evaluate_quad(0.5, [10.0, 20.0, 30.0], parallel_camera)
            .is_none());
        let fallback_camera = Camera::new([70.0, 100.0, 1_230.0]);
        let fallback = effect
            .evaluate_quad(0.5, [10.0, 20.0, 30.0], fallback_camera)
            .expect("non-unit parallel view uses native fallback axis");
        assert_eq!(fallback.vertices[0][1], fallback.vertices[1][1]);
        assert!((fallback.vertices[0][0] - fallback.vertices[1][0] - 120.0).abs() < 1.0e-5);
    }

    #[test]
    fn lightning_uses_native_delay_cooldown_and_fx_lifetime() {
        let effect = LightningEffect {
            duration_seconds: 0.75,
            visual: EffectResource {
                archive: "material.lib".to_owned(),
                name: "env_lightning".to_owned(),
            },
            visual_body: {
                let mut body = vec![0; 196];
                body[0..4].copy_from_slice(&1_u32.to_le_bytes());
                body[4..8].copy_from_slice(&(-1.0_f32).to_le_bytes());
                body[8..12].copy_from_slice(&1.0_f32.to_le_bytes());
                for offset in [96_usize, 100, 104, 108, 112, 116, 120, 124, 128] {
                    body[offset..offset + 4].copy_from_slice(&1.0_f32.to_le_bytes());
                }
                Arc::from(body.into_boxed_slice())
            },
            opcode1_body: {
                let mut body = vec![0; 220];
                body[8..12].copy_from_slice(&0.75_f32.to_le_bytes());
                body[36..40].copy_from_slice(&1.0_f32.to_le_bytes());
                body[128..132].copy_from_slice(&1.0_f32.to_le_bytes());
                Arc::from(body.into_boxed_slice())
            },
            opcode1_command_index: 1,
            sound: EffectResource {
                archive: "sounds.lib".to_owned(),
                name: "atm_light1.wav".to_owned(),
            },
            sound_body: Arc::from(vec![0; 144].into_boxed_slice()),
            visual_command_index: 0,
            sound_command_index: 2,
        };
        let mut system = EnvironmentSystem::new(11);
        system.set_lightning_effect(effect);
        let weather = WeatherInput {
            lightning: WeatherState::new(true, 1.0, [1.0; 4]),
            ..WeatherInput::clear()
        };
        let camera = Camera::new([50.0, 0.0, 0.0]);
        system.set_lightning_bounds(LightningBounds::new(0.0, 0.0, 0.0, 0.0, 0.0));
        // The constructor starts in native state 1, which schedules a random
        // delay. No external trigger sequence is needed.
        let first = system.update(0.1, weather.clone(), camera);
        assert!(first.sounds.is_empty());
        assert_eq!(system.lightning.phase, 2);

        let mut one_shots = 0;
        let mut effect_frames = 0;
        let mut point_light_frames = 0;
        let mut inspected_shape = false;
        for _ in 0..200 {
            let frame = system.update(0.1, weather.clone(), camera);
            point_light_frames += frame.point_lights.len();
            one_shots += frame
                .sounds
                .iter()
                .filter(|sound| sound.action == SoundAction::OneShot)
                .count();
            for primitive in &frame.primitives {
                if let EnvironmentPrimitive::Lightning(bolt) = primitive {
                    inspected_shape = true;
                    assert_eq!(bolt.quad.dimensions, LIGHTNING_QUAD_DIMENSIONS);
                    assert!(bolt
                        .quad
                        .vertices
                        .iter()
                        .flatten()
                        .all(|value| value.is_finite()));
                    assert_eq!(
                        bolt.quad.uv,
                        [[0.0, 0.99], [0.0, 0.0], [0.99, 0.0], [0.99, 0.99]]
                    );
                    assert!(bolt.age_seconds >= 0.0 && bolt.age_seconds < bolt.duration_seconds);
                    assert_eq!(bolt.duration_seconds, 0.75);
                    effect_frames += 1;
                }
            }
        }
        assert!(one_shots >= 2, "native timer must repeat lightning");
        assert!(effect_frames > 0);
        assert!(inspected_shape);
        assert!(point_light_frames > 0);

        let clear = system.update(0.1, WeatherInput::clear(), camera);
        assert!(clear.primitives.is_empty());
        assert!(clear.point_lights.is_empty());
        assert!(clear.sounds.is_empty());
    }

    #[test]
    fn fractional_milliseconds_do_not_drift_at_120_fps() {
        let mut system = EnvironmentSystem::new(1);
        for _ in 0..120 {
            let _ = system.update(1.0 / 120.0, WeatherInput::clear(), Camera::default());
        }
        assert_eq!(system.time_millis(), 1_000);
    }

    fn header(command_count: u32, duration: f32) -> Vec<u8> {
        let mut out = Vec::with_capacity(60);
        out.extend_from_slice(&command_count.to_le_bytes());
        out.extend_from_slice(&1_u32.to_le_bytes());
        out.extend_from_slice(&duration.to_bits().to_le_bytes());
        out.extend_from_slice(&0.0_f32.to_bits().to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        for _ in 0..9 {
            out.extend_from_slice(&0.0_f32.to_bits().to_le_bytes());
        }
        out
    }

    fn command(word: u32, size: usize) -> Vec<u8> {
        let mut out = vec![0; size];
        out[..4].copy_from_slice(&word.to_le_bytes());
        out
    }

    fn copy_name(dst: &mut [u8], name: &[u8]) {
        dst[..name.len()].copy_from_slice(name);
    }
}
