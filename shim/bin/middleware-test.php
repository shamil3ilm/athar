<?php

declare(strict_types=1);

/**
 * Smoke test for the Laravel HTTP middleware without needing a full Laravel install.
 *
 * We fake Laravel's request/response contracts with duck-typed anonymous classes.
 * The middleware only reads method(), path(), route()->uri(), and $response->status()
 * — well within what a mock can satisfy.
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
require $root . '/src/Adapter/Laravel/HttpMiddleware.php';

use Athar\Runtime;
use Athar\Adapter\Laravel\HttpMiddleware;

$failures = [];
function assertTrue(string $name, bool $cond, string $detail = ''): void
{
    global $failures;
    if ($cond) echo "  ok    $name\n";
    else {
        $failures[] = "$name" . ($detail !== '' ? " -- $detail" : '');
        echo "  FAIL  $name" . ($detail !== '' ? " -- $detail" : '') . "\n";
    }
}

// Enable runtime; buffer will capture events instead of trying to talk to a real daemon.
Runtime::enable();
if (!Runtime::isEnabled()) {
    fwrite(STDERR, "runtime failed to enable\n");
    exit(2);
}

// Fake Laravel request/response.
$route = new class {
    public function uri(): string { return 'api/payment/{id}'; }
};
$request = new class($route) {
    private $route;
    public function __construct($route) { $this->route = $route; }
    public function method(): string { return 'POST'; }
    public function path(): string { return 'api/payment/42'; }
    public function route() { return $this->route; }
};
$response = new class {
    public function status(): int { return 200; }
};

$mw = new HttpMiddleware();

echo "== HTTP middleware emits a canonical http.request event ==\n";
$before = Runtime::buffer()->count();
$mw->emit($request, $response, 12345);
$after = Runtime::buffer()->count();
assertTrue('one event buffered', ($after - $before) === 1);

$frames = Runtime::buffer()->drain();
$event = json_decode($frames[0], true);
assertTrue('event_type is http.request', ($event['event_type'] ?? null) === 'http.request');
assertTrue('method is POST', ($event['entry_point']['method'] ?? null) === 'POST');
assertTrue('route_template is /api/payment/{id}',
    ($event['entry_point']['route_template'] ?? null) === 'api/payment/{id}');
assertTrue('concrete route captured', ($event['entry_point']['route'] ?? '') !== '');
assertTrue('endpoint_id is stable and prefixed',
    isset($event['entry_point']['endpoint_id'])
    && str_starts_with($event['entry_point']['endpoint_id'], 'ep_'));
assertTrue('data.status_code carries the response status', ($event['data']['status_code'] ?? null) === 200);
assertTrue('data.latency_us carries the latency', ($event['data']['latency_us'] ?? null) === 12345);
assertTrue('provenance origin=UNKNOWN (V0: no auth inference)', ($event['provenance']['origin'] ?? null) === 'UNKNOWN');
assertTrue('provenance trigger=API_REQUEST', ($event['provenance']['trigger'] ?? null) === 'API_REQUEST');
assertTrue('coverage.redacted_fields declared',
    is_array($event['coverage']['redacted_fields'] ?? null)
    && in_array('headers.authorization', $event['coverage']['redacted_fields'], true));

echo "\n== Same endpoint → same endpoint_id ==\n";
$mw->emit($request, $response, 100);
$mw->emit($request, $response, 100);
$two = Runtime::buffer()->drain();
$e1 = json_decode($two[0], true);
$e2 = json_decode($two[1], true);
assertTrue('endpoint_id is deterministic per (method, route_template)',
    $e1['entry_point']['endpoint_id'] === $e2['entry_point']['endpoint_id']);

echo "\n== Middleware NEVER throws into caller ==\n";
$broken_request = new class {
    public function method() { throw new \RuntimeException('adapter probe'); }
    public function path() { throw new \RuntimeException('adapter probe'); }
    public function route() { throw new \RuntimeException('adapter probe'); }
};
$broken_response = new class {
    public function status() { throw new \RuntimeException('adapter probe'); }
};
try {
    $mw->emit($broken_request, $broken_response, 0);
    assertTrue('emit swallowed exceptions', true);
} catch (\Throwable $e) {
    assertTrue('emit swallowed exceptions', false, 'threw: ' . $e->getMessage());
}

echo "\n";
if (count($failures) === 0) { echo "MIDDLEWARE ALL GREEN\n"; exit(0); }
echo count($failures) . " failure(s)\n";
foreach ($failures as $f) echo "  - $f\n";
exit(1);
