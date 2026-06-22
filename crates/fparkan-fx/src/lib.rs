#![forbid(unsafe_code)]
//! FXID effect contracts.

use fparkan_binary::{Cursor, DecodeError};
use std::sync::Arc;

/// `FXID` `NRes` entry type.
pub const FXID_KIND: u32 = 0x4449_5846;
const HEADER_SIZE: usize = 60;

/// FX document.
#[derive(Clone, Debug)]
pub struct FxDocument {
    bytes: Arc<[u8]>,
    header: FxHeader,
    commands: Vec<FxCommand>,
}

/// FX header.
#[derive(Clone, Debug, PartialEq)]
pub struct FxHeader {
    /// Number of commands in the stream.
    pub command_count: u32,
    /// Time mode.
    pub time_mode: u32,
    /// Duration in seconds.
    pub duration_seconds: f32,
    /// Phase jitter.
    pub phase_jitter: f32,
    /// Opaque flags.
    pub flags: u32,
    /// Opaque settings id.
    pub settings_id: u32,
    /// Random spatial shift.
    pub random_shift: [f32; 3],
    /// Local pivot.
    pub pivot: [f32; 3],
    /// Base scale.
    pub scale: [f32; 3],
}

/// FX opcode.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum FxOpcode {
    /// Opcode 1.
    Op1,
    /// Opcode 2.
    Op2,
    /// Opcode 3.
    Op3,
    /// Opcode 4.
    Op4,
    /// Opcode 5.
    Op5,
    /// Opcode 6.
    Op6,
    /// Opcode 7.
    Op7,
    /// Opcode 8.
    Op8,
    /// Opcode 9.
    Op9,
    /// Opcode 10.
    Op10,
}

/// FX resource reference.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FxResourceRef {
    /// Fixed archive field bytes.
    pub archive_raw: [u8; 32],
    /// Fixed name field bytes.
    pub name_raw: [u8; 32],
}

/// FX command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FxCommand {
    /// Raw command word.
    pub word: u32,
    /// Decoded opcode.
    pub opcode: FxOpcode,
    /// Enabled bit.
    pub enabled: bool,
    /// Command body after the word.
    pub raw_body: Arc<[u8]>,
    /// Resource references discovered in known command layouts.
    pub resource_refs: Vec<FxResourceRef>,
}

/// FX instance id.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FxInstanceId(pub u64);

/// FX seed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FxSeed(pub u64);

/// External transform snapshot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    /// Translation.
    pub translation: [f32; 3],
    /// Rotation quaternion.
    pub rotation: [f32; 4],
    /// Scale.
    pub scale: [f32; 3],
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            translation: [0.0; 3],
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0; 3],
        }
    }
}

/// Game time in ticks.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GameTime(pub u64);

/// FX runtime state.
#[derive(Clone, Debug)]
pub struct FxState {
    /// Instance id.
    pub id: FxInstanceId,
    /// Source document.
    pub document: Arc<FxDocument>,
    /// Seed.
    pub seed: FxSeed,
    /// Transform at creation time.
    pub transform: Transform,
    /// Last updated time.
    pub time: GameTime,
    /// RNG call count reserved for deterministic captures.
    pub rng_calls: u64,
    /// Lifecycle phase.
    pub lifecycle: FxLifecycle,
}

/// FX lifecycle phase.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FxLifecycle {
    /// Running and eligible to emit.
    #[default]
    Running,
    /// Stopped and not eligible to emit.
    Stopped,
    /// Ended permanently for the current instance.
    Ended,
}

/// Visual FX emission produced from a command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FxPrimitive {
    /// Command index.
    pub command_index: u32,
    /// Opcode.
    pub opcode: FxOpcode,
}

/// Sound FX emission produced from a command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FxSoundEvent {
    /// Command index.
    pub command_index: u32,
}

/// FX emission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FxEmission {
    /// Visual primitive.
    Primitive(FxPrimitive),
    /// Sound event.
    Sound(FxSoundEvent),
}

/// FX decode/runtime error.
#[derive(Debug)]
pub enum FxError {
    /// Binary decode error.
    Decode(DecodeError),
    /// Unknown opcode.
    UnknownOpcode {
        /// Command index.
        index: u32,
        /// Raw opcode byte.
        opcode: u8,
    },
    /// Command stream exceeds payload.
    CommandOutOfBounds {
        /// Command index.
        index: u32,
        /// Expected command end.
        expected_end: u64,
        /// Payload size.
        payload_size: u64,
    },
    /// Resource reference cannot be framed from body.
    InvalidResourceRef {
        /// Command index.
        index: u32,
        /// Opcode.
        opcode: FxOpcode,
    },
    /// A referenced dependency is missing.
    MissingDependency {
        /// Effect name or stable effect id.
        effect: String,
        /// Command index.
        command_index: u32,
        /// Archive name.
        archive: String,
        /// Resource name.
        name: String,
    },
}

impl From<DecodeError> for FxError {
    fn from(value: DecodeError) -> Self {
        Self::Decode(value)
    }
}

impl std::fmt::Display for FxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Decode(source) => write!(f, "{source}"),
            Self::UnknownOpcode { index, opcode } => {
                write!(f, "unknown FX opcode {opcode} at command {index}")
            }
            Self::CommandOutOfBounds {
                index,
                expected_end,
                payload_size,
            } => write!(
                f,
                "FX command {index} out of bounds: expected_end={expected_end}, payload_size={payload_size}"
            ),
            Self::InvalidResourceRef { index, opcode } => {
                write!(f, "invalid FX resource reference in command {index} ({opcode:?})")
            }
            Self::MissingDependency {
                effect,
                command_index,
                archive,
                name,
            } => write!(
                f,
                "missing FX dependency: effect={effect}, command={command_index}, archive={archive}, name={name}"
            ),
        }
    }
}

impl std::error::Error for FxError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Decode(source) => Some(source),
            Self::UnknownOpcode { .. }
            | Self::CommandOutOfBounds { .. }
            | Self::InvalidResourceRef { .. }
            | Self::MissingDependency { .. } => None,
        }
    }
}

/// Decodes an `FXID` payload.
///
/// # Errors
///
/// Returns [`FxError`] when the 60-byte header, fixed-size command stream, or
/// exact EOF framing is invalid.
pub fn decode_fxid(bytes: Arc<[u8]>) -> Result<FxDocument, FxError> {
    let mut cursor = Cursor::new(&bytes);
    let header = read_header(&mut cursor)?;
    debug_assert_eq!(cursor.offset(), HEADER_SIZE as u64);
    let mut commands = Vec::with_capacity(
        usize::try_from(header.command_count)
            .map_err(|_| FxError::Decode(DecodeError::IntegerOverflow))?,
    );
    for index in 0..header.command_count {
        let start = cursor.offset();
        let word = cursor.read_u32_le()?;
        let opcode_byte = (word & 0xFF) as u8;
        let opcode = opcode_from_byte(opcode_byte).ok_or(FxError::UnknownOpcode {
            index,
            opcode: opcode_byte,
        })?;
        let command_size = command_size(opcode);
        let expected_end = start
            .checked_add(u64::try_from(command_size).map_err(|_| DecodeError::IntegerOverflow)?)
            .ok_or(DecodeError::IntegerOverflow)?;
        if expected_end > bytes.len() as u64 {
            return Err(FxError::CommandOutOfBounds {
                index,
                expected_end,
                payload_size: bytes.len() as u64,
            });
        }
        let body_size = command_size
            .checked_sub(4)
            .ok_or(DecodeError::IntegerOverflow)?;
        let body = cursor.read_exact(body_size)?;
        let raw_body = Arc::from(body.to_vec().into_boxed_slice());
        let resource_refs = resource_refs(index, opcode, body)?;
        commands.push(FxCommand {
            word,
            opcode,
            enabled: ((word >> 8) & 1) != 0,
            raw_body,
            resource_refs,
        });
    }
    cursor.require_eof()?;
    Ok(FxDocument {
        bytes,
        header,
        commands,
    })
}

/// Creates an FX instance.
///
/// # Errors
///
/// Currently returns [`FxError`] only for future resource/lifecycle validation
/// hooks; creation is deterministic for a decoded document.
pub fn create_instance(
    document: Arc<FxDocument>,
    seed: FxSeed,
    transform: Transform,
) -> Result<FxState, FxError> {
    Ok(FxState {
        id: FxInstanceId(seed.0),
        document,
        seed,
        transform,
        time: GameTime::default(),
        rng_calls: 0,
        lifecycle: FxLifecycle::Running,
    })
}

/// Updates FX simulation time without emitting side effects.
///
/// # Errors
///
/// Reserved for future runtime validation.
pub fn update(state: &mut FxState, time: GameTime) -> Result<(), FxError> {
    state.time = time;
    Ok(())
}

/// Emits active commands without advancing state.
///
/// # Errors
///
/// Reserved for future resource/runtime validation.
pub fn emit(state: &FxState, out: &mut Vec<FxEmission>) -> Result<(), FxError> {
    if state.lifecycle != FxLifecycle::Running {
        return Ok(());
    }
    for (index, command) in state.document.commands.iter().enumerate() {
        if !command.enabled {
            continue;
        }
        let command_index = u32::try_from(index).map_err(|_| DecodeError::IntegerOverflow)?;
        if command.opcode == FxOpcode::Op2 {
            out.push(FxEmission::Sound(FxSoundEvent { command_index }));
        } else {
            out.push(FxEmission::Primitive(FxPrimitive {
                command_index,
                opcode: command.opcode,
            }));
        }
    }
    Ok(())
}

/// Stops an FX instance.
pub fn stop(state: &mut FxState) {
    state.lifecycle = FxLifecycle::Stopped;
}

/// Restarts a stopped FX instance from a time.
pub fn restart(state: &mut FxState, time: GameTime) {
    state.lifecycle = FxLifecycle::Running;
    state.time = time;
}

/// Ends an FX instance permanently.
pub fn end(state: &mut FxState) {
    state.lifecycle = FxLifecycle::Ended;
}

/// Validates resource references through a caller-provided dependency probe.
///
/// # Errors
///
/// Returns [`FxError::MissingDependency`] with effect, command, archive and
/// resource name context when the probe reports a missing resource.
pub fn validate_dependencies(
    document: &FxDocument,
    effect: &str,
    exists: impl Fn(&[u8], &[u8]) -> bool,
) -> Result<(), FxError> {
    for (index, command) in document.commands.iter().enumerate() {
        for reference in &command.resource_refs {
            if !exists(reference.archive_name(), reference.resource_name()) {
                return Err(FxError::MissingDependency {
                    effect: effect.to_string(),
                    command_index: u32::try_from(index)
                        .map_err(|_| DecodeError::IntegerOverflow)?,
                    archive: String::from_utf8_lossy(reference.archive_name()).into_owned(),
                    name: String::from_utf8_lossy(reference.resource_name()).into_owned(),
                });
            }
        }
    }
    Ok(())
}

/// Builds a byte-stable capture for emitted commands.
///
/// # Errors
///
/// Returns [`FxError`] when emission fails.
pub fn canonical_emission_capture(state: &FxState) -> Result<Vec<u8>, FxError> {
    let mut emissions = Vec::new();
    emit(state, &mut emissions)?;
    let mut out = Vec::new();
    for emission in emissions {
        match emission {
            FxEmission::Primitive(primitive) => {
                out.extend_from_slice(
                    format!("P,{}, {:?}\n", primitive.command_index, primitive.opcode).as_bytes(),
                );
            }
            FxEmission::Sound(sound) => {
                out.extend_from_slice(format!("S,{}\n", sound.command_index).as_bytes());
            }
        }
    }
    Ok(out)
}

impl FxDocument {
    /// Returns original bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the parsed header.
    #[must_use]
    pub fn header(&self) -> &FxHeader {
        &self.header
    }

    /// Returns commands in disk order.
    #[must_use]
    pub fn commands(&self) -> &[FxCommand] {
        &self.commands
    }
}

impl FxResourceRef {
    /// Archive name before first NUL, ASCII-trimmed.
    #[must_use]
    pub fn archive_name(&self) -> &[u8] {
        bounded_cstr(&self.archive_raw)
    }

    /// Resource name before first NUL, ASCII-trimmed.
    #[must_use]
    pub fn resource_name(&self) -> &[u8] {
        bounded_cstr(&self.name_raw)
    }
}

fn read_header(cursor: &mut Cursor<'_>) -> Result<FxHeader, FxError> {
    Ok(FxHeader {
        command_count: cursor.read_u32_le()?,
        time_mode: cursor.read_u32_le()?,
        duration_seconds: cursor.read_f32_le()?,
        phase_jitter: cursor.read_f32_le()?,
        flags: cursor.read_u32_le()?,
        settings_id: cursor.read_u32_le()?,
        random_shift: [
            cursor.read_f32_le()?,
            cursor.read_f32_le()?,
            cursor.read_f32_le()?,
        ],
        pivot: [
            cursor.read_f32_le()?,
            cursor.read_f32_le()?,
            cursor.read_f32_le()?,
        ],
        scale: [
            cursor.read_f32_le()?,
            cursor.read_f32_le()?,
            cursor.read_f32_le()?,
        ],
    })
}

fn opcode_from_byte(opcode: u8) -> Option<FxOpcode> {
    match opcode {
        1 => Some(FxOpcode::Op1),
        2 => Some(FxOpcode::Op2),
        3 => Some(FxOpcode::Op3),
        4 => Some(FxOpcode::Op4),
        5 => Some(FxOpcode::Op5),
        6 => Some(FxOpcode::Op6),
        7 => Some(FxOpcode::Op7),
        8 => Some(FxOpcode::Op8),
        9 => Some(FxOpcode::Op9),
        10 => Some(FxOpcode::Op10),
        _ => None,
    }
}

fn command_size(opcode: FxOpcode) -> usize {
    match opcode {
        FxOpcode::Op1 => 224,
        FxOpcode::Op2 => 148,
        FxOpcode::Op3 => 200,
        FxOpcode::Op4 => 204,
        FxOpcode::Op5 => 112,
        FxOpcode::Op6 => 4,
        FxOpcode::Op7 | FxOpcode::Op9 | FxOpcode::Op10 => 208,
        FxOpcode::Op8 => 248,
    }
}

fn resource_refs(index: u32, opcode: FxOpcode, body: &[u8]) -> Result<Vec<FxResourceRef>, FxError> {
    if !has_resource_ref(opcode) {
        return Ok(Vec::new());
    }
    let raw = body
        .get(..64)
        .ok_or(FxError::InvalidResourceRef { index, opcode })?;
    let mut archive_raw = [0; 32];
    let mut name_raw = [0; 32];
    archive_raw.copy_from_slice(&raw[..32]);
    name_raw.copy_from_slice(&raw[32..64]);
    Ok(vec![FxResourceRef {
        archive_raw,
        name_raw,
    }])
}

fn has_resource_ref(opcode: FxOpcode) -> bool {
    matches!(
        opcode,
        FxOpcode::Op2
            | FxOpcode::Op3
            | FxOpcode::Op4
            | FxOpcode::Op5
            | FxOpcode::Op7
            | FxOpcode::Op8
            | FxOpcode::Op9
            | FxOpcode::Op10
    )
}

fn bounded_cstr(raw: &[u8]) -> &[u8] {
    let len = raw.iter().position(|byte| *byte == 0).unwrap_or(raw.len());
    trim_ascii(&raw[..len])
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let mut start = 0usize;
    let mut end = bytes.len();
    while start < end && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    &bytes[start..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_nres::ReadProfile;
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    #[test]
    fn decodes_synthetic_opcodes_and_refs() {
        let mut bytes = header(2);
        bytes.extend_from_slice(&command_with_ref(0x0102, 148, b"sounds.lib", b"boom.wav"));
        bytes.extend_from_slice(&command(0x0106, 4));
        let document = decode_fxid(Arc::from(bytes.into_boxed_slice())).expect("fx");

        assert_eq!(document.header().command_count, 2);
        assert_eq!(document.commands()[0].opcode, FxOpcode::Op2);
        assert!(document.commands()[0].enabled);
        assert_eq!(
            document.commands()[0].resource_refs[0].archive_name(),
            b"sounds.lib"
        );
        assert_eq!(document.commands()[1].opcode, FxOpcode::Op6);
        assert!(document.commands()[1].raw_body.is_empty());
    }

    #[test]
    fn header_is_exactly_sixty_bytes_and_command_sizes_are_fixed() {
        let mut bytes = header(10);
        for opcode in 1..=10_u32 {
            bytes.extend_from_slice(&command(0x0100 | opcode, opcode_size(opcode)));
        }
        let document = decode_fxid(Arc::from(bytes.into_boxed_slice())).expect("fx");

        assert_eq!(header(0).len(), HEADER_SIZE);
        assert_eq!(document.commands().len(), 10);
        for (index, command) in document.commands().iter().enumerate() {
            let opcode = u32::try_from(index + 1).expect("opcode");
            assert_eq!(command.raw_body.len() + 4, opcode_size(opcode));
        }
    }

    #[test]
    fn opcode6_four_byte_command_is_accepted() {
        let mut bytes = header(1);
        bytes.extend_from_slice(&command(0x0106, 4));
        let document = decode_fxid(Arc::from(bytes.into_boxed_slice())).expect("fx");

        assert_eq!(document.commands()[0].opcode, FxOpcode::Op6);
        assert!(document.commands()[0].raw_body.is_empty());
    }

    #[test]
    fn rejects_unknown_opcode_at_command_index() {
        let mut bytes = header(1);
        bytes.extend_from_slice(&99_u32.to_le_bytes());
        let err = decode_fxid(Arc::from(bytes.into_boxed_slice())).expect_err("unknown opcode");

        assert!(matches!(
            err,
            FxError::UnknownOpcode {
                index: 0,
                opcode: 99
            }
        ));
    }

    #[test]
    fn rejects_command_count_that_exceeds_payload() {
        let mut bytes = header(2);
        bytes.extend_from_slice(&command(0x0106, 4));
        let err = decode_fxid(Arc::from(bytes.into_boxed_slice())).expect_err("out of bounds");

        assert!(matches!(
            err,
            FxError::Decode(DecodeError::UnexpectedEof { .. }) | FxError::CommandOutOfBounds { .. }
        ));
    }

    #[test]
    fn rejects_trailing_bytes_after_command_stream() {
        let mut bytes = header(0);
        bytes.push(0);
        let err = decode_fxid(Arc::from(bytes.into_boxed_slice())).expect_err("trailing");

        assert!(matches!(
            err,
            FxError::Decode(DecodeError::TrailingBytes { .. })
        ));
    }

    #[test]
    fn fixed_resource_refs_preserve_tails() {
        let mut bytes = header(1);
        let mut command = command_with_ref(0x0102, 148, b"sounds.lib", b"boom.wav");
        command[4 + 20] = 0xAB;
        command[36 + 20] = 0xCD;
        bytes.extend_from_slice(&command);
        let document = decode_fxid(Arc::from(bytes.into_boxed_slice())).expect("fx");

        let reference = &document.commands()[0].resource_refs[0];
        assert_eq!(reference.archive_name(), b"sounds.lib");
        assert_eq!(reference.resource_name(), b"boom.wav");
        assert_eq!(reference.archive_raw[20], 0xAB);
        assert_eq!(reference.name_raw[20], 0xCD);
    }

    #[test]
    fn missing_dependency_error_contains_effect_command_archive_and_name() {
        let mut bytes = header(1);
        bytes.extend_from_slice(&command_with_ref(
            0x0102,
            148,
            b"sounds.lib",
            b"missing.wav",
        ));
        let document = decode_fxid(Arc::from(bytes.into_boxed_slice())).expect("fx");

        let err = validate_dependencies(&document, "spark", |_, _| false)
            .expect_err("missing dependency");

        assert!(matches!(
            err,
            FxError::MissingDependency {
                ref effect,
                command_index: 0,
                ref archive,
                ref name,
            } if effect == "spark" && archive == "sounds.lib" && name == "missing.wav"
        ));
        assert!(err.to_string().contains("spark"));
        assert!(err.to_string().contains("missing.wav"));
    }

    #[test]
    fn update_and_emit_are_separate() {
        let mut bytes = header(1);
        bytes.extend_from_slice(&command(0x0101, 224));
        let document = Arc::new(decode_fxid(Arc::from(bytes.into_boxed_slice())).expect("fx"));
        let mut state = create_instance(document, FxSeed(7), Transform::default()).expect("state");
        update(&mut state, GameTime(42)).expect("update");
        let before = state.time;
        let mut out = Vec::new();

        emit(&state, &mut out).expect("emit");

        assert_eq!(state.time, before);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn create_records_seed_transform_and_start_time() {
        let bytes = header(0);
        let document = Arc::new(decode_fxid(Arc::from(bytes.into_boxed_slice())).expect("fx"));
        let transform = Transform {
            translation: [1.0, 2.0, 3.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [4.0, 5.0, 6.0],
        };

        let state = create_instance(document, FxSeed(77), transform).expect("state");

        assert_eq!(state.id, FxInstanceId(77));
        assert_eq!(state.seed, FxSeed(77));
        assert_eq!(state.transform, transform);
        assert_eq!(state.time, GameTime(0));
        assert_eq!(state.rng_calls, 0);
        assert_eq!(state.lifecycle, FxLifecycle::Running);
    }

    #[test]
    fn stable_command_order_and_emission_capture_are_seed_stable() {
        let mut bytes = header(3);
        bytes.extend_from_slice(&command(0x0101, 224));
        bytes.extend_from_slice(&command(0x0102, 148));
        bytes.extend_from_slice(&command(0x0106, 4));
        let document = Arc::new(decode_fxid(Arc::from(bytes.into_boxed_slice())).expect("fx"));
        let mut first =
            create_instance(document.clone(), FxSeed(5), Transform::default()).expect("first");
        let mut second =
            create_instance(document, FxSeed(5), Transform::default()).expect("second");

        update(&mut first, GameTime(9)).expect("update");
        update(&mut second, GameTime(9)).expect("update");

        assert_eq!(
            canonical_emission_capture(&first).expect("first capture"),
            canonical_emission_capture(&second).expect("second capture")
        );
        assert_eq!(
            canonical_emission_capture(&first).expect("capture"),
            b"P,0, Op1\nS,1\nP,2, Op6\n"
        );
    }

    #[test]
    fn stop_restart_end_lifecycle_controls_emission() {
        let mut bytes = header(1);
        bytes.extend_from_slice(&command(0x0101, 224));
        let document = Arc::new(decode_fxid(Arc::from(bytes.into_boxed_slice())).expect("fx"));
        let mut state = create_instance(document, FxSeed(1), Transform::default()).expect("state");

        assert!(!canonical_emission_capture(&state)
            .expect("running")
            .is_empty());
        stop(&mut state);
        assert_eq!(state.lifecycle, FxLifecycle::Stopped);
        assert!(canonical_emission_capture(&state)
            .expect("stopped")
            .is_empty());
        restart(&mut state, GameTime(12));
        assert_eq!(state.lifecycle, FxLifecycle::Running);
        assert_eq!(state.time, GameTime(12));
        assert!(!canonical_emission_capture(&state)
            .expect("restarted")
            .is_empty());
        end(&mut state);
        assert_eq!(state.lifecycle, FxLifecycle::Ended);
        assert!(canonical_emission_capture(&state)
            .expect("ended")
            .is_empty());
    }

    #[test]
    fn unrelated_rng_stream_use_does_not_perturb_fx_capture() {
        let mut bytes = header(1);
        bytes.extend_from_slice(&command(0x0101, 224));
        let document = Arc::new(decode_fxid(Arc::from(bytes.into_boxed_slice())).expect("fx"));
        let state = create_instance(document, FxSeed(3), Transform::default()).expect("state");
        let before = canonical_emission_capture(&state).expect("before");

        let mut unrelated = 0x1234_u64;
        for _ in 0..32 {
            unrelated = unrelated.rotate_left(7).wrapping_mul(17);
        }

        assert_ne!(unrelated, 0);
        assert_eq!(canonical_emission_capture(&state).expect("after"), before);
    }

    #[test]
    fn arbitrary_command_streams_are_bounded_and_panic_free() {
        for len in 0..256usize {
            let mut bytes = vec![0xA5; len];
            if len >= HEADER_SIZE {
                bytes[0..4].copy_from_slice(&1_u32.to_le_bytes());
            }
            let result = std::panic::catch_unwind(|| {
                let _ = decode_fxid(Arc::from(bytes.into_boxed_slice()));
            });
            assert!(result.is_ok());
        }
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_corpus_fxid_exact_eof_and_distribution() {
        for (corpus, expected_count) in [("IS", 923_usize), ("IS2", 1065_usize)] {
            let Some(root) = corpus_root(corpus) else {
                continue;
            };
            let mut count = 0usize;
            let mut opcodes = BTreeMap::<FxOpcode, usize>::new();
            let mut time_modes = BTreeMap::<u32, usize>::new();
            for path in files_under(&root) {
                let Ok(bytes) = std::fs::read(&path) else {
                    continue;
                };
                let Ok(archive) = fparkan_nres::decode(
                    Arc::from(bytes.into_boxed_slice()),
                    ReadProfile::Compatible,
                ) else {
                    continue;
                };
                for entry in archive
                    .entries()
                    .iter()
                    .filter(|entry| entry.meta().type_id == FXID_KIND)
                {
                    let payload = archive.payload(entry.id()).expect("payload");
                    let document = decode_fxid(Arc::from(payload.to_vec().into_boxed_slice()))
                        .unwrap_or_else(|err| {
                            panic!("{corpus} {path:?} {:?}: {err}", entry.name_bytes())
                        });
                    count += 1;
                    *time_modes.entry(document.header().time_mode).or_insert(0) += 1;
                    for command in document.commands() {
                        *opcodes.entry(command.opcode).or_insert(0) += 1;
                    }
                }
            }

            assert_eq!(count, expected_count, "{corpus} FXID count");
            assert!(!opcodes.contains_key(&FxOpcode::Op6), "{corpus} opcode 6");
            for mode in time_modes.keys() {
                assert!(
                    matches!(*mode, 0 | 1 | 2 | 4 | 5 | 14 | 15 | 16 | 17),
                    "{corpus} unexpected time mode {mode}"
                );
            }
        }
    }

    #[test]
    #[ignore = "requires licensed corpus"]
    fn licensed_corpus_fxid_emission_captures_are_approved() {
        for (corpus, expected_count, expected_emitting, expected_hash) in [
            ("IS", 923_usize, 467_usize, 10_553_431_922_547_057_702_u64),
            ("IS2", 1065_usize, 532_usize, 9_217_284_592_334_143_531_u64),
        ] {
            let Some(root) = corpus_root(corpus) else {
                continue;
            };
            let mut count = 0usize;
            let mut emitting = 0usize;
            let mut hash = FNV_OFFSET;
            for path in files_under(&root) {
                let Ok(bytes) = std::fs::read(&path) else {
                    continue;
                };
                let Ok(archive) = fparkan_nres::decode(
                    Arc::from(bytes.into_boxed_slice()),
                    ReadProfile::Compatible,
                ) else {
                    continue;
                };
                for entry in archive
                    .entries()
                    .iter()
                    .filter(|entry| entry.meta().type_id == FXID_KIND)
                {
                    let payload = archive.payload(entry.id()).expect("payload");
                    let document = Arc::new(
                        decode_fxid(Arc::from(payload.to_vec().into_boxed_slice())).unwrap_or_else(
                            |err| panic!("{corpus} {path:?} {:?}: {err}", entry.name_bytes()),
                        ),
                    );
                    let state =
                        create_instance(document, FxSeed(count as u64), Transform::default())
                            .expect("fx state");
                    let capture = canonical_emission_capture(&state).expect("capture");
                    if !capture.is_empty() {
                        emitting += 1;
                    }
                    hash_bytes(&mut hash, entry.name_bytes());
                    hash_bytes(&mut hash, &capture);
                    count += 1;
                }
            }

            assert_eq!(count, expected_count, "{corpus} FXID count");
            assert_eq!(emitting, expected_emitting, "{corpus} emitting FXID count");
            assert_eq!(hash, expected_hash, "{corpus} FXID capture hash");
        }
    }

    fn header(command_count: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_SIZE);
        out.extend_from_slice(&command_count.to_le_bytes());
        out.extend_from_slice(&1_u32.to_le_bytes());
        out.extend_from_slice(&1.0_f32.to_bits().to_le_bytes());
        out.extend_from_slice(&0.0_f32.to_bits().to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        for _ in 0..9 {
            out.extend_from_slice(&0.0_f32.to_bits().to_le_bytes());
        }
        assert_eq!(out.len(), HEADER_SIZE);
        out
    }

    fn command(word: u32, size: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(size);
        out.extend_from_slice(&word.to_le_bytes());
        out.resize(size, 0);
        out
    }

    fn command_with_ref(word: u32, size: usize, archive: &[u8], name: &[u8]) -> Vec<u8> {
        let mut out = command(word, size);
        copy_cstr(&mut out[4..36], archive);
        copy_cstr(&mut out[36..68], name);
        out
    }

    fn opcode_size(opcode: u32) -> usize {
        match opcode {
            1 => 224,
            2 => 148,
            3 => 200,
            4 => 204,
            5 => 112,
            6 => 4,
            7 | 9 | 10 => 208,
            8 => 248,
            _ => unreachable!("test opcode"),
        }
    }

    fn copy_cstr(dst: &mut [u8], src: &[u8]) {
        let len = dst.len().saturating_sub(1).min(src.len());
        dst[..len].copy_from_slice(&src[..len]);
    }

    fn corpus_root(name: &str) -> Option<PathBuf> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("testdata")
            .join(name);
        root.is_dir().then_some(root)
    }

    fn files_under(root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(path) = stack.pop() {
            let Ok(read_dir) = std::fs::read_dir(path) else {
                continue;
            };
            for entry in read_dir.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    out.push(path);
                }
            }
        }
        out.sort();
        out
    }

    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
        for byte in bytes {
            *hash ^= u64::from(*byte);
            *hash = hash.wrapping_mul(FNV_PRIME);
        }
    }
}
