# Texture (`Texm`)

`Texm` — основной формат текстур движка.

Связанные страницы:

- [Material (`MAT0`)](material.md)
- [Wear table (`WEAR`)](wear.md)
- [Render pipeline](render.md)

## 1. Контейнер

- Тип ресурса: `0x6D786554` (`Texm`).
- Используется в `Textures.lib`, `LightMap.lib` и других `NRes` архивах.

## 2. Заголовок

```c
struct TexmHeader32 {
    uint32_t magic;    // 'Texm'
    uint32_t width;
    uint32_t height;
    uint32_t mipCount;
    uint32_t flags4;
    uint32_t flags5;
    uint32_t unk6;
    uint32_t format;
};
```

## 3. Поддерживаемые форматы

Базовые форматы:

- `0` (8-bit indexed + palette)
- `565`
- `4444`
- `888`
- `8888`

Дополнительные ветки загрузки поддерживают также `556` и `88`.

## 4. Layout payload

1. `TexmHeader32` (32 байта)
2. palette `1024` байта, если `format == 0`
3. mip-chain пикселей
4. optional `Page` chunk

Расчёт ядра:

```c
bytesPerPixel =
    (format == 0) ? 1 :
    (format == 565 || format == 556 || format == 4444 || format == 88) ? 2 :
    4;

pixelCount = sum(max(1, width>>i) * max(1, height>>i), i=0..mipCount-1);
sizeCore = 32 + (format==0 ? 1024 : 0) + bytesPerPixel * pixelCount;
```

## 5. `Page` chunk

```c
struct PageChunk {
    uint32_t magic;      // 'Page'
    uint32_t rectCount;
    Rect16   rects[rectCount];
};

struct Rect16 {
    int16_t x;
    int16_t w;
    int16_t y;
    int16_t h;
};
```

`Page` задаёт atlas-прямоугольники для выборки под-областей текстуры.

## 6. Mip-skip политика

Загрузчик может пропускать первые mip-уровни в зависимости от:

- `flags5`,
- размеров текстуры,
- количества mip.

После `mipSkip`:

- уменьшаются `width/height/mipCount`;
- сдвигается начало пиксельных данных;
- `Page`-координаты пересчитываются в соответствии с новым базовым уровнем.

## 7. Палитры

Для части текстур движок связывает палитру по суффиксу имени.

Практический формат:

- буква `A..Z` + вариант `""` или `0..9`
- всего `26 * 11 = 286` возможных слотов палитр.

Невалидные суффиксы нужно считать ошибкой входных данных в инструментах.

## 8. Кэширование

Движок ведёт отдельные кэши:

- общий texture cache;
- lightmap cache.

Для обычных текстур используется отложенный сбор неиспользуемых слотов (по времени нулевого refcount).

## 9. Правила writer/editor

1. Не нормализовать `flags4/flags5/unk6`.
2. Сохранять payload без лишних хвостовых байт.
3. Если есть `Page`, его размер должен быть ровно `8 + rectCount * 8`.
4. Проверять `width > 0`, `height > 0`, `mipCount > 0`.

## 10. Статус валидации

- Инварианты `Texm` реализованы в `tools/msh_doc_validator.py`.
- В текущем окружении нет полного игрового набора текстур в `testdata`, поэтому массовая перепроверка не запускалась.
