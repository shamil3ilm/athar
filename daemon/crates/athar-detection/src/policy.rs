//! Policy engine (SPEC §9.3, SEC-21..SEC-24).
//!
//! V0: one hardcoded policy in OBSERVE mode. CEL (D11) is deferred; the point of
//! this pass is to prove the evaluation → decision → audit path end-to-end.
//!
//! Policy under evaluation (v1):
//!   `new_beneficiary` AND `high_amount` → CHALLENGE (recorded as OBSERVE decision).

use serde::{Deserialize, Serialize};

use crate::signal::{Signal, SignalKind};

/// Policy modes (SPEC §9.3, SEC-22). Only OBSERVE is exercised in V0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolicyMode {
    Observe,
    Challenge,
    Enforce,
}

impl PolicyMode {
    pub fn as_str(self) -> &'static str {
        match self {
            PolicyMode::Observe => "OBSERVE",
            PolicyMode::Challenge => "CHALLENGE",
            PolicyMode::Enforce => "ENFORCE",
        }
    }
}

/// Actions a decision can propose. In OBSERVE mode, the daemon records but does not enforce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    Allow,
    Challenge,
    Restrict,
    Block,
    Alert,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Allow => "ALLOW",
            Action::Challenge => "CHALLENGE",
            Action::Restrict => "RESTRICT",
            Action::Block => "BLOCK",
            Action::Alert => "ALERT",
        }
    }
}

/// Result of evaluating a policy against a set of signals.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolicyDecision {
    pub policy_id: String,
    pub policy_version: u32,
    pub mode: PolicyMode,
    pub matched: bool,
    pub action: Action,
    pub reason_codes: Vec<String>,
}

pub struct PolicyEngine;

impl PolicyEngine {
    pub fn new() -> Self { Self }

    /// Evaluate all V0 policies against the given signals. First match wins.
    /// Deterministic, side-effect free (SEC-23).
    pub fn evaluate(&self, signals: &[Signal]) -> PolicyDecision {
        let has_new_beneficiary = signals.iter().any(|s| s.kind == SignalKind::NewBeneficiary);
        let has_high_amount = signals.iter().any(|s| s.kind == SignalKind::HighAmount);
        let has_velocity = signals.iter().any(|s| s.kind == SignalKind::HighVelocity);

        // Policy A: high-amount payment to an unknown beneficiary → CHALLENGE.
        if has_new_beneficiary && has_high_amount {
            return PolicyDecision {
                policy_id: "pol_new_beneficiary_high_amount".into(),
                policy_version: 1,
                mode: PolicyMode::Observe,
                matched: true,
                action: Action::Challenge,
                reason_codes: vec!["TARGET_NEW_BENEFICIARY_HIGH_AMOUNT".into()],
            };
        }

        // Policy B: sustained high event rate for a subject → CHALLENGE.
        // Catches velocity fraud (many payments from same actor in a short window),
        // credential-stuffing shapes, automated abuse.
        if has_velocity {
            return PolicyDecision {
                policy_id: "pol_high_velocity".into(),
                policy_version: 1,
                mode: PolicyMode::Observe,
                matched: true,
                action: Action::Challenge,
                reason_codes: vec!["VELOCITY_HIGH_RATE".into()],
            };
        }

        // Default: no policy matched → allow.
        PolicyDecision {
            policy_id: "pol_default".into(),
            policy_version: 1,
            mode: PolicyMode::Observe,
            matched: false,
            action: Action::Allow,
            reason_codes: vec![],
        }
    }
}

impl Default for PolicyEngine {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::{Signal, SignalKind};

    fn sig(kind: SignalKind) -> Signal {
        Signal { kind, confidence: 0.9, value: "x".into(), threshold: None }
    }

    #[test]
    fn matches_when_both_signals_present() {
        let engine = PolicyEngine::new();
        let d = engine.evaluate(&[sig(SignalKind::NewBeneficiary), sig(SignalKind::HighAmount)]);
        assert!(d.matched);
        assert_eq!(d.action, Action::Challenge);
        assert_eq!(d.mode, PolicyMode::Observe);
        assert!(d.reason_codes.contains(&"TARGET_NEW_BENEFICIARY_HIGH_AMOUNT".to_string()));
    }

    #[test]
    fn no_match_with_just_one_signal() {
        let engine = PolicyEngine::new();
        let d = engine.evaluate(&[sig(SignalKind::HighAmount)]);
        assert!(!d.matched);
        assert_eq!(d.action, Action::Allow);
    }

    #[test]
    fn deterministic_result() {
        let engine = PolicyEngine::new();
        let sigs = vec![sig(SignalKind::NewBeneficiary), sig(SignalKind::HighAmount)];
        let d1 = engine.evaluate(&sigs);
        let d2 = engine.evaluate(&sigs);
        assert_eq!(d1, d2);
    }

    #[test]
    fn matches_on_high_velocity_alone() {
        let engine = PolicyEngine::new();
        let d = engine.evaluate(&[sig(SignalKind::HighVelocity)]);
        assert!(d.matched);
        assert_eq!(d.action, Action::Challenge);
        assert_eq!(d.policy_id, "pol_high_velocity");
        assert!(d.reason_codes.contains(&"VELOCITY_HIGH_RATE".to_string()));
    }

    #[test]
    fn new_beneficiary_high_amount_takes_precedence_over_velocity() {
        // If both patterns match on the same event, the more-specific policy wins
        // (first-match semantics).
        let engine = PolicyEngine::new();
        let d = engine.evaluate(&[
            sig(SignalKind::NewBeneficiary),
            sig(SignalKind::HighAmount),
            sig(SignalKind::HighVelocity),
        ]);
        assert_eq!(d.policy_id, "pol_new_beneficiary_high_amount");
    }
}
