//! Follow a terrain route with a shared time budget for turning and travel.

use crate::{
    route_obstacles::{open_leg, Footprint},
    EngineError, TerrainWorld, TransformState,
};
use fparkan_terrain::{NavigationQuery, RouteRequest};
use std::collections::VecDeque;

pub(super) fn route_points(
    terrain: &TerrainWorld,
    from: [f32; 3],
    goal: [f32; 3],
) -> Result<Option<VecDeque<[f32; 2]>>, EngineError> {
    let route = terrain
        .route(RouteRequest { start: from, goal })
        .map_err(EngineError::Terrain)?;
    Ok(route.and_then(|route| {
        let mut points = route
            .areas
            .windows(2)
            .map(|pair| terrain.route_crossing(pair[0], pair[1]))
            .collect::<Option<VecDeque<_>>>()?;
        points.push_back([goal[0], goal[1]]);
        Some(points)
    }))
}

pub(super) fn turn_toward(yaw: f32, [dx, dy]: [f32; 2], seconds: f32) -> (f32, f32) {
    let goal_yaw = (-dx).atan2(dy);
    let turn = (goal_yaw - yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
        - std::f32::consts::PI;
    let turn_budget = 2.0 * seconds;
    (
        yaw + turn.clamp(-turn_budget, turn_budget),
        seconds.min(turn.abs() * 0.5),
    )
}

pub(super) fn follow_route(
    mut pose: TransformState,
    points: &mut VecDeque<[f32; 2]>,
    speed: f32,
    mut seconds: f32,
    obstacles: &[Footprint],
    mut height_at: impl FnMut([f32; 2]) -> Result<Option<f32>, EngineError>,
) -> Result<TransformState, EngineError> {
    let [mut x, mut y, mut z] = pose.position.map(f32::from_bits);
    while seconds > 0.0 && !points.is_empty() {
        if !open_leg([x, y], points, obstacles, |point| {
            height_at(point).map(|z| z.is_some())
        })? {
            points.clear();
            break;
        }
        let Some(&target) = points.front() else { break };
        let [dx, dy] = [target[0] - x, target[1] - y];
        let distance = dx.hypot(dy);
        if distance <= 0.05 {
            points.pop_front();
            continue;
        }
        let (yaw, turn_time) = turn_toward(f32::from_bits(pose.orientation[2]), [dx, dy], seconds);
        pose.orientation[2] = yaw.to_bits();
        seconds -= turn_time;
        if seconds == 0.0 || speed <= 0.0 {
            break;
        }
        let travel_time = distance / speed;
        let reached = travel_time <= seconds;
        let xy = if reached {
            target
        } else {
            let fraction = speed * seconds / distance;
            [x + dx * fraction, y + dy * fraction]
        };
        let Some(ground) = height_at(xy)? else {
            points.clear();
            break;
        };
        let offset = height_at([x, y])?.map_or(0.0, |height| z - height);
        [x, y] = xy;
        z = ground + offset;
        pose.position = [x, y, z].map(f32::to_bits);
        if !reached {
            break;
        }
        points.pop_front();
        seconds -= travel_time;
    }
    Ok(pose)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_frame_crosses_waypoints_and_spends_time_turning() {
        let start = TransformState {
            position: [0.0_f32, 0.0, 2.0].map(f32::to_bits),
            ..Default::default()
        };
        let mut route = VecDeque::from([[0.0, 0.0], [0.0, 1.0], [4.0, 1.0]]);
        let end = follow_route(start, &mut route, 2.0, 2.0, &[], |[x, y]| Ok(Some(x + y))).unwrap();
        let [x, y, z] = end.position.map(f32::from_bits);
        let expected_x = 3.0 - std::f32::consts::FRAC_PI_2;
        assert!((x - expected_x).abs() < 0.000_01);
        assert_eq!(y, 1.0);
        assert!((z - (x + y + 2.0)).abs() < 0.000_01);
        assert_eq!(route, [[4.0, 1.0]]);
    }
}
