use crate::clan_factory_schemes::TRANSPORT_CLASS;
use crate::clan_problem_conditions::Condition;
use crate::{
    clan_combat_groups::{CombatGroup, GroupTarget},
    clan_targeting::CaptureApproach,
};
use crate::{BehaviorOreState, LoadedMissionState, MissionEventStop, MissionNotice};
use fparkan_script::{
    Handler15TargetPayload, Handler2Record, Handler2RecordInput, Handler2RecordKey,
    Handler2RecordScheduler, ScriptDispatchSelector, VarSet, VarSetDefault,
};
use fparkan_world::{identity_metadata, registration_sequence, ObjectHandle, OwnerId, World};
use std::collections::BTreeMap;

const STORAGE_CLASS: u32 = 0x8000_0008;
const RESOURCE_ORE: u32 = 0x0200_0100;
const BUILDING_PROTECT: u32 = 8;
const PLACE_PROTECT: u32 = 11;
const ATTACK_UNIT: u32 = 13;
const OPTIMAL_TRANSPORT: u32 = 4;
const BUILDING_NEEDED: u32 = 3;
const MINE_NEEDED: u32 = 1;
const MAKE_RESEARCH: u32 = 10;

pub(super) struct ScriptFactoryOrder {
    pub owner: OwnerId,
    pub plant: ObjectHandle,
    pub class: u32,
    selection: u32,
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
        if let Some(patrol) = &mut loaded.unit_patrol {
            patrol.clear_capture_passage(approach.unit, approach.target);
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

pub(super) fn tick(engine: &mut crate::Engine, now_ms: u32) -> Vec<MissionNotice> {
    let mut notices = Vec::new();
    let Some(loaded) = &mut engine.loaded else {
        return notices;
    };
    crate::laboratory::tick(
        loaded,
        &engine.world,
        now_ms,
        &mut engine.behavior_rng_state,
    );
    crate::construction::tick(
        loaded,
        &mut engine.world,
        engine.production.as_mut(),
        now_ms,
    );
    loaded.clan_ai_events.groups.retain(|&(clan, _), group| {
        if !group.target.valid(&engine.world) {
            if let Some(patrol) = &mut loaded.unit_patrol {
                for &member in &group.members {
                    group.target.clear_order(patrol, member);
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
                    .is_some_and(|patrol| group.target.has_order(patrol, member))
        });
        !group.members.is_empty()
    });
    let ai = &mut loaded.clan_ai_events;
    ai.transports.retain_mut(|(_, transport)| {
        transport.tick(
            &engine.world,
            &mut loaded.behavior_ore_states,
            &mut loaded.unit_patrol,
            crate::ore_transport::TransportGeometry {
                assets: &loaded.mission_assets,
                drafts: &loaded.object_drafts,
                terrain: &loaded.terrain,
            },
            now_ms,
        )
    });
    ai.waits.retain(|&(clan, key), conditions| {
        let group = ai.groups.get(&(clan, key));
        let lost_building = key.word_0 == BUILDING_PROTECT
            && !crate::mission_events::placed_logic_objects(&loaded.mission, &engine.world, key.argument_0)
                .any(|target| identity_metadata(&engine.world, target)
                    .is_ok_and(|identity| identity.owner_id == Some(OwnerId(clan as u16))));
        // shortcut: recheck the request when a transport arrives until native problem aging is wired.
        let transport_arrived = key.word_0 == OPTIMAL_TRANSPORT && conditions.is_empty()
            && loaded.mission_events.clan_units(&engine.world, OwnerId(clan as u16), false)
                .any(|unit| identity_metadata(&engine.world, unit).ok().and_then(|meta| meta.original_id)
                    .and_then(|id| loaded.object_drafts.get(id.0 as usize))
                    .is_some_and(|draft| draft.unit_dat_header_flags == Some(TRANSPORT_CLASS)));
        let ready = lost_building || transport_arrived || conditions.iter().any(|&condition| {
            condition.met(
                &engine.world,
                OwnerId(clan as u16),
                loaded.unit_patrol.as_ref(),
                group,
                |unit| loaded.construction.active(unit) || loaded.laboratories.active(unit)
                    || ai.transports.iter().any(|(job, task)| *job == (clan, key) && task.unit == unit),
            )
        });
        if ready {
            if let Some(queue) = ai.problems.get_mut(&clan) {
                queue.remove(key);
            }
            if let Some(group) = ai.groups.remove(&(clan, key)) {
                if let Some(patrol) = &mut loaded.unit_patrol {
                    for member in group.members {
                        group.target.clear_order(patrol, member);
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
    for order in loaded.tick_clan_ai_events(
        &engine.world,
        engine.production.as_ref(),
        engine.services.vfs.as_deref(),
        &mut engine.ai_rng_state,
        now_ms,
        &mut notices,
    ) {
        match crate::production::order_script_factory(
            engine,
            order.owner,
            order.plant,
            order.class,
            order.selection,
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
                    "script_factory_order_approx clan={} plant={} class={:#010x} selection={} duration_ms={duration} queue_time_ms={now_ms}",
                    order.owner.0, order.plant.slot, order.class, order.selection,
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
    notices
}

#[derive(Default)]
pub(super) struct ClanAiEvents {
    clocks: crate::clan_clock::ClanClocks,
    stops: BTreeMap<(usize, usize, usize), (MissionEventStop, u64)>,
    research_stubs: BTreeMap<(usize, u8), u64>,
    research_scan_queries: BTreeMap<(usize, u32), u64>,
    pub(super) research_catalogs: BTreeMap<usize, crate::clan_research::ClanResearch>,
    problems: BTreeMap<usize, Handler2RecordScheduler>,
    started: BTreeMap<(usize, u32), u64>,
    factory_orders: BTreeMap<(u16, u32), u64>,
    factory_jobs: Vec<((u16, Handler2RecordKey), ObjectHandle)>,
    factory_rejections: BTreeMap<(u16, u32), u64>,
    strength_queries: BTreeMap<(usize, u16), u64>,
    capture_queries: BTreeMap<usize, u64>,
    attack_queries: BTreeMap<usize, u64>,
    factory_defence_queries: BTreeMap<(usize, u32), u64>,
    building_defence_queries: BTreeMap<(usize, u32), u64>,
    area_strength_queries: BTreeMap<(usize, usize, usize), u64>,
    base_threat_queries: BTreeMap<(usize, u32), u64>,
    base_radius_limits: BTreeMap<usize, f32>,
    capturer_queries: BTreeMap<usize, u64>,
    capture_orders: Vec<(OwnerId, Handler2RecordKey, CaptureApproach)>,
    transports: Vec<(
        (usize, Handler2RecordKey),
        crate::ore_transport::OreTransport,
    )>,
    transport_orders: BTreeMap<(usize, u32), u64>,
    capture_approaches: BTreeMap<(u32, u32), u64>,
    groups: BTreeMap<(usize, Handler2RecordKey), CombatGroup>,
    waits: BTreeMap<(usize, Handler2RecordKey), Vec<Condition>>,
    wait_starts: BTreeMap<(usize, u32), u64>,
    building_sites: crate::construction_site::SiteReservations,
    building_site_queries: BTreeMap<(usize, u32), u64>,
    construction_orders: BTreeMap<(usize, u32, u32), (u64, u64)>,
    patrol_continues: BTreeMap<(usize, u32), u64>,
    group_orders: BTreeMap<(usize, u32), u64>,
    point_group_orders: BTreeMap<(usize, u32, u32), u64>,
    point_patrol_continues: BTreeMap<(usize, u32, u32), u64>,
    free_unit_queries: BTreeMap<(usize, u32, u32), u64>,
    free_mind_queries: BTreeMap<usize, (u32, u32, u32, u64)>,
    group_recruitments: BTreeMap<(usize, u32), u64>,
    class_target_queries: BTreeMap<(usize, u32, u32), u64>,
    size_class_queries: BTreeMap<(usize, u32), u64>,
    class_limit_queries: BTreeMap<(usize, u32, bool), u64>,
    mine_queries: BTreeMap<(usize, u32, u32, u32), u64>,
}

impl LoadedMissionState {
    pub(super) fn tick_clan_ai_events(
        &mut self,
        world: &World,
        production: Option<&crate::production::Production>,
        vfs: Option<&dyn fparkan_vfs::Vfs>,
        rng: &mut u32,
        now_ms: u32,
        notices: &mut Vec<MissionNotice>,
    ) -> Vec<ScriptFactoryOrder> {
        let mut orders = Vec::new();
        let Some(varset) = &self.script_varset else {
            return orders;
        };
        let elapsed_ms = now_ms.wrapping_sub(self.mission_events.start_ms);
        for bundle in self
            .script_bundles
            .iter()
            .filter(|bundle| bundle.behavior_warm_start.is_some())
        {
            let clan = bundle.clan_index;
            let Some(clock) = self.clan_ai_events.clocks.tick(clan, elapsed_ms, rng) else {
                continue;
            };
            let Some((event_index, event)) =
                bundle.package.events.iter().enumerate().find(|(_, event)| {
                    event.name_raw == b"Problems0\0" && !event.instructions.is_empty()
                })
            else {
                continue;
            };
            let Some(state) = self
                .script_varset_states
                .iter_mut()
                .find(|state| state.clan_index == clan)
            else {
                continue;
            };
            let mut problems_event = Some((event_index, event, None::<Handler2Record>));
            // shortcut: task continuations still share the clan cycle rather than per-task scheduling.
            let mut pending = self
                .clan_ai_events
                .problems
                .get(&clan)
                .into_iter()
                .flat_map(|queue| queue.records())
                .rev()
                .filter(|record| {
                    matches!(record.key.word_0, BUILDING_PROTECT | PLACE_PROTECT)
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
                        if problem_id == PLACE_PROTECT {
                            let [x, y] = [problem.key.argument_0, problem.key.argument_1];
                            let count = self
                                .clan_ai_events
                                .point_patrol_continues
                                .entry((clan, x, y))
                                .or_default();
                            *count += 1;
                            if *count == 1 {
                                eprintln!("sim_coverage script_point_patrol_continue_approx=1 clan={clan} x={x} y={y}");
                            }
                        } else {
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
                let Some((event_index, _event, problem)) = next.or_else(|| problems_event.take())
                else {
                    break;
                };
                let mut solved = false;
                let mut transport_factory_busy = false;
                let mut assignment = None;
                let mut solving = false;
                let mut capturer = None;
                let mut group = None;
                let mut conditions = Vec::new();
                let result = varset.declarations.execute_package_event_prefix(
                    &bundle.package,
                    event_index,
                    bundle.formula_table.as_ref(),
                    &mut state.values,
                    |active_event_index, instruction, values| match instruction.dispatch_selector() {
                        ScriptDispatchSelector::Handler(40) => {
                            let selected = crate::clan_mining::query(
                                &self.mission, &self.mission_events, world, &self.object_drafts,
                                &self.control_device_tables, &self.mine_sites, clan,
                            );
                            let result = selected.write(&instruction.references, values)?;
                            if let Some(position) = selected.reservation() {
                                self.clan_ai_events.building_sites.reserve(position, now_ms);
                            }
                            let (target, [x, y]) = selected.report_key();
                            let count = self.clan_ai_events.mine_queries.entry((clan, target, x, y)).or_default();
                            *count += 1;
                            if *count == 1 {
                                eprintln!("sim_coverage script_mine_target_approx=1 clan={clan} target={target} x={x} y={y}");
                            }
                            Ok(Some(VarSetDefault::Dword(result)))
                        }
                        ScriptDispatchSelector::Handler(44) => {
                            let strength = crate::clan_area_strength::query(
                                &self.mission, &self.mission_events, world, &self.object_drafts,
                                &self.control_device_tables, clan, instruction, values,
                            )?;
                            let index = bundle.package.events[active_event_index].instructions.iter().position(|item| std::ptr::eq(item, instruction))
                                .expect("instruction belongs to the executing event");
                            let count = self.clan_ai_events.area_strength_queries.entry((clan, active_event_index, index)).or_default();
                            *count += 1;
                            if *count == 1 {
                                eprintln!("sim_coverage script_area_strength_approx=1 clan={clan} event={active_event_index} instruction={index} strength={strength}");
                            }
                            Ok(Some(VarSetDefault::Dword(strength)))
                        }
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
                        ScriptDispatchSelector::Handler(30) => {
                            let command = varset.declarations
                                .resolve_handler30_with_values(instruction, values)
                                .map_err(|error| error.to_string())?;
                            crate::script_host::apply(
                                self.script_host_mode, &mut self.script_host_status,
                                &mut self.script_outcome, &mut self.mission_events, command, notices,
                            )?;
                            Ok(None)
                        }
                        ScriptDispatchSelector::Handler(71) => {
                            let &[radius, class, estimate] = instruction.references.as_slice() else {
                                return Err("Handler(71) requires radius, class and strength output".into());
                            };
                            let radius = match values.get(radius as usize) {
                                Some(VarSetDefault::Dword(value)) => *value as f32,
                                Some(VarSetDefault::FloatBits(bits)) => f32::from_bits(*bits),
                                None => return Err("Handler(71) radius is unavailable".into()),
                            };
                            let class = values.get(class as usize).and_then(|value| value.as_dword())
                                .ok_or("Handler(71) class must be a DWORD")?;
                            let (target, strength) = crate::clan_building_queries::weakest_enemy_of_class(
                                &self.mission, &self.mission_events, &self.object_drafts,
                                &self.control_device_tables, world, clan, class, radius,
                            ).unwrap_or((u32::MAX, 1e12_f32));
                            let strength = strength as i64 as u32;
                            let output = values.get_mut(estimate as usize)
                                .ok_or("Handler(71) strength output is unavailable")?;
                            *output = match *output {
                                VarSetDefault::Dword(_) => VarSetDefault::Dword(strength),
                                VarSetDefault::FloatBits(_) => VarSetDefault::FloatBits((strength as f32).to_bits()),
                            };
                            let count = self.clan_ai_events.class_target_queries.entry((clan, class, target)).or_default();
                            *count += 1;
                            if *count == 1 {
                                eprintln!("sim_coverage script_class_target_approx=1 clan={clan} class={class} target={target} strength={strength}");
                            }
                            Ok(Some(VarSetDefault::Dword(target)))
                        }
                        ScriptDispatchSelector::Handler(72) => {
                            let id = single_dword(&instruction.references, values)
                                .ok_or("Handler(72) requires a logical ID")?;
                            let size = crate::unit_size_class::query(
                                &self.mission, &self.mission_events, &self.object_drafts, world, id,
                            )?;
                            let count = self.clan_ai_events.size_class_queries.entry((clan, id)).or_default();
                            *count += 1;
                            if *count == 1 {
                                eprintln!("sim_coverage script_size_class_approx=1 clan={clan} unit={id} class={size}");
                            }
                            Ok(Some(VarSetDefault::Dword(size)))
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
                            let word = |index: u32| values.get(index as usize)?.as_dword();
                            let (&mode, rest) = instruction.references.split_first()
                                .ok_or("Handler(25) requires a recruitment mode")?;
                            let mode = word(mode).filter(|mode| matches!(mode, 1..=3))
                                .ok_or("Handler(25) requires a supported recruitment mode")?;
                            let problem_kind = problem.as_ref().expect("recruitment requires a problem").key.word_0;
                            let count = self.clan_ai_events.group_recruitments.entry((clan, mode)).or_default();
                            *count += 1;
                            let first_recruitment = *count == 1;
                            let owner = OwnerId(clan as u16);
                            // shortcut: infer roster occupancy from runtime jobs until native problem-slot ownership is wired.
                            let units = self.mission_events.clan_units(world, owner, mode != 1)
                                .filter(|&unit| !self.mission_events.is_reserved(owner, unit)
                                    && (!self.clan_ai_events.unit_assigned(unit)
                                        || mode == 3 && self.clan_ai_events.combat_assignment(unit)
                                            .is_some_and(|kind| crate::clan_combat_groups::battle_assignment_available(Some(kind), problem_kind)))
                                    && !self.construction.active(unit)
                                    && !self.laboratories.active(unit)
                                    && !production.is_some_and(|queue| queue.is_building_at(unit, owner))
                                    && !orders.iter().any(|order: &ScriptFactoryOrder| order.plant == unit))
                                .filter(|&unit| mode != 2 || self.unit_patrol.as_ref().is_none_or(|patrol| !patrol.is_shut_down(unit)));
                            if mode != 2 {
                                if !rest.is_empty() {
                                    return Err("Handler(25) all-unit recruitment takes one operand".into());
                                }
                                group = crate::clan_combat_groups::recruit_all(units);
                                if mode == 3 {
                                    if let Some(recruited) = &group {
                                        for previous in self.clan_ai_events.groups.values_mut() {
                                            previous.members.retain(|unit| !recruited.members.contains(unit));
                                        }
                                        self.clan_ai_events.capture_orders.retain(|(_, _, order)| !recruited.members.contains(&order.unit));
                                    }
                                }
                                if first_recruitment {
                                    eprintln!("sim_coverage script_group_recruitment_approx=1 clan={clan} mode={mode} units={:?}", group.as_ref().map(|group| group.members.iter().map(|unit| unit.slot).collect::<Vec<_>>()).unwrap_or_default());
                                }
                                return Ok(Some(VarSetDefault::Dword(if group.is_some() { 0 } else { u32::MAX })));
                            }
                            let &[hits, target_mode, ref target @ ..] = rest else {
                                return Err("Handler(25) TAKE_BY_HITS requires strength and a target".into());
                            };
                            let requested = word(hits).ok_or("Handler(25) strength must be a DWORD")?;
                            let target = match (word(target_mode), target) {
                                (Some(0x201), &[id]) => Handler15TargetPayload::ByLogicId(word(id).ok_or("Handler(25) logical ID must be a DWORD")?),
                                (Some(0x202), &[x, y]) => Handler15TargetPayload::ByPlace {
                                    first: word(x).ok_or("Handler(25) X must be a DWORD")?,
                                    second: word(y).ok_or("Handler(25) Y must be a DWORD")?,
                                },
                                _ => return Err("Handler(25) requires a logical ID or XY point".into()),
                            };
                            // shortcut: use free warbots until lower-priority orders can be preempted.
                            let (recruited, strength) = crate::clan_combat_groups::recruit(
                                world, units, &self.control_device_tables, &self.mission, target, requested,
                            );
                            if strength < requested as f32 || strength == 0.0 {
                                values[hits as usize] = VarSetDefault::Dword(strength as u32);
                            }
                            group = recruited;
                            if first_recruitment {
                                eprintln!("sim_coverage script_group_recruitment_approx=1 clan={clan} mode={mode} units={:?}", group.as_ref().map(|group| group.members.iter().map(|unit| unit.slot).collect::<Vec<_>>()).unwrap_or_default());
                            }
                            // shortcut: group IDs stay local until groups are script-visible across events.
                            Ok(Some(VarSetDefault::Dword(if group.is_some() { 0 } else { u32::MAX })))
                        }
                        ScriptDispatchSelector::Handler(28) if problem.is_some() => {
                            let order = varset.declarations.resolve_handler15_with_values(instruction, values)
                                .map_err(|error| error.to_string())?;
                            let mut group = group.take().ok_or("Handler(28) requires an active combat group")?;
                            let patrol = self.unit_patrol.get_or_insert_with(crate::unit_patrol::UnitPatrol::scripted);
                            group.dispatch(patrol, world, &self.mission, &self.object_drafts, order)?;
                            let key = problem.as_ref().expect("group order requires a problem").key;
                            let (name, count) = match group.target {
                                GroupTarget::Unassigned => return Err("Handler(28) group has no assigned order".into()),
                                GroupTarget::Object(target, _) => ("script_group_order_approx", self.clan_ai_events.group_orders.entry((clan, target.slot)).or_default()),
                                GroupTarget::Point([x, y]) => ("script_point_group_order_approx", self.clan_ai_events.point_group_orders.entry((clan, x, y)).or_default()),
                            };
                            *count += 1;
                            eprintln!("sim_coverage {name}=1 clan={clan} target={:?} order={} units={:?}", group.target, order.word_1, group.members.iter().map(|unit| unit.slot).collect::<Vec<_>>());
                            let groups = &mut self.clan_ai_events.groups;
                            if let Some(assigned) = groups.get_mut(&(clan, key))
                                .filter(|assigned| matches!(key.word_0, BUILDING_PROTECT | PLACE_PROTECT)
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
                            let limit = crate::clan_build_limits::maximum_variable(class).map(|name| {
                                varset.declarations.declarations.iter()
                                    .position(|entry| entry.name == name)
                                    .and_then(|index| values.get(index)?.as_dword())
                                    .ok_or("Handler(11) class limit is unavailable")
                            }).transpose()?;
                            let count = self.mission_events.count_clan_class(
                                world,
                                OwnerId(clan as u16),
                                class,
                            );
                            // shortcut: bind pending counters to active runtime construction until native task accounting is wired.
                            let pending = self.construction.pending_count(OwnerId(clan as u16), class);
                            let blocked = crate::clan_build_limits::at_limit(class, count, limit, pending);
                            let queries = self.clan_ai_events.class_limit_queries.entry((clan, class, blocked)).or_default();
                            *queries += 1;
                            if *queries == 1 {
                                eprintln!("sim_coverage script_class_limit_approx=1 clan={clan} class={class:#010x} blocked={blocked} owned={count} pending={pending}");
                            }
                            Ok(Some(VarSetDefault::Dword(u32::from(blocked))))
                        }
                        ScriptDispatchSelector::Handler(13) => {
                            let (mode, class) = instruction.dword_pair(values)
                                .ok_or("Handler(13) requires a unit mode and class")?;
                            if mode != 0 {
                                return Err("Handler(13) only supports UNIT_FREE_UNIT".into());
                            }
                            let owner = OwnerId(clan as u16);
                            // shortcut: infer roster occupancy from runtime jobs until native problem-slot ownership is wired.
                            let id = self.mission_events.first_available_placed_unit(
                                &self.mission,
                                world,
                                clan,
                                class,
                                |unit| !self.clan_ai_events.unit_assigned(unit)
                                    && !self.construction.active(unit)
                                    && !self.laboratories.active(unit)
                                    && !production.is_some_and(|queue| queue.is_building_at(unit, owner))
                                    && !orders.iter().any(|order: &ScriptFactoryOrder| order.plant == unit),
                            ).unwrap_or(u32::MAX);
                            let count = self.clan_ai_events.free_unit_queries.entry((clan, class, id)).or_default();
                            *count += 1;
                            if *count == 1 {
                                eprintln!("sim_coverage script_free_unit_approx=1 clan={clan} class={class} unit={id}");
                            }
                            Ok(Some(VarSetDefault::Dword(id)))
                        }
                        ScriptDispatchSelector::Handler(15) => {
                            let order = varset
                                .declarations
                                .resolve_handler15_with_values(instruction, values)
                                .map_err(|error| error.to_string())?;
                            if crate::unit_patrol::supports_order(&order) {
                                let patrol = self.unit_patrol.get_or_insert_with(crate::unit_patrol::UnitPatrol::scripted);
                                return Ok(patrol.script_order(world, &self.mission, &self.object_drafts, order)
                                    .map(VarSetDefault::Dword));
                            }
                            if let (7, 3, Handler15TargetPayload::ByPlace { first, second }) =
                                (order.word_1, order.word_2, order.target)
                            {
                                let supported = problem.as_ref().is_some_and(|p| match p.key.word_0 {
                                    MINE_NEEDED => order.word_3 == crate::mining::MINE_CLASS,
                                    BUILDING_NEEDED => crate::construction::supports_class(order.word_3),
                                    _ => false,
                                });
                                if !supported
                                {
                                    return Err("ORDER_ROBOT_BUILD requires a supported building scheme".into());
                                }
                                let owner = OwnerId(clan as u16);
                                let builder = self.mission_events.logic_objects(&self.mission, world, order.word_0)
                                    .find(|&handle| identity_metadata(world, handle).is_ok_and(|identity| identity.owner_id == Some(owner)));
                                let result = crate::construction::order(
                                    &mut self.construction, &mut self.unit_patrol, world, &self.terrain, &self.mission,
                                    &self.object_drafts, &self.mine_sites, &self.behavior_ore_states, &self.free_research,
                                    vfs, builder, owner, order.word_3, [first, second],
                                );
                                let rates = self.clan_ai_events.construction_orders.entry((clan, order.word_0, order.word_3)).or_default();
                                match result {
                                    Ok(()) => {
                                        rates.0 += 1;
                                        eprintln!("sim_coverage script_building_order_approx=1 clan={clan} builder={} class={:#010x} site=[{first}, {second}]", order.word_0, order.word_3);
                                    }
                                    Err(ref reason) => {
                                        rates.1 += 1;
                                        if rates.1 == 1 {
                                            eprintln!("sim_coverage script_building_rejected=1 clan={clan} builder={} class={:#010x} reason={reason}", order.word_0, order.word_3);
                                        }
                                    }
                                }
                                return Ok(Some(VarSetDefault::Dword(u32::from(result.is_ok()))));
                            }
                            if let (6, 3, Handler15TargetPayload::NotDefined(_), Some(problem)) =
                                (order.word_1, order.word_2, order.target, problem.as_ref())
                            {
                                let owner = OwnerId(clan as u16);
                                let Some(unit) = assignment.filter(|&unit| self.mission_events
                                    .logic_objects(&self.mission, world, order.word_0)
                                    .any(|handle| handle == unit)) else {
                                    return Err("Handler(15) transport requires the assigned unit".into());
                                };
                                if problem.key.word_0 != OPTIMAL_TRANSPORT
                                    || !self.behavior_ore_states.iter().any(|ore| ore.handle == unit && ore.class_flags == TRANSPORT_CLASS)
                                {
                                    return Err("Handler(15) transport requires an ore transport".into());
                                }
                                self.clan_ai_events.transports.push(((clan, problem.key), crate::ore_transport::OreTransport::new(unit, owner, now_ms)));
                                *self.clan_ai_events.transport_orders.entry((clan, unit.slot)).or_default() += 1;
                                eprintln!("sim_coverage script_ore_transport_approx=1 clan={clan} unit={} id={} queue_time_ms={now_ms}", unit.slot, order.word_0);
                                return Ok(Some(VarSetDefault::Dword(1)));
                            }
                            if let (0x11, 3, Handler15TargetPayload::ByLogicId(target_id)) =
                                (order.word_1, order.word_2, order.target)
                            {
                                let unit = capturer.filter(|unit: &crate::clan_targeting::Capturer| unit.id == order.word_0)
                                    .ok_or("Handler(15) capture requires a selected capturer")?;
                                let problem = problem.as_ref()
                                    .ok_or("Handler(15) capture requires a current problem")?;
                                let approach = crate::clan_targeting::send_capturer(
                                    &mut self.unit_patrol, world, &self.object_drafts, &self.mission_assets, &self.terrain, unit, target_id,
                                );
                                if let Some(approach) = approach {
                                    self.clan_ai_events.capture_orders.push((OwnerId(clan as u16), problem.key, approach));
                                    if approach.fallback {
                                        *self.clan_ai_events.capture_approaches.entry((approach.unit.slot, approach.target.slot)).or_default() += 1;
                                    }
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
                            let Handler15TargetPayload::ByName(selection) = order.target else {
                                return Err("Handler(15) factory order requires design selection".into());
                            };
                            if order.word_1 != 0x0c
                                || order.word_2 != 1
                                || !matches!(order.word_3, crate::boarding::WARBOT_CLASS | TRANSPORT_CLASS | crate::construction::BUILDER_CLASS)
                            {
                                return Err(
                                    "Handler(15) factory order class or insertion mode is unsupported"
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
                            if production.is_some_and(|queue| queue.is_building_at(plant, owner))
                                || orders.iter().any(|order: &ScriptFactoryOrder| order.plant == plant)
                            {
                                transport_factory_busy = order.word_3 == TRANSPORT_CLASS;
                                return Ok(Some(VarSetDefault::Dword(0)));
                            }
                            orders.push(ScriptFactoryOrder {
                                owner,
                                plant,
                                class: order.word_3,
                                selection,
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
                            assignment = self.mission_events.logic_objects(
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
                                && problem.as_ref().is_some_and(|record| matches!(record.key.word_0, BUILDING_PROTECT | PLACE_PROTECT)) =>
                        {
                            let key = problem.as_ref().expect("protection problem").key;
                            if handler == 7 {
                                if let Some(assigned) = self.clan_ai_events.groups.remove(&(clan, key)) {
                                    if let Some(patrol) = &mut self.unit_patrol {
                                        for unit in assigned.members {
                                            assigned.target.clear_order(patrol, unit);
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
                                        .is_some_and(|record| matches!(record.key.word_0, OPTIMAL_TRANSPORT | MINE_NEEDED | MAKE_RESEARCH | 6 | BUILDING_PROTECT | PLACE_PROTECT))
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
                                (2, 0) if matches!(key.word_0, BUILDING_NEEDED | MINE_NEEDED | MAKE_RESEARCH) || (key.word_0 == OPTIMAL_TRANSPORT && solving) => {
                                    let unit = assignment.filter(|&unit| self.mission_events
                                        .logic_objects(&self.mission, world, id).any(|handle| handle == unit))
                                        .ok_or("Handler(27) task wait requires the assigned unit")?;
                                    Some(Condition::TaskInactive(unit))
                                }
                                (1 | 3, 0) if matches!(key.word_0, 7 | 9) || (action == 1 && matches!(key.word_0, BUILDING_PROTECT | ATTACK_UNIT | MAKE_RESEARCH)) => {
                                    let target = self.mission_events.logic_objects(
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
                            let id = self.mission_events.first_available_placed_unit(
                                &self.mission, world, clan, class, |_| true,
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
                            let lab = self.mission_events.logic_objects(
                                &self.mission,
                                world,
                                id,
                            )
                            .find(|handle| Some(*handle) == assignment);
                            let Some(lab) = lab else { return Ok(Some(VarSetDefault::Dword(u32::MAX))); };
                            let result = crate::laboratory::order(&mut self.laboratories, &mut self.clan_ai_events.research_catalogs,
                                &self.object_drafts, &self.free_research, world, vfs, clan, lab, now_ms);
                            if let Err(reason) = &result { self.laboratories.rejected(clan, lab, reason); }
                            Ok(Some(VarSetDefault::Dword(u32::from(result.is_ok()))))
                        }
                        ScriptDispatchSelector::Handler(43) if crate::clan_script_init::research_catalog(instruction) => {
                            let vfs = vfs.ok_or("research VFS is unavailable")?;
                            let catalog = self.clan_ai_events.research_catalogs.get_mut(&clan).ok_or("research catalog is unavailable")?;
                            catalog.refresh(vfs)?;
                            Ok(None)
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
                        ScriptDispatchSelector::Handler(18)
                            if problem.as_ref().is_some_and(|record| record.key.word_0 == BUILDING_NEEDED) =>
                        {
                            let &[x, y] = instruction.references.as_slice() else {
                                return Err("Handler(18) requires two DWORD outputs".into());
                            };
                            if x == y || [x, y].into_iter().any(|index| !matches!(values.get(index as usize), Some(VarSetDefault::Dword(_)))) {
                                return Err("Handler(18) requires two distinct DWORD outputs".into());
                            }
                            let base = self.mission.clans[clan].anchor.map(f32::round_ties_even);
                            let position = self.clan_ai_events.building_sites.find(
                                &self.terrain, world, &self.object_drafts, &self.mine_sites, base, now_ms,
                            );
                            let [px, py] = position.unwrap_or([0, 0]);
                            values[x as usize] = VarSetDefault::Dword(px);
                            values[y as usize] = VarSetDefault::Dword(py);
                            let class = problem.as_ref().unwrap().key.argument_0;
                            let count = self.clan_ai_events.building_site_queries.entry((clan, class)).or_default();
                            *count += 1;
                            if *count == 1 {
                                eprintln!("sim_coverage script_building_site_approx=1 clan={clan} class={class:#010x} site={position:?}");
                            }
                            Ok(Some(VarSetDefault::Dword(1)))
                        }
                        ScriptDispatchSelector::Handler(39) => {
                            let id = single_dword(&instruction.references, values)
                                .ok_or("Handler(39) requires one DWORD logical ID")?;
                            let exists = self.mission_events.has_live_logic_object(&self.mission, world, id);
                            Ok(Some(VarSetDefault::Dword(u32::from(exists))))
                        }
                        ScriptDispatchSelector::Handler(49)
                            if instruction.references.is_empty() =>
                        {
                            let capacity = self
                                .ai_startup_states
                                .iter()
                                .find(|state| state.clan_index == clan)
                                .map_or(0, |state| state.record_slots.len());
                            let owner = OwnerId(clan as u16);
                            let reserved = production.map_or(0, |production| production.reserved_minds(world, owner))
                                + orders.iter().filter(|order| order.owner == owner).count();
                            let free = crate::clan_minds::free_slots(
                                capacity, self.mission_events.clan_classes(world, owner), reserved,
                            );
                            let query = self.clan_ai_events.free_mind_queries.entry(clan).or_default();
                            *query = (capacity as u32, free, reserved as u32, query.3 + 1);
                            if query.3 == 1 {
                                eprintln!("sim_coverage script_free_minds_approx=1 clan={clan} capacity={capacity} free={free} reserved={reserved}");
                            }
                            Ok(Some(VarSetDefault::Dword(free)))
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
                        ScriptDispatchSelector::Handler(handler @ (37 | 47 | 67)) => {
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
                                (37 | 67, &[output]) => {
                                    let units = self.mission_events.clan_units(world, OwnerId(clan as u16), false);
                                    let selection = if handler == 37 {
                                        crate::clan_targeting::building_defence(world, &self.object_drafts, &self.control_device_tables, units)
                                    } else {
                                        crate::clan_targeting::factory_defence(world, &self.object_drafts, &self.control_device_tables, units)
                                    };
                                    (output, selection)
                                }
                                _ => return Err("defence query requires a strength output".into()),
                            };
                            let cell = values.get_mut(output as usize)
                                .ok_or("defence query strength output is unavailable")?;
                            let (id, power) = selection.unwrap_or((u32::MAX, if handler == 37 { 1e12_f32 } else { 0.0 }));
                            let power = crate::clan_targeting::encode_strength(power);
                            *cell = match *cell {
                                VarSetDefault::Dword(_) => VarSetDefault::Dword(power as u32),
                                VarSetDefault::FloatBits(_) => VarSetDefault::FloatBits(power.to_bits()),
                            };
                            let (queries, name) = match handler {
                                37 => (&mut self.clan_ai_events.building_defence_queries, "script_building_defence_approx"),
                                47 => (&mut self.clan_ai_events.base_threat_queries, "script_base_threat_approx"),
                                _ => (&mut self.clan_ai_events.factory_defence_queries, "script_factory_defence_approx"),
                            };
                            let count = queries.entry((clan, id)).or_default();
                            *count += 1;
                            if *count == 1 {
                                eprintln!("sim_coverage {name}=1 clan={clan} target={id} strength={power}");
                            }
                            Ok(Some(VarSetDefault::Dword(id)))
                        }
                        ScriptDispatchSelector::Handler(56)
                            if instruction.references.is_empty() =>
                        {
                            let result = crate::clan_research::scan_status(
                                self.clan_ai_events.research_catalogs.get(&clan).map(|catalog| &catalog.candidates),
                            );
                            let count = self.clan_ai_events.research_scan_queries.entry((clan, result)).or_default();
                            *count += 1;
                            if *count == 1 {
                                eprintln!("sim_coverage script_research_scan_status=1 clan={clan} result={result}");
                            }
                            Ok(Some(VarSetDefault::Dword(result)))
                        }
                        ScriptDispatchSelector::Handler(handler @ 65)
                            if instruction.references.is_empty() =>
                        {
                            // shortcut: large-warbot eligibility needs the native candidate power property.
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
                if let Err((stopped_event, instruction_index, reason)) = result {
                    let (stop, count) = self
                        .clan_ai_events
                        .stops
                        .entry((clan, stopped_event, instruction_index))
                        .or_insert_with(|| {
                            (
                                MissionEventStop {
                                    clan_index: clan,
                                    script_path: bundle.script_path.clone(),
                                    event_index: stopped_event,
                                    instruction_index,
                                    reason,
                                },
                                0,
                            )
                        });
                    *count += 1;
                    if *count == 1 {
                        eprintln!("sim_coverage script_clan_ai_unsupported=1 clan={clan} script={} event={stopped_event} instruction={instruction_index} reason={}", stop.script_path, stop.reason);
                    }
                }
                if let Some(problem) = problem {
                    // shortcut: keep transport demand queued until the factory can accept it.
                    if solved && !transport_factory_busy {
                        self.clan_ai_events
                            .problems
                            .entry(clan)
                            .or_default()
                            .remove(problem.key);
                    } else if solving
                        && (!conditions.is_empty()
                            || matches!(
                                problem.key.word_0,
                                OPTIMAL_TRANSPORT | BUILDING_PROTECT | PLACE_PROTECT
                            ))
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
    pub(super) fn research_scan_queries(&self) -> impl Iterator<Item = (usize, u32, u64)> + '_ {
        self.research_scan_queries
            .iter()
            .map(|(&(clan, result), &count)| (clan, result, count))
    }

    pub(super) fn free_mind_queries(
        &self,
    ) -> impl Iterator<Item = (usize, u32, u32, u32, u64)> + '_ {
        self.free_mind_queries
            .iter()
            .map(|(&clan, &(capacity, free, reserved, count))| {
                (clan, capacity, free, reserved, count)
            })
    }

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

    pub(super) fn building_defence_queries(&self) -> impl Iterator<Item = (usize, u32, u64)> + '_ {
        self.building_defence_queries
            .iter()
            .map(|(&(clan, target), &count)| (clan, target, count))
    }

    pub(super) fn area_strength_queries(
        &self,
    ) -> impl Iterator<Item = (usize, usize, usize, u64)> + '_ {
        self.area_strength_queries
            .iter()
            .map(|(&(clan, event, instruction), &count)| (clan, event, instruction, count))
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
        self.factory_jobs.iter().any(|&(_, plant)| plant == unit)
            || self
                .transports
                .iter()
                .any(|(_, transport)| transport.unit == unit)
            || self
                .capture_orders
                .iter()
                .any(|&(_, _, order)| order.unit == unit)
            || self
                .groups
                .values()
                .any(|group| group.members.contains(&unit))
    }

    fn combat_assignment(&self, unit: ObjectHandle) -> Option<u32> {
        self.groups
            .iter()
            .find_map(|((_, key), group)| group.members.contains(&unit).then_some(key.word_0))
            .or_else(|| {
                self.capture_orders
                    .iter()
                    .find_map(|(_, key, order)| (order.unit == unit).then_some(key.word_0))
            })
    }

    pub(super) fn group_orders(&self) -> impl Iterator<Item = (usize, u32, u64)> + '_ {
        self.group_orders
            .iter()
            .map(|(&(clan, target), &count)| (clan, target, count))
    }

    pub(super) fn free_unit_queries(&self) -> impl Iterator<Item = (usize, u32, u32, u64)> + '_ {
        self.free_unit_queries
            .iter()
            .map(|(&(clan, class, unit), &count)| (clan, class, unit, count))
    }

    pub(super) fn group_recruitments(&self) -> impl Iterator<Item = (usize, u32, u64)> + '_ {
        self.group_recruitments
            .iter()
            .map(|(&(clan, mode), &count)| (clan, mode, count))
    }

    pub(super) fn class_target_queries(&self) -> impl Iterator<Item = (usize, u32, u32, u64)> + '_ {
        self.class_target_queries
            .iter()
            .map(|(&(clan, class, target), &count)| (clan, class, target, count))
    }

    pub(super) fn size_class_queries(&self) -> impl Iterator<Item = (usize, u32, u64)> + '_ {
        self.size_class_queries
            .iter()
            .map(|(&(clan, id), &count)| (clan, id, count))
    }

    pub(super) fn point_group_orders(&self) -> impl Iterator<Item = (usize, u32, u32, u64)> + '_ {
        self.point_group_orders
            .iter()
            .map(|(&(clan, x, y), &count)| (clan, x, y, count))
    }

    pub(super) fn point_patrol_continues(
        &self,
    ) -> impl Iterator<Item = (usize, u32, u32, u64)> + '_ {
        self.point_patrol_continues
            .iter()
            .map(|(&(clan, x, y), &count)| (clan, x, y, count))
    }

    pub(super) fn transport_orders(&self) -> impl Iterator<Item = (usize, u32, u64)> + '_ {
        self.transport_orders
            .iter()
            .map(|(&(clan, unit), &count)| (clan, unit, count))
    }

    pub(super) fn building_site_queries(&self) -> impl Iterator<Item = (usize, u32, u64)> + '_ {
        self.building_site_queries
            .iter()
            .map(|(&(clan, class), &count)| (clan, class, count))
    }

    pub(super) fn class_limit_queries(&self) -> impl Iterator<Item = (usize, u32, bool, u64)> + '_ {
        self.class_limit_queries
            .iter()
            .map(|(&(clan, class, blocked), &count)| (clan, class, blocked, count))
    }

    pub(super) fn mine_queries(&self) -> impl Iterator<Item = (usize, u32, u32, u32, u64)> + '_ {
        self.mine_queries
            .iter()
            .map(|(&(clan, target, x, y), &count)| (clan, target, x, y, count))
    }

    pub(super) fn construction_orders(
        &self,
    ) -> impl Iterator<Item = (usize, u32, u32, u64, u64)> + '_ {
        self.construction_orders
            .iter()
            .map(|(&(clan, unit, class), &(accepted, rejected))| {
                (clan, unit, class, accepted, rejected)
            })
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

    pub(super) fn advance_for_registration(&mut self, clan: usize) -> u64 {
        self.clocks.advance_for_registration(clan)
    }

    pub(super) fn clock_seconds(&self, clan: usize) -> u32 {
        self.clocks.seconds(clan)
    }

    pub(super) fn wall_time_ms(&self, queue_ms: u32) -> u32 {
        self.clocks.wall_time_ms(queue_ms)
    }

    pub(super) fn add_paused_time(&mut self, paused_ms: u32) {
        self.clocks.add_paused_time(paused_ms);
    }

    pub(super) fn clock_ticks(&self) -> impl Iterator<Item = (usize, u64, u64)> + '_ {
        self.clocks.ticks()
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
    fn licensed_free_group_waits_empty_then_attacks_after_a_debug_roster_transfer() {
        use crate::{EngineServices, MissionMode, MissionRequest, MissionStartupInput};
        use fparkan_vfs::DirectoryVfs;
        use std::sync::Arc;

        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed corpus root");
        let mut engine =
            crate::create(EngineServices::new(Arc::new(DirectoryVfs::new(root)))).unwrap();
        crate::load_gameplay_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/CAMPAIGN/CAMPAIGN.02/Mission.03/data.tma".into(),
                mode: MissionMode::SingleMission,
            },
            MissionStartupInput {
                queue_time_ms: 0,
                research_creation_gate: false,
            },
            None,
        )
        .unwrap();
        for now in (0..=7_000).step_by(100) {
            crate::frame(&mut engine, now).unwrap();
        }
        let loaded = engine.loaded.as_ref().unwrap();
        assert_eq!(
            loaded
                .mission_events
                .clan_units(&engine.world, OwnerId(5), false)
                .count(),
            0
        );
        assert!(loaded
            .clan_ai_events
            .stops()
            .all(|(stop, _)| stop.clan_index != 5));
        assert!(!loaded
            .clan_ai_events
            .groups
            .keys()
            .any(|&(clan, _)| clan == 5));
        let transferred = loaded
            .mission_events
            .clan_units(&engine.world, OwnerId(1), true)
            .next()
            .expect("stock enemy has a warbot for the debug transfer");
        // Debug setup exercises the attack path; this is not a stock mission playthrough.
        fparkan_world::set_owner(&mut engine.world, transferred, Some(OwnerId(5))).unwrap();
        for now in (7_100..=14_000).step_by(100) {
            crate::frame(&mut engine, now).unwrap();
        }
        let loaded = engine.loaded.as_ref().unwrap();
        let (&key, group) = loaded
            .clan_ai_events
            .groups
            .iter()
            .find(|(&(clan, key), _)| clan == 5 && key.word_0 == ATTACK_UNIT)
            .expect("script selects all free units and dispatches its attack");
        let GroupTarget::Object(target, _) = group.target else {
            panic!("attack requires an object")
        };
        assert_eq!(group.members, [transferred]);
        let patrol = loaded.unit_patrol.as_ref().unwrap();
        assert!(group.members.iter().all(|&unit| patrol
            .attack_target(unit)
            .is_some_and(|(object, _)| object == target)));
        assert!(loaded.clan_ai_events.waits.contains_key(&key));
        assert!(loaded.clan_ai_events.stops().all(|(stop, _)| {
            stop.clan_index != 5
                || !["Handler(25)", "Handler(28)", "Handler(27)"]
                    .iter()
                    .any(|handler| stop.reason.starts_with(handler))
        }));
        crate::frame(&mut engine, 14_100).unwrap();
        assert!(engine
            .loaded
            .as_ref()
            .unwrap()
            .clan_ai_events
            .groups
            .contains_key(&key));
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_free_factory_query_dispatches_robot_production_from_stock_start() {
        use crate::{EngineServices, MissionMode, MissionRequest, MissionStartupInput};
        use fparkan_vfs::DirectoryVfs;
        use std::sync::Arc;

        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed corpus root");
        let mut engine =
            crate::create(EngineServices::new(Arc::new(DirectoryVfs::new(root)))).unwrap();
        crate::load_gameplay_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/CAMPAIGN/CAMPAIGN.01/Mission.03/data.tma".into(),
                mode: MissionMode::SingleMission,
            },
            MissionStartupInput {
                queue_time_ms: 0,
                research_creation_gate: false,
            },
            None,
        )
        .unwrap();

        for now in (0..=90_000).step_by(100) {
            crate::frame(&mut engine, now).unwrap();
        }
        let loaded = engine.loaded.as_ref().unwrap();
        assert!(loaded
            .clan_ai_events
            .free_unit_queries()
            .any(|(clan, class, id, count)| {
                clan == 1 && class == 0x8000_0010 && id != u32::MAX && count > 0
            }));
        assert!(loaded
            .clan_ai_events
            .factory_orders
            .iter()
            .any(|(&(owner, _), &count)| owner == 1 && count > 0));
        assert!(loaded
            .clan_ai_events
            .stops()
            .all(|(stop, _)| !stop.reason.starts_with("Handler(13)")));
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_point_protection_patrols_and_resumes_from_stock_start() {
        use crate::{EngineServices, MissionMode, MissionRequest, MissionStartupInput};
        use fparkan_vfs::DirectoryVfs;
        use std::sync::Arc;

        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed corpus root");
        let mut engine =
            crate::create(EngineServices::new(Arc::new(DirectoryVfs::new(root)))).unwrap();
        crate::load_gameplay_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/CAMPAIGN/CAMPAIGN.02/Mission.01/data.tma".into(),
                mode: MissionMode::SingleMission,
            },
            MissionStartupInput {
                queue_time_ms: 0,
                research_creation_gate: false,
            },
            None,
        )
        .unwrap();

        for now in (0..=150_000).step_by(100) {
            crate::frame(&mut engine, now).unwrap();
        }
        let loaded = engine.loaded.as_ref().unwrap();
        assert!(loaded
            .clan_ai_events
            .point_group_orders()
            .any(|(_, _, _, count)| count > 0));
        assert!(loaded
            .clan_ai_events
            .point_patrol_continues()
            .any(|(_, _, _, count)| count > 1));
        let patrol = loaded.unit_patrol.as_ref().unwrap();
        let group = loaded
            .clan_ai_events
            .groups
            .iter()
            .find_map(|(&(_, key), group)| {
                (key.word_0 == PLACE_PROTECT && matches!(group.target, GroupTarget::Point(_)))
                    .then_some(group)
            })
            .expect("point protection keeps its group between script invocations");
        assert!(!group.members.is_empty());
        assert!(group
            .members
            .iter()
            .all(|&unit| group.target.has_order(patrol, unit)));
        assert!(loaded.clan_ai_events.stops().all(|(stop, _)| {
            !stop.reason.starts_with("Handler(25)") && !stop.reason.starts_with("Handler(28)")
        }));
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_weak_building_gets_scripted_guards_from_stock_start() {
        use crate::{EngineServices, MissionMode, MissionRequest, MissionStartupInput};
        use fparkan_vfs::DirectoryVfs;
        use std::sync::Arc;

        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed corpus root");
        let mut engine =
            crate::create(EngineServices::new(Arc::new(DirectoryVfs::new(root)))).unwrap();
        crate::load_gameplay_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/CAMPAIGN/CAMPAIGN.01/Mission.03/data.tma".into(),
                mode: MissionMode::SingleMission,
            },
            MissionStartupInput {
                queue_time_ms: 0,
                research_creation_gate: false,
            },
            None,
        )
        .unwrap();

        for now in (0..=7_000).step_by(100) {
            crate::frame(&mut engine, now).unwrap();
        }
        let loaded = engine.loaded.as_ref().unwrap();
        assert!(loaded
            .clan_ai_events
            .stops()
            .all(|(stop, _)| stop.reason != "Handler(37)"));
        let target = crate::mission_events::placed_logic_objects(
            &loaded.mission,
            &engine.world,
            0x8000_0001,
        )
        .next()
        .unwrap();
        let group = loaded
            .clan_ai_events
            .groups
            .iter()
            .find_map(|(&(clan, key), group)| {
                (clan == 1 && key.word_0 == BUILDING_PROTECT && key.argument_0 == 0x8000_0001)
                    .then_some(group)
            })
            .expect("weak building receives a guard group");
        assert_eq!(group.target, GroupTarget::Object(target, Some(OwnerId(1))));
        assert!(!group.members.is_empty());
        let patrol = loaded.unit_patrol.as_ref().unwrap();
        assert!(group
            .members
            .iter()
            .all(|&unit| patrol.has_object_order(unit, target)));
        assert_eq!(
            loaded
                .clan_ai_events
                .building_defence_queries()
                .collect::<Vec<_>>(),
            vec![(1, 0x8000_0001, 1)]
        );
    }

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
        assert_eq!(group.target, GroupTarget::Object(factory, Some(OwnerId(1))));
        assert!(!group.members.is_empty());
        let patrol = loaded.unit_patrol.as_ref().unwrap();
        assert!(group
            .members
            .iter()
            .all(|&unit| patrol.has_object_order(unit, factory)));
        assert!(loaded.clan_ai_events.waits.contains_key(key));
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
