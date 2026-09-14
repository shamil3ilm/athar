//! Explainable decision record (SPEC §9.4, INV-16, INV-17).
//!
//! Every automated decision the runtime makes MUST produce a record sufficient
//! to reconstruct it without access to the live application (INV-10).
//!
//! Mandatory fields per INV-17:
//!   - inputs_missing         — what was UNKNOWN at decision time
//!   - coverage_gaps_overlapping — was the runtime blind for any of the window
//!   - degradation_level      — pressure level at decision time
//! plus mode, fail_mode, engine versions, reason codes.

use serde::{Deserialize, Serialize};

use crate::policy::{Action, PolicyDecision, PolicyMode};
use crate::signal::SignalRecord;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum FailMode {
    Open,
    Closed,
}

impl FailMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            FailMode::Open => "OPEN",
            FailMode::Closed => "CLOSED",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionSubject {
    pub lifecycle_id: Option<String>,
    pub event_id: String,
    pub operation_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngineVersions {
    pub detector: String,
    pub policy: String,
    pub resolver: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionRecord {
    pub decision_id: String,
    pub timestamp_ms: u64,
    pub tenant_id: String,
    pub subject: DecisionSubject,

    pub policies_evaluated: Vec<PolicyDecision>,
    pub signals_used: Vec<SignalRecord>,

    pub action: Action,
    pub reason_codes: Vec<String>,
    pub mode: PolicyMode,
    pub fail_mode: FailMode,

    /// INV-17: inputs the daemon knows were UNKNOWN at decision time.
    pub inputs_missing: Vec<String>,
    /// INV-17: coverage_gap records overlapping the window feeding this decision.
    pub coverage_gaps_overlapping: Vec<String>,
    /// INV-17: pressure level at decision time.
    pub degradation_level: String,

    pub latency_us: u64,
    pub engine_versions: EngineVersions,

    /// Human-readable explanation generated from the above; NOT free-form.
    pub explanation: String,
}

impl DecisionRecord {
    /// Build a fresh decision record. `now_ms` is the wall clock at emission.
    pub fn build(
        tenant_id: &str,
        subject: DecisionSubject,
        policy: PolicyDecision,
        signals: Vec<SignalRecord>,
        now_ms: u64,
        degradation_level: &str,
        latency_us: u64,
    ) -> Self {
        let action = policy.action;
        let mode = policy.mode;
        let reason_codes = policy.reason_codes.clone();
        let explanation = generate_explanation(&policy, &signals);
        Self {
            decision_id: format!("dec_{}", ulid::Ulid::new()),
            timestamp_ms: now_ms,
            tenant_id: tenant_id.to_string(),
            subject,
            policies_evaluated: vec![policy],
            signals_used: signals,
            action,
            reason_codes,
            mode,
            fail_mode: FailMode::Open, // V0 default per SEC-10
            inputs_missing: vec![],
            coverage_gaps_overlapping: vec![],
            degradation_level: degradation_level.to_string(),
            latency_us,
            engine_versions: EngineVersions {
                detector: "signal-engine-0.1.0".into(),
                policy: "policy-engine-0.1.0".into(),
                resolver: "lifecycle-engine-0.1.0".into(),
            },
            explanation,
        }
    }
}

fn generate_explanation(policy: &PolicyDecision, signals: &[SignalRecord]) -> String {
    if !policy.matched {
        return format!(
            "policy {}:v{} did not match; action ALLOW ({} signal(s) evaluated)",
            policy.policy_id, policy.policy_version, signals.len(),
        );
    }
    let signal_summary: Vec<String> = signals.iter()
        .map(|s| format!("{}={} (conf {:.2})", s.kind.as_str(), s.value, s.confidence))
        .collect();
    format!(
        "policy {}:v{} matched in {} mode → {}. Signals: [{}]. Reason codes: [{}].",
        policy.policy_id,
        policy.policy_version,
        policy.mode.as_str(),
        policy.action.as_str(),
        signal_summary.join(", "),
        policy.reason_codes.join(", "),
    )
}
