# render-demo

Тестовый рендерер Parkan-моделей на Rust (`SDL2 + OpenGL ES 2.0`).

## Назначение

- Проверить, что `nres + msh-core + render-core` дают рабочий draw-path на реальных ассетах.
- Служить минимальным reference-приложением.

## Запуск

```bash
cargo run -p render-demo --features demo -- \
  --archive "testdata/Parkan - Iron Strategy/animals.rlb" \
  --model "A_L_01.msh" \
  --lod 0 \
  --group 0
```

Параметры:

- `--archive` (обязательный): NRes-архив с `.msh` entry.
- `--model` (опционально): имя модели; если не задано, берётся первая `.msh`.
- `--lod` (опционально, default `0`).
- `--group` (опционально, default `0`).

## Ограничения

- Рендер только геометрии (без материалов/текстур/FX).
- Вывод через `glDrawArrays(GL_TRIANGLES)` из расширенного triangle-list.
