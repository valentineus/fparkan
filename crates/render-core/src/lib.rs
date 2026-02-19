use msh_core::Model;

#[derive(Clone, Debug)]
pub struct RenderMesh {
    pub vertices: Vec<[f32; 3]>,
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
                vertices.push(*pos);
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
    let mut iter = vertices.iter();
    let first = iter.next()?;
    let mut min_v = *first;
    let mut max_v = *first;

    for v in iter {
        for i in 0..3 {
            if v[i] < min_v[i] {
                min_v[i] = v[i];
            }
            if v[i] > max_v[i] {
                max_v[i] = v[i];
            }
        }
    }

    Some((min_v, max_v))
}

#[cfg(test)]
mod tests;
