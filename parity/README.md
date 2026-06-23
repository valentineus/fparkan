# Render Parity Dataset

This folder stores parity-test input for legacy render comparison workflows.

- `cases.toml`: list of deterministic render cases.
- `reference/*.png`: baseline frames captured from the original renderer.

Expected workflow:

1. Capture baseline PNG frames for each case.
2. Add entries to `cases.toml`.
3. Run the acceptance renderer capture workflow with fixed profiles and compare
   output captures out-of-tree.

```text
1) Prepare `cases.toml` and baseline captures.
2) Run `fparkan-game` (or dedicated acceptance runner) with fixed seed.
3) Compare outputs against baseline in dedicated comparison tooling.
```

The `render-parity` crate is no longer present as a standalone runner in this
workspace snapshot; parity evidence is now produced through the acceptance
artifacts and stage audit tooling.
