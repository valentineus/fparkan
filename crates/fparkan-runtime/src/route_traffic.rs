//! Temporary ground occupancy during one autonomous movement step.

use crate::{
    autodemo::MOBILE_UNIT, projectile::unit_sphere, route_obstacles::Footprint, LoadedMissionState,
    MissionAssets, ObjectHandle, TransformState, World,
};
use fparkan_world::{handle_by_original_id, transform_state};

struct Body {
    handle: ObjectHandle,
    radius: f32,
    swept: Footprint,
    moved: bool,
}

#[derive(Default)]
pub(super) struct FrameTraffic {
    bodies: Vec<Body>,
}

fn body_radius(assets: &MissionAssets, index: usize, pose: TransformState) -> Option<f32> {
    let (center, radius) = unit_sphere(assets, index, pose)?;
    let [x, y, _] = pose.position.map(f32::from_bits);
    let radius = radius + (center[0] - x).hypot(center[1] - y);
    (radius.is_finite() && radius > 0.0).then_some(radius)
}

impl FrameTraffic {
    pub(super) fn read(world: &World, mission: &LoadedMissionState) -> Self {
        let bodies = mission
            .object_drafts
            .iter()
            .enumerate()
            .filter_map(|(index, draft)| {
                (draft.unit_dat_header_flags? & MOBILE_UNIT != 0).then_some(())?;
                let handle = handle_by_original_id(world, draft.original_id?)?;
                let pose = transform_state(world, handle).ok()?;
                let [x, y, _] = pose.position.map(f32::from_bits);
                Some(Body {
                    handle,
                    radius: body_radius(&mission.mission_assets, index, pose)?,
                    swept: Footprint::point([x, y]),
                    moved: false,
                })
            })
            .collect();
        Self { bodies }
    }

    pub(super) fn radius(&self, handle: ObjectHandle) -> f32 {
        self.bodies
            .iter()
            .find(|body| body.handle == handle)
            .map_or(0.0, |body| body.radius)
    }

    /// None means the unit's starting point is inside a trajectory that was
    /// already accepted this frame. It can try again with a fresh snapshot.
    pub(super) fn blockers(
        &self,
        handle: ObjectHandle,
        from: [f32; 2],
        radius: f32,
    ) -> Option<Vec<Footprint>> {
        self.bodies
            .iter()
            .filter(|body| body.handle != handle)
            .map(|body| {
                let bounds = body.swept.expanded(radius + body.radius);
                (!body.moved || !bounds.contains(from)).then_some(bounds)
            })
            .collect()
    }

    pub(super) fn reserve(&mut self, handle: ObjectHandle, swept: Footprint) {
        if let Some(body) = self.bodies.iter_mut().find(|body| body.handle == handle) {
            if body.swept != swept {
                body.swept = swept;
                body.moved = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::route_motion::follow_route;
    use std::{collections::VecDeque, f32::consts::FRAC_PI_2};

    fn pose(xy: [f32; 2], yaw: f32) -> TransformState {
        TransformState {
            position: [xy[0], xy[1], 0.0].map(f32::to_bits),
            orientation: [0.0, 0.0, yaw].map(f32::to_bits),
            ..Default::default()
        }
    }

    fn xy(pose: TransformState) -> [f32; 2] {
        [
            f32::from_bits(pose.position[0]),
            f32::from_bits(pose.position[1]),
        ]
    }

    #[test]
    fn two_units_swap_positions_without_intersecting() {
        let handles = [1, 2].map(|slot| ObjectHandle {
            slot,
            generation: 1,
        });
        let destinations = [[7.0, 0.0], [-7.0, 0.0]];
        let mut poses = [
            pose(destinations[1], -FRAC_PI_2),
            pose(destinations[0], FRAC_PI_2),
        ];
        let mut routes = destinations.map(|target| VecDeque::from([target]));
        for _ in 0..250 {
            let mut traffic = FrameTraffic {
                bodies: handles
                    .into_iter()
                    .zip(poses)
                    .map(|(handle, pose)| Body {
                        handle,
                        radius: 0.5,
                        swept: Footprint::point(xy(pose)),
                        moved: false,
                    })
                    .collect(),
            };
            for i in 0..2 {
                let Some(blockers) = traffic.blockers(handles[i], xy(poses[i]), 0.5) else {
                    continue;
                };
                let (after, sweep) =
                    follow_route(poses[i], &mut routes[i], 3.0, 0.1, &[], &blockers, |_| {
                        Ok(Some(0.0))
                    })
                    .unwrap();
                poses[i] = after;
                traffic.reserve(handles[i], sweep);
            }
            let [[a, b], [c, d]] = poses.map(xy);
            assert!(
                (a - c).hypot(b - d) > 1.0,
                "units overlapped at {a},{b} and {c},{d}"
            );
            if routes.iter().all(VecDeque::is_empty) {
                break;
            }
        }
        assert_eq!(poses.map(xy), destinations, "remaining routes: {routes:?}");
    }

    #[test]
    fn later_unit_avoids_the_first_units_crossing_during_a_long_frame() {
        let [first, second] = [1, 2].map(|slot| ObjectHandle {
            slot,
            generation: 1,
        });
        let start = [pose([-4.0, 0.0], -FRAC_PI_2), pose([0.0, -4.0], 0.0)];
        let mut traffic = FrameTraffic {
            bodies: [first, second]
                .into_iter()
                .zip(start)
                .map(|(handle, at)| Body {
                    handle,
                    radius: 0.5,
                    swept: Footprint::point(xy(at)),
                    moved: false,
                })
                .collect(),
        };
        let flat = |_| Ok(Some(0.0));
        let first_blockers = traffic.blockers(first, xy(start[0]), 0.5).unwrap();
        let (after, swept) = follow_route(
            start[0],
            &mut VecDeque::from([[4.0, 0.0]]),
            10.0,
            1.0,
            &[],
            &first_blockers,
            flat,
        )
        .unwrap();
        assert_eq!(xy(after), [4.0, 0.0]);
        traffic.reserve(first, swept);
        let second_blockers = traffic.blockers(second, xy(start[1]), 0.5).unwrap();
        let mut route = VecDeque::from([[0.0, 4.0]]);
        let (after, _) =
            follow_route(start[1], &mut route, 10.0, 1.0, &[], &second_blockers, flat).unwrap();
        assert!(xy(after)[1] < -1.0);
        assert_eq!(route.back(), Some(&[0.0, 4.0]));
    }

    #[test]
    fn stopped_vehicle_does_not_erase_an_occupied_destination() {
        let position = [0.0, 0.0];
        let target = [0.0, 5.0];
        let mut route = VecDeque::from([target]);
        let start = pose(position, 0.0);
        let bounds = Footprint::point(target).expanded(1.0);
        let flat = |_| Ok(Some(0.0));
        let (waiting, _) =
            follow_route(start, &mut route, 10.0, 2.0, &[], &[bounds], flat).unwrap();
        assert!(xy(waiting)[1] > 3.0);
        assert!(!bounds.contains(xy(waiting)));
        assert_eq!(route, [target]);
        let (arrived, _) = follow_route(waiting, &mut route, 10.0, 2.0, &[], &[], flat).unwrap();
        assert_eq!(xy(arrived), target);
        assert!(route.is_empty());
    }
}
