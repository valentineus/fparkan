use std::collections::BTreeMap;

struct Clock {
    deadline_ms: u32,
    seconds: u32,
    ticks: u64,
}

impl Default for Clock {
    fn default() -> Self {
        Self {
            // shortcut: the initial phase and registration's 1400 ms advance remain unwired.
            deadline_ms: 7_000,
            seconds: 0,
            ticks: 0,
        }
    }
}

#[derive(Default)]
pub(super) struct ClanClocks(BTreeMap<usize, Clock>);

impl ClanClocks {
    /// ai.dll RVA 0x17ae: equality is not due; a late callback runs one cycle.
    pub(super) fn tick(&mut self, clan: usize, elapsed_ms: u32, rng: &mut u32) -> Option<u32> {
        let clock = self.0.entry(clan).or_default();
        if elapsed_ms <= clock.deadline_ms {
            return None;
        }
        let jitter = crate::behavior_clock::next_crt_rand(rng) % 1_000;
        clock.deadline_ms = elapsed_ms.wrapping_add(7_000 + jitter);
        clock.seconds = clock.seconds.wrapping_add(7);
        clock.ticks += 1;
        if clock.ticks == 1 {
            eprintln!("sim_coverage script_clan_clock_approx=1 clan={clan}");
        }
        Some(clock.seconds)
    }

    pub(super) fn ticks(&self) -> impl Iterator<Item = (usize, u64)> + '_ {
        self.0.iter().map(|(&clan, clock)| (clan, clock.ticks))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_rng_jitter_is_shared_between_clans_and_equality_does_not_consume_it() {
        let mut clocks = ClanClocks::default();
        let mut rng = 1;
        assert_eq!(clocks.tick(1, 7_000, &mut rng), None);
        assert_eq!(rng, 1);
        assert_eq!(clocks.tick(1, 7_001, &mut rng), Some(7));
        assert_eq!((clocks.0[&1].deadline_ms, rng), (14_042, 2_745_024));
        assert_eq!(clocks.tick(2, 7_001, &mut rng), Some(7));
        assert_eq!((clocks.0[&2].deadline_ms, rng), (14_468, 3_357_800_067));
        assert_eq!(clocks.tick(1, 14_042, &mut rng), None);
        assert_eq!(rng, 3_357_800_067);
        assert_eq!(clocks.tick(1, 14_043, &mut rng), Some(14));
        assert_eq!((clocks.0[&1].deadline_ms, rng), (21_377, 415_139_642));
    }

    #[test]
    fn a_late_callback_advances_logical_time_once_and_schedules_from_actual_time() {
        let mut clocks = ClanClocks::default();
        let mut rng = 1;
        assert_eq!(clocks.tick(1, 100_000, &mut rng), Some(7));
        assert_eq!(clocks.0[&1].deadline_ms, 107_041);
        assert_eq!(clocks.tick(1, 100_000, &mut rng), None);
        assert_eq!(clocks.ticks().collect::<Vec<_>>(), [(1, 1)]);
    }
}
