<?php

declare(strict_types=1);

/**
 * Emit N observation events to the daemon (real or fake) and exit.
 *
 *   php shim/bin/emit.php [count]
 *
 * Reads ATHAR_* env vars for daemon address (see RuntimeConfig).
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

$count = (int) ($argv[1] ?? 3);

Runtime::enable();
if (!Runtime::isEnabled()) {
    fwrite(STDERR, "athar failed to enable\n");
    exit(2);
}

$factory = Runtime::factory();
for ($i = 0; $i < $count; $i++) {
    // A synthetic HTTP payment observation that also contains secret fields
    // to exercise redaction end-to-end.
    $event = $factory->httpRequest('POST', '/api/payment', [
        'application_id' => 'refapp-laravel',
        'application_version' => '0.0.1',
        'service_id' => 'payment-api',
        'environment' => 'dev',
        'endpoint_id' => 'ep_pay_0001',
        'customer_correlation_ids' => ['request_id' => 'req_' . $i],
    ]);
    $event['data'] = [
        'amount' => 4200 + $i,
        'currency' => 'AED',
        'card_number' => '4111111111111111', // MUST be dropped
        'cvv' => 'DEV_TEST_CVV_MARKER',      // MUST be dropped
        'password' => 'DEV_TEST_PASSWORD_MARKER', // MUST be dropped
    ];
    Runtime::observe($event);
}

Runtime::flush();
echo "emitted {$count} event(s)\n";
