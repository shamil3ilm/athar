<?php

declare(strict_types=1);

namespace Athar\Shim;

/**
 * Data classification per SPEC §4.3.
 *
 * Every field the shim captures MUST be classified before any buffer or disk write.
 * The classification drives redaction (PRI-7, PRI-9), retention (§4.5), and access control.
 *
 * Field-name matching is case-insensitive substring. This is deliberately conservative:
 * if a field looks like a secret, treat it as one. Default for unclassified names is
 * C3 (personal) — capture is opt-in per PRI-8, so unknown = drop.
 */
final class Classify
{
    public const C0_PUBLIC       = 'C0';
    public const C1_TECHNICAL    = 'C1';
    public const C2_PSEUDONYMOUS = 'C2';
    public const C3_PERSONAL     = 'C3';
    public const C4_SENSITIVE    = 'C4';
    public const C5_SECRET       = 'C5';

    /** PRI-9 denylist. Anything containing these substrings in a field name is C5. */
    private const C5_SUBSTRINGS = [
        'authorization',
        'auth_token',
        'cookie',
        'set-cookie',
        'x-api-key',
        'x_api_key',
        'apikey',
        'api_key',
        'password',
        'passwd',
        'passphrase',
        'secret',
        'token',
        'card',
        'cardnumber',
        'card_number',
        'cvv',
        'cvc',
        'pan',
        'iban',
        'swift',
        'routing',
        'ssn',
        'private_key',
        'privatekey',
        'access_key',
        'accesskey',
        'session_id',
        'sessionid',
        'session_token',
        'refresh_token',
        'bearer',
    ];

    private const C4_SUBSTRINGS = [
        'passport',
        'nationalid',
        'national_id',
        'taxid',
        'tax_id',
        'dob',
        'date_of_birth',
        'birthdate',
        'biometric',
        'fingerprint',
        'medical',
        'health',
        'diagnosis',
    ];

    private const C3_SUBSTRINGS = [
        'email',
        'phone',
        'mobile',
        'first_name',
        'last_name',
        'full_name',
        'firstname',
        'lastname',
        'fullname',
        'address',
        'city',
        'postcode',
        'postal_code',
        'zip',
        'ipaddress',
        'ip_address',
        'remote_addr',
    ];

    private const C2_SUBSTRINGS = [
        'user_id',
        'userid',
        'device_id',
        'deviceid',
        'account_id',
        'accountid',
        'customer_id',
        'customerid',
    ];

    private const C1_SUBSTRINGS = [
        'endpoint_id',
        'service_id',
        'deployment_id',
        'trace_id',
        'traceid',
        'request_id',
        'requestid',
        'span_id',
        'latency',
        'duration',
        'status_code',
        'route',
    ];

    private const C0_SUBSTRINGS = [
        'method',
        'framework_version',
        'http_version',
    ];

    public static function classifyField(string $fieldName): string
    {
        $needle = strtolower($fieldName);
        // Order matters: check most-sensitive first so a field named "session_token"
        // classifies as C5 (contains "token"), not C2 (contains "session_id").
        if (self::matchesAny($needle, self::C5_SUBSTRINGS)) return self::C5_SECRET;
        if (self::matchesAny($needle, self::C4_SUBSTRINGS)) return self::C4_SENSITIVE;
        if (self::matchesAny($needle, self::C3_SUBSTRINGS)) return self::C3_PERSONAL;
        if (self::matchesAny($needle, self::C2_SUBSTRINGS)) return self::C2_PSEUDONYMOUS;
        if (self::matchesAny($needle, self::C1_SUBSTRINGS)) return self::C1_TECHNICAL;
        if (self::matchesAny($needle, self::C0_SUBSTRINGS)) return self::C0_PUBLIC;
        // PRI-8: capture is opt-in per field. Unknown → treat as C3 (conservative).
        return self::C3_PERSONAL;
    }

    /**
     * @param string[] $needles
     */
    private static function matchesAny(string $haystack, array $needles): bool
    {
        foreach ($needles as $needle) {
            if (str_contains($haystack, $needle)) {
                return true;
            }
        }
        return false;
    }
}
