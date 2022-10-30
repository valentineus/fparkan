/// First constant value of the NRes file ("NRes" characters in numeric)
pub const FILE_TYPE_1: i32 = 1936020046;
/// Second constant value of the NRes file
pub const FILE_TYPE_2: i32 = 256;
/// Size of the element item (in bytes)
pub const LIST_ELEMENT_SIZE: i32 = 64;
/// Minimum allowed file size (in bytes)
pub const MINIMUM_FILE_SIZE: i32 = 16;

mod converter;
mod error;
pub mod reader;
