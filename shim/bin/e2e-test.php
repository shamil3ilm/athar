<?php

declare(strict_types=1);

/**
 * End-to-end shim test on this host, using the pure-PHP fake daemon.
 *
 * 1. Spawn the fake daemon on 127.0.0.1:$port
 * 2. Emit N events through the shim
 * 3. Wait for the daemon to write its output file
 * 4. Read the file, verify count, schema shape, and — critically — that no
 *    C5 marker string appears in the daemon-visible bytes (PRI-7 end-to-end).
 */

$root = dirname(__DIR__);
$port = 11223 + random_int(1, 500); // avoid collisions on repeat runs
$outfile = sys_get_temp_dir() . DIRECTORY_SEPARATOR . 'athar-e2e-' . getmypid() . '.jsonl';
$count = 5;

$phpBin = PHP_BINARY;
$fakeCmd = escapeshellcmd($phpBin) . ' ' . escapeshellarg($root . '/bin/fake-daemon.php')
    . ' 127.0.0.1 ' . $port . ' ' . escapeshellarg($outfile);

$descriptors = [
    0 => ['pipe', 'r'],
    1 => ['pipe', 'w'],
    2 => ['pipe', 'w'],
];
$fake = proc_open($fakeCmd, $descriptors, $pipes);
if (!is_resource($fake)) {
    fwrite(STDERR, "failed to start fake daemon\n");
    exit(1);
}
// Give the fake daemon a moment to bind.
usleep(200_000);

// Emit events via the shim.
putenv("ATHAR_DAEMON_HOST=127.0.0.1");
putenv("ATHAR_DAEMON_PORT={$port}");
putenv("ATHAR_TENANT_ID=tnt_e2e");
$emitCmd = escapeshellcmd($phpBin) . ' ' . escapeshellarg($root . '/bin/emit.php') . ' ' . $count;
$emitOut = shell_exec($emitCmd . ' 2>&1');
echo "[emit] " . trim((string) $emitOut) . "\n";

// Wait for the fake daemon to finish writing (accept timeout is 5s).
$deadline = microtime(true) + 6.0;
while (microtime(true) < $deadline) {
    $status = proc_get_status($fake);
    if (!$status['running']) break;
    usleep(100_000);
}
proc_terminate($fake);
foreach ($pipes as $p) @fclose($p);
proc_close($fake);

if (!file_exists($outfile)) {
    fwrite(STDERR, "FAIL: no output file at $outfile\n");
    exit(1);
}
$raw = file_get_contents($outfile);
$lines = array_values(array_filter(explode("\n", $raw), fn($s) => $s !== ''));

$failures = [];
$assert = function (string $name, bool $ok, string $detail = '') use (&$failures) {
    if ($ok) echo "  ok    $name\n";
    else {
        $failures[] = $name . ($detail !== '' ? " -- $detail" : '');
        echo "  FAIL  $name" . ($detail !== '' ? " -- $detail" : '') . "\n";
    }
};

echo "\n== E2E: {$count} emitted, " . count($lines) . " received ==\n";
$assert('received count matches emitted', count($lines) === $count, count($lines) . " != $count");

$firstEvent = null;
foreach ($lines as $i => $line) {
    $obj = json_decode($line, true);
    if (!is_array($obj)) {
        $assert("line $i is JSON", false, "not JSON: " . substr($line, 0, 60));
        continue;
    }
    if ($i === 0) $firstEvent = $obj;
    $assert("line $i has schema_version 1.0", ($obj['schema_version'] ?? null) === '1.0');
    $assert("line $i has tenant_id tnt_e2e", ($obj['tenant_id'] ?? null) === 'tnt_e2e');
    $assert("line $i has event_type http.request", ($obj['event_type'] ?? null) === 'http.request');
    $assert("line $i has ULID-shaped event_id",
        is_string($obj['event_id'] ?? null) && (bool) preg_match('/^[0-9A-HJKMNP-TV-Z]{26}$/', $obj['event_id']));
}

echo "\n== PRI-7 end-to-end: no C5 markers reach daemon ==\n";
$forbidden = ['4111111111111111', 'DEV_TEST_CVV_MARKER', 'DEV_TEST_PASSWORD_MARKER'];
foreach ($forbidden as $m) {
    $assert("no leak: $m", !str_contains($raw, $m));
}

echo "\n== Sample event structure ==\n";
if ($firstEvent) {
    foreach (['schema_version', 'event_id', 'event_type', 'tenant_id', 'clock', 'provenance', 'truth', 'trust', 'causality', 'coverage'] as $k) {
        $assert("first event has $k", array_key_exists($k, $firstEvent));
    }
}

@unlink($outfile);
echo "\n";
if (count($failures) === 0) { echo "E2E ALL GREEN\n"; exit(0); }
echo count($failures) . " failure(s)\n";
foreach ($failures as $f) echo "  - $f\n";
exit(1);
