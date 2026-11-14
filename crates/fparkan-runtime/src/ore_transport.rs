use crate::unit_patrol::UnitPatrol;
use crate::BehaviorOreState;
use fparkan_world::{
    identity_metadata, registration_sequence, transform_state, ObjectHandle, OwnerId, World,
};

pub(super) struct OreTransport {
    pub(super) unit: ObjectHandle,
    owner: OwnerId,
    destination: Option<ObjectHandle>,
    unloading: bool,
    last_ms: u32,
}

impl OreTransport {
    pub(super) fn new(unit: ObjectHandle, owner: OwnerId, now_ms: u32) -> Self {
        Self {
            unit,
            owner,
            destination: None,
            unloading: false,
            last_ms: now_ms,
        }
    }

    pub(super) fn tick(
        &mut self,
        world: &World,
        ores: &mut [BehaviorOreState],
        patrol: &mut Option<UnitPatrol>,
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
            let pose = transform_state(world, ore.handle).ok()?;
            let [a, b, c] = pose.position.map(f32::from_bits);
            let distance = ((x - a).hypot(y - b)).hypot(z - c);
            distance
                .is_finite()
                .then_some((index, ore.handle, [a, b], distance))
        });
        let target = endpoints.min_by(|a, b| {
            (self.destination != Some(a.1))
                .cmp(&(self.destination != Some(b.1)))
                .then_with(|| a.3.total_cmp(&b.3))
        });
        let Some((index, handle, center, _)) = target else {
            if moving {
                patrol.as_mut().unwrap().clear_move(self.unit);
            }
            self.destination = None;
            if !self.unloading && current > 0.0 {
                self.unloading = true;
            }
            return true;
        };
        let distance = (x - center[0]).hypot(y - center[1]);
        // shortcut: use exterior approach points until native dock and port movement are wired.
        if distance > 48.0 {
            if self.destination != Some(handle) || !moving {
                let offset = 40.0 / distance;
                let approach = [
                    center[0] + (x - center[0]) * offset,
                    center[1] + (y - center[1]) * offset,
                ];
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
