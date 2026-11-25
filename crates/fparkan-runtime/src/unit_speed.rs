//! Requested speeds supplied to the movement controller by Behavior orders.

use crate::{BehaviorProfileState, MissionControlDeviceTableState};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum UnitSpeedKind {
    PatrolPlace,
    Go,
}

impl std::fmt::Display for UnitSpeedKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::PatrolPlace => "patrol",
            Self::Go => "go",
        })
    }
}

/// Requested speeds and unresolved inputs, grouped by original object ID and order.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UnitSpeedReport {
    /// Key -> (samples, last Control forward limit, coefficient, requested speed), in raw bits.
    pub samples: BTreeMap<(u32, UnitSpeedKind), (u64, u32, u32, u32)>,
    pub stubs: BTreeMap<(u32, UnitSpeedKind, &'static str), u64>,
}

fn requested_speed(current_words: &[u32; 27], percent: f32) -> Option<f32> {
    let limit = f32::from_bits(current_words[7]);
    let speed = limit * percent;
    (limit.is_finite()
        && limit >= 0.0
        && percent.is_finite()
        && percent >= 0.0
        && speed.is_finite())
    .then_some(speed)
}

pub(super) fn order_speed(
    table: Option<&MissionControlDeviceTableState>,
    behavior: Option<&BehaviorProfileState>,
    object: u32,
    kind: UnitSpeedKind,
    report: &mut UnitSpeedReport,
) -> f32 {
    let sample = || {
        let percent = match kind {
            UnitSpeedKind::PatrolPlace => 0.8,
            UnitSpeedKind::Go => {
                f32::from_bits(behavior.ok_or("behavior_profile")?.go_speed_percent_bits)
            }
        };
        let profile = table
            .and_then(|table| table.owner_profile.as_ref())
            .ok_or("owner_profile")?;
        profile
            .fca0_projection
            .as_ref()
            .map_err(|_| "profile_fca0")?;
        let speed = requested_speed(&profile.current_words, percent).ok_or("speed_inputs")?;
        Ok((profile.current_words[7], percent.to_bits(), speed))
    };
    match sample() {
        Ok((limit, percent, speed)) => {
            let sample = report.samples.entry((object, kind)).or_default();
            if sample.0 == 0 {
                eprintln!("{kind}_speed_profile_approx object={object} limit={} percent={} requested={speed} (motion controller and owner activation remain open)", f32::from_bits(limit), f32::from_bits(percent));
            }
            *sample = (sample.0 + 1, limit, percent, speed.to_bits());
            speed
        }
        Err(reason) => {
            let count = report.stubs.entry((object, kind, reason)).or_default();
            if *count == 0 {
                eprintln!("{kind}_speed_stub object={object} reason={reason}");
            }
            *count += 1;
            // shortcut: retain the old speed only for an unresolved profile.
            9.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_place_request_scales_the_current_forward_limit() {
        // Live GOG Behavior.dll, RVA 0x2dc57..0x2dc67; word 7 read by RVA 0x1bbe0.
        let mut words = [0; 27];
        words[7] = 0x416b55d8;
        assert_eq!(requested_speed(&words, 0.8).unwrap().to_bits(), 0x413c44ad);
        words[7] = 20.0_f32.to_bits();
        assert_eq!(requested_speed(&words, 0.8), Some(16.0));
        words[7] = 0;
        assert_eq!(requested_speed(&words, 0.8), Some(0.0));
        words[7] = (-1.0_f32).to_bits();
        assert_eq!(requested_speed(&words, 0.8), None);
    }

    #[test]
    fn an_unresolved_profile_keeps_the_explicit_fallback_and_counts_each_sample() {
        let mut report = UnitSpeedReport::default();
        assert_eq!(
            order_speed(None, None, 42, UnitSpeedKind::PatrolPlace, &mut report),
            9.0
        );
        assert_eq!(
            order_speed(None, None, 42, UnitSpeedKind::PatrolPlace, &mut report),
            9.0
        );
        assert_eq!(
            report
                .stubs
                .get(&(42, UnitSpeedKind::PatrolPlace, "owner_profile")),
            Some(&2)
        );
        assert!(report.samples.is_empty());
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_stock_patrol_reads_each_units_current_control_limit() {
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
                key: "MISSIONS/Autodemo.00/data.tma".into(),
                mode: MissionMode::AutoDemo,
            },
            MissionStartupInput {
                queue_time_ms: 0,
                research_creation_gate: false,
            },
            None,
        )
        .unwrap();
        frame(&mut engine, 0).unwrap();
        for now in (50..=5_000).step_by(50) {
            frame(&mut engine, now).unwrap();
        }
        let mission = engine.loaded.as_ref().unwrap();
        let report = &mission.unit_patrol.as_ref().unwrap().speed_report;
        assert!(report.stubs.is_empty(), "{:?}", report.stubs);
        assert!(report.samples.len() >= 2, "{:?}", report.samples);
        for (&(object, kind), &(samples, limit, percent, speed)) in &report.samples {
            assert_eq!(kind, UnitSpeedKind::PatrolPlace);
            assert_eq!(percent, 0.8_f32.to_bits());
            let profile = mission
                .control_device_tables
                .iter()
                .find(|table| table.original_id.is_some_and(|id| id.0 == object))
                .unwrap()
                .owner_profile
                .as_ref()
                .unwrap();
            assert!(samples > 0);
            assert_eq!(limit, profile.current_words[7]);
            assert_eq!(speed, (f32::from_bits(limit) * 0.8).to_bits());
            assert!(f32::from_bits(speed) > 0.0, "stationary patrol {object}");
        }
    }
    #[test]
    fn native_go_request_uses_current_limit_and_registered_percent() {
        let mut words = [0; 27];
        words[7] = 20.0_f32.to_bits();
        assert_eq!(requested_speed(&words, 1.0), Some(20.0));
        assert_eq!(requested_speed(&words, 0.0), Some(0.0));
        words[7] = 0x416b55d8;
        assert_eq!(requested_speed(&words, 0.5).unwrap().to_bits(), 0x40eb55d8);
        words[7] = 0;
        assert_eq!(requested_speed(&words, 1.0), Some(0.0));
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_stock_raid_go_requests_its_current_control_limit() {
        use crate::{
            create, frame, load_gameplay_mission, EngineServices, MissionMode, MissionRequest,
            MissionStartupInput,
        };
        use fparkan_vfs::DirectoryVfs;
        use std::sync::Arc;
        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed corpus");
        let mut engine = create(EngineServices::new(Arc::new(DirectoryVfs::new(root)))).unwrap();
        load_gameplay_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/CAMPAIGN/CAMPAIGN.04/Mission.02/data.tma".into(),
                mode: MissionMode::SingleMission,
            },
            MissionStartupInput {
                queue_time_ms: 0,
                research_creation_gate: false,
            },
            None,
        )
        .unwrap();
        for now in (0..=400_000).step_by(50) {
            frame(&mut engine, now).unwrap();
            let loaded = engine.loaded.as_ref().unwrap();
            let Some(patrol) = loaded.unit_patrol.as_ref() else {
                continue;
            };
            let Some((&(object, _), &(count, limit, percent, speed))) = patrol
                .speed_report
                .samples
                .iter()
                .find(|((_, kind), _)| *kind == UnitSpeedKind::Go)
            else {
                continue;
            };
            assert!(patrol.speed_report.stubs.is_empty());
            assert!(count > 0);
            assert_eq!(
                percent,
                1.0_f32.to_bits(),
                "stock profiles keep the native Go default"
            );
            assert_eq!(speed, limit);
            assert_ne!(speed, 9.0_f32.to_bits());
            let profile = loaded
                .control_device_tables
                .iter()
                .find(|table| table.original_id.is_some_and(|id| id.0 == object))
                .unwrap()
                .owner_profile
                .as_ref()
                .unwrap();
            assert_eq!(profile.current_words[7], limit);
            eprintln!(
                "stock Go object={object} at {now}: limit={} request={}",
                f32::from_bits(limit),
                f32::from_bits(speed)
            );
            return;
        }
        panic!("stock scripted Go never requested its Control speed");
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_produced_unit_rebinds_its_behavior_speed_profile() {
        use crate::{
            create, frame, load_gameplay_mission, EngineServices, MissionMode, MissionRequest,
            MissionStartupInput,
        };
        use fparkan_vfs::DirectoryVfs;
        use std::sync::Arc;
        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed corpus");
        let mut engine = create(EngineServices::new(Arc::new(DirectoryVfs::new(root)))).unwrap();
        load_gameplay_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/Autodemo.00/data.tma".into(),
                mode: MissionMode::AutoDemo,
            },
            MissionStartupInput {
                queue_time_ms: 0,
                research_creation_gate: false,
            },
            None,
        )
        .unwrap();
        for now in (0..=60_000).step_by(50) {
            let result = frame(&mut engine, now).unwrap();
            let loaded = engine.loaded.as_ref().unwrap();
            for born in result.autodemo_reinforcements {
                let original_id = loaded.object_drafts[born.object_index].original_id.unwrap();
                let behavior = loaded
                    .behavior_profiles
                    .iter()
                    .find(|profile| profile.original_id == original_id)
                    .unwrap();
                assert_eq!(behavior.handle, born.handle);
                let table = loaded
                    .control_device_tables
                    .iter()
                    .find(|table| table.handle == born.handle)
                    .unwrap();
                let mut report = UnitSpeedReport::default();
                let speed = order_speed(
                    Some(table),
                    Some(behavior),
                    original_id.0,
                    UnitSpeedKind::Go,
                    &mut report,
                );
                assert!(report.stubs.is_empty());
                assert_eq!(
                    speed.to_bits(),
                    table.owner_profile.as_ref().unwrap().current_words[7]
                );
                return;
            }
        }
        panic!("stock factory did not produce a replacement");
    }
}
