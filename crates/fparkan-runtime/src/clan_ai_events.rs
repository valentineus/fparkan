use crate::{BehaviorOreState, LoadedMissionState, MissionEventStop};
use fparkan_script::{
    Handler2RecordInput, Handler2RecordScheduler, ScriptDispatchSelector, VarSet, VarSetDefault,
};
use fparkan_world::{identity_metadata, registration_sequence, OwnerId, World};
use std::collections::BTreeMap;

const STORAGE_CLASS: u32 = 0x8000_0008;
const RESOURCE_ORE: u32 = 0x0200_0100;

#[derive(Default)]
pub(super) struct ClanAiEvents {
    next_ms: BTreeMap<usize, u32>,
    stops: BTreeMap<(usize, usize, usize), (MissionEventStop, u64)>,
    research_stubs: BTreeMap<(usize, u8), u64>,
    problems: BTreeMap<usize, Handler2RecordScheduler>,
}

impl LoadedMissionState {
    pub(super) fn tick_clan_ai_events(&mut self, world: &World, now_ms: u32) {
        let Some(varset) = &self.script_varset else {
            return;
        };
        for bundle in self.script_bundles.iter().filter(|bundle| {
            bundle.behavior_warm_start.is_some() && self.mission.clans[bundle.clan_index].mode == 2
        }) {
            let Some((event_index, event)) =
                bundle.package.events.iter().enumerate().find(|(_, event)| {
                    event.name_raw == b"Problems0\0" && !event.instructions.is_empty()
                })
            else {
                continue;
            };
            let clan = bundle.clan_index;
            // shortcut: use the base interval until the separate native AI clock and RNG are wired.
            if !self
                .clan_ai_events
                .due(clan, self.mission_events.start_ms, now_ms)
            {
                continue;
            }
            let Some(state) = self
                .script_varset_states
                .iter_mut()
                .find(|state| state.clan_index == clan)
            else {
                continue;
            };
            let result = varset.declarations.execute_event_prefix(
                event,
                bundle.formula_table.as_ref(),
                &mut state.values,
                |instruction, values| match instruction.dispatch_selector() {
                    ScriptDispatchSelector::Handler(2) => {
                        let (name, input) = problem_operands(&varset.declarations, values, &instruction.references)
                            .ok_or("Handler(2) requires seven numeric problem operands")?;
                        let start = format!("{name}_Start\0");
                        let resume = format!("{name}_Continue\0");
                        if bundle.package.events.iter().any(|event| event.name_raw == start.as_bytes())
                            && bundle.package.events.iter().any(|event| event.name_raw == resume.as_bytes())
                        {
                            let update = self.clan_ai_events.problems.entry(clan).or_default().submit(input);
                            if update.created {
                                eprintln!("sim_coverage script_problem_dispatch_stub=1 clan={clan} problem={name}");
                            }
                        }
                        Ok(None)
                    }
                    ScriptDispatchSelector::Handler(3) if instruction.references.is_empty() => {
                        let [available, _, _, demand, _] = bundle
                            .behavior_warm_start
                            .ok_or("Handler(3) requires live Behavior totals")?
                            .cleared_words
                            .map(f32::from_bits);
                        Ok(Some(VarSetDefault::FloatBits(
                            (available - demand).to_bits(),
                        )))
                    }
                    ScriptDispatchSelector::Handler(16)
                        if instruction.dword_pair(values) == Some((STORAGE_CLASS, RESOURCE_ORE)) =>
                    {
                        let remaining = storage_space(&self.behavior_ore_states, world, OwnerId(clan as u16));
                        Ok(Some(VarSetDefault::FloatBits(remaining.to_bits())))
                    }
                    ScriptDispatchSelector::Handler(17)
                        if instruction.dword_pair(values)
                            == Some((crate::mining::MINE_CLASS, 0x0afc_0900)) =>
                    {
                        let owner = OwnerId(clan as u16);
                        let remaining =
                            crate::mining::remaining_for_clan(&self.mine_sites, world, owner);
                        Ok(Some(VarSetDefault::FloatBits(remaining.to_bits())))
                    }
                    ScriptDispatchSelector::Handler(49) if instruction.references.is_empty() => {
                        let slots = self
                            .ai_startup_states
                            .iter()
                            .find(|state| state.clan_index == clan);
                        Ok(Some(VarSetDefault::Dword(
                            slots.map_or(0, |state| count_free_minds(&state.record_slots)),
                        )))
                    }
                    ScriptDispatchSelector::Handler(34) => {
                        let &[index] = instruction.references.as_slice() else {
                            return Err("Handler(34) requires one DWORD operand".to_owned());
                        };
                        let Some(&VarSetDefault::Dword(class)) = values.get(index as usize) else {
                            return Err("Handler(34) requires one DWORD operand".to_owned());
                        };
                        Ok(Some(VarSetDefault::Dword(
                            self.mission_events.count_clan_class(
                                world,
                                OwnerId(clan as u16),
                                class,
                            ),
                        )))
                    }
                    ScriptDispatchSelector::Handler(handler @ (56 | 65)) if instruction.references.is_empty() => {
                        // shortcut: keep research disabled until the native policy flag and catalog are wired.
                        let count = self
                            .clan_ai_events
                            .research_stubs
                            .entry((clan, handler))
                            .or_default();
                        *count += 1;
                        if *count == 1 {
                            eprintln!(
                                "sim_coverage script_research_status_stub=1 clan={clan} handler={handler} script={}",
                                bundle.script_path
                            );
                        }
                        Ok(Some(VarSetDefault::Dword(0)))
                    }
                    selector => Err(format!("{selector:?}")),
                },
            );
            if let Err((instruction_index, reason)) = result {
                let (stop, count) = self
                    .clan_ai_events
                    .stops
                    .entry((clan, event_index, instruction_index))
                    .or_insert_with(|| {
                        (
                            MissionEventStop {
                                clan_index: clan,
                                script_path: bundle.script_path.clone(),
                                event_index,
                                instruction_index,
                                reason,
                            },
                            0,
                        )
                    });
                *count += 1;
                if *count == 1 {
                    eprintln!("sim_coverage script_clan_ai_unsupported=1 clan={clan} script={} event={event_index} instruction={instruction_index} reason={}", stop.script_path, stop.reason);
                }
            }
        }
    }
}

fn problem_operands<'a>(
    varset: &'a VarSet,
    values: &[VarSetDefault],
    references: &[u32],
) -> Option<(&'a str, Handler2RecordInput)> {
    let &[kind, priority, increment, sender, first, second, third] = references else {
        return None;
    };
    let value = |index: u32| values.get(index as usize).copied();
    let word = |index| match value(index)? {
        VarSetDefault::Dword(value) => Some(value),
        VarSetDefault::FloatBits(bits) => {
            let value = f64::from(f32::from_bits(bits));
            let limit = 9_223_372_036_854_775_808.0;
            (-limit..limit)
                .contains(&value)
                .then(|| value.trunc() as i64 as u32)
        }
    };
    let scalar = |index| match value(index)? {
        VarSetDefault::Dword(value) => Some(value as f32),
        VarSetDefault::FloatBits(bits) => Some(f32::from_bits(bits)),
    };
    Some((
        varset.declarations.get(kind as usize)?.name.as_str(),
        Handler2RecordInput {
            word_0: word(kind)?,
            scalar_1: scalar(priority)?,
            word_2: word(increment)?,
            word_3: word(sender)?,
            scalar_4: scalar(first)?,
            scalar_5: scalar(second)?,
            scalar_6: scalar(third)?,
        },
    ))
}

fn count_free_minds(slots: &[u32]) -> u32 {
    slots.iter().filter(|&&word| word == u32::MAX).count() as u32
}

fn storage_space(ores: &[BehaviorOreState], world: &World, clan: OwnerId) -> f32 {
    ores.iter()
        .filter(|ore| {
            ore.class_flags == STORAGE_CLASS
                && registration_sequence(world, ore.handle).is_ok_and(|sequence| sequence.is_some())
                && identity_metadata(world, ore.handle)
                    .is_ok_and(|identity| identity.owner_id == Some(clan))
        })
        .fold(0.0, |space, ore| {
            space + (f32::from_bits(ore.maximum_ore_bits) - f32::from_bits(ore.current_ore_bits))
        })
}

impl ClanAiEvents {
    fn due(&mut self, clan: usize, start_ms: u32, now_ms: u32) -> bool {
        let next = self.next_ms.entry(clan).or_insert(7_000);
        let elapsed = now_ms.wrapping_sub(start_ms);
        if elapsed < *next {
            return false;
        }
        *next = elapsed.saturating_add(7_000);
        true
    }

    pub(super) fn stops(&self) -> impl Iterator<Item = (&MissionEventStop, u64)> {
        self.stops.values().map(|(stop, count)| (stop, *count))
    }

    pub(super) fn research_status_stubs(&self) -> impl Iterator<Item = (usize, u8, u64)> + '_ {
        self.research_stubs
            .iter()
            .map(|(&(clan, handler), &count)| (clan, handler, count))
    }

    pub(super) fn pending_problems(&self) -> impl Iterator<Item = (usize, u32)> + '_ {
        self.problems.iter().flat_map(|(&clan, problems)| {
            problems
                .records()
                .iter()
                .map(move |record| (clan, record.key.word_0))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clans_have_independent_due_times_and_no_replay_after_a_late_tick() {
        let mut ai = ClanAiEvents::default();
        assert!(!ai.due(1, 100_000, 106_999));
        assert!(ai.due(1, 100_000, 107_000));
        assert!(ai.due(2, 100_000, 107_000));
        assert!(!ai.due(1, 100_000, 113_999));
        assert!(ai.due(1, 100_000, 120_000));
        assert!(!ai.due(1, 100_000, 120_000));
    }

    #[test]
    fn free_mind_count_uses_only_the_native_sentinel() {
        assert_eq!(count_free_minds(&[]), 0);
        assert_eq!(count_free_minds(&[u32::MAX, 0, u32::MAX, 1, 0xfffe]), 2);
    }

    #[test]
    fn problem_operands_resolve_live_cells_and_numeric_types() {
        let varset = fparkan_script::parse_varset(
            b"VAR(DWORD, PBM_ROBOT_NEEDED, 6)\nVAR(float, priority, 0)\nVAR(DWORD, arg, 0)\n",
        )
        .unwrap();
        let values = [
            VarSetDefault::Dword(6),
            VarSetDefault::FloatBits(5.75_f32.to_bits()),
            VarSetDefault::Dword(12),
        ];
        let (name, problem) = problem_operands(&varset, &values, &[0, 1, 1, 2, 2, 1, 0]).unwrap();
        assert_eq!(name, "PBM_ROBOT_NEEDED");
        assert_eq!(
            problem,
            Handler2RecordInput {
                word_0: 6,
                scalar_1: 5.75,
                word_2: 5,
                word_3: 12,
                scalar_4: 12.0,
                scalar_5: 5.75,
                scalar_6: 6.0,
            }
        );
    }

    #[test]
    fn free_ore_space_follows_storage_ownership_and_lifecycle() {
        use fparkan_world::{
            construct_object, new, register_object, request_delete, set_owner, step, InputSnapshot,
            ObjectDraft, OriginalObjectId, WorldConfig,
        };
        let mut world = new(WorldConfig);
        let mut make = |id, class_flags, owner, ore: f32| {
            let original_id = OriginalObjectId(id);
            let handle = construct_object(
                &mut world,
                ObjectDraft {
                    original_id: Some(original_id),
                },
            )
            .unwrap();
            set_owner(&mut world, handle, Some(OwnerId(owner))).unwrap();
            register_object(&mut world, handle).unwrap();
            BehaviorOreState {
                original_id,
                handle,
                class_flags,
                current_ore_bits: ore.to_bits(),
                maximum_ore_bits: 100.0_f32.to_bits(),
            }
        };
        let ores = [
            make(1, STORAGE_CLASS, 1, 15.0),
            make(2, STORAGE_CLASS, 1, 40.0),
            make(3, STORAGE_CLASS, 0, 10.0),
            make(4, crate::mining::MINE_CLASS, 1, 20.0),
        ];
        assert_eq!(storage_space(&ores, &world, OwnerId(1)), 145.0);
        request_delete(&mut world, ores[0].handle).unwrap();
        step(&mut world, &InputSnapshot).unwrap();
        assert_eq!(storage_space(&ores, &world, OwnerId(1)), 60.0);
    }
}
