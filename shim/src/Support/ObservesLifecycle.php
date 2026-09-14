<?php

declare(strict_types=1);

namespace Athar\Support;

use Athar\Runtime;

/**
 * Eloquent model trait: auto-emit lifecycle events on model state changes.
 *
 * Works for any domain — payment, invoice, order, refund, subscription,
 * transfer, login attempt, whatever. `$atharLifecycleType` names the domain
 * and appears in the emitted event type (`{type}.create` etc.) and the
 * resource type on the canonical event.
 *
 * Payment example:
 *
 *   class Payment extends Model
 *   {
 *       use \Athar\Support\ObservesLifecycle;
 *
 *       protected string $atharLifecycleType   = 'payment';
 *       protected string $atharResourcePrefix  = 'pay';       // → resource.id = "pay_{id}"
 *       // optional overrides:
 *       // protected string $atharStateField     = 'status';
 *       // protected array  $atharTerminalMap    = [...];      // custom state → verb
 *       // override atharObservableData() to pick payload fields
 *   }
 *
 * Invoice example:
 *
 *   class Invoice extends Model
 *   {
 *       use \Athar\Support\ObservesLifecycle;
 *       protected string $atharLifecycleType   = 'invoice';
 *       protected string $atharResourcePrefix  = 'inv';
 *   }
 *
 * Behaviour:
 *   - On create → emits "{type}.create"
 *   - On update with a terminal state transition (status → success/failed/...) → emits the mapped event
 *   - On update with no terminal transition → emits "{type}.process"
 *   - On delete → emits "{type}.cancel"
 *
 * INV-15: all emission is wrapped in a Throwable-catching guard so a shim
 * failure NEVER breaks the model save.
 */
trait ObservesLifecycle
{
    /**
     * Eloquent auto-invokes bootTraits during model boot; hook here.
     */
    public static function bootObservesLifecycle(): void
    {
        static::created(function ($model) { self::atharDispatch($model, EventMapper::PHASE_CREATED); });
        static::updated(function ($model) { self::atharDispatch($model, EventMapper::PHASE_UPDATED); });
        static::deleted(function ($model) { self::atharDispatch($model, EventMapper::PHASE_DELETED); });
    }

    /** Dispatch a lifecycle event for one persistence phase. */
    private static function atharDispatch($model, string $phase): void
    {
        try {
            $prefix = property_exists($model, 'atharLifecycleType')
                ? (string) $model->atharLifecycleType
                : 'record';
            $stateField = property_exists($model, 'atharStateField')
                ? (string) $model->atharStateField
                : 'status';
            $resourcePrefix = property_exists($model, 'atharResourcePrefix')
                ? (string) $model->atharResourcePrefix
                : $prefix;
            $terminalMap = property_exists($model, 'atharTerminalMap')
                ? (array) $model->atharTerminalMap
                : [];

            $currentState = self::readAttr($model, $stateField);
            $previousState = self::readOriginalAttr($model, $stateField);
            $eventType = EventMapper::eventType(
                $prefix,
                $phase,
                self::toStringOrNull($currentState),
                self::toStringOrNull($previousState),
                $terminalMap,
            );

            $key = method_exists($model, 'getKey') ? $model->getKey() : null;
            if ($key === null) return;
            $resourceId = "{$resourcePrefix}_{$key}";
            $data = method_exists($model, 'atharObservableData')
                ? (array) $model->atharObservableData()
                : self::defaultObservableData($model);

            Runtime::observePayment($eventType, $resourceId, $data);
        } catch (\Throwable $e) {
            @error_log('[athar] ObservesLifecycle emit failed: ' . $e->getMessage());
        }
    }

    /** Default: expose amount + currency only (safe C1 fields). */
    private static function defaultObservableData($model): array
    {
        return array_filter([
            'amount'   => self::readAttr($model, 'amount'),
            'currency' => self::readAttr($model, 'currency'),
        ], fn($v) => $v !== null);
    }

    private static function readAttr($model, string $attr)
    {
        if (method_exists($model, 'getAttribute')) {
            return $model->getAttribute($attr);
        }
        return $model->$attr ?? null;
    }

    private static function readOriginalAttr($model, string $attr)
    {
        if (method_exists($model, 'getOriginal')) {
            return $model->getOriginal($attr);
        }
        return null;
    }

    private static function toStringOrNull($value): ?string
    {
        if ($value === null) return null;
        return is_scalar($value) ? (string) $value : null;
    }
}
