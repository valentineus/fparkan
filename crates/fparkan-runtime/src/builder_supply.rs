use crate::{unit_patrol::UnitPatrol, BehaviorOreState};
use fparkan_world::{
    identity_metadata, registration_sequence, transform_state, ObjectHandle, OwnerId, World,
};

// Behavior.dll config constructor RVA 0x16250 and build task RVA 0x28ff0.
const BUILDING_COST_THRESHOLD: f32 = 100.0;
const LOADING_RATE: f32 = 100.0;

pub(super) struct BuilderSupply {
    pub(super) source: ObjectHandle,
    last_ms: Option<u32>,
}

impl BuilderSupply {
    pub(super) fn plan(
        world: &World,
        ores: &[BehaviorOreState],
        builder: ObjectHandle,
        owner: OwnerId,
        class: u32,
        stock: f32,
    ) -> Result<Option<Self>, String> {
        if class == crate::mining::MINE_CLASS || stock >= BUILDING_COST_THRESHOLD {
            return Ok(None);
        }
        let source = source(world, ores, builder, owner).ok_or("builder has no ore source")?;
        Ok(Some(Self {
            source,
            last_ms: None,
        }))
    }

    pub(super) fn tick(
        &mut self,
        world: &World,
        ores: &mut [BehaviorOreState],
        patrol: &mut Option<UnitPatrol>,
        builder: ObjectHandle,
        owner: OwnerId,
        cost: f32,
        now_ms: u32,
    ) -> Result<bool, String> {
        let elapsed = self.last_ms.map_or(0, |last| now_ms.wrapping_sub(last));
        self.last_ms = Some(now_ms);
        if !owned(world, self.source, owner) {
            self.source =
                source(world, ores, builder, owner).ok_or("builder lost its ore source")?;
        }
        let target = ores
            .iter()
            .position(|ore| ore.handle == self.source)
            .ok_or("builder ore source is unavailable")?;
        let cargo = ores
            .iter()
            .position(|ore| ore.handle == builder)
            .ok_or("builder ore state is unavailable")?;
        let [x, y, _] = transform_state(world, builder)
            .map_err(|err| err.to_string())?
            .position
            .map(f32::from_bits);
        let [a, b, _] = transform_state(world, self.source)
            .map_err(|err| err.to_string())?
            .position
            .map(f32::from_bits);
        let distance = (x - a).hypot(y - b);
        let moving = patrol
            .as_ref()
            .is_some_and(|patrol| patrol.is_moving_to_destination(builder));
        // shortcut: use an exterior approach until native dock and port movement are wired.
        if distance > 48.0 {
            if !moving {
                let offset = 40.0 / distance;
                patrol.get_or_insert_with(UnitPatrol::scripted).move_to(
                    world,
                    builder,
                    [a + (x - a) * offset, b + (y - b) * offset],
                    false,
                );
                eprintln!(
                    "script_builder_supply_route clan={} builder={} source={} queue_time_ms={now_ms}",
                    owner.0, builder.slot, self.source.slot
                );
            }
            return Ok(false);
        }
        if let Some(patrol) = patrol {
            patrol.clear_move(builder);
        }
        let stock = f32::from_bits(ores[cargo].current_ore_bits);
        let available = f32::from_bits(ores[target].current_ore_bits);
        if !stock.is_finite() || !available.is_finite() {
            return Err("builder supply contains non-finite ore".into());
        }
        let amount = batch(stock, available, cost, elapsed);
        ores[target].current_ore_bits = (available - amount).to_bits();
        let collected = stock + amount;
        ores[cargo].current_ore_bits = collected.to_bits();
        Ok(collected >= cost)
    }
}

fn owned(world: &World, handle: ObjectHandle, owner: OwnerId) -> bool {
    registration_sequence(world, handle)
        .ok()
        .flatten()
        .is_some()
        && identity_metadata(world, handle).is_ok_and(|id| id.owner_id == Some(owner))
}

fn source(
    world: &World,
    ores: &[BehaviorOreState],
    builder: ObjectHandle,
    owner: OwnerId,
) -> Option<ObjectHandle> {
    let from = transform_state(world, builder)
        .ok()?
        .position
        .map(f32::from_bits);
    ores.iter()
        .filter(|ore| {
            matches!(ore.class_flags, crate::mining::MINE_CLASS | 0x8000_0008)
                && eligible_source(f32::from_bits(ore.current_ore_bits))
                && owned(world, ore.handle, owner)
        })
        .filter_map(|ore| {
            let to = transform_state(world, ore.handle)
                .ok()?
                .position
                .map(f32::from_bits);
            let distance = (from[0] - to[0])
                .hypot(from[1] - to[1])
                .hypot(from[2] - to[2]);
            distance.is_finite().then_some((ore.handle, distance))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(handle, _)| handle)
}

fn eligible_source(stock: f32) -> bool {
    stock.is_finite() && stock > BUILDING_COST_THRESHOLD * 1.5
}

fn batch(stock: f32, available: f32, cost: f32, elapsed_ms: u32) -> f32 {
    let missing = (cost - stock).max(0.0);
    let limit = elapsed_ms as f32 * 0.001 * LOADING_RATE;
    limit.min(available).min(missing)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_start_uses_the_fixed_threshold_and_bypasses_supply_for_a_mine() {
        let world = fparkan_world::new(fparkan_world::WorldConfig);
        let builder = ObjectHandle {
            slot: 0,
            generation: 0,
        };
        for (class, stock) in [(0x8000_0010, 100.0), (crate::mining::MINE_CLASS, 0.0)] {
            assert!(
                BuilderSupply::plan(&world, &[], builder, OwnerId(1), class, stock)
                    .unwrap()
                    .is_none()
            );
        }
        assert!(
            matches!(BuilderSupply::plan(&world, &[], builder, OwnerId(1), 0x8000_0010, 99.0), Err(reason) if reason == "builder has no ore source")
        );
    }

    #[test]
    fn native_loading_stops_at_the_price_or_available_ore() {
        for (stock, available, cost, elapsed, expected) in [
            (0.0, 1000.0, 770.0, 1000, 100.0),
            (700.0, 1000.0, 770.0, 1000, 70.0),
            (0.0, 10.0, 770.0, 1000, 10.0),
            (800.0, 1000.0, 770.0, 1000, 0.0),
            (-670.0, 1000.0, 560.0, 1000, 100.0),
        ] {
            assert_eq!(batch(stock, available, cost, elapsed), expected);
        }
        assert!(!eligible_source(150.0));
        assert!(eligible_source(150.01));
    }
}
