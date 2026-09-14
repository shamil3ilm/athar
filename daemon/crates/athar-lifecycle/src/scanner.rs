//! Staleness scanner (SPEC §5.11, MOD-25).

use crate::store::LifecycleStore;
use crate::types::*;

pub struct StalenessScanner;

#[derive(Debug, Clone)]
pub struct StalenessResult {
    pub lifecycle_id: String,
    pub at_ms: u64,
    pub was_state: State,
    pub new_state: State,
}

impl StalenessScanner {
    pub fn sweep(store: &dyn LifecycleStore, now_ms: u64) -> Vec<StalenessResult> {
        let mut results = Vec::new();
        for mut lc in store.all_open() {
            let age = now_ms.saturating_sub(lc.last_event_at_ms);
            if age >= lc.staleness_ms {
                let was_state = lc.state;
                let new_state = timeout_state_for(lc.state);
                lc.state = new_state;
                lc.closure = Closure::ClosedWithUncertainty;
                lc.closed_at_ms = Some(now_ms);
                store.upsert(&lc);
                results.push(StalenessResult {
                    lifecycle_id: lc.id,
                    at_ms: now_ms,
                    was_state,
                    new_state,
                });
            }
        }
        results
    }
}

fn timeout_state_for(current: State) -> State {
    match current {
        State::Started => State::Abandoned,
        State::Processing | State::Pending | State::Unknown => State::Timeout,
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::LifecycleEngine;
    use crate::store::InMemoryStore;
    use athar_event::*;

    fn ev(kind: &str, id: &str, resource: &str) -> Event {
        Event {
            schema_version: "1.0".into(),
            event_id: id.into(),
            event_type: kind.into(),
            tenant_id: "tnt_test".into(),
            timestamp: "2026-09-14T10:00:00.000000Z".into(),
            received_at: "2026-09-14T10:00:00.000000Z".into(),
            clock: Clock { source: ClockSource::Shim, skew_estimate_ms: None, monotonic_seq: 1 },
            actor: None, authenticated_principal: None, caller: None,
            service_identity: None, on_behalf_of: None, beneficiary: None, resource_owner: None,
            resource: Some(ResourceRef { id: Some(resource.into()), r#type: "payment".into(), namespace: None }),
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
    fn stale_lifecycle_is_closed_with_uncertainty() {
        let store = InMemoryStore::new();
        let engine = LifecycleEngine::new();
        engine.apply(&store, &ev("payment.create", "e1", "pay_stale"), 0);
        engine.apply(&store, &ev("payment.process", "e2", "pay_stale"), 500);
        assert!(StalenessScanner::sweep(&store, 500 + 60 * 60 * 1000 - 1).is_empty());
        let r = StalenessScanner::sweep(&store, 500 + 60 * 60 * 1000);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].new_state, State::Timeout);
        let id = store.find_by_resource("pay_stale").unwrap();
        let lc = store.get(&id).unwrap();
        assert_eq!(lc.state, State::Timeout);
        assert_eq!(lc.closure, Closure::ClosedWithUncertainty);
    }

    #[test]
    fn closed_lifecycles_not_re_swept() {
        let store = InMemoryStore::new();
        let engine = LifecycleEngine::new();
        engine.apply(&store, &ev("payment.create", "e1", "pay_c"), 0);
        engine.apply(&store, &ev("payment.settle", "e2", "pay_c"), 1000);
        assert!(StalenessScanner::sweep(&store, 10_000 + 60 * 60 * 1000).is_empty());
    }

    #[test]
    fn sqlite_backed_staleness_works_too() {
        let store = crate::sqlite_store::SqliteLifecycleStore::open_in_memory().unwrap();
        let engine = LifecycleEngine::new();
        engine.apply(&store, &ev("payment.create", "e1", "pay_sq_stale"), 0);
        let r = StalenessScanner::sweep(&store, 60 * 60 * 1000 + 1);
        assert_eq!(r.len(), 1);
        assert_eq!(store.count_open(), 0);
    }
}
