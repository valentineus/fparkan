use crate::unit_spawn::spawn_prepared_unit;
use crate::{LoadedMissionState, MissionScriptBundle};
use fparkan_path::ResourceName;
use fparkan_script::ScriptDispatchSelector;
use fparkan_terrain::SurfaceQuery;
use fparkan_world::{
    handle_by_original_id, identity_metadata, transform_state, ObjectHandle, OwnerId, World,
};

pub(super) const BUILDER_CLASS: u32 = 0x0100_4000;
const SLOTS_PER_CLASS: usize = 3;
const WAIT_MS: u32 = 1_000;
const ARRIVAL_RADIUS: f32 = 1.0;

pub(super) fn reserve_schemes(
    categories: &[crate::BuildCategory],
    bundles: &[MissionScriptBundle],
) -> Vec<ResourceName> {
    let enabled = bundles.iter().any(|bundle| {
        bundle.package.events.iter().any(|event| {
            event.name_raw == b"PBM_BUILDING_NEEDED_Start\0"
                && event.instructions.iter().any(|instruction| {
                    instruction.dispatch_selector() == ScriptDispatchSelector::Handler(18)
                })
        })
    });
    // shortcut: prepare a bounded number of buildings until runtime GPU instancing exists.
    categories
        .iter()
        .filter(move |category| enabled && supports_class(category.mask))
        .filter_map(|category| category.unit_paths.first())
        .flat_map(|path| (0..SLOTS_PER_CLASS).map(move |_| crate::resource_name(path.as_bytes())))
        .collect()
}

pub(super) fn supports_class(class: u32) -> bool {
    matches!(
        class,
        0x8000_0008 | 0x8000_0010 | 0x8000_0400 | 0x8010_0000 | 0x8020_0000
    )
}

#[derive(Default)]
pub(super) struct Construction {
    first_slot: usize,
    controls: Vec<crate::MissionControlDeviceTableState>,
    tasks: Vec<Task>,
    pub(super) supply_orders: std::collections::BTreeMap<(u16, u32, u32), u64>,
}

struct Task {
    builder: ObjectHandle,
    owner: OwnerId,
    slot: usize,
    class: u32,
    position: [f32; 3],
    cost: f32,
    arrived_at: Option<u32>,
    supply: Option<crate::builder_supply::BuilderSupply>,
}

impl Construction {
    pub(super) fn new(
        first_slot: usize,
        controls: Vec<crate::MissionControlDeviceTableState>,
    ) -> Self {
        Self {
            first_slot,
            controls,
            tasks: Vec::new(),
            supply_orders: Default::default(),
        }
    }

    pub(super) fn active(&self, builder: ObjectHandle) -> bool {
        self.tasks.iter().any(|task| task.builder == builder)
    }

    pub(super) fn pending_count(&self, owner: OwnerId, class: u32) -> usize {
        self.tasks
            .iter()
            .filter(|task| task.owner == owner && task.class == class)
            .count()
    }

    fn available_slot(
        &self,
        world: &World,
        drafts: &[crate::MissionObjectDraft],
        class: u32,
    ) -> Option<usize> {
        (self.first_slot..self.first_slot + self.controls.len()).find(|&index| {
            let draft = &drafts[index];
            draft.unit_dat_header_flags == Some(class)
                && draft
                    .original_id
                    .is_some_and(|id| handle_by_original_id(world, id).is_none())
                && !self.tasks.iter().any(|task| task.slot == index)
        })
    }

    fn start(
        &mut self,
        builder: ObjectHandle,
        owner: OwnerId,
        slot: usize,
        class: u32,
        position: [f32; 3],
        cost: f32,
        supply: Option<crate::builder_supply::BuilderSupply>,
    ) {
        if let Some(supply) = &supply {
            *self
                .supply_orders
                .entry((owner.0, builder.slot, supply.source.slot))
                .or_default() += 1;
            eprintln!(
                "sim_coverage script_builder_supply_approx=1 clan={} builder={} source={}",
                owner.0, builder.slot, supply.source.slot
            );
        }
        self.tasks.push(Task {
            builder,
            owner,
            slot,
            class,
            position,
            cost,
            arrived_at: None,
            supply,
        });
    }
}

pub(super) fn order(
    construction: &mut Construction,
    patrol: &mut Option<crate::unit_patrol::UnitPatrol>,
    world: &World,
    terrain: &fparkan_terrain::TerrainWorld,
    mission: &crate::MissionDocument,
    drafts: &[crate::MissionObjectDraft],
    mine_sites: &[crate::mining::MineSite],
    ores: &[crate::BehaviorOreState],
    research: &crate::research::FreeResearch,
    vfs: Option<&dyn fparkan_vfs::Vfs>,
    builder: Option<ObjectHandle>,
    owner: OwnerId,
    class: u32,
    xy: [u32; 2],
) -> Result<(), String> {
    let builder = builder.ok_or("builder is unavailable")?;
    if construction.active(builder) {
        return Err("builder is busy".into());
    }
    if patrol
        .as_ref()
        .is_some_and(|patrol| patrol.is_shut_down(builder))
    {
        return Err("builder is shut down".into());
    }
    let cargo = ores
        .iter()
        .find(|ore| ore.handle == builder && ore.class_flags == BUILDER_CLASS)
        .ok_or("unit has no builder ore state")?;
    let slot = if class == crate::mining::MINE_CLASS {
        mine_sites.iter().find_map(|site| {
            let point = [site.position[0], site.position[1]].map(|axis| axis.trunc() as i64 as u32);
            let id = site.control.as_ref()?.original_id?;
            (point == xy && site.live_mine(world).is_none()).then_some(id.0 as usize)
        })
    } else {
        construction.available_slot(world, drafts, class)
    }
    .filter(|&slot| !construction.tasks.iter().any(|task| task.slot == slot))
    .ok_or("no free prepared building slot")?;
    let table = crate::research::load_technology_table(
        vfs.ok_or("no clan technology provider")?,
        mission,
        owner,
    )
    .map_err(|err| err.to_string())?;
    let cost = crate::production::scheme_cost(&drafts[slot].unit_components, &table, |row| {
        research.learned.contains(&(owner, row))
    })
    .map_err(|err| err.to_string())?
    .ore;
    let stock = f32::from_bits(cargo.current_ore_bits);
    if !stock.is_finite() {
        return Err("builder has non-finite ore".into());
    }
    let supply =
        crate::builder_supply::BuilderSupply::plan(world, ores, builder, owner, class, stock)?;
    let from = transform_state(world, builder)
        .map_err(|err| err.to_string())?
        .position
        .map(f32::from_bits);
    let position = destination(terrain, from, xy).ok_or("no route to the building site")?;
    if supply.is_none() {
        patrol
            .get_or_insert_with(crate::unit_patrol::UnitPatrol::scripted)
            .move_to(world, builder, [position[0], position[1]], false);
    }
    construction.start(builder, owner, slot, class, position, cost, supply);
    Ok(())
}

pub(super) fn tick(
    mission: &mut LoadedMissionState,
    world: &mut World,
    mut production: Option<&mut crate::production::Production>,
    now_ms: u32,
) {
    let mut index = 0;
    while let Some(task) = mission.construction.tasks.get_mut(index) {
        let owned =
            identity_metadata(world, task.builder).is_ok_and(|id| id.owner_id == Some(task.owner));
        let running = !mission
            .unit_patrol
            .as_ref()
            .is_some_and(|patrol| patrol.is_shut_down(task.builder));
        let mut finish = false;
        if let Some(pose) = transform_state(world, task.builder)
            .ok()
            .filter(|_| owned && running)
        {
            if let Some(supply) = &mut task.supply {
                match supply.tick(
                    world,
                    &mut mission.behavior_ore_states,
                    &mut mission.unit_patrol,
                    task.builder,
                    task.owner,
                    task.cost,
                    now_ms,
                ) {
                    Ok(ready) => {
                        if ready {
                            task.supply = None;
                            mission
                                .unit_patrol
                                .get_or_insert_with(crate::unit_patrol::UnitPatrol::scripted)
                                .move_to(
                                    world,
                                    task.builder,
                                    [task.position[0], task.position[1]],
                                    false,
                                );
                            eprintln!(
                                "script_builder_supplied clan={} builder={} queue_time_ms={now_ms}",
                                task.owner.0, task.builder.slot
                            );
                        }
                        index += 1;
                        continue;
                    }
                    Err(reason) => eprintln!(
                        "script_builder_supply_failed clan={} builder={} reason={reason}",
                        task.owner.0, task.builder.slot
                    ),
                }
            } else {
                let [x, y, _] = pose.position.map(f32::from_bits);
                let [a, b, _] = task.position;
                if (x - a).hypot(y - b) > ARRIVAL_RADIUS {
                    task.arrived_at = None;
                    if mission
                        .unit_patrol
                        .as_ref()
                        .is_some_and(|patrol| patrol.is_moving_to_destination(task.builder))
                    {
                        index += 1;
                        continue;
                    }
                } else {
                    let arrived_at = *task.arrived_at.get_or_insert(now_ms);
                    if now_ms.wrapping_sub(arrived_at) < WAIT_MS {
                        index += 1;
                        continue;
                    }
                    finish = true;
                }
            }
        }
        let task = mission.construction.tasks.remove(index);
        if let Some(patrol) = &mut mission.unit_patrol {
            patrol.clear_move(task.builder);
        }
        if finish {
            if let Err(error) = complete(mission, world, &task, production.as_deref_mut()) {
                eprintln!(
                    "script_building_construction_failed clan={} builder={} reason={error}",
                    task.owner.0, task.builder.slot
                );
            }
        }
    }
}

fn complete(
    mission: &mut LoadedMissionState,
    world: &mut World,
    task: &Task,
    production: Option<&mut crate::production::Production>,
) -> Result<(), crate::EngineError> {
    let index = crate::behavior_ore_index_for_handle(&mission.behavior_ore_states, task.builder)
        .map_err(crate::EngineError::Production)?;
    let stock = f32::from_bits(mission.behavior_ore_states[index].current_ore_bits);
    if !stock.is_finite() {
        return Err(crate::EngineError::Production(
            "builder has non-finite ore".into(),
        ));
    }
    let [x, y, _] = task.position;
    if !crate::construction_site::building_centers(world, &mission.object_drafts)
        .all(|other| crate::construction_site::separated([x, y], other))
    {
        return Err(crate::EngineError::Production(
            "building site is occupied".into(),
        ));
    }
    let draft = &mission.object_drafts[task.slot];
    let id = draft.original_id.expect("prepared building has an ID");
    let class = draft
        .unit_dat_header_flags
        .expect("prepared building has a class");
    let profile = mission
        .behavior_profiles
        .iter()
        .position(|profile| profile.original_id == id);
    let control = if class == crate::mining::MINE_CLASS {
        mission
            .mine_sites
            .iter()
            .filter_map(|site| site.control.as_ref())
            .find(|control| control.original_id == Some(id))
            .cloned()
            .ok_or_else(|| crate::EngineError::Production("mine control is unavailable".into()))?
    } else {
        mission.construction.controls[task.slot - mission.construction.first_slot].clone()
    };
    let handle = spawn_prepared_unit(
        world,
        mission,
        task.slot,
        task.owner.0,
        task.position,
        control,
    )?;
    mission.behavior_ore_states[index].current_ore_bits = (stock - task.cost).to_bits();
    mission
        .behavior_ore_states
        .retain(|ore| ore.original_id != id);
    mission
        .behavior_ore_states
        .push(crate::initialize_behavior_ore_state(id, handle, class, &[]));
    if let Some(profile) = profile {
        mission.behavior_profiles[profile].handle = handle;
    }
    if let Some(distributor) = mission.behavior_distributors.get_mut(task.owner.0 as usize) {
        distributor
            .registered_objects
            .retain(|entry| entry.original_id != id);
        crate::add_behavior_registered_object(
            distributor,
            crate::BehaviorRegisteredObject {
                original_id: id,
                handle,
                selector: u32::from(task.owner.0),
                class_flags: class,
                share: 0.0,
                native_tail: [0; 5],
            },
        );
    }
    mission
        .mission_events
        .issue_logic_id(&mission.mission, world, handle);
    if let Some(production) = production {
        production.register_factories(task.slot, &mission.object_drafts[task.slot..=task.slot]);
    }
    eprintln!(
        "script_building_constructed clan={} builder={} building={} class={class:#010x}",
        task.owner.0, task.builder.slot, handle.slot
    );
    Ok(())
}

fn destination(
    terrain: &fparkan_terrain::TerrainWorld,
    from: [f32; 3],
    xy: [u32; 2],
) -> Option<[f32; 3]> {
    let [x, y] = xy.map(|x| x as f32);
    let position = [x, y, terrain.height_at([x, y]).ok()??];
    crate::unit_patrol::route_points(terrain, from, position).ok()??;
    Some(position)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        create, load_gameplay_mission, EngineServices, MissionMode, MissionRequest,
        MissionStartupInput,
    };
    use fparkan_vfs::DirectoryVfs;
    use std::sync::Arc;

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_stock_builder_resupplies_and_finishes_another_building() {
        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").unwrap();
        let mut engine = create(EngineServices::new(Arc::new(DirectoryVfs::new(root)))).unwrap();
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
        let mut supplied = None;
        for now in (0..=900_000).step_by(100) {
            crate::frame(&mut engine, now).unwrap();
            let loaded = engine.loaded.as_ref().unwrap();
            assert!(loaded.clan_ai_events.stops().next().is_none());
            if supplied.is_none() {
                if let Some(task) = loaded
                    .construction
                    .tasks
                    .iter()
                    .find(|task| task.supply.is_some())
                {
                    let source = task.supply.as_ref().unwrap().source;
                    assert_eq!(
                        identity_metadata(&engine.world, source).unwrap().owner_id,
                        Some(task.owner)
                    );
                    let ore = loaded
                        .behavior_ore_states
                        .iter()
                        .find(|ore| ore.handle == task.builder)
                        .unwrap();
                    assert!(f32::from_bits(ore.current_ore_bits) < 100.0);
                    supplied = Some((
                        loaded.object_drafts[task.slot].original_id.unwrap(),
                        task.builder,
                    ));
                }
            }
            if let Some((id, builder)) = supplied {
                if let Some(building) = handle_by_original_id(&engine.world, id) {
                    assert_eq!(
                        identity_metadata(&engine.world, building).unwrap().owner_id,
                        identity_metadata(&engine.world, builder).unwrap().owner_id
                    );
                    assert!(loaded
                        .construction
                        .supply_orders
                        .keys()
                        .any(|&(_, slot, _)| slot == builder.slot));
                    println!(
                        "stock builder {} resupplied and completed {} at {now} ms",
                        builder.slot, building.slot
                    );
                    return;
                }
            }
        }
        panic!("stock builders did not resupply and complete another building");
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_stock_clan_builds_a_working_factory() {
        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").unwrap();
        let vfs = Arc::new(DirectoryVfs::new(root));
        let mut engine = create(EngineServices::new(vfs.clone())).unwrap();
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
        let loaded = engine.loaded.as_ref().unwrap();
        let start = loaded.construction.first_slot;
        let end = start + loaded.construction.controls.len();
        for owner in [OwnerId(1), OwnerId(2)] {
            let table =
                crate::research::load_technology_table(vfs.as_ref(), &loaded.mission, owner)
                    .unwrap();
            for slot in (start..end).step_by(SLOTS_PER_CLASS) {
                let draft = &loaded.object_drafts[slot];
                let cost =
                    crate::production::scheme_cost(&draft.unit_components, &table, |_| false);
                match draft.unit_dat_header_flags.unwrap() {
                    0x8000_0008 => assert_eq!(cost.unwrap().ore, 670.0),
                    0x8000_0010 => assert_eq!(cost.unwrap().ore, 770.0),
                    0x8000_0400 => assert_eq!(cost.unwrap().ore, 560.0),
                    _ => assert!(cost.is_err()),
                }
            }
        }
        for now in (0..=900_000).step_by(100) {
            crate::frame(&mut engine, now).unwrap();
            let loaded = engine.loaded.as_ref().unwrap();
            if let Some(handle) = loaded.object_drafts[start..end].iter().find_map(|draft| {
                (draft.unit_dat_header_flags == Some(0x8000_0010))
                    .then(|| handle_by_original_id(&engine.world, draft.original_id.unwrap()))
                    .flatten()
            }) {
                assert_eq!(
                    identity_metadata(&engine.world, handle).unwrap().owner_id,
                    Some(OwnerId(1))
                );
                assert!(loaded
                    .control_device_tables
                    .iter()
                    .any(|table| table.handle == handle));
                assert!(loaded
                    .behavior_ore_states
                    .iter()
                    .any(|ore| ore.handle == handle && ore.class_flags == 0x8000_0010));
                let result = crate::production::order_script_factory(
                    &mut engine,
                    OwnerId(1),
                    handle,
                    crate::clan_transport_schemes::TRANSPORT_CLASS,
                    crate::clan_transport_schemes::SELECT_FASTEST,
                    now,
                );
                assert_eq!(result.unwrap(), 60_000);
                return;
            }
        }
        panic!("clans did not complete any building from the stock start");
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn tower_order_rejects_unresearched_parts_and_preserves_native_ore_debt() {
        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").unwrap();
        let vfs = Arc::new(DirectoryVfs::new(root));
        let mut engine = create(EngineServices::new(vfs.clone())).unwrap();
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
        let class = 0x8010_0000;
        let mission = engine.loaded.as_mut().unwrap();
        let builder_ore = mission
            .behavior_ore_states
            .iter()
            .position(|ore| {
                ore.class_flags == BUILDER_CLASS
                    && identity_metadata(&engine.world, ore.handle)
                        .is_ok_and(|id| id.owner_id == Some(owner))
            })
            .unwrap();
        let builder = mission.behavior_ore_states[builder_ore].handle;
        let site = crate::construction_site::SiteReservations::default()
            .find(
                &mission.terrain,
                &engine.world,
                &mission.object_drafts,
                &mission.mine_sites,
                mission.mission.clans[1].anchor.map(f32::round_ties_even),
                7_000,
            )
            .unwrap();
        let slot = mission
            .construction
            .available_slot(&engine.world, &mission.object_drafts, class)
            .unwrap();
        let id = mission.object_drafts[slot].original_id.unwrap();
        let order_for = |mission: &mut LoadedMissionState, world: &World| {
            order(
                &mut mission.construction,
                &mut mission.unit_patrol,
                world,
                &mission.terrain,
                &mission.mission,
                &mission.object_drafts,
                &mission.mine_sites,
                &mission.behavior_ore_states,
                &mission.free_research,
                Some(vfs.as_ref()),
                Some(builder),
                owner,
                class,
                site,
            )
        };
        assert!(order_for(mission, &engine.world)
            .unwrap_err()
            .contains("unavailable for construction"));
        let table =
            crate::research::load_technology_table(vfs.as_ref(), &mission.mission, owner).unwrap();
        // Debug setup isolates the native payment rule after the stock research gate.
        for component in &mission.object_drafts[slot].unit_components {
            if let Some(row) = table.find_key(&component.resource_raw).unwrap() {
                mission.free_research.learned.insert((owner, row));
            }
        }
        let stock_before = 500.0_f32;
        mission.behavior_ore_states[builder_ore].current_ore_bits = stock_before.to_bits();
        order_for(mission, &engine.world).unwrap();
        assert!(mission.construction.active(builder));
        assert_eq!(mission.construction.pending_count(owner, class), 1);
        assert_eq!(mission.construction.pending_count(OwnerId(0), class), 0);
        assert!(handle_by_original_id(&engine.world, id).is_none());
        let cost = mission.construction.tasks[0].cost;
        assert_eq!(cost, 1100.0);
        let pose = transform_state(&engine.world, builder).unwrap();
        fparkan_world::set_transform(
            &mut engine.world,
            builder,
            fparkan_world::TransformState {
                position: mission.construction.tasks[0].position.map(f32::to_bits),
                ..pose
            },
        )
        .unwrap();
        tick(mission, &mut engine.world, None, 8_000);
        assert!(handle_by_original_id(&engine.world, id).is_none());
        tick(mission, &mut engine.world, None, 9_000);
        let building = handle_by_original_id(&engine.world, id).unwrap();
        assert_eq!(
            identity_metadata(&engine.world, building).unwrap().owner_id,
            Some(owner)
        );
        assert_eq!(
            f32::from_bits(mission.behavior_ore_states[builder_ore].current_ore_bits),
            stock_before - cost
        );
        assert!(!mission.construction.active(builder));
        assert_eq!(mission.construction.pending_count(owner, class), 0);
    }
}
