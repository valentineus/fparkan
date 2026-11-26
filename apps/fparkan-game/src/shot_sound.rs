//! Spatial one-shots from weapons and projectile startup effects.

use crate::audio::GameAudio;
use fparkan_assets::load_resource_entry_bytes_from_root;
use fparkan_fx::{ambient_sound::Op2SoundCommand, decode_fxid, FxDocument};
use fparkan_runtime::AutoDemoCombatEvent;
use std::path::Path;

pub(super) fn load(root: &Path, archive: &str, name: &str) -> Option<Op2SoundCommand> {
    let bytes = load_resource_entry_bytes_from_root(root, archive, name).ok()?;
    let effect = decode_fxid(bytes).ok()?;
    effect
        .commands()
        .iter()
        .find_map(Op2SoundCommand::from_command)
}

pub(super) fn projectile_launch<'a>(
    effects: impl Iterator<Item = &'a FxDocument>,
) -> Vec<Op2SoundCommand> {
    effects
        .filter(|effect| {
            let header = effect.header();
            header.time_mode == 1 && header.flags == 0 && header.phase_jitter == 0.0
        })
        .flat_map(FxDocument::commands)
        .filter_map(Op2SoundCommand::from_command)
        .filter(Op2SoundCommand::starts_at_spawn)
        .collect()
}

pub(super) fn report_missing_player_sounds(
    layouts: &std::collections::HashMap<usize, super::PlayerWeaponLayout>,
    projectiles: &[Option<super::ProjectileDraw>],
) {
    for (&object, layout) in layouts {
        for (weapon, (pool, sound)) in layout.weapons.weapons().iter().zip(&layout.slots) {
            let has_launch_sound = projectiles
                .get(*pool)
                .and_then(Option::as_ref)
                .is_some_and(|draw| !draw.launch_sounds.is_empty());
            if sound.is_none() && !has_launch_sound {
                eprintln!(
                    "sim_coverage player_weapon_sound_stub=1 object={object} projectile={}",
                    weapon.projectile
                );
            }
        }
    }
}

pub(super) fn play_autonomous(
    sounds: &[Option<Op2SoundCommand>],
    projectiles: &[Option<super::ProjectileDraw>],
    events: &[AutoDemoCombatEvent],
    audio: &mut GameAudio,
) {
    for event in events {
        let AutoDemoCombatEvent::Shot {
            effect,
            projectile,
            muzzle_bits,
            ..
        } = *event
        else {
            continue;
        };
        let weapon_sound = effect
            .and_then(|effect| sounds.get(effect))
            .and_then(Option::as_ref);
        let launch_sounds = projectiles
            .get(projectile)
            .and_then(Option::as_ref)
            .map_or(&[][..], |draw| draw.launch_sounds.as_slice());
        let position = muzzle_bits.map(f32::from_bits);
        for sound in weapon_sound
            .into_iter()
            .chain(launch_sounds)
            .filter_map(|sound| sound.one_shot_at(position))
        {
            if let Err(error) = audio.handle_sound_event(&sound) {
                eprintln!(
                    "sim_coverage autonomous_weapon_sound_stub=1 effect={}:{} reason={error}",
                    sound.archive, sound.name
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_runtime::{create, load_player_projectile, EngineServices};
    use fparkan_vfs::DirectoryVfs;
    use std::{path::PathBuf, sync::Arc};

    #[test]
    #[ignore = "requires licensed projectile and effect resources"]
    fn licensed_hero_missile_prepares_launch_without_the_engine_loop() {
        let root = PathBuf::from(std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").unwrap());
        let engine = create(EngineServices::new(Arc::new(DirectoryVfs::new(&root)))).unwrap();
        let missile = load_player_projectile(&engine, "objects.rlb", "bm_h_01").unwrap();
        let effects = crate::projectile_render::initial_effects(&root, &missile, "bm_h_01");
        let sounds = projectile_launch(effects.iter().map(|(_, effect)| effect));
        let [sound] = sounds.as_slice() else {
            panic!("expected the single launch sound, got {sounds:?}");
        };
        let position = [10.0, 20.0, 30.0];
        let event = sound.one_shot_at(position).unwrap();
        assert_eq!(event.position, position);
        assert_eq!(event.archive, "sounds.lib");
        assert_eq!(event.name, "H_fire_missile.wav");
    }
}
