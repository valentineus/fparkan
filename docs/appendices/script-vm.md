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
находится в отдельном позднем update path. Воспроизводимый read-only extractor:
`tools/ghidra/ExportAiVmHandler2Dispatch.java`.

Corpus priority теперь измерен, а не предполагается: во всех 58 GOG `.scr`
имеются 6 087 instruction records, из них 3 992 sentinel; самый частый
non-sentinel selector — `Handler(30)`, 246 records. Его VA `0x1000c266`
читает первые два reference words активной instruction, разрешает каждый
через varset (`0x10002d30` и `0x10013570`) и вызывает внешний callback с
тремя `u32`: `(0, first, second)`. Callback не принадлежит `ai.dll`: его
кладёт десятый argument экспортного `CreateSuperAI`. Тот же callback встречен
у `Handler(57)` с первым word `2` и у отдельного lifecycle path с первым word
`1`; предметная семантика этих modes ещё не доказана. В частности, это пока
не основание назвать Handler(30) сообщением, приказом или UI opcode. Точный
text-to-varset resolver расположен за wrapper `0x10011ea0` в
`0x100174a0`. Воспроизводимые exports: `ExportAiVmHandler30.java`,
`FindAiVmHandler30Callback.java`, `ExportAiVarSetLoader.java`.

Следующий pass восстанавливает эту индексацию. `0x100174a0` добавляет каждый
recognized source declaration в encounter order как 48-byte record; GOG shared
`varset.var` не содержит `STRING(...)`, поэтому его 231 numeric `VAR` entries
образуют точно это index space. `0x10013570` возвращает `DWORD` record kind
raw `u32`; float kind проходит `__ftol`, чей x87 rounding profile ещё требует
capture. Полный GOG scan всех 246 Handler(30) instructions показывает 492
operand references: все 492 in-range и указывают на `DWORD`. Поэтому
`VarSet::resolve_handler30` уже materializes точный opaque callback command
`(mode=0, first, second)` для данного corpus path, но явно отклоняет float,
out-of-range и incomplete instructions вместо silent coercion. Extractors:
`ExportAiVarSetParser.java`, `ExportAiVarSetU32Resolver.java`.

Следующий static pass закрывает equality/update policy. Identity ровно равна
`(slot0 word, slot4 IEEE-754 bits, slot5 IEEE-754 bits)`, поэтому `-0.0` и
`+0.0` различаются. Новый 100-byte record получает slot1 в поле `+0x14`,
slot2 одновременно в `+0x24/+0x28`, slot3 в `+0x2c` и slot6 в `+0x0c`. При
совпавшем key refresh случается только когда slot1 сравнивается unequal
(включая NaN); он заменяет `+0x14` и `+0x0c`, затем прибавляет сохранённый
`+0x28` к `+0x24` с x86 wrapping arithmetic. `fparkan-script` отражает эту
изолированную часть как `Handler2RecordScheduler`; он не выполняет bytecode,
не назначает игровых имён и не делает event lookup за original VM.

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

### Handler(19): AutoDemo Init varset initialization

`Handler(19)` is the twentieth VM-table entry at GOG `ai.dll` VA `0x1000aa38`.
It is the only instruction in the `Init` event of the two `default.scr` bundles
referenced by `MISSIONS\\Autodemo.00\\data.tma`; together those bundles account for
the observed 18 named script events. Each instruction references varset records
`224`, `225`, and `226`: `ClanBaseX`, `ClanBaseY`, and `ClanID` respectively.

The original writes three raw DWORD values in order. It converts the VM fields
at `+0x80` and `+0x84` through the x87 `__ftol` helper and stores the resulting
words into references 0 and 1. It copies the raw word from `+0x7c` into reference
2, then clears VM field `+0x50`. The default targets are `DWORD` records; the
shared setter preserves the incoming word for that type. Therefore this is not
a license to replace the first two conversions with Rust float casts: their
rounding behavior remains an x87 compatibility boundary until it has captured
test vectors.

The missing source-field provenance is now constrained by the public creation
boundary. `CreateSuperAI` at `ai.dll` VA `0x1000f710` allocates `0x8b0` bytes
and calls constructor `0x10001000` with its first eight arguments. That
constructor calls `0x10006340(this + 0x7c, clan_id, base_x, base_y)`: these are
the fields later read by `Handler(19)`. `base_x` and `base_y` are unsigned and
the constructor rejects values greater than `10000`. The actual `__ftol` helper
at `0x1001df70` saves the x87 control word, sets its rounding-control bits to
truncate, executes `fistp qword`, then restores the control word.

Consequently the runtime resolves and retains this one proved vertical slice
during mission loading: each selected clan's TMA anchor is accepted only in the recovered
`0..=10000` base range, truncated through the recovered x87 rule, and paired
with its zero-based clan index. For every `Init` instruction whose selector is
`Handler(19)`, `VarSet::resolve_handler19` produces the three per-clan DWORD
writes. The runtime then materializes an independent declaration-ordered value
array for each selected clan and applies those writes, so later recovered
handlers can consume `ClanBaseX`, `ClanBaseY`, and `ClanID` as runtime cells
rather than loader defaults. Other Init selectors and all other events remain
decoded but unexecuted. AutoDemo validates the path end-to-end: its non-integral first
anchor (`500.2857`) yields captured `ClanBaseX=500`, and the live GOG process
contains two initialized SuperAI entries `(500, 752, 0)` and `(728, 449, 1)`;
the Rust loader reports `script_init_states=2` and `script_varset_states=2`.

`GetSuperAI` returns element `n` of the 64-pointer global table at preferred
`ai.dll + 0x55398` for `n <= 63`. The read-only
`tools/capture-ai-init.ps1` probe observed the running GOG AutoDemo values
`(500, 752, 0)` for entry 0 and `(728, 449, 1)` for entry 1 at fields
`(+0x80, +0x84, +0x7c)`. These values are integral samples, not a rounding
profile.

The Rust reader exposes `VarSet::resolve_handler19`. It accepts the already
converted first two words and the third raw word, produces three typed writes,
and rejects missing, out-of-range, or non-`DWORD` targets. The runtime only
binds it to the proven creation/anchor path above; it does not guess the
remaining script event semantics. The associated Ghidra scripts are
`ExportAiVmHandler19.java`, `ExportAiVmHandler19Setter.java`,
`ExportAiVmHandler19SetterCallee.java`, and `ExportAiGetSuperAi.java`.
The creation and conversion boundaries are reproducible with
`ExportAiCreateSuperAi.java`, `ExportAiSuperAiConstructor.java`, and
`ExportAiFtol.java`.

### Runtime Handler(30) operand binding

`resolve_handler30_with_values` preserves the recovered declaration-kind ABI
but reads operands from instantiated per-clan cells rather than textual
defaults. Runtime exposes `resolve_loaded_handler30` for the exact opaque
`(mode=0, first, second)` callback command. It intentionally returns that
command without invoking a guessed game-side consumer: the tenth
`CreateSuperAI` callback argument still needs its own recovery.

The live GOG AutoDemo closes that consumer boundary: the read-only callback
pointer at `ai.dll + 0x555e4` is `0x100611d0`, or `iron3d.dll + 0x611d0` at
the observed load base. Its recovered `__cdecl` ABI is `(mode, command,
payload)`, matching `Handler(30)` as `(0, first, second)`. In `mode == 0`,
`command == 0` and `payload == 0` selects `VOICE_MISSION_FAIL`, records the
failed status, and clears an IGame byte; `payload == 1` selects
`VOICE_MISSION_COMPLETE`, records completion, and sets that byte. Commands
1, 3, 4, and 5 have additional game-side paths; mode 2 is a separate IGame
call. Those branches are not yet assigned Rust gameplay meanings. Reproduce
the current evidence with `capture-ai-init.ps1` and
`ExportIron3dAiCallback.java`.

Runtime now applies only this recovered branch as
`apply_loaded_script_host_callback`: `(0, 0, 0)` transitions a loaded mission
to `Failed`, `(0, 0, 1)` transitions it to `Completed`, and a repeated target
state is a no-op just as the Iron3D guards require. The effect is deliberately
separate from audio/UI playback; commands 1, 3, 4, 5 and mode 2 return
`Unhandled` until their consumers are recovered.

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
the table owner, the pre-reset helper, or callback semantics. Reproduce the
evidence with `ExportAiVmHandler8.java`, `ExportAiVmHandler8Callees.java`, and
`ExportAiVmHandler8Transitions.java`.

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
and unobserved-mode inputs are explicit errors. Reproduce the static evidence
with `tools/ghidra/ExportAiVmHandler15.java`.

## Готовность

Все demo packages должны проходить package/version checks, offsets оставаться
в bytecode, а confirmed disassembler — не терять синхронизацию. VM считается
готовой после deterministic Init/basic mission events, stable object bindings,
typed research/property tests и save/load script state. Для закрытия остаются
dispatcher/jump table, minimal differential packages и traces world/variable
effects; до них unknown opcode — явная unsupported branch, не no-op.
