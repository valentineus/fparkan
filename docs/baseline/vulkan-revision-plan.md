# План реализации: Vulkan revision (Windows)

Это локальная, действующая редакция плана `stage 0--5`. Она была получена
вдумчивой сверкой с одноимённой страницей книги FParkan в Notion 18 июля
2026 года. В Notion оставлены исторические формулировки и кроссплатформенные
цели; этот документ сохраняет все применимые технические требования и
приводит их к текущему контракту: самостоятельный движок для Windows,
оригинальные файлы игры и Vulkan.

## Неизменяемые решения

- Vulkan — единственный GPU API. Он заменяет прежнюю DirectDraw/Direct3D
  реализацию на уровне наблюдаемой семантики кадра, а не через эмуляцию COM
  объектов или буквальный перевод старых вызовов.
- `winit` — отдельный adapter окна, ввода и event loop. Vulkan не является
  заменой SDL2: это независимые слои. В текущем проекте SDL2 не используется.
- `ash`, `ash-window` и `raw-window-handle` остаются внутри Windows
  platform/Vulkan adapters. Backend-neutral crates не экспортируют raw Vulkan
  handles и сохраняют `#![forbid(unsafe_code)]`; каждый `unsafe` в adapter-е
  имеет локальный safety contract, правило владения и regression test.
- Baseline: Vulkan 1.1, surface + Win32 surface + swapchain, classic render
  pass, binary semaphores и fences. Format/queue/present/image-count/sampler
  capabilities запрашиваются у конкретного устройства. Dynamic rendering,
  descriptor indexing, synchronization2, timeline semaphores и extended
  dynamic state допускаются только через capability gate.
- Исходные MSH, WEAR, MAT0 и Texm остаются CPU-форматами. Стартовый upload
  путь — канонический RGBA8 UNORM; packed/native GPU formats допустимы только
  после доказательства эквивалентности. Shader variants собираются offline в
  SPIR-V и проверяются validator-ом, manifest-ом и hash.
- Первичный эталон — backend-neutral command capture. Сравнение пикселей
  начинается лишь после совпадения draw order, pipeline key, resource IDs,
  descriptor bindings, ranges и transforms. GPU handles, allocator addresses
  и driver timing не входят в deterministic state hash.

Windows — единственная runtime-платформа acceptance. Требования Notion к
Linux, macOS/MoltenVK, portability enumeration и hosted CI намеренно не
переносятся: они противоречат утверждённой области проекта, а не являются
пропуском документации. Headless сборка по-прежнему не зависит от окна,
Vulkan loader или `winit`.

## Stage 0 — воспроизводимая Windows/Vulkan основа

**Цель:** минимальный реальный Vulkan vertical slice без игровых assets и
локальные повторяемые gates.

- Зафиксировать stable Rust/MSRV, `Cargo.lock` и `--locked`; расширять
  `cargo xtask ci` форматированием, tests, clippy, документацией, policy для
  licenses/advisories/sources и проверкой разрешённого `unsafe` allowlist.
- Synthetic gate не читает лицензированные каталоги и не может молча пропускать
  тест. Licensed corpus запускается отдельно по абсолютным путям local
  manifest. Hosted CI/CD в этот scope не входит.
- Поддерживать typed parsing конфигурации xtask и `cargo_metadata`, а не
  ручную интерпретацию TOML; исключать устаревшие adapter names и Python
  runtime components из policy.
- Поддерживать `fparkan-platform-winit` (lifecycle, resize/DPI, input,
  suspend/resume, raw handles) и `fparkan-render-vulkan` (instance,
  validation, device scoring, queues, swapchain, resize/out-of-date/suboptimal
  handling, deterministic capability report).
- Acceptance: Windows smoke создаёт настоящее окно/swapchain, показывает не
  менее 300 кадров с resize и завершается без validation errors; negative
  cases проверяют loader/device/present-queue/surface-format failures.

## Stage 1 — пути, VFS и архивы

**Цель:** безопасный lossless resource substrate без GPU coupling.

- Для каждого пути различать raw legacy bytes, normalized path, ASCII lookup
  key и host path; strict и compatible policy не смешивать.
- Применить symlink-safe traversal и casefold-collision policy ко всем VFS.
- В каждый parser/decompressor внедрить общие `DecodeLimits` и
  `AllocationBudget`; malformed offsets, counts и decompression bombs должны
  завершаться bounded errors.
- Довести NRes и RsLi до lossless reader/editor/writer: сохранять unknown и
  non-zero regions, stable directory order, все наблюдённые decode methods,
  explicit compatibility profile и output limits.
- Resource repository обязан иметь generation handles, decoded-byte budget,
  deterministic eviction, lock-free decompression section и структурированные
  ошибки с archive/entry/path/offset/phase/cause chain.
- Acceptance: synthetic no-edit и edit roundtrip, stale handles, traversal,
  symlink/casefold и byte-identical corpus reports; Part 1/Part 2 не дают
  необъяснённых parser failures.

## Stage 2 — prototype graph и CPU assets

**Цель:** полный mission-reachable graph и typed prepared assets до GPU.

- Разрешить `objects.rlb`, unit DAT, inheritance, BASE/resource variants и
  все компоненты unit, сохраняя hierarchy, provenance и multi-component
  composition.
- Каждый edge хранит typed provenance: mission object, component, prototype,
  model, wear, material, texture, lightmap или effect. Циклы, depth limit,
  optional fallback и corrupt reachable dependency имеют разные outcomes.
- `fparkan-assets` — единственный слой CPU preparation; apps и runtime не
  парсят assets ad hoc. Assets immutable, имеют stable IDs, а graph failures
  содержат полную parent chain.
- Acceptance: graph order/IDs стабильны; все mission-reachable requests
  обеих частей завершаются с failures 0 и передают runtime только prepared
  assets.

## Stage 3 — статический Vulkan viewer

**Цель:** доказуемый статический MSH/terrain render из оригинальных assets.

- Закрыть validation streams/slots/batches/indices, Texm decode/mips/palettes/
  Page rectangles и WEAR/MAT0 fallback с раздельными texture/lightmap identity.
- Backend-neutral `LegacyPipelineState` и canonical `PipelineKey` выбираются
  до GPU. Vulkan adapter владеет staging/device buffers, image transitions,
  samplers/descriptors, pipeline cache, depth, diffuse/lightmap bindings,
  alpha/depth/cull/blend mapping и lifecycle per-frame resources.
- Viewer/debug modes включают model, texture, material, wireframe, normals,
  bounds, LOD/group и terrain; upload cache ограничен GPU budget.
- Acceptance: CPU golden vectors, descriptor/pipeline-key/row-stride tests,
  command captures до GPU и fixed-camera captures модели, lightmapped модели
  и terrain; Windows validation smoke остаётся clean.

## Stage 4 — animation и FX runtime

**Цель:** заменить reference stubs доказанным deterministic runtime.

- Реализовать type 8/type 19 node sampling, fallback keys, hierarchy и
  material timeline по подтверждённым modes/masks. Portable math не выдают за
  x87-compatible: второй путь появляется только после captured vectors.
- FXID отделяет lifecycle/time/RNG gates от backend: неподтверждённые fields
  сохраняются raw и не исполняются как догадки; emit формирует
  backend-neutral primitive/audio commands.
- Pose/effect snapshots immutable per frame; Part 1/Part 2 profiles различают
  только там, где это подтверждено differential captures.
- Acceptance: frame-by-frame poses имеют approved references, FXID corpus не
  имеет parser errors, один seed даёт одинаковые commands. До этого semantic
  статус строго `reference-only`, а не `runtime-compatible`.

## Stage 5 — карта, миссия и мир

**Цель:** транзакционно загрузить миссию, выполнить headless steps и показать
тот же immutable world snapshot через Vulkan.

- Закрыть Land.msh/TerrainFace28, Land.map, grid/graph validation и runtime
  spatial acceleration для surface/raycast/visibility queries.
- Loader выполняет `Context -> Map -> TMA -> Graph -> Assets -> Construct ->
  Register`, откатывая любую ошибку. Он сохраняет raw transforms, properties,
  original IDs и provenance всех mission components.
- World queue, generation handles, deferred deletion, deterministic clock и
  snapshot contract проверяются replay/hash tests; terrain/navigation и render
  читают один опубликованный snapshot, не mutable world.
- Acceptance: headless mission replay стабилен, transaction rollback не
  оставляет частичного мира, а Windows Vulkan frame использует ту же snapshot
  и имеет связанный command/pixel artifact.

## Сверка с Notion

Восемь томов локальной книги покрывают 42 основные статьи Notion по тем же
разделам I--VIII; приложения сведены в `appendices/` и том VIII. Специальное
Vulkan-ревью дополнительно внесло в локальные материалы следующие точные
факты: исходный Ngi32 dynamically resolves DirectDraw/Direct3D, современная
граница замены находится выше Vulkan, а совпадающий SHA-256 Ngi32 в Частях 1 и
2 позволяет использовать один backend contract. Детали доказательства и
текущие native captures находятся в `tomes/05-render.md`,
`evidence/original_engine_hashes.md` и `rendering/renderer_truth_table.md`.
