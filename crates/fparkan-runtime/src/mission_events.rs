use super::{
    add_behavior_registered_object, apply_recovered_script_host_callback,
    has_behavior_registration_key, normalize_behavior_distributor, BehaviorDistributorState,
    BehaviorRegisteredObject, Engine, EngineError, LoadedMissionState, MissionObjectDraft,
    MissionScriptHostEffect, StartingCockpit, LAST_SUPER_AI_TABLE_INDEX,
};
use fparkan_msh::{draw_batches, node38_sampled_hierarchy, selected_slot, Group, Lod, NodeId};
use fparkan_render::LegacyIron3dEulerTransform;
use fparkan_script::{
    ScriptDispatchSelector, ScriptInstruction, VarSetDefault, VmHostCallbackCommand,
};
use fparkan_world::{
    handle_by_original_id, identity_metadata, registration_sequence, set_owner, transform_state,
    ObjectHandle, OriginalObjectId, OwnerId, World,
};
use std::collections::{HashMap, HashSet};

const MOBILE_HQ_CLASS: u32 = 0x0101_0000;
const MAIN_TELEPORT_CLASS: u32 = 0x8000_0200;

/// First encounter with an uncovered periodic script instruction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MissionEventStop {
    /// Clan owning the event's varset.
    pub clan_index: usize,
    /// Compiled script path.
    pub script_path: String,
    /// Index in the package's event table.
    pub event_index: usize,
    /// Index of the instruction in that event.
    pub instruction_index: usize,
    /// Unsupported opcode, operand, or expression.
    pub reason: String,
}

/// One invocation of the active clan's periodic `Mission` event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MissionEventTick {
    /// An uncovered instruction encountered for the first time, if any.
    pub stop: Option<MissionEventStop>,
    /// Notifications produced by this invocation, in script order.
    pub notices: Vec<MissionNotice>,
    /// Newly assigned or changed place patrols in script order.
    pub patrol_orders: Vec<(ObjectHandle, [u32; 2])>,
}

/// A notification emitted by the scenario host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MissionNotice {
    /// First presentation of an information message by index.
    Information(u32),
    /// Objective changed from pending to complete.
    ObjectiveComplete(u32),
    /// The player's capture took a building from a neutral or enemy clan.
    BuildingCaptured { neutral: bool },
    /// Mission completed.
    Completed,
    /// Mission failed.
    Failed,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ObjectiveState {
    #[default]
    Pending,
    Complete,
    Failed,
}

#[derive(Default)]
pub(super) struct MissionObjectives {
    primary_count: usize,
    states: Vec<ObjectiveState>,
}

impl MissionObjectives {
    fn from_config(bytes: &[u8]) -> Self {
        let mut primary = 0;
        let mut bonus = 0;
        let mut group = None;
        for line in bytes.split(|&byte| byte == b'\n') {
            let mut words = line
                .split(|byte| byte.is_ascii_whitespace())
                .filter(|word| !word.is_empty());
            match words.next() {
                Some(word) if word.eq_ignore_ascii_case(b"object") => {
                    group = match words.next() {
                        Some(name) if name.eq_ignore_ascii_case(b"primary_objectives") => {
                            Some(&mut primary)
                        }
                        Some(name) if name.eq_ignore_ascii_case(b"bonus_objectives") => {
                            Some(&mut bonus)
                        }
                        _ => None,
                    };
                }
                Some(word) if word.eq_ignore_ascii_case(b"end") => group = None,
                Some(word)
                    if !word.starts_with(b"#")
                        && !word.starts_with(b"//")
                        && line.contains(&b'=') =>
                {
                    if let Some(count) = group.as_deref_mut() {
                        *count += 1;
                    }
                }
                _ => {}
            }
        }
        Self {
            primary_count: primary,
            states: vec![ObjectiveState::Pending; primary + bonus],
        }
    }

    pub(super) fn apply(
        &mut self,
        command: u32,
        index: u32,
        notices: &mut Vec<MissionNotice>,
    ) -> Result<bool, String> {
        let state = self
            .states
            .get_mut(index as usize)
            .ok_or_else(|| format!("Handler(30) objective {index} is unavailable"))?;
        match command {
            3 => {
                if *state == ObjectiveState::Pending {
                    *state = ObjectiveState::Complete;
                    notices.push(MissionNotice::ObjectiveComplete(index));
                }
                return Ok(self.states[..self.primary_count]
                    .iter()
                    .all(|state| *state == ObjectiveState::Complete));
            }
            4 => *state = ObjectiveState::Failed,
            5 => *state = ObjectiveState::Pending,
            _ => unreachable!(),
        }
        Ok(false)
    }
}

#[derive(Default)]
pub(super) struct MissionEvents {
    pub(super) start_ms: u32,
    next_ms: u32,
    reported_stops: HashSet<(usize, usize)>,
    inside_hero_exit: bool,
    pub(super) voiced_messages: HashSet<u32>,
    reserved_units: HashSet<(usize, ObjectHandle)>,
    placed_clan_objects: Vec<(ObjectHandle, u32)>,
    issued_logic_ids: HashMap<ObjectHandle, u32>,
    next_logic_id: u32,
    pub(super) objectives: MissionObjectives,
}

impl MissionEvents {
    pub(super) fn is_reserved(&self, owner: OwnerId, unit: ObjectHandle) -> bool {
        self.reserved_units.contains(&(usize::from(owner.0), unit))
    }

    pub(super) fn clan_units<'a>(
        &'a self,
        world: &'a World,
        owner: OwnerId,
        free_only: bool,
    ) -> impl Iterator<Item = ObjectHandle> + Clone + 'a {
        self.placed_clan_objects
            .iter()
            .filter_map(move |&(handle, class)| {
                if free_only
                    && (class != super::boarding::WARBOT_CLASS
                        || self
                            .reserved_units
                            .contains(&(usize::from(owner.0), handle)))
                {
                    return None;
                }
                registration_sequence(world, handle).ok()??;
                (identity_metadata(world, handle).ok()?.owner_id == Some(owner)).then_some(handle)
            })
    }

    pub(super) fn count_clan_class(&self, world: &World, owner: OwnerId, class: u32) -> u32 {
        self.clan_classes(world, owner)
            .filter(|&candidate| candidate == class)
            .count() as u32
    }

    pub(super) fn clan_classes<'a>(
        &'a self,
        world: &'a World,
        owner: OwnerId,
    ) -> impl Iterator<Item = u32> + 'a {
        clan_object_classes(&self.placed_clan_objects, world, owner)
    }

    pub(super) fn first_available_placed_unit(
        &self,
        mission: &super::MissionDocument,
        world: &World,
        clan: usize,
        class: u32,
        available: impl Fn(ObjectHandle) -> bool,
    ) -> Option<u32> {
        self.placed_clan_objects
            .iter()
            .find_map(|&(handle, candidate)| {
                if candidate != class
                    || self.reserved_units.contains(&(clan, handle))
                    || !available(handle)
                {
                    return None;
                }
                registration_sequence(world, handle).ok()??;
                let identity = identity_metadata(world, handle).ok()?;
                if identity.owner_id != Some(OwnerId(clan as u16)) {
                    return None;
                }
                self.logic_id(mission, world, handle)
            })
    }

    pub(super) fn logic_id(
        &self,
        mission: &super::MissionDocument,
        world: &World,
        handle: ObjectHandle,
    ) -> Option<u32> {
        self.issued_logic_ids.get(&handle).copied().or_else(|| {
            let identity = identity_metadata(world, handle).ok()?;
            mission
                .objects
                .get(identity.original_id?.0 as usize)
                .map(|object| object.identity_or_clan_raw)
        })
    }

    pub(super) fn logic_objects<'a>(
        &'a self,
        mission: &'a super::MissionDocument,
        world: &'a World,
        id: u32,
    ) -> impl Iterator<Item = ObjectHandle> + 'a {
        placed_logic_objects(mission, world, id).chain(self.issued_logic_ids.iter().filter_map(
            move |(&handle, &issued)| {
                (issued == id
                    && registration_sequence(world, handle)
                        .ok()
                        .flatten()
                        .is_some())
                .then_some(handle)
            },
        ))
    }

    pub(super) fn has_live_logic_object(
        &self,
        mission: &super::MissionDocument,
        world: &World,
        id: u32,
    ) -> bool {
        self.logic_objects(mission, world, id).any(|handle| {
            registration_sequence(world, handle)
                .ok()
                .flatten()
                .is_some()
        })
    }

    pub(super) fn issue_logic_id(
        &mut self,
        mission: &super::MissionDocument,
        world: &World,
        handle: ObjectHandle,
    ) -> u32 {
        self.issued_logic_ids
            .retain(|&unit, _| registration_sequence(world, unit).ok().flatten().is_some());
        if let Some(&id) = self.issued_logic_ids.get(&handle) {
            return id;
        }
        // shortcut: allocate an internal script ID until native registration numbers are recovered.
        loop {
            self.next_logic_id = self.next_logic_id.wrapping_add(1) & 0x7fff_ffff;
            if self.next_logic_id != 0
                && !mission
                    .objects
                    .iter()
                    .any(|object| object.identity_or_clan_raw == self.next_logic_id)
                && !self
                    .issued_logic_ids
                    .values()
                    .any(|&id| id == self.next_logic_id)
            {
                self.issued_logic_ids.insert(handle, self.next_logic_id);
                return self.next_logic_id;
            }
        }
    }

    pub(super) fn set_reservation(
        &mut self,
        mission: &super::MissionDocument,
        world: &World,
        clan: usize,
        id: u32,
        reserved: bool,
    ) {
        let Some(handle) = placed_logic_objects(mission, world, id).find(|&handle| {
            identity_metadata(world, handle)
                .is_ok_and(|identity| identity.owner_id == Some(OwnerId(clan as u16)))
        }) else {
            return;
        };
        let key = (clan, handle);
        if if reserved {
            self.reserved_units.insert(key)
        } else {
            self.reserved_units.remove(&key)
        } {
            eprintln!(
                "script_unit_reservation clan={clan} unit={} id={id} reserved={reserved}",
                handle.slot
            );
        }
    }

    pub(super) fn count_clan_mask(
        &self,
        world: &World,
        distributors: &[BehaviorDistributorState],
        clan: u32,
        mask: u32,
    ) -> u32 {
        if !distributors
            .get(clan as usize)
            .is_some_and(|entry| entry.super_ai_visible)
        {
            return 0;
        }
        clan_object_classes(&self.placed_clan_objects, world, OwnerId(clan as u16))
            .filter(|class| class & mask != 0)
            .count() as u32
    }

    pub(super) fn register_unit(&mut self, world: &World, handle: ObjectHandle, class: u32) {
        self.placed_clan_objects
            .retain(|(handle, _)| registration_sequence(world, *handle).is_ok());
        self.placed_clan_objects.push((handle, class));
        self.reserved_units.retain(|(_, handle)| {
            registration_sequence(world, *handle)
                .ok()
                .flatten()
                .is_some()
        });
    }

    pub(super) fn with_placed_objects(
        world: &mut World,
        drafts: &[MissionObjectDraft],
        handles: &[ObjectHandle],
        clan_count: usize,
        mission_config: &[u8],
    ) -> Result<Self, EngineError> {
        let mut roster = Vec::new();
        for (draft, &handle) in drafts.iter().zip(handles) {
            if let Some(class) = draft.unit_dat_header_flags {
                let selector = draft.raw_after_resource;
                if selector <= LAST_SUPER_AI_TABLE_INDEX && (selector as usize) < clan_count {
                    set_owner(world, handle, Some(OwnerId(selector as u16)))?;
                    roster.push((handle, class));
                }
            }
        }
        Ok(Self {
            placed_clan_objects: roster,
            objectives: MissionObjectives::from_config(mission_config),
            ..Self::default()
        })
    }

    fn due(&mut self, now_ms: u32) -> bool {
        if now_ms < self.next_ms {
            return false;
        }
        self.next_ms = now_ms.wrapping_add(2_000);
        true
    }
}

/// Transfers the nearest available neutral warbot or mobile HQ to the player's clan.
///
/// The provisional interaction uses a 12-unit radius around the player's body.
/// Returns the original object ID of a successful transfer.
///
/// # Errors
///
/// Returns an error if the player's world handle or the ownership update fails.
pub fn capture_neutral_warbot(
    engine: &mut Engine,
    cockpit: StartingCockpit,
) -> Result<Option<OriginalObjectId>, EngineError> {
    let Some(mission) = &engine.loaded else {
        return Ok(None);
    };
    let Some(player_owner) = identity_metadata(&engine.world, cockpit.handle)?.owner_id else {
        return Ok(None);
    };
    if !mission
        .mission
        .clans
        .get(usize::from(player_owner.0))
        .is_some_and(|clan| clan.mode == 1)
    {
        return Ok(None);
    }
    let position = transform_state(&engine.world, cockpit.handle)?
        .position
        .map(f32::from_bits);
    let candidate = mission
        .mission_events
        .placed_clan_objects
        .iter()
        .filter_map(|&(handle, class)| {
            if !matches!(class, super::boarding::WARBOT_CLASS | MOBILE_HQ_CLASS) {
                return None;
            }
            registration_sequence(&engine.world, handle).ok()??;
            let identity = identity_metadata(&engine.world, handle).ok()?;
            let clan = mission
                .mission
                .clans
                .get(usize::from(identity.owner_id?.0))?;
            if clan.mode != 3 {
                return None;
            }
            let target = transform_state(&engine.world, handle)
                .ok()?
                .position
                .map(f32::from_bits);
            let distance_squared: f32 = position
                .iter()
                .zip(target)
                .map(|(p, q)| (p - q).powi(2))
                .sum();
            // shortcut: radius only, until native aim and range checks are measured.
            (distance_squared <= 12.0_f32.powi(2)).then_some((
                distance_squared,
                handle,
                identity.original_id?,
            ))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0));
    if let Some((_, handle, original_id)) = candidate {
        set_owner(&mut engine.world, handle, Some(player_owner))?;
        engine
            .loaded
            .as_mut()
            .expect("loaded mission")
            .note_clan_registration(player_owner, handle);
        return Ok(Some(original_id));
    }
    Ok(None)
}

/// Transfers the neutral building whose communicator the player is standing on.
///
/// # Errors
///
/// Returns an error if the player's world handle or the ownership update fails.
pub fn capture_neutral_building(
    engine: &mut Engine,
    cockpit: StartingCockpit,
) -> Result<Option<OriginalObjectId>, EngineError> {
    capture_building_at_communicator(engine, cockpit, false)
        .map(|capture| capture.map(|(id, _)| id))
}

/// Transfers a neutral or hostile building at the player's communicator.
///
/// # Errors
/// Returns an error if the player's world handle or the ownership update fails.
pub fn capture_building(
    engine: &mut Engine,
    cockpit: StartingCockpit,
) -> Result<Option<OriginalObjectId>, EngineError> {
    capture_building_with_notice(engine, cockpit).map(|capture| capture.map(|(id, _)| id))
}

/// Transfers a building and reports which capture announcement to play.
///
/// # Errors
///
/// Returns an error if the player's world handle or the ownership update fails.
pub fn capture_building_with_notice(
    engine: &mut Engine,
    cockpit: StartingCockpit,
) -> Result<Option<(OriginalObjectId, MissionNotice)>, EngineError> {
    capture_building_at_communicator(engine, cockpit, true)
}

fn capture_building_at_communicator(
    engine: &mut Engine,
    cockpit: StartingCockpit,
    allow_hostile: bool,
) -> Result<Option<(OriginalObjectId, MissionNotice)>, EngineError> {
    let Some(mission) = &mut engine.loaded else {
        return Ok(None);
    };
    let Some(player_owner) = identity_metadata(&engine.world, cockpit.handle)?.owner_id else {
        return Ok(None);
    };
    let Some(player_clan) = mission
        .mission
        .clans
        .get(usize::from(player_owner.0))
        .filter(|clan| clan.mode == 1)
    else {
        return Ok(None);
    };
    let position = transform_state(&engine.world, cockpit.handle)?
        .position
        .map(f32::from_bits);
    let candidate = mission
        .mission_events
        .placed_clan_objects
        .iter()
        .filter_map(|&(handle, class)| {
            registration_sequence(&engine.world, handle).ok()??;
            let identity = identity_metadata(&engine.world, handle).ok()?;
            let owner = identity.owner_id?;
            let clan = mission.mission.clans.get(usize::from(owner.0))?;
            // shortcut: use TMA diplomacy and the same communicator as a neutral transfer.
            let hostile = allow_hostile
                && owner != player_owner
                && player_clan.relations.iter().any(|relation| {
                    relation.relation_value == 0 && relation.other_clan_name.raw == clan.name.raw
                });
            let neutral = clan.mode == 3;
            (class & 0x8000_0000 != 0 && (neutral || hostile)).then_some((
                handle,
                identity.original_id?,
                owner,
                class,
                neutral,
            ))
        })
        .find(|&(handle, id, _, class, _)| {
            transform_state(&engine.world, handle).is_ok_and(|pose| {
                on_communicator(
                    &mission.mission_assets,
                    id.0 as usize,
                    class,
                    pose,
                    position,
                )
            })
        });
    if let Some((handle, id, former_owner, class, neutral)) = candidate {
        mission.transfer_building(
            &mut engine.world,
            handle,
            id,
            former_owner,
            player_owner,
            class,
        )?;
        mission.capture_barrier = crate::capture_barrier::CaptureBarrier::start(
            &mission.mission_assets,
            handle,
            id.0 as usize,
            cockpit.handle,
        );
        Ok(Some((id, MissionNotice::BuildingCaptured { neutral })))
    } else {
        Ok(None)
    }
}

impl LoadedMissionState {
    pub(super) fn transfer_building(
        &mut self,
        world: &mut World,
        handle: ObjectHandle,
        id: OriginalObjectId,
        former_owner: OwnerId,
        new_owner: OwnerId,
        class: u32,
    ) -> Result<(), EngineError> {
        set_owner(world, handle, Some(new_owner))?;
        if former_owner != new_owner {
            self.note_clan_registration(new_owner, handle);
        }
        if has_behavior_registration_key(class) {
            let distributors = &mut self.behavior_distributors;
            if let Some(previous) = distributors.get_mut(usize::from(former_owner.0)) {
                let count = previous.registered_objects.len();
                previous
                    .registered_objects
                    .retain(|object| object.handle != handle);
                if previous.registered_objects.len() != count {
                    normalize_behavior_distributor(previous, None);
                }
            }
            if let Some(next) = distributors
                .get_mut(usize::from(new_owner.0))
                .filter(|entry| entry.super_ai_visible)
            {
                add_behavior_registered_object(
                    next,
                    BehaviorRegisteredObject {
                        original_id: id,
                        handle,
                        selector: u32::from(new_owner.0),
                        class_flags: class,
                        share: 0.0,
                        native_tail: [0; 5],
                    },
                );
            }
        }
        Ok(())
    }
}

pub(super) fn on_communicator(
    assets: &super::MissionAssets,
    object_index: usize,
    class: u32,
    pose: super::TransformState,
    [x, y, z]: [f32; 3],
) -> bool {
    let platform_height = communicator_triangles(assets, object_index, class, pose)
        .into_iter()
        .flatten()
        .filter(|triangle| in_tactical_polygon(triangle, x, y))
        .map(|[a, b, c]| a[2].max(b[2]).max(c[2]))
        .max_by(f32::total_cmp);
    platform_height.is_some_and(|height| (z - height).abs() <= 2.0)
}

pub(super) fn communicator_triangles(
    assets: &super::MissionAssets,
    object_index: usize,
    class: u32,
    pose: super::TransformState,
) -> Option<Vec<[[f32; 3]; 3]>> {
    let is_generator = class == 0x8000_0002;
    let material = if is_generator {
        &b"B_MTP_07G"[..]
    } else {
        &b"B_COMP_4G"[..]
    };
    let visual = assets
        .visual_for_object(object_index)
        .and_then(|id| assets.visual_by_id(id))?;
    if visual.source_type != Some(u32::from_le_bytes(*b"FORT")) {
        return None;
    }
    let wear = assets
        .wears
        .iter()
        .find(|wear| Some(wear.id) == visual.wear_id)?;
    let model = visual
        .model_id
        .and_then(|id| assets.model_by_id(id))
        .map(|model| &model.validated)?;
    let hierarchy = node38_sampled_hierarchy(model, 0)?;
    let transform = LegacyIron3dEulerTransform {
        translation: pose.position.map(f32::from_bits),
        orientation_radians: pose.orientation.map(f32::from_bits),
    };
    let scale = pose.scale.map(f32::from_bits);
    // shortcut: use the green platform mesh until the native volume is known.
    Some(
        hierarchy
            .poses
            .iter()
            .enumerate()
            .filter_map(|(index, pose)| {
                let slot = selected_slot(model, NodeId(index as u32), Lod(0), Group(0))?;
                Some((*pose, draw_batches(model, slot).ok()?))
            })
            .flat_map(|(pose, batches)| batches.iter().map(move |batch| (pose, batch)))
            .filter(|(_, batch)| {
                wear.table
                    .entries
                    .get(usize::from(batch.material_index))
                    .is_some_and(|entry| entry.material.0.eq_ignore_ascii_case(material))
            })
            .flat_map(|(pose, batch)| {
                let first = batch.index_start as usize;
                let end = first + usize::from(batch.index_count);
                model.indices[first..end]
                    .chunks_exact(3)
                    .filter_map(move |indices| {
                        let [a, b, c] = [indices[0], indices[1], indices[2]].map(|index| {
                            let local =
                                model.positions[batch.base_vertex as usize + usize::from(index)];
                            transform.try_transform_scaled_point(pose.transform_point(local), scale)
                        });
                        let (a, b, c) = (a?, b?, c?);
                        // The floor and ceiling of the communicator share the same material.
                        let normal_z =
                            (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
                        (normal_z > 0.0).then_some([a, b, c])
                    })
            })
            .collect(),
    )
}

impl LoadedMissionState {
    fn hero_in_teleport_exit(&self, world: &World) -> bool {
        if self.boarding.is_some() || self.script_host_mode != super::MissionMode::SingleMission {
            return false;
        }
        let hero = self.object_drafts.iter().find(|draft| {
            draft.unit_dat_header_flags == Some(super::HERO_UNIT_CLASS)
                && draft.raw_after_resource == 0
        });
        let Some(hero_position) = hero
            .and_then(|draft| handle_by_original_id(world, draft.original_id?))
            .and_then(|hero| transform_state(world, hero).ok())
            .map(|pose| pose.position.map(f32::from_bits))
        else {
            return false;
        };
        self.object_drafts.iter().enumerate().any(|(index, draft)| {
            if draft.unit_dat_header_flags != Some(MAIN_TELEPORT_CLASS) {
                return false;
            }
            let hit = || {
                let handle = handle_by_original_id(world, draft.original_id?)?;
                let pose = transform_state(world, handle).ok()?;
                let visual = self
                    .mission_assets
                    .visual_by_id(self.mission_assets.visual_for_object(index)?)?;
                let model = &self.mission_assets.model_by_id(visual.model_id?)?.validated;
                if model.hero_exit_points.is_empty() {
                    return Some(false);
                }
                let hierarchy = node38_sampled_hierarchy(model, 0)?;
                let transform = LegacyIron3dEulerTransform {
                    translation: pose.position.map(f32::from_bits),
                    orientation_radians: pose.orientation.map(f32::from_bits),
                };
                Some(model.hero_exit_points.iter().any(|&(node, local)| {
                    let Some(center) = hierarchy.poses.get(node.0 as usize).and_then(|node_pose| {
                        transform.try_transform_scaled_point(
                            node_pose.transform_point(local),
                            pose.scale.map(f32::from_bits),
                        )
                    }) else {
                        return false;
                    };
                    // shortcut: static node poses and the ordinary Res17 volume until animated triggers are wired.
                    inside_exit_volume(center, hero_position)
                }))
            };
            hit() == Some(true)
        })
    }

    pub(super) fn tick_mission_event(
        &mut self,
        world: &World,
        now_ms: u32,
    ) -> Option<MissionEventTick> {
        // shortcut: use the local clan (0 in the observed standalone modes),
        // until runtime has the original active-clan selector.
        if self
            .local_hero
            .is_some_and(|hero| transform_state(world, hero).is_err())
        {
            if self.script_outcome != super::MissionScriptOutcome::Active {
                return None;
            }
            let effect = apply_recovered_script_host_callback(
                self.script_host_mode,
                &mut self.script_host_status,
                &mut self.script_outcome,
                VmHostCallbackCommand {
                    mode: 0,
                    first: 0,
                    second: 0,
                },
            );
            return (effect == MissionScriptHostEffect::MissionFailed).then(|| MissionEventTick {
                stop: None,
                notices: vec![MissionNotice::Failed],
                patrol_orders: Vec::new(),
            });
        }
        let bundle = self
            .script_bundles
            .iter()
            .find(|bundle| bundle.clan_index == 0 && bundle.behavior_warm_start.is_some())?;
        let mut events = bundle.package.events.iter().enumerate();
        let exit_event = events.clone().find(|(_, event)| {
            event.name_raw == b"Hero_Teleported\0" && !event.instructions.is_empty()
        });
        let inside_exit = exit_event.is_some() && self.hero_in_teleport_exit(world);
        let entered_exit = inside_exit && !self.mission_events.inside_hero_exit;
        self.mission_events.inside_hero_exit = inside_exit;
        let (event_index, event) =
            if entered_exit && self.script_outcome == super::MissionScriptOutcome::Active {
                exit_event?
            } else if self
                .mission_events
                .due(self.clan_ai_events.wall_time_ms(now_ms))
            {
                events.find(|(_, event)| {
                    event.name_raw.split(|byte| *byte == 0).next() == Some(b"Mission")
                })?
            } else {
                return None;
            };
        let declarations = &self.script_varset.as_ref()?.declarations;
        let state = self
            .script_varset_states
            .iter_mut()
            .find(|state| state.clan_index == bundle.clan_index)?;
        let mission_events = &mut self.mission_events;
        let clock_seconds = self.clan_ai_events.clock_seconds(bundle.clan_index);
        let mut notices = Vec::new();
        let mut patrol_orders = Vec::new();
        let execution = declarations.execute_mission_prefix(
            event,
            bundle.formula_table.as_ref(),
            &mut state.values,
            |instruction, values| match instruction.dispatch_selector() {
                // The host is already bound to this clan's AI context.
                ScriptDispatchSelector::Handler(0) => Ok(Some(1)),
                ScriptDispatchSelector::Handler(9)
                    if instruction.references.is_empty()
                        && instruction.header_words[1] == u32::MAX =>
                {
                    Ok(None)
                }
                ScriptDispatchSelector::Handler(13) => {
                    if single_dword(instruction, values) != Some(1) {
                        return Err("Handler(13) only supports UNIT_FREE_CAPTURER".into());
                    }
                    let owner = OwnerId(bundle.clan_index as u16);
                    for &(handle, class) in &mission_events.placed_clan_objects {
                        if class != super::boarding::WARBOT_CLASS
                            || mission_events
                                .reserved_units
                                .contains(&(bundle.clan_index, handle))
                            || registration_sequence(world, handle)
                                .ok()
                                .flatten()
                                .is_none()
                            || self.unit_patrol.as_ref().is_some_and(|patrol| {
                                patrol.is_patrolling(handle) || patrol.is_shut_down(handle)
                            })
                            || self
                                .boarding
                                .is_some_and(|boarding| boarding.cockpit.handle == handle)
                        {
                            continue;
                        }
                        let Ok(identity) = identity_metadata(world, handle) else {
                            continue;
                        };
                        let Some(id) = identity
                            .original_id
                            .filter(|_| identity.owner_id == Some(owner))
                        else {
                            continue;
                        };
                        let index = id.0 as usize;
                        let Some(chassis) = self
                            .object_drafts
                            .get(index)
                            .and_then(|draft| draft.unit_components.first())
                        else {
                            continue;
                        };
                        if !capturer_chassis(&chassis.resource_raw) {
                            continue;
                        }
                        // shortcut: native logical IDs for newly produced robots are still open.
                        let object = self
                            .mission
                            .objects
                            .get(index)
                            .ok_or("Handler(13) dynamic capturer logical ID is unavailable")?;
                        return Ok(Some(object.identity_or_clan_raw));
                    }
                    Ok(Some(u32::MAX))
                }
                ScriptDispatchSelector::Handler(31) => {
                    let (clan, mask) = instruction
                        .dword_pair(values)
                        .ok_or("Handler(31) requires two DWORD operands")?;
                    let count = mission_events.count_clan_mask(
                        world,
                        &self.behavior_distributors,
                        clan,
                        mask,
                    );
                    Ok(Some(count))
                }
                ScriptDispatchSelector::Handler(32) => {
                    let (area, id) = instruction
                        .dword_pair(values)
                        .ok_or("Handler(32) requires two DWORD operands")?;
                    let inside = tactical_area_contains(&self.mission, world, area, id);
                    Ok(Some(u32::from(inside)))
                }
                ScriptDispatchSelector::Handler(34) => {
                    let class = single_dword(instruction, values)
                        .ok_or("Handler(34) requires one DWORD operand")?;
                    let count = mission_events.count_clan_class(
                        world,
                        OwnerId(bundle.clan_index as u16),
                        class,
                    );
                    Ok(Some(count))
                }
                ScriptDispatchSelector::Handler(39) => {
                    let id = single_dword(instruction, values)
                        .ok_or("Handler(39) requires one DWORD logical ID")?;
                    Ok(Some(u32::from(mission_events.has_live_logic_object(
                        &self.mission,
                        world,
                        id,
                    ))))
                }
                ScriptDispatchSelector::Handler(52) => {
                    let id = single_dword(instruction, values)
                        .ok_or("Handler(52) requires one DWORD operand")?;
                    let clan = placed_logic_objects(&self.mission, world, id)
                        .find_map(|handle| identity_metadata(world, handle).ok()?.owner_id);
                    Ok(Some(clan.map_or(u32::MAX, |owner| u32::from(owner.0))))
                }
                ScriptDispatchSelector::Handler(51) => {
                    let (id, reserved) = crate::clan_script_init::reservation(instruction, values)
                        .ok_or("Handler(51) requires a logical ID and a DWORD flag")?;
                    mission_events.set_reservation(
                        &self.mission,
                        world,
                        bundle.clan_index,
                        id,
                        reserved,
                    );
                    Ok(None)
                }
                ScriptDispatchSelector::Handler(59 | 60) => {
                    crate::script_time::result(instruction, values, clock_seconds)
                        .map(Some)
                        .ok_or_else(|| {
                            format!(
                                "{:?} requires one DWORD operand",
                                instruction.dispatch_selector()
                            )
                        })
                }
                ScriptDispatchSelector::Handler(15) => {
                    let order = declarations
                        .resolve_handler15_with_values(instruction, values)
                        .map_err(|error| error.to_string())?;
                    if crate::unit_patrol::supports_order(&order) {
                        let patrol = self
                            .unit_patrol
                            .get_or_insert_with(crate::unit_patrol::UnitPatrol::scripted);
                        return Ok(patrol.script_order(
                            world,
                            &self.mission,
                            &self.object_drafts,
                            order,
                        ));
                    }
                    let Some(order) = crate::script_patrol::ScriptPatrol::from_order(order) else {
                        return Err(format!(
                            "Handler(15) order={} insertion={} target={:?}",
                            order.word_1, order.word_2, order.target
                        ));
                    };
                    let Some(patrol) = &mut self.unit_patrol else {
                        return Err("Handler(15) patrol runtime is unavailable".into());
                    };
                    if let Some(order) =
                        order.apply(patrol, &self.mission, &self.object_drafts, world)
                    {
                        patrol_orders.push(order);
                    }
                    Ok(None)
                }
                ScriptDispatchSelector::Handler(30) => {
                    let command = declarations
                        .resolve_handler30_with_values(instruction, values)
                        .map_err(|error| error.to_string())?;
                    crate::script_host::apply(
                        self.script_host_mode,
                        &mut self.script_host_status,
                        &mut self.script_outcome,
                        mission_events,
                        command,
                        &mut notices,
                    )?;
                    Ok(None)
                }
                selector => Err(format!("{selector:?}")),
            },
        );
        let stop = execution
            .err()
            .filter(|(index, _)| mission_events.reported_stops.insert((event_index, *index)))
            .map(|(instruction_index, reason)| MissionEventStop {
                clan_index: bundle.clan_index,
                script_path: bundle.script_path.clone(),
                event_index,
                instruction_index,
                reason,
            });
        Some(MissionEventTick {
            stop,
            notices,
            patrol_orders,
        })
    }
}

fn inside_exit_volume([x, y, z]: [f32; 3], [px, py, pz]: [f32; 3]) -> bool {
    (pz - z).abs() <= 3.0 && (px - x).powi(2) + (py - y).powi(2) <= 25.0
}

pub(super) fn capturer_chassis(chassis: &[u8; 32]) -> bool {
    // shortcut: stock robot keys map to native size classes 1 and 2;
    // query the effective Behavior controller when custom types are supported.
    matches!(
        &chassis[..3],
        [b'R' | b'r', b'_', b'T' | b't' | b'L' | b'l' | b'H' | b'h']
    )
}

fn single_dword(instruction: &ScriptInstruction, values: &[VarSetDefault]) -> Option<u32> {
    let [index] = *instruction.references.as_slice() else {
        return None;
    };
    match *values.get(index as usize)? {
        VarSetDefault::Dword(value) => Some(value),
        _ => None,
    }
}

fn clan_object_classes<'a>(
    objects: &'a [(ObjectHandle, u32)],
    world: &'a World,
    owner: OwnerId,
) -> impl Iterator<Item = u32> + 'a {
    objects.iter().filter_map(move |&(handle, class)| {
        registration_sequence(world, handle).ok()??;
        (identity_metadata(world, handle).ok()?.owner_id == Some(owner)).then_some(class)
    })
}

fn tactical_area_contains(
    mission: &super::MissionDocument,
    world: &World,
    area: u32,
    id: u32,
) -> bool {
    // MisLoad allocates a slot per path and selects the first record matching
    // the slot ID. Out-of-range IDs and unfilled slots yield an empty list.
    if area as usize >= mission.paths.len() {
        return false;
    }
    let Some(path) = mission.paths.iter().find(|path| path.id as u32 == area) else {
        return false;
    };
    placed_logic_objects(mission, world, id).any(|handle| {
        transform_state(world, handle).is_ok_and(|pose| {
            let [x, y, _] = pose.position.map(f32::from_bits);
            in_tactical_polygon(&path.points, x, y)
        })
    })
}

pub(super) fn placed_logic_objects<'a>(
    mission: &'a super::MissionDocument,
    world: &'a World,
    id: u32,
) -> impl Iterator<Item = ObjectHandle> + 'a {
    // shortcut: only TMA IDs, until the world assigns native IDs at runtime.
    mission
        .objects
        .iter()
        .enumerate()
        .filter(move |(_, object)| id != u32::MAX && object.identity_or_clan_raw == id)
        .filter_map(|(index, _)| handle_by_original_id(world, OriginalObjectId(index as u32)))
}

fn in_tactical_polygon(vertices: &[[f32; 3]], x: f32, y: f32) -> bool {
    let Some(&[mut previous_x, mut previous_y, _]) = vertices.last() else {
        return false;
    };
    let mut inside = false;
    for &[edge_x, edge_y, _] in vertices {
        if (y <= edge_y) != (y <= previous_y) {
            let crossed = if (x <= edge_x) == (x <= previous_x) {
                x <= edge_x
            } else {
                x <= (y - edge_y) * (previous_x - edge_x) / (previous_y - edge_y) + edge_x
            };
            inside ^= crossed;
        }
        previous_x = edge_x;
        previous_y = edge_y;
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::{
        capturer_chassis, clan_object_classes, in_tactical_polygon, inside_exit_volume,
        MissionEvents, MissionNotice, MissionObjectives,
    };
    use fparkan_world::{
        construct_object, new, register_object, request_delete, set_owner, ObjectDraft, OwnerId,
        WorldConfig,
    };

    #[test]
    fn free_unit_selection_skips_busy_reserved_foreign_and_removed_entries() {
        let mission = crate::MissionDocument {
            format_version: 1,
            clan_section_version: 6,
            object_section_version: 10,
            extra_section_version: 1,
            versions: vec![],
            paths: vec![],
            clans: vec![],
            objects: vec![],
            land_path: Default::default(),
            mission_flag: 0,
            description_raw: Default::default(),
            extras: vec![],
            raw: std::sync::Arc::from([]),
        };
        let mut world = new(WorldConfig);
        let factory = 0x8000_0010;
        let mut events = MissionEvents::default();
        let entries = [
            (91, 0x0100_8000, 1),
            (12345, factory, 1),
            (45678, factory, 1),
            (45679, factory, 2),
        ]
        .map(|(id, class, owner)| {
            let unit = construct_object(&mut world, ObjectDraft { original_id: None }).unwrap();
            set_owner(&mut world, unit, Some(OwnerId(owner))).unwrap();
            register_object(&mut world, unit).unwrap();
            events.placed_clan_objects.push((unit, class));
            events.issued_logic_ids.insert(unit, id);
            unit
        });
        assert_eq!(
            events.first_available_placed_unit(&mission, &world, 1, factory, |_| true),
            Some(12345)
        );
        assert_eq!(
            events.first_available_placed_unit(&mission, &world, 1, factory, |unit| unit
                != entries[1]),
            Some(45678)
        );
        events.reserved_units.insert((1, entries[1]));
        assert_eq!(
            events.first_available_placed_unit(&mission, &world, 1, factory, |_| true),
            Some(45678)
        );
        events.reserved_units.insert((1, entries[2]));
        assert_eq!(
            events.first_available_placed_unit(&mission, &world, 1, factory, |_| true),
            None
        );
        events.reserved_units.remove(&(1, entries[2]));
        request_delete(&mut world, entries[2]).unwrap();
        assert_eq!(
            events.first_available_placed_unit(&mission, &world, 1, factory, |_| true),
            None
        );
        events.reserved_units.clear();
        set_owner(&mut world, entries[1], Some(OwnerId(2))).unwrap();
        assert_eq!(
            events.first_available_placed_unit(&mission, &world, 1, factory, |_| true),
            None
        );
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_clan_checks_for_live_unit_in_problems0() {
        use crate::{EngineServices, MissionMode, MissionRequest, MissionStartupInput};
        use fparkan_vfs::DirectoryVfs;
        use std::sync::Arc;

        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").unwrap();
        let mut engine =
            crate::create(EngineServices::new(Arc::new(DirectoryVfs::new(root)))).unwrap();
        crate::load_gameplay_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/CAMPAIGN/CAMPAIGN.05/Mission.01/data.tma".into(),
                mode: MissionMode::SingleMission,
            },
            MissionStartupInput {
                queue_time_ms: 0,
                research_creation_gate: false,
            },
            None,
        )
        .unwrap();

        let loaded = engine.loaded.as_ref().unwrap();
        let unit = super::placed_logic_objects(&loaded.mission, &engine.world, 25)
            .next()
            .unwrap();
        let attackers = [2, 3, 23, 27, 28, 30].map(|id| {
            super::placed_logic_objects(&loaded.mission, &engine.world, id)
                .next()
                .unwrap()
        });
        let hero = super::placed_logic_objects(&loaded.mission, &engine.world, 12)
            .next()
            .unwrap();
        assert!(loaded
            .mission_events
            .has_live_logic_object(&loaded.mission, &engine.world, 25));
        crate::clan_ai_events::tick(&mut engine, 7_000);
        let patrol = engine
            .loaded
            .as_ref()
            .unwrap()
            .unit_patrol
            .as_ref()
            .unwrap();
        assert_eq!(patrol.attack_target(attackers[0]), None);
        request_delete(&mut engine.world, unit).unwrap();
        crate::clan_ai_events::tick(&mut engine, 14_000);
        let loaded = engine.loaded.as_ref().unwrap();
        assert!(!loaded
            .mission_events
            .has_live_logic_object(&loaded.mission, &engine.world, 25));
        assert!(!loaded.mission_events.has_live_logic_object(
            &loaded.mission,
            &engine.world,
            u32::MAX
        ));
        assert!(loaded.clan_ai_events.stops().next().is_none());
        let patrol = loaded.unit_patrol.as_ref().unwrap();
        assert!(attackers.into_iter().all(|handle| {
            patrol
                .attack_target(handle)
                .is_some_and(|(target, _)| target == hero)
        }));
    }

    #[test]
    fn exit_cylinder_includes_the_wall_and_caps() {
        let center = [10., 20., 30.];
        for (point, inside) in [
            ([15., 20., 30.], true),
            ([15.1, 20., 30.], false),
            ([10., 20., 27.], true),
            ([10., 20., 33.], true),
            ([10., 20., 33.1], false),
        ] {
            assert_eq!(inside_exit_volume(center, point), inside);
        }
    }

    #[test]
    fn capturer_classes_are_tiny_and_light() {
        for (key, expected) in [
            (b"R_T_02", true),
            (b"r_l_01", true),
            (b"R_H_03", true),
            (b"R_M_02", false),
            (b"R_B_02", false),
        ] {
            let mut chassis = [0; 32];
            chassis[..key.len()].copy_from_slice(key);
            assert_eq!(capturer_chassis(&chassis), expected);
        }
    }

    #[test]
    fn victory_requires_primary_objectives_in_config_order_only() {
        let config = b"object bonus_objectives\noptional = \"bonus\"\nend\nobject primary_objectives\na = \"one\"\na = \"two\"\nend\n";
        let mut objectives = MissionObjectives::from_config(config);
        let mut notices = Vec::new();
        assert!(!objectives.apply(3, 0, &mut notices).unwrap());
        assert!(!objectives.apply(3, 0, &mut notices).unwrap());
        assert!(!objectives.apply(4, 1, &mut notices).unwrap());
        assert!(!objectives.apply(3, 1, &mut notices).unwrap());
        assert!(!objectives.apply(5, 1, &mut notices).unwrap());
        assert!(objectives.apply(3, 1, &mut notices).unwrap());
        assert!(objectives.apply(3, 3, &mut notices).is_err());
        assert_eq!(
            notices,
            [
                MissionNotice::ObjectiveComplete(0),
                MissionNotice::ObjectiveComplete(1)
            ]
        );
    }

    #[test]
    fn clan_count_uses_overlapping_bits_and_skips_removed_objects() {
        let mut world = new(WorldConfig);
        let player = OwnerId(0);
        let neutral = OwnerId(3);
        let objects = [0x0102_0000, 0x0100_8000, 0x2000_0000, 0x8000_0010].map(|class| {
            let handle = construct_object(&mut world, ObjectDraft { original_id: None }).unwrap();
            register_object(&mut world, handle).unwrap();
            set_owner(&mut world, handle, Some(player)).unwrap();
            (handle, class)
        });
        let count_clan_objects = |objects: &[_], world: &_, owner, mask| {
            clan_object_classes(objects, world, owner)
                .filter(|class| class & mask != 0)
                .count()
        };
        assert_eq!(count_clan_objects(&objects, &world, player, 0x0100_8000), 2);
        assert_eq!(count_clan_objects(&objects, &world, player, 0x2100_0000), 3);
        assert_eq!(count_clan_objects(&objects, &world, player, 0x8000_0000), 1);
        set_owner(&mut world, objects[1].0, Some(neutral)).unwrap();
        assert_eq!(count_clan_objects(&objects, &world, player, 0x0100_0000), 1);
        assert_eq!(
            count_clan_objects(&objects, &world, neutral, 0x0100_0000),
            1
        );
        request_delete(&mut world, objects[0].0).unwrap();
        assert_eq!(count_clan_objects(&objects, &world, player, 0x0100_0000), 0);
    }

    #[test]
    fn live_exact_class_count_uses_owner_and_full_class_word() {
        let mut world = new(WorldConfig);
        let player = OwnerId(0);
        let other = OwnerId(1);
        let mine = 0x8000_0004;
        let objects =
            [(mine, player), (0x8000_0008, player), (mine, other)].map(|(class, owner)| {
                let handle =
                    construct_object(&mut world, ObjectDraft { original_id: None }).unwrap();
                set_owner(&mut world, handle, Some(owner)).unwrap();
                register_object(&mut world, handle).unwrap();
                (handle, class)
            });
        let count = |world: &_| {
            clan_object_classes(&objects, world, player)
                .filter(|&class| class == mine)
                .count()
        };
        assert_eq!(count(&world), 1);
        set_owner(&mut world, objects[2].0, Some(player)).unwrap();
        assert_eq!(count(&world), 2);
        request_delete(&mut world, objects[0].0).unwrap();
        assert_eq!(count(&world), 1);
    }

    #[test]
    fn tactical_polygon_preserves_original_edge_inclusion() {
        let points = [[0., 0., 0.], [4., 0., 1.], [4., 4., 2.], [0., 4., 3.]];
        for (point, expected) in [
            ([2., 2.], true),
            ([0., 2.], false),
            ([4., 2.], true),
            ([2., 0.], false),
            ([2., 4.], true),
        ] {
            assert_eq!(in_tactical_polygon(&points, point[0], point[1]), expected);
        }
    }

    #[test]
    fn deadline_is_inclusive_and_reschedules_from_current_sample_without_catch_up() {
        let mut events = MissionEvents::default();
        assert!(events.due(0));
        assert!(!events.due(0));
        assert!(!events.due(1_999));
        assert!(events.due(2_000));
        assert!(events.due(10_123));
        assert!(!events.due(12_122));
        assert!(events.due(12_123));
    }

    #[test]
    fn mission_timer_is_due_after_a_long_pause_without_advancing_the_world_clock() {
        let mut events = MissionEvents::default();
        let mut clocks = crate::clan_ai_events::ClanAiEvents::default();
        assert!(events.due(clocks.wall_time_ms(0)));
        assert!(!events.due(clocks.wall_time_ms(500)));
        clocks.add_paused_time(22_300);
        assert!(events.due(clocks.wall_time_ms(500)));
        assert!(!events.due(clocks.wall_time_ms(500)));
        assert!(!events.due(clocks.wall_time_ms(2_499)));
        assert!(events.due(clocks.wall_time_ms(2_500)));
    }
}
