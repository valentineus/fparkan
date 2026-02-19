# Render pipeline

Документ описывает полный процесс рендера кадра в движке Parkan: Iron Strategy, без привязки к внутренним адресам/именам дизассемблера.

Связанные страницы:

- [MSH core](msh-core.md)
- [MSH animation](msh-animation.md)
- [Material (`MAT0`)](material.md)
- [Wear table (`WEAR`)](wear.md)
- [Texture (`Texm`)](texture.md)
- [FXID](fxid.md)

## 1. Инициализация рендера

На старте движок:

1. Выбирает видеодрайвер (software или аппаратный).
2. Создаёт render backend.
3. Подключает библиотеки ресурсов:
   - `Material.lib`
   - `Textures.lib`
   - `LightMap.lib`
   - `palettes.lib`
4. Инициализирует менеджеры:
   - material manager
   - texture/lightmap cache
   - effect manager
5. Загружает базовые world-ресурсы (включая наборы объектов сцены).

## 2. Структура кадра

Кадр выполняется как последовательность:

1. `Simulation update`
2. `Animation sampling`
3. `Visibility / culling`
4. `Material + texture resolve`
5. `Mesh draw`
6. `FX update + draw`
7. `UI/overlay draw`
8. `Present`

## 3. Geometry path

### 3.1. Подготовка инстансов

Для каждого видимого объекта:

1. Вычисляется `world transform`.
2. Выбирается `LOD`.
3. Для каждого узла выбирается slot через `Res1`.

### 3.2. Culling

Сначала отсекаются узлы/слоты по bounds (`AABB/sphere`) из `Res2`.

### 3.3. Батчи

Для каждого прошедшего slot:

1. Берутся батчи из диапазона `Res13`.
2. По `materialIndex` выбирается активный материал.
3. По фазе материала выбирается текстура/lightmap.
4. Выполняется `DrawIndexedPrimitive`:
   - индексный диапазон: `indexStart/indexCount`
   - базовая вершина: `baseVertex`
   - индексы читаются из `Res6`
   - вершины/атрибуты читаются из `Res3/Res4/Res5` (+ optional streams)

## 4. Animation path

Для анимированных моделей:

1. Для узла выбирается ключ через `Res19` и fallback-логику.
2. Декодируются `pos + quat` из `Res8`.
3. При необходимости выполняется blending двух сэмплов.
4. Узловая матрица передаётся в geometry path.

## 5. Material path

Material pipeline на кадре:

1. По material handle выбирается запись `MAT0`.
2. По игровому времени выбирается текущая фаза.
3. Применяются коэффициенты фазы (цвет/альфа/параметры).
4. Резолвятся ссылки на texture/lightmap.
5. Невалидные ссылки обрабатываются fallback-стратегией.

Практическая цепочка привязки для большинства `*.msh` ассетов из `*.rlb`:

1. Для модели выбирается одноимённый `WEAR` (`<model_stem>.wea`).
2. Из `WEAR` берётся material-слот (по имени, `legacyId` не участвует в выборе).
3. В `Material.lib` ищется `MAT0` по имени (`DEFAULT`, затем индекс `0` как fallback).
4. Из выбранной material-фазы берётся `textureName`.
5. `Texm` ищется в `Textures.lib` (и/или lightmap-архиве для lightmap-ветки).

## 6. Texture path

При резолве текстуры:

1. Ищется `Texm` entry по имени.
2. Проверяется и декодируется заголовок.
3. При необходимости применяется `mipSkip`.
4. Для indexed-формата подключается палитра.
5. Optional `Page` chunk интерпретируется как atlas-таблица.
6. Объект текстуры кладётся/берётся из cache.

## 7. FX path

Эффекты выполняются параллельно mesh-рендеру:

1. Для активных инстансов FX вычисляется runtime-коэффициент (`time_mode + flags`).
2. Команды FX обновляют внутреннее состояние.
3. Команды emit-этапа формируют примитивы/батчи эффектов.
4. Эффекты рисуются в 3D-кадре с собственным счётчиком батчей.

## 8. Псевдокод кадра

```c
void RenderFrame(Scene* scene, Camera* cam, float dt) {
    UpdateGame(scene, dt);

    for (Object* obj : scene->objects) {
        if (!obj->visible) continue;

        UpdateObjectAnimation(obj, scene->time);
        BuildObjectNodeTransforms(obj);
    }

    BeginFrame(cam);

    for (Object* obj : scene->objects) {
        if (!obj->visible) continue;
        RenderObjectMeshes(obj, cam);
    }

    UpdateAndRenderFx(scene, dt, cam);
    RenderUI(scene);
    Present();
}
```

## 9. Критичные условия для 1:1

1. Та же политика округления/FP для анимации и FX.
2. Та же логика fallback по материалам и текстурам.
3. Та же очередность стадий кадра.
4. Тот же контракт интерпретации `Res1/Res2/Res13/Res6`.
5. Тот же контракт `FXID` командного потока.

## 10. Статус валидации

- Порядок кадра и подключение `Material.lib / Textures.lib / LightMap.lib` подтверждены текущей runtime-валидацией проекта.
- Детальные инварианты форматов зафиксированы в `tools/msh_doc_validator.py` и `tools/fxid_abs100_audit.py`.

## 11. Статус покрытия и что осталось до 100%

Закрыто:

1. Высокоуровневый кадр: simulation -> animation -> culling -> material/texture resolve -> mesh draw -> fx -> ui -> present.
2. Связка MSH/MAT0/WEAR/Texm/FXID в едином runtime-процессе.
3. Форматная валидация входных данных на полном retail-корпусе.

Осталось:

1. Полный pixel-parity контур с эталонными кадрами оригинального рендера по набору моделей/сцен.
2. Формализация всех render-state деталей (точные blend/depth/cull/state transitions) для гарантии 1:1 в каждом draw-pass.
3. Полный coverage-пакет по динамическим веткам (FX-heavy кадры, сложные material-режимы, lightmap-комбинации).
