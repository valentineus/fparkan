//! Audio playback for a running mission.
//!
//! The game keeps this module deliberately small. Mission ambience is read
//! from the names in mission.cfg, samples are resolved through the same VFS
//! and resource archive formats as the rest of the game, and rodio owns the
//! output thread. A sample is decoded once to validate it before it is
//! queued, then its bytes are retained so a new source can be created when a
//! loop or one-shot starts.

use fparkan_fx::environment::{SoundAction, SoundEvent};
use fparkan_path::{normalize_relative, NormalizedPath, PathPolicy};
use fparkan_resource::{archive_path, resource_name, CachedResourceRepository, ResourceRepository};
use fparkan_vfs::Vfs;
use rodio::mixer::Mixer;
use rodio::source::{ChannelVolume, SeekError};
use rodio::{
    ChannelCount, Decoder, DeviceSinkBuilder, MixerDeviceSink, Player, SampleRate, Source,
};
use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const SWAV_TYPE: u32 = u32::from_le_bytes(*b"SWAV");
const DEFAULT_MUSIC_TRACK: u8 = 2;
const FIRST_MUSIC_TRACK: u8 = 2;
const LAST_MUSIC_TRACK: u8 = 10;
const EAR_HALF_WIDTH: f32 = 0.15;
const VALIDATION_SAMPLES: usize = 64;
const AMBIENT_NIGHT_LENGTH: f32 = 1.1;
const AMBIENT_MIN_DELAY_SECONDS: f32 = 10.0;
const AMBIENT_DELAY_VARIANTS: u32 = 10;

/// Position and orientation of the listener in world coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Listener {
    /// Listener position.
    pub position: [f32; 3],
    /// Direction in which the camera looks.
    pub forward: [f32; 3],
    /// Camera up direction.
    pub up: [f32; 3],
}

impl Default for Listener {
    fn default() -> Self {
        Self {
            position: [0.0; 3],
            forward: [0.0, 1.0, 0.0],
            up: [0.0, 0.0, 1.0],
        }
    }
}

/// Audio loading/playback failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AudioError {
    /// Mission path or archive path is not a valid relative resource path.
    InvalidPath { path: String, message: String },
    /// Mission configuration could not be read.
    MissionConfigRead { path: String, message: String },
    /// Mission configuration does not contain the required audio object.
    MissionConfig { path: String, message: String },
    /// A declared archive could not be opened.
    Archive { path: String, message: String },
    /// A declared archive entry is absent.
    MissingEntry { archive: String, name: String },
    /// A declared entry exists but contains no payload.
    EmptyEntry { archive: String, name: String },
    /// A payload is not the expected SWAV entry type.
    WrongEntryType {
        archive: String,
        name: String,
        type_id: Option<u32>,
    },
    /// The payload has a valid container but cannot be decoded by rodio.
    Decode { resource: String, message: String },
    /// A decoded container produced no samples.
    NoSamples { resource: String },
    /// A direct VFS file such as a music track is missing or unreadable.
    FileRead { path: String, message: String },
    /// Track selection is outside the names shipped by the original game.
    UnsupportedMusicTrack(u8),
    /// The operating system has no usable output device.
    DeviceUnavailable { message: String },
    /// A listener direction is non-finite or has zero length.
    InvalidListener { field: &'static str },
    /// A listener or emitter position is non-finite.
    InvalidPosition { field: &'static str },
}

impl std::fmt::Display for AudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPath { path, message } => {
                write!(f, "invalid audio path {path}: {message}")
            }
            Self::MissionConfigRead { path, message } => {
                write!(f, "failed to read mission audio config {path}: {message}")
            }
            Self::MissionConfig { path, message } => {
                write!(f, "invalid mission audio config {path}: {message}")
            }
            Self::Archive { path, message } => {
                write!(f, "failed to open audio archive {path}: {message}")
            }
            Self::MissingEntry { archive, name } => {
                write!(
                    f,
                    "declared audio resource {name} is missing from {archive}"
                )
            }
            Self::EmptyEntry { archive, name } => {
                write!(f, "declared audio resource {name} in {archive} is empty")
            }
            Self::WrongEntryType {
                archive,
                name,
                type_id,
            } => write!(
                f,
                "audio resource {name} in {archive} has type {type_id:?}, expected SWAV"
            ),
            Self::Decode { resource, message } => {
                write!(f, "failed to decode audio resource {resource}: {message}")
            }
            Self::NoSamples { resource } => {
                write!(f, "audio resource {resource} contains no samples")
            }
            Self::FileRead { path, message } => {
                write!(f, "failed to read audio file {path}: {message}")
            }
            Self::UnsupportedMusicTrack(track) => {
                write!(
                    f,
                    "music track Track{track:02}.ogg is outside Track02..Track10"
                )
            }
            Self::DeviceUnavailable { message } => {
                write!(f, "audio output device is unavailable: {message}")
            }
            Self::InvalidListener { field } => {
                write!(
                    f,
                    "audio listener direction {field} must be finite and non-zero"
                )
            }
            Self::InvalidPosition { field } => {
                write!(f, "audio {field} position must be finite")
            }
        }
    }
}

impl std::error::Error for AudioError {}

/// Parsed names from the two mission audio objects.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MissionAudioConfig {
    /// Archive named by ambient_music_loop.
    pub library: String,
    /// Archive named by ambient_music_variation, or the loop archive when it
    /// is omitted.
    pub variation_library: String,
    /// Theme loop name from THEME.
    pub theme: String,
    /// Ambient default variation names in numeric key order.
    pub default_variations: Vec<String>,
    /// Ambient day variation names in numeric key order.
    pub day_variations: Vec<String>,
    /// Ambient night variation names in numeric key order.
    pub night_variations: Vec<String>,
}

#[derive(Clone, Debug)]
struct SoundAsset {
    archive: String,
    name: String,
    bytes: Arc<[u8]>,
}

impl SoundAsset {
    fn resource_name(&self) -> String {
        format!("{}:{}", self.archive, self.name)
    }
}

struct AudioLibrary {
    vfs: Arc<dyn Vfs>,
    repository: Arc<CachedResourceRepository>,
    theme: SoundAsset,
    default_variations: Vec<SoundAsset>,
    day_variations: Vec<SoundAsset>,
    night_variations: Vec<SoundAsset>,
    music: BTreeMap<u8, Arc<[u8]>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VariationPool {
    Default,
    Day,
    Night,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct VariationSelection {
    pool: VariationPool,
    index: Option<usize>,
}

/// The two 16-bit states used by the original ambient variation selector.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct VariationRng {
    a: u16,
    b: u16,
}

impl VariationRng {
    fn from_clock(clock: u32) -> Self {
        Self {
            a: clock as u16,
            b: (clock >> 16) as u16,
        }
    }

    fn step(&mut self) {
        self.a = self.a.wrapping_shl(1) ^ self.b;
        self.b = self.b.wrapping_shr(1) ^ self.a;
    }

    fn choose(&mut self, length: usize, last_index: &mut i32) -> Option<usize> {
        if length == 0 {
            *last_index = -1;
            return None;
        }
        if length == 1 {
            *last_index = 0;
            return Some(0);
        }
        for _ in 0..=u16::MAX {
            self.step();
            let candidate = usize::from(self.b) % length;
            if candidate as i32 != *last_index {
                *last_index = candidate as i32;
                return Some(candidate);
            }
        }
        // A zero state is not produced by the native clock seed, but keeping
        // this fallback makes malformed or deterministic test seeds harmless.
        let candidate = usize::from(self.b) % length;
        *last_index = candidate as i32;
        Some(candidate)
    }
}

/// CRT-style delay generator kept separate from the ambient pool selector.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CrtRand {
    state: u32,
}

impl CrtRand {
    fn new(state: u32) -> Self {
        Self { state }
    }

    fn next_u15(&mut self) -> u32 {
        self.state = self.state.wrapping_mul(214_013).wrapping_add(2_531_011);
        (self.state >> 16) & 0x7fff
    }

    fn next_delay_seconds(&mut self) -> f32 {
        AMBIENT_MIN_DELAY_SECONDS + (self.next_u15() % AMBIENT_DELAY_VARIANTS) as f32
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct AmbienceScheduler {
    elapsed_seconds: f32,
    next_delay_seconds: f32,
    pool: VariationPool,
    variation_rng: VariationRng,
    delay_rng: CrtRand,
    last_index: i32,
}

impl AmbienceScheduler {
    fn new(clock_seed: u32, delay_seed: u32) -> Self {
        Self {
            elapsed_seconds: 0.0,
            next_delay_seconds: 0.0,
            pool: VariationPool::Default,
            variation_rng: VariationRng::from_clock(clock_seed),
            delay_rng: CrtRand::new(delay_seed),
            last_index: -1,
        }
    }

    fn update(
        &mut self,
        dt_seconds: f32,
        primary_rgb: [f32; 3],
        default_len: usize,
        day_len: usize,
        night_len: usize,
    ) -> Option<VariationSelection> {
        self.pool = selected_pool(primary_rgb, day_len, night_len);
        self.elapsed_seconds += finite_non_negative(dt_seconds);
        if self.elapsed_seconds <= self.next_delay_seconds {
            return None;
        }
        self.elapsed_seconds = 0.0;
        let length = match self.pool {
            VariationPool::Default => default_len,
            VariationPool::Day => day_len,
            VariationPool::Night => night_len,
        };
        let index = self.variation_rng.choose(length, &mut self.last_index);
        self.next_delay_seconds = self.delay_rng.next_delay_seconds();
        Some(VariationSelection {
            pool: self.pool,
            index,
        })
    }
}

fn finite_non_negative(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

fn random_seed() -> u32 {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (duration.as_nanos() as u64 as u32) ^ duration.subsec_nanos().rotate_left(13)
}

fn selected_pool(primary_rgb: [f32; 3], day_len: usize, night_len: usize) -> VariationPool {
    if day_len == 0 && night_len == 0 {
        return VariationPool::Default;
    }
    let length = primary_rgb
        .iter()
        .map(|value| value * value)
        .sum::<f32>()
        .sqrt();
    if length.is_finite() && length <= AMBIENT_NIGHT_LENGTH {
        VariationPool::Night
    } else {
        VariationPool::Day
    }
}

struct SpatialOneShot {
    player: NativeSpatialPlayer,
    volume: f32,
}

#[derive(Clone, Copy, Debug)]
struct NativeSpatialState {
    emitter_position: [f32; 3],
    left_ear: [f32; 3],
    right_ear: [f32; 3],
    volume: f32,
    min_distance: f32,
    max_distance: f32,
}

/// A spatial player with the native DirectSound distance curve.
///
/// `rodio::SpatialPlayer` applies an inverse-square curve unconditionally.
/// Native FX sounds instead use the DirectSound min/max distance curve, so
/// this small wrapper keeps rodio's mono and stereo panning behavior while
/// supplying the native per-channel gains itself.
struct NativeSpatialPlayer {
    player: Player,
    state: Arc<Mutex<NativeSpatialState>>,
}

impl NativeSpatialPlayer {
    fn connect_new(
        mixer: &Mixer,
        emitter_position: [f32; 3],
        left_ear: [f32; 3],
        right_ear: [f32; 3],
    ) -> Self {
        Self {
            player: Player::connect_new(mixer),
            state: Arc::new(Mutex::new(NativeSpatialState {
                emitter_position,
                left_ear,
                right_ear,
                volume: 1.0,
                min_distance: 0.0,
                max_distance: f32::INFINITY,
            })),
        }
    }

    fn set_emitter_position(&self, position: [f32; 3]) {
        self.state
            .lock()
            .expect("audio state poisoned")
            .emitter_position = position;
    }

    fn set_left_ear_position(&self, position: [f32; 3]) {
        self.state.lock().expect("audio state poisoned").left_ear = position;
    }

    fn set_right_ear_position(&self, position: [f32; 3]) {
        self.state.lock().expect("audio state poisoned").right_ear = position;
    }

    fn set_sound_parameters(&self, min_distance: f32, max_distance: f32) {
        let mut state = self.state.lock().expect("audio state poisoned");
        state.min_distance = min_distance;
        state.max_distance = max_distance;
    }

    fn append<S>(&self, source: S)
    where
        S: Source + Send + 'static,
    {
        self.player
            .append(NativeSpatialSource::new(source, Arc::clone(&self.state)));
    }

    #[cfg(test)]
    fn volume(&self) -> f32 {
        self.state.lock().expect("audio state poisoned").volume
    }

    fn set_volume(&self, volume: f32) {
        self.state.lock().expect("audio state poisoned").volume = volume;
    }

    fn set_speed(&self, speed: f32) {
        self.player.set_speed(speed);
    }

    fn play(&self) {
        self.player.play();
    }

    fn pause(&self) {
        self.player.pause();
    }

    #[cfg(test)]
    fn is_paused(&self) -> bool {
        self.player.is_paused()
    }

    fn clear(&self) {
        self.player.clear();
    }

    fn stop(&self) {
        self.player.stop();
    }

    fn empty(&self) -> bool {
        self.player.empty()
    }
}

struct NativeSpatialSource<I>
where
    I: Source,
{
    input: ChannelVolume<I>,
    state: Arc<Mutex<NativeSpatialState>>,
    channel_gains: [f32; 2],
    refresh_remaining: usize,
}

const NATIVE_SPATIAL_REFRESH_SAMPLES: usize = 256;

impl<I> NativeSpatialSource<I>
where
    I: Source,
{
    fn new(input: I, state: Arc<Mutex<NativeSpatialState>>) -> Self {
        Self {
            input: ChannelVolume::new(input, vec![0.0, 0.0]),
            state,
            channel_gains: [0.0, 0.0],
            refresh_remaining: 0,
        }
    }
}

impl<I> Iterator for NativeSpatialSource<I>
where
    I: Source,
{
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.refresh_remaining == 0 {
            if let Ok(state) = self.state.lock() {
                self.channel_gains = native_spatial_channel_gains(*state);
            }
            self.refresh_remaining = NATIVE_SPATIAL_REFRESH_SAMPLES;
        }
        self.refresh_remaining -= 1;
        self.input.set_volume(0, self.channel_gains[0]);
        self.input.set_volume(1, self.channel_gains[1]);
        self.input.next()
    }
}

impl<I> Source for NativeSpatialSource<I>
where
    I: Source,
{
    fn current_span_len(&self) -> Option<usize> {
        self.input.current_span_len()
    }

    fn channels(&self) -> ChannelCount {
        self.input.channels()
    }

    fn sample_rate(&self) -> SampleRate {
        self.input.sample_rate()
    }

    fn total_duration(&self) -> Option<std::time::Duration> {
        self.input.total_duration()
    }

    fn try_seek(&mut self, position: std::time::Duration) -> Result<(), SeekError> {
        self.input.try_seek(position)
    }
}

impl AudioLibrary {
    fn load(vfs: Arc<dyn Vfs>, mission_cfg: &NormalizedPath) -> Result<Self, AudioError> {
        let repository = Arc::new(CachedResourceRepository::new(Arc::clone(&vfs)));
        let cfg_bytes = vfs
            .read(mission_cfg)
            .map_err(|source| AudioError::MissionConfigRead {
                path: mission_cfg.as_str().to_string(),
                message: source.to_string(),
            })?;
        let config = parse_mission_audio_config(&cfg_bytes).map_err(|message| {
            AudioError::MissionConfig {
                path: mission_cfg.as_str().to_string(),
                message,
            }
        })?;
        let archive =
            archive_path(config.library.as_bytes()).map_err(|source| AudioError::InvalidPath {
                path: config.library.clone(),
                message: source.to_string(),
            })?;
        let variation_archive =
            archive_path(config.variation_library.as_bytes()).map_err(|source| {
                AudioError::InvalidPath {
                    path: config.variation_library.clone(),
                    message: source.to_string(),
                }
            })?;

        let theme = load_swav(&repository, &archive, &config.theme)?;
        let default_variations = config
            .default_variations
            .iter()
            .map(|name| load_swav(&repository, &variation_archive, name))
            .collect::<Result<Vec<_>, _>>()?;
        let day_variations = config
            .day_variations
            .iter()
            .map(|name| load_swav(&repository, &variation_archive, name))
            .collect::<Result<Vec<_>, _>>()?;
        let night_variations = config
            .night_variations
            .iter()
            .map(|name| load_swav(&repository, &variation_archive, name))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            vfs,
            repository,
            theme,
            default_variations,
            day_variations,
            night_variations,
            music: BTreeMap::new(),
        })
    }

    fn load_fx_sound(&self, archive: &str, name: &str) -> Result<SoundAsset, AudioError> {
        let archive =
            archive_path(archive.as_bytes()).map_err(|source| AudioError::InvalidPath {
                path: archive.to_string(),
                message: source.to_string(),
            })?;
        load_swav(&self.repository, &archive, name)
    }

    fn music_bytes(&mut self, track: u8) -> Result<Arc<[u8]>, AudioError> {
        let path = music_path(track)?;
        if let Some(bytes) = self.music.get(&track) {
            return Ok(Arc::clone(bytes));
        }
        let bytes = self
            .vfs
            .read(&path)
            .map_err(|source| AudioError::FileRead {
                path: path.as_str().to_string(),
                message: source.to_string(),
            })?;
        let bytes = validate_audio_bytes(path.as_str(), &bytes)?;
        self.music.insert(track, Arc::clone(&bytes));
        Ok(bytes)
    }
}

/// Runtime audio state for one loaded mission.
pub(crate) struct GameAudio {
    stream: MixerDeviceSink,
    ambient: Player,
    music: Player,
    variations: Player,
    weather_loop: NativeSpatialPlayer,
    one_shots: Vec<SpatialOneShot>,
    library: AudioLibrary,
    ambience: AmbienceScheduler,
    listener: Listener,
    paused: bool,
    dynamic_sounds: BTreeMap<String, SoundAsset>,
    weather_volume: f32,
    weather_position: [f32; 3],
}

impl GameAudio {
    /// Opens and validates mission audio using the caller's VFS.
    ///
    /// mission is the same relative path passed to the runtime, usually
    /// MISSIONS/Autodemo.00/data.tma. Its neighbouring mission.cfg supplies
    /// the ambient names. Only the declared samples and selected music tracks
    /// are read.
    ///
    /// Returns a resource/configuration error for missing or malformed audio,
    /// or DeviceUnavailable when no output device can be opened. The caller
    /// may report the latter and continue without audio.
    pub(crate) fn new(vfs: Arc<dyn Vfs>, mission: &str) -> Result<Self, AudioError> {
        let mission_cfg = mission_cfg_path(mission)?;
        let library = AudioLibrary::load(vfs, &mission_cfg)?;
        let mut stream = DeviceSinkBuilder::open_default_sink().map_err(|source| {
            AudioError::DeviceUnavailable {
                message: source.to_string(),
            }
        })?;
        stream.log_on_drop(false);
        let ambient = Player::connect_new(stream.mixer());
        let music = Player::connect_new(stream.mixer());
        let variations = Player::connect_new(stream.mixer());
        let listener = Listener::default();
        let (left, right) = ear_positions(listener)?;
        let weather_loop =
            NativeSpatialPlayer::connect_new(stream.mixer(), listener.position, left, right);
        let mut audio = Self {
            stream,
            ambient,
            music,
            variations,
            weather_loop,
            one_shots: Vec::new(),
            library,
            ambience: AmbienceScheduler::new(random_seed(), random_seed()),
            listener,
            paused: false,
            dynamic_sounds: BTreeMap::new(),
            weather_volume: 1.0,
            weather_position: listener.position,
        };
        audio.start_theme()?;
        audio.play_music_track(DEFAULT_MUSIC_TRACK)?;
        Ok(audio)
    }

    /// Updates rodio's left/right ears from the current free camera.
    pub(crate) fn update_listener(
        &mut self,
        position: [f32; 3],
        forward: [f32; 3],
        up: [f32; 3],
    ) -> Result<(), AudioError> {
        let listener = Listener {
            position,
            forward,
            up,
        };
        let (left, right) = ear_positions(listener)?;
        self.listener = listener;
        self.weather_loop.set_left_ear_position(left);
        self.weather_loop.set_right_ear_position(right);
        self.weather_loop
            .set_emitter_position(self.weather_position);
        self.retain_active_one_shots();
        for one_shot in &self.one_shots {
            one_shot.player.set_left_ear_position(left);
            one_shot.player.set_right_ear_position(right);
            one_shot.player.set_volume(one_shot.volume);
        }
        self.refresh_weather_volume();
        Ok(())
    }

    /// Advances the native ambient variation timer and starts a selected
    /// sample when its strict elapsed-time boundary is crossed.
    ///
    /// `dt_seconds` is the non-negative delta for this frame, not an
    /// absolute mission clock.
    ///
    /// `primary_rgb` is the first active celestial light's base color before
    /// any camera-dependent sun glare is applied. The original atmosphere
    /// treats a color length at or below 1.1 as night.
    pub(crate) fn update_ambience(
        &mut self,
        dt_seconds: f32,
        primary_rgb: [f32; 3],
    ) -> Result<(), AudioError> {
        if self.paused {
            return Ok(());
        }
        let selection = self.ambience.update(
            dt_seconds,
            primary_rgb,
            self.library.default_variations.len(),
            self.library.day_variations.len(),
            self.library.night_variations.len(),
        );
        let Some(selection) = selection else {
            return Ok(());
        };
        let Some(index) = selection.index else {
            return Ok(());
        };
        let asset = match selection.pool {
            VariationPool::Default => self.library.default_variations.get(index),
            VariationPool::Day => self.library.day_variations.get(index),
            VariationPool::Night => self.library.night_variations.get(index),
        }
        .cloned()
        .ok_or_else(|| AudioError::NoSamples {
            resource: format!("ambient variation {:?}[{index}]", selection.pool),
        })?;
        self.variations.clear();
        self.variations.append(decoder_for(&asset)?);
        if self.paused {
            self.variations.pause();
        } else {
            self.variations.play();
        }
        Ok(())
    }

    /// Applies one sound event emitted by the environment simulation.
    ///
    /// Resource names are resolved lazily from the archive named by the event;
    /// this keeps mission `sky.ske`/FX references data-driven and avoids
    /// requiring every possible weather sample during mission startup.
    pub(crate) fn handle_sound_event(&mut self, event: &SoundEvent) -> Result<(), AudioError> {
        validate_point(event.position, "emitter")?;
        let volume = if event.volume.is_finite() {
            event.volume.max(0.0)
        } else {
            0.0
        };
        let min_distance = finite_non_negative(event.min_distance);
        let max_distance = if event.max_distance.is_finite() {
            event.max_distance.max(0.0)
        } else {
            f32::INFINITY
        };
        let frequency_ratio = if event.frequency_ratio.is_finite() && event.frequency_ratio > 0.0 {
            event.frequency_ratio
        } else {
            1.0
        };
        match event.action {
            SoundAction::StartLoop => {
                let asset = self.dynamic_sound(&event.archive, &event.name)?;
                self.weather_loop.clear();
                self.weather_loop.set_emitter_position(event.position);
                self.weather_position = event.position;
                self.weather_volume = volume;
                self.weather_loop
                    .set_sound_parameters(min_distance, max_distance);
                self.weather_loop.set_volume(volume);
                self.weather_loop.set_speed(frequency_ratio);
                self.weather_loop
                    .append(decoder_for(&asset)?.repeat_infinite());
                if self.paused {
                    self.weather_loop.pause();
                } else {
                    self.weather_loop.play();
                }
            }
            SoundAction::SetLoopVolume => {
                self.weather_loop.set_emitter_position(event.position);
                self.weather_position = event.position;
                self.weather_volume = volume;
                self.weather_loop
                    .set_sound_parameters(min_distance, max_distance);
                self.weather_loop.set_volume(volume);
                self.weather_loop.set_speed(frequency_ratio);
            }
            SoundAction::StopLoop => {
                self.weather_loop.clear();
                self.weather_loop.set_volume(1.0);
                self.weather_volume = 1.0;
                self.weather_position = self.listener.position;
                self.weather_loop.set_sound_parameters(0.0, f32::INFINITY);
                self.weather_loop.set_speed(1.0);
            }
            SoundAction::OneShot => {
                self.retain_active_one_shots();
                let asset = self.dynamic_sound(&event.archive, &event.name)?;
                self.one_shots.push(self.new_spatial_one_shot(
                    &asset,
                    event.position,
                    volume,
                    min_distance,
                    max_distance,
                    frequency_ratio,
                )?);
            }
        }
        Ok(())
    }

    /// Selects one of the original MUSIC/Track02..10.ogg files and loops it.
    pub(crate) fn play_music_track(&mut self, track: u8) -> Result<(), AudioError> {
        let bytes = self.library.music_bytes(track)?;
        self.music.clear();
        self.music.append(
            decoder_for_bytes(&format!("MUSIC/Track{track:02}.ogg"), &bytes)?.repeat_infinite(),
        );
        if self.paused {
            self.music.pause();
        } else {
            self.music.play();
        }
        Ok(())
    }

    /// Pauses or resumes every live audio source without changing selection.
    pub(crate) fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
        for player in [&self.ambient, &self.music, &self.variations] {
            if paused {
                player.pause();
            } else {
                player.play();
            }
        }
        if paused {
            self.weather_loop.pause();
        } else {
            self.weather_loop.play();
        }
        for one_shot in &self.one_shots {
            if paused {
                one_shot.player.pause();
            } else {
                one_shot.player.play();
            }
        }
    }

    /// Focus callback used by the window event loop.
    pub(crate) fn on_focus_changed(&mut self, focused: bool) {
        self.set_paused(!focused);
    }

    /// Stops all sources before the window/event loop exits.
    pub(crate) fn shutdown(&mut self) {
        self.ambient.stop();
        self.music.stop();
        self.variations.stop();
        self.weather_loop.stop();
        for one_shot in &self.one_shots {
            one_shot.player.stop();
        }
    }

    fn start_theme(&self) -> Result<(), AudioError> {
        self.ambient
            .append(decoder_for(&self.library.theme)?.repeat_infinite());
        if self.paused {
            self.ambient.pause();
        } else {
            self.ambient.play();
        }
        Ok(())
    }

    fn new_spatial_one_shot(
        &self,
        asset: &SoundAsset,
        emitter: [f32; 3],
        volume: f32,
        min_distance: f32,
        max_distance: f32,
        frequency_ratio: f32,
    ) -> Result<SpatialOneShot, AudioError> {
        validate_point(emitter, "emitter")?;
        let (left, right) = ear_positions(self.listener)?;
        let player = NativeSpatialPlayer::connect_new(self.stream.mixer(), emitter, left, right);
        player.set_sound_parameters(min_distance, max_distance);
        player.set_volume(volume);
        player.set_speed(frequency_ratio);
        player.append(decoder_for(asset)?);
        if self.paused {
            player.pause();
        } else {
            player.play();
        }
        Ok(SpatialOneShot { player, volume })
    }

    fn refresh_weather_volume(&self) {
        self.weather_loop.set_volume(self.weather_volume);
    }

    fn dynamic_sound(&mut self, archive: &str, name: &str) -> Result<SoundAsset, AudioError> {
        let archive = if archive.is_empty() {
            self.library.theme.archive.as_str()
        } else {
            archive
        };
        let key = format!(
            "{}:{}",
            archive.to_ascii_lowercase(),
            name.to_ascii_lowercase()
        );
        if let Some(asset) = self.dynamic_sounds.get(&key) {
            return Ok(asset.clone());
        }
        let asset = self.library.load_fx_sound(archive, name)?;
        self.dynamic_sounds.insert(key, asset.clone());
        Ok(asset)
    }

    fn retain_active_one_shots(&mut self) {
        self.one_shots.retain(|one_shot| !one_shot.player.empty());
    }
}

impl Drop for GameAudio {
    fn drop(&mut self) {
        self.shutdown();
    }
}

type AudioDecoder = Decoder<Cursor<Arc<[u8]>>>;

fn decoder_for(asset: &SoundAsset) -> Result<AudioDecoder, AudioError> {
    decoder_for_bytes(&asset.resource_name(), &asset.bytes)
}

fn decoder_for_bytes(resource: &str, bytes: &Arc<[u8]>) -> Result<AudioDecoder, AudioError> {
    Decoder::try_from(Cursor::new(Arc::clone(bytes))).map_err(|source| AudioError::Decode {
        resource: resource.to_string(),
        message: source.to_string(),
    })
}

fn validate_audio_bytes(resource: &str, bytes: &Arc<[u8]>) -> Result<Arc<[u8]>, AudioError> {
    if bytes.is_empty() {
        return Err(AudioError::EmptyEntry {
            archive: "VFS".to_string(),
            name: resource.to_string(),
        });
    }
    let decoder = decoder_for_bytes(resource, bytes)?;
    if decoder.take(VALIDATION_SAMPLES).count() == 0 {
        return Err(AudioError::NoSamples {
            resource: resource.to_string(),
        });
    }
    Ok(Arc::clone(bytes))
}

fn load_swav(
    repository: &CachedResourceRepository,
    archive: &NormalizedPath,
    name: &str,
) -> Result<SoundAsset, AudioError> {
    let display_archive = archive.as_str().to_string();
    let resource = resource_name(name.as_bytes());
    let archive_id = repository
        .open_archive_unchanged(archive)
        .map_err(|source| AudioError::Archive {
            path: display_archive.clone(),
            message: source.to_string(),
        })?;
    let entry = repository
        .find(archive_id, &resource)
        .map_err(|source| AudioError::Archive {
            path: display_archive.clone(),
            message: source.to_string(),
        })?
        .ok_or_else(|| AudioError::MissingEntry {
            archive: display_archive.clone(),
            name: name.to_string(),
        })?;
    let info = repository
        .entry_info(entry)
        .map_err(|source| AudioError::Archive {
            path: display_archive.clone(),
            message: source.to_string(),
        })?;
    if info.key.type_id != Some(SWAV_TYPE) {
        return Err(AudioError::WrongEntryType {
            archive: display_archive,
            name: name.to_string(),
            type_id: info.key.type_id,
        });
    }
    let bytes = repository
        .read(entry)
        .map_err(|source| AudioError::Archive {
            path: archive.as_str().to_string(),
            message: source.to_string(),
        })?;
    if bytes.is_empty() {
        return Err(AudioError::EmptyEntry {
            archive: archive.as_str().to_string(),
            name: name.to_string(),
        });
    }
    let bytes = Arc::<[u8]>::from(bytes.as_slice());
    let bytes = validate_audio_bytes(&format!("{}:{name}", archive.as_str()), &bytes)?;
    Ok(SoundAsset {
        archive: archive.as_str().to_string(),
        name: name.to_string(),
        bytes,
    })
}

fn music_path(track: u8) -> Result<NormalizedPath, AudioError> {
    if !(FIRST_MUSIC_TRACK..=LAST_MUSIC_TRACK).contains(&track) {
        return Err(AudioError::UnsupportedMusicTrack(track));
    }
    let display = format!("MUSIC/Track{track:02}.ogg");
    normalize_relative(display.as_bytes(), PathPolicy::StrictLegacy).map_err(|source| {
        AudioError::InvalidPath {
            path: display,
            message: source.to_string(),
        }
    })
}

/// Derives the sibling mission.cfg path from a mission TMA path.
pub(crate) fn mission_cfg_path(mission: &str) -> Result<NormalizedPath, AudioError> {
    let mission =
        normalize_relative(mission.as_bytes(), PathPolicy::StrictLegacy).map_err(|source| {
            AudioError::InvalidPath {
                path: mission.to_string(),
                message: source.to_string(),
            }
        })?;
    let cfg = mission.as_str().rsplit_once('/').map_or_else(
        || "mission.cfg".to_string(),
        |(parent, _)| format!("{parent}/mission.cfg"),
    );
    normalize_relative(cfg.as_bytes(), PathPolicy::StrictLegacy).map_err(|source| {
        AudioError::InvalidPath {
            path: cfg,
            message: source.to_string(),
        }
    })
}

fn ear_positions(listener: Listener) -> Result<([f32; 3], [f32; 3]), AudioError> {
    validate_point(listener.position, "position")?;
    let forward = unit(listener.forward, "forward")?;
    let up = unit(listener.up, "up")?;
    let right = unit(cross(forward, up), "forward/up")?;
    let left_ear = sub(listener.position, scale(right, EAR_HALF_WIDTH));
    let right_ear = add(listener.position, scale(right, EAR_HALF_WIDTH));
    validate_point(left_ear, "position")?;
    validate_point(right_ear, "position")?;
    Ok((left_ear, right_ear))
}

const DIRECTSOUND_DB_COEFFICIENT: f32 = 3_321.927_978_515_625;
const DIRECTSOUND_MIN_VOLUME_DB: f32 = -10_000.0;
const DIRECTSOUND_PAN_UNITS: f32 = 4_000.0;

/// Converts a DirectSound distance range into a linear sample multiplier.
/// The native listener uses rolloff 1.0 and stores volume in hundredths of a
/// decibel. Its max-distance branch is strict (`distance > max`).
fn native_distance_gain(distance: f32, min_distance: f32, max_distance: f32) -> f32 {
    if !distance.is_finite()
        || !min_distance.is_finite()
        || min_distance <= 0.0
        || !max_distance.is_finite()
        || max_distance <= min_distance
    {
        return 1.0;
    }
    if distance <= min_distance {
        return 1.0;
    }
    if distance > max_distance {
        return 10.0_f32.powf(DIRECTSOUND_MIN_VOLUME_DB / 2_000.0);
    }
    let ratio = min_distance / (min_distance + (distance - min_distance));
    if !ratio.is_finite() || ratio <= 0.0 {
        return 10.0_f32.powf(DIRECTSOUND_MIN_VOLUME_DB / 2_000.0);
    }
    let decibels = (ratio.log10() * DIRECTSOUND_DB_COEFFICIENT)
        .round()
        .max(DIRECTSOUND_MIN_VOLUME_DB);
    10.0_f32.powf(decibels / 2_000.0)
}

fn native_spatial_channel_gains(state: NativeSpatialState) -> [f32; 2] {
    let listener_position = midpoint(state.left_ear, state.right_ear);
    let distance = squared_distance(state.emitter_position, listener_position).sqrt();
    let attenuation = native_distance_gain(distance, state.min_distance, state.max_distance);
    // Effect.dll computes software pan from the listener's right vector and
    // the normalized emitter direction, then passes +/-4000 to DirectSound.
    // The engine's ear positions already encode that right vector. Do not use
    // the distance to each ear: that heuristic reverses the native pan when a
    // source is placed to the listener's right.
    let ear_axis = sub(state.right_ear, state.left_ear);
    let ear_axis_length = squared_distance(ear_axis, [0.0; 3]).sqrt();
    let emitter_direction = sub(state.emitter_position, listener_position);
    let emitter_length = squared_distance(emitter_direction, [0.0; 3]).sqrt();
    let pan = if ear_axis_length.is_finite()
        && ear_axis_length > f32::EPSILON
        && emitter_length.is_finite()
        && emitter_length > f32::EPSILON
    {
        let right = scale(ear_axis, 1.0 / ear_axis_length);
        let direction = scale(emitter_direction, 1.0 / emitter_length);
        (DIRECTSOUND_PAN_UNITS * dot(right, direction)).round()
    } else {
        0.0
    };
    let (left_pan, right_pan) = if pan > 0.0 {
        (10.0_f32.powf(-pan / 2_000.0), 1.0)
    } else if pan < 0.0 {
        (1.0, 10.0_f32.powf(pan / 2_000.0))
    } else {
        (1.0, 1.0)
    };
    let volume = if state.volume.is_finite() {
        state.volume.max(0.0)
    } else {
        0.0
    } * attenuation;
    [volume * left_pan, volume * right_pan]
}

fn dot(left: [f32; 3], right: [f32; 3]) -> f32 {
    left[0].mul_add(right[0], left[1].mul_add(right[1], left[2] * right[2]))
}

fn squared_distance(left: [f32; 3], right: [f32; 3]) -> f32 {
    left.into_iter()
        .zip(right)
        .map(|(left, right)| (left - right) * (left - right))
        .sum()
}

fn midpoint(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [
        (left[0] + right[0]) * 0.5,
        (left[1] + right[1]) * 0.5,
        (left[2] + right[2]) * 0.5,
    ]
}

fn validate_point(point: [f32; 3], field: &'static str) -> Result<(), AudioError> {
    if point.iter().all(|value| value.is_finite()) {
        Ok(())
    } else {
        Err(AudioError::InvalidPosition { field })
    }
}

fn unit(vector: [f32; 3], field: &'static str) -> Result<[f32; 3], AudioError> {
    if !vector.iter().all(|value| value.is_finite()) {
        return Err(AudioError::InvalidListener { field });
    }
    let length = vector[0]
        .mul_add(
            vector[0],
            vector[1].mul_add(vector[1], vector[2] * vector[2]),
        )
        .sqrt();
    if !length.is_finite() || length <= f32::EPSILON {
        return Err(AudioError::InvalidListener { field });
    }
    Ok(vector.map(|value| value / length))
}

fn cross(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

fn add(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] + right[0], left[1] + right[1], left[2] + right[2]]
}

fn sub(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

fn scale(vector: [f32; 3], factor: f32) -> [f32; 3] {
    [vector[0] * factor, vector[1] * factor, vector[2] * factor]
}

fn parse_mission_audio_config(bytes: &[u8]) -> Result<MissionAudioConfig, String> {
    #[derive(Default)]
    struct Object {
        library: Option<String>,
        theme: Option<String>,
        default: BTreeMap<usize, String>,
        day: BTreeMap<usize, String>,
        night: BTreeMap<usize, String>,
    }

    let text = String::from_utf8_lossy(bytes);
    let mut loop_object = Object::default();
    let mut variation_object = Object::default();
    let mut current = None;
    for raw_line in text.lines() {
        let line = raw_line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        let mut tokens = line.split_whitespace();
        if tokens
            .next()
            .is_some_and(|token| token.eq_ignore_ascii_case("object"))
        {
            current = tokens.next().map(|name| name.to_ascii_lowercase());
            continue;
        }
        if line.eq_ignore_ascii_case("end") {
            current = None;
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_uppercase();
        let value = parse_cfg_value(value.trim())?;
        match current.as_deref() {
            Some("ambient_music_loop") => {
                if key == "LIBRARY" {
                    loop_object.library = Some(value);
                } else if key == "THEME" {
                    loop_object.theme = Some(value);
                }
            }
            Some("ambient_music_variation") => {
                if key == "LIBRARY" {
                    variation_object.library = Some(value);
                } else if let Some(index) = key.strip_prefix("DEFAULT_VARIATION") {
                    let index = index
                        .parse::<usize>()
                        .map_err(|_| format!("invalid default variation key {key}"))?;
                    if index == 0 {
                        return Err(format!("zero default variation key {key}"));
                    }
                    variation_object.default.insert(index, value);
                } else if let Some(index) = key.strip_prefix("DAY_VARIATION") {
                    let index = index
                        .parse::<usize>()
                        .map_err(|_| format!("invalid day variation key {key}"))?;
                    if index == 0 {
                        return Err(format!("zero day variation key {key}"));
                    }
                    variation_object.day.insert(index, value);
                } else if let Some(index) = key.strip_prefix("NIGHT_VARIATION") {
                    let index = index
                        .parse::<usize>()
                        .map_err(|_| format!("invalid night variation key {key}"))?;
                    if index == 0 {
                        return Err(format!("zero night variation key {key}"));
                    }
                    variation_object.night.insert(index, value);
                }
            }
            _ => {}
        }
    }
    let library = loop_object
        .library
        .ok_or_else(|| "ambient_music_loop.library is missing".to_string())?;
    let theme = loop_object
        .theme
        .ok_or_else(|| "ambient_music_loop.THEME is missing".to_string())?;
    let variation_library = variation_object.library.unwrap_or_else(|| library.clone());
    Ok(MissionAudioConfig {
        library,
        variation_library,
        theme,
        default_variations: variation_object.default.into_values().collect(),
        day_variations: variation_object.day.into_values().collect(),
        night_variations: variation_object.night.into_values().collect(),
    })
}

fn parse_cfg_value(value: &str) -> Result<String, String> {
    if let Some(rest) = value.strip_prefix('"') {
        let end = rest
            .find('"')
            .ok_or_else(|| "unterminated quoted value".to_string())?;
        return Ok(rest[..end].to_string());
    }
    Ok(value
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_original_ambient_names_in_numeric_order() {
        let config = parse_mission_audio_config(
            br#"
                object ambient_music_loop
                 library = "sounds.lib"
                 THEME = "atm_c1_lp.wav"
                end
                object ambient_music_variation
                 library = "ambient.lib"
                 DEFAULT_VARIATION2 = "atm_default2.wav"
                 DEFAULT_VARIATION1 = "atm_default1.wav"
                 DAY_VARIATION2 = "atm_bird2.wav"
                 DAY_VARIATION1 = "atm_bees.wav"
                 NIGHT_VARIATION2 = "atm_frog2.wav"
                 NIGHT_VARIATION1 = "atm_frog1.wav"
                end
            "#,
        )
        .expect("config");
        assert_eq!(config.library, "sounds.lib");
        assert_eq!(config.variation_library, "ambient.lib");
        assert_eq!(config.theme, "atm_c1_lp.wav");
        assert_eq!(
            config.default_variations,
            ["atm_default1.wav", "atm_default2.wav"]
        );
        assert_eq!(config.day_variations, ["atm_bees.wav", "atm_bird2.wav"]);
        assert_eq!(config.night_variations, ["atm_frog1.wav", "atm_frog2.wav"]);
    }

    #[test]
    fn omitted_variation_library_uses_loop_library() {
        let config = parse_mission_audio_config(
            br#"
                object ambient_music_loop
                 library = "sounds.lib"
                 THEME = "atm_c1_lp.wav"
                end
                object ambient_music_variation
                 DEFAULT_VARIATION1 = "atm_c1_1.wav"
                end
            "#,
        )
        .expect("config");
        assert_eq!(config.variation_library, "sounds.lib");
        assert_eq!(config.default_variations, ["atm_c1_1.wav"]);
    }

    #[test]
    fn duplicate_native_variation_key_keeps_last_definition() {
        let config = parse_mission_audio_config(
            br#"
                object ambient_music_loop
                 library = "sounds.lib"
                 THEME = "atm_c1_lp.wav"
                end
                object ambient_music_variation
                 DAY_VARIATION1 = "old.wav"
                 DAY_VARIATION1 = "native-last.wav"
                end
            "#,
        )
        .expect("config");
        assert_eq!(config.day_variations, ["native-last.wav"]);
    }

    #[test]
    fn variation_rng_matches_native_two_state_step() {
        let mut rng = VariationRng {
            a: 0x1234,
            b: 0xabcd,
        };
        rng.step();
        assert_eq!((rng.a, rng.b), (0x8fa5, 0xda43));
        rng.step();
        assert_eq!((rng.a, rng.b), (0xc509, 0xa828));
        rng.step();
        assert_eq!((rng.a, rng.b), (0x223a, 0x762e));
    }

    #[test]
    fn variation_rng_does_not_repeat_last_index_when_pool_has_choices() {
        let mut rng = VariationRng {
            a: 0x1234,
            b: 0xabcd,
        };
        let mut last = -1;
        let mut previous = None;
        for _ in 0..64 {
            let selected = rng.choose(4, &mut last).expect("non-empty pool");
            assert_ne!(Some(selected), previous);
            previous = Some(selected);
        }
    }

    #[test]
    fn variation_rng_clears_last_index_for_empty_pool() {
        let mut rng = VariationRng {
            a: 0x1234,
            b: 0xabcd,
        };
        let mut last = 2;
        assert_eq!(rng.choose(0, &mut last), None);
        assert_eq!(last, -1);
        assert_eq!(rng.choose(3, &mut last), Some(0));
        assert_eq!(last, 0);
    }

    #[test]
    fn crt_delay_uses_native_multiplier_and_ten_to_nineteen_seconds() {
        let mut rng = CrtRand::new(1);
        assert_eq!(rng.next_u15(), 41);
        assert_eq!(rng.next_delay_seconds(), 17.0);
        assert!((10.0..20.0).contains(&rng.next_delay_seconds()));
    }

    #[test]
    fn ambience_timer_uses_strict_boundary_and_initial_positive_tick() {
        let mut scheduler = AmbienceScheduler {
            elapsed_seconds: 0.0,
            next_delay_seconds: 0.0,
            pool: VariationPool::Default,
            variation_rng: VariationRng {
                a: 0x1234,
                b: 0xabcd,
            },
            delay_rng: CrtRand::new(16),
            last_index: -1,
        };
        assert_eq!(scheduler.update(0.0, [2.0, 0.0, 0.0], 0, 1, 1), None);
        let initial = scheduler
            .update(0.001, [2.0, 0.0, 0.0], 0, 1, 1)
            .expect("first positive tick selects a variation");
        assert_eq!(initial.pool, VariationPool::Day);
        assert_eq!(initial.index, Some(0));
        assert_eq!(scheduler.next_delay_seconds, 10.0);
        assert_eq!(scheduler.update(10.0, [2.0, 0.0, 0.0], 0, 1, 1), None);
        assert!(scheduler.update(0.0001, [2.0, 0.0, 0.0], 0, 1, 1).is_some());
    }

    #[test]
    fn ambience_pool_selection_has_no_cross_day_fallback() {
        let mut scheduler = AmbienceScheduler::new(1, 1);
        let selected = scheduler
            .update(0.01, [0.0, 0.0, 0.0], 2, 1, 0)
            .expect("night selection attempt");
        assert_eq!(selected.pool, VariationPool::Night);
        assert_eq!(selected.index, None);

        let mut scheduler = AmbienceScheduler::new(1, 1);
        let selected = scheduler
            .update(0.01, [2.0, 0.0, 0.0], 2, 0, 0)
            .expect("default selection");
        assert_eq!(selected.pool, VariationPool::Default);
        assert_eq!(selected.index, Some(0));
    }

    #[test]
    fn night_threshold_is_native_primary_rgb_length() {
        assert_eq!(selected_pool([1.1, 0.0, 0.0], 1, 1), VariationPool::Night);
        assert_eq!(selected_pool([1.1001, 0.0, 0.0], 1, 1), VariationPool::Day);
        assert_eq!(selected_pool([0.0, 0.0, 0.0], 0, 0), VariationPool::Default);
    }

    #[test]
    fn derives_sibling_mission_config_path() {
        assert_eq!(
            mission_cfg_path("MISSIONS/Single.01/data.tma")
                .expect("mission path")
                .as_str(),
            "MISSIONS/Single.01/mission.cfg"
        );
    }

    #[test]
    fn listener_ears_are_spaced_across_camera_right() {
        let (left, right) = ear_positions(Listener::default()).expect("ears");
        assert_eq!(left, [-EAR_HALF_WIDTH, 0.0, 0.0]);
        assert_eq!(right, [EAR_HALF_WIDTH, 0.0, 0.0]);
    }

    #[test]
    fn native_pan_uses_listener_right_axis_and_directsound_units() {
        let state = NativeSpatialState {
            emitter_position: [0.0, 0.0, 0.0],
            left_ear: [-EAR_HALF_WIDTH, 0.0, 0.0],
            right_ear: [EAR_HALF_WIDTH, 0.0, 0.0],
            volume: 1.0,
            min_distance: 0.0,
            max_distance: f32::INFINITY,
        };
        assert_eq!(native_spatial_channel_gains(state), [1.0, 1.0]);

        let right = NativeSpatialState {
            emitter_position: [1.0, 0.0, 0.0],
            ..state
        };
        let right_gains = native_spatial_channel_gains(right);
        assert!((right_gains[0] - 0.01).abs() < 1.0e-6);
        assert_eq!(right_gains[1], 1.0);

        let left = NativeSpatialState {
            emitter_position: [-1.0, 0.0, 0.0],
            ..state
        };
        let left_gains = native_spatial_channel_gains(left);
        assert_eq!(left_gains[0], 1.0);
        assert!((left_gains[1] - 0.01).abs() < 1.0e-6);
    }

    #[test]
    fn native_distance_gain_keeps_min_and_max_boundaries_native() {
        let below_min = native_distance_gain(99.0, 100.0, 1_500.0);
        let at_min = native_distance_gain(100.0, 100.0, 1_500.0);
        let at_max = native_distance_gain(1_500.0, 100.0, 1_500.0);
        let past_max = native_distance_gain(1_500.001, 100.0, 1_500.0);

        assert_eq!(below_min, 1.0);
        assert_eq!(at_min, 1.0);
        assert!(at_max > past_max);
        assert!(at_max < 1.0);
        assert!((past_max - 1.0e-5).abs() < 1.0e-8);
    }

    #[test]
    fn rejects_non_finite_or_unbounded_listener_inputs() {
        assert!(ear_positions(Listener {
            position: [f32::NAN, 0.0, 0.0],
            ..Listener::default()
        })
        .is_err());
        assert!(ear_positions(Listener {
            forward: [f32::MAX, f32::MAX, f32::MAX],
            ..Listener::default()
        })
        .is_err());
    }

    #[test]
    #[ignore = "requires the installed GOG game files; set FPARKAN_GAME_ROOT"]
    fn decodes_gog_pcm_adpcm_and_ogg_assets() {
        let root = std::env::var_os("FPARKAN_GAME_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(r"C:\GOG Games\Parkan - Iron Strategy"));
        assert!(
            root.is_dir(),
            "game root does not exist: {}",
            root.display()
        );
        let vfs: Arc<dyn Vfs> = Arc::new(fparkan_vfs::DirectoryVfs::new(&root));
        let mission_cfg = normalize_relative(
            b"MISSIONS/Autodemo.00/mission.cfg",
            PathPolicy::StrictLegacy,
        )
        .expect("mission cfg path");
        let mut library = AudioLibrary::load(Arc::clone(&vfs), &mission_cfg).expect("audio assets");
        assert_eq!(library.theme.name, "atm_c1_lp.wav");
        assert!(!library.day_variations.is_empty());
        assert!(!library.night_variations.is_empty());
        assert_eq!(wav_format_tag(&library.theme.bytes), Some(1));
        assert_decodes_samples(&library.theme.resource_name(), &library.theme.bytes, 256);

        let adpcm = library
            .day_variations
            .iter()
            .find(|asset| wav_format_tag(&asset.bytes) == Some(2))
            .expect("an original day variation must use Microsoft ADPCM");
        assert_eq!(wav_format_tag(&adpcm.bytes), Some(2));
        assert_decodes_samples(&adpcm.resource_name(), &adpcm.bytes, 256);

        let music = library
            .music_bytes(DEFAULT_MUSIC_TRACK)
            .expect("original Track02 OGG");
        assert!(music.starts_with(b"OggS"));
        assert_decodes_samples("MUSIC/Track02.ogg", &music, 256);
        assert_eq!(library.music.len(), 1, "music must load lazily");
    }

    #[test]
    #[ignore = "requires the installed GOG game files; set FPARKAN_GAME_ROOT"]
    fn loads_every_gog_mission_variation_pool() {
        let root = std::env::var_os("FPARKAN_GAME_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(r"C:\GOG Games\Parkan - Iron Strategy"));
        assert!(
            root.is_dir(),
            "game root does not exist: {}",
            root.display()
        );
        let mut paths = Vec::new();
        collect_mission_configs(&root, &mut paths).expect("enumerate mission configs");
        assert!(!paths.is_empty(), "GOG install has no mission.cfg files");
        let vfs: Arc<dyn Vfs> = Arc::new(fparkan_vfs::DirectoryVfs::new(&root));
        let mut default_pools = 0;
        let mut day_pools = 0;
        let mut night_pools = 0;
        for path in paths {
            let relative = path
                .strip_prefix(&root)
                .expect("mission config under game root")
                .to_string_lossy()
                .replace('\\', "/");
            let mission_cfg = normalize_relative(relative.as_bytes(), PathPolicy::StrictLegacy)
                .expect("mission cfg path");
            let library = AudioLibrary::load(Arc::clone(&vfs), &mission_cfg)
                .unwrap_or_else(|error| panic!("load {}: {error}", mission_cfg.as_str()));
            default_pools += usize::from(!library.default_variations.is_empty());
            day_pools += usize::from(!library.day_variations.is_empty());
            night_pools += usize::from(!library.night_variations.is_empty());
        }
        assert!(default_pools > 0, "GOG missions must exercise DEFAULT pool");
        assert!(day_pools > 0, "GOG missions must exercise DAY pool");
        assert!(night_pools > 0, "GOG missions must exercise NIGHT pool");
    }

    #[test]
    #[ignore = "requires installed GOG assets and a usable OS audio output"]
    fn starts_pauses_and_drops_gog_audio() {
        let root = std::env::var_os("FPARKAN_GAME_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(r"C:\GOG Games\Parkan - Iron Strategy"));
        assert!(
            root.is_dir(),
            "game root does not exist: {}",
            root.display()
        );
        let vfs: Arc<dyn Vfs> = Arc::new(fparkan_vfs::DirectoryVfs::new(&root));
        let mut audio =
            GameAudio::new(vfs, "MISSIONS/Autodemo.00/data.tma").expect("open original game audio");
        audio
            .handle_sound_event(&SoundEvent {
                archive: "sounds.lib".to_string(),
                name: "atm_rain1.wav".to_string(),
                position: [0.0; 3],
                volume: 0.0,
                min_distance: 0.0,
                max_distance: f32::INFINITY,
                frequency_ratio: 1.0,
                action: SoundAction::StartLoop,
            })
            .expect("start original rain loop");
        assert_eq!(audio.weather_loop.volume(), 0.0);
        audio
            .handle_sound_event(&SoundEvent {
                archive: "sounds.lib".to_string(),
                name: "atm_rain1.wav".to_string(),
                position: [1.0, 2.0, 3.0],
                volume: 0.5,
                min_distance: 0.0,
                max_distance: f32::INFINITY,
                frequency_ratio: 1.0,
                action: SoundAction::SetLoopVolume,
            })
            .expect("update original rain loop volume");
        assert_eq!(audio.weather_loop.volume(), 0.5);
        audio
            .update_listener([10.0, 20.0, 30.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0])
            .expect("move listener");
        audio
            .handle_sound_event(&SoundEvent {
                archive: "sounds.lib".to_string(),
                name: "atm_light1.wav".to_string(),
                position: [3.0, 4.0, 5.0],
                volume: 0.75,
                min_distance: 100.0,
                max_distance: 1500.0,
                frequency_ratio: 1.0,
                action: SoundAction::OneShot,
            })
            .expect("play original lightning sample");
        assert_eq!(
            audio
                .one_shots
                .last()
                .unwrap()
                .player
                .state
                .lock()
                .unwrap()
                .emitter_position,
            [3.0, 4.0, 5.0]
        );
        audio
            .handle_sound_event(&SoundEvent {
                archive: "sounds.lib".to_string(),
                name: "atm_rain1.wav".to_string(),
                position: [10.0, 20.0, 30.0],
                volume: 0.0,
                min_distance: 0.0,
                max_distance: f32::INFINITY,
                frequency_ratio: 1.0,
                action: SoundAction::StopLoop,
            })
            .expect("stop original rain loop");
        assert!(audio.weather_loop.empty());
        audio.set_paused(true);
        assert!(audio.ambient.is_paused());
        assert!(audio.music.is_paused());
        assert!(audio.weather_loop.is_paused());
        audio.set_paused(false);
        assert!(!audio.ambient.is_paused());
        assert!(!audio.music.is_paused());
        audio.shutdown();
    }

    fn assert_decodes_samples(resource: &str, bytes: &Arc<[u8]>, minimum: usize) {
        let decoded = decoder_for_bytes(resource, bytes)
            .expect("decoder construction")
            .take(minimum)
            .count();
        assert_eq!(decoded, minimum, "{resource} decoded too few samples");
    }

    fn wav_format_tag(bytes: &[u8]) -> Option<u16> {
        if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
            return None;
        }
        let mut offset = 12usize;
        while offset.checked_add(8)? <= bytes.len() {
            let chunk_len = usize::try_from(u32::from_le_bytes(
                bytes[offset + 4..offset + 8].try_into().ok()?,
            ))
            .ok()?;
            let data_start = offset.checked_add(8)?;
            let data_end = data_start.checked_add(chunk_len)?;
            if data_end > bytes.len() {
                return None;
            }
            if &bytes[offset..offset + 4] == b"fmt " && chunk_len >= 2 {
                return Some(u16::from_le_bytes(
                    bytes[data_start..data_start + 2].try_into().ok()?,
                ));
            }
            offset = data_end.checked_add(chunk_len & 1)?;
        }
        None
    }

    fn collect_mission_configs(
        directory: &std::path::Path,
        output: &mut Vec<std::path::PathBuf>,
    ) -> std::io::Result<()> {
        for entry in std::fs::read_dir(directory)? {
            let path = entry?.path();
            if path.is_dir() {
                collect_mission_configs(&path, output)?;
            } else if path
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case("mission.cfg"))
            {
                output.push(path);
            }
        }
        Ok(())
    }
}
