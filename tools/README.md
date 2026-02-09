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
