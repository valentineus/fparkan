#![forbid(unsafe_code)]

//! `FParkan` render-planning composition root.

use fparkan_animation::{AnimationTime, NodePoseBuffer};
use fparkan_assets::{
    load_material_name_texture_rgba8_and_phase_with_document_from_root,
    load_resource_entry_bytes_from_root,
    load_standalone_wear_named_material_textures_rgba8_and_phases_with_documents_from_root,
    page_uv_transform, sample_control_node_times, sample_material_phase, ControlInstanceState,
    ControlSamplingError, Mat0Document, MaterialPhase, MaterialPhaseSample,
    PreparedComponentControl, PreparedControlGateProvider, PreparedControlNodeBinding,
    PreparedMaterialPhase, PreparedTexture, PreparedTextureUsage, PreparedVisual, RgbaImage,
    StandaloneWearMaterialLoader,
};
use fparkan_fx::atmosphere::TypedAtmosphere;
use fparkan_fx::environment::{
    decode_env_lightning_fxid, Camera as FxCamera, EnvironmentFrame, EnvironmentPrimitive,
    EnvironmentSystem, PrecipitationKind, ScreenBillboard,
};
use fparkan_fx::shadow::{
    ShadowCamera, ShadowCaster, ShadowFrame, ShadowLight, ShadowLodSettings, ShadowPageCache,
    ShadowScene, ShadowSphere, ShadowTriangle, SHADOW_PRIMARY_DIRECTIONAL_FLAG,
};
use fparkan_fx::sky::{
    sprite_half_size_pixels, SkyFrame, SkyMaterials, SkyMesh, SkyPassKind, SkySpriteFrame,
    SkySystem, SunVisibility, SKY_FLARE_ROWS,
};
use fparkan_msh::{
    node38_fallback_hierarchy, node38_sampled_hierarchy, node38_sampled_hierarchy_at_times,
    ModelAsset,
};
use fparkan_path::ResourceName;
use fparkan_platform_winit::{window_native_handles, WinitWindow, WinitWindowPlan};
use fparkan_prototype::{unit_component_tree, UnitComponentTreeNode, PROTOTYPE_TYPE_EXTO};
use fparkan_render::{
    LegacyBlendMode, LegacyCullMode, LegacyD3d7Projection, LegacyDepthMode,
    LegacyIron3dEulerTransform, LegacyPipelineState, RawCameraTransform,
};
use fparkan_render_vulkan::{
    node38_pose, node38_pose_from_hierarchy, node38_pose_relative_to_root,
    node38_pose_relative_to_root_from_hierarchy, project_land_msh_to_static_mesh_in_world_space,
    project_msh_to_static_mesh_in_world_space_with_node_fallback_poses,
    project_msh_to_static_mesh_in_world_space_with_node_fallback_poses_and_mount,
    project_msh_to_static_mesh_in_world_space_with_node_pose_buffer,
    project_msh_to_static_mesh_in_world_space_with_node_pose_buffer_and_mount,
    project_msh_to_static_mesh_in_world_space_with_node_sampled_poses,
    project_msh_to_static_mesh_in_world_space_with_node_sampled_poses_and_mount,
    VulkanDynamicDrawRange, VulkanNodePose, VulkanPointLight, VulkanSmokeFrameOutcome,
    VulkanSmokeRenderer, VulkanSmokeRendererCreateInfo, VulkanStaticCamera, VulkanStaticDrawRange,
    VulkanStaticMaterial, VulkanStaticMesh, VulkanStaticTexture, VulkanStaticTextureMip,
    VulkanStaticVertex,
};
use fparkan_runtime::{
    create, load_mission_static_preview, load_mission_static_preview_roots, loaded_mission_assets,
    loaded_mission_object_drafts, loaded_terrain, EngineServices, MissionAssets, MissionMode,
    MissionObjectDraft, MissionRequest,
};
use fparkan_terrain::{
    FullSurfaceMask, SurfaceQuery, TerrainLod, TerrainMaterialLayers, TerrainWorld,
};
use fparkan_vfs::DirectoryVfs;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize as WinitPhysicalSize;
use winit::event::{DeviceEvent, DeviceId, ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Window, WindowId};

mod audio;

fn main() {
    let raw_args = std::env::args().skip(1).collect::<Vec<_>>();
    let code = match run(&raw_args) {
        Ok(output) => {
            println!("{output}");
            0
        }
        Err(err) => {
            eprintln!("{err}");
            2
        }
    };
    std::process::exit(code);
}

fn run(args: &[String]) -> Result<String, String> {
    let args = Args::parse(args)?;
    let services = EngineServices::new(Arc::new(DirectoryVfs::new(&args.root)));
    let mut engine = create(services).map_err(|err| err.to_string())?;
    let mission_assets_started = Instant::now();
    let loaded = load_requested_mission(&mut engine, &args)?;
    let mission_assets_duration = mission_assets_started.elapsed();

    let mission_assets = loaded_mission_assets(&engine)
        .ok_or_else(|| "mission assets are unavailable after loading".to_string())?;
    let terrain = loaded_terrain(&engine)
        .ok_or_else(|| "mission terrain is unavailable after loading".to_string())?;
    let roots = loaded_mission_object_drafts(&engine)
        .map(|drafts| &drafts[..args.preview_roots.get().min(drafts.len())])
        .filter(|roots| !roots.is_empty())
        .ok_or_else(|| {
            "selected mission object drafts are unavailable after loading".to_string()
        })?;
    let camera = args
        .legacy_camera_capture
        .as_deref()
        .map(load_legacy_camera_capture)
        .transpose()?;
    let atmosphere_seconds = selected_atmosphere_seconds(args.atmosphere_seconds, camera.as_ref());
    let hold_schedule_phase = camera.is_some() && atmosphere_seconds.is_some();
    let terrain_materials_duration_started = Instant::now();
    let preview = static_preview_mesh_and_materials(
        mission_assets,
        terrain,
        roots,
        camera,
        args.static_animation_frame,
        args.static_material_phase,
        &args.root,
        &loaded.land_msh_path,
    )?;
    let terrain_materials_duration = terrain_materials_duration_started.elapsed();
    // Atmosphere files are mission-local assets.  Resolving them from the
    // entire install tree would silently attach the first unrelated mission's
    // sky to the selected map (there are many `sky.ske` files in the campaign
    // tree).
    let sky_started = Instant::now();
    let environment = load_environment(
        &args.root,
        &args.mission,
        atmosphere_seconds,
        hold_schedule_phase,
    )?;
    let sky_duration = sky_started.elapsed();
    let audio = match audio::GameAudio::new(Arc::new(DirectoryVfs::new(&args.root)), &args.mission)
    {
        Ok(audio) => Some(audio),
        Err(audio::AudioError::DeviceUnavailable { message }) => {
            eprintln!("audio output unavailable; continuing without sound: {message}");
            None
        }
        Err(error) => return Err(format!("load mission audio: {error}")),
    };
    run_static_vulkan_mode(
        preview,
        environment,
        audio,
        args.frames,
        args.control_clock_ms,
        &args.mission,
        loaded.object_count,
        args.readback_out.as_deref(),
        args.validation,
        StartupTimings {
            mission_assets: mission_assets_duration,
            terrain_materials: terrain_materials_duration,
            sky: sky_duration,
            gpu_initialization: Duration::ZERO,
        },
    )
}

fn load_requested_mission(
    engine: &mut fparkan_runtime::Engine,
    args: &Args,
) -> Result<fparkan_runtime::LoadedMission, String> {
    let request = MissionRequest {
        key: args.mission.clone(),
        mode: MissionMode::SingleMission,
    };
    if args.preview_roots.get() == 1 {
        load_mission_static_preview(engine, request)
    } else {
        load_mission_static_preview_roots(engine, request, args.preview_roots)
    }
    .map_err(|err| err.to_string())
}

/// Camera state used by the interactive world-space preview.
#[derive(Clone, Copy, Debug, PartialEq)]
struct FreeFlightCamera {
    position: [f32; 3],
    yaw: f32,
    pitch: f32,
    horizontal_fov: f32,
    near_plane: f32,
    far_plane: f32,
    move_speed: f32,
    ground_clearance: f32,
}

impl FreeFlightCamera {
    fn from_mesh(mesh: &VulkanStaticMesh) -> Result<Self, String> {
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        for vertex in &mesh.vertices {
            for axis in 0..3 {
                let value = vertex.position[axis];
                if !value.is_finite() {
                    return Err("world preview contains a non-finite vertex".to_string());
                }
                min[axis] = min[axis].min(value);
                max[axis] = max[axis].max(value);
            }
        }
        if !min.iter().chain(max.iter()).all(|value| value.is_finite()) {
            return Err("world preview has no finite bounds".to_string());
        }
        Self::from_bounds(min, max)
    }

    fn from_bounds(min: [f32; 3], max: [f32; 3]) -> Result<Self, String> {
        if !min.iter().chain(max.iter()).all(|value| value.is_finite()) {
            return Err("free-flight bounds must be finite".to_string());
        }
        let center = [
            (min[0] + max[0]) * 0.5,
            (min[1] + max[1]) * 0.5,
            (min[2] + max[2]) * 0.5,
        ];
        let radius = (0..3)
            .map(|axis| (max[axis] - min[axis]).abs())
            .fold(1.0_f32, f32::max);
        if !radius.is_finite() || radius <= f32::EPSILON {
            return Err("free-flight bounds are degenerate".to_string());
        }
        let position = [
            center[0],
            center[1] - radius * 2.5,
            center[2] + radius * 1.25,
        ];
        let direction = sub3(center, position);
        let distance = length3(direction)
            .ok_or_else(|| "free-flight initial camera has no direction to the map".to_string())?;
        let yaw = direction[1].atan2(direction[0]);
        let pitch = (direction[2] / distance).clamp(-1.0, 1.0).asin();
        Ok(Self {
            position,
            yaw,
            pitch,
            horizontal_fov: 1.3,
            near_plane: 0.5,
            far_plane: 700.0,
            // Keep the default traversal speed comfortable on a full mission
            // map; Shift/Ctrl still provide the coarse speed controls.
            move_speed: (radius * 0.03).clamp(4.0, 40.0),
            ground_clearance: 0.0,
        })
    }

    fn from_mesh_and_terrain(
        mesh: &VulkanStaticMesh,
        terrain: &TerrainWorld,
    ) -> Result<Self, String> {
        let mut camera = Self::from_mesh(mesh)?;
        let positions = terrain
            .source_positions()
            .ok_or_else(|| "free-flight terrain has no source positions".to_string())?;
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        for position in positions {
            if !position.iter().all(|value| value.is_finite()) {
                return Err("free-flight terrain contains a non-finite position".to_string());
            }
            for axis in 0..3 {
                min[axis] = min[axis].min(position[axis]);
                max[axis] = max[axis].max(position[axis]);
            }
        }
        if !min.iter().chain(max.iter()).all(|value| value.is_finite()) {
            return Err("free-flight terrain has no finite bounds".to_string());
        }
        let extent = (max[0] - min[0]).max(max[1] - min[1]);
        if !extent.is_finite() || extent <= f32::EPSILON {
            return Err("free-flight terrain has degenerate XY bounds".to_string());
        }
        let center = [(min[0] + max[0]) * 0.5, (min[1] + max[1]) * 0.5];
        let offset = extent * 0.18;
        let requested_spawn = [
            (center[0] - offset).clamp(min[0], max[0]),
            (center[1] - offset).clamp(min[1], max[1]),
        ];
        let sample_offsets = [
            [0.0, 0.0],
            [0.18, 0.18],
            [-0.18, 0.18],
            [0.18, -0.18],
            [-0.18, -0.18],
            [0.0, 0.18],
            [0.18, 0.0],
            [-0.18, 0.0],
            [0.0, -0.18],
        ];
        let height_world = |xy: [f32; 2]| {
            terrain
                .height_at(xy)
                .ok()
                .flatten()
                .filter(|height| height.is_finite())
        };
        let (spawn_xy, spawn_ground) = sample_offsets
            .into_iter()
            .map(|sample| {
                [
                    (requested_spawn[0] + sample[0] * extent).clamp(min[0], max[0]),
                    (requested_spawn[1] + sample[1] * extent).clamp(min[1], max[1]),
                ]
            })
            .find_map(|xy| height_world(xy).map(|height| (xy, height)))
            .ok_or_else(|| "free-flight terrain has no queryable spawn surface".to_string())?;
        let target_ground = height_world(center).unwrap_or(spawn_ground);
        let clearance = (extent * 0.005).clamp(2.0, 8.0);
        camera.ground_clearance = clearance;
        camera.position = [spawn_xy[0], spawn_xy[1], spawn_ground + clearance];
        let target = [center[0], center[1], target_ground + 1.5];
        let direction = sub3(target, camera.position);
        let distance = length3(direction)
            .ok_or_else(|| "free-flight terrain spawn has no view direction".to_string())?;
        camera.yaw = direction[1].atan2(direction[0]);
        camera.pitch = (direction[2] / distance).clamp(-1.0, 1.0).asin();
        Ok(camera)
    }

    fn keep_above_terrain(&mut self, terrain: &TerrainWorld) -> Result<(), String> {
        if !self.ground_clearance.is_finite() || self.ground_clearance < 0.0 {
            return Err("free-flight ground clearance must be finite and nonnegative".to_string());
        }
        if let Some([min, max]) = terrain.surface_bounds() {
            for axis in 0..2 {
                self.position[axis] = self.position[axis].clamp(min[axis], max[axis]);
            }
        }
        let Some(ground_height) = terrain
            .height_at([self.position[0], self.position[1]])
            .map_err(|error| format!("free-flight terrain height query: {error}"))?
        else {
            return Ok(());
        };
        if !ground_height.is_finite() {
            return Err("free-flight terrain height is not finite".to_string());
        }
        let minimum_z = ground_height + self.ground_clearance;
        if !minimum_z.is_finite() {
            return Err("free-flight terrain floor is not finite".to_string());
        }
        self.position[2] = self.position[2].max(minimum_z);
        Ok(())
    }

    fn forward(self) -> [f32; 3] {
        let (sin_yaw, cos_yaw) = self.yaw.sin_cos();
        let (sin_pitch, cos_pitch) = self.pitch.sin_cos();
        [cos_pitch * cos_yaw, cos_pitch * sin_yaw, sin_pitch]
    }

    fn right(self) -> [f32; 3] {
        normalize3(cross3(self.forward(), [0.0, 0.0, 1.0])).unwrap_or([1.0, 0.0, 0.0])
    }

    fn up(self) -> [f32; 3] {
        normalize3(cross3(self.right(), self.forward())).unwrap_or([0.0, 0.0, 1.0])
    }

    fn vertical_fov(self, aspect: f32) -> f32 {
        let aspect = if aspect.is_finite() && aspect > f32::EPSILON {
            aspect
        } else {
            16.0 / 9.0
        };
        2.0 * ((self.horizontal_fov * 0.5).tan() / aspect).atan()
    }

    fn look_delta(&mut self, dx: f64, dy: f64) {
        const SENSITIVITY: f32 = 0.0025;
        let dx = dx as f32;
        let dy = dy as f32;
        if dx.is_finite() && dy.is_finite() {
            // Moving the mouse right turns the view to the right.
            self.yaw -= dx * SENSITIVITY;
            self.pitch = (self.pitch - dy * SENSITIVITY).clamp(-1.5, 1.5);
        }
    }

    fn advance(&mut self, keys: &HashSet<KeyCode>, seconds: f32) {
        if !seconds.is_finite() || seconds <= 0.0 {
            return;
        }
        let mut movement = [0.0; 3];
        let forward = self.forward();
        let right = self.right();
        if keys.contains(&KeyCode::KeyW) {
            add3_in_place(&mut movement, forward);
        }
        if keys.contains(&KeyCode::KeyS) {
            add3_in_place(&mut movement, scale3(forward, -1.0));
        }
        if keys.contains(&KeyCode::KeyD) {
            add3_in_place(&mut movement, right);
        }
        if keys.contains(&KeyCode::KeyA) {
            add3_in_place(&mut movement, scale3(right, -1.0));
        }
        if keys.contains(&KeyCode::KeyE) {
            add3_in_place(&mut movement, [0.0, 0.0, 1.0]);
        }
        if keys.contains(&KeyCode::KeyQ) {
            add3_in_place(&mut movement, [0.0, 0.0, -1.0]);
        }
        let Some(direction) = normalize3(movement) else {
            return;
        };
        let mut speed = self.move_speed;
        if keys.contains(&KeyCode::ShiftLeft) || keys.contains(&KeyCode::ShiftRight) {
            speed *= 4.0;
        }
        if keys.contains(&KeyCode::ControlLeft) || keys.contains(&KeyCode::ControlRight) {
            speed *= 0.25;
        }
        self.position = add3(self.position, scale3(direction, speed * seconds));
    }

    fn vulkan_camera(self, aspect: f32) -> VulkanStaticCamera {
        VulkanStaticCamera {
            clip_from_world: clip_from_world_for_view(
                self.position,
                self.forward(),
                self.right(),
                self.up(),
                self.vertical_fov(aspect),
                self.near_plane,
                self.far_plane,
                aspect,
            ),
        }
    }

    fn preview_frame(self, aspect: f32) -> PreviewCameraFrame {
        let camera = self.vulkan_camera(aspect);
        PreviewCameraFrame {
            position: self.position,
            forward: self.forward(),
            right: self.right(),
            up: self.up(),
            vertical_fov: self.vertical_fov(aspect),
            near_plane: self.near_plane,
            far_plane: self.far_plane,
            clip_from_world: camera.clip_from_world,
        }
    }
}

fn preview_aspect(aspect: f32) -> f32 {
    if aspect.is_finite() && aspect > f32::EPSILON {
        aspect
    } else {
        16.0 / 9.0
    }
}

fn clip_from_world_for_view(
    position: [f32; 3],
    forward: [f32; 3],
    right: [f32; 3],
    up: [f32; 3],
    vertical_fov: f32,
    near_plane: f32,
    far_plane: f32,
    aspect: f32,
) -> [f32; 16] {
    let aspect = preview_aspect(aspect);
    // VulkanStaticCamera carries row-major D3D-style data. GLSL interprets
    // those bytes as a column-major mat4, so shader multiplication is the
    // transpose of this row-vector transform: world * V * P. The camera
    // basis is right-handed with +Z-up world coordinates and +Z depth.
    let view = [
        right[0],
        up[0],
        forward[0],
        0.0,
        right[1],
        up[1],
        forward[1],
        0.0,
        right[2],
        up[2],
        forward[2],
        0.0,
        -dot3(right, position),
        -dot3(up, position),
        -dot3(forward, position),
        1.0,
    ];
    let focal = (vertical_fov * 0.5).tan().recip();
    let near_plane = near_plane.max(0.001);
    let far_plane = far_plane.max(near_plane + 0.001);
    let projection = [
        focal / aspect,
        0.0,
        0.0,
        0.0,
        0.0,
        -focal,
        0.0,
        0.0,
        0.0,
        0.0,
        far_plane / (far_plane - near_plane),
        1.0,
        0.0,
        0.0,
        -(near_plane * far_plane) / (far_plane - near_plane),
        0.0,
    ];
    multiply_row_major(view, projection)
}

/// Immutable view data shared by the environment renderers. The optional
/// `FreeFlightCamera` remains only the input controller; captured views keep
/// their original basis and clip transform, including roll.
#[derive(Clone, Copy, Debug, PartialEq)]
struct PreviewCameraFrame {
    position: [f32; 3],
    forward: [f32; 3],
    right: [f32; 3],
    up: [f32; 3],
    vertical_fov: f32,
    near_plane: f32,
    far_plane: f32,
    clip_from_world: [f32; 16],
}

impl PreviewCameraFrame {
    fn from_legacy_d3d7(
        transform: RawCameraTransform,
        projection: LegacyD3d7Projection,
    ) -> Option<Self> {
        let camera = VulkanStaticCamera::from_legacy_d3d7(transform, projection)?;
        let view = transform.try_direct3d7_view_row_major()?;
        let width = projection.viewport[2].checked_sub(projection.viewport[0])?;
        let height = projection.viewport[3].checked_sub(projection.viewport[1])?;
        if width <= 0 || height <= 0 {
            return None;
        }
        let aspect = width as f32 / height as f32;
        let vertical_fov = 2.0 * ((projection.field_of_view_radians * 0.5).tan() / aspect).atan();
        let frame = Self {
            position: transform.translation(),
            // The camera axes are the columns of Ngi32's D3D7 view matrix.
            // Keeping them directly preserves native roll and avoids
            // reconstructing an incomplete yaw/pitch camera.
            right: [view[0], view[4], view[8]],
            up: [view[1], view[5], view[9]],
            forward: [view[2], view[6], view[10]],
            vertical_fov,
            near_plane: projection.near_plane,
            far_plane: projection.far_plane,
            clip_from_world: camera.clip_from_world,
        };
        (frame.position.iter().all(|value| value.is_finite())
            && frame.forward.iter().all(|value| value.is_finite())
            && frame.right.iter().all(|value| value.is_finite())
            && frame.up.iter().all(|value| value.is_finite())
            && frame.vertical_fov.is_finite()
            && frame.vertical_fov > 0.0
            && frame.vertical_fov < std::f32::consts::PI)
            .then_some(frame)
    }

    fn vulkan_camera(self) -> VulkanStaticCamera {
        VulkanStaticCamera {
            clip_from_world: self.clip_from_world,
        }
    }

    fn with_aspect(self, previous_aspect: f32, aspect: f32) -> Self {
        let previous_aspect = preview_aspect(previous_aspect);
        let aspect = preview_aspect(aspect);
        let horizontal_half_fov_tangent = (self.vertical_fov * 0.5).tan() * previous_aspect;
        let vertical_fov = 2.0 * (horizontal_half_fov_tangent / aspect).atan();
        let clip_from_world = clip_from_world_for_view(
            self.position,
            self.forward,
            self.right,
            self.up,
            vertical_fov,
            self.near_plane,
            self.far_plane,
            aspect,
        );
        Self {
            vertical_fov,
            clip_from_world,
            ..self
        }
    }

    fn forward(self) -> [f32; 3] {
        self.forward
    }

    fn right(self) -> [f32; 3] {
        self.right
    }

    fn up(self) -> [f32; 3] {
        self.up
    }

    fn native_sky_heading(self) -> f32 {
        // Terrain+0x4F9C0 uses atan2(forward.x, forward.y); CSky+0x481C5 adds pi later.
        self.forward[0].atan2(self.forward[1])
    }
}

const ENVIRONMENT_SPRITE_ROWS: [usize; 2] = [3, 4];
const ENVIRONMENT_FLARE_SLOTS: usize = 12;
const ENVIRONMENT_PARTICLE_SLOTS: usize = 1_000;

#[derive(Clone, Copy, Debug)]
struct EnvironmentGpuRange {
    range_index: usize,
    vertex_start: usize,
    vertex_capacity: usize,
    material_index: usize,
}

#[derive(Clone, Debug)]
struct EnvironmentMaterialAsset {
    texture: VulkanStaticTexture,
    phase: PreparedMaterialPhase,
    document: Mat0Document,
    phase_textures: Vec<VulkanStaticTexture>,
    phases: Vec<PreparedMaterialPhase>,
}

/// One source MAT0 document retained for frame-time sampling.  The renderer
/// receives the selected coefficients through its dynamic material state;
/// keeping this binding beside the prepared scene avoids rebuilding assets or
/// reopening archives on every redraw.
#[derive(Clone, Debug)]
struct MaterialPhaseBinding {
    document: Mat0Document,
    animation_block_index: usize,
    wear_row_start_ms: u32,
    random_state: u32,
    frozen_phase_index: Option<usize>,
    phase_uv_transforms: Vec<[f32; 4]>,
}

/// Texture phases for the material stages that do not select the draw
/// variant.  Terrain MAT0 rows are independent: Land1 base, Land2 detail,
/// Land1 overlay, and Land2 overlay-detail can each have their own animation
/// timeline.  Keeping these handles beside the binding lets the renderer
/// update the three descriptor images in place without materialising every
/// Cartesian product of phase indices.
#[derive(Clone, Debug)]
struct MaterialStageTextures {
    detail: Option<Vec<VulkanStaticTexture>>,
    overlay: Option<Vec<VulkanStaticTexture>>,
    overlay_detail: Option<Vec<VulkanStaticTexture>>,
    applied_material_phases: HashMap<usize, [usize; 3]>,
}

impl MaterialPhaseBinding {
    fn sample(
        &mut self,
        material_index: usize,
        clock_ms: u32,
    ) -> Result<MaterialPhaseSample, String> {
        if let Some(phase_index) = self.frozen_phase_index {
            let phase_index = phase_index.min(self.document.phases.len().saturating_sub(1));
            let phase = self.document.phases.get(phase_index).ok_or_else(|| {
                format!(
                    "sample MAT0 material {material_index} at clock {clock_ms}ms: no material phases"
                )
            })?;
            return Ok(MaterialPhaseSample {
                phase_index,
                coefficients: phase.coefficients(),
                texture_raw: phase.texture_raw,
            });
        }

        // GetPhase's random mode asks the CRT generator for one fresh value
        // per query.  Animated loop/ping-pong/clamp modes do not consume the
        // RNG and receive the sampler's ignored value.
        let block = self
            .document
            .animation_blocks
            .get(self.animation_block_index)
            .or_else(|| self.document.animation_blocks.first());
        let random_value = if block.is_some_and(|block| block.header_raw & 7 == 3) {
            self.random_state = self
                .random_state
                .wrapping_mul(214_013)
                .wrapping_add(2_531_011);
            (self.random_state >> 16) & 0x7fff
        } else {
            0
        };
        sample_material_phase(
            &self.document,
            self.animation_block_index,
            clock_ms,
            self.wear_row_start_ms,
            random_value,
        )
        .map_err(|error| {
            format!(
                "sample MAT0 material {} at clock {clock_ms}ms: {error}",
                material_index
            )
        })
    }

    fn page_uv_transform(&self, phase_index: usize) -> [f32; 4] {
        self.phase_uv_transforms
            .get(phase_index)
            .copied()
            .or_else(|| self.phase_uv_transforms.first().copied())
            .unwrap_or([0.0, 0.0, 1.0, 1.0])
    }

    fn checked_page_uv_transform(&self, phase_index: usize) -> Result<[f32; 4], String> {
        self.phase_uv_transforms
            .get(phase_index)
            .copied()
            .ok_or_else(|| {
                format!(
                    "MAT0 Page transform is missing phase {phase_index} of {}",
                    self.phase_uv_transforms.len()
                )
            })
    }
}

#[derive(Clone, Debug)]
struct MaterialVariantSet {
    phase_material_indices: Vec<usize>,
    range_indices: Vec<usize>,
}

#[derive(Clone, Debug)]
struct MaterialAnimationBinding {
    initial_phase_index: usize,
    variants: Vec<MaterialVariantSet>,
    base: MaterialPhaseBinding,
    detail: Option<MaterialPhaseBinding>,
    overlay: Option<MaterialPhaseBinding>,
    overlay_detail: Option<MaterialPhaseBinding>,
    stage_textures: Option<MaterialStageTextures>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ActiveMaterialState {
    material_index: usize,
    diffuse_alpha: f32,
}

fn selected_material_phase(
    variant: &MaterialVariantSet,
    phase_index: usize,
    pipeline_category_for: impl FnOnce(usize) -> Option<u8>,
) -> Result<(usize, u8), String> {
    let material_index = selected_material_variant_index(variant, phase_index)
        .ok_or_else(|| "MAT0 material animation has no texture variant".to_string())?;
    let pipeline_category = pipeline_category_for(material_index).ok_or_else(|| {
        format!("MAT0 material animation selector {material_index} is out of bounds")
    })?;
    Ok((material_index, pipeline_category))
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct MaterialRangeSelection {
    range_index: usize,
    material_index: usize,
    pipeline_category: u8,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct MaterialPropertiesUpdate {
    material_index: usize,
    diffuse_alpha: f32,
    overlay_alpha: f32,
    base_directional: [f32; 3],
    base_additive: [f32; 3],
    overlay_directional: Option<[f32; 3]>,
    overlay_additive: Option<[f32; 3]>,
    specular_rgb: [f32; 3],
    specular_power: u8,
    overlay_specular_rgb: Option<[f32; 3]>,
    overlay_power: Option<u8>,
    uv_transforms: [[f32; 4]; 4],
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct MaterialStagePhaseUpdate {
    binding_index: usize,
    material_index: usize,
    phases: [usize; 3],
}

#[derive(Clone, Debug, Default, PartialEq)]
struct MaterialFrameUpdate {
    active_materials: HashMap<usize, ActiveMaterialState>,
    range_selections: Vec<MaterialRangeSelection>,
    properties: Vec<MaterialPropertiesUpdate>,
    stage_phases: Vec<MaterialStagePhaseUpdate>,
}

fn sample_material_animations(
    bindings: &mut [MaterialAnimationBinding],
    materials: &[VulkanStaticMaterial],
    clock_ms: u32,
) -> Result<MaterialFrameUpdate, String> {
    let mut update = MaterialFrameUpdate::default();
    for (binding_index, binding) in bindings.iter_mut().enumerate() {
        let base_sample = binding.base.sample(binding.initial_phase_index, clock_ms)?;
        let detail_sample = binding
            .detail
            .as_mut()
            .map(|detail| detail.sample(binding.initial_phase_index, clock_ms))
            .transpose()?;
        let overlay_sample = binding
            .overlay
            .as_mut()
            .map(|overlay| overlay.sample(binding.initial_phase_index, clock_ms))
            .transpose()?;
        let overlay_detail_sample = binding
            .overlay_detail
            .as_mut()
            .map(|overlay_detail| overlay_detail.sample(binding.initial_phase_index, clock_ms))
            .transpose()?;
        let mut updated_materials = HashSet::new();
        for variant in &binding.variants {
            let (material_index, pipeline_category) =
                selected_material_phase(variant, base_sample.phase_index, |index| {
                    materials
                        .get(index)
                        .map(|material| material.pipeline_category)
                })?;
            for &range_index in &variant.range_indices {
                update.active_materials.insert(
                    range_index,
                    ActiveMaterialState {
                        material_index,
                        diffuse_alpha: base_sample.coefficients.opacity,
                    },
                );
                update.range_selections.push(MaterialRangeSelection {
                    range_index,
                    material_index,
                    pipeline_category,
                });
            }
            if !updated_materials.insert(material_index) {
                continue;
            }
            let material = materials.get(material_index).ok_or_else(|| {
                format!("MAT0 material animation selector {material_index} is out of bounds")
            })?;
            let base_coefficients = base_sample.coefficients;
            let overlay_coefficients = overlay_sample.map(|sample| sample.coefficients);
            let (base_directional, base_additive, overlay_directional, overlay_additive) =
                if material.lightmap_mode {
                    // Terrain mode 3 samples the lightmap in the secondary
                    // stage and uses the authored directional RGB as its
                    // multiplicative lightmap coefficient. It has no normal
                    // lighting or overlay stage.
                    ([0.0; 3], base_coefficients.directional_rgb, None, None)
                } else {
                    (
                        base_coefficients.directional_rgb,
                        base_coefficients.additive_rgb,
                        overlay_coefficients.map(|coefficients| coefficients.directional_rgb),
                        overlay_coefficients.map(|coefficients| coefficients.additive_rgb),
                    )
                };
            if !material.lightmap_mode && binding.stage_textures.is_some() {
                update.stage_phases.push(MaterialStagePhaseUpdate {
                    binding_index,
                    material_index,
                    phases: [
                        detail_sample.map_or(0, |sample| sample.phase_index),
                        overlay_sample.map_or(0, |sample| sample.phase_index),
                        overlay_detail_sample.map_or(0, |sample| sample.phase_index),
                    ],
                });
            }
            let overlay_uv = overlay_sample
                .map(|sample| {
                    binding
                        .overlay
                        .as_ref()
                        .map_or([0.0, 0.0, 1.0, 1.0], |overlay| {
                            overlay.page_uv_transform(sample.phase_index)
                        })
                })
                .or(material.overlay_uv_transform)
                .unwrap_or([0.0, 0.0, 1.0, 1.0]);
            let detail_uv = if material.lightmap_mode {
                material.detail_uv_transform
            } else {
                detail_sample
                    .map(|sample| {
                        binding
                            .detail
                            .as_ref()
                            .map_or(material.detail_uv_transform, |detail| {
                                detail.page_uv_transform(sample.phase_index)
                            })
                    })
                    .or(Some(material.detail_uv_transform))
                    .unwrap_or([0.0, 0.0, 1.0, 1.0])
            };
            let overlay_detail_uv = if material.lightmap_mode {
                material
                    .overlay_detail_uv_transform
                    .unwrap_or([0.0, 0.0, 1.0, 1.0])
            } else {
                overlay_detail_sample
                    .map(|sample| {
                        binding.overlay_detail.as_ref().map_or(
                            material
                                .overlay_detail_uv_transform
                                .unwrap_or([0.0, 0.0, 1.0, 1.0]),
                            |overlay_detail| overlay_detail.page_uv_transform(sample.phase_index),
                        )
                    })
                    .or(material.overlay_detail_uv_transform)
                    .unwrap_or([0.0, 0.0, 1.0, 1.0])
            };
            let base_uv = binding
                .base
                .checked_page_uv_transform(base_sample.phase_index)?;
            update.properties.push(MaterialPropertiesUpdate {
                material_index,
                diffuse_alpha: base_coefficients.opacity.clamp(0.0, 1.0),
                overlay_alpha: overlay_coefficients
                    .map_or(0.0, |coefficients| coefficients.opacity.clamp(0.0, 1.0)),
                base_directional,
                base_additive,
                overlay_directional,
                overlay_additive,
                specular_rgb: base_coefficients.specular_rgb,
                specular_power: base_coefficients.power,
                overlay_specular_rgb: overlay_coefficients
                    .map(|coefficients| coefficients.specular_rgb),
                overlay_power: overlay_coefficients.map(|coefficients| coefficients.power),
                uv_transforms: [base_uv, detail_uv, overlay_uv, overlay_detail_uv],
            });
        }
    }
    Ok(update)
}

fn apply_material_frame_update(
    update: &MaterialFrameUpdate,
    bindings: &mut [MaterialAnimationBinding],
    renderer: &mut VulkanSmokeRenderer,
) -> Result<(), String> {
    for selection in &update.range_selections {
        renderer
            .set_draw_range_material(selection.range_index, selection.material_index)
            .map_err(|error| format!("update MAT0 material draw selector: {error}"))?;
        renderer
            .set_draw_range_material_category(selection.range_index, selection.pipeline_category)
            .map_err(|error| format!("update MAT0 material pipeline category: {error}"))?;
    }
    for properties in &update.properties {
        renderer
            .set_material_alphas(
                properties.material_index,
                properties.diffuse_alpha,
                properties.overlay_alpha,
            )
            .map_err(|error| format!("update MAT0 material alpha: {error}"))?;
        renderer
            .set_material_lighting_with_overlay(
                properties.material_index,
                properties.base_directional,
                properties.base_additive,
                properties.overlay_directional,
                properties.overlay_additive,
            )
            .map_err(|error| format!("update MAT0 material lighting: {error}"))?;
        renderer
            .set_material_specular(
                properties.material_index,
                properties.specular_rgb,
                properties.specular_power,
                properties.overlay_specular_rgb,
                properties.overlay_power,
            )
            .map_err(|error| format!("update MAT0 material specular: {error}"))?;
        renderer
            .set_material_uv_transforms(properties.material_index, properties.uv_transforms)
            .map_err(|error| format!("update MAT0 material Page transform: {error}"))?;
    }
    for stage_update in &update.stage_phases {
        let stage_textures = bindings
            .get_mut(stage_update.binding_index)
            .and_then(|binding| binding.stage_textures.as_mut())
            .ok_or_else(|| "MAT0 stage texture binding disappeared".to_string())?;
        if stage_textures
            .applied_material_phases
            .get(&stage_update.material_index)
            .copied()
            == Some(stage_update.phases)
        {
            continue;
        }
        let textures = [
            stage_textures
                .detail
                .as_ref()
                .and_then(|textures| textures.get(stage_update.phases[0])),
            stage_textures
                .overlay
                .as_ref()
                .and_then(|textures| textures.get(stage_update.phases[1])),
            stage_textures
                .overlay_detail
                .as_ref()
                .and_then(|textures| textures.get(stage_update.phases[2])),
        ];
        for (stage, texture) in textures.into_iter().enumerate() {
            if let Some(texture) = texture {
                renderer
                    .update_material_texture(stage_update.material_index, stage + 1, texture)
                    .map_err(|error| match stage {
                        0 => format!("update MAT0 detail texture: {error}"),
                        1 => format!("update MAT0 overlay texture: {error}"),
                        _ => format!("update MAT0 overlay detail texture: {error}"),
                    })?;
            }
        }
        stage_textures
            .applied_material_phases
            .insert(stage_update.material_index, stage_update.phases);
    }
    Ok(())
}

fn native_world_transparent(range: &VulkanStaticDrawRange, diffuse_alpha: f32) -> bool {
    diffuse_alpha < 1.0 || (range.batch_flags & 0x108) != 0
}

fn native_msh_handedness(transform: LegacyIron3dEulerTransform, scale: [f32; 3]) -> i8 {
    let Some(origin) = transform.try_transform_scaled_point([0.0; 3], scale) else {
        return 0;
    };
    let Some(x_axis) = transform
        .try_transform_scaled_point([1.0, 0.0, 0.0], scale)
        .map(|point| sub3(point, origin))
    else {
        return 0;
    };
    let Some(y_axis) = transform
        .try_transform_scaled_point([0.0, 1.0, 0.0], scale)
        .map(|point| sub3(point, origin))
    else {
        return 0;
    };
    let Some(z_axis) = transform
        .try_transform_scaled_point([0.0, 0.0, 1.0], scale)
        .map(|point| sub3(point, origin))
    else {
        return 0;
    };
    // Node and mount poses in the preview are unit-quaternion rotations, so
    // they have determinant +1 and preserve this exact object-transform sign.
    let determinant_sign = dot3(x_axis, cross3(y_axis, z_axis));
    if !determinant_sign.is_finite() || determinant_sign == 0.0 {
        0
    } else if determinant_sign < 0.0 {
        -1
    } else {
        1
    }
}

fn native_msh_cull_mode(batch_flags: u16, handedness: i8) -> LegacyCullMode {
    if batch_flags & 0x2 != 0 || batch_flags & 0x4000 != 0 || handedness == 0 {
        LegacyCullMode::Disabled
    } else if handedness > 0 {
        // The Y-negated projection maps native d > 0 faces to Vulkan's back
        // under the renderer's CLOCKWISE front-face convention.
        LegacyCullMode::FrontFace
    } else {
        LegacyCullMode::BackFace
    }
}

fn native_msh_first_triangle_visible(
    mesh: &VulkanStaticMesh,
    range: &VulkanStaticDrawRange,
    eye_world: [f32; 3],
    handedness: i8,
) -> bool {
    if range.batch_flags & 0x2 != 0 || range.batch_flags & 0x4000 == 0 {
        return true;
    }
    if handedness == 0 {
        return false;
    }
    let Some(indices) = usize::try_from(range.first_index)
        .ok()
        .and_then(|first| mesh.indices.get(first..first.checked_add(3)?))
    else {
        return false;
    };
    let Some(v0) = usize::try_from(indices[0])
        .ok()
        .and_then(|index| mesh.vertices.get(index))
    else {
        return false;
    };
    let Some(v1) = usize::try_from(indices[1])
        .ok()
        .and_then(|index| mesh.vertices.get(index))
    else {
        return false;
    };
    let Some(v2) = usize::try_from(indices[2])
        .ok()
        .and_then(|index| mesh.vertices.get(index))
    else {
        return false;
    };
    let [v0, v1, v2] = [v0.position, v1.position, v2.position];
    let face = cross3(sub3(v1, v0), sub3(v2, v0));
    let facing = dot3(sub3(eye_world, v0), face) * f32::from(handedness);
    facing.is_finite() && facing > 0.0
}

fn world_draw_range_index_count(
    mesh: &VulkanStaticMesh,
    range: &VulkanStaticDrawRange,
    original_index_count: u32,
    diffuse_alpha: f32,
    eye_world: [f32; 3],
    native_msh_handedness: Option<i8>,
) -> u32 {
    if !(diffuse_alpha > 0.0) {
        return 0;
    }
    if native_msh_handedness.is_some_and(|handedness| {
        !native_msh_first_triangle_visible(mesh, range, eye_world, handedness)
    }) {
        return 0;
    }
    original_index_count
}

fn configure_world_material_categories(
    mesh: &mut VulkanStaticMesh,
    materials: &[VulkanStaticMaterial],
    world_range_indices: &[usize],
) -> Result<(), String> {
    for &range_index in world_range_indices {
        let range = mesh
            .draw_ranges
            .get_mut(range_index)
            .ok_or_else(|| "world material range index is out of bounds".to_string())?;
        let material = materials
            .get(usize::from(range.material_index))
            .ok_or_else(|| {
                format!(
                    "world material selector {} is out of bounds",
                    range.material_index
                )
            })?;
        range.pipeline_state = range
            .pipeline_state
            .with_material_category(material.pipeline_category)
            .ok_or_else(|| {
                format!(
                    "world material selector {} has unsupported native pipeline category {}",
                    range.material_index, material.pipeline_category
                )
            })?;
    }
    Ok(())
}

fn register_material_range(
    bindings: &mut [MaterialAnimationBinding],
    material_index: usize,
    range_index: usize,
) {
    for binding in bindings {
        if let Some(variant) = binding.variants.iter_mut().find(|variant| {
            variant
                .phase_material_indices
                .get(binding.initial_phase_index)
                .copied()
                == Some(material_index)
        }) {
            variant.range_indices.push(range_index);
            return;
        }
    }
}

fn find_material_animation_variant(
    bindings: &[MaterialAnimationBinding],
    material_index: usize,
) -> Option<(usize, Vec<usize>)> {
    bindings
        .iter()
        .enumerate()
        .find_map(|(binding_index, binding)| {
            binding.variants.iter().find_map(|variant| {
                (variant
                    .phase_material_indices
                    .get(binding.initial_phase_index)
                    .copied()
                    == Some(material_index))
                .then(|| (binding_index, variant.phase_material_indices.clone()))
            })
        })
}

fn selected_material_variant_index(
    variant: &MaterialVariantSet,
    phase_index: usize,
) -> Option<usize> {
    variant.phase_material_indices.get(phase_index).copied()
}

fn remap_material_animation_ranges(
    bindings: &mut [MaterialAnimationBinding],
    remap: &[usize],
) -> Result<(), String> {
    for binding in bindings {
        for variant in &mut binding.variants {
            for range_index in &mut variant.range_indices {
                *range_index = *remap
                    .get(*range_index)
                    .ok_or_else(|| "MAT0 draw range remap is out of bounds".to_string())?;
            }
        }
    }
    Ok(())
}

fn phase_texture_name(phase: &MaterialPhase) -> Option<ResourceName> {
    let length = phase
        .texture_raw
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(phase.texture_raw.len());
    (length != 0).then(|| ResourceName(phase.texture_raw[..length].to_vec()))
}

fn phase_uv_transforms_from_assets(
    document: &Mat0Document,
    assets: &MissionAssets,
) -> Vec<[f32; 4]> {
    document
        .phases
        .iter()
        .map(|phase| {
            phase_texture_name(phase)
                .and_then(|name| {
                    assets
                        .textures
                        .iter()
                        .find(|texture| {
                            texture.usage == PreparedTextureUsage::Diffuse
                                && texture.source.name == name
                        })
                        .and_then(|texture| {
                            page_uv_transform(
                                texture.texm.width(),
                                texture.texm.height(),
                                &texture.texm.page_rects(),
                                i8::from_ne_bytes([phase.parameters[17]]),
                            )
                            .ok()
                        })
                })
                .unwrap_or([0.0, 0.0, 1.0, 1.0])
        })
        .collect()
}

fn phase_uv_transforms_from_standalone(
    loader: &mut StandaloneWearMaterialLoader,
    wear_path: &Path,
    material_index: u16,
    document: &Mat0Document,
) -> Result<Vec<[f32; 4]>, String> {
    document
        .phases
        .iter()
        .enumerate()
        .map(|(phase_index, phase)| {
            if phase_texture_name(phase).is_none() {
                return Ok([0.0, 0.0, 1.0, 1.0]);
            }
            loader
                .load(
                    wear_path,
                    material_index,
                    u16::try_from(phase_index).unwrap_or(u16::MAX),
                )
                .map(|(_, prepared)| prepared.page_uv_transform)
        })
        .collect()
}

#[derive(Clone, Copy, Debug)]
struct EnvironmentSkyLayer {
    kind: SkyPassKind,
    stage: Option<usize>,
    secondary_stage: Option<usize>,
    material_index: usize,
    diffuse_alpha: f32,
    static_directional_rgb: [f32; 3],
    static_additive_rgb: [f32; 3],
    translation_z: f32,
    range: EnvironmentGpuRange,
}

#[derive(Clone, Copy, Debug)]
struct EnvironmentSpriteLayer {
    row: usize,
    material_index: usize,
    diffuse_alpha: f32,
    flare_index: Option<usize>,
    range: EnvironmentGpuRange,
}

/// Fixed-capacity GPU allocations for the CPU-owned sky and environment
/// output.  The Vulkan renderer only receives one shared vertex update and
/// smaller draw counts each frame; descriptor images and index allocations
/// stay stable while the camera and weather move.
#[derive(Clone, Debug)]
struct EnvironmentGpuScene {
    material_animations: Vec<MaterialAnimationBinding>,
    sky_layers: Vec<EnvironmentSkyLayer>,
    sprites: Vec<EnvironmentSpriteLayer>,
    rain: EnvironmentGpuRange,
    snow: EnvironmentGpuRange,
    lightning: EnvironmentGpuRange,
    lightning_material_index: usize,
}

fn world_pipeline_state(blend: LegacyBlendMode, depth: LegacyDepthMode) -> LegacyPipelineState {
    LegacyPipelineState {
        blend,
        depth,
        ..LegacyPipelineState::default()
    }
}

fn environment_vertex(position: [f32; 3], color: [f32; 3], uv: [f32; 2]) -> VulkanStaticVertex {
    environment_vertex_with_alpha(position, color, 1.0, uv)
}

fn environment_vertex_with_alpha(
    position: [f32; 3],
    color: [f32; 3],
    alpha: f32,
    uv: [f32; 2],
) -> VulkanStaticVertex {
    environment_vertex_with_alpha_normal(position, color, alpha, uv, [0.0, 0.0, 1.0])
}

fn environment_vertex_with_alpha_normal(
    position: [f32; 3],
    color: [f32; 3],
    alpha: f32,
    uv: [f32; 2],
    normal: [f32; 3],
) -> VulkanStaticVertex {
    VulkanStaticVertex {
        position,
        color,
        normal,
        uv,
        detail_uv: [0.0, 0.0],
        overlay_alpha: alpha.clamp(0.0, 1.0),
    }
}

fn sky_normal(raw: [i8; 3]) -> [f32; 3] {
    // Ngi32's cloud-light decode uses the signed-byte scale 1/128 and keeps
    // the authored vector length. Renormalizing (or using 1/127) changes the
    // edge values and shifts the native cloud shading.
    [
        f32::from(raw[0]) / 128.0,
        f32::from(raw[1]) / 128.0,
        f32::from(raw[2]) / 128.0,
    ]
}

fn solid_environment_texture(rgba: [u8; 4]) -> VulkanStaticTexture {
    let mip = VulkanStaticTextureMip {
        width: 1,
        height: 1,
        rgba8: rgba.to_vec(),
    };
    VulkanStaticTexture {
        width: 1,
        height: 1,
        rgba8: mip.rgba8.clone(),
        mip_levels: vec![mip],
    }
}

fn vulkan_texture_from_rgba_mips(
    mips: Vec<RgbaImage>,
    label: &str,
) -> Result<VulkanStaticTexture, String> {
    let first = mips
        .first()
        .ok_or_else(|| format!("{label} has no mip zero"))?;
    Ok(VulkanStaticTexture {
        width: first.width,
        height: first.height,
        rgba8: first.rgba8.clone(),
        mip_levels: mips
            .into_iter()
            .map(|mip| VulkanStaticTextureMip {
                width: mip.width,
                height: mip.height,
                rgba8: mip.rgba8,
            })
            .collect(),
    })
}

fn append_environment_material(
    materials: &mut Vec<VulkanStaticMaterial>,
    material_names: &mut HashMap<String, usize>,
    material_assets: &HashMap<String, EnvironmentMaterialAsset>,
    material_animations: &mut Vec<MaterialAnimationBinding>,
    name: &str,
    fallback_row: usize,
) -> Result<usize, String> {
    if let Some(index) = material_names.get(name).copied() {
        return Ok(index);
    }
    let asset = material_assets.get(name).ok_or_else(|| {
        format!("environment material {name:?} is missing from resolved material.lib/MAT0 assets")
    })?;
    let phases = if asset.phases.is_empty() {
        vec![asset.phase]
    } else {
        asset.phases.clone()
    };
    let first_material_index = materials.len();
    let mut phase_material_indices = Vec::with_capacity(phases.len());
    for (phase_index, phase) in phases.iter().enumerate() {
        let index = materials.len();
        let material_index = u16::try_from(index).map_err(|_| {
            "environment exceeds the available 16-bit material selector space".to_string()
        })?;
        materials.push(VulkanStaticMaterial {
            material_index,
            pipeline_category: phase.pipeline_category,
            texture: asset
                .phase_textures
                .get(phase_index)
                .cloned()
                .unwrap_or_else(|| asset.texture.clone()),
            directional_rgb: phase.directional_rgb,
            additive_rgb: phase.additive_rgb,
            uv_transform: phase.page_uv_transform,
            detail_texture: Some(solid_environment_texture([128, 128, 128, 255])),
            overlay_texture: None,
            overlay_detail_texture: None,
            diffuse_alpha: phase.diffuse_alpha.clamp(0.0, 1.0),
            overlay_diffuse_alpha: None,
            overlay_directional_rgb: None,
            overlay_additive_rgb: None,
            specular_rgb: phase.specular_rgb,
            specular_power: phase.power,
            overlay_specular_rgb: None,
            overlay_specular_power: None,
            detail_uv_transform: [0.0, 0.0, 1.0, 1.0],
            overlay_uv_transform: None,
            overlay_detail_uv_transform: None,
            // Native cloud draws carry shade/fog flags.  The remaining sky rows
            // are unlit overlays; all rows still retain their decoded normals.
            unlit: fallback_row != 2,
            sky_nebula_stars: false,
            lightmap_mode: false,
            sky_far_depth: matches!(fallback_row, 0..=2),
            screen_tl: ENVIRONMENT_SPRITE_ROWS.contains(&fallback_row),
        });
        phase_material_indices.push(index);
    }
    let document = asset.document.clone();
    material_animations.push(MaterialAnimationBinding {
        initial_phase_index: 0,
        variants: vec![MaterialVariantSet {
            phase_material_indices,
            range_indices: Vec::new(),
        }],
        base: MaterialPhaseBinding {
            document,
            animation_block_index: 0,
            wear_row_start_ms: 0,
            random_state: first_material_index as u32,
            frozen_phase_index: None,
            phase_uv_transforms: phases.iter().map(|phase| phase.page_uv_transform).collect(),
        },
        detail: None,
        overlay: None,
        overlay_detail: None,
        stage_textures: None,
    });
    material_names.insert(name.to_owned(), first_material_index);
    Ok(first_material_index)
}

fn append_environment_gradient_material(
    materials: &mut Vec<VulkanStaticMaterial>,
) -> Result<usize, String> {
    let index = materials.len();
    let material_index = u16::try_from(index).map_err(|_| {
        "environment exceeds the available 16-bit material selector space".to_string()
    })?;
    materials.push(VulkanStaticMaterial {
        material_index,
        pipeline_category: 0,
        texture: solid_environment_texture([255, 255, 255, 255]),
        directional_rgb: [1.0; 3],
        additive_rgb: [0.0; 3],
        uv_transform: [0.0, 0.0, 1.0, 1.0],
        detail_texture: Some(solid_environment_texture([128, 128, 128, 255])),
        overlay_texture: None,
        overlay_detail_texture: None,
        diffuse_alpha: 1.0,
        overlay_diffuse_alpha: None,
        overlay_directional_rgb: None,
        overlay_additive_rgb: None,
        specular_rgb: [0.0; 3],
        specular_power: 0,
        overlay_specular_rgb: None,
        overlay_specular_power: None,
        detail_uv_transform: [0.0, 0.0, 1.0, 1.0],
        overlay_uv_transform: None,
        overlay_detail_uv_transform: None,
        unlit: true,
        sky_nebula_stars: false,
        lightmap_mode: false,
        sky_far_depth: true,
        screen_tl: false,
    });
    Ok(index)
}

fn shadow_atlas_texture() -> VulkanStaticTexture {
    let rgba8 = vec![0_u8; 256 * 256 * 4];
    VulkanStaticTexture {
        width: 256,
        height: 256,
        rgba8: rgba8.clone(),
        mip_levels: vec![VulkanStaticTextureMip {
            width: 256,
            height: 256,
            rgba8,
        }],
    }
}

fn append_shadow_material(materials: &mut Vec<VulkanStaticMaterial>) -> Result<usize, String> {
    let material_index = u16::try_from(materials.len()).map_err(|_| {
        "shadow material selector exceeds the available 16-bit material selector space".to_string()
    })?;
    let index = materials.len();
    materials.push(VulkanStaticMaterial {
        material_index,
        pipeline_category: 0,
        texture: shadow_atlas_texture(),
        directional_rgb: [1.0; 3],
        additive_rgb: [0.0; 3],
        uv_transform: [0.0, 0.0, 1.0, 1.0],
        detail_texture: Some(solid_environment_texture([255, 255, 255, 255])),
        overlay_texture: None,
        overlay_detail_texture: None,
        diffuse_alpha: 1.0,
        overlay_diffuse_alpha: None,
        overlay_directional_rgb: None,
        overlay_additive_rgb: None,
        specular_rgb: [0.0; 3],
        specular_power: 0,
        overlay_specular_rgb: None,
        overlay_specular_power: None,
        detail_uv_transform: [0.0, 0.0, 1.0, 1.0],
        overlay_uv_transform: None,
        overlay_detail_uv_transform: None,
        // Combiner mode 1 keeps the atlas alpha and the projected receiver's
        // per-vertex alpha while bypassing world lighting/fog.
        unlit: true,
        sky_nebula_stars: false,
        lightmap_mode: false,
        sky_far_depth: false,
        screen_tl: false,
    });
    Ok(index)
}

fn shadow_placeholder_vertices() -> Vec<VulkanStaticVertex> {
    [
        environment_vertex_with_alpha([0.0, 0.0, 0.0], [1.0; 3], 0.0, [0.0, 0.0]),
        environment_vertex_with_alpha([1.0, 0.0, 0.0], [1.0; 3], 0.0, [1.0, 0.0]),
        environment_vertex_with_alpha([0.0, 1.0, 0.0], [1.0; 3], 0.0, [0.0, 1.0]),
        environment_vertex_with_alpha([1.0, 1.0, 0.0], [1.0; 3], 0.0, [1.0, 1.0]),
    ]
    .to_vec()
}

fn shadow_triangle(positions: [[f32; 3]; 3], mask: u32) -> ShadowTriangle {
    let normal = normalize3(cross3(
        sub3(positions[1], positions[0]),
        sub3(positions[2], positions[0]),
    ))
    .unwrap_or([0.0, 0.0, 1.0]);
    ShadowTriangle {
        positions,
        normal,
        mask,
    }
}

fn terrain_shadow_receivers(
    terrain: &fparkan_terrain_format::LandMeshDocument,
) -> Result<Vec<ShadowTriangle>, String> {
    let assignments = terrain
        .face_lod_assignments()
        .map_err(|error| error.to_string())?;
    let mut receivers = Vec::with_capacity(
        assignments
            .iter()
            .filter(|assignment| assignment.lod == TerrainLod::Lod0)
            .count(),
    );
    for assignment in assignments {
        if assignment.lod != TerrainLod::Lod0 {
            continue;
        }
        let face = terrain.faces.get(assignment.face_index).ok_or_else(|| {
            format!(
                "terrain LOD0 references unknown face {}",
                assignment.face_index
            )
        })?;
        let positions = [
            terrain_shadow_vertex_position(terrain, assignment.face_index, face.vertices[0])?,
            terrain_shadow_vertex_position(terrain, assignment.face_index, face.vertices[1])?,
            terrain_shadow_vertex_position(terrain, assignment.face_index, face.vertices[2])?,
        ];
        receivers.push(shadow_triangle(positions, face.flags.0));
    }
    Ok(receivers)
}

fn terrain_shadow_vertex_position(
    terrain: &fparkan_terrain_format::LandMeshDocument,
    face_index: usize,
    vertex_index: u16,
) -> Result<[f32; 3], String> {
    let position = terrain
        .positions
        .get(usize::from(vertex_index))
        .copied()
        .ok_or_else(|| {
            format!(
                "terrain LOD0 shadow face {face_index} references unknown vertex {vertex_index}"
            )
        })?;
    if !position.iter().all(|value| value.is_finite()) {
        return Err(format!(
            "terrain LOD0 shadow face {face_index} has a non-finite vertex {vertex_index}"
        ));
    }
    Ok(position)
}

fn static_mesh_shadow_triangles(mesh: &VulkanStaticMesh) -> Vec<ShadowTriangle> {
    mesh.indices
        .chunks_exact(3)
        .filter_map(|triangle| {
            let positions = [
                mesh.vertices
                    .get(usize::try_from(triangle[0]).ok()?)?
                    .position,
                mesh.vertices
                    .get(usize::try_from(triangle[1]).ok()?)?
                    .position,
                mesh.vertices
                    .get(usize::try_from(triangle[2]).ok()?)?
                    .position,
            ];
            positions
                .iter()
                .flatten()
                .all(|value| value.is_finite())
                .then_some(shadow_triangle(positions, 0))
        })
        .collect()
}

fn expand_shadow_sphere_to_triangles(
    sphere: &mut ShadowSphere,
    triangles: &[ShadowTriangle],
) -> Result<bool, String> {
    if !sphere.radius.is_finite()
        || sphere.radius < 0.0
        || !sphere.center.iter().all(|value| value.is_finite())
    {
        return Err("animated shadow caster has an invalid bounding sphere".to_string());
    }
    let mut changed = false;
    for point in triangles.iter().flat_map(|triangle| triangle.positions) {
        if !point.iter().all(|value| value.is_finite()) {
            return Err("animated shadow triangle has a non-finite position".to_string());
        }
        let offset = sub3(point, sphere.center);
        let distance = length3(offset)
            .ok_or_else(|| "animated shadow point distance is non-finite".to_string())?;
        if distance <= sphere.radius {
            continue;
        }
        let next_radius = 0.5 * (sphere.radius + distance);
        let center_shift = if distance > f32::EPSILON {
            scale3(offset, (next_radius - sphere.radius) / distance)
        } else {
            [0.0; 3]
        };
        sphere.center = add3(sphere.center, center_shift);
        sphere.radius = next_radius;
        changed = true;
    }
    Ok(changed)
}

fn shadow_sphere_enclosing_triangles(
    baseline: ShadowSphere,
    triangles: &[ShadowTriangle],
) -> Result<ShadowSphere, String> {
    let mut sphere = baseline;
    expand_shadow_sphere_to_triangles(&mut sphere, triangles)?;
    Ok(sphere)
}

#[derive(Clone, Copy, Debug)]
struct ShadowComponentSphere {
    center: [f32; 3],
    radius: f32,
}

fn mounted_shadow_sphere(
    center: [f32; 3],
    radius: f32,
    mount_pose: Option<VulkanNodePose>,
    node1_pose: Option<VulkanNodePose>,
) -> Option<ShadowComponentSphere> {
    if !center.iter().all(|value| value.is_finite()) || !radius.is_finite() || radius <= 0.0 {
        return None;
    }
    let center = mount_pose.map_or(center, |parent_pose| {
        // The native mounted-unit sphere query keeps node 1's accumulated
        // rotation but replaces its translation with the accumulated parent
        // translation. This is deliberately different from the render mount
        // composition, which also carries node 1's translation.
        let rotation = node1_pose.map_or(parent_pose.rotation, |node_pose| {
            multiply_unit_quaternions(parent_pose.rotation, node_pose.rotation)
        });
        add3(
            parent_pose.translation,
            rotate_unit_vector(center, rotation),
        )
    });
    center
        .iter()
        .all(|value| value.is_finite())
        .then_some(ShadowComponentSphere { center, radius })
}

fn aggregate_shadow_caster(
    id: u32,
    native_kind: u32,
    transform: LegacyIron3dEulerTransform,
    scale: [f32; 3],
    triangles: Vec<ShadowTriangle>,
    component_spheres: &[ShadowComponentSphere],
) -> Option<ShadowCaster> {
    if triangles.is_empty() {
        return None;
    }
    let weight = component_spheres
        .iter()
        .map(|sphere| sphere.radius)
        .sum::<f32>();
    if !weight.is_finite() || weight <= f32::EPSILON {
        return None;
    }
    // Native 9510 aggregates visible root/EXTO spheres by radius-weighted
    // center, then encloses every component sphere around that center.
    let aggregate_center = component_spheres.iter().fold([0.0; 3], |sum, sphere| {
        add3(sum, scale3(sphere.center, sphere.radius))
    });
    let aggregate_center = scale3(aggregate_center, weight.recip());
    let radius = component_spheres
        .iter()
        .map(|sphere| length3(sub3(sphere.center, aggregate_center)).unwrap_or(0.0) + sphere.radius)
        .fold(0.0, f32::max);
    let max_scale = scale.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    if !aggregate_center.iter().all(|value| value.is_finite())
        || !radius.is_finite()
        || radius <= f32::EPSILON
        || !max_scale.is_finite()
        || max_scale <= 0.0
    {
        return None;
    }
    // The native scale flag applies to the aggregate exactly once. The
    // mission Euler transform then rotates/translates that scaled sphere.
    let scaled_center = [
        aggregate_center[0] * scale[0],
        aggregate_center[1] * scale[1],
        aggregate_center[2] * scale[2],
    ];
    let center = transform.try_transform_scaled_point(scaled_center, [1.0; 3])?;
    let radius = radius * max_scale;
    if !center.iter().all(|value| value.is_finite()) || !radius.is_finite() {
        return None;
    }
    let world_axes = transform
        .try_row_major()
        .map(|matrix| {
            [
                [matrix[0], matrix[4], matrix[8]],
                [matrix[1], matrix[5], matrix[9]],
                [matrix[2], matrix[6], matrix[10]],
            ]
        })
        .unwrap_or([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
    Some(ShadowCaster {
        id,
        world_axes,
        sphere: ShadowSphere { center, radius },
        native_kind,
        triangles,
    })
}

fn shadow_lights(
    sky_frame: Option<&SkyFrame<'_>>,
    environment_frame: Option<&EnvironmentFrame>,
    renderer: &VulkanSmokeRenderer,
) -> Vec<ShadowLight> {
    let mut lights = Vec::new();
    if let Some(sky_frame) = sky_frame {
        lights.extend(sky_frame.directional_lights.into_iter().enumerate().map(
            |(index, light)| {
                ShadowLight::Directional {
                    direction: light.direction,
                    rgb: light.color,
                    active: light.active,
                    // CSun's constructor marks both the sun (slot 0) and
                    // moon (slot 2) as primary. ShadowReference selects the
                    // first one for kind 3 and accumulates both active
                    // primary directions for ordinary objects (kind 4).
                    flags: (index == 0 || index == 2)
                        .then_some(SHADOW_PRIMARY_DIRECTIONAL_FLAG)
                        .unwrap_or(0),
                }
            },
        ));
    } else {
        let uniforms = renderer.frame_uniforms();
        lights.extend(
            uniforms
                .directional_lights
                .into_iter()
                .enumerate()
                .map(|(index, light)| ShadowLight::Directional {
                    direction: light.direction,
                    rgb: light.rgb,
                    active: light.active,
                    flags: (index == 0 || index == 2)
                        .then_some(SHADOW_PRIMARY_DIRECTIONAL_FLAG)
                        .unwrap_or(0),
                }),
        );
    }
    if let Some(environment_frame) = environment_frame {
        lights.extend(
            environment_frame
                .point_lights
                .iter()
                .map(|light| ShadowLight::Point {
                    position: light.position,
                    rgb: [light.color[0], light.color[1], light.color[2]],
                    range: light.range,
                    coefficients: light.attenuation,
                    active: light.active,
                    flags: 0,
                }),
        );
    } else {
        let point = renderer.frame_uniforms().point_light;
        lights.push(ShadowLight::Point {
            position: point.position,
            rgb: point.rgb,
            range: point.range,
            coefficients: point.coefficients,
            active: point.active,
            flags: 0,
        });
    }
    lights
}

/// Native `Terrain30737` expands each caster sphere by 100 world units before
/// testing it against the six planes in the current world frustum. Filtering
/// here matters because `CShade` only accepts its first twenty candidates;
/// off-screen objects must not consume that budget.
const SHADOW_FRUSTUM_RADIUS_MARGIN: f32 = 100.0;

fn native_shadow_caster_visible(caster: &ShadowCaster, clip_from_world: [f32; 16]) -> bool {
    let radius = caster.sphere.radius + SHADOW_FRUSTUM_RADIUS_MARGIN;
    if !radius.is_finite() || radius < 0.0 || !caster.sphere.center.iter().all(|v| v.is_finite()) {
        return false;
    }

    // The camera matrix is row-major D3D-style storage. A world point is
    // transformed by p * M, so each clip coordinate uses one matrix column.
    // The six Vulkan clip half-spaces are x+w, -x+w, y+w, -y+w, z, and w-z.
    let planes = [
        [
            clip_from_world[0] + clip_from_world[3],
            clip_from_world[4] + clip_from_world[7],
            clip_from_world[8] + clip_from_world[11],
            clip_from_world[12] + clip_from_world[15],
        ],
        [
            -clip_from_world[0] + clip_from_world[3],
            -clip_from_world[4] + clip_from_world[7],
            -clip_from_world[8] + clip_from_world[11],
            -clip_from_world[12] + clip_from_world[15],
        ],
        [
            clip_from_world[1] + clip_from_world[3],
            clip_from_world[5] + clip_from_world[7],
            clip_from_world[9] + clip_from_world[11],
            clip_from_world[13] + clip_from_world[15],
        ],
        [
            -clip_from_world[1] + clip_from_world[3],
            -clip_from_world[5] + clip_from_world[7],
            -clip_from_world[9] + clip_from_world[11],
            -clip_from_world[13] + clip_from_world[15],
        ],
        [
            clip_from_world[2],
            clip_from_world[6],
            clip_from_world[10],
            clip_from_world[14],
        ],
        [
            clip_from_world[3] - clip_from_world[2],
            clip_from_world[7] - clip_from_world[6],
            clip_from_world[11] - clip_from_world[10],
            clip_from_world[15] - clip_from_world[14],
        ],
    ];
    planes.into_iter().all(|plane| {
        let normal_length =
            (plane[0].mul_add(plane[0], plane[1].mul_add(plane[1], plane[2] * plane[2]))).sqrt();
        if !normal_length.is_finite() || normal_length <= f32::EPSILON {
            return false;
        }
        let distance = plane[0].mul_add(
            caster.sphere.center[0],
            plane[1].mul_add(
                caster.sphere.center[1],
                plane[2].mul_add(caster.sphere.center[2], plane[3]),
            ),
        ) / normal_length;
        distance.is_finite() && distance > -radius
    })
}

fn native_visible_shadow_casters(
    casters: &[ShadowCaster],
    clip_from_world: [f32; 16],
) -> Vec<ShadowCaster> {
    casters
        .iter()
        .filter(|caster| native_shadow_caster_visible(caster, clip_from_world))
        .cloned()
        .collect()
}

fn append_shadow_frame_geometry(
    mesh: &mut VulkanStaticMesh,
    shadow_range: EnvironmentGpuRange,
    shadow_base_vertex: usize,
    shadow_base_index: usize,
    frame: &ShadowFrame,
) -> Result<VulkanDynamicDrawRange, String> {
    if shadow_base_vertex > mesh.vertices.len() || shadow_base_index > mesh.indices.len() {
        return Err("shadow dynamic base is outside the shared mesh".to_string());
    }
    mesh.vertices.truncate(shadow_base_vertex);
    mesh.indices.truncate(shadow_base_index);
    let vertex_base = u32::try_from(mesh.vertices.len())
        .map_err(|_| "shadow vertex base exceeds u32".to_string())?;
    let first_index = u32::try_from(mesh.indices.len())
        .map_err(|_| "shadow index base exceeds u32".to_string())?;
    if frame.indices.iter().any(|&index| {
        usize::try_from(index)
            .ok()
            .is_none_or(|index| index >= frame.vertices.len())
    }) {
        return Err("shadow frame index exceeds its vertex stream".to_string());
    }
    mesh.vertices.extend(frame.vertices.iter().map(|vertex| {
        environment_vertex_with_alpha_normal(
            vertex.position,
            [1.0; 3],
            vertex.alpha.clamp(0.0, 1.0),
            vertex.uv,
            vertex.normal,
        )
    }));
    mesh.indices.extend(
        frame
            .indices
            .iter()
            .map(|&index| {
                vertex_base
                    .checked_add(index)
                    .ok_or_else(|| "shadow index exceeds u32".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?,
    );
    let index_count = u32::try_from(frame.indices.len())
        .map_err(|_| "shadow index count exceeds u32".to_string())?;
    if let Some(range) = mesh.draw_ranges.get_mut(shadow_range.range_index) {
        range.first_index = first_index;
        range.index_count = index_count;
    }
    Ok(VulkanDynamicDrawRange {
        range_index: shadow_range.range_index,
        first_index,
        index_count,
    })
}

fn shadow_atlas_texture_from_frame(frame: &ShadowFrame) -> VulkanStaticTexture {
    VulkanStaticTexture {
        width: 256,
        height: 256,
        rgba8: frame.atlas_rgba8.clone(),
        mip_levels: vec![VulkanStaticTextureMip {
            width: 256,
            height: 256,
            rgba8: frame.atlas_rgba8.clone(),
        }],
    }
}

fn append_environment_range(
    mesh: &mut VulkanStaticMesh,
    material_index: usize,
    vertices: Vec<VulkanStaticVertex>,
    local_indices: Vec<u32>,
    pipeline_state: LegacyPipelineState,
) -> Result<EnvironmentGpuRange, String> {
    if vertices.is_empty() || local_indices.is_empty() || !local_indices.len().is_multiple_of(3) {
        return Err("environment geometry range is empty or not triangle-aligned".to_string());
    }
    let vertex_start = mesh.vertices.len();
    let first_index = mesh.indices.len();
    let material_index = u16::try_from(material_index)
        .map_err(|_| "environment material selector exceeds u16".to_string())?;
    let vertex_start_u32 = u32::try_from(vertex_start)
        .map_err(|_| "environment vertex allocation exceeds u32".to_string())?;
    for index in local_indices {
        mesh.indices.push(
            vertex_start_u32
                .checked_add(index)
                .ok_or_else(|| "environment index exceeds u32".to_string())?,
        );
    }
    let index_count = u32::try_from(mesh.indices.len() - first_index)
        .map_err(|_| "environment index allocation exceeds u32".to_string())?;
    let vertex_capacity = vertices.len();
    mesh.vertices.extend(vertices);
    let first_index = u32::try_from(first_index)
        .map_err(|_| "environment first index exceeds u32".to_string())?;
    let range_index = mesh.draw_ranges.len();
    mesh.draw_ranges
        .push(fparkan_render_vulkan::VulkanStaticDrawRange {
            first_index,
            index_count,
            material_index,
            lightmap_index: u8::MAX,
            batch_flags: 0,
            pipeline_state,
            terrain_base_only: false,
            alpha_test_reference: VulkanStaticDrawRange::NATIVE_DEFAULT_ALPHA_TEST_REFERENCE,
        });
    Ok(EnvironmentGpuRange {
        range_index,
        vertex_start,
        vertex_capacity,
        material_index: usize::from(material_index),
    })
}

/// Reorders indexed draw segments without breaking the static mesh contract.
///
/// Draw ranges are required to cover one contiguous index buffer.  Changing
/// only their vector order therefore is insufficient when a background pass
/// must run before mission geometry: the corresponding index segments have to
/// move with them and every range receives a new `first_index`.
fn reorder_draw_ranges(
    mesh: &mut VulkanStaticMesh,
    old_order: &[usize],
) -> Result<Vec<usize>, String> {
    if old_order.len() != mesh.draw_ranges.len() {
        return Err("draw-range reorder does not cover the whole mesh".to_string());
    }
    let mut seen = vec![false; mesh.draw_ranges.len()];
    let old_ranges = mesh.draw_ranges.clone();
    let old_indices = mesh.indices.clone();
    let mut indices = Vec::with_capacity(old_indices.len());
    let mut ranges = Vec::with_capacity(old_ranges.len());
    let mut remap = vec![usize::MAX; old_ranges.len()];
    for (new_index, &old_index) in old_order.iter().enumerate() {
        if old_index >= old_ranges.len() || seen[old_index] {
            return Err("draw-range reorder contains a duplicate or invalid range".to_string());
        }
        seen[old_index] = true;
        let old_range = old_ranges[old_index];
        let first = usize::try_from(old_range.first_index)
            .map_err(|_| "draw-range first index exceeds addressable memory".to_string())?;
        let count = usize::try_from(old_range.index_count)
            .map_err(|_| "draw-range index count exceeds addressable memory".to_string())?;
        let end = first
            .checked_add(count)
            .ok_or_else(|| "draw-range index segment overflows address space".to_string())?;
        let segment = old_indices
            .get(first..end)
            .ok_or_else(|| "draw-range index segment is outside the mesh".to_string())?;
        let first_index = u32::try_from(indices.len())
            .map_err(|_| "reordered index buffer exceeds u32".to_string())?;
        indices.extend_from_slice(segment);
        let mut range = old_range;
        range.first_index = first_index;
        ranges.push(range);
        remap[old_index] = new_index;
    }
    if seen.iter().any(|seen| !seen) || indices.len() != old_indices.len() {
        return Err("draw-range reorder does not cover the whole index buffer".to_string());
    }
    mesh.indices = indices;
    mesh.draw_ranges = ranges;
    Ok(remap)
}

fn quad_indices(slot_count: usize) -> Result<Vec<u32>, String> {
    let mut indices = Vec::with_capacity(
        slot_count
            .checked_mul(6)
            .ok_or_else(|| "environment quad index count overflow".to_string())?,
    );
    for slot in 0..slot_count {
        let base = u32::try_from(
            slot.checked_mul(4)
                .ok_or_else(|| "environment quad vertex count overflow".to_string())?,
        )
        .map_err(|_| "environment quad vertex count exceeds u32".to_string())?;
        indices.extend_from_slice(&[base, base + 1, base + 2, base + 2, base + 1, base + 3]);
    }
    Ok(indices)
}

fn placeholder_quad_vertices(
    camera: &PreviewCameraFrame,
    slot_count: usize,
    size: f32,
) -> Vec<VulkanStaticVertex> {
    let center = add3(
        camera.position,
        scale3(camera.forward(), camera.far_plane * 0.5),
    );
    let right = scale3(camera.right(), size);
    let up = scale3(camera.up(), size);
    (0..slot_count)
        .flat_map(|_| {
            [
                environment_vertex(sub3(sub3(center, right), up), [1.0; 3], [0.0, 1.0]),
                environment_vertex(add3(sub3(center, up), right), [1.0; 3], [1.0, 1.0]),
                environment_vertex(add3(sub3(center, right), up), [1.0; 3], [0.0, 0.0]),
                environment_vertex(add3(add3(center, right), up), [1.0; 3], [1.0, 0.0]),
            ]
        })
        .collect()
}

fn screen_gradient_quad_vertices(
    camera: &PreviewCameraFrame,
    viewport: [f32; 2],
    color: [f32; 3],
) -> [VulkanStaticVertex; 4] {
    let width = viewport[0].max(1.0);
    let height = viewport[1].max(1.0);
    let aspect = width / height;
    let fov = camera.vertical_fov;
    // The quad is placed close to the camera and drawn with depth disabled.
    // It therefore covers the complete projected viewport while the later
    // dome, terrain, and object ranges provide their own depth-tested detail.
    let distance = 1.0_f32;
    let half_height = distance * (fov * 0.5).tan() * 1.05;
    let half_width = half_height * aspect;
    let center = add3(camera.position, scale3(camera.forward(), distance));
    let right = scale3(camera.right(), half_width);
    let up = scale3(camera.up(), half_height);
    [
        environment_vertex(sub3(sub3(center, right), up), color, [0.0, 1.0]),
        environment_vertex(add3(sub3(center, up), right), color, [1.0, 1.0]),
        environment_vertex(add3(sub3(center, right), up), color, [0.0, 0.0]),
        environment_vertex(add3(add3(center, right), up), color, [1.0, 0.0]),
    ]
}

impl EnvironmentGpuScene {
    fn transparent_background_range_indices(&self) -> Vec<usize> {
        self.sky_layers
            .iter()
            .filter(|layer| {
                matches!(
                    layer.kind,
                    SkyPassKind::NebulaStars | SkyPassKind::DomeGradient | SkyPassKind::Clouds
                )
            })
            .map(|layer| layer.range.range_index)
            .collect()
    }

    fn fixed_range_indices(&self) -> HashSet<usize> {
        self.sky_layers
            .iter()
            .map(|layer| layer.range.range_index)
            .chain(self.sprites.iter().map(|sprite| sprite.range.range_index))
            .chain([
                self.rain.range_index,
                self.snow.range_index,
                self.lightning.range_index,
            ])
            .collect()
    }

    fn new(
        mesh: &mut VulkanStaticMesh,
        native_handedness_by_range: &mut HashMap<usize, i8>,
        materials: &mut Vec<VulkanStaticMaterial>,
        preview_material_animations: &mut Vec<MaterialAnimationBinding>,
        sun_occlusion_range_indices: &mut Vec<usize>,
        environment_materials: &SkyMaterials,
        material_assets: &HashMap<String, EnvironmentMaterialAsset>,
        sky_mesh: &SkyMesh,
        camera: &PreviewCameraFrame,
        shadow_range_index: &mut usize,
    ) -> Result<Self, String> {
        let mut names = HashMap::new();
        let mut material_animations = Vec::new();
        let mut material_indices = [None; 9];
        let fallback_names = SkyMaterials::default();
        for row in 0..9 {
            let name = environment_materials
                .material_name(row)
                .or_else(|| fallback_names.material_name(row))
                .unwrap_or("ENVIRONMENT");
            let index = append_environment_material(
                materials,
                &mut names,
                material_assets,
                &mut material_animations,
                name,
                row,
            )?;
            material_indices[row] = Some(index);
        }
        let gradient_material_index = append_environment_gradient_material(materials)?;
        let nebula_material_index = material_indices[0]
            .ok_or_else(|| "nebula material selector is unavailable".to_string())?;
        let stars_material_index = material_indices[1]
            .ok_or_else(|| "stars material selector is unavailable".to_string())?;
        // Native sky mode 4 uses one descriptor set: TEX0 is the nebula and
        // TEX1 is the independently animated stars texture whose alpha chooses
        // the contribution. Keep the stars MAT0 binding as the nebula binding's
        // detail stage; do not form a Cartesian product of the two phase rows.
        let (nebula_binding_index, nebula_phase_materials) =
            find_material_animation_variant(&material_animations, nebula_material_index)
                .ok_or_else(|| {
                    format!("MAT0 sky material {nebula_material_index} has no phase variants")
                })?;
        let (stars_binding_index, stars_phase_materials) =
            find_material_animation_variant(&material_animations, stars_material_index)
                .ok_or_else(|| {
                    format!("MAT0 sky material {stars_material_index} has no phase variants")
                })?;
        if stars_phase_materials.is_empty() {
            return Err("stars material has no phase variants".to_string());
        }
        let stars_descriptors = stars_phase_materials
            .iter()
            .map(|&material_index| {
                let material = materials.get(material_index).ok_or_else(|| {
                    format!("stars material descriptor {material_index} is unavailable")
                })?;
                Ok((material.texture.clone(), material.uv_transform))
            })
            .collect::<Result<Vec<_>, String>>()?;
        let stars_binding = material_animations
            .get(stars_binding_index)
            .ok_or_else(|| "stars material animation binding is unavailable".to_string())?
            .base
            .clone();
        let stars_stage_textures = stars_descriptors
            .iter()
            .map(|(texture, _)| texture.clone())
            .collect();
        let stars_initial_uv = stars_descriptors
            .first()
            .map(|(_, uv_transform)| *uv_transform)
            .unwrap_or([0.0, 0.0, 1.0, 1.0]);
        for &material_index in &nebula_phase_materials {
            let stars_texture = stars_descriptors
                .first()
                .map(|(texture, _)| texture.clone())
                .ok_or_else(|| "stars material descriptor is unavailable".to_string())?;
            let nebula = materials.get_mut(material_index).ok_or_else(|| {
                format!("nebula material descriptor {material_index} is unavailable")
            })?;
            nebula.detail_texture = Some(stars_texture);
            nebula.detail_uv_transform = stars_initial_uv;
            nebula.sky_nebula_stars = true;
        }
        let nebula_binding = material_animations
            .get_mut(nebula_binding_index)
            .ok_or_else(|| "nebula material animation binding is unavailable".to_string())?;
        nebula_binding.detail = Some(stars_binding);
        nebula_binding.stage_textures = Some(MaterialStageTextures {
            detail: Some(stars_stage_textures),
            overlay: None,
            overlay_detail: None,
            // Force TEX1 to the independently sampled stars phase on the first
            // frame, including when the initial star descriptor is already set.
            applied_material_phases: HashMap::new(),
        });
        let lightning_material_index = append_environment_material(
            materials,
            &mut names,
            material_assets,
            &mut material_animations,
            "env_lightning",
            3,
        )?;

        let mut scene = Self {
            material_animations,
            sky_layers: Vec::with_capacity(4),
            sprites: Vec::with_capacity(ENVIRONMENT_SPRITE_ROWS.len() + ENVIRONMENT_FLARE_SLOTS),
            rain: EnvironmentGpuRange {
                range_index: 0,
                vertex_start: 0,
                vertex_capacity: 0,
                material_index: material_indices[8].unwrap_or(0),
            },
            snow: EnvironmentGpuRange {
                range_index: 0,
                vertex_start: 0,
                vertex_capacity: 0,
                material_index: material_indices[7].unwrap_or(0),
            },
            lightning: EnvironmentGpuRange {
                range_index: 0,
                vertex_start: 0,
                vertex_capacity: 0,
                material_index: lightning_material_index,
            },
            lightning_material_index,
        };

        let screen_range = append_environment_range(
            mesh,
            gradient_material_index,
            screen_gradient_quad_vertices(camera, [16.0, 9.0], [1.0; 3]).to_vec(),
            quad_indices(1)?,
            world_pipeline_state(LegacyBlendMode::Opaque, LegacyDepthMode::Disabled),
        )?;
        scene.sky_layers.push(EnvironmentSkyLayer {
            kind: SkyPassKind::ScreenGradient,
            stage: None,
            secondary_stage: None,
            material_index: gradient_material_index,
            diffuse_alpha: 1.0,
            static_directional_rgb: [1.0; 3],
            static_additive_rgb: [0.0; 3],
            translation_z: 0.0,
            range: screen_range,
        });

        let nebula_material = materials
            .get(nebula_material_index)
            .ok_or_else(|| "nebula material descriptor is unavailable".to_string())?;
        let nebula_vertices = sky_mesh
            .vertices
            .iter()
            .map(|vertex| {
                let color = vertex.color_rgba();
                environment_vertex_with_alpha_normal(
                    add3(vertex.position, camera.position),
                    [color[0], color[1], color[2]],
                    color[3],
                    vertex.uv[0],
                    sky_normal(vertex.normal),
                )
            })
            .collect();
        let nebula_range = append_environment_range(
            mesh,
            nebula_material_index,
            nebula_vertices,
            sky_mesh.indices.clone(),
            world_pipeline_state(LegacyBlendMode::SourceAlpha, LegacyDepthMode::TestReadOnly),
        )?;
        register_material_range(
            &mut scene.material_animations,
            nebula_material_index,
            nebula_range.range_index,
        );
        scene.sky_layers.push(EnvironmentSkyLayer {
            kind: SkyPassKind::NebulaStars,
            stage: Some(0),
            secondary_stage: Some(1),
            material_index: nebula_material_index,
            diffuse_alpha: nebula_material.diffuse_alpha,
            static_directional_rgb: nebula_material.directional_rgb,
            static_additive_rgb: nebula_material.additive_rgb,
            translation_z: 0.0,
            range: nebula_range,
        });

        let dome_vertices = sky_mesh
            .vertices
            .iter()
            .map(|vertex| {
                let color = vertex.color_rgba();
                environment_vertex_with_alpha_normal(
                    add3(vertex.position, camera.position),
                    [color[0], color[1], color[2]],
                    color[3],
                    [0.0, 0.0],
                    sky_normal(vertex.normal),
                )
            })
            .collect();
        let dome_range = append_environment_range(
            mesh,
            gradient_material_index,
            dome_vertices,
            sky_mesh.indices.clone(),
            world_pipeline_state(LegacyBlendMode::SourceAlpha, LegacyDepthMode::TestReadOnly),
        )?;
        scene.sky_layers.push(EnvironmentSkyLayer {
            kind: SkyPassKind::DomeGradient,
            stage: None,
            secondary_stage: None,
            material_index: gradient_material_index,
            diffuse_alpha: 1.0,
            static_directional_rgb: [1.0; 3],
            static_additive_rgb: [0.0; 3],
            translation_z: 0.0,
            range: dome_range,
        });

        let clouds_material_index = material_indices[2]
            .ok_or_else(|| "cloud material selector is unavailable".to_string())?;
        let clouds_material = materials
            .get(clouds_material_index)
            .ok_or_else(|| "cloud material descriptor is unavailable".to_string())?;
        let clouds_vertices = sky_mesh
            .vertices
            .iter()
            .map(|vertex| {
                let color = vertex.color_rgba();
                environment_vertex_with_alpha_normal(
                    add3(add3(vertex.position, camera.position), [0.0, 0.0, -5000.0]),
                    [color[0], color[1], color[2]],
                    color[3],
                    vertex.uv[2],
                    sky_normal(vertex.normal),
                )
            })
            .collect();
        let clouds_range = append_environment_range(
            mesh,
            clouds_material_index,
            clouds_vertices,
            sky_mesh.indices.clone(),
            world_pipeline_state(LegacyBlendMode::SourceAlpha, LegacyDepthMode::TestReadOnly),
        )?;
        register_material_range(
            &mut scene.material_animations,
            clouds_material_index,
            clouds_range.range_index,
        );
        scene.sky_layers.push(EnvironmentSkyLayer {
            kind: SkyPassKind::Clouds,
            stage: Some(2),
            secondary_stage: None,
            material_index: clouds_material_index,
            diffuse_alpha: clouds_material.diffuse_alpha,
            static_directional_rgb: clouds_material.directional_rgb,
            static_additive_rgb: clouds_material.additive_rgb,
            translation_z: -5000.0,
            range: clouds_range,
        });

        let sprite_size = (camera.far_plane * 0.01).max(1.0);
        for row in ENVIRONMENT_SPRITE_ROWS {
            let material_index = material_indices[row]
                .ok_or_else(|| format!("sky sprite row {row} has no material selector"))?;
            let range = append_environment_range(
                mesh,
                material_index,
                placeholder_quad_vertices(camera, 1, sprite_size),
                quad_indices(1)?,
                world_pipeline_state(LegacyBlendMode::SourceAlpha, LegacyDepthMode::TestReadOnly),
            )?;
            register_material_range(
                &mut scene.material_animations,
                material_index,
                range.range_index,
            );
            scene.sprites.push(EnvironmentSpriteLayer {
                row,
                material_index,
                diffuse_alpha: materials
                    .get(material_index)
                    .map_or(1.0, |material| material.diffuse_alpha),
                flare_index: None,
                range,
            });
        }

        for flare_index in 0..ENVIRONMENT_FLARE_SLOTS {
            let row = SKY_FLARE_ROWS[flare_index];
            let material_index = material_indices[row]
                .ok_or_else(|| format!("sky flare row {row} has no material selector"))?;
            let range = append_environment_range(
                mesh,
                material_index,
                placeholder_quad_vertices(camera, 1, sprite_size),
                quad_indices(1)?,
                world_pipeline_state(LegacyBlendMode::Additive, LegacyDepthMode::TestReadOnly),
            )?;
            register_material_range(
                &mut scene.material_animations,
                material_index,
                range.range_index,
            );
            scene.sprites.push(EnvironmentSpriteLayer {
                row,
                material_index,
                diffuse_alpha: materials
                    .get(material_index)
                    .map_or(1.0, |material| material.diffuse_alpha),
                flare_index: Some(flare_index),
                range,
            });
        }

        let particle_indices = quad_indices(ENVIRONMENT_PARTICLE_SLOTS)?;
        scene.rain = append_environment_range(
            mesh,
            material_indices[8]
                .ok_or_else(|| "rain material selector is unavailable".to_string())?,
            placeholder_quad_vertices(camera, ENVIRONMENT_PARTICLE_SLOTS, 0.25),
            particle_indices.clone(),
            world_pipeline_state(LegacyBlendMode::SourceAlpha, LegacyDepthMode::TestReadOnly),
        )?;
        register_material_range(
            &mut scene.material_animations,
            scene.rain.material_index,
            scene.rain.range_index,
        );
        scene.snow = append_environment_range(
            mesh,
            material_indices[7]
                .ok_or_else(|| "snow material selector is unavailable".to_string())?,
            placeholder_quad_vertices(camera, ENVIRONMENT_PARTICLE_SLOTS, 0.25),
            particle_indices,
            world_pipeline_state(LegacyBlendMode::SourceAlpha, LegacyDepthMode::TestReadOnly),
        )?;
        register_material_range(
            &mut scene.material_animations,
            scene.snow.material_index,
            scene.snow.range_index,
        );
        scene.lightning = append_environment_range(
            mesh,
            lightning_material_index,
            placeholder_quad_vertices(camera, 1, sprite_size),
            quad_indices(1)?,
            world_pipeline_state(LegacyBlendMode::SourceAlpha, LegacyDepthMode::TestReadOnly),
        )?;
        register_material_range(
            &mut scene.material_animations,
            scene.lightning.material_index,
            scene.lightning.range_index,
        );

        // Preserve the native sky order while keeping the indexed mesh
        // contiguous.  The screen gradient must be the first draw, and the
        // cloud dome follows all directional sprites.  Reordering the range
        // metadata alone would leave first_index values pointing into the old
        // segment order, so move each index segment together with its range.
        let range_for_kind = |kind: SkyPassKind| {
            scene
                .sky_layers
                .iter()
                .find(|layer| layer.kind == kind)
                .map(|layer| layer.range.range_index)
        };
        let screen_index = range_for_kind(SkyPassKind::ScreenGradient)
            .ok_or_else(|| "screen gradient range was not allocated".to_string())?;
        let nebula_index = range_for_kind(SkyPassKind::NebulaStars)
            .ok_or_else(|| "nebula sky range was not allocated".to_string())?;
        let dome_index = range_for_kind(SkyPassKind::DomeGradient)
            .ok_or_else(|| "dome gradient range was not allocated".to_string())?;
        let cloud_index = range_for_kind(SkyPassKind::Clouds)
            .ok_or_else(|| "cloud sky range was not allocated".to_string())?;
        let sprite_indices = scene
            .sprites
            .iter()
            .map(|sprite| sprite.range.range_index)
            .collect::<Vec<_>>();
        if sprite_indices.is_empty() {
            return Err("sky sprite ranges were not allocated".to_string());
        }
        let rain_index = scene.rain.range_index;
        let snow_index = scene.snow.range_index;
        let lightning_index = scene.lightning.range_index;
        let environment_indices = [
            screen_index,
            nebula_index,
            dome_index,
            cloud_index,
            rain_index,
            snow_index,
            lightning_index,
            *shadow_range_index,
        ]
        .into_iter()
        .chain(sprite_indices.iter().copied())
        .collect::<HashSet<_>>();
        let old_range_count = mesh.draw_ranges.len();
        let mut old_order = Vec::with_capacity(old_range_count);
        old_order.push(screen_index);
        old_order.extend((0..old_range_count).filter(|index| {
            !environment_indices.contains(index)
                && *index != screen_index
                && *index != nebula_index
                && *index != dome_index
                && *index != cloud_index
        }));
        old_order.push(*shadow_range_index);
        old_order.extend([nebula_index, dome_index, cloud_index]);
        old_order.extend(sprite_indices.iter().copied());
        old_order.extend([rain_index, snow_index, lightning_index]);
        let remap = reorder_draw_ranges(mesh, &old_order)?;
        let mut remapped_handedness = HashMap::with_capacity(native_handedness_by_range.len());
        for (old_range_index, handedness) in std::mem::take(native_handedness_by_range) {
            let new_range_index = remap.get(old_range_index).copied().ok_or_else(|| {
                format!("native MSH draw range remap is missing range {old_range_index}")
            })?;
            remapped_handedness.insert(new_range_index, handedness);
        }
        *native_handedness_by_range = remapped_handedness;
        for layer in &mut scene.sky_layers {
            layer.range.range_index = *remap
                .get(layer.range.range_index)
                .ok_or_else(|| "sky draw range remap is out of bounds".to_string())?;
        }
        for sprite in &mut scene.sprites {
            sprite.range.range_index = *remap
                .get(sprite.range.range_index)
                .ok_or_else(|| "sky sprite draw range remap is out of bounds".to_string())?;
        }
        scene.rain.range_index = *remap
            .get(scene.rain.range_index)
            .ok_or_else(|| "rain draw range remap is out of bounds".to_string())?;
        scene.snow.range_index = *remap
            .get(scene.snow.range_index)
            .ok_or_else(|| "snow draw range remap is out of bounds".to_string())?;
        scene.lightning.range_index = *remap
            .get(scene.lightning.range_index)
            .ok_or_else(|| "lightning draw range remap is out of bounds".to_string())?;
        remap_material_animation_ranges(&mut scene.material_animations, &remap)?;
        remap_material_animation_ranges(preview_material_animations, &remap)?;
        for range_index in sun_occlusion_range_indices {
            *range_index = *remap
                .get(*range_index)
                .ok_or_else(|| "sun occlusion range remap is out of bounds".to_string())?;
        }
        *shadow_range_index = *remap
            .get(*shadow_range_index)
            .ok_or_else(|| "shadow draw range remap is out of bounds".to_string())?;
        Ok(scene)
    }

    fn update(
        &mut self,
        mesh: &mut VulkanStaticMesh,
        renderer: &mut VulkanSmokeRenderer,
        material_update: &MaterialFrameUpdate,
        sky_frame: &SkyFrame,
        sky_mesh: &SkyMesh,
        environment_frame: &EnvironmentFrame,
        camera: &PreviewCameraFrame,
        viewport: [f32; 2],
        terrain: &TerrainWorld,
        sun_occlusion_range_indices: &[usize],
    ) -> Result<(), String> {
        let active_materials = &material_update.active_materials;
        // Resolve the native glare once for the frame.  CSky feeds the
        // boosted primary color back into the light manager and uses the
        // positive boost delta for both the screen-gradient horizon floor and
        // the dome palette clamp.
        let projected_sun =
            project_direction_pixels(camera, sky_frame.sun.geometry_direction(), viewport)
                .unwrap_or([f32::NAN; 2]);
        let optics = sky_frame
            .sun_optics(SunVisibility {
                projected_sun,
                viewport,
                view_forward: camera.forward(),
                // Flares use the same terrain/object ray query as the native
                // visibility gate. The sun sprite itself deliberately stays
                // independent of this test, matching the native layer's
                // `requires_occlusion` split.
                unoccluded: native_sun_unoccluded_with_world(
                    terrain,
                    mesh,
                    sun_occlusion_range_indices,
                    camera,
                    sky_frame.sun.geometry_direction(),
                ),
            })
            .ok();
        let glare_rgb_delta = optics.map_or([0.0; 3], |optics| {
            std::array::from_fn(|index| {
                (optics.primary_color[index] - sky_frame.sun.color[index]).max(0.0)
            })
        });
        let gradient = sky_frame.screen_gradient(camera.native_sky_heading(), glare_rgb_delta);
        if let Some(optics) = optics {
            let mut uniforms = renderer.frame_uniforms();
            if sky_frame.sun.active {
                uniforms.directional_lights[0].rgb = optics.primary_color;
            }
            renderer
                .set_frame_uniforms(uniforms)
                .map_err(|error| format!("update glare light uniforms: {error}"))?;
        }
        let gradient_rgba = gradient.color.rgba();
        for layer in &self.sky_layers {
            let range = layer.range;
            let destination = mesh
                .vertices
                .get_mut(range.vertex_start..range.vertex_start + range.vertex_capacity)
                .ok_or_else(|| "sky vertex allocation is out of bounds".to_string())?;
            let pass_frame = sky_frame
                .passes
                .iter()
                .find(|pass| pass.kind == layer.kind)
                .ok_or_else(|| format!("sky frame is missing {:?} pass", layer.kind))?;
            match layer.kind {
                SkyPassKind::ScreenGradient => {
                    if destination.len() != 4 {
                        return Err("screen gradient vertex allocation changed".to_string());
                    }
                    destination.copy_from_slice(&screen_gradient_quad_vertices(
                        camera,
                        viewport,
                        [gradient_rgba[0], gradient_rgba[1], gradient_rgba[2]],
                    ));
                }
                SkyPassKind::NebulaStars | SkyPassKind::DomeGradient | SkyPassKind::Clouds => {
                    if destination.len() != sky_mesh.vertices.len() {
                        return Err(
                            "sky mesh vertex count changed after GPU allocation".to_string()
                        );
                    }
                    for (target, source) in destination.iter_mut().zip(&sky_mesh.vertices) {
                        let source_color = match layer.kind {
                            // Native mode 4 receives a white DIFFUSE and
                            // chooses nebula/stars through TEX1 alpha.
                            SkyPassKind::NebulaStars | SkyPassKind::Clouds => [1.0; 4],
                            SkyPassKind::DomeGradient => {
                                gradient.clamp_dome_color(source.color).rgba()
                            }
                            SkyPassKind::ScreenGradient => unreachable!(),
                        };
                        target.position = add3(
                            add3(source.position, camera.position),
                            [0.0, 0.0, layer.translation_z],
                        );
                        target.color = [source_color[0], source_color[1], source_color[2]];
                        target.uv = layer
                            .stage
                            .and_then(|stage| source.uv.get(stage).copied())
                            .unwrap_or([0.0, 0.0]);
                        target.detail_uv = layer
                            .secondary_stage
                            .and_then(|stage| source.uv.get(stage).copied())
                            .unwrap_or([0.0, 0.0]);
                        target.normal = sky_normal(source.normal);
                        target.overlay_alpha = source_color[3];
                    }
                }
            }
            let material_index = active_materials
                .get(&range.range_index)
                .map(|state| state.material_index)
                .unwrap_or(layer.material_index);
            if !active_materials.contains_key(&range.range_index) {
                renderer
                    .set_material_alphas(material_index, layer.diffuse_alpha.clamp(0.0, 1.0), 0.0)
                    .map_err(|error| format!("update sky material alpha: {error}"))?;
            }
            if layer.kind == SkyPassKind::Clouds {
                let directional_rgb = pass_frame
                    .material_color
                    .unwrap_or(layer.static_directional_rgb);
                renderer
                    .set_material_lighting(
                        material_index,
                        directional_rgb,
                        layer.static_additive_rgb,
                    )
                    .map_err(|error| format!("update cloud material lighting: {error}"))?;
            }
            renderer
                .set_draw_range_index_count(
                    range.range_index,
                    if layer.kind == SkyPassKind::ScreenGradient {
                        6
                    } else {
                        u32::try_from(sky_mesh.indices.len())
                            .map_err(|_| "sky index count exceeds u32".to_string())?
                    },
                )
                .map_err(|error| format!("update sky draw range: {error}"))?;
        }

        // Flares share their source material row, so their packed alpha lives
        // in each vertex.  Upload the shared phase alpha once before emitting
        // the twelve fixed-capacity ranges.
        let mut flare_materials = HashMap::new();
        for sprite in &self.sprites {
            if sprite.flare_index.is_some()
                && !active_materials.contains_key(&sprite.range.range_index)
            {
                flare_materials.insert(sprite.material_index, sprite.diffuse_alpha);
            }
        }
        for (material_index, diffuse_alpha) in flare_materials {
            renderer
                .set_material_alphas(material_index, diffuse_alpha.clamp(0.0, 1.0), 0.0)
                .map_err(|error| format!("update sky flare alpha: {error}"))?;
        }
        for sprite in &self.sprites {
            let mut active = false;
            let mut vertices = None;
            let mut material_alpha = 0.0;
            if let Some(flare_index) = sprite.flare_index {
                if let Some(flare) =
                    optics.and_then(|optics| optics.flares.get(flare_index).copied())
                {
                    let direction = screen_ray_from_pixels(camera, flare.center_pixels, viewport);
                    active = flare.active && direction.is_some() && flare.color[3] > 0.0;
                    if let Some(direction) = direction {
                        vertices = Some(sprite_quad_vertices_pixels(
                            camera,
                            direction,
                            (camera.far_plane * 0.5).max(camera.near_plane * 2.0),
                            flare.half_size_pixels,
                            flare.half_size_pixels,
                            viewport,
                            [flare.color[0], flare.color[1], flare.color[2]],
                            flare.color[3],
                            flare.uv,
                        ));
                    }
                }
            } else if let Some(frame) = sprite_frame_for_row(sky_frame, sprite.row) {
                let projected = project_direction_pixels(camera, frame.direction, viewport);
                active = frame.active
                    && frame.intensity > 0.0
                    && projected.is_some_and(|center| {
                        center[0] >= 0.0
                            && center[0] <= viewport[0]
                            && center[1] >= 0.0
                            && center[1] <= viewport[1]
                    });
                if active {
                    if let Ok([half_width, half_height]) = sprite_half_size_pixels(
                        frame.size_values,
                        viewport,
                        horizontal_fov(camera, viewport[0] / viewport[1].max(1.0)),
                    ) {
                        if ENVIRONMENT_SPRITE_ROWS.contains(&sprite.row) {
                            if let Some(center_pixels) = projected {
                                vertices = Some(screen_tl_quad_vertices_pixels(
                                    center_pixels,
                                    half_width,
                                    half_height,
                                    viewport,
                                    frame.color,
                                    frame.alpha,
                                    frame.uv,
                                ));
                            } else {
                                active = false;
                            }
                        } else {
                            vertices = Some(sprite_quad_vertices_pixels(
                                camera,
                                frame.direction,
                                (camera.far_plane * 0.5).max(camera.near_plane * 2.0),
                                half_width,
                                half_height,
                                viewport,
                                frame.color,
                                frame.alpha,
                                frame.uv,
                            ));
                        }
                    } else {
                        active = false;
                    }
                }
                material_alpha = if active {
                    // The packed sprite alpha is carried by the vertex. The
                    // MAT0 phase alpha remains the material multiplier;
                    // intensity controls lifecycle visibility, not a second
                    // alpha fade.
                    sprite.diffuse_alpha
                } else {
                    0.0
                };
            }
            if active {
                if let Some(vertices) = vertices {
                    let destination = mesh
                        .vertices
                        .get_mut(sprite.range.vertex_start..sprite.range.vertex_start + 4)
                        .ok_or_else(|| {
                            "sky sprite vertex allocation is out of bounds".to_string()
                        })?;
                    destination.copy_from_slice(&vertices);
                } else {
                    active = false;
                }
            }
            if sprite.flare_index.is_none() {
                let material_index = active_materials
                    .get(&sprite.range.range_index)
                    .map(|state| state.material_index)
                    .unwrap_or(sprite.material_index);
                // The animation update above has already restored the
                // sampled phase alpha for an active animated material.  A
                // hidden sprite still needs an explicit zero, while a static
                // sprite uses its authored material alpha when visible.
                let should_write_alpha =
                    !active || !active_materials.contains_key(&sprite.range.range_index);
                if should_write_alpha {
                    renderer
                        .set_material_alphas(material_index, material_alpha.clamp(0.0, 1.0), 0.0)
                        .map_err(|error| format!("update sky sprite alpha: {error}"))?;
                }
            }
            renderer
                .set_draw_range_index_count(sprite.range.range_index, if active { 6 } else { 0 })
                .map_err(|error| format!("update sky sprite draw range: {error}"))?;
        }

        let mut rain_count = 0usize;
        let mut snow_count = 0usize;
        let mut lightning = None;
        for primitive in &environment_frame.primitives {
            match primitive {
                EnvironmentPrimitive::Particle {
                    kind,
                    color,
                    screen,
                    ..
                } => {
                    let (range, slot) = match kind {
                        PrecipitationKind::Rain if rain_count < ENVIRONMENT_PARTICLE_SLOTS => {
                            let slot = rain_count;
                            rain_count += 1;
                            (self.rain, slot)
                        }
                        PrecipitationKind::Snow if snow_count < ENVIRONMENT_PARTICLE_SLOTS => {
                            let slot = snow_count;
                            snow_count += 1;
                            (self.snow, slot)
                        }
                        _ => continue,
                    };
                    let vertices =
                        precipitation_quad_vertices(*kind, screen, *color, camera, viewport);
                    let start = range.vertex_start + slot * 4;
                    let destination = mesh.vertices.get_mut(start..start + 4).ok_or_else(|| {
                        "precipitation vertex allocation is out of bounds".to_string()
                    })?;
                    destination.copy_from_slice(&vertices);
                }
                EnvironmentPrimitive::Lightning(bolt) if lightning.is_none() => {
                    lightning = Some(bolt);
                }
                EnvironmentPrimitive::Lightning(_) => {}
            }
        }
        renderer
            .set_draw_range_index_count(
                self.rain.range_index,
                u32::try_from(rain_count * 6)
                    .map_err(|_| "rain draw count exceeds u32".to_string())?,
            )
            .map_err(|error| format!("update rain draw range: {error}"))?;
        renderer
            .set_draw_range_index_count(
                self.snow.range_index,
                u32::try_from(snow_count * 6)
                    .map_err(|_| "snow draw count exceeds u32".to_string())?,
            )
            .map_err(|error| format!("update snow draw range: {error}"))?;

        let lightning_alpha = if let Some(bolt) = lightning {
            let vertices = lightning_quad_vertices(bolt);
            let destination = mesh
                .vertices
                .get_mut(self.lightning.vertex_start..self.lightning.vertex_start + 4)
                .ok_or_else(|| "lightning vertex allocation is out of bounds".to_string())?;
            destination.copy_from_slice(&vertices);
            bolt.intensity.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let lightning_material_index = active_materials
            .get(&self.lightning.range_index)
            .map(|state| state.material_index)
            .unwrap_or(self.lightning_material_index);
        renderer
            .set_material_alphas(lightning_material_index, lightning_alpha, 0.0)
            .map_err(|error| format!("update lightning material alpha: {error}"))?;
        renderer
            .set_draw_range_index_count(
                self.lightning.range_index,
                if lightning.is_some() { 6 } else { 0 },
            )
            .map_err(|error| format!("update lightning draw range: {error}"))?;
        Ok(())
    }
}

fn sprite_frame_for_row<'frame, 'materials>(
    sky_frame: &'frame SkyFrame<'materials>,
    row: usize,
) -> Option<SkySpriteFrame<'materials>> {
    sky_frame
        .sprites
        .iter()
        .copied()
        .find(|sprite| sprite.row == row)
}

fn horizontal_fov(camera: &PreviewCameraFrame, aspect: f32) -> f32 {
    let aspect = if aspect.is_finite() && aspect > 0.0 {
        aspect
    } else {
        16.0 / 9.0
    };
    2.0 * ((camera.vertical_fov * 0.5).tan() * aspect).atan()
}

/// Tests the camera-to-sun segment against the validated mission terrain.
///
/// The native CSun caller starts a half-unit in front of the camera along the
/// normalized geometry direction, then queries the finite segment ending at
/// the sun center. Terrain source positions and captured-camera positions both
/// use the `Land.msh` coordinate units, so the ray is queried without a Z
/// conversion. Its parameter remains normalized to the finite segment and is
/// therefore independent of world distance.
#[cfg(test)]
fn native_sun_unoccluded(
    terrain: &TerrainWorld,
    camera: &PreviewCameraFrame,
    direction: [f32; 3],
) -> bool {
    let empty_mesh = VulkanStaticMesh {
        vertices: Vec::new(),
        indices: Vec::new(),
        draw_ranges: Vec::new(),
    };
    native_sun_unoccluded_with_world(terrain, &empty_mesh, &[], camera, direction)
}

fn native_sun_unoccluded_with_world(
    terrain: &TerrainWorld,
    world_mesh: &VulkanStaticMesh,
    sun_occlusion_range_indices: &[usize],
    camera: &PreviewCameraFrame,
    direction: [f32; 3],
) -> bool {
    // Native CSun requests object kinds 1, 3, 4, and 10 (the aggregate
    // object mask is 0x41A) and excludes full surface flag 0x20. The preview
    // range list is built from roots whose raw mission kind is one of those
    // four eligible kinds; fixed atmosphere ranges are remapped out before
    // this call.
    let Some(direction) = normalize3(direction) else {
        return false;
    };
    let sun_center = add3(
        camera.position,
        scale3(
            direction,
            (camera.far_plane * 0.5).max(camera.near_plane * 2.0),
        ),
    );
    let start = add3(camera.position, scale3(direction, 0.5));
    let segment = sub3(sun_center, start);
    if !sun_center
        .iter()
        .chain(start.iter())
        .chain(segment.iter())
        .all(|value| value.is_finite())
    {
        return false;
    }
    if dot3(segment, segment) <= f32::EPSILON {
        return true;
    }
    if let Ok(Some(hit)) = terrain.raycast_excluding(
        start,
        segment,
        FullSurfaceMask(0),
        FullSurfaceMask(0x0000_0020),
    ) {
        if hit.distance.is_finite() && hit.distance <= 1.0 {
            return false;
        }
    }
    !world_mesh_sun_occluded(world_mesh, sun_occlusion_range_indices, start, segment)
}

fn world_mesh_sun_occluded(
    mesh: &VulkanStaticMesh,
    range_indices: &[usize],
    origin: [f32; 3],
    direction: [f32; 3],
) -> bool {
    for &range_index in range_indices {
        let Some(range) = mesh.draw_ranges.get(range_index).copied() else {
            continue;
        };
        // The native world query receives the compact exclusion bit 0x8,
        // which is the world-space representation of full surface flag 0x20.
        if range.batch_flags & 0x0008 != 0 {
            continue;
        }
        let Ok(first_index) = usize::try_from(range.first_index) else {
            continue;
        };
        let Ok(index_count) = usize::try_from(range.index_count) else {
            continue;
        };
        let Some(indices) = mesh
            .indices
            .get(first_index..first_index.saturating_add(index_count))
        else {
            continue;
        };
        for triangle in indices.chunks_exact(3) {
            let Ok(a) = usize::try_from(triangle[0]) else {
                continue;
            };
            let Ok(b) = usize::try_from(triangle[1]) else {
                continue;
            };
            let Ok(c) = usize::try_from(triangle[2]) else {
                continue;
            };
            let (Some(&a), Some(&b), Some(&c)) = (
                mesh.vertices.get(a),
                mesh.vertices.get(b),
                mesh.vertices.get(c),
            ) else {
                continue;
            };
            if segment_intersects_triangle(origin, direction, a.position, b.position, c.position) {
                return true;
            }
        }
    }
    false
}

fn segment_intersects_triangle(
    origin: [f32; 3],
    direction: [f32; 3],
    a: [f32; 3],
    b: [f32; 3],
    c: [f32; 3],
) -> bool {
    let edge1 = sub3(b, a);
    let edge2 = sub3(c, a);
    let pvec = cross3(direction, edge2);
    let determinant = dot3(edge1, pvec);
    if !determinant.is_finite() || determinant.abs() <= f32::EPSILON {
        return false;
    }
    let inverse = determinant.recip();
    let tvec = sub3(origin, a);
    let u = dot3(tvec, pvec) * inverse;
    if !u.is_finite() || !(0.0..=1.0).contains(&u) {
        return false;
    }
    let qvec = cross3(tvec, edge1);
    let v = dot3(direction, qvec) * inverse;
    if !v.is_finite() || v < 0.0 || u + v > 1.0 {
        return false;
    }
    let distance = dot3(edge2, qvec) * inverse;
    distance.is_finite() && (0.0..=1.0).contains(&distance)
}

/// Replays the native draw-item callback: one whole indexed batch is keyed by
/// the camera distance to its first indexed vertex.
fn first_indexed_vertex_distance(
    mesh: &VulkanStaticMesh,
    range_index: usize,
    camera_position: [f32; 3],
) -> Result<f32, String> {
    let range: VulkanStaticDrawRange = *mesh
        .draw_ranges
        .get(range_index)
        .ok_or_else(|| "world draw range index is out of bounds".to_string())?;
    let first_index = usize::try_from(range.first_index)
        .map_err(|_| "world draw range first index is not addressable".to_string())?;
    let vertex_index =
        usize::try_from(*mesh.indices.get(first_index).ok_or_else(|| {
            "world draw range first index is outside the index buffer".to_string()
        })?)
        .map_err(|_| "world draw range vertex index is not addressable".to_string())?;
    let vertex = mesh
        .vertices
        .get(vertex_index)
        .ok_or_else(|| "world draw range vertex index is outside the vertex buffer".to_string())?;
    let delta = sub3(vertex.position, camera_position);
    let distance = dot3(delta, delta).sqrt();
    distance
        .is_finite()
        .then_some(distance)
        .ok_or_else(|| "world draw range sort distance is non-finite".to_string())
}

fn project_direction_pixels(
    camera: &PreviewCameraFrame,
    direction: [f32; 3],
    viewport: [f32; 2],
) -> Option<[f32; 2]> {
    let direction = normalize3(direction)?;
    let width = viewport[0];
    let height = viewport[1];
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return None;
    }
    let depth = dot3(camera.forward(), direction);
    if !depth.is_finite() || depth <= 1.0e-4 {
        return None;
    }
    let aspect = width / height;
    let half_vertical = (camera.vertical_fov * 0.5).tan();
    let half_horizontal = half_vertical * aspect;
    let ndc_x = dot3(camera.right(), direction) / (depth * half_horizontal);
    let ndc_y = dot3(camera.up(), direction) / (depth * half_vertical);
    let pixels = [(ndc_x * 0.5 + 0.5) * width, (0.5 - ndc_y * 0.5) * height];
    pixels
        .iter()
        .all(|value| value.is_finite())
        .then_some(pixels)
}

fn screen_ray_from_pixels(
    camera: &PreviewCameraFrame,
    pixels: [f32; 2],
    viewport: [f32; 2],
) -> Option<[f32; 3]> {
    if !pixels.iter().all(|value| value.is_finite())
        || !viewport.iter().all(|value| value.is_finite())
        || viewport[0] <= 0.0
        || viewport[1] <= 0.0
    {
        return None;
    }
    let aspect = viewport[0] / viewport[1];
    let half_vertical = (camera.vertical_fov * 0.5).tan();
    let half_horizontal = half_vertical * aspect;
    let ndc_x = pixels[0] / viewport[0] * 2.0 - 1.0;
    let ndc_y = 1.0 - pixels[1] / viewport[1] * 2.0;
    normalize3(add3(
        add3(
            camera.forward(),
            scale3(camera.right(), ndc_x * half_horizontal),
        ),
        scale3(camera.up(), ndc_y * half_vertical),
    ))
}

fn sprite_quad_vertices_pixels(
    camera: &PreviewCameraFrame,
    direction: [f32; 3],
    distance: f32,
    half_width_pixels: f32,
    half_height_pixels: f32,
    viewport: [f32; 2],
    color: [f32; 3],
    alpha: f32,
    uv: [[f32; 2]; 4],
) -> [VulkanStaticVertex; 4] {
    let direction = normalize3(direction).unwrap_or(camera.forward());
    let center = add3(camera.position, scale3(direction, distance));
    let depth = dot3(camera.forward(), direction)
        .mul_add(distance, 0.0)
        .max(camera.near_plane.max(0.001));
    let width = viewport[0].max(1.0);
    let height = viewport[1].max(1.0);
    let aspect = width / height;
    let vertical_fov = camera.vertical_fov;
    let horizontal_fov_radians = horizontal_fov(camera, aspect);
    let world_per_pixel_x = 2.0 * depth * (horizontal_fov_radians * 0.5).tan() / width;
    let world_per_pixel_y = 2.0 * depth * (vertical_fov * 0.5).tan() / height;
    let right = scale3(
        camera.right(),
        half_width_pixels.max(0.0) * world_per_pixel_x,
    );
    let up = scale3(camera.up(), half_height_pixels.max(0.0) * world_per_pixel_y);
    // Native sun and flare UVs are ordered left-top, left-bottom, right-bottom,
    // right-top. The shared index order is bottom-left, bottom-right, top-left,
    // top-right.
    let uv = [uv[1], uv[2], uv[0], uv[3]];
    [
        environment_vertex_with_alpha(sub3(sub3(center, right), up), color, alpha, uv[0]),
        environment_vertex_with_alpha(add3(sub3(center, up), right), color, alpha, uv[1]),
        environment_vertex_with_alpha(add3(sub3(center, right), up), color, alpha, uv[2]),
        environment_vertex_with_alpha(add3(add3(center, right), up), color, alpha, uv[3]),
    ]
}

fn screen_tl_quad_vertices_pixels(
    center_pixels: [f32; 2],
    half_width_pixels: f32,
    half_height_pixels: f32,
    viewport: [f32; 2],
    color: [f32; 3],
    alpha: f32,
    uv: [[f32; 2]; 4],
) -> [VulkanStaticVertex; 4] {
    let width = viewport[0].max(1.0);
    let height = viewport[1].max(1.0);
    let half_width = half_width_pixels.max(0.0);
    let half_height = half_height_pixels.max(0.0);
    let pixel_corners = [
        [
            center_pixels[0] - half_width,
            center_pixels[1] + half_height,
        ],
        [
            center_pixels[0] + half_width,
            center_pixels[1] + half_height,
        ],
        [
            center_pixels[0] - half_width,
            center_pixels[1] - half_height,
        ],
        [
            center_pixels[0] + half_width,
            center_pixels[1] - half_height,
        ],
    ];
    // Positive-height Vulkan viewports map NDC -Y to top-down screen pixels.
    let uv = [uv[1], uv[2], uv[0], uv[3]];
    std::array::from_fn(|index| {
        let pixel = pixel_corners[index];
        let position = [
            pixel[0] * (2.0 / width) - 1.0,
            pixel[1] * (2.0 / height) - 1.0,
            0.999,
        ];
        environment_vertex_with_alpha(position, color, alpha, uv[index])
    })
}

fn precipitation_quad_vertices(
    kind: PrecipitationKind,
    screen: &ScreenBillboard,
    color: [f32; 4],
    camera: &PreviewCameraFrame,
    viewport: [f32; 2],
) -> [VulkanStaticVertex; 4] {
    let rgb = [color[0], color[1], color[2]];
    let depths = match kind {
        PrecipitationKind::Rain => [
            screen.tail_depth,
            screen.tail_depth,
            screen.head_depth,
            screen.head_depth,
        ],
        PrecipitationKind::Snow => [screen.head_depth; 4],
    };
    std::array::from_fn(|index| {
        environment_vertex_with_alpha(
            world_from_ndc(camera, screen.corners[index], depths[index], viewport),
            rgb,
            color[3],
            screen.uv[index],
        )
    })
}

fn world_from_ndc(
    camera: &PreviewCameraFrame,
    ndc: [f32; 2],
    depth: f32,
    viewport: [f32; 2],
) -> [f32; 3] {
    let depth = if depth.is_finite() {
        depth.max(camera.near_plane.max(0.001))
    } else {
        camera.near_plane.max(0.001)
    };
    let aspect = viewport[0].max(1.0) / viewport[1].max(1.0);
    let half_vertical = (camera.vertical_fov * 0.5).tan();
    let half_horizontal = half_vertical * aspect;
    let offset = add3(
        scale3(camera.right(), ndc[0] * half_horizontal),
        scale3(camera.up(), ndc[1] * half_vertical),
    );
    add3(
        camera.position,
        scale3(add3(camera.forward(), offset), depth),
    )
}

fn lightning_quad_vertices(
    bolt: &fparkan_fx::environment::LightningBolt,
) -> [VulkanStaticVertex; 4] {
    std::array::from_fn(|index| {
        environment_vertex(bolt.quad.vertices[index], [1.0; 3], bolt.quad.uv[index])
    })
}

/// CPU atmosphere state shared by the interactive preview and the renderer's
/// per-frame light/fog block.  The sky mesh and weather primitives stay on the
/// same update clock, so camera movement cannot leave environment audio or
/// lighting one frame behind.
#[derive(Clone, Debug, PartialEq)]
struct DynamicEnvironmentFrame {
    atmosphere: fparkan_fx::atmosphere::AtmosphereFrame,
    sky_mesh: SkyMesh,
    environment_frame: EnvironmentFrame,
    absolute_time_seconds: f32,
}

fn environment_camera_for_preview(
    camera: &PreviewCameraFrame,
    aspect: f32,
    viewport: [f32; 2],
) -> FxCamera {
    FxCamera::with_projection(
        camera.position,
        camera.forward(),
        camera.right(),
        camera.up(),
        camera.vertical_fov,
        aspect,
    )
    .with_viewport(viewport)
}

struct DynamicEnvironment {
    schedule: TypedAtmosphere,
    sky: SkySystem,
    environment: EnvironmentSystem,
    materials: SkyMaterials,
    material_assets: HashMap<String, EnvironmentMaterialAsset>,
    schedule_offset_seconds: f32,
    time_seconds: f32,
    fixed_schedule_seconds: Option<f32>,
    fixed_phase_reported: bool,
}

impl DynamicEnvironment {
    fn update(
        &mut self,
        dt_seconds: f32,
        camera: &PreviewCameraFrame,
        aspect: f32,
        viewport: [f32; 2],
        renderer: &mut VulkanSmokeRenderer,
        mut audio: Option<&mut audio::GameAudio>,
    ) -> Result<DynamicEnvironmentFrame, String> {
        let dt_seconds = if dt_seconds.is_finite() {
            dt_seconds.max(0.0)
        } else {
            0.0
        };
        let (time_seconds, absolute_time_seconds) = environment_sample_time(
            self.time_seconds,
            self.schedule_offset_seconds,
            dt_seconds,
            self.fixed_schedule_seconds,
        );
        self.time_seconds = time_seconds;
        if let Some(seconds) = self.fixed_schedule_seconds {
            if !self.fixed_phase_reported {
                eprintln!(
                    "CAPTURE_PHASE_SAMPLE absolute_seconds={seconds:.6} schedule_frozen=true"
                );
                self.fixed_phase_reported = true;
            }
        }
        let atmosphere = self.schedule.sample(absolute_time_seconds);
        let mut sky_mesh = self.sky.mesh().clone();
        let sky_frame =
            SkyFrame::from_atmosphere(&atmosphere, &self.materials, absolute_time_seconds);
        sky_mesh.update_colors(&sky_frame.sky);
        if let Some(audio) = audio.as_mut() {
            let primary_rgb = if sky_frame.sun.active {
                sky_frame.sun.color
            } else if sky_frame.moon.active {
                sky_frame.moon.color
            } else {
                [0.0; 3]
            };
            audio
                .update_ambience(dt_seconds, primary_rgb)
                .map_err(|error| format!("update ambient variation: {error}"))?;
        }

        let mut uniforms = renderer.frame_uniforms();
        uniforms.clip_from_world = camera.clip_from_world;
        for (target, source) in uniforms
            .directional_lights
            .iter_mut()
            .zip(sky_frame.directional_lights)
        {
            target.direction = source.direction;
            target.rgb = source.color;
            target.coefficients = [0.0, 0.0, 1.0];
            target.active = source.active;
        }
        uniforms.lighting_floor = sky_frame.lighting_floor;
        let fog_rgb = sky_frame
            .screen_gradient(camera.native_sky_heading(), [0.0; 3])
            .color
            .rgba();
        uniforms.fog_color = [fog_rgb[0], fog_rgb[1], fog_rgb[2]];
        uniforms.fog_start = sky_frame.fog.start.max(0.0);
        uniforms.fog_end = sky_frame.fog.end.max(uniforms.fog_start);
        uniforms.camera_position = camera.position;
        renderer
            .set_frame_uniforms(uniforms)
            .map_err(|error| format!("update sky frame uniforms: {error}"))?;

        let fx_camera = environment_camera_for_preview(camera, aspect, viewport);
        let environment_frame = self.environment.update_atmosphere_with_materials(
            dt_seconds,
            &atmosphere,
            &self.materials,
            fx_camera,
        );
        let mut uniforms = renderer.frame_uniforms();
        uniforms.point_light = environment_frame
            .point_lights
            .iter()
            .find(|light| light.active)
            .map(|light| VulkanPointLight {
                position: light.position,
                rgb: [light.color[0], light.color[1], light.color[2]],
                range: light.range,
                coefficients: light.attenuation,
                active: true,
            })
            .unwrap_or_default();
        renderer
            .set_frame_uniforms(uniforms)
            .map_err(|error| format!("update environment point-light uniforms: {error}"))?;
        if let Some(audio) = audio.as_mut() {
            for sound in &environment_frame.sounds {
                audio
                    .handle_sound_event(sound)
                    .map_err(|error| format!("play environment sound: {error}"))?;
            }
        }
        Ok(DynamicEnvironmentFrame {
            atmosphere,
            sky_mesh,
            environment_frame,
            absolute_time_seconds,
        })
    }
}

fn load_environment(
    root: &Path,
    mission: &str,
    atmosphere_seconds: Option<f32>,
    hold_schedule_phase: bool,
) -> Result<Option<DynamicEnvironment>, String> {
    let mission_dir = mission_asset_directory(root, mission)?;
    let Some(sky_path) = find_mission_file(&mission_dir, "sky.ske")? else {
        return Ok(None);
    };
    let sky_bytes = std::fs::read(&sky_path)
        .map_err(|error| format!("read atmosphere {}: {error}", sky_path.display()))?;
    let schedule = TypedAtmosphere::parse(&sky_bytes)
        .map_err(|error| format!("parse atmosphere {}: {error}", sky_path.display()))?;

    let sky_wea_path = match find_sibling_file(&sky_path, "sky.wea") {
        Some(path) => Some(path),
        None => find_mission_file(&mission_dir, "sky.wea")?,
    }
    .ok_or_else(|| format!("selected atmosphere {} has no sky.wea", sky_path.display()))?;
    let materials = SkyMaterials::parse(
        &std::fs::read(&sky_wea_path)
            .map_err(|error| format!("read sky materials {}: {error}", sky_wea_path.display()))?,
    )
    .map_err(|error| format!("parse sky materials {}: {error}", sky_wea_path.display()))?;
    let mut sky_material_loader = StandaloneWearMaterialLoader::new(root);
    let mut material_assets =
        load_standalone_wear_named_material_textures_rgba8_and_phases_with_documents_from_root(
            root,
            &sky_wea_path,
            0,
        )
        .map_err(|error| {
            format!(
                "resolve sky material WEAR {}: {error}",
                sky_wea_path.display()
            )
        })?
        .into_iter()
        .enumerate()
        .map(
            |(material_index, (name, mips, phase, document))| -> Result<_, String> {
                let initial_phase = phase;
                let mut phase_textures = Vec::with_capacity(document.phases.len());
                let mut phases = Vec::with_capacity(document.phases.len());
                for phase_index in 0..document.phases.len() {
                    let phase_index_u16 = u16::try_from(phase_index).unwrap_or(u16::MAX);
                    let (phase_mips, sampled_phase) = if phase_index == 0 {
                        (mips.clone(), initial_phase)
                    } else {
                        sky_material_loader
                            .load(
                                &sky_wea_path,
                                u16::try_from(material_index).unwrap_or(u16::MAX),
                                phase_index_u16,
                            )
                            .map_err(|error| {
                                format!("resolve sky material phase {phase_index}: {error}")
                            })?
                    };
                    phase_textures.push(vulkan_texture_from_rgba_mips(phase_mips, "sky material")?);
                    phases.push(sampled_phase);
                }
                let texture = phase_textures
                    .first()
                    .cloned()
                    .ok_or_else(|| "sky material has no MAT0 phases".to_string())?;
                Ok((
                    String::from_utf8_lossy(&name).into_owned(),
                    EnvironmentMaterialAsset {
                        texture,
                        phase: initial_phase,
                        document,
                        phase_textures,
                        phases,
                    },
                ))
            },
        )
        .collect::<Result<HashMap<_, _>, String>>()?;
    let effect_bytes = load_resource_entry_bytes_from_root(root, "effects.rlb", "env_lightning")
        .map_err(|error| format!("load effects.rlb/env_lightning: {error}"))?;
    let lightning_effect = decode_env_lightning_fxid(effect_bytes)
        .map_err(|error| format!("decode effects.rlb/env_lightning: {error}"))?;
    if !lightning_effect
        .visual
        .archive
        .eq_ignore_ascii_case("material.lib")
    {
        return Err(format!(
            "env_lightning visual archive must be material.lib, got {:?}",
            lightning_effect.visual.archive
        ));
    }
    let (lightning_mips, lightning_phase, lightning_document) =
        load_material_name_texture_rgba8_and_phase_with_document_from_root(
            root,
            &lightning_effect.visual.name,
            0,
        )
        .map_err(|error| {
            format!(
                "resolve env_lightning MAT0 {}: {error}",
                lightning_effect.visual.name
            )
        })?;
    let mut lightning_phase_textures = vec![vulkan_texture_from_rgba_mips(
        lightning_mips,
        "env_lightning material",
    )?];
    let mut lightning_phases = vec![lightning_phase];
    for phase_index in 1..lightning_document.phases.len() {
        let (phase_mips, phase, _document) =
            load_material_name_texture_rgba8_and_phase_with_document_from_root(
                root,
                &lightning_effect.visual.name,
                u16::try_from(phase_index).unwrap_or(u16::MAX),
            )
            .map_err(|error| {
                format!(
                    "resolve env_lightning MAT0 phase {phase_index} {}: {error}",
                    lightning_effect.visual.name
                )
            })?;
        lightning_phase_textures.push(vulkan_texture_from_rgba_mips(
            phase_mips,
            "env_lightning material",
        )?);
        lightning_phases.push(phase);
    }
    let lightning_texture = lightning_phase_textures
        .first()
        .cloned()
        .ok_or_else(|| "env_lightning MAT0 has no phases".to_string())?;
    material_assets.insert(
        lightning_effect.visual.name.clone(),
        EnvironmentMaterialAsset {
            texture: lightning_texture,
            phase: lightning_phase,
            document: lightning_document,
            phase_textures: lightning_phase_textures,
            phases: lightning_phases,
        },
    );
    let initial_offset_seconds = schedule.initial_offset_seconds() as f32;
    let sky = SkySystem::new(Default::default(), materials.clone())
        .map_err(|error| format!("build sky geometry: {error}"))?;
    let time_seconds = atmosphere_seconds
        .map(|seconds| seconds - initial_offset_seconds)
        .unwrap_or(0.0);
    let fixed_schedule_seconds = if hold_schedule_phase {
        atmosphere_seconds
    } else {
        None
    };
    let mut environment = EnvironmentSystem::new(0x4650_4152_4B41_4E_u64);
    environment.set_lightning_effect(lightning_effect);
    Ok(Some(DynamicEnvironment {
        schedule,
        sky,
        environment,
        materials,
        material_assets,
        schedule_offset_seconds: initial_offset_seconds,
        time_seconds,
        fixed_schedule_seconds,
        fixed_phase_reported: false,
    }))
}

fn environment_sample_time(
    time_seconds: f32,
    schedule_offset_seconds: f32,
    dt_seconds: f32,
    fixed_schedule_seconds: Option<f32>,
) -> (f32, f32) {
    if let Some(absolute_seconds) = fixed_schedule_seconds {
        return (time_seconds, absolute_seconds);
    }

    let next_time_seconds = if time_seconds.is_finite() {
        time_seconds + dt_seconds
    } else {
        0.0
    };
    (
        next_time_seconds,
        schedule_offset_seconds + next_time_seconds,
    )
}

/// Returns the host directory containing the selected mission's assets.
/// Mission keys are the same relative paths accepted by the runtime VFS; the
/// host root is only used here for the small set of loose mission-local files
/// that are not archive entries (`sky.ske` and `sky.wea`).
fn mission_asset_directory(root: &Path, mission: &str) -> Result<PathBuf, String> {
    let mission_path = Path::new(mission);
    if mission_path.is_absolute() {
        return Err("mission environment path must be relative to --root".to_string());
    }
    let directory = mission_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let directory = root.join(directory);
    if !directory.is_dir() {
        return Err(format!(
            "selected mission asset directory does not exist: {}",
            directory.display()
        ));
    }
    Ok(directory)
}

/// Finds one mission-local loose environment file.  A direct sibling wins;
/// nested `env`, `environment`, or `planet` layouts are accepted only when
/// they contain a single matching file, so a second unrelated candidate never
/// changes the selected mission silently.
fn find_mission_file(root: &Path, name: &str) -> Result<Option<PathBuf>, String> {
    let direct = root.join(name);
    if direct.is_file() {
        return Ok(Some(direct));
    }
    let mut stack = vec![root.to_path_buf()];
    let mut matches = Vec::new();
    while let Some(directory) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file()
                && path
                    .file_name()
                    .is_some_and(|file_name| file_name.eq_ignore_ascii_case(name))
            {
                matches.push(path);
            }
        }
    }
    matches.sort();
    match matches.as_slice() {
        [] => Ok(None),
        [path] => Ok(Some(path.clone())),
        _ => Err(format!(
            "selected mission has multiple {name} files: {}",
            matches
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn find_sibling_file(source: &Path, name: &str) -> Option<PathBuf> {
    source
        .parent()
        .map(|parent| parent.join(name))
        .filter(|path| path.is_file())
}

fn add3(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] + right[0], left[1] + right[1], left[2] + right[2]]
}

fn add3_in_place(target: &mut [f32; 3], value: [f32; 3]) {
    *target = add3(*target, value);
}

fn sub3(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

fn scale3(value: [f32; 3], factor: f32) -> [f32; 3] {
    [value[0] * factor, value[1] * factor, value[2] * factor]
}

fn dot3(left: [f32; 3], right: [f32; 3]) -> f32 {
    left[0].mul_add(right[0], left[1].mul_add(right[1], left[2] * right[2]))
}

fn cross3(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

fn length3(value: [f32; 3]) -> Option<f32> {
    let length = dot3(value, value).sqrt();
    length
        .is_finite()
        .then_some(length)
        .filter(|length| *length > f32::EPSILON)
}

fn normalize3(value: [f32; 3]) -> Option<[f32; 3]> {
    let inverse = length3(value)?.recip();
    Some(scale3(value, inverse))
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

struct StaticPreviewScene {
    mesh: VulkanStaticMesh,
    native_handedness_by_range: HashMap<usize, i8>,
    materials: Vec<VulkanStaticMaterial>,
    material_animations: Vec<MaterialAnimationBinding>,
    sun_occlusion_range_indices: Vec<usize>,
    shadow_casters: Vec<ShadowCaster>,
    shadow_sphere_baselines: Vec<ShadowSphere>,
    shadow_receivers: Vec<ShadowTriangle>,
    shadow_material_index: usize,
    terrain: Arc<TerrainWorld>,
    component_pose_buffers: Vec<Vec<Option<NodePoseBuffer>>>,
    component_mount_poses: Vec<Vec<Option<VulkanNodePose>>>,
    control_animation_instances: Vec<ControlAnimationInstance>,
    dynamic_component_geometry: Vec<DynamicComponentGeometry>,
    dynamic_component_mounts: Vec<DynamicComponentMount>,
    shadow_caster_indices_by_object: Vec<Option<usize>>,
    camera: VulkanStaticCamera,
    view_camera: PreviewCameraFrame,
    captured_viewport: Option<CapturedViewport>,
    free_camera: Option<FreeFlightCamera>,
    camera_mode: &'static str,
    mesh_components: usize,
    terrain_components: usize,
}

#[derive(Clone, Debug)]
struct ControlAnimationInstance {
    object_index: usize,
    component_index: usize,
    model: Arc<ModelAsset>,
    control: PreparedComponentControl,
    state: ControlInstanceState,
}

#[derive(Clone, Debug)]
struct DynamicComponentGeometry {
    object_index: usize,
    component_index: usize,
    model: Arc<ModelAsset>,
    transform: LegacyIron3dEulerTransform,
    scale: [f32; 3],
    vertex_start: usize,
    vertex_count: usize,
    shadow_triangle_range: Option<std::ops::Range<usize>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DynamicComponentMount {
    object_index: usize,
    component_index: usize,
    parent_index: usize,
    socket: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct CapturedViewport {
    /// Original D3D7 viewport rectangle `(left, top, right, bottom)`. The
    /// preview window renders `extent` and should be compared with this crop.
    rect: [i32; 4],
    extent: [u32; 2],
}

fn validate_captured_drawable_extent(
    captured: CapturedViewport,
    actual: [u32; 2],
) -> Result<(), String> {
    if actual == captured.extent {
        return Ok(());
    }
    Err(format!(
        "legacy camera viewport crop {:?} requires a {}x{} drawable, but the window provides {}x{}; refusing to compare a stretched render",
        captured.rect, captured.extent[0], captured.extent[1], actual[0], actual[1]
    ))
}

#[derive(Clone, Copy, Debug, Default)]
struct StartupTimings {
    mission_assets: Duration,
    terrain_materials: Duration,
    sky: Duration,
    gpu_initialization: Duration,
}

#[derive(Clone, Copy, Debug, Default)]
struct ShadowFrameEvidence {
    visible_casters: usize,
    projected_indices: usize,
}

fn elapsed_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn unit_component_is_visible(index: usize, source_type: Option<u32>) -> bool {
    index == 0 || source_type == Some(PROTOTYPE_TYPE_EXTO)
}

fn native_sun_object_kind_eligible(source_type: Option<u32>) -> bool {
    native_shadow_kind(source_type).is_some()
}

const PROTOTYPE_TYPE_FORT: u32 = u32::from_le_bytes(*b"FORT");
const PROTOTYPE_TYPE_BTLU: u32 = u32::from_le_bytes(*b"BTLU");
const PROTOTYPE_TYPE_STAT: u32 = u32::from_le_bytes(*b"STAT");

// shortcut: the preview does not load shade.cfg yet, so use CShade's native
// defaults until the settings loader is implemented.
const NATIVE_DEFAULT_ROB_SHADOWS_ON: bool = true;
const NATIVE_DEFAULT_BLD_SHADOWS_ON: bool = false;

/// Maps the resolved registry type to the native object-kind selector used by
/// CSun/CShade.  The mission object's raw kind is an unrelated TMA field; the
/// renderer receives the registry provenance from the prepared visual.
fn native_shadow_kind(source_type: Option<u32>) -> Option<u32> {
    match source_type {
        Some(PROTOTYPE_TYPE_FORT) => Some(3),
        Some(PROTOTYPE_TYPE_BTLU) => Some(4),
        Some(PROTOTYPE_TYPE_STAT) => Some(10),
        _ => None,
    }
}

fn native_shadow_caster_enabled(native_kind: u32) -> bool {
    if native_kind == 4 {
        NATIVE_DEFAULT_ROB_SHADOWS_ON
    } else {
        NATIVE_DEFAULT_BLD_SHADOWS_ON
    }
}

fn compose_unit_pose(parent: VulkanNodePose, child: VulkanNodePose) -> VulkanNodePose {
    let rotated = rotate_unit_vector(child.translation, parent.rotation);
    VulkanNodePose {
        translation: [
            parent.translation[0] + rotated[0],
            parent.translation[1] + rotated[1],
            parent.translation[2] + rotated[2],
        ],
        rotation: multiply_unit_quaternions(parent.rotation, child.rotation),
    }
}

fn rotate_unit_vector(value: [f32; 3], rotation: [f32; 4]) -> [f32; 3] {
    let [x, y, z, w] = rotation;
    let tx = 2.0 * (y * value[2] - z * value[1]);
    let ty = 2.0 * (z * value[0] - x * value[2]);
    let tz = 2.0 * (x * value[1] - y * value[0]);
    [
        value[0] + w * tx + (y * tz - z * ty),
        value[1] + w * ty + (z * tx - x * tz),
        value[2] + w * tz + (x * ty - y * tx),
    ]
}

fn multiply_unit_quaternions(left: [f32; 4], right: [f32; 4]) -> [f32; 4] {
    let [lx, ly, lz, lw] = left;
    let [rx, ry, rz, rw] = right;
    [
        lw * rx + lx * rw + ly * rz - lz * ry,
        lw * ry - lx * rz + ly * rw + lz * rx,
        lw * rz + lx * ry - ly * rx + lz * rw,
        lw * rw - lx * rx - ly * ry - lz * rz,
    ]
}

/// Projects the mission terrain plus every MSH component of the selected roots
/// into one shared world-space mesh.
///
/// Terrain resolves its packed low-byte base and high-byte overlay through
/// map-local `Land1.wea`, then pairs each layer with its `Land2.wea`
/// microtexture and type-18 UV stream. Type-14 alpha remains a continuous
/// interpolated overlay mask. MSH component hierarchy, animation sampling,
/// lightmaps, and gameplay visibility are handled by the preview bridge below.

fn static_preview_node_times(
    node_count: usize,
    bindings: &[PreparedControlNodeBinding],
) -> Result<Vec<Option<AnimationTime>>, String> {
    let mut node_times = vec![Some(AnimationTime(0.0)); node_count];
    for binding in bindings {
        // Disabled rows are filtered while loading CTLD, but preserve the
        // native guard here in case a caller constructs MissionAssets itself.
        if binding.flags & 4 != 0 {
            continue;
        }
        if binding.node_index >= node_count {
            return Err(format!(
                "control binding node {} is outside model node count {node_count}",
                binding.node_index
            ));
        }
        if !binding.frame_a.is_finite()
            || !binding.frame_b.is_finite()
            || !binding.initial_blend.is_finite()
            || binding.frame_a < 0.0
            || binding.frame_b < 0.0
        {
            return Err(format!(
                "control binding for node {} contains an invalid frame or blend",
                binding.node_index
            ));
        }

        let mut blend = binding.initial_blend;
        if binding.flags & 1 != 0 {
            if blend > 1.0 {
                blend -= 1.0;
            }
            if blend < 0.0 {
                blend += 1.0;
            }
        } else {
            blend = blend.clamp(0.0, 1.0);
        }
        if binding.flags & 2 != 0 {
            blend = 1.0 - blend;
        }
        let sample_time = (1.0 - blend) * binding.frame_a + blend * binding.frame_b;
        if !sample_time.is_finite() || sample_time < 0.0 {
            return Err(format!(
                "control binding for node {} produced a negative or non-finite sample time unsupported by the static sampler",
                binding.node_index
            ));
        }
        // Source order is significant: native initialization writes each
        // enabled row in turn, so a later row for the same node wins.
        node_times[binding.node_index] = Some(AnimationTime(sample_time));
    }
    Ok(node_times)
}

fn static_preview_component_pose_buffer(
    model: &ModelAsset,
    bindings: &[PreparedControlNodeBinding],
    static_animation_frame: Option<u16>,
) -> Result<Option<NodePoseBuffer>, String> {
    if let Some(frame) = static_animation_frame {
        return Ok(node38_sampled_hierarchy(model, frame));
    }
    let node_times = static_preview_node_times(model.node_count, bindings)?;
    Ok(node38_sampled_hierarchy_at_times(model, &node_times)
        .or_else(|| node38_fallback_hierarchy(model)))
}

fn initial_control_gate_values(control: &PreparedComponentControl) -> Vec<Option<f32>> {
    control
        .typed_records
        .iter()
        .filter(|record| record.tag == 21)
        .map(|record| match &record.gate_provider {
            PreparedControlGateProvider::Resolved {
                initial_type1_value,
                ..
            } if initial_type1_value.is_finite() => Some(*initial_type1_value),
            _ => None,
        })
        .collect()
}

fn control_has_tag21(control: &PreparedComponentControl) -> bool {
    control.typed_records.iter().any(|record| record.tag == 21)
}

fn component_chain_has_tag21(
    tree: Option<&[UnitComponentTreeNode]>,
    controls: &[Option<PreparedComponentControl>],
    component_index: usize,
) -> bool {
    let Some(tree) = tree else {
        return false;
    };
    let mut current = Some(component_index);
    for _ in 0..=tree.len() {
        let Some(index) = current else {
            return false;
        };
        if controls
            .get(index)
            .and_then(Option::as_ref)
            .is_some_and(control_has_tag21)
        {
            return true;
        }
        current = tree.get(index).and_then(|node| node.parent_index);
    }
    false
}

fn refresh_dynamic_component_mounts(
    mounts: &[DynamicComponentMount],
    pose_buffers: &[Vec<Option<NodePoseBuffer>>],
    mount_poses: &mut [Vec<Option<VulkanNodePose>>],
) -> Result<bool, String> {
    let mut changed = false;
    for mount in mounts {
        let parent_mounted = mount_poses
            .get(mount.object_index)
            .and_then(|poses| poses.get(mount.parent_index))
            .copied()
            .flatten();
        let parent_hierarchy = pose_buffers
            .get(mount.object_index)
            .and_then(|buffers| buffers.get(mount.parent_index))
            .and_then(Option::as_ref)
            .ok_or_else(|| {
                format!(
                    "dynamic component {} parent {} has no sampled Node38 hierarchy",
                    mount.component_index, mount.parent_index
                )
            })?;
        let socket_pose = if parent_mounted.is_some() {
            node38_pose_relative_to_root_from_hierarchy(parent_hierarchy, mount.socket)
        } else {
            node38_pose_from_hierarchy(parent_hierarchy, mount.socket)
        }
        .ok_or_else(|| {
            format!(
                "dynamic component {} mount socket {} is absent from parent {}",
                mount.component_index, mount.socket, mount.parent_index
            )
        })?;
        let root_pose = parent_mounted.unwrap_or(VulkanNodePose {
            translation: [0.0; 3],
            rotation: [0.0, 0.0, 0.0, 1.0],
        });
        let next_pose = compose_unit_pose(root_pose, socket_pose);
        let slot = mount_poses
            .get_mut(mount.object_index)
            .and_then(|poses| poses.get_mut(mount.component_index))
            .ok_or_else(|| {
                format!(
                    "dynamic component {} mount slot is outside object {}",
                    mount.component_index, mount.object_index
                )
            })?;
        if *slot != Some(next_pose) {
            *slot = Some(next_pose);
            changed = true;
        }
    }
    Ok(changed)
}

fn static_preview_control_pose_buffer(
    model: &ModelAsset,
    control: &PreparedComponentControl,
    native_clock_ms: u32,
    state: &mut ControlInstanceState,
) -> Result<Option<NodePoseBuffer>, String> {
    if control.model_node_count != model.node_count {
        return Err(format!(
            "control model node count {} does not match MSH node count {}",
            control.model_node_count, model.node_count
        ));
    }
    let gate_values = initial_control_gate_values(control);
    let sampled_times = match sample_control_node_times(
        control,
        native_clock_ms,
        &gate_values,
        state,
    ) {
        Ok(times) => times,
        Err(
            error @ (ControlSamplingError::UnsupportedTag21Mode { .. }
            | ControlSamplingError::UnsupportedVariableMapping { .. }
            | ControlSamplingError::MissingGateProvider { .. }
            | ControlSamplingError::GateProviderKeyOutOfRange { .. }
            | ControlSamplingError::MissingGateProviderValue { .. }
            | ControlSamplingError::GateProviderCountMismatch { .. }),
        ) => {
            return static_preview_component_pose_buffer(model, &control.bindings, None)
                .map_err(|fallback| {
                    format!(
                        "control timeline sample failed ({error:?}) and static CTLD fallback failed: {fallback}"
                    )
                });
        }
        Err(error) => {
            return Err(format!("control timeline sample failed: {error:?}"));
        }
    };
    let node_times = sampled_times
        .into_iter()
        .map(|time| time.map(AnimationTime))
        .collect::<Vec<_>>();
    Ok(node38_sampled_hierarchy_at_times(model, &node_times)
        .or_else(|| node38_fallback_hierarchy(model)))
}

fn project_dynamic_component_geometry(
    dynamic: &DynamicComponentGeometry,
    pose: &NodePoseBuffer,
    mount: Option<VulkanNodePose>,
) -> Result<VulkanStaticMesh, String> {
    let projected = if let Some(mount) = mount {
        project_msh_to_static_mesh_in_world_space_with_node_pose_buffer_and_mount(
            &dynamic.model,
            dynamic.transform,
            dynamic.scale,
            pose,
            mount,
        )
    } else {
        project_msh_to_static_mesh_in_world_space_with_node_pose_buffer(
            &dynamic.model,
            dynamic.transform,
            dynamic.scale,
            pose,
        )
    };
    projected.map_err(|error| {
        format!(
            "update mission object {} component {} geometry: {error}",
            dynamic.object_index, dynamic.component_index
        )
    })
}

fn static_preview_mesh_and_materials(
    assets: &MissionAssets,
    terrain: &TerrainWorld,
    roots: &[MissionObjectDraft],
    legacy_camera: Option<LegacyCameraCapture>,
    static_animation_frame: Option<u16>,
    static_material_phase: Option<u16>,
    root: &std::path::Path,
    land_msh_path: &str,
) -> Result<StaticPreviewScene, String> {
    let terrain_mesh = terrain
        .source_mesh()
        .ok_or_else(|| "runtime terrain does not retain its validated source mesh".to_string())?;
    let mut mesh = VulkanStaticMesh {
        vertices: Vec::new(),
        indices: Vec::new(),
        draw_ranges: Vec::new(),
    };
    let mut native_handedness_by_range = HashMap::new();
    let mut materials = Vec::new();
    let mut material_animations = Vec::new();
    let mut sun_occlusion_range_indices = Vec::new();
    let mut shadow_casters = Vec::new();
    let mut shadow_sphere_baselines = Vec::new();
    let mut shadow_caster_indices_by_object = Vec::with_capacity(roots.len());
    let mut component_pose_buffers_by_object = Vec::with_capacity(roots.len());
    let mut component_mount_poses_by_object = Vec::with_capacity(roots.len());
    let mut control_animation_instances = Vec::new();
    let mut dynamic_component_geometry = Vec::new();
    let mut dynamic_component_mounts = Vec::new();
    let shadow_receivers = terrain_shadow_receivers(terrain_mesh)?;
    let mut terrain_material_loader = StandaloneWearMaterialLoader::new(root);
    let terrain_component = project_land_msh_to_static_mesh_in_world_space(terrain_mesh)
        .map_err(|err| format!("project mission terrain for Vulkan: {err}"))?;
    let terrain_materials = static_preview_terrain_base_materials(
        &mut terrain_material_loader,
        root,
        land_msh_path,
        &terrain_component,
        &mut materials,
        &mut material_animations,
        static_material_phase,
    )?;
    append_static_preview_component(
        &mut mesh,
        terrain_component,
        &terrain_materials,
        Some(&mut material_animations),
    )?;
    let mut mesh_components = 0;
    for (object_index, mission_root) in roots.iter().enumerate() {
        let visual_ids = assets.visuals_for_object(object_index);
        let records = &mission_root.unit_components;
        let tree = if records.is_empty() {
            None
        } else {
            Some(unit_component_tree(records).map_err(|error| {
                format!("decode unit component tree for mission object {object_index}: {error}")
            })?)
        };
        if let Some(tree) = tree.as_ref() {
            if tree.len() != visual_ids.len() {
                return Err(format!(
                    "mission object {object_index} unit tree has {} records but {} visuals",
                    tree.len(),
                    visual_ids.len()
                ));
            }
        }
        let component_refs = visual_ids
            .iter()
            .map(|visual_id| {
                let visual = assets.visual_by_id(*visual_id).ok_or_else(|| {
                    format!(
                        "static preview root {object_index} references unknown visual {visual_id:?}"
                    )
                })?;
                let model = visual
                    .model_id
                    .map(|model_id| {
                        assets.model_by_id(model_id).ok_or_else(|| {
                            format!(
                                "static preview visual {visual_id:?} references unknown model {model_id:?}"
                            )
                        })
                    })
                    .transpose()?;
                Ok((visual, model))
            })
            .collect::<Result<Vec<_>, String>>()?;
        let component_controls = assets.object_component_controls.get(object_index);
        if component_controls.is_some_and(|controls| controls.len() != component_refs.len()) {
            return Err(format!(
                "mission object {object_index} has {} visuals but {} component controls",
                component_refs.len(),
                component_controls.map_or(0, Vec::len)
            ));
        }
        let component_controls = component_controls.map_or(&[][..], Vec::as_slice);
        let root_native_kind = component_refs
            .first()
            .and_then(|(visual, _)| native_shadow_kind(visual.source_type));
        let root_sun_eligible = native_sun_object_kind_eligible(
            component_refs
                .first()
                .and_then(|(visual, _)| visual.source_type),
        );
        let mut root_shadow_triangles = Vec::new();
        let mut root_shadow_spheres = Vec::new();
        let mut component_mount_poses = vec![None; component_refs.len()];
        let mut component_pose_buffers = vec![None; component_refs.len()];
        for (component_index, (visual, model)) in component_refs.iter().enumerate() {
            let Some(model) = *model else {
                continue;
            };
            let component_control = component_controls
                .get(component_index)
                .and_then(Option::as_ref);
            let control_animates_component = component_control.is_some_and(control_has_tag21);
            let timeline_moves_component = static_animation_frame.is_none()
                && (control_animates_component
                    || component_chain_has_tag21(
                        tree.as_deref(),
                        component_controls,
                        component_index,
                    ));
            let runtime_model = timeline_moves_component.then(|| Arc::new(model.validated.clone()));
            let mut control_state = ControlInstanceState::default();
            component_pose_buffers[component_index] =
                match (static_animation_frame, component_control) {
                    (None, Some(control)) => static_preview_control_pose_buffer(
                        &model.validated,
                        control,
                        0,
                        &mut control_state,
                    )?,
                    (animation_frame, control) => static_preview_component_pose_buffer(
                        &model.validated,
                        control.map_or(&[][..], |control| control.bindings.as_slice()),
                        animation_frame,
                    )?,
                };
            if control_animates_component && static_animation_frame.is_none() {
                control_animation_instances.push(ControlAnimationInstance {
                    object_index,
                    component_index,
                    model: runtime_model
                        .as_ref()
                        .map(Arc::clone)
                        .ok_or_else(|| "animated component model was not retained".to_string())?,
                    control: component_control
                        .ok_or_else(|| "tag-21 component control disappeared".to_string())?
                        .clone(),
                    state: control_state,
                });
            }
            let should_render = tree.as_ref().map_or(true, |_| {
                unit_component_is_visible(component_index, visual.source_type)
            });
            // Native AniMesh only resolves global MSH node sockets for EXTO
            // geometry. Internal INTO components use controller/equipment
            // slots, whose `parent_or_link` values are not MSH node indices.
            // Do not ask the MSH hierarchy to resolve hidden internal parts.
            let mount_pose = if should_render {
                if let Some(tree) = tree.as_ref() {
                    let record = records.get(component_index).ok_or_else(|| {
                        format!("unit component {component_index} is outside decoded records")
                    })?;
                    let parent_index = tree[component_index].parent_index;
                    match parent_index {
                        None => None,
                        Some(parent_index) => {
                            let parent_visible = component_refs.get(parent_index).is_some_and(
                                |(parent_visual, _)| {
                                    unit_component_is_visible(
                                        parent_index,
                                        parent_visual.source_type,
                                    )
                                },
                            );
                            if !parent_visible {
                                None
                            } else {
                                let parent_model = component_refs
                                    .get(parent_index)
                                    .and_then(|(_, model)| *model)
                                    .ok_or_else(|| {
                                        format!(
                                            "unit component {component_index} parent {parent_index} has no model for mount socket"
                                        )
                                    })?;
                                let parent_hierarchy = component_pose_buffers
                                    .get(parent_index)
                                    .and_then(Option::as_ref);
                                let parent_is_mounted =
                                    component_mount_poses[parent_index].is_some();
                                let socket =
                                    usize::try_from(record.parent_or_link).map_err(|_| {
                                        format!(
                                            "unit component {component_index} has negative mount socket {}",
                                            record.parent_or_link
                                        )
                                    })?;
                                let parent_socket = match parent_hierarchy {
                                    Some(hierarchy) if parent_is_mounted => {
                                        node38_pose_relative_to_root_from_hierarchy(
                                            hierarchy, socket,
                                        )
                                    }
                                    Some(hierarchy) => {
                                        node38_pose_from_hierarchy(hierarchy, socket)
                                    }
                                    None if parent_is_mounted => node38_pose_relative_to_root(
                                        &parent_model.validated,
                                        static_animation_frame,
                                        socket,
                                    ),
                                    None => node38_pose(
                                        &parent_model.validated,
                                        static_animation_frame,
                                        socket,
                                    ),
                                }
                                .ok_or_else(|| {
                                        format!(
                                            "unit component {component_index} mount socket {socket} is absent from parent {parent_index} (parent source={:?}, node_count={}, slots={}, animation={})",
                                            parent_model.source,
                                            parent_model.validated.node_count,
                                            parent_model.validated.slots.len(),
                                            parent_model.validated.animation.is_some(),
                                        )
                                    })?;
                                let parent_root_pose = component_mount_poses[parent_index]
                                    .unwrap_or(VulkanNodePose {
                                        translation: [0.0; 3],
                                        rotation: [0.0, 0.0, 0.0, 1.0],
                                    });
                                Some(compose_unit_pose(parent_root_pose, parent_socket))
                            }
                        }
                    }
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(mount_pose) = mount_pose {
                component_mount_poses[component_index] = Some(mount_pose);
            }
            if timeline_moves_component && mount_pose.is_some() {
                let parent_index = tree
                    .as_ref()
                    .and_then(|tree| tree[component_index].parent_index)
                    .ok_or_else(|| {
                        format!("mounted dynamic component {component_index} has no parent")
                    })?;
                let socket = usize::try_from(
                    records
                        .get(component_index)
                        .ok_or_else(|| {
                            format!("unit component {component_index} is outside decoded records")
                        })?
                        .parent_or_link,
                )
                .map_err(|_| {
                    format!("unit component {component_index} has a negative mount socket")
                })?;
                dynamic_component_mounts.push(DynamicComponentMount {
                    object_index,
                    component_index,
                    parent_index,
                    socket,
                });
            }

            let transform = LegacyIron3dEulerTransform {
                translation: mission_root.position,
                orientation_radians: mission_root.orientation_raw,
            };
            if should_render {
                let pose_buffer = component_pose_buffers[component_index].as_ref();
                let mut component = match (pose_buffer, mount_pose, static_animation_frame) {
                    (Some(poses), Some(mount), _) => {
                        project_msh_to_static_mesh_in_world_space_with_node_pose_buffer_and_mount(
                            &model.validated,
                            transform,
                            mission_root.scale,
                            poses,
                            mount,
                        )
                    }
                    (Some(poses), None, _) => {
                        project_msh_to_static_mesh_in_world_space_with_node_pose_buffer(
                            &model.validated,
                            transform,
                            mission_root.scale,
                            poses,
                        )
                    }
                    (None, Some(mount), Some(frame)) => {
                        project_msh_to_static_mesh_in_world_space_with_node_sampled_poses_and_mount(
                            &model.validated,
                            transform,
                            mission_root.scale,
                            frame,
                            mount,
                        )
                    }
                    (None, Some(mount), None) => {
                        project_msh_to_static_mesh_in_world_space_with_node_fallback_poses_and_mount(
                            &model.validated,
                            transform,
                            mission_root.scale,
                            mount,
                        )
                    }
                    (None, None, Some(frame)) => {
                        project_msh_to_static_mesh_in_world_space_with_node_sampled_poses(
                            &model.validated,
                            transform,
                            mission_root.scale,
                            frame,
                        )
                    }
                    (None, None, None) => {
                        project_msh_to_static_mesh_in_world_space_with_node_fallback_poses(
                            &model.validated,
                            transform,
                            mission_root.scale,
                        )
                    }
                }
                .map_err(|err| format!("project mission MSH for Vulkan: {err}"))?;
                let component_handedness = native_msh_handedness(transform, mission_root.scale);
                for range in &mut component.draw_ranges {
                    range.pipeline_state.cull =
                        native_msh_cull_mode(range.batch_flags, component_handedness);
                }
                let component_shadow_triangles = static_mesh_shadow_triangles(&component);
                let shadow_triangle_range = root_native_kind.map(|_| {
                    let start = root_shadow_triangles.len();
                    let end = start.saturating_add(component_shadow_triangles.len());
                    start..end
                });
                if root_native_kind.is_some() {
                    root_shadow_triangles.extend(component_shadow_triangles);
                    let node1_pose = mount_pose.and_then(|_| match pose_buffer {
                        Some(poses) => node38_pose_relative_to_root_from_hierarchy(poses, 1),
                        None => node38_pose_relative_to_root(
                            &model.validated,
                            static_animation_frame,
                            1,
                        ),
                    });
                    if let Some(sphere) = mounted_shadow_sphere(
                        model.validated.bounding_sphere_center,
                        model.validated.bounding_sphere_radius,
                        mount_pose,
                        node1_pose,
                    ) {
                        root_shadow_spheres.push(sphere);
                    }
                }
                let selector_remap = static_preview_component_materials(
                    assets,
                    visual,
                    &component,
                    static_material_phase,
                    &mut materials,
                    &mut material_animations,
                )?;
                let lightmap_textures = static_preview_lightmap_textures(assets, visual)?;
                let vertex_start = mesh.vertices.len();
                let vertex_count = component.vertices.len();
                let range_start = mesh.draw_ranges.len();
                append_static_preview_component_with_lightmaps(
                    &mut mesh,
                    component,
                    &selector_remap,
                    Some(&mut materials),
                    &lightmap_textures,
                    Some(&mut material_animations),
                )?;
                for range_index in range_start..mesh.draw_ranges.len() {
                    native_handedness_by_range.insert(range_index, component_handedness);
                }
                if root_sun_eligible {
                    sun_occlusion_range_indices.extend(range_start..mesh.draw_ranges.len());
                }
                if timeline_moves_component && vertex_count != 0 {
                    dynamic_component_geometry.push(DynamicComponentGeometry {
                        object_index,
                        component_index,
                        model: runtime_model.as_ref().map(Arc::clone).ok_or_else(|| {
                            "animated component model was not retained".to_string()
                        })?,
                        transform,
                        scale: mission_root.scale,
                        vertex_start,
                        vertex_count,
                        shadow_triangle_range,
                    });
                }
                mesh_components += 1;
            }
        }
        component_pose_buffers_by_object.push(component_pose_buffers);
        component_mount_poses_by_object.push(component_mount_poses);
        let shadow_caster_index = if let Some(native_kind) =
            root_native_kind.filter(|kind| native_shadow_caster_enabled(*kind))
        {
            let caster_id = u32::try_from(shadow_casters.len())
                .map_err(|_| "shadow caster id exceeds u32".to_string())?;
            aggregate_shadow_caster(
                caster_id,
                native_kind,
                LegacyIron3dEulerTransform {
                    translation: mission_root.position,
                    orientation_radians: mission_root.orientation_raw,
                },
                mission_root.scale,
                root_shadow_triangles,
                &root_shadow_spheres,
            )
            .map(|caster| {
                let index = shadow_casters.len();
                shadow_sphere_baselines.push(caster.sphere);
                shadow_casters.push(caster);
                index
            })
        } else {
            None
        };
        shadow_caster_indices_by_object.push(shadow_caster_index);
    }
    if mesh_components == 0 {
        return Err("selected static preview roots have no mesh-backed visual".to_string());
    }
    let free_camera = if legacy_camera.is_none() {
        Some(FreeFlightCamera::from_mesh_and_terrain(&mesh, terrain)?)
    } else {
        None
    };
    let captured_viewport = legacy_camera
        .as_ref()
        .map(LegacyCameraCapture::captured_viewport)
        .transpose()?;
    let view_camera = if let Some(capture) = legacy_camera.as_ref() {
        capture.preview_camera_frame()?
    } else {
        free_camera
            .map(|camera| camera.preview_frame(16.0 / 9.0))
            .ok_or_else(|| "static preview camera is unavailable".to_string())?
    };
    let camera = view_camera.vulkan_camera();
    let shadow_material_index = append_shadow_material(&mut materials)?;
    Ok(StaticPreviewScene {
        mesh,
        native_handedness_by_range,
        materials,
        material_animations,
        sun_occlusion_range_indices,
        shadow_casters,
        shadow_sphere_baselines,
        shadow_receivers,
        shadow_material_index,
        terrain: Arc::new(terrain.clone()),
        component_pose_buffers: component_pose_buffers_by_object,
        component_mount_poses: component_mount_poses_by_object,
        control_animation_instances,
        dynamic_component_geometry,
        dynamic_component_mounts,
        shadow_caster_indices_by_object,
        camera,
        view_camera,
        captured_viewport,
        free_camera,
        camera_mode: if legacy_camera.is_some() {
            "legacy-d3d7-capture"
        } else {
            "free-flight"
        },
        mesh_components,
        terrain_components: 1,
    })
}

fn static_preview_terrain_base_materials(
    loader: &mut StandaloneWearMaterialLoader,
    root: &std::path::Path,
    land_msh_path: &str,
    mesh: &VulkanStaticMesh,
    materials: &mut Vec<VulkanStaticMaterial>,
    material_animations: &mut Vec<MaterialAnimationBinding>,
    static_material_phase: Option<u16>,
) -> Result<Vec<(u16, u16)>, String> {
    let land_path = root.join(land_msh_path);
    let land_parent = land_path
        .parent()
        .ok_or_else(|| format!("terrain mesh has no parent path: {}", land_path.display()))?;
    let land1_path = land_parent.join("Land1.wea");
    let land2_path = land_parent.join("Land2.wea");
    let mut packed_tags = mesh
        .draw_ranges
        .iter()
        .map(|range| range.material_index)
        .collect::<Vec<_>>();
    packed_tags.sort_unstable();
    packed_tags.dedup();
    packed_tags
        .into_iter()
        .map(|packed_tag| {
            let layers = TerrainMaterialLayers::from_packed_tag(packed_tag);
            let base_document = loader
                .material_document(&land1_path, layers.base_selection().material_index)
                .map_err(|err| format!("resolve terrain base MAT0 document: {err}"))?;
            let detail_document = loader
                .material_document(&land2_path, layers.detail_base_selection().material_index)
                .map_err(|err| format!("resolve terrain detail MAT0 document: {err}"))?;
            let overlay_selection = layers.overlay_selection();
            let overlay_detail_selection = layers.detail_overlay_selection();
            let overlay_document = overlay_selection
                .map(|selection| loader.material_document(&land1_path, selection.material_index))
                .transpose()
                .map_err(|err| format!("resolve terrain overlay MAT0 document: {err}"))?;
            let overlay_detail_document = overlay_detail_selection
                .map(|selection| loader.material_document(&land2_path, selection.material_index))
                .transpose()
                .map_err(|err| format!("resolve terrain overlay detail MAT0 document: {err}"))?;
            let base_phases = load_standalone_phase_set(
                loader,
                &land1_path,
                layers.base_selection().material_index,
                &base_document,
            )?;
            let detail_phases = load_standalone_phase_set(
                loader,
                &land2_path,
                layers.detail_base_selection().material_index,
                &detail_document,
            )?;
            let overlay_phases = overlay_selection
                .map(|selection| {
                    load_standalone_phase_set(
                        loader,
                        &land1_path,
                        selection.material_index,
                        overlay_document.as_ref().expect("overlay document"),
                    )
                })
                .transpose()?;
            let overlay_detail_phases = overlay_detail_selection
                .map(|selection| {
                    load_standalone_phase_set(
                        loader,
                        &land2_path,
                        selection.material_index,
                        overlay_detail_document
                            .as_ref()
                            .expect("overlay detail document"),
                    )
                })
                .transpose()?;
            let phase_count = base_phases.len();
            if phase_count == 0 || detail_phases.is_empty() {
                return Err(format!(
                    "terrain material tag 0x{packed_tag:04x} has incomplete phase tables"
                ));
            }
            if overlay_phases.as_ref().is_some_and(Vec::is_empty)
                || overlay_detail_phases.as_ref().is_some_and(Vec::is_empty)
            {
                return Err(format!(
                    "terrain material tag 0x{packed_tag:04x} has an empty optional phase table"
                ));
            }
            let initial_phase_index =
                usize::from(static_material_phase.unwrap_or(0)).min(phase_count.saturating_sub(1));
            let initial_detail_phase_index = usize::from(static_material_phase.unwrap_or(0))
                .min(detail_phases.len().saturating_sub(1));
            let initial_overlay_phase_index = overlay_phases.as_ref().map(|phases| {
                usize::from(static_material_phase.unwrap_or(0)).min(phases.len().saturating_sub(1))
            });
            let initial_overlay_detail_phase_index = overlay_detail_phases.as_ref().map(|phases| {
                usize::from(static_material_phase.unwrap_or(0)).min(phases.len().saturating_sub(1))
            });
            let mut phase_material_indices = Vec::with_capacity(phase_count);
            for phase_index in 0..phase_count {
                let (base, base_phase) = &base_phases[phase_index];
                // The base draw selector follows the base MAT0 timeline. The
                // other descriptor stages start at their own initial phase;
                // frame-time sampling below can then advance them separately.
                let (detail, detail_phase) = &detail_phases[initial_detail_phase_index];
                let overlay = overlay_phases
                    .as_ref()
                    .zip(initial_overlay_phase_index)
                    .map(|(phases, phase_index)| &phases[phase_index]);
                let overlay_detail = overlay_detail_phases
                    .as_ref()
                    .zip(initial_overlay_detail_phase_index)
                    .map(|(phases, phase_index)| &phases[phase_index]);
                let preview_selector = u16::try_from(materials.len()).map_err(|_| {
                    "static preview exceeds the available 16-bit material selector space"
                        .to_string()
                })?;
                materials.push(VulkanStaticMaterial {
                    material_index: preview_selector,
                    pipeline_category: base_phase.pipeline_category,
                    texture: base.clone(),
                    detail_texture: Some(detail.clone()),
                    overlay_texture: overlay.map(|(texture, _)| texture.clone()),
                    overlay_detail_texture: overlay_detail.map(|(texture, _)| texture.clone()),
                    diffuse_alpha: base_phase.diffuse_alpha,
                    overlay_diffuse_alpha: overlay.map(|(_, phase)| phase.diffuse_alpha),
                    directional_rgb: base_phase.directional_rgb,
                    additive_rgb: base_phase.additive_rgb,
                    uv_transform: base_phase.page_uv_transform,
                    overlay_directional_rgb: overlay.map(|(_, phase)| phase.directional_rgb),
                    overlay_additive_rgb: overlay.map(|(_, phase)| phase.additive_rgb),
                    specular_rgb: base_phase.specular_rgb,
                    specular_power: base_phase.power,
                    overlay_specular_rgb: overlay.map(|(_, phase)| phase.specular_rgb),
                    overlay_specular_power: overlay.map(|(_, phase)| phase.power),
                    detail_uv_transform: detail_phase.page_uv_transform,
                    overlay_uv_transform: overlay.map(|(_, phase)| phase.page_uv_transform),
                    overlay_detail_uv_transform: overlay_detail
                        .map(|(_, phase)| phase.page_uv_transform),
                    unlit: false,
                    sky_nebula_stars: false,
                    lightmap_mode: false,
                    sky_far_depth: false,
                    screen_tl: false,
                });
                phase_material_indices.push(usize::from(preview_selector));
            }
            let base_uvs = phase_uv_transforms_from_standalone(
                loader,
                &land1_path,
                layers.base_selection().material_index,
                &base_document,
            )?;
            let detail_uvs = phase_uv_transforms_from_standalone(
                loader,
                &land2_path,
                layers.detail_base_selection().material_index,
                &detail_document,
            )?;
            let overlay_uvs = match (&overlay_document, layers.overlay_selection()) {
                (Some(document), Some(selection)) => phase_uv_transforms_from_standalone(
                    loader,
                    &land1_path,
                    selection.material_index,
                    document,
                )?,
                _ => Vec::new(),
            };
            let overlay_detail_uvs =
                match (&overlay_detail_document, layers.detail_overlay_selection()) {
                    (Some(document), Some(selection)) => phase_uv_transforms_from_standalone(
                        loader,
                        &land2_path,
                        selection.material_index,
                        document,
                    )?,
                    _ => Vec::new(),
                };
            let stage_textures = MaterialStageTextures {
                detail: Some(
                    detail_phases
                        .iter()
                        .map(|(texture, _)| texture.clone())
                        .collect(),
                ),
                overlay: overlay_phases
                    .as_ref()
                    .map(|phases| phases.iter().map(|(texture, _)| texture.clone()).collect()),
                overlay_detail: overlay_detail_phases
                    .as_ref()
                    .map(|phases| phases.iter().map(|(texture, _)| texture.clone()).collect()),
                applied_material_phases: phase_material_indices
                    .iter()
                    .copied()
                    .map(|material_index| {
                        (
                            material_index,
                            [
                                initial_detail_phase_index,
                                initial_overlay_phase_index.unwrap_or(0),
                                initial_overlay_detail_phase_index.unwrap_or(0),
                            ],
                        )
                    })
                    .collect(),
            };
            material_animations.push(MaterialAnimationBinding {
                initial_phase_index: static_material_phase
                    .map_or(0, usize::from)
                    .min(base_document.phases.len().saturating_sub(1)),
                variants: vec![MaterialVariantSet {
                    phase_material_indices,
                    range_indices: Vec::new(),
                }],
                base: MaterialPhaseBinding {
                    document: base_document,
                    animation_block_index: 0,
                    wear_row_start_ms: 0,
                    random_state: packed_tag as u32,
                    frozen_phase_index: static_material_phase.map(usize::from),
                    phase_uv_transforms: base_uvs,
                },
                detail: Some(MaterialPhaseBinding {
                    document: detail_document,
                    animation_block_index: 0,
                    wear_row_start_ms: 0,
                    random_state: packed_tag as u32 ^ 0x5A5A_A5A5,
                    frozen_phase_index: static_material_phase.map(usize::from),
                    phase_uv_transforms: detail_uvs,
                }),
                overlay: overlay_document.map(|document| MaterialPhaseBinding {
                    document,
                    animation_block_index: 0,
                    wear_row_start_ms: 0,
                    random_state: packed_tag as u32 ^ 0xA5A5_5A5A,
                    frozen_phase_index: static_material_phase.map(usize::from),
                    phase_uv_transforms: overlay_uvs,
                }),
                overlay_detail: overlay_detail_document.map(|document| MaterialPhaseBinding {
                    document,
                    animation_block_index: 0,
                    wear_row_start_ms: 0,
                    random_state: packed_tag as u32 ^ 0xC3C3_3C3C,
                    frozen_phase_index: static_material_phase.map(usize::from),
                    phase_uv_transforms: overlay_detail_uvs,
                }),
                stage_textures: Some(stage_textures),
            });
            let preview_selector =
                u16::try_from(materials.len() - phase_count + initial_phase_index)
                    .map_err(|_| "terrain material selector exceeds u16".to_string())?;
            Ok((packed_tag, preview_selector))
        })
        .collect()
}

fn static_preview_component_materials(
    assets: &MissionAssets,
    visual: &PreparedVisual,
    mesh: &VulkanStaticMesh,
    static_material_phase: Option<u16>,
    materials: &mut Vec<VulkanStaticMaterial>,
    material_animations: &mut Vec<MaterialAnimationBinding>,
) -> Result<Vec<(u16, u16)>, String> {
    let mut source_selectors = mesh
        .draw_ranges
        .iter()
        .map(|range| range.material_index)
        .collect::<Vec<_>>();
    source_selectors.sort_unstable();
    source_selectors.dedup();
    source_selectors
        .into_iter()
        .map(|source_selector| {
            let material_id = visual
                .material_ids
                .get(usize::from(source_selector))
                .ok_or_else(|| {
                    format!(
                        "static preview MSH batch selector {source_selector} has no prepared WEAR material"
                    )
                })?;
            let material = assets.material_by_id(*material_id).ok_or_else(|| {
                format!("static preview prepared material {source_selector} is unavailable")
            })?;
            let initial_phase_index = usize::from(static_material_phase.unwrap_or(0))
                .min(material.mat0.phases.len().saturating_sub(1));
            if material.mat0.phases.is_empty() {
                return Err(format!(
                    "static preview material {source_selector} has no MAT0 phases"
                ));
            }
            let mut phase_material_indices = Vec::with_capacity(material.mat0.phases.len());
            for phase in &material.mat0.phases {
                let texture_name = phase_texture_name(phase);
                let prepared_texture = texture_name.as_ref().and_then(|name| {
                    assets.textures.iter().find(|texture| {
                        texture.usage == PreparedTextureUsage::Diffuse
                            && texture.source.name == *name
                    })
                });
                // Empty MAT0 texture names remain valid untextured materials.
                let texture = prepared_texture
                    .map(|texture| {
                        prepared_texture_to_vulkan(texture, "static preview diffuse texture")
                    })
                    .transpose()?
                    .unwrap_or_else(|| solid_environment_texture([255, 255, 255, 255]));
                let coefficients = phase.coefficients();
                let uv_transform = if let Some(texture) = prepared_texture {
                    page_uv_transform(
                        texture.texm.width(),
                        texture.texm.height(),
                        &texture.texm.page_rects(),
                        coefficients.page_index,
                    )?
                } else {
                    [0.0, 0.0, 1.0, 1.0]
                };
                let preview_selector = u16::try_from(materials.len()).map_err(|_| {
                    "static preview exceeds the available 16-bit material selector space"
                        .to_string()
                })?;
                materials.push(VulkanStaticMaterial {
                    material_index: preview_selector,
                    pipeline_category: material.pipeline_category,
                    texture,
                    detail_texture: None,
                    overlay_texture: None,
                    overlay_detail_texture: None,
                    diffuse_alpha: coefficients.opacity,
                    overlay_diffuse_alpha: None,
                    directional_rgb: coefficients.directional_rgb,
                    additive_rgb: coefficients.additive_rgb,
                    uv_transform,
                    overlay_directional_rgb: None,
                    overlay_additive_rgb: None,
                    specular_rgb: coefficients.specular_rgb,
                    specular_power: coefficients.power,
                    overlay_specular_rgb: None,
                    overlay_specular_power: None,
                    detail_uv_transform: [0.0, 0.0, 1.0, 1.0],
                    overlay_uv_transform: None,
                    overlay_detail_uv_transform: None,
                    unlit: false,
                    sky_nebula_stars: false,
                    lightmap_mode: false,
                    sky_far_depth: false,
                    screen_tl: false,
                });
                phase_material_indices.push(usize::from(preview_selector));
            }
            let preview_selector = u16::try_from(
                *phase_material_indices
                    .get(initial_phase_index)
                    .ok_or_else(|| "MAT0 phase variant table is empty".to_string())?,
            )
            .map_err(|_| "MAT0 phase variant selector exceeds u16".to_string())?;
            material_animations.push(MaterialAnimationBinding {
                initial_phase_index,
                variants: vec![MaterialVariantSet {
                    phase_material_indices,
                    range_indices: Vec::new(),
                }],
                base: MaterialPhaseBinding {
                    document: material.mat0.clone(),
                    animation_block_index: 0,
                    wear_row_start_ms: 0,
                    random_state: material_id.raw() as u32,
                    frozen_phase_index: static_material_phase.map(usize::from),
                    phase_uv_transforms: phase_uv_transforms_from_assets(&material.mat0, assets),
                },
                detail: None,
                overlay: None,
                overlay_detail: None,
                stage_textures: None,
            });
            Ok((source_selector, preview_selector))
        })
        .collect()
}

fn prepared_texture_to_vulkan(
    texture: &PreparedTexture,
    label: &str,
) -> Result<VulkanStaticTexture, String> {
    let mip_images = (0..texture.texm.mip_count())
        .map(|level| texture.decode_mip_rgba8(u32::try_from(level).unwrap_or(u32::MAX)))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| format!("decode {label} {:?}: {err}", texture.source.name))?;
    let image = mip_images
        .first()
        .ok_or_else(|| format!("{label} {:?} has no mip zero", texture.source.name))?;
    Ok(VulkanStaticTexture {
        width: image.width,
        height: image.height,
        rgba8: image.rgba8.clone(),
        mip_levels: mip_images
            .into_iter()
            .map(|image| VulkanStaticTextureMip {
                width: image.width,
                height: image.height,
                rgba8: image.rgba8,
            })
            .collect(),
    })
}

fn static_preview_lightmap_textures(
    assets: &MissionAssets,
    visual: &PreparedVisual,
) -> Result<Vec<VulkanStaticTexture>, String> {
    visual
        .lightmap_ids
        .iter()
        .map(|id| {
            let texture = assets
                .textures
                .iter()
                .find(|texture| {
                    texture.id == *id && texture.usage == PreparedTextureUsage::Lightmap
                })
                .ok_or_else(|| format!("static preview lightmap {id:?} is unavailable"))?;
            prepared_texture_to_vulkan(texture, "static preview lightmap")
        })
        .collect()
}

fn append_static_preview_component(
    target: &mut VulkanStaticMesh,
    component: VulkanStaticMesh,
    selector_remap: &[(u16, u16)],
    material_animations: Option<&mut Vec<MaterialAnimationBinding>>,
) -> Result<(), String> {
    append_static_preview_component_with_lightmaps(
        target,
        component,
        selector_remap,
        None,
        &[],
        material_animations,
    )
}

fn append_static_preview_component_with_lightmaps(
    target: &mut VulkanStaticMesh,
    component: VulkanStaticMesh,
    selector_remap: &[(u16, u16)],
    mut materials: Option<&mut Vec<VulkanStaticMaterial>>,
    lightmap_textures: &[VulkanStaticTexture],
    mut material_animations: Option<&mut Vec<MaterialAnimationBinding>>,
) -> Result<(), String> {
    let vertex_base = u32::try_from(target.vertices.len())
        .map_err(|_| "static preview vertex count exceeds u32".to_string())?;
    let first_index_base = u32::try_from(target.indices.len())
        .map_err(|_| "static preview index count exceeds u32".to_string())?;
    target
        .vertices
        .len()
        .checked_add(component.vertices.len())
        .ok_or_else(|| "static preview vertex count exceeds addressable memory".to_string())?;
    target.vertices.extend(component.vertices);
    target.indices.extend(
        component
            .indices
            .into_iter()
            .map(|index| {
                index
                    .checked_add(vertex_base)
                    .ok_or_else(|| "static preview vertex index exceeds u32".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?,
    );
    // A lightmapped range must keep the lightmap material family when MAT0
    // advances to another phase.  The range's original selector identifies
    // the base binding, while the lightmap selector identifies the secondary
    // texture.  Keep the generated phase family and the binding/variant it
    // belongs to so the pending MAT0 packet can rebind every frame.
    let mut lightmap_materials: HashMap<(usize, u8), (Vec<usize>, usize, usize)> = HashMap::new();
    for range in component.draw_ranges {
        let source_material_index = selector_remap
            .iter()
            .find_map(|(source, preview)| (*source == range.material_index).then_some(*preview))
            .ok_or_else(|| {
                format!(
                    "static preview component has no material remap for selector {}",
                    range.material_index
                )
            })?;
        let (material_index, lightmap_variant) = if lightmap_textures
            .get(usize::from(range.lightmap_index))
            .is_some()
        {
            let key = (usize::from(source_material_index), range.lightmap_index);
            let (phase_material_indices, binding_index, variant_index) = if let Some(existing) =
                lightmap_materials.get(&key)
            {
                existing.clone()
            } else {
                let bindings = material_animations.as_deref().ok_or_else(|| {
                    "static preview lightmap animation storage is unavailable".to_string()
                })?;
                let (binding_index, source_phase_indices) =
                    find_material_animation_variant(bindings, usize::from(source_material_index))
                        .ok_or_else(|| {
                            format!(
                                "static preview material selector {source_material_index} has no MAT0 animation binding"
                            )
                        })?;
                let lightmap = lightmap_textures
                    .get(usize::from(range.lightmap_index))
                    .expect("lightmap presence checked above");
                let materials = materials.as_mut().ok_or_else(|| {
                    "static preview lightmap material storage is unavailable".to_string()
                })?;
                let mut phase_material_indices = Vec::with_capacity(source_phase_indices.len());
                for source_phase_index in source_phase_indices {
                    let mut lightmap_material = materials
                            .get(source_phase_index)
                            .cloned()
                            .ok_or_else(|| {
                                format!(
                                    "static preview material selector {source_phase_index} is unavailable"
                                )
                            })?;
                    let original_directional = lightmap_material.directional_rgb;
                    lightmap_material.directional_rgb = [0.0; 3];
                    lightmap_material.additive_rgb = original_directional;
                    lightmap_material.detail_texture = Some(lightmap.clone());
                    lightmap_material.detail_uv_transform = [0.0, 0.0, 1.0, 1.0];
                    lightmap_material.overlay_texture = None;
                    lightmap_material.overlay_detail_texture = None;
                    lightmap_material.overlay_diffuse_alpha = None;
                    lightmap_material.overlay_directional_rgb = None;
                    lightmap_material.overlay_additive_rgb = None;
                    lightmap_material.overlay_uv_transform = None;
                    lightmap_material.overlay_detail_uv_transform = None;
                    lightmap_material.unlit = false;
                    lightmap_material.sky_nebula_stars = false;
                    lightmap_material.lightmap_mode = true;
                    let index = u16::try_from(materials.len()).map_err(|_| {
                        "static preview exceeds the available 16-bit material selector space"
                            .to_string()
                    })?;
                    lightmap_material.material_index = index;
                    materials.push(lightmap_material);
                    phase_material_indices.push(usize::from(index));
                }
                let bindings = material_animations.as_deref_mut().ok_or_else(|| {
                    "static preview lightmap animation storage is unavailable".to_string()
                })?;
                let variant_index = bindings[binding_index].variants.len();
                bindings[binding_index].variants.push(MaterialVariantSet {
                    phase_material_indices: phase_material_indices.clone(),
                    range_indices: Vec::new(),
                });
                let value = (phase_material_indices, binding_index, variant_index);
                lightmap_materials.insert(key, value.clone());
                value
            };
            let initial_material = *phase_material_indices
                .get(
                    material_animations
                        .as_deref()
                        .and_then(|bindings| bindings.get(binding_index))
                        .map_or(0, |binding| binding.initial_phase_index),
                )
                .ok_or_else(|| "static preview lightmap phase table is empty".to_string())?;
            (
                u16::try_from(initial_material).map_err(|_| {
                    "static preview lightmap material selector exceeds u16".to_string()
                })?,
                Some((binding_index, variant_index)),
            )
        } else {
            (source_material_index, None)
        };
        let target_range_index = target.draw_ranges.len();
        target
            .draw_ranges
            .push(fparkan_render_vulkan::VulkanStaticDrawRange {
                first_index: first_index_base
                    .checked_add(range.first_index)
                    .ok_or_else(|| "static preview index range exceeds u32".to_string())?,
                material_index,
                ..range
            });
        if let Some(bindings) = material_animations.as_deref_mut() {
            if let Some((binding_index, variant_index)) = lightmap_variant {
                bindings
                    .get_mut(binding_index)
                    .and_then(|binding| binding.variants.get_mut(variant_index))
                    .ok_or_else(|| {
                        "static preview lightmap animation variant is missing".to_string()
                    })?
                    .range_indices
                    .push(target_range_index);
            } else {
                register_material_range(
                    bindings,
                    usize::from(source_material_index),
                    target_range_index,
                );
            }
        }
    }
    Ok(())
}

fn load_standalone_vulkan_texture(
    loader: &mut StandaloneWearMaterialLoader,
    wear_path: &Path,
    material_index: u16,
    phase_index: u16,
) -> Result<(VulkanStaticTexture, PreparedMaterialPhase), String> {
    let (mips, phase) = loader.load(wear_path, material_index, phase_index)?;
    let first = mips
        .first()
        .ok_or_else(|| "standalone TEXM mip loader returned no mip zero".to_string())?;
    Ok((
        VulkanStaticTexture {
            width: first.width,
            height: first.height,
            rgba8: first.rgba8.clone(),
            mip_levels: mips
                .into_iter()
                .map(|image| VulkanStaticTextureMip {
                    width: image.width,
                    height: image.height,
                    rgba8: image.rgba8,
                })
                .collect(),
        },
        phase,
    ))
}

fn load_standalone_phase_set(
    loader: &mut StandaloneWearMaterialLoader,
    wear_path: &Path,
    material_index: u16,
    document: &Mat0Document,
) -> Result<Vec<(VulkanStaticTexture, PreparedMaterialPhase)>, String> {
    (0..document.phases.len())
        .map(|phase_index| {
            load_standalone_vulkan_texture(
                loader,
                wear_path,
                material_index,
                u16::try_from(phase_index)
                    .map_err(|_| "MAT0 phase index exceeds u16".to_string())?,
            )
        })
        .collect()
}

fn run_static_vulkan_mode(
    preview: StaticPreviewScene,
    environment: Option<DynamicEnvironment>,
    audio: Option<audio::GameAudio>,
    target_frames: u64,
    control_clock_target_ms: Option<u32>,
    mission: &str,
    object_count: usize,
    readback_out: Option<&std::path::Path>,
    enable_validation: bool,
    startup_timings: StartupTimings,
) -> Result<String, String> {
    let event_loop = EventLoop::new().map_err(|err| format!("winit event loop: {err}"))?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut preview = preview;
    let mut environment = environment;
    // Reserve the projected-shadow draw range before the environment builder
    // reorders its fixed passes. The range itself remains a stable draw slot;
    // frame geometry is appended after every static environment allocation.
    let mut shadow_range = append_environment_range(
        &mut preview.mesh,
        preview.shadow_material_index,
        shadow_placeholder_vertices(),
        quad_indices(1)?,
        world_pipeline_state(LegacyBlendMode::SourceAlpha, LegacyDepthMode::TestReadOnly),
    )?;
    let environment_gpu = match environment.as_mut() {
        Some(environment) => {
            let camera = preview.view_camera;
            let environment_materials = environment.materials.clone();
            let material_assets = environment.material_assets.clone();
            let sky_mesh = environment.sky.mesh().clone();
            Some(EnvironmentGpuScene::new(
                &mut preview.mesh,
                &mut preview.native_handedness_by_range,
                &mut preview.materials,
                &mut preview.material_animations,
                &mut preview.sun_occlusion_range_indices,
                &environment_materials,
                &material_assets,
                &sky_mesh,
                &camera,
                &mut shadow_range.range_index,
            )?)
        }
        None => None,
    };
    let shadow_base_vertex = preview.mesh.vertices.len();
    let shadow_base_index = preview.mesh.indices.len();
    let mut app = StaticVulkanApp::new(
        preview,
        environment,
        environment_gpu,
        shadow_range,
        shadow_base_vertex,
        shadow_base_index,
        audio,
        target_frames,
        control_clock_target_ms,
        mission,
        object_count,
        readback_out.map(std::path::Path::to_path_buf),
        enable_validation,
        startup_timings,
    );
    if let Err(err) = event_loop.run_app(&mut app) {
        app.error = Some(format!("winit event loop: {err}"));
    }
    app.finish()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SceneSampleId {
    Live(u64),
    Replay(u64),
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct FrameClockSample {
    id: SceneSampleId,
    time_ms: u32,
    delta_seconds: f32,
}

#[derive(Clone, Debug, PartialEq)]
struct SceneFrameUpdate {
    sample: FrameClockSample,
    camera: PreviewCameraFrame,
    viewport: [f32; 2],
    aspect: f32,
    material_update: MaterialFrameUpdate,
    environment_material_update: Option<MaterialFrameUpdate>,
    environment: Option<DynamicEnvironmentFrame>,
}

#[derive(Debug, Default)]
struct SceneFrameLifecycle {
    pending: Option<SceneFrameUpdate>,
    frames_presented: u64,
    last_replay_time_ms: u32,
}

impl SceneFrameLifecycle {
    fn sample_replay(&self, target_ms: u32, target_frames: u64) -> FrameClockSample {
        if let Some(pending) = self.pending.as_ref() {
            return pending.sample;
        }
        let time_ms = control_clock_ms_for_frame(target_ms, self.frames_presented, target_frames);
        FrameClockSample {
            id: SceneSampleId::Replay(self.frames_presented),
            time_ms,
            delta_seconds: replay_environment_delta_seconds(self.last_replay_time_ms, time_ms),
        }
    }

    fn prepare(&mut self, update: SceneFrameUpdate) -> bool {
        if self.pending.is_some() {
            return false;
        }
        self.pending = Some(update);
        true
    }

    fn record_presented(&mut self) -> Option<u64> {
        let sample = self.pending.take()?.sample;
        let frame_index = self.frames_presented;
        self.frames_presented = self.frames_presented.saturating_add(1);
        if matches!(sample.id, SceneSampleId::Replay(_)) {
            self.last_replay_time_ms = sample.time_ms;
        }
        Some(frame_index)
    }
}

fn control_clock_ms_for_frame(target_ms: u32, frame_index: u64, target_frames: u64) -> u32 {
    if target_frames <= 1 {
        return target_ms;
    }
    let last_frame = u128::from(target_frames - 1);
    let frame = u128::from(frame_index).min(last_frame);
    ((u128::from(target_ms) * frame) / last_frame) as u32
}

fn replay_environment_delta_seconds(last_applied_ms: u32, current_ms: u32) -> f32 {
    current_ms.saturating_sub(last_applied_ms) as f32 / 1_000.0
}

struct StaticVulkanApp {
    mesh: VulkanStaticMesh,
    native_handedness_by_range: HashMap<usize, i8>,
    materials: Vec<VulkanStaticMaterial>,
    material_animations: Vec<MaterialAnimationBinding>,
    sun_occlusion_range_indices: Vec<usize>,
    terrain: Arc<TerrainWorld>,
    component_pose_buffers: Vec<Vec<Option<NodePoseBuffer>>>,
    component_mount_poses: Vec<Vec<Option<VulkanNodePose>>>,
    control_animation_instances: Vec<ControlAnimationInstance>,
    dynamic_component_geometry: Vec<DynamicComponentGeometry>,
    dynamic_component_mounts: Vec<DynamicComponentMount>,
    shadow_caster_indices_by_object: Vec<Option<usize>>,
    shadow_sphere_baselines: Vec<ShadowSphere>,
    world_range_indices: Vec<usize>,
    world_range_index_counts: HashMap<usize, u32>,
    camera: VulkanStaticCamera,
    view_camera: PreviewCameraFrame,
    captured_viewport: Option<CapturedViewport>,
    free_camera: Option<FreeFlightCamera>,
    environment: Option<DynamicEnvironment>,
    environment_gpu: Option<EnvironmentGpuScene>,
    shadow_range: EnvironmentGpuRange,
    shadow_base_vertex: usize,
    shadow_base_index: usize,
    shadow_material_index: usize,
    shadow_casters: Vec<ShadowCaster>,
    shadow_receivers: Vec<ShadowTriangle>,
    shadow_cache: ShadowPageCache,
    shadow_last_evidence: ShadowFrameEvidence,
    material_time_ms: u32,
    environment_primitive_count: usize,
    pressed_keys: HashSet<KeyCode>,
    last_tick: Instant,
    simulation_elapsed: Duration,
    mouse_look: bool,
    audio: Option<audio::GameAudio>,
    camera_mode: &'static str,
    mesh_components: usize,
    terrain_components: usize,
    target_frames: u64,
    control_clock_target_ms: Option<u32>,
    mission: String,
    object_count: usize,
    readback_out: Option<PathBuf>,
    enable_validation: bool,
    startup_timings: StartupTimings,
    window_id: Option<WindowId>,
    window: Option<Window>,
    renderer: Option<VulkanSmokeRenderer>,
    lifecycle: SceneFrameLifecycle,
    output: Option<String>,
    error: Option<String>,
}

impl StaticVulkanApp {
    fn new(
        preview: StaticPreviewScene,
        environment: Option<DynamicEnvironment>,
        environment_gpu: Option<EnvironmentGpuScene>,
        shadow_range: EnvironmentGpuRange,
        shadow_base_vertex: usize,
        shadow_base_index: usize,
        audio: Option<audio::GameAudio>,
        target_frames: u64,
        control_clock_target_ms: Option<u32>,
        mission: &str,
        object_count: usize,
        readback_out: Option<PathBuf>,
        enable_validation: bool,
        startup_timings: StartupTimings,
    ) -> Self {
        let mut fixed_range_indices = environment_gpu
            .as_ref()
            .map(EnvironmentGpuScene::fixed_range_indices)
            .unwrap_or_default();
        fixed_range_indices.insert(shadow_range.range_index);
        let world_range_indices = (0..preview.mesh.draw_ranges.len())
            .filter(|range_index| !fixed_range_indices.contains(range_index))
            .collect();
        let world_range_index_counts = preview
            .mesh
            .draw_ranges
            .iter()
            .enumerate()
            .filter(|(range_index, _)| !fixed_range_indices.contains(range_index))
            .map(|(range_index, range)| (range_index, range.index_count))
            .collect();
        Self {
            mesh: preview.mesh,
            native_handedness_by_range: preview.native_handedness_by_range,
            materials: preview.materials,
            material_animations: preview.material_animations,
            sun_occlusion_range_indices: preview.sun_occlusion_range_indices,
            terrain: preview.terrain,
            component_pose_buffers: preview.component_pose_buffers,
            component_mount_poses: preview.component_mount_poses,
            control_animation_instances: preview.control_animation_instances,
            dynamic_component_geometry: preview.dynamic_component_geometry,
            dynamic_component_mounts: preview.dynamic_component_mounts,
            shadow_caster_indices_by_object: preview.shadow_caster_indices_by_object,
            shadow_sphere_baselines: preview.shadow_sphere_baselines,
            world_range_indices,
            world_range_index_counts,
            camera: preview.camera,
            view_camera: preview.view_camera,
            captured_viewport: preview.captured_viewport,
            free_camera: preview.free_camera,
            environment,
            environment_gpu,
            shadow_base_vertex,
            shadow_base_index,
            shadow_material_index: preview.shadow_material_index,
            shadow_range,
            shadow_casters: preview.shadow_casters,
            shadow_receivers: preview.shadow_receivers,
            shadow_cache: ShadowPageCache::default(),
            shadow_last_evidence: ShadowFrameEvidence::default(),
            material_time_ms: 0,
            environment_primitive_count: 0,
            pressed_keys: HashSet::new(),
            last_tick: Instant::now(),
            simulation_elapsed: Duration::ZERO,
            mouse_look: false,
            audio,
            camera_mode: preview.camera_mode,
            mesh_components: preview.mesh_components,
            terrain_components: preview.terrain_components,
            target_frames,
            control_clock_target_ms,
            mission: mission.to_string(),
            object_count,
            readback_out,
            enable_validation,
            startup_timings,
            window_id: None,
            window: None,
            renderer: None,
            lifecycle: SceneFrameLifecycle::default(),
            output: None,
            error: None,
        }
    }

    fn configure_world_draw_ranges(
        &self,
        renderer: &mut VulkanSmokeRenderer,
    ) -> Result<(), String> {
        for &range_index in &self.world_range_indices {
            let range = self
                .mesh
                .draw_ranges
                .get(range_index)
                .ok_or_else(|| "world draw range index is out of bounds".to_string())?;
            let alpha = self
                .materials
                .get(usize::from(range.material_index))
                .map_or(1.0, |material| material.diffuse_alpha);
            let transparent = native_world_transparent(range, alpha);
            renderer
                .set_draw_range_transparency(range_index, transparent)
                .map_err(|error| format!("configure world draw range {range_index}: {error}"))?;
            let category = self
                .materials
                .get(usize::from(range.material_index))
                .ok_or_else(|| "world draw range has no material selector".to_string())?
                .pipeline_category;
            renderer
                .set_draw_range_material_category(range_index, category)
                .map_err(|error| {
                    format!("configure world material category {range_index}: {error}")
                })?;
            renderer
                .set_draw_range_index_count(
                    range_index,
                    world_draw_range_index_count(
                        &self.mesh,
                        range,
                        range.index_count,
                        alpha,
                        self.view_camera.position,
                        self.native_handedness_by_range.get(&range_index).copied(),
                    ),
                )
                .map_err(|error| {
                    format!("configure world draw range {range_index} count: {error}")
                })?;
        }
        Ok(())
    }

    fn update_world_transparency(
        mesh: &VulkanStaticMesh,
        materials: &[VulkanStaticMaterial],
        world_range_indices: &[usize],
        world_range_index_counts: &HashMap<usize, u32>,
        native_handedness_by_range: &HashMap<usize, i8>,
        eye_world: [f32; 3],
        active_materials: &HashMap<usize, ActiveMaterialState>,
        renderer: &mut VulkanSmokeRenderer,
    ) -> Result<(), String> {
        for &range_index in world_range_indices {
            let range = mesh
                .draw_ranges
                .get(range_index)
                .ok_or_else(|| "world draw range index is out of bounds".to_string())?;
            let state = active_materials.get(&range_index);
            let alpha = state.map_or_else(
                || {
                    materials
                        .get(usize::from(range.material_index))
                        .map_or(1.0, |material| material.diffuse_alpha)
                },
                |state| state.diffuse_alpha,
            );
            renderer
                .set_draw_range_transparency(range_index, native_world_transparent(range, alpha))
                .map_err(|error| format!("update world draw range {range_index}: {error}"))?;
            let original_index_count = *world_range_index_counts
                .get(&range_index)
                .ok_or_else(|| "world draw range count is missing".to_string())?;
            let count = world_draw_range_index_count(
                mesh,
                range,
                original_index_count,
                alpha,
                eye_world,
                native_handedness_by_range.get(&range_index).copied(),
            );
            renderer
                .set_draw_range_index_count(range_index, count)
                .map_err(|error| format!("update world draw range {range_index} count: {error}"))?;
        }
        Ok(())
    }

    fn world_draw_sort_keys(&self, camera_position: [f32; 3]) -> Result<Vec<(usize, f32)>, String> {
        let mut keys = Vec::with_capacity(self.world_range_indices.len());
        for &range_index in &self.world_range_indices {
            let sort_key = first_indexed_vertex_distance(&self.mesh, range_index, camera_position)?;
            keys.push((range_index, sort_key));
        }
        Ok(keys)
    }

    fn update_free_camera(&mut self, delta_seconds: f32, viewport: [f32; 2]) -> Result<(), String> {
        if self.free_camera.is_none() {
            return Ok(());
        }
        let aspect = viewport[0] / viewport[1].max(1.0);
        let view_camera = {
            let free_camera = self
                .free_camera
                .as_mut()
                .ok_or_else(|| "free-flight camera disappeared".to_string())?;
            free_camera.advance(&self.pressed_keys, delta_seconds.min(0.1));
            free_camera.keep_above_terrain(&self.terrain)?;
            free_camera.preview_frame(aspect)
        };
        let camera = view_camera.vulkan_camera();
        self.view_camera = view_camera;
        self.camera = camera;
        let sort_keys = self.world_draw_sort_keys(view_camera.position)?;
        if let Some(renderer) = self.renderer.as_mut() {
            renderer
                .set_camera(camera)
                .map_err(|error| format!("update free-flight camera: {error}"))?;
            for (range_index, sort_key) in sort_keys {
                renderer
                    .set_draw_range_sort_key(range_index, sort_key)
                    .map_err(|error| {
                        format!("update world draw range {range_index} sort key: {error}")
                    })?;
            }
        }
        Ok(())
    }

    fn update_material_clock(&mut self, sample: FrameClockSample) {
        self.material_time_ms = sample.time_ms;
    }

    fn update_controlled_component_geometry(
        &mut self,
        sample: FrameClockSample,
    ) -> Result<(), String> {
        let mut pose_changed = false;
        for instance in &mut self.control_animation_instances {
            let pose = static_preview_control_pose_buffer(
                &instance.model,
                &instance.control,
                sample.time_ms,
                &mut instance.state,
            )?;
            let slot = self
                .component_pose_buffers
                .get_mut(instance.object_index)
                .and_then(|buffers| buffers.get_mut(instance.component_index))
                .ok_or_else(|| {
                    format!(
                        "animated component {} pose slot is outside object {}",
                        instance.component_index, instance.object_index
                    )
                })?;
            if *slot != pose {
                *slot = pose;
                pose_changed = true;
            }
        }
        if !pose_changed {
            return Ok(());
        }

        // Mounts are recalculated from the sampled hierarchy even when the
        // parent has no visible draw geometry of its own.
        let mounts_changed = refresh_dynamic_component_mounts(
            &self.dynamic_component_mounts,
            &self.component_pose_buffers,
            &mut self.component_mount_poses,
        )?;
        let mut geometry_changed = false;
        let mut shadow_updates = Vec::new();
        let mut updated_shadow_casters = HashSet::new();
        for dynamic in &self.dynamic_component_geometry {
            let pose = self
                .component_pose_buffers
                .get(dynamic.object_index)
                .and_then(|buffers| buffers.get(dynamic.component_index))
                .and_then(Option::as_ref)
                .ok_or_else(|| {
                    format!(
                        "animated component {} has no current Node38 hierarchy",
                        dynamic.component_index
                    )
                })?;
            let mount = self
                .component_mount_poses
                .get(dynamic.object_index)
                .and_then(|poses| poses.get(dynamic.component_index))
                .copied()
                .flatten();
            let projected = project_dynamic_component_geometry(dynamic, pose, mount)?;
            if projected.vertices.len() != dynamic.vertex_count {
                return Err(format!(
                    "animated component {} changed projected vertex count from {} to {}",
                    dynamic.component_index,
                    dynamic.vertex_count,
                    projected.vertices.len()
                ));
            }
            let end = dynamic
                .vertex_start
                .checked_add(dynamic.vertex_count)
                .ok_or_else(|| "animated component vertex range overflowed".to_string())?;
            let destination = self
                .mesh
                .vertices
                .get_mut(dynamic.vertex_start..end)
                .ok_or_else(|| {
                    format!(
                        "animated component {} vertex range is outside the shared mesh",
                        dynamic.component_index
                    )
                })?;
            if destination != projected.vertices.as_slice() {
                destination.copy_from_slice(&projected.vertices);
                geometry_changed = true;
            }
            if let Some(range) = dynamic.shadow_triangle_range.clone() {
                let triangles = static_mesh_shadow_triangles(&projected);
                if triangles.len() != range.len() {
                    return Err(format!(
                        "animated component {} changed shadow triangle count from {} to {}",
                        dynamic.component_index,
                        range.len(),
                        triangles.len()
                    ));
                }
                shadow_updates.push((dynamic.object_index, range, triangles));
            }
        }
        for (object_index, range, triangles) in shadow_updates {
            let Some(caster_index) = self
                .shadow_caster_indices_by_object
                .get(object_index)
                .copied()
                .flatten()
            else {
                continue;
            };
            let caster = self
                .shadow_casters
                .get_mut(caster_index)
                .ok_or_else(|| format!("shadow caster {caster_index} is unavailable"))?;
            let destination = caster.triangles.get_mut(range).ok_or_else(|| {
                format!("animated component shadow range is outside caster {caster_index}")
            })?;
            destination.clone_from_slice(&triangles);
            updated_shadow_casters.insert(caster_index);
        }
        for caster_index in updated_shadow_casters {
            let baseline = self
                .shadow_sphere_baselines
                .get(caster_index)
                .copied()
                .ok_or_else(|| format!("shadow sphere baseline {caster_index} is unavailable"))?;
            let caster = self
                .shadow_casters
                .get_mut(caster_index)
                .ok_or_else(|| format!("shadow caster {caster_index} is unavailable"))?;
            // shortcut: native animated sphere updates are unverified; derive
            // a conservative envelope from the native startup sphere and all
            // current triangles, never from a prior frame's expanded bounds.
            caster.sphere = shadow_sphere_enclosing_triangles(baseline, &caster.triangles)?;
        }
        if geometry_changed || mounts_changed {
            let sort_keys = self.world_draw_sort_keys(self.view_camera.position)?;
            if let Some(renderer) = self.renderer.as_mut() {
                for (range_index, sort_key) in sort_keys {
                    renderer
                        .set_draw_range_sort_key(range_index, sort_key)
                        .map_err(|error| {
                            format!("update animated component sort key {range_index}: {error}")
                        })?;
                }
            }
        }
        Ok(())
    }

    fn update_shadow(
        mesh: &mut VulkanStaticMesh,
        shadow_cache: &mut ShadowPageCache,
        shadow_casters: &[ShadowCaster],
        shadow_receivers: &[ShadowTriangle],
        shadow_range: EnvironmentGpuRange,
        shadow_base_vertex: usize,
        shadow_base_index: usize,
        shadow_material_index: usize,
        renderer: &mut VulkanSmokeRenderer,
        sky_frame: Option<&SkyFrame<'_>>,
        environment_frame: Option<&EnvironmentFrame>,
        shadow_camera: Option<ShadowCamera>,
    ) -> Result<ShadowFrameEvidence, String> {
        let lights = shadow_lights(sky_frame, environment_frame, renderer);
        let visible_casters = native_visible_shadow_casters(
            shadow_casters,
            renderer.frame_uniforms().clip_from_world,
        );
        let frame = shadow_cache
            .build(ShadowScene {
                lights: &lights,
                casters: &visible_casters,
                receivers: shadow_receivers,
                fade: 1.0,
                smooth_passes: 1,
                camera: shadow_camera,
                lod: ShadowLodSettings::default(),
            })
            .map_err(|error| format!("build projected shadow frame: {error}"))?;
        let geometry = append_shadow_frame_geometry(
            mesh,
            shadow_range,
            shadow_base_vertex,
            shadow_base_index,
            &frame,
        )?;
        let atlas = shadow_atlas_texture_from_frame(&frame);
        renderer
            .update_material_texture(shadow_material_index, 0, &atlas)
            .map_err(|error| format!("upload projected shadow atlas: {error}"))?;
        renderer
            .update_mesh_geometry(&mesh.vertices, &mesh.indices, &[geometry])
            .map_err(|error| format!("upload environment and shadow geometry: {error}"))?;
        Ok(ShadowFrameEvidence {
            visible_casters: visible_casters.len(),
            projected_indices: frame.indices.len(),
        })
    }

    fn update_environment(
        &mut self,
        sample: FrameClockSample,
        viewport: [f32; 2],
    ) -> Result<Option<DynamicEnvironmentFrame>, String> {
        if self.renderer.is_none() {
            return Ok(None);
        }
        let camera = self.view_camera;
        if let Some(audio) = self.audio.as_mut() {
            audio
                .update_listener(camera.position, camera.forward, camera.up)
                .map_err(|error| format!("update audio listener: {error}"))?;
        }
        let environment_frame = if self.environment.is_some() {
            let renderer = self
                .renderer
                .as_mut()
                .ok_or_else(|| "native Vulkan renderer was not initialized".to_string())?;
            let environment = self
                .environment
                .as_mut()
                .ok_or_else(|| "dynamic environment disappeared".to_string())?;
            Some(environment.update(
                sample.delta_seconds,
                &camera,
                viewport[0] / viewport[1].max(1.0),
                viewport,
                renderer,
                self.audio.as_mut(),
            )?)
        } else {
            None
        };
        if let Some(frame) = environment_frame.as_ref() {
            self.environment_primitive_count = frame.environment_frame.primitives.len();
        }
        Ok(environment_frame)
    }

    fn apply_environment_view(
        &mut self,
        frame: Option<&DynamicEnvironmentFrame>,
        environment_material_update: Option<&MaterialFrameUpdate>,
        camera: PreviewCameraFrame,
        viewport: [f32; 2],
    ) -> Result<(), String> {
        let Some(renderer) = self.renderer.as_mut() else {
            return Ok(());
        };
        let aspect = viewport[0] / viewport[1].max(1.0);
        if let Some(frame) = frame {
            let environment = self
                .environment
                .as_ref()
                .ok_or_else(|| "dynamic environment disappeared".to_string())?;
            let sky_frame = SkyFrame::from_atmosphere(
                &frame.atmosphere,
                &environment.materials,
                frame.absolute_time_seconds,
            );
            if let Some(environment_gpu) = self.environment_gpu.as_mut() {
                let material_update = environment_material_update
                    .ok_or_else(|| "sampled environment material state is missing".to_string())?;
                environment_gpu.update(
                    &mut self.mesh,
                    renderer,
                    material_update,
                    &sky_frame,
                    &frame.sky_mesh,
                    &frame.environment_frame,
                    &camera,
                    viewport,
                    &self.terrain,
                    &self.sun_occlusion_range_indices,
                )?;
            }
            let shadow_camera = ShadowCamera {
                position: camera.position,
                viewport_width: viewport[0],
                horizontal_fov: horizontal_fov(&camera, aspect),
            };
            self.shadow_last_evidence = Self::update_shadow(
                &mut self.mesh,
                &mut self.shadow_cache,
                &self.shadow_casters,
                &self.shadow_receivers,
                self.shadow_range,
                self.shadow_base_vertex,
                self.shadow_base_index,
                self.shadow_material_index,
                renderer,
                Some(&sky_frame),
                Some(&frame.environment_frame),
                Some(shadow_camera),
            )?;
        } else {
            let shadow_camera = Some(ShadowCamera {
                position: camera.position,
                viewport_width: viewport[0],
                horizontal_fov: horizontal_fov(&camera, aspect),
            });
            self.shadow_last_evidence = Self::update_shadow(
                &mut self.mesh,
                &mut self.shadow_cache,
                &self.shadow_casters,
                &self.shadow_receivers,
                self.shadow_range,
                self.shadow_base_vertex,
                self.shadow_base_index,
                self.shadow_material_index,
                renderer,
                None,
                None,
                shadow_camera,
            )?;
        }
        Ok(())
    }

    fn refresh_pending_viewport(&mut self, viewport: [f32; 2]) -> Result<(), String> {
        if self.captured_viewport.is_some() {
            return Ok(());
        }
        let Some(mut update) = self.lifecycle.pending.clone() else {
            return Ok(());
        };
        let viewport = [viewport[0].max(1.0), viewport[1].max(1.0)];
        let aspect = viewport[0] / viewport[1];
        let camera = update.camera.with_aspect(update.aspect, aspect);
        self.view_camera = camera;
        self.camera = camera.vulkan_camera();
        if let Some(renderer) = self.renderer.as_mut() {
            renderer
                .set_camera(self.camera)
                .map_err(|error| format!("refresh free-flight camera viewport: {error}"))?;
        }
        let environment_camera = environment_camera_for_preview(&camera, aspect, viewport);
        if let Some(environment) = update.environment.as_mut() {
            environment
                .environment_frame
                .reproject_for_camera(environment_camera);
        }
        update.camera = camera;
        update.viewport = viewport;
        update.aspect = aspect;
        self.lifecycle.pending = Some(update);
        Ok(())
    }

    fn apply_pending_scene_update(&mut self) -> Result<(), String> {
        let Some(frame) = self.lifecycle.pending.take() else {
            return Ok(());
        };
        let result = (|| {
            {
                let Some(renderer) = self.renderer.as_mut() else {
                    return Err("native Vulkan renderer was not initialized".to_string());
                };
                apply_material_frame_update(
                    &frame.material_update,
                    &mut self.material_animations,
                    renderer,
                )?;
                Self::update_world_transparency(
                    &self.mesh,
                    &self.materials,
                    &self.world_range_indices,
                    &self.world_range_index_counts,
                    &self.native_handedness_by_range,
                    frame.camera.position,
                    &frame.material_update.active_materials,
                    renderer,
                )?;
                if let Some(material_update) = frame.environment_material_update.as_ref() {
                    let environment_gpu = self.environment_gpu.as_mut().ok_or_else(|| {
                        "sampled environment material state lost its owner".to_string()
                    })?;
                    apply_material_frame_update(
                        material_update,
                        &mut environment_gpu.material_animations,
                        renderer,
                    )?;
                }
            }
            self.apply_environment_view(
                frame.environment.as_ref(),
                frame.environment_material_update.as_ref(),
                frame.camera,
                frame.viewport,
            )
        })();
        self.lifecycle.pending = Some(frame);
        result
    }

    fn sample_frame_clock(&mut self) -> FrameClockSample {
        if let Some(target_ms) = self.control_clock_target_ms {
            return self.lifecycle.sample_replay(target_ms, self.target_frames);
        }
        self.sample_live_frame_clock_at(Instant::now())
    }

    fn sample_live_frame_clock_at(&mut self, now: Instant) -> FrameClockSample {
        if let Some(pending) = self.lifecycle.pending.as_ref() {
            return pending.sample;
        }
        let elapsed = now.saturating_duration_since(self.last_tick);
        self.last_tick = now;
        self.simulation_elapsed = self.simulation_elapsed.saturating_add(elapsed);
        FrameClockSample {
            id: SceneSampleId::Live(self.lifecycle.frames_presented),
            time_ms: self.simulation_elapsed.as_millis() as u32,
            delta_seconds: elapsed.as_secs_f32(),
        }
    }

    fn prepare_scene_update(
        &mut self,
        sample: FrameClockSample,
        viewport: [f32; 2],
    ) -> Result<SceneFrameUpdate, String> {
        let viewport = [viewport[0].max(1.0), viewport[1].max(1.0)];
        let aspect = viewport[0] / viewport[1];
        self.update_free_camera(sample.delta_seconds, viewport)?;
        self.update_material_clock(sample);
        self.update_controlled_component_geometry(sample)?;
        let material_update = sample_material_animations(
            &mut self.material_animations,
            &self.materials,
            self.material_time_ms,
        )?;
        let environment_material_update = self
            .environment_gpu
            .as_mut()
            .map(|environment_gpu| {
                sample_material_animations(
                    &mut environment_gpu.material_animations,
                    &self.materials,
                    self.material_time_ms,
                )
            })
            .transpose()?;
        let environment = self.update_environment(sample, viewport)?;
        Ok(SceneFrameUpdate {
            sample,
            camera: self.view_camera,
            viewport,
            aspect,
            material_update,
            environment_material_update,
            environment,
        })
    }

    fn set_mouse_look(&mut self, active: bool) {
        self.mouse_look = active;
        if let Some(window) = self.window.as_ref() {
            if active {
                // Relative device motion keeps the camera usable at the edge
                // of the window. Locked grab is unavailable on a few window
                // backends, so confined grab is a safe fallback.
                if window.set_cursor_grab(CursorGrabMode::Locked).is_err() {
                    let _ = window.set_cursor_grab(CursorGrabMode::Confined);
                }
            } else {
                let _ = window.set_cursor_grab(CursorGrabMode::None);
            }
            window.set_cursor_visible(!active);
        }
    }

    fn schedule_next_redraw(&self) {
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    fn complete(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(audio) = self.audio.as_mut() {
            audio.shutdown();
        }
        let Some(renderer) = self.renderer.take() else {
            self.error = Some("native Vulkan renderer was not initialized".to_string());
            event_loop.exit();
            return;
        };
        let report = match renderer.shutdown() {
            Ok(report) => report,
            Err(err) => {
                self.error = Some(err.to_string());
                event_loop.exit();
                return;
            }
        };
        self.window.take();
        if self.enable_validation
            && (report.validation.warning_count != 0 || report.validation.error_count != 0)
        {
            self.error = Some(format!(
                "native Vulkan validation must stay clean (warnings={}, errors={}, vuids={:?})",
                report.validation.warning_count,
                report.validation.error_count,
                report.validation.vuids,
            ));
            event_loop.exit();
            return;
        }
        let validation_status = if self.enable_validation {
            "clean"
        } else {
            "disabled"
        };
        let readback_path = match (&self.readback_out, &report.readback_artifact) {
            (Some(path), Some(artifact)) => {
                if let Some(parent) = path.parent() {
                    if let Err(err) = std::fs::create_dir_all(parent) {
                        self.error = Some(format!("{}: {err}", parent.display()));
                        event_loop.exit();
                        return;
                    }
                }
                if let Err(err) = std::fs::write(path, &artifact.bytes) {
                    self.error = Some(format!("{}: {err}", path.display()));
                    event_loop.exit();
                    return;
                }
                Some(path.display().to_string())
            }
            (Some(_), None) => {
                self.error =
                    Some("native Vulkan renderer produced no synchronized readback".to_string());
                event_loop.exit();
                return;
            }
            (None, _) => None,
        };
        let readback_format = report
            .readback_artifact
            .as_ref()
            .map_or(report.renderer_report.swapchain_image_format, |artifact| {
                artifact.format
            });
        let captured_crop = self.captured_viewport.map_or_else(
            || "none".to_string(),
            |viewport| {
                format!(
                    "{:?} ({}x{})",
                    viewport.rect, viewport.extent[0], viewport.extent[1]
                )
            },
        );
        self.output = Some(format!(
            "rendered mission {}: {} frames, {} objects, {} mesh components, {} terrain components, camera={}, captured_viewport_crop={}, materials={}, environment_primitives={}, shadow_casters={}, shadow_indices={}, swapchain_recreates={}, validation={}, swapchain_format={}, readback_format={}, readback={}, startup_ms={{mission_assets:{:.1},terrain_materials:{:.1},sky:{:.1},gpu_initialization:{:.1}}}",
            self.mission,
            self.lifecycle.frames_presented,
            self.object_count,
            self.mesh_components,
            self.terrain_components,
            self.camera_mode,
            captured_crop,
            self.materials.len(),
            self.environment_primitive_count,
            self.shadow_last_evidence.visible_casters,
            self.shadow_last_evidence.projected_indices,
            report.swapchain_recreate_count,
            validation_status,
            report.renderer_report.swapchain_image_format,
            readback_format,
            readback_path.as_deref().unwrap_or("none"),
            elapsed_ms(self.startup_timings.mission_assets),
            elapsed_ms(self.startup_timings.terrain_materials),
            elapsed_ms(self.startup_timings.sky),
            elapsed_ms(self.startup_timings.gpu_initialization),
        ));
        event_loop.exit();
    }

    fn finish(self) -> Result<String, String> {
        self.output.ok_or_else(|| {
            self.error.unwrap_or_else(|| {
                "native Vulkan mode exited before producing a report".to_string()
            })
        })
    }
}

impl ApplicationHandler for StaticVulkanApp {
    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: DeviceId,
        event: DeviceEvent,
    ) {
        if self.mouse_look {
            if let DeviceEvent::MouseMotion { delta } = event {
                if let Some(camera) = self.free_camera.as_mut() {
                    camera.look_delta(delta.0, delta.1);
                }
            }
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let plan = match WinitWindowPlan::smoke().validate() {
            Ok(plan) => plan,
            Err(err) => {
                self.error = Some(err.to_string());
                event_loop.exit();
                return;
            }
        };
        let (width, height) = self
            .captured_viewport
            .map(|viewport| (viewport.extent[0], viewport.extent[1]))
            .unwrap_or((plan.width, plan.height));
        let attributes = Window::default_attributes()
            .with_title("FParkan mission")
            .with_inner_size(WinitPhysicalSize::new(width, height))
            // The captured clip matrix is exact for the recorded viewport
            // aspect. Keep that drawable fixed; compare it with the same
            // (left, top, right, bottom) crop in the original capture.
            .with_resizable(self.captured_viewport.is_none());
        let window = match event_loop.create_window(attributes) {
            Ok(window) => window,
            Err(err) => {
                self.error = Some(format!("winit window: {err}"));
                event_loop.exit();
                return;
            }
        };
        let size = window.inner_size();
        if let Some(captured_viewport) = self.captured_viewport {
            if let Err(error) =
                validate_captured_drawable_extent(captured_viewport, [size.width, size.height])
            {
                self.error = Some(error);
                event_loop.exit();
                return;
            }
        }
        let Some(native_handles) = window_native_handles(&window) else {
            self.error = Some("winit window does not expose native handles".to_string());
            event_loop.exit();
            return;
        };
        let gpu_initialization_started = Instant::now();
        if let Err(error) = configure_world_material_categories(
            &mut self.mesh,
            &self.materials,
            &self.world_range_indices,
        ) {
            self.error = Some(error);
            event_loop.exit();
            return;
        }
        let mut renderer = match VulkanSmokeRenderer::new(&VulkanSmokeRendererCreateInfo {
            application_name: "fparkan-game".to_string(),
            native_handles,
            drawable_extent: (size.width.max(1), size.height.max(1)),
            render_request: WinitWindow::default_render_request(),
            enable_validation: self.enable_validation,
            mesh: self.mesh.clone(),
            camera: self.camera,
            materials: self.materials.clone(),
            bootstrap_progress: None,
        }) {
            Ok(renderer) => renderer,
            Err(error) => {
                self.error = Some(error.to_string());
                event_loop.exit();
                return;
            }
        };
        let background_ranges = self.environment_gpu.as_ref().map_or_else(
            Vec::new,
            EnvironmentGpuScene::transparent_background_range_indices,
        );
        if let Err(error) = renderer.set_world_transparent_background_ranges(&background_ranges) {
            self.error = Some(format!("configure world background ranges: {error}"));
            event_loop.exit();
            return;
        }
        if let Err(error) = self.configure_world_draw_ranges(&mut renderer) {
            self.error = Some(error);
            event_loop.exit();
            return;
        }
        match self.world_draw_sort_keys(self.view_camera.position) {
            Ok(sort_keys) => {
                for (range_index, sort_key) in sort_keys {
                    if let Err(error) = renderer.set_draw_range_sort_key(range_index, sort_key) {
                        self.error = Some(format!(
                            "update world draw range {range_index} sort key: {error}"
                        ));
                        event_loop.exit();
                        return;
                    }
                }
            }
            Err(error) => {
                self.error = Some(error);
                event_loop.exit();
                return;
            }
        }
        self.startup_timings.gpu_initialization = gpu_initialization_started.elapsed();
        renderer.set_readback_enabled(self.readback_out.is_some());
        self.window_id = Some(window.id());
        self.window = Some(window);
        self.renderer = Some(renderer);
        self.last_tick = Instant::now();
        self.simulation_elapsed = Duration::ZERO;
        self.material_time_ms = 0;
        self.schedule_next_redraw();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if Some(window_id) != self.window_id {
            return;
        }
        match event {
            WindowEvent::CloseRequested => {
                self.complete(event_loop);
            }
            WindowEvent::Focused(focused) => {
                if !focused {
                    self.pressed_keys.clear();
                    self.set_mouse_look(false);
                }
                if let Some(audio) = self.audio.as_mut() {
                    audio.on_focus_changed(focused);
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(code) = event.physical_key {
                    if code == KeyCode::Escape && event.state == ElementState::Pressed {
                        if self.mouse_look {
                            self.set_mouse_look(false);
                        } else {
                            self.complete(event_loop);
                        }
                    } else if event.state == ElementState::Pressed {
                        self.pressed_keys.insert(code);
                    } else {
                        self.pressed_keys.remove(&code);
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if button == MouseButton::Right {
                    self.set_mouse_look(state == ElementState::Pressed);
                }
            }
            WindowEvent::CursorMoved { .. } => {}
            WindowEvent::Resized(size) => {
                // Queued creation resize events can predate the current physical drawable.
                let drawable_size = if self.captured_viewport.is_some() {
                    self.window.as_ref().map(Window::inner_size).unwrap_or(size)
                } else {
                    size
                };
                if let Some(captured_viewport) = self.captured_viewport {
                    if drawable_size.width != 0 && drawable_size.height != 0 {
                        if let Err(error) = validate_captured_drawable_extent(
                            captured_viewport,
                            [drawable_size.width, drawable_size.height],
                        ) {
                            self.error = Some(error);
                            event_loop.exit();
                            return;
                        }
                    }
                }
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.request_resize((drawable_size.width, drawable_size.height));
                }
                if drawable_size.width == 0 || drawable_size.height == 0 {
                    event_loop.set_control_flow(ControlFlow::Wait);
                } else {
                    event_loop.set_control_flow(ControlFlow::Poll);
                }
            }
            WindowEvent::RedrawRequested => {
                if self.lifecycle.pending.is_none() {
                    return;
                }
                let drawable_available = self.window.as_ref().is_some_and(|window| {
                    let size = window.inner_size();
                    size.width != 0 && size.height != 0
                });
                if drawable_available {
                    if let Err(error) = self.apply_pending_scene_update() {
                        self.error = Some(error);
                        event_loop.exit();
                        return;
                    }
                }
                let outcome = {
                    let Some(renderer) = self.renderer.as_mut() else {
                        self.error = Some("native Vulkan renderer was not initialized".to_string());
                        event_loop.exit();
                        return;
                    };
                    renderer.draw_frame()
                };
                match outcome {
                    Ok(VulkanSmokeFrameOutcome::Presented) => {
                        self.lifecycle.record_presented();
                    }
                    Ok(VulkanSmokeFrameOutcome::Recreated) => {
                        if let Some((width, height)) = self
                            .renderer
                            .as_ref()
                            .map(|renderer| renderer.report().swapchain_extent)
                        {
                            if width != 0 && height != 0 {
                                if let Err(error) =
                                    self.refresh_pending_viewport([width as f32, height as f32])
                                {
                                    self.error = Some(error);
                                    event_loop.exit();
                                    return;
                                }
                            }
                        }
                    }
                    Ok(VulkanSmokeFrameOutcome::ZeroExtent) => {
                        if !drawable_available {
                            event_loop.set_control_flow(ControlFlow::Wait);
                        }
                    }
                    Err(err) => {
                        self.error = Some(err.to_string());
                        event_loop.exit();
                        return;
                    }
                }
                if self.target_frames != 0 && self.lifecycle.frames_presented >= self.target_frames
                {
                    self.complete(event_loop);
                } else if drawable_available {
                    self.schedule_next_redraw();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.output.is_some() || self.error.is_some() {
            return;
        }
        let Some(window) = self.window.as_ref() else {
            return;
        };
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            event_loop.set_control_flow(ControlFlow::Wait);
            return;
        }
        event_loop.set_control_flow(ControlFlow::Poll);
        if self.lifecycle.pending.is_none() {
            let viewport = self
                .captured_viewport
                .map_or([size.width as f32, size.height as f32], |captured| {
                    [captured.extent[0] as f32, captured.extent[1] as f32]
                });
            let sample = self.sample_frame_clock();
            let update = match self.prepare_scene_update(sample, viewport) {
                Ok(update) => update,
                Err(error) => {
                    self.error = Some(error);
                    event_loop.exit();
                    return;
                }
            };
            if !self.lifecycle.prepare(update) {
                return;
            }
        }
        self.schedule_next_redraw();
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Args {
    root: PathBuf,
    mission: String,
    frames: u64,
    atmosphere_seconds: Option<f32>,
    control_clock_ms: Option<u32>,
    validation: bool,
    readback_out: Option<PathBuf>,
    preview_roots: NonZeroUsize,
    legacy_camera_capture: Option<PathBuf>,
    static_animation_frame: Option<u16>,
    static_material_phase: Option<u16>,
}

impl Args {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut root = None;
        let mut mission = None;
        // Zero means keep the interactive preview open until the user closes
        // it. A bounded `--frames N` remains available for smoke/readback
        // runs and validation scripts.
        let mut frames = 0;
        let mut atmosphere_seconds = None;
        let mut control_clock_ms = None;
        let mut validation = false;
        let mut readback_out = None;
        // A native static-Vulkan invocation is the usable mission preview, so
        // its default scope covers every root. `PreviewRoots` is clamped by the
        // runtime to the decoded mission length; an explicit --preview-roots N
        // remains available for bounded diagnostic work.
        let mut preview_roots = NonZeroUsize::MAX;
        let mut legacy_camera_capture = None;
        let mut static_animation_frame = None;
        let mut static_material_phase = None;
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--root" => {
                    root = Some(
                        iter.next()
                            .map(PathBuf::from)
                            .ok_or_else(|| "--root requires a path".to_string())?,
                    );
                }
                "--mission" => {
                    mission = Some(
                        iter.next()
                            .cloned()
                            .ok_or_else(|| "--mission requires a path".to_string())?,
                    );
                }
                "--frames" => {
                    frames = iter
                        .next()
                        .ok_or_else(|| "--frames requires a value".to_string())?
                        .parse()
                        .map_err(|_| "--frames must be an integer".to_string())?;
                }
                "--atmosphere-seconds" => {
                    let value = iter
                        .next()
                        .ok_or_else(|| "--atmosphere-seconds requires a value".to_string())?
                        .parse::<f32>()
                        .map_err(|_| "--atmosphere-seconds must be a number".to_string())?;
                    if !value.is_finite() || value < 0.0 {
                        return Err(
                            "--atmosphere-seconds must be finite and non-negative".to_string()
                        );
                    }
                    atmosphere_seconds = Some(value);
                }
                "--control-clock-ms" => {
                    control_clock_ms = Some(
                        iter.next()
                            .ok_or_else(|| "--control-clock-ms requires a value".to_string())?
                            .parse()
                            .map_err(|_| "--control-clock-ms must be a u32 integer".to_string())?,
                    );
                }
                "--validation" => {
                    validation = true;
                }
                "--readback-out" => {
                    readback_out = Some(
                        iter.next()
                            .map(PathBuf::from)
                            .ok_or_else(|| "--readback-out requires a path".to_string())?,
                    );
                }
                "--preview-roots" => {
                    preview_roots = iter
                        .next()
                        .ok_or_else(|| "--preview-roots requires a value".to_string())?
                        .parse()
                        .map_err(|_| "--preview-roots must be a non-zero integer".to_string())?;
                }
                "--legacy-camera-capture" => {
                    legacy_camera_capture =
                        Some(iter.next().map(PathBuf::from).ok_or_else(|| {
                            "--legacy-camera-capture requires a path".to_string()
                        })?);
                }
                "--static-animation-frame" => {
                    static_animation_frame = Some(
                        iter.next()
                            .ok_or_else(|| "--static-animation-frame requires a value".to_string())?
                            .parse()
                            .map_err(|_| {
                                "--static-animation-frame must be a u16 integer".to_string()
                            })?,
                    );
                }
                "--static-material-phase" => {
                    static_material_phase = Some(
                        iter.next()
                            .ok_or_else(|| "--static-material-phase requires a value".to_string())?
                            .parse()
                            .map_err(|_| {
                                "--static-material-phase must be a u16 integer".to_string()
                            })?,
                    );
                }
                _ => return Err(usage()),
            }
        }
        let root = root.ok_or_else(|| "missing --root".to_string())?;
        let mission = mission.ok_or_else(|| "missing --mission".to_string())?;
        if control_clock_ms.is_some() && frames < 2 {
            return Err(
                "--control-clock-ms requires --frames of at least 2 to replay the animation clock"
                    .to_string(),
            );
        }
        Ok(Self {
            root,
            mission,
            frames,
            atmosphere_seconds,
            control_clock_ms,
            validation,
            readback_out,
            preview_roots,
            legacy_camera_capture,
            static_animation_frame,
            static_material_phase,
        })
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct LegacyCameraCapture {
    schema: String,
    render_input_usable: Option<bool>,
    selector0_words: [u32; 16],
    parented_camera: Option<bool>,
    effective_world_matrix_space: Option<String>,
    effective_world_matrix_words: Option<[u32; 16]>,
    viewport: [i32; 4],
    near_plane: f32,
    far_plane: f32,
    field_of_view_radians: f32,
    atmosphere_seconds: Option<f32>,
}

fn selected_atmosphere_seconds(
    command_line: Option<f32>,
    capture: Option<&LegacyCameraCapture>,
) -> Option<f32> {
    command_line.or_else(|| capture.and_then(|capture| capture.atmosphere_seconds))
}

impl LegacyCameraCapture {
    fn preview_matrix_words(&self) -> Result<[u32; 16], String> {
        match (
            self.effective_world_matrix_space.as_deref(),
            self.effective_world_matrix_words,
        ) {
            (Some("world"), Some(words)) => Ok(words),
            (Some(_), _) => Err(
                "legacy camera effective_world_matrix_space must be 'world'".to_string(),
            ),
            (None, Some(_)) => Err(
                "legacy camera effective_world_matrix_words requires an explicit world-space marker"
                    .to_string(),
            ),
            (None, None) if self.parented_camera == Some(true) => Err(
                "parented legacy camera requires an effective world-space matrix".to_string(),
            ),
            (None, None) => Ok(self.selector0_words),
        }
    }

    fn captured_viewport(&self) -> Result<CapturedViewport, String> {
        let width = self.viewport[2]
            .checked_sub(self.viewport[0])
            .filter(|width| *width > 0)
            .ok_or_else(|| "legacy camera capture has an invalid viewport width".to_string())?;
        let height = self.viewport[3]
            .checked_sub(self.viewport[1])
            .filter(|height| *height > 0)
            .ok_or_else(|| "legacy camera capture has an invalid viewport height".to_string())?;
        Ok(CapturedViewport {
            rect: self.viewport,
            extent: [
                u32::try_from(width)
                    .map_err(|_| "legacy camera viewport width exceeds u32".to_string())?,
                u32::try_from(height)
                    .map_err(|_| "legacy camera viewport height exceeds u32".to_string())?,
            ],
        })
    }

    fn preview_camera_frame(&self) -> Result<PreviewCameraFrame, String> {
        let frame = PreviewCameraFrame::from_legacy_d3d7(
            RawCameraTransform {
                words: self.preview_matrix_words()?,
            },
            LegacyD3d7Projection {
                viewport: self.viewport,
                near_plane: self.near_plane,
                far_plane: self.far_plane,
                field_of_view_radians: self.field_of_view_radians,
            },
        )
        .ok_or_else(|| "legacy camera capture contains an invalid D3D7 camera".to_string())?;
        self.captured_viewport()?;
        Ok(frame)
    }
}

fn load_legacy_camera_capture(path: &std::path::Path) -> Result<LegacyCameraCapture, String> {
    let bytes = std::fs::read(path).map_err(|err| format!("{}: {err}", path.display()))?;
    parse_legacy_camera_capture(&bytes).map_err(|err| format!("{}: {err}", path.display()))
}

fn parse_legacy_camera_capture(bytes: &[u8]) -> Result<LegacyCameraCapture, String> {
    // Windows PowerShell 5.1 writes redirected text as UTF-16LE with a BOM.
    // `capture-original-camera.ps1` intentionally emits plain JSON, so accept
    // that normal hand-off format as well as UTF-8 without making the caller
    // re-encode a read-only capture file.
    let json = decode_legacy_camera_capture_json(bytes)?;
    let capture: LegacyCameraCapture =
        serde_json::from_str(&json).map_err(|err| format!("invalid legacy camera JSON: {err}"))?;
    if capture.schema != "fparkan-legacy-camera-v1" {
        return Err("unsupported legacy camera capture schema".to_string());
    }
    if capture.effective_world_matrix_space.is_some()
        != capture.effective_world_matrix_words.is_some()
    {
        return Err(
            "effective_world_matrix_space and effective_world_matrix_words must be supplied together"
                .to_string(),
        );
    }
    if capture.render_input_usable == Some(false) {
        return Err("legacy camera capture is marked render_input_usable=false".to_string());
    }
    if capture
        .atmosphere_seconds
        .is_some_and(|seconds| !seconds.is_finite() || seconds < 0.0)
    {
        return Err(
            "legacy camera capture atmosphere_seconds must be finite and non-negative".to_string(),
        );
    }
    capture.preview_camera_frame()?;
    Ok(capture)
}

fn decode_legacy_camera_capture_json(bytes: &[u8]) -> Result<String, String> {
    let decode_utf16 = |words: Vec<u16>| {
        String::from_utf16(&words)
            .map_err(|err| format!("invalid UTF-16 legacy camera JSON: {err}"))
    };
    match bytes {
        [0xff, 0xfe, rest @ ..] => {
            let chunks = rest.chunks_exact(2);
            if !chunks.remainder().is_empty() {
                return Err("invalid UTF-16LE legacy camera JSON length".to_string());
            }
            decode_utf16(
                chunks
                    .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
                    .collect(),
            )
        }
        [0xfe, 0xff, rest @ ..] => {
            let chunks = rest.chunks_exact(2);
            if !chunks.remainder().is_empty() {
                return Err("invalid UTF-16BE legacy camera JSON length".to_string());
            }
            decode_utf16(
                chunks
                    .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
                    .collect(),
            )
        }
        [0xef, 0xbb, 0xbf, rest @ ..] => std::str::from_utf8(rest)
            .map(str::to_owned)
            .map_err(|err| format!("invalid UTF-8 legacy camera JSON: {err}")),
        _ => std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|err| format!("invalid UTF-8 legacy camera JSON: {err}")),
    }
}

fn usage() -> String {
    "usage: fparkan-game --root <path> --mission <path> [--frames <n>] [--atmosphere-seconds <seconds>] [--control-clock-ms <u32>] [--validation] [--preview-roots <non-zero n>] [--legacy-camera-capture <path>] [--static-animation-frame <u16>] [--static-material-phase <u16>] [--readback-out <path>]\nstatic preview uses saved per-component CTLD defaults, with uncontrolled nodes at frame 0; --static-animation-frame overrides all components globally. --control-clock-ms replays one shared MSH/material/environment clock from zero to the selected time over --frames frames.".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_fx::environment::{WeatherInput, WeatherState};
    use fparkan_material::{MaterialAnimationBlock, MaterialKey};

    fn material_test_phase(parameters: [u8; 18]) -> MaterialPhase {
        MaterialPhase {
            parameters,
            texture_raw: [0; 16],
        }
    }

    fn random_material_binding(range_index: usize) -> MaterialAnimationBinding {
        let mut overlay_phase = [0; 18];
        overlay_phase[..3].copy_from_slice(&[100, 50, 25]);
        overlay_phase[3] = 25;
        overlay_phase[4..7].copy_from_slice(&[30, 60, 90]);
        overlay_phase[8..11].copy_from_slice(&[40, 80, 120]);
        overlay_phase[16] = 5;
        let mut active_phase = [0; 18];
        active_phase[..3].copy_from_slice(&[25, 50, 75]);
        active_phase[3] = 75;
        active_phase[4..7].copy_from_slice(&[64, 128, 192]);
        active_phase[8..11].copy_from_slice(&[100, 150, 200]);
        active_phase[16] = 9;
        active_phase[17] = 1;
        let document = Mat0Document {
            version: 0,
            animation_block_count: 1,
            phases: vec![
                material_test_phase(overlay_phase),
                material_test_phase(active_phase),
            ],
            prefix: Vec::new(),
            header_opaque: [0; 2],
            animation_blocks: vec![MaterialAnimationBlock {
                header_raw: 3,
                keys: vec![
                    MaterialKey {
                        k0: 0,
                        k1: 20,
                        k2: 0,
                    },
                    MaterialKey {
                        k0: 1,
                        k1: 40,
                        k2: 0,
                    },
                ],
                bytes: Vec::new(),
            }],
        };
        let frozen_binding = |phase_index: usize, uv_transform: [f32; 4]| MaterialPhaseBinding {
            document: document.clone(),
            animation_block_index: 0,
            wear_row_start_ms: 0,
            random_state: 0,
            frozen_phase_index: Some(phase_index),
            phase_uv_transforms: vec![uv_transform; 2],
        };
        let texture = solid_environment_texture([255; 4]);
        MaterialAnimationBinding {
            initial_phase_index: 0,
            variants: vec![MaterialVariantSet {
                phase_material_indices: vec![4, 5],
                range_indices: vec![range_index],
            }],
            base: MaterialPhaseBinding {
                document: document.clone(),
                animation_block_index: 0,
                wear_row_start_ms: 0,
                random_state: 0,
                frozen_phase_index: None,
                phase_uv_transforms: vec![[0.0, 0.0, 0.5, 0.5], [0.5, 0.25, 0.25, 0.5]],
            },
            detail: Some(frozen_binding(1, [0.1, 0.2, 0.3, 0.4])),
            overlay: Some(frozen_binding(0, [0.3, 0.4, 0.5, 0.6])),
            overlay_detail: Some(frozen_binding(1, [0.6, 0.7, 0.8, 0.9])),
            stage_textures: Some(MaterialStageTextures {
                detail: Some(vec![texture.clone(), texture.clone()]),
                overlay: Some(vec![texture.clone()]),
                overlay_detail: Some(vec![texture.clone(), texture]),
                applied_material_phases: HashMap::from([(5, [1, 0, 1])]),
            }),
        }
    }

    fn packet_test_materials() -> Vec<VulkanStaticMaterial> {
        let mut materials = Vec::new();
        for _ in 0..6 {
            append_environment_gradient_material(&mut materials).expect("test material");
        }
        materials[4].pipeline_category = 1;
        materials[5].pipeline_category = 3;
        materials
    }

    #[test]
    fn sampled_material_packet_keeps_native_selector_properties_and_stage_phases() {
        let materials = packet_test_materials();
        let mut bindings = vec![random_material_binding(7)];
        let packet = sample_material_animations(&mut bindings, &materials, 500)
            .expect("sample random MAT0 state");

        assert_eq!(
            packet.range_selections,
            vec![MaterialRangeSelection {
                range_index: 7,
                material_index: 5,
                pipeline_category: 3,
            }]
        );
        assert_eq!(
            packet.active_materials.get(&7),
            Some(&ActiveMaterialState {
                material_index: 5,
                diffuse_alpha: 0.75,
            })
        );
        let properties = packet.properties[0];
        assert_eq!(
            (properties.diffuse_alpha, properties.overlay_alpha),
            (0.75, 0.25)
        );
        assert_eq!(
            (properties.base_directional, properties.base_additive),
            (
                [64.0 / 255.0, 128.0 / 255.0, 192.0 / 255.0],
                [25.0 / 255.0, 50.0 / 255.0, 75.0 / 255.0],
            )
        );
        assert_eq!(
            (properties.overlay_directional, properties.overlay_additive),
            (
                Some([30.0 / 255.0, 60.0 / 255.0, 90.0 / 255.0]),
                Some([100.0 / 255.0, 50.0 / 255.0, 25.0 / 255.0]),
            )
        );
        assert_eq!(
            (properties.specular_rgb, properties.specular_power),
            ([100.0 / 255.0, 150.0 / 255.0, 200.0 / 255.0], 9)
        );
        assert_eq!(
            (properties.overlay_specular_rgb, properties.overlay_power),
            (Some([40.0 / 255.0, 80.0 / 255.0, 120.0 / 255.0]), Some(5))
        );
        assert_eq!(
            properties.uv_transforms,
            [
                [0.5, 0.25, 0.25, 0.5],
                [0.1, 0.2, 0.3, 0.4],
                [0.3, 0.4, 0.5, 0.6],
                [0.6, 0.7, 0.8, 0.9],
            ]
        );
        assert_eq!(
            packet.stage_phases,
            vec![MaterialStagePhaseUpdate {
                binding_index: 0,
                material_index: 5,
                phases: [1, 0, 1],
            }]
        );
        assert_eq!(bindings[0].base.random_state, 2_531_011);
    }

    fn control_pose_test_model() -> ModelAsset {
        let mut nodes = vec![u8::MAX; 38];
        nodes[..2].copy_from_slice(&[0, 0]);
        nodes[2..4].copy_from_slice(&u16::MAX.to_le_bytes());
        nodes[4..6].copy_from_slice(&0_u16.to_le_bytes());
        nodes[6..8].copy_from_slice(&1_u16.to_le_bytes());
        ModelAsset {
            node_stride: 38,
            node_count: 1,
            nodes_raw: nodes,
            bounding_sphere_center: [0.0; 3],
            bounding_sphere_radius: 1.0,
            slots: vec![],
            positions: vec![],
            normals: None,
            uv0: None,
            uv1: None,
            indices: vec![],
            batches: vec![],
            triangle_descriptors: None,
            node_names: None,
            animation: Some(fparkan_msh::ModelAnimation {
                keys: vec![
                    fparkan_animation::AnimKey24 {
                        time: AnimationTime(0.0),
                        pose: fparkan_animation::Pose::default(),
                    },
                    fparkan_animation::AnimKey24 {
                        time: AnimationTime(1.0),
                        pose: fparkan_animation::Pose {
                            translation: [0.0, 2.0, 0.0],
                            rotation: [0.0, 0.0, 0.0, 1.0],
                        },
                    },
                ],
                frame_map: vec![0, 1],
                frame_count: 2,
            }),
        }
    }

    fn control_pose_drawable_test_model() -> ModelAsset {
        let mut model = control_pose_test_model();
        model.nodes_raw[8..10].copy_from_slice(&0_u16.to_le_bytes());
        model.slots = vec![fparkan_msh::Slot {
            tri_start: 0,
            tri_count: 1,
            batch_start: 0,
            batch_count: 1,
            aabb_min: [0.0; 3],
            aabb_max: [1.0; 3],
            sphere_center: [0.0; 3],
            sphere_radius: 1.0,
            opaque: [0; 5],
        }];
        model.positions = vec![[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        model.normals = Some(vec![[0, 0, 127, 0]; 3]);
        model.uv0 = Some(vec![[0, 0]; 3]);
        model.uv1 = None;
        model.indices = vec![0, 1, 2];
        model.batches = vec![fparkan_msh::Batch {
            batch_flags: 0,
            material_index_hi: 0,
            material_index: 0,
            lightmap_index: u8::MAX,
            local_batch_index: 0,
            index_count: 3,
            index_start: 0,
            vertex_count: 3,
            base_vertex: 0,
        }];
        model
    }

    fn control_pose_test_control() -> PreparedComponentControl {
        let mut fixed_words = [0; 44];
        fixed_words[2] = 0;
        fixed_words[6] = u32::MAX;
        fixed_words[9] = 1.0_f32.to_bits();
        fixed_words[10] = 1.0_f32.to_bits();
        let archive = || {
            fparkan_path::normalize_relative(b"test.rlb", fparkan_path::PathPolicy::HostCompatible)
                .expect("test archive path")
        };
        PreparedComponentControl {
            source: fparkan_resource::ResourceKey {
                archive: archive(),
                name: ResourceName(b"animated.ctl".to_vec()),
                type_id: None,
            },
            model_node_count: 1,
            native_profile_words: [0; 27],
            bindings: vec![],
            timeline_rows: vec![fparkan_assets::PreparedControlTimelineRow {
                row_index: 0,
                node_index: 0,
                frame_a: 0.0,
                frame_b: 1.0,
                initial_blend: 0.0,
                raw_parameters: [u32::MAX, u32::MAX, 1.0_f32.to_bits(), 0],
                flags: 0,
            }],
            typed_records: vec![fparkan_assets::PreparedControlTypedRecord {
                tag: 21,
                fixed_words,
                values: vec![0],
                string_bytes: vec![],
                gate_provider: PreparedControlGateProvider::Resolved {
                    key: 0,
                    ndp_source: fparkan_resource::ResourceKey {
                        archive: archive(),
                        name: ResourceName(b"animated.ndp".to_vec()),
                        type_id: None,
                    },
                    row_count: 1,
                    initial_type1_value: 1.0,
                },
            }],
            node_data: None,
            effect_section: fparkan_assets::PreparedControlEffectSection::Absent,
            initial_effect_group: None,
            point_list: None,
        }
    }

    #[test]
    fn control_timeline_and_ndp_gate_change_component_pose_at_sampled_time() {
        let model = control_pose_test_model();
        let control = control_pose_test_control();
        let gate_values = initial_control_gate_values(&control);
        let mut raw_state = ControlInstanceState::default();
        let raw_initial = sample_control_node_times(&control, 0, &gate_values, &mut raw_state)
            .expect("initial raw control timeline");
        let raw_later = sample_control_node_times(&control, 225, &gate_values, &mut raw_state)
            .expect("later raw control timeline");
        assert_eq!(raw_initial, vec![Some(0.0)]);
        assert!((raw_later[0].expect("node 0 time") - 0.225).abs() < 1.0e-5);

        let mut state = ControlInstanceState::default();
        let initial = static_preview_control_pose_buffer(&model, &control, 0, &mut state)
            .expect("initial control pose")
            .expect("sampled initial hierarchy");
        let later = static_preview_control_pose_buffer(&model, &control, 225, &mut state)
            .expect("later control pose")
            .expect("sampled later hierarchy");

        assert_eq!(initial.poses[0].translation, [0.0; 3]);
        assert!(
            (later.poses[0].translation[1] - 0.45).abs() < 1.0e-4,
            "later sampled pose: {:?}",
            later.poses[0]
        );

        let mut closed_gate = control.clone();
        let ndp_source = match &control.typed_records[0].gate_provider {
            PreparedControlGateProvider::Resolved { ndp_source, .. } => ndp_source.clone(),
            _ => panic!("fixture has a resolved NDP source"),
        };
        closed_gate.typed_records[0].gate_provider = PreparedControlGateProvider::Resolved {
            key: 0,
            ndp_source,
            row_count: 1,
            initial_type1_value: 0.0,
        };
        let mut closed_state = ControlInstanceState::default();
        let closed_initial =
            static_preview_control_pose_buffer(&model, &closed_gate, 0, &mut closed_state)
                .expect("closed-gate initial pose")
                .expect("closed-gate initial hierarchy");
        let closed_later =
            static_preview_control_pose_buffer(&model, &closed_gate, 225, &mut closed_state)
                .expect("closed-gate control pose")
                .expect("closed-gate hierarchy");
        assert_eq!(initial.poses[0], closed_initial.poses[0]);
        assert_eq!(closed_initial.poses[0], closed_later.poses[0]);
        assert_ne!(later.poses[0], closed_later.poses[0]);

        let mut mismatched_control = control.clone();
        mismatched_control.model_node_count += 1;
        let mismatch = static_preview_control_pose_buffer(
            &model,
            &mismatched_control,
            225,
            &mut ControlInstanceState::default(),
        )
        .expect_err("control/mesh node-count mismatch is rejected");
        assert!(mismatch.contains("does not match MSH node count"));
    }

    #[test]
    fn exto_parent_control_updates_visible_child_mount_without_draw_geometry() {
        let mut model = control_pose_test_model();
        let mut socket_node = vec![u8::MAX; model.node_stride];
        socket_node[..2].copy_from_slice(&[0, 0]);
        socket_node[2..4].copy_from_slice(&0_u16.to_le_bytes());
        socket_node[4..6].copy_from_slice(&u16::MAX.to_le_bytes());
        socket_node[6..8].copy_from_slice(&1_u16.to_le_bytes());
        model.nodes_raw.extend(socket_node);
        model.node_count = 2;
        let mut control = control_pose_test_control();
        control.model_node_count = 2;
        let unit_record = |immediate_child_count| fparkan_prototype::UnitComponentRecord {
            archive_raw: [0; 32],
            resource_raw: [0; 32],
            kind: 0,
            parent_or_link: 1,
            description_raw: [0; 32],
            tail0: 0,
            immediate_child_count,
        };
        let unit_tree = unit_component_tree(&[unit_record(1), unit_record(1), unit_record(0)])
            .expect("preorder Unit DAT tree");
        let controls = vec![None, Some(control.clone()), None];
        assert!(unit_component_is_visible(1, Some(PROTOTYPE_TYPE_EXTO)));
        assert!(unit_component_is_visible(2, Some(PROTOTYPE_TYPE_EXTO)));
        assert!(component_chain_has_tag21(Some(&unit_tree), &controls, 2));
        assert_eq!(unit_tree[1].parent_index, Some(0));
        assert_eq!(unit_tree[2].parent_index, Some(1));
        assert!(
            unit_tree[2]
                .parent_index
                .is_some_and(|parent_index| parent_index < 2),
            "Unit DAT preorder places a parent before its child"
        );
        let mut state = ControlInstanceState::default();
        let initial_hierarchy = static_preview_control_pose_buffer(&model, &control, 0, &mut state)
            .expect("initial EXTO parent pose")
            .expect("initial parent hierarchy");
        let later_hierarchy = static_preview_control_pose_buffer(&model, &control, 225, &mut state)
            .expect("animated EXTO parent pose")
            .expect("animated parent hierarchy");
        let mut pose_buffers = vec![vec![None, Some(initial_hierarchy), None]];
        let mounts = [DynamicComponentMount {
            object_index: 0,
            component_index: 2,
            parent_index: 1,
            socket: 1,
        }];
        let initial = node38_pose_from_hierarchy(
            pose_buffers[0][1].as_ref().expect("EXTO parent hierarchy"),
            1,
        )
        .expect("parent socket");
        let mut mount_poses = vec![vec![None, None, Some(initial)]];
        pose_buffers[0][1] = Some(later_hierarchy);
        let parent_geometry = DynamicComponentGeometry {
            object_index: 0,
            component_index: 1,
            model: Arc::new(model.clone()),
            transform: LegacyIron3dEulerTransform {
                translation: [0.0; 3],
                orientation_radians: [0.0; 3],
            },
            scale: [1.0; 3],
            vertex_start: 0,
            vertex_count: 0,
            shadow_triangle_range: None,
        };
        let empty_parent_mesh = project_dynamic_component_geometry(
            &parent_geometry,
            pose_buffers[0][1].as_ref().expect("EXTO parent hierarchy"),
            None,
        )
        .expect("zero-geometry EXTO parent");
        assert!(empty_parent_mesh.vertices.is_empty());
        assert!(empty_parent_mesh.indices.is_empty());

        assert!(
            refresh_dynamic_component_mounts(&mounts, &pose_buffers, &mut mount_poses)
                .expect("refresh child mount")
        );
        assert_eq!(
            mount_poses[0][2].expect("child mount").translation,
            [0.0, 2.45, 0.0]
        );
        // The EXTO parent has a sampled Node38 hierarchy but no projected draw
        // vertices. Its hierarchy still drives the visible child's mount.
    }

    #[test]
    fn live_tick_and_pending_resize_keep_the_scene_sample_while_refreshing_projection() {
        let model = Arc::new(control_pose_drawable_test_model());
        let control = control_pose_test_control();
        let dynamic = DynamicComponentGeometry {
            object_index: 0,
            component_index: 0,
            model: Arc::clone(&model),
            transform: LegacyIron3dEulerTransform {
                translation: [0.0; 3],
                orientation_radians: [0.0; 3],
            },
            scale: [1.0; 3],
            vertex_start: 0,
            vertex_count: 3,
            shadow_triangle_range: Some(0..1),
        };
        let mut state = ControlInstanceState::default();
        let initial_pose = static_preview_control_pose_buffer(&model, &control, 0, &mut state)
            .expect("initial hierarchy")
            .expect("initial Node38 pose");
        let initial_mesh = project_dynamic_component_geometry(&dynamic, &initial_pose, None)
            .expect("initial projected component");
        let initial_triangles = static_mesh_shadow_triangles(&initial_mesh);
        let baseline = ShadowSphere {
            center: [0.0; 3],
            radius: 1.0,
        };
        let mut land = fparkan_terrain_format::LandMeshDocument {
            streams: Vec::new(),
            nodes_raw: Vec::new(),
            slots: fparkan_terrain_format::TerrainSlotTable {
                header_raw: Vec::new(),
                slots_raw: Vec::new(),
            },
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            normals: Vec::new(),
            uv0: Vec::new(),
            accelerator: Vec::new(),
            aux14: Vec::new(),
            aux18: Vec::new(),
            faces: vec![fparkan_terrain_format::TerrainFace28 {
                flags: FullSurfaceMask(1),
                material_tag: 0,
                aux_tag: 0,
                vertices: [0, 1, 2],
                neighbors: [None, None, None],
                tail_raw: [0; 8],
                raw: [0; 28],
            }],
        };
        set_test_terrain_lod0_range(&mut land);
        let terrain = Arc::new(TerrainWorld::from_land_msh(&land).expect("test terrain"));
        let free_camera = FreeFlightCamera {
            position: [0.25, 0.25, 0.0],
            yaw: 0.0,
            pitch: 0.0,
            horizontal_fov: 1.0,
            near_plane: 0.1,
            far_plane: 100.0,
            move_speed: 1.0,
            ground_clearance: 0.0,
        };
        let view_camera = free_camera.preview_frame(16.0 / 9.0);
        let preview = StaticPreviewScene {
            mesh: initial_mesh.clone(),
            native_handedness_by_range: HashMap::new(),
            materials: Vec::new(),
            material_animations: Vec::new(),
            sun_occlusion_range_indices: Vec::new(),
            shadow_casters: vec![ShadowCaster {
                id: 0,
                world_axes: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                sphere: baseline,
                native_kind: 4,
                triangles: initial_triangles.clone(),
            }],
            shadow_sphere_baselines: vec![baseline],
            shadow_receivers: Vec::new(),
            shadow_material_index: 0,
            terrain,
            component_pose_buffers: vec![vec![Some(initial_pose)]],
            component_mount_poses: vec![vec![None]],
            control_animation_instances: vec![ControlAnimationInstance {
                object_index: 0,
                component_index: 0,
                model: Arc::clone(&model),
                control,
                state,
            }],
            dynamic_component_geometry: vec![dynamic],
            dynamic_component_mounts: Vec::new(),
            shadow_caster_indices_by_object: vec![Some(0)],
            camera: view_camera.vulkan_camera(),
            view_camera,
            captured_viewport: None,
            free_camera: None,
            camera_mode: "test",
            mesh_components: 1,
            terrain_components: 0,
        };
        let mut app = StaticVulkanApp::new(
            preview,
            None,
            None,
            EnvironmentGpuRange {
                range_index: usize::MAX,
                vertex_start: initial_mesh.vertices.len(),
                vertex_capacity: 0,
                material_index: 0,
            },
            initial_mesh.vertices.len(),
            initial_mesh.indices.len(),
            None,
            0,
            None,
            "control-test",
            1,
            None,
            false,
            StartupTimings::default(),
        );
        app.free_camera = Some(free_camera);
        app.pressed_keys.insert(KeyCode::KeyW);
        app.materials = packet_test_materials();
        app.material_animations = vec![random_material_binding(0)];
        let empty_range = app.shadow_range;
        app.environment_gpu = Some(EnvironmentGpuScene {
            material_animations: vec![random_material_binding(1)],
            sky_layers: Vec::new(),
            sprites: Vec::new(),
            rain: empty_range,
            snow: empty_range,
            lightning: empty_range,
            lightning_material_index: 0,
        });
        let start = app.last_tick;
        let sample = app.sample_live_frame_clock_at(start + Duration::from_millis(225));
        assert_eq!(sample.time_ms, 225);
        assert!((sample.delta_seconds - 0.225).abs() < f32::EPSILON);
        let mut update = app
            .prepare_scene_update(sample, [1280.0, 720.0])
            .expect("apply one full live tick to the scene");
        assert!(update.environment.is_none());
        assert!(update.environment_material_update.is_some());
        let world_material_packet = update.material_update.clone();
        let sky_material_packet = update.environment_material_update.clone();
        assert_eq!(app.material_animations[0].base.random_state, 2_531_011);
        assert_eq!(
            app.environment_gpu.as_ref().unwrap().material_animations[0]
                .base
                .random_state,
            2_531_011
        );
        let expected_resized_camera = app
            .free_camera
            .expect("free-flight input controller")
            .preview_frame(4.0 / 3.0);
        let camera_after_tick = app.free_camera.expect("free-flight camera").position;
        assert!((camera_after_tick[0] - 0.35).abs() < 1.0e-5);
        assert!((camera_after_tick[1] - 0.25).abs() < 1.0e-5);
        assert_eq!(app.material_time_ms, sample.time_ms);
        let weather = WeatherInput {
            rain: WeatherState::new(true, 0.03, [0.7, 0.8, 1.0, 0.6]),
            snow: WeatherState::new(true, 0.03, [0.9, 0.95, 1.0, 0.8]),
            ..WeatherInput::clear()
        };
        let mut weather_system = EnvironmentSystem::new(0xC1_225);
        let first_environment_frame = weather_system.update(
            0.225,
            weather,
            environment_camera_for_preview(&update.camera, update.aspect, update.viewport),
        );
        let mut precipitation_counts = [0usize; 2];
        for primitive in &first_environment_frame.primitives {
            if let EnvironmentPrimitive::Particle { kind, .. } = primitive {
                let slot = match kind {
                    PrecipitationKind::Rain => 0,
                    PrecipitationKind::Snow => 1,
                };
                precipitation_counts[slot] += 1;
            }
        }
        assert!(precipitation_counts.iter().all(|count| *count > 0));
        update.environment = Some(DynamicEnvironmentFrame {
            atmosphere: TypedAtmosphere {
                marker: 0,
                version: 0,
                tracks: Vec::new(),
                trailer: Default::default(),
            }
            .sample(0.225),
            sky_mesh: SkySystem::new(Default::default(), SkyMaterials::default())
                .expect("test sky topology")
                .mesh()
                .clone(),
            environment_frame: first_environment_frame.clone(),
            absolute_time_seconds: 0.225,
        });
        assert!(app.lifecycle.prepare(update.clone()));
        let control_state_after_tick = app.control_animation_instances[0].state.clone();
        assert_eq!(
            app.sample_live_frame_clock_at(start + Duration::from_millis(500)),
            sample
        );
        assert_eq!(app.simulation_elapsed, Duration::from_millis(225));
        assert_eq!(app.lifecycle.pending.as_ref(), Some(&update));

        assert_eq!(app.mesh.indices, initial_mesh.indices);
        assert_eq!(app.mesh.vertices.len(), initial_mesh.vertices.len());
        assert_ne!(
            initial_mesh.vertices[0].position,
            app.mesh.vertices[0].position
        );
        assert!((app.mesh.vertices[0].position[1] - 0.45).abs() < 1.0e-4);
        let b_vertices = app.mesh.vertices.clone();
        let b_triangles = app.shadow_casters[0].triangles.clone();
        let b_sphere = app.shadow_casters[0].sphere;
        assert_ne!(b_triangles, initial_triangles);
        assert!(b_sphere.radius > baseline.radius);

        app.free_camera.as_mut().unwrap().look_delta(64.0, -24.0);
        let controller_after_input = app.free_camera.unwrap();
        app.refresh_pending_viewport([800.0, 600.0])
            .expect("refresh only viewport-dependent state after recreation");
        let refreshed = app
            .lifecycle
            .pending
            .as_ref()
            .expect("pending sample")
            .clone();
        assert_eq!(refreshed.sample, sample);
        assert_eq!(refreshed.material_update, world_material_packet);
        assert_eq!(refreshed.environment_material_update, sky_material_packet);
        assert_eq!(app.material_animations[0].base.random_state, 2_531_011);
        assert_eq!(
            app.environment_gpu.as_ref().unwrap().material_animations[0]
                .base
                .random_state,
            2_531_011
        );
        app.refresh_pending_viewport([1024.0, 768.0])
            .expect("refresh the same pending sample a second time");
        let repeated = app.lifecycle.pending.as_ref().expect("pending sample");
        assert_eq!(repeated.sample, sample);
        assert_eq!(repeated.material_update, world_material_packet);
        assert_eq!(repeated.environment_material_update, sky_material_packet);
        assert_eq!(repeated.viewport, [1024.0, 768.0]);
        assert_eq!(repeated.aspect, 4.0 / 3.0);
        assert_eq!(app.material_animations[0].base.random_state, 2_531_011);
        assert_eq!(
            app.environment_gpu.as_ref().unwrap().material_animations[0]
                .base
                .random_state,
            2_531_011
        );
        assert_eq!(refreshed.viewport, [800.0, 600.0]);
        assert_eq!(refreshed.aspect, 4.0 / 3.0);
        assert_eq!(refreshed.camera.position, update.camera.position);
        assert_eq!(refreshed.camera.forward, update.camera.forward);
        assert_eq!(refreshed.camera.right, update.camera.right);
        assert_eq!(refreshed.camera.up, update.camera.up);
        assert_eq!(refreshed.camera.position, expected_resized_camera.position);
        assert_eq!(refreshed.camera.forward, expected_resized_camera.forward);
        assert_eq!(refreshed.camera.right, expected_resized_camera.right);
        assert_eq!(refreshed.camera.up, expected_resized_camera.up);
        assert!(
            (refreshed.camera.vertical_fov - expected_resized_camera.vertical_fov).abs() < 1.0e-6
        );
        assert!(refreshed
            .camera
            .clip_from_world
            .iter()
            .zip(expected_resized_camera.clip_from_world)
            .all(|(actual, expected)| (actual - expected).abs() < 1.0e-5));
        assert_ne!(
            refreshed.camera.clip_from_world,
            update.camera.clip_from_world
        );
        assert_eq!(app.free_camera, Some(controller_after_input));
        let refreshed_environment_frame =
            &refreshed.environment.as_ref().unwrap().environment_frame;
        assert_eq!(
            refreshed_environment_frame.point_lights,
            first_environment_frame.point_lights
        );
        assert_eq!(
            refreshed_environment_frame.sounds,
            first_environment_frame.sounds
        );
        let mut reprojected_counts = [0usize; 2];
        for (sampled, refreshed) in first_environment_frame
            .primitives
            .iter()
            .zip(&refreshed_environment_frame.primitives)
        {
            let (
                EnvironmentPrimitive::Particle {
                    kind: sampled_kind,
                    position: sampled_position,
                    velocity: sampled_velocity,
                    size: sampled_size,
                    screen: sampled_screen,
                    ..
                },
                EnvironmentPrimitive::Particle {
                    kind,
                    position,
                    velocity,
                    size,
                    screen,
                    ..
                },
            ) = (sampled, refreshed)
            else {
                panic!("rain and snow fixture contains only precipitation particles");
            };
            assert_eq!(kind, sampled_kind);
            assert_eq!(position, sampled_position);
            assert_eq!(velocity, sampled_velocity);
            assert_ne!(screen, sampled_screen);
            assert_ne!(size, sampled_size);
            let slot = match kind {
                PrecipitationKind::Rain => 0,
                PrecipitationKind::Snow => 1,
            };
            reprojected_counts[slot] += 1;
        }
        assert!(reprojected_counts.iter().all(|count| *count > 0));
        assert_eq!(
            app.apply_pending_scene_update().unwrap_err(),
            "native Vulkan renderer was not initialized"
        );
        let restored = app
            .lifecycle
            .pending
            .as_ref()
            .expect("restored pending sample");
        assert_eq!(restored.sample, sample);
        assert_eq!(restored.material_update, world_material_packet);
        assert_eq!(restored.environment_material_update, sky_material_packet);
        assert_eq!(app.material_animations[0].base.random_state, 2_531_011);
        assert_eq!(
            app.environment_gpu.as_ref().unwrap().material_animations[0]
                .base
                .random_state,
            2_531_011
        );
        assert_eq!(app.mesh.vertices, b_vertices);
        assert_eq!(app.shadow_casters[0].triangles, b_triangles);
        assert_eq!(
            app.control_animation_instances[0].state,
            control_state_after_tick
        );
        assert_eq!(app.material_time_ms, sample.time_ms);
        assert_eq!(app.simulation_elapsed, Duration::from_millis(225));
        assert_eq!(app.lifecycle.frames_presented, 0);
        assert_eq!(app.lifecycle.record_presented(), Some(0));

        app.update_controlled_component_geometry(sample)
            .expect("repeat the same control timestamp");
        assert_eq!(app.mesh.vertices, b_vertices);
        assert_eq!(app.shadow_casters[0].triangles, b_triangles);
        assert_eq!(app.shadow_casters[0].sphere, b_sphere);

        let first_sample = FrameClockSample {
            id: SceneSampleId::Live(0),
            time_ms: 0,
            delta_seconds: 0.0,
        };
        app.update_material_clock(first_sample);
        app.update_controlled_component_geometry(first_sample)
            .expect("return to first control pose");
        assert_eq!(app.mesh.vertices, initial_mesh.vertices);
        assert_eq!(app.shadow_casters[0].triangles, initial_triangles);
        assert_eq!(app.shadow_casters[0].sphere, baseline);

        let next_sample = app.sample_live_frame_clock_at(start + Duration::from_millis(241));
        assert_eq!(next_sample.id, SceneSampleId::Live(1));
        assert_eq!(next_sample.time_ms, 241);
        let next_update = app
            .prepare_scene_update(next_sample, [800.0, 600.0])
            .expect("the next sampled frame adopts mouse input received during retry");
        assert_ne!(next_update.camera.forward, update.camera.forward);
        assert_eq!(next_update.camera.forward, controller_after_input.forward());
    }

    fn set_test_terrain_lod0_range(terrain: &mut fparkan_terrain_format::LandMeshDocument) {
        const NODE_STRIDE: usize = 38;
        const SLOT_STRIDE: usize = 68;
        let mut node = vec![u8::MAX; NODE_STRIDE];
        node[8..10].copy_from_slice(&0_u16.to_le_bytes());
        node[10..12].copy_from_slice(&u16::MAX.to_le_bytes());
        let mut slot = [0_u8; SLOT_STRIDE];
        slot[..2].copy_from_slice(&0_u16.to_le_bytes());
        let face_count = u16::try_from(terrain.faces.len()).expect("test face count fits");
        slot[2..4].copy_from_slice(&face_count.to_le_bytes());
        terrain.nodes_raw = node;
        terrain.slots.slots_raw = vec![slot];
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    fn scene_frame_update(sample: FrameClockSample, viewport: [f32; 2]) -> SceneFrameUpdate {
        let aspect = viewport[0] / viewport[1];
        let camera = FreeFlightCamera {
            position: [0.0; 3],
            yaw: 0.0,
            pitch: 0.0,
            horizontal_fov: 1.0,
            near_plane: 0.1,
            far_plane: 100.0,
            move_speed: 1.0,
            ground_clearance: 0.0,
        }
        .preview_frame(aspect);
        SceneFrameUpdate {
            sample,
            camera,
            viewport,
            aspect,
            material_update: MaterialFrameUpdate::default(),
            environment_material_update: None,
            environment: None,
        }
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_corpus_part1_prepares_every_mission() {
        prepare_licensed_corpus("FPARKAN_CORPUS_PART1_ROOT", 29);
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_corpus_part2_prepares_every_mission() {
        prepare_licensed_corpus("FPARKAN_CORPUS_PART2_ROOT", 31);
    }

    fn prepare_licensed_corpus(variable: &str, expected_missions: usize) {
        let root = std::env::var_os(variable)
            .map(PathBuf::from)
            .unwrap_or_else(|| panic!("{variable} is required"));
        assert!(
            root.is_dir(),
            "licensed corpus root is missing: {}",
            root.display()
        );

        let mut missions = Vec::new();
        let mut directories = vec![root.clone()];
        while let Some(directory) = directories.pop() {
            let entries = std::fs::read_dir(&directory)
                .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()));
            for entry in entries {
                let path = entry
                    .unwrap_or_else(|error| {
                        panic!("read entry in {}: {error}", directory.display())
                    })
                    .path();
                if path.is_dir() {
                    directories.push(path);
                } else if path
                    .file_name()
                    .is_some_and(|name| name.eq_ignore_ascii_case("data.tma"))
                {
                    let relative = path
                        .strip_prefix(&root)
                        .expect("mission path is under corpus root")
                        .to_str()
                        .expect("mission path is UTF-8")
                        .replace('\\', "/");
                    missions.push(relative);
                }
            }
        }
        missions.sort();
        assert_eq!(
            missions.len(),
            expected_missions,
            "installed mission count for {variable}"
        );

        let vfs: Arc<dyn fparkan_vfs::Vfs> = Arc::new(DirectoryVfs::new(&root));
        let mut prepared = 0usize;
        let mut environment_missions = 0usize;
        for mission in missions {
            let mut engine = create(EngineServices::new(vfs.clone())).expect("engine");
            let loaded = load_mission_static_preview_roots(
                &mut engine,
                MissionRequest {
                    key: mission.clone(),
                    mode: MissionMode::SingleMission,
                },
                NonZeroUsize::MAX,
            )
            .unwrap_or_else(|error| panic!("prepare runtime mission {mission}: {error}"));
            let assets = loaded_mission_assets(&engine).expect("mission assets");
            let terrain = loaded_terrain(&engine).expect("mission terrain");
            let roots = loaded_mission_object_drafts(&engine).expect("mission object drafts");
            let preview = static_preview_mesh_and_materials(
                assets,
                terrain,
                roots,
                None,
                None,
                None,
                &root,
                &loaded.land_msh_path,
            )
            .unwrap_or_else(|error| panic!("prepare renderer mission {mission}: {error}"));
            assert!(!preview.mesh.vertices.is_empty(), "{mission}: empty mesh");
            assert!(
                !preview.materials.is_empty(),
                "{mission}: empty material set"
            );

            if load_environment(&root, &mission, None, false)
                .unwrap_or_else(|error| panic!("prepare environment mission {mission}: {error}"))
                .is_some()
            {
                environment_missions += 1;
            }

            // `GameAudio::new` resolves and decodes every sample declared by
            // mission.cfg before opening the OS device. A headless CI host is
            // therefore still a useful resource check: only the final device
            // step may be unavailable.
            match audio::GameAudio::new(vfs.clone(), &mission) {
                Ok(mut audio) => audio.shutdown(),
                Err(audio::AudioError::DeviceUnavailable { .. }) => {}
                Err(error) => panic!("prepare audio mission {mission}: {error}"),
            }
            prepared += 1;
        }

        assert_eq!(prepared, expected_missions);
        assert_eq!(environment_missions, expected_missions);
    }

    #[test]
    fn draw_range_reorder_remaps_all_material_animation_bindings() {
        let range = |first_index, material_index| VulkanStaticDrawRange {
            first_index,
            index_count: 3,
            material_index,
            lightmap_index: u8::MAX,
            batch_flags: 0,
            pipeline_state: world_pipeline_state(
                LegacyBlendMode::Opaque,
                LegacyDepthMode::TestReadOnly,
            ),
            terrain_base_only: false,
            alpha_test_reference: 0,
        };
        let mut mesh = VulkanStaticMesh {
            vertices: Vec::new(),
            indices: (0..9).collect(),
            draw_ranges: vec![range(0, 10), range(3, 20), range(6, 30)],
        };
        let empty_binding = || MaterialPhaseBinding {
            document: Mat0Document {
                version: 0,
                animation_block_count: 0,
                phases: Vec::new(),
                prefix: Vec::new(),
                header_opaque: [0; 2],
                animation_blocks: Vec::new(),
            },
            animation_block_index: 0,
            wear_row_start_ms: 0,
            random_state: 0,
            frozen_phase_index: Some(0),
            phase_uv_transforms: Vec::new(),
        };
        let mut bindings = vec![MaterialAnimationBinding {
            initial_phase_index: 0,
            variants: vec![MaterialVariantSet {
                phase_material_indices: vec![10],
                range_indices: vec![0, 2],
            }],
            base: empty_binding(),
            detail: None,
            overlay: None,
            overlay_detail: None,
            stage_textures: None,
        }];

        let remap = reorder_draw_ranges(&mut mesh, &[2, 0, 1]).expect("reorder");
        remap_material_animation_ranges(&mut bindings, &remap).expect("binding remap");

        assert_eq!(
            mesh.indices,
            (6..9).chain(0..3).chain(3..6).collect::<Vec<_>>()
        );
        assert_eq!(mesh.draw_ranges[0].material_index, 30);
        assert_eq!(mesh.draw_ranges[1].material_index, 10);
        assert_eq!(mesh.draw_ranges[2].material_index, 20);
        assert_eq!(bindings[0].variants[0].range_indices, vec![1, 0]);
    }

    #[test]
    fn native_world_transparency_uses_sampled_alpha_and_native_batch_flags() {
        let range = |blend, batch_flags| VulkanStaticDrawRange {
            first_index: 0,
            index_count: 3,
            material_index: 0,
            lightmap_index: u8::MAX,
            batch_flags,
            pipeline_state: world_pipeline_state(blend, LegacyDepthMode::TestReadOnly),
            terrain_base_only: false,
            alpha_test_reference: 0,
        };

        // Transparency is driven by the native predicate. The pipeline blend
        // enum is an independent rendering detail and does not qualify an
        // opaque range on its own.
        assert!(!native_world_transparent(
            &range(LegacyBlendMode::SourceAlpha, 0),
            1.0
        ));
        assert!(native_world_transparent(
            &range(LegacyBlendMode::Opaque, 0),
            0.999
        ));
        assert!(native_world_transparent(
            &range(LegacyBlendMode::Opaque, 0x100),
            1.0
        ));
        assert!(native_world_transparent(
            &range(LegacyBlendMode::Opaque, 0x8),
            1.0
        ));
    }

    #[test]
    fn material_animation_selects_pipeline_category_with_active_phase() {
        let variant = MaterialVariantSet {
            phase_material_indices: vec![10, 11],
            range_indices: vec![3],
        };
        let category_for = |material_index| match material_index {
            10 => Some(1),
            11 => Some(2),
            _ => None,
        };

        assert_eq!(
            selected_material_phase(&variant, 0, category_for),
            Ok((10, 1))
        );
        assert_eq!(
            selected_material_phase(&variant, 1, category_for),
            Ok((11, 2))
        );
        assert!(selected_material_phase(&variant, 2, category_for).is_err());
    }

    #[test]
    fn native_shadow_kind_mapping_keeps_sun_eligibility_separate_from_caster_defaults() {
        assert_eq!(native_shadow_kind(Some(PROTOTYPE_TYPE_FORT)), Some(3));
        assert_eq!(native_shadow_kind(Some(PROTOTYPE_TYPE_BTLU)), Some(4));
        assert_eq!(native_shadow_kind(Some(PROTOTYPE_TYPE_STAT)), Some(10));
        assert!(!native_shadow_caster_enabled(3));
        assert!(native_shadow_caster_enabled(4));
        assert!(!native_shadow_caster_enabled(10));
        assert!(native_sun_object_kind_eligible(Some(PROTOTYPE_TYPE_FORT)));
        assert!(native_sun_object_kind_eligible(Some(PROTOTYPE_TYPE_BTLU)));
        assert!(native_sun_object_kind_eligible(Some(PROTOTYPE_TYPE_STAT)));
        assert!(!native_sun_object_kind_eligible(Some(PROTOTYPE_TYPE_EXTO)));
        assert!(!native_sun_object_kind_eligible(None));
    }

    fn test_shadow_caster(id: u32, center: [f32; 3]) -> ShadowCaster {
        ShadowCaster {
            id,
            world_axes: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            sphere: ShadowSphere {
                center,
                radius: 0.0,
            },
            native_kind: 4,
            triangles: Vec::new(),
        }
    }

    #[test]
    fn shadow_frustum_filters_before_the_native_twenty_caster_budget() {
        let casters = [
            test_shadow_caster(1, [202.0, 0.0, 0.0]),
            test_shadow_caster(2, [0.0, 0.0, 0.0]),
        ];
        let visible =
            native_visible_shadow_casters(&casters, VulkanStaticCamera::default().clip_from_world);
        assert_eq!(
            visible.iter().map(|caster| caster.id).collect::<Vec<_>>(),
            [2]
        );
    }

    #[test]
    fn shadow_frustum_keeps_the_native_hundred_unit_sphere_margin() {
        let matrix = VulkanStaticCamera::default().clip_from_world;
        assert!(native_shadow_caster_visible(
            &test_shadow_caster(1, [100.99, 0.0, 0.0]),
            matrix
        ));
        assert!(!native_shadow_caster_visible(
            &test_shadow_caster(2, [101.0, 0.0, 0.0]),
            matrix
        ));
    }

    #[test]
    fn animated_shadow_bounds_rebuild_from_native_baseline() {
        let baseline = ShadowSphere {
            center: [0.0; 3],
            radius: 1.0,
        };
        let pose_a = [shadow_triangle(
            [[0.5, 0.0, 0.0], [0.0, 0.5, 0.0], [0.0, 0.0, 0.5]],
            0,
        )];
        let pose_b = [shadow_triangle(
            [[3.0, 0.0, 0.0], [4.0, 0.0, 0.0], [3.0, 1.0, 0.0]],
            0,
        )];

        let first_a =
            shadow_sphere_enclosing_triangles(baseline, &pose_a).expect("initial animated bounds");
        assert_eq!(first_a, baseline);
        let at_b =
            shadow_sphere_enclosing_triangles(baseline, &pose_b).expect("expanded animated bounds");
        assert!((at_b.center[0] - 1.5).abs() < 1.0e-5);
        assert!((at_b.radius - 2.5).abs() < 1.0e-5);
        assert!(length3(at_b.center).expect("finite sphere center") + 1.0 <= at_b.radius);
        for point in pose_b.iter().flat_map(|triangle| triangle.positions) {
            assert!(
                length3(sub3(point, at_b.center)).expect("finite point distance") <= at_b.radius
            );
        }
        let second_a = shadow_sphere_enclosing_triangles(baseline, &pose_a)
            .expect("return to first animated pose");
        assert_eq!(second_a, first_a);
    }

    #[test]
    fn mounted_shadow_bounds_ignore_child_translation_and_scale_aggregate_once() {
        let parent = VulkanNodePose {
            translation: [10.0, 20.0, 30.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
        };
        let node1 = VulkanNodePose {
            // Native bounds replace this translation with the accumulated
            // parent translation while retaining node 1's rotation.
            translation: [99.0, -77.0, 42.0],
            rotation: [0.0, 0.0, (0.5_f32).sqrt(), (0.5_f32).sqrt()],
        };
        let first = mounted_shadow_sphere([2.0, 0.0, 0.0], 2.0, Some(parent), Some(node1))
            .expect("finite mounted sphere");
        assert!((first.center[0] - 10.0).abs() < 1.0e-5);
        assert!((first.center[1] - 22.0).abs() < 1.0e-5);
        assert!((first.center[2] - 30.0).abs() < 1.0e-5);

        let second = ShadowComponentSphere {
            center: [12.0, 22.0, 30.0],
            radius: 1.0,
        };
        let caster = aggregate_shadow_caster(
            7,
            4,
            LegacyIron3dEulerTransform {
                translation: [1.0, 2.0, 3.0],
                orientation_radians: [0.0; 3],
            },
            [2.0, 3.0, 4.0],
            vec![shadow_triangle(
                [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
                0,
            )],
            &[first, second],
        )
        .expect("aggregate caster");
        let nearly_equal = |left: f32, right: f32| assert!((left - right).abs() < 1.0e-4);
        nearly_equal(caster.sphere.center[0], 22.333_334);
        nearly_equal(caster.sphere.center[1], 68.0);
        nearly_equal(caster.sphere.center[2], 123.0);
        nearly_equal(caster.sphere.radius, 10.666_667);
    }

    #[test]
    fn internal_components_do_not_enter_msh_socket_mount_path() {
        assert!(unit_component_is_visible(0, None));
        assert!(!unit_component_is_visible(1, None));
        assert!(unit_component_is_visible(1, Some(PROTOTYPE_TYPE_EXTO)));
    }

    #[test]
    fn parses_required_args() {
        assert_eq!(
            Args::parse(&strings(&[
                "--root",
                "testdata/IS",
                "--mission",
                "MISSIONS/Autodemo.00/data.tma",
                "--frames",
                "3",
            ])),
            Ok(Args {
                root: PathBuf::from("testdata/IS"),
                mission: "MISSIONS/Autodemo.00/data.tma".to_string(),
                frames: 3,
                atmosphere_seconds: None,
                control_clock_ms: None,
                validation: false,
                readback_out: None,
                preview_roots: NonZeroUsize::MAX,
                legacy_camera_capture: None,
                static_animation_frame: None,
                static_material_phase: None,
            })
        );
    }

    #[test]
    fn defaults_to_interactive_frame_loop() {
        let parsed = Args::parse(&strings(&[
            "--root",
            "testdata/IS",
            "--mission",
            "MISSIONS/Autodemo.00/data.tma",
        ]))
        .expect("default interactive arguments");
        assert_eq!(parsed.frames, 0);
        assert_eq!(parsed.preview_roots, NonZeroUsize::MAX);
        assert_eq!(parsed.static_animation_frame, None);
        assert_eq!(parsed.control_clock_ms, None);
    }

    #[test]
    fn replay_clock_stays_on_one_sample_until_presented() {
        let mut lifecycle = SceneFrameLifecycle::default();
        let first = lifecycle.sample_replay(500, 101);
        assert_eq!(first.id, SceneSampleId::Replay(0));
        assert_eq!(first.time_ms, 0);
        assert_eq!(first.delta_seconds, 0.0);
        let update = scene_frame_update(first, [1280.0, 720.0]);
        assert!(lifecycle.prepare(update.clone()));

        // A swapchain recreation retries the same packet and does not count a frame.
        assert_eq!(lifecycle.sample_replay(500, 101), first);
        assert_eq!(lifecycle.frames_presented, 0);
        assert_eq!(lifecycle.pending.as_ref(), Some(&update));
        assert_eq!(lifecycle.record_presented(), Some(0));

        let second = lifecycle.sample_replay(500, 101);
        assert_eq!(second.id, SceneSampleId::Replay(1));
        assert_eq!(second.time_ms, 5);
        assert!((second.delta_seconds - 0.005).abs() < f32::EPSILON);
    }

    #[test]
    fn replay_clock_requires_at_least_two_requested_frames() {
        for frame_count in ["0", "1"] {
            assert!(Args::parse(&strings(&[
                "--root",
                "testdata/IS",
                "--mission",
                "MISSIONS/Autodemo.00/data.tma",
                "--frames",
                frame_count,
                "--control-clock-ms",
                "500",
            ]))
            .unwrap_err()
            .contains("--frames of at least 2"));
        }

        let parsed = Args::parse(&strings(&[
            "--root",
            "testdata/IS",
            "--mission",
            "MISSIONS/Autodemo.00/data.tma",
            "--frames",
            "101",
            "--control-clock-ms",
            "500",
        ]))
        .expect("valid bounded replay arguments");
        assert_eq!(parsed.control_clock_ms, Some(500));
    }

    #[test]
    fn lightmap_ranges_stay_on_the_decorated_variant_when_phase_changes() {
        let empty_mat0 = Mat0Document {
            version: 0,
            animation_block_count: 0,
            phases: Vec::new(),
            prefix: Vec::new(),
            header_opaque: [0; 2],
            animation_blocks: Vec::new(),
        };
        let mut bindings = vec![MaterialAnimationBinding {
            initial_phase_index: 0,
            variants: vec![MaterialVariantSet {
                phase_material_indices: vec![10, 11],
                range_indices: Vec::new(),
            }],
            base: MaterialPhaseBinding {
                document: empty_mat0,
                animation_block_index: 0,
                wear_row_start_ms: 0,
                random_state: 0,
                frozen_phase_index: Some(0),
                phase_uv_transforms: Vec::new(),
            },
            detail: None,
            overlay: None,
            overlay_detail: None,
            stage_textures: None,
        }];
        let (binding_index, source_phases) =
            find_material_animation_variant(&bindings, 10).expect("base material variant");
        assert_eq!(source_phases, vec![10, 11]);
        bindings[binding_index].variants.push(MaterialVariantSet {
            phase_material_indices: vec![20, 21],
            range_indices: vec![7],
        });

        let lightmap_variant = &bindings[binding_index].variants[1];
        assert_eq!(lightmap_variant.range_indices, vec![7]);
        assert_eq!(
            selected_material_variant_index(lightmap_variant, 0),
            Some(20)
        );
        assert_eq!(
            selected_material_variant_index(lightmap_variant, 1),
            Some(21)
        );
    }

    fn row_vector_transform(point: [f32; 4], matrix: [f32; 16]) -> [f32; 4] {
        let mut result = [0.0; 4];
        for column in 0..4 {
            result[column] = (0..4)
                .map(|row| point[row] * matrix[row * 4 + column])
                .sum();
        }
        result
    }

    #[test]
    fn free_flight_camera_uses_right_handed_z_up_basis_and_positive_depth() {
        let camera = FreeFlightCamera {
            position: [0.0, 0.0, 0.0],
            yaw: 0.0,
            pitch: 0.0,
            horizontal_fov: std::f32::consts::FRAC_PI_2,
            near_plane: 1.0,
            far_plane: 11.0,
            move_speed: 1.0,
            ground_clearance: 0.0,
        };

        let nearly_equal = |left: f32, right: f32| assert!((left - right).abs() < 1.0e-5);
        assert_eq!(camera.forward(), [1.0, 0.0, 0.0]);
        assert_eq!(camera.right(), [0.0, -1.0, 0.0]);
        assert_eq!(camera.up(), [0.0, 0.0, 1.0]);
        let matrix = camera.vulkan_camera(1.0).clip_from_world;
        let near = row_vector_transform([1.0, 0.0, 0.0, 1.0], matrix);
        let far = row_vector_transform([11.0, 0.0, 0.0, 1.0], matrix);
        let camera_right = row_vector_transform([1.0, -1.0, 0.0, 1.0], matrix);
        let world_up = row_vector_transform([1.0, 0.0, 1.0, 1.0], matrix);
        nearly_equal(near[0], 0.0);
        nearly_equal(near[1], 0.0);
        nearly_equal(near[2], 0.0);
        nearly_equal(near[3], 1.0);
        assert!(camera_right[0] / camera_right[3] > 0.0);
        assert!(world_up[1] / world_up[3] < 0.0);
        nearly_equal(far[2] / far[3], 1.0);
    }

    #[test]
    fn free_flight_camera_uses_native_horizontal_fov_for_every_viewport_aspect() {
        let horizontal_fov = 1.3_f32;
        let camera =
            FreeFlightCamera::from_bounds([0.0; 3], [100.0; 3]).expect("finite free-flight bounds");
        assert_eq!(camera.horizontal_fov, horizontal_fov);
        assert_eq!(camera.near_plane, 0.5);
        assert_eq!(camera.far_plane, 700.0);

        let assert_edges = |frame: PreviewCameraFrame, aspect: f32| {
            let expected_vertical_fov = 2.0 * ((horizontal_fov * 0.5).tan() / aspect).atan();
            assert!((frame.vertical_fov - expected_vertical_fov).abs() < 1.0e-5);
            let depth = 10.0;
            let center = add3(frame.position, scale3(frame.forward, depth));
            let right_edge = add3(
                center,
                scale3(frame.right, depth * (horizontal_fov * 0.5).tan()),
            );
            let top_edge = add3(
                center,
                scale3(frame.up, depth * (expected_vertical_fov * 0.5).tan()),
            );
            let center_clip = row_vector_transform(
                [center[0], center[1], center[2], 1.0],
                frame.clip_from_world,
            );
            let right_clip = row_vector_transform(
                [right_edge[0], right_edge[1], right_edge[2], 1.0],
                frame.clip_from_world,
            );
            let top_clip = row_vector_transform(
                [top_edge[0], top_edge[1], top_edge[2], 1.0],
                frame.clip_from_world,
            );
            assert!(center_clip[3] > 0.0 && right_clip[3] > 0.0 && top_clip[3] > 0.0);
            assert!(
                center_clip[0].abs() < 5.0e-4,
                "aspect {aspect}: {center_clip:?}"
            );
            assert!(
                center_clip[1].abs() < 5.0e-4,
                "aspect {aspect}: {center_clip:?}"
            );
            assert!(
                (right_clip[0] / right_clip[3] - 1.0).abs() < 1.0e-4,
                "aspect {aspect}: {right_clip:?}"
            );
            assert!(
                (top_clip[1] / top_clip[3] + 1.0).abs() < 1.0e-4,
                "aspect {aspect}: {top_clip:?}"
            );
        };

        for aspect in [0.05, 0.75, 4.0 / 3.0, 16.0 / 9.0, 20.0] {
            assert_edges(camera.preview_frame(aspect), aspect);

            let projection = LegacyD3d7Projection {
                viewport: [0, 0, 1000, (1000.0 / aspect).round() as i32],
                near_plane: 0.5,
                far_plane: 700.0,
                field_of_view_radians: horizontal_fov,
            };
            let raw_transform = RawCameraTransform {
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
            let captured = PreviewCameraFrame::from_legacy_d3d7(raw_transform, projection)
                .expect("valid D3D7 projection");
            assert_edges(
                captured,
                projection.viewport[2] as f32 / projection.viewport[3] as f32,
            );
        }
    }

    #[test]
    fn direction_projection_accepts_front_offscreen_rays_but_rejects_horizon() {
        let camera = FreeFlightCamera {
            position: [0.0; 3],
            yaw: 0.0,
            pitch: 0.0,
            horizontal_fov: 1.3,
            near_plane: 0.5,
            far_plane: 700.0,
            move_speed: 1.0,
            ground_clearance: 0.0,
        }
        .preview_frame(16.0 / 9.0);
        let viewport = [1280.0, 720.0];
        let offscreen = project_direction_pixels(&camera, [0.2, 0.979_795_9, 0.0], viewport)
            .expect("front-facing directions are not rejected by the near distance");
        assert!(offscreen[0] < 0.0 || offscreen[0] > viewport[0]);
        assert!(project_direction_pixels(&camera, [1.0e-5, 1.0, 0.0], viewport).is_none());
        assert!(project_direction_pixels(&camera, [-1.0, 0.0, 0.0], viewport).is_none());

        let ray = screen_ray_from_pixels(&camera, [viewport[0] * 1.1, viewport[1] * 0.5], viewport)
            .expect("an offscreen pixel still has a view ray");
        let ray_pixels = project_direction_pixels(&camera, ray, viewport)
            .expect("the offscreen ray remains in the front hemisphere");
        assert!((ray_pixels[0] - viewport[0] * 1.1).abs() < 1.0e-3);
        assert!((ray_pixels[1] - viewport[1] * 0.5).abs() < 1.0e-3);
    }

    #[test]
    fn native_sky_heading_selects_matching_gradient_sectors_for_both_camera_origins() {
        use fparkan_fx::atmosphere::{PackedColor, SkySample, SKY_COLOR_COUNT};

        let mut colors = [PackedColor(0xFF00_0000); SKY_COLOR_COUNT];
        colors[..4].copy_from_slice(&[
            PackedColor(0xFF11_0000),
            PackedColor(0xFF00_2200),
            PackedColor(0xFF00_0033),
            PackedColor(0xFF44_4400),
        ]);
        let sample = SkySample {
            colors,
            values: [0.0; 2],
            packed: [PackedColor(0); 2],
        };
        let projection = LegacyD3d7Projection {
            viewport: [0, 0, 1280, 720],
            near_plane: 0.5,
            far_plane: 700.0,
            field_of_view_radians: 1.596_851_4,
        };

        let cardinal_forwards: [([f32; 3], usize); 4] = [
            ([0.0, 1.0, 0.0], 0),
            ([1.0, 0.0, 0.0], 1),
            ([0.0, -1.0, 0.0], 2),
            ([-1.0, 0.0, 0.0], 3),
        ];
        for (forward, sector) in cardinal_forwards {
            let free_flight = FreeFlightCamera {
                position: [0.0; 3],
                yaw: forward[1].atan2(forward[0]),
                pitch: 0.0,
                horizontal_fov: projection.field_of_view_radians,
                near_plane: projection.near_plane,
                far_plane: projection.far_plane,
                move_speed: 1.0,
                ground_clearance: 0.0,
            }
            .preview_frame(1280.0 / 720.0);

            let right = normalize3(cross3(forward, [0.0, 0.0, 1.0])).expect("camera right");
            let up = normalize3(cross3(right, forward)).expect("camera up");
            let raw_transform = RawCameraTransform {
                words: [
                    forward[0].to_bits(),
                    (-right[0]).to_bits(),
                    up[0].to_bits(),
                    0.0_f32.to_bits(),
                    forward[1].to_bits(),
                    (-right[1]).to_bits(),
                    up[1].to_bits(),
                    0.0_f32.to_bits(),
                    forward[2].to_bits(),
                    (-right[2]).to_bits(),
                    up[2].to_bits(),
                    0.0_f32.to_bits(),
                    0.0_f32.to_bits(),
                    0.0_f32.to_bits(),
                    0.0_f32.to_bits(),
                    1.0_f32.to_bits(),
                ],
            };
            let captured = PreviewCameraFrame::from_legacy_d3d7(raw_transform, projection)
                .expect("captured camera frame");

            for (origin, camera) in [("free flight", free_flight), ("captured", captured)] {
                let gradient = fparkan_fx::sky::screen_gradient_frame(
                    &sample,
                    camera.native_sky_heading(),
                    [0.0; 3],
                );
                assert_eq!(
                    gradient.color, sample.colors[sector],
                    "{origin} camera heading for forward {forward:?}"
                );
            }
        }
    }

    #[test]
    fn captured_native_camera_uses_view_dependent_sky_fog_color() {
        use fparkan_fx::atmosphere::{PackedColor, SkySample, SKY_COLOR_COUNT};

        // Compact camera fields from the native 1493.337-second AutoDemo capture.
        let capture = parse_legacy_camera_capture(
            br#"{"schema":"fparkan-legacy-camera-v1","render_input_usable":true,"parented_camera":true,"effective_world_matrix_space":"world","effective_world_matrix_words":[1052770830,1064129158,991881820,1141874407,3211610778,1052772554,3144770972,1145115441,3146624790,3127057462,1065352580,1087889619,0,0,0,1065353216],"selector0_words":[1063123992,3204380634,996787387,3130169600,1056894806,1063125889,990301923,3129510813,3146181515,3070164857,1065352633,1080608413,0,0,0,1065353216],"viewport":[0,0,1280,1024],"near_plane":5.0,"far_plane":700.0,"field_of_view_radians":1.3,"atmosphere_seconds":1493.337}"#,
        )
        .expect("valid native camera fixture");
        let camera = capture.preview_camera_frame().expect("valid camera frame");

        let mut colors = [PackedColor(0xFF00_0000); SKY_COLOR_COUNT];
        colors[..4].copy_from_slice(&[
            PackedColor(0xFF38_3864),
            PackedColor(0xFF38_3864),
            PackedColor(0xFF40_4064),
            PackedColor(0xFF38_3864),
        ]);
        let sample = SkySample {
            colors,
            values: [0.0; 2],
            packed: [PackedColor(0); 2],
        };
        let fog_color =
            fparkan_fx::sky::screen_gradient_frame(&sample, camera.native_sky_heading(), [0.0; 3])
                .color;

        assert_eq!(capture.atmosphere_seconds, Some(1493.337));
        assert_eq!(fog_color, PackedColor(0xFF3E_3E64));
    }

    #[test]
    fn sky_normals_keep_native_signed_byte_scale_without_renormalizing() {
        assert_eq!(sky_normal([-128, 127, -64]), [-1.0, 127.0 / 128.0, -0.5]);
    }

    #[test]
    fn sun_visibility_uses_a_valid_ray_and_fails_open_without_surfaces() {
        let camera = FreeFlightCamera {
            position: [10.0, 20.0, 30.0],
            yaw: 0.0,
            pitch: 0.0,
            horizontal_fov: 1.0,
            near_plane: 0.1,
            far_plane: 100.0,
            move_speed: 1.0,
            ground_clearance: 0.0,
        }
        .preview_frame(16.0 / 9.0);
        let terrain = TerrainWorld::default();
        assert!(native_sun_unoccluded(&terrain, &camera, [1.0, 0.0, 0.0]));
        assert!(!native_sun_unoccluded(&terrain, &camera, [0.0, 0.0, 0.0]));

        let mut land = fparkan_terrain_format::LandMeshDocument {
            streams: Vec::new(),
            nodes_raw: Vec::new(),
            slots: fparkan_terrain_format::TerrainSlotTable {
                header_raw: Vec::new(),
                slots_raw: Vec::new(),
            },
            // Nonzero source Z is shared by the camera, terrain and shadows.
            positions: vec![[0.0, 0.0, 32.0], [1.0, 0.0, 32.0], [0.0, 1.0, 32.0]],
            normals: Vec::new(),
            uv0: Vec::new(),
            accelerator: Vec::new(),
            aux14: Vec::new(),
            aux18: Vec::new(),
            faces: vec![fparkan_terrain_format::TerrainFace28 {
                flags: FullSurfaceMask(1),
                material_tag: 0,
                aux_tag: 0,
                vertices: [0, 1, 2],
                neighbors: [None, None, None],
                tail_raw: [0; 8],
                raw: [0; 28],
            }],
        };
        set_test_terrain_lod0_range(&mut land);
        let shadow_receivers = terrain_shadow_receivers(&land).expect("raw-height receivers");
        assert_eq!(shadow_receivers[0].positions[0], [0.0, 0.0, 32.0]);
        let mut invalid_shadow_face = land.clone();
        invalid_shadow_face.faces[0].vertices[2] = u16::MAX;
        assert!(terrain_shadow_receivers(&invalid_shadow_face)
            .expect_err("bad terrain indices must fail instead of inventing vertices")
            .contains("unknown vertex 65535"));
        let terrain = TerrainWorld::from_land_msh(&land).expect("synthetic terrain");
        assert_eq!(terrain.height_at([0.25, 0.25]).unwrap(), Some(32.0));
        let floor_camera =
            FreeFlightCamera::from_mesh_and_terrain(&VulkanStaticMesh::smoke_triangle(), &terrain)
                .expect("camera above nonzero terrain");
        assert_eq!(floor_camera.position[2], 34.0);
        assert_eq!(floor_camera.ground_clearance, 2.0);
        let blocked_camera = FreeFlightCamera {
            position: [0.25, 0.25, 34.0],
            yaw: 0.0,
            pitch: 0.0,
            horizontal_fov: 1.0,
            near_plane: 0.1,
            far_plane: 100.0,
            move_speed: 1.0,
            ground_clearance: 0.0,
        }
        .preview_frame(16.0 / 9.0);
        assert!(!native_sun_unoccluded(
            &terrain,
            &blocked_camera,
            [0.0, 0.0, -1.0]
        ));

        // The same hit is beyond a short native sun segment, so it must not
        // occlude the light even though the terrain contains a surface.
        let short_segment_camera = PreviewCameraFrame {
            far_plane: 1.0,
            ..blocked_camera
        };
        assert!(native_sun_unoccluded(
            &terrain,
            &short_segment_camera,
            [0.0, 0.0, -1.0]
        ));

        let world_mesh = VulkanStaticMesh::smoke_triangle();
        assert!(!native_sun_unoccluded_with_world(
            &TerrainWorld::default(),
            &world_mesh,
            &[0],
            &blocked_camera,
            [0.0, 0.0, -1.0]
        ));
        assert!(native_sun_unoccluded_with_world(
            &TerrainWorld::default(),
            &world_mesh,
            &[],
            &blocked_camera,
            [0.0, 0.0, -1.0]
        ));

        let mut excluded_land = fparkan_terrain_format::LandMeshDocument {
            streams: Vec::new(),
            nodes_raw: Vec::new(),
            slots: fparkan_terrain_format::TerrainSlotTable {
                header_raw: Vec::new(),
                slots_raw: Vec::new(),
            },
            positions: vec![[0.0, 0.0, 32.0], [1.0, 0.0, 32.0], [0.0, 1.0, 32.0]],
            normals: Vec::new(),
            uv0: Vec::new(),
            accelerator: Vec::new(),
            aux14: Vec::new(),
            aux18: Vec::new(),
            faces: vec![fparkan_terrain_format::TerrainFace28 {
                flags: FullSurfaceMask(0x20),
                material_tag: 0,
                aux_tag: 0,
                vertices: [0, 1, 2],
                neighbors: [None, None, None],
                tail_raw: [0; 8],
                raw: [0; 28],
            }],
        };
        set_test_terrain_lod0_range(&mut excluded_land);
        let excluded_terrain =
            TerrainWorld::from_land_msh(&excluded_land).expect("excluded terrain");
        assert!(native_sun_unoccluded_with_world(
            &excluded_terrain,
            &world_mesh,
            &[],
            &blocked_camera,
            [0.0, 0.0, -1.0]
        ));

        let mut excluded_world_mesh = world_mesh.clone();
        excluded_world_mesh.draw_ranges[0].batch_flags = 0x0008;
        assert!(native_sun_unoccluded_with_world(
            &TerrainWorld::default(),
            &excluded_world_mesh,
            &[0],
            &blocked_camera,
            [0.0, 0.0, -1.0]
        ));
    }

    #[test]
    fn free_flight_camera_keeps_clearance_after_descending_and_moving_uphill() {
        let mut land = fparkan_terrain_format::LandMeshDocument {
            streams: Vec::new(),
            nodes_raw: Vec::new(),
            slots: fparkan_terrain_format::TerrainSlotTable {
                header_raw: Vec::new(),
                slots_raw: Vec::new(),
            },
            positions: vec![[0.0, 0.0, 32.0], [1.0, 0.0, 34.0], [0.0, 1.0, 36.0]],
            normals: Vec::new(),
            uv0: Vec::new(),
            accelerator: Vec::new(),
            aux14: Vec::new(),
            aux18: Vec::new(),
            faces: vec![fparkan_terrain_format::TerrainFace28 {
                flags: FullSurfaceMask(1),
                material_tag: 0,
                aux_tag: 0,
                vertices: [0, 1, 2],
                neighbors: [None, None, None],
                tail_raw: [0; 8],
                raw: [0; 28],
            }],
        };
        set_test_terrain_lod0_range(&mut land);
        let terrain = TerrainWorld::from_land_msh(&land).expect("sloped terrain");
        assert_eq!(terrain.height_at([0.25, 0.25]).unwrap(), Some(33.5));
        assert_eq!(terrain.height_at([0.75, 0.25]).unwrap(), Some(34.5));

        let mut camera = FreeFlightCamera {
            position: [0.25, 0.25, 37.0],
            yaw: 0.0,
            pitch: 0.0,
            horizontal_fov: 1.0,
            near_plane: 0.1,
            far_plane: 100.0,
            move_speed: 4.0,
            ground_clearance: 2.0,
        };
        let mut keys = HashSet::from([KeyCode::KeyQ]);
        camera.advance(&keys, 1.0);
        camera
            .keep_above_terrain(&terrain)
            .expect("floor query after descent");
        assert_eq!(camera.position, [0.25, 0.25, 35.5]);

        keys.clear();
        keys.insert(KeyCode::KeyW);
        camera.move_speed = 1.0;
        camera.advance(&keys, 0.5);
        camera
            .keep_above_terrain(&terrain)
            .expect("floor query after uphill movement");
        assert_eq!(camera.position, [0.75, 0.25, 36.5]);

        let mut outside_camera = FreeFlightCamera {
            position: [2.0, -2.0, -17.0],
            ground_clearance: 2.0,
            ..camera
        };
        outside_camera
            .keep_above_terrain(&terrain)
            .expect("clamp to the map before sampling the floor");
        assert_eq!(outside_camera.position, [1.0, 0.0, 36.0]);

        outside_camera.position = [-2.0, 2.0, 100.0];
        outside_camera
            .keep_above_terrain(&terrain)
            .expect("clamp the other map edges without limiting altitude");
        assert_eq!(outside_camera.position, [0.0, 1.0, 100.0]);

        outside_camera.move_speed = 40.0;
        outside_camera.advance(&HashSet::from([KeyCode::KeyW, KeyCode::ShiftLeft]), 0.1);
        outside_camera
            .keep_above_terrain(&terrain)
            .expect("accelerated movement still stops at the map edge");
        assert_eq!(outside_camera.position, [1.0, 1.0, 100.0]);

        let mut unsupported_camera = FreeFlightCamera {
            position: [0.25, 0.25, 35.5],
            ground_clearance: 2.0,
            ..camera
        };
        assert!(unsupported_camera
            .keep_above_terrain(&TerrainWorld::default())
            .expect_err("terrain query errors must propagate")
            .contains("terrain height query"));
    }

    #[test]
    #[ignore = "requires the original GOG installation"]
    fn free_flight_camera_stops_at_original_map_edges_under_accelerated_movement() {
        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT")
            .expect("set FPARKAN_CORPUS_PART1_ROOT to the original GOG installation");
        let mut engine =
            create(EngineServices::new(Arc::new(DirectoryVfs::new(root)))).expect("engine");
        load_mission_static_preview(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/Autodemo.00/data.tma".to_string(),
                mode: MissionMode::SingleMission,
            },
        )
        .expect("original mission");
        let terrain = loaded_terrain(&engine).expect("original terrain");
        let [min, max] = terrain.surface_bounds().expect("original map bounds");
        let keys = HashSet::from([KeyCode::KeyW, KeyCode::ShiftLeft]);
        for (axis, yaw, edge) in [
            (0, 0.0, max[0]),
            (0, std::f32::consts::PI, min[0]),
            (1, std::f32::consts::FRAC_PI_2, max[1]),
            (1, -std::f32::consts::FRAC_PI_2, min[1]),
        ] {
            let mut camera = FreeFlightCamera::from_bounds(min, max).expect("camera");
            camera.position = [
                (min[0] + max[0]) * 0.5,
                (min[1] + max[1]) * 0.5,
                max[2] + 10.0,
            ];
            camera.yaw = yaw;
            camera.pitch = 0.0;
            for _ in 0..512 {
                camera.advance(&keys, 0.1);
                camera.keep_above_terrain(terrain).expect("map constraint");
                for axis in 0..2 {
                    assert!((min[axis]..=max[axis]).contains(&camera.position[axis]));
                }
            }
            assert_eq!(camera.position[axis], edge);
        }
    }

    #[test]
    fn world_sort_key_uses_the_first_indexed_vertex() {
        let mesh = VulkanStaticMesh::smoke_triangle();
        let distance = first_indexed_vertex_distance(&mesh, 0, [0.0, -0.55, -3.0])
            .expect("first indexed vertex distance");
        assert!((distance - 3.0).abs() < 1.0e-6);
    }

    #[test]
    fn free_flight_camera_advances_wasd_and_vertical_controls() {
        let mut camera = FreeFlightCamera {
            position: [0.0, 0.0, 0.0],
            yaw: 0.0,
            pitch: 0.0,
            horizontal_fov: 1.0,
            near_plane: 0.1,
            far_plane: 100.0,
            move_speed: 2.0,
            ground_clearance: 0.0,
        };
        let mut keys = HashSet::new();
        keys.insert(KeyCode::KeyW);
        keys.insert(KeyCode::KeyD);
        camera.advance(&keys, 1.0);
        let diagonal = 2.0 / 2.0_f32.sqrt();
        assert!((camera.position[0] - diagonal).abs() < 1.0e-5);
        assert!((camera.position[1] + diagonal).abs() < 1.0e-5);
        assert_eq!(camera.position[2], 0.0);

        keys.clear();
        keys.insert(KeyCode::KeyE);
        keys.insert(KeyCode::ShiftLeft);
        camera.advance(&keys, 0.5);
        assert!((camera.position[2] - 4.0).abs() < 1.0e-5);
    }

    #[test]
    fn free_flight_mouse_right_turns_toward_previous_right_and_vertical_is_world_z() {
        let mut camera = FreeFlightCamera {
            position: [0.0, 0.0, 0.0],
            yaw: 0.0,
            pitch: 0.6,
            horizontal_fov: 1.0,
            near_plane: 0.1,
            far_plane: 100.0,
            move_speed: 2.0,
            ground_clearance: 0.0,
        };
        let old_right = camera.right();
        let old_forward = camera.forward();
        camera.look_delta(100.0, 0.0);
        assert!(dot3(camera.forward(), old_right) > 0.0);
        assert!(dot3(camera.forward(), old_forward) < 1.0);

        let mut keys = HashSet::new();
        keys.insert(KeyCode::KeyE);
        camera.advance(&keys, 0.5);
        assert!((camera.position[0]).abs() < 1.0e-5);
        assert!((camera.position[1]).abs() < 1.0e-5);
        assert!((camera.position[2] - 1.0).abs() < 1.0e-5);
    }

    #[test]
    fn parses_validation_flag() {
        let parsed = Args::parse(&strings(&[
            "--root",
            "testdata/IS",
            "--mission",
            "MISSIONS/Autodemo.00/data.tma",
            "--validation",
        ]))
        .expect("valid validation flag");
        assert!(parsed.validation);
    }

    #[test]
    fn parses_nonzero_static_preview_root_count() {
        let parsed = Args::parse(&strings(&[
            "--root",
            "testdata/IS",
            "--mission",
            "MISSIONS/Autodemo.00/data.tma",
            "--preview-roots",
            "2",
        ]))
        .expect("valid static preview arguments");
        assert_eq!(
            parsed.preview_roots,
            NonZeroUsize::new(2).expect("non-zero literal")
        );
    }

    #[test]
    fn parses_readback_output() {
        let parsed = Args::parse(&strings(&[
            "--root",
            "testdata/IS",
            "--mission",
            "MISSIONS/Autodemo.00/data.tma",
            "--readback-out",
            "target/frame.raw",
        ]))
        .expect("valid readback output arguments");
        assert_eq!(parsed.readback_out, Some(PathBuf::from("target/frame.raw")));
    }

    #[test]
    fn parses_static_animation_frame() {
        let parsed = Args::parse(&strings(&[
            "--root",
            "testdata/IS",
            "--mission",
            "MISSIONS/Autodemo.00/data.tma",
            "--static-animation-frame",
            "12",
        ]))
        .expect("valid static animation frame");
        assert_eq!(parsed.static_animation_frame, Some(12));
    }

    #[test]
    fn static_preview_control_defaults_apply_native_flags_and_last_row_wins() {
        let binding =
            |node_index, frame_a, frame_b, initial_blend, flags| PreparedControlNodeBinding {
                node_index,
                frame_a,
                frame_b,
                initial_blend,
                flags,
            };
        let times = static_preview_node_times(
            6,
            &[
                binding(1, 1.0, 3.0, 0.0, 0),
                binding(2, 1.0, 3.0, 1.25, 1),
                binding(3, 1.0, 3.0, -0.5, 1),
                binding(4, 1.0, 3.0, -0.5, 0),
                binding(1, 1.0, 3.0, 0.25, 2),
                binding(usize::MAX, f32::NAN, f32::INFINITY, f32::NAN, 4),
            ],
        )
        .expect("valid static CTLD defaults");
        let values = times
            .iter()
            .map(|time| time.map(|time| time.0))
            .collect::<Vec<_>>();
        assert_eq!(
            values,
            vec![
                Some(0.0),
                Some(2.5),
                Some(1.5),
                Some(2.0),
                Some(1.0),
                Some(0.0)
            ]
        );
    }

    #[test]
    fn static_preview_rejects_negative_derived_control_sample_time() {
        let error = static_preview_node_times(
            1,
            &[PreparedControlNodeBinding {
                node_index: 0,
                frame_a: 0.0,
                frame_b: 2.0,
                initial_blend: -2.0,
                flags: 1,
            }],
        )
        .expect_err("one wrap still leaves an unsupported negative sample time");
        assert!(error.contains("negative or non-finite sample time"));
    }

    #[test]
    fn parses_static_material_phase() {
        let parsed = Args::parse(&strings(&[
            "--root",
            "testdata/IS",
            "--mission",
            "MISSIONS/Autodemo.00/data.tma",
            "--static-material-phase",
            "2",
        ]))
        .expect("valid static material phase");
        assert_eq!(parsed.static_material_phase, Some(2));
    }

    #[test]
    fn rejects_zero_static_preview_root_count() {
        let error = Args::parse(&strings(&[
            "--root",
            "testdata/IS",
            "--mission",
            "MISSIONS/Autodemo.00/data.tma",
            "--preview-roots",
            "0",
        ]));
        assert_eq!(
            error,
            Err("--preview-roots must be a non-zero integer".to_string())
        );
    }

    #[test]
    fn parses_legacy_camera_capture_path() {
        let valid = Args::parse(&strings(&[
            "--root",
            "testdata/IS",
            "--mission",
            "MISSIONS/Autodemo.00/data.tma",
            "--legacy-camera-capture",
            "target/camera.json",
        ]))
        .expect("valid legacy camera arguments");
        assert_eq!(
            valid.legacy_camera_capture,
            Some(PathBuf::from("target/camera.json"))
        );
    }

    #[test]
    fn legacy_camera_capture_parses_exact_d3d7_inputs() {
        let words = [
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
        ];
        let json = format!(
            "{{\"schema\":\"fparkan-legacy-camera-v1\",\"render_input_usable\":true,\"atmosphere_seconds\":56.227,\"selector0_words\":{:?},\"viewport\":[23,17,1047,785],\"near_plane\":0.5,\"far_plane\":700.0,\"field_of_view_radians\":1.3}}",
            words
        );

        let capture = parse_legacy_camera_capture(json.as_bytes()).expect("valid legacy camera");
        let camera = capture.preview_camera_frame().expect("valid preview frame");

        assert_eq!(capture.render_input_usable, Some(true));
        assert_eq!(capture.atmosphere_seconds, Some(56.227));
        assert!(camera.clip_from_world.iter().all(|value| value.is_finite()));
        assert_eq!(camera.clip_from_world[0], 0.65_f32.cos());
        assert_eq!(capture.viewport, [23, 17, 1047, 785]);
        assert_eq!(
            capture.captured_viewport().unwrap().rect,
            [23, 17, 1047, 785]
        );
        assert_eq!(capture.captured_viewport().unwrap().extent, [1024, 768]);
        assert!((horizontal_fov(&camera, 4.0 / 3.0) - 1.3).abs() < 1.0e-5);
        assert_eq!(
            camera.vulkan_camera().clip_from_world,
            camera.clip_from_world
        );
        let mut utf16le = vec![0xff, 0xfe];
        utf16le.extend(json.encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(
            parse_legacy_camera_capture(&utf16le)
                .expect("PowerShell UTF-16LE capture must be accepted")
                .preview_camera_frame()
                .expect("UTF-16 preview frame")
                .clip_from_world,
            camera.clip_from_world
        );
        assert_eq!(
            parse_legacy_camera_capture(br#"{"schema":"unknown","selector0_words":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],"viewport":[0,0,1,1],"near_plane":0.1,"far_plane":1.0,"field_of_view_radians":1.0}"#),
            Err("unsupported legacy camera capture schema".to_string())
        );
    }

    #[test]
    fn parented_camera_preview_uses_effective_world_matrix_fixture() {
        let fixture = include_bytes!("../testdata/legacy-camera-parented.json");
        let capture = parse_legacy_camera_capture(fixture).expect("valid parented camera fixture");
        let parse_value = |value: serde_json::Value| {
            let bytes = serde_json::to_vec(&value).expect("serialize camera fixture");
            parse_legacy_camera_capture(&bytes)
        };
        assert_eq!(capture.parented_camera, Some(true));
        assert_eq!(
            capture.preview_camera_frame().unwrap().position,
            [10.0, 20.0, 30.0]
        );
        assert_eq!(
            capture.preview_matrix_words().unwrap(),
            capture.effective_world_matrix_words.unwrap()
        );

        let mut missing_world: serde_json::Value =
            serde_json::from_slice(fixture).expect("valid fixture JSON");
        missing_world
            .as_object_mut()
            .unwrap()
            .remove("effective_world_matrix_words");
        let missing_world = serde_json::to_vec(&missing_world).unwrap();
        assert!(parse_legacy_camera_capture(&missing_world)
            .expect_err("incomplete world matrix pair")
            .contains("effective_world_matrix_space and effective_world_matrix_words"));

        let mut parented_without_world: serde_json::Value =
            serde_json::from_slice(fixture).expect("valid fixture JSON");
        let parented_without_world = parented_without_world.as_object_mut().unwrap();
        parented_without_world.remove("effective_world_matrix_space");
        parented_without_world.remove("effective_world_matrix_words");
        assert!(
            parse_value(serde_json::Value::Object(parented_without_world.clone()))
                .expect_err("parented camera without its world matrix")
                .contains("parented legacy camera requires an effective world-space matrix")
        );

        let mut invalid_marker: serde_json::Value =
            serde_json::from_slice(fixture).expect("valid fixture JSON");
        invalid_marker["effective_world_matrix_space"] =
            serde_json::Value::String("local".to_string());
        assert!(parse_value(invalid_marker)
            .expect_err("unknown matrix space")
            .contains("effective_world_matrix_space must be 'world'"));

        let mut legacy: serde_json::Value =
            serde_json::from_slice(fixture).expect("valid fixture JSON");
        let legacy = legacy.as_object_mut().unwrap();
        legacy.remove("parented_camera");
        legacy.remove("effective_world_matrix_space");
        legacy.remove("effective_world_matrix_words");
        let legacy = parse_value(serde_json::Value::Object(legacy.clone()))
            .expect("older local-only camera capture");
        assert_eq!(
            legacy.preview_camera_frame().unwrap().position,
            [1.0, 2.0, 3.0]
        );
    }

    #[test]
    fn captured_atmosphere_is_default_and_cli_time_takes_precedence() {
        let capture = LegacyCameraCapture {
            schema: "fparkan-legacy-camera-v1".to_string(),
            render_input_usable: None,
            selector0_words: [0; 16],
            parented_camera: None,
            effective_world_matrix_space: None,
            effective_world_matrix_words: None,
            viewport: [0, 0, 1, 1],
            near_plane: 0.1,
            far_plane: 1.0,
            field_of_view_radians: 1.0,
            atmosphere_seconds: Some(56.227),
        };

        assert_eq!(
            selected_atmosphere_seconds(None, Some(&capture)),
            Some(56.227)
        );
        assert_eq!(
            selected_atmosphere_seconds(Some(100.0), Some(&capture)),
            Some(100.0)
        );
        assert_eq!(selected_atmosphere_seconds(None, None), None);
    }

    #[test]
    fn captured_environment_holds_exact_phase_while_free_time_advances() {
        let (fixed_relative, fixed_absolute) =
            environment_sample_time(0.227, 56.0, 0.25, Some(56.227));
        assert_eq!(fixed_relative, 0.227);
        assert_eq!(fixed_absolute, 56.227);

        let (free_relative, free_absolute) = environment_sample_time(0.227, 56.0, 0.25, None);
        assert_eq!(free_relative, 0.477);
        assert_eq!(free_absolute, 56.477);
    }

    #[test]
    fn legacy_camera_capture_rejects_invalid_phase_and_unusable_input() {
        let base = r#"{"schema":"fparkan-legacy-camera-v1","selector0_words":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],"viewport":[0,0,1,1],"near_plane":0.1,"far_plane":1.0,"field_of_view_radians":1.0}"#;
        let invalid_phase = base.replace(
            "\"near_plane\"",
            "\"atmosphere_seconds\":-1.0,\"near_plane\"",
        );
        assert!(parse_legacy_camera_capture(invalid_phase.as_bytes())
            .expect_err("negative captured phase")
            .contains("atmosphere_seconds must be finite and non-negative"));

        let unusable = base.replace(
            "\"near_plane\"",
            "\"render_input_usable\":false,\"near_plane\"",
        );
        assert!(parse_legacy_camera_capture(unusable.as_bytes())
            .expect_err("explicitly unusable camera capture")
            .contains("render_input_usable=false"));
    }

    #[test]
    fn captured_camera_frame_keeps_native_roll_basis() {
        let roll = 0.37_f32;
        let (sin, cos) = roll.sin_cos();
        let transform = RawCameraTransform {
            words: [
                1.0_f32.to_bits(),
                0.0_f32.to_bits(),
                0.0_f32.to_bits(),
                0.0_f32.to_bits(),
                0.0_f32.to_bits(),
                cos.to_bits(),
                (-sin).to_bits(),
                0.0_f32.to_bits(),
                0.0_f32.to_bits(),
                sin.to_bits(),
                cos.to_bits(),
                0.0_f32.to_bits(),
                0.0_f32.to_bits(),
                0.0_f32.to_bits(),
                0.0_f32.to_bits(),
                1.0_f32.to_bits(),
            ],
        };
        let projection = LegacyD3d7Projection {
            viewport: [0, 0, 800, 600],
            near_plane: 0.1,
            far_plane: 500.0,
            field_of_view_radians: 1.0,
        };
        let view = transform
            .try_direct3d7_view_row_major()
            .expect("valid native camera basis");
        let frame = PreviewCameraFrame::from_legacy_d3d7(transform, projection)
            .expect("valid captured frame");

        assert_eq!(frame.forward, [view[2], view[6], view[10]]);
        assert_eq!(frame.right, [view[0], view[4], view[8]]);
        assert_eq!(frame.up, [view[1], view[5], view[9]]);
        assert!(frame.right[2].abs() > 0.3, "roll must tilt the right axis");
        assert!(frame.up[1].abs() > 0.3, "roll must tilt the up axis");
        assert_eq!(
            frame.clip_from_world,
            VulkanStaticCamera::from_legacy_d3d7(transform, projection)
                .expect("legacy clip matrix")
                .clip_from_world
        );
    }

    #[test]
    fn captured_camera_refuses_a_different_drawable_extent() {
        let captured = CapturedViewport {
            rect: [23, 17, 1047, 785],
            extent: [1024, 768],
        };
        assert!(validate_captured_drawable_extent(captured, [1024, 768]).is_ok());
        let error = validate_captured_drawable_extent(captured, [1280, 720])
            .expect_err("a mismatched surface must not stretch the captured view");
        assert!(error.contains("[23, 17, 1047, 785]"));
        assert!(error.contains("1024x768"));
        assert!(error.contains("1280x720"));
    }

    #[test]
    fn static_preview_component_merge_offsets_indices_and_remaps_local_selectors(
    ) -> Result<(), String> {
        let mut merged = VulkanStaticMesh {
            vertices: Vec::new(),
            indices: Vec::new(),
            draw_ranges: Vec::new(),
        };
        append_static_preview_component(
            &mut merged,
            VulkanStaticMesh::smoke_triangle(),
            &[(0, 4)],
            None,
        )?;
        append_static_preview_component(
            &mut merged,
            VulkanStaticMesh::smoke_triangle(),
            &[(0, 9)],
            None,
        )?;

        assert_eq!(merged.vertices.len(), 6);
        assert_eq!(merged.indices, vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(merged.draw_ranges.len(), 2);
        assert_eq!(merged.draw_ranges[0].first_index, 0);
        assert_eq!(merged.draw_ranges[0].material_index, 4);
        assert_eq!(merged.draw_ranges[1].first_index, 3);
        assert_eq!(merged.draw_ranges[1].material_index, 9);
        Ok(())
    }

    #[test]
    fn static_preview_component_merge_keeps_indices_above_u16() -> Result<(), String> {
        let vertex = fparkan_render_vulkan::VulkanStaticVertex {
            position: [0.0, 0.0, 0.0],
            color: [1.0, 1.0, 1.0],
            normal: [0.0, 0.0, 1.0],
            uv: [0.0, 0.0],
            detail_uv: [0.0, 0.0],
            overlay_alpha: 0.0,
        };
        let mut merged = VulkanStaticMesh {
            vertices: vec![vertex; usize::from(u16::MAX) + 1],
            indices: Vec::new(),
            draw_ranges: Vec::new(),
        };

        append_static_preview_component(
            &mut merged,
            VulkanStaticMesh::smoke_triangle(),
            &[(0, 0)],
            None,
        )?;

        assert_eq!(merged.indices, vec![65_536, 65_537, 65_538]);
        Ok(())
    }

    #[test]
    fn precipitation_upload_reconstructs_native_ndc_depths_and_uvs() {
        let camera = FreeFlightCamera {
            position: [10.0, 20.0, 30.0],
            yaw: 0.0,
            pitch: 0.0,
            horizontal_fov: std::f32::consts::FRAC_PI_2,
            near_plane: 0.1,
            far_plane: 100.0,
            move_speed: 1.0,
            ground_clearance: 0.0,
        }
        .preview_frame(1.0);
        let screen = ScreenBillboard {
            world_head: [0.0; 3],
            world_tail: [0.0; 3],
            head: [0.0, 0.0],
            tail: [0.0, 0.0],
            corners: [[-0.25, -0.5], [0.25, -0.5], [-0.25, 0.5], [0.25, 0.5]],
            uv: [[0.1, 0.2], [0.3, 0.4], [0.5, 0.6], [0.7, 0.8]],
            half_size: 12.0,
            head_depth: 8.0,
            tail_depth: 4.0,
        };
        let vertices = precipitation_quad_vertices(
            PrecipitationKind::Rain,
            &screen,
            [1.0, 0.5, 0.25, 0.75],
            &camera,
            [100.0, 100.0],
        );
        assert_eq!(vertices[0].uv, screen.uv[0]);
        assert_eq!(vertices[3].uv, screen.uv[3]);
        assert!(
            (dot3(
                sub3(vertices[0].position, camera.position),
                camera.forward()
            ) - 4.0)
                .abs()
                < 1.0e-5
        );
        assert!(
            (dot3(
                sub3(vertices[3].position, camera.position),
                camera.forward()
            ) - 8.0)
                .abs()
                < 1.0e-5
        );
        assert_eq!(vertices[0].overlay_alpha, 0.75);
    }

    #[test]
    fn sprite_upload_remaps_native_left_top_uv_order_to_quad_positions() {
        let camera = FreeFlightCamera {
            position: [0.0; 3],
            yaw: 0.0,
            pitch: 0.0,
            horizontal_fov: 1.0,
            near_plane: 0.1,
            far_plane: 100.0,
            move_speed: 1.0,
            ground_clearance: 0.0,
        }
        .preview_frame(1.0);
        let uv = [[1.0, 2.0], [3.0, 4.0], [5.0, 6.0], [7.0, 8.0]];
        let vertices = sprite_quad_vertices_pixels(
            &camera,
            camera.forward(),
            10.0,
            1.0,
            1.0,
            [100.0, 100.0],
            [1.0; 3],
            1.0,
            uv,
        );
        assert_eq!(
            vertices.map(|vertex| vertex.uv),
            [uv[1], uv[2], uv[0], uv[3]]
        );
    }

    #[test]
    fn native_tl_sprite_pixels_use_current_viewport_and_preserve_depth_999() {
        let uv = [[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]];
        let small = screen_tl_quad_vertices_pixels(
            [160.0, 120.0],
            40.0,
            30.0,
            [640.0, 480.0],
            [1.0; 3],
            1.0,
            uv,
        );
        let resized = screen_tl_quad_vertices_pixels(
            [320.0, 240.0],
            80.0,
            60.0,
            [1280.0, 960.0],
            [1.0; 3],
            1.0,
            uv,
        );
        let wide = screen_tl_quad_vertices_pixels(
            [640.0, 360.0],
            100.0,
            45.0,
            [1280.0, 720.0],
            [1.0; 3],
            1.0,
            uv,
        );

        for (small, resized) in small.iter().zip(resized) {
            for axis in 0..2 {
                assert!((small.position[axis] - resized.position[axis]).abs() < 1.0e-6);
            }
            assert_eq!(small.position[2], 0.999);
            assert_eq!(resized.position[2], 0.999);
        }
        assert_eq!(small[0].uv, [0.0, 1.0]);
        assert_eq!(small[2].uv, [0.0, 0.0]);
        for (actual, expected) in [
            (wide[0].position, [-0.15625, 0.125, 0.999]),
            (wide[2].position, [-0.15625, -0.125, 0.999]),
        ] {
            for axis in 0..3 {
                assert!((actual[axis] - expected[axis]).abs() < 1.0e-6);
            }
        }
    }

    #[test]
    fn native_tl_depth_999_is_nearer_than_opaque_geometry_at_320_units() {
        let camera = FreeFlightCamera {
            position: [0.0; 3],
            yaw: 0.0,
            pitch: 0.0,
            horizontal_fov: 1.3,
            near_plane: 0.5,
            far_plane: 700.0,
            move_speed: 1.0,
            ground_clearance: 0.0,
        };
        let frame = camera.preview_frame(16.0 / 9.0);
        let opaque_clip = row_vector_transform([320.0, 0.0, 0.0, 1.0], frame.clip_from_world);
        let opaque_depth = opaque_clip[2] / opaque_clip[3];

        assert!((opaque_depth - 0.999151_18).abs() < 1.0e-6);
        assert!(0.999 < opaque_depth);
    }

    fn facing_mesh(
        batch_flags: u16,
        positions: &[[f32; 3]],
    ) -> (VulkanStaticMesh, VulkanStaticDrawRange) {
        let range = VulkanStaticDrawRange {
            first_index: 0,
            index_count: u32::try_from(positions.len()).expect("fixture index count fits u32"),
            material_index: 0,
            lightmap_index: 0,
            batch_flags,
            pipeline_state: LegacyPipelineState::default(),
            terrain_base_only: false,
            alpha_test_reference: VulkanStaticDrawRange::NATIVE_DEFAULT_ALPHA_TEST_REFERENCE,
        };
        let mesh = VulkanStaticMesh {
            vertices: positions
                .iter()
                .copied()
                .map(|position| VulkanStaticVertex {
                    position,
                    color: [1.0; 3],
                    normal: [0.0, 0.0, 1.0],
                    uv: [0.0; 2],
                    detail_uv: [0.0; 2],
                    overlay_alpha: 0.0,
                })
                .collect(),
            indices: (0..positions.len())
                .map(|index| u32::try_from(index).expect("fixture index fits u32"))
                .collect(),
            draw_ranges: vec![range],
        };
        (mesh, range)
    }

    fn native_front_triangle() -> [[f32; 3]; 3] {
        [[-0.2, -0.2, -1.0], [0.2, -0.2, -1.0], [0.0, 0.2, -1.0]]
    }

    fn camera_face_triangle(camera: PreviewCameraFrame, mirror: bool) -> [[f32; 3]; 3] {
        let center = add3(camera.position, scale3(camera.forward, 10.0));
        let mirror = if mirror { -1.0 } else { 1.0 };
        [
            add3(
                add3(center, scale3(camera.right, -2.0 * mirror)),
                scale3(camera.up, -2.0),
            ),
            add3(
                add3(center, scale3(camera.right, 2.0 * mirror)),
                scale3(camera.up, -2.0),
            ),
            add3(center, scale3(camera.up, 2.0)),
        ]
    }

    fn projected_vulkan_area(camera: PreviewCameraFrame, triangle: [[f32; 3]; 3]) -> f32 {
        let screen = triangle.map(|point| {
            let clip =
                row_vector_transform([point[0], point[1], point[2], 1.0], camera.clip_from_world);
            assert!(clip[3] > 0.0 && clip.iter().all(|value| value.is_finite()));
            // This is the positive-height viewport transform used by the Vulkan renderer.
            [
                (clip[0] / clip[3] + 1.0) * 0.5,
                (clip[1] / clip[3] + 1.0) * 0.5,
            ]
        });
        let shoelace = (0..3)
            .map(|index| {
                let next = (index + 1) % 3;
                screen[index][0] * screen[next][1] - screen[next][0] * screen[index][1]
            })
            .sum::<f32>()
            * 0.5;
        // Vulkan's framebuffer signed-area definition is the negative shoelace area.
        -shoelace
    }

    #[test]
    fn native_msh_cull_mode_matches_legacy_and_free_flight_camera_projection() {
        let identity = RawCameraTransform {
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
            viewport: [0, 0, 1280, 1024],
            near_plane: 0.5,
            far_plane: 700.0,
            field_of_view_radians: 1.3,
        };
        let legacy = PreviewCameraFrame::from_legacy_d3d7(identity, projection)
            .expect("valid legacy camera projection");
        let free_flight = FreeFlightCamera {
            position: legacy.position,
            yaw: 0.0,
            pitch: 0.0,
            horizontal_fov: 1.3,
            near_plane: 0.5,
            far_plane: 700.0,
            move_speed: 1.0,
            ground_clearance: 0.0,
        }
        .preview_frame(1280.0 / 1024.0);

        for camera in [legacy, free_flight] {
            let front = camera_face_triangle(camera, false);
            let back = [front[0], front[2], front[1]];
            let front_d = dot3(
                sub3(camera.position, front[0]),
                cross3(sub3(front[1], front[0]), sub3(front[2], front[0])),
            );
            let back_d = dot3(
                sub3(camera.position, back[0]),
                cross3(sub3(back[1], back[0]), sub3(back[2], back[0])),
            );
            assert!(front_d > 0.0);
            assert!(back_d < 0.0);

            let front_vk_area = projected_vulkan_area(camera, front);
            let back_vk_area = projected_vulkan_area(camera, back);
            // Vulkan CLOCKWISE front faces have negative framebuffer area.
            // The native-visible d>0 face is therefore a Vulkan back face.
            assert!(front_vk_area > 0.0, "front area: {front_vk_area}");
            assert!(back_vk_area < 0.0, "back area: {back_vk_area}");
            assert_eq!(native_msh_cull_mode(0, 1), LegacyCullMode::FrontFace);

            let mirrored_front = camera_face_triangle(camera, true);
            let mirrored_d = dot3(
                sub3(camera.position, mirrored_front[0]),
                cross3(
                    sub3(mirrored_front[1], mirrored_front[0]),
                    sub3(mirrored_front[2], mirrored_front[0]),
                ),
            );
            assert!(mirrored_d < 0.0);
            assert!(projected_vulkan_area(camera, mirrored_front) < 0.0);
            assert_eq!(native_msh_cull_mode(0, -1), LegacyCullMode::BackFace);
        }
    }

    #[test]
    fn native_msh_handedness_uses_full_placement_transform_and_handles_singular_scale() {
        let rotated = LegacyIron3dEulerTransform {
            translation: [8.0, -4.0, 3.0],
            orientation_radians: [0.4, -0.7, 1.2],
        };
        assert_eq!(native_msh_handedness(rotated, [2.0, 3.0, 4.0]), 1);
        assert_eq!(native_msh_handedness(rotated, [-2.0, 3.0, 4.0]), -1);
        assert_eq!(native_msh_handedness(rotated, [0.0, 3.0, 4.0]), 0);
        assert_eq!(native_msh_cull_mode(0, 0), LegacyCullMode::Disabled);
    }

    #[test]
    fn native_msh_4000_samples_only_first_triangle_and_raw_two_sided_bypasses_it() {
        let front = native_front_triangle();
        let back = front.map(|point| [-point[0], point[1], point[2]]);
        let front_then_back = [front[0], front[1], front[2], back[0], back[1], back[2]];
        let (mesh, range) = facing_mesh(0x4000, &front_then_back);
        assert!(native_msh_first_triangle_visible(
            &mesh, &range, [0.0; 3], 1
        ));

        let back_then_front = [back[0], back[1], back[2], front[0], front[1], front[2]];
        let (mesh, range) = facing_mesh(0x4000, &back_then_front);
        assert!(!native_msh_first_triangle_visible(
            &mesh, &range, [0.0; 3], 1
        ));

        let (mesh, range) = facing_mesh(0x4002, &back_then_front);
        assert!(native_msh_first_triangle_visible(
            &mesh, &range, [0.0; 3], 1
        ));
        assert_eq!(
            native_msh_cull_mode(range.batch_flags, 1),
            LegacyCullMode::Disabled
        );
    }

    #[test]
    fn world_draw_range_count_restores_after_alpha_and_msh_facing_changes() {
        let triangle = native_front_triangle();
        let (mesh, range) = facing_mesh(0x4000, &triangle);
        let original_count = range.index_count;

        assert_eq!(
            world_draw_range_index_count(&mesh, &range, original_count, 0.0, [0.0; 3], Some(1)),
            0
        );
        assert_eq!(
            world_draw_range_index_count(
                &mesh,
                &range,
                original_count,
                1.0,
                [0.0, 0.0, -2.0],
                Some(1),
            ),
            0
        );
        assert_eq!(
            world_draw_range_index_count(&mesh, &range, original_count, 1.0, [0.0; 3], Some(1)),
            original_count
        );
        assert_eq!(
            world_draw_range_index_count(
                &mesh,
                &range,
                original_count,
                1.0,
                [0.0, 0.0, -2.0],
                None,
            ),
            original_count
        );
    }
}
