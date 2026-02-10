use core::fmt;

#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    Io(std::io::Error),

    InvalidMagic {
        got: [u8; 4],
    },
    UnsupportedVersion {
        got: u32,
    },
    TotalSizeMismatch {
        header: u32,
        actual: u64,
    },

    InvalidEntryCount {
        got: i32,
    },
    TooManyEntries {
        got: usize,
    },
    DirectoryOutOfBounds {
        directory_offset: u64,
        directory_len: u64,
        file_len: u64,
    },

    EntryIdOutOfRange {
        id: u32,
        entry_count: u32,
    },
    EntryDataOutOfBounds {
        id: u32,
        offset: u64,
        size: u32,
        directory_offset: u64,
    },
    NameTooLong {
        got: usize,
        max: usize,
    },
    NameContainsNul,
    BadNameEncoding,

    IntegerOverflow,

    RawModeDisallowsOperation(&'static str),
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "I/O error: {e}"),
            Error::InvalidMagic { got } => write!(f, "invalid NRes magic: {got:02X?}"),
            Error::UnsupportedVersion { got } => {
                write!(f, "unsupported NRes version: {got:#x}")
            }
            Error::TotalSizeMismatch { header, actual } => {
                write!(f, "NRes total_size mismatch: header={header}, actual={actual}")
            }
            Error::InvalidEntryCount { got } => write!(f, "invalid entry_count: {got}"),
            Error::TooManyEntries { got } => write!(f, "too many entries: {got} exceeds u32::MAX"),
            Error::DirectoryOutOfBounds {
                directory_offset,
                directory_len,
                file_len,
            } => write!(
                f,
                "directory out of bounds: off={directory_offset}, len={directory_len}, file={file_len}"
            ),
            Error::EntryIdOutOfRange { id, entry_count } => {
                write!(f, "entry id out of range: id={id}, count={entry_count}")
            }
            Error::EntryDataOutOfBounds {
                id,
                offset,
                size,
                directory_offset,
            } => write!(
                f,
                "entry data out of bounds: id={id}, off={offset}, size={size}, dir_off={directory_offset}"
            ),
            Error::NameTooLong { got, max } => write!(f, "name too long: {got} > {max}"),
            Error::NameContainsNul => write!(f, "name contains NUL byte"),
            Error::BadNameEncoding => write!(f, "bad name encoding"),
            Error::IntegerOverflow => write!(f, "integer overflow"),
            Error::RawModeDisallowsOperation(op) => {
                write!(f, "operation not allowed in raw mode: {op}")
            }
        }
    }
}

impl std::error::Error for Error {}
