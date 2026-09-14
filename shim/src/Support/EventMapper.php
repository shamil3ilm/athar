<?php

declare(strict_types=1);

namespace Athar\Support;

/**
 * Pure event-type mapping from model state transitions.
 *
 * Split out from the Eloquent trait so it's testable without a Laravel install.
 * Given a lifecycle type prefix and a model's current state, decide which
 * canonical event type ("payment.create", "payment.settle", "payment.fail") to
 * emit for a given persistence phase ("created" / "updated" / "deleted").
 *
 * The mapping intentionally errs on the side of the state machine defined in
 * `athar-lifecycle::engine::transition_for`:
 *
 *   Payment states → payment.* events:
 *     created + status=null                       → payment.create
 *     updated + status transitioned to success    → payment.settle
 *     updated + status transitioned to failed     → payment.fail
 *     updated + status transitioned to cancelled  → payment.cancel
 *     updated + status transitioned to reversed   → payment.reverse
 *     updated (no terminal transition)            → payment.process
 *     deleted                                     → payment.cancel
 *
 * `stateField` names the attribute holding the business state; default `status`.
 * `terminalMap` maps values of that field to lifecycle-terminal semantics.
 */
final class EventMapper
{
    public const PHASE_CREATED = 'created';
    public const PHASE_UPDATED = 'updated';
    public const PHASE_DELETED = 'deleted';

    /**
     * @param string $prefix              e.g. "payment"
     * @param string $phase               PHASE_CREATED | PHASE_UPDATED | PHASE_DELETED
     * @param string|null $currentState   Current value of the state field, if any
     * @param string|null $previousState  Previous value (updated phase only)
     * @param array<string,string> $terminalMap  Optional custom mapping (state → semantic)
     */
    public static function eventType(
        string $prefix,
        string $phase,
        ?string $currentState = null,
        ?string $previousState = null,
        array $terminalMap = [],
    ): string {
        $terminal = $terminalMap ?: self::defaultTerminalMap();

        if ($phase === self::PHASE_DELETED) {
            return "$prefix.cancel";
        }
        if ($phase === self::PHASE_CREATED) {
            // If the record was created already in a terminal state (unusual), respect it.
            if ($currentState !== null && isset($terminal[strtolower($currentState)])) {
                return $prefix . '.' . $terminal[strtolower($currentState)];
            }
            return "$prefix.create";
        }
        // PHASE_UPDATED
        $curr = $currentState !== null ? strtolower($currentState) : null;
        $prev = $previousState !== null ? strtolower($previousState) : null;
        if ($curr !== null && $curr !== $prev && isset($terminal[$curr])) {
            return $prefix . '.' . $terminal[$curr];
        }
        // Non-terminal update: it's just processing.
        return "$prefix.process";
    }

    /** @return array<string,string> business-state → event verb */
    public static function defaultTerminalMap(): array
    {
        return [
            'success'   => 'settle',
            'succeeded' => 'settle',
            'completed' => 'settle',
            'settled'   => 'settle',
            'paid'      => 'settle',
            'failed'    => 'fail',
            'error'     => 'fail',
            'declined'  => 'fail',
            'cancelled' => 'cancel',
            'canceled'  => 'cancel',
            'reversed'  => 'reverse',
            'refunded'  => 'reverse',
        ];
    }
}
