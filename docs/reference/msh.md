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
type 18  rare stream
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

`slot_index[lod * 5 + group]` выбирает geometry slot. `0xFFFF` означает
отсутствие геометрии для комбинации LOD/group.

Validated `ModelAsset` также сохраняет decoded type 8 keys и type 19 map как
`ModelAnimation`. `node38_fallback_pose` возвращает pose по `fallback_key`,
то есть доказанный static input. `parent_or_link == 0xFFFF` означает root;
иначе это parent index, обязательно меньший индекса child. Этот контракт
подтверждён на licensed animation gates обеих частей и защищён fallback-ом:
модель с нарушенным порядком не получает придуманную hierarchy.

В legacy-camera static preview стандартный узел уже получает свой fallback pose
до внешнего TMA/Iron3D transform. Parent pose поворачивает child translation,
затем translation суммируется, а rotations умножаются; после полученной global
pose применяется `Rz * Ry * Rx`, scale и mission translation. Геометрия
намеренно дублируется на draw-range узла, потому что один source vertex может
быть нарисован разными node poses. Это static fallback hierarchy, а не полная
animation parity: dynamic type-19 frame-map sampling остаётся отдельной задачей.

Для воспроизводимого исследования `fparkan-cli model inspect --root <game>
--archive <archive> --resource <model.msh>` выводит Node38 metadata, включая
parent index, fallback key и наличие LOD0/group0 geometry.

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
    uint16_t material_index;
    uint16_t opaque4;
    uint16_t opaque6;
    uint16_t index_count;
    uint32_t index_start;
    uint16_t opaque14;
    uint32_t base_vertex;
};
#pragma pack(pop)
```

Index check выполняется как `base_vertex + index < vertex_count` для всего
используемого slice.
