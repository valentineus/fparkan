extern crate miette;
extern crate thiserror;

use miette::Diagnostic;
use thiserror::Error;

#[derive(Error, Diagnostic, Debug)]
pub enum ConverterError {
    #[error("error converting an value")]
    #[diagnostic(code(libnres::infallible))]
    Infallible(#[from] std::convert::Infallible),

    #[error("error converting an value")]
    #[diagnostic(code(libnres::try_from_int_error))]
    TryFromIntError(#[from] std::num::TryFromIntError),
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
    IncorrectSizeFile { expected: u32, received: u32 },

    #[error(
        "incorrect size of the file list (not a multiple of {expected:?}, received {received:?})"
    )]
    #[diagnostic(code(libnres::list_size_error))]
    IncorrectSizeList { expected: u32, received: u32 },

    #[error("resource file reading error")]
    #[diagnostic(code(libnres::io_error))]
    ReadFile(#[from] std::io::Error),

    #[error("file is too small (must be at least {expected:?} bytes, received {received:?} byte)")]
    #[diagnostic(code(libnres::file_size_error))]
    SmallFile { expected: u32, received: u32 },
}
