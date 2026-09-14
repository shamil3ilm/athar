<?php

declare(strict_types=1);

namespace Athar\Adapter\Laravel;

use Athar\Runtime;
use Athar\Shim\Clock;

/**
 * Outbound HTTP client auto-instrumentation (INT-3).
 *
 * Subscribes to Laravel's Http client events:
 *
 *   ResponseReceived → outbound.request.completed
 *   ConnectionFailed → outbound.request.failed
 *
 * PRIVACY POSTURE (PRI-8, PRI-9):
 *   Only the HTTP method and the host (scheme + host + optional port) are
 *   captured by default. The path, query string, headers, and body are NOT
 *   captured — a path like `/v1/customers/{secret_id}/charges/{payment_id}`
 *   would leak IDs that identify specific records.
 *
 * `resource.id` uses a hash of (method + host) so the same processor's calls
 * correlate together without carrying the URL directly.
 *
 * INV-15: every method catches Throwable; a shim failure never breaks the caller's
 * HTTP request.
 */
final class OutboundHttpSubscriber
{
    public function onResponseReceived($event): void
    {
        $status = null;
        $response = property_exists($event, 'response') ? $event->response : null;
        if (is_object($response) && method_exists($response, 'status')) {
            try { $status = (int) $response->status(); } catch (\Throwable) {}
        }
        $this->emit('outbound.request.completed', $event, [
            'status_code' => $status,
            'outcome' => 'response',
        ]);
    }

    public function onConnectionFailed($event): void
    {
        $this->emit('outbound.request.failed', $event, [
            'outcome' => 'connection_failed',
        ]);
    }

    private function emit(string $eventType, $event, array $extraData): void
    {
        if (!Runtime::isEnabled()) return;
        $factory = Runtime::factory();
        if ($factory === null) return;
        try {
            $request = property_exists($event, 'request') ? $event->request : null;
            if ($request === null || !is_object($request)) return;

            $method = self::safeCall($request, 'method');
            $url = self::safeCall($request, 'url');
            if (!is_string($method) || !is_string($url)) return;
            $method = strtoupper($method);

            $host = self::hostOnly($url);
            if ($host === null) return;

            $endpointId = 'ext_' . substr(hash('sha256', $method . ' ' . $host), 0, 16);
            $resourceId = $endpointId; // one lifecycle per (method, host) unless caller enriches

            $now = Clock::nowRfc3339();
            $canonical = $factory->businessEvent(
                eventType: $eventType,
                resourceId: $resourceId,
                resourceType: 'external_call',
                data: array_filter($extraData, fn($v) => $v !== null),
                context: [
                    'trigger' => 'EXTERNAL_PROCESSOR',
                    'source'  => 'HTTP',
                    'origin'  => 'SYSTEM',
                ],
            );
            $canonical['event_type'] = $eventType;
            $canonical['timestamp'] = $now;
            $canonical['received_at'] = $now;
            $canonical['entry_point'] = [
                'type' => 'INTERNAL',
                'endpoint_id' => $endpointId,
                'method' => $method,
                'route' => $host,
                'route_template' => $host,
            ];
            $canonical['coverage']['redacted_fields'] = ['url.path', 'url.query', 'headers.*', 'body'];
            Runtime::observe($canonical);
        } catch (\Throwable $e) {
            @error_log('[athar] OutboundHttpSubscriber emit failed: ' . $e->getMessage());
        }
    }

    /** Extract scheme+host[:port] from a URL. Returns null on parse failure. */
    public static function hostOnly(string $url): ?string
    {
        $parts = @parse_url($url);
        if (!is_array($parts)) return null;
        $scheme = $parts['scheme'] ?? 'https';
        $host = $parts['host'] ?? null;
        if (!is_string($host) || $host === '') return null;
        $port = isset($parts['port']) ? ':' . $parts['port'] : '';
        return "{$scheme}://{$host}{$port}";
    }

    private static function safeCall($obj, string $method)
    {
        if (!method_exists($obj, $method)) return null;
        try {
            return $obj->$method();
        } catch (\Throwable $e) {
            return null;
        }
    }
}
