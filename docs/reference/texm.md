# Texm

`Texm` -- основной формат изображений Iron3D. Payload содержит header,
необязательную палитру, mip chain и иногда `Page` chunk.

```c
struct TexmHeader32 {
    uint32_t magic;      // 'Texm'
    uint32_t width;
    uint32_t height;
    uint32_t mip_count;
    uint32_t flags4;
    uint32_t flags5;
    uint32_t unknown6;
    uint32_t format;
};
```

## Pixel formats

```text
0      Indexed8 + palette 256 * 4 bytes
565    R5 G6 B5
556    R5 G5 B6
4444   A4 R4 G4 B4
88     L8 A8
888    RGB8 in four-byte element
8888   A8 R8 G8 B8
```

Короткие каналы расширяются до 8 bits повторением значимых bits. Для 888
служебный четвёртый byte сохраняется при roundtrip.

## Layout

```text
TexmHeader32
[palette 1024 bytes, only for format 0]
level 0 pixels
level 1 pixels
...
level mip_count-1 pixels
[optional Page chunk]
```

Размер mip level вычисляется через `max(1, width >> i)` и
`max(1, height >> i)`. Parser суммирует размеры с проверкой переполнения до
чтения данных.

## Page chunk

```c
struct PageHeader8 {
    uint32_t magic;      // 'Page'
    uint32_t rect_count;
};

struct PageRect8 {
    int16_t x;
    int16_t width;
    int16_t y;
    int16_t height;
};
```

Chunk обязан иметь размер `8 + rect_count * 8`. Rectangles находятся в pixel
space базового mip и масштабируются после mip-skip.
