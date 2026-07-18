#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        clippy::cast_precision_loss,
        clippy::expect_used,
        clippy::float_cmp,
        clippy::identity_op,
        clippy::too_many_lines,
        clippy::uninlined_format_args,
        clippy::map_unwrap_or,
        clippy::needless_raw_string_hashes,
        clippy::semicolon_if_nothing_returned,
        clippy::type_complexity,
        clippy::panic,
        clippy::unwrap_used
    )
)]
#![allow(clippy::print_stderr, clippy::print_stdout)]
//! `FParkan` render-planning composition root.

use fparkan_assets::{PreparedTextureUsage, PreparedVisual};
use fparkan_platform::WindowPort;
use fparkan_platform_winit::{window_native_handles, WinitWindow, WinitWindowPlan};
use fparkan_render::{
    build_commands, CameraSnapshot, DrawId, GpuMaterialId, GpuMeshId, IndexRange, RenderBackend,
    RenderCommand, RenderCommandList, RenderPhase, RenderProfile, RenderSnapshot,
    RenderSnapshotDraw,
};
use fparkan_render_vulkan::{
    project_land_msh_to_static_mesh_in_xz_frame, project_msh_to_static_mesh_in_xz_frame,
    VulkanPlanningBackend, VulkanSmokeFrameOutcome, VulkanSmokeRenderer,
    VulkanSmokeRendererCreateInfo, VulkanStaticMaterial, VulkanStaticMesh, VulkanStaticTexture,
    VulkanStaticXzFrame,
};
use fparkan_runtime::{
    create, frame, load_mission, load_mission_static_preview, load_mission_static_preview_roots,
    load_mission_static_preview_roots_with_progress, load_mission_with_progress,
    loaded_mission_assets, loaded_mission_object_drafts, loaded_terrain, EngineConfig, EngineMode,
    EngineServices, MissionAssets, MissionLoadPhase, MissionObjectDraft, MissionRequest,
};
use fparkan_terrain::TerrainWorld;
use fparkan_vfs::DirectoryVfs;
#[cfg(test)]
use fparkan_world::OriginalObjectId;
use fparkan_world::WorldSnapshot;
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
    let mut engine = create(
        EngineConfig {
            mode: EngineMode::Rendered,
        },
        services,
    )
    .map_err(|err| err.to_string())?;
    let loaded = load_requested_mission(&mut engine, &args)?;

    if args.backend == RenderBackendMode::StaticVulkan {
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
        let preview = static_preview_mesh_and_materials(mission_assets, terrain, roots)?;
        return run_static_vulkan_mode(
            preview,
            roots.len(),
            args.frames,
            &args.mission,
            loaded.object_count,
        );
    }

    let mut backend = VulkanPlanningBackend::new();
    let _request = WinitWindow::default_render_request();
    let window = WinitWindow::synthetic(1280, 720);
    let _ = window.drawable_size();
    let _ = window.handle();
    let mut last_draw_count = 0usize;
    let mut last_tick = 0u64;
    let mut last_hash = [0u8; 32];
    for _ in 0..args.frames {
        let result = frame(&mut engine).map_err(|err| err.to_string())?;
        last_tick = result.snapshot.tick.0;
        last_hash = result.snapshot.hash.0;
        let mission_assets = loaded_mission_assets(&engine);
        let mission_drafts = fparkan_runtime::loaded_mission_object_drafts(&engine);
        let commands =
            render_snapshot_commands_with_assets(&result.snapshot, mission_assets, mission_drafts)
                .map_err(|err| format!("render snapshot: {err}"))?;
        last_draw_count = commands
            .commands
            .iter()
            .filter(|command| matches!(command, RenderCommand::Draw(_)))
            .count();
        backend
            .execute(&commands)
            .map_err(|err| format!("render backend: {err}"))?;
    }

    let capture_report = backend.report();

    Ok(format!(
        "{{\"report_kind\":\"render-planning\",\"backend\":\"vulkan-planning\",\"window\":\"synthetic\",\"mission\":{},\"objects\":{},\"frames\":{},\"tick\":{},\"draws\":{},\"submission_plans\":{},\"last_command_capture_bytes\":{},\"hash\":{}}}",
        json_string(&args.mission),
        loaded.object_count,
        args.frames,
        last_tick,
        last_draw_count,
        capture_report.execution.submission_plans,
        capture_report.execution.last_capture_size,
        json_hash(&last_hash)
    ))
}

fn load_requested_mission(
    engine: &mut fparkan_runtime::Engine,
    args: &Args,
) -> Result<fparkan_runtime::LoadedMission, String> {
    let request = MissionRequest {
        key: args.mission.clone(),
    };
    if let Some(progress_path) = args.load_progress.as_ref() {
        prepare_load_progress_path(progress_path)?;
        let mut write_error = None;
        let loaded = if args.backend == RenderBackendMode::StaticVulkan {
            load_mission_static_preview_roots_with_progress(
                engine,
                request,
                args.preview_roots,
                |phase| {
                    if write_error.is_none() {
                        if let Err(err) = write_load_progress(progress_path, phase) {
                            write_error = Some(err);
                        }
                    }
                },
            )
        } else {
            load_mission_with_progress(engine, request, |phase| {
                if write_error.is_none() {
                    if let Err(err) = write_load_progress(progress_path, phase) {
                        write_error = Some(err);
                    }
                }
            })
        }
        .map_err(|err| err.to_string())?;
        if let Some(err) = write_error {
            return Err(err);
        }
        std::fs::write(progress_path, "Complete\n")
            .map_err(|err| format!("{}: {err}", progress_path.display()))?;
        return Ok(loaded);
    }
    if args.backend == RenderBackendMode::StaticVulkan {
        if args.preview_roots.get() == 1 {
            load_mission_static_preview(engine, request)
        } else {
            load_mission_static_preview_roots(engine, request, args.preview_roots)
        }
    } else {
        load_mission(engine, request)
    }
    .map_err(|err| err.to_string())
}

/// Static geometry and descriptors belonging to the first preview root.
struct StaticPreviewScene {
    mesh: VulkanStaticMesh,
    materials: Vec<VulkanStaticMaterial>,
    mesh_components: usize,
    terrain_components: usize,
}

/// Projects the mission terrain plus every MSH component of the explicitly
/// bounded static-preview root into one diagnostic XZ frame.
///
/// This intentionally takes only the first MAT0 texture request for each MSH
/// batch selector. The terrain uses an explicit white diagnostic texture;
/// terrain slot/material selection, orientation, camera, later material phases,
/// animation, lightmaps and gameplay visibility remain outside this bridge.
fn static_preview_mesh_and_materials(
    assets: &MissionAssets,
    terrain: &TerrainWorld,
    roots: &[MissionObjectDraft],
) -> Result<StaticPreviewScene, String> {
    let terrain_mesh = terrain
        .source_mesh()
        .ok_or_else(|| "runtime terrain does not retain its validated source mesh".to_string())?;
    let frame = static_preview_xz_frame(assets, terrain, roots)?;
    let mut mesh = VulkanStaticMesh {
        vertices: Vec::new(),
        indices: Vec::new(),
        draw_ranges: Vec::new(),
    };
    let mut materials = vec![VulkanStaticMaterial {
        material_index: 0,
        texture: VulkanStaticTexture {
            width: 1,
            height: 1,
            rgba8: vec![255, 255, 255, 255],
        },
    }];
    let terrain_component = project_land_msh_to_static_mesh_in_xz_frame(terrain_mesh, frame)
        .map_err(|err| format!("project mission terrain for Vulkan: {err}"))?;
    append_static_preview_component(&mut mesh, terrain_component, &[(0, 0)])?;
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
            let component = project_msh_to_static_mesh_in_xz_frame(
                &model.validated,
                frame,
                root.position,
                root.scale,
            )
            .map_err(|err| format!("project mission MSH for Vulkan: {err}"))?;
            let selector_remap =
                static_preview_component_materials(assets, visual, &component, &mut materials)?;
            append_static_preview_component(&mut mesh, component, &selector_remap)?;
            mesh_components += 1;
        }
    }
    if mesh_components == 0 {
        return Err("selected static preview roots have no mesh-backed visual".to_string());
    }
    Ok(StaticPreviewScene {
        mesh,
        materials,
        mesh_components,
        terrain_components: 1,
    })
}

fn static_preview_xz_frame(
    assets: &MissionAssets,
    terrain: &TerrainWorld,
    roots: &[MissionObjectDraft],
) -> Result<VulkanStaticXzFrame, String> {
    let mut min_x = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut min_z = f32::INFINITY;
    let mut max_z = f32::NEG_INFINITY;
    let terrain_positions = terrain
        .source_positions()
        .ok_or_else(|| "runtime terrain does not retain source positions".to_string())?;
    for position in terrain_positions {
        extend_static_preview_xz_bounds(*position, &mut min_x, &mut max_x, &mut min_z, &mut max_z)?;
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
                extend_static_preview_xz_bounds(
                    [
                        position[0] * root.scale[0] + root.position[0],
                        position[1] * root.scale[1] + root.position[1],
                        position[2] * root.scale[2] + root.position[2],
                    ],
                    &mut min_x,
                    &mut max_x,
                    &mut min_z,
                    &mut max_z,
                )?;
            }
        }
    }
    VulkanStaticXzFrame::from_bounds(min_x, max_x, min_z, max_z)
        .map_err(|err| format!("build static preview XZ frame: {err}"))
}

fn extend_static_preview_xz_bounds(
    position: [f32; 3],
    min_x: &mut f32,
    max_x: &mut f32,
    min_z: &mut f32,
    max_z: &mut f32,
) -> Result<(), String> {
    if !position.iter().all(|value| value.is_finite()) {
        return Err("static preview contains a non-finite XZ position".to_string());
    }
    *min_x = min_x.min(position[0]);
    *max_x = max_x.max(position[0]);
    *min_z = min_z.min(position[2]);
    *max_z = max_z.max(position[2]);
    Ok(())
}

fn static_preview_component_materials(
    assets: &MissionAssets,
    visual: &PreparedVisual,
    mesh: &VulkanStaticMesh,
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
            let texture_name = material.texture_requests.first().ok_or_else(|| {
                format!("static preview material {source_selector} has no MAT0 diffuse texture")
            })?;
            let texture = assets
                .textures
                .iter()
                .find(|texture| {
                    texture.usage == PreparedTextureUsage::Diffuse
                        && texture.source.name == *texture_name
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
    let vertex_base = u16::try_from(target.vertices.len()).map_err(|_| {
        "static preview exceeds the available 16-bit vertex index space".to_string()
    })?;
    let first_index_base = u32::try_from(target.indices.len())
        .map_err(|_| "static preview index count exceeds u32".to_string())?;
    target
        .vertices
        .len()
        .checked_add(component.vertices.len())
        .filter(|count| *count <= usize::from(u16::MAX) + 1)
        .ok_or_else(|| {
            "static preview exceeds the available 16-bit vertex index space".to_string()
        })?;
    target.vertices.extend(component.vertices);
    target.indices.extend(
        component
            .indices
            .into_iter()
            .map(|index| {
                index.checked_add(vertex_base).ok_or_else(|| {
                    "static preview exceeds the available 16-bit vertex index space".to_string()
                })
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

fn prepare_load_progress_path(path: &std::path::Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| format!("{}: {err}", parent.display()))?;
    }
    std::fs::write(path, "Starting\n").map_err(|err| format!("{}: {err}", path.display()))
}

fn write_load_progress(path: &std::path::Path, phase: MissionLoadPhase) -> Result<(), String> {
    std::fs::write(path, format!("{phase:?}\n")).map_err(|err| format!("{}: {err}", path.display()))
}

fn run_static_vulkan_mode(
    preview: StaticPreviewScene,
    preview_roots: usize,
    target_frames: u64,
    mission: &str,
    object_count: usize,
) -> Result<String, String> {
    let event_loop = EventLoop::new().map_err(|err| format!("winit event loop: {err}"))?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app =
        StaticVulkanApp::new(preview, preview_roots, target_frames, mission, object_count);
    if let Err(err) = event_loop.run_app(&mut app) {
        app.error = Some(format!("winit event loop: {err}"));
    }
    app.finish()
}

struct StaticVulkanApp {
    mesh: VulkanStaticMesh,
    materials: Vec<VulkanStaticMaterial>,
    mesh_components: usize,
    terrain_components: usize,
    preview_roots: usize,
    target_frames: u64,
    mission: String,
    object_count: usize,
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
        preview_roots: usize,
        target_frames: u64,
        mission: &str,
        object_count: usize,
    ) -> Self {
        Self {
            mesh: preview.mesh,
            materials: preview.materials,
            mesh_components: preview.mesh_components,
            terrain_components: preview.terrain_components,
            preview_roots,
            target_frames,
            mission: mission.to_string(),
            object_count,
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
        if report.validation.warning_count != 0 || report.validation.error_count != 0 {
            self.error = Some(format!(
                "native Vulkan validation must stay clean (warnings={}, errors={})",
                report.validation.warning_count, report.validation.error_count
            ));
            event_loop.exit();
            return;
        }
        self.output = Some(format!(
            "{{\"report_kind\":\"rendered-static-mission\",\"backend\":\"vulkan-static\",\"window\":\"native\",\"mission\":{},\"objects\":{},\"frames\":{},\"preview_roots\":{},\"mesh_components\":{},\"terrain_components\":{},\"material_descriptors\":{},\"swapchain\":[{},{}],\"swapchain_images\":{},\"validation_warnings\":{},\"validation_errors\":{},\"readback_bytes\":{},\"readback_hash\":{}}}",
            json_string(&self.mission),
            self.object_count,
            self.frames_presented,
            self.preview_roots,
            self.mesh_components,
            self.terrain_components,
            self.materials.len(),
            report.renderer_report.swapchain_extent.0,
            report.renderer_report.swapchain_extent.1,
            report.renderer_report.swapchain_image_count,
            report.validation.warning_count,
            report.validation.error_count,
            report.renderer_report.readback_byte_count,
            report.renderer_report.readback_fnv1a64,
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
            enable_validation: true,
            mesh: self.mesh.clone(),
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
                self.error = Some("native Vulkan window closed before completion".to_string());
                event_loop.exit();
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

#[cfg(test)]
fn render_snapshot_commands(snapshot: &WorldSnapshot) -> Result<RenderCommandList, String> {
    render_snapshot_commands_with_assets(snapshot, None, None)
}

fn render_snapshot_commands_with_assets(
    snapshot: &WorldSnapshot,
    mission_assets: Option<&MissionAssets>,
    mission_drafts: Option<&[MissionObjectDraft]>,
) -> Result<RenderCommandList, String> {
    let render_snapshot = render_snapshot_with_assets(snapshot, mission_assets, mission_drafts);
    build_commands(&render_snapshot, RenderProfile::default()).map_err(|err| err.to_string())
}

fn render_snapshot_with_assets(
    snapshot: &WorldSnapshot,
    mission_assets: Option<&MissionAssets>,
    mission_drafts: Option<&[MissionObjectDraft]>,
) -> RenderSnapshot {
    let mut draws = Vec::with_capacity(snapshot.objects.len());
    for (index, handle) in snapshot.objects.iter().enumerate() {
        let visuals = mission_assets.map_or(&[][..], |assets| assets.visuals_for_object(index));
        let visual_count = visuals.len().max(1);
        for visual_index in 0..visual_count {
            let prepared = mission_assets.and_then(|assets| {
                visuals
                    .get(visual_index)
                    .and_then(|visual_id| assets.visual_by_id(*visual_id))
            });
            let mesh = prepared.map_or_else(
                || GpuMeshId(u64::from(handle.slot) + 1),
                |visual| GpuMeshId(visual.id.raw()),
            );
            let material = prepared
                .and_then(PreparedVisual::primary_material_id)
                .map_or(GpuMaterialId(1), |material_id| {
                    GpuMaterialId(material_id.raw())
                });
            let stable_order = if visual_count == 1 {
                u64::from(handle.slot)
            } else {
                u64::from(handle.slot)
                    .wrapping_mul(1_000_003)
                    .wrapping_add(u64::try_from(visual_index).unwrap_or(u64::MAX))
            };
            let draw_id = snapshot
                .tick
                .0
                .wrapping_mul(1_000_003)
                .wrapping_add(stable_order);
            draws.push(RenderSnapshotDraw {
                id: DrawId(draw_id),
                phase: RenderPhase::Opaque,
                object_id: mission_drafts
                    .and_then(|drafts| drafts.get(index))
                    .and_then(|draft| draft.original_id),
                mesh,
                material_slots: vec![material],
                material_index: 0,
                pipeline_state: fparkan_render::LegacyPipelineState::default(),
                transform: mission_drafts
                    .and_then(|drafts| drafts.get(index))
                    .map_or_else(
                        || identity_transform(index_to_f32(index)),
                        mission_position_scale_transform,
                    ),
                range: IndexRange { start: 0, count: 3 },
                stable_order,
            });
        }
    }
    RenderSnapshot {
        camera: CameraSnapshot::default(),
        draws,
    }
}

fn mission_position_scale_transform(draft: &MissionObjectDraft) -> [f32; 16] {
    [
        draft.scale[0],
        0.0,
        0.0,
        draft.position[0],
        0.0,
        draft.scale[1],
        0.0,
        draft.position[1],
        0.0,
        0.0,
        draft.scale[2],
        draft.position[2],
        0.0,
        0.0,
        0.0,
        1.0,
    ]
}

fn identity_transform(x: f32) -> [f32; 16] {
    [
        1.0, 0.0, 0.0, x, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ]
}

fn index_to_f32(index: usize) -> f32 {
    u16::try_from(index).map_or(f32::from(u16::MAX), f32::from)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Args {
    root: PathBuf,
    mission: String,
    frames: u64,
    backend: RenderBackendMode,
    load_progress: Option<PathBuf>,
    preview_roots: NonZeroUsize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RenderBackendMode {
    Planning,
    StaticVulkan,
}

impl Args {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut root = None;
        let mut mission = None;
        let mut frames = 1;
        let mut backend = RenderBackendMode::Planning;
        let mut load_progress = None;
        let mut preview_roots = NonZeroUsize::MIN;
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
                "--backend" => {
                    backend = match iter
                        .next()
                        .ok_or_else(|| "--backend requires a value".to_string())?
                        .as_str()
                    {
                        "planning" => RenderBackendMode::Planning,
                        "static-vulkan" => RenderBackendMode::StaticVulkan,
                        _ => return Err("--backend must be planning or static-vulkan".to_string()),
                    };
                }
                "--load-progress" => {
                    load_progress = Some(
                        iter.next()
                            .map(PathBuf::from)
                            .ok_or_else(|| "--load-progress requires a path".to_string())?,
                    );
                }
                "--preview-roots" => {
                    preview_roots = iter
                        .next()
                        .ok_or_else(|| "--preview-roots requires a value".to_string())?
                        .parse()
                        .map_err(|_| "--preview-roots must be a non-zero integer".to_string())?;
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
            backend,
            load_progress,
            preview_roots,
        })
    }
}

fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn json_hash(hash: &[u8; 32]) -> String {
    let mut out = String::from("\"");
    for byte in hash {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out.push('"');
    out
}

fn usage() -> String {
    "usage: fparkan-game --root <path> --mission <path> [--frames <n>] [--backend <planning|static-vulkan>] [--preview-roots <non-zero n>] [--load-progress <path>]".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_assets::AssetId;
    use fparkan_world::{ObjectHandle, StateHash, Tick};
    use std::path::Path;

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
                backend: RenderBackendMode::Planning,
                load_progress: None,
                preview_roots: NonZeroUsize::MIN,
            })
        );
    }

    #[test]
    fn parses_static_vulkan_backend() {
        assert_eq!(
            Args::parse(&strings(&[
                "--root",
                "testdata/IS",
                "--mission",
                "MISSIONS/Autodemo.00/data.tma",
                "--backend",
                "static-vulkan",
            ])),
            Ok(Args {
                root: PathBuf::from("testdata/IS"),
                mission: "MISSIONS/Autodemo.00/data.tma".to_string(),
                frames: 1,
                backend: RenderBackendMode::StaticVulkan,
                load_progress: None,
                preview_roots: NonZeroUsize::MIN,
            })
        );
    }

    #[test]
    fn parses_nonzero_static_preview_root_count() {
        let parsed = Args::parse(&strings(&[
            "--root",
            "testdata/IS",
            "--mission",
            "MISSIONS/Autodemo.00/data.tma",
            "--backend",
            "static-vulkan",
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
    fn parses_load_progress_path() {
        assert_eq!(
            Args::parse(&strings(&[
                "--root",
                "testdata/IS",
                "--mission",
                "MISSIONS/Autodemo.00/data.tma",
                "--load-progress",
                "target/probe.txt",
            ])),
            Ok(Args {
                root: PathBuf::from("testdata/IS"),
                mission: "MISSIONS/Autodemo.00/data.tma".to_string(),
                frames: 1,
                backend: RenderBackendMode::Planning,
                load_progress: Some(PathBuf::from("target/probe.txt")),
                preview_roots: NonZeroUsize::MIN,
            })
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
    fn render_commands_follow_snapshot_order() -> Result<(), String> {
        let snapshot = WorldSnapshot {
            tick: Tick(7),
            objects: vec![
                ObjectHandle {
                    generation: 1,
                    slot: 2,
                },
                ObjectHandle {
                    generation: 1,
                    slot: 5,
                },
            ],
            events: Vec::new(),
            hash: StateHash([0; 32]),
        };

        let commands = render_snapshot_commands(&snapshot)?;

        assert_eq!(commands.commands.len(), 4);
        assert!(matches!(
            commands.commands[0],
            fparkan_render::RenderCommand::BeginFrame
        ));
        assert!(matches!(
            commands.commands[3],
            fparkan_render::RenderCommand::EndFrame
        ));
        let fparkan_render::RenderCommand::Draw(first) = &commands.commands[1] else {
            return Err("expected draw".to_string());
        };
        assert_eq!(first.mesh, GpuMeshId(3));
        assert_eq!(first.stable_order, 2);
        Ok(())
    }

    #[test]
    fn render_snapshot_retains_every_prepared_visual_for_an_object() -> Result<(), String> {
        let snapshot = WorldSnapshot {
            tick: Tick(3),
            objects: vec![ObjectHandle {
                generation: 1,
                slot: 7,
            }],
            events: Vec::new(),
            hash: StateHash([0; 32]),
        };
        let first = prepared_visual(101, 201);
        let second = prepared_visual(102, 202);
        let assets = MissionAssets {
            visuals: vec![first, second],
            object_visuals: vec![vec![AssetId::new(101), AssetId::new(102)]],
            ..MissionAssets::default()
        };
        let commands = render_snapshot_commands_with_assets(&snapshot, Some(&assets), None)?;
        let draws: Vec<_> = commands
            .commands
            .iter()
            .filter_map(|command| match command {
                RenderCommand::Draw(draw) => Some(draw),
                _ => None,
            })
            .collect();
        assert_eq!(draws.len(), 2);
        assert_eq!(draws[0].mesh, GpuMeshId(101));
        assert_eq!(draws[1].mesh, GpuMeshId(102));
        assert_eq!(draws[0].material, GpuMaterialId(201));
        assert_eq!(draws[1].material, GpuMaterialId(202));
        assert_ne!(draws[0].stable_order, draws[1].stable_order);
        Ok(())
    }

    #[test]
    fn render_snapshot_uses_mission_position_and_scale_without_interpreting_orientation() {
        let draft = MissionObjectDraft {
            original_id: None,
            resource_name_raw: Vec::new(),
            identity_or_clan_raw: 0,
            position: [10.0, 20.0, 30.0],
            orientation_raw: [1.0, 2.0, 3.0],
            scale: [2.0, 3.0, 4.0],
            visual_ids: Vec::new(),
            properties: Vec::new(),
        };
        assert_eq!(
            mission_position_scale_transform(&draft),
            [2.0, 0.0, 0.0, 10.0, 0.0, 3.0, 0.0, 20.0, 0.0, 0.0, 4.0, 30.0, 0.0, 0.0, 0.0, 1.0,]
        );
    }

    #[test]
    fn render_snapshot_preserves_mission_original_object_id() -> Result<(), String> {
        let snapshot = WorldSnapshot {
            tick: Tick(1),
            objects: vec![ObjectHandle {
                generation: 1,
                slot: 0,
            }],
            events: Vec::new(),
            hash: StateHash([0; 32]),
        };
        let drafts = vec![MissionObjectDraft {
            original_id: Some(OriginalObjectId(42)),
            resource_name_raw: Vec::new(),
            identity_or_clan_raw: 0,
            position: [0.0; 3],
            orientation_raw: [0.0; 3],
            scale: [1.0; 3],
            visual_ids: Vec::new(),
            properties: Vec::new(),
        }];

        let commands = render_snapshot_commands_with_assets(&snapshot, None, Some(&drafts))?;
        let RenderCommand::Draw(draw) = &commands.commands[1] else {
            return Err("expected draw".to_string());
        };
        assert_eq!(draw.object_id, Some(OriginalObjectId(42)));
        Ok(())
    }

    fn prepared_visual(id: u64, material: u64) -> PreparedVisual {
        PreparedVisual {
            id: AssetId::new(id),
            mesh: None,
            model_id: None,
            wear_id: None,
            model_nodes: 0,
            model_slots: 0,
            model_batches: 0,
            material_count: 1,
            material_ids: vec![AssetId::new(material)],
            texture_ids: Vec::new(),
            lightmap_ids: Vec::new(),
            texture_count: 0,
            lightmap_count: 0,
        }
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn selected_is_and_is2_missions_produce_approved_render_captures() {
        for case in [
            RenderCase {
                root: "IS",
                mission: "MISSIONS/CAMPAIGN/CAMPAIGN.00/Mission.01/data.tma",
                expected: "{\"report_kind\":\"render-planning\",\"backend\":\"vulkan-planning\",\"window\":\"synthetic\",\"mission\":\"MISSIONS/CAMPAIGN/CAMPAIGN.00/Mission.01/data.tma\",\"objects\":33,\"frames\":1,\"tick\":1,\"draws\":33,\"submission_plans\":1,\"last_command_capture_bytes\":2008,\"hash\":\"ca17cc76e55c45e83c1c9c1c088e84bf1a698be91a7730943210fe27596af841\"}",
            },
            RenderCase {
                root: "IS2",
                mission: "MISSIONS/Campaign/CAMPAIGN.00/Mission.02/data.tma",
                expected: "{\"report_kind\":\"render-planning\",\"backend\":\"vulkan-planning\",\"window\":\"synthetic\",\"mission\":\"MISSIONS/Campaign/CAMPAIGN.00/Mission.02/data.tma\",\"objects\":10,\"frames\":1,\"tick\":1,\"draws\":10,\"submission_plans\":1,\"last_command_capture_bytes\":605,\"hash\":\"5d720b3ab690076a398a79a404850bbeaee2e33811b5bb570ec8a96d4a7a2fc4\"}",
            },
        ] {
            assert_eq!(
                run(&render_args(&licensed_root(case.root), case.mission)),
                Ok(case.expected.to_string())
            );
        }
    }

    #[test]
    fn json_hash_is_hex() {
        let mut hash = [0; 32];
        hash[0] = 0xab;
        hash[31] = 0xcd;

        assert_eq!(
            json_hash(&hash),
            "\"ab000000000000000000000000000000000000000000000000000000000000cd\""
        );
    }

    #[derive(Clone, Copy)]
    struct RenderCase {
        root: &'static str,
        mission: &'static str,
        expected: &'static str,
    }

    fn render_args(root: &Path, mission: &str) -> Vec<String> {
        vec![
            "--root".to_string(),
            root.to_str().expect("utf8 root").to_string(),
            "--mission".to_string(),
            mission.to_string(),
            "--frames".to_string(),
            "1".to_string(),
        ]
    }

    fn licensed_root(name: &str) -> PathBuf {
        let variable = match name {
            "IS" => "FPARKAN_CORPUS_PART1_ROOT",
            "IS2" => "FPARKAN_CORPUS_PART2_ROOT",
            _ => panic!("unknown licensed corpus part: {name}"),
        };
        let root = std::env::var_os(variable)
            .map(PathBuf::from)
            .unwrap_or_else(|| panic!("{variable} is required for licensed corpus tests"));
        assert!(
            root.is_dir(),
            "licensed corpus root is missing: {}",
            root.display()
        );
        root
    }
}
