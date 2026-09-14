# athar shim (PHP / Laravel)

The in-process side of the runtime. Per SPEC `INV-11`, this package does the least possible work: capture, redact, hand off. It **MUST NOT** load models, parse policy, open network sockets beyond the local Unix socket to the daemon, or do any disk I/O beyond appending to its bounded buffer (`INV-13`).

## Public API

```php
use Athar\Runtime;

Runtime::enable();                                        // once, at boot
Runtime::context(['business_id' => $x, 'operation' => 'INVOICE_ISSUE']);  // optional enrichment
```

That's it. `INT-1`/`INT-2`: no other developer-facing surface is mandatory.

## Layout

```
src/
  Runtime.php                    Public API (INT-1)
  Contract/
    RuntimeInterface.php         Behaviour under test doubles
  Shim/
    Capture.php                  Hook + capture
    Classify.php                 Data classification (§4.3)
    Redact.php                   C5 drop + denylist (PRI-7, PRI-9)
    Buffer.php                   Bounded ring buffer (INV-12, INV-13)
    Transport.php                Unix-socket writer, non-blocking, framed
    Clock.php                    Wall + monotonic + skew (D14)
  Adapter/
    Laravel/
      ServiceProvider.php        Auto-instrumentation entry (INT-3)
```

## Non-obvious constraints (read before editing)

- **PHP-FPM worker-per-request has no background thread.** The shim must flush at request end. No `pcntl_fork`, no threads. `register_shutdown_function` is the flush point.
- **`INV-15`:** any exception raised inside the shim MUST be caught inside the shim. Nothing propagates into application code. Repeated panics in the same hook disable that hook for the process lifetime.
- **`PERF-8`:** startup overhead ≤ 25 ms. No discovery, no network, no disk I/O at boot. `Runtime::enable()` is lazy — it registers listeners; it does not connect to the daemon.
- **`PRI-7`:** `C5` (secrets) MUST be dropped in this process, before any buffer or socket write. There is no "capture-then-filter" path.

## Build / test

```
composer install
vendor/bin/phpunit
```

Not yet fully implemented; scaffold pending Appendix A step 3.
