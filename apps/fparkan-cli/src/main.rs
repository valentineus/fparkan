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
const PROTOTYPE_INSPECT_SCHEMA: &str = "fparkan-prototype-inspect-v1";
const MISSION_GRAPH_SCHEMA: &str = "fparkan-mission-graph-v1";

#[derive(Serialize)]
struct ArchiveInspectOutput<'a> {
    schema_version: &'static str,
    path: &'a str,
    kind: &'a str,
    entries: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    lookup_order_valid: Option<bool>,
}

#[derive(Serialize)]
struct PrototypeInspectOutput {
    schema_version: &'static str,
    key: String,
    roots: usize,
    node_count: usize,
    edge_count: usize,
    prototype_requests: usize,
    resolved: usize,
    unit_references: usize,
    unit_components: usize,
    direct_references: usize,
    wear_requests: usize,
    wear: usize,
    materials: usize,
    textures: usize,
    lightmaps: usize,
    is_success: bool,
    failures: Vec<GraphFailureOutput>,
}

#[derive(Serialize)]
struct MissionGraphOutput {
    schema_version: &'static str,
    mission: String,
    objects: usize,
    paths: usize,
    clans: usize,
    extras: usize,
    roots: usize,
    node_count: usize,
    edge_count: usize,
    direct_references: usize,
    unit_references: usize,
    unit_components: usize,
    prototype_requests: usize,
    wear_requests: usize,
    wear: usize,
    materials: usize,
    textures: usize,
    lightmaps: usize,
    is_success: bool,
    failures: usize,
}

#[derive(Serialize)]
struct GraphFailureOutput {
    root_index: usize,
    edge: &'static str,
    requiredness: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    archive: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resource: Option<String>,
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
    let (mut graph, resolved, mut report) =
        build_prototype_graph_report(&repository, vfs.as_ref(), &roots);
    extend_graph_report_with_visual_dependencies(&repository, &mut report, &mut graph, &resolved);
    println!(
        "{}",
        prototype_inspect_json(&key, &graph, &report).map_err(|err| err.to_string())?
    );
    Ok(())
}

fn prototype_inspect_json(
    key: &str,
    graph: &fparkan_prototype::PrototypeGraph,
    report: &fparkan_prototype::PrototypeGraphReport,
) -> Result<String, serde_json::Error> {
    serde_json::to_string(&PrototypeInspectOutput {
        schema_version: PROTOTYPE_INSPECT_SCHEMA,
        key: key.to_string(),
        roots: report.root_count,
        node_count: graph.nodes.len(),
        edge_count: graph.edges.len(),
        prototype_requests: graph.prototype_requests.len(),
        resolved: report.resolved_count,
        unit_references: report.unit_reference_count,
        unit_components: report.unit_component_count,
        direct_references: report.direct_reference_count,
        wear_requests: report.wear_request_count,
        wear: report.wear_resolved_count,
        materials: report.material_resolved_count,
        textures: report.texture_resolved_count,
        lightmaps: report.lightmap_resolved_count,
        is_success: report.is_success(),
        failures: report
            .failures
            .iter()
            .take(16)
            .map(graph_failure_output)
            .collect(),
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
    println!(
        "{}",
        serialize_json(&MissionGraphOutput {
            schema_version: MISSION_GRAPH_SCHEMA,
            mission: mission.clone(),
            objects: loaded.object_count,
            paths: loaded.path_count,
            clans: loaded.clan_count,
            extras: loaded.extra_count,
            roots: loaded.graph_root_count,
            node_count: loaded.graph_node_count,
            edge_count: loaded.graph_edge_count,
            direct_references: loaded.graph_direct_reference_count,
            unit_references: loaded.graph_unit_reference_count,
            unit_components: loaded.graph_unit_component_count,
            prototype_requests: loaded.graph_resolved_count,
            wear_requests: loaded.graph_wear_request_count,
            wear: loaded.graph_wear_resolved_count,
            materials: loaded.graph_material_resolved_count,
            textures: loaded.graph_texture_resolved_count,
            lightmaps: loaded.graph_lightmap_resolved_count,
            is_success: loaded.graph_failure_count == 0,
            failures: loaded.graph_failure_count,
        })?
    );
    Ok(())
}

fn inspect_archive(args: &[String]) -> Result<(), String> {
    let path = parse_archive_path(args)?;
    let inspection = inspect_archive_file(&path, 0).map_err(|err| err.clone())?;

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

fn graph_failure_output(failure: &fparkan_prototype::PrototypeGraphFailure) -> GraphFailureOutput {
    GraphFailureOutput {
        root_index: failure.root_index,
        edge: prototype_graph_edge_label(failure.edge),
        requiredness: prototype_graph_requiredness_label(failure.requiredness),
        message: failure.message.clone(),
        archive: failure
            .provenance
            .as_ref()
            .and_then(|provenance| provenance.archive.clone()),
        resource: failure.provenance.as_ref().and_then(|provenance| {
            provenance
                .resource
                .as_ref()
                .map(|raw| String::from_utf8_lossy(raw).into_owned())
        }),
    }
}

fn prototype_graph_edge_label(edge: fparkan_prototype::PrototypeGraphEdge) -> &'static str {
    match edge {
        fparkan_prototype::PrototypeGraphEdge::MissionToUnitDat => "mission_to_unit_dat",
        fparkan_prototype::PrototypeGraphEdge::MissionToObjectsRegistry => {
            "mission_to_objects_registry"
        }
        fparkan_prototype::PrototypeGraphEdge::UnitDatToComponent => "unit_dat_to_component",
        fparkan_prototype::PrototypeGraphEdge::PrototypeToMesh => "prototype_to_mesh",
        fparkan_prototype::PrototypeGraphEdge::MeshToWear => "mesh_to_wear",
        fparkan_prototype::PrototypeGraphEdge::WearToMaterial => "wear_to_material",
        fparkan_prototype::PrototypeGraphEdge::MaterialToTexture => "material_to_texture",
        fparkan_prototype::PrototypeGraphEdge::WearToLightmap => "wear_to_lightmap",
    }
}

fn prototype_graph_requiredness_label(
    requiredness: fparkan_prototype::PrototypeGraphRequiredness,
) -> &'static str {
    match requiredness {
        fparkan_prototype::PrototypeGraphRequiredness::Required => "required",
        fparkan_prototype::PrototypeGraphRequiredness::Optional => "optional",
        fparkan_prototype::PrototypeGraphRequiredness::Fallback => "fallback",
    }
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

        let json = prototype_inspect_json("root", &graph, &report).expect("serialize");

        assert_eq!(
            json,
            "{\"schema_version\":\"fparkan-prototype-inspect-v1\",\"key\":\"root\",\"roots\":1,\"node_count\":0,\"edge_count\":0,\"prototype_requests\":1,\"resolved\":1,\"unit_references\":0,\"unit_components\":0,\"direct_references\":1,\"wear_requests\":0,\"wear\":0,\"materials\":0,\"textures\":0,\"lightmaps\":0,\"is_success\":true,\"failures\":[]}"
        );
    }

    #[test]
    fn mission_graph_json_has_canonical_field_order() {
        let json = serialize_json(&MissionGraphOutput {
            schema_version: MISSION_GRAPH_SCHEMA,
            mission: "MISSIONS/Autodemo.00/data.tma".to_string(),
            objects: 2,
            paths: 3,
            clans: 4,
            extras: 5,
            roots: 6,
            node_count: 7,
            edge_count: 8,
            direct_references: 9,
            unit_references: 10,
            unit_components: 11,
            prototype_requests: 12,
            wear_requests: 13,
            wear: 14,
            materials: 15,
            textures: 16,
            lightmaps: 17,
            is_success: true,
            failures: 0,
        })
        .expect("serialize");

        assert_eq!(
            json,
            "{\"schema_version\":\"fparkan-mission-graph-v1\",\"mission\":\"MISSIONS/Autodemo.00/data.tma\",\"objects\":2,\"paths\":3,\"clans\":4,\"extras\":5,\"roots\":6,\"node_count\":7,\"edge_count\":8,\"direct_references\":9,\"unit_references\":10,\"unit_components\":11,\"prototype_requests\":12,\"wear_requests\":13,\"wear\":14,\"materials\":15,\"textures\":16,\"lightmaps\":17,\"is_success\":true,\"failures\":0}"
        );
    }
}
