# IV. Мир, миссии и игровой runtime

Миссия в Iron3D не является готовым снимком мира. Она задаёт исходные данные:
маршруты, кланы, размещённые объекты, свойства, ссылку на ландшафт и
дополнительные записи. Runtime строит из этого карту, пространственные
структуры, очередь `World3D`, визуальные представления, controllers и связи с
ресурсной системой.

Для совместимой реализации важно не смешивать три слоя:

1. **Disk data** -- `data.tma`, `Land.msh`, `Land.map`, `BuildDat.lst` и
   связанные resource archives.
2. **Prepared data** -- разобранные paths, clans, terrain streams, areal graph,
   prototype graph, material и texture handles.
3. **Runtime objects** -- World3D instances, domain controllers, spatial
   registration, AI/scripts, timers и расчётный tick.

Граница между этими слоями нужна для диагностики и отката. Ошибка в достижимой
цепочке размещённого объекта должна остановить создание миссии до публикации
объекта в очереди событий. Недостижимая запись общего архива может быть
inventory warning и не обязана блокировать текущую карту.

## `data.tma`: данные миссии

`data.tma` -- основное описание расстановки и логической конфигурации миссии.
Он не содержит всю геометрию, материалы или AI-код. Файл перечисляет paths,
clans, objects, свойства и ссылки на внешние прототипы. Подробный справочный
контракт формата вынесен в [TMA](../reference/tma.md), но глава использует его
как часть сквозного runtime pipeline.

TMA читается строго последовательно bounded cursor-ом. Записи имеют переменную
длину, поэтому offsets следующих секций получаются только после разбора
предыдущих. Секции нельзя искать по сигнатурам: порядок управляется счётчиками,
длинами и mode-dependent ветками.

Главный критерий корректности -- `cursor.offset == file_size` после последней
записи. Неописанный хвост, переполнение при вычислении размеров, отрицательный
или чрезмерный count и выход за bounds являются ошибками parser-а, а не
материалом для эвристического восстановления.

### Верхний уровень

Все переменные строки в проверенных TMA используют length-prefixed primitive:

```c
struct LpString {
    uint32_t byte_length;
    uint8_t  bytes[byte_length];
};
```

Завершающий NUL не является обязательной частью framing. Reader продвигается
ровно на `4 + byte_length`. Текст можно декодировать как legacy ANSI/CP1251 для
человекочитаемого представления, но исходные bytes сохраняются для lossless
режима.

Подтверждённый верхний уровень:

```text
u32 format_version              // 1
u32 path_count
PathRecord paths[path_count]
u32 clan_section_version        // 6
u32 clan_count
ClanRecord clans[clan_count]
u32 object_section_version      // 10
u32 object_count
PlacedObject objects[object_count]
LpString land_path
u32 mission_flag
LpString description_raw
u32 extra_section_version       // 1
u32 extra_count
ExtraRecord28 extras[extra_count]
```

Имена `clan_section_version`, `object_section_version` и
`extra_section_version` описывают устойчивое положение полей в контракте. Они
не доказывают исходные имена C++-структур. Strict mode проверяет известные
значения, compatible mode сохраняет raw value и сообщает диагностический
контекст.

### Paths

```c
struct PathRecord {
    int32_t  path_id;
    uint32_t point_count;
    float    points[point_count][3];
};
```

Paths идут сразу после `path_count` без имён и padding. `path_id` не обязан
совпадать с физической позицией записи: script/gameplay reference должен
использовать сохранённый ID, а не индекс массива.

Перед выделением массива проверяются `point_count`, умножение `point_count *
12` и наличие всего диапазона в файле. Координаты хранятся как little-endian
`float32` triples в общей системе координат мира.

### Clans

Clan section задаёт участников миссии, их ресурсные связи, позиционные anchors
и таблицы отношений. Общая prefix-часть:

```text
LpString name
i32      raw_id
f32      anchor_x
f32      anchor_y
u32      mode
mode-dependent body
relation table
```

Для обычных modes `1..3` тело содержит две пары:

```text
LpString resource_path
i32      resource_tag
LpString resource_path
i32      resource_tag
```

После них идёт relation table:

```text
u32 relation_count
repeat relation_count:
    LpString other_clan_name
    i32      relation_value
```

Первая ресурсная строка обычно указывает на script/formula base, вторая -- на
TRF или пустой ресурс. Tags различаются между кланами и должны сохраняться как
raw-поля, пока их потребительская семантика не закрыта.

Mode `0` имеет отдельный count-driven layout:

```text
LpString first_resource
u32 spatial_group_count
repeat spatial_group_count:
    u32 record_count
    repeat record_count:
        float raw_spatial[5]
LpString second_resource
i32 second_tag
u32 relation_count
relations...
```

Внутренний `record_count` в известных живых образцах равен `1`, но parser читает
объявленное значение. Нельзя разбирать mode `0` как обычные две resource
references: это сдвигает cursor и ломает последующую relation table.

### PlacedObject и свойства

Ключевое поле размещённого объекта -- `resource_name`. Оно имеет два рабочих
варианта:

1. прямой логический ключ прототипа, который ищется в `objects.rlb`;
2. путь к unit DAT, из которого получается список компонентных ключей.

Доказанное framing объектной записи:

```text
u32      raw_kind
u32      class_or_flags
LpString resource_name
u32      raw_after_resource
u32      identity_or_clan_raw
f32      position[3]
f32      orientation[3]
f32      scale[3]
LpString instance_name
u32      raw_after_name
i32      link0
i32      link1
u32      property_schema_version    // 1
u32      property_count
Property properties[property_count]
```

`orientation[3]` названа по наблюдаемому использованию как transform-поле, но
точный Euler order должен подтверждаться pose/render parity. `scale` в
большинстве записей равен `(1,1,1)`. `instance_name` может быть пустым у
unit-ссылки или содержать stem размещённого прототипа.

Свойства хранятся как ordered property bag:

```text
Property:
    u32 raw_value[4]
    LpString name
```

Порядок, повторяемость имени и raw 16-byte value важнее удобного словаря.
Разные consumers интерпретируют четыре слова как integer, float, default или
range data в зависимости от имени свойства. Typed view допустим только для
доказанных property names; базовый parser обязан сохранить исходный порядок.

В раннем проверенном корпусе на каждом из 201 размещённого объекта встречаются
`Invulnerability` и `Life state`. Для 48 unit-ссылок дополнительно наблюдаются
`LogicalID`, `ClanID`, `Type`, `MaxSpeedPercent`, `MaximumOre`, `CurrentOre`,
`ChargeRadius`, `FreeBotNum`, `FreeTechnoNum`, `FreeConstructionTime` и
`FreeResearchTime`. Имя `NOT USED` встречается массово и сохраняется как
обычное поле, несмотря на исторический смысл названия.

### Epilogue и extras

После объектов идут путь к ландшафту, флаг миссии, raw-описание и trailing
section. `description_raw` не всегда является чистым текстом: внутри
объявленной длины встречаются служебные bytes и остатки путей. Поэтому decoded
view является вспомогательным, а не каноническим представлением.

```c
struct ExtraRecord28 {
    float    position[3];
    uint32_t raw[4];
};
```

Последние четыре слова `ExtraRecord28` пока не нормализуются. Reader хранит их
как raw data и не позволяет extra record поглотить начало следующей секции или
файловый хвост.

Покрытие полных каталогов:

```text
Часть 1: 29 TMA, 34 paths, 101 clans, 864 objects, 28 extra records
Часть 2: 31 TMA, 61 paths, 91 clans, 885 objects, 41 extra records
```

Версии стабильны: верхний уровень `1`, clan section `6`, object section `10`,
property schema `1`, trailing section `1`. У всех размещённых объектов
`class_or_flags == 0x80000002`.

## Сквозная загрузка миссии

`data.tma` описывает размещение, но видимый runtime-объект появляется только
после прохождения dependency graph. Простая загрузка файлов с похожим stem
работает на отдельных объектах, но ломается на составных unit DAT, изменённых
именах моделей и наследовании прототипов через `objects.rlb`.

Сквозная цепочка:

```text
TMA object
  -> direct prototype key или unit DAT
  -> component key
  -> objects.rlb entry
  -> MSH и WEAR
  -> material slots
  -> MAT0 phases
  -> Texm и lightmap
  -> prepared World3D instance
```

Контейнеры и графические форматы описаны отдельно в [NRes](../reference/nres.md),
[MSH](../reference/msh.md), [WEAR и MAT0](../reference/materials.md) и
[Texm](../reference/texm.md). В этой главе они рассматриваются как ребра
создания мира.

### Фазы loader-а

1. **Mission context.** Выбрать каталог миссии, прочитать конфигурацию и
   определить карту.
2. **World foundation.** Загрузить `Land.msh`, `Land.map`, `BuildDat.lst` и
   создать spatial managers.
3. **Mission description.** Разобрать TMA, paths и clans, но пока не публиковать
   объекты.
4. **Prototype resolution.** Для каждой размещённой сущности раскрыть прямой
   ключ или unit DAT и построить component list.
5. **Resource preparation.** Открыть требуемые RLB/LIB, проверить MSH, WEAR,
   MAT0, textures, lightmaps и effects.
6. **Instance construction.** Создать World3D objects и domain controllers,
   заполнить transform, ownership и properties.
7. **Registration.** Только после успешной настройки добавить instances в
   queue и spatial structures.
8. **Scenario start.** Подключить AI/scripts, активировать timers и разрешить
   первый calculation tick.

Разделение construction и registration предотвращает появление наполовину
созданного объекта в очереди событий. Если ошибка возникает до регистрации,
pending objects освобождаются без рассылки gameplay-событий. После регистрации
откат выполняется через обычный lifecycle очереди.

### Статистика dependency graph

Для ранних шести миссий 201 размещённый объект даёт 48 ссылок на unit-файлы и
153 прямых ключа. Unit-файлы раскрываются в 348 компонентов. Всего получается
501 запрос прототипа; для каждого достижимого запроса найдены запись реестра,
MSH и WEAR.

Полный dependency graph частей 1 и 2:

```text
Часть 1
864 placed objects
463 unit references -> 4 300 components
4 701 prototype/MSH/WEAR requests
36 954 material slots
48 806 texture requests + 139 lightmaps
failures 0

Часть 2
885 placed objects
561 unit references -> 5 521 components
5 845 prototype/MSH/WEAR requests
50 888 material slots
68 603 texture requests + 214 lightmaps
failures 0
```

`failures 0` означает, что для каждой достижимой ветви найдены prototype,
effective MSH/WEAR, MAT0, Texm и lightmap. Это не означает, что во всём
глобальном каталоге нет недостижимых или служебных записей.

Метрики нужно помечать областью. Чистая object chain шести ранних миссий даёт
3 873 material slots и 5 049 texture requests. Mission total включает по одной
environment WEAR-таблице на миссию и становится 3 879 material slots и 5 067
texture references.

### Диагностика ошибок

Ошибка привязывается к конкретному ребру графа:

- миссия ссылается на отсутствующий unit-файл;
- unit DAT раскрывается в component key, которого нет в реестре;
- prototype найден, но его MSH отсутствует в ожидаемом archive;
- WEAR указывает на неизвестный MAT0;
- MAT0 phase ссылается на отсутствующий Texm или lightmap;
- prepared object не прошёл валидацию transform/properties.

Сообщение вида `resource not found` недостаточно для восстановления каталога.
Диагностика должна содержать исходный placed object, раскрытый ключ, archive,
entry и тип связи.

## `Land.msh`: ландшафт как специализированная модель

`Land.msh` является [NRes](../reference/nres.md)-архивом, но его содержимое
отличается от обычной объектной MSH. Он хранит геометрию поверхности, таблицы
участков и ускорители пространственных запросов. Видимые buffers являются лишь
частью данных: CPU-подсистемам остаются нужны adjacency, surface classes и
cell accelerator streams.

Во всех проверенных картах порядок типов одинаков:

```text
1, 2, 3, 4, 5, 18, 14, 11, 21
```

Типы `1`, `3`, `4` и `5` совместимы по базовому представлению с узлами,
позициями, нормалями и UV обычной модели. Типы `11` и `21` специфичны для
terrain; `14` и `18` являются дополнительными потоками.

### Streams и размеры элементов

```text
type 1   38 байт   node/slot mapping
type 3   12 байт   float3 positions
type 4    4 байта  packed normals
type 5    4 байта  packed UV
type 11   4 байта  cell accelerator data
type 14   4 байта  auxiliary stream
type 18   4 байта  auxiliary stream
type 21  28 байт   terrain face
```

Для этих streams `attr1` соответствует числу элементов, а `attr3` -- stride.
Тип `2` начинается заголовком размером `0x8C`, после которого идут slot records
по 68 байт. Число slots вычисляется как `(size - 0x8C) / 68`; reader проверяет
делимость, bounds и отсутствие хвоста.

### `TerrainFace28`

Запись type `21` связывает triangles, соседей и surface metadata:

```text
+0x00 .. +0x07  flags и служебные поля
+0x08           u16 vertex0
+0x0A           u16 vertex1
+0x0C           u16 vertex2
+0x0E           u16 neighbor0
+0x10           u16 neighbor1
+0x12           u16 neighbor2
+0x14 .. +0x1B  material/class/edge fields
```

Каждый vertex index обязан быть меньше числа позиций type `3`. Neighbor равен
`0xFFFF` либо указывает на другой элемент type `21`. Последние восемь bytes
сохраняются без нормализации до полного закрытия предметной семантики.

### Маски поверхности

Runtime использует полную 32-битную маску face и два compact-представления.
Основное 16-битное поле собирается из отдельных битов полной маски; второе
шестибитное поле хранит material classes. Это не усечение младших битов.

Для совместимого writer-а нужны явные функции `full_to_compact()` и
`compact_to_full()`. Неизвестные биты полной маски сохраняются отдельно, иначе
обратное преобразование потеряет информацию.

Основное соответствие:

```text
full 00000001 -> compact 0001
full 00000008 -> compact 0002
full 00000010 -> compact 0004
full 00000020 -> compact 0008
full 00001000 -> compact 0010
full 00004000 -> compact 0020
full 00000002 -> compact 0040
full 00000400 -> compact 0080
full 00000800 -> compact 0100
full 00020000 -> compact 0200
full 00002000 -> compact 0400
full 00000200 -> compact 0800
full 00000004 -> compact 1000
full 00000040 -> compact 2000
full 00200000 -> compact 8000
```

Для шестибитного material-поля используются full-биты `0x100`, `0x8000`,
`0x10000`, `0x40000`, `0x80000` и `0x80`; они переходят соответственно в
compact-биты `1`, `2`, `4`, `8`, `0x10`, `0x20`.

### Проверенное покрытие

```text
AutoMAP   3 051 вершина, 3 174 faces
PROL     11 125 вершин, 9 234 faces
Tut_1     8 827 вершин, 8 290 faces
Tut_2     9 456 вершин, 8 996 faces
Tut_3     9 833 вершины, 8 560 faces
Tut_4     9 022 вершины, 8 612 faces
```

Расширенное покрытие:

```text
Часть 1: 33 карты, 299 450 vertices, 275 882 faces
Часть 2: 32 карты, 188 024 vertices, 184 454 faces
```

Во всех 65 картах порядок типов равен `[1,2,3,4,5,18,14,11,21]`. Strides,
count-driven размеры, vertex indices, neighbor indices и payload bounds
валидны. Различия карт являются различиями данных, а не новым вариантом
loader-а.

## `Land.map` и ArealMap

`Land.map` хранит логическое разбиение пространства на связанные области. Это
NRes-архив с одной записью type `12`. Payload содержит переменное число
ареалов, links и grid быстрого поиска.

Ареал -- участок мира с геометрической границей и метаданными. Граф соседств
позволяет искать маршрут между крупными областями вместо обхода каждой
terrain-вершины. Grid отвечает на быстрый вопрос: какие области потенциально
находятся рядом с координатой.

### Prefix ареала

```c
struct ArealPrefix56 {
    float anchor_x;
    float anchor_y;
    float anchor_z;
    float reserved_12;
    float area_metric;
    float normal_x;
    float normal_y;
    float normal_z;
    uint32_t logic_flag;
    uint32_t reserved_36;
    uint32_t class_id;
    uint32_t reserved_44;
    uint32_t vertex_count;
    uint32_t poly_count;
};
```

После prefix идут `float3 vertices[vertex_count]`. Нормаль в проверенных
записях имеет длину, практически равную единице. Поля `reserved_12`,
`reserved_36` и `reserved_44` в живом корпусе равны нулю, но writer сохраняет
их без нормализации.

### Links и polygon blocks

За вершинами хранится массив:

```c
struct EdgeLink8 {
    int32_t area_ref;
    int32_t edge_ref;
};
```

Пара `(-1, -1)` означает отсутствие соседа. Иначе `area_ref` указывает на
другую область, а `edge_ref` -- на соответствующее ребро. Число пар равно
`vertex_count + 3 * poly_count`.

После links для каждого polygon читается `u32 n`, затем block размером
`4 * (3*n + 1)` bytes. Во всех 65 проверенных картах `poly_count == 0`.
Framing ветки восстановлен по loader path, но предметное поведение polygon
blocks не получает статус corpus-verified.

### Grid быстрого поиска

После всех ареалов записаны `cellsX` и `cellsY`. Далее для каждой ячейки идут
`u16 hitCount` и `hitCount` номеров областей. Runtime уплотняет это в одно
32-битное значение: старшие 10 бит содержат число попаданий, младшие 22 --
начальный индекс в общем пуле.

Grid не является точной геометрической проверкой. Он возвращает короткий список
candidates, после чего выполняется проверка принадлежности области. При
загрузке каждый area ID обязан быть меньше общего числа ареалов.

Покрытие:

```text
Ранние шесть карт: 3 811 areals, grid 128 x 128
Часть 1: 33 карты, 34 662 areals, 197 698 areal vertices
Часть 2: 32 карты, 18 984 areals, 114 968 areal vertices
```

Во всех картах grid равен `128 x 128`. Максимальное число candidates в ячейке
-- 20 для Части 1 и 14 для Части 2. Все area/edge references находятся в
диапазоне, normals имеют единичную длину в пределах float32-погрешности, parser
заканчивается точно на конце payload.

## Пространственные задачи runtime

Движок решает три похожих, но независимых вопроса:

- **видимость** -- нужно ли рисовать объект для текущей камеры;
- **столкновение** -- пересекается ли движение с поверхностью или другим телом;
- **навигация** -- через какие области допустимо провести маршрут.

Terrain, Control и ArealMap используют общие координаты мира, но разные
структуры данных. Нельзя заменять навигационный граф видимыми triangles или
вычислять collision только по границе areal. Render frame описан отдельно в
[Render frame](../reference/render-frame.md); здесь важна подготовка world data,
которую renderer получает уже после загрузки миссии.

### Поиск области

Координата переводится в ячейку grid из `Land.map`. Ячейка даёт список
candidate areas, затем выполняется точная геометрическая проверка. Такой запрос
не перебирает все области карты и не зависит от количества terrain faces.

Если координата попадает в несколько candidates, выбор должен учитывать
геометрию boundary и class/logic flags, а не только первый ID из grid cell.
Если область не найдена, caller получает явный miss и решает, допустим ли
fallback к ближайшей области.

### Маршрут

После определения начальной и целевой областей маршрут строится по графу
соседств. Результат высокого уровня -- последовательность areal IDs. Из неё
формируется локальный corridor, внутри которого movement controller выбирает
конкретное движение по поверхности.

Такое разделение оставляет навигацию устойчивой к деталям terrain mesh:
изменение density triangles не должно менять high-level route, пока areal graph
и links остаются теми же.

### Категории зон объектов

`BuildDat.lst` связывает 12 имён категорий с 32-битными масками:

```text
Bunker_Small    80010000
Bunker_Medium   80020000
Bunker_Large    80040000
Generator       80000002
Mine            80000004
Storage         80000008
Plant           80000010
Hangar          80000040
MainTeleport    80000200
Institute       80000400
Tower_Medium    80100000
Tower_Large     80200000
```

Файл читается секционно. Неизвестное имя, дублирование или нарушенная структура
не должны тихо превращаться в нулевую маску. Нулевая маска является
диагностируемым состоянием, а не универсальным default.

## Создание мира

Инициализация карты должна быть staged pipeline, а не набором независимых
autoload-ов:

1. открыть `Land.msh` и построить geometry/spatial данные terrain;
2. открыть `Land.map` и создать areals, links и cell grid;
3. загрузить категории `BuildDat.lst`;
4. создать world managers для поверхности, областей, света и атмосферы;
5. разобрать TMA, paths и clans;
6. раскрыть object resources через unit DAT и `objects.rlb`;
7. подготовить MSH, WEAR, MAT0, Texm, lightmap и FXID dependencies;
8. создать World3D objects и domain controllers в pending state;
9. проверить cross references между components, controllers и spatial data;
10. зарегистрировать visual, physical и behavior components;
11. подключить AI/scripts и разрешить первый calculation tick.

Минимальный псевдокод объектной части:

```c
for (const PlacedObject& placed : mission.objects) {
    vector<string> keys = expand_resource_name(placed.resource_name);

    for (const string& key : keys) {
        Prototype p = registry.resolve(key);
        PreparedVisual v = prepare_visual(p);
        Object* o = construct_component(p, v, placed.properties);

        o->set_world_transform(placed.transform);
        pending_registration.push_back(o);
    }
}

validate_cross_references(pending_registration);
register_all(pending_registration);
```

`prepare_visual` использует явные ссылки прототипа и правила fallback ресурсной
системы. Она не должна угадывать модель по имени placed object, если prototype
уже задаёт другой effective MSH/WEAR.

## Инварианты реализации

- Reader всех count-driven структур проверяет overflow до выделения памяти.
- Parser TMA, `Land.msh` и `Land.map` завершает работу точно на конце своего
  payload.
- Неизвестные поля, reserved bytes, raw strings и property values сохраняются
  lossless.
- Object properties остаются ordered property bag; сортировка имён запрещена.
- Clan relations и area links проверяются на диапазон, но физический порядок
  записей сохраняется.
- Terrain vertex indices, face neighbors и areal references валидируются до
  публикации spatial managers.
- Достижимый missing resource останавливает mission load до регистрации
  объектов; недостижимая запись общего каталога остаётся диагностикой.
- Calculation tick включается только после успешной сборки terrain, areal graph,
  managers, object queue и scenario bindings.
