//! Select a free point on the ground for the current outdoor factory release.

use crate::{
    flight_altitude::FlightClearance,
    ground_support::GroundSupport,
    route_motion::{follow_route, RouteClearance, WAYPOINT_TOLERANCE},
    route_obstacles::{BuildingFootprints, Footprint},
    route_traffic::{body_radius, FrameTraffic},
    EngineError, LoadedMissionState, ObjectHandle, TransformState, World,
};
use fparkan_terrain::SurfaceQuery;
use std::collections::VecDeque;

pub(super) struct ReleaseSite {
    pub(super) position: [f32; 3],
    pub(super) exit: Option<[f32; 2]>,
}

pub(super) fn clear_release_position(
    world: &World,
    mission: &LoadedMissionState,
    buildings: &mut BuildingFootprints,
    plant: ObjectHandle,
    slot: usize,
    preferred: [f32; 3],
) -> Result<Option<ReleaseSite>, EngineError> {
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
        return Ok(Some(ReleaseSite {
            position: preferred,
            exit: None,
        }));
    };
    let mut blocked = buildings.with_clearance(radius);
    let [x, y, _] = preferred;
    let traffic = FrameTraffic::read(world, mission);
    let Some(neighbors) = traffic.blockers(plant, [x, y], radius) else {
        return Ok(None);
    };
    blocked.extend(neighbors);
    release_position(
        [x, y],
        plant_bounds.expanded(radius),
        radius,
        &blocked,
        |point| {
            mission
                .terrain
                .height_at(point)
                .map_err(EngineError::Terrain)
        },
    )
}

fn release_position(
    preferred: [f32; 2],
    plant: Footprint,
    radius: f32,
    blocked: &[Footprint],
    mut height_at: impl FnMut([f32; 2]) -> Result<Option<f32>, EngineError>,
) -> Result<Option<ReleaseSite>, EngineError> {
    let center = plant.center();
    let mut alternatives = Vec::from(plant.exits(center));
    alternatives.extend(plant.corners());
    let distance = |p: [f32; 2]| (p[0] - preferred[0]).hypot(p[1] - preferred[1]);
    alternatives.sort_by(|&a, &b| distance(a).total_cmp(&distance(b)));
    let apron = plant.expanded(radius);
    for point in alternatives.into_iter().chain([preferred]) {
        if blocked.iter().any(|bounds| bounds.contains(point)) {
            continue;
        }
        let Some(z) = height_at(point)? else {
            continue;
        };
        let position = [point[0], point[1], z + 2.0];
        if !apron.contains(point) {
            return Ok(Some(ReleaseSite {
                position,
                exit: None,
            }));
        }
        let mut exits = apron.exits(point);
        let distance = |p: [f32; 2]| (p[0] - point[0]).hypot(p[1] - point[1]);
        exits.sort_by(|&a, &b| distance(a).total_cmp(&distance(b)));
        for exit in exits {
            if !blocked.iter().any(|bounds| bounds.intersects(point, exit))
                && height_at(exit)?.is_some()
            {
                return Ok(Some(ReleaseSite {
                    position,
                    exit: Some(exit),
                }));
            }
        }
    }
    Ok(None)
}

#[derive(Clone)]
pub(super) struct FactoryDeparture {
    pub(super) handle: ObjectHandle,
    goal: [f32; 2],
    ground: Option<GroundSupport>,
    airborne: bool,
}

impl FactoryDeparture {
    pub(super) fn new(
        handle: ObjectHandle,
        index: usize,
        goal: [f32; 2],
        mission: &LoadedMissionState,
    ) -> Self {
        let airborne = mission.object_drafts[index]
            .original_id
            .and_then(|id| mission.unit_chassis_flags.get(&id.0))
            .and_then(|chassis| chassis.can_fly)
            == Some(true);
        let ground = (!airborne)
            .then(|| GroundSupport::prepare(&mission.mission_assets, index))
            .flatten();
        Self {
            handle,
            goal,
            ground,
            airborne,
        }
    }

    pub(super) fn move_out(
        &self,
        before: TransformState,
        mission: &LoadedMissionState,
        traffic: &mut FrameTraffic,
        buildings: &BuildingFootprints,
        seconds: f32,
    ) -> Result<TransformState, EngineError> {
        let [x, y, _] = before.position.map(f32::from_bits);
        let radius = traffic.radius(self.handle);
        let Some(neighbors) = traffic.blockers(self.handle, [x, y], radius) else {
            return Ok(before);
        };
        let flight = (self.airborne && radius > 0.0).then_some(FlightClearance {
            body_radius: radius,
        });
        let speed = mission
            .control_device_tables
            .iter()
            .find(|table| table.handle == self.handle)
            .and_then(|table| table.owner_profile.as_ref())
            .map(|profile| f32::from_bits(profile.current_words[7]))
            .filter(|speed| speed.is_finite() && *speed >= 0.0)
            .unwrap_or(9.0);
        let (after, swept) = follow_route(
            before,
            &mut VecDeque::from([self.goal]),
            speed,
            seconds,
            RouteClearance {
                buildings: &buildings.with_clearance(radius),
                traffic: &neighbors,
                flight,
                ground: self.ground,
            },
            |point| {
                mission
                    .terrain
                    .height_at(point)
                    .map_err(EngineError::Terrain)
            },
        )?;
        traffic.reserve(self.handle, swept);
        Ok(after)
    }

    pub(super) fn finished(&self, pose: TransformState) -> bool {
        let [x, y, _] = pose.position.map(f32::from_bits);
        (x - self.goal[0]).hypot(y - self.goal[1]) <= WAYPOINT_TOLERANCE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_keeps_the_cleared_path_out_of_the_factory_apron() {
        let plant = Footprint::point([0.0; 2]).expanded(5.0);
        let parked = Footprint::point([6.0, 0.0]).expanded(1.0);
        let flat = |_| Ok(Some(3.0));
        let site = release_position([6.0, 0.0], plant, 2.0, &[plant, parked], flat)
            .unwrap()
            .unwrap();
        let [x, y, z] = site.position;
        let exit = site.exit.unwrap();
        assert!(!plant.contains([x, y]) && !parked.contains([x, y]));
        assert!(!plant.expanded(2.0).contains(exit));
        assert!(!parked.intersects([x, y], exit));
        assert_eq!(z, 5.0);
        let occupied = Footprint::point([0.0; 2]).expanded(8.0);
        assert!(release_position([6.0, 0.0], plant, 2.0, &[occupied], flat)
            .unwrap()
            .is_none());
        let open_ground = release_position([8.0, 0.0], plant, 2.0, &[plant], |point| {
            Ok((point == [8.0, 0.0]).then_some(3.0))
        })
        .unwrap()
        .unwrap();
        assert_eq!(open_ground.position, [8.0, 0.0, 5.0]);
        assert!(open_ground.exit.is_none());
    }
}
