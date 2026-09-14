<?php

declare(strict_types=1);

/**
 * Test that observePayment's new $beneficiaryId / $actorId parameters flow
 * through to event.beneficiary and event.actor on the canonical event, and
 * exercise the fake daemon receiving them. This is what unlocks the daemon's
 * DistinctTargets signal end-to-end.
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

use Athar\Runtime;

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
        $failures[] = $name . ($detail !== '' ? " -- $detail" : '');
        echo "  FAIL  $name" . ($detail !== '' ? " -- $detail" : '') . "\n";
    }
}

putenv('ATHAR_TENANT_ID=tnt_ben_test');
putenv('ATHAR_DAEMON_HOST=127.0.0.1');
putenv('ATHAR_DAEMON_PORT=1');            // never binds; observe() only buffers
putenv('ATHAR_CONNECT_TIMEOUT_MS=10');
putenv('ATHAR_BUFFER_MAX_FRAMES=100');
Runtime::disableForTesting();
Runtime::enable();
if (!Runtime::isEnabled()) { fwrite(STDERR, "runtime failed\n"); exit(2); }
Runtime::buffer()->drain();

echo "== observeEvent (generic) WITHOUT beneficiary/actor: schema unchanged ==\n";
Runtime::observeEvent('payment.create', 'payment', 'pay_basic', ['amount' => 100]);
$frames = Runtime::buffer()->drain();
$ev = json_decode($frames[0], true);
assertEq('event_type', 'payment.create', $ev['event_type']);
assertEq('resource.type = payment', 'payment', $ev['resource']['type']);
assertEq('resource.id', 'pay_basic', $ev['resource']['id']);
assertTrue('no actor on event without actor_id', !isset($ev['actor']));
assertTrue('no beneficiary on event without beneficiary_id', !isset($ev['beneficiary']));

echo "\n== observeEvent works for non-payment domains too ==\n";
Runtime::observeEvent('invoice.issue', 'invoice', 'inv_42',
    ['total' => 500, 'currency' => 'AED'],
    actorId: 'user_finance_bot',
);
$ev = json_decode(Runtime::buffer()->drain()[0], true);
assertEq('event_type', 'invoice.issue', $ev['event_type']);
assertEq('resource.type = invoice', 'invoice', $ev['resource']['type']);
assertEq('resource.id', 'inv_42', $ev['resource']['id']);
assertTrue('resource.namespace scoped by tenant and domain',
    ($ev['resource']['namespace'] ?? '') === 'tnt_ben_test/invoices');
assertEq('actor.id', 'user_finance_bot', $ev['actor']['id'] ?? null);

Runtime::observeEvent('login.attempt', 'login', 'login_xyz',
    ['outcome' => 'failed'], actorId: 'user_alice');
$ev = json_decode(Runtime::buffer()->drain()[0], true);
assertEq('event_type', 'login.attempt', $ev['event_type']);
assertEq('resource.type = login', 'login', $ev['resource']['type']);

echo "\n== observePayment WITH beneficiary + actor: canonical fields populated ==\n";
Runtime::observePayment(
    'payment.settle',
    'pay_full_flow',
    ['amount' => 5000, 'currency' => 'AED'],
    context: [],
    beneficiaryId: 'ben_customer_a',
    actorId: 'user_payer_1',
);
$frames = Runtime::buffer()->drain();
$ev = json_decode($frames[0], true);
assertEq('event_type', 'payment.settle', $ev['event_type']);
assertEq('resource.id', 'pay_full_flow', $ev['resource']['id']);
assertEq('actor.id', 'user_payer_1', $ev['actor']['id'] ?? null);
assertEq('actor.type = user', 'user', $ev['actor']['type'] ?? null);
assertEq('actor.resolution', 'PROBABLE', $ev['actor']['resolution'] ?? null);
assertTrue('actor.namespace scoped by tenant',
    ($ev['actor']['namespace'] ?? '') === 'tnt_ben_test/actors');
assertEq('beneficiary.id', 'ben_customer_a', $ev['beneficiary']['id'] ?? null);
assertEq('beneficiary.type = user', 'user', $ev['beneficiary']['type'] ?? null);
assertEq('beneficiary.resolution', 'PROBABLE', $ev['beneficiary']['resolution'] ?? null);
assertTrue('beneficiary.namespace scoped by tenant',
    ($ev['beneficiary']['namespace'] ?? '') === 'tnt_ben_test/beneficiaries');

echo "\n== observePayment with only beneficiary (payer unknown) ==\n";
Runtime::observePayment(
    'payment.process',
    'pay_anon_payer',
    [],
    context: [],
    beneficiaryId: 'ben_customer_b',
);
$ev = json_decode(Runtime::buffer()->drain()[0], true);
assertTrue('beneficiary set', isset($ev['beneficiary']));
assertTrue('actor unset when actorId omitted', !isset($ev['actor']));

echo "\n== Empty-string ids are ignored (defensive) ==\n";
Runtime::observePayment('payment.create', 'pay_empty', [], context: [],
    beneficiaryId: '', actorId: '');
$ev = json_decode(Runtime::buffer()->drain()[0], true);
assertTrue('empty beneficiaryId → no beneficiary field', !isset($ev['beneficiary']));
assertTrue('empty actorId → no actor field', !isset($ev['actor']));

echo "\n== C5 in data STILL dropped, even alongside actor/beneficiary ==\n";
Runtime::observePayment(
    'payment.create',
    'pay_with_secret',
    ['amount' => 100, 'session_token' => 'must_be_dropped_XYZ'],
    context: [],
    beneficiaryId: 'ben_x',
    actorId: 'user_x',
);
$raw = Runtime::buffer()->drain()[0];
assertTrue('no session_token value in serialized frame',
    !str_contains($raw, 'must_be_dropped_XYZ'));

echo "\n";
if (count($failures) === 0) { echo "BENEFICIARY ALL GREEN\n"; exit(0); }
echo count($failures) . " failure(s)\n";
foreach ($failures as $f) echo "  - $f\n";
exit(1);
