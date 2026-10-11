//! Parser and sampler for the original sky.ske atmosphere schedule.
//!
//! The game stores a schedule rather than a list of named weather presets.
//! A schedule contains one or more compressed in-game days. Track durations
//! are real seconds, while key dates are hours, minutes, and seconds in the
//! in-game day. Sampling selects tracks in file order and interpolates the
//! two surrounding keys on the complete cyclic schedule, including adjacent
//! track boundaries.
//!
//! Fields not consumed by the original atmosphere renderer keep neutral names
//! such as raw, header_values, and parameters. They are retained so a later
//! renderer can use them without changing the file parser.

use fparkan_binary::{read_lp_bytes, Cursor, DecodeError};
use std::fmt;

/// sky.ske header marker.
pub const SKE_HEADER_MARKER: i32 = -1;
/// Supported sky.ske version.
pub const SKE_VERSION: u32 = 5;
/// Number of color parameters emitted by the sky sampler.
pub const SKY_COLOR_COUNT: usize = 13;
/// Number of packed colors in every on-disk key palette.
pub const KEY_COLOR_COUNT: usize = 15;
/// Number of fixed length-prefixed strings in every key.
pub const KEY_STRING_COUNT: usize = 6;
/// Number of words in an on-disk date.
pub const DATE_WORD_COUNT: usize = 8;

const MAX_TRACKS: u32 = 256;
const MAX_KEYS: u32 = 16_384;
const MAX_STRING_BYTES: u32 = 64 * 1024;
const MAX_REFERENCES: u32 = 256;
const DAY_SECONDS: f32 = 86_400.0;
const COLOR_MIN_COMPONENT: f32 = 80.0 / 255.0;
const WEATHER_ALPHA: f32 = 150.0 / 255.0;

/// A decoded atmosphere error.
#[derive(Clone, Debug, PartialEq)]
pub enum AtmosphereError {
    /// A bounded binary read failed.
    Decode(DecodeError),
    /// The file marker or version is not supported.
    InvalidHeader {
        /// Header marker read from the file.
        marker: i32,
        /// Header version read from the file.
        version: u32,
    },
    /// A track has a version other than one.
    InvalidTrackVersion {
        /// Track number.
        track_index: usize,
        /// Version read from the file.
        version: u32,
    },
    /// A key has a version other than three.
    InvalidKeyVersion {
        /// Track number.
        track_index: usize,
        /// Key number.
        key_index: usize,
        /// Version read from the file.
        version: u32,
    },
    /// A date contains an impossible time of day.
    InvalidDate {
        /// Context in which the date appeared.
        context: &'static str,
        /// Hour word.
        hours: u32,
        /// Minute word.
        minutes: u32,
        /// Second word.
        seconds: u32,
    },
    /// A key date is earlier than the previous key date.
    NonMonotonicKey {
        /// Track number.
        track_index: usize,
        /// Key number that is out of order.
        key_index: usize,
        /// Previous key time in seconds.
        previous_seconds: u32,
        /// Current key time in seconds.
        current_seconds: u32,
    },
    /// A track contains no key and therefore cannot be sampled.
    EmptyTrack {
        /// Track number.
        track_index: usize,
    },
    /// A floating point value in a key is NaN or infinite.
    NonFiniteValue {
        /// Track number.
        track_index: usize,
        /// Key number.
        key_index: usize,
        /// Neutral field name.
        field: &'static str,
        /// Array element.
        index: usize,
    },
    /// A real duration could not be represented.
    InvalidDuration {
        /// Track number.
        track_index: usize,
    },
}

impl From<DecodeError> for AtmosphereError {
    fn from(value: DecodeError) -> Self {
        Self::Decode(value)
    }
}

impl fmt::Display for AtmosphereError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode(source) => write!(f, "{source}"),
            Self::InvalidHeader { marker, version } => {
                write!(f, "unsupported sky.ske header marker={marker} version={version}")
            }
            Self::InvalidTrackVersion {
                track_index,
                version,
            } => write!(f, "unsupported sky.ske track {track_index} version {version}"),
            Self::InvalidKeyVersion {
                track_index,
                key_index,
                version,
            } => write!(
                f,
                "unsupported sky.ske track {track_index} key {key_index} version {version}"
            ),
            Self::InvalidDate {
                context,
                hours,
                minutes,
                seconds,
            } => write!(
                f,
                "invalid sky.ske {context} time {hours:02}:{minutes:02}:{seconds:02}"
            ),
            Self::NonMonotonicKey {
                track_index,
                key_index,
                previous_seconds,
                current_seconds,
            } => write!(
                f,
                "sky.ske track {track_index} key {key_index} goes backward from {previous_seconds}s to {current_seconds}s"
            ),
            Self::EmptyTrack { track_index } => {
                write!(f, "sky.ske track {track_index} has no keys")
            }
            Self::NonFiniteValue {
                track_index,
                key_index,
                field,
                index,
            } => write!(
                f,
                "sky.ske track {track_index} key {key_index} has non-finite {field}[{index}]"
            ),
            Self::InvalidDuration { track_index } => {
                write!(f, "sky.ske track {track_index} has an invalid duration")
            }
        }
    }
}

impl std::error::Error for AtmosphereError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Decode(source) => Some(source),
            Self::InvalidHeader { .. }
            | Self::InvalidTrackVersion { .. }
            | Self::InvalidKeyVersion { .. }
            | Self::InvalidDate { .. }
            | Self::NonMonotonicKey { .. }
            | Self::EmptyTrack { .. }
            | Self::NonFiniteValue { .. }
            | Self::InvalidDuration { .. } => None,
        }
    }
}

/// The eight words copied by the original date reader.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SkeDate {
    /// Raw date words, preserved in file order.
    pub words: [u32; DATE_WORD_COUNT],
}

impl SkeDate {
    /// Constructs a date from its raw words.
    #[must_use]
    pub const fn from_words(words: [u32; DATE_WORD_COUNT]) -> Self {
        Self { words }
    }

    /// In-game hour word.
    #[must_use]
    pub const fn hours(self) -> u32 {
        self.words[3]
    }

    /// In-game minute word.
    #[must_use]
    pub const fn minutes(self) -> u32 {
        self.words[4]
    }

    /// In-game second word.
    #[must_use]
    pub const fn seconds(self) -> u32 {
        self.words[5]
    }

    /// Returns the time of day in seconds.
    #[must_use]
    pub const fn seconds_of_day(self) -> u32 {
        self.hours() * 3_600 + self.minutes() * 60 + self.seconds()
    }

    /// Returns the hour/minute position used by the original atmosphere
    /// scheduler. The serialized seconds word is deliberately ignored by the
    /// scheduler and is retained only in [`Self::words`].
    #[must_use]
    pub const fn hour_minute_seconds(self) -> u32 {
        (self.hours() * 60 + self.minutes()) * 60
    }

    /// Returns the hour and minute duration marker used by the original track
    /// constructor.
    #[must_use]
    pub const fn real_duration_marker(self) -> u32 {
        self.hour_minute_seconds()
    }
}

/// A packed original ARGB color (0xAARRGGBB).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PackedColor(pub u32);

impl PackedColor {
    /// Returns the raw ARGB word.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// Decodes the word to normalized RGBA components.
    #[must_use]
    pub fn rgba(self) -> [f32; 4] {
        [
            f32::from((self.0 >> 16) as u8) / 255.0,
            f32::from((self.0 >> 8) as u8) / 255.0,
            f32::from(self.0 as u8) / 255.0,
            f32::from((self.0 >> 24) as u8) / 255.0,
        ]
    }

    /// Interpolates two packed colors in normalized channel space and packs
    /// the result back to ARGB.
    #[must_use]
    pub fn lerp(self, other: Self, factor: f32) -> Self {
        let a = self.rgba();
        let b = other.rgba();
        Self::from_rgba([
            lerp(a[0], b[0], factor),
            lerp(a[1], b[1], factor),
            lerp(a[2], b[2], factor),
            lerp(a[3], b[3], factor),
        ])
    }

    /// Packs normalized RGBA components as ARGB.
    #[must_use]
    pub fn from_rgba(rgba: [f32; 4]) -> Self {
        let channel = |value: f32| -> u32 { (value.clamp(0.0, 1.0) * 255.0).round() as u32 };
        Self(
            (channel(rgba[3]) << 24)
                | (channel(rgba[0]) << 16)
                | (channel(rgba[1]) << 8)
                | channel(rgba[2]),
        )
    }
}

/// A length-prefixed string from a key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AtmosphereString {
    /// Original bytes without a terminating NUL.
    pub raw: Vec<u8>,
    /// Lossy UTF-8 view. Resource names in the original files are ASCII.
    pub text: String,
}

impl AtmosphereString {
    /// Returns the decoded text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Whether the length-prefixed value is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }
}

/// A key kind from the original atmosphere dispatcher.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AtmosphereKind {
    /// Starts or updates the sun object.
    SunStart,
    /// Stops the sun object.
    SunStop,
    /// An unclassified state key with raw kind two.
    State2,
    /// Starts rain.
    RainStart,
    /// Stops rain.
    RainStop,
    /// Starts snow.
    SnowStart,
    /// Stops snow.
    SnowStop,
    /// An interpolated state key with raw kind seven.
    State7,
    /// Starts lightning.
    LightningStart,
    /// Stops lightning.
    LightningStop,
    /// A kind not present in the known original dispatcher.
    Unknown(u32),
}

impl AtmosphereKind {
    /// Converts the raw kind word without discarding unknown values.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Self {
        match raw {
            0 => Self::SunStart,
            1 => Self::SunStop,
            2 => Self::State2,
            3 => Self::RainStart,
            4 => Self::RainStop,
            5 => Self::SnowStart,
            6 => Self::SnowStop,
            7 => Self::State7,
            8 => Self::LightningStart,
            9 => Self::LightningStop,
            other => Self::Unknown(other),
        }
    }

    /// Returns the original kind word.
    #[must_use]
    pub const fn raw(self) -> u32 {
        match self {
            Self::SunStart => 0,
            Self::SunStop => 1,
            Self::State2 => 2,
            Self::RainStart => 3,
            Self::RainStop => 4,
            Self::SnowStart => 5,
            Self::SnowStop => 6,
            Self::State7 => 7,
            Self::LightningStart => 8,
            Self::LightningStop => 9,
            Self::Unknown(raw) => raw,
        }
    }
}

/// One exact key record from a version-five sky.ske.
#[derive(Clone, Debug, PartialEq)]
pub struct AtmosphereKey {
    /// Key record version (three in the original files).
    pub version: u32,
    /// In-game date of this key.
    pub date: SkeDate,
    /// Decoded key kind.
    pub kind: AtmosphereKind,
    /// Original kind word.
    pub kind_raw: u32,
    /// Opaque key word following the kind.
    pub raw: u32,
    /// Four packed colors at the start of the key payload.
    pub header_colors: [PackedColor; 4],
    /// Two floating point values following header_colors.
    pub header_values: [f32; 2],
    /// Fifteen packed colors in the key palette.
    pub colors: [PackedColor; KEY_COLOR_COUNT],
    /// Six fixed length-prefixed strings.
    pub strings: Vec<AtmosphereString>,
    /// Four trailing floating point values.
    pub parameters: [f32; 4],
    /// Length-prefixed resource references.
    pub references: Vec<AtmosphereString>,
}

impl AtmosphereKey {
    /// Returns non-empty resource references in file order.
    #[must_use]
    pub fn resource_references(&self) -> Vec<&AtmosphereString> {
        self.references
            .iter()
            .filter(|reference| !reference.is_empty())
            .collect()
    }
}

/// One atmosphere track.
#[derive(Clone, Debug, PartialEq)]
pub struct AtmosphereTrack {
    /// Track version (one in the original files).
    pub version: u32,
    /// Track start date copied from disk.
    pub start: SkeDate,
    /// Track end/duration date copied from disk.
    pub end: SkeDate,
    /// Keys in file order.
    pub keys: Vec<AtmosphereKey>,
    /// Real seconds assigned to this track by the original constructor.
    pub duration_seconds: u32,
}

impl AtmosphereTrack {
    /// Returns a key's in-game time in seconds.
    #[must_use]
    pub fn key_seconds(&self, index: usize) -> Option<u32> {
        self.keys.get(index).map(|key| key.date.seconds_of_day())
    }
}

/// The final forty bytes of a sky.ske.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AtmosphereTrailer {
    /// Trailer date copied from disk.
    pub date: SkeDate,
    /// Two opaque trailer words.
    pub values: [u32; 2],
}

/// A timeline record emitted by TypedAtmosphere::timeline and
/// TypedAtmosphere::events_between.
#[derive(Clone, Debug, PartialEq)]
pub struct AtmosphereEvent {
    /// Track containing the key.
    pub track_index: usize,
    /// Key containing the event.
    pub key_index: usize,
    /// In-game time of the event in seconds.
    pub day_seconds: f32,
    /// Real time from the beginning of the repeating schedule.
    pub real_seconds: f32,
    /// Decoded event kind.
    pub kind: AtmosphereKind,
    /// Opaque key word.
    pub raw: u32,
    /// Non-empty resource references attached to the key.
    pub resources: Vec<AtmosphereString>,
}

/// A sky sample with the thirteen color parameters used by the original
/// CSky sampler.
#[derive(Clone, Debug, PartialEq)]
pub struct SkySample {
    /// Colors zero through eleven: four header colors, then two reordered
    /// groups of four palette colors.
    pub colors: [PackedColor; SKY_COLOR_COUNT],
    /// The two floating point values copied from the key header values.
    pub values: [f32; 2],
    /// The two remaining packed color parameters.
    pub packed: [PackedColor; 2],
}

/// Birth payload generated by the native atmosphere dispatcher for a named
/// celestial object.
///
/// The dispatcher does not serialize this payload in `sky.ske`. For a kind
/// zero key it chooses the angle pair from the fixed string in slot zero and
/// computes the lifetime from the following matching kind one key. Keeping
/// the generated values beside the sample lets the renderer reproduce the
/// native object without inventing a start time on its first active frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SunBirth {
    /// Absolute real schedule time at which this interval began.
    pub start_seconds: f32,
    /// Real schedule duration until the matching stop key.
    pub lifetime_seconds: f32,
    /// Native birth angles in degrees.
    pub initial_angles_degrees: [f32; 2],
    /// WEA material row selected by the dispatcher.
    pub material_row: usize,
}

/// A sampled sun parameter set. The same payload shape is used for the moon
/// because the original dispatcher creates both from the same object class.
#[derive(Clone, Debug, PartialEq)]
pub struct SunSample {
    /// Whether the latest SunStart/SunStop lifecycle interval is active.
    pub active: bool,
    /// RGB color multiplied by the third trailing key parameter.
    pub rgb: [f32; 3],
    /// First two trailing key parameters, kept with neutral names.
    pub values: [f32; 2],
    /// Two packed parameters consumed by the original sun object.
    pub packed: [PackedColor; 2],
    /// The third trailing parameter used for RGB intensity.
    pub intensity: f32,
    /// Generated birth payload for the current named interval, if active.
    pub birth: Option<SunBirth>,
}

/// A sampled moon parameter set with the native sun-object layout.
pub type MoonSample = SunSample;

/// A sampled precipitation or lightning state.
#[derive(Clone, Debug, PartialEq)]
pub struct WeatherSample {
    /// Whether the corresponding start/stop state is active.
    pub active: bool,
    /// Interpolated fourth trailing key parameter.
    pub intensity: f32,
    /// Effective color. Active weather follows the original 80/255 RGB floor
    /// and 150/255 alpha used by the particle objects.
    pub color: [f32; 4],
    /// Interpolated source color before the particle floor is applied.
    pub raw_color: [f32; 4],
    /// Resource references for the currently active interval.
    pub resources: Vec<AtmosphereString>,
}

/// A complete atmosphere sample.
#[derive(Clone, Debug, PartialEq)]
pub struct AtmosphereFrame {
    /// Requested real schedule time after finite-value sanitization.
    pub real_seconds: f32,
    /// Selected track.
    pub track_index: usize,
    /// Real seconds within the selected track.
    pub track_seconds: f32,
    /// In-game seconds within the selected day.
    pub day_seconds: f32,
    /// Interpolated sky parameters.
    pub sky: SkySample,
    /// Interpolated sun parameters.
    pub sun: SunSample,
    /// Interpolated moon parameters and its independent lifecycle.
    pub moon: MoonSample,
    /// Rain state.
    pub rain: WeatherSample,
    /// Snow state.
    pub snow: WeatherSample,
    /// Lightning state.
    pub lightning: WeatherSample,
}

/// A parsed, repeating version-five atmosphere schedule.
#[derive(Clone, Debug, PartialEq)]
pub struct TypedAtmosphere {
    /// Header marker.
    pub marker: i32,
    /// Header version.
    pub version: u32,
    /// Tracks in file order.
    pub tracks: Vec<AtmosphereTrack>,
    /// Trailer copied from disk.
    pub trailer: AtmosphereTrailer,
}

impl TypedAtmosphere {
    /// Parses a complete version-five sky.ske byte slice.
    ///
    /// The parser consumes every byte, bounds all declared counts and strings,
    /// checks every floating point field, and preserves unknown kinds and
    /// payload words.
    ///
    /// Returns an error for unsupported versions, malformed dates,
    /// non-finite values, truncation, or trailing bytes.
    pub fn parse(bytes: &[u8]) -> Result<Self, AtmosphereError> {
        let mut cursor = Cursor::new(bytes);
        let marker = cursor.read_i32_le()?;
        let version = cursor.read_u32_le()?;
        let track_count = cursor.read_u32_le()?;
        if marker != SKE_HEADER_MARKER || version != SKE_VERSION {
            return Err(AtmosphereError::InvalidHeader { marker, version });
        }
        checked_count(track_count, MAX_TRACKS)?;

        let mut tracks = Vec::with_capacity(track_count as usize);
        for track_index in 0..track_count as usize {
            let track_version = cursor.read_u32_le()?;
            if track_version != 1 {
                return Err(AtmosphereError::InvalidTrackVersion {
                    track_index,
                    version: track_version,
                });
            }
            let key_count = cursor.read_u32_le()?;
            checked_count(key_count, MAX_KEYS)?;
            let start = read_date(&mut cursor, "track start")?;
            let end = read_date(&mut cursor, "track end")?;
            let duration_seconds = end.real_duration_marker();
            if duration_seconds == 0 {
                return Err(AtmosphereError::InvalidDuration { track_index });
            }
            if key_count == 0 {
                return Err(AtmosphereError::EmptyTrack { track_index });
            }

            let mut keys = Vec::with_capacity(key_count as usize);
            let mut previous_seconds = None;
            for key_index in 0..key_count as usize {
                let key = read_key(&mut cursor, track_index, key_index)?;
                let current_seconds = key.date.hour_minute_seconds();
                if let Some(previous_seconds) = previous_seconds {
                    if current_seconds < previous_seconds {
                        return Err(AtmosphereError::NonMonotonicKey {
                            track_index,
                            key_index,
                            previous_seconds,
                            current_seconds,
                        });
                    }
                }
                previous_seconds = Some(current_seconds);
                keys.push(key);
            }
            tracks.push(AtmosphereTrack {
                version: track_version,
                start,
                end,
                keys,
                duration_seconds,
            });
        }

        let trailer = AtmosphereTrailer {
            date: read_date(&mut cursor, "trailer")?,
            values: [cursor.read_u32_le()?, cursor.read_u32_le()?],
        };
        cursor.require_eof()?;
        Ok(Self {
            marker,
            version,
            tracks,
            trailer,
        })
    }

    /// Returns the duration of one complete repeating schedule.
    #[must_use]
    pub fn total_duration_seconds(&self) -> f32 {
        self.tracks
            .iter()
            .map(|track| track.duration_seconds as f32)
            .sum()
    }

    /// Returns the integer real-time offset selected by the trailer for the
    /// initial atmosphere clock. The trailer's first word selects a track;
    /// its hour/minute words are scaled by that track's duration and floored
    /// exactly as in the original constructor.
    #[must_use]
    pub fn initial_offset_seconds(&self) -> u32 {
        let track_index = usize::try_from(self.trailer.date.words[0]).ok();
        let Some(track) = track_index.and_then(|index| self.tracks.get(index)) else {
            return 0;
        };
        let scaled = u64::from(track.duration_seconds)
            .saturating_mul(u64::from(self.trailer.date.hour_minute_seconds()))
            / DAY_SECONDS_U64;
        let track_offset = track_index
            .map(|index| self.track_offset_seconds(index))
            .unwrap_or(0);
        u32::try_from(track_offset.saturating_add(scaled)).unwrap_or(u32::MAX)
    }

    /// Samples at real seconds from the beginning of the repeating schedule.
    ///
    /// Each track occupies its original real duration. Its in-game day is
    /// then traversed from 00:00 through 24:00. Non-finite input is treated
    /// as zero so a renderer cannot poison a frame with NaN coordinates.
    #[must_use]
    pub fn sample(&self, real_seconds: f32) -> AtmosphereFrame {
        let real_seconds = if real_seconds.is_finite() {
            real_seconds
        } else {
            0.0
        };
        if self.tracks.is_empty() {
            return empty_frame(real_seconds);
        }
        let total = self.total_duration_seconds();
        let wrapped = real_seconds.rem_euclid(total);
        let (track_index, track_seconds) = self.locate_track(wrapped);
        let track = &self.tracks[track_index];
        let day_seconds = track_seconds / track.duration_seconds as f32 * DAY_SECONDS;
        let points = self.schedule_points();
        self.sample_track(
            real_seconds,
            wrapped,
            real_seconds,
            track_index,
            track_seconds,
            day_seconds,
            &points,
        )
    }

    /// Samples a selected track directly in in-game seconds.
    ///
    /// This is useful for deterministic tests and tools that already have the
    /// original game clock. The day wraps at 24 hours.
    #[must_use]
    pub fn sample_day(&self, track_index: usize, day_seconds: f32) -> AtmosphereFrame {
        if self.tracks.is_empty() {
            return empty_frame(0.0);
        }
        let track_index = track_index.min(self.tracks.len() - 1);
        let track = &self.tracks[track_index];
        let day_seconds = if day_seconds.is_finite() {
            day_seconds.rem_euclid(DAY_SECONDS)
        } else {
            0.0
        };
        let track_seconds = day_seconds / DAY_SECONDS * track.duration_seconds as f32;
        let schedule_seconds = self.track_offset_seconds(track_index) as f32 + track_seconds;
        let wrapped = schedule_seconds.rem_euclid(self.total_duration_seconds());
        let points = self.schedule_points();
        self.sample_track(
            schedule_seconds,
            wrapped,
            schedule_seconds,
            track_index,
            track_seconds,
            day_seconds,
            &points,
        )
    }

    /// Returns every key in file order with real schedule offsets.
    #[must_use]
    pub fn timeline(&self) -> Vec<AtmosphereEvent> {
        self.schedule_points()
            .into_iter()
            .map(|point| {
                event_for_key(
                    point.track_index,
                    point.key_index,
                    point.key,
                    point.real_seconds as f32,
                )
            })
            .collect()
    }

    /// Returns timeline records crossed by (start, end], including schedule
    /// wraparound and track boundaries.
    #[must_use]
    pub fn events_between(&self, start: f32, end: f32) -> Vec<AtmosphereEvent> {
        if self.tracks.is_empty() || !start.is_finite() || !end.is_finite() || end <= start {
            return Vec::new();
        }
        let total = self.total_duration_seconds();
        if total <= 0.0 || !total.is_finite() {
            return Vec::new();
        }
        let base = self.timeline();
        if base.is_empty() {
            return Vec::new();
        }
        let first_cycle = (start / total).floor() as i64 - 1;
        let last_cycle = (end / total).ceil() as i64 + 1;
        let mut out = Vec::new();
        for cycle in first_cycle..=last_cycle {
            let offset = cycle as f32 * total;
            for event in &base {
                let event_time = event.real_seconds + offset;
                if event_time > start && event_time <= end {
                    let mut event = event.clone();
                    event.real_seconds = event_time;
                    out.push(event);
                }
            }
        }
        out.sort_by(|a, b| {
            a.real_seconds
                .partial_cmp(&b.real_seconds)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        out
    }

    fn locate_track(&self, wrapped_seconds: f32) -> (usize, f32) {
        let mut offset = 0.0;
        for (index, track) in self.tracks.iter().enumerate() {
            let duration = track.duration_seconds as f32;
            if wrapped_seconds < offset + duration || index + 1 == self.tracks.len() {
                return (index, (wrapped_seconds - offset).clamp(0.0, duration));
            }
            offset += duration;
        }
        (self.tracks.len() - 1, 0.0)
    }

    fn track_offset_seconds(&self, track_index: usize) -> u64 {
        self.tracks
            .iter()
            .take(track_index)
            .map(|track| u64::from(track.duration_seconds))
            .sum()
    }

    fn schedule_points(&self) -> Vec<SchedulePoint<'_>> {
        let mut points = Vec::new();
        let mut track_offset = 0_u64;
        for (track_index, track) in self.tracks.iter().enumerate() {
            for (key_index, key) in track.keys.iter().enumerate() {
                let key_offset = u64::from(track.duration_seconds)
                    .saturating_mul(u64::from(key.date.hour_minute_seconds()))
                    / DAY_SECONDS_U64;
                points.push(SchedulePoint {
                    track_index,
                    key_index,
                    key,
                    real_seconds: track_offset.saturating_add(key_offset),
                });
            }
            track_offset = track_offset.saturating_add(u64::from(track.duration_seconds));
        }
        points
    }

    fn sample_track(
        &self,
        real_seconds: f32,
        sample_seconds: f32,
        birth_seconds: f32,
        track_index: usize,
        track_seconds: f32,
        day_seconds: f32,
        points: &[SchedulePoint<'_>],
    ) -> AtmosphereFrame {
        let interpolation =
            interpolate_schedule(points, sample_seconds, self.total_duration_seconds());
        let sky = sky_sample(&interpolation);
        let total_seconds = self.total_duration_seconds();
        let sun_birth = celestial_birth(points, birth_seconds, total_seconds, b"sun");
        let moon_birth = celestial_birth(points, birth_seconds, total_seconds, b"moon");
        let sun = sun_sample(&interpolation, sun_birth);
        let moon = sun_sample(&interpolation, moon_birth);
        let rain = weather_sample(
            points,
            sample_seconds,
            self.total_duration_seconds(),
            &[AtmosphereKind::RainStart, AtmosphereKind::RainStop],
        );
        let snow = weather_sample(
            points,
            sample_seconds,
            self.total_duration_seconds(),
            &[AtmosphereKind::SnowStart, AtmosphereKind::SnowStop],
        );
        let lightning = weather_sample(
            points,
            sample_seconds,
            self.total_duration_seconds(),
            &[
                AtmosphereKind::LightningStart,
                AtmosphereKind::LightningStop,
            ],
        );
        AtmosphereFrame {
            real_seconds,
            track_index,
            track_seconds,
            day_seconds,
            sky,
            sun,
            moon,
            rain,
            snow,
            lightning,
        }
    }
}

const DAY_SECONDS_U64: u64 = 86_400;

#[derive(Clone, Copy, Debug)]
struct SchedulePoint<'a> {
    track_index: usize,
    key_index: usize,
    key: &'a AtmosphereKey,
    real_seconds: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct InterpolatedKey {
    header_colors: [PackedColor; 4],
    header_values: [f32; 2],
    colors: [PackedColor; KEY_COLOR_COUNT],
    parameters: [f32; 4],
}

fn read_date(cursor: &mut Cursor<'_>, context: &'static str) -> Result<SkeDate, AtmosphereError> {
    let mut words = [0_u32; DATE_WORD_COUNT];
    for word in &mut words {
        *word = cursor.read_u32_le()?;
    }
    let date = SkeDate::from_words(words);
    if date.hours() > 24
        || date.minutes() > 59
        || date.seconds() > 59
        || (date.hours() == 24 && (date.minutes() != 0 || date.seconds() != 0))
    {
        return Err(AtmosphereError::InvalidDate {
            context,
            hours: date.hours(),
            minutes: date.minutes(),
            seconds: date.seconds(),
        });
    }
    Ok(date)
}

fn read_key(
    cursor: &mut Cursor<'_>,
    track_index: usize,
    key_index: usize,
) -> Result<AtmosphereKey, AtmosphereError> {
    let version = cursor.read_u32_le()?;
    if version != 3 {
        return Err(AtmosphereError::InvalidKeyVersion {
            track_index,
            key_index,
            version,
        });
    }
    let date = read_date(cursor, "key")?;
    let kind_raw = cursor.read_u32_le()?;
    let raw = cursor.read_u32_le()?;
    let mut header_colors = [PackedColor::default(); 4];
    for color in &mut header_colors {
        *color = PackedColor(cursor.read_u32_le()?);
    }
    let header_values = [cursor.read_f32_le()?, cursor.read_f32_le()?];
    let mut colors = [PackedColor::default(); KEY_COLOR_COUNT];
    for color in &mut colors {
        *color = PackedColor(cursor.read_u32_le()?);
    }
    let mut strings = Vec::with_capacity(KEY_STRING_COUNT);
    for _ in 0..KEY_STRING_COUNT {
        strings.push(read_string(cursor)?);
    }
    let parameters = [
        cursor.read_f32_le()?,
        cursor.read_f32_le()?,
        cursor.read_f32_le()?,
        cursor.read_f32_le()?,
    ];
    for (index, value) in header_values.iter().enumerate() {
        if !value.is_finite() {
            return Err(AtmosphereError::NonFiniteValue {
                track_index,
                key_index,
                field: "header_values",
                index,
            });
        }
    }
    for (index, value) in parameters.iter().enumerate() {
        if !value.is_finite() {
            return Err(AtmosphereError::NonFiniteValue {
                track_index,
                key_index,
                field: "parameters",
                index,
            });
        }
    }
    let reference_count = cursor.read_u32_le()?;
    checked_count(reference_count, MAX_REFERENCES)?;
    let mut references = Vec::with_capacity(reference_count as usize);
    for _ in 0..reference_count {
        references.push(read_string(cursor)?);
    }
    Ok(AtmosphereKey {
        version,
        date,
        kind: AtmosphereKind::from_raw(kind_raw),
        kind_raw,
        raw,
        header_colors,
        header_values,
        colors,
        strings,
        parameters,
        references,
    })
}

fn read_string(cursor: &mut Cursor<'_>) -> Result<AtmosphereString, AtmosphereError> {
    let raw = read_lp_bytes(cursor, MAX_STRING_BYTES)?;
    let text = String::from_utf8_lossy(&raw).into_owned();
    Ok(AtmosphereString { raw, text })
}

fn checked_count(count: u32, limit: u32) -> Result<(), AtmosphereError> {
    if count > limit {
        return Err(AtmosphereError::Decode(DecodeError::LimitExceeded {
            count: u64::from(count),
            limit: u64::from(limit),
        }));
    }
    Ok(())
}

fn lerp(a: f32, b: f32, factor: f32) -> f32 {
    a + (b - a) * factor.clamp(0.0, 1.0)
}

fn interpolate_schedule(
    points: &[SchedulePoint<'_>],
    schedule_seconds: f32,
    total_seconds: f32,
) -> InterpolatedKey {
    let (before, after, factor) = surrounding_points(points, schedule_seconds, total_seconds);
    InterpolatedKey {
        header_colors: std::array::from_fn(|index| {
            before.key.header_colors[index].lerp(after.key.header_colors[index], factor)
        }),
        header_values: std::array::from_fn(|index| {
            lerp(
                before.key.header_values[index],
                after.key.header_values[index],
                factor,
            )
        }),
        colors: std::array::from_fn(|index| {
            before.key.colors[index].lerp(after.key.colors[index], factor)
        }),
        parameters: std::array::from_fn(|index| {
            lerp(
                before.key.parameters[index],
                after.key.parameters[index],
                factor,
            )
        }),
    }
}

fn surrounding_points<'a>(
    points: &'a [SchedulePoint<'a>],
    schedule_seconds: f32,
    total_seconds: f32,
) -> (&'a SchedulePoint<'a>, &'a SchedulePoint<'a>, f32) {
    debug_assert!(!points.is_empty());
    if points.len() == 1 {
        return (&points[0], &points[0], 0.0);
    }
    let sample_seconds = schedule_seconds.rem_euclid(total_seconds);

    // The native previous-key helper is called with current time + 1 and a
    // strict comparison, which makes an integer key at the current time the
    // previous key. `<=` expresses that behavior without a frame epsilon.
    let mut previous_index = None;
    let mut next_index = None;
    for (index, point) in points.iter().enumerate() {
        let point_seconds = point.real_seconds as f32;
        if point_seconds <= sample_seconds {
            previous_index = Some(index);
        }
        if next_index.is_none() && point_seconds >= sample_seconds {
            // Prefer the last key when several track boundaries share the
            // same real timestamp. This selects the newly entered track.
            let mut end = index;
            while end + 1 < points.len() && points[end + 1].real_seconds == point.real_seconds {
                end += 1;
            }
            next_index = Some(end);
        }
    }

    let previous_index = previous_index.unwrap_or(points.len() - 1);
    let next_index = next_index.unwrap_or(0);
    let previous_wrap = if points[previous_index].real_seconds as f32 > sample_seconds {
        total_seconds
    } else {
        0.0
    };
    let next_wrap = if (points[next_index].real_seconds as f32) < sample_seconds {
        total_seconds
    } else {
        0.0
    };
    let previous_seconds = points[previous_index].real_seconds as f32 - previous_wrap;
    let next_seconds = points[next_index].real_seconds as f32 + next_wrap;
    let span = (next_seconds - previous_seconds).max(0.0);
    let factor = if span > 0.0 {
        ((sample_seconds - previous_seconds) / span).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (&points[previous_index], &points[next_index], factor)
}

fn sky_sample(key: &InterpolatedKey) -> SkySample {
    let colors = [
        key.header_colors[1],
        key.header_colors[2],
        key.header_colors[0],
        key.header_colors[3],
        key.colors[0],
        key.colors[3],
        key.colors[1],
        key.colors[2],
        key.colors[4],
        key.colors[7],
        key.colors[5],
        key.colors[6],
        key.colors[8],
    ];
    SkySample {
        colors,
        values: key.header_values,
        packed: [key.colors[11], key.colors[13]],
    }
}

fn sun_sample(key: &InterpolatedKey, birth: Option<SunBirth>) -> SunSample {
    let source = key.colors[12].rgba();
    let intensity = key.parameters[2];
    // Native selector-0 outputs 5/6 are palette[9]/[14] (Terrain.dll RVA 0x3DFB0);
    // CSun uses slot 5 alpha for flare opacity (+0x4B3AB) and slot 6 RGB for secondary light (+0x4B4BE).
    SunSample {
        active: birth.is_some(),
        rgb: [
            source[0] * intensity,
            source[1] * intensity,
            source[2] * intensity,
        ],
        values: [key.parameters[0], key.parameters[1]],
        packed: [key.colors[9], key.colors[14]],
        intensity,
        birth,
    }
}

fn weather_sample(
    points: &[SchedulePoint<'_>],
    schedule_seconds: f32,
    total_seconds: f32,
    markers: &[AtmosphereKind; 2],
) -> WeatherSample {
    let (active, resources) = active_interval(points, schedule_seconds, total_seconds, markers);
    let interpolation = interpolate_schedule(points, schedule_seconds, total_seconds);
    let raw_color = interpolation.colors[12].rgba();
    let intensity = if active {
        interpolation.parameters[3].max(0.0)
    } else {
        0.0
    };
    let color = if active {
        [
            raw_color[0].max(COLOR_MIN_COMPONENT),
            raw_color[1].max(COLOR_MIN_COMPONENT),
            raw_color[2].max(COLOR_MIN_COMPONENT),
            WEATHER_ALPHA,
        ]
    } else {
        [0.0; 4]
    };
    WeatherSample {
        active,
        intensity,
        color,
        raw_color,
        resources,
    }
}

fn active_interval(
    points: &[SchedulePoint<'_>],
    schedule_seconds: f32,
    total_seconds: f32,
    markers: &[AtmosphereKind; 2],
) -> (bool, Vec<AtmosphereString>) {
    let sample_seconds = schedule_seconds.rem_euclid(total_seconds);
    let mut latest = None;
    for point in points {
        if point.key.kind != markers[0] && point.key.kind != markers[1] {
            continue;
        }
        if point.real_seconds as f32 <= sample_seconds {
            latest = Some(point);
        }
    }
    let Some(latest) = latest.or_else(|| {
        points
            .iter()
            .rev()
            .find(|point| point.key.kind == markers[0] || point.key.kind == markers[1])
    }) else {
        return (false, Vec::new());
    };
    if latest.key.kind == markers[0] {
        (
            true,
            latest
                .key
                .resource_references()
                .into_iter()
                .cloned()
                .collect(),
        )
    } else {
        (false, Vec::new())
    }
}

/// Reconstructs the native birth payload for one named celestial object.
///
/// The dispatcher compares the first fixed key string with `"sun"`; every
/// other celestial name uses the moon angle pair. It then scans forward for
/// the next matching stop event and stores the real-time difference as the
/// lifetime. Schedule points already contain the native integer floor, so no
/// fractional day-time conversion is introduced here.
fn celestial_birth(
    points: &[SchedulePoint<'_>],
    schedule_seconds: f32,
    total_seconds: f32,
    name: &[u8],
) -> Option<SunBirth> {
    if points.is_empty() || !total_seconds.is_finite() || total_seconds <= 0.0 {
        return None;
    }
    let schedule_seconds = if schedule_seconds.is_finite() {
        schedule_seconds
    } else {
        0.0
    };
    let cycle = (schedule_seconds / total_seconds).floor();
    let sample_absolute = cycle * total_seconds + schedule_seconds.rem_euclid(total_seconds);

    let mut latest: Option<(f32, usize, AtmosphereKind)> = None;
    for (index, point) in points.iter().enumerate() {
        if !matches!(
            point.key.kind,
            AtmosphereKind::SunStart | AtmosphereKind::SunStop
        ) || point
            .key
            .strings
            .first()
            .map(|string| string.raw.as_slice())
            != Some(name)
        {
            continue;
        }
        let base = point.real_seconds as f32;
        // Only the surrounding cycles can contain the latest event. Keeping
        // all three also handles sample_day values at either schedule edge.
        for cycle_delta in -1..=1 {
            let time = base + (cycle + cycle_delta as f32) * total_seconds;
            if time > sample_absolute {
                continue;
            }
            let replace = latest
                .map(|(old_time, old_index, _)| {
                    time > old_time || (time == old_time && index >= old_index)
                })
                .unwrap_or(true);
            if replace {
                latest = Some((time, index, point.key.kind));
            }
        }
    }
    let (start_seconds, start_index, kind) = latest?;
    if kind != AtmosphereKind::SunStart {
        return None;
    }

    let mut stop_seconds = None;
    for point in points {
        if point.key.kind != AtmosphereKind::SunStop
            || point
                .key
                .strings
                .first()
                .map(|string| string.raw.as_slice())
                != Some(name)
        {
            continue;
        }
        let base = point.real_seconds as f32;
        for cycle_delta in -1..=2 {
            let time = base + (cycle + cycle_delta as f32) * total_seconds;
            if time <= start_seconds {
                continue;
            }
            if stop_seconds.map(|old| time < old).unwrap_or(true) {
                stop_seconds = Some(time);
            }
        }
    }
    let lifetime_seconds = stop_seconds
        .map(|stop| stop - start_seconds)
        .filter(|lifetime| lifetime.is_finite() && *lifetime > 0.0)?;
    let (initial_angles_degrees, material_row) = if name == b"sun" {
        ([90.0, 30.0], 3)
    } else {
        ([0.0, 50.0], 4)
    };
    let _ = start_index;
    Some(SunBirth {
        start_seconds,
        lifetime_seconds,
        initial_angles_degrees,
        material_row,
    })
}

fn event_for_key(
    track_index: usize,
    key_index: usize,
    key: &AtmosphereKey,
    real_seconds: f32,
) -> AtmosphereEvent {
    AtmosphereEvent {
        track_index,
        key_index,
        day_seconds: key.date.hour_minute_seconds() as f32,
        real_seconds,
        kind: key.kind,
        raw: key.raw,
        resources: key.resource_references().into_iter().cloned().collect(),
    }
}

fn empty_frame(real_seconds: f32) -> AtmosphereFrame {
    AtmosphereFrame {
        real_seconds,
        track_index: 0,
        track_seconds: 0.0,
        day_seconds: 0.0,
        sky: SkySample {
            colors: [PackedColor::default(); SKY_COLOR_COUNT],
            values: [0.0; 2],
            packed: [PackedColor::default(); 2],
        },
        sun: SunSample {
            active: false,
            rgb: [0.0; 3],
            values: [0.0; 2],
            packed: [PackedColor::default(); 2],
            intensity: 0.0,
            birth: None,
        },
        moon: SunSample {
            active: false,
            rgb: [0.0; 3],
            values: [0.0; 2],
            packed: [PackedColor::default(); 2],
            intensity: 0.0,
            birth: None,
        },
        rain: WeatherSample {
            active: false,
            intensity: 0.0,
            color: [0.0; 4],
            raw_color: [0.0; 4],
            resources: Vec::new(),
        },
        snow: WeatherSample {
            active: false,
            intensity: 0.0,
            color: [0.0; 4],
            raw_color: [0.0; 4],
            resources: Vec::new(),
        },
        lightning: WeatherSample {
            active: false,
            intensity: 0.0,
            color: [0.0; 4],
            raw_color: [0.0; 4],
            resources: Vec::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn date(hours: u32, minutes: u32, seconds: u32) -> [u32; DATE_WORD_COUNT] {
        [0, 0, 0, hours, minutes, seconds, 0, 0]
    }

    fn lp(bytes: &[u8], out: &mut Vec<u8>) {
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(bytes);
    }

    fn key(
        out: &mut Vec<u8>,
        hours: u32,
        minutes: u32,
        kind: u32,
        palette: [u32; KEY_COLOR_COUNT],
        tail: [f32; 4],
        references: &[&[u8]],
    ) {
        key_with_seconds(out, hours, minutes, 0, kind, palette, tail, references);
    }

    fn key_with_seconds(
        out: &mut Vec<u8>,
        hours: u32,
        minutes: u32,
        seconds: u32,
        kind: u32,
        palette: [u32; KEY_COLOR_COUNT],
        tail: [f32; 4],
        references: &[&[u8]],
    ) {
        out.extend_from_slice(&3_u32.to_le_bytes());
        for word in date(hours, minutes, seconds) {
            out.extend_from_slice(&word.to_le_bytes());
        }
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&0xAABB_CCDD_u32.to_le_bytes());
        for value in [0xFF11_2233_u32, 0xFF44_5566, 0xFF77_8899, 0xFFAA_BBCC] {
            out.extend_from_slice(&value.to_le_bytes());
        }
        for value in [0.25_f32, 0.75_f32] {
            out.extend_from_slice(&value.to_bits().to_le_bytes());
        }
        for value in palette {
            out.extend_from_slice(&value.to_le_bytes());
        }
        for _ in 0..KEY_STRING_COUNT {
            lp(b"", out);
        }
        for value in tail {
            out.extend_from_slice(&value.to_bits().to_le_bytes());
        }
        out.extend_from_slice(&(references.len() as u32).to_le_bytes());
        for reference in references {
            lp(reference, out);
        }
    }

    fn document(key_count: u32, keys: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&SKE_HEADER_MARKER.to_le_bytes());
        out.extend_from_slice(&SKE_VERSION.to_le_bytes());
        out.extend_from_slice(&1_u32.to_le_bytes());
        out.extend_from_slice(&1_u32.to_le_bytes());
        out.extend_from_slice(&key_count.to_le_bytes());
        for word in date(23, 59, 0) {
            out.extend_from_slice(&word.to_le_bytes());
        }
        for word in date(0, 15, 0) {
            out.extend_from_slice(&word.to_le_bytes());
        }
        keys(&mut out);
        for word in date(1, 30, 0) {
            out.extend_from_slice(&word.to_le_bytes());
        }
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&1_u32.to_le_bytes());
        out
    }

    fn two_track_document(palette: [u32; KEY_COLOR_COUNT]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&SKE_HEADER_MARKER.to_le_bytes());
        out.extend_from_slice(&SKE_VERSION.to_le_bytes());
        out.extend_from_slice(&2_u32.to_le_bytes());

        out.extend_from_slice(&1_u32.to_le_bytes());
        out.extend_from_slice(&2_u32.to_le_bytes());
        for word in date(23, 59, 0) {
            out.extend_from_slice(&word.to_le_bytes());
        }
        for word in date(0, 1, 0) {
            out.extend_from_slice(&word.to_le_bytes());
        }
        key_with_seconds(&mut out, 23, 0, 0, 7, palette, [0.0, 0.0, 0.0, 0.0], &[]);
        key_with_seconds(&mut out, 23, 59, 59, 7, palette, [0.0, 0.0, 0.0, 0.0], &[]);

        out.extend_from_slice(&1_u32.to_le_bytes());
        out.extend_from_slice(&3_u32.to_le_bytes());
        for word in date(23, 59, 0) {
            out.extend_from_slice(&word.to_le_bytes());
        }
        for word in date(0, 2, 0) {
            out.extend_from_slice(&word.to_le_bytes());
        }
        key_with_seconds(
            &mut out,
            0,
            0,
            0,
            3,
            palette,
            [0.0, 0.0, 2.0, 1.0],
            &[b"rain"],
        );
        key_with_seconds(
            &mut out,
            6,
            0,
            0,
            7,
            palette,
            [0.0, 0.0, 2.0, 1.0],
            &[b"future"],
        );
        key_with_seconds(
            &mut out,
            12,
            0,
            0,
            4,
            palette,
            [0.0, 0.0, 0.0, 0.0],
            &[b"stop"],
        );

        for word in [1_u32, 0, 0, 12, 0, 0, 0, 0] {
            out.extend_from_slice(&word.to_le_bytes());
        }
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&1_u32.to_le_bytes());
        out
    }

    #[test]
    fn parses_exact_v5_layout_and_rejects_trailing_bytes() {
        let palette = [0xFF00_0000_u32; KEY_COLOR_COUNT];
        let bytes = document(1, |out| {
            key(out, 0, 0, 3, palette, [0.0, 0.0, 0.0, 1.0], &[b"rain"])
        });
        let parsed = TypedAtmosphere::parse(&bytes).expect("v5 parse");
        assert_eq!(parsed.marker, -1);
        assert_eq!(parsed.version, 5);
        assert_eq!(parsed.tracks.len(), 1);
        assert_eq!(parsed.tracks[0].duration_seconds, 900);
        assert_eq!(parsed.tracks[0].keys[0].references[0].as_str(), "rain");
        assert_eq!(parsed.trailer.date.hours(), 1);
        assert!(matches!(
            TypedAtmosphere::parse(&[bytes, vec![0]].concat()),
            Err(AtmosphereError::Decode(DecodeError::TrailingBytes { .. }))
        ));
    }

    #[test]
    fn maps_original_sky_color_order_and_interpolates() {
        let first_palette = std::array::from_fn(|i| 0xFF00_0000_u32 | i as u32);
        let second_palette = std::array::from_fn(|i| 0xFF00_1000_u32 | (i as u32) << 4);
        let bytes = document(2, |out| {
            key(out, 0, 0, 7, first_palette, [0.0, 0.0, 0.0, 0.0], &[]);
            key(out, 12, 0, 7, second_palette, [1.0, 2.0, 3.0, 1.0], &[]);
        });
        let parsed = TypedAtmosphere::parse(&bytes).expect("parse");
        let start = parsed.sample_day(0, 0.0);
        assert_eq!(start.sky.colors[0].raw(), 0xFF44_5566);
        assert_eq!(start.sky.colors[1].raw(), 0xFF77_8899);
        assert_eq!(start.sky.colors[2].raw(), 0xFF11_2233);
        assert_eq!(start.sky.colors[4].raw(), first_palette[0]);
        assert_eq!(start.sky.colors[5].raw(), first_palette[3]);
        assert_eq!(start.sky.colors[8].raw(), first_palette[4]);
        assert_eq!(start.sky.colors[12].raw(), first_palette[8]);
        assert_eq!(start.sun.intensity, 0.0);
        let middle = parsed.sample_day(0, 6.0 * 3_600.0);
        assert!((middle.sun.intensity - 1.5).abs() < 0.01);
        assert_eq!(middle.rain.intensity, 0.0);
    }

    #[test]
    fn sun_packed_slots_feed_native_flare_alpha_and_secondary_light() {
        let mut palette = [0xFF00_0000_u32; KEY_COLOR_COUNT];
        palette[9] = 0x4011_2233;
        palette[10] = 0xFFAA_BBCC;
        palette[12] = 0xFFFF_FFFF;
        palette[14] = 0x8044_5566;
        let bytes = document(2, |out| {
            key(out, 0, 0, 0, palette, [0.0, 0.0, 1.5, 0.0], &[]);
            key(out, 12, 0, 1, palette, [0.0, 0.0, 1.5, 0.0], &[]);
        });
        let mut parsed = TypedAtmosphere::parse(&bytes).expect("parse");
        for key in &mut parsed.tracks[0].keys {
            key.strings[0] = AtmosphereString {
                raw: b"sun".to_vec(),
                text: "sun".to_owned(),
            };
        }

        let sampled = parsed.sample_day(0, 0.0);
        assert!(sampled.sun.active);
        assert_eq!(sampled.sun.packed[0], PackedColor(palette[9]));
        assert_eq!(sampled.sun.packed[1], PackedColor(palette[14]));

        let birth = sampled.sun.birth.expect("sun birth");
        let sun = crate::sky::SunFrame::from_sample(
            &sampled.sun,
            0.0,
            crate::sky::SunConfig::from_birth(birth),
        );
        assert_eq!(sun.sprite_color, PackedColor(palette[9]).rgba());
        assert_eq!(sun.secondary_color, PackedColor(palette[14]).rgba()[..3]);

        let optics = crate::sky::sun_optics(
            sun,
            crate::sky::SunVisibility {
                projected_sun: [50.0, 50.0],
                viewport: [100.0, 100.0],
                view_forward: sun.direction,
                unoccluded: true,
            },
        )
        .expect("sun optics");
        assert!(optics.flares[0].active);
        assert!((optics.flares[0].color[3] - 64.0 / 255.0).abs() < 1.0e-6);
    }

    #[test]
    fn wraps_last_key_to_first_and_emits_events() {
        let palette = [0xFF00_0000_u32; KEY_COLOR_COUNT];
        let bytes = document(2, |out| {
            key(out, 6, 0, 3, palette, [0.0, 0.0, 0.0, 0.0], &[b"rain"]);
            key(out, 12, 0, 4, palette, [1.0, 1.0, 1.0, 0.0], &[]);
        });
        let parsed = TypedAtmosphere::parse(&bytes).expect("parse");
        let near_midnight = parsed.sample_day(0, 23.0 * 3_600.0);
        assert!(!near_midnight.rain.active);
        let events = parsed.events_between(0.0, 300.0);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, AtmosphereKind::RainStart);
        let wrapped = parsed.events_between(900.0, 1_300.0);
        assert_eq!(wrapped.len(), 1);
        assert_eq!(wrapped[0].kind, AtmosphereKind::RainStart);
    }

    #[test]
    fn samples_across_tracks_uses_integer_key_times_and_trailer_offset() {
        let palette = [0xFF00_0000_u32; KEY_COLOR_COUNT];
        let parsed = TypedAtmosphere::parse(&two_track_document(palette)).expect("parse");

        assert_eq!(parsed.tracks[0].duration_seconds, 60);
        assert_eq!(parsed.tracks[1].duration_seconds, 120);
        assert_eq!(parsed.initial_offset_seconds(), 120);
        assert_eq!(parsed.timeline()[1].real_seconds, 59.0);

        let before_transition = parsed.sample(59.5);
        assert_eq!(before_transition.track_index, 0);
        assert!((before_transition.sun.intensity - 1.0).abs() < 0.01);

        let active = parsed.sample_day(1, 6.0 * 3_600.0);
        assert_eq!(active.real_seconds, 90.0);
        assert!(active.rain.active);
        assert_eq!(active.rain.resources.len(), 1);
        assert_eq!(active.rain.resources[0].as_str(), "rain");
        assert!(parsed
            .events_between(59.0, 61.0)
            .iter()
            .any(|event| event.kind == AtmosphereKind::RainStart));
    }

    #[test]
    fn repeating_schedule_shifts_celestial_birth_with_absolute_clock() {
        let palette = [0xFF00_0000_u32; KEY_COLOR_COUNT];
        let bytes = document(2, |out| {
            key(out, 0, 0, 0, palette, [0.0, 0.0, 1.0, 0.0], &[]);
            key(out, 12, 0, 1, palette, [0.0, 0.0, 1.0, 0.0], &[]);
        });
        let mut parsed = TypedAtmosphere::parse(&bytes).expect("parse");
        for (key, name) in parsed.tracks[0].keys.iter_mut().zip(["sun", "sun"]) {
            key.strings[0] = AtmosphereString {
                raw: name.as_bytes().to_vec(),
                text: name.to_owned(),
            };
        }

        let total = parsed.total_duration_seconds();
        let first = parsed.sample(300.0);
        let later = parsed.sample(300.0 + total);
        let first_birth = first.sun.birth.expect("sun birth");
        let later_birth = later.sun.birth.expect("wrapped sun birth");
        assert!((later_birth.start_seconds - first_birth.start_seconds - total).abs() < 1.0e-5);
        assert_eq!(first_birth.lifetime_seconds, later_birth.lifetime_seconds);
        assert_eq!(
            first_birth.initial_angles_degrees,
            later_birth.initial_angles_degrees
        );
        assert_eq!(first_birth.material_row, later_birth.material_row);

        let first_frame = crate::sky::sun_frame(
            &first.sun,
            300.0,
            crate::sky::SunConfig::from_birth(first_birth),
        );
        let later_frame = crate::sky::sun_frame(
            &later.sun,
            300.0 + total,
            crate::sky::SunConfig::from_birth(later_birth),
        );
        assert!((first_frame.age - later_frame.age).abs() < 1.0e-5);
        for (first, later) in first_frame.direction.iter().zip(later_frame.direction) {
            assert!((*first - later).abs() < 1.0e-5);
        }
    }

    #[test]
    fn rejects_non_finite_key_values() {
        let palette = [0xFF00_0000_u32; KEY_COLOR_COUNT];
        let mut bytes = document(1, |out| {
            key(out, 0, 0, 7, palette, [f32::NAN, 0.0, 0.0, 0.0], &[])
        });
        assert!(matches!(
            TypedAtmosphere::parse(&bytes),
            Err(AtmosphereError::NonFiniteValue {
                field: "parameters",
                index: 0,
                ..
            })
        ));
        bytes[0] = 0;
        assert!(matches!(
            TypedAtmosphere::parse(&bytes),
            Err(AtmosphereError::InvalidHeader { .. })
        ));
    }

    #[test]
    #[ignore = "requires the original GOG installation"]
    fn parses_all_original_gog_sky_files() {
        let root = std::env::var_os("PARKAN_GOG_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\GOG Games\Parkan - Iron Strategy"));
        let mission_root = root.join("MISSIONS");
        let mut files = Vec::new();
        collect_ske(&mission_root, &mut files);
        files.sort();
        assert_eq!(files.len(), 29);
        for path in files {
            let bytes = std::fs::read(&path).expect("read sky.ske");
            let parsed = TypedAtmosphere::parse(&bytes)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            assert!(parsed.total_duration_seconds().is_finite());
            assert!(!parsed.timeline().is_empty(), "{}", path.display());
            let materials = std::fs::read(path.with_file_name("sky.wea"))
                .ok()
                .and_then(|bytes| crate::sky::SkyMaterials::parse(&bytes).ok())
                .unwrap_or_default();
            let mut sky = crate::sky::SkySystem::new(Default::default(), materials)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            for second in [0.0, 0.5, 900.0, 86_399.0, 172_800.0] {
                let frame = parsed.sample(second);
                assert!(frame.sky.values.iter().all(|value| value.is_finite()));
                assert!(frame.sun.rgb.iter().all(|value| value.is_finite()));
                assert!(frame.rain.intensity.is_finite());
                assert!(frame.snow.intensity.is_finite());
                assert!(frame.lightning.intensity.is_finite());
                let sky_frame = sky
                    .update(&frame, second)
                    .unwrap_or_else(|error| panic!("{} at {second}: {error}", path.display()));
                assert_eq!(sky_frame.directional_lights.len(), 4);
                let optics = sky_frame
                    .sun_optics(crate::sky::SunVisibility {
                        projected_sun: [640.0, 360.0],
                        viewport: [1280.0, 720.0],
                        view_forward: [0.0, 0.0, 1.0],
                        unoccluded: true,
                    })
                    .expect("native sky optics");
                assert_eq!(optics.flares.len(), 12);
            }
        }
    }

    fn collect_ske(root: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(root) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_ske(&path, out);
            } else if path.file_name().is_some_and(|name| name == "sky.ske") {
                out.push(path);
            }
        }
    }
}
