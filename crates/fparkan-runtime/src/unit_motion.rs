//! Observations of the temporary unit controller, not a native motion solver.

use crate::{CombatObject, Engine};
use std::collections::BTreeMap;

/// Work performed by the temporary controller for one retained object identity.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UnitMotionSample {
    pub updates: u64,
    pub first_ms: u32,
    pub last_ms: u32,
}

/// Counts persist after orders end or the object leaves the world.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UnitMotionReport {
    pub objects: BTreeMap<CombatObject, UnitMotionSample>,
}

impl UnitMotionReport {
    pub(super) fn update(&mut self, object: CombatObject, now: u32) {
        let sample = self.objects.entry(object).or_insert_with(|| {
            eprintln!("sim_coverage unit_motion_controller_stub=1 object={object} original_id={:?} first_ms={now}", object.original_id);
            UnitMotionSample {
                first_ms: now,
                ..Default::default()
            }
        });
        sample.updates += 1;
        sample.last_ms = now;
    }
}

/// Reads observations shared by the interactive and headless mission paths.
#[must_use]
pub fn loaded_mission_unit_motion_report(engine: &Engine) -> Option<&UnitMotionReport> {
    Some(&engine.loaded.as_ref()?.unit_patrol.as_ref()?.motion_report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_updates_separately_for_reused_slots() {
        let object = CombatObject {
            slot: 4,
            generation: 1,
            original_id: Some(17),
        };
        let replacement = CombatObject {
            generation: 2,
            ..object
        };
        let mut report = UnitMotionReport::default();
        report.update(object, 0);
        report.update(object, 500);
        report.update(replacement, 1_000);
        assert_eq!(
            report.objects[&object],
            UnitMotionSample {
                updates: 2,
                first_ms: 0,
                last_ms: 500,
            }
        );
        assert_eq!(report.objects[&replacement].updates, 1);
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_stock_patrol_reports_controller_updates() {
        use crate::{
            create, load_gameplay_mission, simulate_mission_report, EngineServices, MissionMode,
            MissionRequest, MissionStartupInput,
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
        let report = simulate_mission_report(&mut engine, 120_000).unwrap();
        assert_eq!(
            &report.unit_motion,
            loaded_mission_unit_motion_report(&engine).unwrap()
        );
        assert!(!report.unit_motion.objects.is_empty());
        assert!(report
            .unit_motion
            .objects
            .keys()
            .all(|object| object.original_id.is_some()));
        assert!(report
            .unit_motion
            .objects
            .values()
            .any(|sample| sample.updates > 1 && sample.last_ms > sample.first_ms));
    }
}
