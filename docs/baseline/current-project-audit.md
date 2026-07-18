# Current Project Audit

Baseline command:

```text
cargo xtask ci
```

Result on 2026-07-18:

- canonical pipeline passes locally with Rust 1.97.1: formatting, policy,
  shader provenance, workspace tests, `clippy`, docs and `cargo deny` are
  executed through `cargo xtask ci`;
- `rust-toolchain.toml` pins Rust 1.97.1 and installs only the
  `x86_64-pc-windows-msvc` target; Linux and macOS target installation is not
  part of the default developer setup;
- internal path dependencies are version-pinned and the Windows-only winit
  graph enables only `rwh_06`, excluding Unix window-system dependencies;
- `cargo deny` runs without advisory exceptions in the supported Windows graph.

Native Vulkan evidence:

- Windows x86_64 local smoke passed again on 2026-07-18 with Rust 1.97.1, the
  system Vulkan loader 1.4.350, and an AMD Radeon Pro WX 3200 Series;
- the smoke created a Win32 surface and swapchain, presented 300 frames,
  exercised a controlled resize (three observed resize events and two
  swapchain recreations), and shut down with zero validation warnings and
  errors;
- Windows is the sole supported runtime target for this project; Linux and
  macOS smoke gates are explicitly out of scope.

Scope labels:

- Stage 0 codebase gates: locally evidenced.
- Stage 0 Windows native runtime: locally evidenced.
- Linux/macOS runtime and cross-platform hosted CI: out of scope.

## Current architecture contract

The standalone engine keeps binary formats, the resource graph, simulation,
animation math and backend-neutral render commands independent from the GPU
adapter. Windows presentation uses `winit`, `raw-window-handle`, `ash-window`
and `ash`; only the Vulkan/FFI adapters may contain narrowly documented
`unsafe` code. Raw Vulkan handles do not cross that boundary.

The baseline is Vulkan 1.1 with surface, Win32-surface and swapchain support,
binary semaphores/fences and a classic render-pass path. Device capability is
queried at runtime; dynamic rendering, descriptor indexing, synchronization2,
timeline semaphores and extended dynamic state are optional capability-gated
enhancements, not requirements. The canonical initial texture upload is RGBA8
UNORM. Headless builds remain independent of `winit`, Vulkan and a window
system.

## Current stage model

The canonical Vulkan-revision plan has six dependency-ordered stages numbered
0--5.  Keeping its original numbering matters: Stage 4 is an evidence-gated
animation/FX runtime, while Stage 5 is the mission/world vertical slice that
depends on it.  They must not be reported as one completed stage.

0. reproducible Windows/Vulkan foundation;
1. paths, VFS and lossless archives;
2. prototype graph and prepared CPU assets;
3. static Vulkan model/terrain viewer;
4. animation and FX runtime, with reference-only semantics until runtime
   captures close the x87 and effect-lifecycle evidence gaps;
5. transactional map, mission and world vertical slice, rendered from the
   same immutable snapshot through Vulkan.

This is a local, Windows-only adoption of the Notion page "План реализации
stage 0--5: Vulkan revision" (reviewed on 2026-07-18).  Its former
Linux/macOS portability and hosted-CI goals are intentionally not imported:
they conflict with the current supported-platform boundary above.  The
portable architectural rules that do apply -- backend-neutral commands,
runtime capability queries, narrow Vulkan/FFI `unsafe`, offline shader
validation, and command capture before pixel comparison -- are retained in
this audit and the rendering tome.

Contract tests and failure tests precede implementation. Synthetic checks never
read licensed roots; licensed corpus checks use absolute paths from the local
manifest. Backend-neutral command capture precedes pixel comparison, and GPU
addresses, allocator addresses and driver timing are excluded from deterministic
state hashes.
