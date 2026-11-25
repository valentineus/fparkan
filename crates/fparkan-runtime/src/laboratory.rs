use crate::{BehaviorOreState, LoadedMissionState, MissionControlDeviceTableState};
use fparkan_assets::{decode_behavior_work_rates, research_project_rows};
use fparkan_vfs::Vfs;
use fparkan_world::{identity_metadata, ObjectHandle, OwnerId, World};
use std::collections::BTreeMap;

const LAB_CLASS: u32 = 0x8000_0400;

/// Executed laboratory orders and technology state changes.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct LaboratoryReport {
    /// Accepted/rejected orders by clan and laboratory.
    pub orders: Vec<(usize, u32, u64, u64)>,
    /// Completed technologies by clan, laboratory and TRF row.
    pub completed: Vec<(usize, u32, usize, u64)>,
    /// Newly available technologies by clan and TRF row.
    pub available: Vec<(usize, usize, u64)>,
    /// Orders stopped by unavailable runtime state, by clan and laboratory.
    pub stopped: Vec<(usize, u32, u64)>,
    /// Due task updates; the idle owner phase is still approximate.
    pub clock_ticks: Vec<(usize, u32, u64)>,
}

#[derive(Default)]
pub(super) struct Laboratories {
    tasks: Vec<Order>,
    rates: Option<[f32; 2]>,
    progress: BTreeMap<(usize, usize), f32>,
    orders: BTreeMap<(usize, u32), (u64, u64)>,
    completed: BTreeMap<(usize, u32, usize), u64>,
    available: BTreeMap<(usize, usize), u64>,
    stopped: BTreeMap<(usize, u32), u64>,
    clock_ticks: BTreeMap<(usize, u32), u64>,
}

struct Order {
    clan: usize,
    lab: ObjectHandle,
    slot: usize,
    rows: Vec<usize>,
    current: Option<(usize, Work)>,
    duration: f32,
    last_ms: u32,
    clock: crate::behavior_clock::TaskClock,
}

#[derive(Default)]
struct Work {
    ore_cost: f32,
    power_cost: f32,
    duration: f32,
    ore: f32,
    power: f32,
    time: f32,
}

impl Work {
    fn fraction(&self) -> f32 {
        let ratio = |value, target| if target > 0.0 { value / target } else { 1.0 };
        ratio(self.ore, self.ore_cost)
            .min(ratio(self.power, self.power_cost))
            .min(ratio(self.time, self.duration))
    }

    fn advance(
        &mut self,
        seconds: f32,
        rate: f32,
        uses: [f32; 2],
        buffer: &mut BehaviorOreState,
        progress: f32,
    ) -> f32 {
        let before = self.fraction();
        self.ore += (rate * uses[1] * seconds).min(f32::from_bits(buffer.current_ore_bits));
        let remaining = self.ore_cost - self.ore;
        buffer.current_ore_bits = 0;
        buffer.maximum_ore_bits = (if remaining > 5.0 {
            remaining * 0.5
        } else {
            remaining
        })
        .max(0.0)
        .to_bits();
        self.power += rate * uses[0] * seconds;
        self.time += seconds;
        (progress + (self.fraction() - before)).min(1.0)
    }
}

impl Laboratories {
    pub(super) fn active(&self, lab: ObjectHandle) -> bool {
        self.tasks.iter().any(|order| order.lab == lab)
    }

    pub(super) fn rejected(&mut self, clan: usize, lab: ObjectHandle, reason: &str) {
        let rates = self.orders.entry((clan, lab.slot)).or_default();
        rates.1 += 1;
        if rates.1 == 1 {
            eprintln!(
                "script_lab_research_rejected clan={clan} lab={} reason={reason}",
                lab.slot
            );
        }
    }

    pub(super) fn report(&self) -> LaboratoryReport {
        LaboratoryReport {
            orders: self
                .orders
                .iter()
                .map(|(&(clan, lab), &(accepted, rejected))| (clan, lab, accepted, rejected))
                .collect(),
            completed: self
                .completed
                .iter()
                .map(|(&(clan, lab, row), &count)| (clan, lab, row, count))
                .collect(),
            available: self
                .available
                .iter()
                .map(|(&(clan, row), &count)| (clan, row, count))
                .collect(),
            stopped: self
                .stopped
                .iter()
                .map(|(&(clan, lab), &count)| (clan, lab, count))
                .collect(),
            clock_ticks: self
                .clock_ticks
                .iter()
                .map(|(&(clan, lab), &count)| (clan, lab, count))
                .collect(),
        }
    }
}

pub(super) fn order(
    state: &mut Laboratories,
    catalogs: &mut BTreeMap<usize, crate::clan_research::ClanResearch>,
    drafts: &[crate::MissionObjectDraft],
    free: &crate::research::FreeResearch,
    world: &World,
    vfs: Option<&dyn Vfs>,
    clan: usize,
    lab: ObjectHandle,
    now_ms: u32,
) -> Result<(), String> {
    let owner = OwnerId(u16::try_from(clan).map_err(|_| "invalid research clan")?);
    let identity = identity_metadata(world, lab).map_err(|error| error.to_string())?;
    if identity.owner_id != Some(owner) {
        return Err("laboratory belongs to another clan".into());
    }
    let slot = identity
        .original_id
        .ok_or("laboratory has no resource slot")?
        .0 as usize;
    let draft = drafts.get(slot).ok_or("laboratory draft is unavailable")?;
    if draft.unit_dat_header_flags != Some(LAB_CLASS) {
        return Err("research target is not a laboratory".into());
    }
    let vfs = vfs.ok_or("research VFS is unavailable")?;
    let catalog = catalogs
        .get_mut(&clan)
        .ok_or("research catalog is unavailable")?;
    catalog.refresh(vfs)?;
    if state.active(lab) || free.active(lab) {
        return Err("laboratory is busy".into());
    }
    let selected = catalog
        .candidates
        .entries
        .get(catalog.candidates.selected_index as usize)
        .ok_or("research project is unavailable")?;
    if selected.status != 0 {
        return Err("research project is already complete".into());
    }
    let bytes = vfs
        .read(&selected.path)
        .map_err(|error| error.to_string())?;
    let unit = fparkan_prototype::decode_unit_dat(&bytes).map_err(|error| error.to_string())?;
    let rows = research_project_rows(&unit, &catalog.table).map_err(|error| error.to_string())?;
    if rows.is_empty() {
        return Err("research project has no technologies".into());
    }
    let duration = f32::from_bits(
        crate::research::property(draft, b"FreeResearchTime").unwrap_or(2.0_f32.to_bits()),
    );
    if !duration.is_finite() || duration < 0.0 {
        return Err("invalid laboratory research time".into());
    }
    if state.rates.is_none() {
        state.rates = Some(load_rates(vfs)?);
    }
    state.orders.entry((clan, lab.slot)).or_default().0 += 1;
    state.tasks.push(Order {
        clan,
        lab,
        slot,
        rows,
        current: None,
        duration,
        last_ms: now_ms,
        clock: Default::default(),
    });
    eprintln!(
        "sim_coverage script_lab_research_approx=1 clan={clan} lab={} project={}",
        lab.slot,
        selected.path.as_str()
    );
    Ok(())
}

pub(super) fn tick(mission: &mut LoadedMissionState, world: &World, now_ms: u32, rng: &mut u32) {
    let mut state = std::mem::take(&mut mission.laboratories);
    let mut index = 0;
    while index < state.tasks.len() {
        let task = &mut state.tasks[index];
        if !task.clock.due(now_ms, rng) {
            index += 1;
            continue;
        }
        let count = state
            .clock_ticks
            .entry((task.clan, task.lab.slot))
            .or_default();
        *count += 1;
        if *count == 1 {
            eprintln!("sim_coverage script_lab_clock_approx=1 clan={} lab={} (active task timer; idle owner phases remain open)", task.clan, task.lab.slot);
        }
        let result = advance(&mut state, index, mission, world, now_ms);
        if matches!(result, Ok(true)) {
            index += 1;
            continue;
        }
        let task = state.tasks.remove(index);
        if let Err(reason) = result {
            *state.stopped.entry((task.clan, task.lab.slot)).or_default() += 1;
            eprintln!(
                "script_lab_research_stopped clan={} lab={} reason={reason}",
                task.clan, task.lab.slot
            );
        }
        if let Some(ore) = mission
            .behavior_ore_states
            .iter_mut()
            .find(|ore| ore.handle == task.lab)
        {
            ore.current_ore_bits = 0;
            ore.maximum_ore_bits = 0;
        }
        if let Some(table) = mission
            .control_device_tables
            .iter_mut()
            .find(|table| table.handle == task.lab)
        {
            if let Err(reason) = power(table, 0.0) {
                eprintln!(
                    "script_lab_power_reset_failed lab={} reason={reason}",
                    task.lab.slot
                );
            }
        }
    }
    mission.laboratories = state;
}

fn advance(
    state: &mut Laboratories,
    index: usize,
    mission: &mut LoadedMissionState,
    world: &World,
    now_ms: u32,
) -> Result<bool, String> {
    let order = &mut state.tasks[index];
    let owner = OwnerId(order.clan as u16);
    if !identity_metadata(world, order.lab).is_ok_and(|id| id.owner_id == Some(owner)) {
        return Ok(false);
    }
    let seconds = now_ms.wrapping_sub(order.last_ms) as f32 * 0.001;
    order.last_ms = now_ms;
    let catalog = mission
        .clan_ai_events
        .research_catalogs
        .get_mut(&order.clan)
        .ok_or("research catalog disappeared")?;
    let ore = mission
        .behavior_ore_states
        .iter_mut()
        .find(|ore| ore.handle == order.lab)
        .ok_or("laboratory ore state is unavailable")?;
    let table = mission
        .control_device_tables
        .iter_mut()
        .find(|table| table.handle == order.lab)
        .ok_or("laboratory controls are unavailable")?;
    let uses = state.rates.ok_or("laboratory rates are unavailable")?;
    if order.current.is_none() {
        let Some(row) = order.rows.iter().copied().find(|&row| {
            catalog
                .table
                .flag_byte(row)
                .is_some_and(|flag| flag & 3 == 1)
        }) else {
            return Ok(false);
        };
        let free = mission.free_research.take_free(order.slot);
        let data = catalog
            .table
            .row_words(row)
            .ok_or("research row is unavailable")?;
        let ore_cost = if free { 0.0 } else { f32::from_bits(data[1]) };
        let power_cost = if free { 0.0 } else { f32::from_bits(data[0]) };
        if !ore_cost.is_finite() || !power_cost.is_finite() {
            return Err("research costs are non-finite".into());
        }
        order.current = Some((
            row,
            Work {
                ore_cost,
                power_cost,
                duration: order.duration,
                ..Default::default()
            },
        ));
        ore.current_ore_bits = 0;
        ore.maximum_ore_bits = ore_cost.to_bits();
        power(table, uses[0])?;
        eprintln!("script_lab_technology_started clan={} lab={} row={row} ore={ore_cost} power={power_cost} free={free} queue_time_ms={now_ms}", order.clan, order.lab.slot);
    }
    let (row, work) = order.current.as_mut().unwrap();
    if catalog
        .table
        .flag_byte(*row)
        .is_some_and(|flag| flag & 2 != 0)
    {
        order.current = None;
        return Ok(true);
    }
    let rate = crate::query_behavior_profile_ore_rate(table, 1.0_f32.to_bits())
        .map_err(|error| format!("{error:?}"))?;
    let progress = state.progress.entry((order.clan, *row)).or_default();
    *progress = work.advance(seconds, rate, uses, ore, *progress);
    if *progress + 0.001 >= 1.0 {
        *progress = 1.0;
        let changes = catalog
            .table
            .complete_research(*row)
            .map_err(|error| error.to_string())?;
        for row in changes.available {
            *state.available.entry((order.clan, row)).or_default() += 1;
            eprintln!(
                "script_lab_technology_available clan={} row={row} queue_time_ms={now_ms}",
                order.clan
            );
        }
        for row in changes.learned {
            mission.free_research.learned.insert((owner, row));
            *state
                .completed
                .entry((order.clan, order.lab.slot, row))
                .or_default() += 1;
            eprintln!(
                "script_lab_technology_researched clan={} lab={} row={row} queue_time_ms={now_ms}",
                order.clan, order.lab.slot
            );
        }
        order.current = None;
    }
    Ok(true)
}

fn power(table: &mut MissionControlDeviceTableState, demand: f32) -> Result<(), String> {
    let crate::MissionControlDeviceTableStatus::Available(devices) = &table.status else {
        return Err("laboratory control table is unavailable".into());
    };
    for index in 0..devices.len() {
        crate::set_control_device_table_selector(table, index, 0x500, demand)
            .map_err(|error| format!("{error:?}"))?;
    }
    Ok(())
}

fn load_rates(vfs: &dyn Vfs) -> Result<[f32; 2], String> {
    let path = crate::normalize_engine_path("research profile", "behpsp.res")
        .map_err(|error| error.to_string())?;
    let data = vfs.read(&path).map_err(|error| error.to_string())?;
    let archive = fparkan_assets::decode_nres_payload(data).map_err(|error| error.to_string())?;
    let entry = archive
        .find("prof_institute.var")
        .ok_or("laboratory profile is unavailable")?;
    let rates =
        decode_behavior_work_rates(archive.payload(entry).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    let values = [rates.power_bits, rates.ore_bits].map(f32::from_bits);
    if values.iter().any(|rate| !rate.is_finite() || *rate < 0.0) {
        return Err("invalid laboratory work rates".into());
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn buffer(current: f32) -> BehaviorOreState {
        BehaviorOreState {
            original_id: fparkan_world::OriginalObjectId(0),
            handle: ObjectHandle {
                slot: 0,
                generation: 0,
            },
            class_flags: LAB_CLASS,
            current_ore_bits: current.to_bits(),
            maximum_ore_bits: 0,
        }
    }

    #[test]
    fn native_paid_tick_uses_the_slowest_completion_fraction_and_requests_remaining_ore() {
        let mut work = Work {
            ore_cost: 10.0,
            power_cost: 30.0,
            duration: 10.0,
            ..Default::default()
        };
        let mut ore = buffer(4.0);
        assert_eq!(work.advance(1.0, 2.0, [3.0, 1.0], &mut ore, 0.0), 0.1);
        assert_eq!([work.ore, work.power, work.time], [2.0, 6.0, 1.0]);
        assert_eq!(ore.current_ore_bits, 0);
        assert_eq!(f32::from_bits(ore.maximum_ore_bits), 4.0);
    }

    #[test]
    fn native_free_work_finishes_by_time_without_resource_supply() {
        let mut work = Work {
            duration: 5.0,
            ..Default::default()
        };
        let mut ore = buffer(0.0);
        let progress = work.advance(1.0, 0.0, [3.0, 1.0], &mut ore, 0.0);
        assert_eq!(progress, 0.2);
        assert_eq!(work.advance(4.0, 0.0, [3.0, 1.0], &mut ore, progress), 1.0);
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_stock_ai_laboratories_continue_research_and_keep_the_request_pending() {
        use crate::{
            create, frame, load_gameplay_mission, EngineServices, MissionMode, MissionRequest,
            MissionStartupInput,
        };
        use fparkan_vfs::DirectoryVfs;
        use std::sync::Arc;
        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").unwrap();
        let vfs = Arc::new(DirectoryVfs::new(root));
        assert_eq!(load_rates(vfs.as_ref()).unwrap(), [3.0, 1.0]);
        let mut engine = create(EngineServices::new(vfs)).unwrap();
        load_gameplay_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/Single.02/data.tma".into(),
                mode: MissionMode::SingleMission,
            },
            MissionStartupInput {
                queue_time_ms: 0,
                research_creation_gate: false,
            },
            None,
        )
        .unwrap();
        for now in (0..=900_000).step_by(100) {
            frame(&mut engine, now).unwrap();
            let loaded = engine.loaded.as_ref().unwrap();
            for task in &loaded.laboratories.tasks {
                assert!(
                    loaded
                        .clan_ai_events
                        .pending_problems()
                        .any(|problem| problem == (task.clan, 10)),
                    "research request was removed while laboratory {} was busy at {now}",
                    task.lab.slot
                );
            }
        }
        let loaded = engine.loaded.as_ref().unwrap();
        assert!(loaded.clan_ai_events.stops().next().is_none());
        assert!(
            !loaded.laboratories.completed.is_empty(),
            "no completed laboratory technology"
        );
        assert!(!loaded.free_research.learned.is_empty());
        assert!(loaded.laboratories.stopped.is_empty());
        for clan in [1, 2] {
            assert!(
                loaded
                    .laboratories
                    .completed
                    .keys()
                    .filter(|&&(owner, _, _)| owner == clan)
                    .count()
                    >= 2,
                "clan {clan} did not continue research after its first project"
            );
        }
        assert!(
            !loaded.laboratories.available.is_empty(),
            "later projects did not unlock technologies"
        );
        for &(clan, _, row, _) in &loaded.laboratories.report().completed {
            assert!(loaded
                .free_research
                .learned
                .contains(&(OwnerId(clan as u16), row)));
            assert_eq!(
                loaded.clan_ai_events.research_catalogs[&clan]
                    .table
                    .flag_byte(row)
                    .unwrap()
                    & 2,
                2
            );
        }
        assert!(loaded
            .clan_ai_events
            .research_status_stubs()
            .all(|(_, handler, _)| handler != 41));
        println!(
            "stock research completions={:?}",
            loaded.laboratories.report().completed
        );
    }
}
