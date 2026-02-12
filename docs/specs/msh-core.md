# MSH core

Документ фиксирует core-часть формата MSH на уровне, достаточном для:

- реализации runtime-совместимого движка (поведение 1:1);
- реализации reader/writer/editor/converter с lossless round-trip;
- валидации ассетов и диагностики повреждений.

Связанные документы:

- [NRes / RsLi](nres.md) — контейнер, каталог, атрибуты, выравнивание.
- [MSH animation](msh-animation.md) — детальная спецификация `Res8`/`Res19`.
- [Materials + Texm](materials-texm.md) — материальная часть и текстуры.
- [Terrain + map loading](terrain-map-loading.md) — отдельная ветка terrain-ресурсов.

---

## 1. Область и источники

### 1.1. Что покрывает этот документ

Этот документ покрывает именно **core-геометрию и её runtime-связи**:

- `Res1` (node table),
- `Res2` (header + slots),
- `Res3/4/5` (позиции/нормали/UV0),
- `Res6` (индексы),
- `Res7` (triangle descriptors),
- `Res10` (node string table),
- `Res13` (batch table),
- optional `Res15/16/18/20`,
- точки стыка с анимацией (`Res8/Res19`).

### 1.2. Что не покрывает

- детальную семантику материалов/текстурных фаз (см. `materials-texm.md`),
- terrain-ветку (`type 11/14/21` и связанные структуры, см. `terrain-map-loading.md`),
- полную математику анимационного сэмплирования (см. `msh-animation.md`).

### 1.3. Источники реверса

Основные подтверждения:

- `tmp/disassembler1/AniMesh.dll.c`:
  - `sub_10015FD0` (загрузка ресурсов core-модели),
  - `sub_100124D0` (поиск slot по node/lod/group),
  - `sub_10012530` (доступ к строке узла в `Res10`),
  - `sub_1000B2C0`/`sub_10013680` (tri/batch path),
  - `sub_1000A460` (инициализация runtime-инстансов, копирование глобальных bounds).
- `tmp/disassembler2/AniMesh.dll.asm` — подтверждение смещений/stride/ветвлений.
- валидация corpus: `testdata/nres` (435 MSH моделей, нулевые ошибки в `tools/msh_doc_validator.py`).

---

## 2. Модель данных MSH (high-level)

MSH-модель — это NRes-контейнер, где ресурсы связаны **не по порядку, а по type-id**.

Базовая связь таблиц:

1. `Res1` для `(node, lod, group)` выбирает `slotIndex`.
2. `Res2.slot[slotIndex]` даёт диапазоны triangle/batch (`triStart/triCount`, `batchStart/batchCount`).
3. `Res13.batch` даёт `indexStart/indexCount/baseVertex`.
4. `Res6` даёт сырые `uint16` индексы.
5. `Res3/4/5` дают vertex-атрибуты по `baseVertex + index`.

Ключевая особенность runtime:

- скиннинг по узлам жёсткий (rigid attachment), без per-vertex bone weights в core-ресурсах.

---

## 3. Карта ресурсов и границы core

### 3.1. Ресурсы, которые читает core-loader (`sub_10015FD0`)

| Type | Ресурс | Статус в core-loader | Формат/stride |
|---:|---|---|---|
| 1 | Node table | required | 38 байт/узел (основной случай) |
| 2 | Model header + slots | required | `0x8C + slotCount*0x44` |
| 3 | Positions | required | 12 |
| 4 | Packed normals | обычно required | 4 |
| 5 | Packed UV0 | обычно required | 4 |
| 6 | Index buffer | required | 2 |
| 7 | Triangle descriptors | обычно required | 16 |
| 8 | Anim key pool | optional для статических | 24 |
| 10 | String table | обычно required | variable |
| 13 | Batch table | required | 20 |
| 15 | Доп. stream | optional | 8 |
| 16 | Tangent/bitangent stream | optional | 8 |
| 18 | Vertex color stream | optional | 4 |
| 19 | Anim mapping | optional для статических | 2 |
| 20 | Доп. таблица | optional | variable |

### 3.2. Ресурсы, которые встречаются в MSH, но вне этого документа

В corpus из 435 моделей стабильно встречаются также `type 9` и `type 17`.
Они **не загружаются** `sub_10015FD0` и относятся к некоревым подсистемам (материалы/эффекты/прочие runtime-ветки).

### 3.3. Прямая MSH и вложенная MSH

Tooling должен поддерживать два режима входа:

- файл уже является модельным NRes (`magic NRes` и содержит `type 1/2/3/6/13`),
- файл-архив содержит `.msh` entry, внутри которой вложенный NRes модели.

---

## 4. Runtime-контракт загрузки (`sub_10015FD0`)

`sub_10015FD0` заполняет структуру модели размером `0xA4` байт и строит derived pointers/stride.

### 4.1. Порядок `find/open`

Фактический порядок загрузки:

1. `type 1 -> this+0x00`
2. `type 2 -> this+0x04`
3. `type 3 -> this+0x0C`
4. `type 4 -> this+0x10`
5. `type 5 -> this+0x14`
6. `type 10 -> this+0x20`
7. `type 8 -> this+0x18`
8. `type 19 -> this+0x1C`
9. `type 7 -> this+0x24`
10. `type 13 -> this+0x28`
11. `type 6 -> this+0x2C`
12. `type 15 -> this+0x34`
13. `type 16 -> this+0x38`
14. `type 18 -> this+0x64` (через отдельный `find`, optional)
15. `type 20 -> this+0x30` (optional)

### 4.2. Derived-поля (стримы)

После загрузки ставятся derived-поля:

- `this+0x08 = Res2 + 0x8C` (начало slot table),
- `this+0x3C = Res3`, `this+0x40 = 12`,
- `this+0x44 = Res4`, `this+0x48 = 4`,
- `this+0x5C = Res5`, `this+0x60 = 4`,
- `this+0x8C = Res15`, `this+0x90 = 8`,
- `this+0x94 = 0` (инициализация нулём).

Для `Res16`:

- если есть: `this+0x4C = Res16`, `this+0x50 = 8`, `this+0x54 = Res16+4`, `this+0x58 = 8`;
- если нет: `this+0x4C = 0`, `this+0x54 = 0` (stride остаются несущественными, т.к. указатели нулевые).

Для `Res18`:

- если найден: `this+0x64 = ptr`, `this+0x68 = 4`;
- иначе: `this+0x64 = 0`, `this+0x68 = 0`.

### 4.3. Метаданные из каталога NRes

- `this+0x9C` получает `entry(type19).attr2` (читается из поля `+8` каталожной записи, индекс `entry * 64`).
- `this+0xA0` получает `entry(type20).attr1` (поле `+4`) только если `type20` существует и успешно открыт; иначе `0`.

---

## 5. Бинарные структуры core-ресурсов

Все структуры little-endian.

### 5.1. `Res1` — Node table

Базовый stride: `38` байт (`19 * uint16`).

```c
struct Node38 {
    uint16_t hdr0;            // +0
    uint16_t hdr1;            // +2
    uint16_t hdr2;            // +4
    uint16_t hdr3;            // +6
    uint16_t slotIndex[15];   // +8: [lod0 g0..g4][lod1 g0..g4][lod2 g0..g4]
};
```

#### Подтверждённые поля

- `hdr1`: parent/index-link (используется при построении инстанса), `0xFFFF` = нет.
- `hdr2`: `mapStart` для `Res19` (см. `msh-animation.md`), `0xFFFF` = нет map.
- `hdr3`: fallback key index в `Res8`.
- `hdr0`: node flags (есть битовые проверки, но полная доменная семантика не закрыта).

#### Адресация slot (runtime-функция `sub_100124D0`)

```c
uint16_t get_slot_index(const Node38* node_table, uint32_t nodeIndex, int lod, int group, int current_lod) {
    int use_lod = (lod == -1) ? current_lod : lod;
    int word_index = 4 + (int)nodeIndex * 19 + use_lod * 5 + group;
    return *(uint16_t*)((const uint8_t*)node_table + word_index * 2);
}
```

`0xFFFF` означает "слот отсутствует".

#### Вариант stride=24

В corpus есть единичный служебный outlier с `Res1.attr3 = 24`.
Для 1:1 editing существующих ассетов требуется copy-through этого варианта.
Новая генерация должна ориентироваться на stride `38`, если нет чёткой цели поддержать legacy-вариант.

---

### 5.2. `Res2` — Model header + Slot table

```
Res2:
  [0x00 .. 0x8B]   model header (140 bytes)
  [0x8C .. end]    slot records (68 bytes each)
```

#### 5.2.1. Header (0x8C)

Runtime копирует блоки как float-массивы:

- `0x00..0x5F` (`24 float`) — глобальный hull (`vec3[8]`),
- `0x60..0x6F` (`4 float`) — глобальная sphere (`center.xyz + radius`),
- `0x70..0x8B` (`7 float`) — сегмент/капсула (`A.xyz`, `B.xyz`, `radius`).

#### 5.2.2. Slot record (68 bytes)

```c
struct Slot68 {
    uint16_t triStart;      // +0
    uint16_t triCount;      // +2
    uint16_t batchStart;    // +4
    uint16_t batchCount;    // +6

    float aabbMin[3];       // +8
    float aabbMax[3];       // +20
    float sphereCenter[3];  // +32
    float sphereRadius;     // +44

    uint32_t unk30;         // +48
    uint32_t unk34;         // +52
    uint32_t unk38;         // +56
    uint32_t unk3C;         // +60
    uint32_t unk40;         // +64
};
```

`triCount` подтверждён как длина диапазона:

```c
triId >= triStart && triId < triStart + triCount
```

Хвост `unk30..unk40` должен сохраняться без изменений в editor/writer.

#### 5.2.3. Bounds semantics

- Slot bounds локальны относительно узла.
- При world-трансформации sphere radius масштабируется по `max(scaleX, scaleY, scaleZ)` при неравномерном scale.

---

### 5.3. `Res3` — Positions

```c
struct Position12 {
    float x;
    float y;
    float z;
};
```

Stride `12`.

---

### 5.4. `Res4` — Packed normals

```c
struct PackedNormal4 {
    int8_t nx;
    int8_t ny;
    int8_t nz;
    int8_t nw; // семантика 4-го байта не зафиксирована
};
```

Декодирование:

```c
normal = clamp((float)n / 127.0f, -1.0f, 1.0f)
```

- делитель строго `127.0`;
- clamp обязателен из-за `-128 / 127.0`.

Кодирование (writer):

```c
int8_t q = (int8_t)clamp(round(v * 127.0f), -128, 127);
```

---

### 5.5. `Res5` — Packed UV0

```c
struct PackedUV4 {
    int16_t u;
    int16_t v;
};
```

Декодирование:

```c
uv = packed / 1024.0f
```

Кодирование:

```c
int16_t q = (int16_t)clamp(round(uv * 1024.0f), -32768, 32767);
```

---

### 5.6. `Res6` — Index buffer

Массив `uint16`, stride `2`.

Runtime-путь:

```c
vertexIndex = Res6[indexStart + i] + batch.baseVertex;
```

`indexStart` хранится в элементах, не в байтах.

---

### 5.7. `Res7` — Triangle descriptors (16 bytes)

```c
struct TriDesc16 {
    uint16_t triFlags;    // +0
    uint16_t linkTri0;    // +2
    uint16_t linkTri1;    // +4
    uint16_t linkTri2;    // +6
    int16_t  nX;          // +8
    int16_t  nY;          // +10
    int16_t  nZ;          // +12
    uint16_t selPacked;   // +14
};
```

- `nX/nY/nZ` декодируются через `1/32767`.
- `linkTri*` используются в tri-neighbour/collision path.

Раскладка `selPacked` (3 селектора по 2 бита):

```c
sel0 = (selPacked >> 0) & 0x3; if (sel0 == 3) sel0 = 0xFFFF;
sel1 = (selPacked >> 2) & 0x3; if (sel1 == 3) sel1 = 0xFFFF;
sel2 = (selPacked >> 4) & 0x3; if (sel2 == 3) sel2 = 0xFFFF;
```

---

### 5.8. `Res13` — Batch table (20 bytes)

```c
struct Batch20 {
    uint16_t batchFlags;     // +0
    uint16_t materialIndex;  // +2
    uint16_t unk4;           // +4
    uint16_t unk6;           // +6
    uint16_t indexCount;     // +8
    uint32_t indexStart;     // +10
    uint16_t unk14;          // +14
    uint32_t baseVertex;     // +16
};
```

`unk4/unk6/unk14` семантически не закрыты; writer/editor должны сохранять.

---

### 5.9. `Res10` — Node string table

Последовательность записей variable-length:

```c
struct Res10Record {
    uint32_t len;   // длина строки без '\0'
    char text[];    // если len>0: len+1 байт (с '\0'), иначе payload нет
};
```

Переход:

```c
next = cur + 4 + (len ? len + 1 : 0);
```

`sub_10012530` возвращает:

- `NULL`, если `len == 0`,
- `record + 4`, если `len > 0`.

Индекс записи в `Res10` соответствует `nodeIndex`.

---

### 5.10. Optional streams

#### `Res15` (stride 8)

Дополнительный поток на вершину (семантика не полностью подтверждена).

#### `Res16` (stride 8, split 2x4)

Runtime делит поток на два interleaved подпотока:

- stream A: `base+0`, stride 8,
- stream B: `base+4`, stride 8.

В corpus из `testdata/nres` этот ресурс не встретился, но loader поддерживает.

#### `Res18` (stride 4)

Vertex color / доп. packed-канал. В corpus встречается на части моделей.

#### `Res20`

Доп. таблица неизвестной доменной семантики. Loader хранит pointer и метаданные каталога (`attr1`).

---

### 5.11. Точки стыка с анимацией (`Res8`/`Res19`)

Core-loader загружает:

- `Res8` в `this+0x18`,
- `Res19` в `this+0x1C`,
- `Res19.attr2` в `this+0x9C`.

Полный runtime-алгоритм сэмплирования/смешивания описан в [MSH animation](msh-animation.md).

---

## 6. Runtime-алгоритмы core

### 6.1. Slot lookup (`sub_100124D0`)

Вход: runtime-node-instance, `group`, `lod`.

1. Если нет model pointer -> `NULL`.
2. `lod == -1` -> подставить `current_lod` инстанса.
3. Вычислить `slotIndex` через формулу `4 + node*19 + lod*5 + group`.
4. Если `slotIndex == 0xFFFF` -> `NULL`.
5. Иначе вернуть `Res2.slotBase + slotIndex * 68`.

### 6.2. Node string lookup (`sub_10012530`)

1. Идти по `Res10`-записям `nodeIndex` раз.
2. Возвращать `NULL` или `char*` по правилу `len==0`.

### 6.3. Геометрический обход для рендера

Reference-путь, эквивалентный runtime-логике:

```c
for each node:
    slot = resolve_slot(node, lod, group)
    if (!slot) continue

    for b in [slot.batchStart .. slot.batchStart + slot.batchCount):
        batch = Res13[b]
        for i in [0 .. batch.indexCount):
            idx = Res6[batch.indexStart + i]
            vtx = batch.baseVertex + idx

            pos = Res3[vtx]
            nrm = decode_res4(Res4[vtx])
            uv0 = decode_res5(Res5[vtx])
```

### 6.4. Tri/collision path (обобщённо)

- `sub_1000B2C0` и `sub_10013680` используют tri-диапазоны слота + `Res7` link/select-поля.
- Для collision/picking-контекста должны быть валидны:
  - `slot.triStart + slot.triCount <= triDescCount`,
  - `linkTri*` либо `0xFFFF`, либо `< triDescCount`.

---

## 7. Инварианты и валидация (reader)

### 7.1. Базовые проверки целостности

- каждый fixed-stride ресурс делится на stride без остатка;
- `Res2.size >= 0x8C`;
- `(Res2.size - 0x8C) % 68 == 0`;
- `Res2.attr1 == slotCount`, `Res2.attr3 == 68`;
- `Res3.attr3 == 12`, `Res4.attr3 == 4`, `Res5.attr3 == 4`, `Res6.attr3 == 2`, `Res7.attr3 == 16`, `Res13.attr3 == 20`;
- `Res8.attr3 == 4` (не stride), `Res19.attr3 == 2`, `Res10.attr3 == 0` (в observed assets).

### 7.2. Cross-table проверки

- `slot.batchStart + slot.batchCount <= batchCount`;
- `slot.triStart + slot.triCount <= triDescCount`;
- `batch.indexStart + batch.indexCount <= indexCount`;
- `batch.baseVertex + max(indexSlice) < vertexCount`;
- все `Res1.slotIndex[*]` либо `0xFFFF`, либо `< slotCount`;
- для `Res10`: парсинг ровно `nodeCount` записей без хвостовых байт;
- для `Res7.linkTri*`: либо `0xFFFF`, либо `< triDescCount`.

### 7.3. Strict vs tolerant режим

Рекомендуется 2 режима reader:

- `strict`: любое нарушение инвариантов -> ошибка;
- `tolerant`: безопасно отбрасывать/игнорировать только локально повреждённые диапазоны (без OOB).

---

## 8. Правила writer/editor

### 8.1. Обязательная политика для 1:1 editing

- сохранять неизвестные поля (`Slot68.unk*`, `Batch20.unk*`, `Node.hdr0` и т.д.) без модификации, если нет осознанного пересчёта;
- сохранять неизвестные resource types и их payload/атрибуты;
- не полагаться на порядок ресурсов в контейнере: lookup в runtime идёт по type-id.

### 8.2. Пересчёт атрибутов каталога

При записи изменённых ресурсов:

- `attr1` = count (или форматно-специфичное значение),
- `attr2` — по формату/семантике ресурса,
- `attr3` — stride/константа формата.

Практические правила для core:

- `Res1`: `attr1=nodeCount`, `attr3=38` (или исходный вариант, если copy-through legacy), `attr2` лучше сохранять из исходника;
- `Res2`: `attr1=slotCount`, `attr2=0`, `attr3=68`;
- `Res3/4/5/6/7/13/15/16/18`: `attr1=size/stride`, `attr2=0`, `attr3=stride`;
- `Res8`: `attr1=size/24`, `attr3=4`;
- `Res10`: `attr1=nodeCount`, `attr2=0`, `attr3=0`;
- `Res19`: `attr1=size/2`, `attr2=frameCount`, `attr3=2`.

### 8.3. Матрица зависимостей при редактировании

| Операция | Какие ресурсы обновлять |
|---|---|
| Смещение/деформация вершин | `Res3`, при необходимости `Res4`, bounds в `Res2` |
| Изменение UV | `Res5` (и опционально `Res15`) |
| Изменение topology (индексы/треугольники) | `Res6`, `Res13`, `Res7`, диапазоны `Res2.slot` |
| Изменение LOD/group назначения | `Res1.slotIndex`, возможно `Res2.slot` |
| Изменение имени узла | `Res10` |
| Изменение иерархии/анимации узлов | `Res1.hdr1/hdr2/hdr3`, `Res8`, `Res19` |
| Добавление/удаление slot | `Res2`, ссылки из `Res1`, диапазоны batch/tri |

### 8.4. Детерминированная сериализация

- little-endian для всех чисел;
- без внутреннего padding в таблицах ресурсов;
- выравнивание блоков ресурсов в NRes по 8 байт (через контейнер).

---

## 9. Рекомендованный canonical IR для toolchain

Минимальный IR для безопасного round-trip:

```c
struct ModelCoreIR {
    // raw payloads for unknown/passthrough types
    map<uint32_t, RawResource> raw_passthrough;

    vector<Node> nodes;          // Res1 decoded (hdr + matrix)
    Header140 header;            // Res2[0x00..0x8B]
    vector<Slot> slots;          // Res2 slot table (включая unk tail)

    vector<float3> positions;    // Res3
    vector<PackedNormal4> normals_raw; // Res4 raw + optional decoded cache
    vector<PackedUV4> uv0_raw;   // Res5 raw + optional decoded cache

    vector<uint16_t> indices;    // Res6
    vector<TriDesc16> tri;       // Res7
    vector<Batch20> batches;     // Res13
    vector<optional<string>> node_names; // Res10

    optional<vector<uint8_t>> res15_raw;
    optional<vector<uint8_t>> res16_raw;
    optional<vector<uint32_t>> colors_raw; // Res18
    optional<RawResource> res20_raw;

    // animation bridge
    optional<vector<AnimKey24>> anim_keys;    // Res8
    optional<vector<uint16_t>> anim_map_words; // Res19
    uint32_t anim_frame_count;
};
```

Принцип: где семантика неполная, хранить raw и переизлучать байт-в-байт.

---

## 10. Практика конвертации

### 10.1. MSH -> OBJ/GLTF

- `Res3` напрямую в позиции;
- `Res6 + Res13` в faces;
- нормали/UV декодировать через коэффициенты `1/127`, `1/1024`;
- при экспорте по LOD/group использовать `Res1` матрицу слотов, а не "все batch подряд" (если нужен runtime-эквивалент);
- пометить ограничения: core не содержит классический weight-скиннинг.

### 10.2. Обратный импорт (OBJ/GLTF -> MSH)

Для 1:1 ожидаемого поведения импортёр должен:

- строить корректные `Res13` диапазоны,
- строить/обновлять `Res2.slot` ranges и bounds,
- поддерживать quantization при упаковке (`Res4/Res5`),
- сохранять unknown-поля таблиц, если вход был редактированием существующей модели.

---

## 11. Наблюдения по corpus (testdata/nres)

Сводка по 435 MSH-моделям:

- валидны все 435/435 по `tools/msh_doc_validator.py`;
- основной порядок типов:
  - `414`: `(1,2,3,4,5,15,13,6,7,8,19,9,10,17)`
  - `21`: `(1,2,3,4,5,18,15,13,6,7,8,19,9,10,17,20)`
- `Res1.attr3`: `38` в 434 моделях, `24` в 1 модели;
- `Res18` и `Res20` встречаются в 21 модели;
- `Res16` в данном corpus не встретился;
- `Res8/Res19` присутствуют во всех моделях, но `Res19.attr2=1` часто соответствует статике.

---

## 12. Открытые вопросы (не блокируют 1:1)

- точная доменная семантика `Node.hdr0` битов;
- полные имена/назначения `Batch20.unk4/unk6/unk14`;
- назначение `Slot68.unk30..unk40`;
- полная семантика `Res15/Res16/Res18/Res20` payload beyond stride-level;
- точная семантика 4-го байта в `PackedNormal4`.

Для runtime/reader/writer это не критично при условии byte-preserving policy.

---

## 13. Чеклист реализации 1:1

### 13.1. Engine runtime

- реализован loader-порядок как в `sub_10015FD0`;
- slot lookup по формуле `4 + node*19 + lod*5 + group`;
- декодирование `Res4` через `/127.0` с clamp;
- декодирование `Res5` через `/1024.0`;
- tri селекторы `selPacked` трактуются как 2-битные с `3 -> 0xFFFF`;
- корректная обработка `0xFFFF` sentinel во всех таблицах.

### 13.2. Reader/validator

- строгая проверка stride/размеров/диапазонов;
- OOB-защита всех индексных доступов;
- поддержка both direct-model и nested `.msh` payload.

### 13.3. Writer/editor

- стабильный пересчёт `attr1/attr2/attr3`;
- сохранение unknown fields и unknown resource types;
- детерминированная сериализация NRes (8-byte align);
- regression-проверка round-trip: `decode -> encode -> decode` без расхождений структуры/диапазонов.

