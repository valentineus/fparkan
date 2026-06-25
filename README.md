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

Перед запуском убедитесь, что на машине доступен Vulkan loader и рабочий ICD:

- macOS: используйте ту же схему, что и GitHub CI (`macos-15` arm64):

  ```bash
  brew install molten-vk vulkan-loader vulkan-tools vulkan-validationlayers
  export VK_ICD_FILENAMES="$(brew --prefix)/opt/molten-vk/etc/vulkan/icd.d/MoltenVK_icd.json"
  export VK_LAYER_PATH="$(brew --prefix)/opt/vulkan-validationlayers/share/vulkan/explicit_layer.d"
  export DYLD_FALLBACK_LIBRARY_PATH="$(brew --prefix)/opt/vulkan-loader/lib:$(brew --prefix)/opt/molten-vk/lib"
  vulkaninfo --summary
  ```

  Workflow fail-closed проверяет exact formula versions и ожидает наличие `VK_LAYER_KHRONOS_validation`.
- Linux: установлен `libvulkan` и драйвер/ICD (`mesa-vulkan-drivers`, Lavapipe или vendor GPU stack); smoke нужно запускать из активной графической сессии X11/Wayland.
- Windows: установлен Vulkan runtime от GPU vendor или LunarG Vulkan SDK; validation layer должен быть доступен из активного runtime.

Для полного локального closure gate используйте:

```bash
cargo xtask ci
```

В текущем macOS-only цикле GitHub workflow собирает только macOS report и проверяет его через `native-smoke audit`. Windows и Linux smoke stages сознательно не входят в этот closure:

```bash
cargo xtask native-smoke audit --dir target/fparkan/native-smoke-artifacts
```

## Contributing & Support

Проект активно поддерживается и открыт для contribution. Issues и pull requests можно создавать в обоих репозиториях:

- **Primary development**: [valentineus/fparkan](https://code.popov.link/valentineus/fparkan)
- **GitHub mirror**: [valentineus/fparkan](https://github.com/valentineus/fparkan)

Основная разработка ведётся в self-hosted репозитории.

## Лицензия

Проект распространяется под лицензией **[GNU GPL v2](LICENSE.txt)**.
