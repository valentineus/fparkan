# rsli

Rust-библиотека для чтения архивов формата **RsLi**.

## Что умеет

- Открытие библиотеки из файла (`open_path`, `open_path_with`).
- Дешифрование таблицы записей (XOR stream cipher).
- Поддержка AO-трейлера и media overlay (`allow_ao_trailer`).
- Поддержка quirk для Deflate `EOF+1` (`allow_deflate_eof_plus_one`).
- Поиск по имени (`find`, c приведением запроса к uppercase).
- Загрузка данных:
- `load`, `load_into`, `load_packed`, `unpack`, `load_fast`.

## Поддерживаемые методы упаковки

- `0x000` None
- `0x020` XorOnly
- `0x040` Lzss
- `0x060` XorLzss
- `0x080` LzssHuffman
- `0x0A0` XorLzssHuffman
- `0x100` Deflate

## Модель ошибок

Типизированные ошибки без паник в production-коде (`InvalidMagic`, `UnsupportedVersion`, `EntryTableOutOfBounds`, `PackedSizePastEof`, `DeflateEofPlusOneQuirkRejected`, `UnsupportedMethod`, и др.).

## Покрытие тестами

### Реальные файлы

- Рекурсивный прогон по `testdata/rsli/**`.
- Сейчас в наборе: **2 архива**.
- На реальных данных подтверждены и проходят byte-to-byte проверки методы:
- `0x040` (LZSS)
- `0x100` (Deflate)
- Для каждого архива проверяется:
- `load`/`load_into`/`load_packed`/`unpack`/`load_fast`;
- `find`;
- пересборка и сравнение **byte-to-byte**.

### Синтетические тесты

Из-за отсутствия реальных файлов для части методов добавлены синтетические архивы и тесты:

- Методы:
- `0x000`, `0x020`, `0x060`, `0x080`, `0x0A0`.
- Спецкейсы формата:
  - AO trailer + overlay;
  - Deflate `EOF+1` (оба режима: accepted/rejected);
- некорректные заголовки/таблицы/смещения/методы.

## Быстрый запуск тестов

```bash
cargo test -p rsli -- --nocapture
```
