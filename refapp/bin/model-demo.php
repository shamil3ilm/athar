<?php

declare(strict_types=1);

/**
 * refapp: show the ObservesLifecycle trait against an Eloquent-lookalike model.
 *
 * This is what a real Laravel `Payment extends Model` with the trait attached
 * looks like at runtime, without needing composer install / illuminate.
 *
 *   php refapp/bin/model-demo.php
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
require $shim . '/src/Support/EventMapper.php';
require $shim . '/src/Support/ObservesLifecycle.php';
require __DIR__ . '/../src/Payment.php';

use Athar\Runtime;
use Athar\RefApp\Payment;

if (getenv('ATHAR_TENANT_ID') === false) putenv('ATHAR_TENANT_ID=tnt_refapp');
Runtime::enable();
if (!Runtime::isEnabled()) { fwrite(STDERR, "runtime failed\n"); exit(2); }

echo "== ObservesLifecycle demo — Payment model with one trait ==\n\n";

// Case 1: Happy flow — customer creates a payment, gateway settles it.
$payment = new Payment(['amount' => 250.0, 'currency' => 'AED', 'status' => 'processing']);
$payment->save();       // → payment.create emitted
echo "  created payment {$payment->id} in status=processing (emits payment.create)\n";

$payment->status = 'success';
$payment->save();       // → payment.settle emitted (terminal transition)
echo "  updated to status=success (emits payment.settle)\n";

// Case 2: Failed payment
$fail = new Payment(['amount' => 100.0, 'status' => 'processing']);
$fail->save();
echo "  created payment {$fail->id} in status=processing (emits payment.create)\n";
$fail->status = 'failed';
$fail->save();
echo "  updated to status=failed (emits payment.fail)\n";

// Case 3: Cancellation via delete
$cancel = new Payment(['amount' => 50.0]);
$cancel->save();
echo "  created payment {$cancel->id} (emits payment.create)\n";
$cancel->delete();
echo "  deleted (emits payment.cancel)\n";

// Force flush before we finish.
Runtime::flush();
echo "\ndone. 6 events sent. Now run:\n";
echo "  athar lifecycle list <data-dir>/state/lifecycles.db\n";
echo "  athar decision recent <data-dir>/state/decisions.db\n";
