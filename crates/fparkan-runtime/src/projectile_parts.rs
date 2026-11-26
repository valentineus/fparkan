//! Resolve projectile contacts against the damageable nodes of a root model.

use crate::{
    projectile::{segment_sphere_fraction, ProjectileNode},
    Engine, LoadedMissionState, MissionAssets, MissionControlNodeProviderTableStatus, ObjectHandle,
    TransformState,
};
use fparkan_assets::PreparedModel;
use fparkan_msh::{
    node38_fallback_hierarchy, query_slot_segment, selected_slot, Group, Lod, NodeId,
    SegmentQueryMasks,
};
use fparkan_render::LegacyIron3dEulerTransform;

/// Visible chassis nodes exhausted in live warbots, in mission-object and local MSH order.
pub fn depleted_chassis_nodes(engine: &Engine) -> impl Iterator<Item = (usize, usize)> + '_ {
    engine.loaded.iter().flat_map(|mission| {
        mission
            .control_device_tables
            .iter()
            .filter_map(|table| {
                let index = usize::try_from(table.original_id?.0).ok()?;
                if mission.object_drafts.get(index)?.unit_dat_header_flags
                    != Some(super::boarding::WARBOT_CLASS)
                    || fparkan_world::transform_state(&engine.world, table.handle).is_err()
                {
                    return None;
                }
                let MissionControlNodeProviderTableStatus::Available(rows) =
                    &table.node_provider_status
                else {
                    return None;
                };
                Some((index, rows))
            })
            .flat_map(|(index, rows)| {
                rows.iter()
                    .skip(1)
                    .take_while(|row| row.unit_component_index == 0)
                    .filter(|row| row.current_flags.is_some_and(|flags| flags & 0x12 == 0x10))
                    .map(move |row| (index, row.source_row_index))
            })
    })
}

pub(super) fn chassis_part_contact(
    mission: &LoadedMissionState,
    index: usize,
    handle: ObjectHandle,
    state: TransformState,
    segment: [[f32; 3]; 2],
    (center, radius): ([f32; 3], f32),
    sphere_fraction: f32,
) -> Option<(f32, Option<ProjectileNode>)> {
    let rows = mission
        .control_device_tables
        .iter()
        .find(|table| table.handle == handle)
        .and_then(|table| match &table.node_provider_status {
            MissionControlNodeProviderTableStatus::Available(rows) => Some(rows),
            MissionControlNodeProviderTableStatus::Unavailable(_) => None,
        });
    let Some(rows) = rows.filter(|rows| {
        rows.first()
            .is_some_and(|root| root.initial_mesh_state.is_some())
    }) else {
        return Some((sphere_fraction, None));
    };
    if let Some((fraction, node)) =
        root_triangle_hit(&mission.mission_assets, index, state, segment, |node| {
            rows.get(node as usize).is_some_and(|row| {
                row.unit_component_index == 0
                    && row.source_row_index == node as usize
                    && f32::from_bits(row.current_amount_bits) > 0.0
                    && row.current_flags.is_some_and(|flags| flags & 0x10 == 0)
            })
        })
    {
        return Some((fraction, Some(ProjectileNode::Chassis(node))));
    }
    // shortcut: keep the hull sphere for unresolved mounted geometry, but
    // give the chassis mesh priority until the projectile exits the sphere.
    let [start, end] = segment;
    if (0..3)
        .map(|axis| (end[axis] - center[axis]).powi(2))
        .sum::<f32>()
        <= radius * radius
    {
        return None;
    }
    let reverse = segment_sphere_fraction([end, start], center, radius)?;
    Some((1.0 - reverse, None))
}

pub(super) fn root_model(assets: &MissionAssets, index: usize) -> Option<&PreparedModel> {
    let visual = assets.visual_by_id(*assets.visuals_for_object(index).first()?)?;
    assets.model_by_id(visual.model_id?)
}

pub(super) fn root_triangle_hit(
    assets: &MissionAssets,
    index: usize,
    state: TransformState,
    segment: [[f32; 3]; 2],
    accept_node: impl Fn(u32) -> bool,
) -> Option<(f32, u32)> {
    let model = &root_model(assets, index)?.validated;
    let root = LegacyIron3dEulerTransform {
        translation: state.position.map(f32::from_bits),
        orientation_radians: state.orientation.map(f32::from_bits),
    }
    .try_row_major()?;
    let scale = state.scale.map(f32::from_bits);
    let mut body_segment = segment;
    for (local, point) in body_segment.iter_mut().zip(segment) {
        let [x, y, z] = [point[0] - root[3], point[1] - root[7], point[2] - root[11]];
        for i in 0..3 {
            local[i] = (root[i] * x + root[i + 4] * y + root[i + 8] * z) / scale[i];
        }
    }
    // shortcut: use root MSH fallback poses until combat shares live component transforms.
    let poses = node38_fallback_hierarchy(model)?;
    let squared_length: f32 = (0..3)
        .map(|i| (segment[1][i] - segment[0][i]).powi(2))
        .sum();
    if squared_length == 0.0 {
        return None;
    }
    poses
        .poses
        .into_iter()
        .enumerate()
        .filter_map(|(i, pose)| {
            let node = NodeId(u32::try_from(i).ok()?);
            if !accept_node(node.0) {
                return None;
            }
            let slot = selected_slot(model, node, Lod(0), Group(0))?;
            let mut inverse = pose;
            inverse.translation = [0.0; 3];
            for component in &mut inverse.rotation[..3] {
                *component = -*component;
            }
            let endpoints = body_segment.map(|point| {
                inverse.transform_point(std::array::from_fn(|axis| {
                    point[axis] - pose.translation[axis]
                }))
            });
            let hit =
                query_slot_segment(model, node, slot, endpoints, SegmentQueryMasks::default())
                    .ok()??;
            let body_hit = pose.transform_point(hit.position);
            let distance: f32 = (0..3)
                .map(|axis| ((body_hit[axis] - body_segment[0][axis]) * scale[axis]).powi(2))
                .sum();
            Some(((distance / squared_length).sqrt(), node.0))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
}
