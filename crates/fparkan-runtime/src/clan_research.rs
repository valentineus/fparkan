use crate::MissionDocument;
use fparkan_assets::{ResearchCandidate, ResearchCandidateSet, ResearchTrfDocument};
use fparkan_path::NormalizedPath;
use fparkan_vfs::Vfs;
use fparkan_world::OwnerId;

pub(super) struct ClanResearch {
    pub(super) candidates: ResearchCandidateSet,
    pub(super) table: ResearchTrfDocument,
}

impl ClanResearch {
    pub(super) fn load(
        vfs: &dyn Vfs,
        mission: &MissionDocument,
        clan: usize,
    ) -> Result<Self, String> {
        let owner = OwnerId(u16::try_from(clan).map_err(|_| "research clan is out of range")?);
        let table = crate::research::load_technology_table(vfs, mission, owner)
            .map_err(|error| error.to_string())?;
        let candidates = ResearchCandidateSet {
            entries: paths(vfs)?
                .into_iter()
                .map(|path| ResearchCandidate { path, status: 0 })
                .collect(),
            loaded: true,
            ..Default::default()
        };
        let mut catalog = Self { candidates, table };
        catalog
            .candidates
            .refresh_selection(vfs, Some(&catalog.table))
            .map_err(|error| error.to_string())?;
        Ok(catalog)
    }
}

pub(super) fn initialize(
    vfs: &dyn Vfs,
    mission: &MissionDocument,
    ai: &mut crate::clan_ai_events::ClanAiEvents,
    bundles: &mut [crate::MissionScriptBundle],
    clan: usize,
    instruction_index: usize,
) {
    match ClanResearch::load(vfs, mission, clan) {
        Ok(catalog) => {
            eprintln!(
                "sim_coverage script_research_catalog_approx=1 clan={clan} entries={} selected={}",
                catalog.candidates.entries.len(),
                catalog.candidates.selected_index
            );
            ai.research_catalogs.insert(clan, catalog);
        }
        Err(reason) => {
            eprintln!("script_research_catalog_stub clan={clan} reason={reason}");
            if let Some(bundle) = bundles.iter_mut().find(|bundle| bundle.clan_index == clan) {
                bundle.init_stubs.push((instruction_index, 43));
            }
        }
    }
}

impl crate::clan_ai_events::ClanAiEvents {
    pub(super) fn research_catalogs(&self) -> impl Iterator<Item = (usize, usize, u32)> + '_ {
        self.research_catalogs.iter().map(|(&clan, catalog)| {
            (
                clan,
                catalog.candidates.entries.len(),
                catalog.candidates.selected_index,
            )
        })
    }
}

pub(super) fn paths(vfs: &dyn Vfs) -> Result<Vec<NormalizedPath>, String> {
    let directory = crate::normalize_engine_path("AI research", "UNITS/UNITS/AI")
        .expect("static AI catalog path");
    let mut paths: Vec<_> = vfs
        .list(&directory)
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter_map(|entry| {
            let (parent, name) = entry.path.as_str().rsplit_once('/')?;
            (parent.eq_ignore_ascii_case(directory.as_str())
                && name.to_ascii_lowercase().ends_with(".dat"))
            .then_some(entry.path)
        })
        .collect();
    // shortcut: reproduce stock NTFS enumeration; other filesystem ordering remains open.
    paths.sort_by_cached_key(|path| path.as_bytes().to_ascii_uppercase());
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::paths;
    use crate::{
        create, load_gameplay_mission, EngineServices, MissionMode, MissionRequest,
        MissionStartupInput,
    };
    use fparkan_vfs::{DirectoryVfs, MemoryVfs};
    use std::sync::Arc;

    #[test]
    fn catalog_uses_direct_dat_files_and_the_observed_ntfs_name_order() {
        let mut vfs = MemoryVfs::default();
        for path in [
            "UNITS/UNITS/AI/23_cpt1.dat",
            "UNITS/UNITS/AI/23sfly1e.dat",
            "UNITS/UNITS/AI/sub/skip.dat",
            "UNITS/UNITS/AI/notes.txt",
            "UNITS/UNITS/AI/12hel1.DAT",
        ] {
            vfs.insert(
                crate::normalize_engine_path("test", path).unwrap(),
                Arc::from([]),
            );
        }
        let actual: Vec<_> = paths(&vfs)
            .unwrap()
            .into_iter()
            .map(|path| path.as_str().to_owned())
            .collect();
        assert_eq!(
            actual,
            [
                "UNITS/UNITS/AI/12hel1.DAT",
                "UNITS/UNITS/AI/23sfly1e.dat",
                "UNITS/UNITS/AI/23_cpt1.dat"
            ]
        );
    }

    #[test]
    #[ignore = "requires licensed corpus and a live native catalog snapshot"]
    fn licensed_catalog_matches_the_running_original() {
        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").unwrap();
        let snapshot =
            std::path::PathBuf::from(std::env::var_os("FPARKAN_NATIVE_AI_CATALOG").unwrap());
        let native = std::fs::read(&snapshot).unwrap();
        assert_eq!(native.len() % 0x124, 0);
        let mut engine = create(EngineServices::new(Arc::new(DirectoryVfs::new(root)))).unwrap();
        load_gameplay_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/Single.02/data.tma".into(),
                mode: MissionMode::SingleMission,
            },
            MissionStartupInput {
                queue_time_ms: 0,
                research_creation_gate: false,
            },
            None,
        )
        .unwrap();
        let catalog = &engine
            .loaded
            .as_ref()
            .unwrap()
            .clan_ai_events
            .research_catalogs[&1];
        assert_eq!(catalog.candidates.entries.len(), native.len() / 0x124);
        for (entry, row) in catalog
            .candidates
            .entries
            .iter()
            .zip(native.chunks_exact(0x124))
        {
            let path = std::str::from_utf8(row[..260].split(|&b| b == 0).next().unwrap())
                .unwrap()
                .replace('\\', "/");
            assert_eq!(entry.path.as_str(), path);
            assert_eq!(entry.status, row[260], "{}", entry.path.as_str());
        }
        let index: u32 = std::fs::read_to_string(snapshot.with_extension("selection"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(catalog.candidates.selected_index, index);
        assert_eq!(catalog.table.row_count(), 368);
        println!(
            "native catalog entries={} selected={} path={}",
            native.len() / 0x124,
            index,
            catalog.candidates.entries[index as usize].path.as_str()
        );
    }
}
