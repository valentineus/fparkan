use crate::{BuildCategory, MissionObjectDraft};
use fparkan_assets::ResearchTrfDocument;
use fparkan_vfs::Vfs;
use fparkan_world::{identity_metadata, ObjectHandle, World};

const BUILDING_FLAG: u32 = 0x8000_0000;

pub(super) fn needs_upgrade_selection(
    world: &World,
    units: impl Iterator<Item = ObjectHandle>,
    drafts: &[MissionObjectDraft],
    categories: &[BuildCategory],
    research: Option<(&dyn Vfs, &ResearchTrfDocument)>,
    index: u32,
    learned: impl Fn(usize) -> bool,
) -> bool {
    units
        .filter(|&handle| {
            let Some(draft) = identity_metadata(world, handle)
                .ok()
                .and_then(|identity| identity.original_id)
                .and_then(|id| drafts.get(id.0 as usize))
            else {
                return true;
            };
            let Some(class) = draft.unit_dat_header_flags else {
                return true;
            };
            if !upgradeable_class(class) {
                return false;
            }
            let Some(category) = categories.iter().find(|category| category.mask == class) else {
                return false;
            };
            let Some(stage) = draft
                .unit_components
                .first()
                .and_then(|root| initial_stage(&root.resource_raw))
            else {
                return true;
            };
            let Some(next) = category.unit_paths.get(stage + 1) else {
                return false;
            };
            !research.is_some_and(|(vfs, table)| {
                root_research_row(vfs, table, next).is_some_and(|row| {
                    table
                        .flag_byte(row)
                        .is_some_and(|flags| flags & crate::research::RESEARCHED == 0)
                        && !learned(row)
                })
            })
        })
        .nth(index as usize)
        .is_some()
}

fn upgradeable_class(class: u32) -> bool {
    class & BUILDING_FLAG != 0
        && !matches!(
            class,
            0x8000_0002
                | 0x8000_0040
                | 0x8000_0200
                | 0x8000_1000
                | 0x8000_2000
                | 0x8001_0000
                | 0x8002_0000
                | 0x8004_0000
        )
}

fn initial_stage(root: &[u8]) -> Option<usize> {
    if !root.get(..3)?.eq_ignore_ascii_case(b"fr_") {
        return None;
    }
    match root.get(3)?.to_ascii_lowercase() {
        b'l' => Some(0),
        b'm' => Some(1),
        b'b' => Some(2),
        b'e' => Some(3),
        _ => None,
    }
}

fn root_research_row(vfs: &dyn Vfs, table: &ResearchTrfDocument, path: &str) -> Option<usize> {
    let path = crate::normalize_engine_path("next building scheme", path).ok()?;
    let bytes = vfs.read(&path).ok()?;
    let unit = fparkan_prototype::decode_unit_dat(&bytes).ok()?;
    let root = unit.records.first()?;
    if root.kind == 0 {
        return None;
    }
    let end = root.resource_raw.iter().position(|&byte| byte == 0)?;
    table.find_key(&root.resource_raw[..end]).ok()?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upgrade_uses_zero_based_stage_and_excludes_native_building_classes() {
        assert_eq!(initial_stage(b"fr_l_plant"), Some(0));
        assert_eq!(initial_stage(b"FR_M_PLANT"), Some(1));
        assert_eq!(initial_stage(b"fr_b_plant"), Some(2));
        assert_eq!(initial_stage(b"fr_e_inst"), Some(3));
        assert_eq!(initial_stage(b"unknown_building"), None);
        assert!(upgradeable_class(0x8000_0010));
        assert!(upgradeable_class(0x8000_0400));
        assert!(upgradeable_class(0x8010_0000));
        assert!(!upgradeable_class(0x8000_0002));
        assert!(!upgradeable_class(0x8000_0200));
        assert!(!upgradeable_class(0x8002_0000));
        assert!(!upgradeable_class(0x0100_4000));
    }

    #[test]
    #[ignore = "requires licensed building schemes and technology table"]
    fn licensed_factory_is_excluded_until_its_next_root_is_researched() {
        use fparkan_vfs::DirectoryVfs;
        use fparkan_world::{construct_object, new, ObjectDraft, OriginalObjectId, WorldConfig};

        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").unwrap();
        let vfs = DirectoryVfs::new(root);
        let read = |name| {
            vfs.read(&crate::normalize_engine_path("test", name).unwrap())
                .unwrap()
        };
        let table = ResearchTrfDocument::decode(read("MISSIONS/SCRIPTS/scream.trf")).unwrap();
        let categories = [BuildCategory {
            name: "Plant".into(),
            mask: 0x8000_0010,
            unit_paths: vec![
                "UNITS/BUILDS/PLANT/splant01.dat".into(),
                "UNITS/BUILDS/PLANT/mplant01.dat".into(),
            ],
        }];
        let unit =
            fparkan_prototype::decode_unit_dat(&read("UNITS/BUILDS/PLANT/splant01.dat")).unwrap();
        let id = OriginalObjectId(0);
        let draft = MissionObjectDraft {
            original_id: Some(id),
            resource_name_raw: b"UNITS/BUILDS/PLANT/splant01.dat".to_vec(),
            raw_after_resource: 0,
            identity_or_clan_raw: 0,
            raw_after_name: 0,
            unit_dat_header_flags: Some(u32::from_le_bytes(
                unit.header_opaque[4..].try_into().unwrap(),
            )),
            position: [0.0; 3],
            orientation_raw: [0.0; 3],
            scale: [1.0; 3],
            visual_ids: vec![],
            properties: vec![],
            unit_components: unit.records,
        };
        let mut world = new(WorldConfig);
        let handle = construct_object(
            &mut world,
            ObjectDraft {
                original_id: Some(id),
            },
        )
        .unwrap();
        let row = table.find_key(b"fr_m_plant").unwrap().unwrap();
        let query = |index, unlocked| {
            needs_upgrade_selection(
                &world,
                [handle].into_iter(),
                std::slice::from_ref(&draft),
                &categories,
                Some((&vfs, &table)),
                index,
                |candidate| unlocked && candidate == row,
            )
        };
        assert!(!query(0, false));
        assert!(query(0, true));
        assert!(!query(1, true));
    }
}
