use crate::{Engine, MissionAssets, TerrainWorld, CONTROL_CLASS_SOURCE_BULL};
use fparkan_assets::AssetManager;
use fparkan_prototype::{resolve_prototype_all, PrototypeSource};
use fparkan_resource::{resource_name, CachedResourceRepository};
use fparkan_terrain::{FullSurfaceMask, SurfaceQuery, TerrainError};

/// Prepared visuals and travel parameters for one projectile prototype.
pub struct ProjectileAssets {
    /// Visual, materials and control profile for the single projectile mesh.
    pub assets: MissionAssets,
    /// Forward speed in world units per second.
    pub speed: f32,
    /// Maximum age on the mission clock, in milliseconds.
    pub lifetime_ms: u32,
}

/// Prepares the BULL prototype referenced by a manual weapon.
pub fn load_player_projectile(
    engine: &Engine,
    archive: &str,
    name: &str,
) -> Result<ProjectileAssets, String> {
    if !archive.eq_ignore_ascii_case("objects.rlb")
        || name.is_empty()
        || name.bytes().any(|byte| matches!(byte, b'/' | b'\\' | b'.'))
    {
        return Err(format!("unsupported projectile key {archive}:{name}"));
    }
    let vfs = engine.services.vfs.as_ref().ok_or("missing resource VFS")?;
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
    if !speed.is_finite() || speed <= 0.0 || lifetime_ms == 0 || lifetime_ms >= 0x8000_0000 {
        return Err("invalid projectile speed or lifetime".into());
    }
    Ok(ProjectileAssets {
        assets,
        speed,
        lifetime_ms,
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
pub fn projectile_crosses_terrain(
    terrain: &TerrainWorld,
    [start, end]: [[f32; 3]; 2],
) -> Result<bool, TerrainError> {
    let direction = std::array::from_fn(|axis| end[axis] - start[axis]);
    // The terrain query reports the ray parameter, so this segment ends at 1.
    Ok(terrain
        .raycast(start, direction, FullSurfaceMask(0))?
        .is_some_and(|hit| hit.distance <= 1.0))
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
