#![forbid(unsafe_code)]
//! Backend-neutral render commands and deterministic captures.

use fparkan_world::OriginalObjectId;

/// Immutable camera data visible to command generation.
#[derive(Clone, Debug, PartialEq)]
pub struct CameraSnapshot {
    /// View matrix, row-major.
    pub view: [f32; 16],
    /// Projection matrix, row-major.
    pub projection: [f32; 16],
}

impl Default for CameraSnapshot {
    fn default() -> Self {
        Self {
            view: identity_transform(),
            projection: identity_transform(),
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
                        "D,{:?},{},{},{},{}\n",
                        draw.phase, draw.id.0, draw.mesh.0, draw.material.0, draw.stable_order
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
                    transform: [0.0; 16],
                    range: IndexRange { start: 0, count: 3 },
                    stable_order: 4,
                }),
                RenderCommand::EndFrame,
            ],
        };
        assert_eq!(
            canonical_capture(&list).expect("capture"),
            b"B\nD,Opaque,1,2,3,4\nE\n"
        );
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
            b"B\nD,Opaque,1,11,31,10\nD,Opaque,2,12,31,20\nD,Transparent,3,13,31,0\nE\n"
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
