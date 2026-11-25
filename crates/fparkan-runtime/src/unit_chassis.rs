//! Chassis behavior flags shared by autonomous and player control.
use crate::MissionObjectDraft;
use fparkan_resource::{resource_name, CachedResourceRepository, ResourceRepository};
use std::collections::BTreeMap;

pub(super) fn load_flags(
    resources: &CachedResourceRepository,
    drafts: &[MissionObjectDraft],
) -> BTreeMap<u32, Option<bool>> {
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
                .map(|id| (id.0, can_fly(resources, draft)))
        })
        .collect()
}

pub(super) fn can_fly(
    resources: &CachedResourceRepository,
    draft: &MissionObjectDraft,
) -> Option<bool> {
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
    fparkan_assets::decode_chassis_can_fly(resources.read(entry).ok()?.as_slice()).ok()?
}
