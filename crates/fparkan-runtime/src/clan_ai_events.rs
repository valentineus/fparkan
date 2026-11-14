use crate::{LoadedMissionState, MissionEventStop};
use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct ClanAiEvents {
    next_ms: BTreeMap<usize, u32>,
    stops: BTreeMap<(usize, usize, usize), (MissionEventStop, u64)>,
}

impl LoadedMissionState {
    pub(super) fn tick_clan_ai_events(&mut self, now_ms: u32) {
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
            let result = varset.declarations.execute_mission_prefix(
                event,
                bundle.formula_table.as_ref(),
                &mut state.values,
                |instruction, _| Err(format!("{:?}", instruction.dispatch_selector())),
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
}
