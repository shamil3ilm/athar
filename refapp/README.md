# athar — reference application

A pure-PHP payment simulator that exercises the athar daemon end-to-end without needing a real Laravel install. Ships with:

- **`bin/simulator.php`** — generates payment flows in 10 scenarios: `happy`, `fraud`, `fail`, `stale`, `late`, `duplicate`, `velocity`, `fanout`, `stuffing`, `mixed` (or `all`).
- **`bin/evaluate-demo.php`** — shows the synchronous `Runtime::evaluate()` API: emit an event, block for a Decision, act on it (challenge / allow / fail-open).
- **`bin/model-demo.php`** — shows a Laravel-shaped `Payment` model with the `ObservesLifecycle` trait, emitting the full lifecycle on `save()` / `delete()`.
- **`bin/verify.php`** — reads the daemon's SQLite state stores and asserts what should be there (lifecycles, decisions, signals, `INV-17` fields).
- **`src/Payment.php`** — an Eloquent-lookalike model that quacks like `Illuminate\Database\Eloquent\Model` for the trait to bind to.

Everything runs with plain `php` — no `composer install`, no `illuminate/*` dependencies. That's on purpose: this exercises the shim's ability to run in *any* PHP context, and the daemon's ability to process *any* canonical event stream.

## Prerequisites

- **PHP 8.1+** with `ext-sockets` and `ext-pdo_sqlite` (both default on almost every install).
- **A running daemon** — build with `cd daemon && cargo build --release` and run `./target/release/athar-daemon`. Or, for local shim-only testing, spin up `php shim/bin/fake-daemon.php` on the same host.

## Quick start — full end-to-end run

```powershell
# terminal 1: daemon
cd C:\athar\daemon
$env:ATHAR_DATA_DIR = "C:\athar\refapp-data"
cargo run --release --bin athar-daemon

# terminal 2: simulate a mix of payment flows
cd C:\athar
php refapp\bin\simulator.php --scenario=all --verbose

# Ctrl-C the daemon in terminal 1 to flush.

# Verify: read the SQLite state directly
php refapp\bin\verify.php --data-dir=C:\athar\refapp-data
```

## Scenarios

| Scenario | What it does | Expected daemon outcome |
|---|---|---|
| `happy` | `create → process → settle` for N small payments | `state=Success closure=Closed`; `action=ALLOW` decisions |
| `fraud` | Same, but with amounts ≥ $5000 to fresh beneficiaries | Signals `new_beneficiary` + `high_amount` fire; policy matches; `action=CHALLENGE` decisions (observe mode) |
| `fail` | `create → process → fail` | `state=Failed closure=ClosedWithException` |
| `stale` | `create` only, no follow-up | Staleness scanner closes it as `state=Abandoned closure=ClosedWithUncertainty` (1h default) |
| `late` | `create → settle`, then a late `fail` after closure | Lifecycle stays `Success/Closed`; late event classified as `Conflict`, stored on the lifecycle |
| `duplicate` | Two `settle` events for the same payment | First settles; second is a late event after closure |
| `velocity` | Same actor fires many events in a burst | `high_velocity` signal fires once the tracker's rolling-window count crosses its threshold |
| `fanout` | Same actor pays many distinct beneficiaries | `distinct_targets` signal fires once distinct-target count crosses its threshold |
| `stuffing` | Same actor fires many failed login attempts | `credential_stuffing_pattern` signal fires once failed-auth rate crosses its threshold (uses `login.fail` and `outcome=failed` classifiers) |
| `mixed` | Realistic blend: 60% happy, 15% fraud, 15% fail, 10% stale | All of the above |
| `all` | Every scenario, twice | Full spread of state/closure/decision outcomes |

Every scenario accepts `--count=N` and `--seed=N` for reproducibility.

## Synchronous evaluation demo

The simulator fires events and moves on. Real flows sometimes need to *wait*
for the daemon's opinion before proceeding — e.g. rejecting a high-risk
payment. `Runtime::evaluate()` sends one event and returns a `Decision`
object; on any failure it returns a fail-open synthetic Decision so the
caller always gets something back.

```
php refapp/bin/evaluate-demo.php
```

Runs three cases:

1. A happy payment → daemon returns `action=ALLOW`.
2. A high-amount payment to a fresh beneficiary → policy A matches →
   `action=CHALLENGE`, `mode=OBSERVE` (advisory in V1) or `ENFORCE` if
   `policies.json` upgraded that rule.
3. Shows how a caller decides whether to block, log, or allow.

## Model demo

If you want to see `ObservesLifecycle` in action against a Laravel-shaped `Payment` model without spinning up Laravel:

```
php refapp/bin/model-demo.php
```

This runs three concrete Payment lifecycles (create, update to success, update to failed, delete) and prints what got emitted. Verify with the daemon's SQLite as above.

## What the verifier checks

`verify.php` asserts, over whatever the simulator sent:

- ✅ Lifecycles DB exists and has at least one row.
- ✅ Every row has one `tenant_id` (single-tenant sanity for V0).
- ✅ Decisions DB exists.
- ✅ At least one `new_beneficiary` signal fired.
- ℹ️ Reports counts for `high_velocity` and `distinct_targets`; suggests the specific scenario if either is zero.
- ✅ Each decision record has the mandatory `INV-17` fields: `degradation_level`, `inputs_missing`, `coverage_gaps_overlapping`, `explanation`, and `engine_versions` (detector/policy/resolver).

The verifier reads SQLite directly via PDO — no daemon-running required. That means you can Ctrl-C the daemon between simulator and verify, or run them at the same time (WAL mode allows concurrent readers).

## Notes on scope

- **Not a full Laravel install.** A real Laravel app with `composer require athar/shim-laravel` gets the same behaviour plus auto-instrumented HTTP router, queue jobs, outbound HTTP, and Eloquent model events via the `Athar\Adapter\Laravel\ServiceProvider`.
- **No real payment gateway.** All settlement decisions are simulated by emitting the right event type in code. That's the point: no real money moves, no real customer data exists, but the daemon exercises the same code paths it would in production.
- **Not a load test.** Each event has a `usleep(1000)` between phases so the traffic is orderly and easy to follow. Real load testing needs a proper harness — see `docs/RUN.md` for the D13 refapp discussion.
