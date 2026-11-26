use super::{
    initial_native_effect_phase, AmbientEffectGpuSlot, ControlEffectFrameSample,
    ControlEffectInstanceKey, ControlEffectInstanceState, ControlEffectSlot, ControlSoundSlot,
    MissionRenderRoot, SceneSampleId,
};
use fparkan_fx::clock::{FxInstanceClock, FxPhaseProvider};
use fparkan_runtime::{loaded_mission_control_device_tables, AutoDemoCombatEvent, Engine};
use std::collections::{HashMap, HashSet};

pub(super) fn owner_control_capacity(
    engine: &Engine,
    roots: &[Option<MissionRenderRoot>],
    index: usize,
) -> Option<(f32, f32)> {
    let handle = roots.get(index)?.as_ref()?.handle;
    let table = loaded_mission_control_device_tables(engine)?
        .iter()
        .find(|table| table.handle == handle)?;
    Some((
        f32::from_bits(table.node_provider_amount_bits?),
        f32::from_bits(table.node_provider_maximum_amount_bits?),
    ))
}

pub(super) fn advance_control_effect_instances(
    states: &mut HashMap<ControlEffectInstanceKey, ControlEffectInstanceState>,
    ambient_slots: &[AmbientEffectGpuSlot],
    point_light_slots: &[ControlEffectSlot],
    sound_slots: &[ControlSoundSlot],
    sample_id: SceneSampleId,
    control_clock_ms: u32,
    manager_flag: bool,
    owner_capacity: impl Fn(usize) -> Option<(f32, f32)>,
    owner_motion: impl Fn(usize) -> Option<FxPhaseProvider>,
) -> HashMap<ControlEffectInstanceKey, ControlEffectFrameSample> {
    let mut specifications = HashMap::new();
    for slot in ambient_slots {
        specifications.entry(slot.random_instance_key).or_insert((
            slot.random_seed,
            slot.header_flags,
            slot.phase_jitter,
            slot.random_shift,
            slot.time_mode,
            slot.duration_seconds,
        ));
    }
    for slot in point_light_slots
        .iter()
        .chain(sound_slots.iter().map(|slot| &slot.effect))
    {
        specifications.entry(slot.random_instance_key).or_insert((
            slot.random_seed,
            slot.header_flags,
            slot.phase_jitter,
            slot.random_shift,
            slot.time_mode,
            slot.duration_seconds,
        ));
    }

    specifications
        .into_iter()
        .map(
            |(key, (seed, flags, phase_jitter, random_shift, mode, duration))| {
                let state = states.entry(key).or_insert_with(|| {
                    ControlEffectInstanceState::new(seed, flags, phase_jitter, random_shift)
                });
                if state.last_update_id == Some(sample_id) {
                    return (key, state.sample);
                }
                let idle = FxPhaseProvider::idle();
                let mut provider = idle;
                let mut resolved = false;
                if mode == 14 {
                    let capacity = owner_capacity(key.object_index)
                        .and_then(|(remaining, capacity)| idle.with_control_capacity(remaining, capacity));
                    provider = capacity.unwrap_or(idle);
                    if state.clock.is_none() {
                        eprintln!("render_coverage control_damage_fx_activation_approx=1 (instance={key:?})");
                        if capacity.is_none() {
                            eprintln!("render_coverage control_fx_owner_state_stub=1 (missing Control capacity; instance={key:?} mode={mode})");
                        }
                    }
                } else if matches!(mode, 5 | 15) {
                    if let Some(motion) = owner_motion(key.object_index) {
                        provider = motion;
                        resolved = true;
                    }
                }
                if mode != 14 && !resolved && state.clock.is_none() && fparkan_fx::fx_time_mode_requires_owner_state(mode) {
                    eprintln!("render_coverage control_fx_owner_state_stub=1 (idle provider; instance={key:?} mode={mode})");
                }
                let clock = state.clock.get_or_insert_with(|| {
                    // All static-preview instances are created at scene time zero.
                    FxInstanceClock::new(mode, flags, duration, 0.0)
                        .expect("prepared FX clock metadata")
                });
                let now_ms = control_clock_ms as f32;
                let active = provider.control_effect_active(mode);
                let phase = clock.update(now_ms).and_then(|()| {
                    clock.phase(now_ms, manager_flag, Some(&provider))
                });
                let instance_elapsed = clock.elapsed_seconds(now_ms).unwrap_or(0.0);
                if phase.is_none() && state.last_update_id.is_none() {
                    eprintln!("render_coverage control_fx_clock_unsupported=1 (instance={key:?} mode={mode})");
                }
                let phase = phase.filter(|_| active);
                let initial_phase = phase
                    .map(|phase| initial_native_effect_phase(flags, phase))
                    .unwrap_or(-1.0);
                state.advance(sample_id, control_clock_ms, initial_phase);
                state.sample.phase = phase;
                state.sample.elapsed_seconds = instance_elapsed;
                (key, state.sample)
            },
        )
        .collect()
}

pub(super) fn activate_firing_flashes(
    samples: &mut HashMap<ControlEffectInstanceKey, ControlEffectFrameSample>,
    ambient_slots: &[AmbientEffectGpuSlot],
    point_light_slots: &[ControlEffectSlot],
    events: &[AutoDemoCombatEvent],
) -> HashSet<ControlEffectInstanceKey> {
    events
        .iter()
        .filter_map(|event| {
            let AutoDemoCombatEvent::Shot {
                effect_instance: Some(effect),
                ..
            } = *event
            else {
                return None;
            };
            let key = ControlEffectInstanceKey {
                object_index: effect.object_index,
                component_index: effect.component_index,
                table_index: effect.table_index,
                row_index: effect.row_index,
            };
            let has_external_flash = ambient_slots
                .iter()
                .any(|slot| slot.random_instance_key == key && slot.time_mode == 4)
                || point_light_slots
                    .iter()
                    .any(|slot| slot.random_instance_key == key && slot.time_mode == 4);
            if !has_external_flash {
                return None;
            }
            let sample = samples.get_mut(&key)?;
            // shortcut: the release occurs at the midpoint of the weapon cycle.
            // Keep the flash to this snapshot until the full effect phase is wired.
            sample.phase = Some(0.5);
            sample.elapsed_seconds = 0.0;
            Some(key)
        })
        .collect()
}
