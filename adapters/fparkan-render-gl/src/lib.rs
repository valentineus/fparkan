#![forbid(unsafe_code)]
//! OpenGL render adapter proof behind safe `FParkan` render ports.

use fparkan_render::{
    canonical_capture, FrameOutput, RenderBackend, RenderCommandList, RenderError,
};

/// Portable OpenGL profile requested by the game composition root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GlProfile {
    /// Desktop OpenGL 3.3 Core.
    DesktopCore33,
    /// OpenGL ES 2.0 portable baseline.
    Gles2,
}

/// Shader stage used in diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShaderStage {
    /// Vertex shader.
    Vertex,
    /// Fragment shader.
    Fragment,
}

/// Shader compilation diagnostic surfaced by the adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShaderCompileError {
    /// Requested GL profile.
    pub profile: GlProfile,
    /// Shader stage.
    pub stage: ShaderStage,
    /// Backend compiler log.
    pub log: String,
}

impl std::fmt::Display for ShaderCompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:?} {:?} shader compile failed: {}",
            self.profile, self.stage, self.log
        )
    }
}

impl std::error::Error for ShaderCompileError {}

/// Adapter capabilities compiled into this package.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GlAdapterCapabilities {
    /// Supported profiles in preference order.
    pub profiles: Vec<GlProfile>,
    /// Whether adapter-owned code is free of `unsafe`.
    pub project_owned_unsafe_free: bool,
}

impl Default for GlAdapterCapabilities {
    fn default() -> Self {
        Self {
            profiles: vec![GlProfile::DesktopCore33, GlProfile::Gles2],
            project_owned_unsafe_free: true,
        }
    }
}

/// Returns adapter readiness status for the safe project-owned layer.
#[must_use]
pub fn safe_adapter_ready() -> bool {
    GlAdapterCapabilities::default().project_owned_unsafe_free
}

/// Validates shader source through the adapter diagnostic contract.
///
/// # Errors
///
/// Returns [`ShaderCompileError`] when the source is empty or contains a
/// deterministic synthetic failure marker.
pub fn compile_shader_source(
    profile: GlProfile,
    stage: ShaderStage,
    source: &str,
) -> Result<(), ShaderCompileError> {
    if source.trim().is_empty() {
        return Err(ShaderCompileError {
            profile,
            stage,
            log: "empty shader source".to_string(),
        });
    }
    if source.contains("#error") {
        return Err(ShaderCompileError {
            profile,
            stage,
            log: "synthetic compiler failure marker".to_string(),
        });
    }
    Ok(())
}

/// Safe render backend facade used for adapter-level command validation.
///
/// A concrete OpenGL implementation can be injected behind the same
/// [`RenderBackend`] port once an audited safe GL facade is selected. This type
/// keeps the project-owned adapter API executable without introducing local FFI.
#[derive(Clone, Debug)]
pub struct SafeGlCommandBackend {
    profile: GlProfile,
    captures: Vec<Vec<u8>>,
}

impl SafeGlCommandBackend {
    /// Creates a backend proof for a requested GL profile.
    #[must_use]
    pub fn new(profile: GlProfile) -> Self {
        Self {
            profile,
            captures: Vec::new(),
        }
    }

    /// Active GL profile.
    #[must_use]
    pub fn profile(&self) -> GlProfile {
        self.profile
    }

    /// Deterministic command captures produced by executed frames.
    #[must_use]
    pub fn captures(&self) -> &[Vec<u8>] {
        &self.captures
    }
}

impl RenderBackend for SafeGlCommandBackend {
    fn execute(&mut self, commands: &RenderCommandList) -> Result<FrameOutput, RenderError> {
        self.captures.push(canonical_capture(commands)?);
        Ok(FrameOutput)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fparkan_render::{
        DrawCommand, DrawId, GpuMaterialId, GpuMeshId, IndexRange, RenderCommand, RenderPhase,
    };

    #[test]
    fn adapter_reports_safe_project_layer_ready() {
        assert!(safe_adapter_ready());
        assert_eq!(GlAdapterCapabilities::default().profiles.len(), 2);
    }

    #[test]
    fn backend_executes_and_captures_commands() -> Result<(), RenderError> {
        let mut backend = SafeGlCommandBackend::new(GlProfile::Gles2);
        let commands = RenderCommandList {
            commands: vec![
                RenderCommand::BeginFrame,
                RenderCommand::Draw(DrawCommand {
                    id: DrawId(7),
                    phase: RenderPhase::Opaque,
                    object_id: None,
                    mesh: GpuMeshId(11),
                    material: GpuMaterialId(13),
                    transform: [0.0; 16],
                    range: IndexRange { start: 0, count: 3 },
                    stable_order: 17,
                }),
                RenderCommand::EndFrame,
            ],
        };

        backend.execute(&commands)?;

        assert_eq!(backend.profile(), GlProfile::Gles2);
        assert_eq!(backend.captures().len(), 1);
        Ok(())
    }

    #[test]
    fn desktop_gl33_triangle_command_capture() -> Result<(), RenderError> {
        let mut backend = SafeGlCommandBackend::new(GlProfile::DesktopCore33);
        let commands = triangle_commands();

        backend.execute(&commands)?;

        assert_eq!(backend.profile(), GlProfile::DesktopCore33);
        assert_eq!(
            backend.captures(),
            &[b"B\nD,Opaque,7,11,13,17\nE\n".to_vec()]
        );
        Ok(())
    }

    #[test]
    fn gles2_triangle_command_capture() -> Result<(), RenderError> {
        let mut backend = SafeGlCommandBackend::new(GlProfile::Gles2);
        let commands = triangle_commands();

        backend.execute(&commands)?;

        assert_eq!(backend.profile(), GlProfile::Gles2);
        assert_eq!(
            backend.captures(),
            &[b"B\nD,Opaque,7,11,13,17\nE\n".to_vec()]
        );
        Ok(())
    }

    #[test]
    fn shader_compile_failure_diagnostic_contains_profile_and_log() {
        let err = compile_shader_source(GlProfile::Gles2, ShaderStage::Fragment, "#error")
            .expect_err("shader failure");

        assert_eq!(err.profile, GlProfile::Gles2);
        assert_eq!(err.stage, ShaderStage::Fragment);
        assert!(err.log.contains("synthetic compiler failure"));
        assert!(err.to_string().contains("Gles2"));
        assert!(err.to_string().contains("synthetic compiler failure"));
    }

    fn triangle_commands() -> RenderCommandList {
        RenderCommandList {
            commands: vec![
                RenderCommand::BeginFrame,
                RenderCommand::Draw(DrawCommand {
                    id: DrawId(7),
                    phase: RenderPhase::Opaque,
                    object_id: None,
                    mesh: GpuMeshId(11),
                    material: GpuMaterialId(13),
                    transform: [0.0; 16],
                    range: IndexRange { start: 0, count: 3 },
                    stable_order: 17,
                }),
                RenderCommand::EndFrame,
            ],
        }
    }
}
