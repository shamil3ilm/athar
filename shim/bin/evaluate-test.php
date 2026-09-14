<?php

declare(strict_types=1);

/**
 * End-to-end test for Runtime::evaluate() — synchronous evaluation.
 *
 * Spawns fake-daemon.php (which responds to __evaluate__ frames with a
 * deterministic synthetic decision), then verifies:
 *   - Decision comes back within deadline
 *   - Decision fields are correctly parsed
 *   - Fail-open on unreachable daemon
 *   - No secrets in the frames the daemon sees
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
require $root . '/src/Decision.php';
require $root . '/src/RuntimeConfig.php';
require $root . '/src/Contract/RuntimeInterface.php';
require $root . '/src/Runtime.php';

use Athar\Decision;
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

$port = 11223 + random_int(1, 500);
$outfile = sys_get_temp_dir() . DIRECTORY_SEPARATOR . 'athar-eval-' . getmypid() . '.jsonl';
@unlink($outfile);

// Spawn the fake daemon persistently so both evaluate calls hit it.
$phpBin = PHP_BINARY;
$cmd = escapeshellcmd($phpBin) . ' ' . escapeshellarg($root . '/bin/fake-daemon.php')
    . ' 127.0.0.1 ' . $port . ' ' . escapeshellarg($outfile);
// Explicit env — filter $_SERVER to only string values (otherwise proc_open
// warns "Array to string conversion" on non-string entries like $_SERVER['argv']).
$env = [];
foreach ($_SERVER as $k => $v) {
    if (is_string($v)) $env[$k] = $v;
}
$env['ATHAR_FAKE_PERSIST'] = '1';
$descriptors = [0 => ['pipe', 'r'], 1 => ['pipe', 'w'], 2 => ['pipe', 'w']];
$fake = proc_open($cmd, $descriptors, $pipes, null, $env);
if (!is_resource($fake)) { fwrite(STDERR, "cannot spawn fake daemon\n"); exit(1); }
usleep(300_000); // let it bind

putenv("ATHAR_DAEMON_HOST=127.0.0.1");
putenv("ATHAR_DAEMON_PORT={$port}");
putenv("ATHAR_TENANT_ID=tnt_eval");
putenv("ATHAR_CONNECT_TIMEOUT_MS=500");

Runtime::disableForTesting();
Runtime::enable();
if (!Runtime::isEnabled()) { proc_terminate($fake); fwrite(STDERR, "runtime failed\n"); exit(2); }

$factory = Runtime::factory();

echo "== high-amount event → CHALLENGE decision ==\n";
$event = $factory->businessEvent(
    'payment.create', 'pay_hot_1', 'payment',
    ['amount' => 5000, 'currency' => 'AED', 'session_token' => 'secret_MUST_NOT_LEAK'],
);
$decision = Runtime::evaluate($event, deadlineMs: 500);
assertTrue('is a Decision', $decision instanceof Decision);
assertEq('action', Decision::ACTION_CHALLENGE, $decision->action);
assertEq('mode', Decision::MODE_OBSERVE, $decision->mode);
assertEq('outcomeReason', Decision::OUTCOME_POLICY_MATCH, $decision->outcomeReason);
assertTrue('reason_codes non-empty', !empty($decision->reasonCodes));
assertTrue('decisionId set', $decision->decisionId !== '');
assertTrue('latencyUs recorded', $decision->latencyUs > 0);
assertTrue('isMatched() true', $decision->isMatched());
assertTrue('wouldRestrict() true', $decision->wouldRestrict());
assertTrue('isEnforced() false (V1 all OBSERVE)', !$decision->isEnforced());
assertTrue('isRealDecision() true', $decision->isRealDecision());

echo "\n== low-amount event → ALLOW decision ==\n";
$event2 = $factory->businessEvent(
    'payment.create', 'pay_hot_2', 'payment',
    ['amount' => 50, 'currency' => 'AED'],
);
$decision2 = Runtime::evaluate($event2, deadlineMs: 500);
assertEq('action', Decision::ACTION_ALLOW, $decision2->action);
assertEq('outcomeReason', Decision::OUTCOME_NO_POLICY_APPLIED, $decision2->outcomeReason);
assertTrue('reason_codes empty', empty($decision2->reasonCodes));
assertTrue('isMatched() false', !$decision2->isMatched());
assertTrue('wouldRestrict() false', !$decision2->wouldRestrict());

echo "\n== redaction: session_token stripped from frame the daemon sees ==\n";
// The frame was already logged to $outfile by the fake daemon.
$logged = file_get_contents($outfile);
assertTrue('no session_token leak', !str_contains($logged, 'secret_MUST_NOT_LEAK'));
assertTrue('daemon saw the payment.create event', str_contains($logged, 'payment.create'));

// Shut down fake daemon before testing fail-open path (need an unreachable daemon).
proc_terminate($fake);
foreach ($pipes as $p) @fclose($p);
proc_close($fake);

echo "\n== unreachable daemon → fail-open Decision with DEADLINE_EXCEEDED ==\n";
putenv("ATHAR_DAEMON_PORT=1");
putenv("ATHAR_CONNECT_TIMEOUT_MS=50");
Runtime::disableForTesting();
Runtime::enable();
$event3 = $factory->businessEvent('payment.create', 'pay_offline', 'payment', ['amount' => 5000]);
$decision3 = Runtime::evaluate($event3, deadlineMs: 100);
assertEq('action = ALLOW (fail-open)', Decision::ACTION_ALLOW, $decision3->action);
assertTrue('outcomeReason is a transport-error variant',
    in_array($decision3->outcomeReason, [
        Decision::OUTCOME_DEADLINE_EXCEEDED,
        Decision::OUTCOME_TRANSPORT_ERROR,
    ], true));
assertTrue('isRealDecision() false', !$decision3->isRealDecision());
assertTrue('isMatched() false', !$decision3->isMatched());

@unlink($outfile);
echo "\n";
if (count($failures) === 0) { echo "EVALUATE ALL GREEN\n"; exit(0); }
echo count($failures) . " failure(s)\n";
foreach ($failures as $f) echo "  - $f\n";
exit(1);
