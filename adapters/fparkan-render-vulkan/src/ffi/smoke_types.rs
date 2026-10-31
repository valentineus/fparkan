use ash::vk;
use fparkan_platform::{NativeWindowHandles, RenderRequest};
use fparkan_render::{LegacyD3d7Projection, LegacyPipelineState, PipelineKey, RawCameraTransform};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use super::{
    VulkanAllocatedBuffer, VulkanAllocatedImage, VulkanFrameSync, VulkanInstanceError,
    VulkanInstanceProbe, VulkanLogicalDeviceError, VulkanLogicalDeviceProbe, VulkanSurfaceError,
    VulkanSurfaceProbe, VulkanSwapchainProbe, VulkanSwapchainProbeError, VulkanSwapchainResources,
    VulkanValidationMessenger,
};
use crate::shader::VulkanShaderError;

/// Creates a live native Vulkan renderer for the Stage 0 smoke loop.
#[derive(Clone, Debug)]
pub struct VulkanSmokeRendererCreateInfo {
    /// Application name reported to the Vulkan loader.
    pub application_name: String,
    /// Native window/display handles borrowed from a live window.
    pub native_handles: NativeWindowHandles,
    /// Initial drawable extent.
    pub drawable_extent: (u32, u32),
    /// Stage 0 render request used for capability gating.
    pub render_request: RenderRequest,
    /// Whether validation layers must be enabled.
    pub enable_validation: bool,
    /// Static indexed geometry uploaded before the first live frame.
    pub mesh: VulkanStaticMesh,
    /// Camera contract for static geometry.
    ///
    /// The default identity matrix retains the initial XY diagnostic viewer.
    pub camera: VulkanStaticCamera,
    /// Material textures uploaded before the first live frame.
    ///
    /// An empty list retains the compatibility white fallback. A singleton list
    /// is a deliberately explicit one-material compatibility mode; otherwise
    /// every source batch selector must resolve to one entry in this list.
    pub materials: Vec<VulkanStaticMaterial>,
    /// Optional shared bootstrap progress tracker for failure evidence.
    pub bootstrap_progress: Option<Arc<VulkanSmokeBootstrapProgress>>,
}

/// One vertex accepted by the initial static Vulkan geometry path.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VulkanStaticVertex {
    /// World or clip position transformed by [`VulkanStaticCamera`].
    pub position: [f32; 3],
    /// Linear RGB vertex color.
    pub color: [f32; 3],
    /// Unit vertex normal in world/object coordinates.
    pub normal: [f32; 3],
    /// Texture coordinate consumed by the static material bridge.
    pub uv: [f32; 2],
    /// Repeating detail texture coordinate from terrain type-18 UV data.
    ///
    /// Non-terrain geometry supplies `[0.0; 2]`; its neutral detail texture
    /// keeps the additional stage mathematically transparent.
    pub detail_uv: [f32; 2],
    /// Interpolated terrain overlay mask from type-14 data.
    ///
    /// Non-terrain geometry supplies `0.0`, disabling the transparent overlay
    /// slot while retaining one vertex contract for every static draw.
    pub overlay_alpha: f32,
}

/// A static geometry camera represented in the shader's matrix memory order.
///
/// `clip_from_world` contains row-major D3D7 data. GLSL's default column-major
/// `mat4` interpretation deliberately transposes that storage, making
/// `matrix * vec4(position, 1)` equivalent to D3D7's row-vector multiplication.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VulkanStaticCamera {
    /// Row-major clip-from-world transform consumed by the vertex shader.
    pub clip_from_world: [f32; 16],
}

impl VulkanStaticCamera {
    /// Builds a static camera from the recovered Ngi32 D3D7 camera contract.
    #[must_use]
    pub fn from_legacy_d3d7(
        transform: RawCameraTransform,
        projection: LegacyD3d7Projection,
    ) -> Option<Self> {
        let view = transform.try_direct3d7_view_row_major()?;
        let mut projection = projection.try_direct3d7_projection_row_major()?;
        // D3D7's top-down viewport and Vulkan's positive-height viewport invert NDC Y.
        projection[5] = -projection[5];
        Self::from_row_major_view_projection(view, projection)
    }

    /// Builds a camera from finite row-major view and projection matrices.
    ///
    /// This is the runtime-facing form of the contract. It is intentionally
    /// independent of the D3D7 recovery path so a future native camera
    /// controller can submit its per-frame matrices without recreating Vulkan
    /// resources.
    #[must_use]
    pub fn from_row_major_view_projection(view: [f32; 16], projection: [f32; 16]) -> Option<Self> {
        view.iter()
            .chain(projection.iter())
            .all(|value| value.is_finite())
            .then_some(Self {
                clip_from_world: multiply_row_major(view, projection),
            })
    }

    /// Returns whether every matrix element is finite.
    #[must_use]
    pub fn is_finite(self) -> bool {
        self.clip_from_world.iter().all(|value| value.is_finite())
    }
}

impl Default for VulkanStaticCamera {
    fn default() -> Self {
        Self {
            clip_from_world: [
                1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
            ],
        }
    }
}

/// Number of directional records carried by [`VulkanFrameUniforms`].
pub const VULKAN_DIRECTIONAL_LIGHT_COUNT: usize = 4;

/// One directional light in the frame lighting contract.
///
/// The direction is the vector from the light toward the surface.  The
/// coefficients are retained in the same three-component record as the
/// native point callback so frame construction can copy the original light
/// table without a lossy conversion.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VulkanDirectionalLight {
    /// Direction from the light toward the surface.
    pub direction: [f32; 3],
    /// Light RGB multiplier.
    pub rgb: [f32; 3],
    /// Native light coefficients.  Directional callbacks normally use the
    /// neutral `[0, 0, 1]` record; the field keeps the shared light ABI.
    pub coefficients: [f32; 3],
    /// Whether the light participates in this frame.
    pub active: bool,
}

impl Default for VulkanDirectionalLight {
    fn default() -> Self {
        Self {
            direction: [0.0, 0.0, 1.0],
            rgb: [0.0; 3],
            coefficients: [0.0, 0.0, 1.0],
            active: false,
        }
    }
}

/// One point light in the frame lighting contract.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VulkanPointLight {
    /// World-space position.
    pub position: [f32; 3],
    /// Light RGB multiplier.
    pub rgb: [f32; 3],
    /// Radius used by native normalized distance attenuation.
    pub range: f32,
    /// Native attenuation coefficients `a0`, `a1`, and `a2`.
    pub coefficients: [f32; 3],
    /// Whether the light participates in this frame.
    pub active: bool,
}

impl Default for VulkanPointLight {
    fn default() -> Self {
        Self {
            position: [0.0; 3],
            rgb: [0.0; 3],
            range: 0.0,
            // A neutral record is explicit.  In particular, this is the
            // proven native environment contract and avoids an accidental
            // zero polynomial when a caller enables the point slot.
            coefficients: [0.0, 0.0, 1.0],
            active: false,
        }
    }
}

/// Number of bytes occupied by one frame-uniform slot.
///
/// The block is deliberately made entirely from `mat4`/`vec4`-sized records
/// so its CPU representation follows Vulkan's `std140` alignment rules on
/// every device.  Four directional lights and one point light fit in a small
/// dynamic UBO while the push-constant range remains available for material
/// state.
pub const VULKAN_FRAME_UNIFORM_BYTES: usize = 368;

/// Number of CPU/GPU frame slots shared by the swapchain renderer.
pub const VULKAN_FRAME_SLOT_COUNT: usize = 2;

/// `uvec4` header bytes for the dynamic point-light storage buffer.
pub const VULKAN_POINT_LIGHT_HEADER_BYTES: usize = 16;

/// Three `vec4` records per dynamically uploaded point light.
pub const VULKAN_POINT_LIGHT_RECORD_BYTES: usize = 3 * 4 * std::mem::size_of::<f32>();

/// Bytes occupied by one material's base and overlay specular records.
///
/// Each record is one `vec4`: RGB followed by the native integer power stored
/// as an IEEE-754 float for direct shader use.  The allocation stride is
/// rounded up to the device's `minStorageBufferOffsetAlignment`.
pub const VULKAN_MATERIAL_SPECULAR_RECORD_BYTES: usize = 2 * 4 * std::mem::size_of::<f32>();

/// Per-frame data shared by the vertex and fragment stages.
///
/// `clip_from_world` is row-major D3D-style storage for the same reason as
/// [`VulkanStaticCamera`].  A directional light direction points from the
/// light toward the surface; the shader negates it when taking the
/// surface-normal dot product.  Fog distances are disabled when
/// `fog_end <= fog_start`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VulkanFrameUniforms {
    /// Row-major clip-from-world transform.
    pub clip_from_world: [f32; 16],
    /// Up to four native directional/celestial lights.
    pub directional_lights: [VulkanDirectionalLight; VULKAN_DIRECTIONAL_LIGHT_COUNT],
    /// Native point light slot.
    pub point_light: VulkanPointLight,
    /// Per-channel lighting floor applied before native range compression.
    pub lighting_floor: [f32; 3],
    /// Linear fog RGB color.
    pub fog_color: [f32; 3],
    /// Fog start distance.
    pub fog_start: f32,
    /// Fog end distance.
    pub fog_end: f32,
    /// World-space camera position used for fog distance.
    pub camera_position: [f32; 3],
}

impl VulkanFrameUniforms {
    /// Builds neutral frame state from a static camera.
    #[must_use]
    pub fn from_camera(camera: VulkanStaticCamera) -> Self {
        let mut directional_lights =
            [VulkanDirectionalLight::default(); VULKAN_DIRECTIONAL_LIGHT_COUNT];
        directional_lights[0] = VulkanDirectionalLight {
            direction: [0.35, 0.45, 0.82],
            rgb: [1.0; 3],
            coefficients: [0.0, 0.0, 1.0],
            active: true,
        };
        Self {
            clip_from_world: camera.clip_from_world,
            directional_lights,
            point_light: VulkanPointLight::default(),
            lighting_floor: [0.30; 3],
            fog_color: [0.0; 3],
            fog_start: f32::MAX,
            fog_end: f32::MAX,
            camera_position: [0.0; 3],
        }
    }

    /// Returns whether all submitted frame values are finite.
    #[must_use]
    pub fn is_finite(self) -> bool {
        self.clip_from_world
            .into_iter()
            .chain(self.directional_lights.into_iter().flat_map(|light| {
                light
                    .direction
                    .into_iter()
                    .chain(light.rgb)
                    .chain(light.coefficients)
                    .chain([if light.active { 1.0 } else { 0.0 }])
            }))
            .chain(self.point_light.position)
            .chain(self.point_light.rgb)
            .chain([self.point_light.range])
            .chain(self.point_light.coefficients)
            .chain([if self.point_light.active { 1.0 } else { 0.0 }])
            .chain(self.lighting_floor)
            .chain(self.fog_color)
            .chain([self.fog_start, self.fog_end])
            .chain(self.camera_position)
            .all(f32::is_finite)
    }

    /// Serializes one frame slot in the exact `std140` member order.
    #[must_use]
    pub fn to_ne_bytes(self) -> [u8; VULKAN_FRAME_UNIFORM_BYTES] {
        let mut values = [0.0_f32; VULKAN_FRAME_UNIFORM_BYTES / 4];
        values[..16].copy_from_slice(&self.clip_from_world);
        for (index, light) in self.directional_lights.into_iter().enumerate() {
            let direction_offset = 16 + index * 4;
            values[direction_offset..direction_offset + 4].copy_from_slice(&[
                light.direction[0],
                light.direction[1],
                light.direction[2],
                if light.active { 1.0 } else { 0.0 },
            ]);
            let rgb_offset = 32 + index * 4;
            values[rgb_offset..rgb_offset + 4].copy_from_slice(&[
                light.rgb[0],
                light.rgb[1],
                light.rgb[2],
                if light.active { 1.0 } else { 0.0 },
            ]);
            let coefficient_offset = 48 + index * 4;
            values[coefficient_offset..coefficient_offset + 4].copy_from_slice(&[
                light.coefficients[0],
                light.coefficients[1],
                light.coefficients[2],
                if light.active { 1.0 } else { 0.0 },
            ]);
        }
        values[64..68].copy_from_slice(&[
            self.point_light.position[0],
            self.point_light.position[1],
            self.point_light.position[2],
            self.point_light.range,
        ]);
        values[68..72].copy_from_slice(&[
            self.point_light.rgb[0],
            self.point_light.rgb[1],
            self.point_light.rgb[2],
            if self.point_light.active { 1.0 } else { 0.0 },
        ]);
        values[72..76].copy_from_slice(&[
            self.point_light.coefficients[0],
            self.point_light.coefficients[1],
            self.point_light.coefficients[2],
            if self.point_light.active { 1.0 } else { 0.0 },
        ]);
        values[76..80].copy_from_slice(&[
            self.lighting_floor[0],
            self.lighting_floor[1],
            self.lighting_floor[2],
            0.0,
        ]);
        values[80..84].copy_from_slice(&[
            self.fog_color[0],
            self.fog_color[1],
            self.fog_color[2],
            0.0,
        ]);
        values[84..88].copy_from_slice(&[self.fog_start, self.fog_end, 0.0, 0.0]);
        values[88..92].copy_from_slice(&[
            self.camera_position[0],
            self.camera_position[1],
            self.camera_position[2],
            0.0,
        ]);
        let mut bytes = [0_u8; VULKAN_FRAME_UNIFORM_BYTES];
        for (index, value) in values.into_iter().enumerate() {
            let offset = index * std::mem::size_of::<f32>();
            bytes[offset..offset + std::mem::size_of::<f32>()]
                .copy_from_slice(&value.to_ne_bytes());
        }
        bytes
    }
}

impl Default for VulkanFrameUniforms {
    fn default() -> Self {
        Self::from_camera(VulkanStaticCamera::default())
    }
}

fn multiply_row_major(left: [f32; 16], right: [f32; 16]) -> [f32; 16] {
    let mut result = [0.0; 16];
    for row in 0..4 {
        for column in 0..4 {
            result[row * 4 + column] = (0..4)
                .map(|inner| left[row * 4 + inner] * right[inner * 4 + column])
                .sum();
        }
    }
    result
}

/// Static indexed geometry uploaded to live Vulkan buffers.
#[derive(Clone, Debug, PartialEq)]
pub struct VulkanStaticMesh {
    /// Vertex data in pipeline order.
    pub vertices: Vec<VulkanStaticVertex>,
    /// Triangle-list indices into [`Self::vertices`].
    pub indices: Vec<u32>,
    /// Source-preserving triangle draw ranges in [`Self::indices`].
    pub draw_ranges: Vec<VulkanStaticDrawRange>,
}

/// Geometry-only changes applied to one retained draw range.
///
/// The renderer owns the material, blend/depth state, active count changes,
/// and queue classification for every range.  A frame builder uses this
/// record only when a variable-size geometry stream (such as projected
/// shadows) moves its index segment or changes its triangle count.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VulkanDynamicDrawRange {
    /// Stable draw-range handle returned by the initial mesh layout.
    pub range_index: usize,
    /// First index in the current shared index upload.
    pub first_index: u32,
    /// Active triangle-list index count. Zero disables this range for the
    /// current frame without allocating a zero-sized Vulkan buffer.
    pub index_count: u32,
}

/// One indexed triangle-list draw retained from an original mesh batch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VulkanStaticDrawRange {
    /// First index in the shared index buffer.
    pub first_index: u32,
    /// Number of indices in this draw.
    pub index_count: u32,
    /// Original positional `Batch20.material_index` selector.
    pub material_index: u16,
    /// Original positional `Batch20.lightmap_index` selector.
    pub lightmap_index: u8,
    /// Original MSH Batch20 flag word used by the native transparent queue.
    /// Bits `0x100` and `0x008` opt a range into that queue independently of
    /// the material blend category.
    pub batch_flags: u16,
    /// Terrain ranges that use the native lit base-only path (face flag 0x400 clear).
    pub terrain_base_only: bool,
    /// Backend-neutral fixed-function state for this source range.
    pub pipeline_state: LegacyPipelineState,
    /// Alpha-test reference in legacy 0..=255 units; dynamic material data.
    pub alpha_test_reference: u8,
}

impl VulkanStaticDrawRange {
    /// The value installed by the native renderer before a material-specific
    /// alpha reference is written.  The native Ngi32 initialiser stores one
    /// in `D3DRS_ALPHAREF`; callers that do not have a material-specific value
    /// should pass this constant explicitly.  Zero remains a valid explicit
    /// alpha reference.
    pub const NATIVE_DEFAULT_ALPHA_TEST_REFERENCE: u8 = 1;

    /// Returns the canonical key used for Vulkan pipeline selection.
    #[must_use]
    pub fn pipeline_key(self) -> PipelineKey {
        self.pipeline_state.into()
    }

    /// Returns the normalized cutoff consumed by the fragment shader.
    #[must_use]
    pub fn alpha_test_cutoff(self) -> f32 {
        if self.pipeline_state.alpha_test {
            f32::from(self.alpha_test_reference) / 255.0
        } else {
            0.0
        }
    }
}

/// One diffuse material texture keyed by an original MSH batch selector.
#[derive(Clone, Debug, PartialEq)]
pub struct VulkanStaticMaterial {
    /// Positional material selector used by one or more source batches.
    pub material_index: u16,
    /// Native MAT0 pipeline category selected for this phase (0..=4).
    pub pipeline_category: u8,
    /// Decoded RGBA8 diffuse texture for this selector.
    pub texture: VulkanStaticTexture,
    /// Phase parameters 4..6, normalized directional light RGB coefficient.
    pub directional_rgb: [f32; 3],
    /// Phase parameters 0..2, normalized additive RGB coefficient.
    pub additive_rgb: [f32; 3],
    /// Normalized atlas transform for base texture `[x, y, w, h]`.
    pub uv_transform: [f32; 4],
    /// Optional Land2 detail texture paired with the base texture.
    pub detail_texture: Option<VulkanStaticTexture>,
    /// Optional Land1 overlay texture selected by the face tag high byte.
    pub overlay_texture: Option<VulkanStaticTexture>,
    /// Optional Land2 detail texture paired with the overlay texture.
    pub overlay_detail_texture: Option<VulkanStaticTexture>,
    /// MAT0 phase diffuse alpha, normalized from the legacy 0..=255 byte.
    pub diffuse_alpha: f32,
    /// MAT0 phase alpha for the optional overlay material.
    pub overlay_diffuse_alpha: Option<f32>,
    /// Phase parameters 4..6 for the optional overlay material.
    pub overlay_directional_rgb: Option<[f32; 3]>,
    /// Phase parameters 0..2 for the optional overlay material.
    pub overlay_additive_rgb: Option<[f32; 3]>,
    /// Phase parameters 8..10, normalized base specular RGB coefficient.
    ///
    /// Specular data is kept in a storage buffer instead of push constants so
    /// the vertex stage can evaluate the native Gouraud highlight while the
    /// material's other dynamic values retain their existing ABI.
    pub specular_rgb: [f32; 3],
    /// Phase parameter 16 for the base material, carried as the native byte.
    pub specular_power: u8,
    /// Phase parameters 8..10 for the optional overlay material.
    pub overlay_specular_rgb: Option<[f32; 3]>,
    /// Phase parameter 16 for the optional overlay material.
    pub overlay_specular_power: Option<u8>,
    /// Normalized atlas transform for the optional detail texture.
    pub detail_uv_transform: [f32; 4],
    /// Normalized atlas transform for the optional overlay texture.
    pub overlay_uv_transform: Option<[f32; 4]>,
    /// Normalized atlas transform for the optional overlay detail texture.
    pub overlay_detail_uv_transform: Option<[f32; 4]>,
    /// Environment layers bypass terrain lighting and fog while retaining the
    /// same descriptor/pipeline path.
    pub unlit: bool,
    /// Native sky mode 4: blend base and secondary textures by the secondary
    /// texture alpha before multiplying by the vertex diffuse color.
    pub sky_nebula_stars: bool,
    /// Native terrain mode 3: combine base and secondary lightmap textures
    /// without the regular terrain `2 * TEX0 * TEX1` combiner or NdotL.
    pub lightmap_mode: bool,
    /// Push material mode bit 4 and map this sky layer to the Vulkan far
    /// plane without changing its world-space vertices or UVs.  This is a
    /// renderer projection choice; it is not a claim about the original D3D
    /// depth-bit encoding.
    pub sky_far_depth: bool,
    /// Push material mode bit 8 and treat positions as Vulkan clip-space
    /// `XYZRHW` vertices. Their x/y coordinates are converted from viewport
    /// pixels by the caller and z retains the native `0.999` depth.
    pub screen_tl: bool,
}

impl VulkanStaticMaterial {
    pub(crate) fn encoded_material_mode(&self) -> f32 {
        let combiner_mode = if self.screen_tl {
            1.0
        } else if self.lightmap_mode {
            3.0
        } else if self.sky_nebula_stars {
            2.0
        } else if self.unlit {
            1.0
        } else {
            0.0
        };
        combiner_mode
            + if self.sky_far_depth && !self.screen_tl {
                4.0
            } else {
                0.0
            }
            + if self.screen_tl { 8.0 } else { 0.0 }
    }
}

/// One authored RGBA8 mip level accepted by the Vulkan texture upload path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VulkanStaticTextureMip {
    /// Mip width in texels.
    pub width: u32,
    /// Mip height in texels.
    pub height: u32,
    /// Row-major RGBA8 pixels.
    pub rgba8: Vec<u8>,
}

/// Decoded RGBA8 image accepted by the initial Vulkan texture upload path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VulkanStaticTexture {
    /// Image width in texels.
    pub width: u32,
    /// Image height in texels.
    pub height: u32,
    /// Row-major RGBA8 pixels.
    pub rgba8: Vec<u8>,
    /// Authored mip levels, including mip zero, when available.
    ///
    /// An empty vector requests deterministic CPU generation from `rgba8`.
    pub mip_levels: Vec<VulkanStaticTextureMip>,
}

impl VulkanStaticTexture {
    pub(super) fn validate(&self) -> Result<(), &'static str> {
        let pixels = usize::try_from(self.width)
            .ok()
            .and_then(|width| {
                usize::try_from(self.height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .and_then(|pixels| pixels.checked_mul(4));
        match pixels {
            None => Err("static texture dimensions overflow address space"),
            Some(0) => Err("static texture has zero extent"),
            Some(expected) if expected != self.rgba8.len() => {
                Err("static texture rgba8 byte count does not match extent")
            }
            Some(_) => {
                if self.mip_levels.is_empty() {
                    return Ok(());
                }
                // TEXM files commonly stop at the authored 4x4 level. Vulkan
                // permits a partial mip chain; only the supplied level
                // extents and bytes need validation.
                if self.mip_levels.len()
                    > usize::try_from(self.width.max(self.height))
                        .unwrap_or(usize::MAX)
                        .ilog2() as usize
                        + 1
                {
                    return Err("static texture authored mip count exceeds extent");
                }
                let mut width = self.width;
                let mut height = self.height;
                for (level, mip) in self.mip_levels.iter().enumerate() {
                    if mip.width != width || mip.height != height {
                        return Err("static texture authored mip extent does not match level");
                    }
                    let expected = usize::try_from(width)
                        .ok()
                        .and_then(|width| {
                            usize::try_from(height)
                                .ok()
                                .and_then(|height| width.checked_mul(height))
                        })
                        .and_then(|pixels| pixels.checked_mul(4));
                    if expected != Some(mip.rgba8.len()) {
                        return Err("static texture authored mip byte count does not match extent");
                    }
                    if level == 0 && mip.rgba8 != self.rgba8 {
                        return Err("static texture authored mip zero does not match rgba8");
                    }
                    width = (width / 2).max(1);
                    height = (height / 2).max(1);
                }
                Ok(())
            }
        }
    }
}

/// Resolves each source range to its descriptor-set material index.
///
/// Empty material input selects the renderer's white fallback. A singleton is
/// the explicit direct-TEXM compatibility mode. Multiple materials must map
/// each `Batch20.material_index` exactly and never duplicate a selector.
pub(super) fn resolve_draw_texture_indices(
    draw_ranges: &[VulkanStaticDrawRange],
    materials: &[VulkanStaticMaterial],
) -> Result<Vec<usize>, &'static str> {
    for (index, material) in materials.iter().enumerate() {
        material.texture.validate()?;
        if let Some(texture) = &material.detail_texture {
            texture.validate()?;
        }
        if let Some(texture) = &material.overlay_texture {
            texture.validate()?;
        }
        if let Some(texture) = &material.overlay_detail_texture {
            texture.validate()?;
        }
        if !material.diffuse_alpha.is_finite() || !(0.0..=1.0).contains(&material.diffuse_alpha) {
            return Err("static material diffuse alpha must be finite and 0..1");
        }
        if material
            .directional_rgb
            .into_iter()
            .chain(material.additive_rgb)
            .chain(material.specular_rgb)
            .chain(material.overlay_specular_rgb.into_iter().flatten())
            .any(|value| !value.is_finite())
        {
            return Err("static material lighting and specular RGB parameters must be finite");
        }
        if material
            .uv_transform
            .into_iter()
            .chain(material.detail_uv_transform)
            .any(|value| !value.is_finite())
        {
            return Err("static material UV transforms must be finite");
        }
        if material
            .overlay_diffuse_alpha
            .is_some_and(|alpha| !alpha.is_finite() || !(0.0..=1.0).contains(&alpha))
        {
            return Err("static material overlay diffuse alpha must be finite and 0..1");
        }
        if material
            .overlay_directional_rgb
            .into_iter()
            .flatten()
            .chain(material.overlay_additive_rgb.into_iter().flatten())
            .any(|value| !value.is_finite())
        {
            return Err("static material overlay RGB parameters must be finite");
        }
        if material
            .overlay_uv_transform
            .into_iter()
            .flatten()
            .chain(material.overlay_detail_uv_transform.into_iter().flatten())
            .any(|value| !value.is_finite())
        {
            return Err("static material overlay UV transforms must be finite");
        }
        if materials[..index]
            .iter()
            .any(|previous| previous.material_index == material.material_index)
        {
            return Err("static material selectors must be unique");
        }
    }
    draw_ranges
        .iter()
        .map(|range| {
            if materials.len() <= 1 {
                Ok(0)
            } else {
                materials
                    .iter()
                    .position(|material| material.material_index == range.material_index)
                    .ok_or("static mesh draw range has no material texture")
            }
        })
        .collect()
}

impl VulkanStaticMesh {
    /// Returns the compatibility triangle used by the native Stage 0 smoke app.
    #[must_use]
    pub fn smoke_triangle() -> Self {
        Self {
            vertices: vec![
                VulkanStaticVertex {
                    position: [0.0, -0.55, 0.0],
                    color: [1.0, 0.2, 0.2],
                    normal: [0.0, 0.0, 1.0],
                    uv: [0.5, 0.0],
                    detail_uv: [0.0, 0.0],
                    overlay_alpha: 0.0,
                },
                VulkanStaticVertex {
                    position: [0.55, 0.55, 0.0],
                    color: [0.2, 1.0, 0.2],
                    normal: [0.0, 0.0, 1.0],
                    uv: [1.0, 1.0],
                    detail_uv: [0.0, 0.0],
                    overlay_alpha: 0.0,
                },
                VulkanStaticVertex {
                    position: [-0.55, 0.55, 0.0],
                    color: [0.2, 0.4, 1.0],
                    normal: [0.0, 0.0, 1.0],
                    uv: [0.0, 1.0],
                    detail_uv: [0.0, 0.0],
                    overlay_alpha: 0.0,
                },
            ],
            indices: vec![0, 1, 2],
            draw_ranges: vec![VulkanStaticDrawRange {
                first_index: 0,
                index_count: 3,
                material_index: 0,
                lightmap_index: u8::MAX,
                batch_flags: 0,
                pipeline_state: LegacyPipelineState::default(),
                terrain_base_only: false,
                alpha_test_reference: 0,
            }],
        }
    }

    pub(super) fn validate(&self) -> Result<(), &'static str> {
        if self.vertices.is_empty() {
            return Err("static mesh has no vertices");
        }
        if self.vertices.iter().any(|vertex| {
            !vertex
                .position
                .iter()
                .chain(vertex.color.iter())
                .chain(vertex.normal.iter())
                .chain(vertex.uv.iter())
                .chain(vertex.detail_uv.iter())
                .chain(std::iter::once(&vertex.overlay_alpha))
                .all(|value| value.is_finite())
                || !(0.0..=1.0).contains(&vertex.overlay_alpha)
        }) {
            return Err(
                "static mesh vertex attributes must be finite and overlay alpha must be 0..1",
            );
        }
        if self.indices.is_empty() || !self.indices.len().is_multiple_of(3) {
            return Err("static mesh indices must contain complete triangles");
        }
        if self.draw_ranges.is_empty() {
            return Err("static mesh has no draw ranges");
        }
        let mut expected_first = 0_u32;
        for range in &self.draw_ranges {
            if range.first_index != expected_first
                || range.index_count == 0
                || !range.index_count.is_multiple_of(3)
            {
                return Err("static mesh draw ranges must be contiguous complete triangles");
            }
            expected_first = expected_first
                .checked_add(range.index_count)
                .ok_or("static mesh draw range exceeds index count")?;
        }
        if usize::try_from(expected_first).ok() != Some(self.indices.len()) {
            return Err("static mesh draw ranges must cover all indices");
        }
        if self.indices.iter().any(|&index| {
            usize::try_from(index)
                .ok()
                .is_none_or(|index| index >= self.vertices.len())
        }) {
            return Err("static mesh index exceeds vertex count");
        }
        Ok(())
    }
}

#[cfg(test)]
mod static_mesh_tests {
    use super::*;

    #[test]
    fn frame_uniforms_use_std140_sized_slots_and_preserve_dynamic_values() {
        let frame = VulkanFrameUniforms {
            clip_from_world: std::array::from_fn(|index| index as f32),
            directional_lights: [
                VulkanDirectionalLight {
                    direction: [1.0, 2.0, 3.0],
                    rgb: [4.0, 5.0, 6.0],
                    coefficients: [7.0, 8.0, 9.0],
                    active: true,
                },
                VulkanDirectionalLight::default(),
                VulkanDirectionalLight::default(),
                VulkanDirectionalLight::default(),
            ],
            point_light: VulkanPointLight {
                position: [28.0, 29.0, 30.0],
                rgb: [31.0, 32.0, 33.0],
                range: 34.0,
                coefficients: [35.0, 36.0, 37.0],
                active: true,
            },
            lighting_floor: [7.0, 8.0, 9.0],
            fog_color: [10.0, 11.0, 12.0],
            fog_start: 13.0,
            fog_end: 14.0,
            camera_position: [15.0, 16.0, 17.0],
        };
        let bytes = frame.to_ne_bytes();
        assert_eq!(bytes.len(), VULKAN_FRAME_UNIFORM_BYTES);
        let read =
            |offset: usize| f32::from_ne_bytes(bytes[offset..offset + 4].try_into().expect("f32"));
        assert_eq!(read(0), 0.0);
        assert_eq!(read(60), 15.0);
        assert_eq!(read(64), 1.0);
        assert_eq!(read(76), 1.0);
        assert_eq!(read(128), 4.0);
        assert_eq!(read(192), 7.0);
        assert_eq!(read(256), 28.0);
        assert_eq!(read(272), 31.0);
        assert_eq!(read(288), 35.0);
        assert_eq!(read(304), 7.0);
        assert_eq!(read(320), 10.0);
        assert_eq!(read(336), 13.0);
        assert_eq!(read(352), 15.0);
    }

    #[test]
    fn smoke_triangle_is_valid_complete_geometry() {
        let mesh = VulkanStaticMesh::smoke_triangle();

        assert_eq!(mesh.indices, vec![0, 1, 2]);
        assert_eq!(mesh.validate(), Ok(()));
    }

    #[test]
    fn static_camera_composes_recovered_d3d7_view_before_projection() {
        let transform = RawCameraTransform {
            words: [
                0.0_f32.to_bits(),
                (-1.0_f32).to_bits(),
                0.0_f32.to_bits(),
                10.0_f32.to_bits(),
                1.0_f32.to_bits(),
                0.0_f32.to_bits(),
                0.0_f32.to_bits(),
                20.0_f32.to_bits(),
                0.0_f32.to_bits(),
                0.0_f32.to_bits(),
                1.0_f32.to_bits(),
                30.0_f32.to_bits(),
                0.0_f32.to_bits(),
                0.0_f32.to_bits(),
                0.0_f32.to_bits(),
                1.0_f32.to_bits(),
            ],
        };
        let projection = LegacyD3d7Projection {
            viewport: [0, 0, 1024, 768],
            near_plane: 0.5,
            far_plane: 700.0,
            field_of_view_radians: 1.3,
        };

        let camera = VulkanStaticCamera::from_legacy_d3d7(transform, projection)
            .expect("recovered camera inputs are valid");

        assert!(camera.is_finite());
        assert_eq!(camera.clip_from_world[0], 0.65_f32.cos());
        assert_eq!(camera.clip_from_world[6], 0.65_f32.sin() * 700.0 / 699.5);
        assert_eq!(camera.clip_from_world[7], 0.65_f32.sin());
    }

    #[test]
    fn legacy_camera_maps_world_up_to_top_of_positive_height_vulkan_viewport() {
        let transform = RawCameraTransform {
            words: [
                1.0_f32.to_bits(),
                0.0_f32.to_bits(),
                0.0_f32.to_bits(),
                0.0_f32.to_bits(),
                0.0_f32.to_bits(),
                1.0_f32.to_bits(),
                0.0_f32.to_bits(),
                0.0_f32.to_bits(),
                0.0_f32.to_bits(),
                0.0_f32.to_bits(),
                1.0_f32.to_bits(),
                0.0_f32.to_bits(),
                0.0_f32.to_bits(),
                0.0_f32.to_bits(),
                0.0_f32.to_bits(),
                1.0_f32.to_bits(),
            ],
        };
        let projection = LegacyD3d7Projection {
            viewport: [0, 0, 1024, 768],
            near_plane: 0.5,
            far_plane: 700.0,
            field_of_view_radians: 1.3,
        };

        let camera = VulkanStaticCamera::from_legacy_d3d7(transform, projection)
            .expect("recovered camera inputs are valid");
        // The raw identity camera's recovered view maps world +X to depth
        // and world +Z to camera up.
        let world_up_at_positive_depth = [10.0, 0.0, 1.0, 1.0];
        let clip_y = (0..4)
            .map(|row| world_up_at_positive_depth[row] * camera.clip_from_world[row * 4 + 1])
            .sum::<f32>();
        let clip_w = (0..4)
            .map(|row| world_up_at_positive_depth[row] * camera.clip_from_world[row * 4 + 3])
            .sum::<f32>();

        assert!(clip_w > 0.0);
        assert!(
            clip_y / clip_w < 0.0,
            "world up must map to the viewport top"
        );
    }

    #[test]
    fn static_camera_accepts_finite_runtime_matrices_and_rejects_nan() {
        let identity = VulkanStaticCamera::default().clip_from_world;

        assert_eq!(
            VulkanStaticCamera::from_row_major_view_projection(identity, identity),
            Some(VulkanStaticCamera::default())
        );
        let mut invalid = identity;
        invalid[5] = f32::NAN;
        assert_eq!(
            VulkanStaticCamera::from_row_major_view_projection(invalid, identity),
            None
        );
    }

    #[test]
    fn static_mesh_rejects_bad_triangle_topology_and_indices() {
        let no_vertices = VulkanStaticMesh {
            vertices: Vec::new(),
            indices: vec![0, 1, 2],
            draw_ranges: VulkanStaticMesh::smoke_triangle().draw_ranges,
        };
        let incomplete_triangle = VulkanStaticMesh {
            vertices: VulkanStaticMesh::smoke_triangle().vertices,
            indices: vec![0, 1],
            draw_ranges: vec![VulkanStaticDrawRange {
                first_index: 0,
                index_count: 2,
                material_index: 0,
                lightmap_index: u8::MAX,
                batch_flags: 0,
                pipeline_state: LegacyPipelineState::default(),
                terrain_base_only: false,
                alpha_test_reference: 0,
            }],
        };
        let out_of_range_index = VulkanStaticMesh {
            vertices: VulkanStaticMesh::smoke_triangle().vertices,
            indices: vec![0, 1, 3],
            draw_ranges: VulkanStaticMesh::smoke_triangle().draw_ranges,
        };

        assert_eq!(no_vertices.validate(), Err("static mesh has no vertices"));
        assert_eq!(
            incomplete_triangle.validate(),
            Err("static mesh indices must contain complete triangles")
        );
        assert_eq!(
            out_of_range_index.validate(),
            Err("static mesh index exceeds vertex count")
        );
    }

    #[test]
    fn material_descriptors_follow_source_batch_selectors() {
        let ranges = [
            VulkanStaticDrawRange {
                first_index: 0,
                index_count: 3,
                material_index: 7,
                lightmap_index: u8::MAX,
                batch_flags: 0,
                pipeline_state: LegacyPipelineState::default(),
                terrain_base_only: false,
                alpha_test_reference: 0,
            },
            VulkanStaticDrawRange {
                first_index: 3,
                index_count: 3,
                material_index: 2,
                lightmap_index: u8::MAX,
                batch_flags: 0,
                pipeline_state: LegacyPipelineState::default(),
                terrain_base_only: false,
                alpha_test_reference: 0,
            },
        ];
        let texture = || VulkanStaticTexture {
            width: 1,
            height: 1,
            rgba8: vec![255; 4],
            mip_levels: Vec::new(),
        };
        let materials = [
            VulkanStaticMaterial {
                material_index: 2,
                pipeline_category: 0,
                texture: texture(),
                detail_texture: None,
                overlay_texture: None,
                overlay_detail_texture: None,
                diffuse_alpha: 1.0,
                overlay_diffuse_alpha: None,
                directional_rgb: [-0.25; 3],
                additive_rgb: [-0.5; 3],
                specular_rgb: [-0.75; 3],
                specular_power: 0,
                overlay_specular_rgb: Some([-1.0; 3]),
                overlay_specular_power: None,
                uv_transform: [0.0, 0.0, 1.0, 1.0],
                overlay_directional_rgb: Some([-0.125; 3]),
                overlay_additive_rgb: Some([-0.375; 3]),
                detail_uv_transform: [0.0, 0.0, 1.0, 1.0],
                overlay_uv_transform: None,
                overlay_detail_uv_transform: None,
                unlit: false,
                sky_nebula_stars: false,
                lightmap_mode: false,
                sky_far_depth: false,
                screen_tl: false,
            },
            VulkanStaticMaterial {
                material_index: 7,
                pipeline_category: 0,
                texture: texture(),
                detail_texture: None,
                overlay_texture: None,
                overlay_detail_texture: None,
                diffuse_alpha: 1.0,
                overlay_diffuse_alpha: None,
                directional_rgb: [1.0; 3],
                additive_rgb: [0.0; 3],
                specular_rgb: [0.0; 3],
                specular_power: 0,
                overlay_specular_rgb: None,
                overlay_specular_power: None,
                uv_transform: [0.0, 0.0, 1.0, 1.0],
                overlay_directional_rgb: None,
                overlay_additive_rgb: None,
                detail_uv_transform: [0.0, 0.0, 1.0, 1.0],
                overlay_uv_transform: None,
                overlay_detail_uv_transform: None,
                unlit: false,
                sky_nebula_stars: false,
                lightmap_mode: false,
                sky_far_depth: false,
                screen_tl: false,
            },
        ];

        assert_eq!(
            resolve_draw_texture_indices(&ranges, &materials),
            Ok(vec![1, 0])
        );

        let mut screen_tl = materials[0].clone();
        screen_tl.screen_tl = true;
        screen_tl.sky_far_depth = true;
        assert_eq!(screen_tl.encoded_material_mode(), 9.0);
        let mut far_sky = materials[0].clone();
        far_sky.unlit = true;
        far_sky.sky_far_depth = true;
        assert_eq!(far_sky.encoded_material_mode(), 5.0);
        let mut nebula_with_stars = far_sky.clone();
        nebula_with_stars.sky_nebula_stars = true;
        assert_eq!(nebula_with_stars.encoded_material_mode(), 6.0);

        let mut non_finite_base = materials.clone();
        non_finite_base[0].specular_rgb[0] = f32::NAN;
        assert_eq!(
            resolve_draw_texture_indices(&ranges, &non_finite_base),
            Err("static material lighting and specular RGB parameters must be finite")
        );

        let mut non_finite_overlay = materials.clone();
        non_finite_overlay[0].overlay_additive_rgb = Some([f32::INFINITY, 0.0, 0.0]);
        assert_eq!(
            resolve_draw_texture_indices(&ranges, &non_finite_overlay),
            Err("static material overlay RGB parameters must be finite")
        );
    }

    #[test]
    fn draw_range_pipeline_key_follows_backend_neutral_state() {
        let base = VulkanStaticMesh::smoke_triangle().draw_ranges[0];
        let blended = VulkanStaticDrawRange {
            pipeline_state: LegacyPipelineState {
                blend: fparkan_render::LegacyBlendMode::SourceAlpha,
                ..LegacyPipelineState::default()
            },
            ..base
        };

        assert_eq!(base.pipeline_key().packed(), 0);
        assert_ne!(base.pipeline_key(), blended.pipeline_key());
    }

    #[test]
    fn alpha_cutoff_is_dynamic_and_disabled_outside_alpha_test_pipeline() {
        let base = VulkanStaticMesh::smoke_triangle().draw_ranges[0];
        let disabled = VulkanStaticDrawRange {
            terrain_base_only: false,
            alpha_test_reference: 200,
            ..base
        };
        let enabled = VulkanStaticDrawRange {
            pipeline_state: LegacyPipelineState {
                alpha_test: true,
                ..LegacyPipelineState::default()
            },
            terrain_base_only: false,
            alpha_test_reference: 128,
            ..base
        };
        assert_eq!(disabled.alpha_test_cutoff(), 0.0);
        assert_eq!(enabled.alpha_test_cutoff(), 128.0 / 255.0);
        let explicit_zero = VulkanStaticDrawRange {
            pipeline_state: LegacyPipelineState {
                alpha_test: true,
                ..LegacyPipelineState::default()
            },
            terrain_base_only: false,
            alpha_test_reference: 0,
            ..base
        };
        assert_eq!(explicit_zero.alpha_test_cutoff(), 0.0);
        let native_default = VulkanStaticDrawRange {
            pipeline_state: LegacyPipelineState {
                alpha_test: true,
                ..LegacyPipelineState::default()
            },
            terrain_base_only: false,
            alpha_test_reference: VulkanStaticDrawRange::NATIVE_DEFAULT_ALPHA_TEST_REFERENCE,
            ..base
        };
        assert_eq!(native_default.alpha_test_cutoff(), 1.0 / 255.0);
    }
}

/// Shared bootstrap progress used to report partial renderer startup evidence.
#[derive(Debug, Default)]
pub struct VulkanSmokeBootstrapProgress {
    flags: AtomicU8,
}

impl VulkanSmokeBootstrapProgress {
    /// Marks the Vulkan loader as available.
    pub fn mark_loader_available(&self) {
        self.set_flag(BOOTSTRAP_LOADER_AVAILABLE);
    }

    /// Marks the Vulkan instance as created.
    pub fn mark_instance_created(&self) {
        self.set_flag(BOOTSTRAP_INSTANCE_CREATED);
    }

    /// Marks the Vulkan surface as created.
    pub fn mark_surface_created(&self) {
        self.set_flag(BOOTSTRAP_SURFACE_CREATED);
    }

    /// Marks a suitable Vulkan device as selected and the logical device as created.
    pub fn mark_logical_device_created(&self) {
        self.set_flag(BOOTSTRAP_DEVICE_SELECTED | BOOTSTRAP_LOGICAL_DEVICE_CREATED);
    }

    /// Marks the Vulkan swapchain as created.
    pub fn mark_swapchain_created(&self) {
        self.set_flag(BOOTSTRAP_SWAPCHAIN_CREATED);
    }

    /// Returns a stable snapshot of the measured bootstrap state.
    #[must_use]
    pub fn snapshot(&self) -> VulkanSmokeBootstrapSnapshot {
        let flags = self.flags.load(Ordering::SeqCst);
        VulkanSmokeBootstrapSnapshot {
            loader_available: flags & BOOTSTRAP_LOADER_AVAILABLE != 0,
            instance_created: flags & BOOTSTRAP_INSTANCE_CREATED != 0,
            surface_created: flags & BOOTSTRAP_SURFACE_CREATED != 0,
            device_selected: flags & BOOTSTRAP_DEVICE_SELECTED != 0,
            logical_device_created: flags & BOOTSTRAP_LOGICAL_DEVICE_CREATED != 0,
            swapchain_created: flags & BOOTSTRAP_SWAPCHAIN_CREATED != 0,
        }
    }

    fn set_flag(&self, flag: u8) {
        self.flags.fetch_or(flag, Ordering::SeqCst);
    }
}

/// Stable snapshot of measured bootstrap progress.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]

pub struct VulkanSmokeBootstrapSnapshot {
    /// Whether the Vulkan loader was resolved.
    pub loader_available: bool,
    /// Whether the Vulkan instance was created.
    pub instance_created: bool,
    /// Whether the Vulkan surface was created.
    pub surface_created: bool,
    /// Whether a suitable Vulkan device was selected.
    pub device_selected: bool,
    /// Whether the logical device was created.
    pub logical_device_created: bool,
    /// Whether the swapchain was created.
    pub swapchain_created: bool,
}

const BOOTSTRAP_LOADER_AVAILABLE: u8 = 1 << 0;
const BOOTSTRAP_INSTANCE_CREATED: u8 = 1 << 1;
const BOOTSTRAP_SURFACE_CREATED: u8 = 1 << 2;
const BOOTSTRAP_DEVICE_SELECTED: u8 = 1 << 3;
const BOOTSTRAP_LOGICAL_DEVICE_CREATED: u8 = 1 << 4;
const BOOTSTRAP_SWAPCHAIN_CREATED: u8 = 1 << 5;

/// Stable smoke renderer bootstrap report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VulkanSmokeRendererReport {
    /// Whether portability enumeration was enabled at instance creation.
    pub portability_enumeration: bool,
    /// Whether the logical device enabled `VK_KHR_portability_subset`.
    pub portability_subset_enabled: bool,
    /// Selected device name.
    pub device_name: String,
    /// Graphics queue-family index.
    pub graphics_queue_family: u32,
    /// Present queue-family index.
    pub present_queue_family: u32,
    /// Enabled logical-device extension count.
    pub enabled_extension_count: u32,
    /// Current swapchain extent.
    pub swapchain_extent: (u32, u32),
    /// Current swapchain image count.
    pub swapchain_image_count: u32,
    /// Current swapchain image format as a raw Vulkan enum value.
    pub swapchain_image_format: i32,
    /// Current swapchain image-usage flags as raw Vulkan bits.
    pub swapchain_image_usage: u32,
    /// Number of submitted swapchain image-to-buffer copy commands.
    pub readback_copy_count: u64,
    /// Byte length of the final completed readback artifact.
    pub readback_byte_count: u64,
    /// Stable FNV-1a hash of the final completed readback artifact.
    pub readback_fnv1a64: u64,
}

/// Measured validation counters from the live smoke loop.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VulkanValidationReport {
    /// Validation warnings observed by the debug messenger.
    pub warning_count: u32,
    /// Validation errors observed by the debug messenger.
    pub error_count: u32,
    /// Stable sorted VUID list.
    pub vuids: Vec<String>,
}

/// CPU-owned copy of a completed swapchain image readback.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VulkanReadbackArtifact {
    /// Vulkan swapchain format as a raw enum value.
    pub format: i32,
    /// Width of each image in pixels.
    pub width: u32,
    /// Height of each image in pixels.
    pub height: u32,
    /// One raw four-byte-per-pixel image from the last successfully submitted
    /// swapchain image at the synchronization point.
    pub bytes: Vec<u8>,
}

/// Final smoke renderer shutdown evidence captured after explicit teardown.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VulkanSmokeShutdownReport {
    /// Stable renderer bootstrap and swapchain report.
    pub renderer_report: VulkanSmokeRendererReport,
    /// Optional raw GPU readback retained after synchronization and before teardown.
    pub readback_artifact: Option<VulkanReadbackArtifact>,
    /// Measured swapchain recreation count for the completed smoke loop.
    pub swapchain_recreate_count: u32,
    /// Final validation snapshot captured before the debug messenger is destroyed.
    pub validation: VulkanValidationReport,
}

/// Result of one rendered smoke frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VulkanSmokeFrameOutcome {
    /// A frame was submitted and presented.
    Presented,
    /// Rendering was skipped because the swapchain had to be recreated.
    Recreated,
    /// Rendering was skipped because the drawable extent is zero.
    ZeroExtent,
}

/// Live smoke renderer error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VulkanSmokeRendererError {
    /// Instance bootstrap failed.
    Instance(VulkanInstanceError),
    /// Surface bootstrap failed.
    Surface(VulkanSurfaceError),
    /// Logical-device bootstrap failed.
    LogicalDevice(VulkanLogicalDeviceError),
    /// Swapchain bootstrap failed.
    Swapchain(VulkanSwapchainProbeError),
    /// Checked-in shader validation failed.
    Shader(VulkanShaderError),
    /// Vulkan operation failed.
    VulkanOperation {
        /// Operation context.
        context: &'static str,
        /// Raw Vulkan result code.
        result: vk::Result,
    },
    /// No suitable memory type exists for the required properties.
    MissingMemoryType {
        /// Operation context.
        context: &'static str,
    },
    /// The submitted static geometry cannot be represented by this path.
    InvalidStaticMesh {
        /// Validation failure detail.
        context: &'static str,
    },
    /// The static camera matrix contains a non-finite element.
    InvalidStaticCamera {
        /// Validation failure detail.
        context: &'static str,
    },
    /// Submitted dynamic point-light data exceeds the Vulkan storage range or contains invalid values.
    InvalidPointLights {
        /// Validation failure detail.
        context: &'static str,
    },
    /// The submitted static texture cannot be represented by this path.
    InvalidStaticTexture {
        /// Validation failure detail.
        context: &'static str,
    },
    /// Internal smoke renderer state was unexpectedly absent.
    InvariantViolation {
        /// Missing state context.
        context: &'static str,
    },
}

impl std::fmt::Display for VulkanSmokeRendererError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Instance(error) => write!(f, "{error}"),
            Self::Surface(error) => write!(f, "{error}"),
            Self::LogicalDevice(error) => write!(f, "{error}"),
            Self::Swapchain(error) => write!(f, "{error}"),
            Self::Shader(error) => write!(f, "{error}"),
            Self::VulkanOperation { context, result } => {
                write!(f, "{context}: {result:?}")
            }
            Self::MissingMemoryType { context } => {
                write!(f, "{context}: no compatible Vulkan memory type")
            }
            Self::InvalidStaticMesh { context } => write!(f, "invalid static mesh: {context}"),
            Self::InvalidStaticCamera { context } => {
                write!(f, "invalid static camera: {context}")
            }
            Self::InvalidPointLights { context } => write!(f, "invalid point lights: {context}"),
            Self::InvalidStaticTexture { context } => {
                write!(f, "invalid static texture: {context}")
            }
            Self::InvariantViolation { context } => {
                write!(f, "renderer invariant violated: {context}")
            }
        }
    }
}

impl std::error::Error for VulkanSmokeRendererError {}

/// Live Stage 0 Vulkan triangle renderer used by the smoke app.
pub struct VulkanSmokeRenderer {
    pub(super) instance: Option<VulkanInstanceProbe>,
    pub(super) validation: Option<VulkanValidationMessenger>,
    pub(super) surface: Option<VulkanSurfaceProbe>,
    pub(super) device: Option<VulkanLogicalDeviceProbe>,
    pub(super) swapchain: Option<VulkanSwapchainProbe>,
    pub(super) command_pool: vk::CommandPool,
    pub(super) swapchain_resources: Option<VulkanSwapchainResources>,
    pub(super) vertex_buffer: Option<VulkanAllocatedBuffer>,
    pub(super) index_buffer: Option<VulkanAllocatedBuffer>,
    /// Positional material slots mapped to the owned image list. Fallback
    /// stages may share one image without coupling independently animated
    /// material textures.
    pub(super) texture_slot_indices: Vec<usize>,
    pub(super) textures: Vec<VulkanAllocatedImage>,
    pub(super) vertex_count: usize,
    /// Retained index allocation length. The upload path grows this only when
    /// a frame actually needs more shared indices and never shrinks it.
    pub(super) index_capacity: usize,
    pub(super) draw_range_capacities: Vec<u32>,
    pub(super) draw_ranges: Vec<VulkanStaticDrawRange>,
    pub(super) draw_texture_indices: Vec<usize>,
    /// Stable range handles used when recording the native pass order.
    pub(super) draw_order: Vec<usize>,
    /// Native camera-sort values returned by the draw-item camera callback.
    pub(super) draw_range_sort_keys: Vec<Option<f32>>,
    /// Whether the stable order must be rebuilt before the next command
    /// buffer.  Range setters are called in batches by the game loop.
    pub(super) draw_order_dirty: bool,
    /// World queue classification for each stable range handle. `None` keeps
    /// a fixed screen/sky/weather pass; `Some(false)` is world opaque and
    /// `Some(true)` is world transparent.
    pub(super) draw_range_world_queue: Vec<Option<bool>>,
    /// Optional diffuse-alpha override for each draw range, independent of
    /// the shared material record.
    pub(super) draw_range_diffuse_alpha_overrides: Vec<Option<f32>>,
    /// Fixed sky background ranges placed between world opaque and transparent.
    pub(super) world_transparent_background_ranges: Vec<usize>,
    /// Pipeline state before transparent queue depth policy is applied.
    pub(super) draw_range_base_pipeline_states: Vec<LegacyPipelineState>,
    pub(super) material_alphas: Vec<[f32; 2]>,
    pub(super) material_lighting: Vec<[[f32; 3]; 4]>,
    pub(super) material_uv_transforms: Vec<[[f32; 4]; 4]>,
    /// Base and overlay specular records uploaded to the dynamic SSBO.
    pub(super) material_specular: Vec<[[f32; 4]; 2]>,
    /// Original selector for each positional material record.
    pub(super) material_selectors: Vec<u16>,
    /// Whether the host-side specular records need a fence-safe GPU upload.
    pub(super) material_specular_dirty: bool,
    /// Encoded material mode pushed at offset 124: low bits select the
    /// combiner, bit 4 selects sky far-depth, and bit 8 selects screen/TL.
    pub(super) material_unlit: Vec<f32>,
    pub(super) readback_enabled: bool,
    pub(super) camera: VulkanStaticCamera,
    pub(super) frame_uniforms: VulkanFrameUniforms,
    /// Dynamically sized point-light records submitted by the client.
    pub(super) point_lights: Vec<VulkanPointLight>,
    /// Whether `set_point_lights` has replaced legacy `frame_uniforms.point_light`.
    pub(super) point_lights_explicit: bool,
    pub(super) frame_sync: Vec<VulkanFrameSync>,
    pub(super) images_in_flight: Vec<vk::Fence>,
    pub(super) current_frame: usize,
    /// Last swapchain image whose color attachment was copied after a
    /// successful graphics submission. Reset whenever swapchain resources are
    /// replaced so a teardown never reads a buffer from an old swapchain.
    pub(super) last_readback_image_index: Option<usize>,
    pub(super) depth_request: fparkan_platform::DepthStencilSupport,
    pub(super) pending_extent: Option<(u32, u32)>,
    pub(super) swapchain_recreate_count: u32,
    pub(super) report: VulkanSmokeRendererReport,
}
