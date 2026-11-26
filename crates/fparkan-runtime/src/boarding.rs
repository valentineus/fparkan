use super::{
    component_cockpit, control_device_provider_value,
    projectile::{projectile_terrain_hit, projectile_unit_hit, unit_sphere},
    starting_cockpit, Engine, EngineError, MissionControlDeviceTableState,
    MissionControlDeviceTableStatus, StartingCockpit, SurfaceQuery, TransformState,
};
use fparkan_world::{
    handle_by_original_id, identity_metadata, set_transform, transform_state, ObjectHandle,
};

pub(super) const WARBOT_CLASS: u32 = 0x0100_8000;

#[derive(Clone, Copy)]
pub(super) struct Boarding {
    pub(super) cockpit: StartingCockpit,
    pub(super) hero: StartingCockpit,
    pub(super) can_fly: bool,
    pub(super) ground: Option<crate::ground_support::GroundSupport>,
    pub(super) flight_velocity: [f32; 3],
}

/// The vehicle cockpit while the player is aboard.
#[must_use]
pub fn player_vehicle(engine: &Engine) -> Option<StartingCockpit> {
    Some(engine.loaded.as_ref()?.boarding.as_ref()?.cockpit)
}

/// Selects the friendly warbot under a line of sight supplied by the camera.
pub fn select_boarding_target(
    engine: &Engine,
    hero: StartingCockpit,
    segment: [[f32; 3]; 2],
) -> Option<ObjectHandle> {
    let target = projectile_unit_hit(engine, hero.handle, segment)?;
    let owner = identity_metadata(&engine.world, hero.handle)
        .ok()?
        .owner_id?;
    let mission = engine.loaded.as_ref()?;
    // shortcut: use the cockpit sight ray until the original selection ranking is recovered.
    if projectile_terrain_hit(&mission.terrain, segment)
        .ok()?
        .is_some_and(|ground| ground <= target.fraction)
    {
        return None;
    }
    let warbot = mission.object_drafts.iter().any(|draft| {
        draft.unit_dat_header_flags == Some(WARBOT_CLASS)
            && draft
                .original_id
                .and_then(|id| handle_by_original_id(&engine.world, id))
                == Some(target.handle)
    });
    (warbot
        && identity_metadata(&engine.world, target.handle)
            .ok()?
            .owner_id
            == Some(owner))
    .then_some(target.handle)
}

/// Boards the selected compatible allied warbot within 20 units in XY.
///
/// # Errors
///
/// Returns an error if the hero's world handle is stale.
pub fn board_friendly_warbot(
    engine: &mut Engine,
    target: Option<ObjectHandle>,
) -> Result<Option<StartingCockpit>, EngineError> {
    let Some(target) = target else {
        return Ok(None);
    };
    let Some(hero) = starting_cockpit(engine) else {
        return Ok(None);
    };
    let Some(mission) = &engine.loaded else {
        return Ok(None);
    };
    if mission.boarding.is_some() {
        return Ok(None);
    }
    let Some(owner) = identity_metadata(&engine.world, hero.handle)?.owner_id else {
        return Ok(None);
    };
    let hero_pose = transform_state(&engine.world, hero.handle)?;
    let [hero_x, hero_y, _] = hero_pose.position.map(f32::from_bits);
    let candidate = mission
        .object_drafts
        .iter()
        .enumerate()
        .filter_map(|(index, draft)| {
            if draft.unit_dat_header_flags != Some(WARBOT_CLASS) {
                return None;
            }
            let handle = handle_by_original_id(&engine.world, draft.original_id?)?;
            if handle != target
                || identity_metadata(&engine.world, handle).ok()?.owner_id != Some(owner)
            {
                return None;
            }
            let table = mission
                .control_device_tables
                .iter()
                .find(|table| table.handle == handle)?;
            if !has_working_pilot_control(table) {
                return None;
            }
            let state = transform_state(&engine.world, handle).ok()?;
            let [x, y, _] = state.position.map(f32::from_bits);
            if !within_boarding_range([hero_x, hero_y], [x, y]) {
                return None;
            }
            Some((component_cockpit(mission, index, handle)?, state.position))
        })
        .next();
    let Some((cockpit, position)) = candidate else {
        return Ok(None);
    };
    let can_fly = crate::player_motion::warbot_can_fly(engine, cockpit.object_index);
    let ground = (!can_fly)
        .then(|| {
            crate::ground_support::GroundSupport::prepare(
                &mission.mission_assets,
                cockpit.object_index,
            )
        })
        .flatten();
    set_transform(
        &mut engine.world,
        hero.handle,
        TransformState {
            position,
            ..hero_pose
        },
    )?;
    if let Some(mission) = &mut engine.loaded {
        mission.boarding = Some(Boarding {
            cockpit,
            hero,
            can_fly,
            ground,
            flight_velocity: [0.0; 3],
        });
        mission.player_pitch_bits = 0;
    }
    Ok(Some(cockpit))
}

fn within_boarding_range([hero_x, hero_y]: [f32; 2], [x, y]: [f32; 2]) -> bool {
    (x - hero_x).hypot(y - hero_y) < 20.0
}

#[cfg(test)]
mod tests {
    #[test]
    fn boarding_reach_excludes_the_horizontal_boundary() {
        assert!(super::within_boarding_range([0.0, 0.0], [12.0, 15.99]));
        assert!(!super::within_boarding_range([0.0, 0.0], [12.0, 16.0]));
        assert!(!super::within_boarding_range([0.0, 0.0], [f32::NAN, 0.0]));
    }
}

fn has_working_pilot_control(table: &MissionControlDeviceTableState) -> bool {
    let MissionControlDeviceTableStatus::Available(devices) = &table.status else {
        return false;
    };
    devices
        .iter()
        .find(|device| device.descriptor.tag == 1)
        .is_some_and(|device| {
            control_device_provider_value(table, device).is_ok_and(|value| value > 0.0)
        })
}

/// Leaves the vehicle and restores the hero's cockpit.
///
/// # Errors
///
/// Returns an error when the hero pose or terrain query cannot be read.
pub fn disembark_warbot(engine: &mut Engine) -> Result<Option<StartingCockpit>, EngineError> {
    let Some(mission) = &mut engine.loaded else {
        return Ok(None);
    };
    let Some(Boarding {
        cockpit: vehicle,
        hero,
        ..
    }) = mission.boarding
    else {
        return Ok(None);
    };
    let mut pose = transform_state(&engine.world, hero.handle)?;
    if let Ok(vehicle_pose) = transform_state(&engine.world, vehicle.handle) {
        let [x, y, _] = vehicle_pose.position.map(f32::from_bits);
        let yaw = f32::from_bits(vehicle_pose.orientation[2]);
        let (sin, cos) = yaw.sin_cos();
        let radius = unit_sphere(&mission.mission_assets, vehicle.object_index, vehicle_pose)
            .map_or(1.0, |(_, radius)| radius);
        // shortcut: use the sides and terrain surface until the exit
        // point and collision checks are recovered.
        let mut exit = None;
        for side in [1.0, -1.0] {
            let xy = [
                x + cos * (radius + 1.0) * side,
                y + sin * (radius + 1.0) * side,
            ];
            if let Some(ground) = mission
                .terrain
                .height_at(xy)
                .map_err(EngineError::Terrain)?
            {
                exit = Some([xy[0], xy[1], ground + 0.02]);
                break;
            }
        }
        let Some(exit) = exit else {
            return Ok(None);
        };
        pose.position = exit.map(f32::to_bits);
        pose.orientation[2] = yaw.to_bits();
        set_transform(&mut engine.world, hero.handle, pose)?;
    }
    mission.boarding = None;
    mission.player_pitch_bits = 0;
    Ok(Some(hero))
}

pub(super) fn move_passenger(
    engine: &mut Engine,
    cockpit: StartingCockpit,
    vehicle_pose: TransformState,
) -> Option<(ObjectHandle, TransformState)> {
    let boarding = engine.loaded.as_ref()?.boarding.as_ref()?;
    if boarding.cockpit.handle != cockpit.handle {
        return None;
    }
    let hero = boarding.hero;
    let previous = transform_state(&engine.world, hero.handle).ok()?;
    let pose = TransformState {
        position: vehicle_pose.position,
        ..previous
    };
    set_transform(&mut engine.world, hero.handle, pose).ok()?;
    Some((hero.handle, previous))
}
