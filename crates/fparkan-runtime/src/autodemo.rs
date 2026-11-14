use super::{
    component_cockpit, mission_object_handle, normalize_engine_path, read_vfs, Engine, MissionMode,
    StartingCockpit, WorldSnapshot,
};
use fparkan_path::{normalize_relative, NormalizedPath, PathPolicy};
use fparkan_prototype::decode_unit_dat;
use fparkan_render::{LegacyIron3dEulerTransform, RawCameraTransform};
use fparkan_terrain::{SurfaceQuery, TerrainWorld};
use fparkan_vfs::{Vfs, VfsError};
use std::sync::Arc;

const PITCH: f32 = 1.25;
const YAW: f32 = std::f32::consts::FRAC_PI_2;
const FOLLOW_DISTANCE: f32 = 30.0;
const PLANT_CLASS: u32 = 0x8000_0010;
pub(super) const MOBILE_UNIT: u32 = 0x0100_0000;

pub(super) fn read_config(
    vfs: &Arc<dyn Vfs>,
    mission_path: &str,
) -> Result<Arc<[u8]>, super::EngineError> {
    let cfg = mission_path.rsplit_once(['/', '\\']).map_or_else(
        || "mission.cfg".to_owned(),
        |(parent, _)| format!("{parent}/mission.cfg"),
    );
    read_vfs(vfs, &normalize_engine_path("AutoDemo config", &cfg)?)
}

pub(super) fn startup_reinforcements(
    vfs: &Arc<dyn Vfs>,
    mission: &super::MissionDocument,
    mission_path: &str,
) -> Result<Vec<(NormalizedPath, usize)>, super::EngineError> {
    let config = match read_config(vfs, mission_path) {
        Ok(config) => config,
        Err(super::EngineError::Vfs {
            source: VfsError::NotFound(_),
            ..
        }) => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let Some(minimum) = read_setting(&config, "MinBotsNum") else {
        return Ok(Vec::new());
    };
    if minimum == 0 {
        return Ok(Vec::new());
    }
    let classes = mission
        .objects
        .iter()
        .map(|object| {
            let raw = &object.resource_name.raw;
            let path = raw.split(|byte| *byte == 0).next().unwrap_or_default();
            if !path
                .get(path.len().saturating_sub(4)..)
                .is_some_and(|suffix| suffix.eq_ignore_ascii_case(b".dat"))
            {
                return Ok(None);
            }
            let path = normalize_relative(path, PathPolicy::StrictLegacy).map_err(|source| {
                super::EngineError::Path {
                    role: "AutoDemo Unit DAT",
                    value: String::from_utf8_lossy(path).into_owned(),
                    source,
                }
            })?;
            let bytes = read_vfs(vfs, &path)?;
            Ok(decode_unit_dat(&bytes).ok().map(|unit| {
                u32::from_le_bytes(
                    unit.header_opaque[4..8]
                        .try_into()
                        .expect("fixed Unit DAT header slice"),
                )
            }))
        })
        .collect::<Result<Vec<_>, super::EngineError>>()?;
    let prefix = normalize_engine_path("AutoDemo robot catalog", "units/units/autodemo")?;
    let deck = match vfs.list(&prefix) {
        Ok(deck) => deck,
        Err(VfsError::NotFound(_)) => return Ok(Vec::new()),
        Err(source) => {
            return Err(super::EngineError::Vfs {
                path: prefix.as_str().into(),
                source,
            })
        }
    };
    let mut deck: Vec<_> = deck
        .into_iter()
        .map(|entry| entry.path)
        .filter(|path| {
            path.as_str()
                .rsplit_once('/')
                .is_some_and(|(parent, name)| {
                    parent.eq_ignore_ascii_case(prefix.as_str())
                        && name
                            .rsplit_once('.')
                            .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("dat"))
                })
        })
        .collect();
    deck.sort_by_cached_key(|path| path.lookup_key().0);
    if deck.is_empty() {
        return Ok(Vec::new());
    }
    let mut resources = deck.iter().cycle();
    let mut reinforcements = Vec::new();
    // shortcut: run only the first replenishment pass until the scene supports spawning after load.
    for (clan, _) in mission.clans.iter().enumerate() {
        let clan = clan as u32;
        let mut members = mission
            .objects
            .iter()
            .zip(&classes)
            .enumerate()
            .filter(|(_, (object, _))| object.raw_after_resource == clan);
        let robots = members
            .clone()
            .filter(|(_, (_, class))| class.is_some_and(|class| class & MOBILE_UNIT != 0))
            .count();
        if robots >= minimum as usize {
            continue;
        }
        if let Some((plant_index, _)) =
            members.rfind(|(_, (_, class))| **class == Some(PLANT_CLASS))
        {
            let resource = resources.next().expect("non-empty repeating catalog");
            reinforcements.push((resource.clone(), plant_index));
        }
    }
    Ok(reinforcements)
}

pub(super) fn reinforcement_position(
    plant: &super::MissionObjectDraft,
    plant_index: usize,
    assets: &super::MissionAssets,
    terrain: &TerrainWorld,
    rng: &mut u32,
) -> Result<[f32; 3], super::EngineError> {
    let bounds = assets
        .visuals_for_object(plant_index)
        .first()
        .and_then(|id| assets.visual_by_id(*id))
        .and_then(|visual| assets.model_by_id(visual.model_id?))
        .map(|model| &model.validated)
        .ok_or_else(|| super::EngineError::Movement("missing AutoDemo plant bounds".into()))?;
    let place = LegacyIron3dEulerTransform {
        translation: plant.position,
        orientation_radians: plant.orientation_raw,
    };
    let [x, y, _] = place
        .try_transform_scaled_point(bounds.bounding_sphere_center, plant.scale)
        .ok_or_else(|| super::EngineError::Movement("invalid AutoDemo plant center".into()))?;
    // shortcut: the root MSH sphere supplies the plant bounds until the assembled collider is available.
    let scale = plant.scale.into_iter().map(f32::abs).fold(0.0, f32::max);
    let radius = bounds.bounding_sphere_radius * scale;
    // shortcut: isolated CRT state until AutoDemo shares the host's complete RNG call order.
    let angle = (super::next_crt_rand(rng) % 360) as f32;
    let (sin, cos) = angle.to_radians().sin_cos();
    let [x, y] = [x + sin * radius, y + cos * radius];
    let z = terrain
        .height_at([x, y])
        .map_err(super::EngineError::Terrain)?;
    Ok([x, y, z.unwrap_or(0.0) + 2.0])
}

/// View selected by the AutoDemo clock.
pub enum AutoDemoView {
    /// One shared exterior camera is used in both exterior phases.
    Exterior {
        /// Camera transform in world space.
        transform: RawCameraTransform,
        /// Whether the strategic interval has ended.
        bird: bool,
    },
    /// A cockpit point on the next live unit.
    Cockpit(StartingCockpit),
}

/// State of the automatic mission camera, sampled once per game frame.
pub struct AutoDemoCamera {
    durations_ms: [u32; 3],
    phase: usize,
    phase_start_ms: u32,
    position: [f32; 3],
    targets: Vec<StartingCockpit>,
    target_index: usize,
    last_clock_ms: Option<u32>,
}

/// Reads the AutoDemo camera schedule from the loaded mission's configuration.
/// Other mission modes do not have an automatic camera.
///
/// # Errors
/// Returns the reason why the camera could not be initialized.
pub fn autodemo_camera(engine: &Engine) -> Result<Option<AutoDemoCamera>, String> {
    let Some(mission) = engine
        .loaded
        .as_ref()
        .filter(|state| state.script_host_mode == MissionMode::AutoDemo)
    else {
        return Ok(None);
    };
    let vfs = engine.services.vfs.as_ref().ok_or("missing mission VFS")?;
    let config = read_config(vfs, &mission.summary.key).map_err(|err| err.to_string())?;
    let durations_ms =
        read_intervals(&config).ok_or("invalid autoplay_settings camera intervals")?;
    let clan = mission
        .mission
        .clans
        .first()
        .ok_or("missing AutoDemo clan")?;
    let [_, max] = mission
        .terrain
        .surface_bounds()
        .ok_or("missing AutoDemo terrain bounds")?;
    let position = [clan.anchor[0], clan.anchor[1], max[2] * 0.5];
    if !position.iter().all(|axis| axis.is_finite()) {
        return Err("non-finite AutoDemo camera origin".to_owned());
    }
    // shortcut: use initial clan-0 units with camera points until live ownership
    // and native eligibility are available for all units.
    let targets = mission
        .object_drafts
        .iter()
        .enumerate()
        .filter(|(_, draft)| draft.raw_after_resource == 0)
        .filter_map(|(index, draft)| {
            let handle = mission_object_handle(engine, draft.original_id?)?;
            component_cockpit(mission, index, handle)
        })
        .collect();
    Ok(Some(AutoDemoCamera {
        durations_ms,
        phase: 0,
        phase_start_ms: 0,
        position,
        targets,
        target_index: 0,
        last_clock_ms: None,
    }))
}

fn read_intervals(bytes: &[u8]) -> Option<[u32; 3]> {
    Some([
        read_setting(bytes, "StrategyModeTime")?.checked_mul(1_000)?,
        read_setting(bytes, "BirdModeTime")?.checked_mul(1_000)?,
        read_setting(bytes, "CockpitModeTime")?.checked_mul(1_000)?,
    ])
}

fn read_setting(bytes: &[u8], key: &str) -> Option<u32> {
    let source = String::from_utf8_lossy(bytes);
    let mut inside = false;
    let mut result = None;
    for line in source.lines() {
        let line = line.split('#').next()?.trim();
        let mut words = line.split_whitespace();
        match words.next() {
            Some(word) if word.eq_ignore_ascii_case("object") => {
                inside = words
                    .next()
                    .is_some_and(|name| name.eq_ignore_ascii_case("autoplay_settings"));
                continue;
            }
            Some(word) if word.eq_ignore_ascii_case("end") => {
                inside = false;
                continue;
            }
            _ => {}
        }
        if !inside {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case(key) {
            result = Some(value.trim().parse().ok()?);
        }
    }
    result
}

impl AutoDemoCamera {
    /// Advances at most one interval per sampled frame. Returns an exterior
    /// pose while waiting for a usable cockpit target.
    ///
    /// # Errors
    /// Returns an error if terrain sampling or camera placement fails.
    pub fn sample(
        &mut self,
        clock_ms: u32,
        snapshot: &WorldSnapshot,
        terrain: &TerrainWorld,
    ) -> Result<AutoDemoView, String> {
        self.sample_with_height(clock_ms, snapshot, |xy| {
            terrain.height_at(xy).map_err(|err| err.to_string())
        })
    }

    fn sample_with_height(
        &mut self,
        clock_ms: u32,
        snapshot: &WorldSnapshot,
        height_at: impl FnOnce([f32; 2]) -> Result<Option<f32>, String>,
    ) -> Result<AutoDemoView, String> {
        let delta = self
            .last_clock_ms
            .map_or(0, |last| clock_ms.saturating_sub(last));
        let first_frame = self.last_clock_ms.is_none();
        self.last_clock_ms = Some(clock_ms);
        if first_frame {
            self.phase_start_ms = clock_ms;
        }
        let target = |index: usize| {
            let cockpit = *self.targets.get(index)?;
            let (_, state) = snapshot
                .transforms
                .iter()
                .find(|(handle, _)| *handle == cockpit.handle)?;
            Some((cockpit, state.position.map(f32::from_bits)))
        };
        if target(self.target_index).is_none() {
            self.target_index = (0..self.targets.len())
                .find(|&index| target(index).is_some())
                .unwrap_or(0);
            self.phase = 0;
            self.phase_start_ms = clock_ms;
        } else if clock_ms.saturating_sub(self.phase_start_ms) > self.durations_ms[self.phase] {
            if self.phase != 0 {
                let next = (1..self.targets.len())
                    .map(|offset| (self.target_index + offset) % self.targets.len())
                    .find(|&index| target(index).is_some());
                if let Some(next) = next {
                    self.target_index = next;
                } else if self.phase == 1 {
                    self.phase = 2;
                }
            }
            self.phase = (self.phase + 1) % 3;
            self.phase_start_ms = clock_ms;
        }
        let selected = target(self.target_index);
        if let (2, Some((cockpit, _))) = (self.phase, selected) {
            return Ok(AutoDemoView::Cockpit(cockpit));
        }
        if let Some((_, target)) = selected {
            // shortcut: approach the native offset without the native per-axis
            // acceleration, selection flags, and terrain contact filters.
            let seconds = delta as f32 / 1000.0;
            let end = clock_ms.saturating_sub(self.phase_start_ms) as f32 / 1000.0;
            let ramp_integral = |time: f32| {
                let ramp = time.min(0.5);
                ramp * ramp + (time - ramp)
            };
            let motion = if self.phase == 0 {
                (ramp_integral(end) - ramp_integral((end - seconds).max(0.0))).max(0.0)
            } else {
                seconds
            };
            let desired = [
                target[0],
                target[1] - PITCH.sin() * FOLLOW_DISTANCE,
                target[2] + PITCH.cos() * FOLLOW_DISTANCE,
            ];
            for ((axis, desired), speed) in self
                .position
                .iter_mut()
                .zip(desired)
                .zip([80.0, 80.0, 40.0])
            {
                *axis += (desired - *axis).clamp(-speed * motion, speed * motion);
            }
        }
        let [x, y, z] = self.position;
        let surface = height_at([x, y])?;
        self.position[2] = z.max(surface.unwrap_or(0.0) + 2.5);
        let transform = LegacyIron3dEulerTransform {
            translation: self.position,
            orientation_radians: [0.0, if first_frame { 0.0 } else { YAW - PITCH }, YAW],
        }
        .try_row_major()
        .ok_or("invalid AutoDemo camera transform")?;
        Ok(AutoDemoView::Exterior {
            transform: RawCameraTransform {
                words: transform.map(f32::to_bits),
            },
            bird: self.phase == 1,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_world::{ObjectHandle, StateHash, Tick, TransformState};

    #[test]
    fn demo_uses_configured_intervals_and_changes_target_at_cockpit_cut() {
        let durations_ms = read_intervals(b"object other\nStrategyModeTime=6\nend\nobject autoplay_settings\n StrategyModeTime = 5 # seconds\nBirdModeTime=10\nCockpitModeTime=20\nend").unwrap();
        assert_eq!(durations_ms, [5_000, 10_000, 20_000]);
        let cockpit = |slot| StartingCockpit {
            handle: ObjectHandle {
                slot,
                generation: 1,
            },
            object_index: slot as usize,
            component_index: 0,
            node_index: 0,
            node_point: [0.0; 3],
        };
        let mut demo = AutoDemoCamera {
            durations_ms,
            phase: 0,
            phase_start_ms: 0,
            position: [5.0, 5.0, 10.0],
            targets: vec![cockpit(0), cockpit(1)],
            target_index: 0,
            last_clock_ms: None,
        };
        let mut snapshot = WorldSnapshot {
            tick: Tick(0),
            objects: vec![],
            events: vec![],
            hash: StateHash([0; 32]),
            transforms: vec![
                (cockpit(0).handle, TransformState::default()),
                (cockpit(1).handle, TransformState::default()),
            ],
        };
        let terrain = |_| Ok(Some(0.0));
        assert!(matches!(
            demo.sample_with_height(0, &snapshot, terrain).unwrap(),
            AutoDemoView::Exterior { bird: false, .. }
        ));
        assert!(matches!(
            demo.sample_with_height(5_000, &snapshot, terrain).unwrap(),
            AutoDemoView::Exterior { bird: false, .. }
        ));
        assert!(matches!(
            demo.sample_with_height(5_001, &snapshot, terrain).unwrap(),
            AutoDemoView::Exterior { bird: true, .. }
        ));
        assert!(matches!(
            demo.sample_with_height(15_001, &snapshot, terrain).unwrap(),
            AutoDemoView::Exterior { bird: true, .. }
        ));
        assert!(
            matches!(demo.sample_with_height(15_002, &snapshot, terrain).unwrap(), AutoDemoView::Cockpit(selected) if selected == cockpit(1))
        );
        snapshot.transforms.truncate(1);
        assert!(matches!(
            demo.sample_with_height(16_000, &snapshot, terrain).unwrap(),
            AutoDemoView::Exterior { bird: false, .. }
        ));
    }
}
