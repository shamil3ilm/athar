//! Shim-spool collector — closes V0 criterion 3.
//!
//! When the shim can't reach the daemon (daemon down, port change, network
//! partition), PHP-FPM has no next request to retry with, so the shim writes
//! a small JSONL loss record to a well-known spool directory. This module
//! scans that directory (on startup and on a periodic tick), turns each record
//! into a canonical `coverage_gap` event, and commits it through the same
//! ingest path as any other event.
//!
//! Once a file is fully ingested (event appended + audit-chain commitment
//! written), the collector deletes it so it isn't re-ingested on next boot.
//! If deletion fails, worst case is duplicate ingestion; the daemon's audit
//! chain will still be internally consistent.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use athar_audit::writer::AuditChainWriter;
use athar_storage::segment_log::SegmentLog;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

/// Default spool directory; MUST match the shim's `Athar\Shim\Spool::defaultDir()`.
pub fn default_dir() -> PathBuf {
    std::env::temp_dir().join("athar-shim-spool")
}

/// One line from a shim-spool JSONL file.
#[derive(Debug, Deserialize, Serialize)]
struct ShimLossRecord {
    kind: String, // "shim_loss"
    reason: String,
    shim_pid: i64,
    at_ms: u64,
    frames_lost: u64,
    bytes_lost: u64,
    #[serde(default = "default_tenant")]
    tenant_id: String,
}

fn default_tenant() -> String { "tnt_unknown".into() }

pub struct ShimSpoolCollector {
    pub spool_dir: PathBuf,
    pub log: Arc<Mutex<SegmentLog>>,
    pub audit: Arc<Mutex<AuditChainWriter>>,
}

impl ShimSpoolCollector {
    /// Scan the spool directory once, ingest every file, delete each on success.
    /// Returns the number of records ingested.
    pub async fn drain(&self) -> usize {
        if !self.spool_dir.exists() {
            debug!(dir = %self.spool_dir.display(), "spool dir does not exist; skipping");
            return 0;
        }
        let entries = match std::fs::read_dir(&self.spool_dir) {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, "failed to read spool dir");
                return 0;
            }
        };
        let mut ingested = 0usize;
        for entry in entries {
            let path = match entry {
                Ok(e) => e.path(),
                Err(e) => { warn!(error = %e, "failed to read spool dir entry"); continue; }
            };
            if !path.is_file() { continue; }
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") { continue; }

            match self.ingest_file(&path).await {
                Ok(n) => {
                    ingested += n;
                    if let Err(e) = std::fs::remove_file(&path) {
                        warn!(path = %path.display(), error = %e, "failed to delete ingested spool file; will re-ingest next boot");
                    }
                }
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "failed to ingest spool file; leaving in place");
                }
            }
        }
        if ingested > 0 {
            info!(ingested, dir = %self.spool_dir.display(), "shim-spool: ingested coverage_gap record(s)");
        }
        ingested
    }

    async fn ingest_file(&self, path: &Path) -> anyhow::Result<usize> {
        let content = std::fs::read_to_string(path)?;
        let mut n = 0usize;
        for (line_no, raw) in content.lines().enumerate() {
            let raw = raw.trim();
            if raw.is_empty() { continue; }
            let rec: ShimLossRecord = match serde_json::from_str(raw) {
                Ok(r) => r,
                Err(e) => {
                    warn!(path = %path.display(), line = line_no + 1, error = %e, "malformed spool record; skipping line");
                    continue;
                }
            };
            self.ingest_record(&rec).await?;
            n += 1;
        }
        Ok(n)
    }

    /// Turn one shim-loss record into a `coverage_gap` event: append the raw
    /// JSON to the evidence log, then commit through the audit chain.
    async fn ingest_record(&self, rec: &ShimLossRecord) -> anyhow::Result<()> {
        let payload = serde_json::to_vec(&serde_json::json!({
            "kind": "coverage_gap",
            "cause": "SHIM_DAEMON_UNREACHABLE",
            "reason": rec.reason,
            "shim_pid": rec.shim_pid,
            "frames": rec.frames_lost,
            "bytes": rec.bytes_lost,
            "window_started_at_ms": rec.at_ms,
            "recorded_at_ms": now_millis(),
            "tenant_id": rec.tenant_id,
        }))?;
        {
            let mut log = self.log.lock().await;
            log.append(&payload)?;
        }
        {
            let mut audit = self.audit.lock().await;
            audit.observe("coverage_gap", &payload)?;
        }
        Ok(())
    }
}

/// Spawn a periodic collector task alongside a one-shot startup drain.
pub fn spawn_collector(collector: Arc<ShimSpoolCollector>, interval: Duration) -> JoinHandle<()> {
    tokio::spawn(async move {
        // One-shot drain on startup — catches everything spooled since last daemon exit.
        collector.drain().await;

        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tick.tick().await; // skip first fire
        loop {
            tick.tick().await;
            collector.drain().await;
        }
    })
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use athar_audit::signer::FileKeySigner;
    use athar_storage::quota::Quota;
    use athar_storage::segment_log::Config as LogConfig;

    async fn build_collector(dir: &tempfile::TempDir, spool_dir: PathBuf) -> ShimSpoolCollector {
        let log = SegmentLog::open(LogConfig {
            root: dir.path().join("evidence"),
            max_segment_bytes: 1 << 20,
            max_record_bytes: 1 << 20,
            quota: Quota::new(1 << 30),
        }).expect("open log");
        let key_path = dir.path().join("audit-key");
        let signer = FileKeySigner::load_or_create_dev(&key_path).expect("signer");
        let audit = AuditChainWriter::open(
            dir.path().join("audit/segments"),
            "tnt_test".to_string(),
            Box::new(signer),
        ).expect("open audit").with_max_records_per_segment(100);
        ShimSpoolCollector {
            spool_dir,
            log: Arc::new(Mutex::new(log)),
            audit: Arc::new(Mutex::new(audit)),
        }
    }

    #[tokio::test]
    async fn drain_ingests_records_and_deletes_file() {
        let data_dir = tempfile::tempdir().unwrap();
        let spool_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            spool_dir.path().join("shim-1234-abc.jsonl"),
            r#"{"kind":"shim_loss","reason":"daemon_unreachable","shim_pid":1234,"at_ms":1700000000000,"frames_lost":7,"bytes_lost":2048,"tenant_id":"tnt_x"}"#,
        ).unwrap();

        let collector = build_collector(&data_dir, spool_dir.path().to_path_buf()).await;
        let n = collector.drain().await;
        assert_eq!(n, 1);

        // File deleted.
        let files: Vec<_> = std::fs::read_dir(spool_dir.path()).unwrap().collect();
        assert!(files.is_empty(), "spool file should be deleted after ingest");

        // Flush audit and verify a coverage_gap record was committed.
        collector.audit.lock().await.flush().unwrap();
        let audit_root = collector.audit.lock().await.store().root().to_path_buf();
        let store = athar_audit::persistence::SegmentStore::open(&audit_root).unwrap();
        let report = store.verify_all().unwrap();
        assert!(report.records_verified >= 1, "at least one audit record ingested");
    }

    #[tokio::test]
    async fn drain_handles_missing_dir() {
        let data_dir = tempfile::tempdir().unwrap();
        let missing = data_dir.path().join("does-not-exist");
        let collector = build_collector(&data_dir, missing).await;
        // No panic; no work.
        assert_eq!(collector.drain().await, 0);
    }

    #[tokio::test]
    async fn drain_skips_malformed_lines_but_keeps_going() {
        let data_dir = tempfile::tempdir().unwrap();
        let spool_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            spool_dir.path().join("shim-x.jsonl"),
            "not json at all\n{\"kind\":\"shim_loss\",\"reason\":\"x\",\"shim_pid\":1,\"at_ms\":1,\"frames_lost\":1,\"bytes_lost\":1}\nmore garbage\n",
        ).unwrap();
        let collector = build_collector(&data_dir, spool_dir.path().to_path_buf()).await;
        let n = collector.drain().await;
        assert_eq!(n, 1, "only the well-formed record should be ingested");
    }

    #[tokio::test]
    async fn non_jsonl_files_are_ignored() {
        let data_dir = tempfile::tempdir().unwrap();
        let spool_dir = tempfile::tempdir().unwrap();
        std::fs::write(spool_dir.path().join("readme.txt"), "just a note").unwrap();
        std::fs::write(spool_dir.path().join("valid.jsonl"),
            r#"{"kind":"shim_loss","reason":"x","shim_pid":1,"at_ms":1,"frames_lost":1,"bytes_lost":1}"#).unwrap();
        let collector = build_collector(&data_dir, spool_dir.path().to_path_buf()).await;
        assert_eq!(collector.drain().await, 1);
        // The .txt file must survive.
        assert!(spool_dir.path().join("readme.txt").exists());
    }
}
