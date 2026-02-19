# MSH core

`MSH core` описывает геометрию, слоты, батчи и базовые таблицы модели.  
Документ покрывает контракт, необходимый для 1:1 воспроизведения рендера и коллизии.

Связанные страницы:

- [MSH animation](msh-animation.md)
- [Material](material.md)
- [Texture (Texm)](texture.md)
- [Render pipeline](render.md)
- [NRes](nres.md)
- [RsLi](rsli.md)

## 1. Общая модель

MSH-модель хранится как `NRes`-контейнер.  
Связь таблиц строится по `type`, а не по порядку записей.

Базовый путь геометрии:

1. `Res1` выбирает slot по `(node, lod, group)`.
2. `Res2.slot` задаёт диапазоны треугольников и батчей.
3. `Res13` задаёт диапазон индексов и `baseVertex`.
4. `Res6` даёт `uint16` индексы.
5. `Res3/Res4/Res5` дают вершины, нормали и UV.

## 2. Карта core-ресурсов

| Type | Ресурс | Обязательность | Stride / layout |
|---:|---|---|---|
| 1 | Node table | обязательный | обычно 38 байт |
| 2 | Header + slots | обязательный | `0x8C + n*68` |
| 3 | Positions | обязательный | 12 |
| 4 | Packed normals | обычно обязательный | 4 |
| 5 | Packed UV0 | обычно обязательный | 4 |
| 6 | Index buffer | обязательный | 2 |
| 7 | Tri descriptors | для коллизии/пикинга | 16 |
| 8 | Anim key pool | для анимированных | 24 |
| 10 | Node strings | опциональный | variable |
| 13 | Batch table | обязательный | 20 |
| 15 | Доп. stream | опциональный | 8 |
| 16 | Доп. stream | опциональный | 8 |
| 18 | Доп. stream | опциональный | 4 |
| 19 | Anim map | для анимированных | 2 |
| 20 | Доп. таблица | опциональный | variable |

## 3. Основные структуры

### 3.1. `Res1` (узлы)

```c
struct Node38 {
    uint16_t hdr0;
    uint16_t parent_or_link;
    uint16_t anim_map_start;
    uint16_t fallback_key;
    uint16_t slotIndex[15]; // lod0:g0..g4, lod1:g0..g4, lod2:g0..g4
};
```

Формула slot-выбора:

```c
slot = node.slotIndex[lod * 5 + group]
```

`0xFFFF` означает отсутствие слота.

### 3.2. `Res2` (header + slot records)

```c
struct Slot68 {
    uint16_t triStart;
    uint16_t triCount;
    uint16_t batchStart;
    uint16_t batchCount;
    float    aabbMin[3];
    float    aabbMax[3];
    float    sphereCenter[3];
    float    sphereRadius;
    uint32_t opaque[5];
};
```

`opaque[5]` должны сохраняться 1:1.

### 3.3. `Res3`, `Res4`, `Res5`, `Res6`

- `Res3`: `float3` позиции (`stride=12`)
- `Res4`: `int8[4]` packed normal (`stride=4`)
- `Res5`: `int16[2]` UV (`stride=4`)
- `Res6`: `uint16` индексы (`stride=2`)

Декодирование:

- normal = `clamp(n / 127.0, -1..1)`
- uv = `packed / 1024.0`

### 3.4. `Res7` и `Res13`

```c
struct TriDesc16 {
    uint16_t triFlags;
    uint16_t link0;
    uint16_t link1;
    uint16_t link2;
    int16_t  nx;
    int16_t  ny;
    int16_t  nz;
    uint16_t selPacked;
};

struct Batch20 {
    uint16_t batchFlags;
    uint16_t materialIndex;
    uint16_t opaque4;
    uint16_t opaque6;
    uint16_t indexCount;
    uint32_t indexStart;
    uint16_t opaque14;
    uint32_t baseVertex;
};
```

`selPacked` хранит 3 селектора по 2 бита; значение `3` трактуется как `0xFFFF`.

## 4. Runtime-обход модели

Псевдокод рендера:

```c
for each node:
    slot = resolve_slot(node, lod, group)
    if slot == none: continue

    if culled(slot.bounds, node_transform): continue

    for b in slot.batchRange:
        batch = batches[b]
        bind_material(batch.materialIndex)

        draw_indexed(
            baseVertex = batch.baseVertex,
            indexStart = batch.indexStart,
            indexCount = batch.indexCount
        )
```

## 5. Критические инварианты

Обязательно проверять:

- `Res2.size >= 0x8C`
- `(Res2.size - 0x8C) % 68 == 0`
- `batchStart + batchCount` не выходит за `Res13`
- `triStart + triCount` не выходит за `Res7`
- `indexStart + indexCount` не выходит за `Res6`
- `baseVertex + max(indexSlice) < vertexCount`
- `slotIndex == 0xFFFF` или `< slotCount`

## 6. Важные edge-cases

- Встречается редкий вариант `Res1.attr3 = 24`; для существующих ассетов нужен copy-through.
- Для строгого writer лучше генерировать `Res1` в основном формате `38` байт/узел.
- Неизвестные поля таблиц нельзя нормализовать или обнулять.

## 7. Правила для writer/editor

1. Сохранять неизвестные поля и неизвестные `type`-ресурсы.
2. Пересчитывать только явно вычислимые атрибуты (`attr1/attr3` и size-зависимые поля).
3. Не менять порядок/контент opaque-данных без явной цели.
4. Сериализовать little-endian, без внутреннего padding.

## 8. Статус валидации

- Инварианты формата реализованы в `tools/msh_doc_validator.py`.
- На полном retail-корпусе `testdata/Parkan - Iron Strategy` проверено `435/435` MSH-моделей без структурных ошибок.

## 9. Статус покрытия и что осталось до 100%

Закрыто:

1. Базовые таблицы geometry path (`Res1/2/3/4/5/6/7/13`).
2. Критичные range-инварианты slot/batch/index.
3. Правила совместимого writer/editor для lossless работы с существующими ассетами.

Осталось:

1. Полная семантика части opaque-полей (`Slot68` tail, `Batch20` opaque-поля) для authoring без copy-through.
2. Полная формализация редких веток (`Res1.attr3 != 38`) на расширенном корпусе.
3. End-to-end writer для генерации новых игровых MSH с подтвержденным runtime-паритетом.

