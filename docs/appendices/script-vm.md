# Сценарная VM, формулы и игровые свойства

## Подтверждённый surface

Миссионный сценарный слой задаёт стартовые события, completion/failure,
messages, teleports, задачи, research и campaign transitions. Точки входа и
файлы: `ai.dll: CreateSuperAI/GetSuperAI`, `MisLoad.dll: LoadResearch`,
`ArealMap.dll: CalcFullResearchCost`, `MISSIONS/SCRIPTS/*.scr`, `*.fml`,
`*.trf`, `varset.var`, `MISSIONS/dispatcher.ini`, `mission.cfg`, `messages.cfg`
и `briefing.cfg`.

`.scr` — binary package с version checks, symbol/event sections и offsets;
полная opcode grammar не доказана. Его внешний framing читает
`fparkan-script`: первый little-endian `u32` является числом required opcode
handlers, второй — числом event records. Каждый event хранит `name_len`,
`name_len + 1` raw bytes с обязательным NUL, opaque event word и count вложенных
records. Вложенный record сохраняет семь `u32` header words (в disk order),
список `u32` references после шестого header word и trailing seventh word.
Никакой из этих words ещё не получает semantic name. `.fml` — текстовый
symbol/formula oracle; `varset.var` задаёт `VAR(...)`/`STRING(...)` defaults.
`fparkan-script::parse_varset` уже читает подтверждённые numeric
`VAR(float|DWORD, name, default)` declarations byte-safe (comments остаются
opaque, поэтому legacy non-UTF-8 text не ломает загрузку); `STRING(...)` и
`FUNCTION(...)` пока сохранены за границей этого numeric contract;
GOG `MISSIONS/SCRIPTS/varset.var` даёт через него ровно 231 declaration:
31 `float` и 200 `DWORD` (от `f0` до `fY`);
loader `ai.dll!0x10001000` сначала открывает `<bundle-base>.var` и только при
`not found` откатывается к этому shared file. Runtime повторяет данный порядок
транзакционно и публикует selected `MissionScriptVarSet` с путём/provenance, но
ещё не исполняет declarations как VM state;
`.trf` — NRes tables, чей framing подтверждён, а field semantics местами лишь
consumer-inferred.

## Безопасная модель исполнения

Целевая VM должна разделять immutable package (bytecode, symbols, events,
constants), per-mission variables/timers/frames, bindings
logical-name/ObjectId/clan/research key и typed commands к
World3D/Behavior/UI/campaign. После varset defaults и bindings она будет
запускать Init/start, обновлять timers на каждом tick, ставить готовые events
в стабильную очередь и исполнять bounded instruction budget. Опасное удаление
должно идти через World3D queue и общий deferred lifecycle.

До восстановления opcode table package mode читает header/strings/symbols/
event offsets/raw bytecode losslessly. Статический анализ уже выделил отдельный
evaluator записей условий (`ai.dll` RVA `0x5180`). Его подтверждённые ветви и
точные границы описаны ниже; он не является dispatcher-ом инструкций или jump
table `.scr`. Bytecode opcode table всё ещё требует отдельного доказательства.
Unknown opcode нельзя пропустить как один byte: это ломает синхронизацию. Для
каждого доказанного opcode фиксируются number, size, operands, control flow,
effects, errors и минимальный test.

GOG `ai.dll` доказывает этот framing двумя consumer-ами: loader по
`0x10001000` открывает `<bundle>.scr`, `varset.var`, `<bundle>.fml`, затем
собирает ровно 73 pointers handlers; `0x10011b20` читает описанную count-driven
структуру. В GOG `c1m2p.scr` reader находит 73 handler slots,
9 events, 17 nested records и 20 references, заканчивая на EOF.
Это подтверждает разбор пакета, но не смысл всех opcodes.

Selector устроен следующим образом: loader `0x10001000` создаёт 73 pointers в
фиксированном порядке, а `0x10011e70` копирует их без перестановки в runtime
array. Во всех 58 GOG `.scr` первый header word каждого nested record равен
`0..72` либо `0xffff_ffff`: соответственно 2095 handler selectors и 3992
sentinel records. Поэтому `ScriptInstruction::dispatch_selector()` возвращает
`Handler(0..72)`, `Sentinel` или сохраняемый `Unknown(u32)`. Первый handler
(`Handler(0)`, VA `0x10008034`) только устанавливает current context и flag
`+0x50 = 1`; это не даёт ему игрового имени и не заменяет runtime trace.

### Evaluator записей условий: `ai.dll` RVA `0x5180`

Ниже описан отдельный слой готовых условий, а не декодирование instructions.
Результат привязан к GOG `ai.dll` версии `1.00`, SHA-256
`c19cd6cd31cd2896f8c4ba0f0e700dd3408aec8182daaf4863d98a7d233a2abc`;
PE image base — `0x10000000`, адрес функции — VA `0x10005180` / RVA `0x5180`.
Функция занимает 910 байт и имеет единственного прямого caller-а
`FUN_10004ee0` (RVA `0x4ee0`). Это статический вывод по машинному коду; сам
evaluator и его внешний объектный интерфейс здесь не запускались.

Функция принимает signed index в массив записей по `this + 0x34`. Проверенный
доступ `FUN_100070c0` возвращает 16-byte запись:

| Смещение | Поле | Роль в evaluator-е |
| ---: | --- | --- |
| `+0x00` | `u32 tag` | Выбор одной из пяти ветвей |
| `+0x04` | `u32 subtype` | Дополнительная проверка формата ветви |
| `+0x08` | payload | Ключ объекта или индекс списка, в зависимости от tag |
| `+0x0c` | byte completion | Записывается в `1` только после успешной проверки |

Общие проверки допускают только пары `(tag 1..3, subtype 0)` и
`(tag 4..5, subtype 1)`. Неизвестный tag и неподходящий subtype не завершают
условие. При провале evaluator не очищает и не меняет байт завершения; при
успехе он устанавливает его в `1`. Вызывающая функция `0x4ee0` отдельно
пропускает уже завершённые записи, поэтому этот пропуск нельзя приписывать
самому evaluator-у. Прямая запись evaluator-а в запись — только этот байт, но
вызываемые виртуальные методы могут иметь внешние эффекты.

Пары tag/subtype проверяются так:

| Запись | Условие установки флага завершения |
| --- | --- |
| `(1, 0)` | Менеджер объектов, полученный из контекста `this + 0x60` по полю `+0x35c`, ищет payload через виртуальный слот `+0x1c`; поиск должен вернуть null. Найденный объект оставляет условие незавершённым. |
| `(2, 0)` | Поиск payload через тот же слот `+0x1c` должен найти объект. Затем его слот `+0x10` должен вернуть `0`, либо слот `+0x18` должен заполнить переданную структуру результата так, чтобы её первый `u32` был `1`, `0x12` или `0x13`. |
| `(3, 0)` | Поиск должен найти объект; его слот `+0x44` должен вернуть тот же `u32`, что и первый word контекста, на который указывает поле `this + 0x60`. |
| `(4, 1)` | Payload выбирает 32-byte запись в списке `this + 0x4c`; вложенный в неё список дочерних ссылок находится по смещению `+0x0c`. Ни один проверяемый дочерний ключ не должен разрешиться в объект. |
| `(5, 1)` | Используется тот же список. Отсутствующий объект пропускается; найденный объект проходит проверку, только если его слот `+0x10` вернул `0`. Первый найденный объект с ненулевым результатом завершает обход неуспехом. |

Для tag 2 структура размером `0x138` байт инициализируется функцией
`FUN_10004330`, затем передаётся в slot `+0x18`. x87-значение, которое
возвращает этот вызов, инструкция `FSTP ST0` по RVA `0x5256` сразу удаляет со
стека FPU. Следовательно, ветвь принимает решение только по первому word
выходной структуры, а не по возвращённому числовому значению.

У tag 4 и 5 есть важная граница индекса. Сравнение payload с количеством
элементов — unsigned `CMP` с условием `JA`: только `payload > count` сразу
оставляет запись незавершённой. Значение `payload == count` проходит эту
проверку и доходит до проверенного доступа `FUN_10002e20`; тот отвергает
`index >= count` и выбрасывает C++ exception. Это не эквивалентно обычной проверке
`payload < count` с безопасным отказом для всех неверных индексов.

Сами дочерние ссылки списка читаются как signed `i32`. Ровно `-1` — sentinel,
который пропускается. Остальные значения проверяются как индексы в контекстном
массиве `context + 0x10` с 32-byte элементами; в элементе по `+0x04` берётся
ключ следующего поиска объекта. Для tag 4 первый найденный объект немедленно
прерывает обход с незавершённым результатом; для tag 5 обход прекращается на
первом найденном объекте с ненулевым результатом slot `+0x10`. Если обход
достигает конца без такого запрета, соответствующая ветвь устанавливает byte
completion.

Из этих ветвей нельзя выводить языковые имена вроде «объект уничтожен» или
«объект принадлежит игроку»: в коде подтверждены только поиски, виртуальные
вызовы, сравнения и коды результата. Неизвестно и то, какая функция создаёт
массив 16-byte записей. Считыватель `FUN_10011b20` (RVA `0x11b20`) отдельно
разбирает `.scr` с переменной длиной: событие содержит длину и байты имени,
счётчик вложенных записей, а вложенная запись — семь заголовочных `u32` и список
ссылок. При разборе reader хранит описатели событий с шагом `0x14` и описатели
вложенных записей с шагом `0x20`; это шаги элементов внутренних контейнеров, а не
фиксированные размеры записей на диске. Связь этих контейнеров с массивом
`this + 0x34` не найдена, поэтому приравнивать их нельзя. Не установлены и
объект-владелец массива, соглашение его runtime-вызова и предметные эффекты
внешних виртуальных вызовов; границы перечислены в
[`knowledge-boundaries.md`](knowledge-boundaries.md).

`Handler(1)` — второй table entry, VA `0x10007fd0`, — не создаёт игровую
команду. Он сохраняет active VM context, берёт один instruction-derived index
через current event/instruction offsets `+0x48/+0x4c`, а затем разрешает его
в varset object по `this + 0x18`. Resolver `0x10002d30` проверяет
`0 <= index < count` и возвращает record `base + index * 0x30`; invalid index
вызывает C++ exception, а не становится нулём. Полученный 48-byte record
передаётся в `0x10013190`, который возвращает x87 floating result: kinds `0`
и `4` идут через отдельный opaque conversion path, kind `1` — signed integer,
kind `2` выбирает одну из двух static scalar constants по нулевости payload,
kind `3` — float, kind `5` — unsigned integer; остальные и пустые cases дают
один fixed fallback scalar. Это доказанный numeric
bridge для VM, но пока не Rust handler: неизвестны точный disk operand slot,
ownership значения на FPU stack и следующий consumer, поэтому нельзя назвать
его арифметическим opcode или silently заменить portable `f32` execution.
Отдельный проход по всем 58 GOG `.scr` (6 087 instruction records) не нашёл
ни одного selector `1`: из них 2 095 записей выбирают один из handlers, а
3 992 являются sentinel. Значит, это установленная, но не corpus-reachable
ветка данного издания; её нельзя делать приоритетным execution path без
отдельного dynamic/evidence route.

`Handler(2)` (третья entry table, VA `0x10009610`) уже имеет статический
contract, но ещё не Rust execution: он выбирает active event/instruction через
runtime offsets `+0x48/+0x4c` и разрешает семь 32-bit slots через varset object
`+0x18`. Их доказанный dataflow: slot 0 даёт один `u32` и base string, slot 1
даёт numeric scalar, slots 2 и 3 — по `u32`, slots 4, 5 и 6 принимают только
kind `5`/`3` и иначе дают `0.0`. Затем он вызывает `0x100059f0` объекта по
`this + 0x7c` и очищает flag `+0x50`.

Этот callee больше не opaque. Он строит key из семи значений, ищет matching
record в своей collection по `this + 0x24` и при совпадении обновляет только
record fields `+0x0c` и `+0x14`, затем вызывает его refresh path `0x10005070`.
При отсутствии record он лениво ищет в event table имена `<base>_Start` и
`<base>_Continue`, сохраняет их IDs в indexed state и materializes новый
internal record. В этой ветке не видно прямого World3D/Behavior call, поэтому
это доказанная scheduler/event-record boundary, а не команда движения, атаки
или строительства. Semantic names семи slots и consumer нового record остаются
открытыми; до dynamic capture Rust возвращает явный
unsupported result, а не «примерный» game command.

У этой границы также нет скрытого immediate dispatch: после добавления новой
записи `0x100059f0` вызывает `0x1000f920`, а Ghidra 12.1.2 декомпилирует эту
функцию как пустой `return`. Следовательно, найденные `<base>_Start` и
`<base>_Continue` только кэшируются в scheduler state; их фактический consumer
находится в отдельном позднем update path.

Во всех 58 GOG `.scr` имеются 6 087 instruction records, включая 3 992
sentinel. Самый частый non-sentinel selector — `Handler(30)`, 246 records.
Его VA `0x1000c266` читает первые два слова-ссылки активной инструкции,
разрешает каждое через varset (`0x10002d30` и `0x10013570`) и вызывает внешний
обратный вызов с тремя `u32`: `(0, first, second)`. Он не принадлежит `ai.dll`:
указатель из десятого аргумента сохраняет экспорт `CreateSuperAI`. В
проверенном GOG AutoDemo он указывает на `iron3d.dll` RVA `0x611d0` (SHA-256
`d1dc4ea8535e2069b0a05c9e8be2724dd438ba44cca7a83007d8b2e50f9e35fa`).
Вызываемая функция использует ABI `__cdecl(mode, command, payload)`. При
`mode == 0`, `command == 0` аргумент `payload` со значением `0` выбирает ветвь
отказа, а `1` — ветвь завершения миссии; обе срабатывают только при условиях,
описанных ниже. Среда выполнения Rust может
записать соответствующее целевое состояние в собственный
`MissionScriptOutcome`, но не проверяет эти условия и пока не исполняет
команду из диспетчера `.scr`.
`Handler(57)` передаёт тому же обратному вызову режим `2`, для которого в
вызываемой функции есть отдельная ветвь IGame; отдельный путь жизненного цикла
передаёт режим `1`, но ветви для него в `FUN_100611d0` не найдено. По одному
вызову нельзя присвоить общее игровое имя остальным командам или называть весь
`Handler(30)` сообщением, приказом либо кодом интерфейса. Преобразователь
текстовых ссылок в varset находится за обёрткой `0x10011ea0`, в
`0x100174a0`.

`0x100174a0` добавляет каждый
recognized source declaration в encounter order как 48-byte record; GOG shared
`varset.var` не содержит `STRING(...)`, поэтому его 231 numeric `VAR` entries
образуют точно это index space. `0x10013570` возвращает `DWORD` record kind
raw `u32`; float kind проходит `__ftol`, чей x87 rounding profile ещё требует
capture. Полный GOG scan всех 246 Handler(30) instructions показывает 492
operand references: все 492 in-range и указывают на `DWORD`. Поэтому
`VarSet::resolve_handler30` уже materializes точный opaque callback command
`(mode=0, first, second)` для данного corpus path, но явно отклоняет float,
out-of-range и incomplete instructions вместо silent coercion.

Identity ровно равна
`(slot0 word, slot4 IEEE-754 bits, slot5 IEEE-754 bits)`, поэтому `-0.0` и
`+0.0` различаются. Новый 100-byte record получает slot1 в поле `+0x14`,
slot2 одновременно в `+0x24/+0x28`, slot3 в `+0x2c` и slot6 в `+0x0c`. При
совпавшем key refresh случается только когда slot1 сравнивается unequal
(включая NaN); он заменяет `+0x14` и `+0x0c`, затем прибавляет сохранённый
`+0x28` к `+0x24` с x86 wrapping arithmetic. `fparkan-script` отражает эту
изолированную часть как `Handler2RecordScheduler`; он не выполняет bytecode,
не назначает игровых имён и не делает event lookup за original VM.

Шаг прибавления проверен прямым вызовом оригинального helper-а
`FUN_10005070` (RVA `0x5070`) из GOG `ai.dll`, SHA-256
`c19cd6cd31cd2896f8c4ba0f0e700dd3408aec8182daaf4863d98a7d233a2abc`.
Исходник x86 test harness-а хранится в
[`handler2_native_oracle.cs`](../../crates/fparkan-script/tests/handler2_native_oracle.cs):
он сначала сверяет hash DLL, затем вызывает helper с 100-byte record и
проверяет оба поля. Пример воспроизведения в Windows PowerShell:

```powershell
$env:FPARKAN_ORIGINAL_AI_DLL = 'C:\GOG Games\Parkan - Iron Strategy\ai.dll'
& "$env:WINDIR\Microsoft.NET\Framework\v4.0.30319\csc.exe" /nologo /platform:x86 /out:target\Handler2NativeOracle.exe crates\fparkan-script\tests\handler2_native_oracle.cs
.\target\Handler2NativeOracle.exe
```

Полученные значения:

| `+0x24` до вызова | сохранённый `+0x28` | `+0x24` после вызова | `+0x28` после вызова |
| ---: | ---: | ---: | ---: |
| `0x00000004` | `0x00000004` | `0x00000008` | `0x00000004` |
| `0x00000008` | `0x00000004` | `0x0000000C` | `0x00000004` |
| `0xFFFFFFFF` | `0xFFFFFFFF` | `0xFFFFFFFE` | `0xFFFFFFFF` |
| `0xFFFFFFFE` | `0xFFFFFFFF` | `0xFFFFFFFD` | `0xFFFFFFFF` |

Rust-тест повторяет эти начальные значения. Для первой пары обновлений входы
несут slot 2, равный `99` и `7`, однако сохранённый при создании шаг равен `4`;
для второй пары оба счётчика и шаг начинаются с `0xFFFFFFFF`, а новые входы
несут `2` и `17`. Правило refresh читается прямо из модели:

```rust
record.counter = record.counter.wrapping_add(record.increment);
```

Native helper получает только указатель на record, поэтому напрямую проверяет
сложение текущего `+0x24` с сохранённым `+0x28`; свежий slot 2 принадлежит
сценарию Rust-теста, а не аргументам helper-а. Проверка не охватывает полный
scheduler path внутри запущенной игры.

На границе mission runtime выбранный TMA clan `first_resource`
материализуется как отдельный `MissionScriptBundle`: loader нормализует
`<base>.scr`, декодирует его тем же bounded reader-ом и сохраняет clan
provenance. Полная загрузка получает от вызывающей стороны значение
32-битного времени очереди, затем для каждого bundle вычисляет начальное
состояние Behavior и пробует выполнить только восстановленный поднабор первого
`Init` с `Handler(19)`. Остальные события пока остаются прочитанными данными.
Статический просмотр не запускает warm-start Behavior, но сохраняет результат
поддержанных `Handler(19)` writes и создаёт ячейки varset; при неподдержанной
инструкции оставляет default-значения и статус `Unsupported`.

TMA properties остаются four raw `u32` words плюс имя, пока consumer/schema не
задаст тип (integer/float bits/ObjectId/enum/fixed-point/index). В том числе
сохраняются `NOT USED`; corpus подтверждает `Invulnerability`, life state,
`ClanID`, ore, speed и free-time properties.

Research/economy работают в simulation: `LoadResearch` и
`CalcFullResearchCost` доказывают данные и вычислимую стоимость, но не полный
layout prerequisites/modifiers/unlocks. Formula evaluator требует strict
grammar/version, typed operands, deterministic numeric policy, bounded stack и
явных errors; x87-compatible rounding нужен там, где оно выбирает ветку.

### Первый фрагмент dispatcher-а: `Init` и Handler(19)

Этот контракт относится к проверенной сборке GOG: `ai.dll`, SHA-256
`c19cd6cd31cd2896f8c4ba0f0e700dd3408aec8182daaf4863d98a7d233a2abc`, PE32 с
image base `0x10000000`. В ней `FUN_10011f40` (RVA `0x11f40`) ищет имя
события как C-строку: сравнивает байты до первого NUL, с учётом регистра, и
возвращает индекс первого совпадения. Поле `name_len` при поиске не используется.
Если совпадения нет, поиск возвращает `-1`; `FUN_10011f10` (RVA `0x11f10`) не
вызывает обработчик для такого индекса. Поэтому повторные записи `Init` не
исполняются все подряд: выбирается первая.

`FUN_10011f10` передаёт выбранный индекс внутреннему циклу `FUN_10011fb0`
(RVA `0x11fb0`). Цикл начинает с первой вложенной инструкции, сбрасывает индекс
инструкции и глубину стека условий, затем идёт по записям в порядке файла.
В исходном `default.scr` событие `Init` содержит одну инструкцию selector `19`,
control word `0xffffffff` (действие) и result reference `0xffffffff` (результат
не сохраняется). Это наблюдаемая форма пакета; поведение оригинала для остальных
форм здесь не утверждается.

В GOG `default.scr` события `Init` в двух AutoDemo bundles имеют по одной такой
инструкции. Её ссылки `224`, `225`, `226` ведут к объявлениям `DWORD`
`ClanBaseX`, `ClanBaseY` и `ClanID`. `Handler(19)` (двадцатая запись таблицы,
VA `0x1000aa38`, RVA `0xaa38`) пишет их по порядку: поля SuperAI `+0x80` и
`+0x84` переводит через x87 helper `__ftol`, а поле `+0x7c` копирует как
неизменённое слово. После вызова он очищает поле SuperAI `+0x50`; соответствующее
смещение в VM, начинающейся с SuperAI `+8`, равно `+0x48`. Поскольку цели имеют
тип `DWORD`, общий setter сохраняет полученные слова без дополнительного
преобразования. Helper `__ftol` в `ai.dll` (VA `0x1001df70`, RVA `0x1df70`)
сохраняет x87 control word, временно задаёт округление к нулю, выполняет
`FISTP qword` и восстанавливает control word.

Входные координаты идут через два DLL. В проверенной сборке `iron3d.dll`
(SHA-256 `d1dc4ea8535e2069b0a05c9e8be2724dd438ba44cca7a83007d8b2e50f9e35fa`)
функция `FUN_100392e0` преобразует две координаты TMA из `float` в signed
64-bit через x87 `FISTP qword` (RVA `0x39307` и `0x3932f`), а пятый и шестой
аргументы `CreateSuperAI` получает как младшие DWORD этих результатов. Десятый
аргумент — указатель callback. В прямом вызове оригинала пара
`(500.2857, 751.8217)` дала `(500, 752)`, а tie-векторы `(500.5, 501.5)` при
ближайшем режиме дали `(500, 502)`. Например, `-0.6` сначала округляется до
`-1`, а его младший DWORD равен `0xffffffff`.

`CreateSuperAI` в `ai.dll` (VA `0x1000f710`, RVA `0xf710`) выделяет `0x8b0`
байт и вызывает конструктор `FUN_10001000`. Тот передаёт `clan_id`, `base_x` и
`base_y` в `FUN_10006340`, которая заполняет поля позднее читаемые
`Handler(19)`. Значения больше `10000` вызывают исходное предупреждение через
`MessageBoxA`, после которого конструктор продолжает работу; это не отказ и не
ограничение координат. `FUN_10005c70` (RVA `0x5c70`) затем расширяет DWORD до
qword, загружает его через x87 и записывает `float` в поля структуры `+4` и
`+8` — в этой цепочке это SuperAI `+0x80` и `+0x84`. Поэтому `0xffffffff`
превращается в `2^32` при округлении до single precision.
Сам `Handler(19)` применяет `__ftol` с усечением к этим полям и берёт младший
DWORD: для `2^32` получается `0`, а для `10001.0` — `10001`.

Rust повторяет весь числовой путь до DWORD, записываемого в `VarSet`:
nearest-even `FISTP qword`, младший DWORD, преобразование DWORD в `f32`, затем
усечение результата до qword и его младший DWORD. В частности, это сохраняет
наблюдаемый переход `-0.6 -> 0xffffffff -> 2^32 -> 0` и не отвергает `10001`.
Координата, не конечная или не представимая в signed qword на первом шаге,
остаётся явной ошибкой: поведение такой области здесь не моделируется. Rust не
показывает исходное предупреждение `MessageBoxA`; оно находится за пределами
этого числового resolver-а.

Ограниченный Rust-исполнитель `VarSet::resolve_handler19_init` ищет только
первое событие `Init`, проходит его инструкции по порядку и возвращает
упорядоченные записи. Runtime применяет их к отдельной копии varset выбранного
клана лишь после успешного прохода всего события. Отсутствующий `Init` даёт
статус `Missing` и не создаёт записей инициализации; пустой первый `Init` даёт
`Executed` без записей. Если форма инструкции пока не поддержана, package
остаётся доступен в preview режиме со статусом `Unsupported`, а default-значения
ячеек сохраняются; при полной загрузке миссии такая же форма возвращает ошибку с
индексами события и инструкции. Предшествующие частичные записи не применяются.
Повреждённая ссылка или цель неправильного типа остаётся ошибкой в обоих режимах.
Это правила ограниченного Rust-исполнителя, а не обнаруженное поведение
оригинального dispatcher-а.

### Первый вызов Behavior и сценарного `Init`

В той же GOG-сборке `Behavior.dll` (SHA-256
`ddfbdbde07832358fe6e35e01c8dba9b58e7313789498b6049ed51d4ed9baf99`) функция
`FUN_1001adb0` (RVA `0x1adb0`) обновляет один Behavior distributor. Она получает
текущее время через `GetQueue` и queue vslot `+0x38`. Если distributor `+0x10`
равен нулю, функция записывает туда текущее время очереди, считает начальный
deadline `+0x14` по формуле `время + 3*64 + jitter`, где `jitter` лежит в
`0..63` и получен одним шагом глобального MSVCRT LCG. В этой ветви она также
обнуляет пять слов `+0x30..+0x40`. При повторном вызове до deadline ненулевой
`+0x10` оставляет deadline прежним, не расходует новое случайное число и не
очищает эти слова. Поведение после истечения deadline этим опытом не проверено.
Если само время очереди равно нулю, `+0x10` остаётся нулевым, и повторный вызов
снова проходит ветвь начальной подготовки.

Путь host callback в проверенной паре DLL теперь статически известен. В
`iron3d.dll` функция `FUN_1005ff40` (RVA `0x5ff40`) сначала вызывает
`FUN_100392e0` для TMA clan records, а затем для записей вызывает
`FUN_10039440` (RVA `0x39440`). Последняя передаёт AI callback-у аргументы через
vtable slot `+0x14`. Метод `ai.dll!FUN_10001ae0` (RVA `0x1ae0`) сохраняет это
слово аргумента в AI `+0x3fc`, сначала обновляет Behavior через объект `AI+0x438`
и его vslot `+0x2c`, а затем, если latch `AI+0x8a8` ещё сброшен, находит первый
`Init`. При отсутствии события latch остаётся нулевым. Если событие найдено,
метод запускает dispatcher только при нулевом recursion guard `AI+0x78`,
временно ставит guard и снимает его после dispatch; затем ставит latch, даже
если занятый guard пропустил dispatch. Это определяет одну штатную
callback-точку входа и порядок внутри неё, но не раскрывает всех смыслов
входного слова аргумента и не подменяет динамическую трассу полного запуска игры.

`fparkan-runtime::load_mission` получает значение времени очереди явным
аргументом `queue_time_ms`; Rust не читает оригинальную очередь. Для полной
загрузки Rust после регистрации объектов запускает последовательность
подготовки Behavior → ограниченный `Init` по каждому bundle. Путь использует
локальный RNG engine, начиная с состояния `1`; его шаги LCG общие между
bundle, но остальные потребители исходного глобального RNG не моделируются.
В Rust также нет значения аргумента в `AI+0x3fc`, recursion guard `AI+0x78`
или одноразового latch `AI+0x8a8`. Поэтому это связанный начальный поднабор, а
не полная активация SuperAI и не обновление Behavior на игровых тиках. При
ошибке ограниченного `Init` Rust отклоняет полную загрузку и сохраняет прежний
мир и состояние генератора; такая политика отката принадлежит Rust loader-у.

Для проверки использованы три x86 helper-а на синтетической памяти. Caller
oracle вызывает оригинальные `iron3d.dll` entry points и проверяет преобразование
TMA-координат в аргументы `CreateSuperAI`. AI oracle проходит оригинальный
`iron3d.dll!FUN_10039440` → `ai.dll!FUN_10001ae0` → реальный `Init` dispatcher;
также проверяет оба AutoDemo-набора, повторный/missing/busy callback и связку
`FUN_10005c70` → Handler(19). Для входных DWORD `(0xffffffff, 10001)` поля стали
`(2^32, 10001.0)`, а varset получил `(0, 10001, 2)`. Behavior oracle вызывает
`FUN_1001adb0` с искусственно заданной очередью и проверяет шаг генератора,
срок, очистку и повторный вызов. Эти проверки исполняют оригинальные DLL-функции на заданных
структурах, но не являются трассой штатного старта или последующего игрового
цикла.

Проверки можно повторить из корня репозитория в Windows PowerShell:

```powershell
$env:FPARKAN_ORIGINAL_IRON3D_DLL = 'C:\GOG Games\Parkan - Iron Strategy\iron3d.dll'
$env:FPARKAN_ORIGINAL_AI_DLL = 'C:\GOG Games\Parkan - Iron Strategy\ai.dll'
$env:FPARKAN_ORIGINAL_BEHAVIOR_DLL = 'C:\GOG Games\Parkan - Iron Strategy\Behavior.dll'
$env:FPARKAN_ORIGINAL_DEFAULT_SCR = 'C:\GOG Games\Parkan - Iron Strategy\MISSIONS\SCRIPTS\default.scr'
New-Item -ItemType Directory -Force -Path target\handler19-native-oracles | Out-Null
& "$env:WINDIR\Microsoft.NET\Framework\v4.0.30319\csc.exe" /nologo /platform:x86 /out:target\handler19-native-oracles\AnchorOracle.exe crates\fparkan-runtime\tests\handler19_anchor_native_oracle.cs
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
& "$env:WINDIR\Microsoft.NET\Framework\v4.0.30319\csc.exe" /nologo /platform:x86 /out:target\handler19-native-oracles\InitOracle.exe crates\fparkan-runtime\tests\handler19_init_native_oracle.cs
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
& "$env:WINDIR\Microsoft.NET\Framework\v4.0.30319\csc.exe" /nologo /platform:x86 /out:target\handler19-native-oracles\BehaviorWarmStartOracle.exe crates\fparkan-runtime\tests\behavior_warm_start_native_oracle.cs
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
& .\target\handler19-native-oracles\AnchorOracle.exe
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
& .\target\handler19-native-oracles\InitOracle.exe
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
& .\target\handler19-native-oracles\BehaviorWarmStartOracle.exe
exit $LASTEXITCODE
```

Caller oracle проверяет AutoDemo пару `(500.2857, 751.8217)`, четыре режима
x87 на tie-векторах, границу `10000` и младший DWORD. AI oracle отдельно
проверяет, что `FUN_10005c70(0xffffffff, 10001)` даёт поля `(2^32, 10001.0)`,
а обработчик записывает `(0, 10001, 2)`.

`GetSuperAI` возвращает элемент `n` из таблицы на 64 указателя по RVA `0x55398`
в `ai.dll`, если `n <= 63`. В режиме только чтения у запущенной GOG AutoDemo
одновременно сняты исходные координаты кланов в `iron3d.dll` и поля созданных
SuperAI в этой таблице. Для первого клана raw TMA-слова `0x43fa2492` и
`0x443bf497` декодируются как `(500.2857, 751.8217)`, а соответствующие поля
SuperAI `+0x80`, `+0x84`, `+0x7c` содержали `(500.0, 752.0, 0)`. Для второго
клана сырые координаты и поля совпали: `(728.0, 449.0, 1)`. Связанный снимок
исходных и конечных значений исключает усечение, направление вверх и направление
вниз в реально запущенной игре. Отдельный caller oracle вызывает оригинальный
`FUN_100392e0` при явно заданных x87 RC-режимах, включая nearest; сравнение
tie-векторов подтверждает реакцию `FISTP`, но не измеряет control word процесса.
Профиль живой игры выводится из связанной пары raw X/Y → SuperAI.

Нижнеуровневый `VarSet::resolve_handler19` принимает уже преобразованные первые
два слова и третье исходное слово, выдаёт три типизированные записи и отклоняет
отсутствующие, выходящие за диапазон или не-`DWORD` цели. Метод
`resolve_handler19_init` добавляет выбор события и остановку на неподдержанной
форме инструкции. Всё остальное содержимое `.scr` сохраняется reader-ом, но
остаётся вне этого исполнения.

### Аргументы и обратный вызов Handler(30)

`resolve_handler30_with_values` сохраняет восстановленное ABI типов объявлений,
но читает аргументы из экземпляра ячеек varset конкретного клана, а не из
текстовых значений по умолчанию. `resolve_loaded_handler30` возвращает точную
команду обратного вызова `(mode=0, first, second)`, не приписывая ей
дополнительный смысл. Отдельная функция `apply_loaded_script_host_callback`
моделирует в Rust состояние исхода для `(0, 0, payload=0|1)`; она не выбирает
событие `.scr` и не вызывает машинный код оригинала.

Снимок работающей GOG AutoDemo определяет обратный вызов: прочитанный без записи
указатель по `ai.dll + 0x555e4` равен `0x100611d0`, то есть `iron3d.dll +
0x611d0` при наблюдавшейся базе загрузки. Восстановленное ABI вызываемой
функции — `__cdecl(mode, command, payload)`; `Handler(30)` передаёт те же поля как
`(0, first, second)`. Обратный вызов принимает интересующую нас команду только
при `mode == 0`, `command == 0` и нулевом байте `IGame + 0xe5`. Ветвь отказа
(`payload == 0`) запускает `VOICE_MISSION_FAIL`, записывает `1` в слово `IGame + 8`
и обнуляет байт `*(IGame + 4) + 0x157`, только если прежнее слово не равно `1`.
Ветвь завершения (`payload == 1`) запускает `VOICE_MISSION_COMPLETE`, записывает
`0` в слово и устанавливает байт в `1`, только если прежнее слово не равно `0`.
Оба звуковых вызова предшествуют этим записям. Команды 3, 4 и 5 ведут к другим
ветвям игры; режим `2` выполняет отдельный вызов IGame, а ветви для режима `1`
в этой функции нет.

Статический анализ восстановил также `command == 1`, но не дал основания
присвоить этой ветви предметное имя. Её аргумент `payload` — знаковый ключ
бинарного дерева поиска. Поле `+0x04` у получателя указывает на корневой
сторожевой узел; в каждом узле ссылки на дочерние узлы лежат по `+0x08` и
`+0x0c`, ключ — по `+0x10`, а передаваемый дальше аргумент — по `+0x14`. При
совпадении ключа вызывается `FUN_10095600(node + 0x14)`. У этого объекта есть
одноразовый флаг `+0x19` и второй флаг `+0x18`. При первом вызове Iron3D
устанавливает `+0x19`, получает строку из менеджера ресурсов через виртуальный
слот `+0x1c` и передаёт значение `+0x0c` объекта-аргумента в `FUN_10061fb0`.
При повторных вызовах используются идентификаторы менеджера ресурсов `0x181a`
и `0x184f`, селектор IGame `0x2ed` и непрозрачный запрос с категорией `3` или
`4`, выбираемой по `+0x18`. Это подтверждает поиск и различие первого и
повторных вызовов, но не назначение сообщения интерфейса или смысл этих
идентификаторов; в Rust команда `1` поэтому остаётся `Unhandled`.

### Состояние обратного вызова исхода миссии

Слово `IGame + 8` создаётся не загрузчиком миссии. В исследованной GOG-сборке
`iron3d.dll` (SHA-256
`d1dc4ea8535e2069b0a05c9e8be2724dd438ba44cca7a83007d8b2e50f9e35fa`, image base
`0x10000000`) конструктор `FUN_1005c820` записывает туда `5` (RVA `0x5c84f`), а
байт `IGame + 0xe5` обнуляет (RVA `0x5c9f6`). При настройке игры функция
`FUN_1005cbb0` задаёт `+0xe5` выражением `(*AppState == 3)` (RVA `0x5cc4a`);
обработчик таймера главного меню выставляет режим `3` для AutoDemo (RVA `0x70f3`).
Отдельный обработчик выбора миссии записывает режим `1` (RVA `0x29142`) перед
переходом к пути `missions/single.*`. После успешной стандартной инициализации
игровой цикл записывает в `IGame + 8` значение `4` (`FUN_1005eb70`, RVA `0x5eb70`).
Этот переход запуска не выполняется при статическом просмотре миссии и здесь не
моделируется.

`MissionRequest` в Rust несёт явный `MissionMode::{SingleMission, AutoDemo}`;
режим задаёт вызывающая сторона и его нельзя выводить из имени пути. Во время
загрузки Rust инициализирует своё слово состояния значением `5`, как у нового
IGame, и сохраняет запрошенный режим; сама загрузка не вызывает нативный
конструктор.
Публичный `apply_loaded_script_host_callback` применяет те же интересующие нас
проверки: режим `AutoDemo` закрывает ветвь, а в `SingleMission` обратный вызов
меняет только слово состояния и исход Rust, если выполняется исходное условие
по этому слову. Поэтому при состоянии `4` ветвь завершения разрешена, как и
ветвь отказа, пока слово не равно `1`. Повторный отказ при состоянии `1` либо
завершение при состоянии `0` ничего не меняют; если тест намеренно
рассинхронизирует исход Rust и слово состояния, повтор оставляет это
расхождение. Так работает исходная проверка слова, а не выравнивание всех полей.

Изолированное сравнение с оригинальной DLL выполнено небольшой тестовой
x86-программой `crates/fparkan-runtime/tests/original_host_callback_oracle.cs`:
программа проверяет SHA-256, загружает `iron3d.dll` и вызывает RVA `0x611d0` с
синтетическими IGame и AppState. Для изоляции проверки она временно подставляет
синтетический IGame через глобальную переменную `DAT_1010a5f8` (RVA `0x10a5f8`)
и заменяет слот таблицы импортов звукового интерфейса (RVA `0xe42cc`)
регистратором; машинный код обратного вызова остаётся исходным. Проверка сверяет
записи полей, повторные/обратные переходы, блокирующий `+0xe5` и порядок вызова
звука. Это
подтверждает локальную семантику обратного вызова, но не запуск исходной миссии,
диспетчеризацию Handler(30) или весь игровой цикл. Rust пока не запускает
диспетчеризацию сценария автоматически. Команды 1, 3, 4, 5 и режим 2 остаются
`Unhandled` в этом обработчике исхода.

Повторить из корня репозитория в PowerShell на Windows с установленными GOG
ресурсами можно так:

```powershell
$env:FPARKAN_ORIGINAL_IRON3D_DLL = 'C:\GOG Games\Parkan - Iron Strategy\iron3d.dll'
New-Item -ItemType Directory -Force -Path target\original-host-callback-probe | Out-Null
& 'C:\Windows\Microsoft.NET\Framework\v4.0.30319\csc.exe' /nologo /platform:x86 /out:target\original-host-callback-probe\HostCallbackOracle.exe crates\fparkan-runtime\tests\original_host_callback_oracle.cs
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
& .\target\original-host-callback-probe\HostCallbackOracle.exe
exit $LASTEXITCODE
```

### Handler(8): problem-record state write

`Handler(8)` is the ninth VM-table entry at GOG `ai.dll` VA `0x10009b0d`.
All 179 corpus records have exactly one in-range `DWORD` reference; the two
observed entries are `ST_SOLVING=1` (122 records) and `ST_SOLVED=2` (57).
The handler resolves loader-bound `dCurrentProblem` through varset index
`this+0x868`, uses that live DWORD as a bounds-checked index into a table at
`this+0xa0` with 100-byte records, then resolves the instruction's one DWORD
and writes it to the selected record at `+0x18`.

The write has two statically proven exceptional branches. State `2` calls a
reset helper that zeroes record words `0..=3` and `6` before invoking two
opaque callback slots; state `3` does the same except word `3` is preserved.
Both then write `+0x18`. Every other state simply writes the state word.
`VarSet::resolve_handler8` emits a `Handler8StateChange` with the caller-owned
live record index, resolved state, and explicit reset kind. It does not invent
the table owner, the pre-reset helper, or callback semantics.

### Handler(15): typed target-call boundary

`Handler(15)` is the sixteenth VM-table entry at GOG `ai.dll` VA `0x10008054`
and the second most frequent non-sentinel selector: 236 records in 28 of 58
GOG packages. It is not a one-word opcode. Across the complete corpus,
references `0..3` are `DWORD`, `4..7` are `float`, and `8` is the DWORD mode.
The original resolves the first four through `0x10013570`, the next four
through x87 scalar helper `0x10013190`, then reads the mode through
`0x10013570`.

The shipped `varset.var` names the only observed mode values: `NONE=0` uses
9 total references; `TARGET_BY_LOGIC_ID=0x0201`, `TARGET_BY_TYPE=0x0203`,
`TARGET_NOT_DEFINED=0x0204`, and `TARGET_BY_NAME=0x0205` use 10; and
`TARGET_BY_PLACE=0x0202` uses 11. The corpus contains 34/122/80 records in
these three arities. The trailing references at positions 9 and, only for
`TARGET_BY_PLACE`, 10 are all in-range DWORD declarations.

After resolving those inputs, the handler looks up an opaque target through
virtual slot `+0x1c` on `this+0x3d8` using the first word. A missing target
sets VM flag `+0x50=5`; otherwise the handler builds a temporary call record,
applies the mode-specific tail, and invokes the target's `+0x0c` virtual slot
with that record and the third word. Its zero/non-zero result becomes
`+0x50=0/1`. The target type, the virtual method's semantic action, and its
return value remain unproven. Accordingly `VarSet::resolve_handler15` only
materializes a type-checked `Handler15Invocation` and `Handler15TargetPayload`;
it never executes the opaque target call. Missing, out-of-range, wrong-type,
and unobserved-mode inputs are explicit errors.

## Готовность

Все demo packages должны проходить package/version checks, offsets оставаться
в bytecode, а confirmed disassembler — не терять синхронизацию. VM считается
готовой после deterministic Init/basic mission events, stable object bindings,
typed research/property tests и save/load script state. Для закрытия остаются
dispatcher/jump table, minimal differential packages и traces world/variable
effects; до них unknown opcode — явная unsupported branch, не no-op.
