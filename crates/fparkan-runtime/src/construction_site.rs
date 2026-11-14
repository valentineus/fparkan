use fparkan_terrain::{NavigationQuery, SurfaceQuery, TerrainWorld};
use fparkan_world::{handle_by_original_id, registration_sequence, transform_state, World};

use crate::{mining::MineSite, MissionObjectDraft};

#[derive(Default)]
pub(super) struct SiteReservations(Vec<([u32; 2], u32)>);

impl SiteReservations {
    pub(super) fn find(
        &mut self,
        terrain: &TerrainWorld,
        world: &World,
        drafts: &[MissionObjectDraft],
        mines: &[MineSite],
        base: [f32; 2],
        now_ms: u32,
    ) -> Option<[u32; 2]> {
        self.0
            .retain(|&(_, time)| now_ms.wrapping_sub(time) <= 60_000);
        let occupied = drafts.iter().filter_map(|draft| {
            if draft.unit_dat_header_flags? & 0x8000_0000 == 0 {
                return None;
            }
            let handle = handle_by_original_id(world, draft.original_id?)?;
            registration_sequence(world, handle).ok()??;
            let [x, y, _] = transform_state(world, handle)
                .ok()?
                .position
                .map(f32::from_bits);
            Some([x, y])
        });
        let deposits = mines.iter().filter_map(|site| {
            let [x, y, _] = site.position;
            site.live_mine(world).is_none().then_some([x, y])
        });
        let occupied = occupied
            .chain(deposits)
            .chain(
                self.0
                    .iter()
                    .map(|&(position, _)| position.map(|axis| axis as f32)),
            )
            .collect::<Vec<_>>();
        let position = find_site(base, &occupied, |[x, y]| {
            // shortcut: use navigable terrain polygons until the ArealMap placement bit is mapped.
            terrain.locate_areal([x, y, 0.0]).ok()??;
            terrain.height_at([x, y]).ok()?
        })?;
        if self.0.len() < 20 {
            self.0.push((position, now_ms));
        }
        Some(position)
    }
}

fn find_site(
    base: [f32; 2],
    occupied: &[[f32; 2]],
    mut ground: impl FnMut([f32; 2]) -> Option<f32>,
) -> Option<[u32; 2]> {
    let [base_x, base_y] = base.map(f32::trunc);
    for ring in 0..10 {
        let radius = ring as f64 * 150.0;
        let step = (150.0 / radius).atan() as f32;
        let mut angle = 0.0_f32;
        while angle < 2.0 * (31415.0 / 10000.0) {
            let (sin, cos) = f64::from(angle).sin_cos();
            let x = base_x + (radius * cos).trunc() as f32;
            let y = base_y + (radius * sin).trunc() as f32;
            if x > 0.0
                && y > 0.0
                && occupied
                    .iter()
                    .all(|&[a, b]| (x - a).trunc().hypot((y - b).trunc()) >= 250.0)
            {
                let corners = [
                    [x + 75.0, y + 75.0],
                    [x - 75.0, y - 75.0],
                    [x + 75.0, y - 75.0],
                    [x - 75.0, y + 75.0],
                ];
                let [Some(a), Some(b), Some(c), Some(d)] = corners.map(&mut ground) else {
                    angle += step;
                    continue;
                };
                let mean = (a + b + c + d) * 0.25;
                if [a, b, c, d]
                    .into_iter()
                    .map(|height| (height - mean).abs())
                    .sum::<f32>()
                    * 0.25
                    <= 20.0
                {
                    return Some([x as u32, y as u32]);
                }
            }
            angle += step;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn site_checks_the_footprint_and_clearance_in_base_ring_order() {
        let flat = |_| Some(10.0);
        assert_eq!(find_site([600.8, 600.9], &[], flat), Some([600, 600]));
        assert_eq!(
            find_site([600.0, 600.0], &[[600.0, 600.0]], flat),
            Some([900, 600])
        );
        assert_eq!(
            find_site([600.0, 600.0], &[[350.0, 600.0]], flat),
            Some([600, 600])
        );
        assert!(find_site([600.0, 600.0], &[], |_| None).is_none());
        assert!(find_site([600.0, 600.0], &[], |[x, _]| Some(x)).is_none());
    }
}
