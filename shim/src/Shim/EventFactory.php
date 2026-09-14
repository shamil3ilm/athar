<?php

declare(strict_types=1);

namespace Athar\Shim;

/**
 * Build canonical events from context (SPEC §5.4, MOD-6).
 *
 * The shim does not correlate, resolve identity, or bind lifecycles. It emits
 * the minimum: what was observed, when, by whom (per unverified assertion), with
 * `resolution: UNKNOWN` on every actor role by default. The daemon does the rest.
 */
final class EventFactory
{
    public function __construct(
        private readonly string $tenantId,
        private readonly string $adapter = 'php-shim@0.1.0',
    ) {}

    /**
     * Build an HTTP-observation event.
     *
     * @param array<string,mixed> $context
     */
    public function httpRequest(
        string $method,
        string $routeTemplate,
        array $context = [],
    ): array {
        $now = Clock::nowRfc3339();
        return [
            'schema_version' => '1.0',
            'event_id'       => Ulid::generate(),
            'event_type'     => 'http.request',
            'tenant_id'      => $this->tenantId,
            'timestamp'      => $now,
            'received_at'    => $now, // shim-side receipt; daemon overwrites with its own ingest time
            'clock' => [
                'source' => 'shim',
                'skew_estimate_ms' => Clock::skewEstimateMs(),
                'monotonic_seq' => Clock::nextSeq(),
            ],
            'entry_point' => [
                'type' => 'HTTP_ENDPOINT',
                'endpoint_id' => $context['endpoint_id'] ?? null,
                'method' => strtoupper($method),
                'route' => $context['route'] ?? $routeTemplate,
                'route_template' => $routeTemplate,
            ],
            'technical_context' => array_filter([
                'application_id' => $context['application_id'] ?? null,
                'application_version' => $context['application_version'] ?? null,
                'platform' => 'php',
                'service_id' => $context['service_id'] ?? null,
                'environment' => $context['environment'] ?? null,
            ], fn($v) => $v !== null),
            'provenance' => [
                'origin' => 'UNKNOWN',
                'trigger' => 'API_REQUEST',
                'source' => 'HTTP',
                'producer' => $context['producer'] ?? null,
                'authority' => null,
            ],
            'truth' => [
                'stage' => 'OBSERVED',
                'asserted_by' => 'shim',
                'adapter' => $this->adapter,
            ],
            'trust' => [
                'level' => 'UNKNOWN',
                'confidence' => 0.0,
                'calibration' => 'NOMINAL_UNVALIDATED',
                'factors' => [],
            ],
            'causality' => [
                'parent_event_id' => null,
                'causation_id' => null,
                'correlation_id' => null,
                'customer_correlation_ids' => (object) ($context['customer_correlation_ids'] ?? []),
            ],
            'coverage' => [
                'complete' => true,
                'shed' => [],
                'redacted_fields' => $context['redacted_fields'] ?? [],
                'degradation_level' => 'L0',
            ],
        ];
    }

    /**
     * Build a business-domain event with a resource binding — the shape the
     * daemon's lifecycle engine expects.
     *
     * `$resourceType` is required and identifies the domain of the resource
     * (payment, invoice, order, refund, subscription, login, ...). The daemon's
     * lifecycle correlator groups events by (tenant, resource_type, resource_id).
     *
     * Examples:
     *   $factory->businessEvent(
     *       eventType: 'payment.create',
     *       resourceId: "pay_{$payment->id}",
     *       resourceType: 'payment',
     *       data: ['amount' => $payment->amount, 'currency' => 'AED'],
     *   );
     *
     *   $factory->businessEvent(
     *       eventType: 'invoice.issue',
     *       resourceId: "inv_{$invoice->id}",
     *       resourceType: 'invoice',
     *       data: ['total' => $invoice->total, 'currency' => 'AED'],
     *   );
     *
     *   $factory->businessEvent(
     *       eventType: 'login.attempt',
     *       resourceId: "login_{$attempt->id}",
     *       resourceType: 'login',
     *       data: ['outcome' => 'failed'],
     *   );
     *
     * @param array<string,mixed> $data
     * @param array<string,mixed> $context
     */
    public function businessEvent(
        string $eventType,
        string $resourceId,
        string $resourceType,
        array $data = [],
        array $context = [],
    ): array {
        $now = Clock::nowRfc3339();
        $namespace = ($context['namespace'] ?? "{$this->tenantId}/{$resourceType}s");
        $event = [
            'schema_version' => '1.0',
            'event_id'       => Ulid::generate(),
            'event_type'     => $eventType,
            'tenant_id'      => $this->tenantId,
            'timestamp'      => $now,
            'received_at'    => $now,
            'clock' => [
                'source' => 'shim',
                'skew_estimate_ms' => Clock::skewEstimateMs(),
                'monotonic_seq' => Clock::nextSeq(),
            ],
            'resource' => [
                'id' => $resourceId,
                'type' => $resourceType,
                'namespace' => $namespace,
            ],
            'operation' => isset($context['operation_id']) || isset($context['operation_type']) ? [
                'operation_id' => $context['operation_id'] ?? null,
                'type' => $context['operation_type'] ?? strtoupper(str_replace('.', '_', $eventType)),
                'inference' => 'EXPLICIT',
                'confidence' => 1.0,
                'calibration' => 'NOMINAL_UNVALIDATED',
            ] : null,
            'provenance' => [
                'origin'   => $context['origin']   ?? 'HUMAN',
                'trigger'  => $context['trigger']  ?? 'API_REQUEST',
                'source'   => $context['source']   ?? 'HTTP',
                'producer' => $context['producer'] ?? null,
                'authority'=> $context['authority']?? null,
            ],
            'truth' => [
                'stage' => 'OBSERVED',
                'asserted_by' => 'shim',
                'adapter' => $this->adapter,
            ],
            'trust' => [
                'level' => 'UNKNOWN',
                'confidence' => 0.0,
                'calibration' => 'NOMINAL_UNVALIDATED',
                'factors' => [],
            ],
            'causality' => [
                'parent_event_id' => $context['parent_event_id'] ?? null,
                'causation_id'    => $context['causation_id']    ?? null,
                'correlation_id'  => $context['correlation_id']  ?? null,
                'customer_correlation_ids' => (object) ($context['customer_correlation_ids'] ?? []),
            ],
            'coverage' => [
                'complete' => true,
                'shed' => [],
                'redacted_fields' => [],
                'degradation_level' => 'L0',
            ],
            'data' => $data,
        ];

        // Optional actor / beneficiary. When present these unlock the
        // DistinctTargets and per-actor velocity signals in the daemon.
        if (isset($context['actor_id']) && is_string($context['actor_id']) && $context['actor_id'] !== '') {
            $event['actor'] = [
                'id'          => $context['actor_id'],
                'type'        => $context['actor_type'] ?? 'user',
                'namespace'   => $context['actor_namespace'] ?? "{$this->tenantId}/actors",
                'resolution'  => $context['actor_resolution'] ?? 'PROBABLE',
                'confidence'  => $context['actor_confidence'] ?? 0.7,
                'calibration' => 'NOMINAL_UNVALIDATED',
            ];
        }
        if (isset($context['beneficiary_id']) && is_string($context['beneficiary_id']) && $context['beneficiary_id'] !== '') {
            $event['beneficiary'] = [
                'id'          => $context['beneficiary_id'],
                'type'        => $context['beneficiary_type'] ?? 'user',
                'namespace'   => $context['beneficiary_namespace'] ?? "{$this->tenantId}/beneficiaries",
                'resolution'  => $context['beneficiary_resolution'] ?? 'PROBABLE',
                'confidence'  => $context['beneficiary_confidence'] ?? 0.7,
                'calibration' => 'NOMINAL_UNVALIDATED',
            ];
        }

        return $event;
    }

    /** Encode an event as a JSON frame ready for `Transport::send`. */
    public static function encodeFrame(array $event): string
    {
        $json = json_encode($event, JSON_UNESCAPED_SLASHES | JSON_THROW_ON_ERROR);
        return $json;
    }
}
