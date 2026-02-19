use msh_core::Model;

pub const DEFAULT_UV_SCALE: f32 = 1024.0;

#[derive(Clone, Debug)]
pub struct RenderVertex {
    pub position: [f32; 3],
    pub uv0: [f32; 2],
}

#[derive(Clone, Debug)]
pub struct RenderMesh {
    pub vertices: Vec<RenderVertex>,
    pub batch_count: usize,
}

impl RenderMesh {
    pub fn triangle_count(&self) -> usize {
        self.vertices.len() / 3
    }
}

/// Builds an expanded triangle list for a specific LOD/group pair.
///
/// The output is suitable for simple `glDrawArrays(GL_TRIANGLES, ...)` paths.
pub fn build_render_mesh(model: &Model, lod: usize, group: usize) -> RenderMesh {
    let mut vertices = Vec::new();
    let mut batch_count = 0usize;
    let uv0 = model.uv0.as_ref();

    for node_index in 0..model.node_count {
        let Some(slot_idx) = model.slot_index(node_index, lod, group) else {
            continue;
        };
        let Some(slot) = model.slots.get(slot_idx) else {
            continue;
        };
        let batch_start = usize::from(slot.batch_start);
        let batch_end = batch_start.saturating_add(usize::from(slot.batch_count));
        if batch_end > model.batches.len() {
            continue;
        }

        for batch in &model.batches[batch_start..batch_end] {
            let index_start = usize::try_from(batch.index_start).unwrap_or(usize::MAX);
            let index_count = usize::from(batch.index_count);
            let index_end = index_start.saturating_add(index_count);
            if index_end > model.indices.len() || index_count < 3 {
                continue;
            }

            for &idx in &model.indices[index_start..index_end] {
                let final_idx_u64 = u64::from(batch.base_vertex).saturating_add(u64::from(idx));
                let Ok(final_idx) = usize::try_from(final_idx_u64) else {
                    continue;
                };
                let Some(pos) = model.positions.get(final_idx) else {
                    continue;
                };
                let uv = uv0
                    .and_then(|uvs| uvs.get(final_idx))
                    .copied()
                    .map(|packed| {
                        [
                            packed[0] as f32 / DEFAULT_UV_SCALE,
                            packed[1] as f32 / DEFAULT_UV_SCALE,
                        ]
                    })
                    .unwrap_or([0.0, 0.0]);
                vertices.push(RenderVertex {
                    position: *pos,
                    uv0: uv,
                });
            }
            batch_count += 1;
        }
    }

    RenderMesh {
        vertices,
        batch_count,
    }
}

pub fn compute_bounds(vertices: &[[f32; 3]]) -> Option<([f32; 3], [f32; 3])> {
    compute_bounds_impl(vertices.iter().copied())
}

pub fn compute_bounds_for_mesh(vertices: &[RenderVertex]) -> Option<([f32; 3], [f32; 3])> {
    compute_bounds_impl(vertices.iter().map(|v| v.position))
}

fn compute_bounds_impl<I>(mut positions: I) -> Option<([f32; 3], [f32; 3])>
where
    I: Iterator<Item = [f32; 3]>,
{
    let first = positions.next()?;
    let mut min_v = first;
    let mut max_v = first;

    for pos in positions {
        for i in 0..3 {
            if pos[i] < min_v[i] {
                min_v[i] = pos[i];
            }
            if pos[i] > max_v[i] {
                max_v[i] = pos[i];
            }
        }
    }

    Some((min_v, max_v))
}

#[cfg(test)]
mod tests;
