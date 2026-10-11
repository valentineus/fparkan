# MSH

Файл `*.msh` является NRes-контейнером. Geometry, узлы, slots, batches,
animation и служебные streams лежат в entries с разными `type_id`.

## Entry map

```text
type 1   nodes and slot selection
type 2   header 0x8C + Slot68 records
type 3   positions float3
type 4   packed normals
type 5   packed UV0
type 6   index buffer u16
type 7   triangle descriptors
type 8   animation keys
type 9   service stream
type 10  strings and node names
type 13  Batch20 records
type 15  auxiliary stream
type 17  auxiliary data
type 18  packed UV1 (optional secondary coordinates)
type 19  animation frame map
type 20  rare auxiliary table
```

Reader ищет entries по type, но сохраняет исходный порядок для roundtrip.

## Node and slot selection

Type 1 обычно состоит из records по 38 bytes:

```c
struct Node38 {
    uint16_t hdr0;
    uint16_t parent_or_link;
    uint16_t anim_map_start;
    uint16_t fallback_key;
    uint16_t slot_index[15];
};
```

В исходном layout это 3 состояния по 5 LOD: `slot_index[state * 5 + lod]`.
Публичные Rust-обёртки пока сохраняют исторические имена `Lod` и `Group`,
поэтому в `selected_slot` аргумент `Lod` задаёт native state (0..2), а
`Group` задаёт native LOD (0..4). `0xFFFF` означает отсутствие геометрии для
комбинации state/LOD.

Validated `ModelAsset` сохраняет decoded type 8 keys и type 19 map как
`ModelAnimation`. `node38_fallback_pose` возвращает pose по `fallback_key`,
а `node38_fallback_hierarchy` собирает parent-before-child hierarchy.
`node38_sampled_hierarchy` использует type 19 map в пределах объявленного
числа кадров и интерполирует соседние type 8 keys; при отсутствии usable map
или выходе за frame count используется fallback key. Для покомпонентного
sampling `node38_sampled_hierarchy_at_times` принимает отдельный optional float
time для каждого узла: `Some(time)` выбирает type 19 frame по округлению
`time - 0.5` к ближайшему целому с ties-to-even, затем интерполирует выбранный
type 8 key и его следующий key в исходный float time; `None` использует
fallback key. Значения должны быть конечными и неотрицательными. Это
portable-поведение при default-nearest rounding; полного совпадения со всеми
режимами округления x87 оно не заявляет. `parent_or_link == 0xFFFF`
означает root, иначе это parent index, который должен быть меньше индекса
child. Модель с нарушенным порядком не получает придуманную hierarchy.

Type 8 записывает quaternion в порядке WXYZ, после decode `AnimKey24` хранит
его как XYZW. Native AniMesh `+0x12560` строит local matrix через NGI32
`g_FastProc + 0x38`; scalar implementation выдаёт transpose обычной active
`Pose` basis (`M01=2(xy+zw)`, `M02=2(xz-yw)`, `M10=2(xy-zw)`). Поэтому путь
Node38 MSH сопрягает source-local quaternion ровно один раз после sampling и
до hierarchy composition (меняет знаки компонент `x`, `y`, `z`). Для fallback
poses применяется та же граница. Общий animation decoder и другие форматы
анимации не изменяются.

В legacy-camera static preview без `--static-animation-frame <u16>` используются
сохранённые начальные значения из явной ссылки каждого prototype на `.ctl`.
Узлы без control binding получают time `0`. Для каждой component instance
обрабатываются её CTLD bindings, поэтому общий deduplicated MSH не смешивает
controls разных компонентов. Строки идут в source order, и последняя включённая
строка для одного узла заменяет предыдущую. Flag `0x01` выполняет один wrap
значения blend на единицу, иначе значение ограничивается `[0, 1]`; flag `0x02`
инвертирует результат (`1 - blend`), а строки с `0x04` пропускаются. Выбранный
float time равен `(1 - blend) * frame_a + blend * frame_b`. Если он выходит за
type 19 map, MSH sampler использует fallback key; если после одной wrap/clamp и
invert получается отрицательное или нечисловое время, preview завершится
понятной ошибкой. Явный `--static-animation-frame` остаётся global override
для всех компонентов. Parent pose поворачивает child translation, затем
translation суммируется, а rotations умножаются; после полученной global pose
применяется `Rz * Ry * Rx`, scale и mission translation. Геометрия намеренно
дублируется на draw-range узла, потому что один source vertex может быть
нарисован разными node poses. Это только начальная статическая pose: она не
запускает игровой Control/AI и не обещает полного совпадения с runtime или
всеми x87 rounding modes.

## Vertex streams

Основные vertex streams имеют фиксированный source index. У type 3 `attr1`
задаёт количество вершин, а `attr3` — размер записи в байтах. Type 4
содержит четыре signed bytes нормали. Type 5 и optional type 18 содержат по
два little-endian `uint16` на вершину:

```c
struct PackedUv16x2 {
    uint16_t u;
    uint16_t v;
};
```

Type 5 — primary UV0, type 18 — secondary UV1 для lightmap/detail stage.
Для type 18 loader требует `attr3 == 4`, совпадение `attr1` и длины payload,
а также одинаковое число записей с type 3. Обе пары декодируются как
`packed / 1024.0`; renderer обращается к UV1 по тому же `source_index`, что и
к UV0. Отсутствующий type 18 означает, что secondary stage не имеет authored
координат.

## Slot and batch

Type 2 содержит header `0x8C`, затем `Slot68`:

```c
struct Slot68 {
    uint16_t tri_start;
    uint16_t tri_count;
    uint16_t batch_start;
    uint16_t batch_count;
    float aabb_min[3];
    float aabb_max[3];
    float sphere_center[3];
    float sphere_radius;
    uint32_t opaque[5];
};
```

Type 13 задаёт draw ranges:

```c
#pragma pack(push, 1)
struct Batch20 {
    uint16_t batch_flags;
    uint16_t material_index_hi; // +0x02, native high selector/slot field
    uint8_t  material_index;    // +0x04, WEAR material selector
    uint8_t  lightmap_index;    // +0x05, 0xFF means no lightmap
    uint16_t local_batch_index; // +0x06
    uint16_t index_count;
    uint32_t index_start;
    uint16_t vertex_count;      // +0x0E
    uint32_t base_vertex;
};
#pragma pack(pop)
```

Byte `material_index` выбирает строку WEAR; high word at `+0x02` сохраняется
отдельно для native overrides. Loader проверяет, что
`index_start + index_count` остаётся внутри type 6, а каждый использованный
индекс через `base_vertex` попадает в position stream. `vertex_count` и
`local_batch_index` доступны renderer для исходного draw contract.
