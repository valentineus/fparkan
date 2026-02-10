# Инструменты в каталоге `tools`

## `archive_roundtrip_validator.py`

Скрипт предназначен для **валидации документации по форматам NRes и RsLi на реальных данных игры**.

Что делает утилита:

- находит архивы по сигнатуре заголовка (а не по расширению файла);
- распаковывает архивы в структуру `manifest.json + entries/*`;
- собирает архивы обратно из `manifest.json`;
- выполняет проверку `unpack -> repack -> byte-compare`;
- формирует отчёт о расхождениях со спецификацией.

Скрипт не изменяет оригинальные файлы игры. Рабочие файлы создаются только в указанном `--workdir` (или во временной папке).

## Поддерживаемые сигнатуры

- `NRes` (`4E 52 65 73`)
- `RsLi` в файловом формате библиотеки: `NL 00 01`

## Основные команды

Сканирование архива по сигнатурам:

```bash
python3 tools/archive_roundtrip_validator.py scan --input tmp/gamedata
```

Распаковка/упаковка одного NRes:

```bash
python3 tools/archive_roundtrip_validator.py nres-unpack \
  --archive tmp/gamedata/sounds.lib \
  --output tmp/work/nres_sounds

python3 tools/archive_roundtrip_validator.py nres-pack \
  --manifest tmp/work/nres_sounds/manifest.json \
  --output tmp/work/sounds.repacked.lib
```

Распаковка/упаковка одного RsLi:

```bash
python3 tools/archive_roundtrip_validator.py rsli-unpack \
  --archive tmp/gamedata/sprites.lib \
  --output tmp/work/rsli_sprites

python3 tools/archive_roundtrip_validator.py rsli-pack \
  --manifest tmp/work/rsli_sprites/manifest.json \
  --output tmp/work/sprites.repacked.lib
```

Полная валидация документации на всём наборе данных:

```bash
python3 tools/archive_roundtrip_validator.py validate \
  --input tmp/gamedata \
  --workdir tmp/validation_work \
  --report tmp/validation_report.json \
  --fail-on-diff
```

## Формат распаковки

Для каждого архива создаются:

- `manifest.json` — все поля заголовка, записи, индексы, смещения, контрольные суммы;
- `entries/*.bin` — payload-файлы.

Имена файлов в `entries` включают индекс записи, поэтому коллизии одинаковых имён внутри архива обрабатываются корректно.

## `init_testdata.py`

Скрипт инициализирует тестовые данные по сигнатурам архивов из спецификации:

- `NRes` (`4E 52 65 73`);
- `RsLi` (`NL 00 01`).

Что делает утилита:

- рекурсивно сканирует все файлы в `--input`;
- копирует найденные `NRes` в `--output/nres/`;
- копирует найденные `RsLi` в `--output/rsli/`;
- сохраняет относительный путь исходного файла внутри целевого каталога;
- создаёт целевые каталоги автоматически, если их нет.

Базовый запуск:

```bash
python3 tools/init_testdata.py --input tmp/gamedata --output testdata
```

Если целевой файл уже существует, скрипт спрашивает подтверждение перезаписи (`yes/no/all/quit`).

Для перезаписи без вопросов используйте `--force`:

```bash
python3 tools/init_testdata.py --input tmp/gamedata --output testdata --force
```

Проверки надёжности:

- `--input` должен существовать и быть каталогом;
- если `--output` указывает на существующий файл, скрипт завершится с ошибкой;
- если `--output` расположен внутри `--input`, каталог вывода исключается из сканирования;
- если `stdin` неинтерактивный и требуется перезапись, нужно явно указать `--force`.

## `msh_doc_validator.py`

Скрипт валидирует ключевые инварианты из документации `/Users/valentineus/Developer/personal/fparkan/docs/specs/msh.md` на реальных данных.

Проверяемые группы:

- модели `*.msh` (вложенные `NRes` в архивах `NRes`);
- текстуры `Texm` (`type_id = 0x6D786554`);
- эффекты `FXID` (`type_id = 0x44495846`).

Что проверяет для моделей:

- обязательные ресурсы (`Res1/2/3/6/13`) и известные опциональные (`Res4/5/7/8/10/15/16/18/19`);
- `size/attr1/attr3` и шаги структур по таблицам;
- диапазоны индексов, батчей и ссылок между таблицами;
- разбор `Res10` как `len + bytes + NUL` для каждого узла;
- матрицу слотов в `Res1` (LOD/group) и границы по `Res2/Res7/Res13/Res19`.

Быстрый запуск:

```bash
python3 tools/msh_doc_validator.py scan --input testdata/nres
python3 tools/msh_doc_validator.py validate --input testdata/nres --print-limit 20
```

С отчётом в JSON:

```bash
python3 tools/msh_doc_validator.py validate \
  --input testdata/nres \
  --report tmp/msh_validation_report.json \
  --fail-on-warnings
```

## `msh_preview_renderer.py`

Примитивный программный рендерер моделей `*.msh` без внешних зависимостей.

- вход: архив `NRes` (например `animals.rlb`) или прямой payload модели;
- выход: изображение `PPM` (`P6`);
- использует `Res3` (позиции), `Res6` (индексы), `Res13` (батчи), `Res1/Res2` (выбор слотов по `lod/group`).

Показать доступные модели в архиве:

```bash
python3 tools/msh_preview_renderer.py list-models --archive testdata/nres/animals.rlb
```

Сгенерировать тестовый рендер:

```bash
python3 tools/msh_preview_renderer.py render \
  --archive testdata/nres/animals.rlb \
  --model A_L_01.msh \
  --output tmp/renders/A_L_01.ppm \
  --width 800 \
  --height 600 \
  --lod 0 \
  --group 0 \
  --wireframe
```

Ограничения:

- инструмент предназначен для smoke-теста геометрии, а не для пиксельно-точного рендера движка;
- текстуры/материалы/эффектные проходы не эмулируются.

## `msh_export_obj.py`

Экспортирует геометрию `*.msh` в `Wavefront OBJ`, чтобы открыть модель в Blender/MeshLab.

- вход: `NRes` архив (например `animals.rlb`) или прямой payload модели;
- выбор геометрии: через `Res1` slot matrix (`lod/group`) как в рендерере;
- опция `--all-batches` экспортирует все батчи, игнорируя slot matrix.

Показать модели в архиве:

```bash
python3 tools/msh_export_obj.py list-models --archive testdata/nres/animals.rlb
```

Экспорт в OBJ:

```bash
python3 tools/msh_export_obj.py export \
  --archive testdata/nres/animals.rlb \
  --model A_L_01.msh \
  --output tmp/renders/A_L_01.obj \
  --lod 0 \
  --group 0
```

Файл `OBJ` можно открыть напрямую в Blender (`File -> Import -> Wavefront (.obj)`).
