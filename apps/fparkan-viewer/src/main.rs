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
//! `FParkan` asset inspection composition root.

use fparkan_inspection::{
    inspect_land_file, inspect_model_from_root, inspect_texture_from_root, ArchiveInspection,
    LandFileKind, MapInspection, NresEntrySummary,
};
use fparkan_render::{
    build_commands, CameraSnapshot, DrawId, GpuMaterialId, GpuMeshId, IndexRange, RenderPhase,
    RenderProfile, RenderSnapshot, RenderSnapshotDraw,
};
use std::fmt::Write;
use std::path::PathBuf;

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let code = match run(&args) {
        Ok(json) => {
            println!("{json}");
            0
        }
        Err(err) => {
            eprintln!("{err}");
            2
        }
    };
    std::process::exit(code);
}

fn run(args: &[String]) -> Result<String, String> {
    match args {
        [domain, rest @ ..] if domain == "archive" => inspect_archive(rest),
        [domain, rest @ ..] if domain == "model" => inspect_model(rest),
        [domain, rest @ ..] if domain == "texture" => inspect_texture(rest),
        [domain, rest @ ..] if domain == "map" => inspect_map(rest),
        _ => Err(usage()),
    }
}

fn inspect_archive(args: &[String]) -> Result<String, String> {
    let file = parse_file(args)?;
    let limit = parse_limit(args)?;
    let inspection = fparkan_inspection::inspect_archive_file(&file, limit)?;

    match inspection {
        ArchiveInspection::Nres {
            entries,
            lookup_order_valid,
            sample,
        } => Ok(format!(
            "{{\"report_kind\":\"archive-inspection\",\"kind\":\"NRes\",\"path\":{},\"entries\":{},\"lookup_order_valid\":{},\"sample\":[{}]}}",
            json_string(&file.display().to_string()),
            entries,
            lookup_order_valid,
            render_nres_entries(&sample)
        )),
        ArchiveInspection::Rsli { entries } => Ok(format!(
            "{{\"report_kind\":\"archive-inspection\",\"kind\":\"RsLi\",\"path\":{},\"entries\":{}}}",
            json_string(&file.display().to_string()),
            entries
        )),
        ArchiveInspection::Unsupported => Err(format!("{}: unsupported archive magic", file.display())),
    }
}

fn inspect_model(args: &[String]) -> Result<String, String> {
    if let Some(fixture) = parse_option(args, &["--fixture"]) {
        return ViewerModelService::inspect_synthetic_model(&fixture);
    }

    let query = parse_resource_query(args)?;
    let inspection = inspect_model_from_root(&query.root, &query.archive, &query.name)?;

    Ok(format!(
        "{{\"report_kind\":\"model-inspection\",\"kind\":\"model\",\"archive\":{},\"name\":{},\"streams\":{},\"nodes\":{},\"slots\":{},\"positions\":{},\"indices\":{},\"batches\":{}}}",
        json_string(&query.archive),
        json_string(&query.name),
        inspection.streams,
        inspection.nodes,
        inspection.slots,
        inspection.positions,
        inspection.indices,
        inspection.batches
    ))
}

#[derive(Clone, Debug)]
struct ViewerModelService;

impl ViewerModelService {
    fn inspect_synthetic_model(fixture: &str) -> Result<String, String> {
        if fixture != "synthetic/model-basic" {
            return Err(format!("unknown model fixture: {fixture}"));
        }

        let snapshot = RenderSnapshot {
            camera: CameraSnapshot::default(),
            draws: vec![RenderSnapshotDraw {
                id: DrawId(1),
                phase: RenderPhase::Opaque,
                object_id: None,
                mesh: GpuMeshId(1),
                material_slots: vec![GpuMaterialId(7)],
                material_index: 0,
                pipeline_state: fparkan_render::LegacyPipelineState::default(),
                transform: identity_transform(),
                range: IndexRange { start: 0, count: 3 },
                stable_order: 0,
            }],
        };
        let commands = build_commands(&snapshot, RenderProfile::default())
            .map_err(|err| format!("render command generation: {err}"))?;
        let draw_commands = commands
            .commands
            .iter()
            .filter(|command| matches!(command, fparkan_render::RenderCommand::Draw(_)))
            .count();

        Ok(format!(
            "{{\"report_kind\":\"model-inspection\",\"kind\":\"model\",\"fixture\":{},\"service\":\"synthetic-model-inspection\",\"draw_commands\":{draw_commands}}}",
            json_string(fixture)
        ))
    }
}

fn inspect_texture(args: &[String]) -> Result<String, String> {
    let query = parse_resource_query(args)?;
    let inspection = inspect_texture_from_root(&query.root, &query.archive, &query.name)?;

    Ok(format!(
        "{{\"report_kind\":\"texture-inspection\",\"kind\":\"texture\",\"archive\":{},\"name\":{},\"width\":{},\"height\":{},\"format\":{},\"mips\":{},\"pages\":{}}}",
        json_string(&query.archive),
        json_string(&query.name),
        inspection.width,
        inspection.height,
        json_string(&inspection.format),
        inspection.mips,
        inspection.pages
    ))
}

fn inspect_map(args: &[String]) -> Result<String, String> {
    let file = parse_file(args)?;
    let kind = parse_option(args, &["--kind"]).ok_or_else(|| "missing --kind".to_string())?;
    let inspection = inspect_land_file(
        &file,
        match kind.as_str() {
            "land-msh" => LandFileKind::LandMsh,
            "land-map" => LandFileKind::LandMap,
            _ => return Err(format!("unknown map kind: {kind}")),
        },
    )?;

    Ok(render_map_inspection_json(
        &file.display().to_string(),
        &kind,
        &inspection,
    ))
}

fn render_map_inspection_json(path: &str, kind: &str, inspection: &MapInspection) -> String {
    match kind {
        "land-msh" => format!(
            "{{\"report_kind\":\"map-inspection\",\"kind\":\"land-msh\",\"path\":{},\"streams\":{},\"positions\":{},\"faces\":{},\"slots\":{}}}",
            json_string(path),
            inspection.streams,
            inspection.positions,
            inspection.faces,
            inspection.slots
        ),
        "land-map" => format!(
            "{{\"report_kind\":\"map-inspection\",\"kind\":\"land-map\",\"path\":{},\"areals\":{},\"declared_areals\":{},\"grid_width\":{},\"grid_height\":{}}}",
            json_string(path),
            inspection.areals,
            inspection.declared_areals,
            inspection.grid_width,
            inspection.grid_height
        ),
        _ => unreachable!("invalid land kind: {kind}"),
    }
}

struct ResourceQuery {
    root: PathBuf,
    archive: String,
    name: String,
}

fn parse_resource_query(args: &[String]) -> Result<ResourceQuery, String> {
    Ok(ResourceQuery {
        root: parse_path_option(args, &["--root", "--game-root"], "--root")?,
        archive: parse_option(args, &["--archive"])
            .ok_or_else(|| "missing --archive".to_string())?,
        name: parse_option(args, &["--name"]).ok_or_else(|| "missing --name".to_string())?,
    })
}

fn parse_file(args: &[String]) -> Result<PathBuf, String> {
    parse_path_option(args, &["--file"], "--file")
}

fn parse_limit(args: &[String]) -> Result<usize, String> {
    parse_option(args, &["--limit"])
        .map(|value| {
            value
                .parse::<usize>()
                .map_err(|_| format!("invalid --limit: {value}"))
        })
        .transpose()
        .map(|value| value.unwrap_or(0))
}

fn render_nres_entries(entries: &[NresEntrySummary]) -> String {
    let mut out = String::new();
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let name = &entry.name;
        let _ = write!(
            out,
            "{{\"name\":{},\"type\":{},\"size\":{}}}",
            json_string(name),
            entry.type_id,
            entry.data_size
        );
    }
    out
}

fn parse_path_option(args: &[String], names: &[&str], label: &str) -> Result<PathBuf, String> {
    parse_option(args, names)
        .map(PathBuf::from)
        .ok_or_else(|| format!("missing {label}"))
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

fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn identity_transform() -> [f32; 16] {
    [
        1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ]
}

fn usage() -> String {
    "usage: fparkan-viewer archive --file <archive> [--limit N] | model --root <game-root> --archive <archive> --name <msh> | model --fixture synthetic/model-basic | texture --root <game-root> --archive <archive> --name <texm> | map --file <Land.msh|Land.map> --kind land-msh|land-map".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn parses_resource_query() -> Result<(), String> {
        let query = parse_resource_query(&strings(&[
            "--root",
            "testdata/IS",
            "--archive",
            "textures.lib",
            "--name",
            "grass.tex",
        ]))?;

        assert_eq!(query.root, PathBuf::from("testdata/IS"));
        assert_eq!(query.archive, "textures.lib");
        assert_eq!(query.name, "grass.tex");
        Ok(())
    }

    #[test]
    fn json_string_escapes_controls() {
        assert_eq!(json_string("a\"b\\c\n"), "\"a\\\"b\\\\c\\n\"");
    }

    #[test]
    fn usage_rejects_empty_args() {
        assert_eq!(run(&[]), Err(usage()));
    }

    #[test]
    fn parses_limit() {
        assert_eq!(parse_limit(&strings(&["--limit", "2"])), Ok(2));
        assert_eq!(parse_limit(&[]), Ok(0));
        assert_eq!(
            parse_limit(&strings(&["--limit", "x"])),
            Err("invalid --limit: x".to_string())
        );
    }

    #[test]
    fn model_fixture_uses_viewer_service_and_render_commands() -> Result<(), String> {
        assert_eq!(
            run(&strings(&["model", "--fixture", "synthetic/model-basic"]))?,
            "{\"report_kind\":\"model-inspection\",\"kind\":\"model\",\"fixture\":\"synthetic/model-basic\",\"service\":\"synthetic-model-inspection\",\"draw_commands\":1}"
        );
        Ok(())
    }
}
