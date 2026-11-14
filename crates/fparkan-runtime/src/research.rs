use super::{
    mission_events::on_communicator, normalize_engine_path, read_vfs, ClanBody, Engine,
    EngineError, LoadedMissionState, MissionObjectDraft, StartingCockpit,
};
use fparkan_assets::ResearchTrfDocument;
use fparkan_vfs::Vfs;
use fparkan_world::{
    handle_by_original_id, identity_metadata, transform_state, ObjectHandle, OwnerId, World,
};
use std::collections::HashSet;
use std::sync::Arc;

const LAB_CLASS: u32 = 0x8000_0400;
pub(super) const RESEARCH_AVAILABLE: u8 = 1;
pub(super) const RESEARCHED: u8 = 2;

#[derive(Default)]
pub(super) struct FreeResearch {
    remaining: Vec<(usize, u32)>,
    pending: Option<Order>,
    pub(super) learned: HashSet<(OwnerId, usize)>,
}

struct Order {
    lab: ObjectHandle,
    owner: OwnerId,
    row: usize,
    component: String,
    start_ms: u32,
    duration_ms: u32,
}

impl FreeResearch {
    pub(super) fn new(drafts: &[MissionObjectDraft]) -> Self {
        let remaining = drafts
            .iter()
            .enumerate()
            .filter_map(|(index, draft)| {
                if draft.unit_dat_header_flags != Some(LAB_CLASS) {
                    return None;
                }
                let count = property(draft, b"FreeTechnoNum")? as i32;
                (count > 0).then_some((index, count as u32))
            })
            .collect();
        Self {
            remaining,
            ..Self::default()
        }
    }

    pub(super) fn tick(&mut self, world: &World, now_ms: u32) -> Option<String> {
        let order = self.pending.as_ref()?;
        if !identity_metadata(world, order.lab)
            .is_ok_and(|identity| identity.owner_id == Some(order.owner))
        {
            self.pending = None;
            return None;
        }
        if now_ms.wrapping_sub(order.start_ms) < order.duration_ms {
            return None;
        }
        let order = self.pending.take()?;
        self.learned.insert((order.owner, order.row));
        Some(order.component)
    }
}

/// Starts the first available free technology at the player's laboratory.
///
/// # Errors
/// Returns a diagnostic if the lab, research slot, or clan table is unavailable.
pub fn order_lab_research(
    engine: &mut Engine,
    cockpit: StartingCockpit,
    now_ms: u32,
) -> Result<(String, u32), EngineError> {
    let fail = |message: &str| EngineError::Production(message.into());
    let mission = engine
        .loaded
        .as_mut()
        .ok_or_else(|| fail("no loaded mission"))?;
    if mission.free_research.pending.is_some() {
        return Err(fail("laboratory research already in progress"));
    }
    let owner = identity_metadata(&engine.world, cockpit.handle)?
        .owner_id
        .ok_or_else(|| fail("player clan is unavailable"))?;
    let position = transform_state(&engine.world, cockpit.handle)?
        .position
        .map(f32::from_bits);
    let (quota, lab, draft) = mission
        .free_research
        .remaining
        .iter()
        .enumerate()
        .find_map(|(slot, &(index, count))| {
            if count == 0 {
                return None;
            }
            let draft = &mission.object_drafts[index];
            let lab = handle_by_original_id(&engine.world, draft.original_id?)?;
            (identity_metadata(&engine.world, lab).ok()?.owner_id == Some(owner)
                && on_communicator(
                    &mission.mission_assets,
                    index,
                    LAB_CLASS,
                    transform_state(&engine.world, lab).ok()?,
                    position,
                ))
            .then_some((slot, lab, draft))
        })
        .ok_or_else(|| {
            fail("stand on a friendly laboratory communicator with free research remaining")
        })?;
    let duration = f32::from_bits(property(draft, b"FreeResearchTime").unwrap_or_default());
    if !duration.is_finite() || duration <= 0.0 || duration >= u32::MAX as f32 / 1000.0 {
        return Err(fail("laboratory research time is unavailable"));
    }
    let owned;
    let research = match mission
        .research_trf_tables
        .iter()
        .find(|table| table.clan_index == usize::from(owner.0))
    {
        Some(table) => &table.document,
        None => {
            owned = load_clan_table(
                engine
                    .services
                    .vfs
                    .as_ref()
                    .ok_or(EngineError::MissingVfs)?,
                mission,
                owner,
            )?;
            &owned
        }
    };
    // shortcut: select the first eligible row until the laboratory exposes a technology list.
    let row = (0..research.row_count())
        .find(|&row| {
            research.flag_byte(row).unwrap_or_default() & (RESEARCH_AVAILABLE | RESEARCHED)
                == RESEARCH_AVAILABLE
                && !mission.free_research.learned.contains(&(owner, row))
        })
        .ok_or_else(|| fail("no available technology to research"))?;
    let component =
        String::from_utf8_lossy(research.row_name(row).map_err(research_error)?).into_owned();
    let duration_ms = (duration * 1000.0).ceil() as u32;
    mission.free_research.remaining[quota].1 -= 1;
    mission.free_research.pending = Some(Order {
        lab,
        owner,
        row,
        component: component.clone(),
        start_ms: now_ms,
        duration_ms,
    });
    Ok((component, duration_ms))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        capture_neutral_building, create, frame, load_gameplay_mission, starting_cockpit,
        EngineServices, MissionMode, MissionRequest, MissionStartupInput,
    };
    use fparkan_vfs::DirectoryVfs;
    use fparkan_world::{set_transform, OriginalObjectId};

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_teleport_lab_researches_the_available_turret() {
        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").unwrap();
        let vfs: Arc<dyn Vfs> = Arc::new(DirectoryVfs::new(std::path::PathBuf::from(root)));
        let mut engine = create(EngineServices::new(vfs.clone())).unwrap();
        load_gameplay_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/CAMPAIGN/CAMPAIGN.00/Mission.04/data.tma".into(),
                mode: MissionMode::SingleMission,
            },
            MissionStartupInput {
                queue_time_ms: 0,
                research_creation_gate: false,
            },
            None,
        )
        .unwrap();
        let hero = starting_cockpit(&engine).unwrap();
        let mut pose = transform_state(&engine.world, hero.handle).unwrap();
        pose.position = [950.7_f32, 869.4, 69.98].map(f32::to_bits);
        set_transform(&mut engine.world, hero.handle, pose).unwrap();
        assert!(order_lab_research(&mut engine, hero, 1_000).is_err());
        assert_eq!(
            capture_neutral_building(&mut engine, hero).unwrap(),
            Some(OriginalObjectId(5))
        );
        let (component, duration) = order_lab_research(&mut engine, hero, 1_000).unwrap();
        assert_eq!((component.as_str(), duration), ("e_tur_bt_01", 5_000));
        assert!(order_lab_research(&mut engine, hero, 1_001).is_err());
        assert!(frame(&mut engine, 5_999)
            .unwrap()
            .researched_component
            .is_none());
        assert_eq!(
            frame(&mut engine, 6_000).unwrap().researched_component,
            Some(component)
        );
        assert!(frame(&mut engine, 6_001)
            .unwrap()
            .researched_component
            .is_none());
        let mission = engine.loaded.as_ref().unwrap();
        let table = load_clan_table(&vfs, mission, OwnerId(0)).unwrap();
        let row = table.find_key(b"e_tur_bb_01").unwrap().unwrap();
        assert!(mission.free_research.learned.contains(&(OwnerId(0), row)));
        assert!(!mission.free_research.learned.contains(&(OwnerId(1), row)));
        assert!(order_lab_research(&mut engine, hero, 6_001).is_err());
    }
}

fn property(draft: &MissionObjectDraft, name: &[u8]) -> Option<u32> {
    draft
        .properties
        .iter()
        .find(|property| property.name_raw == name)
        .map(|property| property.raw_value[1])
}

pub(super) fn load_clan_table(
    vfs: &Arc<dyn Vfs>,
    mission: &LoadedMissionState,
    owner: OwnerId,
) -> Result<ResearchTrfDocument, EngineError> {
    let clan = mission
        .mission
        .clans
        .get(usize::from(owner.0))
        .ok_or_else(|| research_error("clan is unavailable"))?;
    let raw = match &clan.body {
        ClanBody::Standard {
            second_resource, ..
        }
        | ClanBody::Spatial {
            second_resource, ..
        } => &second_resource.path.raw,
    };
    let raw = raw.split(|&byte| byte == 0).next().unwrap_or_default();
    let path = std::str::from_utf8(raw).map_err(research_error)?;
    if path.is_empty() {
        return Err(research_error("clan has no technology table"));
    }
    let path = normalize_engine_path("clan technology table", path)?;
    ResearchTrfDocument::decode(read_vfs(vfs, &path)?).map_err(research_error)
}

fn research_error(error: impl std::fmt::Display) -> EngineError {
    EngineError::Production(error.to_string())
}
