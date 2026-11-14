use super::{
    add_behavior_registered_object,
    autodemo::{spawn_prepared_unit, SpawnedUnit},
    behavior_ore_index_for_handle, BehaviorOreState, BehaviorRegisteredObject, Engine, EngineError,
    MissionControlDeviceTableState, MissionObjectDraft, StartingCockpit,
};
use fparkan_assets::ExtraRecord28;
use fparkan_world::{
    handle_by_original_id, identity_metadata, transform_state, ObjectHandle, OriginalObjectId,
    OwnerId, World,
};

pub(super) const MINE_CLASS: u32 = 0x8000_0004;
const BUILDER_CLASS: u32 = 0x0100_4000;
const BUILD_DISTANCE: f32 = 150.0;
const BUILD_ORE: f32 = 100.0;
const MINE_ORE_PER_SECOND: f32 = 50.0;
const MINE_COLLECTION_RADIUS: f32 = 250.0;

pub(super) struct MineSite {
    pub position: [f32; 3],
    pub remaining: f32,
    pub control: Option<MissionControlDeviceTableState>,
    placed_mine: Option<OriginalObjectId>,
}

impl MineSite {
    pub fn from_extra(record: &ExtraRecord28) -> Option<Self> {
        let remaining = f32::from_bits(record.raw_words[2]);
        // shortcut: prepare only the ordinary mineral-lode records with the
        // shipped default site flags, until the alternate ore records are mapped.
        (record.raw_words[1] == 0x1000_1000
            && record.position.iter().all(|value| value.is_finite())
            && remaining.is_finite()
            && remaining > 0.0)
            .then_some(Self {
                position: record.position,
                remaining,
                control: None,
                placed_mine: None,
            })
    }

    pub fn extract(remaining: &mut f32, ore: &mut BehaviorOreState, delta_seconds: f32) {
        let current = f32::from_bits(ore.current_ore_bits);
        let maximum = f32::from_bits(ore.maximum_ore_bits);
        let amount = (delta_seconds * MINE_ORE_PER_SECOND)
            .min((maximum - current).max(0.0))
            .min(*remaining);
        *remaining -= amount;
        ore.current_ore_bits = (current + amount).to_bits();
    }

    pub fn bind_placed_mines(sites: &mut [Self], drafts: &[MissionObjectDraft]) {
        for draft in drafts
            .iter()
            .filter(|draft| draft.unit_dat_header_flags == Some(MINE_CLASS))
        {
            let Some(id) = draft.original_id else {
                continue;
            };
            for site in sites.iter_mut() {
                // shortcut: assign each deposit once until competing mine tasks are modeled.
                if site.placed_mine.is_none()
                    && distance_xy(draft.position, site.position) < MINE_COLLECTION_RADIUS
                {
                    site.placed_mine = Some(id);
                }
            }
        }
    }

    fn live_mine(&self, world: &World) -> Option<ObjectHandle> {
        self.placed_mine
            .and_then(|id| handle_by_original_id(world, id))
            .or_else(|| handle_by_original_id(world, self.control.as_ref()?.original_id?))
    }
}

fn distance_xy(a: [f32; 3], b: [f32; 3]) -> f32 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

/// Deploys the smallest mine from BuildDat at a nearby mineral deposit
/// using a friendly builder and its onboard ore.
///
/// # Errors
///
/// Returns a diagnostic if the player, builder, or prepared site is unavailable
/// or the builder has insufficient ore.
pub fn deploy_mine(
    engine: &mut Engine,
    cockpit: StartingCockpit,
) -> Result<SpawnedUnit, EngineError> {
    let mission = engine
        .loaded
        .as_mut()
        .ok_or_else(|| reject("mission is unavailable"))?;
    let owner = identity_metadata(&engine.world, cockpit.handle)?
        .owner_id
        .ok_or_else(|| reject("player clan is unavailable"))?;
    if !mission
        .mission
        .clans
        .get(usize::from(owner.0))
        .is_some_and(|clan| clan.mode == 1)
    {
        return Err(reject("mine orders require a player clan"));
    }
    let player = transform_state(&engine.world, cockpit.handle)?
        .position
        .map(f32::from_bits);
    let start = mission.object_drafts.len() - mission.mine_sites.len();
    // shortcut: an order from the hero selects a nearby deposit and builder
    // until the strategic selection and builder movement UI are available.
    let (offset, site) = mission
        .mine_sites
        .iter()
        .enumerate()
        .filter(|(_, site)| {
            site.remaining > 0.0
                && site
                    .control
                    .as_ref()
                    .and_then(|table| table.original_id)
                    .is_some()
                && site.live_mine(&engine.world).is_none()
        })
        .map(|(offset, site)| (distance_xy(site.position, player), offset, site))
        .filter(|(distance, _, _)| *distance <= BUILD_DISTANCE)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, offset, site)| (offset, site))
        .ok_or_else(|| reject("stand within 150 units of a free ore deposit"))?;
    let builder = mission
        .behavior_ore_states
        .iter()
        .enumerate()
        .filter(|(_, ore)| ore.class_flags == BUILDER_CLASS)
        .filter_map(|(index, ore)| {
            let identity = identity_metadata(&engine.world, ore.handle).ok()?;
            if identity.owner_id != Some(owner) {
                return None;
            }
            let position = transform_state(&engine.world, ore.handle)
                .ok()?
                .position
                .map(f32::from_bits);
            let distance = distance_xy(position, site.position);
            let stock = f32::from_bits(ore.current_ore_bits);
            (distance <= BUILD_DISTANCE && stock.is_finite() && stock >= BUILD_ORE)
                .then_some((distance, index, stock))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .ok_or_else(|| reject("no friendly builder in range with 100 ore onboard"))?;
    let table = site
        .control
        .as_ref()
        .expect("selected site has controls")
        .clone();
    let id = table
        .original_id
        .ok_or_else(|| reject("mine slot has no identity"))?;
    let position = site.position;
    let distributor = mission
        .behavior_distributors
        .get(usize::from(owner.0))
        .filter(|entry| entry.super_ai_visible)
        .ok_or_else(|| reject("player clan has no resource distributor"))?;
    let clan = distributor.clan_index;
    let index = start + offset;
    let draft = &mission.object_drafts[index];
    if draft.unit_dat_header_flags != Some(MINE_CLASS)
        || !matches!(
            table.status,
            super::MissionControlDeviceTableStatus::Available(_)
        )
    {
        return Err(reject("mine scheme has no working mine control"));
    }
    let profile_index = mission
        .behavior_profiles
        .iter()
        .position(|entry| entry.original_id == id)
        .ok_or_else(|| reject("mine profile is unavailable"))?;
    let handle = spawn_prepared_unit(&mut engine.world, mission, index, owner.0, position, table)?;
    mission.behavior_ore_states[builder.1].current_ore_bits = (builder.2 - BUILD_ORE).to_bits();
    mission
        .behavior_ore_states
        .retain(|ore| ore.original_id != id);
    mission
        .behavior_ore_states
        .push(super::initialize_behavior_ore_state(
            id,
            handle,
            MINE_CLASS,
            &[],
        ));
    mission.behavior_profiles[profile_index].handle = handle;
    let distributor = &mut mission.behavior_distributors[clan];
    let count = distributor.registered_objects.len();
    distributor
        .registered_objects
        .retain(|entry| entry.original_id != id);
    if distributor.registered_objects.len() != count {
        super::normalize_behavior_distributor(distributor, None);
    }
    add_behavior_registered_object(
        distributor,
        BehaviorRegisteredObject {
            original_id: id,
            handle,
            selector: u32::from(owner.0),
            class_flags: MINE_CLASS,
            share: 0.0,
            native_tail: [0; 5],
        },
    );
    Ok(SpawnedUnit {
        object_index: index,
        handle,
        clan: owner.0,
    })
}

pub(super) fn extract_for_clan(
    sites: &[MineSite],
    remaining: &mut [f32],
    world: &World,
    clan_index: usize,
    ore_states: &mut [BehaviorOreState],
    delta_seconds: f32,
) -> Result<(), String> {
    for (site, remaining) in sites.iter().zip(remaining) {
        let Some(handle) = site.live_mine(world) else {
            continue;
        };
        if identity_metadata(world, handle)
            .ok()
            .and_then(|identity| identity.owner_id)
            .is_some_and(|owner| usize::from(owner.0) == clan_index)
        {
            let index = behavior_ore_index_for_handle(ore_states, handle)?;
            MineSite::extract(remaining, &mut ore_states[index], delta_seconds);
        }
    }
    Ok(())
}

pub(super) fn remaining_for_clan(sites: &[MineSite], world: &World, owner: OwnerId) -> f32 {
    sites
        .iter()
        .filter_map(|site| {
            let handle = site.live_mine(world)?;
            (identity_metadata(world, handle).ok()?.owner_id == Some(owner))
                .then_some(site.remaining)
        })
        .fold(0.0, |sum, amount| sum + amount)
}

pub(super) fn resume_mining(sites: &mut [MineSite], world: &World, mine: ObjectHandle) -> bool {
    let Some(id) = identity_metadata(world, mine)
        .ok()
        .and_then(|identity| identity.original_id)
    else {
        return false;
    };
    let Ok(transform) = transform_state(world, mine) else {
        return false;
    };
    let position = transform.position.map(f32::from_bits);
    let mut available = false;
    for site in sites.iter_mut().filter(|site| {
        site.remaining > 0.0 && distance_xy(site.position, position) < MINE_COLLECTION_RADIUS
    }) {
        match site.live_mine(world) {
            Some(handle) if handle == mine => available = true,
            None => {
                site.placed_mine = Some(id);
                available = true;
            }
            _ => {}
        }
    }
    available
}

fn reject(message: &str) -> EngineError {
    EngineError::Production(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_world::{
        construct_object, register_object, request_delete, set_owner, ObjectDraft, WorldConfig,
    };

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_two_fortresses_supplies_the_enemy_mine_from_its_deposit() {
        use super::super::{
            create, frame, load_gameplay_mission, EngineServices, MissionMode, MissionRequest,
            MissionStartupInput,
        };
        use fparkan_vfs::DirectoryVfs;
        use std::sync::Arc;

        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").unwrap();
        let mut engine = create(EngineServices::new(Arc::new(DirectoryVfs::new(root)))).unwrap();
        load_gameplay_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/Single.01/data.tma".into(),
                mode: MissionMode::SingleMission,
            },
            MissionStartupInput {
                queue_time_ms: 0,
                research_creation_gate: false,
            },
            None,
        )
        .unwrap();
        let reserve = |engine: &Engine| {
            remaining_for_clan(
                &engine.loaded.as_ref().unwrap().mine_sites,
                &engine.world,
                OwnerId(1),
            )
        };
        assert_eq!(reserve(&engine), 9_999_999.0);
        frame(&mut engine, 1000).unwrap();
        frame(&mut engine, 2000).unwrap();
        assert!(reserve(&engine) < 9_999_999.0);
    }

    #[test]
    fn reserve_sum_uses_only_live_mines_of_the_selected_clan() {
        let mut world = fparkan_world::new(WorldConfig);
        let mut create = |id, owner, remaining| {
            let id = OriginalObjectId(id);
            let handle = construct_object(
                &mut world,
                ObjectDraft {
                    original_id: Some(id),
                },
            )
            .unwrap();
            register_object(&mut world, handle).unwrap();
            set_owner(&mut world, handle, Some(OwnerId(owner))).unwrap();
            (
                handle,
                MineSite {
                    position: [0.0; 3],
                    remaining,
                    control: None,
                    placed_mine: Some(id),
                },
            )
        };
        let (a, first) = create(0, 1, 20.0);
        let (_, second) = create(1, 0, 30.0);
        let (_, third) = create(2, 1, 40.0);
        let mut sites = [first, second, third];
        assert_eq!(remaining_for_clan(&sites, &world, OwnerId(1)), 60.0);
        request_delete(&mut world, a).unwrap();
        fparkan_world::step(&mut world, &fparkan_world::InputSnapshot).unwrap();
        assert_eq!(remaining_for_clan(&sites, &world, OwnerId(1)), 40.0);
        let remaining_mine = sites[2].live_mine(&world).unwrap();
        assert!(resume_mining(&mut sites, &world, remaining_mine));
        assert_eq!(remaining_for_clan(&sites, &world, OwnerId(1)), 60.0);
        assert_eq!(remaining_for_clan(&sites, &world, OwnerId(0)), 30.0);
    }

    #[test]
    fn extraction_respects_both_the_buffer_and_deposit_limits() {
        let mut ore = super::super::initialize_behavior_ore_state(
            OriginalObjectId(0),
            ObjectHandle {
                slot: 0,
                generation: 0,
            },
            MINE_CLASS,
            &[],
        );
        ore.current_ore_bits = 480.0_f32.to_bits();
        let mut remaining = 30.0;
        MineSite::extract(&mut remaining, &mut ore, 1.0);
        assert_eq!(remaining, 10.0);
        assert_eq!(f32::from_bits(ore.current_ore_bits), 500.0);
        ore.current_ore_bits = 0;
        MineSite::extract(&mut remaining, &mut ore, 1.0);
        assert_eq!(remaining, 0.0);
        assert_eq!(f32::from_bits(ore.current_ore_bits), 10.0);
    }
}
