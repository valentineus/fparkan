//! Terrain support inferred from the chassis's initial rendered geometry.

use crate::{initial_component_node_times, EngineError, MissionAssets, TransformState};
use fparkan_msh::{
    draw_batches, node38_fallback_hierarchy, node38_sampled_hierarchy_at_times, selected_slot,
    Group, Lod, NodeId,
};
use fparkan_render::LegacyIron3dEulerTransform;

#[derive(Clone, Copy, Debug)]
pub(super) struct GroundSupport {
    // One lowest visible mesh point per quadrant of the chassis. A central
    // ground sample alone can miss the ground under an outboard foot.
    contacts: [Option<[f32; 3]>; 4],
}

impl GroundSupport {
    pub(super) fn prepare(assets: &MissionAssets, index: usize) -> Option<Self> {
        let visual = assets.visual_by_id(assets.visual_for_object(index)?)?;
        let model = &assets.model_by_id(visual.model_id?)?.validated;
        let bindings = assets
            .object_component_controls
            .get(index)
            .and_then(|controls| controls.first())
            .and_then(Option::as_ref)
            .map_or(&[][..], |control| control.bindings.as_slice());
        let hierarchy = initial_component_node_times(model.node_count, bindings)
            .ok()
            .and_then(|times| node38_sampled_hierarchy_at_times(model, &times))
            .or_else(|| node38_fallback_hierarchy(model))?;
        let mut contacts: [Option<[f32; 3]>; 4] = [None; 4];
        for (index, pose) in hierarchy.poses.into_iter().enumerate() {
            let Some(slot) = selected_slot(model, NodeId(index as u32), Lod(0), Group(0)) else {
                continue;
            };
            for batch in draw_batches(model, slot).ok()? {
                let start = batch.index_start as usize;
                for &vertex in model
                    .indices
                    .get(start..start + usize::from(batch.index_count))?
                {
                    let point = pose.transform_point(
                        *model
                            .positions
                            .get(batch.base_vertex as usize + usize::from(vertex))?,
                    );
                    if !point.into_iter().all(f32::is_finite) {
                        continue;
                    }
                    let quadrant = usize::from(point[0] >= 0.0) + 2 * usize::from(point[1] >= 0.0);
                    if contacts[quadrant].is_none_or(|other| point[2] < other[2]) {
                        contacts[quadrant] = Some(point);
                    }
                }
            }
        }
        contacts
            .iter()
            .any(Option::is_some)
            .then_some(Self { contacts })
    }

    /// Lowest world Z that keeps the sampled chassis contacts above terrain.
    pub(super) fn height(
        self,
        xy: [f32; 2],
        pose: TransformState,
        height_at: &mut impl FnMut([f32; 2]) -> Result<Option<f32>, EngineError>,
    ) -> Result<Option<f32>, EngineError> {
        let world = LegacyIron3dEulerTransform {
            translation: [xy[0], xy[1], 0.0],
            orientation_radians: pose.orientation.map(f32::from_bits),
        };
        let scale = pose.scale.map(f32::from_bits);
        let mut root = None::<f32>;
        for contact in self.contacts.into_iter().flatten() {
            let Some([x, y, offset]) = world.try_transform_scaled_point(contact, scale) else {
                continue;
            };
            if let Some(ground) = height_at([x, y])? {
                let z = ground - offset;
                root = Some(root.map_or(z, |other| other.max(z)));
            }
        }
        Ok(root)
    }

    pub(super) fn reanchor(
        self,
        before: TransformState,
        after: &mut TransformState,
        mut height_at: impl FnMut([f32; 2]) -> Result<Option<f32>, EngineError>,
    ) -> Result<(), EngineError> {
        let [x, y, z] = before.position.map(f32::from_bits);
        let [nx, ny, nz] = after.position.map(f32::from_bits);
        let previous_support = self.height([x, y], before, &mut height_at)?;
        let next_support = if [x, y] == [nx, ny] && before.orientation == after.orientation {
            previous_support
        } else {
            self.height([nx, ny], *after, &mut height_at)?
        };
        if let Some(next_support) = next_support {
            let altitude = previous_support.map_or(nz.max(next_support), |height| {
                next_support + (z - height).max(0.0)
            });
            after.position[2] = altitude.to_bits();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        create, frame, load_gameplay_mission, EngineServices, MissionMode, MissionRequest,
        MissionStartupInput,
    };
    use fparkan_terrain::SurfaceQuery;
    use fparkan_vfs::DirectoryVfs;
    use fparkan_world::{handle_by_original_id, transform_state, OriginalObjectId};
    use std::sync::Arc;

    #[test]
    fn support_clears_the_high_foot_and_does_not_keep_the_rise_after_leaving() {
        let model = GroundSupport {
            contacts: [Some([-1.0, 0.0, -2.0]), Some([1.0, 0.0, -2.0]), None, None],
        };
        let ground = |[x, y]: [f32; 2]| Ok(Some(if x > 0.5 && y == 0.0 { 3.0 } else { 0.0 }));
        let embedded = TransformState {
            position: [0.0_f32, 0.0, 1.0].map(f32::to_bits),
            scale: [1.0_f32.to_bits(); 3],
            ..Default::default()
        };
        let mut planted = embedded;
        model.reanchor(embedded, &mut planted, ground).unwrap();
        assert_eq!(planted.position.map(f32::from_bits), [0.0, 0.0, 5.0]);
        let mut downhill = planted;
        downhill.position[1] = 2.0_f32.to_bits();
        model.reanchor(planted, &mut downhill, ground).unwrap();
        assert_eq!(downhill.position.map(f32::from_bits), [0.0, 2.0, 2.0]);
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_stock_heavy_autodemo_chassis_stays_above_its_ground_contacts() {
        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed corpus");
        let mut engine = create(EngineServices::new(Arc::new(DirectoryVfs::new(root)))).unwrap();
        load_gameplay_mission(
            &mut engine,
            MissionRequest {
                key: "MISSIONS/Autodemo.00/data.tma".into(),
                mode: MissionMode::AutoDemo,
            },
            MissionStartupInput {
                queue_time_ms: 0,
                research_creation_gate: false,
            },
            None,
        )
        .unwrap();
        let index = engine
            .loaded
            .as_ref()
            .unwrap()
            .object_drafts
            .iter()
            .position(|draft| draft.resource_name_raw.ends_with(b"w_b_wlk1.dat"))
            .unwrap();
        let id = OriginalObjectId(index as u32);
        let support =
            GroundSupport::prepare(&engine.loaded.as_ref().unwrap().mission_assets, index).unwrap();
        let mut previous = None;
        for now in (0..=210_000).step_by(50) {
            frame(&mut engine, now).unwrap();
            let Some(handle) = handle_by_original_id(&engine.world, id) else {
                continue;
            };
            if previous.replace(handle) != Some(handle) {
                continue;
            }
            let pose = transform_state(&engine.world, handle).unwrap();
            let [x, y, z] = pose.position.map(f32::from_bits);
            let terrain = &engine.loaded.as_ref().unwrap().terrain;
            let min_z = support
                .height([x, y], pose, &mut |xy| {
                    terrain.height_at(xy).map_err(EngineError::Terrain)
                })
                .unwrap()
                .unwrap();
            assert!(
                z + 0.001 >= min_z,
                "at {now} ms: heavy chassis Z {z} intersects support Z {min_z}"
            );
            return;
        }
        panic!("stock heavy chassis did not reach an active frame");
    }
}
