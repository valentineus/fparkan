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
//! `FParkan` render-planning composition root.

use fparkan_assets::PreparedVisual;
use fparkan_platform::WindowPort;
use fparkan_platform_winit::WinitWindow;
use fparkan_render::{
    build_commands, CameraSnapshot, DrawId, GpuMaterialId, GpuMeshId, IndexRange, RenderBackend,
    RenderCommand, RenderCommandList, RenderPhase, RenderProfile, RenderSnapshot,
    RenderSnapshotDraw,
};
use fparkan_render_vulkan::VulkanPlanningBackend;
use fparkan_runtime::{
    create, frame, load_mission, loaded_mission_assets, EngineConfig, EngineMode, EngineServices,
    MissionAssets, MissionRequest,
};
use fparkan_vfs::DirectoryVfs;
use fparkan_world::WorldSnapshot;
use std::path::PathBuf;
use std::sync::Arc;

fn main() {
    let raw_args = std::env::args().skip(1).collect::<Vec<_>>();
    let code = match run(&raw_args) {
        Ok(output) => {
            println!("{output}");
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
    let args = Args::parse(args)?;
    let services = EngineServices::new(Arc::new(DirectoryVfs::new(&args.root)));
    let mut engine = create(
        EngineConfig {
            mode: EngineMode::Rendered,
        },
        services,
    )
    .map_err(|err| err.to_string())?;
    let loaded = load_mission(
        &mut engine,
        MissionRequest {
            key: args.mission.clone(),
        },
    )
    .map_err(|err| err.to_string())?;

    let mut backend = VulkanPlanningBackend::new();
    let _request = WinitWindow::default_render_request();
    let window = WinitWindow::synthetic(1280, 720);
    let _ = window.drawable_size();
    let _ = window.handle();
    let mut last_draw_count = 0usize;
    let mut last_tick = 0u64;
    let mut last_hash = [0u8; 32];
    for _ in 0..args.frames {
        let result = frame(&mut engine).map_err(|err| err.to_string())?;
        last_tick = result.snapshot.tick.0;
        last_hash = result.snapshot.hash.0;
        let mission_assets = loaded_mission_assets(&engine);
        let commands = render_snapshot_commands_with_assets(&result.snapshot, mission_assets)
            .map_err(|err| format!("render snapshot: {err}"))?;
        last_draw_count = commands
            .commands
            .iter()
            .filter(|command| matches!(command, RenderCommand::Draw(_)))
            .count();
        backend
            .execute(&commands)
            .map_err(|err| format!("render backend: {err}"))?;
    }

    let capture_report = backend.report();

    Ok(format!(
        "{{\"report_kind\":\"render-planning\",\"backend\":\"vulkan-planning\",\"window\":\"synthetic\",\"mission\":{},\"objects\":{},\"frames\":{},\"tick\":{},\"draws\":{},\"submission_plans\":{},\"last_command_capture_bytes\":{},\"hash\":{}}}",
        json_string(&args.mission),
        loaded.object_count,
        args.frames,
        last_tick,
        last_draw_count,
        capture_report.execution.submission_plans,
        capture_report.execution.last_capture_size,
        json_hash(&last_hash)
    ))
}

#[cfg(test)]
fn render_snapshot_commands(snapshot: &WorldSnapshot) -> Result<RenderCommandList, String> {
    render_snapshot_commands_with_assets(snapshot, None)
}

fn render_snapshot_commands_with_assets(
    snapshot: &WorldSnapshot,
    mission_assets: Option<&MissionAssets>,
) -> Result<RenderCommandList, String> {
    let render_snapshot = render_snapshot_with_assets(snapshot, mission_assets);
    build_commands(&render_snapshot, RenderProfile::default()).map_err(|err| err.to_string())
}

fn render_snapshot_with_assets(
    snapshot: &WorldSnapshot,
    mission_assets: Option<&MissionAssets>,
) -> RenderSnapshot {
    let mut draws = Vec::with_capacity(snapshot.objects.len());
    for (index, handle) in snapshot.objects.iter().enumerate() {
        let stable_order = u64::from(handle.slot);
        let prepared = mission_assets.and_then(|assets| {
            assets
                .visual_for_object(index)
                .and_then(|visual_id| assets.visual_by_id(visual_id))
        });
        let mesh = if let Some(visual) = prepared {
            visual.mesh.as_ref().map_or_else(
                || GpuMeshId(u64::from(handle.slot) + 1),
                |_| GpuMeshId(visual.id.raw()),
            )
        } else {
            GpuMeshId(u64::from(handle.slot) + 1)
        };
        let material = prepared
            .and_then(PreparedVisual::primary_material_id)
            .map_or(GpuMaterialId(1), |material_id| {
                GpuMaterialId(material_id.raw())
            });
        let draw_id = snapshot
            .tick
            .0
            .wrapping_mul(1_000_003)
            .wrapping_add(stable_order);
        draws.push(RenderSnapshotDraw {
            id: DrawId(draw_id),
            phase: RenderPhase::Opaque,
            object_id: None,
            mesh,
            material_slots: vec![material],
            material_index: 0,
            transform: identity_transform(index_to_f32(index)),
            range: IndexRange { start: 0, count: 3 },
            stable_order,
        });
    }
    RenderSnapshot {
        camera: CameraSnapshot::default(),
        draws,
    }
}

fn identity_transform(x: f32) -> [f32; 16] {
    [
        1.0, 0.0, 0.0, x, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ]
}

fn index_to_f32(index: usize) -> f32 {
    u16::try_from(index).map_or(f32::from(u16::MAX), f32::from)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Args {
    root: PathBuf,
    mission: String,
    frames: u64,
}

impl Args {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut root = None;
        let mut mission = None;
        let mut frames = 1;
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--root" => {
                    root = Some(
                        iter.next()
                            .map(PathBuf::from)
                            .ok_or_else(|| "--root requires a path".to_string())?,
                    );
                }
                "--mission" => {
                    mission = Some(
                        iter.next()
                            .cloned()
                            .ok_or_else(|| "--mission requires a path".to_string())?,
                    );
                }
                "--frames" => {
                    frames = iter
                        .next()
                        .ok_or_else(|| "--frames requires a value".to_string())?
                        .parse()
                        .map_err(|_| "--frames must be an integer".to_string())?;
                }
                _ => return Err(usage()),
            }
        }
        let root = root.ok_or_else(|| "missing --root".to_string())?;
        let mission = mission.ok_or_else(|| "missing --mission".to_string())?;
        if frames == 0 {
            return Err("--frames must be greater than zero".to_string());
        }
        Ok(Self {
            root,
            mission,
            frames,
        })
    }
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
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn json_hash(hash: &[u8; 32]) -> String {
    let mut out = String::from("\"");
    for byte in hash {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out.push('"');
    out
}

fn usage() -> String {
    "usage: fparkan-game --root <path> --mission <path> [--frames <n>]".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_world::{ObjectHandle, StateHash, Tick};
    use std::path::Path;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn parses_required_args() {
        assert_eq!(
            Args::parse(&strings(&[
                "--root",
                "testdata/IS",
                "--mission",
                "MISSIONS/Autodemo.00/data.tma",
                "--frames",
                "3",
            ])),
            Ok(Args {
                root: PathBuf::from("testdata/IS"),
                mission: "MISSIONS/Autodemo.00/data.tma".to_string(),
                frames: 3,
            })
        );
    }

    #[test]
    fn render_commands_follow_snapshot_order() -> Result<(), String> {
        let snapshot = WorldSnapshot {
            tick: Tick(7),
            objects: vec![
                ObjectHandle {
                    generation: 1,
                    slot: 2,
                },
                ObjectHandle {
                    generation: 1,
                    slot: 5,
                },
            ],
            events: Vec::new(),
            hash: StateHash([0; 32]),
        };

        let commands = render_snapshot_commands(&snapshot)?;

        assert_eq!(commands.commands.len(), 4);
        assert!(matches!(
            commands.commands[0],
            fparkan_render::RenderCommand::BeginFrame
        ));
        assert!(matches!(
            commands.commands[3],
            fparkan_render::RenderCommand::EndFrame
        ));
        let fparkan_render::RenderCommand::Draw(first) = &commands.commands[1] else {
            return Err("expected draw".to_string());
        };
        assert_eq!(first.mesh, GpuMeshId(3));
        assert_eq!(first.stable_order, 2);
        Ok(())
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn selected_is_and_is2_missions_produce_approved_render_captures() {
        for case in [
            RenderCase {
                root: "IS",
                mission: "MISSIONS/CAMPAIGN/CAMPAIGN.00/Mission.01/data.tma",
                expected: "{\"report_kind\":\"render-planning\",\"backend\":\"vulkan-planning\",\"window\":\"synthetic\",\"mission\":\"MISSIONS/CAMPAIGN/CAMPAIGN.00/Mission.01/data.tma\",\"objects\":33,\"frames\":1,\"tick\":1,\"draws\":33,\"submission_plans\":1,\"last_command_capture_bytes\":2008,\"hash\":\"ca17cc76e55c45e83c1c9c1c088e84bf1a698be91a7730943210fe27596af841\"}",
            },
            RenderCase {
                root: "IS2",
                mission: "MISSIONS/Campaign/CAMPAIGN.00/Mission.02/data.tma",
                expected: "{\"report_kind\":\"render-planning\",\"backend\":\"vulkan-planning\",\"window\":\"synthetic\",\"mission\":\"MISSIONS/Campaign/CAMPAIGN.00/Mission.02/data.tma\",\"objects\":10,\"frames\":1,\"tick\":1,\"draws\":10,\"submission_plans\":1,\"last_command_capture_bytes\":605,\"hash\":\"5d720b3ab690076a398a79a404850bbeaee2e33811b5bb570ec8a96d4a7a2fc4\"}",
            },
        ] {
            assert_eq!(
                run(&render_args(&licensed_root(case.root), case.mission)),
                Ok(case.expected.to_string())
            );
        }
    }

    #[test]
    fn json_hash_is_hex() {
        let mut hash = [0; 32];
        hash[0] = 0xab;
        hash[31] = 0xcd;

        assert_eq!(
            json_hash(&hash),
            "\"ab000000000000000000000000000000000000000000000000000000000000cd\""
        );
    }

    #[derive(Clone, Copy)]
    struct RenderCase {
        root: &'static str,
        mission: &'static str,
        expected: &'static str,
    }

    fn render_args(root: &Path, mission: &str) -> Vec<String> {
        vec![
            "--root".to_string(),
            root.to_str().expect("utf8 root").to_string(),
            "--mission".to_string(),
            mission.to_string(),
            "--frames".to_string(),
            "1".to_string(),
        ]
    }

    fn licensed_root(name: &str) -> PathBuf {
        let variable = match name {
            "IS" => "FPARKAN_CORPUS_PART1_ROOT",
            "IS2" => "FPARKAN_CORPUS_PART2_ROOT",
            _ => panic!("unknown licensed corpus part: {name}"),
        };
        let root = std::env::var_os(variable)
            .map(PathBuf::from)
            .unwrap_or_else(|| panic!("{variable} is required for licensed corpus tests"));
        assert!(
            root.is_dir(),
            "licensed corpus root is missing: {}",
            root.display()
        );
        root
    }
}
