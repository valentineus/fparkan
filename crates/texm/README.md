# texm

Парсер формата текстур `Texm`.

Покрывает:

- header (`width/height/mipCount/flags/format`);
- core size расчёт;
- optional `Page` chunk;
- строгую валидацию layout.

Тесты:

- прогон по реальным `Texm` из `testdata`;
- синтетические edge-cases (indexed + page, minimal rgba).
