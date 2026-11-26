//! Play the original Control footstep effects at the walker.

use crate::{audio::GameAudio, shot_sound};
use fparkan_assets::PreparedControlEffectSection;
use fparkan_fx::ambient_sound::Op2SoundCommand;
use fparkan_runtime::{loaded_mission_assets, Engine, PlayerFootstep, StartingCockpit};
use std::path::Path;

#[derive(Default)]
pub(super) struct PlayerFootsteps {
    sounds: [Option<Op2SoundCommand>; 2],
}

impl PlayerFootsteps {
    pub(super) fn load(root: &Path, engine: &Engine, cockpit: StartingCockpit) -> Option<Self> {
        let assets = loaded_mission_assets(engine)?;
        let control = assets
            .object_component_controls
            .get(cockpit.object_index)?
            .first()?
            .as_ref()?;
        let PreparedControlEffectSection::Parsed(rows) = &control.effect_section else {
            return None;
        };
        // shortcut: use the grass and floor variants until native material
        // selection is recovered. The pair's two feet share a sound.
        let sounds = [301, 501].map(|selector| {
            let row = rows.iter().find(|row| row.selector == selector)?;
            let archive = resource_name(&row.resource.archive)?;
            let name = resource_name(&row.resource.name)?;
            shot_sound::load(root, archive, name)
        });
        if sounds.iter().any(Option::is_none) {
            eprintln!("sim_coverage player_footstep_sound_stub=1 (missing ground or floor effect)");
        }
        Some(Self { sounds })
    }

    pub(super) fn play(&self, audio: &mut GameAudio, footstep: PlayerFootstep) {
        let Some(event) = self.sounds[usize::from(footstep.building_floor)]
            .as_ref()
            .and_then(|command| command.one_shot_at(footstep.position_bits.map(f32::from_bits)))
        else {
            return;
        };
        if let Err(error) = audio.handle_sound_event(&event) {
            eprintln!("sim_coverage player_footstep_sound_stub=1 reason={error}");
        }
    }
}

fn resource_name(bytes: &[u8; 32]) -> Option<&str> {
    let end = bytes
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(bytes.len());
    std::str::from_utf8(&bytes[..end])
        .ok()
        .filter(|name| !name.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_runtime::{
        create, load_gameplay_mission, starting_cockpit, EngineServices, MissionMode,
        MissionRequest, MissionStartupInput,
    };
    use fparkan_vfs::DirectoryVfs;
    use std::path::PathBuf;
    use std::sync::Arc;

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_hero_footsteps_resolve_ground_and_floor_samples() {
        let root =
            PathBuf::from(std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed root"));
        let mut engine = create(EngineServices::new(Arc::new(DirectoryVfs::new(&root)))).unwrap();
        load_gameplay_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/CAMPAIGN/CAMPAIGN.00/Mission.01/data.tma".into(),
                mode: MissionMode::SingleMission,
            },
            MissionStartupInput {
                queue_time_ms: 0,
                research_creation_gate: false,
            },
            None,
        )
        .unwrap();
        let cockpit = starting_cockpit(&engine).unwrap();
        let footsteps = PlayerFootsteps::load(&root, &engine, cockpit).unwrap();
        for (command, sample) in footsteps.sounds.iter().zip(["step_hg.wav", "step_hf.wav"]) {
            let event = command.as_ref().unwrap().one_shot_at([0.0; 3]).unwrap();
            assert!(event.archive.eq_ignore_ascii_case("sounds.lib"));
            assert!(event.name.eq_ignore_ascii_case(sample), "{event:?}");
        }
    }
}
