use fparkan_assets::{PreparedComponentControl, PreparedControlTimelineRow};
use fparkan_msh::{node38_metadata, ModelAsset, NodeId};

/// The independent CTL 24 tracks that move a weapon between its mounts.
#[derive(Clone)]
pub(super) struct WeaponDeployment {
    tracks: Vec<MountTrack>,
    extending: bool,
    pending_shot: bool,
}

#[derive(Clone)]
struct MountTrack {
    row: PreparedControlTimelineRow,
    duration_ms: f64,
    position_ms: f64,
}

impl WeaponDeployment {
    pub(super) fn prepare(
        control: &PreparedComponentControl,
        model: &ModelAsset,
        muzzles: impl Iterator<Item = u32>,
    ) -> Option<Self> {
        let ancestors: Vec<_> = muzzles
            .map(|node| mount_ancestors(model, NodeId(node)))
            .collect::<Option<_>>()?;
        let mut groups = control.typed_records.iter().filter(|group| {
            group.tag == 24
                && group.fixed_words[2] == 0
                && group.fixed_words[6] == 0x21
                && group.fixed_words[9] == 1.0f32.to_bits()
                && ancestors.iter().all(|chain| {
                    group.values.iter().any(|&index| {
                        control
                            .timeline_rows
                            .get(index as usize)
                            .is_some_and(|row| chain.contains(&row.node_index))
                    })
                })
        });
        let group = groups.next()?;
        if groups.next().is_some() {
            return None;
        }
        let tracks = group
            .values
            .iter()
            .map(|&index| {
                let row = *control.timeline_rows.get(index as usize)?;
                let rate = f32::from_bits(row.raw_parameters[2]);
                if row.flags != 0
                    || row.node_index >= model.node_count
                    || !(0.0..=1.0).contains(&row.initial_blend)
                    || !rate.is_finite()
                    || rate <= 0.0
                {
                    return None;
                }
                let duration_ms = 1000.0 / f64::from(rate);
                Some(MountTrack {
                    row,
                    duration_ms,
                    position_ms: f64::from(row.initial_blend) * duration_ms,
                })
            })
            .collect::<Option<_>>()?;
        Some(Self {
            tracks,
            extending: false,
            pending_shot: false,
        })
    }

    /// Preserves a short click until the mounting tracks reach their firing pose.
    pub(super) fn sample(
        &mut self,
        elapsed_ms: u32,
        selected: bool,
        trigger: bool,
        cycling: bool,
    ) -> bool {
        let step = f64::from(elapsed_ms);
        for track in &mut self.tracks {
            track.position_ms = if self.extending {
                (track.position_ms + step).min(track.duration_ms)
            } else {
                (track.position_ms - step).max(0.0)
            };
        }
        // shortcut: use the manual trigger to deploy. Native CTL 24 activation
        // rules beyond the authored tracks and clock still need a live comparison.
        self.pending_shot = selected && (self.pending_shot || trigger);
        self.extending = self.pending_shot || cycling;
        let start = self.pending_shot
            && self
                .tracks
                .iter()
                .all(|track| track.position_ms == track.duration_ms);
        if start {
            self.pending_shot = false;
        }
        start
    }

    pub(super) fn apply(&self, node_times: &mut [Option<f32>]) {
        for track in &self.tracks {
            if let Some(time) = node_times.get_mut(track.row.node_index) {
                let blend = (track.position_ms / track.duration_ms) as f32;
                *time = Some(track.row.frame_a + (track.row.frame_b - track.row.frame_a) * blend);
            }
        }
    }
}

fn mount_ancestors(model: &ModelAsset, mut node: NodeId) -> Option<Vec<usize>> {
    let mut ancestors = Vec::new();
    loop {
        ancestors.push(node.0 as usize);
        let parent = node38_metadata(model, node)?.parent_or_link_raw;
        if parent == u16::MAX {
            return Some(ancestors);
        }
        let parent = u32::from(parent);
        if parent >= node.0 {
            return None;
        }
        node = NodeId(parent);
    }
}
