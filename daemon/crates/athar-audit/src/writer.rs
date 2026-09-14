//! High-level audit chain writer used by the daemon.
//!
//! Combines `ChainWriter`, a `Signer`, and a `SegmentStore` behind one API:
//!
//! ```ignore
//! let signer = Box::new(FileKeySigner::load_or_create_dev(&keys)?);
//! let mut audit = AuditChainWriter::open(root, "tnt_default".into(), signer)?;
//! audit.observe("event", &canonical_bytes)?;
//! // ... N events ...
//! audit.flush()?; // closes and persists the current segment
//! ```
//!
//! Auto-rotation: `observe` closes the current segment when the record count reaches
//! `max_records_per_segment` (default 1000). Time-based rotation is not implemented
//! in V0 (SEC-12 also asks for at-least-hourly closure — the daemon can call
//! `flush()` on a timer).
//!
//! On open, the writer resumes the chain from the last persisted segment's tip, so
//! all segments (past + present) form one continuous chain (SEC-11).

use std::path::PathBuf;

use crate::persistence::{PersistError, SegmentStore};
use crate::{payload_commitment, AuditRecord, ChainWriter, Signer, GENESIS_PREV_HASH, HASH_LEN};

#[derive(Debug, thiserror::Error)]
pub enum WriterError {
    #[error("persistence: {0}")]
    Persist(#[from] PersistError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub struct AuditChainWriter {
    inner: ChainWriter,
    store: SegmentStore,
    signer: Box<dyn Signer + Send + Sync>,
    tenant_id: String,
    tip: [u8; HASH_LEN],
    next_seq: u64,
    next_segment_id: u64,
    max_records_per_segment: u64,
}

impl AuditChainWriter {
    pub fn open(
        root: PathBuf,
        tenant_id: String,
        signer: Box<dyn Signer + Send + Sync>,
    ) -> Result<Self, WriterError> {
        let store = SegmentStore::open(root)?;
        let ids = store.list()?;
        let (tip, next_seq, next_segment_id) = if let Some(&last_id) = ids.last() {
            let last = store.read(last_id)?;
            let tip = last
                .records
                .last()
                .map(|r| r.record_hash)
                .unwrap_or(GENESIS_PREV_HASH);
            let next_seq = last.records.last().map_or(0, |r| r.record.seq + 1);
            (tip, next_seq, last_id + 1)
        } else {
            (GENESIS_PREV_HASH, 0, 0)
        };
        Ok(Self {
            inner: ChainWriter::from_tip(tip, next_seq),
            store,
            signer,
            tenant_id,
            tip,
            next_seq,
            next_segment_id,
            max_records_per_segment: 1000,
        })
    }

    pub fn with_max_records_per_segment(mut self, n: u64) -> Self {
        self.max_records_per_segment = n.max(1);
        self
    }

    pub fn store(&self) -> &SegmentStore { &self.store }
    pub fn tip(&self) -> [u8; HASH_LEN] { self.tip }
    pub fn pending_records(&self) -> usize { self.inner.len() }
    pub fn next_segment_id(&self) -> u64 { self.next_segment_id }

    /// Commit to a payload. The audit chain does NOT store the payload itself —
    /// only its `BLAKE3` commitment. The payload lives in the evidence log.
    pub fn observe(&mut self, kind: &str, canonical_payload: &[u8]) -> Result<(), WriterError> {
        let now_us = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as i64;
        let record = AuditRecord {
            seq: 0, // ChainWriter assigns
            timestamp_us: now_us,
            tenant_id: self.tenant_id.clone(),
            kind: kind.to_string(),
            payload_commitment: payload_commitment(canonical_payload),
            redacted: false,
        };
        self.inner.append(record);
        if self.inner.len() as u64 >= self.max_records_per_segment {
            self.close_segment()?;
        }
        Ok(())
    }

    /// Close and persist the current segment. Returns the path if anything was written.
    pub fn close_segment(&mut self) -> Result<Option<PathBuf>, WriterError> {
        if self.inner.is_empty() {
            return Ok(None);
        }
        let id = self.next_segment_id;
        let seg = self.inner.close_segment(id, self.signer.as_ref());
        // Advance our tip + next_seq from the just-closed segment.
        if let Some(last) = seg.records.last() {
            self.tip = last.record_hash;
            self.next_seq = last.record.seq + 1;
        }
        // Continue the chain in a fresh writer state.
        self.inner = ChainWriter::from_tip(self.tip, self.next_seq);
        self.next_segment_id += 1;
        let path = self.store.write(&seg)?;
        Ok(Some(path))
    }

    /// Force closure of any pending records. Call on shutdown.
    pub fn flush(&mut self) -> Result<Option<PathBuf>, WriterError> {
        self.close_segment()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::SegmentStore;
    use crate::SoftwareSigner;

    fn tmp_root(tag: &str) -> PathBuf {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        std::env::temp_dir().join(format!("athar-audit-writer-{}-{}-{}", std::process::id(), tag, t))
    }
    fn cleanup(p: &std::path::Path) { let _ = std::fs::remove_dir_all(p); }

    #[test]
    fn writer_persists_and_verify_all_passes() {
        let root = tmp_root("happy");
        let signer = Box::new(SoftwareSigner::from_seed([9u8; 32]));
        let mut w = AuditChainWriter::open(root.clone(), "tnt_test".into(), signer)
            .expect("open")
            .with_max_records_per_segment(3);
        for i in 0..7 {
            w.observe("evt", format!("payload-{i}").as_bytes()).expect("observe");
        }
        // 7 events, 3 per segment → 2 full segments + 1 pending.
        w.flush().expect("flush");

        let store = SegmentStore::open(&root).expect("open store");
        let report = store.verify_all().expect("verify_all");
        assert_eq!(report.segments_verified, 3);
        assert_eq!(report.records_verified, 7);
        cleanup(&root);
    }

    #[test]
    fn chain_is_continuous_across_segments() {
        // The tip of segment N is the prev_hash of segment N+1's first record.
        let root = tmp_root("continuous");
        let signer = Box::new(SoftwareSigner::from_seed([2u8; 32]));
        let mut w = AuditChainWriter::open(root.clone(), "tnt_test".into(), signer)
            .expect("open")
            .with_max_records_per_segment(2);
        for i in 0..4 {
            w.observe("evt", format!("p-{i}").as_bytes()).unwrap();
        }
        w.flush().unwrap();

        let store = SegmentStore::open(&root).unwrap();
        let ids = store.list().unwrap();
        assert_eq!(ids.len(), 2);
        let s0 = store.read(ids[0]).unwrap();
        let s1 = store.read(ids[1]).unwrap();
        let tip_of_s0 = s0.records.last().unwrap().record_hash;
        let prev_of_s1_first = s1.records.first().unwrap().prev_hash;
        assert_eq!(tip_of_s0, prev_of_s1_first);

        // Also: verify_all traverses both segments as one chain.
        let report = store.verify_all().unwrap();
        assert_eq!(report.segments_verified, 2);
        assert_eq!(report.records_verified, 4);
        cleanup(&root);
    }

    #[test]
    fn reopen_resumes_from_last_tip() {
        let root = tmp_root("reopen");
        {
            let signer = Box::new(SoftwareSigner::from_seed([3u8; 32]));
            let mut w = AuditChainWriter::open(root.clone(), "tnt_test".into(), signer).unwrap();
            w.observe("evt", b"first").unwrap();
            w.observe("evt", b"second").unwrap();
            w.flush().unwrap();
        }
        {
            let signer = Box::new(SoftwareSigner::from_seed([3u8; 32]));
            let mut w = AuditChainWriter::open(root.clone(), "tnt_test".into(), signer).unwrap();
            w.observe("evt", b"third").unwrap();
            w.flush().unwrap();
        }

        let store = SegmentStore::open(&root).unwrap();
        let report = store.verify_all().expect("chain remains continuous across reopen");
        assert_eq!(report.segments_verified, 2);
        assert_eq!(report.records_verified, 3);
        cleanup(&root);
    }

    #[test]
    fn corrupted_on_disk_segment_is_caught_at_exact_index() {
        // Write a segment, then hand-corrupt one record's payload_commitment, then verify_all.
        let root = tmp_root("corrupt-disk");
        {
            let signer = Box::new(SoftwareSigner::from_seed([4u8; 32]));
            let mut w = AuditChainWriter::open(root.clone(), "tnt_test".into(), signer).unwrap();
            for i in 0..5 { w.observe("evt", format!("x-{i}").as_bytes()).unwrap(); }
            w.flush().unwrap();
        }
        // Corrupt by rewriting record 2's payload_commitment hex.
        let store = SegmentStore::open(&root).unwrap();
        let ids = store.list().unwrap();
        let mut seg = store.read(ids[0]).unwrap();
        // Flip a bit in the commitment.
        seg.records[2].record.payload_commitment[0] ^= 0x01;
        store.write(&seg).unwrap();

        let err = store.verify_all().expect_err("must fail");
        match err {
            PersistError::Verify(crate::VerifyError::RecordHashMismatch { index: 2 }) => {}
            other => panic!("expected RecordHashMismatch at 2, got {other:?}"),
        }
        cleanup(&root);
    }
}
