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
//! `FParkan` command-line tools.

use fparkan_assets::extend_graph_report_with_visual_dependencies;
use fparkan_corpus::{discover, render_report_json, report, DiscoverOptions};
use fparkan_inspection::inspect_archive_file;
use fparkan_inspection::ArchiveInspection;
use fparkan_prototype::build_prototype_graph_report;
use fparkan_resource::{resource_name, CachedResourceRepository};
use fparkan_runtime::{
    create, load_mission, EngineConfig, EngineMode, EngineServices, MissionRequest,
};
use fparkan_vfs::DirectoryVfs;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;

const ARCHIVE_INSPECT_SCHEMA: &str = "fparkan-archive-inspect-v1";
const MISSION_GRAPH_SCHEMA: &str = "fparkan-mission-graph-v1";
const PROTOTYPE_INSPECT_SCHEMA: &str = "fparkan-prototype-inspect-v1";

#[derive(Serialize)]
struct PrototypeInspectOutput<'a> {
    schema_version: &'static str,
    key: &'a str,
    roots: usize,
    prototype_requests: usize,
    resolved: usize,
    unit_references: usize,
    unit_components: usize,
    direct_references: usize,
    wear: usize,
    materials: usize,
    textures: usize,
    lightmaps: usize,
    failures: usize,
}

#[derive(Serialize)]
struct MissionGraphOutput<'a> {
    schema_version: &'static str,
    mission: &'a str,
    objects: usize,
    paths: usize,
    clans: usize,
    extras: usize,
    roots: usize,
    direct_references: usize,
    unit_references: usize,
    unit_components: usize,
    prototype_requests: usize,
    wear: usize,
    materials: usize,
    textures: usize,
    lightmaps: usize,
    failures: usize,
}

#[derive(Serialize)]
struct ArchiveInspectOutput<'a> {
    schema_version: &'static str,
    path: &'a str,
    kind: &'a str,
    entries: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    lookup_order_valid: Option<bool>,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = run(&args);
    let code = exit_code(&result);
    if let Err(err) = result {
        eprintln!("{err}");
    }
    std::process::exit(code);
}

fn run(args: &[String]) -> Result<(), String> {
    match args {
        [domain, command, rest @ ..] if domain == "corpus" && command == "discover" => {
            let rest = strip_format_json(rest)?;
            let root = parse_root(&rest)?;
            let manifest =
                discover(&root, DiscoverOptions::default()).map_err(|e| e.to_string())?;
            let report = report(&root, &manifest).map_err(|e| e.to_string())?;
            println!("{}", render_report_json(&report));
            Ok(())
        }
        [domain, command, rest @ ..] if domain == "corpus" && command == "validate" => {
            let rest = strip_format_json(rest)?;
            let root = parse_root(&rest)?;
            let manifest =
                discover(&root, DiscoverOptions::default()).map_err(|e| e.to_string())?;
            let report = report(&root, &manifest).map_err(|e| e.to_string())?;
            if report.casefold_collisions > 0 {
                return Err("casefold collisions found".to_string());
            }
            if report.failures > 0 {
                return Err(format!("corpus report found {} failures", report.failures));
            }
            println!("{}", render_report_json(&report));
            Ok(())
        }
        [domain, command, rest @ ..] if domain == "archive" && command == "inspect" => {
            let rest = strip_format_json(rest)?;
            inspect_archive(&rest)
        }
        [domain, command, rest @ ..] if domain == "prototype" && command == "inspect" => {
            let rest = strip_format_json(rest)?;
            inspect_prototype(&rest)
        }
        [domain, command, rest @ ..] if domain == "mission" && command == "graph" => {
            let rest = strip_format_json(rest)?;
            graph_mission(&rest)
        }
        _ => Err(usage()),
    }
}

fn exit_code(result: &Result<(), String>) -> i32 {
    if result.is_ok() {
        0
    } else {
        2
    }
}

fn strip_format_json(args: &[String]) -> Result<Vec<String>, String> {
    let mut stripped = Vec::with_capacity(args.len());
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--format" {
            let value = iter
                .next()
                .ok_or_else(|| "--format requires a value".to_string())?;
            if value != "json" {
                return Err(format!("unsupported output format: {value}"));
            }
            continue;
        }
        stripped.push(arg.clone());
    }
    Ok(stripped)
}

fn parse_root(args: &[String]) -> Result<PathBuf, String> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--root" {
            return iter
                .next()
                .map(PathBuf::from)
                .ok_or_else(|| "--root requires a path".to_string());
        }
    }
    Err("missing --root".to_string())
}

fn parse_root_alias(args: &[String]) -> Result<PathBuf, String> {
    parse_option(args, &["--root", "--game-root"])
        .map(PathBuf::from)
        .ok_or_else(|| "missing --root".to_string())
}

fn parse_required(args: &[String], names: &[&str], label: &str) -> Result<String, String> {
    parse_option(args, names).ok_or_else(|| format!("missing {label}"))
}

fn parse_option(args: &[String], names: &[&str]) -> Option<String> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if names.iter().any(|name| arg == name) {
            return iter.next().cloned();
        }
    }
    None
}

fn inspect_prototype(args: &[String]) -> Result<(), String> {
    let root = parse_root_alias(args)?;
    let key = parse_required(args, &["--key"], "--key")?;
    let vfs = Arc::new(DirectoryVfs::new(root));
    let repository = CachedResourceRepository::new(vfs.clone());
    let roots = [resource_name(key.as_bytes())];
    let (graph, resolved, mut report) =
        build_prototype_graph_report(&repository, vfs.as_ref(), &roots);
    extend_graph_report_with_visual_dependencies(&repository, &mut report, &graph, &resolved);
    println!("{}", prototype_inspect_json(&key, &graph, &report)?);
    Ok(())
}

fn prototype_inspect_json(
    key: &str,
    graph: &fparkan_prototype::PrototypeGraph,
    report: &fparkan_prototype::PrototypeGraphReport,
) -> Result<String, String> {
    serialize_json(&PrototypeInspectOutput {
        schema_version: PROTOTYPE_INSPECT_SCHEMA,
        key,
        roots: report.root_count,
        prototype_requests: graph.prototype_requests.len(),
        resolved: report.resolved_count,
        unit_references: report.unit_reference_count,
        unit_components: report.unit_component_count,
        direct_references: report.direct_reference_count,
        wear: report.wear_resolved_count,
        materials: report.material_resolved_count,
        textures: report.texture_resolved_count,
        lightmaps: report.lightmap_resolved_count,
        failures: report.failures.len(),
    })
}

fn graph_mission(args: &[String]) -> Result<(), String> {
    let root = parse_root_alias(args)?;
    let mission = parse_required(args, &["--mission"], "--mission")?;
    let services = EngineServices::new(Arc::new(DirectoryVfs::new(root)));
    let mut engine = create(
        EngineConfig {
            mode: EngineMode::Headless,
        },
        services,
    )
    .map_err(|err| err.to_string())?;
    let loaded = load_mission(
        &mut engine,
        MissionRequest {
            key: mission.clone(),
        },
    )
    .map_err(|err| err.to_string())?;
    println!("{}", mission_graph_json(&mission, &loaded)?);
    Ok(())
}

fn mission_graph_json(
    mission: &str,
    loaded: &fparkan_runtime::LoadedMission,
) -> Result<String, String> {
    serialize_json(&MissionGraphOutput {
        schema_version: MISSION_GRAPH_SCHEMA,
        mission,
        objects: loaded.object_count,
        paths: loaded.path_count,
        clans: loaded.clan_count,
        extras: loaded.extra_count,
        roots: loaded.graph_root_count,
        direct_references: loaded.graph_direct_reference_count,
        unit_references: loaded.graph_unit_reference_count,
        unit_components: loaded.graph_unit_component_count,
        prototype_requests: loaded.graph_resolved_count,
        wear: loaded.graph_wear_resolved_count,
        materials: loaded.graph_material_resolved_count,
        textures: loaded.graph_texture_resolved_count,
        lightmaps: loaded.graph_lightmap_resolved_count,
        failures: loaded.graph_failure_count,
    })
}

fn inspect_archive(args: &[String]) -> Result<(), String> {
    let path = parse_archive_path(args)?;
    let inspection = inspect_archive_file(&path, 0).map_err(|err| err.to_string())?;

    match inspection {
        ArchiveInspection::Nres {
            entries,
            lookup_order_valid,
            ..
        } => {
            println!(
                "{}",
                archive_inspect_json(
                    &path.display().to_string(),
                    "NRes",
                    entries,
                    Some(lookup_order_valid),
                )?
            );
            Ok(())
        }
        ArchiveInspection::Rsli { entries } => {
            println!(
                "{}",
                archive_inspect_json(&path.display().to_string(), "RsLi", entries, None)?
            );
            Ok(())
        }
        ArchiveInspection::Unsupported => {
            Err(format!("{}: unsupported archive magic", path.display()))
        }
    }
}

fn archive_inspect_json(
    path: &str,
    kind: &str,
    entries: usize,
    lookup_order_valid: Option<bool>,
) -> Result<String, String> {
    serialize_json(&ArchiveInspectOutput {
        schema_version: ARCHIVE_INSPECT_SCHEMA,
        path,
        kind,
        entries,
        lookup_order_valid,
    })
}

fn parse_archive_path(args: &[String]) -> Result<PathBuf, String> {
    match args {
        [path] => Ok(PathBuf::from(path)),
        [flag, path] if flag == "--file" => Ok(PathBuf::from(path)),
        _ => Err("archive inspect requires <file> or --file <file>".to_string()),
    }
}

fn serialize_json<T: Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_string(value).map_err(|err| err.to_string())
}

fn usage() -> String {
    "usage: fparkan corpus discover|validate --root <path> [--format json] | archive inspect <file> [--format json] | prototype inspect --root <path> --key <key> [--format json] | mission graph --root <path> --mission <path> [--format json]".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn stable_exit_codes_are_mapped() {
        assert_eq!(exit_code(&Ok(())), 0);
        assert_eq!(exit_code(&Err("failure".to_string())), 2);
    }

    #[test]
    fn accepts_json_format_option() {
        assert_eq!(
            strip_format_json(&strings(&["--root", "testdata", "--format", "json"])),
            Ok(strings(&["--root", "testdata"]))
        );
        assert_eq!(
            strip_format_json(&strings(&["--format", "text"])),
            Err("unsupported output format: text".to_string())
        );
    }

    #[test]
    fn archive_json_has_schema_version() {
        let json = archive_inspect_json("archive.lib", "NRes", 3, Some(true))
            .expect("serialize archive inspection");

        assert!(json.contains("\"schema_version\":\"fparkan-archive-inspect-v1\""));
        assert!(json.contains("\"kind\":\"NRes\""));
        assert!(json.contains("\"lookup_order_valid\":true"));
    }

    #[test]
    fn prototype_graph_json_has_canonical_field_order() {
        let mut graph = fparkan_prototype::PrototypeGraph::default();
        graph
            .prototype_requests
            .push(fparkan_prototype::PrototypeKey(resource_name(b"root")));
        let report = fparkan_prototype::PrototypeGraphReport {
            root_count: 1,
            direct_reference_count: 1,
            resolved_count: 1,
            ..fparkan_prototype::PrototypeGraphReport::default()
        };

        let json =
            prototype_inspect_json("root", &graph, &report).expect("serialize prototype graph");

        assert_eq!(
            json,
            "{\"schema_version\":\"fparkan-prototype-inspect-v1\",\"key\":\"root\",\"roots\":1,\"prototype_requests\":1,\"resolved\":1,\"unit_references\":0,\"unit_components\":0,\"direct_references\":1,\"wear\":0,\"materials\":0,\"textures\":0,\"lightmaps\":0,\"failures\":0}"
        );
    }

    #[test]
    fn mission_graph_json_has_canonical_field_order() {
        let loaded = fparkan_runtime::LoadedMission {
            key: "mission".to_string(),
            path_count: 3,
            clan_count: 4,
            object_count: 2,
            extra_count: 5,
            land_msh_path: "land.msh".to_string(),
            land_map_path: "land.map".to_string(),
            build_category_count: 0,
            areal_count: 0,
            surface_count: 0,
            registered_objects: 0,
            graph_unit_reference_count: 7,
            graph_direct_reference_count: 6,
            graph_unit_component_count: 8,
            graph_root_count: 9,
            asset_visual_count: 0,
            graph_resolved_count: 10,
            graph_mesh_dependency_count: 0,
            graph_failure_count: 15,
            graph_wear_request_count: 0,
            graph_wear_resolved_count: 11,
            graph_material_slot_count: 0,
            graph_material_resolved_count: 12,
            graph_texture_request_count: 0,
            graph_texture_resolved_count: 13,
            graph_lightmap_request_count: 0,
            graph_lightmap_resolved_count: 14,
            asset_model_count: 0,
            asset_material_count: 0,
            asset_texture_count: 0,
            asset_lightmap_count: 0,
        };

        let json = mission_graph_json("mission", &loaded).expect("serialize mission graph");

        assert_eq!(
            json,
            "{\"schema_version\":\"fparkan-mission-graph-v1\",\"mission\":\"mission\",\"objects\":2,\"paths\":3,\"clans\":4,\"extras\":5,\"roots\":9,\"direct_references\":6,\"unit_references\":7,\"unit_components\":8,\"prototype_requests\":10,\"wear\":11,\"materials\":12,\"textures\":13,\"lightmaps\":14,\"failures\":15}"
        );
    }
}
