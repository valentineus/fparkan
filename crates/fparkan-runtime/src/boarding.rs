use super::{
    component_cockpit, projectile::unit_sphere, starting_cockpit, Engine, EngineError,
    StartingCockpit, SurfaceQuery, TransformState,
};
use fparkan_world::{
    handle_by_original_id, identity_metadata, set_transform, transform_state, ObjectHandle,
};

#[derive(Clone, Copy)]
pub(super) struct Boarding {
    pub(super) cockpit: StartingCockpit,
    pub(super) hero: StartingCockpit,
}

/// The vehicle cockpit while the player is aboard.
#[must_use]
pub fn player_vehicle(engine: &Engine) -> Option<StartingCockpit> {
    Some(engine.loaded.as_ref()?.boarding.as_ref()?.cockpit)
}

/// Boards the closest compatible allied warbot.
///
/// # Errors
///
/// Returns an error if the hero's world handle is stale.
pub fn board_friendly_warbot(engine: &mut Engine) -> Result<Option<StartingCockpit>, EngineError> {
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
    let position = hero_pose.position.map(f32::from_bits);
    let candidate = mission
        .object_drafts
        .iter()
        .enumerate()
        .filter_map(|(index, draft)| {
            if draft.unit_dat_header_flags != Some(0x0100_8000) {
                return None;
            }
            let handle = handle_by_original_id(&engine.world, draft.original_id?)?;
            if identity_metadata(&engine.world, handle).ok()?.owner_id != Some(owner) {
                return None;
            }
            let state = transform_state(&engine.world, handle).ok()?;
            let (center, radius) = unit_sphere(&mission.mission_assets, index, state)?;
            let distance = center
                .into_iter()
                .zip(position)
                .map(|(center, player)| (center - player).powi(2))
                .sum::<f32>()
                .sqrt()
                - radius;
            // shortcut: same-clan proximity replaces native selection and boarding admission.
            if !distance.is_finite() || distance > 6.0 {
                return None;
            }
            Some((
                distance,
                component_cockpit(mission, index, handle)?,
                state.position,
            ))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0));
    let Some((_, cockpit, position)) = candidate else {
        return Ok(None);
    };
    set_transform(
        &mut engine.world,
        hero.handle,
        TransformState {
            position,
            ..hero_pose
        },
    )?;
    if let Some(mission) = &mut engine.loaded {
        mission.boarding = Some(Boarding { cockpit, hero });
        mission.player_pitch_bits = 0;
    }
    Ok(Some(cockpit))
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
