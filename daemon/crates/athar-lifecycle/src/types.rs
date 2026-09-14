//! Lifecycle domain types (SPEC §5.11).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LifecycleType {
    Payment,
    // Future: Invoice, Refund, Onboarding, Order, ...
}

impl LifecycleType {
    /// Map an event type ("payment.create", "payment.settle") to the lifecycle type
    /// it belongs to. Return None if the event does not initiate a known type.
    pub fn from_event_type(event_type: &str) -> Option<Self> {
        if event_type.starts_with("payment.") {
            Some(LifecycleType::Payment)
        } else {
            None
        }
    }
}

/// Lifecycle states (SPEC §5.11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum State {
    Started,
    Processing,
    Pending,
    Success,
    Failed,
    Cancelled,
    Rejected,
    Timeout,
    Expired,
    Abandoned,
    Reversed,
    Compensated,
    PartiallyCompleted,
    Conflicted,
    Unknown,
}

impl State {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            State::Success
                | State::Failed
                | State::Cancelled
                | State::Rejected
                | State::Timeout
                | State::Expired
                | State::Abandoned
                | State::Reversed
                | State::Compensated
        )
    }
}

/// Closure states (SPEC §5.11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Closure {
    Open,
    Closed,
    ClosedWithException,
    ClosedWithUncertainty,
}

impl Closure {
    pub fn is_closed(self) -> bool { !matches!(self, Closure::Open) }
}

/// Correlation tier (SPEC §5.9). Recorded on every match so a decision built on the
/// binding remains explainable months later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InferenceTier {
    Explicit,
    BusinessId,
    Causation,
    ResourceId,
    Unknown,
}

impl InferenceTier {
    pub fn confidence(self) -> f32 {
        match self {
            InferenceTier::Explicit => 1.00,
            InferenceTier::BusinessId => 0.95,
            InferenceTier::Causation => 0.90,
            InferenceTier::ResourceId => 0.80,
            InferenceTier::Unknown => 0.0,
        }
    }
}

/// One lifecycle instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lifecycle {
    pub id: String,
    pub tenant_id: String,
    pub lifecycle_type: LifecycleType,
    pub business_key: Option<String>,
    pub resource_id: Option<String>,
    pub state: State,
    pub closure: Closure,
    pub started_at_ms: u64,
    pub last_event_at_ms: u64,
    pub closed_at_ms: Option<u64>,
    pub event_ids: Vec<String>,
    pub late_events: Vec<LateEvent>,
    /// Staleness threshold in milliseconds; MOD-25.
    pub staleness_ms: u64,
    /// Correlation tier by which each event was matched. Same order as `event_ids`.
    pub tiers: Vec<InferenceTier>,
}

/// A late event: arrived after the lifecycle was closed. Classified (MOD-27) but
/// not applied — the lifecycle stays closed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LateEvent {
    pub event_id: String,
    pub event_type: String,
    pub arrived_at_ms: u64,
    pub class: LateEventClass,
}

/// Classification for events observed after closure (SPEC §5.11 late events).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LateEventClass {
    Duplicate,
    Correction,
    Amendment,
    Conflict,
    LateInformation,
    Unknown,
}

/// Staleness threshold per lifecycle type (V0 hardcoded; later configurable).
pub fn staleness_for(t: LifecycleType) -> u64 {
    match t {
        LifecycleType::Payment => 60 * 60 * 1000, // 1 hour
    }
}
