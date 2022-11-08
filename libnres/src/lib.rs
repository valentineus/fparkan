/// First constant value of the NRes file ("NRes" characters in numeric)
pub const FILE_TYPE_1: u32 = 1936020046;
/// Second constant value of the NRes file
pub const FILE_TYPE_2: u32 = 256;
/// Size of the element item (in bytes)
pub const LIST_ELEMENT_SIZE: u32 = 64;
/// Minimum allowed file size (in bytes)
pub const MINIMUM_FILE_SIZE: u32 = 16;

static DEBUG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

mod converter;
mod error;
pub mod reader;

/// Get debug status value
pub fn get_debug() -> bool {
    DEBUG.load(std::sync::atomic::Ordering::Relaxed)
}

/// Change debug status value
pub fn set_debug(value: bool) {
    DEBUG.store(value, std::sync::atomic::Ordering::Relaxed)
}
