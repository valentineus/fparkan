extern crate thiserror;

use thiserror::Error;

use crate::reader::FileHeader;

#[derive(Error, Debug)]
pub enum ConverterError {
    #[error("error converting an value")]
    ConvertValue(#[from] std::num::TryFromIntError),
}

#[derive(Error, Debug)]
pub enum ReaderError {
    #[error(transparent)]
    ConvertValue(#[from] ConverterError),

    #[error("incorrect header format (received {received:?})")]
    IncorrectHeader { received: FileHeader },

    #[error("incorrect file size (expected {expected:?} bytes, received {received:?} bytes)")]
    IncorrectSizeFile { expected: i32, received: i32 },

    #[error(
        "incorrect size of the file list (not a multiple of {expected:?}, received {received:?})"
    )]
    IncorrectSizeList { expected: i32, received: i32 },

    #[error("resource file reading error")]
    ReadFile(#[from] std::io::Error),

    #[error("file is too small (must be at least {expected:?} bytes, received {received:?} byte)")]
    SmallFile { expected: i32, received: i32 },
}
