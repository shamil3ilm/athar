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
use std::sync::{Mutex, RwLock};

use athar_event::Event;

use crate::trackers::{TargetConfig, TargetTracker, VelocityConfig, VelocityTracker};

/// Rules for classifying an event as a "failed auth attempt", and the
/// rolling-window rate at which such events cross into a credential-stuffing
/// pattern for a subject.
///
/// The daemon uses the SAME subject grouping as `HighVelocity` (actor.id
/// fallback tenant_id), so real deployments should push a stable identifier
/// (user id, session id, IP hash) into `event.actor.id` for meaningful
/// per-source grouping.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CredentialStuffingConfig {
    /// Event types that immediately qualify. Matched by exact-string equality.
    pub event_types: Vec<String>,
    /// If set, and the event has this field in `data` with a value listed in
    /// `outcome_failed_values`, the event ALSO qualifies (in addition to any
    /// event_types match). Empty string disables the outcome-field check.
    pub outcome_field: String,
    /// Values of `data[outcome_field]` that count as failure.
    pub outcome_failed_values: Vec<String>,
    /// Rolling window in ms. Default 60_000 (1 minute).
    pub window_ms: u64,
    /// Fire the signal once count-in-window strictly exceeds this. Default 5.
    pub threshold: u32,
    /// Bound on tracked subjects (SEC-18). Default 10_000.
    pub max_subjects: usize,
}

impl Default for CredentialStuffingConfig {
    fn default() -> Self {
        Self {
            event_types: vec![
                "login.fail".into(),
                "auth.fail".into(),
                "signin.fail".into(),
            ],
            outcome_field: "outcome".into(),
            outcome_failed_values: vec![
                "failed".into(),
                "invalid_credentials".into(),
                "unauthorized".into(),
            ],
            window_ms: 60_000,
            threshold: 5,
            max_subjects: 10_000,
        }
    }
}

impl CredentialStuffingConfig {
    /// Does the event qualify as a failed-auth attempt under this config?
    pub fn qualifies(&self, event: &Event) -> bool {
        if self.event_types.iter().any(|t| t == &event.event_type) {
            return true;
        }
        if !self.outcome_field.is_empty() {
            if let Some(data) = &event.data {
                if let Some(v) = data.get(&self.outcome_field).and_then(|v| v.as_str()) {
                    if self.outcome_failed_values.iter().any(|f| f == v) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Corresponding VelocityConfig — mapped from this struct's fields.
    pub fn as_velocity(&self) -> VelocityConfig {
        VelocityConfig {
            window_ms: self.window_ms,
            threshold: self.threshold,
            max_subjects: self.max_subjects,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SignalKind {
    NewBeneficiary,
    HighAmount,
    HighVelocity,
    DistinctTargets,
    /// A burst of qualifying "failed auth" events from a single subject
    /// (default: `login.fail` / `auth.fail` / `outcome=failed`) crossing a
    /// per-subject rate threshold in a rolling window — classic credential
    /// stuffing footprint.
    CredentialStuffingPattern,
}

impl SignalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SignalKind::NewBeneficiary => "new_beneficiary",
            SignalKind::HighAmount => "high_amount",
            SignalKind::HighVelocity => "high_velocity",
            SignalKind::DistinctTargets => "distinct_targets",
            SignalKind::CredentialStuffingPattern => "credential_stuffing_pattern",
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SignalEngineConfig {
    /// Amount at or above this floor triggers `HighAmount`. Currency-agnostic in V0.
    pub high_amount_floor: f64,
    /// Field name in `event.data` that carries the numeric amount.
    pub amount_field: String,
    /// Velocity tracker configuration. Fires `HighVelocity` when count-in-window > threshold.
    pub velocity: VelocityConfig,
    /// Target tracker configuration. Fires `DistinctTargets` when distinct
    /// beneficiaries per subject > threshold in the rolling window.
    pub targets: TargetConfig,
    /// Credential-stuffing tracker configuration. Fires
    /// `CredentialStuffingPattern` when qualifying failed-auth events per
    /// subject exceed the threshold in the rolling window.
    pub credential_stuffing: CredentialStuffingConfig,
}

impl Default for SignalEngineConfig {
    fn default() -> Self {
        Self {
            high_amount_floor: 1_000.0,
            amount_field: "amount".to_string(),
            velocity: VelocityConfig::default(),
            targets: TargetConfig::default(),
            credential_stuffing: CredentialStuffingConfig::default(),
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
    velocity: Mutex<VelocityTracker>,
    targets: Mutex<TargetTracker>,
    /// Reused VelocityTracker scoped only to failed-auth events — same
    /// data-structure, different config. Separating the tracker means normal
    /// business events don't dilute the credential-stuffing signal.
    credstuff: Mutex<VelocityTracker>,
}

impl SignalEngine {
    pub fn new(config: SignalEngineConfig) -> Self {
        let velocity = Mutex::new(VelocityTracker::new(config.velocity.clone()));
        let targets = Mutex::new(TargetTracker::new(config.targets.clone()));
        let credstuff = Mutex::new(VelocityTracker::new(config.credential_stuffing.as_velocity()));
        Self { config, known: KnownResources::default(), velocity, targets, credstuff }
    }

    /// Evaluate an event against all V0 signal producers. Returns every fired signal.
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
        // HighVelocity — rate of events per subject over a rolling window.
        // Subject preference: actor.id > tenant_id. V0 fallback since shim often
        // ships `actor: None`; downstream identity resolution will refine this.
        let subject: String = event
            .actor
            .as_ref()
            .and_then(|a| a.id.clone())
            .unwrap_or_else(|| event.tenant_id.clone());
        let now_ms = wall_millis();
        if let Some(count) = self.velocity.lock().expect("velocity lock").observe(&subject, now_ms) {
            out.push(Signal {
                kind: SignalKind::HighVelocity,
                confidence: 0.90,
                value: format!("subject={subject} count={count} window_ms={}", self.config.velocity.window_ms),
                threshold: Some(format!(">{}", self.config.velocity.threshold)),
            });
        }

        // CredentialStuffingPattern — burst of qualifying failed-auth events
        // for a subject in a rolling window. Uses same subject grouping as
        // HighVelocity so operators can push a stable identifier once and
        // both signals benefit.
        if self.config.credential_stuffing.qualifies(event) {
            if let Some(count) = self
                .credstuff
                .lock()
                .expect("credstuff lock")
                .observe(&subject, now_ms)
            {
                out.push(Signal {
                    kind: SignalKind::CredentialStuffingPattern,
                    confidence: 0.85,
                    value: format!(
                        "subject={subject} failed_count={count} window_ms={} event_type={}",
                        self.config.credential_stuffing.window_ms,
                        event.event_type,
                    ),
                    threshold: Some(format!(">{}", self.config.credential_stuffing.threshold)),
                });
            }
        }

        // DistinctTargets — distinct beneficiaries per subject over a rolling
        // window. Catches fanout fraud. Fires only if the event has a
        // beneficiary.id — otherwise this signal is not applicable to the event.
        if let Some(target) = event.beneficiary.as_ref().and_then(|b| b.id.clone()) {
            if let Some(count) = self.targets.lock().expect("targets lock").observe(&subject, &target, now_ms) {
                out.push(Signal {
                    kind: SignalKind::DistinctTargets,
                    confidence: 0.90,
                    value: format!(
                        "subject={subject} distinct_targets={count} window_ms={}",
                        self.config.targets.window_ms
                    ),
                    threshold: Some(format!(">{}", self.config.targets.threshold)),
                });
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

fn wall_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
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
        let eng = SignalEngine::new(SignalEngineConfig {
            high_amount_floor: 1_000.0,
            amount_field: "amount".into(),
            velocity: crate::trackers::VelocityConfig::default(),
            targets: crate::trackers::TargetConfig::default(),
            credential_stuffing: CredentialStuffingConfig::default(),
        });
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
    fn high_velocity_fires_after_threshold() {
        // Threshold 3 with a wide window; 4+ observations for the same tenant should fire.
        let cfg = SignalEngineConfig {
            velocity: crate::trackers::VelocityConfig {
                window_ms: 60_000,
                threshold: 3,
                max_subjects: 100,
            },
            ..SignalEngineConfig::default()
        };
        let eng = SignalEngine::new(cfg);
        // Same tenant, no actor. Each event is a fresh unique resource so
        // NewBeneficiary always fires — but we're checking HighVelocity here.
        let mut fired_velocity_count = 0;
        for i in 0..5 {
            let sigs = eng.evaluate(&ev(&format!("e{i}"), Some(&format!("r{i}")), None));
            if sigs.iter().any(|s| s.kind == SignalKind::HighVelocity) {
                fired_velocity_count += 1;
            }
        }
        // 5 events, threshold 3: observations 4 and 5 should fire (count>3 and count>3 within window).
        assert_eq!(fired_velocity_count, 2, "expected HighVelocity on events 4 and 5");
    }

    fn ev_with_beneficiary(id: &str, resource: Option<&str>, beneficiary: &str) -> Event {
        let mut e = ev(id, resource, None);
        e.beneficiary = Some(ActorRef {
            id: Some(beneficiary.into()),
            r#type: Some(EntityType::User),
            namespace: None,
            resolution: ResolutionStatus::Probable,
            confidence: Some(Confidence(0.7)),
            calibration: None,
            conflicts: vec![],
        });
        // Also give it an actor so subject grouping works consistently.
        e.actor = Some(ActorRef {
            id: Some("actor_alpha".into()),
            r#type: Some(EntityType::User),
            namespace: None,
            resolution: ResolutionStatus::Verified,
            confidence: Some(Confidence(0.99)),
            calibration: None,
            conflicts: vec![],
        });
        e
    }

    #[test]
    fn distinct_targets_fires_after_threshold() {
        // Threshold 3: 4th distinct beneficiary from the same actor fires.
        let cfg = SignalEngineConfig {
            targets: crate::trackers::TargetConfig {
                window_ms: 60 * 60 * 1000,
                threshold: 3,
                max_subjects: 100,
                max_targets_per_subject: 100,
            },
            velocity: crate::trackers::VelocityConfig {
                window_ms: 60_000,
                threshold: 10_000, // effectively never
                max_subjects: 100,
            },
            ..SignalEngineConfig::default()
        };
        let eng = SignalEngine::new(cfg);
        let mut fired = 0;
        for i in 0..6 {
            let sigs = eng.evaluate(&ev_with_beneficiary(
                &format!("e{i}"),
                Some(&format!("pay_{i}")),
                &format!("ben_{i}"),
            ));
            if sigs.iter().any(|s| s.kind == SignalKind::DistinctTargets) {
                fired += 1;
            }
        }
        // Events 4, 5, 6 have 4, 5, 6 distinct beneficiaries respectively — all > 3.
        assert_eq!(fired, 3);
    }

    fn ev_of_type(event_type: &str, data: Option<serde_json::Value>) -> Event {
        let mut e = ev("e_stuff", None, None);
        e.event_type = event_type.into();
        e.data = data;
        // Same actor so credstuff subject grouping is deterministic.
        e.actor = Some(ActorRef {
            id: Some("actor_stuffer".into()),
            r#type: Some(EntityType::User),
            namespace: None,
            resolution: ResolutionStatus::Verified,
            confidence: Some(Confidence(0.9)),
            calibration: None,
            conflicts: vec![],
        });
        e
    }

    #[test]
    fn credential_stuffing_fires_on_event_type_match() {
        // Threshold 3: 4th failed login from same actor fires.
        let cfg = SignalEngineConfig {
            credential_stuffing: CredentialStuffingConfig {
                event_types: vec!["login.fail".into()],
                outcome_field: "".into(),
                outcome_failed_values: vec![],
                window_ms: 60_000,
                threshold: 3,
                max_subjects: 100,
            },
            velocity: crate::trackers::VelocityConfig { window_ms: 60_000, threshold: 10_000, max_subjects: 100 },
            ..SignalEngineConfig::default()
        };
        let eng = SignalEngine::new(cfg);
        let mut fired = 0;
        for _ in 0..5 {
            let sigs = eng.evaluate(&ev_of_type("login.fail", None));
            if sigs.iter().any(|s| s.kind == SignalKind::CredentialStuffingPattern) {
                fired += 1;
            }
        }
        assert_eq!(fired, 2, "events 4 and 5 both exceed threshold 3");
    }

    #[test]
    fn credential_stuffing_fires_on_outcome_field_match() {
        // The event type is neutral (login.attempt) but data.outcome='failed'.
        let cfg = SignalEngineConfig {
            credential_stuffing: CredentialStuffingConfig {
                event_types: vec![],
                outcome_field: "outcome".into(),
                outcome_failed_values: vec!["failed".into()],
                window_ms: 60_000,
                threshold: 2,
                max_subjects: 100,
            },
            velocity: crate::trackers::VelocityConfig { window_ms: 60_000, threshold: 10_000, max_subjects: 100 },
            ..SignalEngineConfig::default()
        };
        let eng = SignalEngine::new(cfg);
        for i in 0..3 {
            eng.evaluate(&ev_of_type("login.attempt", Some(serde_json::json!({ "outcome": "failed", "n": i }))));
        }
        // 3rd matching event fires.
        let sigs = eng.evaluate(&ev_of_type("login.attempt", Some(serde_json::json!({ "outcome": "failed", "n": 3 }))));
        assert!(sigs.iter().any(|s| s.kind == SignalKind::CredentialStuffingPattern));
    }

    #[test]
    fn credential_stuffing_does_not_fire_on_successful_logins() {
        let cfg = SignalEngineConfig {
            credential_stuffing: CredentialStuffingConfig {
                event_types: vec!["login.fail".into()],
                outcome_field: "outcome".into(),
                outcome_failed_values: vec!["failed".into()],
                window_ms: 60_000,
                threshold: 2,
                max_subjects: 100,
            },
            velocity: crate::trackers::VelocityConfig { window_ms: 60_000, threshold: 10_000, max_subjects: 100 },
            ..SignalEngineConfig::default()
        };
        let eng = SignalEngine::new(cfg);
        // Successful logins should never contribute to the tracker.
        for _ in 0..10 {
            let sigs = eng.evaluate(&ev_of_type("login.success", Some(serde_json::json!({ "outcome": "ok" }))));
            assert!(!sigs.iter().any(|s| s.kind == SignalKind::CredentialStuffingPattern));
        }
    }

    #[test]
    fn distinct_targets_does_not_fire_without_beneficiary() {
        let cfg = SignalEngineConfig {
            targets: crate::trackers::TargetConfig {
                window_ms: 60 * 60 * 1000, threshold: 1, max_subjects: 100, max_targets_per_subject: 100,
            },
            ..SignalEngineConfig::default()
        };
        let eng = SignalEngine::new(cfg);
        // Event has no beneficiary — DistinctTargets should NOT fire regardless of threshold.
        let sigs = eng.evaluate(&ev("e1", Some("pay_1"), None));
        assert!(!sigs.iter().any(|s| s.kind == SignalKind::DistinctTargets));
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
