<?php

declare(strict_types=1);

/**
 * Smoke test for the shim spool: when the daemon is unreachable, dropped
 * frames are recorded to the spool directory so the daemon can pick them up
 * later.
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
use Athar\Shim\Spool;

$failures = [];
function assertTrue(string $name, bool $cond, string $detail = ''): void {
    global $failures;
    if ($cond) echo "  ok    $name\n";
    else {
        $failures[] = $name . ($detail !== '' ? " -- $detail" : '');
        echo "  FAIL  $name" . ($detail !== '' ? " -- $detail" : '') . "\n";
    }
}
function assertEq(string $name, $expected, $actual): void {
    global $failures;
    if ($expected === $actual) echo "  ok    $name\n";
    else {
        $failures[] = "$name -- expected " . var_export($expected, true) . ", got " . var_export($actual, true);
        echo "  FAIL  $name -- expected " . var_export($expected, true) . ", got " . var_export($actual, true) . "\n";
    }
}

echo "== Spool::writeLoss creates a JSONL record ==\n";
$dir = sys_get_temp_dir() . DIRECTORY_SEPARATOR . 'athar-spool-test-' . getmypid();
@array_map('unlink', glob("$dir/*") ?: []);
@rmdir($dir);
$spool = new Spool($dir);
assertTrue('writeLoss returns true', $spool->writeLoss('daemon_unreachable', 3, 512));
$files = glob("$dir/*.jsonl") ?: [];
assertEq('one spool file exists', 1, count($files));
if (!empty($files)) {
    $raw = file_get_contents($files[0]);
    $rec = json_decode(trim($raw), true);
    assertEq('kind is shim_loss', 'shim_loss', $rec['kind'] ?? null);
    assertEq('reason is daemon_unreachable', 'daemon_unreachable', $rec['reason'] ?? null);
    assertEq('frames_lost = 3', 3, $rec['frames_lost'] ?? null);
    assertEq('bytes_lost = 512', 512, $rec['bytes_lost'] ?? null);
    assertTrue('shim_pid is present', ($rec['shim_pid'] ?? 0) > 0);
    assertTrue('at_ms is a recent timestamp',
        isset($rec['at_ms']) && $rec['at_ms'] > 1_700_000_000_000);
    assertTrue('tenant_id is present', isset($rec['tenant_id']));
}
@array_map('unlink', glob("$dir/*") ?: []);
@rmdir($dir);

echo "\n== Runtime::flush() with no daemon → spool file appears ==\n";
$dir2 = sys_get_temp_dir() . DIRECTORY_SEPARATOR . 'athar-spool-e2e-' . getmypid();
@array_map('unlink', glob("$dir2/*") ?: []);
@rmdir($dir2);

// Point the shim at a port nothing is listening on and a fresh spool dir.
putenv("ATHAR_DAEMON_HOST=127.0.0.1");
putenv("ATHAR_DAEMON_PORT=63" . random_int(100, 999)); // unreachable
putenv("ATHAR_SHIM_SPOOL_DIR={$dir2}");
putenv("ATHAR_TENANT_ID=tnt_spool_test");
putenv("ATHAR_CONNECT_TIMEOUT_MS=50"); // fast fail

Runtime::disableForTesting();
Runtime::enable();
if (!Runtime::isEnabled()) { fwrite(STDERR, "runtime failed to enable\n"); exit(2); }

// Emit two events; they'll fail to reach the daemon.
$factory = Runtime::factory();
for ($i = 0; $i < 2; $i++) {
    $event = $factory->httpRequest('POST', '/api/payment', [
        'application_id' => 'refapp',
        'endpoint_id' => 'ep_test',
    ]);
    Runtime::observe($event);
}
Runtime::flush();

// Give the transport a moment to abandon.
$files = glob("$dir2/*.jsonl") ?: [];
assertEq('one spool file after failed flush', 1, count($files));
if (!empty($files)) {
    $rec = json_decode(trim(file_get_contents($files[0])), true);
    assertEq('reason is daemon_unreachable', 'daemon_unreachable', $rec['reason'] ?? null);
    assertEq('frames_lost = 2 (both events)', 2, $rec['frames_lost'] ?? null);
    assertTrue('bytes_lost > 0', ($rec['bytes_lost'] ?? 0) > 0);
    assertEq('tenant_id matches env', 'tnt_spool_test', $rec['tenant_id'] ?? null);
}

@array_map('unlink', glob("$dir2/*") ?: []);
@rmdir($dir2);

echo "\n";
if (count($failures) === 0) { echo "SPOOL ALL GREEN\n"; exit(0); }
echo count($failures) . " failure(s)\n";
foreach ($failures as $f) echo "  - $f\n";
exit(1);
