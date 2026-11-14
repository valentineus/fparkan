use crate::{BehaviorOreState, LoadedMissionState, MissionEventStop};
use fparkan_script::{
    Handler15TargetPayload, Handler2Record, Handler2RecordInput, Handler2RecordScheduler,
    ScriptDispatchSelector, VarSet, VarSetDefault,
};
use fparkan_world::{identity_metadata, registration_sequence, ObjectHandle, OwnerId, World};
use std::collections::BTreeMap;

const STORAGE_CLASS: u32 = 0x8000_0008;
const RESOURCE_ORE: u32 = 0x0200_0100;

pub(super) struct ScriptFactoryOrder {
    pub owner: OwnerId,
    pub plant: ObjectHandle,
    pub class: u32,
}

pub(super) fn tick(engine: &mut crate::Engine, now_ms: u32) {
    let Some(loaded) = &mut engine.loaded else {
        return;
    };
    for order in loaded.tick_clan_ai_events(&engine.world, now_ms) {
        match crate::production::order_script_factory(
            engine,
            order.owner,
            order.plant,
            order.class,
            now_ms,
        ) {
            Ok(duration) => {
                let ai = &mut engine
                    .loaded
                    .as_mut()
                    .expect("tick requires a loaded mission")
                    .clan_ai_events;
                *ai.factory_orders
                    .entry((order.owner.0, order.plant.slot))
                    .or_default() += 1;
                eprintln!(
                    "script_factory_order_approx clan={} plant={} duration_ms={duration} queue_time_ms={now_ms}",
                    order.owner.0, order.plant.slot,
                );
            }
            Err(error) => {
                let ai = &mut engine
                    .loaded
                    .as_mut()
                    .expect("tick requires a loaded mission")
                    .clan_ai_events;
                let count = ai
                    .factory_rejections
                    .entry((order.owner.0, order.plant.slot))
                    .or_default();
                *count += 1;
                if *count == 1 {
                    eprintln!(
                        "sim_coverage script_factory_rejected=1 clan={} plant={} reason={error}",
                        order.owner.0, order.plant.slot
                    );
                }
            }
        }
    }
}

#[derive(Default)]
pub(super) struct ClanAiEvents {
    next_ms: BTreeMap<usize, u32>,
    stops: BTreeMap<(usize, usize, usize), (MissionEventStop, u64)>,
    research_stubs: BTreeMap<(usize, u8), u64>,
    problems: BTreeMap<usize, Handler2RecordScheduler>,
    started: BTreeMap<(usize, u32), u64>,
    factory_orders: BTreeMap<(u16, u32), u64>,
    factory_rejections: BTreeMap<(u16, u32), u64>,
}

impl LoadedMissionState {
    pub(super) fn tick_clan_ai_events(
        &mut self,
        world: &World,
        now_ms: u32,
    ) -> Vec<ScriptFactoryOrder> {
        let mut orders = Vec::new();
        let Some(varset) = &self.script_varset else {
            return orders;
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
            let mut next = Some((event_index, event, None));
            let mut pending = Vec::new();
            while let Some((event_index, event, problem)) = next.take() {
                let mut solved = false;
                let mut assignment = None;
                let result = varset.declarations.execute_event_prefix(
                    event,
                    bundle.formula_table.as_ref(),
                    &mut state.values,
                    |instruction, values| match instruction.dispatch_selector() {
                        ScriptDispatchSelector::Handler(11)
                            if single_dword(&instruction.references, values)
                                == Some(crate::boarding::WARBOT_CLASS) =>
                        {
                            // Behavior has no count cap for ordinary warbots in Handler(11).
                            Ok(Some(VarSetDefault::Dword(0)))
                        }
                        ScriptDispatchSelector::Handler(15) if problem.is_some() => {
                            let order = varset
                                .declarations
                                .resolve_handler15_with_values(instruction, values)
                                .map_err(|error| error.to_string())?;
                            if order.word_1 != 0x0c
                                || order.word_2 != 1
                                || order.word_3 != crate::boarding::WARBOT_CLASS
                                || !matches!(order.target, Handler15TargetPayload::ByName(_))
                            {
                                return Err(
                                    "Handler(15) only supports the scripted warbot factory order"
                                        .into(),
                                );
                            }
                            let owner = OwnerId(clan as u16);
                            let plant = crate::mission_events::placed_logic_objects(
                                &self.mission,
                                world,
                                order.word_0,
                            )
                            .find(|&handle| {
                                identity_metadata(world, handle)
                                    .is_ok_and(|identity| identity.owner_id == Some(owner))
                            });
                            let Some(plant) = plant else {
                                return Ok(Some(VarSetDefault::Dword(5)));
                            };
                            orders.push(ScriptFactoryOrder {
                                owner,
                                plant,
                                class: order.word_3,
                            });
                            // shortcut: queue the order for the host to apply after this script pass.
                            Ok(Some(VarSetDefault::Dword(1)))
                        }
                        ScriptDispatchSelector::Handler(6) if problem.is_some() => {
                            let (id, critical) = instruction.dword_pair(values).ok_or(
                                "Handler(6) requires a placed unit and its assignment mode",
                            )?;
                            if critical > 1 {
                                return Err(
                                    "Handler(6) only supports a plain or critical assignment"
                                        .into(),
                                );
                            }
                            assignment = crate::mission_events::placed_logic_objects(
                                &self.mission,
                                world,
                                id,
                            )
                            .find(|&handle| {
                                identity_metadata(world, handle).is_ok_and(|identity| {
                                    identity.owner_id == Some(OwnerId(clan as u16))
                                })
                            });
                            Ok(None)
                        }
                        ScriptDispatchSelector::Handler(8) if problem.is_some() => {
                            if single_dword(&instruction.references, values) != Some(2) {
                                return Err("Handler(8) only supports ST_SOLVED".into());
                            }
                            solved = true;
                            Ok(None)
                        }
                        ScriptDispatchSelector::Handler(14) if problem.is_some() => {
                            let (mode, class) = instruction
                                .dword_pair(values)
                                .ok_or("Handler(14) requires a unit mode and class")?;
                            if mode != 2 {
                                return Err("Handler(14) only supports UNIT_ANY_UNIT".into());
                            }
                            // shortcut: only unassigned placed objects until PBM owns roster slots.
                            let id = self.mission_events.first_available_placed_unit(
                                &self.mission,
                                world,
                                clan,
                                class,
                            );
                            Ok(Some(VarSetDefault::Dword(id.unwrap_or(u32::MAX))))
                        }
                        ScriptDispatchSelector::Handler(29) => {
                            let problem: &Handler2Record = problem
                                .as_ref()
                                .ok_or("Handler(29) requires a current problem")?;
                            let bits =
                                match single_dword(&instruction.references, values) {
                                    Some(0) => problem.key.scalar_4_bits,
                                    Some(1) => problem.key.scalar_5_bits,
                                    Some(2) => problem.scalar_6.to_bits(),
                                    _ => return Err(
                                        "Handler(29) requires a problem argument index from 0 to 2"
                                            .into(),
                                    ),
                                };
                            Ok(Some(VarSetDefault::Dword(problem_dword(bits).ok_or(
                                "Handler(29) argument is outside the supported DWORD range",
                            )?)))
                        }
                        ScriptDispatchSelector::Handler(0)
                            if problem.is_some() && instruction.references.len() == 1 =>
                        {
                            Ok(Some(VarSetDefault::Dword(1)))
                        }
                        ScriptDispatchSelector::Handler(41) if problem.is_some() => {
                            let id = single_dword(&instruction.references, values)
                                .ok_or("Handler(41) requires a laboratory logical ID")?;
                            let exists = crate::mission_events::placed_logic_objects(
                                &self.mission,
                                world,
                                id,
                            )
                            .any(|handle| Some(handle) == assignment);
                            // shortcut: an unloaded candidate catalog rejects the order; wire lab dispatch with Handler(43).
                            self.clan_ai_events
                                .log_research_stub(clan, 41, &bundle.script_path);
                            Ok(Some(VarSetDefault::Dword(if exists {
                                0
                            } else {
                                u32::MAX
                            })))
                        }
                        ScriptDispatchSelector::Handler(2) => {
                            let (name, input) = problem_operands(
                                &varset.declarations,
                                values,
                                &instruction.references,
                            )
                            .ok_or("Handler(2) requires seven numeric problem operands")?;
                            let start = format!("{name}_Start\0");
                            let resume = format!("{name}_Continue\0");
                            if bundle
                                .package
                                .events
                                .iter()
                                .any(|event| event.name_raw == start.as_bytes())
                                && bundle
                                    .package
                                    .events
                                    .iter()
                                    .any(|event| event.name_raw == resume.as_bytes())
                            {
                                self.clan_ai_events
                                    .problems
                                    .entry(clan)
                                    .or_default()
                                    .submit(input);
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
                            if instruction.dword_pair(values)
                                == Some((STORAGE_CLASS, RESOURCE_ORE)) =>
                        {
                            let remaining = storage_space(
                                &self.behavior_ore_states,
                                world,
                                OwnerId(clan as u16),
                            );
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
                        ScriptDispatchSelector::Handler(49)
                            if instruction.references.is_empty() =>
                        {
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
                            let Some(&VarSetDefault::Dword(class)) = values.get(index as usize)
                            else {
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
                        ScriptDispatchSelector::Handler(handler @ (56 | 65))
                            if instruction.references.is_empty() =>
                        {
                            // shortcut: keep research disabled until the native policy flag and catalog are wired.
                            self.clan_ai_events.log_research_stub(
                                clan,
                                handler,
                                &bundle.script_path,
                            );
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
                if let Some(problem) = problem {
                    if solved {
                        self.clan_ai_events
                            .problems
                            .entry(clan)
                            .or_default()
                            .remove(problem.key);
                    }
                } else if let Some(queue) = self.clan_ai_events.problems.get(&clan) {
                    pending = ranked_problems(queue.records());
                }
                // shortcut: retry unresolved Starts until decay and Continue are wired.
                while let Some(problem) = pending.pop() {
                    let problem_id = problem.key.word_0;
                    let Some(kind) = varset.declarations.declarations.iter().find(|entry| {
                        entry.name.starts_with("PBM_")
                            && entry.default_value == VarSetDefault::Dword(problem_id)
                    }) else {
                        continue;
                    };
                    let start = format!("{}_Start\0", kind.name);
                    let Some((index, event)) =
                        bundle.package.events.iter().enumerate().find(|(_, event)| {
                            event.name_raw == start.as_bytes() && !event.instructions.is_empty()
                        })
                    else {
                        continue;
                    };
                    let count = self
                        .clan_ai_events
                        .started
                        .entry((clan, problem_id))
                        .or_default();
                    *count += 1;
                    if *count == 1 {
                        eprintln!(
                            "sim_coverage script_problem_start_approx=1 clan={clan} problem={}",
                            kind.name
                        );
                    }
                    next = Some((index, event, Some(problem)));
                    break;
                }
            }
        }
        orders
    }
}

fn ranked_problems(problems: &[Handler2Record]) -> Vec<Handler2Record> {
    let mut pending: Vec<_> = problems
        .iter()
        .filter(|record| record.scalar_1 > 0.0)
        .cloned()
        .collect();
    pending.sort_by(|a, b| b.scalar_1.total_cmp(&a.scalar_1));
    pending.reverse();
    pending
}

fn single_dword(references: &[u32], values: &[VarSetDefault]) -> Option<u32> {
    let &[index] = references else {
        return None;
    };
    values.get(index as usize)?.as_dword()
}

fn problem_dword(bits: u32) -> Option<u32> {
    let value = f64::from(f32::from_bits(bits));
    let limit = 9_223_372_036_854_775_808.0;
    (-limit..limit)
        .contains(&value)
        .then(|| value.trunc() as i64 as u32)
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
        VarSetDefault::FloatBits(bits) => problem_dword(bits),
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
    fn log_research_stub(&mut self, clan: usize, handler: u8, script: &str) {
        let count = self.research_stubs.entry((clan, handler)).or_default();
        *count += 1;
        if *count == 1 {
            eprintln!("sim_coverage script_research_status_stub=1 clan={clan} handler={handler} script={script}");
        }
    }

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

    pub(super) fn started_problems(&self) -> impl Iterator<Item = (usize, u32, u64)> + '_ {
        self.started
            .iter()
            .map(|(&(clan, kind), &count)| (clan, kind, count))
    }

    pub(super) fn factory_orders(&self) -> impl Iterator<Item = (u16, u32, u64)> + '_ {
        self.factory_orders
            .iter()
            .map(|(&(clan, plant), &count)| (clan, plant, count))
    }

    pub(super) fn factory_rejections(&self) -> impl Iterator<Item = (u16, u32, u64)> + '_ {
        self.factory_rejections
            .iter()
            .map(|(&(clan, plant), &count)| (clan, plant, count))
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
    fn problem_selection_requires_a_positive_priority_and_preserves_order_on_ties() {
        let mut queue = Handler2RecordScheduler::default();
        for (word_0, scalar_1) in [(1, 0.0), (2, -3.0), (3, 0.5), (4, 0.5)] {
            queue.submit(Handler2RecordInput {
                word_0,
                scalar_1,
                word_2: 0,
                word_3: 0,
                scalar_4: 0.0,
                scalar_5: 0.0,
                scalar_6: 0.0,
            });
        }
        let first = ranked_problems(queue.records()).pop().unwrap().key;
        assert_eq!(first.word_0, 3);
        queue.remove(first);
        assert_eq!(
            ranked_problems(queue.records()).pop().unwrap().key.word_0,
            4
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
