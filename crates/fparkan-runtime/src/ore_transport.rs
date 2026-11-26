use crate::{
    route_motion::WAYPOINT_TOLERANCE,
    route_obstacles::{model_footprint, Footprint, CORNER_GAP},
    route_traffic::body_radius,
    unit_patrol::UnitPatrol,
    BehaviorOreState, MissionAssets, MissionObjectDraft, TerrainWorld, TransformState,
};
use fparkan_terrain::SurfaceQuery;
use fparkan_world::{
    identity_metadata, registration_sequence, transform_state, ObjectHandle, OriginalObjectId,
    OwnerId, World,
};

#[derive(Clone, Copy)]
pub(super) struct TransportGeometry<'a> {
    pub(super) assets: &'a MissionAssets,
    pub(super) drafts: &'a [MissionObjectDraft],
    pub(super) terrain: &'a TerrainWorld,
}

impl TransportGeometry<'_> {
    fn index(self, id: OriginalObjectId) -> Option<usize> {
        self.drafts
            .iter()
            .position(|draft| draft.original_id == Some(id))
    }

    fn clearance(
        self,
        unit: OriginalObjectId,
        unit_pose: TransformState,
        building: OriginalObjectId,
        building_pose: TransformState,
    ) -> Option<Footprint> {
        let radius = body_radius(self.assets, self.index(unit)?, unit_pose)?;
        model_footprint(self.assets, self.index(building)?, building_pose)
            .map(|bounds| bounds.expanded(radius))
    }
}

pub(super) struct OreTransport {
    pub(super) unit: ObjectHandle,
    owner: OwnerId,
    destination: Option<ObjectHandle>,
    dock: Option<(ObjectHandle, TransformState, Option<Footprint>)>,
    unloading: bool,
    last_ms: u32,
}

impl OreTransport {
    pub(super) fn new(unit: ObjectHandle, owner: OwnerId, now_ms: u32) -> Self {
        Self {
            unit,
            owner,
            destination: None,
            dock: None,
            unloading: false,
            last_ms: now_ms,
        }
    }

    pub(super) fn tick(
        &mut self,
        world: &World,
        ores: &mut [BehaviorOreState],
        patrol: &mut Option<UnitPatrol>,
        geometry: TransportGeometry<'_>,
        now_ms: u32,
    ) -> bool {
        let owned = |unit| {
            registration_sequence(world, unit).ok().flatten().is_some()
                && identity_metadata(world, unit).is_ok_and(|id| id.owner_id == Some(self.owner))
        };
        let moving = patrol
            .as_ref()
            .is_some_and(|patrol| patrol.is_moving_to_destination(self.unit));
        if !owned(self.unit)
            || patrol
                .as_ref()
                .is_some_and(|patrol| patrol.is_shut_down(self.unit))
        {
            if moving {
                patrol.as_mut().unwrap().clear_move(self.unit);
            }
            return false;
        }
        let elapsed = now_ms.wrapping_sub(self.last_ms);
        self.last_ms = now_ms;
        let Some(cargo) = ores.iter().position(|ore| ore.handle == self.unit) else {
            return false;
        };
        let Ok(pose) = transform_state(world, self.unit) else {
            return false;
        };
        let [x, y, z] = pose.position.map(f32::from_bits);
        let current = f32::from_bits(ores[cargo].current_ore_bits);
        let capacity = f32::from_bits(ores[cargo].maximum_ore_bits);
        if !capacity.is_finite() || capacity <= 0.0 || !current.is_finite() {
            return false;
        }
        let class = if self.unloading {
            0x8000_0008
        } else {
            crate::mining::MINE_CLASS
        };
        let endpoints = ores.iter().enumerate().filter_map(|(index, ore)| {
            if ore.class_flags != class || !owned(ore.handle) {
                return None;
            }
            let amount = f32::from_bits(ore.current_ore_bits);
            if !amount.is_finite()
                || (self.unloading && amount >= f32::from_bits(ore.maximum_ore_bits))
            {
                return None;
            }
            let building = transform_state(world, ore.handle).ok()?;
            let [a, b, c] = building.position.map(f32::from_bits);
            let distance = ((x - a).hypot(y - b)).hypot(z - c);
            distance
                .is_finite()
                .then_some((index, ore.handle, building, distance))
        });
        let target = endpoints.min_by(|a, b| {
            (self.destination != Some(a.1))
                .cmp(&(self.destination != Some(b.1)))
                .then_with(|| a.3.total_cmp(&b.3))
        });
        let Some((index, handle, building, _)) = target else {
            if moving {
                patrol.as_mut().unwrap().clear_move(self.unit);
            }
            self.destination = None;
            self.dock = None;
            if !self.unloading && current > 0.0 {
                self.unloading = true;
            }
            return true;
        };
        let dock_changed = self
            .dock
            .is_none_or(|(target, pose, _)| target != handle || pose != building);
        if dock_changed {
            let clearance = geometry.clearance(
                ores[cargo].original_id,
                pose,
                ores[index].original_id,
                building,
            );
            self.dock = Some((handle, building, clearance));
        }
        let [a, b, _] = building.position.map(f32::from_bits);
        let center_distance = (x - a).hypot(y - b);
        let clearance = self.dock.and_then(|(_, _, clearance)| clearance);
        let arrived = clearance.map_or(center_distance <= 48.0, |bounds| {
            beside_dock([x, y], bounds)
        });
        if !arrived {
            if self.destination != Some(handle) || dock_changed || !moving {
                let approach = match clearance {
                    Some(bounds) => dock_approach([x, y], bounds, |point| {
                        geometry.terrain.height_at(point).ok().flatten().is_some()
                    }),
                    None => {
                        let offset = 40.0 / center_distance;
                        Some([a + (x - a) * offset, b + (y - b) * offset])
                    }
                };
                if let Some(approach) = approach {
                    patrol
                        .get_or_insert_with(UnitPatrol::scripted)
                        .move_to(world, self.unit, approach, false);
                    if self.destination != Some(handle) {
                        eprintln!(
                            "script_ore_transport_route clan={} unit={} target={} unloading={} queue_time_ms={now_ms}",
                            self.owner.0, self.unit.slot, handle.slot, self.unloading,
                        );
                    }
                    self.destination = Some(handle);
                }
            }
            return true;
        }
        if moving {
            patrol.as_mut().unwrap().clear_move(self.unit);
        }
        self.destination = Some(handle);
        let (source, sink) = if self.unloading {
            (cargo, index)
        } else {
            (index, cargo)
        };
        // shortcut: use the cargo capacity per four seconds until the live transport rate is wired.
        let limit = capacity * 0.25 * elapsed as f32 * 0.001;
        let clamped = transfer(ores, source, sink, limit);
        if clamped && (self.unloading || f32::from_bits(ores[cargo].current_ore_bits) > 0.0) {
            if self.unloading {
                eprintln!("script_ore_transport_unloaded clan={} unit={} storage={} queue_time_ms={now_ms}",
                    self.owner.0, self.unit.slot, handle.slot);
            }
            self.unloading = !self.unloading;
            self.destination = None;
        }
        true
    }
}

fn beside_dock([x, y]: [f32; 2], bounds: Footprint) -> bool {
    let [a, b] = bounds.closest_point([x, y]);
    !bounds.contains([x, y]) && (a - x).hypot(b - y) <= CORNER_GAP + 2.0 * WAYPOINT_TOLERANCE
}

fn dock_approach(
    [x, y]: [f32; 2],
    bounds: Footprint,
    mut on_ground: impl FnMut([f32; 2]) -> bool,
) -> Option<[f32; 2]> {
    bounds
        .exits([x, y])
        .into_iter()
        .chain(bounds.corners())
        .filter(|&point| on_ground(point))
        .min_by(|a, b| {
            let distance = |[px, py]: [f32; 2]| (px - x).hypot(py - y);
            distance(*a).total_cmp(&distance(*b))
        })
}

fn transfer(ores: &mut [BehaviorOreState], source: usize, sink: usize, limit: f32) -> bool {
    let from = f32::from_bits(ores[source].current_ore_bits);
    let to = f32::from_bits(ores[sink].current_ore_bits);
    let space = f32::from_bits(ores[sink].maximum_ore_bits) - to;
    let amount = limit.min(from).min(space).max(0.0);
    ores[source].current_ore_bits = (from - amount).to_bits();
    ores[sink].current_ore_bits = (to + amount).to_bits();
    limit >= from.min(space)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_world::OriginalObjectId;

    #[test]
    fn transport_uses_a_clear_stop_for_wide_buildings() {
        let clearance = Footprint::point([0.0; 2]).expanded(74.0);
        let outside = dock_approach([130.0, 10.0], clearance, |_| true).unwrap();
        assert_eq!(outside, [74.1, 10.0]);
        assert!(beside_dock(outside, clearance));
        assert!(!beside_dock([40.0, 10.0], clearance));
        assert!(!beside_dock([80.0, 10.0], clearance));
        let alternate = dock_approach([130.0, 10.0], clearance, |point| point != outside).unwrap();
        assert!(beside_dock(alternate, clearance));
    }

    #[test]
    fn transfer_limits_the_batch_to_storage_room_and_preserves_ore() {
        let mut ores =
            [(100.0_f32, 250.0_f32), (80.0, 100.0)].map(|(current, capacity)| BehaviorOreState {
                original_id: OriginalObjectId(0),
                handle: ObjectHandle {
                    slot: 0,
                    generation: 0,
                },
                class_flags: 0,
                current_ore_bits: current.to_bits(),
                maximum_ore_bits: capacity.to_bits(),
            });
        assert!(!transfer(&mut ores, 0, 1, 12.0));
        assert!(transfer(&mut ores, 0, 1, 12.0));
        assert_eq!(
            ores.map(|ore| f32::from_bits(ore.current_ore_bits)),
            [80.0, 100.0]
        );
    }
}
