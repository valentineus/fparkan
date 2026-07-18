# VII. Руководство по полной реализации

Этот том описывает инженерный путь к совместимому движку FParkan. Он опирается
на доказанные форматы и runtime-контракты, но не требует повторять физическое
деление оригинала на пятнадцать DLL. Повторить нужно наблюдаемое поведение:
форматы, имена, fallback, object IDs, порядок событий, численную политику,
границы кадра, сохранения и воспроизводимость прохождения.

Предложенные ниже modules, handles, snapshots, queues и scheduler phases являются
целевой архитектурой новой реализации, а не восстановленным внутренним layout
оригинального Iron3D. Главная практическая цель: запускаться из неизменённого
оригинального каталога игры, проходить corpus gates для демоверсии, Части 1 и
Части 2, а затем измеримо двигаться от archive compatibility к полной игровой
совместимости.

## Целевая архитектура

Практичная форма новой реализации -- модульный монолит с узкими интерфейсами и
отдельными platform adapters. Внутренние границы должны соответствовать ролям
Iron3D, а не обязательно его DLL. Это упрощает перенос на современные платформы
и оставляет возможность поддерживать разные compatibility profiles для разных
сборок данных.

```text
application      запуск, окно, конфигурация, shutdown
platform         filesystem, clocks, input, threads, dynamic libraries
resources        NRes, RsLi, paths, archives, cache and diagnostics
assets           MSH, WEAR, MAT0, Texm, FXID and auxiliary formats
mission          TMA, unit DAT, prototype graph, scenario data
world            ObjectId, queue, lifecycle, time, messages, mirrors
terrain          Land.msh, Land.map, surface and spatial queries
navigation       areals, graph search, corridors
behavior         unit state machines, target and path requests
physics          control systems, collision proxies and contacts
animation        pose sampling, hierarchy and blending
audio            sample cache, sources, listener and buses
render           immutable frame contracts and modern backend
network          game message schema plus transport adapters
tools            validators, extractors, viewers, captures and editors
```

Каждый модуль зависит от нижележащих интерфейсов, а не от concrete managers.
Behavior видит `INavigation` и `IPhysicsCommandSink`, но не включает headers
renderer-а. Render получает immutable snapshot, а не mutable world. Network
receive не меняет мир напрямую: validated messages попадают в очередь следующей
calculation boundary.

### Центральные идентичности

Resource identity хранит и исходное написание, и нормализованный ASCII-key для
поиска:

```c
struct ResourceKey {
    NormalizedRelativePath archive;
    FixedAsciiName name;
    uint32_t type_id;
};
```

Normalization сохраняет исходную строку для diagnostics и roundtrip, а отдельный
ASCII-casefold key используется только для lookup. Эта граница важна для
архивов [NRes](../reference/nres.md), таблиц [RsLi](../reference/rsli.md),
prototype references и fallback-путей материалов.

Object identity разделяет внутреннюю защиту от dangling references и исходную
сетевую/script-семантику:

```c
struct ObjectHandle { uint32_t generation; uint32_t slot; };
struct OriginalObjectId { uint32_t raw; };
```

`ObjectHandle` нужен для безопасного внутреннего владения, deferred deletion и
weak references. `OriginalObjectId` сохраняет наблюдаемую семантику исходной
игры: scripts, mirrors, network messages и savegame references должны видеть
логический ID, а не адрес объекта или номер slot в новом allocator-е.

Frame snapshot отделяет simulation от render. Simulation пишет mutable state;
renderer читает опубликованное состояние или строго ограниченную фазу
`in_render`. Deferred deletion применяется между фазами, а не во время traversal.
Командный контур renderer-а должен сверяться с [описанием кадра](../reference/render-frame.md)
до pixel comparison.

### Владение ресурсами

Ресурс проходит несколько уровней:

```text
ArchiveHandle -> EntryView -> DecodedBlob -> ParsedAsset -> RuntimeResource
```

`EntryView` ссылается на metadata архива, `DecodedBlob` владеет подготовленными
bytes, `ParsedAsset` является CPU-представлением, `RuntimeResource` может
дополнительно владеть GPU/audio objects. Eviction верхнего уровня не закрывает
архив, если он ещё нужен другому entry. Ссылки идут вниз только через явные
handles.

Для shared objects допустимы reference counting или generation handles.
Intrusive refcount нужен только в ABI-shim; внутренний современный код
предпочтительно держит понятное владение и weak handles. Архивы, decoded blobs,
CPU assets и GPU resources имеют отдельные бюджеты и отдельные diagnostics.

### Backend adapters

Render, audio, input и network получают отдельные adapters. Compatibility state
живёт вне Vulkan, D3D11 или Metal backend; DirectPlay compatibility живёт
отдельно от modern transport. Так можно заменить платформу, не меняя форматы,
игровую семантику и regression corpus.

Backend adapter не должен быть местом, где исправляются данные. Если
[MSH](../reference/msh.md), [MAT0](../reference/materials.md) или
[Texm](../reference/texm.md) требуют fallback, это фиксируется в asset/runtime
слое и попадает в trace. Backend получает уже выбранные resources, states и
draw items.

### Scheduler phases

```text
collect_platform_events
build_input_snapshot
advance_game_clock
calculate_world_queue
apply_deferred_operations
update_navigation_physics_animation_fx
publish_render_snapshot
render_world
render_ui
end_frame_callbacks
maintenance_and_eviction
```

Фазы имеют стабильный порядок и запрещённые операции. Registry mutation
запрещена во время world traversal, GPU upload не изменяет simulation state, а
maintenance не влияет на gameplay. Script timers, material animation и FX
lifetime относятся к game time, если обратное не доказано.

Сначала реализуется однопоточный эталон. Параллелизм добавляется только внутри
фаз с детерминированным merge: decoding независимых assets, culling chunks или
подготовка immutable draw items. Это снижает риск скрытых race conditions и
расхождений replay.

### Структурированные ошибки

Каждая ошибка должна содержать фазу, путь, archive entry, object/prototype key,
offset и цепочку причины.

```text
MissionLoadError
  mission: Campaign.00/Mission.02
  object: 17
  resource_name: UNITS/.../unit.dat
  component: e_tur_...
  prototype: objects.rlb::e_tur_...
  cause: model archive missing
```

Логическое отсутствие необязательного lightmap, отсутствующий entry в архиве,
неизвестное opaque поле, выход ссылки за диапазон и повреждённый offset имеют
разный severity и разные способы исправления. Ошибка данных должна быть
actionable chain, а не строка вида `failed to load resource`.

## Порядок работ

Движок строится от данных к поведению и от детерминированных CPU-компонентов к
аппаратным. Каждый этап заканчивается исполняемым инструментом и тестовым
критерием. Нельзя начинать полноценный gameplay, пока ресурсный граф и
model/material path не дают воспроизводимый результат.

### Этап 0. Corpus harness

- индексировать оригинальный каталог и вычислить hashes;
- реализовать bounded binary cursor и structured diagnostics;
- создать CLI для массового запуска parser-ов;
- сохранять JSON-отчёт с counts, variants, warnings и failures;
- зафиксировать демоверсию, Часть 1 и Часть 2 как независимые baselines.

Готовность: повторный запуск на каждом неизменённом каталоге даёт идентичный
отчёт. Любой parser умеет завершиться контролируемой ошибкой с offset и
контекстом, а не crash или allocation по непроверенному count.

### Этап 1. Архивы и пути

- реализовать strict/lossless [NRes](../reference/nres.md) reader/writer;
- реализовать [RsLi](../reference/rsli.md) mapping, table transform, lookup,
  LZSS и Deflate;
- добавить адаптивный decoder для методов `0x080` и `0x0A0`;
- воспроизвести overlay и известные compatibility quirks;
- реализовать archive-handle cache и ASCII name policy.

Готовность: неизменённые архивы проходят byte-identical roundtrip; поиск всех
имён совпадает с каталогом; malformed corpus отклоняется без выхода за память.
NRes с ненулевым unindexed region обязательно остаётся regression case.

### Этап 2. Граф ресурсов

- разобрать `objects.rlb` и unit DAT;
- построить resolver прямой MSH, рекурсивного parent prototype через
  `objects.rlb` и отдельного BASE payload;
- реализовать dependency graph с reachability от миссии;
- добавить parsers CTPT, NDPR и остальных служебных форматов в lossless-режиме;
- создать инспектор прототипа, показывающий все связанные ресурсы.

Готовность: 201 demo-объект раскрывается в 501 прототип. Затем все миссии
Частей 1 и 2 дают 4 701 и 5 845 prototype requests без failures. Недостижимые
отсутствующие ресурсы отмечаются отдельно от критических ошибок в reachable
graph.

### Этап 3. Статический asset viewer

- реализовать [MSH](../reference/msh.md) core streams, slots и batches;
- декодировать Texm во все подтверждённые pixel formats;
- разобрать WEAR и [MAT0](../reference/materials.md) с точными fallback;
- построить современный renderer compatibility layer;
- добавить wireframe, normals, bounds, LOD/group и material debug views.

Готовность: открываются 435/511 моделей, 518/631 textures и 905/1 127 materials
Частей 1/2; batch/index bounds не нарушаются; viewer показывает корректно
текстурированную статическую модель из исходного архива. Красивый viewer всё ещё
означает только asset compatibility, а не готовую игру.

Текущее состояние репозитория нужно формулировать строже. `apps/fparkan-viewer`
сейчас является inspection CLI и synthetic command producer, а не live Vulkan
asset viewer. Реальный Vulkan в репозитории сегодня доказан только через
Stage 0 smoke triangle path; Stage 3 GPU vertical slice для оригинального
`MSH` + `Texm` + `WEAR/MAT0` + terrain остаётся блокером. Для различения
smoke, planning и live GPU путей используйте [таблицу правды renderer paths](../rendering/renderer_truth_table.md).

### Этап 4. Анимация и эффекты

- реализовать MSH type 8/type 19 sampling и hierarchy;
- добавить x87-compatible reference path для чувствительных формул;
- реализовать material phase animation;
- разобрать FXID header/commands и runtime instances;
- сначала поддержать все opcodes, встречающиеся в корпусе, сохраняя raw body;
- добавить deterministic RNG stream и effect capture.

Готовность: frame-by-frame poses совпадают с golden reference своей части; все
923/1 065 FXID создаются без parser errors; перезапуск одинакового effect seed
даёт идентичный список emitted primitives.

Текущее состояние репозитория опять же уже, чем целевой этап. В коде есть
portable reference sampler и детерминированный FX reference stub, но нет
runtime-captured parity для lifecycle/opcode semantics и нет Stage 4 rendered
acceptance поверх live Vulkan asset renderer. Поэтому rendered Stage 4 следует
считать заблокированным входным gate Stage 3, а parallel Stage 4 work вести
через captures, schemas и backend-neutral snapshots.

### Этап 5. Карта и мир

- реализовать `Land.msh` и corrected `TerrainFace28` layout;
- построить terrain rendering и CPU surface queries;
- реализовать `Land.map`, cell grid и graph links;
- визуализировать areals и найденные маршруты;
- разобрать [TMA](../reference/tma.md) и выполнять staged mission loading;
- создать World3D queue, ObjectId и deferred deletion.

Готовность: 65 карт и 60 TMA Частей 1 и 2 загружаются до EOF; все areal links
валидны; objects появляются в правильных transforms; мир выдерживает расчётные
шаги без рендера.

### Этап 6. Gameplay controllers

- подключить input snapshot и camera controller;
- реализовать navigation corridor, Behavior state machine и Wizard boundary;
- создать physical controller и collision manager;
- загрузить control resources в lossless typed model;
- внедрить game time, pause, event queue и end-of-frame callbacks;
- подключить AI layer и symbol/event layer сценариев.

Готовность: юнит получает цель, строит маршрут, движется по terrain, реагирует
на collision и исполняет базовые миссионные события в детерминированном replay.
На этом этапе вводится differential branch для изменённых `AniMesh`, `Control` и
`Effect`; неизменённые DLL используют общий reference path.

### Этап 7. Полный кадр, звук и UI

- реализовать render phases, sorting, lighting, shadows и atmosphere;
- подключить 3D listener, sample cache, FX sounds и mission audio;
- воспроизвести shell/UI loading и post-world pass;
- добавить frame capture до UI и после UI;
- зафиксировать capability fallback profiles.

Готовность: миссия визуально и звуково проходима; каждый draw и sound event
имеет trace; одинаковый replay создаёт одинаковые command lists. На этом этапе
вводится differential branch для `iron3d` и `services`.

### Этап 8. Сеть, сохранения и динамическая совместимость

- реализовать modern transport над versioned game-message schema;
- отдельно исследовать DirectPlay wire и `netZipData` для native compatibility;
- добавить mirrors, ownership transfer и disconnect cleanup;
- восстановить save/campaign state и dispatcher;
- выполнить динамические captures оригинала для render states, script VM и
  physics edge cases.

Готовность: одиночная кампания запускается из оригинального каталога,
сохраняется и продолжается; multiplayer replay согласован между peers; full
corpus не создаёт новых parser variants без явной регистрации.

## Тестовый контур

Совместимость нельзя подтвердить одним screenshot. Нужны тесты на уровне bytes,
структур, ссылок, simulation state, команд renderer-а и конечного изображения.
Каждый слой локализует свой класс ошибки.

```text
unit tests
  -> parser/property tests
  -> corpus validation
  -> cross-resource integration
  -> deterministic simulation replay
  -> render/audio command captures
  -> pixel and gameplay parity
```

Failure верхнего уровня всегда должен позволять спуститься к меньшему тесту и
понять причину.

### Unit, property и fuzz tests

Для каждого binary primitive проверяются little-endian чтение, bounded strings,
checked arithmetic и cursor boundaries. Для структур -- минимальный размер,
максимальные counts, пустые arrays, нулевые варианты и редкие branches.

Property tests генерируют случайные корректные NRes/RsLi/WEAR records,
выполняют encode -> decode и сравнивают семантику. Fuzz tests изменяют длины,
offsets, counts и termination bytes и требуют контролируемой ошибки без crash и
чрезмерного выделения памяти.

Критические алгоритмы имеют отдельные vectors: ASCII casefold, NRes permutation
search, RsLi byte transform, LZSS backreferences, quaternion shortest path,
matrix composition и terrain mask remap.

### Corpus validation

Каждый файл оригинального каталога проходит parser своего семейства. Отчёт
содержит hash, variant, counts, warnings, errors и точный offset сбоя. Baseline
демоверсии:

```text
MSH       435
MAT0      905
Texm      518
FXID      923
WEAR      457
Land.msh    6
Land.map    6
TMA         6
unit DAT  425
errors      0
```

Изменение parser-а принимается только если baseline остаётся стабильной либо
новый variant зарегистрирован с образцом и объяснением. Warnings должны быть
именованными: «неизвестное opaque поле» не равно «выход ссылки за диапазон».

### Cross-resource integration

Интеграционный тест начинается с миссии и проходит весь dependency graph:
object -> prototype -> MSH -> WEAR -> MAT0 -> Texm/lightmap/FXID. Он не
ограничивается тем, что файлы существуют: material slot должен указывать на
допустимый MAT0, phase -- на допустимую texture, model batch -- на существующий
WEAR index.

Demo mission total: 201 objects -> 501 prototypes -> 501 object MSH/WEAR.
Чистый object graph даёт 3 873 material slots и 5 049 texture requests; после
включения environment WEAR итог равен 3 879 material slots, 5 067 textures и
18 lightmaps, failures 0. Такой тест ловит ошибки casefold, suffix, fallback и
путей, которые отдельный parser не замечает.

Для каждого отсутствующего узла отчёт хранит полный parent chain, чтобы
различать broken global archive и реально достижимый mission failure.

### Deterministic simulation replay

#### Mission transform state in the world contract

`fparkan-world` now carries a `TransformState` for every live object: the
three TMA position words, three orientation words and three scale words are
preserved as exact IEEE-754 bit patterns. This stores source identity before a
movement or physics controller interprets axes, units or Euler order.
`WorldSnapshot` publishes transforms in stable object-handle order and the
canonical SHA-256 state hash includes every transform word.

Mission loading assigns this state after construction and before registration.
A headless licensed GOG AutoDemo run on 2026-07-18 loaded eight objects, 343
areals and 3,174 terrain surfaces with zero graph failures, then completed two
deterministic ticks. This is the state foundation for a future route/movement
controller; it does not claim recovered velocity, collision or original
behavior-controller semantics.

The ordinary planning renderer now consumes this snapshot state before falling
back to a mission draft. Its current transform bridge applies the preserved
position and non-uniform scale only; raw orientation remains uninterpreted in
this backend-neutral path. A GOG AutoDemo planning run on 2026-07-18 completed
two ticks with eight objects, 66 draws and state hash
`a54855a4f47ffa380911228f295dd49a9a7b88d6ff271a23db48ba318b1fbbb4`.

Записывается начальная миссия, seed, input events, network messages и значения
внешних часов. На контрольных ticks сохраняется canonical state hash:

```text
sorted ObjectId list
transforms and velocities
critical properties and owners
AI/behavior state IDs
active effect state
game clock and RNG states
```

Pointer addresses, allocator order и GPU handles в hash не входят. Два запуска с
одинаковым log должны давать одинаковый state hash на каждом checkpoint. Первое
расхождение гораздо информативнее финального разного результата миссии.

### Render command parity

До pixel comparison сравнивается command list:

```text
camera matrices and viewport
visible ObjectIds
render phase and stable order
model/node/slot/batch IDs
material phase and texture handles
legacy pipeline states
index ranges and transforms
```

Если command lists совпадают, но pixels различаются, проблема находится в
shader/backend, sampling или численной точности. Если command lists уже
различаются, pixel diff лишь скрывает более раннюю ошибку.

Golden captures следует хранить отдельно для статической модели, анимации,
terrain, transparent FX, shadows, lightmap и atmosphere.

### Pixel, audio и network tests

Pixel tests используют фиксированное разрешение, camera, device profile, seed и
timeline. Сравниваются exact pixels для CPU/reference path и tolerance metrics
для GPU path, но tolerance не должна скрывать переставленные прозрачные
primitives.

Audio tests сравнивают список sound events, sample IDs, positions, loop flags и
gains; waveform зависит от mixer/device и является вторичным уровнем. Network
tests воспроизводят captured message sequences, проверяют mirrors, ownership и
disconnect. Для native DirectPlay compatibility дополнительно нужен packet-level
corpus.

## Regression baselines

Corpus validation формирует три независимых отчёта: демоверсия, Часть 1 и
Часть 2. Каждый сохраняет manifest файлов, hashes executable/DLL, variants,
warnings, global archive health и mission reachability.

Ключевые corpus gates:

```text
NRes: 120 файлов / 6 804 entries и 134 / 8 171 для Частей 1/2
TMA: 29 миссий / 864 objects / 28 extras и 31 / 885 / 41
MSH: 435 и 511 моделей
MAT0: 905 и 1 127 материалов
Texm: 518 и 631 текстура
FXID: 923 и 1 065 эффектов
full reachability: 4 701 и 5 845 prototype requests, failures 0
```

Расширенные mission-reachability totals:

```text
Часть 1: 29 TMA, 864 objects, 4 701 prototypes,
         36 954 materials, 48 806 textures, 139 lightmaps, failures 0
Часть 2: 31 TMA, 885 objects, 5 845 prototypes,
         50 888 materials, 68 603 textures, 214 lightmaps, failures 0
```

Обязательные regression cases:

- NRes с ненулевым unindexed region;
- prototype inheritance через `objects.rlb`;
- unit DAT `description[32]` без NUL;
- TMA epilogue и `extra_count` 0--4;
- empty SWAV entry;
- stale save-slot metadata без payload;
- build-scoped RVA lookup.

Byte-identical asset comparison выполняется только внутри одного корпуса. Между
Частями 1 и 2 сравниваются semantic invariants и decoded representation,
поскольку многие assets пересобраны.

## Точность, скорость и повторяемость

Совместимый движок должен быть корректным, повторяемым и достаточно быстрым.
Эти свойства нельзя получать одним и тем же приёмом. Сначала создаётся простой
эталонный путь, затем он измеряется и оптимизируется без изменения результата.

Главные источники расхождений: x87 extended precision, преобразование float в
integer, порядок операций, старые SIMD implementations, нестабильная сортировка,
RNG и использование разных часов.

### x87 и округление

Оригинальный x86-код мог хранить промежуточные значения в 80-битных регистрах
x87, а в память записывать 32-битный float. Современный compiler чаще использует
SSE с округлением после каждой операции. Различие заметно на границах animation
frame, culling plane и collision threshold.

Для критических формул нужен reference mode:

- фиксированный порядок операций без reassociation;
- запрещённый fast-math;
- явные преобразования и проверенный режим округления;
- тесты возле half-integer и epsilon boundaries;
- при необходимости extended intermediate через `long double` на проверенной
  платформе.

Не требуется эмулировать x87 во всём движке. Нужно локализовать функции, где
малое отличие меняет дискретное решение, и держать для них scalar reference path.

### RNG как часть состояния

FX, atmosphere и, вероятно, AI используют случайные значения. Один глобальный
RNG легко расходится, если новая реализация запрашивает дополнительное число для
визуальной оптимизации. Для трассировки полезны именованные streams:

```text
world/gameplay RNG
AI/script RNG
FX instance RNG
atmosphere RNG
non-deterministic cosmetic RNG
```

Для native parity может потребоваться один общий алгоритм и точная sequence. До
подтверждения capture каждый stream хранит seed и счётчик вызовов в trace.
Cosmetic stream не входит в simulation hash.

### Стабильный порядок

Коллекции не должны зависеть от адресов, unordered containers или порядка
завершения worker threads. Для объектов, collision pairs, opaque/transparent
draws и network messages задаются явные stable keys:

- objects -- queue insertion sequence или OriginalObjectId;
- collision pairs -- упорядоченная пара IDs;
- opaque draws -- phase, pipeline key, material, stable insertion ID;
- transparent draws -- layer, quantized distance, stable insertion ID;
- network messages -- sequence и sender.

Даже когда математический результат коммутативен, side effects, cache accesses и
RNG делают порядок наблюдаемым.

### Часы и fixed-step

Monotonic platform clock хранится отдельно от game clock. Pause и time scaling
применяются к game clock. Simulation работает с фиксированным или точно
воспроизводимым шагом, а render может интерполировать presentation state, не
изменяя authoritative world.

Maintenance timers кэшей используют реальные часы или отдельную подтверждённую
шкалу; их срабатывание не должно менять gameplay. При перегрузке лучше выполнить
ограниченное число simulation steps и явно зафиксировать dropped presentation
frames, чем передать огромный `dt` в AI/physics.

### Оптимизация без потери эталона

1. Сохранить scalar reference implementation.
2. Добавить profiler counters на decoding, culling, sorting, animation, upload
   и draw.
3. Оптимизировать только измеренный bottleneck.
4. Сравнить SIMD/parallel результат с reference на полном corpus.
5. Оставить runtime switch для отключения оптимизации при диагностике.

`g_FastProc` удобно моделировать как таблицу function objects: все slots сначала
указывают на scalar path, затем безопасные slots заменяются SIMD-вариантами
после self-test на старте.

### Кэш и память

Архивы, decoded blobs, CPU assets и GPU resources имеют отдельные budgets.
Eviction разрешена только для объектов с нулевым external refcount и после
безопасной frame fence. Original delayed cleanup порядка десятков секунд можно
воспроизвести policy-параметрами, не сканируя все entries каждый кадр.

Основные показатели: число открытых архивов, decoded bytes, resident
textures/lightmaps, models, active FX, draw items и deferred-delete size. Любой
неограниченно растущий счётчик является regression. Производительность считается
достаточной только после корректности: стабильные 60 FPS с неверным LOD или
пропущенными эффектами не являются успехом.

## Release gates

Версия не выпускается, если:

- появился новый corpus error;
- изменился byte roundtrip неизменённых ресурсов;
- dependency graph получил failure в достижимом пути;
- deterministic replay расходится;
- command capture изменился без ожидаемого changelog;
- parser допускает allocation по непроверенному count;
- новая оптимизация не имеет scalar reference comparison.

Каждое исправление регистрирует минимальный regression asset или synthetic
vector. Если новый behavior намеренно отличается от предыдущего, изменение
должно иметь compatibility profile, corpus sample и объяснение, почему старый
baseline был неполным или неверным.

## Уровни совместимости

Слово «совместимый» используется только с уровнем:

1. **Archive-compatible** -- открывает и сохраняет контейнеры.
2. **Asset-compatible** -- декодирует модели, материалы, текстуры и эффекты.
3. **Mission-compatible** -- загружает карту и создаёт все объекты.
4. **Runtime-compatible** -- исполняет время, события, поведение и физику.
5. **Presentation-compatible** -- воспроизводит рендер и звук.
6. **Game-compatible** -- позволяет пройти миссии, сохраняться и продолжать.
7. **Native-interoperable** -- взаимодействует с оригинальной сетью и внешним
   ABI.

Viewer с красивой моделью находится только на втором уровне.

### Обязательные критерии запуска и данных

- приложение запускается из неизменённого оригинального каталога;
- относительные пути, регистр и legacy encodings разрешаются по исходным
  правилам;
- все требуемые NRes/RsLi открываются без предварительной конвертации;
- parsers проверяют границы и не используют неопределённые bytes как указатели;
- неизвестные поля сохраняются lossless;
- все mission-reachable prototype, model, material, texture, lightmap и effect
  references разрешаются;
- отсутствие необязательного ресурса следует документированному fallback, а не
  случайному default.

### Обязательные критерии мира

- TMA разбирается до точного EOF;
- `Land.msh` и `Land.map` создают корректную поверхность и areal graph;
- ObjectId, owner и mirror semantics устойчивы;
- queue traversal и deferred deletion безопасны;
- pause, game time и simulation steps повторяемы;
- AI/Behavior/Wizard/Control взаимодействуют через заданные границы;
- collision и navigation не подменяют друг друга;
- script events используют logical IDs и переживают удаление объектов;
- deterministic replay совпадает на контрольных ticks.

### Обязательные критерии presentation

- static и animated MSH используют правильные slots, batches и transforms;
- WEAR/MAT0/Texm fallback и phase timing совпадают;
- mip-skip, palettes, Page atlases и lightmaps работают;
- render phases, depth/cull/blend state и transparent order подтверждены
  captures;
- FXID commands и RNG дают устойчивый результат;
- camera и 3D sound listener синхронизированы;
- atmosphere, тени, солнце и flares не являются декоративными заглушками;
- UI и world rendering имеют правильную границу;
- golden command captures стабильны, pixel parity измеряется на фиксированных
  сценах.

### Обязательные критерии полной игры

- все доступные миссии стартуют, завершаются и корректно сообщают
  success/failure;
- campaign dispatcher сохраняет прогресс;
- savegame восстанавливает world, script, AI, RNG и clocks, а не только
  placement;
- input remapping, pause, camera modes, sound и настройки работают из UI;
- длительный прогон не накапливает objects, resources или audio sources;
- ошибки данных показывают actionable chain;
- производительность приемлема без отключения подсистем;
- демоверсия, Часть 1 и Часть 2 проходят один и тот же тестовый контур с
  раздельными manifests и эталонами.

### Native interoperability

Самый строгий уровень дополнительно требует совпадения x86 ABI экспортов, vtable
slots и calling conventions для подключаемых оригинальных модулей, а также
DirectPlay wire/framing и compression. Этот уровень независим от возможности
играть в новом standalone runtime.

Проект может честно заявлять game compatibility без native DLL/network
interoperability, но это должно быть явно указано. Аналогично pixel-perfect режим
может быть отдельным compatibility profile поверх функционально корректного
renderer-а.

### Совместимость нескольких наборов данных

Критерий полной совместимости применяется отдельно к демоверсии, Части 1 и
Части 2. Прохождение одного набора не позволяет заявлять поддержку остальных.

Обязательное различие:

- **format compatibility** -- один parser принимает все три набора;
- **content compatibility** -- конкретная миссия разрешает весь reachable graph;
- **behavior compatibility** -- runtime совпадает с соответствующей сборкой
  изменённых DLL;
- **cross-version support** -- один новый движок выбирает корректные данные и
  defaults по fingerprint установки.

Content fingerprint включает hashes executable/DLL и manifest ключевых архивов.
Он не используется для запрета модификаций, но выбирает compatibility profile и
делает отклонение диагностируемым.

## Definition of done

Полное документирование и реализация считаются завершёнными только когда каждый
критерий связан с главой спецификации, executable test и хотя бы одним
corpus/golden case. Утверждение без проверяемого критерия остаётся
исследовательской заметкой, а не контрактом.
