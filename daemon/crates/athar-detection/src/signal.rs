//! Signal engine (SPEC §9.1, SEC-16..SEC-18).
//!
//! Signals are EVIDENCE for policy decisions, never decisions themselves.
//! Every signal record carries the evidence that produced it, so a decision
//! built on it is reconstructable months later (INV-10).
//!
//! V0 signal types:
//!   - `NewBeneficiary` — resource.id has never been observed for this tenant.
//!     Requires a "known resources" tracker (in-memory for V0).
//!   - `HighAmount` — `data.amount` is above a configured floor.
//!     Trivial to compute per-event; no history required.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::RwLock;

use athar_event::Event;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SignalKind {
    NewBeneficiary,
    HighAmount,
    HighVelocity,     // reserved for future — needs windowed store
    DistinctTargets,  // reserved for future
}

impl SignalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SignalKind::NewBeneficiary => "new_beneficiary",
            SignalKind::HighAmount => "high_amount",
            SignalKind::HighVelocity => "high_velocity",
            SignalKind::DistinctTargets => "distinct_targets",
        }
    }
}

/// A produced signal (in-memory shape).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Signal {
    pub kind: SignalKind,
    /// Confidence in the signal's applicability, [0.0, 1.0].
    pub confidence: f32,
    /// Human-readable value the signal represents (e.g. "amount=5000").
    pub value: String,
    /// The threshold or benchmark that was compared against, if any.
    pub threshold: Option<String>,
}

/// A signal record as persisted (INV-16: references the evidence, so an investigator
/// can reconstruct why the signal fired).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignalRecord {
    pub signal_id: String,
    pub tenant_id: String,
    pub kind: SignalKind,
    pub confidence: f32,
    pub value: String,
    pub threshold: Option<String>,
    pub timestamp_ms: u64,
    pub event_id: String,        // The event that triggered this signal.
    pub lifecycle_id: Option<String>,
    pub tracker_version: String, // What version of the tracker computed this.
}

#[derive(Debug, Clone)]
pub struct SignalEngineConfig {
    /// Amount at or above this floor triggers `HighAmount`. Currency-agnostic in V0.
    pub high_amount_floor: f64,
    /// Field name in `event.data` that carries the numeric amount.
    pub amount_field: String,
}

impl Default for SignalEngineConfig {
    fn default() -> Self {
        Self {
            high_amount_floor: 1_000.0,
            amount_field: "amount".to_string(),
        }
    }
}

/// V0 in-memory tracker for "known resources". Real implementation persists to
/// SQLite; that's a straightforward migration when we need it.
#[derive(Default)]
struct KnownResources {
    seen: RwLock<HashSet<String>>,
}

pub struct SignalEngine {
    config: SignalEngineConfig,
    known: KnownResources,
}

impl SignalEngine {
    pub fn new(config: SignalEngineConfig) -> Self {
        Self { config, known: KnownResources::default() }
    }

    /// Evaluate an event against all V0 signal producers. Returns every fired signal.
    /// Idempotent per (kind, event_id, resource_id).
    pub fn evaluate(&self, event: &Event) -> Vec<Signal> {
        let mut out = Vec::new();
        // NewBeneficiary
        if let Some(resource) = &event.resource {
            if let Some(rid) = &resource.id {
                let key = format!("{}:{}", event.tenant_id, rid);
                let is_new = !self.known.seen.read().expect("known lock").contains(&key);
                if is_new {
                    self.known.seen.write().expect("known lock").insert(key.clone());
                    out.push(Signal {
                        kind: SignalKind::NewBeneficiary,
                        confidence: 0.85,
                        value: rid.clone(),
                        threshold: None,
                    });
                }
            }
        }
        // HighAmount
        if let Some(data) = &event.data {
            if let Some(amount) = extract_amount(data, &self.config.amount_field) {
                if amount >= self.config.high_amount_floor {
                    out.push(Signal {
                        kind: SignalKind::HighAmount,
                        confidence: 0.95,
                        value: format!("amount={amount}"),
                        threshold: Some(format!(">={:.2}", self.config.high_amount_floor)),
                    });
                }
            }
        }
        out
    }

    pub fn tracker_version(&self) -> &'static str { "signal-engine-0.1.0" }
}

fn extract_amount(data: &serde_json::Value, field: &str) -> Option<f64> {
    match data.get(field) {
        Some(serde_json::Value::Number(n)) => n.as_f64(),
        Some(serde_json::Value::String(s)) => s.parse().ok(),
        _ => None,
    }
}

/// Build a `SignalRecord` for persistence. `signal_id` is a fresh ULID.
pub fn record_from_signal(
    signal: &Signal,
    tenant_id: &str,
    event_id: &str,
    lifecycle_id: Option<&str>,
    now_ms: u64,
    tracker_version: &str,
) -> SignalRecord {
    SignalRecord {
        signal_id: format!("sig_{}", ulid::Ulid::new()),
        tenant_id: tenant_id.to_string(),
        kind: signal.kind,
        confidence: signal.confidence,
        value: signal.value.clone(),
        threshold: signal.threshold.clone(),
        timestamp_ms: now_ms,
        event_id: event_id.to_string(),
        lifecycle_id: lifecycle_id.map(String::from),
        tracker_version: tracker_version.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use athar_event::*;

    fn ev(id: &str, resource: Option<&str>, amount: Option<f64>) -> Event {
        Event {
            schema_version: "1.0".into(),
            event_id: id.into(),
            event_type: "payment.create".into(),
            tenant_id: "tnt_t".into(),
            timestamp: "2026-09-14T10:00:00.000000Z".into(),
            received_at: "2026-09-14T10:00:00.000000Z".into(),
            clock: Clock { source: ClockSource::Shim, skew_estimate_ms: None, monotonic_seq: 1 },
            actor: None, authenticated_principal: None, caller: None,
            service_identity: None, on_behalf_of: None, beneficiary: None, resource_owner: None,
            resource: resource.map(|r| ResourceRef {
                id: Some(r.into()), r#type: "payment".into(), namespace: None,
            }),
            operation: None, lifecycle: None, entry_point: None, technical_context: None,
            provenance: Provenance {
                origin: Origin::Human, trigger: Trigger::ApiRequest,
                source: "test".into(), producer: None, authority: None,
            },
            truth: Truth { stage: TruthStage::Observed, asserted_by: "t".into(), adapter: None },
            trust: Trust {
                level: TrustLevel::Unknown, confidence: Confidence(0.0),
                calibration: None, factors: vec![],
            },
            causality: Causality::default(),
            coverage: Coverage { complete: true, shed: vec![], redacted_fields: vec![], degradation_level: None },
            data: amount.map(|a| serde_json::json!({ "amount": a })),
            attributes: None,
        }
    }

    #[test]
    fn new_beneficiary_fires_once_per_resource() {
        let eng = SignalEngine::new(SignalEngineConfig::default());
        let s1 = eng.evaluate(&ev("e1", Some("pay_1"), None));
        assert_eq!(s1.len(), 1);
        assert_eq!(s1[0].kind, SignalKind::NewBeneficiary);
        let s2 = eng.evaluate(&ev("e2", Some("pay_1"), None));
        assert!(s2.is_empty(), "second event on same resource should NOT re-fire new_beneficiary");
    }

    #[test]
    fn high_amount_fires_above_floor() {
        let eng = SignalEngine::new(SignalEngineConfig { high_amount_floor: 1_000.0, amount_field: "amount".into() });
        let s = eng.evaluate(&ev("e1", None, Some(5_000.0)));
        assert!(s.iter().any(|x| x.kind == SignalKind::HighAmount));
        let s = eng.evaluate(&ev("e2", None, Some(500.0)));
        assert!(!s.iter().any(|x| x.kind == SignalKind::HighAmount));
    }

    #[test]
    fn both_signals_can_fire_together() {
        let eng = SignalEngine::new(SignalEngineConfig::default());
        let s = eng.evaluate(&ev("e1", Some("pay_x"), Some(10_000.0)));
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn record_from_signal_carries_evidence() {
        let sig = Signal {
            kind: SignalKind::HighAmount,
            confidence: 0.95,
            value: "amount=5000".into(),
            threshold: Some(">=1000.00".into()),
        };
        let r = record_from_signal(&sig, "tnt_t", "e1", Some("lc_1"), 1000, "v1");
        assert!(r.signal_id.starts_with("sig_"));
        assert_eq!(r.event_id, "e1");
        assert_eq!(r.lifecycle_id.as_deref(), Some("lc_1"));
        assert_eq!(r.kind, SignalKind::HighAmount);
    }
}
