# RsLi

`RsLi` -- библиотечный архив Iron3D с каталогом в начале файла и payloads после
него.

```text
[Header: 32 bytes]
[Entry table: entry_count * 32 bytes]
[Payloads]
[optional trailer]
```

## Header fields

```text
+0x00  char[2]  "NL"
+0x02  u8       reserved
+0x03  u8       version = 1
+0x04  i16      entry_count
+0x0E  u16      presorted_flag = 0xABBA
+0x14  u32      xor_seed
```

Остальные bytes сохраняются без нормализации.

## Entry

```c
struct RsLiEntry32 {
    char     name[12];
    uint8_t  service[4];
    int16_t  flags;
    int16_t  sort_to_original;
    uint32_t unpacked_size;
    uint32_t data_offset_raw;
    uint32_t packed_size;
};
```

Имя обычно хранится в uppercase ASCII. `sort_to_original` связывает sorted
position с исходной записью.

## Table transform

Entry table проходит обратимое потоковое XOR-преобразование. Начальное
состояние берётся из младших 16 bits `xor_seed` и продолжается через всю
таблицу, не сбрасываясь на границе записи.

## Storage methods

```text
0x000  raw block
0x020  byte transform only
0x040  LZSS
0x060  transform + LZSS
0x080  adaptive Huffman + LZSS
0x0A0  transform + adaptive Huffman + LZSS
0x100  raw Deflate
```

После любого пути должно получиться ровно `unpacked_size` bytes. Методы
`0x080` и `0x0A0` подтверждены decoder-кодом, но не живыми payload демоверсии
или обеих частей.

## Compatibility quirk

`sprites.lib::INTERF8.TEX` объявляет Deflate range на один byte дальше EOF.
Совместимый reader допускает `packed_size - 1` только для этого именованного
случая. Строгий режим сообщает `deflate_eof_plus_one`.
