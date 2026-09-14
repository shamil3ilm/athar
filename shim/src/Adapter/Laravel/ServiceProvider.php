<?php

declare(strict_types=1);

namespace Athar\Adapter\Laravel;

use Athar\Runtime;
use Illuminate\Contracts\Events\Dispatcher;
use Illuminate\Contracts\Http\Kernel;
use Illuminate\Support\ServiceProvider as LaravelServiceProvider;

/**
 * Laravel auto-instrumentation entry (INT-3).
 *
 * On boot:
 *   1. Enables the runtime (lazy — no daemon connection yet).
 *   2. Registers HttpMiddleware globally (every request → http.request event).
 *   3. Registers QueueSubscriber for JobProcessing / JobProcessed / JobFailed.
 *   4. Registers OutboundHttpSubscriber for Http client ResponseReceived / ConnectionFailed.
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

    public function boot(?Kernel $kernel = null, ?Dispatcher $events = null): void
    {
        try {
            Runtime::enable();

            // HTTP router — every request becomes an http.request event.
            if ($kernel !== null && method_exists($kernel, 'prependMiddleware')) {
                $kernel->prependMiddleware(HttpMiddleware::class);
            }

            // Queue jobs + outbound HTTP — subscribe via the event dispatcher.
            $events = $events ?? ($this->app->bound('events') ? $this->app->make('events') : null);
            if ($events !== null) {
                $this->registerQueueSubscriber($events);
                $this->registerOutboundHttpSubscriber($events);
            }
        } catch (\Throwable $e) {
            @error_log('[athar] Laravel boot failed: ' . $e->getMessage());
        }
    }

    private function registerQueueSubscriber(Dispatcher $events): void
    {
        try {
            $sub = new QueueSubscriber();
            // Fully-qualified event class names; if the framework isn't shipping them
            // (older Laravel versions), $events->listen still accepts arbitrary names —
            // it just won't ever fire.
            $events->listen('Illuminate\\Queue\\Events\\JobProcessing', [$sub, 'onJobProcessing']);
            $events->listen('Illuminate\\Queue\\Events\\JobProcessed', [$sub, 'onJobProcessed']);
            $events->listen('Illuminate\\Queue\\Events\\JobFailed', [$sub, 'onJobFailed']);
        } catch (\Throwable $e) {
            @error_log('[athar] queue subscriber registration failed: ' . $e->getMessage());
        }
    }

    private function registerOutboundHttpSubscriber(Dispatcher $events): void
    {
        try {
            $sub = new OutboundHttpSubscriber();
            $events->listen('Illuminate\\Http\\Client\\Events\\ResponseReceived', [$sub, 'onResponseReceived']);
            $events->listen('Illuminate\\Http\\Client\\Events\\ConnectionFailed', [$sub, 'onConnectionFailed']);
        } catch (\Throwable $e) {
            @error_log('[athar] outbound-http subscriber registration failed: ' . $e->getMessage());
        }
    }
}
