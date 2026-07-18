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
- [apps/fparkan-cli](apps/fparkan-cli) — CLI для архивов, графов и acceptance-отчетов.
- [apps/fparkan-viewer](apps/fparkan-viewer) — inspection-only CLI для archive/model/texture/map без live Vulkan draw path.
- [apps/fparkan-headless](apps/fparkan-headless) — headless runtime composition root.
- [apps/fparkan-game](apps/fparkan-game) — mission composition root: по умолчанию выдаёт planning report; opt-in `--backend static-vulkan` открывает native Vulkan окно и рисует terrain и все подготовленные MSH-компоненты выбранных mission roots. По умолчанию выбираются все roots; `--preview-roots N` оставляет bounded diagnostic scope.

## Текущий статус рендера

- `fparkan-vulkan-smoke` доказывает живой Stage 0 Vulkan triangle path с native window, swapchain и validation telemetry.
- `VulkanPlanningBackend` и default-режим `fparkan-game` подтверждают только deterministic command planning/capture, а не draw пикселей. `fparkan-game --backend static-vulkan` — узкий mission-to-native-Vulkan bridge: он объединяет terrain и подготовленные MSH-компоненты всех выбранных roots, применяет static TMA transforms и загружает первую MAT0 diffuse texture на каждый используемый selector с preview-local remap. Legacy D3D7 camera capture поддерживается; полный corpus, material phases, dynamic ownership/visibility и pixel parity ещё не подтверждены.
- `fparkan-viewer` пока является инспектором ассетов. `fparkan-vulkan-smoke` имеет live Stage 3 bridge для original `MSH`/`Texm`/`WEAR`/`MAT0` и geometry-only `Land.msh`; полноценный viewer, исходные terrain-material states, camera и pixel parity ещё не закрыты.
- Truth table и evidence-артефакты вынесены в [`docs/rendering/renderer_truth_table.md`](docs/rendering/renderer_truth_table.md) и [`docs/evidence/`](docs/evidence).

## Тестирование

Базовое тестирование проходит на синтетических тестах из репозитория:

```bash
cargo xtask ci
```

Для дополнительного тестирования на реальных игровых ресурсах:

- используйте оригинальную копию игры (диск или [GOG-версия](https://www.gog.com/en/game/parkan_iron_strategy));
- разместите игровые каталоги в [`testdata/`](testdata);
- игровые ресурсы в репозиторий не включаются, так как защищены авторским правом.

Локальный licensed gate использует некоммитимый manifest:

```bash
cat > /private/tmp/fparkan-corpora.toml <<'EOF'
schema = 1

[[corpus]]
id = "part1-local"
kind = "part1"
root = "/absolute/path/to/IS"
expected_profile = "parkan-is-part1"

[[corpus]]
id = "part2-local"
kind = "part2"
root = "/absolute/path/to/IS2"
expected_profile = "parkan-is-part2"
EOF

FPARKAN_CORPORA_MANIFEST=/private/tmp/fparkan-corpora.toml \
  cargo xtask acceptance report --suite licensed --stage 5
```

## Stage 0 Vulkan smoke

Локальный Stage 0 smoke запускает реальный `winit` lifecycle и Vulkan triangle path с включёнными validation layers. Успешный прогон обязан:

- отрисовать 300 кадров;
- выполнить как минимум один реальный resize;
- пересоздать swapchain после resize;
- завершиться без validation warnings/errors.

Команда запуска:

```bash
cargo run -p fparkan-vulkan-smoke --locked -- \
  --out target/fparkan/native-smoke/local.json \
  --timeout-seconds 120
```

Поддерживается только Windows. Перед запуском убедитесь, что установлен Vulkan runtime
от GPU vendor или LunarG Vulkan SDK и активный runtime предоставляет
`VK_LAYER_KHRONOS_validation`. Linux, macOS/MoltenVK и их smoke-пути не входят в
проектный scope.

Для полного локального closure gate используйте:

```bash
cargo xtask ci
```

Windows native smoke — единственный platform acceptance path. Вопросы hosted CI
и других операционных систем намеренно находятся вне scope разработки.

## Contributing & Support

Проект активно поддерживается и открыт для contribution. Issues и pull requests можно создавать в обоих репозиториях:

- **Primary development**: [valentineus/fparkan](https://code.popov.link/valentineus/fparkan)
- **GitHub mirror**: [valentineus/fparkan](https://github.com/valentineus/fparkan)

Основная разработка ведётся в self-hosted репозитории.

## Лицензия

Проект распространяется под лицензией **[GNU GPL v2](LICENSE.txt)**.
