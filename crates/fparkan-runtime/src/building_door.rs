use fparkan_assets::PreparedComponentControl;
use fparkan_msh::{selected_slot, Group, Lod, ModelAsset, NodeId};
use fparkan_render::LegacyIron3dEulerTransform;

/// Whether this controller has a door record with a supported node binding.
pub fn has_proximity_doors(control: &PreparedComponentControl) -> bool {
    door_bindings(control).next().is_some()
}

fn door_bindings(
    control: &PreparedComponentControl,
) -> impl Iterator<Item = &fparkan_assets::PreparedControlNodeBinding> {
    control
        .typed_records
        .iter()
        .filter(|record| record.tag == 12)
        .filter_map(|record| {
            let index = record.fixed_words[1] as usize;
            control
                .bindings
                .iter()
                .rev()
                .find(|binding| binding.node_index == index)
                .filter(|binding| binding.flags == 0)
        })
}

/// Updates the authored door tracks next to the player.
pub fn sample_proximity_doors(
    control: &PreparedComponentControl,
    model: &ModelAsset,
    root: LegacyIron3dEulerTransform,
    scale: [f32; 3],
    player: [f32; 3],
    delta_seconds: f32,
    blends: &mut Vec<f32>,
    times: &mut [Option<f32>],
) {
    // shortcut: use the closed model's door bounds and a local proximity trigger
    // until the original building graph trigger and its timing are recovered.
    let Some(closed) = fparkan_msh::node38_sampled_hierarchy(model, 0) else {
        return;
    };
    let previous_len = blends.len();
    blends.resize(model.node_count, 0.0);
    for binding in door_bindings(control) {
        let index = binding.node_index;
        let node = NodeId(index as u32);
        let Some(pose) = closed.poses.get(index) else {
            continue;
        };
        let Some(slot) = selected_slot(model, node, Lod(0), Group(0)) else {
            continue;
        };
        let slot = &model.slots[slot.0 as usize];
        let center = std::array::from_fn(|axis| (slot.aabb_min[axis] + slot.aabb_max[axis]) * 0.5);
        let Some(center) = root.try_transform_scaled_point(pose.transform_point(center), scale)
        else {
            continue;
        };
        let distance_squared: f32 = player
            .into_iter()
            .zip(center)
            .map(|(p, c)| (p - c).powi(2))
            .sum();
        let blend = &mut blends[index];
        if index >= previous_len {
            *blend = binding.initial_blend.clamp(0.0, 1.0);
        }
        let direction = if distance_squared < 8.0_f32.powi(2) {
            1.0
        } else {
            -1.0
        };
        *blend = (*blend + direction * delta_seconds * 2.0).clamp(0.0, 1.0);
        if let Some(time) = times.get_mut(index) {
            *time = Some(binding.frame_a + (binding.frame_b - binding.frame_a) * *blend);
        }
    }
}
