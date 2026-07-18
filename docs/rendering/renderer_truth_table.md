# Renderer Truth Table

Эта страница нужна для одной вещи: не позволять путям smoke, planning и capture
выглядеть как «почти готовый renderer». Каждый путь доказывает разный класс
свойств, и acceptance не должен смешивать их.

## Краткая матрица

| Path | Native window / swapchain | Draws pixels | Uses original assets | Acceptance class | Что доказывает | Чего не доказывает |
| --- | --- | --- | --- | --- | --- | --- |
| `fparkan-vulkan-smoke` / `VulkanSmokeRenderer` | Yes | Yes | Static MSH only | `covered-gpu` for Stage 0 smoke and explicit MSH bridge IDs | Loader, instance, surface, swapchain, submit/present, validation-clean triangle path; original MSH vertex/index upload and indexed draw | Texture sampling, descriptors, terrain, gameplay rendering |
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

- Реальный Vulkan в репозитории имеет smoke triangle path и узкий static-MSH bridge: `fparkan-vulkan-smoke --model-root <GOG_ROOT> --model-archive system.rlb --model-name MTCHECK.MSH` загружает validated original MSH, разворачивает batch `base_vertex`, нормализует XZ viewer plane и выполняет real indexed draw. Это не является texture/material/terrain renderer.
- `apps/fparkan-game` сейчас выдает `render-planning` JSON report поверх
  synthetic window descriptor и `VulkanPlanningBackend`.
- `apps/fparkan-viewer` сейчас inspection-only CLI и не открывает live Vulkan
  asset viewer.
- Следующий реальный milestone для rendered acceptance: `VulkanAssetRenderer`
  с upload/draw/capture path для хотя бы одной оригинальной модели и одного
  terrain slice.
