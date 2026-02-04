# Документация по формату NRes

## Обзор

NRes — это формат контейнера ресурсов, используемый в игровом движке Nikita. Файл представляет собой архив, содержащий несколько упакованных файлов с метаданными и поддержкой различных методов сжатия.

## Структура файла NRes

### 1. Заголовок файла (16 байт)

```c
struct NResHeader {
    uint32_t signature;      // +0x00: Сигнатура "NRes" (0x7365526E в little-endian)
    uint32_t version;        // +0x04: Версия формата (0x00000100 = версия 1.0)
    uint32_t fileCount;      // +0x08: Количество файлов в архиве
    uint32_t fileSize;       // +0x0C: Общий размер файла в байтах
};
```

**Детали:**

- `signature`: Константа `0x7365526E` (1936020046 в десятичном виде). Это ASCII строка "nRes" в обратном порядке байт
- `version`: Всегда должна быть `0x00000100` (256 в десятичном виде) для версии 1.0
- `fileCount`: Общее количество файлов в архиве (используется для валидации)
- `fileSize`: Полный размер NRes файла, включая заголовок

### 2. Данные файлов

Сразу после заголовка (с offset 0x10) начинаются данные упакованных файлов. Они хранятся последовательно, один за другим. Точное расположение каждого файла определяется записью в каталоге (см. раздел 3).

**⚠️ ВАЖНО: Выравнивание данных**

Данные каждого файла **выравниваются по границе 8 байт**. После записи данных файла добавляется padding (нулевые байты) до ближайшего кратного 8 адреса.

**Формула выравнивания:**

```
aligned_size = (packed_size + 7) & ~7
padding_bytes = aligned_size - packed_size
```

**Пример:**

- Файл размером 100 байт → padding 4 байта (до 104)
- Файл размером 104 байт → padding 0 байт (уже выровнен)
- Файл размером 105 байт → padding 3 байта (до 108)

Это означает, что:

1. `dataOffset` следующего файла всегда кратен 8
2. Между данными файлов могут быть 0-7 байт нулевого padding
3. При чтении нужно использовать `packedSize`, а не выравнивать вручную

### 3. Каталог файлов (Directory)

Каталог находится в **конце файла**. Его расположение вычисляется по формуле:

```
DirectoryOffset = FileSize - (FileCount * 64)
```

Каждая запись в каталоге имеет **фиксированный размер 64 байта (0x40)**:

```c
struct NResFileEntry {
    char     name[16];           // +0x00: Имя файла (NULL-terminated, uppercase)
    uint32_t crc32;              // +0x10: CRC32 хеш упакованных данных
    uint32_t packMethod;         // +0x14: Флаги метода упаковки и опции
    uint32_t unpackedSize;       // +0x18: Размер файла после распаковки
    uint32_t packedSize;         // +0x1C: Размер упакованных данных
    uint32_t dataOffset;         // +0x20: Смещение данных от начала файла
    uint32_t fastDataPtr;        // +0x24: Указатель для быстрого доступа (в памяти)
    uint32_t xorSize;            // +0x28: Размер данных для XOR-шифрования
    uint32_t sortIndex;          // +0x2C: Индекс для сортировки по имени
    uint32_t reserved[4];        // +0x30: Зарезервировано (обычно нули)
};
```

## Подробное описание полей каталога

### Поле: name (смещение +0x00, 16 байт)

- **Назначение**: Имя файла в архиве
- **Формат**: NULL-terminated строка, максимум 15 символов + NULL
- **Особенности**:
    - Все символы хранятся в **UPPERCASE** (заглавными буквами)
    - При поиске файлов используется регистронезависимое сравнение (`_strcmpi`)
    - Если имя короче 16 байт, остаток заполняется нулями

### Поле: crc32 (смещение +0x10, 4 байта)

- **Назначение**: Контрольная сумма CRC32 упакованных данных
- **Использование**: Проверка целостности данных при чтении

### Поле: packMethod (смещение +0x14, 4 байта)

**Критически важное поле!** Содержит битовые флаги, определяющие метод обработки данных:

```c
// Маски для извлечения метода упаковки
#define PACK_METHOD_MASK    0x1E0  // Биты 5-8 (основной метод)
#define PACK_METHOD_MASK2   0x1C0  // Биты 6-7 (альтернативная маска)

// Методы упаковки (биты 5-8)
#define PACK_NONE           0x000  // Нет упаковки (копирование)
#define PACK_XOR            0x020  // XOR-шифрование
#define PACK_FRES           0x040  // FRES компрессия (устаревшая)
#define PACK_FRES_XOR       0x060  // FRES + XOR (два прохода)
#define PACK_ZLIB           0x080  // Zlib сжатие (устаревшее)
#define PACK_ZLIB_XOR       0x0A0  // Zlib + XOR (два прохода)
#define PACK_HUFFMAN        0x0E0  // Huffman кодирование (основной метод)

// Дополнительные флаги
#define FLAG_ENCRYPTED      0x040  // Файл зашифрован/требует декодирования
```

**Алгоритм определения метода:**

1. Извлечь биты `packMethod & 0x1E0`
2. Проверить конкретные значения:
    - `0x000`: Данные не сжаты, простое копирование
    - `0x020`: XOR-шифрование с двухбайтовым ключом
    - `0x040` или `0x060`: FRES компрессия (может быть + XOR)
    - `0x080` или `0x0A0`: Zlib компрессия (может быть + XOR)
    - `0x0E0`: Huffman кодирование (наиболее распространенный)

### Поле: unpackedSize (смещение +0x18, 4 байта)

- **Назначение**: Размер файла после полной распаковки
- **Использование**:
    - Для выделения памяти под распакованные данные
    - Для проверки корректности распаковки

### Поле: packedSize (смещение +0x1C, 4 байта)

- **Назначение**: Размер сжатых данных в архиве
- **Особенности**:
    - Если `packedSize == 0`, файл пустой или является указателем
    - Для несжатых файлов: `packedSize == unpackedSize`

### Поле: dataOffset (смещение +0x20, 4 байта)

- **Назначение**: Абсолютное смещение данных файла от начала NRes файла
- **Формула вычисления**: `BaseAddress + dataOffset = начало данных`
- **Диапазон**: Обычно от 0x10 (после заголовка) до начала каталога

### Поле: fastDataPtr (смещение +0x24, 4 байта)

- **Назначение**: Указатель на данные в памяти для быстрого доступа
- **Использование**: Только во время выполнения (runtime)
- **В файле**: Обычно равно 0 или содержит относительный offset
- **Особенность**: Используется функцией `rsLoadFast()` для файлов без упаковки

### Поле: xorSize (смещение +0x28, 4 байта)

- **Назначение**: Размер данных для XOR-шифрования при комбинированных методах
- **Использование**:
    - Когда `packMethod & 0x60 == 0x60` (FRES + XOR)
    - Сначала применяется XOR к этому количеству байт, затем FRES к результату
- **Значение**: Может отличаться от `packedSize` при многоэтапной упаковке

### Поле: sortIndex (смещение +0x2C, 4 байта)

- **Назначение**: Индекс для быстрого поиска по отсортированному каталогу
- **Использование**:
    - Каталог сортируется по алфавиту (имени файлов)
    - `sortIndex` хранит оригинальный порядковый номер файла
    - Позволяет использовать бинарный поиск для функции `rsFind()`

### Поле: reserved (смещение +0x30, 16 байт)

- **Назначение**: Зарезервировано для будущих расширений
- **В файле**: Обычно заполнено нулями
- **Может содержать**: Дополнительные метаданные в новых версиях формата

## Алгоритмы упаковки

### 1. Без упаковки (PACK_NONE = 0x000)

```
Простое копирование данных:
    memcpy(destination, source, packedSize);
```

### 2. XOR-шифрование (PACK_XOR = 0x020)

```c
// Ключ берется из поля crc32
uint16_t key = (uint16_t)(crc32 & 0xFFFF);

for (int i = 0; i < packedSize; i++) {
    uint8_t byte = source[i];
    destination[i] = byte ^ (key >> 8) ^ (key << 1);

    // Обновление ключа
    uint8_t newByte = (key >> 8) ^ (key << 1);
    key = (newByte ^ ((key >> 8) >> 1)) | (newByte << 8);
}
```

**Ключевые особенности:**

- Используется 16-битный ключ из младших байт CRC32
- Ключ изменяется после каждого байта по специальному алгоритму
- Операции: XOR с старшим байтом ключа и со сдвинутым значением

### 3. [FRES компрессия](fres_decompression.md) (PACK_FRES = 0x040, 0x060)

Алгоритм FRES — это RLE-подобное сжатие с особой кодировкой повторов:

```
sub_1001B22E() - функция декомпрессии FRES
    - Читает управляющие байты
    - Декодирует литералы и повторы
    - Использует скользящее окно для ссылок
```

### 4. [Huffman кодирование](huffman_decompression.md) (PACK_HUFFMAN = 0x0E0)

Наиболее сложный и эффективный метод:

```c
// Структура декодера
struct HuffmanDecoder {
    uint32_t  bitBuffer[0x4000];   // Буфер для битов
    uint32_t  compressedSize;      // Размер сжатых данных
    uint32_t  outputPosition;      // Текущая позиция в выходном буфере
    uint32_t  inputPosition;       // Позиция в входных данных
    uint8_t*  sourceData;          // Указатель на сжатые данные
    uint8_t*  destData;            // Указатель на выходной буфер
    uint32_t  bitPosition;         // Позиция бита в буфере
    // ... дополнительные поля
};
```

**Процесс декодирования:**

1. Инициализация структуры декодера
2. Чтение битов и построение дерева Huffman
3. Декодирование символов по дереву
4. Запись в выходной буфер

## Высокоуровневая инструкция по реализации

### Этап 1: Открытие файла

```python
def open_nres_file(filepath):
    with open(filepath, 'rb') as f:
        # 1. Читаем заголовок (16 байт)
        header_data = f.read(16)
        signature, version, file_count, file_size = struct.unpack('<4I', header_data)

        # 2. Проверяем сигнатуру
        if signature != 0x7365526E:  # "nRes"
            raise ValueError("Неверная сигнатура файла")

        # 3. Проверяем версию
        if version != 0x100:
            raise ValueError(f"Неподдерживаемая версия: {version}")

        # 4. Вычисляем расположение каталога
        directory_offset = file_size - (file_count * 64)

        # 5. Читаем весь файл в память (или используем memory mapping)
        f.seek(0)
        file_data = f.read()

        return {
            'file_count': file_count,
            'file_size': file_size,
            'directory_offset': directory_offset,
            'data': file_data
        }
```

### Этап 2: Чтение каталога

```python
def read_directory(nres_file):
    data = nres_file['data']
    offset = nres_file['directory_offset']
    file_count = nres_file['file_count']

    entries = []

    for i in range(file_count):
        entry_offset = offset + (i * 64)
        entry_data = data[entry_offset:entry_offset + 64]

        # Парсим 64-байтовую запись
        name = entry_data[0:16].decode('ascii').rstrip('\x00')
        crc32, pack_method, unpacked_size, packed_size, data_offset, \
        fast_ptr, xor_size, sort_index = struct.unpack('<8I', entry_data[16:48])

        entry = {
            'name': name,
            'crc32': crc32,
            'pack_method': pack_method,
            'unpacked_size': unpacked_size,
            'packed_size': packed_size,
            'data_offset': data_offset,
            'fast_data_ptr': fast_ptr,
            'xor_size': xor_size,
            'sort_index': sort_index
        }

        entries.append(entry)

    return entries
```

### Этап 3: Поиск файла по имени

```python
def find_file(entries, filename):
    # Имена в архиве хранятся в UPPERCASE
    search_name = filename.upper()

    # Используем бинарный поиск, так как каталог отсортирован
    # Сортировка по sort_index восстанавливает алфавитный порядок
    sorted_entries = sorted(entries, key=lambda e: e['sort_index'])

    left, right = 0, len(sorted_entries) - 1

    while left <= right:
        mid = (left + right) // 2
        mid_name = sorted_entries[mid]['name']

        if mid_name == search_name:
            return sorted_entries[mid]
        elif mid_name < search_name:
            left = mid + 1
        else:
            right = mid - 1

    return None
```

### Этап 4: Извлечение данных файла

```python
def extract_file(nres_file, entry):
    data = nres_file['data']

    # 1. Получаем упакованные данные
    packed_data = data[entry['data_offset']:
                       entry['data_offset'] + entry['packed_size']]

    # 2. Определяем метод упаковки
    pack_method = entry['pack_method'] & 0x1E0

    # 3. Распаковываем в зависимости от метода
    if pack_method == 0x000:
        # Без упаковки
        return unpack_none(packed_data)

    elif pack_method == 0x020:
        # XOR-шифрование
        return unpack_xor(packed_data, entry['crc32'], entry['unpacked_size'])

    elif pack_method == 0x040 or pack_method == 0x060:
        # FRES компрессия (может быть с XOR)
        if pack_method == 0x060:
            # Сначала XOR
            temp_data = unpack_xor(packed_data, entry['crc32'], entry['xor_size'])
            return unpack_fres(temp_data, entry['unpacked_size'])
        else:
            return unpack_fres(packed_data, entry['unpacked_size'])

    elif pack_method == 0x0E0:
        # Huffman кодирование
        return unpack_huffman(packed_data, entry['unpacked_size'])

    else:
        raise ValueError(f"Неподдерживаемый метод упаковки: 0x{pack_method:X}")
```

### Этап 5: Реализация алгоритмов распаковки

```python
def unpack_none(data):
    """Без упаковки - просто возвращаем данные"""
    return data

def unpack_xor(data, crc32, size):
    """XOR-дешифрование с изменяющимся ключом"""
    result = bytearray(size)
    key = crc32 & 0xFFFF  # Берем младшие 16 бит

    for i in range(min(size, len(data))):
        byte = data[i]

        # XOR операция
        high_byte = (key >> 8) & 0xFF
        shifted = (key << 1) & 0xFFFF
        result[i] = byte ^ high_byte ^ (shifted & 0xFF)

        # Обновление ключа
        new_byte = high_byte ^ (key << 1)
        key = (new_byte ^ (high_byte >> 1)) | ((new_byte & 0xFF) << 8)
        key &= 0xFFFF

    return bytes(result)

def unpack_fres(data, unpacked_size):
    """
    FRES декомпрессия - гибридный RLE+LZ77 алгоритм
    Полная реализация в nres_decompression.py (класс FRESDecoder)
    """
    from nres_decompression import FRESDecoder
    decoder = FRESDecoder()
    return decoder.decompress(data, unpacked_size)

def unpack_huffman(data, unpacked_size):
    """
    Huffman декодирование (DEFLATE-подобный)
    Полная реализация в nres_decompression.py (класс HuffmanDecoder)
    """
    from nres_decompression import HuffmanDecoder
    decoder = HuffmanDecoder()
    return decoder.decompress(data, unpacked_size)
```

### Этап 6: Извлечение всех файлов

```python
def extract_all(nres_filepath, output_dir):
    import os

    # 1. Открываем NRes файл
    nres_file = open_nres_file(nres_filepath)

    # 2. Читаем каталог
    entries = read_directory(nres_file)

    # 3. Создаем выходную директорию
    os.makedirs(output_dir, exist_ok=True)

    # 4. Извлекаем каждый файл
    for entry in entries:
        print(f"Извлечение: {entry['name']}")

        try:
            # Извлекаем данные
            unpacked_data = extract_file(nres_file, entry)

            # Сохраняем в файл
            output_path = os.path.join(output_dir, entry['name'])
            with open(output_path, 'wb') as f:
                f.write(unpacked_data)

            print(f"  ✓ Успешно ({len(unpacked_data)} байт)")

        except Exception as e:
            print(f"  ✗ Ошибка: {e}")
```

## Особенности и важные замечания

### 1. Порядок байт (Endianness)

- **Все многобайтовые значения хранятся в Little-Endian порядке**
- При чтении используйте `struct.unpack('<...')`

### 2. Сортировка каталога

- Каталог файлов **отсортирован по имени файла** (алфавитный порядок)
- Поле `sortIndex` хранит оригинальный индекс до сортировки
- Это позволяет использовать бинарный поиск

### 3. Регистр символов

- Все имена файлов конвертируются в **UPPERCASE** (заглавные буквы)
- При поиске используйте регистронезависимое сравнение

### 4. Memory Mapping

- Оригинальный код использует `MapViewOfFile` для эффективной работы с большими файлами
- Рекомендуется использовать memory-mapped файлы для больших архивов

### 5. Валидация данных

- **Всегда проверяйте сигнатуру** перед обработкой
- **Проверяйте версию** формата
- **Проверяйте CRC32** после распаковки
- **Проверяйте размеры** (unpacked_size должен совпадать с результатом)

### 6. Обработка ошибок

- Файл может быть поврежден
- Метод упаковки может быть неподдерживаемым
- Данные могут быть частично зашифрованы

### 7. Производительность

- Для несжатых файлов (`packMethod & 0x1E0 == 0`) можно использовать прямое чтение
- Поле `fastDataPtr` может содержать кешированный указатель
- Используйте буферизацию при последовательном чтении

### 8. Выравнивание данных

- **Все данные файлов выравниваются по 8 байт**
- После каждого файла может быть 0-7 байт нулевого padding
- `dataOffset` следующего файла всегда кратен 8
- При чтении используйте `packedSize` из записи, не вычисляйте выравнивание
- При создании архива добавляйте padding: `padding = ((size + 7) & ~7) - size`

## Пример использования

```python
# Открыть архив
nres = open_nres_file("resources.nres")

# Прочитать каталог
entries = read_directory(nres)

# Вывести список файлов
for entry in entries:
    print(f"{entry['name']:20s} - {entry['unpacked_size']:8d} байт")

# Найти конкретный файл
entry = find_file(entries, "texture.bmp")
if entry:
    data = extract_file(nres, entry)
    with open("extracted_texture.bmp", "wb") as f:
        f.write(data)

# Извлечь все файлы
extract_all("resources.nres", "./extracted/")
```

## Дополнительные функции

### Проверка формата файла

```python
def is_nres_file(filepath):
    try:
        with open(filepath, 'rb') as f:
            signature = struct.unpack('<I', f.read(4))[0]
            return signature == 0x7365526E
    except:
        return False
```

### Получение информации о файле

```python
def get_file_info(entry):
    pack_names = {
        0x000: "Без сжатия",
        0x020: "XOR",
        0x040: "FRES",
        0x060: "FRES+XOR",
        0x080: "Zlib",
        0x0A0: "Zlib+XOR",
        0x0E0: "Huffman"
    }

    pack_method = entry['pack_method'] & 0x1E0
    pack_name = pack_names.get(pack_method, f"Неизвестный (0x{pack_method:X})")

    ratio = 100.0 * entry['packed_size'] / entry['unpacked_size'] if entry['unpacked_size'] > 0 else 0

    return {
        'name': entry['name'],
        'size': entry['unpacked_size'],
        'packed': entry['packed_size'],
        'compression': pack_name,
        'ratio': f"{ratio:.1f}%",
        'crc32': f"0x{entry['crc32']:08X}"
    }
```

## Заключение

Формат NRes представляет собой эффективный архив с поддержкой множества методов сжатия.
