use crate::MissionControlDeviceTableState;
use fparkan_world::{registration_sequence, transform_state, ObjectHandle, World};

/// Behavior+0x27250 ends Attack on a missing/depleted object or a zero position.
pub(super) fn complete(
    world: &World,
    controls: &[MissionControlDeviceTableState],
    target: ObjectHandle,
) -> bool {
    if !registration_sequence(world, target).is_ok_and(|sequence| sequence.is_some()) {
        return true;
    }
    if controls
        .iter()
        .any(|table| table.handle == target && table.node_provider_depletion_marked)
    {
        return true;
    }
    transform_state(world, target).map_or(true, |pose| {
        pose.position
            .into_iter()
            .all(|word| f32::from_bits(word) == 0.0)
    })
}
