//! Append-only segmented log (SPEC §6.5, INV-9).
//!
//! One "log" is a directory of segment files. Each segment is a sequence of
//! length-prefixed records:
//!
//! ```text
//! [len:u32-BE][data:len]
//! [len:u32-BE][data:len]
//! ...
//! ```
//!
//! Segment files are named `NNNNNNNNNNNNNNNN.seg` (16-hex, zero-padded segment id).
//! The active segment carries a `.wip` suffix; on rotate we fsync and rename to
//! `.seg` atomically. On open, any leftover `.wip` from a prior crash is renamed
//! to `.quarantine` (OPS-16) — never silently truncated.
//!
//! There is no update or delete on prior records. Retention deletes whole
//! segments (a different operation, not exposed on this type).

use std::fs::{File, OpenOptions, ReadDir};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

use crate::error::{Result, StorageError};
use crate::quota::{Fits, Quota};

const SEG_EXT: &str = "seg";
const WIP_EXT: &str = "seg.wip";
const QUARANTINE_EXT: &str = "seg.quarantine";
const RECORD_HEADER_BYTES: usize = 4;

#[derive(Debug, Clone)]
pub struct Config {
    pub root: PathBuf,
    /// Rotate the active segment when it reaches this many bytes.
    pub max_segment_bytes: u64,
    /// Maximum bytes for a single record.
    pub max_record_bytes: usize,
    /// Disk quota over the entire log.
    pub quota: Quota,
}

impl Config {
    pub fn defaults(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            max_segment_bytes: 64 * 1024 * 1024, // 64 MB per segment
            max_record_bytes: 8 * 1024 * 1024,   // 8 MB per record
            quota: Quota::new(2 * 1024 * 1024 * 1024), // 2 GB default
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppendReceipt {
    pub segment_id: u64,
    pub record_index: u64,
    pub offset_in_segment: u64,
    pub bytes_written: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentInfo {
    pub id: u64,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub record_count: u64,
    pub state: SegmentState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentState {
    Closed,
    Active,
    Quarantined,
}

pub struct SegmentLog {
    config: Config,
    active: Option<Active>,
    /// Sum of bytes across all `.seg` and `.wip` files in `root`.
    total_bytes: u64,
    next_seg_id: u64,
    /// Total quarantined bytes at open() time; reported so callers can raise coverage_gap.
    quarantined_at_open: u64,
}

struct Active {
    id: u64,
    file: File,
    path: PathBuf,
    written_bytes: u64,
    record_count: u64,
}

impl SegmentLog {
    /// Open (or create) the log at `config.root`. Handles crash recovery.
    pub fn open(config: Config) -> Result<Self> {
        std::fs::create_dir_all(&config.root).map_err(|_| StorageError::RootUnavailable {
            path: config.root.clone(),
        })?;
        let mut quarantined_at_open: u64 = 0;
        // Recovery: any `.wip` is a crashed-mid-write segment. OPS-16: quarantine, do not truncate.
        for entry in read_dir(&config.root)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_none() {
                continue;
            }
            if path.to_string_lossy().ends_with(&format!(".{WIP_EXT}")) {
                let size = std::fs::metadata(&path)?.len();
                let new_path = path.with_extension(QUARANTINE_EXT);
                std::fs::rename(&path, &new_path)?;
                quarantined_at_open += size;
            }
        }

        // Determine next segment id from the highest closed or quarantined segment.
        let mut highest_id: Option<u64> = None;
        let mut total_bytes: u64 = 0;
        for entry in read_dir(&config.root)? {
            let entry = entry?;
            let path = entry.path();
            let s = path.to_string_lossy();
            if s.ends_with(&format!(".{SEG_EXT}")) || s.ends_with(&format!(".{QUARANTINE_EXT}")) {
                total_bytes += std::fs::metadata(&path)?.len();
                if let Some(id) = parse_segment_id(&path) {
                    highest_id = Some(highest_id.map_or(id, |h| h.max(id)));
                }
            }
        }
        let next_seg_id = highest_id.map_or(0, |h| h + 1);

        Ok(Self {
            config,
            active: None,
            total_bytes,
            next_seg_id,
            quarantined_at_open,
        })
    }

    /// Bytes quarantined by the most recent recovery. Callers should log a coverage_gap.
    pub fn quarantined_bytes_at_open(&self) -> u64 {
        self.quarantined_at_open
    }

    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    pub fn quota(&self) -> Quota {
        self.config.quota
    }

    /// Append `data` as one record. Rotates automatically when the active segment
    /// exceeds `max_segment_bytes` OR when the record wouldn't fit and quota allows a new one.
    pub fn append(&mut self, data: &[u8]) -> Result<AppendReceipt> {
        if data.len() > self.config.max_record_bytes {
            return Err(StorageError::RecordTooLarge {
                size: data.len(),
                max: self.config.max_record_bytes,
            });
        }
        let record_size_on_disk = (RECORD_HEADER_BYTES + data.len()) as u64;

        // Quota check: what would we occupy after this write?
        match self.config.quota.check(self.total_bytes, record_size_on_disk) {
            Fits::Yes => {}
            Fits::No { .. } => {
                return Err(StorageError::QuotaExceeded {
                    total: self.total_bytes,
                    record: record_size_on_disk,
                    limit: self.config.quota.limit_bytes,
                });
            }
        }

        // Rotate BEFORE writing if the active segment would overflow.
        let must_rotate = match &self.active {
            None => true,
            Some(a) => a.written_bytes + record_size_on_disk > self.config.max_segment_bytes,
        };
        if must_rotate {
            self.rotate()?;
        }

        let active = self.active.as_mut().ok_or(StorageError::NoActiveSegment)?;

        let offset_in_segment = active.written_bytes;
        let record_index = active.record_count;

        // Write: [len:u32-BE][data]
        let len = data.len() as u32;
        active.file.write_all(&len.to_be_bytes())?;
        active.file.write_all(data)?;
        // Don't fsync per-record for V0 performance (PERF-1/2); flush at segment rotate.
        active.written_bytes += record_size_on_disk;
        active.record_count += 1;
        self.total_bytes += record_size_on_disk;

        Ok(AppendReceipt {
            segment_id: active.id,
            record_index,
            offset_in_segment,
            bytes_written: record_size_on_disk,
        })
    }

    /// Close the active segment (if any) and open a fresh one. Returns the path of the
    /// segment that was just closed, if any.
    pub fn rotate(&mut self) -> Result<Option<PathBuf>> {
        let closed_path = if let Some(mut a) = self.active.take() {
            a.file.sync_all()?;
            drop(a.file);
            // Rename .wip -> .seg (atomic on POSIX; on Windows this can fail if a handle is open,
            // but we dropped the handle above).
            let closed = a.path.with_extension(SEG_EXT);
            std::fs::rename(&a.path, &closed)?;
            Some(closed)
        } else {
            None
        };
        // Open a new active segment.
        let id = self.next_seg_id;
        self.next_seg_id += 1;
        let path = self.config.root.join(format!("{:016x}.{WIP_EXT}", id));
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        self.active = Some(Active {
            id,
            file,
            path,
            written_bytes: 0,
            record_count: 0,
        });
        Ok(closed_path)
    }

    /// Force fsync of the active segment without rotating.
    pub fn sync(&mut self) -> Result<()> {
        if let Some(a) = self.active.as_mut() {
            a.file.sync_all()?;
        }
        Ok(())
    }

    /// Close the active segment cleanly. Call on shutdown.
    pub fn close(mut self) -> Result<()> {
        if self.active.is_some() {
            self.rotate()?;
            // Discard the freshly-opened empty segment.
            if let Some(a) = self.active.take() {
                drop(a.file);
                // If nothing was written, delete the empty .wip file.
                if a.written_bytes == 0 {
                    let _ = std::fs::remove_file(&a.path);
                }
            }
        }
        Ok(())
    }

    /// List all segments in the log, sorted by id ascending.
    pub fn segments(&self) -> Result<Vec<SegmentInfo>> {
        let mut out = Vec::new();
        for entry in read_dir(&self.config.root)? {
            let entry = entry?;
            let path = entry.path();
            let s = path.to_string_lossy().to_string();
            let (state, ok_ext) = if s.ends_with(&format!(".{SEG_EXT}")) {
                (SegmentState::Closed, true)
            } else if s.ends_with(&format!(".{WIP_EXT}")) {
                (SegmentState::Active, true)
            } else if s.ends_with(&format!(".{QUARANTINE_EXT}")) {
                (SegmentState::Quarantined, true)
            } else {
                (SegmentState::Closed, false)
            };
            if !ok_ext {
                continue;
            }
            let Some(id) = parse_segment_id(&path) else { continue };
            let meta = std::fs::metadata(&path)?;
            let size_bytes = meta.len();
            let record_count = if state == SegmentState::Closed {
                count_records_best_effort(&path).unwrap_or(0)
            } else {
                self.active.as_ref().filter(|a| a.id == id).map_or(0, |a| a.record_count)
            };
            out.push(SegmentInfo {
                id,
                path,
                size_bytes,
                record_count,
                state,
            });
        }
        out.sort_by_key(|s| s.id);
        Ok(out)
    }

    /// Delete the oldest closed segment. Used by the eviction ladder (§4.5).
    /// Returns the id of the segment deleted, or None if there are no closed segments.
    pub fn evict_oldest_closed(&mut self) -> Result<Option<u64>> {
        let segs = self.segments()?;
        let oldest = segs.into_iter().find(|s| s.state == SegmentState::Closed);
        let Some(target) = oldest else { return Ok(None) };
        std::fs::remove_file(&target.path)?;
        self.total_bytes = self.total_bytes.saturating_sub(target.size_bytes);
        Ok(Some(target.id))
    }

    /// Read all records from a closed segment. For test and audit-chain verification;
    /// production reads stream via `SegmentReader` (not yet implemented).
    pub fn read_segment_records(path: &Path) -> Result<Vec<Vec<u8>>> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut out = Vec::new();
        let mut offset: u64 = 0;
        loop {
            let mut header = [0u8; RECORD_HEADER_BYTES];
            match reader.read_exact(&mut header) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e.into()),
            }
            let len = u32::from_be_bytes(header) as usize;
            let mut buf = vec![0u8; len];
            reader.read_exact(&mut buf).map_err(|_| StorageError::SegmentCorrupt {
                path: path.to_path_buf(),
                offset,
                reason: "truncated record body",
            })?;
            out.push(buf);
            offset += RECORD_HEADER_BYTES as u64 + len as u64;
        }
        Ok(out)
    }
}

fn parse_segment_id(path: &Path) -> Option<u64> {
    let name = path.file_name()?.to_str()?;
    // Take up to the first '.'.
    let stem = name.split_once('.').map_or(name, |(a, _)| a);
    u64::from_str_radix(stem, 16).ok()
}

fn count_records_best_effort(path: &Path) -> Result<u64> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut count: u64 = 0;
    loop {
        let mut header = [0u8; RECORD_HEADER_BYTES];
        match reader.read_exact(&mut header) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(_) => break,
        }
        let len = u32::from_be_bytes(header) as usize;
        if let Err(_) = std::io::Read::read_exact(&mut reader, &mut vec![0u8; len]) {
            break;
        }
        count += 1;
    }
    Ok(count)
}

fn read_dir(root: &Path) -> Result<ReadDir> {
    Ok(std::fs::read_dir(root)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn tmp_root(tag: &str) -> PathBuf {
        let n = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let pid = std::process::id();
        let mut p = std::env::temp_dir();
        p.push(format!("athar-storage-test-{pid}-{n}-{tag}"));
        p
    }

    fn cleanup(p: &Path) {
        let _ = std::fs::remove_dir_all(p);
    }

    #[test]
    fn append_and_read_back() {
        let root = tmp_root("basic");
        let cfg = Config::defaults(&root);
        let mut log = SegmentLog::open(cfg).expect("open");
        let r1 = log.append(b"hello").expect("append 1");
        let r2 = log.append(b"world").expect("append 2");
        assert_eq!(r1.segment_id, r2.segment_id);
        assert_eq!(r1.record_index, 0);
        assert_eq!(r2.record_index, 1);
        let path = log.rotate().unwrap().unwrap();
        log.close().expect("close");
        let records = SegmentLog::read_segment_records(&path).expect("read back");
        assert_eq!(records, vec![b"hello".to_vec(), b"world".to_vec()]);
        cleanup(&root);
    }

    #[test]
    fn rotates_when_segment_full() {
        let root = tmp_root("rotate");
        let mut cfg = Config::defaults(&root);
        cfg.max_segment_bytes = 32;
        let mut log = SegmentLog::open(cfg).expect("open");
        let r1 = log.append(b"1234567890").expect("a1"); // 14 bytes on disk
        let r2 = log.append(b"1234567890").expect("a2"); // 14 more
        let r3 = log.append(b"1234567890").expect("a3"); // triggers rotate
        assert_eq!(r1.segment_id, r2.segment_id);
        assert_ne!(r2.segment_id, r3.segment_id);
        log.close().expect("close");
        cleanup(&root);
    }

    #[test]
    fn quota_rejects_oversized_total() {
        let root = tmp_root("quota");
        let mut cfg = Config::defaults(&root);
        cfg.quota = Quota::new(20); // very small
        let mut log = SegmentLog::open(cfg).expect("open");
        log.append(b"1234567890").expect("first fits (14 bytes)");
        let err = log.append(b"1234567890").expect_err("should exceed");
        assert!(matches!(err, StorageError::QuotaExceeded { .. }));
        cleanup(&root);
    }

    #[test]
    fn record_too_large_rejected() {
        let root = tmp_root("toolarge");
        let mut cfg = Config::defaults(&root);
        cfg.max_record_bytes = 4;
        let mut log = SegmentLog::open(cfg).expect("open");
        let err = log.append(b"12345").expect_err("too large");
        assert!(matches!(err, StorageError::RecordTooLarge { size: 5, max: 4 }));
        cleanup(&root);
    }

    #[test]
    fn crash_recovery_quarantines_wip() {
        // Simulate a crashed segment by placing a `.wip` file in the root, then re-opening.
        let root = tmp_root("crash");
        std::fs::create_dir_all(&root).unwrap();
        let wip = root.join(format!("{:016x}.{WIP_EXT}", 0));
        std::fs::write(&wip, b"some bytes").unwrap();

        let log = SegmentLog::open(Config::defaults(&root)).expect("open");
        assert!(log.quarantined_bytes_at_open() > 0);
        let segs = log.segments().expect("list");
        assert!(segs.iter().any(|s| s.state == SegmentState::Quarantined));
        assert!(!segs.iter().any(|s| s.state == SegmentState::Active));
        cleanup(&root);
    }

    #[test]
    fn evict_oldest_closed() {
        let root = tmp_root("evict");
        let mut cfg = Config::defaults(&root);
        cfg.max_segment_bytes = 32;
        let mut log = SegmentLog::open(cfg).expect("open");
        log.append(b"aaaaaaaaaa").expect("a"); // 14 bytes
        log.rotate().expect("rot 1");           // closes seg 0
        log.append(b"bbbbbbbbbb").expect("b");
        log.rotate().expect("rot 2");           // closes seg 1
        log.append(b"cccccccccc").expect("c");
        // three segments now (0, 1 closed; 2 active)
        assert_eq!(log.segments().unwrap().len(), 3);
        let evicted = log.evict_oldest_closed().expect("evict").expect("some");
        assert_eq!(evicted, 0);
        // Two segments left.
        let remaining_closed: Vec<u64> = log
            .segments()
            .unwrap()
            .into_iter()
            .filter(|s| s.state == SegmentState::Closed)
            .map(|s| s.id)
            .collect();
        assert_eq!(remaining_closed, vec![1]);
        cleanup(&root);
    }

    #[test]
    fn segment_id_monotonic_across_reopens() {
        let root = tmp_root("reopen");
        {
            let mut log = SegmentLog::open(Config::defaults(&root)).expect("open 1");
            log.append(b"first").unwrap();
            log.rotate().unwrap();
            log.close().unwrap();
        }
        {
            let mut log = SegmentLog::open(Config::defaults(&root)).expect("open 2");
            let r = log.append(b"second").unwrap();
            // First reopen after 1 closed segment should hand out id 1 (or higher).
            assert!(r.segment_id >= 1);
            log.close().unwrap();
        }
        cleanup(&root);
    }
}
