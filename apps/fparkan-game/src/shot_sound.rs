//! Load the spatial one-shot from a weapon's firing effect.

use crate::audio::GameAudio;
use fparkan_assets::load_resource_entry_bytes_from_root;
use fparkan_fx::{ambient_sound::Op2SoundCommand, decode_fxid};
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

pub(super) fn play_autonomous(
    sounds: &[Option<Op2SoundCommand>],
    events: &[AutoDemoCombatEvent],
    audio: &mut GameAudio,
) {
    for event in events {
        let AutoDemoCombatEvent::Shot {
            effect: Some(effect),
            muzzle_bits,
            ..
        } = *event
        else {
            continue;
        };
        let Some(sound) = sounds
            .get(effect)
            .and_then(Option::as_ref)
            .and_then(|command| command.one_shot_at(muzzle_bits.map(f32::from_bits)))
        else {
            continue;
        };
        if let Err(error) = audio.handle_sound_event(&sound) {
            eprintln!(
                "sim_coverage autonomous_weapon_sound_stub=1 effect={}:{} reason={error}",
                sound.archive, sound.name
            );
        }
    }
}
