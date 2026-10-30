# Shell, HUD, шрифты и локализация

## Доказанная граница

`iron_3d.exe` создаёт процесс и окно; `iron3d.dll` экспортирует `createShell`,
`deleteShell` и `getIShell`; `services.dll` предоставляет `getGUIServer` и
`getDisplay`. Поэтому меню, briefing, HUD и системные диалоги — отдельная
оболочка над World3D, а не игровые объекты, созданные ради отрисовки.

```text
Win32 messages
  -> shell/GUI input и manual events
  -> simulation World3D
  -> world render
  -> shell/HUD overlay и presentation state
```

Точное положение legacy DirectDraw flip относительно последнего UI draw пока
требует API capture. Разделение world и UI pass доказано, но не следует
выдавать его за восстановленный layout классов виджетов.

## Ресурсы и идентичность

В demo найдены `ui/shell_ctrls.cfg`, `ui/menu_resources.cfg`, `ui/cursor.cfg`,
`ui/game_resources.cfg`, `ui/hq.cfg`, `DATA/TextRes.cfg`, `gamefont.rlb`,
`sprites.lib` и `Palettes.lib`. Конфигурации задают controls/resources/cursor,
overlays и HQ; `TextRes.cfg` связывает символические ключи с текстом.

В demo `gamefont.rlb` содержит две LZSS-записи (`0x040`); `sprites.lib`
содержит 24 записи raw Deflate (`0x100`) и известный `deflate_eof_plus_one`
quirk для `INTERF8.TEX`. В полной GOG-сборке меню отдельно связывает
`ui/menu_resources.cfg` с `ui/font.lib`, архивом NRes. Поэтому `gamefont.rlb`
не задаёт единственный путь шрифта. Оба контейнера уже читаются общими
[RsLi](../reference/rsli.md)- и [NRes](../reference/nres.md)-readers проекта;
будущая UI-подсистема должна пользоваться ими, а не добавлять свою распаковку.

В `menu_resources.cfg` ресурс `fonts` указывает `ui\\font.lib` с `libtype=multi`
и `type=2`; это поля CFG, а не FourCC записей NRes. У девяти записей
`font.lib` `type_id` равен `0x544E4F46` (`FONT`). Значение подстановки в
`font-substitute` — строковое имя: этот CFG alias (`MENU_FONT` или `GAME_FONT`)
связывает роль шрифта с его именем, но сам не является именем записи архива.
Числовые ссылки находятся отдельно, в объекте `fonts`. В проверенной паре CFG и
архива эти ссылки совпадают с исходными порядковыми номерами записей NRes
(`EntryId`), а не с `sort_index`:

| Размер CFG | Строковый alias `font-substitute` | Ссылка `fonts` | `EntryId` и запись NRes |
| --- | --- | --- | --- |
| `640x480` | `MENU_FONT` | `8` | `8`, `mf_640.tft` |
| `640x480` | `GAME_FONT` | `6` | `6`, `gf_640.tft` |
| `800x600` | `MENU_FONT` | `5` | `5`, `mf_800.tft` |
| `800x600` | `GAME_FONT` | `3` | `3`, `gf_800.tft` |
| `1024x768` | `MENU_FONT` | `2` | `2`, `mf_1024.tft` |
| `1024x768` | `GAME_FONT` | `0` | `0`, `gf_1024.tft` |

Пример из этой CFG: `font-substitute` связывает `MENU_FONT` со строкой
`MENU_FONT_1024x768`, а объект `fonts` связывает уже это имя с числом `2`.
Такое сопоставление относится только к этой конфигурации и архиву, а не является
универсальным правилом native selector. Строка `1024x768` не доказывает выбор
этого варианта при фактическом экране `1280x1024`.

Проверены оригинальные `menu_resources.cfg` (SHA-256
`508C3783EAF112023E46BF4617424FBB57C393BCEF328CE1B925549B0069EF97`) и
`font.lib` (440392 bytes, SHA-256
`E4AF5D1F0BF0464574DE654DE378766090681F610705CE53539DF9041E045D1C`).
Шесть сопоставленных payload начинаются с `Tfnt`; это подтверждает сигнатуру
данных, но не значение последующих полей.

Связка подтверждена и в оригинальном коде. В `iron3d.dll` SHA-256
`D1DC4EA8535E2069B0A05C9E8BE2724DD438BA44CCA7A83007D8B2E50F9E35FA` shell
читает `shell_ctrls.cfg` в RVA `0x7410`, а инициализация в `0x78c0` читает и
регистрирует `menu_resources.cfg` в слоте `0` ResourceManager. `services.dll` SHA-256
`D3619DEEB4D598B16099E0263E15F9C3213F0FAA7208DC093328CFBA93FDC0EA`, RVA
`0x92a0`, разбирает `font-substitute`, размерные значения и запись ресурса
`type=2`. Затем `iron3d.dll` запрашивает `MENU_FONT` в RVA `0x1eb20`, ищет ресурс
в `0x1ed06` и разыменовывает результат в `0x1ed3e`; прямая x86-трасса на
последнем участке подтвердила ненулевой объект. Это подтверждает получение
реального font-объекта, но не конкретной NRes-записи при `1280x1024`, её
отрисовку или паритет шрифтов.

Ключ ресурса, legacy path, локализованный текст и исходные bytes — четыре
разные идентичности. Archive keys остаются ASCII-casefold; `TextRes.cfg`,
briefing/messages и script strings могут требовать ANSI/CP1251 decoding.
Unicode-представление хранит raw bytes для roundtrip и diagnostics.

`gamefont.rlb` и `sprites.lib` побайтно совпадают в Частях 1 и 2. Во второй
части добавлен `ui_factory.lib` (NRes с шестью Texm) и расширен
`ui/minimap.lib`; при этом пересобранные `iron3d.dll`/`services.dll` требуют
отдельной трассировки lifecycle и HUD state.

## Контракт новой реализации

UI scene хранит корневые widgets, focus, viewport/clip rectangles и modal
depth. Demo `640x480` — полезный baseline, но не доказанное универсальное
design resolution. Отдельно хранятся legacy-layout coordinates, реальный
viewport, scale/letterbox policy, mouse-to-layout transform, clipping и
z-order. Cursor, sprite и hit-test обязаны применять одно преобразование.

Font contract включает glyph image, advance, bearing/offset, line height и
fallback glyph. Содержимое `gamefont.rlb` и `Tfnt` payload записей `.tft`
сохраняется без потери bytes через общие readers (`RsLi` и `NRes`), но семантика
метрик и глифов ещё не восстановлена по consumer trace.

Нормализованное событие проходит modal widget, затем shell command map; лишь
разрешённая gameplay-команда превращается в World3D manual event. Held state и
axes попадают в calculation snapshot. Это не позволяет UI-click одновременно
исполнить команду мира. HUD только читает presentation view (selected object,
resources, mission text, timers, research/build state, camera mode); authority
остаётся у simulation.

## Проверки и открытые вопросы

- UI cfg читаются до EOF с сохранением неизвестных fields; symbolic key
  разрешается в RsLi entry независимо от регистра.
- Visual rect и hit-test совпадают при разных viewport; modal scene блокирует
  gameplay input; missing optional sprite/font имеет named fallback.
- UI-only и world-only command captures собираются раздельно.

Не закрыты grammar всех `ui/*.cfg`, hierarchy original widgets, glyph metrics,
HUD state machine и pixel-perfect layout. Нужны GUI-factory hooks, event traces
и captures меню, briefing, HQ и игрового HUD.
