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
    /**
     * Stack of correlation-id + causation-id pairs. Top-of-stack values are
     * auto-injected into every observeEvent()'s context, so callers don't
     * have to plumb correlation IDs through every layer of a handler.
     *
     * @var list<array{correlation_id: string, causation_id: ?string}>
     */
    private static array $correlationStack = [];

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

    /**
     * Push a correlation ID onto the stack. Every subsequent observeEvent()
     * (and observePayment) auto-fills causality.correlation_id and
     * causality.causation_id from top-of-stack UNLESS the caller passes them
     * explicitly in `$context` (explicit always wins).
     *
     * Typical use: HTTP middleware pushes a per-request ID at the start of a
     * request and pops it at the end (or via `withCorrelation`). Every event
     * emitted inside that request handler carries the same correlation ID
     * with zero controller-side plumbing.
     *
     * INV-15: never throws. Safe to call before enable().
     */
    public static function pushCorrelation(string $correlationId, ?string $causationId = null): void
    {
        try {
            self::$correlationStack[] = [
                'correlation_id' => $correlationId,
                'causation_id'   => $causationId,
            ];
        } catch (\Throwable $e) {
            @error_log('[athar] pushCorrelation() failed: ' . $e->getMessage());
        }
    }

    /**
     * Pop the most recently pushed correlation. Safe to call when the stack
     * is empty (no-op). Prefer `withCorrelation` for exception safety.
     */
    public static function popCorrelation(): void
    {
        try {
            array_pop(self::$correlationStack);
        } catch (\Throwable $e) {
            @error_log('[athar] popCorrelation() failed: ' . $e->getMessage());
        }
    }

    /**
     * Run `$fn` with a correlation id pushed for its duration. The push is
     * balanced with a pop even if `$fn` throws — the exception propagates,
     * but the stack is always restored.
     *
     * @template T
     * @param callable(): T $fn
     * @return T
     */
    public static function withCorrelation(string $correlationId, callable $fn, ?string $causationId = null): mixed
    {
        self::pushCorrelation($correlationId, $causationId);
        try {
            return $fn();
        } finally {
            self::popCorrelation();
        }
    }

    /** Read the current top-of-stack correlation, or null if the stack is empty. */
    public static function currentCorrelation(): ?string
    {
        $top = end(self::$correlationStack);
        return $top === false ? null : ($top['correlation_id'] ?? null);
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

    /**
     * Synchronous evaluation — send an event, wait for a decision, return it.
     *
     * PERF-3: hard deadline of `$deadlineMs`. On timeout / transport error /
     * shim disabled, returns a fail-open Decision. NEVER throws — the caller
     * always gets a Decision object.
     *
     * Use this when the app needs to act on the daemon's opinion before
     * proceeding (e.g. reject a payment on `Decision::wouldRestrict()`).
     * For pure telemetry, use `Runtime::observe()` (fire-and-forget) — it's
     * cheaper and doesn't block.
     *
     * The decision is ALSO written to the daemon's audit chain and decision
     * store — so this is not a "peek", it's a real, persisted evaluation.
     */
    public static function evaluate(array $event, int $deadlineMs = 5): Decision
    {
        $started = hrtime(true);
        if (!self::$enabled || self::$transport === null) {
            return Decision::failOpen(Decision::OUTCOME_DISABLED, 0);
        }
        try {
            // Redact before crossing any process boundary.
            [$safe, $droppedPaths] = \Athar\Shim\Redact::stripSecrets($event);
            if (!empty($droppedPaths) && isset($safe['coverage']['redacted_fields'])) {
                $safe['coverage']['redacted_fields'] = array_values(array_unique(array_merge(
                    (array) $safe['coverage']['redacted_fields'],
                    $droppedPaths,
                )));
            }
            // Flag the frame as an evaluation request.
            $safe['__evaluate__'] = true;
            $frame = \Athar\Shim\EventFactory::encodeFrame($safe);
            $response = self::$transport->sendAndReceive($frame, $deadlineMs);
            $latencyUs = (int) ((hrtime(true) - $started) / 1000);
            if ($response === null) {
                return Decision::failOpen(Decision::OUTCOME_DEADLINE_EXCEEDED, $latencyUs);
            }
            $decoded = json_decode($response, true);
            if (!is_array($decoded)) {
                return Decision::failOpen(Decision::OUTCOME_MALFORMED, $latencyUs);
            }
            return Decision::fromResponse($decoded, $latencyUs);
        } catch (\Throwable $e) {
            @error_log('[athar] evaluate() failed: ' . $e->getMessage());
            $latencyUs = (int) ((hrtime(true) - $started) / 1000);
            return Decision::failOpen(Decision::OUTCOME_EXCEPTION, $latencyUs);
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
            // Correlation stack: top-of-stack fills causality IDs UNLESS the
            // caller passed them explicitly. Explicit always wins.
            $top = end(self::$correlationStack);
            if ($top !== false) {
                if (!isset($context['correlation_id']) && !empty($top['correlation_id'])) {
                    $context['correlation_id'] = $top['correlation_id'];
                }
                if (!isset($context['causation_id']) && !empty($top['causation_id'])) {
                    $context['causation_id'] = $top['causation_id'];
                }
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

    /**
     * Reachability check — is the daemon accepting connections on the
     * configured host:port? Returns false when the shim is disabled or the
     * connect attempt fails within the configured timeout. NEVER throws.
     *
     * Useful at boot / in a health-check endpoint to decide whether policy
     * decisions can be trusted. A `false` result means the shim will still
     * observe (buffered + spooled) but `evaluate()` will fail-open.
     */
    public static function ping(): bool
    {
        if (!self::$enabled || self::$transport === null) return false;
        try {
            return self::$transport->ping();
        } catch (\Throwable $e) {
            @error_log('[athar] ping() failed: ' . $e->getMessage());
            return false;
        }
    }
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
        self::$correlationStack = [];
    }
}
