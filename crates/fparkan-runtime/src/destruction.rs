//! Presentation events for projectile damage that destroys a unit.

use crate::{
    autodemo::MOBILE_UNIT, Engine, MissionControlDeviceTableState, MissionControlNodeProviderRow,
    MissionControlNodeProviderTableStatus, ObjectHandle, UNIT_DAT_CONTROL_CLASS_HIGH_BIT,
};
use fparkan_world::{identity_metadata, transform_state};

/// Position and attached EXPL resource of a destroyed chassis or building part.
pub struct UnitDestructionEffect {
    /// Prepared draft index for the destroyed actor when it has an authored layout.
    pub object_index: Option<usize>,
    /// World-space origin of the effect.
    pub position: [f32; 3],
    /// Archive containing the explosion description.
    pub archive: String,
    /// Name of the explosion description.
    pub name: String,
}

/// Collect lethal projectile events when a scene is presenting the mission.
pub fn collect_unit_destruction_effects(engine: &mut Engine) {
    engine.destruction_effects.get_or_insert_with(Vec::new);
}

/// Takes each newly destroyed unit once, after both player and AI damage.
pub fn take_unit_destruction_effects(engine: &mut Engine) -> Vec<UnitDestructionEffect> {
    engine
        .destruction_effects
        .as_mut()
        .map(std::mem::take)
        .unwrap_or_default()
}

/// Explosion descriptors for prepared chassis roots and damageable model parts.
pub fn unit_destruction_effect_sources(
    engine: &Engine,
) -> impl Iterator<Item = (String, String)> + '_ {
    engine.loaded.iter().flat_map(|mission| {
        mission
            .object_drafts
            .iter()
            .filter_map(|draft| {
                let class = draft.unit_dat_header_flags?;
                let id = draft.original_id?;
                let table = mission
                    .control_device_tables
                    .iter()
                    .find(|table| table.original_id == Some(id))?;
                let MissionControlNodeProviderTableStatus::Available(rows) =
                    &table.node_provider_status
                else {
                    return None;
                };
                Some(if class & UNIT_DAT_CONTROL_CLASS_HIGH_BIT != 0 {
                    rows.as_slice()
                } else if class == super::boarding::WARBOT_CLASS {
                    &rows[..rows.partition_point(|row| row.unit_component_index == 0)]
                } else if class & MOBILE_UNIT != 0 {
                    rows.get(..1).unwrap_or_default()
                } else {
                    &[]
                })
            })
            .flat_map(|rows| rows.iter().filter_map(node_effect))
    })
}

pub(super) fn part_effect(
    row: &MissionControlNodeProviderRow,
    position: [f32; 3],
) -> Option<UnitDestructionEffect> {
    let (archive, name) = node_effect(row)?;
    Some(UnitDestructionEffect {
        object_index: None,
        position,
        archive,
        name,
    })
}

pub(super) fn pending_effect(
    engine: &Engine,
    handle: ObjectHandle,
) -> Option<UnitDestructionEffect> {
    engine.destruction_effects.as_ref()?;
    let mission = engine.loaded.as_ref()?;
    let table = mission
        .control_device_tables
        .iter()
        .find(|table| table.handle == handle)?;
    let (archive, name) = root_effect(table)?;
    let object_index = identity_metadata(&engine.world, handle)
        .ok()
        .and_then(|identity| identity.original_id)
        .and_then(|id| {
            mission
                .object_drafts
                .iter()
                .position(|draft| draft.original_id == Some(id))
        });
    let position = transform_state(&engine.world, handle)
        .ok()?
        .position
        .map(f32::from_bits);
    Some(UnitDestructionEffect {
        object_index,
        position,
        archive,
        name,
    })
}

fn root_effect(table: &MissionControlDeviceTableState) -> Option<(String, String)> {
    let row = crate::control_node_provider_row(table, 0).ok()?;
    node_effect(row)
}

fn node_effect(row: &MissionControlNodeProviderRow) -> Option<(String, String)> {
    if f32::from_bits(row.initial_amount_bits) <= 0.0 {
        return None;
    }
    let effect = &row.source_row.effect;
    let archive = crate::bounded_ascii_c_string(&effect.archive)?;
    let name = crate::bounded_ascii_c_string(&effect.name)?;
    (!archive.is_empty() && !name.is_empty()).then_some((archive, name))
}
