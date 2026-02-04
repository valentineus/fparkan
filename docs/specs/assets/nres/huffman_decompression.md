# Huffman Декомпрессия

## Обзор

Это реализация **RAW-DEFLATE (inflate)**, используемого в [NRes](overview.md). Поток подаётся без zlib-обёртки (нет 2-байтового заголовка и Adler32). Алгоритм поддерживает три режима блоков и использует два Huffman дерева для кодирования литералов/длин и расстояний.

```c
int __thiscall sub_1001AF10(
    unsigned int *this,  // Контекст декодера (HuffmanContext)
    int *a2              // Выходной параметр (результат операции)
)
```

## Структура контекста (HuffmanContext)

```c
struct HuffmanContext {
    uint8_t   window[0x10000];      // 0x00000-0x0FFFF: Внутренний буфер/окно
    uint32_t  compressedSize;       // 0x10000: packedSize
    uint32_t  outputPosition;       // 0x10004: Сколько уже выведено
    uint32_t  windowPos;            // 0x10008: Позиция в 0x8000 окне
    uint32_t  sourcePtr;            // 0x1000C: Указатель на сжатые данные
    uint32_t  destPtr;              // 0x10010: Указатель на выходной буфер
    uint32_t  sourcePos;            // 0x10014: Текущая позиция чтения
    uint32_t  unpackedSize;         // 0x10018: Ожидаемый размер распаковки
    uint32_t  bitBufferValue;       // 0x1001C: Битовый буфер
    uint32_t  bitsAvailable;        // 0x10020: Количество доступных бит
    uint32_t  maxWindowPosSeen;     // 0x10024: Максимум окна (статистика)
    // ...
};

// Смещения в структуре (индексация this[]):
#define CTX_COMPRESSED_SIZE 0x4000  // this[0x4000] == 0x10000
#define CTX_OUTPUT_POS      16385   // this[16385] == 0x10004
#define CTX_WINDOW_POS      16386   // this[16386] == 0x10008
#define CTX_SOURCE_PTR      16387   // this[16387] == 0x1000C
#define CTX_DEST_PTR        16388   // this[16388] == 0x10010
#define CTX_SOURCE_POS      16389   // this[16389] == 0x10014
#define CTX_UNPACKED_SIZE   16390   // this[16390] == 0x10018
#define CTX_BIT_BUFFER      16391   // this[16391] == 0x1001C
#define CTX_BITS_COUNT      16392   // this[16392] == 0x10020
#define CTX_MAX_WINDOW_POS  16393   // this[16393] == 0x10024
```

## Три режима блоков

Алгоритм определяет тип блока по первым 3 битам:

```
Биты:  [TYPE:2] [FINAL:1]

FINAL = 1: Это последний блок
TYPE:
    00 = Несжатый блок (сырые данные)
    01 = Сжатый с фиксированными Huffman кодами
    10 = Сжатый с динамическими Huffman кодами
    11 = Зарезервировано (ошибка)
```

Соответствие функциям:

- type 0 → `sub_1001A750` (stored)
- type 1 → `sub_1001A8C0` (fixed Huffman)
- type 2 → `sub_1001AA30` (dynamic Huffman)

### Основной цикл декодирования

```c
int decode_block(HuffmanContext* ctx) {
    // Читаем первый бит (FINAL)
    int final_bit = read_bit(ctx);

    // Читаем 2 бита (TYPE)
    int type = read_bits(ctx, 2);

    switch (type) {
        case 0:  // 00 - Несжатый блок
            return decode_uncompressed_block(ctx);

        case 1:  // 01 - Фиксированные Huffman коды
            return decode_fixed_huffman_block(ctx);

        case 2:  // 10 - Динамические Huffman коды
            return decode_dynamic_huffman_block(ctx);

        case 3:  // 11 - Ошибка
            return 2;  // Неподдерживаемый тип
    }

    return final_bit ? 0 : 1;  // 0 = конец, 1 = есть еще блоки
}
```

## Режим 0: Несжатый блок

Простое копирование байтов без сжатия.

### Алгоритм

```python
def decode_uncompressed_block(ctx):
    """
    Формат несжатого блока:
        [LEN:16][NLEN:16][DATA:LEN]

    Где:
        LEN  - длина данных (little-endian)
        NLEN - инверсия LEN (~LEN)
        DATA - сырые данные
    """
    # Выравнивание к границе байта
    bits_to_skip = ctx.bits_available & 7
    ctx.bit_buffer >>= bits_to_skip
    ctx.bits_available -= bits_to_skip

    # Читаем длину (16 бит)
    length = read_bits(ctx, 16)

    # Читаем инверсию длины (16 бит)
    nlength = read_bits(ctx, 16)

    # Проверка целостности
    if length != (~nlength & 0xFFFF):
        return 1  # Ошибка

    # Копируем данные
    for i in range(length):
        byte = read_byte(ctx)
        write_output_byte(ctx, byte)

        # Проверка переполнения выходного буфера
        if ctx.output_position >= 0x8000:
            flush_output_buffer(ctx)

    return 0
```

### Детали

- Данные копируются "как есть"
- Используется для несжимаемых данных
- Требует выравнивания по байтам перед чтением длины

## Режим 1: Фиксированные Huffman коды

Использует предопределенные Huffman таблицы.

### Фиксированные таблицы длин кодов

```python
# Таблица для литералов/длин (288 символов)
FIXED_LITERAL_LENGTHS = [
    8, 8, 8, 8, ..., 8,   # 0-143:   коды длины 8  (144 символа)
    9, 9, 9, 9, ..., 9,   # 144-255: коды длины 9  (112 символов)
    7, 7, 7, 7, ..., 7,   # 256-279: коды длины 7  (24 символа)
    8, 8, 8, 8, ..., 8    # 280-287: коды длины 8  (8 символов)
]

# Таблица для расстояний (30 символов)
FIXED_DISTANCE_LENGTHS = [
    5, 5, 5, 5, ..., 5    # 0-29: все коды длины 5
]
```

### Алгоритм декодирования

```python
def decode_fixed_huffman_block(ctx):
    """Декодирование блока с фиксированными Huffman кодами"""

    # Инициализация фиксированных таблиц
    lit_tree = build_huffman_tree(FIXED_LITERAL_LENGTHS)
    dist_tree = build_huffman_tree(FIXED_DISTANCE_LENGTHS)

    while True:
        # Декодировать символ литерала/длины
        symbol = decode_huffman_symbol(ctx, lit_tree)

        if symbol < 256:
            # Литерал - просто вывести байт
            write_output_byte(ctx, symbol)

        elif symbol == 256:
            # Конец блока
            break

        else:
            # Символ длины (257-285)
            length = decode_length(ctx, symbol)

            # Декодировать расстояние
            dist_symbol = decode_huffman_symbol(ctx, dist_tree)
            distance = decode_distance(ctx, dist_symbol)

            # Скопировать из истории
            copy_from_history(ctx, distance, length)
```

### Таблицы экстра-бит

```python
# Дополнительные биты для длины
LENGTH_EXTRA_BITS = {
    257: 0, 258: 0, 259: 0, 260: 0, 261: 0, 262: 0, 263: 0, 264: 0,  # 3-10
    265: 1, 266: 1, 267: 1, 268: 1,                                  # 11-18
    269: 2, 270: 2, 271: 2, 272: 2,                                  # 19-34
    273: 3, 274: 3, 275: 3, 276: 3,                                  # 35-66
    277: 4, 278: 4, 279: 4, 280: 4,                                  # 67-130
    281: 5, 282: 5, 283: 5, 284: 5,                                  # 131-257
    285: 0                                                           # 258
}

LENGTH_BASE = {
    257: 3, 258: 4, 259: 5, ..., 285: 258
}

# Дополнительные биты для расстояния
DISTANCE_EXTRA_BITS = {
    0: 0, 1: 0, 2: 0, 3: 0,              # 1-4
    4: 1, 5: 1, 6: 2, 7: 2,              # 5-12
    8: 3, 9: 3, 10: 4, 11: 4,            # 13-48
    12: 5, 13: 5, 14: 6, 15: 6,          # 49-192
    16: 7, 17: 7, 18: 8, 19: 8,          # 193-768
    20: 9, 21: 9, 22: 10, 23: 10,        # 769-3072
    24: 11, 25: 11, 26: 12, 27: 12,      # 3073-12288
    28: 13, 29: 13                       # 12289-24576
}

DISTANCE_BASE = {
    0: 1, 1: 2, 2: 3, 3: 4, ..., 29: 24577
}
```

### Декодирование длины и расстояния

```python
def decode_length(ctx, symbol):
    """Декодировать длину из символа"""
    base = LENGTH_BASE[symbol]
    extra_bits = LENGTH_EXTRA_BITS[symbol]

    if extra_bits > 0:
        extra = read_bits(ctx, extra_bits)
        return base + extra

    return base


def decode_distance(ctx, symbol):
    """Декодировать расстояние из символа"""
    base = DISTANCE_BASE[symbol]
    extra_bits = DISTANCE_EXTRA_BITS[symbol]

    if extra_bits > 0:
        extra = read_bits(ctx, extra_bits)
        return base + extra

    return base
```

## Режим 2: Динамические Huffman коды

Самый сложный режим. Huffman таблицы передаются в начале блока.

### Формат заголовка динамического блока

```
Биты заголовка:
    [HLIT:5]   - Количество литерал/длина кодов - 257 (значение: 257-286)
    [HDIST:5]  - Количество расстояние кодов - 1 (значение: 1-30)
    [HCLEN:4]  - Количество длин кодов для code length алфавита - 4 (значение: 4-19)

Далее идут длины кодов для code length алфавита:
    [CL0:3] [CL1:3] ... [CL(HCLEN-1):3]

Порядок code length кодов:
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15
```

### Алгоритм декодирования

```python
def decode_dynamic_huffman_block(ctx):
    """Декодирование блока с динамическими Huffman кодами"""

    # 1. Читаем заголовок
    hlit = read_bits(ctx, 5) + 257   # Количество литерал/длина кодов
    hdist = read_bits(ctx, 5) + 1    # Количество расстояние кодов
    hclen = read_bits(ctx, 4) + 4    # Количество code length кодов

    if hlit > 286 or hdist > 30:
        return 1  # Ошибка

    # 2. Читаем длины для code length алфавита
    CODE_LENGTH_ORDER = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5,
                         11, 4, 12, 3, 13, 2, 14, 1, 15]

    code_length_lengths = [0] * 19
    for i in range(hclen):
        code_length_lengths[CODE_LENGTH_ORDER[i]] = read_bits(ctx, 3)

    # 3. Строим дерево для code length
    cl_tree = build_huffman_tree(code_length_lengths)

    # 4. Декодируем длины литерал/длина и расстояние кодов
    lengths = decode_code_lengths(ctx, cl_tree, hlit + hdist)

    # 5. Разделяем на два алфавита
    literal_lengths = lengths[:hlit]
    distance_lengths = lengths[hlit:]

    # 6. Строим деревья для декодирования
    lit_tree = build_huffman_tree(literal_lengths)
    dist_tree = build_huffman_tree(distance_lengths)

    # 7. Декодируем данные (аналогично фиксированному режиму)
    return decode_huffman_data(ctx, lit_tree, dist_tree)
```

### Декодирование длин кодов

Используется специальный алфавит с тремя специальными символами:

```python
def decode_code_lengths(ctx, cl_tree, total_count):
    """
    Декодирование последовательности длин кодов

    Специальные символы:
        16 - Повторить предыдущую длину 3-6 раз (2 доп. бита)
        17 - Повторить 0 длину 3-10 раз (3 доп. бита)
        18 - Повторить 0 длину 11-138 раз (7 доп. бит)
    """
    lengths = []
    last_length = 0

    while len(lengths) < total_count:
        symbol = decode_huffman_symbol(ctx, cl_tree)

        if symbol < 16:
            # Обычная длина (0-15)
            lengths.append(symbol)
            last_length = symbol

        elif symbol == 16:
            # Повторить предыдущую длину
            repeat = read_bits(ctx, 2) + 3
            lengths.extend([last_length] * repeat)

        elif symbol == 17:
            # Повторить ноль (короткий)
            repeat = read_bits(ctx, 3) + 3
            lengths.extend([0] * repeat)
            last_length = 0

        elif symbol == 18:
            # Повторить ноль (длинный)
            repeat = read_bits(ctx, 7) + 11
            lengths.extend([0] * repeat)
            last_length = 0

    return lengths[:total_count]
```

## Построение Huffman дерева

```python
def build_huffman_tree(code_lengths):
    """
    Построить Huffman дерево из длин кодов

    Использует алгоритм "canonical Huffman codes"
    """
    max_length = max(code_lengths) if code_lengths else 0

    # 1. Подсчитать количество кодов каждой длины
    bl_count = [0] * (max_length + 1)
    for length in code_lengths:
        if length > 0:
            bl_count[length] += 1

    # 2. Вычислить первый код для каждой длины
    code = 0
    next_code = [0] * (max_length + 1)

    for bits in range(1, max_length + 1):
        code = (code + bl_count[bits - 1]) << 1
        next_code[bits] = code

    # 3. Присвоить числовые коды символам
    tree = {}
    for symbol, length in enumerate(code_lengths):
        if length > 0:
            tree[symbol] = {
                'code': next_code[length],
                'length': length
            }
            next_code[length] += 1

    # 4. Создать структуру быстрого поиска
    lookup_table = create_lookup_table(tree)

    return lookup_table


def decode_huffman_symbol(ctx, tree):
    """Декодировать один символ из Huffman дерева"""
    code = 0
    length = 0

    for length in range(1, 16):
        bit = read_bit(ctx)
        code = (code << 1) | bit

        # Проверить в таблице быстрого поиска
        if (code, length) in tree:
            return tree[(code, length)]

    return -1  # Ошибка декодирования
```

## Управление выходным буфером

```python
def write_output_byte(ctx, byte):
    """Записать байт в выходной буфер"""
    # Записываем в окно 0x8000
    ctx.window[ctx.windowPos] = byte
    ctx.windowPos += 1

    # Если окно заполнено (32KB)
    if ctx.windowPos >= 0x8000:
        flush_output_buffer(ctx)


def flush_output_buffer(ctx):
    """Сбросить выходной буфер в финальный выход"""
    # Копируем окно в финальный выходной буфер
    dest_offset = ctx.outputPosition + ctx.destPtr
    memcpy(dest_offset, ctx.window, ctx.windowPos)

    # Обновляем счетчики
    ctx.outputPosition += ctx.windowPos
    ctx.windowPos = 0


def copy_from_history(ctx, distance, length):
    """Скопировать данные из истории (LZ77)"""
    # Позиция источника в циклическом буфере
    src_pos = (ctx.windowPos - distance) & 0x7FFF

    for i in range(length):
        byte = ctx.window[src_pos]
        write_output_byte(ctx, byte)
        src_pos = (src_pos + 1) & 0x7FFF
```

## Полная реализация на Python

```python
class HuffmanDecoder:
    """Полный RAW-DEFLATE декодер"""

    def __init__(self, input_data, output_size):
        self.input_data = input_data
        self.output_size = output_size
        self.input_pos = 0
        self.bit_buffer = 0
        self.bits_available = 0
        self.output = bytearray()
        self.history = bytearray(32768)  # 32KB циклический буфер
        self.history_pos = 0

    def read_bit(self):
        """Прочитать один бит"""
        if self.bits_available == 0:
            if self.input_pos >= len(self.input_data):
                return 0
            self.bit_buffer = self.input_data[self.input_pos]
            self.input_pos += 1
            self.bits_available = 8

        bit = self.bit_buffer & 1
        self.bit_buffer >>= 1
        self.bits_available -= 1
        return bit

    def read_bits(self, count):
        """Прочитать несколько бит (LSB first)"""
        result = 0
        for i in range(count):
            result |= self.read_bit() << i
        return result

    def write_byte(self, byte):
        """Записать байт в выход и историю"""
        self.output.append(byte)
        self.history[self.history_pos] = byte
        self.history_pos = (self.history_pos + 1) & 0x7FFF

    def copy_from_history(self, distance, length):
        """Скопировать из истории"""
        src_pos = (self.history_pos - distance) & 0x7FFF

        for _ in range(length):
            byte = self.history[src_pos]
            self.write_byte(byte)
            src_pos = (src_pos + 1) & 0x7FFF

    def decompress(self):
        """Основной цикл декомпрессии"""
        while len(self.output) < self.output_size:
            # Читаем заголовок блока
            final = self.read_bit()
            block_type = self.read_bits(2)

            if block_type == 0:
                # Несжатый блок
                if not self.decode_uncompressed_block():
                    break
            elif block_type == 1:
                # Фиксированные Huffman коды
                if not self.decode_fixed_huffman_block():
                    break
            elif block_type == 2:
                # Динамические Huffman коды
                if not self.decode_dynamic_huffman_block():
                    break
            else:
                # Ошибка
                raise ValueError("Invalid block type")

            if final:
                break

        return bytes(self.output[:self.output_size])

    # ... реализации decode_*_block методов ...
```

## Оптимизации

### 1. Таблица быстрого поиска

```python
# Предвычисленная таблица для 9 бит (первый уровень)
FAST_LOOKUP_BITS = 9
fast_table = [None] * (1 << FAST_LOOKUP_BITS)

# Заполнение таблицы при построении дерева
for symbol, info in tree.items():
    if info['length'] <= FAST_LOOKUP_BITS:
        # Все возможные префиксы для этого кода
        code = info['code']
        for i in range(1 << (FAST_LOOKUP_BITS - info['length'])):
            lookup_code = code | (i << info['length'])
            fast_table[lookup_code] = symbol
```

### 2. Буферизация битов

```python
# Читать по 32 бита за раз вместо побитового чтения
def refill_bits(self):
    """Пополнить битовый буфер"""
    while self.bits_available < 24 and self.input_pos < len(self.input_data):
        byte = self.input_data[self.input_pos]
        self.input_pos += 1
        self.bit_buffer |= byte << self.bits_available
        self.bits_available += 8
```

## Отладка и тестирование

```python
def debug_huffman_decode(data):
    """Декодирование с отладочной информацией"""
    decoder = HuffmanDecoder(data, len(data) * 10)

    original_read_bits = decoder.read_bits
    def debug_read_bits(count):
        result = original_read_bits(count)
        print(f"Read {count} bits: 0x{result:0{count//4}X} ({result})")
        return result

    decoder.read_bits = debug_read_bits
    return decoder.decompress()
```

## Заключение

Этот декодер реализует **RAW-DEFLATE** с тремя режимами блоков:

1. **Несжатый** - для несжимаемых данных
2. **Фиксированный Huffman** - быстрое декодирование с предопределенными таблицами
3. **Динамический Huffman** - максимальное сжатие с пользовательскими таблицами

**Ключевые особенности:**

- Поддержка LZ77 для повторяющихся последовательностей
- Канонические Huffman коды для эффективного декодирования
- Циклический буфер 32KB для истории
- Оптимизации через таблицы быстрого поиска

**Сложность:** O(n) где n - размер выходных данных
