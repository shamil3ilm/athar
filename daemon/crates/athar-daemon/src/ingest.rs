//! TCP ingest server (SPEC §6.1, OPS-2/4/7, INV-6, MOD-29).
//!
//! Wire protocol V0:
//!
//!   [len:u32-BE][JSON canonical event]
//!   [len:u32-BE][JSON canonical event]
//!   ...
//!
//! Per connection: decode frames, deserialize into `athar_event::Event`, run structural
//! validation (`Event::validate`), overwrite `received_at` with the daemon's clock,
//! append the canonical JSON bytes to the segment log AND commit through the audit chain.
//!
//! Under pressure (`OPS-17`/`OPS-18`): the ingest task subscribes to the governor's
//! `PressureLevel`. Ingest events are classified as `Priority::P1` (evidence integrity
//! and audit chain, §2.2). If the governor is at L4 (safe mode), ingest drops the
//! frame and increments the drop counters — a summarising `coverage_gap` audit record
//! (`MOD-29`) is emitted on shutdown or when pressure returns to normal.
//!
//! On any per-frame parse or validation error, log and continue with the next frame.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::Context as _;
use futures::StreamExt;
use tokio::io::AsyncRead;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio_util::codec::{FramedRead, LengthDelimitedCodec};
use tracing::{error, info, warn};

use athar_audit::signer::FileKeySigner;
use athar_audit::writer::AuditChainWriter;
use athar_event::Event;
use athar_governor::{Config as GovConfig, Governor, Priority};
use athar_lifecycle::{ApplyOutcome, LifecycleEngine, LifecycleStore, SqliteLifecycleStore, StalenessScanner};
use athar_detection::{
    DecisionRecord, DecisionStore, PolicyEngine, SignalEngine,
    SqliteDecisionStore,
    decision::DecisionSubject,
    signal::{SignalEngineConfig, record_from_signal},
};
use athar_storage::quota::Quota;
use athar_storage::segment_log::{Config as LogConfig, SegmentLog};

use crate::config::Config;

pub struct IngestServer {
    config: Config,
    pub(crate) log: Arc<Mutex<SegmentLog>>,
    pub(crate) audit: Arc<Mutex<AuditChainWriter>>,
    pub(crate) governor: Arc<Governor>,
    pub(crate) drops: DropCounters,
    pub(crate) lifecycles: Arc<dyn LifecycleStore>,
    pub(crate) engine: Arc<LifecycleEngine>,
    pub(crate) signal_engine: Arc<SignalEngine>,
    pub(crate) policy_engine: Arc<PolicyEngine>,
    pub(crate) decisions: Arc<dyn DecisionStore>,
}

/// Aggregate drop counters. Written on the ingest hot path; read on shutdown or
/// on pressure-level transitions to emit a summary `coverage_gap`.
#[derive(Debug, Default, Clone)]
pub struct DropCounters {
    pub frames: Arc<AtomicU64>,
    pub bytes: Arc<AtomicU64>,
    pub since_ms: Arc<AtomicU64>,
}

impl DropCounters {
    pub fn record(&self, frame_bytes: usize) {
        if self.frames.fetch_add(1, Ordering::Relaxed) == 0 {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            self.since_ms.store(now_ms, Ordering::Relaxed);
        }
        self.bytes.fetch_add(frame_bytes as u64, Ordering::Relaxed);
    }

    /// Take and reset. Returns None if no drops.
    pub fn take(&self) -> Option<CoverageGapSummary> {
        let frames = self.frames.swap(0, Ordering::Relaxed);
        if frames == 0 {
            return None;
        }
        let bytes = self.bytes.swap(0, Ordering::Relaxed);
        let since_ms = self.since_ms.swap(0, Ordering::Relaxed);
        Some(CoverageGapSummary { frames, bytes, window_started_at_ms: since_ms })
    }
}

/// A snapshot of drops used to produce one `coverage_gap` audit + evidence record.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CoverageGapSummary {
    pub frames: u64,
    pub bytes: u64,
    pub window_started_at_ms: u64,
}

impl IngestServer {
    pub fn new(config: Config) -> anyhow::Result<Self> {
        // Evidence log.
        let log_cfg = LogConfig {
            root: config.data_dir.join("evidence"),
            max_segment_bytes: config.max_segment_bytes,
            max_record_bytes: config.max_record_bytes,
            quota: Quota::new(config.quota_bytes),
        };
        let log = SegmentLog::open(log_cfg).context("opening segment log")?;
        let quarantined = log.quarantined_bytes_at_open();
        if quarantined > 0 {
            warn!(bytes = quarantined, "recovered from a crash: quarantined leftover .wip segment(s)");
        }

        // Audit chain.
        let audit_root = config.data_dir.join("audit");
        let key_path = audit_root.join("keys").join("segment.key");
        let signer = FileKeySigner::load_or_create_dev(&key_path)
            .context("open audit-chain signing key")?;
        let audit = AuditChainWriter::open(
            audit_root.join("segments"),
            "tnt_default".to_string(),
            Box::new(signer),
        )
        .context("open audit chain")?
        .with_max_records_per_segment(config.audit_records_per_segment);

        // Governor. Defaults are per §7.1. No live host-metric collector in V0 yet —
        // callers may push readings via `governor.report_host(...)` from a separate task.
        let governor = Arc::new(Governor::new(GovConfig::default()));

        info!(
            evidence_root = %config.data_dir.join("evidence").display(),
            audit_root = %audit_root.display(),
            "storage ready"
        );

        // SQLite state store for lifecycles — restart-durable (D4, OPS-9).
        let state_dir = config.data_dir.join("state");
        std::fs::create_dir_all(&state_dir).context("create state dir")?;
        let state_path = state_dir.join("lifecycles.db");
        let sqlite_store = SqliteLifecycleStore::open(&state_path)
            .context("open SQLite lifecycle store")?;
        info!(state_db = %state_path.display(), "lifecycle store ready");

        // Detection: signals + policy + decision persistence.
        let decisions_path = state_dir.join("decisions.db");
        let decision_store = SqliteDecisionStore::open(&decisions_path)
            .context("open decision store")?;
        info!(decisions_db = %decisions_path.display(), "decision store ready");

        Ok(Self {
            config,
            log: Arc::new(Mutex::new(log)),
            audit: Arc::new(Mutex::new(audit)),
            governor,
            drops: DropCounters::default(),
            lifecycles: Arc::new(sqlite_store),
            engine: Arc::new(LifecycleEngine::new()),
            signal_engine: Arc::new(SignalEngine::new(SignalEngineConfig::default())),
            policy_engine: Arc::new(PolicyEngine::new()),
            decisions: Arc::new(decision_store),
        })
    }

    /// Test seam: expose the governor so tests can drive pressure changes.
    pub fn governor(&self) -> Arc<Governor> { Arc::clone(&self.governor) }

    /// Test seam: read the drop counters.
    pub fn drops(&self) -> DropCounters { self.drops.clone() }

    /// Test seam: read the live lifecycle store.
    pub fn lifecycles(&self) -> Arc<dyn LifecycleStore> { Arc::clone(&self.lifecycles) }

    /// Test seam: read the decision store.
    pub fn decisions(&self) -> Arc<dyn DecisionStore> { Arc::clone(&self.decisions) }

    pub async fn run<S: std::future::Future<Output = ()>>(self, shutdown: S) -> anyhow::Result<()> {
        let listener = TcpListener::bind(&self.config.listen_addr)
            .await
            .with_context(|| format!("bind {}", self.config.listen_addr))?;
        info!(addr = %self.config.listen_addr, "ingest listening");

        // Background task: emit a coverage_gap summary each time pressure returns to L0
        // after having been at L4. Also emits on shutdown (below).
        let coverage_task = tokio::spawn(watch_pressure_and_emit_gaps(
            self.governor.subscribe(),
            self.drops.clone(),
            Arc::clone(&self.audit),
            Arc::clone(&self.log),
        ));

        // Background task: periodic staleness scan. MOD-25 / V0 criterion 6.
        let scanner_task = tokio::spawn(run_staleness_scanner(
            Arc::clone(&self.lifecycles),
            std::time::Duration::from_secs(self.config.staleness_scan_interval_secs),
        ));

        // Background task: live host-metrics collector — feeds the governor with
        // real CPU/memory usage every N seconds. Closes V0 criterion 14 for real
        // host stress (previously only injected budget observations proved the path).
        let host_metrics_task = crate::host_metrics::spawn_collector(
            Arc::clone(&self.governor),
            std::time::Duration::from_secs(self.config.host_metrics_interval_secs),
        );

        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                _ = &mut shutdown => {
                    info!("shutdown signal received");
                    break;
                }
                accept = listener.accept() => {
                    match accept {
                        Ok((sock, peer)) => {
                            let log = Arc::clone(&self.log);
                            let audit = Arc::clone(&self.audit);
                            let governor = Arc::clone(&self.governor);
                            let drops = self.drops.clone();
                            let lifecycles = Arc::clone(&self.lifecycles);
                            let engine = Arc::clone(&self.engine);
                            let signal_engine = Arc::clone(&self.signal_engine);
                            let policy_engine = Arc::clone(&self.policy_engine);
                            let decisions = Arc::clone(&self.decisions);
                            let gov_for_level = Arc::clone(&self.governor);
                            tokio::spawn(async move {
                                if let Err(e) = handle_connection(sock, log, audit, governor, drops, lifecycles, engine, signal_engine, policy_engine, decisions, gov_for_level).await {
                                    warn!(?peer, error = %e, "connection ended with error");
                                }
                            });
                        }
                        Err(e) => {
                            error!(error = %e, "accept failed");
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        }
                    }
                }
            }
        }

        // Shutdown drain: emit final coverage_gap if any drops accumulated, then flush.
        emit_coverage_gap_if_any(&self.drops, &self.audit, &self.log).await;
        coverage_task.abort();
        scanner_task.abort();
        host_metrics_task.abort();

        {
            let mut audit = self.audit.lock().await;
            match audit.flush() {
                Ok(Some(path)) => info!(path = %path.display(), "audit segment flushed on shutdown"),
                Ok(None) => info!("no pending audit records at shutdown"),
                Err(e) => error!(error = %e, "audit flush failed at shutdown"),
            }
        }
        let log = Arc::try_unwrap(self.log)
            .map_err(|_| anyhow::anyhow!("segment log still borrowed at shutdown"))?
            .into_inner();
        log.close().context("closing segment log")?;
        info!("ingest stopped cleanly");
        Ok(())
    }
}

async fn watch_pressure_and_emit_gaps(
    mut rx: tokio::sync::watch::Receiver<athar_governor::PressureLevel>,
    drops: DropCounters,
    audit: Arc<Mutex<AuditChainWriter>>,
    log: Arc<Mutex<SegmentLog>>,
) {
    use athar_governor::PressureLevel;
    let mut was_l4 = *rx.borrow() == PressureLevel::L4SafeMode;
    loop {
        if rx.changed().await.is_err() { return; }
        let now = *rx.borrow();
        let is_l4 = now == PressureLevel::L4SafeMode;
        if was_l4 && !is_l4 {
            // Recovered. Emit the accumulated coverage_gap.
            emit_coverage_gap_if_any(&drops, &audit, &log).await;
        }
        was_l4 = is_l4;
    }
}

async fn emit_coverage_gap_if_any(
    drops: &DropCounters,
    audit: &Mutex<AuditChainWriter>,
    log: &Mutex<SegmentLog>,
) {
    let Some(summary) = drops.take() else { return };
    let payload = match serde_json::to_vec(&serde_json::json!({
        "kind": "coverage_gap",
        "cause": "L4_SAFE_MODE",
        "frames": summary.frames,
        "bytes": summary.bytes,
        "window_started_at_ms": summary.window_started_at_ms,
        "recorded_at_ms": now_millis(),
    })) {
        Ok(b) => b,
        Err(e) => {
            error!(error = %e, "failed to serialize coverage_gap");
            return;
        }
    };
    // Put the JSON body in the evidence log, then commit through the audit chain.
    if let Err(e) = log.lock().await.append(&payload) {
        error!(error = %e, "failed to append coverage_gap to evidence log");
        // Continue anyway — the commitment still records that a gap existed.
    }
    if let Err(e) = audit.lock().await.observe("coverage_gap", &payload) {
        error!(error = %e, "failed to commit coverage_gap through audit chain");
        return;
    }
    info!(
        frames = summary.frames,
        bytes = summary.bytes,
        "coverage_gap emitted"
    );
}

async fn handle_connection(
    sock: TcpStream,
    log: Arc<Mutex<SegmentLog>>,
    audit: Arc<Mutex<AuditChainWriter>>,
    governor: Arc<Governor>,
    drops: DropCounters,
    lifecycles: Arc<dyn LifecycleStore>,
    engine: Arc<LifecycleEngine>,
    signal_engine: Arc<SignalEngine>,
    policy_engine: Arc<PolicyEngine>,
    decisions: Arc<dyn DecisionStore>,
    gov_for_level: Arc<Governor>,
) -> anyhow::Result<()> {
    let (reader, _writer) = sock.into_split();
    process_stream(reader, log, audit, governor, drops, lifecycles, engine, signal_engine, policy_engine, decisions, gov_for_level).await
}

pub async fn process_stream<R: AsyncRead + Unpin>(
    reader: R,
    log: Arc<Mutex<SegmentLog>>,
    audit: Arc<Mutex<AuditChainWriter>>,
    governor: Arc<Governor>,
    drops: DropCounters,
    lifecycles: Arc<dyn LifecycleStore>,
    engine: Arc<LifecycleEngine>,
    signal_engine: Arc<SignalEngine>,
    policy_engine: Arc<PolicyEngine>,
    decisions: Arc<dyn DecisionStore>,
    gov_for_level: Arc<Governor>,
) -> anyhow::Result<()> {
    let codec = LengthDelimitedCodec::builder()
        .length_field_length(4)
        .max_frame_length(64 * 1024 * 1024)
        .big_endian()
        .new_codec();
    let mut framed = FramedRead::new(reader, codec);

    while let Some(next) = framed.next().await {
        let frame = match next {
            Ok(bytes) => bytes,
            Err(e) => {
                warn!(error = %e, "frame decode error; closing connection");
                break;
            }
        };
        if let Err(e) = ingest_one(&frame, &log, &audit, &governor, &drops, lifecycles.as_ref(), &engine, &signal_engine, &policy_engine, decisions.as_ref(), &gov_for_level).await {
            warn!(error = %e, "dropped one bad frame; continuing");
        }
    }
    Ok(())
}

async fn ingest_one(
    frame: &[u8],
    log: &Mutex<SegmentLog>,
    audit: &Mutex<AuditChainWriter>,
    governor: &Governor,
    drops: &DropCounters,
    lifecycles: &dyn LifecycleStore,
    engine: &LifecycleEngine,
    signal_engine: &SignalEngine,
    policy_engine: &PolicyEngine,
    decisions: &dyn DecisionStore,
    gov_for_level: &Governor,
) -> anyhow::Result<()> {
    // OPS-18: honour pressure within 100 ms. Ingest is P1 (evidence integrity).
    // At L4 safe mode, drop and record. At L0-L3, continue.
    if !governor.current_level().allows(Priority::P1) {
        drops.record(frame.len());
        return Ok(());
    }

    let mut event: Event = serde_json::from_slice(frame).context("parse canonical event")?;
    event.validate().context("validate event")?;

    // Daemon overwrites received_at with its own clock (D14, MOD-8).
    event.received_at = now_rfc3339();

    let canonical = serde_json::to_vec(&event).context("re-serialize")?;

    // Evidence FIRST (so the payload the commitment points at is durable),
    // then audit chain, then lifecycle. Lifecycle is a projection; if it fails,
    // the raw event is still recorded and the audit chain still holds.
    {
        let mut log = log.lock().await;
        log.append(&canonical).context("append evidence")?;
    }
    {
        let mut audit = audit.lock().await;
        audit.observe("event", &canonical).context("append audit commitment")?;
    }
    // Lifecycle apply is best-effort. Any panic or error is contained.
    let started = std::time::Instant::now();
    let outcome = engine.apply(lifecycles, &event, now_millis());
    let lifecycle_id: Option<String> = match &outcome {
        ApplyOutcome::Created { lifecycle_id, .. } => {
            tracing::debug!(lifecycle_id, "lifecycle created");
            Some(lifecycle_id.clone())
        }
        ApplyOutcome::Updated { lifecycle_id, .. } => Some(lifecycle_id.clone()),
        ApplyOutcome::LateEvent { lifecycle_id, class, .. } => {
            tracing::info!(lifecycle_id, ?class, "late event after closure");
            Some(lifecycle_id.clone())
        }
        ApplyOutcome::Unbound => {
            tracing::trace!(event_id = %event.event_id, "event not bound to any lifecycle");
            None
        }
    };

    // Detection: compute signals for this event, evaluate the observe-mode policy,
    // and persist a decision record (INV-16). Best-effort; never fails ingest.
    let signals = signal_engine.evaluate(&event);
    let now_ms = now_millis();
    let mut signal_records = Vec::with_capacity(signals.len());
    for s in &signals {
        let r = record_from_signal(
            s, &event.tenant_id, &event.event_id,
            lifecycle_id.as_deref(), now_ms, signal_engine.tracker_version(),
        );
        if let Err(e) = decisions.write_signal(&r) {
            tracing::warn!(error = %e, "failed to persist signal; continuing");
        }
        signal_records.push(r);
    }

    let policy_decision = policy_engine.evaluate(&signals);
    let level = gov_for_level.current_level();
    let level_str = format!("{:?}", level);
    let latency_us = started.elapsed().as_micros() as u64;
    let decision = DecisionRecord::build(
        &event.tenant_id,
        DecisionSubject {
            lifecycle_id: lifecycle_id.clone(),
            event_id: event.event_id.clone(),
            operation_id: event.operation.as_ref().and_then(|o| o.operation_id.clone()),
        },
        policy_decision,
        signal_records,
        now_ms,
        &level_str,
        latency_us,
    );
    if let Err(e) = decisions.write_decision(&decision) {
        tracing::warn!(error = %e, "failed to persist decision; continuing");
    }
    Ok(())
}

async fn run_staleness_scanner(lifecycles: Arc<dyn LifecycleStore>, interval: std::time::Duration) {
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let now = now_millis();
        let closed = StalenessScanner::sweep(lifecycles.as_ref(), now);
        if !closed.is_empty() {
            info!(count = closed.len(), "lifecycles closed with uncertainty");
        }
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn now_rfc3339() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let micros = now.subsec_micros();
    let (year, month, day, hour, min, sec) = unix_to_ymdhms(secs);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}.{micros:06}Z")
}

fn unix_to_ymdhms(secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let days_since_epoch = (secs / 86_400) as i64;
    let rem = (secs % 86_400) as u32;
    let hour = rem / 3600;
    let min = (rem % 3600) / 60;
    let sec = rem % 60;
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as u32, m as u32, d as u32, hour, min, sec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use athar_governor::{HostMetrics, PressureLevel};
    use std::time::Instant;
    use tokio::io::AsyncWriteExt;

    /// Test helper: process a reader against an IngestServer using its live state.
    /// Feed `frames` to the ingest pipeline and return only after the stream
    /// has been fully consumed. Closes the write side by dropping the whole
    /// DuplexStream so the server sees EOF and process_stream returns.
    ///
    /// NOTE: `tokio::io::split(client) → drop(WriteHalf)` does NOT signal EOF,
    /// because the ReadHalf keeps the DuplexStream alive. That deadlocks the
    /// server's FramedRead. Always use this helper (or drop the whole
    /// DuplexStream) instead of splitting the client.
    async fn drive_bytes(server: &IngestServer, frames: &[u8]) {
        let cap = frames.len().max(1024).min(1 << 20);
        let (mut client, server_side) = tokio::io::duplex(cap);
        client.write_all(frames).await.unwrap();
        drop(client);
        drive(server, server_side).await;
    }

    async fn drive(server: &IngestServer, reader: impl AsyncRead + Unpin) {
        process_stream(
            reader,
            Arc::clone(&server.log),
            Arc::clone(&server.audit),
            Arc::clone(&server.governor),
            server.drops(),
            Arc::clone(&server.lifecycles),
            Arc::clone(&server.engine),
            Arc::clone(&server.signal_engine),
            Arc::clone(&server.policy_engine),
            Arc::clone(&server.decisions),
            Arc::clone(&server.governor),
        ).await.unwrap();
    }

    async fn tmp_config() -> (Config, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = Config {
            listen_addr: "127.0.0.1:0".into(),
            data_dir: dir.path().to_path_buf(),
            quota_bytes: 64 * 1024 * 1024,
            max_segment_bytes: 1 * 1024 * 1024,
            max_record_bytes: 1 * 1024 * 1024,
            audit_records_per_segment: 100,
            staleness_scan_interval_secs: 3600,
            host_metrics_interval_secs: 3600,
        };
        (cfg, dir)
    }

    fn minimal_event_json() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "schema_version": "1.0",
            "event_id": "01J8F0Z0F0Z0F0Z0F0Z0F0Z0F0",
            "event_type": "http.request",
            "tenant_id": "tnt_test",
            "timestamp": "2026-09-14T10:00:00.000000Z",
            "received_at": "2026-09-14T10:00:00.000000Z",
            "clock": { "source": "shim", "skew_estimate_ms": null, "monotonic_seq": 1 },
            "provenance": { "origin": "UNKNOWN", "trigger": "API_REQUEST", "source": "HTTP", "producer": null, "authority": null },
            "truth": { "stage": "OBSERVED", "asserted_by": "shim" },
            "trust": { "level": "UNKNOWN", "confidence": 0.0, "factors": [] },
            "causality": { "customer_correlation_ids": {} },
            "coverage": { "complete": true }
        })).unwrap()
    }

    fn frame(body: &[u8]) -> Vec<u8> {
        let mut v = Vec::with_capacity(4 + body.len());
        v.extend_from_slice(&(body.len() as u32).to_be_bytes());
        v.extend_from_slice(body);
        v
    }

    #[tokio::test]
    async fn ingest_appends_evidence_and_audit_under_normal_pressure() {
        let (cfg, _tmp) = tmp_config().await;
        let server = IngestServer::new(cfg.clone()).expect("build");
        let governor = server.governor();
        assert_eq!(governor.current_level(), PressureLevel::L0Normal);

        // Simulate a client stream.
        let ev = minimal_event_json();
        let framed_bytes = frame(&ev);
        let drops = server.drops();
        let audit = Arc::clone(&server.audit);

        drive_bytes(&server, &framed_bytes).await;

        assert_eq!(drops.frames.load(Ordering::Relaxed), 0);
        // Flush audit so verify_all() sees the pending record.
        audit.lock().await.flush().unwrap();

        let store = audit.lock().await.store().root().to_path_buf();
        let s = athar_audit::persistence::SegmentStore::open(&store).unwrap();
        let report = s.verify_all().unwrap();
        assert!(report.records_verified >= 1, "at least one audit record persisted");
    }

    fn payment_event_json(event_id: &str, event_type: &str, resource_id: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "schema_version": "1.0",
            "event_id": event_id,
            "event_type": event_type,
            "tenant_id": "tnt_test",
            "timestamp": "2026-09-14T10:00:00.000000Z",
            "received_at": "2026-09-14T10:00:00.000000Z",
            "clock": { "source": "shim", "skew_estimate_ms": null, "monotonic_seq": 1 },
            "resource": { "id": resource_id, "type": "payment", "namespace": "tnt_test/payments" },
            "provenance": { "origin": "HUMAN", "trigger": "API_REQUEST", "source": "HTTP", "producer": null, "authority": null },
            "truth": { "stage": "OBSERVED", "asserted_by": "shim" },
            "trust": { "level": "UNKNOWN", "confidence": 0.0, "factors": [] },
            "causality": { "customer_correlation_ids": {} },
            "coverage": { "complete": true }
        })).unwrap()
    }

    fn payment_event_with_amount(event_id: &str, resource_id: &str, amount: f64) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "schema_version": "1.0",
            "event_id": event_id,
            "event_type": "payment.create",
            "tenant_id": "tnt_test",
            "timestamp": "2026-09-14T10:00:00.000000Z",
            "received_at": "2026-09-14T10:00:00.000000Z",
            "clock": { "source": "shim", "skew_estimate_ms": null, "monotonic_seq": 1 },
            "resource": { "id": resource_id, "type": "payment", "namespace": "tnt_test/payments" },
            "provenance": { "origin": "HUMAN", "trigger": "API_REQUEST", "source": "HTTP", "producer": null, "authority": null },
            "truth": { "stage": "OBSERVED", "asserted_by": "shim" },
            "trust": { "level": "UNKNOWN", "confidence": 0.0, "factors": [] },
            "causality": { "customer_correlation_ids": {} },
            "coverage": { "complete": true },
            "data": { "amount": amount }
        })).unwrap()
    }

    #[tokio::test]
    async fn detection_produces_decision_with_matched_policy_for_high_amount_new_beneficiary() {
        // V0 criterion 11 foundation: end-to-end signal → policy → decision record.
        let (cfg, _tmp) = tmp_config().await;
        let server = IngestServer::new(cfg).expect("build");

        // A single high-amount payment to a new beneficiary should trigger both signals
        // and match the observe-mode policy.
        let body = payment_event_with_amount("01AAAAAAAAAAAAAAAAAAAAAAAA", "pay_new_high", 5_000.0);
        drive_bytes(&server, &frame(&body)).await;

        // The decision is persisted and matches the policy.
        let decisions = server.decisions();
        assert_eq!(decisions.count_decisions(), 1);
        assert!(decisions.count_signals() >= 2, "at least new_beneficiary + high_amount recorded");
        let recent = decisions.recent_decisions(1);
        assert_eq!(recent.len(), 1);
        let d = &recent[0];
        assert_eq!(d.action, athar_detection::Action::Challenge);
        assert_eq!(d.mode, athar_detection::PolicyMode::Observe);
        assert!(d.reason_codes.iter().any(|r| r.contains("NEW_BENEFICIARY")));
        assert!(d.explanation.contains("matched"));
        assert!(d.signals_used.len() >= 2);
        // INV-17 mandatory fields present.
        assert_eq!(d.degradation_level, "L0Normal");
        assert_eq!(d.subject.event_id, "01AAAAAAAAAAAAAAAAAAAAAAAA");
    }

    #[tokio::test]
    async fn detection_produces_allow_decision_for_small_new_payment() {
        let (cfg, _tmp) = tmp_config().await;
        let server = IngestServer::new(cfg).expect("build");
        // Small amount → only new_beneficiary fires, policy does not match.
        let body = payment_event_with_amount("01BBBBBBBBBBBBBBBBBBBBBBBB", "pay_small", 42.0);
        drive_bytes(&server, &frame(&body)).await;

        let recent = server.decisions().recent_decisions(1);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].action, athar_detection::Action::Allow);
        assert!(!recent[0].policies_evaluated[0].matched);
    }

    #[tokio::test]
    async fn lifecycles_survive_daemon_restart() {
        // Create a daemon, ingest a payment.create for pay_persist, tear it down,
        // build a fresh IngestServer at the same data_dir, and verify the lifecycle
        // is still there in the SQLite state store.
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config {
            listen_addr: "127.0.0.1:0".into(),
            data_dir: dir.path().to_path_buf(),
            quota_bytes: 64 * 1024 * 1024,
            max_segment_bytes: 1 * 1024 * 1024,
            max_record_bytes: 1 * 1024 * 1024,
            audit_records_per_segment: 100,
            staleness_scan_interval_secs: 3600,
            host_metrics_interval_secs: 3600,
        };

        {
            let server = IngestServer::new(cfg.clone()).expect("first boot");
            let f = frame(&payment_event_json(
                "01AAAAAAAAAAAAAAAAAAAAAAAA",
                "payment.create",
                "pay_persist",
            ));
            drive_bytes(&server, &f).await;
            assert_eq!(server.lifecycles().count(), 1);
        } // server dropped here

        // Simulate a restart: fresh IngestServer at the same data_dir.
        let server2 = IngestServer::new(cfg).expect("second boot");
        assert_eq!(server2.lifecycles().count(), 1, "lifecycle should survive restart");
        let lc = server2.lifecycles().get(&format!("lc_{}", "01AAAAAAAAAAAAAAAAAAAAAAAA"))
            .expect("lifecycle present after restart");
        assert_eq!(lc.resource_id.as_deref(), Some("pay_persist"));
        assert_eq!(lc.state, athar_lifecycle::State::Started);
        assert_eq!(lc.closure, athar_lifecycle::Closure::Open);
    }

    #[tokio::test]
    async fn ingest_assembles_full_payment_lifecycle_to_closure() {
        // V0 criterion 5: payment lifecycle spanning multiple events, correlated by
        // resource_id, ends in a closed state through the real ingest path.
        let (cfg, _tmp) = tmp_config().await;
        let server = IngestServer::new(cfg).expect("build");

        // Three events sharing resource_id = "pay_e2e".
        let mut frames = Vec::new();
        for (id, kind) in [
            ("01AAAAAAAAAAAAAAAAAAAAAAAA", "payment.create"),
            ("01BBBBBBBBBBBBBBBBBBBBBBBB", "payment.process"),
            ("01CCCCCCCCCCCCCCCCCCCCCCCC", "payment.settle"),
        ] {
            let body = payment_event_json(id, kind, "pay_e2e");
            frames.extend_from_slice(&frame(&body));
        }

        drive_bytes(&server, &frames).await;
        let lifecycles = server.lifecycles();

        // One lifecycle exists, and it's closed with SUCCESS.
        assert_eq!(lifecycles.count(), 1);
        assert_eq!(lifecycles.count_open(), 0);
        let lc_id = lifecycles.find_by_resource("pay_e2e").expect("lifecycle bound to resource");
        let lc = lifecycles.get(&lc_id).unwrap();
        assert_eq!(lc.event_ids.len(), 3);
        assert_eq!(lc.state, athar_lifecycle::State::Success);
        assert_eq!(lc.closure, athar_lifecycle::Closure::Closed);
    }

    #[tokio::test]
    async fn ingest_late_event_after_closure_is_classified_no_reopen() {
        // V0 criterion 7 through the real ingest path.
        let (cfg, _tmp) = tmp_config().await;
        let server = IngestServer::new(cfg).expect("build");

        // Create + settle → closed.
        let mut frames = Vec::new();
        for (id, kind) in [
            ("01AAAAAAAAAAAAAAAAAAAAAAAA", "payment.create"),
            ("01BBBBBBBBBBBBBBBBBBBBBBBB", "payment.settle"),
        ] {
            frames.extend_from_slice(&frame(&payment_event_json(id, kind, "pay_late")));
        }
        drive_bytes(&server, &frames).await;

        // Second connection: late "payment.fail" → classified as CONFLICT, lifecycle stays closed.
        let late = frame(&payment_event_json("01CCCCCCCCCCCCCCCCCCCCCCCC", "payment.fail", "pay_late"));
        drive_bytes(&server, &late).await;

        let lc_id = server.lifecycles().find_by_resource("pay_late").unwrap();
        let lc = server.lifecycles().get(&lc_id).unwrap();
        // Still closed with SUCCESS from the settle event.
        assert_eq!(lc.state, athar_lifecycle::State::Success);
        assert_eq!(lc.closure, athar_lifecycle::Closure::Closed);
        // But the late event is recorded and classified.
        assert_eq!(lc.late_events.len(), 1);
        assert_eq!(lc.late_events[0].class, athar_lifecycle::LateEventClass::Conflict);
        assert_eq!(lc.late_events[0].event_type, "payment.fail");
    }

    #[tokio::test]
    async fn ingest_drops_and_records_coverage_gap_under_l4() {
        let (cfg, _tmp) = tmp_config().await;
        let server = IngestServer::new(cfg.clone()).expect("build");

        // Force the governor to L4 by tripping the dead-man's switch via
        // sustained-latency breach with a very short window.
        {
            let start = Instant::now();
            // Sustained breach configuration would come from Config; but the governor
            // in the server was created with defaults. Simplest: report an unhealthy
            // host that pushes us to L2, then simulate a missed heartbeat window.
            // Easier still: use the governor's public API to report and tick.
            //
            // The Governor is behind Arc; we use the public methods only.
            let g = server.governor();
            g.report_budget(athar_governor::BudgetObservation {
                p50_latency_us: 1000,
                p99_latency_us: 10_000,      // way over PERF-2
                throughput_delta_pct: -20.0,
                taken_at: start,
            });
            // Push past the sustained-breach window so deadman trips.
            let later = start + std::time::Duration::from_secs(60);
            g.tick(later);
            assert_eq!(g.current_level(), PressureLevel::L4SafeMode);
        }

        let ev = minimal_event_json();
        let framed_bytes = frame(&ev);
        let drops = server.drops();

        drive_bytes(&server, &framed_bytes).await;

        // The frame was dropped.
        assert_eq!(drops.frames.load(Ordering::Relaxed), 1);
        assert!(drops.bytes.load(Ordering::Relaxed) > 0);

        // Now simulate recovery: report healthy host, wait past good_news_window.
        {
            let g = server.governor();
            let recovery = Instant::now() + std::time::Duration::from_secs(60);
            g.report_host(HostMetrics {
                cpu_headroom_pct: 90.0,
                memory_free_pct: 90.0,
                disk_free_pct: 90.0,
                taken_at: recovery,
            });
            g.shim_heartbeat(recovery);
            g.report_budget(athar_governor::BudgetObservation {
                p50_latency_us: 100,
                p99_latency_us: 400,
                throughput_delta_pct: 0.0,
                taken_at: recovery,
            });
            // Advance well past good_news_window. A LIVE shim would keep sending
            // heartbeats during this interval; in the test we simulate that by
            // sending one heartbeat right at `later` so the NoShimConfirmation
            // trip doesn't re-fire and mask the recovery.
            let later = recovery + std::time::Duration::from_secs(120);
            g.shim_heartbeat(later);
            g.tick(later);
            // Governor returns to L0 after good_news_window.
            assert_eq!(g.current_level(), PressureLevel::L0Normal);
        }

        // Emit the pending coverage_gap manually (in the running daemon a background
        // task does this; here we call directly).
        emit_coverage_gap_if_any(&drops, &server.audit, &server.log).await;

        // After emit, counters are zeroed.
        assert_eq!(drops.frames.load(Ordering::Relaxed), 0);

        // Flush and verify: the coverage_gap should be in the audit chain.
        server.audit.lock().await.flush().unwrap();
        let store = server.audit.lock().await.store().root().to_path_buf();
        let s = athar_audit::persistence::SegmentStore::open(&store).unwrap();
        let report = s.verify_all().unwrap();
        assert!(report.records_verified >= 1, "coverage_gap record persisted");
    }
}
