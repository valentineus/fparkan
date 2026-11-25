use crate::{
    mission_events::MissionEvents, MissionControlDeviceTableState, MissionDocument,
    MissionObjectDraft,
};
use fparkan_script::{ScriptInstruction, VarSetDefault};
use fparkan_world::{transform_state, OwnerId, World};

pub(super) fn query(
    mission: &MissionDocument,
    roster: &MissionEvents,
    world: &World,
    drafts: &[MissionObjectDraft],
    controls: &[MissionControlDeviceTableState],
    caller: usize,
    instruction: &ScriptInstruction,
    values: &[VarSetDefault],
) -> Result<u32, String> {
    let number = |index: u32| {
        values.get(index as usize).map(|cell| match *cell {
            VarSetDefault::Dword(value) => value as f32,
            VarSetDefault::FloatBits(bits) => f32::from_bits(bits),
        })
    };
    let word = |index: u32| values.get(index as usize).and_then(|cell| cell.as_dword());
    let &[radius, clan, mode, ref target @ ..] = instruction.references.as_slice() else {
        return Err("Handler(44) requires radius, clan and a place or logical target".into());
    };
    let radius = number(radius).ok_or("Handler(44) radius is unavailable")?;
    let clan = word(clan).ok_or("Handler(44) clan is unavailable")?;
    let position = match (word(mode), target) {
        (Some(0x202), &[x, y]) => Some([
            number(x).ok_or("Handler(44) X is unavailable")?,
            number(y).ok_or("Handler(44) Y is unavailable")?,
            0.0,
        ]),
        (Some(0x201), &[id]) => {
            let id = word(id).ok_or("Handler(44) logical ID is unavailable")?;
            roster
                .logic_objects(mission, world, id)
                .find_map(|handle| transform_state(world, handle).ok())
                .map(|pose| pose.position.map(f32::from_bits))
        }
        _ => return Err("Handler(44) requires TARGET_BY_PLACE or TARGET_BY_LOGIC_ID".into()),
    };
    let Some(position) = position else {
        return Ok(0);
    };
    let source = &mission.clans[caller];
    let units = mission
        .clans
        .iter()
        .enumerate()
        .filter(|(index, candidate)| {
            if clan != u32::MAX {
                return *index == clan as usize;
            }
            *index != caller
                && source.relations.iter().any(|relation| {
                    relation.other_clan_name.raw == candidate.name.raw
                        && relation.relation_value == 0
                })
        })
        .flat_map(|(index, _)| roster.clan_units(world, OwnerId(index as u16), false));
    let strength =
        crate::clan_targeting::area_strength(world, drafts, controls, units, position, radius);
    Ok(crate::clan_targeting::encode_strength(strength) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_area_queries_accept_both_target_forms_and_continue_the_script() {
        use crate::{EngineServices, MissionMode, MissionRequest, MissionStartupInput};
        use fparkan_vfs::DirectoryVfs;
        use std::sync::Arc;

        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed corpus root");
        let mut engine =
            crate::create(EngineServices::new(Arc::new(DirectoryVfs::new(root)))).unwrap();
        crate::load_gameplay_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/CAMPAIGN/CAMPAIGN.01/Mission.03/data.tma".into(),
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
        let run = |references: &[u32], values: &[VarSetDefault]| {
            let instruction = ScriptInstruction {
                header_words: [
                    44,
                    6,
                    u32::MAX,
                    u32::MAX,
                    6,
                    references.len() as u32,
                    u32::MAX,
                ],
                references: references.to_vec(),
            };
            query(
                &loaded.mission,
                &loaded.mission_events,
                &engine.world,
                &loaded.object_drafts,
                &loaded.control_device_tables,
                0,
                &instruction,
                values,
            )
        };
        let mut values = vec![
            VarSetDefault::FloatBits(10_000.0_f32.to_bits()),
            VarSetDefault::Dword(1),
            VarSetDefault::Dword(0x202),
            VarSetDefault::Dword(0),
            VarSetDefault::Dword(0),
            VarSetDefault::Dword(0x8000_0001),
        ];
        let own = run(&[0, 1, 2, 3, 4], &values).unwrap();
        assert!(own > 0);
        values[2] = VarSetDefault::Dword(0x201);
        assert_eq!(run(&[0, 1, 2, 5], &values).unwrap(), own);
        values[5] = VarSetDefault::Dword(u32::MAX);
        assert_eq!(run(&[0, 1, 2, 5], &values).unwrap(), 0);
        values[2] = VarSetDefault::Dword(0x202);
        values[3] = VarSetDefault::FloatBits(0.25_f32.to_bits());
        assert_eq!(run(&[0, 1, 2, 3, 4], &values).unwrap(), own);
        assert!(run(&[0, 1, 2, 3], &values).is_err());
        assert!(run(&[0, 1, 2, 3, 99], &values).is_err());

        for now in (0..=7_000).step_by(100) {
            crate::frame(&mut engine, now).unwrap();
        }
        let loaded = engine.loaded.as_ref().unwrap();
        assert!(loaded.clan_ai_events.area_strength_queries().any(
            |(clan, event, instruction, count)| {
                (clan, event, instruction, count) == (1, 1, 12, 1)
            }
        ));
        assert!(loaded
            .clan_ai_events
            .stops()
            .all(|(stop, _)| stop.reason != "Handler(44)"));
    }
}
