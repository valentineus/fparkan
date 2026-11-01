//! Per-instance time for the owner-independent Effect.dll phase modes.

/// The clock fields read by Effect.dll +0x5C60 and set by +0x7B00/+0x7B40.
#[derive(Clone, Debug)]
pub struct FxInstanceClock {
    header_mode: u32,
    mode: u32,
    header_flags: u32,
    duration_ms: f32,
    origin_ms: f32,
    end_ms: f32,
    constant_phase: f32,
    restart_requested: bool,
}

impl FxInstanceClock {
    /// Creates an instance at its own origin, using the document's default mode.
    /// Modes requiring an owner remain unsupported by this clock.
    pub fn new(
        mode: u32,
        header_flags: u32,
        duration_seconds: f32,
        origin_ms: f32,
    ) -> Option<Self> {
        let duration_ms = duration_seconds * 1000.0;
        if mode > 3
            || !duration_ms.is_finite()
            || duration_ms < 0.0
            || !origin_ms.is_finite()
            || (mode != 0 && duration_ms == 0.0)
            || !(origin_ms + duration_ms).is_finite()
        {
            return None;
        }
        Some(Self {
            header_mode: mode,
            mode,
            header_flags,
            duration_ms,
            origin_ms,
            end_ms: origin_ms + duration_ms,
            constant_phase: 0.0,
            restart_requested: false,
        })
    }

    /// Changes the caller-selected mode and epoch without changing the document.
    pub fn set_time(&mut self, now_ms: f32, mode: u32) -> Option<()> {
        if mode > 3
            || !now_ms.is_finite()
            || !(now_ms + self.duration_ms).is_finite()
            || (mode != 0 && self.duration_ms == 0.0)
        {
            return None;
        }
        self.origin_ms = now_ms;
        self.end_ms = now_ms + self.duration_ms;
        self.mode = mode;
        Some(())
    }

    /// Sets the value used by mode zero; command evaluation clamps it later.
    pub fn set_constant_phase(&mut self, phase: f32) -> Option<()> {
        if !phase.is_finite() {
            return None;
        }
        self.constant_phase = phase;
        Some(())
    }

    /// Requests the native deferred restart: the current lifetime finishes first.
    pub fn request_restart(&mut self) {
        self.restart_requested = true;
    }

    /// Applies the restart portion of Effect.dll +0x8120 before phase sampling.
    /// The new lifetime starts at the update time rather than retaining overshoot.
    pub fn update(&mut self, now_ms: f32) -> Option<()> {
        if !now_ms.is_finite() {
            return None;
        }
        if self.restart_requested && self.end_ms <= now_ms {
            self.set_time(now_ms, self.header_mode)?;
            self.restart_requested = false;
        }
        Some(())
    }

    /// Elapsed instance time for command simulation, independent of app startup.
    pub fn elapsed_seconds(&self, now_ms: f32) -> Option<f32> {
        now_ms
            .is_finite()
            .then_some((now_ms - self.origin_ms) * 0.001)
    }

    /// Samples the raw phase before command clamping and random phase jitter.
    /// `manager_flag_a8` is the native owner bit tested by header gates 0x80/0x100.
    pub fn phase(&self, now_ms: f32, manager_flag_a8: bool) -> Option<f32> {
        if !now_ms.is_finite() {
            return None;
        }
        if (self.header_flags & 0x80 != 0 && !manager_flag_a8)
            || (self.header_flags & 0x100 != 0 && manager_flag_a8)
        {
            return Some(0.0);
        }
        let ratio = (now_ms - self.origin_ms) / (self.end_ms - self.origin_ms);
        let mut phase = match self.mode {
            0 => self.constant_phase,
            1 => ratio,
            2 => ratio - ratio.floor(),
            3 => 1.0 - ratio,
            _ => return None,
        };
        if self.header_flags & 0x200 != 0 {
            phase *= ratio;
        }
        if self.header_flags & 0x20 != 0 {
            if phase >= 0.5 {
                phase = 1.0 - phase;
            }
            phase *= 2.0;
        }
        phase.is_finite().then_some(phase)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_epoch_modes_and_unclamped_header_transforms() {
        // Effect.dll GOG +0x5C60, x87 CW 007F; native oracle uses these epochs.
        for (mode, expected) in [
            (0, [0.375, 0.375, 0.375, 0.375, 0.375]),
            (1, [0.0, 0.25, 0.5, 1.0, 1.25]),
            (2, [0.0, 0.25, 0.5, 0.0, 0.25]),
            (3, [1.0, 0.75, 0.5, 0.0, -0.25]),
        ] {
            let mut clock = FxInstanceClock::new(mode, 0, 4.0, 135_000.0).unwrap();
            clock.set_constant_phase(0.375).unwrap();
            for (now, phase) in [135_000.0, 136_000.0, 137_000.0, 139_000.0, 140_000.0]
                .into_iter()
                .zip(expected)
            {
                assert_eq!(clock.phase(now, false), Some(phase));
            }
        }
        for (flags, expected) in [(0x200, 2.25), (0x20, -1.0), (0x220, -2.5)] {
            let clock = FxInstanceClock::new(1, flags, 4.0, 135_000.0).unwrap();
            assert_eq!(clock.phase(141_000.0, false), Some(expected));
        }
    }

    #[test]
    fn caller_mode_and_deferred_restart_use_the_instance_origin() {
        let mut clock = FxInstanceClock::new(2, 0, 4.0, 135_000.0).unwrap();
        clock.set_time(136_000.0, 3).unwrap();
        clock.request_restart();
        clock.update(139_999.0).unwrap();
        assert!(clock.phase(139_999.0, false).unwrap() < 0.001);
        clock.update(140_250.0).unwrap();
        assert_eq!(clock.phase(140_250.0, false), Some(0.0));
        assert_eq!(clock.elapsed_seconds(140_750.0), Some(0.5));
        assert_eq!(clock.phase(141_250.0, false), Some(0.25));
    }

    #[test]
    fn manager_header_gates_precede_phase_transforms() {
        for (flags, manager, expected) in [
            (0x80, false, 0.0),
            (0x80, true, 0.5),
            (0x100, false, 0.5),
            (0x100, true, 0.0),
        ] {
            let clock = FxInstanceClock::new(1, flags, 2.0, 20_000.0).unwrap();
            assert_eq!(clock.phase(21_000.0, manager), Some(expected));
        }
        assert_eq!(
            FxInstanceClock::new(0, 0, 0.0, 0.0)
                .unwrap()
                .phase(100.0, false),
            Some(0.0)
        );
        assert!(FxInstanceClock::new(1, 0, 0.0, 0.0).is_none());
        assert!(FxInstanceClock::new(0, 0, f32::MAX, 0.0).is_none());
        assert!(FxInstanceClock::new(4, 0, 2.0, 0.0).is_none());
    }
}
