<?php

declare(strict_types=1);

namespace Athar\RefApp;

use Athar\Support\ObservesLifecycle;

/**
 * A minimal Eloquent-lookalike Payment model, purely to demonstrate how
 * `ObservesLifecycle` behaves against something that quacks like a Laravel
 * model — without requiring illuminate/database.
 *
 * When shipped in a real Laravel app, `class Payment extends Model { use
 * ObservesLifecycle; ... }` behaves identically to this. Every save() /
 * update() / delete() emits the correct canonical event via the shim.
 */
final class Payment
{
    use ObservesLifecycle;

    protected string $atharLifecycleType = 'payment';
    protected string $atharResourcePrefix = 'pay';

    /** @var array<string,mixed> */
    private array $attrs = [];
    /** @var array<string,mixed> */
    private array $original = [];

    /** @var array<class-string, array<string, list<callable>>> */
    private static array $listeners = [];

    public function __construct(array $attrs = [])
    {
        $this->attrs = $attrs;
        $this->original = $attrs;
    }

    // Eloquent-compatible surface consumed by ObservesLifecycle:

    public static function created(callable $cb): void { self::$listeners[static::class]['created'][] = $cb; }
    public static function updated(callable $cb): void { self::$listeners[static::class]['updated'][] = $cb; }
    public static function deleted(callable $cb): void { self::$listeners[static::class]['deleted'][] = $cb; }

    public function getAttribute(string $key) { return $this->attrs[$key] ?? null; }
    public function getOriginal(string $key) { return $this->original[$key] ?? null; }
    public function getKey() { return $this->attrs['id'] ?? null; }

    public function __get($key) { return $this->attrs[$key] ?? null; }
    public function __set($key, $value) { $this->attrs[$key] = $value; }

    // Simulate a Laravel save() — decide create-vs-update, fire the event, roll originals.
    public function save(): void
    {
        $existed = isset($this->original['id']) && $this->original['id'] !== null;
        if (!$existed && !isset($this->attrs['id'])) {
            // Auto-assign an id for the demo.
            $this->attrs['id'] = 'demo_' . bin2hex(random_bytes(6));
        }
        if (!$existed) {
            $this->fire('created');
        } else {
            $this->fire('updated');
        }
        $this->original = $this->attrs;
    }

    public function delete(): void
    {
        $this->fire('deleted');
    }

    private function fire(string $phase): void
    {
        // Boot the trait's listeners lazily so the demo file is self-contained.
        if (!isset(self::$listeners[static::class]['_booted'])) {
            self::bootObservesLifecycle();
            self::$listeners[static::class]['_booted'] = [fn() => null];
        }
        foreach (self::$listeners[static::class][$phase] ?? [] as $cb) {
            $cb($this);
        }
    }
}
