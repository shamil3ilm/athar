# Testing athar locally against a Laravel payments app

This document is how to stand up the current V0 stack against a real Laravel application (Windows/laragon or macOS/Linux) and verify the invariants it currently supports.

## What this will and won't demonstrate

**Will:**
- Events emitted from your Laravel controllers/jobs land in the daemon.
- No card numbers, CVVs, passwords, or auth tokens survive the shim on disk (`PRI-7`).
- Every observed event has a signed, hash-chained audit commitment; tampering with any record is detected with its exact index (`SEC-13`).
- `payment.create` → `payment.process` → `payment.settle` events correlated by `resource.id` assemble into one lifecycle that closes with SUCCESS (`MOD-24`–`MOD-26`).
- A payment that never settles is auto-closed as `CLOSED_WITH_UNCERTAINTY` within its threshold (`MOD-25`).
- A late `payment.fail` after settlement is classified as `CONFLICT` without reopening (`MOD-27`).

**Will not:**
- Auto-instrumentation: you must call `Runtime::observePayment(...)` at each lifecycle point. The Laravel `ServiceProvider` is a stub — it does not yet hook the router, queue, or HTTP client automatically.
- Query surface: no CLI or HTTP endpoint currently exposes lifecycles from outside the daemon process.
- Fraud detection: signals, policy, and decision records are not implemented.
- Persistence across daemon restart: lifecycles live in memory (evidence and audit chain persist; the projection does not).

## Prerequisites

- **Rust** 1.75+ (`rustup default stable`).
- **PHP** 8.1+ (Laragon's PHP 8.3 is fine).
- **Composer**.
- A **Laravel app** to point the shim at. Any 10.x/11.x will do; the following assumes you already have one.

## Step 1 — build the daemon

```
cd C:\athar\daemon
cargo build --release
```

This produces `target/release/athar-daemon` (or `.exe` on Windows) and `target/release/athar`.

## Step 2 — run the daemon

Choose a data directory *outside* your app:

```
$env:ATHAR_DATA_DIR="C:\athar\v0-data"        # PowerShell
$env:ATHAR_TENANT_ID="tnt_yourco"             # informational; will move to a header once handshake exists
cargo run --release --bin athar-daemon
```

Expected startup log includes:
- `athar-daemon starting`
- `storage ready`
- a warning about the file-backed dev signing key
- `ingest listening addr=127.0.0.1:11223`

Leave this running. `Ctrl-C` shuts down cleanly.

## Step 3 — install the shim in your Laravel app

The shim isn't published yet. Add it as a **path repository** in your Laravel app's `composer.json`:

```json
{
  "repositories": [
    { "type": "path", "url": "C:/athar/shim" }
  ],
  "require": {
    "athar/shim-laravel": "*"
  }
}
```

Then:

```
composer require athar/shim-laravel:*
```

Laravel's package discovery picks up `Athar\Adapter\Laravel\ServiceProvider` automatically. It calls `Runtime::enable()` in `boot()`. If you'd rather be explicit, remove the auto-discovery and call `Runtime::enable()` in your `AppServiceProvider::boot()`.

## Step 4 — configure the shim

Environment variables (add to your `.env`):

```
ATHAR_TENANT_ID=tnt_yourco
ATHAR_DAEMON_HOST=127.0.0.1
ATHAR_DAEMON_PORT=11223
```

## Step 5 — emit lifecycle events from your payment controller

Wherever your app touches a payment, add one line per lifecycle point:

```php
use Athar\Runtime;

// When you create a payment record
Runtime::observePayment('payment.create', "pay_{$payment->id}", [
    'amount'   => $payment->amount,
    'currency' => $payment->currency,
]);

// When you hand it to your gateway
Runtime::observePayment('payment.process', "pay_{$payment->id}");

// When the gateway confirms settlement (webhook handler)
Runtime::observePayment('payment.settle', "pay_{$payment->id}", [
    'gateway_ref' => $webhook->id,
]);

// If it fails
Runtime::observePayment('payment.fail', "pay_{$payment->id}", [
    'reason' => $error,
]);
```

Rules of the road:
- The `$paymentId` must be **stable across all events for the same payment.** That's how correlation works (tier 4, `resource.id`).
- You may include secrets in `$data` — the shim drops them before anything crosses a process boundary. Do NOT rely on this as your only defence; it's a safety net, not a substitute for not passing secrets around unnecessarily.
- `observePayment` is safe under all error conditions — it will never throw into your caller.

## Step 6 — hit an endpoint

Trigger a real payment through your app. Watch the daemon logs; you should see:
- `frame decoded` (at `RUST_LOG=debug`)
- `audit segment flushed` when 1000 records accumulate (or on graceful shutdown)

## Step 7 — verify the audit trail

Stop the daemon (`Ctrl-C`). Then:

```
cd C:\athar\daemon
cargo run --release --bin athar -- audit verify C:\athar\v0-data\audit\segments
```

Expected output:

```
OK: N segment(s), M record(s) verified
```

Now try tampering: open any `.audit.json` file and change a `payload_commitment` hex character. Re-run `athar audit verify`. It reports the exact record index of the break.

## Step 8 — verify redaction

Grep the raw evidence log for anything sensitive:

```powershell
Get-ChildItem C:\athar\v0-data\evidence -Filter *.seg | ForEach-Object {
    Get-Content $_.FullName -Raw
} | Select-String -Pattern "password|cvv|4111111111111111"
```

Should return nothing, even if your test payment included those in the `$data` array — the shim's classification denylist drops them before they leave your PHP process.

## Step 9 — verify a lifecycle closes

To watch a complete lifecycle assemble, use small payment amounts and:

1. Trigger `payment.create` + `payment.process` + `payment.settle` in quick succession.
2. Read the evidence log — you'll see three canonical events, all sharing `resource.id`.
3. Since there's no CLI yet, this pass verifies the **wire**; internal lifecycle state is verifiable only from inside the daemon process. For visible confirmation of the state machine, add a temporary `tracing::info!` to `athar-daemon/src/ingest.rs::ingest_one` around the `engine.apply` call — you'll see `ApplyOutcome::Created`, `Updated`, and finally `Updated { state: Success, closure: Closed }`.

## Step 10 — verify staleness closure

Emit only a `payment.create` and don't follow it up. Configure the daemon with a short scan interval so you don't have to wait an hour:

```
ATHAR_STALENESS_SCAN_INTERVAL_SECS=10
```

Wait past the type's staleness threshold (Payment: 1 hour by default — for testing, either wait, or edit `staleness_for` in `athar-lifecycle/src/types.rs` to `60 * 1000` and rebuild). The daemon logs `lifecycles closed with uncertainty count=1`.

## Step 11 — verify late-event handling

1. Emit `payment.create` + `payment.settle` → lifecycle closes with SUCCESS.
2. Emit `payment.fail` for the same `resource.id`.
3. Daemon logs: `late event after closure ... class=Conflict`.
4. The lifecycle stays SUCCESS/Closed. Prior evidence is untouched (`INV-9`).

## What to check next

If all the above passes, you have a working ingest pipeline with cryptographic tamper-evidence, correct lifecycle correlation, and provable redaction — running against a real Laravel app on real payment flows. That's the foundation.

To make this useful commercially, next in scope:
- Auto-instrumentation of Laravel router / queue / HTTP client (so callers don't call `observePayment` manually).
- SQLite state-store persistence for lifecycles (so daemon restart doesn't wipe the projection).
- A CLI `athar lifecycle list --open` / `athar lifecycle show <id>` so an operator can see state without reading logs.
- Signals + policy + decision records (the fraud-detection wedge).
- Reference application (`bench/refapp-laravel`) so perf numbers are reproducible.

## Reporting an issue

If any of steps 6–11 misbehave in ways this document doesn't describe:
- Run with `RUST_LOG=debug` and capture the daemon log around the misbehaviour.
- Include the emitted event's `event_id` and the exact code that emitted it.
- Do NOT include the contents of the evidence log or audit segments — they may contain (redacted, but still customer-adjacent) event bodies.
