<?php

declare(strict_types=1);

/**
 * Pure-PHP TCP listener that decodes athar shim frames.
 *
 * Only for LOCAL DEVELOPMENT and shim end-to-end testing on hosts where the
 * Rust daemon isn't built. Not a substitute for `athar-daemon`.
 *
 * Usage:
 *   php shim/bin/fake-daemon.php [host] [port] [outfile]
 *
 * Writes one JSON-per-line to $outfile (default: shim/bin/received.jsonl) so a
 * test script can grep the results.
 */

$host = $argv[1] ?? '127.0.0.1';
$port = (int) ($argv[2] ?? 11223);
$outfile = $argv[3] ?? __DIR__ . '/received.jsonl';

@unlink($outfile);

$server = @stream_socket_server("tcp://{$host}:{$port}", $errno, $errstr);
if ($server === false) {
    fwrite(STDERR, "listen failed: $errstr\n");
    exit(1);
}
fwrite(STDERR, "[fake-daemon] listening on {$host}:{$port}, writing to {$outfile}\n");

// One-shot mode: read one connection worth of frames, write them out, exit.
// Optional persistent mode when env var ATHAR_FAKE_PERSIST=1.
$persist = getenv('ATHAR_FAKE_PERSIST') === '1';

do {
    $client = @stream_socket_accept($server, 5);
    if ($client === false) {
        if (!$persist) {
            fwrite(STDERR, "[fake-daemon] accept timeout\n");
            exit(0);
        }
        continue;
    }
    stream_set_timeout($client, 2);
    $count = 0;
    while (!feof($client)) {
        $header = fread_exact($client, 4);
        if ($header === null) break;
        $len = unpack('N', $header)[1];
        if ($len <= 0 || $len > 8 * 1024 * 1024) {
            fwrite(STDERR, "[fake-daemon] bad length $len\n");
            break;
        }
        $body = fread_exact($client, $len);
        if ($body === null) break;
        file_put_contents($outfile, $body . "\n", FILE_APPEND);
        $count++;
    }
    fclose($client);
    fwrite(STDERR, "[fake-daemon] connection closed after $count frame(s)\n");
} while ($persist);

/** Read exactly $n bytes or return null on EOF/short read. */
function fread_exact($sock, int $n): ?string
{
    $buf = '';
    while (strlen($buf) < $n) {
        $chunk = fread($sock, $n - strlen($buf));
        if ($chunk === false || $chunk === '') return null;
        $buf .= $chunk;
    }
    return $buf;
}
