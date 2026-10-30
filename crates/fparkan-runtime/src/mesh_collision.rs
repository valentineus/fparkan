//! World-space segment queries over selected building MSH nodes and slots.

use fparkan_msh::{query_slot_segment, ModelAsset, MshError, NodeId, SegmentQueryMasks, SlotId};
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
/// the native building camera does.
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
    let mut intersection_visits = 0;
    loop {
        let mut nearest = None;
        for node in &prepared_nodes {
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
        segment_start = nearest.position;
    }
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
    use fparkan_msh::{Batch, ModelAsset, MshError, NodeId, Slot, SlotId, TriangleDescriptor};

    const IDENTITY: [f32; 16] = [
        1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ];

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
        let mut positions = Vec::new();
        let mut indices = Vec::new();
        let mut batches = Vec::new();
        let mut triangle_descriptors = Vec::new();
        let mut slots = Vec::new();
        for planes in planes_per_slot {
            let descriptor_start = triangle_descriptors.len();
            let batch_start = batches.len();
            for (y, batch_flags) in *planes {
                let base_vertex = positions.len();
                let index_start = indices.len();
                positions.extend([[-2.0, *y, -2.0], [0.0, *y, 2.0], [2.0, *y, -2.0]]);
                indices.extend([0, 1, 2]);
                batches.push(Batch {
                    batch_flags: *batch_flags,
                    material_index_hi: 0,
                    material_index: 0,
                    lightmap_index: 0,
                    local_batch_index: u16::try_from(batches.len() - batch_start)
                        .expect("small synthetic batch index"),
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
                aabb_min: [-2.0, -10.0, -2.0],
                aabb_max: [2.0, 10.0, 2.0],
                sphere_center: [0.0; 3],
                sphere_radius: 0.0,
                opaque: [0; 5],
            });
        }

        ModelAsset {
            node_stride: 38,
            node_count: 2,
            nodes_raw: vec![0; 76],
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
        let model = model_with_slots(&[&helper_plane, &wall_plane]);
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
        let model = model_with_planes(&[(0.0, 0), (0.0, 0x0008)]);
        let result = query_building_segment_world(
            &model,
            &[node(0, IDENTITY)],
            [[0.0, 3.0, 0.0], [0.0, -1.0, 0.0]],
        )
        .expect("valid world query");

        assert!(result.is_none());
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
        let result = query_building_segment_world(
            &model_with_planes(&planes),
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
        let result = query_building_segment_world(
            &model_with_planes(&planes),
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
}
