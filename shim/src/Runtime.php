<?php

declare(strict_types=1);

namespace Athar;

use Athar\Shim\Buffer;
use Athar\Shim\EventFactory;
use Athar\Shim\Spool;
use Athar\Shim\Transport;

/**
 * Public API for the athar shim.
 *
 *   Runtime::enable();                    // once, at boot
 *   Runtime::context([...]);              // optional enrichment
 *   Runtime::observe($event);             // internal use by adapters
 *
 * INV-15: no method here may propagate an exception into caller code.
 * INV-13: no method opens a socket at boot; sockets are opened only at flush time.
 * PERF-8: `enable()` does the minimum — register a shutdown flush and no more.
 */
final class Runtime
{
    private static bool $enabled = false;
    private static ?Buffer $buffer = null;
    private static ?Transport $transport = null;
    private static ?EventFactory $factory = null;
    private static ?Spool $spool = null;
    /** @var array<string,mixed> */
    private static array $context = [];

    public static function enable(?RuntimeConfig $config = null): void
    {
        if (self::$enabled) return;
        try {
            $config ??= RuntimeConfig::fromEnv();
            self::$buffer = new Buffer($config->bufferMaxFrames, $config->bufferMaxBytes);
            self::$transport = new Transport($config->host, $config->port, $config->connectTimeoutMs);
            self::$factory = new EventFactory($config->tenantId);
            self::$spool = new Spool($config->spoolDir);
            // Flush at end-of-request (PHP-FPM has no background thread).
            register_shutdown_function(static function (): void {
                Runtime::flush();
            });
            self::$enabled = true;
        } catch (\Throwable $e) {
            @error_log('[athar] enable() failed: ' . $e->getMessage());
            self::$enabled = false;
        }
    }

    /** Attach non-sensitive enrichment context (INT-7). */
    public static function context(array $attrs): void
    {
        if (!self::$enabled) return;
        try {
            self::$context = array_merge(self::$context, $attrs);
        } catch (\Throwable $e) {
            @error_log('[athar] context() failed: ' . $e->getMessage());
        }
    }

    /**
     * Observe one canonical event (called by adapters or directly).
     * The event is redacted (C5 stripped) before it enters the buffer.
     */
    public static function observe(array $event): void
    {
        if (!self::$enabled || self::$buffer === null) return;
        try {
            [$safe, $droppedPaths] = \Athar\Shim\Redact::stripSecrets($event);
            if (!empty($droppedPaths) && isset($safe['coverage']['redacted_fields'])) {
                $safe['coverage']['redacted_fields'] = array_values(array_unique(array_merge(
                    (array) $safe['coverage']['redacted_fields'],
                    $droppedPaths,
                )));
            }
            $frame = EventFactory::encodeFrame($safe);
            self::$buffer->push($frame);
        } catch (\Throwable $e) {
            @error_log('[athar] observe() failed: ' . $e->getMessage());
        }
    }

    /** Force a flush to the daemon. Called automatically at shutdown. */
    public static function flush(): void
    {
        if (!self::$enabled || self::$buffer === null || self::$transport === null) return;
        try {
            $frames = self::$buffer->drain();
            if (empty($frames)) return;
            $written = self::$transport->send($frames);
            if ($written < count($frames)) {
                // Partial write / transport failure. PHP-FPM has NO next flush — this
                // request is about to die. Record the loss to the shim spool so a
                // subsequent daemon can pick it up and emit a coverage_gap record.
                $lost = array_slice($frames, $written);
                $bytesLost = 0;
                foreach ($lost as $frame) $bytesLost += strlen($frame);
                if (self::$spool !== null) {
                    self::$spool->writeLoss('daemon_unreachable', count($lost), $bytesLost);
                }
                @error_log(sprintf(
                    '[athar] daemon unreachable: %d frame(s), %d byte(s) spooled to %s',
                    count($lost), $bytesLost,
                    self::$spool?->dir() ?? '?'
                ));
            }
        } catch (\Throwable $e) {
            @error_log('[athar] flush() failed: ' . $e->getMessage());
        }
    }

    /**
     * Generic entry point: emit a business-domain event with a resource binding.
     * Handles ID generation, timestamps, redaction, and buffering.
     *
     * `$resourceType` is required and names the domain: 'payment', 'invoice',
     * 'order', 'refund', 'subscription', 'login', 'transfer', ... whatever the
     * customer's business uses. The daemon groups a lifecycle by
     * (tenant, resource_type, resource_id).
     *
     * Optional `$beneficiaryId` (payee / recipient / assignee — depends on
     * domain) unlocks the daemon's DistinctTargets signal. Optional
     * `$actorId` (payer / initiator) tightens per-actor grouping used by
     * DistinctTargets and HighVelocity. Both map onto the canonical schema's
     * `event.actor.id` / `event.beneficiary.id`.
     *
     * Examples:
     *   Runtime::observeEvent('payment.create', 'payment', "pay_{$id}", ['amount' => 500]);
     *   Runtime::observeEvent('invoice.issue', 'invoice', "inv_{$id}", ['total' => 500]);
     *   Runtime::observeEvent('login.attempt', 'login', "login_{$id}", ['outcome' => 'failed'],
     *                          actorId: $user->id);
     *
     * @param array<string,mixed> $data
     * @param array<string,mixed> $context
     */
    public static function observeEvent(
        string $eventType,
        string $resourceType,
        string $resourceId,
        array $data = [],
        array $context = [],
        ?string $beneficiaryId = null,
        ?string $actorId = null,
    ): void {
        if (!self::$enabled || self::$factory === null) return;
        try {
            if ($beneficiaryId !== null && $beneficiaryId !== '') {
                $context['beneficiary_id'] = $beneficiaryId;
                $context['beneficiary_type'] ??= 'user';
            }
            if ($actorId !== null && $actorId !== '') {
                $context['actor_id'] = $actorId;
                $context['actor_type'] ??= 'user';
            }
            $event = self::$factory->businessEvent($eventType, $resourceId, $resourceType, $data, $context);
            self::observe($event);
        } catch (\Throwable $e) {
            @error_log('[athar] observeEvent() failed: ' . $e->getMessage());
        }
    }

    /**
     * Backwards-compatible generic entry (older name).
     *
     * @deprecated use {@see observeEvent()}. Kept for pre-existing call sites
     *             so no external code breaks. Will not be removed in V1.
     */
    public static function observeBusinessEvent(
        string $eventType,
        string $resourceId,
        string $resourceType,
        array $data = [],
        array $context = [],
    ): void {
        self::observeEvent($eventType, $resourceType, $resourceId, $data, $context);
    }

    /**
     * Domain-specific convenience wrapper for PAYMENT events. This is ONE of
     * many possible wrappers on top of `observeEvent()` — a customer with
     * different domains would write parallel helpers such as `observeInvoice`,
     * `observeOrder`, or `observeLogin`. They all delegate to `observeEvent`
     * with a fixed `$resourceType`.
     *
     *   Runtime::observePayment('payment.settle', "pay_{$id}",
     *       data: ['amount' => 500],
     *       beneficiaryId: $payment->recipient_id,
     *       actorId: $payment->payer_id,
     *   );
     */
    public static function observePayment(
        string $eventType,
        string $paymentId,
        array $data = [],
        array $context = [],
        ?string $beneficiaryId = null,
        ?string $actorId = null,
    ): void {
        self::observeEvent($eventType, 'payment', $paymentId, $data, $context, $beneficiaryId, $actorId);
    }

    public static function factory(): ?EventFactory { return self::$factory; }
    public static function buffer(): ?Buffer { return self::$buffer; }
    public static function isEnabled(): bool { return self::$enabled; }
    /** @return array<string,mixed> */
    public static function currentContext(): array { return self::$context; }

    /** @internal Testing seam. */
    public static function disableForTesting(): void
    {
        self::$enabled = false;
        self::$buffer = null;
        self::$transport = null;
        self::$factory = null;
        self::$context = [];
    }
}
