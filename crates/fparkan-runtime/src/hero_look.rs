use fparkan_path::{normalize_relative, PathPolicy};
use fparkan_vfs::Vfs;
use std::f32::consts::TAU;

#[derive(Clone, Copy, Debug)]
pub(super) struct HeroLook {
    turns: f32,
    sensitivity: f32,
    filtered_x: f32,
    phase: f32,
}

impl HeroLook {
    pub(super) fn load(vfs: &dyn Vfs) -> Self {
        let read = |name: &[u8]| {
            let path = normalize_relative(name, PathPolicy::StrictLegacy).ok()?;
            vfs.read(&path).ok()
        };
        let table = read(b"hero.tbl");
        let settings = read(b"Iron_3D.ini");
        Self {
            turns: table
                .as_deref()
                .and_then(|bytes| horizontal_binding(&String::from_utf8_lossy(bytes)))
                .unwrap_or(0.15),
            sensitivity: settings
                .as_deref()
                .and_then(|bytes| mouse_sensitivity(&String::from_utf8_lossy(bytes)))
                .unwrap_or(1.0),
            filtered_x: 0.0,
            phase: 0.5,
        }
    }

    pub(super) fn reset(&mut self) {
        self.filtered_x = 0.0;
        self.phase = 0.5;
    }

    pub(super) fn has_state(self) -> bool {
        self.filtered_x != 0.0 || self.phase != 0.5
    }

    /// Samples the input once and returns the yaw delivered by this tick.
    pub(super) fn advance(&mut self, dx: f32, dy: f32, max_angle: f32) -> f32 {
        self.filtered_x = if dx == 0.0 && dy == 0.0 {
            0.0
        } else {
            dx.clamp(i32::MIN as f32, i32::MAX as f32) * self.sensitivity * 0.95
                + self.filtered_x * 0.05
        };
        let scaled = self.filtered_x * 0.006;
        let axis = if scaled.abs() > 0.999 {
            scaled.signum()
        } else {
            scaled
        };
        if axis != 0.0 {
            let phase = self.phase + axis * self.turns;
            self.phase = if phase >= 1.0 {
                phase - 1.0
            } else if phase <= 0.0 {
                phase + 1.0
            } else {
                phase
            };
        }

        let requested = -(self.phase - 0.5) * TAU;
        let delivered = requested.clamp(-max_angle, max_angle);
        self.phase = if requested == delivered {
            0.5
        } else {
            (self.phase + delivered * (1.0 / TAU)).rem_euclid(1.0)
        };
        delivered
    }
}

fn horizontal_binding(table: &str) -> Option<f32> {
    table.lines().find_map(|line| {
        let line = line.split_once("//").map_or(line, |(binding, _)| binding);
        let mut words = line.split_ascii_whitespace();
        for name in [
            "MOUSE",
            "SCAN_NULL",
            "SCAN_MOUSE_X",
            "1",
            "CICLS_UNKNOWN",
            "MCMD_ANGLE_Z",
        ] {
            if !words.next()?.eq_ignore_ascii_case(name) {
                return None;
            }
        }
        let turns = words.next()?.parse::<f32>().ok()?;
        (turns.abs() <= 1.0 && words.nth(1)?.eq_ignore_ascii_case("MAN_WRAP")).then_some(turns)
    })
}

fn mouse_sensitivity(settings: &str) -> Option<f32> {
    let mut controls = false;
    for line in settings.lines().map(str::trim) {
        if let Some(section) = line.strip_prefix('[').and_then(|line| line.split_once(']')) {
            controls = section.0.eq_ignore_ascii_case("CS");
        }
        if controls {
            if let Some((name, value)) = line.split_once('=') {
                if name.trim().eq_ignore_ascii_case("MOUSE_SENS") {
                    return Some(value.trim().parse::<i32>().ok()? as f32 * 0.01);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn horizontal_profile_reads_only_unmodified_body_turn() {
        let bindings =
            "+MOUSE SCAN_SHIFT SCAN_MOUSE_X 1 CICLS_UNKNOWN MCMD_ANGLE_Z 0.10 0 MAN_WRAP 0.0 0
MOUSE SCAN_NULL SCAN_MOUSE_Y 1 CICLS_UNKNOWN MCMD_ANGLE_Z 0.25 0 MAN_WRAP 0.0 0
MOUSE SCAN_NULL SCAN_MOUSE_X 1 CICLS_UNKNOWN MCMD_ANGLE_Z 0.20 0 MAN_WRAP 0.0 0 // body";
        assert_eq!(horizontal_binding(bindings), Some(0.20));
        let settings = "[VIDEO]\nMOUSE_SENS=100\n[cs]\n mouse_sens = 50";
        assert_eq!(mouse_sensitivity(settings), Some(0.5));
    }

    #[test]
    fn hero_mouse_matches_world3d_dispatch() {
        let mut look = HeroLook {
            turns: 0.15,
            sensitivity: 1.0,
            filtered_x: 0.0,
            phase: 0.5,
        };
        for (dx, expected) in [(100.0, 0.0855), (-100.0, -0.0855), (250.0, 0.15)] {
            look.reset();
            assert!((look.advance(dx, 0.0, TAU) / -TAU - expected).abs() < 1e-6);
        }
        look.reset();
        look.advance(100.0, 0.0, TAU);
        assert!((look.advance(0.0, 1.0, TAU) / -TAU - 0.004275).abs() < 1e-6);
        assert_eq!(look.filtered_x, 4.75);
        assert_eq!(look.advance(0.0, 0.0, TAU), 0.0);
        assert!(!look.has_state());
    }

    #[test]
    fn hero_finishes_a_capped_turn_on_the_next_tick_without_mouse_input() {
        let mut look = HeroLook {
            turns: 0.15,
            sensitivity: 1.0,
            filtered_x: 0.0,
            phase: 0.5,
        };
        let max_angle = 50.22849 * 0.01;
        assert!((look.advance(250.0, 0.0, max_angle) + 0.5022847).abs() < 1e-6);
        assert!((look.phase - 0.5700589).abs() < 1e-6);
        assert!((look.advance(0.0, 0.0, max_angle) + 0.44019294).abs() < 1e-6);
        assert!(!look.has_state());
    }
}
