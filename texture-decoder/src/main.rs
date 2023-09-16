use std::io::Read;

use byteorder::ReadBytesExt;
use image::Rgba;

fn decode_texture(file_path: &str, output_path: &str) -> Result<(), std::io::Error> {
    // Читаем файл
    let mut file = std::fs::File::open(file_path)?;
    let mut buffer: Vec<u8> = Vec::new();
    file.read_to_end(&mut buffer)?;

    // Декодируем метаданные
    let mut cursor = std::io::Cursor::new(&buffer[4..]);
    let img_width = cursor.read_u32::<byteorder::LittleEndian>()?;
    let img_height = cursor.read_u32::<byteorder::LittleEndian>()?;

    // Пропустить оставшиеся байты метаданных
    cursor.set_position(20);

    // Извлекаем данные изображения
    let image_data = buffer[cursor.position() as usize..].to_vec();
    let img =
        image::ImageBuffer::<Rgba<u8>, _>::from_raw(img_width, img_height, image_data.to_vec())
            .expect("Failed to decode image");

    // Сохраняем изображение
    img.save(output_path).unwrap();

    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let input = &args[1];
    let output = &args[2];

    if let Err(err) = decode_texture(&input, &output) {
        eprintln!("Error: {}", err)
    }
}
