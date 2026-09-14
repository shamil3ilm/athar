<?php

declare(strict_types=1);

namespace Athar\Adapter\Laravel;

use Athar\Runtime;
use Athar\Shim\Clock;
use Athar\Shim\Ulid;
use Closure;

/**
 * Automatic HTTP capture (INT-3).
 *
 * Registered as a global middleware by the ServiceProvider. Captures:
 *   - HTTP method
 *   - Route template (framework-normalised, e.g. `/users/{id}` not `/users/42`)
 *   - Concrete route (URL path)
 *   - Response status code
 *   - Latency (µs)
 *
 * Emits ONE canonical `http.request` event per request, at request end (after
 * the response is generated) so status_code is known. Redaction runs before the
 * event enters the buffer — no C5 data (auth headers, cookies) is captured.
 *
 * INV-15: this middleware MUST NEVER break the application. Any exception is
 * caught, logged, and the request continues unmodified.
 */
final class HttpMiddleware
{
    public function handle($request, Closure $next)
    {
        // Push a per-request correlation id so every event emitted during
        // this request (the http.request record below, plus anything the
        // application code emits via observeEvent/observePayment) shares
        // the same causality.correlation_id — enabling per-request grouping
        // in the audit log without any controller-side plumbing.
        $correlationId = 'req_' . Ulid::generate();
        Runtime::pushCorrelation($correlationId);
        try {
            // Don't let a malformed request break the shim; and don't let the
            // shim block the response either. Time the actual work only, not
            // the capture.
            $startedUs = self::microtimeUs();
            $response = $next($request);
            try {
                $latencyUs = self::microtimeUs() - $startedUs;
                $this->emit($request, $response, $latencyUs);
            } catch (\Throwable $e) {
                @error_log('[athar] HttpMiddleware capture failed: ' . $e->getMessage());
            }
            return $response;
        } finally {
            // Pop even if $next threw; the caller's exception propagates
            // normally, and we don't leak correlation state across requests
            // in long-running PHP workers (e.g. Octane, RoadRunner, Swoole).
            Runtime::popCorrelation();
        }
    }

    /**
     * Public helper so a non-Laravel PSR-15 adapter (or a test) can build the
     * same event shape.
     */
    public function emit($request, $response, int $latencyUs): void
    {
        if (!Runtime::isEnabled()) return;
        $factory = Runtime::factory();
        if ($factory === null) return;

        $method  = self::extract($request, 'method',        fn($r) => strtoupper($r->method()));
        $path    = self::extract($request, 'path',          fn($r) => '/' . ltrim($r->path(), '/'));
        $routeTpl = self::extract($request, 'route_template',
            fn($r) => method_exists($r, 'route') && $r->route()
                ? (string) $r->route()->uri()
                : $path);
        $status  = self::extract($response, 'status', fn($x) => (int) $x->status());

        // Some HTTP inputs are safe (path template, method, status). Query and body
        // params are NOT captured automatically — that's opt-in via context() to
        // stay within PRI-8 ("capture is opt-in per field, never capture everything").
        $event = $factory->httpRequest($method ?? 'GET', $routeTpl ?? $path ?? '/', [
            'application_id' => Runtime::currentContext()['application_id'] ?? 'laravel',
            'endpoint_id'    => self::endpointId($method ?? 'GET', $routeTpl ?? $path ?? '/'),
            'route'          => $path,
            'customer_correlation_ids' => Runtime::currentContext()['customer_correlation_ids'] ?? [],
            'redacted_fields' => ['headers.authorization', 'headers.cookie', 'body.*'],
        ]);
        // Enrich with status + latency in `data` (values are C0/C1 — safe).
        $event['data'] = [
            'status_code' => $status ?? 0,
            'latency_us'  => $latencyUs,
        ];
        Runtime::observe($event);
    }

    private static function extract($obj, string $label, callable $fn)
    {
        try {
            return $fn($obj);
        } catch (\Throwable $e) {
            @error_log("[athar] HttpMiddleware could not extract {$label}: " . $e->getMessage());
            return null;
        }
    }

    /**
     * Stable per-endpoint identifier: hash of "METHOD route_template".
     * Not tenant-scoped — that's fine, endpoint_id is C1-technical.
     */
    private static function endpointId(string $method, string $route): string
    {
        return 'ep_' . substr(hash('sha256', strtoupper($method) . ' ' . $route), 0, 16);
    }

    private static function microtimeUs(): int
    {
        return (int) (microtime(true) * 1_000_000);
    }
}
