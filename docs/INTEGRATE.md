# Integrating athar into a real project (local test)

This is the ~5-minute path to instrument an EXISTING PHP or Laravel app with
athar on your dev machine. No production deployment — just enough to prove
the daemon sees your events, correlates lifecycles, and produces decisions
your app can act on.

## Prerequisites

- PHP 8.1+ with `ext-sockets` and `ext-pdo_sqlite`
- A working Rust toolchain to build the daemon (`cargo` 1.85+), OR a prebuilt
  binary from `daemon/target/release/athar-daemon`
- Your existing app, running locally

## 1. Start the daemon

```powershell
# Windows
cd C:\athar\daemon
$env:ATHAR_DATA_DIR = "C:\athar-test-data"
cargo run --release --bin athar-daemon
```

```sh
# Linux/macOS
cd athar/daemon
ATHAR_DATA_DIR=./athar-test-data cargo run --release --bin athar-daemon
```

The daemon binds `127.0.0.1:11223` by default. You should see:

```
INFO  athar_daemon: athar-daemon starting
INFO  athar_daemon::ingest: ingest listening addr=127.0.0.1:11223
INFO  athar_daemon::policy_reload: policy live-reload enabled interval_secs=5
```

Leave it running in a terminal.

## 2. Add the shim to your app

The shim isn't published to Packagist yet, so you point Composer at your
local `athar/shim/` directory.

In your app's `composer.json`:

```json
{
    "repositories": [
        {
            "type": "path",
            "url": "/absolute/path/to/athar/shim",
            "options": { "symlink": true }
        }
    ],
    "require": {
        "athar/shim-laravel": "@dev"
    }
}
```

Then:

```
composer update athar/shim-laravel
```

Laravel's package discovery picks up `Athar\Adapter\Laravel\ServiceProvider`
automatically — nothing to add to `config/app.php`.

For plain (non-Laravel) PHP, autoloading via `vendor/autoload.php` still
works. Just call `Athar\Runtime::enable()` yourself at boot.

## 3. Configure via environment

The shim reads these on `Runtime::enable()`:

```
ATHAR_TENANT_ID=tnt_myapp          # a stable id for your app; groups events per tenant
ATHAR_DAEMON_HOST=127.0.0.1
ATHAR_DAEMON_PORT=11223
```

Put them in `.env` for Laravel or `$_ENV` / `putenv()` for plain PHP.

## 4. Verify the shim can reach the daemon

Anywhere in your app (health check, artisan tinker, a debug route):

```php
use Athar\Runtime;

Runtime::enable();
var_dump(Runtime::isEnabled());   // true
var_dump(Runtime::ping());        // true if daemon is reachable
```

If `ping()` returns false: check the daemon is running, the port isn't
firewalled to localhost only (it shouldn't be — it binds loopback), and
that `ATHAR_DAEMON_PORT` matches.

## 5. Instrument a flow

### Payment flow (any e-commerce / financial app)

```php
use Athar\Runtime;

// When the customer submits a payment:
Runtime::observePayment(
    'payment.create',
    $payment->id,                             // your ID
    data: ['amount' => $payment->amount, 'currency' => $payment->currency],
    beneficiaryId: $payment->recipient_id,    // enables the DistinctTargets signal
    actorId: $request->user()->id,            // enables per-user velocity signals
);

// After gateway response:
Runtime::observePayment(
    $result->success ? 'payment.settle' : 'payment.fail',
    $payment->id,
    data: ['gateway_ref' => $result->reference, 'reason' => $result->failureReason],
);
```

### Login / auth flow (unlocks credential_stuffing signal)

```php
Runtime::observeEvent(
    $ok ? 'login.success' : 'login.fail',
    'login',                                  // resourceType
    'login_' . bin2hex(random_bytes(6)),      // unique per attempt
    ['outcome' => $ok ? 'ok' : 'failed', 'reason' => $reason],
    actorId: $email,                          // OR IP address hash — pick a stable identifier
);
```

### Any Eloquent model (auto-instrument via trait)

```php
// app/Models/Invoice.php
use Athar\Support\ObservesLifecycle;

class Invoice extends Model
{
    use ObservesLifecycle;

    // Optional: override the emitted event names.
    protected static function atharEventPrefix(): string { return 'invoice'; }
}
```

Now every `$invoice->save()`, update, and delete emits a lifecycle event
without any further code in your controllers.

### Synchronous decisions (block if policy says so)

For flows that need to gate on the daemon's opinion BEFORE proceeding:

```php
$event = Runtime::factory()->businessEvent(
    'payment.create', $payment->id, 'payment',
    ['amount' => $payment->amount],
    ['beneficiary_id' => $payment->recipient_id, 'actor_id' => $user->id],
);
$decision = Runtime::evaluate($event, deadlineMs: 25);

if ($decision->isEnforced() && $decision->wouldRestrict()) {
    // Policy is in ENFORCE mode + wants to block. Respect it.
    return response()->json(['error' => 'PAYMENT_BLOCKED', 'reasons' => $decision->reasonCodes], 403);
}
// Otherwise proceed. OBSERVE-mode challenges are advisory; log them if you like.
```

Timeout (`deadlineMs`) and any daemon failure both return a fail-open
`Decision` with `outcomeReason` telling you why. You always get a Decision
back — the call never throws.

## 6. Verify events are being processed

For a live snapshot from a running daemon (pressure level, pending drops,
totals) that doesn't require read access to the SQLite files:

```sh
athar status
# --> athar daemon @ 127.0.0.1:11223
#       version          : 0.1.0
#       pressure_level   : L0
#       pending drops    : frames=0  bytes=0
#       lifecycles       : total=42  open=3
#       decisions        : 67
#       signals          : 104
```

`athar status --json` for machine-readable output (feed into your monitoring).

For an end-to-end install health check (audit chain valid, DBs open, policies
parseable, daemon reachable, all in one shot):

```sh
athar doctor ./athar-test-data
athar doctor ./athar-test-data --deep    # also round-trips the daemon's status endpoint
```

`--deep` adds a live query against the daemon and validates the response
shape — the difference between "daemon is accepting TCP connections" and
"daemon is actively serving." Useful for detecting a hung ingest task
that a bare TCP connect wouldn't catch.



Exits 0 if every check passes. Sample output:

```
athar doctor — checking install at ./athar-test-data

  ok    data-dir exists
  ok    audit segment store opens
  ok    audit chain verifies to genesis
  ok    lifecycles DB opens  (total=42 open=3)
  ok    decisions DB opens  (decisions=67 signals=104)
  ok    policies.json is valid
  ok    daemon reachable at 127.0.0.1:11223

DOCTOR: OK
```

For per-record inspection, the specialised verbs work while your app is
running (WAL mode allows concurrent readers):

```sh
athar decision recent ./athar-test-data/state/decisions.db --limit 10
athar lifecycle list ./athar-test-data/state/lifecycles.db
athar audit verify ./athar-test-data/audit
```

You should see one lifecycle per business object and one decision per
event. Every decision has an `explanation` field you can read directly.

## 7. Tune policies without restarting the daemon

Create `./athar-test-data/config/policies.json`:

```json
{
    "policies": {
        "high_amount_new_beneficiary": { "enabled": true, "mode": "ENFORCE" },
        "credential_stuffing":         { "enabled": true, "mode": "CHALLENGE" }
    },
    "signals": {
        "high_amount_floor": 10000.0
    }
}
```

The daemon picks it up within `ATHAR_POLICY_RELOAD_INTERVAL_SECS` (default
5s). You'll see:

```
INFO  athar_daemon::policy_reload: policy config reloaded ...
```

`athar policy show ./athar-test-data` renders the effective config plus
warns on suspicious settings (all rules disabled, any rule in ENFORCE).

## What "working" looks like

By the time you've completed steps 1–6:

- `Runtime::ping()` returns true on every request
- Every business action in your app produces one lifecycle row and at
  least one decision row
- Fraud-shaped events (high amount + fresh beneficiary, or a burst of
  failed logins) produce `action=CHALLENGE` decisions with populated
  `reason_codes`
- `athar audit verify` shows all segments hash-chain to genesis
- Editing `policies.json` while the daemon is running changes the next
  decision without a restart

If any of those don't hold, check:
- `RUST_LOG=debug` on the daemon to see per-frame processing
- `error_log` output (Laravel's `storage/logs/laravel.log` or PHP's
  configured error_log) — the shim writes there on failures
- Your PHP has `ext-sockets` and `ext-pdo_sqlite` compiled in

## Known rough edges (V0 / for local test only)

- **No Composer package** on Packagist — path repo is the current install
  path
- **No shim/daemon version negotiation** — mismatch is silent; keep both
  from the same commit
- **Signal thresholds require restart** — only `policies` reloads live
- **No TLS, no rate limit on the ingest port** — loopback bind only
  protects you here
- **No `/metrics` endpoint** — health is via `ping()` + reading state
  DBs directly

These are Stage 2 hardening items, not blockers for local integration
testing.
