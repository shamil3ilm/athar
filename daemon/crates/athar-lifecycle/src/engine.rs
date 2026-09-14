//! Lifecycle engine (SPEC §5.9, §5.11).
//!
//! Applies each ingested event to the lifecycle graph:
//! 1. Correlate to an existing lifecycle (tiers 1-4) OR create a new one.
//! 2. If CLOSED, classify the event as a late event; DO NOT reopen (MOD-27).
//! 3. Otherwise, run the state machine for the lifecycle type.
//!
//! The engine works against any `LifecycleStore` implementation, so it doesn't
//! know whether it's talking to in-memory storage or SQLite.

use athar_event::Event;

use crate::store::LifecycleStore;
use crate::types::*;

pub struct LifecycleEngine;

impl LifecycleEngine {
    pub fn new() -> Self { Self }

    pub fn apply(&self, store: &dyn LifecycleStore, event: &Event, now_ms: u64) -> ApplyOutcome {
        let corr = correlate(store, event);
        match corr {
            Corr::Match { lifecycle_id, tier } => {
                let Some(mut lc) = store.get(&lifecycle_id) else {
                    return ApplyOutcome::Unbound;
                };
                if lc.closure.is_closed() {
                    let class = classify_late_event(&lc, event);
                    lc.late_events.push(LateEvent {
                        event_id: event.event_id.clone(),
                        event_type: event.event_type.clone(),
                        arrived_at_ms: now_ms,
                        class,
                    });
                    store.upsert(&lc);
                    return ApplyOutcome::LateEvent { lifecycle_id, tier, class };
                }
                let (state, closure) = transition_for(lc.lifecycle_type, event)
                    .unwrap_or((lc.state, lc.closure));
                lc.event_ids.push(event.event_id.clone());
                lc.tiers.push(tier);
                lc.last_event_at_ms = now_ms;
                lc.state = state;
                lc.closure = closure;
                if closure.is_closed() {
                    lc.closed_at_ms = Some(now_ms);
                }
                store.upsert(&lc);
                store.record_event(&event.event_id, &lifecycle_id);
                ApplyOutcome::Updated { lifecycle_id, tier, state, closure }
            }
            Corr::New { lifecycle_type, resource_id, business_key } => {
                let lifecycle_id = format!("lc_{}", event.event_id);
                let (state, closure) = transition_for(lifecycle_type, event)
                    .unwrap_or((State::Started, Closure::Open));
                let closed_at = if closure.is_closed() { Some(now_ms) } else { None };
                let lc = Lifecycle {
                    id: lifecycle_id.clone(),
                    tenant_id: event.tenant_id.clone(),
                    lifecycle_type,
                    business_key,
                    resource_id,
                    state,
                    closure,
                    started_at_ms: now_ms,
                    last_event_at_ms: now_ms,
                    closed_at_ms: closed_at,
                    event_ids: vec![event.event_id.clone()],
                    late_events: vec![],
                    staleness_ms: staleness_for(lifecycle_type),
                    tiers: vec![InferenceTier::ResourceId],
                };
                store.upsert(&lc);
                store.record_event(&event.event_id, &lifecycle_id);
                ApplyOutcome::Created { lifecycle_id, lifecycle_type, state, closure }
            }
            Corr::None => ApplyOutcome::Unbound,
        }
    }
}

impl Default for LifecycleEngine {
    fn default() -> Self { Self::new() }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ApplyOutcome {
    Created { lifecycle_id: String, lifecycle_type: LifecycleType, state: State, closure: Closure },
    Updated { lifecycle_id: String, tier: InferenceTier, state: State, closure: Closure },
    LateEvent { lifecycle_id: String, tier: InferenceTier, class: LateEventClass },
    Unbound,
}

enum Corr {
    Match { lifecycle_id: String, tier: InferenceTier },
    New { lifecycle_type: LifecycleType, resource_id: Option<String>, business_key: Option<String> },
    None,
}

fn correlate(store: &dyn LifecycleStore, event: &Event) -> Corr {
    // Tier 1: explicit operation_id
    if let Some(op) = &event.operation {
        if let Some(op_id) = &op.operation_id {
            if let Some(lc_id) = store.find_by_business_key(op_id) {
                return Corr::Match { lifecycle_id: lc_id, tier: InferenceTier::Explicit };
            }
        }
    }
    // Tier 2: business_id customer_correlation_id
    if let Some(bid) = event.causality.customer_correlation_ids.get("business_id") {
        if let Some(lc_id) = store.find_by_business_key(bid) {
            return Corr::Match { lifecycle_id: lc_id, tier: InferenceTier::BusinessId };
        }
    }
    // Tier 3: parent / causation
    let parent = event.causality.parent_event_id.as_ref()
        .or(event.causality.causation_id.as_ref());
    if let Some(p) = parent {
        if let Some(lc_id) = store.find_by_event(p) {
            return Corr::Match { lifecycle_id: lc_id, tier: InferenceTier::Causation };
        }
    }
    // Tier 4: resource id
    if let Some(r) = &event.resource {
        if let Some(rid) = &r.id {
            if let Some(lc_id) = store.find_by_resource(rid) {
                return Corr::Match { lifecycle_id: lc_id, tier: InferenceTier::ResourceId };
            }
            if let Some(lct) = LifecycleType::from_event_type(&event.event_type) {
                let business_key = event.causality.customer_correlation_ids.get("business_id").cloned()
                    .or_else(|| event.operation.as_ref().and_then(|o| o.operation_id.clone()));
                return Corr::New {
                    lifecycle_type: lct,
                    resource_id: Some(rid.clone()),
                    business_key,
                };
            }
        }
    }
    // Business-id only:
    if let Some(bid) = event.causality.customer_correlation_ids.get("business_id") {
        if let Some(lct) = LifecycleType::from_event_type(&event.event_type) {
            return Corr::New {
                lifecycle_type: lct,
                resource_id: None,
                business_key: Some(bid.clone()),
            };
        }
    }
    Corr::None
}

fn transition_for(t: LifecycleType, event: &Event) -> Option<(State, Closure)> {
    match t {
        LifecycleType::Payment => match event.event_type.as_str() {
            "payment.create" => Some((State::Started, Closure::Open)),
            "payment.process" | "payment.processing" => Some((State::Processing, Closure::Open)),
            "payment.pending" => Some((State::Pending, Closure::Open)),
            "payment.retry" => Some((State::Processing, Closure::Open)),
            "payment.settle" | "payment.success" | "payment.completed"
                => Some((State::Success, Closure::Closed)),
            "payment.fail" | "payment.failed" | "payment.error"
                => Some((State::Failed, Closure::ClosedWithException)),
            "payment.cancel" | "payment.cancelled"
                => Some((State::Cancelled, Closure::Closed)),
            "payment.reverse" | "payment.reversed"
                => Some((State::Reversed, Closure::ClosedWithException)),
            _ => None,
        },
    }
}

fn classify_late_event(lc: &Lifecycle, event: &Event) -> LateEventClass {
    if lc.event_ids.contains(&event.event_id) {
        return LateEventClass::Duplicate;
    }
    if let Some((new_state, _)) = transition_for(lc.lifecycle_type, event) {
        if new_state.is_terminal() && new_state != lc.state {
            return LateEventClass::Conflict;
        }
        return LateEventClass::Amendment;
    }
    LateEventClass::LateInformation
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::InMemoryStore;
    use athar_event::*;

    fn ev(kind: &str, id: &str, resource: Option<&str>) -> Event {
        Event {
            schema_version: "1.0".into(),
            event_id: id.into(),
            event_type: kind.into(),
            tenant_id: "tnt_test".into(),
            timestamp: "2026-09-14T10:00:00.000000Z".into(),
            received_at: "2026-09-14T10:00:00.000000Z".into(),
            clock: Clock { source: ClockSource::Shim, skew_estimate_ms: None, monotonic_seq: 1 },
            actor: None, authenticated_principal: None, caller: None,
            service_identity: None, on_behalf_of: None, beneficiary: None,
            resource_owner: None,
            resource: resource.map(|r| ResourceRef {
                id: Some(r.into()),
                r#type: "payment".into(),
                namespace: None,
            }),
            operation: None, lifecycle: None, entry_point: None, technical_context: None,
            provenance: Provenance {
                origin: Origin::Unknown, trigger: Trigger::ApiRequest,
                source: "test".into(), producer: None, authority: None,
            },
            truth: Truth { stage: TruthStage::Observed, asserted_by: "test".into(), adapter: None },
            trust: Trust {
                level: TrustLevel::Unknown, confidence: Confidence(0.0),
                calibration: None, factors: vec![],
            },
            causality: Causality::default(),
            coverage: Coverage { complete: true, shed: vec![], redacted_fields: vec![], degradation_level: None },
            data: None, attributes: None,
        }
    }

    #[test]
    fn happy_path_open_progress_close() {
        let store = InMemoryStore::new();
        let engine = LifecycleEngine::new();
        let created = engine.apply(&store, &ev("payment.create", "e1", Some("pay_1")), 1000);
        let lifecycle_id = match created {
            ApplyOutcome::Created { lifecycle_id, state: State::Started, closure: Closure::Open, .. } => lifecycle_id,
            other => panic!("expected Created, got {other:?}"),
        };
        engine.apply(&store, &ev("payment.process", "e2", Some("pay_1")), 2000);
        engine.apply(&store, &ev("payment.settle", "e3", Some("pay_1")), 3000);
        let lc = store.get(&lifecycle_id).unwrap();
        assert_eq!(lc.state, State::Success);
        assert_eq!(lc.closure, Closure::Closed);
        assert_eq!(lc.event_ids.len(), 3);
    }

    #[test]
    fn late_event_after_closure_is_classified_not_applied() {
        let store = InMemoryStore::new();
        let engine = LifecycleEngine::new();
        engine.apply(&store, &ev("payment.create", "e1", Some("pay_2")), 1000);
        engine.apply(&store, &ev("payment.settle", "e2", Some("pay_2")), 2000);
        let outcome = engine.apply(&store, &ev("payment.fail", "e3", Some("pay_2")), 3000);
        assert!(matches!(outcome, ApplyOutcome::LateEvent { class: LateEventClass::Conflict, .. }));
        let id = store.find_by_resource("pay_2").unwrap();
        let lc = store.get(&id).unwrap();
        assert_eq!(lc.state, State::Success);
        assert_eq!(lc.closure, Closure::Closed);
        assert_eq!(lc.late_events.len(), 1);
    }

    #[test]
    fn duplicate_event_id_classified_duplicate() {
        let store = InMemoryStore::new();
        let engine = LifecycleEngine::new();
        engine.apply(&store, &ev("payment.create", "e1", Some("pay_3")), 1000);
        engine.apply(&store, &ev("payment.settle", "e2", Some("pay_3")), 2000);
        let out = engine.apply(&store, &ev("payment.settle", "e2", Some("pay_3")), 3000);
        assert!(matches!(out, ApplyOutcome::LateEvent { class: LateEventClass::Duplicate, .. }));
    }

    #[test]
    fn engine_works_with_sqlite_store() {
        // Same test as the in-memory happy path, but running against SQLite.
        let store = crate::sqlite_store::SqliteLifecycleStore::open_in_memory().unwrap();
        let engine = LifecycleEngine::new();
        engine.apply(&store, &ev("payment.create", "e1", Some("pay_sqlite")), 1000);
        engine.apply(&store, &ev("payment.process", "e2", Some("pay_sqlite")), 2000);
        engine.apply(&store, &ev("payment.settle", "e3", Some("pay_sqlite")), 3000);
        let id = store.find_by_resource("pay_sqlite").unwrap();
        let lc = store.get(&id).unwrap();
        assert_eq!(lc.state, State::Success);
        assert_eq!(lc.closure, Closure::Closed);
        assert_eq!(lc.event_ids.len(), 3);
        assert_eq!(store.count_open(), 0);
    }

    #[test]
    fn unrelated_event_is_unbound() {
        let store = InMemoryStore::new();
        let engine = LifecycleEngine::new();
        assert!(matches!(engine.apply(&store, &ev("http.request", "eX", None), 100), ApplyOutcome::Unbound));
    }
}
