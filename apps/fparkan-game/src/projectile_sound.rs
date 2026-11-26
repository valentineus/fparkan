//! Spatial Op2 engine loops owned by live player projectiles.

use super::{
    audio::GameAudio,
    control_matrix_from_columns, control_row_points, control_type4_effect_local_matrix,
    matrix_to_row_major, multiply_row_major,
    projectile_render::{flight_basis, ProjectileDraw, ProjectileEffect},
};
use fparkan_fx::{
    ambient_sound::{Op2SoundCommand, Op2SoundState},
    clock::{FxInstanceClock, FxPhaseProvider},
};
use fparkan_runtime::{PlayerProjectile, ProjectileAssets};

pub(super) struct EngineSound {
    command: Op2SoundCommand,
    projectile_from_effect: [[f32; 4]; 4],
    phase: f32,
}

pub(super) fn prepare(
    projectile: &ProjectileAssets,
    effects: &[ProjectileEffect<'_>],
) -> Vec<EngineSound> {
    let Some(control) = projectile.assets.object_component_controls[0][0].as_ref() else {
        return Vec::new();
    };
    let Some(points) = &control.point_list else {
        return Vec::new();
    };
    let mut owner = FxPhaseProvider::idle();
    owner.linear_velocity = [0.0, projectile.speed, 0.0];
    owner.linear_velocity_limit =
        std::array::from_fn(|axis| f32::from_bits(control.native_profile_words[6 + axis]));
    let mut sounds = Vec::new();
    for effect in effects {
        let header = effect.document.header();
        if header.time_mode != 5 || header.flags != 0 {
            continue;
        }
        let Some(mut clock) = FxInstanceClock::new(5, 0, header.duration_seconds, 0.0) else {
            continue;
        };
        // This owner has a constant local forward velocity throughout its flight.
        let Some(phase) = clock.phase(0.0, false, Some(&owner)) else {
            continue;
        };
        let Some(anchors) = control_row_points(points, effect.row) else {
            continue;
        };
        if anchors[0].raw_signed_field_04 != 0 {
            continue;
        }
        let Some(projectile_from_effect) = control_type4_effect_local_matrix(anchors, header.scale)
        else {
            continue;
        };
        sounds.extend(
            effect
                .document
                .commands()
                .iter()
                .filter_map(Op2SoundCommand::from_command)
                .filter(Op2SoundCommand::is_looping)
                .map(|command| EngineSound {
                    command,
                    projectile_from_effect,
                    phase: phase.clamp(0.0, 1.0),
                }),
        );
    }
    sounds
}

impl EngineSound {
    fn sample(
        &self,
        flight: &PlayerProjectile,
        playing: bool,
    ) -> Option<fparkan_fx::environment::SoundEvent> {
        let [right, forward, up] = flight_basis(flight.direction);
        let world_from_projectile =
            control_matrix_from_columns(right, forward, up, flight.position);
        let matrix = multiply_row_major(
            matrix_to_row_major(world_from_projectile),
            matrix_to_row_major(self.projectile_from_effect),
        );
        self.command.update(
            &mut Op2SoundState::default(),
            self.phase,
            std::array::from_fn(|row| std::array::from_fn(|column| matrix[row * 4 + column])),
            playing,
        )
    }
}

pub(super) fn update(
    audio: &mut GameAudio,
    draws: &mut [Option<ProjectileDraw>],
    first_sound_id: usize,
) -> Result<(), String> {
    // Manual projectile slots keep their indices until collision or expiry.
    let mut id = first_sound_id;
    for draw in draws
        .iter_mut()
        .flatten()
        .filter(|draw| !draw.engine_sounds.is_empty())
    {
        for instance in &mut draw.instances {
            let born_ms = instance.flight.as_ref().map(PlayerProjectile::born_ms);
            let replaced = born_ms != instance.sound_born_ms;
            instance.sound_born_ms = born_ms;
            for sound in &draw.engine_sounds {
                if replaced {
                    audio.stop_control_sound(id);
                }
                if let Some(event) = instance
                    .flight
                    .as_ref()
                    .and_then(|flight| sound.sample(flight, audio.control_sound_is_playing(id)))
                {
                    audio
                        .handle_control_sound(id, &event)
                        .map_err(|error| format!("play projectile engine sound: {error}"))?;
                }
                id += 1;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projectile_render::initial_effects;
    use fparkan_fx::environment::SoundAction;
    use fparkan_runtime::{create, load_player_projectile, EngineServices};
    use fparkan_vfs::DirectoryVfs;
    use std::{path::PathBuf, sync::Arc};

    #[test]
    #[ignore = "requires licensed projectile and effect resources"]
    fn licensed_missile_engine_loop_follows_the_tail() {
        let root = PathBuf::from(std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").unwrap());
        let engine = create(EngineServices::new(Arc::new(DirectoryVfs::new(&root)))).unwrap();
        let missile = load_player_projectile(&engine, "objects.rlb", "bm_h_01").unwrap();
        let effects = initial_effects(&root, &missile, "bm_h_01");
        let sounds = prepare(&missile, &effects);
        let [sound] = sounds.as_slice() else {
            panic!("expected one engine loop");
        };
        assert_eq!(sound.phase, 1.0);
        let mut flight = PlayerProjectile::new(
            [10.0, 20.0, 30.0],
            [1.0, 0.0, 0.0],
            missile.speed,
            missile.lifetime_ms,
            100,
        )
        .unwrap();
        let start = sound.sample(&flight, false).unwrap();
        assert_eq!(start.action, SoundAction::StartLoop);
        assert_eq!(start.archive, "sounds.lib");
        assert_eq!(start.name, "H_eng_missile.wav");
        assert!((start.position[0] - (10.0 - 0.40465)).abs() < 0.0001);
        assert_eq!(&start.position[1..], &[20.0, 30.0]);

        assert!(flight.advance(200, |_| Ok(false)).unwrap());
        let next = sound.sample(&flight, true).unwrap();
        assert_eq!(next.action, SoundAction::SetLoopVolume);
        assert!((next.position[0] - start.position[0] - 7.0).abs() < 0.0001);
        assert_eq!(&next.position[1..], &[20.0, 30.0]);
    }
}
