use crate::{
    clan_combat_strength::unit_strength, MissionControlDeviceTableState, MissionDocument,
    MissionObjectDraft,
};
use fparkan_world::{
    handle_by_original_id, identity_metadata, registration_sequence, transform_state, ObjectHandle,
    OwnerId, World,
};

#[derive(Clone, Copy)]
pub(super) struct Capturer {
    handle: ObjectHandle,
    target: ObjectHandle,
    pub(super) id: u32,
    target_id: u32,
}

pub(super) fn send_capturer(
    patrol: &mut Option<crate::unit_patrol::UnitPatrol>,
    world: &World,
    drafts: &[MissionObjectDraft],
    unit: Capturer,
    target_id: u32,
) -> Option<(ObjectHandle, ObjectHandle)> {
    let pose = transform_state(world, unit.target).ok()?;
    let target = identity_metadata(world, unit.target).ok()?;
    if unit.target_id != target_id
        || target_id & 0x8000_0000 == 0
        || target.owner_id == identity_metadata(world, unit.handle).ok()?.owner_id
        || target
            .original_id
            .and_then(|id| drafts.get(id.0 as usize))
            .and_then(|draft| draft.unit_dat_header_flags)
            == Some(0x8000_0200)
    {
        return None;
    }
    let [x, y, _] = pose.position.map(f32::from_bits);
    // shortcut: route towards the building; native entrance selection and capture are still open.
    patrol
        .get_or_insert_with(crate::unit_patrol::UnitPatrol::scripted)
        .move_to(world, unit.handle, [x, y], false);
    eprintln!(
        "sim_coverage script_capture_approach_stub=1 unit={} target={} target_id={target_id}",
        unit.handle.slot, unit.target.slot
    );
    Some((unit.handle, unit.target))
}

pub(super) fn nearest_capturer(
    mission: &MissionDocument,
    drafts: &[MissionObjectDraft],
    roster: &crate::mission_events::MissionEvents,
    world: &World,
    owner: OwnerId,
    target_id: u32,
    available: impl Fn(ObjectHandle) -> bool,
) -> Option<Capturer> {
    let target = crate::mission_events::placed_logic_objects(mission, world, target_id).next()?;
    let point = transform_state(world, target)
        .ok()?
        .position
        .map(f32::from_bits);
    // shortcut: all routed robots currently share one speed, so travel time ranks by XY distance.
    roster
        .clan_units(world, owner, true)
        .filter(|&handle| available(handle))
        .filter_map(|handle| {
            let original_id = identity_metadata(world, handle).ok()?.original_id?.0;
            let index = original_id as usize;
            let chassis = drafts.get(index)?.unit_components.first()?;
            if !crate::mission_events::capturer_chassis(&chassis.resource_raw) {
                return None;
            }
            let position = transform_state(world, handle)
                .ok()?
                .position
                .map(f32::from_bits);
            let distance = (point[0] - position[0]).hypot(point[1] - position[1]);
            // shortcut: generated ID is local to this script section until native robot IDs are allocated.
            let id = mission
                .objects
                .get(index)
                .map_or(original_id, |object| object.identity_or_clan_raw);
            (id != u32::MAX).then_some((
                Capturer {
                    handle,
                    target,
                    id,
                    target_id,
                },
                distance,
            ))
        })
        .min_by(|(_, a), (_, b)| a.total_cmp(b))
        .map(|(unit, _)| unit)
}

pub(super) fn encode_strength(strength: f32) -> f32 {
    // The original narrows the truncated x87 result to a DWORD before the script assignment.
    (strength as i64 as u32) as f32
}

pub(super) fn capture_target(
    mission: &MissionDocument,
    drafts: &[MissionObjectDraft],
    controls: &[MissionControlDeviceTableState],
    world: &World,
    clan: usize,
    radius: f32,
    already_targeted: impl Fn(u32) -> bool,
) -> Option<(u32, f32)> {
    let source = mission.clans.get(clan)?;
    // shortcut: use placed-object order until the native ArealMap roster is wired.
    let hostile_objects = || {
        drafts.iter().filter_map(|draft| {
            let class = draft.unit_dat_header_flags?;
            let handle = handle_by_original_id(world, draft.original_id?)?;
            registration_sequence(world, handle).ok()??;
            let owner = usize::from(identity_metadata(world, handle).ok()?.owner_id?.0);
            let other = mission.clans.get(owner)?;
            if owner == clan
                || !source.relations.iter().any(|relation| {
                    relation.relation_value == 0 && relation.other_clan_name.raw == other.name.raw
                })
            {
                return None;
            }
            let position = transform_state(world, handle)
                .ok()?
                .position
                .map(f32::from_bits);
            Some((draft.identity_or_clan_raw, class, handle, position))
        })
    };
    let (id, _, _, position) = hostile_objects()
        .filter(|&(id, class, _, _)| {
            id != u32::MAX && class & 0x8000_0000 != 0 && !already_targeted(id)
        })
        .min_by_key(|&(_, class, _, _)| building_priority(class))?;
    let strength = hostile_objects()
        .filter(|&(_, class, _, candidate)| {
            class & crate::autodemo::MOBILE_UNIT != 0
                && (candidate[0] - position[0]).hypot(candidate[1] - position[1]) < radius
        })
        .map(|(_, _, handle, _)| unit_strength(controls, handle))
        .fold(0.0, |sum, amount| sum + amount);
    Some((id, strength))
}

fn building_priority(class: u32) -> u8 {
    match class {
        0x8000_0002 => 1,
        crate::autodemo::PLANT_CLASS => 2,
        crate::mining::MINE_CLASS => 3,
        0x8000_0400 => 4,
        0x8000_0008 => 5,
        _ => 10,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_order_replaces_patrol_and_rejects_a_friendly_target() {
        use fparkan_world::{
            construct_object, new, register_object, set_owner, ObjectDraft, WorldConfig,
        };

        let mut world = new(WorldConfig);
        let mut create = |owner| {
            let handle = construct_object(&mut world, ObjectDraft { original_id: None }).unwrap();
            register_object(&mut world, handle).unwrap();
            set_owner(&mut world, handle, Some(OwnerId(owner))).unwrap();
            handle
        };
        let handle = create(1);
        let target = create(0);
        let capturer = Capturer {
            handle,
            target,
            id: 3,
            target_id: 0x8000_0002,
        };
        let mut patrol = None;
        assert_eq!(
            send_capturer(&mut patrol, &world, &[], capturer, capturer.target_id),
            Some((handle, target))
        );
        assert!(patrol.as_ref().unwrap().is_moving_to_destination(handle));
        set_owner(&mut world, target, Some(OwnerId(1))).unwrap();
        assert_eq!(
            send_capturer(&mut patrol, &world, &[], capturer, capturer.target_id),
            None
        );
    }

    #[test]
    fn generators_and_factories_rank_ahead_of_other_buildings() {
        let classes = [
            0x8000_0002,
            0x8000_0010,
            0x8000_0004,
            0x8000_0400,
            0x8000_0008,
            0x8002_0000,
        ];
        assert_eq!(classes.map(building_priority), [1, 2, 3, 4, 5, 10]);
    }

    #[test]
    fn strength_output_truncates_and_keeps_the_native_error_value() {
        assert_eq!(encode_strength(8.2), 8.0);
        assert_eq!(encode_strength(1e12), 3_567_583_232.0);
    }
}
