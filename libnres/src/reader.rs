use std::io::{Read, Seek};

use byteorder::ByteOrder;

use crate::error::ReaderError;
use crate::{converter, FILE_TYPE_1, FILE_TYPE_2, LIST_ELEMENT_SIZE, MINIMUM_FILE_SIZE};

#[derive(Debug)]
pub struct ListElement {
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
    /// Position in the file
    pub position: i32,
    /// File size (in bytes)
    pub size: i32,
}

#[derive(Debug)]
pub struct FileHeader {
    /// File size
    size: i32,
    /// Number of files
    total: i32,
    /// First constant value
    type1: i32,
    /// Second constant value
    type2: i32,
}

pub fn get_file(file: &std::fs::File, element: &ListElement) -> Result<Vec<u8>, ReaderError> {
    todo!()
}

/// Get a list of packed files
pub fn get_list(file: &std::fs::File) -> Result<Vec<ListElement>, ReaderError> {
    let mut list: Vec<ListElement> = Vec::new();

    //region Getting the file size
    let file_size = get_file_size(&file)?;

    if file_size < MINIMUM_FILE_SIZE {
        return Err(ReaderError::SmallFile {
            expected: MINIMUM_FILE_SIZE,
            received: file_size,
        });
    }
    //endregion

    //region Getting the file header
    let header = get_file_header(&file)?;

    if header.type1 != FILE_TYPE_1 || header.type2 != FILE_TYPE_2 {
        return Err(ReaderError::IncorrectHeader { received: header });
    }

    if header.size != file_size {
        return Err(ReaderError::IncorrectSizeFile {
            expected: file_size,
            received: header.size,
        });
    }

    if header.total <= 0 {
        return Ok(list);
    }
    //endregion

    //region Extracting the list of elements
    get_file_list(&file, &header, &mut list)?;
    //endregion

    Ok(list)
}

fn get_element_position(index: i32) -> Result<(usize, usize), ReaderError> {
    let from = converter::i32_to_usize(index * LIST_ELEMENT_SIZE)?;
    let to = converter::i32_to_usize((index * LIST_ELEMENT_SIZE) + LIST_ELEMENT_SIZE)?;
    Ok((from, to))
}

fn get_file_header(file: &std::fs::File) -> Result<FileHeader, ReaderError> {
    let mut reader = std::io::BufReader::new(file);
    let mut buffer = vec![0u8; MINIMUM_FILE_SIZE as usize];

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

fn get_file_list(
    file: &std::fs::File,
    header: &FileHeader,
    list: &mut Vec<ListElement>,
) -> Result<(), ReaderError> {
    let (start_position, list_size) = get_list_position(header)?;
    let mut reader = std::io::BufReader::new(file);
    let mut buffer = vec![0u8; list_size];

    match reader.seek(std::io::SeekFrom::Start(start_position)) {
        Err(error) => return Err(ReaderError::ReadFile(error)),
        _ => {}
    };

    match reader.read_exact(&mut buffer) {
        Err(error) => return Err(ReaderError::ReadFile(error)),
        _ => {}
    }

    let buffer_size = converter::usize_to_i32(buffer.len())?;

    if buffer_size % LIST_ELEMENT_SIZE != 0 {
        return Err(ReaderError::IncorrectSizeList {
            expected: LIST_ELEMENT_SIZE,
            received: buffer_size,
        });
    }

    for i in 0..(buffer_size / LIST_ELEMENT_SIZE) {
        let (from, to) = get_element_position(i)?;
        let chunk: &[u8] = &buffer[from..to];

        let element = get_list_element(chunk)?;
        list.push(element);
    }

    buffer.clear();
    Ok(())
}

fn get_file_size(file: &std::fs::File) -> Result<i32, ReaderError> {
    let metadata = match file.metadata() {
        Err(error) => return Err(ReaderError::ReadFile(error)),
        Ok(value) => value,
    };

    let result = converter::u64_to_i32(metadata.len())?;
    Ok(result)
}

fn get_list_element(buffer: &[u8]) -> Result<ListElement, ReaderError> {
    let index = byteorder::LittleEndian::read_i32(&buffer[60..64]);
    let position = byteorder::LittleEndian::read_i32(&buffer[56..60]);
    let size = byteorder::LittleEndian::read_i32(&buffer[12..16]);
    let unknown0 = byteorder::LittleEndian::read_i32(&buffer[4..8]);
    let unknown1 = byteorder::LittleEndian::read_i32(&buffer[8..12]);
    let unknown2 = byteorder::LittleEndian::read_i32(&buffer[16..20]);

    let extension = String::from_utf8_lossy(&buffer[0..4])
        .trim_matches(char::from(0))
        .to_string();

    let name = String::from_utf8_lossy(&buffer[20..56])
        .trim_matches(char::from(0))
        .to_string();

    Ok(ListElement {
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

fn get_list_position(header: &FileHeader) -> Result<(u64, usize), ReaderError> {
    let position = converter::i32_to_u64(header.size - (header.total * LIST_ELEMENT_SIZE))?;
    let size = converter::i32_to_usize(header.total * LIST_ELEMENT_SIZE)?;
    Ok((position, size))
}
