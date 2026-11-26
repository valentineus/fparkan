//! The main teleporter transfers its owner from an entry vertex to the lower passage.

use crate::{
    mission_events::{inside_exit_volume, MAIN_TELEPORT_CLASS},
    projectile_parts::root_model,
    Engine, ObjectHandle, StartingCockpit,
};
use fparkan_msh::node38_sampled_hierarchy;
use fparkan_render::LegacyIron3dEulerTransform;
use fparkan_world::{handle_by_original_id, identity_metadata, transform_state};

const TELEPORT_IN: u32 = 0x8000;
const TELEPORT_OUT: u32 = 0x10000;
const GENERATOR: u32 = 0x8000_0002;

pub(super) fn landing_position(
    engine: &Engine,
    cockpit: StartingCockpit,
    body_position: [f32; 3],
) -> Option<[u32; 3]> {
    let mission = engine.loaded.as_ref()?;
    if mission.boarding.is_some() || mission.local_hero != Some(cockpit.handle) {
        return None;
    }
    let hero_owner = identity_metadata(&engine.world, cockpit.handle)
        .ok()?
        .owner_id?;
    let owns = |handle: ObjectHandle| {
        identity_metadata(&engine.world, handle)
            .is_ok_and(|identity| identity.owner_id == Some(hero_owner))
    };

    mission
        .object_drafts
        .iter()
        .enumerate()
        .find_map(|(index, draft)| {
            if draft.unit_dat_header_flags != Some(MAIN_TELEPORT_CLASS) {
                return None;
            }
            let handle = handle_by_original_id(&engine.world, draft.original_id?)?;
            if !owns(handle) {
                return None;
            }
            let pose = transform_state(&engine.world, handle).ok()?;
            let model = &root_model(&mission.mission_assets, index)?.validated;
            let hierarchy = node38_sampled_hierarchy(model, 0)?;
            let transform = LegacyIron3dEulerTransform {
                translation: pose.position.map(f32::from_bits),
                orientation_radians: pose.orientation.map(f32::from_bits),
            };
            let project = |point: &fparkan_msh::HallwayPoint| {
                let node = hierarchy.poses.get(point.node.0 as usize)?;
                transform.try_transform_scaled_point(
                    node.transform_point(point.position),
                    pose.scale.map(f32::from_bits),
                )
            };
            let points = &model.hallway.points;
            if !points
                .iter()
                .filter(|point| point.flags & TELEPORT_IN != 0)
                .filter_map(project)
                .any(|entry| inside_exit_volume(entry, body_position))
            {
                return None;
            }

            // shortcut: the native generator check uses the active queue's selection.
            let mut generators = mission
                .object_drafts
                .iter()
                .filter(|draft| draft.unit_dat_header_flags == Some(GENERATOR))
                .filter_map(|draft| handle_by_original_id(&engine.world, draft.original_id?));
            if !generators.next().is_some_and(owns) || !generators.all(owns) {
                return None;
            }

            points
                .iter()
                .find(|point| point.flags & TELEPORT_OUT != 0)
                .and_then(project)
                .map(|exit| exit.map(f32::to_bits))
        })
}

#[cfg(test)]
mod tests {
    use crate::{
        capture_neutral_building, create, frame_with_player_motion, load_gameplay_mission,
        plan_player_motion, starting_cockpit, EngineServices, MissionMode, MissionRequest,
        MissionStartupInput, PlayerMotion,
    };
    use fparkan_vfs::DirectoryVfs;
    use fparkan_world::{set_transform, transform_state, OriginalObjectId};
    use std::sync::Arc;

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_main_teleporter_sends_hero_to_the_lower_passage() {
        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed root");
        let vfs = Arc::new(DirectoryVfs::new(std::path::PathBuf::from(root)));
        let mut engine = create(EngineServices::new(vfs)).unwrap();
        load_gameplay_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/CAMPAIGN/CAMPAIGN.00/Mission.04/data.tma".into(),
                mode: MissionMode::SingleMission,
            },
            MissionStartupInput {
                queue_time_ms: 0,
                research_creation_gate: false,
            },
            None,
        )
        .unwrap();
        let hero = starting_cockpit(&engine).unwrap();
        let move_on_entry = |engine: &mut crate::Engine, now| {
            let mut pose = transform_state(&engine.world, hero.handle).unwrap();
            pose.position = [1152.2509_f32, 1264.5708, 100.14729].map(f32::to_bits);
            set_transform(&mut engine.world, hero.handle, pose).unwrap();
            let step = plan_player_motion(
                engine,
                hero,
                PlayerMotion {
                    movement: [0.0, 1.0],
                    delta_seconds: 0.02,
                    ..Default::default()
                },
            )
            .unwrap()
            .unwrap();
            frame_with_player_motion(engine, now, hero, step, false).unwrap();
            transform_state(&engine.world, hero.handle)
                .unwrap()
                .position
                .map(f32::from_bits)
        };
        assert!(move_on_entry(&mut engine, 20)[2] > 100.0);

        let mut pose = transform_state(&engine.world, hero.handle).unwrap();
        pose.position = [1178.25_f32, 1238.89, 101.62].map(f32::to_bits);
        set_transform(&mut engine.world, hero.handle, pose).unwrap();
        assert_eq!(
            capture_neutral_building(&mut engine, hero).unwrap(),
            Some(OriginalObjectId(3))
        );

        let actual = move_on_entry(&mut engine, 40);
        for (value, expected) in actual.into_iter().zip([1126.3053, 1254.558, 68.9137]) {
            assert!((value - expected).abs() < 0.002);
        }
    }
}
