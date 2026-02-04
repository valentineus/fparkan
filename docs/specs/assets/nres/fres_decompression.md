# FRES Декомпрессия

## Обзор

FRES — это гибридный алгоритм сжатия, использующий комбинацию RLE (Run-Length Encoding) и LZ77-подобного сжатия со скользящим окном. Существуют два режима работы: **adaptive Huffman** (флаг `a1 < 0`) и **простой битовый** (флаг `a1 >= 0`).

```c
char __stdcall sub_1001B22E(
    char a1,        // Флаг режима (< 0 = Huffman, >= 0 = простой)
    int a2,         // Ключ/seed (не используется напрямую)
    _BYTE *a3,      // Выходной буфер
    int a4,         // Размер выходного буфера
    _BYTE *a5,      // Входные сжатые данные
    int a6          // Размер входных данных
)
```

## Структуры данных

### Глобальные переменные

```c
byte_1003A910[4096]     // Циклический буфер скользящего окна (12 бит адрес)
dword_1003E09C          // Указатель на конец выходного буфера
dword_1003E0A0          // Текущая позиция в циклическом буфере
dword_1003E098          // Состояние Huffman дерева
dword_1003E0A4          // Длина повтора для LZ77
```

### Константы

```c
#define WINDOW_SIZE     4096        // Размер скользящего окна (0x1000)
#define WINDOW_MASK     0x0FFF      // Маска для циклического буфера
#define INIT_POS_NEG    4078        // Начальная позиция для Huffman режима
#define INIT_POS_POS    4036        // Начальная позиция для простого режима
```

## Режим 1: Простой битовый режим (a1 >= 0)

Это более простой режим без Huffman кодирования. Работает следующим образом:

### Алгоритм

```
Инициализация:
    position = 4036
    flags = 0
    flagBits = 0

Цикл декомпрессии:
    Пока есть входные данные и выходной буфер не заполнен:

        1. Прочитать бит флага (LSB-first):
           if (flagBits == 0):
               flags = *input++
               flagBits = 8

           flag_bit = flags & 1
           flags >>= 1
           flagBits -= 1

        2. Выбор действия по биту:

           a) Если bit == 1:
              // Литерал - копировать один байт
              byte = *input++
              window[position] = byte
              *output++ = byte
              position = (position + 1) & 0xFFF

           b) Если bit == 0:
              // LZ77 копирование (2 байта)
              word = *(uint16*)input
              input += 2

              b0 = word & 0xFF
              b1 = (word >> 8) & 0xFF

              offset = b0 | ((b1 & 0xF0) << 4)   // 12 бит offset
              length = (b1 & 0x0F) + 3           // 4 бита длины + 3

              src_pos = offset
              Повторить length раз:
                  byte = window[src_pos]
                  window[position] = byte
                  *output++ = byte
                  src_pos = (src_pos + 1) & 0xFFF
                  position = (position + 1) & 0xFFF
```

### Формат сжатых данных (простой режим)

```
Битовый поток:

Битовый поток:

[FLAG_BIT] [DATA]

Где:
    FLAG_BIT = 1: → Литерал (1 байт следует)
    FLAG_BIT = 0: → LZ77 копирование (2 байта следуют)

Формат LZ77 копирования (2 байта, little-endian):
    Байт 0: offset_low (биты 0-7)
    Байт 1: [length:4][offset_high:4]

    offset = byte0 | ((byte1 & 0xF0) << 4)  // 12 бит
    length = (byte1 & 0x0F) + 3             // 4 бита + 3 = 3-18 байт
```

## Режим 2: Adaptive Huffman режим (a1 < 0)

Более сложный режим с динамическим Huffman деревом.

### Инициализация Huffman

```c
Инициализация таблиц:
    1. Создание таблицы быстрого декодирования (dword_1003B94C[256])
    2. Инициализация длин кодов (byte_1003BD4C[256])
    3. Построение начального дерева (627 узлов, T = 2*N_CHAR - 1)
       где N_CHAR = 314 (256 литералов + 58 кодов длины)
```

### Алгоритм декодирования

```
Инициализация:
    position = 4078
    bit_buffer = 0
    bit_count = 8

    Инициализировать окно значением 0x20 (пробел):
        for i in range(2039):
            window[i] = 0x20

Цикл декомпрессии:
    Пока не конец выходного буфера:

        1. Декодировать символ через Huffman дерево:

           tree_index = dword_1003E098  // начальный узел

           Пока tree_index < 627:       // внутренний узел
               bit = прочитать_бит()
               tree_index = tree[tree_index + bit]

           symbol = tree_index - 627     // лист дерева

           Обновить дерево (sub_1001B0AE)

        2. Обработать символ:

           if (symbol < 256):
               // Литерал
               window[position] = symbol
               *output++ = symbol
               position = (position + 1) & 0xFFF

           else:
               // LZSS копирование (LZHUF)
               length = symbol - 253          // 3..60
               match_pos = decode_position()  // префикс + 6 бит

               src_pos = (position - 1 - match_pos) & 0xFFF

               Повторить length раз:
                   byte = window[src_pos]
                   window[position] = byte
                   *output++ = byte
                   src_pos = (src_pos + 1) & 0xFFF
                   position = (position + 1) & 0xFFF
```

### Обновление дерева

Адаптивное Huffman дерево обновляется после каждого декодированного символа:

```
Алгоритм обновления:
    1. Увеличить счетчик частоты символа
    2. Если частота превысила порог:
        Перестроить узлы дерева (swapping)
    3. Если счетчик достиг 0x8000:
        Пересчитать все частоты (разделить на 2)
```

## Псевдокод полной реализации

### Декодер (простой режим)

```python
def fres_decompress_simple(input_data, output_size):
    """
    FRES декомпрессия в простом режиме
    """
    # Инициализация
    window = bytearray(4096)
    position = 4036
    output = bytearray()

    input_pos = 0
    flags = 0
    flag_bits = 0

    while len(output) < output_size and input_pos < len(input_data):
        # Читаем флаг (LSB-first)
        if flag_bits == 0:
            if input_pos >= len(input_data):
                break
            flags = input_data[input_pos]
            input_pos += 1
            flag_bits = 8

        flag = flags & 1
        flags >>= 1
        flag_bits -= 1

        # Обработка по флагу
        if flag:  # 1 = literal
            # Литерал
            if input_pos >= len(input_data):
                break
            byte = input_data[input_pos]
            input_pos += 1

            window[position] = byte
            output.append(byte)
            position = (position + 1) & 0xFFF
        else:  # 0 = backref (2 байта)
            if input_pos + 1 >= len(input_data):
                break

            b0 = input_data[input_pos]
            b1 = input_data[input_pos + 1]
            input_pos += 2

            offset = b0 | ((b1 & 0xF0) << 4)
            length = (b1 & 0x0F) + 3

            for _ in range(length):
                if len(output) >= output_size:
                    break

                byte = window[offset]
                window[position] = byte
                output.append(byte)

                offset = (offset + 1) & 0xFFF
                position = (position + 1) & 0xFFF

    return bytes(output[:output_size])
```

### Вспомогательные функции

```python
class BitReader:
    """Класс для побитового чтения"""

    def __init__(self, data):
        self.data = data
        self.pos = 0
        self.bit_buffer = 0
        self.bits_available = 0

    def read_bit(self):
        """Прочитать один бит"""
        if self.bits_available == 0:
            if self.pos >= len(self.data):
                return 0
            self.bit_buffer = self.data[self.pos]
            self.pos += 1
            self.bits_available = 8

        bit = self.bit_buffer & 1
        self.bit_buffer >>= 1
        self.bits_available -= 1
        return bit

    def read_bits(self, count):
        """Прочитать несколько бит"""
        result = 0
        for i in range(count):
            result |= self.read_bit() << i
        return result


def initialize_window():
    """Инициализация окна для Huffman режима"""
    window = bytearray(4096)
    # Заполняем начальным значением
    for i in range(4078):
        window[i] = 0x20  # Пробел
    return window
```

## Ключевые особенности

### 1. Циклический буфер

- Размер: 4096 байт (12 бит адресации)
- Маска: `0xFFF` для циклического доступа
- Начальная позиция зависит от режима

### 2. Dual-режимы

- **Простой**: Быстрее, меньше сжатие, для данных с низкой энтропией
- **Huffman**: Медленнее, лучше сжатие, для данных с высокой энтропией

### 3. LZ77 кодирование

- Offset: 12 бит (0-4095)
- Length: 4 бита + 3 (3-18 байт)
- Максимальное копирование: 18 байт

### 4. Битовые флаги

Используется один флаговый бит (LSB-first) для определения типа данных:

- `1` → literal (1 байт)
- `0` → backref (2 байта)

## Проблемы реализации

### 1. Битовый порядок

Биты читаются справа налево (LSB first), что может вызвать путаницу

### 2. Huffman дерево

Адаптивное дерево требует точного отслеживания частот и правильной перестройки

### 3. Граничные условия

Необходимо тщательно проверять границы буферов

- В простом режиме перед backref нужно гарантировать наличие **2 байт** входных данных

## Примеры данных

### Пример 1: Литералы (простой режим)

```
Входные биты: 00 00 00 ...
Выход: Последовательность литералов

Пример:
    Flags: 0xFF (11111111)
    Data:  0x41 ('A'), 0x42 ('B'), 0x43 ('C'), ...
    Выход: "ABC..."
```

### Пример 2: LZ77 копирование

```
Входные биты: 10 ...
Выход: Копирование из окна

Пример:
    Flags: 0x00 (00000000) - первый бит = 0
    Bytes: b0=0x34, b1=0x12

    Разбор:
        offset = 0x34 | ((0x12 & 0xF0) << 4) = 0x234
        length = (0x12 & 0x0F) + 3 = 5

    Действие: Скопировать 5 байт с позиции offset
```

## Отладка

Для отладки рекомендуется:

```python
def debug_fres_decompress(input_data, output_size):
    """Версия с отладочным выводом"""
    print(f"Input size: {len(input_data)}")
    print(f"Output size: {output_size}")

    # ... реализация с print на каждом шаге

    print(f"Flag: {flag}")
    if is_literal:
        print(f"  Literal: 0x{byte:02X}")
    else:
        print(f"  LZ77: offset={offset}, length={length}")
```

## Заключение

FRES — это эффективный гибридный алгоритм, сочетающий:

- RLE для повторяющихся данных
- LZ77 для ссылок на предыдущие данные
- Опциональный Huffman для символов

**Сложность декомпрессии:** O(n) где n — размер выходных данных

**Размер окна:** 4 КБ — хороший баланс между памятью и степенью сжатия
