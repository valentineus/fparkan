//! Exterior approach to a capture point in a building's authored hallway.

use crate::{
    projectile_parts::root_model, route_motion::route_points, route_obstacles::model_footprint,
    MissionAssets, TerrainWorld,
};
use fparkan_msh::{node38_fallback_hierarchy, Hallway};
use fparkan_render::LegacyIron3dEulerTransform;
use fparkan_terrain::SurfaceQuery;
use fparkan_world::TransformState;
use std::collections::VecDeque;

pub(super) struct CaptureEntrance {
    pub(super) staging: [f32; 2],
    pub(super) capture: [f32; 3],
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
    let capture = hallway
        .points
        .iter()
        .position(|point| point.flags & 0x40 != 0)?;
    let toward_capture = links_to(hallway, capture);
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
    let capture = project(capture)?;
    let footprint = model_footprint(assets, index, pose)?.expanded(clearance);
    hallway
        .points
        .iter()
        .enumerate()
        .filter(|(_, point)| point.flags & 0x1000_0001 == 1)
        .filter_map(|(index, _)| {
            let entrance = project(index)?;
            let inside = project(toward_capture[index]?)?;
            let staging = footprint.exterior_along(
                [entrance[0], entrance[1]],
                [entrance[0] - inside[0], entrance[1] - inside[1]],
            )?;
            let ground = terrain.height_at(staging).ok()??;
            route_points(terrain, from, [staging[0], staging[1], ground]).ok()??;
            let distance: f32 = (0..3).map(|i| (from[i] - entrance[i]).powi(2)).sum();
            Some((distance, CaptureEntrance { staging, capture }))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, entrance)| entrance)
}

fn links_to(hallway: &Hallway, goal: usize) -> Vec<Option<usize>> {
    let mut next = vec![None; hallway.points.len()];
    next[goal] = Some(goal);
    let mut queue = VecDeque::from([goal]);
    while let Some(point) = queue.pop_front() {
        for &[a, b] in &hallway.links {
            let (a, b) = (a as usize, b as usize);
            let neighbor = if a == point {
                b
            } else if b == point {
                a
            } else {
                continue;
            };
            if let Some(slot @ None) = next.get_mut(neighbor) {
                *slot = Some(point);
                queue.push_back(neighbor);
            }
        }
    }
    next
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
            links: vec![[1, 0], [1, 2], [2, 3], [3, 0], [4, 9]],
        };
        assert_eq!(
            links_to(&hallway, 3),
            [Some(3), Some(2), Some(3), Some(3), None]
        );
    }
}
