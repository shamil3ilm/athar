<?php

declare(strict_types=1);

namespace Athar\Shim;

/**
 * Bounded, drop-on-overflow in-memory buffer (INV-12, INV-13).
 *
 * The shim MUST NOT block or grow unboundedly. On overflow, the oldest record is
 * dropped and the drop is counted; the caller is expected to emit a `coverage_gap`
 * record (MOD-29) with the count.
 *
 * V0: simple array-backed queue. Suitable for PHP-FPM's short request lifetime.
 * Long-lived process shims (Node/Python) will want a genuine ring later.
 */
final class Buffer
{
    /** @var list<string> encoded frames ready to write */
    private array $frames = [];
    private int $totalBytes = 0;
    private int $dropped = 0;

    public function __construct(
        private readonly int $maxFrames = 512,
        private readonly int $maxBytes = 4 * 1024 * 1024, // 4 MB budget in-process
    ) {}

    public function push(string $frame): void
    {
        $size = strlen($frame);
        // Drop oldest until this new frame fits.
        while ((count($this->frames) >= $this->maxFrames) || ($this->totalBytes + $size > $this->maxBytes)) {
            if (empty($this->frames)) {
                // Frame alone exceeds byte budget → drop this frame outright.
                $this->dropped++;
                return;
            }
            $old = array_shift($this->frames);
            $this->totalBytes -= strlen((string) $old);
            $this->dropped++;
        }
        $this->frames[] = $frame;
        $this->totalBytes += $size;
    }

    /** @return list<string> */
    public function drain(): array
    {
        $out = $this->frames;
        $this->frames = [];
        $this->totalBytes = 0;
        return $out;
    }

    public function count(): int { return count($this->frames); }
    public function bytes(): int { return $this->totalBytes; }
    public function dropped(): int { return $this->dropped; }
    public function resetDroppedCounter(): int
    {
        $n = $this->dropped;
        $this->dropped = 0;
        return $n;
    }
}
