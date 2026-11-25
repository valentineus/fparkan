//! Behavior's cyclic PatrolPlace point list and its refresh timer.

use crate::{behavior_clock::next_deadline, next_crt_rand, EngineError};
use std::collections::BTreeMap;

/// PatrolPlace batches and unresolved inputs, retained after units disappear.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PatrolPointReport {
    /// Object ID -> (batches, generated goals, approximate ground-area checks).
    pub batches: BTreeMap<u32, (u64, u64, u64)>,
    /// Object ID and missing input -> affected refreshes.
    pub stubs: BTreeMap<(u32, &'static str), u64>,
}

#[derive(Clone, Default)]
pub(super) struct PatrolPoints {
    points: Vec<[f32; 2]>,
    index: usize,
    deadline_ms: u32,
}

impl PatrolPoints {
    pub(super) fn current(&self) -> Option<[f32; 2]> {
        self.points.get(self.index).copied()
    }

    pub(super) fn advance(&mut self) {
        if !self.points.is_empty() {
            self.index = (self.index + 1) % self.points.len();
        }
    }

    pub(super) fn len(&self) -> usize {
        self.points.len()
    }

    pub(super) fn refresh(
        &mut self,
        center: [f32; 2],
        radius: f32,
        bounds: [[f32; 3]; 2],
        now_ms: u32,
        crt: &mut u32,
        count_rng: &mut u32,
        mut allowed: impl FnMut([f32; 2]) -> Result<bool, EngineError>,
    ) -> Result<bool, EngineError> {
        if !self.points.is_empty() && now_ms <= self.deadline_ms {
            return Ok(false);
        }
        let low = (*count_rng as u16).wrapping_mul(2) ^ (*count_rng >> 16) as u16;
        let high = ((*count_rng >> 16) as u16 >> 1) ^ low;
        *count_rng = u32::from(low) | (u32::from(high) << 16);
        let count = usize::from(high % 5 + 15);
        let mut points = Vec::with_capacity(count);
        for _ in 0..count {
            let mut point = center;
            // Native stores the last candidate even if all 350 checks fail.
            for _ in 0..350 {
                point = center.map(|axis| {
                    let random = next_crt_rand(crt) as f32 * (1.0 / 32767.0);
                    (random + random - 1.0) * radius + axis
                });
                if (0..2).all(|axis| {
                    bounds[0][axis] + 100.0 <= point[axis] && point[axis] <= bounds[1][axis] - 100.0
                }) && allowed(point)?
                {
                    break;
                }
            }
            points.push(point);
        }
        self.points = points;
        self.index = 0;
        // Default PatrolPlace 20 s + 10 s jitter, quantized by Behavior's timer.
        self.deadline_ms = next_deadline(now_ms, 312, 156, crt);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_place_batch_consumes_both_random_streams_and_schedules_its_timer() {
        // GOG Behavior.dll RVA 0x2dd90, x87 CW 0x007f.
        let mut points = PatrolPoints::default();
        let (mut crt, mut count_rng) = (1, 1);
        assert!(points
            .refresh(
                [500.0, 752.0],
                150.0,
                [[0.0; 3], [2000.0, 2000.0, 100.0]],
                1000,
                &mut crt,
                &mut count_rng,
                |_| Ok(true)
            )
            .unwrap());
        let expected = [
            [0x43af300c, 0x4440c4d7],
            [0x43cbfee2, 0x445327d1],
            [0x4403602e, 0x443a7d90],
            [0x43e38b31, 0x4459b27a],
            [0x44153687, 0x444e7ed0],
            [0x43c91dc0, 0x4456ebb7],
            [0x440cc9a1, 0x443d03df],
            [0x43dc9967, 0x44179fb4],
            [0x43bcb5e0, 0x4431d57b],
            [0x43c518d0, 0x4422f141],
            [0x4421a3ae, 0x4437ed4b],
            [0x43c0dccc, 0x4416d9a7],
            [0x43b05632, 0x4432d74d],
            [0x43febfdb, 0x444156be],
            [0x4404a1de, 0x44440995],
            [0x43c7ef66, 0x44483a77],
            [0x43f29e4b, 0x4430e8c1],
        ];
        assert_eq!(
            points
                .points
                .iter()
                .map(|p| p.map(f32::to_bits))
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(
            (crt, count_rng, points.deadline_ms),
            (0x074d495a, 0x00020002, 23971)
        );
        let first = points.current();
        for _ in 0..17 {
            points.advance();
        }
        assert_eq!(
            points.current(),
            first,
            "list is cyclic until its timer expires"
        );
        assert!(!points
            .refresh(
                [500.0, 752.0],
                150.0,
                [[0.0; 3], [2000.0, 2000.0, 100.0]],
                23971,
                &mut crt,
                &mut count_rng,
                |_| panic!("equality is not due")
            )
            .unwrap());
        assert_eq!((crt, count_rng), (0x074d495a, 0x00020002));
        assert!(points
            .refresh(
                [500.0, 752.0],
                150.0,
                [[0.0; 3], [2000.0, 2000.0, 100.0]],
                23972,
                &mut crt,
                &mut count_rng,
                |_| Ok(true)
            )
            .unwrap());
        assert_eq!(
            (crt, count_rng, points.deadline_ms),
            (0xd8782d77, 0x00070006, 48620)
        );
        assert_eq!(
            points.current().unwrap().map(f32::to_bits),
            [0x4405138b, 0x44513fb8]
        );
    }

    #[test]
    fn live_autodemo_batch_matches_captured_rng_state_and_first_points() {
        let mut points = PatrolPoints::default();
        let (mut crt, mut count_rng) = (0xad599c29, 0x66883bac);
        let edge = f32::from_bits(0x4494d653);
        points
            .refresh(
                [500.0, 752.0],
                150.0,
                [[0.0; 3], [edge, edge, 100.0]],
                42767193,
                &mut crt,
                &mut count_rng,
                |_| Ok(true),
            )
            .unwrap();
        assert_eq!(points.len(), 17);
        assert_eq!(points.points[0].map(f32::to_bits), [0x43cd89d1, 0x4452e9b5]);
        assert_eq!(points.points[1].map(f32::to_bits), [0x44218d6a, 0x44489b26]);
        assert_eq!(
            (crt, count_rng, points.deadline_ms),
            (0x762b0522, 0x229411d0, 42788838)
        );
    }

    #[test]
    fn native_bounds_reject_candidates_in_the_outer_hundred_units() {
        let mut points = PatrolPoints::default();
        let (mut crt, mut count_rng) = (1, 1);
        points
            .refresh(
                [140.0, 140.0],
                150.0,
                [[0.0; 3], [2000.0, 2000.0, 100.0]],
                1000,
                &mut crt,
                &mut count_rng,
                |_| Ok(true),
            )
            .unwrap();
        assert_eq!(points.points[0].map(f32::to_bits), [0x432580b8, 0x4305f640]);
        assert_eq!(points.points[1].map(f32::to_bits), [0x436cda1e, 0x4355fb40]);
        assert_eq!(
            (crt, count_rng, points.deadline_ms),
            (0x5e14f28a, 0x00020002, 21748)
        );
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_stock_patrol_keeps_its_batch_before_the_native_refresh_deadline() {
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
        let report = &engine
            .loaded
            .as_ref()
            .unwrap()
            .unit_patrol
            .as_ref()
            .unwrap()
            .point_report;
        assert!(!report.batches.is_empty());
        assert!(report.stubs.is_empty(), "{:?}", report.stubs);
        for &(batches, points, _) in report.batches.values() {
            assert_eq!(batches, 1);
            assert!((15..=19).contains(&points));
        }
        let first = report.clone();
        for now in (50..=10_000).step_by(50) {
            frame(&mut engine, now).unwrap();
        }
        let report = &engine
            .loaded
            .as_ref()
            .unwrap()
            .unit_patrol
            .as_ref()
            .unwrap()
            .point_report;
        assert_eq!(
            report, &first,
            "moving toward a goal must not draw a fresh patrol list"
        );
    }
}
