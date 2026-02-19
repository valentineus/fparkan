use glow::HasContext as _;
use render_core::{build_render_mesh, compute_bounds_for_mesh};
use render_demo::{load_model_with_name_from_archive, resolve_texture_for_model, LoadedTexture};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

struct Args {
    archive: PathBuf,
    model: Option<String>,
    lod: usize,
    group: usize,
    width: u32,
    height: u32,
    fov_deg: f32,
    capture: Option<PathBuf>,
    angle: Option<f32>,
    spin_rate: f32,
    texture: Option<String>,
    texture_archive: Option<PathBuf>,
    material_archive: Option<PathBuf>,
    wear: Option<String>,
    no_texture: bool,
}

struct GpuTexture {
    handle: glow::NativeTexture,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum GlBackend {
    Gles2,
    Core33,
}

fn parse_args() -> Result<Args, String> {
    let mut archive = None;
    let mut model = None;
    let mut lod = 0usize;
    let mut group = 0usize;
    let mut width = 1280u32;
    let mut height = 720u32;
    let mut fov_deg = 60.0f32;
    let mut capture = None;
    let mut angle = None;
    let mut spin_rate = 0.35f32;
    let mut texture = None;
    let mut texture_archive = None;
    let mut material_archive = None;
    let mut wear = None;
    let mut no_texture = false;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--archive" => {
                let value = it
                    .next()
                    .ok_or_else(|| String::from("missing value for --archive"))?;
                archive = Some(PathBuf::from(value));
            }
            "--model" => {
                let value = it
                    .next()
                    .ok_or_else(|| String::from("missing value for --model"))?;
                model = Some(value);
            }
            "--lod" => {
                let value = it
                    .next()
                    .ok_or_else(|| String::from("missing value for --lod"))?;
                lod = value
                    .parse::<usize>()
                    .map_err(|_| String::from("invalid --lod value"))?;
            }
            "--group" => {
                let value = it
                    .next()
                    .ok_or_else(|| String::from("missing value for --group"))?;
                group = value
                    .parse::<usize>()
                    .map_err(|_| String::from("invalid --group value"))?;
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
            "--capture" => {
                let value = it
                    .next()
                    .ok_or_else(|| String::from("missing value for --capture"))?;
                capture = Some(PathBuf::from(value));
            }
            "--angle" => {
                let value = it
                    .next()
                    .ok_or_else(|| String::from("missing value for --angle"))?;
                angle = Some(
                    value
                        .parse::<f32>()
                        .map_err(|_| String::from("invalid --angle value"))?,
                );
            }
            "--spin-rate" => {
                let value = it
                    .next()
                    .ok_or_else(|| String::from("missing value for --spin-rate"))?;
                spin_rate = value
                    .parse::<f32>()
                    .map_err(|_| String::from("invalid --spin-rate value"))?;
            }
            "--texture" => {
                let value = it
                    .next()
                    .ok_or_else(|| String::from("missing value for --texture"))?;
                texture = Some(value);
            }
            "--texture-archive" => {
                let value = it
                    .next()
                    .ok_or_else(|| String::from("missing value for --texture-archive"))?;
                texture_archive = Some(PathBuf::from(value));
            }
            "--material-archive" => {
                let value = it
                    .next()
                    .ok_or_else(|| String::from("missing value for --material-archive"))?;
                material_archive = Some(PathBuf::from(value));
            }
            "--wear" => {
                let value = it
                    .next()
                    .ok_or_else(|| String::from("missing value for --wear"))?;
                wear = Some(value);
            }
            "--no-texture" => {
                no_texture = true;
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

    let archive = archive.ok_or_else(|| String::from("missing required --archive"))?;
    Ok(Args {
        archive,
        model,
        lod,
        group,
        width,
        height,
        fov_deg,
        capture,
        angle,
        spin_rate,
        texture,
        texture_archive,
        material_archive,
        wear,
        no_texture,
    })
}

fn print_help() {
    eprintln!(
        "parkan-render-demo --archive <path> [--model <name.msh>] [--lod N] [--group N] [--width W] [--height H] [--fov DEG]"
    );
    eprintln!("                  [--capture <out.png>] [--angle RAD] [--spin-rate RAD_PER_SEC]");
    eprintln!("                  [--texture <name>] [--texture-archive <path>] [--material-archive <path>] [--wear <name.wea>] [--no-texture]");
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
    let loaded_model = load_model_with_name_from_archive(&args.archive, args.model.as_deref())
        .map_err(|err| {
            format!(
                "failed to load model from archive {}: {err}",
                args.archive.display()
            )
        })?;
    let mesh = build_render_mesh(&loaded_model.model, args.lod, args.group);
    if mesh.indices.is_empty() {
        return Err(format!(
            "model has no renderable triangles for lod={} group={}",
            args.lod, args.group
        ));
    }
    if mesh.index_overflow {
        eprintln!(
            "warning: mesh exceeds u16 index space and may be partially rendered on GLES2 targets"
        );
    }
    let Some((bounds_min, bounds_max)) = compute_bounds_for_mesh(&mesh.vertices) else {
        return Err(String::from("failed to compute mesh bounds"));
    };

    let resolved_texture = resolve_texture(&args, &loaded_model.name)?;
    if let Some(tex) = resolved_texture.as_ref() {
        println!(
            "resolved texture '{}' ({}x{})",
            tex.name, tex.width, tex.height
        );
    } else {
        println!("texture path disabled or unresolved; rendering with fallback color");
    }

    let center = [
        0.5 * (bounds_min[0] + bounds_max[0]),
        0.5 * (bounds_min[1] + bounds_max[1]),
        0.5 * (bounds_min[2] + bounds_max[2]),
    ];
    let extent = [
        bounds_max[0] - bounds_min[0],
        bounds_max[1] - bounds_min[1],
        bounds_max[2] - bounds_min[2],
    ];
    let radius =
        (extent[0] * extent[0] + extent[1] * extent[1] + extent[2] * extent[2]).sqrt() * 0.5;
    let camera_distance = (radius * 2.5).max(2.0);

    let sdl = sdl2::init().map_err(|err| format!("failed to init SDL2: {err}"))?;
    let video = sdl
        .video()
        .map_err(|err| format!("failed to init SDL2 video: {err}"))?;

    let (mut window, _gl_ctx, gl_backend) = create_window_and_context(&video, &args)?;
    let _ = if args.capture.is_some() {
        video.gl_set_swap_interval(0)
    } else {
        video.gl_set_swap_interval(1)
    };

    let mut vertex_data = Vec::with_capacity(mesh.vertices.len() * 5);
    for vertex in &mesh.vertices {
        vertex_data.push(vertex.position[0]);
        vertex_data.push(vertex.position[1]);
        vertex_data.push(vertex.position[2]);
        vertex_data.push(vertex.uv0[0]);
        vertex_data.push(vertex.uv0[1]);
    }
    let vertex_bytes = f32_slice_to_ne_bytes(&vertex_data);
    let index_bytes = u16_slice_to_ne_bytes(&mesh.indices);

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

    let vbo = unsafe { gl.create_buffer().map_err(|e| e.to_string())? };
    let ebo = unsafe { gl.create_buffer().map_err(|e| e.to_string())? };
    unsafe {
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, &vertex_bytes, glow::STATIC_DRAW);
        gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(ebo));
        gl.buffer_data_u8_slice(glow::ELEMENT_ARRAY_BUFFER, &index_bytes, glow::STATIC_DRAW);
        gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, None);
        gl.bind_buffer(glow::ARRAY_BUFFER, None);
    }
    let vao = unsafe { create_vertex_layout_if_needed(&gl, gl_backend, vbo, ebo, a_pos, a_uv)? };

    let gpu_texture = if let Some(texture) = resolved_texture.as_ref() {
        Some(unsafe { create_texture(&gl, texture)? })
    } else {
        None
    };

    let result = if let Some(capture_path) = args.capture.as_ref() {
        run_capture(
            &gl,
            program,
            u_mvp.as_ref(),
            u_use_tex.as_ref(),
            u_tex.as_ref(),
            a_pos,
            a_uv,
            vbo,
            ebo,
            vao,
            gpu_texture.as_ref(),
            mesh.indices.len(),
            &args,
            center,
            camera_distance,
            capture_path,
        )
    } else {
        run_interactive(
            &sdl,
            &mut window,
            &gl,
            program,
            u_mvp.as_ref(),
            u_use_tex.as_ref(),
            u_tex.as_ref(),
            a_pos,
            a_uv,
            vbo,
            ebo,
            vao,
            gpu_texture.as_ref(),
            mesh.indices.len(),
            &args,
            center,
            camera_distance,
        )
    };

    unsafe {
        if let Some(texture) = gpu_texture {
            gl.delete_texture(texture.handle);
        }
        if let Some(vao) = vao {
            gl.delete_vertex_array(vao);
        }
        gl.delete_buffer(ebo);
        gl.delete_buffer(vbo);
        gl.delete_program(program);
    }

    result
}

fn create_window_and_context(
    video: &sdl2::VideoSubsystem,
    args: &Args,
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

        let mut window_builder = video.window(
            "Parkan Render Demo (SDL2 + OpenGL)",
            args.width,
            args.height,
        );
        window_builder.opengl();
        if args.capture.is_some() {
            window_builder.hidden();
        } else {
            window_builder.resizable();
        }

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

unsafe fn create_vertex_layout_if_needed(
    gl: &glow::Context,
    backend: GlBackend,
    vbo: glow::NativeBuffer,
    ebo: glow::NativeBuffer,
    a_pos: u32,
    a_uv: u32,
) -> Result<Option<glow::NativeVertexArray>, String> {
    if backend != GlBackend::Core33 {
        return Ok(None);
    }

    let vao = gl.create_vertex_array().map_err(|e| e.to_string())?;
    gl.bind_vertex_array(Some(vao));
    gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
    gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(ebo));
    gl.enable_vertex_attrib_array(a_pos);
    gl.vertex_attrib_pointer_f32(a_pos, 3, glow::FLOAT, false, 20, 0);
    gl.enable_vertex_attrib_array(a_uv);
    gl.vertex_attrib_pointer_f32(a_uv, 2, glow::FLOAT, false, 20, 12);
    gl.bind_vertex_array(None);
    Ok(Some(vao))
}

fn resolve_texture(args: &Args, model_name: &str) -> Result<Option<LoadedTexture>, String> {
    if args.no_texture {
        return Ok(None);
    }

    match resolve_texture_for_model(
        &args.archive,
        model_name,
        args.texture.as_deref(),
        args.texture_archive.as_deref(),
        args.material_archive.as_deref(),
        args.wear.as_deref(),
    ) {
        Ok(texture) => Ok(texture),
        Err(err) => {
            if args.texture.is_some()
                || args.texture_archive.is_some()
                || args.material_archive.is_some()
                || args.wear.is_some()
            {
                Err(format!("failed to resolve texture: {err}"))
            } else {
                eprintln!("warning: auto texture resolve failed ({err}), fallback to solid color");
                Ok(None)
            }
        }
    }
}

unsafe fn create_texture(
    gl: &glow::Context,
    texture: &LoadedTexture,
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

#[allow(clippy::too_many_arguments)]
fn run_capture(
    gl: &glow::Context,
    program: glow::NativeProgram,
    u_mvp: Option<&glow::NativeUniformLocation>,
    u_use_tex: Option<&glow::NativeUniformLocation>,
    u_tex: Option<&glow::NativeUniformLocation>,
    a_pos: u32,
    a_uv: u32,
    vbo: glow::NativeBuffer,
    ebo: glow::NativeBuffer,
    vao: Option<glow::NativeVertexArray>,
    texture: Option<&GpuTexture>,
    index_count: usize,
    args: &Args,
    center: [f32; 3],
    camera_distance: f32,
    capture_path: &Path,
) -> Result<(), String> {
    let angle = args.angle.unwrap_or(0.0);
    let mvp = compute_mvp(
        args.width,
        args.height,
        args.fov_deg,
        center,
        camera_distance,
        angle,
    );
    unsafe {
        draw_frame(
            gl,
            program,
            u_mvp,
            u_use_tex,
            u_tex,
            a_pos,
            a_uv,
            vbo,
            ebo,
            vao,
            texture,
            index_count,
            args.width,
            args.height,
            &mvp,
        );
    }
    let mut rgba = unsafe { read_pixels_rgba(gl, args.width, args.height)? };
    flip_image_y_rgba(&mut rgba, args.width as usize, args.height as usize);
    save_png(capture_path, args.width, args.height, rgba)?;
    println!("captured frame to {}", capture_path.display());
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_interactive(
    sdl: &sdl2::Sdl,
    window: &mut sdl2::video::Window,
    gl: &glow::Context,
    program: glow::NativeProgram,
    u_mvp: Option<&glow::NativeUniformLocation>,
    u_use_tex: Option<&glow::NativeUniformLocation>,
    u_tex: Option<&glow::NativeUniformLocation>,
    a_pos: u32,
    a_uv: u32,
    vbo: glow::NativeBuffer,
    ebo: glow::NativeBuffer,
    vao: Option<glow::NativeVertexArray>,
    texture: Option<&GpuTexture>,
    index_count: usize,
    args: &Args,
    center: [f32; 3],
    camera_distance: f32,
) -> Result<(), String> {
    let mut events = sdl
        .event_pump()
        .map_err(|err| format!("failed to get SDL event pump: {err}"))?;
    let start = Instant::now();
    let mut fps_window_start = Instant::now();
    let mut fps_frames: u32 = 0;
    let mut fps_printed = false;
    let base_title = "Parkan Render Demo (SDL2 + OpenGL)";

    'main_loop: loop {
        for event in events.poll_iter() {
            match event {
                sdl2::event::Event::Quit { .. } => break 'main_loop,
                sdl2::event::Event::KeyDown {
                    keycode: Some(sdl2::keyboard::Keycode::Escape),
                    ..
                } => break 'main_loop,
                _ => {}
            }
        }

        let (w, h) = window.size();
        let angle = args
            .angle
            .unwrap_or(start.elapsed().as_secs_f32() * args.spin_rate);
        let mvp = compute_mvp(w, h, args.fov_deg, center, camera_distance, angle);

        unsafe {
            draw_frame(
                gl,
                program,
                u_mvp,
                u_use_tex,
                u_tex,
                a_pos,
                a_uv,
                vbo,
                ebo,
                vao,
                texture,
                index_count,
                w,
                h,
                &mvp,
            );
        }
        window.gl_swap_window();

        fps_frames = fps_frames.saturating_add(1);
        let elapsed = fps_window_start.elapsed();
        if elapsed >= Duration::from_millis(500) {
            let fps = fps_frames as f32 / elapsed.as_secs_f32().max(0.000_1);
            let frame_time_ms = 1000.0 / fps.max(0.000_1);
            let _ = window.set_title(&format!(
                "{base_title} | FPS: {fps:.1} ({frame_time_ms:.2} ms)"
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

    Ok(())
}

fn compute_mvp(
    width: u32,
    height: u32,
    fov_deg: f32,
    center: [f32; 3],
    camera_distance: f32,
    angle_rad: f32,
) -> [f32; 16] {
    let aspect = (width as f32 / (height.max(1) as f32)).max(0.01);
    let proj = mat4_perspective(fov_deg.to_radians(), aspect, 0.01, camera_distance * 10.0);
    let view = mat4_translation(0.0, 0.0, -camera_distance);
    let center_shift = mat4_translation(-center[0], -center[1], -center[2]);
    let rot = mat4_rotation_y(angle_rad);
    let model_m = mat4_mul(&rot, &center_shift);
    let vp = mat4_mul(&view, &model_m);
    mat4_mul(&proj, &vp)
}

#[allow(clippy::too_many_arguments)]
unsafe fn draw_frame(
    gl: &glow::Context,
    program: glow::NativeProgram,
    u_mvp: Option<&glow::NativeUniformLocation>,
    u_use_tex: Option<&glow::NativeUniformLocation>,
    u_tex: Option<&glow::NativeUniformLocation>,
    a_pos: u32,
    a_uv: u32,
    vbo: glow::NativeBuffer,
    ebo: glow::NativeBuffer,
    vao: Option<glow::NativeVertexArray>,
    texture: Option<&GpuTexture>,
    index_count: usize,
    width: u32,
    height: u32,
    mvp: &[f32; 16],
) {
    gl.viewport(
        0,
        0,
        width.min(i32::MAX as u32) as i32,
        height.min(i32::MAX as u32) as i32,
    );
    gl.enable(glow::DEPTH_TEST);
    gl.clear_color(0.06, 0.08, 0.12, 1.0);
    gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);

    gl.use_program(Some(program));
    gl.uniform_matrix_4_f32_slice(u_mvp, false, mvp);

    let texture_enabled = texture.is_some();
    gl.uniform_1_f32(u_use_tex, if texture_enabled { 1.0 } else { 0.0 });
    if let Some(tex) = texture {
        gl.active_texture(glow::TEXTURE0);
        gl.bind_texture(glow::TEXTURE_2D, Some(tex.handle));
        gl.uniform_1_i32(u_tex, 0);
    } else {
        gl.bind_texture(glow::TEXTURE_2D, None);
    }

    if let Some(vao) = vao {
        gl.bind_vertex_array(Some(vao));
        gl.draw_elements(
            glow::TRIANGLES,
            index_count.min(i32::MAX as usize) as i32,
            glow::UNSIGNED_SHORT,
            0,
        );
        gl.bind_vertex_array(None);
    } else {
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
        gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(ebo));
        gl.enable_vertex_attrib_array(a_pos);
        gl.vertex_attrib_pointer_f32(a_pos, 3, glow::FLOAT, false, 20, 0);
        gl.enable_vertex_attrib_array(a_uv);
        gl.vertex_attrib_pointer_f32(a_uv, 2, glow::FLOAT, false, 20, 12);
        gl.draw_elements(
            glow::TRIANGLES,
            index_count.min(i32::MAX as usize) as i32,
            glow::UNSIGNED_SHORT,
            0,
        );
        gl.disable_vertex_attrib_array(a_uv);
        gl.disable_vertex_attrib_array(a_pos);
        gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, None);
        gl.bind_buffer(glow::ARRAY_BUFFER, None);
    }
    gl.bind_texture(glow::TEXTURE_2D, None);
    gl.use_program(None);
}

unsafe fn read_pixels_rgba(gl: &glow::Context, width: u32, height: u32) -> Result<Vec<u8>, String> {
    let pixel_count = usize::try_from(width)
        .ok()
        .and_then(|w| usize::try_from(height).ok().map(|h| w.saturating_mul(h)))
        .ok_or_else(|| String::from("frame dimensions are too large"))?;
    let mut pixels = vec![0u8; pixel_count.saturating_mul(4)];
    gl.read_pixels(
        0,
        0,
        width.min(i32::MAX as u32) as i32,
        height.min(i32::MAX as u32) as i32,
        glow::RGBA,
        glow::UNSIGNED_BYTE,
        glow::PixelPackData::Slice(Some(pixels.as_mut_slice())),
    );
    Ok(pixels)
}

fn flip_image_y_rgba(rgba: &mut [u8], width: usize, height: usize) {
    let stride = width.saturating_mul(4);
    if stride == 0 {
        return;
    }
    for y in 0..(height / 2) {
        let top = y * stride;
        let bottom = (height - 1 - y) * stride;
        for i in 0..stride {
            rgba.swap(top + i, bottom + i);
        }
    }
}

fn save_png(path: &Path, width: u32, height: u32, rgba: Vec<u8>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|err| {
                format!(
                    "failed to create output directory {}: {err}",
                    parent.display()
                )
            })?;
        }
    }
    let image = image::RgbaImage::from_raw(width, height, rgba)
        .ok_or_else(|| String::from("failed to build image from framebuffer bytes"))?;
    image
        .save(path)
        .map_err(|err| format!("failed to save PNG {}: {err}", path.display()))
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
    vec4 base = vec4(0.85, 0.90, 1.00, 1.0);
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
    vec4 base = vec4(0.85, 0.90, 1.00, 1.0);
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
