use crate::{
    mining::MineSite, mission_events::MissionEvents, MissionControlDeviceTableState,
    MissionDocument, MissionObjectDraft,
};
use fparkan_script::VarSetDefault;
use fparkan_world::{identity_metadata, transform_state, OwnerId, World};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Selection {
    Vacant([f32; 3], f32),
    Mine(u32, f32),
    None,
}

struct Candidate {
    position: [f32; 3],
    occupied: bool,
    target: Option<(u32, f32)>,
}

pub(super) fn query(
    mission: &MissionDocument,
    roster: &MissionEvents,
    world: &World,
    drafts: &[MissionObjectDraft],
    controls: &[MissionControlDeviceTableState],
    sites: &[MineSite],
    caller: usize,
) -> Selection {
    let source = &mission.clans[caller];
    let enemy_clans: Vec<_> = mission
        .clans
        .iter()
        .enumerate()
        .filter_map(|(index, other)| {
            (index != caller
                && source.relations.iter().any(|relation| {
                    relation.relation_value == 0 && relation.other_clan_name.raw == other.name.raw
                }))
            .then_some(OwnerId(index as u16))
        })
        .collect();
    let enemies: Vec<_> = enemy_clans
        .iter()
        .flat_map(|&owner| roster.clan_units(world, owner, false))
        .collect();
    let strength = |position| {
        crate::clan_targeting::area_strength(
            world,
            drafts,
            controls,
            enemies.iter().copied(),
            position,
            350.0,
        )
    };
    // shortcut: use the current lode bindings until native ArealMap mineral records are wired.
    let candidates: Vec<_> = sites
        .iter()
        .map(|site| {
            let mine = site.live_mine(world);
            let target = mine.and_then(|handle| {
                let owner = identity_metadata(world, handle).ok()?.owner_id?;
                if !enemy_clans.contains(&owner) {
                    return None;
                }
                let id = roster.logic_id(mission, world, handle)?;
                let position = transform_state(world, handle)
                    .ok()?
                    .position
                    .map(f32::from_bits);
                Some((id, strength(position)))
            });
            Candidate {
                position: site.position,
                occupied: mine.is_some(),
                target,
            }
        })
        .collect();
    select(&candidates, source.anchor, strength)
}

fn select(
    candidates: &[Candidate],
    base: [f32; 2],
    strength: impl Fn([f32; 3]) -> f32,
) -> Selection {
    let mut nearest = None;
    let mut distance = 999_999_999;
    for site in candidates.iter().filter(|site| !site.occupied) {
        let dx = (site.position[0] - base[0]).abs();
        let dy = (site.position[1] - base[1]).abs();
        // The x87 result is truncated to a DWORD before comparing distances.
        let next = (dx * dx + dy * dy).sqrt() as u32;
        if next < distance {
            distance = next;
            nearest = Some(site.position);
        }
    }
    if let Some(position) = nearest {
        return Selection::Vacant(position, strength(position));
    }
    let mut weakest = Selection::None;
    let mut power = 1e12_f32;
    for &(id, candidate) in candidates.iter().filter_map(|site| site.target.as_ref()) {
        if candidate < power {
            power = candidate;
            weakest = Selection::Mine(id, candidate);
        }
    }
    weakest
}

impl Selection {
    pub(super) fn reservation(self) -> Option<[f32; 2]> {
        match self {
            Self::Vacant([x, y, _], _) => Some([x, y]),
            _ => None,
        }
    }

    pub(super) fn report_key(self) -> (u32, [u32; 2]) {
        match self {
            Self::Vacant([x, y, _], _) => (u32::MAX, [word(x), word(y)]),
            Self::Mine(id, _) => (id, [0; 2]),
            Self::None => (u32::MAX, [0; 2]),
        }
    }

    pub(super) fn write(
        self,
        references: &[u32],
        values: &mut [VarSetDefault],
    ) -> Result<u32, String> {
        let &[x, y, power] = references else {
            return Err("Handler(40) requires X, Y and strength outputs".into());
        };
        if [x, y, power]
            .iter()
            .any(|&index| values.get(index as usize).is_none())
        {
            return Err("Handler(40) output is unavailable".into());
        }
        let (id, outputs) = match self {
            Self::Vacant([x, y, _], power) => (
                u32::MAX,
                [
                    Some(word(x)),
                    Some(word(y)),
                    Some(crate::clan_targeting::encode_strength(power) as u32),
                ],
            ),
            Self::Mine(id, power) => (
                id,
                [
                    None,
                    None,
                    Some(crate::clan_targeting::encode_strength(power) as u32),
                ],
            ),
            Self::None => (u32::MAX, [Some(0), Some(0), None]),
        };
        for (index, output) in [x, y, power].into_iter().zip(outputs) {
            if let Some(output) = output {
                let cell = &mut values[index as usize];
                *cell = match *cell {
                    VarSetDefault::Dword(_) => VarSetDefault::Dword(output),
                    VarSetDefault::FloatBits(_) => {
                        VarSetDefault::FloatBits((output as f32).to_bits())
                    }
                };
            }
        }
        Ok(id)
    }
}

fn word(value: f32) -> u32 {
    value.trunc() as i64 as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_stock_clan_builds_a_mine_and_extracts_ore() {
        use crate::{EngineServices, MissionMode, MissionRequest, MissionStartupInput};
        use fparkan_vfs::DirectoryVfs;
        use std::sync::Arc;

        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").unwrap();
        let mut engine =
            crate::create(EngineServices::new(Arc::new(DirectoryVfs::new(root)))).unwrap();
        crate::load_gameplay_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/Single.02/data.tma".into(),
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
        assert!(loaded
            .mine_sites
            .iter()
            .all(|site| site.live_mine(&engine.world).is_none()));
        for now in (0..=120_000).step_by(100) {
            crate::frame(&mut engine, now).unwrap();
            let loaded = engine.loaded.as_ref().unwrap();
            if let Some((site_index, handle)) =
                loaded
                    .mine_sites
                    .iter()
                    .enumerate()
                    .find_map(|(index, site)| {
                        site.live_mine(&engine.world).map(|handle| (index, handle))
                    })
            {
                assert_eq!(
                    identity_metadata(&engine.world, handle).unwrap().owner_id,
                    Some(OwnerId(1))
                );
                assert!(loaded
                    .clan_ai_events
                    .mine_queries()
                    .any(|(clan, id, x, y, count)| clan == 1
                        && id == u32::MAX
                        && x > 0
                        && y > 0
                        && count > 0));
                assert!(!loaded
                    .clan_ai_events
                    .stops()
                    .any(|(stop, _)| stop.event_index == 21 || stop.event_index == 23));
                let [x, y, _] = loaded.mine_sites[site_index].position;
                let [mx, my, _] = transform_state(&engine.world, handle)
                    .unwrap()
                    .position
                    .map(f32::from_bits);
                assert!((x - mx).hypot(y - my) < 1.5);
                for later in (now + 100..=now + 2_000).step_by(100) {
                    crate::frame(&mut engine, later).unwrap();
                }
                let loaded = engine.loaded.as_ref().unwrap();
                let ore = loaded
                    .behavior_ore_states
                    .iter()
                    .find(|ore| ore.handle == handle)
                    .unwrap();
                assert!(f32::from_bits(ore.current_ore_bits) > 0.0);
                assert_eq!(f32::from_bits(ore.maximum_ore_bits), 500.0);
                return;
            }
        }
        panic!("no clan completed a mine from the stock start");
    }

    #[test]
    fn vacant_lode_precedes_enemy_mine_and_keeps_first_integer_distance_tie() {
        let sites = [
            Candidate {
                position: [1.0, 0.0, 0.0],
                occupied: true,
                target: Some((10, 0.0)),
            },
            Candidate {
                position: [100.9, 0.1, 0.0],
                occupied: false,
                target: None,
            },
            Candidate {
                position: [100.1, 0.1, 0.0],
                occupied: false,
                target: None,
            },
        ];
        assert_eq!(
            select(&sites, [0.0; 2], |_| 8.2),
            Selection::Vacant(sites[1].position, 8.2)
        );
    }

    #[test]
    fn occupied_lodes_choose_weakest_enemy_and_first_strength_tie() {
        let sites = [
            Candidate {
                position: [0.0; 3],
                occupied: true,
                target: None,
            },
            Candidate {
                position: [0.0; 3],
                occupied: true,
                target: Some((10, 8.0)),
            },
            Candidate {
                position: [0.0; 3],
                occupied: true,
                target: Some((11, 0.0)),
            },
            Candidate {
                position: [0.0; 3],
                occupied: true,
                target: Some((12, 0.0)),
            },
        ];
        assert_eq!(
            select(&sites, [0.0; 2], |_| unreachable!()),
            Selection::Mine(11, 0.0)
        );
        assert_eq!(
            select(&sites[..1], [0.0; 2], |_| unreachable!()),
            Selection::None
        );
    }

    #[test]
    fn output_writes_preserve_the_native_untouched_cells() {
        let mut values = [111, 222, 333].map(VarSetDefault::Dword);
        assert_eq!(
            Selection::None.write(&[0, 1, 2], &mut values).unwrap(),
            u32::MAX
        );
        assert_eq!(values, [0, 0, 333].map(VarSetDefault::Dword));
        values = [111, 222, 333].map(VarSetDefault::Dword);
        assert_eq!(
            Selection::Mine(10, 8.2)
                .write(&[0, 1, 2], &mut values)
                .unwrap(),
            10
        );
        assert_eq!(values, [111, 222, 8].map(VarSetDefault::Dword));
        assert_eq!(
            Selection::Vacant([500.9, 600.8, 0.0], 8.2)
                .write(&[0, 1, 2], &mut values)
                .unwrap(),
            u32::MAX
        );
        assert_eq!(values, [500, 600, 8].map(VarSetDefault::Dword));
    }
}
