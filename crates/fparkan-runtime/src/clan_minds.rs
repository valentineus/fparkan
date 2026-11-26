/// Project the native mind slots from registered mobile units and factory orders.
pub(super) fn free_slots(
    capacity: usize,
    classes: impl Iterator<Item = u32>,
    reserved: usize,
) -> u32 {
    let live = classes
        .filter(|class| class & super::autodemo::MOBILE_UNIT != 0)
        .count();
    capacity.saturating_sub(live.saturating_add(reserved)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stock_clan_occupancy_and_factory_reservation_match_native_slots() {
        let units = [
            0x0100_2000,
            0x0100_8000,
            0x0100_8000,
            0x0100_8000,
            0x0100_8000,
        ];
        assert_eq!(free_slots(9, units.into_iter(), 0), 4);
        assert_eq!(free_slots(9, units.into_iter(), 1), 3);
        // Replacing the factory reservation with a newborn keeps the same occupancy.
        assert_eq!(free_slots(9, units.into_iter().chain([0x0100_8000]), 0), 3);
        assert_eq!(free_slots(9, units.into_iter(), 4), 0);
        assert_eq!(free_slots(9, units.into_iter(), 5), 0);
    }

    #[test]
    fn hero_uses_a_mind_and_buildings_do_not() {
        let classes = [0x0102_0000, 0x0100_4000, 0x8000_0008, 0x8000_0010];
        assert_eq!(free_slots(9, classes.into_iter(), 0), 7);
        assert_eq!(free_slots(0, classes.into_iter(), 0), 0);
    }

    #[test]
    fn captured_and_deleted_units_release_the_former_clans_slots() {
        use fparkan_world::{
            construct_object, new, register_object, request_delete, set_owner, step, InputSnapshot,
            ObjectDraft, OwnerId, WorldConfig,
        };
        let mut world = new(WorldConfig);
        let mut events = crate::mission_events::MissionEvents::default();
        let unit = construct_object(&mut world, ObjectDraft { original_id: None }).unwrap();
        register_object(&mut world, unit).unwrap();
        set_owner(&mut world, unit, Some(OwnerId(1))).unwrap();
        events.register_unit(&world, unit, 0x0100_8000);
        assert_eq!(free_slots(9, events.clan_classes(&world, OwnerId(1)), 0), 8);
        set_owner(&mut world, unit, Some(OwnerId(0))).unwrap();
        assert_eq!(free_slots(9, events.clan_classes(&world, OwnerId(1)), 0), 9);
        assert_eq!(free_slots(9, events.clan_classes(&world, OwnerId(0)), 0), 8);
        request_delete(&mut world, unit).unwrap();
        step(&mut world, &InputSnapshot).unwrap();
        assert_eq!(free_slots(9, events.clan_classes(&world, OwnerId(0)), 0), 9);
    }

    #[test]
    #[ignore = "requires licensed mission resources"]
    fn licensed_stock_factory_reserves_a_mind_until_birth_or_ownership_loss() {
        use crate::{
            create, frame, load_gameplay_mission, EngineServices, MissionMode, MissionRequest,
            MissionStartupInput,
        };
        use fparkan_vfs::DirectoryVfs;
        use fparkan_world::{identity_metadata, set_owner, OwnerId};
        use std::sync::Arc;
        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").unwrap();
        let mut engine = create(EngineServices::new(Arc::new(DirectoryVfs::new(root)))).unwrap();
        load_gameplay_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/Single.01/data.tma".into(),
                mode: MissionMode::SingleMission,
            },
            MissionStartupInput {
                queue_time_ms: 0,
                research_creation_gate: false,
            },
            None,
        )
        .unwrap();
        let available = |engine: &crate::Engine, owner: OwnerId| {
            let loaded = engine.loaded.as_ref().unwrap();
            let capacity = loaded
                .ai_startup_states
                .iter()
                .find(|state| state.clan_index == usize::from(owner.0))
                .unwrap()
                .record_slots
                .len();
            free_slots(
                capacity,
                loaded.mission_events.clan_classes(&engine.world, owner),
                engine
                    .production
                    .as_ref()
                    .unwrap()
                    .reserved_minds(&engine.world, owner),
            )
        };
        let reserved = |engine: &crate::Engine| {
            engine
                .production
                .as_ref()
                .unwrap()
                .reserved_minds(&engine.world, OwnerId(1))
        };
        assert_eq!(available(&engine, OwnerId(0)), 3);
        assert_eq!(available(&engine, OwnerId(1)), 4);
        frame(&mut engine, 0).unwrap();
        assert_eq!(reserved(&engine), 1);
        assert_eq!(available(&engine, OwnerId(1)), 3);
        let first_release = (100..=22_000)
            .step_by(100)
            .find(|&now_ms| {
                frame(&mut engine, now_ms).unwrap();
                reserved(&engine) == 0
            })
            .expect("the first build converted its reservation into a robot");
        assert_eq!(available(&engine, OwnerId(1)), 3);
        assert!(((first_release + 100)..=32_000).step_by(100).any(|now_ms| {
            frame(&mut engine, now_ms).unwrap();
            reserved(&engine) == 1
        }));
        assert_eq!(available(&engine, OwnerId(1)), 2);
        let loaded = engine.loaded.as_ref().unwrap();
        let plant = loaded
            .mission_events
            .clan_units(&engine.world, OwnerId(1), false)
            .find(|&unit| {
                engine
                    .production
                    .as_ref()
                    .unwrap()
                    .is_building_at(unit, OwnerId(1))
            })
            .unwrap();
        assert_eq!(
            identity_metadata(&engine.world, plant).unwrap().owner_id,
            Some(OwnerId(1))
        );
        // Debug ownership transfer tests cancellation; it is not mission acceptance.
        set_owner(&mut engine.world, plant, Some(OwnerId(0))).unwrap();
        assert_eq!(available(&engine, OwnerId(1)), 3);
        assert_eq!(
            engine
                .production
                .as_ref()
                .unwrap()
                .reserved_minds(&engine.world, OwnerId(0)),
            0
        );
    }
}
