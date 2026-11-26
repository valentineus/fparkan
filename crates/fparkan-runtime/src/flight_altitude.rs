//! A terrain clearance policy for the shared route follower.

use crate::EngineError;

#[derive(Clone, Copy)]
pub(super) struct FlightClearance {
    pub(super) body_radius: f32,
}

impl FlightClearance {
    pub(super) fn raised(self, altitude: f32, ground: f32, speed: f32, seconds: f32) -> (f32, f32) {
        // Keep approximately one radius of air under the hull, even when a
        // parked actor starts on the surface. No descent is requested here.
        let target = ground + 2.0 * self.body_radius;
        if target <= altitude || speed <= 0.0 {
            return (altitude, seconds);
        }
        let rise_time = (target - altitude) / speed;
        if rise_time >= seconds {
            ((altitude + speed * seconds).min(target), 0.0)
        } else {
            (target, seconds - rise_time)
        }
    }

    pub(super) fn corridor_floor(
        self,
        from: [f32; 2],
        to: [f32; 2],
        endpoint_floor: f32,
        height_at: &mut impl FnMut([f32; 2]) -> Result<Option<f32>, EngineError>,
    ) -> Result<f32, EngineError> {
        let [dx, dy] = [to[0] - from[0], to[1] - from[1]];
        let samples = (dx.hypot(dy) / self.body_radius).ceil() as usize;
        let samples = samples.clamp(2, 256);
        let mut highest = endpoint_floor;
        for index in 1..samples {
            let t = index as f32 / samples as f32;
            if let Some(ground) = height_at([from[0] + dx * t, from[1] + dy * t])? {
                highest = highest.max(ground);
            }
        }
        Ok(highest)
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        create, frame, load_gameplay_mission, EngineServices, MissionMode, MissionRequest,
        MissionStartupInput,
    };
    use fparkan_terrain::SurfaceQuery;
    use fparkan_vfs::DirectoryVfs;
    use fparkan_world::{handle_by_original_id, transform_state, OriginalObjectId};
    use std::sync::Arc;

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_stock_autodemo_flyer_takes_off_and_moves_above_the_surface() {
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
        let id = OriginalObjectId(2);
        assert_eq!(
            engine
                .loaded
                .as_ref()
                .unwrap()
                .unit_chassis_flags
                .get(&id.0)
                .and_then(|flags| flags.can_fly),
            Some(true)
        );
        let handle = handle_by_original_id(&engine.world, id).unwrap();
        let [a, b, initial_z] = transform_state(&engine.world, handle)
            .unwrap()
            .position
            .map(f32::from_bits);
        for now in (0..=5_000).step_by(50) {
            frame(&mut engine, now).unwrap();
            let [x, y, z] = transform_state(&engine.world, handle)
                .unwrap()
                .position
                .map(f32::from_bits);
            if (x - a).hypot(y - b) > 1.0 {
                let floor = engine
                    .loaded
                    .as_ref()
                    .unwrap()
                    .terrain
                    .height_at([x, y])
                    .unwrap()
                    .unwrap();
                assert!(z > initial_z + 1.0 && z > floor + 1.0);
                return;
            }
        }
        panic!("stock flyer did not move after takeoff");
    }
}
