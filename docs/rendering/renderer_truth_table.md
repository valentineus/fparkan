# Renderer Truth Table

Эта страница нужна для одной вещи: не позволять путям smoke, planning и capture
выглядеть как «почти готовый renderer». Каждый путь доказывает разный класс
свойств, и acceptance не должен смешивать их.

## Краткая матрица

| Path | Native window / swapchain | Draws pixels | Uses original assets | Acceptance class | Что доказывает | Чего не доказывает |
| --- | --- | --- | --- | --- | --- | --- |
| `fparkan-vulkan-smoke` / `VulkanSmokeRenderer` | Yes | Yes | Static MSH plus sampled TEXM | `covered-gpu` for Stage 0 smoke and explicit MSH/TEXM/descriptor bridge IDs | Loader, instance, surface, swapchain, submit/present, validation-clean triangle path; original MSH indexed draw; TEXM RGBA8 staging upload; per-batch WEAR/MAT0 diffuse descriptors and fragment sampling | MAT0 phase animation, lightmaps, legacy blend/depth/cull states, terrain, camera/node transforms, gameplay rendering |
| `VulkanPlanningBackend` | No | No | Optional CPU-side IDs only | `covered-planning` | Deterministic command validation, canonical capture, frame submission planning | Любой live GPU draw, pixel parity, validation-clean asset frame |
| `RecordingBackend` | No | No | Optional CPU-side IDs only | `covered-planning` | Stable command capture for backend-neutral tests | Native window, Vulkan, GPU resource lifetime, pixels |
| `NullBackend` | No | No | Optional CPU-side IDs only | Usually `covered` for validation-only rows | Command stream framing and bounds validation | Capture stability, GPU execution, pixels |
| `VulkanAssetRenderer` | Yes | Yes | Yes | `covered-gpu` | Static original asset rendering: MSH/Texm/WEAR/MAT0/terrain through Vulkan | Animation/FX parity unless explicitly wired |
| `fparkan-game --backend static-vulkan` | Yes, GOG/Part 1/Part 2 `Autodemo.00` | Yes, merged static MSH component draws | All prepared MSH components from first mission root; first MAT0 diffuse TEXM per source selector | `covered-gpu` only for the narrow static-preview bridge | Opt-in mission-to-native-window bootstrap, component mesh merge, preview-local selector remap, diffuse descriptor upload and synchronized teardown telemetry | Full mission scene, later MAT0 phases/animation, lightmaps, placed transforms/orientation, camera, gameplay, original-runtime parity |
| Future rendered `fparkan-game` mode | Yes | Yes | Yes | `covered-gpu` plus original-evidence IDs | Mission-driven render snapshot execution and pixel capture | Original-runtime parity for animation/FX/x87 without dedicated captures |

## Rules

1. IDs со смыслом `VK`, `GPU`, `DRAW`, `PIXEL`, `VALIDATION` или `RENDERED`
   на Stage 3+ не могут закрываться через `NullBackend`, `RecordingBackend`
   или `VulkanPlanningBackend`.
2. `covered-planning` означает command planning/capture evidence. Этот статус
   никогда не считается доказательством draw пикселей.
3. `covered-stub` зарезервирован для явно помеченных `STUB` acceptance rows и
   не считается compatibility closure для FX lifecycle.
4. `covered-gpu` требует live native handles, реальный draw path и связанный
   renderer artifact: report, capture или approved pixel.

## Current repository status

- Реальный Vulkan в репозитории имеет smoke triangle path и узкий static asset bridge. `VulkanStaticDrawRange` сохраняет исходный `Batch20.material_index`; когда smoke запускается с `--wear-root`, `--wear-archive`, `--wear-name` без override, он дедуплицирует selectors, проходит каждый через `WEAR → MAT0 → Textures.lib`, создаёт по одному image/descriptor set и бинит set непосредственно перед соответствующим indexed draw. Direct TEXM и `--material-index` — намеренно однотекстурные compatibility modes. Fresh GOG `fortif.rlb::FR_L_MTP.msh` подтверждает 237 batch draws, но его selectors все `0`, поэтому live report содержит один binding `MTP_01.0`; unit contract подтверждает точное сопоставление двух разных selectors с разными descriptor sets. Это не доказывает material phase animation, lightmaps, alpha/depth/cull state, terrain, camera/node transforms или pixel approval.
- Lightmap остаётся отдельным, не реализованным contract: оригинальный `World3D.dll` экспортирует самостоятельный `SetLightMapLib` наряду с `SetTexturesLib` и `SetMaterialLib`; WEAR содержит независимый блок `LIGHTMAPS`. Текущая документация не подтверждает связь этих slots с `Batch20.material_index` или их UV/channel semantics, поэтому viewer не подменяет lightmap diffuse texture и не добавляет недоказанное binding.
- `apps/fparkan-game` по умолчанию выдает `render-planning` JSON report поверх
  synthetic window descriptor и `VulkanPlanningBackend`. Opt-in `--backend static-vulkan`
  уже создаёт native `winit` window и передаёт все подготовленные MSH-компоненты первого root в
  `VulkanSmokeRenderer`. Каждый исходный `Batch20.material_index` сначала разрешается внутри
  собственного WEAR/MAT0 visual, затем получает уникальный preview-local selector и первый
  diffuse TEXM. Режим использует
  отдельный first-root preview loader: normal `load_mission` по-прежнему готовит все reachable
  assets и весь graph, тогда как preview строит graph и готовит assets только для первого
  mission root. `--load-progress <file>` writes the last entered loader phase synchronously for
  timeout diagnosis. Fresh GOG `MISSIONS/Autodemo.00/data.tma` run passed in 38.7 seconds with
  one presented frame, native 1280×720 swapchain (2 images), 14 mesh components and 14 original
  diffuse material descriptors, 7,372,800-byte readback hash `16595193636416981301`, and
  validation warnings/errors `0/0`. Part 1 matches that artifact; Part 2 passes validation with
  14/14 but has a distinct hash `18268338333658342130`. This is `covered-gpu` evidence for that
  narrow static-preview bridge only, not full-scene or original-renderer parity.
- `apps/fparkan-viewer` сейчас inspection-only CLI и не открывает live Vulkan
  asset viewer.
- Следующий реальный milestone для rendered acceptance: `VulkanAssetRenderer`
  с upload/draw/capture path для хотя бы одной оригинальной модели и одного
  terrain slice.
