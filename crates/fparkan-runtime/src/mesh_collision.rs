//! World-space segment queries over selected building MSH nodes and slots.

use fparkan_msh::{
    node38_metadata, query_slot_segment, ModelAsset, MshError, NodeId, SegmentQueryMasks, SlotId,
};
use fparkan_render::RawCameraTransform;

/// One selected MSH node/slot and its affine transform into world space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BuildingMeshNode {
    /// Model-local node id.
    pub node: NodeId,
    /// Currently selected slot for this node.
    pub slot: SlotId,
    /// Row-major affine transform from node-local space to world space.
    pub world_from_node: [f32; 16],
}

/// One accepted building mesh hit in world space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BuildingMeshHit {
    /// Node that supplied this hit.
    pub node: NodeId,
    /// Selected slot that supplied this hit.
    pub slot: SlotId,
    /// Hit position in world space.
    pub position: [f32; 3],
    /// Squared world distance from the original segment start.
    pub distance_squared: f32,
    /// Global Batch20 index.
    pub batch_index: usize,
    /// Global type-7 triangle descriptor index.
    pub triangle_descriptor_index: usize,
    /// Global Res3 position indices of the hit triangle.
    pub vertex_indices: [usize; 3],
}

const MAX_INTERSECTION_VISITS: usize = 200;

/// Queries a world-space segment against all supplied building nodes and slots.
///
/// Node flags use the native camera query's `0/0` masks. The initial batch
/// query excludes `0x0200` while keeping bit `0x0008` visible to the building
/// retry rule; triangle flags use `0/0x0024`. A nearest bit-8 batch hit restarts
/// the query from that world-space point toward the original endpoint, just as
/// the native building camera does. Each helper's Batch20 `local_batch_index`
/// selects the next Node38 root; that retry visits its eligible descendants
/// child-before-self and queries only nodes with supplied geometry bindings.
pub fn query_building_segment_world(
    model: &ModelAsset,
    nodes: &[BuildingMeshNode],
    world_segment: [[f32; 3]; 2],
) -> Result<Option<BuildingMeshHit>, MshError> {
    if !world_segment
        .iter()
        .flatten()
        .all(|value| value.is_finite())
    {
        return Err(MshError::InvalidGeometry(
            "building query segment contains a non-finite endpoint".to_string(),
        ));
    }

    let prepared_nodes = nodes
        .iter()
        .map(|node| {
            prepare_node(*node).map_err(|reason| {
                MshError::InvalidGeometry(format!(
                    "building node {} has an invalid world transform: {reason}",
                    node.node.0
                ))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let original_start = world_segment[0];
    let original_end = world_segment[1];
    let mut segment_start = original_start;
    let mut linked_root = None;
    let mut intersection_visits = 0;
    loop {
        let mut nearest = None;
        let query_nodes = if let Some(root) = linked_root {
            let order = linked_node_postorder(model, root)?;
            let mut ordered = Vec::new();
            for node_id in order {
                ordered.extend(
                    prepared_nodes
                        .iter()
                        .filter(|node| node.source.node == node_id),
                );
            }
            ordered
        } else {
            prepared_nodes.iter().collect()
        };
        for node in query_nodes {
            let local_segment = [
                transform_point(node.node_from_world, segment_start)
                    .ok_or_else(|| invalid_transformed_point(node.source.node, "world start"))?,
                transform_point(node.node_from_world, original_end)
                    .ok_or_else(|| invalid_transformed_point(node.source.node, "world endpoint"))?,
            ];
            let hit = query_slot_segment(
                model,
                node.source.node,
                node.source.slot,
                local_segment,
                SegmentQueryMasks {
                    node_include: 0,
                    node_exclude: 0,
                    batch_include: 0,
                    // CBuilding clears bit 8 from the camera's 0x208 exclude
                    // mask so it can restart after inspecting the raw batch.
                    batch_exclude: 0x0200,
                    triangle_include: 0,
                    triangle_exclude: 0x0024,
                },
            )
            .map_err(|source| {
                MshError::InvalidGeometry(format!(
                    "building node {} slot {} segment query failed: {source}",
                    node.source.node.0, node.source.slot.0
                ))
            })?;
            let Some(hit) = hit else {
                continue;
            };
            let position = transform_point(node.source.world_from_node, hit.position)
                .ok_or_else(|| invalid_transformed_point(node.source.node, "local hit"))?;
            let offset = subtract3(position, original_start);
            let distance_squared = dot3(offset, offset);
            if !distance_squared.is_finite() {
                return Err(MshError::InvalidGeometry(format!(
                    "building node {} produced a non-finite world hit distance",
                    node.source.node.0
                )));
            }
            let batch_flags = model
                .batches
                .get(hit.batch_index)
                .ok_or_else(|| {
                    MshError::InvalidGeometry(format!(
                        "building node {} query returned missing batch {}",
                        node.source.node.0, hit.batch_index
                    ))
                })?
                .batch_flags;
            let candidate = BuildingMeshHit {
                node: node.source.node,
                slot: node.source.slot,
                position,
                distance_squared,
                batch_index: hit.batch_index,
                triangle_descriptor_index: hit.triangle_descriptor_index,
                vertex_indices: hit.vertex_indices,
            };
            if nearest.is_none_or(|(best, _): (BuildingMeshHit, u16)| {
                candidate.distance_squared <= best.distance_squared
            }) {
                nearest = Some((candidate, batch_flags));
            }
        }

        let Some((nearest, batch_flags)) = nearest else {
            return Ok(None);
        };
        intersection_visits += 1;
        if intersection_visits == MAX_INTERSECTION_VISITS {
            return Err(MshError::InvalidGeometry(format!(
                "building query exhausted the native {MAX_INTERSECTION_VISITS}-intersection visit limit"
            )));
        }
        if batch_flags & 0x0008 == 0 {
            return Ok(Some(nearest));
        }
        let batch = model.batches.get(nearest.batch_index).ok_or_else(|| {
            MshError::InvalidGeometry(format!(
                "building query returned missing batch {}",
                nearest.batch_index
            ))
        })?;
        if batch.local_batch_index == u16::MAX {
            return Err(MshError::InvalidGeometry(format!(
                "building helper batch {} has the sentinel linked-node key",
                nearest.batch_index
            )));
        }
        let linked_node = NodeId(u32::from(batch.local_batch_index));
        if usize::try_from(linked_node.0).unwrap_or(usize::MAX) >= model.node_count {
            return Err(MshError::InvalidGeometry(format!(
                "building helper batch {} has linked node {} outside Node38 count {}",
                nearest.batch_index, linked_node.0, model.node_count
            )));
        }
        segment_start = nearest.position;
        linked_root = Some(linked_node);
    }
}

/// Builds the native child-before-self query order rooted at a portal link.
///
/// Node metadata is traversed even when the caller has no selected geometry
/// binding for that node, because eligible descendants can still carry hits.
fn linked_node_postorder(model: &ModelAsset, root: NodeId) -> Result<Vec<NodeId>, MshError> {
    let root_index = usize::try_from(root.0).map_err(|_| {
        MshError::InvalidGeometry(format!("linked query node {} is not addressable", root.0))
    })?;
    if root_index >= model.node_count {
        return Err(MshError::InvalidGeometry(format!(
            "linked query node {} is outside Node38 count {}",
            root.0, model.node_count
        )));
    }

    let metadata = (0..model.node_count)
        .map(|index| {
            let node = NodeId(u32::try_from(index).map_err(|_| {
                MshError::InvalidGeometry(format!("Node38 index {index} is not addressable"))
            })?);
            node38_metadata(model, node).ok_or_else(|| {
                MshError::InvalidGeometry(format!(
                    "linked query requires Node38 metadata for node {index}"
                ))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    for (index, entry) in metadata.iter().enumerate() {
        if entry.parent_or_link_raw != u16::MAX
            && usize::from(entry.parent_or_link_raw) >= model.node_count
        {
            return Err(MshError::InvalidGeometry(format!(
                "Node38 parent {} for node {index} is outside node count {}",
                entry.parent_or_link_raw, model.node_count
            )));
        }
    }

    // Each stack frame marks entry or postorder exit. This keeps traversal
    // bounded by the model table rather than by the native hierarchy depth.
    let mut stack = vec![(root_index, false)];
    let mut active = vec![false; model.node_count];
    let mut visited = vec![false; model.node_count];
    let mut ordered = Vec::new();
    while let Some((index, exiting)) = stack.pop() {
        if exiting {
            active[index] = false;
            visited[index] = true;
            ordered.push(NodeId(u32::try_from(index).map_err(|_| {
                MshError::InvalidGeometry(format!("Node38 index {index} is not addressable"))
            })?));
            continue;
        }
        if active[index] {
            return Err(MshError::InvalidGeometry(format!(
                "linked Node38 query contains a cycle at node {index}"
            )));
        }
        if visited[index] {
            continue;
        }

        active[index] = true;
        stack.push((index, true));
        if metadata[index].header_flags & 0x0010 == 0 {
            let parent_word = u16::try_from(index).map_err(|_| {
                MshError::InvalidGeometry(format!(
                    "Node38 index {index} cannot be represented by its parent field"
                ))
            })?;
            for child in (0..model.node_count).rev() {
                if metadata[child].parent_or_link_raw != u16::MAX
                    && metadata[child].parent_or_link_raw == parent_word
                    && metadata[child].header_flags & 0x0004 == 0
                {
                    stack.push((child, false));
                }
            }
        }
    }
    Ok(ordered)
}

#[derive(Clone, Copy)]
struct PreparedNode {
    source: BuildingMeshNode,
    node_from_world: [f32; 16],
}

fn prepare_node(node: BuildingMeshNode) -> Result<PreparedNode, &'static str> {
    if !node.world_from_node.iter().all(|value| value.is_finite()) {
        return Err("matrix contains a non-finite value");
    }
    if node.world_from_node[12].abs() > f32::EPSILON
        || node.world_from_node[13].abs() > f32::EPSILON
        || node.world_from_node[14].abs() > f32::EPSILON
        || (node.world_from_node[15] - 1.0).abs() > f32::EPSILON
    {
        return Err("matrix is not affine");
    }
    let node_from_world = RawCameraTransform {
        words: node.world_from_node.map(f32::to_bits),
    }
    .try_inverse_affine_row_major()
    .ok_or("matrix is singular or its inverse is not finite")?;
    if !node_from_world.iter().all(|value| value.is_finite()) {
        return Err("matrix inverse contains a non-finite value");
    }
    Ok(PreparedNode {
        source: node,
        node_from_world,
    })
}

fn invalid_transformed_point(node: NodeId, label: &str) -> MshError {
    MshError::InvalidGeometry(format!(
        "building node {} transformed {label} to a non-finite point",
        node.0
    ))
}

fn transform_point(matrix: [f32; 16], point: [f32; 3]) -> Option<[f32; 3]> {
    let transformed = [
        matrix[0] * point[0] + matrix[1] * point[1] + matrix[2] * point[2] + matrix[3],
        matrix[4] * point[0] + matrix[5] * point[1] + matrix[6] * point[2] + matrix[7],
        matrix[8] * point[0] + matrix[9] * point[1] + matrix[10] * point[2] + matrix[11],
    ];
    transformed
        .iter()
        .all(|value| value.is_finite())
        .then_some(transformed)
}

fn subtract3(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

fn dot3(left: [f32; 3], right: [f32; 3]) -> f32 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

#[cfg(test)]
mod tests {
    use super::{
        query_building_segment_world, BuildingMeshHit, BuildingMeshNode, MAX_INTERSECTION_VISITS,
    };
    use fparkan_msh::{
        query_slot_segment, Batch, ModelAsset, MshError, NodeId, SegmentQueryMasks, Slot, SlotId,
        TriangleDescriptor,
    };

    const IDENTITY: [f32; 16] = [
        1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ];
    const CAMERA_MASKS: SegmentQueryMasks = SegmentQueryMasks {
        node_include: 0,
        node_exclude: 0,
        batch_include: 0,
        batch_exclude: 0x0200,
        triangle_include: 0,
        triangle_exclude: 0x0024,
    };

    fn node(node: u32, world_from_node: [f32; 16]) -> BuildingMeshNode {
        node_with_slot(node, 0, world_from_node)
    }

    fn node_with_slot(node: u32, slot: u32, world_from_node: [f32; 16]) -> BuildingMeshNode {
        BuildingMeshNode {
            node: NodeId(node),
            slot: SlotId(slot),
            world_from_node,
        }
    }

    fn model_with_planes(planes: &[(f32, u16)]) -> ModelAsset {
        model_with_slots(&[planes])
    }

    fn model_with_slots(planes_per_slot: &[&[(f32, u16)]]) -> ModelAsset {
        model_with_slots_and_nodes(planes_per_slot, 2)
    }

    fn model_with_slots_and_nodes(
        planes_per_slot: &[&[(f32, u16)]],
        node_count: usize,
    ) -> ModelAsset {
        let mut positions = Vec::new();
        let mut indices = Vec::new();
        let mut batches = Vec::new();
        let mut triangle_descriptors = Vec::new();
        let mut slots = Vec::new();
        for planes in planes_per_slot {
            let descriptor_start = triangle_descriptors.len();
            let batch_start = batches.len();
            let min_y = planes.iter().map(|(y, _)| *y).fold(0.0, f32::min);
            let max_y = planes.iter().map(|(y, _)| *y).fold(0.0, f32::max);
            let center_y = (min_y + max_y) * 0.5;
            let half_height = (max_y - min_y) * 0.5;
            for (y, batch_flags) in *planes {
                let base_vertex = positions.len();
                let index_start = indices.len();
                positions.extend([[-20.0, *y, -20.0], [0.0, *y, 20.0], [20.0, *y, -20.0]]);
                indices.extend([0, 1, 2]);
                batches.push(Batch {
                    batch_flags: *batch_flags,
                    material_index_hi: 0,
                    material_index: 0,
                    lightmap_index: 0,
                    local_batch_index: u16::MAX,
                    index_count: 3,
                    index_start: u32::try_from(index_start).expect("small synthetic index offset"),
                    vertex_count: 3,
                    base_vertex: u32::try_from(base_vertex).expect("small synthetic vertex offset"),
                });
                triangle_descriptors.push(TriangleDescriptor {
                    tri_flags: 0,
                    opaque: [0; 4],
                    normal: [0, 32767, 0],
                });
            }
            slots.push(Slot {
                tri_start: u16::try_from(descriptor_start)
                    .expect("small synthetic descriptor start"),
                tri_count: u16::try_from(triangle_descriptors.len() - descriptor_start)
                    .expect("small synthetic descriptor count"),
                batch_start: u16::try_from(batch_start).expect("small synthetic batch start"),
                batch_count: u16::try_from(batches.len() - batch_start)
                    .expect("small synthetic batch count"),
                aabb_min: [-20.0, min_y, -20.0],
                aabb_max: [20.0, max_y, 20.0],
                sphere_center: [0.0, center_y, 0.0],
                sphere_radius: (20.0_f32.powi(2) * 2.0 + half_height.powi(2)).sqrt(),
                opaque: [0; 5],
            });
        }

        ModelAsset {
            node_stride: 38,
            node_count,
            nodes_raw: vec![0; node_count * 38],
            type16_present: false,
            bounding_sphere_center: [0.0; 3],
            bounding_sphere_radius: 0.0,
            slots,
            positions,
            normals: None,
            uv0: None,
            uv1: None,
            indices,
            batches,
            triangle_descriptors: Some(triangle_descriptors),
            node_names: None,
            animation: None,
        }
    }

    fn set_node_metadata(model: &mut ModelAsset, node: usize, flags: u16, parent: u16) {
        let offset = node * model.node_stride;
        model.nodes_raw[offset..offset + 2].copy_from_slice(&flags.to_le_bytes());
        model.nodes_raw[offset + 2..offset + 4].copy_from_slice(&parent.to_le_bytes());
    }

    fn set_batch_link(model: &mut ModelAsset, batch: usize, node: u16) {
        model.batches[batch].local_batch_index = node;
    }

    fn set_root_node_metadata(model: &mut ModelAsset) {
        for node in 0..model.node_count {
            set_node_metadata(model, node, 0, u16::MAX);
        }
    }

    fn translated(x: f32, y: f32, z: f32) -> [f32; 16] {
        let mut matrix = IDENTITY;
        matrix[3] = x;
        matrix[7] = y;
        matrix[11] = z;
        matrix
    }

    fn hit(result: Option<BuildingMeshHit>) -> BuildingMeshHit {
        result.expect("segment should hit the synthetic plane")
    }

    fn assert_point(actual: [f32; 3], expected: [f32; 3]) {
        for axis in 0..3 {
            assert!(
                (actual[axis] - expected[axis]).abs() < 1.0e-4,
                "{actual:?} != {expected:?}"
            );
        }
    }

    #[test]
    fn translation_returns_a_world_space_hit() {
        let result = query_building_segment_world(
            &model_with_planes(&[(0.0, 0)]),
            &[node(0, translated(4.0, 5.0, -3.0))],
            [[4.0, 8.0, -3.0], [4.0, 2.0, -3.0]],
        )
        .expect("valid world query");

        let hit = hit(result);
        assert_point(hit.position, [4.0, 5.0, -3.0]);
        assert_eq!(hit.distance_squared, 9.0);
    }

    #[test]
    fn rotation_transforms_the_world_segment_into_node_space() {
        let rotated = [
            0.0, -1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];
        let result = query_building_segment_world(
            &model_with_planes(&[(0.0, 0)]),
            &[node(0, rotated)],
            [[-3.0, 0.0, 0.0], [3.0, 0.0, 0.0]],
        )
        .expect("valid world query");

        assert_point(hit(result).position, [0.0, 0.0, 0.0]);
    }

    #[test]
    fn nonuniform_scale_preserves_world_hit_coordinates() {
        let scaled = [
            2.0, 0.0, 0.0, 0.0, 0.0, 3.0, 0.0, 0.0, 0.0, 0.0, 0.5, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];
        let result = query_building_segment_world(
            &model_with_planes(&[(0.0, 0)]),
            &[node(0, scaled)],
            [[0.0, 2.0, 0.25], [0.0, -2.0, 0.25]],
        )
        .expect("valid world query");

        let hit = hit(result);
        assert_point(hit.position, [0.0, 0.0, 0.25]);
        assert_eq!(hit.distance_squared, 4.0);
    }

    #[test]
    fn mirrored_transform_uses_the_mirrored_world_side() {
        let mirrored = [
            1.0, 0.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];
        let result = query_building_segment_world(
            &model_with_planes(&[(0.0, 0)]),
            &[node(0, mirrored)],
            [[0.0, -3.0, 0.0], [0.0, 3.0, 0.0]],
        )
        .expect("valid mirrored world query");

        assert_point(hit(result).position, [0.0, 0.0, 0.0]);
    }

    #[test]
    fn nearest_hit_is_ranked_in_world_space_from_original_start() {
        let mut farther = translated(0.0, 0.0, 0.0);
        let mut nearer = translated(0.0, 2.0, 0.0);
        // Make local distances deliberately incomparable across nodes.
        farther[5] = 2.0;
        nearer[5] = 0.5;
        let result = query_building_segment_world(
            &model_with_planes(&[(0.0, 0)]),
            &[node(0, farther), node(1, nearer)],
            [[0.0, 5.0, 0.0], [0.0, -1.0, 0.0]],
        )
        .expect("valid world query");

        let hit = hit(result);
        assert_eq!(hit.node, NodeId(1));
        assert_point(hit.position, [0.0, 2.0, 0.0]);
        assert_eq!(hit.distance_squared, 9.0);
    }

    #[test]
    fn retries_from_bit8_helper_to_the_farther_wall() {
        let helper_plane = [(2.0, 0x0008)];
        let wall_plane = [(0.0, 0)];
        let mut model = model_with_slots(&[&helper_plane, &wall_plane]);
        set_root_node_metadata(&mut model);
        set_batch_link(&mut model, 0, 1);
        let result = query_building_segment_world(
            &model,
            &[
                node_with_slot(0, 0, IDENTITY),
                node_with_slot(1, 1, IDENTITY),
            ],
            [[0.0, 3.0, 0.0], [0.0, -1.0, 0.0]],
        )
        .expect("valid world query");

        let hit = hit(result);
        assert_eq!(hit.node, NodeId(1));
        assert_eq!(hit.slot, SlotId(1));
        assert_eq!(hit.batch_index, 1);
        assert_point(hit.position, [0.0, 0.0, 0.0]);
        assert_eq!(hit.distance_squared, 9.0);
    }

    #[test]
    fn coplanar_helper_hit_is_requeried_instead_of_filtered_out() {
        let mut model = model_with_slots(&[&[(0.0, 0)], &[(0.0, 0x0008)]]);
        set_root_node_metadata(&mut model);
        set_batch_link(&mut model, 1, 1);
        let result = query_building_segment_world(
            &model,
            &[
                node_with_slot(1, 0, IDENTITY),
                node_with_slot(0, 1, IDENTITY),
            ],
            [[0.0, 3.0, 0.0], [0.0, -1.0, 0.0]],
        )
        .expect("valid world query");

        assert!(result.is_none());
    }

    #[test]
    fn world_roundtrip_helper_cannot_be_reselected_after_portal_link() {
        const START: [f32; 3] = [301.39682, 806.24396, 2.868942];
        const END: [f32; 3] = [304.3922, 806.4102, 2.868942];
        let world_from_node = [
            f32::from_bits(0xbd62_f745),
            f32::from_bits(0xbf7f_9b4f),
            0.0,
            f32::from_bits(0x4379_43ca),
            f32::from_bits(0x3f7f_9b4f),
            f32::from_bits(0xbd62_f745),
            0.0,
            f32::from_bits(0x4448_b667),
            0.0,
            0.0,
            1.0,
            f32::from_bits(0x4110_2494),
            0.0,
            0.0,
            0.0,
            1.0,
        ];
        let mut model = model_with_slots_and_nodes(&[&[(-54.239994, 0x0008)]], 4);
        set_batch_link(&mut model, 0, 2);
        set_node_metadata(&mut model, 0, 0, u16::MAX);
        set_node_metadata(&mut model, 1, 0x0104, 0);
        set_node_metadata(&mut model, 2, 0x0004, 1);
        set_node_metadata(&mut model, 3, 0x00d1, 2);
        let source = node(1, world_from_node);

        // Reproduce the old all-node retry: the world-space first hit maps
        // back to a local start just above this same helper plane.
        let prepared = super::prepare_node(source).expect("invert actual AutoDemo matrix");
        let local_start =
            super::transform_point(prepared.node_from_world, START).expect("finite local start");
        let local_end =
            super::transform_point(prepared.node_from_world, END).expect("finite local endpoint");
        let first_hit = query_slot_segment(
            &model,
            NodeId(1),
            SlotId(0),
            [local_start, local_end],
            CAMERA_MASKS,
        )
        .expect("valid first helper query")
        .expect("first helper crossing");
        let world_hit =
            super::transform_point(world_from_node, first_hit.position).expect("finite world hit");
        let repeated_local_start = super::transform_point(prepared.node_from_world, world_hit)
            .expect("finite round-tripped start");
        assert!(query_slot_segment(
            &model,
            NodeId(1),
            SlotId(0),
            [repeated_local_start, local_end],
            CAMERA_MASKS,
        )
        .expect("valid repeated helper query")
        .is_some());

        let result = query_building_segment_world(&model, &[source], [START, END])
            .expect("valid portal-linked world query");
        assert!(result.is_none());
    }

    #[test]
    fn linked_subtree_finds_drawable_child_through_nondrawable_parent() {
        let mut model = model_with_slots_and_nodes(&[&[(2.0, 0x0008)], &[(0.0, 0)]], 4);
        set_root_node_metadata(&mut model);
        set_batch_link(&mut model, 0, 2);
        set_node_metadata(&mut model, 1, 0x0104, 0);
        set_node_metadata(&mut model, 2, 0x0004, 1);
        set_node_metadata(&mut model, 3, 0x00d1, 2);

        let result = query_building_segment_world(
            &model,
            &[
                node_with_slot(1, 0, IDENTITY),
                node_with_slot(3, 1, IDENTITY),
            ],
            [[0.0, 3.0, 0.0], [0.0, -1.0, 0.0]],
        )
        .expect("valid linked-child query");

        let hit = hit(result);
        assert_eq!(hit.node, NodeId(3));
        assert_eq!(hit.slot, SlotId(1));
        assert_eq!(hit.batch_index, 1);
        assert_point(hit.position, [0.0, 0.0, 0.0]);
    }

    #[test]
    fn linked_query_is_child_before_self_and_self_bit4_does_not_skip_geometry() {
        let mut model =
            model_with_slots_and_nodes(&[&[(2.0, 0x0008)], &[(0.0, 0)], &[(0.0, 0)]], 4);
        set_root_node_metadata(&mut model);
        set_batch_link(&mut model, 0, 2);
        set_node_metadata(&mut model, 1, 0x0104, 0);
        set_node_metadata(&mut model, 2, 0x0004, 1);
        set_node_metadata(&mut model, 3, 0x00d1, 2);

        let result = query_building_segment_world(
            &model,
            &[
                node_with_slot(1, 0, IDENTITY),
                node_with_slot(2, 1, IDENTITY),
                node_with_slot(3, 2, IDENTITY),
            ],
            [[0.0, 3.0, 0.0], [0.0, -1.0, 0.0]],
        )
        .expect("valid child-first linked query");

        let hit = hit(result);
        // The equal-distance later candidate replaces the child result.
        assert_eq!(hit.node, NodeId(2));
        assert_eq!(hit.slot, SlotId(1));
    }

    #[test]
    fn linked_query_traverses_nondrawable_nodes_and_skips_child_bit4_branches() {
        let mut model = model_with_slots_and_nodes(
            &[&[(2.0, 0x0008)], &[(0.0, 0)], &[(0.0, 0)], &[(0.0, 0)]],
            7,
        );
        set_root_node_metadata(&mut model);
        set_batch_link(&mut model, 0, 2);
        set_node_metadata(&mut model, 1, 0x0104, 0);
        set_node_metadata(&mut model, 2, 0x0004, 1);
        set_node_metadata(&mut model, 3, 0x0000, 2);
        set_node_metadata(&mut model, 4, 0x0010, 3);
        set_node_metadata(&mut model, 5, 0x0004, 3);
        set_node_metadata(&mut model, 6, 0x0004, 2);

        let result = query_building_segment_world(
            &model,
            &[
                node_with_slot(1, 0, IDENTITY),
                node_with_slot(4, 1, IDENTITY),
                node_with_slot(5, 2, IDENTITY),
                node_with_slot(6, 3, IDENTITY),
            ],
            [[0.0, 3.0, 0.0], [0.0, -1.0, 0.0]],
        )
        .expect("valid linked branch query");

        let hit = hit(result);
        assert_eq!(hit.node, NodeId(4));
        assert_eq!(hit.slot, SlotId(1));
    }

    #[test]
    fn linked_query_keeps_native_batch_and_triangle_exclude_masks() {
        let mut model =
            model_with_slots_and_nodes(&[&[(2.0, 0x0008)], &[(0.0, 0)], &[(0.0, 0x0200)]], 5);
        set_root_node_metadata(&mut model);
        set_batch_link(&mut model, 0, 2);
        set_node_metadata(&mut model, 1, 0x0104, 0);
        set_node_metadata(&mut model, 2, 0x0004, 1);
        set_node_metadata(&mut model, 3, 0x00d1, 2);
        set_node_metadata(&mut model, 4, 0x00d1, 2);
        model
            .triangle_descriptors
            .as_mut()
            .expect("synthetic type-7 table")[1]
            .tri_flags = 0x0024;

        let result = query_building_segment_world(
            &model,
            &[
                node_with_slot(1, 0, IDENTITY),
                node_with_slot(3, 1, IDENTITY),
                node_with_slot(4, 2, IDENTITY),
            ],
            [[0.0, 3.0, 0.0], [0.0, -1.0, 0.0]],
        )
        .expect("valid query with native exclusion masks");

        assert!(result.is_none());
    }

    #[test]
    fn linked_order_stops_descent_at_header_bit10_but_keeps_self() {
        let mut model = model_with_slots_and_nodes(&[&[]], 5);
        set_root_node_metadata(&mut model);
        set_node_metadata(&mut model, 1, 0, 0);
        set_node_metadata(&mut model, 2, 0x0014, 1);
        set_node_metadata(&mut model, 3, 0, 2);
        set_node_metadata(&mut model, 4, 0, 3);

        let order = super::linked_node_postorder(&model, NodeId(2)).expect("valid linked tree");

        assert_eq!(order, [NodeId(2)]);
    }

    #[test]
    fn invalid_portal_link_and_linked_cycle_are_explicit_errors() {
        let mut invalid_link = model_with_planes(&[(2.0, 0x0008)]);
        set_root_node_metadata(&mut invalid_link);
        set_batch_link(&mut invalid_link, 0, 2);
        assert!(matches!(
            query_building_segment_world(
                &invalid_link,
                &[node(0, IDENTITY)],
                [[0.0, 3.0, 0.0], [0.0, -1.0, 0.0]],
            ),
            Err(MshError::InvalidGeometry(message)) if message.contains("linked")
        ));

        let mut cycle = model_with_slots_and_nodes(&[&[(2.0, 0x0008)]], 3);
        set_batch_link(&mut cycle, 0, 1);
        set_node_metadata(&mut cycle, 0, 0, u16::MAX);
        set_node_metadata(&mut cycle, 1, 0, 2);
        set_node_metadata(&mut cycle, 2, 0, 1);
        assert!(matches!(
            query_building_segment_world(
                &cycle,
                &[node(0, IDENTITY)],
                [[0.0, 3.0, 0.0], [0.0, -1.0, 0.0]],
            ),
            Err(MshError::InvalidGeometry(message)) if message.contains("cycle")
        ));
    }

    #[test]
    fn invalid_non_affine_singular_and_nonfinite_transforms_are_explicit_errors() {
        let mut non_affine = IDENTITY;
        non_affine[12] = 0.5;
        assert!(matches!(
            query_building_segment_world(
                &model_with_planes(&[(0.0, 0)]),
                &[node(0, non_affine)],
                [[0.0, 1.0, 0.0], [0.0, -1.0, 0.0]],
            ),
            Err(MshError::InvalidGeometry(message)) if message.contains("not affine")
        ));

        let mut singular = IDENTITY;
        singular[5] = 0.0;
        assert!(matches!(
            query_building_segment_world(
                &model_with_planes(&[(0.0, 0)]),
                &[node(0, singular)],
                [[0.0, 1.0, 0.0], [0.0, -1.0, 0.0]],
            ),
            Err(MshError::InvalidGeometry(message)) if message.contains("singular")
        ));

        let mut nonfinite = IDENTITY;
        nonfinite[0] = f32::NAN;
        assert!(matches!(
            query_building_segment_world(
                &model_with_planes(&[(0.0, 0)]),
                &[node(0, nonfinite)],
                [[0.0, 1.0, 0.0], [0.0, -1.0, 0.0]],
            ),
            Err(MshError::InvalidGeometry(message)) if message.contains("non-finite")
        ));
    }

    #[test]
    fn wall_after_198_helpers_is_accepted_as_the_199th_hit() {
        let helper_count = MAX_INTERSECTION_VISITS - 2;
        let mut planes = (0..helper_count)
            .map(|index| ((helper_count - index) as f32, 0x0008))
            .collect::<Vec<_>>();
        planes.push((0.0, 0));
        let mut model = model_with_planes(&planes);
        set_root_node_metadata(&mut model);
        for batch in 0..helper_count {
            set_batch_link(&mut model, batch, 0);
        }
        let result = query_building_segment_world(
            &model,
            &[node(0, IDENTITY)],
            [[0.0, (helper_count + 1) as f32, 0.0], [0.0, -1.0, 0.0]],
        )
        .expect("199th intersection is below the native limit");

        let hit = hit(result);
        assert_eq!(hit.batch_index, helper_count);
        assert_point(hit.position, [0.0, 0.0, 0.0]);
    }

    #[test]
    fn wall_after_199_helpers_errors_as_the_200th_hit() {
        let helper_count = MAX_INTERSECTION_VISITS - 1;
        let mut planes = (0..helper_count)
            .map(|index| ((helper_count - index) as f32, 0x0008))
            .collect::<Vec<_>>();
        planes.push((0.0, 0));
        let mut model = model_with_planes(&planes);
        set_root_node_metadata(&mut model);
        for batch in 0..helper_count {
            set_batch_link(&mut model, batch, 0);
        }
        let result = query_building_segment_world(
            &model,
            &[node(0, IDENTITY)],
            [[0.0, (helper_count + 1) as f32, 0.0], [0.0, -1.0, 0.0]],
        );

        assert!(matches!(
            result,
            Err(MshError::InvalidGeometry(message)) if message.contains("200")
        ));
    }

    fn compose_row_major(left: [f32; 16], right: [f32; 16]) -> [f32; 16] {
        let mut result = [0.0; 16];
        for row in 0..4 {
            for column in 0..4 {
                for inner in 0..4 {
                    result[row * 4 + column] += left[row * 4 + inner] * right[inner * 4 + column];
                }
            }
        }
        result
    }

    fn pose_matrix(pose: fparkan_animation::Pose) -> [f32; 16] {
        let x = rotate_unit_vector([1.0, 0.0, 0.0], pose.rotation);
        let y = rotate_unit_vector([0.0, 1.0, 0.0], pose.rotation);
        let z = rotate_unit_vector([0.0, 0.0, 1.0], pose.rotation);
        [
            x[0],
            y[0],
            z[0],
            pose.translation[0],
            x[1],
            y[1],
            z[1],
            pose.translation[1],
            x[2],
            y[2],
            z[2],
            pose.translation[2],
            0.0,
            0.0,
            0.0,
            1.0,
        ]
    }

    fn rotate_unit_vector(value: [f32; 3], rotation: [f32; 4]) -> [f32; 3] {
        let [x, y, z, w] = rotation;
        let tx = 2.0 * (y * value[2] - z * value[1]);
        let ty = 2.0 * (z * value[0] - x * value[2]);
        let tz = 2.0 * (x * value[1] - y * value[0]);
        [
            value[0] + w * tx + (y * tz - z * ty),
            value[1] + w * ty + (z * tx - x * tz),
            value[2] + w * tz + (x * ty - y * tx),
        ]
    }

    fn scale_matrix(scale: [f32; 3]) -> [f32; 16] {
        [
            scale[0], 0.0, 0.0, 0.0, 0.0, scale[1], 0.0, 0.0, 0.0, 0.0, scale[2], 0.0, 0.0, 0.0,
            0.0, 1.0,
        ]
    }

    fn node_times_from_bindings(
        node_count: usize,
        bindings: &[fparkan_assets::PreparedControlNodeBinding],
    ) -> Vec<Option<fparkan_animation::AnimationTime>> {
        use fparkan_animation::AnimationTime;

        let mut times = vec![Some(AnimationTime(0.0)); node_count];
        for binding in bindings {
            if binding.flags & 0x04 != 0 {
                continue;
            }
            assert!(binding.node_index < node_count);
            let mut blend = if binding.flags & 0x01 != 0 {
                let mut value = binding.initial_blend;
                if value > 1.0 {
                    value -= 1.0;
                }
                if value < 0.0 {
                    value += 1.0;
                }
                value
            } else {
                binding.initial_blend.clamp(0.0, 1.0)
            };
            if binding.flags & 0x02 != 0 {
                blend = 1.0 - blend;
            }
            times[binding.node_index] = Some(AnimationTime(
                (1.0 - blend) * binding.frame_a + blend * binding.frame_b,
            ));
        }
        times
    }

    fn nodes_for_hierarchy(
        model: &ModelAsset,
        poses: &[fparkan_animation::Pose],
        root_from_body: [f32; 16],
        owner_scale: [f32; 3],
    ) -> Vec<BuildingMeshNode> {
        use fparkan_msh::{selected_slot, Group, Lod};

        let scale = scale_matrix(owner_scale);
        (0..model.node_count)
            .filter_map(|index| {
                let node = NodeId(u32::try_from(index).expect("model node id fits u32"));
                let slot = selected_slot(model, node, Lod(0), Group(0))?;
                let slot_data = &model.slots[usize::try_from(slot.0).expect("slot id fits usize")];
                let batch_start = usize::from(slot_data.batch_start);
                let batch_end = batch_start + usize::from(slot_data.batch_count);
                if !model.batches[batch_start..batch_end]
                    .iter()
                    .any(|batch| batch.index_count != 0)
                {
                    return None;
                }
                let node_pose = pose_matrix(*poses.get(index).expect("Node38 pose available"));
                let body_from_node = compose_row_major(scale, node_pose);
                let world_from_node = compose_row_major(root_from_body, body_from_node);
                Some(BuildingMeshNode {
                    node,
                    slot,
                    world_from_node,
                })
            })
            .collect()
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn factory_plant_world_query_preserves_native_fixture_hit_under_affine_transforms() {
        use fparkan_assets::decode_nres_payload;
        use fparkan_msh::{
            decode_msh, node38_fallback_hierarchy, selected_slot, validate_msh, Group, Lod, NodeId,
            SlotId,
        };
        use std::{path::PathBuf, sync::Arc};

        const BODY_WALL: [[f32; 3]; 2] = [[-10.5, 22.16, 2.5], [-10.5, 19.16, 2.5]];
        const BODY_DOOR: [[f32; 3]; 2] = [[0.5, 22.16, 2.5], [0.5, 19.16, 2.5]];
        const BODY_REVERSED_WALL: [[f32; 3]; 2] = [BODY_WALL[1], BODY_WALL[0]];
        const BODY_HIT: [f32; 3] = [-10.5, 20.16, 2.5];
        const ROTATED_SCALED_TRANSLATED: [f32; 16] = [
            0.0, -3.0, 0.0, 7.0, 2.0, 0.0, 0.0, -4.0, 0.0, 0.0, 0.5, 2.0, 0.0, 0.0, 0.0, 1.0,
        ];

        let root = PathBuf::from(
            std::env::var_os("FPARKAN_CORPUS_PART1_ROOT")
                .expect("FPARKAN_CORPUS_PART1_ROOT is required for licensed corpus tests"),
        );
        let archive_bytes = std::fs::read(root.join("fortif.rlb")).expect("fortif.rlb bytes");
        let archive = decode_nres_payload(Arc::from(archive_bytes.into_boxed_slice()))
            .expect("fortif.rlb archive");
        let entry = archive
            .entries()
            .iter()
            .find(|entry| entry.name_bytes().eq_ignore_ascii_case(b"fr_l_plant.msh"))
            .expect("plant factory MSH");
        let nested = decode_nres_payload(Arc::from(
            archive
                .payload(entry.id())
                .expect("MSH payload")
                .to_vec()
                .into_boxed_slice(),
        ))
        .expect("nested plant MSH");
        let model = validate_msh(&decode_msh(&nested).expect("plant MSH document"))
            .expect("plant MSH model");
        let node_id = NodeId(1);
        let slot = selected_slot(&model, node_id, Lod(0), Group(0)).expect("Node 1 slot");
        assert_eq!(slot, SlotId(0));

        let pose = node38_fallback_hierarchy(&model)
            .expect("Node38 fallback hierarchy")
            .poses[1];
        assert_eq!(pose.rotation, [0.0, 0.0, 0.0, 1.0]);
        let body_from_node = translated(
            pose.translation[0],
            pose.translation[1],
            pose.translation[2],
        );

        for body_to_world in [IDENTITY, ROTATED_SCALED_TRANSLATED] {
            let world_from_node = compose_row_major(body_to_world, body_from_node);
            let node = BuildingMeshNode {
                node: node_id,
                slot,
                world_from_node,
            };
            let to_world = |point| {
                super::transform_point(body_to_world, point).expect("finite fixture transform")
            };
            let world_wall = [to_world(BODY_WALL[0]), to_world(BODY_WALL[1])];
            let expected_hit = to_world(BODY_HIT);
            let hit = query_building_segment_world(&model, &[node], world_wall)
                .expect("valid factory wall query")
                .expect("solid factory wall hit");

            assert_eq!(hit.node, node_id);
            assert_eq!(hit.slot, slot);
            assert_eq!(hit.batch_index, 5);
            assert_eq!(hit.triangle_descriptor_index, 62);
            for axis in 0..3 {
                assert!(
                    (hit.position[axis] - expected_hit[axis]).abs() <= 1.0e-3,
                    "world hit {:?} != {:?}",
                    hit.position,
                    expected_hit
                );
            }
            let world_delta = [
                expected_hit[0] - world_wall[0][0],
                expected_hit[1] - world_wall[0][1],
                expected_hit[2] - world_wall[0][2],
            ];
            let expected_distance_squared = super::dot3(world_delta, world_delta);
            assert!(
                (hit.distance_squared - expected_distance_squared).abs() <= 1.0e-2,
                "world distance squared {} != {}",
                hit.distance_squared,
                expected_distance_squared
            );

            for body_segment in [BODY_DOOR, BODY_REVERSED_WALL] {
                let world_segment = [to_world(body_segment[0]), to_world(body_segment[1])];
                assert!(query_building_segment_world(&model, &[node], world_segment)
                    .expect("valid factory miss query")
                    .is_none());
            }
        }
    }

    #[test]
    #[ignore = "requires licensed AutoDemo corpus"]
    fn autodemo_portal_uses_node3_authored_closed_and_open_poses() {
        use crate::{
            create, load_mission_static_preview_roots, loaded_mission_assets,
            loaded_mission_object_drafts, EngineServices, MissionMode, MissionRequest,
        };
        use fparkan_animation::AnimationTime;
        use fparkan_msh::{node38_fallback_hierarchy, node38_sampled_hierarchy_at_times};
        use fparkan_render::LegacyIron3dEulerTransform;
        use fparkan_vfs::DirectoryVfs;
        use std::{num::NonZeroUsize, path::PathBuf, sync::Arc};

        const OBJECT_INDEX: usize = 4;
        const BODY_DOOR: [[f32; 3]; 2] = [[0.5, 22.16, 2.5], [0.5, 19.16, 2.5]];

        let root = PathBuf::from(
            std::env::var_os("FPARKAN_CORPUS_PART1_ROOT")
                .expect("FPARKAN_CORPUS_PART1_ROOT is required for licensed corpus tests"),
        );
        let mut engine = create(EngineServices::new(Arc::new(DirectoryVfs::new(&root))))
            .expect("create SingleMission fixture engine");
        load_mission_static_preview_roots(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/Autodemo.00/data.tma".to_string(),
                mode: MissionMode::SingleMission,
            },
            NonZeroUsize::MAX,
        )
        .expect("load SingleMission preview roots");
        let assets = loaded_mission_assets(&engine).expect("loaded mission assets");
        let drafts = loaded_mission_object_drafts(&engine).expect("loaded object drafts");
        let draft = &drafts[OBJECT_INDEX];
        assert!(draft
            .resource_name_raw
            .eq_ignore_ascii_case(b"UNITS\\BUILDS\\PLANT\\wor_l_n1.dat"));
        let visual_id = *assets
            .visuals_for_object(OBJECT_INDEX)
            .first()
            .expect("PLANT root visual");
        let visual = assets.visual_by_id(visual_id).expect("PLANT visual");
        let model_id = visual.model_id.expect("PLANT MSH model");
        let model = &assets
            .model_by_id(model_id)
            .expect("prepared PLANT model")
            .validated;
        assert_eq!(model.node_count, 29);

        let controls = assets
            .object_component_controls
            .get(OBJECT_INDEX)
            .and_then(|components| components.first())
            .and_then(Option::as_ref)
            .expect("PLANT component CTLD bindings");
        let node3_binding = controls
            .bindings
            .iter()
            .find(|binding| binding.node_index == 3)
            .expect("Node3 CTLD binding");
        assert_eq!(node3_binding.frame_a, 0.0);
        assert_eq!(node3_binding.frame_b, 1.0);
        assert_eq!(node3_binding.initial_blend, 0.0);

        let mut node_times = node_times_from_bindings(model.node_count, &controls.bindings);
        let closed_hierarchy = node38_sampled_hierarchy_at_times(model, &node_times)
            .or_else(|| node38_fallback_hierarchy(model))
            .expect("initial Node38 hierarchy");
        assert_eq!(node_times[3], Some(AnimationTime(0.0)));

        let root_transform = LegacyIron3dEulerTransform {
            translation: draft.position,
            orientation_radians: draft.orientation_raw,
        };
        let root_from_body = root_transform
            .try_row_major()
            .expect("finite PLANT root matrix");
        let world_door = BODY_DOOR.map(|point| {
            root_transform
                .try_transform_scaled_point(point, draft.scale)
                .expect("finite BODY_DOOR world point")
        });
        let closed_nodes =
            nodes_for_hierarchy(model, &closed_hierarchy.poses, root_from_body, draft.scale);
        let closed = query_building_segment_world(model, &closed_nodes, world_door)
            .expect("closed-door building query")
            .expect("authored time-0 doorway blocker");
        assert_eq!(closed.node, NodeId(3));
        assert_eq!(closed.slot, SlotId(25));
        assert_eq!(closed.batch_index, 100);
        assert_eq!(closed.triangle_descriptor_index, 855);
        assert_eq!(model.batches[closed.batch_index].batch_flags, 0x2000);

        node_times[3] = Some(AnimationTime(node3_binding.frame_b));
        let open_hierarchy = node38_sampled_hierarchy_at_times(model, &node_times)
            .expect("authored Node3 time-1 hierarchy");
        let open_nodes =
            nodes_for_hierarchy(model, &open_hierarchy.poses, root_from_body, draft.scale);
        assert!(query_building_segment_world(model, &open_nodes, world_door)
            .expect("time-1 building query")
            .is_none());
    }
}
