# render-demo

Тестовый рендерер Parkan-моделей на Rust (`SDL2 + OpenGL`: GLES2 с fallback на Core 3.3).

## Назначение

- Проверить, что `nres + msh-core + render-core` дают рабочий draw-path на реальных ассетах.
- Проверить текстурный path `WEAR -> MAT0 -> Texm` на реальных ассетах.
- Служить минимальным reference-приложением.

## Запуск

```bash
cargo run -p render-demo --features demo -- \
  --archive "testdata/Parkan - Iron Strategy/animals.rlb" \
  --model "A_L_01.msh" \
  --lod 0 \
  --group 0
```

### macOS prerequisites

Для macOS `render-demo` ожидает системный SDL2 через `pkg-config`:

```bash
brew install sdl2 pkg-config
```

После этого запускайте той же командой `cargo run ... --features demo`.

Параметры:

- `--archive` (обязательный): NRes-архив с `.msh` entry.
- `--model` (опционально): имя модели; если не задано, берётся первая `.msh`.
- `--lod` (опционально, default `0`).
- `--group` (опционально, default `0`).
- `--width`, `--height` (опционально, default `1280x720`).
- `--angle` (опционально): фиксированный угол поворота вокруг Y (в радианах).
- `--spin-rate` (опционально, default `0.35`): скорость вращения в интерактивном режиме.
- `--texture <name>`: явное имя `Texm` (override авто-резолва).
- `--texture-archive <path>`: путь к архиву текстур (по умолчанию `textures.lib` рядом с `--archive`).
- `--material-archive <path>`: путь к `material.lib` (по умолчанию соседний `material.lib`).
- `--wear <name.wea>`: имя wear-entry внутри модельного архива (по умолчанию `<model_stem>.wea`).
- `--no-texture`: отключить текстуры и рендерить однотонным цветом.

## Авто-резолв текстуры

Если не передан `--texture`, демо пытается взять текстуру из игровых данных:

1. `model.msh -> model.wea` (первый wear-материал),
2. `material.lib` (`MAT0`) по имени материала с fallback `DEFAULT`,
3. первая непустая `textureName` фаза материала,
4. загрузка `Texm` из `textures.lib` (или `lightmap.lib` как fallback).

## Детерминированный снимок кадра

Для parity-проверок используется headless-сценарий с фиксированными параметрами:

```bash
cargo run -p render-demo --features demo -- \
  --archive "testdata/Parkan - Iron Strategy/animals.rlb" \
  --model "A_L_01.msh" \
  --lod 0 \
  --group 0 \
  --width 1280 \
  --height 720 \
  --angle 0.0 \
  --capture "target/render-parity/current/animals_a_l_01.png"
```

Явный выбор текстуры:

```bash
cargo run -p render-demo --features demo -- \
  --archive "testdata/Parkan - Iron Strategy/animals.rlb" \
  --model "A_L_01.msh" \
  --texture "PG09.0"
```

## Ограничения

- Используется только базовая texture-фаза (без полной material/fx анимации).
- Вывод через `glDrawElements(GL_TRIANGLES)` с index-buffer (позиции+UV).
