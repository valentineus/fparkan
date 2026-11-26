use fparkan_path::{normalize_relative, PathPolicy};
use fparkan_vfs::Vfs;
use std::f32::consts::TAU;

#[derive(Clone, Copy, Debug)]
pub(super) struct HeroLook {
    yaw_turns: f32,
    pitch_blend: f32,
    reverse_y: bool,
    sensitivity: f32,
    filtered: [f32; 2],
    phase: f32,
}

impl HeroLook {
    pub(super) fn load(vfs: &dyn Vfs) -> Self {
        let read = |name: &[u8]| {
            let path = normalize_relative(name, PathPolicy::StrictLegacy).ok()?;
            vfs.read(&path).ok()
        };
        let table_bytes = read(b"hero.tbl");
        let settings_bytes = read(b"Iron_3D.ini");
        let table = String::from_utf8_lossy(table_bytes.as_deref().unwrap_or_default());
        let settings = String::from_utf8_lossy(settings_bytes.as_deref().unwrap_or_default());
        Self {
            yaw_turns: mouse_binding(
                &table,
                "SCAN_MOUSE_X",
                "CICLS_UNKNOWN",
                "MCMD_ANGLE_Z",
                "MAN_WRAP",
            )
            .unwrap_or(0.15),
            pitch_blend: mouse_binding(
                &table,
                "SCAN_MOUSE_Y",
                "CICLS_TURRET",
                "MCMD_ANGLE_Y",
                "MAN_NOTWRAP",
            )
            .unwrap_or(0.25),
            reverse_y: control_setting(&settings, "MOUSE_REV_Y").unwrap_or(0) != 0,
            sensitivity: control_setting(&settings, "MOUSE_SENS")
                .map_or(1.0, |value| value as f32 * 0.01),
            filtered: [0.0; 2],
            phase: 0.5,
        }
    }

    pub(super) fn reset(&mut self) {
        self.filtered = [0.0; 2];
        self.phase = 0.5;
    }

    pub(super) fn has_state(self) -> bool {
        self.filtered != [0.0; 2] || self.phase != 0.5
    }

    /// Returns delivered yaw in radians and the requested change in turret blend.
    pub(super) fn advance(&mut self, raw: [f32; 2], max_angle: f32) -> [f32; 2] {
        self.filtered = if raw == [0.0; 2] {
            [0.0; 2]
        } else {
            std::array::from_fn(|axis| {
                raw[axis].clamp(i32::MIN as f32, i32::MAX as f32)
                    * self.sensitivity
                    * [1.0, 1.2][axis]
                    * 0.95
                    + self.filtered[axis] * 0.05
            })
        };
        let [x, y] = self.filtered.map(|filtered| {
            let scaled = filtered * 0.006;
            if scaled.abs() > 0.999 {
                scaled.signum()
            } else {
                scaled
            }
        });
        if x != 0.0 {
            let phase = self.phase + x * self.yaw_turns;
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
        let pitch = y * self.pitch_blend;
        [delivered, if self.reverse_y { -pitch } else { pitch }]
    }
}

pub(super) fn turret_pitch(
    row: &fparkan_assets::PreparedControlTimelineRow,
    previous: f32,
    blend_delta: f32,
) -> Option<f32> {
    let neutral = row.initial_blend;
    let span = f32::from_bits(row.raw_parameters[3]);
    if row.flags & 4 != 0 || !span.is_finite() || span <= 0.0 || !(0.0..=1.0).contains(&neutral) {
        return None;
    }
    let target = (neutral + previous / span + blend_delta).clamp(0.0, 1.0);
    Some((target - neutral) * span)
}

fn mouse_binding(table: &str, axis: &str, class: &str, command: &str, wrap: &str) -> Option<f32> {
    table.lines().find_map(|line| {
        let line = line.split_once("//").map_or(line, |(binding, _)| binding);
        let mut words = line.split_ascii_whitespace();
        for name in ["MOUSE", "SCAN_NULL", axis, "1", class, command] {
            if !words.next()?.eq_ignore_ascii_case(name) {
                return None;
            }
        }
        let turns = words.next()?.parse::<f32>().ok()?;
        (turns.abs() <= 1.0 && words.nth(1)?.eq_ignore_ascii_case(wrap)).then_some(turns)
    })
}

fn control_setting(settings: &str, key: &str) -> Option<i32> {
    let mut controls = false;
    for line in settings.lines().map(str::trim) {
        if let Some(section) = line.strip_prefix('[').and_then(|line| line.split_once(']')) {
            controls = section.0.eq_ignore_ascii_case("CS");
        }
        if controls {
            if let Some((name, value)) = line.split_once('=') {
                if name.trim().eq_ignore_ascii_case(key) {
                    return value.trim().parse().ok();
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stock_look() -> HeroLook {
        HeroLook {
            yaw_turns: 0.15,
            pitch_blend: 0.25,
            reverse_y: true,
            sensitivity: 1.0,
            filtered: [0.0; 2],
            phase: 0.5,
        }
    }

    #[test]
    fn hero_profile_reads_body_yaw_and_turret_pitch_without_shift() {
        let bindings = "\
MOUSE SCAN_LSHIFT SCAN_MOUSE_X 1 CICLS_CAMERA MCMD_ANGLE_X 0.10 1 MAN_WRAP 0.0 0
MOUSE SCAN_LSHIFT SCAN_MOUSE_Y 1 CICLS_CAMERA MCMD_ANGLE_Y 0.15 1 MAN_NOTWRAP 0.0 0
MOUSE SCAN_NULL SCAN_MOUSE_Y 1 CICLS_UNKNOWN MCMD_ANGLE_Z 0.25 0 MAN_WRAP 0.0 0
MOUSE SCAN_NULL SCAN_MOUSE_X 1 CICLS_UNKNOWN MCMD_ANGLE_Z 0.20 0 MAN_WRAP 0.0 0 // body
MOUSE SCAN_NULL SCAN_MOUSE_Y 1 CICLS_TURRET MCMD_ANGLE_Y 0.30 1 MAN_NOTWRAP 0.0 0 // turret";
        assert_eq!(
            mouse_binding(
                bindings,
                "SCAN_MOUSE_X",
                "CICLS_UNKNOWN",
                "MCMD_ANGLE_Z",
                "MAN_WRAP"
            ),
            Some(0.20)
        );
        assert_eq!(
            mouse_binding(
                bindings,
                "SCAN_MOUSE_Y",
                "CICLS_TURRET",
                "MCMD_ANGLE_Y",
                "MAN_NOTWRAP"
            ),
            Some(0.30)
        );
        let settings = "[VIDEO]\nMOUSE_SENS=100\n[cs]\n mouse_sens = 50\n mouse_rev_y=1";
        assert_eq!(control_setting(settings, "MOUSE_SENS"), Some(50));
        assert_eq!(control_setting(settings, "MOUSE_REV_Y"), Some(1));
    }

    #[test]
    fn hero_mouse_matches_world3d_dispatch() {
        let mut look = stock_look();
        for (dx, expected) in [(100.0, 0.0855), (-100.0, -0.0855), (250.0, 0.15)] {
            look.reset();
            assert!((look.advance([dx, 0.0], TAU)[0] / -TAU - expected).abs() < 1e-6);
        }
        look.reset();
        look.advance([100.0, 0.0], TAU);
        assert!((look.advance([0.0, 1.0], TAU)[0] / -TAU - 0.004275).abs() < 1e-6);
        assert_eq!(look.filtered[0], 4.75);
        assert_eq!(look.advance([0.0; 2], TAU), [0.0; 2]);
        assert!(!look.has_state());
    }

    #[test]
    fn hero_finishes_a_capped_turn_on_the_next_tick_without_mouse_input() {
        let mut look = stock_look();
        let max_angle = 50.22849 * 0.01;
        assert!((look.advance([250.0, 0.0], max_angle)[0] + 0.5022847).abs() < 1e-6);
        assert!((look.phase - 0.5700589).abs() < 1e-6);
        assert!((look.advance([0.0; 2], max_angle)[0] + 0.44019294).abs() < 1e-6);
        assert!(!look.has_state());
    }

    #[test]
    fn hero_turret_uses_world3d_axis_and_control_elevation_endpoints() {
        let mut look = stock_look();
        let row = fparkan_assets::PreparedControlTimelineRow {
            row_index: 2,
            node_index: 34,
            frame_a: 55.0,
            frame_b: 57.0,
            initial_blend: 0.27272728,
            raw_parameters: [0, 0, 0, 1.9198622_f32.to_bits()],
            flags: 0,
        };
        let pitch = |previous, blend| turret_pitch(&row, previous, blend).unwrap().to_degrees();
        let upward = look.advance([0.0, -100.0], TAU)[1];
        assert!((upward - 0.171).abs() < 1e-6);
        assert!((pitch(0.0, upward) - 18.81).abs() < 1e-5);
        let tail = look.advance([1.0, 0.0], TAU)[1];
        assert!((tail - 0.00855).abs() < 1e-6);

        look.reset();
        let downward = look.advance([0.0, 300.0], TAU)[1];
        assert_eq!(downward, -0.25);
        assert!((pitch(-25.0_f32.to_radians(), downward) + 30.0).abs() < 1e-5);
        assert!((pitch(70.0_f32.to_radians(), upward) - 80.0).abs() < 1e-5);

        look.reset();
        look.reverse_y = false;
        assert!((look.advance([0.0, 100.0], TAU)[1] - upward).abs() < 1e-6);
    }
}
