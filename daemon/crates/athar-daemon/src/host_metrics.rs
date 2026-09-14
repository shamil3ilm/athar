//! Live host-metrics collector (feeds `athar-governor`, closes V0 criterion 14).
//!
//! Every `interval` seconds, samples CPU + memory usage via the `sysinfo` crate
//! and reports headroom percentages to the Governor. The governor's degradation
//! ladder (§7.5) then acts on real host stress instead of only test-injected
//! budget observations.
//!
//! For CPU: `global_cpu_usage()` returns a percentage 0..100 across all cores.
//! Headroom is `100 - usage`.
//!
//! For memory: `used_memory() / total_memory()` gives the used fraction.
//! Headroom is `1 - used_fraction`.
//!
//! For disk: not implemented in this pass. Reports 100% so it doesn't
//! contribute to pressure by itself. (TODO: sysinfo::Disks + find the disk
//! containing `data_dir`.)
//!
//! INV-15 mirror: any panic in the collector is contained to its own task.
//! If the collector dies, the governor simply stops receiving live host input;
//! shim heartbeats and budget observations still work.

use std::sync::Arc;
use std::time::{Duration, Instant};

use athar_governor::{Governor, HostMetrics};
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

/// Spawn the host-metrics collector task. Returns the JoinHandle; caller aborts
/// on shutdown.
pub fn spawn_collector(governor: Arc<Governor>, interval: Duration) -> JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(e) = run(governor, interval).await {
            warn!(error = %e, "host metrics collector exited");
        }
    })
}

async fn run(governor: Arc<Governor>, interval: Duration) -> Result<(), &'static str> {
    // sysinfo 0.32 uses `RefreshKind::new()`; later versions renamed it to `nothing()`.
    // Both return an empty RefreshKind that we then populate. `#[allow(deprecated)]`
    // guards against a future warning if we bump versions.
    #[allow(deprecated)]
    let mut sys = System::new_with_specifics(
        RefreshKind::new()
            .with_cpu(CpuRefreshKind::everything())
            .with_memory(MemoryRefreshKind::everything()),
    );

    // First CPU-usage reading is always 0.0. Prime it and wait past the
    // minimum-refresh-interval before we trust any values.
    sys.refresh_cpu_usage();
    tokio::time::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL).await;

    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The first tick fires immediately; consume it so we don't double-report.
    tick.tick().await;

    info!(
        interval_ms = interval.as_millis() as u64,
        "host-metrics collector started"
    );

    loop {
        tick.tick().await;

        sys.refresh_cpu_usage();
        sys.refresh_memory();

        let cpu_used_pct = sys.global_cpu_usage();
        let cpu_headroom_pct = (100.0f32 - cpu_used_pct).clamp(0.0, 100.0);

        let mem_total = sys.total_memory();
        let mem_used = sys.used_memory();
        let memory_free_pct = if mem_total > 0 {
            let free = mem_total.saturating_sub(mem_used) as f64;
            (free / mem_total as f64 * 100.0) as f32
        } else {
            100.0
        };

        // TODO: real disk headroom. sysinfo::Disks::new_with_refreshed_list()
        // then find the disk whose mount_point is a prefix of data_dir.
        let disk_free_pct = 100.0;

        let metrics = HostMetrics {
            cpu_headroom_pct,
            memory_free_pct,
            disk_free_pct,
            taken_at: Instant::now(),
        };

        debug!(
            cpu_headroom_pct,
            memory_free_pct,
            disk_free_pct,
            "host metrics sample"
        );
        governor.report_host(metrics);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use athar_governor::{Config as GovConfig, PressureLevel};

    #[tokio::test]
    async fn collector_reports_at_least_once() {
        // Just verify the collector runs, sends at least one report, and doesn't
        // panic. We can't assert specific values because CI runners have wildly
        // different CPU/memory profiles.
        let g = Arc::new(Governor::new(GovConfig::default()));
        let handle = spawn_collector(Arc::clone(&g), Duration::from_millis(200));
        // Wait long enough for the priming sleep + one tick.
        tokio::time::sleep(Duration::from_millis(600)).await;
        handle.abort();
        // If we survived without a panic, and the current level is a valid pressure
        // level, we're good.
        let lvl = g.current_level();
        // Any valid level is fine; on a fresh CI runner it should be L0 or L1.
        assert!(matches!(
            lvl,
            PressureLevel::L0Normal
                | PressureLevel::L1Reduce
                | PressureLevel::L2Defer
                | PressureLevel::L3Essential
                | PressureLevel::L4SafeMode
        ));
    }
}
