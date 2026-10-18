//! Native-shaped building basement foundation geometry.

use crate::contour::subtract_contour;
use fparkan_terrain_format::{FullSurfaceMask, LandMeshDocument, TerrainLod};

const LINE_EPSILON: f64 = 1.0e-5;
const POINT_EPSILON_SQUARED: f64 = 1.0e-12;

/// A terrain or generated basement triangle shared by preview consumers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BasementFoundationFragment {
    /// Original Land.msh face for terrain fragments; `None` for generated band faces.
    pub source_face: Option<usize>,
    /// Raw terrain or generated basement face flags.
    pub surface_mask: FullSurfaceMask,
    /// Triangle coordinates shared by rendering, shadows, and terrain queries.
    pub positions: [[f32; 3]; 3],
}

/// Canonical output of the native-shaped terrain basement preparation.
#[derive(Clone, Debug, PartialEq)]
pub struct BasementFoundation {
    pub fragments: Vec<BasementFoundationFragment>,
    /// Native placement-height inputs in footprint order.
    pub placements: Vec<BasementPlacement>,
}

/// One building footprint in native preparation order.
#[derive(Clone, Copy, Debug)]
pub struct BasementFootprint<'a> {
    /// Authored outside ring in world XY coordinates.
    pub outer: &'a [[f32; 2]],
    /// Authored inner ring, retaining its source Z for placement calculations.
    pub inner: &'a [[f32; 3]],
    /// Whether native getter mode flattens the generated inner boundary to mean outer Z.
    pub flatten_inner_to_mean_outer_z: bool,
}

/// Native placement-height inputs captured for one building.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BasementPlacement {
    /// Native transform adjustment: first authored inner Z minus mean sampled outer terrain Z.
    pub placement_delta: f32,
}

/// Error returned while preparing a basement foundation.
#[derive(Clone, Debug, PartialEq)]
pub enum BasementFoundationError {
    /// Terrain Node38/Slot68 data cannot resolve LOD0 faces.
    InvalidLodMapping(fparkan_terrain_format::TerrainLodError),
    /// A selected LOD0 face refers to a missing vertex.
    InvalidTerrainFace { face: usize, vertex: u16 },
    /// A contour, nested-ring relationship, or generated annulus is invalid.
    InvalidContour,
    /// The LOD0 mesh has no usable terrain faces.
    EmptyLod0,
    /// The outer contour did not cross any LOD0 face.
    NoOuterCrossings,
    /// A point of the authored outer contour lies outside the LOD0 terrain.
    OuterPointOutsideLod0 { point: usize },
    /// An authored outer segment leaves the current terrain surface.
    UncoveredOuterSegment { segment: usize },
    /// Footprint outer contours overlap; native overlap semantics remain unverified.
    OverlappingFootprints,
}

impl std::fmt::Display for BasementFoundationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidLodMapping(error) => write!(f, "invalid terrain LOD mapping: {error}"),
            Self::InvalidTerrainFace { face, vertex } => {
                write!(f, "terrain face {face} references missing vertex {vertex}")
            }
            Self::InvalidContour => write!(f, "invalid basement contour geometry"),
            Self::EmptyLod0 => write!(f, "terrain has no LOD0 faces"),
            Self::NoOuterCrossings => write!(f, "outer contour does not cross LOD0 terrain"),
            Self::OuterPointOutsideLod0 { point } => {
                write!(f, "outer contour point {point} is outside LOD0 terrain")
            }
            Self::UncoveredOuterSegment { segment } => {
                write!(
                    f,
                    "outer contour segment {segment} leaves the terrain surface"
                )
            }
            Self::OverlappingFootprints => {
                write!(f, "overlapping basement footprints are not supported")
            }
        }
    }
}

impl std::error::Error for BasementFoundationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidLodMapping(error) => Some(error),
            Self::InvalidTerrainFace { .. }
            | Self::InvalidContour
            | Self::EmptyLod0
            | Self::NoOuterCrossings => None,
            Self::OuterPointOutsideLod0 { .. } | Self::UncoveredOuterSegment { .. } => None,
            Self::OverlappingFootprints => None,
        }
    }
}

/// Строит основания ландшафта в порядке нативной подготовки размещений.
///
/// Вершины внешнего контура нормализуются по топологии LOD0: сначала проверяются
/// вершины граней и соседних граней, затем применяется проекция, используемая
/// нативным `FUN_10006240`. Это косинусная формула с отдельной проверкой
/// расстояния, а не обычная ортогональная проекция. При обходе сегмента
/// вставляются только вершины LOD0, прошедшие тот же нативный предикат и
/// находящиеся не ближе 0.5 мировых единиц к обоим концам; геометрическое
/// совпадение с отрезком само по себе недостаточно.
///
/// Для каждого контура функция упорядочивает пересечения с плоскостями текущих
/// канонических треугольников, вычисляет среднюю высоту внешней границы и строит
/// ограниченную триангуляцию кольца. Явный флаг выбирает нативную ветвь высоты
/// внутренней границы. Непересекающиеся контуры последовательно вырезаются из
/// текущей поверхности; пересекающиеся контуры отклоняются, поскольку их
/// взаимодействие в нативном коде пока не установлено.
///
/// Возвращённый набор фрагментов является общей геометрией для рендеринга,
/// теней и запросов поверхности. Фрагменты исходной поверхности сохраняют
/// номера и маски исходных граней; у сгенерированных граней маска соответствует
/// записи `AddBasementFaceProc`.
///
/// The returned fragment vector is the canonical geometry for rendering,
/// shadows, and terrain queries. Source faces preserve their original face IDs
/// and masks. Generated faces use the raw mask written by AddBasementFaceProc.
pub fn build_basement_foundations(
    mesh: &LandMeshDocument,
    footprints: &[BasementFootprint<'_>],
) -> Result<BasementFoundation, BasementFoundationError> {
    let assignments = mesh
        .face_lod_assignments()
        .map_err(BasementFoundationError::InvalidLodMapping)?;
    let mut lod0_face_indices = Vec::new();
    let mut terrain = Vec::new();
    for assignment in assignments
        .iter()
        .filter(|assignment| assignment.lod == TerrainLod::Lod0)
    {
        let face = &mesh.faces[assignment.face_index];
        let positions = terrain_face_positions(mesh, assignment.face_index)?;
        terrain.push(BasementFoundationFragment {
            source_face: Some(assignment.face_index),
            surface_mask: face.flags,
            positions,
        });
        lod0_face_indices.push(assignment.face_index);
    }
    if terrain.is_empty() {
        return Err(BasementFoundationError::EmptyLod0);
    }

    let mut bands = Vec::new();
    let mut placements = Vec::with_capacity(footprints.len());
    let mut previous_outer_rings: Vec<Vec<[f64; 2]>> = Vec::with_capacity(footprints.len());
    for footprint in footprints {
        let (outer_points, outer_closed) = normalize_ring(footprint.outer)?;
        let snapped_outer = normalize_outer_points(&outer_points, &lod0_face_indices, mesh)?;
        if !simple_ring(&snapped_outer) {
            return Err(BasementFoundationError::InvalidContour);
        }
        if previous_outer_rings
            .iter()
            .any(|previous| rings_overlap(previous, &snapped_outer))
        {
            return Err(BasementFoundationError::OverlappingFootprints);
        }
        previous_outer_rings.push(snapped_outer.clone());
        let mut outer_ring = snapped_outer
            .iter()
            .map(|point| [point[0] as f32, point[1] as f32])
            .collect::<Vec<_>>();
        if outer_closed {
            outer_ring.push(outer_ring[0]);
        }
        let samples = terrain
            .iter()
            .map(|fragment| {
                (
                    fragment
                        .source_face
                        .expect("terrain fragments have source faces"),
                    fragment.surface_mask,
                    fragment.positions,
                )
            })
            .collect::<Vec<_>>();
        let mut outer_crossings = contour_crossings(&snapped_outer, &samples)?;
        if outer_crossings.is_empty() {
            return Err(BasementFoundationError::NoOuterCrossings);
        }
        // Crossings are open here; restore the authored duplicate only afterward.
        let mean_outer_z = outer_crossings
            .iter()
            .map(|crossing| f64::from(crossing[2]))
            .sum::<f64>()
            / outer_crossings.len() as f64;
        if !mean_outer_z.is_finite() {
            return Err(BasementFoundationError::InvalidContour);
        }
        let mean_outer_z = mean_outer_z as f32;
        if outer_closed {
            let closing = *outer_crossings
                .first()
                .ok_or(BasementFoundationError::NoOuterCrossings)?;
            outer_crossings.push(closing);
        }

        let inner_xy = footprint
            .inner
            .iter()
            .map(|point| [point[0], point[1]])
            .collect::<Vec<_>>();
        let (inner_points, inner_closed) = normalize_ring(&inner_xy)?;
        if footprint
            .inner
            .iter()
            .flatten()
            .any(|value| !value.is_finite())
            || !nested_rings(&snapped_outer, &inner_points)
        {
            return Err(BasementFoundationError::InvalidContour);
        }
        let authored_inner_z = footprint
            .inner
            .first()
            .ok_or(BasementFoundationError::InvalidContour)?[2];
        let placement_delta = authored_inner_z - mean_outer_z;
        if !placement_delta.is_finite() {
            return Err(BasementFoundationError::InvalidContour);
        }
        placements.push(BasementPlacement { placement_delta });
        let inner_vertex_count = if inner_closed {
            footprint.inner.len() - 1
        } else {
            footprint.inner.len()
        };
        let mut inner_ring = footprint.inner[..inner_vertex_count]
            .iter()
            .map(|point| {
                [
                    point[0],
                    point[1],
                    if footprint.flatten_inner_to_mean_outer_z {
                        mean_outer_z
                    } else {
                        point[2]
                    },
                ]
            })
            .collect::<Vec<_>>();
        if inner_closed {
            inner_ring.push(inner_ring[0]);
        }
        let outer_surface = crossings_as_ring(&outer_crossings, outer_closed)?;
        let inner_surface = inner_ring_without_closure(&inner_ring);
        for positions in triangulate_annulus(&outer_surface, inner_surface)? {
            bands.push(BasementFoundationFragment {
                source_face: None,
                surface_mask: basement_surface_mask(positions),
                positions,
            });
        }

        let source_positions = terrain
            .iter()
            .map(|fragment| fragment.positions)
            .collect::<Vec<_>>();
        let outside = subtract_contour(&source_positions, &outer_ring)
            .map_err(|_| BasementFoundationError::InvalidContour)?;
        let clipped = outside
            .into_iter()
            .map(|fragment| {
                let source = terrain[fragment.source_triangle];
                BasementFoundationFragment {
                    source_face: source.source_face,
                    surface_mask: source.surface_mask,
                    positions: fragment.positions,
                }
            })
            .collect();
        terrain = clipped;
    }
    terrain.extend(bands);
    Ok(BasementFoundation {
        fragments: terrain,
        placements,
    })
}

fn terrain_face_positions(
    mesh: &LandMeshDocument,
    face_index: usize,
) -> Result<[[f32; 3]; 3], BasementFoundationError> {
    let face = &mesh.faces[face_index];
    let resolve = |vertex: u16| {
        mesh.positions.get(usize::from(vertex)).copied().ok_or(
            BasementFoundationError::InvalidTerrainFace {
                face: face_index,
                vertex,
            },
        )
    };
    Ok([
        resolve(face.vertices[0])?,
        resolve(face.vertices[1])?,
        resolve(face.vertices[2])?,
    ])
}

fn normalize_ring(ring: &[[f32; 2]]) -> Result<(Vec<[f64; 2]>, bool), BasementFoundationError> {
    if ring.iter().flatten().any(|value| !value.is_finite()) {
        return Err(BasementFoundationError::InvalidContour);
    }
    let mut points = ring
        .iter()
        .map(|point| [f64::from(point[0]), f64::from(point[1])])
        .collect::<Vec<_>>();
    let closed = points.len() > 1
        && distance_squared(points[0], *points.last().expect("nonempty")) <= POINT_EPSILON_SQUARED;
    if closed {
        points.pop();
    }
    if points.len() < 3 || !simple_ring(&points) {
        return Err(BasementFoundationError::InvalidContour);
    }
    Ok((points, closed))
}

fn normalize_outer_points(
    points: &[[f64; 2]],
    lod0_faces: &[usize],
    mesh: &LandMeshDocument,
) -> Result<Vec<[f64; 2]>, BasementFoundationError> {
    // shortcut: first-face lookup and neighbor ordering still use source LOD0
    // IDs for every placement. The crossing sampler and cutter use the current
    // canonical fragment list, but dynamic neighbor-ID reconstruction is not
    // modeled yet, so later placements are not claimed as native-identical.
    let mut output = points
        .iter()
        .enumerate()
        .map(|(point_index, point)| {
            native_normalize_outer_point(
                mesh,
                lod0_faces,
                [point[0] as f32, point[1] as f32],
                point_index,
            )
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|point| [f64::from(point[0]), f64::from(point[1])])
        .collect::<Vec<_>>();

    // The native contour pass asks for terrain faces overlapping the current
    // segment's AABB, then tries their three vertices in face order. On one
    // accepted vertex it inserts the full terrain point and resumes from the
    // shortened segment at the same index.
    let mut segment_index = 0;
    while segment_index < output.len() {
        let first = output[segment_index].map(|value| value as f32);
        let second = output[(segment_index + 1) % output.len()].map(|value| value as f32);
        let min = [first[0].min(second[0]), first[1].min(second[1])];
        let max = [first[0].max(second[0]), first[1].max(second[1])];
        let mut inserted = false;
        'faces: for &face_index in lod0_faces {
            let Some(face) = mesh.faces.get(face_index) else {
                continue;
            };
            let triangle = face
                .vertices
                .map(|vertex| mesh.positions.get(usize::from(vertex)).copied());
            let [Some(a), Some(b), Some(c)] = triangle else {
                continue;
            };
            let face_min = [a[0].min(b[0]).min(c[0]), a[1].min(b[1]).min(c[1])];
            let face_max = [a[0].max(b[0]).max(c[0]), a[1].max(b[1]).max(c[1])];
            if (0..2).any(|axis| face_max[axis] < min[axis] || max[axis] < face_min[axis]) {
                continue;
            }
            for vertex in [a, b, c] {
                if !native_segment_inserts_vertex(first, second, vertex) {
                    continue;
                }
                let vertex_xy = [f64::from(vertex[0]), f64::from(vertex[1])];
                if output
                    .iter()
                    .any(|existing| distance_squared(*existing, vertex_xy) <= POINT_EPSILON_SQUARED)
                {
                    continue;
                }
                output.insert(segment_index + 1, vertex_xy);
                inserted = true;
                break 'faces;
            }
        }
        if !inserted {
            segment_index += 1;
        }
    }
    Ok(output)
}

fn native_normalize_outer_point(
    mesh: &LandMeshDocument,
    lod0_faces: &[usize],
    point: [f32; 2],
    point_index: usize,
) -> Result<[f32; 2], BasementFoundationError> {
    let containing_face = lod0_faces.iter().copied().find(|&face_index| {
        let Some(face) = mesh.faces.get(face_index) else {
            return false;
        };
        let triangle = face
            .vertices
            .map(|vertex| mesh.positions[usize::from(vertex)]);
        point_in_triangle_xy(point, triangle)
    });
    let containing_face = containing_face
        .ok_or(BasementFoundationError::OuterPointOutsideLod0 { point: point_index })?;
    let mut candidates = vec![containing_face];
    for neighbor in mesh.faces[containing_face].neighbors.iter().flatten() {
        let neighbor = usize::from(*neighbor);
        if lod0_faces.contains(&neighbor) && !candidates.contains(&neighbor) {
            candidates.push(neighbor);
        }
    }
    let normalized = point;
    for face_index in candidates {
        let face = &mesh.faces[face_index];
        let vertices = face
            .vertices
            .map(|vertex| mesh.positions.get(usize::from(vertex)).copied());
        let [Some(a), Some(b), Some(c)] = vertices else {
            return Err(BasementFoundationError::InvalidTerrainFace {
                face: face_index,
                vertex: face.vertices[0],
            });
        };
        let vertices = [a, b, c];
        for (first, second) in [(0, 1), (1, 2), (2, 0)] {
            if let Some(candidate) =
                native_normalize_edge(normalized, vertices[first], vertices[second])?
            {
                return Ok(candidate);
            }
        }
    }
    Ok(normalized)
}

fn point_in_triangle_xy(point: [f32; 2], triangle: [[f32; 3]; 3]) -> bool {
    let sign = |a: [f32; 3], b: [f32; 3]| {
        (point[0] - b[0]) * (a[1] - b[1]) - (a[0] - b[0]) * (point[1] - b[1])
    };
    let d1 = sign(triangle[0], triangle[1]);
    let d2 = sign(triangle[1], triangle[2]);
    let d3 = sign(triangle[2], triangle[0]);
    let has_negative = d1 < -1.0e-4 || d2 < -1.0e-4 || d3 < -1.0e-4;
    let has_positive = d1 > 1.0e-4 || d2 > 1.0e-4 || d3 > 1.0e-4;
    !(has_negative && has_positive)
}

fn native_normalize_edge(
    point: [f32; 2],
    first: [f32; 3],
    second: [f32; 3],
) -> Result<Option<[f32; 2]>, BasementFoundationError> {
    if distance_xy(point, [first[0], first[1]]) < 0.5 {
        let point = [first[0], first[1]];
        return Ok(Some(point));
    }
    if distance_xy(point, [second[0], second[1]]) < 0.5 {
        let point = [second[0], second[1]];
        return Ok(Some(point));
    }
    let Some((offset, _)) = native_edge_projection(point, first, second) else {
        return Ok(None);
    };
    let mut projected = [point[0] + offset[0], point[1] + offset[1]];
    let Some((check_offset, _)) = native_edge_projection(projected, first, second) else {
        return Err(BasementFoundationError::InvalidContour);
    };
    if check_offset[0].hypot(check_offset[1]) >= 0.0001 {
        return Err(BasementFoundationError::InvalidContour);
    }
    if distance_xy(projected, [first[0], first[1]]) < 0.5 {
        projected = [first[0], first[1]];
    }
    if distance_xy(projected, [second[0], second[1]]) < 0.5 {
        projected = [second[0], second[1]];
    }
    Ok(Some(projected))
}

fn native_segment_inserts_vertex(first: [f32; 2], second: [f32; 2], point: [f32; 3]) -> bool {
    if distance_xy([point[0], point[1]], first) < 0.5
        || distance_xy([point[0], point[1]], second) < 0.5
    {
        return false;
    }
    native_edge_projection(
        [point[0], point[1]],
        [first[0], first[1], 0.0],
        [second[0], second[1], 0.0],
    )
    .is_some()
}

fn native_edge_projection(
    point: [f32; 2],
    first: [f32; 3],
    second: [f32; 3],
) -> Option<([f32; 2], f32)> {
    let ux = point[0] - first[0];
    let uy = point[1] - first[1];
    let ex = second[0] - first[0];
    let ey = second[1] - first[1];
    let edge_length = (ex * ex + ey * ey).sqrt();
    let point_length = (ux * ux + uy * uy).sqrt();
    if edge_length == 0.0 || point_length == 0.0 {
        return None;
    }
    let t = ((ex * ux + ey * uy) / edge_length) / point_length;
    if !(0.0..=1.0).contains(&t) {
        return None;
    }
    let offset = [
        (t * ex + first[0]) - point[0],
        (t * ey + first[1]) - point[1],
    ];
    (offset[0].hypot(offset[1]) < 0.5).then_some((offset, t))
}

fn distance_xy(first: [f32; 2], second: [f32; 2]) -> f32 {
    ((first[0] - second[0]).powi(2) + (first[1] - second[1]).powi(2)).sqrt()
}

fn contour_crossings(
    ring: &[[f64; 2]],
    terrain_faces: &[(usize, FullSurfaceMask, [[f32; 3]; 3])],
) -> Result<Vec<[f32; 3]>, BasementFoundationError> {
    let mut crossings = Vec::new();
    for segment_index in 0..ring.len() {
        let first = ring[segment_index];
        let second = ring[(segment_index + 1) % ring.len()];
        let mut segment_crossings = Vec::new();
        let mut coverage = Vec::new();
        for (source_face, _, triangle) in terrain_faces {
            let Some((start, end)) = segment_triangle_interval(first, second, *triangle) else {
                continue;
            };
            coverage.push((start, end));
            for t in [start, end] {
                let point = [
                    first[0] + (second[0] - first[0]) * t,
                    first[1] + (second[1] - first[1]) * t,
                ];
                let Some(position) = lift_to_face(point, *triangle) else {
                    continue;
                };
                segment_crossings.push((t, *source_face, position));
            }
        }
        segment_crossings.sort_by(|left, right| {
            left.0
                .total_cmp(&right.0)
                .then_with(|| left.1.cmp(&right.1))
        });
        coverage.sort_by(|left, right| {
            left.0
                .total_cmp(&right.0)
                .then_with(|| left.1.total_cmp(&right.1))
        });
        let mut covered_until = 0.0_f64;
        for (start, end) in coverage {
            if start > covered_until + 1.0e-8 {
                return Err(BasementFoundationError::UncoveredOuterSegment {
                    segment: segment_index,
                });
            }
            covered_until = covered_until.max(end);
        }
        if covered_until < 1.0 - 1.0e-8 {
            return Err(BasementFoundationError::UncoveredOuterSegment {
                segment: segment_index,
            });
        }
        for (_, _, position) in segment_crossings {
            let duplicate_of_previous = crossings.last().is_some_and(|previous: &[f32; 3]| {
                distance_squared(
                    [f64::from(previous[0]), f64::from(previous[1])],
                    [f64::from(position[0]), f64::from(position[1])],
                ) <= POINT_EPSILON_SQUARED
            });
            if duplicate_of_previous {
                continue;
            }
            crossings.push(position);
        }
    }
    if crossings.len() > 1
        && distance_squared(
            [f64::from(crossings[0][0]), f64::from(crossings[0][1])],
            [
                f64::from(crossings.last().expect("nonempty")[0]),
                f64::from(crossings.last().expect("nonempty")[1]),
            ],
        ) <= POINT_EPSILON_SQUARED
    {
        crossings.pop();
    }
    Ok(crossings)
}

fn segment_triangle_interval(
    first: [f64; 2],
    second: [f64; 2],
    triangle: [[f32; 3]; 3],
) -> Option<(f64, f64)> {
    let vertices = triangle.map(|point| [f64::from(point[0]), f64::from(point[1])]);
    let area = cross2(vertices[0], vertices[1], vertices[2]);
    if !area.is_finite() || area.abs() <= 1.0e-12 {
        return None;
    }
    let orientation = area.signum();
    let delta = [second[0] - first[0], second[1] - first[1]];
    let mut lower: f64 = 0.0;
    let mut upper: f64 = 1.0;
    for edge in 0..3 {
        let edge_first = vertices[edge];
        let edge_second = vertices[(edge + 1) % 3];
        let start_distance = orientation * cross2(edge_first, edge_second, first);
        let end = [first[0] + delta[0], first[1] + delta[1]];
        let end_distance = orientation * cross2(edge_first, edge_second, end);
        let slope = end_distance - start_distance;
        if slope.abs() <= 1.0e-15 {
            if start_distance < -LINE_EPSILON {
                return None;
            }
            continue;
        }
        // Tolerance decides whether a nearly outside interval is accepted;
        // the actual boundary sample remains on the geometric edge (distance 0).
        let crossing = -start_distance / slope;
        if slope > 0.0 {
            lower = lower.max(crossing);
        } else {
            upper = upper.min(crossing);
        }
        if lower > upper {
            return None;
        }
    }
    (upper >= 0.0 && lower <= 1.0).then_some((lower.clamp(0.0, 1.0), upper.clamp(0.0, 1.0)))
}

fn lift_to_face(point: [f64; 2], triangle: [[f32; 3]; 3]) -> Option<[f32; 3]> {
    let vertices = triangle.map(|vertex| vertex.map(f64::from));
    let denominator = cross2(
        [vertices[0][0], vertices[0][1]],
        [vertices[1][0], vertices[1][1]],
        [vertices[2][0], vertices[2][1]],
    );
    if denominator.abs() <= 1.0e-12 {
        return None;
    }
    let weight_b = cross2(
        [vertices[0][0], vertices[0][1]],
        point,
        [vertices[2][0], vertices[2][1]],
    ) / denominator;
    let weight_c = cross2(
        [vertices[0][0], vertices[0][1]],
        [vertices[1][0], vertices[1][1]],
        point,
    ) / denominator;
    let weight_a = 1.0 - weight_b - weight_c;
    let z = weight_a * vertices[0][2] + weight_b * vertices[1][2] + weight_c * vertices[2][2];
    let result = [point[0] as f32, point[1] as f32, z as f32];
    result
        .iter()
        .all(|value| value.is_finite())
        .then_some(result)
}

fn crossings_as_ring(
    crossings: &[[f32; 3]],
    closed: bool,
) -> Result<Vec<[f32; 3]>, BasementFoundationError> {
    let length = crossings.len().saturating_sub(usize::from(closed));
    let ring = crossings.iter().take(length).copied().collect::<Vec<_>>();
    (ring.len() >= 3)
        .then_some(ring)
        .ok_or(BasementFoundationError::InvalidContour)
}

fn inner_ring_without_closure(ring: &[[f32; 3]]) -> Vec<[f32; 3]> {
    let mut result = ring.to_vec();
    if result.len() > 1 && same_xy(result[0], *result.last().expect("nonempty")) {
        result.pop();
    }
    result
}

fn triangulate_annulus(
    outer: &[[f32; 3]],
    inner: Vec<[f32; 3]>,
) -> Result<Vec<[[f32; 3]; 3]>, BasementFoundationError> {
    let mut outer = remove_consecutive_duplicates(outer);
    let mut inner = remove_consecutive_duplicates(&inner);
    if outer.len() < 3 || inner.len() < 3 || !nested_rings_3d(&outer, &inner) {
        return Err(BasementFoundationError::InvalidContour);
    }
    if signed_area_3d(&outer) < 0.0 {
        outer.reverse();
    }
    if signed_area_3d(&inner) > 0.0 {
        inner.reverse();
    }
    let polygon = bridge_hole(&outer, &inner)?;
    let output =
        legalize_constrained_edges(ear_clip_weakly_simple_polygon(&polygon)?, &outer, &inner)?;
    if output.iter().any(|triangle| {
        !triangle.iter().flatten().all(|value| value.is_finite())
            || cross2(xy(triangle[0]), xy(triangle[1]), xy(triangle[2])).abs() <= 1.0e-9
    }) {
        return Err(BasementFoundationError::InvalidContour);
    }
    Ok(output)
}

fn bridge_hole(
    outer: &[[f32; 3]],
    inner: &[[f32; 3]],
) -> Result<Vec<[f32; 3]>, BasementFoundationError> {
    let hole_index = (0..inner.len())
        .max_by(|left, right| {
            inner[*left][0]
                .total_cmp(&inner[*right][0])
                .then_with(|| inner[*right][1].total_cmp(&inner[*left][1]))
        })
        .ok_or(BasementFoundationError::InvalidContour)?;
    let hole = inner[hole_index];
    let mut candidates = (0..outer.len())
        .map(|index| (distance_squared(xy(hole), xy(outer[index])), index))
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        left.0
            .total_cmp(&right.0)
            .then_with(|| outer[right.1][0].total_cmp(&outer[left.1][0]))
    });
    let mut bridged_outer = None;
    for (_, candidate_index) in candidates {
        let candidate = outer[candidate_index];
        if visible_bridge(xy(hole), xy(candidate), outer, inner) {
            bridged_outer = Some((outer.to_vec(), candidate_index));
            break;
        }
    }
    let (outer, outer_index) = bridged_outer.ok_or(BasementFoundationError::InvalidContour)?;
    let mut polygon = Vec::with_capacity(outer.len() + inner.len() + 2);
    for step in 0..outer.len() {
        let index = (outer_index + step) % outer.len();
        polygon.push(outer[index]);
        if step == 0 {
            polygon.push(inner[hole_index]);
            for hole_step in 1..inner.len() {
                polygon.push(inner[(hole_index + hole_step) % inner.len()]);
            }
            polygon.push(inner[hole_index]);
            polygon.push(outer[outer_index]);
        }
    }
    Ok(polygon)
}

fn visible_bridge(
    hole: [f64; 2],
    outer: [f64; 2],
    outer_ring: &[[f32; 3]],
    inner_ring: &[[f32; 3]],
) -> bool {
    let midpoint = [(hole[0] + outer[0]) * 0.5, (hole[1] + outer[1]) * 0.5];
    let outer_xy = outer_ring.iter().copied().map(xy).collect::<Vec<_>>();
    let inner_xy = inner_ring.iter().copied().map(xy).collect::<Vec<_>>();
    if !point_in_ring(midpoint, &outer_xy) || point_in_ring(midpoint, &inner_xy) {
        return false;
    }
    if (0..outer_xy.len()).any(|index| {
        segment_crosses_open_edge(
            hole,
            outer,
            outer_xy[index],
            outer_xy[(index + 1) % outer_xy.len()],
        )
    }) {
        return false;
    }
    !(0..inner_xy.len()).any(|index| {
        segment_crosses_open_edge(
            hole,
            outer,
            inner_xy[index],
            inner_xy[(index + 1) % inner_xy.len()],
        )
    })
}

fn segment_crosses_open_edge(
    first: [f64; 2],
    second: [f64; 2],
    edge_first: [f64; 2],
    edge_second: [f64; 2],
) -> bool {
    if same_point(first, edge_first)
        || same_point(first, edge_second)
        || same_point(second, edge_first)
        || same_point(second, edge_second)
    {
        return false;
    }
    segments_intersect(first, second, edge_first, edge_second)
}

fn same_point(first: [f64; 2], second: [f64; 2]) -> bool {
    distance_squared(first, second) <= POINT_EPSILON_SQUARED
}

fn ear_clip_weakly_simple_polygon(
    polygon: &[[f32; 3]],
) -> Result<Vec<[[f32; 3]; 3]>, BasementFoundationError> {
    let mut remaining = (0..polygon.len()).collect::<Vec<_>>();
    let mut triangles = Vec::with_capacity(polygon.len().saturating_sub(2));
    while remaining.len() > 3 {
        let mut ear = None;
        for slot in 0..remaining.len() {
            let previous = remaining[(slot + remaining.len() - 1) % remaining.len()];
            let current = remaining[slot];
            let next = remaining[(slot + 1) % remaining.len()];
            let a = xy(polygon[previous]);
            let b = xy(polygon[current]);
            let c = xy(polygon[next]);
            if cross2(a, b, c) <= 1.0e-9 || !diagonal_is_clear(previous, next, &remaining, polygon)
            {
                continue;
            }
            let contains_point = remaining.iter().copied().any(|candidate| {
                candidate != previous
                    && candidate != current
                    && candidate != next
                    && !same_xy(polygon[candidate], polygon[previous])
                    && !same_xy(polygon[candidate], polygon[current])
                    && !same_xy(polygon[candidate], polygon[next])
                    && point_in_or_on_triangle(xy(polygon[candidate]), a, b, c)
            });
            if !contains_point {
                ear = Some((slot, [polygon[previous], polygon[current], polygon[next]]));
                break;
            }
        }
        if let Some((slot, triangle)) = ear {
            triangles.push(triangle);
            remaining.remove(slot);
        } else {
            return Err(BasementFoundationError::InvalidContour);
        }
    }
    if remaining.len() == 3 {
        let triangle = [
            polygon[remaining[0]],
            polygon[remaining[1]],
            polygon[remaining[2]],
        ];
        if cross2(xy(triangle[0]), xy(triangle[1]), xy(triangle[2])) > 1.0e-9 {
            triangles.push(triangle);
        }
    }
    (!triangles.is_empty())
        .then_some(triangles)
        .ok_or(BasementFoundationError::InvalidContour)
}

fn diagonal_is_clear(
    first_index: usize,
    second_index: usize,
    polygon_indices: &[usize],
    polygon: &[[f32; 3]],
) -> bool {
    let first = xy(polygon[first_index]);
    let second = xy(polygon[second_index]);
    for edge in 0..polygon_indices.len() {
        let edge_first_index = polygon_indices[edge];
        let edge_second_index = polygon_indices[(edge + 1) % polygon_indices.len()];
        if edge_first_index == first_index
            || edge_second_index == first_index
            || edge_first_index == second_index
            || edge_second_index == second_index
        {
            continue;
        }
        if segment_crosses_open_edge(
            first,
            second,
            xy(polygon[edge_first_index]),
            xy(polygon[edge_second_index]),
        ) {
            return false;
        }
    }
    true
}

fn point_in_or_on_triangle(point: [f64; 2], a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> bool {
    let epsilon = 1.0e-9;
    cross2(a, b, point) >= -epsilon
        && cross2(b, c, point) >= -epsilon
        && cross2(c, a, point) >= -epsilon
}

fn nested_rings(outer: &[[f64; 2]], inner: &[[f64; 2]]) -> bool {
    if !simple_ring(outer) || !simple_ring(inner) {
        return false;
    }
    if rings_intersect(outer, inner) {
        return false;
    }
    point_in_ring(inner[0], outer)
}

fn nested_rings_3d(outer: &[[f32; 3]], inner: &[[f32; 3]]) -> bool {
    let outer_xy = outer.iter().copied().map(xy).collect::<Vec<_>>();
    let inner_xy = inner.iter().copied().map(xy).collect::<Vec<_>>();
    nested_rings(&outer_xy, &inner_xy)
}

fn remove_consecutive_duplicates(ring: &[[f32; 3]]) -> Vec<[f32; 3]> {
    let mut result = Vec::with_capacity(ring.len());
    for point in ring {
        if result
            .last()
            .is_none_or(|previous| !same_xy(*previous, *point))
        {
            result.push(*point);
        }
    }
    if result.len() > 1 && same_xy(result[0], *result.last().expect("nonempty")) {
        result.pop();
    }
    result
}

fn simple_ring(ring: &[[f64; 2]]) -> bool {
    if ring.len() < 3 || ring.iter().flatten().any(|value| !value.is_finite()) {
        return false;
    }
    let area = ring_area(ring);
    if !area.is_finite() || area.abs() <= 1.0e-10 {
        return false;
    }
    for first in 0..ring.len() {
        let first_next = (first + 1) % ring.len();
        if distance_squared(ring[first], ring[first_next]) <= POINT_EPSILON_SQUARED {
            return false;
        }
        for second in (first + 1)..ring.len() {
            let second_next = (second + 1) % ring.len();
            if first == second || first_next == second || second_next == first {
                continue;
            }
            if segments_intersect(
                ring[first],
                ring[first_next],
                ring[second],
                ring[second_next],
            ) {
                return false;
            }
        }
    }
    true
}

fn rings_intersect(first: &[[f64; 2]], second: &[[f64; 2]]) -> bool {
    (0..first.len()).any(|first_index| {
        (0..second.len()).any(|second_index| {
            segments_intersect(
                first[first_index],
                first[(first_index + 1) % first.len()],
                second[second_index],
                second[(second_index + 1) % second.len()],
            )
        })
    })
}

fn rings_overlap(first: &[[f64; 2]], second: &[[f64; 2]]) -> bool {
    rings_intersect(first, second)
        || point_in_ring(first[0], second)
        || point_in_ring(second[0], first)
}

fn point_in_ring(point: [f64; 2], ring: &[[f64; 2]]) -> bool {
    let mut inside = false;
    for index in 0..ring.len() {
        let first = ring[index];
        let second = ring[(index + 1) % ring.len()];
        if point_on_segment(point, first, second) {
            return true;
        }
        if (first[1] > point[1]) != (second[1] > point[1]) {
            let intersection_x =
                first[0] + (point[1] - first[1]) * (second[0] - first[0]) / (second[1] - first[1]);
            if point[0] < intersection_x {
                inside = !inside;
            }
        }
    }
    inside
}

fn point_on_segment(point: [f64; 2], first: [f64; 2], second: [f64; 2]) -> bool {
    let dx = second[0] - first[0];
    let dy = second[1] - first[1];
    let length = (dx * dx + dy * dy).sqrt();
    cross2(first, second, point).abs() <= LINE_EPSILON * length
        && point[0] >= first[0].min(second[0]) - LINE_EPSILON
        && point[0] <= first[0].max(second[0]) + LINE_EPSILON
        && point[1] >= first[1].min(second[1]) - LINE_EPSILON
        && point[1] <= first[1].max(second[1]) + LINE_EPSILON
}

fn segments_intersect(a: [f64; 2], b: [f64; 2], c: [f64; 2], d: [f64; 2]) -> bool {
    let ab_c = cross2(a, b, c);
    let ab_d = cross2(a, b, d);
    let cd_a = cross2(c, d, a);
    let cd_b = cross2(c, d, b);
    let epsilon = 1.0e-8;
    (ab_c.abs() <= epsilon && on_segment(c, a, b, epsilon))
        || (ab_d.abs() <= epsilon && on_segment(d, a, b, epsilon))
        || (cd_a.abs() <= epsilon && on_segment(a, c, d, epsilon))
        || (cd_b.abs() <= epsilon && on_segment(b, c, d, epsilon))
        || (ab_c * ab_d < 0.0 && cd_a * cd_b < 0.0)
}

fn on_segment(point: [f64; 2], first: [f64; 2], second: [f64; 2], epsilon: f64) -> bool {
    point[0] >= first[0].min(second[0]) - epsilon
        && point[0] <= first[0].max(second[0]) + epsilon
        && point[1] >= first[1].min(second[1]) - epsilon
        && point[1] <= first[1].max(second[1]) + epsilon
}

fn ring_area(ring: &[[f64; 2]]) -> f64 {
    (0..ring.len())
        .map(|index| {
            let first = ring[index];
            let second = ring[(index + 1) % ring.len()];
            first[0] * second[1] - second[0] * first[1]
        })
        .sum::<f64>()
        * 0.5
}

fn signed_area_3d(ring: &[[f32; 3]]) -> f64 {
    ring_area(&ring.iter().copied().map(xy).collect::<Vec<_>>())
}

fn xy(position: [f32; 3]) -> [f64; 2] {
    [f64::from(position[0]), f64::from(position[1])]
}

fn same_xy(first: [f32; 3], second: [f32; 3]) -> bool {
    distance_squared(xy(first), xy(second)) <= POINT_EPSILON_SQUARED
}

fn cross2(first: [f64; 2], second: [f64; 2], third: [f64; 2]) -> f64 {
    (second[0] - first[0]) * (third[1] - first[1]) - (second[1] - first[1]) * (third[0] - first[0])
}

fn distance_squared(first: [f64; 2], second: [f64; 2]) -> f64 {
    let dx = first[0] - second[0];
    let dy = first[1] - second[1];
    dx * dx + dy * dy
}

pub(crate) fn basement_surface_mask(positions: [[f32; 3]; 3]) -> FullSurfaceMask {
    let first = positions[1];
    let origin = positions[0];
    let second = positions[2];
    let normal_z = (first[0] - origin[0]) * (second[1] - origin[1])
        - (first[1] - origin[1]) * (second[0] - origin[0]);
    FullSurfaceMask(if normal_z < 0.0 { 0x0300 } else { 0x8300 })
}

fn legalize_constrained_edges(
    mut triangles: Vec<[[f32; 3]; 3]>,
    outer: &[[f32; 3]],
    inner: &[[f32; 3]],
) -> Result<Vec<[[f32; 3]; 3]>, BasementFoundationError> {
    let max_flips = triangles
        .len()
        .saturating_mul(triangles.len())
        .saturating_mul(4)
        .max(1);
    for _ in 0..max_flips {
        let mut flipped = false;
        'pairs: for first in 0..triangles.len() {
            for second in (first + 1)..triangles.len() {
                let Some((a, b, c, d)) = shared_edge(triangles[first], triangles[second]) else {
                    continue;
                };
                if ring_has_edge(outer, a, b) || ring_has_edge(inner, a, b) {
                    continue;
                }
                if !convex_quadrilateral(a, b, c, d) || in_circle(a, b, c, d) <= 1.0e-8 {
                    continue;
                }
                let first_triangle = orient_ccw([c, d, b]);
                let second_triangle = orient_ccw([d, c, a]);
                if cross2(
                    xy(first_triangle[0]),
                    xy(first_triangle[1]),
                    xy(first_triangle[2]),
                ) <= 1.0e-9
                    || cross2(
                        xy(second_triangle[0]),
                        xy(second_triangle[1]),
                        xy(second_triangle[2]),
                    ) <= 1.0e-9
                {
                    return Err(BasementFoundationError::InvalidContour);
                }
                triangles[first] = first_triangle;
                triangles[second] = second_triangle;
                flipped = true;
                break 'pairs;
            }
        }
        if !flipped {
            return Ok(triangles);
        }
    }
    Err(BasementFoundationError::InvalidContour)
}

fn shared_edge(
    first: [[f32; 3]; 3],
    second: [[f32; 3]; 3],
) -> Option<([f32; 3], [f32; 3], [f32; 3], [f32; 3])> {
    for edge in 0..3 {
        let a = first[edge];
        let b = first[(edge + 1) % 3];
        if let Some(opposite) = second
            .iter()
            .copied()
            .find(|point| !same_xy(*point, a) && !same_xy(*point, b))
        {
            let has_a = second.iter().any(|point| same_xy(*point, a));
            let has_b = second.iter().any(|point| same_xy(*point, b));
            if has_a && has_b {
                let first_opposite = first[(edge + 2) % 3];
                return Some((a, b, first_opposite, opposite));
            }
        }
    }
    None
}

fn ring_has_edge(ring: &[[f32; 3]], a: [f32; 3], b: [f32; 3]) -> bool {
    (0..ring.len()).any(|index| {
        let first = ring[index];
        let second = ring[(index + 1) % ring.len()];
        (same_xy(first, a) && same_xy(second, b)) || (same_xy(first, b) && same_xy(second, a))
    })
}

fn convex_quadrilateral(a: [f32; 3], b: [f32; 3], c: [f32; 3], d: [f32; 3]) -> bool {
    let epsilon = 1.0e-9;
    let c_side = cross2(xy(a), xy(b), xy(c));
    let d_side = cross2(xy(a), xy(b), xy(d));
    let a_side = cross2(xy(c), xy(d), xy(a));
    let b_side = cross2(xy(c), xy(d), xy(b));
    c_side * d_side < -epsilon && a_side * b_side < -epsilon
}

fn in_circle(a: [f32; 3], b: [f32; 3], c: [f32; 3], d: [f32; 3]) -> f64 {
    let a = [
        f64::from(a[0]) - f64::from(d[0]),
        f64::from(a[1]) - f64::from(d[1]),
    ];
    let b = [
        f64::from(b[0]) - f64::from(d[0]),
        f64::from(b[1]) - f64::from(d[1]),
    ];
    let c = [
        f64::from(c[0]) - f64::from(d[0]),
        f64::from(c[1]) - f64::from(d[1]),
    ];
    (a[0] * a[0] + a[1] * a[1]) * (b[0] * c[1] - b[1] * c[0])
        - (b[0] * b[0] + b[1] * b[1]) * (a[0] * c[1] - a[1] * c[0])
        + (c[0] * c[0] + c[1] * c[1]) * (a[0] * b[1] - a[1] * b[0])
}

fn orient_ccw(mut triangle: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    if cross2(xy(triangle[0]), xy(triangle[1]), xy(triangle[2])) < 0.0 {
        triangle.swap(1, 2);
    }
    triangle
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_terrain_format::{TerrainFace28, TerrainSlotTable};

    #[test]
    fn foundation_samples_lod0_planes_and_flattens_or_preserves_authored_inner_z() {
        let mesh = two_lod_mesh();
        let outer = [[0.4, 0.0], [4.0, 0.0], [4.0, 4.0], [0.0, 4.0], [0.4, 0.0]];
        let inner = [
            [1.0, 1.0, 10.0],
            [3.0, 1.0, 11.0],
            [3.0, 3.0, 12.0],
            [1.0, 3.0, 13.0],
            [1.0, 1.0, 10.0],
        ];
        let (open_outer, _) = normalize_ring(&outer).expect("outer ring");
        let snapped = normalize_outer_points(&open_outer, &[0, 1, 2], &mesh)
            .expect("outer points lie within LOD0");
        assert_eq!(snapped.first(), Some(&[0.0, 0.0]));
        assert!(!snapped.contains(&[2.0, 0.0]));
        assert!(native_segment_inserts_vertex(
            [0.0, 0.0],
            [4.0, 0.0],
            [3.96875, 0.4991, 12.0]
        ));
        assert!(!native_segment_inserts_vertex(
            [0.0, 0.0],
            [4.0, 0.0],
            [2.0, 0.0, 12.0]
        ));
        let terrain = [
            (
                0,
                mesh.faces[0].flags,
                terrain_face_positions(&mesh, 0).unwrap(),
            ),
            (
                1,
                mesh.faces[1].flags,
                terrain_face_positions(&mesh, 1).unwrap(),
            ),
            (
                2,
                mesh.faces[2].flags,
                terrain_face_positions(&mesh, 2).unwrap(),
            ),
        ];
        let crossings = contour_crossings(&snapped, &terrain).expect("LOD0 crossings");
        assert!(crossings.len() > 4);
        assert_eq!(crossings.first().unwrap()[2], 0.0);
        assert!(crossings
            .iter()
            .any(|crossing| same_xy(*crossing, [2.0, 0.0, 2.0])));
        let expected_mean =
            crossings.iter().map(|crossing| crossing[2]).sum::<f32>() / crossings.len() as f32;

        let flat = build_basement_foundations(
            &mesh,
            &[BasementFootprint {
                outer: &outer,
                inner: &inner,
                flatten_inner_to_mean_outer_z: true,
            }],
        )
        .expect("flat foundation");
        let placement = flat.placements[0];
        assert!((placement.placement_delta - (10.0 - expected_mean)).abs() < 1.0e-6);
        let flat_band = flat
            .fragments
            .iter()
            .filter(|fragment| fragment.source_face.is_none())
            .collect::<Vec<_>>();
        assert!(flat
            .fragments
            .iter()
            .filter_map(|fragment| fragment.source_face)
            .all(|face| face < 3));
        assert!(!flat_band.is_empty());
        assert!(flat_band
            .iter()
            .all(|fragment| fragment.surface_mask == FullSurfaceMask(0x8300)));
        for point in &inner[..4] {
            assert!(flat_band
                .iter()
                .flat_map(|fragment| fragment.positions)
                .any(|vertex| {
                    same_xy(vertex, *point) && (vertex[2] - expected_mean).abs() < 1.0e-6
                }));
        }
        let flat_area = flat_band
            .iter()
            .map(|fragment| {
                let [a, b, c] = fragment.positions;
                cross2(xy(a), xy(b), xy(c)).abs() * 0.5
            })
            .sum::<f64>();
        assert!((flat_area - 12.0).abs() < 1.0e-5);

        let authored = build_basement_foundations(
            &mesh,
            &[BasementFootprint {
                outer: &outer,
                inner: &inner,
                flatten_inner_to_mean_outer_z: false,
            }],
        )
        .expect("authored-height foundation");
        let authored_band = authored
            .fragments
            .iter()
            .filter(|fragment| fragment.source_face.is_none())
            .collect::<Vec<_>>();
        for point in &inner[..4] {
            assert!(authored_band
                .iter()
                .flat_map(|fragment| fragment.positions)
                .any(|vertex| {
                    same_xy(vertex, *point) && (vertex[2] - point[2]).abs() < 1.0e-6
                }));
        }
    }

    #[test]
    fn foundation_rejects_outer_contours_outside_or_across_uncovered_terrain() {
        let mesh = two_lod_mesh();
        let outside = [[5.0, 1.0], [5.5, 1.0], [5.0, 1.5]];
        let inner = [[5.1, 1.1, 0.0], [5.3, 1.1, 0.0], [5.1, 1.3, 0.0]];
        assert_eq!(
            build_basement_foundations(
                &mesh,
                &[BasementFootprint {
                    outer: &outside,
                    inner: &inner,
                    flatten_inner_to_mean_outer_z: true,
                }],
            ),
            Err(BasementFoundationError::OuterPointOutsideLod0 { point: 0 })
        );

        let mut islands = two_lod_mesh();
        islands.positions = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [2.0, 2.0, 0.0],
            [3.0, 2.0, 0.0],
            [2.0, 3.0, 0.0],
        ];
        islands.faces.truncate(2);
        islands.faces[0].vertices = [0, 1, 2];
        islands.faces[1].vertices = [3, 4, 5];
        islands.slots.slots_raw[0][2..4].copy_from_slice(&2_u16.to_le_bytes());
        islands.slots.slots_raw[1][0..2].copy_from_slice(&2_u16.to_le_bytes());
        islands.slots.slots_raw[1][2..4].copy_from_slice(&0_u16.to_le_bytes());
        let outer = [[0.1, 0.1], [0.7, 0.1], [2.1, 2.1]];
        let inner = [[0.2, 0.2, 0.0], [0.4, 0.2, 0.0], [0.2, 0.4, 0.0]];
        assert_eq!(
            build_basement_foundations(
                &islands,
                &[BasementFootprint {
                    outer: &outer,
                    inner: &inner,
                    flatten_inner_to_mean_outer_z: true,
                }],
            ),
            Err(BasementFoundationError::UncoveredOuterSegment { segment: 1 })
        );
    }

    #[test]
    fn shared_diagonal_crossing_is_exact_and_counted_once() {
        let first = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 1.0]];
        let second = [[0.0, 0.0, 0.0], [1.0, 1.0, 1.0], [0.0, 1.0, 2.0]];
        let edge_start = [0.8, 0.5];
        let edge_end = [0.5, 0.8];
        let first_interval =
            segment_triangle_interval(edge_start, edge_end, first).expect("first face interval");
        let second_interval =
            segment_triangle_interval(edge_start, edge_end, second).expect("second face interval");
        assert!((first_interval.1 - 0.5).abs() < 1.0e-9);
        assert!((second_interval.0 - 0.5).abs() < 1.0e-9);

        let ring = [[0.5, 0.2], [0.8, 0.5], [0.5, 0.8], [0.2, 0.5]];
        let crossings = contour_crossings(
            &ring,
            &[
                (0, FullSurfaceMask(1), first),
                (1, FullSurfaceMask(2), second),
            ],
        )
        .expect("shared-edge crossings");
        let shared = crossings
            .iter()
            .filter(|crossing| {
                distance_squared(
                    [f64::from(crossing[0]), f64::from(crossing[1])],
                    [0.65, 0.65],
                ) <= POINT_EPSILON_SQUARED
            })
            .collect::<Vec<_>>();
        assert_eq!(
            shared.len(),
            1,
            "shared-edge crossing duplicated: {shared:?}"
        );
        assert!((shared[0][2] - 0.65).abs() < 1.0e-6);
    }

    #[test]
    fn annulus_preserves_collinear_outer_height_breakpoints_and_is_constrained_delaunay() {
        let outer = [
            [0.0, 0.0, 0.0],
            [2.0, 0.0, 3.0],
            [4.0, 0.0, 10.0],
            [4.0, 4.0, 12.0],
            [0.0, 4.0, 0.0],
        ];
        let inner = vec![
            [1.0, 1.0, 5.0],
            [3.0, 1.0, 5.0],
            [3.0, 3.0, 5.0],
            [1.0, 3.0, 5.0],
        ];
        let triangles = triangulate_annulus(&outer, inner.clone()).expect("annulus");
        for edge in outer.windows(2) {
            assert!(
                triangles
                    .iter()
                    .any(|triangle| has_xy_edge(*triangle, edge[0], edge[1])),
                "missing outer boundary segment {:?} -> {:?}",
                edge[0],
                edge[1]
            );
        }
        for point in outer {
            assert!(
                triangles
                    .iter()
                    .flat_map(|triangle| triangle.iter())
                    .any(|vertex| {
                        same_xy(*vertex, point) && (vertex[2] - point[2]).abs() < 1.0e-6
                    }),
                "lost outer breakpoint {point:?}"
            );
        }
        assert_locally_delaunay(&triangles, &outer, &inner);
    }

    #[test]
    fn constrained_annulus_handles_concave_rings_with_different_vertex_counts() {
        let outer = [
            [0.0, 0.0, 1.0],
            [10.0, 0.0, 2.0],
            [10.0, 3.0, 3.0],
            [4.0, 3.0, 4.0],
            [4.0, 10.0, 5.0],
            [0.0, 10.0, 6.0],
        ];
        let inner = vec![
            [1.0, 1.0, 3.0],
            [3.0, 1.0, 3.0],
            [3.0, 1.8, 3.0],
            [2.2, 1.8, 3.0],
            [2.2, 2.4, 3.0],
            [3.0, 2.4, 3.0],
            [3.0, 3.0, 3.0],
            [1.0, 3.0, 3.0],
        ];
        let band = triangulate_annulus(&outer, inner.clone()).expect("constrained annulus");
        let area = band
            .iter()
            .map(|triangle| cross2(xy(triangle[0]), xy(triangle[1]), xy(triangle[2])).abs() * 0.5)
            .sum::<f64>();
        let expected = ring_area(&outer.iter().copied().map(xy).collect::<Vec<_>>()).abs()
            - ring_area(&inner.iter().copied().map(xy).collect::<Vec<_>>()).abs();
        assert!(
            (area - expected).abs() < 1.0e-5,
            "area {area}, expected {expected}"
        );

        let outer_xy = outer.iter().copied().map(xy).collect::<Vec<_>>();
        let inner_xy = inner.iter().copied().map(xy).collect::<Vec<_>>();
        for triangle in &band {
            let centroid = triangle_centroid(*triangle);
            assert!(point_in_ring(centroid, &outer_xy));
            assert!(!point_in_ring(centroid, &inner_xy));
        }
        for first in 0..band.len() {
            for second in (first + 1)..band.len() {
                assert!(!positive_area_overlap(band[first], band[second]));
            }
        }
        assert_locally_delaunay(&band, &outer, &inner);
    }

    #[test]
    fn multiple_disjoint_footprints_clip_canonical_outside_once() {
        let mesh = two_lod_mesh();
        let first_outer = [[0.6, 0.6], [1.7, 0.6], [1.7, 1.7], [0.6, 1.7]];
        let first_inner = [
            [0.9, 0.9, 5.0],
            [1.4, 0.9, 5.0],
            [1.4, 1.4, 5.0],
            [0.9, 1.4, 5.0],
        ];
        let second_outer = [[2.3, 2.3], [3.4, 2.3], [3.4, 3.4], [2.3, 3.4]];
        let second_inner = [
            [2.6, 2.6, 7.0],
            [3.1, 2.6, 7.0],
            [3.1, 3.1, 7.0],
            [2.6, 3.1, 7.0],
        ];
        let output = build_basement_foundations(
            &mesh,
            &[
                BasementFootprint {
                    outer: &first_outer,
                    inner: &first_inner,
                    flatten_inner_to_mean_outer_z: true,
                },
                BasementFootprint {
                    outer: &second_outer,
                    inner: &second_inner,
                    flatten_inner_to_mean_outer_z: true,
                },
            ],
        )
        .expect("two disjoint foundations");
        assert_eq!(output.placements.len(), 2);
        let terrain = output
            .fragments
            .iter()
            .filter(|fragment| fragment.source_face.is_some())
            .collect::<Vec<_>>();
        let bands = output
            .fragments
            .iter()
            .filter(|fragment| fragment.source_face.is_none())
            .collect::<Vec<_>>();
        assert!(!terrain.is_empty());
        assert!(!bands.is_empty());
        for fragment in &terrain {
            let centroid = triangle_centroid(fragment.positions);
            assert!(!point_strictly_in_ring(centroid, &ring_f64(&first_outer)));
            assert!(!point_strictly_in_ring(centroid, &ring_f64(&second_outer)));
        }
        for first in 0..terrain.len() {
            for second in (first + 1)..terrain.len() {
                assert!(!positive_area_overlap(
                    terrain[first].positions,
                    terrain[second].positions
                ));
            }
        }
        for band in bands {
            let centroid = triangle_centroid(band.positions);
            assert!(
                point_in_ring(centroid, &ring_f64(&first_outer))
                    || point_in_ring(centroid, &ring_f64(&second_outer))
            );
            assert!(!point_in_ring(
                centroid,
                &ring_f64(&first_inner.iter().map(|p| [p[0], p[1]]).collect::<Vec<_>>())
            ));
            assert!(!point_in_ring(
                centroid,
                &ring_f64(
                    &second_inner
                        .iter()
                        .map(|p| [p[0], p[1]])
                        .collect::<Vec<_>>()
                )
            ));
        }
    }

    #[test]
    fn overlapping_footprints_are_explicitly_rejected() {
        let mesh = two_lod_mesh();
        let outer_a = [[0.5, 0.5], [2.5, 0.5], [2.5, 2.5], [0.5, 2.5]];
        let inner_a = [
            [1.0, 1.0, 3.0],
            [2.0, 1.0, 3.0],
            [2.0, 2.0, 3.0],
            [1.0, 2.0, 3.0],
        ];
        let outer_b = [[1.5, 1.5], [3.5, 1.5], [3.5, 3.5], [1.5, 3.5]];
        let inner_b = [
            [2.0, 2.0, 3.0],
            [3.0, 2.0, 3.0],
            [3.0, 3.0, 3.0],
            [2.0, 3.0, 3.0],
        ];
        assert_eq!(
            build_basement_foundations(
                &mesh,
                &[
                    BasementFootprint {
                        outer: &outer_a,
                        inner: &inner_a,
                        flatten_inner_to_mean_outer_z: true
                    },
                    BasementFootprint {
                        outer: &outer_b,
                        inner: &inner_b,
                        flatten_inner_to_mean_outer_z: true
                    },
                ],
            ),
            Err(BasementFoundationError::OverlappingFootprints)
        );
    }

    #[test]
    fn generated_flags_follow_native_normal_z_branch() {
        let positive = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let negative = [positive[0], positive[2], positive[1]];
        assert_eq!(basement_surface_mask(positive), FullSurfaceMask(0x8300));
        assert_eq!(basement_surface_mask(negative), FullSurfaceMask(0x0300));
    }

    #[test]
    fn foundation_rejects_inner_ring_that_is_not_nested() {
        let mesh = two_lod_mesh();
        let outer = [[0.0, 0.0], [4.0, 0.0], [4.0, 4.0], [0.0, 4.0]];
        let inner = [
            [3.0, 1.0, 0.0],
            [5.0, 1.0, 0.0],
            [5.0, 3.0, 0.0],
            [3.0, 3.0, 0.0],
        ];
        assert_eq!(
            build_basement_foundations(
                &mesh,
                &[BasementFootprint {
                    outer: &outer,
                    inner: &inner,
                    flatten_inner_to_mean_outer_z: true
                }],
            ),
            Err(BasementFoundationError::InvalidContour)
        );
    }

    fn assert_locally_delaunay(
        triangles: &[[[f32; 3]; 3]],
        outer: &[[f32; 3]],
        inner: &[[f32; 3]],
    ) {
        for first in 0..triangles.len() {
            for second in (first + 1)..triangles.len() {
                let Some((a, b, c, d)) = shared_edge(triangles[first], triangles[second]) else {
                    continue;
                };
                if ring_has_edge(outer, a, b)
                    || ring_has_edge(inner, a, b)
                    || !convex_quadrilateral(a, b, c, d)
                {
                    continue;
                }
                assert!(
                    in_circle(a, b, c, d) <= 1.0e-8,
                    "illegal diagonal {a:?}->{b:?}"
                );
            }
        }
    }

    fn has_xy_edge(triangle: [[f32; 3]; 3], first: [f32; 3], second: [f32; 3]) -> bool {
        (0..3).any(|index| {
            (same_xy(triangle[index], first) && same_xy(triangle[(index + 1) % 3], second))
                || (same_xy(triangle[index], second) && same_xy(triangle[(index + 1) % 3], first))
        })
    }

    fn triangle_centroid(triangle: [[f32; 3]; 3]) -> [f64; 2] {
        [
            triangle
                .iter()
                .map(|point| f64::from(point[0]))
                .sum::<f64>()
                / 3.0,
            triangle
                .iter()
                .map(|point| f64::from(point[1]))
                .sum::<f64>()
                / 3.0,
        ]
    }

    fn point_strictly_in_ring(point: [f64; 2], ring: &[[f64; 2]]) -> bool {
        !((0..ring.len())
            .any(|index| point_on_segment(point, ring[index], ring[(index + 1) % ring.len()])))
            && point_in_ring(point, ring)
    }

    fn ring_f64(ring: &[[f32; 2]]) -> Vec<[f64; 2]> {
        ring.iter()
            .map(|point| [f64::from(point[0]), f64::from(point[1])])
            .collect()
    }

    fn two_lod_mesh() -> LandMeshDocument {
        let positions = vec![
            [0.0, 0.0, 0.0],
            [2.0, 0.0, 2.0],
            [4.0, 0.0, 4.0],
            [0.0, 4.0, 4.0],
            [4.0, 4.0, 8.0],
            [0.0, 0.0, 100.0],
            [4.0, 0.0, 100.0],
            [0.0, 4.0, 100.0],
        ];
        let face = |flags, vertices| TerrainFace28 {
            flags: FullSurfaceMask(flags),
            material_tag: 0,
            aux_tag: 0,
            vertices,
            neighbors: [None; 3],
            tail_raw: [0; 8],
            raw: [0; 28],
        };
        let faces = vec![
            face(1, [0, 1, 3]),
            face(2, [1, 4, 3]),
            face(4, [1, 2, 4]),
            face(8, [5, 6, 7]),
        ];
        let mut nodes_raw = vec![u8::MAX; 38];
        nodes_raw[8..10].copy_from_slice(&0_u16.to_le_bytes());
        nodes_raw[10..12].copy_from_slice(&1_u16.to_le_bytes());
        let mut slots_raw = vec![[0_u8; 68]; 2];
        slots_raw[0][2..4].copy_from_slice(&3_u16.to_le_bytes());
        slots_raw[1][0..2].copy_from_slice(&3_u16.to_le_bytes());
        slots_raw[1][2..4].copy_from_slice(&1_u16.to_le_bytes());
        LandMeshDocument {
            streams: Vec::new(),
            nodes_raw,
            slots: TerrainSlotTable {
                header_raw: Vec::new(),
                slots_raw,
            },
            positions,
            normals: Vec::new(),
            uv0: Vec::new(),
            accelerator: Vec::new(),
            aux14: Vec::new(),
            aux18: Vec::new(),
            faces,
        }
    }

    fn positive_area_overlap(first: [[f32; 3]; 3], second: [[f32; 3]; 3]) -> bool {
        for triangle in [first, second] {
            for edge in 0..3 {
                let a = xy(triangle[edge]);
                let b = xy(triangle[(edge + 1) % 3]);
                let axis = [b[1] - a[1], a[0] - b[0]];
                let project = |vertices: [[f32; 3]; 3]| {
                    vertices.map(|vertex| {
                        let point = xy(vertex);
                        point[0] * axis[0] + point[1] * axis[1]
                    })
                };
                let first_projection = project(first);
                let second_projection = project(second);
                let first_min = first_projection
                    .iter()
                    .copied()
                    .fold(f64::INFINITY, f64::min);
                let first_max = first_projection
                    .iter()
                    .copied()
                    .fold(f64::NEG_INFINITY, f64::max);
                let second_min = second_projection
                    .iter()
                    .copied()
                    .fold(f64::INFINITY, f64::min);
                let second_max = second_projection
                    .iter()
                    .copied()
                    .fold(f64::NEG_INFINITY, f64::max);
                if first_max <= second_min + 1.0e-8 || second_max <= first_min + 1.0e-8 {
                    return false;
                }
            }
        }
        true
    }
}
