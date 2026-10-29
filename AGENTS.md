# AGENTS.md — agent_core contributor notes

## Gospels

The cross-repo canon lives in the `shippopotamuslabs/gospels` repo,
pinned in this repo as a submodule at `contracts/gospel/`. Read the
gospels there; bump the pin deliberately, and never edit the
submodule's contents from this repo.

- Architecture follows
  `contracts/gospel/RUST-ARCHITECTURE-GOSPEL.md` — the dependency
  rule in Rust form: crates as boundaries, traits only at genuine
  ports, checked facts as types, typed errors. (Its sibling,
  `ARCHITECTURE-GOSPEL.md`, is the Android/Kotlin statement of the
  same intent; it does not govern this repo.)
- Tests follow `contracts/gospel/TESTING-GOSPEL.md`: the general
  sections apply in full; its Android-specific §6 does not apply
  here.
- Anything a person reads — docs, comments, plan write-ups —
  follows `contracts/gospel/WUMBO-GOSPEL.md`.
- The UI gospels (Palette, Compose, Glass Bars, Button Shape) do
  not apply to this repo; it has no UI surface.

## Tests

CI runs the following, and a change should pass them locally
before pushing:

- `cargo fmt --check`
- `cargo clippy --all-targets --locked -- -D warnings`, and the
  same with `--no-default-features`
- `cargo test --locked`
- `cargo test --locked --no-default-features`
- `cargo test --locked --features lalr-harness`
- `bash scripts/check-vendored-freshness.sh --self-test`
