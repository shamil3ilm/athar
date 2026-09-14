<?php

declare(strict_types=1);

/**
 * Smoke test for EventMapper + ObservesLifecycle trait.
 *
 * Runs with plain PHP. The trait is exercised via a fake model that quacks like
 * Eloquent (getAttribute / getOriginal / getKey) without pulling illuminate/database.
 */

$root = dirname(__DIR__);
require $root . '/src/Shim/Classify.php';
require $root . '/src/Shim/Redact.php';
require $root . '/src/Shim/Ulid.php';
require $root . '/src/Shim/Clock.php';
require $root . '/src/Shim/Buffer.php';
require $root . '/src/Shim/Transport.php';
require $root . '/src/Shim/EventFactory.php';
require $root . '/src/Shim/Spool.php';
require $root . '/src/RuntimeConfig.php';
require $root . '/src/Contract/RuntimeInterface.php';
require $root . '/src/Runtime.php';
require $root . '/src/Support/EventMapper.php';
require $root . '/src/Support/ObservesLifecycle.php';

use Athar\Runtime;
use Athar\Support\EventMapper;
use Athar\Support\ObservesLifecycle;

$failures = [];
function assertEq(string $name, $expected, $actual): void
{
    global $failures;
    $ok = $expected === $actual;
    if ($ok) echo "  ok    $name\n";
    else {
        $failures[] = "$name -- expected " . var_export($expected, true) . ", got " . var_export($actual, true);
        echo "  FAIL  $name -- expected " . var_export($expected, true) . ", got " . var_export($actual, true) . "\n";
    }
}
function assertTrue(string $name, bool $cond, string $detail = ''): void
{
    global $failures;
    if ($cond) echo "  ok    $name\n";
    else {
        $failures[] = "$name" . ($detail !== '' ? " -- $detail" : '');
        echo "  FAIL  $name" . ($detail !== '' ? " -- $detail" : '') . "\n";
    }
}

echo "== EventMapper: pure state-transition mapping ==\n";
// created → create
assertEq('created + no status → payment.create',
    'payment.create',
    EventMapper::eventType('payment', 'created'));
assertEq('created + status=processing → payment.create (non-terminal)',
    'payment.create',
    EventMapper::eventType('payment', 'created', 'processing'));
assertEq('created + status=success (unusual) → payment.settle',
    'payment.settle',
    EventMapper::eventType('payment', 'created', 'success'));
// updated with terminal transition
assertEq('updated + status: processing → success → payment.settle',
    'payment.settle',
    EventMapper::eventType('payment', 'updated', 'success', 'processing'));
assertEq('updated + status: processing → failed → payment.fail',
    'payment.fail',
    EventMapper::eventType('payment', 'updated', 'failed', 'processing'));
assertEq('updated + status: processing → cancelled → payment.cancel',
    'payment.cancel',
    EventMapper::eventType('payment', 'updated', 'cancelled', 'processing'));
assertEq('updated + status: settled → refunded → payment.reverse',
    'payment.reverse',
    EventMapper::eventType('payment', 'updated', 'refunded', 'settled'));
// updated without terminal transition
assertEq('updated + no state change → payment.process',
    'payment.process',
    EventMapper::eventType('payment', 'updated', 'processing', 'processing'));
assertEq('updated + status: null → processing → payment.process',
    'payment.process',
    EventMapper::eventType('payment', 'updated', 'processing', null));
// deleted
assertEq('deleted → payment.cancel',
    'payment.cancel',
    EventMapper::eventType('payment', 'deleted'));
// case insensitivity
assertEq('updated + SUCCESS (uppercase) still maps',
    'payment.settle',
    EventMapper::eventType('payment', 'updated', 'SUCCESS', 'PENDING'));
// custom terminal map
assertEq('custom terminal map: chargeback → dispute',
    'payment.dispute',
    EventMapper::eventType('payment', 'updated', 'chargeback', 'settled', ['chargeback' => 'dispute']));

echo "\n== ObservesLifecycle trait against a fake Eloquent model ==\n";

Runtime::enable();
if (!Runtime::isEnabled()) { fwrite(STDERR, "runtime failed\n"); exit(2); }

// A tiny fake Eloquent model — enough surface to exercise the trait.
final class FakePaymentModel {
    use ObservesLifecycle;
    protected string $atharLifecycleType = 'payment';
    protected string $atharResourcePrefix = 'pay';
    private array $attrs = [];
    private array $original = [];
    private static array $eventsRegistered = [];
    public function __construct(array $attrs, array $original = []) {
        $this->attrs = $attrs;
        $this->original = $original ?: $attrs;
    }
    public function getAttribute(string $k) { return $this->attrs[$k] ?? null; }
    public function getOriginal(string $k) { return $this->original[$k] ?? null; }
    public function getKey() { return $this->attrs['id'] ?? null; }
    // Fake Eloquent's `static::created(...)`: just record the callback.
    public static function created(callable $cb): void { self::$eventsRegistered['created'][] = $cb; }
    public static function updated(callable $cb): void { self::$eventsRegistered['updated'][] = $cb; }
    public static function deleted(callable $cb): void { self::$eventsRegistered['deleted'][] = $cb; }
    /** Fire the registered callbacks for a given phase. */
    public function fire(string $phase): void {
        foreach (self::$eventsRegistered[$phase] ?? [] as $cb) { $cb($this); }
    }
    public static function boot(): void { self::bootObservesLifecycle(); }
}

FakePaymentModel::boot();

// Drain any pre-existing buffered events.
Runtime::buffer()->drain();

// Case 1: created
$p = new FakePaymentModel(['id' => 42, 'amount' => 100.0, 'currency' => 'AED', 'status' => 'processing']);
$p->fire('created');
$frames = Runtime::buffer()->drain();
assertEq('created emits one event', 1, count($frames));
$ev = json_decode($frames[0], true);
assertEq('event_type = payment.create', 'payment.create', $ev['event_type']);
assertEq('resource.id = pay_42', 'pay_42', $ev['resource']['id'] ?? null);
assertEq('resource.type = payment', 'payment', $ev['resource']['type'] ?? null);
assertTrue('data.amount forwarded', ($ev['data']['amount'] ?? null) == 100.0);
assertEq('data.currency forwarded', 'AED', $ev['data']['currency'] ?? null);

// Case 2: updated to success → settle
$p2 = new FakePaymentModel(
    ['id' => 42, 'amount' => 100.0, 'currency' => 'AED', 'status' => 'success'],
    ['id' => 42, 'amount' => 100.0, 'currency' => 'AED', 'status' => 'processing'],
);
$p2->fire('updated');
$frames = Runtime::buffer()->drain();
assertEq('updated to success emits one event', 1, count($frames));
$ev = json_decode($frames[0], true);
assertEq('event_type = payment.settle', 'payment.settle', $ev['event_type']);
assertEq('same resource.id', 'pay_42', $ev['resource']['id']);

// Case 3: updated to failed → fail
$p3 = new FakePaymentModel(
    ['id' => 42, 'status' => 'failed'],
    ['id' => 42, 'status' => 'processing'],
);
$p3->fire('updated');
$frames = Runtime::buffer()->drain();
$ev = json_decode($frames[0], true);
assertEq('event_type = payment.fail', 'payment.fail', $ev['event_type']);

// Case 4: updated with no state change → process
$p4 = new FakePaymentModel(
    ['id' => 42, 'status' => 'processing', 'amount' => 100.0],
    ['id' => 42, 'status' => 'processing', 'amount' => 100.0],
);
$p4->fire('updated');
$frames = Runtime::buffer()->drain();
$ev = json_decode($frames[0], true);
assertEq('event_type = payment.process (no terminal transition)', 'payment.process', $ev['event_type']);

// Case 5: deleted → cancel
$p5 = new FakePaymentModel(['id' => 42, 'status' => 'processing']);
$p5->fire('deleted');
$frames = Runtime::buffer()->drain();
$ev = json_decode($frames[0], true);
assertEq('event_type = payment.cancel', 'payment.cancel', $ev['event_type']);

echo "\n== Broken model does NOT propagate exceptions (INV-15) ==\n";
$broken = new class {
    use ObservesLifecycle;
    public function getKey() { throw new \RuntimeException('boom'); }
    public function getAttribute($k) { throw new \RuntimeException('boom'); }
};
try {
    $ref = new \ReflectionClass($broken);
    $method = $ref->getMethod('atharDispatch');
    $method->setAccessible(true);
    $method->invoke(null, $broken, 'created');
    assertTrue('dispatch swallowed exceptions', true);
} catch (\Throwable $e) {
    assertTrue('dispatch swallowed exceptions', false, 'threw: ' . $e->getMessage());
}

echo "\n";
if (count($failures) === 0) { echo "OBSERVES_LIFECYCLE ALL GREEN\n"; exit(0); }
echo count($failures) . " failure(s)\n";
foreach ($failures as $f) echo "  - $f\n";
exit(1);
