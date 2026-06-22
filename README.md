# FParkan

Open source проект с реализацией компонентов игрового движка игры **«Паркан: Железная Стратегия»**.

## Описание

Проект находится в активной разработке и включает:

- библиотеки для работы с форматами игровых архивов;
- спецификации форматов и сопутствующую документацию.

## Установка

Проект находится в начальной стадии, подробная инструкция по установке пока отсутствует.

## Документация

- локально: каталог [`docs/`](docs)
- сайт: <https://fparkan.popov.link>

## Библиотеки

- [crates/fparkan-nres](crates/fparkan-nres) — strict/lossless модель архивов NRes.
- [crates/fparkan-rsli](crates/fparkan-rsli) — чтение, lookup и lossless roundtrip архивов RsLi.
- [crates/fparkan-msh](crates/fparkan-msh) — validated static MSH geometry.
- [crates/fparkan-runtime](crates/fparkan-runtime) — transactional mission loading и headless runtime foundation.
- [apps/fparkan-cli](apps/fparkan-cli), [apps/fparkan-viewer](apps/fparkan-viewer), [apps/fparkan-headless](apps/fparkan-headless), [apps/fparkan-game](apps/fparkan-game) — composition roots.

## Тестирование

Базовое тестирование проходит на синтетических тестах из репозитория:

```bash
cargo xtask ci
```

Для дополнительного тестирования на реальных игровых ресурсах:

- используйте оригинальную копию игры (диск или [GOG-версия](https://www.gog.com/en/game/parkan_iron_strategy));
- разместите игровые каталоги в [`testdata/`](testdata);
- игровые ресурсы в репозиторий не включаются, так как защищены авторским правом.

Локальный licensed gate:

```bash
cargo xtask acceptance report --suite licensed --stage 5 --root testdata
```

## Contributing & Support

Проект активно поддерживается и открыт для contribution. Issues и pull requests можно создавать в обоих репозиториях:

- **Primary development**: [valentineus/fparkan](https://code.popov.link/valentineus/fparkan)
- **GitHub mirror**: [valentineus/fparkan](https://github.com/valentineus/fparkan)

Основная разработка ведётся в self-hosted репозитории.

## Лицензия

Проект распространяется под лицензией **[GNU GPL v2](LICENSE.txt)**.
