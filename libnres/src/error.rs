extern crate miette;
extern crate thiserror;

use miette::Diagnostic;
use thiserror::Error;

#[derive(Error, Diagnostic, Debug)]
pub enum ConverterError {
    #[error("error converting an value")]
    #[diagnostic(code(libnres::convert_error))]
    ConvertValue(#[from] std::num::TryFromIntError),
}

#[derive(Error, Diagnostic, Debug)]
pub enum ReaderError {
    #[error(transparent)]
    #[diagnostic(code(libnres::convert_error))]
    ConvertValue(#[from] ConverterError),

    #[error("incorrect header format")]
    #[diagnostic(code(libnres::list_type_error))]
    IncorrectHeader,

    #[error("incorrect file size (expected {expected:?} bytes, received {received:?} bytes)")]
    #[diagnostic(code(libnres::file_size_error))]
    IncorrectSizeFile { expected: i32, received: i32 },

    #[error(
        "incorrect size of the file list (not a multiple of {expected:?}, received {received:?})"
    )]
    #[diagnostic(code(libnres::list_size_error))]
    IncorrectSizeList { expected: i32, received: i32 },

    #[error("resource file reading error")]
    #[diagnostic(code(libnres::io_error))]
    ReadFile(#[from] std::io::Error),

    #[error("file is too small (must be at least {expected:?} bytes, received {received:?} byte)")]
    #[diagnostic(code(libnres::file_size_error))]
    SmallFile { expected: i32, received: i32 },
}
