use glow::HasContext as _;
use render_core::{build_render_mesh, compute_bounds};
use render_demo::load_model_from_archive;
use std::path::{Path, PathBuf};
use std::time::Instant;

struct Args {
    archive: PathBuf,
    model: Option<String>,
    lod: usize,
    group: usize,
    width: u32,
    height: u32,
    capture: Option<PathBuf>,
    angle: Option<f32>,
    spin_rate: f32,
}

fn parse_args() -> Result<Args, String> {
    let mut archive = None;
    let mut model = None;
    let mut lod = 0usize;
    let mut group = 0usize;
    let mut width = 1280u32;
    let mut height = 720u32;
    let mut capture = None;
    let mut angle = None;
    let mut spin_rate = 0.35f32;

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
        capture,
        angle,
        spin_rate,
    })
}

fn print_help() {
    eprintln!(
        "parkan-render-demo --archive <path> [--model <name.msh>] [--lod N] [--group N] [--width W] [--height H]"
    );
    eprintln!("                  [--capture <out.png>] [--angle RAD] [--spin-rate RAD_PER_SEC]");
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
    let model = load_model_from_archive(&args.archive, args.model.as_deref()).map_err(|err| {
        format!(
            "failed to load model from archive {}: {err:?}",
            args.archive.display()
        )
    })?;

    let mesh = build_render_mesh(&model, args.lod, args.group);
    if mesh.vertices.is_empty() {
        return Err(format!(
            "model has no renderable triangles for lod={} group={}",
            args.lod, args.group
        ));
    }
    let Some((bounds_min, bounds_max)) = compute_bounds(&mesh.vertices) else {
        return Err(String::from("failed to compute mesh bounds"));
    };

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

    {
        let gl_attr = video.gl_attr();
        gl_attr.set_context_profile(sdl2::video::GLProfile::GLES);
        gl_attr.set_context_version(2, 0);
        gl_attr.set_depth_size(24);
        gl_attr.set_double_buffer(true);
    }

    let mut window_builder = video.window(
        "Parkan Render Demo (SDL2 + OpenGL ES 2.0)",
        args.width,
        args.height,
    );
    window_builder.opengl();
    if args.capture.is_some() {
        window_builder.hidden();
    } else {
        window_builder.resizable();
    }
    let window = window_builder
        .build()
        .map_err(|err| format!("failed to create window: {err}"))?;

    let gl_ctx = window
        .gl_create_context()
        .map_err(|err| format!("failed to create OpenGL context: {err}"))?;
    window
        .gl_make_current(&gl_ctx)
        .map_err(|err| format!("failed to make GL context current: {err}"))?;
    let _ = if args.capture.is_some() {
        video.gl_set_swap_interval(0)
    } else {
        video.gl_set_swap_interval(1)
    };

    let mut vertices_flat = Vec::with_capacity(mesh.vertices.len() * 3);
    for pos in &mesh.vertices {
        vertices_flat.extend_from_slice(pos);
    }

    let gl = unsafe {
        glow::Context::from_loader_function(|name| video.gl_get_proc_address(name) as *const _)
    };

    let program = unsafe { create_program(&gl)? };
    let u_mvp = unsafe { gl.get_uniform_location(program, "u_mvp") };
    let a_pos = unsafe { gl.get_attrib_location(program, "a_pos") }
        .ok_or_else(|| String::from("shader attribute a_pos is missing"))?;

    let vbo = unsafe { gl.create_buffer().map_err(|e| e.to_string())? };
    unsafe {
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
        gl.buffer_data_u8_slice(
            glow::ARRAY_BUFFER,
            cast_slice_u8(&vertices_flat),
            glow::STATIC_DRAW,
        );
        gl.bind_buffer(glow::ARRAY_BUFFER, None);
    }

    let result = if let Some(capture_path) = args.capture.as_ref() {
        run_capture(
            &gl,
            program,
            u_mvp.as_ref(),
            a_pos,
            vbo,
            mesh.vertices.len(),
            &args,
            center,
            camera_distance,
            capture_path,
        )
    } else {
        run_interactive(
            &sdl,
            &window,
            &gl,
            program,
            u_mvp.as_ref(),
            a_pos,
            vbo,
            mesh.vertices.len(),
            &args,
            center,
            camera_distance,
        )
    };

    unsafe {
        gl.delete_buffer(vbo);
        gl.delete_program(program);
    }

    result
}

#[allow(clippy::too_many_arguments)]
fn run_capture(
    gl: &glow::Context,
    program: glow::NativeProgram,
    u_mvp: Option<&glow::NativeUniformLocation>,
    a_pos: u32,
    vbo: glow::NativeBuffer,
    vertex_count: usize,
    args: &Args,
    center: [f32; 3],
    camera_distance: f32,
    capture_path: &Path,
) -> Result<(), String> {
    let angle = args.angle.unwrap_or(0.0);
    let mvp = compute_mvp(args.width, args.height, center, camera_distance, angle);
    unsafe {
        draw_frame(
            gl,
            program,
            u_mvp,
            a_pos,
            vbo,
            vertex_count,
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
    window: &sdl2::video::Window,
    gl: &glow::Context,
    program: glow::NativeProgram,
    u_mvp: Option<&glow::NativeUniformLocation>,
    a_pos: u32,
    vbo: glow::NativeBuffer,
    vertex_count: usize,
    args: &Args,
    center: [f32; 3],
    camera_distance: f32,
) -> Result<(), String> {
    let mut events = sdl
        .event_pump()
        .map_err(|err| format!("failed to get SDL event pump: {err}"))?;
    let start = Instant::now();

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
        let mvp = compute_mvp(w, h, center, camera_distance, angle);

        unsafe {
            draw_frame(gl, program, u_mvp, a_pos, vbo, vertex_count, w, h, &mvp);
        }
        window.gl_swap_window();
    }

    Ok(())
}

fn compute_mvp(
    width: u32,
    height: u32,
    center: [f32; 3],
    camera_distance: f32,
    angle_rad: f32,
) -> [f32; 16] {
    let aspect = (width as f32 / (height.max(1) as f32)).max(0.01);
    let proj = mat4_perspective(60.0_f32.to_radians(), aspect, 0.01, camera_distance * 10.0);
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
    a_pos: u32,
    vbo: glow::NativeBuffer,
    vertex_count: usize,
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

    gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
    gl.enable_vertex_attrib_array(a_pos);
    gl.vertex_attrib_pointer_f32(a_pos, 3, glow::FLOAT, false, 12, 0);
    gl.draw_arrays(
        glow::TRIANGLES,
        0,
        vertex_count.min(i32::MAX as usize) as i32,
    );
    gl.disable_vertex_attrib_array(a_pos);
    gl.bind_buffer(glow::ARRAY_BUFFER, None);
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

unsafe fn create_program(gl: &glow::Context) -> Result<glow::NativeProgram, String> {
    let vs_src = r#"
attribute vec3 a_pos;
uniform mat4 u_mvp;
void main() {
    gl_Position = u_mvp * vec4(a_pos, 1.0);
}
"#;

    let fs_src = r#"
precision mediump float;
void main() {
    gl_FragColor = vec4(0.85, 0.90, 1.00, 1.0);
}
"#;

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

fn cast_slice_u8<T>(slice: &[T]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(slice.as_ptr() as *const u8, std::mem::size_of_val(slice)) }
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
