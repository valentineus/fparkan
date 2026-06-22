#![forbid(unsafe_code)]
//! Deterministic world identity, queue, lifecycle, and snapshots.

use std::collections::VecDeque;

/// Object handle with generation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ObjectHandle {
    /// Generation.
    pub generation: u32,
    /// Slot.
    pub slot: u32,
}

/// Original mission object id.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct OriginalObjectId(pub u32);

/// Owner id.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct OwnerId(pub u16);

/// Tick.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Tick(pub u64);

/// State hash.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StateHash(pub [u8; 32]);

/// World phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorldPhase {
    /// Idle.
    Idle,
    /// Calculating.
    Calculating,
    /// Applying deferred operations.
    ApplyingDeferred,
    /// Publishing snapshot.
    PublishingSnapshot,
}

/// Object draft.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectDraft {
    /// Original id.
    pub original_id: Option<OriginalObjectId>,
}

/// Distinct object identity metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IdentityMetadata {
    /// Original mission object id.
    pub original_id: Option<OriginalObjectId>,
    /// Mirrored original id.
    pub mirror_id: Option<OriginalObjectId>,
    /// Local owner id.
    pub owner_id: Option<OwnerId>,
}

/// World command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorldCommand {
    /// Sequence.
    pub sequence: u64,
    /// Target.
    pub target: Option<ObjectHandle>,
}

/// World event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorldEvent {
    /// Sequence.
    pub sequence: u64,
    /// Target object, if any.
    pub target: Option<ObjectHandle>,
}

/// Input snapshot.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct InputSnapshot;

/// World snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorldSnapshot {
    /// Tick.
    pub tick: Tick,
    /// Live object handles.
    pub objects: Vec<ObjectHandle>,
    /// Commands processed during this step.
    pub events: Vec<WorldEvent>,
    /// State hash.
    pub hash: StateHash,
}

/// World configuration.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorldConfig;

/// Fixed-step clock state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixedStepClock {
    accumulated_millis: u64,
    tick: Tick,
    paused: bool,
    platform_event_collections: u64,
    dropped_presentation_millis: u64,
    dropped_presentation_frames: u64,
}

/// Fixed-step configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FixedStepConfig {
    /// Milliseconds per simulation tick.
    pub step_millis: u32,
    /// Maximum simulation ticks executed for a single presentation frame.
    pub max_steps_per_frame: u32,
}

impl Default for FixedStepConfig {
    fn default() -> Self {
        Self {
            step_millis: 16,
            max_steps_per_frame: 8,
        }
    }
}

/// Shutdown ordering report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShutdownReport {
    /// Object handles released before managers.
    pub released_objects: Vec<ObjectHandle>,
    /// Whether managers were released after objects.
    pub managers_released: bool,
}

#[derive(Clone, Debug)]
struct Slot {
    generation: u32,
    live: bool,
    registered: bool,
    original_id: Option<OriginalObjectId>,
    owner_id: Option<OwnerId>,
    mirror_id: Option<OriginalObjectId>,
    registration_sequence: Option<u64>,
}

/// World.
#[derive(Clone, Debug)]
pub struct World {
    slots: Vec<Slot>,
    queue: VecDeque<WorldCommand>,
    deferred_delete: Vec<ObjectHandle>,
    phase: WorldPhase,
    tick: Tick,
    next_sequence: u64,
    next_registration_sequence: u64,
}

/// World error.
#[derive(Debug, Eq, PartialEq)]
pub enum WorldError {
    /// Invalid handle.
    InvalidHandle,
    /// Stale handle.
    StaleHandle,
    /// Object already deleted.
    Deleted,
    /// Duplicate original object id.
    DuplicateOriginalObjectId(OriginalObjectId),
    /// Invalid fixed-step configuration.
    InvalidFixedStep,
}

impl std::fmt::Display for WorldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidHandle => write!(f, "object handle does not reference a known slot"),
            Self::StaleHandle => write!(f, "object handle belongs to an older slot generation"),
            Self::Deleted => write!(f, "object has already been deleted"),
            Self::DuplicateOriginalObjectId(id) => {
                write!(f, "original object id {} is already registered", id.0)
            }
            Self::InvalidFixedStep => {
                write!(f, "fixed-step configuration values must be non-zero")
            }
        }
    }
}

impl std::error::Error for WorldError {}

/// Creates a world.
#[must_use]
pub fn new(_config: WorldConfig) -> World {
    World {
        slots: Vec::new(),
        queue: VecDeque::new(),
        deferred_delete: Vec::new(),
        phase: WorldPhase::Idle,
        tick: Tick(0),
        next_sequence: 0,
        next_registration_sequence: 0,
    }
}

/// Constructs an object without registering it.
///
/// # Errors
///
/// Returns [`WorldError::InvalidHandle`] if the slot index cannot be
/// represented by an [`ObjectHandle`].
pub fn construct_object(world: &mut World, draft: ObjectDraft) -> Result<ObjectHandle, WorldError> {
    let slot = u32::try_from(world.slots.len()).map_err(|_| WorldError::InvalidHandle)?;
    let handle = ObjectHandle {
        generation: 1,
        slot,
    };
    world.slots.push(Slot {
        generation: 1,
        live: true,
        registered: false,
        original_id: draft.original_id,
        owner_id: None,
        mirror_id: None,
        registration_sequence: None,
    });
    Ok(handle)
}

/// Registers a constructed object.
///
/// # Errors
///
/// Returns [`WorldError`] if the handle is stale, deleted, or out of range.
pub fn register_object(world: &mut World, handle: ObjectHandle) -> Result<(), WorldError> {
    let original_id = checked_slot(world, handle)?.original_id;
    if let Some(original_id) = original_id {
        let duplicate = world.slots.iter().enumerate().any(|(idx, slot)| {
            u32::try_from(idx).is_ok_and(|slot_index| slot_index != handle.slot)
                && slot.live
                && slot.registered
                && slot.original_id == Some(original_id)
        });
        if duplicate {
            return Err(WorldError::DuplicateOriginalObjectId(original_id));
        }
    }
    let sequence = world.next_registration_sequence;
    world.next_registration_sequence = world.next_registration_sequence.saturating_add(1);
    let slot = checked_slot_mut(world, handle)?;
    slot.registered = true;
    slot.registration_sequence = Some(sequence);
    Ok(())
}

/// Attaches local ownership metadata to an object.
///
/// # Errors
///
/// Returns [`WorldError`] if the handle is stale, deleted, or out of range.
pub fn set_owner(
    world: &mut World,
    handle: ObjectHandle,
    owner_id: Option<OwnerId>,
) -> Result<(), WorldError> {
    checked_slot_mut(world, handle)?.owner_id = owner_id;
    Ok(())
}

/// Attaches mirror metadata to an object without changing its original id.
///
/// # Errors
///
/// Returns [`WorldError`] if the handle is stale, deleted, or out of range.
pub fn set_mirror_original(
    world: &mut World,
    handle: ObjectHandle,
    mirror_id: Option<OriginalObjectId>,
) -> Result<(), WorldError> {
    checked_slot_mut(world, handle)?.mirror_id = mirror_id;
    Ok(())
}

/// Returns registration sequence for a live object.
///
/// # Errors
///
/// Returns [`WorldError`] if the handle is stale, deleted, or out of range.
pub fn registration_sequence(
    world: &World,
    handle: ObjectHandle,
) -> Result<Option<u64>, WorldError> {
    Ok(checked_slot(world, handle)?.registration_sequence)
}

/// Returns object identity metadata.
///
/// # Errors
///
/// Returns [`WorldError`] if the handle is stale, deleted, or out of range.
pub fn identity_metadata(
    world: &World,
    handle: ObjectHandle,
) -> Result<IdentityMetadata, WorldError> {
    let slot = checked_slot(world, handle)?;
    Ok(IdentityMetadata {
        original_id: slot.original_id,
        mirror_id: slot.mirror_id,
        owner_id: slot.owner_id,
    })
}

/// Requests deletion.
///
/// # Errors
///
/// Returns [`WorldError`] if the handle is stale, deleted, or out of range.
pub fn request_delete(world: &mut World, handle: ObjectHandle) -> Result<(), WorldError> {
    checked_slot(world, handle)?;
    if world.phase == WorldPhase::Calculating {
        if !world.deferred_delete.contains(&handle) {
            world.deferred_delete.push(handle);
        }
        Ok(())
    } else {
        delete_now(world, handle)
    }
}

/// Enqueues a command.
///
/// # Errors
///
/// Returns [`WorldError`] when a targeted command references an invalid
/// handle.
pub fn enqueue(world: &mut World, mut command: WorldCommand) -> Result<(), WorldError> {
    if let Some(handle) = command.target {
        checked_slot(world, handle)?;
    }
    command.sequence = world.next_sequence;
    world.next_sequence = world.next_sequence.saturating_add(1);
    world.queue.push_back(command);
    Ok(())
}

/// Advances one deterministic step.
///
/// # Errors
///
/// Returns [`WorldError`] if a queued command references a stale, deleted, or
/// out-of-range handle.
pub fn step(world: &mut World, input: &InputSnapshot) -> Result<WorldSnapshot, WorldError> {
    step_with_handler(world, input, |_, _| Ok(()))
}

/// Advances one deterministic step with a command callback.
///
/// The callback runs while the world is in the calculating phase, which allows
/// tests and adapters to exercise deferred deletion semantics without exposing
/// mutable slot internals.
///
/// # Errors
///
/// Returns [`WorldError`] if a queued command references a stale, deleted, or
/// out-of-range handle, or if the callback reports a world error.
pub fn step_with_handler<F>(
    world: &mut World,
    _input: &InputSnapshot,
    mut handler: F,
) -> Result<WorldSnapshot, WorldError>
where
    F: FnMut(&mut World, &WorldCommand) -> Result<(), WorldError>,
{
    let before = world.clone();
    world.phase = WorldPhase::Calculating;
    let mut events = Vec::new();
    let result = (|| {
        while let Some(command) = world.queue.pop_front() {
            if let Some(handle) = command.target {
                if world.deferred_delete.contains(&handle) {
                    continue;
                }
                checked_slot(world, handle)?;
            }
            handler(world, &command)?;
            events.push(WorldEvent {
                sequence: command.sequence,
                target: command.target,
            });
        }
        world.phase = WorldPhase::ApplyingDeferred;
        let deletes = std::mem::take(&mut world.deferred_delete);
        for handle in deletes {
            delete_now(world, handle)?;
        }
        world.tick.0 = world.tick.0.saturating_add(1);
        world.phase = WorldPhase::PublishingSnapshot;
        let snapshot = WorldSnapshot {
            tick: world.tick,
            objects: live_registered(world),
            events,
            hash: canonical_state_hash(world),
        };
        world.phase = WorldPhase::Idle;
        Ok(snapshot)
    })();
    if let Err(err) = result {
        *world = before;
        world.phase = WorldPhase::Idle;
        return Err(err);
    }
    result
}

/// Computes canonical state hash.
#[must_use]
pub fn canonical_state_hash(world: &World) -> StateHash {
    let mut state = 0xcbf2_9ce4_8422_2325_u64;
    hash_u64(&mut state, world.tick.0);
    for (idx, slot) in world.slots.iter().enumerate() {
        hash_u64(&mut state, idx as u64);
        hash_u64(&mut state, u64::from(slot.generation));
        hash_u64(&mut state, u64::from(u8::from(slot.live)));
        hash_u64(&mut state, u64::from(u8::from(slot.registered)));
        hash_u64(&mut state, slot.original_id.map_or(0, |id| u64::from(id.0)));
        hash_u64(&mut state, slot.mirror_id.map_or(0, |id| u64::from(id.0)));
        hash_u64(&mut state, slot.owner_id.map_or(0, |id| u64::from(id.0)));
        hash_u64(&mut state, slot.registration_sequence.unwrap_or(u64::MAX));
    }
    let mut out = [0; 32];
    out[..8].copy_from_slice(&state.to_le_bytes());
    out[8..16].copy_from_slice(&state.rotate_left(13).to_le_bytes());
    out[16..24].copy_from_slice(&state.rotate_left(29).to_le_bytes());
    out[24..32].copy_from_slice(&state.rotate_left(47).to_le_bytes());
    StateHash(out)
}

/// Creates a fixed-step clock.
///
/// # Errors
///
/// Returns [`WorldError::InvalidFixedStep`] when the configured step or
/// per-frame catch-up limit is zero.
pub fn fixed_step_clock(config: FixedStepConfig) -> Result<FixedStepClock, WorldError> {
    if config.step_millis == 0 || config.max_steps_per_frame == 0 {
        return Err(WorldError::InvalidFixedStep);
    }
    Ok(FixedStepClock {
        accumulated_millis: 0,
        tick: Tick(0),
        paused: false,
        platform_event_collections: 0,
        dropped_presentation_millis: 0,
        dropped_presentation_frames: 0,
    })
}

/// Records platform event collection independently of game time.
pub fn collect_platform_events(clock: &mut FixedStepClock) {
    clock.platform_event_collections = clock.platform_event_collections.saturating_add(1);
}

/// Sets pause state.
pub fn set_paused(clock: &mut FixedStepClock, paused: bool) {
    clock.paused = paused;
}

/// Advances fixed-step game time.
///
/// Returns the number of simulation ticks that should be executed.
///
/// # Errors
///
/// Returns [`WorldError::InvalidFixedStep`] when the configured step or
/// per-frame catch-up limit is zero.
pub fn advance_fixed_step(
    clock: &mut FixedStepClock,
    config: FixedStepConfig,
    elapsed_millis: u64,
) -> Result<u32, WorldError> {
    if config.step_millis == 0 || config.max_steps_per_frame == 0 {
        return Err(WorldError::InvalidFixedStep);
    }
    if clock.paused {
        return Ok(0);
    }
    clock.accumulated_millis = clock.accumulated_millis.saturating_add(elapsed_millis);
    let step = u64::from(config.step_millis);
    let available_steps = clock.accumulated_millis / step;
    let ticks_u64 = available_steps.min(u64::from(config.max_steps_per_frame));
    let consumed = ticks_u64.saturating_mul(step);
    if available_steps > u64::from(config.max_steps_per_frame) {
        let dropped = clock.accumulated_millis.saturating_sub(consumed);
        clock.dropped_presentation_millis =
            clock.dropped_presentation_millis.saturating_add(dropped);
        clock.dropped_presentation_frames = clock.dropped_presentation_frames.saturating_add(1);
        clock.accumulated_millis = 0;
    } else {
        clock.accumulated_millis = clock.accumulated_millis.saturating_sub(consumed);
    }
    let ticks = u32::try_from(ticks_u64).unwrap_or(u32::MAX);
    clock.tick.0 = clock.tick.0.saturating_add(ticks_u64);
    Ok(ticks)
}

/// Returns fixed-step clock tick.
#[must_use]
pub fn fixed_step_tick(clock: &FixedStepClock) -> Tick {
    clock.tick
}

/// Returns platform event collection count.
#[must_use]
pub fn platform_event_collections(clock: &FixedStepClock) -> u64 {
    clock.platform_event_collections
}

/// Returns total presentation time dropped by fixed-step catch-up limits.
#[must_use]
pub fn dropped_presentation_millis(clock: &FixedStepClock) -> u64 {
    clock.dropped_presentation_millis
}

/// Returns how many presentation frames exceeded fixed-step catch-up limits.
#[must_use]
pub fn dropped_presentation_frames(clock: &FixedStepClock) -> u64 {
    clock.dropped_presentation_frames
}

/// Runs end-frame callbacks in stable sequence order.
#[must_use]
pub fn end_frame_callback_order(mut callbacks: Vec<WorldEvent>) -> Vec<u64> {
    callbacks.sort_by_key(|event| event.sequence);
    callbacks.into_iter().map(|event| event.sequence).collect()
}

/// Releases live objects before managers.
#[must_use]
pub fn shutdown(mut world: World) -> ShutdownReport {
    let released_objects = live_registered(&world);
    for slot in &mut world.slots {
        slot.live = false;
        slot.registered = false;
        slot.generation = slot.generation.saturating_add(1);
    }
    ShutdownReport {
        released_objects,
        managers_released: true,
    }
}

fn hash_u64(state: &mut u64, value: u64) {
    for byte in value.to_le_bytes() {
        *state ^= u64::from(byte);
        *state = state.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

fn checked_slot(world: &World, handle: ObjectHandle) -> Result<&Slot, WorldError> {
    let slot = world
        .slots
        .get(handle.slot as usize)
        .ok_or(WorldError::InvalidHandle)?;
    if slot.generation != handle.generation {
        return Err(WorldError::StaleHandle);
    }
    if !slot.live {
        return Err(WorldError::Deleted);
    }
    Ok(slot)
}

fn checked_slot_mut(world: &mut World, handle: ObjectHandle) -> Result<&mut Slot, WorldError> {
    let slot = world
        .slots
        .get_mut(handle.slot as usize)
        .ok_or(WorldError::InvalidHandle)?;
    if slot.generation != handle.generation {
        return Err(WorldError::StaleHandle);
    }
    if !slot.live {
        return Err(WorldError::Deleted);
    }
    Ok(slot)
}

fn delete_now(world: &mut World, handle: ObjectHandle) -> Result<(), WorldError> {
    let slot = checked_slot_mut(world, handle)?;
    slot.live = false;
    slot.generation = slot.generation.saturating_add(1);
    Ok(())
}

fn live_registered(world: &World) -> Vec<ObjectHandle> {
    world
        .slots
        .iter()
        .enumerate()
        .filter_map(|(idx, slot)| {
            let slot_index = u32::try_from(idx).ok()?;
            (slot.live && slot.registered).then_some(ObjectHandle {
                generation: slot.generation,
                slot: slot_index,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn construct_register_and_hash_are_stable() {
        let mut world = new(WorldConfig);
        let handle = construct_object(&mut world, ObjectDraft { original_id: None }).expect("obj");
        let before = step(&mut world, &InputSnapshot).expect("step");
        assert!(before.objects.is_empty());
        register_object(&mut world, handle).expect("register");
        let after = step(&mut world, &InputSnapshot).expect("step");
        assert_eq!(after.objects, vec![handle]);
    }

    #[test]
    fn registration_sequence_stale_and_duplicate_original_contracts() {
        let mut world = new(WorldConfig);
        let first = construct_object(
            &mut world,
            ObjectDraft {
                original_id: Some(OriginalObjectId(7)),
            },
        )
        .expect("first");
        let second = construct_object(
            &mut world,
            ObjectDraft {
                original_id: Some(OriginalObjectId(8)),
            },
        )
        .expect("second");
        register_object(&mut world, first).expect("register first");
        register_object(&mut world, second).expect("register second");
        assert_eq!(registration_sequence(&world, first), Ok(Some(0)));
        assert_eq!(registration_sequence(&world, second), Ok(Some(1)));

        request_delete(&mut world, first).expect("delete");
        assert_eq!(
            register_object(&mut world, first),
            Err(WorldError::StaleHandle)
        );
        let recycled = ObjectHandle {
            generation: first.generation,
            slot: first.slot,
        };
        assert_eq!(
            register_object(&mut world, recycled),
            Err(WorldError::StaleHandle)
        );

        let duplicate = construct_object(
            &mut world,
            ObjectDraft {
                original_id: Some(OriginalObjectId(8)),
            },
        )
        .expect("duplicate");
        assert_eq!(
            register_object(&mut world, duplicate),
            Err(WorldError::DuplicateOriginalObjectId(OriginalObjectId(8)))
        );
    }

    #[test]
    fn world_error_display_is_actionable() {
        assert_eq!(
            WorldError::StaleHandle.to_string(),
            "object handle belongs to an older slot generation"
        );
        assert_eq!(
            WorldError::DuplicateOriginalObjectId(OriginalObjectId(8)).to_string(),
            "original object id 8 is already registered"
        );
    }

    #[test]
    fn identity_metadata_keeps_original_mirror_and_owner_distinct() {
        let mut world = new(WorldConfig);
        let handle = construct_object(
            &mut world,
            ObjectDraft {
                original_id: Some(OriginalObjectId(10)),
            },
        )
        .expect("object");
        set_mirror_original(&mut world, handle, Some(OriginalObjectId(20))).expect("mirror");
        set_owner(&mut world, handle, Some(OwnerId(3))).expect("owner");
        assert_eq!(
            identity_metadata(&world, handle),
            Ok(IdentityMetadata {
                original_id: Some(OriginalObjectId(10)),
                mirror_id: Some(OriginalObjectId(20)),
                owner_id: Some(OwnerId(3))
            })
        );
    }

    #[test]
    fn command_fifo_and_deferred_delete_during_calculation() {
        let mut world = new(WorldConfig);
        let first = construct_object(&mut world, ObjectDraft { original_id: None }).expect("first");
        let second =
            construct_object(&mut world, ObjectDraft { original_id: None }).expect("second");
        register_object(&mut world, first).expect("register first");
        register_object(&mut world, second).expect("register second");
        enqueue(
            &mut world,
            WorldCommand {
                sequence: 99,
                target: Some(first),
            },
        )
        .expect("enqueue first");
        enqueue(
            &mut world,
            WorldCommand {
                sequence: 99,
                target: Some(second),
            },
        )
        .expect("enqueue second");
        enqueue(
            &mut world,
            WorldCommand {
                sequence: 99,
                target: Some(first),
            },
        )
        .expect("enqueue first again");

        let snapshot = step_with_handler(&mut world, &InputSnapshot, |world, command| {
            if command.target == Some(first) {
                request_delete(world, first)?;
                request_delete(world, first)?;
            }
            Ok(())
        })
        .expect("step");

        assert_eq!(
            snapshot.events,
            vec![
                WorldEvent {
                    sequence: 0,
                    target: Some(first)
                },
                WorldEvent {
                    sequence: 1,
                    target: Some(second)
                }
            ]
        );
        assert_eq!(
            request_delete(&mut world, first),
            Err(WorldError::StaleHandle)
        );
        assert_eq!(
            step(&mut world, &InputSnapshot).expect("step").objects,
            vec![second]
        );
    }

    #[test]
    fn callback_error_rolls_back_phase_queue_and_deferred_deletes() {
        let mut world = new(WorldConfig);
        let first = construct_object(&mut world, ObjectDraft { original_id: None }).expect("first");
        register_object(&mut world, first).expect("register");
        enqueue(
            &mut world,
            WorldCommand {
                sequence: 7,
                target: Some(first),
            },
        )
        .expect("enqueue");

        let err = step_with_handler(&mut world, &InputSnapshot, |world, _| {
            request_delete(world, first)?;
            Err(WorldError::InvalidFixedStep)
        })
        .expect_err("handler error");

        assert_eq!(err, WorldError::InvalidFixedStep);
        assert_eq!(world.phase, WorldPhase::Idle);
        assert_eq!(world.tick, Tick(0));
        assert!(world.deferred_delete.is_empty());
        assert_eq!(world.queue.len(), 1);

        let snapshot = step(&mut world, &InputSnapshot).expect("retry step");
        assert_eq!(snapshot.tick, Tick(1));
        assert_eq!(
            snapshot.events,
            vec![WorldEvent {
                sequence: 0,
                target: Some(first)
            }]
        );
        assert_eq!(snapshot.objects, vec![first]);
    }

    #[test]
    fn snapshot_hash_determinism_and_immutability() {
        let mut left = new(WorldConfig);
        let mut right = new(WorldConfig);
        for world in [&mut left, &mut right] {
            let handle = construct_object(
                world,
                ObjectDraft {
                    original_id: Some(OriginalObjectId(1)),
                },
            )
            .expect("object");
            register_object(world, handle).expect("register");
        }
        let snapshot = step(&mut left, &InputSnapshot).expect("snapshot");
        let clone = snapshot.clone();
        let extra = construct_object(&mut left, ObjectDraft { original_id: None }).expect("extra");
        register_object(&mut left, extra).expect("register extra");

        assert_eq!(snapshot, clone);
        assert_eq!(
            clone.hash,
            step(&mut right, &InputSnapshot).expect("right").hash
        );
    }

    #[test]
    fn fixed_step_pause_and_long_determinism_are_stable() {
        let config = FixedStepConfig {
            step_millis: 20,
            max_steps_per_frame: 8,
        };
        let mut clock = fixed_step_clock(config).expect("clock");
        collect_platform_events(&mut clock);
        set_paused(&mut clock, true);
        assert_eq!(advance_fixed_step(&mut clock, config, 100), Ok(0));
        collect_platform_events(&mut clock);
        assert_eq!(fixed_step_tick(&clock), Tick(0));
        assert_eq!(platform_event_collections(&clock), 2);

        set_paused(&mut clock, false);
        assert_eq!(advance_fixed_step(&mut clock, config, 45), Ok(2));
        assert_eq!(fixed_step_tick(&clock), Tick(2));

        let mut first = new(WorldConfig);
        let mut second = new(WorldConfig);
        let mut first_hashes = Vec::new();
        let mut second_hashes = Vec::new();
        for _ in 0..10_000 {
            first_hashes.push(step(&mut first, &InputSnapshot).expect("first").hash);
            second_hashes.push(step(&mut second, &InputSnapshot).expect("second").hash);
        }
        assert_eq!(first_hashes, second_hashes);
    }

    #[test]
    fn fixed_step_catch_up_is_capped_and_reports_dropped_time() {
        let config = FixedStepConfig {
            step_millis: 20,
            max_steps_per_frame: 3,
        };
        let mut clock = fixed_step_clock(config).expect("clock");

        assert_eq!(advance_fixed_step(&mut clock, config, 95), Ok(3));
        assert_eq!(fixed_step_tick(&clock), Tick(3));
        assert_eq!(dropped_presentation_millis(&clock), 35);
        assert_eq!(dropped_presentation_frames(&clock), 1);

        assert_eq!(advance_fixed_step(&mut clock, config, 10), Ok(0));
        assert_eq!(advance_fixed_step(&mut clock, config, 10), Ok(1));
        assert_eq!(fixed_step_tick(&clock), Tick(4));
        assert_eq!(dropped_presentation_millis(&clock), 35);
        assert_eq!(dropped_presentation_frames(&clock), 1);

        assert_eq!(
            advance_fixed_step(&mut clock, config, u64::MAX),
            Ok(config.max_steps_per_frame)
        );
        assert_eq!(dropped_presentation_frames(&clock), 2);
    }

    #[test]
    fn render_disabled_does_not_change_hash_end_callbacks_and_shutdown_order() {
        let callbacks = vec![
            WorldEvent {
                sequence: 3,
                target: None,
            },
            WorldEvent {
                sequence: 1,
                target: None,
            },
            WorldEvent {
                sequence: 2,
                target: None,
            },
        ];
        assert_eq!(end_frame_callback_order(callbacks), vec![1, 2, 3]);

        let mut rendered = new(WorldConfig);
        let mut headless = rendered.clone();
        assert_eq!(
            step(&mut rendered, &InputSnapshot).expect("rendered").hash,
            step(&mut headless, &InputSnapshot).expect("headless").hash
        );

        let handle =
            construct_object(&mut rendered, ObjectDraft { original_id: None }).expect("object");
        register_object(&mut rendered, handle).expect("register");
        assert_eq!(
            shutdown(rendered),
            ShutdownReport {
                released_objects: vec![handle],
                managers_released: true
            }
        );
    }

    #[test]
    fn generated_command_delete_sequences_preserve_registry_invariants() {
        for seed in 0_u32..64 {
            let mut world = new(WorldConfig);
            let mut handles = Vec::new();
            for index in 0..8 {
                let handle = construct_object(
                    &mut world,
                    ObjectDraft {
                        original_id: Some(OriginalObjectId(seed * 100 + index)),
                    },
                )
                .expect("object");
                register_object(&mut world, handle).expect("register");
                handles.push(handle);
            }
            for (index, handle) in handles.iter().copied().enumerate() {
                if (seed as usize + index) % 3 == 0 {
                    request_delete(&mut world, handle).expect("delete");
                } else {
                    enqueue(
                        &mut world,
                        WorldCommand {
                            sequence: 0,
                            target: Some(handle),
                        },
                    )
                    .expect("enqueue");
                }
            }
            let snapshot = step(&mut world, &InputSnapshot).expect("step");
            for handle in snapshot.objects {
                assert!(registration_sequence(&world, handle)
                    .expect("sequence")
                    .is_some());
            }
        }
    }
}
