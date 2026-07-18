# Renderer Truth Table

Эта страница нужна для одной вещи: не позволять путям smoke, planning и capture
выглядеть как «почти готовый renderer». Каждый путь доказывает разный класс
свойств, и acceptance не должен смешивать их.

## Краткая матрица

| Path | Native window / swapchain | Draws pixels | Uses original assets | Acceptance class | Что доказывает | Чего не доказывает |
| --- | --- | --- | --- | --- | --- | --- |
| `fparkan-vulkan-smoke` / `VulkanSmokeRenderer` | Yes | Yes | Static MSH plus sampled TEXM | `covered-gpu` for Stage 0 smoke and explicit MSH/TEXM/descriptor bridge IDs | Loader, instance, surface, swapchain, submit/present, validation-clean triangle path; original MSH indexed draw; TEXM RGBA8 staging upload, sampler/descriptor binding, fragment sampling and one explicitly selected WEAR/MAT0 material path | Per-batch material selection, phase animation, lightmaps, terrain, gameplay rendering |
| `VulkanPlanningBackend` | No | No | Optional CPU-side IDs only | `covered-planning` | Deterministic command validation, canonical capture, frame submission planning | Любой live GPU draw, pixel parity, validation-clean asset frame |
| `RecordingBackend` | No | No | Optional CPU-side IDs only | `covered-planning` | Stable command capture for backend-neutral tests | Native window, Vulkan, GPU resource lifetime, pixels |
| `NullBackend` | No | No | Optional CPU-side IDs only | Usually `covered` for validation-only rows | Command stream framing and bounds validation | Capture stability, GPU execution, pixels |
| `VulkanAssetRenderer` | Yes | Yes | Yes | `covered-gpu` | Static original asset rendering: MSH/Texm/WEAR/MAT0/terrain through Vulkan | Animation/FX parity unless explicitly wired |
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

- Реальный Vulkan в репозитории имеет smoke triangle path и узкий static asset bridge. Помимо direct TEXM input, `fparkan-vulkan-smoke --model-root <GOG_ROOT> --model-archive fortif.rlb --model-name FR_L_MTP.msh --wear-root <GOG_ROOT> --wear-archive fortif.rlb --wear-name FR_L_MTP.WEA --material-index 0` проходит `WEAR → MAT0 → Textures.lib`, выбирает `MTP_01.0`, выполняет real indexed draw и загружает selected TEXM mip-0 в device-local image. Image переходит в sampling layout, записывается в `set=0,binding=0` combined image sampler и sampled fragment shader-ом. `Res5` UV0 декодируется как signed fixed point `int16 / 1024.0`; XZ-planar UV остаётся только fallback для модели без optional stream. Это один explicit material selector, не полный per-batch/phase/lightmap/terrain или gameplay renderer.
- `apps/fparkan-game` сейчас выдает `render-planning` JSON report поверх
  synthetic window descriptor и `VulkanPlanningBackend`.
- `apps/fparkan-viewer` сейчас inspection-only CLI и не открывает live Vulkan
  asset viewer.
- Следующий реальный milestone для rendered acceptance: `VulkanAssetRenderer`
  с upload/draw/capture path для хотя бы одной оригинальной модели и одного
  terrain slice.
