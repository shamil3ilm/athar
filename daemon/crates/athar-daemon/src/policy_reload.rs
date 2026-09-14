//! Live policy reload — watches `<data-dir>/config/policies.json` and swaps
//! the daemon's `PolicyEngine` config when the file changes. No restart needed.
//!
//! Detection strategy: file mtime. Simple, portable (works on both Linux and
//! Windows), avoids inotify/kqueue platform deps. Poll interval is
//! configurable via `ATHAR_POLICY_RELOAD_INTERVAL_SECS` (0 disables reload).
//!
//! Semantics:
//! - Only `PolicyConfig` (the policy rules) is reloaded. `SignalEngineConfig`
//!   thresholds ARE deserialized but IGNORED — the SignalEngine holds live
//!   rolling-window state (velocity/target trackers) that shouldn't be reset
//!   or reconfigured mid-stream. Signal threshold changes require a restart.
//! - A missing file after successful earlier load is treated as "no change"
//!   (config was deleted; keep last-known config running rather than reverting
//!   to defaults, which would be a silent policy weakening).
//! - Malformed JSON after successful earlier load logs a warning and keeps
//!   the last-known good config.
//! - Only actually calls `.reload()` when the parsed config is DIFFERENT from
//!   the last one applied — avoids noisy log spam on `touch` without edits.
//!
//! INV-15: any panic in the watcher is contained to its own task. If it dies,
//! the daemon continues to enforce whatever policy config it had at boot.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use athar_detection::{DetectionConfig, PolicyConfig, PolicyEngine};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

/// Spawn the policy-file watcher. `interval == Duration::ZERO` returns a
/// handle to a no-op task so the caller doesn't need to branch.
pub fn spawn_watcher(
    engine: Arc<PolicyEngine>,
    path: PathBuf,
    interval: Duration,
) -> JoinHandle<()> {
    if interval.is_zero() {
        info!(path = %path.display(), "policy live-reload disabled (interval=0)");
        return tokio::spawn(async {});
    }
    info!(
        path = %path.display(),
        interval_secs = interval.as_secs(),
        "policy live-reload enabled"
    );
    tokio::spawn(async move { run(engine, path, interval).await })
}

async fn run(engine: Arc<PolicyEngine>, path: PathBuf, interval: Duration) {
    let mut last_mtime: Option<SystemTime> = None;
    let mut last_applied: Option<PolicyConfig> = Some(engine.snapshot());
    let mut ticker = tokio::time::interval(interval);
    // Skip the immediate first tick — otherwise on daemon boot the very first
    // tick tries to reload before any user edit could have happened, and the
    // reload is a no-op that spams the logs.
    ticker.tick().await;
    loop {
        ticker.tick().await;
        match tokio::fs::metadata(&path).await {
            Ok(meta) => {
                let mtime = meta.modified().ok();
                if mtime == last_mtime {
                    continue; // no change since last check
                }
                last_mtime = mtime;
                match tokio::fs::read_to_string(&path).await {
                    Ok(text) => match serde_json::from_str::<DetectionConfig>(&text) {
                        Ok(new_cfg) => {
                            let new_policies = new_cfg.policies;
                            if last_applied.as_ref() != Some(&new_policies) {
                                let summary = summarise(&new_policies);
                                engine.reload(new_policies.clone());
                                last_applied = Some(new_policies);
                                info!(path = %path.display(), summary = %summary, "policy config reloaded");
                            } else {
                                debug!(path = %path.display(), "policy file mtime changed but content is identical — no reload");
                            }
                        }
                        Err(e) => {
                            warn!(
                                path = %path.display(),
                                error = %e,
                                "policy file changed but JSON is invalid — keeping previous config"
                            );
                        }
                    },
                    Err(e) => {
                        warn!(
                            path = %path.display(),
                            error = %e,
                            "policy file changed but cannot be read — keeping previous config"
                        );
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // File disappeared after we'd been running with a config.
                // Keep previous config running — reverting to defaults would
                // silently weaken policy on an accidental `rm`.
                if last_mtime.is_some() {
                    warn!(path = %path.display(), "policy file was deleted; keeping last-known config running");
                    last_mtime = None;
                }
            }
            Err(e) => {
                warn!(path = %path.display(), error = %e, "cannot stat policy file");
            }
        }
    }
}

/// Compact one-line summary of what was applied, for the reload log line.
fn summarise(p: &PolicyConfig) -> String {
    let one = |name: &str, r: &athar_detection::PolicyRule| {
        format!(
            "{name}={{{}:{}:{}}}",
            if r.enabled { "on" } else { "off" },
            r.mode.as_str(),
            r.fail_mode.as_str(),
        )
    };
    format!(
        "{} {} {}",
        one("hi_amt_new_ben", &p.high_amount_new_beneficiary),
        one("hi_vel",         &p.high_velocity),
        one("dist_tgt",       &p.distinct_targets),
    )
}

