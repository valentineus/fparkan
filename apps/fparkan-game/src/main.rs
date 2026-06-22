#![forbid(unsafe_code)]
#![allow(clippy::print_stderr, clippy::print_stdout)]
//! `FParkan` rendered game composition root.

use fparkan_render::{
    DrawCommand, DrawId, GpuMaterialId, GpuMeshId, IndexRange, RecordingBackend, RenderBackend,
    RenderCommand, RenderCommandList, RenderPhase,
};
use fparkan_runtime::{
    create, frame, load_mission, EngineConfig, EngineMode, EngineServices, MissionRequest,
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

    let mut backend = RecordingBackend::default();
    let mut last_draw_count = 0usize;
    let mut last_tick = 0u64;
    let mut last_hash = [0u8; 32];
    for _ in 0..args.frames {
        let result = frame(&mut engine).map_err(|err| err.to_string())?;
        last_tick = result.snapshot.tick.0;
        last_hash = result.snapshot.hash.0;
        let commands = render_snapshot_commands(&result.snapshot);
        last_draw_count = commands
            .commands
            .iter()
            .filter(|command| matches!(command, RenderCommand::Draw(_)))
            .count();
        backend
            .execute(&commands)
            .map_err(|err| format!("render backend: {err}"))?;
    }

    Ok(format!(
        "{{\"mission\":{},\"objects\":{},\"frames\":{},\"tick\":{},\"draws\":{},\"captures\":{},\"last_capture_bytes\":{},\"hash\":{}}}",
        json_string(&args.mission),
        loaded.object_count,
        args.frames,
        last_tick,
        last_draw_count,
        backend.captures().len(),
        backend.last_capture().map_or(0, <[u8]>::len),
        json_hash(&last_hash)
    ))
}

fn render_snapshot_commands(snapshot: &WorldSnapshot) -> RenderCommandList {
    let mut commands = Vec::with_capacity(snapshot.objects.len() + 2);
    commands.push(RenderCommand::BeginFrame);
    for (index, handle) in snapshot.objects.iter().enumerate() {
        let stable_order = u64::from(handle.slot);
        let draw_id = snapshot
            .tick
            .0
            .wrapping_mul(1_000_003)
            .wrapping_add(stable_order);
        commands.push(RenderCommand::Draw(DrawCommand {
            id: DrawId(draw_id),
            phase: RenderPhase::Opaque,
            object_id: None,
            mesh: GpuMeshId(u64::from(handle.slot) + 1),
            material: GpuMaterialId(1),
            transform: identity_transform(index_to_f32(index)),
            range: IndexRange { start: 0, count: 3 },
            stable_order,
        }));
    }
    commands.push(RenderCommand::EndFrame);
    RenderCommandList { commands }
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
                let _ = write!(out, "\\u{:04x}", u32::from(c));
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

        let commands = render_snapshot_commands(&snapshot);

        assert_eq!(commands.commands.len(), 4);
        assert!(matches!(commands.commands[0], RenderCommand::BeginFrame));
        assert!(matches!(commands.commands[3], RenderCommand::EndFrame));
        let RenderCommand::Draw(first) = &commands.commands[1] else {
            return Err("expected draw".to_string());
        };
        assert_eq!(first.mesh, GpuMeshId(3));
        assert_eq!(first.stable_order, 2);
        Ok(())
    }

    #[test]
    fn selected_is_and_is2_missions_produce_approved_render_captures() {
        for case in [
            RenderCase {
                root: "IS",
                mission: "MISSIONS/CAMPAIGN/CAMPAIGN.00/Mission.01/data.tma",
                expected: "{\"mission\":\"MISSIONS/CAMPAIGN/CAMPAIGN.00/Mission.01/data.tma\",\"objects\":33,\"frames\":1,\"tick\":1,\"draws\":33,\"captures\":1,\"last_capture_bytes\":810,\"hash\":\"8584c4307bc911fc82bf909018662f392f3982bf909018666298bde408fe4242\"}",
            },
            RenderCase {
                root: "IS2",
                mission: "MISSIONS/Campaign/CAMPAIGN.00/Mission.02/data.tma",
                expected: "{\"mission\":\"MISSIONS/Campaign/CAMPAIGN.00/Mission.02/data.tma\",\"objects\":10,\"frames\":1,\"tick\":1,\"draws\":10,\"captures\":1,\"last_capture_bytes\":235,\"hash\":\"c52267cb14f699cb73b958e46c99c23ec23e73b958e46c99b3650afbcce56291\"}",
            },
        ] {
            assert_eq!(
                run(&render_args(&workspace_root().join("testdata").join(case.root), case.mission)),
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

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root")
            .to_path_buf()
    }
}
