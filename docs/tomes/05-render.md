# V. Геометрия, материалы и рендер

Этот том описывает путь от загруженного игрового состояния до pixels в back
buffer. Renderer не решает игровые правила: он получает transforms, geometry,
материалы, свет, эффекты, камеру и список видимых объектов, затем превращает
их в упорядоченный набор draw calls и fixed-function states.

Графический pipeline FParkan держится на нескольких слоях данных:

```text
MSH node/slot/batch
  -> Batch20.material_index
  -> строка WEAR
  -> имя MAT0
  -> активная phase
  -> textureName и lightmap slot
  -> Texm payload
  -> LegacyRenderState
  -> draw item кадра
```

Важное практическое правило: форматы ресурсов, runtime-состояние renderer-а и
современный backend являются разными уровнями. Файл можно прочитать правильно и
всё равно получить неверный кадр из-за другой сортировки, другого mip-skip,
другой ветки material fallback или другого округления animation time.

## Контур рендера

Изображение является последней стадией длинного цикла. До renderer-а уже
накоплен ввод, рассчитан simulation step, применены отложенные операции,
обновлены animation states, выбрана camera и выставлен listener для 3D sound.

```text
system messages and input
  -> simulation calculation
  -> deferred object operations
  -> animation and transforms
  -> camera and sound listener
  -> visibility and render queues
  -> materials and draw passes
  -> renderer completion
  -> end-of-render callbacks and UI
```

CPU делает отбор объектов, сэмплирует animation, собирает matrices, выбирает
LOD/slot, группирует batches и готовит состояния. Графический pipeline
преобразует вершины из model space в screen space, rasterizes triangles,
проверяет depth, применяет texture stages, lighting, alpha test/blend и пишет
pixels.

Координатный путь вершины:

```text
local/model space
  -> world space
  -> view/camera space
  -> clip space
  -> normalized device coordinates
  -> viewport pixels
```

Порядок умножения матриц и соглашение о layout должны быть едины во всём
движке. Ошибка транспонирования часто выглядит как сломанная анимация, хотя
ключи модели прочитаны верно.

## Граница Ngi32

`Ngi32.dll` является платформенной границей Iron3D-era renderer-а. Она создаёт
графический и звуковой interfaces, перечисляет устройства, хранит capability
profile, предоставляет память, часы и быстрые математические процедуры.
Высокоуровневые DLL должны обращаться к interface Ngi32, а не напрямую к
конкретному DirectDraw/Direct3D device.

`iron_3d.ini` задаёт выбранный `CURRENT_D3DCARD`. Display layer перечисляет
drivers и video modes, проверяет поддержку 3D, переводит native capabilities во
внутренний профиль и создаёт render object. `niCreate3DRender` принимает
выбранный driver/mode, window handle и flags владения, динамически получает
функции DirectDraw/Direct3D семейства 5-7 и публикует refcounted renderer.
`niGet3DRender` возвращает уже созданный объект и увеличивает число владельцев.

```text
enumerate adapters and video modes
  -> choose CURRENT_D3DCARD
  -> translate native capabilities
  -> create DirectDraw surfaces and 3D interface
  -> construct engine renderer
  -> publish global refcounted pointer
```

Старый API работает как state machine. Перед draw подсистема terrain/shade
выбирает matrices, texture stages, filtering, depth test/write, culling, alpha
test, blending и vertex format. Современный backend может собрать это в
immutable pipeline key и реализовать через shaders, но compatibility layer
должен видеть исходную fixed-function модель.

```c
struct LegacyRenderState {
    Mat4 world, view, projection;
    TextureStage stages[2];
    BlendMode blend;
    DepthMode depth;
    CullMode cull;
    bool alpha_test;
    uint8_t alpha_ref;
    VertexFormat vertex_format;
};
```

Эта структура является переносимой моделью наблюдаемого контракта, а не
утверждением о точном layout оригинального объекта renderer-а.

Отдельная часть ABI -- таблица `g_FastProc`. При запуске выбираются scalar,
MMX, Katmai/SSE, 3DNow или PPro-реализации процедур, а `niGetProcAddress(index)`
возвращает pointer из изменяемой таблицы. Номер slot является частью ABI:
signature менять нельзя. Различия scalar/SIMD округления способны менять
animation sampling, culling, particles и даже gameplay-adjacent decisions.

## MSH как граф модели

`*.msh` является nested NRes, а не одной монолитной структурой. Geometry,
nodes, slots, batches, animation и служебные streams лежат в отдельных entries
и связываются по `type_id`. Физический порядок entries сохраняется для
roundtrip, но reader не должен выводить из него смысловую связь.

Карта основных entries:

```text
type 1   узлы и выбор slot, обычно stride 38
type 2   header 0x8C + slots по 68 байт
type 3   positions float3, stride 12
type 4   packed normals, stride 4
type 5   packed UV0, stride 4
type 6   index buffer, u16
type 7   triangle descriptors, stride 16
type 8   animation keys, stride 24
type 9   служебный поток модели
type 10  строки и имена узлов
type 13  draw batches, stride 20
type 15  дополнительный поток, stride 8
type 17  вспомогательные данные
type 18  редкий поток, stride 4
type 19  animation frame map, u16
type 20  редкая вспомогательная таблица
```

Базовый набор types стабилен для проверенных моделей Частей 1 и 2. Расширенный
вариант добавляет types 18 и 20. Редкий вариант `MTCHECK.MSH` имеет
альтернативный атрибут type 1; его payload нужно поддерживать copy-through до
закрытия layout.

### Узлы и slots

Type 1 обычно состоит из записей по 38 байт:

```c
struct Node38 {
    uint16_t hdr0;
    uint16_t parent_or_link;
    uint16_t anim_map_start;
    uint16_t fallback_key;
    uint16_t slot_index[15];
};
```

`slot_index` образует матрицу `3 LOD x 5 groups`. Выбор выполняется как
`slot_index[lod * 5 + group]`; `0xFFFF` означает отсутствие geometry для этой
комбинации. Поле `parent_or_link` участвует в иерархии или связи узлов, но
название остаётся описательным.

Type 2 начинается с header `0x8C`, затем содержит slots по 68 байт:

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

Slot связывает диапазон triangle descriptors, диапазон draw batches, AABB и
sphere bounds. AABB удобен для более точных осевых тестов, sphere -- для
быстрого отбрасывания. Последние пять слов сохраняются без интерпретации.

Обязательные проверки:

- `type 2` имеет размер не меньше `0x8C`;
- остаток после header кратен 68;
- каждый `slot_index` либо `0xFFFF`, либо меньше числа slots;
- `tri_start + tri_count` не выходит за type 7;
- `batch_start + batch_count` не выходит за type 13.

### Vertex streams, triangles и batches

Основные vertex streams:

```text
type 3: position = три float32
type 4: normal   = четыре int8
type 5: UV0      = два int16
type 6: index    = uint16
```

Normal XYZ декодируется как signed component / `127.0` с clamp в `[-1, 1]`.
Четвёртый byte normal stream не отбрасывается при roundtrip. UV декодируется
как `packed / 1024.0`. Index buffer адресует вершины относительно `base_vertex`
batch-а, поэтому проверка допустимости всегда использует
`base_vertex + index < vertex_count`.

Type 7 хранит descriptors triangles:

```c
struct TriDesc16 {
    uint16_t tri_flags;
    uint16_t link0;
    uint16_t link1;
    uint16_t link2;
    int16_t  nx;
    int16_t  ny;
    int16_t  nz;
    uint16_t sel_packed;
};
```

Descriptors используются коллизией, выбором и связями triangles. `sel_packed`
содержит три двухбитовых selector-а; значение `3` преобразуется в отсутствие
ссылки (`0xFFFF`). Полная семантика links и flags не закрывается одним layout.

Type 13 задаёт draw ranges:

```c
#pragma pack(push, 1)
struct Batch20 {
    uint16_t batch_flags;    // +0x00
    uint16_t material_index; // +0x02
    uint16_t opaque4;        // +0x04
    uint16_t opaque6;        // +0x06
    uint16_t index_count;    // +0x08
    uint32_t index_start;    // +0x0A
    uint16_t opaque14;       // +0x0E
    uint32_t base_vertex;    // +0x10
};
#pragma pack(pop)
static_assert(sizeof(Batch20) == 20);
```

`material_index` выбирает строку WEAR. `index_start`, `index_count` и
`base_vertex` описывают один indexed draw. Неизвестные поля могут влиять на
редкие проходы или state grouping, поэтому writer сохраняет их 1:1.

Типовой обход модели:

```c
for (Node& node : model.nodes) {
    Matrix node_world = parent_world * local_transform(node);
    uint16_t sid = node.slot_index[lod * 5 + group];
    if (sid == 0xFFFF) continue;

    Slot& slot = model.slots[sid];
    if (camera.culls(transform(slot.bounds, node_world))) continue;

    for (uint32_t i = 0; i < slot.batch_count; ++i) {
        Batch& b = model.batches[slot.batch_start + i];
        bind_wear_material(b.material_index);
        draw_indexed(b.base_vertex, b.index_start, b.index_count);
    }
}
```

В реальном кадре между culling и draw добавляются material resolve, lightmap,
render queues и сортировка, но связи данных остаются такими.

## Иерархия и анимация

Анимация MSH меняет локальный transform узлов. Geometry streams не изменяются:
для каждого узла на кадр строится matrix из position и quaternion. Дочерний
узел наследует transform родителя, поэтому изменение корпуса переносит башню,
точки крепления и все связанные slots.

Связка состоит из:

- type 8: пул animation keys;
- type 19: карта кадров;
- `anim_map_start` и `fallback_key` в `Node38`;
- parent links, задающих порядок умножения matrices.

Ключ type 8 занимает 24 байта:

```c
struct AnimKey24 {
    float position[3];
    float time;
    int16_t qx;
    int16_t qy;
    int16_t qz;
    int16_t qw;
};
```

Quaternion components декодируются как signed value / `32767.0`. На диске
порядок полей XYZ-W, но runtime math использует логическое `[w, x, y, z]`.
Безусловная современная нормализация после чтения не добавляется без parity
проверки: она может изменить крайние кадры.

Type 19 является массивом `uint16_t`; его `attr2` задаёт общее число кадров
timeline. Для конкретного узла `anim_map_start` указывает на блок длиной
`frame_count` либо равен `0xFFFF`.

Выбор ключа:

1. вычислить frame index из времени;
2. если frame вне диапазона, взять `fallback_key`;
3. если `anim_map_start == 0xFFFF`, взять `fallback_key`;
4. иначе прочитать `map_words[anim_map_start + frame]`;
5. если значение не меньше `fallback_key`, снова использовать fallback;
6. иначе использовать mapped key и следующий key для interpolation.

Fallback возвращается без interpolation. Это защищает статические узлы и конец
track-а.

Для времени между двумя keys:

```text
alpha    = (t - k0.time) / (k1.time - k0.time)
position = lerp(k0.position, k1.position, alpha)
rotation = shortest-path quaternion blend
```

Перед quaternion blend проверяется dot product. Если стороны находятся в
противоположных полусферах, знак второй стороны меняется, чтобы пройти по
короткому пути. При точном совпадении времени возвращается соответствующий key
без вычисления alpha.

Объект может переходить между двумя animation states. Тогда для каждого узла
сэмплируются позы A и B, затем position смешивается линейно, а quaternion --
через shortest-path blend. Если одна сторона невалидна, используется другая.

```c
Pose sample_node(Node n, float t);
Pose blend_pose(Pose a, Pose b, float weight);
Mat4 local = quaternion_matrix(pose.rotation);
local.set_translation(pose.position);
world[n] = world[parent(n)] * local;
```

Для parity особенно важны x87-compatible округление при выборе frame index и
порядок операций. Одинаковая формула на SSE может выбрать соседний кадр возле
границы.

Проверки animation data:

- размер type 8 кратен 24;
- размер type 19 кратен 2;
- каждый `fallback_key` меньше числа keys;
- блок карты узла полностью помещается в type 19;
- времена keys внутри track возрастают;
- parent links не образуют cycle;
- quaternion components читаются как signed 16-bit.

## WEAR и MAT0

MSH batch хранит только числовой `material_index`. WEAR переводит позиционный
slot в имя материала. MAT0 по этому имени описывает phases, parameters,
texture names и animation blocks. Такое разделение позволяет одной geometry
использовать разные appearances.

```text
Batch20.material_index
  -> строка WEAR
  -> имя MAT0
  -> активная phase
  -> textureName и render parameters
```

### WEAR

WEAR имеет type ID `0x52414557` и обычно хранится как `*.wea` рядом с моделью.
Формат текстовый:

```text
<wearCount>
<legacyId> <materialName>
... wearCount строк

[пустая строка]
[LIGHTMAPS
<lightmapCount>
<legacyId> <lightmapName>
... lightmapCount строк]
```

`legacyId` читается и сохраняется, но material выбирается по позиции строки и
имени. Пустая строка перед `LIGHTMAPS` является частью совместимого framing:
parser paths по-разному обрабатывают переход, и отсутствие разделителя ломает
совместимость. Material handle кодируется как `(table_index << 16) |
wear_index`; manager поддерживает ограниченное число wear tables.

Fallback material resolve строго разделён:

1. имя из WEAR;
2. `DEFAULT`;
3. entry 0;
4. для lightmap отсутствие означает slot `-1`, а не замену обычной texture.

Пустое имя texture внутри phase означает намеренно untextured surface.
Lightmap ищется в отдельном cache и не подменяется diffuse texture.

### MAT0

MAT0 имеет type ID `0x3054414D` и обычно находится в `Material.lib`. `attr1`
содержит runtime flags, `attr2` -- версию payload. Versioned metadata читается
cursor-ом: старые версии получают runtime defaults, но reader не пытается
насильно читать поля новой версии.

```c
#pragma pack(push, 1)
struct Mat0PrefixV4Plus {
    uint16_t phase_count;             // +0x00
    uint16_t animation_block_count;   // +0x02, меньше 20
    uint8_t  metadata_a;              // +0x04, attr2 >= 2
    uint8_t  metadata_b;              // +0x05, attr2 >= 2
    uint32_t metadata_c_raw;          // +0x06, attr2 >= 3
    uint32_t metadata_d_raw;          // +0x0A, attr2 >= 4
};

struct Phase34 {
    uint8_t parameters[18];
    char texture_name[16];
};
#pragma pack(pop)
static_assert(sizeof(Phase34) == 34);
```

Если `attr2 < 2`, metadata A/B получают default `255`; при `attr2 < 3`
значение C соответствует `1.0f`; при `attr2 < 4` D равно 0. C/D сохраняются
как raw 32-bit values до полного подтверждения интерпретации. Phase parameters
сохраняются как 18 raw bytes даже там, где часть bytes уже имеет понятный
смысл.

Каждая phase разворачивается в runtime-запись примерно 76 байт: коэффициенты
цвета, освещения и прозрачности, texture slot и служебные поля. Material time
выбирает одну или две phases; только часть полей интерполируется, остальные
копируются из активной записи.

Animation block MAT0 имеет плотный framing без 4-byte tail alignment:

```text
u32 header_raw
u16 key_count
repeat key_count:
    u16 k0
    u16 k1
    u16 k2
```

Младшие три бита `header_raw` задают числовой mode, остальные образуют mask
interpolation. Наблюдаются modes 0, 1, 2 и 3, связанные с семействами loop,
ping-pong, one-shot/clamp и random-offset, но точные boundary cases остаются
предметом runtime parity. Поле `k2` сохраняется всегда.

Проверки MAT0:

- `animation_block_count < 20`;
- все versioned metadata помещаются в payload;
- секция phases имеет ровно `phase_count * 34` байта;
- `texture_name` ограничено 16 байтами;
- каждый animation block и его keys помещаются в payload;
- parser заканчивает чтение на точном конце записи.

Material manager кэширует разобранный MAT0 и texture handles. Current phase
лучше вычислять на экземпляр материала, если random offset или локальное время
различаются между объектами; immutable phase data остаются общими.

## Texm: текстуры, mip-уровни и атласы

`Texm` -- основной формат изображений. Он хранится в `Textures.lib`,
`LightMap.lib` и других NRes-архивах. Payload содержит header, необязательную
palette, mip chain и иногда `Page` chunk для atlas rectangles.

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

Подтверждённые formats:

```text
0      Indexed8 + palette 256 x 4 байта
565    R5 G6 B5
556    R5 G5 B6
4444   A4 R4 G4 B4
88     L8 A8
888    RGB8 в четырёхбайтовом element
8888   A8 R8 G8 B8
```

Formats 556 и 88 являются loader-confirmed, но не corpus-verified для
доступных игровых payload. CPU decoder расширяет короткие каналы до 8 bit через
повторение значимых bit, а не простым shift. Для 888 служебный четвёртый byte
сохраняется при roundtrip.

Layout:

```text
TexmHeader32
[palette 1024 байта, только для format 0]
level 0 pixels
level 1 pixels
...
level mip_count-1 pixels
[optional Page chunk]
```

Размер уровня `i` вычисляется из `max(1, width >> i)` и
`max(1, height >> i)`. Bytes per pixel: 1 для indexed; 2 для 565, 556, 4444 и
88; 4 для 888 и 8888. Parser суммирует размеры с проверкой overflow до чтения.

`Page` chunk:

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

Chunk обязан иметь размер `8 + rect_count * 8`; произвольный tail не
допускается. Rectangles задаются в pixel space базового mip. Если loader
пропускает верхние mip-уровни, rectangles масштабируются вместе с новым base
level.

Mip-skip является поведением loader-а, а не offline-изменением файла. После
skip меняются runtime width, height, mip count и pointer на первый загружаемый
уровень. Современный renderer должен повторить выбор base level или
эквивалентно эмулировать его upload policy; использование полной texture при
тех же UV меняет резкость и atlas coordinates.

Indexed texture требует связанную palette. Часть palettes выбирается по suffix
имени: буква `A..Z` и вариант пустой или `0..9`, всего 286 возможных slots.
Невалидный suffix диагностируется явно.

Обычные textures и lightmaps находятся в разных managers. Обычный cache
отслеживает refcount и время неиспользования, а eviction выполняется
отложенно. Lightmap lifetime связан с world/mission и не должен попадать под
ту же политику удаления.

Строгий Texm parser проверяет положительные dimensions, положительный
`mip_count`, известный format, точный размер palette/mip chain, корректный
`Page` и отсутствие лишних bytes. `flags4`, `flags5` и `unknown6` сохраняются
1:1; участие `flags5` в mip-skip подтверждено, но полная семантика всех bits не
закрыта.

## Свет, тени, атмосфера и сортировка

Свет является отдельной world-подсистемой. Terrain layer создаёт
`LightManager`, `Shader` и primitive managers. Это не один глобальный
коэффициент яркости: world управляет point lights, lightmaps, shadows,
atmospheric objects и sort phases. Материал сообщает свойства поверхности, а
CShade превращает их в states renderer-а.

Подтверждённые точки: `CreateLightManager`, `CreateShader`,
`CreateAtmosphere`, `CreatePrimitives`, `CreatePrimitives2`,
`CShade::StartMeshRender`, `CShade::EndMeshRender` и
`CShade::ConfigureTextureAndAlphaBlendModes`.

CShade получает active MAT0 phase, capability profile устройства и pass
context. Он выбирает texture mode, alpha blending, depth/cull behavior и способ
освещения. Наличие fallback вроде `TEXTUREMODE_MODULATE not supported`
означает, что material нельзя напрямую преобразовать в современный PBR.
Сначала строится legacy state, затем он сопоставляется shader permutation.

CLightManager выдаёт numeric IDs источникам и проверяет допустимое количество.
Ветка `EmulatePointLights()` позволяет воспроизводить point lights даже при
ограничениях hardware lighting. Неизвестный type light должен давать отдельную
ошибку.

Lightmap не является обычной diffuse texture. WEAR содержит отдельный блок
`LIGHTMAPS`, manager открывает `LightMap.lib`, а shade path подаёт lightmap
отдельным slot или texture stage. Замена lightmap предварительным умножением в
diffuse texture ломает LOD, atlas coordinates и динамическую модуляцию.

Тени проходят отдельным render pass. Terrain содержит пути для теней зданий и
роботов, ограничения максимального числа, detail level и smoothing. Доказаны
shadow manager/pass, настройки detail/smoothing/count и зависимость от
Terrain/CShade; полная формула projection geometry для каждого caster требует
dynamic trace. Unknown settings из `shade.cfg` читаются и сохраняются по
именам, а не заменяются произвольными modern defaults.

Atmosphere manager создаёт world objects для фоновых и погодных явлений.
Отдельно подтверждены lightning, sun render, flare, `env_lightning`, rain
background sound и обязательные ссылки на lightning effect. Эти объекты
обновляются по игровому времени, но часть параметров зависит от camera: flare
требует screen position и occlusion test, rain -- области рядом с observer,
sound -- listener. Их нельзя один раз запечь в terrain.

RNG для lightning, atmosphere phases и FX должен иметь стабильный порядок.
Даже правильный средний интервал не даёт повторяемый кадр, если random values
запрашиваются в другой последовательности.

Согласованная модель sort phases:

```text
opaque terrain and models
  -> lightmapped/state-grouped passes
  -> shadows and projected primitives
  -> alpha-tested surfaces
  -> transparent objects/effects back-to-front
  -> atmosphere, flares and overlays
```

Точный взаимный порядок отдельных FX, shadow и atmosphere subpasses требует
capture. Новый renderer должен хранить явный `RenderPhase` и стабильный
secondary sort key, а не сортировать всё только по material ID.

## FXID: система эффектов

FXID -- не готовая картинка, а описание небольшого runtime command stream.
Header задаёт lifetime, time mode, random shifts и transform. Затем идут
команды разных types. При создании manager превращает disk-команды в runtime
objects; во время кадра они обновляются и выпускают sounds, particles,
materials или projected primitives.

Type ID равен `0x44495846`. Header занимает 60 байт:

```c
struct FxHeader60 {
    uint32_t command_count;
    uint32_t time_mode;
    float duration_seconds;
    float phase_jitter;
    uint32_t flags;
    uint32_t settings_id;
    float random_shift[3];
    float pivot[3];
    float scale[3];
};
```

Поток команд начинается строго с offset `0x3C`. `duration_seconds`
преобразуется runtime-ом во внутреннюю шкалу времени. `phase_jitter` и
`random_shift` используются только при соответствующих flags. Pivot задаёт
локальную точку опоры, scale -- базовый масштаб экземпляра. Unknown flags и
settings ID сохраняются.

Каждая команда начинается с `uint32_t command_word`:

```text
opcode  = command_word & 0xFF
enabled = (command_word >> 8) & 1
```

Bits 9-31 являются частью данных и сохраняются. Между командами нет
выравнивания. Размер команды, включая word:

```text
opcode 1   224 байта
opcode 2   148 байт
opcode 3   200 байт
opcode 4   204 байта
opcode 5   112 байт
opcode 6     4 байта
opcode 7   208 байт
opcode 8   248 байт
opcode 9   208 байт
opcode 10  208 байт
```

Parser использует opcode только для выбора фиксированного размера. Неизвестный
opcode отклоняется: попытка угадать длину потеряет синхронизацию всего stream.

Opcodes 2, 3, 4, 5, 7, 8, 9 и 10 содержат pair fixed strings:

```c
struct FxResourceRef64 {
    char archive[32];
    char name[32];
};
```

Имена сравниваются case-insensitive по ASCII, а tail после первого nul byte
сохраняется. Resolve выполняется при создании command object или лениво при
первом запуске, но ошибка должна включать имя эффекта, номер команды, archive
и resource name.

Базовый normalized age:

```text
tn = (now - start_time) / (end_time - start_time)
```

`time_mode` выбирает источник коэффициента: constant, forward/reverse age,
cyclic phase, external world state и варианты с ограничением относительно
предыдущего значения. Точные формулы редких modes являются parity-задачей.
Flags могут умножать alpha на lifetime, применять triangular remap, случайно
сдвигать phase/space, инвертировать active-state, фильтровать по времени суток
или включать manager gates.

Lifecycle:

```text
create instance
  -> copy header and external transform
  -> calculate end time and random offsets
  -> create command objects in disk order
  -> resolve required resources
  -> Start

on each calculation/render frame
  -> evaluate time coefficient and gates
  -> update commands in stable order
  -> emit active primitives or sounds
  -> collect render batches
  -> handle Stop / Restart / end-of-life
```

Update и emit разделяются. Simulation может продолжаться в кадре без render, а
emit не должен повторно менять игровое состояние. Для authoring безопасно
типизировать header и resource references, а body редких commands сохранять raw
до подтверждения field-level semantics.

## Полный кадр

Крупный вход в world render проходит через `World3D::stdRenderGame`. Доказан
следующий порядок boundary операций:

1. передать camera в Terrain через `stdSetCurrentCamera2` и сохранить её как
   текущую;
2. получить camera/view/viewport interfaces через virtual queries;
3. обновить положение и ориентацию 3D sound listener;
4. настроить renderer viewport и matrices;
5. вызвать два renderer boundary slots перед traversal;
6. установить глобальный флаг `in_render`;
7. вызвать главный virtual метод camera/world traversal;
8. выполнить дополнительную post queue при включённом режиме;
9. завершить world/shade pass;
10. вызвать renderer completion slot;
11. снять `in_render`, восстановить viewport и разослать end-of-render.

Семантические имена нескольких slots перед и после traversal не подтверждены,
поэтому в compatibility code их лучше временно называть
`frame_boundary_0`, `frame_boundary_1`, `frame_boundary_2`.

Обход видимого мира:

```text
проверить active/visible state
  -> выбрать LOD по расстоянию и настройкам
  -> получить node matrices из animation state
  -> выбрать slot для каждого node/group
  -> преобразовать bounds в world space
  -> выполнить culling
  -> добавить batches в подходящую render queue
```

Material/texture resolve желательно выполнять после visibility и slot
selection, чтобы невидимые объекты не меняли порядок обращений к caches и не
создавали лишние side effects. Невидимость объекта и отсутствие slot являются
разными причинами пропуска и диагностируются отдельно.

Подготовленный draw item содержит:

```text
node world matrix
batch flags and index range
WEAR material handle
MAT0 active phase and coefficients
texture handle
optional lightmap handle
render phase and sorting key
legacy pipeline state
```

Draw item должен ссылаться на immutable данные кадра. Изменение phase или
texture cache посреди прохода не должно менять уже собранную очередь.

Согласованная декомпозиция внутренних render phases:

1. подготовка frame state, camera и viewport;
2. непрозрачный terrain;
3. непрозрачные object batches;
4. lightmap и дополнительные material passes;
5. projected primitives и тени;
6. alpha-tested geometry;
7. transparent objects и FX в сортировочных слоях;
8. atmosphere, sun, flare и weather;
9. renderer completion boundary;
10. end-of-render callbacks;
11. shell/UI и post-render state.

Точный взаимный порядок пунктов 4-8 и связь completion slot с физическим
DirectDraw flip/present требуют dynamic capture. Сортировка внутри каждой фазы
должна быть стабильной: для opaque первичен pipeline/material key, для
transparent -- distance layer и depth order, затем stable insertion ID.

Геометрический draw использует streams type 3/4/5, optional streams, index
buffer type 6, `base_vertex`, `index_start` и `index_count`. Матрица узла
устанавливается как world transform, затем CShade привязывает texture stages и
fixed-function state.

```c
set_world_matrix(item.node_world);
bind_vertex_streams(model.streams);
bind_index_buffer(model.indices);
apply_legacy_state(item.pipeline);
bind_texture(0, item.texture);
bind_texture(1, item.lightmap);
draw_indexed(item.batch.base_vertex,
             item.batch.index_start,
             item.batch.index_count);
```

## Текущий контракт pipeline key

`fparkan-render` теперь переносит вместе с каждым backend-neutral draw
`LegacyPipelineState` и детерминированный `PipelineKey`. Ключ не хешируется:
он явно упаковывает blend mode, depth mode, cull mode и флаг alpha-test, поэтому
одинаковые snapshots дают одинаковый capture и будущий Vulkan cache не зависит
от версии Rust или процесса. Alpha reference намеренно не входит в key: это
dynamic material constant, а не структурный вариант graphics pipeline.

Текущий static Vulkan viewer дедуплицирует состояния ranges при каждой сборке
swapchain resources, создаёт один `vk::Pipeline` на уникальный `PipelineKey` и
выбирает его непосредственно перед соответствующим `vkCmdDrawIndexed`.
Кэш корректно уничтожается при recreate/teardown; setup failure откатывает уже
созданные variants. Baseline оригинального MSH подтверждён с состоянием
`opaque + depth disabled + cull disabled + alpha-test disabled`.

`SourceAlpha`, front/back culling и depth modes имеют Vulkan mapping в этом
кэше. При создании swapchain resources renderer выбирает capability-approved
depth/stencil format, выделяет device-local attachment, очищает его в render
pass и прикрепляет к каждому framebuffer. `TestWrite` включает depth test и
write; `TestReadOnly` включает test без write. Alpha test выполняется во
fragment shader: перед каждым draw renderer передаёт через push constant
`alpha_test_reference / 255`, а shader отбрасывает fragment с меньшей alpha.
При выключенном `alpha_test` cutoff принудительно равен нулю; reference остаётся
dynamic material data и не входит в `PipelineKey`. Также не установлен источник
значений для Batch20/MAT0: их
поля нельзя объявлять blend/depth/cull mapping без dynamic capture или
дополнительного дизассемблирования. Это частично реализованная compatibility
boundary, а не заявление о готовой parity fixed-function state.

### Capability-gated swapchain source for future pixel capture

Pixel parity требует чтения результата GPU, но swapchain image разрешает это
только если surface сообщает `VK_IMAGE_USAGE_TRANSFER_SRC_BIT`. Поэтому
swapchain policy всегда запрашивает обязательный `COLOR_ATTACHMENT`, а
`TRANSFER_SRC` добавляет лишь при фактической поддержке surface; выбранные raw
usage bits попадают в deterministic plan и native smoke report. Это не создаёт
readback buffer, не копирует pixels и не закрывает pixel-capture acceptance.

На Windows GOG native smoke 2026-07-18 (`MTCHECK.MSH` из `system.rlb` и
`DEFAULT.0` из `Textures.lib`) AMD Radeon Pro WX 3200 Series подтвердил
`image_usage=17` (`COLOR_ATTACHMENT | TRANSFER_SRC`): 300 frames, 3 resize
events, 2 swapchain recreations, Vulkan validation warnings/errors `0/0`.
Следующий шаг должен выполнить явный image-to-buffer copy и сравнить полученные
bytes с зафиксированным reference capture.

### First synchronized Vulkan pixel-readback artifact

Stage 3 static viewer теперь выполняет фактический readback для surface с
`TRANSFER_SRC`: на каждый swapchain image создаётся host-visible coherent
`TRANSFER_DST` buffer. После render pass command buffer переводит image из
`PRESENT_SRC_KHR` в `TRANSFER_SRC_OPTIMAL`, выполняет `vkCmdCopyImageToBuffer`
и возвращает image в `PRESENT_SRC_KHR`. CPU отображает memory только после
`vkDeviceWaitIdle` during shutdown; это исключает чтение GPU work in flight.
Smoke JSON фиксирует число записанных copy-команд, final byte count и FNV-1a-64
hash всех current-swapchain readback buffers, не сохраняя игровые pixels в
репозитории.

На GOG `MTCHECK.MSH`/`DEFAULT.0` AMD Radeon Pro WX 3200 Series выполнил 300
copy-команд, final artifact 4,147,200 bytes и hash `2184179010340020629` при
validation warnings/errors `0/0`. Повторный идентичный запуск дал тот же
размер и hash. Это доказывает Vulkan copy/readback path только для нашего
static viewer. Он ещё не захватывает original DirectDraw frame, не задаёт
fixed original camera и не сравнивает два изображения, поэтому pixel-parity
acceptance остаётся blocked.

Smoke также сохраняет raw artifact рядом с JSON: `<report-stem>.readback-vkformat-<raw>.raw`.
Это concatenated current-swapchain images in Vulkan order, каждый с dimensions
из JSON и четырьмя bytes per pixel; format намеренно указан в имени файла,
потому что bytes не перекодируются; JSON содержит actual raw enum. GOG selected `50` and produced a 4,147,200-byte file
for two 960x540 images. Артефакт остаётся локальным output и не попадает в Git.

`Land.msh` использует отдельный geometry-only bridge: validated `TerrainFace28`
сохраняет source triangle order, а его positions и packed UV0 попадают в тот же
static vertex/index upload path. Для текущего диагностического viewer XZ bounds
нормализуются в clip-space, packed UV0 декодируется как signed `int16 / 1024`.
Один static draw range намеренно не трактует `material_tag`, surface mask, slots
или auxiliary streams как material/pipeline state: их связь с original terrain
renderer пока не доказана. GOG `DATA/MAPS/11/Land.msh` дал 6 458 vertices и
24 672 indices в 300-frame native Vulkan run без validation warning/error.

Выбор depth/stencil attachment отделён от renderer lifetime:
`select_depth_stencil_attachment_format` применяет тот же фиксированный порядок
форматов, что и capability gate, к фактически поддерживаемому списку GPU.
Это исключает ситуацию, когда admission принимает один совместимый формат, а
allocation позднее выбирает другой; resource уничтожается после framebuffer и
render pass при recreate/teardown.

После последнего world pass renderer закрывает сцену и выводит back buffer.
World3D снимает `in_render`, восстанавливает временный viewport state и вызывает
`on_end_render` у active objects. Только после этого допустимо освобождать
temporary vertex buffers или заменять render representation. UI/shell
обслуживается верхним уровнем после возврата из world-render path; для
диагностики полезно уметь сохранять world-only command list и финальный
framebuffer отдельно.

## Проверки паритета

Главные риски совпадения кадра:

- x87 extended precision и правила округления;
- различия scalar/SIMD slots `g_FastProc`;
- порядок objects, batches и transparent primitives;
- depth write/test, cull, alpha test и blend transitions;
- mip-skip, palette и `Page` coordinates;
- material fallback и выбор phase;
- последовательность RNG для FX и atmosphere;
- capability fallback конкретного устройства;
- quantization времени и дополнительный simulation step;
- eager/lazy resource resolve и cache side effects.

Минимальный deterministic frame capture должен включать camera state, viewport,
visible object IDs, выбранные LOD/group/slot, draw-item list, material и texture
handles, pipeline keys, matrices, render phase, sort key, причины culling и
hashes промежуточных buffers. Без такой трассировки нельзя уверенно отделить
ошибку формата MSH от ошибки state machine renderer-а или сортировки.

Связанные справочные страницы с таблицами форматов: [MSH](../reference/msh.md),
[materials](../reference/materials.md), [Texm](../reference/texm.md) и
[render frame](../reference/render-frame.md).
