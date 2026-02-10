use core::fmt;

#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    Io(std::io::Error),

    InvalidMagic {
        got: [u8; 2],
    },
    UnsupportedVersion {
        got: u8,
    },
    InvalidEntryCount {
        got: i16,
    },
    TooManyEntries {
        got: usize,
    },

    EntryTableOutOfBounds {
        table_offset: u64,
        table_len: u64,
        file_len: u64,
    },
    EntryTableDecryptFailed,
    CorruptEntryTable(&'static str),

    EntryIdOutOfRange {
        id: u32,
        entry_count: u32,
    },
    EntryDataOutOfBounds {
        id: u32,
        offset: u64,
        size: u32,
        file_len: u64,
    },

    AoTrailerInvalid,
    MediaOverlayOutOfBounds {
        overlay: u32,
        file_len: u64,
    },

    UnsupportedMethod {
        raw: u32,
    },
    PackedSizePastEof {
        id: u32,
        offset: u64,
        packed_size: u32,
        file_len: u64,
    },
    DeflateEofPlusOneQuirkRejected {
        id: u32,
    },

    DecompressionFailed(&'static str),
    OutputSizeMismatch {
        expected: u32,
        got: u32,
    },

    IntegerOverflow,
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
            Error::InvalidMagic { got } => write!(f, "invalid RsLi magic: {got:02X?}"),
            Error::UnsupportedVersion { got } => write!(f, "unsupported RsLi version: {got:#x}"),
            Error::InvalidEntryCount { got } => write!(f, "invalid entry_count: {got}"),
            Error::TooManyEntries { got } => write!(f, "too many entries: {got} exceeds u32::MAX"),
            Error::EntryTableOutOfBounds {
                table_offset,
                table_len,
                file_len,
            } => write!(
                f,
                "entry table out of bounds: off={table_offset}, len={table_len}, file={file_len}"
            ),
            Error::EntryTableDecryptFailed => write!(f, "failed to decrypt entry table"),
            Error::CorruptEntryTable(s) => write!(f, "corrupt entry table: {s}"),
            Error::EntryIdOutOfRange { id, entry_count } => {
                write!(f, "entry id out of range: id={id}, count={entry_count}")
            }
            Error::EntryDataOutOfBounds {
                id,
                offset,
                size,
                file_len,
            } => write!(
                f,
                "entry data out of bounds: id={id}, off={offset}, size={size}, file={file_len}"
            ),
            Error::AoTrailerInvalid => write!(f, "invalid AO trailer"),
            Error::MediaOverlayOutOfBounds { overlay, file_len } => {
                write!(
                    f,
                    "media overlay out of bounds: overlay={overlay}, file={file_len}"
                )
            }
            Error::UnsupportedMethod { raw } => write!(f, "unsupported packing method: {raw:#x}"),
            Error::PackedSizePastEof {
                id,
                offset,
                packed_size,
                file_len,
            } => write!(
                f,
                "packed range past EOF: id={id}, off={offset}, size={packed_size}, file={file_len}"
            ),
            Error::DeflateEofPlusOneQuirkRejected { id } => {
                write!(f, "deflate EOF+1 quirk rejected for entry {id}")
            }
            Error::DecompressionFailed(s) => write!(f, "decompression failed: {s}"),
            Error::OutputSizeMismatch { expected, got } => {
                write!(f, "output size mismatch: expected={expected}, got={got}")
            }
            Error::IntegerOverflow => write!(f, "integer overflow"),
        }
    }
}

impl std::error::Error for Error {}
