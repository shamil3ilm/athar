<?php

declare(strict_types=1);

/**
 * Correlation stack test.
 *
 * Verifies:
 *   - pushCorrelation / popCorrelation maintain LIFO order
 *   - withCorrelation is exception-safe (stack pops even if the closure throws)
 *   - Events emitted inside a correlation scope get causality.correlation_id
 *     auto-filled
 *   - Explicit context.correlation_id in the caller wins over the stack
 *   - No stack + no explicit id = null correlation, no crash
 *   - Nothing throws to callers
 *
 * Uses the fake daemon (persist mode) to receive events, then reads the
 * JSONL output and inspects the causality fields.
 */

$root = dirname(__DIR__);
require_once $root . '/src/Shim/Classify.php';
require_once $root . '/src/Shim/Redact.php';
require_once $root . '/src/Shim/Ulid.php';
require_once $root . '/src/Shim/Clock.php';
require_once $root . '/src/Shim/Buffer.php';
require_once $root . '/src/Shim/Transport.php';
require_once $root . '/src/Shim/EventFactory.php';
require_once $root . '/src/Shim/Spool.php';
require_once $root . '/src/RuntimeConfig.php';
require_once $root . '/src/Contract/RuntimeInterface.php';
require_once $root . '/src/Decision.php';
require_once $root . '/src/Runtime.php';

use Athar\Runtime;

$failures = [];
function check(string $name, bool $ok, string $detail = ''): void
{
    global $failures;
    if ($ok) echo "  ok    $name\n";
    else {
        $failures[] = $name . ($detail !== '' ? " -- $detail" : '');
        echo "  FAIL  $name" . ($detail !== '' ? " -- $detail" : '') . "\n";
    }
}

// ----- API-level: stack push/pop/current, no daemon needed -------------------

echo "\n== A: stack push/pop/current ==\n";
Runtime::disableForTesting();
check('empty stack → currentCorrelation() null', Runtime::currentCorrelation() === null);

Runtime::pushCorrelation('req_outer');
check('after push, currentCorrelation() reports it', Runtime::currentCorrelation() === 'req_outer');

Runtime::pushCorrelation('req_inner');
check('after second push, current reports inner (LIFO)', Runtime::currentCorrelation() === 'req_inner');

Runtime::popCorrelation();
check('after pop, current reverts to outer', Runtime::currentCorrelation() === 'req_outer');

Runtime::popCorrelation();
check('after final pop, current is null again', Runtime::currentCorrelation() === null);

Runtime::popCorrelation();
check('extra pop on empty stack is a no-op (no throw)', true);

echo "\n== B: withCorrelation is exception-safe ==\n";
try {
    Runtime::withCorrelation('req_scoped', function () {
        throw new \RuntimeException('boom');
    });
    check('withCorrelation re-throws caller exception', false, 'expected boom');
} catch (\RuntimeException $e) {
    check('withCorrelation re-throws caller exception', $e->getMessage() === 'boom');
}
check('withCorrelation popped even after throw', Runtime::currentCorrelation() === null);

// ----- Event-level: correlation propagates through observeEvent --------------

// In-process TCP listener instead of a subprocess — avoids Windows
// proc_terminate/proc_close reliability issues on tests that outlive
// a single one-shot connection. The shim's Transport::send() opens a
// TCP connection, writes all frames, and closes; we accept it here
// and read the frames off the socket ourselves.
$port = 12300 + random_int(1, 500);
putenv("ATHAR_DAEMON_HOST=127.0.0.1");
putenv("ATHAR_DAEMON_PORT={$port}");
putenv("ATHAR_TENANT_ID=tnt_corr");

$errno = 0;
$errstr = '';
$server = @stream_socket_server("tcp://127.0.0.1:{$port}", $errno, $errstr);
if ($server === false) {
    fwrite(STDERR, "cannot bind on {$port}: {$errstr}\n");
    exit(1);
}

Runtime::disableForTesting();
Runtime::enable();

// Event 1: no stack, no context → correlation stays null.
Runtime::observeEvent('order.create', 'order', 'ord_1', ['amount' => 10]);

// Event 2: stack pushed → auto-injected.
Runtime::pushCorrelation('req_alpha');
Runtime::observeEvent('order.update', 'order', 'ord_2', ['amount' => 20]);
Runtime::popCorrelation();

// Event 3: stack + explicit context → explicit wins.
Runtime::pushCorrelation('req_beta');
Runtime::observeEvent('order.settle', 'order', 'ord_3', ['amount' => 30], [
    'correlation_id' => 'req_explicit',
]);
Runtime::popCorrelation();

// Event 4: withCorrelation closure — id is filled inside, cleared outside.
Runtime::withCorrelation('req_gamma', function () {
    Runtime::observeEvent('order.fail', 'order', 'ord_4', ['reason' => 'x']);
});
// Event 5: after withCorrelation returns, correlation should be null again.
Runtime::observeEvent('order.log', 'order', 'ord_5', []);

Runtime::flush();

// Accept the shim's connection and read all frames.
$client = stream_socket_accept($server, 2);
if ($client === false) {
    fwrite(STDERR, "no connection from shim within 2s\n");
    fclose($server);
    exit(1);
}
stream_set_timeout($client, 1);
$lines = [];
while (!feof($client)) {
    $header = _read_exact($client, 4);
    if ($header === null) break;
    $len = unpack('N', $header)[1];
    if ($len <= 0 || $len > 8 * 1024 * 1024) break;
    $body = _read_exact($client, $len);
    if ($body === null) break;
    $lines[] = $body;
}
fclose($client);
fclose($server);

function _read_exact($sock, int $n): ?string
{
    $buf = '';
    while (strlen($buf) < $n) {
        $chunk = fread($sock, $n - strlen($buf));
        if ($chunk === false || $chunk === '') return null;
        $buf .= $chunk;
    }
    return $buf;
}

echo "\n== C: causality.correlation_id is auto-injected ==\n";
$byResource = [];
foreach ($lines as $line) {
    $obj = json_decode($line, true);
    if (is_array($obj) && isset($obj['resource']['id'])) {
        $byResource[$obj['resource']['id']] = $obj;
    }
}

$corrOf = function (string $rid) use ($byResource): ?string {
    return $byResource[$rid]['causality']['correlation_id'] ?? null;
};

check('ord_1 (no stack)   → correlation null',   $corrOf('ord_1') === null);
check('ord_2 (stack)      → correlation req_alpha', $corrOf('ord_2') === 'req_alpha');
check('ord_3 (explicit)   → correlation req_explicit (explicit wins)', $corrOf('ord_3') === 'req_explicit');
check('ord_4 (withCorr)   → correlation req_gamma', $corrOf('ord_4') === 'req_gamma');
check('ord_5 (after with) → correlation null (scope closed)', $corrOf('ord_5') === null);

// ----- Case D: Runtime::observe() ALSO honours the stack --------------------
// Adapters like HttpMiddleware call observe() directly with a pre-built event
// rather than going through observeEvent(). The stack injection must fire on
// this path too, otherwise the http.request record wouldn't share the
// correlation with the events the controller emits underneath it.

// Bring up a listener again to receive the observe() calls.
$port2 = 12300 + random_int(600, 900);
putenv("ATHAR_DAEMON_PORT={$port2}");
$server2 = @stream_socket_server("tcp://127.0.0.1:{$port2}", $errno, $errstr);
if ($server2 === false) {
    fwrite(STDERR, "cannot bind second listener: {$errstr}\n");
    exit(1);
}
Runtime::disableForTesting();
Runtime::enable();

echo "\n== D: observe() honours the correlation stack ==\n";

Runtime::pushCorrelation('req_direct');
$factory = Runtime::factory();
// Build an http.request-ish event via the factory, then hand it to observe()
// directly — same code path adapters use.
$directEvent = $factory->businessEvent('http.request', 'ep_hash_xx', 'endpoint');
Runtime::observe($directEvent);
Runtime::popCorrelation();

// One more, no scope, to confirm null.
Runtime::observe($factory->businessEvent('http.request', 'ep_hash_yy', 'endpoint'));

Runtime::flush();

$client2 = stream_socket_accept($server2, 2);
if ($client2 === false) {
    fwrite(STDERR, "no connection from shim to second listener\n");
    fclose($server2);
    exit(1);
}
stream_set_timeout($client2, 1);
$directLines = [];
while (!feof($client2)) {
    $header = _read_exact($client2, 4);
    if ($header === null) break;
    $len = unpack('N', $header)[1];
    if ($len <= 0 || $len > 8 * 1024 * 1024) break;
    $body = _read_exact($client2, $len);
    if ($body === null) break;
    $directLines[] = $body;
}
fclose($client2);
fclose($server2);

$byResourceD = [];
foreach ($directLines as $line) {
    $obj = json_decode($line, true);
    if (is_array($obj) && isset($obj['resource']['id'])) {
        $byResourceD[$obj['resource']['id']] = $obj;
    }
}
$corrOfD = fn(string $rid) => $byResourceD[$rid]['causality']['correlation_id'] ?? null;

check('ep_hash_xx (observe within scope) → correlation req_direct', $corrOfD('ep_hash_xx') === 'req_direct');
check('ep_hash_yy (observe outside scope) → correlation null',      $corrOfD('ep_hash_yy') === null);

echo "\n";
if (count($failures) === 0) {
    echo "CORRELATION TEST: ALL GREEN\n";
    exit(0);
}
echo count($failures) . " failure(s):\n";
foreach ($failures as $f) echo "  - $f\n";
exit(1);
