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

Статическая проверка подтверждает этот вывод без подмены его runtime-паритетом:
`Ngi32.dll` импортирует `LoadLibraryA`, `GetProcAddress` и `FreeLibrary`, но не
имеет статического импорта `DDRAW.dll`; в нём присутствуют строки `DDRAW`,
`DirectDrawCreate`, `DirectDrawCreateEx`, `DirectDrawEnumerateA` и
`DirectDrawEnumerateExA`, а также GUID семейств `IDirectDraw` 1/2/4/7,
`IDirect3D` 1/2/3/7 и соответствующих `IDirect3DDevice`. Следовательно,
современная реализация заменяет наблюдаемую fixed-function семантику над этой
границей, а не должна эмулировать COM-объекты DirectX. Равный SHA-256 Ngi32 в
Частях 1 и 2 зафиксирован в [таблице оригинальных модулей](../reference/original-binaries.md);
это поддерживает единый контракт Vulkan backend. RVA относятся к конкретной
сборке DLL: перед переносом адреса на другую установку сверяйте размер и
SHA-256 соответствующего модуля в этой таблице.

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

`slot_index` хранит 3 состояния по 5 LOD: `slot_index[state * 5 + lod]`;
`0xFFFF` означает отсутствие геометрии для комбинации. Публичные Rust-обёртки
сохраняют исторические имена `Lod` и `Group`: аргумент `Lod` задаёт нативное
состояние (0..2), а `Group` — нативный уровень LOD (0..4). Поле `parent_or_link`
участвует в иерархии или связи узлов, но название остаётся описательным.

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
    uint16_t sid = node.slot_index[native_state * 5 + native_lod];
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
    int16_t qw;
    int16_t qx;
    int16_t qy;
    int16_t qz;
};
```

На диске quaternion записан в порядке WXYZ; каждый signed component
декодируется делением на `32767.0`. Декодер переставляет поля в portable
`Pose.rotation = [x, y, z, w]` и оставляет их ненормализованными. Нормализация
выполняется при подготовке pose к sampling.

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

Статический Vulkan preview сначала строит начальную позу по CTLD `.ctl`.
В ней участвуют только включённые строки; при повторе node ID поздняя строка
заменяет раннюю, а узлы без настройки остаются на кадре `0`. Без
`--static-animation-frame` preview сэмплирует поддержанные Control timelines
при каждом новом сэмпле сцены. Флаг `--static-animation-frame N` задаёт `N`
всем узлам и пропускает runtime Control sample. Это визуальный preview позы,
а не полный Control/AI runtime.

MSH sampler выбирает type 19 frame округлением `(time - 0.5)` к ближайшему
целому с ties-to-even, затем интерполирует выбранный type 8 key и следующий
key в исходном `time`; это portable-вариант native x87 conversion в default
rounding mode, без обещания совпадения при любом x87 control word.

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

### Почему проём становится прозрачным вблизи

Портал здания — поверхность, которая закрывает интерьер издалека и постепенно
исчезает при приближении камеры. Если оставить ей постоянную непрозрачность,
вместо окна или перехода получится тёмный прямоугольник. Прозрачность материала
здесь заменяется значением, вычисленным для всего пакета треугольников; alpha
текстуры продолжает участвовать в смешивании.

Для MSH без потока цветов type 16 правило включает флаг batch `0x0008`.
В обычной ветви при горизонтальном FOV `1.3` поверхность прозрачна до расстояния
75 и непрозрачна после 95; между ними alpha растёт линейно. При другом FOV обе
границы умножаются на `1.3 / FOV`. Расстояние берётся до первой вершины первого
принятого треугольника, а не до центра здания.

Флаг `0x0010` выбирает другую кривую: сравнивается квадратный корень уже
вычисленного расстояния, границы равны `2.6 / FOV` и `7.7999997 / FOV`.
Например, при FOV `1.04` поверхность на расстоянии 25 имеет alpha примерно
`0.5`, а на расстоянии 5 полностью исчезает. Флаг `0x0040` без `0x0010`
сразу задаёт нулевую alpha; при наличии обоих побеждает `0x0010`.

FParkan пересчитывает эту alpha при каждом обновлении камеры, после анимации
модели. Нулевое значение временно выключает диапазон индексов; при удалении
камеры диапазон восстанавливается. Подробности оригинального вычисления и
пока приближённого выбора вершины приведены в [описании кадра](../reference/render-frame.md#прозрачность-порталов-msh).

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
888    packed 0xXXRRGGBB; little-endian bytes [B,G,R,X], RGBA output использует alpha 255
8888   packed 0xAARRGGBB; little-endian bytes [B,G,R,A], декодируется как RGBA
```

Formats 556 и 88 являются loader-confirmed, но не corpus-verified для
доступных игровых payload. CPU decoder расширяет короткие каналы до 8 bit через
повторение значимых bit, а не простым shift. Для 888 служебный четвёртый byte
сохраняется в raw mip, но не попадает в RGBA output.
Нативный код Ngi32 подтверждает layout: VA `0x10005930` упаковывает
`0xAARRGGBB` в ARGB4444. VA `0x1000F620`/`0x1000F70C` выбирают converter для
форматов 888/8888; mip pointer передаётся без изменений и перестановки каналов.
На диске little-endian байты имеют порядок `[B,G,R,A]` для 8888 и `[B,G,R,X]`
для 888.

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

Vulkan preview строит проецируемые тени для объектов, отбрасывающих тени,
отбрасывает сферы вне шести clip-плоскостей с радиусом `sphere + 100` и
сохраняет порядок видимых кандидатов; frame builder обрабатывает не более 20
объектов, отбрасывающих тени. Текущая реализация выделяет страницы в atlas
`256 x 256` с 28 page slots и использует одно сглаживание. Preview применяет
native defaults `RobShadowsOn = true` и `BldShadowsOn = false`: объект kind `4`
проверяет первый флаг, остальные kinds — второй. Поэтому роботы BTLU остаются
объектами, отбрасывающими тени, а объекты FORT и STAT их не создают. Preview
пока не загружает `shade.cfg`, поэтому значения из пользовательского файла не
меняют список объектов, отбрасывающих тени. Проверка действует только на
проецируемые тени: отдельный список объектов для затенения солнца по-прежнему
включает FORT, BTLU и STAT. Вызов preview передаёт `fade = 1.0` и LOD defaults:
minimum LOD `1` и detail multiplier `1.0` отдельно для зданий и роботов.
Проекция учитывает вклад directional и point lights.

Receiver projection обрезает projected quad по рёбрам треугольника. Native ray
query descriptor в `+0x10` требует наличия всех переданных bits, а descriptor
в `+0x14` исключает поверхности с любым переданным bit. Общий shadow query
использует full exclusion mask `0x2000`; actor four-ray path получает compact
mask `0x8008`, который landscape boundary разворачивает в full mask
`0x00200020`. Sun path передаёт full `0x20`, кодирует его как compact bit `8`
и затем получает тот же full bit после разворачивания. Hit record хранит в
`+0x20` квадрат евклидовой длины луча, поэтому native four-ray fade использует
`distance² * 1e-4`, а не линейную дальность.

Atmosphere manager создаёт world objects для фоновых и погодных явлений.
Отдельно подтверждены lightning, sun render, flare, `env_lightning`, rain
background sound и обязательные ссылки на lightning effect. Эти объекты
обновляются по игровому времени, но часть параметров зависит от camera: flare
требует screen position и occlusion test, rain -- области рядом с observer,
sound -- listener. Их нельзя один раз запечь в terrain.

Sun occlusion начинается от точки `camera + 0.5 * normalize(sun - camera)`;
этот offset является частью native query, а не произвольным bias renderer-а.

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
native_flag = (command_word >> 8) & 1
```

`native_flag` передаётся созданному native command object как режим/feature
flag и не отключает команду. Например, реальный `env_lightning` использует
`native_flag == 0` для opcode 3, opcode 1 и opcode 2, и все три команды
исполняются. Bits 9-31 являются частью данных и сохраняются. Между командами
нет выравнивания. Размер команды, включая word:

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

Offsets относятся к body команды, сразу после `command_word`. Native loaders
получают pointer на всю запись, поэтому record offset на 4 байта больше:

| Opcode | Body offset | Record offset |
| --- | ---: | ---: |
| 2 | `0x50` | `0x54` |
| 3 | `0x84` | `0x88` |
| 4 | `0x84` | `0x88` |
| 5 | `0x2C` | `0x30` |
| 7 | `0x8C` | `0x90` |
| 8 | `0xB4` | `0xB8` |
| 9 | `0x84` | `0x88` |
| 10 | `0x8C` | `0x90` |

Parser сохраняет все 64 байта пары, включая suffix после первого nul byte и
пустые поля. Декодированные имена заканчиваются на первом nul byte.
Извлечение dependency не подтверждает визуальную parity команд.

Имена сравниваются case-insensitive по ASCII. Resolve выполняется при создании
command object или лениво при первом запуске, но ошибка должна включать имя
эффекта, номер команды, archive и resource name.

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

В `fparkan-game` выбранные Control FX проходят отдельный transparent path.
Начальная CTL group запускает только строки без predicates, chain или state
gate. Type 4 point list задаёт локальную матрицу; текущие MSH owner и mounted
node poses входят в transform до локальной FX basis. Реализованные visuals
охватывают Op3 modes 0–2, Op4, Op7/8 particle pools и Op9 generated mesh. Op1
selector 1 добавляет point lights без native flag `0x80000000` в общий diffuse
список; остальные active records сохраняют flags для специальных путей и
projected shadows. Радиус источника масштабируется длиной его преобразованного
направления до нормализации. Например, локальный радиус 7 при масштабе узла
около 4,85 освещает область радиусом около 34. Потеря этого множителя оставляла
синий свет завода почти незаметным. Неположительный итог заменяется на 0,01.
Случайные добавки к цвету и радиусу пока явно считаются в
`render_coverage control_op1_jitter_unsupported`.
Effect renderer подавляет header bit `0x800` только при
render selector 0. Native AniMesh component path захвачен с selector 1, а в
его 53-entry manager присутствуют FX entries с header `0xD00`; preview
использует selector 1 и пропускает такие visual commands. Изменения manager
`A8`, которые задают runtime phase/header gate, пока не моделируются. Header
bit `0x10` относится к activation lifecycle и не выключает draw.

Для visual effect renderer использует MAT0, указанный самой командой. Его
phase выбирается по normalized FX phase, а не по wall-clock clock материала
мира. FX opacity остаётся в vertex alpha, MAT0 alpha для этой FX binding
фиксируется на единице, texture alpha остаётся в shader. Preview передаёт FX
через SourceAlpha/read-only-depth state. MAT0 phases проходят локальный
eligibility predicate additive-RGB approximation; native `flag40` и
directional-light branch остаются неразрешёнными.

Op9 генерирует индексированную полусферу с отдельными vertex и UV данными и
отправляет каждый quad и cap как отдельный draw range. Native packet distance
callback берёт первую индексированную вершину (`Terrain.dll:0x246A0` через
`0x247B0`), преобразует её согласно режиму packet и использует евклидово
расстояние до камеры. Queue размещает большие расстояния раньше меньших;
равный ключ вставляется в найденный binary-search midpoint, поэтому порядок
равных packets не стабилен по времени поступления. Preview повторяет этот ключ
и insertion order. Renderer category 0 остаётся highest-detail mesh; native
category вычисляется из projected sphere size и quality table, поэтому
фиксированная категория не является native LOD parity.

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


## Камера и размещение в оригинале

Миссия хранит положение, три угла и масштаб объекта. Функция `iron3d.dll`
RVA `0x36610` строит `Rz(z) * Ry(y) * Rx(x)` с переносом в последнем столбце.
Точка модели преобразуется как `R * (scale * point) + position`.
Повороты и переносы узлов модели применяются до размещения объекта в миссии.

Горизонтальная плоскость карты — XY, высота — Z. У GOG AutoDemo диапазон XY
ландшафта составляет `0..1190.6976`, а исходный диапазон Z в `Land.msh` —
`0..94.50981`. Проверенный native frame с той же камерой совпадает с силуэтом
ландшафта при прямом использовании исходного Z; деление на 32 опускало бы этот
горизонт примерно на 185 пикселей. Terrain, camera floor, shadow receivers и
sun-ray queries поэтому используют исходные координаты `Land.msh` без
преобразования Z.

`Ngi32!niGet3DRender` (RVA `0x5640`) возвращает графический интерфейс.
Вызов Direct3D7 `SetTransform` устанавливает projection из RVA `0x7030`
и view из RVA `0x9450`. Для FOV `f`, ближней плоскости `n`, дальней `z`
и размеров viewport `w`, `h` projection в порядке строк имеет вид:

```text
[ cos(f/2), 0,              0,                    0        ]
[ 0,        w/h*cos(f/2),   0,                    0        ]
[ 0,        0,             sin(f/2)*z/(z-n),      sin(f/2) ]
[ 0,        0,            -sin(f/2)*n*z/(z-n),    0        ]
```

После деления на однородную координату получается обычная перспективная
проекция с глубиной `0..1`. Матрица view дополнительно меняет оси и знаки;
она не равна одному лишь обратному преобразованию камеры.
В проверенном GOG контексте Ngi32 использовались `n=0.5`, `z=700`, `f=1.3`.

У GOG `Terrain.dll + 0x7355c` хранит указатель на активную внешнюю камеру.
Её selector 0 возвращает 16 слов начиная с `+0x20`, перенос находится в словах
3, 7, 11. Интерфейс `LoadCamera` расположен отдельно, по `+0x134` объекта.
Эти два указателя нельзя подменять друг другом. Формулы камеры и размещения
реализованы и проверяются в `fparkan-render`.

## LOD ландшафта

У `Land.msh` список slot-ссылок terrain хранится в 38-байтовых записях
Node38, а диапазоны граней — в 68-байтовых Slot68. В текущем runtime первые
две ссылки Node38 выбирают соответственно LOD0 и LOD1; каждая запись слота
задаёт начало и число граней в исходном массиве `TerrainFace28`. Все грани
проверяются на единственное назначение, а выходящий за массив, повторный или
неподдерживаемый диапазон считается ошибкой загрузки.

Первый рендер карты, terrain-запросы высоты и пересечения, receiver-ы теней и
обрезка контуром зданий используют только LOD0. Отбор сохраняет порядок
исходных граней, а контурный проход отдельно переводит индекс спроецированной
грани обратно в исходный. LOD1 пока не подставляется по расстоянию: такой
переход не реализован.

```rust
let assignments = mesh.face_lod_assignments()?;
for (face, assignment) in mesh.faces.iter().zip(assignments.iter()) {
    if assignment.lod != TerrainLod::Lod0 {
        continue;
    }
    // Build terrain query geometry from this source face.
}
```

Подготовка BAS вынесена в `fparkan-terrain::build_basement_foundations`.
Функция получает внешние и внутренние кольца в порядке размещения, строит
упорядоченные пересечения внешней границы с текущей поверхностью и возвращает
исходные фрагменты рельефа вместе с треугольниками отмостки в одном наборе.
`TerrainWorld::with_basement_foundation` принимает этот набор во владение и
устанавливает те же координаты и маски в BVH высотных запросов и пересечений.
Предпросмотр миссии на Vulkan пока строит только LOD0: связывание BAS-записей
с этой подготовкой ещё не подключено, поэтому наличие API не означает, что
геометрия миссии уже обрезана.

Внешнее кольцо должно лежать на области LOD0, а каждый его сегмент — оставаться
покрытым текущей поверхностью. Непокрытый сегмент или пересекающиеся контуры
дают ошибку; карта не остаётся частично обрезанной. Первичная нормализация
использует исходные грани и соседей LOD0, последующие пересечения и вырезания —
канонические фрагменты после предыдущих размещений. Соответствие динамических
соседей, создаваемых нативным движком, пока не моделируется.

## Материалы ландшафта

Рядом с `Land.msh` лежат `Land1.wea` и `Land2.wea`. Старший байт
`TerrainFace28.material_tag` выбирает материал первой таблицы, младший —
второй. `0xff` в старшем байте означает отсутствие первого слоя.
В AutoDemo встречаются теги `0102`, `0203`, `ff01`, `ff02`, `ff03`.
Менеджер World3D получает селектор `(table_index << 16) | material_index`
и возвращает выбранную фазу MAT0 размером 76 байт.

Type 18 в `Land.msh` оригинал называет microtexture mapping chunk;
он содержит четырёхбайтовые записи. Type 11 хранит пары `u16`, выбранные
таблицей terrain slots. Их первое поле — ключ shade cache, а не номер строки
WEAR. Флаг `0x0010` отделяет группы при обходе. `GetShade` использует собственный
материал `LIGHT1` из `system.rlb::Shade.wea`.

Shade cache в GOG `Terrain.dll` имеет vtable RVA `0x643D0`. Его таблица по
`+316` состоит из восьмибайтовых записей: указатель и номер банка. Число записей
находится по `+320`; банки начинаются с `+332`, их шаг — 212 байт. Lookup
RVA `0x10910` возвращает временное представление по `+24`, а не саму запись.
Построение в RVA `0x10280` и `0x12e20` использует созданную загрузчиком геометрию.
В рабочем Vulkan-пути `TerrainMaterialLayers` разрешает для каждого terrain slot
базовую, detail, overlay и overlay-detail фазы из WEAR/MAT0; их UV, alpha и
lightmap state передаются в `VulkanStaticMaterial`, а shader последовательно
сэмплирует эти четыре стадии. Animation clock обновляет выбранные фазы и
коэффициенты без пересборки terrain mesh, сохраняя native type-14 overlay mask.

## Реализация Vulkan

`LegacyPipelineState` хранит blend, depth, cull и alpha-test. Из этих значений
получается ключ Vulkan pipeline. Alpha reference передаётся отдельно через
push constant; он не требует нового pipeline.

Рабочий путь `fparkan-game` загружает terrain и все выбранные mission roots
(по умолчанию весь список объектов), разрешая локальные material slots через
WEAR и MAT0 до объединения геометрии. Индексы GPU имеют тип `u32`: вся карта
может содержать больше 65 535 вершин, даже если каждый исходный mesh использует
`u16`. Terrain передаёт в shader базовый, detail и overlay stages, lightmap и
animation phase; object batches сохраняют их legacy blend/depth/alpha state.

После загрузки world path создаёт интерактивную free-flight камеру (`WASD`,
`E/Q`, `Shift`, `Ctrl`, RMB relative-look), обновляет listener и на каждом
кадре собирает environment frame. Для каждого отрисовываемого строения
(корневого объекта класса `FORT`) и для каждого его отрисовываемого компонента
создаётся отдельная запись проверки. Она использует те же текущие матрицы узлов
и присоединённых компонентов, что и геометрия. После ограничения целевой
позиции по TerrainWorld камера проверяет непрерывный отрезок от прежней позиции
до цели; при пересечении
она остаётся в прежней позиции, сохраняя новый поворот. Повторная отправка
ожидающего кадра не применяет движение второй раз. Это ограничение
диагностической свободной камеры, а не правило игрового или следящего режима
оригинала.
Mission-local `sky.ske`/`sky.wea` выбирают
атмосферное расписание и sky materials, `effects.rlb/env_lightning` даёт
lightning visual; weather particles, flares, sun/moon, point lights,
projected shadows и audio events обновляются вместе с камерой и временем.
`--atmosphere-seconds` задаёт старт времени, `--frames 0` оставляет цикл
бесконечным, а `--preview-roots` служит только диагностическим ограничителем.
`--legacy-camera-capture` выбирает воспроизводимую камеру без free-flight
управления. `selector0_words` содержит локальную матрицу камеры. Если запись
содержит `effective_world_matrix_words` с маркером
`effective_world_matrix_space="world"`, preview использует эту мировую
матрицу. При `parented_camera=true` отсутствие мировой матрицы — ошибка;
локальный selector 0 остаётся legacy fallback только для записей без признака
parented и без мировой матрицы.
Если native helper явно применяет выбранную мировую камеру, setter работает в
mode 2, а restore возвращает исходную локальную матрицу в mode 1. Clip
параметры и native viewport RECT `[left, top, right, bottom]` идут из той же
записи. Путь сохраняет оси basis из view matrix вместе с roll, переносит
полученный `clip_from_world` в preview camera, переводит RECT в физический
drawable размер `(right - left) × (bottom - top)` и отклоняет несовпадение
размера при создании или resize вместо растягивания кадра. Результат нужно
сравнивать с тем же crop исходного PNG `[left, top, right, bottom]`. Контроллер
`FreeFlightCamera` в этом режиме не создаётся, но redraw продолжает обновлять
environment и projected shadows от captured camera. Перед environment update и
его audio events listener получает position, forward и up captured camera;
начальная сортировка world draw использует её же position. Phase capture может
содержать наблюдаемый mission path и проверенный `atmosphere_seconds`; viewer
использует эту абсолютную фазу по умолчанию, если не указан явный CLI override,
и удерживает sample атмосферы на ней через readback frames. Фазы FX следуют
выбранному control clock: без `--control-clock-ms` он обновляется по elapsed
time, а с явным значением readback frames переигрывают заданный clock. Это не
полное simulation time: `weather`, RNG и
остальное mission state остаются неизвестными, поэтому совпадение камеры и
фазы не доказывает visual parity с native frame.

Для сохранения кадра surface должен поддерживать `TRANSFER_SRC`. Renderer
копирует последний отправленный swapchain image в host-visible buffer и
читает его после завершения GPU. Размер raw-файла — `width * height * 4`.
Порядок каналов определяется выбранным Vulkan format; его необходимо учитывать
при преобразовании в обычное изображение.
