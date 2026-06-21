# Render frame

Кадр является последней стадией цикла, а не самостоятельной функцией renderer-а.
До draw calls уже накоплен input, рассчитан tick, применены отложенные операции,
выбрана камера и обновлён 3D sound listener.

## Frame skeleton

```text
system messages and input
  -> simulation calculation
  -> deferred object operations
  -> animation and transforms
  -> camera and sound listener
  -> visibility and render queues
  -> materials and draw passes
  -> renderer completion
  -> end-of-render callbacks and UI
```

В `World3D::stdRenderGame` доказан крупный порядок: camera передаётся Terrain,
настраиваются viewport/matrices, вызываются renderer boundary slots,
устанавливается `in_render`, выполняется traversal мира, закрывается world/shade
pass, вызывается renderer completion, снимается `in_render`, рассылается
end-of-render.

## Draw item

Подготовленный draw item содержит:

- node world matrix;
- batch flags and index range;
- WEAR material handle;
- MAT0 active phase and coefficients;
- texture handle;
- optional lightmap handle;
- render phase and sorting key;
- legacy pipeline state.

Подготовленный item должен ссылаться на immutable данные кадра. Изменение phase
или texture cache посреди прохода не должно менять уже собранную очередь.

## Parity risks

- x87 precision and rounding;
- scalar/SIMD `g_FastProc` differences;
- object, batch and transparent primitive order;
- depth, cull, alpha test and blend transitions;
- mip-skip, palette and Page coordinates;
- material fallback and phase selection;
- RNG sequence for FX and atmosphere;
- device capability fallback;
- simulation time quantization.

Для отладки нужен deterministic frame capture: camera state, visible object IDs,
draw-item list, pipeline keys, matrices и hashes промежуточных buffers.
