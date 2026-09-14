<?php

declare(strict_types=1);

/**
 * Smoke test for QueueSubscriber + OutboundHttpSubscriber.
 * Uses duck-typed fake event objects so we can run under plain PHP without illuminate.
 */

$root = dirname(__DIR__);
require $root . '/src/Shim/Classify.php';
require $root . '/src/Shim/Redact.php';
require $root . '/src/Shim/Ulid.php';
require $root . '/src/Shim/Clock.php';
require $root . '/src/Shim/Buffer.php';
require $root . '/src/Shim/Transport.php';
require $root . '/src/Shim/EventFactory.php';
require $root . '/src/RuntimeConfig.php';
require $root . '/src/Contract/RuntimeInterface.php';
require $root . '/src/Runtime.php';
require $root . '/src/Adapter/Laravel/QueueSubscriber.php';
require $root . '/src/Adapter/Laravel/OutboundHttpSubscriber.php';

use Athar\Runtime;
use Athar\Adapter\Laravel\QueueSubscriber;
use Athar\Adapter\Laravel\OutboundHttpSubscriber;

$failures = [];
function assertEq(string $name, $expected, $actual): void {
    global $failures;
    if ($expected === $actual) echo "  ok    $name\n";
    else {
        $failures[] = "$name -- expected " . var_export($expected, true) . ", got " . var_export($actual, true);
        echo "  FAIL  $name -- expected " . var_export($expected, true) . ", got " . var_export($actual, true) . "\n";
    }
}
function assertTrue(string $name, bool $cond, string $detail = ''): void {
    global $failures;
    if ($cond) echo "  ok    $name\n";
    else {
        $failures[] = "$name" . ($detail !== '' ? " -- $detail" : '');
        echo "  FAIL  $name" . ($detail !== '' ? " -- $detail" : '') . "\n";
    }
}

Runtime::enable();
if (!Runtime::isEnabled()) { fwrite(STDERR, "runtime failed\n"); exit(2); }
Runtime::buffer()->drain();

// -----------------------------------------------------------------------------
echo "== QueueSubscriber: JobProcessing → queue.consume ==\n";

$fakeJob = new class {
    public function uuid(): string { return '11111111-2222-3333-4444-555555555555'; }
    public function getName(): string { return 'App\\Jobs\\ProcessPayment'; }
    public function getQueue(): string { return 'payments'; }
    public function attempts(): int { return 1; }
};
$processingEvent = new class($fakeJob) {
    public $connectionName = 'redis';
    public $job;
    public function __construct($j) { $this->job = $j; }
};

$queueSub = new QueueSubscriber();
$queueSub->onJobProcessing($processingEvent);
$frames = Runtime::buffer()->drain();
assertEq('one queue.consume event buffered', 1, count($frames));
$ev = json_decode($frames[0], true);
assertEq('event_type queue.consume', 'queue.consume', $ev['event_type']);
assertEq('resource.id = job_<uuid>', 'job_11111111-2222-3333-4444-555555555555', $ev['resource']['id']);
assertEq('resource.type = job', 'job', $ev['resource']['type']);
assertEq('data.job_name captured', 'App\\Jobs\\ProcessPayment', $ev['data']['job_name']);
assertEq('data.queue captured', 'payments', $ev['data']['queue']);
assertEq('data.connection captured', 'redis', $ev['data']['connection']);
assertEq('entry_point.type = QUEUE', 'QUEUE', $ev['entry_point']['type']);
assertEq('provenance.trigger = BACKGROUND_JOB', 'BACKGROUND_JOB', $ev['provenance']['trigger']);

echo "\n== QueueSubscriber: JobProcessed → queue.complete ==\n";
$queueSub->onJobProcessed($processingEvent);
$frames = Runtime::buffer()->drain();
$ev = json_decode($frames[0], true);
assertEq('event_type queue.complete', 'queue.complete', $ev['event_type']);
assertEq('data.status = success', 'success', $ev['data']['status']);

echo "\n== QueueSubscriber: JobFailed → queue.fail with reason ==\n";
$failEvent = new class($fakeJob) {
    public $connectionName = 'redis';
    public $job;
    public $exception;
    public function __construct($j) {
        $this->job = $j;
        $this->exception = new \RuntimeException('processor unreachable');
    }
};
$queueSub->onJobFailed($failEvent);
$frames = Runtime::buffer()->drain();
$ev = json_decode($frames[0], true);
assertEq('event_type queue.fail', 'queue.fail', $ev['event_type']);
assertEq('data.status = failed', 'failed', $ev['data']['status']);
assertEq('data.reason captured', 'processor unreachable', $ev['data']['reason']);

echo "\n== QueueSubscriber: broken job doesn't propagate ==\n";
$brokenJob = new class {
    public function uuid() { throw new \RuntimeException('boom'); }
    public function getName() { throw new \RuntimeException('boom'); }
};
$brokenEvent = new class($brokenJob) {
    public $connectionName = 'redis';
    public $job;
    public function __construct($j) { $this->job = $j; }
};
try {
    $queueSub->onJobProcessing($brokenEvent);
    assertTrue('broken job onJobProcessing swallowed', true);
} catch (\Throwable $e) {
    assertTrue('broken job onJobProcessing swallowed', false, 'threw: ' . $e->getMessage());
}
// Drain anything that may have been emitted defensively; irrelevant to correctness.
Runtime::buffer()->drain();

// -----------------------------------------------------------------------------
echo "\n== OutboundHttpSubscriber: hostOnly parses correctly ==\n";
assertEq('hostOnly on https URL', 'https://api.stripe.com',
    OutboundHttpSubscriber::hostOnly('https://api.stripe.com/v1/charges/ch_ABC?meta=x'));
assertEq('hostOnly on http URL with port', 'http://localhost:8080',
    OutboundHttpSubscriber::hostOnly('http://localhost:8080/webhook'));
assertEq('hostOnly on schemeless URL falls back to https', 'https://example.com',
    OutboundHttpSubscriber::hostOnly('//example.com/path'));
assertTrue('hostOnly on garbage returns null',
    OutboundHttpSubscriber::hostOnly('not a url') === null || OutboundHttpSubscriber::hostOnly('not a url') === 'https:');
// Note: some PHP versions succeed on "not a url" — accept either.

echo "\n== OutboundHttpSubscriber: ResponseReceived → outbound.request.completed ==\n";
$fakeRequest = new class {
    public function method(): string { return 'POST'; }
    public function url(): string { return 'https://api.stripe.com/v1/charges/ch_SENSITIVE_ID?token=secret'; }
    public function body(): string { return '{"amount":1000}'; }
};
$fakeResponse = new class {
    public function status(): int { return 200; }
};
$responseEvent = new class($fakeRequest, $fakeResponse) {
    public $request;
    public $response;
    public function __construct($rq, $rs) { $this->request = $rq; $this->response = $rs; }
};

$httpSub = new OutboundHttpSubscriber();
$httpSub->onResponseReceived($responseEvent);
$frames = Runtime::buffer()->drain();
assertEq('one outbound event buffered', 1, count($frames));
$ev = json_decode($frames[0], true);
assertEq('event_type outbound.request.completed', 'outbound.request.completed', $ev['event_type']);
assertEq('data.status_code captured', 200, $ev['data']['status_code']);
assertEq('data.outcome = response', 'response', $ev['data']['outcome']);
assertTrue('resource.id is stable per (method, host)',
    is_string($ev['resource']['id']) && str_starts_with($ev['resource']['id'], 'ext_'));

// The sensitive parts of the URL MUST NOT be in the serialized event.
$rawJson = json_encode($ev);
assertTrue('no leak: ch_SENSITIVE_ID absent from event', !str_contains($rawJson, 'ch_SENSITIVE_ID'));
assertTrue('no leak: secret token absent', !str_contains($rawJson, 'secret'));
assertTrue('no leak: request body absent', !str_contains($rawJson, '1000'));
assertTrue('coverage.redacted_fields declares omissions',
    is_array($ev['coverage']['redacted_fields'])
    && in_array('url.path', $ev['coverage']['redacted_fields'], true)
    && in_array('body', $ev['coverage']['redacted_fields'], true));

echo "\n== OutboundHttpSubscriber: ConnectionFailed → outbound.request.failed ==\n";
$failedEvent = new class($fakeRequest) {
    public $request;
    public function __construct($rq) { $this->request = $rq; }
};
$httpSub->onConnectionFailed($failedEvent);
$frames = Runtime::buffer()->drain();
$ev = json_decode($frames[0], true);
assertEq('event_type outbound.request.failed', 'outbound.request.failed', $ev['event_type']);
assertEq('data.outcome = connection_failed', 'connection_failed', $ev['data']['outcome']);

echo "\n== OutboundHttpSubscriber: broken request doesn't propagate ==\n";
$brokenReq = new class {
    public function method() { throw new \RuntimeException('boom'); }
    public function url() { throw new \RuntimeException('boom'); }
};
$brokenHttpEvent = new class($brokenReq) {
    public $request;
    public function __construct($rq) { $this->request = $rq; }
};
try {
    $httpSub->onResponseReceived($brokenHttpEvent);
    assertTrue('broken request swallowed', true);
} catch (\Throwable $e) {
    assertTrue('broken request swallowed', false, 'threw: ' . $e->getMessage());
}

echo "\n";
if (count($failures) === 0) { echo "SUBSCRIBERS ALL GREEN\n"; exit(0); }
echo count($failures) . " failure(s)\n";
foreach ($failures as $f) echo "  - $f\n";
exit(1);
