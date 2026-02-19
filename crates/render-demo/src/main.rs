use glow::HasContext as _;
use render_core::{build_render_mesh, compute_bounds};
use render_demo::load_model_from_archive;
use std::path::PathBuf;
use std::time::Instant;

struct Args {
    archive: PathBuf,
    model: Option<String>,
    lod: usize,
    group: usize,
}

fn parse_args() -> Result<Args, String> {
    let mut archive = None;
    let mut model = None;
    let mut lod = 0usize;
    let mut group = 0usize;

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
    })
}

fn print_help() {
    eprintln!("parkan-render-demo --archive <path> [--model <name.msh>] [--lod N] [--group N]");
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

    let model = match load_model_from_archive(&args.archive, args.model.as_deref()) {
        Ok(v) => v,
        Err(err) => {
            eprintln!("failed to load model: {err:?}");
            std::process::exit(1);
        }
    };

    let mesh = build_render_mesh(&model, args.lod, args.group);
    if mesh.vertices.is_empty() {
        eprintln!(
            "model has no renderable triangles for lod={} group={}",
            args.lod, args.group
        );
        std::process::exit(1);
    }
    let Some((bounds_min, bounds_max)) = compute_bounds(&mesh.vertices) else {
        eprintln!("failed to compute mesh bounds");
        std::process::exit(1);
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

    let sdl = sdl2::init().expect("failed to init SDL2");
    let video = sdl.video().expect("failed to init SDL2 video");

    {
        let gl_attr = video.gl_attr();
        gl_attr.set_context_profile(sdl2::video::GLProfile::GLES);
        gl_attr.set_context_version(2, 0);
        gl_attr.set_depth_size(24);
        gl_attr.set_double_buffer(true);
    }

    let window = video
        .window("Parkan Render Demo (SDL2 + OpenGL ES 2.0)", 1280, 720)
        .opengl()
        .resizable()
        .build()
        .expect("failed to create window");

    let gl_ctx = window
        .gl_create_context()
        .expect("failed to create OpenGL context");
    window
        .gl_make_current(&gl_ctx)
        .expect("failed to make GL context current");
    let _ = video.gl_set_swap_interval(1);

    let mut vertices_flat = Vec::with_capacity(mesh.vertices.len() * 3);
    for pos in &mesh.vertices {
        vertices_flat.extend_from_slice(pos);
    }

    let gl = unsafe {
        glow::Context::from_loader_function(|name| video.gl_get_proc_address(name) as *const _)
    };

    let program = unsafe { create_program(&gl).expect("failed to create shader program") };
    let u_mvp = unsafe { gl.get_uniform_location(program, "u_mvp") };
    let a_pos = unsafe { gl.get_attrib_location(program, "a_pos") };
    let a_pos = a_pos.expect("shader attribute a_pos is missing");

    let vbo = unsafe { gl.create_buffer().expect("failed to create VBO") };
    unsafe {
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
        gl.buffer_data_u8_slice(
            glow::ARRAY_BUFFER,
            cast_slice_u8(&vertices_flat),
            glow::STATIC_DRAW,
        );
        gl.bind_buffer(glow::ARRAY_BUFFER, None);
    }

    let mut events = sdl.event_pump().expect("failed to get SDL event pump");
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

        let elapsed = start.elapsed().as_secs_f32();
        let (w, h) = window.size();
        let aspect = (w as f32 / (h.max(1) as f32)).max(0.01);

        let proj = mat4_perspective(60.0_f32.to_radians(), aspect, 0.01, camera_distance * 10.0);
        let view = mat4_translation(0.0, 0.0, -camera_distance);
        let center_shift = mat4_translation(-center[0], -center[1], -center[2]);
        let rot = mat4_rotation_y(elapsed * 0.35);
        let model_m = mat4_mul(&rot, &center_shift);
        let vp = mat4_mul(&view, &model_m);
        let mvp = mat4_mul(&proj, &vp);

        unsafe {
            gl.viewport(0, 0, w as i32, h as i32);
            gl.enable(glow::DEPTH_TEST);
            gl.clear_color(0.06, 0.08, 0.12, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);

            gl.use_program(Some(program));
            gl.uniform_matrix_4_f32_slice(u_mvp.as_ref(), false, &mvp);

            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.enable_vertex_attrib_array(a_pos);
            gl.vertex_attrib_pointer_f32(a_pos, 3, glow::FLOAT, false, 12, 0);
            gl.draw_arrays(
                glow::TRIANGLES,
                0,
                i32::try_from(mesh.vertices.len()).unwrap_or(i32::MAX),
            );
            gl.disable_vertex_attrib_array(a_pos);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);
            gl.use_program(None);
        }

        window.gl_swap_window();
    }

    unsafe {
        gl.delete_buffer(vbo);
        gl.delete_program(program);
    }
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
