# agent_core

Shared Rust agent harness for Pocket Crew. Exposed to Android via UniFFI. Desktop-ready via native Rust host traits (no UniFFI required for direct consumers).

See [`plans/FEAT-agent-core-rust/`](https://gitlab.com/browntown.development-group/pocket-crew/-/tree/main/plans/FEAT-agent-core-rust) for the full feature plan and ADR.

## Building

### Host (tests)

```bash
cargo test
cargo clippy -- -D warnings
cargo fmt --check
```

### Android (aarch64)

```bash
# From pocket-crew repo root:
scripts/build-agent-bindings.sh
```

Requires: Android NDK 27, API 34, `cargo-ndk`. See [AGENT-023](../plans/FEAT-agent-core-rust/tickets/AGENT-023.md) for setup.

## Desktop integration

Add as a path dependency (no UniFFI required):

```toml
[dependencies]
agent_core = { path = "../../third_party/agent_core" }
```

Implement `AgentTransport`, `AgentToolExecutor`, `AgentSummarizer` from `agent_core::host` directly.

## Version / submodule sync

See [AGENT-042](../plans/FEAT-agent-core-rust/tickets/AGENT-042.md) for versioning strategy.
To update the submodule ref in `pocket-crew`: `cd third_party/agent_core && git pull origin main && cd ../.. && git add third_party/agent_core && git commit`.
