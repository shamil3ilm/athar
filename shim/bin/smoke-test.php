<?php

declare(strict_types=1);

/**
 * Smoke test for the shim's classification + redaction pipeline.
 *
 * Runs without composer / phpunit. Executes with plain PHP:
 *
 *   php shim/bin/smoke-test.php
 *
 * Purpose: exercise PRI-7 / PRI-9 (C5 drop at capture) and verify that a crafted
 * adversarial payload containing every kind of secret leaves the redaction pass
 * with NO C5 bytes anywhere. This is the essence of V0 criterion 12.
 */

require_once __DIR__ . '/../src/Shim/Classify.php';
require_once __DIR__ . '/../src/Shim/Redact.php';

use Athar\Shim\Classify;
use Athar\Shim\Redact;

$failures = [];
function assertTrue(string $name, bool $cond, string $detail = ''): void
{
    global $failures;
    if ($cond) {
        echo "  ok    $name\n";
    } else {
        $failures[] = "$name" . ($detail !== '' ? " -- $detail" : '');
        echo "  FAIL  $name" . ($detail !== '' ? " -- $detail" : '') . "\n";
    }
}
function section(string $s): void { echo "\n== $s ==\n"; }

section('Classify::classifyField basics');
assertTrue('password → C5', Classify::classifyField('password') === Classify::C5_SECRET);
assertTrue('Authorization → C5', Classify::classifyField('Authorization') === Classify::C5_SECRET);
assertTrue('x-api-key → C5', Classify::classifyField('x-api-key') === Classify::C5_SECRET);
assertTrue('card_number → C5', Classify::classifyField('card_number') === Classify::C5_SECRET);
assertTrue('session_token → C5', Classify::classifyField('session_token') === Classify::C5_SECRET);
assertTrue('refresh_token → C5', Classify::classifyField('refresh_token') === Classify::C5_SECRET);
assertTrue('passport → C4', Classify::classifyField('passport') === Classify::C4_SENSITIVE);
assertTrue('date_of_birth → C4', Classify::classifyField('date_of_birth') === Classify::C4_SENSITIVE);
assertTrue('email → C3', Classify::classifyField('email') === Classify::C3_PERSONAL);
assertTrue('ip_address → C3', Classify::classifyField('ip_address') === Classify::C3_PERSONAL);
assertTrue('user_id → C2', Classify::classifyField('user_id') === Classify::C2_PSEUDONYMOUS);
assertTrue('trace_id → C1', Classify::classifyField('trace_id') === Classify::C1_TECHNICAL);
assertTrue('method → C0', Classify::classifyField('method') === Classify::C0_PUBLIC);
assertTrue('unknown_field → C3 (conservative default)', Classify::classifyField('unknown_field') === Classify::C3_PERSONAL);

section('stripSecrets drops every C5 field, keeps others');
$adversarial = [
    'method'       => 'POST',
    'route'        => '/api/payment',
    'user_id'      => 'user_123',
    'email'        => 'foo@bar.example',
    'password'     => 'p@ssw0rd',
    'authorization'=> 'Bearer eyJhbGciOi...',
    'headers' => [
        'Cookie'        => 'session=abc',
        'X-Api-Key'     => 'sk_live_xxx',
        'User-Agent'    => 'agent/1',
    ],
    'body' => [
        'card_number'   => '4111111111111111',
        'cvv'           => 'CVV_MARKER_VALUE_XYZ',
        'iban'          => 'DE89370400440532013000',
        'amount'        => 4200,
        'currency'      => 'AED',
        'session_token' => 'st_xxx',
    ],
];
[$stripped, $dropped] = Redact::stripSecrets($adversarial);

// Serialize the result and grep for every secret value. If any survived, the test fails hard.
$serialized = json_encode($stripped, JSON_UNESCAPED_SLASHES);
$forbidden = ['p@ssw0rd', 'Bearer eyJhbGciOi', 'session=abc', 'sk_live_xxx', '4111111111111111', 'CVV_MARKER_VALUE_XYZ', 'DE89370400440532013000', 'st_xxx'];
foreach ($forbidden as $needle) {
    assertTrue("no leak: $needle", !str_contains($serialized, $needle), "found leaked value in output");
}
// Everything not C5 should remain.
assertTrue('kept: method', ($stripped['method'] ?? null) === 'POST');
assertTrue('kept: user_id', ($stripped['user_id'] ?? null) === 'user_123');
assertTrue('kept: email (C3, not stripped by stripSecrets)', ($stripped['email'] ?? null) === 'foo@bar.example');
assertTrue('kept: amount', ($stripped['body']['amount'] ?? null) === 4200);
// Dropped path list captures nested keys.
assertTrue('dropped path list includes body.card_number', in_array('body.card_number', $dropped, true));
assertTrue('dropped path list includes headers.X-Api-Key', in_array('headers.X-Api-Key', $dropped, true));

section('apply() enforces PRI-8: opt-in per field, unknown = drop');
$data = [
    'method'    => 'POST',           // C0 → keep
    'route'     => '/x',             // C1 → keep
    'user_id'   => 'user_1',         // C2 → drop unless allow-listed
    'email'     => 'a@b.example',    // C3 → drop
    'password'  => 'pw',             // C5 → drop unconditionally
];
[$filtered, $dropped] = Redact::apply($data, ['user_id']);
assertTrue('C0 kept', array_key_exists('method', $filtered));
assertTrue('C1 kept', array_key_exists('route', $filtered));
assertTrue('C2 kept via allowlist', ($filtered['user_id'] ?? null) === 'user_1');
assertTrue('C3 dropped', !array_key_exists('email', $filtered));
assertTrue('C5 dropped even if allow-listed', !array_key_exists('password', $filtered));

section('apply(): allow-listed C5 is STILL dropped (secrets are not opt-in-able)');
[$out, ] = Redact::apply(['token' => 'xxx', 'method' => 'GET'], ['token', 'method']);
assertTrue('C5 token dropped despite allowlist', !array_key_exists('token', $out));
assertTrue('C0 method kept', ($out['method'] ?? null) === 'GET');

section('Wildcard allowlist prefix.*');
$nested = ['ctx' => ['user_id' => 'u1', 'email' => 'e@e.example'], 'other_field' => 'x'];
[$out, ] = Redact::apply($nested, ['ctx.*']);
assertTrue('nested C2 kept via ctx.*', ($out['ctx']['user_id'] ?? null) === 'u1');
assertTrue('nested C3 kept via ctx.*', ($out['ctx']['email'] ?? null) === 'e@e.example');
assertTrue('outside allowlist dropped', !array_key_exists('other_field', $out));

section('Immutability: input array is not mutated');
$in = ['password' => 'x', 'method' => 'GET'];
$snapshot = $in;
Redact::stripSecrets($in);
Redact::apply($in);
assertTrue('input unchanged', $in === $snapshot);

echo "\n";
if (count($failures) === 0) {
    echo "ALL GREEN\n";
    exit(0);
}
echo count($failures) . " FAILURE(S):\n";
foreach ($failures as $f) echo "  - $f\n";
exit(1);
