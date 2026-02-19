# render-parity

Deterministic frame-diff runner for `parkan-render-demo`.

Usage:

```bash
cargo run -p render-parity -- \
  --manifest parity/cases.toml \
  --output-dir target/render-parity/current
```

Options:

- `--demo-bin <path>`: use prebuilt `parkan-render-demo` binary instead of `cargo run`.
- `--keep-going`: continue all cases even after failures.
