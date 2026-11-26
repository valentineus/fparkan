//! Chassis support and Control speed limits for a boarded ground warbot.

use crate::{
    ground_support::GroundSupport, EngineError, LoadedMissionState, ObjectHandle, PlayerMotionStep,
    TerrainWorld,
};
use fparkan_terrain::SurfaceQuery;
use fparkan_world::TransformState;

pub(super) fn speed_limits(mission: &LoadedMissionState, handle: ObjectHandle) -> Option<[f32; 2]> {
    let profile = mission
        .control_device_tables
        .iter()
        .find(|table| table.handle == handle)?
        .owner_profile
        .as_ref()?;
    let limits = [6, 7].map(|word| f32::from_bits(profile.current_words[word]));
    limits
        .into_iter()
        .all(|rate| rate.is_finite() && rate >= 0.0)
        .then_some(limits)
}

pub(super) fn support_step(
    ground: Option<GroundSupport>,
    terrain: &TerrainWorld,
    previous: TransformState,
    step: &mut PlayerMotionStep,
) -> Result<(), EngineError> {
    let Some(ground) = ground else {
        return Ok(());
    };
    if step.sweep.is_none() || step.on_building_floor() {
        return Ok(());
    }
    ground.reanchor(previous, &mut step.pose, |xy| {
        terrain.height_at(xy).map_err(EngineError::Terrain)
    })?;
    if let Some([_, end]) = step.sweep.as_mut() {
        end[2] = f32::from_bits(step.pose.position[2]) + 0.5;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        board_friendly_warbot, create, frame, frame_with_player_motion, load_gameplay_mission,
        mission_object_handle, plan_player_motion, starting_cockpit, EngineServices, MissionMode,
        MissionRequest, MissionStartupInput, PlayerMotion,
    };
    use fparkan_vfs::DirectoryVfs;
    use fparkan_world::{set_transform, transform_state, OriginalObjectId};
    use std::{path::PathBuf, sync::Arc};

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_boarded_warbot_uses_its_control_profile_and_chassis_support() {
        let root =
            PathBuf::from(std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed root"));
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
        frame(&mut engine, 0).unwrap();
        let hero = starting_cockpit(&engine).unwrap();
        let id = OriginalObjectId(18);
        let robot = mission_object_handle(&engine, id).unwrap();
        let mut hero_pose = transform_state(&engine.world, hero.handle).unwrap();
        hero_pose.position = transform_state(&engine.world, robot).unwrap().position;
        set_transform(&mut engine.world, hero.handle, hero_pose).unwrap();
        let cockpit = board_friendly_warbot(&mut engine, Some(robot))
            .unwrap()
            .unwrap();
        let mission = engine.loaded.as_ref().unwrap();
        let ground = mission.boarding.unwrap().ground.expect("ground chassis");
        let [_, cap] = speed_limits(mission, robot).unwrap();
        assert!(cap > 0.0 && (cap - 8.0).abs() > 0.1);
        let mut initial = transform_state(&engine.world, robot).unwrap();
        let [x, y, _] = initial.position.map(f32::from_bits);
        let floor = mission.terrain.height_at([x, y]).unwrap().unwrap();
        initial.position[2] = (floor - 1.0).to_bits();
        set_transform(&mut engine.world, robot, initial).unwrap();

        let motion = plan_player_motion(
            &engine,
            cockpit,
            PlayerMotion {
                movement: [0.0, 1.0],
                delta_seconds: 0.1,
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap();
        frame_with_player_motion(&mut engine, 100, cockpit, motion, false).unwrap();
        let after = transform_state(&engine.world, robot).unwrap();
        let [nx, ny, z] = after.position.map(f32::from_bits);
        assert!(((nx - x).hypot(ny - y) - 0.1 * cap).abs() < 0.001);
        let terrain = &engine.loaded.as_ref().unwrap().terrain;
        let support = ground
            .height([nx, ny], after, &mut |xy| {
                terrain.height_at(xy).map_err(EngineError::Terrain)
            })
            .unwrap()
            .unwrap();
        assert!((z - support).abs() < 0.001);
        assert_eq!(
            transform_state(&engine.world, hero.handle)
                .unwrap()
                .position,
            after.position
        );
    }
}
