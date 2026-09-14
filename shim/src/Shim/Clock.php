<?php

declare(strict_types=1);

namespace Athar\Shim;

/**
 * Time source for the shim (D14, MOD-8).
 *
 * - `timestamp` and `received_at` use wall-clock time (`gettimeofday`).
 * - `monotonic_seq` is a per-process counter that never goes backward, so ordering
 *   within a request is stable even if the wall clock jumps.
 * - `skew_estimate_ms` is a rolling delta between wall-clock and monotonic time,
 *   initialised to null; a background refresher is not present in V0.
 *
 * NTP is not assumed.
 */
final class Clock
{
    private static int $seq = 0;

    /** Wall-clock timestamp, RFC3339 with microseconds and Z timezone. */
    public static function nowRfc3339(): string
    {
        $t = microtime(true);
        $whole = (int) floor($t);
        $micros = (int) round(($t - $whole) * 1_000_000);
        // Clamp micros in case of rounding to 10^6.
        if ($micros === 1_000_000) {
            $whole += 1;
            $micros = 0;
        }
        return gmdate('Y-m-d\TH:i:s', $whole) . '.' . str_pad((string) $micros, 6, '0', STR_PAD_LEFT) . 'Z';
    }

    /** Next monotonic sequence for this process (D14). */
    public static function nextSeq(): int
    {
        return ++self::$seq;
    }

    /** V0: no skew estimation. Returns null. */
    public static function skewEstimateMs(): ?int
    {
        return null;
    }

    /** Testing seam. */
    public static function resetSeqForTesting(): void
    {
        self::$seq = 0;
    }
}
