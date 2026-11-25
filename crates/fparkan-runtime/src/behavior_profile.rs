//! Registered Behavior profile values loaded from the class profile archive.
use crate::{
    normalize_engine_path, read_vfs, EngineError, MissionObjectDraft, PreparedBehaviorProfileState,
    HERO_UNIT_CLASS,
};
use fparkan_assets::{decode_nres_payload, BehaviorProfileRates, BehaviorProfileVarError};
use fparkan_vfs::Vfs;
use std::{collections::HashMap, sync::Arc};

pub(super) fn behavior_profile_entry_for_class(class_flags: u32) -> Option<&'static str> {
    match class_flags {
        0x8000_0004 => Some("prof_mine.var"),
        0x8000_0008 => Some("prof_storage.var"),
        0x8000_0002 => Some("prof_generator.var"),
        0x8000_0010 => Some("prof_plant.var"),
        0x8000_0040 => Some("prof_angar.var"),
        0x8000_0200 => Some("prof_mtp.var"),
        0x8000_0400 => Some("prof_institute.var"),
        0x8001_0000 | 0x8002_0000 | 0x8004_0000 => Some("prof_bunker.var"),
        0x8010_0000 | 0x8020_0000 => Some("prof_tower.var"),
        0x0100_2000 => Some("prof_trn.var"),
        0x0100_4000 => Some("prof_bld.var"),
        0x0100_8000 => Some("prof_war.var"),
        0x0101_0000 => Some("prof_hq.var"),
        HERO_UNIT_CLASS => Some("prof_hero.var"),
        0x2000_0000 => Some("prof_animal.var"),
        _ => None,
    }
}

pub(super) fn prepare_behavior_profile_states(
    vfs: &Arc<dyn Vfs>,
    object_drafts: &[MissionObjectDraft],
) -> Result<Vec<Option<PreparedBehaviorProfileState>>, EngineError> {
    const DEFAULT_TRANSFER_ORE_OFF_BOARD_BITS: u32 = 0x40a0_0000;
    const DEFAULT_TRANSFER_POWER_OUT_BITS: u32 = 0x42c8_0000;

    let has_known_profile = object_drafts.iter().any(|draft| {
        draft
            .unit_dat_header_flags
            .and_then(behavior_profile_entry_for_class)
            .is_some()
    });
    let archive_path = has_known_profile
        .then(|| normalize_engine_path("Behavior profile archive", "behpsp.res"))
        .transpose()?;
    let profile_archive = archive_path
        .as_ref()
        .map(|path| {
            let bytes = read_vfs(vfs, path)?;
            decode_nres_payload(bytes).map_err(|source| EngineError::Nres {
                path: path.as_str().to_string(),
                source,
            })
        })
        .transpose()?;
    let mut values_by_entry = HashMap::<&'static str, (BehaviorProfileRates, u32)>::new();
    let mut states = Vec::with_capacity(object_drafts.len());

    for draft in object_drafts {
        let Some(class_flags) = draft.unit_dat_header_flags else {
            states.push(None);
            continue;
        };
        let Some(original_id) = draft.original_id else {
            states.push(None);
            continue;
        };
        let profile_entry = behavior_profile_entry_for_class(class_flags);
        let (transfer_ore_off_board_bits, transfer_power_out_bits, go_speed_percent_bits) =
            if let Some(entry_name) = profile_entry {
                let rates = if let Some(rates) = values_by_entry.get(entry_name) {
                    *rates
                } else {
                    let path = archive_path
                        .as_ref()
                        .expect("known profiles require the profile archive path");
                    let archive = profile_archive
                        .as_ref()
                        .expect("known profiles require the decoded profile archive");
                    let entry_id =
                        archive
                            .find(entry_name)
                            .ok_or_else(|| EngineError::BehaviorProfile {
                                path: path.as_str().to_string(),
                                entry: entry_name.to_string(),
                                message: "entry was not found".to_string(),
                            })?;
                    let payload =
                        archive
                            .payload(entry_id)
                            .map_err(|source| EngineError::Nres {
                                path: path.as_str().to_string(),
                                source,
                            })?;
                    let decode_error =
                        |source: BehaviorProfileVarError| EngineError::BehaviorProfile {
                            path: path.as_str().to_string(),
                            entry: entry_name.to_string(),
                            message: source.to_string(),
                        };
                    let rates = (
                        fparkan_assets::decode_behavior_profile_rates(payload)
                            .map_err(&decode_error)?,
                        fparkan_assets::decode_behavior_go_speed_percent(payload)
                            .map_err(&decode_error)?,
                    );
                    values_by_entry.insert(entry_name, rates);
                    rates
                };
                (
                    rates.0.transfer_ore_off_board_bits,
                    rates.0.transfer_power_out_bits,
                    rates.1,
                )
            } else {
                (
                    DEFAULT_TRANSFER_ORE_OFF_BOARD_BITS,
                    DEFAULT_TRANSFER_POWER_OUT_BITS,
                    1.0_f32.to_bits(),
                )
            };
        states.push(Some(PreparedBehaviorProfileState {
            original_id,
            class_flags,
            profile_entry,
            transfer_ore_off_board_bits,
            transfer_power_out_bits,
            go_speed_percent_bits,
        }));
    }
    Ok(states)
}
