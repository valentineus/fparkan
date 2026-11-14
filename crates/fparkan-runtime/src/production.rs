use super::{
    autodemo::{
        reinforcement_position, spawn_prepared_unit, SpawnedUnit, MOBILE_UNIT, PLANT_CLASS,
    },
    mission_events::on_communicator,
    Engine, EngineError, LoadedMissionState, MissionControlDeviceTableState, MissionObjectDraft,
    StartingCockpit, World,
};
use fparkan_world::{
    handle_by_original_id, identity_metadata, transform_state, ObjectHandle, OwnerId,
};

pub(super) struct Production {
    reserve: usize,
    control: MissionControlDeviceTableState,
    free_units: Vec<(usize, u32)>,
    pending: Option<Order>,
    rng: u32,
}

struct Order {
    plant: ObjectHandle,
    plant_index: usize,
    owner: OwnerId,
    start_ms: u32,
    duration_ms: u32,
}

impl Production {
    pub(super) fn new(
        reserve: usize,
        control: MissionControlDeviceTableState,
        drafts: &[MissionObjectDraft],
    ) -> Result<Self, EngineError> {
        if !drafts[reserve]
            .unit_dat_header_flags
            .is_some_and(|class| class & MOBILE_UNIT != 0 && class != super::HERO_UNIT_CLASS)
        {
            return Err(EngineError::Production(
                "build scheme must be a mobile Unit DAT".into(),
            ));
        }
        let free_units = drafts[..reserve]
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
            reserve,
            control,
            free_units,
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
        let Some(order) = &self.pending else {
            return Ok(None);
        };
        if !identity_metadata(world, order.plant)
            .is_ok_and(|identity| identity.owner_id == Some(order.owner))
        {
            self.pending = None;
            return Ok(None);
        }
        if now_ms.wrapping_sub(order.start_ms) < order.duration_ms {
            return Ok(None);
        }
        let order = self.pending.take().expect("due construction order");
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
            self.reserve,
            order.owner.0,
            position,
            self.control.clone(),
        )?;
        if let Some((_, count)) = self
            .free_units
            .iter_mut()
            .find(|(index, _)| *index == order.plant_index)
        {
            *count -= 1;
        }
        Ok(Some(SpawnedUnit {
            object_index: self.reserve,
            handle,
            clan: order.owner.0,
        }))
    }
}

/// Queues the selected free robot scheme at a friendly factory communicator.
/// Returns its construction time in milliseconds on success.
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
    let (Some(mission), Some(production)) = (&engine.loaded, &mut engine.production) else {
        return Err(EngineError::Production(
            "launch with --build-scheme to choose a robot".into(),
        ));
    };
    if production.pending.is_some() {
        return Err(EngineError::Production(
            "factory order already in progress".into(),
        ));
    }
    let draft = &mission.object_drafts[production.reserve];
    if draft
        .original_id
        .is_some_and(|id| handle_by_original_id(&engine.world, id).is_some())
    {
        return Err(EngineError::Production(
            "the previous robot is still active".into(),
        ));
    }
    let player = identity_metadata(&engine.world, cockpit.handle)?;
    let owner = player
        .owner_id
        .ok_or_else(|| EngineError::Production("player clan is unavailable".into()))?;
    let position = transform_state(&engine.world, cockpit.handle)?
        .position
        .map(f32::from_bits);
    let (plant_index, plant) = production
        .free_units
        .iter()
        .find_map(|(index, slots)| {
            let plant =
                handle_by_original_id(&engine.world, mission.object_drafts[*index].original_id?)?;
            (*slots != 0
                && identity_metadata(&engine.world, plant).ok()?.owner_id == Some(owner)
                && on_communicator(
                    &mission.mission_assets,
                    *index,
                    transform_state(&engine.world, plant).ok()?,
                    position,
                ))
            .then_some((*index, plant))
        })
        .ok_or_else(|| {
            EngineError::Production(
                "stand on a friendly factory communicator with free capacity".into(),
            )
        })?;
    let plant_draft = &mission.object_drafts[plant_index];
    let plant_key = &plant_draft.unit_components[0].resource_raw;
    let robot_key = &draft.unit_components[0].resource_raw;
    let duration_ms = free_duration_ms(plant_key, robot_key).ok_or_else(|| {
        EngineError::Production("scheme size is unsupported by this factory".into())
    })?;
    production.pending = Some(Order {
        plant,
        plant_index,
        owner,
        start_ms: now_ms,
        duration_ms,
    });
    Ok(duration_ms)
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
