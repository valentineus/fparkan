//! Opcode-2 sound commands, independent of an audio device.

use crate::environment::{SoundAction, SoundEvent};
use crate::{FxCommand, FxOpcode};

/// A validated ordinary Op2 command. Modes 1/3 request an unverified backend
/// flag and are left to the caller's unsupported-command report.
#[derive(Clone, Debug)]
pub struct Op2SoundCommand {
    looping: bool,
    start: f32,
    end: f32,
    position: [f32; 3],
    position_delta: [f32; 3],
    min_distance: f32,
    max_distance: f32,
    frequency: f32,
    frequency_delta: f32,
    archive: String,
    name: String,
}

/// Playback crossing state belongs to a command instance, even when several
/// instances share the same sample.
#[derive(Clone, Debug, Default)]
pub struct Op2SoundState {
    previous_phase: f32,
}

impl Op2SoundCommand {
    /// Decodes the supported native modes 0 (once) and 2 (loop).
    #[must_use]
    pub fn from_command(command: &FxCommand) -> Option<Self> {
        let body = command.raw_body.as_ref();
        if command.opcode != FxOpcode::Op2 || body.len() != 144 {
            return None;
        }
        let mode = u32::from_le_bytes(body[..4].try_into().ok()?);
        // shortcut: backend flag 0x200 and body+76 need a native playback
        // comparison before modes 1/3 or nonzero backend parameters are enabled.
        if !matches!(mode, 0 | 2) || body[76..80] != [0; 4] {
            return None;
        }
        let float = |offset| {
            let value = f32::from_le_bytes(body[offset..offset + 4].try_into().ok()?);
            value.is_finite().then_some(value)
        };
        let vector = |offset| Some([float(offset)?, float(offset + 4)?, float(offset + 8)?]);
        let start = float(4)?;
        let end = float(8)?;
        let position = vector(12)?;
        let position_end = vector(24)?;
        let min_distance = float(60)?;
        let max_distance = float(64)?;
        let frequency = float(68)?;
        let frequency_end = float(72)?;
        let position_delta = std::array::from_fn(|axis| position_end[axis] - position[axis]);
        let frequency_delta = frequency_end - frequency;
        if end <= start
            || !(end - start).is_finite()
            || !position_delta.iter().all(|value: &f32| value.is_finite())
            || !frequency_delta.is_finite()
            || min_distance < 0.0
            || max_distance < min_distance
            || frequency <= 0.0
            || frequency_end <= 0.0
        {
            return None;
        }
        let text = |bytes: &[u8]| {
            let end = bytes
                .iter()
                .position(|&byte| byte == 0)
                .unwrap_or(bytes.len());
            (!bytes[..end].is_empty()).then(|| String::from_utf8_lossy(&bytes[..end]).into_owned())
        };
        Some(Self {
            looping: mode == 2,
            start,
            end,
            position,
            position_delta,
            min_distance,
            max_distance,
            frequency,
            frequency_delta,
            archive: text(&body[80..112])?,
            name: text(&body[112..144])?,
        })
    }

    /// Evaluates Effect.dll's phase crossings and current source parameters.
    /// `playing` comes from the audio owner, since a one-shot can finish between
    /// updates. The matrix maps local command coordinates into world space.
    #[must_use]
    pub fn update(
        &self,
        state: &mut Op2SoundState,
        phase: f32,
        world_from_effect: [[f32; 4]; 4],
        playing: bool,
    ) -> Option<SoundEvent> {
        if !phase.is_finite() || !world_from_effect.iter().flatten().all(|v| v.is_finite()) {
            return None;
        }
        let action = if self.looping {
            if phase >= self.start && phase <= self.end {
                if playing {
                    SoundAction::SetLoopVolume
                } else {
                    SoundAction::StartLoop
                }
            } else if playing {
                SoundAction::StopLoop
            } else {
                return None;
            }
        } else {
            let crossing_phase = if phase == 1.0 { 0.0 } else { phase };
            let crossed = state.previous_phase <= crossing_phase
                && state.previous_phase <= self.start
                && self.start < crossing_phase;
            state.previous_phase = crossing_phase;
            if crossed {
                SoundAction::OneShot
            } else if playing {
                SoundAction::SetLoopVolume
            } else {
                return None;
            }
        };
        let t = (phase - self.start) / (self.end - self.start);
        let local: [f32; 3] =
            std::array::from_fn(|axis| self.position[axis] + t * self.position_delta[axis]);
        let position = std::array::from_fn(|axis| {
            let row = world_from_effect[axis];
            row[0] * local[0] + row[1] * local[1] + row[2] * local[2] + row[3]
        });
        let frequency_ratio = self.frequency + t * self.frequency_delta;
        if !position.iter().all(|value| value.is_finite()) || !frequency_ratio.is_finite() {
            return None;
        }
        Some(SoundEvent {
            archive: self.archive.clone(),
            name: self.name.clone(),
            position,
            volume: 1.0,
            min_distance: self.min_distance,
            max_distance: self.max_distance,
            frequency_ratio,
            action,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const IDENTITY: [[f32; 4]; 4] = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];

    fn command(mode: u32) -> FxCommand {
        let mut body = vec![0; 144];
        body[..4].copy_from_slice(&mode.to_le_bytes());
        for (offset, value) in [
            (4, 0.25_f32),
            (8, 0.75),
            (60, 12.0),
            (64, 80.0),
            (68, 1.0),
            (72, 2.0),
        ] {
            body[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        body[80..90].copy_from_slice(b"sounds.lib");
        body[112..122].copy_from_slice(b"f_defl.wav");
        FxCommand {
            word: 2,
            opcode: FxOpcode::Op2,
            native_flag: false,
            raw_body: Arc::from(body),
            resource_refs: vec![],
        }
    }

    #[test]
    fn loop_has_inclusive_boundaries_and_updates_without_restarting() {
        let command = Op2SoundCommand::from_command(&command(2)).unwrap();
        let mut state = Op2SoundState::default();
        assert!(command.update(&mut state, 0.0, IDENTITY, false).is_none());
        assert_eq!(
            command
                .update(&mut state, 0.25, IDENTITY, false)
                .unwrap()
                .action,
            SoundAction::StartLoop
        );
        assert_eq!(
            command
                .update(&mut state, 0.75, IDENTITY, true)
                .unwrap()
                .action,
            SoundAction::SetLoopVolume
        );
        assert_eq!(
            command
                .update(&mut state, 0.8, IDENTITY, true)
                .unwrap()
                .action,
            SoundAction::StopLoop
        );
        assert!(command.update(&mut state, 0.8, IDENTITY, false).is_none());
    }

    #[test]
    fn one_shot_crossing_is_strict_and_rearms_after_phase_one() {
        let command = Op2SoundCommand::from_command(&command(0)).unwrap();
        let mut state = Op2SoundState::default();
        assert!(command.update(&mut state, 0.25, IDENTITY, false).is_none());
        assert_eq!(
            command
                .update(&mut state, 0.3, IDENTITY, false)
                .unwrap()
                .action,
            SoundAction::OneShot
        );
        assert_eq!(
            command
                .update(&mut state, 0.5, IDENTITY, true)
                .unwrap()
                .action,
            SoundAction::SetLoopVolume
        );
        assert!(command.update(&mut state, 0.6, IDENTITY, false).is_none());
        assert!(command.update(&mut state, 1.0, IDENTITY, false).is_none());
        assert_eq!(
            command
                .update(&mut state, 0.3, IDENTITY, false)
                .unwrap()
                .action,
            SoundAction::OneShot
        );
    }

    #[test]
    fn parameters_keep_native_constant_ranges_and_transform_the_position() {
        let mut source = command(2);
        let mut body = source.raw_body.to_vec();
        body[24..28].copy_from_slice(&4.0_f32.to_le_bytes());
        source.raw_body = Arc::from(body);
        let command = Op2SoundCommand::from_command(&source).unwrap();
        let mut matrix = IDENTITY;
        matrix[0] = [0.0, -1.0, 0.0, 10.0];
        matrix[1] = [2.0, 0.0, 0.0, 20.0];
        let event = command
            .update(&mut Op2SoundState::default(), 0.5, matrix, false)
            .unwrap();
        assert_eq!(event.position, [10.0, 24.0, 0.0]);
        assert_eq!(
            (
                event.min_distance,
                event.max_distance,
                event.frequency_ratio
            ),
            (12.0, 80.0, 1.5)
        );
        assert_eq!(
            (event.archive.as_str(), event.name.as_str()),
            ("sounds.lib", "f_defl.wav")
        );
    }

    #[test]
    fn backend_modes_and_malformed_commands_are_explicitly_unsupported() {
        assert!(Op2SoundCommand::from_command(&command(1)).is_none());
        let mut source = command(2);
        let mut body = source.raw_body.to_vec();
        body[8..12].copy_from_slice(&f32::NAN.to_le_bytes());
        source.raw_body = Arc::from(body);
        assert!(Op2SoundCommand::from_command(&source).is_none());
    }
}
