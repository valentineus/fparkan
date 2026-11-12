#![cfg(target_os = "windows")]

use fparkan_platform_winit::{window_native_handles, WinitWindow};
use fparkan_render_vulkan::{
    VulkanFrameUniforms, VulkanSmokeFrameOutcome, VulkanSmokeRenderer,
    VulkanSmokeRendererCreateInfo, VulkanStaticCamera, VulkanStaticMesh,
};
use winit::dpi::PhysicalSize;
use winit::event_loop::EventLoop;
use winit::platform::windows::EventLoopBuilderExtWindows;
use winit::window::Window;

#[test]
#[ignore = "requires a Windows display, Vulkan GPU and validation layers"]
#[allow(deprecated)]
fn gouraud_gradient_uses_native_integer_pixel_centers_on_gpu() {
    let event_loop = EventLoop::builder().with_any_thread(true).build().unwrap();
    let window = event_loop
        .create_window(
            Window::default_attributes()
                .with_title("FParkan pixel center regression")
                .with_visible(false)
                .with_inner_size(PhysicalSize::new(64, 64)),
        )
        .unwrap();
    let size = window.inner_size();
    let camera = VulkanStaticCamera::default();
    let mut mesh = VulkanStaticMesh::smoke_triangle();
    let mut vertex = mesh.vertices[0];
    mesh.vertices = [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)]
        .map(|(x, y)| {
            vertex.position = [x, y, 0.5];
            vertex.color = [(x + 1.0) * 0.5, (y + 1.0) * 0.5, 0.0];
            vertex
        })
        .to_vec();
    mesh.indices = vec![0, 1, 2, 2, 1, 3];
    mesh.draw_ranges[0].index_count = 6;
    mesh.draw_ranges[0].terrain_base_only = true;
    let mut renderer = VulkanSmokeRenderer::new(&VulkanSmokeRendererCreateInfo {
        application_name: "fparkan-pixel-center-test".into(),
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
    frame.lighting_floor = [0.0; 3];
    renderer.set_frame_uniforms(frame).unwrap();
    renderer
        .set_material_lighting(0, [0.0; 3], [1.0; 3])
        .unwrap();
    renderer.set_readback_enabled(true);
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
    let channels = match image.format {
        37 => [0, 1, 2],
        44 => [2, 1, 0],
        format => panic!("pixel center regression requires UNORM, got {format}"),
    };
    for (x, y) in [(8, 12), (24, 40), (47, 20)] {
        let offset = ((y * image.width + x) * 4) as usize;
        let expected = [
            (x as f32 * 255.0 / image.width as f32).round() as u8,
            (y as f32 * 255.0 / image.height as f32).round() as u8,
            0,
        ];
        for (channel, expected) in channels.into_iter().zip(expected) {
            assert!(
                image.bytes[offset + channel].abs_diff(expected) <= 1,
                "pixel ({x}, {y}): {:?}, expected {expected}",
                &image.bytes[offset..offset + 4]
            );
        }
    }
    let report = renderer.shutdown().unwrap();
    assert_eq!(report.validation.error_count, 0);
    assert_eq!(report.validation.warning_count, 0);
}
