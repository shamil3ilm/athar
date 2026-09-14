//! Eviction ladder driver (SPEC §4.5, OPS-12, V0 criterion 4).
//!
//! Periodic background task that watches the evidence log's fill ratio and
//! evicts oldest closed segments when it exceeds `high_water_pct`, until fill
//! returns below `low_water_pct` (hysteresis to avoid thrash).
//!
//! Only the evidence log is subject to this driver — audit chain records
//! (`PRI-13`) MUST NOT be evicted before their retention period regardless of
//! disk pressure.
//!
//! V0 scope:
//!   - Only the first rung of the §4.5 ladder is wired: `evict_oldest_closed`
//!     (roughly `DropCorrelatedUneventful` — since V0 doesn't yet distinguish
//!     between payload/decision/gap records at storage layer).
//!   - When there are no more closed segments to evict but fill is still over
//!     high_water, we log a warning and wait for the next tick. `MOD-25`
//!     staleness closure will eventually close more open lifecycles which
//!     rotates active segments to closed → giving the evictor more to work with.
//!
//! Later stages wire the full ladder: `DropP4Artifacts` (caches), retention-based
//! eviction, `StopCaptureAlert` when even those don't free enough.

use std::sync::Arc;
use std::time::Duration;

use athar_storage::segment_log::SegmentLog;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{info, warn};

#[derive(Debug, Clone)]
pub struct EvictionConfig {
    /// If fill % >= this, start evicting. Default 85.
    pub high_water_pct: f32,
    /// Stop evicting once fill % drops below this. Default 70.
    pub low_water_pct: f32,
    /// How often to run the driver. Default 30s.
    pub interval: Duration,
    /// Safety cap: max evictions per tick. Default 32.
    pub max_evictions_per_tick: usize,
}

impl Default for EvictionConfig {
    fn default() -> Self {
        Self {
            high_water_pct: 85.0,
            low_water_pct: 70.0,
            interval: Duration::from_secs(30),
            max_evictions_per_tick: 32,
        }
    }
}

pub fn spawn_driver(log: Arc<Mutex<SegmentLog>>, config: EvictionConfig) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(config.interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // First tick fires immediately; skip so we don't run before the daemon
        // has ingested anything.
        tick.tick().await;
        info!(
            high_water_pct = config.high_water_pct,
            low_water_pct = config.low_water_pct,
            interval_ms = config.interval.as_millis() as u64,
            "eviction driver started"
        );
        loop {
            tick.tick().await;
            run_once(log.as_ref(), &config).await;
        }
    })
}

/// One pass of the eviction driver. Public so tests can exercise it deterministically.
pub async fn run_once(log: &Mutex<SegmentLog>, config: &EvictionConfig) -> EvictionOutcome {
    let mut evicted = 0usize;
    let initial_fill = fill_pct(log).await;
    if initial_fill < config.high_water_pct {
        return EvictionOutcome { evicted, initial_fill, final_fill: initial_fill, ran_dry: false };
    }
    while evicted < config.max_evictions_per_tick {
        let result = { log.lock().await.evict_oldest_closed() };
        match result {
            Ok(Some(id)) => {
                evicted += 1;
                info!(id, "evicted oldest closed segment");
            }
            Ok(None) => {
                let fill = fill_pct(log).await;
                warn!(fill_pct = fill, evicted, "eviction exhausted: no more closed segments");
                return EvictionOutcome { evicted, initial_fill, final_fill: fill, ran_dry: true };
            }
            Err(e) => {
                warn!(error = %e, "eviction failed");
                let fill = fill_pct(log).await;
                return EvictionOutcome { evicted, initial_fill, final_fill: fill, ran_dry: false };
            }
        }
        let fill = fill_pct(log).await;
        if fill < config.low_water_pct {
            info!(fill_pct = fill, evicted, "eviction reached low_water_pct");
            return EvictionOutcome { evicted, initial_fill, final_fill: fill, ran_dry: false };
        }
    }
    let final_fill = fill_pct(log).await;
    warn!(evicted, final_fill, "eviction hit max_per_tick; will resume next tick");
    EvictionOutcome { evicted, initial_fill, final_fill, ran_dry: false }
}

async fn fill_pct(log: &Mutex<SegmentLog>) -> f32 {
    let l = log.lock().await;
    let total = l.total_bytes();
    let limit = l.quota().limit_bytes;
    if limit == 0 {
        return 100.0;
    }
    (total as f64 / limit as f64 * 100.0) as f32
}

#[derive(Debug, Clone, PartialEq)]
pub struct EvictionOutcome {
    pub evicted: usize,
    pub initial_fill: f32,
    pub final_fill: f32,
    /// True if we tried to evict but there were no more closed segments.
    pub ran_dry: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use athar_storage::quota::Quota;
    use athar_storage::segment_log::{Config as LogConfig, SegmentLog};

    fn build_log(dir: &tempfile::TempDir, quota_bytes: u64, max_segment_bytes: u64) -> Arc<Mutex<SegmentLog>> {
        let cfg = LogConfig {
            root: dir.path().to_path_buf(),
            max_segment_bytes,
            max_record_bytes: 1024,
            quota: Quota::new(quota_bytes),
        };
        Arc::new(Mutex::new(SegmentLog::open(cfg).expect("open")))
    }

    #[tokio::test]
    async fn no_op_when_fill_below_high_water() {
        let dir = tempfile::tempdir().unwrap();
        let log = build_log(&dir, 10_000, 1024);
        // No writes → 0 bytes, 0% fill.
        let outcome = run_once(&log, &EvictionConfig { high_water_pct: 85.0, low_water_pct: 70.0, ..EvictionConfig::default() }).await;
        assert_eq!(outcome.evicted, 0);
        assert!(outcome.initial_fill < 85.0);
    }

    #[tokio::test]
    async fn evicts_when_fill_exceeds_high_water() {
        // Small quota + small segments so a few appends fill the log past high water.
        let dir = tempfile::tempdir().unwrap();
        let log = build_log(&dir, 200, 32);
        {
            let mut l = log.lock().await;
            // Each record: [len:u32-be][10 bytes] = 14 bytes on disk.
            // Segment cap 32 bytes → 2 records per segment, then rotate.
            // Append 10 records to fill and rotate several times.
            for _ in 0..10 {
                match l.append(b"1234567890") {
                    Ok(_) => {}
                    Err(_) => break, // quota rejected further; that's fine for the test
                }
            }
            l.rotate().unwrap(); // force close of the last active segment so evictor has targets
        }

        let outcome = run_once(
            &log,
            &EvictionConfig {
                high_water_pct: 40.0,
                low_water_pct: 20.0,
                interval: Duration::from_secs(30),
                max_evictions_per_tick: 32,
            },
        )
        .await;
        assert!(outcome.evicted >= 1, "expected at least one eviction, got {outcome:?}");
        assert!(outcome.final_fill < outcome.initial_fill, "fill should decrease: {outcome:?}");
    }

    #[tokio::test]
    async fn ran_dry_reported_when_no_closed_segments_left() {
        let dir = tempfile::tempdir().unwrap();
        // Quota 20 bytes, segment cap huge — appends stay in one active .wip.
        let log = build_log(&dir, 20, 1024 * 1024);
        {
            let mut l = log.lock().await;
            l.append(b"1234567890").unwrap(); // 14 bytes → 70% fill, one active segment
        }
        // Push high water below current fill so the driver tries to evict.
        let outcome = run_once(
            &log,
            &EvictionConfig {
                high_water_pct: 50.0,
                low_water_pct: 20.0,
                interval: Duration::from_secs(30),
                max_evictions_per_tick: 32,
            },
        )
        .await;
        assert!(outcome.ran_dry, "should have ran_dry: {outcome:?}");
        assert_eq!(outcome.evicted, 0);
    }
}
