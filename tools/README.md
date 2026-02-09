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
