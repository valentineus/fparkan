use super::{
    autodemo::{
        reinforcement_position, spawn_prepared_unit, SpawnedUnit, MOBILE_UNIT, PLANT_CLASS,
    },
    behavior_ore_index_for_handle,
    mission_events::on_communicator,
    normalize_engine_path, query_behavior_profile_ore_rate, read_vfs,
    set_control_device_table_selector, BehaviorOreState, ClanBody, Engine, EngineError,
    LoadedMissionState, MissionControlDeviceTableState, MissionControlDeviceTableStatus,
    MissionObjectDraft, StartingCockpit, World,
};
use fparkan_assets::ResearchTrfDocument;
use fparkan_prototype::{unit_component_tree, UnitComponentRecord};
use fparkan_world::{
    handle_by_original_id, identity_metadata, transform_state, ObjectHandle, OwnerId,
};

// shortcut: GPU geometry is prepared for a bounded number of live builds.
// Lift the cap when the renderer can instantiate new roots during a mission.
pub(super) const RESERVE_SLOTS: usize = 8;
const PAID_MINIMUM_MS: u32 = 5_000;
const RESOURCE_EPSILON: f32 = 0.01;

pub(super) struct Production {
    reserve_start: usize,
    controls: Vec<MissionControlDeviceTableState>,
    plants: Vec<(usize, u32)>,
    pending: Option<Order>,
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
}

#[derive(Default)]
struct Cost {
    ore: f32,
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
    pub(super) fn new(
        reserve_start: usize,
        controls: Vec<MissionControlDeviceTableState>,
        drafts: &[MissionObjectDraft],
    ) -> Result<Self, EngineError> {
        if controls.len() != RESERVE_SLOTS
            || drafts.len().checked_sub(reserve_start) != Some(controls.len())
        {
            return Err(EngineError::Production(
                "missing prepared build scheme slots".into(),
            ));
        }
        if !drafts[reserve_start]
            .unit_dat_header_flags
            .is_some_and(|class| class & MOBILE_UNIT != 0 && class != super::HERO_UNIT_CLASS)
        {
            return Err(EngineError::Production(
                "build scheme must be a mobile Unit DAT".into(),
            ));
        }
        let plants = drafts[..reserve_start]
            .iter()
            .enumerate()
            .filter(|(_, draft)| draft.unit_dat_header_flags == Some(PLANT_CLASS))
            .map(|(index, draft)| {
                let count = draft
                    .properties
                    .iter()
                    .find(|property| property.name_raw == b"FreeBotNum")
                    .map_or(1, |property| property.raw_value[1] as i32);
                (index, count.max(0) as u32)
            })
            .collect();
        Ok(Self {
            reserve_start,
            controls,
            plants,
            pending: None,
            rng: 1,
        })
    }

    pub(super) fn tick(
        &mut self,
        world: &mut World,
        mission: &mut LoadedMissionState,
        now_ms: u32,
    ) -> Result<Option<SpawnedUnit>, EngineError> {
        let Some(mut order) = self.pending.take() else {
            return Ok(None);
        };
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
            // shortcut: gate the factory rate on clan power until Control
            // propagates the delivered power to the production device.
            let powered = mission.script_bundles.iter().any(|bundle| {
                bundle.clan_index == usize::from(order.owner.0)
                    && bundle
                        .behavior_warm_start
                        .is_some_and(|state| f32::from_bits(state.cleared_words[0]) > 0.0)
            });
            let (table, ore) = factory_resources(mission, order.plant)?;
            let rate = if powered { factory_rate(table)? } else { 0.0 };
            if paid.advance(now_ms, rate, ore)
                && paid.collected.power + RESOURCE_EPSILON >= paid.cost.power
            {
                set_factory_power(table, 0.0)?;
            }
            ready &= paid.ready();
        }
        if !ready {
            self.pending = Some(order);
            return Ok(None);
        }
        let pose = transform_state(world, order.plant)?;
        // shortcut: reuse the outdoor spawn point until the factory exit
        // device and its clear-space check are available.
        let position = reinforcement_position(
            pose.position.map(f32::from_bits),
            pose.orientation.map(f32::from_bits),
            pose.scale.map(f32::from_bits),
            order.plant_index,
            &mission.mission_assets,
            &mission.terrain,
            &mut self.rng,
        )?;
        let handle = spawn_prepared_unit(
            world,
            mission,
            order.slot,
            order.owner.0,
            position,
            self.controls[order.slot - self.reserve_start].clone(),
        )?;
        if order.paid.is_none() {
            let (_, count) = self
                .plants
                .iter_mut()
                .find(|(index, _)| *index == order.plant_index)
                .expect("free order belongs to a prepared factory");
            *count -= 1;
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
    let (Some(mission), Some(production)) = (&mut engine.loaded, &mut engine.production) else {
        return Err(EngineError::Production(
            "launch with --build-scheme to choose a robot".into(),
        ));
    };
    if production.pending.is_some() {
        return Err(EngineError::Production(
            "factory order already in progress".into(),
        ));
    }
    let slot = (production.reserve_start..production.reserve_start + production.controls.len())
        .find(|&index| {
            mission.object_drafts[index]
                .original_id
                .is_some_and(|id| handle_by_original_id(&engine.world, id).is_none())
        })
        .ok_or_else(|| {
            EngineError::Production("all prepared factory robot slots are occupied".into())
        })?;
    let draft = &mission.object_drafts[slot];
    let player = identity_metadata(&engine.world, cockpit.handle)?;
    let owner = player
        .owner_id
        .ok_or_else(|| EngineError::Production("player clan is unavailable".into()))?;
    let position = transform_state(&engine.world, cockpit.handle)?
        .position
        .map(f32::from_bits);
    let (plant_index, plant, is_free) = production
        .plants
        .iter()
        .find_map(|(index, slots)| {
            let plant =
                handle_by_original_id(&engine.world, mission.object_drafts[*index].original_id?)?;
            (identity_metadata(&engine.world, plant).ok()?.owner_id == Some(owner)
                && on_communicator(
                    &mission.mission_assets,
                    *index,
                    PLANT_CLASS,
                    transform_state(&engine.world, plant).ok()?,
                    position,
                ))
            .then_some((*index, plant, *slots != 0))
        })
        .ok_or_else(|| {
            EngineError::Production("stand on a friendly factory communicator".into())
        })?;
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
            let clan = mission
                .mission
                .clans
                .get(usize::from(owner.0))
                .ok_or_else(|| EngineError::Production("factory clan is unavailable".into()))?;
            let raw = match &clan.body {
                ClanBody::Standard {
                    second_resource, ..
                }
                | ClanBody::Spatial {
                    second_resource, ..
                } => &second_resource.path.raw,
            };
            let path = raw.split(|&byte| byte == 0).next().unwrap_or_default();
            let path = std::str::from_utf8(path).map_err(production_error)?;
            if path.is_empty() {
                return Err(EngineError::Production(
                    "factory clan has no price table".into(),
                ));
            }
            let path = normalize_engine_path("factory price table", path)?;
            let vfs = engine
                .services
                .vfs
                .as_ref()
                .ok_or(EngineError::MissingVfs)?;
            document =
                ResearchTrfDocument::decode(read_vfs(vfs, &path)?).map_err(production_error)?;
            &document
        };
        let mut cost = scheme_cost(&draft.unit_components, research)?;
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
    production.pending = Some(Order {
        slot,
        plant,
        plant_index,
        owner,
        start_ms: now_ms,
        duration_ms,
        paid,
    });
    Ok(duration_ms)
}

fn production_error(error: impl std::fmt::Display) -> EngineError {
    EngineError::Production(error.to_string())
}

fn scheme_cost(
    records: &[UnitComponentRecord],
    research: &ResearchTrfDocument,
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
        if research.flag_byte(row).unwrap_or_default() & 0b110 != 0b110 {
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
            Some("UNITS/UNITS/PREBLD/tut3_p1.dat"),
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
        let order = engine
            .production
            .as_ref()
            .unwrap()
            .pending
            .as_ref()
            .unwrap();
        let cost = &order.paid.as_ref().unwrap().cost;
        assert!((cost.ore - 25.12).abs() < 0.001);
        assert!((cost.power - 72.7).abs() < 0.001);
        for now in (100..=6100).step_by(50) {
            assert!(step_headless(&mut engine, InputSnapshot::default(), now)
                .unwrap()
                .produced_unit
                .is_none());
        }
        let factory_position = pose.position;
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
        let mut complete = false;
        for now in (30_150..=190_100).step_by(50) {
            let frame = step_headless(&mut engine, InputSnapshot::default(), now).unwrap();
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
                event
                    .notices
                    .contains(&crate::MissionNotice::ObjectiveComplete(3))
            }) {
                complete = true;
                break;
            }
        }
        assert_eq!(produced, 4);
        assert!(complete, "factory objective must follow the fourth robot");
    }
}
