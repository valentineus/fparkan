//! Binds locomotion playback to the existing mesh pose update.

use super::{live_root_pose, ControlAnimationInstance, MissionRenderRoot, StaticPreviewScene};
use fparkan_runtime::{loaded_mission_assets, loaded_walk_chassis, Engine, WalkAnimation};
use fparkan_world::WorldSnapshot;
use std::collections::HashMap;

pub(super) fn connect_walkers(engine: &Engine, scene: &mut StaticPreviewScene) {
    let Some(assets) = loaded_mission_assets(engine) else {
        return;
    };
    let mut clips = HashMap::new();
    for (index, components) in scene.control_animation_instances.iter_mut().enumerate() {
        if !loaded_walk_chassis(engine, index) {
            continue;
        }
        let Some(root) = components.first_mut().and_then(Option::as_mut) else {
            continue;
        };
        if !root.control.bindings.is_empty() {
            continue;
        }
        let Some(model_id) = assets
            .visual_for_object(index)
            .and_then(|id| assets.visual_by_id(id))
            .and_then(|visual| visual.model_id)
        else {
            continue;
        };
        root.walk = clips
            .entry(model_id)
            .or_insert_with(|| WalkAnimation::prepare(&root.model))
            .clone();
        if root.walk.is_none() {
            continue;
        }
        for geometry in scene.dynamic_component_geometry[index].iter_mut().flatten() {
            geometry.animated = true;
        }
        for caster in scene
            .dynamic_shadow_casters
            .iter_mut()
            .filter(|caster| caster.object_index == index)
        {
            caster.animated = true;
        }
    }
}

pub(super) fn sample(
    instance: &mut ControlAnimationInstance,
    root: &MissionRenderRoot,
    snapshot: &WorldSnapshot,
    node_times: &mut [Option<f32>],
) -> bool {
    if instance.wheels.is_none() && instance.walk.is_none() {
        return false;
    }
    let Some(pose) = live_root_pose(root.handle, snapshot) else {
        return false;
    };
    if let Some(wheels) = &mut instance.wheels {
        wheels.sample(root.handle, pose, node_times);
    }
    let Some(frame) = instance
        .walk
        .as_mut()
        .and_then(|walk| walk.sample(root.handle, snapshot.tick, pose))
    else {
        return false;
    };
    node_times.fill(Some(frame));
    true
}
