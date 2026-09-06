#![forbid(unsafe_code)]

//! `FParkan` render-planning composition root.

use fparkan_assets::{
    load_standalone_wear_material_texture_mip0_rgba8_from_root, PreparedTextureUsage,
    PreparedVisual,
};
use fparkan_path::ResourceName;
use fparkan_platform_winit::{window_native_handles, WinitWindow, WinitWindowPlan};
use fparkan_render::{LegacyD3d7Projection, LegacyIron3dEulerTransform, RawCameraTransform};
use fparkan_render_vulkan::{
    project_land_msh_to_static_mesh_in_legacy_world_space,
    project_land_msh_to_static_mesh_in_xy_frame,
    project_msh_to_static_mesh_in_world_space_with_node_fallback_poses,
    project_msh_to_static_mesh_in_world_space_with_node_sampled_poses,
    project_msh_to_static_mesh_in_xy_frame_with_node_fallback_poses,
    project_msh_to_static_mesh_in_xy_frame_with_node_sampled_poses, VulkanSmokeFrameOutcome,
    VulkanSmokeRenderer, VulkanSmokeRendererCreateInfo, VulkanStaticCamera, VulkanStaticMaterial,
    VulkanStaticMesh, VulkanStaticTexture, VulkanStaticXyFrame,
};
use fparkan_runtime::{
    create, load_mission_static_preview, load_mission_static_preview_roots, loaded_mission_assets,
    loaded_mission_object_drafts, loaded_terrain, EngineServices, MissionAssets,
    MissionObjectDraft, MissionRequest,
};
use fparkan_terrain::TerrainMaterialLayers;
use fparkan_terrain::TerrainWorld;
use fparkan_vfs::DirectoryVfs;
use serde::Deserialize;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize as WinitPhysicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

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
    let loaded = load_requested_mission(&mut engine, &args)?;

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
    run_static_vulkan_mode(
        preview,
        args.frames,
        &args.mission,
        loaded.object_count,
        args.readback_out.as_deref(),
        args.validation,
    )
}

fn load_requested_mission(
    engine: &mut fparkan_runtime::Engine,
    args: &Args,
) -> Result<fparkan_runtime::LoadedMission, String> {
    let request = MissionRequest {
        key: args.mission.clone(),
    };
    if args.preview_roots.get() == 1 {
        load_mission_static_preview(engine, request)
    } else {
        load_mission_static_preview_roots(engine, request, args.preview_roots)
    }
    .map_err(|err| err.to_string())
}

/// Static geometry and descriptors belonging to the selected preview roots.
struct StaticPreviewScene {
    mesh: VulkanStaticMesh,
    materials: Vec<VulkanStaticMaterial>,
    camera: VulkanStaticCamera,
    camera_mode: &'static str,
    mesh_components: usize,
    terrain_components: usize,
}

/// Projects the mission terrain plus every MSH component of the explicitly
/// bounded static-preview roots into one diagnostic XY frame.
///
/// This intentionally takes only the first MAT0 texture request for each MSH
/// batch selector. Terrain maps its proven low-byte packed tag channel through
/// the map-local `Land2.wea` table. The high-byte `Land1.wea` channel is
/// retained on draw ranges but awaits a recovered blend equation; orientation,
/// later material phases, animation, lightmaps and gameplay visibility remain
/// outside this bridge.

fn static_preview_mesh_and_materials(
    assets: &MissionAssets,
    terrain: &TerrainWorld,
    roots: &[MissionObjectDraft],
    legacy_camera: Option<VulkanStaticCamera>,
    static_animation_frame: Option<u16>,
    static_material_phase: Option<u16>,
    root: &std::path::Path,
    land_msh_path: &str,
) -> Result<StaticPreviewScene, String> {
    let terrain_mesh = terrain
        .source_mesh()
        .ok_or_else(|| "runtime terrain does not retain its validated source mesh".to_string())?;
    let xy_frame = legacy_camera
        .is_none()
        .then(|| static_preview_xy_frame(assets, terrain, roots))
        .transpose()?;
    let mut mesh = VulkanStaticMesh {
        vertices: Vec::new(),
        indices: Vec::new(),
        draw_ranges: Vec::new(),
    };
    let mut materials = Vec::new();
    let terrain_component = if legacy_camera.is_some() {
        project_land_msh_to_static_mesh_in_legacy_world_space(terrain_mesh)
    } else {
        let frame = xy_frame.ok_or_else(|| "missing diagnostic XY frame".to_string())?;
        project_land_msh_to_static_mesh_in_xy_frame(terrain_mesh, frame)
    }
    .map_err(|err| format!("project mission terrain for Vulkan: {err}"))?;
    let terrain_materials = static_preview_terrain_base_materials(
        root,
        land_msh_path,
        &terrain_component,
        &mut materials,
    )?;
    append_static_preview_component(&mut mesh, terrain_component, &terrain_materials)?;
    let mut mesh_components = 0;
    for (object_index, root) in roots.iter().enumerate() {
        for visual_id in assets.visuals_for_object(object_index) {
            let visual = assets.visual_by_id(*visual_id).ok_or_else(|| {
                format!(
                    "static preview root {object_index} references unknown visual {visual_id:?}"
                )
            })?;
            let Some(model_id) = visual.model_id else {
                continue;
            };
            let model = assets.model_by_id(model_id).ok_or_else(|| {
                format!("static preview visual {visual_id:?} references unknown model {model_id:?}")
            })?;
            let transform = LegacyIron3dEulerTransform {
                translation: root.position,
                orientation_radians: root.orientation_raw,
            };
            let component = if legacy_camera.is_some() {
                if let Some(frame) = static_animation_frame {
                    project_msh_to_static_mesh_in_world_space_with_node_sampled_poses(
                        &model.validated,
                        transform,
                        root.scale,
                        frame,
                    )
                } else {
                    project_msh_to_static_mesh_in_world_space_with_node_fallback_poses(
                        &model.validated,
                        transform,
                        root.scale,
                    )
                }
            } else {
                let frame = xy_frame.ok_or_else(|| "missing diagnostic XY frame".to_string())?;
                if let Some(animation_frame) = static_animation_frame {
                    project_msh_to_static_mesh_in_xy_frame_with_node_sampled_poses(
                        &model.validated,
                        frame,
                        transform,
                        root.scale,
                        animation_frame,
                    )
                } else {
                    project_msh_to_static_mesh_in_xy_frame_with_node_fallback_poses(
                        &model.validated,
                        frame,
                        transform,
                        root.scale,
                    )
                }
            }
            .map_err(|err| format!("project mission MSH for Vulkan: {err}"))?;
            let selector_remap = static_preview_component_materials(
                assets,
                visual,
                &component,
                static_material_phase,
                &mut materials,
            )?;
            append_static_preview_component(&mut mesh, component, &selector_remap)?;
            mesh_components += 1;
        }
    }
    if mesh_components == 0 {
        return Err("selected static preview roots have no mesh-backed visual".to_string());
    }
    let camera = legacy_camera.unwrap_or_default();
    Ok(StaticPreviewScene {
        mesh,
        materials,
        camera,
        camera_mode: if legacy_camera.is_some() {
            "legacy-d3d7-capture"
        } else {
            "diagnostic-xy"
        },
        mesh_components,
        terrain_components: 1,
    })
}

fn static_preview_xy_frame(
    assets: &MissionAssets,
    terrain: &TerrainWorld,
    roots: &[MissionObjectDraft],
) -> Result<VulkanStaticXyFrame, String> {
    let mut min_x = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    let terrain_positions = terrain
        .source_positions()
        .ok_or_else(|| "runtime terrain does not retain source positions".to_string())?;
    for position in terrain_positions {
        extend_static_preview_xy_bounds(*position, &mut min_x, &mut max_x, &mut min_y, &mut max_y)?;
    }
    for (object_index, root) in roots.iter().enumerate() {
        if !root
            .position
            .iter()
            .chain(root.scale.iter())
            .all(|value| value.is_finite())
        {
            return Err(format!(
                "static preview root {object_index} has a non-finite position or scale"
            ));
        }
        for visual_id in assets.visuals_for_object(object_index) {
            let visual = assets.visual_by_id(*visual_id).ok_or_else(|| {
                format!(
                    "static preview root {object_index} references unknown visual {visual_id:?}"
                )
            })?;
            let Some(model_id) = visual.model_id else {
                continue;
            };
            let model = assets.model_by_id(model_id).ok_or_else(|| {
                format!("static preview visual {visual_id:?} references unknown model {model_id:?}")
            })?;
            for position in &model.validated.positions {
                let position = LegacyIron3dEulerTransform {
                    translation: root.position,
                    orientation_radians: root.orientation_raw,
                }
                .try_transform_scaled_point(*position, root.scale)
                .ok_or_else(|| {
                    "static preview contains a non-finite transformed XY position".to_string()
                })?;
                extend_static_preview_xy_bounds(
                    position, &mut min_x, &mut max_x, &mut min_y, &mut max_y,
                )?;
            }
        }
    }
    VulkanStaticXyFrame::from_bounds(min_x, max_x, min_y, max_y)
        .map_err(|err| format!("build static preview XY frame: {err}"))
}

fn extend_static_preview_xy_bounds(
    position: [f32; 3],
    min_x: &mut f32,
    max_x: &mut f32,
    min_y: &mut f32,
    max_y: &mut f32,
) -> Result<(), String> {
    if !position.iter().all(|value| value.is_finite()) {
        return Err("static preview contains a non-finite XY position".to_string());
    }
    *min_x = min_x.min(position[0]);
    *max_x = max_x.max(position[0]);
    *min_y = min_y.min(position[1]);
    *max_y = max_y.max(position[1]);
    Ok(())
}

fn static_preview_terrain_base_materials(
    root: &std::path::Path,
    land_msh_path: &str,
    mesh: &VulkanStaticMesh,
    materials: &mut Vec<VulkanStaticMaterial>,
) -> Result<Vec<(u16, u16)>, String> {
    let land_path = root.join(land_msh_path);
    let wear_path = land_path
        .parent()
        .ok_or_else(|| format!("terrain mesh has no parent path: {}", land_path.display()))?
        .join("Land2.wea");
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
            let selection = TerrainMaterialLayers::from_packed_tag(packed_tag).land2_selection();
            let image = load_standalone_wear_material_texture_mip0_rgba8_from_root(
                root,
                &wear_path,
                selection.material_index,
            )
            .map_err(|err| {
                format!(
                    "resolve terrain base material tag 0x{packed_tag:04x} through {}: {err}",
                    wear_path.display()
                )
            })?;
            let preview_selector = u16::try_from(materials.len()).map_err(|_| {
                "static preview exceeds the available 16-bit material selector space".to_string()
            })?;
            materials.push(VulkanStaticMaterial {
                material_index: preview_selector,
                texture: VulkanStaticTexture {
                    width: image.width,
                    height: image.height,
                    rgba8: image.rgba8,
                },
            });
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
            let texture_name = static_material_phase
                .and_then(|phase_index| {
                    let phase = material.mat0.phases.get(usize::from(phase_index))?;
                    let length = phase.texture_raw.iter().position(|byte| *byte == 0).unwrap_or(phase.texture_raw.len());
                    (length != 0).then(|| ResourceName(phase.texture_raw[..length].to_vec()))
                })
                .or_else(|| material.texture_requests.first().cloned())
                .ok_or_else(|| {
                    format!("static preview material {source_selector} has no MAT0 diffuse texture")
                })?;
            let texture = assets
                .textures
                .iter()
                .find(|texture| {
                    texture.usage == PreparedTextureUsage::Diffuse && texture.source.name == texture_name
                })
                .ok_or_else(|| {
                    format!(
                        "static preview diffuse texture {texture_name:?} for material {source_selector} is unavailable"
                    )
                })?;
            let image = texture.decode_mip_rgba8(0).map_err(|err| {
                format!(
                    "decode static preview diffuse texture {texture_name:?} for material {source_selector}: {err}"
                )
            })?;
            let preview_selector = u16::try_from(materials.len()).map_err(|_| {
                "static preview exceeds the available 16-bit material selector space".to_string()
            })?;
            materials.push(VulkanStaticMaterial {
                material_index: preview_selector,
                texture: VulkanStaticTexture {
                    width: image.width,
                    height: image.height,
                    rgba8: image.rgba8,
                },
            });
            Ok((source_selector, preview_selector))
        })
        .collect()
}

fn append_static_preview_component(
    target: &mut VulkanStaticMesh,
    component: VulkanStaticMesh,
    selector_remap: &[(u16, u16)],
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
    for range in component.draw_ranges {
        let material_index = selector_remap
            .iter()
            .find_map(|(source, preview)| (*source == range.material_index).then_some(*preview))
            .ok_or_else(|| {
                format!(
                    "static preview component has no material remap for selector {}",
                    range.material_index
                )
            })?;
        target
            .draw_ranges
            .push(fparkan_render_vulkan::VulkanStaticDrawRange {
                first_index: first_index_base
                    .checked_add(range.first_index)
                    .ok_or_else(|| "static preview index range exceeds u32".to_string())?,
                material_index,
                ..range
            });
    }
    Ok(())
}

fn run_static_vulkan_mode(
    preview: StaticPreviewScene,
    target_frames: u64,
    mission: &str,
    object_count: usize,
    readback_out: Option<&std::path::Path>,
    enable_validation: bool,
) -> Result<String, String> {
    let event_loop = EventLoop::new().map_err(|err| format!("winit event loop: {err}"))?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = StaticVulkanApp::new(
        preview,
        target_frames,
        mission,
        object_count,
        readback_out.map(std::path::Path::to_path_buf),
        enable_validation,
    );
    if let Err(err) = event_loop.run_app(&mut app) {
        app.error = Some(format!("winit event loop: {err}"));
    }
    app.finish()
}

struct StaticVulkanApp {
    mesh: VulkanStaticMesh,
    materials: Vec<VulkanStaticMaterial>,
    camera: VulkanStaticCamera,
    camera_mode: &'static str,
    mesh_components: usize,
    terrain_components: usize,
    target_frames: u64,
    mission: String,
    object_count: usize,
    readback_out: Option<PathBuf>,
    enable_validation: bool,
    window_id: Option<WindowId>,
    window: Option<Window>,
    renderer: Option<VulkanSmokeRenderer>,
    frames_presented: u64,
    output: Option<String>,
    error: Option<String>,
}

impl StaticVulkanApp {
    fn new(
        preview: StaticPreviewScene,
        target_frames: u64,
        mission: &str,
        object_count: usize,
        readback_out: Option<PathBuf>,
        enable_validation: bool,
    ) -> Self {
        Self {
            mesh: preview.mesh,
            materials: preview.materials,
            camera: preview.camera,
            camera_mode: preview.camera_mode,
            mesh_components: preview.mesh_components,
            terrain_components: preview.terrain_components,
            target_frames,
            mission: mission.to_string(),
            object_count,
            readback_out,
            enable_validation,
            window_id: None,
            window: None,
            renderer: None,
            frames_presented: 0,
            output: None,
            error: None,
        }
    }

    fn schedule_next_redraw(&self) {
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    fn complete(&mut self, event_loop: &ActiveEventLoop) {
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
                "native Vulkan validation must stay clean (warnings={}, errors={})",
                report.validation.warning_count, report.validation.error_count
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
        self.output = Some(format!(
            "rendered mission {}: {} frames, {} objects, {} mesh components, {} terrain components, camera={}, materials={}, validation={}, readback={}",
            self.mission,
            self.frames_presented,
            self.object_count,
            self.mesh_components,
            self.terrain_components,
            self.camera_mode,
            self.materials.len(),
            validation_status,
            readback_path.as_deref().unwrap_or("none"),
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
        let attributes = Window::default_attributes()
            .with_title("FParkan static mission Vulkan")
            .with_inner_size(WinitPhysicalSize::new(plan.width, plan.height));
        let window = match event_loop.create_window(attributes) {
            Ok(window) => window,
            Err(err) => {
                self.error = Some(format!("winit window: {err}"));
                event_loop.exit();
                return;
            }
        };
        let Some(native_handles) = window_native_handles(&window) else {
            self.error = Some("winit window does not expose native handles".to_string());
            event_loop.exit();
            return;
        };
        let size = window.inner_size();
        let renderer = match VulkanSmokeRenderer::new(&VulkanSmokeRendererCreateInfo {
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
            Err(err) => {
                self.error = Some(err.to_string());
                event_loop.exit();
                return;
            }
        };
        self.window_id = Some(window.id());
        self.renderer = Some(renderer);
        self.window = Some(window);
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
            WindowEvent::Resized(size) => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.request_resize((size.width, size.height));
                }
            }
            WindowEvent::RedrawRequested => {
                let Some(renderer) = self.renderer.as_mut() else {
                    self.error = Some("native Vulkan renderer was not initialized".to_string());
                    event_loop.exit();
                    return;
                };
                match renderer.draw_frame() {
                    Ok(VulkanSmokeFrameOutcome::Presented) => {
                        self.frames_presented = self.frames_presented.saturating_add(1);
                    }
                    Ok(
                        VulkanSmokeFrameOutcome::Recreated | VulkanSmokeFrameOutcome::ZeroExtent,
                    ) => {}
                    Err(err) => {
                        self.error = Some(err.to_string());
                        event_loop.exit();
                        return;
                    }
                }
                if self.frames_presented >= self.target_frames {
                    self.complete(event_loop);
                } else {
                    self.schedule_next_redraw();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if self.output.is_none() && self.error.is_none() {
            self.schedule_next_redraw();
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Args {
    root: PathBuf,
    mission: String,
    frames: u64,
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
        let mut frames = 1;
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
        if frames == 0 {
            return Err("--frames must be greater than zero".to_string());
        }
        Ok(Self {
            root,
            mission,
            frames,
            validation,
            readback_out,
            preview_roots,
            legacy_camera_capture,
            static_animation_frame,
            static_material_phase,
        })
    }
}

#[derive(Deserialize)]
struct LegacyCameraCapture {
    schema: String,
    selector0_words: [u32; 16],
    viewport: [i32; 4],
    near_plane: f32,
    far_plane: f32,
    field_of_view_radians: f32,
}

fn load_legacy_camera_capture(path: &std::path::Path) -> Result<VulkanStaticCamera, String> {
    let bytes = std::fs::read(path).map_err(|err| format!("{}: {err}", path.display()))?;
    parse_legacy_camera_capture(&bytes).map_err(|err| format!("{}: {err}", path.display()))
}

fn parse_legacy_camera_capture(bytes: &[u8]) -> Result<VulkanStaticCamera, String> {
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
    VulkanStaticCamera::from_legacy_d3d7(
        RawCameraTransform {
            words: capture.selector0_words,
        },
        LegacyD3d7Projection {
            viewport: capture.viewport,
            near_plane: capture.near_plane,
            far_plane: capture.far_plane,
            field_of_view_radians: capture.field_of_view_radians,
        },
    )
    .ok_or_else(|| "legacy camera capture contains an invalid D3D7 camera".to_string())
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
    "usage: fparkan-game --root <path> --mission <path> [--frames <n>] [--validation] [--preview-roots <non-zero n>] [--legacy-camera-capture <path>] [--static-animation-frame <u16>] [--static-material-phase <u16>] [--readback-out <path>]".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
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
            "{{\"schema\":\"fparkan-legacy-camera-v1\",\"selector0_words\":{:?},\"viewport\":[0,0,1024,768],\"near_plane\":0.5,\"far_plane\":700.0,\"field_of_view_radians\":1.3}}",
            words
        );

        let camera = parse_legacy_camera_capture(json.as_bytes()).expect("valid legacy camera");

        assert!(camera.is_finite());
        assert_eq!(camera.clip_from_world[0], 0.65_f32.cos());
        let mut utf16le = vec![0xff, 0xfe];
        utf16le.extend(json.encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(
            parse_legacy_camera_capture(&utf16le)
                .expect("PowerShell UTF-16LE capture must be accepted")
                .clip_from_world,
            camera.clip_from_world
        );
        assert_eq!(
            parse_legacy_camera_capture(br#"{"schema":"unknown","selector0_words":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],"viewport":[0,0,1,1],"near_plane":0.1,"far_plane":1.0,"field_of_view_radians":1.0}"#),
            Err("unsupported legacy camera capture schema".to_string())
        );
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
        )?;
        append_static_preview_component(
            &mut merged,
            VulkanStaticMesh::smoke_triangle(),
            &[(0, 9)],
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
            uv: [0.0, 0.0],
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
        )?;

        assert_eq!(merged.indices, vec![65_536, 65_537, 65_538]);
        Ok(())
    }
}
