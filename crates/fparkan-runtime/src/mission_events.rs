use super::LoadedMissionState;
use fparkan_script::{ScriptDispatchSelector, VarSetDefault};
use fparkan_world::{handle_by_original_id, transform_state, OriginalObjectId, World};
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
    /// IDs of messages requested by this invocation, in script order.
    pub messages: Vec<u32>,
}

#[derive(Default)]
pub(super) struct MissionEvents {
    next_ms: u32,
    reported_stops: HashSet<usize>,
    voiced_messages: HashSet<u32>,
}

impl MissionEvents {
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
        let mut messages = Vec::new();
        let stop = declarations
            .execute_mission_prefix(
                event,
                bundle.formula_table.as_ref(),
                &mut state.values,
                |instruction, values| match instruction.dispatch_selector() {
                    ScriptDispatchSelector::Handler(32) => {
                        let [area, id] = *instruction.references.as_slice() else {
                            return Err("Handler(32) requires two DWORD operands".into());
                        };
                        let (Some(VarSetDefault::Dword(area)), Some(VarSetDefault::Dword(id))) =
                            (values.get(area as usize), values.get(id as usize))
                        else {
                            return Err("Handler(32) requires two DWORD operands".into());
                        };
                        let inside = tactical_area_contains(&self.mission, world, *area, *id);
                        Ok(Some(u32::from(inside)))
                    }
                    ScriptDispatchSelector::Handler(30) => {
                        let command = declarations
                            .resolve_handler30_with_values(instruction, values)
                            .map_err(|error| error.to_string())?;
                        if self.script_host_mode == super::MissionMode::AutoDemo {
                            return Ok(None);
                        }
                        if command.first != 1 {
                            return Err(format!("Handler(30) command {}", command.first));
                        }
                        if self.mission_events.voiced_messages.insert(command.second) {
                            messages.push(command.second);
                        }
                        Ok(None)
                    }
                    selector => Err(format!("{selector:?}")),
                },
            )
            .err()
            .filter(|(index, _)| self.mission_events.reported_stops.insert(*index))
            .map(|(instruction_index, reason)| MissionEventStop {
                clan_index: bundle.clan_index,
                script_path: bundle.script_path.clone(),
                event_index,
                instruction_index,
                reason,
            });
        Some(MissionEventTick { stop, messages })
    }
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
    use super::{in_tactical_polygon, MissionEvents};

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
