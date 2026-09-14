<?php

declare(strict_types=1);

namespace Athar\Shim;

/**
 * ULID generator (Crockford base32, 26 chars).
 *
 * Format: 48-bit millisecond timestamp || 80-bit randomness.
 * Matches the pattern `^[0-9A-HJKMNP-TV-Z]{26}$` used by the canonical event schema.
 *
 * Not cryptographic; sufficient for `event_id` uniqueness under the volumes a single
 * PHP-FPM worker produces.
 */
final class Ulid
{
    private const ALPHABET = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';

    public static function generate(?int $timestampMs = null): string
    {
        $ts = $timestampMs ?? (int) floor(microtime(true) * 1000);
        // 10 bytes = 80 bits of randomness.
        $rand = random_bytes(10);
        return self::encodeTimestamp($ts) . self::encodeRandom($rand);
    }

    private static function encodeTimestamp(int $ts): string
    {
        $out = '';
        for ($i = 9; $i >= 0; $i--) {
            $out .= self::ALPHABET[$ts & 0x1F];
            $ts >>= 5;
        }
        return strrev($out);
    }

    private static function encodeRandom(string $bytes): string
    {
        // 10 bytes = 80 bits → 16 base-32 chars.
        // Read as a big-endian bit stream, 5 bits at a time.
        $bits = '';
        for ($i = 0; $i < 10; $i++) {
            $bits .= str_pad(decbin(ord($bytes[$i])), 8, '0', STR_PAD_LEFT);
        }
        $out = '';
        for ($i = 0; $i < 16; $i++) {
            $chunk = substr($bits, $i * 5, 5);
            $out .= self::ALPHABET[bindec($chunk)];
        }
        return $out;
    }
}
