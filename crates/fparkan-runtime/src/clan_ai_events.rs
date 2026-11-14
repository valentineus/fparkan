use crate::{LoadedMissionState, MissionEventStop};
use fparkan_script::{ScriptDispatchSelector, VarSetDefault};
use fparkan_world::{OwnerId, World};
use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct ClanAiEvents {
    next_ms: BTreeMap<usize, u32>,
    stops: BTreeMap<(usize, usize, usize), (MissionEventStop, u64)>,
    research_status_stubs: BTreeMap<usize, u64>,
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
                    ScriptDispatchSelector::Handler(65) if instruction.references.is_empty() => {
                        // RVA 0x111d0 returns false while the research catalog is unloaded.
                        let count = self
                            .clan_ai_events
                            .research_status_stubs
                            .entry(clan)
                            .or_default();
                        *count += 1;
                        if *count == 1 {
                            eprintln!(
                                "sim_coverage script_research_status_stub=1 clan={clan} script={}",
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

fn count_free_minds(slots: &[u32]) -> u32 {
    slots.iter().filter(|&&word| word == u32::MAX).count() as u32
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

    pub(super) fn research_status_stubs(&self) -> impl Iterator<Item = (usize, u64)> + '_ {
        self.research_status_stubs
            .iter()
            .map(|(&clan, &count)| (clan, count))
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
}
