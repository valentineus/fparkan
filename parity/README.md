# Render Parity Dataset

This folder stores parity-test input for `crates/render-parity`.

- `cases.toml`: list of deterministic render cases.
- `reference/*.png`: baseline frames captured from the original renderer.

Expected workflow:

1. Capture baseline PNG frames from original game/editor for each case.
2. Add entries to `cases.toml`.
3. Run:

```bash
cargo run -p render-parity -- \
  --manifest parity/cases.toml \
  --output-dir target/render-parity/current
```

On failure, diff images are saved to `target/render-parity/current/diff`.
