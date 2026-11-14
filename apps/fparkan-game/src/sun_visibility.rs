use super::{
    add3, cross3, dot3, normalize3, scale3, sub3, ActiveMaterialState, PreviewCameraFrame,
};
use fparkan_render_vulkan::{VulkanStaticMaterial, VulkanStaticMesh};
use fparkan_terrain::{FullSurfaceMask, SurfaceQuery, TerrainWorld};
use std::collections::{HashMap, HashSet};

pub(super) struct SunOcclusionMaterials<'a> {
    pub materials: &'a [VulkanStaticMaterial],
    pub active: &'a HashMap<usize, ActiveMaterialState>,
    pub hidden: &'a HashSet<usize>,
    pub boarded_hero: Option<usize>,
}

#[cfg(test)]
pub(super) fn native_sun_unoccluded(
    terrain: &TerrainWorld,
    camera: &PreviewCameraFrame,
    direction: [f32; 3],
) -> bool {
    let empty_mesh = VulkanStaticMesh {
        vertices: Vec::new(),
        indices: Vec::new(),
        draw_ranges: Vec::new(),
    };
    native_sun_unoccluded_with_world(terrain, &empty_mesh, &[], camera, direction, None)
}

/// Tests the sun's center ray to the scene's far distance in Land.msh units.
pub(super) fn native_sun_unoccluded_with_world(
    terrain: &TerrainWorld,
    world_mesh: &VulkanStaticMesh,
    sun_occlusion_range_indices: &[usize],
    camera: &PreviewCameraFrame,
    direction: [f32; 3],
    world: Option<SunOcclusionMaterials<'_>>,
) -> bool {
    // The supplied ranges are eligible world models, excluding sky and FX.
    let Some(direction) = normalize3(direction) else {
        return false;
    };
    let sun_center = add3(
        camera.position,
        scale3(direction, camera.far_plane.max(camera.near_plane * 2.0)),
    );
    let start = add3(camera.position, scale3(direction, 0.5));
    let segment = sub3(sun_center, start);
    if !sun_center
        .iter()
        .chain(start.iter())
        .chain(segment.iter())
        .all(|value| value.is_finite())
    {
        return false;
    }
    if dot3(segment, segment) <= f32::EPSILON {
        return true;
    }
    if let Ok(Some(hit)) = terrain.raycast_excluding(
        start,
        segment,
        FullSurfaceMask(0),
        FullSurfaceMask(0x0000_0020),
    ) {
        if hit.distance.is_finite() && hit.distance <= 1.0 {
            return false;
        }
    }
    !world_mesh_sun_occluded(
        world_mesh,
        sun_occlusion_range_indices,
        start,
        segment,
        world,
    )
}

fn world_mesh_sun_occluded(
    mesh: &VulkanStaticMesh,
    range_indices: &[usize],
    origin: [f32; 3],
    direction: [f32; 3],
    world: Option<SunOcclusionMaterials<'_>>,
) -> bool {
    for &range_index in range_indices {
        let Some(range) = mesh.draw_ranges.get(range_index).copied() else {
            continue;
        };
        let material = world.as_ref().and_then(|world| {
            if range.owner_scope.checked_sub(1).is_some_and(|owner| {
                world.hidden.contains(&(owner as usize))
                    || world.boarded_hero == Some(owner as usize)
            }) {
                return None;
            }
            let active = world.active.get(&range_index);
            let index = active.map_or(usize::from(range.material_index), |state| {
                state.material_index
            });
            let material = world.materials.get(index)?;
            Some((
                material,
                active.map_or(material.diffuse_alpha, |state| state.diffuse_alpha),
            ))
        });
        if world.is_some() && material.is_none_or(|(_, alpha)| alpha <= 0.0) {
            continue;
        }
        // shortcut: let alpha-blended draw ranges refine the geometric exclusion
        // until the native foliage visibility query is recovered.
        if range.batch_flags & 0x0008 != 0
            && material.is_none_or(|(material, _)| material.pipeline_category != 1)
        {
            continue;
        }
        let Ok(first_index) = usize::try_from(range.first_index) else {
            continue;
        };
        let Ok(index_count) = usize::try_from(range.index_count) else {
            continue;
        };
        let Some(indices) = mesh
            .indices
            .get(first_index..first_index.saturating_add(index_count))
        else {
            continue;
        };
        for triangle in indices.chunks_exact(3) {
            let Ok(a) = usize::try_from(triangle[0]) else {
                continue;
            };
            let Ok(b) = usize::try_from(triangle[1]) else {
                continue;
            };
            let Ok(c) = usize::try_from(triangle[2]) else {
                continue;
            };
            let (Some(&a), Some(&b), Some(&c)) = (
                mesh.vertices.get(a),
                mesh.vertices.get(b),
                mesh.vertices.get(c),
            ) else {
                continue;
            };
            let Some(weights) =
                segment_triangle_weights(origin, direction, a.position, b.position, c.position)
            else {
                continue;
            };
            if material.is_none_or(|(material, alpha)| {
                let uv = std::array::from_fn(|axis| {
                    a.uv[axis] * weights[0] + b.uv[axis] * weights[1] + c.uv[axis] * weights[2]
                });
                material_blocks_sun(material, alpha, uv)
            }) {
                return true;
            }
        }
    }
    false
}

fn segment_triangle_weights(
    origin: [f32; 3],
    direction: [f32; 3],
    a: [f32; 3],
    b: [f32; 3],
    c: [f32; 3],
) -> Option<[f32; 3]> {
    let edge1 = sub3(b, a);
    let edge2 = sub3(c, a);
    let pvec = cross3(direction, edge2);
    let determinant = dot3(edge1, pvec);
    if !determinant.is_finite() || determinant.abs() <= f32::EPSILON {
        return None;
    }
    let inverse = determinant.recip();
    let tvec = sub3(origin, a);
    let u = dot3(tvec, pvec) * inverse;
    if !u.is_finite() || !(0.0..=1.0).contains(&u) {
        return None;
    }
    let qvec = cross3(tvec, edge1);
    let v = dot3(direction, qvec) * inverse;
    if !v.is_finite() || v < 0.0 || u + v > 1.0 {
        return None;
    }
    let distance = dot3(edge2, qvec) * inverse;
    (0.0..=1.0)
        .contains(&distance)
        .then_some([1.0 - u - v, u, v])
}

fn material_blocks_sun(material: &VulkanStaticMaterial, alpha: f32, uv: [f32; 2]) -> bool {
    match material.pipeline_category {
        0 => true,
        1 => {
            let [x, y, width, height] = material.uv_transform;
            let uv = [x + uv[0] * width, y + uv[1] * height];
            // shortcut: use the base mip and a binary center-ray test for
            // glare until its attenuation through partial coverage is known.
            alpha * texture_alpha(&material.texture, uv) >= 0.5
        }
        _ => false,
    }
}

fn texture_alpha(texture: &fparkan_render_vulkan::VulkanStaticTexture, uv: [f32; 2]) -> f32 {
    if !uv.iter().all(|value| value.is_finite()) || texture.width == 0 || texture.height == 0 {
        return 0.0;
    }
    let coordinate = |value: f32, extent: u32| {
        let texel = value.rem_euclid(1.0) * extent as f32 - 0.5;
        let lower = texel.floor();
        (
            (lower as i64).rem_euclid(i64::from(extent)) as usize,
            (lower as i64 + 1).rem_euclid(i64::from(extent)) as usize,
            texel - lower,
        )
    };
    let (x0, x1, dx) = coordinate(uv[0], texture.width);
    let (y0, y1, dy) = coordinate(uv[1], texture.height);
    let sample = |x, y: usize| {
        let offset = (y * texture.width as usize + x) * 4 + 3;
        texture
            .rgba8
            .get(offset)
            .map_or(0.0, |&a| f32::from(a) / 255.0)
    };
    let top = sample(x0, y0) * (1.0 - dx) + sample(x1, y0) * dx;
    let bottom = sample(x0, y1) * (1.0 - dx) + sample(x1, y1) * dx;
    top * (1.0 - dy) + bottom * dy
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment_vertex;

    #[test]
    fn sun_visibility_follows_leaf_alpha_at_the_hit() {
        let mut mesh = VulkanStaticMesh::smoke_triangle();
        mesh.vertices = vec![
            environment_vertex([5.0, 0.0, 0.0], [1.0; 3], [0.0, 0.0]),
            environment_vertex([5.0, 1.0, 0.0], [1.0; 3], [1.0, 0.0]),
            environment_vertex([5.0, 0.0, 1.0], [1.0; 3], [0.0, 1.0]),
        ];
        mesh.draw_ranges[0].batch_flags = 0x0008;
        mesh.draw_ranges[0].owner_scope = 1;
        let mut materials = Vec::new();
        crate::append_shadow_material(&mut materials).unwrap();
        let material = &mut materials[0];
        material.pipeline_category = 1;
        material.texture = fparkan_render_vulkan::VulkanStaticTexture {
            width: 2,
            height: 1,
            rgba8: vec![0, 0, 0, 255, 0, 0, 0, 0],
            mip_levels: Vec::new(),
        };
        let blocked = |y, hidden| {
            world_mesh_sun_occluded(
                &mesh,
                &[0],
                [0.0, y, 0.1],
                [10.0, 0.0, 0.0],
                Some(SunOcclusionMaterials {
                    materials: &materials,
                    active: &HashMap::new(),
                    hidden,
                    boarded_hero: None,
                }),
            )
        };
        let hidden = HashSet::new();
        let removed = HashSet::from([0]);
        assert!(blocked(0.25, &hidden));
        assert!(!blocked(0.75, &hidden));
        assert!(!blocked(0.25, &removed));
    }
}
