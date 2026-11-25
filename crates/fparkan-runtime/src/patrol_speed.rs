//! PatrolPlace's requested speed from the mutable Control owner profile.

use crate::MissionControlDeviceTableState;
use std::collections::BTreeMap;

/// Autonomous place-patrol speed samples and unresolved profiles, by original ID.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PatrolSpeedReport {
    /// Object ID -> (samples, last Control forward limit bits, last requested speed bits).
    pub samples: BTreeMap<u32, (u64, u32, u32)>,
    /// Object ID and missing input -> affected movement samples.
    pub stubs: BTreeMap<(u32, &'static str), u64>,
}

fn requested_speed(current_words: &[u32; 27]) -> Option<f32> {
    let limit = f32::from_bits(current_words[7]);
    (limit.is_finite() && limit >= 0.0).then_some(limit * 0.8)
}

pub(super) fn place_speed(
    table: Option<&MissionControlDeviceTableState>,
    object: u32,
    report: &mut PatrolSpeedReport,
) -> f32 {
    let sample = || {
        let profile = table
            .and_then(|table| table.owner_profile.as_ref())
            .ok_or("owner_profile")?;
        profile
            .fca0_projection
            .as_ref()
            .map_err(|_| "profile_fca0")?;
        let speed = requested_speed(&profile.current_words).ok_or("forward_limit")?;
        Ok((profile.current_words[7], speed))
    };
    match sample() {
        Ok((limit, speed)) => {
            let sample = report.samples.entry(object).or_default();
            if sample.0 == 0 {
                eprintln!("patrol_speed_profile_approx object={object} limit={} requested={speed} (motion energy and owner activation remain open)", f32::from_bits(limit));
            }
            *sample = (sample.0 + 1, limit, speed.to_bits());
            speed
        }
        Err(reason) => {
            let count = report.stubs.entry((object, reason)).or_default();
            if *count == 0 {
                eprintln!("patrol_speed_stub object={object} reason={reason}");
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
        assert_eq!(requested_speed(&words).unwrap().to_bits(), 0x413c44ad);
        words[7] = 20.0_f32.to_bits();
        assert_eq!(requested_speed(&words), Some(16.0));
        words[7] = 0;
        assert_eq!(requested_speed(&words), Some(0.0));
        words[7] = (-1.0_f32).to_bits();
        assert_eq!(requested_speed(&words), None);
    }

    #[test]
    fn an_unresolved_profile_keeps_the_explicit_fallback_and_counts_each_sample() {
        let mut report = PatrolSpeedReport::default();
        assert_eq!(place_speed(None, 42, &mut report), 9.0);
        assert_eq!(place_speed(None, 42, &mut report), 9.0);
        assert_eq!(report.stubs.get(&(42, "owner_profile")), Some(&2));
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
        for (&object, &(samples, limit, speed)) in &report.samples {
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
}
