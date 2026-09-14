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
     * One-liner for the common case: emit a business-domain event with a resource
     * binding. Handles ID generation, timestamps, redaction and buffering.
     *
     * Example:
     *   Runtime::observePayment('payment.create', "pay_{$id}", ['amount' => $x]);
     *
     * @param array<string,mixed> $data
     * @param array<string,mixed> $context
     */
    public static function observeBusinessEvent(
        string $eventType,
        string $resourceId,
        string $resourceType = 'payment',
        array $data = [],
        array $context = [],
    ): void {
        if (!self::$enabled || self::$factory === null) return;
        try {
            $event = self::$factory->businessEvent($eventType, $resourceId, $resourceType, $data, $context);
            self::observe($event);
        } catch (\Throwable $e) {
            @error_log('[athar] observeBusinessEvent() failed: ' . $e->getMessage());
        }
    }

    /** Convenience: `payment.*` events with resource id and payload. */
    public static function observePayment(
        string $eventType,
        string $paymentId,
        array $data = [],
        array $context = [],
    ): void {
        self::observeBusinessEvent($eventType, $paymentId, 'payment', $data, $context);
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
