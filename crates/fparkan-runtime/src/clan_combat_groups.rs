use crate::clan_combat_strength::unit_strength;
use crate::unit_patrol::UnitPatrol;
use crate::{MissionControlDeviceTableState, MissionDocument, MissionObjectDraft};
use fparkan_script::{Handler15Invocation, Handler15TargetPayload};
use fparkan_world::{identity_metadata, transform_state, ObjectHandle, OwnerId, World};

pub(super) struct CombatGroup {
    pub(super) members: Vec<ObjectHandle>,
    pub(super) target: GroupTarget,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum GroupTarget {
    Unassigned,
    Object(ObjectHandle, Option<OwnerId>),
    Point([u32; 2]),
}

impl GroupTarget {
    fn resolve(
        world: &World,
        mission: &MissionDocument,
        target: Handler15TargetPayload,
    ) -> Option<Self> {
        match target {
            Handler15TargetPayload::ByLogicId(id) => {
                let handle =
                    crate::mission_events::placed_logic_objects(mission, world, id).next()?;
                Some(Self::Object(
                    handle,
                    identity_metadata(world, handle).ok()?.owner_id,
                ))
            }
            Handler15TargetPayload::ByPlace { first, second } => Some(Self::Point([first, second])),
            _ => None,
        }
    }

    fn position(self, world: &World) -> Option<[f32; 2]> {
        match self {
            Self::Unassigned => None,
            Self::Object(handle, _) => {
                let [x, y, _] = transform_state(world, handle)
                    .ok()?
                    .position
                    .map(f32::from_bits);
                Some([x, y])
            }
            Self::Point(point) => Some(point.map(|value| value as f32)),
        }
    }

    pub(super) fn valid(self, world: &World) -> bool {
        match self {
            Self::Unassigned => false,
            Self::Object(handle, owner) => {
                identity_metadata(world, handle).is_ok_and(|identity| identity.owner_id == owner)
            }
            Self::Point(_) => true,
        }
    }

    pub(super) fn has_order(self, patrol: &UnitPatrol, unit: ObjectHandle) -> bool {
        match self {
            Self::Unassigned => false,
            Self::Object(target, _) => patrol.has_object_order(unit, target),
            Self::Point(point) => patrol.has_point_order(unit, point.map(|value| value as f32)),
        }
    }

    pub(super) fn clear_order(self, patrol: &mut UnitPatrol, unit: ObjectHandle) {
        match self {
            Self::Unassigned => {}
            Self::Object(target, _) => patrol.clear_object_order(unit, target),
            Self::Point(point) => patrol.clear_point_order(unit, point.map(|value| value as f32)),
        }
    }
}

pub(super) fn recruit_all(units: impl Iterator<Item = ObjectHandle>) -> Option<CombatGroup> {
    let members: Vec<_> = units.collect();
    (!members.is_empty()).then_some(CombatGroup {
        members,
        target: GroupTarget::Unassigned,
    })
}

pub(super) fn recruit(
    world: &World,
    units: impl Iterator<Item = ObjectHandle>,
    controls: &[MissionControlDeviceTableState],
    mission: &MissionDocument,
    target: Handler15TargetPayload,
    requested: u32,
) -> (Option<CombatGroup>, f32) {
    let Some(target) = GroupTarget::resolve(world, mission, target) else {
        return (None, 0.0);
    };
    let Some([x, y]) = target.position(world) else {
        return (None, 0.0);
    };
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
        target: GroupTarget::Unassigned,
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
        &mut self,
        patrol: &mut UnitPatrol,
        world: &World,
        mission: &MissionDocument,
        drafts: &[MissionObjectDraft],
        order: Handler15Invocation,
    ) -> Result<(), String> {
        let (0, kind @ (3 | 4), 3) = (order.word_0, order.word_1, order.word_2) else {
            return Err(
                "Handler(28) requires a replacing patrol or attack order on group 0".into(),
            );
        };
        let target = GroupTarget::resolve(world, mission, order.target)
            .ok_or("Handler(28) target is unavailable")?;
        for &unit in &self.members {
            let class = identity_metadata(world, unit)
                .ok()
                .and_then(|identity| identity.original_id)
                .and_then(|id| drafts.get(id.0 as usize))
                .and_then(|draft| draft.unit_dat_header_flags);
            if class != Some(crate::boarding::WARBOT_CLASS) {
                return Err(format!(
                    "Handler(28) unsupported group member class {class:?} unit={}",
                    unit.slot
                ));
            }
        }
        let point = target
            .position(world)
            .ok_or("Handler(28) target has no position")?;
        let attack = match (kind, target) {
            (3, GroupTarget::Object(handle, _)) => Some((
                handle,
                identity_metadata(world, handle)
                    .ok()
                    .and_then(|identity| identity.original_id)
                    .ok_or("Handler(28) target has no runtime ID")?
                    .0 as usize,
            )),
            (3, GroupTarget::Point(_)) => {
                return Err("Handler(28) attack by place is unsupported".into())
            }
            _ => None,
        };
        for &unit in &self.members {
            if let Some((handle, index)) = attack {
                patrol.set_attack_target(world, unit, handle, index, point);
            } else {
                let radius = match order.word_3 {
                    0 | u32::MAX => 60.0,
                    radius => radius as f32,
                };
                match target {
                    GroupTarget::Unassigned => unreachable!("resolved order has a target"),
                    GroupTarget::Object(handle, _) => {
                        patrol.order_around_object(world, unit, handle, point, radius);
                    }
                    GroupTarget::Point(_) => {
                        patrol.order(world, unit, point, radius);
                    }
                }
            }
        }
        self.target = target;
        Ok(())
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
