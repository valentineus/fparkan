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

#[derive(Clone, Copy)]
pub(super) struct CaptureApproach {
    pub(super) unit: ObjectHandle,
    pub(super) target: ObjectHandle,
    point: [f32; 3],
}

pub(super) fn send_capturer(
    patrol: &mut Option<crate::unit_patrol::UnitPatrol>,
    world: &World,
    drafts: &[MissionObjectDraft],
    assets: &crate::MissionAssets,
    unit: Capturer,
    target_id: u32,
) -> Option<CaptureApproach> {
    let pose = transform_state(world, unit.target).ok()?;
    let target = identity_metadata(world, unit.target).ok()?;
    let index = target.original_id?.0 as usize;
    let class = drafts.get(index)?.unit_dat_header_flags?;
    if unit.target_id != target_id
        || target_id & 0x8000_0000 == 0
        || target.owner_id == identity_metadata(world, unit.handle).ok()?.owner_id
        || class == 0x8000_0200
    {
        return None;
    }
    let from = transform_state(world, unit.handle)
        .ok()?
        .position
        .map(f32::from_bits);
    // shortcut: use the communicator mesh until the native door and interior route are wired.
    let point = nearest_platform_point(
        crate::mission_events::communicator_triangles(assets, index, class, pose)?,
        from,
    )?;
    let [x, y, _] = point;
    patrol
        .get_or_insert_with(crate::unit_patrol::UnitPatrol::scripted)
        .move_to(world, unit.handle, [x, y], false);
    eprintln!(
        "sim_coverage script_capture_approach_stub=1 unit={} target={} target_id={target_id}",
        unit.handle.slot, unit.target.slot
    );
    Some(CaptureApproach {
        unit: unit.handle,
        target: unit.target,
        point,
    })
}

fn nearest_platform_point(triangles: Vec<[[f32; 3]; 3]>, from: [f32; 3]) -> Option<[f32; 3]> {
    triangles
        .into_iter()
        .map(|[a, b, c]| std::array::from_fn(|axis| (a[axis] + b[axis] + c[axis]) / 3.0))
        .min_by(|a, b| distance_squared(from, *a).total_cmp(&distance_squared(from, *b)))
}

fn distance_squared(from: [f32; 3], to: [f32; 3]) -> f32 {
    from.into_iter()
        .zip(to)
        .map(|(from, to)| (from - to).powi(2))
        .sum()
}

impl CaptureApproach {
    pub(super) fn reached(self, world: &World) -> bool {
        transform_state(world, self.unit)
            .is_ok_and(|pose| at_platform(pose.position.map(f32::from_bits), self.point))
    }
}

fn at_platform([x, y, z]: [f32; 3], [tx, ty, tz]: [f32; 3]) -> bool {
    (x - tx).hypot(y - ty) <= 0.05 && (z - tz).abs() <= 2.0
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
    fn approach_targets_the_nearest_platform_triangle() {
        assert_eq!(
            nearest_platform_point(
                vec![
                    [[12., 10., 0.], [15., 10., 0.], [12., 13., 0.]],
                    [[0., 0., 2.], [3., 0., 2.], [0., 3., 2.]],
                ],
                [1., 1., 0.],
            ),
            Some([1., 1., 2.])
        );
    }

    #[test]
    fn capture_requires_the_platform_height_as_well_as_the_route_endpoint() {
        let platform = [425.7, 326.9, 61.1];
        assert!(!at_platform([425.7, 326.9, 76.6], platform));
        assert!(!at_platform([425.8, 326.9, 61.1], platform));
        assert!(at_platform([425.7, 326.9, 61.1], platform));
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
