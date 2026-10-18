# WEAR и MAT0

MSH batch хранит только `material_index`. WEAR переводит этот индекс в имя
материала, а MAT0 по этому имени описывает phases, parameters и texture
references.

```text
Batch20.material_index
  -> WEAR row
  -> MAT0 entry
  -> active phase
  -> textureName
```

## WEAR

WEAR -- текстовый ресурс type ID `0x52414557`, обычно `*.wea` рядом с моделью.

```text
<wearCount>
<legacyId> <materialName>
...

[empty line]
[LIGHTMAPS
<lightmapCount>
<legacyId> <lightmapName>
...]
```

`legacyId` сохраняется, но выбор выполняется по позиции строки и имени. Между
основной таблицей и `LIGHTMAPS` нужен пустой разделитель.

## MAT0

MAT0 имеет type ID `0x3054414D`, обычно расположен в `Material.lib`. `attr1`
содержит runtime flags, `attr2` -- версию payload.

```c
#pragma pack(push, 1)
struct Mat0Header {
    uint16_t phase_count;
    uint16_t animation_block_count;
};

struct Phase34 {
    uint8_t parameters[18];
    char texture_name[16];
};
#pragma pack(pop)
```

`attr1` читается из NRes entry metadata и сохраняется вместе с разрешённым
материалом. Это runtime flags материала; его нельзя путать с `attr2`, который
задаёт версию payload. Versioned prefix bytes читаются только если версия их
содержит, а raw values сохраняются для последующих слоёв.

### Pipeline category

Call site извлекает native category как `(attr1 >> 2) & 0xF` и сохраняет её на
разрешённом phase material. World draw ranges получают состояние для начального
selector при подготовке, а animated selector обновляет category вместе с
material selector. Fixed environment ranges keep their authored pipeline state.
Таблица содержит пять подтверждённых категорий; helper возвращает `None` для
остальных значений и не меняет уже выбранные depth и cull state.

| category | source/destination colour factors | alpha test |
| ---: | --- | --- |
| 0 | `ONE / ZERO`, blending disabled | disabled |
| 1 | `SRC_ALPHA / INV_SRC_ALPHA` | enabled, `GREATER_EQUAL` |
| 2 | `SRC_ALPHA / ONE` | enabled, `GREATER_EQUAL` |
| 3 | `ZERO / SRC_COLOR` | enabled, `GREATER_EQUAL` |
| 4 | `DEST_COLOR / SRC_COLOR` | enabled, `GREATER_EQUAL` |

The mapping comes from the native five render-state pairs selected by the
material category. Alpha reference remains draw-range data, while the Vulkan
pipeline key includes the blend mode, depth mode, cull mode and alpha-test
variant so ranges with different native state do not share a pipeline.

World transparency is a separate draw-queue decision. A world range joins that
queue when sampled diffuse opacity is below one or any bit in the native
`0x108` batch-flag mask is set; the MAT0 category alone does not make it
transparent. A queued range
with a depth attachment tests depth without writing it. Unsupported categories
fail world-range setup instead of silently falling back to opaque state.

Каждая phase занимает 34 байта: 18 parameter bytes и 16-byte C string имени
текстуры. Loader переводит параметры в коэффициенты так:

| bytes | runtime value |
| --- | --- |
| `p0..p2` | additive RGB, `p / 255` |
| `p3` | opacity, `p * 0.01` |
| `p4..p7` | directional values, `p / 255` |
| `p8..p11` | specular values, `p / 255` |
| `p12..p15` | extra values, `p / 255` |
| `p16` | integer power |
| `p17` | signed TEXM page index |

Texture name и page index выбираются из текущей phase. Они не смешиваются с
соседней phase.

## Animation

После phase table идут `animation_block_count` плотных блоков. Каждый блок имеет
`u32 header_raw`, `u16 key_count`, затем `key_count` записей по три `u16`:

```text
u32 header_raw
u16 key_count
repeat key_count:
    u16 phase_index
    u16 end_time_ms
    u16 raw_k2
```

`header_raw & 7` задаёт режим: `0` loop, `1` ping-pong, `2` clamp, `3`
random-per-query. Значения `4..7` сохраняются как unknown. `header_raw >> 3`
является interpolation mask: bits `1`, `2`, `4`, `8` смешивают группы
`p0..p2`, `p4..p6`, `p8..p10`, `p12..p14`, а bit `16` смешивает `p3`.
Компоненты `p7`, `p11`, `p15`, `p16` и `p17` копируются из текущей phase.

Время для блока вычисляется как `clock_ms - wear_row_start_ms` с wrapping
`u32` subtraction. Для loop берётся остаток от последнего `end_time_ms`, для
ping-pong нечётный цикл идёт в обратном направлении, clamp после duration
выбирает последнюю phase, а random-per-query использует переданное случайное
значение по модулю duration. Ключ выбирается по интервалу
`previous_end <= local_time < end_time`; следующий ключ циклически замыкается на
первый. На ping-pong turnaround и clamp после duration native routine проходит
общий float interpolation path, включая unsigned subtraction endpoint arithmetic;
это может дать extrapolated coefficients при ненулевой mask. Переданный в
sampler `animation_block_index` уже выбран вызывающим runtime из block table;
packed WEAR handle и номер строки WEAR являются отдельными значениями.

Для native effect path с явной normalized phase режим блока не вычисляется:
конечное значение вне `0..=1` заменяется на `0.5`, затем phase умножается на
последний `end_time_ms` и усекается до целого tick. Поиск интервала идёт от
последнего ключа к первому. При phase `1.0` tick равен duration, поэтому ни один
интервал не подходит: выбирается key 0 и общий interpolation path может
экстраполировать за пределы его первого интервала.

## Fallback

Material resolve:

1. имя из WEAR;
2. `DEFAULT`;
3. entry с индексом 0.

Пустое texture name означает намеренно нетекстурированную поверхность. Lightmap
fallback отдельный: отсутствующий lightmap даёт slot `-1`.
