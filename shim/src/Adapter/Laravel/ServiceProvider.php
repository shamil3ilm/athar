<?php

declare(strict_types=1);

namespace Athar\Adapter\Laravel;

use Athar\Runtime;
use Illuminate\Contracts\Http\Kernel;
use Illuminate\Support\ServiceProvider as LaravelServiceProvider;

/**
 * Laravel auto-instrumentation entry (INT-3).
 *
 * On boot:
 *   1. Enables the runtime (lazy — no daemon connection yet).
 *   2. Registers HttpMiddleware globally so every HTTP request produces a
 *      canonical `http.request` event automatically.
 *
 * The middleware is prepended (highest priority) so it captures BEFORE any
 * app-level middleware can throw, giving us request-start latency measurement
 * from as close to the framework boundary as we can get.
 *
 * INT-2: no controller edits required. INT-5: attach failure degrades capability,
 * never breaks the app.
 */
final class ServiceProvider extends LaravelServiceProvider
{
    public function register(): void
    {
        // PERF-8 / OPS-20: no heavy work at register time.
    }

    public function boot(?Kernel $kernel = null): void
    {
        try {
            Runtime::enable();
            if ($kernel !== null && method_exists($kernel, 'prependMiddleware')) {
                $kernel->prependMiddleware(HttpMiddleware::class);
            }
        } catch (\Throwable $e) {
            @error_log('[athar] Laravel boot failed: ' . $e->getMessage());
        }
    }
}
