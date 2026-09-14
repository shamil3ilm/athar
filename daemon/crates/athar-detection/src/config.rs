//! Detection configuration — loaded from a JSON file at daemon startup.
//!
//! This is the shape a customer edits to:
//!   - Enable / disable individual policies
//!   - Promote a policy from OBSERVE to CHALLENGE or ENFORCE mode
//!   - Set per-policy fail-open / fail-closed behaviour
//!   - Tune signal thresholds (velocity window/threshold, high-amount floor,
//!     distinct-targets threshold)
//!
//! Absent or malformed config → sensible defaults (all three policies enabled
//! in OBSERVE mode, thresholds matching the SPEC's suggested seeds). Never
//! panics on bad config; logs a warning and continues with defaults.

use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::decision::FailMode;
use crate::policy::PolicyMode;
use crate::signal::SignalEngineConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DetectionConfig {
    pub policies: PolicyConfig,
    pub signals: SignalEngineConfig,
}

impl Default for DetectionConfig {
    fn default() -> Self {
        Self {
            policies: PolicyConfig::default(),
            signals: SignalEngineConfig::default(),
        }
    }
}

impl DetectionConfig {
    /// Load JSON from `path`. Returns defaults on missing file, unreadable
    /// file, or malformed JSON (with a warning in each case). Never panics.
    pub fn load_or_default<P: AsRef<Path>>(path: P) -> Self {
        let path = path.as_ref();
        if !path.exists() {
            info!(
                path = %path.display(),
                "detection config not found; using built-in defaults"
            );
            return Self::default();
        }
        match std::fs::read_to_string(path) {
            Ok(contents) => match serde_json::from_str::<Self>(&contents) {
                Ok(cfg) => {
                    info!(path = %path.display(), "detection config loaded");
                    cfg
                }
                Err(e) => {
                    warn!(
                        path = %path.display(),
                        error = %e,
                        "detection config JSON invalid; using defaults"
                    );
                    Self::default()
                }
            },
            Err(e) => {
                warn!(
                    path = %path.display(),
                    error = %e,
                    "cannot read detection config; using defaults"
                );
                Self::default()
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PolicyConfig {
    /// Rule A: NewBeneficiary AND HighAmount → CHALLENGE
    pub high_amount_new_beneficiary: PolicyRule,
    /// Rule B: HighVelocity → CHALLENGE
    pub high_velocity: PolicyRule,
    /// Rule C: DistinctTargets → CHALLENGE
    pub distinct_targets: PolicyRule,
    /// Rule D: CredentialStuffingPattern → CHALLENGE
    pub credential_stuffing: PolicyRule,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            high_amount_new_beneficiary: PolicyRule::default(),
            high_velocity: PolicyRule::default(),
            distinct_targets: PolicyRule::default(),
            credential_stuffing: PolicyRule::default(),
        }
    }
}

/// One policy's config: on/off, mode, and fail behaviour if the daemon can't
/// reach a decision within its deadline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PolicyRule {
    pub enabled: bool,
    pub mode: PolicyMode,
    pub fail_mode: FailMode,
}

impl Default for PolicyRule {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: PolicyMode::Observe,
            fail_mode: FailMode::Open,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    fn tmp_config(json: &str) -> tempfile::TempDir {
        let d = tempfile::tempdir().expect("tempdir");
        let mut f = fs::File::create(d.path().join("policies.json")).unwrap();
        f.write_all(json.as_bytes()).unwrap();
        d
    }

    #[test]
    fn defaults_when_file_missing() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("does-not-exist.json");
        let cfg = DetectionConfig::load_or_default(&path);
        assert!(cfg.policies.high_amount_new_beneficiary.enabled);
        assert!(cfg.policies.high_velocity.enabled);
        assert!(cfg.policies.distinct_targets.enabled);
        assert_eq!(cfg.policies.high_velocity.mode, PolicyMode::Observe);
    }

    #[test]
    fn defaults_when_file_malformed() {
        let d = tmp_config("this is not json {{");
        let path = d.path().join("policies.json");
        let cfg = DetectionConfig::load_or_default(&path);
        assert!(cfg.policies.high_velocity.enabled); // fell back
    }

    #[test]
    fn per_policy_disabled_reads_back() {
        let d = tmp_config(r#"{
            "policies": {
                "distinct_targets": { "enabled": false }
            }
        }"#);
        let cfg = DetectionConfig::load_or_default(d.path().join("policies.json"));
        // Explicitly disabled:
        assert!(!cfg.policies.distinct_targets.enabled);
        // Untouched policies keep their defaults:
        assert!(cfg.policies.high_velocity.enabled);
        assert!(cfg.policies.high_amount_new_beneficiary.enabled);
    }

    #[test]
    fn mode_override_reads_back() {
        let d = tmp_config(r#"{
            "policies": {
                "high_velocity": { "mode": "ENFORCE", "fail_mode": "CLOSED" }
            }
        }"#);
        let cfg = DetectionConfig::load_or_default(d.path().join("policies.json"));
        assert_eq!(cfg.policies.high_velocity.mode, PolicyMode::Enforce);
        assert_eq!(cfg.policies.high_velocity.fail_mode, FailMode::Closed);
        // enabled defaults to true
        assert!(cfg.policies.high_velocity.enabled);
    }

    #[test]
    fn signal_thresholds_read_back() {
        let d = tmp_config(r#"{
            "signals": {
                "high_amount_floor": 5000.0,
                "amount_field": "amount",
                "velocity": { "window_ms": 30000, "threshold": 5, "max_subjects": 500 },
                "targets": { "window_ms": 1800000, "threshold": 3, "max_subjects": 500, "max_targets_per_subject": 100 }
            }
        }"#);
        let cfg = DetectionConfig::load_or_default(d.path().join("policies.json"));
        assert!((cfg.signals.high_amount_floor - 5000.0).abs() < f64::EPSILON);
        assert_eq!(cfg.signals.velocity.window_ms, 30000);
        assert_eq!(cfg.signals.velocity.threshold, 5);
        assert_eq!(cfg.signals.targets.threshold, 3);
    }

    #[test]
    fn full_round_trip_serialize_deserialize() {
        let cfg = DetectionConfig::default();
        let json = serde_json::to_string_pretty(&cfg).unwrap();
        let back: DetectionConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.policies.high_velocity.enabled, cfg.policies.high_velocity.enabled);
    }
}
