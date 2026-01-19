use crate::error::ConverterError;

/// Method for converting u32 to u64.
pub fn u32_to_u64(value: u32) -> Result<u64, ConverterError> {
    Ok(u64::from(value))
}

/// Method for converting u32 to usize.
pub fn u32_to_usize(value: u32) -> Result<usize, ConverterError> {
    match usize::try_from(value) {
        Err(error) => Err(ConverterError::TryFromIntError(error)),
        Ok(result) => Ok(result),
    }
}

/// Method for converting u64 to u32.
pub fn u64_to_u32(value: u64) -> Result<u32, ConverterError> {
    match u32::try_from(value) {
        Err(error) => Err(ConverterError::TryFromIntError(error)),
        Ok(result) => Ok(result),
    }
}

/// Method for converting usize to u32.
pub fn usize_to_u32(value: usize) -> Result<u32, ConverterError> {
    match u32::try_from(value) {
        Err(error) => Err(ConverterError::TryFromIntError(error)),
        Ok(result) => Ok(result),
    }
}
