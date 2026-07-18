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

use fparkan_assets::{
    decode_mission_payload, extend_graph_report_with_visual_dependencies, TmaProfile,
};
use fparkan_corpus::{discover, render_report_json, report, DiscoverOptions};
use fparkan_inspection::{
    inspect_archive_file, inspect_land_msh_bounds_file, inspect_model_from_root,
    inspect_wear_from_root, load_land_msh_from_path, ArchiveInspection, ModelInspection,
};
use fparkan_path::{normalize_relative, PathPolicy};
use fparkan_prototype::build_prototype_graph_report;
use fparkan_resource::{resource_name, CachedResourceRepository};
use fparkan_runtime::{
    create, load_mission, EngineConfig, EngineMode, EngineServices, MissionRequest,
};
use fparkan_vfs::{DirectoryVfs, Vfs};
use serde::Serialize;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

const ARCHIVE_INSPECT_SCHEMA: &str = "fparkan-archive-inspect-v1";
const PROTOTYPE_INSPECT_SCHEMA: &str = "fparkan-prototype-inspect-v2";
const MISSION_GRAPH_SCHEMA: &str = "fparkan-mission-graph-v1";
const MISSION_INSPECT_SCHEMA: &str = "fparkan-mission-inspect-v1";
const TERRAIN_INSPECT_SCHEMA: &str = "fparkan-terrain-inspect-v1";
const MODEL_INSPECT_SCHEMA: &str = "fparkan-model-inspect-v1";
const WEAR_INSPECT_SCHEMA: &str = "fparkan-wear-inspect-v1";
const SCRIPT_INSPECT_SCHEMA: &str = "fparkan-script-inspect-v2";

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
    unit_component_records: Vec<UnitComponentInspectOutput>,
    edges: Vec<PrototypeGraphEdgeInspectOutput>,
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
struct MissionInspectOutput {
    schema_version: &'static str,
    mission: String,
    objects: Vec<MissionObjectInspectOutput>,
}

#[derive(Serialize)]
struct MissionObjectInspectOutput {
    index: usize,
    resource: String,
    position: [f32; 3],
    orientation_raw: [f32; 3],
    scale: [f32; 3],
}

#[derive(Serialize)]
struct TerrainInspectOutput {
    schema_version: &'static str,
    path: String,
    positions: usize,
    min: [f32; 3],
    max: [f32; 3],
    faces: usize,
    slots: usize,
    material_tags: Vec<TerrainMaterialTagCount>,
    shade_pairs: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    shade_lookup_key_min: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    shade_lookup_key_max: Option<u16>,
    shade_batch_boundaries: usize,
}

#[derive(Serialize)]
struct TerrainMaterialTagCount {
    tag: u16,
    faces: usize,
}

#[derive(Serialize)]
struct WearInspectOutput {
    schema_version: &'static str,
    archive: String,
    resource: String,
    materials: usize,
    lightmaps: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    first_material: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_material: Option<String>,
}

#[derive(Serialize)]
struct ModelInspectOutput<'a> {
    schema_version: &'static str,
    archive: &'a str,
    resource: &'a str,
    streams: usize,
    nodes: usize,
    node_stride: usize,
    slots: usize,
    positions: usize,
    indices: usize,
    batches: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    animation_keys: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    animation_frame_count: Option<u32>,
    node38: Vec<ModelNodeInspectOutput>,
}

#[derive(Serialize)]
struct ModelNodeInspectOutput {
    index: usize,
    parent_or_link_raw: u16,
    anim_map_start: u16,
    fallback_key: u16,
    has_lod0_group0: bool,
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

#[derive(Serialize)]
struct UnitComponentInspectOutput {
    root_index: usize,
    component_index: usize,
    archive_raw_hex: String,
    resource_raw_hex: String,
    kind: u32,
    parent_or_link: i32,
    description_raw_hex: String,
    tail0: u32,
    tail1: u32,
}

#[derive(Serialize)]
struct PrototypeGraphEdgeInspectOutput {
    id: u32,
    from: u32,
    to: u32,
    kind: &'static str,
    requiredness: &'static str,
    root_index: Option<usize>,
    parent_edge: Option<u32>,
    unit_component_index: Option<usize>,
    archive: Option<String>,
    resource_raw_hex: Option<String>,
}

#[derive(Serialize)]
struct ScriptInspectOutput<'a> {
    schema_version: &'static str,
    path: &'a str,
    opcode_handler_count: u32,
    events: usize,
    instructions: usize,
    references: usize,
    trailing_bytes: usize,
    first_header_word_candidates: Vec<ScriptHeaderWordCount>,
}

#[derive(Serialize)]
struct ScriptHeaderWordCount {
    value: u32,
    instructions: usize,
}

#[derive(Serialize)]
struct VarSetInspectOutput<'a> {
    schema_version: &'static str,
    path: &'a str,
    declarations: usize,
    float_defaults: usize,
    dword_defaults: usize,
    first_name: Option<&'a str>,
    last_name: Option<&'a str>,
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
        [domain, command, rest @ ..] if domain == "mission" && command == "inspect" => {
            let rest = strip_format_json(rest)?;
            inspect_mission(&rest)
        }
        [domain, command, rest @ ..] if domain == "terrain" && command == "inspect" => {
            let rest = strip_format_json(rest)?;
            inspect_terrain(&rest)
        }
        [domain, command, rest @ ..] if domain == "wear" && command == "inspect" => {
            let rest = strip_format_json(rest)?;
            inspect_wear(&rest)
        }
        [domain, command, rest @ ..] if domain == "model" && command == "inspect" => {
            let rest = strip_format_json(rest)?;
            inspect_model(&rest)
        }
        [domain, command, rest @ ..] if domain == "script" && command == "inspect" => {
            let rest = strip_format_json(rest)?;
            inspect_script(&rest)
        }
        [domain, command, rest @ ..] if domain == "varset" && command == "inspect" => {
            let rest = strip_format_json(rest)?;
            inspect_varset(&rest)
        }
        _ => Err(usage()),
    }
}

fn inspect_script(args: &[String]) -> Result<(), String> {
    let path = parse_file_path(args, "script inspect")?;
    let bytes = std::fs::read(&path).map_err(|err| format!("{}: {err}", path.display()))?;
    let package =
        fparkan_script::decode(&bytes).map_err(|err| format!("{}: {err}", path.display()))?;
    let display_path = path.display().to_string();
    println!("{}", script_inspect_json(&display_path, &package)?);
    Ok(())
}

fn script_inspect_json(
    path: &str,
    package: &fparkan_script::ScriptPackage,
) -> Result<String, String> {
    let instructions = package
        .events
        .iter()
        .map(|event| event.instructions.len())
        .sum();
    let references = package
        .events
        .iter()
        .flat_map(|event| &event.instructions)
        .map(|instruction| instruction.references.len())
        .sum();
    let mut candidates = std::collections::BTreeMap::<u32, usize>::new();
    for instruction in package.events.iter().flat_map(|event| &event.instructions) {
        *candidates.entry(instruction.header_words[0]).or_insert(0) += 1;
    }
    serialize_json(&ScriptInspectOutput {
        schema_version: SCRIPT_INSPECT_SCHEMA,
        path,
        opcode_handler_count: package.opcode_handler_count,
        events: package.events.len(),
        instructions,
        references,
        trailing_bytes: package.trailing_bytes.len(),
        first_header_word_candidates: candidates
            .into_iter()
            .map(|(value, instructions)| ScriptHeaderWordCount {
                value,
                instructions,
            })
            .collect(),
    })
}

fn inspect_varset(args: &[String]) -> Result<(), String> {
    let path = parse_file_path(args, "varset inspect")?;
    let bytes = std::fs::read(&path).map_err(|err| format!("{}: {err}", path.display()))?;
    let varset =
        fparkan_script::parse_varset(&bytes).map_err(|err| format!("{}: {err}", path.display()))?;
    let display_path = path.display().to_string();
    println!("{}", varset_inspect_json(&display_path, &varset)?);
    Ok(())
}

fn varset_inspect_json(path: &str, varset: &fparkan_script::VarSet) -> Result<String, String> {
    let float_defaults = varset
        .declarations
        .iter()
        .filter(|declaration| declaration.type_name == fparkan_script::VarSetType::Float)
        .count();
    let dword_defaults = varset.declarations.len() - float_defaults;
    serialize_json(&VarSetInspectOutput {
        schema_version: "fparkan-varset-inspect-v1",
        path,
        declarations: varset.declarations.len(),
        float_defaults,
        dword_defaults,
        first_name: varset
            .declarations
            .first()
            .map(|declaration| declaration.name.as_str()),
        last_name: varset
            .declarations
            .last()
            .map(|declaration| declaration.name.as_str()),
    })
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
        unit_component_records: graph
            .root_unit_components
            .iter()
            .enumerate()
            .flat_map(|(root_index, records)| {
                records
                    .iter()
                    .enumerate()
                    .map(
                        move |(component_index, record)| UnitComponentInspectOutput {
                            root_index,
                            component_index,
                            archive_raw_hex: hex_bytes(&record.archive_raw),
                            resource_raw_hex: hex_bytes(&record.resource_raw),
                            kind: record.kind,
                            parent_or_link: record.parent_or_link,
                            description_raw_hex: hex_bytes(&record.description_raw),
                            tail0: record.tail0,
                            tail1: record.tail1,
                        },
                    )
            })
            .collect(),
        edges: graph
            .edges
            .iter()
            .map(|edge| PrototypeGraphEdgeInspectOutput {
                id: edge.id.0,
                from: edge.from.0,
                to: edge.to.0,
                kind: prototype_graph_edge_kind_label(edge.kind),
                requiredness: prototype_graph_requiredness_label(edge.requiredness),
                root_index: edge.provenance.as_ref().map(|value| value.root_index),
                parent_edge: edge
                    .provenance
                    .as_ref()
                    .and_then(|value| value.parent_edge.map(|parent| parent.0)),
                unit_component_index: edge
                    .provenance
                    .as_ref()
                    .and_then(|value| value.unit_component_index),
                archive: edge
                    .provenance
                    .as_ref()
                    .and_then(|value| value.archive.clone()),
                resource_raw_hex: edge
                    .provenance
                    .as_ref()
                    .and_then(|value| value.resource.as_ref())
                    .map(|raw| hex_bytes(raw)),
            })
            .collect(),
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

fn inspect_mission(args: &[String]) -> Result<(), String> {
    let root = parse_root_alias(args)?;
    let mission = parse_required(args, &["--mission"], "--mission")?;
    let mission_path = normalize_relative(mission.as_bytes(), PathPolicy::StrictLegacy)
        .map_err(|err| err.to_string())?;
    let vfs = DirectoryVfs::new(root);
    let bytes = vfs.read(&mission_path).map_err(|err| err.to_string())?;
    let document =
        decode_mission_payload(bytes, TmaProfile::Strict).map_err(|err| err.to_string())?;
    let objects = document
        .objects
        .iter()
        .enumerate()
        .map(|(index, object)| MissionObjectInspectOutput {
            index,
            resource: String::from_utf8_lossy(&object.resource_name.raw).into_owned(),
            position: object.position,
            orientation_raw: object.orientation,
            scale: object.scale,
        })
        .collect();
    println!(
        "{}",
        serialize_json(&MissionInspectOutput {
            schema_version: MISSION_INSPECT_SCHEMA,
            mission,
            objects,
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

fn inspect_terrain(args: &[String]) -> Result<(), String> {
    let path = parse_file_path(args, "terrain inspect")?;
    let bounds = inspect_land_msh_bounds_file(&path)?;
    let terrain = load_land_msh_from_path(&path)?;
    let mut material_tags = terrain
        .faces
        .iter()
        .fold(std::collections::BTreeMap::new(), |mut counts, face| {
            *counts.entry(face.material_tag).or_insert(0usize) += 1;
            counts
        })
        .into_iter()
        .map(|(tag, faces)| TerrainMaterialTagCount { tag, faces })
        .collect::<Vec<_>>();
    material_tags.sort_unstable_by_key(|entry| entry.tag);
    let shade_pairs = (0..terrain.slots.slots_raw.len())
        .filter_map(|slot_index| terrain.slot_material_pairs(slot_index))
        .flatten()
        .collect::<Vec<_>>();
    let shade_lookup_key_min = shade_pairs.iter().map(|pair| pair.shade_lookup_key()).min();
    let shade_lookup_key_max = shade_pairs.iter().map(|pair| pair.shade_lookup_key()).max();
    let shade_batch_boundaries = shade_pairs
        .iter()
        .filter(|pair| pair.flags & 0x0010 != 0)
        .count();
    println!(
        "{}",
        serialize_json(&TerrainInspectOutput {
            schema_version: TERRAIN_INSPECT_SCHEMA,
            path: path.display().to_string(),
            positions: bounds.positions,
            min: bounds.min,
            max: bounds.max,
            faces: terrain.faces.len(),
            slots: terrain.slots.slots_raw.len(),
            material_tags,
            shade_pairs: shade_pairs.len(),
            shade_lookup_key_min,
            shade_lookup_key_max,
            shade_batch_boundaries,
        })?
    );
    Ok(())
}

fn inspect_wear(args: &[String]) -> Result<(), String> {
    let root = parse_root_alias(args)?;
    let archive = parse_required(args, &["--archive"], "--archive")?;
    let resource = parse_required(args, &["--resource"], "--resource")?;
    let inspection = inspect_wear_from_root(&root, &archive, &resource)?;
    println!(
        "{}",
        serialize_json(&WearInspectOutput {
            schema_version: WEAR_INSPECT_SCHEMA,
            archive,
            resource,
            materials: inspection.materials,
            lightmaps: inspection.lightmaps,
            first_material: inspection.first_material,
            last_material: inspection.last_material,
        })?
    );
    Ok(())
}

fn inspect_model(args: &[String]) -> Result<(), String> {
    let root = parse_root_alias(args)?;
    let archive = parse_required(args, &["--archive"], "--archive")?;
    let resource = parse_required(args, &["--resource"], "--resource")?;
    let inspection = inspect_model_from_root(&root, &archive, &resource)?;
    println!("{}", model_inspect_json(&archive, &resource, &inspection)?);
    Ok(())
}

fn model_inspect_json(
    archive: &str,
    resource: &str,
    inspection: &ModelInspection,
) -> Result<String, String> {
    serialize_json(&ModelInspectOutput {
        schema_version: MODEL_INSPECT_SCHEMA,
        archive,
        resource,
        streams: inspection.streams,
        nodes: inspection.nodes,
        node_stride: inspection.node_stride,
        slots: inspection.slots,
        positions: inspection.positions,
        indices: inspection.indices,
        batches: inspection.batches,
        animation_keys: inspection.animation_keys,
        animation_frame_count: inspection.animation_frame_count,
        node38: inspection
            .node38
            .iter()
            .map(|node| ModelNodeInspectOutput {
                index: node.index,
                parent_or_link_raw: node.parent_or_link_raw,
                anim_map_start: node.anim_map_start,
                fallback_key: node.fallback_key,
                has_lod0_group0: node.has_lod0_group0,
            })
            .collect(),
    })
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
    parse_file_path(args, "archive inspect")
}

fn parse_file_path(args: &[String], command: &str) -> Result<PathBuf, String> {
    match args {
        [path] => Ok(PathBuf::from(path)),
        [flag, path] if flag == "--file" => Ok(PathBuf::from(path)),
        _ => Err(format!("{command} requires <file> or --file <file>")),
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

fn hex_bytes(raw: &[u8]) -> String {
    let mut output = String::with_capacity(raw.len().saturating_mul(2));
    for byte in raw {
        #[allow(clippy::expect_used)]
        write!(&mut output, "{byte:02x}").expect("writing into String cannot fail");
    }
    output
}

fn prototype_graph_edge_kind_label(
    edge: fparkan_prototype::PrototypeGraphEdgeKind,
) -> &'static str {
    match edge {
        fparkan_prototype::PrototypeGraphEdgeKind::MissionToRoot => "mission_to_root",
        fparkan_prototype::PrototypeGraphEdgeKind::UnitDatToComponent => "unit_dat_to_component",
        fparkan_prototype::PrototypeGraphEdgeKind::PrototypeToMesh => "prototype_to_mesh",
        fparkan_prototype::PrototypeGraphEdgeKind::MeshToWear => "mesh_to_wear",
        fparkan_prototype::PrototypeGraphEdgeKind::WearToMaterial => "wear_to_material",
        fparkan_prototype::PrototypeGraphEdgeKind::MaterialToTexture => "material_to_texture",
        fparkan_prototype::PrototypeGraphEdgeKind::WearToLightmap => "wear_to_lightmap",
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
    "usage: fparkan corpus discover|validate --root <path> [--format json] | archive inspect <file> [--format json] | terrain inspect <Land.msh> [--format json] | wear inspect --root <path> --archive <archive> --resource <wear.wea> [--format json] | model inspect --root <path> --archive <archive> --resource <model.msh> [--format json] | script inspect <file> [--format json] | varset inspect <file> [--format json] | prototype inspect --root <path> --key <key> [--format json] | mission graph|inspect --root <path> --mission <path> [--format json]".to_string()
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
    fn script_inspect_json_has_canonical_field_order() {
        let package =
            fparkan_script::decode(&[73, 0, 0, 0, 0, 0, 0, 0]).expect("minimal script package");
        assert_eq!(
            script_inspect_json("script.scr", &package),
            Ok("{\"schema_version\":\"fparkan-script-inspect-v2\",\"path\":\"script.scr\",\"opcode_handler_count\":73,\"events\":0,\"instructions\":0,\"references\":0,\"trailing_bytes\":0,\"first_header_word_candidates\":[]}".to_string())
        );
    }

    #[test]
    fn varset_inspect_json_has_canonical_field_order() {
        let varset =
            fparkan_script::parse_varset(b"VAR( float, f0, 0)\nVAR( DWORD, d0, 0xffffffff)\n")
                .expect("minimal varset");
        assert_eq!(
            varset_inspect_json("varset.var", &varset),
            Ok("{\"schema_version\":\"fparkan-varset-inspect-v1\",\"path\":\"varset.var\",\"declarations\":2,\"float_defaults\":1,\"dword_defaults\":1,\"first_name\":\"f0\",\"last_name\":\"d0\"}".to_string())
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
            "{\"schema_version\":\"fparkan-prototype-inspect-v2\",\"key\":\"root\",\"roots\":1,\"node_count\":0,\"edge_count\":0,\"unit_component_records\":[],\"edges\":[],\"prototype_requests\":1,\"resolved\":1,\"unit_references\":0,\"unit_components\":0,\"direct_references\":1,\"wear_requests\":0,\"wear\":0,\"materials\":0,\"textures\":0,\"lightmaps\":0,\"is_success\":true,\"failures\":[]}"
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

    #[test]
    fn mission_inspect_output_retains_raw_transform_fields() {
        let json = serialize_json(&MissionInspectOutput {
            schema_version: MISSION_INSPECT_SCHEMA,
            mission: "MISSIONS/test/data.tma".to_string(),
            objects: vec![MissionObjectInspectOutput {
                index: 1,
                resource: "unit.dat".to_string(),
                position: [1.0, 2.0, 3.0],
                orientation_raw: [4.0, 5.0, 6.0],
                scale: [7.0, 8.0, 9.0],
            }],
        })
        .expect("serialize mission inspection");

        assert_eq!(
            json,
            "{\"schema_version\":\"fparkan-mission-inspect-v1\",\"mission\":\"MISSIONS/test/data.tma\",\"objects\":[{\"index\":1,\"resource\":\"unit.dat\",\"position\":[1.0,2.0,3.0],\"orientation_raw\":[4.0,5.0,6.0],\"scale\":[7.0,8.0,9.0]}]}"
        );
    }

    #[test]
    fn terrain_inspect_output_retains_axis_bounds() {
        let json = serialize_json(&TerrainInspectOutput {
            schema_version: TERRAIN_INSPECT_SCHEMA,
            path: "DATA/MAPS/AutoMAP/Land.msh".to_string(),
            positions: 3,
            min: [-1.0, -2.0, -3.0],
            max: [4.0, 5.0, 6.0],
            faces: 2,
            slots: 4,
            material_tags: vec![
                TerrainMaterialTagCount { tag: 0, faces: 1 },
                TerrainMaterialTagCount { tag: 3, faces: 1 },
            ],
            shade_pairs: 2,
            shade_lookup_key_min: Some(1),
            shade_lookup_key_max: Some(2),
            shade_batch_boundaries: 1,
        })
        .expect("serialize terrain inspection");

        assert_eq!(
            json,
            "{\"schema_version\":\"fparkan-terrain-inspect-v1\",\"path\":\"DATA/MAPS/AutoMAP/Land.msh\",\"positions\":3,\"min\":[-1.0,-2.0,-3.0],\"max\":[4.0,5.0,6.0],\"faces\":2,\"slots\":4,\"material_tags\":[{\"tag\":0,\"faces\":1},{\"tag\":3,\"faces\":1}],\"shade_pairs\":2,\"shade_lookup_key_min\":1,\"shade_lookup_key_max\":2,\"shade_batch_boundaries\":1}"
        );
    }

    #[test]
    fn wear_inspect_output_retains_material_bounds() {
        let json = serialize_json(&WearInspectOutput {
            schema_version: WEAR_INSPECT_SCHEMA,
            archive: "system.rlb".to_string(),
            resource: "SHADE.WEA".to_string(),
            materials: 1,
            lightmaps: 0,
            first_material: Some("LIGHT1".to_string()),
            last_material: Some("LIGHT1".to_string()),
        })
        .expect("serialize wear inspection");

        assert_eq!(
            json,
            "{\"schema_version\":\"fparkan-wear-inspect-v1\",\"archive\":\"system.rlb\",\"resource\":\"SHADE.WEA\",\"materials\":1,\"lightmaps\":0,\"first_material\":\"LIGHT1\",\"last_material\":\"LIGHT1\"}"
        );
    }
}
