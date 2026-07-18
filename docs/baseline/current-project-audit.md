# Current Project Audit

Baseline command:

```text
cargo xtask ci
```

Result on 2026-07-18:

- canonical pipeline now uses a fixed MSRV/toolchain, policy checks,
  full-format workspace test command, `clippy`/`doc`/`cargo deny` gates and
  typed manifest parsing in `xtask`;
- `rpath`/offline mode is still useful for synthetic local checks;
- full online dependency resolution remains unavailable in the sandbox.

Native Vulkan evidence:

- Windows x86_64 local smoke passed on 2026-07-18 with Rust 1.87.0, the
  system Vulkan loader 1.4.350, and an AMD Radeon Pro WX 3200 Series;
- the smoke created a Win32 surface and swapchain, presented 300 frames,
  exercised a controlled resize (three observed resize events and two
  swapchain recreations), and shut down with zero validation warnings and
  errors;
- the result is configuration-specific evidence, not a substitute for the
  separate Linux and macOS smoke gates.

Scope labels:

- Stage 0 macOS/codebase: closed.
- Stage 0 Windows native runtime: locally evidenced.
- Stage 0 Linux native runtime and cross-platform hosted CI: deferred.
