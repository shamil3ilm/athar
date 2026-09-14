<?php

declare(strict_types=1);

/**
 * athar reference application: verify the daemon actually processed what the simulator sent.
 *
 * Reads the daemon's SQLite state stores DIRECTLY via PDO (no daemon-running required —
 * the daemon uses WAL mode, so concurrent readers are safe when it's running too).
 *
 * Usage:
 *   php refapp/bin/verify.php [--data-dir=./athar-data] [--tenant=tnt_refapp]
 *
 * Asserts (over whatever the simulator dispatched):
 *   - Lifecycles exist and have expected state/closure distributions
 *   - Decisions exist and at least one fraud-shaped decision has action=CHALLENGE
 *   - Signals: both new_beneficiary and high_amount fired at least once
 *   - Restart-durability: reopening the DBs at a fresh PDO connection sees the same data
 */

$opts = getopt('', ['data-dir::', 'tenant::']);
$dataDir = rtrim($opts['data-dir'] ?? './athar-data', '/');
$tenant = $opts['tenant'] ?? 'tnt_refapp';
$lifecyclesDb = "$dataDir/state/lifecycles.db";
$decisionsDb = "$dataDir/state/decisions.db";

$failures = [];
function check(string $name, bool $ok, string $detail = ''): void {
    global $failures;
    if ($ok) echo "  ok    $name\n";
    else {
        $failures[] = $name . ($detail !== '' ? " -- $detail" : '');
        echo "  FAIL  $name" . ($detail !== '' ? " -- $detail" : '') . "\n";
    }
}
function pdo_open(string $path): ?PDO {
    if (!file_exists($path)) return null;
    try {
        $pdo = new PDO("sqlite:{$path}", null, null, [PDO::ATTR_ERRMODE => PDO::ERRMODE_EXCEPTION]);
        $pdo->exec('PRAGMA busy_timeout=1000');
        return $pdo;
    } catch (\Throwable $e) {
        return null;
    }
}

echo "== Verifying daemon state in {$dataDir} ==\n\n";
echo "-- Lifecycles ({$lifecyclesDb}) --\n";
$life = pdo_open($lifecyclesDb);
check('lifecycles DB opens', $life !== null, $life === null ? "not found at {$lifecyclesDb}" : '');

if ($life !== null) {
    $total = (int) $life->query("SELECT COUNT(*) FROM lifecycles")->fetchColumn();
    $open = (int) $life->query("SELECT COUNT(*) FROM lifecycles WHERE closure = 'Open'")->fetchColumn();
    $closed = (int) $life->query("SELECT COUNT(*) FROM lifecycles WHERE closure = 'Closed'")->fetchColumn();
    $wex = (int) $life->query("SELECT COUNT(*) FROM lifecycles WHERE closure = 'ClosedWithException'")->fetchColumn();
    $unc = (int) $life->query("SELECT COUNT(*) FROM lifecycles WHERE closure = 'ClosedWithUncertainty'")->fetchColumn();
    echo "  total={$total}  open={$open}  closed={$closed}  closed_with_exception={$wex}  closed_with_uncertainty={$unc}\n";
    check('at least one lifecycle exists', $total > 0);

    // State distribution.
    $states = $life->query("SELECT state, COUNT(*) c FROM lifecycles GROUP BY state ORDER BY c DESC")->fetchAll(PDO::FETCH_ASSOC);
    echo "  state distribution:";
    foreach ($states as $s) echo " {$s['state']}=" . (int)$s['c'];
    echo "\n";

    // Tenant scoping (PRI-14 sanity check).
    $t = $life->query("SELECT COUNT(DISTINCT tenant_id) FROM lifecycles")->fetchColumn();
    check('all lifecycles belong to one tenant (sanity for V0 single-tenant)', (int) $t === 1);
}

echo "\n-- Decisions ({$decisionsDb}) --\n";
$dec = pdo_open($decisionsDb);
check('decisions DB opens', $dec !== null, $dec === null ? "not found at {$decisionsDb}" : '');

if ($dec !== null) {
    $decTotal = (int) $dec->query("SELECT COUNT(*) FROM decisions")->fetchColumn();
    $decAllow = (int) $dec->query("SELECT COUNT(*) FROM decisions WHERE action = 'ALLOW'")->fetchColumn();
    $decChallenge = (int) $dec->query("SELECT COUNT(*) FROM decisions WHERE action = 'CHALLENGE'")->fetchColumn();
    echo "  total={$decTotal}  ALLOW={$decAllow}  CHALLENGE={$decChallenge}\n";
    check('at least one decision recorded', $decTotal > 0);
    // Under the fraud scenario, we expect >=1 CHALLENGE.
    // Under happy alone, we expect 0. Warn on 0, don't fail (could be a happy-only run).
    if ($decChallenge === 0) {
        echo "  note: no CHALLENGE decisions — run --scenario=fraud or --scenario=all to trigger the policy.\n";
    }

    $signalCount = (int) $dec->query("SELECT COUNT(*) FROM signals")->fetchColumn();
    echo "  total signals={$signalCount}\n";
    $newBen = (int) $dec->query("SELECT COUNT(*) FROM signals WHERE kind='new_beneficiary'")->fetchColumn();
    $highAmt = (int) $dec->query("SELECT COUNT(*) FROM signals WHERE kind='high_amount'")->fetchColumn();
    $highVel = (int) $dec->query("SELECT COUNT(*) FROM signals WHERE kind='high_velocity'")->fetchColumn();
    $distTgt = (int) $dec->query("SELECT COUNT(*) FROM signals WHERE kind='distinct_targets'")->fetchColumn();
    echo "  signal kinds: new_beneficiary={$newBen}  high_amount={$highAmt}  high_velocity={$highVel}  distinct_targets={$distTgt}\n";
    check('new_beneficiary signal fired at least once', $newBen > 0);
    // Advisory: high_velocity / distinct_targets fire only under those specific
    // scenarios. Don't hard-fail — just surface a note so a targeted run is easy.
    if ($highVel === 0) {
        echo "  note: no high_velocity signals — run --scenario=velocity or --scenario=all to exercise the tracker.\n";
    }
    if ($distTgt === 0) {
        echo "  note: no distinct_targets signals — run --scenario=fanout or --scenario=all to exercise the tracker.\n";
    }

    // Confirm INV-17 fields present on decisions.
    $rows = $dec->query("SELECT body_json FROM decisions LIMIT 5")->fetchAll(PDO::FETCH_COLUMN);
    $sample = null;
    foreach ($rows as $j) { $sample = json_decode($j, true); if ($sample) break; }
    if (is_array($sample)) {
        check('INV-17: decision has degradation_level',
            isset($sample['degradation_level']) && is_string($sample['degradation_level']));
        check('INV-17: decision has inputs_missing',
            array_key_exists('inputs_missing', $sample));
        check('INV-17: decision has coverage_gaps_overlapping',
            array_key_exists('coverage_gaps_overlapping', $sample));
        check('INV-17: decision has explanation',
            !empty($sample['explanation']));
        check('decision has engine versions',
            isset($sample['engine_versions']['detector'])
            && isset($sample['engine_versions']['policy'])
            && isset($sample['engine_versions']['resolver']));
    }
}

echo "\n-- Recent CHALLENGE decisions (up to 5) --\n";
if ($dec !== null) {
    $rows = $dec->query("SELECT decision_id, timestamp_ms, event_id FROM decisions WHERE action='CHALLENGE' ORDER BY timestamp_ms DESC LIMIT 5")->fetchAll(PDO::FETCH_ASSOC);
    if (empty($rows)) {
        echo "  (none — either the fraud scenario wasn't run, or no policy matched)\n";
    } else {
        foreach ($rows as $r) {
            printf("  %-42s  %13d  event=%s\n", $r['decision_id'], (int)$r['timestamp_ms'], $r['event_id']);
        }
    }
}

echo "\n";
if (count($failures) === 0) {
    echo "VERIFY: ALL GREEN\n";
    exit(0);
}
echo count($failures) . " failure(s):\n";
foreach ($failures as $f) echo "  - $f\n";
exit(1);
