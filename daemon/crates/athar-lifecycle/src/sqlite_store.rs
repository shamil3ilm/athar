//! SQLite-backed lifecycle store (SPEC §6.5, OPS-9, D4).
//!
//! Persistent home for the lifecycle projection. Uses SQLite in WAL mode. Each
//! `Lifecycle` is stored as a row with a small set of indexed columns for
//! fast lookups (`resource_id`, `business_key`, `state`, `closure`) plus a JSON
//! blob column holding the full body (`event_ids`, `tiers`, `late_events`, ...).
//!
//! A separate `event_bindings` table supports tier-3 (causation) lookups.
//!
//! Crash safety: WAL means an unclean shutdown leaves the database consistent to
//! the last committed transaction. Individual `upsert` operations commit
//! immediately for durability.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension};

use crate::store::LifecycleStore;
use crate::types::*;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
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

CREATE TABLE IF NOT EXISTS lifecycles (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    lifecycle_type TEXT NOT NULL,
    business_key TEXT,
    resource_id TEXT,
    state TEXT NOT NULL,
    closure TEXT NOT NULL,
    started_at_ms INTEGER NOT NULL,
    last_event_at_ms INTEGER NOT NULL,
    closed_at_ms INTEGER,
    body_json TEXT NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_lifecycles_resource
    ON lifecycles(tenant_id, resource_id) WHERE resource_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_lifecycles_business_key
    ON lifecycles(tenant_id, business_key) WHERE business_key IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_lifecycles_open
    ON lifecycles(closure, last_event_at_ms);

CREATE TABLE IF NOT EXISTS event_bindings (
    event_id TEXT PRIMARY KEY,
    lifecycle_id TEXT NOT NULL
);
";

pub struct SqliteLifecycleStore {
    conn: Mutex<Connection>,
}

impl SqliteLifecycleStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn: Mutex::new(conn) })
    }
}

// ---- enum <-> string helpers (avoid depending on serde_json's exact form) ----

fn state_str(s: State) -> &'static str {
    match s {
        State::Started => "Started",
        State::Processing => "Processing",
        State::Pending => "Pending",
        State::Success => "Success",
        State::Failed => "Failed",
        State::Cancelled => "Cancelled",
        State::Rejected => "Rejected",
        State::Timeout => "Timeout",
        State::Expired => "Expired",
        State::Abandoned => "Abandoned",
        State::Reversed => "Reversed",
        State::Compensated => "Compensated",
        State::PartiallyCompleted => "PartiallyCompleted",
        State::Conflicted => "Conflicted",
        State::Unknown => "Unknown",
    }
}

fn closure_str(c: Closure) -> &'static str {
    match c {
        Closure::Open => "Open",
        Closure::Closed => "Closed",
        Closure::ClosedWithException => "ClosedWithException",
        Closure::ClosedWithUncertainty => "ClosedWithUncertainty",
    }
}

fn lct_str(t: LifecycleType) -> &'static str {
    match t {
        LifecycleType::Payment => "Payment",
    }
}

fn deserialize_row(json: &str) -> Option<Lifecycle> {
    serde_json::from_str(json).ok()
}

impl LifecycleStore for SqliteLifecycleStore {
    fn get(&self, id: &str) -> Option<Lifecycle> {
        let conn = self.conn.lock().expect("sqlite mutex");
        let json: Option<String> = conn
            .query_row(
                "SELECT body_json FROM lifecycles WHERE id = ?",
                params![id],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten();
        json.as_deref().and_then(deserialize_row)
    }

    fn find_by_resource(&self, r: &str) -> Option<String> {
        let conn = self.conn.lock().expect("sqlite mutex");
        conn.query_row(
            "SELECT id FROM lifecycles WHERE resource_id = ? LIMIT 1",
            params![r],
            |row| row.get(0),
        )
        .optional()
        .ok()
        .flatten()
    }

    fn find_by_business_key(&self, k: &str) -> Option<String> {
        let conn = self.conn.lock().expect("sqlite mutex");
        conn.query_row(
            "SELECT id FROM lifecycles WHERE business_key = ? LIMIT 1",
            params![k],
            |row| row.get(0),
        )
        .optional()
        .ok()
        .flatten()
    }

    fn find_by_event(&self, event_id: &str) -> Option<String> {
        let conn = self.conn.lock().expect("sqlite mutex");
        conn.query_row(
            "SELECT lifecycle_id FROM event_bindings WHERE event_id = ?",
            params![event_id],
            |row| row.get(0),
        )
        .optional()
        .ok()
        .flatten()
    }

    fn upsert(&self, lc: &Lifecycle) {
        let json = match serde_json::to_string(lc) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, id = %lc.id, "lifecycle serialize failed; skipping upsert");
                return;
            }
        };
        let mut conn = self.conn.lock().expect("sqlite mutex");
        let tx = match conn.transaction() {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(error = %e, "begin transaction failed");
                return;
            }
        };
        let res = tx.execute(
            "INSERT OR REPLACE INTO lifecycles \
             (id, tenant_id, lifecycle_type, business_key, resource_id, state, closure, \
              started_at_ms, last_event_at_ms, closed_at_ms, body_json) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                lc.id,
                lc.tenant_id,
                lct_str(lc.lifecycle_type),
                lc.business_key,
                lc.resource_id,
                state_str(lc.state),
                closure_str(lc.closure),
                lc.started_at_ms as i64,
                lc.last_event_at_ms as i64,
                lc.closed_at_ms.map(|v| v as i64),
                json,
            ],
        );
        if let Err(e) = res {
            tracing::warn!(error = %e, "lifecycle upsert failed");
            return;
        }
        // ACID-A: every event binding for this lifecycle either lands with the
        // lifecycle row or none of them does. A silent-ignore-and-commit here
        // would leave `lifecycles` in the "new" state while `event_bindings`
        // is stale, breaking tier-3 causation lookups on some events but not
        // others. Abort on first failure; the rusqlite Transaction rolls back
        // on drop (default RollbackOnDrop).
        for ev in &lc.event_ids {
            if let Err(e) = tx.execute(
                "INSERT OR REPLACE INTO event_bindings (event_id, lifecycle_id) VALUES (?, ?)",
                params![ev, lc.id],
            ) {
                tracing::warn!(
                    error = %e,
                    lifecycle_id = %lc.id,
                    event_id = %ev,
                    "event binding insert failed; rolling back lifecycle upsert",
                );
                return;
            }
        }
        if let Err(e) = tx.commit() {
            tracing::warn!(error = %e, "lifecycle upsert commit failed");
        }
    }

    fn record_event(&self, event_id: &str, lifecycle_id: &str) {
        let conn = self.conn.lock().expect("sqlite mutex");
        if let Err(e) = conn.execute(
            "INSERT OR REPLACE INTO event_bindings (event_id, lifecycle_id) VALUES (?, ?)",
            params![event_id, lifecycle_id],
        ) {
            // Single-row insert — no atomicity concern. But a silent failure
            // here means a subsequent tier-3 (causation) lookup for this event
            // won't find its lifecycle. Log so operators can spot it.
            tracing::warn!(error = %e, event_id, lifecycle_id, "record_event insert failed");
        }
    }

    fn all_open(&self) -> Vec<Lifecycle> {
        let conn = self.conn.lock().expect("sqlite mutex");
        let mut stmt = match conn.prepare(
            "SELECT body_json FROM lifecycles WHERE closure = 'Open' ORDER BY started_at_ms",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt.query_map([], |r| r.get::<_, String>(0));
        let Ok(rows) = rows else { return vec![] };
        rows.filter_map(|r| r.ok().and_then(|json| deserialize_row(&json)))
            .collect()
    }

    fn count(&self) -> usize {
        let conn = self.conn.lock().expect("sqlite mutex");
        conn.query_row("SELECT COUNT(*) FROM lifecycles", [], |r| r.get::<_, i64>(0))
            .map(|c| c as usize)
            .unwrap_or(0)
    }

    fn count_open(&self) -> usize {
        let conn = self.conn.lock().expect("sqlite mutex");
        conn.query_row(
            "SELECT COUNT(*) FROM lifecycles WHERE closure = 'Open'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|c| c as usize)
        .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Lifecycle {
        Lifecycle {
            id: "lc_sample".into(),
            tenant_id: "tnt_test".into(),
            lifecycle_type: LifecycleType::Payment,
            business_key: Some("op_1".into()),
            resource_id: Some("pay_1".into()),
            state: State::Started,
            closure: Closure::Open,
            started_at_ms: 1000,
            last_event_at_ms: 1000,
            closed_at_ms: None,
            event_ids: vec!["e1".into()],
            late_events: vec![],
            staleness_ms: 60_000,
            tiers: vec![InferenceTier::ResourceId],
        }
    }

    #[test]
    fn upsert_then_get_roundtrips() {
        let store = SqliteLifecycleStore::open_in_memory().expect("open");
        let lc = sample();
        store.upsert(&lc);
        let got = store.get("lc_sample").expect("present");
        assert_eq!(got.id, lc.id);
        assert_eq!(got.state, State::Started);
        assert_eq!(got.closure, Closure::Open);
        assert_eq!(got.event_ids, vec!["e1"]);
    }

    #[test]
    fn indexes_return_correct_ids() {
        let store = SqliteLifecycleStore::open_in_memory().expect("open");
        let lc = sample();
        store.upsert(&lc);
        assert_eq!(store.find_by_resource("pay_1"), Some("lc_sample".into()));
        assert_eq!(store.find_by_business_key("op_1"), Some("lc_sample".into()));
        assert_eq!(store.find_by_event("e1"), Some("lc_sample".into()));
        assert_eq!(store.find_by_resource("pay_missing"), None);
    }

    #[test]
    fn count_open_reflects_closure() {
        let store = SqliteLifecycleStore::open_in_memory().expect("open");
        let mut lc = sample();
        store.upsert(&lc);
        assert_eq!(store.count_open(), 1);
        lc.state = State::Success;
        lc.closure = Closure::Closed;
        lc.closed_at_ms = Some(2000);
        store.upsert(&lc);
        assert_eq!(store.count(), 1);
        assert_eq!(store.count_open(), 0);
    }

    #[test]
    fn survives_close_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        {
            let store = SqliteLifecycleStore::open(&path).expect("open");
            store.upsert(&sample());
        }
        // Reopen: data still there.
        let store = SqliteLifecycleStore::open(&path).expect("reopen");
        assert_eq!(store.count(), 1);
        let lc = store.get("lc_sample").expect("present after reopen");
        assert_eq!(lc.state, State::Started);
    }

    #[test]
    fn upsert_writes_all_event_bindings_atomically() {
        // ACID-A guard: given N event_ids on a lifecycle, all N bindings
        // must be queryable after upsert. Missing any one means the code
        // silently swallowed an error mid-transaction — regression on the
        // "abort on first binding failure" contract in upsert().
        let store = SqliteLifecycleStore::open_in_memory().expect("open");
        let mut lc = sample();
        lc.id = "lc_multi".into();
        lc.event_ids = vec!["e_alpha".into(), "e_beta".into(), "e_gamma".into()];
        store.upsert(&lc);

        assert_eq!(store.find_by_event("e_alpha").as_deref(), Some("lc_multi"));
        assert_eq!(store.find_by_event("e_beta").as_deref(),  Some("lc_multi"));
        assert_eq!(store.find_by_event("e_gamma").as_deref(), Some("lc_multi"));
    }

    #[test]
    fn reupsert_preserves_previous_event_bindings() {
        // An upsert with an EXPANDED event_ids list should keep the old
        // bindings AND add the new one. INSERT OR REPLACE on event_bindings
        // is keyed on event_id, so re-inserting an existing event with the
        // same lifecycle is a no-op; a new event_id inserts fresh.
        let store = SqliteLifecycleStore::open_in_memory().expect("open");
        let mut lc = sample();
        lc.id = "lc_grow".into();
        lc.event_ids = vec!["e1".into()];
        store.upsert(&lc);
        assert_eq!(store.find_by_event("e1").as_deref(), Some("lc_grow"));

        lc.event_ids = vec!["e1".into(), "e2".into()];
        store.upsert(&lc);
        assert_eq!(store.find_by_event("e1").as_deref(), Some("lc_grow"));
        assert_eq!(store.find_by_event("e2").as_deref(), Some("lc_grow"));
    }

    #[test]
    fn all_open_returns_only_open() {
        let store = SqliteLifecycleStore::open_in_memory().expect("open");
        let mut a = sample();
        a.id = "lc_a".into();
        a.resource_id = Some("res_a".into());
        a.business_key = None;
        store.upsert(&a);

        let mut b = sample();
        b.id = "lc_b".into();
        b.resource_id = Some("res_b".into());
        b.business_key = None;
        b.state = State::Success;
        b.closure = Closure::Closed;
        store.upsert(&b);

        let open = store.all_open();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, "lc_a");
    }
}
