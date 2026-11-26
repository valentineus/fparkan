//! Presentation events for projectile damage that destroys a unit.

use crate::{autodemo::MOBILE_UNIT, Engine, MissionControlDeviceTableState, ObjectHandle};
use fparkan_world::{identity_metadata, transform_state};

/// Position and attached EXPL resource of a chassis at lethal contact.
pub struct UnitDestructionEffect {
    /// Prepared draft index for the destroyed actor when it has an authored layout.
    pub object_index: Option<usize>,
    /// World-space origin of the destroyed chassis.
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

/// One explosion descriptor per prepared mobile chassis, including reserves.
pub fn unit_destruction_effect_sources(
    engine: &Engine,
) -> impl Iterator<Item = (String, String)> + '_ {
    engine.loaded.iter().flat_map(|mission| {
        mission.object_drafts.iter().filter_map(|draft| {
            if !draft
                .unit_dat_header_flags
                .is_some_and(|class| class & MOBILE_UNIT != 0)
            {
                return None;
            }
            let id = draft.original_id?;
            let table = mission
                .control_device_tables
                .iter()
                .find(|table| table.original_id == Some(id))?;
            root_effect(table)
        })
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
    let effect = &row.source_row.effect;
    let archive = crate::bounded_ascii_c_string(&effect.archive)?;
    let name = crate::bounded_ascii_c_string(&effect.name)?;
    (!archive.is_empty() && !name.is_empty()).then_some((archive, name))
}
