<?php

declare(strict_types=1);

namespace Athar\Shim;

/**
 * TCP-loopback transport (OPS-2, OPS-4).
 *
 * V0 wire protocol: a stream of length-prefixed records.
 *
 *   [len:u32 big-endian][JSON canonical event]
 *   [len:u32 big-endian][JSON canonical event]
 *   ...
 *
 * The shim opens a socket on demand, writes any frames it holds, and closes on
 * flush(). Failure to connect or write is non-fatal to the caller (INV-12,
 * INV-15) — frames stay in the buffer to be retried on next flush (or dropped
 * when the buffer overflows).
 */
final class Transport
{
    private string $host;
    private int $port;
    private int $connectTimeoutMs;

    public function __construct(string $host = '127.0.0.1', int $port = 11223, int $connectTimeoutMs = 100)
    {
        $this->host = $host;
        $this->port = $port;
        $this->connectTimeoutMs = $connectTimeoutMs;
    }

    /**
     * Attempt to send the given frames. Returns the number successfully written.
     * A partial write returns the count that were fully written; the rest stay for retry.
     *
     * @param list<string> $frames
     */
    public function send(array $frames): int
    {
        if (empty($frames)) return 0;
        $errno = 0;
        $errstr = '';
        // Non-blocking connect with a hard timeout so the shim never stalls the request.
        $sock = @stream_socket_client(
            "tcp://{$this->host}:{$this->port}",
            $errno,
            $errstr,
            $this->connectTimeoutMs / 1000,
            STREAM_CLIENT_CONNECT,
        );
        if ($sock === false) {
            @error_log("[athar] transport connect failed: $errstr");
            return 0;
        }
        // Short write timeout so a slow daemon can't hold up shutdown.
        stream_set_timeout($sock, 0, 200_000);
        $written = 0;
        try {
            foreach ($frames as $frame) {
                $len = strlen($frame);
                if ($len > 0x7fff_ffff) {
                    // > 2 GB single frame is nonsense here; skip.
                    continue;
                }
                $header = pack('N', $len);
                $payload = $header . $frame;
                $offset = 0;
                $remain = strlen($payload);
                while ($remain > 0) {
                    $n = @fwrite($sock, substr($payload, $offset), $remain);
                    if ($n === false || $n === 0) {
                        return $written; // partial: caller retries the rest
                    }
                    $offset += $n;
                    $remain -= $n;
                }
                $written++;
            }
        } finally {
            @fclose($sock);
        }
        return $written;
    }

    /**
     * Send one frame and wait for a single response frame. Used by the
     * synchronous evaluation path (Runtime::evaluate).
     *
     * Returns the response payload bytes, or null on connect/write/read
     * failure or timeout. `deadlineMs` is a hard cap for the whole round trip.
     */
    public function sendAndReceive(string $frame, int $deadlineMs): ?string
    {
        $errno = 0;
        $errstr = '';
        // We use a socket-per-call for V0. Not the fastest, but simple. A
        // pooled long-lived socket is a Stage 2 optimization.
        $sock = @stream_socket_client(
            "tcp://{$this->host}:{$this->port}",
            $errno,
            $errstr,
            $this->connectTimeoutMs / 1000,
            STREAM_CLIENT_CONNECT,
        );
        if ($sock === false) {
            return null;
        }
        // Enforce the deadline as read/write timeouts. Read may block up to
        // deadlineMs. Write completes almost instantly (loopback), so most of
        // the budget is available for the daemon's compute + response.
        $sec = intdiv($deadlineMs, 1000);
        $usec = ($deadlineMs % 1000) * 1000;
        stream_set_timeout($sock, $sec, $usec);
        try {
            $len = strlen($frame);
            if ($len === 0 || $len > 0x7fff_ffff) return null;
            $payload = pack('N', $len) . $frame;
            $offset = 0;
            $remain = strlen($payload);
            while ($remain > 0) {
                $n = @fwrite($sock, substr($payload, $offset), $remain);
                if ($n === false || $n === 0) return null;
                $offset += $n;
                $remain -= $n;
            }
            // Read response: [len:u32-BE][body:len].
            $header = self::readExact($sock, 4);
            if ($header === null) return null;
            $respLen = unpack('N', $header)[1];
            if ($respLen <= 0 || $respLen > 8 * 1024 * 1024) return null;
            return self::readExact($sock, $respLen);
        } finally {
            @fclose($sock);
        }
    }

    private static function readExact($sock, int $n): ?string
    {
        $buf = '';
        while (strlen($buf) < $n) {
            $chunk = @fread($sock, $n - strlen($buf));
            if ($chunk === false || $chunk === '') {
                // Includes timeout — stream_get_meta_data($sock)['timed_out'] is true.
                return null;
            }
            $buf .= $chunk;
        }
        return $buf;
    }
}
