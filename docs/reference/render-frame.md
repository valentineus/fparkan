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

## GOG camera dispatch evidence

For the GOG `World3D.dll` baseline with SHA-256
`17e4a3089b2583a8cf2356c9db0390b1aba138356a09130d79b4e7e4791da61e`,
the exported `stdRenderGame` is RVA `0x13BD0`. It first calls
`Terrain::stdSetCurrentCamera2(camera)`, stores the camera pointer only for
the frame, and clears it before return. `sendEndOfRender` is a separate export
at RVA `0x13D90`. This is frame-order evidence only: the camera ABI, matrices
and viewport values still require dynamic capture or further decompilation.

Receiver-side GOG Terrain evidence refines this contract. `stdSetCurrentCamera2`
(RVA `0x4FD40` in Terrain SHA-256
`af87d1b2e728a0be73c52be3b44cc196ab46da7799f25a15d40f8c9b0b425ead`) queries
selector `18` on the supplied object and invokes slot `+12` on that result. It
does not store the supplied pointer in `stdGetCurrentCamera2`; that getter
returns a Terrain global populated by selector `8` during Terrain initialization.

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
