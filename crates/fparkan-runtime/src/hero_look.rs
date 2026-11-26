use fparkan_path::{normalize_relative, PathPolicy};
use fparkan_vfs::Vfs;

#[derive(Clone, Copy, Debug)]
pub(super) struct HeroLook {
    turns: f32,
    sensitivity: f32,
    pub(super) filtered_x: f32,
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
        }
    }

    /// Returns the filter state and the requested change in world yaw.
    pub(super) fn sample(self, dx: f32, dy: f32) -> (f32, f32) {
        let filtered = if dx == 0.0 && dy == 0.0 {
            0.0
        } else {
            dx.clamp(i32::MIN as f32, i32::MAX as f32) * self.sensitivity * 0.95
                + self.filtered_x * 0.05
        };
        let scaled = filtered * 0.006;
        let axis = if scaled.abs() > 0.999 {
            scaled.signum()
        } else {
            scaled
        };
        (filtered, -(axis * self.turns) * std::f32::consts::TAU)
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
        (turns.is_finite() && words.nth(1)?.eq_ignore_ascii_case("MAN_WRAP")).then_some(turns)
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
        };
        for (dx, expected) in [(100.0, 0.0855), (-100.0, -0.0855), (250.0, 0.15)] {
            let (_, yaw) = look.sample(dx, 0.0);
            assert!((yaw / -std::f32::consts::TAU - expected).abs() < 1e-6);
        }
        look.filtered_x = look.sample(100.0, 0.0).0;
        let (tail, yaw) = look.sample(0.0, 1.0);
        assert_eq!(tail, 4.75);
        assert!((yaw / -std::f32::consts::TAU - 0.004275).abs() < 1e-6);
        assert_eq!(look.sample(0.0, 0.0), (0.0, 0.0));
    }
}
