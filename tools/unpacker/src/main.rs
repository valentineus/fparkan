use std::env;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};

use byteorder::{ByteOrder, LittleEndian};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct FileHeader {
    pub size: u32,
    pub total: u32,
    pub type1: u32,
    pub type2: u32,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ListElement {
    pub extension: String,
    pub index: u32,
    pub name: String,
    #[serde(skip_serializing)]
    pub position: u32,
    #[serde(skip_serializing)]
    pub size: u32,
    pub unknown0: u32,
    pub unknown1: u32,
    pub unknown2: u32,
}

fn main() {
    let args: Vec<String> = env::args().collect();

    let input = &args[1];
    let output = &args[2];

    unpack(String::from(input), String::from(output));
}

fn unpack(input: String, output: String) {
    let file = File::open(input).unwrap();
    let metadata = file.metadata().unwrap();

    let mut reader = BufReader::new(file);
    let mut list: Vec<ListElement> = Vec::new();

    // Считываем заголовок файла
    let mut header_buffer = [0u8; 16];
    reader.seek(SeekFrom::Start(0)).unwrap();
    reader.read_exact(&mut header_buffer).unwrap();

    let file_header = FileHeader {
        size: LittleEndian::read_u32(&header_buffer[12..16]),
        total: LittleEndian::read_u32(&header_buffer[8..12]),
        type1: LittleEndian::read_u32(&header_buffer[0..4]),
        type2: LittleEndian::read_u32(&header_buffer[4..8]),
    };

    if file_header.type1 != 1936020046 || file_header.type2 != 256 {
        panic!("this isn't NRes file");
    }

    if metadata.len() != file_header.size as u64 {
        panic!("incorrect size")
    }

    // Считываем список файлов
    let list_files_start_position = file_header.size - (file_header.total * 64);
    let list_files_size = file_header.total * 64;

    let mut list_buffer = vec![0u8; list_files_size as usize];
    reader
        .seek(SeekFrom::Start(list_files_start_position as u64))
        .unwrap();
    reader.read_exact(&mut list_buffer).unwrap();

    if !list_buffer.len().is_multiple_of(64) {
        panic!("invalid files list")
    }

    for i in 0..(list_buffer.len() / 64) {
        let from = i * 64;
        let to = (i * 64) + 64;
        let chunk: &[u8] = &list_buffer[from..to];

        let element_list = ListElement {
            extension: String::from_utf8_lossy(&chunk[0..4])
                .trim_matches(char::from(0))
                .to_string(),
            index: LittleEndian::read_u32(&chunk[60..64]),
            name: String::from_utf8_lossy(&chunk[20..56])
                .trim_matches(char::from(0))
                .to_string(),
            position: LittleEndian::read_u32(&chunk[56..60]),
            size: LittleEndian::read_u32(&chunk[12..16]),
            unknown0: LittleEndian::read_u32(&chunk[4..8]),
            unknown1: LittleEndian::read_u32(&chunk[8..12]),
            unknown2: LittleEndian::read_u32(&chunk[16..20]),
        };

        list.push(element_list)
    }

    // Распаковываем файлы в директорию
    for element in &list {
        let path = format!("{}/{}.{}", output, element.name, element.index);
        let mut file = File::create(path).unwrap();

        let mut file_buffer = vec![0u8; element.size as usize];
        reader
            .seek(SeekFrom::Start(element.position as u64))
            .unwrap();
        reader.read_exact(&mut file_buffer).unwrap();

        file.write_all(&file_buffer).unwrap();
        file_buffer.clear();
    }

    // Выгрузка списка файлов в JSON
    let path = format!("{}/{}", output, "index.json");
    let file = File::create(path).unwrap();
    let mut writer = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, &list).unwrap();
    writer.flush().unwrap();
}
