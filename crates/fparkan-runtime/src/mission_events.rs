use super::{
    apply_recovered_script_host_callback, LoadedMissionState, MissionObjectDraft,
    MissionScriptHostEffect, LAST_SUPER_AI_TABLE_INDEX,
};
use fparkan_script::{
    ScriptDispatchSelector, ScriptInstruction, VarSetDefault, VmHostCallbackCommand,
};
use fparkan_world::{
    handle_by_original_id, transform_state, ObjectHandle, OriginalObjectId, World,
};
use std::collections::HashSet;

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
}

/// A notification emitted by the scenario host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MissionNotice {
    /// First presentation of an information message by index.
    Information(u32),
    /// Objective changed from pending to complete.
    ObjectiveComplete(u32),
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
struct MissionObjectives {
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

    fn apply(
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
    next_ms: u32,
    reported_stops: HashSet<usize>,
    voiced_messages: HashSet<u32>,
    placed_clan_objects: Vec<Vec<(ObjectHandle, u32)>>,
    objectives: MissionObjectives,
}

impl MissionEvents {
    #[cfg(test)]
    pub(super) fn add_test_robot(&mut self, clan: usize, handle: ObjectHandle) {
        self.placed_clan_objects[clan].push((handle, 0x0100_8000));
    }

    pub(super) fn with_placed_objects(
        drafts: &[MissionObjectDraft],
        handles: &[ObjectHandle],
        clan_count: usize,
        mission_config: &[u8],
    ) -> Self {
        let mut rosters = vec![Vec::new(); clan_count.min(LAST_SUPER_AI_TABLE_INDEX as usize + 1)];
        for (draft, &handle) in drafts.iter().zip(handles) {
            if let (Some(roster), Some(class)) = (
                rosters.get_mut(draft.raw_after_resource as usize),
                draft.unit_dat_header_flags,
            ) {
                roster.push((handle, class));
            }
        }
        Self {
            placed_clan_objects: rosters,
            objectives: MissionObjectives::from_config(mission_config),
            ..Self::default()
        }
    }

    fn due(&mut self, now_ms: u32) -> bool {
        if now_ms < self.next_ms {
            return false;
        }
        self.next_ms = now_ms.wrapping_add(2_000);
        true
    }
}

impl LoadedMissionState {
    pub(super) fn tick_mission_event(
        &mut self,
        world: &World,
        now_ms: u32,
    ) -> Option<MissionEventTick> {
        // shortcut: use the local clan (0 in the observed standalone modes),
        // until runtime has the original active-clan selector.
        let bundle = self
            .script_bundles
            .iter()
            .find(|bundle| bundle.clan_index == 0 && bundle.behavior_warm_start.is_some())?;
        // shortcut: use the caller's queue clock until the AI wall clock is wired.
        if !self.mission_events.due(now_ms) {
            return None;
        }
        let (event_index, event) =
            bundle
                .package
                .events
                .iter()
                .enumerate()
                .find(|(_, event)| {
                    event.name_raw.split(|byte| *byte == 0).next() == Some(b"Mission")
                })?;
        let declarations = &self.script_varset.as_ref()?.declarations;
        let state = self
            .script_varset_states
            .iter_mut()
            .find(|state| state.clan_index == bundle.clan_index)?;
        let mission_events = &mut self.mission_events;
        let mut notices = Vec::new();
        let stop = declarations
            .execute_mission_prefix(
                event,
                bundle.formula_table.as_ref(),
                &mut state.values,
                |instruction, values| match instruction.dispatch_selector() {
                    ScriptDispatchSelector::Handler(31) => {
                        let (clan, mask) = dword_pair(instruction, values)
                            .ok_or("Handler(31) requires two DWORD operands")?;
                        let count = self
                            .behavior_distributors
                            .get(clan as usize)
                            .filter(|distributor| distributor.super_ai_visible)
                            .and_then(|_| mission_events.placed_clan_objects.get(clan as usize))
                            .map_or(0, |objects| count_clan_objects(objects, world, mask));
                        Ok(Some(count))
                    }
                    ScriptDispatchSelector::Handler(32) => {
                        let (area, id) = dword_pair(instruction, values)
                            .ok_or("Handler(32) requires two DWORD operands")?;
                        let inside = tactical_area_contains(&self.mission, world, area, id);
                        Ok(Some(u32::from(inside)))
                    }
                    ScriptDispatchSelector::Handler(30) => {
                        let command = declarations
                            .resolve_handler30_with_values(instruction, values)
                            .map_err(|error| error.to_string())?;
                        if self.script_host_mode == super::MissionMode::AutoDemo {
                            return Ok(None);
                        }
                        let outcome = match command.first {
                            0 => Some(command),
                            1 => {
                                if mission_events.voiced_messages.insert(command.second) {
                                    notices.push(MissionNotice::Information(command.second));
                                }
                                None
                            }
                            3..=5 => mission_events
                                .objectives
                                .apply(command.first, command.second, &mut notices)?
                                .then_some(VmHostCallbackCommand {
                                    mode: 0,
                                    first: 0,
                                    second: 1,
                                }),
                            _ => return Err(format!("Handler(30) command {}", command.first)),
                        };
                        if let Some(outcome) = outcome {
                            match apply_recovered_script_host_callback(
                                self.script_host_mode,
                                &mut self.script_host_status,
                                &mut self.script_outcome,
                                outcome,
                            ) {
                                MissionScriptHostEffect::MissionCompleted => {
                                    notices.push(MissionNotice::Completed)
                                }
                                MissionScriptHostEffect::MissionFailed => {
                                    notices.push(MissionNotice::Failed)
                                }
                                MissionScriptHostEffect::NoChange => {}
                                MissionScriptHostEffect::Unhandled => {
                                    return Err(format!("Handler(30) outcome {}", outcome.second))
                                }
                            }
                        }
                        Ok(None)
                    }
                    selector => Err(format!("{selector:?}")),
                },
            )
            .err()
            .filter(|(index, _)| mission_events.reported_stops.insert(*index))
            .map(|(instruction_index, reason)| MissionEventStop {
                clan_index: bundle.clan_index,
                script_path: bundle.script_path.clone(),
                event_index,
                instruction_index,
                reason,
            });
        Some(MissionEventTick { stop, notices })
    }
}

fn dword_pair(instruction: &ScriptInstruction, values: &[VarSetDefault]) -> Option<(u32, u32)> {
    let [first, second] = *instruction.references.as_slice() else {
        return None;
    };
    match (values.get(first as usize)?, values.get(second as usize)?) {
        (&VarSetDefault::Dword(first), &VarSetDefault::Dword(second)) => Some((first, second)),
        _ => None,
    }
}

fn count_clan_objects(objects: &[(ObjectHandle, u32)], world: &World, mask: u32) -> u32 {
    // shortcut: placed units retain their starting clan until capture and
    // production update the roster.
    objects
        .iter()
        .filter(|&&(handle, class)| {
            class & mask != 0
                && fparkan_world::registration_sequence(world, handle)
                    .is_ok_and(|seq| seq.is_some())
        })
        .count() as u32
}

fn tactical_area_contains(
    mission: &super::MissionDocument,
    world: &World,
    area: u32,
    id: u32,
) -> bool {
    // MisLoad allocates a slot per path and selects the first record matching
    // the slot ID. Out-of-range IDs and unfilled slots yield an empty list.
    if area as usize >= mission.paths.len() || id == u32::MAX {
        return false;
    }
    let Some(path) = mission.paths.iter().find(|path| path.id as u32 == area) else {
        return false;
    };
    // shortcut: resolve placed objects through their TMA logic ID until the
    // world assigns native logic IDs to units created during gameplay.
    mission.objects.iter().enumerate().any(|(index, object)| {
        object.identity_or_clan_raw == id
            && handle_by_original_id(world, OriginalObjectId(index as u32))
                .and_then(|handle| transform_state(world, handle).ok())
                .is_some_and(|pose| {
                    let [x, y, _] = pose.position.map(f32::from_bits);
                    in_tactical_polygon(&path.points, x, y)
                })
    })
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
        count_clan_objects, in_tactical_polygon, MissionEvents, MissionNotice, MissionObjectives,
    };
    use fparkan_world::{
        construct_object, new, register_object, request_delete, ObjectDraft, WorldConfig,
    };

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
        let objects = [0x0102_0000, 0x0100_8000, 0x2000_0000, 0x8000_0010].map(|class| {
            let handle = construct_object(&mut world, ObjectDraft { original_id: None }).unwrap();
            register_object(&mut world, handle).unwrap();
            (handle, class)
        });
        assert_eq!(count_clan_objects(&objects, &world, 0x0100_8000), 2);
        assert_eq!(count_clan_objects(&objects, &world, 0x2100_0000), 3);
        assert_eq!(count_clan_objects(&objects, &world, 0x8000_0000), 1);
        request_delete(&mut world, objects[0].0).unwrap();
        assert_eq!(count_clan_objects(&objects, &world, 0x0100_0000), 1);
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
}
