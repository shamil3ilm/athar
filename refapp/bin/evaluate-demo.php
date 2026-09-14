<?php

declare(strict_types=1);

/**
 * athar reference application: synchronous evaluation demo.
 *
 * Fire-and-forget `Runtime::observePayment(...)` is fine for telemetry, but
 * some flows need to ACT on the daemon's opinion before proceeding — think
 * "should we let this payment through?".
 *
 * `Runtime::evaluate(...)` sends the event and blocks until the daemon returns
 * a Decision (or the deadline fires). It never throws — a transport failure
 * or timeout produces a fail-open synthetic Decision so the caller can still
 * make a call.
 *
 * Usage:
 *   php refapp/bin/evaluate-demo.php
 *
 * Requires: a running daemon on ATHAR_DAEMON_HOST:ATHAR_DAEMON_PORT.
 *
 * What this exercises:
 *   1. Happy event → daemon evaluates, no policy matches → action=ALLOW.
 *   2. High-amount to a fresh beneficiary → policy A matches → action=CHALLENGE.
 *   3. The response shape: decisionId, action, mode, reasonCodes, outcomeReason,
 *      and how a caller decides whether to gate on it.
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
require $shim . '/src/Decision.php';
require $shim . '/src/Runtime.php';

use Athar\Decision;
use Athar\Runtime;

if (getenv('ATHAR_TENANT_ID') === false) {
    putenv('ATHAR_TENANT_ID=tnt_refapp');
}
Runtime::enable();
if (!Runtime::isEnabled()) {
    fwrite(STDERR, "athar runtime failed to enable\n");
    exit(2);
}

function build_event(string $type, string $paymentId, array $data, ?string $beneficiaryId = null): array
{
    $factory = Runtime::factory();
    return $factory->businessEvent(
        $type,
        $paymentId,
        'payment',
        $data,
        $beneficiaryId !== null
            ? ['beneficiary_id' => $beneficiaryId, 'beneficiary_type' => 'user']
            : [],
    );
}

function print_decision(string $label, Decision $d): void
{
    printf(
        "%s\n  decision_id    : %s\n  action         : %s\n  mode           : %s\n  reason_codes   : [%s]\n  outcome_reason : %s\n  latency_us     : %d\n  isMatched()    : %s\n  wouldRestrict(): %s\n  isEnforced()   : %s\n  isRealDecision(): %s\n\n",
        $label,
        $d->decisionId,
        $d->action,
        $d->mode,
        implode(', ', $d->reasonCodes),
        $d->outcomeReason,
        $d->latencyUs,
        $d->isMatched()      ? 'true' : 'false',
        $d->wouldRestrict()  ? 'true' : 'false',
        $d->isEnforced()     ? 'true' : 'false',
        $d->isRealDecision() ? 'true' : 'false',
    );
}

// ---------- 1) A perfectly ordinary payment ----------
echo "== [1] Happy payment ($100 to a known-looking beneficiary) ==\n";
$happy = build_event(
    'payment.create',
    'pay_' . bin2hex(random_bytes(6)),
    ['amount' => 100, 'currency' => 'AED'],
    beneficiaryId: 'ben_recurring',
);
$d1 = Runtime::evaluate($happy, deadlineMs: 25);
print_decision('  happy result:', $d1);

// ---------- 2) High amount to a NEW beneficiary → policy match ----------
echo "== [2] High-amount payment to a fresh beneficiary (should match policy) ==\n";
$fraudLike = build_event(
    'payment.create',
    'pay_new_' . bin2hex(random_bytes(6)),
    ['amount' => 25_000, 'currency' => 'AED'],
    beneficiaryId: 'ben_fresh_' . bin2hex(random_bytes(6)),
);
$d2 = Runtime::evaluate($fraudLike, deadlineMs: 25);
print_decision('  fraud-shaped result:', $d2);

// ---------- 3) How a caller USES a Decision ----------
echo "== [3] Applying a decision to a hypothetical flow ==\n";
if ($d2->isEnforced() && $d2->wouldRestrict()) {
    echo "  → app WOULD BLOCK the request (policy in ENFORCE mode).\n";
} elseif ($d2->wouldRestrict()) {
    echo "  → app WOULD LOG the challenge (policy in OBSERVE mode — advisory only).\n";
} elseif ($d2->isRealDecision()) {
    echo "  → app WOULD ALLOW the request (daemon had no objection).\n";
} else {
    echo "  → daemon unavailable ({$d2->outcomeReason}); fail-open path — app allowed.\n";
}

echo "\ndone.\n";
