//! Canonical event, v1.0 (SPEC §5.4, MOD-6).
//!
//! Every integration MUST normalize into this shape before core processing.
//! Types here mirror `schema/event.v1.0.json` exactly. Any divergence is a defect.
//!
//! Design notes:
//! - `Option<T>` is used for genuinely optional fields; `None` serializes as absent.
//! - Identifier fields use `Option<String>` because MOD-7 requires null (absent),
//!   not empty string, when unknown. Validators in this crate enforce non-empty
//!   when present.
//! - All enums use `#[serde(rename_all = ...)]` matching the JSON Schema exactly.

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: &str = "1.0";

/// A canonical event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub schema_version: String,
    pub event_id: String,
    pub event_type: String,
    pub tenant_id: String,
    /// RFC3339: when it happened, per the source. MOD-8.
    pub timestamp: String,
    /// RFC3339: when the runtime saw it. MOD-8: never conflated with `timestamp`.
    pub received_at: String,
    pub clock: Clock,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<ActorRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authenticated_principal: Option<ActorRefWithAuthn>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<CallerRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_identity: Option<ServiceIdentityRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_behalf_of: Option<ActorRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub beneficiary: Option<ActorRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_owner: Option<ActorRef>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<ResourceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<OperationRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<LifecycleRef>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_point: Option<EntryPoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub technical_context: Option<TechnicalContext>,

    pub provenance: Provenance,
    pub truth: Truth,
    pub trust: Trust,
    pub causality: Causality,
    pub coverage: Coverage,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Clock {
    pub source: ClockSource,
    pub skew_estimate_ms: Option<i64>,
    pub monotonic_seq: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClockSource {
    Host,
    Shim,
    Adapter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum ResolutionStatus {
    Verified,
    Probable,
    Possible,
    Unknown,
    Conflicted,
    Revoked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Calibration {
    NominalUnvalidated,
    Calibrated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntityType {
    User,
    Service,
    Agent,
    System,
    External,
    Unknown,
}

/// A confidence in `[0.0, 1.0]`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Confidence(pub f64);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActorRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<EntityType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    pub resolution: ResolutionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<Confidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calibration: Option<Calibration>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<EvidenceConflict>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceConflict {
    pub value: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActorRefWithAuthn {
    #[serde(flatten)]
    pub actor: ActorRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallerRef {
    pub application_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceIdentityRef {
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spiffe_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceRef {
    pub id: Option<String>,
    pub r#type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OperationInference {
    Explicit,
    BusinessId,
    Causation,
    ResourceId,
    ActorContext,
    Semantic,
    Temporal,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperationRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    pub r#type: String,
    pub inference: OperationInference,
    pub confidence: Confidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calibration: Option<Calibration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LifecycleBinding {
    Explicit,
    BusinessId,
    Causation,
    ResourceId,
    ActorContext,
    Semantic,
    Temporal,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LifecycleRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_id: Option<String>,
    pub binding: LifecycleBinding,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<Confidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calibration: Option<Calibration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EntryPointType {
    HttpEndpoint,
    Queue,
    Scheduler,
    Cli,
    Webhook,
    Internal,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntryPoint {
    pub r#type: EntryPointType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_template: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TechnicalContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployment_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Origin {
    Human,
    System,
    External,
    Scheduled,
    Automated,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Trigger {
    UserAction,
    ApiRequest,
    BackgroundJob,
    Queue,
    Scheduler,
    Webhook,
    Polling,
    DatabaseChange,
    SystemRule,
    ExternalProcessor,
    Reconciliation,
    ManualAdminAction,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    pub origin: Origin,
    pub trigger: Trigger,
    pub source: String,
    pub producer: Option<String>,
    pub authority: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TruthStage {
    Observed,
    Executed,
    Authorized,
    Committed,
    Approved,
    Signed,
    Audited,
    Authoritative,
    Inferred,
    Uncertain,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Truth {
    pub stage: TruthStage,
    pub asserted_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TrustLevel {
    Verified,
    Probable,
    Possible,
    Unknown,
    Conflicted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Trust {
    pub level: TrustLevel,
    pub confidence: Confidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calibration: Option<Calibration>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub factors: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Causality {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub causation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(default)]
    pub customer_correlation_ids: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DegradationLevel {
    L0,
    L1,
    L2,
    L3,
    L4,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Coverage {
    pub complete: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shed: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redacted_fields: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degradation_level: Option<DegradationLevel>,
}

/// Validation errors that structural (JSON Schema-equivalent) checks catch.
#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error("schema_version must be {expected}, got {got}")]
    SchemaVersionMismatch { expected: &'static str, got: String },
    #[error("field {field} is required but missing or empty")]
    MissingRequired { field: &'static str },
    #[error("field {field} is present as empty string; MOD-7 requires null when unknown")]
    EmptyStringForUnknown { field: &'static str },
    #[error("field {field} contains reserved unknown-token '{value}'; MOD-7 requires null")]
    ReservedUnknownToken { field: &'static str, value: String },
    #[error("confidence {field} out of range: {value}")]
    ConfidenceOutOfRange { field: &'static str, value: f64 },
}

const RESERVED_UNKNOWN_TOKENS: &[&str] = &["unknown", "UNKNOWN", "null", "None", "-"];

impl Event {
    /// Structural post-deserialization checks. Complements the JSON Schema; called by the daemon
    /// on ingest before any downstream stage.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(ValidationError::SchemaVersionMismatch {
                expected: SCHEMA_VERSION,
                got: self.schema_version.clone(),
            });
        }
        check_nonempty("event_id", &self.event_id)?;
        check_nonempty("event_type", &self.event_type)?;
        check_nonempty("tenant_id", &self.tenant_id)?;
        check_nonempty("timestamp", &self.timestamp)?;
        check_nonempty("received_at", &self.received_at)?;

        if let Some(a) = &self.actor {
            check_actor("actor", a)?;
        }
        if let Some(a) = &self.on_behalf_of {
            check_actor("on_behalf_of", a)?;
        }
        if let Some(a) = &self.beneficiary {
            check_actor("beneficiary", a)?;
        }
        if let Some(a) = &self.resource_owner {
            check_actor("resource_owner", a)?;
        }
        if let Some(a) = &self.authenticated_principal {
            check_actor("authenticated_principal", &a.actor)?;
        }
        if let Some(t) = &self.trust.confidence.value_option() {
            if !(0.0..=1.0).contains(t) {
                return Err(ValidationError::ConfidenceOutOfRange { field: "trust.confidence", value: *t });
            }
        }
        Ok(())
    }
}

fn check_nonempty(field: &'static str, value: &str) -> Result<(), ValidationError> {
    if value.is_empty() {
        return Err(ValidationError::MissingRequired { field });
    }
    if RESERVED_UNKNOWN_TOKENS.iter().any(|t| *t == value) {
        return Err(ValidationError::ReservedUnknownToken { field, value: value.to_string() });
    }
    Ok(())
}

fn check_actor(field: &'static str, actor: &ActorRef) -> Result<(), ValidationError> {
    if let Some(id) = &actor.id {
        if id.is_empty() {
            return Err(ValidationError::EmptyStringForUnknown { field });
        }
        if RESERVED_UNKNOWN_TOKENS.iter().any(|t| *t == id) {
            return Err(ValidationError::ReservedUnknownToken { field, value: id.clone() });
        }
    }
    Ok(())
}

impl Confidence {
    fn value_option(&self) -> Option<f64> {
        Some(self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_event_json() -> serde_json::Value {
        serde_json::json!({
            "schema_version": "1.0",
            "event_id": "01J8F0Z0F0Z0F0Z0F0Z0F0Z0F0",
            "event_type": "http.request",
            "tenant_id": "tnt_demo",
            "timestamp": "2026-01-01T10:00:00.123456Z",
            "received_at": "2026-01-01T10:00:00.198000Z",
            "clock": { "source": "shim", "skew_estimate_ms": null, "monotonic_seq": 1 },
            "provenance": {
                "origin": "UNKNOWN", "trigger": "API_REQUEST",
                "source": "HTTP", "producer": null, "authority": null
            },
            "truth": { "stage": "OBSERVED", "asserted_by": "shim" },
            "trust": { "level": "UNKNOWN", "confidence": 0.0, "factors": [] },
            "causality": { "customer_correlation_ids": {} },
            "coverage": { "complete": true }
        })
    }

    #[test]
    fn minimal_event_round_trips() {
        let v = minimal_event_json();
        let e: Event = serde_json::from_value(v).expect("deserialize");
        e.validate().expect("validate");
        assert_eq!(e.schema_version, SCHEMA_VERSION);
        assert_eq!(e.tenant_id, "tnt_demo");
    }

    #[test]
    fn empty_string_actor_id_rejected() {
        // MOD-7: unknown MUST be null, never empty string.
        let mut v = minimal_event_json();
        v["actor"] = serde_json::json!({ "id": "", "type": "user", "resolution": "UNKNOWN" });
        let e: Event = serde_json::from_value(v).expect("deserialize");
        let err = e.validate().expect_err("must reject");
        assert!(matches!(err, ValidationError::EmptyStringForUnknown { field: "actor" }));
    }

    #[test]
    fn reserved_unknown_token_rejected() {
        let mut v = minimal_event_json();
        v["actor"] = serde_json::json!({ "id": "unknown", "type": "user", "resolution": "UNKNOWN" });
        let e: Event = serde_json::from_value(v).expect("deserialize");
        let err = e.validate().expect_err("must reject");
        assert!(matches!(err, ValidationError::ReservedUnknownToken { field: "actor", .. }));
    }

    #[test]
    fn schema_version_mismatch_rejected() {
        let mut v = minimal_event_json();
        v["schema_version"] = serde_json::json!("0.9");
        let e: Event = serde_json::from_value(v).expect("deserialize");
        let err = e.validate().expect_err("must reject");
        assert!(matches!(err, ValidationError::SchemaVersionMismatch { .. }));
    }
}
