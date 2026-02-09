# nres

Rust-библиотека для работы с архивами формата **NRes**.

## Что умеет

- Открытие архива из файла (`open_path`) и из памяти (`open_bytes`).
- Поддержка `raw_mode` (весь файл как единый ресурс).
- Чтение метаданных и итерация по записям.
- Поиск по имени без учёта регистра (`find`).
- Чтение данных ресурса (`read`, `read_into`, `raw_slice`).
- Редактирование архива через `Editor`:
- `add`, `replace_data`, `remove`.
- `commit` с пересчётом `sort_index`, выравниванием по 8 байт и атомарной записью файла.

## Модель ошибок

Библиотека возвращает типизированные ошибки (`InvalidMagic`, `UnsupportedVersion`, `TotalSizeMismatch`, `DirectoryOutOfBounds`, `EntryDataOutOfBounds`, и др.) без паник в production-коде.

## Покрытие тестами

### Реальные файлы

- Рекурсивный прогон по `testdata/nres/**`.
- Сейчас в наборе: **120 архивов**.
- Для каждого архива проверяется:
- чтение всех записей;
- `read`/`read_into`/`raw_slice`;
- `find`;
- `unpack -> repack (Editor::commit)` с проверкой **byte-to-byte**.

### Синтетические тесты

- Проверка основных сценариев редактирования (`add/replace/remove/commit`).
- Проверка валидации и ошибок:
- `InvalidMagic`, `UnsupportedVersion`, `TotalSizeMismatch`, `InvalidEntryCount`, `DirectoryOutOfBounds`, `NameTooLong`, `EntryDataOutOfBounds`, `EntryIdOutOfRange`, `NameContainsNul`.

## Быстрый запуск тестов

```bash
cargo test -p nres -- --nocapture
```
