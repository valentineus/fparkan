# VI. Поведение, управление, звук и сеть

Шестой том описывает подсистемы, которые превращают загруженный мир в
реагирующую игру: AI, Behavior, Wizard, Control, ввод, камеру, звук и сеть.
Эти области нельзя восстанавливать только по структуре файлов. Для них важны
порядок кадра, ownership объектов, timing событий и доказуемые границы между
решением, движением, presentation и транспортом.

Ключевой принцип: reader compatibility не равна gameplay compatibility.
Корректно разобранный ресурс ещё не доказывает, что runtime выбирает ту же
цель, строит тот же маршрут, применяет ту же collision correction, создаёт тот
же sound event или отправляет тот же network payload. Поэтому все утверждения
ниже разделяют подтверждённую структуру, восстановленный архитектурный
контракт и открытые участки, требующие динамической трассировки.

```text
AI / mission script
  -> стратегическая цель, условия, команды миссии
Behavior
  -> состояние объекта, target, global/local path
Wizard
  -> локальная коррекция траектории
Control
  -> physical step, collision proxy, итоговый transform
World3D
  -> очередь событий, ownership, deferred deletion
Render / Sound / Net
  -> представление, listener, mirrors и сообщения
```

Связанные главы: [мир и миссии](04-world.md), [геометрия и рендер](05-render.md)
и справочный [render frame](../reference/render-frame.md).

## AI, Behavior и Wizard

Iron3D разделяет стратегическое принятие решений, поведение конкретного объекта
и локальную коррекцию движения. Это разделение должно сохраниться в новой
реализации: стратегический AI не меняет transform напрямую, а collision manager
не выбирает игровую цель.

```text
ai.dll / SuperAI
  -> цель клана, миссии и группы
Behavior.dll
  -> состояние юнита, target, global path, local corridor
Wizard.dll
  -> ближайшая допустимая траектория
Control.dll
  -> физическое движение и столкновения
```

### Behavior

`CreateBehaviour` создаёт controller для отдельного игрового объекта.
`CreateDistributor` восстановлен по consumers как посредник распределения
команд или ресурсов; это высокоуверенный архитектурный вывод, а не доказанное
имя внутреннего класса. Behavior получает `IArealMap` через AI/клановый
контекст, ведёт radar/target state, строит global path, превращает его в local
corridor и передаёт движение Wizard.

Ошибочные состояния проверяются явно:

1. отсутствует system map;
2. отсутствует terrain interface;
3. active behavior не имеет `IArealMap`;
4. объект попал в non-reachable area;
5. объект пытается выйти из non-walkable area;
6. path generator вошёл в infinite cycle.

Эти случаи являются fatal или diagnostic conditions. Совместимая реализация не
должна тихо исправлять их teleport-ом, потому что такое исправление скрывает
ошибку areal graph, terrain query или state machine.

### Параметры Behavior.ini

Подтверждены настройки:

```text
PathFind_BuildingHitDist
PathFind_BuildingNearestDist
PathFind_NearBuildSpeedPercent
PathFind_CorridorRadius
PathFind_NearDoorCoeff
PathFind_fStepOffBuilding
PathFind_MaxAccel
PathFind_MaxRotation
PathFind_fStepDist
PathFind_MinPointInTrajectory
Network_ResourceTransferMaxDelay
```

Они задают геометрию corridor, дистанции реакции на здания, снижение скорости
возле препятствий, пределы ускорения и поворота, дискретизацию trajectory и
сетевой timeout передачи ресурсов. Значения читаются как runtime-конфигурация,
а не компилируются в код. Parser должен поддерживать комментарии `//`, пробелы
вокруг `=` и CRLF.

Файл также содержит logging/debug switches: `Behavior.log`, уровни ошибок,
show vectors и z-buffer debug. Эти переключатели полезны не только для
совместимости, но и как модель современных trace flags.

### Wizard

Wizard получает желаемое направление и corridor, анализирует ближайшие
ограничения и выдаёт скорректированную локальную траекторию. Behavior может
очищать её через `ClearWizardPath` при смене цели, повреждении global path или
переходе объекта в неактивное состояние.

Нужно различать четыре уровня движения:

- **global path** -- последовательность areals;
- **local path** -- точки или сегменты внутри corridor;
- **wizard path** -- краткосрочное движение с учётом ближайших препятствий;
- **physical step** -- фактически разрешённое Control перемещение.

Хранение всего маршрута одним массивом лишает систему возможности локально
обойти препятствие без полного повторного поиска. Граница Behavior/Wizard
существует именно для того, чтобы краткосрочная геометрическая коррекция не
ломала стратегический path state.

### SuperAI и миссионные сценарии

`CreateSuperAI` создаёт центральный controller клана; `GetSuperAI` возвращает
его. AI загружает файлы из `MISSIONS\SCRIPTS\`, проверяет версию и пишет ошибки
в `ai.log`. Несовпадение версии является отдельной ошибкой, а не неизвестной
командой.

Сценарный корпус содержит binary `.scr`, formula exports `.fml`, таблицу
переменных `varset.var` и `.trf`-данные. `.scr` хранит именованные секции и
события, например `Init`, `Mission`, `Problems0`, `Fort_Task_Complete` и
`Hero_Teleported`, вместе с числовыми ссылками на compiled instructions.
`.fml` является текстовым экспортом formula set. `varset.var` декларативно
описывает типы, defaults, ranges и строки через макросоподобные формы
`VAR(...)` и `STRING(...)`.

Compiled package больше не opaque blob: `fparkan-script` losslessly читает
проверенный внешний framing. Сначала идут `opcode_handler_count` и
`event_count` (`u32 LE`), затем именованные события с NUL-terminated raw
именем и nested records. Reader сохраняет все seven raw header words и
references каждого record, жёстко ограничивает counts/allocations и отдельно
сохраняет trailing bytes. На `c1m2p.scr` GOG reader получает `73` handlers,
`9` events, `17` records и `20` references без trailing bytes. Это структура
файла, но не таблица семантик: названия opcode/words появятся только после
handler contracts и runtime traces.

Связь первого header word с dispatch теперь доказана статически: `ai.dll`
создаёт 73 handler pointers в известном порядке и копирует table без
перестановки. По всем 58 GOG packages первый word — индекс `0..72` либо
`0xffff_ffff` sentinel; `fparkan-script` отражает это как typed
`ScriptDispatchSelector`, сохраняя неожиданные значения `Unknown`. Первый
handler лишь инициирует execution context (`+0x50 = 1`); его gameplay meaning
пока не назван.

Первый часто встречающийся table entry с side effect — `Handler(2)` (176
records в corpus). Он берёт active instruction, разрешает семь slots через
varset, приводит три значения к float по observed kinds `5`/`3`, затем
materializes или refresh-ит внутренний event record. Его base string связывает
`<base>_Start` и `<base>_Continue` с event table; прямого World3D/Behavior
call на этой ветке не найдено. Slot names и consumer record ещё не установлены
динамически, поэтому Rust не исполняет handler как гипотетическую команду
движения/атаки/строительства.

Безопасная runtime-модель:

```text
load script bundle
  -> validate version and symbol tables
  -> create global/formula variables
  -> bind named events to instruction offsets
  -> instantiate SuperAI per clan
  -> dispatch MISSION_START and object events
  -> update timers/conditions each simulation tick
  -> enqueue game commands through World3D/Behavior
```

Сценарий не должен владеть игровым объектом напрямую. Он хранит logical/object
IDs и отправляет команды через игровые interfaces, чтобы удаление объекта или
сетевой mirror не оставили dangling pointer.

Полная grammar compiled instructions и точное значение всех opcodes остаются
открытым направлением. До появления decompiler-а `.scr` binary body сохраняется
lossless, а доказанные symbol/event tables документируются отдельно.

### Подтверждённый evaluator выражений

Ghidra 12.1.2 decompile GOG `ai.dll` фиксирует отдельный evaluator по VA
`0x10005180` (не dispatcher инструкций `.scr`). Он получает индекс записи,
ищет её в контейнере `this + 0x34`, переключается по `u32 tag` в offset `+0`
и при успешном результате пишет completion byte в `+0x0c`. Из кода доказаны
ровно пять ветвей `tag = 1..5`; `tag = 1..3` требуют `u32` subtype в
`+0x04 == 0`, а `tag = 4..5` — `+0x04 == 1`. Поле payload находится по
`+0x08`.

Ветки 1--3 делают lookup через object/interface, достижимый от
`this + 0x60 + 0x35c`, и используют его virtual slot `+0x1c`. Ветка 2
дополнительно читает virtual slots `+0x10`/`+0x18` возвращённого объекта и
разрешает result tags `1`, `0x12`, `0x13`. Ветка 3 сравнивает virtual slot
`+0x44` с текущим object value. Ветки 4--5 используют `payload` как index в
контейнере `this + 0x4c`, обходят его child indices и делают тот же lookup;
их разные success-guards пока не именуются семантически. Это доказывает
typed condition/evaluation layer, но **не** формат `.scr`, размеры инструкций
или связь чисел tag с языковыми операторами.

Выгрузка воспроизводится без изменения PE:

```powershell
& 'C:\Tools\ghidra_12.1.2_PUBLIC\support\analyzeHeadless.bat' `
  C:\temp\fparkan-ghidra ai -import 'C:\GOG Games\Parkan - Iron Strategy\ai.dll' `
  -processor x86:LE:32:default `
  -scriptPath C:\Develop\fparkan\tools\ghidra `
  -postScript ExportAiExpressionDispatcher.java -deleteProject
```

### TRF и preload-данные

TRF-файлы проходят структурный разбор. `auto.trf`, `data.trf` и tutorial
variants имеют сигнатуру [NRes](../reference/nres.md) и содержат большие
таблицы имён игровых прототипов: оружия, башен, сооружений и других объектов.
Также найдены preload-записи, ANI и SKE resources.

По содержимому, порядку загрузки и consumers TRF с высокой вероятностью
предоставляет AI/сценарному слою заранее подготовленную таблицу типов и
связанных данных. Framing и имена подтверждены corpus-ом, но полная семантика
каждой TRF-записи ещё не закрыта. Имена должны разрешаться через тот же
resource registry, что и миссионные объекты.

### Стабильность AI-слоя

`ai.dll`, `Behavior.dll` и `Wizard.dll` побайтно идентичны в Частях 1 и 2. Это
подтверждает, что разделение SuperAI -> Behavior -> Wizard и бинарная
реализация этих трёх уровней не менялись.

Сценарный корпус:

```text
Часть 1: 58 SCR, 58 FML, 29 TRF
Часть 2: 59 SCR, 59 FML, 44 TRF
```

Все TRF являются структурно валидными NRes. Неизменность DLL усиливает вывод о
стабильной VM, но не закрывает instruction grammar `.scr`: для неё нужен
dispatcher/jump-table decompiler. Дополнительные сценарные данные расширяют
differential corpus, но не заменяют анализ VM.

## Control, физика и коллизии

Control превращает желаемое движение в физически допустимое изменение
состояния. World3D владеет жизненным циклом объекта; Terrain предоставляет
поверхность и world queries; Behavior/Wizard задают намерение; Control создаёт
physical controller и collision representation.

Публичная поверхность:

```text
InitializeSettings
LoadControlSystem
LoadPhysicalModel
CreateCollManager
CreateCollObject
```

Модуль импортирует World3D queue/object functions, `Terrain::GetWorld`, часы,
тригонометрию и `g_FastProc`. Это подтверждает его положение между gameplay
object и геометрией мира.

Статическая сверка GOG `Control.dll` уточняет границу, но пока не раскрывает
per-tick solver. PE32 image base — `0x10000000`; exports имеют следующие RVA:

```text
InitializeSettings  0x32260
LoadControlSystem   0x32280
LoadPhysicalModel   0x32580
CreateCollManager   0x325d0
CreateCollObject    0x32600
```

`LoadControlSystem` возвращается `ret 0x20`, следовательно ABI снимает со
стека восемь 32-bit аргументов. Оно выбирает allocation размером `0x668` при
mode `9` и `0x670` для другого mode, создаёт внутренний reader через virtual
dispatch с selector `10`, переносит несколько caller strings в локальные
buffers и передаёт собранную settings-структуру дальше через virtual slot
`+0x08` с key `0x80000020`. Это доказывает loader/configuration boundary и
два layout variants, но не даёт права назвать поля скоростью или acceleration.
`LoadPhysicalModel` аналогично создаёт `0xa0`-byte reader и возвращается
`ret 0x0c`; `CreateCollManager` и `CreateCollObject` возвращают interface
pointer с поправкой `+4` после внутренней инициализации.

Headless Ghidra 12.1.2 decompile GOG binary подтверждает ABI формой экспортов
`LoadControlSystem(char*, char*, char*, char*, char*, u32, void*, i32)`,
`LoadPhysicalModel(u32, u32, u32)`, `CreateCollManager(u32)` и
`CreateCollObject(u32, u32)`. Первые пять параметров Control loader — строки,
а mode передаётся последним; decompiler не восстанавливает предметные имена
остальных слов. `InitializeSettings` получает `CreateGameSettings()` из
World3D и делает virtual call slot `+0x24` с literal `0x15` и строкой по RVA
`0x42478`. Reproducible extractor находится в
`tools/ghidra/ExportControlFunctions.java`; он декомпилирует только эти exports
в локальном Ghidra project и не изменяет оригинальную DLL.

Именно update methods этих private objects, а не пять exports, остаются
следующим объектом динамической трассировки. Поэтому reference movement в
новом runtime намеренно не использует неподтверждённые параметры Control.

### Связь с AniMesh

PE import table GOG `AniMesh.dll` показывает, что это прямой consumer
`CreateCollManager`, `CreateCollObject` и `LoadControlSystem`; других DLL,
которые статически импортируют эти три named exports, не найдено. Внутренний
AniMesh path по VA `0x100032e7` вызывает Control thunks в строгой наблюдаемой
последовательности:

```text
LoadControlSystem  (thunk 0x1001934e)
  -> CreateCollObject (thunk 0x10019348)
  -> CreateCollManager (thunk 0x10019342)
```

После factory calls caller immediately берёт returned interface vtable и
делает дальнейшие virtual calls; это связывает Control с загрузкой
AniMesh/unit components, а не с одной глобальной настройкой процесса. В
частности, `CreateCollObject` получает два stack arguments, а
`CreateCollManager` — один. Предметные значения этих arguments, ownership
private objects и per-tick update slots пока не восстановлены; порядок
создания не доказывает скорость, collision algorithm или AI semantics.

Headless Ghidra call-site decompile уточняет configuration provenance:
AniMesh передаёт в `LoadControlSystem` шесть 32-byte strings из одного
configuration block по offsets `+0x80`, `+0xa0`, `+0xc0`, `+0xe0`, `+0x100` и
`+0x120`, затем resource context и mode из owner `+0x6d8`. Returned Control
interface сразу получает пять virtual calls, связывающих его с owner slots
`+0x158`, `+0x160`, `+0x164`, `+0x168` и `+0x18c`; collision object затем
связывается с `+0x170`. Это достаточное основание хранить будущий Control
component как ordered raw-string/resource provenance, но не для присвоения
этим строкам смысловых имён до трассировки private update methods. Extractor:
`tools/ghidra/ExportAniMeshControlCaller.java`.

Runtime сохраняет ordered raw Unit DAT records рядом с каждым mission object
draft. Это создаёт проверяемую границу передачи данных от loader-а к будущему
Control consumer-у: никакой компонент пока не получает имя `Control` только
по `kind`, `parent_or_link` или description; semantic binding появится лишь
после trace private update/load methods.

### Control system и physical model

`LoadControlSystem` загружает настройки controller-а: ограничения скорости,
ускорения, поворота и режимы управления. `LoadPhysicalModel` загружает форму и
параметры, используемые для столкновений. Visible MSH не обязан совпадать с
collision representation: для физики часто нужна более простая и устойчивая
форма.

Практичная runtime-модель:

```c
struct PhysicalState {
    Transform transform;
    Vec3 linear_velocity;
    Vec3 angular_velocity;
    float requested_speed;
    float requested_turn;
    uint32_t flags;
};

struct CollisionProxy {
    ObjectId owner;
    ShapeSet shapes;
    Bounds broad_phase_bounds;
    uint32_t category_mask;
};
```

Названия полей здесь описывают контракт совместимой реализации, а не точный
layout исходного C++-объекта.

### Collision pipeline

Один расчётный шаг удобно разделить так:

1. controller получает желаемые `speed`/`turn` от Behavior или manual input;
2. вычисляет кандидатный transform на основе `dt`;
3. обновляет broad-phase bounds collision object;
4. collision manager находит потенциальные пары и terrain candidates;
5. narrow phase вычисляет контакт или допустимый остаток перемещения;
6. physical state корректируется;
7. World3D получает итоговый transform;
8. событие `GMSG_COLLISION_DETECTED` отправляется в согласованной фазе.

Позиция collision event после narrow phase является рекомендуемой фазой
реализации и согласуется с назначением сообщения, но точный call-site
относительно всех correction steps требует динамической трассировки Control.
Удаление объекта из обработчика остаётся отложенным по правилам World3D.
Collision manager не должен хранить прямую незащищённую ссылку на объект,
который уже pending-delete.

### CTLD и physical resources

Реестр прототипов ссылается на `*.ctl`, `*.cpt` и связанные control resources.
В Части 1 структурно проверен 531 CTLD payload без ошибок. Размеры и пять
внутренних счётчиков образуют множество вариантов: наиболее частый размер
392 байта с pattern `(0,0,0,1,0)`, но встречаются блоки от примерно 212 до
1868 байт и более сложные комбинации.

CTLD является составным count-driven форматом, а не фиксированной struct.
Parser должен:

- прочитать prefix и все счётчики с проверкой переполнения;
- вычислить границы секций по их counts;
- сохранять неизвестные records в typed raw containers;
- требовать точного завершения payload;
- не использовать размер одного популярного варианта как универсальный layout.

Полная предметная семантика всех секций ещё не доказана, но существующие файлы
можно безопасно читать, индексировать и сохранять.

### Terrain queries и movement handoff

Control получает world-interface Terrain и использует поверхность, faces и
ускорители для высоты, нормали и пересечений. Навигационный маршрут сообщает,
куда двигаться, но итоговый transform определяется по физической поверхности.
При переходе через склон controller должен согласовать горизонтальный шаг,
высоту и ориентацию с terrain normal.

Порядок операций должен быть детерминированным: пары collision objects
сортируются по стабильному ID, contacts обрабатываются в фиксированной
последовательности, а интеграция использует одну политику `dt` и округления.
Иначе одинаковая миссия постепенно расходится даже без сети.

#### Reference controller в текущем runtime

`fparkan-runtime::advance_reference_movement` — намеренно маленький
детерминированный мост между сохранённым mission transform и `TerrainWorld`.
Он получает `OriginalObjectId`, явную XY-цель и положительный максимум шага,
находит только live/registered object по исходному ID, двигает XY не более чем
на этот шаг и записывает высоту из `TerrainWorld::height_at`. Orientation и
scale остаются исходными IEEE-754 words; функция не изменяет clock, очередь
World3D или animation state. Возвращаемое значение означает достижение именно
заданной XY-цели.

Это **не** восстановленный Control, Behavior или navigation controller:
функция не строит маршрут, не использует `dt`, скорость, terrain normal,
коллизии, acceleration и оригинальные AI decisions. Она существует как
проверяемый reference path, который фиксирует границу будущих controller-ов и
не позволяет renderer-у или gameplay обходить terrain query. Non-finite input,
неположительный шаг, отсутствующая миссия/объект и XY вне поверхности дают
явную ошибку без частичного изменения transform.

Licensed test на GOG `Autodemo.00` запускает этот путь для live mission object,
находит существующую поверхность и проверяет точные XY/Z words после snap. Это
доказывает связывание current runtime data, но не доказывает семантику
оригинального движения.

Путь доступен и через самостоятельный composition root:

```powershell
fparkan-headless --root "C:\GOG Games\Parkan - Iron Strategy" `
  --mission MISSIONS/Autodemo.00/data.tma `
  --move-object 0 419.10318 717.433 0.25 --ticks 1
```

`--move-object` принимает original object ID, target X/Y и maximum step;
требует `--root` и `--mission`, допускается один раз за запуск и отвергает
non-finite/неположительный шаг ещё при разборе аргументов. Приложение печатает
`reached`, затем normal headless tick/hash. Проверка на GOG 18 июля 2026 года
загрузила 8 objects, 343 areals и 3 174 terrain surfaces без graph failures;
команда для объекта `0` вернула `reached=false`, что подтверждает именно
ограниченный шаг, а не телепортацию к цели.

### Различия Control в Части 2

`Control.dll` пересобрана при неизменных размере, imports и пяти именах/ordinals
exports; RVA всех пяти exports изменились. Форматы и cross-module boundary
сохранились, но точное physical/collision behavior нельзя считать побайтно тем
же.

CTLD-корпус расширен с 531 до 623 payload. Новых framing errors не найдено;
большинство общих CTLD изменено вместе с переработанными моделями. Это
подтверждает count-driven parser, но не закрывает предметную семантику shape
records и contact solver.

Differential test обеих частей должен воспроизводить движение без препятствий,
slope following, pair collision, timing collision event и удаление объекта в
callback. Сравниваются transforms и contact events по tick, а не только факт
успешной загрузки.

## Ввод, камера и управление

World3D нормализует клавиатуру, мышь и joystick в общие scan codes и manual
commands. Win32 message handler вызывает `UpdateManualEventsList`; перед
обработкой новой порции сообщений основной цикл вызывает
`ClearManualEventsList`. Снимок клавиатуры очищается отдельно через
`stdClearKeyboard`.

Публичная поверхность включает `WinMsg2ScanCode`, converters для
keyboard/mouse/joystick/predicate, `ScanCode2Str`, `ManualCommand2Str`,
`stdIsKeyPressed`, lock/unlock keyboard и чтение mouse shift. Это позволяет
хранить конфигурацию управления независимо от физического устройства.

### Event, state и axis

Ввод имеет минимум три семантики:

- **edge event** -- нажатие или отпускание в текущей порции сообщений;
- **held state** -- клавиша остаётся нажатой между кадрами;
- **analog value** -- смещение мыши или положение joystick axis.

Manual command дополняет источник коэффициентом, режимом wrap, dead
zone/threshold и временной характеристикой. Строки camera bindings показывают
команды `MCMD_STATE`, `MCMD_ANGLE_X`, `MCMD_ANGLE_Y`, режимы `MAN_WRAP` и
`MAN_NOTWRAP`, а также параметры ускорения в миллисекундах.

Simulation читает подготовленный input snapshot. Renderer не должен
самостоятельно опрашивать OS, иначе одно и то же нажатие будет зависеть от
частоты кадров.

### Joystick через DirectInput

`Joystick.dll` экспортирует:

```text
QueryJoy
CreateJoy
ReleaseJoy
SetJoyRange
PeekJoyMessage
GetJoyCaps
```

`QueryJoy` обнаруживает устройство, `CreateJoy` получает интерфейс DirectInput,
`SetJoyRange` нормализует оси в диапазон движка, `PeekJoyMessage` выдаёт
очередное унифицированное событие.

При потере устройства чтение может вернуть ошибку acquired state. Интерфейс
следует повторно получить, очистить устаревшее состояние и продолжить.
Hot-unplug не должен оставлять последнюю ось навсегда отклонённой.
`GetInstalledJoyNames` и `SetActiveJoy` в World3D связывают device list с
game-facing выбором.

### Два camera interface

World3D предоставляет `stdSetCurrentCamera`/`stdGetCurrentCamera`: это камера
как часть игрового состояния. Terrain имеет
`stdSetCurrentCamera2`/`stdGetCurrentCamera2`: concrete camera, которую world
renderer использует для matrices, viewport и visibility.

`LoadCamera` экспортирован обоими модулями. По call graph World3D-вариант
играет роль component bridge, а Terrain-вариант связан с concrete
camera/world implementation. Это архитектурный вывод: точные class names и
layout не восстановлены.

Минимальные данные камеры:

```text
world position and orientation
view matrix
projection parameters / field of view
near and far planes
viewport rectangle
camera mode and target object
manual angles/state
```

Такая граница позволяет game code работать с абстрактной камерой, не зная
внутреннего renderer representation.

### Camera commands и порядок кадра

Подтверждены команды `CMD_CAMERA_LEFT`, `CMD_CAMERA_RIGHT`, `CMD_CAMERA_UP`,
`CMD_CAMERA_DOWN`, `CMD_CAMERA_CENTER`, `CMD_CAMERA_INFRARED`, а также
spotlight и внешние/миссионные camera modes. Горизонтальный угол использует
wrap, вертикальный -- ограниченный диапазон. Center плавно возвращает обе оси к
заданному значению.

Порядок кадра:

1. собрать manual events;
2. обновить camera controller во время calculation;
3. вычислить итоговый transform и ограничения;
4. перед render установить current camera;
5. передать её Terrain и sound listener;
6. после кадра сохранить mode-specific state.

Camera smoothing должно использовать игровое время или специально
подтверждённые часы. Привязка к render delta делает управление разным при 30 и
144 FPS.

## Звуковая подсистема

Ngi32 создаёт низкоуровневый DirectSound backend. `services.dll` публикует
`ISoundServer`. Game, Terrain и FX работают уже через эти интерфейсы:
воспроизводят 2D/3D sources, меняют volume и связывают listener с camera.

Публичные функции Ngi32:

```text
niCreate3DSound
niGet3DSound
niGet3DSoundCaps
niMuteSound
```

Backend динамически вызывает `DirectSoundEnumerateA` и `DirectSoundCreate`;
параметр `DisableDSound` может полностью отключить этот путь.

### Устройство и capabilities

Конфигурация учитывает `3D Sound`, качество, reverse sound, частоту buffer,
режим постоянного воспроизведения и автоматический выбор лучшего устройства.
Эти значения преобразуются во внутренний capability/profile object до создания
sources.

Код содержит отдельный no-device state и строку `3D Sound was not initialized`.
Отсутствие 3D sound обрабатывается отдельно от ошибок simulation/resources.
Новый runtime не должен позволять отсутствию звука разрушать simulation и
обязан возвращать звуковым командам явный no-device result.

Общий sound object разделяется между подсистемами и использует счётчик
владельцев. Закрывать DirectSound следует после остановки всех sources и
atmosphere/FX managers.

### Sound resources и SWAV

Основная библиотека называется `sounds.lib`; `mission.cfg` также создаёт
именованные sound resources и variations. Legacy API `rsLoadWave` загружает
waveform из archive. Импорт `MSACM32` подтверждает путь преобразования сжатых
wave-данных в формат playback buffer.

Resource identity состоит из library и name. Один sound asset может иметь
несколько runtime sources с различными position, volume, pitch/flags и временем
запуска. Поэтому кэшировать следует decoded sample/buffer, а source object
создавать на событие.

FX opcode 2 хранит `archive[32] + name[32]` и обычно создаёт sound command.
Atmosphere использует отдельные loop/variation sources, например rain
background. Миссионный слой содержит voice events для завершения или провала
задания.

Проверенный SWAV-корпус:

```text
Часть 1: 399 — 306 MS ADPCM, 93 PCM
Часть 2: 540 — 446 MS ADPCM, 93 PCM, 1 empty entry
```

Все непустые записи имеют RIFF/WAVE framing и частоту 22 050 Hz. В Части 2
entry `ALIEN_ME.WAV` имеет размер 0. Это присутствующий archive key без
decodable waveform.

Sound loader должен различать:

- `entry_missing`;
- `entry_empty`;
- `wave_invalid`;
- `decoded_sample`.

Нулевой payload не передаётся RIFF parser-у и не должен приводить к чтению
header за границей.

### 3D listener и sources

Перед world traversal `stdRenderGame` обновляет listener из camera transform.
Listener содержит position, orientation и, при наличии, velocity. Source
содержит world position и параметры затухания. Spatialization выполняется
backend-ом либо совместимой программной моделью.

```text
camera transform
  -> listener position/front/up
object or effect transform
  -> source position
sample + source parameters
  -> DirectSound 3D buffer
```

Прямо подтверждено обновление listener в начале `stdRenderGame`, до world
traversal. Sound events могут создаваться и в calculation/FX path, поэтому
нельзя утверждать, что listener предшествует созданию каждого source. Важно,
что spatial backend получает camera state текущего отображаемого кадра до
завершения его обработки. Перенос listener update после world render создаст
как минимум однокадровое рассогласование presentation.

### Громкость, mute и CD-аудио

`iron3d.dll` применяет отдельные настройки эффектов и CD sound. Параметр
`FORCE_CD_SOUND` меняет политику выбора музыкального источника. `niMuteSound`
должен временно остановить вывод без разрушения sample cache и logical playback
state.

В новой реализации полезно разделить buses: master, effects, ambient, voice и
music/CD. Это проектное решение совместимого backend-а, а не доказанный layout
оригинального mixer-а. Оно позволяет применять старые коэффициенты, не
переписывая individual source volume.

### Граница service layer

`Ngi32.dll` с DirectSound/backend code не изменилась между Частями 1 и 2, но
`services.dll` пересобрана и уменьшилась на 4 096 байт. Поэтому low-level
decoder/device path подтверждается одной машинной реализацией, а service
lifecycle, GUI/audio wiring и defaults требуют раздельной трассировки обеих
частей.

## Сетевая подсистема

Net инкапсулирует DirectPlay4A и lobby/service-provider API. World3D строит над
транспортом player identity, mirror objects и игровые сообщения. Эти уровни
следует разделять: DirectPlay отвечает за доставку bytes между players,
World3D -- за смысл сообщения и владение объектом.

Application GUID:

```text
{3C1D1F01-A870-11D1-8400-000021B14415}
```

Он передаётся network instance и service layer. Экземпляры с другим GUID не
принадлежат одному логическому приложению.

### Lifecycle соединения

Публичные функции Net покрывают полный цикл:

```text
CreateNetworkInstance
  -> select/use service provider
  -> setup connection
  -> enumerate or create session
  -> join/create session
  -> create local player
  -> send/receive messages and player data
  -> destroy player
  -> close session
  -> close connection
```

Поддерживаются providers эпохи DirectPlay: TCP/IP, IPX и modem/lobby варианты,
если они установлены в системе. Функции явно проверяют, что DirectPlay enabled
до enumeration, session и player operations. Неверный порядок вызовов должен
возвращать понятную ошибку, а не разыменовывать пустой interface.

### Sessions, players и адреса

Net предоставляет enumeration service providers и sessions, выбор host/join,
player name/password/data, latency, максимальный размер сообщения, размер
очереди, server player info и provider address. Lobby launch обрабатывается
отдельной веткой.

Внутренняя модель должна хранить как минимум:

```c
struct NetPlayer {
    TransportPlayerId transport_id;
    uint16_t game_player_number;
    string name;
    RawBytes player_data;
    bool is_local;
    bool is_host;
};
```

Transport ID нельзя использовать как постоянный `ObjectId`. NetWatcher связывает
временный DirectPlay identifier с номером игрока и World3D entities.

### Игровые сообщения World3D

Подтверждённые имена message surface:

```text
GMSG_CREATE_REMOTE_PLAYER
GMSG_APPEND_RESOURCE
GMSG_CHANGE_OBJECT_OWNER
GMSG_SET_PLAYER_DATA
GMSG_MISSION_DATA_PATH
GMSG_TAKE_OBJECT
GMSG_TEXT_FOR_PLAYER
GMSG_SYNC_STATE
GMSG_CREATE_MIRROR
GMSG_PAUSE_REMOTE_PLAYER
GMSG_CONFIRM_PLAYER_DATA
GMSG_KILL_PLAYER
SYSMSG_SET_TIME
SYSMSG_SET_PLAYER_NUMBER
GMSG_END_MESSAGE_SEQ
GMSG_REMOVE_RESOURCE
```

`GMSG_COLLISION_DETECTED` относится к общей очереди, но не обязательно
передаётся по сети. Message ID, payload size и delivery policy должны быть
частью явной schema. Нельзя сериализовать C++ pointers или native padding.

### Mirror objects и ownership

Удалённо принадлежащий объект представлен local mirror instance. Он участвует в
рендере и spatial queries, но authority над его созданием, ключевыми properties
и удалением находится у owner player. Сообщение смены владельца обновляет эту
границу; оно не должно создавать второй объект с тем же ID.

Типовой путь:

```text
remote create message
  -> validate player and ObjectId
  -> resolve prototype/resources
  -> CreateMirrorObject
  -> apply initial state
  -> AddMirrorObjectToGame
  -> subsequent sync messages update mirror
```

При потере player NetWatcher инициирует предписанное удаление или transfer
ownership через World3D queue. Мгновенное освобождение во время receive callback
запрещено по тем же причинам, что и в calculation pass.

### Сжатие и wire compatibility

`netZipData` и `netUnZipData` образуют встроенный слой упаковки payload. Он
находится выше транспорта: переход с DirectPlay на UDP/ENet не отменяет
необходимость воспроизводить формат упакованного сообщения, если требуется
соединение с оригинальной игрой.

Полный wire schema, framing и алгоритм сжатия пока не доказаны packet
capture-ом. Поэтому нужны два режима:

- **native compatibility** -- отдельный adapter, реализуемый после трассировки
  оригинальных packets;
- **modern multiplayer** -- новая versioned protocol schema, использующая ту же
  game-message семантику, но не заявляющая совместимость с DirectPlay client.

Эти режимы нельзя незаметно смешивать. До доказательства native wire
compatibility современный transport должен быть versioned и отделён от слоя,
который претендует на совместимость с оригинальным клиентом.

### Стабильность сетевого слоя

`Net.dll` и `World3D.dll` побайтно идентичны в обеих частях. Application GUID,
DirectPlay wrapper, mirror-object API и World3D message surface относятся к
одной машинной реализации.

Это подтверждает отсутствие отдельной сетевой реализации для Части 2, но не
закрывает wire schema: без packet/send-receive capture по-прежнему неизвестны
точное framing, reliability flags, payload layouts и алгоритм `netZipData` для
native interoperability.

Для binary regression достаточно одного профиля неизменённых DLL, но message
captures должны включать контент обеих частей, потому что prototype/resource IDs
и mission data различаются.

## Контракты реализации

Совместимая реализация должна фиксировать не только результат, но и момент его
появления в кадре. Для Behavior, Control, input, sound и network особенно важны
tick boundaries: одна и та же команда, применённая на один tick раньше или
позже, меняет дальнейшую симуляцию.

### Trace-события

Минимальный trace для этого тома:

- input snapshot: edge events, held state, analog values;
- camera state: mode, target, angles, matrices, viewport;
- Behavior: target, areal, global path revision, local corridor;
- Wizard: requested vector, constraints, wizard path;
- Control: candidate transform, contacts, correction, final transform;
- World3D queue: message name, ObjectId, dispatch phase, deferred deletion;
- sound: sample key, source owner, position, event tick, listener state;
- network: player mapping, message ID, payload length, delivery policy.

Для рендера это связывается с [render frame](../reference/render-frame.md):
camera и listener должны попадать в trace до world traversal, иначе нельзя
отделить ошибку presentation от ошибки управления.

### Проверки Behavior и сценариев

- script version mismatch даёт отдельную ошибку;
- event table читается lossless;
- VM body сохраняется без потери неизвестных bytes;
- отсутствующий `IArealMap` не замалчивается;
- non-walkable/non-reachable states дают diagnostic condition;
- одинаковый input log воспроизводит одинаковый sequence Behavior commands;
- resource names из TRF разрешаются через общий registry.

### Проверки Control

- движение без препятствий;
- slope/terrain-following;
- симметричные pair-collision tests с переставленными IDs;
- contact event отправляется один раз в предписанной фазе;
- удаление объекта в collision callback безопасно;
- replay одинакового input log даёт одинаковые transforms;
- collision proxy перестраивается после смены component/model state.

### Проверки input и камеры

- edge event не повторяется как held state;
- mouse/joystick axis сбрасывается по правилам snapshot;
- hot-unplug joystick не оставляет старое отклонение;
- camera horizontal angle wraps, vertical angle clamps;
- center command использует подтверждённое время, а не render FPS;
- Terrain и sound получают одну и ту же camera frame.

### Проверки звука

- backend может отсутствовать без нарушения simulation;
- один decoded sample переиспользуется несколькими sources;
- `entry_missing`, `entry_empty` и `wave_invalid` различаются;
- listener совпадает с camera frame;
- loop source корректно переживает pause/resume;
- mute не сбрасывает position и time;
- missing sound resource содержит полную диагностическую цепочку;
- deterministic test сравнивает список sound events, а не waveform устройства.

### Проверки сети

- нельзя создавать queue с активной сетью и нулевым player ID;
- session/player operations до enable/setup возвращают ошибку;
- сообщения проверяют длину до чтения payload;
- sequence/end markers обрабатываются в стабильном порядке;
- duplicate create mirror не создаёт второй instance;
- ownership change атомарно обновляет routing;
- pause/time messages применяются в одной simulation boundary;
- resource transfer имеет timeout `Network_ResourceTransferMaxDelay`;
- disconnect не оставляет objects с несуществующим owner;
- replay записанного message log даёт одинаковое World3D state.

`resnet.log` и `NetWatch.log` следует поддерживать как отдельные каналы: первый
относится к transport/resource exchange, второй -- к связи players и game
objects.

## Границы знания

Подтверждены внешние interfaces, часть runtime order, значимые строки,
конфигурационные параметры, corpus-level counts и стабильность ряда DLL между
двумя частями. Открытыми остаются:

- instruction grammar `.scr` и semantics всех VM opcodes;
- точная семантика всех TRF-записей;
- полный layout CTLD shape records;
- contact solver и порядок всех correction steps;
- class layout камер, контроллеров, sound service и network watcher;
- DirectPlay wire framing, reliability flags и payload schema;
- алгоритм `netZipData`/`netUnZipData`;
- точные defaults service layer там, где DLL пересобраны.

Эти границы должны оставаться видимыми в документации и тестах. Если новая
реализация вводит удобный современный abstraction layer, он обязан быть
отделён от утверждений о native compatibility и покрыт отдельным trace.
