use crate::{MissionDocument, MissionScriptBundle};
use fparkan_assets::{ResearchCandidate, ResearchCandidateSet};
use fparkan_path::NormalizedPath;
use fparkan_vfs::Vfs;
use fparkan_world::OwnerId;

pub(super) const TRANSPORT_CLASS: u32 = 0x0100_2000;
pub(super) const SELECT_FASTEST: u32 = 4;
pub(super) const AUXILIARY_CLASSES: [u32; 2] =
    [TRANSPORT_CLASS, crate::construction::BUILDER_CLASS];

pub(super) fn available(
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
            AUXILIARY_CLASSES
                .contains(&class)
                .then_some((class, ResearchCandidate { path, status: 0 }))
        })
        .collect();
    let mut selected = Vec::new();
    for bundle in clans {
        let Ok(owner) = u16::try_from(bundle.clan_index).map(OwnerId) else {
            continue;
        };
        let Ok(research) = crate::research::load_technology_table(vfs, mission, owner) else {
            continue;
        };
        for class in AUXILIARY_CLASSES {
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
            // shortcut: select unique qualified designs until native ranking and RNG are wired.
            let mut available = catalog
                .entries
                .into_iter()
                .filter(|entry| entry.status != 0);
            if let Some(entry) = available.next().filter(|_| available.next().is_none()) {
                selected.push((owner, class, entry.path));
            }
        }
    }
    selected
}
