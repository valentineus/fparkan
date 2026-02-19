use image::RgbaImage;
use render_parity::{
    build_diff_image, compare_images, evaluate_metrics, CaseSpec, ManifestMeta, ParityManifest,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const DEFAULT_MANIFEST: &str = "parity/cases.toml";
const DEFAULT_OUTPUT_DIR: &str = "target/render-parity/current";
const DEFAULT_WIDTH: u32 = 1280;
const DEFAULT_HEIGHT: u32 = 720;
const DEFAULT_LOD: usize = 0;
const DEFAULT_GROUP: usize = 0;
const DEFAULT_ANGLE: f32 = 0.0;
const DEFAULT_DIFF_THRESHOLD: u8 = 8;
const DEFAULT_MAX_MEAN_ABS: f32 = 2.0;
const DEFAULT_MAX_CHANGED_RATIO: f32 = 0.01;

struct Args {
    manifest: PathBuf,
    output_dir: PathBuf,
    demo_bin: Option<PathBuf>,
    keep_going: bool,
}

#[derive(Debug, Clone)]
struct EffectiveCase {
    id: String,
    archive: PathBuf,
    model: Option<String>,
    reference: PathBuf,
    width: u32,
    height: u32,
    lod: usize,
    group: usize,
    angle: f32,
    diff_threshold: u8,
    max_mean_abs: f32,
    max_changed_ratio: f32,
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

fn parse_args() -> Result<Args, String> {
    let mut manifest = PathBuf::from(DEFAULT_MANIFEST);
    let mut output_dir = PathBuf::from(DEFAULT_OUTPUT_DIR);
    let mut demo_bin = None;
    let mut keep_going = false;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--manifest" => {
                let value = it
                    .next()
                    .ok_or_else(|| String::from("missing value for --manifest"))?;
                manifest = PathBuf::from(value);
            }
            "--output-dir" => {
                let value = it
                    .next()
                    .ok_or_else(|| String::from("missing value for --output-dir"))?;
                output_dir = PathBuf::from(value);
            }
            "--demo-bin" => {
                let value = it
                    .next()
                    .ok_or_else(|| String::from("missing value for --demo-bin"))?;
                demo_bin = Some(PathBuf::from(value));
            }
            "--keep-going" => {
                keep_going = true;
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

    Ok(Args {
        manifest,
        output_dir,
        demo_bin,
        keep_going,
    })
}

fn print_help() {
    eprintln!(
        "render-parity [--manifest <cases.toml>] [--output-dir <dir>] [--demo-bin <path>] [--keep-going]"
    );
    eprintln!("  --manifest    path to parity manifest (default: {DEFAULT_MANIFEST})");
    eprintln!("  --output-dir  where current renders and diff images are written");
    eprintln!("  --demo-bin    prebuilt parkan-render-demo binary path");
    eprintln!("  --keep-going  continue all cases even after failures");
}

fn run(args: Args) -> Result<(), String> {
    let workspace = workspace_root()?;
    let manifest_path = resolve_path(&workspace, &args.manifest);
    let output_dir = resolve_path(&workspace, &args.output_dir);
    let demo_bin = args
        .demo_bin
        .as_ref()
        .map(|path| resolve_path(&workspace, path));

    let manifest_raw = fs::read_to_string(&manifest_path)
        .map_err(|err| format!("failed to read manifest {}: {err}", manifest_path.display()))?;
    let manifest: ParityManifest = toml::from_str(&manifest_raw).map_err(|err| {
        format!(
            "failed to parse manifest {}: {err}",
            manifest_path.display()
        )
    })?;

    if manifest.cases.is_empty() {
        println!(
            "render-parity: no cases in {} (nothing to validate)",
            manifest_path.display()
        );
        return Ok(());
    }

    fs::create_dir_all(&output_dir).map_err(|err| {
        format!(
            "failed to create output directory {}: {err}",
            output_dir.display()
        )
    })?;

    let manifest_dir = manifest_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| workspace.clone());

    let mut failed_cases = 0usize;
    for case in &manifest.cases {
        let effective = make_effective_case(&manifest.meta, case, &manifest_dir)?;
        let case_file = output_dir.join(format!("{}.png", sanitize_case_id(&effective.id)));
        let diff_file = output_dir
            .join("diff")
            .join(format!("{}.png", sanitize_case_id(&effective.id)));

        let run_res = run_single_case(
            &workspace, // ensure `cargo run` executes from workspace root
            demo_bin.as_deref(),
            &effective,
            &case_file,
            &diff_file,
        );

        match run_res {
            Ok(()) => {}
            Err(err) => {
                failed_cases = failed_cases.saturating_add(1);
                eprintln!("[FAIL] {}: {}", effective.id, err);
                if !args.keep_going {
                    break;
                }
            }
        }
    }

    if failed_cases > 0 {
        return Err(format!(
            "render-parity failed: {} case(s) did not match reference frames",
            failed_cases
        ));
    }

    println!("render-parity: all cases passed");
    Ok(())
}

fn run_single_case(
    workspace: &Path,
    demo_bin: Option<&Path>,
    case: &EffectiveCase,
    case_file: &Path,
    diff_file: &Path,
) -> Result<(), String> {
    run_render_capture(workspace, demo_bin, case, case_file)?;

    let reference = load_rgba(&case.reference)?;
    let actual = load_rgba(case_file)?;
    let metrics = compare_images(&reference, &actual, case.diff_threshold)?;
    let violations = evaluate_metrics(&metrics, case.max_mean_abs, case.max_changed_ratio);

    if violations.is_empty() {
        println!(
            "[OK] {} mean_abs={:.4} changed={:.4}% max_abs={} ({}x{})",
            case.id,
            metrics.mean_abs,
            metrics.changed_ratio * 100.0,
            metrics.max_abs,
            metrics.width,
            metrics.height
        );
        return Ok(());
    }

    if let Some(parent) = diff_file.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            format!(
                "failed to create diff output directory {}: {err}",
                parent.display()
            )
        })?;
    }
    let diff = build_diff_image(&reference, &actual)?;
    diff.save(diff_file)
        .map_err(|err| format!("failed to save diff image {}: {err}", diff_file.display()))?;

    let mut details = String::new();
    for item in violations {
        if !details.is_empty() {
            details.push_str("; ");
        }
        details.push_str(&item);
    }
    Err(format!(
        "{} | diff={} | mean_abs={:.4}, changed={:.4}% ({} px), max_abs={}",
        details,
        diff_file.display(),
        metrics.mean_abs,
        metrics.changed_ratio * 100.0,
        metrics.changed_pixels,
        metrics.max_abs
    ))
}

fn run_render_capture(
    workspace: &Path,
    demo_bin: Option<&Path>,
    case: &EffectiveCase,
    out_path: &Path,
) -> Result<(), String> {
    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            format!(
                "failed to create capture directory {}: {err}",
                parent.display()
            )
        })?;
    }

    let mut cmd = if let Some(bin) = demo_bin {
        Command::new(bin)
    } else {
        let mut command = Command::new("cargo");
        command.args(["run", "-p", "render-demo", "--features", "demo", "--"]);
        command
    };

    cmd.current_dir(workspace)
        .arg("--archive")
        .arg(&case.archive)
        .arg("--lod")
        .arg(case.lod.to_string())
        .arg("--group")
        .arg(case.group.to_string())
        .arg("--width")
        .arg(case.width.to_string())
        .arg("--height")
        .arg(case.height.to_string())
        .arg("--angle")
        .arg(case.angle.to_string())
        .arg("--capture")
        .arg(out_path);

    if let Some(model) = case.model.as_deref() {
        cmd.arg("--model").arg(model);
    }

    let output = cmd.output().map_err(|err| {
        let mode = if demo_bin.is_some() {
            "parkan-render-demo"
        } else {
            "cargo run -p render-demo"
        };
        format!("failed to execute {} for case {}: {err}", mode, case.id)
    })?;
    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "render command exited with status {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            stdout,
            stderr
        ));
    }

    Ok(())
}

fn load_rgba(path: &Path) -> Result<RgbaImage, String> {
    image::open(path)
        .map_err(|err| format!("failed to load image {}: {err}", path.display()))
        .map(|img| img.to_rgba8())
}

fn make_effective_case(
    meta: &ManifestMeta,
    case: &CaseSpec,
    manifest_dir: &Path,
) -> Result<EffectiveCase, String> {
    let width = case.width.or(meta.width).unwrap_or(DEFAULT_WIDTH);
    let height = case.height.or(meta.height).unwrap_or(DEFAULT_HEIGHT);
    if width == 0 || height == 0 {
        return Err(format!(
            "case '{}' has invalid dimensions {}x{}",
            case.id, width, height
        ));
    }

    let archive = resolve_path(manifest_dir, Path::new(&case.archive));
    let reference = resolve_path(manifest_dir, Path::new(&case.reference));
    if !archive.is_file() {
        return Err(format!(
            "case '{}' archive not found: {}",
            case.id,
            archive.display()
        ));
    }
    if !reference.is_file() {
        return Err(format!(
            "case '{}' reference frame not found: {}",
            case.id,
            reference.display()
        ));
    }

    Ok(EffectiveCase {
        id: case.id.clone(),
        archive,
        model: case.model.clone(),
        reference,
        width,
        height,
        lod: case.lod.or(meta.lod).unwrap_or(DEFAULT_LOD),
        group: case.group.or(meta.group).unwrap_or(DEFAULT_GROUP),
        angle: case.angle.or(meta.angle).unwrap_or(DEFAULT_ANGLE),
        diff_threshold: case
            .diff_threshold
            .or(meta.diff_threshold)
            .unwrap_or(DEFAULT_DIFF_THRESHOLD),
        max_mean_abs: case
            .max_mean_abs
            .or(meta.max_mean_abs)
            .unwrap_or(DEFAULT_MAX_MEAN_ABS),
        max_changed_ratio: case
            .max_changed_ratio
            .or(meta.max_changed_ratio)
            .unwrap_or(DEFAULT_MAX_CHANGED_RATIO),
    })
}

fn sanitize_case_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn workspace_root() -> Result<PathBuf, String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .map_err(|err| format!("failed to resolve workspace root: {err}"))?;
    Ok(root)
}

fn resolve_path(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}
