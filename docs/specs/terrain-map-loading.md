# Terrain + map loading

Документ описывает полный runtime-пайплайн загрузки ландшафта и карты (`Terrain.dll` + `ArealMap.dll`) и требования к toolchain для 1:1 совместимости (чтение, конвертация, редактирование, обратная сборка).

Источник реверса:

- `tmp/disassembler1/Terrain.dll.c`
- `tmp/disassembler1/ArealMap.dll.c`
- `tmp/disassembler2/Terrain.dll.asm`
- `tmp/disassembler2/ArealMap.dll.asm`

Связанные спецификации:

- [NRes / RsLi](nres.md)
- [MSH core](msh-core.md)
- [ArealMap](arealmap.md)

---

## 1. Назначение подсистем

### 1.1. `Terrain.dll`

Отвечает за:

- загрузку и хранение terrain-геометрии из `*.msh` (NRes);
- фильтрацию и выборку треугольников для коллизий/трассировки/рендера;
- рендер terrain-примитивов и связанного shading;
- использование микро-текстурного канала (chunk type 18).

Характерные runtime-строки:

- `CLandscape::CLandscape()`
- `Unable to find microtexture mapping chunk`
- `Rendering empty primitive!`
- `Rendering empty primitive2!`

### 1.2. `ArealMap.dll`

Отвечает за:

- загрузку геометрии ареалов из `*.map` (NRes, chunk type 12);
- построение связей "ареал <-> соседи/подграфы";
- grid-ускорение по ячейкам карты;
- runtime-доступ к `ISystemArealMap` (интерфейс id `770`) и ареалам (id `771`).

Характерные runtime-строки:

- `SystemArealMap panic: Cannot load ArealMapGeometry`
- `SystemArealMap panic: Cannot find chunk in resource`
- `SystemArealMap panic: ArealMap Cells are empty`
- `SystemArealMap panic: Incorrect ArealMap`

---

## 2. End-to-End загрузка уровня

### 2.1. Имена файлов уровня

В `CLandscape::CLandscape()` базовое имя уровня `levelBase` разворачивается в:

- `levelBase + ".msh"`: terrain-геометрия;
- `levelBase + ".map"`: геометрия ареалов/навигация;
- `levelBase + "1.wea"` и `levelBase + "2.wea"`: weather/материалы.

### 2.2. Порядок инициализации (высокоуровнево)

1. Получение `3DRender` и `3DSound`.
2. Загрузка `MatManager` (`*.wea`), `LightManager`, `CollManager`, `FxManager`.
3. Создание `SystemArealMap` через `CreateSystemArealMap(..., "<level>.map", ...)`.
4. Открытие terrain-библиотеки `niOpenResFile("<level>.msh")`.
5. Загрузка terrain-chunk-ов (см. §3).
6. Построение runtime-границ, grid-ускорителей и рабочих массивов.

Критичные ошибки на любом шаге приводят к `ngiProcessError`/panic.

---

## 3. Формат terrain `*.msh` (NRes)

### 3.1. Используемые chunk type в `Terrain.dll`

Порядок загрузки в `CLandscape::CLandscape()`:

| Порядок | Type | Обяз. | Использование (подтверждено кодом) |
|---|---:|---|---|
| 1 | 3 | да | поток позиций (`stride = 12`) |
| 2 | 4 | да | поток packed normal (`stride = 4`) |
| 3 | 5 | да | UV-поток (`stride = 4`) |
| 4 | 18 | да | microtexture mapping (`stride = 4`) |
| 5 | 14 | нет | опциональный доп. поток (`stride = 4`, отсутствует на части карт) |
| 6 | 21 | да | таблица terrain-face (по 28 байт) |
| 7 | 2 | да | header + slot-таблицы (используются диапазоны face) |
| 8 | 1 | да | node/grid-таблица (stride 38) |
| 9 | 11 | да | доп. индекс/ускоритель для запросов (cell->list) |

Ключевые проверки:

- отсутствие type `18` вызывает `Unable to find microtexture mapping chunk`;
- отсутствие остальных обязательных чанков вызывает `Unable to open file`.

### 3.2. Node/slot структура для terrain

Terrain-код использует те же stride и адресацию, что и core-описание:

- node-запись: `38` байт;
- slot-запись: `68` байт;
- доступ к первому slot-index: `node + 8`;
- tri-диапазон в slot: `slot + 140` (offset 0 внутри slot), `slot + 142` (offset 2).

Это согласуется с [MSH core](msh-core.md) для `Res1/Res2`:

- `Res1`: `uint16[19]` на node;
- `Res2`: header + slot table (`0x8C + N * 0x44`).

### 3.3. Terrain face record (type 21, 28 bytes)

Подтвержденные поля из runtime-декодирования face:

```c
struct TerrainFace28 {
    uint32_t flags;        // +0
    uint8_t  materialId;   // +4 (читается как byte)
    uint8_t  auxByte;      // +5
    uint16_t unk06;        // +6
    uint16_t i0;           // +8  (индекс вершины)
    uint16_t i1;           // +10
    uint16_t i2;           // +12
    uint16_t n0;           // +14 (сосед, 0xFFFF -> нет)
    uint16_t n1;           // +16
    uint16_t n2;           // +18
    int16_t  nx;           // +20 packed normal component
    int16_t  ny;           // +22
    int16_t  nz;           // +24
    uint8_t  edgeClass;    // +26 (три 2-бит значения)
    uint8_t  unk27;        // +27
};
```

`edgeClass` декодируется как:

- `edge0 = byte26 & 0x3`
- `edge1 = (byte26 >> 2) & 0x3`
- `edge2 = (byte26 >> 4) & 0x3`

### 3.4. Маски флагов face

Во многих запросах применяется фильтр:

```c
(faceFlags & requiredMask) == requiredMask &&
(faceFlags | ~forbiddenMask) == ~forbiddenMask
```

Эквивалентно: "все required-биты выставлены, forbidden-биты отсутствуют".

Подтверждено активное использование битов:

- `0x8` (особая обработка в трассировке)
- `0x2000`
- `0x20000`
- `0x100000`
- `0x200000`

Кроме "полной" 32-бит маски, runtime использует компактные маски в API-запросах.

Подтверждённый remap `full -> compactMain16` (функции `sub_10013FC0`, `sub_1004BA00`, `sub_1004BB40`):

| Full bit | Compact bit |
|---:|---:|
| `0x00000001` | `0x0001` |
| `0x00000008` | `0x0002` |
| `0x00000010` | `0x0004` |
| `0x00000020` | `0x0008` |
| `0x00001000` | `0x0010` |
| `0x00004000` | `0x0020` |
| `0x00000002` | `0x0040` |
| `0x00000400` | `0x0080` |
| `0x00000800` | `0x0100` |
| `0x00020000` | `0x0200` |
| `0x00002000` | `0x0400` |
| `0x00000200` | `0x0800` |
| `0x00000004` | `0x1000` |
| `0x00000040` | `0x2000` |
| `0x00200000` | `0x8000` |

Подтверждённый remap `full -> compactMaterial6` (функции `sub_10014090`, `sub_10015540`, `sub_1004BB40`):

| Full bit | Compact bit |
|---:|---:|
| `0x00000100` | `0x01` |
| `0x00008000` | `0x02` |
| `0x00010000` | `0x04` |
| `0x00040000` | `0x08` |
| `0x00080000` | `0x10` |
| `0x00000080` | `0x20` |

Подтверждённый remap `compact -> full` (функция `sub_10015680`):

- `a2[4]`/`a2[5]` (compactMain16 required/forbidden) + `a2[6]`/`a2[7]` (compactMaterial6 required/forbidden)
- разворачиваются в `fullRequired/fullForbidden` в `this[4]/this[5]`.

Для toolchain это означает:

- если редактируется только бинарник `type 21`, достаточно сохранять `flags` как есть;
- если реализуется API-совместимый runtime-слой, нужно поддерживать оба представления (`full` и `compact`) и точный remap выше.

### 3.5. Grid-ускоритель terrain-запросов

Runtime строит grid descriptor с параметрами:

- origin (`baseX/baseY`);
- масштабные коэффициенты (`invSizeX/invSizeY`);
- размеры сетки (`cellsX`, `cellsY`).

Дальше запросы:

1. переводят world AABB в диапазон grid-ячеек (`floor(...)`);
2. берут диапазон face через `Res1/Res2` (slot `triStart/triCount`);
3. дополняют кандидаты из cell-списков (chunk type 11);
4. применяют маски флагов;
5. выполняют геометрию (plane/intersection/point-in-triangle).

### 3.6. Cell-списки по ячейкам (`type 11` и runtime-массивы)

В `CLandscape` после инициализации используются три параллельных массива по ячейкам (`cellsX * cellsY`):

- `this+31588` (`sub_100164B0` ctor): массив записей по `12` байт, каждая запись содержит динамический буфер `8`-байтовых элементов;
- `this+31592` (`sub_100164E0` ctor): массив записей по `12` байт, каждая запись содержит динамический буфер `4`-байтовых элементов;
- `this+31596` (`sub_1001F880` ctor): массив записей по `12` байт для runtime-объектов/агентов (буфер `4`-байтовых идентификаторов/указателей).

Общий header записи списка:

```c
struct CellListHdr {
    void* ptr;      // +0
    int   count;    // +4
    int   capacity; // +8
};
```

Подтвержденные element-layout:

- `this+31588`: элемент `8` байт (`uint32_t id`, `uint32_t aux`), добавление через `sub_10012E20` пишет `aux = 0`;
- `this+31592`: элемент `4` байта (`uint32_t id`);
- `this+31596`: элемент `4` байта (runtime object handle/pointer id).

Практический вывод для редактора:

- `type 11` должен считаться источником cell-ускорителя;
- неизвестные/дополнительные поля внутри списков должны сохраняться как есть;
- нельзя "нормализовать" или переупорядочивать списки без полного пересчёта всех зависимых runtime-структур.

---

## 4. Формат `*.map` (ArealMapGeometry, chunk type 12)

### 4.1. Точка входа

`CreateSystemArealMap(..., "<level>.map", ...)` вызывает `sub_1001E0D0`:

1. `niOpenResFile("<level>.map")`;
2. поиск chunk type `12`;
3. чтение chunk-данных;
4. разбор `ArealMapGeometry`.

При ошибках выдаются panic-строки `SystemArealMap panic: ...`.

### 4.2. Верхний уровень chunk 12

Используются:

- `entry.attr1` (из каталога NRes) как `areal_count`;
- `entry[+0x0C]` как размер payload chunk для контроля полного разбора.

Данные chunk:

1. `areal_count` переменных записей ареалов;
2. секция grid-ячеек (`cellsX/cellsY` + списки попаданий).

### 4.3. Переменная запись ареала

Полностью подтверждённые элементы layout:

```c
// record = начало записи ареала
float    anchor_x      = *(float*)(record + 0);
float    anchor_y      = *(float*)(record + 4);
float    anchor_z      = *(float*)(record + 8);
float    reserved_12   = *(float*)(record + 12);        // в retail-данных всегда 0
float    area_metric   = *(float*)(record + 16);        // предрасчитанная площадь ареала
float    normal_x      = *(float*)(record + 20);
float    normal_y      = *(float*)(record + 24);
float    normal_z      = *(float*)(record + 28);        // unit vector (|n| ~= 1)
uint32_t logic_flag    = *(uint32_t*)(record + 32);     // активно используется в runtime
uint32_t reserved_36   = *(uint32_t*)(record + 36);     // в retail-данных всегда 0
uint32_t class_id      = *(uint32_t*)(record + 40);     // runtime-class/type id ареала
uint32_t reserved_44   = *(uint32_t*)(record + 44);     // в retail-данных всегда 0
uint32_t vertex_count = *(uint32_t*)(record + 48);
uint32_t poly_count   = *(uint32_t*)(record + 52);
float*   vertices     = (float*)(record + 56);          // float3[vertex_count]

// сразу после vertices:
// EdgeLink8[vertex_count + 3*poly_count]
// где EdgeLink8 = { int32_t area_ref; int32_t edge_ref; }
// первые vertex_count записей используются как per-edge соседство границы ареала.
EdgeLink8* links = (EdgeLink8*)(record + 56 + 12 * vertex_count);

uint8_t* p = (uint8_t*)(links + (vertex_count + 3 * poly_count));
for (i=0; i<poly_count; i++) {
    uint32_t n = *(uint32_t*)p;
    p += 4 * (3*n + 1);
}
// p -> начало следующей записи ареала
```

То есть для toolchain:

- поля `+0/+4/+8`, `+16`, `+20..+28`, `+32`, `+40`, `+48`, `+52` являются runtime-значимыми;
- для `links[0..vertex_count-1]` подтверждена интерпретация как `(area_ref, edge_ref)`:
  - `area_ref == -1 && edge_ref == -1` = нет соседа;
  - иначе `area_ref` указывает на индекс ареала, `edge_ref` — на индекс ребра в целевом ареале;
- при редактировании безопасно работать через parser+writer этой формулы;
- неизвестные байты внутри записи должны сохраняться без изменений.

Дополнительно по runtime-поведению:

- `anchor_x/anchor_y` валидируются на попадание внутрь полигона; при промахе движок делает случайный re-seed позиции (см. §4.5);
- `logic_flag` по смещению `+32` используется как gating-условие в логике `SystemArealMap`.

### 4.4. Секция grid-ячеек в chunk 12

После массива ареалов идёт:

```c
uint32_t cellsX;
uint32_t cellsY;
for (x in 0..cellsX-1) {
    for (y in 0..cellsY-1) {
        uint16_t hitCount;
        uint16_t areaIds[hitCount];
    }
}
```

Runtime упаковывает метаданные ячейки в `uint32`:

- high 10 bits: `hitCount` (`value >> 22`);
- low 22 bits: `startIndex` (1-based индекс в общем `uint16`-пуле areaIds).

Контроль целостности:

- после разбора `ptr_end - chunk_begin` должен строго совпасть с `entry[+0x0C]`;
- иначе `SystemArealMap panic: Incorrect ArealMap`.

### 4.5. Нормализация геометрии при загрузке

Если опорная точка ареала не попадает внутрь его полигона:

- до 100 попыток случайного сдвига в радиусе ~30;
- затем до 200 попыток в радиусе ~100.

Это runtime-correction; для 1:1-офлайн инструментов лучше генерировать валидные данные, чтобы не зависеть от недетерминизма `rand()`.

---

## 5. `BuildDat.lst` и объектные категории ареалов

`ArealMap.dll` инициализирует 12 категорий и читает `BuildDat.lst`.

Хардкод-категории (имя -> mask):

| Имя | Маска |
|---|---:|
| `Bunker_Small` | `0x80010000` |
| `Bunker_Medium` | `0x80020000` |
| `Bunker_Large` | `0x80040000` |
| `Generator` | `0x80000002` |
| `Mine` | `0x80000004` |
| `Storage` | `0x80000008` |
| `Plant` | `0x80000010` |
| `Hangar` | `0x80000040` |
| `MainTeleport` | `0x80000200` |
| `Institute` | `0x80000400` |
| `Tower_Medium` | `0x80100000` |
| `Tower_Large` | `0x80200000` |

Файл `BuildDat.lst` парсится секционно; при сбое формата используется panic `BuildDat.lst is corrupted`.

---

## 6. Требования к toolchain (конвертер/ридер/редактор)

### 6.1. Общие принципы 1:1

1. Никаких "переупорядочиваний по вкусу": сохранять порядок chunk-ов, если не требуется явная нормализация.
2. Все неизвестные поля сохранять побайтно.
3. При roundtrip обеспечивать byte-identical для неизмененных сущностей.
4. Валидации должны повторять runtime-ожидания (размеры, count-формулы, обязательность chunk-ов).

### 6.2. Для terrain `*.msh`

Обязательные проверки:

- наличие chunk types `1,2,3,4,5,11,18,21`;
- type `14` опционален;
- для `type 2`: `size >= 0x8C`, `(size - 0x8C) % 68 == 0`, `attr1 == (size - 0x8C) / 68`;
- `type21_size % 28 == 0`;
- индексы `i0/i1/i2` в `TerrainFace28` не выходят за `vertex_count` (type 3);
- `slot.triStart + slot.triCount` не выходит за `face_count`.

Сериализация:

- `flags`, соседи, `edgeClass`, material байты в `TerrainFace28` сохранять как есть;
- содержимое `type 11`-derived cell-списков (`id`, `aux`) сохранять без "починки";
- для packed normal не делать "улучшений" нормализации, если цель 1:1.

### 6.3. Для `*.map` (chunk 12)

Обязательные проверки:

- chunk type `12` существует;
- `areal_count > 0`;
- `cellsX > 0 && cellsY > 0`;
- `|normal_x,normal_y,normal_z| ~= 1` для каждого ареала;
- `links[0..vertex_count-1]` валидны (`-1/-1` или корректные `(area_ref, edge_ref)`);
- полный consumed-bytes строго равен `entry[+0x0C]`.

При редактировании:

- перестраивать только то, что действительно изменено;
- пересчитывать cell-списки и packed `cellMeta` синхронно;
- сохранять неизвестные части записи ареала без изменений.

### 6.4. Рекомендуемая архитектура редактора

1. `Parser`:
   - NRes-слой;
   - `TerrainMsh`-слой;
   - `ArealMapChunk12`-слой.
2. `Model`:
   - явные известные поля;
   - `raw_unknown` для непросаженных блоков.
3. `Writer`:
   - стабильная сериализация;
   - проверка контрольных инвариантов перед записью.
4. `Verifier`:
   - roundtrip hash/byte-compare;
   - runtime-совместимые asserts.

---

## 7. Практический чеклист "движок 1:1"

Для runtime-совместимого движка нужно реализовать:

1. NRes API-уровень (`niOpenResFile`, `niOpenResInMem`, поиск chunk по type, получение data/attrs).
2. `CLandscape` пайплайн загрузки `*.msh` + менеджеров + `CreateSystemArealMap`.
3. Terrain face decode (28-byte запись), mask-фильтр, spatial grid queries.
4. Загрузчик `ArealMapGeometry` (chunk 12) с той же валидацией и packed-cell логикой.
5. Пост-обработку ареалов (пересвязка, корректировки опорных точек).
6. Поддержку `BuildDat.lst` для объектных категорий/схем.

---

## 8. Нерасшифрованные зоны (важно для редакторов)

Ниже поля, которые пока нельзя безопасно "пересобирать по смыслу":

- семантика `class_id` (`record + 40`) на уровне геймдизайна/скриптов (числовое поле подтверждено, но человекочитаемая таблица соответствий не восстановлена полностью);
- ветки формата для `poly_count > 0` (в retail `tmp/gamedata` это всегда `0`, поэтому поведение этих веток подтверждено только по коду, без живых образцов);
- человекочитаемая семантика части битов `TerrainFace28.flags` (при этом remap и бинарные значения подтверждены);
- семантика поля `aux` во `8`-байтовом элементе cell-списка (`this+31588`, второй `uint32_t`), которое в известных runtime-путях инициализируется нулем.

Правило до полного реверса: `preserve-as-is`.

---

## 9. Эмпирическая верификация (retail `tmp/gamedata`)

Для массовой проверки спецификации добавлен валидатор:

- `tools/terrain_map_doc_validator.py`

Запуск:

```bash
python3 tools/terrain_map_doc_validator.py \
  --maps-root tmp/gamedata/DATA/MAPS \
  --report-json tmp/terrain_map_doc_validator.report.json
```

Проверенные инварианты (на 33 картах, 2026-02-12):

- `Land.msh`:
  - порядок chunk-ов всегда `[1,2,3,4,5,18,14,11,21]`;
  - `type11` первые dword всегда `[5767168, 4718593]`;
  - `type21` индексы вершин/соседей валидны;
  - `type2` slot-таблица валидна по формуле `0x8C + 68*N`.
- `Land.map`:
  - всегда один chunk `type 12`;
  - `cellsX == cellsY == 128` на всех картах;
  - `poly_count == 0` для всех `34662` записей ареалов в retail-наборе;
  - `record+12`, `record+36`, `record+44` всегда `0`;
  - `area_metric` (`record+16`) стабильно коррелирует с площадью XY-полигона (макс. абсолютное отклонение `51.39`, макс. относительное `14.73%`, `18` кейсов > `5%`);
  - `normal` в `record+20..28` всегда unit (диапазон длины `0.9999998758..1.0000001194`);
  - link-таблицы `EdgeLink8` проходят строгую валидацию ссылочной целостности.

Сводный результат текущего набора данных:

- `issues_total = 0`, `errors_total = 0`, `warnings_total = 0`.
