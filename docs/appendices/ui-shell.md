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

## Фиксированный reader Tfnt

В подтверждённом пути `World3D.dll` (SHA-256
`17e4a3089b2583a8cf2356c9db0390b1aba138356a09130d79b4e7e4791da61e`)
передаёт ordinal NRes-reader-у в RVA `0x1361f`, а затем передаёт полученный
указатель renderer-у в RVA `0x13624`; бит `0x400000` установлен у вызова в RVA
`0x1360d`. В `Ngi32.dll` (SHA-256
`bab9840d94f4e4e74ffc26677724fa896cf4823845504d09a9e025f80016edf5`) vtable
NRes у RVA `0x318bc` ведёт к `FUN_10011f80` (RVA `0x11f80`), которая возвращает
`mapped_base + data_offset`. Renderer vtable у RVA `0x315e0` направляет slot
`+0x58` в `FUN_10007dd0` (RVA `0x7dd0`), затем ресурс проходит через
`FUN_10010c10` (RVA `0x10c10`) и хранит pointer в `FUN_1000f3d0` (RVA `0x0f3d0`).
Loader font-объекта `FUN_1000fb30` (RVA `0x0fb30`) при флаге `0x400000`
использует этот адрес как начало Tfnt payload; отдельная RsLi-обёртка в этом
пути не читается.

| Смещение | Что читает `fparkan-font` |
| --- | --- |
| `0x00..0x03` | Сигнатура `Tfnt`; reader проверяет её на границе файлового API. В наблюдавшемся native loader проверки сигнатуры не было. |
| `0x04..0x07` | `i32`, нижняя граница ширины в native fixed-width path. |
| `0x08..0x0b` | `i32`, вертикальный размер для native clipping/drawing. |
| `0x0c..0x0f` | Сырые биты `u32`, скопированные в font object и участвующие в float-сложении при drawing; имя и смысл поля неизвестны. |
| `0x10..0x13` | `i32`, прибавляемый один раз на входной байт при native text measurement. |
| `0x14..0x1013` | 256 записей по 16 байт: три сырых DWORD и signed `i32` advance по смещению `+0x0c`. |
| `0x1014..конец` | Обычный Texm-документ, передаваемый общему `fparkan-texm::decode_texm`. |

Первые три слова glyph record имеют разные native consumers, но их общая
семантика не восстановлена: одно слово загружается как float, другое попадает
в сформированные vertex data, а третье не прояснилось в исследованных ветвях.
Поэтому reader сохраняет их как `word0..word2`, а поле заголовка по смещению
`0x0c` — как неинтерпретированные float-биты. Он не выбирает размер текстуры по разрешению
дисплея и не задаёт диапазон размеров: в проверенных ресурсах встречаются
`128×128` и `256×256`, а форматы, mip-цепочку и палитру проверяет общий Texm
reader.

```rust
let font = decode_tfnt(Arc::<[u8]>::from(raw_font_payload))?;
let glyph = font.glyphs[usize::from(input_byte)];
let texture = font.texm;
```

Игнорируемый corpus-тест разбирает через общий NRes reader девять записей
`ui/font.lib` и `ARIALTEX.TFT` из `sys.lib`; он сверяет известные размеры,
Indexed8, одну mip-ступень и выборочные поля `gf_640.tft`. Это проверка байтов
лицензированной GOG-сборки и parser contract, а не запуск shell или проверка
отрисовки.

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

Font contract включает glyph image, bearing/offset, line height и fallback
glyph. `fparkan-font` читает фиксированную таблицу Tfnt и отдаёт signed advance
и embedded Texm; он не выполняет text measurement/layout или glyph drawing.
`gamefont.rlb` по-прежнему читается общим RsLi reader, но его native consumer не
установлен.

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
