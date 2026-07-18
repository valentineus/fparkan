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
    advance_reference_movement, create, load_mission, step_headless, EngineConfig, EngineMode,
    EngineServices, MissionRequest,
};
use fparkan_vfs::DirectoryVfs;
use fparkan_world::{InputSnapshot, OriginalObjectId};
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
            "mission objects={} areals={} surfaces={} graph_roots={} components={} wear={} material_slots={} textures={} lightmaps={} scripts={} script_events={} script_varset_declarations={} graph_failures={}",
            loaded.object_count,
            loaded.areal_count,
            loaded.surface_count,
            loaded.graph_root_count,
            loaded.graph_unit_component_count,
            loaded.graph_wear_resolved_count,
            loaded.graph_material_resolved_count,
            loaded.graph_texture_resolved_count,
            loaded.graph_lightmap_resolved_count,
            loaded.script_bundle_count,
            loaded.script_event_count,
            loaded.script_varset_declaration_count,
            loaded.graph_failure_count
        );
    }
    if let Some(movement) = args.reference_movement {
        let reached = advance_reference_movement(
            &mut engine,
            OriginalObjectId(movement.original_id),
            movement.target_xy,
            movement.max_step,
        )
        .map_err(|err| format!("{err}"))?;
        println!(
            "reference_movement original_id={} target_xy=[{},{}] max_step={} reached={reached}",
            movement.original_id, movement.target_xy[0], movement.target_xy[1], movement.max_step,
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

#[derive(Debug)]
struct Args {
    root: Option<PathBuf>,
    mission: Option<String>,
    ticks: u64,
    reference_movement: Option<ReferenceMovement>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ReferenceMovement {
    original_id: u32,
    target_xy: [f32; 2],
    max_step: f32,
}

impl Args {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut parsed = Self {
            root: None,
            mission: None,
            ticks: 1,
            reference_movement: None,
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
                "--move-object" => {
                    if parsed.reference_movement.is_some() {
                        return Err("--move-object may be specified once".to_string());
                    }
                    let original_id = iter
                        .next()
                        .ok_or_else(|| "--move-object requires an original object id".to_string())?
                        .parse()
                        .map_err(|_| {
                            "--move-object object id must be an unsigned integer".to_string()
                        })?;
                    let x = parse_finite_argument(
                        iter.next(),
                        "--move-object requires a finite X target",
                    )?;
                    let y = parse_finite_argument(
                        iter.next(),
                        "--move-object requires a finite Y target",
                    )?;
                    let max_step = parse_finite_argument(
                        iter.next(),
                        "--move-object requires a finite positive maximum step",
                    )?;
                    if max_step <= 0.0 {
                        return Err("--move-object maximum step must be positive".to_string());
                    }
                    parsed.reference_movement = Some(ReferenceMovement {
                        original_id,
                        target_xy: [x, y],
                        max_step,
                    });
                }
                _ => return Err(usage()),
            }
        }
        if parsed.mission.is_some() && parsed.root.is_none() {
            return Err("--mission requires --root".to_string());
        }
        if parsed.reference_movement.is_some() && parsed.mission.is_none() {
            return Err("--move-object requires --mission".to_string());
        }
        Ok(parsed)
    }
}

fn parse_finite_argument(value: Option<&String>, error: &str) -> Result<f32, String> {
    let value: f32 = value
        .ok_or_else(|| error.to_string())?
        .parse()
        .map_err(|_| error.to_string())?;
    value
        .is_finite()
        .then_some(value)
        .ok_or_else(|| error.to_string())
}

fn usage() -> String {
    "usage: fparkan-headless [--root <path> --mission <path>] [--move-object <original-id> <x> <y> <max-step>] [--ticks <n>]".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn move_object_parses_a_single_finite_reference_command() {
        let parsed = Args::parse(&args(&[
            "--root",
            "C:/game",
            "--mission",
            "MISSIONS/Autodemo.00/data.tma",
            "--move-object",
            "7",
            "12.5",
            "-3",
            "0.25",
            "--ticks",
            "2",
        ]))
        .expect("args");
        assert_eq!(parsed.ticks, 2);
        assert_eq!(
            parsed.reference_movement,
            Some(ReferenceMovement {
                original_id: 7,
                target_xy: [12.5, -3.0],
                max_step: 0.25,
            })
        );
    }

    #[test]
    fn move_object_requires_a_loaded_mission_and_valid_step() {
        assert_eq!(
            Args::parse(&args(&["--move-object", "7", "1", "2", "1"])).expect_err("mission"),
            "--move-object requires --mission"
        );
        assert_eq!(
            Args::parse(&args(&[
                "--root",
                "C:/game",
                "--mission",
                "M/data.tma",
                "--move-object",
                "7",
                "1",
                "2",
                "0",
            ]))
            .expect_err("step"),
            "--move-object maximum step must be positive"
        );
    }
}
