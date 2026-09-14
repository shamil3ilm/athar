<?php

declare(strict_types=1);

namespace Athar\Adapter\Laravel;

use Athar\Runtime;
use Illuminate\Support\ServiceProvider as LaravelServiceProvider;

/**
 * Laravel auto-instrumentation entry (INT-3).
 *
 * Discovered via composer.json's extra.laravel.providers.
 * INT-2: no controller edits required. INT-5: attach failure degrades capability,
 * never breaks the app.
 */
final class ServiceProvider extends LaravelServiceProvider
{
    public function register(): void
    {
        // OPS-20 / PERF-8: no heavy work at register time.
    }

    public function boot(): void
    {
        try {
            Runtime::enable();
            // TODO: wire the framework hooks (router, queue, http-client, auth, DB)
            // via Athar\Shim\Capture once the shim is filled in.
        } catch (\Throwable $e) {
            // INV-15: contain.
            @error_log('[athar] Laravel boot failed: ' . $e->getMessage());
        }
    }
}
