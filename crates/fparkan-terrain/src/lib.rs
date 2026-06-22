#![forbid(unsafe_code)]
//! Validated terrain runtime queries.

use fparkan_terrain_format::{FullSurfaceMask, LandMapDocument, LandMeshDocument};
use std::collections::VecDeque;

/// Terrain world.
#[derive(Clone, Debug, Default)]
pub struct TerrainWorld {
    areals: Vec<RuntimeAreal>,
    grid: RuntimeGrid,
    adjacency: Vec<Vec<ArealId>>,
    surfaces: Vec<RuntimeTriangle>,
}

/// Surface hit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfaceHit {
    /// Height.
    pub height: f32,
    /// Hit position.
    pub position: [f32; 3],
    /// Ray distance parameter.
    pub distance: f32,
    /// Source face index.
    pub face: usize,
}

/// Areal id.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ArealId(pub u32);

/// Route request.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RouteRequest {
    /// Start.
    pub start: [f32; 3],
    /// Goal.
    pub goal: [f32; 3],
}

/// Areal route.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArealRoute {
    /// Areas.
    pub areas: Vec<ArealId>,
}

/// Terrain error.
#[derive(Debug)]
pub enum TerrainError {
    /// Query is not supported by current data.
    Unsupported,
    /// Area count exceeds runtime id range.
    TooManyAreals {
        /// Area count.
        count: usize,
    },
    /// Grid references an out-of-range area.
    InvalidGridReference {
        /// Referenced area.
        area: u32,
        /// Area count.
        area_count: usize,
    },
    /// Areal graph references an out-of-range area.
    InvalidArealReference {
        /// Source area.
        source: usize,
        /// Referenced area.
        target: u32,
        /// Area count.
        area_count: usize,
    },
    /// Terrain face references an out-of-range vertex.
    InvalidSurfaceVertex {
        /// Source face.
        face: usize,
        /// Referenced vertex.
        vertex: u16,
        /// Position count.
        position_count: usize,
    },
}

impl std::fmt::Display for TerrainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported => write!(f, "terrain query unsupported by current data"),
            Self::TooManyAreals { count } => write!(f, "too many areals: {count}"),
            Self::InvalidGridReference { area, area_count } => {
                write!(f, "grid references area {area} outside {area_count} areas")
            }
            Self::InvalidArealReference {
                source,
                target,
                area_count,
            } => write!(
                f,
                "area {source} references area {target} outside {area_count} areas"
            ),
            Self::InvalidSurfaceVertex {
                face,
                vertex,
                position_count,
            } => write!(
                f,
                "terrain face {face} references vertex {vertex} outside {position_count} positions"
            ),
        }
    }
}

impl std::error::Error for TerrainError {}

/// Surface query.
pub trait SurfaceQuery {
    /// Height at position.
    ///
    /// # Errors
    ///
    /// Returns [`TerrainError`] when the current world lacks surface geometry.
    fn height_at(&self, position: [f32; 2]) -> Result<Option<f32>, TerrainError>;

    /// Raycast.
    ///
    /// # Errors
    ///
    /// Returns [`TerrainError`] when the current world lacks surface geometry.
    fn raycast(
        &self,
        origin: [f32; 3],
        direction: [f32; 3],
        mask: FullSurfaceMask,
    ) -> Result<Option<SurfaceHit>, TerrainError>;
}

/// Navigation query.
pub trait NavigationQuery {
    /// Locate areal.
    ///
    /// # Errors
    ///
    /// Returns [`TerrainError`] when runtime indexes are invalid.
    fn locate_areal(&self, position: [f32; 3]) -> Result<Option<ArealId>, TerrainError>;

    /// Route.
    ///
    /// # Errors
    ///
    /// Returns [`TerrainError`] when runtime indexes are invalid.
    fn route(&self, request: RouteRequest) -> Result<Option<ArealRoute>, TerrainError>;
}

impl TerrainWorld {
    /// Builds navigation runtime data from a decoded `Land.map`.
    ///
    /// # Errors
    ///
    /// Returns [`TerrainError`] if ids or references cannot be represented by
    /// runtime indexes.
    pub fn from_land_map(map: &LandMapDocument) -> Result<Self, TerrainError> {
        let areal_count = map.areals.len();
        if u32::try_from(areal_count).is_err() {
            return Err(TerrainError::TooManyAreals { count: areal_count });
        }
        let mut areals = Vec::with_capacity(areal_count);
        for (index, areal) in map.areals.iter().enumerate() {
            let id = ArealId(
                u32::try_from(index)
                    .map_err(|_| TerrainError::TooManyAreals { count: areal_count })?,
            );
            areals.push(RuntimeAreal {
                id,
                polygon: areal
                    .vertices
                    .iter()
                    .map(|vertex| [vertex[0], vertex[2]])
                    .collect(),
            });
        }

        let mut adjacency = vec![Vec::new(); areal_count];
        for (source_index, areal) in map.areals.iter().enumerate() {
            for link in &areal.links {
                let Some(target) = link.area_ref else {
                    continue;
                };
                let target_index =
                    usize::try_from(target).map_err(|_| TerrainError::InvalidArealReference {
                        source: source_index,
                        target,
                        area_count: areal_count,
                    })?;
                if target_index >= areal_count {
                    return Err(TerrainError::InvalidArealReference {
                        source: source_index,
                        target,
                        area_count: areal_count,
                    });
                }
                let id = ArealId(target);
                if !adjacency[source_index].contains(&id) {
                    adjacency[source_index].push(id);
                }
            }
            adjacency[source_index].sort_by_key(|id| id.0);
        }

        let grid = RuntimeGrid::from_land_map(map)?;
        Ok(Self {
            areals,
            grid,
            adjacency,
            surfaces: Vec::new(),
        })
    }

    /// Builds surface runtime data from a decoded `Land.msh`.
    ///
    /// # Errors
    ///
    /// Returns [`TerrainError`] if a face cannot be represented by runtime
    /// indexes.
    pub fn from_land_msh(mesh: &LandMeshDocument) -> Result<Self, TerrainError> {
        Ok(Self {
            surfaces: build_surfaces(mesh)?,
            ..Self::default()
        })
    }

    /// Builds terrain runtime data from decoded `Land.msh` and `Land.map`.
    ///
    /// # Errors
    ///
    /// Returns [`TerrainError`] if surface or navigation runtime indexes are
    /// invalid.
    pub fn from_land_assets(
        mesh: &LandMeshDocument,
        map: &LandMapDocument,
    ) -> Result<Self, TerrainError> {
        let mut world = Self::from_land_map(map)?;
        world.surfaces = build_surfaces(mesh)?;
        Ok(world)
    }

    /// Returns the number of navigation areas.
    #[must_use]
    pub fn areal_count(&self) -> usize {
        self.areals.len()
    }

    /// Returns the number of surface triangles.
    #[must_use]
    pub fn surface_count(&self) -> usize {
        self.surfaces.len()
    }

    fn locate_by_candidates(
        &self,
        position: [f32; 3],
        candidates: &[ArealId],
    ) -> Result<Option<ArealId>, TerrainError> {
        let point = [position[0], position[2]];
        for candidate in candidates {
            let Some(areal) = usize::try_from(candidate.0)
                .ok()
                .and_then(|index| self.areals.get(index))
            else {
                return Err(TerrainError::InvalidGridReference {
                    area: candidate.0,
                    area_count: self.areals.len(),
                });
            };
            if areal.contains(point) {
                return Ok(Some(areal.id));
            }
        }
        Ok(None)
    }

    fn route_ids(&self, start: ArealId, goal: ArealId) -> Result<Option<ArealRoute>, TerrainError> {
        let start_index =
            usize::try_from(start.0).map_err(|_| TerrainError::InvalidArealReference {
                source: 0,
                target: start.0,
                area_count: self.areals.len(),
            })?;
        let goal_index =
            usize::try_from(goal.0).map_err(|_| TerrainError::InvalidArealReference {
                source: start_index,
                target: goal.0,
                area_count: self.areals.len(),
            })?;
        if start_index >= self.areals.len() {
            return Err(TerrainError::InvalidArealReference {
                source: start_index,
                target: start.0,
                area_count: self.areals.len(),
            });
        }
        if goal_index >= self.areals.len() {
            return Err(TerrainError::InvalidArealReference {
                source: start_index,
                target: goal.0,
                area_count: self.areals.len(),
            });
        }
        if start == goal {
            return Ok(Some(ArealRoute { areas: vec![start] }));
        }

        let mut previous = vec![None; self.areals.len()];
        let mut visited = vec![false; self.areals.len()];
        let mut queue = VecDeque::new();
        visited[start_index] = true;
        queue.push_back(start_index);

        while let Some(current) = queue.pop_front() {
            for next in &self.adjacency[current] {
                let next_index =
                    usize::try_from(next.0).map_err(|_| TerrainError::InvalidArealReference {
                        source: current,
                        target: next.0,
                        area_count: self.areals.len(),
                    })?;
                if next_index >= self.areals.len() {
                    return Err(TerrainError::InvalidArealReference {
                        source: current,
                        target: next.0,
                        area_count: self.areals.len(),
                    });
                }
                if visited[next_index] {
                    continue;
                }
                visited[next_index] = true;
                previous[next_index] = Some(current);
                if next_index == goal_index {
                    return Ok(Some(reconstruct_route(&previous, start_index, goal_index)));
                }
                queue.push_back(next_index);
            }
        }
        Ok(None)
    }
}

impl SurfaceQuery for TerrainWorld {
    fn height_at(&self, position: [f32; 2]) -> Result<Option<f32>, TerrainError> {
        if self.surfaces.is_empty() {
            return Err(TerrainError::Unsupported);
        }
        let mut best = None;
        for triangle in &self.surfaces {
            if let Some(height) = triangle.height_at(position) {
                best = Some(best.map_or(height, |current: f32| current.max(height)));
            }
        }
        Ok(best)
    }

    fn raycast(
        &self,
        origin: [f32; 3],
        direction: [f32; 3],
        mask: FullSurfaceMask,
    ) -> Result<Option<SurfaceHit>, TerrainError> {
        if self.surfaces.is_empty() {
            return Err(TerrainError::Unsupported);
        }
        let mut best: Option<SurfaceHit> = None;
        for triangle in &self.surfaces {
            if mask.0 != 0 && triangle.mask.0 & mask.0 == 0 {
                continue;
            }
            let Some(distance) = triangle.raycast(origin, direction) else {
                continue;
            };
            if best.is_some_and(|hit| hit.distance <= distance) {
                continue;
            }
            let position = [
                origin[0] + direction[0] * distance,
                origin[1] + direction[1] * distance,
                origin[2] + direction[2] * distance,
            ];
            best = Some(SurfaceHit {
                height: position[1],
                position,
                distance,
                face: triangle.face,
            });
        }
        Ok(best)
    }
}

impl NavigationQuery for TerrainWorld {
    fn locate_areal(&self, position: [f32; 3]) -> Result<Option<ArealId>, TerrainError> {
        if let Some(candidates) = self.grid.candidates(position) {
            if let Some(id) = self.locate_by_candidates(position, candidates)? {
                return Ok(Some(id));
            }
        }
        let all: Vec<ArealId> = self.areals.iter().map(|areal| areal.id).collect();
        self.locate_by_candidates(position, &all)
    }

    fn route(&self, request: RouteRequest) -> Result<Option<ArealRoute>, TerrainError> {
        let Some(start) = self.locate_areal(request.start)? else {
            return Ok(None);
        };
        let Some(goal) = self.locate_areal(request.goal)? else {
            return Ok(None);
        };
        self.route_ids(start, goal)
    }
}

#[derive(Clone, Debug)]
struct RuntimeTriangle {
    face: usize,
    mask: FullSurfaceMask,
    vertices: [[f32; 3]; 3],
}

impl RuntimeTriangle {
    fn height_at(&self, position: [f32; 2]) -> Option<f32> {
        let a = [self.vertices[0][0], self.vertices[0][2]];
        let b = [self.vertices[1][0], self.vertices[1][2]];
        let c = [self.vertices[2][0], self.vertices[2][2]];
        let weights = barycentric_2d(position, a, b, c)?;
        if weights
            .iter()
            .all(|weight| *weight >= -1.0e-4 && *weight <= 1.0001)
        {
            Some(
                weights[0] * self.vertices[0][1]
                    + weights[1] * self.vertices[1][1]
                    + weights[2] * self.vertices[2][1],
            )
        } else {
            None
        }
    }

    fn raycast(&self, origin: [f32; 3], direction: [f32; 3]) -> Option<f32> {
        let edge1 = sub3(self.vertices[1], self.vertices[0]);
        let edge2 = sub3(self.vertices[2], self.vertices[0]);
        let pvec = cross3(direction, edge2);
        let det = dot3(edge1, pvec);
        if det.abs() <= 1.0e-6 {
            return None;
        }
        let inv_det = 1.0 / det;
        let tvec = sub3(origin, self.vertices[0]);
        let u = dot3(tvec, pvec) * inv_det;
        if !(-1.0e-5..=1.00001).contains(&u) {
            return None;
        }
        let qvec = cross3(tvec, edge1);
        let v = dot3(direction, qvec) * inv_det;
        if v < -1.0e-5 || u + v > 1.00001 {
            return None;
        }
        let distance = dot3(edge2, qvec) * inv_det;
        (distance >= 0.0).then_some(distance)
    }
}

#[derive(Clone, Debug)]
struct RuntimeAreal {
    id: ArealId,
    polygon: Vec<[f32; 2]>,
}

impl RuntimeAreal {
    fn contains(&self, point: [f32; 2]) -> bool {
        if self.polygon.len() < 3 {
            return false;
        }
        if self.on_boundary(point) {
            return true;
        }

        let mut inside = false;
        let mut prev = self.polygon[self.polygon.len() - 1];
        for current in &self.polygon {
            let crosses = (current[1] > point[1]) != (prev[1] > point[1]);
            if crosses {
                let x_intersect = (prev[0] - current[0]) * (point[1] - current[1])
                    / (prev[1] - current[1])
                    + current[0];
                if point[0] < x_intersect {
                    inside = !inside;
                }
            }
            prev = *current;
        }
        inside
    }

    fn on_boundary(&self, point: [f32; 2]) -> bool {
        let mut prev = self.polygon[self.polygon.len() - 1];
        for current in &self.polygon {
            if point_on_segment(point, prev, *current) {
                return true;
            }
            prev = *current;
        }
        false
    }
}

#[derive(Clone, Debug, Default)]
struct RuntimeGrid {
    cells_x: u32,
    cells_y: u32,
    min: [f32; 2],
    max: [f32; 2],
    cells: Vec<Vec<ArealId>>,
}

impl RuntimeGrid {
    fn from_land_map(map: &LandMapDocument) -> Result<Self, TerrainError> {
        let mut min = [f32::INFINITY, f32::INFINITY];
        let mut max = [f32::NEG_INFINITY, f32::NEG_INFINITY];
        for areal in &map.areals {
            for vertex in &areal.vertices {
                min[0] = min[0].min(vertex[0]);
                min[1] = min[1].min(vertex[2]);
                max[0] = max[0].max(vertex[0]);
                max[1] = max[1].max(vertex[2]);
            }
        }
        if !min[0].is_finite() || !min[1].is_finite() || !max[0].is_finite() || !max[1].is_finite()
        {
            min = [0.0, 0.0];
            max = [1.0, 1.0];
        }
        if (min[0] - max[0]).abs() <= f32::EPSILON {
            max[0] += 1.0;
        }
        if (min[1] - max[1]).abs() <= f32::EPSILON {
            max[1] += 1.0;
        }

        let mut cells = Vec::with_capacity(map.grid.cells.len());
        for cell in &map.grid.cells {
            let mut ids = Vec::with_capacity(cell.area_ids.len());
            for area in &cell.area_ids {
                let index =
                    usize::try_from(*area).map_err(|_| TerrainError::InvalidGridReference {
                        area: *area,
                        area_count: map.areals.len(),
                    })?;
                if index >= map.areals.len() {
                    return Err(TerrainError::InvalidGridReference {
                        area: *area,
                        area_count: map.areals.len(),
                    });
                }
                ids.push(ArealId(*area));
            }
            cells.push(ids);
        }
        Ok(Self {
            cells_x: map.grid.cells_x,
            cells_y: map.grid.cells_y,
            min,
            max,
            cells,
        })
    }

    fn candidates(&self, position: [f32; 3]) -> Option<&[ArealId]> {
        if self.cells_x == 0 || self.cells_y == 0 || self.cells.is_empty() {
            return None;
        }
        let point = [position[0], position[2]];
        if point[0] < self.min[0]
            || point[0] > self.max[0]
            || point[1] < self.min[1]
            || point[1] > self.max[1]
        {
            return None;
        }
        let nx = normalized_cell(point[0], self.min[0], self.max[0], self.cells_x);
        let ny = normalized_cell(point[1], self.min[1], self.max[1], self.cells_y);
        let index_u32 = ny.checked_mul(self.cells_x)?.checked_add(nx)?;
        let index = usize::try_from(index_u32).ok()?;
        self.cells.get(index).map(Vec::as_slice)
    }
}

fn build_surfaces(mesh: &LandMeshDocument) -> Result<Vec<RuntimeTriangle>, TerrainError> {
    let mut triangles = Vec::with_capacity(mesh.faces.len());
    for (face_index, face) in mesh.faces.iter().enumerate() {
        let vertices = [
            surface_vertex(mesh, face_index, face.vertices[0])?,
            surface_vertex(mesh, face_index, face.vertices[1])?,
            surface_vertex(mesh, face_index, face.vertices[2])?,
        ];
        triangles.push(RuntimeTriangle {
            face: face_index,
            mask: face.flags,
            vertices,
        });
    }
    Ok(triangles)
}

fn surface_vertex(
    mesh: &LandMeshDocument,
    face: usize,
    vertex: u16,
) -> Result<[f32; 3], TerrainError> {
    mesh.positions
        .get(usize::from(vertex))
        .copied()
        .ok_or(TerrainError::InvalidSurfaceVertex {
            face,
            vertex,
            position_count: mesh.positions.len(),
        })
}

fn barycentric_2d(
    point: [f32; 2],
    first: [f32; 2],
    second: [f32; 2],
    third: [f32; 2],
) -> Option<[f32; 3]> {
    let edge_second = [second[0] - first[0], second[1] - first[1]];
    let edge_third = [third[0] - first[0], third[1] - first[1]];
    let point_delta = [point[0] - first[0], point[1] - first[1]];
    let denom = edge_second[0] * edge_third[1] - edge_third[0] * edge_second[1];
    if denom.abs() <= 1.0e-6 {
        return None;
    }
    let inv = 1.0 / denom;
    let second_weight = (point_delta[0] * edge_third[1] - edge_third[0] * point_delta[1]) * inv;
    let third_weight = (edge_second[0] * point_delta[1] - point_delta[0] * edge_second[1]) * inv;
    let first_weight = 1.0 - second_weight - third_weight;
    Some([first_weight, second_weight, third_weight])
}

fn sub3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn normalized_cell(value: f32, min: f32, max: f32, cells: u32) -> u32 {
    let span = max - min;
    if span <= 0.0 {
        return 0;
    }
    if value <= min {
        return 0;
    }
    if value >= max {
        return cells.saturating_sub(1);
    }
    let value = f64::from(value);
    let min = f64::from(min);
    let span = f64::from(span);
    for cell in 0..cells {
        let upper = min + span * f64::from(cell + 1) / f64::from(cells);
        if value <= upper {
            return cell;
        }
    }
    cells.saturating_sub(1)
}

fn point_on_segment(point: [f32; 2], a: [f32; 2], b: [f32; 2]) -> bool {
    let cross = (point[1] - a[1]) * (b[0] - a[0]) - (point[0] - a[0]) * (b[1] - a[1]);
    if cross.abs() > 1.0e-4 {
        return false;
    }
    let min_x = a[0].min(b[0]) - 1.0e-4;
    let max_x = a[0].max(b[0]) + 1.0e-4;
    let min_y = a[1].min(b[1]) - 1.0e-4;
    let max_y = a[1].max(b[1]) + 1.0e-4;
    point[0] >= min_x && point[0] <= max_x && point[1] >= min_y && point[1] <= max_y
}

fn reconstruct_route(previous: &[Option<usize>], start: usize, goal: usize) -> ArealRoute {
    let mut route = Vec::new();
    let mut current = goal;
    route.push(ArealId(u32::try_from(current).unwrap_or(u32::MAX)));
    while current != start {
        let Some(prev) = previous[current] else {
            break;
        };
        current = prev;
        route.push(ArealId(u32::try_from(current).unwrap_or(u32::MAX)));
    }
    route.reverse();
    ArealRoute { areas: route }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_nres::ReadProfile;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    #[test]
    fn locates_areal_and_routes_synthetic_neighbors() {
        let map = synthetic_land_map();
        let world = TerrainWorld::from_land_map(&map).expect("world");

        assert_eq!(world.areal_count(), 2);
        assert_eq!(
            world.locate_areal([0.25, 0.0, 0.25]).expect("locate"),
            Some(ArealId(0))
        );
        assert_eq!(
            world.locate_areal([1.75, 0.0, 0.25]).expect("locate"),
            Some(ArealId(1))
        );
        assert_eq!(
            world
                .route(RouteRequest {
                    start: [0.25, 0.0, 0.25],
                    goal: [1.75, 0.0, 0.25],
                })
                .expect("route"),
            Some(ArealRoute {
                areas: vec![ArealId(0), ArealId(1)]
            })
        );
    }

    #[test]
    fn missing_start_or_goal_returns_no_route() {
        let world = TerrainWorld::from_land_map(&synthetic_land_map()).expect("world");

        assert_eq!(
            world
                .route(RouteRequest {
                    start: [10.0, 0.0, 10.0],
                    goal: [1.75, 0.0, 0.25],
                })
                .expect("route"),
            None
        );
    }

    #[test]
    fn synthetic_surface_height_and_raycast_work() {
        let world = TerrainWorld::from_land_msh(&synthetic_land_mesh()).expect("world");

        assert_eq!(world.surface_count(), 2);
        assert_eq!(world.height_at([0.25, 0.25]).expect("height"), Some(0.5));
        assert_eq!(world.height_at([10.0, 10.0]).expect("height"), None);

        let hit = world
            .raycast(
                [0.25, 2.0, 0.25],
                [0.0, -1.0, 0.0],
                FullSurfaceMask(0x0000_0001),
            )
            .expect("raycast")
            .expect("hit");
        assert_eq!(hit.face, 0);
        assert!((hit.height - 0.5).abs() < 1.0e-5);
        assert!((hit.distance - 1.5).abs() < 1.0e-5);

        assert_eq!(
            world
                .raycast(
                    [0.25, 2.0, 0.25],
                    [0.0, -1.0, 0.0],
                    FullSurfaceMask(0x8000_0000)
                )
                .expect("raycast"),
            None
        );
    }

    #[test]
    fn licensed_corpus_land_maps_build_navigation_worlds() {
        for (corpus, expected_files, expected_areals) in [
            ("IS", 33_usize, 34_662_usize),
            ("IS2", 32_usize, 18_984_usize),
        ] {
            let Some(root) = corpus_root(corpus) else {
                continue;
            };
            let mut files = 0usize;
            let mut areals = 0usize;
            let mut located_centers = 0usize;
            for path in files_under(&root) {
                if !path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.eq_ignore_ascii_case("Land.map"))
                {
                    continue;
                }
                let bytes = std::fs::read(&path).expect("read Land.map");
                let nres = fparkan_nres::decode(
                    Arc::from(bytes.into_boxed_slice()),
                    ReadProfile::Compatible,
                )
                .unwrap_or_else(|err| panic!("{corpus} {path:?}: {err}"));
                let map = fparkan_terrain_format::decode_land_map(&nres)
                    .unwrap_or_else(|err| panic!("{corpus} {path:?}: {err}"));
                let world = TerrainWorld::from_land_map(&map)
                    .unwrap_or_else(|err| panic!("{corpus} {path:?}: {err}"));
                files += 1;
                areals += world.areal_count();
                for (index, areal) in map.areals.iter().take(8).enumerate() {
                    if let Some(point) = polygon_probe_point(&areal.vertices) {
                        let located = world
                            .locate_areal([point[0], point[1], point[2]])
                            .unwrap_or_else(|err| panic!("{corpus} {path:?}: {err}"));
                        assert!(
                            located.is_some(),
                            "{corpus} {path:?} area {index} probe point was not located"
                        );
                        located_centers += 1;
                    }
                }
            }

            assert_eq!(files, expected_files, "{corpus} Land.map count");
            assert_eq!(areals, expected_areals, "{corpus} areal count");
            assert!(
                located_centers >= expected_files,
                "{corpus} located center coverage"
            );
        }
    }

    #[test]
    fn licensed_corpus_land_meshes_build_surface_worlds() {
        for (corpus, expected_files, expected_faces) in [
            ("IS", 33_usize, 275_882_usize),
            ("IS2", 32_usize, 184_454_usize),
        ] {
            let Some(root) = corpus_root(corpus) else {
                continue;
            };
            let mut files = 0usize;
            let mut faces = 0usize;
            for path in files_under(&root) {
                if !path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.eq_ignore_ascii_case("Land.msh"))
                {
                    continue;
                }
                let bytes = std::fs::read(&path).expect("read Land.msh");
                let nres = fparkan_nres::decode(
                    Arc::from(bytes.into_boxed_slice()),
                    ReadProfile::Compatible,
                )
                .unwrap_or_else(|err| panic!("{corpus} {path:?}: {err}"));
                let mesh = fparkan_terrain_format::decode_land_msh(&nres)
                    .unwrap_or_else(|err| panic!("{corpus} {path:?}: {err}"));
                let world = TerrainWorld::from_land_msh(&mesh)
                    .unwrap_or_else(|err| panic!("{corpus} {path:?}: {err}"));
                files += 1;
                faces += world.surface_count();
            }

            assert_eq!(files, expected_files, "{corpus} Land.msh count");
            assert_eq!(faces, expected_faces, "{corpus} surface face count");
        }
    }

    fn synthetic_land_mesh() -> LandMeshDocument {
        use fparkan_terrain_format::{TerrainSlotTable, TerrainStream};

        let face0 = terrain_face(FullSurfaceMask(0x0000_0001), [0, 1, 2]);
        let face1 = terrain_face(FullSurfaceMask(0x0000_0002), [1, 3, 2]);
        LandMeshDocument {
            streams: Vec::<TerrainStream>::new(),
            nodes_raw: Vec::new(),
            slots: TerrainSlotTable {
                header_raw: Vec::new(),
                slots_raw: Vec::new(),
            },
            positions: vec![
                [0.0, 0.0, 0.0],
                [1.0, 1.0, 0.0],
                [0.0, 1.0, 1.0],
                [1.0, 2.0, 1.0],
            ],
            normals: Vec::new(),
            uv0: Vec::new(),
            accelerator: Vec::new(),
            aux14: Vec::new(),
            aux18: Vec::new(),
            faces: vec![face0, face1],
        }
    }

    fn terrain_face(
        flags: FullSurfaceMask,
        vertices: [u16; 3],
    ) -> fparkan_terrain_format::TerrainFace28 {
        use fparkan_terrain_format::TerrainFace28;

        TerrainFace28 {
            flags,
            material_tag: 0,
            aux_tag: 0,
            vertices,
            neighbors: [None, None, None],
            tail_raw: [0; 8],
            raw: [0; 28],
        }
    }

    fn synthetic_land_map() -> LandMapDocument {
        use fparkan_terrain_format::{
            Areal, ArealGrid, ArealGridCell, EdgeLink, TerrainStream, TerrainStreamAttributes,
        };

        LandMapDocument {
            entry: TerrainStream {
                type_id: 12,
                attributes: TerrainStreamAttributes::default(),
                size: 0,
            },
            areal_count: 2,
            areals: vec![
                Areal {
                    prefix_raw: [0; 56],
                    anchor: [0.5, 0.0, 0.5],
                    reserved_12: 0.0,
                    area_metric: 1.0,
                    normal: [0.0, 1.0, 0.0],
                    logic_flag: 0,
                    reserved_36: 0,
                    class_id: 0,
                    reserved_44: 0,
                    vertices: vec![
                        [0.0, 0.0, 0.0],
                        [1.0, 0.0, 0.0],
                        [1.0, 0.0, 1.0],
                        [0.0, 0.0, 1.0],
                    ],
                    links: vec![
                        EdgeLink {
                            raw_area_ref: -1,
                            raw_edge_ref: -1,
                            area_ref: None,
                            edge_ref: None,
                        },
                        EdgeLink {
                            raw_area_ref: 1,
                            raw_edge_ref: 3,
                            area_ref: Some(1),
                            edge_ref: Some(3),
                        },
                        EdgeLink {
                            raw_area_ref: -1,
                            raw_edge_ref: -1,
                            area_ref: None,
                            edge_ref: None,
                        },
                        EdgeLink {
                            raw_area_ref: -1,
                            raw_edge_ref: -1,
                            area_ref: None,
                            edge_ref: None,
                        },
                    ],
                    polygon_blocks: Vec::new(),
                },
                Areal {
                    prefix_raw: [0; 56],
                    anchor: [1.5, 0.0, 0.5],
                    reserved_12: 0.0,
                    area_metric: 1.0,
                    normal: [0.0, 1.0, 0.0],
                    logic_flag: 0,
                    reserved_36: 0,
                    class_id: 0,
                    reserved_44: 0,
                    vertices: vec![
                        [1.0, 0.0, 0.0],
                        [2.0, 0.0, 0.0],
                        [2.0, 0.0, 1.0],
                        [1.0, 0.0, 1.0],
                    ],
                    links: vec![
                        EdgeLink {
                            raw_area_ref: -1,
                            raw_edge_ref: -1,
                            area_ref: None,
                            edge_ref: None,
                        },
                        EdgeLink {
                            raw_area_ref: -1,
                            raw_edge_ref: -1,
                            area_ref: None,
                            edge_ref: None,
                        },
                        EdgeLink {
                            raw_area_ref: -1,
                            raw_edge_ref: -1,
                            area_ref: None,
                            edge_ref: None,
                        },
                        EdgeLink {
                            raw_area_ref: 0,
                            raw_edge_ref: 1,
                            area_ref: Some(0),
                            edge_ref: Some(1),
                        },
                    ],
                    polygon_blocks: Vec::new(),
                },
            ],
            grid: ArealGrid {
                cells_x: 2,
                cells_y: 1,
                cells: vec![
                    ArealGridCell { area_ids: vec![0] },
                    ArealGridCell { area_ids: vec![1] },
                ],
                candidate_pool: vec![0, 1],
                compact_cells: vec![0x0040_0000, 0x0040_0001],
            },
        }
    }

    fn polygon_probe_point(vertices: &[[f32; 3]]) -> Option<[f32; 3]> {
        vertices.first().copied()
    }

    fn corpus_root(name: &str) -> Option<PathBuf> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("testdata")
            .join(name);
        root.is_dir().then_some(root)
    }

    fn files_under(root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(path) = stack.pop() {
            let Ok(read_dir) = std::fs::read_dir(path) else {
                continue;
            };
            for entry in read_dir.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    out.push(path);
                }
            }
        }
        out.sort();
        out
    }
}
