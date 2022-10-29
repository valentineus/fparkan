use std::io::{Read, Seek};

use byteorder::ByteOrder;

use crate::converter;
use crate::error::ReaderError;

/// String value "NRes" in decimal notation
const HEADER_VALUE_1: i32 = 1936020046;
const HEADER_VALUE_2: i32 = 256;
const MINIMUM_FILE_SIZE: i64 = 16;
const ELEMENT_SIZE: i32 = 64;

#[derive(Debug)]
pub struct FileListElement {
    /// Unknown parameter
    _unknown0: i32,
    /// Unknown parameter
    _unknown1: i32,
    /// Unknown parameter
    _unknown2: i32,
    /// File extension
    pub extension: String,
    /// Identifier or sequence number
    pub index: i32,
    /// File name
    pub name: String,
    /// Bytes position
    pub position: i32,
    /// File size (in bytes)
    pub size: i32,
}

#[derive(Debug)]
struct FileHeader {
    /// File size
    size: i32,
    /// Number of files
    total: i32,
    /// String value "NRes" in decimal notation
    type1: i32,
    /// Constant value
    type2: i32,
}

pub fn list(file: &std::fs::File) -> Result<Vec<FileListElement>, ReaderError> {
    let list: Vec<FileListElement> = Vec::new();

    //region Getting the file size
    let file_size = get_file_size(&file)?;

    if file_size < MINIMUM_FILE_SIZE {
        return Err(ReaderError::SmallFile {
            expected: 0,
            received: file_size as i32,
        });
    }
    //endregion

    //region Getting the file header
    let header = get_file_header(&file)?;

    if header.type1 != HEADER_VALUE_1 || header.type2 != HEADER_VALUE_2 {
        return Err(ReaderError::IncorrectHeader);
    }

    if i64::from(header.size) != file_size {
        return Err(ReaderError::IncorrectSizeFile {
            expected: file_size as i32,
            received: header.size,
        });
    }

    if header.total <= 0 {
        return Ok(list);
    }
    //endregion

    let list = get_list_of_files(&file, &header)?;
    Ok(list)
}

fn get_list_of_files(
    file: &std::fs::File,
    header: &FileHeader,
) -> Result<Vec<FileListElement>, ReaderError> {
    let (start_position, list_size) = get_position_list_of_files(header)?;
    let mut reader = std::io::BufReader::new(file);
    let mut buffer = vec![0u8; list_size];
    let mut list: Vec<FileListElement> = Vec::new();

    match reader.seek(std::io::SeekFrom::Start(start_position)) {
        Err(error) => return Err(ReaderError::ReadFile(error)),
        _ => {}
    };

    match reader.read_exact(&mut buffer) {
        Err(error) => return Err(ReaderError::ReadFile(error)),
        _ => {}
    }

    let buffer_size = converter::usize_to_i32(buffer.len())?;

    if buffer_size % ELEMENT_SIZE != 0 {
        return Err(ReaderError::IncorrectSizeList {
            expected: 0,
            received: buffer_size,
        });
    }

    for i in 0..(buffer_size / ELEMENT_SIZE) {
        let (from, to) = get_position_element(i)?;
        let chunk: &[u8] = &buffer[from..to];

        let element = get_list_item(chunk)?;
        list.push(element);
    }

    Ok(list)
}

fn get_position_element(index: i32) -> Result<(usize, usize), ReaderError> {
    let from = converter::i32_to_usize(index * ELEMENT_SIZE)?;
    let to = converter::i32_to_usize((index * ELEMENT_SIZE) + ELEMENT_SIZE)?;
    Ok((from, to))
}

fn get_position_list_of_files(header: &FileHeader) -> Result<(u64, usize), ReaderError> {
    let position = converter::i32_to_u64(header.size - (header.total * ELEMENT_SIZE))?;
    let size = converter::i32_to_usize(header.total * ELEMENT_SIZE)?;
    Ok((position, size))
}

fn get_list_item(buffer: &[u8]) -> Result<FileListElement, ReaderError> {
    let index = byteorder::LittleEndian::read_i32(&buffer[60..64]);
    let position = byteorder::LittleEndian::read_i32(&buffer[56..60]);
    let size = byteorder::LittleEndian::read_i32(&buffer[12..16]);
    let unknown0 = byteorder::LittleEndian::read_i32(&buffer[4..8]);
    let unknown1 = byteorder::LittleEndian::read_i32(&buffer[8..12]);
    let unknown2 = byteorder::LittleEndian::read_i32(&buffer[16..20]);

    let extension = String::from_utf8_lossy(&buffer[0..4]);
    let extension = extension.trim_matches(char::from(0)).to_string();

    let name = String::from_utf8_lossy(&buffer[20..56]);
    let name = name.trim_matches(char::from(0)).to_string();

    Ok(FileListElement {
        _unknown0: unknown0,
        _unknown1: unknown1,
        _unknown2: unknown2,
        extension,
        index,
        name,
        position,
        size,
    })
}

fn get_file_header(file: &std::fs::File) -> Result<FileHeader, ReaderError> {
    let mut reader = std::io::BufReader::new(file);
    // TODO: Add to constants
    let mut buffer = vec![0u8; 16];

    match reader.read_exact(&mut buffer) {
        Err(error) => return Err(ReaderError::ReadFile(error)),
        _ => {}
    };

    let header = FileHeader {
        size: byteorder::LittleEndian::read_i32(&buffer[12..16]),
        total: byteorder::LittleEndian::read_i32(&buffer[8..12]),
        type1: byteorder::LittleEndian::read_i32(&buffer[0..4]),
        type2: byteorder::LittleEndian::read_i32(&buffer[4..8]),
    };

    buffer.clear();
    Ok(header)
}

fn get_file_size(file: &std::fs::File) -> Result<i64, ReaderError> {
    let metadata = match file.metadata() {
        Err(error) => return Err(ReaderError::ReadFile(error)),
        Ok(value) => value,
    };

    let result = converter::u64_to_i64(metadata.len())?;
    Ok(result)
}
