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

## MAT0 state and transparent backgrounds

For mission world ranges, the selected MAT0 material supplies its native
pipeline category. The category sets blend and alpha-test state while preserving
the range's depth and cull state; an animated material selector updates the
category with the selector. Queue membership is independent: sampled opacity
below one or native batch flags `0x108` mark a world range transparent, and a
queued range with depth enabled tests depth without writing it. Fixed
environment ranges keep their explicit states.

The fixed-camera preview explicitly marks nebula/stars, dome and cloud ranges as
transparent backgrounds. Opaque world ranges stay in their world slots; the
transparent world queue is sorted back-to-front and inserted immediately after
the last marked background. Other fixed ranges retain their relative order.
Thus cloud layers can be drawn before transparent foliage, while later sun,
weather and other foreground ranges keep their existing relative order. This
is a narrow queue rule, not a reordering of every sky or effects pass.

## Regression evidence

`cargo test -p fparkan-render-vulkan --lib material_category_and_transparency_preserve_world_state_in_either_order --offline`
checks that category 1 remains source-alpha with alpha testing, retains depth
and cull settings in either update order, and rejects unsupported categories.
`cargo test -p fparkan-render-vulkan --lib tree_canopy_queue_follows_sky_and_precedes_foreground_effects --offline`
checks the explicit queue order: screen gradient, opaque terrain/building,
shadow, nebula/dome/clouds, far-to-near transparent canopy, then sun/weather.

Viewer-only paired readbacks used `--frames 3 --validation
--legacy-camera-capture` with the pre-fix and patched builds. On AutoDemo TMA
object 4 (`wor_l_n1.dat`, `fr_l_plant`), the 1280x720 camera at eye
`[464.319, 904.719, 38.0]`, aimed at `[374.319, 814.719, 12.0]` with FOV
`0.62`, changed the two black facade panels to tinted, textured portal
surfaces. On Mission.01 object 12 (`s_tree_04`), the 1280x1024 camera at eye
`[492.63855, 312.90244, 46.5]` aimed at
`[492.63855, 442.90244, 55.0]` crosses the canopy against open sky; paired
three-frame captures at the same synthetic atmosphere phase (600 seconds) show
the patched build restoring the missing upper/right fronds along that sky
boundary. Both runs reported clean Vulkan validation. These captures verify our
preview behavior and do not prove the native renderer's pass order, portal
transmittance or exact color.

## GOG camera dispatch evidence

For the GOG `World3D.dll` baseline with SHA-256
`17e4a3089b2583a8cf2356c9db0390b1aba138356a09130d79b4e7e4791da61e`,
the exported `stdRenderGame` is RVA `0x13BD0`. It first calls
`Terrain::stdSetCurrentCamera2(camera)`, stores the camera pointer only for
the frame, and clears it before return. `sendEndOfRender` is a separate export
at RVA `0x13D90`. This is frame-order evidence only: the camera ABI, matrices
and viewport values are not recovered by this call-order analysis. The
implemented preview path below accepts selector-0 input produced through the
[native frame capture workflow](../../tools/native-frame-capture/README.md).
The capture workflow also records an observed mission path and, when
its thread/generation and clock checks succeed, an `atmosphere_seconds` sample.
For a fixed-camera preview, the renderer uses that optional absolute schedule
phase when no `--atmosphere-seconds` override is supplied and keeps schedule
sampling at that exact phase across readback frames. An explicit CLI value takes
precedence. The phase sample is not full simulation time: weather, RNG, and
other mission state remain unknown, so this evidence does not establish
full-frame parity.

Receiver-side GOG Terrain evidence refines this contract. `stdSetCurrentCamera2`
(RVA `0x4FD40` in Terrain SHA-256
`af87d1b2e728a0be73c52be3b44cc196ab46da7799f25a15d40f8c9b0b425ead`) queries
selector `18` on the supplied object and invokes slot `+12` on that result. It
does not store the supplied pointer in `stdGetCurrentCamera2`; that getter
returns a Terrain global populated by selector `8` during Terrain initialization.

`Terrain.dll!LoadCamera` adds a separate factory-side contract. Its GOG entry
at RVA `0x4EBE0` is `__stdcall` with four machine-word arguments, allocates
`0x1A4` bytes, calls the constructor at RVA `0x4EC60`, and returns the interface
at object offset `+0x134`. The constructor passes arguments 3 and 4 into base
initialization at `this + 4`, writes argument 3 to `this + 0x138`, and calls a
helper at RVA `0x4CEF0` with arguments 1, 2 and 4 plus `this`. Argument 1 is a
mode string: only `REFLECTION` and `REFLECTION_SHIFTED` produce a non-zero mode
value at `this + 0x1A0`. This is factory ABI evidence, not a recovered view or
projection matrix contract.

## Legacy camera preview path

`fparkan-game --legacy-camera-capture <json>` consumes the selector-0 transform,
the native D3D7 viewport RECT `[left, top, right, bottom]`, near/far planes and
field of view. It derives the D3D7 view and clip transform from those inputs,
carries the resulting `clip_from_world` matrix into the preview camera, and
keeps the view-matrix camera axes directly, so the captured basis, including
roll, is not reduced to yaw/pitch. The RECT becomes a physical drawable extent
of `right - left` by `bottom - top`. The preview window is fixed to that extent;
startup and non-zero resize events reject a physical-size mismatch instead of
stretching the captured view. Compare the result with the corresponding
`[left, top, right, bottom]` crop of the original PNG. `FreeFlightCamera` is not
created in this mode, but redraws still update environment and projected
shadows from the captured camera. Before environment updates can emit sound
events, the audio listener receives the captured position, forward and up axes;
initial world draw sorting uses the same captured position. A phase capture may
include an observed mission path and a verified `atmosphere_seconds` value; the
latter is used only when no explicit CLI override is supplied. This value pins
the atmosphere schedule sample during a fixed-camera comparison while FX still
receives real frame time. `simulation_time_seconds`, weather, RNG and complete
mission state remain unknown, and a camera/phase match alone does not establish
visual parity with a native frame.

The preview's sky heading is `atan2(forward.x, forward.y)`, matching the second
output calculated at `Terrain.dll` RVA `0x4F9C0`; `CSky` adds π at RVA `0x481C5`
when selecting screen-gradient sectors. This mapping is applied only at the sky
boundary and does not change `FreeFlightCamera` yaw or movement controls.
Matched same-camera, same-phase three-frame captures at phases 56.227 and
998.893 showed zero changed pixels before and after this correction; the
remaining sky and color mismatch is unresolved, so this source-level match does
not establish visual parity.

## Parity risks

`VulkanStaticCamera::from_legacy_d3d7` keeps the D3D7 view and projection
reconstruction in `fparkan-render`, then negates the vertical projection term
once at the Vulkan adapter boundary. D3D7's top-down viewport places positive
NDC Y at the top; the Vulkan swapchain uses a positive viewport height, so the
adapter flips NDC Y while the shader preserves `gl_Position`. The generic
`from_row_major_view_projection` path remains unchanged; its callers supply
Vulkan-ready matrices, as the free-flight camera already does.

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
