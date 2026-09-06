# VIII. Устройство оригинальной программы

Здесь собраны детали, полезные при чтении оригинальных DLL: экспортируемые
функции, соглашения вызова, адреса и конфигурационные файлы. Адрес относится
к конкретной сборке; сравнить её можно по
[хэшам модулей](../reference/original-binaries.md).

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

Сводка hashes и import/export оригинального движка вынесена в отдельную
[страницу](../reference/original-binaries.md). Текущее состояние
реализации и границы live Vulkan path описаны в [справочнике render frame](../reference/render-frame.md)
и в исходном коде адаптера.

Подтверждённые hashes неизменённых DLL:

```text
World3D.dll 17e4a3089b2583a8cf2356c9db0390b1aba138356a09130d79b4e7e4791da61e
Ngi32.dll   bab9840d94f4e4e74ffc26677724fa896cf4823845504d09a9e025f80016edf5
```

Для GOG `World3D.dll` с этим hash RVA export-ов такие: `stdCalculateGame=0x139A0`,
`stdRenderGame=0x13BD0`,
`sendEndOfRender=0x13D90`, `stdSetCurrentCamera=0x13E60` и
`stdGetCurrentCamera=0x13E80`.

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

Статический анализ той же GOG базы уточняет рабочий контракт
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

Outer vtable `off_100665B4` теперь даёт exact transform adjustment для
world-like sample. Slot `+0x54` вызывает у subobject `outer + 4` slot `+0x20`
с selector `0`, затем копирует returned `+0x0C/+0x1C/+0x2C`. В live объекте
selector field `outer + 0x10` был `0xFFFFFFFF`; реализация selector `0` при
этом возвращает `outer + 0x20`. Значит observed triple
`outer + 0x2C/+0x3C/+0x4C` — доказанная translation часть active affine
transform, а не корреляция. Selector `2` возвращает paired block `outer + 0x60`;
outer slot `+0x70` применяет `atan2` к его axis values, что доказывает
orientation-angle path. Названия полей, angle order и handedness пока не
установлены, но raw affine transform и translation можно сохранять как
backend-neutral camera pose без догадок.

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


## Дальнейшее чтение

[Глоссарий](../appendices/glossary.md),
[открытые вопросы](../appendices/knowledge-boundaries.md),
[сценарная VM](../appendices/script-vm.md),
[сохранения и кампания](../appendices/saves-campaign.md),
[интерфейс игры](../appendices/ui-shell.md).
