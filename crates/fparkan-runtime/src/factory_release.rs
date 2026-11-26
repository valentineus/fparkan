//! Select a free point on the ground for the current outdoor factory release.

use crate::{
    route_obstacles::{BuildingFootprints, Footprint},
    route_traffic::{body_radius, FrameTraffic},
    EngineError, LoadedMissionState, ObjectHandle, TransformState, World,
};
use fparkan_terrain::SurfaceQuery;

pub(super) fn clear_release_position(
    world: &World,
    mission: &LoadedMissionState,
    buildings: &mut BuildingFootprints,
    plant: ObjectHandle,
    slot: usize,
    preferred: [f32; 3],
) -> Result<Option<[f32; 3]>, EngineError> {
    let draft = &mission.object_drafts[slot];
    let pose = TransformState {
        position: preferred.map(f32::to_bits),
        orientation: draft.orientation_raw.map(f32::to_bits),
        scale: draft.scale.map(f32::to_bits),
    };
    buildings.refresh(world, mission);
    // The outdoor approximation remains the fallback for unprepared bounds.
    let (Some(plant_bounds), Some(radius)) = (
        buildings.for_handle(plant),
        body_radius(&mission.mission_assets, slot, pose),
    ) else {
        return Ok(Some(preferred));
    };
    let mut blocked = buildings.with_clearance(radius);
    let [x, y, _] = preferred;
    let traffic = FrameTraffic::read(world, mission);
    let Some(neighbors) = traffic.blockers(plant, [x, y], radius) else {
        return Ok(None);
    };
    blocked.extend(neighbors);
    release_position([x, y], plant_bounds.expanded(radius), &blocked, |point| {
        mission
            .terrain
            .height_at(point)
            .map_err(EngineError::Terrain)
    })
}

fn release_position(
    preferred: [f32; 2],
    plant: Footprint,
    blocked: &[Footprint],
    mut height_at: impl FnMut([f32; 2]) -> Result<Option<f32>, EngineError>,
) -> Result<Option<[f32; 3]>, EngineError> {
    let center = plant.center();
    let mut alternatives = Vec::from(plant.exits(center));
    alternatives.extend(plant.corners());
    let distance = |p: [f32; 2]| (p[0] - preferred[0]).hypot(p[1] - preferred[1]);
    alternatives.sort_by(|&a, &b| distance(a).total_cmp(&distance(b)));
    for point in std::iter::once(preferred).chain(alternatives) {
        if blocked.iter().any(|bounds| bounds.contains(point)) {
            continue;
        }
        if let Some(z) = height_at(point)? {
            return Ok(Some([point[0], point[1], z + 2.0]));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_keeps_the_preferred_clear_point_or_checks_the_factory_perimeter() {
        let plant = Footprint::point([0.0; 2]).expanded(5.0);
        let parked = Footprint::point([6.0, 0.0]).expanded(1.0);
        let flat = |_| Ok(Some(3.0));
        assert_eq!(
            release_position([7.5, 0.0], plant, &[plant, parked], flat).unwrap(),
            Some([7.5, 0.0, 5.0])
        );
        let [x, y, z] = release_position([6.0, 0.0], plant, &[plant, parked], flat)
            .unwrap()
            .unwrap();
        assert!(!plant.contains([x, y]) && !parked.contains([x, y]));
        assert_eq!(z, 5.0);
        let occupied = Footprint::point([0.0; 2]).expanded(8.0);
        assert_eq!(
            release_position([6.0, 0.0], plant, &[occupied], flat).unwrap(),
            None
        );
    }
}
