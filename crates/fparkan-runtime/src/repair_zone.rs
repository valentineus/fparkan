//! Health service inside the repair volumes authored on facilities.

use crate::{
    initial_component_node_times, node_amount::apply_control_node_delta, MissionAssets,
    MissionControlDeviceTableState, MissionControlNodeProviderTableStatus, MissionObjectDraft,
    ObjectHandle, TransformState, World,
};
use fparkan_assets::PreparedControlPoint;
use fparkan_msh::{node38_fallback_hierarchy, node38_sampled_hierarchy_at_times};
use fparkan_render::LegacyIron3dEulerTransform;
use fparkan_world::{
    handle_by_original_id, identity_metadata, registration_sequence, transform_state,
    OriginalObjectId, OwnerId,
};

// shortcut: interpolate hull recovery until the facility's supply and repair rate are wired.
const HULL_FRACTION_PER_SECOND: f32 = 0.1;

#[derive(Clone, Copy)]
struct RepairVolume {
    floor: [f32; 3],
    axes: [[f32; 3]; 3],
}

impl RepairVolume {
    fn contains(self, local: [f32; 3]) -> bool {
        let offset: [f32; 3] = std::array::from_fn(|axis| local[axis] - self.floor[axis]);
        self.axes.into_iter().enumerate().all(|(axis, vector)| {
            let length_sq: f32 = vector.iter().map(|component| component * component).sum();
            let distance = offset
                .into_iter()
                .zip(vector)
                .map(|(a, b)| a * b)
                .sum::<f32>();
            let portion = distance / length_sq;
            if axis == 2 {
                (0.0..=1.0).contains(&portion)
            } else {
                portion.abs() <= 1.0
            }
        })
    }
}

struct Facility {
    id: OriginalObjectId,
    volumes: Vec<RepairVolume>,
}

#[derive(Default)]
pub(super) struct RepairZones {
    facilities: Vec<Facility>,
    prepared_count: usize,
}

pub(super) struct ActiveFacility<'a> {
    owner: OwnerId,
    matrix: [f32; 16],
    scale: [f32; 3],
    volumes: &'a [RepairVolume],
}

impl RepairZones {
    pub(super) fn prepare(&mut self, assets: &MissionAssets, drafts: &[MissionObjectDraft]) {
        if self.prepared_count > drafts.len() {
            self.facilities.clear();
            self.prepared_count = 0;
        }
        for (index, draft) in drafts.iter().enumerate().skip(self.prepared_count) {
            if !draft
                .unit_dat_header_flags
                .is_some_and(|class| class & 0x8000_0000 != 0)
            {
                continue;
            }
            if let Some(id) = draft.original_id {
                let volumes = facility_volumes(assets, index);
                if !volumes.is_empty() {
                    self.facilities.push(Facility { id, volumes });
                }
            }
        }
        self.prepared_count = drafts.len();
    }

    pub(super) fn active(&self, world: &World) -> Vec<ActiveFacility<'_>> {
        self.facilities
            .iter()
            .filter_map(|facility| {
                let handle = handle_by_original_id(world, facility.id)?;
                registration_sequence(world, handle).ok()??;
                let owner = identity_metadata(world, handle).ok()?.owner_id?;
                let pose = transform_state(world, handle).ok()?;
                let scale = pose.scale.map(f32::from_bits);
                if scale.contains(&0.0) {
                    return None;
                }
                let matrix = LegacyIron3dEulerTransform {
                    translation: pose.position.map(f32::from_bits),
                    orientation_radians: pose.orientation.map(f32::from_bits),
                }
                .try_row_major()?;
                Some(ActiveFacility {
                    owner,
                    matrix,
                    scale,
                    volumes: &facility.volumes,
                })
            })
            .collect()
    }
}

fn facility_volumes(assets: &MissionAssets, index: usize) -> Vec<RepairVolume> {
    let Some(control) = assets
        .object_component_controls
        .get(index)
        .and_then(|controls| controls.first())
        .and_then(Option::as_ref)
    else {
        return Vec::new();
    };
    let Some(points) = control.point_list.as_ref() else {
        return Vec::new();
    };
    let Some(model) = assets
        .visual_for_object(index)
        .and_then(|id| assets.visual_by_id(id))
        .and_then(|visual| visual.model_id)
        .and_then(|model| assets.model_by_id(model))
    else {
        return Vec::new();
    };
    let model = &model.validated;
    let Some(poses) = initial_component_node_times(model.node_count, &control.bindings)
        .ok()
        .and_then(|times| node38_sampled_hierarchy_at_times(model, &times))
        .or_else(|| node38_fallback_hierarchy(model))
    else {
        return Vec::new();
    };
    let named = |name: &[u8]| {
        points.points.iter().find(|point| {
            let end = point
                .name
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(point.name.len());
            point.raw_flags & 0x8000_0000 != 0 && point.name[..end].eq_ignore_ascii_case(name)
        })
    };
    let world_point = |point: &PreparedControlPoint| {
        let pose = poses
            .poses
            .get(usize::try_from(point.raw_signed_field_04).ok()?)?;
        let origin = pose.transform_point(point.position);
        let end = pose.transform_point(std::array::from_fn(|axis| {
            point.position[axis] + point.direction[axis]
        }));
        let axis = std::array::from_fn(|i| end[i] - origin[i]);
        Some((origin, axis))
    };
    [
        [b"rech_w".as_slice(), b"rech_h", b"rech_d"],
        [b"rech_w_i", b"rech_h_i", b"rech_d_i"],
    ]
    .into_iter()
    .filter_map(|[w, h, d]| {
        let (_, width) = world_point(named(w)?)?;
        let (_, length) = world_point(named(h)?)?;
        let (floor, up) = world_point(named(d)?)?;
        Some(RepairVolume {
            floor,
            axes: [width, length, up],
        })
    })
    .collect()
}

pub(super) fn repair_hull(
    table: &mut MissionControlDeviceTableState,
    handle: ObjectHandle,
    world: &World,
    pose: TransformState,
    facilities: &[ActiveFacility<'_>],
    seconds: f32,
) {
    let MissionControlNodeProviderTableStatus::Available(rows) = &table.node_provider_status else {
        return;
    };
    let Some(root) = rows.first() else { return };
    let volume = f32::from_bits(root.current_amount_bits);
    let max = root.source_row.max_volume;
    if !(volume > 0.0 && volume < max && seconds > 0.0) {
        return;
    }
    let Some(owner) = identity_metadata(world, handle)
        .ok()
        .and_then(|meta| meta.owner_id)
    else {
        return;
    };
    let position = pose.position.map(f32::from_bits);
    let in_service = facilities
        .iter()
        .filter(|site| site.owner == owner)
        .any(|site| {
            let m = &site.matrix;
            let [x, y, z] = [position[0] - m[3], position[1] - m[7], position[2] - m[11]];
            let local =
                std::array::from_fn(|i| (m[i] * x + m[i + 4] * y + m[i + 8] * z) / site.scale[i]);
            site.volumes.iter().any(|volume| volume.contains(local))
        });
    if in_service {
        let _ = apply_control_node_delta(table, 0, max * HULL_FRACTION_PER_SECOND * seconds);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        apply_loaded_mission_control_node_delta, create, frame, load_gameplay_mission,
        starting_cockpit, EngineServices, MissionMode, MissionRequest, MissionStartupInput,
    };
    use fparkan_vfs::DirectoryVfs;
    use fparkan_world::{set_owner, set_transform};
    use std::sync::Arc;

    #[test]
    fn repair_volume_extends_up_from_the_deck_and_uses_its_two_lateral_axes() {
        let area = RepairVolume {
            floor: [10.0, 20.0, 30.0],
            axes: [[0.0, 4.0, 0.0], [-3.0, 0.0, 0.0], [0.0, 0.0, 5.0]],
        };
        assert!(area.contains([8.0, 23.0, 30.0]));
        assert!(area.contains([12.0, 17.0, 34.0]));
        assert!(!area.contains([8.0, 23.0, 29.0]));
        assert!(!area.contains([8.0, 25.0, 34.0]));
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_constructor_repairs_the_hero_at_friendly_authored_pads() {
        licensed_repair_pads(
            "MISSIONS/CAMPAIGN/CAMPAIGN.00/Mission.02/data.tma",
            &[0x8000_0010, 0x8000_0040],
        );
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_fortresses_repairs_the_hero_at_fort_and_generator_pads() {
        licensed_repair_pads("MISSIONS/Single.01/data.tma", &[0x8002_0000, 0x8000_0002]);
    }

    fn licensed_repair_pads(key: &str, classes: &[u32]) {
        let root = std::env::var_os("FPARKAN_CORPUS_PART1_ROOT").expect("licensed corpus");
        let mut engine = create(EngineServices::new(Arc::new(DirectoryVfs::new(root)))).unwrap();
        load_gameplay_mission(
            &mut engine,
            MissionRequest {
                key: key.into(),
                mode: MissionMode::SingleMission,
            },
            MissionStartupInput {
                queue_time_ms: 0,
                research_creation_gate: false,
            },
            None,
        )
        .unwrap();
        let hero = starting_cockpit(&engine).unwrap().handle;
        let initial_pose = transform_state(&engine.world, hero).unwrap();
        let read_hull = |engine: &crate::Engine| {
            let table = engine
                .loaded
                .as_ref()
                .unwrap()
                .control_device_tables
                .iter()
                .find(|table| table.handle == hero)
                .unwrap();
            let row = crate::control_node_provider_row(table, 0).unwrap();
            (
                f32::from_bits(row.current_amount_bits),
                row.source_row.max_volume,
            )
        };
        for (case, &class) in classes.iter().enumerate() {
            let start = case as u32 * 3000;
            let mission = engine.loaded.as_ref().unwrap();
            let (index, draft) = mission
                .object_drafts
                .iter()
                .enumerate()
                .find(|(_, draft)| draft.unit_dat_header_flags == Some(class))
                .unwrap();
            let facility =
                handle_by_original_id(&engine.world, draft.original_id.unwrap()).unwrap();
            let zone = facility_volumes(&mission.mission_assets, index)[0];
            let floor = std::array::from_fn(|axis| zone.floor[axis] + zone.axes[2][axis] * 0.5);
            let pose = transform_state(&engine.world, facility).unwrap();
            let world = LegacyIron3dEulerTransform {
                translation: pose.position.map(f32::from_bits),
                orientation_radians: pose.orientation.map(f32::from_bits),
            };
            let mut actor = initial_pose;
            actor.position = world
                .try_transform_scaled_point(floor, pose.scale.map(f32::from_bits))
                .unwrap()
                .map(f32::to_bits);
            let (health, max) = read_hull(&engine);
            apply_loaded_mission_control_node_delta(&mut engine, hero, 0, max * 0.5 - health)
                .unwrap();
            let damaged = read_hull(&engine).0;
            set_transform(&mut engine.world, hero, actor).unwrap();
            set_owner(&mut engine.world, facility, Some(OwnerId(1))).unwrap();
            frame(&mut engine, start).unwrap();
            frame(&mut engine, start + 1000).unwrap();
            assert_eq!(read_hull(&engine).0, damaged);
            set_owner(&mut engine.world, facility, Some(OwnerId(0))).unwrap();
            frame(&mut engine, start + 2000).unwrap();
            let repaired = read_hull(&engine).0;
            assert!(repaired > damaged && repaired <= max);
            set_transform(&mut engine.world, hero, initial_pose).unwrap();
            frame(&mut engine, start + 3000).unwrap();
            assert_eq!(read_hull(&engine).0, repaired);
        }
    }
}
