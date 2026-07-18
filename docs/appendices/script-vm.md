# Сценарная VM, формулы и игровые свойства

## Подтверждённый surface

Миссионный сценарный слой задаёт стартовые события, completion/failure,
messages, teleports, задачи, research и campaign transitions. Точки входа и
файлы: `ai.dll: CreateSuperAI/GetSuperAI`, `MisLoad.dll: LoadResearch`,
`ArealMap.dll: CalcFullResearchCost`, `MISSIONS/SCRIPTS/*.scr`, `*.fml`,
`*.trf`, `varset.var`, `MISSIONS/dispatcher.ini`, `mission.cfg`, `messages.cfg`
и `briefing.cfg`.

`.scr` — binary package с version checks, symbol/event sections и offsets;
полная opcode grammar не доказана. `.fml` — текстовый symbol/formula oracle;
`varset.var` задаёт `VAR(...)`/`STRING(...)` defaults; `.trf` — NRes tables,
чей framing подтверждён, а field semantics местами лишь consumer-inferred.

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
