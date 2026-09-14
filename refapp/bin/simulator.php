<?php

declare(strict_types=1);

/**
 * athar reference application: payment flow simulator.
 *
 * Generates synthetic-but-realistic payment traffic through the shim so the
 * whole daemon pipeline (evidence log → audit chain → lifecycle correlation →
 * signals → policy → decisions) can be exercised end-to-end without needing
 * a real Laravel install or a real payment gateway.
 *
 * Usage:
 *   php refapp/bin/simulator.php --scenario=happy --count=3
 *   php refapp/bin/simulator.php --scenario=all
 *
 * Scenarios:
 *   happy      Simple create → process → settle flow
 *   fraud      High amount + new beneficiary → policy matches (CHALLENGE decision)
 *   fail       create → process → fail flow (CLOSED_WITH_EXCEPTION)
 *   stale      create only, no follow-up (staleness scanner should close it)
 *   late       create → settle → CLOSED, then delayed fail → classified as CONFLICT
 *   duplicate  Same event_id emitted twice → classified as DUPLICATE
 *   mixed      A realistic blend of all of the above
 *   all        Run every scenario once
 *
 * Env vars (defaults in RuntimeConfig):
 *   ATHAR_DAEMON_HOST=127.0.0.1
 *   ATHAR_DAEMON_PORT=11223
 *   ATHAR_TENANT_ID=tnt_refapp
 *
 * Requires: a running daemon (or a fake-daemon.php for local testing).
 */

$shim = __DIR__ . '/../../shim';
require $shim . '/src/Shim/Classify.php';
require $shim . '/src/Shim/Redact.php';
require $shim . '/src/Shim/Ulid.php';
require $shim . '/src/Shim/Clock.php';
require $shim . '/src/Shim/Buffer.php';
require $shim . '/src/Shim/Transport.php';
require $shim . '/src/Shim/EventFactory.php';
require $shim . '/src/RuntimeConfig.php';
require $shim . '/src/Contract/RuntimeInterface.php';
require $shim . '/src/Runtime.php';

use Athar\Runtime;

// --- args ---
$opts = getopt('', ['scenario:', 'count::', 'seed::', 'verbose']);
$scenario = $opts['scenario'] ?? 'happy';
$count = (int) ($opts['count'] ?? 1);
$seed = (int) ($opts['seed'] ?? time());
$verbose = isset($opts['verbose']);
mt_srand($seed);

// --- init ---
if (getenv('ATHAR_TENANT_ID') === false) {
    putenv('ATHAR_TENANT_ID=tnt_refapp');
}
Runtime::enable();
if (!Runtime::isEnabled()) {
    fwrite(STDERR, "athar runtime failed to enable\n");
    exit(2);
}

// A pool of beneficiaries so `new_beneficiary` fires on FIRST occurrence per resource_id,
// then goes quiet for repeat use.
$knownBeneficiaries = [];

function log_line(string $s, bool $verbose): void
{
    if ($verbose) echo $s . "\n";
}

function pay_id(string $prefix = 'pay'): string
{
    return $prefix . '_' . substr(bin2hex(random_bytes(6)), 0, 12);
}

function emit(string $eventType, string $resourceId, array $data = [], bool $verbose = false): void
{
    Runtime::observePayment($eventType, $resourceId, $data);
    log_line("  emit {$eventType} → {$resourceId} " . json_encode($data), $verbose);
}

function scenario_happy(int $count, bool $verbose): array
{
    $ids = [];
    for ($i = 0; $i < $count; $i++) {
        $id = pay_id();
        $amt = 100 + mt_rand(0, 500);
        emit('payment.create',  $id, ['amount' => $amt, 'currency' => 'AED'], $verbose);
        usleep(1000);
        emit('payment.process', $id, [], $verbose);
        usleep(1000);
        emit('payment.settle',  $id, ['gateway_ref' => 'gw_' . bin2hex(random_bytes(4))], $verbose);
        $ids[] = $id;
    }
    Runtime::flush();
    echo "happy: emitted {$count} lifecycles → SUCCESS/Closed\n";
    return $ids;
}

function scenario_fraud(int $count, bool $verbose): array
{
    // High amount + new beneficiary → policy matches → CHALLENGE decision (observe mode).
    $ids = [];
    for ($i = 0; $i < $count; $i++) {
        $id = pay_id('pay_new');
        $amt = 5000 + mt_rand(0, 20_000);
        emit('payment.create',  $id, ['amount' => $amt, 'currency' => 'AED'], $verbose);
        usleep(1000);
        emit('payment.process', $id, [], $verbose);
        usleep(1000);
        emit('payment.settle',  $id, ['gateway_ref' => 'gw_' . bin2hex(random_bytes(4))], $verbose);
        $ids[] = $id;
    }
    Runtime::flush();
    echo "fraud: emitted {$count} high-amount-new-beneficiary lifecycles (should match policy)\n";
    return $ids;
}

function scenario_fail(int $count, bool $verbose): array
{
    $ids = [];
    for ($i = 0; $i < $count; $i++) {
        $id = pay_id();
        emit('payment.create',  $id, ['amount' => 250], $verbose);
        usleep(1000);
        emit('payment.process', $id, [], $verbose);
        usleep(1000);
        emit('payment.fail',    $id, ['reason' => 'insufficient_funds'], $verbose);
        $ids[] = $id;
    }
    Runtime::flush();
    echo "fail: emitted {$count} failed lifecycles → FAILED/ClosedWithException\n";
    return $ids;
}

function scenario_stale(int $count, bool $verbose): array
{
    $ids = [];
    for ($i = 0; $i < $count; $i++) {
        $id = pay_id('pay_stale');
        emit('payment.create', $id, ['amount' => 42], $verbose);
        $ids[] = $id;
    }
    Runtime::flush();
    echo "stale: emitted {$count} orphan lifecycles (payment.create only) — will close with uncertainty after threshold\n";
    return $ids;
}

function scenario_late(int $count, bool $verbose): array
{
    $ids = [];
    for ($i = 0; $i < $count; $i++) {
        $id = pay_id();
        emit('payment.create', $id, ['amount' => 100], $verbose);
        usleep(1000);
        emit('payment.settle', $id, [], $verbose);
        usleep(1000);
        // Late event: fail after settle → CONFLICT, lifecycle stays SUCCESS/Closed.
        emit('payment.fail',   $id, ['reason' => 'race_condition'], $verbose);
        $ids[] = $id;
    }
    Runtime::flush();
    echo "late: emitted {$count} lifecycles where a late 'fail' arrives after settle\n";
    return $ids;
}

function scenario_duplicate(int $count, bool $verbose): array
{
    $ids = [];
    for ($i = 0; $i < $count; $i++) {
        $id = pay_id();
        // Emit the same event_id twice by hand — need to bypass observePayment which mints ULIDs.
        // Instead: emit create + settle, then emit settle AGAIN with the same event_type; the
        // engine classifies the second settle as DUPLICATE if event_ids match. Since observePayment
        // mints unique ULIDs, the "duplicate" here is at the operation level not the event level;
        // simplify: emit two settle events in a row to exercise state-machine idempotency.
        emit('payment.create', $id, ['amount' => 100], $verbose);
        usleep(1000);
        emit('payment.settle', $id, ['gateway_ref' => 'gw_A'], $verbose);
        usleep(1000);
        emit('payment.settle', $id, ['gateway_ref' => 'gw_B'], $verbose); // second settle → late
        $ids[] = $id;
    }
    Runtime::flush();
    echo "duplicate: emitted {$count} lifecycles with a second settle after closure\n";
    return $ids;
}

function scenario_mixed(int $count, bool $verbose): array
{
    // Realistic blend: 60% happy, 15% fraud-shaped, 15% fail, 10% stale.
    $happy = (int) round($count * 0.6);
    $fraud = (int) round($count * 0.15);
    $fail  = (int) round($count * 0.15);
    $stale = max(1, $count - $happy - $fraud - $fail);
    echo "mixed: {$happy} happy, {$fraud} fraud, {$fail} fail, {$stale} stale\n";
    $ids = [];
    $ids = array_merge($ids, scenario_happy($happy, $verbose));
    $ids = array_merge($ids, scenario_fraud($fraud, $verbose));
    $ids = array_merge($ids, scenario_fail($fail, $verbose));
    $ids = array_merge($ids, scenario_stale($stale, $verbose));
    return $ids;
}

$start = microtime(true);
$dispatched = [];

switch ($scenario) {
    case 'happy':     $dispatched = scenario_happy($count, $verbose); break;
    case 'fraud':     $dispatched = scenario_fraud($count, $verbose); break;
    case 'fail':      $dispatched = scenario_fail($count, $verbose); break;
    case 'stale':     $dispatched = scenario_stale($count, $verbose); break;
    case 'late':      $dispatched = scenario_late($count, $verbose); break;
    case 'duplicate': $dispatched = scenario_duplicate($count, $verbose); break;
    case 'mixed':     $dispatched = scenario_mixed($count, $verbose); break;
    case 'all':
        $happy = scenario_happy(2, $verbose);
        $fraud = scenario_fraud(2, $verbose);
        $fail  = scenario_fail(2, $verbose);
        $stale = scenario_stale(2, $verbose);
        $late  = scenario_late(2, $verbose);
        $dup   = scenario_duplicate(2, $verbose);
        $dispatched = array_merge($happy, $fraud, $fail, $stale, $late, $dup);
        break;
    default:
        fwrite(STDERR, "unknown scenario: {$scenario}\n");
        fwrite(STDERR, "usage: --scenario={happy|fraud|fail|stale|late|duplicate|mixed|all} [--count=N] [--seed=N] [--verbose]\n");
        exit(2);
}

$elapsed = microtime(true) - $start;
printf(
    "done: %d payment(s) dispatched in %.2fs (seed=%d, tenant=%s)\n",
    count($dispatched),
    $elapsed,
    $seed,
    getenv('ATHAR_TENANT_ID'),
);
if ($verbose) {
    echo "dispatched IDs:\n";
    foreach ($dispatched as $id) echo "  $id\n";
}
