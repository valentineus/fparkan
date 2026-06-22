#![forbid(unsafe_code)]
#![allow(clippy::cast_precision_loss)]
//! Deterministic animation sampling contracts.

use std::fmt;

/// Numeric profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NumericProfile {
    /// Portable reference.
    PortableReference,
    /// X87-compatible compatibility profile for captured parity vectors.
    X87Compatibility,
}

/// Animation time in frames.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AnimationTime(pub f32);

/// Pose.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    /// Translation.
    pub translation: [f32; 3],
    /// Quaternion.
    pub rotation: [f32; 4],
}

impl Default for Pose {
    fn default() -> Self {
        Self {
            translation: [0.0; 3],
            rotation: [0.0, 0.0, 0.0, 1.0],
        }
    }
}

/// Scalar animation key.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScalarKey {
    /// Frame number.
    pub frame: u32,
    /// Scalar value at the frame.
    pub value: f32,
}

/// Pose animation key.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PoseKey {
    /// Frame number.
    pub frame: u32,
    /// Pose at the frame.
    pub pose: Pose,
}

/// Pose key addressed by a floating-point animation time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimedPoseKey {
    /// Key time in frames.
    pub time: AnimationTime,
    /// Pose at the time.
    pub pose: Pose,
}

/// Decoded 24-byte animation key.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AnimKey24 {
    /// Key time.
    pub time: AnimationTime,
    /// Pose decoded from signed fixed-point channels.
    pub pose: Pose,
}

/// Optional frame remapping table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameMap {
    attr_frame_count: u16,
    frames: Vec<u16>,
}

/// Scalar track with a deterministic fallback.
#[derive(Clone, Debug, PartialEq)]
pub struct ScalarTrack {
    fallback: f32,
    keys: Vec<ScalarKey>,
}

/// Pose track with a deterministic fallback.
#[derive(Clone, Debug, PartialEq)]
pub struct PoseTrack {
    fallback: Pose,
    keys: Vec<PoseKey>,
}

/// Pose track keyed by floating-point animation times.
#[derive(Clone, Debug, PartialEq)]
pub struct TimedPoseTrack {
    fallback: Pose,
    keys: Vec<TimedPoseKey>,
}

/// Parent index for a node in an animation hierarchy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParentIndex(pub Option<u16>);

/// Node pose after hierarchy evaluation.
#[derive(Clone, Debug, PartialEq)]
pub struct NodePoseBuffer {
    /// Global poses in node order.
    pub poses: Vec<Pose>,
}

/// Difference between portable and x87-compatible samples.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NumericProfileDifference {
    /// Time that was sampled.
    pub time: AnimationTime,
    /// Per-axis translation delta: x87 - portable.
    pub translation_delta: [f32; 3],
    /// Per-component quaternion delta: x87 - portable.
    pub rotation_delta: [f32; 4],
}

/// Material animation state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaterialAnimationState {
    /// Time used by material phase evaluation.
    pub time: AnimationTime,
    /// Named deterministic random stream.
    pub rng: NamedRngStream,
}

/// Named deterministic random stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NamedRngStream {
    state: u64,
    calls: u64,
}

/// Animation sampling error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AnimationError {
    /// Track keys are not sorted by frame or contain duplicate frames.
    NonMonotonicKeys,
    /// Time was NaN or infinite.
    InvalidTime,
    /// Quaternion could not be normalized.
    InvalidQuaternion,
    /// Input buffer size is invalid for the expected record stride.
    InvalidSize,
    /// Frame map entry points outside the clip frame count.
    InvalidFrameMapValue {
        /// Requested mapped frame.
        frame: u16,
        /// Declared frame count.
        frame_count: u16,
    },
    /// Parent index is not before its child.
    ParentOrder {
        /// Child node index.
        child: usize,
        /// Parent node index.
        parent: usize,
    },
    /// Parent graph contains a cycle.
    ParentCycle {
        /// Node where the cycle was detected.
        node: usize,
    },
}

impl fmt::Display for AnimationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for AnimationError {}

impl ScalarTrack {
    /// Creates a scalar track.
    ///
    /// # Errors
    ///
    /// Returns [`AnimationError::NonMonotonicKeys`] when keys are not strictly
    /// sorted by frame.
    pub fn new(fallback: f32, keys: Vec<ScalarKey>) -> Result<Self, AnimationError> {
        validate_scalar_keys(&keys)?;
        Ok(Self { fallback, keys })
    }

    /// Returns the keys in frame order.
    #[must_use]
    pub fn keys(&self) -> &[ScalarKey] {
        &self.keys
    }

    /// Samples the scalar track with clamp-and-linear semantics.
    ///
    /// # Errors
    ///
    /// Returns [`AnimationError::InvalidTime`] when `time` is NaN or infinite.
    pub fn sample(&self, time: AnimationTime) -> Result<f32, AnimationError> {
        validate_time(time)?;
        let Some(first) = self.keys.first() else {
            return Ok(self.fallback);
        };
        if time.0 <= first.frame as f32 {
            return Ok(first.value);
        }

        for pair in self.keys.windows(2) {
            let left = pair[0];
            let right = pair[1];
            let left_frame = left.frame as f32;
            let right_frame = right.frame as f32;
            if time.0 <= right_frame {
                let span = right_frame - left_frame;
                let t = if span == 0.0 {
                    0.0
                } else {
                    (time.0 - left_frame) / span
                };
                return Ok(lerp(left.value, right.value, t));
            }
        }

        Ok(self.keys.last().map_or(self.fallback, |key| key.value))
    }
}

impl AnimKey24 {
    /// Decodes one 24-byte animation key.
    ///
    /// Layout: `position:f32x3`, `time:f32`, `rotation:i16x4` scaled by
    /// `1/32767`.
    ///
    /// # Errors
    ///
    /// Returns [`AnimationError::InvalidSize`] when the record is not exactly
    /// 24 bytes or [`AnimationError::InvalidTime`] when the key time is not
    /// finite.
    pub fn decode(bytes: &[u8]) -> Result<Self, AnimationError> {
        if bytes.len() != 24 {
            return Err(AnimationError::InvalidSize);
        }
        let translation = [
            read_f32(bytes, 0)?,
            read_f32(bytes, 4)?,
            read_f32(bytes, 8)?,
        ];
        let time = AnimationTime(read_f32(bytes, 12)?);
        validate_time(time)?;
        let raw_rotation = [
            f32::from(read_i16(bytes, 16)?) / 32767.0,
            f32::from(read_i16(bytes, 18)?) / 32767.0,
            f32::from(read_i16(bytes, 20)?) / 32767.0,
            f32::from(read_i16(bytes, 22)?) / 32767.0,
        ];
        Ok(Self {
            time,
            pose: Pose {
                translation,
                rotation: raw_rotation,
            },
        })
    }

    /// Returns a pose ready for runtime sampling.
    ///
    /// Degenerate all-zero quaternions are treated as identity, matching the
    /// safe static-node fallback used by legacy animation data.
    #[must_use]
    pub fn sampling_pose(&self) -> Pose {
        let rotation = normalize_quat(self.pose.rotation).unwrap_or(Pose::default().rotation);
        Pose {
            translation: self.pose.translation,
            rotation,
        }
    }
}

impl TimedPoseTrack {
    /// Creates a pose track keyed by floating-point times.
    ///
    /// # Errors
    ///
    /// Returns [`AnimationError::NonMonotonicKeys`] when keys are not strictly
    /// sorted by time, [`AnimationError::InvalidTime`] when a key time is not
    /// finite, or [`AnimationError::InvalidQuaternion`] when a key rotation
    /// cannot be normalized.
    pub fn new(fallback: Pose, keys: Vec<TimedPoseKey>) -> Result<Self, AnimationError> {
        validate_pose(&fallback)?;
        validate_timed_pose_keys(&keys)?;
        Ok(Self { fallback, keys })
    }

    /// Returns keys in time order.
    #[must_use]
    pub fn keys(&self) -> &[TimedPoseKey] {
        &self.keys
    }

    /// Samples the pose track with linear translation and normalized
    /// quaternion interpolation.
    ///
    /// # Errors
    ///
    /// Returns [`AnimationError::InvalidTime`] when `time` is NaN or infinite.
    pub fn sample(&self, time: AnimationTime) -> Result<Pose, AnimationError> {
        validate_time(time)?;
        let Some(first) = self.keys.first() else {
            return Ok(self.fallback);
        };
        if time.0 <= first.time.0 {
            return Ok(first.pose);
        }

        for pair in self.keys.windows(2) {
            let left = pair[0];
            let right = pair[1];
            if time.0 <= right.time.0 {
                let span = right.time.0 - left.time.0;
                let t = if span == 0.0 {
                    0.0
                } else {
                    (time.0 - left.time.0) / span
                };
                return blend_pose(left.pose, right.pose, t);
            }
        }

        Ok(self.keys.last().map_or(self.fallback, |key| key.pose))
    }
}

impl FrameMap {
    /// Decodes a `u16` frame map from type-19 bytes and an attr frame count.
    ///
    /// # Errors
    ///
    /// Returns [`AnimationError::InvalidSize`] when bytes are not u16-aligned.
    pub fn decode(bytes: &[u8], attr_frame_count: u16) -> Result<Self, AnimationError> {
        if !bytes.len().is_multiple_of(2) {
            return Err(AnimationError::InvalidSize);
        }
        let mut frames = Vec::with_capacity(bytes.len() / 2);
        for offset in (0..bytes.len()).step_by(2) {
            frames.push(read_u16(bytes, offset)?);
        }
        Ok(Self {
            attr_frame_count,
            frames,
        })
    }

    /// Resolves a logical frame through the optional map.
    ///
    /// Missing map entries and invalid mapped values fall back to the input
    /// frame, which is the documented compatibility branch for incomplete
    /// legacy clips.
    #[must_use]
    pub fn resolve_or_fallback(&self, logical_frame: u16) -> u16 {
        let Some(mapped) = self.frames.get(usize::from(logical_frame)).copied() else {
            return logical_frame;
        };
        if mapped < self.attr_frame_count {
            mapped
        } else {
            logical_frame
        }
    }

    /// Resolves a logical frame and reports invalid mapped values explicitly.
    ///
    /// # Errors
    ///
    /// Returns [`AnimationError::InvalidFrameMapValue`] when the mapped frame is
    /// outside the declared attr frame count.
    pub fn resolve_strict(&self, logical_frame: u16) -> Result<u16, AnimationError> {
        let Some(mapped) = self.frames.get(usize::from(logical_frame)).copied() else {
            return Ok(logical_frame);
        };
        if mapped < self.attr_frame_count {
            Ok(mapped)
        } else {
            Err(AnimationError::InvalidFrameMapValue {
                frame: mapped,
                frame_count: self.attr_frame_count,
            })
        }
    }

    /// Declared frame count from attributes.
    #[must_use]
    pub const fn attr_frame_count(&self) -> u16 {
        self.attr_frame_count
    }

    /// Raw frame map values.
    #[must_use]
    pub fn frames(&self) -> &[u16] {
        &self.frames
    }
}

impl PoseTrack {
    /// Creates a pose track.
    ///
    /// # Errors
    ///
    /// Returns [`AnimationError::NonMonotonicKeys`] when keys are not strictly
    /// sorted by frame, or [`AnimationError::InvalidQuaternion`] when a key
    /// rotation cannot be normalized.
    pub fn new(fallback: Pose, keys: Vec<PoseKey>) -> Result<Self, AnimationError> {
        validate_pose(&fallback)?;
        validate_pose_keys(&keys)?;
        Ok(Self { fallback, keys })
    }

    /// Returns the keys in frame order.
    #[must_use]
    pub fn keys(&self) -> &[PoseKey] {
        &self.keys
    }

    /// Samples the pose track with linear translation and normalized quaternion
    /// interpolation.
    ///
    /// # Errors
    ///
    /// Returns [`AnimationError::InvalidTime`] when `time` is NaN or infinite.
    pub fn sample(
        &self,
        time: AnimationTime,
        _profile: NumericProfile,
    ) -> Result<Pose, AnimationError> {
        validate_time(time)?;
        let Some(first) = self.keys.first() else {
            return Ok(self.fallback);
        };
        if time.0 <= first.frame as f32 {
            return Ok(first.pose);
        }

        for pair in self.keys.windows(2) {
            let left = pair[0];
            let right = pair[1];
            let left_frame = left.frame as f32;
            let right_frame = right.frame as f32;
            if time.0 <= right_frame {
                let span = right_frame - left_frame;
                let t = if span == 0.0 {
                    0.0
                } else {
                    (time.0 - left_frame) / span
                };
                return blend_pose(left.pose, right.pose, t);
            }
        }

        Ok(self.keys.last().map_or(self.fallback, |key| key.pose))
    }
}

impl NamedRngStream {
    /// Creates a deterministic stream from a global seed and a stable stream
    /// name.
    #[must_use]
    pub fn new(seed: u64, name: &str) -> Self {
        let mut state = 0x9e37_79b9_7f4a_7c15_u64 ^ seed;
        for byte in name.as_bytes() {
            state ^= u64::from(*byte);
            state = splitmix64(state);
        }
        if state == 0 {
            state = 0x6a09_e667_f3bc_c909;
        }
        Self { state, calls: 0 }
    }

    /// Returns how many values have been generated.
    #[must_use]
    pub const fn calls(&self) -> u64 {
        self.calls
    }

    /// Returns the next deterministic `u32`.
    pub fn next_u32(&mut self) -> u32 {
        self.calls = self.calls.wrapping_add(1);
        self.state = splitmix64(self.state);
        (self.state >> 32) as u32
    }

    /// Returns the next deterministic scalar in `[0, 1]`.
    pub fn next_unit_f32(&mut self) -> f32 {
        let value = self.next_u32() >> 8;
        value as f32 / 0x00ff_ffff_u32 as f32
    }
}

impl MaterialAnimationState {
    /// Advances material time without drawing or emitting side effects.
    #[must_use]
    pub fn advanced(self, delta_frames: f32) -> Self {
        Self {
            time: AnimationTime(self.time.0 + delta_frames),
            rng: self.rng,
        }
    }
}

/// Builds a canonical pose capture from a track and frame list.
///
/// # Errors
///
/// Returns [`AnimationError`] when pose sampling fails.
pub fn canonical_pose_capture(
    track: &PoseTrack,
    times: &[AnimationTime],
) -> Result<Vec<u8>, AnimationError> {
    let mut out = Vec::new();
    for time in times {
        let pose = track.sample(*time, NumericProfile::PortableReference)?;
        out.extend_from_slice(b"P,");
        write_f32_bits(&mut out, time.0);
        for value in pose.translation {
            out.push(b',');
            write_f32_bits(&mut out, value);
        }
        for value in pose.rotation {
            out.push(b',');
            write_f32_bits(&mut out, value);
        }
        out.push(b'\n');
    }
    Ok(out)
}

/// Builds a canonical pose capture from a float-time track.
///
/// # Errors
///
/// Returns [`AnimationError`] when pose sampling fails.
pub fn canonical_timed_pose_capture(
    track: &TimedPoseTrack,
    times: &[AnimationTime],
) -> Result<Vec<u8>, AnimationError> {
    let mut out = Vec::new();
    for time in times {
        let pose = track.sample(*time)?;
        out.extend_from_slice(b"P,");
        write_f32_bits(&mut out, time.0);
        for value in pose.translation {
            out.push(b',');
            write_f32_bits(&mut out, value);
        }
        for value in pose.rotation {
            out.push(b',');
            write_f32_bits(&mut out, value);
        }
        out.push(b'\n');
    }
    Ok(out)
}

/// Blends two optional poses.
///
/// When only one side is valid, the valid side is returned. When both sides are
/// absent, [`AnimationError::InvalidQuaternion`] is returned as a deterministic
/// invalid-pose marker.
///
/// # Errors
///
/// Returns [`AnimationError`] when both inputs are invalid or quaternion
/// interpolation cannot be normalized.
pub fn blend_optional_pose(
    left: Option<Pose>,
    right: Option<Pose>,
    weight: f32,
) -> Result<Pose, AnimationError> {
    match (left, right) {
        (Some(left), Some(right)) => blend_pose(left, right, weight),
        (Some(pose), None) | (None, Some(pose)) => Ok(pose),
        (None, None) => Err(AnimationError::InvalidQuaternion),
    }
}

/// Evaluates local poses into global poses with parent-before-child ordering.
///
/// # Errors
///
/// Returns [`AnimationError::ParentOrder`] when a parent appears after its
/// child, or [`AnimationError::ParentCycle`] when a node is its own ancestor.
pub fn evaluate_hierarchy(
    parents: &[ParentIndex],
    local_poses: &[Pose],
) -> Result<NodePoseBuffer, AnimationError> {
    if parents.len() != local_poses.len() {
        return Err(AnimationError::InvalidSize);
    }
    for (index, parent) in parents.iter().enumerate() {
        let Some(raw_parent) = parent.0 else {
            continue;
        };
        let parent_index = usize::from(raw_parent);
        if parent_index == index {
            return Err(AnimationError::ParentCycle { node: index });
        }
        if parent_index > index {
            return Err(AnimationError::ParentOrder {
                child: index,
                parent: parent_index,
            });
        }
    }

    let mut global = Vec::with_capacity(local_poses.len());
    for (index, pose) in local_poses.iter().copied().enumerate() {
        let composed = if let Some(parent) = parents[index].0 {
            compose_pose(global[usize::from(parent)], pose)?
        } else {
            pose
        };
        global.push(composed);
    }
    Ok(NodePoseBuffer { poses: global })
}

/// Compares portable and x87-compatible profile samples explicitly.
///
/// # Errors
///
/// Returns [`AnimationError`] when either profile fails to sample.
pub fn compare_numeric_profiles(
    track: &PoseTrack,
    times: &[AnimationTime],
) -> Result<Vec<NumericProfileDifference>, AnimationError> {
    let mut out = Vec::with_capacity(times.len());
    for time in times {
        let portable = track.sample(*time, NumericProfile::PortableReference)?;
        let x87 = track.sample(*time, NumericProfile::X87Compatibility)?;
        out.push(NumericProfileDifference {
            time: *time,
            translation_delta: [
                x87.translation[0] - portable.translation[0],
                x87.translation[1] - portable.translation[1],
                x87.translation[2] - portable.translation[2],
            ],
            rotation_delta: [
                x87.rotation[0] - portable.rotation[0],
                x87.rotation[1] - portable.rotation[1],
                x87.rotation[2] - portable.rotation[2],
                x87.rotation[3] - portable.rotation[3],
            ],
        });
    }
    Ok(out)
}

fn validate_scalar_keys(keys: &[ScalarKey]) -> Result<(), AnimationError> {
    for pair in keys.windows(2) {
        if pair[0].frame >= pair[1].frame {
            return Err(AnimationError::NonMonotonicKeys);
        }
    }
    Ok(())
}

fn validate_pose_keys(keys: &[PoseKey]) -> Result<(), AnimationError> {
    for key in keys {
        validate_pose(&key.pose)?;
    }
    for pair in keys.windows(2) {
        if pair[0].frame >= pair[1].frame {
            return Err(AnimationError::NonMonotonicKeys);
        }
    }
    Ok(())
}

fn validate_timed_pose_keys(keys: &[TimedPoseKey]) -> Result<(), AnimationError> {
    for key in keys {
        validate_time(key.time)?;
        validate_pose(&key.pose)?;
    }
    for pair in keys.windows(2) {
        if pair[0].time.0 >= pair[1].time.0 {
            return Err(AnimationError::NonMonotonicKeys);
        }
    }
    Ok(())
}

fn validate_pose(pose: &Pose) -> Result<(), AnimationError> {
    normalize_quat(pose.rotation).map(|_| ())
}

fn validate_time(time: AnimationTime) -> Result<(), AnimationError> {
    if time.0.is_finite() {
        Ok(())
    } else {
        Err(AnimationError::InvalidTime)
    }
}

fn blend_pose(left: Pose, right: Pose, t: f32) -> Result<Pose, AnimationError> {
    let mut right_rotation = right.rotation;
    if dot4(left.rotation, right_rotation) < 0.0 {
        for value in &mut right_rotation {
            *value = -*value;
        }
    }

    Ok(Pose {
        translation: [
            lerp(left.translation[0], right.translation[0], t),
            lerp(left.translation[1], right.translation[1], t),
            lerp(left.translation[2], right.translation[2], t),
        ],
        rotation: normalize_quat([
            lerp(left.rotation[0], right_rotation[0], t),
            lerp(left.rotation[1], right_rotation[1], t),
            lerp(left.rotation[2], right_rotation[2], t),
            lerp(left.rotation[3], right_rotation[3], t),
        ])?,
    })
}

fn normalize_quat(quat: [f32; 4]) -> Result<[f32; 4], AnimationError> {
    let len2 = dot4(quat, quat);
    if !len2.is_finite() || len2 <= f32::EPSILON {
        return Err(AnimationError::InvalidQuaternion);
    }
    let inv = len2.sqrt().recip();
    Ok([quat[0] * inv, quat[1] * inv, quat[2] * inv, quat[3] * inv])
}

fn dot4(left: [f32; 4], right: [f32; 4]) -> f32 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2] + left[3] * right[3]
}

fn lerp(left: f32, right: f32, t: f32) -> f32 {
    left + (right - left) * t
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut mixed = value;
    mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    mixed ^ (mixed >> 31)
}

fn write_f32_bits(out: &mut Vec<u8>, value: f32) {
    out.extend_from_slice(format!("{:08x}", value.to_bits()).as_bytes());
}

fn compose_pose(parent: Pose, child: Pose) -> Result<Pose, AnimationError> {
    Ok(Pose {
        translation: [
            parent.translation[0] + child.translation[0],
            parent.translation[1] + child.translation[1],
            parent.translation[2] + child.translation[2],
        ],
        rotation: normalize_quat(mul_quat(parent.rotation, child.rotation))?,
    })
}

fn mul_quat(left: [f32; 4], right: [f32; 4]) -> [f32; 4] {
    let [lx, ly, lz, lw] = left;
    let [rx, ry, rz, rw] = right;
    [
        lw * rx + lx * rw + ly * rz - lz * ry,
        lw * ry - lx * rz + ly * rw + lz * rx,
        lw * rz + lx * ry - ly * rx + lz * rw,
        lw * rw - lx * rx - ly * ry - lz * rz,
    ]
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, AnimationError> {
    let raw = bytes
        .get(offset..offset + 2)
        .ok_or(AnimationError::InvalidSize)?;
    Ok(u16::from_le_bytes(
        raw.try_into().map_err(|_| AnimationError::InvalidSize)?,
    ))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, AnimationError> {
    let raw = bytes
        .get(offset..offset + 4)
        .ok_or(AnimationError::InvalidSize)?;
    Ok(u32::from_le_bytes(
        raw.try_into().map_err(|_| AnimationError::InvalidSize)?,
    ))
}

fn read_f32(bytes: &[u8], offset: usize) -> Result<f32, AnimationError> {
    Ok(f32::from_bits(read_u32(bytes, offset)?))
}

fn read_i16(bytes: &[u8], offset: usize) -> Result<i16, AnimationError> {
    let raw = bytes
        .get(offset..offset + 2)
        .ok_or(AnimationError::InvalidSize)?;
    Ok(i16::from_le_bytes(
        raw.try_into().map_err(|_| AnimationError::InvalidSize)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_track_clamps_and_interpolates() {
        let track = ScalarTrack::new(
            -1.0,
            vec![
                ScalarKey {
                    frame: 10,
                    value: 2.0,
                },
                ScalarKey {
                    frame: 20,
                    value: 6.0,
                },
            ],
        )
        .expect("track");

        assert_eq!(track.sample(AnimationTime(0.0)).expect("sample"), 2.0);
        assert_eq!(track.sample(AnimationTime(15.0)).expect("sample"), 4.0);
        assert_eq!(track.sample(AnimationTime(30.0)).expect("sample"), 6.0);
    }

    #[test]
    fn anim_key24_decodes_signed_quaternion() {
        let mut bytes = [0_u8; 24];
        bytes[0..4].copy_from_slice(&(-1.0_f32).to_bits().to_le_bytes());
        bytes[4..8].copy_from_slice(&(2.0_f32).to_bits().to_le_bytes());
        bytes[8..12].copy_from_slice(&(0.0_f32).to_bits().to_le_bytes());
        bytes[12..16].copy_from_slice(&(12.5_f32).to_bits().to_le_bytes());
        bytes[16..18].copy_from_slice(&0_i16.to_le_bytes());
        bytes[18..20].copy_from_slice(&(-23170_i16).to_le_bytes());
        bytes[20..22].copy_from_slice(&0_i16.to_le_bytes());
        bytes[22..24].copy_from_slice(&23170_i16.to_le_bytes());

        let key = AnimKey24::decode(&bytes).expect("key");

        assert_eq!(key.time, AnimationTime(12.5));
        assert_eq!(key.pose.translation, [-1.0, 2.0, 0.0]);
        assert!(key.pose.rotation[1] < 0.0);
        assert!((key.pose.rotation[1] + std::f32::consts::FRAC_1_SQRT_2).abs() < 0.000_05);
        assert!((key.pose.rotation[3] - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.000_05);
    }

    #[test]
    fn frame_map_decodes_u16_and_uses_attr_frame_count() {
        let map = FrameMap::decode(&[2, 0, 4, 0], 5).expect("map");

        assert_eq!(map.attr_frame_count(), 5);
        assert_eq!(map.frames(), &[2, 4]);
        assert_eq!(map.resolve_strict(0).expect("mapped"), 2);
        assert_eq!(map.resolve_strict(2).expect("fallback missing"), 2);
    }

    #[test]
    fn frame_map_falls_back_when_absent_or_invalid() {
        let empty = FrameMap::decode(&[], 3).expect("empty map");
        let invalid = FrameMap::decode(&[5, 0], 3).expect("invalid map");

        assert_eq!(empty.resolve_or_fallback(2), 2);
        assert_eq!(invalid.resolve_or_fallback(0), 0);
        assert_eq!(
            invalid.resolve_strict(0).expect_err("invalid mapped value"),
            AnimationError::InvalidFrameMapValue {
                frame: 5,
                frame_count: 3,
            }
        );
    }

    #[test]
    fn exact_key_time_returns_exact_pose() {
        let target = Pose {
            translation: [1.0, 2.0, 3.0],
            rotation: [0.0, 0.0, 1.0, 0.0],
        };
        let track = PoseTrack::new(
            Pose::default(),
            vec![
                PoseKey {
                    frame: 0,
                    pose: Pose::default(),
                },
                PoseKey {
                    frame: 8,
                    pose: target,
                },
            ],
        )
        .expect("track");

        assert_eq!(
            track
                .sample(AnimationTime(8.0), NumericProfile::PortableReference)
                .expect("pose"),
            target
        );
    }

    #[test]
    fn scalar_track_uses_fallback_when_empty() {
        let track = ScalarTrack::new(3.5, Vec::new()).expect("track");

        assert_eq!(track.sample(AnimationTime(4.0)).expect("sample"), 3.5);
    }

    #[test]
    fn rejects_unsorted_keys_and_invalid_time() {
        let track = ScalarTrack::new(
            0.0,
            vec![
                ScalarKey {
                    frame: 7,
                    value: 0.0,
                },
                ScalarKey {
                    frame: 7,
                    value: 1.0,
                },
            ],
        );
        assert_eq!(
            track.expect_err("unsorted"),
            AnimationError::NonMonotonicKeys
        );

        let track = ScalarTrack::new(0.0, Vec::new()).expect("track");
        assert_eq!(
            track
                .sample(AnimationTime(f32::NAN))
                .expect_err("invalid time"),
            AnimationError::InvalidTime
        );
    }

    #[test]
    fn pose_track_blends_translation_and_rotation() {
        let track = PoseTrack::new(
            Pose::default(),
            vec![
                PoseKey {
                    frame: 0,
                    pose: Pose::default(),
                },
                PoseKey {
                    frame: 10,
                    pose: Pose {
                        translation: [10.0, 20.0, 30.0],
                        rotation: [0.0, 1.0, 0.0, 0.0],
                    },
                },
            ],
        )
        .expect("track");

        let pose = track
            .sample(AnimationTime(5.0), NumericProfile::PortableReference)
            .expect("pose");

        assert_eq!(pose.translation, [5.0, 10.0, 15.0]);
        assert!((pose.rotation[1] - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.000_001);
        assert!((pose.rotation[3] - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.000_001);
    }

    #[test]
    fn timed_pose_track_samples_float_key_times() {
        let track = TimedPoseTrack::new(
            Pose::default(),
            vec![
                TimedPoseKey {
                    time: AnimationTime(1.5),
                    pose: Pose::default(),
                },
                TimedPoseKey {
                    time: AnimationTime(3.5),
                    pose: Pose {
                        translation: [4.0, 8.0, 12.0],
                        rotation: [0.0, 0.0, 1.0, 0.0],
                    },
                },
            ],
        )
        .expect("track");

        let pose = track.sample(AnimationTime(2.5)).expect("pose");

        assert_eq!(pose.translation, [2.0, 4.0, 6.0]);
        assert!((pose.rotation[2] - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.000_001);
        assert!((pose.rotation[3] - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.000_001);
    }

    #[test]
    fn quaternion_shortest_path_sign_flip_is_stable() {
        let track = PoseTrack::new(
            Pose::default(),
            vec![
                PoseKey {
                    frame: 0,
                    pose: Pose {
                        translation: [0.0; 3],
                        rotation: [0.0, 0.0, 0.0, 1.0],
                    },
                },
                PoseKey {
                    frame: 10,
                    pose: Pose {
                        translation: [0.0; 3],
                        rotation: [0.0, 0.0, 0.0, -1.0],
                    },
                },
            ],
        )
        .expect("track");

        let pose = track
            .sample(AnimationTime(5.0), NumericProfile::PortableReference)
            .expect("pose");

        assert_eq!(pose.rotation, [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn zero_or_degenerate_key_interval_is_rejected() {
        let track = PoseTrack::new(
            Pose::default(),
            vec![
                PoseKey {
                    frame: 1,
                    pose: Pose::default(),
                },
                PoseKey {
                    frame: 1,
                    pose: Pose::default(),
                },
            ],
        );

        assert_eq!(
            track.expect_err("duplicate key"),
            AnimationError::NonMonotonicKeys
        );
    }

    #[test]
    fn x87_boundary_golden_vectors_and_profile_difference_report() {
        let track = PoseTrack::new(
            Pose::default(),
            vec![
                PoseKey {
                    frame: 0,
                    pose: Pose::default(),
                },
                PoseKey {
                    frame: 2,
                    pose: Pose {
                        translation: [2.0, 0.0, 0.0],
                        rotation: [0.0, 0.0, 0.0, 1.0],
                    },
                },
            ],
        )
        .expect("track");

        let portable = track
            .sample(AnimationTime(1.0), NumericProfile::PortableReference)
            .expect("portable");
        let x87 = track
            .sample(AnimationTime(1.0), NumericProfile::X87Compatibility)
            .expect("x87");

        assert_eq!(portable, x87);
        assert_eq!(portable.translation, [1.0, 0.0, 0.0]);
        assert_eq!(
            compare_numeric_profiles(&track, &[AnimationTime(1.0)]).expect("diff"),
            vec![NumericProfileDifference {
                time: AnimationTime(1.0),
                translation_delta: [0.0; 3],
                rotation_delta: [0.0; 4],
            }]
        );
    }

    #[test]
    fn blend_optional_pose_uses_valid_side() {
        let valid = Pose {
            translation: [3.0, 4.0, 5.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
        };

        assert_eq!(
            blend_optional_pose(Some(valid), None, 0.5).expect("left"),
            valid
        );
        assert_eq!(
            blend_optional_pose(None, Some(valid), 0.5).expect("right"),
            valid
        );
        assert_eq!(
            blend_optional_pose(None, None, 0.5).expect_err("invalid"),
            AnimationError::InvalidQuaternion
        );
    }

    #[test]
    fn hierarchy_evaluates_parent_before_child_and_rejects_cycles() {
        let local = vec![
            Pose {
                translation: [1.0, 0.0, 0.0],
                rotation: [0.0, 0.0, 0.0, 1.0],
            },
            Pose {
                translation: [0.0, 2.0, 0.0],
                rotation: [0.0, 0.0, 0.0, 1.0],
            },
        ];

        let buffer = evaluate_hierarchy(&[ParentIndex(None), ParentIndex(Some(0))], &local)
            .expect("hierarchy");

        assert_eq!(buffer.poses[0].translation, [1.0, 0.0, 0.0]);
        assert_eq!(buffer.poses[1].translation, [1.0, 2.0, 0.0]);
        assert_eq!(
            evaluate_hierarchy(&[ParentIndex(Some(0))], &[Pose::default()]).expect_err("cycle"),
            AnimationError::ParentCycle { node: 0 }
        );
        assert_eq!(
            evaluate_hierarchy(
                &[ParentIndex(Some(1)), ParentIndex(None)],
                &[Pose::default(), Pose::default()],
            )
            .expect_err("order"),
            AnimationError::ParentOrder {
                child: 0,
                parent: 1,
            }
        );
    }

    #[test]
    fn generated_valid_quaternions_remain_finite() {
        for index in 1..64_u16 {
            let mut bytes = [0_u8; 24];
            bytes[12..16].copy_from_slice(&f32::from(index).to_bits().to_le_bytes());
            bytes[16..18].copy_from_slice(&(i16::try_from(index).expect("small")).to_le_bytes());
            bytes[18..20].copy_from_slice(&123_i16.to_le_bytes());
            bytes[20..22].copy_from_slice(&(-456_i16).to_le_bytes());
            bytes[22..24].copy_from_slice(&32767_i16.to_le_bytes());

            let key = AnimKey24::decode(&bytes).expect("key");

            assert!(key.pose.rotation.iter().all(|value| value.is_finite()));
        }
    }

    #[test]
    fn named_rng_stream_is_stable_and_named() {
        let mut material_a = NamedRngStream::new(42, "material");
        let mut material_b = NamedRngStream::new(42, "material");
        let mut fx = NamedRngStream::new(42, "fx");

        assert_eq!(material_a.next_u32(), material_b.next_u32());
        assert_ne!(material_a.next_u32(), fx.next_u32());
        assert_eq!(material_a.calls(), 2);
    }

    #[test]
    fn pose_capture_uses_float_bits() {
        let track = PoseTrack::new(
            Pose::default(),
            vec![PoseKey {
                frame: 0,
                pose: Pose::default(),
            }],
        )
        .expect("track");

        let capture = canonical_pose_capture(&track, &[AnimationTime(0.0)]).expect("capture");

        assert_eq!(
            capture,
            b"P,00000000,00000000,00000000,00000000,00000000,00000000,00000000,3f800000\n"
        );
    }

    #[test]
    fn timed_pose_capture_uses_float_bits() {
        let track = TimedPoseTrack::new(
            Pose::default(),
            vec![TimedPoseKey {
                time: AnimationTime(0.5),
                pose: Pose::default(),
            }],
        )
        .expect("track");

        let capture = canonical_timed_pose_capture(&track, &[AnimationTime(0.5)]).expect("capture");

        assert_eq!(
            capture,
            b"P,3f000000,00000000,00000000,00000000,00000000,00000000,00000000,3f800000\n"
        );
    }
}
