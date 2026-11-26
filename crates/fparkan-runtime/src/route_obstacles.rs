//! Ground route clearance against the projected bounds of live buildings.

use crate::{EngineError, LoadedMissionState, ObjectHandle, TransformState, World};
use fparkan_msh::{node38_fallback_hierarchy, selected_slot, Group, Lod, NodeId};
use fparkan_render::LegacyIron3dEulerTransform;
use fparkan_world::{handle_by_original_id, registration_sequence, transform_state};
use std::collections::{BTreeMap, VecDeque};

// Keep detour nodes far enough from the boundary that arrival tolerance can
// skip a node without putting the next leg back inside the obstacle.
const CORNER_GAP: f32 = 0.1;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Footprint {
    min: [f32; 2],
    max: [f32; 2],
}

#[derive(Clone, Default)]
pub(super) struct BuildingFootprints {
    entries: BTreeMap<u32, (ObjectHandle, TransformState, Option<Footprint>)>,
}

impl BuildingFootprints {
    pub(super) fn refresh(&mut self, world: &World, mission: &LoadedMissionState) {
        self.entries.retain(|_, (handle, _, _)| {
            registration_sequence(world, *handle)
                .ok()
                .flatten()
                .is_some()
        });
        for (index, draft) in mission.object_drafts.iter().enumerate() {
            if !draft
                .unit_dat_header_flags
                .is_some_and(|class| class >> 31 == 1)
            {
                continue;
            }
            let Some(id) = draft.original_id else {
                continue;
            };
            let Some(handle) = handle_by_original_id(world, id) else {
                continue;
            };
            if registration_sequence(world, handle)
                .ok()
                .flatten()
                .is_none()
            {
                continue;
            }
            let Ok(pose) = transform_state(world, handle) else {
                continue;
            };
            if self
                .entries
                .get(&id.0)
                .is_none_or(|&(old_handle, old_pose, _)| (old_handle, old_pose) != (handle, pose))
            {
                self.entries
                    .insert(id.0, (handle, pose, model_footprint(mission, index, pose)));
            }
        }
    }

    pub(super) fn with_clearance(&self, radius: f32) -> Vec<Footprint> {
        self.entries
            .values()
            .filter_map(|(_, _, footprint)| footprint.map(|bounds| bounds.expanded(radius)))
            .collect()
    }

    pub(super) fn for_handle(&self, handle: ObjectHandle) -> Option<Footprint> {
        self.entries
            .values()
            .find_map(|&(owner, _, bounds)| (owner == handle).then_some(bounds).flatten())
    }
}

fn model_footprint(
    mission: &LoadedMissionState,
    index: usize,
    pose: TransformState,
) -> Option<Footprint> {
    let assets = &mission.mission_assets;
    let visual = assets.visual_by_id(assets.visual_for_object(index)?)?;
    let model = &assets.model_by_id(visual.model_id?)?.validated;
    let hierarchy = node38_fallback_hierarchy(model)?;
    let transform = LegacyIron3dEulerTransform {
        translation: pose.position.map(f32::from_bits),
        orientation_radians: pose.orientation.map(f32::from_bits),
    };
    let scale = pose.scale.map(f32::from_bits);
    let mut bounds = Footprint {
        min: [f32::INFINITY; 2],
        max: [f32::NEG_INFINITY; 2],
    };
    for (index, node) in hierarchy.poses.iter().enumerate() {
        let Some(slot) = selected_slot(model, NodeId(index as u32), Lod(0), Group(0)) else {
            continue;
        };
        let slot = &model.slots[slot.0 as usize];
        if slot.batch_count == 0 {
            continue;
        }
        for corner in 0..8 {
            let local = std::array::from_fn(|axis| {
                if corner & (1 << axis) == 0 {
                    slot.aabb_min[axis]
                } else {
                    slot.aabb_max[axis]
                }
            });
            let p = transform.try_transform_scaled_point(node.transform_point(local), scale)?;
            for axis in 0..2 {
                bounds.min[axis] = bounds.min[axis].min(p[axis]);
                bounds.max[axis] = bounds.max[axis].max(p[axis]);
            }
        }
    }
    (0..2)
        .all(|axis| bounds.min[axis] < bounds.max[axis])
        .then_some(bounds)
}

impl Footprint {
    pub(super) fn point(xy: [f32; 2]) -> Self {
        Self { min: xy, max: xy }
    }

    pub(super) fn include(&mut self, xy: [f32; 2]) {
        for (axis, value) in xy.into_iter().enumerate() {
            self.min[axis] = self.min[axis].min(value);
            self.max[axis] = self.max[axis].max(value);
        }
    }

    pub(super) fn expanded(self, margin: f32) -> Self {
        Self {
            min: self.min.map(|v| v - margin),
            max: self.max.map(|v| v + margin),
        }
    }

    pub(super) fn contains(self, p: [f32; 2]) -> bool {
        (0..2).all(|axis| p[axis] >= self.min[axis] && p[axis] <= self.max[axis])
    }

    pub(super) fn intersects(self, from: [f32; 2], to: [f32; 2]) -> bool {
        let (mut first, mut last) = (0.0_f32, 1.0_f32);
        for axis in 0..2 {
            let delta = to[axis] - from[axis];
            if delta == 0.0 {
                if from[axis] < self.min[axis] || from[axis] > self.max[axis] {
                    return false;
                }
                continue;
            }
            let a = (self.min[axis] - from[axis]) / delta;
            let b = (self.max[axis] - from[axis]) / delta;
            first = first.max(a.min(b));
            last = last.min(a.max(b));
            if first > last {
                return false;
            }
        }
        true
    }

    pub(super) fn corners(self) -> [[f32; 2]; 4] {
        let Self {
            min: [x0, y0],
            max: [x1, y1],
        } = self.expanded(CORNER_GAP);
        [[x0, y0], [x0, y1], [x1, y0], [x1, y1]]
    }

    pub(super) fn exits(self, p: [f32; 2]) -> [[f32; 2]; 4] {
        let Self {
            min: [x0, y0],
            max: [x1, y1],
        } = self.expanded(CORNER_GAP);
        [[x0, p[1]], [x1, p[1]], [p[0], y0], [p[0], y1]]
    }

    pub(super) fn center(self) -> [f32; 2] {
        std::array::from_fn(|axis| self.min[axis].midpoint(self.max[axis]))
    }
}

fn distance(a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

/// Replaces a blocked leg with exterior nodes. Returns false if the unit has
/// to wait for a new route. The walkable query keeps nodes on the surface.
pub(super) fn open_leg(
    from: [f32; 2],
    points: &mut VecDeque<[f32; 2]>,
    buildings: &[Footprint],
    traffic: &[Footprint],
    mut walkable: impl FnMut([f32; 2]) -> Result<bool, EngineError>,
) -> Result<bool, EngineError> {
    let obstacles = buildings.iter().chain(traffic);
    while points.len() > 1 && obstacles.clone().any(|o| o.contains(points[0])) {
        points.pop_front();
    }
    let Some(&goal) = points.front() else {
        return Ok(false);
    };
    if !obstacles.clone().any(|o| o.intersects(from, goal)) {
        return Ok(true);
    }
    let contains = |point| obstacles.clone().any(|o| o.contains(point));

    if contains(from) {
        let mut exits: Vec<_> = obstacles
            .clone()
            .filter(|o| o.contains(from))
            .flat_map(|o| o.exits(from))
            .filter(|&p| {
                !contains(p)
                    && !obstacles
                        .clone()
                        .any(|o| !o.contains(from) && o.intersects(from, p))
            })
            .collect();
        exits.sort_by(|&a, &b| distance(from, a).total_cmp(&distance(from, b)));
        for exit in exits {
            if walkable(exit)? {
                if exit != goal {
                    points.push_front(exit);
                }
                return Ok(true);
            }
        }
        return Ok(false);
    }

    let mut nodes = vec![from];
    let mut end = goal;
    let occupied = traffic.iter().any(|body| body.contains(goal));
    if contains(goal) {
        let mut exits: Vec<_> = obstacles
            .clone()
            .filter(|o| o.contains(goal))
            .flat_map(|o| o.exits(goal))
            .filter(|&p| !contains(p))
            .collect();
        exits.sort_by(|&a, &b| distance(from, a).total_cmp(&distance(from, b)));
        let mut selected = None;
        for exit in exits {
            if walkable(exit)? {
                selected = Some(exit);
                break;
            }
        }
        let Some(exit) = selected else {
            return Ok(false);
        };
        end = exit;
    }
    if occupied && distance(from, end) <= crate::route_motion::WAYPOINT_TOLERANCE {
        return Ok(false);
    }
    nodes.push(end);
    if obstacles.clone().any(|o| o.intersects(from, end)) {
        for p in obstacles
            .clone()
            .flat_map(|o| o.corners())
            .filter(|&p| !contains(p))
        {
            if walkable(p)? {
                nodes.push(p);
            }
        }
    }
    let Some(path) = shortest_path(&nodes, obstacles) else {
        return Ok(false);
    };
    if !occupied {
        points.pop_front();
    }
    for p in path {
        points.push_front(p);
    }
    Ok(true)
}

// Dijkstra over the visibility graph. Edges are built on demand and a node
// may connect to any other visible node, including corners on another box.
fn shortest_path<'a>(
    nodes: &[[f32; 2]],
    obstacles: impl Iterator<Item = &'a Footprint> + Clone,
) -> Option<Vec<[f32; 2]>> {
    let mut distance_to = vec![f32::INFINITY; nodes.len()];
    let mut parent = vec![None; nodes.len()];
    let mut settled = vec![false; nodes.len()];
    distance_to[0] = 0.0;
    loop {
        let next = (0..nodes.len())
            .filter(|&i| !settled[i] && distance_to[i].is_finite())
            .min_by(|&a, &b| distance_to[a].total_cmp(&distance_to[b]))?;
        if next == 1 {
            break;
        }
        settled[next] = true;
        for (i, &to) in nodes.iter().enumerate().filter(|&(i, _)| !settled[i]) {
            let cost = distance_to[next] + distance(nodes[next], to);
            if cost < distance_to[i] && !obstacles.clone().any(|o| o.intersects(nodes[next], to)) {
                distance_to[i] = cost;
                parent[i] = Some(next);
            }
        }
    }
    let mut path = Vec::new();
    let mut index = 1;
    while index != 0 {
        path.push(nodes[index]);
        index = parent[index]?;
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detour_clears_both_buildings_and_keeps_the_destination() {
        let a = Footprint {
            min: [2.0, -2.0],
            max: [4.0, 2.0],
        }
        .expanded(0.5);
        let b = Footprint {
            min: [5.0, -1.0],
            max: [7.0, 3.0],
        }
        .expanded(0.5);
        let start = [0.0, 0.0];
        let goal = [9.0, 0.0];
        let mut route = VecDeque::from([goal]);
        assert!(open_leg(start, &mut route, &[a, b], &[], |_| Ok(true)).unwrap());
        assert!(route.len() >= 3);
        assert_eq!(route.back(), Some(&goal));
        let mut from = start;
        for to in route {
            assert!(!a.intersects(from, to));
            assert!(!b.intersects(from, to));
            from = to;
        }
    }

    #[test]
    fn a_unit_already_inside_first_moves_towards_the_nearest_exit() {
        let building = Footprint {
            min: [0.0, -3.0],
            max: [8.0, 3.0],
        };
        let goal = [10.0, 0.0];
        let mut route = VecDeque::from([goal]);
        assert!(open_leg([1.0, 0.0], &mut route, &[building], &[], |_| Ok(true)).unwrap());
        assert_eq!(route, [[-CORNER_GAP, 0.0], goal]);
        assert!(open_leg([0.5, 0.0], &mut route, &[building], &[], |_| Ok(true)).unwrap());
        assert_eq!(route.len(), 2);
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_factory_footprint_blocks_a_ground_route() {
        use crate::{
            create, load_gameplay_mission, EngineServices, MissionMode, MissionRequest,
            MissionStartupInput,
        };
        use fparkan_terrain::SurfaceQuery;
        use fparkan_vfs::DirectoryVfs;
        use std::sync::Arc;

        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed corpus");
        let mut engine = create(EngineServices::new(Arc::new(DirectoryVfs::new(root)))).unwrap();
        load_gameplay_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/CAMPAIGN/CAMPAIGN.04/Mission.02/data.tma".into(),
                mode: MissionMode::SingleMission,
            },
            MissionStartupInput {
                queue_time_ms: 0,
                research_creation_gate: false,
            },
            None,
        )
        .unwrap();
        let mission = engine.loaded.as_ref().unwrap();
        let mut buildings = BuildingFootprints::default();
        buildings.refresh(&engine.world, mission);
        let (_, _, Some(outline)) = buildings.entries[&1] else {
            panic!("factory footprint unavailable")
        };
        let y = outline.min[1].midpoint(outline.max[1]);
        let start = [outline.min[0] - 8.0, y];
        let goal = [outline.max[0] + 8.0, y];
        assert!(outline.intersects(start, goal));
        let mut route = VecDeque::from([goal]);
        assert!(open_leg(start, &mut route, &[outline], &[], |p| {
            mission
                .terrain
                .height_at(p)
                .map(|z| z.is_some())
                .map_err(EngineError::Terrain)
        })
        .unwrap());
        assert!(route.len() > 1);
        assert_eq!(route.back(), Some(&goal));
        let mut from = start;
        for to in route {
            assert!(!outline.intersects(from, to));
            from = to;
        }
    }
}
