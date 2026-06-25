use fparkan_binary::{sha256, sha256_hex};
use serde::Serialize;

pub(crate) use crate::ffi::{
    SPIRV_MAGIC, SPIRV_VERSION_1_0, TRIANGLE_FRAGMENT_SHADER_WORDS, TRIANGLE_VERTEX_SHADER_WORDS,
};
use crate::policy::serialize_json_or_fallback;

pub(crate) const SHADER_MANIFEST_SCHEMA: u32 = 2;
pub(crate) const SHADER_TARGET_ENV: &str = "vulkan1.0";
pub(crate) const SHADER_COMPILER_NAME: &str = "glslangValidator";
pub(crate) const SHADER_COMPILER_VERSION: &str = "11:16.3.0";
pub(crate) const SHADER_COMPILER_BINARY_SHA256: &str =
    "9bcd69d830b350aaa6e2254915ff74e46070e217b67f38daad27c1fc1f22910f";
pub(crate) const SPIRV_VALIDATOR_NAME: &str = "spirv-val";
pub(crate) const SPIRV_VALIDATOR_VERSION: &str =
    "SPIRV-Tools v2026.2 unknown hash, 2026-04-29T17:02:58+00:00";
pub(crate) const SPIRV_VALIDATOR_BINARY_SHA256: &str =
    "f6d5b96ff19f073f3af0c0bcfa0c18702d288d3ec598efc242d01cd104d8354f";
pub(crate) const TRIANGLE_VERTEX_SOURCE_PATH: &str =
    "adapters/fparkan-render-vulkan/shaders/triangle.vert";
pub(crate) const TRIANGLE_VERTEX_SOURCE_SHA256: &str =
    "1e57f14d193fc61457c0749081c452ad25669998913107df12f3ccc3c33e0341";
pub(crate) const TRIANGLE_VERTEX_SPIRV_PATH: &str =
    "adapters/fparkan-render-vulkan/shaders/triangle.vert.spv";
pub(crate) const TRIANGLE_VERTEX_COMPILE_COMMAND: &str =
    "glslangValidator -V -S vert -e main adapters/fparkan-render-vulkan/shaders/triangle.vert -o adapters/fparkan-render-vulkan/shaders/triangle.vert.spv";
pub(crate) const TRIANGLE_VERTEX_VALIDATE_COMMAND: &str =
    "spirv-val --target-env vulkan1.0 adapters/fparkan-render-vulkan/shaders/triangle.vert.spv";
const TRIANGLE_FRAGMENT_SOURCE_PATH: &str = "adapters/fparkan-render-vulkan/shaders/triangle.frag";
const TRIANGLE_FRAGMENT_SOURCE_SHA256: &str =
    "f19e74d001d07fb537d4b0f9e621f9b8bc40eeb68816130220853abea6bd4445";
const TRIANGLE_FRAGMENT_SPIRV_PATH: &str =
    "adapters/fparkan-render-vulkan/shaders/triangle.frag.spv";
const TRIANGLE_FRAGMENT_COMPILE_COMMAND: &str =
    "glslangValidator -V -S frag -e main adapters/fparkan-render-vulkan/shaders/triangle.frag -o adapters/fparkan-render-vulkan/shaders/triangle.frag.spv";
const TRIANGLE_FRAGMENT_VALIDATE_COMMAND: &str =
    "spirv-val --target-env vulkan1.0 adapters/fparkan-render-vulkan/shaders/triangle.frag.spv";

fn shader_compiler_name() -> &'static str {
    option_env!("FPARKAN_BUILD_SHADER_COMPILER_NAME").unwrap_or(SHADER_COMPILER_NAME)
}

fn shader_compiler_version() -> &'static str {
    option_env!("FPARKAN_BUILD_SHADER_COMPILER_VERSION").unwrap_or(SHADER_COMPILER_VERSION)
}

fn shader_compiler_binary_sha256() -> &'static str {
    option_env!("FPARKAN_BUILD_SHADER_COMPILER_SHA256").unwrap_or(SHADER_COMPILER_BINARY_SHA256)
}

fn spirv_validator_name() -> &'static str {
    option_env!("FPARKAN_BUILD_SPIRV_VALIDATOR_NAME").unwrap_or(SPIRV_VALIDATOR_NAME)
}

fn spirv_validator_version() -> &'static str {
    option_env!("FPARKAN_BUILD_SPIRV_VALIDATOR_VERSION").unwrap_or(SPIRV_VALIDATOR_VERSION)
}

fn spirv_validator_binary_sha256() -> &'static str {
    option_env!("FPARKAN_BUILD_SPIRV_VALIDATOR_SHA256").unwrap_or(SPIRV_VALIDATOR_BINARY_SHA256)
}

/// Shader tool metadata pinned in the Stage 0 manifest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct VulkanShaderToolManifest {
    /// Tool executable name.
    pub name: &'static str,
    /// Tool version string.
    pub version: &'static str,
    /// Tool binary SHA-256.
    pub binary_sha256: &'static str,
}

/// Vulkan shader stage.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VulkanShaderStage {
    /// Vertex stage.
    Vertex,
    /// Fragment stage.
    Fragment,
}

/// Offline SPIR-V shader manifest entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VulkanShaderModuleManifest {
    /// Logical shader name.
    pub name: &'static str,
    /// Shader stage.
    pub stage: VulkanShaderStage,
    /// SPIR-V entry point.
    pub entry_point: &'static str,
    /// Descriptor set count.
    pub descriptor_sets: u32,
    /// Push constant byte count.
    pub push_constant_bytes: u32,
    /// Checked-in GLSL source path.
    pub source_path: &'static str,
    /// Checked-in GLSL source SHA-256.
    pub source_sha256: &'static str,
    /// Checked-in SPIR-V module path.
    pub spirv_path: &'static str,
    /// Exact offline compile command used for the checked-in SPIR-V artifact.
    pub compile_command: &'static str,
    /// Exact offline validation command used for the checked-in SPIR-V artifact.
    pub validate_command: &'static str,
    /// SPIR-V words.
    pub words: &'static [u32],
}

/// Shader manifest validation report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VulkanShaderManifestReport {
    /// Report schema version.
    pub schema: u32,
    /// Explicit Vulkan target environment for the checked-in SPIR-V.
    pub target_env: &'static str,
    /// Pinned compiler metadata.
    pub compiler: VulkanShaderToolManifest,
    /// Pinned validator metadata.
    pub validator: VulkanShaderToolManifest,
    /// Shader module reports.
    pub modules: Vec<VulkanShaderModuleReport>,
    /// Hash of the normalized shader manifest.
    pub manifest_hash: String,
}

/// Shader module validation report.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct VulkanShaderModuleReport {
    /// Logical shader name.
    pub name: &'static str,
    /// Shader stage.
    pub stage: VulkanShaderStage,
    /// SPIR-V entry point.
    pub entry_point: &'static str,
    /// Checked-in GLSL source path.
    pub source_path: &'static str,
    /// Checked-in GLSL source SHA-256.
    pub source_sha256: &'static str,
    /// Checked-in SPIR-V module path.
    pub spirv_path: &'static str,
    /// SPIR-V word count.
    pub word_count: usize,
    /// SPIR-V byte hash.
    pub sha256: String,
    /// Descriptor set count.
    pub descriptor_sets: u32,
    /// Push constant byte count.
    pub push_constant_bytes: u32,
    /// Exact offline compile command used for the checked-in SPIR-V artifact.
    pub compile_command: &'static str,
    /// Exact offline validation command used for the checked-in SPIR-V artifact.
    pub validate_command: &'static str,
    /// Stable hash of the reflected interface contract for this module.
    pub interface_hash: String,
}

/// Shader manifest validation error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VulkanShaderManifestError {
    /// SPIR-V module is too short to contain a header.
    TooShort {
        /// Shader name.
        name: &'static str,
    },
    /// SPIR-V module has an invalid magic word.
    InvalidMagic {
        /// Shader name.
        name: &'static str,
        /// Found magic word.
        found: u32,
    },
    /// SPIR-V module version is below 1.0.
    UnsupportedVersion {
        /// Shader name.
        name: &'static str,
        /// Found version word.
        found: u32,
    },
    /// SPIR-V module declares an invalid bound.
    InvalidBound {
        /// Shader name.
        name: &'static str,
    },
}

impl std::fmt::Display for VulkanShaderManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooShort { name } => write!(f, "shader {name} SPIR-V module is too short"),
            Self::InvalidMagic { name, found } => {
                write!(f, "shader {name} has invalid SPIR-V magic 0x{found:08x}")
            }
            Self::UnsupportedVersion { name, found } => write!(
                f,
                "shader {name} has unsupported SPIR-V version 0x{found:08x}"
            ),
            Self::InvalidBound { name } => write!(f, "shader {name} has invalid SPIR-V bound"),
        }
    }
}

impl std::error::Error for VulkanShaderManifestError {}

/// Returns the built-in Stage 0 indexed-triangle shader manifest.
#[must_use]
pub fn triangle_shader_manifest() -> Vec<VulkanShaderModuleManifest> {
    vec![
        VulkanShaderModuleManifest {
            name: "triangle.vert",
            stage: VulkanShaderStage::Vertex,
            entry_point: "main",
            descriptor_sets: 0,
            push_constant_bytes: 0,
            source_path: TRIANGLE_VERTEX_SOURCE_PATH,
            source_sha256: TRIANGLE_VERTEX_SOURCE_SHA256,
            spirv_path: TRIANGLE_VERTEX_SPIRV_PATH,
            compile_command: TRIANGLE_VERTEX_COMPILE_COMMAND,
            validate_command: TRIANGLE_VERTEX_VALIDATE_COMMAND,
            words: TRIANGLE_VERTEX_SHADER_WORDS,
        },
        VulkanShaderModuleManifest {
            name: "triangle.frag",
            stage: VulkanShaderStage::Fragment,
            entry_point: "main",
            descriptor_sets: 0,
            push_constant_bytes: 0,
            source_path: TRIANGLE_FRAGMENT_SOURCE_PATH,
            source_sha256: TRIANGLE_FRAGMENT_SOURCE_SHA256,
            spirv_path: TRIANGLE_FRAGMENT_SPIRV_PATH,
            compile_command: TRIANGLE_FRAGMENT_COMPILE_COMMAND,
            validate_command: TRIANGLE_FRAGMENT_VALIDATE_COMMAND,
            words: TRIANGLE_FRAGMENT_SHADER_WORDS,
        },
    ]
}

/// Validates shader SPIR-V containers and renders a deterministic report.
///
/// # Errors
///
/// Returns [`VulkanShaderManifestError`] when a module fails Stage 0 SPIR-V
/// container validation.
pub fn validate_shader_manifest(
    modules: &[VulkanShaderModuleManifest],
) -> Result<VulkanShaderManifestReport, VulkanShaderManifestError> {
    let mut reports = Vec::with_capacity(modules.len());
    for module in modules {
        validate_spirv_container(module)?;
        let bytes = spirv_words_to_bytes(module.words);
        reports.push(VulkanShaderModuleReport {
            name: module.name,
            stage: module.stage,
            entry_point: module.entry_point,
            source_path: module.source_path,
            source_sha256: module.source_sha256,
            spirv_path: module.spirv_path,
            word_count: module.words.len(),
            sha256: sha256_hex(&sha256(&bytes)),
            descriptor_sets: module.descriptor_sets,
            push_constant_bytes: module.push_constant_bytes,
            compile_command: module.compile_command,
            validate_command: module.validate_command,
            interface_hash: shader_interface_hash(module),
        });
    }
    let normalized = render_shader_manifest_without_hash_json(&reports);
    Ok(VulkanShaderManifestReport {
        schema: SHADER_MANIFEST_SCHEMA,
        target_env: SHADER_TARGET_ENV,
        compiler: VulkanShaderToolManifest {
            name: shader_compiler_name(),
            version: shader_compiler_version(),
            binary_sha256: shader_compiler_binary_sha256(),
        },
        validator: VulkanShaderToolManifest {
            name: spirv_validator_name(),
            version: spirv_validator_version(),
            binary_sha256: spirv_validator_binary_sha256(),
        },
        modules: reports,
        manifest_hash: sha256_hex(&sha256(normalized.as_bytes())),
    })
}

/// Renders a deterministic JSON shader manifest report.
#[must_use]
pub fn render_shader_manifest_report_json(report: &VulkanShaderManifestReport) -> String {
    #[derive(Serialize)]
    struct ShaderManifestReportJson<'a> {
        schema: u32,
        target_env: &'a str,
        compiler: &'a VulkanShaderToolManifest,
        validator: &'a VulkanShaderToolManifest,
        modules: &'a [VulkanShaderModuleReport],
        manifest_hash: &'a str,
    }

    serialize_json_or_fallback(
        &ShaderManifestReportJson {
            schema: report.schema,
            target_env: report.target_env,
            compiler: &report.compiler,
            validator: &report.validator,
            modules: &report.modules,
            manifest_hash: &report.manifest_hash,
        },
        "{\"schema\":0,\"target_env\":\"unknown\",\"compiler\":{\"name\":\"unknown\",\"version\":\"unknown\",\"binary_sha256\":\"unknown\"},\"validator\":{\"name\":\"unknown\",\"version\":\"unknown\",\"binary_sha256\":\"unknown\"},\"modules\":[],\"manifest_hash\":\"unknown\"}",
    )
}

fn shader_interface_hash(module: &VulkanShaderModuleManifest) -> String {
    #[derive(Serialize)]
    struct ShaderInterfaceHashJson<'a> {
        stage: VulkanShaderStage,
        entry_point: &'a str,
        descriptor_sets: u32,
        push_constant_bytes: u32,
    }

    let normalized = serialize_json_or_fallback(
        &ShaderInterfaceHashJson {
            stage: module.stage,
            entry_point: module.entry_point,
            descriptor_sets: module.descriptor_sets,
            push_constant_bytes: module.push_constant_bytes,
        },
        "{\"stage\":\"vertex\",\"entry_point\":\"main\",\"descriptor_sets\":0,\"push_constant_bytes\":0}",
    );
    sha256_hex(&sha256(normalized.as_bytes()))
}

fn validate_spirv_container(
    module: &VulkanShaderModuleManifest,
) -> Result<(), VulkanShaderManifestError> {
    if module.words.len() < 5 {
        return Err(VulkanShaderManifestError::TooShort { name: module.name });
    }
    if module.words[0] != SPIRV_MAGIC {
        return Err(VulkanShaderManifestError::InvalidMagic {
            name: module.name,
            found: module.words[0],
        });
    }
    if module.words[1] < SPIRV_VERSION_1_0 {
        return Err(VulkanShaderManifestError::UnsupportedVersion {
            name: module.name,
            found: module.words[1],
        });
    }
    if module.words[3] == 0 {
        return Err(VulkanShaderManifestError::InvalidBound { name: module.name });
    }
    Ok(())
}

fn spirv_words_to_bytes(words: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(words.len() * 4);
    for word in words {
        out.extend_from_slice(&word.to_le_bytes());
    }
    out
}

fn render_shader_manifest_without_hash_json(modules: &[VulkanShaderModuleReport]) -> String {
    #[derive(Serialize)]
    struct ShaderManifestWithoutHashJson<'a> {
        schema: u32,
        target_env: &'a str,
        compiler: VulkanShaderToolManifest,
        validator: VulkanShaderToolManifest,
        modules: &'a [VulkanShaderModuleReport],
    }

    let json = serialize_json_or_fallback(
        &ShaderManifestWithoutHashJson {
            schema: SHADER_MANIFEST_SCHEMA,
            target_env: SHADER_TARGET_ENV,
            compiler: VulkanShaderToolManifest {
                name: SHADER_COMPILER_NAME,
                version: SHADER_COMPILER_VERSION,
                binary_sha256: SHADER_COMPILER_BINARY_SHA256,
            },
            validator: VulkanShaderToolManifest {
                name: SPIRV_VALIDATOR_NAME,
                version: SPIRV_VALIDATOR_VERSION,
                binary_sha256: SPIRV_VALIDATOR_BINARY_SHA256,
            },
            modules,
        },
        "{\"schema\":0,\"target_env\":\"unknown\",\"compiler\":{\"name\":\"unknown\",\"version\":\"unknown\",\"binary_sha256\":\"unknown\"},\"validator\":{\"name\":\"unknown\",\"version\":\"unknown\",\"binary_sha256\":\"unknown\"},\"modules\":[]}",
    );
    match json.strip_suffix('}') {
        Some(stripped) => stripped.to_string(),
        None => json,
    }
}
