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
888    packed 0xXXRRGGBB; little-endian bytes [B,G,R,X], RGBA output uses alpha 255
8888   packed 0xAARRGGBB; little-endian bytes [B,G,R,A], decoded as RGBA
```

Короткие каналы расширяются до 8 bits повторением значимых bits. Для 888
служебный четвёртый byte сохраняется в raw mip, но не попадает в RGBA output.
Native Ngi32 code confirms this layout: VA `0x10005930` packs `0xAARRGGBB` into
ARGB4444. VA `0x1000F620`/`0x1000F70C` select the format-specific converter for
888/8888; the mip pointer is passed unchanged, without a channel swizzle. On
disk, little-endian bytes are `[B,G,R,A]` for 8888 and `[B,G,R,X]` for 888.

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
