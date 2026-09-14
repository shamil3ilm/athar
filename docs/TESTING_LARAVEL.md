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

## Step 5 — what's auto-captured, by surface

As of this pass, four Laravel surfaces are auto-captured without any controller edits:

| Surface | Event types emitted | Correlation key |
|---|---|---|
| HTTP router | `http.request` | `endpoint_id` = hash(method + route_template) |
| Eloquent models with `ObservesLifecycle` | `{type}.create` / `.process` / `.settle` / `.fail` / `.cancel` / `.reverse` | `resource.id` = `{prefix}_{primary_key}` |
| Queue jobs (Laravel Queue) | `queue.consume` / `queue.complete` / `queue.fail` | `resource.id` = `job_{uuid}` |
| Outbound HTTP (Laravel Http client) | `outbound.request.completed` / `outbound.request.failed` | `resource.id` = hash(method + host) |

The queue subscriber captures on Laravel's `JobProcessing`/`JobProcessed`/`JobFailed`
events; the outbound HTTP subscriber uses `ResponseReceived`/`ConnectionFailed`
from the `Illuminate\Http\Client\Events` namespace (Laravel 10+).

Privacy posture for the outbound HTTP subscriber: **only method + scheme + host + port**
are captured. Path, query, headers, and body are NOT captured — a URL like
`https://api.stripe.com/v1/charges/ch_SECRET_ID?token=xyz` yields
`https://api.stripe.com`, not the sensitive path or query. Verified by
`subscribers-test.php` (leak-detection assertion).

## Step 5a — every HTTP request is captured automatically

As of this pass, the Laravel `ServiceProvider` registers a global `HttpMiddleware`
that emits a canonical `http.request` event per request:

- method
- route template (framework-normalised, e.g. `/users/{id}` not `/users/42`)
- concrete route
- response status code
- request latency (µs)

No auth headers, cookies, or body params are captured (PRI-8). Redaction is
declared explicitly on `coverage.redacted_fields`.

You don't have to touch controller code for this to work. `Runtime::enable()`
in the `ServiceProvider::boot` (which package discovery does for you) is enough.

## Step 5b — one-line auto-instrumentation for your Payment model

If your Laravel app already has a `Payment` Eloquent model (nearly always the
case), you can auto-emit the entire lifecycle from model events. One trait:

```php
use Athar\Support\ObservesLifecycle;

class Payment extends Model
{
    use ObservesLifecycle;

    protected string $atharLifecycleType = 'payment';
    // Optional:
    // protected string $atharStateField    = 'status';       // default
    // protected string $atharResourcePrefix = 'pay';         // default = $atharLifecycleType
    // protected array  $atharTerminalMap   = ['refunded' => 'reverse', 'declined' => 'fail'];
}
```

That's it. Now:

| Eloquent event | Model state transition | Emitted event |
|---|---|---|
| `created` | any | `payment.create` |
| `updated` | `status → success/succeeded/completed/settled/paid` | `payment.settle` |
| `updated` | `status → failed/error/declined` | `payment.fail` |
| `updated` | `status → cancelled` | `payment.cancel` |
| `updated` | `status → refunded/reversed` | `payment.reverse` |
| `updated` | no terminal transition | `payment.process` |
| `deleted` | any | `payment.cancel` |

`resource.id` is set to `{prefix}_{primary_key}`, so `Payment::find(42)` becomes
`pay_42` on every event — that's what the daemon uses to correlate.

By default the shim forwards only `amount` and `currency` in the event payload
(both C1 fields). Override `atharObservableData(): array` on the model to
add other business fields — but remember `PRI-9` still applies: any field name
matching the denylist is dropped before the event leaves your process.

`INV-15` verified: if a model attribute throws, the trait catches; your save
completes normally.

## Step 5c — emit lifecycle events manually from controllers

If you'd rather emit events explicitly (or in addition to the model trait),
call the helper directly:

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
