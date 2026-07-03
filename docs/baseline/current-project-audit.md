# Current Project Audit

Baseline command:

```text
cargo xtask ci
```

Result on 2026-06-23:

- canonical pipeline now uses a fixed MSRV/toolchain, policy checks,
  full-format workspace test command, `clippy`/`doc`/`cargo deny` gates and
  typed manifest parsing in `xtask`;
- `rpath`/offline mode is still useful for synthetic local checks;
- full online dependency resolution remains unavailable in the sandbox.

Scope labels:

- Stage 0 macOS/codebase: closed.
- Stage 0 cross-platform native runtime and hosted CI: deferred.
