use super::StaticVulkanApp;

impl StaticVulkanApp {
    pub(super) fn resolve_player_building_step(
        &self,
        mut step: fparkan_runtime::PlayerMotionStep,
    ) -> Result<(fparkan_runtime::PlayerMotionStep, bool), String> {
        if let Some(probe) = step.floor_probe() {
            let crossing_cut = if self.terrain.basement_foundation().is_some() {
                let [start, end] = step.sweep.expect("walking floor probe");
                let support = |[x, y, _]: [f32; 3]| {
                    self.terrain
                        .support_at([x, y])
                        .map_err(|err| err.to_string())
                };
                let previous = support(start)?.map(|(height, _)| height);
                let next = support(end)?;
                step = step.with_scene_terrain(previous, next);
                previous.is_none() || next.is_none()
            } else {
                false
            };

            let mesh_floor = self
                .building_segment_hit(probe, true)?
                .map(|point| point[2]);
            let terrain_exit = step.terrain_exit_floor();
            let previous_mesh = if mesh_floor.is_some() || terrain_exit.is_some() {
                self.building_segment_hit(
                    step.previous_floor_probe().expect("walking floor probe"),
                    true,
                )?
                .map(|point| point[2])
            } else {
                None
            };
            let mut floor = mesh_floor.map(|height| (height, false));
            if let Some(height) = terrain_exit {
                if mesh_floor.is_none_or(|mesh| mesh < height)
                    && (crossing_cut || previous_mesh.is_some())
                    && !self.building_segment_blocked([
                        [probe[0][0], probe[0][1], height + 0.02],
                        probe[0],
                    ])?
                {
                    floor = Some((height, true));
                }
            }
            if let Some((height, terrain)) = floor {
                step = if terrain {
                    step.with_terrain_floor(height, previous_mesh)
                } else {
                    step.with_building_floor(height, previous_mesh)
                };
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
    fn licensed_constructor_walks_to_factory_cabin_and_back() {
        let root = std::path::PathBuf::from(
            std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed root"),
        );
        let args = Args::parse(&[
            "--root".into(),
            root.to_string_lossy().into_owned(),
            "--mission".into(),
            "MISSIONS/CAMPAIGN/CAMPAIGN.00/Mission.02/data.tma".into(),
        ])
        .unwrap();
        let mut engine = create(EngineServices::new(Arc::new(DirectoryVfs::new(&root)))).unwrap();
        let loaded = load_requested_mission(&mut engine, &args).unwrap();
        let terrain = loaded_terrain(&engine).unwrap();
        let assets = loaded_mission_assets(&engine).unwrap();
        let drafts = loaded_mission_object_drafts(&engine).unwrap();
        let footprints =
            mission_basement_footprints(assets, &drafts[..loaded.object_count]).unwrap();
        let foundation = build_basement_foundations(
            terrain.source_mesh().unwrap(),
            &footprints
                .iter()
                .map(|footprint| BasementFootprint {
                    outer: &footprint.outer,
                    inner: &footprint.inner,
                    flatten_inner_to_mean_outer_z: footprint.flatten_inner_to_mean_outer_z,
                })
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let adjusted =
            apply_basement_placement_deltas(drafts, &footprints, &foundation.placements).unwrap();
        let factory = &adjusted[0];
        let root_transform = LegacyIron3dEulerTransform {
            translation: factory.position,
            orientation_radians: factory.orientation_raw,
        };
        let scale = factory.scale;
        let visual = assets.visual_by_id(factory.visual_ids[0]).unwrap();
        let model = Arc::new(
            assets
                .model_by_id(visual.model_id.unwrap())
                .unwrap()
                .validated
                .clone(),
        );
        let control = assets.object_component_controls[0][0]
            .as_ref()
            .unwrap()
            .clone();
        let hierarchy = node38_fallback_hierarchy(&model).unwrap();
        let route = std::iter::once([320.0, 790.0])
            .chain(
                [67, 66, 65, 58, 59, 37, 36, 6, 7, 25, 35, 28, 31].map(|index| {
                    let point = &model.hallway.points[index];
                    let local =
                        hierarchy.poses[point.node.0 as usize].transform_point(point.position);
                    let [x, y, _] = root_transform
                        .try_transform_scaled_point(local, scale)
                        .unwrap();
                    [x, y]
                }),
            )
            .collect::<Vec<_>>();
        let (mut app, _) = clock_test_app(None);
        app.terrain = Arc::new(
            terrain
                .clone()
                .with_basement_foundation(foundation)
                .unwrap(),
        );
        app.building_collision_bindings.push(
            building_collision_binding(
                0,
                0,
                "factory".into(),
                model.clone(),
                root_transform,
                scale,
                false,
            )
            .unwrap(),
        );
        app.component_pose_buffers = vec![vec![None]];
        let cockpit = starting_cockpit(&engine).unwrap();
        let pose = |frame: FrameResult| {
            frame
                .snapshot
                .transforms
                .into_iter()
                .find(|(handle, _)| *handle == cockpit.handle)
                .unwrap()
                .1
        };
        let mut now = 0;
        let mut hero = pose(step_mission_frame(&mut engine, now).unwrap());
        let mut door_blends = Vec::new();
        let mut footstep_surfaces = [false; 2];
        for (index, [x, y]) in route
            .iter()
            .chain(route.iter().rev().skip(1))
            .copied()
            .enumerate()
        {
            let mut stalled = 0;
            loop {
                let [hx, hy, hz] = hero.position.map(f32::from_bits);
                let distance = (x - hx).hypot(y - hy);
                if distance <= 0.04 {
                    break;
                }
                let [dx, dy] = [(x - hx) / distance, (y - hy) / distance];
                let yaw = f32::from_bits(hero.orientation[2]);
                let (sin, cos) = yaw.sin_cos();
                let mut node_times: Vec<_> = fparkan_runtime::initial_component_node_times(
                    model.node_count,
                    &control.bindings,
                )
                .unwrap()
                .into_iter()
                .map(|time| time.map(|time| time.0))
                .collect();
                fparkan_runtime::sample_proximity_doors(
                    &control,
                    &model,
                    root_transform,
                    scale,
                    [hx, hy, hz + 0.5],
                    0.1,
                    &mut door_blends,
                    &mut node_times,
                );
                let factory_handle =
                    mission_object_handle(&engine, fparkan_world::OriginalObjectId(0)).unwrap();
                fparkan_runtime::sample_capture_barrier(
                    &engine,
                    factory_handle,
                    &control,
                    &mut node_times,
                );
                let times: Vec<_> = node_times
                    .into_iter()
                    .map(|time| time.map(AnimationTime))
                    .collect();
                app.component_pose_buffers[0][0] =
                    node38_sampled_hierarchy_at_times(&model, &times);
                let motion = plan_player_motion_with_collision(
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
                let frame =
                    frame_with_player_motion(&mut engine, now, cockpit, motion, false).unwrap();
                if let Some(footstep) = frame.player_footstep {
                    footstep_surfaces[usize::from(footstep.building_floor)] = true;
                }
                let moved = pose(frame);
                stalled = if moved.position == hero.position {
                    stalled + 1
                } else {
                    0
                };
                assert!(
                    stalled < 20,
                    "blocked before waypoint {index} [{x}, {y}] at [{hx}, {hy}, {hz}]"
                );
                assert!(now < 150_000, "walk stalled before waypoint {index}");
                hero = moved;
            }
            if index + 1 == route.len() {
                let z = f32::from_bits(hero.position[2]);
                assert!(z < factory.position[2] - 4.0, "factory cabin at {z}");
                assert_eq!(app.terrain.height_at([x, y]).unwrap(), None);
                assert_eq!(
                    fparkan_runtime::capture_building_with_notice(&mut engine, cockpit).unwrap(),
                    Some((
                        fparkan_world::OriginalObjectId(0),
                        fparkan_runtime::MissionNotice::BuildingCaptured { neutral: true }
                    ))
                );
                now += 17;
                step_mission_frame(&mut engine, now).unwrap();
                now += 500;
                step_mission_frame(&mut engine, now).unwrap();
            }
        }
        let [x, y, z] = hero.position.map(f32::from_bits);
        assert_eq!(footstep_surfaces, [true, true]);
        let ground = app.terrain.height_at([x, y]).unwrap().unwrap();
        assert!(
            (ground..ground + 1.0).contains(&z),
            "factory exit at {z}, ground {ground}"
        );
    }

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
