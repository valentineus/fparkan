use crate::clan_combat_strength::unit_strength;
use crate::unit_patrol::UnitPatrol;
use crate::{MissionControlDeviceTableState, MissionDocument};
use fparkan_script::{Handler15Invocation, Handler15TargetPayload};
use fparkan_world::{identity_metadata, transform_state, ObjectHandle, OwnerId, World};

pub(super) struct CombatGroup {
    pub(super) members: Vec<ObjectHandle>,
    pub(super) target: ObjectHandle,
    pub(super) target_owner: Option<OwnerId>,
}

pub(super) fn recruit(
    world: &World,
    units: impl Iterator<Item = ObjectHandle>,
    controls: &[MissionControlDeviceTableState],
    mission: &MissionDocument,
    target_id: u32,
    requested: u32,
) -> (Option<CombatGroup>, f32) {
    let Some(target) =
        crate::mission_events::placed_logic_objects(mission, world, target_id).next()
    else {
        return (None, 0.0);
    };
    let Ok(pose) = transform_state(world, target) else {
        return (None, 0.0);
    };
    let [x, y, _] = pose.position.map(f32::from_bits);
    let mut candidates: Vec<_> = units
        .filter_map(|unit| {
            let [ux, uy, _] = transform_state(world, unit)
                .ok()?
                .position
                .map(f32::from_bits);
            Some(((ux - x).hypot(uy - y), unit_strength(controls, unit), unit))
        })
        .collect();
    let (members, strength) = take_by_hits(&mut candidates, requested);
    let group = (strength > 0.0).then(|| CombatGroup {
        members,
        target,
        target_owner: identity_metadata(world, target)
            .ok()
            .and_then(|identity| identity.owner_id),
    });
    (group, strength)
}

fn take_by_hits(
    candidates: &mut [(f32, f32, ObjectHandle)],
    requested: u32,
) -> (Vec<ObjectHandle>, f32) {
    candidates.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut members = Vec::new();
    let mut strength = 0.0;
    for &(_, power, member) in candidates.iter() {
        members.push(member);
        strength += power;
        if strength >= requested as f32 {
            break;
        }
    }
    (members, strength)
}

impl CombatGroup {
    pub(super) fn dispatch(
        &self,
        patrol: &mut UnitPatrol,
        world: &World,
        mission: &MissionDocument,
        order: Handler15Invocation,
    ) -> Option<()> {
        let (0, kind @ (3 | 4), 3, Handler15TargetPayload::ByLogicId(target_id)) =
            (order.word_0, order.word_1, order.word_2, order.target)
        else {
            return None;
        };
        let target =
            crate::mission_events::placed_logic_objects(mission, world, target_id).next()?;
        if target != self.target {
            return None;
        }
        let index = identity_metadata(world, target).ok()?.original_id?.0 as usize;
        let [x, y, _] = transform_state(world, target)
            .ok()?
            .position
            .map(f32::from_bits);
        for &unit in &self.members {
            if kind == 3 {
                patrol.set_attack_target(world, unit, target, index, [x, y]);
            } else {
                let radius = match order.word_3 {
                    0 | u32::MAX => 60.0,
                    radius => radius as f32,
                };
                patrol.order_around_object(world, unit, target, [x, y], radius);
            }
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recruits_by_distance_until_the_threshold_and_keeps_partial_groups() {
        let [a, b, c] = [1, 2, 3].map(|slot| ObjectHandle {
            slot,
            generation: 1,
        });
        let mut candidates = [(18.0, 20.0, c), (8.0, 5.0, a), (15.0, 7.0, b)];
        assert_eq!(take_by_hits(&mut candidates, 12), (vec![a, b], 12.0));
        assert_eq!(take_by_hits(&mut candidates, 40), (vec![a, b, c], 32.0));
    }
}
