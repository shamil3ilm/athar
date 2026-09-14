<?php

declare(strict_types=1);

namespace Athar;

/**
 * A decision returned by the daemon in response to a synchronous evaluation.
 *
 * Immutable value object (per common/coding-style.md: prefer immutable DTOs
 * for data crossing service boundaries).
 *
 * The daemon returns Decisions for both matched and unmatched policies:
 *   - matched && action != ALLOW  → the daemon has an opinion
 *   - matched == false            → allowed, no policy matched
 *   - outcomeReason != POLICY_MATCH → we didn't reach a real decision
 *     (deadline exceeded, transport failed, shim disabled)
 *
 * `mode` distinguishes OBSERVE (informational — app decides whether to act)
 * from ENFORCE (the app MUST respect the decision). V1 daemon runs all
 * policies in OBSERVE; Stage 2 introduces per-policy ENFORCE.
 */
final class Decision
{
    public const ACTION_ALLOW     = 'ALLOW';
    public const ACTION_CHALLENGE = 'CHALLENGE';
    public const ACTION_RESTRICT  = 'RESTRICT';
    public const ACTION_BLOCK     = 'BLOCK';
    public const ACTION_ALERT     = 'ALERT';

    public const MODE_OBSERVE   = 'OBSERVE';
    public const MODE_CHALLENGE = 'CHALLENGE';
    public const MODE_ENFORCE   = 'ENFORCE';

    public const OUTCOME_POLICY_MATCH      = 'POLICY_MATCH';
    public const OUTCOME_NO_POLICY_APPLIED = 'NO_POLICY_APPLIED';
    public const OUTCOME_DEADLINE_EXCEEDED = 'DEADLINE_EXCEEDED';
    public const OUTCOME_TRANSPORT_ERROR   = 'TRANSPORT_ERROR';
    public const OUTCOME_MALFORMED         = 'MALFORMED_RESPONSE';
    public const OUTCOME_DISABLED          = 'SHIM_DISABLED';
    public const OUTCOME_EXCEPTION         = 'SHIM_EXCEPTION';

    /**
     * @param list<string> $reasonCodes
     */
    public function __construct(
        public readonly string $decisionId,
        public readonly string $action,
        public readonly string $mode,
        public readonly array $reasonCodes,
        public readonly string $outcomeReason,
        public readonly int $latencyUs,
    ) {}

    /** Any non-ALLOW action means at least one policy had an opinion. */
    public function isMatched(): bool
    {
        return $this->outcomeReason === self::OUTCOME_POLICY_MATCH
            && $this->action !== self::ACTION_ALLOW;
    }

    /** Would this decision block or challenge the request if the caller enforces it? */
    public function wouldRestrict(): bool
    {
        return in_array($this->action, [
            self::ACTION_BLOCK,
            self::ACTION_CHALLENGE,
            self::ACTION_RESTRICT,
        ], true);
    }

    /**
     * True if the daemon is in ENFORCE mode for this decision — the calling
     * app MUST respect the action (block/challenge/restrict) or leave open a
     * risk gap. In OBSERVE the app may treat it purely as telemetry.
     */
    public function isEnforced(): bool
    {
        return $this->mode === self::MODE_ENFORCE;
    }

    /** Was this a real daemon decision (vs. a fail-open synthetic)? */
    public function isRealDecision(): bool
    {
        return in_array($this->outcomeReason, [
            self::OUTCOME_POLICY_MATCH,
            self::OUTCOME_NO_POLICY_APPLIED,
        ], true);
    }

    /** Fail-open synthetic — used when we can't reach the daemon within budget. */
    public static function failOpen(string $reason, int $latencyUs = 0): self
    {
        return new self(
            decisionId: 'dec_failopen_' . bin2hex(random_bytes(6)),
            action: self::ACTION_ALLOW,
            mode: self::MODE_OBSERVE,
            reasonCodes: [],
            outcomeReason: $reason,
            latencyUs: $latencyUs,
        );
    }

    /**
     * Parse a decision JSON blob (as sent by the daemon).
     * @param array<string,mixed> $data
     */
    public static function fromResponse(array $data, int $latencyUs): self
    {
        return new self(
            decisionId:    (string) ($data['decision_id']    ?? 'dec_unknown'),
            action:        (string) ($data['action']         ?? self::ACTION_ALLOW),
            mode:          (string) ($data['mode']           ?? self::MODE_OBSERVE),
            reasonCodes:   array_values(array_map('strval', (array) ($data['reason_codes'] ?? []))),
            outcomeReason: (string) ($data['outcome_reason'] ?? self::OUTCOME_POLICY_MATCH),
            latencyUs:     $latencyUs,
        );
    }
}
