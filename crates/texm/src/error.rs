use core::fmt;

#[derive(Debug)]
pub enum Error {
    HeaderTooSmall {
        size: usize,
    },
    InvalidMagic {
        got: u32,
    },
    InvalidDimensions {
        width: u32,
        height: u32,
    },
    InvalidMipCount {
        mip_count: u32,
    },
    UnknownFormat {
        format: u32,
    },
    IntegerOverflow,
    CoreDataOutOfBounds {
        expected_end: usize,
        actual_size: usize,
    },
    MipIndexOutOfRange {
        requested: usize,
        mip_count: usize,
    },
    MipDataOutOfBounds {
        offset: usize,
        size: usize,
        payload_size: usize,
    },
    InvalidPageMagic,
    InvalidPageSize {
        expected: usize,
        actual: usize,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HeaderTooSmall { size } => {
                write!(f, "Texm payload too small for header: {size}")
            }
            Self::InvalidMagic { got } => write!(f, "invalid Texm magic: 0x{got:08X}"),
            Self::InvalidDimensions { width, height } => {
                write!(f, "invalid Texm dimensions: {width}x{height}")
            }
            Self::InvalidMipCount { mip_count } => write!(f, "invalid Texm mip_count={mip_count}"),
            Self::UnknownFormat { format } => write!(f, "unknown Texm format={format}"),
            Self::IntegerOverflow => write!(f, "integer overflow"),
            Self::CoreDataOutOfBounds {
                expected_end,
                actual_size,
            } => write!(
                f,
                "Texm core data out of bounds: expected_end={expected_end}, actual_size={actual_size}"
            ),
            Self::MipIndexOutOfRange {
                requested,
                mip_count,
            } => write!(
                f,
                "Texm mip index out of range: requested={requested}, mip_count={mip_count}"
            ),
            Self::MipDataOutOfBounds {
                offset,
                size,
                payload_size,
            } => write!(
                f,
                "Texm mip data out of bounds: offset={offset}, size={size}, payload_size={payload_size}"
            ),
            Self::InvalidPageMagic => write!(f, "Texm tail exists but Page magic is missing"),
            Self::InvalidPageSize { expected, actual } => {
                write!(f, "invalid Page chunk size: expected={expected}, actual={actual}")
            }
        }
    }
}

impl std::error::Error for Error {}
