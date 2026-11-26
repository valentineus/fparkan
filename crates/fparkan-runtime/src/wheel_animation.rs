//! Distance-driven playback of authored wheel roll loops.

use crate::{ObjectHandle, TransformState};
use fparkan_assets::{PreparedComponentControl, PreparedControlTimelineRow};
use fparkan_msh::{node38_metadata, selected_slot, Group, Lod, ModelAsset, NodeId};
use std::f32::consts::TAU;

#[derive(Clone)]
struct Wheel {
    row: PreparedControlTimelineRow,
    radius: f32,
    phase: f32,
}

/// One chassis instance's wheel animation. The world transform drives its
/// phase, independently of the material and effect clocks.
#[derive(Clone)]
pub struct WheelAnimation {
    wheels: Vec<Wheel>,
    previous: Option<(ObjectHandle, [f32; 2])>,
}

impl WheelAnimation {
    /// Selects enabled wrapping controller rows whose mapped node keys form
    /// one complete spin around a stationary local axle.
    pub fn prepare(control: &PreparedComponentControl, model: &ModelAsset) -> Option<Self> {
        let wheels: Vec<_> = control
            .timeline_rows
            .iter()
            .copied()
            .filter_map(|row| {
                if row.flags & 5 != 1
                    || !row.initial_blend.is_finite()
                    || !control.typed_records.iter().any(|record| {
                        record.tag == 3 && record.values.contains(&(row.row_index as u32))
                    })
                {
                    return None;
                }
                Some(Wheel {
                    row,
                    radius: wheel_radius(model, row)?,
                    phase: row.initial_blend.rem_euclid(1.0),
                })
            })
            .collect();
        (!wheels.is_empty()).then_some(Self {
            wheels,
            previous: None,
        })
    }

    /// Applies the part of the recent displacement along the chassis's
    /// forward axis. Repeated snapshots and idle turns spend no distance.
    pub fn sample(
        &mut self,
        handle: ObjectHandle,
        pose: TransformState,
        times: &mut [Option<f32>],
    ) {
        let [x, y, _] = pose.position.map(f32::from_bits);
        let previous = self.previous.replace((handle, [x, y]));
        let scale = pose
            .scale
            .map(|bits| f32::from_bits(bits).abs())
            .into_iter()
            .fold(0.0, f32::max);
        let yaw = f32::from_bits(pose.orientation[2]);
        for wheel in &mut self.wheels {
            match previous {
                Some((owner, [old_x, old_y])) if owner == handle => {
                    let distance = (old_x - x) * yaw.sin() + (y - old_y) * yaw.cos();
                    let revolutions = distance / (TAU * wheel.radius * scale);
                    if revolutions.is_finite() {
                        wheel.phase = (wheel.phase + revolutions).rem_euclid(1.0);
                    }
                }
                _ => wheel.phase = wheel.row.initial_blend.rem_euclid(1.0),
            }
            if let Some(time) = times.get_mut(wheel.row.node_index) {
                let phase = if wheel.row.flags & 2 != 0 {
                    1.0 - wheel.phase
                } else {
                    wheel.phase
                };
                *time = Some(wheel.row.frame_a + phase * (wheel.row.frame_b - wheel.row.frame_a));
            }
        }
    }
}

fn wheel_radius(model: &ModelAsset, row: PreparedControlTimelineRow) -> Option<f32> {
    let animation = model.animation.as_ref()?;
    let [a, b] = [row.frame_a, row.frame_b];
    if ![a, b]
        .into_iter()
        .all(|frame| frame.is_finite() && frame >= 0.0 && frame.fract() == 0.0)
        || b - a < 3.0
        || b >= animation.frame_count as f32
    {
        return None;
    }
    let node = NodeId(u32::try_from(row.node_index).ok()?);
    let offset = usize::from(node38_metadata(model, node)?.anim_map_start);
    if offset == usize::from(u16::MAX) {
        return None;
    }
    let map = animation
        .frame_map
        .get(offset + a as usize..=offset + b as usize)?;
    let first = animation.keys.get(map[0] as usize)?.sampling_pose();
    let mut previous = first.rotation;
    let mut axis = None;
    let target_step = TAU / (b - a);
    for &key in &map[1..] {
        let pose = animation.keys.get(key as usize)?.sampling_pose();
        if pose
            .translation
            .into_iter()
            .zip(first.translation)
            .any(|(a, b)| (a - b).abs() > 0.0001)
        {
            return None;
        }
        let dot: f32 = previous
            .into_iter()
            .zip(pose.rotation)
            .map(|(a, b)| a * b)
            .sum();
        if (2.0 * dot.abs().min(1.0).acos() - target_step).abs() > 0.01 {
            return None;
        }
        // Imaginary part of conjugate(previous) * next, in slot-local axes.
        let next = pose.rotation;
        let relative: [f32; 3] = std::array::from_fn(|i| {
            let j = (i + 1) % 3;
            let k = (i + 2) % 3;
            (previous[3] * next[i] - next[3] * previous[i] - previous[j] * next[k]
                + previous[k] * next[j])
                * dot.signum()
        });
        let (i, value) = relative
            .into_iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))?;
        if relative
            .iter()
            .enumerate()
            .any(|(other, v)| i != other && v.abs() > 0.01)
            || axis.is_some_and(|last| last != (i, value.is_sign_positive()))
        {
            return None;
        }
        axis = Some((i, value.is_sign_positive()));
        previous = next;
    }
    let axle = axis?.0;
    let bounds = model
        .slots
        .get(selected_slot(model, node, Lod(0), Group(0))?.0 as usize)?;
    let radius = (0..3)
        .filter(|&i| i != axle)
        .map(|i| (bounds.aabb_max[i] - bounds.aabb_min[i]) * 0.5)
        .fold(0.0, f32::max);
    (radius.is_finite() && radius > 0.0).then_some(radius)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_travel_respects_scale_reverse_and_lifetime() {
        let handle = ObjectHandle {
            slot: 7,
            generation: 2,
        };
        let wheel = Wheel {
            row: PreparedControlTimelineRow {
                row_index: 0,
                node_index: 0,
                frame_a: 2.0,
                frame_b: 6.0,
                initial_blend: 0.0,
                raw_parameters: [0; 4],
                flags: 3,
            },
            radius: 2.0,
            phase: 0.0,
        };
        let mut animation = WheelAnimation {
            wheels: vec![wheel],
            previous: None,
        };
        let mut pose = TransformState {
            scale: [2.0_f32.to_bits(); 3],
            ..Default::default()
        };
        let mut times = [None, Some(12.0)];
        animation.sample(handle, pose, &mut times);
        assert_eq!(times, [Some(6.0), Some(12.0)]);
        pose.position[1] = TAU.to_bits();
        animation.sample(handle, pose, &mut times);
        assert_eq!(times, [Some(5.0), Some(12.0)]);
        pose.position[2] = 10.0_f32.to_bits();
        animation.sample(handle, pose, &mut times);
        assert_eq!(times[0], Some(5.0));
        pose.position[1] = 0;
        animation.sample(handle, pose, &mut times);
        assert_eq!(times[0], Some(6.0));
        pose.position[1] = TAU.to_bits();
        animation.sample(
            ObjectHandle {
                generation: 3,
                ..handle
            },
            pose,
            &mut times,
        );
        assert_eq!(times[0], Some(6.0));
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_wheeled_chassis_samples_rolling_msh_keys() {
        use crate::{
            create, load_gameplay_mission, loaded_mission_assets, loaded_mission_object_drafts,
            EngineServices, MissionMode, MissionRequest, MissionStartupInput,
        };
        use fparkan_animation::AnimationTime;
        use fparkan_msh::node38_sampled_hierarchy_at_times;
        use fparkan_vfs::DirectoryVfs;
        use std::sync::Arc;

        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed corpus");
        let mut engine = create(EngineServices::new(Arc::new(DirectoryVfs::new(root)))).unwrap();
        load_gameplay_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/Autodemo.00/data.tma".into(),
                mode: MissionMode::AutoDemo,
            },
            MissionStartupInput {
                queue_time_ms: 0,
                research_creation_gate: false,
            },
            None,
        )
        .unwrap();
        let assets = loaded_mission_assets(&engine).unwrap();
        let drafts = loaded_mission_object_drafts(&engine).unwrap();
        let (index, draft) = drafts
            .iter()
            .enumerate()
            .find(|(_, draft)| draft.resource_name_raw.ends_with(b"w_s_wel1.dat"))
            .unwrap();
        let control = assets.object_component_controls[index][0].as_ref().unwrap();
        let visual = assets
            .visual_by_id(assets.visual_for_object(index).unwrap())
            .unwrap();
        let model = &assets
            .model_by_id(visual.model_id.unwrap())
            .unwrap()
            .validated;
        let mut wheels = WheelAnimation::prepare(control, model).unwrap();
        assert_eq!(wheels.wheels.len(), 4);
        let wheel = &wheels.wheels[0];
        let radius = wheel.radius;
        let node = wheel.row.node_index;
        let handle = ObjectHandle {
            slot: 1,
            generation: 1,
        };
        let mut pose = TransformState {
            scale: draft.scale.map(f32::to_bits),
            ..Default::default()
        };
        let mut times = vec![Some(0.0); model.node_count];
        let sample = |times: &[Option<f32>]| {
            let values: Vec<_> = times.iter().map(|t| t.map(AnimationTime)).collect();
            node38_sampled_hierarchy_at_times(model, &values)
                .unwrap()
                .poses[node]
        };
        wheels.sample(handle, pose, &mut times);
        let start = sample(&times);
        pose.position[1] =
            (radius * TAU * 0.25 * draft.scale.into_iter().map(f32::abs).fold(0.0, f32::max))
                .to_bits();
        wheels.sample(handle, pose, &mut times);
        let turned = sample(&times);
        let dot: f32 = start
            .rotation
            .into_iter()
            .zip(turned.rotation)
            .map(|(a, b)| a * b)
            .sum();
        assert!((dot.abs() - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.0001);
        wheels.sample(handle, pose, &mut times);
        assert_eq!(sample(&times), turned);
    }
}
