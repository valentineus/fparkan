//! Translational inertia for a boarded aircraft.

use crate::{EngineError, TerrainWorld};
use fparkan_terrain::SurfaceQuery;

#[derive(Clone, Copy)]
pub(super) struct FlightProfile {
    pub(super) acceleration: [f32; 3],
    pub(super) limits: [f32; 3],
}

impl FlightProfile {
    pub(super) fn from_control(words: &[u32; 27]) -> Option<Self> {
        let acceleration = [0, 1, 2].map(|word| f32::from_bits(words[word]));
        let limits = [6, 7, 8].map(|word| f32::from_bits(words[word]));
        acceleration
            .into_iter()
            .chain(limits)
            .all(|rate| rate.is_finite() && rate >= 0.0)
            .then_some(Self {
                acceleration,
                limits,
            })
    }

    // shortcut: retain the previous speed limits when the Control profile is unavailable.
    pub(super) const FALLBACK: Self = Self {
        acceleration: [40.0, 40.0, 16.0],
        limits: [40.0, 40.0, 16.0],
    };
}

#[derive(Clone, Copy)]
pub(super) struct FlightDrive {
    pub(super) profile: FlightProfile,
    pub(super) velocity: [f32; 3],
}

impl FlightDrive {
    pub(super) fn step(
        self,
        terrain: &TerrainWorld,
        from: [f32; 3],
        yaw: f32,
        axes: [f32; 3],
        seconds: f32,
    ) -> Result<([f32; 3], [f32; 3]), EngineError> {
        let [(vx, across), (vy, along), (vz, climb)] = std::array::from_fn(|axis| {
            let target = axes[axis].clamp(-1.0, 1.0) * self.profile.limits[axis];
            ramp_axis(
                self.velocity[axis],
                target,
                self.profile.acceleration[axis],
                seconds,
            )
        });
        if [across, along, climb] == [0.0; 3] {
            return Ok((from, [vx, vy, vz]));
        }
        let (sin, cos) = yaw.sin_cos();
        let old_xy = [from[0], from[1]];
        let xy = [
            from[0] + cos * across - sin * along,
            from[1] + sin * across + cos * along,
        ];
        let altitude = from[2] + climb;
        if !xy.iter().all(|axis| axis.is_finite()) || !altitude.is_finite() {
            return Err(EngineError::Movement(
                "player step exceeds finite coordinates".into(),
            ));
        }
        let floor = |xy| terrain.height_at(xy).map_err(EngineError::Terrain);
        let Some(bottom) = floor(old_xy)?.map(|height| height + 0.02) else {
            return Ok((from, [0.0; 3]));
        };
        // A grounded aircraft can taxi; airborne travel must clear the terrain.
        let grounded = from[2] <= bottom && climb <= 0.0;
        let (accepted_xy, height, horizontal) = match floor(xy)?.map(|height| height + 0.02) {
            Some(floor) if grounded || floor <= altitude.max(from[2]) => (xy, floor, [vx, vy]),
            _ => (old_xy, bottom, [0.0; 2]),
        };
        let [x, y] = accepted_xy;
        let vz = if altitude <= height { vz.max(0.0) } else { vz };
        Ok((
            [x, y, altitude.max(height)],
            [horizontal[0], horizontal[1], vz],
        ))
    }
}

pub(super) fn ramp_axis(velocity: f32, target: f32, acceleration: f32, seconds: f32) -> (f32, f32) {
    if acceleration == 0.0 || velocity == target {
        return (velocity, velocity * seconds);
    }
    let difference = target - velocity;
    let arrival_time = difference.abs() / acceleration;
    let ramp_time = seconds.min(arrival_time);
    let next = if seconds >= arrival_time {
        target
    } else {
        velocity + acceleration.copysign(difference) * ramp_time
    };
    let distance = (velocity + next) * 0.5 * ramp_time + next * (seconds - ramp_time);
    (next, distance)
}

#[cfg(test)]
mod tests {
    use super::ramp_axis;

    #[test]
    fn control_ramp_matches_stock_helicopter_velocity_through_release_and_reversal() {
        // Control.dll RVA 0x153c0, R_T_02 active profile.
        let limit = f32::from_bits(1096943108);
        let mut velocity = 0.0;
        for (target, seconds, expected) in [
            (limit, 0.25, 10.0),
            (limit, 1.0, limit),
            (0.0, 0.25, limit - 10.0),
            (-limit, 0.25, limit - 20.0),
            (0.0, 2.0, 0.0),
        ] {
            (velocity, _) = ramp_axis(velocity, target, 40.0, seconds);
            assert!((velocity - expected).abs() < 0.000_001);
        }
    }

    #[test]
    fn coasting_ends_at_the_same_point_across_frame_boundaries() {
        assert_eq!(ramp_axis(15.0, 0.0, 30.0, 1.0), (0.0, 3.75));
        let (next, first) = ramp_axis(15.0, 0.0, 30.0, 0.25);
        let (rest, second) = ramp_axis(next, 0.0, 30.0, 0.75);
        assert_eq!(rest, 0.0);
        assert_eq!(first + second, 3.75);
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_teleport_helicopter_climbs_and_keeps_altitude() {
        use crate::{
            board_friendly_warbot, create, frame_with_player_motion, load_gameplay_mission,
            loaded_terrain, mission_object_handle, plan_player_motion, select_boarding_target,
            starting_cockpit, Engine, EngineServices, MissionControlDeviceTableStatus,
            MissionControlNodeProviderTableStatus, MissionMode, MissionRequest,
            MissionStartupInput, PlayerMotion, StartingCockpit,
        };
        use fparkan_terrain::SurfaceQuery;
        use fparkan_vfs::DirectoryVfs;
        use fparkan_world::{set_transform, transform_state, OriginalObjectId};
        use std::sync::Arc;

        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed root");
        let vfs = Arc::new(DirectoryVfs::new(std::path::PathBuf::from(root)));
        let mut engine = create(EngineServices::new(vfs)).unwrap();
        load_gameplay_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/CAMPAIGN/CAMPAIGN.00/Mission.04/data.tma".into(),
                mode: MissionMode::SingleMission,
            },
            MissionStartupInput {
                queue_time_ms: 0,
                research_creation_gate: false,
            },
            None,
        )
        .unwrap();
        let hero = starting_cockpit(&engine).unwrap();
        let up = PlayerMotion {
            climb: 1.0,
            delta_seconds: 1.0,
            ..PlayerMotion::default()
        };
        assert!(plan_player_motion(&engine, hero, up).unwrap().is_none());

        let helicopter = mission_object_handle(&engine, OriginalObjectId(2)).unwrap();
        let position = |engine: &Engine, cockpit: StartingCockpit| {
            transform_state(&engine.world, cockpit.handle)
                .unwrap()
                .position
                .map(f32::from_bits)
        };
        let mut pose = transform_state(&engine.world, hero.handle).unwrap();
        let [hx, hy, _] = pose.position.map(f32::from_bits);
        let [x, y, _] = transform_state(&engine.world, helicopter)
            .unwrap()
            .position
            .map(f32::from_bits);
        let yaw = (y - hy).atan2(x - hx) - std::f32::consts::FRAC_PI_2;
        pose.orientation[2] = yaw.to_bits();
        set_transform(&mut engine.world, hero.handle, pose).unwrap();
        let walk = PlayerMotion {
            movement: [0.0, 1.0],
            delta_seconds: 0.1,
            ..PlayerMotion::default()
        };
        for tick in 1..=26 {
            let step = plan_player_motion(&engine, hero, walk).unwrap().unwrap();
            frame_with_player_motion(&mut engine, tick * 100, hero, step, false).unwrap();
        }
        let table = engine
            .loaded
            .as_mut()
            .unwrap()
            .control_device_tables
            .iter_mut()
            .find(|table| table.handle == helicopter)
            .unwrap();
        let MissionControlDeviceTableStatus::Available(devices) = &mut table.status else {
            panic!("helicopter Control devices unavailable");
        };
        let pilot = devices
            .iter_mut()
            .find(|device| device.descriptor.tag == 1)
            .unwrap();
        let key = pilot.key as usize;
        let MissionControlNodeProviderTableStatus::Available(nodes) =
            &mut table.node_provider_status
        else {
            panic!("helicopter Control nodes unavailable");
        };
        nodes[key].current_type1_value_bits = 0.0_f32.to_bits();
        assert_eq!(
            board_friendly_warbot(&mut engine, Some(helicopter)).unwrap(),
            None
        );
        let table = engine
            .loaded
            .as_mut()
            .unwrap()
            .control_device_tables
            .iter_mut()
            .find(|table| table.handle == helicopter)
            .unwrap();
        let MissionControlNodeProviderTableStatus::Available(nodes) =
            &mut table.node_provider_status
        else {
            panic!("helicopter Control nodes unavailable");
        };
        nodes[key].current_type1_value_bits = 1.0_f32.to_bits();
        assert_eq!(board_friendly_warbot(&mut engine, None).unwrap(), None);
        let [hx, hy, hz] = position(&engine, hero);
        let [tx, ty, tz] = transform_state(&engine.world, helicopter)
            .unwrap()
            .position
            .map(f32::from_bits);
        assert_eq!(
            select_boarding_target(&engine, hero, [[hx, hy, hz + 1.0], [tx, ty, tz + 0.5]]),
            Some(helicopter),
        );
        let vehicle = board_friendly_warbot(&mut engine, Some(helicopter))
            .unwrap()
            .unwrap();
        assert_eq!(vehicle.handle, helicopter);
        let [x, y, z] = position(&engine, vehicle);
        let step = plan_player_motion(&engine, vehicle, up).unwrap().unwrap();
        assert!(step.sweep.is_some());
        assert!(step.floor_probe().is_none());
        frame_with_player_motion(&mut engine, 5_500, vehicle, step, false).unwrap();
        let [_, _, climb_z] = position(&engine, vehicle);
        assert!((8.0..16.0).contains(&(climb_z - z)));
        assert_eq!(position(&engine, hero), [x, y, climb_z]);

        let coast = PlayerMotion {
            delta_seconds: 1.0,
            ..Default::default()
        };
        let step = plan_player_motion(&engine, vehicle, coast)
            .unwrap()
            .unwrap();
        frame_with_player_motion(&mut engine, 6_500, vehicle, step, false).unwrap();
        let [_, _, hover_z] = position(&engine, vehicle);
        assert!(hover_z > climb_z);
        assert!(plan_player_motion(&engine, vehicle, coast)
            .unwrap()
            .is_none());

        let forward = PlayerMotion {
            climb: 0.0,
            movement: [0.0, 1.0],
            delta_seconds: 0.25,
            ..up
        };
        let step = plan_player_motion(&engine, vehicle, forward)
            .unwrap()
            .unwrap();
        frame_with_player_motion(&mut engine, 6_750, vehicle, step, false).unwrap();
        let [fx, fy, fz] = position(&engine, vehicle);
        assert!(((fx - x).hypot(fy - y) - 1.25).abs() < 0.0001);
        assert_eq!(fz, hover_z);

        let down = PlayerMotion {
            climb: -1.0,
            delta_seconds: 2.0,
            ..up
        };
        let step = plan_player_motion(&engine, vehicle, down).unwrap().unwrap();
        frame_with_player_motion(&mut engine, 8_750, vehicle, step, false).unwrap();
        let [lx, ly, lz] = position(&engine, vehicle);
        assert!(((lx - fx).hypot(ly - fy) - 1.25).abs() < 0.0001);
        let ground = loaded_terrain(&engine)
            .unwrap()
            .height_at([lx, ly])
            .unwrap()
            .unwrap();
        assert_eq!(lz, ground + 0.02);
        assert!(plan_player_motion(&engine, vehicle, coast)
            .unwrap()
            .is_none());
    }
}
