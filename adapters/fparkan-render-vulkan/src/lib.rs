#![deny(unsafe_code)]
//! Vulkan adapter public surface.

mod ffi;
mod policy;
mod shader;

pub use ffi::*;
pub use policy::*;
pub use shader::VulkanShaderError;
