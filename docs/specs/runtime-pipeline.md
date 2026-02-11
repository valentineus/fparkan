# Runtime pipeline

Документ фиксирует runtime-поведение движка: кто кого вызывает в кадре, как проходят рендер, коллизия и подключение эффектов.

---

## 1.15. Алгоритм рендера модели (реконструкция)

```
Вход: model, instanceTransform, cameraFrustum

1. Определить current_lod ∈ {0, 1, 2} (по дистанции до камеры / настройкам).

2. Для каждого node (nodeIndex = 0 .. nodeCount−1):
   a. Вычислить nodeTransform = instanceTransform × nodeLocalTransform

   b. slotIndex = nodeTable[nodeIndex].slotMatrix[current_lod][group=0]
      если slotIndex == 0xFFFF → пропустить узел

   c. slot = slotTable[slotIndex]

   d. // Frustum culling:
      transformedAABB = transform(slot.aabb, nodeTransform)
      если transformedAABB вне cameraFrustum → пропустить

      // Альтернативно по сфере:
      transformedCenter = nodeTransform × slot.sphereCenter
      scaledRadius = slot.sphereRadius × max(scaleX, scaleY, scaleZ)
      если сфера вне frustum → пропустить

   e. Для i = 0 .. slot.batchCount − 1:
      batch = batchTable[slot.batchStart + i]

      // Фильтрация по batchFlags (если нужна)

      // Установить материал:
      setMaterial(batch.materialIndex)

      // Установить transform:
      setWorldMatrix(nodeTransform)

      // Нарисовать:
      DrawIndexedPrimitive(
          baseVertex  = batch.baseVertex,
          indexStart   = batch.indexStart,
          indexCount   = batch.indexCount,
          primitiveType = TRIANGLE_LIST
      )
```

---

## 1.16. Алгоритм обхода треугольников (коллизия / пикинг)

```
Вход: model, nodeIndex, lod, group, filterMask, callback

1. slotIndex = nodeTable[nodeIndex].slotMatrix[lod][group]
   если slotIndex == 0xFFFF → выход

2. slot = slotTable[slotIndex]
   triDescIndex = slot.triStart

3. Для каждого batch в диапазоне [slot.batchStart .. slot.batchStart + slot.batchCount − 1]:
   batch = batchTable[batchIndex]
   triCount = batch.indexCount / 3     // округление: (indexCount + 2) / 3

   Для t = 0 .. triCount − 1:
     triDesc = triDescTable[triDescIndex]

     // Фильтрация:
     если (triDesc.triFlags & filterMask) → пропустить

     // Получить индексы вершин:
     idx0 = indexBuffer[batch.indexStart + t*3 + 0] + batch.baseVertex
     idx1 = indexBuffer[batch.indexStart + t*3 + 1] + batch.baseVertex
     idx2 = indexBuffer[batch.indexStart + t*3 + 2] + batch.baseVertex

     // Получить позиции:
     p0 = positions[idx0]
     p1 = positions[idx1]
     p2 = positions[idx2]

     callback(triDesc, idx0, idx1, idx2, p0, p1, p2)

     triDescIndex += 1
```

---


---

## 3.1. Архитектурный обзор

Подсистема эффектов реализована в `Effect.dll` и интегрирована в рендер через `Terrain.dll`.

### Экспорты Effect.dll

| Функция              | Описание                                               |
|----------------------|--------------------------------------------------------|
| `CreateFxManager`    | Создать менеджер эффектов (3 параметра: int, int, int) |
| `InitializeSettings` | Инициализировать настройки эффектов                    |

`CreateFxManager` возвращает объект‑менеджер, который регистрируется в движке и управляет всеми эффектами.

### Телеметрия из Terrain.dll

Terrain.dll содержит отладочную статистику рендера:

```
"Rendered meshes : %d"
"Rendered primitives : %d"
"Rendered faces : %d"
"Rendered particles/batches : %d/%d"
```

Из этого следует:

- Частицы рендерятся **батчами** (группами).
- Статистика частиц отделена от статистики мешей.
- Частицы интегрированы в общий 3D‑рендер‑пайплайн.

