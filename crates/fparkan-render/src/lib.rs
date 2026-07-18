#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        clippy::cast_precision_loss,
        clippy::expect_used,
        clippy::float_cmp,
        clippy::identity_op,
        clippy::too_many_lines,
        clippy::uninlined_format_args,
        clippy::map_unwrap_or,
        clippy::needless_raw_string_hashes,
        clippy::semicolon_if_nothing_returned,
        clippy::type_complexity,
        clippy::panic,
        clippy::unwrap_used
    )
)]
//! Backend-neutral render commands and deterministic captures.

use fparkan_world::OriginalObjectId;

/// A 64-byte transform block returned by the original Terrain camera ABI.
///
/// The original uses two selector-dependent transform pointers.  Their exact
/// matrix convention is still under recovery, so words are deliberately kept
/// losslessly rather than treated as a renderer-ready matrix.  The three
/// translation words have been confirmed at indices 3, 7, and 11.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RawCameraTransform {
    /// The exact 16 little-endian dwords returned by the legacy camera.
    pub words: [u32; 16],
}

impl RawCameraTransform {
    /// Indices of the confirmed X, Y, and Z translation floats.
    pub const TRANSLATION_WORD_INDICES: [usize; 3] = [3, 7, 11];

    /// Returns the confirmed legacy world position (X, Y, Z).
    #[must_use]
    pub fn translation(self) -> [f32; 3] {
        Self::TRANSLATION_WORD_INDICES.map(|index| f32::from_bits(self.words[index]))
    }

    /// Inverts a finite row-major affine transform without assigning it a
    /// camera-space meaning.
    ///
    /// The legacy SIMD dispatch multiplies these blocks as ordinary row-major
    /// matrices and the confirmed camera samples have translation in the last
    /// column. `Some` therefore means only that this block has a non-singular
    /// affine inverse. Callers must still establish whether it is a
    /// camera-to-world transform before using the result as a view matrix.
    #[must_use]
    pub fn try_inverse_affine_row_major(self) -> Option<[f32; 16]> {
        let matrix = self.words.map(f32::from_bits);
        if !matrix.iter().all(|value| value.is_finite())
            || matrix[12].abs() > f32::EPSILON
            || matrix[13].abs() > f32::EPSILON
            || matrix[14].abs() > f32::EPSILON
            || (matrix[15] - 1.0).abs() > f32::EPSILON
        {
            return None;
        }

        let [m00, m01, m02, _, m10, m11, m12, _, m20, m21, m22, _, _, _, _, _] = matrix;
        let cofactor00 = m11.mul_add(m22, -(m12 * m21));
        let cofactor01 = m02.mul_add(m21, -(m01 * m22));
        let cofactor02 = m01.mul_add(m12, -(m02 * m11));
        let determinant = m00.mul_add(cofactor00, m10.mul_add(cofactor01, m20 * cofactor02));
        if !determinant.is_finite() || determinant == 0.0 {
            return None;
        }

        let inverse_determinant = determinant.recip();
        let inverse = [
            cofactor00 * inverse_determinant,
            m02.mul_add(m21, -(m01 * m22)) * inverse_determinant,
            cofactor02 * inverse_determinant,
            0.0,
            m12.mul_add(m20, -(m10 * m22)) * inverse_determinant,
            m00.mul_add(m22, -(m02 * m20)) * inverse_determinant,
            m02.mul_add(m10, -(m00 * m12)) * inverse_determinant,
            0.0,
            m10.mul_add(m21, -(m11 * m20)) * inverse_determinant,
            m01.mul_add(m20, -(m00 * m21)) * inverse_determinant,
            m00.mul_add(m11, -(m01 * m10)) * inverse_determinant,
            0.0,
            0.0,
            0.0,
            0.0,
            1.0,
        ];
        let [translation_x, translation_y, translation_z] = self.translation();
        let translation = [
            -(inverse[0].mul_add(
                translation_x,
                inverse[1].mul_add(translation_y, inverse[2] * translation_z),
            )),
            -(inverse[4].mul_add(
                translation_x,
                inverse[5].mul_add(translation_y, inverse[6] * translation_z),
            )),
            -(inverse[8].mul_add(
                translation_x,
                inverse[9].mul_add(translation_y, inverse[10] * translation_z),
            )),
        ];

        Some([
            inverse[0],
            inverse[1],
            inverse[2],
            translation[0],
            inverse[4],
            inverse[5],
            inverse[6],
            translation[1],
            inverse[8],
            inverse[9],
            inverse[10],
            translation[2],
            0.0,
            0.0,
            0.0,
            1.0,
        ])
    }
}

/// Raw camera state observed through the original Terrain camera interface.
///
/// Selector 0 supplies the currently active transform and selector 2 supplies
/// its paired transform.  Keeping both lets a later compatibility layer derive
/// a view/projection convention without re-reading the legacy process.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RawCameraPose {
    /// Transform returned by selector 0.
    pub selector0: RawCameraTransform,
    /// Transform returned by selector 2.
    pub selector2: RawCameraTransform,
}

/// Immutable camera data visible to command generation.
#[derive(Clone, Debug, PartialEq)]
pub struct CameraSnapshot {
    /// View matrix, row-major.
    pub view: [f32; 16],
    /// Projection matrix, row-major.
    pub projection: [f32; 16],
    /// Optional unconverted source-camera state.
    ///
    /// This does not alter rendering until the original matrix and projection
    /// conventions have been recovered; it preserves the ABI boundary for the
    /// runtime adapter and deterministic captures.
    pub raw_pose: Option<RawCameraPose>,
}

impl Default for CameraSnapshot {
    fn default() -> Self {
        Self {
            view: identity_transform(),
            projection: identity_transform(),
            raw_pose: None,
        }
    }
}

/// Draw id.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct DrawId(pub u64);

/// GPU mesh id.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GpuMeshId(pub u64);

/// GPU material id.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GpuMaterialId(pub u64);

/// Render phase.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum RenderPhase {
    /// Terrain.
    Terrain,
    /// Opaque.
    Opaque,
    /// Alpha test.
    AlphaTest,
    /// Transparent.
    Transparent,
    /// Effects.
    Effects,
    /// Debug.
    Debug,
    /// UI.
    Ui,
}

/// Fixed-function blend behaviour represented without a graphics API type.
///
/// This is a compatibility contract, not yet a decoded MAT0 mapping.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LegacyBlendMode {
    /// Do not blend the fragment with the existing colour.
    #[default]
    Opaque,
    /// Blend using source alpha.
    SourceAlpha,
}

/// Depth-buffer behaviour represented without a graphics API type.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LegacyDepthMode {
    /// No depth attachment is used.
    #[default]
    Disabled,
    /// Test depth and write passing fragments.
    TestWrite,
    /// Test depth without modifying it.
    TestReadOnly,
}

/// Triangle culling behaviour represented without a graphics API type.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LegacyCullMode {
    /// Keep both front- and back-facing triangles.
    #[default]
    Disabled,
    /// Cull back-facing triangles.
    BackFace,
    /// Cull front-facing triangles.
    FrontFace,
}

/// Legacy fixed-function state that changes graphics-pipeline structure.
///
/// Alpha reference is deliberately not present: it is dynamic material data,
/// whereas this state records only whether an alpha-test shader variant is used.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LegacyPipelineState {
    /// Colour blend mode.
    pub blend: LegacyBlendMode,
    /// Depth test/write mode.
    pub depth: LegacyDepthMode,
    /// Face culling mode.
    pub cull: LegacyCullMode,
    /// Whether alpha-test shader logic is enabled.
    pub alpha_test: bool,
}

/// Canonical, backend-neutral key for a graphics-pipeline variant.
///
/// The value is explicitly packed rather than hashed, so captures and caches
/// remain stable across processes and Rust toolchain updates.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PipelineKey(u8);

impl PipelineKey {
    /// Returns the canonical packed representation.
    #[must_use]
    pub const fn packed(self) -> u8 {
        self.0
    }
}

impl From<LegacyPipelineState> for PipelineKey {
    fn from(state: LegacyPipelineState) -> Self {
        let blend = match state.blend {
            LegacyBlendMode::Opaque => 0,
            LegacyBlendMode::SourceAlpha => 1,
        };
        let depth = match state.depth {
            LegacyDepthMode::Disabled => 0,
            LegacyDepthMode::TestWrite => 1,
            LegacyDepthMode::TestReadOnly => 2,
        };
        let cull = match state.cull {
            LegacyCullMode::Disabled => 0,
            LegacyCullMode::BackFace => 1,
            LegacyCullMode::FrontFace => 2,
        };
        Self(blend | (depth << 1) | (cull << 3) | (u8::from(state.alpha_test) << 5))
    }
}

/// Index range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndexRange {
    /// Start.
    pub start: u32,
    /// Count.
    pub count: u32,
}

/// A draw candidate in an immutable render snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct RenderSnapshotDraw {
    /// Draw id.
    pub id: DrawId,
    /// Phase.
    pub phase: RenderPhase,
    /// Object id.
    pub object_id: Option<OriginalObjectId>,
    /// Mesh.
    pub mesh: GpuMeshId,
    /// Material table after WEAR/MAT0 fallback resolution.
    pub material_slots: Vec<GpuMaterialId>,
    /// Batch material index into [`Self::material_slots`].
    pub material_index: u16,
    /// Fixed-function state resolved for this draw.
    pub pipeline_state: LegacyPipelineState,
    /// Node transform matrix, row-major.
    pub transform: [f32; 16],
    /// Index range.
    pub range: IndexRange,
    /// Stable sort order.
    pub stable_order: u64,
}

/// Immutable backend-neutral render snapshot.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RenderSnapshot {
    /// Camera data for the frame.
    pub camera: CameraSnapshot,
    /// Draw candidates gathered from world/assets.
    pub draws: Vec<RenderSnapshotDraw>,
}

/// Command generation profile.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RenderProfile {
    /// Include UI phase commands when present.
    pub include_ui: bool,
}

/// Draw command.
#[derive(Clone, Debug, PartialEq)]
pub struct DrawCommand {
    /// Draw id.
    pub id: DrawId,
    /// Phase.
    pub phase: RenderPhase,
    /// Object id.
    pub object_id: Option<OriginalObjectId>,
    /// Mesh.
    pub mesh: GpuMeshId,
    /// Material.
    pub material: GpuMaterialId,
    /// Canonical graphics-pipeline variant.
    pub pipeline_key: PipelineKey,
    /// Transform matrix, row-major.
    pub transform: [f32; 16],
    /// Index range.
    pub range: IndexRange,
    /// Stable sort order.
    pub stable_order: u64,
}

/// Render command.
#[derive(Clone, Debug, PartialEq)]
pub enum RenderCommand {
    /// Begin frame.
    BeginFrame,
    /// Draw.
    Draw(DrawCommand),
    /// End frame.
    EndFrame,
}

/// Render command list.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RenderCommandList {
    /// Commands.
    pub commands: Vec<RenderCommand>,
}

/// Optional render command validation limits.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RenderValidationLimits {
    /// Exclusive upper bound for GPU mesh ids.
    pub mesh_count: Option<u64>,
    /// Exclusive upper bound for index ranges.
    pub index_count: Option<u32>,
}

/// Frame output.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FrameOutput;

/// Render error.
#[derive(Debug)]
pub enum RenderError {
    /// Invalid range.
    InvalidRange,
    /// Invalid command stream framing or ordering.
    InvalidCommandStream {
        /// Command index.
        index: usize,
        /// Contextual error message.
        message: &'static str,
    },
    /// Invalid draw range with command-generation context.
    InvalidDrawRange {
        /// Draw id.
        draw_id: DrawId,
        /// Stable sort order.
        stable_order: u64,
        /// Range start.
        start: u32,
        /// Range count.
        count: u32,
    },
    /// Index range arithmetic overflow.
    IndexRangeOverflow {
        /// Draw id.
        draw_id: DrawId,
        /// Range start.
        start: u32,
        /// Range count.
        count: u32,
    },
    /// Index range exceeds validation limits.
    IndexRangeOutOfBounds {
        /// Draw id.
        draw_id: DrawId,
        /// Exclusive index limit.
        index_count: u32,
        /// Range end.
        end: u32,
    },
    /// Mesh id exceeds validation limits.
    MeshOutOfBounds {
        /// Draw id.
        draw_id: DrawId,
        /// Mesh id.
        mesh: GpuMeshId,
        /// Exclusive mesh limit.
        mesh_count: u64,
    },
    /// Draw transform contains a non-finite value.
    NonFiniteTransform {
        /// Draw id.
        draw_id: DrawId,
        /// Matrix element index.
        element: usize,
    },
    /// Draw commands are not ordered by phase, stable order and draw id.
    PhaseOrderViolation {
        /// Draw id.
        draw_id: DrawId,
        /// Previous phase.
        previous: RenderPhase,
        /// Current phase.
        current: RenderPhase,
    },
    /// A batch material index did not resolve through the material table.
    MaterialIndexOutOfBounds {
        /// Draw id.
        draw_id: DrawId,
        /// Requested material index.
        material_index: u16,
        /// Available material slots.
        material_count: usize,
    },
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRange => write!(f, "render command contains an empty index range"),
            Self::InvalidCommandStream { index, message } => {
                write!(
                    f,
                    "render command stream is invalid at command {index}: {message}"
                )
            }
            Self::InvalidDrawRange {
                draw_id,
                stable_order,
                start,
                count,
            } => write!(
                f,
                "draw {} has invalid index range start={} count={} at stable order {}",
                draw_id.0, start, count, stable_order
            ),
            Self::IndexRangeOverflow {
                draw_id,
                start,
                count,
            } => write!(
                f,
                "draw {} index range overflows start={} count={}",
                draw_id.0, start, count
            ),
            Self::IndexRangeOutOfBounds {
                draw_id,
                index_count,
                end,
            } => write!(
                f,
                "draw {} index range ends at {} but mesh has {} indices",
                draw_id.0, end, index_count
            ),
            Self::MeshOutOfBounds {
                draw_id,
                mesh,
                mesh_count,
            } => write!(
                f,
                "draw {} references mesh {} but only {} meshes are available",
                draw_id.0, mesh.0, mesh_count
            ),
            Self::NonFiniteTransform { draw_id, element } => write!(
                f,
                "draw {} has non-finite transform element {}",
                draw_id.0, element
            ),
            Self::PhaseOrderViolation {
                draw_id,
                previous,
                current,
            } => write!(
                f,
                "draw {} phase order regressed from {:?} to {:?}",
                draw_id.0, previous, current
            ),
            Self::MaterialIndexOutOfBounds {
                draw_id,
                material_index,
                material_count,
            } => write!(
                f,
                "draw {} references material index {} but only {} material slots are available",
                draw_id.0, material_index, material_count
            ),
        }
    }
}

impl std::error::Error for RenderError {}

/// Builds a deterministic command list from an immutable render snapshot.
///
/// # Errors
///
/// Returns [`RenderError`] when a draw has an invalid index range or a material
/// index that cannot be resolved through its material slot table.
pub fn build_commands(
    snapshot: &RenderSnapshot,
    profile: RenderProfile,
) -> Result<RenderCommandList, RenderError> {
    let mut draws = snapshot
        .draws
        .iter()
        .filter(|draw| profile.include_ui || draw.phase != RenderPhase::Ui)
        .collect::<Vec<_>>();
    draws.sort_by_key(|draw| (draw.phase, draw.stable_order, draw.id));

    let mut commands = Vec::with_capacity(draws.len() + 2);
    commands.push(RenderCommand::BeginFrame);
    for draw in draws {
        if draw.range.count == 0 {
            return Err(RenderError::InvalidDrawRange {
                draw_id: draw.id,
                stable_order: draw.stable_order,
                start: draw.range.start,
                count: draw.range.count,
            });
        }
        validate_index_range(draw.id, draw.range)?;
        validate_transform(draw.id, &draw.transform)?;
        let material = draw
            .material_slots
            .get(usize::from(draw.material_index))
            .copied()
            .ok_or(RenderError::MaterialIndexOutOfBounds {
                draw_id: draw.id,
                material_index: draw.material_index,
                material_count: draw.material_slots.len(),
            })?;
        commands.push(RenderCommand::Draw(DrawCommand {
            id: draw.id,
            phase: draw.phase,
            object_id: draw.object_id,
            mesh: draw.mesh,
            material,
            pipeline_key: draw.pipeline_state.into(),
            transform: draw.transform,
            range: draw.range,
            stable_order: draw.stable_order,
        }));
    }
    commands.push(RenderCommand::EndFrame);
    Ok(RenderCommandList { commands })
}

/// Backend port.
pub trait RenderBackend {
    /// Executes commands.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError`] when the command stream is malformed for the
    /// backend.
    fn execute(&mut self, commands: &RenderCommandList) -> Result<FrameOutput, RenderError>;
}

/// Marker trait for backends that execute draws against a live GPU.
///
/// Planning and capture-only backends must not implement this trait.
pub trait GpuRenderBackend: RenderBackend {}

/// Backend that validates commands and intentionally produces no pixels.
#[derive(Clone, Debug, Default)]
pub struct NullBackend;

impl RenderBackend for NullBackend {
    fn execute(&mut self, commands: &RenderCommandList) -> Result<FrameOutput, RenderError> {
        validate_command_list(commands)?;
        Ok(FrameOutput)
    }
}

/// Backend that stores deterministic command captures for verification.
#[derive(Clone, Debug, Default)]
pub struct RecordingBackend {
    captures: Vec<Vec<u8>>,
}

impl RecordingBackend {
    /// Returns all captures in submission order.
    #[must_use]
    pub fn captures(&self) -> &[Vec<u8>] {
        &self.captures
    }

    /// Returns the most recent capture.
    #[must_use]
    pub fn last_capture(&self) -> Option<&[u8]> {
        self.captures.last().map(Vec::as_slice)
    }

    /// Clears stored captures without changing backend behavior.
    pub fn clear(&mut self) {
        self.captures.clear();
    }
}

impl RenderBackend for RecordingBackend {
    fn execute(&mut self, commands: &RenderCommandList) -> Result<FrameOutput, RenderError> {
        let capture = canonical_capture(commands)?;
        self.captures.push(capture);
        Ok(FrameOutput)
    }
}

/// Builds a canonical capture.
///
/// # Errors
///
/// Returns [`RenderError`] when a draw command contains an invalid index range.
pub fn canonical_capture(commands: &RenderCommandList) -> Result<Vec<u8>, RenderError> {
    validate_command_list(commands)?;
    let mut out = Vec::new();
    for command in &commands.commands {
        match command {
            RenderCommand::BeginFrame => out.extend_from_slice(b"B\n"),
            RenderCommand::EndFrame => out.extend_from_slice(b"E\n"),
            RenderCommand::Draw(draw) => {
                out.extend_from_slice(
                    format!(
                        "D,{:?},{},{},{},{},{}\n",
                        draw.phase,
                        draw.id.0,
                        draw.mesh.0,
                        draw.material.0,
                        draw.pipeline_key.packed(),
                        draw.stable_order
                    )
                    .as_bytes(),
                );
            }
        }
    }
    Ok(out)
}

/// Validates a render command list without backend-specific resource limits.
///
/// # Errors
///
/// Returns [`RenderError`] when framing, ordering or draw data is invalid.
pub fn validate_command_list(commands: &RenderCommandList) -> Result<(), RenderError> {
    validate_command_list_with_limits(commands, RenderValidationLimits::default())
}

/// Validates a render command list with optional backend resource limits.
///
/// # Errors
///
/// Returns [`RenderError`] when framing, ordering, draw data or resource bounds
/// are invalid.
pub fn validate_command_list_with_limits(
    commands: &RenderCommandList,
    limits: RenderValidationLimits,
) -> Result<(), RenderError> {
    let Some(first) = commands.commands.first() else {
        return Err(RenderError::InvalidCommandStream {
            index: 0,
            message: "empty command list",
        });
    };
    if !matches!(first, RenderCommand::BeginFrame) {
        return Err(RenderError::InvalidCommandStream {
            index: 0,
            message: "first command must be BeginFrame",
        });
    }
    if commands.commands.len() < 2 {
        return Err(RenderError::InvalidCommandStream {
            index: 0,
            message: "frame must end with EndFrame",
        });
    }
    let end_index = commands.commands.len() - 1;
    if !matches!(commands.commands[end_index], RenderCommand::EndFrame) {
        return Err(RenderError::InvalidCommandStream {
            index: end_index,
            message: "last command must be EndFrame",
        });
    }

    let mut previous_key: Option<(RenderPhase, u64, DrawId)> = None;
    for (index, command) in commands.commands.iter().enumerate() {
        match command {
            RenderCommand::BeginFrame if index == 0 => {}
            RenderCommand::BeginFrame => {
                return Err(RenderError::InvalidCommandStream {
                    index,
                    message: "nested BeginFrame is not allowed",
                });
            }
            RenderCommand::EndFrame if index == end_index => {}
            RenderCommand::EndFrame => {
                return Err(RenderError::InvalidCommandStream {
                    index,
                    message: "EndFrame before final command is not allowed",
                });
            }
            RenderCommand::Draw(draw) => {
                validate_draw_command(draw, limits)?;
                let key = (draw.phase, draw.stable_order, draw.id);
                if let Some(previous) = previous_key {
                    if key < previous {
                        return Err(RenderError::PhaseOrderViolation {
                            draw_id: draw.id,
                            previous: previous.0,
                            current: draw.phase,
                        });
                    }
                }
                previous_key = Some(key);
            }
        }
    }
    Ok(())
}

fn validate_draw_command(
    draw: &DrawCommand,
    limits: RenderValidationLimits,
) -> Result<(), RenderError> {
    if draw.range.count == 0 {
        return Err(RenderError::InvalidRange);
    }
    let end = validate_index_range(draw.id, draw.range)?;
    validate_transform(draw.id, &draw.transform)?;
    if let Some(mesh_count) = limits.mesh_count {
        if draw.mesh.0 >= mesh_count {
            return Err(RenderError::MeshOutOfBounds {
                draw_id: draw.id,
                mesh: draw.mesh,
                mesh_count,
            });
        }
    }
    if let Some(index_count) = limits.index_count {
        if end > index_count {
            return Err(RenderError::IndexRangeOutOfBounds {
                draw_id: draw.id,
                index_count,
                end,
            });
        }
    }
    Ok(())
}

fn validate_index_range(draw_id: DrawId, range: IndexRange) -> Result<u32, RenderError> {
    range
        .start
        .checked_add(range.count)
        .ok_or(RenderError::IndexRangeOverflow {
            draw_id,
            start: range.start,
            count: range.count,
        })
}

fn validate_transform(draw_id: DrawId, transform: &[f32; 16]) -> Result<(), RenderError> {
    for (element, value) in transform.iter().enumerate() {
        if !value.is_finite() {
            return Err(RenderError::NonFiniteTransform { draw_id, element });
        }
    }
    Ok(())
}

fn identity_transform() -> [f32; 16] {
    [
        1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn multiply_row_major(left: [f32; 16], right: [f32; 16]) -> [f32; 16] {
        let mut result = [0.0; 16];
        for row in 0..4 {
            for column in 0..4 {
                result[row * 4 + column] = (0..4)
                    .map(|index| left[row * 4 + index] * right[index * 4 + column])
                    .sum();
            }
        }
        result
    }

    fn assert_matrix_approximately_identity(matrix: [f32; 16]) {
        for (index, value) in matrix.into_iter().enumerate() {
            let expected = if index / 4 == index % 4 { 1.0 } else { 0.0 };
            assert!(
                (value - expected).abs() < 0.000_02,
                "matrix element {index}: expected {expected}, got {value}"
            );
        }
    }

    #[test]
    fn raw_camera_pose_preserves_words_and_extracts_confirmed_translation() {
        let mut active = [0_u32; 16];
        active[0] = 0x7FC0_0001;
        active[3] = 491.562_5_f32.to_bits();
        active[7] = 761.550_8_f32.to_bits();
        active[11] = 7.361_0_f32.to_bits();
        let paired = [0xA5A5_5A5A; 16];

        let pose = RawCameraPose {
            selector0: RawCameraTransform { words: active },
            selector2: RawCameraTransform { words: paired },
        };

        assert_eq!(pose.selector0.words, active);
        assert_eq!(pose.selector2.words, paired);
        assert_eq!(
            pose.selector0.translation(),
            [491.562_5, 761.550_8, 7.361_0]
        );
        assert_eq!(CameraSnapshot::default().raw_pose, None);
    }

    #[test]
    fn raw_camera_transform_inverts_only_non_singular_affine_blocks() {
        let source = [
            0.0, -1.0, 0.0, 433.544_7, 0.948_985, 0.0, 0.315_322, 652.292_5, -0.315_322, 0.0,
            0.948_985, 10.673_42, 0.0, 0.0, 0.0, 1.0,
        ];
        let transform = RawCameraTransform {
            words: source.map(f32::to_bits),
        };
        let inverse = transform
            .try_inverse_affine_row_major()
            .expect("observed affine transform is invertible");

        assert_matrix_approximately_identity(multiply_row_major(source, inverse));
        assert_matrix_approximately_identity(multiply_row_major(inverse, source));

        let singular = RawCameraTransform { words: [0_u32; 16] };
        assert_eq!(singular.try_inverse_affine_row_major(), None);
    }

    fn snapshot_draw(
        id: u64,
        phase: RenderPhase,
        material_index: u16,
        stable_order: u64,
    ) -> RenderSnapshotDraw {
        RenderSnapshotDraw {
            id: DrawId(id),
            phase,
            object_id: Some(OriginalObjectId(u32::try_from(id).expect("id fits"))),
            mesh: GpuMeshId(10 + id),
            material_slots: vec![GpuMaterialId(31), GpuMaterialId(37)],
            material_index,
            pipeline_state: LegacyPipelineState::default(),
            transform: identity_transform(),
            range: IndexRange { start: 0, count: 3 },
            stable_order,
        }
    }

    #[test]
    fn capture_is_stable() {
        let list = RenderCommandList {
            commands: vec![
                RenderCommand::BeginFrame,
                RenderCommand::Draw(DrawCommand {
                    id: DrawId(1),
                    phase: RenderPhase::Opaque,
                    object_id: None,
                    mesh: GpuMeshId(2),
                    material: GpuMaterialId(3),
                    pipeline_key: LegacyPipelineState::default().into(),
                    transform: [0.0; 16],
                    range: IndexRange { start: 0, count: 3 },
                    stable_order: 4,
                }),
                RenderCommand::EndFrame,
            ],
        };
        assert_eq!(
            canonical_capture(&list).expect("capture"),
            b"B\nD,Opaque,1,2,3,0,4\nE\n"
        );
    }

    #[test]
    fn pipeline_key_is_explicit_stable_and_sensitive_to_pipeline_structure() {
        let base = LegacyPipelineState::default();
        assert_eq!(PipelineKey::from(base).packed(), 0);

        let variant = LegacyPipelineState {
            blend: LegacyBlendMode::SourceAlpha,
            depth: LegacyDepthMode::TestReadOnly,
            cull: LegacyCullMode::BackFace,
            alpha_test: true,
        };
        assert_eq!(PipelineKey::from(variant).packed(), 0b00_101_101);
        assert_ne!(PipelineKey::from(base), PipelineKey::from(variant));
    }

    #[test]
    fn alpha_test_flag_changes_key_without_encoding_material_threshold() {
        let opaque = LegacyPipelineState::default();
        let alpha_test = LegacyPipelineState {
            alpha_test: true,
            ..opaque
        };
        assert_eq!(PipelineKey::from(alpha_test).packed(), 0b10_0000);
    }

    #[test]
    fn null_backend_validates_without_capture() {
        let mut backend = NullBackend;
        let invalid = RenderCommandList {
            commands: vec![
                RenderCommand::BeginFrame,
                RenderCommand::Draw(DrawCommand {
                    id: DrawId(1),
                    phase: RenderPhase::Opaque,
                    object_id: None,
                    mesh: GpuMeshId(2),
                    material: GpuMaterialId(3),
                    pipeline_key: LegacyPipelineState::default().into(),
                    transform: [0.0; 16],
                    range: IndexRange { start: 0, count: 0 },
                    stable_order: 4,
                }),
                RenderCommand::EndFrame,
            ],
        };

        assert!(matches!(
            backend.execute(&invalid),
            Err(RenderError::InvalidRange)
        ));
    }

    #[test]
    fn recording_backend_stores_captures() {
        let mut backend = RecordingBackend::default();
        let list = RenderCommandList {
            commands: vec![RenderCommand::BeginFrame, RenderCommand::EndFrame],
        };

        backend.execute(&list).expect("execute");
        backend.execute(&list).expect("execute");

        assert_eq!(backend.captures().len(), 2);
        assert_eq!(backend.last_capture(), Some(&b"B\nE\n"[..]));
        backend.clear();
        assert!(backend.captures().is_empty());
    }

    #[test]
    fn one_snapshot_draw_produces_one_draw_command() -> Result<(), RenderError> {
        let snapshot = RenderSnapshot {
            camera: CameraSnapshot::default(),
            draws: vec![snapshot_draw(1, RenderPhase::Opaque, 0, 10)],
        };

        let commands = build_commands(&snapshot, RenderProfile::default())?;

        assert!(matches!(commands.commands[0], RenderCommand::BeginFrame));
        assert!(matches!(commands.commands[2], RenderCommand::EndFrame));
        let RenderCommand::Draw(draw) = &commands.commands[1] else {
            panic!("expected draw");
        };
        assert_eq!(draw.id, DrawId(1));
        assert_eq!(draw.mesh, GpuMeshId(11));
        assert_eq!(draw.range, IndexRange { start: 0, count: 3 });
        Ok(())
    }

    #[test]
    fn material_index_maps_through_resolved_material_slots() -> Result<(), RenderError> {
        let snapshot = RenderSnapshot {
            camera: CameraSnapshot::default(),
            draws: vec![snapshot_draw(2, RenderPhase::Opaque, 1, 10)],
        };

        let commands = build_commands(&snapshot, RenderProfile::default())?;

        let RenderCommand::Draw(draw) = &commands.commands[1] else {
            panic!("expected draw");
        };
        assert_eq!(draw.material, GpuMaterialId(37));
        Ok(())
    }

    #[test]
    fn node_transform_is_retained() -> Result<(), RenderError> {
        let mut draw = snapshot_draw(3, RenderPhase::Opaque, 0, 10);
        draw.transform[3] = 12.5;
        draw.transform[7] = -4.0;
        let snapshot = RenderSnapshot {
            camera: CameraSnapshot::default(),
            draws: vec![draw],
        };

        let commands = build_commands(&snapshot, RenderProfile::default())?;

        let RenderCommand::Draw(draw) = &commands.commands[1] else {
            panic!("expected draw");
        };
        assert_eq!(draw.transform[3], 12.5);
        assert_eq!(draw.transform[7], -4.0);
        Ok(())
    }

    #[test]
    fn command_order_uses_phase_then_stable_key() -> Result<(), RenderError> {
        let snapshot = RenderSnapshot {
            camera: CameraSnapshot::default(),
            draws: vec![
                snapshot_draw(3, RenderPhase::Transparent, 0, 0),
                snapshot_draw(2, RenderPhase::Opaque, 0, 20),
                snapshot_draw(1, RenderPhase::Opaque, 0, 10),
            ],
        };

        let commands = build_commands(&snapshot, RenderProfile::default())?;
        let capture = canonical_capture(&commands)?;

        assert_eq!(
            capture,
            b"B\nD,Opaque,1,11,31,0,10\nD,Opaque,2,12,31,0,20\nD,Transparent,3,13,31,0,0\nE\n"
        );
        Ok(())
    }

    #[test]
    fn command_capture_independent_of_snapshot_construction_order() -> Result<(), RenderError> {
        let forward = RenderSnapshot {
            camera: CameraSnapshot::default(),
            draws: vec![
                snapshot_draw(1, RenderPhase::Opaque, 0, 10),
                snapshot_draw(2, RenderPhase::Opaque, 1, 20),
            ],
        };
        let reverse = RenderSnapshot {
            camera: CameraSnapshot::default(),
            draws: vec![
                snapshot_draw(2, RenderPhase::Opaque, 1, 20),
                snapshot_draw(1, RenderPhase::Opaque, 0, 10),
            ],
        };

        assert_eq!(
            canonical_capture(&build_commands(&forward, RenderProfile::default())?)?,
            canonical_capture(&build_commands(&reverse, RenderProfile::default())?)?
        );
        Ok(())
    }

    #[test]
    fn invalid_range_returns_contextual_error() {
        let mut draw = snapshot_draw(9, RenderPhase::Opaque, 0, 10);
        draw.range = IndexRange { start: 4, count: 0 };
        let snapshot = RenderSnapshot {
            camera: CameraSnapshot::default(),
            draws: vec![draw],
        };

        assert!(matches!(
            build_commands(&snapshot, RenderProfile::default()),
            Err(RenderError::InvalidDrawRange {
                draw_id: DrawId(9),
                stable_order: 10,
                start: 4,
                count: 0
            })
        ));
    }

    #[test]
    fn command_validation_rejects_bad_frame_framing() {
        let missing_begin = RenderCommandList {
            commands: vec![RenderCommand::EndFrame],
        };
        assert!(matches!(
            validate_command_list(&missing_begin),
            Err(RenderError::InvalidCommandStream {
                index: 0,
                message: "first command must be BeginFrame"
            })
        ));

        let nested = RenderCommandList {
            commands: vec![
                RenderCommand::BeginFrame,
                RenderCommand::BeginFrame,
                RenderCommand::EndFrame,
            ],
        };
        assert!(matches!(
            validate_command_list(&nested),
            Err(RenderError::InvalidCommandStream {
                index: 1,
                message: "nested BeginFrame is not allowed"
            })
        ));
    }

    #[test]
    fn command_validation_rejects_nonfinite_transform_and_range_overflow() {
        let mut draw = snapshot_draw(10, RenderPhase::Opaque, 0, 10);
        draw.transform[5] = f32::NAN;
        let nonfinite = build_commands(
            &RenderSnapshot {
                camera: CameraSnapshot::default(),
                draws: vec![draw],
            },
            RenderProfile::default(),
        );
        assert!(matches!(
            nonfinite,
            Err(RenderError::NonFiniteTransform {
                draw_id: DrawId(10),
                element: 5
            })
        ));

        let list = RenderCommandList {
            commands: vec![
                RenderCommand::BeginFrame,
                RenderCommand::Draw(DrawCommand {
                    id: DrawId(11),
                    phase: RenderPhase::Opaque,
                    object_id: None,
                    mesh: GpuMeshId(2),
                    material: GpuMaterialId(3),
                    pipeline_key: LegacyPipelineState::default().into(),
                    transform: identity_transform(),
                    range: IndexRange {
                        start: u32::MAX,
                        count: 1,
                    },
                    stable_order: 4,
                }),
                RenderCommand::EndFrame,
            ],
        };
        assert!(matches!(
            validate_command_list(&list),
            Err(RenderError::IndexRangeOverflow {
                draw_id: DrawId(11),
                start: u32::MAX,
                count: 1
            })
        ));
    }

    #[test]
    fn command_validation_checks_order_and_resource_bounds() {
        let ordered = build_commands(
            &RenderSnapshot {
                camera: CameraSnapshot::default(),
                draws: vec![snapshot_draw(1, RenderPhase::Opaque, 0, 10)],
            },
            RenderProfile::default(),
        )
        .expect("commands");
        assert!(matches!(
            validate_command_list_with_limits(
                &ordered,
                RenderValidationLimits {
                    mesh_count: Some(5),
                    index_count: Some(16)
                }
            ),
            Err(RenderError::MeshOutOfBounds {
                draw_id: DrawId(1),
                mesh: GpuMeshId(11),
                mesh_count: 5
            })
        ));

        let out_of_bounds = RenderCommandList {
            commands: vec![
                RenderCommand::BeginFrame,
                RenderCommand::Draw(DrawCommand {
                    id: DrawId(12),
                    phase: RenderPhase::Opaque,
                    object_id: None,
                    mesh: GpuMeshId(2),
                    material: GpuMaterialId(3),
                    pipeline_key: LegacyPipelineState::default().into(),
                    transform: identity_transform(),
                    range: IndexRange {
                        start: 14,
                        count: 3,
                    },
                    stable_order: 4,
                }),
                RenderCommand::EndFrame,
            ],
        };
        assert!(matches!(
            validate_command_list_with_limits(
                &out_of_bounds,
                RenderValidationLimits {
                    mesh_count: Some(5),
                    index_count: Some(16)
                }
            ),
            Err(RenderError::IndexRangeOutOfBounds {
                draw_id: DrawId(12),
                index_count: 16,
                end: 17
            })
        ));

        let unordered = RenderCommandList {
            commands: vec![
                RenderCommand::BeginFrame,
                RenderCommand::Draw(DrawCommand {
                    id: DrawId(1),
                    phase: RenderPhase::Transparent,
                    object_id: None,
                    mesh: GpuMeshId(1),
                    material: GpuMaterialId(1),
                    pipeline_key: LegacyPipelineState::default().into(),
                    transform: identity_transform(),
                    range: IndexRange { start: 0, count: 3 },
                    stable_order: 0,
                }),
                RenderCommand::Draw(DrawCommand {
                    id: DrawId(2),
                    phase: RenderPhase::Opaque,
                    object_id: None,
                    mesh: GpuMeshId(1),
                    material: GpuMaterialId(1),
                    pipeline_key: LegacyPipelineState::default().into(),
                    transform: identity_transform(),
                    range: IndexRange { start: 0, count: 3 },
                    stable_order: 0,
                }),
                RenderCommand::EndFrame,
            ],
        };
        assert!(matches!(
            validate_command_list(&unordered),
            Err(RenderError::PhaseOrderViolation {
                draw_id: DrawId(2),
                previous: RenderPhase::Transparent,
                current: RenderPhase::Opaque
            })
        ));
    }

    #[test]
    fn render_error_display_is_actionable() {
        assert_eq!(
            RenderError::InvalidDrawRange {
                draw_id: DrawId(9),
                stable_order: 10,
                start: 4,
                count: 0
            }
            .to_string(),
            "draw 9 has invalid index range start=4 count=0 at stable order 10"
        );
        assert_eq!(
            RenderError::MaterialIndexOutOfBounds {
                draw_id: DrawId(7),
                material_index: 3,
                material_count: 2
            }
            .to_string(),
            "draw 7 references material index 3 but only 2 material slots are available"
        );
    }

    #[test]
    fn ui_phase_is_excluded_until_requested() -> Result<(), RenderError> {
        let snapshot = RenderSnapshot {
            camera: CameraSnapshot::default(),
            draws: vec![
                snapshot_draw(1, RenderPhase::Opaque, 0, 10),
                snapshot_draw(2, RenderPhase::Ui, 0, 20),
            ],
        };

        let default_commands = build_commands(&snapshot, RenderProfile::default())?;
        let ui_commands = build_commands(&snapshot, RenderProfile { include_ui: true })?;

        assert_eq!(default_commands.commands.len(), 3);
        assert_eq!(ui_commands.commands.len(), 4);
        Ok(())
    }
}
