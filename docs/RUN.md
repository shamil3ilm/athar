# Running the V0 stack

The V0 stack is two processes: the **daemon** (Rust) and the **shim** (PHP) inside an application. They talk over TCP loopback for now.

## Prerequisites

- **Rust**: `rustup default stable` (1.75+).
- **PHP**: 8.1+. `ext-sockets` must be enabled (default on most builds).
- No other dependencies. No external services. No network calls out.

## Start the daemon

```
cd C:\athar\daemon
cargo run --release --bin athar-daemon
```

Environment variables (all optional):

| Variable | Default | Notes |
|---|---|---|
| `ATHAR_LISTEN_ADDR` | `127.0.0.1:11223` | TCP loopback. Bind to `[::1]:port` for IPv6. |
| `ATHAR_DATA_DIR` | `./athar-data` | Segment log lives under `evidence/` here. |
| `ATHAR_QUOTA_BYTES` | 2 GB | Disk quota over the whole log (`OPS-12`). |
| `ATHAR_MAX_SEGMENT_BYTES` | 64 MB | Rotate at this size. |
| `ATHAR_MAX_RECORD_BYTES` | 8 MB | Hard cap per record. |
| `ATHAR_HOST_METRICS_INTERVAL_SECS` | `5` | Live CPU/memory sampling cadence for the governor. |
| `ATHAR_STALENESS_SCAN_INTERVAL_SECS` | `60` | How often the staleness scanner sweeps open lifecycles. |
| `ATHAR_AUDIT_RECORDS_PER_SEGMENT` | `1000` | Audit segment rotation threshold. |
| `ATHAR_EVICTION_INTERVAL_SECS` | `30` | How often the eviction ladder driver runs. |
| `ATHAR_EVICTION_HIGH_WATER_PCT` | `85.0` | Fill ratio at which eviction begins. |
| `ATHAR_EVICTION_LOW_WATER_PCT` | `70.0` | Fill ratio at which eviction stops (hysteresis). |
| `RUST_LOG` | `info` | `debug` shows every frame. |

`Ctrl-C` shuts down cleanly: the active `.wip` segment is fsynced and renamed to `.seg` so the next boot has nothing to quarantine.

## Emit events from an application

### Standalone PHP (no framework)

```php
require '/path/to/athar/shim/src/Shim/Classify.php';
require '/path/to/athar/shim/src/Shim/Redact.php';
require '/path/to/athar/shim/src/Shim/Ulid.php';
require '/path/to/athar/shim/src/Shim/Clock.php';
require '/path/to/athar/shim/src/Shim/Buffer.php';
require '/path/to/athar/shim/src/Shim/Transport.php';
require '/path/to/athar/shim/src/Shim/EventFactory.php';
require '/path/to/athar/shim/src/RuntimeConfig.php';
require '/path/to/athar/shim/src/Contract/RuntimeInterface.php';
require '/path/to/athar/shim/src/Runtime.php';

use Athar\Runtime;

Runtime::enable();

// ...your app runs...

// The shim auto-flushes at end-of-request via register_shutdown_function.
```

### Laravel

Once the package is `composer require`-installed (Composer publishing is not set up
yet — for now install by pointing at a `path` repository in your app's composer.json),
Laravel's package discovery picks up `Athar\Adapter\Laravel\ServiceProvider`
automatically. Nothing else in `AppServiceProvider` is required.

### With env vars

```
ATHAR_TENANT_ID=tnt_your_company \
ATHAR_DAEMON_HOST=127.0.0.1 \
ATHAR_DAEMON_PORT=11223 \
php your-app.php
```

## Verify end-to-end without the Rust daemon

If Rust isn't built yet, a pure-PHP fake daemon lets you exercise the shim:

```
php shim/bin/e2e-test.php
```

This spawns a fake TCP listener, emits 5 events through the shim, and asserts:
- Every event arrived and parses as JSON with the canonical schema.
- Every `event_id` is a valid ULID.
- No `C5` secret marker survives the shim (PRI-7 end-to-end).

Expected output ends with `E2E ALL GREEN`.

## Verify end-to-end with the Rust daemon

```
# terminal 1
cd C:\athar\daemon
ATHAR_DATA_DIR=./v0-data cargo run --release --bin athar-daemon

# terminal 2
cd C:\athar
ATHAR_TENANT_ID=tnt_local php shim/bin/emit.php 5

# terminal 1 (after Ctrl-C on the daemon)
# Inspect the segments
ls daemon/v0-data/evidence
```

Each `.seg` file is a self-contained append-only log of length-prefixed canonical events.

## Verify the audit chain of persisted data

After the daemon has closed at least one audit segment (either by hitting
`ATHAR_AUDIT_RECORDS_PER_SEGMENT` or by graceful shutdown):

```
cd C:\athar\daemon
cargo run --release --bin athar -- audit verify ./v0-data/audit/segments
```

Expected output on healthy data:

```
OK: N segment(s), M record(s) verified
```

Any tampering — one bit flipped in any record's payload commitment, any prev_hash link,
any signature — is reported with the exact record index (`SEC-13`) and exits non-zero.

The signing key lives at `./v0-data/audit/keys/segment.key` (dev mode only, per D5).
**Do not deploy this configuration.** Real deployments use OS keystore (T1/T2) or
PKCS#11 (T3/T4); the `Signer` trait swaps cleanly.

## Query lifecycles

The daemon writes lifecycle state to a SQLite database at
`<data-dir>/state/lifecycles.db`. WAL mode lets you query it from a separate
process while the daemon runs.

```
# List open lifecycles
cargo run --release --bin athar -- lifecycle list ./v0-data/state/lifecycles.db --open

# Show one lifecycle in detail
cargo run --release --bin athar -- lifecycle show ./v0-data/state/lifecycles.db lc_<event_id>

# Show as JSON (pipe to jq etc.)
cargo run --release --bin athar -- lifecycle show ./v0-data/state/lifecycles.db lc_<event_id> --json
```

The output includes each event's correlation tier + confidence, and any late
events with their classification.

## Production posture — what still has to land before this is a real product

Not yet in V0:

- **SQLite state store** — no lifecycle tracking, no identity graph, no decision records.
- **Audit chain persistence** — the audit chain code exists and its tests pass, but the daemon doesn't yet route events through it. Evidence is stored append-only but not signed.
- **Encryption at rest** — segments are plain bytes on disk. Add before customer pilots.
- **AF_UNIX transport** — TCP loopback is fine for a Windows dev box; on Linux switch to Unix domain sockets for lower overhead and filesystem-permission-based access control.
- **Governor connected to ingest** — the governor exists but is not yet consulted on the ingest path. `OPS-18` unproven end-to-end.
- **CLI `runtime` binary** — `athar-cli` is a stub.
- **Reference application** (D13) — needed to make `PERF-1`..`PERF-4` claims measurable.
- **CEL policy engine** (D11) — no policies can be evaluated.

Don't put this in front of a customer yet. What you have is a *working ingest pipeline* end-to-end, which is the foundation everything else builds on.

## Operational notes

- The daemon owns no keys yet. When D5's `Signer` implementations land, key handles come from the OS keystore or a PKCS#11 module — never from disk.
- If the daemon is stopped uncleanly, the segment log will find a `.wip` on next start and rename it to `.quarantine`. A coverage_gap event should be emitted then (wiring pending).
- Backup = copy the `evidence/` directory. Segments are self-contained.
- Uninstall = `rm -rf` the data directory and stop the daemon. Nothing else to clean up.
