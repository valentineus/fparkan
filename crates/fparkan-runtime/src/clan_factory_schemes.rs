use crate::{
    boarding::WARBOT_CLASS,
    clan_combat_strength::{rating, weapon_power},
    MissionDocument, MissionObjectDraft, MissionScriptBundle,
};
use fparkan_assets::{AssetManager, ResearchCandidate, ResearchCandidateSet};
use fparkan_path::NormalizedPath;
use fparkan_prototype::build_prototype_graph_report;
use fparkan_resource::ResourceRepository;
use fparkan_vfs::Vfs;
use fparkan_world::OwnerId;
use std::collections::BTreeMap;

pub(super) const TRANSPORT_CLASS: u32 = 0x0100_2000;
pub(super) const SELECT_FASTEST: u32 = 4;
pub(super) const SELECT_STRONGEST: u32 = 5;
pub(super) const AUXILIARY_CLASSES: [u32; 2] =
    [TRANSPORT_CLASS, crate::construction::BUILDER_CLASS];

pub(super) fn available<R: ResourceRepository>(
    assets: &AssetManager<R>,
    vfs: &dyn Vfs,
    mission: &MissionDocument,
    bundles: &[MissionScriptBundle],
) -> Vec<(OwnerId, u32, NormalizedPath)> {
    let clans = bundles.iter().filter(|bundle| {
        mission.clans[bundle.clan_index].mode == 2
            && bundle.package.events.iter().any(|event| {
                matches!(
                    event.name_raw.as_slice(),
                    b"PBM_N_OPTIMAL_TRANSPORT_Start\0" | b"PBM_ROBOT_NEEDED_Start\0"
                ) && !event.instructions.is_empty()
            })
    });
    let mut clans = clans.peekable();
    if clans.peek().is_none() {
        return Vec::new();
    }
    let paths = crate::clan_research::paths(vfs).unwrap_or_default();
    let entries: Vec<_> = paths
        .into_iter()
        .filter_map(|path| {
            let bytes = vfs.read(&path).ok()?;
            let header = fparkan_prototype::decode_unit_dat(&bytes)
                .ok()?
                .header_opaque;
            let class = u32::from_le_bytes(header[4..].try_into().unwrap());
            (class == WARBOT_CLASS || AUXILIARY_CLASSES.contains(&class))
                .then_some((class, ResearchCandidate { path, status: 0 }))
        })
        .collect();
    let mut selected = Vec::new();
    let mut ratings = BTreeMap::new();
    for bundle in clans {
        let Ok(owner) = u16::try_from(bundle.clan_index).map(OwnerId) else {
            continue;
        };
        let Ok(research) = crate::research::load_technology_table(vfs, mission, owner) else {
            continue;
        };
        for class in AUXILIARY_CLASSES.into_iter().chain([WARBOT_CLASS]) {
            let mut catalog = ResearchCandidateSet {
                entries: entries
                    .iter()
                    .filter(|(kind, _)| *kind == class)
                    .map(|(_, entry)| entry.clone())
                    .collect(),
                loaded: true,
                ..Default::default()
            };
            if catalog.refresh_selection(vfs, Some(&research)).is_err() {
                continue;
            }
            let mut available = catalog
                .entries
                .into_iter()
                .filter(|entry| entry.status != 0);
            let design = if class == WARBOT_CLASS {
                let candidates = available.filter_map(|entry| {
                    let score = *ratings
                        .entry(entry.path.clone())
                        .or_insert_with(|| combat_rating(assets, vfs, &entry.path));
                    Some((entry.path, score?))
                });
                best_design(candidates)
            } else {
                // shortcut: select the sole qualified auxiliary until those ranks are wired.
                available
                    .next()
                    .filter(|_| available.next().is_none())
                    .map(|entry| entry.path)
            };
            if let Some(path) = design {
                selected.push((owner, class, path));
            }
        }
    }
    selected
}

fn best_design(candidates: impl Iterator<Item = (NormalizedPath, f32)>) -> Option<NormalizedPath> {
    // The original keeps the earliest candidate when scores tie.
    candidates
        .reduce(|best, next| if next.1 > best.1 { next } else { best })
        .map(|(path, _)| path)
}

fn combat_rating<R: ResourceRepository>(
    assets: &AssetManager<R>,
    vfs: &dyn Vfs,
    path: &NormalizedPath,
) -> Option<f32> {
    let root = crate::resource_name(path.as_bytes());
    let (graph, prototypes, report) =
        build_prototype_graph_report(assets.repository(), vfs, &[root]);
    if !report.is_success() {
        return None;
    }
    let span = graph.root_prototype_request_spans.first()?;
    let flags = *graph.root_unit_dat_header_flags.first()?;
    let mission_assets = assets
        .prepare_mission_assets(&graph.root_prototype_request_spans, &prototypes)
        .ok()?;
    let draft = MissionObjectDraft {
        original_id: None,
        resource_name_raw: path.as_bytes().to_vec(),
        raw_after_resource: u32::MAX,
        identity_or_clan_raw: u32::MAX,
        raw_after_name: 0,
        unit_dat_header_flags: flags,
        position: [0.0; 3],
        orientation_raw: [0.0; 3],
        scale: [1.0; 3],
        visual_ids: mission_assets.visuals_for_object(0).to_vec(),
        properties: Vec::new(),
        unit_components: graph.root_unit_components.into_iter().next()?,
    };
    let class = crate::native_control_class_for_root(flags, Some(span), &prototypes);
    let projection = crate::build_control_device_projection_with_class(
        0,
        &draft,
        &mission_assets,
        Some(span),
        &prototypes,
        class,
        &mut |pair| {
            crate::resolve_factory2_pair_from_assets(assets, vfs, pair)
                .unwrap_or(crate::Factory2PairOutcome::Unrepresentable)
        },
    )
    .ok()?;
    let volume = f32::from_bits(crate::initial_node_provider_amount(
        &projection.node_provider_status,
    )?);
    let resistance = projection
        .projectile_resistance_bits
        .map_or(1.0, |[factor, _]| f32::from_bits(factor));
    // shortcut: selector 0x36 adds a shield term and selector 0x204 reads native bank totals.
    let score = rating(
        volume / resistance,
        weapon_power(&projection.devices, &projection.node_provider_status),
    );
    score.is_finite().then_some(score)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strongest_design_keeps_the_first_tied_entry() {
        let candidates = [("light.dat", 4.0), ("heavy.dat", 8.0), ("other.dat", 8.0)]
            .into_iter()
            .map(|(name, score)| {
                (
                    crate::normalize_engine_path("test design", name).unwrap(),
                    score,
                )
            });
        assert_eq!(best_design(candidates).unwrap().as_str(), "heavy.dat");
    }
}
