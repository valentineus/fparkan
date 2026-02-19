# msh-core

Парсер core-части формата `MSH`.

Покрывает:

- `Res1`, `Res2`, `Res3`, `Res6`, `Res13` (обязательные);
- `Res4`, `Res5`, `Res10` (опциональные);
- slot lookup по `node/lod/group`.

Тесты:

- прогон по всем `.msh` в `testdata`;
- синтетическая минимальная модель.
