use crate::{
    apply_loaded_mission_control_node_delta, ControlResourceAccessError, Engine, MissionAssets,
    MissionControlNodeProviderTableStatus, TerrainWorld, CONTROL_CLASS_SOURCE_BULL,
};
use fparkan_assets::AssetManager;
use fparkan_prototype::{resolve_prototype_all, PrototypeSource};
use fparkan_render::LegacyIron3dEulerTransform;
use fparkan_resource::{resource_name, CachedResourceRepository};
use fparkan_terrain::{FullSurfaceMask, SurfaceQuery, TerrainError};
use fparkan_vfs::Vfs;
use fparkan_world::{handle_by_original_id, request_delete, transform_state, ObjectHandle};
use std::sync::Arc;

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
}

/// Finds the first other unit intersected by this flight segment.
pub fn projectile_unit_hit(
    engine: &Engine,
    shooter: ObjectHandle,
    segment: [[f32; 3]; 2],
) -> Option<ProjectileContact> {
    let mission = engine.loaded.as_ref()?;
    // shortcut: root MSH sphere until combat uses the per-node hull query.
    mission
        .object_drafts
        .iter()
        .enumerate()
        .filter_map(|(index, draft)| {
            let class = draft.unit_dat_header_flags?;
            if class & 0x0100_0000 == 0 {
                return None;
            }
            let handle = handle_by_original_id(&engine.world, draft.original_id?)?;
            if handle == shooter {
                return None;
            }
            let state = transform_state(&engine.world, handle).ok()?;
            let (center, radius) = unit_sphere(&mission.mission_assets, index, state)?;
            let fraction = segment_sphere_fraction(segment, center, radius)?;
            Some(ProjectileContact { handle, fraction })
        })
        .min_by(|a, b| a.fraction.total_cmp(&b.fraction))
}

pub(super) fn unit_sphere(
    assets: &MissionAssets,
    index: usize,
    state: fparkan_world::TransformState,
) -> Option<([f32; 3], f32)> {
    let visual = assets.visual_by_id(*assets.visuals_for_object(index).first()?)?;
    let model = &assets.model_by_id(visual.model_id?)?.validated;
    let scale = state.scale.map(f32::from_bits);
    let transform = LegacyIron3dEulerTransform {
        translation: state.position.map(f32::from_bits),
        orientation_radians: state.orientation.map(f32::from_bits),
    };
    let center = transform.try_transform_scaled_point(model.bounding_sphere_center, scale)?;
    let radius = model.bounding_sphere_radius * scale.into_iter().map(f32::abs).fold(0.0, f32::max);
    Some((center, radius))
}

fn segment_sphere_fraction(
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
        request_delete(&mut engine.world, handle).map_err(ControlResourceAccessError::World)?;
        return Ok(Some((0.0, true)));
    }
    apply_loaded_mission_control_node_delta(engine, handle, 0, -volume)?;
    Ok(Some((remaining - volume, false)))
}

#[cfg(test)]
mod tests {
    use super::*;

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
