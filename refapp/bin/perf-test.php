<?php

declare(strict_types=1);

/**
 * Performance harness for the shim's hot path.
 *
 * Measures Runtime::observe() overhead — the work a real Laravel request pays
 * per canonical event. Transport (Runtime::flush()) is deliberately excluded;
 * it fires at request end for PHP-FPM and its cost is I/O-bound, not part of
 * the request-latency budget.
 *
 * Reports p50 / p95 / p99 / max latency (microseconds) plus throughput. Fails
 * if p50 > PERF-1 budget or p99 > PERF-2 budget (with a wide multiplier for
 * CI variability).
 *
 * Usage:
 *   php refapp/bin/perf-test.php [--n=N] [--warmup=W] [--fail-on-budget-breach]
 *
 * Defaults:
 *   n=10000, warmup=1000
 *
 * Budgets (SPEC §7.1):
 *   PERF-1: p50 ≤ 200µs added latency
 *   PERF-2: p99 ≤ 1000µs (1ms) added latency
 *   PERF-4: ≤ 2% throughput reduction
 *
 * CI multiplier: shared runners are noisy. We warn at budget * 1x and only
 * fail (with --fail-on-budget-breach) at budget * 5x. Local runs on a quiet
 * dev box should hit budget consistently.
 */

$shim = __DIR__ . '/../../shim';
require $shim . '/src/Shim/Classify.php';
require $shim . '/src/Shim/Redact.php';
require $shim . '/src/Shim/Ulid.php';
require $shim . '/src/Shim/Clock.php';
require $shim . '/src/Shim/Buffer.php';
require $shim . '/src/Shim/Transport.php';
require $shim . '/src/Shim/EventFactory.php';
require $shim . '/src/Shim/Spool.php';
require $shim . '/src/RuntimeConfig.php';
require $shim . '/src/Contract/RuntimeInterface.php';
require $shim . '/src/Runtime.php';

use Athar\Runtime;

$opts = getopt('', ['n::', 'warmup::', 'fail-on-budget-breach']);
$n      = max(100, (int) ($opts['n'] ?? 10000));
$warmup = max(0,   (int) ($opts['warmup'] ?? 1000));
$fail_hard = isset($opts['fail-on-budget-breach']);

// Fixed budgets from SPEC §7.1.
const PERF1_P50_US = 200;
const PERF2_P99_US = 1000;
const CI_TOLERANCE_MULTIPLIER = 5.0; // warn at budget, fail at budget * 5

// Point at a nonsense port so the shim is fully wired but we never actually flush.
// (The hot path we measure is Runtime::observe(), which only buffers.)
putenv("ATHAR_TENANT_ID=tnt_perf");
putenv("ATHAR_DAEMON_HOST=127.0.0.1");
putenv("ATHAR_DAEMON_PORT=1");
putenv("ATHAR_CONNECT_TIMEOUT_MS=10");
// Big buffer so we don't spill during measurement.
putenv("ATHAR_BUFFER_MAX_FRAMES=" . ($n + $warmup + 1000));
putenv("ATHAR_BUFFER_MAX_BYTES=" . (128 * 1024 * 1024));

Runtime::enable();
if (!Runtime::isEnabled()) {
    fwrite(STDERR, "runtime failed to enable\n");
    exit(2);
}

$factory = Runtime::factory();
$ctx = [
    'application_id' => 'refapp-perf',
    'application_version' => '0.0.1',
    'endpoint_id' => 'ep_perf_hot',
];

// Build one event outside the loop so we're not measuring factory work.
// (Adapters that call Runtime::observe() typically build the event once per
// request. The shim's hot path is buffer+redact, not event construction.)
$sample_event = $factory->httpRequest('POST', '/api/payment', $ctx);
$sample_event['data'] = [
    'amount' => 100,
    'currency' => 'AED',
    // include a would-be secret so we exercise the classify+redact fast path too
    'session_token' => 'must_be_dropped_' . bin2hex(random_bytes(4)),
];

// -- warmup ------------------------------------------------------------------
for ($i = 0; $i < $warmup; $i++) {
    Runtime::observe($sample_event);
}
// Drain buffer so its cost doesn't grow with the measurement window.
Runtime::buffer()->drain();

// -- measure -----------------------------------------------------------------
$timings = [];
$start_ns = hrtime(true);
for ($i = 0; $i < $n; $i++) {
    $t0 = hrtime(true);
    Runtime::observe($sample_event);
    $t1 = hrtime(true);
    $timings[$i] = ($t1 - $t0) / 1000.0; // ns → µs
}
$elapsed_s = (hrtime(true) - $start_ns) / 1_000_000_000.0;

// -- report ------------------------------------------------------------------
sort($timings);
$idx = fn(float $q) => (int) min($n - 1, floor($n * $q));
$p50 = $timings[$idx(0.50)];
$p95 = $timings[$idx(0.95)];
$p99 = $timings[$idx(0.99)];
$max = $timings[$n - 1];
$mean = array_sum($timings) / $n;
$throughput = $n / max(0.000001, $elapsed_s);

printf("\n=== shim observe() hot path ===\n");
printf("iterations       %d  (warmup %d)\n", $n, $warmup);
printf("elapsed          %.3f s\n", $elapsed_s);
printf("throughput       %s obs/s\n", number_format($throughput, 0));
printf("mean             %.2f µs\n", $mean);
printf("p50              %.2f µs\n", $p50);
printf("p95              %.2f µs\n", $p95);
printf("p99              %.2f µs\n", $p99);
printf("max              %.2f µs\n", $max);

$issues = 0;
printf("\n=== budgets ===\n");
$check = function (string $name, float $observed, int $budget) use (&$issues): void {
    $ratio = $observed / $budget;
    if ($observed <= $budget) {
        printf("  ok    %-6s %.1fµs  ≤ %dµs  (%.2fx budget)\n", $name, $observed, $budget, $ratio);
    } elseif ($observed <= $budget * CI_TOLERANCE_MULTIPLIER) {
        printf("  WARN  %-6s %.1fµs  > %dµs  (%.2fx budget; within %dx CI tolerance)\n",
            $name, $observed, $budget, $ratio, (int) CI_TOLERANCE_MULTIPLIER);
        $issues++;
    } else {
        printf("  FAIL  %-6s %.1fµs  > %dµs × %d  (%.2fx budget)\n",
            $name, $observed, $budget, (int) CI_TOLERANCE_MULTIPLIER, $ratio);
        $issues += 10;
    }
};
$check('PERF-1', $p50, PERF1_P50_US);
$check('PERF-2', $p99, PERF2_P99_US);

echo "\n";
if ($issues === 0) {
    echo "PERF ALL GREEN — shim overhead within PERF-1 and PERF-2 budgets\n";
    exit(0);
}
if ($fail_hard && $issues > 0) {
    echo "PERF FAILED — --fail-on-budget-breach set and observed values exceeded budget\n";
    exit(1);
}
if ($issues >= 10) {
    echo "PERF FAILED — observed values exceed CI tolerance (budget × " . (int) CI_TOLERANCE_MULTIPLIER . ")\n";
    exit(1);
}
echo "PERF WARN — observed above budget but within CI tolerance (soft)\n";
exit(0);
