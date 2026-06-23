# FParkan — аудит Stage 0–2 и план полного закрытия

**Проект:** `valentineus/fparkan`  
**Проверенная ветка:** `devel` GitHub-зеркала  
**Дата аудита:** 23 июня 2026 года  
**Область:** Stage 0, Stage 1 и Stage 2 из документа «План реализации stage 0–5: Vulkan revision»  
**Метод:** статический архитектурный и кодовый аудит  
**Сборка и исполнение:** не выполнялись; `cargo build`, `cargo test`, Vulkan smoke и licensed corpus jobs не запускались

---

## 1. Итоговый вердикт

На проверенном состоянии ветки `devel` **ни один из Stage 0–2 нельзя считать закрытым**.

| Stage | Статус | Закрытие exit gate | Главная причина |
|---|---|---:|---|
| Stage 0 — governance и Vulkan foundation | **BLOCKED** | 0 из 3 обязательных результатов подтверждены | Нет `winit`/Vulkan adapters и реального swapchain; workspace всё ещё содержит SDL/OpenGL stubs; CI gate неполон |
| Stage 1 — paths, VFS и archives | **PARTIAL, NOT CLOSED** | 0 из 3 результатов подтверждены | Нет общего allocation budget во всех parsers/decompressors, полноценного RsLi edit/writer path, сквозных diagnostics и production-parser corpus gate |
| Stage 2 — prototype graph и CPU assets | **BLOCKED BY ARCHITECTURE** | 0 из 3 результатов подтверждены | `fparkan-assets` не является единственным preparation layer; runtime и apps парсят данные напрямую; graph не хранит полноценные typed nodes/edges/provenance |

Положительная сторона: проект уже имеет хороший фундамент — bounded cursor, NRes editor с preserved regions, generation handles, deterministic caches, строгую workspace lint policy, typed mission/model primitives, deterministic render command capture и частичное раскрытие unit/prototype/material/texture зависимостей. Эти наработки следует сохранить. Основная работа — не переписывание всего проекта, а **устранение архитектурных обходов, доведение safety contracts и добавление доказуемых acceptance gates**.

### Решение по выпуску этапов

- **Stage 0 нельзя закрывать декларативно.** Он закрывается только артефактами трёх нативных платформ: реальный Vulkan swapchain, triangle, resize, 300 frames и нулевые validation errors.
- **Stage 1 нельзя закрывать только unit tests.** Нужны стабильные отчёты и byte-identical roundtrip на лицензированных каталогах обеих частей игры.
- **Stage 2 нельзя закрывать текущими count-only reports.** Требуется материализованный dependency graph, typed provenance каждого edge и обязательный путь `runtime → AssetManager → immutable CPU assets`.

---

## 2. Основание аудита

Канонические критерии взяты из страницы Notion:

- «План реализации stage 0–5: Vulkan revision»;
- page ID: `387e79f2-db39-8177-8f94-cdf34db5f93f`;
- URL: <https://app.notion.com/p/387e79f2db3981778f94cdf34db5f93f>.

Проверялась ветка `devel` GitHub-зеркала:

- <https://github.com/valentineus/fparkan/tree/devel>

Ключевые проверенные файлы перечислены в разделе «Реестр доказательств».

### Ограничения вывода

1. Ветка `devel` является движущейся ссылкой; connected GitHub mirror не предоставил надёжный SHA текущего tip. Для повторяемости следующего formal audit следует запускать на закреплённом commit SHA/tag.
2. README указывает на self-hosted primary repository. История его Gitea runners и закрытые CI artifacts в рамках этого аудита не были доступны.
3. Licensed Part 1/Part 2 corpus не запускался. Поэтому любые утверждения о «нулевых failures», точных reachability counts и roundtrip являются **неподтверждёнными**, даже если соответствующие тестовые функции существуют в коде.
4. Vulkan runtime не запускался и validation layers не проверялись.
5. Отсутствие динамической проверки не мешает определить архитектурные blockers: manifests и composition roots однозначно показывают, что реального Vulkan vertical slice сейчас нет.

---

## 3. Шкала статусов и приоритетов

### Статус требования

- **PASS** — реализация и статическое доказательство соответствуют требованию; динамический gate всё равно может оставаться непроверенным.
- **PARTIAL** — существенная часть присутствует, но контракт или acceptance evidence неполны.
- **FAIL** — требуемой реализации нет либо текущая архитектура ей противоречит.
- **UNVERIFIED** — реализация может существовать, но нет доступного воспроизводимого доказательства.

### Приоритет замечаний

- **BLOCKER** — без исправления Stage невозможно закрыть.
- **HIGH** — риск повреждения данных, unbounded resource use, неправильного dependency graph или ложноположительного acceptance.
- **MEDIUM** — архитектурный долг, диагностическая неполнота или слабая тестируемость.
- **LOW** — документация, ergonomics или cleanup, не являющиеся самостоятельным блокером.

---

# 4. Stage 0 — Governance, reproducibility и Vulkan foundation

## 4.1 Матрица требований

| Требование Stage 0 | Статус | Доказательство текущего состояния | Что требуется для закрытия |
|---|---|---|---|
| Exact stable Rust toolchain и MSRV | **FAIL** | `rust-toolchain.toml` содержит только `channel = "stable"`; `workspace.package.rust-version` отсутствует | Закрепить точный toolchain, добавить MSRV и отдельный MSRV job |
| Полный `cargo xtask ci` | **FAIL** | Сейчас выполняются custom rustfmt, policy, `cargo test --workspace --locked --offline` и clippy без полного набора flags; отсутствует обязательный doc gate | Реализовать канонический fmt/test/clippy/doc/security/source/license pipeline |
| `--all-targets --all-features`, `-D warnings` | **FAIL** | В inspected `xtask` эти параметры отсутствуют | Добавить дословно и проверить negative tests самого xtask |
| License/advisory/source policy | **PARTIAL/UNVERIFIED** | Есть custom policy и workspace license, но нет доказанного advisory/source gate и formal allowlist | Подключить `cargo-deny` или эквивалент с versioned policy; выгружать report artifact |
| Typed TOML parsing и `cargo_metadata` | **FAIL** | Licensed manifest разбирается ручным line parser; `xtask/Cargo.toml` не зависит от TOML parser или `cargo_metadata` | Ввести serde-backed schema, deny unknown fields, canonical path validation, `cargo_metadata` |
| CI matrix Windows/Linux/macOS | **UNVERIFIED, EXIT BLOCKER** | Канонический audit сам отмечает отсутствие подтверждённой hosted matrix; доступных run artifacts нет | Создать matrix и platform-native acceptance jobs с сохраняемыми reports |
| `fparkan-platform-winit` | **FAIL** | В workspace присутствует `fparkan-platform-sdl`; он содержит in-memory stubs и не зависит от SDL. `winit` adapter отсутствует | Новый adapter с event loop, lifecycle, DPI, input, handles, suspend/resume, resize |
| Backend-neutral platform contract | **FAIL** | Core port экспортирует `GraphicsProfile::DesktopCore/Embedded` и `GraphicsContextRequest`, то есть OpenGL concepts; `WindowPort::present()` смешивает window и GPU presentation | Удалить GL concepts; present перенести в render backend; ввести normalized lifecycle/input API |
| `fparkan-render-vulkan` | **FAIL** | Workspace содержит `fparkan-render-gl`; crate только формирует canonical text capture и не вызывает GPU API | Создать Vulkan adapter с instance/device/surface/swapchain/pipeline/sync |
| Реальный Vulkan triangle | **FAIL** | Ни `ash`, ни `ash-window`, ни `winit` не подключены; rendered apps используют `RecordingBackend` | Реальный indexed triangle в отдельном smoke app и composition roots |
| Device scoring и capability report | **FAIL** | Реального device discovery нет | Pure policy module + Vulkan enumeration + deterministic JSON report |
| Swapchain recreation | **FAIL** | Surface/swapchain отсутствуют | Обработать resize, zero extent, out-of-date, suboptimal, minimized/suspended states |
| macOS portability enumeration/subset | **FAIL** | MoltenVK/Vulkan adapter отсутствуют | Instance portability flag, extension enumeration, subset feature report, packaged MoltenVK |
| Offline SPIR-V build/validation | **FAIL** | Shader manifest/hash pipeline не найден; GL stub проверяет только empty/`#error` markers | Versioned shader sources, compiler pin, SPIR-V validator, descriptor manifest, embedded hashes |
| Удаление прежних adapters/references | **FAIL** | Root workspace явно включает SDL и GL adapters; docs также содержат старую multi-backend формулировку | Удалить crates, lockfile refs, policy exceptions, docs и stale tests |
| Headless без window/GPU deps | **PASS на manifest-level** | `fparkan-headless` зависит от runtime/VFS/world и не содержит adapter dependency | Добавить automated `cargo tree`/metadata assertion и target build gate |
| 300-frame smoke + resize + validation | **FAIL/NOT RUNNABLE** | Нет реального backend | Добавить platform jobs и machine-readable validation report |
| Negative loader/device/present/format tests | **FAIL** | Нет Vulkan error model | Dependency-injected policy layer + loader/device/surface failure fixtures |

## 4.2 Критические замечания Stage 0

### S0-B01 — Workspace всё ещё построен вокруг удаляемых SDL/OpenGL stub crates

**Приоритет:** BLOCKER  
**Файлы:** `Cargo.toml`, `adapters/fparkan-platform-sdl`, `adapters/fparkan-render-gl`

Root workspace включает оба прежних adapter crate. При этом:

- SDL adapter не содержит зависимости на SDL и является deterministic stub;
- GL adapter не содержит OpenGL binding и только сохраняет command captures;
- tests этих crates доказывают поведение stubs, а не platform/GPU integration.

Это создаёт опасный ложноположительный сигнал: названия adapters выглядят как реализованные backends, но фактически проверяется только модель интерфейса.

**Рекомендация:** удалить legacy adapters только после появления `platform-winit` и `render-vulkan`, но до формального Stage 0 acceptance. На время миграции пометить их `legacy-proof`, исключить из default members и запретить composition roots ссылаться на них.

### S0-B02 — Core platform API содержит OpenGL-specific contract

**Приоритет:** BLOCKER  
**Файл:** `crates/fparkan-platform/src/lib.rs`

Проблемы:

- `GraphicsProfile` и `GraphicsContextRequest` описывают GL/GLES context, которого в Vulkan architecture нет;
- `WindowPort::present()` неверно закрепляет presentation за window abstraction. В Vulkan presentation зависит от swapchain, queue, acquired image и semaphores и должен принадлежать render adapter;
- `PlatformEvent` содержит только `Quit`;
- отсутствуют DPI, resize, occlusion/minimize, focus, keyboard/mouse, lifecycle, suspend/resume и raw handles;
- `PlatformError::Backend` не предоставляет context/source.

**Целевой контракт:** platform crate сообщает события и предоставляет handle/size; render adapter владеет surface/swapchain/present.

### S0-B03 — Нет реального Vulkan code path

**Приоритет:** BLOCKER

Ни один inspected manifest не подключает `ash`, `ash-window`, `winit` или `raw-window-handle`. `fparkan-game` использует `RecordingBackend`; `fparkan-viewer` является CLI inspector. Следовательно, instance/device/surface/swapchain не могут быть созданы текущим кодом.

**Definition of fixed:** в workspace существует отдельный `fparkan-render-vulkan`; smoke executable проходит 300 frames, resize и clean shutdown на Windows, Linux и macOS/MoltenVK.

### S0-B04 — CI command не соответствует каноническому gate

**Приоритет:** BLOCKER  
**Файл:** `xtask/src/main.rs`

Текущий `ci` не подтверждает:

- all targets/features;
- `-D warnings` для clippy;
- rustdoc broken-link denial;
- advisory/source policy;
- platform adapter denylist;
- отсутствие project-owned unsafe вне allowlist;
- корректность самого acceptance manifest parser.

Отдельный риск: custom recursion для rustfmt может расходиться с `cargo fmt --all -- --check` и форматировать/пропускать файлы иначе, чем Cargo workspace.

### S0-B05 — Toolchain не воспроизводим

**Приоритет:** BLOCKER  
**Файл:** `rust-toolchain.toml`

`stable` меняется со временем. Один и тот же commit может пройти сегодня и не пройти после следующего stable release. Также не указан `rust-version`, поэтому минимально поддерживаемый компилятор не является контрактом.

**Рекомендация:** точный channel вида `1.xx.y`, components и targets; `rust-version` в workspace package; controlled update PR с changelog и full matrix.

### S0-H01 — Глобальный `unsafe_code = forbid` требует заранее спроектированного FFI boundary

**Приоритет:** HIGH

Полный запрет полезен для neutral crates, но raw Vulkan bindings неизбежно требуют узких unsafe calls. Нельзя ослаблять lint всему workspace.

**Целевая схема:** отдельный low-level adapter crate или модуль, который:

- не наследует blanket `forbid`;
- устанавливает `deny(unsafe_op_in_unsafe_fn)`;
- разрешает unsafe только в одном/нескольких audited modules;
- требует `// SAFETY:` comment;
- не экспортирует raw handles;
- проходит custom policy scanner.

### S0-H02 — Render command model пока недостаточен для последующих Vulkan assets

**Приоритет:** HIGH, не блокирует самый первый hardcoded triangle

`fparkan-render` имеет хорошую deterministic command/capture основу, но neutral API использует `GpuMeshId`/`GpuMaterialId` ещё до существования GPU resource registry. Для Stage 2/3 это смешивает CPU asset identity и backend allocation identity.

**Рекомендация:** neutral layer должен оперировать `MeshAssetId`, `MaterialAssetId`, immutable draw items и legacy pipeline state. Vulkan adapter локально сопоставляет их с buffers/images/descriptors.

### S0-M01 — Documentation drift

**Приоритет:** MEDIUM

`docs/tomes/07-implementation.md` всё ещё описывает старую последовательность этапов и допускает Vulkan/D3D11/Metal backend wording. `parity/README.md` ссылается на `crates/render-parity`, которого нет среди workspace members, а `parity/cases.toml` не содержит активных cases.

Документация должна иметь один canonical stage source либо автоматически генерируемую проверку согласованности.

## 4.3 Положительные элементы Stage 0

- `Cargo.lock` и `--locked` упомянуты и используются в текущем workflow.
- Workspace lint policy достаточно строгая для neutral crates.
- Synthetic и licensed test paths концептуально разделены.
- `fparkan-headless` не зависит от platform/render adapters на manifest-level.
- `fparkan-render` уже предоставляет deterministic command ordering, validation и capture — это можно использовать как pre-GPU oracle.

## 4.4 Обязательные изменения для полного закрытия Stage 0

1. Закрепить toolchain/MSRV.
2. Переписать xtask configuration на typed TOML + `cargo_metadata`.
3. Завершить CI/security/doc gates.
4. Пересмотреть platform port и удалить GL-specific types.
5. Реализовать `fparkan-platform-winit`.
6. Реализовать `fparkan-render-vulkan` с узким unsafe boundary.
7. Добавить offline SPIR-V pipeline.
8. Подключить adapters к game/viewer composition roots.
9. Удалить legacy SDL/GL crates и stale docs.
10. Запустить и сохранить acceptance artifacts на трёх OS.

---

# 5. Stage 1 — Paths, VFS и archives

## 5.1 Матрица требований

| Требование Stage 1 | Статус | Доказательство текущего состояния | Что требуется для закрытия |
|---|---|---|---|
| Raw legacy bytes + normalized + ASCII key + host path | **PARTIAL** | Есть `OriginalPathBytes`, `NormalizedPath`, ASCII lookup и policies; нормализация сначала требует UTF-8 | Сделать byte-first identity; decoding только для diagnostics; определить OS host conversion |
| Strict/compatible path policies | **PARTIAL/PASS** | Два режима есть и тестируются | Формализовать различия и применить одинаково во всех callers |
| Casefold collision policy | **PARTIAL** | Directory/Memory VFS обнаруживают ambiguity; Overlay имеет deterministic precedence | Добавить segment-boundary tests и общий collision report contract |
| Symlink-safe traversal | **PARTIAL/HIGH RISK** | Symlinks проверяются через `symlink_metadata`; однако check/open разделены | Capability-based/openat traversal, root confinement, cycle/escape tests |
| Common `DecodeLimits` | **PARTIAL** | `fparkan-binary::Limits` существует, но многие parsers используют собственные constants или API без limits | Единый `DecodeContext` во всех parsers |
| Common cumulative `AllocationBudget` | **FAIL** | Stateful budget не найден | Reservation/refund model для allocations и decompression output |
| NRes lossless reader/editor/writer | **NEAR PASS, EXIT UNVERIFIED** | Есть lossless/canonical profiles, editor, preserved regions | Подключить limits/diagnostics; corpus roundtrip на обеих частях |
| RsLi all observed decode methods | **PARTIAL** | Enum и implementations покрывают stored/XOR/LZSS/adaptive/deflate variants | Corpus proof, method coverage table, bounded output |
| RsLi explicit compatibility profile | **PARTIAL** | AO, EOF+1 и invalid presort toggles есть | Versioned quirk registry с evidence IDs и per-file activation trace |
| RsLi lossless writer/edit model | **FAIL** | `WriteProfile` содержит только возврат исходного image; editor/repack отсутствуют | Editable document, preserved unknowns, deterministic table rebuild, no-edit identity |
| Structured diagnostics end-to-end | **FAIL** | Отдельный diagnostics crate существует, но NRes/RsLi/VFS/resource/prototype/runtime его не используют | Единый typed diagnostic envelope, source chain и offsets |
| Generation handles | **PASS** | Resource repository хранит generation и выявляет stale handles | Сохранить и добавить concurrency/property tests |
| Decoded byte cache budget | **PARTIAL/PASS** | Есть entry+byte limits и deterministic map | Отделить cache budget от decode allocation budget; test oversize rejection before allocation |
| Decompression outside lock | **PASS статически** | Resource repository формирует task под lock, выполняет payload decode после release | Добавить concurrency regression test |
| Typed resource errors | **PARTIAL/FAIL** | Верхний enum typed, но format/source часто превращаются в `String` | Сохранить concrete source errors и classify missing/archive/entry/corruption |
| Corpus report использует production parsers | **FAIL** | Реально вызывается NRes parser; RsLi только определяется по magic, TMA/Land/unit metrics — по имени пути | Интегрировать все production parsers и считать parser errors failures |
| Stable licensed reports и roundtrip | **UNVERIFIED, EXIT BLOCKER** | Нет запусков и artifacts в доступном аудите | Separate licensed jobs, signed manifests, report diff policy |

## 5.2 Критические замечания Stage 1

### S1-B01 — `Limits` не является обязательным contract всех decoders

**Приоритет:** BLOCKER/HIGH  
**Файлы:** `fparkan-binary`, `fparkan-nres`, `fparkan-rsli`, `fparkan-mission-format`, другие format crates

`fparkan-binary::Limits` определён, но:

- NRes decode API не принимает limits/budget;
- RsLi load/decompression не принимает общий output budget;
- mission parser использует собственный набор `MAX_*` constants;
- нет cumulative budget, отслеживающего сумму вложенных allocations;
- cache byte limit действует после decode и не предотвращает decompression bomb.

Проверка отдельного count недостаточна. Несколько допустимых массивов могут вместе превысить memory budget, а declared decompressed size может привести к крупному выделению до cache rejection.

**Целевой API:**

```rust
pub struct DecodeContext<'a> {
    pub limits: &'a DecodeLimits,
    pub budget: &'a AllocationBudget,
    pub diagnostics: &'a dyn DiagnosticSink,
}
```

Каждый allocation предваряется `reserve(bytes, category, span)`; nested decoders наследуют тот же budget.

### S1-B02 — RsLi не имеет требуемого edit/writer model

**Приоритет:** BLOCKER  
**Файл:** `crates/fparkan-rsli/src/lib.rs`

Текущий lossless write profile фактически возвращает исходный byte image. Это полезный no-edit roundtrip, но не является edit model. Stage 1 требует:

- редактирование entry metadata/payload;
- сохранение unknown/overlay regions;
- rebuild lookup table;
- корректный packing method policy;
- byte-identical no-edit;
- deterministic edited output.

Нужно разделить `OriginalImage`, parsed table, preserved segments и editable entries так же явно, как это уже сделано для NRes.

### S1-B03 — Corpus report создаёт ложное впечатление production coverage

**Приоритет:** BLOCKER  
**Файлы:** `crates/fparkan-corpus/Cargo.toml`, `src/lib.rs`

Crate не зависит от `fparkan-rsli`, mission, terrain, prototype и прочих production parsers. В inspected implementation:

- NRes действительно декодируется;
- RsLi только распознаётся по magic;
- TMA, Land и unit DAT считаются по extension/path patterns;
- parser failure count поэтому не отражает значительную часть corpus.

Фраза «corpus report использует реальные parsers» сейчас верна лишь частично. Exit gate «нулевые необъяснённые failures» нельзя доказать этим report.

### S1-B04 — Structured diagnostics существуют отдельно от error path

**Приоритет:** BLOCKER/HIGH

`fparkan-diagnostics` имеет severity, phase, path, archive entry, object key, span и causes, однако format/resource/runtime crates не зависят от него. Вместо cause chain многие errors преобразуются в строки:

- `ResourceError::Format(String)`;
- `ResourceError::EntryRead { source: String }`;
- `PrototypeError::Resource(String)`;
- `AssetError::* (String)`.

После преобразования невозможно надёжно классифицировать missing vs corrupt, извлечь source offset или сохранить concrete error chain.

### S1-H01 — JSON serializer diagnostics некорректно обрабатывает часть control characters

**Приоритет:** HIGH  
**Файл:** `crates/fparkan-diagnostics/src/lib.rs`

Manual JSON escaping обрабатывает quote, backslash, `\n`, `\r`, `\t`, но не все символы U+0000–U+001F. Сообщение или path с backspace/form-feed/NUL может породить невалидный JSON.

**Рекомендация:** использовать `serde`/`serde_json` для wire format; добавить property tests для всех Unicode/control characters и deterministic field ordering через typed serializable schema.

### S1-H02 — Path identity остаётся UTF-8-first

**Приоритет:** HIGH  
**Файл:** `crates/fparkan-path/src/lib.rs`

Legacy data и host filenames могут содержать CP1251 или произвольные byte sequences. `normalize_relative()` сначала вызывает UTF-8 decode и отклоняет invalid UTF-8. Наличие `OriginalPathBytes` не решает проблему, потому что объект создаётся только после успешной UTF-8 normalization.

**Целевая модель:** canonical legacy identity — bytes; separators/`.`/`..`/drive checks выполняются на ASCII bytes; decoded display name — отдельное необязательное поле.

### S1-H03 — VFS symlink checks подвержены check/use race

**Приоритет:** HIGH  
**Файл:** `crates/fparkan-vfs/src/lib.rs`

Код проверяет `symlink_metadata`, затем открывает/read-ит pathname отдельной операцией. Между проверкой и open entry может быть заменён symlink. Для локального trusted corpus риск невысок, но Stage 1 заявляет безопасный substrate и malformed/adversarial tests.

**Рекомендация:** directory capability/openat traversal, no-follow handles, проверка final object через handle metadata. `follow_symlinks=true` в corpus discovery должен иметь root-confinement и visited-file-id set.

### S1-H04 — Fingerprint cache может пропустить content replacement

**Приоритет:** HIGH/MEDIUM

DirectoryVfs повторно использует SHA по `(path, len, modified)`. На filesystem с грубой timestamp resolution файл можно изменить, сохранив длину и mtime. Generation invalidation тогда не сработает.

Для correctness-critical licensed reports рекомендуются:

- unconditional SHA в audit mode;
- либо file identity + ctime/change counter;
- явное разделение fast interactive cache и strict verification mode.

### S1-M01 — Prefix semantics MemoryVfs требуют segment boundary

**Приоритет:** MEDIUM

`list(prefix)` использует byte prefix. `DATA/FOO` может захватить `DATA/FOOBAR`. Нужна семантика `path == prefix || path starts with prefix + '/'` и одинаковые tests для всех VFS implementations.

### S1-M02 — Собственная SHA-256 реализация увеличивает maintenance surface

**Приоритет:** MEDIUM

Криптографическая новизна проекту не нужна. Если custom implementation сохраняется ради zero dependencies/offline build, она должна иметь exhaustive standard vectors, differential tests и fuzzing. Иначе безопаснее использовать широко проверенный crate и закреплённую версию.

## 5.3 Положительные элементы Stage 1

- Bounded little-endian cursor и checked arithmetic уже централизованы.
- NRes имеет strict/compatible read profiles, editor и preserved regions.
- RsLi содержит явные compatibility switches и широкий набор методов decode.
- VFS выявляет ASCII-casefold ambiguity и отвергает symlink entries в обычном path.
- Resource handles имеют generation и stale detection.
- Payload decode выполняется вне repository mutex.
- Cache имеет entry/byte limits и deterministic data structure.
- Corpus manifest использует SHA-256 и sorted traversal.

## 5.4 Обязательные изменения для полного закрытия Stage 1

1. Ввести обязательные `DecodeLimits` + cumulative `AllocationBudget`.
2. Перевести все parsers/decompressors на context API.
3. Сделать path model byte-first.
4. Усилить VFS до handle/capability-based root confinement.
5. Интегрировать structured diagnostics и typed source errors.
6. Исправить JSON serialization.
7. Завершить RsLi editor/writer.
8. Подключить production parsers к corpus report.
9. Добавить strict verification mode fingerprints.
10. Зафиксировать licensed Part 1/2 reports и roundtrip artifacts.

---

# 6. Stage 2 — Prototype graph и CPU assets

## 6.1 Матрица требований

| Требование Stage 2 | Статус | Доказательство текущего состояния | Что требуется для закрытия |
|---|---|---|---|
| `objects.rlb` decode | **PARTIAL** | Есть 64-byte record decoder и registry resolution | Corpus variants, lossless model, typed provenance и failure spans |
| Unit DAT decode | **PARTIAL** | Есть component records и binding variant | Формальная variant discrimination, all observed records, hierarchy semantics |
| Inheritance/depth handling | **PARTIAL/UNVERIFIED** | Есть depth-limit constant и resolution code | Cycle path, parent edge nodes, BASE/resource variants, corpus proof |
| Все unit components | **PARTIAL** | Internal graph expansion итерирует records; public `resolve_prototype` всё ещё возвращает только first component | Удалить/ограничить lossy API; graph хранит каждый component и hierarchy |
| Typed graph nodes/edges | **FAIL** | `PrototypeGraph` хранит только roots и flattened prototype requests | Materialized graph arena с stable node IDs и typed edge instances |
| Typed provenance каждого edge | **FAIL** | Есть enum kind и count report, но нет parent chain/edge instance/source span | `Provenance` с mission object, component index, archive/entry/span, parent edge |
| Effect edge | **FAIL** | `PrototypeGraphEdge` не содержит effect path; `fparkan-assets` не зависит от FX crate | Добавить typed effect assets и graph reachability |
| BASE/resource variants | **UNVERIFIED/LIKELY PARTIAL** | В доступных contracts нет полноценной variant model/provenance | Явный enum variant, evidence-backed parser, fixtures |
| `fparkan-assets` — единственный preparation layer | **FAIL** | Prototype crate сам зависит от MSH/material/Texm; runtime напрямую вызывает format/prototype parsers; viewer парсит напрямую | Перестроить dependency DAG и запретить parser deps в apps/runtime |
| Immutable prepared CPU assets | **FAIL/PARTIAL** | `PreparedVisual` в основном содержит keys/counts, а не mesh/material/texture data | Immutable `MeshAsset`, `MaterialAsset`, `TextureAsset`, `MissionAssets` |
| Stable IDs | **PARTIAL/HIGH RISK** | 64-bit FNV-like hash от geometry key без collision registry | Canonical key interner/content hash + collision detection + schema version |
| Structured graph failure parent chain | **FAIL** | Failure содержит root index, edge enum и message string | Full chain и concrete typed cause, not string |
| Optional fallback vs corruption | **FAIL/PARTIAL** | Есть optional read helper, но classification не является graph-wide contract | Requiredness enum + severity policy + explicit fallback provenance |
| No ad-hoc parsing in apps/runtime | **FAIL** | Runtime зависит от NRes/mission/terrain/prototype; viewer вызывает decoders напрямую | Только AssetManager/mission loader ports; lint dependency denylist |
| Deterministic graph order/IDs | **PARTIAL/UNVERIFIED** | Некоторые sorted/BTree structures и stable hasher есть | Canonical traversal spec, golden graph serialization, cross-run/cross-OS tests |
| Licensed Part 1/2 zero-failure reachability | **UNVERIFIED, EXIT BLOCKER** | Нет доступных run artifacts; inspected game corpus test помечен `#[ignore]` | Full mission matrix reports, expected counts, zero unexplained failures |

## 6.2 Критические замечания Stage 2

### S2-B01 — `fparkan-prototype` и `fparkan-assets` нарушают целевое разделение ответственности

**Приоритет:** BLOCKER  
**Файлы:** `crates/fparkan-prototype/Cargo.toml`, `crates/fparkan-assets/Cargo.toml`

Prototype crate зависит от:

- material;
- MSH;
- NRes;
- Texm;
- resource/VFS.

И сам расширяет graph report visual dependencies. Затем `fparkan-assets` повторно декодирует MSH, WEAR, MAT0 и Texm. Возникают два preparation paths, которые со временем неизбежно разойдутся по fallback, diagnostics и budgets.

**Целевой DAG:**

```text
mission/prototype formats ──> prototype graph (keys + provenance only)
                                      │
                                      v
resource repository ─────────> fparkan-assets (all CPU decoding/preparation)
                                      │
                                      v
runtime/world ────────────────> immutable MissionAssets
                                      │
                                      v
render bridge ────────────────> backend-neutral draw items
```

Prototype layer не должен декодировать render assets.

### S2-B02 — Runtime не использует `fparkan-assets`

**Приоритет:** BLOCKER  
**Файлы:** `crates/fparkan-runtime/Cargo.toml`, `src/lib.rs`

Runtime manifest не содержит dependency на `fparkan-assets`, зато напрямую зависит от NRes, mission-format, prototype, terrain-format и resource. `LoadedMissionState` хранит `Vec<EffectivePrototype>`, а не prepared immutable assets.

Это прямо противоречит exit gate: «runtime получает prepared assets только через asset manager».

### S2-B03 — Apps продолжают ad-hoc parsing и synthetic resource mapping

**Приоритет:** BLOCKER

- Viewer напрямую вызывает NRes/MSH/Texm/terrain decoders.
- Game создаёт `GpuMeshId(slot + 1)`, constant material ID и triangle range, вместо prepared mission assets.
- Ни game, ни viewer не подключают platform/Vulkan adapters.

Apps должны быть composition roots, а не дополнительным parser/service layer.

### S2-B04 — `PrototypeGraph` не является materialized dependency graph

**Приоритет:** BLOCKER  
**Файл:** `crates/fparkan-prototype/src/lib.rs`

Текущая структура содержит только:

- roots;
- flattened prototype requests.

Report содержит counts и failures, но не даёт:

- stable node identity;
- конкретные edge instances;
- parent/child traversal;
- deduplication semantics;
- source spans;
- full provenance chain;
- serialization для golden comparison.

Для Stage 2 нужен arena/adjacency graph, а report должен вычисляться из него, а не заменять его.

### S2-B05 — Публичный resolver теряет multi-component unit

**Приоритет:** BLOCKER/HIGH

`resolve_prototype()` для DAT вызывает helper, возвращающий первый resolved component. Хотя другой internal path обходит все records, наличие публичного lossy API нарушает invariant Stage 2 и создаёт риск использования «первого visual» в новом caller.

**Рекомендация:** удалить этот API либо переименовать в явно lossy diagnostic helper; основной API всегда возвращает collection/subgraph.

### S2-H01 — Missing unit DAT может быть преобразован в пустое успешное expansion

**Приоритет:** HIGH, требует подтверждающего regression test

В inspected helper `VfsError::NotFound` превращается в `Ok` с `expected_count = 0` и пустым списком. Если caller не создаёт отдельный failure до/после этого вызова, reachable missing dependency исчезает из failure set.

Требуемое поведение:

- reachable + required → typed failure;
- unreachable → warning/report record;
- optional → explicit fallback edge;
- corrupt → error независимо от optionality, если ресурс найден и malformed.

### S2-H02 — Provenance enum недостаточен и не покрывает весь канонический список

**Приоритет:** HIGH

Текущий enum описывает несколько типов переходов, но отсутствуют или не материализованы:

- prototype inheritance parent;
- BASE/resource variant;
- component hierarchy/link;
- effect/FX dependency;
- fallback source;
- source archive entry/span;
- exact mission object identity.

`message: String` не заменяет provenance.

### S2-H03 — `PreparedVisual` является summary, а не prepared asset

**Приоритет:** HIGH

Структура содержит ResourceKey и counts (`model_nodes`, `material_count`, …), но не immutable vertex/index streams, materials, decoded texture mip data, lightmap bindings или dependency handles. Она пригодна для audit report, но не как sole CPU asset handoff в renderer/runtime.

Нужно разделить:

- `PreparedVisualSummary` для отчётов;
- `MeshAsset`;
- `MaterialAsset`;
- `TextureAsset`;
- `LightmapAsset`;
- `EffectAsset`;
- `VisualAsset` с typed IDs/handles;
- `MissionAssets` как транзакционно подготовленный набор.

### S2-H04 — Stable ID без collision handling

**Приоритет:** HIGH/MEDIUM

64-bit FNV-like hash удобен и детерминирован, но collision не проверяется. Stable ID — часть persistent captures и graph comparison, поэтому молчаливая коллизия недопустима.

**Рекомендация:** canonical key interner с equality check; ID может быть SHA-256 prefix/128-bit hash либо deterministic ordinal после canonical sort. При hash collision должна возникать explicit error.

### S2-H05 — Errors теряют concrete causes

**Приоритет:** HIGH

Prototype и assets преобразуют Resource/MSH/Material/Texture errors в строки. В результате graph failure не может отличить missing archive, missing entry, malformed offsets, unsupported variant и allocation limit.

Это одновременно блокирует Stage 1 diagnostics и Stage 2 optional/corrupt policy.

### S2-M01 — Graph success predicate слишком зависим от aggregate counts

**Приоритет:** MEDIUM/HIGH

`PrototypeGraphReport::is_success()` опирается на отсутствие failures и соответствие aggregate resolved count. Такой predicate не доказывает, что:

- каждый material/texture/lightmap/effect request resolved;
- каждый edge имеет provenance;
- отсутствуют orphan/duplicate nodes;
- requiredness policy применена;
- graph deterministic.

Success должен вычисляться как validation materialized graph с инвариантами.

## 6.3 Положительные элементы Stage 2

- Unit DAT records сохраняют raw archive/resource/description bytes и parent/link field.
- Есть отдельный binding variant и CP1251-related support.
- Internal expansion обходит несколько component records.
- Есть depth limit для prototype inheritance.
- Report считает mesh, WEAR, material, texture и lightmap requests/resolutions.
- `AssetId<T>` typed на уровне Rust.
- Asset preparation выполняется транзакционно на уровне метода: ошибка прерывает план.
- Stable ordering поддерживается рядом BTree collections и sorted traversal.
- Mission loading имеет staged phases и transactional world registration concepts.

## 6.4 Обязательные изменения для полного закрытия Stage 2

1. Сделать prototype crate graph-only и убрать из него MSH/material/Texm decoding.
2. Создать materialized typed graph с stable nodes/edge instances.
3. Добавить full provenance и typed requiredness/fallback.
4. Завершить variants: direct, inherited, BASE, unit component hierarchy, effects.
5. Превратить `fparkan-assets` в единственный decode/preparation layer.
6. Создать реальные immutable CPU asset types.
7. Подключить AssetManager к runtime и удалить direct parser dependencies.
8. Перевести apps на runtime/asset services.
9. Ввести deterministic ID registry с collision detection.
10. Запустить полный Part 1/2 mission reachability gate и зафиксировать zero failures.

---

# 7. Сквозные архитектурные замечания

## 7.1 Нужен единый dependency policy, проверяемый Cargo metadata

Текущая архитектура допускает запрещённые направления зависимостей. Следует формально проверить:

- `apps/*` не зависят от format parsers;
- `runtime` не зависит от NRes/RsLi/MSH/Texm/material/FX format crates напрямую;
- `prototype` не зависит от visual asset parsers;
- neutral crates не зависят от `ash`, `winit`, raw OS APIs;
- `headless` dependency closure не содержит platform/Vulkan crates;
- Vulkan raw types не выходят из adapter.

Policy должна использовать `cargo_metadata`, а не поиск строк в TOML.

## 7.2 Один error taxonomy для Stage 1–2

Рекомендуемый общий набор классификаций:

```text
MissingArchive
MissingEntry
AmbiguousName
InvalidPath
UnsupportedVariant
CorruptHeader
OutOfBounds
LimitExceeded
AllocationBudgetExceeded
DecompressionFailed
IntegrityMismatch
OptionalUnavailable
FallbackApplied
PlatformUnavailable
GpuCapabilityMissing
```

Concrete format errors остаются source; верхние layers добавляют context, не превращая source в строку.

## 7.3 Acceptance report должен быть доказательством, а не count dump

Каждый stage report должен включать:

- schema version;
- engine commit SHA;
- toolchain version;
- platform/target;
- corpus manifest fingerprint;
- configuration/profile;
- counts;
- warnings/failures с stable codes;
- evidence status;
- SHA-256 самого report;
- ссылки на dependent artifacts.

## 7.4 Документация и код должны иметь один source of truth

Сейчас canonical Vulkan revision находится в Notion, а repository tome и parity docs содержат прежние stages/commands. Рекомендуется:

- хранить canonical acceptance schema в repository;
- либо экспортировать Notion plan в versioned Markdown;
- CI должен проверять, что workspace adapter names и documented architecture совпадают;
- устаревшие команды должны быть executable doctests или удалены.

---

# 8. План полного закрытия Stage 0–2

Ниже приведена рекомендуемая последовательность PR/changesets. Порядок важен: следующий блок не должен объявляться закрытым до прохождения exit gate предыдущего.

## 8.1 Stage 0 closure train

### PR S0-01 — Reproducible toolchain и repository metadata

**Изменения**

- закрепить exact Rust toolchain;
- добавить `workspace.package.rust-version`;
- документировать supported targets;
- добавить command `xtask doctor` с выводом toolchain/target/SDK versions;
- report всегда включает commit SHA.

**Acceptance**

- clean checkout воспроизводит одинаковый metadata report;
- MSRV job собирает neutral crates;
- current pinned toolchain выполняет полный gate.

### PR S0-02 — Typed xtask configuration и policy engine

**Изменения**

- serde/TOML schema для corpus и acceptance manifests;
- `deny_unknown_fields`;
- canonical absolute path rules для licensed manifest;
- `cargo_metadata` для dependency graph/policy;
- tests malformed/duplicate/unknown/missing fields;
- удалить ручной line parser.

**Acceptance**

- malformed manifest не принимается частично;
- unknown fields fail;
- dependency policy работает по package IDs и targets/features.

### PR S0-03 — Полный synthetic CI gate

**Обязательные команды**

```text
cargo fmt --all -- --check
cargo test --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
RUSTDOCFLAGS="-D warnings -D rustdoc::broken_intra_doc_links" cargo doc --workspace --no-deps --all-features --locked
cargo deny check advisories bans licenses sources
cargo xtask policy
cargo xtask acceptance audit --strict
```

Добавить denylist legacy adapter names, Python runtime files и unsafe allowlist.

### PR S0-04 — Redesign `fparkan-platform`

**Изменения**

- удалить GL context types;
- убрать `present()` из WindowPort;
- ввести normalized events;
- выделить physical/logical size и scale factor;
- определить lifecycle state machine;
- error context/cause.

**Synthetic tests**

- resize coalescing;
- scale-factor change;
- minimized zero-size;
- suspend/resume;
- keyboard repeat/modifiers;
- focus loss clears held input;
- deterministic event ordering.

### PR S0-05 — `fparkan-platform-winit`

**Изменения**

- winit event loop и window;
- raw window/display handles;
- platform-specific lifecycle mapping;
- no GPU ownership.

**Acceptance**

- window-only smoke на трёх OS;
- event trace соответствует synthetic model.

### PR S0-06 — Vulkan low-level boundary

**Изменения**

- `ash`/`ash-window` adapter;
- loader/instance/debug messenger;
- physical device records и pure scoring;
- queue selection;
- deterministic capability JSON;
- narrow unsafe module policy.

**Negative tests**

- no loader;
- no Vulkan 1.1;
- no graphics queue;
- no present queue;
- missing swapchain extension;
- unsupported required format.

### PR S0-07 — Swapchain, pipeline и offline shaders

**Изменения**

- surface/swapchain;
- color/depth policy;
- render pass;
- indexed triangle;
- semaphores/fences;
- frames-in-flight;
- resize/out-of-date/suboptimal;
- offline SPIR-V compile/validate;
- descriptor/push-constant manifest и hashes.

### PR S0-08 — macOS portability и packaging proof

**Изменения**

- enumerate portability flag;
- detect/enable portability subset;
- MoltenVK bundling strategy;
- deterministic portability report;
- `.app` smoke packaging.

### PR S0-09 — Composition roots и legacy removal

**Изменения**

- game/viewer подключают winit+Vulkan adapters;
- headless остаётся isolated;
- удалить SDL/GL stubs;
- очистить lockfile/policy/docs;
- переименовать CPU IDs в neutral render model.

### PR S0-10 — Native acceptance matrix

**Jobs**

- Windows MSVC + Vulkan runtime;
- Linux X11/Wayland smoke; software Vulkan может быть PR gate, отдельный native-GPU job — release gate;
- macOS Apple Silicon + MoltenVK;
- 300 frames, resize, validation error count = 0;
- capability/shader/validation logs как artifacts.

**Stage 0 закрывается только после merge всех PR S0-01…S0-10 и зелёных platform artifacts.**

---

## 8.2 Stage 1 closure train

### PR S1-01 — Unified decode context

- `DecodeLimits` с per-format overrides;
- thread-safe cumulative `AllocationBudget`;
- reservation guards;
- output budget для decompressors;
- span-aware errors;
- migrate all decoders.

### PR S1-02 — Byte-first paths и host adapter

- `LegacyPath(Vec<u8>)`;
- ASCII normalization на bytes;
- optional decoded display;
- Unix `OsStr` bytes path;
- Windows conversion policy с explicit encoding/error;
- strict/compatible contract table.

### PR S1-03 — VFS hardening

- capability/openat-style traversal;
- no-follow final open;
- root confinement;
- visited file identity при allowed symlinks;
- segment-safe prefix semantics;
- strict fingerprint mode;
- одинаковый casefold policy во всех implementations.

### PR S1-04 — Diagnostics integration

- serde-backed diagnostic schema;
- stable codes;
- source chain;
- archive/entry/span/phase context;
- adapters для all format errors;
- property tests JSON;
- запрет `source.to_string()` в domain errors через policy/lint review.

### PR S1-05 — NRes finalization

- decode context integration;
- edit preservation tests;
- stable directory order specification;
- malformed/fuzz corpus;
- Part 1/2 no-edit byte identity.

### PR S1-06 — RsLi editable/lossless model

- parsed/preserved/editable segments;
- deterministic table/lookup rebuild;
- packing policy;
- all observed methods with budgets;
- AO/EOF+1/presort quirk evidence registry;
- no-edit and edited roundtrips.

### PR S1-07 — Resource repository finalization

- typed source errors;
- cache/decode budgets separated;
- deterministic LRU policy documented;
- concurrent same-entry decode coalescing или explicit duplicate policy;
- stale handle/concurrency tests;
- strict verification mode.

### PR S1-08 — Production corpus runner

- parser registry, а не extension counters;
- NRes, RsLi и все Stage 1-relevant production parsers;
- any parser error increments failures и causes non-zero exit;
- stable schema and diff;
- no licensed path leakage in synthetic artifacts.

### PR S1-09 — Licensed closure artifacts

Для Part 1 и Part 2 отдельно:

- corpus manifest SHA;
- archive inventory;
- parser report;
- method/quirk coverage;
- no-edit roundtrip report;
- edit-preservation regression set;
- unexplained failures = 0.

**Stage 1 закрывается только после S1-01…S1-09 и двух стабильных licensed reports.**

---

## 8.3 Stage 2 closure train

### PR S2-01 — Typed graph schema

Ввести:

```text
NodeId
NodeKind { MissionObject, UnitDat, UnitComponent, Prototype, Model,
           Wear, Material, Texture, Lightmap, Effect, Auxiliary }
EdgeId
EdgeKind
Requiredness { Required, Optional, Fallback }
Provenance { source path/archive/entry/span, parent edge, object/component indices }
DependencyGraph { nodes, edges, roots }
```

Graph validation проверяет no dangling edges, canonical order, unique node keys и parent chain.

### PR S2-02 — Prototype resolver variants

- direct registry;
- inherited parent chain;
- BASE/resource variants;
- unit DAT binding;
- multi-component unit hierarchy;
- cycle/depth diagnostics;
- lossless raw fields;
- remove first-component public API.

### PR S2-03 — Requiredness и failure semantics

- reachable required missing = error;
- unreachable missing = warning;
- optional missing = explicit optional record;
- fallback = edge с причиной и chosen source;
- found-but-corrupt = error;
- stable diagnostic codes and full chain.

### PR S2-04 — Разделение prototype/assets

- убрать MSH/material/Texm dependencies из prototype;
- graph хранит только resource identities/provenance;
- `fparkan-assets` единолично вызывает visual/effect parsers;
- policy запрещает обратные dependencies.

### PR S2-05 — Immutable CPU assets и ID registry

- typed immutable model/material/texture/lightmap/effect assets;
- canonical IDs;
- collision detection;
- deduplication;
- source provenance;
- separate parsed-byte and resident-asset budgets.

### PR S2-06 — Mission AssetManager transaction

`AssetManager::prepare_mission(graph)`:

1. валидирует graph;
2. вычисляет canonical load plan;
3. декодирует resources в budget;
4. собирает immutable assets;
5. не публикует partial result при failure;
6. возвращает `MissionAssets` + report.

### PR S2-07 — Runtime integration

- runtime зависит от assets API;
- удалить direct NRes/MSH/Texm/material/effect parsing из runtime;
- `LoadedMissionState` хранит `Arc<MissionAssets>`;
- world objects ссылаются на typed asset IDs;
- render snapshot строится из prepared visuals, а не из object slot.

### PR S2-08 — App cleanup

- viewer использует AssetManager service;
- CLI parser-level commands остаются только в dedicated tooling crate;
- game не генерирует synthetic GPU IDs;
- dependency policy запрещает app → format crates.

### PR S2-09 — Synthetic graph suite

Обязательные fixtures:

- direct prototype;
- inherited prototype;
- multi-level inheritance;
- cycle;
- depth limit;
- BASE variant;
- multi-component unit с hierarchy;
- duplicate/casefold ambiguity;
- required missing;
- optional missing;
- corrupt present resource;
- material/texture/lightmap/effect chain;
- stable serialization/IDs на повторном запуске.

### PR S2-10 — Licensed reachability closure

Для каждой миссии обеих частей:

- roots;
- unit components;
- prototype/model/material/texture/lightmap/effect nodes;
- required/optional/fallback counts;
- failure list;
- canonical graph hash;
- asset plan hash.

Exit conditions:

- reachable failures = 0;
- каждый resolved edge имеет provenance;
- повторный запуск даёт те же graph/asset hashes;
- runtime parser dependency audit = clean.

**Stage 2 закрывается только после S2-01…S2-10 и полного licensed mission matrix.**

---

# 9. Требуемая CI/acceptance модель

## 9.1 Synthetic PR gate

Должен работать без игровых каталогов и без silent skip:

1. formatting/lints/docs/security;
2. all unit/integration/property tests;
3. malformed parser corpus;
4. Stage 0 pure policy tests;
5. Stage 1 archive synthetic roundtrips;
6. Stage 2 graph synthetic fixtures;
7. headless dependency assertion;
8. Linux software-Vulkan smoke при доступности;
9. report schema validation.

Любой test, требующий licensed files, должен быть в отдельном command suite, а не `#[ignore]` внутри общего gate без machine-readable explanation.

## 9.2 Native platform gate Stage 0

| Platform | Минимальный gate | Дополнительное доказательство |
|---|---|---|
| Windows | system Vulkan loader, real swapchain, triangle, resize, 300 frames, validation=0 | NVIDIA/AMD/Intel coverage по release cadence |
| Linux | X11 или Wayland surface, swapchain, resize, validation=0 | software Vulkan PR job + native Mesa/NVIDIA release jobs |
| macOS | MoltenVK, portability enumeration/subset, CAMetalLayer surface, resize, validation=0 | Apple Silicon primary; Intel optional only if declared supported |

## 9.3 Licensed local/restricted gate

- absolute roots только из local manifest;
- CI logs не содержат raw licensed paths;
- reports используют logical corpus IDs;
- artifacts не содержат game bytes;
- manifests содержат только path hashes/size/format metrics;
- failures приводят к non-zero exit;
- baseline update требует reviewed diff и reason.

---

# 10. Definition of Done

## 10.1 Stage 0 DoD

- [ ] Exact Rust toolchain и MSRV закреплены.
- [ ] Full fmt/test/clippy/doc/security/source/license gate проходит.
- [ ] Typed manifests и `cargo_metadata` используются.
- [ ] Windows/Linux/macOS matrix сохраняет artifacts.
- [ ] `fparkan-platform-winit` реализован.
- [ ] `fparkan-render-vulkan` реализован.
- [ ] Vulkan 1.1 baseline и capability report реализованы.
- [ ] MoltenVK portability path реализован.
- [ ] Offline SPIR-V validation/hash manifest реализован.
- [ ] Legacy SDL/GL adapters и references удалены.
- [ ] Game/viewer используют новые composition adapters.
- [ ] 300-frame + resize smoke проходит на трёх OS без validation errors.
- [ ] Headless dependency closure не содержит window/Vulkan.

## 10.2 Stage 1 DoD

- [ ] Path identity byte-first и roundtrip-safe.
- [ ] VFS root/symlink/casefold semantics едины и безопасны.
- [ ] Все decoders принимают общий limits/budget context.
- [ ] Decompression output bounded до allocation.
- [ ] NRes no-edit и edited roundtrips подтверждены.
- [ ] RsLi no-edit и edited roundtrips подтверждены.
- [ ] All observed RsLi methods/quirks имеют evidence records.
- [ ] Structured diagnostics проходят через parsers/repository/runtime.
- [ ] JSON reports валидны для всех Unicode/control inputs.
- [ ] Resource repository сохраняет typed errors, handles и deterministic eviction.
- [ ] Corpus report вызывает production parsers.
- [ ] Part 1/2 reports стабильны; unexplained failures = 0.

## 10.3 Stage 2 DoD

- [ ] Materialized typed dependency graph существует.
- [ ] Все edge instances имеют full provenance.
- [ ] Direct/inherited/BASE/unit hierarchy variants реализованы.
- [ ] Все unit components регистрируются; first-component shortcut отсутствует.
- [ ] Effect dependencies включены.
- [ ] Required/optional/fallback/corrupt semantics разделены.
- [ ] `fparkan-assets` — единственный CPU preparation layer.
- [ ] Apps/runtime не зависят от format parsers напрямую.
- [ ] Immutable CPU assets и collision-safe stable IDs реализованы.
- [ ] Runtime хранит/использует `MissionAssets`.
- [ ] Render snapshots используют prepared asset IDs.
- [ ] Synthetic graph fixtures проходят.
- [ ] Все миссии Part 1/2 дают reachable failures = 0.
- [ ] Graph/asset hashes детерминированы.

---

# 11. Рекомендуемые automated policy checks

Добавить в `cargo xtask policy`:

1. Запрет workspace members/path names:
   - `fparkan-platform-sdl`;
   - `fparkan-render-gl`;
   - stale OpenGL/GLES profile symbols.
2. Dependency rules:
   - apps не зависят от `fparkan-*format`, NRes, RsLi, MSH, Texm, material, FX;
   - runtime не зависит от этих crates напрямую;
   - prototype не зависит от MSH/material/Texm/FX;
   - headless не зависит от winit/ash/ash-window/Vulkan adapter;
   - neutral crates не зависят от platform adapters.
3. Unsafe rules:
   - unsafe разрешён только в exact Vulkan/FFI modules;
   - каждый block содержит `SAFETY:`;
   - raw handles не являются public API.
4. Test rules:
   - synthetic gate не содержит licensed roots;
   - ignored tests должны иметь registered reason и отдельный suite owner;
   - acceptance IDs уникальны.
5. Documentation rules:
   - documented crates/commands существуют;
   - stage schema version совпадает с report schema;
   - old backend names отсутствуют в canonical docs.

---

# 12. Риски реализации и способы снижения

| Риск | Влияние | Снижение |
|---|---|---|
| Vulkan work начнётся до исправления platform contract | Повторная переделка surface/present/lifecycle | Сначала S0-04, затем adapters |
| Global unsafe prohibition будет ослаблен целиком | Рост FFI risk | Изолированный audited crate/module и policy scanner |
| Stage 1 budgets добавят только к top-level files | Nested decompression bomb останется | Общий shared budget, передаваемый во все вложенные decoders |
| RsLi writer будет canonical-only | Потеря unknown/overlay bytes | Segment-preserving editable model и lossless-first tests |
| Graph report останется count-only | False green Stage 2 | Materialized graph + invariant validator + canonical serialization |
| Prototype и assets продолжат оба парсить visuals | Divergent fallback и diagnostics | Жёсткий dependency DAG и policy test |
| Hash IDs столкнутся | Неправильные assets/captures | Collision detection и canonical interner |
| Licensed tests останутся ignored/local-only без artifacts | Невозможность доказать exit gate | Separate command, signed reports, baseline diff process |
| GitHub mirror и primary diverge | Audit не соответствует release | Pin canonical remote + commit SHA в каждом report |
| Documentation останется отдельной от acceptance schema | Повторное рассогласование stages | Versioned repository schema и generated docs/checks |

---

# 13. Приоритетный backlog

## Немедленно — до любых новых gameplay/render features

1. S0-01…S0-04: reproducibility, CI, typed config, platform API.
2. S1-01: общий decode/allocation budget.
3. S2-04: запрет дублирующего parsing между prototype/assets.
4. S1-04: typed diagnostics без string erasure.

## Затем — минимальный доказуемый Vulkan foundation

1. winit adapter.
2. Vulkan loader/device/surface/swapchain.
3. offline shaders.
4. 3-platform smokes.
5. legacy adapter removal.

## Затем — архивный exit gate

1. byte-first paths/VFS hardening;
2. RsLi edit/writer;
3. production corpus runner;
4. licensed reports/roundtrips.

## Затем — graph/assets exit gate

1. typed graph/provenance;
2. all variants/components/effects;
3. immutable assets;
4. runtime/app integration;
5. full Part 1/2 reachability.

---

# 14. Реестр доказательств

## Canonical requirements

- Notion, Vulkan revision: <https://app.notion.com/p/387e79f2db3981778f94cdf34db5f93f>

## Workspace/governance

- Root manifest: <https://github.com/valentineus/fparkan/blob/devel/Cargo.toml>
- Toolchain: <https://github.com/valentineus/fparkan/blob/devel/rust-toolchain.toml>
- Cargo config: <https://github.com/valentineus/fparkan/blob/devel/.cargo/config.toml>
- xtask manifest: <https://github.com/valentineus/fparkan/blob/devel/xtask/Cargo.toml>
- xtask implementation: <https://github.com/valentineus/fparkan/blob/devel/xtask/src/main.rs>
- README: <https://github.com/valentineus/fparkan/blob/devel/README.md>

## Stage 0

- Platform core: <https://github.com/valentineus/fparkan/blob/devel/crates/fparkan-platform/src/lib.rs>
- SDL stub adapter: <https://github.com/valentineus/fparkan/blob/devel/adapters/fparkan-platform-sdl/src/lib.rs>
- Render core: <https://github.com/valentineus/fparkan/blob/devel/crates/fparkan-render/src/lib.rs>
- GL stub adapter: <https://github.com/valentineus/fparkan/blob/devel/adapters/fparkan-render-gl/src/lib.rs>
- Game composition: <https://github.com/valentineus/fparkan/blob/devel/apps/fparkan-game/src/main.rs>
- Viewer composition: <https://github.com/valentineus/fparkan/blob/devel/apps/fparkan-viewer/src/main.rs>
- Headless manifest: <https://github.com/valentineus/fparkan/blob/devel/apps/fparkan-headless/Cargo.toml>

## Stage 1

- Binary/limits: <https://github.com/valentineus/fparkan/blob/devel/crates/fparkan-binary/src/lib.rs>
- Paths: <https://github.com/valentineus/fparkan/blob/devel/crates/fparkan-path/src/lib.rs>
- VFS: <https://github.com/valentineus/fparkan/blob/devel/crates/fparkan-vfs/src/lib.rs>
- NRes: <https://github.com/valentineus/fparkan/blob/devel/crates/fparkan-nres/src/lib.rs>
- RsLi: <https://github.com/valentineus/fparkan/blob/devel/crates/fparkan-rsli/src/lib.rs>
- Diagnostics: <https://github.com/valentineus/fparkan/blob/devel/crates/fparkan-diagnostics/src/lib.rs>
- Resource repository: <https://github.com/valentineus/fparkan/blob/devel/crates/fparkan-resource/src/lib.rs>
- Corpus runner: <https://github.com/valentineus/fparkan/blob/devel/crates/fparkan-corpus/src/lib.rs>

## Stage 2

- Prototype graph: <https://github.com/valentineus/fparkan/blob/devel/crates/fparkan-prototype/src/lib.rs>
- Prototype manifest: <https://github.com/valentineus/fparkan/blob/devel/crates/fparkan-prototype/Cargo.toml>
- Assets: <https://github.com/valentineus/fparkan/blob/devel/crates/fparkan-assets/src/lib.rs>
- Assets manifest: <https://github.com/valentineus/fparkan/blob/devel/crates/fparkan-assets/Cargo.toml>
- Mission format: <https://github.com/valentineus/fparkan/blob/devel/crates/fparkan-mission-format/src/lib.rs>
- Runtime: <https://github.com/valentineus/fparkan/blob/devel/crates/fparkan-runtime/src/lib.rs>
- Runtime manifest: <https://github.com/valentineus/fparkan/blob/devel/crates/fparkan-runtime/Cargo.toml>
- FX manifest: <https://github.com/valentineus/fparkan/blob/devel/crates/fparkan-fx/Cargo.toml>

## Documentation drift

- Repository implementation tome: <https://github.com/valentineus/fparkan/blob/devel/docs/tomes/07-implementation.md>
- Parity README: <https://github.com/valentineus/fparkan/blob/devel/parity/README.md>
- Parity cases: <https://github.com/valentineus/fparkan/blob/devel/parity/cases.toml>

---

# 15. Финальное заключение

Проект не находится в плохом состоянии: у него уже есть заметно более сильная CPU/data foundation, чем обычно бывает на ранней стадии восстановления движка. Однако текущая структура создаёт риск преждевременного объявления stages завершёнными, потому что stubs, count reports и ignored licensed tests могут выглядеть как acceptance evidence.

Главный принцип закрытия:

> Stage считается завершённым не тогда, когда существует crate или test с нужным названием, а когда канонический exit gate выполняется на production path, выдаёт воспроизводимый machine-readable artifact и не имеет обходного альтернативного пути.

Для Stage 0 production path — настоящий Vulkan swapchain на трёх OS.  
Для Stage 1 — bounded production parsers плюс lossless licensed roundtrips.  
Для Stage 2 — materialized typed graph, immutable assets и runtime, который не обходит AssetManager.

До выполнения перечисленного рекомендуется маркировать текущий статус как:

```text
Stage 0: IN PROGRESS / BLOCKED
Stage 1: IN PROGRESS / ARCHIVE FOUNDATION PARTIAL
Stage 2: IN PROGRESS / GRAPH AND ASSET ARCHITECTURE NOT CLOSED
```
