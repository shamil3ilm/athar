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
#[serde(rename_all = "UPPERCASE")]
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
#[serde(rename_all = "UPPERCASE")]
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

pub struct PolicyEngine {
    // Wrapped in RwLock so a background task can hot-swap the config without
    // requiring a daemon restart (INV-25 is aspirational — reload is safe here
    // because PolicyConfig is stateless; SignalEngine's rolling-window state is
    // NOT touched on reload, preserving accuracy).
    config: std::sync::RwLock<crate::config::PolicyConfig>,
}

impl PolicyEngine {
    pub fn new(config: crate::config::PolicyConfig) -> Self {
        Self { config: std::sync::RwLock::new(config) }
    }

    /// Replace the current policy config atomically. Callers who race with an
    /// in-flight `evaluate()` see either the old or the new config for that
    /// call — never a torn read, since the RwLock serialises access.
    pub fn reload(&self, new_config: crate::config::PolicyConfig) {
        let mut w = self.config.write().expect("policy engine config poisoned");
        *w = new_config;
    }

    /// Return a snapshot of the current config (for observability / tests).
    pub fn snapshot(&self) -> crate::config::PolicyConfig {
        self.config.read().expect("policy engine config poisoned").clone()
    }

    /// Evaluate all V0 policies against the given signals. First match wins.
    /// Each rule respects its own `enabled` flag and `mode` (OBSERVE / CHALLENGE
    /// / ENFORCE — set via config, no code change).
    /// Deterministic, side-effect free (SEC-23).
    pub fn evaluate(&self, signals: &[Signal]) -> PolicyDecision {
        let has_new_beneficiary = signals.iter().any(|s| s.kind == SignalKind::NewBeneficiary);
        let has_high_amount = signals.iter().any(|s| s.kind == SignalKind::HighAmount);
        let has_velocity = signals.iter().any(|s| s.kind == SignalKind::HighVelocity);
        let has_distinct = signals.iter().any(|s| s.kind == SignalKind::DistinctTargets);
        let has_credstuff = signals.iter().any(|s| s.kind == SignalKind::CredentialStuffingPattern);

        let config = self.config.read().expect("policy engine config poisoned");

        // Policy A: high-amount payment to an unknown beneficiary → CHALLENGE.
        let rule_a = &config.high_amount_new_beneficiary;
        if rule_a.enabled && has_new_beneficiary && has_high_amount {
            return PolicyDecision {
                policy_id: "pol_new_beneficiary_high_amount".into(),
                policy_version: 1,
                mode: rule_a.mode,
                matched: true,
                action: Action::Challenge,
                reason_codes: vec!["TARGET_NEW_BENEFICIARY_HIGH_AMOUNT".into()],
            };
        }

        // Policy B: sustained high event rate for a subject → CHALLENGE.
        let rule_b = &config.high_velocity;
        if rule_b.enabled && has_velocity {
            return PolicyDecision {
                policy_id: "pol_high_velocity".into(),
                policy_version: 1,
                mode: rule_b.mode,
                matched: true,
                action: Action::Challenge,
                reason_codes: vec!["VELOCITY_HIGH_RATE".into()],
            };
        }

        // Policy C: unusually many distinct targets from a single subject → CHALLENGE.
        let rule_c = &config.distinct_targets;
        if rule_c.enabled && has_distinct {
            return PolicyDecision {
                policy_id: "pol_distinct_targets".into(),
                policy_version: 1,
                mode: rule_c.mode,
                matched: true,
                action: Action::Challenge,
                reason_codes: vec!["TARGET_DISTINCT_FANOUT".into()],
            };
        }

        // Policy D: credential-stuffing pattern (rate of failed auth) → CHALLENGE.
        let rule_d = &config.credential_stuffing;
        if rule_d.enabled && has_credstuff {
            return PolicyDecision {
                policy_id: "pol_credential_stuffing".into(),
                policy_version: 1,
                mode: rule_d.mode,
                matched: true,
                action: Action::Challenge,
                reason_codes: vec!["AUTH_CREDENTIAL_STUFFING".into()],
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
    fn default() -> Self { Self::new(crate::config::PolicyConfig::default()) }
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
        let engine = PolicyEngine::default();
        let d = engine.evaluate(&[sig(SignalKind::NewBeneficiary), sig(SignalKind::HighAmount)]);
        assert!(d.matched);
        assert_eq!(d.action, Action::Challenge);
        assert_eq!(d.mode, PolicyMode::Observe);
        assert!(d.reason_codes.contains(&"TARGET_NEW_BENEFICIARY_HIGH_AMOUNT".to_string()));
    }

    #[test]
    fn no_match_with_just_one_signal() {
        let engine = PolicyEngine::default();
        let d = engine.evaluate(&[sig(SignalKind::HighAmount)]);
        assert!(!d.matched);
        assert_eq!(d.action, Action::Allow);
    }

    #[test]
    fn deterministic_result() {
        let engine = PolicyEngine::default();
        let sigs = vec![sig(SignalKind::NewBeneficiary), sig(SignalKind::HighAmount)];
        let d1 = engine.evaluate(&sigs);
        let d2 = engine.evaluate(&sigs);
        assert_eq!(d1, d2);
    }

    #[test]
    fn matches_on_high_velocity_alone() {
        let engine = PolicyEngine::default();
        let d = engine.evaluate(&[sig(SignalKind::HighVelocity)]);
        assert!(d.matched);
        assert_eq!(d.action, Action::Challenge);
        assert_eq!(d.policy_id, "pol_high_velocity");
        assert!(d.reason_codes.contains(&"VELOCITY_HIGH_RATE".to_string()));
    }

    #[test]
    fn matches_on_distinct_targets_alone() {
        let engine = PolicyEngine::default();
        let d = engine.evaluate(&[sig(SignalKind::DistinctTargets)]);
        assert!(d.matched);
        assert_eq!(d.action, Action::Challenge);
        assert_eq!(d.policy_id, "pol_distinct_targets");
        assert!(d.reason_codes.contains(&"TARGET_DISTINCT_FANOUT".to_string()));
    }

    #[test]
    fn disabled_policy_does_not_fire() {
        use crate::config::PolicyConfig;
        let mut cfg = PolicyConfig::default();
        cfg.high_velocity.enabled = false;
        let engine = PolicyEngine::new(cfg);
        // Only velocity is set — but that policy is disabled → default allow.
        let d = engine.evaluate(&[sig(SignalKind::HighVelocity)]);
        assert!(!d.matched);
        assert_eq!(d.action, Action::Allow);
        assert_eq!(d.policy_id, "pol_default");
    }

    #[test]
    fn mode_override_via_config_promotes_policy_to_enforce() {
        use crate::config::PolicyConfig;
        let mut cfg = PolicyConfig::default();
        cfg.high_velocity.mode = PolicyMode::Enforce;
        let engine = PolicyEngine::new(cfg);
        let d = engine.evaluate(&[sig(SignalKind::HighVelocity)]);
        assert!(d.matched);
        assert_eq!(d.mode, PolicyMode::Enforce);
        assert_eq!(d.action, Action::Challenge);
    }

    #[test]
    fn disabling_first_rule_lets_second_rule_win() {
        use crate::config::PolicyConfig;
        let mut cfg = PolicyConfig::default();
        cfg.high_amount_new_beneficiary.enabled = false;
        let engine = PolicyEngine::new(cfg);
        // Both new_beneficiary+high_amount AND high_velocity match — but rule A
        // is disabled, so rule B fires.
        let d = engine.evaluate(&[
            sig(SignalKind::NewBeneficiary),
            sig(SignalKind::HighAmount),
            sig(SignalKind::HighVelocity),
        ]);
        assert!(d.matched);
        assert_eq!(d.policy_id, "pol_high_velocity");
    }

    #[test]
    fn reload_swaps_config_without_new_engine() {
        use crate::config::PolicyConfig;
        // Start with high_velocity enabled → matches.
        let engine = PolicyEngine::new(PolicyConfig::default());
        let d = engine.evaluate(&[sig(SignalKind::HighVelocity)]);
        assert!(d.matched, "high_velocity should match before reload");

        // Reload with the same rule disabled → no match, same engine instance.
        let mut cfg = PolicyConfig::default();
        cfg.high_velocity.enabled = false;
        engine.reload(cfg);
        let d = engine.evaluate(&[sig(SignalKind::HighVelocity)]);
        assert!(!d.matched, "high_velocity should NOT match after reload");
        assert_eq!(d.policy_id, "pol_default");

        // Snapshot reflects the reload.
        let snap = engine.snapshot();
        assert!(!snap.high_velocity.enabled);
    }

    #[test]
    fn new_beneficiary_high_amount_takes_precedence_over_velocity() {
        // If both patterns match on the same event, the more-specific policy wins
        // (first-match semantics).
        let engine = PolicyEngine::default();
        let d = engine.evaluate(&[
            sig(SignalKind::NewBeneficiary),
            sig(SignalKind::HighAmount),
            sig(SignalKind::HighVelocity),
        ]);
        assert_eq!(d.policy_id, "pol_new_beneficiary_high_amount");
    }
}
