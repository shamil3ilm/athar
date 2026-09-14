<?php

declare(strict_types=1);

namespace Athar\Shim;

/**
 * Redaction (PRI-7, PRI-9).
 *
 * Runs INSIDE the application process, before anything touches the shim's buffer.
 * C5 (secrets) MUST be dropped here — there is no "capture then filter" path (PRI-7).
 *
 * Two entry points:
 *
 *   Redact::stripSecrets($data)
 *       Mandatory pass. Drops every field whose name matches the C5 denylist.
 *       Returns [redacted_data, list_of_dropped_paths].
 *
 *   Redact::apply($data, $allowlist)
 *       Full classification-driven pass. Keeps fields explicitly listed in $allowlist
 *       plus everything at C0/C1. Drops everything else. This is the PRI-8 posture
 *       ("opt-in per field, never capture-everything-and-filter-later").
 *
 * Both are immutable: neither mutates its input array.
 */
final class Redact
{
    /**
     * Mandatory C5 pass. Drops secrets; keeps everything else.
     *
     * @param array<mixed> $data
     * @return array{0: array<mixed>, 1: list<string>}
     */
    public static function stripSecrets(array $data): array
    {
        $droppedPaths = [];
        $result = self::walkStripSecrets('', $data, $droppedPaths);
        return [$result, $droppedPaths];
    }

    /**
     * Classification-driven redaction. Everything that isn't C0/C1 or in the allowlist is dropped.
     *
     * @param array<mixed> $data
     * @param list<string> $allowlist Field paths (dot-separated) explicitly allowed regardless of class.
     * @return array{0: array<mixed>, 1: list<string>}
     */
    public static function apply(array $data, array $allowlist = []): array
    {
        $droppedPaths = [];
        $result = self::walkApply('', $data, $allowlist, $droppedPaths);
        return [$result, $droppedPaths];
    }

    /**
     * @param array<mixed> $data
     * @param list<string> $droppedPaths
     * @return array<mixed>
     */
    private static function walkStripSecrets(string $path, array $data, array &$droppedPaths): array
    {
        $out = [];
        foreach ($data as $key => $value) {
            $childPath = $path === '' ? (string) $key : $path . '.' . $key;
            $class = is_string($key) ? Classify::classifyField($key) : Classify::C1_TECHNICAL;
            if ($class === Classify::C5_SECRET) {
                $droppedPaths[] = $childPath;
                continue;
            }
            if (is_array($value)) {
                $out[$key] = self::walkStripSecrets($childPath, $value, $droppedPaths);
            } else {
                $out[$key] = $value;
            }
        }
        return $out;
    }

    /**
     * @param array<mixed> $data
     * @param list<string> $allowlist
     * @param list<string> $droppedPaths
     * @return array<mixed>
     */
    private static function walkApply(string $path, array $data, array $allowlist, array &$droppedPaths): array
    {
        $out = [];
        foreach ($data as $key => $value) {
            $childPath = $path === '' ? (string) $key : $path . '.' . $key;
            $class = is_string($key) ? Classify::classifyField($key) : Classify::C1_TECHNICAL;

            $isAllowed = self::inAllowlist($childPath, $allowlist);
            // C5 is dropped even if allow-listed. Secrets are not opt-in-able.
            if ($class === Classify::C5_SECRET) {
                $droppedPaths[] = $childPath;
                continue;
            }
            $keep = $isAllowed
                || $class === Classify::C0_PUBLIC
                || $class === Classify::C1_TECHNICAL;
            if (!$keep) {
                $droppedPaths[] = $childPath;
                continue;
            }
            if (is_array($value)) {
                $out[$key] = self::walkApply($childPath, $value, $allowlist, $droppedPaths);
            } else {
                $out[$key] = $value;
            }
        }
        return $out;
    }

    /**
     * @param list<string> $allowlist
     */
    private static function inAllowlist(string $path, array $allowlist): bool
    {
        foreach ($allowlist as $entry) {
            if ($entry === $path) {
                return true;
            }
            // Support wildcard suffix "prefix.*".
            if (str_ends_with($entry, '.*')) {
                $prefix = substr($entry, 0, -2);
                if (str_starts_with($path, $prefix . '.') || $path === $prefix) {
                    return true;
                }
            }
        }
        return false;
    }
}
