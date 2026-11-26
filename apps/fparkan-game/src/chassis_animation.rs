//! Binds locomotion playback to the existing mesh pose update.

use super::{
    live_root_pose, ControlAnimationInstance, MaterialAnimationBinding, MissionRenderRoot,
    StaticPreviewScene,
};
use fparkan_material::MaterialAnimationMode;
use fparkan_runtime::{
    loaded_mission_assets, loaded_mission_control_device_tables, loaded_mission_object_drafts,
    loaded_tracked_chassis, loaded_walk_chassis, Engine, TreadAnimation, WalkAnimation,
};
use fparkan_world::WorldSnapshot;
use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroU16;

pub(super) struct TreadMaterials {
    object_index: usize,
    animation: TreadAnimation,
    bindings: Vec<(usize, NonZeroU16)>,
}

pub(super) fn connect_treads(engine: &Engine, scene: &mut StaticPreviewScene) {
    let Some(drafts) = loaded_mission_object_drafts(engine) else {
        return;
    };
    let Some(tables) = loaded_mission_control_device_tables(engine) else {
        return;
    };
    let mut owners = BTreeMap::<usize, TreadMaterials>::new();
    for (index, binding) in scene.material_animations.iter().enumerate() {
        let mut ranges = binding.variants.iter().flat_map(|variant| {
            variant
                .range_indices
                .iter()
                .filter_map(|&i| scene.mesh.draw_ranges.get(i))
        });
        let Some(first) = ranges.next() else { continue };
        if first.owner_scope == 0
            || first.batch_flags & 4 == 0
            || ranges
                .any(|range| range.owner_scope != first.owner_scope || range.batch_flags & 4 == 0)
        {
            continue;
        }
        let object_index = (first.owner_scope - 1) as usize;
        if !loaded_tracked_chassis(engine, object_index)
            || binding.base.frozen_phase_index.is_some()
        {
            continue;
        }
        let Some(period) = binding
            .base
            .document
            .animation_blocks
            .get(binding.base.animation_block_index)
            .filter(|block| block.mode() == MaterialAnimationMode::Loop && block.keys.len() > 1)
            .and_then(|block| NonZeroU16::new(block.keys.last()?.k1))
        else {
            continue;
        };
        let Some(id) = drafts.get(object_index).and_then(|draft| draft.original_id) else {
            continue;
        };
        let Some(animation) = tables
            .iter()
            .find(|table| table.original_id == Some(id))
            .and_then(|table| table.owner_profile.as_ref())
            .and_then(|profile| TreadAnimation::new(f32::from_bits(profile.original_words[7])))
        else {
            continue;
        };
        owners
            .entry(object_index)
            .or_insert_with(|| TreadMaterials {
                object_index,
                animation,
                bindings: Vec::new(),
            })
            .bindings
            .push((index, period));
    }
    scene.tread_animations = owners.into_values().collect();
}

pub(super) fn update_treads(
    owners: &mut [TreadMaterials],
    materials: &mut [MaterialAnimationBinding],
    roots: &[Option<MissionRenderRoot>],
    snapshot: &WorldSnapshot,
) {
    for owner in owners {
        let Some(root) = roots.get(owner.object_index).and_then(Option::as_ref) else {
            continue;
        };
        let Some(pose) = live_root_pose(root.handle, snapshot) else {
            continue;
        };
        owner.animation.sample(root.handle, pose);
        for &(binding, period) in &owner.bindings {
            materials[binding].normalized_phase_override = Some(owner.animation.phase(period));
        }
    }
}

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
