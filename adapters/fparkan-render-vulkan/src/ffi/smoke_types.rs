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
use crate::shader_manifest::VulkanShaderManifestError;

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
    /// Texture coordinate consumed by the static material bridge.
    pub uv: [f32; 2],
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
        let projection = projection.try_direct3d7_projection_row_major()?;
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
    pub indices: Vec<u16>,
    /// Source-preserving triangle draw ranges in [`Self::indices`].
    pub draw_ranges: Vec<VulkanStaticDrawRange>,
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
    /// Backend-neutral fixed-function state for this source range.
    pub pipeline_state: LegacyPipelineState,
    /// Alpha-test reference in legacy 0..=255 units; dynamic material data.
    pub alpha_test_reference: u8,
}

impl VulkanStaticDrawRange {
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VulkanStaticMaterial {
    /// Positional material selector used by one or more source batches.
    pub material_index: u16,
    /// Decoded RGBA8 diffuse texture for this selector.
    pub texture: VulkanStaticTexture,
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
            Some(_) => Ok(()),
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
                    uv: [0.5, 0.0],
                },
                VulkanStaticVertex {
                    position: [0.55, 0.55, 0.0],
                    color: [0.2, 1.0, 0.2],
                    uv: [1.0, 1.0],
                },
                VulkanStaticVertex {
                    position: [-0.55, 0.55, 0.0],
                    color: [0.2, 0.4, 1.0],
                    uv: [0.0, 1.0],
                },
            ],
            indices: vec![0, 1, 2],
            draw_ranges: vec![VulkanStaticDrawRange {
                first_index: 0,
                index_count: 3,
                material_index: 0,
                pipeline_state: LegacyPipelineState::default(),
                alpha_test_reference: 0,
            }],
        }
    }

    pub(super) fn validate(&self) -> Result<(), &'static str> {
        if self.vertices.is_empty() {
            return Err("static mesh has no vertices");
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
        if self
            .indices
            .iter()
            .any(|&index| usize::from(index) >= self.vertices.len())
        {
            return Err("static mesh index exceeds vertex count");
        }
        Ok(())
    }
}

#[cfg(test)]
mod static_mesh_tests {
    use super::*;

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
        assert_eq!(camera.clip_from_world[6], 700.0_f32 / 699.5);
        assert_eq!(camera.clip_from_world[7], 0.65_f32.sin());
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
                pipeline_state: LegacyPipelineState::default(),
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
                pipeline_state: LegacyPipelineState::default(),
                alpha_test_reference: 0,
            },
            VulkanStaticDrawRange {
                first_index: 3,
                index_count: 3,
                material_index: 2,
                pipeline_state: LegacyPipelineState::default(),
                alpha_test_reference: 0,
            },
        ];
        let texture = || VulkanStaticTexture {
            width: 1,
            height: 1,
            rgba8: vec![255; 4],
        };
        let materials = [
            VulkanStaticMaterial {
                material_index: 2,
                texture: texture(),
            },
            VulkanStaticMaterial {
                material_index: 7,
                texture: texture(),
            },
        ];

        assert_eq!(
            resolve_draw_texture_indices(&ranges, &materials),
            Ok(vec![1, 0])
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
            alpha_test_reference: 200,
            ..base
        };
        let enabled = VulkanStaticDrawRange {
            pipeline_state: LegacyPipelineState {
                alpha_test: true,
                ..LegacyPipelineState::default()
            },
            alpha_test_reference: 128,
            ..base
        };
        assert_eq!(disabled.alpha_test_cutoff(), 0.0);
        assert_eq!(enabled.alpha_test_cutoff(), 128.0 / 255.0);
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
#[allow(clippy::struct_excessive_bools)]
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
    /// Checked-in shader manifest hash used by the renderer.
    pub shader_manifest_hash: String,
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

/// CPU-owned copy of the final completed swapchain readback buffers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VulkanReadbackArtifact {
    /// Vulkan swapchain format as a raw enum value.
    pub format: i32,
    /// Width of each image in pixels.
    pub width: u32,
    /// Height of each image in pixels.
    pub height: u32,
    /// Concatenated raw four-byte-per-pixel images in swapchain order.
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
    /// Shader manifest validation failed.
    ShaderManifest(VulkanShaderManifestError),
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
            Self::ShaderManifest(error) => write!(f, "{error}"),
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
    pub(super) textures: Vec<VulkanAllocatedImage>,
    pub(super) draw_ranges: Vec<VulkanStaticDrawRange>,
    pub(super) draw_texture_indices: Vec<usize>,
    pub(super) camera: VulkanStaticCamera,
    pub(super) frame_sync: Vec<VulkanFrameSync>,
    pub(super) images_in_flight: Vec<vk::Fence>,
    pub(super) current_frame: usize,
    pub(super) depth_request: fparkan_platform::DepthStencilSupport,
    pub(super) pending_extent: Option<(u32, u32)>,
    pub(super) swapchain_recreate_count: u32,
    pub(super) report: VulkanSmokeRendererReport,
}
