//! Chassis behavior flags shared by autonomous and player control.
use crate::{Engine, MissionObjectDraft};
use fparkan_assets::{decode_chassis_flags, ChassisFlags};
use fparkan_resource::{resource_name, CachedResourceRepository, ResourceRepository};
use std::collections::BTreeMap;

pub(super) fn load_flags(
    resources: &CachedResourceRepository,
    drafts: &[MissionObjectDraft],
) -> BTreeMap<u32, ChassisFlags> {
    drafts
        .iter()
        .filter(|draft| {
            draft
                .unit_dat_header_flags
                .is_some_and(|class| class & crate::autodemo::MOBILE_UNIT != 0)
        })
        .filter_map(|draft| {
            draft
                .original_id
                .map(|id| (id.0, load_profile(resources, draft).unwrap_or_default()))
        })
        .collect()
}

pub(super) fn can_fly(
    resources: &CachedResourceRepository,
    draft: &MissionObjectDraft,
) -> Option<bool> {
    load_profile(resources, draft)?.can_fly
}

/// Returns whether a loaded draft has an explicit walking chassis profile.
pub fn loaded_walk_chassis(engine: &Engine, object_index: usize) -> bool {
    loaded_flags(engine, object_index).is_some_and(|flags| flags.walk_chassis == Some(true))
}

/// Returns whether a loaded draft has a tracked chassis profile.
pub fn loaded_tracked_chassis(engine: &Engine, object_index: usize) -> bool {
    const TRACKED_CHASSIS: u32 = 4;
    loaded_flags(engine, object_index)
        .is_some_and(|flags| flags.chassis_type == Some(TRACKED_CHASSIS))
}

fn loaded_flags(engine: &Engine, object_index: usize) -> Option<&ChassisFlags> {
    let mission = engine.loaded.as_ref()?;
    let id = mission.object_drafts.get(object_index)?.original_id?;
    mission.unit_chassis_flags.get(&id.0)
}

fn load_profile(
    resources: &CachedResourceRepository,
    draft: &MissionObjectDraft,
) -> Option<ChassisFlags> {
    let chassis = draft.unit_components.first()?;
    let archive = crate::bounded_ascii_c_string(&chassis.archive_raw)?;
    let registry_path = crate::normalize_engine_path("chassis archive", &archive).ok()?;
    let registry = resources.open_archive(&registry_path).ok()?;
    let name = crate::bounded_ascii_c_string(&chassis.resource_raw)?;
    let entry = resources
        .find(registry, &resource_name(name.as_bytes()))
        .ok()??;
    let references =
        fparkan_prototype::decode_registry_entry(resources.read(entry).ok()?.as_slice()).ok()?;
    // AniMesh supplies the sixth resource pair to CreateBehaviour.
    let profile = references.get(5)?;
    let archive = crate::bounded_ascii_c_string(&profile.archive_raw)?;
    let path = crate::normalize_engine_path("chassis profile archive", &archive).ok()?;
    let name = crate::bounded_ascii_c_string(&profile.resource_raw)?;
    let archive = resources.open_archive(&path).ok()?;
    let entry = resources
        .find(archive, &resource_name(name.as_bytes()))
        .ok()??;
    decode_chassis_flags(resources.read(entry).ok()?.as_slice()).ok()
}
