use super::{
    add3, compose_unit_pose, control_effect_owner_node_pose, rotate_unit_vector,
    LegacyIron3dEulerTransform, PlayerProjectile, PreparedControlPoint, StaticVulkanApp,
};

use fparkan_runtime::FiringEffectInstance;

impl StaticVulkanApp {
    pub(super) fn sample_player_fire(
        &mut self,
        clock_ms: u32,
    ) -> (
        Vec<(usize, PreparedControlPoint)>,
        Vec<FiringEffectInstance>,
    ) {
        let trigger = self.player_fire_held || self.player_fire_pressed;
        self.player_fire_pressed = false;
        let Some(cockpit) = self.cockpit else {
            return (Vec::new(), Vec::new());
        };
        if self.mission_frame.is_none()
            || !self
                .player_weapon_layouts
                .contains_key(&cockpit.object_index)
        {
            return (Vec::new(), Vec::new());
        }
        let Some(weapons) = self.player_weapon_banks.get_mut(&cockpit.handle) else {
            return (Vec::new(), Vec::new());
        };
        if let Some(engine) = &self.mission_engine {
            weapons.read_control_energy(engine, cockpit.handle);
        }
        let shots = weapons
            .fire(trigger, clock_ms)
            .map(|(slot, _, barrel)| (slot, barrel.clone()))
            .collect();
        if let Some(engine) = &mut self.mission_engine {
            weapons.publish_control_energy(engine, cockpit.handle);
        }
        (
            shots,
            weapons
                .started_windup_sounds(cockpit.object_index)
                .collect(),
        )
    }

    pub(super) fn emit_player_shots(
        &mut self,
        clock_ms: u32,
        shots: Vec<(usize, PreparedControlPoint)>,
    ) -> Vec<FiringEffectInstance> {
        if shots.is_empty() {
            return Vec::new();
        }
        let (Some(cockpit), Some(frame)) = (self.cockpit, &self.mission_frame) else {
            return Vec::new();
        };
        let (Some(weapons), Some(layout)) = (
            self.player_weapon_banks.get(&cockpit.handle),
            self.player_weapon_layouts.get(&cockpit.object_index),
        ) else {
            return Vec::new();
        };
        let Some((_, state)) = frame
            .snapshot
            .transforms
            .iter()
            .find(|(handle, _)| *handle == cockpit.handle)
        else {
            return Vec::new();
        };
        let transform = LegacyIron3dEulerTransform {
            translation: state.position.map(f32::from_bits),
            orientation_radians: state.orientation.map(f32::from_bits),
        };
        let scale = state.scale.map(f32::from_bits);
        let orientation = LegacyIron3dEulerTransform {
            translation: [0.0; 3],
            ..transform
        };
        let mut firing_effects = Vec::new();
        for (slot, barrel) in shots {
            let weapon = &weapons.weapons()[slot];
            let hierarchy = self
                .component_pose_buffers
                .get(cockpit.object_index)
                .and_then(|components| components.get(weapon.component_index))
                .and_then(Option::as_ref);
            let mount = self
                .component_mount_poses
                .get(cockpit.object_index)
                .and_then(|components| components.get(weapon.component_index))
                .copied()
                .flatten();
            let pose = control_effect_owner_node_pose(
                hierarchy,
                mount.is_some(),
                barrel.raw_signed_field_04 as usize,
            );
            let launch = pose.and_then(|pose| {
                let pose = mount.map_or(pose, |mount| compose_unit_pose(mount, pose));
                let point = add3(
                    pose.translation,
                    rotate_unit_vector(barrel.position, pose.rotation),
                );
                let muzzle = transform.try_transform_scaled_point(point, scale)?;
                let direction = if weapon.has_deployment_tracks() {
                    let axis = rotate_unit_vector(barrel.direction, pose.rotation);
                    orientation.try_transform_scaled_point(axis, scale)?
                } else {
                    // shortcut: use the sights for boarded warbots until
                    // their muzzle axes are compared with the original.
                    self.player_aim_direction?
                };
                Some((muzzle, direction))
            });
            let Some((muzzle, direction)) = launch else {
                eprintln!("sim_coverage player_weapon_pose_stub=1 slot={}", slot + 1);
                continue;
            };
            firing_effects.extend(weapon.firing_effect(cockpit.object_index));
            eprintln!(
                "player_shot shooter={} slot={} projectile={}:{} queue_time_ms={clock_ms} muzzle={muzzle:?} rounds_remaining={:?}",
                cockpit.handle.slot,
                slot + 1,
                weapon.projectile_archive,
                weapon.projectile,
                weapon.rounds_remaining(),
            );
            let (pool, sound) = &layout.slots[slot];
            let mut launch_sounds = &[][..];
            if let Some(draw) = self.projectile_draws[*pool].as_mut() {
                if let Some(instance) = draw
                    .instances
                    .iter_mut()
                    .find(|instance| instance.flight.is_none())
                {
                    instance.flight = PlayerProjectile::new(
                        muzzle,
                        direction,
                        draw.speed,
                        draw.lifetime_ms,
                        clock_ms,
                    );
                    instance.shooter = Some(cockpit.handle);
                    if let Some(flight) = &instance.flight {
                        launch_sounds = &draw.launch_sounds;
                        eprintln!("player_projectile_spawn slot={} queue_time_ms={clock_ms} speed={} lifetime_ms={} position={muzzle:?} direction={:?}", slot + 1, draw.speed, draw.lifetime_ms, flight.direction);
                    }
                } else {
                    eprintln!(
                        "sim_coverage player_projectile_pool_full=1 slot={}",
                        slot + 1
                    );
                }
            }
            if let Some(audio) = self.audio.as_mut() {
                for event in sound
                    .iter()
                    .chain(launch_sounds)
                    .filter_map(|sound| sound.one_shot_at(muzzle))
                {
                    match audio.handle_sound_event(&event) {
                        Ok(()) => eprintln!(
                            "player_shot_sound slot={} resource={}:{} queue_time_ms={clock_ms}",
                            slot + 1,
                            event.archive,
                            event.name
                        ),
                        Err(error) => eprintln!(
                            "sim_coverage player_weapon_sound_stub=1 slot={} reason={error}",
                            slot + 1
                        ),
                    }
                }
            }
        }
        firing_effects
    }
}
