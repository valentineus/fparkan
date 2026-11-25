/// Native Behavior timer deadline, in 64 ms base/jitter units.
pub(super) fn next_deadline(now_ms: u32, base: u16, jitter: u16, rng: &mut u32) -> u32 {
    let random = next_crt_rand(rng) & 0xff;
    let delay = (u32::from(base) << 6) + ((random * (u32::from(jitter) << 6)) >> 8);
    now_ms.wrapping_add(delay)
}

pub(super) fn next_crt_rand(state: &mut u32) -> u32 {
    *state = state.wrapping_mul(0x343fd).wrapping_add(0x269ec3);
    (*state >> 16) & 0x7fff
}

/// The owner task timer initialized with native words 1/1.
#[derive(Default)]
pub(super) struct TaskClock {
    deadline_ms: u32,
}

impl TaskClock {
    pub(super) fn due(&mut self, now_ms: u32, rng: &mut u32) -> bool {
        if self.deadline_ms != 0 && now_ms <= self.deadline_ms {
            return false;
        }
        self.deadline_ms = next_deadline(now_ms, 1, 1, rng);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_task_timer_requires_a_strictly_later_clock_and_consumes_rng_only_when_due() {
        let mut clock = TaskClock::default();
        let mut rng = 1;
        assert!(clock.due(100, &mut rng));
        assert_eq!((clock.deadline_ms, rng), (174, 2_745_024));
        assert!(!clock.due(174, &mut rng));
        assert_eq!((clock.deadline_ms, rng), (174, 2_745_024));
        assert!(clock.due(175, &mut rng));
        assert_eq!((clock.deadline_ms, rng), (247, 3_357_800_067));
        assert!(clock.due(1000, &mut rng));
        assert_eq!((clock.deadline_ms, rng), (1111, 415_139_642));
    }
}
