use crate::{
    mission_events::MissionEvents, MissionControlDeviceTableState, MissionDocument,
    MissionObjectDraft,
};
use fparkan_world::{handle_by_original_id, transform_state, OwnerId, World};

pub(super) fn weakest_enemy_of_class(
    mission: &MissionDocument,
    roster: &MissionEvents,
    drafts: &[MissionObjectDraft],
    controls: &[MissionControlDeviceTableState],
    world: &World,
    caller: usize,
    class: u32,
    radius: f32,
) -> Option<(u32, f32)> {
    let source = mission.clans.get(caller)?;
    let enemies: Vec<_> = mission
        .clans
        .iter()
        .enumerate()
        .filter(|(index, candidate)| {
            *index != caller
                && source.relations.iter().any(|relation| {
                    relation.relation_value == 0
                        && relation.other_clan_name.raw == candidate.name.raw
                })
        })
        .flat_map(|(index, _)| roster.clan_units(world, OwnerId(index as u16), false))
        .collect();
    // shortcut: use mission order until the native ArealMap building lists are wired.
    let candidates = drafts.iter().filter_map(|draft| {
        if class & 0x8000_0000 == 0
            || draft.unit_dat_header_flags != Some(class)
            || draft.identity_or_clan_raw == u32::MAX
        {
            return None;
        }
        let handle = handle_by_original_id(world, draft.original_id?)?;
        if !enemies.contains(&handle) {
            return None;
        }
        let position = transform_state(world, handle)
            .ok()?
            .position
            .map(f32::from_bits);
        let strength = crate::clan_targeting::area_strength(
            world,
            drafts,
            controls,
            enemies.iter().copied(),
            position,
            radius,
        );
        Some((draft.identity_or_clan_raw, strength))
    });
    crate::clan_targeting::least_defended(candidates)
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_stock_convoy_uses_size_class_and_assigns_an_attack() {
        use crate::{EngineServices, MissionMode, MissionRequest, MissionStartupInput};
        use fparkan_vfs::DirectoryVfs;
        use fparkan_world::{identity_metadata, set_owner, OwnerId};
        use std::sync::Arc;

        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed corpus root");
        let mut engine =
            crate::create(EngineServices::new(Arc::new(DirectoryVfs::new(root)))).unwrap();
        crate::load_gameplay_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/CAMPAIGN/CAMPAIGN.03/Mission.02/data.tma".into(),
                mode: MissionMode::SingleMission,
            },
            MissionStartupInput {
                queue_time_ms: 0,
                research_creation_gate: false,
            },
            None,
        )
        .unwrap();
        let mut heard = Vec::new();
        for now in (0..=900_000).step_by(100) {
            heard.extend(crate::frame(&mut engine, now).unwrap().clan_notices);
            if engine
                .loaded
                .as_ref()
                .unwrap()
                .clan_ai_events
                .class_target_queries()
                .any(|(clan, _, _, _)| clan == 2)
            {
                break;
            }
        }
        assert_eq!(
            heard
                .iter()
                .filter(|&&notice| notice == crate::MissionNotice::Information(0))
                .count(),
            1
        );
        let loaded = engine.loaded.as_ref().unwrap();
        let actor = loaded
            .mission_events
            .logic_objects(&loaded.mission, &engine.world, 15)
            .next()
            .unwrap();
        let target = loaded
            .mission_events
            .logic_objects(&loaded.mission, &engine.world, 0x8000_0001)
            .next()
            .unwrap();
        assert_eq!(
            loaded
                .unit_patrol
                .as_ref()
                .unwrap()
                .attack_target(actor)
                .map(|(unit, _)| unit),
            Some(target)
        );
        assert!(loaded
            .clan_ai_events
            .class_target_queries()
            .any(|(clan, class, id, count)| {
                clan == 2 && class == 0x8001_0000 && id == 0x8000_0001 && count > 0
            }));
        assert!(loaded
            .clan_ai_events
            .size_class_queries()
            .any(|(clan, id, count)| clan == 1 && id == 0x8000_0003 && count > 0));
        assert!(loaded.clan_ai_events.stops().all(|(stop, _)| ![
            "Handler(30)",
            "Handler(71)",
            "Handler(72)"
        ]
        .iter()
        .any(|handler| stop.reason.starts_with(handler))));

        let classified = loaded
            .mission_events
            .logic_objects(&loaded.mission, &engine.world, 0x8000_0003)
            .next()
            .unwrap();
        let original_owner = identity_metadata(&engine.world, classified)
            .unwrap()
            .owner_id;
        // Debug ownership change checks the descriptor binding, separately from the stock-start attack.
        set_owner(&mut engine.world, classified, Some(OwnerId(3))).unwrap();
        assert_eq!(
            crate::unit_size_class::query(
                &loaded.mission,
                &loaded.mission_events,
                &loaded.object_drafts,
                &engine.world,
                0x8000_0003
            )
            .unwrap(),
            2
        );
        set_owner(&mut engine.world, classified, original_owner).unwrap();
        assert_eq!(
            crate::unit_size_class::query(
                &loaded.mission,
                &loaded.mission_events,
                &loaded.object_drafts,
                &engine.world,
                u32::MAX
            )
            .unwrap(),
            u32::MAX
        );
    }
}
