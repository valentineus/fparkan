use super::*;
use fparkan_runtime::{mission_basement_footprint, SpawnedUnit};
use fparkan_terrain::TerrainLodPlan;
use fparkan_terrain_format::LandMeshDocument;

pub(super) struct LiveBasements {
    footprints: Vec<MissionBasementFootprint>,
    face_slots: Vec<Option<usize>>,
    near_slot_count: usize,
    band_slot: usize,
    terrain_ranges: Vec<usize>,
    geometry_base: Option<(usize, usize)>,
    upload: Vec<VulkanDynamicDrawRange>,
}

impl LiveBasements {
    pub(super) fn prepare(
        land: &LandMeshDocument,
        terrain: &TerrainWorld,
        mesh: &mut VulkanStaticMesh,
        footprints: Vec<MissionBasementFootprint>,
    ) -> Result<Self, String> {
        // The near projection emits one triangle per foundation fragment, in
        // order. A later fragment inherits the draw slot of its source face.
        let source_faces = match terrain.basement_foundation() {
            Some(foundation) => foundation.fragments.iter().map(|f| f.source_face).collect(),
            None => land
                .face_lod_assignments()
                .map_err(|error| error.to_string())?
                .iter()
                .filter(|face| face.lod == TerrainLod::Lod0)
                .map(|face| Some(face.face_index))
                .collect::<Vec<_>>(),
        };
        let mut face_slots = vec![None; land.faces.len()];
        let mut band_slot = None;
        let mut near_slot_count = 0;
        for (slot, range) in mesh.draw_ranges.iter().enumerate() {
            let start = range.first_index as usize / 3;
            if start >= source_faces.len() {
                break;
            }
            let end = start + range.index_count as usize / 3;
            for &source_face in source_faces
                .get(start..end)
                .ok_or("terrain draw crosses the end of the near projection")?
            {
                if let Some(face) = source_face {
                    face_slots[face].get_or_insert(slot);
                } else {
                    band_slot.get_or_insert(slot);
                }
            }
            near_slot_count = slot + 1;
        }
        let band_slot = match band_slot {
            Some(slot) => slot,
            None => {
                let template = *mesh
                    .draw_ranges
                    .first()
                    .ok_or("terrain has no draw ranges")?;
                let slot = mesh.draw_ranges.len();
                mesh.draw_ranges.push(VulkanStaticDrawRange {
                    first_index: 0,
                    index_count: 0,
                    material_index: 0xff00,
                    terrain_base_only: true,
                    terrain_reflection: false,
                    ..template
                });
                slot
            }
        };
        Ok(Self {
            footprints,
            face_slots,
            near_slot_count,
            band_slot,
            terrain_ranges: Vec::new(),
            geometry_base: None,
            upload: Vec::new(),
        })
    }

    pub(super) fn bind_scene(&mut self, mesh: &VulkanStaticMesh) {
        self.terrain_ranges = mesh
            .draw_ranges
            .iter()
            .enumerate()
            .filter_map(|(index, range)| range.terrain_surface.then_some(index))
            .collect();
    }

    fn rebuild(
        &self,
        terrain: &TerrainWorld,
        footprint: &MissionBasementFootprint,
    ) -> Result<(BasementFoundation, VulkanStaticMesh, Vec<ShadowTriangle>), String> {
        let land = terrain.source_mesh().ok_or("live BAS needs Land.msh")?;
        let rings = self
            .footprints
            .iter()
            .chain(std::iter::once(footprint))
            .map(MissionBasementFootprint::contours)
            .collect::<Vec<_>>();
        let mut foundation = build_basement_foundations(land, &rings).map_err(|e| e.to_string())?;
        let source = project_land_msh_to_static_mesh_in_world_space(land)
            .map_err(|error| error.to_string())?;
        let (mesh, shadows) = project_basement_foundation(land, &source, &mut foundation)?;
        Ok((foundation, mesh, shadows))
    }

    fn draw_upload(
        &self,
        foundation: &BasementFoundation,
        projected: &VulkanStaticMesh,
        vertex_base: usize,
        index_base: usize,
    ) -> Result<(Vec<u32>, Vec<VulkanDynamicDrawRange>), String> {
        let mut batches = vec![Vec::new(); self.near_slot_count.max(self.band_slot + 1)];
        if projected.indices.len() != foundation.fragments.len() * 3 {
            return Err("live BAS projection lost a fragment".into());
        }
        for (tri, fragment) in projected.indices.chunks_exact(3).zip(&foundation.fragments) {
            let slot = match fragment.source_face {
                Some(face) => {
                    self.face_slots[face].ok_or("live BAS source face has no draw slot")?
                }
                None => self.band_slot,
            };
            batches[slot].extend_from_slice(tri);
        }
        let mut indices = Vec::with_capacity(projected.indices.len());
        let mut ranges = Vec::new();
        let slots = (0..self.near_slot_count)
            .chain((self.band_slot >= self.near_slot_count).then_some(self.band_slot));
        let vertex_base = u32::try_from(vertex_base).map_err(|e| e.to_string())?;
        for slot in slots {
            let batch = &batches[slot];
            ranges.push(VulkanDynamicDrawRange {
                range_index: self.terrain_ranges[slot],
                first_index: u32::try_from(index_base + indices.len())
                    .map_err(|e| e.to_string())?,
                index_count: u32::try_from(batch.len()).map_err(|e| e.to_string())?,
            });
            for &index in batch {
                indices.push(
                    vertex_base
                        .checked_add(index)
                        .ok_or("live BAS vertex index overflow")?,
                );
            }
        }
        Ok((indices, ranges))
    }
}

impl StaticVulkanApp {
    pub(super) fn cut_constructed_basements(&mut self) {
        let Some(mut basements) = self.live_basements.take() else {
            return;
        };
        let births = self
            .mission_frame
            .as_ref()
            .map(|frame| frame.constructed_buildings.clone())
            .unwrap_or_default();
        for building in births {
            if let Err(reason) = self.cut_constructed_basement(&mut basements, building) {
                eprintln!(
                    "render_coverage live_basement_stub=1 object={} reason={reason}",
                    building.object_index
                );
            }
        }
        self.live_basements = Some(basements);
    }

    fn cut_constructed_basement(
        &mut self,
        basements: &mut LiveBasements,
        building: SpawnedUnit,
    ) -> Result<(), String> {
        let engine = self.mission_engine.as_mut().ok_or("missing mission")?;
        let frame = self
            .mission_frame
            .as_mut()
            .ok_or("missing construction frame")?;
        let pose =
            live_root_pose(building.handle, &frame.snapshot).ok_or("missing building pose")?;
        let mut root = loaded_mission_object_drafts(engine).ok_or("missing built root")?
            [building.object_index]
            .clone();
        root.position = pose.position.map(f32::from_bits);
        root.orientation_raw = pose.orientation.map(f32::from_bits);
        root.scale = pose.scale.map(f32::from_bits);
        let assets = loaded_mission_assets(engine).ok_or("missing building assets")?;
        let Some(footprint) = mission_basement_footprint(assets, building.object_index, &root)?
        else {
            return Ok(());
        };
        let (foundation, projected, receivers) = basements.rebuild(&self.terrain, &footprint)?;
        let elevation = root.root_position_with_basement_delta(
            foundation
                .placements
                .last()
                .expect("new foundation placement")
                .placement_delta,
        )[2];
        let (vertex_base, index_base) = basements
            .geometry_base
            .unwrap_or((self.shadow_base_vertex, self.shadow_base_index));
        let (indices, upload) =
            basements.draw_upload(&foundation, &projected, vertex_base, index_base)?;
        let plan = self
            .terrain_lod
            .as_ref()
            .map(|_| {
                TerrainLodPlan::new(
                    self.terrain.source_mesh().expect("rebuild used Land.msh"),
                    Some(&foundation),
                )
                .map_err(|error| error.to_string())
            })
            .transpose()?;
        let terrain = self
            .terrain
            .as_ref()
            .clone()
            .with_basement_foundation(foundation)
            .map_err(|error| error.to_string())?;
        fparkan_runtime::place_constructed_basement(engine, frame, building, elevation)
            .map_err(|error| error.to_string())?;

        self.terrain = Arc::new(terrain);
        self.shadow_receivers = receivers;
        if let (Some(lod), Some(plan)) = (&mut self.terrain_lod, plan) {
            lod.replace_plan(plan);
        }
        self.mesh.vertices.truncate(vertex_base);
        self.mesh.indices.truncate(index_base);
        self.mesh.vertices.extend(projected.vertices);
        self.mesh.indices.extend(indices);
        self.shadow_base_vertex = self.mesh.vertices.len();
        self.shadow_base_index = self.mesh.indices.len();
        for range in &upload {
            let draw = &mut self.mesh.draw_ranges[range.range_index];
            draw.first_index = range.first_index;
            draw.index_count = range.index_count;
            self.world_range_index_counts
                .insert(range.range_index, range.index_count);
        }
        basements.geometry_base = Some((vertex_base, index_base));
        basements.footprints.push(footprint);
        basements.upload = upload;
        eprintln!(
            "live_basement_cut object={} elevation={elevation}",
            building.object_index
        );
        Ok(())
    }

    pub(super) fn upload_live_basements(&mut self) -> Result<(), String> {
        let (Some(renderer), Some(basements)) = (&mut self.renderer, &mut self.live_basements)
        else {
            return Ok(());
        };
        if basements.upload.is_empty() {
            return Ok(());
        }
        // The old shadow indices are in the replaced suffix. The normal shadow
        // pass assigns its new segment before presenting this frame.
        let mut ranges = basements.upload.clone();
        ranges.push(VulkanDynamicDrawRange {
            range_index: self.shadow_range.range_index,
            first_index: u32::try_from(self.shadow_base_index).map_err(|e| e.to_string())?,
            index_count: 0,
        });
        let (_, index_base) = basements.geometry_base.expect("pending live BAS upload");
        renderer
            .update_mesh_geometry(
                &self.mesh.vertices,
                &self.mesh.indices,
                &ranges,
                self.dynamic_vertex_start,
                index_base,
            )
            .map_err(|error| format!("upload live building foundation: {error}"))?;
        basements.upload.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_terrain::BasementFoundationFragment;
    use fparkan_terrain_format::FullSurfaceMask;

    #[test]
    fn rebuilt_faces_keep_their_terrain_draw_slots() {
        let basements = LiveBasements {
            footprints: Vec::new(),
            face_slots: vec![Some(1), Some(0)],
            near_slot_count: 2,
            band_slot: 3,
            terrain_ranges: vec![5, 6, 7, 8],
            geometry_base: None,
            upload: Vec::new(),
        };
        let foundation = BasementFoundation {
            fragments: [Some(0), Some(1), None, Some(0)]
                .into_iter()
                .map(|source_face| BasementFoundationFragment {
                    source_face,
                    surface_mask: FullSurfaceMask(0),
                    positions: [[0.0; 3]; 3],
                })
                .collect(),
            placements: Vec::new(),
        };
        let projected = VulkanStaticMesh {
            vertices: Vec::new(),
            indices: (0..12).collect(),
            draw_ranges: Vec::new(),
        };

        let (indices, ranges) = basements
            .draw_upload(&foundation, &projected, 10, 300)
            .unwrap();

        assert_eq!(indices, [13, 14, 15, 10, 11, 12, 19, 20, 21, 16, 17, 18]);
        assert_eq!(
            ranges
                .iter()
                .map(|range| (range.range_index, range.first_index, range.index_count))
                .collect::<Vec<_>>(),
            [(5, 300, 3), (6, 303, 6), (8, 309, 3)]
        );
    }
}
