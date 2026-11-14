use crate::{MissionDocument, MissionScriptBundle};
use fparkan_assets::{ResearchCandidate, ResearchCandidateSet};
use fparkan_path::NormalizedPath;
use fparkan_vfs::Vfs;
use fparkan_world::OwnerId;

pub(super) const TRANSPORT_CLASS: u32 = 0x0100_2000;
pub(super) const SELECT_FASTEST: u32 = 4;

pub(super) fn available(
    vfs: &dyn Vfs,
    mission: &MissionDocument,
    bundles: &[MissionScriptBundle],
) -> Vec<(OwnerId, NormalizedPath)> {
    let clans = bundles.iter().filter(|bundle| {
        mission.clans[bundle.clan_index].mode == 2
            && bundle.package.events.iter().any(|event| {
                event.name_raw == b"PBM_N_OPTIMAL_TRANSPORT_Start\0"
                    && !event.instructions.is_empty()
            })
    });
    let mut clans = clans.peekable();
    if clans.peek().is_none() {
        return Vec::new();
    }
    let directory = crate::normalize_engine_path("AI transport", "UNITS/UNITS/AI")
        .expect("static AI catalog path");
    let paths = vfs.list(&directory).unwrap_or_default();
    let entries: Vec<_> = paths
        .into_iter()
        .filter_map(|entry| {
            let relative = entry.path.as_str().rsplit_once('/')?;
            if !relative.0.eq_ignore_ascii_case(directory.as_str())
                || !relative.1.to_ascii_lowercase().ends_with(".dat")
            {
                return None;
            }
            let bytes = vfs.read(&entry.path).ok()?;
            let header = fparkan_prototype::decode_unit_dat(&bytes)
                .ok()?
                .header_opaque;
            let class = u32::from_le_bytes(header[4..].try_into().unwrap());
            (class == TRANSPORT_CLASS).then_some(ResearchCandidate {
                path: entry.path,
                status: 0,
            })
        })
        .collect();
    clans
        .filter_map(|bundle| {
            let owner = OwnerId(u16::try_from(bundle.clan_index).ok()?);
            let research = crate::research::load_technology_table(vfs, mission, owner).ok()?;
            let mut catalog = ResearchCandidateSet {
                entries: entries.clone(),
                loaded: true,
                ..Default::default()
            };
            catalog.refresh_selection(vfs, Some(&research)).ok()?;
            // shortcut: a unique qualified design needs no speed ranking or random policy.
            let mut available = catalog
                .entries
                .into_iter()
                .filter(|entry| entry.status != 0);
            let selected = available.next()?;
            available.next().is_none().then_some((owner, selected.path))
        })
        .collect()
}
