#![cfg(target_os = "windows")]

use fparkan_platform_winit::{window_native_handles, WinitWindow};
use fparkan_render_vulkan::{
    VulkanDirectionalLight, VulkanFrameUniforms, VulkanSmokeFrameOutcome, VulkanSmokeRenderer,
    VulkanSmokeRendererCreateInfo, VulkanStaticCamera, VulkanStaticMesh,
};
use winit::dpi::PhysicalSize;
use winit::event_loop::EventLoop;
use winit::platform::windows::EventLoopBuilderExtWindows;
use winit::window::Window;

#[test]
#[ignore = "requires a Windows display, Vulkan GPU and validation layers"]
#[allow(deprecated)]
fn directional_specular_flag_keeps_diffuse_light_on_gpu() {
    let event_loop = EventLoop::builder().with_any_thread(true).build().unwrap();
    let window = event_loop
        .create_window(
            Window::default_attributes()
                .with_title("FParkan directional light regression")
                .with_visible(false)
                .with_inner_size(PhysicalSize::new(64, 64)),
        )
        .unwrap();
    let size = window.inner_size();
    let camera = VulkanStaticCamera::default();
    let mut mesh = VulkanStaticMesh::smoke_triangle();
    let mut vertex = mesh.vertices[0];
    vertex.color = [1.0; 3];
    vertex.normal = [0.0, 0.0, 1.0];
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
        application_name: "fparkan-directional-specular-test".into(),
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
    frame.directional_lights = [VulkanDirectionalLight::default(); 4];
    frame.directional_lights[0] = VulkanDirectionalLight {
        direction: [0.0, 0.0, -1.0],
        rgb: [0.0, 0.25, 0.0],
        active: true,
        specular_enabled: false,
        ..Default::default()
    };
    frame.lighting_floor = [0.0; 3];
    frame.camera_position = [0.0, 0.0, 1000.0];
    renderer
        .set_material_lighting(0, [1.0; 3], [0.0; 3])
        .unwrap();
    renderer
        .set_material_specular(0, [1.0; 3], 1, None, None)
        .unwrap();
    renderer.set_readback_enabled(true);
    for (enabled, expected_green) in [(false, 64_u8), (true, 115)] {
        frame.directional_lights[0].specular_enabled = enabled;
        renderer.set_frame_uniforms(frame).unwrap();
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
            format => panic!("directional regression requires UNORM, got {format}"),
        };
        for (channel, expected) in channels.into_iter().zip([0, expected_green, 0]) {
            assert!(
                image.bytes[offset + channel].abs_diff(expected) <= 1,
                "specular={enabled}: {:?}",
                &image.bytes[offset..offset + 4]
            );
        }
    }
    let report = renderer.shutdown().unwrap();
    assert_eq!(report.validation.error_count, 0);
    assert_eq!(report.validation.warning_count, 0);
}
