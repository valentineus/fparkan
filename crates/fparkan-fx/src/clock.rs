//! Per-instance time and owner values for Effect.dll phase modes.

/// Values read by Effect.dll +0x5C60 from the selected FX parameter and owner.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FxPhaseProvider {
    /// Per-effect value returned by the manager's parameter provider.
    pub effect_value: f32,
    /// Owner selector 0x21.
    pub linear_velocity: [f32; 3],
    /// Owner selector 0x11, float fields 6..=8.
    pub linear_velocity_limit: [f32; 3],
    /// Owner selector 0x24.
    pub angular_velocity: [f32; 3],
    /// Owner selector 0x11, float fields 9..=11.
    pub angular_velocity_limit: [f32; 3],
    /// Selected FML provider value, read in query mode one.
    pub control_value: f32,
    /// Owner selector 0x31.
    pub health: f32,
}

impl FxPhaseProvider {
    /// Explicit preview stub: stationary, healthy, with complementary values at one.
    /// Unit limits are neutral denominators until live owner data is connected.
    pub const fn idle() -> Self {
        Self {
            effect_value: 0.0,
            linear_velocity: [0.0; 3],
            linear_velocity_limit: [1.0; 3],
            angular_velocity: [0.0; 3],
            angular_velocity_limit: [1.0; 3],
            control_value: 1.0,
            health: 1.0,
        }
    }

    fn phase(&self, mode: u32, previous: f32) -> Option<f32> {
        let length = |v: [f32; 3]| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        Some(match mode {
            4 => self.effect_value,
            5 => length(self.linear_velocity) / length(self.linear_velocity_limit),
            6..=8 => {
                let axis = (mode - 6) as usize;
                self.linear_velocity[axis] / self.linear_velocity_limit[axis]
            }
            9 => length(self.angular_velocity) / length(self.angular_velocity_limit),
            10..=12 => {
                let axis = (mode - 10) as usize;
                self.angular_velocity[axis] / self.angular_velocity_limit[axis]
            }
            13 => 1.0 - self.control_value,
            14 => 1.0 - self.health,
            15 => {
                let linear = length(self.linear_velocity) / length(self.linear_velocity_limit);
                let angular = length(self.angular_velocity) / length(self.angular_velocity_limit);
                if !linear.is_finite() || !angular.is_finite() {
                    return None;
                }
                linear.max(angular)
            }
            16 if self.effect_value < previous && self.effect_value != 0.0 => previous,
            17 if self.effect_value > previous && self.effect_value != 1.0 => previous,
            16 | 17 => self.effect_value,
            _ => return None,
        })
    }
}

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
    previous_phase: f32,
    restart_requested: bool,
}

impl FxInstanceClock {
    /// Creates an instance at its own origin, using the document's default mode.
    pub fn new(
        mode: u32,
        header_flags: u32,
        duration_seconds: f32,
        origin_ms: f32,
    ) -> Option<Self> {
        let duration_ms = duration_seconds * 1000.0;
        if mode > 17
            || !duration_ms.is_finite()
            || duration_ms < 0.0
            || !origin_ms.is_finite()
            || (matches!(mode, 1..=3) && duration_ms == 0.0)
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
            previous_phase: 0.0,
            restart_requested: false,
        })
    }

    /// Changes the caller-selected mode and epoch without changing the document.
    pub fn set_time(&mut self, now_ms: f32, mode: u32) -> Option<()> {
        if mode > 17
            || !now_ms.is_finite()
            || !(now_ms + self.duration_ms).is_finite()
            || (matches!(mode, 1..=3) && self.duration_ms == 0.0)
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
    pub fn phase(
        &mut self,
        now_ms: f32,
        manager_flag_a8: bool,
        provider: Option<&FxPhaseProvider>,
    ) -> Option<f32> {
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
            4..=17 => provider?.phase(self.mode, self.previous_phase)?,
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
        if !phase.is_finite() {
            return None;
        }
        self.previous_phase = phase;
        Some(phase)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_phase_modes_have_clocks_even_without_a_time_duration() {
        for mode in 4..=17 {
            assert!(
                FxInstanceClock::new(mode, 0, 0.0, 0.0).is_some(),
                "mode {mode}"
            );
        }
    }

    #[test]
    fn native_owner_phase_vectors_and_idle_defaults() {
        // Effect.dll GOG +0x5C60, x87 CW 007F; callbacks expose these values.
        let provider = FxPhaseProvider {
            effect_value: 0.375,
            linear_velocity: [3.0, -4.0, 12.0],
            linear_velocity_limit: [6.0, 8.0, 24.0],
            angular_velocity: [2.0, -3.0, 6.0],
            angular_velocity_limit: [4.0, 6.0, 12.0],
            control_value: 0.25,
            health: 0.4,
        };
        let expected = [
            0.375, 0.5, 0.5, -0.5, 0.5, 0.5, 0.5, -0.5, 0.5, 0.75, 0.6, 0.5, 0.375, 0.0,
        ];
        for (mode, expected) in (4..=17).zip(expected) {
            let mut clock = FxInstanceClock::new(mode, 0, 4.0, 1_000.0).unwrap();
            assert_eq!(clock.phase(3_000.0, false, None), None);
            assert_eq!(clock.phase(3_000.0, false, Some(&provider)), Some(expected));
            let mut clock = FxInstanceClock::new(mode, 0x200, 4.0, 1_000.0).unwrap();
            assert_eq!(
                clock.phase(3_000.0, false, Some(&provider)),
                Some(expected * 0.5)
            );
            for flags in [0x20, 0x220] {
                let mut clock = FxInstanceClock::new(mode, flags, 4.0, 1_000.0).unwrap();
                let value = if flags & 0x200 != 0 {
                    expected * 0.5
                } else {
                    expected
                };
                let folded = if value >= 0.5 { 1.0 - value } else { value } * 2.0;
                assert_eq!(clock.phase(3_000.0, false, Some(&provider)), Some(folded));
            }
            let mut clock = FxInstanceClock::new(mode, 0, 0.0, 0.0).unwrap();
            assert_eq!(
                clock.phase(0.0, false, Some(&FxPhaseProvider::idle())),
                Some(0.0)
            );
        }
        let mut clock = FxInstanceClock::new(15, 0, 4.0, 0.0).unwrap();
        let mut faster_turn = provider;
        faster_turn.angular_velocity = [3.0, -4.5, 9.0];
        assert_eq!(clock.phase(0.0, false, Some(&faster_turn)), Some(0.75));
        faster_turn.angular_velocity_limit = [0.0; 3];
        assert_eq!(clock.phase(0.0, false, Some(&faster_turn)), None);
    }

    #[test]
    fn native_owner_holds_reset_endpoints_and_retains_phase_across_set_time() {
        for (mode, expected) in [
            (16, [0.6, 0.6, 0.0, 0.3, 0.9, 1.0, 1.0]),
            (17, [0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.4]),
        ] {
            let mut clock = FxInstanceClock::new(mode, 0, 4.0, 0.0).unwrap();
            for (value, expected) in [0.6, 0.2, 0.0, 0.3, 0.9, 1.0, 0.4]
                .into_iter()
                .zip(expected)
            {
                clock.set_time(1_000.0, mode).unwrap();
                let provider = FxPhaseProvider {
                    effect_value: value,
                    ..FxPhaseProvider::idle()
                };
                assert_eq!(clock.phase(1_000.0, false, Some(&provider)), Some(expected));
            }
        }
        let mut clock = FxInstanceClock::new(16, 0x80, 4.0, 0.0).unwrap();
        let mut provider = FxPhaseProvider {
            effect_value: 0.8,
            ..FxPhaseProvider::idle()
        };
        assert_eq!(clock.phase(0.0, true, Some(&provider)), Some(0.8));
        provider.effect_value = 0.2;
        assert_eq!(clock.phase(0.0, false, Some(&provider)), Some(0.0));
        assert_eq!(clock.phase(0.0, true, Some(&provider)), Some(0.8));
    }

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
                assert_eq!(clock.phase(now, false, None), Some(phase));
            }
        }
        for (flags, expected) in [(0x200, 2.25), (0x20, -1.0), (0x220, -2.5)] {
            let mut clock = FxInstanceClock::new(1, flags, 4.0, 135_000.0).unwrap();
            assert_eq!(clock.phase(141_000.0, false, None), Some(expected));
        }
    }

    #[test]
    fn caller_mode_and_deferred_restart_use_the_instance_origin() {
        let mut clock = FxInstanceClock::new(2, 0, 4.0, 135_000.0).unwrap();
        clock.set_time(136_000.0, 3).unwrap();
        clock.request_restart();
        clock.update(139_999.0).unwrap();
        assert!(clock.phase(139_999.0, false, None).unwrap() < 0.001);
        clock.update(140_250.0).unwrap();
        assert_eq!(clock.phase(140_250.0, false, None), Some(0.0));
        assert_eq!(clock.elapsed_seconds(140_750.0), Some(0.5));
        assert_eq!(clock.phase(141_250.0, false, None), Some(0.25));
    }

    #[test]
    fn manager_header_gates_precede_phase_transforms() {
        for (flags, manager, expected) in [
            (0x80, false, 0.0),
            (0x80, true, 0.5),
            (0x100, false, 0.5),
            (0x100, true, 0.0),
        ] {
            let mut clock = FxInstanceClock::new(1, flags, 2.0, 20_000.0).unwrap();
            assert_eq!(clock.phase(21_000.0, manager, None), Some(expected));
        }
        assert_eq!(
            FxInstanceClock::new(0, 0, 0.0, 0.0)
                .unwrap()
                .phase(100.0, false, None),
            Some(0.0)
        );
        assert!(FxInstanceClock::new(1, 0, 0.0, 0.0).is_none());
        assert!(FxInstanceClock::new(0, 0, f32::MAX, 0.0).is_none());
        assert!(FxInstanceClock::new(18, 0, 2.0, 0.0).is_none());
    }
}
