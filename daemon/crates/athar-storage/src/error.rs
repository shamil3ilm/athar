use std::path::PathBuf;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, StorageError>;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("record too large ({size} bytes, max {max})")]
    RecordTooLarge { size: usize, max: usize },

    #[error("quota exceeded: total={total} + record={record} > limit={limit}")]
    QuotaExceeded { total: u64, record: u64, limit: u64 },

    #[error("segment corrupt at offset {offset} in {path}: {reason}")]
    SegmentCorrupt {
        path: PathBuf,
        offset: u64,
        reason: &'static str,
    },

    #[error("no active segment; call open() first")]
    NoActiveSegment,

    #[error("segment root does not exist and could not be created: {path}")]
    RootUnavailable { path: PathBuf },
}
