//! Applies slot-preserving changes to a prepared player factory scheme.

use crate::{
    load_mission_with_options, normalize_engine_path, read_vfs, Engine, EngineError, LoadedMission,
    MissionLoadOptions, MissionMode, MissionRequest, MissionStartupInput,
};
use fparkan_path::NormalizedPath;
use fparkan_prototype::{decode_unit_dat, unit_component_tree};
use fparkan_vfs::{MemoryVfs, OverlayVfs, Vfs};
use std::sync::Arc;

/// Loads the gameplay world and prepares the player's optional Unit DAT design.
///
/// # Errors
///
/// Returns an error if the mission or selected design cannot be loaded.
pub fn load_gameplay_mission(
    engine: &mut Engine,
    request: MissionRequest,
    startup_input: MissionStartupInput,
    build_scheme: Option<&str>,
) -> Result<LoadedMission, EngineError> {
    load_gameplay_mission_with_build_parts(engine, request, startup_input, build_scheme, &[])
}

/// Loads a mission with replacements for internal parts of the selected player scheme.
///
/// # Errors
///
/// Returns an error if the replacements have no compatible slot or the design
/// or mission cannot be loaded.
pub fn load_gameplay_mission_with_build_parts(
    engine: &mut Engine,
    request: MissionRequest,
    startup_input: MissionStartupInput,
    build_scheme: Option<&str>,
    changes: &[(String, String)],
) -> Result<LoadedMission, EngineError> {
    if build_scheme.is_some() && request.mode != MissionMode::SingleMission {
        return Err(invalid("build schemes require a player mission"));
    }
    if build_scheme.is_none() && !changes.is_empty() {
        return Err(invalid("component replacements require a base scheme"));
    }
    let build_scheme = build_scheme
        .map(|path| normalize_engine_path("factory Unit DAT", path))
        .transpose()?;
    load_mission_with_options(
        engine,
        request,
        MissionLoadOptions {
            allow_partial_init: true,
            start_autodemo: true,
            ..MissionLoadOptions::default()
        },
        Some(startup_input),
        build_scheme.as_ref().map(|path| (path, changes)),
    )
}

const HEADER_BYTES: usize = 8;
const RECORD_BYTES: usize = 112;
const RESOURCE_KEY: std::ops::Range<usize> = 32..64;
const DESCRIPTION: std::ops::Range<usize> = 72..104;

pub(super) fn prepare(
    vfs: Arc<dyn Vfs>,
    source: &NormalizedPath,
    changes: &[(String, String)],
) -> Result<(Arc<dyn Vfs>, NormalizedPath), EngineError> {
    let mut bytes = read_vfs(&vfs, source)?.to_vec();
    let scheme = decode_unit_dat(&bytes).map_err(invalid)?;
    unit_component_tree(&scheme.records).map_err(invalid)?;
    for (index, (from, to)) in changes.iter().enumerate() {
        validate_change(from, to)?;
        if changes[..index]
            .iter()
            .any(|(other, _)| other.eq_ignore_ascii_case(from))
        {
            return Err(invalid(format!(
                "component {from} has already been replaced"
            )));
        }
        let mut matched = false;
        for (index, part) in scheme.records.iter().enumerate().skip(1) {
            if !key(&part.resource_raw).eq_ignore_ascii_case(from.as_bytes()) {
                continue;
            }
            if part.kind != 1
                || part.immediate_child_count != 0
                || !key(&part.archive_raw).eq_ignore_ascii_case(b"objects.rlb")
            {
                return Err(invalid(format!(
                    "component {from} is not a replaceable leaf"
                )));
            }
            let offset = HEADER_BYTES + index * RECORD_BYTES;
            let record = &mut bytes[offset..offset + RECORD_BYTES];
            let mut replacement = [0; 32];
            replacement[..to.len()].copy_from_slice(to.as_bytes());
            record[RESOURCE_KEY].copy_from_slice(&replacement);
            record[DESCRIPTION].copy_from_slice(&replacement);
            matched = true;
        }
        if !matched {
            return Err(invalid(format!(
                "scheme has no replaceable component {from}"
            )));
        }
    }

    let path = normalize_engine_path("custom robot Unit DAT", "fparkan/custom.dat")?;
    let mut overlay = MemoryVfs::default();
    overlay.insert(path.clone(), bytes.into());
    Ok((
        Arc::new(OverlayVfs::from_layers(vec![Arc::new(overlay), vfs])),
        path,
    ))
}

fn key(raw: &[u8; 32]) -> &[u8] {
    raw.split(|&byte| byte == 0).next().unwrap_or_default()
}

fn family(resource: &str) -> Option<&str> {
    let (family, variant) = resource.rsplit_once('_')?;
    (family.len() > 2
        && resource.len() < 32
        && variant.len() == 2
        && family
            .get(..2)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("i_"))
        && resource
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'))
    .then_some(family)
}

fn validate_change(from: &str, to: &str) -> Result<(), EngineError> {
    if family(from)
        .zip(family(to))
        .is_none_or(|(a, b)| !a.eq_ignore_ascii_case(b))
    {
        return Err(invalid(format!(
            "components {from} and {to} must belong to the same internal family"
        )));
    }
    if from.eq_ignore_ascii_case(to) {
        return Err(invalid(format!(
            "replacement for {from} must select another component"
        )));
    }
    Ok(())
}

fn invalid(error: impl std::fmt::Display) -> EngineError {
    EngineError::Production(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::validate_change;

    #[test]
    fn design_changes_keep_the_component_family_and_chassis_size() {
        assert!(validate_change("i_pws_b_02", "I_PWS_B_01").is_ok());
        for (from, to) in [
            ("i_pws_b_02", "i_pws_l_02"),
            ("i_pws_b_02", "i_eng_b_02"),
            ("i_pws_b_02", "i_pws_b_02"),
            ("i_pws_b_02", "../i_pws_b_02"),
            ("e_tur_bb_01", "e_tur_bb_02"),
        ] {
            assert!(validate_change(from, to).is_err());
        }
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_constructor_spawns_a_modified_scheme_with_known_parts() {
        use crate::{
            capture_neutral_building, create, factory_schemes, frame, loaded_mission_object_drafts,
            order_factory_robot, starting_cockpit, EngineServices,
        };
        use fparkan_vfs::DirectoryVfs;
        use fparkan_world::{set_transform, transform_state, OriginalObjectId};
        use std::sync::Arc;

        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed root");
        let vfs = Arc::new(DirectoryVfs::new(std::path::PathBuf::from(root)));
        for unknown_armor in [true, false] {
            let mut engine = create(EngineServices::new(vfs.clone())).unwrap();
            let mut parts = vec![
                ("i_pws_b_02".into(), "i_pws_b_01".into()),
                ("i_rdr_b_02".into(), "i_rdr_b_01".into()),
            ];
            if unknown_armor {
                parts.push(("i_arm_b_df".into(), "i_arm_b_03".into()));
            }
            super::load_gameplay_mission_with_build_parts(
                &mut engine,
                crate::MissionRequest {
                    key: "MISSIONS/CAMPAIGN/CAMPAIGN.00/Mission.02/data.tma".into(),
                    mode: crate::MissionMode::SingleMission,
                },
                crate::MissionStartupInput {
                    queue_time_ms: 0,
                    research_creation_gate: false,
                },
                Some("UNITS/UNITS/PREBLD/tut2_f1.dat"),
                &parts,
            )
            .unwrap();
            frame(&mut engine, 0).unwrap();
            let hero = starting_cockpit(&engine).unwrap();
            let mut pose = transform_state(&engine.world, hero.handle).unwrap();
            pose.position = [391.3_f32, 740.1, 140.74].map(f32::to_bits);
            set_transform(&mut engine.world, hero.handle, pose).unwrap();
            assert_eq!(
                capture_neutral_building(&mut engine, hero).unwrap(),
                Some(OriginalObjectId(0))
            );
            assert_eq!(
                factory_schemes(&engine, hero).unwrap(),
                [(0, "fparkan/custom.dat")]
            );
            let order = order_factory_robot(&mut engine, hero, 1);
            if unknown_armor {
                assert!(order.unwrap_err().to_string().contains("i_arm_b_03"));
                continue;
            }
            assert_eq!(order.unwrap(), 60_000);
            let robot = (60_001..80_000)
                .step_by(100)
                .find_map(|now| frame(&mut engine, now).unwrap().produced_unit)
                .expect("factory released the custom scheme");
            let draft = &loaded_mission_object_drafts(&engine).unwrap()[robot.object_index];
            for (previous, new) in &parts {
                assert!(draft
                    .unit_components
                    .iter()
                    .any(|part| super::key(&part.resource_raw) == new.as_bytes()));
                assert!(!draft
                    .unit_components
                    .iter()
                    .any(|part| super::key(&part.resource_raw) == previous.as_bytes()));
            }
        }
    }
}
