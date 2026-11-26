use crate::{
    apply_loaded_mission_control_node_delta,
    projectile_parts::{chassis_part_contact, root_model, root_triangle_hit},
    ControlResourceAccessError, Engine, MissionAssets, MissionControlNodeProviderTableStatus,
    TerrainWorld, CONTROL_CLASS_SOURCE_BULL, UNIT_DAT_CONTROL_CLASS_HIGH_BIT,
};
use fparkan_assets::AssetManager;
use fparkan_prototype::{resolve_prototype_all, PrototypeSource};
use fparkan_render::LegacyIron3dEulerTransform;
use fparkan_resource::{resource_name, CachedResourceRepository};
use fparkan_terrain::{FullSurfaceMask, SurfaceQuery, TerrainError};
use fparkan_vfs::Vfs;
use fparkan_world::{handle_by_original_id, request_delete, transform_state, ObjectHandle};
use std::sync::Arc;

pub(super) const ANIMAL_UNIT_CLASS: u32 = 0x2000_0000;

/// Prepared visuals and travel parameters for one projectile prototype.
pub struct ProjectileAssets {
    /// Visual, materials and control profile for the single projectile mesh.
    pub assets: MissionAssets,
    /// Forward speed in world units per second.
    pub speed: f32,
    /// Maximum age on the mission clock, in milliseconds.
    pub lifetime_ms: u32,
    /// Initial provider volume of the projectile.
    pub volume: Option<f32>,
}

/// Prepares the BULL prototype referenced by a manual weapon.
pub fn load_player_projectile(
    engine: &Engine,
    archive: &str,
    name: &str,
) -> Result<ProjectileAssets, String> {
    let vfs = engine.services.vfs.as_ref().ok_or("missing resource VFS")?;
    load_projectile(vfs, archive, name)
}

pub(super) fn load_projectile(
    vfs: &Arc<dyn Vfs>,
    archive: &str,
    name: &str,
) -> Result<ProjectileAssets, String> {
    if !archive.eq_ignore_ascii_case("objects.rlb")
        || name.is_empty()
        || name.bytes().any(|byte| matches!(byte, b'/' | b'\\' | b'.'))
    {
        return Err(format!("unsupported projectile key {archive}:{name}"));
    }
    let asset_manager = AssetManager::new(CachedResourceRepository::new(vfs.clone()));
    let prototypes = resolve_prototype_all(
        asset_manager.repository(),
        vfs.as_ref(),
        &resource_name(name.as_bytes()),
    )
    .map_err(|error| error.to_string())?;
    let [prototype] = prototypes.as_slice() else {
        return Err("expected a single projectile prototype".into());
    };
    if prototype.source != PrototypeSource::ObjectsRegistry
        || prototype.source_type != Some(CONTROL_CLASS_SOURCE_BULL)
    {
        return Err("expected an objects.rlb BULL prototype".into());
    }
    let assets = asset_manager
        .prepare_mission_assets(&[0..1], &prototypes)
        .map_err(|error| error.to_string())?;
    let [controls] = assets.object_component_controls.as_slice() else {
        return Err("missing projectile component".into());
    };
    let [Some(control)] = controls.as_slice() else {
        return Err("missing projectile Control profile".into());
    };
    let speed = f32::from_bits(control.native_profile_words[7]);
    let lifetime_ms = control.native_profile_words[18];
    let volume = control.node_data.as_ref().and_then(|data| {
        let amount = data.rows.first()?.max_volume;
        (amount.is_finite() && amount > 0.0).then_some(amount)
    });
    if !speed.is_finite() || speed <= 0.0 || lifetime_ms == 0 || lifetime_ms >= 0x8000_0000 {
        return Err("invalid projectile speed or lifetime".into());
    }
    Ok(ProjectileAssets {
        assets,
        speed,
        lifetime_ms,
        volume,
    })
}

/// One in-flight projectile with a pose sampled on the mission queue clock.
#[derive(Clone, Copy)]
pub struct PlayerProjectile {
    /// World-space center of the projectile.
    pub position: [f32; 3],
    /// Unit world-space heading along local +Y.
    pub direction: [f32; 3],
    speed: f32,
    lifetime_ms: u32,
    born_ms: u32,
    updated_ms: u32,
}

impl PlayerProjectile {
    /// Starts a straight-flight projectile at a validated world point.
    pub fn new(
        position: [f32; 3],
        direction: [f32; 3],
        speed: f32,
        lifetime_ms: u32,
        clock_ms: u32,
    ) -> Option<Self> {
        let length = direction.iter().map(|axis| axis * axis).sum::<f32>().sqrt();
        if !position.iter().all(|axis| axis.is_finite())
            || !length.is_finite()
            || length <= 0.0
            || !speed.is_finite()
            || speed <= 0.0
            || lifetime_ms == 0
        {
            return None;
        }
        Some(Self {
            position,
            direction: direction.map(|axis| axis / length),
            speed,
            lifetime_ms,
            born_ms: clock_ms,
            updated_ms: clock_ms,
        })
    }

    /// Advances this projectile and returns whether it remains in flight.
    /// The callback identifies contacts with the world along the traveled segment.
    pub fn advance(
        &mut self,
        clock_ms: u32,
        mut collides: impl FnMut([[f32; 3]; 2]) -> Result<bool, TerrainError>,
    ) -> Result<bool, TerrainError> {
        let age = clock_ms.wrapping_sub(self.born_ms);
        let last_age = self.updated_ms.wrapping_sub(self.born_ms);
        let elapsed_ms = age.min(self.lifetime_ms).saturating_sub(last_age);
        if elapsed_ms != 0 {
            let distance = self.speed * (elapsed_ms as f32 * 0.001);
            let end =
                std::array::from_fn(|axis| self.position[axis] + self.direction[axis] * distance);
            if !end.iter().all(|axis| axis.is_finite()) || collides([self.position, end])? {
                return Ok(false);
            }
            self.position = end;
            self.updated_ms = clock_ms;
        }
        Ok(age < self.lifetime_ms)
    }
}

/// Tests the projectile segment against the mission terrain triangles.
pub fn projectile_terrain_hit(
    terrain: &TerrainWorld,
    [start, end]: [[f32; 3]; 2],
) -> Result<Option<f32>, TerrainError> {
    let direction = std::array::from_fn(|axis| end[axis] - start[axis]);
    // The terrain query reports the ray parameter, so this segment ends at 1.
    Ok(terrain
        .raycast(start, direction, FullSurfaceMask(0))?
        .filter(|hit| hit.distance <= 1.0)
        .map(|hit| hit.distance))
}

/// A live unit reached by a projectile, with the fraction of its travel used.
#[derive(Clone, Copy, Debug)]
pub struct ProjectileContact {
    pub handle: ObjectHandle,
    pub fraction: f32,
    /// World point on the segment at the detected contact.
    pub position: [f32; 3],
    /// Provider row identified by a triangle hit, if its geometry was projected.
    pub node: Option<ProjectileNode>,
}

/// Control node hit by a projectile's segment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectileNode {
    /// A node of the building model.
    Building(u32),
    /// A node of the warbot's chassis model.
    Chassis(u32),
}

impl ProjectileContact {
    /// Provider key for a building hit, for callers that only track structures.
    pub fn building_node(self) -> Option<u32> {
        if let Some(ProjectileNode::Building(key)) = self.node {
            Some(key)
        } else {
            None
        }
    }
}

/// Finds the first unit or building intersected by this flight segment.
pub fn projectile_unit_hit(
    engine: &Engine,
    shooter: ObjectHandle,
    segment: [[f32; 3]; 2],
) -> Option<ProjectileContact> {
    let mission = engine.loaded.as_ref()?;
    let passenger = mission.boarding.map(|boarding| boarding.hero.handle);
    mission
        .object_drafts
        .iter()
        .enumerate()
        .filter_map(|(index, draft)| {
            let class = draft.unit_dat_header_flags?;
            let building = class & UNIT_DAT_CONTROL_CLASS_HIGH_BIT != 0;
            let animal = class == ANIMAL_UNIT_CLASS;
            if !building && !animal && class & super::autodemo::MOBILE_UNIT == 0 {
                return None;
            }
            let handle = handle_by_original_id(&engine.world, draft.original_id?)?;
            if handle == shooter || passenger == Some(handle) {
                return None;
            }
            let state = transform_state(&engine.world, handle).ok()?;
            let (center, radius) = unit_sphere(&mission.mission_assets, index, state)?;
            let sphere_fraction = segment_sphere_fraction(segment, center, radius)?;
            let (fraction, node) = if building {
                let (fraction, node) =
                    root_triangle_hit(&mission.mission_assets, index, state, segment, |_| true)?;
                (fraction, Some(ProjectileNode::Building(node)))
            } else if class == super::boarding::WARBOT_CLASS {
                chassis_part_contact(
                    mission,
                    index,
                    handle,
                    state,
                    segment,
                    (center, radius),
                    sphere_fraction,
                )?
            } else {
                (sphere_fraction, None)
            };
            Some(ProjectileContact {
                handle,
                fraction,
                position: std::array::from_fn(|axis| {
                    segment[0][axis] + (segment[1][axis] - segment[0][axis]) * fraction
                }),
                node,
            })
        })
        .min_by(|a, b| a.fraction.total_cmp(&b.fraction))
}

pub(super) fn unit_sphere(
    assets: &MissionAssets,
    index: usize,
    state: fparkan_world::TransformState,
) -> Option<([f32; 3], f32)> {
    let model = &root_model(assets, index)?.validated;
    let scale = state.scale.map(f32::from_bits);
    let transform = LegacyIron3dEulerTransform {
        translation: state.position.map(f32::from_bits),
        orientation_radians: state.orientation.map(f32::from_bits),
    };
    let center = transform.try_transform_scaled_point(model.bounding_sphere_center, scale)?;
    let radius = model.bounding_sphere_radius * scale.into_iter().map(f32::abs).fold(0.0, f32::max);
    Some((center, radius))
}

/// Applies projectile damage to the contacted part of a building or unit.
pub fn apply_projectile_contact(
    engine: &mut Engine,
    contact: ProjectileContact,
    volume: f32,
) -> Result<Option<(f32, bool)>, ControlResourceAccessError> {
    let table = engine.loaded.as_ref().and_then(|mission| {
        mission
            .control_device_tables
            .iter()
            .find(|table| table.handle == contact.handle)
    });
    let volume = super::projectile_damage::resisted_volume(
        volume,
        table.and_then(|table| table.projectile_resistance_bits),
    );
    let key = match contact.node {
        None | Some(ProjectileNode::Chassis(0)) => {
            return apply_projectile_hit(engine, contact.handle, volume);
        }
        Some(ProjectileNode::Chassis(key) | ProjectileNode::Building(key)) => key,
    };
    let row = table.and_then(|table| super::control_node_provider_row(table, key).ok());
    let Some(row) = row else {
        return Ok(None);
    };
    let remaining = f32::from_bits(row.current_amount_bits);
    if !volume.is_finite()
        || volume <= 0.0
        || !remaining.is_finite()
        || remaining <= 0.0
        || row.current_flags.is_some_and(|flags| flags & 0x10 != 0)
    {
        return Ok(None);
    }
    if volume >= remaining
        && matches!(contact.node, Some(ProjectileNode::Chassis(_)))
        && row.current_flags.is_some_and(|flags| flags & 1 != 0)
    {
        return destroy_projectile_target(engine, contact.handle).map(Some);
    }
    // shortcut: feed the projectile NDP volume into the native reduction while impact falloff is open.
    let effect = (volume >= remaining && engine.destruction_effects.is_some())
        .then(|| super::destruction::part_effect(row, contact.position))
        .flatten();
    let updated = apply_loaded_mission_control_node_delta(engine, contact.handle, key, -volume)?;
    if updated.is_some() {
        if let (Some(effect), Some(events)) = (effect, engine.destruction_effects.as_mut()) {
            events.push(effect);
        }
    }
    Ok(updated.map(|_| ((remaining - volume).max(0.0), false)))
}

pub(super) fn segment_sphere_fraction(
    [start, end]: [[f32; 3]; 2],
    center: [f32; 3],
    radius: f32,
) -> Option<f32> {
    if !radius.is_finite() || radius <= 0.0 {
        return None;
    }
    let dir: [f32; 3] = std::array::from_fn(|i| end[i] - start[i]);
    let rel: [f32; 3] = std::array::from_fn(|i| start[i] - center[i]);
    let dot = |a: [f32; 3], b: [f32; 3]| a.into_iter().zip(b).map(|(a, b)| a * b).sum::<f32>();
    let c = dot(rel, rel) - radius * radius;
    if c <= 0.0 {
        return Some(0.0);
    }
    let a = dot(dir, dir);
    let b = dot(rel, dir);
    let d = b * b - a * c;
    let fraction = (-b - d.sqrt()) / a;
    (0.0..=1.0).contains(&fraction).then_some(fraction)
}

/// Applies the first pass of combat damage to the root Control provider.
/// Returns the remaining volume and whether the world removed the unit.
pub fn apply_projectile_hit(
    engine: &mut Engine,
    handle: ObjectHandle,
    volume: f32,
) -> Result<Option<(f32, bool)>, ControlResourceAccessError> {
    let Some(remaining) = engine.loaded.as_ref().and_then(|mission| {
        let table = mission
            .control_device_tables
            .iter()
            .find(|table| table.handle == handle)?;
        let MissionControlNodeProviderTableStatus::Available(rows) = &table.node_provider_status
        else {
            return None;
        };
        Some(f32::from_bits(rows.first()?.current_amount_bits))
    }) else {
        return Ok(None);
    };
    if !volume.is_finite() || volume <= 0.0 || !remaining.is_finite() || remaining <= 0.0 {
        return Ok(None);
    }
    // shortcut: use the projectile provider volume directly, with immediate
    // removal on root depletion until native damage and death timing are wired.
    if volume >= remaining {
        return destroy_projectile_target(engine, handle).map(Some);
    }
    apply_loaded_mission_control_node_delta(engine, handle, 0, -volume)?;
    Ok(Some((remaining - volume, false)))
}

fn destroy_projectile_target(
    engine: &mut Engine,
    handle: ObjectHandle,
) -> Result<(f32, bool), ControlResourceAccessError> {
    let effect = super::destruction::pending_effect(engine, handle);
    request_delete(&mut engine.world, handle).map_err(ControlResourceAccessError::World)?;
    if let (Some(effect), Some(events)) = (effect, engine.destruction_effects.as_mut()) {
        events.push(effect);
    }
    Ok((0.0, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_chassis_node_takes_damage_without_destroying_the_warbot() {
        use crate::{
            collect_unit_destruction_effects, create, load_gameplay_mission, starting_cockpit,
            take_unit_destruction_effects, EngineServices, MissionMode, MissionRequest,
            MissionStartupInput,
        };
        use fparkan_vfs::DirectoryVfs;
        use fparkan_world::OriginalObjectId;

        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed corpus");
        let mut engine = create(EngineServices::new(Arc::new(DirectoryVfs::new(root)))).unwrap();
        load_gameplay_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/Single.01/data.tma".into(),
                mode: MissionMode::SingleMission,
            },
            MissionStartupInput {
                queue_time_ms: 0,
                research_creation_gate: false,
            },
            None,
        )
        .unwrap();
        let shooter = starting_cockpit(&engine).unwrap().handle;
        let target = handle_by_original_id(&engine.world, OriginalObjectId(14)).unwrap();
        let mission = engine.loaded.as_ref().unwrap();
        let pose = transform_state(&engine.world, target).unwrap();
        let ([x, y, z], radius) = unit_sphere(&mission.mission_assets, 14, pose).unwrap();
        let (contact, node) = (-4..=4)
            .flat_map(|y_offset| (-4..=4).map(move |z_offset| (y_offset, z_offset)))
            .find_map(|(y_offset, z_offset)| {
                let y = y + y_offset as f32 * radius / 5.0;
                let z = z + z_offset as f32 * radius / 5.0;
                let segment = [[x - 2.0 * radius, y, z], [x + 2.0 * radius, y, z]];
                let hit = projectile_unit_hit(&engine, shooter, segment)?;
                let Some(ProjectileNode::Chassis(node @ 2..=9)) = hit.node else {
                    return None;
                };
                (hit.handle == target).then_some((hit, node))
            })
            .expect("a side shot intersects a chassis part of the stock heavy warbot");
        let amount = |engine: &Engine, key| {
            let table = engine
                .loaded
                .as_ref()
                .unwrap()
                .control_device_tables
                .iter()
                .find(|table| table.handle == target)
                .unwrap();
            crate::control_node_provider_row(table, key)
                .unwrap()
                .current_amount_bits
        };
        let root_before = amount(&engine, 0);
        assert!(f32::from_bits(amount(&engine, node)) > 0.0);
        collect_unit_destruction_effects(&mut engine);
        assert_eq!(
            apply_projectile_contact(&mut engine, contact, f32::MAX).unwrap(),
            Some((0.0, false))
        );
        assert_eq!(amount(&engine, node), 0.0_f32.to_bits());
        assert_eq!(amount(&engine, 0), root_before);
        assert!(transform_state(&engine.world, target).is_ok());
        let effects = take_unit_destruction_effects(&mut engine);
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].position, contact.position);
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_factory_part_explodes_once_on_depletion() {
        use crate::{
            collect_unit_destruction_effects, create, load_gameplay_mission,
            take_unit_destruction_effects, unit_destruction_effect_sources, EngineServices,
            MissionMode, MissionRequest, MissionStartupInput,
        };
        use fparkan_vfs::DirectoryVfs;
        use fparkan_world::OriginalObjectId;

        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed corpus");
        let mut engine = create(EngineServices::new(Arc::new(DirectoryVfs::new(root)))).unwrap();
        load_gameplay_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/Single.01/data.tma".into(),
                mode: MissionMode::SingleMission,
            },
            MissionStartupInput {
                queue_time_ms: 0,
                research_creation_gate: false,
            },
            None,
        )
        .unwrap();
        let plant = handle_by_original_id(&engine.world, OriginalObjectId(5)).unwrap();
        let contact = ProjectileContact {
            handle: plant,
            fraction: 0.5,
            position: [1300.0, 627.0, 84.0],
            node: Some(ProjectileNode::Building(3)),
        };
        collect_unit_destruction_effects(&mut engine);
        assert_eq!(
            apply_projectile_contact(&mut engine, contact, 5_000.0).unwrap(),
            Some((5_000.0, false))
        );
        assert!(take_unit_destruction_effects(&mut engine).is_empty());
        assert_eq!(
            apply_projectile_contact(&mut engine, contact, 5_000.0).unwrap(),
            Some((0.0, false))
        );
        let events = take_unit_destruction_effects(&mut engine);
        let [effect] = events.as_slice() else {
            panic!("expected one EXPL at the failed part");
        };
        assert_eq!(effect.position, contact.position);
        assert_eq!(effect.object_index, None);
        assert_eq!(effect.name, "explode_frt_b.exp");
        assert!(unit_destruction_effect_sources(&engine)
            .any(|source| source == (effect.archive.clone(), effect.name.clone())));
        assert!(apply_projectile_contact(&mut engine, contact, 5_000.0)
            .unwrap()
            .is_none());
        assert!(take_unit_destruction_effects(&mut engine).is_empty());
        assert!(transform_state(&engine.world, plant).is_ok());
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_constructor_animal_takes_projectile_damage() {
        use crate::{
            create, load_gameplay_mission, starting_cockpit, EngineServices, MissionMode,
            MissionRequest, MissionStartupInput,
        };
        use fparkan_vfs::DirectoryVfs;

        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed corpus");
        let mut engine = create(EngineServices::new(Arc::new(DirectoryVfs::new(root)))).unwrap();
        load_gameplay_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/CAMPAIGN/CAMPAIGN.00/Mission.02/data.tma".into(),
                mode: MissionMode::SingleMission,
            },
            MissionStartupInput {
                queue_time_ms: 0,
                research_creation_gate: false,
            },
            None,
        )
        .unwrap();
        let shooter = starting_cockpit(&engine).unwrap().handle;
        let mission = engine.loaded.as_ref().unwrap();
        let (index, draft) = mission
            .object_drafts
            .iter()
            .enumerate()
            .find(|(_, draft)| draft.unit_dat_header_flags == Some(ANIMAL_UNIT_CLASS))
            .unwrap();
        let creature = handle_by_original_id(&engine.world, draft.original_id.unwrap()).unwrap();
        let pose = transform_state(&engine.world, creature).unwrap();
        let ([x, y, z], radius) = unit_sphere(&mission.mission_assets, index, pose).unwrap();
        let shot = [[x, y, z + 2.0 * radius], [x, y, z]];
        let contact = projectile_unit_hit(&engine, shooter, shot).expect("animal contact");
        assert_eq!(contact.handle, creature);
        let table = mission
            .control_device_tables
            .iter()
            .find(|table| table.handle == creature)
            .unwrap();
        let hp = f32::from_bits(
            crate::control_node_provider_row(table, 0)
                .unwrap()
                .current_amount_bits,
        );
        assert!(hp.is_finite() && hp > 0.0);
        let remaining = hp * 0.5;
        assert_eq!(
            apply_projectile_contact(&mut engine, contact, remaining).unwrap(),
            Some((remaining, false))
        );
        assert_eq!(
            apply_projectile_contact(&mut engine, contact, remaining).unwrap(),
            Some((0.0, true))
        );
        assert!(transform_state(&engine.world, creature).is_err());
    }

    #[test]
    fn projectile_uses_queue_age_and_tests_only_the_traveled_segment() {
        let mut shot =
            PlayerProjectile::new([0.0; 3], [0.0, 2.0, 0.0], 100.0, 80, u32::MAX - 39).unwrap();
        assert!(shot
            .advance(0, |[from, to]| {
                assert_eq!(from, [0.0; 3]);
                assert!((to[1] - 4.0).abs() < 1e-6);
                Ok(false)
            })
            .unwrap());
        assert!(shot.advance(0, |_| panic!("replayed sample")).unwrap());
        assert!(!shot
            .advance(90, |[from, to]| {
                assert!((from[1] - 4.0).abs() < 1e-6);
                assert!((to[1] - 8.0).abs() < 1e-6);
                Ok(true)
            })
            .unwrap());
    }
}
