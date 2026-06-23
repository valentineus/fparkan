#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        clippy::cast_precision_loss,
        clippy::expect_used,
        clippy::float_cmp,
        clippy::identity_op,
        clippy::too_many_lines,
        clippy::uninlined_format_args,
        clippy::map_unwrap_or,
        clippy::needless_raw_string_hashes,
        clippy::semicolon_if_nothing_returned,
        clippy::type_complexity,
        clippy::panic,
        clippy::unwrap_used
    )
)]
#![allow(clippy::print_stderr, clippy::print_stdout)]
//! `FParkan` headless runtime entrypoint.

use fparkan_runtime::{
    create, load_mission, step_headless, EngineConfig, EngineMode, EngineServices, MissionRequest,
};
use fparkan_vfs::DirectoryVfs;
use fparkan_world::InputSnapshot;
use std::path::PathBuf;
use std::sync::Arc;

fn main() {
    if let Err(err) = run() {
        eprintln!("{err}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    let args = Args::parse(&raw_args)?;
    let services = if let Some(root) = &args.root {
        EngineServices::new(Arc::new(DirectoryVfs::new(root)))
    } else {
        EngineServices::default()
    };
    let mut engine = create(
        EngineConfig {
            mode: EngineMode::Headless,
        },
        services,
    )
    .map_err(|err| format!("{err}"))?;
    if let Some(mission) = args.mission {
        let loaded = load_mission(&mut engine, MissionRequest { key: mission })
            .map_err(|err| format!("{err}"))?;
        println!(
            "mission objects={} areals={} surfaces={} graph_roots={} components={} wear={} material_slots={} textures={} lightmaps={} graph_failures={}",
            loaded.object_count,
            loaded.areal_count,
            loaded.surface_count,
            loaded.graph_root_count,
            loaded.graph_unit_component_count,
            loaded.graph_wear_resolved_count,
            loaded.graph_material_resolved_count,
            loaded.graph_texture_resolved_count,
            loaded.graph_lightmap_resolved_count,
            loaded.graph_failure_count
        );
    }
    let mut last = None;
    for _ in 0..args.ticks {
        last = Some(step_headless(&mut engine, InputSnapshot).map_err(|err| format!("{err}"))?);
    }
    if let Some(frame) = last {
        println!(
            "tick={} hash={:02x?}",
            frame.snapshot.tick.0, frame.snapshot.hash.0
        );
    }
    Ok(())
}

struct Args {
    root: Option<PathBuf>,
    mission: Option<String>,
    ticks: u64,
}

impl Args {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut parsed = Self {
            root: None,
            mission: None,
            ticks: 1,
        };
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--root" => {
                    parsed.root = Some(
                        iter.next()
                            .map(PathBuf::from)
                            .ok_or_else(|| "--root requires a path".to_string())?,
                    );
                }
                "--mission" => {
                    parsed.mission = Some(
                        iter.next()
                            .cloned()
                            .ok_or_else(|| "--mission requires a path".to_string())?,
                    );
                }
                "--ticks" => {
                    parsed.ticks = iter
                        .next()
                        .ok_or_else(|| "--ticks requires a value".to_string())?
                        .parse()
                        .map_err(|_| "--ticks must be an integer".to_string())?;
                }
                _ => return Err(usage()),
            }
        }
        if parsed.mission.is_some() && parsed.root.is_none() {
            return Err("--mission requires --root".to_string());
        }
        Ok(parsed)
    }
}

fn usage() -> String {
    "usage: fparkan-headless [--root <path> --mission <path>] [--ticks <n>]".to_string()
}
