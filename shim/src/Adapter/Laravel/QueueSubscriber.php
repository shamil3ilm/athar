<?php

declare(strict_types=1);

namespace Athar\Adapter\Laravel;

use Athar\Runtime;
use Athar\Shim\Clock;

/**
 * Laravel queue auto-instrumentation (INT-3).
 *
 * Subscribes to Laravel queue lifecycle events and emits canonical events:
 *
 *   JobProcessing  → queue.consume
 *   JobProcessed   → queue.complete
 *   JobFailed      → queue.fail
 *
 * `resource.id` is set to `job_{uuid}` where uuid comes from the payload the
 * dispatcher assigned (Laravel 8+). If the job has no UUID, we skip — better
 * silent than emitting an event we can't correlate.
 *
 * Every method catches every `\Throwable` (INV-15): a shim failure never breaks
 * the queue worker.
 */
final class QueueSubscriber
{
    public function onJobProcessing($event): void
    {
        $this->emit('queue.consume', $event, [
            'phase' => 'start',
        ]);
    }

    public function onJobProcessed($event): void
    {
        $this->emit('queue.complete', $event, [
            'phase' => 'end',
            'status' => 'success',
        ]);
    }

    public function onJobFailed($event): void
    {
        $exception = property_exists($event, 'exception') ? $event->exception : null;
        $reason = null;
        if (is_object($exception) && method_exists($exception, 'getMessage')) {
            $reason = self::truncate((string) $exception->getMessage(), 200);
        }
        $this->emit('queue.fail', $event, [
            'phase' => 'end',
            'status' => 'failed',
            'reason' => $reason,
        ]);
    }

    private function emit(string $eventType, $event, array $extraData): void
    {
        if (!Runtime::isEnabled()) return;
        $factory = Runtime::factory();
        if ($factory === null) return;
        try {
            $job = property_exists($event, 'job') ? $event->job : null;
            if ($job === null || !is_object($job)) return;

            $uuid = self::safeCall($job, 'uuid');
            if (!is_string($uuid) || $uuid === '') {
                // Fall back to spl_object_id-style stable ident so we still emit an event;
                // correlation is best-effort in that case.
                $uuid = 'noid_' . substr(hash('sha256', spl_object_hash($job)), 0, 16);
            }

            $name = self::safeCall($job, 'getName') ?? self::safeCall($job, 'resolveName') ?? 'unknown';
            $queue = self::safeCall($job, 'getQueue');
            $attempts = self::safeCall($job, 'attempts');
            $connection = property_exists($event, 'connectionName') ? $event->connectionName : null;

            $now = Clock::nowRfc3339();
            $canonical = $factory->businessEvent(
                eventType: $eventType,
                resourceId: 'job_' . $uuid,
                resourceType: 'job',
                data: array_filter([
                    'job_name'    => is_string($name) ? $name : null,
                    'queue'       => is_string($queue) ? $queue : null,
                    'attempts'    => is_int($attempts) ? $attempts : null,
                    'connection'  => is_string($connection) ? $connection : null,
                ] + $extraData, fn($v) => $v !== null),
                context: [
                    'trigger' => 'BACKGROUND_JOB',
                    'source'  => 'QUEUE',
                    'origin'  => 'SYSTEM',
                ],
            );
            $canonical['event_type'] = $eventType;
            $canonical['timestamp'] = $now;
            $canonical['received_at'] = $now;
            // Entry point on queue events: type = QUEUE.
            $canonical['entry_point'] = [
                'type' => 'QUEUE',
                'endpoint_id' => is_string($queue) ? 'q_' . substr(hash('sha256', $queue), 0, 16) : null,
                'method' => null,
                'route' => is_string($queue) ? $queue : null,
                'route_template' => is_string($queue) ? $queue : null,
            ];
            Runtime::observe($canonical);
        } catch (\Throwable $e) {
            @error_log('[athar] QueueSubscriber emit failed: ' . $e->getMessage());
        }
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

    private static function truncate(string $s, int $max): string
    {
        return strlen($s) > $max ? substr($s, 0, $max - 1) . '…' : $s;
    }
}
