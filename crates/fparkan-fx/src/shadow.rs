//! CPU reference implementation of the native projected shadow path.
//!
//! The original renderer keeps shadow pages in one 256 by 256 ARGB4444
//! texture.  This module deliberately keeps that work independent from a
//! graphics backend: callers provide world-space caster and receiver
//! triangles, and receive an RGBA8 atlas plus ordinary projected geometry.

use std::error::Error;
use std::fmt;

/// Native shadow atlas width in texels.
pub const SHADOW_ATLAS_WIDTH: usize = 256;
/// Native shadow atlas height in texels.
pub const SHADOW_ATLAS_HEIGHT: usize = 256;
/// Number of native page slots.
pub const SHADOW_PAGE_COUNT: usize = 28;
/// Maximum number of shadow casters considered by the native frame builder.
///
/// The atlas has 28 physical slots, but the native `CShade` object allocates
/// only 20 live shadow records per frame.
pub const SHADOW_MAX_ACTIVE_CASTERS: usize = 20;
/// Native primary directional-light flag.
pub const SHADOW_PRIMARY_DIRECTIONAL_FLAG: u32 = 0x0800_0000;
/// Full receiver flag used by the ordinary shadow query.
///
/// `ShadowTriangle::mask` contains the full decoded face flags.  The native
/// general query excludes a triangle when this bit is present.
pub const SHADOW_RECEIVER_MASK: u32 = 0x2000;
/// Compact query mask passed to the native four-ray world call.
pub const SHADOW_FOUR_RAY_QUERY_MASK_COMPACT: u32 = 0x8008;
/// Full receiver flags produced by expanding [`SHADOW_FOUR_RAY_QUERY_MASK_COMPACT`].
///
/// Terrain expands the compact descriptor before testing a receiver triangle,
/// so this is the mask that belongs in `ShadowTriangle::mask` comparisons.
pub const SHADOW_FOUR_RAY_MASK: u32 = 0x0020_0020;
const SHADOW_PROJECTION_EPSILON: f32 = -1.175_494_4e-37;
const SHADOW_CLIP_EPSILON: f32 = f32::MIN_POSITIVE;
const SHADOW_QUAD_UV_MAX: f32 = 0.99;

/// Bounding sphere used by native light selection and page projection.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadowSphere {
    /// World-space center.
    pub center: [f32; 3],
    /// Radius in world units.
    pub radius: f32,
}

/// World-space triangle used by a caster or receiver.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadowTriangle {
    /// Triangle positions in world space.
    pub positions: [[f32; 3]; 3],
    /// World-space normal.  `new` derives this value when the caller does not
    /// already have the native normal.
    pub normal: [f32; 3],
    /// Full decoded native face flags used as receiver exclusion bits.
    pub mask: u32,
}

/// A world-space shadow caster.
#[derive(Clone, Debug, PartialEq)]
pub struct ShadowCaster {
    /// Stable caster id from the caller's object table.
    pub id: u32,
    /// Native object axes.  Each entry is one world-space axis.
    pub world_axes: [[f32; 3]; 3],
    /// Native bounding sphere.
    pub sphere: ShadowSphere,
    /// Native object kind (`3` is the building path and `4` is the four-ray
    /// actor path).
    pub native_kind: u32,
    /// Entire visible UNIT triangle set in world space.
    pub triangles: Vec<ShadowTriangle>,
}

/// Camera values consumed by the native projected-size shadow LOD test.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadowCamera {
    /// World-space camera position.
    pub position: [f32; 3],
    /// Viewport width in pixels.
    pub viewport_width: f32,
    /// Horizontal field of view in radians.
    pub horizontal_fov: f32,
}

/// Native shadow quality controls used by the page allocator and LOD test.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadowLodSettings {
    /// Minimum building LOD (`kind == 3`).
    pub building_min_lod: u8,
    /// Building screen-size multiplier.
    pub building_detail: f32,
    /// Minimum BTLU/robot LOD (`kind == 4`).
    pub robot_min_lod: u8,
    /// Robot screen-size multiplier.
    pub robot_detail: f32,
}

impl Default for ShadowLodSettings {
    fn default() -> Self {
        Self {
            building_min_lod: 1,
            building_detail: 1.0,
            robot_min_lod: 1,
            robot_detail: 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ShadowCacheSlot {
    caster: u32,
    age: u32,
}

/// Reusable native page ownership state.
///
/// The original allocator keeps page ownership and an age counter between
/// frames.  Keeping that small piece of state separate from the CPU raster
/// output lets callers rebuild the atlas without changing a caster's UV page
/// whenever the camera moves.
#[derive(Clone, Debug)]
pub struct ShadowPageCache {
    slots: [Option<ShadowCacheSlot>; SHADOW_PAGE_COUNT],
    /// Slots claimed while building the current frame.  A slot already used
    /// by an earlier candidate must not be evicted again by a later one.
    frame_used: [bool; SHADOW_PAGE_COUNT],
}

impl Default for ShadowPageCache {
    fn default() -> Self {
        Self {
            slots: [None; SHADOW_PAGE_COUNT],
            frame_used: [false; SHADOW_PAGE_COUNT],
        }
    }
}

impl ShadowPageCache {
    /// Builds a frame while preserving page ownership across calls.
    pub fn build(&mut self, scene: ShadowScene<'_>) -> Result<ShadowFrame, ShadowError> {
        self.begin_frame();
        build_shadow_frame_inner(scene, Some(self))
    }

    /// Clears all page ownership.  The next build will allocate from the
    /// native preferred page groups again.
    pub fn clear(&mut self) {
        self.slots = [None; SHADOW_PAGE_COUNT];
        self.frame_used = [false; SHADOW_PAGE_COUNT];
    }

    /// Returns the cached page slot for a caster, if it currently owns one.
    #[must_use]
    pub fn page_slot(&self, caster: u32) -> Option<usize> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| {
                slot.filter(|slot| slot.caster == caster)
                    .map(|slot| (index, slot.age))
            })
            .min_by_key(|&(_, age)| age)
            .map(|(index, _)| index)
    }

    fn begin_frame(&mut self) {
        self.frame_used = [false; SHADOW_PAGE_COUNT];
        for slot in &mut self.slots {
            if let Some(slot) = slot {
                slot.age = slot.age.saturating_add(1);
            }
        }
    }

    fn assign(&mut self, caster: u32, lod: u8) -> Option<usize> {
        // A native candidate is submitted at most once per frame.  Preserve
        // that invariant even when a caller repeats an id: falling through
        // from the preferred group to the fallback group must not allocate a
        // second page for the same caster.
        if self.slots.iter().enumerate().any(|(index, slot)| {
            self.frame_used[index] && slot.is_some_and(|slot| slot.caster == caster)
        }) {
            return None;
        }
        let large = lod <= 1;
        let preferred = if large { 0..12 } else { 12..28 };
        let fallback = if large { 12..28 } else { 0..28 };
        let threshold = if large { 1 } else { 6 };
        self.assign_range(caster, preferred, 6)
            .or_else(|| self.assign_range(caster, fallback, threshold))
    }

    /// Applies the native allocator's inclusive-range search:
    /// existing owner, first free slot, then the oldest eligible slot.  A
    /// current-frame slot is never eligible for reuse.
    fn assign_range(
        &mut self,
        caster: u32,
        range: std::ops::Range<usize>,
        threshold: u32,
    ) -> Option<usize> {
        let mut oldest = None;
        // 30BE0 scans the inclusive page range once.  An existing owner or
        // the first free page wins immediately; only an occupied page past
        // the age threshold is deferred until the complete scan.
        for index in range {
            if self.frame_used[index] {
                continue;
            }
            match self.slots[index] {
                Some(slot) if slot.caster == caster => return self.claim(index, caster),
                None => return self.claim(index, caster),
                Some(slot) if slot.age >= threshold => {
                    // Native max-age selection keeps the later physical page
                    // on ties.
                    if oldest.is_none_or(|(age, old_index)| {
                        slot.age > age || (slot.age == age && index > old_index)
                    }) {
                        oldest = Some((slot.age, index));
                    }
                }
                Some(_) => {}
            }
        }
        oldest.and_then(|(_, index)| self.claim(index, caster))
    }

    fn claim(&mut self, index: usize, caster: u32) -> Option<usize> {
        if self.frame_used.get(index).copied().unwrap_or(true) {
            return None;
        }
        self.frame_used[index] = true;
        self.slots[index] = Some(ShadowCacheSlot { caster, age: 0 });
        Some(index)
    }
}

/// Native light records that can contribute to projected shadows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ShadowLight {
    /// A directional sun or moon light.
    Directional {
        /// Native world-space direction.
        direction: [f32; 3],
        /// RGB intensity.
        rgb: [f32; 3],
        /// Whether the native slot is live.
        active: bool,
        /// Native light flags.
        flags: u32,
    },
    /// A native point light.
    Point {
        /// World-space point-light position.
        position: [f32; 3],
        /// RGB intensity.
        rgb: [f32; 3],
        /// Light range.
        range: f32,
        /// Native attenuation coefficients.
        coefficients: [f32; 3],
        /// Whether the native slot is live.
        active: bool,
        /// Native light flags.
        flags: u32,
    },
}

/// The projection transform and opacity used by one atlas page.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ShadowProjection {
    /// Row-major matrix from shadow-local coordinates to world space.
    pub world_from_shadow: [f32; 16],
    /// Row-major matrix from world space to shadow-local coordinates.
    pub shadow_from_world: [f32; 16],
    /// Shadow axis (`M.Z`) used by the native receiver test.
    pub shadow_axis: [f32; 3],
    /// Shadow opacity before the receiver normal term.
    pub opacity: f32,
    /// Upper and lower native extrusion distances along `shadow_axis`.
    pub upper_distance: f32,
    pub lower_distance: f32,
    /// Maximum light strength used by receiver opacity.
    light_strength: f32,
}

/// One occupied native atlas page.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ShadowPage {
    /// Top-left atlas origin.
    pub origin: [u32; 2],
    /// Page side in texels, either 64 or 32.
    pub size: u32,
    /// Caster assigned to this page.
    caster: u32,
    /// Native projection transform for this page.
    pub projection: ShadowProjection,
}

/// One vertex of a projected receiver shadow.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadowVertex {
    /// World-space receiver position.
    pub position: [f32; 3],
    /// Normalized UV into the 256 by 256 atlas.
    pub uv: [f32; 2],
    /// Receiver alpha after native light and normal terms.
    pub alpha: f32,
    /// Receiver normal passed to the backend.
    pub normal: [f32; 3],
}

/// CPU-produced shadow frame.
#[derive(Clone, Debug, PartialEq)]
pub struct ShadowFrame {
    /// RGBA8 atlas.  Native `F000` texels become `[0, 0, 0, 255]`.
    pub atlas_rgba8: Vec<u8>,
    /// Projected receiver vertices.
    pub vertices: Vec<ShadowVertex>,
    /// Triangle-list indices into [`ShadowFrame::vertices`].
    pub indices: Vec<u32>,
}

/// Inputs for one shadow build.
pub struct ShadowScene<'a> {
    /// Active light records.
    pub lights: &'a [ShadowLight],
    /// Visible world-space casters.
    pub casters: &'a [ShadowCaster],
    /// World-space receiver triangles.
    pub receivers: &'a [ShadowTriangle],
    /// Native fade multiplier before the `0.2` opacity scale.
    pub fade: f32,
    /// Native blur passes: zero, forward once, or forward then reverse.
    pub smooth_passes: u32,
    /// Camera used for native projected-size LOD selection.  `None` keeps the
    /// CPU reference useful for callers that already performed visibility and
    /// LOD selection upstream.
    pub camera: Option<ShadowCamera>,
    /// Native shadow quality controls.
    pub lod: ShadowLodSettings,
}

/// Shadow build failures at the renderer trust boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShadowError {
    /// A caller supplied a non-finite or otherwise unusable value.
    InvalidInput(&'static str),
}

impl fmt::Display for ShadowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(field) => write!(f, "invalid shadow input: {field}"),
        }
    }
}

impl Error for ShadowError {}

/// Builds one native-style projected shadow frame.
pub fn build_shadow_frame(scene: ShadowScene<'_>) -> Result<ShadowFrame, ShadowError> {
    build_shadow_frame_inner(scene, None)
}

fn build_shadow_frame_inner(
    scene: ShadowScene<'_>,
    mut cache: Option<&mut ShadowPageCache>,
) -> Result<ShadowFrame, ShadowError> {
    if !scene.fade.is_finite() || scene.fade < 0.0 {
        return Err(ShadowError::InvalidInput("fade"));
    }
    if let Some(camera) = scene.camera {
        if !camera.position.iter().all(|value| value.is_finite())
            || !camera.viewport_width.is_finite()
            || camera.viewport_width <= 0.0
            || !camera.horizontal_fov.is_finite()
            || camera.horizontal_fov <= 0.0
        {
            return Err(ShadowError::InvalidInput("shadow camera"));
        }
    }
    if !scene.lod.building_detail.is_finite()
        || scene.lod.building_detail <= 0.0
        || !scene.lod.robot_detail.is_finite()
        || scene.lod.robot_detail <= 0.0
    {
        return Err(ShadowError::InvalidInput("shadow LOD settings"));
    }
    for light in scene.lights {
        validate_light(light)?;
    }
    for receiver in scene.receivers {
        validate_triangle(receiver, "receiver")?;
    }

    let mut frame = ShadowFrame {
        atlas_rgba8: vec![0; SHADOW_ATLAS_WIDTH * SHADOW_ATLAS_HEIGHT * 4],
        vertices: Vec::new(),
        indices: Vec::new(),
    };
    let mut pages = Vec::new();

    let mut candidates = Vec::with_capacity(scene.casters.len().min(SHADOW_MAX_ACTIVE_CASTERS));
    for caster in scene.casters {
        validate_caster(caster)?;
        let lod = scene
            .camera
            .map(|camera| projected_shadow_lod(caster, camera, scene.lod))
            .unwrap_or(0);
        if lod >= 8 {
            continue;
        }
        candidates.push((caster, lod));
        if candidates.len() == SHADOW_MAX_ACTIVE_CASTERS {
            break;
        }
    }

    let mut assignments = Vec::with_capacity(candidates.len());
    for (caster, lod) in candidates {
        let slot = match cache.as_deref_mut() {
            Some(cache) => {
                let Some(slot) = cache.assign(caster.id, lod) else {
                    // A page that is still too young, or a duplicate caster
                    // in this frame, is omitted by the native allocator.  Do
                    // not invent a stateless slot: it could overwrite a page
                    // already assigned to an earlier candidate.
                    continue;
                };
                slot
            }
            None => assignments.len(),
        };
        assignments.push((slot, caster, lod));
    }

    // Terrain owns the candidate list and performs native radius/distance
    // selection.  The CPU contract therefore consumes the visible list in
    // caller order while preserving the native 20-record frame budget.  A
    // cache supplies stable physical page slots; the stateless wrapper above
    // retains the original deterministic slot order for simple callers.
    for (slot, caster, _lod) in assignments {
        let Some(contribution) = select_light(caster, scene.lights) else {
            continue;
        };
        let (origin, size) = native_page_layout(slot);
        let projection = make_projection(caster, contribution, scene.fade)?;
        rasterize_page(
            &mut frame.atlas_rgba8,
            origin,
            size,
            caster,
            projection,
            scene.smooth_passes,
        );
        pages.push(ShadowPage {
            origin: [origin[0] as u32, origin[1] as u32],
            size: size as u32,
            caster: caster.id,
            projection,
        });
    }

    // Page blur happens after all page rasterization and before receiver UVs
    // are emitted, matching the native texture upload order.
    for page in &pages {
        project_receivers(&mut frame, page, scene.receivers, scene.casters);
    }
    Ok(frame)
}

/// Returns the fixed native page layout, including unused slots.
#[cfg(test)]
fn native_shadow_pages() -> Vec<([u32; 2], u32)> {
    (0..SHADOW_PAGE_COUNT)
        .map(native_page_layout)
        .map(|(origin, size)| ([origin[0] as u32, origin[1] as u32], size as u32))
        .collect()
}

#[derive(Clone, Copy, Debug)]
struct LightContribution {
    direction: [f32; 3],
    strength: f32,
}

fn select_light(caster: &ShadowCaster, lights: &[ShadowLight]) -> Option<LightContribution> {
    let center = caster.sphere.center;
    let radius = caster.sphere.radius;
    if caster.native_kind == 3 {
        for light in lights {
            if let ShadowLight::Directional {
                direction,
                rgb,
                active: true,
                flags,
            } = *light
            {
                if flags & SHADOW_PRIMARY_DIRECTIONAL_FLAG == 0 {
                    continue;
                }
                let direction = normalize([direction[0], direction[1], direction[2] - 0.3])?;
                let strength = length(rgb);
                if strength > 0.0 && strength.is_finite() {
                    return Some(LightContribution {
                        direction,
                        strength,
                    });
                }
            }
        }
        return None;
    }

    let mut direction_sum = [0.0; 3];
    let mut max_strength: f32 = 0.0;
    for light in lights {
        match *light {
            ShadowLight::Directional {
                direction,
                rgb,
                active: true,
                flags,
            } if flags & SHADOW_PRIMARY_DIRECTIONAL_FLAG != 0 => {
                let direction = [direction[0], direction[1], direction[2] - 0.3];
                let strength = length(rgb);
                if strength > 0.0 && strength.is_finite() {
                    direction_sum = add(direction_sum, direction);
                    max_strength = max_strength.max(strength);
                }
            }
            ShadowLight::Point {
                position,
                rgb,
                range,
                coefficients,
                active: true,
                flags,
            } if flags & 0x8000_0000 == 0 && range >= 10.0 => {
                let to_center = sub(center, position);
                let distance = length(to_center);
                if !(distance < range) || !distance.is_finite() {
                    continue;
                }
                let t = (range - distance) / range;
                let strength = length(rgb)
                    * (coefficients[0] + coefficients[1] * t + coefficients[2] * t * t)
                    * 1.5;
                if !(strength > 0.0) || !strength.is_finite() {
                    continue;
                }
                let Some(direction) = normalize(to_center) else {
                    continue;
                };
                let Some(direction) = normalize([direction[0], direction[1], -0.3]) else {
                    continue;
                };
                let contribution = (strength / 15.0 * range / (2.0 * radius)).min(2.0);
                direction_sum = add(direction_sum, scale(direction, contribution));
                max_strength = max_strength.max(strength);
            }
            _ => {}
        }
    }
    // The ordinary-object path retains the magnitude of the accumulated
    // vector; native uses it again in the receiver normal term.
    if length(direction_sum) <= 1.0e-6 || !length(direction_sum).is_finite() {
        return None;
    }
    Some(LightContribution {
        direction: direction_sum,
        strength: max_strength,
    })
}

fn make_projection(
    caster: &ShadowCaster,
    light: LightContribution,
    fade: f32,
) -> Result<ShadowProjection, ShadowError> {
    let center = caster.sphere.center;
    let radius = caster.sphere.radius;
    let light_direction =
        normalize(light.direction).ok_or(ShadowError::InvalidInput("light direction"))?;
    // Kind 3 arrives unit length.  Other kinds intentionally keep the
    // accumulated native vector magnitude in M.Z.
    let shadow_axis = scale(light.direction, -1.0);
    let mut axis = caster.world_axes[0];
    let mut best = f32::INFINITY;
    for candidate in caster.world_axes {
        let Some(candidate) = normalize(candidate) else {
            continue;
        };
        let score = dot(candidate, light_direction).abs();
        if score < best {
            best = score;
            axis = candidate;
        }
    }
    let mut u = normalize(cross(axis, shadow_axis));
    if u.is_none() {
        for candidate in identity_axes() {
            u = normalize(cross(candidate, shadow_axis));
            if u.is_some() {
                break;
            }
        }
    }
    let u = u.ok_or(ShadowError::InvalidInput("world axes"))?;
    let v = normalize(cross(shadow_axis, u)).ok_or(ShadowError::InvalidInput("shadow basis"))?;
    let x_column = scale(v, 2.0 * radius);
    let y_column = scale(u, -2.0 * radius);
    let z_column = shadow_axis;
    let world_from_shadow = matrix_from_columns(x_column, y_column, z_column, center);
    let shadow_from_world = inverse_shadow_matrix(v, u, shadow_axis, center, radius);
    Ok(ShadowProjection {
        world_from_shadow,
        shadow_from_world,
        shadow_axis,
        opacity: (fade * light.strength * 0.2).min(1.0),
        upper_distance: 4.0 * radius,
        lower_distance: -100.0,
        light_strength: light.strength,
    })
}

fn projected_shadow_lod(
    caster: &ShadowCaster,
    camera: ShadowCamera,
    settings: ShadowLodSettings,
) -> u8 {
    let offset = sub(camera.position, caster.sphere.center);
    let distance_squared = dot(offset, offset);
    let radius_squared = caster.sphere.radius * caster.sphere.radius;
    let measure = if distance_squared < radius_squared {
        camera.viewport_width
    } else {
        let tangent_distance = (distance_squared - radius_squared).max(0.0).sqrt();
        caster.sphere.radius.atan2(tangent_distance) * camera.viewport_width / camera.horizontal_fov
    };
    if !measure.is_finite() {
        return 8;
    }
    match caster.native_kind {
        4 => {
            let computed = if measure < 4.0 {
                8
            } else if measure >= 70.0 {
                0
            } else {
                let scaled = measure * settings.robot_detail;
                if scaled < 8.0 {
                    3
                } else if scaled < 30.0 {
                    2
                } else {
                    1
                }
            };
            computed.max(settings.robot_min_lod)
        }
        3 => {
            let computed = if measure < 10.0 {
                8
            } else if measure >= 120.0 {
                1
            } else {
                let scaled = measure * settings.building_detail;
                if scaled < 40.0 {
                    3
                } else if scaled < 80.0 {
                    2
                } else {
                    1
                }
            };
            computed.max(settings.building_min_lod)
        }
        _ => 8,
    }
}

fn native_page_layout(index: usize) -> ([usize; 2], usize) {
    // This is the physical order emitted by Terrain's page allocator, not a
    // row-major packing approximation.  The non-monotonic order is observable
    // in the native page table and is part of the texture contract.
    const LARGE: [[usize; 2]; 12] = [
        [0, 0],
        [0, 64],
        [64, 64],
        [64, 0],
        [0, 128],
        [0, 192],
        [64, 192],
        [64, 128],
        [128, 0],
        [128, 64],
        [192, 64],
        [192, 0],
    ];
    const SMALL: [[usize; 2]; 16] = [
        [128, 128],
        [128, 160],
        [160, 160],
        [160, 128],
        [128, 192],
        [128, 224],
        [160, 224],
        [160, 192],
        [192, 192],
        [192, 224],
        [224, 224],
        [224, 192],
        [192, 128],
        [192, 160],
        [224, 160],
        [224, 128],
    ];
    if index < LARGE.len() {
        (LARGE[index], 64)
    } else {
        (SMALL[index - LARGE.len()], 32)
    }
}

/// Matches the x86 `cvtt` helper used by Ngi103F0 with the renderer's
/// default nearest-even rounding mode.  `f32::round` is ties-away-from-zero,
/// so it cannot be used for the native half-pixel boundaries.
fn native_round(value: f32) -> isize {
    debug_assert!(value.is_finite());
    let lower = value.floor();
    let fraction = value - lower;
    let rounded = if fraction < 0.5 {
        lower
    } else if fraction > 0.5 {
        lower + 1.0
    } else if (lower as i64) & 1 == 0 {
        lower
    } else {
        lower + 1.0
    };
    rounded as isize
}

fn rasterize_page(
    atlas: &mut [u8],
    origin: [usize; 2],
    size: usize,
    caster: &ShadowCaster,
    projection: ShadowProjection,
    smooth_passes: u32,
) {
    for triangle in &caster.triangles {
        let points = triangle
            .positions
            .map(|position| transform_point(projection.shadow_from_world, position));
        let area = cross2(sub2(points[1], points[0]), sub2(points[2], points[0]));
        if !area.is_finite() || area < 0.0 {
            continue;
        }

        // Ngi103F0 converts the projected endpoints with the active nearest,
        // ties-to-even rounding mode.  The raster loop visits rows below the
        // rounded upper endpoint; evaluating the edge slopes at `y + 0.5`
        // reproduces the native pixel-center scanline.
        let min_y = points
            .iter()
            .map(|point| point[1])
            .fold(f32::INFINITY, f32::min);
        let max_y = points
            .iter()
            .map(|point| point[1])
            .fold(f32::NEG_INFINITY, f32::max);
        let first_y = native_round((min_y + 0.5) * size as f32);
        let last_y = native_round((max_y + 0.5) * size as f32);
        let first_y = first_y.max(0);
        let last_y = last_y.min(size as isize);
        if first_y >= last_y {
            continue;
        }
        for y in first_y as usize..last_y as usize {
            let local_y = (y as f32 + 0.5) / size as f32 - 0.5;
            let mut intersections = [0.0_f32; 3];
            let mut count = 0;
            for edge in [(0usize, 1usize), (1, 2), (2, 0)] {
                let a = points[edge.0];
                let b = points[edge.1];
                if ((a[1] <= local_y && b[1] > local_y) || (b[1] <= local_y && a[1] > local_y))
                    && count < intersections.len()
                {
                    let t = (local_y - a[1]) / (b[1] - a[1]);
                    intersections[count] = a[0] + (b[0] - a[0]) * t;
                    count += 1;
                }
            }
            if count < 2 {
                continue;
            }
            let min_x = intersections[..count]
                .iter()
                .copied()
                .fold(f32::INFINITY, f32::min);
            let max_x = intersections[..count]
                .iter()
                .copied()
                .fold(f32::NEG_INFINITY, f32::max);

            // Ngi103F0 is also used for both X intersections.  The native
            // fill includes the rounded right endpoint, which matters for
            // half-pixel and zero-width scanline cases.
            let first = native_round((min_x + 0.5) * size as f32);
            let last = native_round((max_x + 0.5) * size as f32);
            let first = first.max(0);
            let last = last.min(size as isize - 1);
            if first > last {
                continue;
            }
            for x in first as usize..=last as usize {
                write_alpha_nibble(atlas, origin, size, x, y, 15);
            }
        }
    }
    if smooth_passes > 0 {
        blur_page(atlas, origin, size, false);
    }
    if smooth_passes > 1 {
        blur_page(atlas, origin, size, true);
    }
}

fn blur_page(atlas: &mut [u8], origin: [usize; 2], size: usize, reverse: bool) {
    if size < 2 {
        return;
    }

    // FastProc3A368/24510 operates in place.  The forward pass writes the
    // top-left texel of each 2x2 window in row-major order; the last row and
    // column are never visited.  The optional reverse pass mirrors that
    // traversal and writes the bottom-right texel, leaving the first row and
    // column untouched.  Native integer division always uses four samples,
    // including at the page boundary.
    if reverse {
        for y in (1..size).rev() {
            for x in (1..size).rev() {
                let total = u32::from(read_alpha_nibble(atlas, origin, size, x - 1, y - 1))
                    + u32::from(read_alpha_nibble(atlas, origin, size, x, y - 1))
                    + u32::from(read_alpha_nibble(atlas, origin, size, x - 1, y))
                    + u32::from(read_alpha_nibble(atlas, origin, size, x, y));
                write_alpha_nibble(atlas, origin, size, x, y, (total / 4) as u8);
            }
        }
    } else {
        for y in 0..size - 1 {
            for x in 0..size - 1 {
                let total = u32::from(read_alpha_nibble(atlas, origin, size, x, y))
                    + u32::from(read_alpha_nibble(atlas, origin, size, x + 1, y))
                    + u32::from(read_alpha_nibble(atlas, origin, size, x, y + 1))
                    + u32::from(read_alpha_nibble(atlas, origin, size, x + 1, y + 1));
                write_alpha_nibble(atlas, origin, size, x, y, (total / 4) as u8);
            }
        }
    }
}

#[derive(Clone, Copy)]
struct ClipVertex {
    position: [f32; 3],
    uv: [f32; 2],
}

fn project_receivers(
    frame: &mut ShadowFrame,
    page: &ShadowPage,
    receivers: &[ShadowTriangle],
    casters: &[ShadowCaster],
) {
    let Some(caster) = casters.iter().find(|caster| caster.id == page.caster) else {
        return;
    };
    // Kind-4 small actors select the four-ray receiver path once for the
    // caster.  The native query descriptor is compact 0x8008, but Landscape
    // expands it to full face flags 0x00200020 before filtering triangles;
    // all other casters use the ordinary full 0x2000 exclusion bit.
    let ray_path = caster.native_kind == 4 && caster.sphere.radius < 10.0;
    let ray_fade = ray_path.then(|| four_ray_fade(page, receivers));
    for receiver in receivers {
        let use_rays = ray_path && receiver.mask & SHADOW_FOUR_RAY_MASK == 0;
        let use_general = !ray_path && receiver.mask & SHADOW_RECEIVER_MASK == 0;
        if !use_rays && !use_general {
            continue;
        }
        let normal = normalize(receiver.normal).unwrap_or_else(|| {
            normalize(cross(
                sub(receiver.positions[1], receiver.positions[0]),
                sub(receiver.positions[2], receiver.positions[0]),
            ))
            .unwrap_or([0.0, 0.0, 0.0])
        });
        let normal_term = dot(page.projection.shadow_axis, normal);
        if normal_term <= 0.0 || !normal_term.is_finite() {
            continue;
        }

        let plane_distance = dot(normal, receiver.positions[0]);
        let corners = shadow_quad(&page.projection);
        let mut projected = Vec::with_capacity(4);
        for (upper, lower, uv) in corners.iter() {
            let delta = sub(*lower, *upper);
            if dot(normal, delta) >= SHADOW_PROJECTION_EPSILON {
                projected.clear();
                break;
            }
            let upper_distance = dot(normal, *upper) - plane_distance;
            let lower_distance = dot(normal, *lower) - plane_distance;
            if upper_distance < 0.0 || lower_distance >= 0.0 {
                projected.clear();
                break;
            }
            let denominator = upper_distance - lower_distance;
            if denominator <= SHADOW_CLIP_EPSILON {
                projected.clear();
                break;
            }
            let t = (upper_distance / denominator).clamp(0.0, 1.0);
            projected.push(ClipVertex {
                position: lerp(*upper, *lower, t),
                uv: *uv,
            });
        }
        if projected.len() != 4 {
            continue;
        }

        // Terrain's receiver routine clips the projected four-corner quad
        // against the three receiver edge planes.  Keeping UV with the point
        // makes the later fan exactly the native interpolated quad.
        for edge in 0..3 {
            let a = receiver.positions[edge];
            let b = receiver.positions[(edge + 1) % 3];
            projected = clip_polygon(projected, |vertex| {
                dot(cross(sub(b, a), sub(vertex.position, a)), normal)
            });
        }
        projected.truncate(9);
        if projected.len() < 3 {
            continue;
        }
        let opacity = if use_rays {
            (ray_fade.unwrap_or(0.0) * page.projection.light_strength * 0.2).min(1.0)
        } else {
            page.projection.opacity
        };
        let alpha = opacity * normal_term.sqrt();
        if alpha <= 0.0 || !alpha.is_finite() {
            continue;
        }
        let start = frame.vertices.len() as u32;
        for vertex in &projected {
            frame.vertices.push(ShadowVertex {
                position: vertex.position,
                uv: [
                    (page.origin[0] as f32 + vertex.uv[0] * page.size as f32)
                        / SHADOW_ATLAS_WIDTH as f32,
                    (page.origin[1] as f32 + vertex.uv[1] * page.size as f32)
                        / SHADOW_ATLAS_HEIGHT as f32,
                ],
                alpha,
                normal,
            });
        }
        for index in 1..projected.len() - 1 {
            frame.indices.extend_from_slice(&[
                start,
                start + index as u32,
                start + index as u32 + 1,
            ]);
        }
    }
}

fn shadow_quad(projection: &ShadowProjection) -> [([f32; 3], [f32; 3], [f32; 2]); 4] {
    let uv_max = SHADOW_QUAD_UV_MAX;
    let corners = [[0.0, 0.0], [uv_max, 0.0], [uv_max, uv_max], [0.0, uv_max]];
    corners.map(|corner| {
        let local = [corner[0] - 0.5, corner[1] - 0.5];
        let uv = corner;
        (
            transform_point(
                projection.world_from_shadow,
                [local[0], local[1], projection.upper_distance],
            ),
            transform_point(
                projection.world_from_shadow,
                [local[0], local[1], projection.lower_distance],
            ),
            uv,
        )
    })
}

/// Casts the four native shadow-quad segments against receiver query
/// triangles.  A miss contributes nothing; the native average divides by the
/// number of successful ray hits rather than by four.
fn four_ray_fade(page: &ShadowPage, receivers: &[ShadowTriangle]) -> f32 {
    let corners = shadow_quad(&page.projection);
    let mut sum = 0.0;
    let mut hits = 0_u32;
    for (upper, lower, _) in corners {
        let delta = sub(lower, upper);
        let mut nearest = f32::INFINITY;
        for receiver in receivers {
            if receiver.mask & SHADOW_FOUR_RAY_MASK != 0 {
                continue;
            }
            if let Some(parameter) = segment_triangle_parameter(upper, delta, receiver) {
                let hit = add(upper, scale(delta, parameter));
                let offset = sub(hit, upper);
                let distance_squared = dot(offset, offset);
                nearest = nearest.min(distance_squared);
            }
        }
        if nearest.is_finite() {
            sum += (1.0 - nearest * 0.0001).clamp(0.0, 1.0);
            hits += 1;
        }
    }
    if hits == 0 {
        0.0
    } else {
        sum / hits as f32
    }
}

fn segment_triangle_parameter(
    origin: [f32; 3],
    delta: [f32; 3],
    triangle: &ShadowTriangle,
) -> Option<f32> {
    let edge1 = sub(triangle.positions[1], triangle.positions[0]);
    let edge2 = sub(triangle.positions[2], triangle.positions[0]);
    let p = cross(delta, edge2);
    let determinant = dot(edge1, p);
    if determinant.abs() <= SHADOW_CLIP_EPSILON {
        return None;
    }
    let inverse = 1.0 / determinant;
    let tvec = sub(origin, triangle.positions[0]);
    let u = dot(tvec, p) * inverse;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = cross(tvec, edge1);
    let v = dot(delta, q) * inverse;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let parameter = dot(edge2, q) * inverse;
    (parameter >= 0.0 && parameter <= 1.0).then_some(parameter)
}

fn clip_polygon<F>(polygon: Vec<ClipVertex>, signed_distance: F) -> Vec<ClipVertex>
where
    F: Fn(ClipVertex) -> f32,
{
    if polygon.is_empty() {
        return polygon;
    }
    let mut output = Vec::with_capacity(polygon.len() + 1);
    let mut previous = *polygon.last().expect("non-empty polygon");
    let mut previous_distance = signed_distance(previous);
    for &current in &polygon {
        let current_distance = signed_distance(current);
        let previous_inside = previous_distance >= 0.0;
        let current_inside = current_distance >= 0.0;
        if previous_inside != current_inside {
            let denominator = previous_distance - current_distance;
            let t = if denominator.abs() > SHADOW_CLIP_EPSILON {
                previous_distance / denominator
            } else {
                0.0
            };
            output.push(interpolate_clip(previous, current, t));
        }
        if current_inside {
            output.push(current);
        }
        previous = current;
        previous_distance = current_distance;
    }
    output
}

fn interpolate_clip(a: ClipVertex, b: ClipVertex, t: f32) -> ClipVertex {
    ClipVertex {
        position: lerp(a.position, b.position, t),
        uv: [
            a.uv[0] + (b.uv[0] - a.uv[0]) * t,
            a.uv[1] + (b.uv[1] - a.uv[1]) * t,
        ],
    }
}

fn validate_caster(caster: &ShadowCaster) -> Result<(), ShadowError> {
    if !caster.sphere.radius.is_finite() || caster.sphere.radius <= 0.0 {
        return Err(ShadowError::InvalidInput("caster radius"));
    }
    if !caster.sphere.center.iter().all(|value| value.is_finite())
        || !caster
            .world_axes
            .iter()
            .flatten()
            .all(|value| value.is_finite())
    {
        return Err(ShadowError::InvalidInput("caster transform"));
    }
    for triangle in &caster.triangles {
        validate_triangle(triangle, "caster")?;
    }
    Ok(())
}

fn validate_triangle(triangle: &ShadowTriangle, field: &'static str) -> Result<(), ShadowError> {
    if !triangle
        .positions
        .iter()
        .flatten()
        .all(|value| value.is_finite())
        || !triangle.normal.iter().all(|value| value.is_finite())
    {
        return Err(ShadowError::InvalidInput(field));
    }
    Ok(())
}

fn validate_light(light: &ShadowLight) -> Result<(), ShadowError> {
    match *light {
        ShadowLight::Directional { direction, rgb, .. }
            if direction.iter().all(|value| value.is_finite())
                && rgb.iter().all(|value| value.is_finite()) =>
        {
            Ok(())
        }
        ShadowLight::Point {
            position,
            rgb,
            range,
            coefficients,
            ..
        } if position.iter().all(|value| value.is_finite())
            && rgb.iter().all(|value| value.is_finite())
            && range.is_finite()
            && coefficients.iter().all(|value| value.is_finite()) =>
        {
            Ok(())
        }
        _ => Err(ShadowError::InvalidInput("light")),
    }
}

fn write_alpha_nibble(
    atlas: &mut [u8],
    origin: [usize; 2],
    size: usize,
    x: usize,
    y: usize,
    nibble: u8,
) {
    let index = ((origin[1] + y) * SHADOW_ATLAS_WIDTH + origin[0] + x) * 4;
    if index + 3 < atlas.len() {
        atlas[index] = 0;
        atlas[index + 1] = 0;
        atlas[index + 2] = 0;
        atlas[index + 3] = nibble.saturating_mul(17);
    }
    let _ = size;
}

fn read_alpha_nibble(atlas: &[u8], origin: [usize; 2], _size: usize, x: usize, y: usize) -> u8 {
    let index = ((origin[1] + y) * SHADOW_ATLAS_WIDTH + origin[0] + x) * 4;
    atlas.get(index + 3).copied().unwrap_or(0) / 17
}

fn matrix_from_columns(x: [f32; 3], y: [f32; 3], z: [f32; 3], translation: [f32; 3]) -> [f32; 16] {
    [
        x[0],
        y[0],
        z[0],
        translation[0],
        x[1],
        y[1],
        z[1],
        translation[1],
        x[2],
        y[2],
        z[2],
        translation[2],
        0.0,
        0.0,
        0.0,
        1.0,
    ]
}

fn inverse_shadow_matrix(
    v: [f32; 3],
    u: [f32; 3],
    shadow_axis: [f32; 3],
    center: [f32; 3],
    radius: f32,
) -> [f32; 16] {
    let x_scale = scale(v, 1.0 / (2.0 * radius));
    let y_scale = scale(u, -1.0 / (2.0 * radius));
    let z_scale = scale(shadow_axis, 1.0 / dot(shadow_axis, shadow_axis));
    [
        x_scale[0],
        x_scale[1],
        x_scale[2],
        -dot(x_scale, center),
        y_scale[0],
        y_scale[1],
        y_scale[2],
        -dot(y_scale, center),
        z_scale[0],
        z_scale[1],
        z_scale[2],
        -dot(z_scale, center),
        0.0,
        0.0,
        0.0,
        1.0,
    ]
}

fn transform_point(matrix: [f32; 16], point: [f32; 3]) -> [f32; 3] {
    [
        matrix[0] * point[0] + matrix[1] * point[1] + matrix[2] * point[2] + matrix[3],
        matrix[4] * point[0] + matrix[5] * point[1] + matrix[6] * point[2] + matrix[7],
        matrix[8] * point[0] + matrix[9] * point[1] + matrix[10] * point[2] + matrix[11],
    ]
}

fn identity_axes() -> [[f32; 3]; 3] {
    [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
}

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn scale(a: [f32; 3], scalar: f32) -> [f32; 3] {
    [a[0] * scalar, a[1] * scalar, a[2] * scalar]
}

fn lerp(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    add(a, scale(sub(b, a), t))
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn cross2(a: [f32; 2], b: [f32; 2]) -> f32 {
    a[0] * b[1] - a[1] * b[0]
}

fn sub2(a: [f32; 3], b: [f32; 3]) -> [f32; 2] {
    [a[0] - b[0], a[1] - b[1]]
}

fn length(a: [f32; 3]) -> f32 {
    dot(a, a).sqrt()
}

fn normalize(a: [f32; 3]) -> Option<[f32; 3]> {
    let length = length(a);
    (length > 1.0e-6 && length.is_finite()).then(|| scale(a, 1.0 / length))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_layout_has_twelve_large_and_sixteen_small_pages() {
        let pages = native_shadow_pages();
        assert_eq!(pages.len(), 28);
        assert_eq!(
            pages[..12].iter().map(|(_, size)| *size).sum::<u32>(),
            12 * 64
        );
        assert!(pages[..12].iter().all(|(_, size)| *size == 64));
        assert!(pages[12..].iter().all(|(_, size)| *size == 32));
        assert_eq!(
            pages.iter().map(|(origin, _)| *origin).collect::<Vec<_>>(),
            vec![
                [0, 0],
                [0, 64],
                [64, 64],
                [64, 0],
                [0, 128],
                [0, 192],
                [64, 192],
                [64, 128],
                [128, 0],
                [128, 64],
                [192, 64],
                [192, 0],
                [128, 128],
                [128, 160],
                [160, 160],
                [160, 128],
                [128, 192],
                [128, 224],
                [160, 224],
                [160, 192],
                [192, 192],
                [192, 224],
                [224, 224],
                [224, 192],
                [192, 128],
                [192, 160],
                [224, 160],
                [224, 128],
            ]
        );
    }

    #[test]
    fn directional_shadow_raster_and_receiver_projection_are_concrete() {
        let caster_triangle = triangle([[-1.0, -1.0, 0.0], [0.0, 1.0, 0.0], [1.0, -1.0, 0.0]], 0);
        let caster = caster(7, 2.0, 1, vec![caster_triangle]);
        let receiver = triangle([[-1.0, -1.0, 1.0], [0.0, 1.0, 1.0], [1.0, -1.0, 1.0]], 0);
        let light = ShadowLight::Directional {
            direction: [0.0, 0.0, 1.0],
            rgb: [1.0, 1.0, 1.0],
            active: true,
            flags: SHADOW_PRIMARY_DIRECTIONAL_FLAG,
        };
        let frame = build_shadow_frame(ShadowScene {
            lights: &[light],
            casters: &[caster],
            receivers: &[receiver],
            fade: 1.0,
            smooth_passes: 0,
            camera: None,
            lod: ShadowLodSettings::default(),
        })
        .expect("shadow frame");
        assert_eq!(frame.atlas_rgba8.len(), 256 * 256 * 4);
        assert!(frame.atlas_rgba8.chunks_exact(4).any(|rgba| rgba[3] == 255));
        assert!(!frame.vertices.is_empty());
        assert_eq!(frame.indices.len() % 3, 0);
        assert!(frame.vertices.iter().all(|vertex| vertex.alpha > 0.0));
    }

    #[test]
    fn point_light_range_and_native_flags_gate_pages() {
        let caster = caster(1, 1.0, 1, Vec::new());
        let inactive = ShadowLight::Point {
            position: [0.0, 0.0, -1.0],
            rgb: [1.0; 3],
            range: 20.0,
            coefficients: [1.0, 0.0, 0.0],
            active: true,
            flags: 0x8000_0000,
        };
        let frame = build_shadow_frame(ShadowScene {
            lights: &[inactive],
            casters: &[caster],
            receivers: &[],
            fade: 1.0,
            smooth_passes: 0,
            camera: None,
            lod: ShadowLodSettings::default(),
        })
        .expect("valid empty frame");
        assert!(frame.atlas_rgba8.iter().all(|byte| *byte == 0));
    }

    #[test]
    fn ordinary_directional_uses_primary_raw_direction_and_max_strength_only() {
        let caster = caster(1, 1.0, 1, Vec::new());
        let lights = [
            ShadowLight::Directional {
                direction: [2.0, 0.0, 4.0],
                rgb: [0.0, 0.0, 3.0],
                active: true,
                flags: SHADOW_PRIMARY_DIRECTIONAL_FLAG,
            },
            ShadowLight::Directional {
                direction: [100.0, 0.0, 100.0],
                rgb: [100.0; 3],
                active: true,
                flags: 0x1000_0000,
            },
        ];
        let contribution = select_light(&caster, &lights).expect("primary directional");
        assert_eq!(contribution.direction, [2.0, 0.0, 3.7]);
        assert!((contribution.strength - 3.0).abs() < 1.0e-6);
    }

    #[test]
    fn point_direction_overwrites_normalized_z_before_second_normalize() {
        let caster = caster(1, 1.0, 1, Vec::new());
        let light = ShadowLight::Point {
            position: [0.0, 0.0, -5.0],
            rgb: [1.0; 3],
            range: 20.0,
            coefficients: [1.0, 0.0, 0.0],
            active: true,
            flags: 0,
        };
        let contribution = select_light(&caster, &[light]).expect("point light");
        let expected_strength = 3.0_f32.sqrt() * 1.5;
        let expected_contribution = expected_strength / 15.0 * 20.0 / 2.0;
        assert!((contribution.direction[2] + expected_contribution).abs() < 1.0e-6);
        assert!(contribution.direction[0].abs() < 1.0e-6);
        assert!((contribution.strength - expected_strength).abs() < 1.0e-6);
    }

    #[test]
    fn four_ray_fade_averages_successes_and_zeroes_all_misses() {
        let caster = caster(1, 1.0, 4, Vec::new());
        let light = ShadowLight::Directional {
            direction: [0.0, 0.0, 1.0],
            rgb: [1.0; 3],
            active: true,
            flags: SHADOW_PRIMARY_DIRECTIONAL_FLAG,
        };
        let projection = make_projection(
            &caster,
            select_light(&caster, &[light]).expect("light"),
            1.0,
        )
        .expect("projection");
        let page = ShadowPage {
            origin: [0, 0],
            size: 64,
            caster: 1,
            projection,
        };
        let partial = triangle([[-1.0, 1.0, 1.0], [0.98, 1.0, 1.0], [-1.0, -0.98, 1.0]], 0);
        let fade = four_ray_fade(&page, &[partial]);
        assert!(
            fade > 0.9,
            "three successful rays must not be divided by four"
        );
        let fifty_world_units = triangle(
            [
                [-100.0, -100.0, 47.2],
                [100.0, -100.0, 47.2],
                [0.0, 100.0, 47.2],
            ],
            0,
        );
        let fade_at_fifty = four_ray_fade(&page, &[fifty_world_units]);
        assert!(
            (fade_at_fifty - 0.75).abs() < 1.0e-6,
            "fade at 50 world units: {fade_at_fifty}"
        );
        let miss = triangle([[10.0, 10.0, 1.0], [11.0, 10.0, 1.0], [10.0, 11.0, 1.0]], 0);
        assert_eq!(four_ray_fade(&page, &[miss]), 0.0);
        let excluded = triangle(
            [[-1.0, 1.0, 1.0], [0.98, 1.0, 1.0], [-1.0, -0.98, 1.0]],
            SHADOW_FOUR_RAY_MASK,
        );
        assert_eq!(four_ray_fade(&page, &[excluded]), 0.0);
    }

    #[test]
    fn ordinary_alpha_keeps_native_hdr_normal_term_after_opacity_clamp() {
        let caster = caster(1, 1.0, 1, Vec::new());
        let receiver = triangle([[-1.0, -1.0, 1.0], [0.0, 1.0, 1.0], [1.0, -1.0, 1.0]], 0);
        let light = ShadowLight::Directional {
            direction: [0.0, 0.0, 10.0],
            rgb: [100.0; 3],
            active: true,
            flags: SHADOW_PRIMARY_DIRECTIONAL_FLAG,
        };
        let frame = build_shadow_frame(ShadowScene {
            lights: &[light],
            casters: &[caster],
            receivers: &[receiver],
            fade: 1.0,
            smooth_passes: 0,
            camera: None,
            lod: ShadowLodSettings::default(),
        })
        .expect("HDR shadow frame");
        assert!(frame.vertices.iter().any(|vertex| vertex.alpha > 1.0));
    }

    #[test]
    fn smoothing_uses_nibble_quantization() {
        let caster_triangle = triangle([[-1.0, -1.0, 0.0], [0.0, 1.0, 0.0], [1.0, -1.0, 0.0]], 0);
        let caster = caster(1, 2.0, 1, vec![caster_triangle]);
        let light = ShadowLight::Directional {
            direction: [0.0, 0.0, 1.0],
            rgb: [1.0; 3],
            active: true,
            flags: SHADOW_PRIMARY_DIRECTIONAL_FLAG,
        };
        let frame = build_shadow_frame(ShadowScene {
            lights: &[light],
            casters: &[caster],
            receivers: &[],
            fade: 1.0,
            smooth_passes: 2,
            camera: None,
            lod: ShadowLodSettings::default(),
        })
        .expect("smooth frame");
        assert!(frame
            .atlas_rgba8
            .chunks_exact(4)
            .any(|rgba| rgba[3] > 0 && rgba[3] < 255 && rgba[3] % 17 == 0));
    }

    #[test]
    fn native_rounding_and_blur_preserve_asymmetric_page_edges() {
        assert_eq!(native_round(1.5), 2);
        assert_eq!(native_round(2.5), 2);
        assert_eq!(native_round(-0.5), 0);
        assert_eq!(native_round(-1.5), -2);

        let size = 4;
        let origin = [0, 0];
        let mut atlas = vec![0; SHADOW_ATLAS_WIDTH * SHADOW_ATLAS_HEIGHT * 4];
        for y in 0..size {
            for x in 0..size {
                write_alpha_nibble(&mut atlas, origin, size, x, y, (y * size + x) as u8);
            }
        }
        blur_page(&mut atlas, origin, size, false);
        let expected_forward = [
            [2, 3, 4, 3],
            [6, 7, 8, 7],
            [10, 11, 12, 11],
            [12, 13, 14, 15],
        ];
        for (y, row) in expected_forward.iter().enumerate() {
            for (x, expected) in row.iter().enumerate() {
                assert_eq!(
                    read_alpha_nibble(&atlas, origin, size, x, y),
                    *expected,
                    "forward blur at ({x},{y})"
                );
            }
        }

        let mut reverse_atlas = vec![0; SHADOW_ATLAS_WIDTH * SHADOW_ATLAS_HEIGHT * 4];
        for y in 0..size {
            for x in 0..size {
                write_alpha_nibble(&mut reverse_atlas, origin, size, x, y, (y * size + x) as u8);
            }
        }
        blur_page(&mut reverse_atlas, origin, size, true);
        let expected_reverse = [[0, 1, 2, 3], [4, 2, 3, 4], [8, 6, 7, 8], [12, 10, 11, 12]];
        for (y, row) in expected_reverse.iter().enumerate() {
            for (x, expected) in row.iter().enumerate() {
                assert_eq!(
                    read_alpha_nibble(&reverse_atlas, origin, size, x, y),
                    *expected,
                    "reverse blur at ({x},{y})"
                );
            }
        }
    }

    #[test]
    fn native_lod_uses_projected_measure_and_reuses_page_slots() {
        let mut building = caster(41, 1.0, 3, Vec::new());
        building.sphere.center = [0.0, 0.0, 1.05];
        let camera = ShadowCamera {
            position: [0.0, 0.0, 0.0],
            viewport_width: 100.0,
            horizontal_fov: std::f32::consts::FRAC_PI_2,
        };
        let close_lod = projected_shadow_lod(&building, camera, ShadowLodSettings::default());
        assert_eq!(close_lod, 1);

        let mut cache = ShadowPageCache::default();
        let light = ShadowLight::Directional {
            direction: [0.0, 0.0, 1.0],
            rgb: [1.0; 3],
            active: true,
            flags: SHADOW_PRIMARY_DIRECTIONAL_FLAG,
        };
        let first = cache
            .build(ShadowScene {
                lights: &[light],
                casters: &[building.clone()],
                receivers: &[],
                fade: 1.0,
                smooth_passes: 0,
                camera: Some(camera),
                lod: ShadowLodSettings::default(),
            })
            .expect("first cached frame");
        assert_eq!(
            first.atlas_rgba8.len(),
            SHADOW_ATLAS_WIDTH * SHADOW_ATLAS_HEIGHT * 4
        );
        let slot = cache.page_slot(building.id).expect("cached page");
        let second = cache
            .build(ShadowScene {
                lights: &[light],
                casters: &[building],
                receivers: &[],
                fade: 1.0,
                smooth_passes: 0,
                camera: Some(camera),
                lod: ShadowLodSettings::default(),
            })
            .expect("reused cached frame");
        assert_eq!(cache.page_slot(41), Some(slot));
        assert_eq!(second.atlas_rgba8.len(), first.atlas_rgba8.len());
    }

    #[test]
    fn native_lod_rejects_small_and_unknown_shadow_objects() {
        let camera = ShadowCamera {
            position: [0.0, 0.0, 0.0],
            viewport_width: 100.0,
            horizontal_fov: std::f32::consts::FRAC_PI_2,
        };
        let mut robot = caster(1, 1.0, 4, Vec::new());
        robot.sphere.center = [0.0, 0.0, 100.0];
        assert_eq!(
            projected_shadow_lod(&robot, camera, ShadowLodSettings::default()),
            8
        );
        let unknown = caster(2, 100.0, 10, Vec::new());
        assert_eq!(
            projected_shadow_lod(&unknown, camera, ShadowLodSettings::default()),
            8
        );
    }

    #[test]
    fn page_cache_keeps_current_frame_slots_unique_and_rejects_stale_overflow() {
        let light = ShadowLight::Directional {
            direction: [0.0, 0.0, 1.0],
            rgb: [1.0; 3],
            active: true,
            flags: SHADOW_PRIMARY_DIRECTIONAL_FLAG,
        };
        let casters = (0..40)
            .map(|id| caster(id, 1.0, 3, Vec::new()))
            .collect::<Vec<_>>();
        let mut cache = ShadowPageCache::default();

        for (start, expected_used) in [(0, 20), (20, 16), (20, 16)] {
            let selected = casters[start..start + 20].to_vec();
            let _frame = cache
                .build(ShadowScene {
                    lights: &[light],
                    casters: &selected,
                    receivers: &[],
                    fade: 1.0,
                    smooth_passes: 0,
                    camera: None,
                    lod: ShadowLodSettings::default(),
                })
                .expect("cached multi-frame shadow build");

            let used = cache
                .frame_used
                .iter()
                .enumerate()
                .filter_map(|(index, used)| used.then_some(index))
                .collect::<Vec<_>>();
            assert_eq!(used.len(), expected_used);
            let mut owners = used
                .iter()
                .map(|&index| cache.slots[index].expect("used slot owner").caster)
                .collect::<Vec<_>>();
            owners.sort_unstable();
            owners.dedup();
            assert_eq!(owners.len(), used.len(), "two current casters share a page");

            for id in 0..40 {
                let owner_count = cache
                    .slots
                    .iter()
                    .flatten()
                    .filter(|slot| slot.caster == id)
                    .count();
                assert!(owner_count <= 1, "caster {id} owns duplicate page slots");
            }
        }

        // The second frame has more new casters than the aged fallback range
        // can accept.  Rejected records must remain unowned rather than being
        // assigned an occupied stateless index.
        assert!(cache.page_slot(36).is_none());
        assert!(cache.page_slot(37).is_none());
        assert!(cache.page_slot(38).is_none());
        assert!(cache.page_slot(39).is_none());
    }

    #[test]
    fn receiver_masks_are_full_exclusions() {
        let caster = caster(1, 1.0, 4, Vec::new());
        let light = ShadowLight::Directional {
            direction: [0.0, 0.0, 1.0],
            rgb: [1.0; 3],
            active: true,
            flags: SHADOW_PRIMARY_DIRECTIONAL_FLAG,
        };
        let ground = triangle([[-1.0, -1.0, 1.0], [0.0, 1.0, 1.0], [1.0, -1.0, 1.0]], 0);
        let excluded = triangle(
            [[-1.0, -1.0, 1.0], [0.0, 1.0, 1.0], [1.0, -1.0, 1.0]],
            SHADOW_FOUR_RAY_MASK,
        );
        let accepted = build_shadow_frame(ShadowScene {
            lights: &[light],
            casters: &[caster.clone()],
            receivers: &[ground],
            fade: 1.0,
            smooth_passes: 0,
            camera: None,
            lod: ShadowLodSettings::default(),
        })
        .expect("unmasked ground receiver");
        let filtered = build_shadow_frame(ShadowScene {
            lights: &[light],
            casters: &[caster],
            receivers: &[ground, excluded],
            fade: 1.0,
            smooth_passes: 0,
            camera: None,
            lod: ShadowLodSettings::default(),
        })
        .expect("masked receiver");
        assert_eq!(filtered.vertices.len(), accepted.vertices.len());
        assert_eq!(filtered.indices.len(), accepted.indices.len());
    }

    fn triangle(positions: [[f32; 3]; 3], mask: u32) -> ShadowTriangle {
        ShadowTriangle {
            positions,
            normal: normalize(cross(
                sub(positions[1], positions[0]),
                sub(positions[2], positions[0]),
            ))
            .unwrap_or([0.0; 3]),
            mask,
        }
    }

    fn caster(
        id: u32,
        radius: f32,
        native_kind: u32,
        triangles: Vec<ShadowTriangle>,
    ) -> ShadowCaster {
        ShadowCaster {
            id,
            world_axes: identity_axes(),
            sphere: ShadowSphere {
                center: [0.0; 3],
                radius,
            },
            native_kind,
            triangles,
        }
    }
}
