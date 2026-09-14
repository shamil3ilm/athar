<?php

declare(strict_types=1);

namespace Athar\Shim;

/**
 * Shim-side spool for events the shim couldn't deliver to the daemon
 * (MOD-29 — records what was NOT observed by the daemon so nothing goes
 * silently missing).
 *
 * PHP-FPM constraint: there is NO next request to retry with. When the transport
 * fails, we can't buffer in-memory for later. So we spool a small record to a
 * well-known local directory. The daemon reads this directory on startup and
 * on a periodic tick, converts each entry into a canonical `coverage_gap`
 * event, and deletes the file after it's committed to the audit chain.
 *
 * Failure to write the spool is silent — we can't cascade shim-internal
 * failures into the application (INV-15).
 */
final class Spool
{
    private string $dir;

    public function __construct(?string $dir = null)
    {
        $this->dir = $dir ?? self::defaultDir();
    }

    public static function defaultDir(): string
    {
        return sys_get_temp_dir() . DIRECTORY_SEPARATOR . 'athar-shim-spool';
    }

    public function dir(): string { return $this->dir; }

    /**
     * Write one loss record. Returns true on success. Never throws.
     *
     * @param string $reason         Short reason tag (e.g. "daemon_unreachable").
     * @param int    $framesLost     Number of canonical events lost.
     * @param int    $bytesLost      Approximate bytes lost.
     * @param array  $extra          Additional context (kept small; safe fields only).
     */
    public function writeLoss(string $reason, int $framesLost, int $bytesLost, array $extra = []): bool
    {
        try {
            if (!is_dir($this->dir)) {
                if (!@mkdir($this->dir, 0700, true) && !is_dir($this->dir)) {
                    return false;
                }
            }
            $record = array_merge([
                'kind'         => 'shim_loss',
                'reason'       => $reason,
                'shim_pid'     => getmypid() ?: 0,
                'at_ms'        => (int) (microtime(true) * 1000),
                'frames_lost'  => $framesLost,
                'bytes_lost'   => $bytesLost,
                'tenant_id'    => (string) (getenv('ATHAR_TENANT_ID') ?: 'tnt_unknown'),
            ], $extra);
            $line = json_encode($record, JSON_UNESCAPED_SLASHES);
            if ($line === false) return false;
            $line .= "\n";

            $filename = sprintf(
                'shim-%d-%s-%d.jsonl',
                getmypid() ?: 0,
                bin2hex(random_bytes(4)),
                $record['at_ms'],
            );
            $path = $this->dir . DIRECTORY_SEPARATOR . $filename;
            $result = @file_put_contents($path, $line, FILE_APPEND | LOCK_EX);
            if ($result === false) return false;
            @chmod($path, 0600);
            return true;
        } catch (\Throwable $e) {
            @error_log('[athar] Spool::writeLoss failed: ' . $e->getMessage());
            return false;
        }
    }
}
