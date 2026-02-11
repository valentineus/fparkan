# Materials, WEAR, MAT0 и Texm

Документ описывает материальную подсистему движка (World3D/Ngi32) на уровне, достаточном для:

- реализации runtime 1:1;
- создания инструментов чтения/валидации;
- создания инструментов конвертации и редактирования с lossless round-trip.

Источник: дизассемблированные `tmp/disassembler1/*.c` и `tmp/disassembler2/*.asm`, плюс проверка на `tmp/gamedata`.

---

## 1. Идентификаторы и сущности

| Сущность | ID (LE uint32) | ASCII | Где используется |
|---|---:|---|---|
| Material resource | `0x3054414D` | `MAT0` | `Material.lib` |
| Wear resource | `0x52414557` | `WEAR` | `.wea` записи в world/mission `.rlb` |
| Texture resource | `0x6D786554` | `Texm` | `Textures.lib`, `lightmap.lib`, другие `.lib/.rlb` |
| Atlas tail chunk | `0x65676150` | `Page` | хвост payload `Texm` |

Дополнительно: палитры загружаются отдельным путём (через `SetPalettesLib` + `sub_10002B40`) и не являются `Texm`.

---

## 2. Архитектура подсистемы

### 2.1 Экспортируемые точки входа (World3D)

- `LoadMatManager`
- `SetPalettesLib`
- `SetTexturesLib`
- `SetMaterialLib`
- `SetLightMapLib`
- `SetGameTime`
- `UnloadAllTextures`

`Set*Lib` просто копируют строки путей в глобальные буферы; валидации пути нет.

### 2.2 Дефолтные библиотеки (из `iron3d.dll`)

- `Textures.lib`
- `Material.lib`
- `LightMap.lib`
- `palettes.lib` (строка собирается как `'p' + "alettes.lib"`)

### 2.3 Ключевые runtime-хранилища

1. Менеджер материалов (`LoadMatManager`) — объект `0x470` байт.
2. Кэш текстурных объектов.
3. Кэш lightmap-объектов.
4. Банк загруженных палитр.
5. Глобальный пул определений материалов (`MAT0`).

---

## 3. Layout `MatManager` (0x470)

Объект содержит 70 таблиц wear/lightmaps (не 140).

```c
// int-индексы относительно this (DWORD*), размер 284 DWORD = 0x470
// [0]   vtable
// [1]   callback iface
// [2]   callback data
// [3..72]     wearTablePtrs[70]         // ptr на массив по 8 байт
// [73..142]   wearCounts[70]
// [143]       tableCount
// [144..213]  lightmapTablePtrs[70]     // ptr на массив по 4 байта
// [214..283]  lightmapCounts[70]
```

### 3.1 Vtable методов (`off_100209E4`)

| Индекс | Функция | Назначение |
|---:|---|---|
| 0 | `loc_10002CE0` | служебный/RTTI-заглушка |
| 1 | `sub_10002D10` | деструктор + освобождение таблиц |
| 2 | `PreLoadAllTextures` | экспорт, но фактически `retn 4` (заглушка) |
| 3 | `sub_100031F0` | получить материал-фазу по `gameTime` |
| 4 | `sub_10003AE0` | сбросить startTime записи wear к `SetGameTime()` |
| 5 | `sub_10003680` | получить материал-фазу по нормализованному `t` |
| 6 | `sub_10003B10` | загрузить wear/lightmaps (файл/ресурс) |
| 7 | `sub_10003F80` | загрузить wear/lightmaps из буфера |
| 8 | `sub_100031A0` | получить указатель на lightmap texture object |
| 9 | `sub_10003AB0` | получить runtime-метаданные материала |
| 10 | `sub_100031D0` | получить `wearCount` для таблицы |

### 3.2 Кодирование material-handle

`uint32 handle = (tableIndex << 16) | wearIndex`.

- `HIWORD(handle)` -> индекс таблицы `0..69`
- `LOWORD(handle)` -> индекс материала в wear-таблице

---

## 4. Глобальные кэши и их ёмкость

Ёмкости подтверждены границами циклов/адресов в дизассемблере.

### 4.1 Кэш текстур (`dword_1014E910`...)

- Размер слота: `5 DWORD` (20 байт)
- Ёмкость: `777`

```c
struct TextureSlot {
    int32_t resIndex;        // +0  индекс записи в NRes (не hash), -1 = свободно
    void*   textureObject;   // +4
    int32_t refCount;        // +8
    uint32_t lastZeroRefTime;// +12 время, когда refCount стал 0
    uint32_t loadFlags;      // +16 флаги загрузки
};
```

`lastZeroRefTime` реально используется: texture-слоты с `refCount==0` освобождаются отложенно периодическим GC.

### 4.2 Кэш lightmaps (`dword_10029C98`...)

- Тот же layout `5 DWORD`
- Ёмкость: `100`

Для lightmap-слотов аналогичного периодического GC по `lastZeroRefTime` в `World3D` не наблюдается.

### 4.3 Пул материалов (`dword_100669F0`...)

- Шаг: `92 DWORD` (`368` байт)
- Ёмкость: `700`

Фиксированные поля на шаг `i*92`:

| DWORD offset | Byte offset | Поле |
|---:|---:|---|
| 0 | 0 | `nameResIndex` (`MAT0` entry index), `-1` = free |
| 1 | 4 | `refCount` |
| 2 | 8 | `phaseCount` |
| 3 | 12 | `phaseArrayPtr` (`phaseCount * 76`) |
| 4 | 16 | `animBlockCount` (`< 20`) |
| 5..84 | 20..339 | `animBlocks[20]` по 16 байт |
| 85 | 340 | metaA (`dword_10066B44`) |
| 86 | 344 | metaB (`dword_10066B48`) |
| 87 | 348 | metaC (`dword_10066B4C`) |
| 88 | 352 | metaD (`dword_10066B50`) |
| 89 | 356 | flagA (`dword_10066B54`) |
| 90 | 360 | nibbleMode (`dword_10066B58`) |
| 91 | 364 | flagB (`dword_10066B5C`) |

### 4.4 Банк палитр

- `dword_1013DA58[]`
- Загружается до `286` элементов (26 букв * 11 вариантов)

---

## 5. Загрузка палитр (`sub_10002B40`)

### 5.1 Генерация имён

Движок перебирает:

- буквы `'A'..'Z'`
- суффиксы: `""`, `"0"`, `"1"`, ..., `"9"`

И формирует имя:

- `<Letter><Suffix>.PAL`
- примеры: `A.PAL`, `A0.PAL`, ..., `Z9.PAL`

### 5.2 Индекс палитры

`paletteIndex = letterIndex * 11 + variantIndex`

- `letterIndex = 0..25`
- `variantIndex = 0..10` (`""`=0, `"0"`=1, ..., `"9"`=10)

### 5.3 Поведение

- Если запись не найдена: `paletteSlots[idx] = 0`
- Если найдена: payload отдаётся в рендер (`render->method+60`)

---

## 6. Формат `MAT0` (`Material.lib`)

### 6.1 Атрибуты NRes entry

`sub_10004310` использует:

- `entry.type` = `MAT0`
- `entry.attr1` (bitfield runtime-флагов)
- `entry.attr2` (версия/вариант заголовка payload)
- `entry.attr3` не используется в runtime-парсере

Маппинг `attr1`:

- bit0 (`0x01`) -> добавить флаг `0x200000` в загрузку текстур фазы
- bit1 (`0x02`) -> `flagA=1`; при некоторых HW-условиях дополнительно OR `0x80000`
- bits2..5 -> `nibbleMode = (attr1 >> 2) & 0xF`
- bit6 (`0x40`) -> `flagB=1`

### 6.2 Payload layout

```c
struct Mat0Payload {
    uint16_t phaseCount;
    uint16_t animBlockCount; // должно быть < 20, иначе "Too many animations for material."

    // Если attr2 >= 2:
    uint8_t  metaA8;
    uint8_t  metaB8;
    // Если attr2 >= 3:
    uint32_t metaC32;
    // Если attr2 >= 4:
    uint32_t metaD32;

    PhaseRecordByte34 phases[phaseCount];
    AnimBlockRaw anim[animBlockCount];
};
```

Если `attr2 < 2`, runtime-значения по умолчанию:

- `metaA = 255`
- `metaB = 255`
- `metaC = 1.0f` (`0x3F800000`)
- `metaD = 0`

### 6.3 `PhaseRecordByte34` -> runtime `76 bytes`

Сырые 34 байта:

```c
struct PhaseRecordByte34 {
    uint8_t p[18];       // параметры
    char textureName[16];// если textureName[0]==0, текстуры нет
};
```

Преобразование в runtime-структуру (точный порядок):

| Из `p[i]` | В offset runtime | Преобразование |
|---:|---:|---|
| `p[0]`  | `+16` | `p[0] / 255.0f` |
| `p[1]`  | `+20` | `p[1] / 255.0f` |
| `p[2]`  | `+24` | `p[2] / 255.0f` |
| `p[3]`  | `+28` | `p[3] * 0.01f` |
| `p[4]`  | `+0`  | `p[4] / 255.0f` |
| `p[5]`  | `+4`  | `p[5] / 255.0f` |
| `p[6]`  | `+8`  | `p[6] / 255.0f` |
| `p[7]`  | `+12` | `p[7] / 255.0f` |
| `p[8]`  | `+32` | `p[8] / 255.0f` |
| `p[9]`  | `+36` | `p[9] / 255.0f` |
| `p[10]` | `+40` | `p[10] / 255.0f` |
| `p[11]` | `+44` | `p[11] / 255.0f` |
| `p[12]` | `+48` | `p[12] / 255.0f` |
| `p[13]` | `+52` | `p[13] / 255.0f` |
| `p[14]` | `+56` | `p[14] / 255.0f` |
| `p[15]` | `+60` | `p[15] / 255.0f` |
| `p[16]` | `+64` | `uint32 = p[16]` |
| `p[17]` | `+72` | `int32 = p[17]` |

Текстура:

- `textureName[0] == 0` -> `runtime[+68] = -1` и `runtime[+72] = -1`
- иначе `runtime[+68] = LoadTexture(textureName, flags)`

### 6.4 Runtime-запись фазы (76 байт)

```c
struct MaterialPhase76 {
    float f0;   // +0
    float f1;   // +4
    float f2;   // +8
    float f3;   // +12
    float f4;   // +16
    float f5;   // +20
    float f6;   // +24
    float f7;   // +28
    float f8;   // +32
    float f9;   // +36
    float f10;  // +40
    float f11;  // +44
    float f12;  // +48
    float f13;  // +52
    float f14;  // +56
    float f15;  // +60
    uint32_t u16; // +64
    int32_t texSlot; // +68 (индекс в texture cache, либо -1)
    int32_t i18; // +72
};
```

### 6.5 Анимационные блоки (`animBlockCount`, максимум 19)

Каждый блок в payload:

```c
struct AnimBlockRaw {
    uint32_t headerRaw;   // mode = headerRaw & 7; interpMask = headerRaw >> 3
    uint16_t keyCount;
    struct KeyRaw {
        uint16_t k0;
        uint16_t k1;
        uint16_t k2;
    } keys[keyCount];
};
```

Runtime-представление блока = 16 байт:

```c
struct AnimBlockRuntime {
    uint32_t mode;      // headerRaw & 7
    uint32_t interpMask;// headerRaw >> 3
    int32_t  keyCount;
    void*    keysPtr;   // массив keyCount * 8
};
```

Ключи в runtime занимают 8 байт/ключ (с расширением `k0` до `uint32`).

`k2` в `sub_100031F0/sub_10003680` не используется.
Поле нужно сохранять lossless, т.к. оно присутствует в бинарном формате.

### 6.6 Поиск и fallback

При `LoadMaterial(name)`:

- сначала точный поиск в `Material.lib`;
- при промахе лог: `"Material %s not found."`;
- fallback на `DEFAULT`;
- если и `DEFAULT` не найден, берётся индекс `0`.

---

## 7. Выбор текущей material-фазы

### 7.1 Интерполяция (`sub_10003030`)

Интерполируются только следующие поля (по `interpMask`):

- bit `0x02`: `+4,+8,+12`
- bit `0x01`: `+20,+24,+28`
- bit `0x04`: `+36,+40,+44`
- bit `0x08`: `+52,+56,+60`
- bit `0x10`: `+32`

Не интерполируются и копируются из «текущей» фазы:

- `+0,+16,+48,+64,+68,+72`

### 7.2 Выбор по времени (`sub_100031F0`)

Вход:

- `handle` (`tableIndex|wearIndex`)
- `animBlockIndex`
- глобальное время `SetGameTime()` (`dword_10032A38`)

Для каждой wear-записи хранится `startTime` (второй DWORD пары `8-byte`).

Режимы `mode = headerRaw & 7`:

- `0`: loop
- `1`: ping-pong
- `2`: one-shot clamp
- `3`: random (`rand() % cycleLength`)

Важные детали 1:1:

- деление/остаток по циклу реализованы через unsigned `div` (`edx=0` перед делением);
- в `mode=3` вычисленное `rand() % cycleLength` записывается прямо в `startTime` записи (не в локальную переменную).
- при `gameTime < startTime` применяется unsigned-wrap семантика (важно для точного воспроизведения edge-case).

После выбора сегмента интерполяции `sub_10003030` строит scratch-материал (`unk_1013B300`), который возвращается через out-параметр.

### 7.3 Выбор по нормализованному `t` (`sub_10003680`)

Аналогично `sub_100031F0`, но time берётся как `t * cycleLength`.

Перед вычислением времени применяется runtime-нормализация:

- если `t < 0.0` или `t > 1.0`, используется `t = 0.5`.

### 7.4 Сброс времени записи

`sub_10003AE0` обновляет `startTime` конкретной wear-записи значением текущего `SetGameTime()`.

---

## 8. Формат `WEAR` (текст)

`WEAR` хранится как текст в NRes entry типа `WEAR` (`0x52414557`), обычно имя `*.wea`.

### 8.1 Грамматика

```text
<wearCount:int>\n
<legacyId:int> <materialName>\n   // повторить wearCount раз

[\n]                               // для buffer-парсера с LIGHTMAPS фактически обязательна пустая строка
[LIGHTMAPS\n
<lightmapCount:int>\n
<legacyId:int> <lightmapName>\n  // повторить lightmapCount раз]
```

- `<legacyId>` читается, но как ключ не используется.
- Идентификатором реально является имя (`materialName` / `lightmapName`).

### 8.2 Парсеры

1. `sub_10003B10`: файл/ресурсный режим.
2. `sub_10003F80`: парсер из строкового буфера.

Различие важно для совместимости:

- `sub_10003B10` после `LIGHTMAPS` сразу читает `lightmapCount` через `fscanf`.
- `sub_10003F80` после детекта `LIGHTMAPS` делает два последовательных skip до `\n`; поэтому при наличии блока `LIGHTMAPS` нужен пустой разделитель перед строкой `LIGHTMAPS`, иначе парсинг может съехать.

### 8.3 Поведение и ошибки

- `wearCount <= 0` (в текстовом файловом режиме) -> `"Illegal wear length."`
- при невозможности открыть wear-файл/entry -> `"Wear <%s> doesn't exist."`
- если найден блок `LIGHTMAPS` и `lightmapCount <= 0` -> `"Illegal lightmaps length."`
- отсутствующий материал -> `"Material %s not found."` + fallback `DEFAULT`
- отсутствующая lightmap -> `"LightMap %s not found."` и slot `-1`
- в buffer-режиме неверная структура вокруг `LIGHTMAPS` может дать некорректный `lightmapCount` и каскадные ошибки чтения.

### 8.4 Ограничения runtime

- Таблиц в `MatManager`: максимум 70 (физический layout).
- Жёсткой проверки на overflow таблиц в `sub_10003B10/sub_10003F80` нет.

Инструментам нужно явно валидировать `tableCount < 70`.

---

## 9. Загрузка texture/lightmap по имени

Общие функции:

- `sub_10004B10` — texture (`Textures.lib`)
- `sub_10004CB0` — lightmap (`LightMap.lib`)

### 9.1 Валидация имени

Алгоритм требует наличие `'.'` в позиции `0..16`.

Иначе:

- `"Bad texture name."`
- возврат `-1`

### 9.2 Palette index из суффикса

После точки разбирается:

- `L = toupper(name[dot+1])`
- `D = name[dot+2]` (опционально)
- `idx = (L - 'A') * 11 + (D ? (D - '0' + 1) : 0)`

Если `idx < 0`, палитра не подставляется (`0`).
Верхняя граница `idx` в runtime не проверяется.

Практически в стоковых ассетах имена часто вида `NAME.0`; это даёт `idx < 0`, т.е. без палитровой привязки.
Для невалидных суффиксов это потенциально даёт OOB-чтение палитрового массива.

### 9.3 Кэширование

- Дедупликация по `resIndex`.
- При повторном запросе увеличивается `refCount`, `lastZeroRefTime` сбрасывается в `0`.
- При освобождении материала `refCount` texture/lightmap уменьшается.
- texture: при `refCount -> 0` запоминается `lastZeroRefTime`; периодический sweep (примерно раз в 20 секунд) удаляет слот, если прошло больше `~60` секунд.
- lightmap: явного аналогичного sweep-пути нет; освобождение в основном происходит при teardown таблиц (`MatManager` dtor).

---

## 10. Формат `Texm`

### 10.1 Заголовок 32 байта

```c
struct TexmHeader32 {
    uint32_t magic;    // 'Texm' = 0x6D786554
    uint32_t width;
    uint32_t height;
    uint32_t mipCount;
    uint32_t flags4;
    uint32_t flags5;
    uint32_t unk6;
    uint32_t format;
};
```

### 10.2 Поддерживаемые `format`

Подтверждённые в данных:

- `0` (палитровый 8-bit)
- `565`
- `4444`
- `888`
- `8888`

Поддерживается loader-ветками Ngi32 (может встречаться в runtime-генерации):

- `556`
- `88`

### 10.3 Layout payload

1. `TexmHeader32`
2. если `format == 0`: palette table `256 * 4 = 1024` байта
3. mip-chain пикселей
4. опциональный `Page` chunk

Расчёт:

```c
bytesPerPixel =
    (format == 0) ? 1 :
    (format == 565 || format == 556 || format == 4444 || format == 88) ? 2 :
    4;

pixelCount = sum_{i=0..mipCount-1}(max(1, width>>i) * max(1, height>>i));
sizeCore   = 32 + (format == 0 ? 1024 : 0) + bytesPerPixel * pixelCount;
```

### 10.4 `Page` chunk

```c
struct PageChunk {
    uint32_t magic; // 'Page'
    uint32_t rectCount;
    struct Rect16 {
        int16_t x;
        int16_t w;
        int16_t y;
        int16_t h;
    } rects[rectCount];
};
```

Runtime конвертирует `Rect16` в:

- пиксельные прямоугольники;
- UV-границы с учётом возможного `mipSkip`.

Формулы (`s = mipSkip`):

- `x0 = x << s`, `x1 = (x + w) << s`
- `y0 = y << s`, `y1 = (y + h) << s`
- `u0 = x / (width << s)`, `du = w / (width << s)`
- `v0 = y / (height << s)`, `dv = h / (height << s)`

Также всегда добавляется базовый rect `[0]` на всю текстуру: пиксели `(0,0,width,height)`, UV `(0,0,1,1)`.

### 10.5 Loader-поведение (`sub_1000FB30`)

- Читает header в внутренние поля (`+56..+84`) напрямую:
  - `+56 magic`, `+60 width`, `+64 height`, `+68 mipCount`,
  - `+72 flags4`, `+76 flags5`, `+80 unk6`, `+84 format`.
- Для `format==0` считывает palette и переставляет каналы в runtime-таблицу.
- Считает `sizeCore`, находит tail.
- `Page` разбирается только если включён флаг загрузки `0x400000` и tail содержит `Page`.
- Может уменьшать стартовый mip (`sub_1000F580`) в зависимости от размеров/формата/флагов.
- При `DisableMipmap == 0` и допустимых условиях может строить mips в runtime.

### 10.6 Политика `mipSkip` (`sub_1000F580`)

`mipSkip` зависит от `flags5 & 0x72000000`, `width`, `height`, `mipCount`:

- если `mipCount <= 1` -> `0`
- если `flags5Mask == 0x02000000` -> `2` при `mipCount > 2`, иначе `1`
- если `flags5Mask == 0x10000000` -> `1`
- если `flags5Mask == 0x20000000`:
  - `1`, если `width >= 256` или `height >= 256`
  - иначе `0`
- если `flags5Mask == 0x40000000`:
  - если `width > 128` и `height > 128`: `2` при `mipCount > 2`, иначе `1`
  - если `width == 128` или `height == 128`: `1`
  - иначе `0`
- иначе `0`

Применение в loader:

- `mipCount -= mipSkip`
- `width >>= mipSkip`, `height >>= mipSkip`
- `pixelDataOffset += bytesPerPixel * origWidth * origHeight` для `mipSkip==1`
- `pixelDataOffset += bytesPerPixel * origWidth * origHeight * 1.25` для `mipSkip==2` (первые два уровня)

---

## 11. Флаги профиля/рендера (Ngi32)

Ключ реестра: `HKCU\Software\Nikita\NgiTool`.

Подтверждённые значения:

- `Disable MultiTexturing`
- `DisableMipmap`
- `Force 16-bit textures`
- `UseFirstCard`
- `DisableD3DCalls`
- `DisableDSound`
- `ForceCpu`

Они напрямую влияют на выбор texture format path, mip handling и fallback-ветки.

---

## 12. Спецификация для toolchain (read/edit/write)

### 12.1 Каноническая модель данных

1. `MAT0`:
- хранить исходные `attr1/attr2/attr3`;
- хранить сырой payload + декодированную структуру;
- при записи сохранять порядок/размеры секций точно.

2. `WEAR`:
- хранить строки wear/lightmaps как текст;
- сохранять порядок строк;
- допускать отсутствие блока `LIGHTMAPS`.
- если нужен полный runtime-parity с buffer-парсером (`sub_10003F80`) и есть `LIGHTMAPS`, сохранять пустую строку-разделитель перед строкой `LIGHTMAPS`.

3. `Texm`:
- хранить header поля как есть (`flags4/flags5/unk6` не нормализовать);
- хранить palette (если есть), mip data, `Page`.

### 12.2 Правила lossless записи

- Не менять значения `flags4/flags5/unk6` без явной причины.
- Не менять `NRes` entry attrs, если цель — бинарный round-trip.
- Для `MAT0`:
  - `animBlockCount < 20`.
  - `phaseCount` и фактический размер секции должны совпадать.
  - textureName в фазе всегда укладывать в 16 байт и NUL-терминировать.
- Для `Texm`:
  - `magic == 'Texm'`.
  - `mipCount > 0`, `width>0`, `height>0`.
  - tail либо отсутствует, либо ровно один корректный `Page` chunk без лишних байт.
  - при эмуляции runtime-загрузчика учитывать, что `Page` обрабатывается только при load-flag `0x400000`.

### 12.3 Рекомендованные валидации редактора

- `WEAR`:
  - `wearCount > 0`.
  - число строк wear соответствует `wearCount`.
  - если есть `LIGHTMAPS`, то `lightmapCount > 0` и число строк совпадает.
  - для buffer-совместимого текста с `LIGHTMAPS` проверять наличие пустой строки перед `LIGHTMAPS`.
- `MAT0`:
  - не выходить за payload при распаковке.
  - все ссылки фаз/keys проверять на диапазоны.
- `Texm`:
  - `sizeCore <= payload_size`.
  - проверка `Page` как `8 + rectCount*8`.
  - предупреждать/блокировать невалидные palette suffix, которые могут дать `idx >= 286` в runtime.

---

## 13. Проверка на реальных данных (`tmp/gamedata`)

### 13.1 `Material.lib`

- `905` entries, все `type=MAT0`
- `attr2 = 6` у всех
- `attr3 = 0` у всех
- `phaseCount` до `29`
- `animBlockCount` до `8` (ограничение runtime `<20` соблюдается)

### 13.2 `Textures.lib`

- `393` entries, все `type=Texm`
- форматы: `8888(237), 888(52), 565(47), 4444(42), 0(15)`
- `flags4`: `32(361), 0(32)`
- `flags5`: `0(312), 0x04000000(81)`
- `Page` chunk присутствует у `65` текстур

### 13.3 `lightmap.lib`

- `25` entries, все `Texm`
- формат: `565`
- `mipCount=1`
- `flags5`: в основном `0`, встречается `0x00800000`

### 13.4 `WEAR`

- `439` entries `type=WEAR`
- `attr1=0, attr2=0, attr3=1`
- `21` entry содержит блок `LIGHTMAPS` (в текущем наборе везде `lightmapCount=1`)
- для всех `21` entry с `LIGHTMAPS` присутствует пустая строка перед `LIGHTMAPS`.

---

## 14. Opaque-поля и границы знания

Для 1:1 runtime/toolchain достаточно фиксировать следующие поля как `opaque-but-required`:

- `MAT0`:
  - `k2` в `AnimBlockRaw::KeyRaw` (хранить/писать без изменений);
  - `metaA/metaB/metaC/metaD` (в `World3D` заполняются и возвращаются наружу; внутренних consumers этих мета-полей не найдено).
- `Texm`:
  - `flags4/flags5/unk6` (часть веток разобрана, но полная доменная семантика не требуется для 1:1).

Это не блокирует реализацию движка/конвертеров 1:1.

---

## 15. Минимальные псевдокоды для реализации

### 15.1 `parse_mat0(payload, attr2)`

```python
def parse_mat0(payload: bytes, attr2: int):
    cur = 0
    phase_count = u16(payload, cur); cur += 2
    anim_count  = u16(payload, cur); cur += 2
    if anim_count >= 20:
        raise ValueError("Too many animations for material")

    if attr2 < 2:
        metaA, metaB, metaC, metaD = 255, 255, 0x3F800000, 0
    else:
        metaA = u8(payload, cur); cur += 1
        metaB = u8(payload, cur); cur += 1
        metaC = u32(payload, cur) if attr2 >= 3 else 0x3F800000
        cur  += 4 if attr2 >= 3 else 0
        metaD = u32(payload, cur) if attr2 >= 4 else 0
        cur  += 4 if attr2 >= 4 else 0

    phases = [payload[cur + i*34 : cur + (i+1)*34] for i in range(phase_count)]
    cur += 34 * phase_count

    anim = []
    for _ in range(anim_count):
        raw = u32(payload, cur); cur += 4
        key_count = u16(payload, cur); cur += 2
        keys = [payload[cur + k*6 : cur + (k+1)*6] for k in range(key_count)]
        cur += 6 * key_count
        anim.append((raw, keys))

    if cur != len(payload):
        raise ValueError("MAT0 tail bytes")

    return phase_count, anim_count, metaA, metaB, metaC, metaD, phases, anim
```

### 15.2 `parse_texm(payload)`

```python
def parse_texm(payload: bytes):
    magic, w, h, mips, f4, f5, unk6, fmt = unpack_u32x8(payload, 0)
    if magic != 0x6D786554:
        raise ValueError("not Texm")

    bpp = 1 if fmt == 0 else (2 if fmt in (565, 556, 4444, 88) else 4)
    pix = 0
    mw, mh = w, h
    for _ in range(mips):
        pix += mw * mh
        mw = max(1, mw >> 1)
        mh = max(1, mh >> 1)

    core = 32 + (1024 if fmt == 0 else 0) + bpp * pix
    if core > len(payload):
        raise ValueError("truncated")

    page = None
    if core < len(payload):
        if core + 8 > len(payload) or payload[core:core+4] != b"Page":
            raise ValueError("tail without Page")
        n = u32(payload, core + 4)
        need = 8 + n * 8
        if core + need != len(payload):
            raise ValueError("invalid Page size")
        page = [unpack_i16x4(payload, core + 8 + i*8) for i in range(n)]

    return (w, h, mips, fmt, f4, f5, unk6, page)
```

### 15.3 `mip_skip_policy(flags5, width, height, mip_count)`

```python
def mip_skip_policy(flags5: int, width: int, height: int, mip_count: int) -> int:
    if mip_count <= 1:
        return 0

    m = flags5 & 0x72000000
    if m == 0x02000000:
        return 2 if mip_count > 2 else 1
    if m == 0x10000000:
        return 1
    if m == 0x20000000:
        return 1 if (width >= 256 or height >= 256) else 0
    if m == 0x40000000:
        if width > 128 and height > 128:
            return 2 if mip_count > 2 else 1
        if width == 128 or height == 128:
            return 1
    return 0
```

### 15.4 `parse_wear_buffer_compatible(text)`

```python
def parse_wear_buffer_compatible(text: str):
    lines = text.splitlines()
    i = 0

    wear_count = int(lines[i].strip()); i += 1
    if wear_count <= 0:
        raise ValueError("Illegal wear length.")

    wear = []
    for _ in range(wear_count):
        legacy, name = lines[i].split(maxsplit=1)
        wear.append((int(legacy), name.strip()))
        i += 1

    lightmaps = []
    tail = lines[i:] if i < len(lines) else []
    if tail and tail[0].strip() == "":
        # sub_10003F80-совместимый разделитель перед LIGHTMAPS
        i += 1
        tail = lines[i:]

    if tail and tail[0].strip().upper() == "LIGHTMAPS":
        i += 1
        if i >= len(lines):
            raise ValueError("Illegal lightmaps length.")
        light_count = int(lines[i].strip()); i += 1
        if light_count <= 0:
            raise ValueError("Illegal lightmaps length.")
        for _ in range(light_count):
            legacy, name = lines[i].split(maxsplit=1)
            lightmaps.append((int(legacy), name.strip()))
            i += 1

    return wear, lightmaps
```

### 15.5 `select_phase_time_1to1(...)`

```python
def select_phase_time_1to1(game_time: int, start_time: int, keys, mode: int):
    # keys: list[(phase_index, t_start, t_end)], t_end последнего = cycle_len
    cycle_len = keys[-1][2]
    if cycle_len <= 0:
        return 0, 0.0

    # unsigned div/mod как в runtime
    delta = (game_time - start_time) & 0xFFFFFFFF
    q = delta // cycle_len
    r = delta % cycle_len

    if mode == 1:  # ping-pong
        if q & 1:
            r = cycle_len - r
    elif mode == 2:  # one-shot
        if q > 0:
            k = len(keys) - 1
            return k, 0.0
    elif mode == 3:  # random
        r = rand32() % cycle_len
        start_time = r  # side effect как в sub_100031F0

    k = find_segment(keys, r)          # t_start <= r < t_end
    kn = 0 if (k + 1 == len(keys)) else (k + 1)
    t0, t1 = keys[k][1], keys[k][2]
    alpha = 0.0 if t1 == t0 else (r - t0) / float(t1 - t0)
    return (k, kn), alpha
```
