# Границы знания

Этот раздел перечисляет области, где контракт ещё не закрыт полностью. Они не
мешают безопасному чтению и lossless сохранению, но не должны превращаться в
authoring API без динамического подтверждения.

## Render state

Доказаны frame boundaries, world traversal, material resolve и крупные проходы.
Не доказаны символами точные имена renderer vtable slots, полный набор CShade
state transitions и окончательный порядок части transparent/FX/shadow subpasses.

Закрывающий эксперимент: запустить оригинал в совместимой Windows/DirectX
среде, перехватить DirectDraw/Direct3D calls и surface flips, сохранить state
log на минимальных сценах с одним типом материала.

## FXID field-level semantics

Размеры команд, resource references, lifecycle, flags families и используемые
time modes известны. Не закрыто значение каждого поля body opcodes 1--10,
отсутствующий во всех проверенных каталогах opcode 6 и точные формулы редких
time modes.

Закрывающий эксперимент: изменять по одному полю копии эффекта, воспроизводить
его в контролируемой сцене и логировать runtime command object, emitted
primitives, sound events и reads в `Effect.dll`.

## Script VM

Доступны packages, symbols, event sections, variable declarations и version
checks. Полная instruction grammar `.scr`, semantics opcodes и serialization
state ещё не восстановлены.

Закрывающий эксперимент: найти dispatcher loop в `ai.dll`, сопоставить jump
table с instruction sizes, построить disassembler и сравнить выполнение
коротких scripts с оригиналом.

## Saves and campaign state

Найдены `saveslots.cfg` и `missions/dispatcher.ini`, но binary savegame payload,
serialization World3D/AI/script/RNG и migration rules не закрыты.

Нужны сохранения оригинала в контролируемых состояниях: старт миссии, изменение
позиции, здоровья, order/path, FX/timer, script variable, research/economy,
mission completion, pause и non-default game time.

## Physical/control formats

CTLD и связанные resources структурно читаются, count patterns и variants
известны. Не названы все секции, shape types, coefficients и точный contact
solver. То же относится к редким MSH auxiliary streams и части CTPT/NDPR flags.

Закрывающий эксперимент: трассировать `LoadControlSystem`,
`LoadPhysicalModel`, `CreateCollManager` и создание collision objects; связать
каждый изменяемый field с созданным shape, contact или реакцией на движение.

## DirectPlay wire

DirectPlay lifecycle и имена игровых messages известны. Wire framing, payload
schema, reliability flags и `netZipData` требуют записи обмена двух
оригинальных клиентов.

Native interoperability подтверждается только успешным обменом original client
<-> compatibility implementation в обе стороны.

## Shell, HUD, шрифты и локализация

Граница shell подтверждена exports `createShell/getIShell`, `IGUIServer`,
верхнеуровневым UI-pass и файлами `ui/*.cfg`, `DATA/TextRes.cfg`,
`gamefont.rlb` и `sprites.lib`. RsLi framing библиотек закрыт, но widget tree,
layout rules, glyph metrics, sprite command semantics, focus/navigation и HUD
state machine пока не восстановлены до field-level спецификации.

Закрывающий эксперимент: трассировать загрузку `shell_ctrls.cfg`,
`menu_resources.cfg`, `cursor.cfg`, `game_resources.cfg` и `hq.cfg`, сопоставить
GUI object factories и снять command/event captures для меню, HUD, briefing и
диалогов.

## Research, economy and properties

Экспорты `LoadResearch`, `CalcFullResearchCost`, TRF/preload resources и TMA
properties доказывают отдельный слой исследований, стоимости, добычи и
производственных параметров. Формулы стоимости, dependency graph технологий,
inventory/economy transitions и точная типизация всех 16-byte property values
не закрыты.

Закрывающий эксперимент: сопоставить research functions с ресурсами и UI,
снять изменения state на контролируемых покупках/исследованиях и построить
typed schema свойств по consumers, а не по одному имени.

## Rare branches

- `Land.map poly_count > 0`;
- RsLi adaptive methods `0x080` и `0x0A0`;
- Texm formats 556 и 88;
- FX opcode 6;
- редкие material flags и MSH auxiliary streams.

Такие ветки реализуются по бинарному коду и synthetic tests, а статус
corpus-verified получают только после реального файла или runtime trace.

## Dynamic-stage requirements

Оставшиеся вопросы нельзя закрыть только статическими архивами. Нужна
изолированная 32-bit Windows-среда, неизменённые игровые каталоги, manifest
SHA-256, debugger, API/vtable hooks, controlled clocks/input и автоматический
launcher, который восстанавливает snapshot, запускает один test case, собирает
логи и завершает процесс без ручного вмешательства.

Для каждого capture сохраняются build profile, module hashes, mission/resource
key, configuration, device profile, initial state, input/time script и версии
инструментов.

## Local evidence requests

На текущем рабочем месте закрыты статические, corpus и headless runtime gates.
Для macOS Desktop GL есть только безопасный command/state trace и исторический
одноразовый offscreen pixel probe:

- `cargo test -p fparkan-render-gl --offline desktop_gl33_triangle_command_capture`;
- `fixtures/acceptance/macos-gl33-triangle-capture.json`.

`S3-GL-001` не считается закрытым: временный `rustc` probe создал CGL/OpenGL
offscreen FBO, выполнил shader-based triangle draw, прочитал RGBA pixels и
сохранил hash capture, но постоянный workspace adapter по-прежнему не создаёт
SDL window, GL context, GPU resources, shader programs, draw calls или present.
Probe не добавляет project-owned `unsafe` в workspace и остаётся только external
evidence request artifact.

Для повышения `S3-GL-001` до `covered` нужен постоянный macOS backend через
выбранную safe facade stack: SDL event/window/context lifecycle, Desktop GL 3.3
shader/buffer/texture/draw/present path, hidden-window/offscreen smoke test и
licensed local model/terrain frame capture.

Для повышения `S3-GL-002` до `covered` всё ещё нужен воспроизводимый GLES2
backend profile: GLES2 должен создать кадр, сохранить pixel capture и тот же
command/state trace. Локальный Docker probe существующего Rust image не нашёл
`libGL`, `libEGL`, `libGLES` или `libOSMesa`, поэтому закрытие этого gate требует
отдельно предоставленного Docker image с Rust + Mesa/EGL/OSMesa либо разрешения
на установку соответствующего проверочного окружения.

Для текущей macOS-focused цели `S3-GL-002`, `L3-DEVICE-001` и `L5-RG40-001`
помечены как `omitted`: они остаются требованиями portable target scope, но не
блокируют локальный macOS acceptance-аудит. При возврате RG40XX/GLES2 в область
цели эти gates снова должны требовать внешнего evidence.

`L3-DEVICE-001` и `L5-RG40-001` не закрываются локально без RG40XX H или
эквивалентного удалённого runner-а. Требуемое доказательство: запуск выбранной
миссии при 640x480 на целевом профиле, сохранённые stdout/stderr, build
fingerprint, manifest игрового каталога, frame/tick budget, memory budget и
итоговый pass/fail report. Desktop/headless результаты не считаются заменой
on-device smoke.

## Closure criteria

Вопрос считается закрытым только при наличии build fingerprint, raw trace,
parser trace-а, минимального воспроизводимого input/resource/save/message,
формального контракта или явно ограниченной гипотезы, differential test для
изменённых DLL, обновления тематической главы и regression case, запускаемого
без ручного анализа.
