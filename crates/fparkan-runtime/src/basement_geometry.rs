use crate::{BasementPlacementMode, MissionAssets, MissionObjectDraft};
use fparkan_render::LegacyIron3dEulerTransform;
use fparkan_terrain::BasementFootprint;
use fparkan_world::{set_transform, transform_state, world_snapshot};

/// Places a newly completed building at its absolute foundation elevation.
///
/// The construction frame is refreshed so draw, collision, and script
/// consumers use a single root position.
///
/// # Errors
///
/// Rejects a missing building or an invalid elevation.
pub fn place_constructed_basement(
    engine: &mut crate::Engine,
    frame: &mut crate::FrameResult,
    building: crate::SpawnedUnit,
    elevation: f32,
) -> Result<(), crate::EngineError> {
    if !frame.constructed_buildings.contains(&building) || !elevation.is_finite() {
        return Err(crate::EngineError::Movement(
            "invalid placement for constructed basement".into(),
        ));
    }
    let mut pose = transform_state(&engine.world, building.handle)?;
    pose.position[2] = elevation.to_bits();
    set_transform(&mut engine.world, building.handle, pose)?;
    frame.snapshot = world_snapshot(&engine.world, std::mem::take(&mut frame.snapshot.events));
    Ok(())
}

/// One eligible building's authored contours projected into the mission.
pub struct MissionBasementFootprint {
    /// Index of the owning root in the prepared scene.
    pub object_index: usize,
    /// Outer contour in world space.
    pub outer: Vec<[f32; 2]>,
    /// Inner contour in world space.
    pub inner: Vec<[f32; 3]>,
    /// Whether the inner contour follows the outer terrain height.
    pub flatten_inner_to_mean_outer_z: bool,
}

impl MissionBasementFootprint {
    /// Borrows the projected contours for terrain processing.
    pub fn contours(&self) -> BasementFootprint<'_> {
        BasementFootprint {
            outer: &self.outer,
            inner: &self.inner,
            flatten_inner_to_mean_outer_z: self.flatten_inner_to_mean_outer_z,
        }
    }
}

/// Projects the contours of eligible mission roots in placement order.
///
/// # Errors
///
/// Rejects malformed or non-finite contours on eligible roots.
pub fn mission_basement_footprints(
    assets: &MissionAssets,
    roots: &[MissionObjectDraft],
) -> Result<Vec<MissionBasementFootprint>, String> {
    roots
        .iter()
        .enumerate()
        .filter_map(|(index, root)| mission_basement_footprint(assets, index, root).transpose())
        .collect()
}

/// Projects a building at its supplied pose while retaining its prepared asset index.
///
/// # Errors
///
/// Rejects invalid rings and non-finite coordinates in an eligible building.
pub fn mission_basement_footprint(
    assets: &MissionAssets,
    object_index: usize,
    mission_root: &MissionObjectDraft,
) -> Result<Option<MissionBasementFootprint>, String> {
    let Some(mode) = mission_root.basement_placement_mode() else {
        return Ok(None);
    };
    let Some(component_bas) = assets
        .object_component_bas
        .get(object_index)
        .and_then(|components| components.first())
        .and_then(Option::as_ref)
    else {
        return Ok(None);
    };
    let geometry = &component_bas.geometry;
    let bas_label = format!(
        "{}:{}",
        component_bas.source.archive.as_str(),
        String::from_utf8_lossy(&component_bas.source.name.0)
    );
    // shortcut: the installed Iron Strategy corpus has one inner and one
    // outer ring per eligible DAT. Native iterates both arrays, so reject
    // unsupported multiplicity until the multi-ring semantics are ported.
    if geometry.first.len() != 1 || geometry.second.len() != 1 {
        return Err(format!(
            "eligible mission object {object_index} from {bas_label} has {} inner and {} outer BAS rings; one of each is supported",
            geometry.first.len(),
            geometry.second.len()
        ));
    }
    let transform = LegacyIron3dEulerTransform {
        translation: mission_root.position,
        orientation_radians: mission_root.orientation_raw,
    };
    let transform_points = |points: &[[f32; 3]], ring_name: &str| {
        let mut transformed_points = Vec::with_capacity(points.len());
        for &point in points {
            let transformed = transform
                .try_transform_scaled_point(point, mission_root.scale)
                .ok_or_else(|| {
                    format!(
                        "building BAS {ring_name} ring on mission object {object_index} from {bas_label} has a non-finite transformed point"
                    )
                })?;
            transformed_points.push(transformed);
        }
        Ok::<_, String>(transformed_points)
    };
    let inner_record = &geometry.first[0];
    let outer_record = &geometry.second[0];
    for (ring_name, declared_count, points) in [
        ("inner", inner_record.declared_count, &inner_record.points),
        ("outer", outer_record.declared_count, &outer_record.points),
    ] {
        let expected_point_count = usize::try_from(declared_count)
            .ok()
            .and_then(|count| count.checked_add(1))
            .ok_or_else(|| format!("building BAS {ring_name} count overflows: {bas_label}"))?;
        if usize::try_from(declared_count).unwrap_or(0) < 3 || points.len() != expected_point_count
        {
            return Err(format!(
                "building BAS {ring_name} ring on mission object {object_index} from {bas_label} has count {declared_count} and {} points",
                points.len()
            ));
        }
    }
    let inner = transform_points(&inner_record.points, "inner")?;
    let outer = transform_points(&outer_record.points, "outer")?;
    Ok(Some(MissionBasementFootprint {
        object_index,
        outer: outer.iter().map(|point| [point[0], point[1]]).collect(),
        inner,
        flatten_inner_to_mean_outer_z: mode == BasementPlacementMode::FlattenInnerAndAdjustRoot,
    }))
}
