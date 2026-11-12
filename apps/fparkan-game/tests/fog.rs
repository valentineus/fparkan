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
fn lit_color_is_saturated_before_fog_on_gpu() {
    let event_loop = EventLoop::builder().with_any_thread(true).build().unwrap();
    let window = event_loop
        .create_window(
            Window::default_attributes()
                .with_title("FParkan fog regression")
                .with_visible(false)
                .with_inner_size(PhysicalSize::new(64, 64)),
        )
        .unwrap();
    let size = window.inner_size();
    let camera = VulkanStaticCamera::default();
    let mut mesh = VulkanStaticMesh::smoke_triangle();
    let mut vertex = mesh.vertices[0];
    vertex.color = [1.0; 3];
    mesh.vertices = [
        [-1.0, -1.0, 0.5],
        [1.0, -1.0, 0.5],
        [-1.0, 1.0, 0.5],
        [1.0, 1.0, 0.5],
    ]
    .map(|position| {
        vertex.position = position;
        vertex
    })
    .to_vec();
    mesh.indices = vec![0, 1, 2, 2, 1, 3];
    mesh.draw_ranges[0].index_count = 6;
    let mut renderer = VulkanSmokeRenderer::new(&VulkanSmokeRendererCreateInfo {
        application_name: "fparkan-fog-test".into(),
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
    frame.fog_color = [0.2, 0.4, 0.6];
    frame.fog_start = 0.0;
    // Equal vertex radii give exactly 50% fog across the entire quad.
    frame.fog_end = 4.5_f32.sqrt();
    renderer.set_frame_uniforms(frame).unwrap();
    renderer
        .set_material_lighting(0, [0.0; 3], [7.0; 3])
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
    let offset = ((image.height / 2 * image.width + image.width / 2) * 4) as usize;
    let channels = match image.format {
        37 => [0, 1, 2], // R8G8B8A8_UNORM
        44 => [2, 1, 0], // B8G8R8A8_UNORM
        format => panic!("fog regression requires an UNORM surface, got {format}"),
    };
    for (channel, expected) in channels.into_iter().zip([153_u8, 179, 204]) {
        assert!(
            image.bytes[offset + channel].abs_diff(expected) <= 1,
            "{:?}",
            &image.bytes[offset..offset + 4]
        );
    }
    let report = renderer.shutdown().unwrap();
    assert_eq!(report.validation.error_count, 0);
    assert_eq!(report.validation.warning_count, 0);
}
