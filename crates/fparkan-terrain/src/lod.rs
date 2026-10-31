use crate::{BasementFoundation, TerrainLod, TerrainLodError};
use fparkan_terrain_format::LandMeshDocument;

/// Camera-dependent terrain detail selection, retaining LOD0 in basement cells.
#[derive(Clone, Debug)]
pub struct TerrainLodPlan {
    origin: [f32; 2],
    inverse_cell_size: [f32; 2],
    counts: [usize; 2],
    forced_lod0: Vec<bool>,
    has_lod1: Vec<bool>,
    face_nodes: Vec<usize>,
}

impl TerrainLodPlan {
    /// Validates the authored grid and registers foundation patches in its cells.
    ///
    /// # Errors
    /// Returns an error for an invalid slot mapping, grid or foundation source face.
    pub fn new(
        mesh: &LandMeshDocument,
        foundation: Option<&BasementFoundation>,
    ) -> Result<Self, TerrainLodError> {
        let assignments = mesh.face_lod_assignments()?;
        let bounds = mesh
            .nodes_raw
            .chunks_exact(38)
            .map(|node| {
                let slot = u16::from_le_bytes([node[8], node[9]]);
                let raw = mesh.slots.slots_raw.get(usize::from(slot))?;
                let coordinate =
                    |offset: usize| f32::from_le_bytes(raw[offset..offset + 4].try_into().unwrap());
                Some((
                    [coordinate(8), coordinate(12)],
                    [coordinate(20), coordinate(24)],
                ))
            })
            .collect::<Option<Vec<_>>>()
            .ok_or(TerrainLodError::InvalidCellGrid)?;
        let &(first_min, first_max) = bounds.first().ok_or(TerrainLodError::InvalidCellGrid)?;
        let header = &mesh.slots.header_raw;
        let coordinate = |offset: usize| {
            header
                .get(offset..offset + 4)
                .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
        };
        let origin = [
            coordinate(0).ok_or(TerrainLodError::InvalidCellGrid)?,
            coordinate(4).ok_or(TerrainLodError::InvalidCellGrid)?,
        ];
        let size = [first_max[0] - first_min[0], first_max[1] - first_min[1]];
        if !origin.iter().all(|value| value.is_finite())
            || !size.iter().all(|value| value.is_finite() && *value > 0.0)
        {
            return Err(TerrainLodError::InvalidCellGrid);
        }
        let count_x = mesh
            .streams
            .iter()
            .find(|s| s.type_id == 1)
            .map(|s| s.attributes.attr2 as usize)
            .ok_or(TerrainLodError::InvalidCellGrid)?;
        if count_x == 0 || !bounds.len().is_multiple_of(count_x) {
            return Err(TerrainLodError::InvalidCellGrid);
        }
        let count_y = bounds.len() / count_x;
        let counts = [count_x, count_y];
        for (node, &(min, max)) in bounds.iter().enumerate() {
            let cell = [node / count_y, node % count_y];
            for axis in 0..2 {
                let expected = origin[axis] + cell[axis] as f32 * size[axis];
                let tolerance = size[axis] * 1.0e-5;
                if !min[axis].is_finite()
                    || !max[axis].is_finite()
                    || (min[axis] - expected).abs() > tolerance
                    || (max[axis] - (expected + size[axis])).abs() > tolerance
                {
                    return Err(TerrainLodError::InvalidCellGrid);
                }
            }
        }
        let has_lod1 = mesh
            .nodes_raw
            .chunks_exact(38)
            .map(|node| {
                let slot = u16::from_le_bytes([node[10], node[11]]);
                slot != 0xff
                    && slot != u16::MAX
                    && mesh
                        .slots
                        .slots_raw
                        .get(usize::from(slot))
                        .is_some_and(|raw| u16::from_le_bytes([raw[2], raw[3]]) != 0)
            })
            .collect();
        let mut result = Self {
            origin,
            inverse_cell_size: size.map(f32::recip),
            counts,
            forced_lod0: vec![false; bounds.len()],
            has_lod1,
            face_nodes: assignments.iter().map(|a| a.node_index).collect(),
        };
        if let Some(foundation) = foundation {
            let mut fragments = vec![0usize; mesh.faces.len()];
            let mut unchanged = vec![false; mesh.faces.len()];
            for fragment in &foundation.fragments {
                if let Some(face_index) = fragment.source_face {
                    let face = mesh
                        .faces
                        .get(face_index)
                        .ok_or(TerrainLodError::MissingFaceAssignment { face: face_index })?;
                    fragments[face_index] += 1;
                    let original = face.vertices.map(|v| mesh.positions[usize::from(v)]);
                    unchanged[face_index] = fragment.positions.iter().all(|p| original.contains(p));
                }
            }
            for assignment in assignments.iter().filter(|a| a.lod == TerrainLod::Lod0) {
                if fragments[assignment.face_index] != 1 || !unchanged[assignment.face_index] {
                    result.forced_lod0[assignment.node_index] = true;
                }
            }
            // Native registers patch triangles against every cell, including shared edges.
            for fragment in foundation.fragments.iter().filter(|fragment| {
                fragment
                    .source_face
                    .is_none_or(|face| fragments[face] != 1 || !unchanged[face])
            }) {
                for node in 0..bounds.len() {
                    let cell = [node / count_y, node % count_y];
                    let center = std::array::from_fn(|axis| {
                        (cell[axis] as f32 + 0.5) * size[axis] + origin[axis]
                    });
                    if triangle_intersects_cell(
                        fragment.positions,
                        center,
                        result.inverse_cell_size,
                    ) {
                        result.forced_lod0[node] = true;
                    }
                }
            }
        }
        Ok(result)
    }

    /// Returns the cell named by the authored slot that owns a source face.
    #[must_use]
    pub fn source_face_node(&self, face: usize) -> Option<usize> {
        self.face_nodes.get(face).copied()
    }

    fn camera_cell(&self, point: [f32; 2]) -> Option<[i32; 2]> {
        let cell: [f32; 2] = std::array::from_fn(|axis| {
            ((point[axis] - self.origin[axis]) * self.inverse_cell_size[axis]).floor()
        });
        cell.iter()
            .all(|v| v.is_finite() && *v >= i32::MIN as f32 && *v < i32::MAX as f32)
            .then(|| cell.map(|v| v as i32))
    }

    /// Selects LOD1 at a squared cell distance of at least nine, unless forced near.
    /// Returns `None` for an invalid node, camera position or FOV.
    #[must_use]
    pub fn selected_lod(
        &self,
        node: usize,
        eye: [f32; 2],
        horizontal_fov: f32,
    ) -> Option<TerrainLod> {
        let forced = *self.forced_lod0.get(node)?;
        let has_far = *self.has_lod1.get(node)?;
        let camera = self.camera_cell(eye)?;
        if !horizontal_fov.is_finite() || horizontal_fov <= 0.0 {
            return None;
        }
        let dx = (node / self.counts[1]) as f64 - f64::from(camera[0]);
        let dy = (node % self.counts[1]) as f64 - f64::from(camera[1]);
        Some(
            if !forced && has_far && horizontal_fov >= 0.5 && dx * dx + dy * dy >= 9.0 {
                TerrainLod::Lod1
            } else {
                TerrainLod::Lod0
            },
        )
    }

    /// Whether the map contains any usable far slots.
    #[must_use]
    pub fn has_far_geometry(&self) -> bool {
        self.has_lod1.iter().any(|&value| value)
    }
}

fn triangle_intersects_cell(
    triangle: [[f32; 3]; 3],
    center: [f32; 2],
    inverse_size: [f32; 2],
) -> bool {
    // Normalize in f32 before testing the native [-0.5, 0.5] box.
    let points = triangle.map(|p| {
        std::array::from_fn::<_, 2, _>(|axis| {
            f64::from((p[axis] - center[axis]) * inverse_size[axis])
        })
    });
    let edges = std::array::from_fn::<_, 3, _>(|i| {
        let next = points[(i + 1) % 3];
        [next[1] - points[i][1], points[i][0] - next[0]]
    });
    [[1.0, 0.0], [0.0, 1.0]]
        .into_iter()
        .chain(edges)
        .all(|axis| {
            let projections = points.map(|p| p[0] * axis[0] + p[1] * axis[1]);
            let radius = 0.5 * (axis[0].abs() + axis[1].abs());
            projections.iter().copied().fold(f64::INFINITY, f64::min) <= radius
                && projections
                    .iter()
                    .copied()
                    .fold(f64::NEG_INFINITY, f64::max)
                    >= -radius
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_registration_keeps_native_normalization_and_triangle_edges() {
        let size = f32::from_bits(0x42d4_15f5);
        let center = [2.5 * size, 6.5 * size];
        let edge = [
            [3.0 * size, 6.0 * size, 0.0],
            [3.0 * size, 7.0 * size, 0.0],
            [4.0 * size, 6.0 * size, 0.0],
        ];
        // The authored boundary rounds outside this cell after native normalization.
        assert!(!triangle_intersects_cell(edge, center, [size.recip(); 2]));
        assert!(triangle_intersects_cell(
            edge,
            [3.5 * size, 6.5 * size],
            [size.recip(); 2]
        ));
        assert!(!triangle_intersects_cell(
            [[0.0, 0.0, 0.0], [2.0, 0.0, 0.0], [0.0, 2.0, 0.0]],
            [1.75, 1.75],
            [2.0; 2]
        ));
        assert!(triangle_intersects_cell(
            [[0.0, 0.0, 0.0], [2.0, 0.0, 0.0], [0.0, 2.0, 0.0]],
            [1.5, 1.0],
            [2.0; 2]
        ));
    }

    #[test]
    fn native_cell_distance_boundary_zoom_and_basement_override() {
        let mut plan = TerrainLodPlan {
            origin: [0.0; 2],
            inverse_cell_size: [1.0; 2],
            counts: [8, 8],
            forced_lod0: vec![false; 64],
            has_lod1: vec![true; 64],
            face_nodes: Vec::new(),
        };
        assert_eq!(
            plan.selected_lod(2 * 8 + 2, [0.9, 0.9], 1.3),
            Some(TerrainLod::Lod0)
        );
        assert_eq!(
            plan.selected_lod(3 * 8, [0.9, 0.9], 1.3),
            Some(TerrainLod::Lod1)
        );
        assert_eq!(
            plan.selected_lod(3 * 8, [1.0, 0.9], 1.3),
            Some(TerrainLod::Lod0)
        );
        assert_eq!(
            plan.selected_lod(3 * 8, [0.9, 0.9], 0.49),
            Some(TerrainLod::Lod0)
        );
        assert_eq!(
            plan.selected_lod(3 * 8, [0.9, 0.9], 0.5),
            Some(TerrainLod::Lod1)
        );
        plan.forced_lod0[3 * 8] = true;
        assert_eq!(
            plan.selected_lod(3 * 8, [0.9, 0.9], 1.3),
            Some(TerrainLod::Lod0)
        );
        plan.has_lod1[7 * 8] = false;
        assert_eq!(
            plan.selected_lod(7 * 8, [0.9, 0.9], 1.3),
            Some(TerrainLod::Lod0)
        );
        assert_eq!(plan.selected_lod(64, [0.0; 2], 1.3), None);
        assert_eq!(plan.selected_lod(0, [f32::NAN, 0.0], 1.3), None);
        assert_eq!(plan.selected_lod(0, [0.0; 2], f32::INFINITY), None);
    }
}
