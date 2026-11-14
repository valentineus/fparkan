use crate::{Engine, MissionControlNodeProviderTableStatus};
use fparkan_world::{identity_metadata, ObjectHandle};

/// Current values for the player's controlled unit and clan.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerStatus {
    /// Remaining root Control capacity as a percentage.
    pub health_percent: Option<u32>,
    /// Ore stored in the clan's buffer.
    pub ore_in_buffer: Option<f32>,
    /// Available power and total power demand.
    pub power: Option<(f32, f32)>,
}

/// Reads the values used by combat and the clan's current distributor.
#[must_use]
pub fn player_status(engine: &Engine, controlled: ObjectHandle) -> Option<PlayerStatus> {
    let mission = engine.loaded.as_ref()?;
    let owner = identity_metadata(&engine.world, controlled)
        .ok()?
        .owner_id?;
    let health_percent = mission
        .control_device_tables
        .iter()
        .find(|table| table.handle == controlled)
        .and_then(|table| {
            let MissionControlNodeProviderTableStatus::Available(rows) =
                &table.node_provider_status
            else {
                return None;
            };
            let root = rows.first()?;
            health_percent(
                f32::from_bits(root.current_amount_bits),
                f32::from_bits(root.initial_amount_bits),
            )
        });
    let resources = mission
        .script_bundles
        .iter()
        .find(|bundle| bundle.clan_index == usize::from(owner.0))
        .and_then(|bundle| bundle.behavior_warm_start);
    Some(PlayerStatus {
        health_percent,
        ore_in_buffer: resources.map(|state| f32::from_bits(state.cleared_words[2])),
        power: resources.map(|state| {
            let [available, _, _, demand, _] = state.cleared_words.map(f32::from_bits);
            (available, demand)
        }),
    })
}

fn health_percent(current: f32, initial: f32) -> Option<u32> {
    (current.is_finite() && initial.is_finite() && initial > 0.0)
        .then(|| (100.0 * (current / initial).clamp(0.0, 1.0)).round() as u32)
}

#[cfg(test)]
mod tests {
    use super::health_percent;

    #[test]
    fn reports_current_capacity_within_display_range() {
        assert_eq!(health_percent(23.0, 80.0), Some(29));
        assert_eq!(health_percent(90.0, 80.0), Some(100));
        assert_eq!(health_percent(-5.0, 80.0), Some(0));
        assert_eq!(health_percent(0.0, 0.0), None);
    }
}
