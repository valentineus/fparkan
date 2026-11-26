//! CPU poses for mounted weapon CPT points, independent of the renderer.

use crate::{
    turret_aim::{apply_autonomous_turret_pose, has_turret_aim, TurretAim},
    MissionAssets, MissionObjectDraft,
};
use fparkan_animation::{AnimationTime, NodePoseBuffer, Pose};
use fparkan_assets::{
    sample_control_node_times, ControlInstanceState, PreparedControlGateProvider,
    PreparedControlNodeBinding, PreparedControlPoint,
};
use fparkan_msh::{node38_fallback_hierarchy, node38_sampled_hierarchy_at_times};
use fparkan_prototype::{unit_component_tree, PROTOTYPE_TYPE_EXTO};
use fparkan_render::LegacyIron3dEulerTransform;
use fparkan_world::TransformState;
use std::collections::BTreeMap;

/// Unresolved autonomous weapon poses, grouped by mission object and component.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WeaponPoseReport {
    /// Samples using authored initial bindings because a live Control track is unsupported.
    pub fallbacks: BTreeMap<(u32, usize), (u64, String)>,
    /// Trigger samples or due shots without a resolvable CPT mount chain.
    pub missing: BTreeMap<u32, u64>,
}

#[derive(Clone, Copy)]
enum Mount {
    Root,
    Socket(usize, usize),
    Unavailable,
}

#[derive(Clone)]
pub(super) struct WeaponPoses {
    mounts: Vec<Mount>,
    states: Vec<ControlInstanceState>,
    hierarchies: Vec<Option<NodePoseBuffer>>,
    aims: Vec<Option<TurretAim>>,
}

impl WeaponPoses {
    pub(super) fn new(draft: &MissionObjectDraft, assets: &MissionAssets, index: usize) -> Self {
        let visuals = assets.visuals_for_object(index);
        let tree = unit_component_tree(&draft.unit_components).ok();
        let visible = |component| {
            component == 0
                || visuals
                    .get(component)
                    .and_then(|&id| assets.visual_by_id(id))
                    .is_some_and(|visual| visual.source_type == Some(PROTOTYPE_TYPE_EXTO))
        };
        let mounts = (0..visuals.len())
            .map(|component| {
                if draft.unit_components.is_empty() || component == 0 {
                    return Mount::Root;
                }
                let socket = || {
                    let parent = tree.as_ref()?.get(component)?.parent_index?;
                    let socket =
                        usize::try_from(draft.unit_components[component].parent_or_link).ok()?;
                    (visible(component) && visible(parent)).then_some(Mount::Socket(parent, socket))
                };
                socket().unwrap_or(Mount::Unavailable)
            })
            .collect();
        Self {
            mounts,
            states: vec![ControlInstanceState::default(); visuals.len()],
            hierarchies: vec![None; visuals.len()],
            aims: vec![None; visuals.len()],
        }
    }

    pub(super) fn sample(
        &mut self,
        assets: &MissionAssets,
        index: usize,
        object: u32,
        now: u32,
        hull_local_target: Option<[f32; 3]>,
        report: &mut WeaponPoseReport,
    ) {
        for (component, &visual) in assets.visuals_for_object(index).iter().enumerate() {
            let model = assets
                .visual_by_id(visual)
                .and_then(|visual| visual.model_id)
                .and_then(|id| assets.model_by_id(id));
            let Some(model) = model else { continue };
            let control = assets
                .object_component_controls
                .get(index)
                .and_then(|controls| controls.get(component))
                .and_then(Option::as_ref);
            let aim = control
                .filter(|control| has_turret_aim(control))
                .and_then(|_| self.aim_at(component, hull_local_target?));
            self.aims[component] = aim;
            let times = control.and_then(|control| {
                // Match the renderer's query gates. Recoil still uses the
                // initial Control providers.
                let gates = control.typed_records.iter().filter(|record| record.tag == 21)
                    .map(|record| match record.gate_provider {
                        PreparedControlGateProvider::Resolved { initial_type1_value, .. }
                            if initial_type1_value.is_finite() => Some(initial_type1_value),
                        _ => None,
                    }).collect::<Vec<_>>();
                let mut times = match sample_control_node_times(control, now, &gates, &mut self.states[component]) {
                    Ok(times) => times,
                    Err(error) => {
                        let (count, reason) = report.fallbacks.entry((object, component)).or_default();
                        if *count == 0 {
                            eprintln!("unit_weapon_pose_fallback object={object} component={component} reason={error:?}");
                            *reason = format!("{error:?}");
                        }
                        *count += 1;
                        initial_component_node_times(model.validated.node_count, &control.bindings).ok()?
                            .into_iter().map(|time| time.map(|time| time.0)).collect()
                    }
                };
                if let Some(aim) = aim {
                    apply_autonomous_turret_pose(control, aim, &mut times);
                }
                Some(times.into_iter().map(|time| time.map(AnimationTime)).collect())
            });
            let times = times.or_else(|| {
                control
                    .is_none()
                    .then(|| vec![Some(AnimationTime(0.0)); model.validated.node_count])
            });
            self.hierarchies[component] = times
                .as_ref()
                .and_then(|times| node38_sampled_hierarchy_at_times(&model.validated, times))
                .or_else(|| node38_fallback_hierarchy(&model.validated));
        }
    }

    fn aim_at(&self, component: usize, target: [f32; 3]) -> Option<TurretAim> {
        let local = match *self.mounts.get(component)? {
            Mount::Root => target,
            Mount::Unavailable => return None,
            Mount::Socket(parent, socket) => {
                let to_hull =
                    |point| mounted_point(&self.hierarchies, &self.mounts, parent, socket, point);
                let origin = to_hull([0.0; 3])?;
                let project = |axis| {
                    let end = to_hull(axis)?;
                    Some(
                        (0..3)
                            .map(|i| (end[i] - origin[i]) * (target[i] - origin[i]))
                            .sum(),
                    )
                };
                [
                    project([1.0, 0.0, 0.0])?,
                    project([0.0, 1.0, 0.0])?,
                    project([0.0, 0.0, 1.0])?,
                ]
            }
        };
        TurretAim::toward(local)
    }

    pub(super) fn aim(&self, component: usize) -> Option<TurretAim> {
        self.aims.get(component).copied().flatten()
    }

    pub(super) fn initial_model_spheres(
        &self,
        assets: &MissionAssets,
        index: usize,
    ) -> Vec<([f32; 3], f32)> {
        let models: Vec<_> = assets
            .visuals_for_object(index)
            .iter()
            .map(|&id| assets.model_by_id(assets.visual_by_id(id)?.model_id?))
            .collect();
        let controls = assets.object_component_controls.get(index);
        let hierarchies: Vec<_> = models
            .iter()
            .enumerate()
            .map(|(component, model)| {
                let model = &model.as_ref()?.validated;
                let bindings = controls
                    .and_then(|controls| controls.get(component)?.as_ref())
                    .map_or(&[][..], |control| control.bindings.as_slice());
                initial_component_node_times(model.node_count, bindings)
                    .ok()
                    .and_then(|times| node38_sampled_hierarchy_at_times(model, &times))
                    .or_else(|| node38_fallback_hierarchy(model))
            })
            .collect();
        models
            .into_iter()
            .zip(&self.mounts)
            .filter_map(|(model, mount)| {
                let model = &model?.validated;
                let center = model.bounding_sphere_center;
                let center = match *mount {
                    Mount::Root => center,
                    Mount::Socket(parent, socket) => {
                        mounted_point(&hierarchies, &self.mounts, parent, socket, center)?
                    }
                    Mount::Unavailable => return None,
                };
                Some((center, model.bounding_sphere_radius))
            })
            .collect()
    }

    #[cfg(test)]
    pub(super) fn has_aim(&self) -> bool {
        self.aims.iter().any(Option::is_some)
    }

    pub(super) fn muzzle(
        &self,
        state: TransformState,
        component: usize,
        barrel: &PreparedControlPoint,
    ) -> Option<[f32; 3]> {
        let node = usize::try_from(barrel.raw_signed_field_04).ok()?;
        let point = mounted_point(
            &self.hierarchies,
            &self.mounts,
            component,
            node,
            barrel.position,
        )?;
        LegacyIron3dEulerTransform {
            translation: state.position.map(f32::from_bits),
            orientation_radians: state.orientation.map(f32::from_bits),
        }
        .try_transform_scaled_point(point, state.scale.map(f32::from_bits))
    }
}

fn mounted_point(
    hierarchies: &[Option<NodePoseBuffer>],
    mounts: &[Mount],
    mut component: usize,
    mut node: usize,
    mut point: [f32; 3],
) -> Option<[f32; 3]> {
    // Components are a preorder tree, but bound traversal also rejects a bad cycle.
    for _ in 0..mounts.len() {
        let hierarchy = hierarchies.get(component)?.as_ref();
        let pose = match hierarchy {
            Some(hierarchy) => *hierarchy.poses.get(node)?,
            None if node == 0 => Pose::default(),
            None => return None,
        };
        point = pose.transform_point(point);
        match *mounts.get(component)? {
            Mount::Root => return Some(point),
            Mount::Unavailable => return None,
            Mount::Socket(parent, socket) => {
                if let Some(root) = hierarchy.and_then(|hierarchy| hierarchy.poses.first()) {
                    let [x, y, z, w] = root.rotation;
                    let length_squared = x * x + y * y + z * z + w * w;
                    let rotation = if length_squared.is_finite() && length_squared > f32::EPSILON {
                        [
                            -x / length_squared,
                            -y / length_squared,
                            -z / length_squared,
                            w / length_squared,
                        ]
                    } else {
                        Pose::default().rotation
                    };
                    point = Pose {
                        translation: [0.0; 3],
                        rotation,
                    }
                    .transform_point(std::array::from_fn(|axis| {
                        point[axis] - root.translation[axis]
                    }));
                }
                component = parent;
                node = socket;
            }
        }
    }
    None
}

/// Samples the authored initial Control bindings in source order.
pub fn initial_component_node_times(
    node_count: usize,
    bindings: &[PreparedControlNodeBinding],
) -> Result<Vec<Option<AnimationTime>>, String> {
    let mut node_times = vec![Some(AnimationTime(0.0)); node_count];
    for binding in bindings {
        // Disabled rows are filtered while loading CTLD, but preserve the
        // native guard here in case a caller constructs MissionAssets itself.
        if binding.flags & 4 != 0 {
            continue;
        }
        if binding.node_index >= node_count {
            return Err(format!(
                "control binding node {} is outside model node count {node_count}",
                binding.node_index
            ));
        }
        if !binding.frame_a.is_finite()
            || !binding.frame_b.is_finite()
            || !binding.initial_blend.is_finite()
            || binding.frame_a < 0.0
            || binding.frame_b < 0.0
        {
            return Err(format!(
                "control binding for node {} contains an invalid frame or blend",
                binding.node_index
            ));
        }

        let mut blend = binding.initial_blend;
        if binding.flags & 1 != 0 {
            if blend > 1.0 {
                blend -= 1.0;
            }
            if blend < 0.0 {
                blend += 1.0;
            }
        } else {
            blend = blend.clamp(0.0, 1.0);
        }
        if binding.flags & 2 != 0 {
            blend = 1.0 - blend;
        }
        let sample_time = (1.0 - blend) * binding.frame_a + blend * binding.frame_b;
        if !sample_time.is_finite() || sample_time < 0.0 {
            return Err(format!(
                "control binding for node {} produced a negative or non-finite sample time unsupported by the static sampler",
                binding.node_index
            ));
        }
        // Source order is significant: native initialization writes each
        // enabled row in turn, so a later row for the same node wins.
        node_times[binding.node_index] = Some(AnimationTime(sample_time));
    }
    Ok(node_times)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpt_scale_and_world_transform_match_control_dll_query() {
        // GOG Control.dll RVA 0x1b4f0, native execution with a supplied node matrix.
        let poses = WeaponPoses {
            mounts: vec![Mount::Root],
            states: vec![],
            aims: vec![],
            hierarchies: vec![Some(NodePoseBuffer {
                poses: vec![Pose::default()],
            })],
        };
        let state = TransformState {
            position: [20.0_f32, 30.0, 40.0].map(f32::to_bits),
            orientation: [0.0_f32, 0.0, std::f32::consts::FRAC_PI_2].map(f32::to_bits),
            scale: [2.0_f32, 3.0, 4.0].map(f32::to_bits),
        };
        let barrel = PreparedControlPoint {
            raw_flags: 0,
            raw_signed_field_04: 0,
            raw_signed_field_08: 0,
            position: [1.0, 2.0, 3.0],
            direction: [0.0, 1.0, 0.0],
            name: [0; 32],
        };
        let actual = poses.muzzle(state, 0, &barrel).unwrap();
        for (actual, expected) in actual.into_iter().zip([14.0, 32.0, 52.0]) {
            assert!((actual - expected).abs() < 0.00001);
        }
    }

    #[test]
    fn mounted_muzzle_replaces_child_roots_and_follows_rotated_parent_socket() {
        let quarter_turn = [
            0.0,
            0.0,
            std::f32::consts::FRAC_1_SQRT_2,
            std::f32::consts::FRAC_1_SQRT_2,
        ];
        let translated = |translation| Pose {
            translation,
            ..Pose::default()
        };
        let poses = [
            Some(NodePoseBuffer {
                poses: vec![
                    Pose::default(),
                    Pose {
                        translation: [10.0, 0.0, 0.0],
                        rotation: quarter_turn,
                    },
                ],
            }),
            Some(NodePoseBuffer {
                poses: vec![
                    Pose {
                        translation: [100.0, 0.0, 0.0],
                        rotation: quarter_turn,
                    },
                    Pose {
                        translation: [96.0, 0.0, 0.0],
                        rotation: quarter_turn,
                    },
                ],
            }),
            Some(NodePoseBuffer {
                poses: vec![translated([0.0, 200.0, 0.0]), translated([0.0, 200.0, 3.0])],
            }),
        ];
        let mounts = [Mount::Root, Mount::Socket(0, 1), Mount::Socket(1, 1)];
        let point = mounted_point(&poses, &mounts, 2, 1, [0.0, 2.0, 0.0]).unwrap();
        for (actual, expected) in point.into_iter().zip([4.0, 0.0, 3.0]) {
            assert!((actual - expected).abs() < 0.00001, "{point:?}");
        }
        assert!(mounted_point(&poses, &mounts, 2, 9, [0.0; 3]).is_none());
        assert!(mounted_point(
            &poses,
            &[Mount::Root, Mount::Unavailable, mounts[2]],
            2,
            1,
            [0.0; 3]
        )
        .is_none());
    }
}
