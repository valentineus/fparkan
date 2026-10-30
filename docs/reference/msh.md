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
type 16  optional vertex-colour data
type 17  auxiliary data
type 18  packed UV1 (optional secondary coordinates)
type 19  animation frame map
type 20  rare auxiliary table
```

Reader ищет entries по type, но сохраняет исходный порядок для roundtrip.
Поле `ModelAsset.type16_present` отмечает только наличие entry типа 16. Данные
payload сохраняются, но этот путь рендера пока не преобразует их в цвета
вершин.

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

## Type 7 и сегментный запрос

Type 7 необязателен для обычного MSH preview. Если stream есть, каждые 16 bytes
описывают одну грань: `tri_flags` находится по `+0`, четыре неинтерпретируемых
слова — по `+2`, `+4`, `+6` и `+14`, а signed `normal[3]` — по `+8`, `+0xA` и
`+0xC`. `ModelAsset.triangle_descriptors` сохраняет декодированную таблицу как
`Option`; отсутствие Type 7 не делает статическую модель недействительной.

`query_slot_segment` использует Node38 header mask, Batch20 leading DWORD
`batch_flags | (material_index_hi << 16)` и `tri_flags`. Для каждого поля
include mask требует все установленные биты, exclude mask пропускает запись при
любом совпавшем бите. Descriptor cursor начинается с `Slot68.tri_start` и
сдвигается на `Batch20.index_count / 3` после каждой записи, даже если batch
или грань отфильтрованы. Индексы грани читаются из type 6 с учётом
`base_vertex`.

Правило восстановлено для GOG-бинарей `AniMesh.dll`
(`65659bf67f20e5be77d67a5b2f59968f61b94293143807fa59ca451c560c4f7e`) и
`Ngi32.dll` (`bab9840d94f4e4e74ffc26677724fa896cf4823845504d09a9e025f80016edf5`).
Callback `AniMesh.dll+0x11090` переводит signed type-7 normal множителем
`1 / 32767`; функция `AniMesh.dll+0x11EB0` записывает этот scale в глобальное
поле. Callback вызывает FastProc slot `+0xB4`: в этой сборке он указывает на
`Ngi32.dll+0x1D560`. После обычной загрузки DLL порог `Ngi32.dll+0x37224`
равен нулю, поэтому plane test принимает только переход от положительного
расстояния к неположительному (`dot(normal, end - start) < 0`, расстояние в
начале `> 0`, в конце `<= 0`). `Batch20.batch_flags & 0x0002` разрешает повторный
plane test в обратном направлении, только если прямой plane test не пересёк
плоскость. Пересечение проверяется export ordinal 202
(`Ngi32.dll+0x1E60`, `mrnPointInPoly`): проекция выбирает ось с наибольшим по
модулю normal component, а точка проходит грань, пока ни одно edge expression
не меньше нуля. Равенство нулю принимается, поэтому попадание на ребро или
вершину считается попаданием. Поздний кандидат с тем же squared distance
заменяет предыдущий; расстояние считается от исходного первого endpoint.

Native helper получает точку пересечения через `RCPSS` и один шаг уточнения;
Rust использует обычное деление `f32`, поэтому последние биты координат могут
отличаться. Игнорируемый тест с ресурсами проверяет исходную стену, дверной
проём и обратное направление на `fortif.rlb/fr_l_plant.msh`.

## Сторона грани в цветовом проходе

В цветном проходе `Terrain.dll`, функция `0x100296F0`, сравнивает камеру с
каждой гранью по знаку тройного произведения:

```c
d = dot(eye - v0, cross(v1 - v0, v2 - v0));
```

Проход оставляет грань только при `d > 0`. Raw `Batch20.flags` задаёт два
исключения: `0x0002` пропускает программную проверку стороны, поэтому обратная
грань не отбрасывается этим условием. С `0x4000` проверяется только первая
грань записи: если `d <= 0`, цикл её граней прекращается; если `d > 0`,
оставшиеся грани этой записи уже не проходят повторную программную проверку.
`Terrain.dll!0x100296F0` также настраивает пакет этого цветного пути: flags по
адресу `packet+0x128` равны `0x404` или `0x414`. Путь отправки в
`Terrain.dll!0x1001D660` перед вызовом renderer vtable `+0x48` очищает только
маску `0x1`; `Ngi32.dll!0x10007470` проверяет маску `0x4` этого аргумента. При
установленной маске render state 22 получает `D3DCULL_NONE`, иначе —
`D3DCULL_CW`. Оба значения CShade сохраняют маску `0x4`, поэтому в этом
цветном пути аппаратное отсечение выключено, а набор обычных граней определяет
программный тест `d > 0`. Raw-флаги `0x0002` и `0x4000` меняют этот тест, как
описано выше.

Статический Vulkan preview сохраняет каждую исходную запись `Batch20` одним
диапазоном рисования:
`append_static_preview_component_with_lightmaps` добавляет исходные индексы без
перестановки, а в диапазон переносит `index_count` и исходный `first_index` с
общим смещением компонента. Поэтому `0x4000` выбирает ту же первую грань, даже
если диапазон получил другой материал для lightmap.

Поскольку вершины preview переведены в мировые координаты, знак `d` для MSH
диапазона корректируется знаком детерминанта полной матрицы размещения. Повороты
узлов и mounts — единичные кватернионы с determinant `+1`; отрицательный object
scale меняет знак. Renderer Vulkan задаёт `front_face=CLOCKWISE` и viewport с
положительной высотой. Для используемых матриц legacy и free-flight preview
при положительном determinant native-visible `d > 0` относится к Vulkan
back-face; поэтому preview отсекает `FrontFace`. При отрицательном determinant
режим меняется на `BackFace`.
Для `0x0002` и `0x4000` аппаратное отсечение выключено, чтобы сохранить их
программную политику. Отклонённый `0x4000` диапазон получает число индексов 0;
каждый кадр это число заново вычисляется из исходного значения, поэтому после
изменения положения камеры или opacity материала диапазон может вернуться в
draw queue. Проверка первой грани
применяется только к MSH-диапазонам цветовой мировой очереди; terrain и
shadow/sun запросы её не используют.
