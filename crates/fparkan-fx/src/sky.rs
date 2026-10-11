//! CPU-side sky dome, sky materials, fog, and sun output.
//!
//! The original `CSky` object is a finite dome rather than an equirectangular
//! skybox.  This module keeps that geometry and the interpolated
//! `sky.ske`/`sky.wea` data independent of any graphics backend.  A renderer
//! can upload [`SkyMesh`] once, update its vertex colors, and resolve the
//! material names in [`SkyMaterials`] through its normal resource path.

use crate::atmosphere::{AtmosphereFrame, PackedColor, SkySample, SunBirth, SunSample};
use std::fmt;

/// Number of rows used by the original environment `sky.wea` table.
pub const SKY_MATERIAL_COUNT: usize = 9;

/// Texture-coordinate scales used by the three original sky vertex stages.
///
/// The values are the constants at PE `65E94`, `65E98`, and `65E9C` in the
/// original renderer.  They are applied to `position.x / radius` and
/// `position.y / radius`.
pub const SKY_UV_SCALES: [f32; 3] = [1.0, 15.0, 3.0];

/// Material names observed in the AutoDemo `sky.wea` table.
///
/// Missions are allowed to replace these names.  Code should use the rows
/// returned by [`SkyMaterials::parse`] when resolving a real mission.
pub const OBSERVED_SKY_MATERIAL_NAMES: [&str; SKY_MATERIAL_COUNT] = [
    "ENV_NEBULA_0",
    "ENV_STARS",
    "ENV_CLOUDS",
    "ENV_SUN_3",
    "ENV_MOON",
    "ENV_FLARE_00",
    "ENV_FLARE_01",
    "SNOWFLAKE",
    "RAIN_DROP",
];

const TAU: f32 = std::f32::consts::TAU;
// These are the values written by Terrain's atmosphere-manager constructor:
// 0x6581C = 10000, and 0x6580C = 3.1415901184 is halved twice before the
// CSky constructor receives it.
const NATIVE_PI: f32 = 3.1415901184;
const DEFAULT_HEIGHT: f32 = 10_000.0;
const DEFAULT_THETA_MAX: f32 = NATIVE_PI * 0.25;
const DEFAULT_Z_SCALE: f32 = 1.0;
const DEFAULT_AZIMUTH_COUNT: usize = 16;
const DEFAULT_RINGS: usize = 5;
const FOG_DISTANCE_SCALE: f32 = 700.0;
const MAX_GEOMETRY_VERTICES: usize = 1_000_000;
// CSky::Render (481C5) uses these data constants when it converts the camera
// yaw to a four-color, ninety-degree horizon sector.
const NATIVE_GRADIENT_SEGMENT_DEGREES: i64 = 90;
const NATIVE_GRADIENT_FRACTION_SCALE: f32 = f32::from_bits(0x3C36_0B61);
const NATIVE_GRADIENT_ZERO: f32 = 0.0;
const NATIVE_GRADIENT_ONE: f32 = 1.0;
const NATIVE_GRADIENT_THREE: f32 = 3.0;
const NATIVE_GRADIENT_SHORT_SCALE: f32 = 0.8;
const NATIVE_GRADIENT_LONG_SCALE: f32 = 0.1;
const NATIVE_GRADIENT_LONG_OFFSET: f32 = 0.7;

/// Configuration passed to the original `CSky` dome constructor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SkyGeometryConfig {
    /// Dome height before the z scale is applied.
    pub height: f32,
    /// Maximum polar angle in radians.
    pub theta_max: f32,
    /// Vertical scale applied to the generated positions.
    pub z_scale: f32,
    /// Number of azimuth sectors around the dome.
    pub azimuth_count: usize,
    /// Number of vertices in each azimuth sector after the zenith vertex.
    pub rings: usize,
}

impl SkyGeometryConfig {
    /// Creates a geometry configuration.
    #[must_use]
    pub const fn new(
        height: f32,
        theta_max: f32,
        z_scale: f32,
        azimuth_count: usize,
        rings: usize,
    ) -> Self {
        Self {
            height,
            theta_max,
            z_scale,
            azimuth_count,
            rings,
        }
    }

    /// Returns the radius used by the original constructor.
    #[must_use]
    pub fn radius(self) -> f32 {
        let half_theta = self.theta_max * 0.5;
        self.height * 0.5 / half_theta.sin().powi(2)
    }

    /// Builds a colored sky mesh and validates all counts and floating point
    /// inputs.
    pub fn mesh(self, sample: &SkySample) -> Result<SkyMesh, SkyGeometryError> {
        self.try_mesh(sample)
    }

    /// Builds a colored sky mesh and validates all counts and floating point
    /// inputs.
    pub fn try_mesh(self, sample: &SkySample) -> Result<SkyMesh, SkyGeometryError> {
        let mut mesh = self.try_topology()?;
        mesh.update_colors(sample);
        Ok(mesh)
    }

    /// Builds the immutable dome topology.
    ///
    /// Vertex colors are initialized to the packed zero value and are filled
    /// by [`SkyMesh::update_colors`] when the first atmosphere sample arrives.
    /// Keeping topology construction separate lets a renderer upload the
    /// position/normal/UV/index buffers once.
    pub fn try_topology(self) -> Result<SkyMesh, SkyGeometryError> {
        validate_geometry(self)?;
        let vertex_count = self
            .azimuth_count
            .checked_mul(self.rings)
            .and_then(|count| count.checked_add(1))
            .ok_or(SkyGeometryError::CountOverflow)?;
        if vertex_count > MAX_GEOMETRY_VERTICES {
            return Err(SkyGeometryError::TooManyVertices { vertex_count });
        }
        let triangle_count = self
            .azimuth_count
            .checked_mul(
                self.rings
                    .checked_mul(2)
                    .and_then(|count| count.checked_sub(1))
                    .ok_or(SkyGeometryError::CountOverflow)?,
            )
            .ok_or(SkyGeometryError::CountOverflow)?;
        let index_count = triangle_count
            .checked_mul(3)
            .ok_or(SkyGeometryError::CountOverflow)?;
        let radius = self.radius();
        if !radius.is_finite() || radius <= 0.0 {
            return Err(SkyGeometryError::NonFiniteRadius);
        }

        let mut vertices = Vec::with_capacity(vertex_count);
        vertices.push(SkyVertex {
            position: [0.0, 0.0, self.height * self.z_scale],
            normal: [0, 0, 127],
            uv: [[0.0, 0.0]; 3],
            color: PackedColor::default(),
        });

        for azimuth in 0..self.azimuth_count {
            let phi = azimuth as f32 / self.azimuth_count as f32 * TAU;
            let sin_phi = phi.sin();
            let cos_phi = phi.cos();
            for ring in 0..self.rings {
                let theta = (ring + 1) as f32 / self.rings as f32 * self.theta_max;
                let sin_theta = theta.sin();
                let cos_theta = theta.cos();
                let position = [
                    radius * sin_theta * sin_phi,
                    radius * sin_theta * cos_phi,
                    (radius * cos_theta + self.height - radius) * self.z_scale,
                ];
                let normal = [
                    normal_byte(sin_theta * sin_phi),
                    normal_byte(sin_theta * cos_phi),
                    normal_byte(cos_theta),
                ];
                let uv = std::array::from_fn(|stage| {
                    [
                        position[0] / radius * SKY_UV_SCALES[stage],
                        position[1] / radius * SKY_UV_SCALES[stage],
                    ]
                });
                vertices.push(SkyVertex {
                    position,
                    normal,
                    uv,
                    color: PackedColor::default(),
                });
            }
        }

        let mut indices = Vec::with_capacity(index_count);
        for azimuth in 0..self.azimuth_count {
            let base = 1 + azimuth * self.rings;
            let next = 1 + ((azimuth + 1) % self.azimuth_count) * self.rings;
            indices.push(0);
            indices.extend_from_slice(&[
                u32::try_from(base).map_err(|_| SkyGeometryError::IndexOverflow)?,
                u32::try_from(next).map_err(|_| SkyGeometryError::IndexOverflow)?,
            ]);
            for ring in 0..self.rings - 1 {
                let a = base + ring;
                let b = base + ring + 1;
                let c = next + ring;
                let d = next + ring + 1;
                indices.extend_from_slice(&[
                    u32::try_from(a).map_err(|_| SkyGeometryError::IndexOverflow)?,
                    u32::try_from(b).map_err(|_| SkyGeometryError::IndexOverflow)?,
                    u32::try_from(c).map_err(|_| SkyGeometryError::IndexOverflow)?,
                    u32::try_from(c).map_err(|_| SkyGeometryError::IndexOverflow)?,
                    u32::try_from(b).map_err(|_| SkyGeometryError::IndexOverflow)?,
                    u32::try_from(d).map_err(|_| SkyGeometryError::IndexOverflow)?,
                ]);
            }
        }

        Ok(SkyMesh {
            config: self,
            radius,
            vertices,
            indices,
        })
    }
}

impl Default for SkyGeometryConfig {
    fn default() -> Self {
        Self::new(
            DEFAULT_HEIGHT,
            DEFAULT_THETA_MAX,
            DEFAULT_Z_SCALE,
            DEFAULT_AZIMUTH_COUNT,
            DEFAULT_RINGS,
        )
    }
}

fn validate_geometry(config: SkyGeometryConfig) -> Result<(), SkyGeometryError> {
    if !config.height.is_finite() || config.height <= 0.0 {
        return Err(SkyGeometryError::InvalidHeight(config.height));
    }
    if !config.theta_max.is_finite() || config.theta_max <= 0.0 || config.theta_max >= TAU {
        return Err(SkyGeometryError::InvalidTheta(config.theta_max));
    }
    if !config.z_scale.is_finite() {
        return Err(SkyGeometryError::InvalidZScale(config.z_scale));
    }
    if config.azimuth_count == 0 || config.rings == 0 {
        return Err(SkyGeometryError::ZeroCount);
    }
    Ok(())
}

fn normal_byte(component: f32) -> i8 {
    // Terrain's native sky constructor writes signed normal components with
    // x87 nearest-even rounding and a 127 scale.  Keep the signed type here;
    // a renderer that uploads the legacy byte stream can cast it to u8.
    (component * 127.0).round_ties_even().clamp(-128.0, 127.0) as i8
}

fn color_for_vertex(
    sample: &SkySample,
    ring: usize,
    azimuth: usize,
    azimuth_count: usize,
) -> PackedColor {
    match ring {
        0 => sample.colors[12],
        1 => belt_color(sample, 8, azimuth, azimuth_count),
        2 => belt_color(sample, 4, azimuth, azimuth_count),
        _ => belt_color(sample, 0, azimuth, azimuth_count),
    }
}

fn belt_color(
    sample: &SkySample,
    offset: usize,
    azimuth: usize,
    azimuth_count: usize,
) -> PackedColor {
    let group_width = azimuth_count.saturating_add(3) / 4;
    if group_width == 0 {
        return sample.colors[offset];
    }
    let anchor = (azimuth / group_width).min(3);
    let next = (anchor + 1) & 3;
    let factor = (azimuth % group_width) as f32 / group_width as f32;
    sample.colors[offset + anchor].lerp(sample.colors[offset + next], factor)
}

/// One CPU vertex in the original sky buffer layout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SkyVertex {
    /// World-space position.
    pub position: [f32; 3],
    /// Signed 8-bit normal components written by the original constructor.
    pub normal: [i8; 3],
    /// Three texture-coordinate stages, in original stage order.
    pub uv: [[f32; 2]; 3],
    /// Current ARGB vertex color.
    pub color: PackedColor,
}

impl SkyVertex {
    /// Returns the packed color as normalized RGBA.
    #[must_use]
    pub fn color_rgba(self) -> [f32; 4] {
        self.color.rgba()
    }

    /// Decodes the native signed normal bytes to normalized components.
    #[must_use]
    pub fn normal_vector(self) -> [f32; 3] {
        self.normal.map(|component| f32::from(component) / 127.0)
    }
}

/// Generated sky dome geometry and indices.
#[derive(Clone, Debug, PartialEq)]
pub struct SkyMesh {
    /// Configuration used to generate this mesh.
    pub config: SkyGeometryConfig,
    /// Radius from the original constructor formula.
    pub radius: f32,
    /// Zenith followed by azimuth-major ring vertices.
    pub vertices: Vec<SkyVertex>,
    /// Triangle-list indices: one fan triangle and `rings - 1` quads per
    /// azimuth sector.
    pub indices: Vec<u32>,
}

impl SkyMesh {
    /// Number of triangles in the mesh.
    #[must_use]
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// Updates only the dynamic vertex colors for an atmosphere sample.
    ///
    /// The vertex order is fixed by [`SkyGeometryConfig::try_topology`], so
    /// this operation never reallocates the topology or index buffer.  The
    /// renderer can upload the changed color fields in place.
    pub fn update_colors(&mut self, sample: &SkySample) {
        let Some(zenith) = self.vertices.first_mut() else {
            return;
        };
        zenith.color = sample.colors[12];
        for azimuth in 0..self.config.azimuth_count {
            let base = 1 + azimuth * self.config.rings;
            for ring in 0..self.config.rings {
                let Some(vertex) = self.vertices.get_mut(base + ring) else {
                    return;
                };
                vertex.color = color_for_vertex(sample, ring, azimuth, self.config.azimuth_count);
            }
        }
    }
}

/// Errors found while constructing a sky dome.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SkyGeometryError {
    /// Height must be finite and positive.
    InvalidHeight(f32),
    /// Polar angle must be finite and in `(0, TAU)`.
    InvalidTheta(f32),
    /// Vertical scale must be finite.
    InvalidZScale(f32),
    /// A sector or ring count was zero.
    ZeroCount,
    /// A count calculation overflowed.
    CountOverflow,
    /// Mesh allocation would exceed the bounded CPU contract.
    TooManyVertices { vertex_count: usize },
    /// The radius formula did not produce a finite positive value.
    NonFiniteRadius,
    /// An index could not fit in the renderer-facing u32 index type.
    IndexOverflow,
}

impl fmt::Display for SkyGeometryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidHeight(value) => {
                write!(f, "sky height must be finite and positive: {value}")
            }
            Self::InvalidTheta(value) => write!(f, "sky theta_max must be in (0, TAU): {value}"),
            Self::InvalidZScale(value) => write!(f, "sky z_scale must be finite: {value}"),
            Self::ZeroCount => write!(f, "sky azimuth and ring counts must be non-zero"),
            Self::CountOverflow => write!(f, "sky geometry count overflow"),
            Self::TooManyVertices { vertex_count } => {
                write!(f, "sky geometry has too many vertices: {vertex_count}")
            }
            Self::NonFiniteRadius => write!(f, "sky geometry radius is not finite and positive"),
            Self::IndexOverflow => write!(f, "sky geometry index does not fit in u32"),
        }
    }
}

impl std::error::Error for SkyGeometryError {}

/// One row from a `sky.wea` material table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkyMaterialRow {
    /// Legacy numeric id preserved from the source row.
    pub legacy_id: i32,
    /// Material lookup name.
    pub name: String,
}

/// Parsed dynamic material names from `sky.wea`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkyMaterials {
    /// Main material rows in file order.
    pub rows: Vec<SkyMaterialRow>,
    /// Optional lightmap rows after the `LIGHTMAPS` section.
    pub lightmaps: Vec<SkyMaterialRow>,
}

impl SkyMaterials {
    /// Parses a text `sky.wea` payload.
    pub fn parse(bytes: &[u8]) -> Result<Self, SkyWeaError> {
        let text = String::from_utf8_lossy(bytes);
        let mut lines = text.lines().enumerate();
        let (count_line, count_text) = lines
            .find_map(|(line, text)| (!text.trim().is_empty()).then_some((line, text.trim())))
            .ok_or(SkyWeaError::Empty)?;
        let count = count_text
            .parse::<usize>()
            .map_err(|_| SkyWeaError::InvalidCount {
                line: count_line,
                text: count_text.to_string(),
            })?;
        if count == 0 {
            return Err(SkyWeaError::ZeroCount);
        }

        let mut rows = Vec::with_capacity(count);
        for index in 0..count {
            let Some((line, text)) = lines.next() else {
                return Err(SkyWeaError::MissingRow { index, count });
            };
            let text = text.trim();
            if text.is_empty() {
                return Err(SkyWeaError::InvalidRow {
                    index,
                    line,
                    text: text.to_string(),
                });
            }
            rows.push(parse_material_row(index, line, text)?);
        }

        let mut lightmaps = Vec::new();
        let mut rest = lines.filter_map(|(line, text)| {
            let trimmed = text.trim();
            (!trimmed.is_empty()).then_some((line, trimmed))
        });
        if let Some((line, marker)) = rest.next() {
            if !marker.eq_ignore_ascii_case("LIGHTMAPS") {
                return Err(SkyWeaError::UnexpectedTrailingLine {
                    line,
                    text: marker.to_string(),
                });
            }
            let Some((count_line, count_text)) = rest.next() else {
                return Err(SkyWeaError::InvalidLightmapCount {
                    line,
                    text: String::new(),
                });
            };
            let lightmap_count =
                count_text
                    .parse::<usize>()
                    .map_err(|_| SkyWeaError::InvalidLightmapCount {
                        line: count_line,
                        text: count_text.to_string(),
                    })?;
            lightmaps.reserve(lightmap_count);
            for index in 0..lightmap_count {
                let Some((row_line, row_text)) = rest.next() else {
                    return Err(SkyWeaError::MissingLightmapRow {
                        index,
                        count: lightmap_count,
                    });
                };
                lightmaps.push(parse_material_row(index, row_line, row_text)?);
            }
            if let Some((extra_line, extra_text)) = rest.next() {
                return Err(SkyWeaError::UnexpectedTrailingLine {
                    line: extra_line,
                    text: extra_text.to_string(),
                });
            }
        }

        Ok(Self { rows, lightmaps })
    }

    /// Creates a table from caller-provided names, assigning positional ids.
    #[must_use]
    pub fn from_names<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            rows: names
                .into_iter()
                .enumerate()
                .map(|(index, name)| SkyMaterialRow {
                    legacy_id: index as i32,
                    name: name.into(),
                })
                .collect(),
            lightmaps: Vec::new(),
        }
    }

    /// Returns the positional material row.
    #[must_use]
    pub fn row(&self, index: usize) -> Option<&SkyMaterialRow> {
        self.rows.get(index)
    }

    /// Returns the material name at a positional row.
    #[must_use]
    pub fn material_name(&self, index: usize) -> Option<&str> {
        self.row(index).map(|row| row.name.as_str())
    }

    /// Returns all main material names in file order.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.rows.iter().map(|row| row.name.as_str()).collect()
    }
}

impl From<Vec<String>> for SkyMaterials {
    fn from(names: Vec<String>) -> Self {
        Self::from_names(names)
    }
}

impl Default for SkyMaterials {
    fn default() -> Self {
        Self::from_names(OBSERVED_SKY_MATERIAL_NAMES)
    }
}

fn parse_material_row(
    index: usize,
    line: usize,
    text: &str,
) -> Result<SkyMaterialRow, SkyWeaError> {
    let mut parts = text.splitn(2, char::is_whitespace);
    let legacy_text = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default().trim();
    let legacy_id = legacy_text
        .parse::<i32>()
        .map_err(|_| SkyWeaError::InvalidRow {
            index,
            line,
            text: text.to_string(),
        })?;
    if name.is_empty() {
        return Err(SkyWeaError::InvalidRow {
            index,
            line,
            text: text.to_string(),
        });
    }
    Ok(SkyMaterialRow {
        legacy_id,
        name: name.to_string(),
    })
}

/// Errors returned while parsing `sky.wea`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SkyWeaError {
    /// Payload has no non-empty lines.
    Empty,
    /// Main row count is malformed.
    InvalidCount { line: usize, text: String },
    /// Main table declared no rows.
    ZeroCount,
    /// A main row is missing.
    MissingRow { index: usize, count: usize },
    /// A main or lightmap row is malformed.
    InvalidRow {
        index: usize,
        line: usize,
        text: String,
    },
    /// Lightmap count is malformed.
    InvalidLightmapCount { line: usize, text: String },
    /// A lightmap row is missing.
    MissingLightmapRow { index: usize, count: usize },
    /// Non-empty data followed the main rows without a known section marker.
    UnexpectedTrailingLine { line: usize, text: String },
}

impl fmt::Display for SkyWeaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "sky.wea payload is empty"),
            Self::InvalidCount { line, text } => {
                write!(f, "invalid sky.wea count at line {line}: {text}")
            }
            Self::ZeroCount => write!(f, "sky.wea material count must be positive"),
            Self::MissingRow { index, count } => {
                write!(f, "missing sky.wea row {index} of {count}")
            }
            Self::InvalidRow { index, line, text } => {
                write!(f, "invalid sky.wea row {index} at line {line}: {text}")
            }
            Self::InvalidLightmapCount { line, text } => {
                write!(f, "invalid sky.wea lightmap count at line {line}: {text}")
            }
            Self::MissingLightmapRow { index, count } => {
                write!(f, "missing sky.wea lightmap row {index} of {count}")
            }
            Self::UnexpectedTrailingLine { line, text } => {
                write!(f, "unexpected sky.wea line {line}: {text}")
            }
        }
    }
}

impl std::error::Error for SkyWeaError {}

/// Parses a `sky.wea` payload without requiring callers to name the parser
/// type.
pub fn parse_sky_wea(bytes: &[u8]) -> Result<SkyMaterials, SkyWeaError> {
    SkyMaterials::parse(bytes)
}

/// The nine positional material roles used by the original sky renderer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SkyLayerKind {
    /// Nebula/background row zero.
    Nebula,
    /// Stars row one.
    Stars,
    /// Clouds row two.
    Clouds,
    /// Sun sprite row three.
    Sun,
    /// Moon sprite row four.
    Moon,
    /// First flare row five.
    Flare0,
    /// Second flare row six.
    Flare1,
    /// Snow row seven.
    Snow,
    /// Rain row eight.
    Rain,
    /// A mission-specific row after the known roles.
    Other(usize),
}

impl SkyLayerKind {
    /// Maps a positional WEA row to its known role.
    #[must_use]
    pub const fn from_row(row: usize) -> Self {
        match row {
            0 => Self::Nebula,
            1 => Self::Stars,
            2 => Self::Clouds,
            3 => Self::Sun,
            4 => Self::Moon,
            5 => Self::Flare0,
            6 => Self::Flare1,
            7 => Self::Snow,
            8 => Self::Rain,
            other => Self::Other(other),
        }
    }
}

/// Native draw order for the dome and its full-screen gradient.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SkyPassKind {
    /// Untextured full-screen gradient quad.
    ScreenGradient,
    /// Row zero nebula with row one stars bound as its secondary texture.
    NebulaStars,
    /// Untextured colored gradient over the dome.
    DomeGradient,
    /// Row two cloud dome translated below the camera.
    Clouds,
}

/// Renderer contract for one native sky pass.
#[derive(Clone, Debug, PartialEq)]
pub struct SkyPassFrame<'a> {
    /// Pass order/role.
    pub kind: SkyPassKind,
    /// Primary positional WEA material row, if textured.
    pub material_row: Option<usize>,
    /// Secondary positional WEA row for the stars texture.
    pub secondary_material_row: Option<usize>,
    /// Primary material name.
    pub material: Option<&'a str>,
    /// Secondary material name.
    pub secondary_material: Option<&'a str>,
    /// Primary vertex UV stage.
    pub uv_stage: Option<usize>,
    /// Secondary vertex UV stage.
    pub secondary_uv_stage: Option<usize>,
    /// World-space Z offset relative to camera translation.
    pub translation_z: f32,
    /// Native blend mode of the pass.
    pub blend: SkyBlendMode,
    /// Native clouds directional RGB, when the pass uses it.
    pub material_color: Option<[f32; 3]>,
}

/// Lighting-dependent colors produced by the native screen-gradient update.
///
/// `color` is the packed ARGB value sent to the full-screen gradient and to
/// the last dome ring.  `horizon_floor` is the independently computed RGB
/// floor from the light-manager glare delta; native CSky clamps every stored
/// dome palette color up to these three channels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SkyGradientFrame {
    /// Packed ARGB screen-gradient and last-ring color.
    pub color: PackedColor,
    /// Lower bound applied to dome palette RGB channels, normalized to 0..1.
    pub horizon_floor: [f32; 3],
}

impl SkyGradientFrame {
    /// Raises a packed dome color to the native lighting-dependent RGB floor.
    #[must_use]
    pub fn clamp_dome_color(self, color: PackedColor) -> PackedColor {
        let rgba = color.rgba();
        PackedColor::from_rgba([
            rgba[0].max(self.horizon_floor[0]),
            rgba[1].max(self.horizon_floor[1]),
            rgba[2].max(self.horizon_floor[2]),
            rgba[3],
        ])
    }
}

/// Per-row state supplied alongside [`SkyMesh`].
#[derive(Clone, Debug, PartialEq)]
pub struct SkyLayerFrame<'a> {
    /// Positional row in `sky.wea`.
    pub row: usize,
    /// Interpreted role for the row.
    pub kind: SkyLayerKind,
    /// Dynamic mission material name, if that row exists.
    pub material: Option<&'a str>,
    /// Whether the row is currently visible according to atmosphere state.
    pub active: bool,
    /// Vertex UV stage used by the native sky material pass.
    pub uv_stage: usize,
    /// Native UV scale for this layer's stage.
    pub uv_scale: f32,
    /// Normalized row intensity where the atmosphere supplies one.
    pub intensity: f32,
    /// Monotonic elapsed time for material UV animation.
    pub phase_seconds: f32,
}

/// Blend operation used by a directional sky sprite pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SkyBlendMode {
    /// Use the sprite's alpha channel over the current sky.
    Alpha,
    /// Add the sprite contribution to the current sky.
    Additive,
}

/// The fixed four-corner UV order used by native sun, moon, and flare quads.
///
/// `CSun` leaves a half-percent border around the source texture.  The order
/// is left-top, left-bottom, right-bottom, right-top in the native quad.
pub const SKY_SPRITE_UV: [[f32; 2]; 4] = [
    [0.005, 0.005],
    [0.005, 0.995],
    [0.995, 0.995],
    [0.995, 0.005],
];

/// Positional flare rows emitted by the original sun object.
pub const SKY_FLARE_ROWS: [usize; 12] = [5, 5, 6, 6, 5, 6, 5, 5, 6, 5, 5, 5];

const SKY_FLARE_SIZES: [f32; 12] = [0.2, 0.3, 0.2, 0.1, 0.1, 0.3, 0.3, 0.7, 0.4, 1.0, 0.3, 0.2];
const SKY_FLARE_OFFSETS: [f32; 12] = [
    1.2, 0.7, 0.5, 0.2, 0.0, -0.2, -0.3, -0.5, -0.6, -0.8, -1.0, -1.1,
];
const SKY_FLARE_COLORS: [u32; 12] = [
    0xFFB0_90A3,
    0xFF5A_58BB,
    0x9630_BE52,
    0x96C9_3432,
    0xFF30_BE52,
    0x9696_9664,
    0xFFB0_90A3,
    0xFF7C_6BC9,
    0x9630_6452,
    0xFF0B_17B9,
    0xFFB6_26B1,
    0xFF7C_C5C9,
];

const GLARE_COS_15: f32 = 0.965_925_8;
const GLARE_COS_30: f32 = 0.866_025_4;
const GLARE_COS_60: f32 = 0.5;
const SUN_SPRITE_DISTANCE: f32 = 0.325;

/// Renderer-facing state for one directional sun, moon or flare sprite.
///
/// The CPU side provides the native material row, direction, sampled RGB and
/// unit-quad UVs. A renderer projects the direction through its active camera
/// and applies the material's size/alpha state. Flare rows additionally need a
/// camera visibility or occlusion test before drawing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SkySpriteFrame<'a> {
    /// Positional role in `sky.wea`.
    pub kind: SkyLayerKind,
    /// Positional material row.
    pub row: usize,
    /// Dynamic mission material name.
    pub material: Option<&'a str>,
    /// Whether the source celestial interval is active.
    pub active: bool,
    /// Direction in world space.
    pub direction: [f32; 3],
    /// Native packed sprite RGB (`SunSample::packed[0]`).
    pub color: [f32; 3],
    /// Native packed sprite alpha (`SunSample::packed[0]`).
    pub alpha: f32,
    /// The two native half-size sample values used by sun/moon quads.
    pub size_values: [f32; 2],
    /// Sampled intensity.
    pub intensity: f32,
    /// Unit quad texture coordinates in native corner order.
    pub uv: [[f32; 2]; 4],
    /// Blend operation for the sprite pass.
    pub blend: SkyBlendMode,
    /// Flares require camera-space visibility testing.
    pub requires_occlusion: bool,
}

/// One native directional source submitted for a celestial object.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DirectionalLightFrame {
    /// False means the light slot must be removed from the light manager.
    pub active: bool,
    /// Direction used by the native light manager.
    pub direction: [f32; 3],
    /// RGB value passed to the native light manager without another intensity
    /// multiplication.
    pub color: [f32; 3],
}

impl DirectionalLightFrame {
    const INACTIVE: Self = Self {
        active: false,
        direction: [0.0; 3],
        color: [0.0; 3],
    };
}

/// Camera data needed by the native screen-space sun and flare path.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SunVisibility {
    /// Projected sun center in viewport pixels.
    pub projected_sun: [f32; 2],
    /// Viewport width and height in pixels.
    pub viewport: [f32; 2],
    /// Camera forward vector in world space.
    pub view_forward: [f32; 3],
    /// Result of the renderer's world-ray visibility query.
    pub unoccluded: bool,
}

/// One screen-space flare generated from the native flare tables.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlareQuad {
    /// Native positional material row (5 or 6).
    pub row: usize,
    /// Whether this quad should be submitted this frame.
    pub active: bool,
    /// Final RGBA color after the packed sun alpha multiplication.
    pub color: [f32; 4],
    /// Center in viewport pixels.
    pub center_pixels: [f32; 2],
    /// Equal X/Y half-size in viewport pixels.
    pub half_size_pixels: f32,
    /// Native UV order for the quad.
    pub uv: [[f32; 2]; 4],
}

/// Result of the native sun glare/flare calculation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SunOptics {
    /// RGB submitted to the primary directional light after the glare boost.
    pub primary_color: [f32; 3],
    /// Squared 15-degree cone amount before the horizon factor.
    pub glare_amount: f32,
    /// Twelve fixed-topology flare slots.
    pub flares: [FlareQuad; 12],
}

/// Errors returned when a camera projection cannot supply native sprite data.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SkyProjectionError {
    /// Viewport dimensions must be finite and positive.
    InvalidViewport([f32; 2]),
    /// Horizontal FOV must be finite and in `(0, pi)`.
    InvalidHorizontalFov(f32),
    /// A camera forward vector could not be normalized.
    InvalidViewForward([f32; 3]),
}

impl fmt::Display for SkyProjectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidViewport(value) => write!(f, "invalid sky viewport: {value:?}"),
            Self::InvalidHorizontalFov(value) => {
                write!(f, "invalid sky horizontal FOV: {value}")
            }
            Self::InvalidViewForward(value) => {
                write!(f, "invalid sky view forward vector: {value:?}")
            }
        }
    }
}

impl std::error::Error for SkyProjectionError {}

/// Errors returned while updating a persistent sky owner.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SkyUpdateError {
    /// An externally supplied atmosphere sample contains a non-finite value.
    NonFiniteAtmosphere,
    /// The persistent mesh no longer matches its immutable configuration.
    TopologyChanged,
}

impl fmt::Display for SkyUpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFiniteAtmosphere => {
                write!(f, "sky atmosphere sample contains a non-finite value")
            }
            Self::TopologyChanged => write!(f, "sky topology changed after construction"),
        }
    }
}

impl std::error::Error for SkyUpdateError {}

/// Fog values sent by the original `CSky` update.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FogFrame {
    /// Normalized RGB fog color from `SkySample::packed[1]`.
    pub color: [f32; 3],
    /// Native global lighting floor from the same sampled RGB state.
    ///
    /// `CShade` merges this vector with the material lighting accumulator by
    /// taking the per-channel maximum before the shader receives the result.
    pub lighting_floor: [f32; 3],
    /// Fog start distance (`sky.values[0] * 700`).
    pub start: f32,
    /// Fog end distance (`sky.values[1] * 700`).
    pub end: f32,
}

impl FogFrame {
    /// Converts an interpolated sky sample to fog parameters.
    #[must_use]
    pub fn from_sample(sample: &SkySample) -> Self {
        let start = finite_or_zero(sample.values[0]) * FOG_DISTANCE_SCALE;
        let end = finite_or_zero(sample.values[1]) * FOG_DISTANCE_SCALE;
        let rgba = sample.packed[1].rgba();
        let color = [rgba[0], rgba[1], rgba[2]];
        Self {
            color,
            lighting_floor: color,
            start,
            end,
        }
    }
}

/// Converts an interpolated sky sample to fog parameters.
#[must_use]
pub fn fog_frame(sample: &SkySample) -> FogFrame {
    FogFrame::from_sample(sample)
}

/// Birth and orbit settings for a sun object.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SunConfig {
    /// Time at which the stateless sun sample starts, in seconds.
    pub start_seconds: f32,
    /// Lifetime used by the original `(now - start) / life` age calculation.
    pub lifetime_seconds: f32,
    /// Two integer birth angles from the sun birth payload, in degrees.
    pub initial_angles_degrees: [f32; 2],
}

impl SunConfig {
    /// Creates sun settings.
    #[must_use]
    pub const fn new(
        start_seconds: f32,
        lifetime_seconds: f32,
        initial_angles_degrees: [f32; 2],
    ) -> Self {
        Self {
            start_seconds,
            lifetime_seconds,
            initial_angles_degrees,
        }
    }

    fn angles_radians(self) -> [f32; 2] {
        [
            finite_or_zero(self.initial_angles_degrees[0]).to_radians(),
            finite_or_zero(self.initial_angles_degrees[1]).to_radians(),
        ]
    }

    /// Converts the native birth payload emitted by the atmosphere sampler.
    #[must_use]
    pub const fn from_birth(birth: SunBirth) -> Self {
        Self::new(
            birth.start_seconds,
            birth.lifetime_seconds,
            birth.initial_angles_degrees,
        )
    }
}

impl Default for SunConfig {
    fn default() -> Self {
        Self::new(0.0, 1.0, [0.0; 2])
    }
}

/// A renderer-facing sun sample.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SunFrame {
    /// Whether the latest SunStart/SunStop interval is active.
    pub active: bool,
    /// Final world-space direction after the original birth-angle matrix.
    pub direction: [f32; 3],
    /// Intermediate orbit direction before the birth-angle matrix.
    pub orbit_direction: [f32; 3],
    /// RGB light color from `SunSample::rgb`.
    pub color: [f32; 3],
    /// RGBA color stored in the native sprite descriptor.
    pub sprite_color: [f32; 4],
    /// RGB color of the opposite directional light from packed parameter 1.
    pub secondary_color: [f32; 3],
    /// The two native screen-quad size values from the SKE tail.
    pub size_values: [f32; 2],
    /// Material row selected by the native birth dispatcher.
    pub material_row: Option<usize>,
    /// Atmosphere intensity from trailing key parameter two.
    pub intensity: f32,
    /// Clamped normalized lifecycle age.
    pub age: f32,
    /// Untransformed orbit angle in radians.
    pub theta: f32,
}

impl SunFrame {
    /// Converts a sampled sun and stateless clock into a frame.
    #[must_use]
    pub fn from_sample(sample: &SunSample, now_seconds: f32, config: SunConfig) -> Self {
        sun_frame(sample, now_seconds, config)
    }

    /// Returns an inactive frame without inventing lifecycle timing.
    #[must_use]
    pub fn inactive(sample: &SunSample) -> Self {
        let sprite_color = sample.packed[0].rgba();
        let secondary_color = sample.packed[1].rgba();
        Self {
            active: false,
            direction: [0.0; 3],
            orbit_direction: [0.0; 3],
            color: sample.rgb.map(finite_or_zero),
            sprite_color,
            secondary_color: [secondary_color[0], secondary_color[1], secondary_color[2]],
            size_values: sample.values.map(finite_or_zero),
            material_row: None,
            intensity: finite_or_zero(sample.intensity).max(0.0),
            age: 0.0,
            theta: 0.0,
        }
    }

    /// Direction from the camera toward the native celestial geometry.
    ///
    /// The light manager stores the opposite vector, so the screen/world
    /// sprite path negates [`Self::direction`].
    #[must_use]
    pub fn geometry_direction(self) -> [f32; 3] {
        self.direction.map(|component| -component)
    }

    /// Places the native celestial object at half the camera far plane.
    /// This is `camera_translation - light_direction * far / 2`, with the
    /// geometry direction exposed by [`Self::geometry_direction`].
    #[must_use]
    pub fn world_center(self, camera_translation: [f32; 3], camera_far: f32) -> [f32; 3] {
        let distance = finite_or_zero(camera_far) * 0.5;
        let direction = self.geometry_direction();
        std::array::from_fn(|index| camera_translation[index] + direction[index] * distance)
    }

    /// Returns primary and opposite directional lights in native order.
    #[must_use]
    pub fn directional_lights(self) -> [DirectionalLightFrame; 2] {
        if !self.active {
            return [DirectionalLightFrame::INACTIVE; 2];
        }
        [
            DirectionalLightFrame {
                active: true,
                direction: self.direction,
                color: self.color,
            },
            DirectionalLightFrame {
                active: true,
                direction: self.direction.map(|component| -component),
                color: self.secondary_color,
            },
        ]
    }
}

/// Converts an interpolated atmosphere sun sample to a renderer-facing frame.
///
/// The original update computes `orbit_direction = [cos(theta), 0,
/// -sin(theta)]`, then applies the row-vector product of the two birth-angle
/// matrices.  With `A` and `B` in radians, the resulting direction is:
///
/// ```text
/// [x*cos(A), -x*sin(A)*cos(B) + z*sin(B),
///  x*sin(A)*sin(B) + z*cos(B)]
/// ```
///
/// where `x` and `z` are the two non-zero orbit components.
#[must_use]
pub fn sun_frame(sample: &SunSample, now_seconds: f32, config: SunConfig) -> SunFrame {
    let color = sample.rgb.map(finite_or_zero);
    let sprite_color = sample.packed[0].rgba();
    let secondary = sample.packed[1].rgba();
    let intensity = finite_or_zero(sample.intensity).max(0.0);
    if !sample.active {
        return SunFrame::inactive(sample);
    }

    let now_seconds = finite_or_zero(now_seconds);
    let start_seconds = finite_or_zero(config.start_seconds);
    let lifetime = finite_or_zero(config.lifetime_seconds);
    let age = if lifetime > 0.0 {
        ((now_seconds - start_seconds) / lifetime).clamp(0.0, 1.0)
    } else {
        1.0
    };
    let theta = (age * 1.2 - 0.1) * std::f32::consts::PI;
    let orbit_direction = [theta.cos(), 0.0, -theta.sin()];
    let [angle_a, angle_b] = config.angles_radians();
    let (sin_a, cos_a) = angle_a.sin_cos();
    let (sin_b, cos_b) = angle_b.sin_cos();
    let x = orbit_direction[0];
    let z = orbit_direction[2];
    let direction = normalize3([
        x * cos_a,
        -x * sin_a * cos_b + z * sin_b,
        x * sin_a * sin_b + z * cos_b,
    ]);
    SunFrame {
        active: true,
        direction,
        orbit_direction,
        color,
        sprite_color,
        secondary_color: [secondary[0], secondary[1], secondary[2]],
        size_values: sample.values.map(finite_or_zero),
        material_row: sample.birth.map(|birth| birth.material_row),
        intensity,
        age,
        theta,
    }
}

/// Converts a sampled celestial object using the birth payload generated by
/// the atmosphere dispatcher.
///
/// The schedule sampler carries the absolute start time, lifetime, angles and
/// material row for the active interval. Keeping that payload in the sample
/// means a renderer does not need a stateful wrapper or a caller-provided
/// placeholder configuration.
#[must_use]
pub fn celestial_frame(sample: &SunSample, now_seconds: f32) -> SunFrame {
    sample
        .birth
        .map(|birth| sun_frame(sample, now_seconds, SunConfig::from_birth(birth)))
        .unwrap_or_else(|| SunFrame::inactive(sample))
}

/// Computes the native half-size of a sun or moon screen quad in pixels.
///
/// The SKE tail values are the first two fields of `SunSample::values`; they
/// are deliberately not read from the material descriptor.  The renderer
/// supplies the real viewport and horizontal FOV for the current swapchain.
pub fn sprite_half_size_pixels(
    values: [f32; 2],
    viewport: [f32; 2],
    horizontal_fov: f32,
) -> Result<[f32; 2], SkyProjectionError> {
    validate_viewport(viewport)?;
    if !horizontal_fov.is_finite()
        || horizontal_fov <= 0.0
        || horizontal_fov >= std::f32::consts::PI
    {
        return Err(SkyProjectionError::InvalidHorizontalFov(horizontal_fov));
    }
    let scale = viewport[0] / horizontal_fov * SUN_SPRITE_DISTANCE * 0.5;
    Ok([
        scale * finite_or_zero(values[0]),
        scale * finite_or_zero(values[1]),
    ])
}

/// Computes native glare and flare quads for the current camera.
pub fn sun_optics(
    sun: SunFrame,
    visibility: SunVisibility,
) -> Result<SunOptics, SkyProjectionError> {
    validate_viewport(visibility.viewport)?;
    let view_forward = normalize3_checked(visibility.view_forward)?;
    let light_direction = sun.direction;
    let to_sun = light_direction.map(|component| -component);
    let cone = dot3(view_forward, to_sun);
    let cone_amount = ((cone - GLARE_COS_15) / (1.0 - GLARE_COS_15)).max(0.0);
    let glare_amount = cone_amount * cone_amount;
    let projected_inside = visibility.projected_sun[0].is_finite()
        && visibility.projected_sun[1].is_finite()
        && visibility.projected_sun[0] >= 0.0
        && visibility.projected_sun[0] <= visibility.viewport[0]
        && visibility.projected_sun[1] >= 0.0
        && visibility.projected_sun[1] <= visibility.viewport[1];
    let primary_length = length3(sun.color);
    let visible = sun.active && projected_inside && visibility.unoccluded && primary_length > 1.1;

    let mut primary_color = sun.color;
    if visible {
        let height =
            ((-light_direction[2] - GLARE_COS_60) / (GLARE_COS_30 - GLARE_COS_60)).clamp(0.0, 1.0);
        let boost = 1.0 + 4.0 * sun.sprite_color[3].clamp(0.0, 1.0) * height * glare_amount;
        primary_color = primary_color.map(|component| component * boost);
    }

    let flare_active = visible && glare_amount <= 0.1;
    let viewport_center = [visibility.viewport[0] * 0.5, visibility.viewport[1] * 0.5];
    let flares = std::array::from_fn(|index| {
        let base = PackedColor(SKY_FLARE_COLORS[index]);
        let rgba = base.rgba();
        let base_alpha = ((SKY_FLARE_COLORS[index] >> 24) & 0xFF) as f32;
        let alpha = (base_alpha * sun.sprite_color[3].clamp(0.0, 1.0)).round_ties_even() / 255.0;
        let offset = SKY_FLARE_OFFSETS[index];
        let center = [
            viewport_center[0] + (visibility.projected_sun[0] - viewport_center[0]) * offset,
            viewport_center[1] + (visibility.projected_sun[1] - viewport_center[1]) * offset,
        ];
        FlareQuad {
            row: SKY_FLARE_ROWS[index],
            active: flare_active,
            color: [rgba[0], rgba[1], rgba[2], alpha],
            center_pixels: center,
            half_size_pixels: visibility.viewport[0] * SKY_FLARE_SIZES[index] / 8.0,
            uv: SKY_SPRITE_UV,
        }
    });
    Ok(SunOptics {
        primary_color,
        glare_amount,
        flares,
    })
}

fn validate_viewport(viewport: [f32; 2]) -> Result<(), SkyProjectionError> {
    if viewport[0].is_finite() && viewport[1].is_finite() && viewport[0] > 0.0 && viewport[1] > 0.0
    {
        Ok(())
    } else {
        Err(SkyProjectionError::InvalidViewport(viewport))
    }
}

fn normalize3_checked(vector: [f32; 3]) -> Result<[f32; 3], SkyProjectionError> {
    let length = length3(vector);
    if length.is_finite() && length > 0.0 {
        Ok(vector.map(|component| component / length))
    } else {
        Err(SkyProjectionError::InvalidViewForward(vector))
    }
}

fn length3(vector: [f32; 3]) -> f32 {
    (vector[0] * vector[0] + vector[1] * vector[1] + vector[2] * vector[2]).sqrt()
}

fn dot3(left: [f32; 3], right: [f32; 3]) -> f32 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

/// Samples the native full-screen sky gradient and lighting-dependent horizon
/// floor for one view.
///
/// The original `CSky::Render` code at `481C5` converts `(yaw + pi)` to
/// degrees with the x87 nearest-even conversion, chooses one of four
/// ninety-degree sectors, and interpolates the corresponding two entries of
/// `SkySample::colors[0..4]`.  The later `483A3..485EE` block derives a
/// non-negative light-manager glare RGB delta and raises the stored dome
/// palette to its piecewise RGB floor.  The renderer supplies
/// `glare_rgb_delta = max(current_light_rgb - base_light_rgb, 0)` componentwise;
/// it is not a camera or projection vector.
#[must_use]
pub fn screen_gradient_frame(
    sample: &SkySample,
    yaw_radians: f32,
    glare_rgb_delta: [f32; 3],
) -> SkyGradientFrame {
    let yaw_radians = finite_or_zero(yaw_radians);
    let phase_degrees = ((yaw_radians + NATIVE_PI) * (180.0 / NATIVE_PI)).round_ties_even();
    let phase_degrees = phase_degrees as i64;
    let segment = phase_degrees.div_euclid(NATIVE_GRADIENT_SEGMENT_DEGREES);
    let remainder = phase_degrees.rem_euclid(NATIVE_GRADIENT_SEGMENT_DEGREES);
    let palette_index = (segment - 2).rem_euclid(4) as usize;
    let next_palette_index = (palette_index + 1) & 3;
    let palette_fraction = remainder as f32 * NATIVE_GRADIENT_FRACTION_SCALE;
    let color = interpolate_gradient_color(
        sample.colors[palette_index],
        sample.colors[next_palette_index],
        palette_fraction,
    );
    let glare_rgb_delta = glare_rgb_delta.map(|component| finite_or_zero(component).max(0.0));
    let horizon_floor = glare_rgb_delta.map(native_horizon_floor_component);
    SkyGradientFrame {
        color,
        horizon_floor,
    }
}

fn interpolate_gradient_color(
    first: PackedColor,
    second: PackedColor,
    fraction: f32,
) -> PackedColor {
    let channel = |shift: u32| {
        let a = ((first.raw() >> shift) & 0xFF) as f32;
        let b = ((second.raw() >> shift) & 0xFF) as f32;
        (a + (b - a) * fraction).clamp(0.0, 255.0).round_ties_even() as u32
    };
    PackedColor(0xFF00_0000 | (channel(16) << 16) | (channel(8) << 8) | channel(0))
}

fn native_horizon_floor_component(delta: f32) -> f32 {
    let mapped = if delta <= NATIVE_GRADIENT_ONE {
        delta * NATIVE_GRADIENT_SHORT_SCALE
    } else if delta <= NATIVE_GRADIENT_THREE {
        delta * NATIVE_GRADIENT_LONG_SCALE + NATIVE_GRADIENT_LONG_OFFSET
    } else {
        NATIVE_GRADIENT_ONE
    };
    // Keep the normalized API bounded even if a caller supplies a malformed
    // negative or non-finite value after the input sanitization above.
    mapped.clamp(NATIVE_GRADIENT_ZERO, NATIVE_GRADIENT_ONE)
}

/// Complete CPU output for one sky frame.
#[derive(Clone, Debug, PartialEq)]
pub struct SkyFrame<'a> {
    /// Interpolated atmosphere sky payload retained for renderer extensions.
    pub sky: SkySample,
    /// Sun and light-manager state.
    pub sun: SunFrame,
    /// Independent moon object state from the moon SunStart/SunStop interval.
    pub moon: SunFrame,
    /// Primary and opposite light for the sun, followed by the moon pair.
    pub directional_lights: [DirectionalLightFrame; 4],
    /// Native directional sprite data for sun, moon and both flares.
    pub sprites: [SkySpriteFrame<'a>; 4],
    /// Native sky draw order. The renderer submits these four passes as a
    /// stable topology and changes only sampled uniforms/material dynamics.
    pub passes: [SkyPassFrame<'a>; 4],
    /// Fog state.
    pub fog: FogFrame,
    /// Native global lighting floor copied from the sampled sky state.
    pub lighting_floor: [f32; 3],
    /// Per-row cloud/star/sprite state.
    pub layers: Vec<SkyLayerFrame<'a>>,
    /// Clock value supplied by the caller.
    pub time_seconds: f32,
}

impl<'a> SkyFrame<'a> {
    /// Builds a frame from one sampled atmosphere snapshot.
    #[must_use]
    pub fn from_atmosphere(
        atmosphere: &AtmosphereFrame,
        materials: &'a SkyMaterials,
        now_seconds: f32,
    ) -> SkyFrame<'a> {
        let sky = atmosphere.sky.clone();
        let sun = celestial_frame(&atmosphere.sun, now_seconds);
        let moon = celestial_frame(&atmosphere.moon, now_seconds);
        let time_seconds = finite_or_zero(now_seconds);
        let fog = fog_frame(&sky);
        let passes = build_passes(materials, &sky);
        Self {
            sky,
            sun,
            moon,
            directional_lights: celestial_lights(&sun, &moon),
            sprites: build_sprites(materials, &sun, &moon),
            passes,
            lighting_floor: fog.lighting_floor,
            fog,
            layers: build_layers(materials, &sun, atmosphere, time_seconds),
            time_seconds,
        }
    }

    /// Computes camera-dependent sun glare and the fixed twelve flare slots.
    pub fn sun_optics(&self, visibility: SunVisibility) -> Result<SunOptics, SkyProjectionError> {
        sun_optics(self.sun, visibility)
    }

    /// Computes the lighting-dependent native screen-gradient and horizon floor.
    #[must_use]
    pub fn screen_gradient(&self, yaw_radians: f32, glare_rgb_delta: [f32; 3]) -> SkyGradientFrame {
        screen_gradient_frame(&self.sky, yaw_radians, glare_rgb_delta)
    }
}

/// Stateful CPU sky owner suitable for a renderer's per-frame update.
#[derive(Clone, Debug)]
pub struct SkySystem {
    /// Dome geometry configuration.
    pub geometry: SkyGeometryConfig,
    /// Mission material rows.
    pub materials: SkyMaterials,
    /// Static dome topology with mutable per-frame colors.
    mesh: SkyMesh,
}

impl SkySystem {
    /// Creates a sky system.
    pub fn new(
        geometry: SkyGeometryConfig,
        materials: SkyMaterials,
    ) -> Result<Self, SkyGeometryError> {
        Ok(Self {
            geometry,
            materials,
            mesh: geometry.try_topology()?,
        })
    }

    /// Returns the persistent dome mesh.
    #[must_use]
    pub const fn mesh(&self) -> &SkyMesh {
        &self.mesh
    }

    /// Updates only dynamic colors and frame state on the persistent topology.
    pub fn update(
        &mut self,
        atmosphere: &AtmosphereFrame,
        now_seconds: f32,
    ) -> Result<SkyFrame<'_>, SkyUpdateError> {
        validate_atmosphere(atmosphere)?;
        let expected_vertices = self
            .geometry
            .azimuth_count
            .checked_mul(self.geometry.rings)
            .and_then(|count| count.checked_add(1));
        let expected_indices = self
            .geometry
            .azimuth_count
            .checked_mul(self.geometry.rings.saturating_mul(2).saturating_sub(1))
            .and_then(|count| count.checked_mul(3));
        if expected_vertices != Some(self.mesh.vertices.len())
            || expected_indices != Some(self.mesh.indices.len())
        {
            return Err(SkyUpdateError::TopologyChanged);
        }
        let sky = atmosphere.sky.clone();
        self.mesh.update_colors(&sky);
        let sun = celestial_frame(&atmosphere.sun, now_seconds);
        let moon = celestial_frame(&atmosphere.moon, now_seconds);
        let time_seconds = finite_or_zero(now_seconds);
        let fog = fog_frame(&sky);
        let passes = build_passes(&self.materials, &sky);
        Ok(SkyFrame {
            sky,
            sun,
            moon,
            directional_lights: celestial_lights(&sun, &moon),
            sprites: build_sprites(&self.materials, &sun, &moon),
            passes,
            lighting_floor: fog.lighting_floor,
            fog,
            layers: build_layers(&self.materials, &sun, atmosphere, time_seconds),
            time_seconds,
        })
    }
}

fn build_sprites<'a>(
    materials: &'a SkyMaterials,
    sun: &SunFrame,
    moon: &SunFrame,
) -> [SkySpriteFrame<'a>; 4] {
    [
        sprite_frame(
            materials,
            SkyLayerKind::Sun,
            3,
            sun,
            false,
            SkyBlendMode::Alpha,
        ),
        sprite_frame(
            materials,
            SkyLayerKind::Moon,
            4,
            moon,
            false,
            SkyBlendMode::Alpha,
        ),
        sprite_frame(
            materials,
            SkyLayerKind::Flare0,
            5,
            sun,
            true,
            SkyBlendMode::Additive,
        ),
        sprite_frame(
            materials,
            SkyLayerKind::Flare1,
            6,
            sun,
            true,
            SkyBlendMode::Additive,
        ),
    ]
}

fn build_passes<'a>(materials: &'a SkyMaterials, sky: &SkySample) -> [SkyPassFrame<'a>; 4] {
    [
        SkyPassFrame {
            kind: SkyPassKind::ScreenGradient,
            material_row: None,
            secondary_material_row: None,
            material: None,
            secondary_material: None,
            uv_stage: None,
            secondary_uv_stage: None,
            translation_z: 0.0,
            blend: SkyBlendMode::Alpha,
            material_color: None,
        },
        SkyPassFrame {
            kind: SkyPassKind::NebulaStars,
            material_row: Some(0),
            secondary_material_row: Some(1),
            material: materials.material_name(0),
            secondary_material: materials.material_name(1),
            uv_stage: Some(0),
            secondary_uv_stage: Some(1),
            translation_z: 0.0,
            blend: SkyBlendMode::Alpha,
            material_color: None,
        },
        SkyPassFrame {
            kind: SkyPassKind::DomeGradient,
            material_row: None,
            secondary_material_row: None,
            material: None,
            secondary_material: None,
            uv_stage: None,
            secondary_uv_stage: None,
            translation_z: 0.0,
            blend: SkyBlendMode::Alpha,
            material_color: None,
        },
        SkyPassFrame {
            kind: SkyPassKind::Clouds,
            material_row: Some(2),
            secondary_material_row: None,
            material: materials.material_name(2),
            secondary_material: None,
            uv_stage: Some(2),
            secondary_uv_stage: None,
            translation_z: -5000.0,
            blend: SkyBlendMode::Alpha,
            material_color: {
                let rgba = sky.packed[0].rgba();
                Some([rgba[0], rgba[1], rgba[2]])
            },
        },
    ]
}

fn sprite_frame<'a>(
    materials: &'a SkyMaterials,
    kind: SkyLayerKind,
    row: usize,
    source: &SunFrame,
    requires_occlusion: bool,
    blend: SkyBlendMode,
) -> SkySpriteFrame<'a> {
    SkySpriteFrame {
        kind,
        row,
        material: materials.material_name(row),
        active: source.active,
        direction: source.geometry_direction(),
        color: [
            source.sprite_color[0],
            source.sprite_color[1],
            source.sprite_color[2],
        ],
        alpha: source.sprite_color[3],
        size_values: source.size_values,
        intensity: source.intensity,
        uv: SKY_SPRITE_UV,
        blend,
        requires_occlusion,
    }
}

fn build_layers<'a>(
    materials: &'a SkyMaterials,
    sun: &SunFrame,
    atmosphere: &AtmosphereFrame,
    phase_seconds: f32,
) -> Vec<SkyLayerFrame<'a>> {
    let count = materials.rows.len().max(SKY_MATERIAL_COUNT);
    (0..count)
        .map(|row| {
            let kind = SkyLayerKind::from_row(row);
            let (active, intensity, uv_stage) = match kind {
                SkyLayerKind::Nebula => (true, 1.0, 0),
                SkyLayerKind::Stars => (true, 1.0, 1),
                SkyLayerKind::Clouds => (true, 1.0, 2),
                SkyLayerKind::Sun | SkyLayerKind::Flare0 | SkyLayerKind::Flare1 => {
                    (sun.active, sun.intensity, 0)
                }
                SkyLayerKind::Moon => (atmosphere.moon.active, atmosphere.moon.intensity, 0),
                SkyLayerKind::Snow => (atmosphere.snow.active, atmosphere.snow.intensity, 0),
                SkyLayerKind::Rain => (atmosphere.rain.active, atmosphere.rain.intensity, 0),
                SkyLayerKind::Other(_) => (true, 1.0, 0),
            };
            SkyLayerFrame {
                row,
                kind,
                material: materials.material_name(row),
                active,
                uv_stage,
                uv_scale: SKY_UV_SCALES[uv_stage],
                intensity: finite_or_zero(intensity).max(0.0),
                phase_seconds,
            }
        })
        .collect()
}

fn celestial_lights(sun: &SunFrame, moon: &SunFrame) -> [DirectionalLightFrame; 4] {
    let sun_lights = sun.directional_lights();
    let moon_lights = moon.directional_lights();
    [sun_lights[0], sun_lights[1], moon_lights[0], moon_lights[1]]
}

fn validate_atmosphere(atmosphere: &AtmosphereFrame) -> Result<(), SkyUpdateError> {
    let finite = |values: &[f32]| values.iter().all(|value| value.is_finite());
    if finite(&atmosphere.sky.values)
        && finite(&atmosphere.sun.rgb)
        && finite(&atmosphere.sun.values)
        && atmosphere.sun.intensity.is_finite()
        && finite(&atmosphere.moon.rgb)
        && finite(&atmosphere.moon.values)
        && atmosphere.moon.intensity.is_finite()
        && atmosphere.rain.intensity.is_finite()
        && atmosphere.snow.intensity.is_finite()
        && atmosphere.lightning.intensity.is_finite()
        && atmosphere.real_seconds.is_finite()
        && atmosphere.track_seconds.is_finite()
        && atmosphere.day_seconds.is_finite()
    {
        Ok(())
    } else {
        Err(SkyUpdateError::NonFiniteAtmosphere)
    }
}

fn finite_or_zero(value: f32) -> f32 {
    if value.is_finite() {
        value
    } else {
        0.0
    }
}

fn normalize3(vector: [f32; 3]) -> [f32; 3] {
    let length = (vector[0] * vector[0] + vector[1] * vector[1] + vector[2] * vector[2]).sqrt();
    if length.is_finite() && length > 0.0 {
        vector.map(|component| component / length)
    } else {
        [0.0; 3]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atmosphere::{AtmosphereFrame, WeatherSample};

    fn sample() -> SkySample {
        let colors = std::array::from_fn(|index| PackedColor(0xFF00_0000 | index as u32));
        SkySample {
            colors,
            values: [1.0, 2.0],
            packed: [PackedColor(0), PackedColor(0xFF11_2233)],
        }
    }

    fn sun(active: bool) -> SunSample {
        SunSample {
            active,
            rgb: [0.5, 0.25, 0.125],
            values: [0.0; 2],
            packed: [PackedColor::default(); 2],
            intensity: 0.5,
            birth: None,
        }
    }

    fn atmosphere(active: bool) -> AtmosphereFrame {
        AtmosphereFrame {
            real_seconds: 0.0,
            track_index: 0,
            track_seconds: 0.0,
            day_seconds: 0.0,
            sky: sample(),
            sun: sun(active),
            moon: sun(false),
            rain: WeatherSample {
                active: false,
                intensity: 0.0,
                color: [0.0; 4],
                raw_color: [0.0; 4],
                resources: Vec::new(),
            },
            snow: WeatherSample {
                active: false,
                intensity: 0.0,
                color: [0.0; 4],
                raw_color: [0.0; 4],
                resources: Vec::new(),
            },
            lightning: WeatherSample {
                active: false,
                intensity: 0.0,
                color: [0.0; 4],
                raw_color: [0.0; 4],
                resources: Vec::new(),
            },
        }
    }

    #[test]
    fn geometry_matches_original_counts_positions_uv_and_winding() {
        let config = SkyGeometryConfig::new(10.0, std::f32::consts::FRAC_PI_2, 2.0, 4, 3);
        let mesh = config.try_mesh(&sample()).expect("valid mesh");
        assert_eq!(mesh.vertices.len(), 13);
        assert_eq!(mesh.indices.len(), 4 * (6 * 3 - 3));
        assert_eq!(mesh.indices[0..3], [0, 1, 4]);
        assert_eq!(mesh.indices[3..9], [1, 2, 4, 4, 2, 5]);
        assert_eq!(mesh.vertices[0].position, [0.0, 0.0, 20.0]);
        assert!((mesh.vertices[1].position[0]).abs() < 1.0e-5);
        assert!((mesh.vertices[1].position[1] - 5.0).abs() < 1.0e-5);
        assert!((mesh.vertices[1].position[2] - 17.320_508).abs() < 1.0e-5);
        assert!((mesh.vertices[1].uv[1][1] - 7.5).abs() < 1.0e-5);
        assert_eq!(mesh.vertices[0].normal, [0, 0, 127]);
        assert_eq!(mesh.vertices[0].normal_vector(), [0.0, 0.0, 1.0]);
    }

    #[test]
    fn normals_use_signed_127_nearest_even_encoding() {
        assert_eq!(normal_byte(0.5), 64);
        assert_eq!(normal_byte(-0.5), -64);
        assert_eq!(normal_byte(-1.0), -127);
    }

    #[test]
    fn colors_use_zenith_and_three_interpolated_quadrant_belts() {
        let mut sky = sample();
        sky.colors[12] = PackedColor(0xFFAA_BBCC);
        sky.colors[8] = PackedColor(0xFF00_0000);
        sky.colors[9] = PackedColor(0xFF00_FF00);
        sky.colors[10] = PackedColor(0xFF00_00FF);
        sky.colors[11] = PackedColor(0xFFFF_FFFF);
        let config = SkyGeometryConfig::new(10.0, 1.0, 1.0, 12, 4);
        let mesh = config.try_mesh(&sky).expect("valid mesh");
        assert_eq!(mesh.vertices[0].color.raw(), 0xFFAA_BBCC);
        assert_eq!(mesh.vertices[1].color.raw(), 0xFFAA_BBCC);
        // First belt, azimuth one: one third from colors 8 to 9.
        assert_eq!(mesh.vertices[6].color.raw(), PackedColor(0xFF00_5500).raw());
        // The next belt starts at the fourth ring, and the last belt is used
        // for every ring after the three original color bands.
        let ring_two = 1 + 2;
        let ring_three = 1 + 3;
        assert_eq!(mesh.vertices[ring_two].color.raw(), sky.colors[4].raw());
        assert_eq!(mesh.vertices[ring_three].color.raw(), sky.colors[0].raw());
    }

    #[test]
    fn wea_parser_keeps_dynamic_names_and_optional_lightmaps() {
        let table = SkyMaterials::parse(
            b"2\r\n7 custom_snow\r\n8 custom_rain\r\n\r\nLIGHTMAPS\r\n1\r\n0 lm\r\n",
        )
        .expect("sky.wea");
        assert_eq!(table.material_name(0), Some("custom_snow"));
        assert_eq!(table.rows[1].legacy_id, 8);
        assert_eq!(table.lightmaps[0].name, "lm");
    }

    #[test]
    fn fog_uses_original_scale_and_packed_rgb() {
        let frame = fog_frame(&sample());
        assert_eq!(frame.start, 700.0);
        assert_eq!(frame.end, 1_400.0);
        assert_eq!(
            frame.color,
            [
                0x11 as f32 / 255.0,
                0x22 as f32 / 255.0,
                0x33 as f32 / 255.0
            ]
        );
    }

    #[test]
    fn screen_gradient_uses_native_yaw_and_lighting_floor() {
        let mut sky = sample();
        sky.colors[0] = PackedColor(0xFF00_0000);
        sky.colors[1] = PackedColor(0xFF00_FF00);
        sky.colors[2] = PackedColor(0xFF00_00FF);
        sky.colors[3] = PackedColor(0xFFFF_0000);

        let at_sector_start = screen_gradient_frame(&sky, 0.0, [0.0; 3]);
        assert_eq!(at_sector_start.color, sky.colors[0]);

        let halfway = screen_gradient_frame(&sky, std::f32::consts::FRAC_PI_4, [0.5, 2.0, 4.0]);
        assert_eq!(halfway.color, PackedColor(0xFF00_8000));
        assert_eq!(halfway.horizon_floor, [0.4, 0.9, 1.0]);
        assert_eq!(
            halfway.clamp_dome_color(PackedColor(0xFF10_2030)),
            PackedColor(0xFF66_E6_FF)
        );
    }

    #[test]
    fn lighting_floor_maps_native_boundaries() {
        for (delta, expected) in [(0.5, 0.4), (1.0, 0.8), (2.0, 0.9), (3.0, 1.0), (4.0, 1.0)] {
            let frame = screen_gradient_frame(&sample(), 0.0, [delta; 3]);
            assert_eq!(frame.horizon_floor, [expected; 3], "delta={delta}");
        }
    }

    #[test]
    fn sun_orientation_applies_birth_matrix_after_orbit_vector() {
        let sample = sun(true);
        let at_start = sun_frame(&sample, 0.0, SunConfig::new(0.0, 1.0, [90.0, 0.0]));
        assert!((at_start.direction[0]).abs() < 1.0e-5);
        assert!((at_start.direction[1] + at_start.orbit_direction[0]).abs() < 1.0e-5);
        assert!((at_start.direction[2] - at_start.orbit_direction[2]).abs() < 1.0e-5);

        let side = sun_frame(&sample, 0.0, SunConfig::new(0.0, 1.0, [0.0, 90.0]));
        assert!((side.direction[0] - side.orbit_direction[0]).abs() < 1.0e-5);
        assert!((side.direction[1] - side.orbit_direction[2]).abs() < 1.0e-5);
        assert!(side.direction[2].abs() < 1.0e-5);
    }

    #[test]
    fn native_birth_keeps_the_real_start_time() {
        assert!(!celestial_frame(&sun(false), 100.0).active);
        let birth = SunBirth {
            start_seconds: 90.0,
            lifetime_seconds: 20.0,
            initial_angles_degrees: [0.0; 2],
            material_row: 3,
        };
        let mut active = sun(true);
        active.birth = Some(birth);
        let first = celestial_frame(&active, 100.0);
        let later = celestial_frame(&active, 105.0);
        assert!((first.age - 0.5).abs() < 1.0e-5);
        assert!((later.age - 0.75).abs() < 1.0e-5);
        assert!(!celestial_frame(&sun(false), 106.0).active);
    }

    #[test]
    fn frame_exposes_dynamic_layers_and_moon_direction() {
        let atmosphere = atmosphere(true);
        let materials = SkyMaterials::from_names(["nebula", "stars", "clouds", "sun", "moon"]);
        let frame = SkyFrame::from_atmosphere(&atmosphere, &materials, 0.0);
        assert_eq!(frame.layers[3].material.as_deref(), Some("sun"));
        assert_eq!(frame.layers[8].material, None);
        assert_eq!(frame.moon.direction, [0.0; 3]);
        assert_eq!(frame.sprites[0].material, Some("sun"));
    }

    #[test]
    fn system_reuses_topology_and_updates_only_colors() {
        let materials = SkyMaterials::from_names(["nebula", "stars", "clouds"]);
        let mut system = SkySystem::new(SkyGeometryConfig::default(), materials).expect("geometry");
        let vertex_ptr = system.mesh().vertices.as_ptr();
        let index_ptr = system.mesh().indices.as_ptr();
        let positions = system
            .mesh()
            .vertices
            .iter()
            .map(|vertex| vertex.position)
            .collect::<Vec<_>>();

        let mut atmosphere = atmosphere(true);
        atmosphere.sky.colors[12] = PackedColor(0xFF12_3456);
        let (lighting_floor, fog_color, sprites_have_native_uv) = {
            let frame = system.update(&atmosphere, 10.0).expect("sky update");
            (
                frame.lighting_floor,
                frame.fog.color,
                frame
                    .sprites
                    .iter()
                    .all(|sprite| sprite.uv == SKY_SPRITE_UV),
            )
        };

        assert_eq!(system.mesh().vertices.as_ptr(), vertex_ptr);
        assert_eq!(system.mesh().indices.as_ptr(), index_ptr);
        assert_eq!(
            system
                .mesh()
                .vertices
                .iter()
                .map(|vertex| vertex.position)
                .collect::<Vec<_>>(),
            positions
        );
        assert_eq!(system.mesh().vertices[0].color, PackedColor(0xFF12_3456));
        assert_eq!(lighting_floor, fog_color);
        assert!(sprites_have_native_uv);
    }

    #[test]
    fn native_sprite_and_light_contract_uses_packed_secondary_values() {
        let mut source = sun(true);
        source.packed = [PackedColor(0x8011_2233), PackedColor(0xFF44_5566)];
        source.values = [2.0, 3.0];
        let frame = sun_frame(&source, 0.5, SunConfig::new(0.0, 1.0, [0.0, 0.0]));
        assert_eq!(
            frame.sprite_color,
            [
                0x11 as f32 / 255.0,
                0x22 as f32 / 255.0,
                0x33 as f32 / 255.0,
                0x80 as f32 / 255.0,
            ]
        );
        let lights = frame.directional_lights();
        assert_eq!(lights[0].color, frame.color);
        assert_eq!(lights[1].direction, frame.direction.map(|value| -value));
        assert_eq!(
            lights[1].color,
            [
                0x44 as f32 / 255.0,
                0x55 as f32 / 255.0,
                0x66 as f32 / 255.0,
            ]
        );
        assert_eq!(
            frame.geometry_direction(),
            frame.direction.map(|value| -value)
        );
        let center = frame.world_center([1.0, 2.0, 3.0], 100.0);
        assert!((center[0] - 1.0).abs() < 1.0e-4);
        assert!((center[1] - 2.0).abs() < 1.0e-4);
        assert!((center[2] - 53.0).abs() < 1.0e-4);
        assert_eq!(
            sprite_half_size_pixels(frame.size_values, [1000.0, 500.0], 2.0).expect("projection"),
            [162.5, 243.75]
        );
    }

    #[test]
    fn glare_boost_and_flares_follow_native_visibility_gates() {
        let mut source = sun(true);
        source.rgb = [1.0, 1.0, 1.0];
        source.packed[0] = PackedColor(0xFF11_2233);
        let sun = sun_frame(&source, 0.5, SunConfig::new(0.0, 1.0, [0.0, 0.0]));
        let visibility = SunVisibility {
            projected_sun: [50.0, 50.0],
            viewport: [100.0, 100.0],
            view_forward: sun.geometry_direction(),
            unoccluded: true,
        };
        let glare = sun_optics(sun, visibility).expect("optics");
        assert_eq!(glare.glare_amount, 1.0);
        assert!(glare.primary_color[0] > sun.color[0]);
        assert!(glare.flares.iter().all(|flare| !flare.active));

        let mut visibility = visibility;
        visibility.view_forward = sun.direction;
        let flare = sun_optics(sun, visibility).expect("optics");
        assert!(flare.glare_amount < 0.1);
        assert!(flare.flares.iter().all(|quad| quad.active));
        assert_eq!(flare.flares[0].row, 5);
        assert_eq!(flare.flares[0].uv, SKY_SPRITE_UV);
        assert_eq!(flare.flares[0].center_pixels, [50.0, 50.0]);
    }

    #[test]
    fn native_passes_preserve_nebula_stars_gradient_cloud_order() {
        let materials = SkyMaterials::from_names([
            "nebula", "stars", "clouds", "sun", "moon", "flare0", "flare1", "snow", "rain",
        ]);
        let sky = sample();
        let frame = SkyFrame::from_atmosphere(&atmosphere(false), &materials, 0.0);
        assert_eq!(frame.passes.len(), 4);
        assert_eq!(frame.passes[0].kind, SkyPassKind::ScreenGradient);
        assert_eq!(frame.passes[1].kind, SkyPassKind::NebulaStars);
        assert_eq!(frame.passes[1].material_row, Some(0));
        assert_eq!(frame.passes[1].secondary_material_row, Some(1));
        assert_eq!(frame.passes[1].uv_stage, Some(0));
        assert_eq!(frame.passes[1].secondary_uv_stage, Some(1));
        assert_eq!(frame.passes[2].kind, SkyPassKind::DomeGradient);
        assert_eq!(frame.passes[3].kind, SkyPassKind::Clouds);
        assert_eq!(frame.passes[3].uv_stage, Some(2));
        assert_eq!(frame.passes[3].translation_z, -5000.0);
        let rgba = sky.packed[0].rgba();
        assert_eq!(
            frame.passes[3].material_color,
            Some([rgba[0], rgba[1], rgba[2]])
        );
    }

    #[test]
    fn sky_update_rejects_non_finite_input() {
        let mut system = SkySystem::new(SkyGeometryConfig::default(), SkyMaterials::default())
            .expect("geometry");
        let mut frame = atmosphere(false);
        frame.sky.values[0] = f32::NAN;
        assert_eq!(
            system.update(&frame, 0.0),
            Err(SkyUpdateError::NonFiniteAtmosphere)
        );
    }
}
