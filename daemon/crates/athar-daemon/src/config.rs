use std::path::PathBuf;

/// Runtime configuration for the daemon binary.
///
/// Loaded from environment variables:
///   ATHAR_LISTEN_ADDR         default 127.0.0.1:11223
///   ATHAR_DATA_DIR            default ./athar-data
///   ATHAR_QUOTA_BYTES         default 2 GB
///   ATHAR_MAX_SEGMENT_BYTES   default 64 MB
///   ATHAR_MAX_RECORD_BYTES    default 8 MB
///
/// D10 stance: no control-plane configuration is read here. There is nothing to
/// misconfigure into calling home.
#[derive(Debug, Clone)]
pub struct Config {
    pub listen_addr: String,
    pub data_dir: PathBuf,
    pub quota_bytes: u64,
    pub max_segment_bytes: u64,
    pub max_record_bytes: usize,
    pub audit_records_per_segment: u64,
    pub staleness_scan_interval_secs: u64,
    pub host_metrics_interval_secs: u64,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            listen_addr: env_or("ATHAR_LISTEN_ADDR", "127.0.0.1:11223"),
            data_dir: PathBuf::from(env_or("ATHAR_DATA_DIR", "./athar-data")),
            quota_bytes: env_or_parse("ATHAR_QUOTA_BYTES", 2 * 1024 * 1024 * 1024),
            max_segment_bytes: env_or_parse("ATHAR_MAX_SEGMENT_BYTES", 64 * 1024 * 1024),
            max_record_bytes: env_or_parse::<usize>("ATHAR_MAX_RECORD_BYTES", 8 * 1024 * 1024),
            audit_records_per_segment: env_or_parse("ATHAR_AUDIT_RECORDS_PER_SEGMENT", 1000),
            staleness_scan_interval_secs: env_or_parse("ATHAR_STALENESS_SCAN_INTERVAL_SECS", 60),
            host_metrics_interval_secs: env_or_parse("ATHAR_HOST_METRICS_INTERVAL_SECS", 5),
        }
    }
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

fn env_or_parse<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}
