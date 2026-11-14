use crate::{
    clan_combat_strength::unit_strength, MissionControlDeviceTableState, MissionDocument,
    MissionObjectDraft,
};
use fparkan_world::{
    handle_by_original_id, identity_metadata, registration_sequence, transform_state, World,
};

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
