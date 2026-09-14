<?php

declare(strict_types=1);

/**
 * Reachability probe test.
 *
 * Verifies Runtime::ping() reports the expected state:
 *   - false BEFORE enable() is called
 *   - true when SOMETHING is listening on the configured port
 *   - false when NOTHING is listening
 *   - never throws, even on unresolvable host
 *
 * Uses stream_socket_server in the same process — much simpler than spawning
 * a subprocess and juggling persist/kill lifecycles. `ping()` performs a
 * connect + close; the OS accepts the connection at the socket layer whether
 * or not userspace ever calls accept(), so this is sufficient to verify
 * TCP reachability semantics.
 */

$root = dirname(__DIR__);

require_once $root . '/src/Shim/Classify.php';
require_once $root . '/src/Shim/Redact.php';
require_once $root . '/src/Shim/Ulid.php';
require_once $root . '/src/Shim/Clock.php';
require_once $root . '/src/Shim/Buffer.php';
require_once $root . '/src/Shim/Transport.php';
require_once $root . '/src/Shim/EventFactory.php';
require_once $root . '/src/Shim/Spool.php';
require_once $root . '/src/RuntimeConfig.php';
require_once $root . '/src/Contract/RuntimeInterface.php';
require_once $root . '/src/Decision.php';
require_once $root . '/src/Runtime.php';

use Athar\Runtime;

$failures = [];
function check(string $name, bool $ok, string $detail = ''): void
{
    global $failures;
    if ($ok) {
        echo "  ok    $name\n";
    } else {
        $failures[] = $name . ($detail !== '' ? " -- $detail" : '');
        echo "  FAIL  $name" . ($detail !== '' ? " -- $detail" : '') . "\n";
    }
}

// ----- Case A: ping BEFORE enable() ------------------------------------------

echo "\n== A: ping() before enable() ==\n";
Runtime::disableForTesting();
check('ping returns false when shim is not enabled', Runtime::ping() === false);

// ----- Case B: ping when a listener is up ------------------------------------

$port = 12200 + random_int(1, 500);
putenv("ATHAR_DAEMON_HOST=127.0.0.1");
putenv("ATHAR_DAEMON_PORT={$port}");
putenv("ATHAR_TENANT_ID=tnt_ping");

$errno = 0;
$errstr = '';
$listener = @stream_socket_server("tcp://127.0.0.1:{$port}", $errno, $errstr);
if ($listener === false) {
    fwrite(STDERR, "cannot bind test listener on port {$port}: {$errstr}\n");
    exit(1);
}

echo "\n== B: ping() with listener bound on 127.0.0.1:{$port} ==\n";
Runtime::enable();
check('shim enabled', Runtime::isEnabled());
$result = Runtime::ping();
check('ping returns true when a listener is bound', $result === true,
    $result === true ? '' : 'expected true, got false');

fclose($listener);
// The OS may hold the port in TIME_WAIT briefly, but our ping opens a
// fresh outbound connection. A no-longer-bound port refuses connects
// immediately (RST) on all supported platforms.

// ----- Case C: ping when nothing is listening -------------------------------

echo "\n== C: ping() with no listener on 127.0.0.1:{$port} ==\n";
$result = Runtime::ping();
check('ping returns false when nothing is listening', $result === false,
    $result === false ? '' : 'expected false, got true');

// ----- Case D: ping never throws --------------------------------------------

echo "\n== D: ping() never propagates exceptions ==\n";
Runtime::disableForTesting();
putenv("ATHAR_DAEMON_HOST=127.0.0.1");
putenv("ATHAR_DAEMON_PORT=1");   // reserved / never listened
Runtime::enable();
$threw = false;
try {
    Runtime::ping();
} catch (\Throwable $e) {
    $threw = true;
}
check('ping does not throw on unreachable port', !$threw);

echo "\n";
if (count($failures) === 0) {
    echo "PING TEST: ALL GREEN\n";
    exit(0);
}
echo count($failures) . " failure(s):\n";
foreach ($failures as $f) echo "  - $f\n";
exit(1);
