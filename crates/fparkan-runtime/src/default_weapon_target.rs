use super::{behavior_clock::next_deadline, ObjectHandle};

#[derive(Clone, Default)]
pub(super) struct DefaultWeaponTarget {
    pub(super) selected: Option<ObjectHandle>,
    update_ms: u32,
    search_ms: u32,
}

impl DefaultWeaponTarget {
    pub(super) fn searches_at(&self, now: u32) -> bool {
        due(self.update_ms, now) && due(self.search_ms, now)
    }

    pub(super) fn update(&mut self, now: u32, candidate: Option<ObjectHandle>, rng: &mut u32) {
        if !due(self.update_ms, now) {
            return;
        }
        self.update_ms = next_deadline(now, 1, 1, rng);
        if due(self.search_ms, now) {
            self.search_ms = next_deadline(now, 31, 15, rng);
            self.selected = candidate;
        }
    }
}

fn due(deadline: u32, now: u32) -> bool {
    deadline == 0 || deadline < now
}

// Behavior mode 2 rounds the score before comparing it, retaining the first tie.
pub(super) fn score(from: [f32; 3], candidate: [f32; 3]) -> Option<f32> {
    let x = from[0] - candidate[0];
    let y = from[1] - candidate[1];
    let distance = (x * x + y * y).sqrt();
    (distance < 500.0).then(|| 1.0 / (10.0 + distance))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn score_uses_root_xy_and_a_strict_search_radius() {
        let origin = [5.0, -10.0, 1.0];
        assert_eq!(
            score(origin, [35.0, 30.0, 1000.0]),
            score(origin, [55.0, -10.0, -1000.0])
        );
        assert_eq!(score(origin, [305.0, 390.0, 0.0]), None);
        assert!(score(origin, [504.9, -10.0, 0.0]).is_some());
        assert_eq!(
            score([0.0; 3], [16.0, 0.0, 0.0]),
            score([0.0; 3], [15.999999, 0.0, 0.0])
        );
    }

    #[test]
    fn keeps_selected_target_until_search_is_due() {
        let mut target = DefaultWeaponTarget::default();
        let one = ObjectHandle {
            slot: 1,
            generation: 0,
        };
        let two = ObjectHandle {
            slot: 2,
            generation: 0,
        };
        let mut rng = 1;
        target.update(100, Some(one), &mut rng);
        assert_eq!((target.update_ms, target.search_ms), (174, 2215));
        assert!(!target.searches_at(174));
        target.update(175, Some(two), &mut rng);
        assert_eq!(target.selected, Some(one));
        assert!(!target.searches_at(2215));
        assert!(target.searches_at(2216));
        target.update(2216, Some(two), &mut rng);
        assert_eq!(target.selected, Some(two));
    }
}
