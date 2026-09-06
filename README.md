# FParkan

Открытый движок *Паркан: Железная стратегия* на Rust и Vulkan. Он читает
ресурсы установленной оригинальной игры. Сейчас приложение загружает миссию
и показывает её ландшафт и модели в статическом окне Vulkan.

## Запуск

Нужны Rust и драйвер видеокарты с поддержкой Vulkan. Игровые файлы в
репозиторий не входят.

```powershell
cargo run -p fparkan-game -- `
  --root 'C:\GOG Games\Parkan - Iron Strategy' `
  --mission 'MISSIONS\Autodemo.00\data.tma' --frames 240
```

`--frames` задаёт число кадров. `--preview-roots N` ограничивает число объектов,
`--legacy-camera-capture` читает сохранённую матрицу камеры,
`--static-animation-frame` и `--static-material-phase` выбирают позу и фазу
материала. `--readback-out target/frame.raw` сохраняет последний кадр GPU.
Без сохранённой камеры используется вид карты сверху.

## Разработка

```text
cargo fmt --all -- --check
cargo test --workspace
cargo check --workspace
```

Алгоритмы и форматы проверяются обычными Rust-тестами рядом с кодом.
Тесты с оригинальными ресурсами помечены `#[ignore]`; пути к двум частям
установки задаются через `FPARKAN_CORPUS_PART1_ROOT` и
`FPARKAN_CORPUS_PART2_ROOT`. Зависимости разрешает Cargo, локальный
`Cargo.lock` не хранится в Git.

Готовые SPIR-V шейдеры находятся рядом с исходниками GLSL.
Vulkan SDK нужен только для их пересборки и проверки с `--validation`.
При доступных в PATH инструментах SDK:

```powershell
glslangValidator -V adapters/fparkan-render-vulkan/shaders/triangle.vert -o adapters/fparkan-render-vulkan/shaders/triangle.vert.spv
glslangValidator -V adapters/fparkan-render-vulkan/shaders/triangle.frag -o adapters/fparkan-render-vulkan/shaders/triangle.frag.spv
spirv-val adapters/fparkan-render-vulkan/shaders/triangle.vert.spv
spirv-val adapters/fparkan-render-vulkan/shaders/triangle.frag.spv
```

## Книга и исходники

[Книга об устройстве движка](docs/index.md) объясняет форматы, математику,
загрузку мира и связь подсистем. Её можно читать прямо в репозитории;
`mkdocs.yml` задаёт представление на [сайте проекта](https://fparkan.popov.link).

`crates/` содержит форматы и игровые алгоритмы, `adapters/` — окно и Vulkan,
`apps/fparkan-game` — исполняемое приложение.

## Лицензия

[GNU GPL v2](LICENSE.txt).
