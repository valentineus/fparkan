use crate::MissionDocument;
use fparkan_assets::{ResearchCandidate, ResearchCandidateSet, ResearchTrfDocument};
use fparkan_path::NormalizedPath;
use fparkan_prototype::decode_unit_dat;
use fparkan_resource::{ArchiveId, ResourceRepository};
use fparkan_vfs::Vfs;
use fparkan_world::OwnerId;

pub(super) struct ClanResearch {
    pub(super) candidates: ResearchCandidateSet,
    pub(super) table: ResearchTrfDocument,
    pub(super) refreshes: u64,
    assault_excluded: Vec<bool>,
}

pub(super) fn scan_status(candidates: Option<&ResearchCandidateSet>) -> u32 {
    u32::from(candidates.is_some_and(|candidates| candidates.status != 0))
}

impl ClanResearch {
    pub(super) fn load(
        vfs: &dyn Vfs,
        repository: &impl ResourceRepository,
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
        let registry_path = crate::normalize_engine_path("AI research", "objects.rlb")
            .expect("static registry path");
        let registry = repository.open_archive_unchanged(&registry_path).ok();
        let assault_excluded = candidates
            .entries
            .iter()
            .map(|entry| {
                excluded_from_assault(vfs, repository, registry, &entry.path).unwrap_or(false)
            })
            .collect();
        let mut catalog = Self {
            candidates,
            table,
            refreshes: 0,
            assault_excluded,
        };
        catalog.refresh(vfs)?;
        Ok(catalog)
    }

    pub(super) fn refresh(&mut self, vfs: &dyn Vfs) -> Result<(), String> {
        self.refreshes += 1;
        self.candidates
            .refresh_selection(vfs, Some(&self.table))
            .map_err(|error| error.to_string())
    }

    pub(super) fn assault_needs_weapon_metadata(&self) -> bool {
        self.candidates.loaded
            && self
                .candidates
                .entries
                .iter()
                .zip(&self.assault_excluded)
                .any(|(candidate, &excluded)| candidate.status != 0 && !excluded)
    }
}

fn excluded_from_assault(
    vfs: &dyn Vfs,
    repository: &impl ResourceRepository,
    registry: Option<ArchiveId>,
    path: &NormalizedPath,
) -> Option<bool> {
    let bytes = vfs.read(path).ok()?;
    let unit = decode_unit_dat(&bytes).ok()?;
    let flags = u32::from_le_bytes(unit.header_opaque[4..].try_into().ok()?);
    if flags & crate::UNIT_DAT_CONTROL_CLASS_HIGH_BIT != 0 {
        return Some(true);
    }
    if !matches!(
        flags,
        crate::boarding::WARBOT_CLASS
            | crate::construction::BUILDER_CLASS
            | crate::clan_factory_schemes::TRANSPORT_CLASS
    ) {
        return None;
    }
    let root = unit.records.first()?;
    let archive = root.archive_raw.split(|&byte| byte == 0).next()?;
    if !archive.eq_ignore_ascii_case(b"objects.rlb") {
        return None;
    }
    let name = root.resource_raw.split(|&byte| byte == 0).next()?;
    let entry = repository
        .find(registry?, &crate::resource_name(name))
        .ok()??;
    let kind = repository.entry_info(entry).ok()?.key.type_id;
    (kind == Some(crate::CONTROL_CLASS_SOURCE_BTLU)).then(|| chassis_size(name) < 3)
}

fn chassis_size(name: &[u8]) -> u32 {
    match name.get(2).map(u8::to_ascii_lowercase) {
        Some(b't') => 1,
        Some(b'h' | b'l') => 2,
        Some(b'm') => 3,
        Some(b'b') => 4,
        _ => 0,
    }
}

pub(super) fn initialize(
    vfs: &dyn Vfs,
    repository: &impl ResourceRepository,
    mission: &MissionDocument,
    ai: &mut crate::clan_ai_events::ClanAiEvents,
    bundles: &mut [crate::MissionScriptBundle],
    clan: usize,
    instruction_index: usize,
) {
    match ClanResearch::load(vfs, repository, mission, clan) {
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
    pub(super) fn research_catalogs(&self) -> impl Iterator<Item = (usize, usize, u32, u64)> + '_ {
        self.research_catalogs.iter().map(|(&clan, catalog)| {
            (
                clan,
                catalog.candidates.entries.len(),
                catalog.candidates.selected_index,
                catalog.refreshes,
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
    fn assault_size_comes_from_the_zero_based_third_byte_of_a_mobile_root() {
        for (root, size) in [
            (b"R_T_01".as_slice(), 1),
            (b"r_h_02", 2),
            (b"R_L_03", 2),
            (b"R_M_04", 3),
            (b"r_b_03", 4),
            (b"R_E_42", 0),
            (b"R_", 0),
        ] {
            assert_eq!(super::chassis_size(root), size);
        }
    }

    #[test]
    fn status_query_reads_the_cached_scan_without_reexamining_candidates() {
        use fparkan_assets::{ResearchCandidate, ResearchCandidateSet};
        let mut candidates = ResearchCandidateSet {
            entries: vec![ResearchCandidate {
                path: crate::normalize_engine_path("test", "UNITS/finished.dat").unwrap(),
                status: 1,
            }],
            loaded: true,
            ..Default::default()
        };
        // Finishing the last candidate does not change the preceding scan's flag.
        assert_eq!(super::scan_status(Some(&candidates)), 0);
        candidates.status = 255;
        assert_eq!(super::scan_status(Some(&candidates)), 1);
        // The native query neither rescans entries nor checks the loaded flag.
        candidates.entries[0].status = 0;
        candidates.loaded = false;
        assert_eq!(super::scan_status(Some(&candidates)), 1);
        assert_eq!(super::scan_status(None), 0);
    }

    #[test]
    #[ignore = "requires licensed scenario resources"]
    fn licensed_scream_scan_flag_gates_the_laboratory_request() {
        use fparkan_assets::ResearchCandidateSet;
        use fparkan_script::{parse_varset, ScriptDispatchSelector, VarSetDefault};
        let root = std::path::PathBuf::from(std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").unwrap());
        let scripts = root.join("MISSIONS/SCRIPTS");
        let varset = parse_varset(&std::fs::read(scripts.join("varset.var")).unwrap()).unwrap();
        let package =
            fparkan_script::decode(&std::fs::read(scripts.join("scream.scr")).unwrap()).unwrap();
        let mut branch = package
            .events
            .into_iter()
            .find(|event| event.name_raw == b"Problems0\0")
            .unwrap();
        let query = branch
            .instructions
            .iter()
            .position(|instruction| {
                instruction.dispatch_selector() == ScriptDispatchSelector::Handler(56)
            })
            .unwrap();
        let request = branch.instructions[query + 1..]
            .iter()
            .find(|instruction| {
                instruction.dispatch_selector() == ScriptDispatchSelector::Handler(2)
            })
            .unwrap()
            .clone();
        assert_eq!(
            varset.declarations[request.references[4] as usize].name,
            "BUILDING_INSTITUTE"
        );
        // Isolate the shipped scan condition and request from unrelated formula assignments.
        branch.instructions = vec![
            branch.instructions[query].clone(),
            branch.instructions[query + 1].clone(),
            request,
        ];
        for (status, expected_requests) in [(0, 1), (1, 0), (255, 0)] {
            let candidates = ResearchCandidateSet {
                status,
                ..Default::default()
            };
            let mut values = varset
                .declarations
                .iter()
                .map(|cell| cell.default_value)
                .collect::<Vec<_>>();
            let mut requests = 0;
            varset
                .execute_event_prefix(
                    &branch,
                    None,
                    &mut values,
                    |instruction, _| match instruction.dispatch_selector() {
                        ScriptDispatchSelector::Handler(56) => Ok(Some(VarSetDefault::Dword(
                            super::scan_status(Some(&candidates)),
                        ))),
                        ScriptDispatchSelector::Handler(2) => {
                            requests += 1;
                            Ok(None)
                        }
                        other => Err(format!("unexpected callback {other:?}")),
                    },
                )
                .unwrap();
            assert_eq!(requests, expected_requests, "cached scan status {status}");
        }
    }

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
        for (index, (entry, row)) in catalog
            .candidates
            .entries
            .iter()
            .zip(native.chunks_exact(0x124))
            .enumerate()
        {
            let path = std::str::from_utf8(row[..260].split(|&b| b == 0).next().unwrap())
                .unwrap()
                .replace('\\', "/");
            assert_eq!(entry.path.as_str(), path);
            assert_eq!(entry.status, row[260], "{}", entry.path.as_str());
            let flags = u32::from_le_bytes(row[0x108..0x10c].try_into().unwrap());
            let size = u32::from_le_bytes(row[0x10c..0x110].try_into().unwrap());
            assert_eq!(
                catalog.assault_excluded[index],
                flags & crate::UNIT_DAT_CONTROL_CLASS_HIGH_BIT != 0 || size < 3,
                "{path} size {size}"
            );
        }
        assert!(!catalog.assault_needs_weapon_metadata());
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
