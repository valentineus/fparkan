use super::{CapturedViewport, PreviewCameraFrame};
use fparkan_fx::atmosphere::{AtmosphereFrame, SunBirth};
use fparkan_render::{LegacyD3d7Projection, RawCameraTransform};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub(super) struct LegacyCameraCapture {
    pub(super) schema: String,
    pub(super) render_input_usable: Option<bool>,
    pub(super) selector0_words: [u32; 16],
    pub(super) parented_camera: Option<bool>,
    pub(super) effective_world_matrix_space: Option<String>,
    pub(super) effective_world_matrix_words: Option<[u32; 16]>,
    pub(super) viewport: [i32; 4],
    pub(super) near_plane: f32,
    pub(super) far_plane: f32,
    pub(super) field_of_view_radians: f32,
    pub(super) atmosphere_seconds: Option<f32>,
    pub(super) sky_uv_clock: Option<SkyUvClock>,
    pub(super) geometry_projection: Option<GeometryProjection>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub(super) struct SkyUvClock {
    pub(super) verified: bool,
    pub(super) elapsed_milliseconds: u32,
    pub(super) raw_clock: Option<u32>,
    pub(super) celestial_births: Option<CelestialBirthClocks>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub(super) struct CelestialBirthClocks {
    sun: Option<CelestialClock>,
    moon: Option<CelestialClock>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct CelestialClock {
    elapsed_milliseconds: u32,
    lifetime_milliseconds: u32,
}

impl CelestialBirthClocks {
    pub(super) fn apply(&self, atmosphere: &mut AtmosphereFrame) {
        for (sample, clock, angles, row) in [
            (&mut atmosphere.sun, &self.sun, [90.0, 30.0], 3),
            (&mut atmosphere.moon, &self.moon, [0.0, 50.0], 4),
        ] {
            sample.active = clock.is_some();
            sample.birth = clock.as_ref().map(|clock| SunBirth {
                start_seconds: atmosphere.real_seconds - clock.elapsed_milliseconds as f32 / 1000.0,
                lifetime_seconds: clock.lifetime_milliseconds as f32 / 1000.0,
                initial_angles_degrees: angles,
                material_row: row,
            });
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub(super) struct GeometryProjection {
    verified: bool,
    viewport: [i32; 4],
    near_plane: f32,
    far_plane: f32,
    field_of_view_radians: f32,
}

pub(super) fn selected_atmosphere_seconds(
    command_line: Option<f32>,
    capture: Option<&LegacyCameraCapture>,
) -> Option<f32> {
    command_line.or_else(|| capture.and_then(|capture| capture.atmosphere_seconds))
}

impl LegacyCameraCapture {
    pub(super) fn preview_matrix_words(&self) -> Result<[u32; 16], String> {
        match (
            self.effective_world_matrix_space.as_deref(),
            self.effective_world_matrix_words,
        ) {
            (Some("world"), Some(words)) => Ok(words),
            (Some(_), _) => Err(
                "legacy camera effective_world_matrix_space must be 'world'".to_string(),
            ),
            (None, Some(_)) => Err(
                "legacy camera effective_world_matrix_words requires an explicit world-space marker"
                    .to_string(),
            ),
            (None, None) if self.parented_camera == Some(true) => Err(
                "parented legacy camera requires an effective world-space matrix".to_string(),
            ),
            (None, None) => Ok(self.selector0_words),
        }
    }

    pub(super) fn captured_viewport(&self) -> Result<CapturedViewport, String> {
        let width = self.viewport[2]
            .checked_sub(self.viewport[0])
            .filter(|width| *width > 0)
            .ok_or_else(|| "legacy camera capture has an invalid viewport width".to_string())?;
        let height = self.viewport[3]
            .checked_sub(self.viewport[1])
            .filter(|height| *height > 0)
            .ok_or_else(|| "legacy camera capture has an invalid viewport height".to_string())?;
        Ok(CapturedViewport {
            rect: self.viewport,
            extent: [
                u32::try_from(width)
                    .map_err(|_| "legacy camera viewport width exceeds u32".to_string())?,
                u32::try_from(height)
                    .map_err(|_| "legacy camera viewport height exceeds u32".to_string())?,
            ],
        })
    }

    pub(super) fn preview_camera_frame(&self) -> Result<PreviewCameraFrame, String> {
        let projection = if let Some(geometry) = &self.geometry_projection {
            if !geometry.verified || geometry.viewport != self.viewport {
                return Err(
                    "legacy camera geometry projection is unverified or has a different viewport"
                        .to_string(),
                );
            }
            LegacyD3d7Projection {
                viewport: geometry.viewport,
                near_plane: geometry.near_plane,
                far_plane: geometry.far_plane,
                field_of_view_radians: geometry.field_of_view_radians,
            }
        } else {
            LegacyD3d7Projection {
                viewport: self.viewport,
                near_plane: self.near_plane,
                far_plane: self.far_plane,
                field_of_view_radians: self.field_of_view_radians,
            }
        };
        let frame = PreviewCameraFrame::from_legacy_d3d7(
            RawCameraTransform {
                words: self.preview_matrix_words()?,
            },
            projection,
        )
        .ok_or_else(|| "legacy camera capture contains an invalid D3D7 camera".to_string())?;
        self.captured_viewport()?;
        Ok(frame)
    }
}

pub(super) fn load_legacy_camera_capture(
    path: &std::path::Path,
) -> Result<LegacyCameraCapture, String> {
    let bytes = std::fs::read(path).map_err(|err| format!("{}: {err}", path.display()))?;
    let capture =
        parse_legacy_camera_capture(&bytes).map_err(|err| format!("{}: {err}", path.display()))?;
    if capture.geometry_projection.is_none() {
        eprintln!("render_coverage captured_projection_unverified=1 (capture has no geometry_projection; using the recorded camera boundary, which can clip nearby surfaces)");
    }
    if capture.sky_uv_clock.is_none() {
        eprintln!("render_coverage captured_sky_phase_unverified=1 (capture has no sky UV clock; using replay elapsed time)");
    }
    if capture
        .sky_uv_clock
        .as_ref()
        .and_then(|c| c.celestial_births.as_ref())
        .is_none()
    {
        eprintln!("render_coverage captured_celestial_clock_unverified=1 (capture has no celestial birth clocks; using nominal schedule births)");
    }
    Ok(capture)
}

pub(super) fn parse_legacy_camera_capture(bytes: &[u8]) -> Result<LegacyCameraCapture, String> {
    // Windows PowerShell 5.1 writes redirected text as UTF-16LE with a BOM.
    // `capture-original-camera.ps1` intentionally emits plain JSON, so accept
    // that normal hand-off format as well as UTF-8 without making the caller
    // re-encode a read-only capture file.
    let json = decode_legacy_camera_capture_json(bytes)?;
    let capture: LegacyCameraCapture =
        serde_json::from_str(&json).map_err(|err| format!("invalid legacy camera JSON: {err}"))?;
    if capture.schema != "fparkan-legacy-camera-v1" {
        return Err("unsupported legacy camera capture schema".to_string());
    }
    if capture.effective_world_matrix_space.is_some()
        != capture.effective_world_matrix_words.is_some()
    {
        return Err(
            "effective_world_matrix_space and effective_world_matrix_words must be supplied together"
                .to_string(),
        );
    }
    if capture.render_input_usable == Some(false) {
        return Err("legacy camera capture is marked render_input_usable=false".to_string());
    }
    if capture
        .sky_uv_clock
        .as_ref()
        .is_some_and(|clock| !clock.verified)
    {
        return Err("legacy camera sky UV clock is unverified".to_string());
    }
    if let Some(births) = capture
        .sky_uv_clock
        .as_ref()
        .and_then(|c| c.celestial_births.as_ref())
    {
        if capture.atmosphere_seconds.is_none() {
            return Err("legacy camera celestial clocks require an atmosphere phase".to_string());
        }
        for clock in [&births.sun, &births.moon].into_iter().flatten() {
            if clock.lifetime_milliseconds == 0 || clock.lifetime_milliseconds > i32::MAX as u32 {
                return Err(
                    "legacy camera celestial clock has an invalid lifetime or age".to_string(),
                );
            }
        }
    }
    if capture
        .atmosphere_seconds
        .is_some_and(|seconds| !seconds.is_finite() || seconds < 0.0)
    {
        return Err(
            "legacy camera capture atmosphere_seconds must be finite and non-negative".to_string(),
        );
    }
    capture.preview_camera_frame()?;
    Ok(capture)
}

fn decode_legacy_camera_capture_json(bytes: &[u8]) -> Result<String, String> {
    let decode_utf16 = |words: Vec<u16>| {
        String::from_utf16(&words)
            .map_err(|err| format!("invalid UTF-16 legacy camera JSON: {err}"))
    };
    match bytes {
        [0xff, 0xfe, rest @ ..] => {
            let chunks = rest.chunks_exact(2);
            if !chunks.remainder().is_empty() {
                return Err("invalid UTF-16LE legacy camera JSON length".to_string());
            }
            decode_utf16(
                chunks
                    .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
                    .collect(),
            )
        }
        [0xfe, 0xff, rest @ ..] => {
            let chunks = rest.chunks_exact(2);
            if !chunks.remainder().is_empty() {
                return Err("invalid UTF-16BE legacy camera JSON length".to_string());
            }
            decode_utf16(
                chunks
                    .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
                    .collect(),
            )
        }
        [0xef, 0xbb, 0xbf, rest @ ..] => std::str::from_utf8(rest)
            .map(str::to_owned)
            .map_err(|err| format!("invalid UTF-8 legacy camera JSON: {err}")),
        _ => std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|err| format!("invalid UTF-8 legacy camera JSON: {err}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json() -> serde_json::Value {
        let words = [
            1.0_f32, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ]
        .map(f32::to_bits);
        serde_json::json!({
            "schema": "fparkan-legacy-camera-v1", "selector0_words": words,
            "viewport": [0, 0, 1280, 1024], "near_plane": 5.0, "far_plane": 700.0,
            "field_of_view_radians": 1.3,
            "geometry_projection": {
                "verified": true, "viewport": [0, 0, 1280, 1024],
                "near_plane": 0.5, "far_plane": 700.0, "field_of_view_radians": 1.3,
            }
        })
    }

    #[test]
    fn celestial_capture_rejects_invalid_lifetime() {
        for lifetime in [0, i32::MAX as u32 + 1] {
            let mut json = json();
            json["atmosphere_seconds"] = serde_json::json!(395.471);
            json["sky_uv_clock"] = serde_json::json!({
                "verified": true, "elapsed_milliseconds": 123,
                "celestial_births": {"sun": {"elapsed_milliseconds": 1000,
                    "lifetime_milliseconds": lifetime}, "moon": null}
            });
            assert!(parse_legacy_camera_capture(&serde_json::to_vec(&json).unwrap()).is_err());
        }
    }

    #[test]
    fn celestial_capture_preserves_executed_sun_age_and_inactive_moon() {
        use fparkan_fx::{
            atmosphere::TypedAtmosphere,
            sky::{SkyFrame, SkyMaterials},
        };
        let mut json = json();
        json["atmosphere_seconds"] = serde_json::json!(395.471);
        json["sky_uv_clock"] = serde_json::json!({
            "verified": true, "elapsed_milliseconds": 123,
            "celestial_births": {"sun": {"elapsed_milliseconds": 378466,
                "lifetime_milliseconds": 525000}, "moon": null}
        });
        let capture = parse_legacy_camera_capture(&serde_json::to_vec(&json).unwrap()).unwrap();
        let mut missing_phase = json.clone();
        missing_phase
            .as_object_mut()
            .unwrap()
            .remove("atmosphere_seconds");
        assert!(parse_legacy_camera_capture(&serde_json::to_vec(&missing_phase).unwrap()).is_err());
        let births = capture.sky_uv_clock.unwrap().celestial_births.unwrap();
        let mut atmosphere = TypedAtmosphere {
            marker: 0,
            version: 0,
            tracks: Vec::new(),
            trailer: Default::default(),
        }
        .sample(395.471);
        births.apply(&mut atmosphere);
        let materials = SkyMaterials::default();
        let sky = SkyFrame::from_atmosphere(&atmosphere, &materials, atmosphere.real_seconds);
        // Executed GOG CSun state; its actual birth was at phase 17.005, not nominal 18.
        for (actual, native) in
            sky.sun
                .direction
                .into_iter()
                .zip([-0.33643198, -0.7397671, -0.5827162])
        {
            assert!((actual - native).abs() < 3e-6);
        }
        assert!(sky.sun.active);
        assert!(!sky.moon.active);
        assert_eq!(atmosphere.sun.birth.unwrap().lifetime_seconds, 525.0);
        assert!(atmosphere.moon.birth.is_none());
    }

    #[test]
    fn geometry_projection_preserves_surfaces_closer_than_restored_camera_near() {
        let mut json = json();
        let captured = parse_legacy_camera_capture(&serde_json::to_vec(&json).unwrap()).unwrap();
        let camera = captured.preview_camera_frame().unwrap();
        let point = [1.0, 0.0, 0.0, 1.0];
        let depth = |matrix: [f32; 16]| {
            (0..4)
                .map(|row| point[row] * matrix[row * 4 + 2])
                .sum::<f32>()
        };
        assert_eq!(camera.near_plane, 0.5);
        assert!(depth(camera.clip_from_world) > 0.0);
        json.as_object_mut().unwrap().remove("geometry_projection");
        let old = parse_legacy_camera_capture(&serde_json::to_vec(&json).unwrap()).unwrap();
        let old_camera = old.preview_camera_frame().unwrap();
        assert_eq!(old_camera.near_plane, 5.0);
        assert!(depth(old_camera.clip_from_world) < 0.0);
    }

    #[test]
    fn geometry_projection_survives_later_sky_depth_range() {
        let mut json = json();
        let world = parse_legacy_camera_capture(&serde_json::to_vec(&json).unwrap())
            .unwrap()
            .preview_camera_frame()
            .unwrap();
        json["near_plane"] = serde_json::json!(700.0);
        json["far_plane"] = serde_json::json!(50000.0);
        let after_sky = parse_legacy_camera_capture(&serde_json::to_vec(&json).unwrap())
            .unwrap()
            .preview_camera_frame()
            .unwrap();
        assert_eq!(after_sky.clip_from_world, world.clip_from_world);
        assert_eq!(after_sky.near_plane, 0.5);
    }

    #[test]
    fn geometry_projection_rejects_unverified_mismatched_or_invalid_input() {
        for (field, value) in [
            ("verified", serde_json::json!(false)),
            ("viewport", serde_json::json!([0, 0, 640, 480])),
            ("near_plane", serde_json::json!(0.0)),
            ("far_plane", serde_json::json!(0.25)),
        ] {
            let mut json = json();
            json["geometry_projection"][field] = value;
            assert!(
                parse_legacy_camera_capture(&serde_json::to_vec(&json).unwrap()).is_err(),
                "{field}"
            );
        }
    }
}
