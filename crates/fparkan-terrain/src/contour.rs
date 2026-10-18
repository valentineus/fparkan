/// One triangle fragment retained after subtracting a building contour.
/// `source_triangle` indexes the triangle array passed to
/// [`subtract_contour`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TerrainTriangleFragment {
    /// Source triangle index, for preserving its material and vertex channels.
    pub source_triangle: usize,
    /// Fragment triangle vertices in the input coordinate space.
    pub positions: [[f32; 3]; 3],
}

/// Error returned when a terrain contour cannot be subtracted safely.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TerrainContourError {
    /// The contour is non-finite, degenerate, or self-intersecting.
    UnsupportedContour,
    /// A source triangle has non-finite coordinates or a degenerate XY plane.
    InvalidSourceTriangle {
        /// Index of the source triangle.
        triangle: usize,
    },
}

impl std::fmt::Display for TerrainContourError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedContour => write!(
                f,
                "terrain contour must be finite, simple, and non-degenerate"
            ),
            Self::InvalidSourceTriangle { triangle } => {
                write!(
                    f,
                    "terrain source triangle {triangle} is not a finite XY surface"
                )
            }
        }
    }
}

impl std::error::Error for TerrainContourError {}

/// Subtracts a finite simple contour using the native per-source-face point set:
/// the three source vertices plus the actual contour segments clipped to that
/// source triangle. It does not extend cuts across the triangle or triangulate
/// the contour into unrelated internal edges.
///
/// # Errors
///
/// Returns [`TerrainContourError::UnsupportedContour`] for malformed or
/// self-intersecting contours, and
/// [`TerrainContourError::InvalidSourceTriangle`] when a source face cannot be
/// triangulated or lifted onto its original plane.
pub(crate) fn subtract_contour(
    triangles: &[[[f32; 3]; 3]],
    contour: &[[f32; 2]],
) -> Result<Vec<TerrainTriangleFragment>, TerrainContourError> {
    let ring = normalize_ring(contour)?;
    let contour_min: [f64; 2] = std::array::from_fn(|axis| {
        ring.iter()
            .map(|point| point[axis])
            .fold(f64::INFINITY, f64::min)
    });
    let contour_max: [f64; 2] = std::array::from_fn(|axis| {
        ring.iter()
            .map(|point| point[axis])
            .fold(f64::NEG_INFINITY, f64::max)
    });
    let mut output = Vec::new();

    for (source_triangle, source) in triangles.iter().copied().enumerate() {
        if !source.iter().flatten().all(|value| value.is_finite()) {
            return Err(TerrainContourError::InvalidSourceTriangle {
                triangle: source_triangle,
            });
        }
        let source_xy = source.map(|point| [f64::from(point[0]), f64::from(point[1])]);
        let source_area = cross2(source_xy[0], source_xy[1], source_xy[2]);
        if source_area.abs() <= 1.0e-10 {
            return Err(TerrainContourError::InvalidSourceTriangle {
                triangle: source_triangle,
            });
        }
        let source_min: [f64; 2] = std::array::from_fn(|axis| {
            source_xy
                .iter()
                .map(|point| point[axis])
                .fold(f64::INFINITY, f64::min)
        });
        let source_max: [f64; 2] = std::array::from_fn(|axis| {
            source_xy
                .iter()
                .map(|point| point[axis])
                .fold(f64::NEG_INFINITY, f64::max)
        });
        if (0..2).any(|axis| {
            source_max[axis] < contour_min[axis] || contour_max[axis] < source_min[axis]
        }) {
            output.push(TerrainTriangleFragment {
                source_triangle,
                positions: source,
            });
            continue;
        }

        let mut vertices = source_xy.to_vec();
        let initial = if source_area > 0.0 {
            [0, 1, 2]
        } else {
            [0, 2, 1]
        };
        let mut triangulation = vec![initial];
        let mut clipped_segments = Vec::new();
        for index in 0..ring.len() {
            let first = ring[index];
            let second = ring[(index + 1) % ring.len()];
            if let Some((start, end)) = clip_segment_to_triangle(first, second, source_xy) {
                let start_id = insert_unique_point(&mut vertices, start);
                let end_id = insert_unique_point(&mut vertices, end);
                if start_id != end_id {
                    clipped_segments.push((start_id, end_id));
                }
            }
        }

        if clipped_segments.is_empty() {
            let center = [
                (source_xy[0][0] + source_xy[1][0] + source_xy[2][0]) / 3.0,
                (source_xy[0][1] + source_xy[1][1] + source_xy[2][1]) / 3.0,
            ];
            if !point_in_ring(center, &ring) {
                output.push(TerrainTriangleFragment {
                    source_triangle,
                    positions: source,
                });
            }
            continue;
        }

        for point_id in 3..vertices.len() {
            insert_triangulation_point(point_id, &vertices, &mut triangulation).map_err(|()| {
                TerrainContourError::InvalidSourceTriangle {
                    triangle: source_triangle,
                }
            })?;
        }

        let mut constraints = source_boundary_constraints(&vertices, source_xy);
        for (first, second) in clipped_segments {
            let mut on_segment = vertices
                .iter()
                .copied()
                .enumerate()
                .filter_map(|(index, point)| {
                    point_on_segment(point, vertices[first], vertices[second]).then_some((
                        segment_parameter(point, vertices[first], vertices[second]),
                        index,
                    ))
                })
                .collect::<Vec<_>>();
            on_segment.sort_by(|left, right| left.0.total_cmp(&right.0));
            on_segment.dedup_by_key(|(_, index)| *index);
            for pair in on_segment.windows(2) {
                let edge = canonical_edge(pair[0].1, pair[1].1);
                if edge.0 == edge.1 {
                    continue;
                }
                recover_constraint(edge, &mut triangulation, &vertices, &constraints).map_err(
                    |()| TerrainContourError::InvalidSourceTriangle {
                        triangle: source_triangle,
                    },
                )?;
                constraints.insert(edge);
            }
        }
        legalize_constrained_delaunay(&mut triangulation, &vertices, &constraints).map_err(
            |()| TerrainContourError::InvalidSourceTriangle {
                triangle: source_triangle,
            },
        )?;

        for triangle in triangulation {
            let center = triangle.map(|index| vertices[index]);
            let center = [
                (center[0][0] + center[1][0] + center[2][0]) / 3.0,
                (center[0][1] + center[1][1] + center[2][1]) / 3.0,
            ];
            if point_in_ring(center, &ring) {
                continue;
            }
            let mut positions: [[f32; 3]; 3] = triangle
                .map(|index| lift_to_source_plane(vertices[index], source))
                .into_iter()
                .collect::<Option<Vec<_>>>()
                .and_then(|points| points.try_into().ok())
                .ok_or(TerrainContourError::InvalidSourceTriangle {
                    triangle: source_triangle,
                })?;
            if source_area < 0.0 {
                positions.swap(1, 2);
            }
            output.push(TerrainTriangleFragment {
                source_triangle,
                positions,
            });
        }
    }
    Ok(output)
}

fn normalize_ring(contour: &[[f32; 2]]) -> Result<Vec<[f64; 2]>, TerrainContourError> {
    let mut ring = contour
        .iter()
        .map(|point| [f64::from(point[0]), f64::from(point[1])])
        .collect::<Vec<_>>();
    if ring.iter().flatten().any(|value| !value.is_finite()) {
        return Err(TerrainContourError::UnsupportedContour);
    }
    if ring.len() > 1 && distance_squared(ring[0], *ring.last().expect("nonempty")) <= 1.0e-16 {
        ring.pop();
    }
    let mut normalized = Vec::with_capacity(ring.len());
    for point in ring {
        if normalized
            .last()
            .is_none_or(|previous| distance_squared(*previous, point) > 1.0e-16)
        {
            normalized.push(point);
        }
    }
    if normalized.len() > 1
        && distance_squared(normalized[0], *normalized.last().expect("nonempty")) <= 1.0e-16
    {
        normalized.pop();
    }
    if normalized.len() < 3 {
        return Err(TerrainContourError::UnsupportedContour);
    }
    let signed_area = polygon_area(&normalized);
    if !signed_area.is_finite() || signed_area.abs() <= 1.0e-10 {
        return Err(TerrainContourError::UnsupportedContour);
    }
    for first in 0..normalized.len() {
        let first_next = (first + 1) % normalized.len();
        for second in (first + 1)..normalized.len() {
            let second_next = (second + 1) % normalized.len();
            if first_next == second || second_next == first {
                continue;
            }
            if segments_intersect(
                normalized[first],
                normalized[first_next],
                normalized[second],
                normalized[second_next],
            ) {
                return Err(TerrainContourError::UnsupportedContour);
            }
        }
    }
    if signed_area < 0.0 {
        normalized.reverse();
    }
    Ok(normalized)
}

fn clip_segment_to_triangle(
    first: [f64; 2],
    second: [f64; 2],
    triangle: [[f64; 2]; 3],
) -> Option<([f64; 2], [f64; 2])> {
    let mut triangle = triangle;
    if cross2(triangle[0], triangle[1], triangle[2]) < 0.0 {
        triangle.swap(1, 2);
    }
    let mut start = 0.0_f64;
    let mut end = 1.0_f64;
    for edge in 0..3 {
        let edge_first = triangle[edge];
        let edge_second = triangle[(edge + 1) % 3];
        let at_start = cross2(edge_first, edge_second, first);
        let at_end = cross2(edge_first, edge_second, second);
        let change = at_end - at_start;
        if change.abs() <= 1.0e-14 {
            if at_start < -1.0e-10 {
                return None;
            }
            continue;
        }
        let crossing = -at_start / change;
        if change > 0.0 {
            start = start.max(crossing);
        } else {
            end = end.min(crossing);
        }
        if start > end + 1.0e-12 {
            return None;
        }
    }
    start = start.clamp(0.0, 1.0);
    end = end.clamp(0.0, 1.0);
    if end - start <= 1.0e-12 {
        return None;
    }
    Some((
        interpolate(first, second, start),
        interpolate(first, second, end),
    ))
}

fn insert_unique_point(vertices: &mut Vec<[f64; 2]>, point: [f64; 2]) -> usize {
    // Terrain positions are f32 throughout the native data and renderer. Make
    // clipped crossings use that same representable coordinate before building
    // the local CDT, so distinct f64 intersections cannot collapse to one XY
    // vertex only after fragment emission.
    let point = [f64::from(point[0] as f32), f64::from(point[1] as f32)];
    if let Some(index) = vertices
        .iter()
        .position(|candidate| distance_squared(*candidate, point) <= 1.0e-18)
    {
        return index;
    }
    let index = vertices.len();
    vertices.push(point);
    index
}

fn insert_triangulation_point(
    point_id: usize,
    vertices: &[[f64; 2]],
    triangles: &mut Vec<[usize; 3]>,
) -> Result<(), ()> {
    let point = vertices[point_id];
    let edge = triangulation_edges(triangles)
        .keys()
        .copied()
        .find(|&(first, second)| point_on_open_segment(point, vertices[first], vertices[second]));
    if let Some(edge) = edge {
        let mut split = Vec::new();
        triangles.retain(|triangle| {
            if triangle_has_edge(*triangle, edge) {
                let opposite = triangle
                    .iter()
                    .copied()
                    .find(|index| *index != edge.0 && *index != edge.1)
                    .expect("triangle has opposite vertex");
                split.push(opposite);
                false
            } else {
                true
            }
        });
        if split.is_empty() {
            return Err(());
        }
        for opposite in split {
            let first = make_ccw([edge.0, point_id, opposite], vertices).ok_or(())?;
            let second = make_ccw([point_id, edge.1, opposite], vertices).ok_or(())?;
            triangles.extend([first, second]);
        }
        return Ok(());
    }

    let containing = triangles.iter().position(|triangle| {
        point_in_triangle(
            point,
            vertices[triangle[0]],
            vertices[triangle[1]],
            vertices[triangle[2]],
        )
    });
    let Some(containing) = containing else {
        return Err(());
    };
    let triangle = triangles.swap_remove(containing);
    for edge in 0..3 {
        let split = make_ccw(
            [triangle[edge], triangle[(edge + 1) % 3], point_id],
            vertices,
        )
        .ok_or(())?;
        triangles.push(split);
    }
    Ok(())
}

fn source_boundary_constraints(
    vertices: &[[f64; 2]],
    triangle: [[f64; 2]; 3],
) -> std::collections::BTreeSet<(usize, usize)> {
    let mut constraints = std::collections::BTreeSet::new();
    for edge in 0..3 {
        let first = triangle[edge];
        let second = triangle[(edge + 1) % 3];
        let mut points = vertices
            .iter()
            .copied()
            .enumerate()
            .filter_map(|(index, point)| {
                point_on_segment(point, first, second)
                    .then_some((segment_parameter(point, first, second), index))
            })
            .collect::<Vec<_>>();
        points.sort_by(|left, right| left.0.total_cmp(&right.0));
        points.dedup_by_key(|(_, index)| *index);
        for pair in points.windows(2) {
            constraints.insert(canonical_edge(pair[0].1, pair[1].1));
        }
    }
    constraints
}

fn recover_constraint(
    target: (usize, usize),
    triangles: &mut [[usize; 3]],
    vertices: &[[f64; 2]],
    constraints: &std::collections::BTreeSet<(usize, usize)>,
) -> Result<(), ()> {
    let limit = vertices
        .len()
        .saturating_mul(vertices.len())
        .saturating_mul(8)
        .max(1);
    for _ in 0..limit {
        if edge_exists(triangles, target) {
            return Ok(());
        }
        let mut crossed = triangulation_edges(triangles)
            .into_iter()
            .filter_map(|(edge, adjacent)| {
                if adjacent.len() != 2
                    || edge.0 == target.0
                    || edge.0 == target.1
                    || edge.1 == target.0
                    || edge.1 == target.1
                {
                    return None;
                }
                proper_intersection_parameter(
                    vertices[target.0],
                    vertices[target.1],
                    vertices[edge.0],
                    vertices[edge.1],
                )
                .map(|parameter| (parameter, edge, adjacent))
            })
            .collect::<Vec<_>>();
        crossed.sort_by(|left, right| left.0.total_cmp(&right.0));
        let mut flipped = false;
        for (_, edge, adjacent) in crossed {
            if constraints.contains(&edge) {
                return Err(());
            }
            let first_index = adjacent[0].0;
            let second_index = adjacent[1].0;
            let first_opposite = adjacent[0].1;
            let second_opposite = adjacent[1].1;
            if !convex_quadrilateral(
                vertices[edge.0],
                vertices[edge.1],
                vertices[first_opposite],
                vertices[second_opposite],
            ) {
                continue;
            }
            let replacement = canonical_edge(first_opposite, second_opposite);
            if constraints.contains(&replacement)
                || proper_intersection_parameter(
                    vertices[target.0],
                    vertices[target.1],
                    vertices[replacement.0],
                    vertices[replacement.1],
                )
                .is_some()
            {
                continue;
            }
            let (a, b) = directed_edge(triangles[first_index], edge).ok_or(())?;
            triangles[first_index] =
                make_ccw([first_opposite, second_opposite, b], vertices).ok_or(())?;
            triangles[second_index] =
                make_ccw([second_opposite, first_opposite, a], vertices).ok_or(())?;
            flipped = true;
            break;
        }
        if !flipped {
            return Err(());
        }
    }
    Err(())
}

fn legalize_constrained_delaunay(
    triangles: &mut [[usize; 3]],
    vertices: &[[f64; 2]],
    constraints: &std::collections::BTreeSet<(usize, usize)>,
) -> Result<(), ()> {
    let limit = triangles
        .len()
        .saturating_mul(triangles.len())
        .saturating_mul(8)
        .max(1);
    for _ in 0..limit {
        let edges = triangulation_edges(triangles);
        let mut flipped = false;
        for (edge, adjacent) in edges {
            if adjacent.len() != 2 || constraints.contains(&edge) {
                continue;
            }
            let first_index = adjacent[0].0;
            let second_index = adjacent[1].0;
            let c = adjacent[0].1;
            let d = adjacent[1].1;
            if !convex_quadrilateral(vertices[edge.0], vertices[edge.1], vertices[c], vertices[d]) {
                continue;
            }
            let (a, b) = directed_edge(triangles[first_index], edge).ok_or(())?;
            if in_circle(vertices[a], vertices[b], vertices[c], vertices[d]) <= 1.0e-10 {
                continue;
            }
            let replacement = canonical_edge(c, d);
            if constraints.iter().any(|constraint| {
                proper_intersection_parameter(
                    vertices[replacement.0],
                    vertices[replacement.1],
                    vertices[constraint.0],
                    vertices[constraint.1],
                )
                .is_some()
            }) {
                continue;
            }
            triangles[first_index] = make_ccw([c, d, b], vertices).ok_or(())?;
            triangles[second_index] = make_ccw([d, c, a], vertices).ok_or(())?;
            flipped = true;
            break;
        }
        if !flipped {
            return Ok(());
        }
    }
    Err(())
}

fn triangulation_edges(
    triangles: &[[usize; 3]],
) -> std::collections::BTreeMap<(usize, usize), Vec<(usize, usize)>> {
    let mut edges = std::collections::BTreeMap::new();
    for (triangle_index, triangle) in triangles.iter().copied().enumerate() {
        for edge in 0..3 {
            let first = triangle[edge];
            let second = triangle[(edge + 1) % 3];
            let opposite = triangle[(edge + 2) % 3];
            edges
                .entry(canonical_edge(first, second))
                .or_insert_with(Vec::new)
                .push((triangle_index, opposite));
        }
    }
    edges
}

fn triangle_has_edge(triangle: [usize; 3], edge: (usize, usize)) -> bool {
    triangle.contains(&edge.0) && triangle.contains(&edge.1)
}

fn edge_exists(triangles: &[[usize; 3]], edge: (usize, usize)) -> bool {
    triangles
        .iter()
        .copied()
        .any(|triangle| triangle_has_edge(triangle, edge))
}

fn directed_edge(triangle: [usize; 3], edge: (usize, usize)) -> Option<(usize, usize)> {
    (0..3).find_map(|index| {
        let first = triangle[index];
        let second = triangle[(index + 1) % 3];
        (canonical_edge(first, second) == edge).then_some((first, second))
    })
}

fn canonical_edge(first: usize, second: usize) -> (usize, usize) {
    if first < second {
        (first, second)
    } else {
        (second, first)
    }
}

fn make_ccw(triangle: [usize; 3], vertices: &[[f64; 2]]) -> Option<[usize; 3]> {
    let area = cross2(
        vertices[triangle[0]],
        vertices[triangle[1]],
        vertices[triangle[2]],
    );
    if !area.is_finite() || area.abs() <= 1.0e-12 {
        return None;
    }
    Some(if area > 0.0 {
        triangle
    } else {
        [triangle[0], triangle[2], triangle[1]]
    })
}

fn point_in_triangle(point: [f64; 2], a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> bool {
    let edges = [(a, b), (b, c), (c, a)];
    edges.into_iter().all(|(first, second)| {
        let scale = distance_squared(first, second).sqrt();
        cross2(first, second, point) >= -linear_tolerance(first, second, point) * scale
    })
}

fn point_on_open_segment(point: [f64; 2], first: [f64; 2], second: [f64; 2]) -> bool {
    if !point_on_segment(point, first, second) {
        return false;
    }
    let t = segment_parameter(point, first, second);
    t > 1.0e-9 && t < 1.0 - 1.0e-9
}

fn point_on_segment(point: [f64; 2], first: [f64; 2], second: [f64; 2]) -> bool {
    let length_squared = distance_squared(first, second);
    if length_squared <= 1.0e-20 {
        return distance_squared(point, first) <= 1.0e-18;
    }
    let cross = cross2(first, second, point).abs();
    let dot = (point[0] - first[0]) * (second[0] - first[0])
        + (point[1] - first[1]) * (second[1] - first[1]);
    let tolerance = linear_tolerance(first, second, point);
    let length = length_squared.sqrt();
    cross <= tolerance * length
        && dot >= -tolerance * length
        && dot <= length_squared + tolerance * length
}

fn linear_tolerance(first: [f64; 2], second: [f64; 2], point: [f64; 2]) -> f64 {
    let coordinate_scale = first
        .into_iter()
        .chain(second)
        .chain(point)
        .map(f64::abs)
        .fold(1.0_f64, f64::max);
    2.0 * f64::from(f32::EPSILON) * coordinate_scale
}

fn segment_parameter(point: [f64; 2], first: [f64; 2], second: [f64; 2]) -> f64 {
    let delta = [second[0] - first[0], second[1] - first[1]];
    let length_squared = delta[0] * delta[0] + delta[1] * delta[1];
    if length_squared <= 1.0e-20 {
        0.0
    } else {
        ((point[0] - first[0]) * delta[0] + (point[1] - first[1]) * delta[1]) / length_squared
    }
}

fn proper_intersection_parameter(
    first: [f64; 2],
    second: [f64; 2],
    edge_first: [f64; 2],
    edge_second: [f64; 2],
) -> Option<f64> {
    let first_delta = [second[0] - first[0], second[1] - first[1]];
    let edge_delta = [
        edge_second[0] - edge_first[0],
        edge_second[1] - edge_first[1],
    ];
    let denominator = first_delta[0] * edge_delta[1] - first_delta[1] * edge_delta[0];
    if denominator.abs() <= 1.0e-12 {
        return None;
    }
    let offset = [edge_first[0] - first[0], edge_first[1] - first[1]];
    let t = (offset[0] * edge_delta[1] - offset[1] * edge_delta[0]) / denominator;
    let u = (offset[0] * first_delta[1] - offset[1] * first_delta[0]) / denominator;
    (t > 1.0e-9 && t < 1.0 - 1.0e-9 && u > 1.0e-9 && u < 1.0 - 1.0e-9).then_some(t)
}

fn convex_quadrilateral(a: [f64; 2], b: [f64; 2], c: [f64; 2], d: [f64; 2]) -> bool {
    cross2(a, b, c) * cross2(a, b, d) < -1.0e-12 && cross2(c, d, a) * cross2(c, d, b) < -1.0e-12
}

fn in_circle(a: [f64; 2], b: [f64; 2], c: [f64; 2], d: [f64; 2]) -> f64 {
    let a = [a[0] - d[0], a[1] - d[1]];
    let b = [b[0] - d[0], b[1] - d[1]];
    let c = [c[0] - d[0], c[1] - d[1]];
    let a2 = a[0] * a[0] + a[1] * a[1];
    let b2 = b[0] * b[0] + b[1] * b[1];
    let c2 = c[0] * c[0] + c[1] * c[1];
    a2 * (b[0] * c[1] - b[1] * c[0]) - b2 * (a[0] * c[1] - a[1] * c[0])
        + c2 * (a[0] * b[1] - a[1] * b[0])
}

fn segments_intersect(a: [f64; 2], b: [f64; 2], c: [f64; 2], d: [f64; 2]) -> bool {
    let ab_c = cross2(a, b, c);
    let ab_d = cross2(a, b, d);
    let cd_a = cross2(c, d, a);
    let cd_b = cross2(c, d, b);
    let eps = 1.0e-8;
    (ab_c.abs() <= eps && point_on_segment(c, a, b))
        || (ab_d.abs() <= eps && point_on_segment(d, a, b))
        || (cd_a.abs() <= eps && point_on_segment(a, c, d))
        || (cd_b.abs() <= eps && point_on_segment(b, c, d))
        || (ab_c * ab_d < 0.0 && cd_a * cd_b < 0.0)
}

fn point_in_ring(point: [f64; 2], ring: &[[f64; 2]]) -> bool {
    let mut winding = 0_i32;
    for index in 0..ring.len() {
        let first = ring[index];
        let second = ring[(index + 1) % ring.len()];
        let side = cross2(first, second, point);
        if first[1] <= point[1] {
            if second[1] > point[1] && side > 0.0 {
                winding += 1;
            }
        } else if second[1] <= point[1] && side < 0.0 {
            winding -= 1;
        }
    }
    winding != 0
}

fn polygon_area(points: &[[f64; 2]]) -> f64 {
    (0..points.len())
        .map(|index| {
            let first = points[index];
            let second = points[(index + 1) % points.len()];
            first[0] * second[1] - second[0] * first[1]
        })
        .sum::<f64>()
        * 0.5
}

fn lift_to_source_plane(point: [f64; 2], source: [[f32; 3]; 3]) -> Option<[f32; 3]> {
    for vertex in source {
        if distance_squared(point, [f64::from(vertex[0]), f64::from(vertex[1])]) <= 1.0e-18 {
            return Some(vertex);
        }
    }
    let a = source[0].map(f64::from);
    let b = source[1].map(f64::from);
    let c = source[2].map(f64::from);
    let denominator = cross2([a[0], a[1]], [b[0], b[1]], [c[0], c[1]]);
    if denominator.abs() <= 1.0e-10 {
        return None;
    }
    let second_weight = cross2([a[0], a[1]], point, [c[0], c[1]]) / denominator;
    let third_weight = cross2([a[0], a[1]], [b[0], b[1]], point) / denominator;
    let first_weight = 1.0 - second_weight - third_weight;
    let z = first_weight * a[2] + second_weight * b[2] + third_weight * c[2];
    let position = [point[0] as f32, point[1] as f32, z as f32];
    position
        .iter()
        .all(|value| value.is_finite())
        .then_some(position)
}

fn cross2(first: [f64; 2], second: [f64; 2], third: [f64; 2]) -> f64 {
    (second[0] - first[0]) * (third[1] - first[1]) - (second[1] - first[1]) * (third[0] - first[0])
}

fn interpolate(first: [f64; 2], second: [f64; 2], amount: f64) -> [f64; 2] {
    [
        first[0] + (second[0] - first[0]) * amount,
        first[1] + (second[1] - first[1]) * amount,
    ]
}

fn distance_squared(first: [f64; 2], second: [f64; 2]) -> f64 {
    let delta_x = first[0] - second[0];
    let delta_y = first[1] - second[1];
    delta_x * delta_x + delta_y * delta_y
}

#[cfg(test)]
mod tests {
    use super::subtract_contour;

    #[test]
    fn translated_thin_outside_strip_keeps_its_area() {
        let base = 10_000.0_f32;
        let source = [[
            [base, base, 0.0],
            [base + 10.0, base, 0.0],
            [base, base + 10.0, 0.0],
        ]];
        let outer = [
            [base + 0.003, base + 0.003],
            [base + 1.0, base + 0.003],
            [base + 1.0, base + 0.997],
            [base + 0.003, base + 0.997],
        ];
        let output = subtract_contour(&source, &outer).expect("simple contour");
        let retained_area = output
            .iter()
            .map(|fragment| {
                let [a, b, c] = fragment.positions;
                let ab = [f64::from(b[0] - a[0]), f64::from(b[1] - a[1])];
                let ac = [f64::from(c[0] - a[0]), f64::from(c[1] - a[1])];
                (ab[0] * ac[1] - ab[1] * ac[0]).abs() * 0.5
            })
            .sum::<f64>();
        let expected_area =
            50.0 - (f64::from(outer[1][0] - outer[0][0]) * f64::from(outer[2][1] - outer[1][1]));
        assert!(
            (retained_area - expected_area).abs() < 0.001,
            "retained area {retained_area}, expected {expected_area}"
        );
    }
}
