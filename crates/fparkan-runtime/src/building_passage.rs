//! Approaches and doors along a building's authored route to the capture point.

use crate::{
    building_door::{is_bound_door, DOOR_BLEND_PER_SECOND},
    projectile_parts::root_model,
    route_motion::{route_points, turn_toward, WAYPOINT_TOLERANCE},
    route_obstacles::{model_footprint, Footprint},
    MissionAssets, ObjectHandle, TerrainWorld,
};
use fparkan_assets::PreparedComponentControl;
use fparkan_msh::{node38_fallback_hierarchy, Hallway, HallwayLink};
use fparkan_render::LegacyIron3dEulerTransform;
use fparkan_terrain::SurfaceQuery;
use fparkan_world::TransformState;
use std::collections::VecDeque;

pub(super) struct CaptureEntrance {
    pub(super) staging: [f32; 2],
    pub(super) capture: [f32; 3],
    pub(super) legs: Option<VecDeque<PassageLeg>>,
}

#[derive(Clone)]
pub(super) struct PassageLeg {
    position: [f32; 3],
    door: Option<usize>,
}

#[derive(Clone)]
pub(super) struct CapturePassage {
    pub(super) building: ObjectHandle,
    pub(super) legs: VecDeque<PassageLeg>,
    pub(super) opening: Option<(usize, f32)>,
}

pub(super) fn capture_entrance(
    assets: &MissionAssets,
    terrain: &TerrainWorld,
    index: usize,
    pose: TransformState,
    from: [f32; 3],
    clearance: f32,
) -> Option<CaptureEntrance> {
    let model = &root_model(assets, index)?.validated;
    let hallway = &model.hallway;
    // Behavior::GoInside passes mask 0x40 to HallWay's first-match query.
    let goal = hallway
        .points
        .iter()
        .position(|point| point.flags & 0x40 != 0)?;
    let toward_capture = links_to(hallway, goal);
    let control = assets
        .object_component_controls
        .get(index)?
        .first()?
        .as_ref()?;
    let hierarchy = node38_fallback_hierarchy(model)?;
    let transform = LegacyIron3dEulerTransform {
        translation: pose.position.map(f32::from_bits),
        orientation_radians: pose.orientation.map(f32::from_bits),
    };
    let scale = pose.scale.map(f32::from_bits);
    let project = |index: usize| {
        let point = hallway.points.get(index)?;
        let node = hierarchy.poses.get(point.node.0 as usize)?;
        transform.try_transform_scaled_point(node.transform_point(point.position), scale)
    };
    let capture = project(goal)?;
    let footprint = model_footprint(assets, index, pose)?.expanded(clearance);
    hallway
        .points
        .iter()
        .enumerate()
        .filter(|(_, point)| point.flags & 0x1000_0001 == 1)
        .filter_map(|(index, _)| {
            let entrance = project(index)?;
            let inside = project(toward_capture[index]?.0)?;
            let staging = footprint.exterior_along(
                [entrance[0], entrance[1]],
                [entrance[0] - inside[0], entrance[1] - inside[1]],
            )?;
            let ground = terrain.height_at(staging).ok()??;
            route_points(terrain, from, [staging[0], staging[1], ground]).ok()??;
            let distance: f32 = (0..3).map(|i| (from[i] - entrance[i]).powi(2)).sum();
            let legs = passage_legs(index, goal, &toward_capture, hallway, control, project);
            Some((
                distance,
                CaptureEntrance {
                    staging,
                    capture,
                    legs,
                },
            ))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, entrance)| entrance)
}

fn links_to(hallway: &Hallway, goal: usize) -> Vec<Option<(usize, usize)>> {
    let mut next = vec![None; hallway.points.len()];
    next[goal] = Some((goal, usize::MAX));
    let mut queue = VecDeque::from([goal]);
    while let Some(point) = queue.pop_front() {
        for (index, link) in hallway.links.iter().enumerate() {
            let [a, b] = link.endpoints;
            let (a, b) = (a as usize, b as usize);
            let neighbor = if a == point {
                b
            } else if b == point {
                a
            } else {
                continue;
            };
            if let Some(slot @ None) = next.get_mut(neighbor) {
                *slot = Some((point, index));
                queue.push_back(neighbor);
            }
        }
    }
    next
}

fn passage_legs(
    mut point: usize,
    goal: usize,
    steps: &[Option<(usize, usize)>],
    hallway: &Hallway,
    control: &PreparedComponentControl,
    project: impl Fn(usize) -> Option<[f32; 3]>,
) -> Option<VecDeque<PassageLeg>> {
    let mut legs = VecDeque::from([PassageLeg {
        position: project(point)?,
        door: None,
    }]);
    while point != goal {
        let (next, link) = steps[point]?;
        let door = linked_door(&hallway.links[link], control)?;
        legs.push_back(PassageLeg {
            position: project(next)?,
            door,
        });
        point = next;
    }
    Some(legs)
}

fn linked_door(link: &HallwayLink, control: &PreparedComponentControl) -> Option<Option<usize>> {
    let [[key, mask], ..] = link.access;
    if link.access != [[key, mask]; 4] {
        return None;
    }
    if [key, mask] == [u32::MAX; 2] {
        return Some(None);
    }
    (mask == 1 && is_bound_door(control, key)).then_some(Some(key as usize))
}

impl CapturePassage {
    pub(super) fn advance(
        &mut self,
        mut pose: TransformState,
        speed: f32,
        mut seconds: f32,
        neighbors: &[Footprint],
    ) -> (TransformState, Footprint) {
        let [x, y, _] = pose.position.map(f32::from_bits);
        let mut swept = Footprint::point([x, y]);
        while let Some(leg) = self.legs.front() {
            if seconds <= 0.0 || speed <= 0.0 {
                break;
            }
            let from = pose.position.map(f32::from_bits);
            let [x, y, _] = from;
            let [tx, ty, _] = leg.position;
            if neighbors
                .iter()
                .any(|bounds| bounds.intersects([x, y], [tx, ty]))
            {
                break;
            }
            if let Some(door) = leg.door {
                let blend = self
                    .opening
                    .filter(|&(node, _)| node == door)
                    .map_or(0.0, |(_, blend)| blend);
                let delay = seconds.min((1.0 - blend) / DOOR_BLEND_PER_SECOND);
                self.opening = Some((door, (blend + delay * DOOR_BLEND_PER_SECOND).min(1.0)));
                seconds -= delay;
                if seconds == 0.0 {
                    break;
                }
            }
            let delta = std::array::from_fn::<_, 3, _>(|axis| leg.position[axis] - from[axis]);
            let distance = delta[0].hypot(delta[1]).hypot(delta[2]);
            if delta[0].hypot(delta[1]) > WAYPOINT_TOLERANCE {
                let (yaw, delay) = turn_toward(
                    f32::from_bits(pose.orientation[2]),
                    [delta[0], delta[1]],
                    seconds,
                );
                pose.orientation[2] = yaw.to_bits();
                seconds -= delay;
            }
            let duration = distance / speed;
            let reached = duration <= seconds;
            let next = if reached {
                leg.position
            } else {
                std::array::from_fn(|axis| from[axis] + delta[axis] * (seconds / duration))
            };
            pose.position = next.map(f32::to_bits);
            swept.include([next[0], next[1]]);
            if !reached {
                break;
            }
            seconds -= duration;
            self.legs.pop_front();
        }
        (pose, swept)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_msh::{HallwayPoint, NodeId};

    #[test]
    fn only_connected_entrances_lead_to_the_capture_point() {
        let hallway = Hallway {
            points: vec![
                HallwayPoint {
                    position: [0.0; 3],
                    flags: 0,
                    node: NodeId(0)
                };
                5
            ],
            links: [[1, 0], [1, 2], [2, 3], [3, 0], [4, 9]]
                .map(|endpoints| HallwayLink {
                    endpoints,
                    access: [[u32::MAX; 2]; 4],
                })
                .to_vec(),
        };
        assert_eq!(
            links_to(&hallway, 3),
            [
                Some((3, 3)),
                Some((2, 1)),
                Some((3, 2)),
                Some((3, usize::MAX)),
                None
            ]
        );
    }

    #[test]
    fn capture_link_waits_for_the_door_and_follows_the_sloped_passage() {
        let mut route = CapturePassage {
            building: ObjectHandle {
                slot: 0,
                generation: 1,
            },
            legs: VecDeque::from([PassageLeg {
                position: [0.0, 1.0, -1.0],
                door: Some(12),
            }]),
            opening: None,
        };
        let start = TransformState::default();
        let (waiting, _) = route.advance(start, 2.0, 0.25, &[]);
        assert_eq!(waiting.position, start.position);
        assert_eq!(route.opening, Some((12, 0.5)));
        let (moving, _) = route.advance(waiting, 2.0, 0.5, &[]);
        let [x, y, z] = moving.position.map(f32::from_bits);
        assert_eq!(x, 0.0);
        assert!((y - 0.5_f32 / 2.0_f32.sqrt()).abs() < 0.0001);
        assert_eq!(z, -y);
        let (arrived, _) = route.advance(moving, 2.0, 1.0, &[]);
        assert_eq!(arrived.position.map(f32::from_bits), [0.0, 1.0, -1.0]);
        assert!(route.legs.is_empty());
    }
}
