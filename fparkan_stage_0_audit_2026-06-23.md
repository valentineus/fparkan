# FParkan — аудит Stage 0 и план полного закрытия

**Проект:** `valentineus/fparkan`  
**Проверенная ветка:** `devel` GitHub-зеркала  
**Дата аудита:** 23 июня 2026 года  
**Область:** только Stage 0 — Governance, reproducibility и Vulkan foundation  
**Метод:** статический архитектурный и кодовый аудит  
**Сборка и исполнение:** не выполнялись; `cargo build`, `cargo test`, Vulkan smoke и validation jobs не запускались

---

## 1. Итоговый вердикт

**Stage 0 не закрыт и находится в статусе `BLOCKED`.**

Главный критерий Stage 0 — воспроизводимый репозиторий и минимальный настоящий Vulkan vertical slice на Windows, Linux и macOS. В проверенном состоянии:

- отсутствует `fparkan-platform-winit`;
- отсутствует `fparkan-render-vulkan`;
- отсутствуют Vulkan instance/device/surface/swapchain;
- `fparkan-game` использует `RecordingBackend`, а не GPU backend;
- workspace по-прежнему содержит SDL/OpenGL stub adapters;
- Rust toolchain закреплён только как изменяемый канал `stable`;
- `cargo xtask ci` не реализует полный канонический gate;
- нет подтверждённых артефактов Windows/Linux/macOS smoke jobs.

### Сводная оценка

| Группа требований | Статус | Основной блокер |
|---|---|---|
| Reproducibility и toolchain | **FAIL** | Toolchain не закреплён точной версией, MSRV не объявлен |
| Repository policy и CI | **FAIL** | Неполные fmt/test/clippy/doc/security gates |
| Platform abstraction | **FAIL** | Core API содержит OpenGL-specific contract; `winit` adapter отсутствует |
| Vulkan backend | **FAIL** | Нет Vulkan loader/device/surface/swapchain/pipeline |
| macOS portability | **FAIL** | Нет MoltenVK integration и portability handling |
| Offline shaders | **FAIL** | Нет SPIR-V build/validation/hash pipeline |
| Legacy cleanup | **FAIL** | SDL/GL stubs остаются workspace members |
| Headless isolation | **PASS на manifest-level** | Автоматическое доказательство dependency closure ещё требуется |
| Native acceptance | **FAIL / NOT RUNNABLE** | Нет реального backend и platform artifacts |

Stage 0 можно объявить закрытым только после прохождения реального Vulkan smoke на всех трёх системах и публикации machine-readable артефактов.

---

## 2. Область и ограничения аудита

Канонические требования взяты из документа:

- «План реализации stage 0–5: Vulkan revision»;
- <https://app.notion.com/p/387e79f2db3981778f94cdf34db5f93f>.

Проверялась ветка:

- <https://github.com/valentineus/fparkan/tree/devel>.

Ограничения:

1. Ветка `devel` является движущейся ссылкой. Следующий formal audit следует выполнять на закреплённом commit SHA или tag.
2. README указывает self-hosted repository как primary. Его закрытые CI runners и artifacts не были доступны.
3. Код не собирался и не запускался по условию аудита.
4. Vulkan runtime, validation layers, MoltenVK и native window creation не проверялись динамически.
5. Статический анализ достаточен для определения текущих архитектурных блокеров: требуемых adapters и зависимостей в workspace нет.

---

## 3. Матрица требований Stage 0

| Требование | Статус | Текущее состояние | Необходимо для закрытия |
|---|---|---|---|
| Exact stable Rust toolchain | **FAIL** | `rust-toolchain.toml`: `channel = "stable"` | Закрепить точную версию, например `1.xx.y` |
| Объявленный MSRV | **FAIL** | `workspace.package.rust-version` отсутствует | Добавить `rust-version` и отдельный MSRV job |
| Полный `cargo xtask ci` | **FAIL** | Есть custom rustfmt, policy, workspace test и clippy | Добавить канонические fmt/test/clippy/doc/security gates |
| `--all-targets --all-features` | **FAIL** | Не используются текущим `ci` | Добавить к test/clippy/doc gates |
| Clippy `-D warnings` | **FAIL** | Явно не передаётся | Сделать предупреждения blocking |
| Rustdoc broken-link gate | **FAIL** | Отсутствует | Добавить `RUSTDOCFLAGS=-D warnings -D rustdoc::broken_intra_doc_links` |
| License/advisory/source policy | **PARTIAL / UNVERIFIED** | Есть custom policy и GPL workspace license | Подключить `cargo-deny` или эквивалент и хранить versioned policy |
| Typed TOML parsing | **FAIL** | Licensed manifest разбирается вручную построчно | `serde` + TOML schema + `deny_unknown_fields` |
| `cargo_metadata` policy | **FAIL** | Dependency rules не опираются на typed Cargo graph | Добавить `cargo_metadata` и package-ID based checks |
| CI matrix Windows/Linux/macOS | **UNVERIFIED / BLOCKER** | Доступных platform artifacts нет | Создать native matrix и сохранять reports |
| Backend-neutral platform API | **FAIL** | В core есть `GraphicsProfile`, GL/GLES versions и `WindowPort::present()` | Удалить GL context concepts; present перенести в renderer |
| `fparkan-platform-winit` | **FAIL** | В workspace только SDL-named stub | Реализовать настоящий event loop/window adapter |
| `fparkan-render-vulkan` | **FAIL** | В workspace только GL-named recording stub | Реализовать настоящий Vulkan backend |
| Vulkan loader/instance/device | **FAIL** | Vulkan bindings отсутствуют | Добавить `ash`, instance, device selection, queues |
| Surface/swapchain/present | **FAIL** | Отсутствуют | Реализовать platform surface и swapchain lifecycle |
| Indexed triangle | **FAIL** | Есть только command capture | Нарисовать реальный indexed triangle |
| Resize/out-of-date/suboptimal | **FAIL** | Swapchain отсутствует | Реализовать полную recreation policy |
| Deterministic capability report | **FAIL** | Device discovery отсутствует | Pure scoring policy + JSON capability report |
| macOS portability | **FAIL** | MoltenVK integration отсутствует | Portability enumeration, subset и packaged MoltenVK |
| Offline SPIR-V pipeline | **FAIL** | GL stub проверяет только synthetic markers | Pinned compiler, validator, descriptor manifest и hashes |
| Legacy adapter removal | **FAIL** | SDL/GL crates входят в workspace | Удалить crates и все references после замены |
| Game/viewer composition | **FAIL** | Game использует `RecordingBackend`; viewer — CLI inspector | Подключить winit + Vulkan только в composition roots |
| Headless isolation | **PASS на manifest-level** | Нет window/Vulkan dependency | Добавить automated Cargo metadata assertion |
| 300 frames + resize + validation=0 | **FAIL** | Невозможно выполнить без backend | Native smoke jobs на трёх OS |
| Negative Vulkan tests | **FAIL** | Нет Vulkan error model | Loader/device/queue/format failure fixtures |

---

## 4. Замечания

### S0-B01 — Workspace содержит удаляемые SDL/OpenGL stub crates

**Приоритет:** BLOCKER  
**Файлы:** `Cargo.toml`, `adapters/fparkan-platform-sdl`, `adapters/fparkan-render-gl`

Root workspace включает оба прежних adapter crate. При этом:

- SDL adapter не зависит от SDL и содержит in-memory stubs;
- GL adapter не зависит от OpenGL и только сохраняет canonical command captures;
- их tests доказывают deterministic stub behavior, а не platform/GPU integration.

Это создаёт ложноположительный сигнал готовности backend-а.

**Рекомендация:**

1. До появления замены пометить crates как `legacy-proof` и исключить из default production composition.
2. Добавить policy, запрещающий приложениям зависеть от них.
3. После подключения `platform-winit` и `render-vulkan` удалить crates, lockfile references, docs и tests.

### S0-B02 — Core platform contract остаётся OpenGL-specific

**Приоритет:** BLOCKER  
**Файл:** `crates/fparkan-platform/src/lib.rs`

Проблемы:

- `GraphicsProfile::DesktopCore/Embedded` описывает GL/GLES profile;
- `GraphicsContextRequest` описывает создание GL context;
- `WindowPort::present()` ошибочно закрепляет presentation за window abstraction;
- `PlatformEvent` содержит только `Quit`;
- отсутствуют resize, scale factor, focus, keyboard, mouse, suspend/resume и raw handles;
- `PlatformError::Backend` не содержит source/context.

Для Vulkan окно не выполняет present. Surface, swapchain, image acquisition и queue presentation принадлежат render adapter.

**Рекомендация:** platform crate должен предоставлять только:

- event/lifecycle model;
- physical и logical size;
- scale factor;
- normalized input;
- raw window/display handles;
- structured platform errors.

### S0-B03 — Реального Vulkan code path нет

**Приоритет:** BLOCKER

В inspected manifests отсутствуют `ash`, `ash-window`, `winit` и `raw-window-handle`. Следовательно, текущий код не может создать Vulkan instance/device/surface/swapchain.

`fparkan-game` выполняет backend-neutral capture через `RecordingBackend`. Это полезный CPU oracle, но не Vulkan renderer.

**Definition of fixed:** отдельный smoke executable открывает окно, создаёт Vulkan swapchain, рисует indexed triangle, обрабатывает resize и корректно завершается.

### S0-B04 — `cargo xtask ci` не соответствует exit gate

**Приоритет:** BLOCKER  
**Файл:** `xtask/src/main.rs`

Текущий gate не подтверждает:

- все targets и features;
- clippy с `-D warnings`;
- rustdoc warnings и broken links;
- advisory/source policy;
- dependency denylist;
- отсутствие project-owned unsafe вне разрешённого Vulkan boundary;
- корректность typed acceptance manifests;
- platform-native smoke jobs.

Custom recursive rustfmt также может расходиться с canonical `cargo fmt --all -- --check`.

### S0-B05 — Toolchain не воспроизводим

**Приоритет:** BLOCKER  
**Файл:** `rust-toolchain.toml`

Канал `stable` изменяется. Один и тот же commit может использовать разные компиляторы в разные дни. MSRV также не объявлен.

**Рекомендация:**

- закрепить точный Rust release;
- указать `rust-version`;
- обновлять toolchain отдельным reviewed PR;
- сохранять toolchain и SDK versions в acceptance report.

### S0-H01 — Нужен изолированный audited unsafe boundary

**Приоритет:** HIGH

`unsafe_code = "forbid"` правильно сохранять для backend-neutral crates. Однако Vulkan FFI требует локальных unsafe calls.

Нельзя ослаблять policy всему workspace.

**Целевая схема:**

- unsafe разрешён только в `fparkan-render-vulkan` low-level modules;
- `unsafe_op_in_unsafe_fn = deny`;
- каждый block имеет `// SAFETY:` comment;
- ownership/lifetime rules документированы;
- raw Vulkan handles не выходят в public neutral API;
- custom policy scanner проверяет allowlist.

### S0-H02 — Neutral render IDs не должны быть GPU allocation IDs

**Приоритет:** HIGH, не блокирует первый hardcoded triangle  
**Файл:** `crates/fparkan-render/src/lib.rs`

`GpuMeshId` и `GpuMaterialId` появляются до существования GPU registry. Это смешивает CPU asset identity и backend-local allocation identity.

**Рекомендация:** использовать neutral `MeshAssetId`/`MaterialAssetId`; Vulkan adapter должен самостоятельно отображать их на buffers, images и descriptors.

### S0-M01 — Документация рассогласована с Vulkan revision

**Приоритет:** MEDIUM

`docs/tomes/07-implementation.md` сохраняет старую последовательность и multi-backend формулировки. Parity documentation ссылается на отсутствующий workspace crate, а active parity cases не определены.

**Рекомендация:** один versioned source of truth для stages и автоматическая проверка упомянутых crates, commands и backend names.

---

## 5. Сильные стороны, которые следует сохранить

- Workspace lint policy строгая и подходит для backend-neutral crates.
- `Cargo.lock` присутствует, а команды используют `--locked`.
- Synthetic и licensed corpus paths концептуально разделены.
- `fparkan-headless` не зависит от platform/render adapters на manifest-level.
- `fparkan-render` уже предоставляет deterministic command ordering, validation и canonical capture.
- Composition roots отделены от большинства core crates.

Эти элементы позволяют построить Vulkan foundation без переписывания CPU/data foundation.

---

## 6. Целевая архитектура Stage 0

```text
apps/fparkan-game, apps/fparkan-viewer
                 │
                 ├── fparkan-platform-winit
                 │      └── winit + raw-window-handle
                 │
                 └── fparkan-render-vulkan
                        ├── ash-window
                        ├── ash
                        ├── surface / swapchain
                        ├── device / queues
                        ├── shaders / pipelines
                        └── synchronization / presentation

apps/fparkan-headless
                 └── runtime/core only
                     no winit, ash, MoltenVK or window dependencies
```

Разделение ответственности:

- `fparkan-platform`: события, input, lifecycle, sizes и handle access;
- `fparkan-platform-winit`: concrete window/event-loop implementation;
- `fparkan-render`: backend-neutral command/snapshot contracts;
- `fparkan-render-vulkan`: Vulkan resources, synchronization и present;
- game/viewer: composition root;
- headless: полностью изолированный путь.

---

## 7. План полного закрытия Stage 0

Порядок PR важен. Vulkan adapter не следует строить поверх текущего GL-oriented platform contract.

### PR S0-01 — Reproducible toolchain и metadata

**Изменения**

- закрепить exact Rust toolchain;
- добавить `workspace.package.rust-version`;
- зафиксировать supported triples;
- добавить `cargo xtask doctor`;
- включать commit SHA, Rust version и platform SDK versions в reports.

**Acceptance**

- clean checkout формирует одинаковый metadata report;
- MSRV job собирает backend-neutral crates;
- pinned toolchain проходит полный synthetic gate.

### PR S0-02 — Typed xtask configuration

**Изменения**

- `serde` + TOML schemas для corpus/acceptance manifests;
- `deny_unknown_fields`;
- duplicate/missing/unknown-field validation;
- absolute canonical paths для local licensed manifest;
- `cargo_metadata` для dependency и workspace policy;
- удалить ручной line parser.

**Acceptance**

- malformed manifest всегда даёт non-zero exit;
- неизвестные поля не игнорируются;
- dependency policy работает по Cargo package IDs, targets и features.

### PR S0-03 — Полный synthetic CI gate

Обязательные команды:

```bash
cargo fmt --all -- --check
cargo test --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
RUSTDOCFLAGS="-D warnings -D rustdoc::broken_intra_doc_links" \
  cargo doc --workspace --no-deps --all-features --locked
cargo deny check advisories bans licenses sources
cargo xtask policy
cargo xtask acceptance audit --strict
```

Добавить reports для каждого gate и запрет silent skip.

### PR S0-04 — Redesign `fparkan-platform`

**Изменения**

- удалить `GraphicsProfile`, `GraphicsContextRequest` и GL version negotiation;
- убрать `present()` из window port;
- добавить normalized keyboard/mouse events;
- physical/logical size и scale factor;
- focus, minimize, occlusion, suspend/resume;
- deterministic lifecycle state machine;
- structured errors с source chain.

**Synthetic tests**

- resize coalescing;
- zero-size/minimized window;
- scale-factor changes;
- focus loss clears held input;
- key repeat и modifiers;
- suspend/resume;
- deterministic event ordering.

### PR S0-05 — `fparkan-platform-winit`

**Изменения**

- winit event loop;
- native window lifecycle;
- raw window/display handles;
- platform-specific event normalization;
- отсутствие GPU ownership.

**Acceptance**

- window-only smoke на Windows, Linux и macOS;
- native event trace соответствует synthetic model.

### PR S0-06 — Vulkan low-level boundary

**Изменения**

- `ash` и `ash-window`;
- dynamic Vulkan loader;
- instance и debug messenger;
- physical device capability records;
- pure deterministic device scoring;
- graphics/present queue selection;
- deterministic capability JSON;
- audited unsafe allowlist.

**Negative tests**

- loader отсутствует;
- Vulkan 1.1 недоступен;
- graphics queue отсутствует;
- present queue отсутствует;
- `VK_KHR_swapchain` отсутствует;
- required surface format отсутствует.

### PR S0-07 — Swapchain, triangle и offline shaders

**Изменения**

- surface и swapchain;
- format/present-mode/image-count policy;
- render pass и graphics pipeline;
- indexed triangle;
- command pools/buffers;
- binary semaphores и fences;
- frames in flight;
- resize/out-of-date/suboptimal/zero extent handling;
- pinned offline shader compiler;
- SPIR-V validation;
- descriptor/push-constant manifest;
- shader content hashes.

### PR S0-08 — macOS portability proof

**Изменения**

- `VK_INSTANCE_CREATE_ENUMERATE_PORTABILITY_BIT_KHR`;
- portability extension enumeration;
- `VK_KHR_portability_subset` enablement, если объявлен device;
- MoltenVK packaging strategy;
- deterministic portability report;
- `.app` bundle smoke.

### PR S0-09 — Composition roots и legacy removal

**Изменения**

- game/viewer подключают winit + Vulkan adapters;
- headless остаётся без window/GPU graph;
- удалить SDL/GL stub crates;
- очистить lockfile, policy и docs;
- заменить GPU-named neutral IDs на asset IDs;
- запретить stale backend names automated policy check-ом.

### PR S0-10 — Native acceptance matrix

**Jobs**

- Windows MSVC + system Vulkan loader;
- Linux X11 или Wayland surface;
- macOS Apple Silicon + MoltenVK;
- отдельный software-Vulkan Linux PR job допустим как быстрый gate;
- native GPU jobs остаются release evidence.

**Обязательный сценарий**

1. Создать окно.
2. Создать real Vulkan swapchain.
3. Показать indexed triangle.
4. Выполнить не менее 300 frames.
5. Изменить размер окна.
6. Пересоздать swapchain.
7. Корректно завершить event loop.
8. Получить `validation_error_count = 0`.
9. Сохранить capability, shader и validation reports как artifacts.

**Stage 0 закрывается только после merge S0-01…S0-10 и зелёных native artifacts.**

---

## 8. Требуемая CI/acceptance модель

### 8.1 Synthetic PR gate

Должен работать без игровых каталогов и без silent skip:

1. fmt, clippy, docs, security и policy;
2. все unit/integration tests;
3. platform lifecycle state-machine tests;
4. device scoring tests на synthetic capability records;
5. swapchain policy tests;
6. shader manifest/hash tests;
7. Vulkan negative-path tests без обязательного GPU;
8. headless dependency assertion;
9. report schema validation.

Tests, требующие native GPU или licensed data, должны иметь отдельные suites и machine-readable ownership/reason, а не оставаться обычными `#[ignore]` без evidence trail.

### 8.2 Native platform gate

| Platform | Минимальный gate | Дополнительное evidence |
|---|---|---|
| Windows | system loader, swapchain, triangle, resize, 300 frames, validation=0 | Периодическая NVIDIA/AMD/Intel coverage |
| Linux | X11 или Wayland surface, swapchain, resize, validation=0 | Software Vulkan PR job + Mesa/NVIDIA native release jobs |
| macOS | MoltenVK, portability enumeration/subset, CAMetalLayer surface, resize, validation=0 | Apple Silicon как primary target |

### 8.3 Формат machine-readable отчёта

Минимальные поля:

```json
{
  "schema": 1,
  "commit": "<sha>",
  "target": "x86_64-pc-windows-msvc",
  "rustc": "1.xx.y",
  "vulkan_api": "1.1",
  "device_name": "...",
  "driver": "...",
  "portability_subset": false,
  "frames": 300,
  "resize_count": 1,
  "swapchain_recreate_count": 1,
  "validation_error_count": 0,
  "shader_manifest_hash": "...",
  "result": "pass"
}
```

---

## 9. Definition of Done

Stage 0 считается закрытым, когда выполнены **все** пункты:

- [ ] Exact Rust toolchain закреплён.
- [ ] MSRV объявлен и проверяется.
- [ ] Full fmt/test/clippy/doc/security/source/license gate проходит.
- [ ] Typed TOML manifests используются.
- [ ] Dependency policy работает через `cargo_metadata`.
- [ ] Windows/Linux/macOS matrix сохраняет artifacts.
- [ ] `fparkan-platform` больше не содержит GL-specific context concepts.
- [ ] `fparkan-platform-winit` реализован.
- [ ] `fparkan-render-vulkan` реализован.
- [ ] Vulkan 1.1 instance/device/queues/surface/swapchain реализованы.
- [ ] Deterministic device scoring и capability report реализованы.
- [ ] Indexed triangle рисуется настоящим Vulkan backend.
- [ ] Resize, zero extent, out-of-date и suboptimal обработаны.
- [ ] MoltenVK portability path реализован.
- [ ] Offline SPIR-V validation и hash manifest реализованы.
- [ ] Unsafe разрешён только в audited Vulkan/FFI modules.
- [ ] Legacy SDL/GL adapters и references удалены.
- [ ] Game/viewer используют новые composition adapters.
- [ ] Headless dependency graph не содержит winit/Vulkan/MoltenVK.
- [ ] 300-frame + resize smoke проходит на трёх OS.
- [ ] Validation error count равен нулю на трёх OS.
- [ ] Acceptance reports включают commit SHA и сохраняются как artifacts.

Наличие crates или unit tests с соответствующими названиями само по себе не является закрытием Stage 0.

---

## 10. Рекомендуемые automated policy checks

Добавить в `cargo xtask policy`:

### Workspace denylist

- запрещены `fparkan-platform-sdl` и `fparkan-render-gl` после миграции;
- запрещены stale symbols `GraphicsProfile`, `DesktopCore`, `Embedded`, `Gles2` в canonical platform/render API;
- canonical docs не содержат OpenGL как production backend.

### Dependency rules

- headless не зависит от `winit`, `raw-window-handle`, `ash`, `ash-window` или Vulkan adapter;
- backend-neutral crates не зависят от concrete platform/render adapters;
- только composition roots связывают platform и renderer;
- raw Vulkan types не экспортируются из adapter public boundary.

### Unsafe rules

- project-owned unsafe разрешён только в exact allowlisted files/modules;
- каждый block содержит `SAFETY:`;
- `unsafe_op_in_unsafe_fn` запрещён;
- изменение allowlist требует отдельного reviewed diff.

### Test и report rules

- synthetic gate не получает licensed paths;
- ignored tests обязаны иметь registered reason и owner;
- acceptance IDs уникальны;
- reports проходят schema validation;
- report всегда содержит commit SHA и target triple.

### Documentation rules

- документированные crates и commands существуют;
- canonical stage version совпадает с acceptance schema;
- старые backend names отсутствуют;
- README не объявляет незакрытый Vulkan path реализованным.

---

## 11. Основные риски

| Риск | Последствие | Снижение |
|---|---|---|
| Vulkan adapter начнут до redesign platform API | Повторная переделка surface/lifecycle/present | Сначала S0-04, затем S0-05/S0-06 |
| `unsafe_code` ослабят всему workspace | Рост FFI и lifetime рисков | Изолированный audited adapter и allowlist scanner |
| Stubs будут приняты за production backend | Ложное закрытие Stage 0 | Удаление legacy crates и real native smoke |
| Linux software Vulkan будет единственным evidence | Не выявятся vendor-driver проблемы | Native Mesa/NVIDIA jobs перед release |
| macOS будет проверен без portability subset report | Скрытая несовместимость MoltenVK | Обязательное capability evidence |
| Shader compiler останется неприкреплённым | Невоспроизводимый SPIR-V | Pinned compiler + manifest hashes |
| GitHub mirror и primary repository разойдутся | Audit и release относятся к разному коду | Commit SHA, canonical remote и artifact metadata |
| Документация останется отдельным source of truth | Повторное рассогласование | Versioned stage schema и automated doc checks |

---

## 12. Реестр доказательств

### Canonical requirement

- Vulkan revision: <https://app.notion.com/p/387e79f2db3981778f94cdf34db5f93f>

### Workspace и governance

- Root manifest: <https://github.com/valentineus/fparkan/blob/devel/Cargo.toml>
- Toolchain: <https://github.com/valentineus/fparkan/blob/devel/rust-toolchain.toml>
- Cargo config: <https://github.com/valentineus/fparkan/blob/devel/.cargo/config.toml>
- xtask manifest: <https://github.com/valentineus/fparkan/blob/devel/xtask/Cargo.toml>
- xtask implementation: <https://github.com/valentineus/fparkan/blob/devel/xtask/src/main.rs>
- README: <https://github.com/valentineus/fparkan/blob/devel/README.md>

### Platform и render

- Platform core: <https://github.com/valentineus/fparkan/blob/devel/crates/fparkan-platform/src/lib.rs>
- SDL stub adapter: <https://github.com/valentineus/fparkan/blob/devel/adapters/fparkan-platform-sdl/src/lib.rs>
- Render core: <https://github.com/valentineus/fparkan/blob/devel/crates/fparkan-render/src/lib.rs>
- GL stub adapter: <https://github.com/valentineus/fparkan/blob/devel/adapters/fparkan-render-gl/src/lib.rs>
- Game composition: <https://github.com/valentineus/fparkan/blob/devel/apps/fparkan-game/src/main.rs>
- Viewer composition: <https://github.com/valentineus/fparkan/blob/devel/apps/fparkan-viewer/src/main.rs>
- Headless manifest: <https://github.com/valentineus/fparkan/blob/devel/apps/fparkan-headless/Cargo.toml>

### Documentation drift

- Implementation tome: <https://github.com/valentineus/fparkan/blob/devel/docs/tomes/07-implementation.md>
- Parity README: <https://github.com/valentineus/fparkan/blob/devel/parity/README.md>
- Parity cases: <https://github.com/valentineus/fparkan/blob/devel/parity/cases.toml>

---

## 13. Финальное заключение

У проекта уже имеется пригодный backend-neutral фундамент: deterministic render commands, строгие neutral-crate lints, отдельный headless composition root и разделение synthetic/licensed tests. Однако Stage 0 пока представлен интерфейсными proof/stub crates, а не настоящим Vulkan vertical slice.

Критический путь:

```text
reproducible toolchain
  → complete CI/policy gate
  → backend-neutral platform redesign
  → winit adapter
  → Vulkan loader/device/surface/swapchain
  → indexed triangle + shaders + synchronization
  → MoltenVK portability
  → composition integration
  → legacy removal
  → three-platform acceptance artifacts
```

До прохождения этого пути рекомендуемый статус:

```text
Stage 0: IN PROGRESS / BLOCKED
```

Главный критерий закрытия:

> Stage 0 завершён не тогда, когда существуют crates с названиями `winit` и `vulkan`, а когда один закреплённый commit создаёт настоящий Vulkan swapchain, рисует triangle, переживает resize и завершается без validation errors на Windows, Linux и macOS, сохраняя воспроизводимые machine-readable artifacts.
