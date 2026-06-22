# Current Project Audit

Baseline command:

```text
env RUSTC=/Users/valentineus/.rustup/toolchains/stable-aarch64-apple-darwin/bin/rustc /opt/homebrew/bin/rustup run stable cargo test --workspace --offline
```

Result on 2026-06-22:

- library and binary unit tests compile and pass after aligning SDL2 versions and pinning `toml` to cached `0.8`;
- doctests fail in this shell because `rustdoc` is not in PATH unless `RUSTDOC` is also set to the real toolchain binary;
- full online dependency resolution is unavailable in the sandbox.
