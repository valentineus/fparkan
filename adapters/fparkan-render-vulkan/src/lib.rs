#![deny(unsafe_code)]
//! Vulkan adapter public surface.

mod ffi;
mod planning_backend;

pub use ffi::*;
pub use planning_backend::*;
