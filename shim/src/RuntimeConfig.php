<?php

declare(strict_types=1);

namespace Athar;

/**
 * Shim configuration. All values sourced from env vars by default so a customer
 * changes settings without a code deploy.
 *
 * Env vars:
 *   ATHAR_TENANT_ID            required in production; defaults to 'tnt_default'
 *   ATHAR_DAEMON_HOST          default 127.0.0.1
 *   ATHAR_DAEMON_PORT          default 11223
 *   ATHAR_CONNECT_TIMEOUT_MS   default 100
 *   ATHAR_BUFFER_MAX_FRAMES    default 512
 *   ATHAR_BUFFER_MAX_BYTES     default 4194304 (4 MB)
 *   ATHAR_SHIM_SPOOL_DIR       default <sys_temp>/athar-shim-spool
 */
final class RuntimeConfig
{
    public function __construct(
        public readonly string $tenantId,
        public readonly string $host,
        public readonly int $port,
        public readonly int $connectTimeoutMs,
        public readonly int $bufferMaxFrames,
        public readonly int $bufferMaxBytes,
        public readonly string $spoolDir,
    ) {}

    public static function fromEnv(): self
    {
        return new self(
            tenantId:         self::env('ATHAR_TENANT_ID', 'tnt_default'),
            host:             self::env('ATHAR_DAEMON_HOST', '127.0.0.1'),
            port:             (int) self::env('ATHAR_DAEMON_PORT', '11223'),
            connectTimeoutMs: (int) self::env('ATHAR_CONNECT_TIMEOUT_MS', '100'),
            bufferMaxFrames:  (int) self::env('ATHAR_BUFFER_MAX_FRAMES', '512'),
            bufferMaxBytes:   (int) self::env('ATHAR_BUFFER_MAX_BYTES', '4194304'),
            spoolDir:         self::env('ATHAR_SHIM_SPOOL_DIR', \Athar\Shim\Spool::defaultDir()),
        );
    }

    private static function env(string $name, string $default): string
    {
        $v = getenv($name);
        return ($v === false || $v === '') ? $default : $v;
    }
}
