use super::{
    autodemo::{reinforcement_position, MOBILE_UNIT, PLANT_CLASS},
    behavior_ore_index_for_handle,
    factory_release::FactoryDeparture,
    mission_events::on_communicator,
    normalize_engine_path, query_behavior_profile_ore_rate,
    research::{load_clan_table, RESEARCHED},
    route_obstacles::BuildingFootprints,
    set_control_device_table_selector,
    unit_spawn::{spawn_prepared_unit, SpawnedUnit},
    BehaviorOreState, Engine, EngineError, LoadedMissionState, MissionControlDeviceTableState,
    MissionControlDeviceTableStatus, MissionObjectDraft, StartingCockpit, World,
};
use fparkan_assets::ResearchTrfDocument;
use fparkan_path::NormalizedPath;
use fparkan_prototype::{unit_component_tree, UnitComponentRecord};
use fparkan_world::{
    handle_by_original_id, identity_metadata, transform_state, ObjectHandle, OwnerId,
};
use std::collections::VecDeque;

// shortcut: GPU geometry is prepared for a bounded number of live builds.
// Lift the cap when the renderer can instantiate new roots during a mission.
pub(super) const RESERVE_SLOTS: usize = 8;
const PAID_MINIMUM_MS: u32 = 5_000;
const RESOURCE_EPSILON: f32 = 0.01;

pub(super) fn mission_prebuilds(config: &[u8]) -> Result<Vec<NormalizedPath>, EngineError> {
    let mut schemes = Vec::new();
    let mut in_prebuild = false;
    for line in config.split(|&byte| byte == b'\n') {
        let line = line.trim_ascii();
        let mut words = line
            .split(u8::is_ascii_whitespace)
            .filter(|word| !word.is_empty());
        match words.next() {
            Some(word) if word.eq_ignore_ascii_case(b"object") => {
                in_prebuild = words
                    .next()
                    .is_some_and(|name| name.eq_ignore_ascii_case(b"prebuild"));
            }
            Some(word) if word.eq_ignore_ascii_case(b"end") => in_prebuild = false,
            Some(word) if in_prebuild && !word.starts_with(b"#") && !word.starts_with(b"//") => {
                let mut fields = line.splitn(3, |&byte| byte == b'"');
                let key = fields.next().unwrap_or_default();
                let name = fields.next().filter(|name| !name.is_empty());
                if key.trim_ascii_end().ends_with(b"=") && fields.next().is_some() {
                    if let Some(name) = name {
                        let name = std::str::from_utf8(name).map_err(production_error)?;
                        let path = normalize_engine_path(
                            "mission prebuild",
                            &format!("UNITS/UNITS/PREBLD/{name}"),
                        )?;
                        if !schemes.iter().any(|entry: &NormalizedPath| {
                            entry.as_bytes().eq_ignore_ascii_case(path.as_bytes())
                        }) {
                            schemes.push(path);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(schemes)
}

pub(super) struct Production {
    reserve_start: usize,
    schemes: Vec<NormalizedPath>,
    player_scheme_count: usize,
    ai_schemes: Vec<(OwnerId, u32, NormalizedPath)>,
    controls: Vec<MissionControlDeviceTableState>,
    plants: Vec<(usize, u32)>,
    pending: VecDeque<Order>,
    clock_ticks: std::collections::BTreeMap<(u16, u32), u64>,
    buildings: BuildingFootprints,
    rng: u32,
}

struct Order {
    slot: usize,
    plant: ObjectHandle,
    plant_index: usize,
    owner: OwnerId,
    start_ms: u32,
    duration_ms: u32,
    paid: Option<PaidOrder>,
    clock: crate::behavior_clock::TaskClock,
}

#[derive(Default)]
pub(super) struct Cost {
    pub(super) ore: f32,
    power: f32,
}

struct PaidOrder {
    cost: Cost,
    collected: Cost,
    last_ms: u32,
}

impl PaidOrder {
    fn advance(&mut self, now_ms: u32, rate: f32, ore: &mut BehaviorOreState) -> bool {
        let seconds = now_ms.wrapping_sub(self.last_ms) as f32 * 0.001;
        if seconds == 0.0 {
            return false;
        }
        self.last_ms = now_ms;
        if self.collected.power + RESOURCE_EPSILON < self.cost.power {
            self.collected.power += seconds * rate * self.cost.ore;
        }
        if self.collected.ore + RESOURCE_EPSILON < self.cost.ore {
            let fraction = self.collected.ore / (self.cost.ore + RESOURCE_EPSILON);
            let factor = if fraction > 0.8 {
                3.0
            } else {
                fraction.max(0.2)
            };
            let requested = (self.cost.ore - self.collected.ore) * factor * rate * 0.2 + 0.07;
            let available = f32::from_bits(ore.current_ore_bits).max(0.0);
            let taken = requested.min(available);
            ore.current_ore_bits = (available - taken).to_bits();
            self.collected.ore += taken;
            ore.maximum_ore_bits = if self.collected.ore + RESOURCE_EPSILON >= self.cost.ore {
                0
            } else {
                requested.to_bits()
            };
        }
        true
    }

    fn ready(&self) -> bool {
        self.collected.ore + RESOURCE_EPSILON >= self.cost.ore
            && self.collected.power + RESOURCE_EPSILON >= self.cost.power
    }
}

impl Production {
    pub(super) fn clock_ticks(&self) -> impl Iterator<Item = (u16, u32, u64)> + '_ {
        self.clock_ticks
            .iter()
            .map(|(&(clan, plant), &count)| (clan, plant, count))
    }

    pub(super) fn reserved_minds(&self, world: &World, owner: OwnerId) -> usize {
        self.pending
            .iter()
            .filter(|order| {
                order.owner == owner
                    && identity_metadata(world, order.plant)
                        .is_ok_and(|identity| identity.owner_id == Some(owner))
            })
            .count()
    }

    pub(super) fn is_building_at(&self, plant: ObjectHandle, owner: OwnerId) -> bool {
        self.pending
            .iter()
            .any(|order| order.plant == plant && order.owner == owner)
    }

    pub(super) fn new(
        reserve_start: usize,
        schemes: Vec<NormalizedPath>,
        player_scheme_count: usize,
        ai_schemes: Vec<(OwnerId, u32, NormalizedPath)>,
        controls: Vec<MissionControlDeviceTableState>,
        drafts: &[MissionObjectDraft],
    ) -> Result<Self, EngineError> {
        if controls.len() / RESERVE_SLOTS != schemes.len()
            || !controls.len().is_multiple_of(RESERVE_SLOTS)
            || drafts.len().checked_sub(reserve_start) != Some(controls.len())
        {
            return Err(EngineError::Production(
                "missing prepared build scheme slots".into(),
            ));
        }
        if drafts[reserve_start..]
            .iter()
            .step_by(RESERVE_SLOTS)
            .any(|draft| {
                !draft.unit_dat_header_flags.is_some_and(|class| {
                    class & MOBILE_UNIT != 0 && class != super::HERO_UNIT_CLASS
                })
            })
        {
            return Err(EngineError::Production(
                "build scheme must be a mobile Unit DAT".into(),
            ));
        }
        let mut production = Self {
            reserve_start,
            schemes,
            player_scheme_count,
            ai_schemes,
            controls,
            plants: Vec::new(),
            pending: VecDeque::new(),
            clock_ticks: Default::default(),
            buildings: BuildingFootprints::default(),
            rng: 1,
        };
        production.register_factories(0, &drafts[..reserve_start]);
        Ok(production)
    }

    pub(super) fn register_factories(&mut self, first_slot: usize, drafts: &[MissionObjectDraft]) {
        self.plants
            .retain(|&(index, _)| !(first_slot..first_slot + drafts.len()).contains(&index));
        self.plants.extend(
            drafts
                .iter()
                .enumerate()
                .filter(|(_, draft)| draft.unit_dat_header_flags == Some(PLANT_CLASS))
                .map(|(index, draft)| {
                    let count = draft
                        .properties
                        .iter()
                        .find(|property| property.name_raw == b"FreeBotNum")
                        .map_or(1, |property| property.raw_value[1] as i32);
                    (first_slot + index, count.max(0) as u32)
                }),
        );
    }

    pub(super) fn tick(
        &mut self,
        world: &mut World,
        mission: &mut LoadedMissionState,
        now_ms: u32,
        behavior_rng: &mut u32,
    ) -> Result<Option<SpawnedUnit>, EngineError> {
        for _ in 0..self.pending.len() {
            let Some(mut order) = self.pending.pop_front() else {
                break;
            };
            let owned = identity_metadata(world, order.plant)
                .is_ok_and(|identity| identity.owner_id == Some(order.owner));
            if owned {
                if !order.clock.due(now_ms, behavior_rng) {
                    self.pending.push_back(order);
                    continue;
                }
                let count = self
                    .clock_ticks
                    .entry((order.owner.0, order.plant.slot))
                    .or_default();
                *count += 1;
                if *count == 1 {
                    eprintln!("sim_coverage script_factory_clock_approx=1 clan={} plant={} (active task timer; idle owner phases remain open)", order.owner.0, order.plant.slot);
                }
            }
            if let Some(unit) = self.advance_order(world, mission, now_ms, &mut order)? {
                return Ok(Some(unit));
            }
            if owned {
                self.pending.push_back(order);
            }
        }
        Ok(None)
    }

    fn advance_order(
        &mut self,
        world: &mut World,
        mission: &mut LoadedMissionState,
        now_ms: u32,
        order: &mut Order,
    ) -> Result<Option<SpawnedUnit>, EngineError> {
        if !identity_metadata(world, order.plant)
            .is_ok_and(|identity| identity.owner_id == Some(order.owner))
        {
            if order.paid.is_some() && identity_metadata(world, order.plant).is_ok() {
                let (table, ore) = factory_resources(mission, order.plant)?;
                set_factory_power(table, 0.0)?;
                ore.maximum_ore_bits = 0;
            }
            return Ok(None);
        }
        let mut ready = now_ms.wrapping_sub(order.start_ms) >= order.duration_ms;
        if let Some(paid) = &mut order.paid {
            let (table, ore) = factory_resources(mission, order.plant)?;
            let rate = factory_rate(table)?;
            if paid.advance(now_ms, rate, ore)
                && paid.collected.power + RESOURCE_EPSILON >= paid.cost.power
            {
                set_factory_power(table, 0.0)?;
            }
            ready &= paid.ready();
        }
        if !ready
            || mission
                .unit_patrol
                .as_ref()
                .is_some_and(|patrol| patrol.has_factory_departure(order.plant))
        {
            return Ok(None);
        }
        let pose = transform_state(world, order.plant)?;
        // shortcut: use the outdoor direction to select the exit from the apron.
        let preferred = reinforcement_position(
            pose.position.map(f32::from_bits),
            pose.orientation.map(f32::from_bits),
            pose.scale.map(f32::from_bits),
            order.plant_index,
            &mission.mission_assets,
            &mission.terrain,
            &mut self.rng,
        )?;
        let Some(site) = super::factory_release::clear_release_position(
            world,
            mission,
            &mut self.buildings,
            order.plant,
            order.plant_index,
            order.slot,
            preferred,
        )?
        else {
            return Ok(None);
        };
        let handle = spawn_prepared_unit(
            world,
            mission,
            order.slot,
            order.owner.0,
            site.position,
            self.controls[order.slot - self.reserve_start].clone(),
        )?;
        let draft = &mission.object_drafts[order.slot];
        let class = draft
            .unit_dat_header_flags
            .expect("prepared factory object has a class");
        if crate::clan_factory_schemes::AUXILIARY_CLASSES.contains(&class) {
            let id = draft
                .original_id
                .expect("prepared factory object has an ID");
            mission
                .mission_events
                .issue_logic_id(&mission.mission, world, handle);
            mission
                .behavior_ore_states
                .retain(|ore| ore.original_id != id);
            mission
                .behavior_ore_states
                .push(crate::initialize_behavior_ore_state(
                    id,
                    handle,
                    class,
                    &draft.properties,
                ));
        }
        if order.paid.is_none() {
            let (_, count) = self
                .plants
                .iter_mut()
                .find(|(index, _)| *index == order.plant_index)
                .expect("free order belongs to a prepared factory");
            *count -= 1;
        }
        if let Some(departure) =
            FactoryDeparture::from_site(handle, order.plant, order.slot, site, mission)
        {
            mission
                .unit_patrol
                .get_or_insert_with(crate::unit_patrol::UnitPatrol::scripted)
                .begin_factory_departure(departure, now_ms);
        }
        Ok(Some(SpawnedUnit {
            object_index: order.slot,
            handle,
            clan: order.owner.0,
        }))
    }
}

/// Queues the selected robot scheme at a friendly factory communicator.
/// Returns the minimum construction time in milliseconds on success. A paid
/// order also waits for its ore and power.
///
/// # Errors
///
/// Returns a diagnostic when no eligible factory or scheme is available or the
/// prepared construction slot is occupied.
pub fn order_factory_robot(
    engine: &mut Engine,
    cockpit: StartingCockpit,
    now_ms: u32,
) -> Result<u32, EngineError> {
    order_factory_scheme(engine, cockpit, 0, now_ms)
}

/// Returns factory schemes that fit the friendly factory at the current communicator.
/// Each entry preserves its configured index for use with [`order_factory_scheme`].
pub fn factory_schemes(
    engine: &Engine,
    cockpit: StartingCockpit,
) -> Result<Vec<(usize, &str)>, EngineError> {
    let (mission, production) = loaded_production(engine)?;
    let (plant, _) = factory_at_communicator(&engine.world, mission, production, cockpit)?;
    Ok(schemes_for_plant(mission, production, plant)
        .filter(|&(scheme, _)| scheme < production.player_scheme_count)
        .collect())
}

/// Lists the live factories of the player's clan and matching configured schemes.
pub fn clan_factory_schemes(
    engine: &Engine,
    cockpit: StartingCockpit,
) -> Result<Vec<(ObjectHandle, usize, &str)>, EngineError> {
    let (mission, production) = loaded_production(engine)?;
    let owner = player_owner(&engine.world, cockpit)?;
    Ok(owned_factories(&engine.world, mission, production, owner)
        .flat_map(|(index, factory, _)| {
            schemes_for_plant(mission, production, index)
                .filter(|&(scheme, _)| scheme < production.player_scheme_count)
                .map(move |(scheme, path)| (factory, scheme, path))
        })
        .collect())
}

fn schemes_for_plant<'a>(
    mission: &'a LoadedMissionState,
    production: &'a Production,
    plant: usize,
) -> impl Iterator<Item = (usize, &'a str)> + 'a {
    let plant_key = &mission.object_drafts[plant].unit_components[0].resource_raw;
    production
        .schemes
        .iter()
        .enumerate()
        .filter_map(|(index, path)| {
            let draft = &mission.object_drafts[production.reserve_start + index * RESERVE_SLOTS];
            free_duration_ms(plant_key, &draft.unit_components[0].resource_raw)?;
            Some((index, path.as_str()))
        })
}

/// Orders a particular configured scheme using the current friendly factory.
pub fn order_factory_scheme(
    engine: &mut Engine,
    cockpit: StartingCockpit,
    scheme: usize,
    now_ms: u32,
) -> Result<u32, EngineError> {
    let (mission, production) = loaded_production(engine)?;
    let (_, plant) = factory_at_communicator(&engine.world, mission, production, cockpit)?;
    order_clan_factory_scheme(engine, cockpit, plant, scheme, now_ms)
}

/// Places an order at a live factory owned by the player's clan.
pub fn order_clan_factory_scheme(
    engine: &mut Engine,
    cockpit: StartingCockpit,
    plant: ObjectHandle,
    scheme: usize,
    now_ms: u32,
) -> Result<u32, EngineError> {
    let owner = player_owner(&engine.world, cockpit)?;
    if engine
        .production
        .as_ref()
        .is_some_and(|production| scheme >= production.player_scheme_count)
    {
        return Err(production_error("unknown factory scheme"));
    }
    order_owned_factory(engine, owner, plant, scheme, now_ms)
}

fn order_owned_factory(
    engine: &mut Engine,
    owner: OwnerId,
    plant: ObjectHandle,
    scheme: usize,
    now_ms: u32,
) -> Result<u32, EngineError> {
    let (Some(mission), Some(production)) = (&mut engine.loaded, &mut engine.production) else {
        return Err(EngineError::Production(
            "mission has no prepared robot design; use --build-scheme to choose one".into(),
        ));
    };
    let (plant_index, _, is_free) = owned_factories(&engine.world, mission, production, owner)
        .find(|&(_, handle, _)| handle == plant)
        .ok_or_else(|| {
            EngineError::Production(
                "selected factory no longer belongs to the ordering clan".into(),
            )
        })?;
    if production.pending.iter().any(|order| order.plant == plant) {
        return Err(EngineError::Production(
            "factory order already in progress".into(),
        ));
    }
    if scheme >= production.schemes.len() {
        return Err(EngineError::Production("unknown factory scheme".into()));
    }
    let slot_start = production.reserve_start + scheme * RESERVE_SLOTS;
    let free_slot = |index: usize| {
        mission.object_drafts[index]
            .original_id
            .is_some_and(|id| handle_by_original_id(&engine.world, id).is_none())
            && !production.pending.iter().any(|order| order.slot == index)
    };
    let live = (production.reserve_start..production.reserve_start + production.controls.len())
        .filter(|&index| !free_slot(index))
        .take(RESERVE_SLOTS)
        .count();
    if live >= RESERVE_SLOTS {
        return Err(EngineError::Production(
            "all prepared factory robot slots are occupied".into(),
        ));
    }
    let slot = (slot_start..slot_start + RESERVE_SLOTS)
        .find(|&index| free_slot(index))
        .ok_or_else(|| {
            EngineError::Production("all prepared factory robot slots are occupied".into())
        })?;
    let draft = &mission.object_drafts[slot];
    let plant_draft = &mission.object_drafts[plant_index];
    let plant_key = &plant_draft.unit_components[0].resource_raw;
    let robot_key = &draft.unit_components[0].resource_raw;
    let free_ms = free_duration_ms(plant_key, robot_key).ok_or_else(|| {
        EngineError::Production("scheme size is unsupported by this factory".into())
    })?;
    let (duration_ms, paid) = if is_free {
        (free_ms, None)
    } else {
        let cached = mission
            .research_trf_tables
            .iter()
            .find(|table| table.clan_index == usize::from(owner.0));
        let document;
        let research = if let Some(table) = cached {
            &table.document
        } else {
            let vfs = engine
                .services
                .vfs
                .as_ref()
                .ok_or(EngineError::MissingVfs)?;
            document = load_clan_table(vfs, mission, owner)?;
            &document
        };
        let mut cost = scheme_cost(&draft.unit_components, research, |row| {
            mission.free_research.learned.contains(&(owner, row))
        })?;
        let (table, ore) = factory_resources(mission, plant)?;
        let rate = factory_rate(table)?;
        if rate > RESOURCE_EPSILON {
            cost.ore /= rate;
        }
        if !cost.ore.is_finite() {
            return Err(EngineError::Production(
                "factory ore target overflow".into(),
            ));
        }
        set_factory_power(table, cost.ore)?;
        ore.maximum_ore_bits = cost.ore.to_bits();
        (
            PAID_MINIMUM_MS,
            Some(PaidOrder {
                cost,
                collected: Cost::default(),
                last_ms: now_ms,
            }),
        )
    };
    let order = Order {
        slot,
        plant,
        plant_index,
        owner,
        start_ms: now_ms,
        duration_ms,
        paid,
        clock: Default::default(),
    };
    production.pending.push_back(order);
    Ok(duration_ms)
}

pub(super) fn order_script_factory(
    engine: &mut Engine,
    owner: OwnerId,
    plant: ObjectHandle,
    class: u32,
    selection: u32,
    now_ms: u32,
) -> Result<u32, EngineError> {
    let (mission, production) = loaded_production(engine)?;
    let (index, _, _) = owned_factories(&engine.world, mission, production, owner)
        .find(|&(_, handle, _)| handle == plant)
        .ok_or_else(|| production_error("script factory is unavailable"))?;
    let auxiliary = crate::clan_factory_schemes::AUXILIARY_CLASSES.contains(&class);
    let use_catalog = (auxiliary && selection == crate::clan_factory_schemes::SELECT_FASTEST)
        || (class == crate::boarding::WARBOT_CLASS
            && selection == crate::clan_factory_schemes::SELECT_STRONGEST);
    let ai_scheme = if use_catalog {
        production
            .ai_schemes
            .iter()
            .find_map(|(clan, kind, path)| (*clan == owner && *kind == class).then_some(path))
    } else {
        None
    };
    let scheme = ai_scheme
        .and_then(|selected| {
            schemes_for_plant(mission, production, index)
                .find(|&(_, path)| selected.as_str() == path)
        })
        .or_else(|| {
            if auxiliary {
                return None;
            }
            // shortcut: retain the compatible reserve for unsupported catalog requests.
            schemes_for_plant(mission, production, index).find(|&(scheme, _)| {
                let draft =
                    &mission.object_drafts[production.reserve_start + scheme * RESERVE_SLOTS];
                scheme < production.player_scheme_count
                    && draft.unit_dat_header_flags == Some(class)
            })
        })
        .map(|(scheme, _)| scheme)
        .ok_or_else(|| production_error("script factory has no compatible prepared scheme"))?;
    order_owned_factory(engine, owner, plant, scheme, now_ms)
}

fn loaded_production(engine: &Engine) -> Result<(&LoadedMissionState, &Production), EngineError> {
    engine
        .loaded
        .as_ref()
        .zip(engine.production.as_ref())
        .ok_or_else(|| {
            EngineError::Production(
                "mission has no prepared robot design; use --build-scheme to choose one".into(),
            )
        })
}

fn factory_at_communicator(
    world: &World,
    mission: &LoadedMissionState,
    production: &Production,
    cockpit: StartingCockpit,
) -> Result<(usize, ObjectHandle), EngineError> {
    let owner = player_owner(world, cockpit)?;
    let position = transform_state(world, cockpit.handle)?
        .position
        .map(f32::from_bits);
    owned_factories(world, mission, production, owner)
        .find_map(|(index, plant, _)| {
            on_communicator(
                &mission.mission_assets,
                index,
                PLANT_CLASS,
                transform_state(world, plant).ok()?,
                position,
            )
            .then_some((index, plant))
        })
        .ok_or_else(|| EngineError::Production("stand on a friendly factory communicator".into()))
}

fn player_owner(world: &World, cockpit: StartingCockpit) -> Result<OwnerId, EngineError> {
    identity_metadata(world, cockpit.handle)?
        .owner_id
        .ok_or_else(|| EngineError::Production("player clan is unavailable".into()))
}

fn owned_factories<'a>(
    world: &'a World,
    mission: &'a LoadedMissionState,
    production: &'a Production,
    owner: OwnerId,
) -> impl Iterator<Item = (usize, ObjectHandle, bool)> + 'a {
    production.plants.iter().filter_map(move |&(index, slots)| {
        let plant = handle_by_original_id(world, mission.object_drafts[index].original_id?)?;
        (identity_metadata(world, plant).ok()?.owner_id == Some(owner)).then_some((
            index,
            plant,
            slots != 0,
        ))
    })
}

fn production_error(error: impl std::fmt::Display) -> EngineError {
    EngineError::Production(error.to_string())
}

pub(super) fn scheme_cost(
    records: &[UnitComponentRecord],
    research: &ResearchTrfDocument,
    learned: impl Fn(usize) -> bool,
) -> Result<Cost, EngineError> {
    let tree = unit_component_tree(records).map_err(production_error)?;
    if tree.is_empty() {
        return Err(EngineError::Production("empty construction scheme".into()));
    }
    let mut stack = vec![0];
    let mut cost = Cost::default();
    while let Some(index) = stack.pop() {
        let component = &records[index];
        if component.kind == 0 && index != 0 {
            continue;
        }
        stack.extend(tree[index].children.iter().rev().copied());
        if component.kind == 0 {
            continue;
        }
        let unavailable = || {
            EngineError::Production(format!(
                "component {} is unavailable for construction",
                String::from_utf8_lossy(&component.resource_raw).trim_end_matches('\0')
            ))
        };
        let row = research
            .find_key(&component.resource_raw)
            .map_err(production_error)?
            .ok_or_else(unavailable)?;
        let mut flags = research.flag_byte(row).unwrap_or_default();
        if learned(row) {
            flags |= RESEARCHED;
        }
        if flags & 0b110 != 0b110 {
            return Err(unavailable());
        }
        let words = research.row_words(row).ok_or_else(unavailable)?;
        let ore = f32::from_bits(words[3]);
        let power = f32::from_bits(words[2]);
        if !ore.is_finite() || ore < 0.0 || !power.is_finite() || power < 0.0 {
            return Err(EngineError::Production(
                "invalid component construction price".into(),
            ));
        }
        cost.ore += ore;
        cost.power += power;
    }
    if !cost.ore.is_finite() || !cost.power.is_finite() {
        return Err(EngineError::Production(
            "construction price overflow".into(),
        ));
    }
    Ok(cost)
}

fn factory_resources(
    mission: &mut LoadedMissionState,
    plant: ObjectHandle,
) -> Result<(&mut MissionControlDeviceTableState, &mut BehaviorOreState), EngineError> {
    let table = super::behavior_control_index_for_handle(&mission.control_device_tables, plant)
        .map_err(production_error)?;
    let ore = behavior_ore_index_for_handle(&mission.behavior_ore_states, plant)
        .map_err(production_error)?;
    Ok((
        &mut mission.control_device_tables[table],
        &mut mission.behavior_ore_states[ore],
    ))
}

fn factory_rate(table: &MissionControlDeviceTableState) -> Result<f32, EngineError> {
    query_behavior_profile_ore_rate(table, 1.0_f32.to_bits())
        .map_err(|error| EngineError::Production(format!("{error:?}")))
}

fn set_factory_power(
    table: &mut MissionControlDeviceTableState,
    demand: f32,
) -> Result<(), EngineError> {
    let MissionControlDeviceTableStatus::Available(devices) = &table.status else {
        return Err(EngineError::Production(
            "factory controls are unavailable".into(),
        ));
    };
    for index in 0..devices.len() {
        set_control_device_table_selector(table, index, 0x500, demand)
            .map_err(|error| EngineError::Production(format!("{error:?}")))?;
    }
    Ok(())
}

fn free_duration_ms(plant: &[u8; 32], robot: &[u8; 32]) -> Option<u32> {
    // shortcut: stock chassis and factory keys supply sizes until the
    // calculated capacity and assembled-scheme descriptors are wired.
    let plant = plant.split(|&byte| byte == 0).next()?;
    let size = match robot.get(..3)? {
        [b'R' | b'r', b'_', b'T' | b't'] => 1,
        [b'R' | b'r', b'_', b'L' | b'l' | b'H' | b'h'] => 2,
        [b'R' | b'r', b'_', b'M' | b'm'] => 3,
        [b'R' | b'r', b'_', b'B' | b'b'] => 4,
        _ => return None,
    };
    let seconds = if plant.eq_ignore_ascii_case(b"fr_l_plant") {
        [30, 60].get(size - 1)?
    } else if plant.eq_ignore_ascii_case(b"fr_m_plant") {
        [20, 35, 60].get(size - 1)?
    } else if plant.eq_ignore_ascii_case(b"fr_b_plant") {
        [10, 20, 40, 60].get(size - 1)?
    } else {
        return None;
    };
    Some(*seconds * 1_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        capture_neutral_building, create, load_gameplay_mission, starting_cockpit, step_headless,
        EngineServices, MissionMode, MissionRequest, MissionStartupInput,
    };
    use fparkan_vfs::DirectoryVfs;
    use fparkan_world::{set_transform, InputSnapshot, OriginalObjectId};
    use std::sync::Arc;

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_stock_factory_releases_the_ranked_combat_design() {
        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed corpus");
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
        let placed_count = engine.loaded.as_ref().unwrap().mission.objects.len();
        for tick in 0..=60 * 12 {
            let now = tick * 1000 / 60;
            let frame = crate::frame(&mut engine, now).unwrap();
            let first_build = frame.snapshot.transforms.iter().find_map(|(handle, _)| {
                let identity = identity_metadata(&engine.world, *handle).ok()?;
                let index = identity.original_id?.0 as usize;
                (identity.owner_id == Some(OwnerId(1)) && index >= placed_count).then_some(index)
            });
            if let Some(index) = first_build {
                let draft = &engine.loaded.as_ref().unwrap().object_drafts[index];
                assert!(draft
                    .resource_name_raw
                    .eq_ignore_ascii_case(b"UNITS/UNITS/AI/12spd1.dat"));
                assert!((10_000..=10_200).contains(&now), "release at {now} ms");
                return;
            }
        }
        panic!("stock AI did not release a combat design in the opening");
    }

    #[test]
    fn mission_prebuilds_preserve_order_and_skip_duplicates() {
        let config = b"object other\n model1 = \"ignored.dat\"\nend\r\n\
            OBJECT PreBuild\r\n # model1 = \"comment.dat\"\r\n\
            model2=\"first.dat\"\r\n model1 = \"second.dat\"\r\n model3=\"FIRST.DAT\"\r\nend\n";
        assert_eq!(
            mission_prebuilds(config)
                .unwrap()
                .iter()
                .map(NormalizedPath::as_str)
                .collect::<Vec<_>>(),
            [
                "UNITS/UNITS/PREBLD/first.dat",
                "UNITS/UNITS/PREBLD/second.dat"
            ]
        );
        assert!(mission_prebuilds(
            b"object prebuild\nend\nobject other\nmodel1 = \"ignored.dat\"\nend"
        )
        .unwrap()
        .is_empty());
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_scripted_factory_builds_a_usable_builder() {
        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed corpus");
        let mut engine = create(EngineServices::new(std::sync::Arc::new(DirectoryVfs::new(
            root,
        ))))
        .unwrap();
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
        let owner = OwnerId(1);
        let loaded = engine.loaded.as_ref().unwrap();
        let former_builder = loaded
            .mission_events
            .clan_units(&engine.world, owner, false)
            .find(|&unit| {
                let identity = identity_metadata(&engine.world, unit).unwrap();
                loaded.object_drafts[identity.original_id.unwrap().0 as usize].unit_dat_header_flags
                    == Some(crate::construction::BUILDER_CLASS)
            })
            .unwrap();
        // Debug casualty exercises the real script's replacement request, not mission acceptance.
        fparkan_world::request_delete(&mut engine.world, former_builder).unwrap();
        fparkan_world::step(&mut engine.world, &InputSnapshot).unwrap();
        let mut builder = None;
        for time in (0..=180_000).step_by(100) {
            if let Some(unit) = step_headless(&mut engine, InputSnapshot::default(), time)
                .unwrap()
                .produced_unit
            {
                if engine.loaded.as_ref().unwrap().object_drafts[unit.object_index]
                    .unit_dat_header_flags
                    == Some(crate::construction::BUILDER_CLASS)
                {
                    builder = Some(unit.handle);
                    break;
                }
            }
        }
        let builder = builder.expect("scripted builder birth");
        assert_eq!(
            identity_metadata(&engine.world, builder).unwrap().owner_id,
            Some(owner)
        );
        let loaded = engine.loaded.as_ref().unwrap();
        assert!(loaded.clan_ai_events.stops().next().is_none());
        assert!(loaded
            .behavior_ore_states
            .iter()
            .any(|ore| ore.handle == builder
                && ore.class_flags == crate::construction::BUILDER_CLASS));
        assert!(loaded
            .mission_events
            .clan_units(&engine.world, owner, false)
            .any(|unit| unit == builder));
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_two_fortresses_builds_a_scripted_enemy_robot() {
        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed corpus");
        let vfs = std::sync::Arc::new(DirectoryVfs::new(std::path::PathBuf::from(root)));
        let mut engine = create(EngineServices::new(vfs)).unwrap();
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
        let enemy_factory = handle_by_original_id(&engine.world, OriginalObjectId(9)).unwrap();
        let enemy_pose = transform_state(&engine.world, enemy_factory).unwrap();
        let report = super::super::simulate_mission_report(&mut engine, 40_000).unwrap();
        assert!(
            report.first_factory_completion_ms.contains_key(&1),
            "{report:?}"
        );
        assert!(!report.first_factory_completion_ms.contains_key(&0));
        assert!(report.script_factory_rejections.is_empty(), "{report:?}");
        let cockpit = super::super::starting_cockpit(&engine).unwrap();
        let factory = handle_by_original_id(&engine.world, OriginalObjectId(5)).unwrap();
        let production = engine.production.as_ref().unwrap();
        let (owner, _, transport) = production
            .ai_schemes
            .iter()
            .find(|(_, class, _)| *class == crate::clan_factory_schemes::TRANSPORT_CLASS)
            .expect("eligible AI transport");
        assert_eq!(*owner, OwnerId(1));
        assert!(transport.as_str().ends_with("/sml_cargo.dat"));
        let transport_index = production
            .schemes
            .iter()
            .position(|path| path == transport)
            .unwrap();
        assert_eq!(clan_factory_schemes(&engine, cockpit).unwrap().len(), 2);
        assert!(
            order_clan_factory_scheme(&mut engine, cockpit, factory, transport_index, 40_000)
                .is_err()
        );
        let duration = order_clan_factory_scheme(&mut engine, cockpit, factory, 0, 40_000).unwrap();
        assert!(duration > 0);
        let production = engine.production.as_ref().unwrap();
        assert_eq!(production.pending.len(), 2);
        assert_ne!(production.pending[0].slot, production.pending[1].slot);
        let cargo_slots = production.reserve_start + transport_index * RESERVE_SLOTS;
        assert!((cargo_slots..cargo_slots + RESERVE_SLOTS).contains(&production.pending[0].slot));
        let mut completed = Vec::new();
        let mut departures = Vec::new();
        for time in (40_016..=40_000 + duration + 2000).step_by(16) {
            if let Some(unit) = step_headless(&mut engine, InputSnapshot::default(), time)
                .unwrap()
                .produced_unit
            {
                let pose = transform_state(&engine.world, unit.handle).unwrap();
                let [x, y, z] = pose.position.map(f32::from_bits);
                let mission = engine.loaded.as_ref().unwrap();
                let (plant, expected_z) = if unit.clan == 1 {
                    (enemy_pose, 205.0637)
                } else {
                    (transform_state(&engine.world, factory).unwrap(), 196.756)
                };
                let [px, py, _] = plant.position.map(f32::from_bits);
                assert_eq!([x, y], [px, py]);
                assert!((z - expected_z).abs() < 0.001, "{z} vs {expected_z}");
                assert!(mission
                    .unit_patrol
                    .as_ref()
                    .unwrap()
                    .is_leaving_factory(unit.handle));
                departures.push((unit.handle, [x, y, z]));
                completed.push((unit.clan, unit.object_index));
            }
        }
        assert!(
            completed.iter().any(|&(clan, _)| clan == 0),
            "{completed:?}"
        );
        assert!(
            completed.iter().any(|&(clan, index)| clan == 1
                && (cargo_slots..cargo_slots + RESERVE_SLOTS).contains(&index)),
            "{completed:?}"
        );
        let transport = completed
            .iter()
            .find_map(|&(clan, index)| {
                (clan == 1 && (cargo_slots..cargo_slots + RESERVE_SLOTS).contains(&index))
                    .then_some(index)
            })
            .unwrap();
        let draft = &engine.loaded.as_ref().unwrap().object_drafts[transport];
        let id = draft.original_id.unwrap();
        let handle = handle_by_original_id(&engine.world, id).unwrap();
        let storage = engine
            .loaded
            .as_ref()
            .unwrap()
            .behavior_ore_states
            .iter()
            .find(|ore| {
                ore.class_flags == 0x8000_0008
                    && identity_metadata(&engine.world, ore.handle)
                        .unwrap()
                        .owner_id
                        == Some(OwnerId(1))
            })
            .unwrap();
        let storage_handle = storage.handle;
        let initial_stock = f32::from_bits(storage.current_ore_bits);
        for time in ((40_000 + duration + 2016)..=175_000).step_by(200) {
            step_headless(&mut engine, InputSnapshot::default(), time).unwrap();
            let loaded = engine.loaded.as_ref().unwrap();
            if loaded.behavior_ore_states.iter().any(|ore| {
                ore.handle == storage_handle && f32::from_bits(ore.current_ore_bits) > initial_stock
            }) {
                assert!(loaded
                    .clan_ai_events
                    .transport_orders()
                    .any(|(clan, unit, _)| clan == 1 && unit == handle.slot));
                assert!(departures.into_iter().any(|(actor, start)| {
                    let Some(patrol) = loaded.unit_patrol.as_ref() else {
                        return false;
                    };
                    transform_state(&engine.world, actor).is_ok_and(|pose| {
                        let [x, y, z] = pose.position.map(f32::from_bits);
                        !patrol.is_leaving_factory(actor) && [x, y] != start[..2] && z < start[2]
                    })
                }));
                return;
            }
        }
        panic!("transport should unload ore into the enemy storage");
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_factory_payment_waits_for_behavior_task_deadline() {
        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed corpus");
        let mut engine = create(EngineServices::new(std::sync::Arc::new(DirectoryVfs::new(
            root,
        ))))
        .unwrap();
        load_gameplay_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/CAMPAIGN/CAMPAIGN.00/Mission.03/data.tma".into(),
                mode: MissionMode::SingleMission,
            },
            MissionStartupInput {
                queue_time_ms: 0,
                research_creation_gate: false,
            },
            None,
        )
        .unwrap();
        let plant = handle_by_original_id(&engine.world, OriginalObjectId(7)).unwrap();
        order_owned_factory(&mut engine, OwnerId(0), plant, 0, 100).unwrap();
        factory_resources(engine.loaded.as_mut().unwrap(), plant)
            .unwrap()
            .1
            .current_ore_bits = 100.0_f32.to_bits();
        // No Control budget runs in this isolated clock test: set the work multiplier explicitly.
        let table = factory_resources(engine.loaded.as_mut().unwrap(), plant)
            .unwrap()
            .0;
        let MissionControlDeviceTableStatus::Available(devices) = &mut table.status else {
            panic!("factory devices");
        };
        devices
            .iter_mut()
            .find(|device| device.descriptor.tag == 26)
            .unwrap()
            .multiplier_bits = Some(0);
        // Isolate the factory's owner timer from unrelated clan updates.
        let mut rng = 1;
        for now in [100, 174] {
            let production = engine.production.as_mut().unwrap();
            production
                .tick(
                    &mut engine.world,
                    engine.loaded.as_mut().unwrap(),
                    now,
                    &mut rng,
                )
                .unwrap();
        }
        let production = engine.production.as_ref().unwrap();
        let paid = production.pending.front().unwrap().paid.as_ref().unwrap();
        assert_eq!(
            paid.collected.ore, 0.0,
            "no resource pull before the deadline"
        );
        assert_eq!(paid.last_ms, 100);
        assert_eq!(rng, 2_745_024);
        let production = engine.production.as_mut().unwrap();
        production
            .tick(
                &mut engine.world,
                engine.loaded.as_mut().unwrap(),
                175,
                &mut rng,
            )
            .unwrap();
        let paid = production.pending.front().unwrap().paid.as_ref().unwrap();
        assert_eq!(paid.last_ms, 175);
        assert_eq!(paid.collected.ore, 0.07);
        assert_eq!(
            production.clock_ticks().collect::<Vec<_>>(),
            [(0, plant.slot, 2)]
        );
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_field_camp_runs_paid_factory_order() {
        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed corpus");
        let vfs = std::sync::Arc::new(DirectoryVfs::new(std::path::PathBuf::from(root)));
        let mut engine = create(EngineServices::new(vfs)).unwrap();
        load_gameplay_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/CAMPAIGN/CAMPAIGN.00/Mission.03/data.tma".into(),
                mode: MissionMode::SingleMission,
            },
            MissionStartupInput {
                queue_time_ms: 0,
                research_creation_gate: false,
            },
            None,
        )
        .unwrap();
        let cockpit = starting_cockpit(&engine).unwrap();
        let mut pose = transform_state(&engine.world, cockpit.handle).unwrap();
        let plant = handle_by_original_id(&engine.world, OriginalObjectId(7)).unwrap();
        let plant_pose = transform_state(&engine.world, plant).unwrap();
        let transform = fparkan_render::LegacyIron3dEulerTransform {
            translation: plant_pose.position.map(f32::from_bits),
            orientation_radians: plant_pose.orientation.map(f32::from_bits),
        };
        pose.position = transform
            .try_transform_scaled_point([0.08, -48.64, -6.63], plant_pose.scale.map(f32::from_bits))
            .unwrap()
            .map(f32::to_bits);
        set_transform(&mut engine.world, cockpit.handle, pose).unwrap();
        assert_eq!(
            order_factory_robot(&mut engine, cockpit, 100).unwrap(),
            PAID_MINIMUM_MS
        );
        let order = engine.production.as_ref().unwrap().pending.front().unwrap();
        let cost = &order.paid.as_ref().unwrap().cost;
        assert!((cost.ore - 25.12).abs() < 0.001);
        assert!((cost.power - 72.7).abs() < 0.001);
        // Debug fixture: empty the factory's local energy reserve as well as leaving it without a generator.
        crate::set_control_device_table_multiplier(
            factory_resources(engine.loaded.as_mut().unwrap(), plant)
                .unwrap()
                .0,
            0.0,
        )
        .unwrap();
        for now in (100..=6100).step_by(50) {
            assert!(step_headless(&mut engine, InputSnapshot::default(), now)
                .unwrap()
                .produced_unit
                .is_none());
        }
        let loaded = engine.loaded.as_ref().unwrap();
        assert!(loaded
            .control_resources
            .ticks()
            .any(|(unit, count)| unit == plant.slot && count > 0));
        assert!(!loaded
            .control_resources
            .stops()
            .any(|(unit, _, _)| unit == plant.slot));
        let table = loaded
            .control_device_tables
            .iter()
            .find(|table| table.handle == plant)
            .unwrap();
        assert!(
            factory_rate(table).unwrap() < 5.0,
            "unpowered production device must lose its full work rate"
        );
        let factory_position = pose.position;
        pose.position = [1288.99_f32, 807.4, 74.08].map(f32::to_bits);
        set_transform(&mut engine.world, cockpit.handle, pose).unwrap();
        assert_eq!(
            capture_neutral_building(&mut engine, cockpit).unwrap(),
            Some(OriginalObjectId(0))
        );
        pose.position = [1026.08_f32, 902.706, 81.24426].map(f32::to_bits);
        set_transform(&mut engine.world, cockpit.handle, pose).unwrap();
        crate::deploy_mine(&mut engine, cockpit).unwrap();
        for now in (6150..=30_100).step_by(50) {
            let frame = step_headless(&mut engine, InputSnapshot::default(), now).unwrap();
            assert!(
                frame.production_failure.is_none(),
                "{:?}",
                frame.production_failure
            );
            assert!(frame.produced_unit.is_none());
        }
        pose.position = [659.4_f32, 1050.6, 80.04].map(f32::to_bits);
        set_transform(&mut engine.world, cockpit.handle, pose).unwrap();
        assert_eq!(
            capture_neutral_building(&mut engine, cockpit).unwrap(),
            Some(OriginalObjectId(9))
        );
        pose.position = factory_position;
        set_transform(&mut engine.world, cockpit.handle, pose).unwrap();
        let mut produced = 0;
        let mut dispatched_at = None;
        let enemy = handle_by_original_id(&engine.world, OriginalObjectId(2)).unwrap();
        let enemy_start = transform_state(&engine.world, enemy).unwrap().position;
        for now in (30_150..=190_100).step_by(50) {
            let frame = step_headless(&mut engine, InputSnapshot::default(), now).unwrap();
            assert!(
                frame
                    .mission_event
                    .as_ref()
                    .and_then(|event| event.stop.as_ref())
                    .is_none(),
                "{:?}",
                frame.mission_event
            );
            assert!(
                frame.production_failure.is_none(),
                "{:?}",
                frame.production_failure
            );
            if let Some(unit) = frame.produced_unit {
                println!("test paid order completed at {now} ms");
                assert_eq!(unit.clan, 0);
                produced += 1;
                if produced < 4 {
                    order_factory_robot(&mut engine, cockpit, now).unwrap();
                }
            }
            if frame.mission_event.is_some_and(|event| {
                if event
                    .notices
                    .contains(&crate::MissionNotice::ObjectiveComplete(3))
                {
                    assert_eq!(event.patrol_orders.len(), 3);
                    assert_eq!(event.patrol_orders[0].1, [1124, 783]);
                    assert_eq!(event.patrol_orders[1].1, [606, 993]);
                    return true;
                }
                false
            }) {
                dispatched_at = Some(now);
                break;
            }
        }
        assert_eq!(produced, 4);
        let dispatched_at = dispatched_at.expect("factory objective must follow the fourth robot");
        for now in (dispatched_at + 50..=dispatched_at + 5_000).step_by(50) {
            step_headless(&mut engine, InputSnapshot::default(), now).unwrap();
        }
        assert_ne!(
            transform_state(&engine.world, enemy).unwrap().position,
            enemy_start
        );
        let enemies =
            [2, 3, 4].map(|id| handle_by_original_id(&engine.world, OriginalObjectId(id)).unwrap());
        for handle in enemies {
            assert_eq!(
                crate::apply_projectile_hit(&mut engine, handle, f32::MAX).unwrap(),
                Some((0.0, true))
            );
        }
        let tick = step_headless(&mut engine, InputSnapshot::default(), dispatched_at + 8_000)
            .unwrap()
            .mission_event
            .unwrap();
        assert!(
            tick.notices
                .contains(&crate::MissionNotice::ObjectiveComplete(4)),
            "{tick:?}"
        );
        assert!(
            tick.notices.contains(&crate::MissionNotice::Completed),
            "{tick:?}"
        );
    }
}
