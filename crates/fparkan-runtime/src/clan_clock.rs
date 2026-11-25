use std::collections::BTreeMap;

struct Clock {
    deadline_ms: i64,
    seconds: u32,
    ticks: u64,
    registrations: u64,
}

impl Default for Clock {
    fn default() -> Self {
        Self {
            // A fresh native context is already due; do not reproduce its uninitialized heap bytes.
            deadline_ms: -1,
            seconds: 0,
            ticks: 0,
            registrations: 0,
        }
    }
}

#[derive(Default)]
pub(super) struct ClanClocks {
    clans: BTreeMap<usize, Clock>,
    paused_ms: u32,
}

impl ClanClocks {
    /// ai.dll RVA 0x17ae: equality is not due; a late callback runs one cycle.
    pub(super) fn tick(&mut self, clan: usize, elapsed_ms: u32, rng: &mut u32) -> Option<u32> {
        let elapsed_ms = self.wall_time_ms(elapsed_ms);
        let clock = self.clans.entry(clan).or_default();
        if i64::from(elapsed_ms) <= clock.deadline_ms {
            return None;
        }
        let jitter = crate::behavior_clock::next_crt_rand(rng) % 1_000;
        clock.deadline_ms = i64::from(elapsed_ms.wrapping_add(7_000 + jitter));
        clock.seconds = clock.seconds.wrapping_add(7);
        clock.ticks += 1;
        if clock.ticks == 1 {
            eprintln!("sim_coverage script_clan_clock_approx=1 clan={clan}");
        }
        Some(clock.seconds)
    }

    pub(super) fn advance_for_registration(&mut self, clan: usize) -> u64 {
        let clock = self.clans.entry(clan).or_default();
        clock.deadline_ms = clock.deadline_ms.wrapping_sub(1_400);
        clock.registrations += 1;
        clock.registrations
    }

    pub(super) fn ticks(&self) -> impl Iterator<Item = (usize, u64, u64)> + '_ {
        self.clans
            .iter()
            .map(|(&clan, clock)| (clan, clock.ticks, clock.registrations))
    }

    pub(super) fn seconds(&self, clan: usize) -> u32 {
        self.clans.get(&clan).map_or(0, |clock| clock.seconds)
    }

    pub(super) fn wall_time_ms(&self, queue_ms: u32) -> u32 {
        queue_ms.wrapping_add(self.paused_ms)
    }

    pub(super) fn add_paused_time(&mut self, paused_ms: u32) {
        self.paused_ms = self.paused_ms.wrapping_add(paused_ms);
    }
}

/// Records wall time excluded by the caller's paused world clock.
/// The next frame checks AI deadlines once, without advancing the world or replaying missed cycles.
pub fn note_mission_pause_elapsed(engine: &mut crate::Engine, elapsed_ms: u32) {
    if let Some(loaded) = &mut engine.loaded {
        loaded.clan_ai_events.add_paused_time(elapsed_ms);
        eprintln!("script_ai_pause_elapsed duration_ms={elapsed_ms}");
    }
}

impl crate::LoadedMissionState {
    pub(super) fn note_clan_registration(
        &mut self,
        owner: fparkan_world::OwnerId,
        unit: fparkan_world::ObjectHandle,
    ) {
        let clan = usize::from(owner.0);
        if !self
            .script_bundles
            .iter()
            .any(|bundle| bundle.clan_index == clan && bundle.behavior_warm_start.is_some())
        {
            return;
        }
        let count = self.clan_ai_events.advance_for_registration(clan);
        eprintln!(
            "script_clan_unit_registered clan={clan} unit={} generation={} count={count}",
            unit.slot, unit.generation
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pause_ages_deadlines_without_ticking_or_replaying_missed_cycles() {
        let mut clocks = ClanClocks::default();
        let mut rng = 1;
        assert_eq!(clocks.tick(0, 0, &mut rng), Some(7));
        assert_eq!(clocks.tick(1, 0, &mut rng), Some(7));
        let before_pause = rng;
        clocks.add_paused_time(22_300);
        assert_eq!(
            (clocks.seconds(0), clocks.seconds(1), rng),
            (7, 7, before_pause)
        );
        assert_eq!(clocks.tick(0, 500, &mut rng), Some(14));
        assert_eq!(clocks.tick(1, 500, &mut rng), Some(14));
        let resumed_rng = rng;
        assert_eq!(clocks.tick(0, 500, &mut rng), None);
        assert_eq!(clocks.tick(1, 500, &mut rng), None);
        assert_eq!(rng, resumed_rng);
        assert_eq!(clocks.clans[&0].deadline_ms, 22_800 + 7_334);
        assert_eq!(clocks.tick(0, 7_834, &mut rng), None);
        assert_eq!(clocks.tick(0, 7_835, &mut rng), Some(21));
    }

    #[test]
    fn native_rng_jitter_is_shared_between_clans_and_equality_does_not_consume_it() {
        let mut clocks = ClanClocks::default();
        let mut rng = 1;
        assert_eq!(clocks.seconds(0), 0);
        assert_eq!(clocks.tick(0, 0, &mut rng), Some(7));
        assert_eq!((clocks.clans[&0].deadline_ms, rng), (7_041, 2_745_024));
        assert_eq!(clocks.tick(1, 0, &mut rng), Some(7));
        assert_eq!((clocks.clans[&1].deadline_ms, rng), (7_467, 3_357_800_067));
        assert_eq!(clocks.tick(0, 7_041, &mut rng), None);
        assert_eq!(rng, 3_357_800_067);
        assert_eq!(clocks.tick(0, 7_042, &mut rng), Some(14));
        assert_eq!((clocks.clans[&0].deadline_ms, rng), (14_376, 415_139_642));
        assert_eq!((clocks.seconds(0), clocks.seconds(1)), (14, 7));
    }

    #[test]
    fn a_late_callback_advances_logical_time_once_and_schedules_from_actual_time() {
        let mut clocks = ClanClocks::default();
        let mut rng = 1;
        assert_eq!(clocks.tick(1, 100_000, &mut rng), Some(7));
        assert_eq!(clocks.clans[&1].deadline_ms, 107_041);
        assert_eq!(clocks.tick(1, 100_000, &mut rng), None);
        assert_eq!(clocks.ticks().collect::<Vec<_>>(), [(1, 1, 0)]);
    }

    #[test]
    fn native_registration_shift_accumulates_and_a_negative_deadline_is_due() {
        let mut clocks = ClanClocks::default();
        clocks.clans.entry(1).or_default().deadline_ms = 8_000;
        assert_eq!(clocks.advance_for_registration(1), 1);
        assert_eq!(clocks.clans[&1].deadline_ms, 6_600);
        assert_eq!(clocks.advance_for_registration(1), 2);
        assert_eq!(clocks.clans[&1].deadline_ms, 5_200);
        clocks.clans.get_mut(&1).unwrap().deadline_ms = 500;
        clocks.advance_for_registration(1);
        assert_eq!(clocks.clans[&1].deadline_ms, -900);
        let mut rng = 1;
        assert_eq!(clocks.tick(1, 0, &mut rng), Some(7));
        assert_eq!(clocks.clans[&1].deadline_ms, 7_041);
        assert_eq!(rng, 2_745_024);
        assert_eq!(clocks.ticks().collect::<Vec<_>>(), [(1, 1, 3)]);
    }

    #[test]
    #[ignore = "requires licensed mission resources"]
    fn licensed_stock_clocks_start_immediately_and_include_the_player_context() {
        use crate::{
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
        frame(&mut engine, 0).unwrap();
        assert_eq!(
            engine
                .loaded
                .as_ref()
                .unwrap()
                .clan_ai_events
                .clock_ticks()
                .collect::<Vec<_>>(),
            [(0, 1, 0), (1, 1, 0)]
        );
        for now_ms in (100..=28_000).step_by(100) {
            frame(&mut engine, now_ms).unwrap();
        }
        let loaded = engine.loaded.as_ref().unwrap();
        assert_eq!(
            loaded.clan_ai_events.clock_ticks().collect::<Vec<_>>(),
            [(0, 4, 0), (1, 4, 1)]
        );
        assert!(loaded.clan_ai_events.stops().next().is_none());
        note_mission_pause_elapsed(&mut engine, 22_300);
        frame(&mut engine, 28_000).unwrap();
        assert_eq!(
            engine
                .loaded
                .as_ref()
                .unwrap()
                .clan_ai_events
                .clock_ticks()
                .collect::<Vec<_>>(),
            [(0, 5, 0), (1, 5, 1)]
        );
        frame(&mut engine, 28_000).unwrap();
        assert_eq!(
            engine
                .loaded
                .as_ref()
                .unwrap()
                .clan_ai_events
                .clock_ticks()
                .collect::<Vec<_>>(),
            [(0, 5, 0), (1, 5, 1)]
        );
    }
}
