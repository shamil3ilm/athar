//! SQLite persistence for signals and decisions.
//!
//! Lives alongside the lifecycle state store (`athar-lifecycle`). Kept as a
//! separate database for V0 to isolate schemas; later can be merged into one
//! `state.db` with more tables.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension};

use crate::decision::DecisionRecord;
use crate::signal::SignalRecord;

#[derive(Debug, thiserror::Error)]
pub enum DecisionStoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

const SCHEMA: &str = "
PRAGMA journal_mode=WAL;
PRAGMA foreign_keys=ON;
PRAGMA synchronous=NORMAL;

CREATE TABLE IF NOT EXISTS signals (
    signal_id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    confidence REAL NOT NULL,
    value TEXT NOT NULL,
    threshold TEXT,
    timestamp_ms INTEGER NOT NULL,
    event_id TEXT NOT NULL,
    lifecycle_id TEXT,
    tracker_version TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_signals_event ON signals(event_id);
CREATE INDEX IF NOT EXISTS idx_signals_lifecycle ON signals(lifecycle_id);
CREATE INDEX IF NOT EXISTS idx_signals_kind ON signals(tenant_id, kind, timestamp_ms);

CREATE TABLE IF NOT EXISTS decisions (
    decision_id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    timestamp_ms INTEGER NOT NULL,
    event_id TEXT NOT NULL,
    lifecycle_id TEXT,
    action TEXT NOT NULL,
    mode TEXT NOT NULL,
    body_json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_decisions_event ON decisions(event_id);
CREATE INDEX IF NOT EXISTS idx_decisions_lifecycle ON decisions(lifecycle_id);
CREATE INDEX IF NOT EXISTS idx_decisions_time ON decisions(tenant_id, timestamp_ms);
";

pub trait DecisionStore: Send + Sync {
    fn write_signal(&self, record: &SignalRecord) -> Result<(), DecisionStoreError>;
    fn write_decision(&self, record: &DecisionRecord) -> Result<(), DecisionStoreError>;
    fn get_decision(&self, id: &str) -> Option<DecisionRecord>;
    fn recent_decisions(&self, limit: usize) -> Vec<DecisionRecord>;
    fn count_decisions(&self) -> usize;
    fn count_signals(&self) -> usize;
}

pub struct SqliteDecisionStore {
    conn: Mutex<Connection>,
}

impl SqliteDecisionStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DecisionStoreError> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    pub fn open_in_memory() -> Result<Self, DecisionStoreError> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn: Mutex::new(conn) })
    }
}

impl DecisionStore for SqliteDecisionStore {
    fn write_signal(&self, r: &SignalRecord) -> Result<(), DecisionStoreError> {
        let conn = self.conn.lock().expect("mutex");
        conn.execute(
            "INSERT OR REPLACE INTO signals \
             (signal_id, tenant_id, kind, confidence, value, threshold, timestamp_ms, event_id, lifecycle_id, tracker_version) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                r.signal_id, r.tenant_id, r.kind.as_str(),
                r.confidence as f64, r.value, r.threshold,
                r.timestamp_ms as i64, r.event_id, r.lifecycle_id, r.tracker_version,
            ],
        )?;
        Ok(())
    }

    fn write_decision(&self, r: &DecisionRecord) -> Result<(), DecisionStoreError> {
        let body_json = serde_json::to_string(r)?;
        let conn = self.conn.lock().expect("mutex");
        conn.execute(
            "INSERT OR REPLACE INTO decisions \
             (decision_id, tenant_id, timestamp_ms, event_id, lifecycle_id, action, mode, body_json) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                r.decision_id, r.tenant_id, r.timestamp_ms as i64,
                r.subject.event_id, r.subject.lifecycle_id,
                r.action.as_str(), r.mode.as_str(), body_json,
            ],
        )?;
        Ok(())
    }

    fn get_decision(&self, id: &str) -> Option<DecisionRecord> {
        let conn = self.conn.lock().expect("mutex");
        let json: Option<String> = conn.query_row(
            "SELECT body_json FROM decisions WHERE decision_id = ?",
            params![id],
            |r| r.get(0),
        ).optional().ok().flatten();
        json.as_deref().and_then(|j| serde_json::from_str(j).ok())
    }

    fn recent_decisions(&self, limit: usize) -> Vec<DecisionRecord> {
        let conn = self.conn.lock().expect("mutex");
        let mut stmt = match conn.prepare(
            "SELECT body_json FROM decisions ORDER BY timestamp_ms DESC LIMIT ?",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt.query_map(params![limit as i64], |r| r.get::<_, String>(0));
        let Ok(rows) = rows else { return vec![] };
        rows.filter_map(|r| r.ok().and_then(|j| serde_json::from_str(&j).ok())).collect()
    }

    fn count_decisions(&self) -> usize {
        let conn = self.conn.lock().expect("mutex");
        conn.query_row("SELECT COUNT(*) FROM decisions", [], |r| r.get::<_, i64>(0))
            .map(|c| c as usize).unwrap_or(0)
    }

    fn count_signals(&self) -> usize {
        let conn = self.conn.lock().expect("mutex");
        conn.query_row("SELECT COUNT(*) FROM signals", [], |r| r.get::<_, i64>(0))
            .map(|c| c as usize).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision::{DecisionSubject, DecisionRecord};
    use crate::policy::{PolicyEngine};
    use crate::signal::{SignalEngine, SignalEngineConfig, record_from_signal};
    use athar_event::*;

    fn ev(id: &str, resource: &str, amount: f64) -> Event {
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
            resource: Some(ResourceRef { id: Some(resource.into()), r#type: "payment".into(), namespace: None }),
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
            data: Some(serde_json::json!({ "amount": amount })),
            attributes: None,
        }
    }

    #[test]
    fn end_to_end_signal_policy_decision_persists() {
        let store = SqliteDecisionStore::open_in_memory().unwrap();
        let signal_engine = SignalEngine::new(SignalEngineConfig::default());
        let policy_engine = PolicyEngine::default();

        // A high-amount payment to a new beneficiary → policy matches → decision produced.
        let event = ev("e1", "pay_new", 5_000.0);
        let signals = signal_engine.evaluate(&event);
        assert_eq!(signals.len(), 2);

        // Persist signals.
        let now_ms = 1_726_352_400_000;
        let mut signal_records = Vec::new();
        for s in &signals {
            let r = record_from_signal(s, "tnt_t", &event.event_id, Some("lc_e1"), now_ms, signal_engine.tracker_version());
            store.write_signal(&r).unwrap();
            signal_records.push(r);
        }

        // Evaluate policy.
        let policy_decision = policy_engine.evaluate(&signals);
        assert!(policy_decision.matched);

        // Build and persist decision record.
        let decision = DecisionRecord::build(
            "tnt_t",
            DecisionSubject { lifecycle_id: Some("lc_e1".into()), event_id: event.event_id.clone(), operation_id: None },
            policy_decision,
            signal_records,
            now_ms,
            "L0",
            123,
        );
        let did = decision.decision_id.clone();
        store.write_decision(&decision).unwrap();

        // Read back.
        assert_eq!(store.count_decisions(), 1);
        assert_eq!(store.count_signals(), 2);
        let back = store.get_decision(&did).expect("decision persists");
        assert_eq!(back.decision_id, did);
        assert_eq!(back.action, crate::policy::Action::Challenge);
        assert_eq!(back.mode, crate::policy::PolicyMode::Observe);
        assert_eq!(back.signals_used.len(), 2);
        assert!(back.explanation.contains("matched"));
    }

    #[test]
    fn recent_decisions_orders_newest_first() {
        let store = SqliteDecisionStore::open_in_memory().unwrap();
        let policy_engine = PolicyEngine::default();
        for (i, ts) in [1000_u64, 3000, 2000].iter().enumerate() {
            let policy = policy_engine.evaluate(&[
                crate::signal::Signal { kind: crate::signal::SignalKind::NewBeneficiary, confidence: 0.9, value: "x".into(), threshold: None },
                crate::signal::Signal { kind: crate::signal::SignalKind::HighAmount, confidence: 0.9, value: "x".into(), threshold: None },
            ]);
            let dec = DecisionRecord::build(
                "tnt_t",
                DecisionSubject { lifecycle_id: None, event_id: format!("e{i}"), operation_id: None },
                policy, vec![], *ts, "L0", 100,
            );
            store.write_decision(&dec).unwrap();
        }
        let recent = store.recent_decisions(3);
        assert_eq!(recent.len(), 3);
        assert!(recent[0].timestamp_ms >= recent[1].timestamp_ms);
        assert!(recent[1].timestamp_ms >= recent[2].timestamp_ms);
    }
}
