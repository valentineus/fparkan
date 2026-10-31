use fparkan_render_vulkan::{
    project_land_msh_lod_to_static_mesh_in_world_space, VulkanStaticDrawRange, VulkanStaticMesh,
};
use fparkan_terrain::{BasementFoundation, TerrainLod, TerrainLodPlan};
use fparkan_terrain_format::LandMeshDocument;

pub(super) struct TerrainLodDraws {
    plan: TerrainLodPlan,
    ranges: Vec<(Option<usize>, TerrainLod)>,
}

impl TerrainLodDraws {
    pub(super) fn index_count(
        &self,
        range: usize,
        count: u32,
        eye: [f32; 3],
        fov: f32,
    ) -> Result<u32, String> {
        let Some(&(Some(node), lod)) = self.ranges.get(range) else {
            return Ok(count);
        };
        let selected = self
            .plan
            .selected_lod(node, [eye[0], eye[1]], fov)
            .ok_or_else(|| "invalid terrain LOD camera or cell".to_string())?;
        Ok(if selected == lod { count } else { 0 })
    }

    pub(super) fn report(&self, eye: [f32; 3], fov: f32) -> Result<(), String> {
        let mut counts = [0; 2];
        for (range, &(_, lod)) in self.ranges.iter().enumerate() {
            if self.index_count(range, 1, eye, fov)? != 0 {
                counts[usize::from(lod == TerrainLod::Lod1)] += 1;
            }
        }
        eprintln!(
            "render_coverage terrain_lod_selected_ranges lod0={} lod1={}",
            counts[0], counts[1]
        );
        Ok(())
    }
}

pub(super) fn prepare(
    terrain: &LandMeshDocument,
    mut near_mesh: VulkanStaticMesh,
    foundation: Option<&BasementFoundation>,
) -> Result<(VulkanStaticMesh, Option<TerrainLodDraws>), String> {
    let assignments = terrain.face_lod_assignments().map_err(|e| e.to_string())?;
    if !assignments.iter().any(|a| a.lod == TerrainLod::Lod1) {
        return Ok((near_mesh, None));
    }
    let plan = TerrainLodPlan::new(terrain, foundation)
        .map_err(|e| format!("prepare terrain LOD: {e}"))?;
    let near_nodes = if let Some(foundation) = foundation {
        foundation
            .fragments
            .iter()
            .map(|fragment| {
                fragment
                    .source_face
                    .map(|face| {
                        plan.source_face_node(face)
                            .ok_or_else(|| "foundation LOD source face is missing".to_string())
                    })
                    .transpose()
            })
            .collect::<Result<Vec<_>, _>>()?
    } else {
        assignments
            .iter()
            .filter(|a| a.lod == TerrainLod::Lod0)
            .map(|a| Some(a.node_index))
            .collect()
    };
    let mut ranges = split_ranges(&mut near_mesh, &near_nodes, TerrainLod::Lod0)?;
    if plan.has_far_geometry() {
        let mut far_mesh =
            project_land_msh_lod_to_static_mesh_in_world_space(terrain, TerrainLod::Lod1)
                .map_err(|e| format!("project far terrain: {e}"))?;
        let far_nodes = assignments
            .iter()
            .filter(|a| a.lod == TerrainLod::Lod1)
            .map(|a| Some(a.node_index))
            .collect::<Vec<_>>();
        ranges.extend(split_ranges(&mut far_mesh, &far_nodes, TerrainLod::Lod1)?);
        let vertex_offset = u32::try_from(near_mesh.vertices.len())
            .map_err(|_| "terrain LOD vertex offset exceeds u32".to_string())?;
        let index_offset = u32::try_from(near_mesh.indices.len())
            .map_err(|_| "terrain LOD index offset exceeds u32".to_string())?;
        for index in &mut far_mesh.indices {
            *index = index
                .checked_add(vertex_offset)
                .ok_or_else(|| "far terrain vertex index overflows".to_string())?;
        }
        for range in &mut far_mesh.draw_ranges {
            range.first_index = range
                .first_index
                .checked_add(index_offset)
                .ok_or_else(|| "far terrain draw range overflows".to_string())?;
        }
        near_mesh.vertices.extend(far_mesh.vertices);
        near_mesh.indices.extend(far_mesh.indices);
        near_mesh.draw_ranges.extend(far_mesh.draw_ranges);
    }
    Ok((near_mesh, Some(TerrainLodDraws { plan, ranges })))
}

fn split_ranges(
    mesh: &mut VulkanStaticMesh,
    nodes: &[Option<usize>],
    lod: TerrainLod,
) -> Result<Vec<(Option<usize>, TerrainLod)>, String> {
    if mesh.indices.len() / 3 != nodes.len() || !mesh.indices.len().is_multiple_of(3) {
        return Err("terrain LOD triangle metadata does not match the index buffer".to_string());
    }
    let mut ranges = Vec::<VulkanStaticDrawRange>::new();
    let mut bindings = Vec::new();
    for &range in &mesh.draw_ranges {
        if !range.first_index.is_multiple_of(3) || !range.index_count.is_multiple_of(3) {
            return Err("terrain LOD range is not triangle aligned".to_string());
        }
        let first = range.first_index as usize / 3;
        let end = first
            .checked_add(range.index_count as usize / 3)
            .filter(|&end| end <= nodes.len())
            .ok_or_else(|| "terrain LOD range is outside triangle metadata".to_string())?;
        let mut start = first;
        while start < end {
            let node = nodes[start];
            let mut next = start + 1;
            while next < end && nodes[next] == node {
                next += 1;
            }
            let mut split = range;
            split.first_index =
                u32::try_from(start * 3).map_err(|_| "terrain LOD index overflows".to_string())?;
            split.index_count = u32::try_from((next - start) * 3)
                .map_err(|_| "terrain LOD count overflows".to_string())?;
            ranges.push(split);
            bindings.push((node, lod));
            start = next;
        }
    }
    mesh.draw_ranges = ranges;
    Ok(bindings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_render_vulkan::VulkanStaticVertex;

    #[test]
    #[ignore = "requires licensed GOG Constructor resources"]
    fn gog_constructor_lod0_cells_match_native_basement_mask() {
        use fparkan_runtime::*;
        use std::{num::NonZeroUsize, sync::Arc};
        let root = std::env::var_os("FPARKAN_GAME_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(r"C:\GOG Games\Parkan - Iron Strategy"));
        let mut engine = create(EngineServices::new(Arc::new(
            fparkan_vfs::DirectoryVfs::new(root),
        )))
        .unwrap();
        load_mission_static_preview_roots(
            &mut engine,
            MissionRequest {
                key: r"MISSIONS\CAMPAIGN\CAMPAIGN.00\Mission.02\data.tma".to_string(),
                mode: MissionMode::SingleMission,
            },
            NonZeroUsize::new(usize::MAX).unwrap(),
        )
        .unwrap();
        let assets = loaded_mission_assets(&engine).unwrap();
        let roots = loaded_mission_object_drafts(&engine).unwrap();
        let terrain = loaded_terrain(&engine).unwrap().source_mesh().unwrap();
        let footprints = super::super::mission_basement_footprints(assets, roots).unwrap();
        let borrowed = footprints
            .iter()
            .map(|f| fparkan_terrain::BasementFootprint {
                outer: &f.outer,
                inner: &f.inner,
                flatten_inner_to_mean_outer_z: f.flatten_inner_to_mean_outer_z,
            })
            .collect::<Vec<_>>();
        let mut foundation =
            fparkan_terrain::build_basement_foundations(terrain, &borrowed).unwrap();
        let source =
            fparkan_render_vulkan::project_land_msh_to_static_mesh_in_world_space(terrain).unwrap();
        let (near, _) =
            super::super::project_basement_foundation(terrain, &source, &mut foundation).unwrap();
        let plan = TerrainLodPlan::new(terrain, Some(&foundation)).unwrap();
        let forced = (0..terrain.nodes_raw.len() / 38)
            .filter(|&node| plan.selected_lod(node, [-10_000.0; 2], 1.3) == Some(TerrainLod::Lod0))
            .collect::<Vec<_>>();
        // GOG Terrain.dll (hash in original-binaries.md), live Render +0x11A19:
        // cells whose patch list at Render-this +0x7A30 has a nonzero count.
        assert_eq!(
            forced,
            [
                54, 55, 56, 57, 70, 71, 72, 73, 86, 87, 113, 114, 115, 129, 130, 131, 146, 147,
                183, 184, 199, 200
            ]
        );
        let (mesh, draws) = prepare(terrain, near, Some(&foundation)).unwrap();
        let draws = draws.unwrap();
        let mut checked = [false; 2];
        for (range, &(node, lod)) in draws.ranges.iter().enumerate() {
            if node != Some(40) {
                continue;
            }
            let count = mesh.draw_ranges[range].index_count;
            // Native cell 40 selects far at camera cell [2, 5], near at [2, 7].
            assert_eq!(
                draws
                    .index_count(range, count, [300.42047, 556.282, 155.91914], 1.3)
                    .unwrap(),
                if lod == TerrainLod::Lod1 { count } else { 0 }
            );
            assert_eq!(
                draws
                    .index_count(range, count, [265.0, 795.0, 155.91914], 1.3)
                    .unwrap(),
                if lod == TerrainLod::Lod0 { count } else { 0 }
            );
            checked[usize::from(lod == TerrainLod::Lod1)] = true;
        }
        assert_eq!(checked, [true, true]);
    }

    #[test]
    fn cell_ranges_keep_material_state_and_separate_generated_bands() {
        let range = VulkanStaticDrawRange {
            first_index: 0,
            index_count: 12,
            material_index: 7,
            lightmap_index: u8::MAX,
            batch_flags: 0,
            terrain_base_only: true,
            pipeline_state: super::super::world_pipeline_state(
                fparkan_render::LegacyBlendMode::Opaque,
                fparkan_render::LegacyDepthMode::TestWrite,
            ),
            alpha_test_reference: VulkanStaticDrawRange::NATIVE_DEFAULT_ALPHA_TEST_REFERENCE,
        };
        let mut mesh = VulkanStaticMesh {
            vertices: [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]
                .map(|position| VulkanStaticVertex {
                    position,
                    color: [1.0; 3],
                    normal: [0.0, 0.0, 1.0],
                    uv: [0.0; 2],
                    detail_uv: [0.0; 2],
                    overlay_alpha: 0.0,
                })
                .to_vec(),
            indices: vec![0, 1, 2, 0, 1, 2, 0, 1, 2, 0, 1, 2],
            draw_ranges: vec![range],
        };
        let nodes = [Some(4), Some(4), None, Some(5)];
        let bindings = split_ranges(&mut mesh, &nodes, TerrainLod::Lod0).unwrap();
        assert_eq!(
            bindings,
            vec![
                (Some(4), TerrainLod::Lod0),
                (None, TerrainLod::Lod0),
                (Some(5), TerrainLod::Lod0)
            ]
        );
        assert_eq!(
            mesh.draw_ranges
                .iter()
                .map(|r| (r.first_index, r.index_count))
                .collect::<Vec<_>>(),
            vec![(0, 6), (6, 3), (9, 3)]
        );
        for split in &mesh.draw_ranges {
            assert_eq!(split.material_index, 7);
            assert_eq!(split.pipeline_state, range.pipeline_state);
            assert!(split.terrain_base_only);
        }
        assert!(split_ranges(&mut mesh, &nodes[..3], TerrainLod::Lod0).is_err());
    }
}
