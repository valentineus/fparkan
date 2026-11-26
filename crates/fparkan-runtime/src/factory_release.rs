//! Release above the factory roof and clear the apron before taking orders.

use crate::{
    flight_altitude::FlightClearance,
    ground_support::GroundSupport,
    projectile_parts::root_triangle_hit,
    route_motion::{follow_route, RouteClearance, WAYPOINT_TOLERANCE},
    route_obstacles::{BuildingFootprints, Footprint},
    route_traffic::{body_radius, FrameTraffic},
    EngineError, LoadedMissionState, ObjectHandle, TransformState, World,
};
use fparkan_terrain::SurfaceQuery;
use fparkan_world::transform_state;
use std::collections::VecDeque;

pub(super) struct ReleaseSite {
    pub(super) position: [f32; 3],
    exit: Option<[f32; 2]>,
    roof: Option<f32>,
}

pub(super) fn clear_release_position(
    world: &World,
    mission: &LoadedMissionState,
    buildings: &mut BuildingFootprints,
    plant: ObjectHandle,
    plant_index: usize,
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
            roof: None,
        }));
    };
    let mut blocked = buildings.with_clearance(radius);
    let [x, y, _] = preferred;
    let traffic = FrameTraffic::read(world, mission);
    let Some(neighbors) = traffic.blockers(plant, [x, y], radius) else {
        return Ok(None);
    };
    blocked.extend(neighbors);
    let site = release_position(
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
    )?;
    let Some(mut site) = site else {
        return Ok(None);
    };
    let plant_pose = transform_state(world, plant)?;
    let [x, y, z] = plant_pose.position.map(f32::from_bits);
    if let Some(terrain) = mission
        .terrain
        .height_at([x, y])
        .map_err(EngineError::Terrain)?
    {
        let top = z + 1000.0;
        if let Some((fraction, _)) = root_triangle_hit(
            &mission.mission_assets,
            plant_index,
            plant_pose,
            [[x, y, top], [x, y, terrain]],
            |_| true,
        ) {
            let roof = top + fraction * (terrain - top);
            site.exit = Some(site.exit.unwrap_or([site.position[0], site.position[1]]));
            // Behavior's upper support ray adds 100 before the 15-unit release offset.
            site.position = [x, y, roof + 100.0 + 15.0];
            site.roof = Some(roof);
        }
    }
    Ok(Some(site))
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
                roof: None,
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
                    roof: None,
                }));
            }
        }
    }
    Ok(None)
}

#[derive(Clone)]
pub(super) struct FactoryDeparture {
    pub(super) handle: ObjectHandle,
    pub(super) plant: ObjectHandle,
    goal: [f32; 2],
    ground: Option<GroundSupport>,
    airborne: bool,
    descent: Option<Descent>,
}

#[derive(Clone)]
struct Descent {
    roof: f32,
    fall_speed: f32,
}

impl FactoryDeparture {
    pub(super) fn from_site(
        handle: ObjectHandle,
        plant: ObjectHandle,
        index: usize,
        site: ReleaseSite,
        mission: &LoadedMissionState,
    ) -> Option<Self> {
        let goal = site.exit?;
        let airborne = mission.object_drafts[index]
            .original_id
            .and_then(|id| mission.unit_chassis_flags.get(&id.0))
            .and_then(|chassis| chassis.can_fly)
            == Some(true);
        let ground = (!airborne)
            .then(|| GroundSupport::prepare(&mission.mission_assets, index))
            .flatten();
        Some(Self {
            handle,
            plant,
            goal,
            ground,
            airborne,
            descent: site.roof.map(|roof| Descent {
                roof,
                fall_speed: 0.0,
            }),
        })
    }

    pub(super) fn move_out(
        &mut self,
        before: TransformState,
        mission: &LoadedMissionState,
        traffic: &mut FrameTraffic,
        buildings: &BuildingFootprints,
        seconds: f32,
    ) -> Result<TransformState, EngineError> {
        let [x, y, altitude] = before.position.map(f32::from_bits);
        let radius = traffic.radius(self.handle);
        let plant_bounds = buildings
            .for_handle(self.plant)
            .map(|bounds| bounds.expanded(radius));
        let above_plant =
            self.descent.is_some() && plant_bounds.is_some_and(|bounds| bounds.contains([x, y]));
        let obstacles = buildings.with_clearance_except(radius, above_plant.then_some(self.plant));
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
        let mut height_at = |point| {
            mission
                .terrain
                .height_at(point)
                .map_err(EngineError::Terrain)
        };
        let (mut after, swept) = follow_route(
            before,
            &mut VecDeque::from([self.goal]),
            speed,
            seconds,
            RouteClearance {
                buildings: &obstacles,
                traffic: &neighbors,
                flight,
                ground: self.descent.is_none().then_some(self.ground).flatten(),
            },
            &mut height_at,
        )?;
        if let Some(descent) = &mut self.descent {
            let [nx, ny, _] = after.position.map(f32::from_bits);
            let xy = [nx, ny];
            if let Some(terrain) = height_at(xy)? {
                let floor = if let Some(ground) = self.ground {
                    ground.height(xy, after, &mut height_at)?.unwrap_or(terrain)
                } else {
                    terrain + if self.airborne { 2.0 * radius } else { 2.0 }
                };
                let over_roof = plant_bounds.is_some_and(|bounds| bounds.contains(xy));
                let support = if over_roof {
                    floor.max(descent.roof + radius)
                } else {
                    floor
                };
                // shortcut: constant gravity until the original release motion is decoded.
                let end_speed = descent.fall_speed + 9.81 * seconds;
                let z = (altitude - descent.fall_speed.midpoint(end_speed) * seconds).max(support);
                descent.fall_speed = if z > support { end_speed } else { 0.0 };
                after.position[2] = z.to_bits();
                if !over_roof && z == floor {
                    self.descent = None;
                }
            }
        }
        traffic.reserve(self.handle, swept);
        Ok(after)
    }

    pub(super) fn finished(&self, pose: TransformState) -> bool {
        let [x, y, _] = pose.position.map(f32::from_bits);
        self.descent.is_none() && (x - self.goal[0]).hypot(y - self.goal[1]) <= WAYPOINT_TOLERANCE
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
