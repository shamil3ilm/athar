# athar daemon (Rust)

Out-of-band local runtime per SPEC `INV-11`. Receives events from shims over Unix socket, normalizes, correlates, tracks lifecycles, evaluates policy, and stores.

## Workspace layout

| Crate | Role | SPEC ref |
|---|---|---|
| `athar-event` | Canonical event types (v1.0 schema), (de)serialization, validation | §5.4 `MOD-6` |
| `athar-storage` | SQLite (state) + append-only segmented log (evidence + audit) | §6.5 `OPS-9`–`OPS-13`, D4 |
| `athar-audit` | Hash chain + Ed25519 signing + `verify` | §8.6 `SEC-11`–`SEC-15`, D5 |
| `athar-governor` | Resource governor + degradation ladder + dead-man's switch | §7.3, §7.5 `OPS-17`–`OPS-19`, `INV-14` |
| `athar-daemon` | Daemon binary (ingest → pipeline → storage) | §6.1 |
| `athar-cli` | `runtime <subcommand>` CLI | §15.1 `OPS-30` |

Governor is built before the features it protects (Appendix A step 5). Do not scale up the daemon binary until `athar-governor` is landed and fault-tested.

## Build

```
cargo build --workspace
cargo test  --workspace
```

Toolchain pinned in `rust-toolchain.toml` (`stable`). MSRV is Rust 1.85 (required by transitive dep `base64ct`, which needs `edition2024`). All crates share the workspace's dependency versions.
