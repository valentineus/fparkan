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

Smoke option `--expected-readback <path>` выполняет exact byte comparison после
readback и завершает run с первым differing byte или length mismatch. Fresh GOG
run с предыдущим format-50 artifact прошёл этот gate. Это regression contract
нашего static Vulkan viewer, не comparison с оригинальным renderer.
Comparator прежде сравнивает `vkformat` identity из имен обоих artifacts и
отклоняет mismatch до byte scan; raw blobs разных Vulkan formats нельзя
считать сопоставимыми без явной conversion contract.

`fparkan-game` render-snapshot bridge больше не берёт только первый prepared
visual mission object. Каждый `MissionAssets::visuals_for_object` entry
создаёт отдельный backend-neutral draw с own mesh/material IDs; object без
prepared visual сохраняет один compatibility fallback draw. Это сохраняет
multi-component prototype graph для будущего real Vulkan renderer, однако
текущий game app всё ещё использует planning backend и triangle ranges.

### Mission position/scale render bridge

`fparkan-game` now maps preserved TMA position and non-uniform scale into each
backend-neutral draw matrix instead of diagnostic index placement. Raw
orientation deliberately remains uninterpreted: the original Euler order and
matrix convention are not yet proven. This preserves authored values without
inventing transform math; the app still uses planning backend and triangle
ranges, so it is not a full renderer claim.

### Mission original-ID render provenance

Each backend-neutral draw now retains the `OriginalObjectId` preserved by the
TMA loader for its source mission object. This gives capture and diagnostics a
stable link from a draw back to its original object record, including every
visual component emitted for that object. The bridge deliberately leaves the
value absent when no mission draft exists; it does not infer an original ID
from a runtime slot or draw order.

### Opt-in mission static-Vulkan bridge

`fparkan-game --backend static-vulkan` is an explicit native-window experiment,
not the default planning path. It visits only the first mission root, selects
every prepared MSH component of that root, merges their static XY clip-space
geometry, and renders a requested number of frames through
`VulkanSmokeRenderer`; teardown rejects validation warnings/errors and reports
swapchain/readback telemetry. For each used `Batch20.material_index`, it resolves
the positional prepared WEAR material and uploads mip 0 of that material's first
MAT0 diffuse texture request. Because `material_index` is local to an MSH/WEAR
component, the merged preview assigns a unique preview-local selector only after
this source resolution; it does not falsely treat equal local indexes as equal
materials.

This is a narrow bootstrap from mission assets to a live Vulkan renderer. It
does not render every placed object, apply TMA transforms or orientation, select
later MAT0 phases or animation, bind lightmaps, or establish a game camera. Fresh GOG
`MISSIONS/Autodemo.00/data.tma` evidence now proves the narrow GPU bridge: one
presented frame completed in 39.6 seconds with a native 1280×720 two-image
swapchain, 14 merged mesh components and 14 selector-keyed original diffuse
descriptors, 7,372,800-byte synchronized readback (FNV-1a
`16595193636416981301`) and validation
warnings/errors `0/0`. This is not a
full-scene or original-renderer pixel-parity claim.

The same bounded command was then run against the licensed installed Part 1
(`C:\\Program Files (x86)\\Nikita\\IS`) and Part 2
(`C:\\Program Files (x86)\\Nikita\\IS2`) corpora supplied for testing. Both
`MISSIONS/Autodemo.00/data.tma` runs completed with 14 mesh components and 14
original diffuse descriptors. Part 1 took 28.2 seconds and matched the GOG
readback hash `16595193636416981301`; Part 2 took 93.6 seconds and remained
validation-clean but produced distinct hash `18268338333658342130`. Part 2's
synchronous checkpoint was `Graph` while it was still loading, so the longer
startup is not attributed to Vulkan. This is only a cross-corpus confirmation
of the first-root static-preview bridge, not a claim that the two games' full
mission renderers are compatible.

The preview now has an explicit root-prefix contract. `--preview-roots N`
prepares the first non-zero `N` TMA roots for the opt-in static Vulkan path.
Without that diagnostic limiter, the static Vulkan path now requests every TMA
root (the runtime clamps the request to the decoded mission length), so a
normal native preview no longer silently displays just root zero. Normal mission
loading remains full and transactional; the prefix is retained for bounded
investigation rather than as a hidden relaxation of gameplay validation.

### Shared terrain/root diagnostic frame

The bounded native preview now retains the already validated `Land.msh` inside
`TerrainWorld`; this lets the application consume source geometry through the
runtime boundary instead of decoding archive formats a second time. It merges
one terrain component with the first TMA root's 14 reachable MSH components in
one explicit top-down XY frame. The frame is computed from terrain positions
and the root models after applying decoded TMA placement: non-uniform `scale`,
then the recovered `Rz * Ry * Rx` orientation, then `position`. The XY camera
itself remains diagnostic; applying the established placement transform here
prevents differently oriented mission objects from being framed or rasterized
at a false location.

Terrain is assigned an explicit 1x1 white diagnostic texture, rather than a
guessed terrain material. `Land.msh` slot selection, `material_tag`, masks,
auxiliary streams, original terrain phases and camera remain unresolved. Thus
this is an integrated geometry/provenance checkpoint, not a claim of original
terrain shading or scene camera reconstruction.

Fresh canonical GOG `MISSIONS/Autodemo.00/data.tma` evidence: one native
1280x720 Vulkan frame completed in 40.6 seconds with 14 root MSH components,
one terrain component, 15 descriptors, a 7,372,800-byte readback (FNV-1a
`10739087367165646439`) and validation warnings/errors `0/0`. The distinct hash
from the earlier first-root-only preview is expected because the submitted
geometry and descriptor set changed; it is not an original-frame comparison.

### Multiple static-preview roots

The native bridge now combines every mesh-backed visual of each selected root,
using that root's recovered static TMA placement (`Rz * Ry * Rx` after scale)
in the shared diagnostic XY frame. It reports the actual selected root count as
`preview_roots`, rather than implying that all mission objects are rendered.
The original camera/frustum, dynamic orientation ownership and visibility
remain unresolved.

Fresh canonical GOG `Autodemo.00` evidence with `--preview-roots 2` completed
in 59.7 seconds: 25 submitted MSH components, one terrain component and 26
descriptors on the native 1280x720 two-image swapchain, with validation
warnings/errors `0/0`. The readback FNV-1a remained
`10739087367165646439`, equal to the one-root terrain/root run. That equality
does **not** prove the second root is visible or equivalent: it was recorded as
an investigation target, not hidden as a parity result.

The reproducible `fparkan-cli mission inspect` probe closes two tempting but
incorrect explanations for that equality. GOG `Autodemo.00` root 0 is
`w_m_wlk2.dat` at `(418.10318, 717.433, 3.0409389)` while root 1 is
`w_s_wlk1.dat` at `(479.12396, 795.95337, 1.6228507)`; therefore the TMA data
is neither duplicate nor co-located. Separately, the Vulkan static submit loop
calls `cmd_draw_indexed` for every prepared draw range and its depth comparison
is `LESS_OR_EQUAL`. A later direct `Land.msh` bounds probe identified the actual
static-viewer defect: GOG `AutoMAP/Land.msh` spans X/Y
`0..1190.6976` but Z only `0..94.50981`; the same TMA objects use X/Y for their
world placement and a small Z height. The old XZ CPU frame discarded the second
horizontal TMA component and treated height as the screen axis. The renderer now
uses a shared XY CPU frame and retains Z as geometry height. This proves only
the source-axis convention needed by the diagnostic bridge; it does not recover
the original camera, orientation order, culling or projection matrix.

The new `fparkan-cli terrain inspect <Land.msh>` exposes these three-axis source
bounds without a renderer. The first XY GPU rechecks were intentionally bounded
to 120 seconds but remained at the existing `Graph` loading checkpoint and were
terminated without a native frame/report; therefore no new readback hash or GPU
acceptance result is claimed for the corrected projection yet.

The load trace now splits base root expansion (`Graph`) from MSH/WEAR/MAT0/TEXM
visual-dependency expansion (`GraphVisuals`) and enters `Assets` immediately
before actual asset preparation. A controlled 60-second GOG first-root run of
the XY build stopped at `GraphVisuals`; the process was terminated by the probe
and produced no frame. The startup bottleneck is therefore narrowed to visual
graph expansion, rather than the base prototype graph, asset decoding, window
creation or Vulkan submission. This is a diagnostic boundary, not a claim that
the visual expansion is semantically optional or may be skipped.

Visual expansion now caches the validation result of each unique diffuse TEXM
and each unique lightmap TEXM independently. It still emits an edge and request
count for every original material reference, and it replays the same cached
success or failure at every reference; only duplicate archive open/read/decode
work is removed. Under the same controlled 60-second GOG first-root probe, the
last checkpoint advanced to `Assets`. The process was deliberately terminated
before a frame, so this proves reduced visual-graph work rather than a Vulkan
acceptance result.

Asset preparation additionally caches validated MSH models and decoded WEAR
tables by their complete resource key. This preserves separate prepared visual
provenance while avoiding repeat format work for repeated components. The same
60-second first-root GOG probe remained at `Assets`, so that corpus does not
demonstrate an additional checkpoint advance from this cache; it must not be
reported as a measured startup improvement.

Visual-graph material resolution now also caches the complete success or failure
of each WEAR material request. Every material edge and failure remains in the
graph; the cache only avoids reopening and decoding an already resolved MAT0.
Under the controlled 60-second GOG first-root probe, the phase sequence advanced
from `GraphVisuals` to `AssetModelMeshes`, `AssetWearTables`, then
`AssetTextures` before termination. This is a bounded timing observation, not a
claim that a frame completed or that every run has identical timing.

After these visual-graph caches, the corrected XY static-Vulkan preview again
completed on canonical GOG `MISSIONS/Autodemo.00/data.tma` within a controlled
120-second run. It reported `Complete`, a native 1280x720 two-image swapchain,
one frame, 14 root MSH components, one terrain component, 15 material
descriptors, validation warnings/errors `0/0`, and a 7,372,800-byte readback
FNV-1a `6451493914305554398`. This is the first GPU acceptance evidence for
the corrected XY diagnostic projection; the hash is not compared with the
original renderer and does not establish original camera, shading or parity.

The identical bounded XY command also completed against the supplied licensed
Part 1 installation (`C:\\Program Files (x86)\\Nikita\\IS`). Its report had
the same 14 root MSH components, terrain component, 15 descriptors, native
1280x720 two-image swapchain, validation `0/0`, and FNV-1a
`6451493914305554398`. This confirms the current first-root diagnostic bridge
across GOG and Part 1 data; it does not claim full mission or Part 2 parity.

The corresponding Part 2 run (`C:\\Program Files (x86)\\Nikita\\IS2`) did
not reach asset preparation or a Vulkan frame within a controlled 180-second
window: its final persisted checkpoint was `GraphVisuals`, and the exact child
process was terminated. This gives no Part 2 GPU acceptance or pixel result and
does not attribute the delay to Vulkan; its visual-dependency corpus remains a
separate profiling and compatibility target.

`GraphVisuals` is now split observationally into `GraphVisualWears`,
`GraphVisualMaterials` and `GraphVisualTextures`; it preserves the existing
graph traversal, edge order and validation semantics. Repeating the controlled
180-second Part 2 probe once ended at `GraphVisualTextures`. That particular
execution had passed WEAR and MAT0 expansion before timeout, but a checkpoint
is only the last phase reached in one run: it is not proof of a single global
bottleneck or of Vulkan involvement.

Visual expansion now reports each dependency-class entrance, its first request
and every 64th later request; runtime progress persists MAT0/TEXM counts at
those boundaries. This is diagnostic-only and does not alter traversal, edges
or validation. A
later controlled 180-second Part 2 probe ended at `GraphVisualMaterials`,
before the first TEXM progress marker. The differing checkpoints rule out the
previous overly narrow claim that TEXM is the sole remaining target. Profiling
must compare WEAR, MAT0, TEXM, graph allocation and archive I/O quantitatively.

Visual expansion now snapshots each prototype's base-graph `Prototype→MSH`
anchor before it appends any WEAR/MAT0/TEXM nodes and edges. The subsequent
traversal reads this immutable vector rather than repeatedly linearly searching
the growing graph; graph content, node/edge IDs and provenance remain
unchanged. The same controlled 180-second Part 2 probe still ended at
`GraphVisualMaterials`, so this is a complexity/correctness improvement rather
than measured evidence of a startup checkpoint advance.

The MAT0 counter now records `GraphVisualMaterialRequests(N)` with the same
64-request throttle. A fresh controlled Part 2 probe ended at
`GraphVisualMaterialRequests(71)`: at least 71 MAT0 requests had begun before
termination. It does not preserve the simultaneous TEXM count, so it neither
identifies a slow individual material nor attributes time to MAT0 parsing; the
next profiler revision needs one cumulative snapshot of all classes.

Visual MAT0/TEXM request markers now also carry the graph node and edge counts
materialized immediately before that request resolves. A rebuilt bounded Part 2
probe recorded MAT0 request 3 at 52 nodes / 51 edges and request 4 at 55 nodes
/ 54 edges. These are observational snapshots for graph-growth profiling only:
they do not change traversal order, node or edge identities, provenance,
validation, cache semantics, or rendering.

The same throttled snapshots now retain the number of distinct validation-cache
keys for WEAR, MAT0, and TEXM before the request. In a fresh 50-second Part 2
probe, MAT0 request 3 recorded cache counts 2 / 3 / 3 and request 4 recorded
3 / 4 / 4, while the graph moved from 52 / 51 to 55 / 54 nodes / edges. The
counts distinguish cache growth from graph growth without adding per-request
file writes or asserting which parser, archive I/O operation, or allocation
caused the elapsed interval.

`fparkan-game --load-progress` now initializes its file with `Starting` and
appends each later mission-load event instead of replacing the prior line. A
bounded probe can therefore retain both MAT0 and TEXM request milestones even
when a later event is last. The behavior is diagnostic persistence only: it
does not alter graph traversal, resource validation, or rendering.

Each trace row now keeps the phase name first and appends a monotonic
`elapsed_ms=<N>` field from one process-local `Instant`; successful completion
is appended rather than overwriting the trace. A short rebuilt Part 2 probe
confirmed real timestamps from `Map` at 5 ms through `GraphVisualTextures` at
2,389 ms, then later MAT0 request milestones at 28,841 and 35,486 ms. The
probe was deliberately terminated and provides no frame/GPU result. These
timestamps measure whole elapsed intervals, including archive I/O, allocation,
validation, cache effects and progress-file writes; they are not per-parser
benchmarks.

One bounded Part 2 trace reached `GraphVisualTextureRequests(64)` at 134,793
ms but did not reach `Assets`: its MAT0 markers were 3/4 at 28,823/35,473 ms,
13/16 at 88,260/88,334 ms, 25 at 108,137 ms, 38 at 121,369 ms, and 47 at
128,160 ms. The process was stopped immediately after the 134.8-second marker.
This makes visual-graph expansion, rather than asset preparation or Vulkan,
the measured unfinished interval for this run. It still does not allocate that
time among MAT0 decode, graph allocation, archive I/O, OS cache state, or the
interleaved TEXM requests.

Visual expansion now also caches WEAR validation by the complete derived WEAR
archive/name key and replays both successes and failures. It still creates each
per-prototype WEAR edge, increments the same request counters, and reports the
same failure provenance. A unit test verifies an archive-qualified cached
failure is replayed without consulting the repository. In a matched bounded
Part 2 probe the first TEXM-64 marker occurred at 135,520 ms, versus 134,793
ms in the preceding sample, and neither run reached `Assets`; therefore this
corpus does not evidence a startup advance from WEAR caching. The path is kept
as a correctness-preserving duplicate-resolve optimization, not a causal
performance claim.

A rebuilt executable then ran a controlled 180-second Part 2 `Autodemo.00`
probe with the append-only trace. Before exact-child termination it recorded
`GraphVisualTextureRequests(64)`, `GraphVisualMaterialRequests(100)`, and every
asset-preparation checkpoint through `AssetTextures`. It produced no native
window, frame, readback, or Vulkan acceptance report because it was terminated
while preparation was still active. This is one warm-state timing sample, not
evidence that the trace change improved loading or that either MAT0/TEXM alone
caused the earlier timeout; it does show the prior last-event-only trace hid
concurrent request classes and later preparation progress.

Mission loading now raises the decoded-payload cache entry budget from 64 to
256 while retaining its 64 MiB byte budget. This avoids premature entry-count
eviction during resource-rich loads without making memory unbounded. The same
180-second Part 2 probe nevertheless remained at `GraphVisualTextures`; this
change is a safe cache-capacity improvement, not measured evidence of a Part 2
startup advance.

`Assets` now has four ordered diagnostic sub-checkpoints: `AssetModelMeshes`
(MSH), `AssetWearTables` (WEAR), `AssetMaterials` (MAT0) and `AssetTextures`
(diffuse TEXM and lightmaps). The callback is observational: it neither changes
the preparation order nor deduplicates requests. A fresh controlled 60-second
canonical GOG `Autodemo.00` first-root probe reached `AssetWearTables` but not
`AssetMaterials`; its created process was terminated and no native frame was
reported. At least one MSH therefore finished before the timeout, but this does
not attribute the remaining interval to WEAR decoding rather than later
MSH/WEAR iteration or archive I/O.

The same source-axis proof now applies to `TerrainWorld`: its `Land.msh` surface
height query and `Land.map` areal/grid lookup use XY ground coordinates, return
source Z height, and leave raycasts as full 3D intersections. The Part 1 and
Part 2 licensed `Land.map` gate successfully locates sampled polygon vertices
under that contract. This closes the former XZ mismatch in terrain queries; it
does not establish object orientation, physics/gravity, original culling or the
camera matrix.

### Camera ownership boundary from the GOG renderer

The GOG `World3D.dll` export `LoadCamera` at RVA `0x1FB06` is only an import
thunk: it jumps through IAT `0x10020148` to `Terrain.dll!LoadCamera`. The
Terrain export is at RVA `0x4EBE0` and is a `__stdcall` entry with `ret 0x10`,
so it receives four machine-word arguments. It allocates a `0x1A4`-byte object,
forwards those four words to its constructor, and returns a pointer at object
offset `+0x134`. The constructor is at RVA `0x4EC60`: it passes arguments 3 and
4 to the base initialization at `this + 4`, stores argument 3 at `this + 0x138`,
and passes arguments 1, 2 and 4 plus `this` to its helper at RVA `0x4CEF0`.
Argument 1 is a NUL-terminated mode string at this call boundary: only the
embedded literals `REFLECTION` and `REFLECTION_SHIFTED` select a non-zero value
stored at `this + 0x1A0`. This proves that `LoadCamera` supports reflection
variants, but not the semantic types of its remaining arguments or the meaning
of the resulting value. `Terrain.dll!stdGetCurrentCamera2` returns a global
camera pointer, and `stdSetCurrentCamera2` updates it through the object's
interface methods.

This establishes that original camera creation/selection is Terrain-owned and
not a field that can safely be inferred from a TMA transform. It does **not**
yet establish world-to-view matrix layout, FOV, near/far values, projection
handedness, or initial mission camera selection. The Vulkan path must therefore
continue to label its XY projection as diagnostic until a dynamic capture or
further Terrain disassembly proves these contracts.

On 2026-07-18 the canonical windowed GOG process was confirmed running with
one targetable window titled `Parkan. Железная Стратегия`. Two read-only
Windows.Graphics.Capture attempts failed before returning a frame with
`SetIsBorderRequired failed: Интерфейс не поддерживается (0x80004002)`. No game
input was sent and this contributes no visual, camera, timing, or memory
evidence; a different capture/debugger path is required for dynamic analysis.

The public `World3D.dll!stdSetCurrentCamera` is a one-pointer `__stdcall`
wrapper: it delegates to the Terrain `stdSetCurrentCamera2` IAT thunk and then
writes the same pointer to a World3D mirror global. An `iron3d.dll` call site
at RVA `0x4FB95` loads the argument from an active game-state chain
`[esi + 0xBC90] + 0x94` before calling that wrapper. This proves that the
selected camera is an object reference supplied by game state, not a scalar
mission-coordinate field. The caller's enclosing type and the meaning of
offsets remain unnamed until a class layout is recovered.

Terrain strings also identify `CBufferingCamera`, `ICamera::SetTransformMatrix`
and `ICamera::GetTransformMatrix`, plus frustum/clip methods. These names prove
that a transform-matrix interface exists, but give neither method slots nor a
matrix convention; they must not be treated as a usable renderer ABI yet.

### Buffering-camera projection path from static Terrain analysis

A further headless-IDA review of the same GOG `Terrain.dll` identifies a
`CBufferingCamera`-related implementation path without requiring a live game
process. The procedure at RVA `0x4D740` copies exactly 64 bytes (16 dwords)
into its receiver at offset `+0x10`. The frame-preparation procedure at RVA
`0x4D9C0` queries a viewport rectangle through a virtual method at `+0x3C`, derives its
width, height, centre and an aspect ratio from that rectangle, and obtains
projection data through the camera interface. Projection type `0` uses a
receiver float at offset `+0x234` as an angle passed to `tan(angle / 2)`;
projection type `2` obtains a five-float block through a virtual method at
`+0x70`. Any other non-zero type raises `Not supported projection type`.

This is sufficient to prove that original viewport and projection state are
live camera inputs, rather than a fixed diagnostic projection. It does **not**
yet prove matrix row/column convention, angle units, near/far field mapping,
handedness, or the initial mission camera object. In particular, the exported
string-labelled `ICamera::SetTransformMatrix` (RVA `0x4F830`) and
`ICamera::GetTransformMatrix` (RVA `0x4F850`) are both obsolete-call stubs, so
they are not a usable method ABI.

Live read-only evidence now confirms the relocation-aware global contract. In
one elevated GOG run, `Terrain.dll` loaded at `0x02510000`; its global at
`base + 0x7355C` held non-null `0x0B37DF08`, whose first dword was
`0x025765B4` — exactly the relocated `off_100665B4` vtable from the
`LoadCamera` construction path. This proves the global is a live camera object
with that outer vtable. It also corrects an overreach: raw reads at global
offsets `+0x10` and `+0x234` did not yield a finite 4×4 matrix and a projection
angle in this sample. The static procedures' receiver and the exported global
pointer are therefore not yet proven to have the same adjustment/layout; those
offsets must remain unassigned until the selector/interface relationship is
recovered.

The unattended demo also proves that this global is dynamic. Across 25 seconds
of read-only sampling it held three distinct heap pointers; all began with the
same relocated outer vtables `0x025765B4` and `0x02576558`. Two selected
objects exposed synchronized translation triples in the paired raw blocks at
offsets `+0x2C/+0x3C/+0x4C` and `+0x6C/+0x7C/+0x8C`, with world-like values
such as `(491.562, 761.551, 7.361)`. A third object used the same vtable but
reported normalized-looking values near `(0.098, 0.018, 0.856)` and did not
match the paired block. The observed pointer transitions are consistent with
the unattended camera changes, but the sample carries no mode label and does
not prove first- versus third-person identity. It does prove that the static
direct-xref initializer is not the sole runtime writer of the global; indirect
or unanalyzed write paths remain to be recovered.

The shared outer vtable now recovers the exact transform adjustment for the
world-like samples. Its slot `+0x54` asks the subobject at `outer + 4` for
selector `0` through that subobject's slot `+0x20`, then copies three dwords
from returned offsets `+0x0C`, `+0x1C`, and `+0x2C`. In the live world-like
object, the selector field at `outer + 0x10` was `0xFFFFFFFF`; the recovered
selector implementation therefore returns `outer + 0x20`. The copied triple is
exactly `outer + 0x2C/+0x3C/+0x4C`, proving its association with the active
affine transform rather than merely a sampled correlation. The paired block
starts at `outer + 0x60` under selector `2`. Another outer slot at `+0x70`
obtains that selector-2 transform and applies `atan2` to its axis values, so it
is a proven orientation-angle path. Field names, angle order and matrix
handedness remain unassigned, but a backend-neutral camera pose may now retain
the raw affine transform and the exact translation triple without guessing them.

The render contract now represents that boundary directly. `RawCameraTransform`
preserves each selector result as sixteen original `u32` words, so diagnostic
data is not rounded or silently assigned a row/column convention. Its only
typed accessor reinterprets the proven X/Y/Z words `3/7/11` as IEEE-754
floats. `RawCameraPose` retains selector `0` and selector `2` together, and
`CameraSnapshot::raw_pose` is optional. The present Vulkan renderer continues
to use its existing view/projection matrices: raw pose capture is intentionally
not converted into a view matrix until the original transform/projection
convention has evidence.

The next live sample strengthens the representation contract but still does
not name the transform's camera-space direction. For six no-input samples of
one auto-demo camera object, selectors `0` and `2` were byte-identical,
finite affine blocks with bottom row `(0, 0, 0, 1)`. Their upper 3x3 basis
was normalized and their last-column translations changed from approximately
`(441.038, 687.481, 10.754)` to `(433.545, 652.293, 10.673)`.

This storage/order is not guessed: live `NGI32.dll!g_FastProc[23]` dispatches
to RVA `0x1D9A0`, whose SSE implementation writes each output row as the sum
of four scalar-weighted rows. Thus a contiguous 16-float block is multiplied
as a conventional row-major 4x4 matrix. The render contract now exposes a
mathematical `try_inverse_affine_row_major` helper for such finite,
non-singular `[R | t; 0 0 0 1]` blocks. It returns no result for a singular or
non-affine block and, crucially, does **not** identify that inverse as a view
matrix. Whether a selector result is camera-to-world or already a renderer
transform remains an evidence boundary.

The active camera now also has a proven live projection-interface boundary.
`LoadCamera` stores its selector-18 camera interface at outer offset `+0x19C`;
in the elevated GOG AutoDemo sample it was a `CBufferingCamera` object with
relocated vtable `0x02576344`. Its rectangle at `+0x10` was exactly
`(left=0, top=0, right=1024, bottom=768)`, its projection-type field at
`+0x230` was `0`, and its FOV field at `+0x234` was `1.04` radians.
`CBufferingCamera` passes that FOV to `tan(fov / 2)` on its type-0 projection
path. This proves the live angle input and viewport for this sample;
it does not yet assign the two remaining projection values, near/far planes,
depth range, clip-space Y direction, or a Vulkan projection matrix.

The unresolved values are now located rather than guessed. `CBufferingCamera`
uses its primary-render interface at component offset `+0x220` to select a
render context, then asks that context for a five-float projection block. The
sample's primary vtable was relocated `0x02574DF0`; its selector at `+0x0C`
is only a table lookup keyed by two still-unnamed inputs. No virtual call was
made by the probe. Therefore the block's values cannot yet be assigned as
near/far or reused by the Vulkan adapter.

The runtime render model now retains this evidence as `RawCameraProjection` on
`CameraSnapshot`: the viewport rectangle, projection selector, exact FOV bits,
and all five context words travel through the backend-neutral boundary without
changing the current Vulkan projection. Its float accessors deliberately leave
the context fields unnamed; preserving a raw ABI value is safer than encoding a
near/far or clip-space convention before it is demonstrated.

The next downstream boundary is now also identified. In the canonical static
`CBufferingCamera` vtable (`0x10066344`), slot `+0x4C` is Terrain RVA
`0x4D9C0`; it packages the camera transform, rectangle-derived viewport data,
FOV-derived tangent, and primary-render context values for another renderer
component. It does not itself assemble the final projection matrix. The GOG
`Ngi32.dll` export named `vrtSetCameraParam` resolves to a no-op compatibility
stub, while the live process loads the Direct3D7 runtime (`D3DIM700.DLL`). The
next recovery target is therefore the Direct3D transform consumer in
`iron3d.dll`, not a guessed Vulkan perspective formula. The observed `0.5`,
`700`, `0.1`, and `0.99` values remain unlabelled until that consumer proves
their roles.

### Ngi32 Direct3D7 camera matrices recovered

The downstream renderer is now concrete. `iron3d.dll` obtains an opaque
render interface through `Ngi32!niGet3DRender`; Ngi32 RVA `0x5640` returns the
global renderer object at RVA `0x3A460`. Its constructor at RVA `0x5E10` gives
the `0x8CC`-byte object vtable `0x100315E0`. During device setup, Ngi32 RVA
`0x8E70` calls the Direct3D7 device vtable at `+0x2C` (`SetTransform`) with
state `3` for the matrix made by RVA `0x7030` and state `2` for the matrix made
by RVA `0x9450`: projection then view.

RVA `0x7030` exactly builds the row-major D3D7 projection from renderer FOV
`f`, near `n`, far `z` and viewport width/height `w`/`h`:

```text
[ cos(f/2),         0,                 0,          0 ]
[        0, w/h * cos(f/2),            0,          0 ]
[        0,         0, sin(f/2)*z/(z-n), sin(f/2) ]
[        0,         0, -sin(f/2)*n*z/(z-n), 0 ]
```

The sine in the depth scale and homogeneous-W term is intentional: after the
D3D perspective divide the diagonal has the expected cotangent scale and the
depth range remains finite. RVA `0x9450` applies a
specific axis permutation/sign change and translated dot products to selector
0 before the view `SetTransform`; it is not merely the generic affine inverse.

An elevated read-only AutoDemo probe sampled a 1024×768 renderer with
near=`0.5`, far=`700`, FOV=`1.3` radians and showed its view-source pointer
byte-identical to the active Terrain outer camera's selector-0 block. This
proves ownership and the D3D7 boundary, while keeping the earlier
`CBufferingCamera` FOV=`1.04` as a distinct upstream interface value.

The live GOG layout is now independently repeatable without invoking any game
method: `Terrain.dll + 0x7355c` contains the current outer camera-object
pointer, whose vtable relocates to `Terrain.dll` RVA `0x665b4`. Selector 0
returns the affine block beginning at outer `+0x20`; its sixteen row-major
words have translations at indices 3, 7 and 11. The public `LoadCamera`
interface at outer `+0x134` is a different view and must not be substituted
for that block. Captures for `RawCameraTransform` therefore contain the sixteen
selector-0 words from outer `+0x20`. A passive AutoDemo sample produced a
finite affine camera matrix and passed through the offline Vulkan adapter as
source-world geometry, rather than relying on a guessed identity view.

The Vulkan static path now accepts this recovered camera as
`VulkanStaticCamera`. It composes the row-major D3D7 result as
`view * projection`, uploads its 64 bytes through a vertex-stage push constant,
and changes static vertices from XY to XYZ. GLSL reads the same bytes as its
default column-major `mat4`; therefore `matrix * vec4(position, 1)` is the
transpose-equivalent of the original D3D7 row-vector multiplication. Both APIs
use the 0..1 depth range, and this renderer uses a positive viewport height, so
this narrow bridge needs no extra depth or Y correction. The fragment alpha
cutoff moved to byte offset 64 in its own push-constant range. The default
identity camera deliberately preserves the old XY diagnostic preview; feeding
live terrain camera/projection values into mission rendering remains later
runtime wiring, not a claim of scene parity.

The geometry side of that handoff is also explicit. The static mesh adapter now
has source-world projections for `Land.msh` and MSH: terrain positions retain
all three decoded coordinates; model positions retain Z and apply the recovered
static mission placement. The diagnostic XY counterpart uses that same placement
before normalizing X/Y, so it no longer silently omits authored object rotation.
These APIs preserve triangle order, batch ranges and packed UV decoding. The
current game preview deliberately stays on its XY diagnostic projection until a
runtime supplies a real legacy camera; selecting source-world coordinates with
identity camera would be a misleading empty/off-screen viewer rather than a
compatibility result.

Static analysis of GOG `iron3d.dll` recovers the placement primitive at RVA
`0x36610`: it evaluates a row-major affine `Rz(z) * Ry(y) * Rx(x)` and writes
translation in the final column. The backend-neutral
`LegacyIron3dEulerTransform` preserves that finite portable formula with golden
yaw and scaled-point tests. A read-only elevated AutoDemo scan closes the
previous TMA binding gap for static placement: it finds the exact raw mission
records (position, `(0, 0, z)`, scale) and live object blocks at the same
world positions. For example, `z = 1.6262363` produces the observed pair
`-sin(z) = -0.99846`, `cos(z) = -0.05541`; matching pairs occur for the other
observed objects. The opt-in captured-camera path therefore applies
`R * (scale * local_position) + translation` before Vulkan upload. It remains
a static-placement contract: dynamic/animated transforms, physics ownership,
terrain material selection, lighting and gameplay parity are still separate
evidence tasks.

`VulkanSmokeRenderer::set_camera` now accepts a new finite camera between frame
submissions and uses it for the next command buffer without rebuilding the
swapchain, buffers, descriptors or pipelines. `VulkanStaticCamera` also accepts
generic finite row-major view/projection pairs, so the final game camera
controller need not be coupled to the D3D7 recovery type. The native Windows
smoke executes that update before each of 300 validation-clean frames. This is
the renderer-side cadence contract only; it does not yet supply a gameplay
camera controller or claim camera movement parity.

For controlled differential work, `fparkan-game --backend static-vulkan` now
accepts `--legacy-camera-capture <path>`. The JSON capture stores only the
selector-0 words and the already recovered Ngi32 viewport/near/far/FOV inputs;
the application reconstructs the D3D7 camera offline, selects source-world
geometry, and never attaches to or controls the original process. A live
read-only AutoDemo confirmation corrected an ABI ambiguity: the Terrain global
is the base of the `0x1A4` camera object (vtables at `+0` and `+4`), while
`LoadCamera`'s public return is the separate interface view at `base + 0x134`
with its own vtable. This capture format deliberately preserves the former
base-object interpretation and does not invent camera ownership.

A three-frame GOG AutoDemo run with one such read-only capture selected the
`legacy-d3d7-capture` path, rendered all eight mission roots / 66 MSH
components plus terrain, reported 5,226 vertices inside the recovered clip
volume, produced a 7,372,800-byte readback with FNV-1a
`1737487240747766901`, and ended with zero Vulkan validation warnings and
errors. This validates the offline handoff for one captured instant; it does
not claim live camera tracking, material parity, or an original-frame pixel
comparison.

`tools/capture-original-camera.ps1` makes that handoff repeatable for a live
GOG process. It finds the actual `Terrain.dll` base with Toolhelp module
enumeration, opens the chosen PID with only
`PROCESS_QUERY_INFORMATION | PROCESS_VM_READ`, verifies the expected outer
vtable, and reads exactly 64 selector-0 bytes at outer `+0x20`. AutoDemo can
briefly select a normalized/reflection-like camera object whose finite
translation is near the origin. The tool therefore re-reads the global on
each attempt and accepts a sample only when the proven translation length
exceeds its explicit `-MinimumWorldTranslation` threshold (default `100`);
the JSON retains the accepted outer pointer and translation for audit. It emits the
`fparkan-legacy-camera-v1` JSON accepted by `fparkan-game`; it does not write
to the process, send window messages, inject code, suspend threads, or invoke
an original method. PowerShell execution policy may require the explicit
one-shot bypass below; that changes neither the script nor the game.

```powershell
$capture = powershell -NoProfile -ExecutionPolicy Bypass -File `
  .\tools\capture-original-camera.ps1 -ProcessId 5632
$capture | Set-Content -NoNewline target\fparkan\live-camera.json
cargo run -q -p fparkan-game -- --root 'C:\GOG Games\Parkan - Iron Strategy' `
  --mission 'MISSIONS\Autodemo.00\data.tma' --backend static-vulkan --frames 3 `
  --legacy-camera-capture target\fparkan\live-camera.json `
  --readback-out target\fparkan\autodemo-live-camera.raw
```

On the unattended live AutoDemo the accepted world-space capture completed the
same all-root static bridge with 10,152 clip-visible vertices, 71 descriptors,
a 7,372,800-byte format-50 readback, and zero Vulkan validation
warnings/errors. A passive desktop-surface capture of that original window also
proved that the selected instant contains the textured terrain, atmospheric
sky, HUD and weapon FX; `PrintWindow` itself returns a black Direct3D buffer,
so it must not be treated as an original-frame oracle. The paired images expose
the next real renderer gap: sharing the raw pose and Ngi32 projection inputs is
not yet pixel parity, because the static bridge still lacks the full runtime
camera-selection timing, terrain composition, culling, lighting, UI and FX.

For visual regression work, `fparkan-game --backend static-vulkan` also accepts
`--readback-out <path>`. It writes the final synchronized Vulkan readback bytes
only after the renderer has completed its normal teardown evidence; the JSON
report records the raw Vulkan format, byte count, hash, and requested path. A
three-frame diagnostic AutoDemo run wrote 7,372,800 bytes at format `50`
(`VK_FORMAT_B8G8R8A8_UNORM`) with the same frame hash reported by the renderer.
The artifact is a Vulkan-side comparison input, not an asserted original-frame
image.

The first bounded launch showed that repeated fingerprinting, rather than
Vulkan initialization, was the load-path bottleneck: each new MAT0/TEXM request
re-opened an already decoded archive and re-hashed its entire source file.
`ResourceRepository::open_archive_unchanged` now makes the *explicit* bounded
loading-transaction contract available. The normal `open_archive` remains the
strict public path and still invalidates stale entry handles after an external
archive replacement; only mission asset preparation uses the transactional
variant because it consumes an immutable snapshot of the selected game root.

With that scoped cache, canonical GOG
`MISSIONS/Autodemo.00/data.tma --backend static-vulkan --preview-roots 1`
completed preparation in 6.308 seconds (the visual graph completed in 6.012
seconds) and produced a 1280×720 native Vulkan frame with the captured D3D7
camera, 15 material descriptors, and zero validation warnings or errors. This
is a real original-asset rendering milestone, but not pixel parity or gameplay
camera parity: it intentionally renders the bounded first preview root and
uses a one-time offline camera capture.

Expanding that same preview to all eight AutoDemo mission objects exposed a
different, renderer-local limit: the merged static scene crossed 65,535
vertices before any Vulkan command was recorded. `VulkanStaticMesh` now uses a
32-bit GPU index buffer (`VK_INDEX_TYPE_UINT32`), while source MSH/TerrainFace28
indices remain decoded in their original 16-bit representations and are widened
only at the static-render bridge. A regression test crosses the former boundary.
The full captured-camera AutoDemo preview completed at 13.369 seconds and
rendered three 1280×720 native frames: 8 mission objects, 66 mesh components,
67 material descriptors, and zero Vulkan validation warnings or errors. A later
orientation-enabled run rendered the same full scene for three 1280×720 frames
with `validation_warnings=0`, `validation_errors=0` and readback hash
`6406107678925513509`. It is still a static source-world preview: terrain
material selection, lighting, animation, physics and gameplay behavior have
not yet been claimed as parity.

A fresh no-input launch of the canonical `iron_3d.exe` did create a responsive
window titled `Parkan. Железная Стратегия`. A read-only probe then requested
`PROCESS_QUERY_INFORMATION | PROCESS_VM_READ` and attempted to read the known
Terrain global at image address `0x1007355C`; `OpenProcess` returned
`ERROR_ACCESS_DENIED` (5), so no process memory or camera value was read. The
window was left running. A same- or higher-integrity 32-bit debugger is still
required for live camera evidence.

`Land.msh` использует отдельный geometry-only bridge: validated `TerrainFace28`
сохраняет source triangle order, а его positions и packed UV0 попадают в тот же
static vertex/index upload path. Для текущего диагностического viewer XY bounds
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

Terrain material discovery is now reproducible instead of inferred from the
mesh shape. The original AutoDemo map places two textual sidecar WEAR tables,
`Land1.wea` and `Land2.wea`, beside `Land.msh`; `Terrain.dll` contains the
corresponding `1.wea` and `2.wea` loading suffixes. The new terrain inspector
reports all packed face tags. For `DATA/MAPS/AutoMAP/Land.msh`, its 3,174 faces
use `0x0102` (592), `0x0203` (386), `0xff01` (918), `0xff02` (970), and
`0xff03` (308). The low byte is always a valid positional selector 1..3 in
`Land2.wea`; the high byte is a `Land1.wea` selector or sentinel `0xff`.
This is evidence for a two-layer terrain-material contract, but not yet for
its blend equation, so the Vulkan bridge intentionally remains texture-agnostic
until the base/overlay composition is recovered.

Static recovery of `Terrain.dll!CLandscape` now fixes the loader contract behind
that observation. The constructor opens `Land.msh`, creates one material manager
from `Land1.wea`, then invokes that manager's second-table load with `Land2.wea`.
It requires NRes type 18 with the original diagnostic “microtexture mapping
chunk” and retains it as four-byte entries; type 14 is optional. Therefore the
two WEAR selectors must not be collapsed into an invented one-layer material,
and the type-18 `aux18` stream is now documented as microtexture mapping data.
`TerrainFace28::material_layers` exposes the evidenced high-byte `Land1`
selector (with `0xff` absent sentinel) and low-byte `Land2` selector. The
manager's actual blend/pass operation remains unrecovered. `World3D.dll`
supplies the manager ABI: its load slot appends `Land1` as table 0 and `Land2`
as table 1, while its material-phase lookup receives the exact 32-bit selector
`(table_index << 16) | material_index`. `TerrainMaterialSelection` preserves
that key for the renderer without assuming how the two phases are blended.

The same `World3D!GetMaterialPhase` recovery establishes the next boundary. It
validates the table/row selector, chooses a MAT0 phase (or interpolates two
phases for an animated material), returns the selected material record, and
hands its caller a 76-byte expanded phase-state block. It does not submit a
draw or compose the two terrain tables. The later consumer of that record and
the type-18 microtexture mapping remains the required evidence before adding
an overlay, blend, or microtexture shader.

The terrain render traversal is now located at `Terrain.dll` RVA `0x11340`.
After visibility/cell work, it derives a 68-byte source-material record and
dispatches `Terrain!GetShade()` slot `+16` with four arguments: zero, the
record's word at `+2`, a pointer selected from a four-byte table by its word at
`+0`, and zero. `GetShade` is a Terrain singleton, not `niGet3DRender`; the
preceding slot `+60` call sets its visibility mode. Slot `+16` consumes the
second word as the count of adjacent `u16` pairs and the first as the pair-table
index, then groups the pairs by material lookup before constructing draw work.
`TerrainSlotTable::render_dispatch` exposes exactly those two proven disk
fields, while `LandMeshDocument::slot_material_pairs` decodes the selected
type-11 entries as `{ material_lookup: u16, flags: u16 }`. Flag `0x0010` is a
proven batch boundary in `GetShade`. Crucially, this lookup does **not** select
either map-local Land table: the `GetShade` constructor loads its own one-table
manager from `system.rlb` resource `Shade.wea`. That WEAR table has one row,
`LIGHT1`, whereas AutoDemo's 3,174 pairs use keys `0..3173`; therefore the
16-bit value is an opaque manager lookup/cache key, **not** a table-row
selector. `TerrainMaterialPair::shade_lookup_key` records this separate key
without conflating it with `Land1.wea`/`Land2.wea`. The AutoDemo inspector now
reports 128 slots and 392 `0x0010` batch boundaries. The meanings of remaining
flags, this cache mapping, and the final blend operation remain unassigned.

The cache boundary is now mechanically separated from the WEAR loader. In the
`GetShade` object, the `Shade.wea` manager is stored at byte offset `3092`, but
the dispatcher calls a distinct object at byte offset `3244`. Its virtual slot
`+16` receives every type-11 key repeatedly with mode values `512`, `16`,
`20497`, and `20752`. The cache object itself has an eight-byte entry table at
`+316` and its exclusive key count at `+320`: the first dword of an entry is a
stored-record pointer and its byte at `+4` selects a 212-byte profile bank. A
key outside that count, or an entry pointer below `0x1000`, fails the lookup.

The static constructor now identifies this object as the embedded `CLandscape`
subobject whose vtable is `Terrain.dll` RVA `0x643D0`. It installs that vtable
at parent offset `+316`, allocates the entry table at parent `+632` (subobject
`+316`) as `0x1f40` bytes / 1,000 eight-byte entries, and initializes its count
to zero. The profile-bank region starts at subobject `+332`: every one of the
100 banks has a 116-byte header followed by a 96-byte initialized stream, hence
the recovered 212-byte stride. The byte selected from each table entry is
therefore a bank index into a constructor-owned, fixed 100-bank region. This
proves the cache's allocation layout. Its population path is also now located.
`Terrain.dll` RVA `0x10280` resets the count, recreates the base entries from
the 28-byte stream at parent `+756`, and assigns bank zero. RVA `0x12e20`
copies one `0xd4`-byte profile header into bank `profile_count`, increments that
count, and remaps supplied keys to 28-byte records through the entry table. A
mapped record is stored as `profile_record_base + 28 * record_index` with the
new bank byte; a mapping whose second index is `-1` stores only the profile
ordinal, below `0x1000`, and is deliberately rejected by the lookup routine.
This explains the runtime sentinel without interpreting it as a missing WEAR
row. The input map is not a direct `Shade.wea` decode: the sole call site is
the terrain loader at `Terrain.dll` RVA `0x0a3f0`, after it has generated and
stitched 28-byte terrain records. That loader first appends pairs copied from
an intermediate primitive-key list with second value `-1`, then appends one
`(-1, generated_record_index)` pair for every generated record. Before calling
the cache builder it runs a three-edge matching pass over those records, using
their quantized positions and adjacency selectors. The cache profile therefore
describes loader-produced geometry and its generated correspondence map, not a
named on-disk `Shade.wea` section. The source of the intermediate primitive-key
list and the exact semantics of the 28-byte record's non-position fields remain
unassigned; this is sufficient to rule out an invented static WEAR parser, but
not to reproduce the profile generator yet.

On success the method returns the shared result view at cache offset `+24`, not
the stored-record pointer and not a WEAR row. Thus the dispatcher's observed
flags at result-view `+60`, secondary selector `+24`, optional byte `+88`,
referenced state block `+104`, three signed shorts `+108`, and another selector
`+112` are materialized lookup state, not yet proven on-disk record offsets.
Mode `0x0200` (`512`) only validates the key in the recovered path; mode
`0x5011` includes the recovered selector-table transfer and mode `0x5110`
includes the recovered bank-byte transfer. The meanings of their remaining
bits, record population, and the result fields remain unrecovered, so the Vulkan
path must not map any of them to invented pipeline state.

A read-only elevated AutoDemo probe confirms that this is a lifecycle boundary,
not a permanently allocated table: the live `GetShade` singleton is initialized
and its `Shade.wea` manager pointer is non-null, while its byte-offset `3244`
cache pointer is normally null. Passive 100-ms sampling of the unattended
AutoDemo caught it briefly at `0x035C64B4`; its relocated vtable is
`Terrain.dll` RVA `0x643D0` and slot `+16` is RVA `0x10910`, matching the
recovered lookup routine. Therefore a future runtime implementation must treat
cache availability as contextual and cannot dereference or prebuild the cache
solely from constructor state.

The static Vulkan bridge now carries each contiguous face run as a separate
draw range keyed by the original packed `material_tag`; it neither reorders
triangles nor collapses the high byte. For the first usable visual layer, the
low byte resolves positionally through the adjacent standalone `Land2.wea`,
then the existing `MAT0 -> Textures.lib -> TEXM` chain supplies the RGBA8
descriptor. `Land1.wea` remains preserved as the high byte/overlay channel and
is deliberately not drawn until its blend equation is recovered. A canonical
three-frame AutoDemo run now uses 71 descriptors (66 MSH plus five terrain
tags), produces readback hash `17034026600547661445`, and stays at zero Vulkan
validation warnings/errors. This is original terrain texture upload, not
terrain visual parity.

```powershell
cargo run -q -p fparkan-cli -- terrain inspect `
  'C:\GOG Games\Parkan - Iron Strategy\DATA\MAPS\AutoMAP\Land.msh' --format json
```

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

### Live AutoDemo: empty frame устранён, но parity ещё не достигнут

Read-only `PrintWindow` capture работающего GOG AutoDemo даёт полноценный
оригинальный кадр (terrain, варбот и близкая geometry), поэтому теперь есть
безвводный source для будущего visual comparison. Текущий полный static-Vulkan
preview раньше состоял только из clear color. Validation `0/0` тогда доказывал
только GPU lifecycle, но не совпадение с original renderer.

Новая CPU-диагностика применяет к тем же source vertices ту же row-major D3D7
matrix, которую GLSL получает как column-major push constant. Для свежего
captured Ngi32 camera (`viewport 1024x768`, near `5`, far `700`, FOV `1.3`)
она насчитала `clip_visible_vertices=0`. Последующая сверка с
`Ngi32!sub_10009450` и `sub_10007030` нашла два literal translation defects:
Y translation view matrix должна быть `-(m23*m22 + m13*m12 + m03*m02)`, а
projection использует `width/height` и множитель `sin(fov/2)` в обоих depth
coefficients. После исправления того же capture диагностировал
`clip_visible_vertices=938`, а `PrintWindow` native Vulkan window показал
реальную rasterized terrain/model geometry. Это исключает прежний empty-frame
барьер; face culling и texture selection не были его причиной.

Получившийся кадр всё ещё явно не похож на оригинал: geometry сосредоточена у
границы и material/terrain representation грубая. Значит, следующие задачи —
camera timing/selection, source model-node transforms, terrain material phases,
lighting и visibility, а не объявление pixel parity.

MSH static bridge теперь также перестал отправлять каждый `Batch20` файла.
Для стандартного `Node38` он выбирает `slot_index[LOD0, group0]`, затем
соответствующий `Slot68.batch_start..batch_count`; alternate LOD/groups больше
не подмешиваются в initial static pose. Regression test фиксирует selection,
а fresh AutoDemo run меняет readback hash при тех же 938 clip-visible vertices
и остаётся validation-clean. Новый capture по-прежнему показывает сильное
смещение частей, поэтому одной selection недостаточно: нужны local node poses,
parent hierarchy и animation-key/fallback semantics. Модели без layout Node38
сохраняют прежний all-batches diagnostic fallback.

Чтобы следующий шаг не перечитывал raw NRes streams в renderer, validated
`ModelAsset` теперь переносит `ModelAnimation` с decoded type-8 keys, type-19
map и declared frame count. `node38_fallback_pose` возвращает exact fallback
key pose. Licensed gates Part 1/Part 2 подтвердили 435/511 models, 157/200
animated models, 3,469/5,233 node samples и утверждённые captures.

Следующий узкий шаг применяет этот уже декодированный `fallback_key` в
legacy-camera static preview. Для каждого выбранного `Node38` bridge поворачивает
вершины normalized quaternion, прибавляет local translation pose и только затем
применяет доказанный TMA `Rz * Ry * Rx`, scale и mission translation. Вершины
намеренно разворачиваются per node/draw range, чтобы один source vertex мог
получить разные poses. `parent_or_link_raw == 0xFFFF` теперь доказан как root,
а любое меньшее child значение — parent index: inspected `R_B_01.msh` содержит
34-node parent-before-child tree, а Part 1/Part 2 animation gate подтвердил
этот контракт для всех standard nodes. Parent rotation корректно поворачивает
child translation до суммирования. Frame map всё ещё не семплируется, поэтому
это static fallback hierarchy, а не full animation parity.
На той же GOG `MISSIONS/Autodemo.00/data.tma` и offline Ngi32 camera capture
full preview из 8 objects / 66 mesh components / 67 descriptors завершился за
один native Vulkan run с `clip_visible_vertices=3415`, `validation_warnings=0`,
`validation_errors=0` и readback hash `1275533143935640133`. Рост с 2584
видимых вершин подтверждает, что composed parent poses реально вошли в geometry
path; он не является оценкой сходства с original frame.

Параллельно bridge для legacy-camera path теперь переводит только высоту
`Land.msh` в world units с масштабом `1/32`: raw AutoDemo heights
`95.29412..288.23532` становятся `2.978..9.007`, что согласуется с TMA Z и
live camera height. Это доказанное coordinate conversion, но оно само по себе
не сделало вершины видимыми и не является заявлением о pixel parity.
