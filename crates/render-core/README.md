# render-core

CPU-подготовка draw-данных для моделей `MSH`.

Покрывает:

- обход `node -> slot -> batch`;
- раскрытие индексов в triangle-list (`position + uv0`);
- расчёт bounds по вершинам.

Тесты:

- построение рендер-сеток на реальных `.msh` из `testdata`;
- unit-test bounds.
