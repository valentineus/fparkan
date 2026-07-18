# Current Project Audit

Baseline command:

```text
cargo xtask ci
```

Result on 2026-07-18:

- canonical pipeline passes locally with Rust 1.97.1: formatting, policy,
  shader provenance, workspace tests, `clippy`, docs and `cargo deny` are
  executed through `cargo xtask ci`;
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
