use crate::{EngineError, LoadedMissionState, MissionControlDeviceTableState};
use fparkan_world::{
    construct_replacement, register_object, set_owner, set_transform, transform_state, ObjectDraft,
    ObjectHandle, OwnerId, TransformState, World,
};

/// A reserve model activated with a newly registered world object.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpawnedUnit {
    /// Prepared scene object index.
    pub object_index: usize,
    /// New world handle.
    pub handle: ObjectHandle,
    /// Clan receiving the replacement.
    pub clan: u16,
}

pub(super) fn spawn_prepared_unit(
    world: &mut World,
    mission: &mut LoadedMissionState,
    index: usize,
    clan: u16,
    position: [f32; 3],
    mut table: MissionControlDeviceTableState,
) -> Result<ObjectHandle, EngineError> {
    let draft = &mission.object_drafts[index];
    let handle = construct_replacement(
        world,
        ObjectDraft {
            original_id: draft.original_id,
        },
    )?;
    let pose = TransformState {
        position: position.map(f32::to_bits),
        orientation: draft.orientation_raw.map(f32::to_bits),
        scale: draft.scale.map(f32::to_bits),
    };
    if let Err(error) = set_transform(world, handle, pose)
        .and_then(|()| set_owner(world, handle, Some(OwnerId(clan))))
        .and_then(|()| register_object(world, handle))
    {
        fparkan_world::request_delete(world, handle)?;
        return Err(error.into());
    }
    mission
        .control_device_tables
        .retain(|table| transform_state(world, table.handle).is_ok());
    mission
        .behavior_control_groupings
        .retain(|group| transform_state(world, group.handle).is_ok());
    table.handle = handle;
    if let Some(profile) = mission
        .behavior_profiles
        .iter_mut()
        .find(|profile| Some(profile.original_id) == draft.original_id)
    {
        profile.handle = handle;
    }
    mission
        .behavior_control_groupings
        .push(crate::project_mission_behavior_control_grouping(&table));
    mission.control_device_tables.push(table);
    mission.mission_events.register_unit(
        world,
        handle,
        draft.unit_dat_header_flags.expect("prepared reserve"),
    );
    mission.note_clan_registration(OwnerId(clan), handle);
    Ok(handle)
}
