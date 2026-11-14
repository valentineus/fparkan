use crate::clan_problem_conditions::Condition;
use crate::{clan_combat_groups::CombatGroup, clan_targeting::CaptureApproach};
use crate::{BehaviorOreState, LoadedMissionState, MissionEventStop};
use fparkan_script::{
    Handler15TargetPayload, Handler2Record, Handler2RecordInput, Handler2RecordKey,
    Handler2RecordScheduler, ScriptDispatchSelector, VarSet, VarSetDefault,
};
use fparkan_world::{identity_metadata, registration_sequence, ObjectHandle, OwnerId, World};
use std::collections::BTreeMap;

const STORAGE_CLASS: u32 = 0x8000_0008;
const RESOURCE_ORE: u32 = 0x0200_0100;
const BUILDING_PROTECT: u32 = 8;

pub(super) struct ScriptFactoryOrder {
    pub owner: OwnerId,
    pub plant: ObjectHandle,
    pub class: u32,
    problem: Handler2RecordKey,
    wait_for_factory: bool,
}

pub(super) fn resolve_captures(
    engine: &mut crate::Engine,
    now_ms: u32,
) -> Result<bool, crate::EngineError> {
    let Some(loaded) = &mut engine.loaded else {
        return Ok(false);
    };
    let mut captured = false;
    let mut index = 0;
    while let Some(&(owner, key, approach)) = loaded.clan_ai_events.capture_orders.get(index) {
        let valid_unit = identity_metadata(&engine.world, approach.unit)
            .is_ok_and(|identity| identity.owner_id == Some(owner));
        let target = identity_metadata(&engine.world, approach.target).ok();
        let target = target
            .and_then(|identity| identity.original_id.zip(identity.owner_id))
            .filter(|&(_, former_owner)| valid_unit && former_owner != owner);
        if let Some((id, former_owner)) = target {
            if !approach.reached(&engine.world) {
                index += 1;
                continue;
            }
            if let Some(class) = loaded
                .object_drafts
                .get(id.0 as usize)
                .and_then(|draft| draft.unit_dat_header_flags)
            {
                loaded.transfer_building(
                    &mut engine.world,
                    approach.target,
                    id,
                    former_owner,
                    owner,
                    class,
                )?;
                captured = true;
                eprintln!(
                    "script_building_captured clan={} unit={} target={} queue_time_ms={now_ms}",
                    owner.0, approach.unit.slot, approach.target.slot
                );
            }
        }
        loaded.clan_ai_events.capture_orders.remove(index);
        let clan = usize::from(owner.0);
        let ai = &mut loaded.clan_ai_events;
        if !valid_unit || !ai.waits.contains_key(&(clan, key)) {
            if let Some(queue) = ai.problems.get_mut(&clan) {
                queue.remove(key);
            }
            ai.waits.remove(&(clan, key));
        }
    }
    Ok(captured)
}

pub(super) fn tick(engine: &mut crate::Engine, now_ms: u32) {
    let Some(loaded) = &mut engine.loaded else {
        return;
    };
    loaded.clan_ai_events.groups.retain(|&(clan, _), group| {
        if !identity_metadata(&engine.world, group.target)
            .is_ok_and(|identity| identity.owner_id == group.target_owner)
        {
            if let Some(patrol) = &mut loaded.unit_patrol {
                for &member in &group.members {
                    patrol.clear_object_order(member, group.target);
                }
            }
            return false;
        }
        group.members.retain(|&member| {
            identity_metadata(&engine.world, member)
                .is_ok_and(|identity| identity.owner_id == Some(OwnerId(clan as u16)))
                && loaded
                    .unit_patrol
                    .as_ref()
                    .is_some_and(|patrol| patrol.has_object_order(member, group.target))
        });
        !group.members.is_empty()
    });
    let ai = &mut loaded.clan_ai_events;
    ai.waits.retain(|&(clan, key), conditions| {
        let group = ai.groups.get(&(clan, key));
        let lost_building = key.word_0 == BUILDING_PROTECT
            && !crate::mission_events::placed_logic_objects(&loaded.mission, &engine.world, key.argument_0)
                .any(|target| identity_metadata(&engine.world, target)
                    .is_ok_and(|identity| identity.owner_id == Some(OwnerId(clan as u16))));
        let ready = lost_building || conditions.iter().any(|&condition| {
            condition.met(
                &engine.world,
                OwnerId(clan as u16),
                loaded.unit_patrol.as_ref(),
                group,
            )
        });
        if ready {
            if let Some(queue) = ai.problems.get_mut(&clan) {
                queue.remove(key);
            }
            if let Some(group) = ai.groups.remove(&(clan, key)) {
                if let Some(patrol) = &mut loaded.unit_patrol {
                    for member in group.members {
                        patrol.clear_object_order(member, group.target);
                    }
                }
            }
            eprintln!("script_problem_condition_met clan={clan} problem={} target={} queue_time_ms={now_ms}", key.word_0, key.argument_0);
            false
        } else {
            true
        }
    });
    let active = &mut loaded.clan_ai_events.factory_jobs;
    active.retain(|((clan, _), plant)| {
        engine
            .production
            .as_ref()
            .is_some_and(|production| production.is_building_at(*plant, OwnerId(*clan)))
    });
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
                if order.wait_for_factory {
                    ai.factory_jobs
                        .push(((order.owner.0, order.problem), order.plant));
                }
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
    factory_jobs: Vec<((u16, Handler2RecordKey), ObjectHandle)>,
    factory_rejections: BTreeMap<(u16, u32), u64>,
    strength_queries: BTreeMap<(usize, u16), u64>,
    capture_queries: BTreeMap<usize, u64>,
    attack_queries: BTreeMap<usize, u64>,
    factory_defence_queries: BTreeMap<(usize, u32), u64>,
    base_threat_queries: BTreeMap<(usize, u32), u64>,
    base_radius_limits: BTreeMap<usize, f32>,
    capturer_queries: BTreeMap<usize, u64>,
    capture_orders: Vec<(OwnerId, Handler2RecordKey, CaptureApproach)>,
    capture_approaches: BTreeMap<(u32, u32), u64>,
    groups: BTreeMap<(usize, Handler2RecordKey), CombatGroup>,
    waits: BTreeMap<(usize, Handler2RecordKey), Vec<Condition>>,
    wait_starts: BTreeMap<(usize, u32), u64>,
    patrol_continues: BTreeMap<(usize, u32), u64>,
    group_orders: BTreeMap<(usize, u32), u64>,
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
        let clock = crate::script_time::elapsed_seconds(self.mission_events.start_ms, now_ms);
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
            let mut problems_event = Some((event_index, event, None::<Handler2Record>));
            // shortcut: only factory protection resumes, on the shared clan interval without native RNG.
            let mut pending = self
                .clan_ai_events
                .problems
                .get(&clan)
                .into_iter()
                .flat_map(|queue| queue.records())
                .rev()
                .filter(|record| {
                    record.key.word_0 == BUILDING_PROTECT
                        && self.clan_ai_events.waits.contains_key(&(clan, record.key))
                })
                .cloned()
                .collect::<Vec<_>>();
            loop {
                let mut next = None;
                while let Some(problem) = pending.pop() {
                    let resume = problems_event.is_some();
                    if !resume && self.clan_ai_events.problem_in_progress(clan, problem.key) {
                        continue;
                    }
                    let problem_id = problem.key.word_0;
                    let Some(kind) = varset.declarations.declarations.iter().find(|entry| {
                        entry.name.starts_with("PBM_")
                            && entry.default_value == VarSetDefault::Dword(problem_id)
                    }) else {
                        continue;
                    };
                    let suffix = if resume { "Continue" } else { "Start" };
                    let event_name = format!("{}_{suffix}\0", kind.name);
                    let Some((index, event)) =
                        bundle.package.events.iter().enumerate().find(|(_, event)| {
                            event.name_raw == event_name.as_bytes()
                                && !event.instructions.is_empty()
                        })
                    else {
                        continue;
                    };
                    if resume {
                        let target = problem.key.argument_0;
                        let count = self
                            .clan_ai_events
                            .patrol_continues
                            .entry((clan, target))
                            .or_default();
                        *count += 1;
                        if *count == 1 {
                            eprintln!("sim_coverage script_factory_patrol_continue_approx=1 clan={clan} target={target}");
                        }
                    } else {
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
                    }
                    next = Some((index, event, Some(problem)));
                    break;
                }
                let Some((event_index, event, problem)) = next.or_else(|| problems_event.take())
                else {
                    break;
                };
                let mut solved = false;
                let mut assignment = None;
                let mut solving = false;
                let mut capturer = None;
                let mut group = None;
                let mut conditions = Vec::new();
                let result = varset.declarations.execute_event_prefix(
                    event,
                    bundle.formula_table.as_ref(),
                    &mut state.values,
                    |instruction, values| match instruction.dispatch_selector() {
                        ScriptDispatchSelector::Handler(9)
                            if instruction.references.is_empty()
                                && instruction.header_words[1] == u32::MAX =>
                        {
                            Ok(None)
                        }
                        ScriptDispatchSelector::Handler(12)
                            if instruction.references.is_empty() =>
                        {
                            let priority = problem
                                .as_ref()
                                .ok_or("Handler(12) requires a current problem")?
                                .scalar_1;
                            Ok(Some(VarSetDefault::FloatBits(priority.to_bits())))
                        }
                        ScriptDispatchSelector::Handler(handler @ (35 | 64)) => {
                            let &[radius, estimate] = instruction.references.as_slice() else {
                                return Err("target query requires a radius and strength output".into());
                            };
                            let radius = values.get(radius as usize).and_then(|cell| cell.as_float())
                                .ok_or("target query requires a float radius")?;
                            if !matches!(values.get(instruction.header_words[1] as usize),
                                Some(VarSetDefault::Dword(_)) | None) {
                                return Err("target query requires a DWORD result".into());
                            }
                            let estimate = values.get_mut(estimate as usize)
                                .ok_or("target query strength output is unavailable")?;
                            let selection = if handler == 64 {
                                crate::clan_targeting::attack_target(
                                    &self.mission, &self.object_drafts, &self.control_device_tables,
                                    world, clan, radius,
                                    self.clan_ai_events.base_radius_limits.get(&clan).copied().unwrap_or(700.0),
                                )
                            } else { crate::clan_targeting::capture_target(
                                &self.mission, &self.object_drafts, &self.control_device_tables,
                                world, clan, radius, |id| {
                                    self.clan_ai_events.problems.get(&clan).is_some_and(|scheduler| {
                                        scheduler.records().iter().any(|record| {
                                            record.key.word_0 == 9
                                                && record.key.argument_0 == id
                                        })
                                    })
                                },
                            ) };
                            let (target, strength) = selection.unwrap_or((u32::MAX, 1e12_f32));
                            let strength = crate::clan_targeting::encode_strength(strength);
                            *estimate = match *estimate {
                                VarSetDefault::Dword(_) => VarSetDefault::Dword(
                                    problem_dword(strength.to_bits())
                                        .ok_or("target strength is outside the DWORD range")?
                                ),
                                VarSetDefault::FloatBits(_) => VarSetDefault::FloatBits(strength.to_bits()),
                            };
                            let (queries, name) = if handler == 64 {
                                (&mut self.clan_ai_events.attack_queries, "script_attack_target_approx")
                            } else {
                                (&mut self.clan_ai_events.capture_queries, "script_capture_target_approx")
                            };
                            let count = queries.entry(clan).or_default();
                            *count += 1;
                            if *count == 1 {
                                eprintln!("sim_coverage {name}=1 clan={clan} target={target} strength={strength}");
                            }
                            Ok(Some(VarSetDefault::Dword(target)))
                        }
                        ScriptDispatchSelector::Handler(25) if problem.is_some() => {
                            let &[mode, hits, target_mode, target] = instruction.references.as_slice() else {
                                return Err("Handler(25) requires TAKE_BY_HITS and a logical ID".into());
                            };
                            let word = |index: u32| values.get(index as usize)?.as_dword();
                            let (Some(2), Some(requested), Some(0x201), Some(target)) =
                                (word(mode), word(hits), word(target_mode), word(target))
                            else {
                                return Err("Handler(25) requires TAKE_BY_HITS and a logical ID".into());
                            };
                            // shortcut: use free warbots until lower-priority orders can be preempted.
                            let units = self.mission_events.clan_units(world, OwnerId(clan as u16), true)
                                .filter(|&unit| !self.clan_ai_events.unit_assigned(unit))
                                .filter(|&unit| self.unit_patrol.as_ref().is_none_or(|patrol| !patrol.is_shut_down(unit)));
                            let (recruited, strength) = crate::clan_combat_groups::recruit(
                                world, units, &self.control_device_tables, &self.mission, target, requested,
                            );
                            if strength < requested as f32 || strength == 0.0 {
                                values[hits as usize] = VarSetDefault::Dword(strength as u32);
                            }
                            group = recruited;
                            // shortcut: group IDs stay local until groups are script-visible across events.
                            Ok(Some(VarSetDefault::Dword(if group.is_some() { 0 } else { u32::MAX })))
                        }
                        ScriptDispatchSelector::Handler(28) if problem.is_some() => {
                            let order = varset.declarations.resolve_handler15_with_values(instruction, values)
                                .map_err(|error| error.to_string())?;
                            let group = group.take().ok_or("Handler(28) requires an active combat group")?;
                            let patrol = self.unit_patrol.get_or_insert_with(crate::unit_patrol::UnitPatrol::scripted);
                            group.dispatch(patrol, world, &self.mission, order)
                                .ok_or("Handler(28) requires a patrol or attack on the group's target")?;
                            let key = problem.as_ref().expect("group order requires a problem").key;
                            let target = group.target.slot;
                            let count = self.clan_ai_events.group_orders.entry((clan, target)).or_default();
                            *count += 1;
                            eprintln!("sim_coverage script_group_order_approx=1 clan={clan} target={target} order={} units={:?}", order.word_1, group.members.iter().map(|unit| unit.slot).collect::<Vec<_>>());
                            let groups = &mut self.clan_ai_events.groups;
                            if let Some(assigned) = groups.get_mut(&(clan, key))
                                .filter(|assigned| key.word_0 == BUILDING_PROTECT
                                    && assigned.target == group.target)
                            {
                                assigned.members.extend(group.members);
                            } else {
                                groups.insert((clan, key), group);
                            }
                            Ok(Some(VarSetDefault::Dword(1)))
                        }
                        ScriptDispatchSelector::Handler(38) => {
                            let (target, mode) = instruction.dword_pair(values)
                                .ok_or("Handler(38) requires a clan and unit-selection mode")?;
                            if mode > 1 {
                                return Err("Handler(38) requires FREE_UNITS or ALL_UNITS".into());
                            }
                            let Ok(target) = u16::try_from(target) else {
                                return Ok(Some(VarSetDefault::Dword(0)));
                            };
                            let sum = self.mission_events.clan_units(world, OwnerId(target), mode == 0)
                                .filter(|&handle| mode != 0 || !self.clan_ai_events.unit_assigned(handle))
                                .map(|handle| crate::clan_combat_strength::unit_strength(&self.control_device_tables, handle))
                                .fold(0.0, |sum, contribution| sum + contribution);
                            let count = self.clan_ai_events.strength_queries.entry((clan, target)).or_default();
                            *count += 1;
                            if *count == 1 {
                                eprintln!("sim_coverage script_combat_strength_approx=1 clan={clan} target={target} strength={sum}");
                            }
                            Ok(Some(VarSetDefault::Dword(sum as u32)))
                        }
                        ScriptDispatchSelector::Handler(59 | 60) => {
                            crate::script_time::result(instruction, values, clock)
                                .map(|result| Some(VarSetDefault::Dword(result)))
                                .ok_or_else(|| "script time requires one DWORD operand".into())
                        }
                        ScriptDispatchSelector::Handler(11) => {
                            let class = single_dword(&instruction.references, values)
                                .ok_or("Handler(11) requires one DWORD class")?;
                            if matches!(class, 0x8000_0040 | 0x8000_0400) {
                                return Err(
                                    "Handler(11) requires the pending hangar or laboratory counter"
                                        .into(),
                                );
                            }
                            let Some(limit_var) = class_limit_variable(class) else {
                                return Ok(Some(VarSetDefault::Dword(0)));
                            };
                            let limit = varset
                                .declarations
                                .declarations
                                .iter()
                                .position(|entry| entry.name == limit_var)
                                .and_then(|index| values.get(index)?.as_dword())
                                .ok_or("Handler(11) class limit is unavailable")?;
                            let count = self.mission_events.count_clan_class(
                                world,
                                OwnerId(clan as u16),
                                class,
                            );
                            // shortcut: include unfinished structures once construction registers with the roster.
                            Ok(Some(VarSetDefault::Dword(u32::from(count >= limit))))
                        }
                        ScriptDispatchSelector::Handler(13)
                            if instruction.dword_pair(values)
                                == Some((0, crate::mining::MINE_CLASS)) =>
                        {
                            let id = self.mission_events.first_available_placed_unit(
                                &self.mission,
                                world,
                                clan,
                                crate::mining::MINE_CLASS,
                            );
                            Ok(Some(VarSetDefault::Dword(id.unwrap_or(u32::MAX))))
                        }
                        ScriptDispatchSelector::Handler(15) => {
                            let order = varset
                                .declarations
                                .resolve_handler15_with_values(instruction, values)
                                .map_err(|error| error.to_string())?;
                            if let (0x11, 3, Handler15TargetPayload::ByLogicId(target_id)) =
                                (order.word_1, order.word_2, order.target)
                            {
                                let unit = capturer.filter(|unit: &crate::clan_targeting::Capturer| unit.id == order.word_0)
                                    .ok_or("Handler(15) capture requires a selected capturer")?;
                                let problem = problem.as_ref()
                                    .ok_or("Handler(15) capture requires a current problem")?;
                                let approach = crate::clan_targeting::send_capturer(
                                    &mut self.unit_patrol, world, &self.object_drafts, &self.mission_assets, unit, target_id,
                                );
                                if let Some(approach) = approach {
                                    self.clan_ai_events.capture_orders.push((OwnerId(clan as u16), problem.key, approach));
                                    *self.clan_ai_events.capture_approaches.entry((approach.unit.slot, approach.target.slot)).or_default() += 1;
                                }
                                return Ok(Some(VarSetDefault::Dword(u32::from(approach.is_some()))));
                            }
                            if order.word_1 == 0x0a
                                && order.word_2 == 3
                                && order.word_3 == 0
                                && matches!(order.target, Handler15TargetPayload::NotDefined(_))
                            {
                                let mine = crate::script_patrol::unit_by_logic_id(
                                    &self.mission,
                                    &self.object_drafts,
                                    world,
                                    order.word_0,
                                    |class| class == crate::mining::MINE_CLASS,
                                )
                                .filter(|&handle| {
                                    identity_metadata(world, handle).is_ok_and(|identity| {
                                        identity.owner_id == Some(OwnerId(clan as u16))
                                    })
                                });
                                let available = mine.is_some_and(|handle| {
                                    crate::mining::resume_mining(
                                        &mut self.mine_sites,
                                        world,
                                        handle,
                                    )
                                });
                                return Ok(Some(VarSetDefault::Dword(u32::from(available))));
                            }
                            if problem.is_none() {
                                return Err("Handler(15) order is unavailable in Problems0".into());
                            }
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
                                problem: problem
                                    .as_ref()
                                    .expect("factory order requires a current problem")
                                    .key,
                                wait_for_factory: false,
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
                        ScriptDispatchSelector::Handler(handler @ (7 | 45))
                            if instruction.references.is_empty()
                                && problem.as_ref().is_some_and(|record| record.key.word_0 == BUILDING_PROTECT) =>
                        {
                            let key = problem.as_ref().expect("factory protection problem").key;
                            if handler == 7 {
                                if let Some(assigned) = self.clan_ai_events.groups.remove(&(clan, key)) {
                                    if let Some(patrol) = &mut self.unit_patrol {
                                        for unit in assigned.members {
                                            patrol.clear_object_order(unit, assigned.target);
                                        }
                                    }
                                }
                                return Ok(None);
                            }
                            let strength = self.clan_ai_events.groups.get(&(clan, key))
                                .into_iter().flat_map(|assigned| &assigned.members)
                                .map(|&unit| crate::clan_combat_strength::unit_strength(&self.control_device_tables, unit))
                                .fold(0.0, |sum, power| sum + power);
                            Ok(Some(VarSetDefault::Dword(strength as i64 as u32)))
                        }
                        ScriptDispatchSelector::Handler(8) if problem.is_some() => {
                            match single_dword(&instruction.references, values) {
                                Some(1)
                                    if problem
                                        .as_ref()
                                        .is_some_and(|record| matches!(record.key.word_0, 6 | BUILDING_PROTECT))
                                        || !conditions.is_empty() =>
                                {
                                    solving = true
                                }
                                Some(2) => solved = true,
                                _ => {
                                    return Err(
                                        "Handler(8) requires a supported problem transition".into(),
                                    )
                                }
                            }
                            Ok(None)
                        }
                        ScriptDispatchSelector::Handler(27) if problem.is_some() => {
                            let [action, mode, id] = instruction.references.as_slice() else {
                                return Err("Handler(27) requires an action and target".into());
                            };
                            let word = |index: u32| {
                                values.get(index as usize).and_then(|cell| cell.as_dword())
                            };
                            let (Some(action), Some(mode), Some(id)) =
                                (word(*action), word(*mode), word(*id))
                            else {
                                return Err("Handler(27) requires three DWORD operands".into());
                            };
                            let key = problem.as_ref().expect("condition requires a problem").key;
                            let condition = match (action, mode) {
                                (1 | 3, 0) if matches!(key.word_0, 7 | 9) || (action == 1 && key.word_0 == BUILDING_PROTECT) => {
                                    let target = crate::mission_events::placed_logic_objects(
                                        &self.mission, world, id
                                    ).next();
                                    Some(if action == 1 { Condition::ObjectDestroyed(target) } else {
                                        Condition::BuildingCaptured(target.ok_or("Handler(27) capture target is unavailable")?)
                                    })
                                }
                                (4 | 5, 1) if id == 0 && self.clan_ai_events.groups.contains_key(&(clan, key)) => {
                                    Some(if action == 4 { Condition::GroupDestroyed } else { Condition::GroupIdle })
                                }
                                (2, 0) if solving => None,
                                _ => return Err("Handler(27) action or target mode is unsupported".into()),
                            };
                            if let Some(condition) = condition {
                                conditions.push(condition);
                                return Ok(None);
                            }
                            let order = orders
                                .last_mut()
                                .filter(|order| {
                                    order.owner == OwnerId(clan as u16)
                                        && Some(order.problem)
                                            == problem.as_ref().map(|record| record.key)
                                })
                                .ok_or("Handler(27) requires an active construction order")?;
                            if assignment != Some(order.plant)
                                || !crate::mission_events::placed_logic_objects(
                                    &self.mission,
                                    world,
                                    id,
                                )
                                .any(|target| target == order.plant)
                            {
                                return Err("Handler(27) requires the assigned factory".into());
                            }
                            order.wait_for_factory = true;
                            Ok(None)
                        }
                        ScriptDispatchSelector::Handler(14) if problem.is_some() => {
                            if let &[mode, target_mode, target] = instruction.references.as_slice() {
                                let word = |index: u32| values.get(index as usize)?.as_dword();
                                let (Some(4), Some(0x201), Some(target)) =
                                    (word(mode), word(target_mode), word(target))
                                else {
                                    return Err("Handler(14) requires a logical ID for nearest capturer selection".into());
                                };
                                capturer = crate::clan_targeting::nearest_capturer(
                                    &self.mission, &self.object_drafts, &self.mission_events,
                                    world, OwnerId(clan as u16), target, |handle| {
                                        !self.clan_ai_events.unit_assigned(handle)
                                            && self.unit_patrol.as_ref().is_none_or(|patrol| !patrol.is_shut_down(handle))
                                    },
                                );
                                let id = capturer.map_or(u32::MAX, |unit| unit.id);
                                let count = self.clan_ai_events.capturer_queries.entry(clan).or_default();
                                *count += 1;
                                if *count == 1 {
                                    eprintln!("sim_coverage script_nearest_capturer_approx=1 clan={clan} target={target} unit={id}");
                                }
                                return Ok(Some(VarSetDefault::Dword(id)));
                            }
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
                            let argument =
                                match single_dword(&instruction.references, values) {
                                    Some(0) => problem.key.argument_0,
                                    Some(1) => problem.key.argument_1,
                                    Some(2) => problem.argument_2,
                                    _ => return Err(
                                        "Handler(29) requires a problem argument index from 0 to 2"
                                            .into(),
                                    ),
                                };
                            let result = match values.get(instruction.header_words[1] as usize) {
                                Some(VarSetDefault::FloatBits(_)) => VarSetDefault::FloatBits(argument),
                                _ => VarSetDefault::Dword(argument),
                            };
                            Ok(Some(result))
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
                        ScriptDispatchSelector::Handler(58) => {
                            single_dword(&instruction.references, values)
                                .ok_or("Handler(58) requires a DWORD building index")?;
                            // shortcut: an unavailable upgrade catalog exposes no eligible buildings.
                            self.clan_ai_events.log_research_stub(clan, 58, &bundle.script_path);
                            Ok(Some(VarSetDefault::Dword(u32::MAX)))
                        }
                        ScriptDispatchSelector::Handler(handler @ (47 | 67)) => {
                            let (output, selection) = match (handler, instruction.references.as_slice()) {
                                (47, &[extension, output]) => {
                                    let extension = values.get(extension as usize).and_then(|cell| cell.as_dword())
                                        .ok_or("Handler(47) requires a DWORD extension of the base radius")?;
                                    let selection = crate::clan_targeting::base_threat(
                                        &self.mission, &self.object_drafts, &self.control_device_tables,
                                        world, clan, extension,
                                        self.clan_ai_events.base_radius_limits.get(&clan).copied().unwrap_or(700.0),
                                        |id| self.clan_ai_events.problems.get(&clan).is_some_and(|queue| {
                                            queue.records().iter().any(|record| record.key.word_0 == 12 && record.key.argument_0 == id)
                                        }),
                                    );
                                    (output, selection)
                                }
                                (67, &[output]) => {
                                    let units = self.mission_events.clan_units(world, OwnerId(clan as u16), false);
                                    (output, crate::clan_targeting::factory_defence(world, &self.object_drafts, &self.control_device_tables, units))
                                }
                                _ => return Err("defence query requires a strength output".into()),
                            };
                            let cell = values.get_mut(output as usize)
                                .ok_or("defence query strength output is unavailable")?;
                            let (id, power) = selection.unwrap_or((u32::MAX, 0.0));
                            let power = crate::clan_targeting::encode_strength(power);
                            *cell = match *cell {
                                VarSetDefault::Dword(_) => VarSetDefault::Dword(power as u32),
                                VarSetDefault::FloatBits(_) => VarSetDefault::FloatBits(power.to_bits()),
                            };
                            let (queries, name) = if handler == 47 {
                                (&mut self.clan_ai_events.base_threat_queries, "script_base_threat_approx")
                            } else {
                                (&mut self.clan_ai_events.factory_defence_queries, "script_factory_defence_approx")
                            };
                            let count = queries.entry((clan, id)).or_default();
                            *count += 1;
                            if *count == 1 {
                                eprintln!("sim_coverage {name}=1 clan={clan} target={id} strength={power}");
                            }
                            Ok(Some(VarSetDefault::Dword(id)))
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
                    } else if solving
                        && (!conditions.is_empty() || problem.key.word_0 == BUILDING_PROTECT)
                    {
                        let count = self
                            .clan_ai_events
                            .wait_starts
                            .entry((clan, problem.key.word_0))
                            .or_default();
                        *count += 1;
                        if *count == 1 {
                            eprintln!("sim_coverage script_problem_wait_approx=1 clan={clan} problem={} target={} conditions={}", problem.key.word_0, problem.key.argument_0, conditions.len());
                        }
                        self.clan_ai_events
                            .waits
                            .insert((clan, problem.key), conditions);
                    }
                } else if let Some(queue) = self.clan_ai_events.problems.get(&clan) {
                    pending = ranked_problems(queue.records());
                }
            }
        }
        orders
    }
}

fn class_limit_variable(class: u32) -> Option<&'static str> {
    match class {
        0x0100_2000 => Some("dMaxTransport"),
        0x0100_4000 => Some("dMaxBuilder"),
        crate::mining::MINE_CLASS => Some("dMaxMine"),
        STORAGE_CLASS => Some("dMaxStorage"),
        crate::autodemo::PLANT_CLASS => Some("dMaxPlant"),
        0x8010_0000 | 0x8020_0000 => Some("dMaxTower"),
        _ => None,
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
    let argument = |index| match value(index)? {
        VarSetDefault::Dword(value) | VarSetDefault::FloatBits(value) => Some(value),
    };
    Some((
        varset.declarations.get(kind as usize)?.name.as_str(),
        Handler2RecordInput {
            word_0: word(kind)?,
            scalar_1: scalar(priority)?,
            word_2: word(increment)?,
            word_3: word(sender)?,
            argument_0: argument(first)?,
            argument_1: argument(second)?,
            argument_2: argument(third)?,
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
    pub(super) fn base_threat_queries(&self) -> impl Iterator<Item = (usize, u32, u64)> + '_ {
        self.base_threat_queries
            .iter()
            .map(|(&(clan, id), &count)| (clan, id, count))
    }

    pub(super) fn factory_defence_queries(&self) -> impl Iterator<Item = (usize, u32, u64)> + '_ {
        self.factory_defence_queries
            .iter()
            .map(|(&(clan, id), &count)| (clan, id, count))
    }

    pub(super) fn wait_starts(&self) -> impl Iterator<Item = (usize, u32, u64)> + '_ {
        self.wait_starts
            .iter()
            .map(|(&(clan, problem), &count)| (clan, problem, count))
    }

    pub(super) fn patrol_continues(&self) -> impl Iterator<Item = (usize, u32, u64)> + '_ {
        self.patrol_continues
            .iter()
            .map(|(&(clan, target), &count)| (clan, target, count))
    }

    fn problem_in_progress(&self, clan: usize, key: Handler2RecordKey) -> bool {
        self.factory_jobs
            .iter()
            .any(|&(job, _)| job == (clan as u16, key))
            || self
                .capture_orders
                .iter()
                .any(|&(owner, assigned, _)| owner == OwnerId(clan as u16) && assigned == key)
            || self.groups.contains_key(&(clan, key))
            || self.waits.contains_key(&(clan, key))
    }

    fn unit_assigned(&self, unit: ObjectHandle) -> bool {
        self.capture_orders
            .iter()
            .any(|&(_, _, order)| order.unit == unit)
            || self
                .groups
                .values()
                .any(|group| group.members.contains(&unit))
    }

    pub(super) fn group_orders(&self) -> impl Iterator<Item = (usize, u32, u64)> + '_ {
        self.group_orders
            .iter()
            .map(|(&(clan, target), &count)| (clan, target, count))
    }

    pub(super) fn set_base_radius(&mut self, clan: usize, limit: f32) {
        if limit.is_finite() && limit > 0.0 {
            self.base_radius_limits.insert(clan, limit);
        }
    }

    pub(super) fn attack_queries(&self) -> impl Iterator<Item = (usize, u64)> + '_ {
        self.attack_queries
            .iter()
            .map(|(&clan, &count)| (clan, count))
    }

    pub(super) fn capture_approaches(&self) -> impl Iterator<Item = (u32, u32, u64)> + '_ {
        self.capture_approaches
            .iter()
            .map(|(&(unit, target), &count)| (unit, target, count))
    }

    pub(super) fn capture_queries(&self) -> impl Iterator<Item = (usize, u64)> + '_ {
        self.capture_queries
            .iter()
            .map(|(&clan, &count)| (clan, count))
    }

    pub(super) fn capturer_queries(&self) -> impl Iterator<Item = (usize, u64)> + '_ {
        self.capturer_queries
            .iter()
            .map(|(&clan, &count)| (clan, count))
    }

    pub(super) fn strength_queries(&self) -> impl Iterator<Item = (usize, u16, u64)> + '_ {
        self.strength_queries
            .iter()
            .map(|(&(clan, target), &count)| (clan, target, count))
    }
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
    #[ignore = "requires licensed corpus"]
    fn licensed_factory_protection_resumes_and_assigns_a_patrol_from_stock_start() {
        use crate::{
            create, load_gameplay_mission, simulate_mission_report, EngineServices, MissionMode,
            MissionRequest, MissionStartupInput,
        };
        use fparkan_vfs::DirectoryVfs;
        use std::sync::Arc;

        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed corpus root");
        let mut engine = create(EngineServices::new(Arc::new(DirectoryVfs::new(root)))).unwrap();
        load_gameplay_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/Single.01/data.tma".into(),
                mode: MissionMode::SingleMission,
            },
            MissionStartupInput {
                queue_time_ms: 0,
                research_creation_gate: false,
            },
            None,
        )
        .unwrap();

        let report = simulate_mission_report(&mut engine, 25_000).unwrap();
        let [(clan, factory_id, count)] = report.script_factory_patrol_continues.as_slice() else {
            panic!("expected one active factory protection problem");
        };
        assert_eq!(*clan, 1);
        assert!(*count > 1);
        let loaded = engine.loaded.as_ref().unwrap();
        let factory = crate::mission_events::placed_logic_objects(
            &loaded.mission,
            &engine.world,
            *factory_id,
        )
        .next()
        .unwrap();
        let (key, group) = loaded
            .clan_ai_events
            .groups
            .iter()
            .find(|(&(owner, key), _)| {
                owner == *clan && key.word_0 == BUILDING_PROTECT && key.argument_0 == *factory_id
            })
            .unwrap();
        assert_eq!(group.target, factory);
        assert_eq!(group.target_owner, Some(OwnerId(1)));
        assert!(!group.members.is_empty());
        let patrol = loaded.unit_patrol.as_ref().unwrap();
        assert!(group
            .members
            .iter()
            .all(|&unit| patrol.has_object_order(unit, factory)));
        assert!(loaded.clan_ai_events.waits.contains_key(key));
    }

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
            VarSetDefault::Dword(0x8000_0002),
        ];
        let (name, problem) = problem_operands(&varset, &values, &[0, 1, 1, 2, 2, 1, 0]).unwrap();
        assert_eq!(name, "PBM_ROBOT_NEEDED");
        assert_eq!(
            problem,
            Handler2RecordInput {
                word_0: 6,
                scalar_1: 5.75,
                word_2: 5,
                word_3: 0x8000_0002,
                argument_0: 0x8000_0002,
                argument_1: 5.75_f32.to_bits(),
                argument_2: 6,
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
                argument_0: 0,
                argument_1: 0,
                argument_2: 0,
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
