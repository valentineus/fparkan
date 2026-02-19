use core::fmt;

#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    Nres(nres::error::Error),
    MissingResource {
        kind: u32,
        label: &'static str,
    },
    InvalidResourceSize {
        label: &'static str,
        size: usize,
        stride: usize,
    },
    InvalidRes2Size {
        size: usize,
    },
    UnsupportedNodeStride {
        stride: usize,
    },
    IndexOutOfBounds {
        label: &'static str,
        index: usize,
        limit: usize,
    },
    IntegerOverflow,
}

impl From<nres::error::Error> for Error {
    fn from(value: nres::error::Error) -> Self {
        Self::Nres(value)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Nres(err) => write!(f, "{err}"),
            Self::MissingResource { kind, label } => {
                write!(f, "missing required resource type={kind} ({label})")
            }
            Self::InvalidResourceSize {
                label,
                size,
                stride,
            } => {
                write!(
                    f,
                    "invalid {label} size={size}, expected multiple of stride={stride}"
                )
            }
            Self::InvalidRes2Size { size } => {
                write!(f, "invalid Res2 size={size}, expected >= 140")
            }
            Self::UnsupportedNodeStride { stride } => {
                write!(
                    f,
                    "unsupported Res1 node stride={stride}, expected 38 or 24"
                )
            }
            Self::IndexOutOfBounds {
                label,
                index,
                limit,
            } => write!(
                f,
                "{label} index out of bounds: index={index}, limit={limit}"
            ),
            Self::IntegerOverflow => write!(f, "integer overflow"),
        }
    }
}

impl std::error::Error for Error {}
