use super::StaticVulkanApp;

impl StaticVulkanApp {
    pub(super) fn resolve_player_building_step(
        &self,
        mut step: fparkan_runtime::PlayerMotionStep,
    ) -> Result<(fparkan_runtime::PlayerMotionStep, bool), String> {
        if let Some(probe) = step.floor_probe() {
            let mesh_floor = self
                .building_segment_hit(probe, true)?
                .map(|point| point[2]);
            let terrain_exit = step.terrain_exit_floor();
            let mut previous_mesh = None;
            if mesh_floor.is_some() || terrain_exit.is_some() {
                previous_mesh = self
                    .building_segment_hit(
                        step.previous_floor_probe().expect("walking floor probe"),
                        true,
                    )?
                    .map(|point| point[2]);
            }
            let mut floor = mesh_floor;
            if let Some(height) = terrain_exit {
                if floor.is_none_or(|mesh| mesh < height)
                    && previous_mesh.is_some()
                    && !self.building_segment_blocked([
                        [probe[0][0], probe[0][1], height + 0.02],
                        probe[0],
                    ])?
                {
                    floor = Some(height);
                }
            }
            if let Some(height) = floor {
                step = step.with_building_floor(height, previous_mesh);
                if let Some(segments) = step.floor_sweeps() {
                    for segment in segments.into_iter().filter(|[from, to]| from != to) {
                        if self.building_segment_blocked(segment)? {
                            return Ok((step, true));
                        }
                    }
                }
                return Ok((step, false));
            }
        }
        let blocked = match step.sweep {
            Some(segment) => self.building_segment_blocked(segment)?,
            None => false,
        };
        Ok((step, blocked))
    }
}

#[cfg(test)]
mod tests {
    use crate::tests::clock_test_app;
    use crate::*;
    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_firing_range_keeps_clearance_across_bridge() {
        let root = std::path::PathBuf::from(
            std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed root"),
        );
        let args = Args::parse(&[
            "--root".into(),
            root.to_string_lossy().into_owned(),
            "--mission".into(),
            "MISSIONS/CAMPAIGN/CAMPAIGN.00/Mission.01/data.tma".into(),
        ])
        .unwrap();
        let mut engine = create(EngineServices::new(Arc::new(DirectoryVfs::new(&root)))).unwrap();
        load_requested_mission(&mut engine, &args).unwrap();
        let assets = loaded_mission_assets(&engine).unwrap();

        let (mut app, _) = clock_test_app(None);
        for (i, draft) in loaded_mission_object_drafts(&engine)
            .unwrap()
            .iter()
            .enumerate()
            .filter(|(i, _)| [23, 24].contains(i))
        {
            let visual = assets.visual_by_id(draft.visual_ids[0]).unwrap();
            let model = Arc::new(
                assets
                    .model_by_id(visual.model_id.unwrap())
                    .unwrap()
                    .validated
                    .clone(),
            );
            let transform = LegacyIron3dEulerTransform {
                translation: draft.position,
                orientation_radians: draft.orientation_raw,
            };
            let binding = building_collision_binding(
                i,
                0,
                "bridge".into(),
                model,
                transform,
                draft.scale,
                false,
            )
            .unwrap();
            app.component_pose_buffers.resize_with(i + 1, || vec![None]);
            app.component_pose_buffers[i][0] = assets.object_component_controls[i][0]
                .as_ref()
                .and_then(|control| {
                    static_preview_component_pose_buffer(&binding.model, &control.bindings, None)
                        .unwrap()
                });
            app.building_collision_bindings.push(binding);
        }
        let transform = app.building_collision_bindings[0].root_transform;
        let terrain = loaded_terrain(&engine).unwrap().clone();
        let cockpit = starting_cockpit(&engine).unwrap();
        let mut now = 0;
        let pose = |frame: FrameResult| {
            frame
                .snapshot
                .transforms
                .into_iter()
                .find(|(handle, _)| *handle == cockpit.handle)
                .unwrap()
                .1
        };
        let mut hero = pose(step_mission_frame(&mut engine, now).unwrap());
        let [x, y, z] = hero.position.map(f32::from_bits);
        let clearance = z - terrain.height_at([x, y]).unwrap().unwrap();
        let waypoints = [-42.0, -34.0, 36.0, 115.0, 198.0];
        for dy in waypoints {
            let [x, y, _] = transform
                .try_transform_scaled_point([0.0, dy, 0.0], [1.0; 3])
                .unwrap();
            for index in 0..900 {
                let [hx, hy, hz] = hero.position.map(f32::from_bits);
                let distance = (x - hx).hypot(y - hy);
                if distance < 0.03 {
                    let support = app
                        .building_segment_hit([[hx, hy, hz], [hx, hy, hz - 2.0]], true)
                        .unwrap()
                        .map_or_else(
                            || terrain.height_at([hx, hy]).unwrap().unwrap(),
                            |point| point[2],
                        );
                    assert!(
                        (hz - support - clearance).abs() < 0.01,
                        "clearance at waypoint {dy}: {}",
                        hz - support
                    );
                    break;
                }
                assert!(index < 899, "blocked for waypoint {dy}: {hx}, {hy}, {hz}");
                let yaw = f32::from_bits(hero.orientation[2]);
                let (sin, cos) = yaw.sin_cos();
                let dx = (x - hx) / distance;
                let dy = (y - hy) / distance;
                let step = plan_player_motion_with_collision(
                    &engine,
                    cockpit,
                    PlayerMotion {
                        movement: [dx * cos + dy * sin, dy * cos - dx * sin],
                        delta_seconds: distance.min(0.8) / 8.0,
                        ..Default::default()
                    },
                    |step| {
                        app.resolve_player_building_step(step)
                            .map_err(EngineError::Movement)
                    },
                )
                .unwrap()
                .unwrap();
                now += 100;
                hero =
                    pose(frame_with_player_motion(&mut engine, now, cockpit, step, false).unwrap());
                let new_z = f32::from_bits(hero.position[2]);
                if dy >= -34.0 {
                    assert!(
                        (new_z - hz).abs() < 0.3,
                        "height jump at {hx} {hy}: {hz} -> {new_z}"
                    );
                }
            }
        }
    }
}
