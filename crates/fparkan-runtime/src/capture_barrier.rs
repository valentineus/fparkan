//! The authored sliding field around a building communicator.

use crate::projectile_parts::{model_triangle_hit, root_model};
use crate::{Engine, LoadedMissionState, MissionAssets, ProjectileNode, TransformState};
use fparkan_animation::AnimationTime;
use fparkan_assets::{PreparedComponentControl, PreparedControlTimelineRow};
use fparkan_msh::node38_sampled_hierarchy_at_times;
use fparkan_world::{transform_state, ObjectHandle, World};

pub(super) struct CaptureBarrier {
    building: ObjectHandle,
    operator: ObjectHandle,
    previous: Option<(u32, [u32; 2])>,
    blend: f32,
    blend_per_second: f32,
    opening: bool,
}

fn rows(control: &PreparedComponentControl) -> impl Iterator<Item = &PreparedControlTimelineRow> {
    control
        .typed_records
        .iter()
        .filter(|record| record.tag == 13)
        .flat_map(|record| &record.values)
        .filter_map(|&row| control.timeline_rows.get(row as usize))
        .filter(|row| row.flags & 4 == 0)
}

fn control(assets: &MissionAssets, index: usize) -> Option<&PreparedComponentControl> {
    assets
        .object_component_controls
        .get(index)?
        .first()?
        .as_ref()
}

fn authored_rate(control: &PreparedComponentControl) -> Option<f32> {
    let mut fields = control
        .typed_records
        .iter()
        .filter(|record| record.tag == 13);
    let field = fields.next()?;
    let factor = f32::from_bits(field.fixed_words[9]);
    // Constant inputs with a phase step small enough for either kind of track.
    if fields.next().is_some()
        || field.fixed_words[2] != 0
        || !factor.is_finite()
        || factor <= 0.0
        || 0.45 * factor > 0.5
    {
        return None;
    }
    let rate = rows(control).try_fold(f32::INFINITY, |slowest, row| {
        let rate = factor * f32::from_bits(row.raw_parameters[2]);
        (rate.is_finite() && rate > 0.0).then_some(slowest.min(rate))
    })?;
    rate.is_finite().then_some(rate)
}

impl CaptureBarrier {
    pub(super) fn start(
        assets: &MissionAssets,
        building: ObjectHandle,
        index: usize,
        operator: ObjectHandle,
    ) -> Option<Self> {
        let control = control(assets, index)?;
        rows(control).next()?;
        Some(Self {
            building,
            operator,
            previous: None,
            blend: 0.0,
            // shortcut: keep the door rate for unsupported controller inputs.
            blend_per_second: authored_rate(control)
                .unwrap_or(crate::building_door::DOOR_BLEND_PER_SECOND),
            opening: false,
        })
    }
}

pub(super) fn advance(mission: &mut LoadedMissionState, world: &World, now: u32) {
    let Some(barrier) = &mut mission.capture_barrier else {
        return;
    };
    let Ok(pose) = transform_state(world, barrier.operator) else {
        mission.capture_barrier = None;
        return;
    };
    let [x, y, _] = pose.position;
    let Some((last, previous_xy)) = barrier.previous.replace((now, [x, y])) else {
        return;
    };
    // shortcut: keep the field down while the operator stays at the station.
    // Opening on resumed movement stands in for leaving the original factory UI.
    barrier.opening |= previous_xy != [x, y];
    let direction = if barrier.opening { -1.0 } else { 1.0 };
    let dt = now.wrapping_sub(last) as f32 / 1_000.0;
    barrier.blend = (barrier.blend + direction * dt * barrier.blend_per_second).clamp(0.0, 1.0);
    if barrier.opening && barrier.blend == 0.0 {
        mission.capture_barrier = None;
    }
}

fn blend(mission: &LoadedMissionState, building: ObjectHandle) -> f32 {
    mission
        .capture_barrier
        .as_ref()
        .filter(|barrier| barrier.building == building)
        .map_or(0.0, |barrier| barrier.blend)
}

fn frame(row: &PreparedControlTimelineRow, blend: f32) -> f32 {
    let blend = row.initial_blend + (1.0 - row.initial_blend) * blend;
    let blend = if row.flags & 2 == 0 {
        blend
    } else {
        1.0 - blend
    };
    row.frame_a + (row.frame_b - row.frame_a) * blend
}

/// Applies the capture field's animated node times to the rendered building.
pub fn sample_capture_barrier(
    engine: &Engine,
    building: ObjectHandle,
    control: &PreparedComponentControl,
    times: &mut [Option<f32>],
) {
    let Some(mission) = engine.loaded.as_ref() else {
        return;
    };
    let blend = blend(mission, building);
    if blend > 0.0 {
        for row in rows(control) {
            if let Some(time) = times.get_mut(row.node_index) {
                *time = Some(frame(row, blend));
            }
        }
    }
}

pub(super) fn projectile_contact(
    mission: &LoadedMissionState,
    index: usize,
    building: ObjectHandle,
    state: TransformState,
    segment: [[f32; 3]; 2],
) -> Option<(f32, ProjectileNode)> {
    let assets = &mission.mission_assets;
    let fallback = || {
        crate::projectile_parts::root_triangle_hit(assets, index, state, segment, |_| true)
            .map(|(fraction, node)| (fraction, ProjectileNode::Building(node)))
    };
    let Some(control) = control(assets, index).filter(|control| rows(control).next().is_some())
    else {
        return fallback();
    };
    let model = &root_model(assets, index)?.validated;
    let mut times = vec![None; model.node_count];
    let blend = blend(mission, building);
    for row in rows(control) {
        if let Some(time) = times.get_mut(row.node_index) {
            *time = Some(AnimationTime(frame(row, blend)));
        }
    }
    let Some(poses) = node38_sampled_hierarchy_at_times(model, &times) else {
        return fallback();
    };
    let (fraction, node) = model_triangle_hit(model, poses, state, segment, |_| true)?;
    let node = if times[node as usize].is_some() {
        ProjectileNode::CaptureBarrier
    } else {
        ProjectileNode::Building(node)
    };
    Some((fraction, node))
}

#[cfg(test)]
mod tests {
    use super::authored_rate;
    use crate::{
        apply_projectile_contact, capture_building, create, frame, load_gameplay_mission,
        projectile_unit_hit, starting_cockpit, EngineServices, MissionMode, MissionRequest,
        MissionStartupInput, ProjectileNode,
    };
    use fparkan_vfs::DirectoryVfs;
    use fparkan_world::{set_transform, transform_state, OriginalObjectId};
    use std::sync::Arc;

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_factory_field_stops_shots_only_when_lowered() {
        let root = std::path::PathBuf::from(
            std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed root"),
        );
        let vfs = Arc::new(DirectoryVfs::new(root));
        let mut engine = create(EngineServices::new(vfs)).unwrap();
        load_gameplay_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/CAMPAIGN/CAMPAIGN.00/Mission.02/data.tma".into(),
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
        let mut pose = transform_state(&engine.world, hero.handle).unwrap();
        let center = [391.2424_f32, 740.083, 140.74289];
        pose.position = center.map(f32::to_bits);
        set_transform(&mut engine.world, hero.handle, pose).unwrap();
        let forward = [[center[0], center[1], 141.6], [center[0], 746.0, 141.6]];
        let backward = [forward[1], forward[0]];
        for segment in [forward, backward] {
            assert!(projectile_unit_hit(&engine, hero.handle, segment).is_none());
        }
        assert_eq!(
            capture_building(&mut engine, hero).unwrap(),
            Some(OriginalObjectId(0))
        );
        let control = super::control(&engine.loaded.as_ref().unwrap().mission_assets, 0).unwrap();
        assert_eq!(authored_rate(control), Some(0.2));
        frame(&mut engine, 1).unwrap();
        frame(&mut engine, 501).unwrap();
        let blend = |engine: &crate::Engine| {
            engine
                .loaded
                .as_ref()
                .unwrap()
                .capture_barrier
                .as_ref()
                .unwrap()
                .blend
        };
        assert_eq!(blend(&engine), 0.1);
        frame(&mut engine, 5001).unwrap();
        assert_eq!(blend(&engine), 1.0);
        for segment in [forward, backward] {
            let hit = projectile_unit_hit(&engine, hero.handle, segment).expect("field contact");
            assert_eq!(hit.node, Some(ProjectileNode::CaptureBarrier));
            assert_eq!(
                apply_projectile_contact(&mut engine, hit, f32::MAX).unwrap(),
                None
            );
        }
        pose.position[1] = (center[1] + 0.2).to_bits();
        set_transform(&mut engine.world, hero.handle, pose).unwrap();
        frame(&mut engine, 5501).unwrap();
        assert_eq!(blend(&engine), 0.9);
        frame(&mut engine, 10001).unwrap();
        for segment in [forward, backward] {
            assert!(projectile_unit_hit(&engine, hero.handle, segment).is_none());
        }
    }
}
