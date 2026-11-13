#![cfg(target_os = "windows")]

use fparkan_platform_winit::{window_native_handles, WinitWindow};
use fparkan_render_vulkan::{
    VulkanFrameUniforms, VulkanSmokeFrameOutcome, VulkanSmokeRenderer,
    VulkanSmokeRendererCreateInfo, VulkanStaticCamera, VulkanStaticMaterial, VulkanStaticMesh,
    VulkanStaticTexture,
};
use winit::dpi::PhysicalSize;
use winit::event_loop::EventLoop;
use winit::platform::windows::EventLoopBuilderExtWindows;
use winit::window::Window;

#[test]
#[ignore = "requires a Windows display, Vulkan GPU and validation layers"]
#[allow(deprecated)]
fn alpha_blended_world_effect_receives_fog_but_screen_effect_does_not() {
    let event_loop = EventLoop::builder().with_any_thread(true).build().unwrap();
    let window = event_loop
        .create_window(
            Window::default_attributes()
                .with_title("FParkan world FX fog regression")
                .with_visible(false)
                .with_inner_size(PhysicalSize::new(64, 64)),
        )
        .unwrap();
    let size = window.inner_size();
    let camera = VulkanStaticCamera::default();
    for screen_tl in [false, true] {
        let mut mesh = VulkanStaticMesh::smoke_triangle();
        let mut vertex = mesh.vertices[0];
        vertex.color = [1.0; 3];
        vertex.overlay_alpha = 1.0;
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
        mesh.draw_ranges[0].pipeline_state = mesh.draw_ranges[0]
            .pipeline_state
            .with_material_category(1)
            .unwrap();
        let material = VulkanStaticMaterial {
            material_index: 0,
            pipeline_category: 1,
            texture: VulkanStaticTexture {
                width: 1,
                height: 1,
                rgba8: vec![0, 0, 0, 255],
                mip_levels: Vec::new(),
            },
            directional_rgb: [0.0; 3],
            additive_rgb: [1.0; 3],
            uv_transform: [0.0, 0.0, 1.0, 1.0],
            detail_texture: None,
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
            sky_far_depth: false,
            screen_tl,
        };
        let mut renderer = VulkanSmokeRenderer::new(&VulkanSmokeRendererCreateInfo {
            application_name: "fparkan-unlit-fog-test".into(),
            native_handles: window_native_handles(&window).unwrap(),
            drawable_extent: (size.width, size.height),
            render_request: WinitWindow::default_render_request(),
            enable_validation: true,
            mesh,
            camera,
            materials: vec![material],
            bootstrap_progress: None,
        })
        .unwrap();
        let mut frame = VulkanFrameUniforms::from_camera(camera);
        frame.fog_color = [0.2, 0.4, 0.6];
        frame.fog_start = 0.0;
        // Equal vertex radii give exactly 50% fog across the world quad.
        frame.fog_end = 4.5_f32.sqrt();
        renderer.set_frame_uniforms(frame).unwrap();
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
            37 => [0, 1, 2],
            44 => [2, 1, 0],
            format => panic!("fog regression requires an UNORM surface, got {format}"),
        };
        let expected = if screen_tl { [0_u8; 3] } else { [26, 51, 77] };
        let report = renderer.shutdown().unwrap();
        assert_eq!(report.validation.error_count, 0);
        assert_eq!(report.validation.warning_count, 0);
        for (channel, expected) in channels.into_iter().zip(expected) {
            assert!(
                image.bytes[offset + channel].abs_diff(expected) <= 1,
                "screen_tl={screen_tl}: {:?}",
                &image.bytes[offset..offset + 4]
            );
        }
    }
}
