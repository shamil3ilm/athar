//! athar daemon binary (SPEC §6.1).
//!
//! V0 responsibilities:
//! - Bind a local TCP socket.
//! - Accept connections from shims.
//! - Decode length-prefixed canonical-event frames.
//! - Validate structurally, append to the append-only segment log.
//! - Shut down cleanly on Ctrl-C (SIGINT), fsyncing and renaming the active segment
//!   so a subsequent boot does not have to quarantine.
//!
//! Deliberately NOT in V0: SQLite state store, audit-chain signing at segment close,
//! CEL policy evaluation, decision records, reconciliation.

mod config;
mod host_metrics;
mod ingest;

use anyhow::Context as _;
use tracing::info;

use crate::config::Config;
use crate::ingest::IngestServer;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")))
        .init();

    let cfg = Config::from_env();
    info!(
        listen = %cfg.listen_addr,
        data_dir = %cfg.data_dir.display(),
        quota_mb = cfg.quota_bytes / 1024 / 1024,
        "athar-daemon starting"
    );

    let server = IngestServer::new(cfg).context("build ingest server")?;

    let shutdown = async {
        // ctrl_c works on Windows and Unix.
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %e, "failed to install signal handler");
        }
    };

    server.run(shutdown).await?;
    Ok(())
}
