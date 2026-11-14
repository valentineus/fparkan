use crate::unit_patrol::UnitPatrol;
use crate::{MissionDocument, MissionObjectDraft};
use fparkan_script::{Handler15Invocation, Handler15TargetPayload};
use fparkan_world::{
    handle_by_original_id, transform_state, ObjectHandle, OriginalObjectId, World,
};

pub(super) fn apply_attack_order(
    patrol: &mut UnitPatrol,
    mission: &MissionDocument,
    drafts: &[MissionObjectDraft],
    world: &World,
    id: u32,
    target_id: u32,
) -> Option<()> {
    let actor = warbot_by_logic_id(mission, drafts, world, id)?;
    let target = crate::mission_events::placed_logic_objects(mission, world, target_id).next()?;
    let index = fparkan_world::identity_metadata(world, target)
        .ok()?
        .original_id?
        .0 as usize;
    let [x, y, _] = transform_state(world, target)
        .ok()?
        .position
        .map(f32::from_bits);
    patrol.set_attack_target(world, actor, target, index, [x, y]);
    eprintln!(
        "sim_coverage script_attack_approx=1 unit={} id={id} target_id={target_id} target_unit={}",
        actor.slot, target.slot
    );
    Some(())
}

pub(super) fn warbot_by_logic_id(
    mission: &MissionDocument,
    drafts: &[MissionObjectDraft],
    world: &World,
    id: u32,
) -> Option<ObjectHandle> {
    unit_by_logic_id(mission, drafts, world, id, |class| {
        class == crate::boarding::WARBOT_CLASS
    })
}

pub(super) fn unit_by_logic_id(
    mission: &MissionDocument,
    drafts: &[MissionObjectDraft],
    world: &World,
    id: u32,
    matches_class: impl Fn(u32) -> bool,
) -> Option<ObjectHandle> {
    mission
        .objects
        .iter()
        .zip(drafts)
        .enumerate()
        .find_map(|(index, (object, draft))| {
            (id != u32::MAX
                && object.identity_or_clan_raw == id
                && draft.unit_dat_header_flags.is_some_and(&matches_class))
            .then(|| handle_by_original_id(world, OriginalObjectId(index as u32)))
            .flatten()
        })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ScriptPatrol {
    pub(super) actor_id: u32,
    target: Handler15TargetPayload,
    pub(super) radius: u32,
}

impl ScriptPatrol {
    pub(super) fn from_order(order: Handler15Invocation) -> Option<Self> {
        if !matches!(
            (order.word_1, order.word_2, order.target),
            (
                4,
                3,
                Handler15TargetPayload::ByPlace { .. } | Handler15TargetPayload::ByLogicId(_)
            )
        ) {
            return None;
        }
        Some(Self {
            actor_id: order.word_0,
            target: order.target,
            radius: order.word_3,
        })
    }

    pub(super) fn apply(
        self,
        patrol: &mut UnitPatrol,
        mission: &MissionDocument,
        drafts: &[MissionObjectDraft],
        world: &World,
    ) -> Option<(ObjectHandle, [u32; 2])> {
        let handle = warbot_by_logic_id(mission, drafts, world, self.actor_id)?;
        // shortcut: use the verified default radius until per-unit patrol profiles are wired.
        let radius = match self.radius {
            0 | u32::MAX => 60.0,
            radius => radius as f32,
        };
        match self.target {
            Handler15TargetPayload::ByPlace { first, second } => {
                let center = [first, second];
                patrol
                    .order(world, handle, center.map(|axis| axis as f32), radius)
                    .then_some((handle, center))
            }
            Handler15TargetPayload::ByLogicId(id) => {
                let target =
                    crate::mission_events::placed_logic_objects(mission, world, id).next()?;
                let [x, y, _] = transform_state(world, target)
                    .ok()?
                    .position
                    .map(f32::from_bits);
                patrol.order_around_object(world, handle, target, [x, y], radius).then(|| {
                    eprintln!("sim_coverage script_patrol_target_approx=1 unit={} target_id={id} target_unit={}", handle.slot, target.slot);
                    (handle, [x as u32, y as u32])
                })
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        create, load_gameplay_mission, EngineServices, MissionMode, MissionRequest,
        MissionStartupInput,
    };
    use fparkan_vfs::DirectoryVfs;
    use std::{path::PathBuf, sync::Arc};

    #[test]
    #[ignore = "requires licensed corpus"]
    fn stock_campaign_assigns_patrols_during_init() {
        let root =
            PathBuf::from(std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed root"));
        let mut engine = create(EngineServices::new(Arc::new(DirectoryVfs::new(&root)))).unwrap();
        load_gameplay_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/CAMPAIGN/CAMPAIGN.01/Mission.02/data.tma".into(),
                mode: MissionMode::SingleMission,
            },
            MissionStartupInput {
                queue_time_ms: 0,
                research_creation_gate: false,
            },
            None,
        )
        .unwrap();

        let mission = engine.loaded.as_ref().unwrap();
        let patrol = mission.unit_patrol.as_ref().unwrap();
        let live_patrols: Vec<_> = mission
            .mission
            .objects
            .iter()
            .enumerate()
            .filter_map(|(index, _)| {
                handle_by_original_id(&engine.world, OriginalObjectId(index as u32))
            })
            .filter(|&handle| patrol.is_patrolling(handle))
            .collect();
        assert!(!live_patrols.is_empty());
    }
}
