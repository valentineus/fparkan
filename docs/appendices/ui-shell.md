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

`gamefont.rlb` содержит две LZSS-записи (`0x040`), `sprites.lib` — 24 записи
raw Deflate (`0x100`). `sprites.lib::INTERF8.TEX` использует известный
`deflate_eof_plus_one` quirk. UI обязан пользоваться общим RsLi reader, а не
отдельной похожей распаковкой.

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
fallback glyph. Binary glyph metrics `gamefont.rlb` пока не восстановлены:
payload читается lossless через RsLi, а семантика устанавливается по consumer
trace.

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
