use glow::HasContext as _;
use render_mission_demo::{
    compute_scene_bounds, detect_game_root_from_mission_path, load_scene_with_options, LoadOptions,
    MissionScene, ModelInstance,
};
use std::io::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

struct Args {
    mission: PathBuf,
    game_root: Option<PathBuf>,
    width: u32,
    height: u32,
    fov_deg: f32,
    no_model_texture: bool,
    no_terrain_texture: bool,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum GlBackend {
    Gles2,
    Core33,
}

struct GpuTexture {
    handle: glow::NativeTexture,
}

struct GpuRenderable {
    vbo: glow::NativeBuffer,
    ebo: glow::NativeBuffer,
    index_count: usize,
    texture: Option<GpuTexture>,
}

struct ModelRenderable {
    gpu: GpuRenderable,
    instances: Vec<ModelInstance>,
}

#[derive(Copy, Clone, Debug)]
struct Camera {
    position: [f32; 3],
    yaw: f32,
    pitch: f32,
    move_speed: f32,
    mouse_sensitivity: f32,
}

fn parse_args() -> Result<Args, String> {
    let mut mission = None;
    let mut game_root = None;
    let mut width = 1600u32;
    let mut height = 900u32;
    let mut fov_deg = 60.0f32;
    let mut no_model_texture = false;
    let mut no_terrain_texture = false;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--mission" => {
                let value = it
                    .next()
                    .ok_or_else(|| String::from("missing value for --mission"))?;
                mission = Some(PathBuf::from(value));
            }
            "--game-root" => {
                let value = it
                    .next()
                    .ok_or_else(|| String::from("missing value for --game-root"))?;
                game_root = Some(PathBuf::from(value));
            }
            "--width" => {
                let value = it
                    .next()
                    .ok_or_else(|| String::from("missing value for --width"))?;
                width = value
                    .parse::<u32>()
                    .map_err(|_| String::from("invalid --width value"))?;
                if width == 0 {
                    return Err(String::from("--width must be > 0"));
                }
            }
            "--height" => {
                let value = it
                    .next()
                    .ok_or_else(|| String::from("missing value for --height"))?;
                height = value
                    .parse::<u32>()
                    .map_err(|_| String::from("invalid --height value"))?;
                if height == 0 {
                    return Err(String::from("--height must be > 0"));
                }
            }
            "--fov" => {
                let value = it
                    .next()
                    .ok_or_else(|| String::from("missing value for --fov"))?;
                fov_deg = value
                    .parse::<f32>()
                    .map_err(|_| String::from("invalid --fov value"))?;
                if !(1.0..=179.0).contains(&fov_deg) {
                    return Err(String::from("--fov must be in range [1, 179]"));
                }
            }
            "--no-model-texture" => {
                no_model_texture = true;
            }
            "--no-terrain-texture" => {
                no_terrain_texture = true;
            }
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            other => {
                return Err(format!("unknown argument: {other}"));
            }
        }
    }

    let mission = mission.ok_or_else(|| String::from("missing required --mission"))?;
    Ok(Args {
        mission,
        game_root,
        width,
        height,
        fov_deg,
        no_model_texture,
        no_terrain_texture,
    })
}

fn print_help() {
    eprintln!("parkan-render-mission-demo --mission <path/to/data.tma> [--game-root <path>] [--width W] [--height H] [--fov DEG]");
    eprintln!("                          [--no-model-texture] [--no-terrain-texture]");
    eprintln!("controls: arrows/WASD move, PageUp/PageDown vertical move, Right Mouse drag look, Shift speed-up, Esc exit");
}

fn main() {
    let args = match parse_args() {
        Ok(v) => v,
        Err(err) => {
            eprintln!("{err}");
            print_help();
            std::process::exit(2);
        }
    };

    if let Err(err) = run(args) {
        eprintln!("{err}");
        std::process::exit(1);
    }
}

fn run(args: Args) -> Result<(), String> {
    let game_root = if let Some(path) = args.game_root.clone() {
        path
    } else {
        detect_game_root_from_mission_path(&args.mission).ok_or_else(|| {
            format!(
                "failed to detect game root from mission path {} (use --game-root)",
                args.mission.display()
            )
        })?
    };

    let scene = load_scene_with_options(
        &game_root,
        &args.mission,
        LoadOptions {
            load_model_textures: !args.no_model_texture,
            load_terrain_texture: !args.no_terrain_texture,
        },
    )
    .map_err(|err| format!("failed to load mission scene: {err}"))?;

    let terrain_mesh = terrain_core::build_render_mesh(&scene.terrain)
        .map_err(|err| format!("failed to build terrain render mesh: {err}"))?;

    let instance_count = scene
        .models
        .iter()
        .map(|model| model.instances.len())
        .sum::<usize>();
    println!(
        "mission loaded: map='{}', terrain_vertices={}, terrain_faces={}, models={}, instances={}, skipped={}",
        scene.mission.footer.map_path,
        scene.terrain.positions.len(),
        scene.terrain.faces.len(),
        scene.models.len(),
        instance_count,
        scene.skipped_objects
    );

    let sdl = sdl2::init().map_err(|err| format!("failed to init SDL2: {err}"))?;
    let video = sdl
        .video()
        .map_err(|err| format!("failed to init SDL2 video: {err}"))?;

    let (mut window, _gl_ctx, gl_backend) =
        create_window_and_context(&video, args.width, args.height)?;
    let _ = video.gl_set_swap_interval(1);

    let gl = unsafe {
        glow::Context::from_loader_function(|name| video.gl_get_proc_address(name) as *const _)
    };

    let program = unsafe { create_program(&gl, gl_backend)? };
    let u_mvp = unsafe { gl.get_uniform_location(program, "u_mvp") };
    let u_use_tex = unsafe { gl.get_uniform_location(program, "u_use_tex") };
    let u_tex = unsafe { gl.get_uniform_location(program, "u_tex") };
    let a_pos = unsafe { gl.get_attrib_location(program, "a_pos") }
        .ok_or_else(|| String::from("shader attribute a_pos is missing"))?;
    let a_uv = unsafe { gl.get_attrib_location(program, "a_uv") }
        .ok_or_else(|| String::from("shader attribute a_uv is missing"))?;

    let terrain_gpu =
        unsafe { upload_terrain_renderable(&gl, &terrain_mesh, scene.terrain_texture.as_ref())? };

    let mut model_gpus = Vec::new();
    for model in &scene.models {
        let renderable = unsafe { upload_model_renderable(&gl, model)? };
        model_gpus.push(renderable);
    }

    let (scene_center, scene_radius) = initial_scene_sphere(&scene);
    let mut camera = Camera {
        position: [
            scene_center[0],
            scene_center[1] + scene_radius * 0.6,
            scene_center[2] + scene_radius * 1.4,
        ],
        yaw: std::f32::consts::PI,
        pitch: -0.28,
        move_speed: (scene_radius * 0.55).max(60.0),
        mouse_sensitivity: 0.005,
    };

    let mut events = sdl
        .event_pump()
        .map_err(|err| format!("failed to get SDL event pump: {err}"))?;
    let mut last = Instant::now();
    let mut fps_window_start = Instant::now();
    let mut fps_frames = 0u32;
    let mut fps_printed = false;
    let mut mouse_look = false;

    'main_loop: loop {
        for event in events.poll_iter() {
            match event {
                sdl2::event::Event::Quit { .. } => break 'main_loop,
                sdl2::event::Event::KeyDown {
                    keycode: Some(sdl2::keyboard::Keycode::Escape),
                    ..
                } => break 'main_loop,
                sdl2::event::Event::MouseButtonDown {
                    mouse_btn: sdl2::mouse::MouseButton::Right,
                    ..
                } => {
                    mouse_look = true;
                    sdl.mouse().set_relative_mouse_mode(true);
                }
                sdl2::event::Event::MouseButtonUp {
                    mouse_btn: sdl2::mouse::MouseButton::Right,
                    ..
                } => {
                    mouse_look = false;
                    sdl.mouse().set_relative_mouse_mode(false);
                }
                sdl2::event::Event::MouseMotion { xrel, yrel, .. } if mouse_look => {
                    camera.yaw += xrel as f32 * camera.mouse_sensitivity;
                    camera.pitch -= yrel as f32 * camera.mouse_sensitivity;
                    camera.pitch = camera.pitch.clamp(-1.54, 1.54);
                }
                _ => {}
            }
        }

        let now = Instant::now();
        let dt = (now - last).as_secs_f32().clamp(0.0, 0.05);
        last = now;

        update_camera(&events, &mut camera, dt);

        let (w, h) = window.size();
        let proj = mat4_perspective(
            args.fov_deg.to_radians(),
            (w as f32 / h.max(1) as f32).max(0.01),
            0.1,
            (scene_radius * 25.0).max(5000.0),
        );
        let forward = camera_forward(camera.yaw, camera.pitch);
        let view = mat4_look_at(
            camera.position,
            [
                camera.position[0] + forward[0],
                camera.position[1] + forward[1],
                camera.position[2] + forward[2],
            ],
            [0.0, 1.0, 0.0],
        );

        unsafe {
            draw_frame_begin(&gl, w, h);

            let terrain_mvp = mat4_mul(&proj, &view);
            draw_gpu_renderable(
                &gl,
                program,
                u_mvp.as_ref(),
                u_use_tex.as_ref(),
                u_tex.as_ref(),
                a_pos,
                a_uv,
                &terrain_gpu,
                &terrain_mvp,
            );

            for model in &model_gpus {
                for instance in &model.instances {
                    let model_m = model_matrix(instance.position, instance.yaw_rad, instance.scale);
                    let view_model = mat4_mul(&view, &model_m);
                    let mvp = mat4_mul(&proj, &view_model);
                    draw_gpu_renderable(
                        &gl,
                        program,
                        u_mvp.as_ref(),
                        u_use_tex.as_ref(),
                        u_tex.as_ref(),
                        a_pos,
                        a_uv,
                        &model.gpu,
                        &mvp,
                    );
                }
            }
        }

        window.gl_swap_window();

        fps_frames = fps_frames.saturating_add(1);
        let elapsed = fps_window_start.elapsed();
        if elapsed >= Duration::from_millis(500) {
            let fps = fps_frames as f32 / elapsed.as_secs_f32().max(0.000_1);
            let frame_time_ms = 1000.0 / fps.max(0.000_1);
            let _ = window.set_title(&format!(
                "Parkan Mission Demo | FPS: {fps:.1} ({frame_time_ms:.2} ms) | objects: {instance_count}"
            ));
            print!("\rFPS: {fps:.1} ({frame_time_ms:.2} ms)");
            let _ = std::io::stdout().flush();
            fps_printed = true;
            fps_frames = 0;
            fps_window_start = Instant::now();
        }
    }

    if fps_printed {
        println!();
    }

    unsafe {
        cleanup_renderable(&gl, terrain_gpu);
        for model in model_gpus {
            cleanup_renderable(&gl, model.gpu);
        }
        gl.delete_program(program);
    }

    Ok(())
}

fn initial_scene_sphere(scene: &MissionScene) -> ([f32; 3], f32) {
    if let Some((min_v, max_v)) = compute_scene_bounds(scene) {
        let center = [
            0.5 * (min_v[0] + max_v[0]),
            0.5 * (min_v[1] + max_v[1]),
            0.5 * (min_v[2] + max_v[2]),
        ];
        let extent = [
            max_v[0] - min_v[0],
            max_v[1] - min_v[1],
            max_v[2] - min_v[2],
        ];
        let radius = ((extent[0] * extent[0]) + (extent[1] * extent[1]) + (extent[2] * extent[2]))
            .sqrt()
            .max(10.0)
            * 0.5;
        return (center, radius);
    }
    ([0.0, 0.0, 0.0], 100.0)
}

fn update_camera(events: &sdl2::EventPump, camera: &mut Camera, dt: f32) {
    use sdl2::keyboard::Scancode;

    let keys = events.keyboard_state();
    let mut move_dir = [0.0f32, 0.0f32, 0.0f32];

    let forward = camera_forward(camera.yaw, camera.pitch);
    let right = normalize3(cross3(forward, [0.0, 1.0, 0.0]));

    if keys.is_scancode_pressed(Scancode::Up) || keys.is_scancode_pressed(Scancode::W) {
        move_dir[0] += forward[0];
        move_dir[1] += forward[1];
        move_dir[2] += forward[2];
    }
    if keys.is_scancode_pressed(Scancode::Down) || keys.is_scancode_pressed(Scancode::S) {
        move_dir[0] -= forward[0];
        move_dir[1] -= forward[1];
        move_dir[2] -= forward[2];
    }
    if keys.is_scancode_pressed(Scancode::Left) || keys.is_scancode_pressed(Scancode::A) {
        move_dir[0] -= right[0];
        move_dir[1] -= right[1];
        move_dir[2] -= right[2];
    }
    if keys.is_scancode_pressed(Scancode::Right) || keys.is_scancode_pressed(Scancode::D) {
        move_dir[0] += right[0];
        move_dir[1] += right[1];
        move_dir[2] += right[2];
    }
    if keys.is_scancode_pressed(Scancode::PageUp) || keys.is_scancode_pressed(Scancode::E) {
        move_dir[1] += 1.0;
    }
    if keys.is_scancode_pressed(Scancode::PageDown) || keys.is_scancode_pressed(Scancode::Q) {
        move_dir[1] -= 1.0;
    }

    let shift =
        keys.is_scancode_pressed(Scancode::LShift) || keys.is_scancode_pressed(Scancode::RShift);
    let speed_mul = if shift { 3.0 } else { 1.0 };

    let norm = normalize3(move_dir);
    camera.position[0] += norm[0] * camera.move_speed * speed_mul * dt;
    camera.position[1] += norm[1] * camera.move_speed * speed_mul * dt;
    camera.position[2] += norm[2] * camera.move_speed * speed_mul * dt;
}

unsafe fn upload_model_renderable(
    gl: &glow::Context,
    model: &render_mission_demo::SceneModel,
) -> Result<ModelRenderable, String> {
    let mut vertex_data = Vec::with_capacity(model.mesh.vertices.len() * 5);
    for vertex in &model.mesh.vertices {
        vertex_data.push(vertex.position[0]);
        vertex_data.push(vertex.position[1]);
        vertex_data.push(vertex.position[2]);
        vertex_data.push(vertex.uv0[0]);
        vertex_data.push(vertex.uv0[1]);
    }

    let gpu = upload_gpu_renderable(
        gl,
        &vertex_data,
        &model.mesh.indices,
        model.texture.as_ref(),
    )?;

    Ok(ModelRenderable {
        gpu,
        instances: model.instances.clone(),
    })
}

unsafe fn upload_terrain_renderable(
    gl: &glow::Context,
    mesh: &terrain_core::TerrainRenderMesh,
    texture: Option<&render_demo::LoadedTexture>,
) -> Result<GpuRenderable, String> {
    let mut vertex_data = Vec::with_capacity(mesh.vertices.len() * 5);
    for vertex in &mesh.vertices {
        vertex_data.push(vertex.position[0]);
        vertex_data.push(vertex.position[1]);
        vertex_data.push(vertex.position[2]);
        vertex_data.push(vertex.uv0[0]);
        vertex_data.push(vertex.uv0[1]);
    }

    upload_gpu_renderable(gl, &vertex_data, &mesh.indices, texture)
}

unsafe fn upload_gpu_renderable(
    gl: &glow::Context,
    vertices: &[f32],
    indices: &[u16],
    texture: Option<&render_demo::LoadedTexture>,
) -> Result<GpuRenderable, String> {
    let vbo = gl.create_buffer().map_err(|e| e.to_string())?;
    let ebo = gl.create_buffer().map_err(|e| e.to_string())?;

    let vertex_bytes = f32_slice_to_ne_bytes(vertices);
    let index_bytes = u16_slice_to_ne_bytes(indices);

    gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
    gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, &vertex_bytes, glow::STATIC_DRAW);
    gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(ebo));
    gl.buffer_data_u8_slice(glow::ELEMENT_ARRAY_BUFFER, &index_bytes, glow::STATIC_DRAW);
    gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, None);
    gl.bind_buffer(glow::ARRAY_BUFFER, None);

    let gpu_texture = if let Some(texture) = texture {
        Some(create_texture(gl, texture)?)
    } else {
        None
    };

    Ok(GpuRenderable {
        vbo,
        ebo,
        index_count: indices.len(),
        texture: gpu_texture,
    })
}

unsafe fn cleanup_renderable(gl: &glow::Context, renderable: GpuRenderable) {
    if let Some(tex) = renderable.texture {
        gl.delete_texture(tex.handle);
    }
    gl.delete_buffer(renderable.ebo);
    gl.delete_buffer(renderable.vbo);
}

unsafe fn draw_frame_begin(gl: &glow::Context, width: u32, height: u32) {
    gl.viewport(
        0,
        0,
        width.min(i32::MAX as u32) as i32,
        height.min(i32::MAX as u32) as i32,
    );
    gl.enable(glow::DEPTH_TEST);
    gl.clear_color(0.06, 0.08, 0.12, 1.0);
    gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
}

unsafe fn draw_gpu_renderable(
    gl: &glow::Context,
    program: glow::NativeProgram,
    u_mvp: Option<&glow::NativeUniformLocation>,
    u_use_tex: Option<&glow::NativeUniformLocation>,
    u_tex: Option<&glow::NativeUniformLocation>,
    a_pos: u32,
    a_uv: u32,
    renderable: &GpuRenderable,
    mvp: &[f32; 16],
) {
    gl.use_program(Some(program));
    gl.uniform_matrix_4_f32_slice(u_mvp, false, mvp);

    let texture_enabled = renderable.texture.is_some();
    gl.uniform_1_f32(u_use_tex, if texture_enabled { 1.0 } else { 0.0 });

    if let Some(tex) = &renderable.texture {
        gl.active_texture(glow::TEXTURE0);
        gl.bind_texture(glow::TEXTURE_2D, Some(tex.handle));
        gl.uniform_1_i32(u_tex, 0);
    } else {
        gl.bind_texture(glow::TEXTURE_2D, None);
    }

    gl.bind_buffer(glow::ARRAY_BUFFER, Some(renderable.vbo));
    gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(renderable.ebo));
    gl.enable_vertex_attrib_array(a_pos);
    gl.vertex_attrib_pointer_f32(a_pos, 3, glow::FLOAT, false, 20, 0);
    gl.enable_vertex_attrib_array(a_uv);
    gl.vertex_attrib_pointer_f32(a_uv, 2, glow::FLOAT, false, 20, 12);

    gl.draw_elements(
        glow::TRIANGLES,
        renderable.index_count.min(i32::MAX as usize) as i32,
        glow::UNSIGNED_SHORT,
        0,
    );

    gl.disable_vertex_attrib_array(a_uv);
    gl.disable_vertex_attrib_array(a_pos);
    gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, None);
    gl.bind_buffer(glow::ARRAY_BUFFER, None);
    gl.bind_texture(glow::TEXTURE_2D, None);
    gl.use_program(None);
}

fn create_window_and_context(
    video: &sdl2::VideoSubsystem,
    width: u32,
    height: u32,
) -> Result<(sdl2::video::Window, sdl2::video::GLContext, GlBackend), String> {
    let candidates = [
        (GlBackend::Gles2, sdl2::video::GLProfile::GLES, 2, 0),
        (GlBackend::Core33, sdl2::video::GLProfile::Core, 3, 3),
    ];
    let mut errors = Vec::new();

    for (backend, profile, major, minor) in candidates {
        {
            let gl_attr = video.gl_attr();
            gl_attr.set_context_profile(profile);
            gl_attr.set_context_version(major, minor);
            gl_attr.set_depth_size(24);
            gl_attr.set_double_buffer(true);
        }

        let mut window_builder = video.window("Parkan Mission Demo", width, height);
        window_builder.opengl().resizable();

        let window = match window_builder.build() {
            Ok(window) => window,
            Err(err) => {
                errors.push(format!(
                    "{profile:?} {major}.{minor}: window build failed ({err})"
                ));
                continue;
            }
        };

        let gl_ctx = match window.gl_create_context() {
            Ok(ctx) => ctx,
            Err(err) => {
                errors.push(format!(
                    "{profile:?} {major}.{minor}: context create failed ({err})"
                ));
                continue;
            }
        };

        if let Err(err) = window.gl_make_current(&gl_ctx) {
            errors.push(format!(
                "{profile:?} {major}.{minor}: make current failed ({err})"
            ));
            continue;
        }

        return Ok((window, gl_ctx, backend));
    }

    Err(format!(
        "failed to create OpenGL context. Attempts: {}",
        errors.join(" | ")
    ))
}

unsafe fn create_texture(
    gl: &glow::Context,
    texture: &render_demo::LoadedTexture,
) -> Result<GpuTexture, String> {
    let handle = gl.create_texture().map_err(|e| e.to_string())?;
    gl.bind_texture(glow::TEXTURE_2D, Some(handle));
    gl.tex_parameter_i32(
        glow::TEXTURE_2D,
        glow::TEXTURE_MIN_FILTER,
        glow::LINEAR as i32,
    );
    gl.tex_parameter_i32(
        glow::TEXTURE_2D,
        glow::TEXTURE_MAG_FILTER,
        glow::LINEAR as i32,
    );
    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::REPEAT as i32);
    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::REPEAT as i32);
    gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
    gl.tex_image_2d(
        glow::TEXTURE_2D,
        0,
        glow::RGBA as i32,
        texture.width.min(i32::MAX as u32) as i32,
        texture.height.min(i32::MAX as u32) as i32,
        0,
        glow::RGBA,
        glow::UNSIGNED_BYTE,
        glow::PixelUnpackData::Slice(Some(texture.rgba8.as_slice())),
    );
    gl.bind_texture(glow::TEXTURE_2D, None);
    Ok(GpuTexture { handle })
}

unsafe fn create_program(
    gl: &glow::Context,
    backend: GlBackend,
) -> Result<glow::NativeProgram, String> {
    let (vs_src, fs_src) = match backend {
        GlBackend::Gles2 => (
            r#"
attribute vec3 a_pos;
attribute vec2 a_uv;
uniform mat4 u_mvp;
varying vec2 v_uv;
void main() {
    v_uv = a_uv;
    gl_Position = u_mvp * vec4(a_pos, 1.0);
}
"#,
            r#"
precision mediump float;
uniform sampler2D u_tex;
uniform float u_use_tex;
varying vec2 v_uv;
void main() {
    vec4 base = vec4(0.82, 0.87, 0.95, 1.0);
    vec4 texColor = texture2D(u_tex, v_uv);
    gl_FragColor = mix(base, texColor, u_use_tex);
}
"#,
        ),
        GlBackend::Core33 => (
            r#"#version 330 core
in vec3 a_pos;
in vec2 a_uv;
uniform mat4 u_mvp;
out vec2 v_uv;
void main() {
    v_uv = a_uv;
    gl_Position = u_mvp * vec4(a_pos, 1.0);
}
"#,
            r#"#version 330 core
uniform sampler2D u_tex;
uniform float u_use_tex;
in vec2 v_uv;
out vec4 fragColor;
void main() {
    vec4 base = vec4(0.82, 0.87, 0.95, 1.0);
    vec4 texColor = texture(u_tex, v_uv);
    fragColor = mix(base, texColor, u_use_tex);
}
"#,
        ),
    };

    let program = gl.create_program().map_err(|e| e.to_string())?;
    let vs = gl
        .create_shader(glow::VERTEX_SHADER)
        .map_err(|e| e.to_string())?;
    let fs = gl
        .create_shader(glow::FRAGMENT_SHADER)
        .map_err(|e| e.to_string())?;

    gl.shader_source(vs, vs_src);
    gl.compile_shader(vs);
    if !gl.get_shader_compile_status(vs) {
        let log = gl.get_shader_info_log(vs);
        gl.delete_shader(vs);
        gl.delete_shader(fs);
        gl.delete_program(program);
        return Err(format!("vertex shader compile failed: {log}"));
    }

    gl.shader_source(fs, fs_src);
    gl.compile_shader(fs);
    if !gl.get_shader_compile_status(fs) {
        let log = gl.get_shader_info_log(fs);
        gl.delete_shader(vs);
        gl.delete_shader(fs);
        gl.delete_program(program);
        return Err(format!("fragment shader compile failed: {log}"));
    }

    gl.attach_shader(program, vs);
    gl.attach_shader(program, fs);
    gl.link_program(program);

    gl.detach_shader(program, vs);
    gl.detach_shader(program, fs);
    gl.delete_shader(vs);
    gl.delete_shader(fs);

    if !gl.get_program_link_status(program) {
        let log = gl.get_program_info_log(program);
        gl.delete_program(program);
        return Err(format!("program link failed: {log}"));
    }

    Ok(program)
}

fn model_matrix(position: [f32; 3], yaw: f32, scale: [f32; 3]) -> [f32; 16] {
    let translation = mat4_translation(position[0], position[1], position[2]);
    let rotation = mat4_rotation_y(yaw);
    let scaling = mat4_scale(scale[0], scale[1], scale[2]);
    let tr = mat4_mul(&translation, &rotation);
    mat4_mul(&tr, &scaling)
}

fn camera_forward(yaw: f32, pitch: f32) -> [f32; 3] {
    let cp = pitch.cos();
    normalize3([yaw.sin() * cp, pitch.sin(), yaw.cos() * cp])
}

fn cross3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len <= 1e-6 {
        [0.0, 0.0, 0.0]
    } else {
        [v[0] / len, v[1] / len, v[2] / len]
    }
}

fn mat4_identity() -> [f32; 16] {
    [
        1.0, 0.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, //
        0.0, 0.0, 0.0, 1.0, //
    ]
}

fn mat4_translation(x: f32, y: f32, z: f32) -> [f32; 16] {
    let mut m = mat4_identity();
    m[12] = x;
    m[13] = y;
    m[14] = z;
    m
}

fn mat4_scale(x: f32, y: f32, z: f32) -> [f32; 16] {
    [
        x, 0.0, 0.0, 0.0, //
        0.0, y, 0.0, 0.0, //
        0.0, 0.0, z, 0.0, //
        0.0, 0.0, 0.0, 1.0, //
    ]
}

fn mat4_rotation_y(rad: f32) -> [f32; 16] {
    let c = rad.cos();
    let s = rad.sin();
    [
        c, 0.0, -s, 0.0, //
        0.0, 1.0, 0.0, 0.0, //
        s, 0.0, c, 0.0, //
        0.0, 0.0, 0.0, 1.0, //
    ]
}

fn mat4_perspective(fovy: f32, aspect: f32, near: f32, far: f32) -> [f32; 16] {
    let f = 1.0 / (0.5 * fovy).tan();
    let nf = 1.0 / (near - far);
    [
        f / aspect,
        0.0,
        0.0,
        0.0,
        0.0,
        f,
        0.0,
        0.0,
        0.0,
        0.0,
        (far + near) * nf,
        -1.0,
        0.0,
        0.0,
        (2.0 * far * near) * nf,
        0.0,
    ]
}

fn mat4_look_at(eye: [f32; 3], target: [f32; 3], up: [f32; 3]) -> [f32; 16] {
    let f = normalize3([target[0] - eye[0], target[1] - eye[1], target[2] - eye[2]]);
    let s = normalize3(cross3(f, up));
    let u = cross3(s, f);

    [
        s[0],
        u[0],
        -f[0],
        0.0,
        s[1],
        u[1],
        -f[1],
        0.0,
        s[2],
        u[2],
        -f[2],
        0.0,
        -dot3(s, eye),
        -dot3(u, eye),
        dot3(f, eye),
        1.0,
    ]
}

fn mat4_mul(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    let mut out = [0.0f32; 16];
    for c in 0..4 {
        for r in 0..4 {
            let mut acc = 0.0f32;
            for k in 0..4 {
                acc += a[k * 4 + r] * b[c * 4 + k];
            }
            out[c * 4 + r] = acc;
        }
    }
    out
}

fn f32_slice_to_ne_bytes(slice: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(slice.len().saturating_mul(std::mem::size_of::<f32>()));
    for &value in slice {
        out.extend_from_slice(&value.to_ne_bytes());
    }
    out
}

fn u16_slice_to_ne_bytes(slice: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(slice.len().saturating_mul(std::mem::size_of::<u16>()));
    for &value in slice {
        out.extend_from_slice(&value.to_ne_bytes());
    }
    out
}
