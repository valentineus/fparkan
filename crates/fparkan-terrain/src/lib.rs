#![forbid(unsafe_code)]
//! Validated terrain runtime queries using XY ground coordinates and Z height.

use fparkan_terrain_format::{LandMapDocument, LandMeshDocument};
use std::collections::VecDeque;

mod contour;
mod foundation;

use foundation::basement_surface_mask;
pub use foundation::{
    build_basement_foundations, BasementFootprint, BasementFoundation, BasementFoundationError,
    BasementFoundationFragment, BasementPlacement,
};

/// Terrain material-selector contract preserved from the decoded map mesh.
///
/// Applications access this semantic terrain API through this terrain crate
/// rather than taking a direct dependency on the binary-format parser.
pub use fparkan_terrain_format::{
    FullSurfaceMask, TerrainLod, TerrainLodError, TerrainMaterialLayers, TerrainMaterialSelection,
};

/// Terrain world.
#[derive(Clone, Debug, Default)]
pub struct TerrainWorld {
    areals: Vec<RuntimeAreal>,
    grid: RuntimeGrid,
    adjacency: Vec<Vec<ArealId>>,
    surfaces: Vec<RuntimeTriangle>,
    surface_bvh: SurfaceBvh,
    source_mesh: Option<LandMeshDocument>,
    basement_foundation: Option<BasementFoundation>,
}

/// Surface hit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfaceHit {
    /// Source Z height.
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
    /// A basement fragment does not preserve a valid LOD0 source face and mask.
    InvalidFoundationFragment {
        /// Fragment index.
        fragment: usize,
    },
    /// Foundation fragments require a retained source Land.msh document.
    FoundationRequiresMesh,
    /// Terrain Node38/Slot68 records do not assign every face to one LOD.
    InvalidLodMapping(TerrainLodError),
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
            Self::InvalidFoundationFragment { fragment } => {
                write!(f, "invalid basement foundation fragment {fragment}")
            }
            Self::FoundationRequiresMesh => {
                write!(f, "basement foundation requires a source Land.msh")
            }
            Self::InvalidLodMapping(error) => write!(f, "invalid terrain LOD mapping: {error}"),
        }
    }
}

impl std::error::Error for TerrainError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidLodMapping(error) => Some(error),
            Self::Unsupported
            | Self::TooManyAreals { .. }
            | Self::InvalidGridReference { .. }
            | Self::InvalidArealReference { .. }
            | Self::InvalidSurfaceVertex { .. }
            | Self::InvalidFoundationFragment { .. }
            | Self::FoundationRequiresMesh => None,
        }
    }
}

/// Surface query.
pub trait SurfaceQuery {
    /// Source Z height at an XY ground position.
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

    /// Raycast while requiring every bit in `include` and rejecting every
    /// face carrying a bit in `exclude`.
    ///
    /// The native landscape query has both predicates.  The legacy
    /// [`Self::raycast`] shorthand keeps its historical include-only form;
    /// callers that model a native receiver or visibility test should use
    /// this method so exclusion flags cannot be mistaken for inclusion bits.
    fn raycast_excluding(
        &self,
        origin: [f32; 3],
        direction: [f32; 3],
        include: FullSurfaceMask,
        exclude: FullSurfaceMask,
    ) -> Result<Option<SurfaceHit>, TerrainError>;
}

/// Navigation query.
pub trait NavigationQuery {
    /// Locates an areal from the XY components of a world position.
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
                    .map(|vertex| [vertex[0], vertex[1]])
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
            surface_bvh: SurfaceBvh::default(),
            source_mesh: None,
            basement_foundation: None,
        })
    }

    /// Builds surface runtime data from a decoded `Land.msh`.
    ///
    /// # Errors
    ///
    /// Returns [`TerrainError`] if terrain LOD metadata or a face cannot be
    /// represented by runtime indexes.
    pub fn from_land_msh(mesh: &LandMeshDocument) -> Result<Self, TerrainError> {
        Ok(Self {
            surfaces: build_surfaces(mesh)?,
            surface_bvh: SurfaceBvh::default(),
            source_mesh: Some(mesh.clone()),
            ..Self::default()
        }
        .with_surface_bvh())
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
        world.surface_bvh = SurfaceBvh::from_surfaces(&world.surfaces);
        world.source_mesh = Some(mesh.clone());
        Ok(world)
    }

    /// Returns a new world whose surface queries use exactly the canonical
    /// source and generated triangles from a basement foundation.
    ///
    /// # Errors
    ///
    /// Returns [`TerrainError`] if this world has no source mesh or any
    /// fragment does not preserve valid LOD0 identity, flags, and geometry.
    pub fn with_basement_foundation(
        mut self,
        foundation: BasementFoundation,
    ) -> Result<Self, TerrainError> {
        let mesh = self
            .source_mesh
            .as_ref()
            .ok_or(TerrainError::FoundationRequiresMesh)?;
        let assignments = mesh
            .face_lod_assignments()
            .map_err(TerrainError::InvalidLodMapping)?;
        let is_lod0 = assignments
            .iter()
            .map(|assignment| assignment.lod == TerrainLod::Lod0)
            .collect::<Vec<_>>();
        let mut surfaces = Vec::with_capacity(foundation.fragments.len());
        let mut generated = 0_usize;
        for (fragment_index, fragment) in foundation.fragments.iter().enumerate() {
            if !valid_foundation_triangle(fragment.positions, fragment.source_face.is_none()) {
                return Err(TerrainError::InvalidFoundationFragment {
                    fragment: fragment_index,
                });
            }
            let face = if let Some(source_face) = fragment.source_face {
                let Some(source) = mesh.faces.get(source_face) else {
                    return Err(TerrainError::InvalidFoundationFragment {
                        fragment: fragment_index,
                    });
                };
                if !is_lod0.get(source_face).copied().unwrap_or(false)
                    || source.flags != fragment.surface_mask
                {
                    return Err(TerrainError::InvalidFoundationFragment {
                        fragment: fragment_index,
                    });
                }
                source_face
            } else {
                if basement_surface_mask(fragment.positions) != fragment.surface_mask {
                    return Err(TerrainError::InvalidFoundationFragment {
                        fragment: fragment_index,
                    });
                }
                let face = mesh.faces.len().checked_add(generated).ok_or(
                    TerrainError::InvalidFoundationFragment {
                        fragment: fragment_index,
                    },
                )?;
                generated =
                    generated
                        .checked_add(1)
                        .ok_or(TerrainError::InvalidFoundationFragment {
                            fragment: fragment_index,
                        })?;
                face
            };
            surfaces.push(RuntimeTriangle {
                face,
                mask: fragment.surface_mask,
                vertices: fragment.positions,
            });
        }
        self.surfaces = surfaces;
        self.surface_bvh = SurfaceBvh::from_surfaces(&self.surfaces);
        self.basement_foundation = Some(foundation);
        Ok(self)
    }

    /// Returns the canonical basement geometry used by the surface query index.
    #[must_use]
    pub fn basement_foundation(&self) -> Option<&BasementFoundation> {
        self.basement_foundation.as_ref()
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

    /// Returns the validated source mesh retained for renderer integration.
    ///
    /// Runtime collision queries use their own compact triangle representation;
    /// keeping this document preserves the original geometry and UV streams for
    /// rendering without asking the application layer to decode an archive again.
    #[must_use]
    pub fn source_mesh(&self) -> Option<&LandMeshDocument> {
        self.source_mesh.as_ref()
    }

    /// Returns source terrain positions without exposing parser ownership to apps.
    #[must_use]
    pub fn source_positions(&self) -> Option<&[[f32; 3]]> {
        self.source_mesh
            .as_ref()
            .map(|mesh| mesh.positions.as_slice())
    }

    fn with_surface_bvh(mut self) -> Self {
        self.surface_bvh = SurfaceBvh::from_surfaces(&self.surfaces);
        self
    }

    fn locate_by_candidates(
        &self,
        position: [f32; 3],
        candidates: &[ArealId],
    ) -> Result<Option<ArealId>, TerrainError> {
        let point = [position[0], position[1]];
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
        for index in self.surface_bvh.xy_candidates(position) {
            let triangle = &self.surfaces[index];
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
        self.raycast_filtered(origin, direction, mask, FullSurfaceMask(0))
    }

    fn raycast_excluding(
        &self,
        origin: [f32; 3],
        direction: [f32; 3],
        include: FullSurfaceMask,
        exclude: FullSurfaceMask,
    ) -> Result<Option<SurfaceHit>, TerrainError> {
        self.raycast_filtered(origin, direction, include, exclude)
    }
}

impl TerrainWorld {
    fn raycast_filtered(
        &self,
        origin: [f32; 3],
        direction: [f32; 3],
        include: FullSurfaceMask,
        exclude: FullSurfaceMask,
    ) -> Result<Option<SurfaceHit>, TerrainError> {
        if self.surfaces.is_empty() {
            return Err(TerrainError::Unsupported);
        }
        let mut candidates = self.surface_bvh.ray_candidates(origin, direction);
        candidates.sort_unstable();
        let mut best: Option<SurfaceHit> = None;
        for index in candidates {
            let triangle = &self.surfaces[index];
            if include.0 != 0 && triangle.mask.0 & include.0 != include.0 {
                continue;
            }
            if triangle.mask.0 & exclude.0 != 0 {
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
                height: position[2],
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

const SURFACE_BVH_LEAF_SIZE: usize = 8;
const SURFACE_BOUNDS_EPSILON: f32 = 1.0e-4;

/// Deterministic CPU index over validated terrain triangles.
///
/// The index changes only candidate selection: triangle math and observable
/// tie-breaking remain in [`SurfaceQuery`] above.
#[derive(Clone, Debug, Default)]
struct SurfaceBvh {
    nodes: Vec<SurfaceBvhNode>,
    triangle_indices: Vec<usize>,
    root: Option<usize>,
}

#[derive(Clone, Debug)]
struct SurfaceBvhNode {
    min: [f32; 3],
    max: [f32; 3],
    kind: SurfaceBvhNodeKind,
}

#[derive(Clone, Debug)]
enum SurfaceBvhNodeKind {
    Leaf { first: usize, count: usize },
    Branch { left: usize, right: usize },
}

impl SurfaceBvh {
    fn from_surfaces(surfaces: &[RuntimeTriangle]) -> Self {
        if surfaces.is_empty() {
            return Self::default();
        }
        let mut index = Self {
            nodes: Vec::with_capacity(surfaces.len().saturating_mul(2)),
            triangle_indices: Vec::with_capacity(surfaces.len()),
            root: None,
        };
        let indices: Vec<usize> = (0..surfaces.len()).collect();
        index.root = Some(index.build(surfaces, indices));
        index
    }

    fn build(&mut self, surfaces: &[RuntimeTriangle], mut indices: Vec<usize>) -> usize {
        let (min, max) = surface_bounds(surfaces, &indices);
        if indices.len() <= SURFACE_BVH_LEAF_SIZE {
            indices.sort_unstable();
            let first = self.triangle_indices.len();
            let count = indices.len();
            self.triangle_indices.extend(indices);
            let node = self.nodes.len();
            self.nodes.push(SurfaceBvhNode {
                min,
                max,
                kind: SurfaceBvhNodeKind::Leaf { first, count },
            });
            return node;
        }

        let axis = longest_axis(min, max);
        indices.sort_by(|left, right| {
            surface_centroid(&surfaces[*left])[axis]
                .total_cmp(&surface_centroid(&surfaces[*right])[axis])
                .then_with(|| left.cmp(right))
        });
        let right_indices = indices.split_off(indices.len() / 2);
        let left = self.build(surfaces, indices);
        let right = self.build(surfaces, right_indices);
        let node = self.nodes.len();
        self.nodes.push(SurfaceBvhNode {
            min,
            max,
            kind: SurfaceBvhNodeKind::Branch { left, right },
        });
        node
    }

    fn xy_candidates(&self, point: [f32; 2]) -> Vec<usize> {
        let mut candidates = Vec::new();
        let Some(root) = self.root else {
            return candidates;
        };
        let mut pending = vec![root];
        while let Some(node_index) = pending.pop() {
            let node = &self.nodes[node_index];
            if !point_within_surface_bounds(point, node.min, node.max) {
                continue;
            }
            match node.kind {
                SurfaceBvhNodeKind::Leaf { first, count } => {
                    candidates.extend_from_slice(&self.triangle_indices[first..first + count]);
                }
                SurfaceBvhNodeKind::Branch { left, right } => {
                    pending.push(right);
                    pending.push(left);
                }
            }
        }
        candidates
    }

    fn ray_candidates(&self, origin: [f32; 3], direction: [f32; 3]) -> Vec<usize> {
        let mut candidates = Vec::new();
        let Some(root) = self.root else {
            return candidates;
        };
        let mut pending = vec![root];
        while let Some(node_index) = pending.pop() {
            let node = &self.nodes[node_index];
            if !ray_intersects_surface_bounds(origin, direction, node.min, node.max) {
                continue;
            }
            match node.kind {
                SurfaceBvhNodeKind::Leaf { first, count } => {
                    candidates.extend_from_slice(&self.triangle_indices[first..first + count]);
                }
                SurfaceBvhNodeKind::Branch { left, right } => {
                    pending.push(right);
                    pending.push(left);
                }
            }
        }
        candidates
    }
}

fn surface_bounds(surfaces: &[RuntimeTriangle], indices: &[usize]) -> ([f32; 3], [f32; 3]) {
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for index in indices {
        for vertex in surfaces[*index].vertices {
            for axis in 0..3 {
                min[axis] = min[axis].min(vertex[axis]);
                max[axis] = max[axis].max(vertex[axis]);
            }
        }
    }
    (min, max)
}

fn surface_centroid(triangle: &RuntimeTriangle) -> [f32; 3] {
    let mut centroid = [0.0; 3];
    for vertex in triangle.vertices {
        for axis in 0..3 {
            centroid[axis] += vertex[axis] / 3.0;
        }
    }
    centroid
}

fn longest_axis(min: [f32; 3], max: [f32; 3]) -> usize {
    let extent = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
    if extent[1] > extent[0] && extent[1] >= extent[2] {
        1
    } else if extent[2] > extent[0] && extent[2] > extent[1] {
        2
    } else {
        0
    }
}

fn point_within_surface_bounds(point: [f32; 2], min: [f32; 3], max: [f32; 3]) -> bool {
    point[0] >= min[0] - SURFACE_BOUNDS_EPSILON
        && point[0] <= max[0] + SURFACE_BOUNDS_EPSILON
        && point[1] >= min[1] - SURFACE_BOUNDS_EPSILON
        && point[1] <= max[1] + SURFACE_BOUNDS_EPSILON
}

fn ray_intersects_surface_bounds(
    origin: [f32; 3],
    direction: [f32; 3],
    min: [f32; 3],
    max: [f32; 3],
) -> bool {
    let mut near = 0.0_f32;
    let mut far = f32::INFINITY;
    for axis in 0..3 {
        if direction[axis].abs() <= f32::EPSILON {
            if origin[axis] < min[axis] - SURFACE_BOUNDS_EPSILON
                || origin[axis] > max[axis] + SURFACE_BOUNDS_EPSILON
            {
                return false;
            }
            continue;
        }
        let inverse = 1.0 / direction[axis];
        let first = (min[axis] - origin[axis]) * inverse;
        let second = (max[axis] - origin[axis]) * inverse;
        near = near.max(first.min(second));
        far = far.min(first.max(second));
        if far < near {
            return false;
        }
    }
    far >= 0.0
}

impl RuntimeTriangle {
    fn height_at(&self, position: [f32; 2]) -> Option<f32> {
        let a = [self.vertices[0][0], self.vertices[0][1]];
        let b = [self.vertices[1][0], self.vertices[1][1]];
        let c = [self.vertices[2][0], self.vertices[2][1]];
        let weights = barycentric_2d(position, a, b, c)?;
        if weights
            .iter()
            .all(|weight| *weight >= -1.0e-4 && *weight <= 1.0001)
        {
            Some(
                weights[0] * self.vertices[0][2]
                    + weights[1] * self.vertices[1][2]
                    + weights[2] * self.vertices[2][2],
            )
        } else {
            None
        }
    }

    fn raycast(&self, origin: [f32; 3], direction: [f32; 3]) -> Option<f32> {
        let edge1 = sub3_f64(self.vertices[1], self.vertices[0]);
        let edge2 = sub3_f64(self.vertices[2], self.vertices[0]);
        let direction = direction.map(f64::from);
        let pvec = cross3_f64(direction, edge2);
        let det = dot3_f64(edge1, pvec);
        if det == 0.0 || !det.is_finite() {
            return None;
        }
        let inv_det = 1.0 / det;
        let tvec = sub3_f64(origin, self.vertices[0]);
        let u = dot3_f64(tvec, pvec) * inv_det;
        if !(-1.0e-5..=1.00001).contains(&u) {
            return None;
        }
        let qvec = cross3_f64(tvec, edge1);
        let v = dot3_f64(direction, qvec) * inv_det;
        if v < -1.0e-5 || u + v > 1.00001 {
            return None;
        }
        let distance = dot3_f64(edge2, qvec) * inv_det;
        if distance >= 0.0 && distance.is_finite() && distance <= f64::from(f32::MAX) {
            Some(distance as f32)
        } else {
            None
        }
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
        let mut bounds_min = [f32::INFINITY, f32::INFINITY];
        let mut bounds_max = [f32::NEG_INFINITY, f32::NEG_INFINITY];
        for areal in &map.areals {
            for vertex in &areal.vertices {
                bounds_min[0] = bounds_min[0].min(vertex[0]);
                bounds_min[1] = bounds_min[1].min(vertex[1]);
                bounds_max[0] = bounds_max[0].max(vertex[0]);
                bounds_max[1] = bounds_max[1].max(vertex[1]);
            }
        }
        if !bounds_min[0].is_finite()
            || !bounds_min[1].is_finite()
            || !bounds_max[0].is_finite()
            || !bounds_max[1].is_finite()
        {
            bounds_min = [0.0, 0.0];
            bounds_max = [1.0, 1.0];
        }
        if (bounds_min[0] - bounds_max[0]).abs() <= f32::EPSILON {
            bounds_max[0] += 1.0;
        }
        if (bounds_min[1] - bounds_max[1]).abs() <= f32::EPSILON {
            bounds_max[1] += 1.0;
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
            min: bounds_min,
            max: bounds_max,
            cells,
        })
    }

    fn candidates(&self, position: [f32; 3]) -> Option<&[ArealId]> {
        if self.cells_x == 0 || self.cells_y == 0 || self.cells.is_empty() {
            return None;
        }
        let point = [position[0], position[1]];
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
    let assignments = mesh
        .face_lod_assignments()
        .map_err(TerrainError::InvalidLodMapping)?;
    let lod0_count = assignments
        .iter()
        .filter(|assignment| assignment.lod == TerrainLod::Lod0)
        .count();
    let mut triangles = Vec::with_capacity(lod0_count);
    for (face, assignment) in mesh.faces.iter().zip(assignments.iter()) {
        if assignment.lod != TerrainLod::Lod0 {
            continue;
        }
        let face_index = assignment.face_index;
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

fn valid_foundation_triangle(vertices: [[f32; 3]; 3], allow_zero_xy_area: bool) -> bool {
    if !vertices.iter().flatten().all(|value| value.is_finite()) {
        return false;
    }
    let first = [
        f64::from(vertices[1][0]) - f64::from(vertices[0][0]),
        f64::from(vertices[1][1]) - f64::from(vertices[0][1]),
    ];
    let second = [
        f64::from(vertices[2][0]) - f64::from(vertices[0][0]),
        f64::from(vertices[2][1]) - f64::from(vertices[0][1]),
    ];
    let cross = first[0] * second[1] - first[1] * second[0];
    // Native vertex welding can collapse generated band faces; query math naturally misses zero area.
    cross != 0.0 || allow_zero_xy_area
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
    let edge_second = [
        f64::from(second[0]) - f64::from(first[0]),
        f64::from(second[1]) - f64::from(first[1]),
    ];
    let edge_third = [
        f64::from(third[0]) - f64::from(first[0]),
        f64::from(third[1]) - f64::from(first[1]),
    ];
    let point_delta = [
        f64::from(point[0]) - f64::from(first[0]),
        f64::from(point[1]) - f64::from(first[1]),
    ];
    let denom = edge_second[0] * edge_third[1] - edge_third[0] * edge_second[1];
    if denom == 0.0 {
        return None;
    }
    let second_weight = (point_delta[0] * edge_third[1] - edge_third[0] * point_delta[1]) / denom;
    let third_weight = (edge_second[0] * point_delta[1] - point_delta[0] * edge_second[1]) / denom;
    let first_weight = 1.0 - second_weight - third_weight;
    let weights = [first_weight, second_weight, third_weight];
    if weights.iter().any(|weight| !weight.is_finite()) {
        return None;
    }
    Some(weights.map(|weight| weight as f32))
}

fn sub3_f64(a: [f32; 3], b: [f32; 3]) -> [f64; 3] {
    [
        f64::from(a[0]) - f64::from(b[0]),
        f64::from(a[1]) - f64::from(b[1]),
        f64::from(a[2]) - f64::from(b[2]),
    ]
}

fn cross3_f64(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot3_f64(a: [f64; 3], b: [f64; 3]) -> f64 {
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
            world.locate_areal([0.25, 0.25, 41.0]).expect("locate"),
            Some(ArealId(0))
        );
        assert_eq!(
            world.locate_areal([1.75, 0.25, -12.0]).expect("locate"),
            Some(ArealId(1))
        );
        assert_eq!(
            world
                .route(RouteRequest {
                    start: [0.25, 0.25, 0.0],
                    goal: [1.75, 0.25, 0.0],
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
                    start: [10.0, 10.0, 0.0],
                    goal: [1.75, 0.25, 0.0],
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
                [0.25, 0.25, 2.0],
                [0.0, 0.0, -1.0],
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
                    [0.25, 0.25, 2.0],
                    [0.0, 0.0, -1.0],
                    FullSurfaceMask(0x8000_0000)
                )
                .expect("raycast"),
            None
        );

        // Native include masks require every requested bit, while exclusion
        // masks reject any matching face bit.
        assert_eq!(
            world
                .raycast_excluding(
                    [0.25, 0.25, 2.0],
                    [0.0, 0.0, -1.0],
                    FullSurfaceMask(0x0000_0003),
                    FullSurfaceMask(0),
                )
                .expect("raycast include all bits"),
            None
        );
        assert_eq!(
            world
                .raycast_excluding(
                    [0.25, 0.25, 2.0],
                    [0.0, 0.0, -1.0],
                    FullSurfaceMask(0),
                    FullSurfaceMask(0x0000_0001),
                )
                .expect("raycast exclusion"),
            None
        );
    }

    #[test]
    fn surface_queries_use_lod0_and_reject_missing_node_metadata() {
        let mut mesh = synthetic_land_mesh();
        mesh.positions = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 100.0],
            [1.0, 0.0, 100.0],
            [0.0, 1.0, 100.0],
        ];
        mesh.faces = vec![
            terrain_face(FullSurfaceMask(1), [0, 1, 2]),
            terrain_face(FullSurfaceMask(2), [3, 4, 5]),
        ];
        assign_terrain_lod_slots(&mut mesh, &[[0, 1]], &[[0, 1], [1, 1]]);

        let world = TerrainWorld::from_land_msh(&mesh).expect("LOD0 terrain");
        assert_eq!(world.surface_count(), 1);
        assert_eq!(world.height_at([0.25, 0.25]).expect("height"), Some(0.0));
        assert_eq!(
            world
                .raycast([0.25, 0.25, 101.0], [0.0, 0.0, -1.0], FullSurfaceMask(2),)
                .expect("LOD1-only mask"),
            None
        );

        mesh.nodes_raw.clear();
        assert!(matches!(
            TerrainWorld::from_land_msh(&mesh),
            Err(TerrainError::InvalidLodMapping(
                fparkan_terrain_format::TerrainLodError::MissingNodes { face_count: 2 }
            ))
        ));
        mesh.faces.clear();
        assert_eq!(
            TerrainWorld::from_land_msh(&mesh)
                .expect("empty scene without Node38")
                .surface_count(),
            0
        );
    }

    #[test]
    fn foundation_world_queries_use_the_canonical_cut_and_generated_band() {
        use crate::{build_basement_foundations, BasementFootprint};

        let mesh = synthetic_land_mesh();
        let outer = [[0.1, 0.1], [0.9, 0.1], [0.9, 0.9], [0.1, 0.9]];
        let inner = [
            [0.4, 0.4, 1.0],
            [0.6, 0.4, 1.0],
            [0.6, 0.6, 1.0],
            [0.4, 0.6, 1.0],
        ];
        let foundation = build_basement_foundations(
            &mesh,
            &[BasementFootprint {
                outer: &outer,
                inner: &inner,
                flatten_inner_to_mean_outer_z: true,
            }],
        )
        .expect("foundation");
        let expected_fragments = foundation.fragments.clone();
        let expected_fragment_count = foundation.fragments.len();
        let world = TerrainWorld::from_land_msh(&mesh)
            .expect("source world")
            .with_basement_foundation(foundation)
            .expect("foundation world");

        assert_eq!(world.surface_count(), expected_fragment_count);
        assert_eq!(
            world
                .basement_foundation()
                .map(|value| value.fragments.as_slice()),
            Some(expected_fragments.as_slice())
        );
        assert!(world
            .height_at([0.05, 0.05])
            .expect("band height")
            .is_some());
        assert!(world.height_at([0.3, 0.3]).expect("band height").is_some());
        assert_eq!(world.height_at([0.5, 0.5]).expect("hole height"), None);
        assert_eq!(
            world
                .raycast([0.5, 0.5, 4.0], [0.0, 0.0, -1.0], FullSurfaceMask(0),)
                .expect("hole ray"),
            None
        );

        let mut generated_face = mesh.faces.len();
        for (runtime, fragment) in world.surfaces.iter().zip(&expected_fragments) {
            assert_eq!(runtime.mask, fragment.surface_mask);
            assert_eq!(runtime.vertices, fragment.positions);
            if let Some(source_face) = fragment.source_face {
                assert_eq!(runtime.face, source_face);
                assert_eq!(runtime.mask, mesh.faces[source_face].flags);
            } else {
                assert_eq!(runtime.face, generated_face);
                generated_face += 1;
            }
        }
        assert!(generated_face > mesh.faces.len());
    }

    #[test]
    fn foundation_install_accepts_thin_nonzero_fragments_for_surface_queries() {
        let mesh = synthetic_land_mesh();
        let fragment = BasementFoundationFragment {
            source_face: Some(0),
            surface_mask: mesh.faces[0].flags,
            positions: [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.000_000_5, 0.0]],
        };
        let foundation = BasementFoundation {
            fragments: vec![fragment],
            placements: Vec::new(),
        };
        let world = TerrainWorld::from_land_msh(&mesh)
            .expect("source world")
            .with_basement_foundation(foundation)
            .expect("thin but non-degenerate triangle");

        assert_eq!(world.surface_count(), 1);
        assert_eq!(
            world.height_at([0.25, 0.000_000_125]).expect("height"),
            Some(0.0)
        );
        assert!(world
            .raycast(
                [0.25, 0.000_000_125, 1.0],
                [0.0, 0.0, -1.0],
                FullSurfaceMask(0),
            )
            .expect("raycast")
            .is_some());
    }

    #[test]
    fn foundation_install_keeps_finite_welded_zero_area_band_for_queries() {
        let mesh = synthetic_land_mesh();
        let positions = [
            [1644.398, 1118.5295, 132.54382],
            [1644.4183, 1118.5342, 132.54755],
            [1644.4183, 1118.5342, 132.54755],
        ];
        let foundation = BasementFoundation {
            fragments: vec![BasementFoundationFragment {
                source_face: None,
                surface_mask: FullSurfaceMask(0x8300),
                positions,
            }],
            placements: Vec::new(),
        };
        let world = TerrainWorld::from_land_msh(&mesh)
            .expect("source world")
            .with_basement_foundation(foundation)
            .expect("finite generated fragment retained after native vertex welding");

        assert_eq!(world.surface_count(), 1);
        assert_eq!(
            world
                .basement_foundation()
                .expect("installed foundation")
                .fragments[0]
                .positions,
            positions
        );
        assert_eq!(
            world
                .height_at([positions[1][0], positions[1][1]])
                .expect("degenerate height query"),
            None
        );
        assert_eq!(
            world
                .raycast(
                    [positions[1][0], positions[1][1], 200.0],
                    [0.0, 0.0, -1.0],
                    FullSurfaceMask(0),
                )
                .expect("degenerate ray query"),
            None
        );
    }

    #[test]
    fn foundation_install_rejects_invalid_source_identity_masks_flags_and_non_finite_geometry() {
        let mesh = synthetic_land_mesh();
        let source_triangle = [[0.0, 0.0, 0.0], [1.0, 0.0, 1.0], [0.0, 1.0, 1.0]];
        let generated_triangle = [[0.0, 0.0, 0.0], [0.25, 0.0, 0.0], [0.0, 0.25, 0.0]];
        let invalid = [
            BasementFoundationFragment {
                source_face: Some(mesh.faces.len()),
                surface_mask: mesh.faces[0].flags,
                positions: source_triangle,
            },
            BasementFoundationFragment {
                source_face: Some(0),
                surface_mask: FullSurfaceMask(mesh.faces[0].flags.0 ^ 1),
                positions: source_triangle,
            },
            BasementFoundationFragment {
                source_face: None,
                surface_mask: FullSurfaceMask(0x0300),
                positions: generated_triangle,
            },
            BasementFoundationFragment {
                source_face: None,
                surface_mask: FullSurfaceMask(0x8300),
                positions: [[0.0, 0.0, 0.0], [0.25, 0.0, 0.0], [0.0, f32::NAN, 0.0]],
            },
        ];

        for (index, fragment) in invalid.into_iter().enumerate() {
            let foundation = BasementFoundation {
                fragments: vec![fragment],
                placements: Vec::new(),
            };
            assert!(
                matches!(
                    TerrainWorld::from_land_msh(&mesh)
                        .expect("source world")
                        .with_basement_foundation(foundation),
                    Err(TerrainError::InvalidFoundationFragment { fragment: 0 })
                ),
                "invalid fragment case {index}"
            );
        }
    }

    #[test]
    fn foundation_install_rejects_lod1_source_faces() {
        use fparkan_terrain_format::TerrainLod;

        let mut mesh = synthetic_land_mesh();
        mesh.positions.push([0.0, 0.0, 2.0]);
        mesh.positions.push([1.0, 0.0, 2.0]);
        mesh.positions.push([0.0, 1.0, 2.0]);
        mesh.faces
            .push(terrain_face(FullSurfaceMask(0x0000_0003), [4, 5, 6]));
        mesh.nodes_raw[10..12].copy_from_slice(&1_u16.to_le_bytes());
        let mut lod1_slot = [0_u8; 68];
        lod1_slot[0..2].copy_from_slice(&2_u16.to_le_bytes());
        lod1_slot[2..4].copy_from_slice(&1_u16.to_le_bytes());
        mesh.slots.slots_raw.push(lod1_slot);
        assert_eq!(
            mesh.face_lod_assignments().expect("LOD assignment")[2].lod,
            TerrainLod::Lod1
        );

        let foundation = BasementFoundation {
            fragments: vec![BasementFoundationFragment {
                source_face: Some(2),
                surface_mask: mesh.faces[2].flags,
                positions: [[0.0, 0.0, 2.0], [1.0, 0.0, 2.0], [0.0, 1.0, 2.0]],
            }],
            placements: Vec::new(),
        };
        assert!(matches!(
            TerrainWorld::from_land_msh(&mesh)
                .expect("source world")
                .with_basement_foundation(foundation),
            Err(TerrainError::InvalidFoundationFragment { fragment: 0 })
        ));
    }

    #[test]
    fn surface_bvh_reduces_candidates_without_changing_surface_math() {
        let surfaces: Vec<_> = (0_u16..16)
            .map(|face| {
                let x = f32::from(face) * 10.0;
                RuntimeTriangle {
                    face: usize::from(face),
                    mask: FullSurfaceMask(1),
                    vertices: [[x, 0.0, x], [x + 1.0, 0.0, x], [x, 1.0, x]],
                }
            })
            .collect();
        let index = SurfaceBvh::from_surfaces(&surfaces);

        let xy = index.xy_candidates([0.25, 0.25]);
        assert!(xy.contains(&0));
        assert!(xy.len() < surfaces.len());
        assert_eq!(surfaces[xy[0]].height_at([0.25, 0.25]), Some(0.0));

        let ray = index.ray_candidates([0.25, 0.25, 5.0], [0.0, 0.0, -1.0]);
        assert!(ray.contains(&0));
        assert!(ray.len() < surfaces.len());
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_corpus_land_maps_build_navigation_worlds() {
        for (corpus, expected_files, expected_areals) in [
            ("IS", 33_usize, 34_662_usize),
            ("IS2", 32_usize, 18_984_usize),
        ] {
            let root = corpus_root(corpus);
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
    #[ignore = "requires licensed corpus"]
    fn licensed_corpus_land_meshes_build_surface_worlds() {
        for (corpus, expected_files, expected_faces) in [
            ("IS", 33_usize, 275_882_usize),
            ("IS2", 32_usize, 184_454_usize),
        ] {
            let root = corpus_root(corpus);
            let mut files = 0usize;
            let mut faces = 0usize;
            let mut indexed_queries = 0usize;
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
                let expected_lod0_faces = mesh
                    .face_lod_assignments()
                    .unwrap_or_else(|err| panic!("{corpus} {path:?}: {err}"))
                    .iter()
                    .filter(|assignment| assignment.lod == TerrainLod::Lod0)
                    .count();
                let world = TerrainWorld::from_land_msh(&mesh)
                    .unwrap_or_else(|err| panic!("{corpus} {path:?}: {err}"));
                assert_eq!(
                    world.surface_count(),
                    expected_lod0_faces,
                    "{corpus} {path:?}"
                );
                if corpus == "IS" {
                    let map_name = path
                        .parent()
                        .and_then(Path::file_name)
                        .and_then(|name| name.to_str())
                        .map(str::to_ascii_lowercase);
                    let expected = match map_name.as_deref() {
                        Some("automap") => Some(1_912),
                        Some("prol") => Some(5_868),
                        Some("tut_1") => Some(5_046),
                        _ => None,
                    };
                    if let Some(expected) = expected {
                        assert_eq!(world.surface_count(), expected, "{corpus} {path:?}");
                    }
                }
                if world.surface_count() > SURFACE_BVH_LEAF_SIZE {
                    let triangle = &world.surfaces[0];
                    let point = [
                        (triangle.vertices[0][0]
                            + triangle.vertices[1][0]
                            + triangle.vertices[2][0])
                            / 3.0,
                        (triangle.vertices[0][1]
                            + triangle.vertices[1][1]
                            + triangle.vertices[2][1])
                            / 3.0,
                    ];
                    let candidates = world.surface_bvh.xy_candidates(point);
                    assert!(candidates.contains(&0), "{corpus} {path:?} face zero");
                    assert!(
                        candidates.len() < world.surface_count(),
                        "{corpus} {path:?} surface index did not reduce candidates"
                    );
                    indexed_queries += 1;
                }
                files += 1;
                faces += world.surface_count();
            }

            assert_eq!(files, expected_files, "{corpus} Land.msh count");
            assert_eq!(faces, expected_faces, "{corpus} surface face count");
            assert_eq!(indexed_queries, files, "{corpus} indexed Land.msh count");
        }
    }

    fn synthetic_land_mesh() -> LandMeshDocument {
        use fparkan_terrain_format::{TerrainSlotTable, TerrainStream};

        let face0 = terrain_face(FullSurfaceMask(0x0000_0001), [0, 1, 2]);
        let face1 = terrain_face(FullSurfaceMask(0x0000_0002), [1, 3, 2]);
        let mut mesh = LandMeshDocument {
            streams: Vec::<TerrainStream>::new(),
            nodes_raw: Vec::new(),
            slots: TerrainSlotTable {
                header_raw: Vec::new(),
                slots_raw: Vec::new(),
            },
            positions: vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 1.0],
                [0.0, 1.0, 1.0],
                [1.0, 1.0, 2.0],
            ],
            normals: Vec::new(),
            uv0: Vec::new(),
            accelerator: Vec::new(),
            aux14: Vec::new(),
            aux18: Vec::new(),
            faces: vec![face0, face1],
        };
        assign_terrain_lod_slots(&mut mesh, &[[0, u16::MAX]], &[[0, 2]]);
        mesh
    }

    fn assign_terrain_lod_slots(
        mesh: &mut LandMeshDocument,
        node_slots: &[[u16; 2]],
        face_ranges: &[[u16; 2]],
    ) {
        const NODE_STRIDE: usize = 38;
        let mut nodes = vec![u8::MAX; node_slots.len() * NODE_STRIDE];
        for (node_index, [lod0, lod1]) in node_slots.iter().copied().enumerate() {
            let offset = node_index * NODE_STRIDE + 8;
            nodes[offset..offset + 2].copy_from_slice(&lod0.to_le_bytes());
            nodes[offset + 2..offset + 4].copy_from_slice(&lod1.to_le_bytes());
        }
        let mut slots = vec![[0_u8; 68]; face_ranges.len()];
        for (slot, [start, count]) in slots.iter_mut().zip(face_ranges.iter().copied()) {
            slot[..2].copy_from_slice(&start.to_le_bytes());
            slot[2..4].copy_from_slice(&count.to_le_bytes());
        }
        mesh.nodes_raw = nodes;
        mesh.slots.slots_raw = slots;
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
                    anchor: [0.5, 0.5, 0.0],
                    reserved_12: 0.0,
                    area_metric: 1.0,
                    normal: [0.0, 0.0, 1.0],
                    logic_flag: 0,
                    reserved_36: 0,
                    class_id: 0,
                    reserved_44: 0,
                    vertices: vec![
                        [0.0, 0.0, 0.0],
                        [1.0, 0.0, 0.0],
                        [1.0, 1.0, 0.0],
                        [0.0, 1.0, 0.0],
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
                    anchor: [1.5, 0.5, 0.0],
                    reserved_12: 0.0,
                    area_metric: 1.0,
                    normal: [0.0, 0.0, 1.0],
                    logic_flag: 0,
                    reserved_36: 0,
                    class_id: 0,
                    reserved_44: 0,
                    vertices: vec![
                        [1.0, 0.0, 0.0],
                        [2.0, 0.0, 0.0],
                        [2.0, 1.0, 0.0],
                        [1.0, 1.0, 0.0],
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

    fn corpus_root(name: &str) -> PathBuf {
        let variable = match name {
            "IS" => "FPARKAN_CORPUS_PART1_ROOT",
            "IS2" => "FPARKAN_CORPUS_PART2_ROOT",
            _ => panic!("unknown licensed corpus part: {name}"),
        };
        let root = std::env::var_os(variable)
            .map(PathBuf::from)
            .unwrap_or_else(|| panic!("{variable} is required for licensed corpus tests"));
        assert!(
            root.is_dir(),
            "licensed corpus root is missing: {}",
            root.display()
        );
        root
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
