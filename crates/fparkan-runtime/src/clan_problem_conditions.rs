use crate::clan_combat_groups::CombatGroup;
use crate::unit_patrol::UnitPatrol;
use fparkan_world::{identity_metadata, registration_sequence, ObjectHandle, OwnerId, World};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Condition {
    ObjectDestroyed(Option<ObjectHandle>),
    BuildingCaptured(ObjectHandle),
    GroupDestroyed,
    GroupIdle,
    TransportInactive(ObjectHandle),
}

impl Condition {
    pub(super) fn met(
        self,
        world: &World,
        clan: OwnerId,
        patrol: Option<&UnitPatrol>,
        group: Option<&CombatGroup>,
        transport_active: impl Fn(ObjectHandle) -> bool,
    ) -> bool {
        let live =
            |unit| registration_sequence(world, unit).is_ok_and(|sequence| sequence.is_some());
        match self {
            Self::ObjectDestroyed(target) => target.is_none_or(|target| !live(target)),
            Self::BuildingCaptured(target) => {
                live(target)
                    && identity_metadata(world, target)
                        .is_ok_and(|identity| identity.owner_id == Some(clan))
            }
            Self::GroupDestroyed => {
                group.is_none_or(|group| group.members.iter().all(|&unit| !live(unit)))
            }
            Self::TransportInactive(unit) => !live(unit) || !transport_active(unit),
            Self::GroupIdle => group.is_none_or(|group| {
                group.members.iter().all(|&unit| {
                    !live(unit) || patrol.is_none_or(|patrol| !patrol.is_patrolling(unit))
                })
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_world::{
        construct_object, new, register_object, set_owner, ObjectDraft, WorldConfig,
    };

    #[test]
    fn capture_waits_for_the_requested_clan_and_destruction_waits_for_removal() {
        let mut world = new(WorldConfig);
        let target = construct_object(&mut world, ObjectDraft { original_id: None }).unwrap();
        register_object(&mut world, target).unwrap();
        let clan = OwnerId(1);
        set_owner(&mut world, target, Some(OwnerId(0))).unwrap();
        assert!(!Condition::BuildingCaptured(target).met(&world, clan, None, None, |_| false));
        assert!(!Condition::ObjectDestroyed(Some(target)).met(&world, clan, None, None, |_| false));
        set_owner(&mut world, target, Some(clan)).unwrap();
        assert!(Condition::BuildingCaptured(target).met(&world, clan, None, None, |_| false));
        fparkan_world::request_delete(&mut world, target).unwrap();
        fparkan_world::step(&mut world, &fparkan_world::InputSnapshot).unwrap();
        assert!(Condition::ObjectDestroyed(Some(target)).met(&world, clan, None, None, |_| false));
        assert!(!Condition::BuildingCaptured(target).met(&world, clan, None, None, |_| false));
    }
}
