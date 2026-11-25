use crate::{
    control_profile, control_resource_budget_pass, ControlProfileReport, LoadedMissionState,
};
use fparkan_world::{transform_state, ObjectHandle, World};
use std::collections::BTreeMap;

/// Factory and laboratory Control budgets; other owners and global RNG ordering remain open.
pub(super) struct Resources {
    clocks: Vec<(ObjectHandle, ResourceClock)>,
    rng: u32,
    ticks: BTreeMap<u32, u64>,
    stops: BTreeMap<u32, (u64, String)>,
    pub(super) profiles: ControlProfileReport,
}

impl Default for Resources {
    fn default() -> Self {
        Self {
            clocks: Vec::new(),
            // Explicit bootstrap seed until Control initialization and other owners are wired.
            rng: 1,
            ticks: BTreeMap::new(),
            stops: BTreeMap::new(),
            profiles: ControlProfileReport::default(),
        }
    }
}

impl Resources {
    pub(super) fn ticks(&self) -> impl Iterator<Item = (u32, u64)> + '_ {
        self.ticks.iter().map(|(&unit, &count)| (unit, count))
    }

    pub(super) fn stops(&self) -> impl Iterator<Item = (u32, u64, &str)> + '_ {
        self.stops
            .iter()
            .map(|(&unit, (count, reason))| (unit, *count, reason.as_str()))
    }
}

pub(super) fn tick(mission: &mut LoadedMissionState, world: &World, now_ms: u32) {
    let state = &mut mission.control_resources;
    state
        .clocks
        .retain(|(handle, _)| transform_state(world, *handle).is_ok());
    for table in &mut mission.control_device_tables {
        let flags = table
            .original_id
            .and_then(|id| mission.object_drafts.get(id.0 as usize))
            .and_then(|draft| draft.unit_dat_header_flags);
        let budget_owner = matches!(
            flags,
            Some(crate::autodemo::PLANT_CLASS | crate::laboratory::LAB_CLASS)
        );
        let mobile_owner = flags.is_some_and(|flags| flags & crate::autodemo::MOBILE_UNIT != 0);
        if (!budget_owner && !mobile_owner) || transform_state(world, table.handle).is_err() {
            continue;
        }
        let clock = if let Some(index) = state
            .clocks
            .iter()
            .position(|(handle, _)| *handle == table.handle)
        {
            &mut state.clocks[index].1
        } else {
            state.clocks.push((
                table.handle,
                ResourceClock {
                    frame_ms: now_ms as f32,
                    resource_ms: now_ms as f32,
                    forced: 0,
                },
            ));
            &mut state
                .clocks
                .last_mut()
                .expect("new Control resource clock")
                .1
        };
        // Control.dll+0x42480: default resource period; this is a separate RNG from Behavior.
        let Some(seconds) = clock.step(now_ms as f32, true, 250.0, &mut state.rng) else {
            continue;
        };
        let object = table.original_id.map_or(table.handle.slot, |id| id.0);
        let ticks = state.profiles.ticks.entry(object).or_default();
        *ticks += 1;
        if *ticks == 1 {
            eprintln!("control_profile_clock_approx object={object} (Control RNG seed, owner order and activation remain open)");
        }
        let before = table
            .owner_profile
            .as_ref()
            .map_or(0, |profile| profile.current_words[7]);
        match control_profile::refresh(table) {
            Ok(true) => {
                let after = table
                    .owner_profile
                    .as_ref()
                    .expect("refreshed profile")
                    .current_words[7];
                let update = state.profiles.updates.entry(object).or_default();
                *update = (update.0 + 1, before, after);
                if before != after {
                    eprintln!(
                        "control_profile_updated object={object} forward_limit={} -> {}",
                        f32::from_bits(before),
                        f32::from_bits(after)
                    );
                }
            }
            Ok(false) => {}
            Err(reason) => {
                let count = state
                    .profiles
                    .stubs
                    .entry((object, reason.clone()))
                    .or_default();
                *count += 1;
                if *count == 1 {
                    eprintln!("control_profile_update_stub object={object} reason={reason}");
                }
            }
        }
        if !budget_owner {
            continue;
        }
        match control_resource_budget_pass(table, seconds) {
            Ok(()) => {
                let count = state.ticks.entry(table.handle.slot).or_default();
                *count += 1;
                if *count == 1 {
                    eprintln!("sim_coverage control_resource_clock_approx=1 unit={} (factory and laboratory owners; Control RNG seed and global order remain open)", table.handle.slot);
                }
            }
            Err(reason) => {
                let stop = state
                    .stops
                    .entry(table.handle.slot)
                    .or_insert_with(|| (0, format!("{reason:?}")));
                stop.0 += 1;
                if stop.0 == 1 {
                    eprintln!(
                        "sim_coverage control_resource_budget_stub=1 unit={} reason={}",
                        table.handle.slot, stop.1
                    );
                }
            }
        }
    }
}

#[derive(Default)]
struct ResourceClock {
    frame_ms: f32,
    resource_ms: f32,
    forced: u32,
}

impl ResourceClock {
    fn step(&mut self, now_ms: f32, active: bool, period_ms: f32, rng: &mut u32) -> Option<f32> {
        if now_ms - self.frame_ms < 1.0 {
            return None;
        }
        self.frame_ms = now_ms;
        if !active {
            self.resource_ms = now_ms;
            return None;
        }
        let low = (*rng as u16).wrapping_shl(1) ^ (*rng >> 16) as u16;
        let high = ((*rng >> 16) as u16 >> 1) ^ low;
        *rng = (u32::from(high) << 16) | u32::from(low);
        let quarter = period_ms * 0.25;
        let random_scale = quarter / 65536.0;
        let deadline =
            ((f32::from(high) * random_scale - quarter * 0.5) + period_ms) + self.resource_ms;
        if self.forced == 0 && now_ms <= deadline {
            return None;
        }
        self.forced = self.forced.saturating_sub(1);
        let seconds = (now_ms - self.resource_ms) * 0.001;
        self.resource_ms = now_ms;
        Some(seconds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_control_redraws_the_deadline_on_each_accepted_frame() {
        let mut clock = ResourceClock::default();
        let mut rng = 0x1234_5678;
        assert_eq!(clock.step(0.5, true, 500.0, &mut rng), None);
        assert_eq!((clock.frame_ms, rng), (0.0, 0x1234_5678));
        for (now, random, last, due) in [
            (500.0, 0xb7de_bec4, 0.0, None),
            (501.0, 0x91b9_ca56, 0.0, None),
            (502.0, 0x4dc9_0515, 502.0, Some(0.50200003)),
            (503.0, 0x6107_47e3, 502.0, None),
        ] {
            assert_eq!(clock.step(now, true, 500.0, &mut rng), due);
            assert_eq!(
                (clock.frame_ms, clock.resource_ms, rng),
                (now, last, random)
            );
        }
    }

    #[test]
    fn native_control_equality_forced_step_and_inactive_baseline() {
        let mut clock = ResourceClock::default();
        let mut rng = 0xffff_0000;
        assert_eq!(clock.step(500.0, true, 500.0, &mut rng), None);
        assert_eq!((clock.resource_ms, rng), (0.0, 0x8000_ffff));
        assert_eq!(clock.step(501.0, true, 500.0, &mut rng), Some(0.50100005));
        assert_eq!((clock.resource_ms, rng), (501.0, 0x3ffe_7ffe));
        clock.forced = 2;
        assert_eq!(clock.step(502.0, true, 500.0, &mut rng), Some(0.001));
        assert_eq!((clock.forced, rng), (1, 0xdffd_c002));
        assert_eq!(clock.step(503.0, false, 500.0, &mut rng), None);
        assert_eq!(
            (clock.resource_ms, clock.forced, rng),
            (503.0, 1, 0xdffd_c002)
        );
    }
}
