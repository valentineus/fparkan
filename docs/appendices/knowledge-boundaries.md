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

Для GOG `ai.dll` статически восстановлены пять ветвей evaluator-а 16-byte
записей условий по RVA `0x5180`, включая проверки типов, поиск объектов,
короткие выходы из обхода списков и запись байта завершения. Однако не найден
код-производитель этого массива и контракт его владельца с runtime. Reader
`FUN_10011b20` хранит описатели событий и вложенных записей `.scr` во внутренних
контейнерах с шагом `0x14` и `0x20`; сами дисковые структуры имеют переменную
длину из-за length-prefixed имён и списков ссылок. Связь этих контейнеров с
массивом evaluator-а не доказана. Виртуальные методы,
через которые проходят проверки, остаются внешней границей, а сам evaluator
не сравнивался с работающим оригиналом. Описание этих ветвей — статический
вывод по одному известному `ai.dll`, а не утверждение о совпадении во время
исполнения.

Для начального сценарного события закрыт более узкий путь. В проверенной GOG
сборке `ai.dll` SHA-256
`c19cd6cd31cd2896f8c4ba0f0e700dd3408aec8182daaf4863d98a7d233a2abc` функции
`FUN_10011f40` (RVA `0x11f40`) и `FUN_10011f10` (RVA `0x11f10`) выбирают первое
имя-событие C-string `Init` и запускают его инструкции по порядку. Для координат
из TMA найден и проверен ещё один участок: `iron3d.dll` SHA-256
`d1dc4ea8535e2069b0a05c9e8be2724dd438ba44cca7a83007d8b2e50f9e35fa`,
`FUN_100392e0` (RVA `0x392e0`) выполняет x87 `FISTP qword` и передаёт младшие
DWORD координат пятым и шестым аргументами `CreateSuperAI`. Затем
`ai.dll!FUN_10005c70` (RVA `0x5c70`) преобразует DWORD в `float`, а `Handler(19)`
возвращает его к DWORD усечением.

Две x86-проверки вызвали эти оригинальные DLL-функции на изолированной
синтетической памяти. Caller oracle подтвердил AutoDemo-координаты
`(500.2857, 751.8217) -> (500, 752)`, ties-to-even в nearest-режиме и переход
`-0.6 -> 0xffffffff`. AI oracle проверил оба набора AutoDemo anchor-ов,
дублирующий `Init`, имя `Init\\0suffix\\0`, отсутствие события, а также
нативную связку `FUN_10005c70` → `Handler(19)`: DWORD `(0xffffffff, 10001)` стал
полями `(2^32, 10001.0)` и varset-записями `(0, 10001, 2)`. Эти проверки
подтверждают только изолированные участки ABI.

В одном read-only snapshot запущенной GOG AutoDemo были связаны записи кланов
Iron3D и указатели `GetSuperAI` из `ai.dll` (таблица RVA `0x55398`). Клан 0 имел
исходные float-координаты `(500.2857, 751.8217)` и поля AI
`(+0x80, +0x84, +0x7c) = (500.0, 752.0, 0)`; клан 1 имел `(728.0, 449.0)` и
`(728.0, 449.0, 1)`. Первый набор связывает те же значения в одном процессе и
выбирает ближайшее округление среди направлений округления x87. Синтетические
tie-векторы проверяют, как сам `FISTP` реагирует на каждый RC, но не измеряют
control word процесса.

Отдельно проверены загрузка лицензированного AutoDemo в Rust и preview
лицензированной кампании. AutoDemo даёт raw TMA anchor-ы
`[(500.2857, 751.8217), (728.0, 449.0)]` и записи `[(500, 752, 0),
(728, 449, 1)]`; preview кампании сохраняет package и статус `Unsupported`,
не применяя частичные записи. Это подтверждает загрузчик и узкий Init resolver
Rust, но не доказывает, что штатный host вызывает `Init` при старте оригинальной
миссии.

Нативный конструктор показывает предупреждение при координате выше `10000`, но
продолжает работу. Rust моделирует числовую запись, а не это UI-предупреждение;
на первом `FISTP qword` он явно завершает загрузку ошибкой для NaN, infinity и
значений вне signed-qword диапазона. В первом событии `Init` Rust поддерживает
только action-форму `Handler(19)` (`control word` и `result reference` равны
`0xffffffff`). Неизвестная инструкция даёт `Unsupported` в preview, не применяя
накопленные частичные записи; при полной загрузке это ошибка. Это ограничение
Rust исполнителя, не правило оригинального dispatcher-а. Ошибочные ссылки и
типы остаются ошибками в обоих режимах.

Открыты вызов `Init` при штатном старте Iron3D и проверка другой миссии с
непустым `Init`. Не восстановлены host-ingress `Init` в игровом цикле и общий
dispatcher остальных событий.

Для `Handler(30)` отдельно подтверждены обратный вызов `iron3d.dll` RVA
`0x611d0` (SHA-256 `d1dc4ea8535e2069b0a05c9e8be2724dd438ba44cca7a83007d8b2e50f9e35fa`),
начальные поля IGame и проверки режима/состояния. Обработчик выбора миссии
выставляет режим 1 (RVA `0x29142`), а обработчик таймера — режим 3 (RVA `0x70f3`).
Небольшая тестовая x86-программа сравнивает этот обратный вызов с исходной DLL
на синтетических структурах. Пока не восстановлен сквозной путь от выбора и
инициализации миссии к первому событию Handler(30): Rust загружает миссию, но
ещё не выполняет штатный `stdInitGame`, переход состояния `5 -> 4`, запуск и
игровой цикл с полной диспетчеризацией событий. Поэтому
совпадение изолированного обратного вызова не доказывает совместимость живого
сценария.

Закрывающий эксперимент: найти dispatcher loop в `ai.dll`, сопоставить jump
table с instruction sizes, построить disassembler и сравнить выполнение
коротких scripts с оригиналом. Для evaluator-а условий отдельно проследить
создание записей `this + 0x34`, выполнить те же входные данные в оригинале и
сверить байт завершения, порядок поиска, ранние выходы и граничный случай
`payload == count`.

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
