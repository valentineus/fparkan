use crate::error::ConverterError;

/// Method for converting i32 to u64.
pub fn i32_to_u64(value: i32) -> Result<u64, ConverterError> {
    match u64::try_from(value) {
        Err(error) => return Err(ConverterError::ConvertValue(error)),
        Ok(result) => Ok(result),
    }
}

/// Method for converting i32 to usize.
pub fn i32_to_usize(value: i32) -> Result<usize, ConverterError> {
    match usize::try_from(value) {
        Err(error) => return Err(ConverterError::ConvertValue(error)),
        Ok(result) => Ok(result),
    }
}

/// Method for converting u64 to i64.
pub fn u64_to_i64(value: u64) -> Result<i64, ConverterError> {
    match i64::try_from(value) {
        Err(error) => return Err(ConverterError::ConvertValue(error)),
        Ok(result) => Ok(result),
    }
}

/// Method for converting usize to i32.
pub fn usize_to_i32(value: usize) -> Result<i32, ConverterError> {
    match i32::try_from(value) {
        Err(error) => return Err(ConverterError::ConvertValue(error)),
        Ok(result) => Ok(result),
    }
}
