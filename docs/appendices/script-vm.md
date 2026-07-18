# Сценарная VM, формулы и игровые свойства

## Подтверждённый surface

Миссионный сценарный слой задаёт стартовые события, completion/failure,
messages, teleports, задачи, research и campaign transitions. Точки входа и
файлы: `ai.dll: CreateSuperAI/GetSuperAI`, `MisLoad.dll: LoadResearch`,
`ArealMap.dll: CalcFullResearchCost`, `MISSIONS/SCRIPTS/*.scr`, `*.fml`,
`*.trf`, `varset.var`, `MISSIONS/dispatcher.ini`, `mission.cfg`, `messages.cfg`
и `briefing.cfg`.

`.scr` — binary package с version checks, symbol/event sections и offsets;
полная opcode grammar не доказана. Его внешний framing теперь читает
`fparkan-script`: первый little-endian `u32` является числом required opcode
handlers, второй — числом event records. Каждый event хранит `name_len`,
`name_len + 1` raw bytes с обязательным NUL, opaque event word и count вложенных
records. Вложенный record сохраняет семь `u32` header words (в disk order),
список `u32` references после шестого header word и trailing seventh word.
Никакой из этих words ещё не получает semantic name. `.fml` — текстовый
symbol/formula oracle; `varset.var` задаёт `VAR(...)`/`STRING(...)` defaults;
`.trf` — NRes tables, чей framing подтверждён, а field semantics местами лишь
consumer-inferred.

## Безопасная модель исполнения

Новая VM разделяет immutable package (bytecode, symbols, events, constants),
per-mission variables/timers/frames, bindings logical-name/ObjectId/clan/
research key и typed commands к World3D/Behavior/UI/campaign. После varset
defaults и bindings она dispatches Init/start, на каждом tick обновляет timers,
ставит готовые events в стабильную очередь и исполняет bounded instruction
budget. Опасное удаление идёт через World3D queue и общий deferred lifecycle.

До восстановления opcode table package mode читает header/strings/symbols/
event offsets/raw bytecode losslessly. Статический анализ уже выделил отдельный
five-way evaluator condition records (`ai.dll` VA `0x10005180`): tags `1..5`,
type guards, object lookup и completion flag. Это не следует выдавать за
instruction dispatcher или jump table `.scr`: bytecode opcode table всё ещё
требует отдельного доказательства. Unknown opcode нельзя пропустить как один
byte: это ломает синхронизацию. Для каждого доказанного opcode фиксируются
number, size, operands, control flow, effects, errors и минимальный test.

GOG `ai.dll` доказывает этот framing двумя consumer-ами: loader по
`0x10001000` открывает `<bundle>.scr`, `varset.var`, `<bundle>.fml`, затем
собирает ровно 73 pointers handlers; `0x10011b20` читает описанную count-driven
структуру. Команда

```powershell
cargo run -p fparkan-cli -- script inspect `
  'C:\GOG Games\Parkan - Iron Strategy\MISSIONS\SCRIPTS\c1m2p.scr' --format json
```

на исходном пакете возвращает `opcode_handler_count=73`, 9 events, 17 nested
records, 20 references и 0 trailing bytes. Это corpus evidence для reader-а,
но не разрешение на исполнение неизвестных 73 opcodes.

Теперь установлен selector: loader `0x10001000` создаёт 73 pointers в
фиксированном порядке, а `0x10011e70` копирует их без перестановки в runtime
array. Во всех 58 GOG `.scr` первый header word каждого nested record равен
`0..72` либо `0xffff_ffff`: соответственно 2095 handler selectors и 3992
sentinel records. Поэтому `ScriptInstruction::dispatch_selector()` возвращает
`Handler(0..72)`, `Sentinel` или сохраняемый `Unknown(u32)`. Первый handler
(`Handler(0)`, VA `0x10008034`) только устанавливает current context и flag
`+0x50 = 1`; это не даёт ему игрового имени и не заменяет runtime trace.

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

`Handler(2)` (третья entry table, VA `0x10009610`) уже имеет статический
contract, но ещё не Rust execution: он выбирает active event/instruction через
runtime offsets `+0x48/+0x4c`, разрешает семь 32-bit slots через varset object
`+0x18`, допускает integer-kind `5` и float-kind `3` для трёх numeric slots
(`+0x10/+0x14/+0x18`, иначе использует `0.0`), затем вызывает opaque AI object
по `this + 0x7c` и очищает flag `+0x50`. Три первых slots передаются без
доказанной semantic type/name. Следующий закрывающий capture должен записать
raw seven slots, resolved variable values и вход/выход вызова `0x100059f0` в
controlled mission. До него compatibility VM возвращает явный unsupported
result, а не «примерный» game command.

На границе mission runtime выбранный TMA clan `first_resource` теперь
материализуется как отдельный `MissionScriptBundle`: loader нормализует
`<base>.scr`, декодирует его тем же bounded reader-ом и публикует immutable
package вместе с clan provenance. Headless report выводит число таких packages
и их named events. Это именно wiring входных данных, не VM execution: Init и
остальные events пока не dispatch-ятся, а ошибка чтения сохраняет
transactional rollback mission loader-а.

TMA properties остаются four raw `u32` words плюс имя, пока consumer/schema не
задаст тип (integer/float bits/ObjectId/enum/fixed-point/index). В том числе
сохраняются `NOT USED`; corpus подтверждает `Invulnerability`, life state,
`ClanID`, ore, speed и free-time properties.

Research/economy работают в simulation: `LoadResearch` и
`CalcFullResearchCost` доказывают данные и вычислимую стоимость, но не полный
layout prerequisites/modifiers/unlocks. Formula evaluator требует strict
grammar/version, typed operands, deterministic numeric policy, bounded stack и
явных errors; x87-compatible rounding нужен там, где оно выбирает ветку.

## Готовность

Все demo packages должны проходить package/version checks, offsets оставаться
в bytecode, а confirmed disassembler — не терять синхронизацию. VM считается
готовой после deterministic Init/basic mission events, stable object bindings,
typed research/property tests и save/load script state. Для закрытия остаются
dispatcher/jump table, minimal differential packages и traces world/variable
effects; до них unknown opcode — явная unsupported branch, не no-op.
