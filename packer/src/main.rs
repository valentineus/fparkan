use std::env;
use std::{
    fs::{self, File},
    io::{BufReader, Read},
};

use byteorder::{ByteOrder, LittleEndian};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct ImportListElement {
    pub extension: String,
    pub index: u32,
    pub name: String,
    pub unknown0: u32,
    pub unknown1: u32,
    pub unknown2: u32,
}

#[derive(Debug)]
pub struct ListElement {
    pub extension: String,
    pub index: u32,
    pub name: String,
    pub position: u32,
    pub size: u32,
    pub unknown0: u32,
    pub unknown1: u32,
    pub unknown2: u32,
}

fn main() {
    let args: Vec<String> = env::args().collect();

    let input = &args[1];
    let output = &args[2];

    pack(String::from(input), String::from(output));
}

fn pack(input: String, output: String) {
    // Загружаем индекс-файл
    let index_file = format!("{}/{}", input, "index.json");
    let data = fs::read_to_string(index_file).unwrap();
    let list: Vec<ImportListElement> = serde_json::from_str(&data).unwrap();

    // Общий буфер хранения файлов
    let mut content_buffer: Vec<u8> = Vec::new();
    let mut list_buffer: Vec<u8> = Vec::new();

    // Общее количество файлов
    let total_files: u32 = list.len() as u32;

    for (index, item) in list.iter().enumerate() {
        // Открываем дескриптор файла
        let path = format!("{}/{}.{}", input, item.name, item.index);
        let file = File::open(path).unwrap();
        let metadata = file.metadata().unwrap();

        // Считываем файл в буфер
        let mut reader = BufReader::new(file);
        let mut file_buffer: Vec<u8> = Vec::new();
        reader.read_to_end(&mut file_buffer).unwrap();

        // Выравнивание буфера
        if index != 0 {
            while content_buffer.len() % 8 != 0 {
                content_buffer.push(0);
            }
        }

        // Получение позиции файла
        let position = content_buffer.len() + 16;

        // Записываем файл в буфер
        content_buffer.extend(file_buffer);

        // Формируем элемент
        let element = ListElement {
            extension: item.extension.to_string(),
            index: item.index,
            name: item.name.to_string(),
            position: position as u32,
            size: metadata.len() as u32,
            unknown0: item.unknown0,
            unknown1: item.unknown1,
            unknown2: item.unknown2,
        };

        // Создаем буфер из элемента
        let mut element_buffer: Vec<u8> = Vec::new();

        // Пишем тип файла
        let mut extension_buffer: [u8; 4] = [0; 4];
        let mut file_extension_buffer = element.extension.into_bytes();
        file_extension_buffer.resize(4, 0);
        extension_buffer.copy_from_slice(&file_extension_buffer);
        element_buffer.extend(extension_buffer);

        // Пишем неизвестное значение #1
        let mut unknown0_buffer: [u8; 4] = [0; 4];
        LittleEndian::write_u32(&mut unknown0_buffer, element.unknown0);
        element_buffer.extend(unknown0_buffer);

        // Пишем неизвестное значение #2
        let mut unknown1_buffer: [u8; 4] = [0; 4];
        LittleEndian::write_u32(&mut unknown1_buffer, element.unknown1);
        element_buffer.extend(unknown1_buffer);

        // Пишем размер файла
        let mut file_size_buffer: [u8; 4] = [0; 4];
        LittleEndian::write_u32(&mut file_size_buffer, element.size);
        element_buffer.extend(file_size_buffer);

        // Пишем неизвестное значение #3
        let mut unknown2_buffer: [u8; 4] = [0; 4];
        LittleEndian::write_u32(&mut unknown2_buffer, element.unknown2);
        element_buffer.extend(unknown2_buffer);

        // Пишем название файла
        let mut name_buffer: [u8; 36] = [0; 36];
        let mut file_name_buffer = element.name.into_bytes();
        file_name_buffer.resize(36, 0);
        name_buffer.copy_from_slice(&file_name_buffer);
        element_buffer.extend(name_buffer);

        // Пишем позицию файла
        let mut position_buffer: [u8; 4] = [0; 4];
        LittleEndian::write_u32(&mut position_buffer, element.position);
        element_buffer.extend(position_buffer);

        // Пишем индекс файла
        let mut index_buffer: [u8; 4] = [0; 4];
        LittleEndian::write_u32(&mut index_buffer, element.index);
        element_buffer.extend(index_buffer);

        // Добавляем итоговый буфер в буфер элементов списка
        list_buffer.extend(element_buffer);
    }

    // Выравнивание буфера
    while content_buffer.len() % 8 != 0 {
        content_buffer.push(0);
    }

    let mut header_buffer: Vec<u8> = Vec::new();

    // Пишем первый тип файла
    let mut header_type_1 = [0; 4];
    LittleEndian::write_u32(&mut header_type_1, 1936020046_u32);
    header_buffer.extend(header_type_1);

    // Пишем второй тип файла
    let mut header_type_2 = [0; 4];
    LittleEndian::write_u32(&mut header_type_2, 256_u32);
    header_buffer.extend(header_type_2);

    // Пишем количество файлов
    let mut header_total_files = [0; 4];
    LittleEndian::write_u32(&mut header_total_files, total_files);
    header_buffer.extend(header_total_files);

    // Пишем общий размер файла
    let mut header_total_size = [0; 4];
    let total_size: u32 = ((content_buffer.len() + 16) as u32) + (total_files * 64);
    LittleEndian::write_u32(&mut header_total_size, total_size);
    header_buffer.extend(header_total_size);

    let mut result_buffer: Vec<u8> = Vec::new();
    result_buffer.extend(header_buffer);
    result_buffer.extend(content_buffer);
    result_buffer.extend(list_buffer);

    fs::write(output, result_buffer).unwrap();
}
