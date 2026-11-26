//! Play a closed authored stride in place, using locomotion from the world.

use crate::{ObjectHandle, TransformState};
use fparkan_animation::{NodePoseBuffer, Pose};
use fparkan_msh::{node38_sampled_hierarchy, ModelAsset};
use fparkan_world::Tick;
use std::sync::Arc;

/// Per-instance playback of a walking chassis's first closed forward stride.
#[derive(Clone)]
pub struct WalkAnimation {
    clip: Arc<Stride>,
    distance: f32,
    moving: bool,
    previous: Option<(ObjectHandle, Tick, [f32; 2])>,
}

struct Stride {
    first_frame: u16,
    rest_forward: f32,
    advances: Vec<f32>,
}

impl WalkAnimation {
    /// Finds a complete stride in a model with forward root motion. Call only
    /// for models whose chassis profile explicitly declares walking.
    pub fn prepare(model: &ModelAsset) -> Option<Self> {
        let clip = Stride::prepare(model)?;
        Some(Self {
            clip: Arc::new(clip),
            distance: 0.0,
            moving: false,
            previous: None,
        })
    }

    /// A new tick advances the stride by actual forward displacement. A
    /// retained snapshot keeps the same pose, including during pause.
    pub fn sample(
        &mut self,
        handle: ObjectHandle,
        tick: Tick,
        pose: TransformState,
    ) -> Option<f32> {
        let [x, y, _] = pose.position.map(f32::from_bits);
        match self.previous {
            Some((owner, frame, [old_x, old_y])) if owner == handle => {
                if tick != frame {
                    let yaw = f32::from_bits(pose.orientation[2]);
                    let forward = (old_x - x) * yaw.sin() + (y - old_y) * yaw.cos();
                    let advance = forward / f32::from_bits(pose.scale[1]).abs();
                    self.moving = advance.is_finite() && advance > 0.0;
                    self.distance = if self.moving {
                        let period = f64::from(*self.clip.advances.last()?);
                        (f64::from(self.distance) + f64::from(advance)).rem_euclid(period) as f32
                    } else {
                        0.0
                    };
                    self.previous = Some((handle, tick, [x, y]));
                }
            }
            _ => {
                self.previous = Some((handle, tick, [x, y]));
                self.moving = false;
                self.distance = 0.0;
            }
        }
        self.moving.then(|| self.clip.frame_at(self.distance))
    }

    /// Removes baked forward translation after sampling, so world movement
    /// owns the distance travelled. Local bob and sway remain in the pose.
    pub fn remove_root_progress(&self, hierarchy: &mut NodePoseBuffer) {
        if let Some(root) = hierarchy.poses.first() {
            let displacement = root.translation[1] - self.clip.rest_forward;
            for pose in &mut hierarchy.poses {
                pose.translation[1] -= displacement;
            }
        }
    }
}

impl Stride {
    fn prepare(model: &ModelAsset) -> Option<Self> {
        let frames = u16::try_from(model.animation.as_ref()?.frame_count).ok()?;
        let rest = node38_sampled_hierarchy(model, 0)?.poses;
        let rest_forward = rest.first()?.translation[1];
        let radius = model.bounding_sphere_radius;
        if !radius.is_finite() || radius <= 0.0 {
            return None;
        }
        let tolerance = (radius * 0.0001).powi(2);
        let mut run = vec![rest];
        for frame in 1..frames {
            let poses = node38_sampled_hierarchy(model, frame)?.poses;
            if run.last()?.first()?.translation[1] >= poses.first()?.translation[1] {
                run.clear();
            }
            run.push(poses);
            let (last, prefix) = run.split_last()?;
            for (i, first) in prefix.iter().enumerate() {
                let span = prefix.len() - i;
                let forward = last[0].translation[1] - first[0].translation[1];
                if span < 4 || forward < radius || !same_stride_pose(first, last, tolerance) {
                    continue;
                }
                // Moving every bone rigidly together would preserve the
                // endpoints but would still be a sliding chassis.
                if !prefix[i + 1..]
                    .iter()
                    .any(|middle| !same_stride_pose(first, middle, tolerance))
                {
                    continue;
                }
                let advances = run[i..]
                    .iter()
                    .map(|pose| pose[0].translation[1] - first[0].translation[1])
                    .collect();
                return Some(Self {
                    first_frame: frame - span as u16,
                    rest_forward,
                    advances,
                });
            }
        }
        None
    }

    fn frame_at(&self, distance: f32) -> f32 {
        let end = self
            .advances
            .partition_point(|&advance| advance <= distance)
            .clamp(1, self.advances.len() - 1);
        let start = end - 1;
        let fraction =
            (distance - self.advances[start]) / (self.advances[end] - self.advances[start]);
        f32::from(self.first_frame) + start as f32 + fraction
    }
}

fn same_stride_pose(first: &[Pose], second: &[Pose], tolerance_squared: f32) -> bool {
    let forward = second[0].translation[1] - first[0].translation[1];
    first.iter().zip(second).all(|(a, b)| {
        let [x, y, z] = a.translation;
        let [nx, ny, nz] = b.translation;
        let shift = (x - nx).powi(2) + (y + forward - ny).powi(2) + (z - nz).powi(2);
        let angle: f32 = a
            .rotation
            .into_iter()
            .zip(b.rotation)
            .map(|(a, b)| a * b)
            .sum();
        shift <= tolerance_squared && angle.abs() >= 0.99999
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stride_uses_world_displacement_and_keeps_phase_on_repeated_ticks() {
        let mut walk = WalkAnimation {
            clip: Arc::new(Stride {
                first_frame: 10,
                rest_forward: 0.0,
                advances: vec![0.0, 1.0, 3.0],
            }),
            distance: 0.0,
            moving: false,
            previous: None,
        };
        let handle = ObjectHandle {
            slot: 0,
            generation: 1,
        };
        let pose = |y| TransformState {
            position: [0.0, y, 0.0].map(f32::to_bits),
            scale: [1.0, 2.0, 1.0].map(f32::to_bits),
            ..TransformState::default()
        };
        assert_eq!(walk.sample(handle, Tick(0), pose(0.0)), None);
        assert_eq!(walk.sample(handle, Tick(1), pose(4.0)), Some(11.5));
        assert_eq!(walk.sample(handle, Tick(1), pose(4.0)), Some(11.5));
        assert_eq!(walk.sample(handle, Tick(2), pose(6.0)), Some(10.0));
        assert_eq!(walk.sample(handle, Tick(3), pose(6.0)), None);
        let replacement = ObjectHandle {
            generation: 2,
            ..handle
        };
        assert_eq!(walk.sample(replacement, Tick(4), pose(12.0)), None);
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn stock_walkers_have_closed_forward_strides() {
        use fparkan_assets::decode_nres_payload;
        use fparkan_msh::{decode_msh, validate_msh};

        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed corpus");
        let bases = std::path::PathBuf::from(root).join("bases.rlb");
        let bases = decode_nres_payload(std::fs::read(bases).unwrap().into()).unwrap();
        for (name, first, last, distance) in [
            ("r_l_01.msh", 10, 18, 5.118131),
            ("r_m_01.msh", 16, 24, 7.68),
            ("r_b_01.msh", 29, 41, 12.8),
        ] {
            let bytes = bases.payload(bases.find(name).unwrap()).unwrap();
            let msh = decode_nres_payload(bytes.to_vec().into()).unwrap();
            let model = validate_msh(&decode_msh(&msh).unwrap()).unwrap();
            let walk = WalkAnimation::prepare(&model).unwrap();
            assert_eq!(walk.clip.first_frame, first, "{name}");
            assert_eq!(
                walk.clip.advances.len(),
                usize::from(last - first + 1),
                "{name}"
            );
            assert!(
                (walk.clip.advances.last().unwrap() - distance).abs() < 0.001,
                "{name}"
            );

            let mut start = node38_sampled_hierarchy(&model, first).unwrap();
            let mut middle = node38_sampled_hierarchy(&model, (first + last) / 2).unwrap();
            let mut end = node38_sampled_hierarchy(&model, last).unwrap();
            for hierarchy in [&mut start, &mut middle, &mut end] {
                walk.remove_root_progress(hierarchy);
                assert!(
                    (hierarchy.poses[0].translation[1] - walk.clip.rest_forward).abs() < 0.0001
                );
            }
            assert!(same_stride_pose(&start.poses, &end.poses, 1e-6), "{name}");
            assert!(
                !same_stride_pose(&start.poses, &middle.poses, 1e-6),
                "{name}"
            );
        }
    }
}
