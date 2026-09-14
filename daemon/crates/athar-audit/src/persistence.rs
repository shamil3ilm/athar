//! On-disk audit segment store.
//!
//! One JSON file per segment. Filename `NNNNNNNNNNNNNNNN.audit.json` (16-hex).
//! Writes go through a `.tmp` file that is fsynced and renamed atomically so a
//! crash mid-write cannot leave a partial audit segment (a partial one would
//! break `verify_all()` — better to lose the segment than have an ambiguous one).
//!
//! `verify_all()` walks every segment on disk in id order and returns a report,
//! or the first `VerifyError` with its exact record index (SEC-13).

use std::path::{Path, PathBuf};

use crate::{verify_chain, verify_segment, Segment, VerifyError};

pub struct SegmentStore {
    root: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum PersistError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("verify: {0}")]
    Verify(#[from] VerifyError),
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct VerifyReport {
    pub segments_verified: usize,
    pub records_verified: usize,
}

const SUFFIX: &str = ".audit.json";
const TMP_SUFFIX: &str = ".audit.json.tmp";

impl SegmentStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, PersistError> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path { &self.root }

    /// Write a segment atomically. Returns the final path.
    pub fn write(&self, seg: &Segment) -> Result<PathBuf, PersistError> {
        let final_path = self.path_for(seg.segment_id);
        let tmp_path = self.root.join(format!("{:016x}{}", seg.segment_id, TMP_SUFFIX));
        let json = serde_json::to_vec_pretty(seg)?;
        {
            let mut f = std::fs::File::create(&tmp_path)?;
            use std::io::Write;
            f.write_all(&json)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp_path, &final_path)?;
        Ok(final_path)
    }

    pub fn read(&self, id: u64) -> Result<Segment, PersistError> {
        let path = self.path_for(id);
        let bytes = std::fs::read(&path)?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// List segment ids in ascending order.
    pub fn list(&self) -> Result<Vec<u64>, PersistError> {
        let mut ids = Vec::new();
        for entry in std::fs::read_dir(&self.root)? {
            let path = entry?.path();
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else { continue };
            if let Some(stem) = name.strip_suffix(SUFFIX) {
                if let Ok(id) = u64::from_str_radix(stem, 16) {
                    ids.push(id);
                }
            }
        }
        ids.sort();
        Ok(ids)
    }

    /// Verify every segment on disk, in id order. Adjacent segments' chains are
    /// verified to be continuous: segment N+1's first record's `prev_hash` MUST
    /// equal segment N's last record's `record_hash`.
    pub fn verify_all(&self) -> Result<VerifyReport, PersistError> {
        let ids = self.list()?;
        let mut report = VerifyReport::default();
        let mut prev_tip: Option<[u8; crate::HASH_LEN]> = None;
        for id in ids {
            let seg = self.read(id)?;
            // Standalone verification: chain + signature.
            verify_segment(&seg)?;
            // Continuity: if there was a prior tip, this segment MUST anchor to it.
            if let Some(anchor) = prev_tip {
                if let Some(first) = seg.records.first() {
                    if first.prev_hash != anchor {
                        return Err(PersistError::Verify(VerifyError::ChainBroken { index: 0 }));
                    }
                    // Re-verify from the anchor to be thorough.
                    verify_chain(&seg.records, anchor, first.record.seq)?;
                }
            }
            prev_tip = seg.records.last().map(|r| r.record_hash);
            report.segments_verified += 1;
            report.records_verified += seg.records.len();
        }
        Ok(report)
    }

    fn path_for(&self, id: u64) -> PathBuf {
        self.root.join(format!("{:016x}{}", id, SUFFIX))
    }
}
