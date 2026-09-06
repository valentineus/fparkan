use crate::ffi::{
    SPIRV_MAGIC, SPIRV_VERSION_1_0, TRIANGLE_FRAGMENT_SHADER_WORDS, TRIANGLE_VERTEX_SHADER_WORDS,
};

/// Error found while checking a checked-in SPIR-V module.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VulkanShaderError {
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

impl std::fmt::Display for VulkanShaderError {
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

impl std::error::Error for VulkanShaderError {}

/// Checks the checked-in triangle shader containers before creating a pipeline.
pub(crate) fn validate_triangle_shaders() -> Result<(), VulkanShaderError> {
    validate_spirv_container("triangle.vert", TRIANGLE_VERTEX_SHADER_WORDS)?;
    validate_spirv_container("triangle.frag", TRIANGLE_FRAGMENT_SHADER_WORDS)?;
    Ok(())
}

pub(crate) fn validate_spirv_container(
    name: &'static str,
    words: &[u32],
) -> Result<(), VulkanShaderError> {
    if words.len() < 5 {
        return Err(VulkanShaderError::TooShort { name });
    }
    if words[0] != SPIRV_MAGIC {
        return Err(VulkanShaderError::InvalidMagic {
            name,
            found: words[0],
        });
    }
    if words[1] < SPIRV_VERSION_1_0 {
        return Err(VulkanShaderError::UnsupportedVersion {
            name,
            found: words[1],
        });
    }
    if words[3] == 0 {
        return Err(VulkanShaderError::InvalidBound { name });
    }
    Ok(())
}
