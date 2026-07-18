# VIII. Справочник и доказательная база

Восьмой том фиксирует, на чём держится книга: ABI, exports/imports, файловая
поверхность, статистика корпусов, открытые вопросы, критерии доказанности и
словарь терминов. Это самостоятельная справочная глава: она не заменяет
профильные статьи о форматах, но задаёт общий контракт, по которому проверяются
реализация, parser-ы, compatibility layer и будущие динамические эксперименты.

## Как читать доказательства

Доказательством считается наблюдение, которое можно повторить на конкретном
файле, сборке или трассе. Вывод может объединять несколько наблюдений, но он
должен сохранять происхождение данных: демоверсия, полная Часть 1 и полная
Часть 2 не смешиваются в один безымянный corpus.

Для каждого утверждения полезно различать четыре уровня:

- `layout-confirmed`: известны offset, size, count, bounds и правила безопасного
  чтения;
- `corpus-verified`: branch или вариант реально встречается в доступных игровых
  данных;
- `code-confirmed`: branch виден в бинарном коде, но отсутствует в доступном
  corpus;
- `behavior-confirmed`: поведение подтверждено исполнением оригинальной
  программы, трассой API/vtable или controlled differential test.

Если поле не имеет доказанного предметного смысла, документация хранит его как
opaque field. Это не мешает lossless read/write, но запрещает строить writer,
который очищает, переименовывает или пересчитывает такое поле на основании
правдоподобной догадки.

## ABI и границы модулей

### Базовый binary profile

Все исследованные модули -- 32-битные PE для x86, собранные C++-компилятором
эпохи MSVC6. Публичная граница сочетает именованные exports, фабрики C++-
объектов, singleton getters и дальнейшие вызовы через vtable.

Для binary shim необходимо учитывать:

- `__cdecl` и `__stdcall` у свободных функций;
- `__thiscall` у методов, где `this` передаётся в `ECX`;
- очистку stack, видимую по `ret N`;
- точный порядок virtual slots;
- multiple-interface pointer adjustments;
- 4-byte alignment и native little-endian types;
- отсутствие безопасного ABI для STL-контейнеров между современным и старым
  compiler-ом.

Внутренний новый движок не обязан использовать этот ABI. Он нужен только
compatibility layer, который принимает старые DLL-facing interfaces, старый
порядок slots и старые ownership rules.

### Публичная поверхность DLL

В 15 DLL обнаружено 313 exports:

```text
AniMesh.dll     2    ArealMap.dll    9
Behavior.dll    3    Control.dll     5
Effect.dll      2    Joystick.dll    6
MisLoad.dll     2    Net.dll        37
Ngi32.dll     145    Terrain.dll    13
Wizard.dll      1    World3D.dll    72
ai.dll          2    iron3d.dll      8
services.dll    6
```

Демоверсия содержит 1 126 imported function slots, а полные Части 1 и 2 --
1 134. Они включают Win32 runtime, DirectX и межмодульные связи. Большое число
exports `Ngi32.dll` состоит из активного объектного API, математических/resource
functions и legacy compatibility stubs.

Compatibility headers должны фиксировать symbol, ordinal, decorated или
undecorated name и signature конкретной сборки. Смысловое имя недостаточно:
порядок exports и calling convention входят в бинарный контракт.

### Композиционный и сервисный слой

`iron3d.dll` экспортирует восемь функций:

```text
createShell        deleteShell
createGame         deleteGame
createSubsystems   deleteSubsystems
getIGame           getIShell
```

`services.dll` публикует шесть getters:

```text
getDisplay
getGUIServer
getNetManager
getResManager
getSoundServer
getTimer
```

Эти getters возвращают shared interfaces. Caller не должен конструировать
concrete implementation или уничтожать singleton напрямую. Для совместимости
важны не только адреса функций, но и порядок startup/shutdown, owner/refcount
transitions и реакция на failure paths: отсутствие sound device, ошибка display,
прерванная загрузка миссии и normal shutdown.

### Предметные фабрики

```text
AniMesh:   LoadAgent, LoadAniMesh
ArealMap:  CreateArealMap, CreateSystemArealMap, GetSystemArealMap,
           CreateHallWay, CreateObjectFromScheme, CreateObjectsForDebug,
           CalcFullResearchCost, Debug_TestSchemeType, ShowDebugVector
Behavior:  CreateBehaviour, CreateDistributor, PressDebugKey
Control:   InitializeSettings, LoadControlSystem, LoadPhysicalModel,
           CreateCollManager, CreateCollObject
Effect:    InitializeSettings, CreateFxManager
MisLoad:   CreateMissionData, LoadResearch
AI:        CreateSuperAI, GetSuperAI
Wizard:    CreateWizard
Terrain:   CreateAtmosphere, CreateLightManager, CreatePrimitives,
           CreatePrimitives2, CreateShader, GetShade, GetWorld,
           LoadCamera, stdGetCurrentCamera2, stdSetCurrentCamera2
```

Фабрика возвращает interface pointer. Конкретный размер объекта и layout
остаются внутренними; внешнему коду важны vtable, QueryInterface-подобная
negotiation, lifetime methods и правила владения.

### World3D export families

72 exports `World3D.dll` группируются по назначению:

```text
lifecycle: stdInitGame, stdCloseGame, stdCalculateGame, stdRenderGame
objects: CreateObject, AddObjectToGame, AddNewObjectToGame,
         CreateMirrorObject, AddMirrorObjectToGame, AddNewMirrorToGame,
         DeleteGameObject, KillGameObject, CreateQueue, GetQueue
camera:  LoadCamera, stdSetCurrentCamera, stdGetCurrentCamera
input:   UpdateManualEventsList, ClearManualEventsList, stdClearKeyboard,
         converters, scan/string functions, key lock/query, mouse shift
clock:   SetGameTime, PauseGameTime, ResumeGameTime, GetGameTime family
network: netCreateNetWatcher, GetNetPlayerNum and mirror/player helpers
resources/render: material, texture, lightmap and end-of-render helpers
settings/state: CreateGameSettings, SetGameRender, SetStateForGameObjects
```

World3D является главным местом, где внешний ABI превращается в game loop:
input обновляет manual events, calculation проходит queue/world traversal,
deferred deletion откладывает фактическое уничтожение объектов, render читает
подготовленный snapshot, а end-of-render helpers закрывают временные ресурсы.

### Net и Joystick

`Net.dll` экспортирует создание instance/interface и 33 операции transport
lifecycle: provider/session enumeration, setup, create/join/close, player
operations, send/receive, latency, addresses, queue size, lobby и
`netZipData`/`netUnZipData`.

`Joystick.dll` имеет компактную границу:

```text
QueryJoy
CreateJoy
ReleaseJoy
SetJoyRange
PeekJoyMessage
GetJoyCaps
```

Эти модули легче всего заменить adapter-ами, потому что их публичная
поверхность достаточно узкая. Для native interoperability сохраняются исходные
signatures; modern runtime может использовать внутренние typed interfaces.

### Ngi32 export families

145 exports `Ngi32.dll` включают:

```text
resource archives: niOpenResFile, niOpenResFileEx, niOpenResInMem,
                   niCreateResFile, rsOpenLib, rsFind, rsLoad
renderer:          niGetD3DDriverAmount, niSelectD3DDriver,
                   niGetD3DDriverCaps, niGetD3DVideoModeList,
                   niCreate3DRender, niGet3DRender, niGetMaxTextureSize
audio:             niCreate3DSound, niGet3DSound, niGet3DSoundCaps,
                   niMuteSound, rsLoadWave
platform:          allocation, clocks, fixed-memory helpers
math/geometry:     plane, ray, polygon and volume intersection routines
CPU dispatch:      g_FastProc, niGetProcAddress and feature detection
legacy ABI:        n3d*, vrt*, bsp* compatibility entries
```

Экспорт переменной `g_FastProc` требует особого shim: consumer получает адрес
таблицы, а не результат функции.

### Подтверждённые RVA

Адреса указаны как RVA конкретной исследованной сборки:

```text
World3D stdCalculateGame    0x139A0
World3D stdRenderGame       0x13BD0
World3D sendEndOfRender     0x13D90
World3D UpdateManualEvents  0x10E10
World3D ClearManualEvents   0x11180
World3D DeleteGameObject    0x087B0
Ngi32 g_FastProc            0x3A058
```

`iron3d.dll` вызывает calculation около RVA `0x5FA94`, `0x604C1`, `0x6086B`,
render около `0x60B2F`, а manual-event update находится в Win32 message path
около `0xA3759`.

RVA используются только для сопоставления и трассировки этой версии. Runtime
implementation не должна встраивать их как постоянные игровые идентификаторы.
Таблица внутренних RVA хранится по SHA-256 конкретного модуля.

Операционные evidence-артефакты, которые должны оставаться синхронизированными
с кодом и acceptance, вынесены в отдельные страницы:

- [Hashes и import/export summary оригинального движка](../evidence/original_engine_hashes.md)
- [Stage 4 capture schema](../evidence/stage4_capture_schema.md)
- [Renderer truth table](../rendering/renderer_truth_table.md)

Подтверждённые hashes неизменённых DLL:

```text
World3D.dll 17e4a3089b2583a8cf2356c9db0390b1aba138356a09130d79b4e7e4791da61e
Ngi32.dll   bab9840d94f4e4e74ffc26677724fa896cf4823845504d09a9e025f80016edf5
```

Повторный headless IDA/Hex-Rays review GOG `World3D.dll` с этим hash уточнил
RVA export-ов: `stdCalculateGame=0x139A0`, `stdRenderGame=0x13BD0`,
`sendEndOfRender=0x13D90`, `stdSetCurrentCamera=0x13E60` и
`stdGetCurrentCamera=0x13E80`. Предыдущая таблица была сдвинута и не должна
использоваться для hooks или differential capture.

`stdRenderGame(camera)` сначала вызывает экспорт Terrain
`stdSetCurrentCamera2(camera)`, затем сохраняет текущий camera pointer в
глобальном состоянии World3D. После этого виден запрос camera interface через
selector `6`, запрос связанного service через selector `264`, renderer/world
boundary slots и traversal render queues. В конце pointer очищается; dispatch
end-of-render callbacks вынесен также в отдельный `sendEndOfRender`.
Это доказывает порядок передачи camera и границы frame lifecycle, но не layout
camera object, не матрицы projection/view и не значения viewport selectors.

Отдельная проверка GOG `Terrain.dll` (`AF87D1B2E728A0BE73C52BE3B44CC196AB46DA7799F25A15D40F8C9B0B425EAD`,
499 712 bytes) уточняет receiver side. `stdSetCurrentCamera2` находится по
RVA `0x4FD40`: при инициализированном Terrain он требует у переданного объекта
interface selector `18` и вызывает slot `+12` результата. Он **не** записывает
переданный pointer в `stdGetCurrentCamera2`. Последний возвращает Terrain global,
который внутренний initialization path устанавливает результатом selector `8` на
Terrain object. Следовательно, selector `18`, slot `+12` и global selector `8`
должны оставаться именованными evidence boundary до dynamic capture; считать
`stdGetCurrentCamera2` getter-ом переданной camera было бы ошибкой.

Повторный headless-IDA review той же GOG базы уточняет рабочий static contract
`CBufferingCamera`. Метод Terrain RVA `0x4D740` копирует ровно 64 байта
(16 dword) в component offset `+0x10`. Frame-preparation метод RVA `0x4D9C0`
получает viewport rectangle через virtual slot `+0x3C`, выводит width, height,
centre и aspect, а projection читает через camera interface. Для projection type
`0` он использует float по component offset `+0x234` в `tan(angle / 2)`; для
type `2` получает five-float block через slot `+0x70`; иной non-zero type
завершается `Not supported projection type`. Это доказывает зависимость
projection от live camera/viewport, но не row/column convention, единицы угла,
near/far mapping, handedness или initial camera selection. Важно, что строки
`ICamera::SetTransformMatrix` (RVA `0x4F830`) и
`ICamera::GetTransformMatrix` (RVA `0x4F850`) ведут только в obsolete-call
stubs и не дают usable ABI.

Live elevated read-only probe впервые подтвердил relocation-aware runtime связь:
`Terrain.dll` был загружен по `0x02510000`, global `base + 0x7355C` содержал
non-null `0x0B37DF08`, а первый dword этого объекта был `0x025765B4` — ровно
relocated `off_100665B4` из `LoadCamera` construction path. Это доказывает
live camera object с outer vtable, но одновременно исправляет прежнее слишком
сильное сопоставление offsets: raw read `global + 0x10` не дал finite 4x4 matrix,
а `+0x234` был zero в данном sample. Receiver static procedures `0x4D740`/
`0x4D9C0` и exported global ещё не доказаны как один layout без interface
adjustment; их offsets остаются unassigned до recovery selector relationship.

Локальная IDA-база уточняет адреса этой связи: `stdGetCurrentCamera2` — это
шестибайтный getter по RVA `0x4FD80`, который возвращает `dword` по RVA
`0x7355C`. Единственный найденный **direct static** initializer этого global — функция Terrain
по RVA `0x4D4D0`: она запрашивает selector `8` у своего `this` и сохраняет
полученный interface pointer. Это доказывает адрес хранения и путь заполнения,
но не разрешает трактовать pointer как конкретный layout камеры либо читать его
как runtime evidence без доступа к процессу на том же уровне привилегий.

Elevated live sampling теперь доказывает, что direct static xref не исчерпывает
runtime writers: за 25 секунд autoplay global переключился между тремя heap
pointer, все с relocated outer vtables `0x025765B4`/`0x02576558`. У двух объектов
paired blocks `+0x2C/+0x3C/+0x4C` и `+0x6C/+0x7C/+0x8C` синхронно несли
world-like translation, например `(491.562, 761.551, 7.361)`; третий давал
normalized-looking `(0.098, 0.018, 0.856)` и не совпадал с paired block.
Наблюдение согласуется с автоматическими camera switches, но не маркирует mode;
оно запрещает называть `0x4D4D0` единственным runtime writer и требует recovery
indirect/unanalyzed write path.

### Vtable и interface negotiation

Вызовы вида `object->vfunc(offset)` доказывают порядок slots, даже когда имя
метода неизвестно. Renderer slots около `+0x28`, `+0x30`, `+0x34` окружают
world traversal; camera и viewport получаются через selector-based interface
calls; shared objects используют ранний slot как AddRef-подобную операцию.

Правила реконструкции:

1. Зафиксировать byte offset slot и число аргументов.
2. Найти все call sites и типы передаваемых значений.
3. Отделить доказанное поведение от назначенного имени.
4. Построить C-compatible shim vtable с точным порядком.
5. Внутри adapter-а перевести вызов в современный typed interface.

Нельзя добавлять virtual destructor в начало reconstructed interface: это
сдвинет все slots.

### ABI-матрица Частей 1 и 2

Во всех пятнадцати DLL совпадают export names, ordinals и import sets. Общее
число exports остаётся 313. Обе полные части содержат 1 134 imported function
slots; значение 1 126 относится к демоверсии и хранится отдельно.

Побайтно идентичны девять DLL:

```text
ai.dll
Behavior.dll
Joystick.dll
MisLoad.dll
Net.dll
Ngi32.dll
Terrain.dll
Wizard.dll
World3D.dll
```

Пересобраны `AniMesh.dll`, `ArealMap.dll`, `Control.dll`, `Effect.dll`,
`iron3d.dll`, `services.dll`.

Изменение export RVA:

```text
AniMesh    2 / 2
Control    5 / 5
iron3d     8 / 8
services   6 / 6
ArealMap   0 / 9
Effect     0 / 2
```

Нулевое изменение export RVA не доказывает идентичность тела функции:
`ArealMap.dll` и `Effect.dll` имеют изменённый `.text` при прежних адресах
exports. Compatibility headers фиксируют внешний ABI один раз, но внутренняя
таблица адресов, тестов и semantic deltas выбирается по build fingerprint.

## Файловая поверхность

### Каталог как внешний API

Оригинальная установка -- не просто набор assets. Имена файлов, относительные
пути, регистр, конфигурационные ключи и разделение библиотек образуют внешний
контракт. Совместимый движок должен принимать каталог без переименования и
предварительной распаковки.

Основные root-файлы включают executable и 15 DLL, `Iron_3D.ini`, `Comp.ini`,
`Behavior.ini`, `ArealMap.ini`, `BuildDat.lst`, input/preload descriptions и
набор `.rlb/.lib` архивов:

```text
objects.rlb
system.rlb
static.rlb
effects.rlb
Material.lib
Textures.lib
LightMap.lib
Palettes.lib
sounds.lib
voices.lib
```

Parser конфигураций должен сохранять неизвестные keys и секции, поддерживать
quoted strings, хранить provenance значения и отличать absent key от explicit
default.

### `Iron_3D.ini`

Демоверсия содержит секции `[CS]`, `[MULTIPLAYER]`, `[TEMP]` и
`[LEVEL_RATIO]`.

```text
DISPLAY_WIDTH=640          DISPLAY_HEIGHT=480
BITDEPTH=16                CURRENT_D3DCARD=0
WINDOW_MODE=0              FORCE_SOFTWARE_CURSOR=1
RENDER_QUALITY=2           REFLECTIONS=0
EMBOSS_BUMP=0              EMBM=0
PLAY_CD_MUSIC=1            MOUSE_SENS=100
JOY_SENS=100               MOUSE_REV_Y=0
JOY_REV_Y=0                JOY_ENABLE=0
SUBTITLES=1
```

`FORCE_CD_SOUND` хранит строку пути. Multiplayer задаёт default IP, login и
password. `[TEMP]` содержит normalization и offence/defence ranges,
`[LEVEL_RATIO]` -- коэффициенты сложности `0.5`, `0.7`, `1.0`.

Parser не должен считать имена регистрозависимыми без отдельного
доказательства. Effective value, raw value и факт присутствия ключа хранятся
раздельно.

### `Comp.ini`: реестр компонентов

Формат строки:

```text
<CID> <DLL-name> <Function-name> [comment]
```

Подтверждённая таблица:

```text
0 terrain.dll LoadLandscape
1 terrain.dll LoadBuilding
2 terrain.dll LoadCamera
3 animesh.dll LoadAgent
4 animesh.dll LoadAgent
5 terrain.dll CreateAtmosphere
6 terrain.dll CreateShader
7 misload.dll LoadResearch
```

World3D использует этот файл как динамический component registry. Standalone
runtime может сопоставить CID внутренним фабрикам, но compatibility loader
должен поддерживать исходные DLL/function strings и комментарии `//`.

### `Behavior.ini` и `ArealMap.ini`

Demo `Behavior.ini` задаёт logging, debug rendering и controller switches:

```text
LogFile=Behavior.log       SaveLog=0
MaxErrorLevel=1            DefErrorLevel=2
LookBugMode=0              ShowVectors=0
NoZBuffer=0                LockBehaviour=0
UseDebugKey=1              GiveDefaultOrder=0
DefaultOrderPhase=10       DeterminMode=0
ImmortalHero=0             UseWizard=1
```

Код Behavior также ищет дополнительные `PathFind_*` и network parameters. В
demo-файле они отсутствуют, следовательно используются compiled defaults или
другой источник; нельзя приписывать им произвольные значения.

`ArealMap.ini` содержит log switches, `ShowAreals`, `Areal_NoZBuffer`,
`HallWay_NoZBuffer`, `EdgeUp` и `RunBehDebug`.

### Миссии, UI и сохранения

Типичный каталог миссии содержит:

```text
data.tma
mission.cfg
briefing.cfg
messages.cfg
```

`mission.cfg` -- текстовое описание именованных resource objects. Блок
начинается `object <name>`, содержит `desc`, `library`, `libtype`, числовой
`type` и произвольные именованные параметры, затем `end`. В демоверсии через
него определяются ambient music loops/variations и другие mission services.

`briefing.cfg` и `messages.cfg` относятся к пользовательскому представлению и
текстовым событиям. Binary TMA остаётся источником placement и properties; эти
файлы дополняют, а не заменяют его.

Отдельные поверхности:

```text
MISSIONS/SCRIPTS/*.scr, *.fml, *.trf, varset.var
MISSIONS/dispatcher.ini
ui/shell_ctrls.cfg
ui/menu_resources.cfg
ui/cursor.cfg
ui/game_resources.cfg
ui/hq.cfg
DATA/TextRes.cfg
SAVE/saveslots.cfg
```

Dispatcher демоверсии содержит секцию `[COMPLETE]`; полные части расширяют
campaign state и набор миссионных файлов. UI-config следует читать отдельным
generic object/config parser-ом, сохраняя порядок блоков и неизвестные fields.
`TextRes.cfg` связывает ключи с локализованными строками.

Save slot list не является полным savegame state. Для полной совместимости
нужно отдельно восстановить binary save payload, campaign dispatcher и
serialization world/script/AI/RNG.

### Правила файловой совместимости

- Поддерживать `/` и `\` во входных legacy paths.
- Разрешать paths относительно root игры и mission context.
- Сохранять исходное написание для log и roundtrip.
- Использовать ASCII case-insensitive lookup внутри архивов.
- Учитывать CP1251/ANSI строки там, где встречается локализованный текст.
- Не применять Unicode normalization к фиксированным resource names.
- Различать физически отсутствующий файл и отсутствующий entry в существующем
  архиве.
- Не требовать одинакового регистра имени файла на case-sensitive системах:
  resolver строит индекс каталога.

Все найденные конфигурации должны иметь schema с defaults, provenance и
признаком `present`. Это позволяет отличить исходный default от явно заданного
пользователем значения.

### Различия файловой поверхности Частей 1 и 2

Часть 2 добавляет `ui_factory.lib` -- NRes с шестью Texm entries.
`ui/minimap.lib` увеличен примерно с 6,95 до 10,10 МБ. `gamefont.rlb` и
`sprites.lib` побайтно совпадают между частями.

`Iron_3D.ini` Части 2 добавляет ключи `SFX_VOLUME`, `CD_VOLUME`,
`DEBUG_KEYS_ON`, меняет некоторые defaults (`MOUSE_SENS`, `MAP_ALPHA128`) и
локализует строки login/password. Это подтверждает правило schema +
provenance: parser хранит не только effective value, но и признак присутствия
ключа в конкретной сборке.

`BuildDat.lst` Части 2 использует более полные пути под
`UNITS\BUILDS\AI\...`; category masks при этом остаются логическим контрактом,
а physical path -- частью content profile.

`TextRes.cfg` и `TextRes.dll` значительно расширены. Localized text, resource
identifier и path normalization должны оставаться разными слоями: локализация
текста не меняет ASCII-casefold policy имён entries.

## Результаты проверки корпусов

### Demo baseline

Демоверсия содержит `iron_3d.exe`, те же 15 DLL и сокращённый набор
миссий/ресурсов. Все 15 DLL совпали с первоначально исследованными файлами по
SHA-256. Поэтому executable, бинарный код DLL и demo-assets относятся к одной
совместимой технологической сборке.

```text
modules: 16, из них DLL: 15
DLL exports: 313
DLL imports: 1126
DLL identity: 15/15
```

`iron_3d.exe`: 36 864 байта, PE32/x86, image base `0x400000`, entry RVA
`0x141E`, timestamp 28 июня 2001 года, SHA-256
`b0a8b0db1c3a8698c4d4604d89c655496bd91ac1f8859a455e8a45838aebfbd6`.

### Миссии и сквозные ссылки

Шесть TMA разобраны до точного EOF: суммарно 20 paths, 15 clans, 201 placed
objects и 1 extra record. 48 объектов ссылаются на unit DAT, 153 -- на прямые
prototype keys. Unit-файлы раскрыли 348 компонентов.

Сквозной результат:

```text
501 prototype requests   501 resolved
501 MSH requests         501 resolved
501 WEAR requests        501 resolved
3879 material slots      3879 resolved
5067 texture requests    5067 resolved
18 lightmap requests     18 resolved
failures                 0
```

Это самое сильное интеграционное подтверждение текущего корпуса: имена,
архивы, ASCII casefold и fallback согласуются между реальными форматами.

### Реестр и unit DAT

`objects.rlb` содержит 590 prototype entries:

```text
554 имеют прямую MSH-ссылку
549 прямых MSH разрешаются в demo-каталоге
34 раскрываются через родительский prototype и локальный BASE
7 не дают доступной геометрии
41 ссылка общего реестра указывает на отсутствующий demo-content
```

Негеометрические или неразрешённые глобальные entries:

```text
sun_01
sun_02
ws_al_01
ws_al_02
ws_fl_01
ws_hm_01
ws_hm_02
```

Они не входят в фактически требуемую цепочку проверенных миссий.

Проверено 425 unit DAT, 5 219 records, errors 0. Все records имеют kind 1 и
archive `objects.rlb`; в 5 205 name fields есть ненулевые хвостовые байты после
string terminator. Такой tail является данными, а не мусором, если цель --
lossless roundtrip.

### Модели

Проверено 435 MSH без errors/warnings; 157 анимированных. Диапазоны: 1-38
nodes, 1-112 slots, 12-9 686 vertices, 1-439 batches.

```text
414 моделей: types [1,2,3,4,5,15,13,6,7,8,19,9,10,17]
21 модель:   [1,2,3,4,5,18,15,13,6,7,8,19,9,10,17,20]
```

Type 17 непуст у 29 моделей; type 20 встречается у 21. Редкий variant type 1
найден в `system.rlb::MTCHECK.MSH`.

Повторная проверка terrain исправила layout face: vertex indices находятся с
`+0x08`, neighbor indices с `+0x0E`. Эта локальная проверка имеет приоритет над
ранними черновыми описаниями.

### Материалы и текстуры

Проверено 457 WEAR, 905 MAT0 и 518 Texm без ошибок. У всех MAT0 `attr2 = 6`.
531 материал содержит одну phase; максимальное число phases -- 29. У 860
материалов один animation block, у 43 -- два, у 2 -- восемь.

Распределение Texm по форматам:

```text
indexed 15
565     155
4444    59
888     52
8888    237
```

Форматы 556 и 88 присутствуют в loader-е, но не встречаются в demo-assets.
65 текстур содержат `Page`; размеры лежат от `8x8` до `256x256`. Все 385
уникальных texture references из MAT0 разрешаются.

### Эффекты

Проверено 923 FXID без ошибок. Наиболее часты команды 3, 7, 1 и 2. Команда 6 в
данных демоверсии не встречается. Наблюдаются режимы времени 0, 1, 2, 4, 5,
14, 15, 16 и 17.

### Карты

Шесть `Land.msh` и шесть `Land.map` проходят проверку без ошибок. Всего 3 811
ареалов; grid всегда `128x128`, максимальное число candidates в ячейке -- 10,
`poly_count` во всех записях равен нулю.

```text
AutoMAP  3051 vertices, 3174 faces, 343 areas
PROL    11125 vertices, 9234 faces, 731 area
Tut_1    8827 vertices, 8290 faces, 378 areas
Tut_2    9456 vertices, 8996 faces, 900 areas
Tut_3    9833 vertices, 8560 faces, 722 areas
Tut_4    9022 vertices, 8612 faces, 737 areas
```

Максимальное отклонение длины areal normal от единицы около `1.05e-7`.

### Вспомогательные форматы

```text
CTPT   284 resources, 3599 points, errors 0
NDPR   494 resources, 1915 records, errors 0
BASE    30 resources, errors 0
EXPL   144 resources, versions 1/2/3, errors 0
reference arrays 585 resources, 2956 records, errors 0
SUND     2 resources, 12 keys, errors 0
CTLD   531 payloads, errors 0
TRF      5 files, errors 0
preload 38 entries
ANI      8 resources
SKE      6 resources
```

CTPT names подтверждают attachment semantics: `TurretCenter`, `TurretDirect`,
`CameraCenter`, `TargetDirect`, `Root`, `Sfx`, `Width`, `Height`, `Dir` и
другие.

### Как читать статистику

Нулевое число parser errors подтверждает layout и диапазонные инварианты на
имеющихся variants, но не автоматически раскрывает предметный смысл каждого
opaque field. Отсутствие opcode или poly branch в corpus означает, что эту
ветку нельзя считать corpus-verified.

Особенно важно различать весь архив и достижимый runtime path. В `objects.rlb`
есть ссылки на вырезанный demo-content, однако шесть миссий не требуют их.
Поэтому quality gate имеет два отчёта: global archive health и mission
reachability.

### Полные каталоги Частей 1 и 2

Статистика демоверсии остаётся неизменной. Полные Части 1 и 2 образуют два
самостоятельных профиля с отдельными manifests, hashes и golden data.

Часть 1:

```text
files 1 017, bytes 197 056 957
NRes 120 / 6 804 entries
TMA 29 / 864 objects / 28 extras
unit DAT 425 / 5 219 records
objects.rlb 590 prototypes
MSH 435, MAT0 905, Texm 518, FXID 923
Land maps 33 / 34 662 areals
reachable prototypes 4 701
materials 36 954, textures 48 806, lightmaps 139
reachability failures 0
```

Часть 2:

```text
files 1 302, bytes 358 004 931
NRes 134 / 8 171 entries
TMA 31 / 885 objects / 41 extras
unit DAT 676 / 8 145 records
objects.rlb 683 prototypes
MSH 511, MAT0 1 127, Texm 631, FXID 1 065
Land maps 32 / 18 984 areals
reachable prototypes 5 845
materials 50 888, textures 68 603, lightmaps 214
reachability failures 0
```

Bootstrap Частей 1 и 2 идентичен. Девять DLL идентичны, шесть пересобраны при
сохранённом ABI. Активные NRes entries сравниваются так: 3 733 идентичны, 2 503
имеют изменённый payload, 1 934 добавлены в Части 2, 567 удалены. Это
показывает стабильность форматов при существенной переработке content,
особенно MSH, CTLD и FXID.

## Границы знания

### Закрытые или практически закрытые области

- Startup bootstrap и восемь exports `iron3d.dll`.
- Карта 15 DLL, exports/imports и основные interface boundaries.
- NRes layout, поиск и writer rules.
- RsLi header, table transform, lookup, mapping и используемые decode paths.
- TMA всех 60 проверенных миссий, unit DAT и `objects.rlb` resolution.
- MSH core/animation range contracts.
- WEAR, MAT0, Texm и FXID framing.
- `Land.msh`/`Land.map` и areal grid.
- World3D calculation/render order и deferred deletion.
- Сквозная mission-to-texture цепочка.

Полная проверка доступных каталогов усилила NRes active ranges, recursive
prototype inheritance через `objects.rlb`, bounded non-NUL unit descriptions,
полный TMA epilogue, extra records и Clan mode 0, MSH/MAT0/Texm/FXID variant
matrix Частей 1 и 2, 65 `Land.msh`/`Land.map`, полный reachable graph 60
миссий, stability matrix пятнадцати DLL, empty SWAV и stale save-slot metadata.

### Render-state и pixel parity

Доказан порядок frame boundaries, world traversal, material resolve и крупных
проходов. Не доказаны символами точные названия renderer vtable slots
`+0x28/+0x30/+0x34`, полный набор state transitions CShade и окончательный
взаимный порядок некоторых transparent/FX/shadow subpasses.

Pixel parity требует эталонных кадров оригинала с фиксированными camera,
timing, seed, разрешением и capability profile. Вместе с изображением
необходимо сохранять command/state trace; иначе pixel difference не позволяет
отличить ошибку формата от ошибки backend-а.

Минимальный capture должен фиксировать resolution, bit depth, selected driver,
device capabilities, camera matrices, mission, game time, seed, input log,
scene boundaries, transforms, render states, texture-stage states, texture
binds, viewport, clear, draw calls и `Blt/Flip`. Сначала сравниваются command
lists; pixel diff имеет смысл только после совпадения geometry/state sequence.

### FXID field-level semantics

Размеры команд, resource references, lifecycle, flags families и используемые
time modes известны. Не закрыто значение каждого поля body opcodes 1-10,
отсутствующий во всех проверенных каталогах opcode 6 и точные формулы редких
time modes.

Закрывающий эксперимент: создать инструмент, который изменяет по одному полю
копии эффекта, воспроизводить его в контролируемой сцене и логировать runtime
command object, emitted primitives и sound events. Одновременно reads в
`Effect.dll` сопоставляются с offsets body.

### Script VM

Сценарные packages, symbol names, event sections, variable declarations и
version check доступны. Полная instruction grammar `.scr`, semantics всех
opcodes и serialization состояния VM ещё не восстановлены.

План реконструкции:

1. Найти loader `.scr`, version check, границы bytecode, таблицы
   strings/symbols/events.
2. Найти dispatcher loop по повторяющемуся чтению opcode и indirect branch или
   jump table.
3. Для каждого handler определить instruction size, operands, чтения/записи VM
   state, stack effect, branch target и world side effects.
4. Hook-нуть dispatcher и писать запись `package,event,ip,opcode,raw
   operands,state before,state after,next ip`.
5. Построить disassembler и CFG; branch target обязан попадать на
   подтверждённую границу инструкции.
6. Закрывать opcode после статического handler contract, одного динамического
   trace и одного regression script.

После opcode table отдельно восстанавливаются serialization IP, call/event
frames, variables, timers и RNG.

### Physical/control formats

CTLD и связанные resources структурно читаются, count patterns и variants
известны. Не названы все секции, shape types, coefficients и точный contact
solver. То же относится к редким MSH types 17/20 и части CTPT/NDPR flags.

Закрывающий эксперимент: трассировать `LoadControlSystem`,
`LoadPhysicalModel` и создание collision objects на нескольких прототипах;
записать offsets, созданные shape instances и реакции на контролируемое
движение. Изменение одного resource field должно связываться с одним
наблюдаемым параметром.

### Сеть

DirectPlay lifecycle и имена игровых сообщений известны. Точные framing,
payload schema, reliability flags и алгоритм `netZipData` пока не подтверждены
записью сетевого обмена. Поэтому совместимость с оригинальным сетевым клиентом
ещё не доказана.

Для закрытия нужны два оригинальных клиента в изолированной среде и логирование
`netZipData`, `netUnZipData`, DirectPlay Send/Receive и World3D message
enqueue/dequeue. Native interoperability подтверждается только успешным
обменом original client <-> compatibility implementation в обе стороны.

### Редкие или отсутствующие corpus-ветки

- `Land.map poly_count > 0`: layout читается из loader-а, но ни одна из 65
  проверенных карт не содержит живой записи.
- RsLi adaptive methods `0x080`/`0x0A0`: decoder path известен, однако
  демоверсия и обе полные части их не используют.
- Texm formats 556 и 88: loader поддерживает их, но ни один проверенный Texm не
  использует эти значения.
- FX opcode 6: размер известен, однако живой command отсутствует во всём
  доступном corpus.
- Некоторые material flags и MSH auxiliary streams встречаются слишком редко
  для полного authoring contract.

Такие ветки реализуются строго по бинарному коду и synthetic tests, а статус
corpus-verified получают только после появления реального файла.

### Сохранения и campaign state

`saveslots.cfg` и `missions/dispatcher.ini` найдены, но полный бинарный
savegame payload, serialization World3D/AI/script/RNG и правила миграции версии
не восстановлены. Без этого нельзя честно заявлять полную campaign
compatibility.

Минимальный набор сохранений для каждой части:

```text
S0  сразу после старта миссии
S1  тот же state без simulation step
S2  изменена только позиция одного объекта
S3  изменено только здоровье/свойство
S4  активен один Behavior order/path
S5  активен один FX и timer
S6  изменена одна script variable
S7  изменён research/economy state
S8  перед/после mission completion
S9  pause и non-default game time
```

Без самих binary save payload возможно описать обязательный state и найти код
сериализации, но невозможно доказать disk layout и roundtrip.

### Shell, HUD, шрифты и локализация

Граница shell подтверждена экспортами `createShell`/`getIShell`, `IGUIServer`,
верхнеуровневым UI-pass и файлами `ui/*.cfg`, `DATA/TextRes.cfg`,
`gamefont.rlb` и `sprites.lib`. RsLi framing двух библиотек закрыт, но widget
tree, layout rules, font glyph metrics, sprite command semantics,
focus/navigation и полный HUD state machine пока не восстановлены до
field-level спецификации.

До закрытия новая реализация может построить функционально эквивалентный UI
поверх известных ресурсов, но не заявлять native layout/behavior parity.

### Исследования, экономика и игровые свойства

Экспорты `LoadResearch`, `CalcFullResearchCost`, TRF/preload resources и TMA
properties доказывают отдельный слой исследований, стоимости, добычи и
производственных параметров. Сквозные имена (`MaximumOre`, `CurrentOre`,
`FreeResearchTime`, `FreeConstructionTime` и другие) доступны, однако формулы
стоимости, dependency graph технологий, inventory/economy transitions и точная
типизация всех 16-byte property values не закрыты.

Закрывающий эксперимент: сопоставить `LoadResearch`/`CalcFullResearchCost` с
ресурсами и UI, снять изменения state на контролируемых покупках/исследованиях
и построить typed schema свойств по consumers, не по одному имени.

### Условия динамического этапа

Полное закрытие оставшихся вопросов технически возможно, но не только по
статическим архивам. Нужна среда, способная запускать оригинальный 32-битный
код, и набор эталонных наблюдений:

1. Изолированная 32-битная Windows VM или отдельная машина с исходными
   DirectDraw/Direct3D/DirectSound/DirectPlay interfaces.
2. Два неизменённых игровых каталога и manifest SHA-256 для executable, DLL,
   конфигураций и ключевых архивов.
3. Отладчик с hardware/software breakpoints, просмотром x87/SSE state и
   сохранением memory dumps.
4. API/vtable hooking для Win32 file I/O, DirectDraw/Direct3D, DirectSound и
   DirectPlay; hooks должны писать binary trace, не изменяя порядок вызовов.
5. Управляемые clocks, input log и RNG seed либо trace всех вызовов источника
   случайности.
6. Автоматический launcher, который восстанавливает snapshot VM, запускает один
   test case, собирает логи и завершает процесс без ручного вмешательства.

Для каждого capture сохраняются profile сборки, hash модулей,
mission/resource key, конфигурация, device profile, начальное состояние,
input/time script и версии инструментов.

### Критерий закрытия открытого вопроса

Для каждого открытого вопроса должны существовать:

- build fingerprint и адреса наблюдаемых функций;
- raw trace и автоматический parser trace-а;
- минимальный воспроизводимый input/resource/save/message;
- формальный контракт или явно ограниченная гипотеза;
- differential test для Частей 1 и 2, если модуль изменён;
- обновление тематической статьи;
- regression case, запускаемый без ручного анализа.

До выполнения этих условий статический контракт пригоден для реализации, но
утверждение о полном поведенческом или native-паритете не публикуется.

## Глоссарий

### Бинарные файлы и reverse engineering

**PE (Portable Executable)** -- формат исполняемых файлов Windows: EXE и DLL.
Он содержит заголовки, секции, таблицы импортов и экспортов, relocations и
адрес точки входа.

**Image base** -- предпочтительный адрес начала загруженного PE-образа.
**VA** -- виртуальный адрес в процессе. **RVA** -- адрес относительно image
base. Адрес функции в памяти обычно равен `image_base + RVA`.

**Import** -- внешняя функция или переменная, которую модуль получает из другой
DLL. **Export** -- символ, предоставляемый другим модулям. Имя, ordinal и
calling convention вместе образуют часть бинарного контракта.

**ABI** -- соглашение о двоичном взаимодействии: размещение аргументов, возврат
значений, очистка stack, layout структур, порядок virtual methods и правила
владения.

**Calling convention** -- часть ABI, определяющая передачу аргументов и очистку
stack. Для исследованного 32-битного кода важны `__cdecl`, `__stdcall` и
`__thiscall`.

**Vtable** -- массив указателей на virtual methods C++-объекта. Запись
`vtable +0x34` означает вызов указателя по байтовому смещению `0x34` от начала
таблицы.

**Static analysis** исследует файл без его исполнения: disassembly, strings,
imports, call graph и data flow. **Dynamic analysis** наблюдает работающую
программу: breakpoints, traces, API hooks, memory state и packet/frame captures.

**Evidence** -- наблюдение, которое можно повторить. **Inference** -- вывод,
объединяющий несколько наблюдений. **Hypothesis** -- рабочее предположение, ещё
не подтверждённое достаточным экспериментом.

### Форматы данных и ресурсы

**Archive** -- контейнер, объединяющий множество ресурсов. **Entry** -- запись
его каталога. **Payload** -- полезные bytes конкретной записи.

**Magic** -- короткая сигнатура формата, например `NRes` или `Texm`.
**Version** -- номер варианта layout. Проверка одной magic без проверки version
и размеров недостаточна.

**Offset** -- положение данных относительно начала файла или структуры.
**Size** -- число занимаемых bytes. **Stride** -- размер одного элемента
массива. **Alignment** -- требование начинать данные на address или offset,
кратном заданному числу.

**Little-endian** -- порядок, в котором младший byte многобайтного числа
расположен первым. Все основные числовые поля исследованных форматов Iron3D
используют этот порядок.

**Fixed-size string** -- поле заранее известной длины. Полезная строка
заканчивается первым NUL, но оставшиеся bytes поля могут содержать служебный
хвост и должны сохраняться.

**Opaque field** -- поле с доказанными offset и размером, но не установленным
предметным смыслом. Его безопасно читать и копировать, но нельзя очищать или
переосмысливать без эксперимента.

**Invariant** -- условие, которое обязано выполняться: диапазон находится
внутри payload, индекс указывает на существующий элемент, число записей
соответствует размеру секции.

**Strict reader** отклоняет любое нарушение контракта. **Compatibility reader**
дополнительно воспроизводит только известные особенности оригинала, например
именованный fallback. Compatibility mode не означает игнорирование произвольной
порчи.

**Roundtrip** -- последовательность decode -> encode. **Byte-identical
roundtrip** создаёт файл, полностью совпадающий с исходным. **Lossless editor**
может изменить известное поле, сохранив все остальные bytes и порядок записей.

**Fallback** -- явно предписанный запасной путь, например материал `DEFAULT`,
затем entry 0. **Heuristic** -- догадка по похожим данным; она не должна
незаметно заменять доказанный fallback.

### Игровой runtime

**Engine** -- программная среда, которая загружает данные, ведёт время,
исполняет мир и формирует изображение/звук. **Game** -- конкретные правила,
миссии и содержимое, работающие поверх engine services.

**World** -- долгоживущее состояние миссии: objects, terrain, время, кланы и
managers. **Scene** -- представление части мира для конкретной обработки, чаще
всего текущей камеры.

**Game object** -- сущность с идентичностью, transform, properties и lifecycle.
**Component/controller** -- специализированная часть поведения: animation,
physics, AI или rendering representation.

**Simulation** отвечает за изменение мира. **Tick** -- один расчётный шаг
simulation. **Frame** -- одно подготовленное изображение. Число ticks и frames
за единицу времени не обязано совпадать.

**Game loop** -- повторяющийся порядок ввода, расчёта, рендера и обслуживания.
**Scheduler phase** -- явно ограниченный участок loop, где разрешены
определённые операции.

**Event/message** -- типизированное сообщение между objects или subsystems.
**Queue traversal** -- стабильный обход зарегистрированных объектов.
**Deferred deletion** -- перенос фактического удаления до безопасной границы
после traversal.

**Determinism** -- одинаковый результат при одинаковом initial state, input,
времени и порядке событий. **Replay** -- повторное исполнение записанной
последовательности входов/сообщений для проверки determinism.

**Authority** -- subsystem или network peer, которому разрешено окончательно
менять состояние объекта. **Mirror object** -- локальное представление объекта,
authority которого находится у другого player.

### Геометрия, анимация и рендеринг

**Mesh** -- набор vertex/index streams и draw-групп, описывающий форму.
**Node** -- элемент hierarchy модели со своим local transform. **Slot** в MSH
-- выбранная геометрическая группа для комбинации node, LOD и group; он также
хранит bounds и диапазоны batches.

**Batch** -- непрерывный индексный диапазон с одним material slot и общим
render state. **Transform** переводит данные между local, world, view и clip
spaces. Порядок умножения matrices является частью контракта.

**Quaternion** -- четырёхкомпонентное представление вращения. **Keyframe** --
pose в определённое время. **Sampling** выбирает pose для времени, а
**blending** смешивает animation states.

**Bounds** -- упрощённый объём для быстрых тестов. **AABB** -- пара
minimum/maximum по осям. **Bounding sphere** -- center и radius.

**Renderer** -- subsystem, преобразующая подготовленную сцену в изображение.
**Backend** -- реализация renderer поверх конкретного API или устройства.

**Draw call** -- команда нарисовать диапазон primitives с текущими resources и
states. **Material** -- правила отображения поверхности: texture, коэффициенты,
прозрачность и режимы pipeline. **Material phase** -- одно временное состояние
анимированного материала.

**Texture** -- двумерный массив texels. **UV coordinates** -- координаты
выборки. **Mip chain** -- последовательность уменьшенных уровней texture.
**Lightmap** -- texture с заранее рассчитанным вкладом освещения.

**Fixed-function pipeline** -- старый графический pipeline, где приложение
выбирает predefined transform, lighting, texture-stage и blend states вместо
пользовательских shaders.

**Depth buffer** хранит глубину уже принятой поверхности. **Alpha test**
полностью принимает или отвергает fragment. **Blending** смешивает новый цвет с
framebuffer.

**Back buffer** -- скрытый framebuffer. **Present/flip** делает завершённый
кадр видимым. **Pixel parity** -- совпадение конечного изображения при
фиксированных условиях.

### Навигация, физика, звук и сеть

**Areal** -- логическая область карты с границей, class/flags и связями с
соседями. **Areal graph** -- граф, вершинами которого служат области, а рёбрами
-- допустимые переходы. **Cell grid** -- пространственный индекс для candidate
areas или objects.

**Pathfinding** -- поиск маршрута по графу. **A\*** использует стоимость уже
пройденного пути и оценку расстояния до цели. Навигационная проходимость,
отсутствие collision и видимость -- разные свойства.

**Collision proxy** -- упрощённое представление объекта для столкновений.
**Broad phase** быстро находит потенциальные пары; **narrow phase** выполняет
точную проверку и вычисляет contact.

**Sample** -- декодированные звуковые данные. **Source** -- экземпляр
воспроизведения с position, gain, loop state и временем. **Listener** -- позиция
и ориентация слушателя для 3D spatialization.

**Transport** -- механизм доставки bytes между peers. **Protocol** -- framing,
message types, порядок и правила подтверждения. **Serialization** --
преобразование typed state в byte sequence.

**Reliable delivery** гарантирует доставку/порядок в пределах выбранной модели;
**unreliable delivery** допускает потери ради задержки. **Wire compatibility**
-- способность обмениваться данными с оригинальным клиентом, а не только
воспроизводить ту же игровую семантику в новом протоколе.

## Связанные локальные справки

- [NRes](../reference/nres.md)
- [RsLi](../reference/rsli.md)
- [TMA](../reference/tma.md)
- [MSH](../reference/msh.md)
- [Texm](../reference/texm.md)
- [Materials](../reference/materials.md)
- [Render frame](../reference/render-frame.md)
- [Границы знания](../appendices/knowledge-boundaries.md)
- [Глоссарий](../appendices/glossary.md)

## Дополнительное чтение

Эти материалы помогают понять PE, ABI, сжатие, graphics pipeline, game loop и
навигацию. Они не являются доказательством поведения Iron3D: детали движка
принимаются только после проверки его бинарного кода и игровых ресурсов.

- [Microsoft PE/COFF specification](https://learn.microsoft.com/en-us/windows/win32/debug/pe-format)
- [Microsoft x86 calling conventions](https://learn.microsoft.com/en-us/cpp/build/x86-calling-conventions)
- [Intel Software Developer Manuals](https://www.intel.com/content/www/us/en/developer/articles/technical/intel-sdm.html)
- [Ghidra documentation](https://ghidra-sre.org/)
- [RFC 1951: DEFLATE](https://www.rfc-editor.org/rfc/rfc1951)
- [zlib manual](https://zlib.net/manual.html)
- [Kaitai Struct user guide](https://doc.kaitai.io/user_guide.html)
- [Microsoft Direct3D documentation](https://learn.microsoft.com/en-us/windows/win32/direct3d)
- [Vulkan specification](https://registry.khronos.org/vulkan/specs/1.4-extensions/html/vkspec.html)
- [Real-Time Rendering resources](https://www.realtimerendering.com/)
- [LearnOpenGL](https://learnopengl.com/)
- [Scratchapixel](https://www.scratchapixel.com/)
- [Game Programming Patterns](https://gameprogrammingpatterns.com/)
- [Fix Your Timestep](https://gafferongames.com/post/fix_your_timestep/)
- [Red Blob Games: A*](https://www.redblobgames.com/pathfinding/a-star/introduction.html)
