# Terrain + map loading

Документ описывает подсистему ландшафта и привязку terrain-данных к миру.

---

## 4.1. Обзор

`Terrain.dll` отвечает за рендер ландшафта (terrain), включая:

- Рендер мешей ландшафта (`"Rendered meshes"`, `"Rendered primitives"`, `"Rendered faces"`).
- Рендер частиц (`"Rendered particles/batches"`).
- Создание текстур (`"CTexture::CTexture()"` — конструктор текстуры).
- Микротекстуры (`"Unable to find microtexture mapping"`).

## 4.2. Текстуры ландшафта

В Terrain.dll присутствует конструктор текстуры `CTexture::CTexture()` со следующими проверками:

- Валидация размера текстуры (`"Unsupported texture size"`).
- Создание D3D‑текстуры (`"Unable to create texture"`).

Ландшафт использует **микротекстуры** (micro‑texture mapping chunks) — маленькие повторяющиеся текстуры, тайлящиеся по поверхности.

## 4.3. Защита от пустых примитивов

Terrain.dll содержит проверки:

- `"Rendering empty primitive!"` — перед первым вызовом отрисовки.
- `"Rendering empty primitive2!"` — перед вторым вызовом отрисовки.

Это подтверждает многопроходный рендер (как минимум 2 прохода для ландшафта).
