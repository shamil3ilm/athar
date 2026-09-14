# athar shim (PHP / Laravel)

The in-process side of the runtime. Per SPEC `INV-11`, this package does the
least possible work: capture, redact, hand off to the local daemon. It **MUST
NOT** load models, parse policy, or do any disk I/O beyond appending to its
bounded buffer (`INV-13`) plus a small on-disk spool for loss-record recovery
when the daemon is unreachable.

## Public API

```php
use Athar\Runtime;

Runtime::enable();                          // once, at boot
Runtime::ping();                            // is the daemon reachable? (bool)
Runtime::context(['operation' => 'CHECKOUT']);  // optional enrichment

// Fire-and-forget telemetry.
Runtime::observeEvent(
    eventType:      'payment.create',
    resourceType:   'payment',
    resourceId:     $payment->id,
    data:           ['amount' => $payment->amount],
    beneficiaryId:  $payment->recipient_id,   // optional
    actorId:        $user->id,                // optional
);

// Domain wrapper (identical, but with resourceType hardcoded).
Runtime::observePayment('payment.settle', $payment->id, ['gateway_ref' => 'gw_xxx']);

// Synchronous decision — blocks until daemon responds or deadline fires.
$decision = Runtime::evaluate($event, deadlineMs: 25);
if ($decision->isEnforced() && $decision->wouldRestrict()) { /* block */ }
```

That's the full public surface. `INT-1` / `INT-2`: no additional
developer-facing API is required.

## Layout

```
src/
  Runtime.php                Public API (INT-1)
  Decision.php               Return value from Runtime::evaluate()
  RuntimeConfig.php          Env-var → config
  Contract/
    RuntimeInterface.php     For test doubles
  Shim/
    Buffer.php               Bounded ring buffer (INV-12, INV-13)
    Classify.php             Data classification (§4.3)
    Clock.php                Wall + monotonic + skew (D14)
    EventFactory.php         Canonical event construction
    Redact.php               C5 drop + denylist (PRI-7, PRI-9)
    Spool.php                On-disk loss records when daemon is unreachable
    Transport.php            TCP-loopback writer, length-framed, non-blocking
    Ulid.php                 Event / lifecycle ID generation
  Support/
    ObservesLifecycle.php    Eloquent trait — auto-emits on save/update/delete
  Adapter/
    Laravel/
      ServiceProvider.php    Auto-registered via package discovery
      HttpMiddleware.php     Optional per-request boot
      OutboundHttpSubscriber.php
      QueueSubscriber.php
```

## Non-obvious constraints (read before editing)

- **PHP-FPM worker-per-request has no background thread.** The shim must
  flush at request end. No `pcntl_fork`, no threads. `register_shutdown_function`
  is the flush point.
- **`INV-15`**: any exception raised inside the shim MUST be caught inside
  the shim. Nothing propagates into application code. Every public method
  wraps its body in `try/catch` and writes to `error_log()` on failure.
- **`PERF-8`**: startup overhead ≤ 25 ms. No discovery, no network, no disk
  I/O at boot. `Runtime::enable()` is lazy — it registers listeners; it does
  not connect to the daemon.
- **`PRI-7`**: `C5` (secrets) MUST be dropped in this process, before any
  buffer or socket write. See `Shim/Redact.php`.

## Install into an existing project

See `docs/INTEGRATE.md` for a step-by-step 5-minute integration guide, or
`refapp/README.md` for a self-contained end-to-end runnable demo.

Short version:

```json
// your app's composer.json
{
    "repositories": [
        { "type": "path", "url": "/absolute/path/to/athar/shim", "options": { "symlink": true } }
    ],
    "require": { "athar/shim-laravel": "@dev" }
}
```

```
composer update athar/shim-laravel
```

Laravel picks up the ServiceProvider automatically.

## Test

```
composer install
vendor/bin/phpunit
```

Also see `refapp/bin/simulator.php` and `refapp/bin/verify.php` for
end-to-end exercising the shim against a live daemon.
