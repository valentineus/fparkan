use fparkan_terrain::{NavigationQuery, SurfaceQuery, TerrainWorld};
use fparkan_world::{handle_by_original_id, registration_sequence, transform_state, World};

use crate::{mining::MineSite, MissionObjectDraft};

#[derive(Default)]
pub(super) struct SiteReservations(Vec<([f32; 2], u32)>);

pub(super) fn building_centers<'a>(
    world: &'a World,
    drafts: &'a [MissionObjectDraft],
) -> impl Iterator<Item = [f32; 2]> + 'a {
    drafts.iter().filter_map(|draft| {
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
    })
}

pub(super) fn separated([x, y]: [f32; 2], [a, b]: [f32; 2]) -> bool {
    (x - a).trunc().hypot((y - b).trunc()) >= 250.0
}

impl SiteReservations {
    pub(super) fn reserve(&mut self, position: [f32; 2], now_ms: u32) {
        self.0
            .retain(|&(_, time)| now_ms.wrapping_sub(time) <= 60_000);
        if position != [0.0; 2] && self.0.len() < 20 {
            self.0.push((position, now_ms));
        }
    }

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
        let occupied = building_centers(world, drafts);
        let deposits = mines.iter().filter_map(|site| {
            let [x, y, _] = site.position;
            site.live_mine(world).is_none().then_some([x, y])
        });
        let occupied = occupied
            .chain(deposits)
            .chain(self.0.iter().map(|&(position, _)| position))
            .collect::<Vec<_>>();
        let position = find_site(base, &occupied, |[x, y]| {
            // shortcut: use navigable terrain polygons until the ArealMap placement bit is mapped.
            terrain.locate_areal([x, y, 0.0]).ok()??;
            terrain.height_at([x, y]).ok()?
        })?;
        self.reserve(position.map(|axis| axis as f32), now_ms);
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
            if x > 0.0 && y > 0.0 && occupied.iter().all(|&other| separated([x, y], other)) {
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
    #[test]
    fn reservations_keep_fractional_coordinates_and_expire_after_one_minute() {
        let mut sites = super::SiteReservations::default();
        sites.reserve([0.0; 2], 1);
        assert!(sites.0.is_empty());
        for _ in 0..25 {
            sites.reserve([10.25, 20.75], 1);
        }
        assert_eq!(sites.0.len(), 20);
        sites.reserve([30.5, 40.5], 60_001);
        assert_eq!(sites.0[0], ([10.25, 20.75], 1));
        sites.reserve([30.5, 40.5], 60_002);
        assert_eq!(sites.0, vec![([30.5, 40.5], 60_002)]);
    }

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
