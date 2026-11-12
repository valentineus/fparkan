#![cfg(target_os = "windows")]

use fparkan_platform_winit::{window_native_handles, WinitWindow};
use fparkan_render::{LegacyBlendMode, LegacyCullMode, LegacyDepthMode};
use fparkan_render_vulkan::{
    VulkanFrameUniforms, VulkanSmokeFrameOutcome, VulkanSmokeRenderer,
    VulkanSmokeRendererCreateInfo, VulkanStaticCamera, VulkanStaticMesh,
};
use winit::dpi::PhysicalSize;
use winit::event_loop::EventLoop;
use winit::platform::windows::EventLoopBuilderExtWindows;
use winit::window::Window;

#[test]
#[ignore = "requires a Windows display, Vulkan RGB565 sampling and validation layers"]
#[allow(deprecated)]
fn reflection_copies_finished_rgb8_color_with_native_bit_drop_on_gpu() {
    let event_loop = EventLoop::builder().with_any_thread(true).build().unwrap();
    let window = event_loop
        .create_window(
            Window::default_attributes()
                .with_title("FParkan reflection copy regression")
                .with_visible(false)
                .with_inner_size(PhysicalSize::new(64, 64)),
        )
        .unwrap();
    let size = window.inner_size();
    let camera = VulkanStaticCamera::default();
    let mut mesh = VulkanStaticMesh::smoke_triangle();
    let mut vertex = mesh.vertices[0];
    mesh.vertices.clear();
    mesh.indices.clear();
    mesh.draw_ranges[0].index_count = 6;
    mesh.draw_ranges[0].pipeline_state.cull = LegacyCullMode::Disabled;
    mesh.draw_ranges[0].pipeline_state.depth = LegacyDepthMode::TestWrite;
    mesh.draw_ranges.push(mesh.draw_ranges[0]);
    mesh.draw_ranges.push(mesh.draw_ranges[0]);
    mesh.draw_ranges[0].terrain_surface = true;
    mesh.draw_ranges[0].terrain_reflection = true;
    mesh.draw_ranges[1].first_index = 6;
    mesh.draw_ranges[2].first_index = 12;
    mesh.draw_ranges[2].pipeline_state.blend = LegacyBlendMode::SourceAlpha;
    for (height, color) in [
        (0.2, [1.0; 3]),
        (0.3, [7.0 / 255.0, 127.0 / 255.0, 248.0 / 255.0]),
        (0.31, [15.0 / 255.0, 127.0 / 255.0, 248.0 / 255.0]),
    ] {
        let first = mesh.vertices.len() as u32;
        vertex.color = color;
        for (x, y) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
            vertex.position = [x, y, height];
            mesh.vertices.push(vertex);
        }
        mesh.indices.extend([0, 1, 2, 2, 1, 3].map(|i| i + first));
    }
    let mut renderer = VulkanSmokeRenderer::new(&VulkanSmokeRendererCreateInfo {
        application_name: "fparkan-reflection-copy-test".into(),
        native_handles: window_native_handles(&window).unwrap(),
        drawable_extent: (size.width, size.height),
        render_request: WinitWindow::default_render_request(),
        enable_validation: true,
        mesh,
        camera,
        materials: Vec::new(),
        bootstrap_progress: None,
    })
    .unwrap();
    let mut frame = VulkanFrameUniforms::from_camera(camera);
    for light in &mut frame.directional_lights {
        light.active = false;
    }
    frame.camera_position = [0.0, 0.0, 1.0];
    frame.lighting_floor = [0.0; 3];
    renderer.set_frame_uniforms(frame).unwrap();
    renderer
        .set_material_lighting(0, [0.0; 3], [1.0; 3])
        .unwrap();
    renderer.set_readback_enabled(true);
    for (alpha, expected_red) in [(0.4, 8_u8), (0.0, 0)] {
        renderer
            .set_draw_range_diffuse_alpha_override(2, Some(alpha))
            .unwrap();
        let mut presented = 0;
        for _ in 0..8 {
            if renderer.draw_frame().unwrap() == VulkanSmokeFrameOutcome::Presented {
                presented += 1;
                if presented == 3 {
                    break;
                }
            }
        }
        assert_eq!(presented, 3);
        let image = renderer.readback_last_submitted_frame().unwrap().unwrap();
        let offset = ((image.height / 2 * image.width + image.width / 2) * 4) as usize;
        let channels = match image.format {
            37 => [0, 1, 2],
            44 => [2, 1, 0],
            format => panic!("reflection regression requires UNORM, got {format}"),
        };
        // Native GDI drops bits after RGB8 blending: red 10 -> 1, or red 7 -> 0.
        for (channel, expected) in channels.into_iter().zip([expected_red, 125, 255]) {
            assert!(
                image.bytes[offset + channel].abs_diff(expected) <= 1,
                "alpha={alpha} pixel={:?}",
                &image.bytes[offset..offset + 4]
            );
        }
    }
    let report = renderer.shutdown().unwrap();
    assert_eq!(report.validation.error_count, 0);
    assert_eq!(report.validation.warning_count, 0);
}
