#![forbid(unsafe_code)]
#![allow(clippy::print_stderr, clippy::print_stdout)]
//! `FParkan` asset viewer composition root.

use fparkan_msh::{decode_msh, validate_msh};
use fparkan_nres::{decode as decode_nres, ReadProfile as NresReadProfile};
use fparkan_render::{
    build_commands, CameraSnapshot, DrawId, GpuMaterialId, GpuMeshId, IndexRange, RenderPhase,
    RenderProfile, RenderSnapshot, RenderSnapshotDraw,
};
use fparkan_resource::{archive_path, resource_name, CachedResourceRepository, ResourceRepository};
use fparkan_terrain_format::{decode_land_map, decode_land_msh};
use fparkan_texm::decode_texm;
use fparkan_vfs::DirectoryVfs;
use std::fmt::Write;
use std::path::PathBuf;
use std::sync::Arc;

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
    let bytes = std::fs::read(&file).map_err(|err| format!("{}: {err}", file.display()))?;
    if bytes.starts_with(b"NRes") {
        let document = decode_nres(
            Arc::from(bytes.into_boxed_slice()),
            NresReadProfile::Compatible,
        )
        .map_err(|err| err.to_string())?;
        let sample = render_nres_entries(&document, limit);
        return Ok(format!(
            "{{\"kind\":\"NRes\",\"path\":{},\"entries\":{},\"lookup_order_valid\":{},\"sample\":[{}]}}",
            json_string(&file.display().to_string()),
            document.entries().len(),
            document.lookup_order_valid(),
            sample
        ));
    }
    if bytes.get(0..4) == Some(b"NL\0\x01") {
        let document = fparkan_rsli::decode(
            Arc::from(bytes.into_boxed_slice()),
            fparkan_rsli::ReadProfile::Compatible,
        )
        .map_err(|err| err.to_string())?;
        return Ok(format!(
            "{{\"kind\":\"RsLi\",\"path\":{},\"entries\":{}}}",
            json_string(&file.display().to_string()),
            document.entries().len()
        ));
    }
    Err(format!("{}: unsupported archive magic", file.display()))
}

fn inspect_model(args: &[String]) -> Result<String, String> {
    if let Some(fixture) = parse_option(args, &["--fixture"]) {
        return ViewerModelService::inspect_synthetic_model(&fixture);
    }

    let query = parse_resource_query(args)?;
    let bytes = read_resource(&query)?;
    let nested = decode_nres(bytes, NresReadProfile::Compatible).map_err(|err| err.to_string())?;
    let document = decode_msh(&nested).map_err(|err| err.to_string())?;
    let model = validate_msh(&document).map_err(|err| err.to_string())?;

    Ok(format!(
        "{{\"kind\":\"model\",\"archive\":{},\"name\":{},\"streams\":{},\"nodes\":{},\"slots\":{},\"positions\":{},\"indices\":{},\"batches\":{}}}",
        json_string(&query.archive),
        json_string(&query.name),
        document.streams().len(),
        model.node_count,
        model.slots.len(),
        model.positions.len(),
        model.indices.len(),
        model.batches.len()
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
            "{{\"kind\":\"model\",\"fixture\":{},\"service\":\"synthetic-model\",\"draw_commands\":{draw_commands}}}",
            json_string(fixture)
        ))
    }
}

fn inspect_texture(args: &[String]) -> Result<String, String> {
    let query = parse_resource_query(args)?;
    let document = decode_texm(read_resource(&query)?).map_err(|err| err.to_string())?;

    Ok(format!(
        "{{\"kind\":\"texture\",\"archive\":{},\"name\":{},\"width\":{},\"height\":{},\"format\":{},\"mips\":{},\"pages\":{}}}",
        json_string(&query.archive),
        json_string(&query.name),
        document.width(),
        document.height(),
        json_string(&format!("{:?}", document.format())),
        document.mip_count(),
        document.page_rects().len()
    ))
}

fn inspect_map(args: &[String]) -> Result<String, String> {
    let file = parse_file(args)?;
    let kind = parse_option(args, &["--kind"]).ok_or_else(|| "missing --kind".to_string())?;
    let bytes = std::fs::read(&file).map_err(|err| format!("{}: {err}", file.display()))?;
    let nres = decode_nres(
        Arc::from(bytes.into_boxed_slice()),
        NresReadProfile::Compatible,
    )
    .map_err(|err| err.to_string())?;

    match kind.as_str() {
        "land-msh" => {
            let land = decode_land_msh(&nres).map_err(|err| err.to_string())?;
            Ok(format!(
                "{{\"kind\":\"land-msh\",\"path\":{},\"streams\":{},\"positions\":{},\"faces\":{},\"slots\":{}}}",
                json_string(&file.display().to_string()),
                land.streams.len(),
                land.positions.len(),
                land.faces.len(),
                land.slots.slots_raw.len()
            ))
        }
        "land-map" => {
            let land = decode_land_map(&nres).map_err(|err| err.to_string())?;
            Ok(format!(
                "{{\"kind\":\"land-map\",\"path\":{},\"areals\":{},\"declared_areals\":{},\"grid_width\":{},\"grid_height\":{}}}",
                json_string(&file.display().to_string()),
                land.areals.len(),
                land.areal_count,
                land.grid.cells_x,
                land.grid.cells_y
            ))
        }
        _ => Err(format!("unknown map kind: {kind}")),
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

fn read_resource(query: &ResourceQuery) -> Result<Arc<[u8]>, String> {
    let repository = CachedResourceRepository::new(Arc::new(DirectoryVfs::new(&query.root)));
    let archive = repository
        .open_archive(&archive_path(query.archive.as_bytes()).map_err(|err| err.to_string())?)
        .map_err(|err| err.to_string())?;
    let entry = repository
        .find(archive, &resource_name(query.name.as_bytes()))
        .map_err(|err| err.to_string())?
        .ok_or_else(|| format!("resource not found: {}/{}", query.archive, query.name))?;
    let bytes = repository.read(entry).map_err(|err| err.to_string())?;
    Ok(Arc::from(bytes.into_owned()))
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

fn render_nres_entries(document: &fparkan_nres::NresDocument, limit: usize) -> String {
    let mut out = String::new();
    for (index, entry) in document.entries().iter().take(limit).enumerate() {
        if index > 0 {
            out.push(',');
        }
        let name = String::from_utf8_lossy(entry.name_bytes());
        let _ = write!(
            out,
            "{{\"name\":{},\"type\":{},\"size\":{}}}",
            json_string(&name),
            entry.meta().type_id,
            entry.meta().data_size
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
                let _ = write!(out, "\\u{:04x}", u32::from(c));
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
            "{\"kind\":\"model\",\"fixture\":\"synthetic/model-basic\",\"service\":\"synthetic-model\",\"draw_commands\":1}"
        );
        Ok(())
    }
}
