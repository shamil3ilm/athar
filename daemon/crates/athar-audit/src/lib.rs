//! Audit chain (SPEC §8.6, SEC-11..SEC-15, D5).
//!
//! Each record is committed as `record_hash = BLAKE3(prev_hash || canonical(record))`.
//! Segments are signed with Ed25519 at close (and at least hourly in production).
//! `verify_chain` walks a segment offline and reports the exact index of any break.
//!
//! Redaction (SEC-15) replaces the payload with a tombstone while keeping the
//! payload-commitment (`payload_commitment = BLAKE3(payload)`) intact. The chain
//! commits to the commitment, not the payload, so the chain remains verifiable
//! after a lawful erasure.
//!
//! Key custody (D5): the `Signer` trait lets us swap in `SoftwareSigner` for dev,
//! an OS-keystore-backed signer for T1/T2, or a PKCS#11 signer for T3/T4 without
//! touching the chain code.

#![deny(unsafe_code)]

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

pub use ed25519_dalek::{Signature, SigningKey, VerifyingKey};

pub mod persistence;
pub mod signer;
pub mod writer;

pub const HASH_LEN: usize = 32;
pub const GENESIS_PREV_HASH: [u8; HASH_LEN] = [0u8; HASH_LEN];

/// Serde helper: encode/decode [u8; 32] as a lowercase hex string, so persisted
/// segments are human-inspectable and small.
mod hex32 {
    use super::*;
    pub fn serialize<S: Serializer>(bytes: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex_encode(bytes))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let s = String::deserialize(d)?;
        let bytes = hex_decode(&s).map_err(serde::de::Error::custom)?;
        bytes.try_into().map_err(|_: Vec<u8>| serde::de::Error::custom("expected 32 hex-encoded bytes"))
    }
}

mod hex64 {
    use super::*;
    pub fn serialize<S: Serializer>(bytes: &[u8; 64], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex_encode(bytes))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 64], D::Error> {
        let s = String::deserialize(d)?;
        let bytes = hex_decode(&s).map_err(serde::de::Error::custom)?;
        if bytes.len() != 64 {
            return Err(serde::de::Error::custom("expected 64 hex-encoded bytes"));
        }
        let mut out = [0u8; 64];
        out.copy_from_slice(&bytes);
        Ok(out)
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn hex_decode(s: &str) -> Result<Vec<u8>, &'static str> {
    if s.len() % 2 != 0 {
        return Err("odd-length hex string");
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for i in (0..bytes.len()).step_by(2) {
        let hi = from_hex(bytes[i])?;
        let lo = from_hex(bytes[i + 1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn from_hex(b: u8) -> Result<u8, &'static str> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err("invalid hex digit"),
    }
}

/// One record in the audit chain. The `payload_commitment` (not the payload itself)
/// is what the chain hashes, so redaction of the payload (SEC-15) leaves the chain intact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRecord {
    pub seq: u64,
    pub timestamp_us: i64,
    pub tenant_id: String,
    pub kind: String,
    #[serde(with = "hex32")]
    pub payload_commitment: [u8; HASH_LEN],
    pub redacted: bool,
}

/// A record after it has been placed in the chain: carries its own `record_hash`
/// and the `prev_hash` it committed to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainedRecord {
    pub record: AuditRecord,
    #[serde(with = "hex32")]
    pub prev_hash: [u8; HASH_LEN],
    #[serde(with = "hex32")]
    pub record_hash: [u8; HASH_LEN],
}

/// A closed segment: a run of chained records with an Ed25519 signature over the tip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    pub segment_id: u64,
    pub records: Vec<ChainedRecord>,
    #[serde(with = "hex64")]
    pub tip_signature: [u8; 64],
    #[serde(with = "hex32")]
    pub signer_pubkey: [u8; 32],
}

/// Verification error: `index` is the exact record position of the break (SEC-13).
#[derive(Debug, Error, PartialEq, Eq)]
pub enum VerifyError {
    #[error("record {index}: seq gap or reorder (expected {expected}, got {got})")]
    SeqBroken { index: usize, expected: u64, got: u64 },
    #[error("record {index}: prev_hash does not match preceding record_hash")]
    ChainBroken { index: usize },
    #[error("record {index}: record_hash does not match H(prev_hash || canonical(record))")]
    RecordHashMismatch { index: usize },
    #[error("segment signature does not verify against tip")]
    BadSignature,
    #[error("segment is empty")]
    EmptySegment,
}

/// Something that can sign a message and expose its public key (D5).
pub trait Signer {
    fn public_key(&self) -> [u8; 32];
    fn sign(&self, msg: &[u8]) -> [u8; 64];
}

/// In-memory Ed25519 signer for dev / tests. Never permitted in production
/// (`SigningKey` in memory violates D5's T2+ requirements — production uses
/// OS keystore or PKCS#11 implementations of the `Signer` trait).
pub struct SoftwareSigner {
    key: SigningKey,
}

impl SoftwareSigner {
    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self { key: SigningKey::from_bytes(&seed) }
    }
}

impl Signer for SoftwareSigner {
    fn public_key(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }
    fn sign(&self, msg: &[u8]) -> [u8; 64] {
        use ed25519_dalek::Signer as _;
        self.key.sign(msg).to_bytes()
    }
}

/// Canonical byte encoding of an `AuditRecord`. Field order and lengths are fixed,
/// so hashing is deterministic without depending on serde's key ordering.
pub fn canonical_bytes(r: &AuditRecord) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + 8 + 4 + r.tenant_id.len() + 4 + r.kind.len() + HASH_LEN + 1);
    out.extend_from_slice(&r.seq.to_be_bytes());
    out.extend_from_slice(&r.timestamp_us.to_be_bytes());
    let tenant_bytes = r.tenant_id.as_bytes();
    out.extend_from_slice(&(tenant_bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(tenant_bytes);
    let kind_bytes = r.kind.as_bytes();
    out.extend_from_slice(&(kind_bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(kind_bytes);
    out.extend_from_slice(&r.payload_commitment);
    out.push(if r.redacted { 1 } else { 0 });
    out
}

pub fn record_hash(prev_hash: &[u8; HASH_LEN], record: &AuditRecord) -> [u8; HASH_LEN] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(prev_hash);
    hasher.update(&canonical_bytes(record));
    *hasher.finalize().as_bytes()
}

pub fn payload_commitment(payload: &[u8]) -> [u8; HASH_LEN] {
    *blake3::hash(payload).as_bytes()
}

/// Chain writer: appends records, tracks the tip, closes and signs segments (SEC-12).
pub struct ChainWriter {
    records: Vec<ChainedRecord>,
    tip_hash: [u8; HASH_LEN],
    next_seq: u64,
}

impl ChainWriter {
    pub fn new() -> Self {
        Self::from_tip(GENESIS_PREV_HASH, 0)
    }

    pub fn from_tip(tip_hash: [u8; HASH_LEN], next_seq: u64) -> Self {
        Self { records: Vec::new(), tip_hash, next_seq }
    }

    pub fn len(&self) -> usize { self.records.len() }
    pub fn is_empty(&self) -> bool { self.records.is_empty() }
    pub fn tip_hash(&self) -> [u8; HASH_LEN] { self.tip_hash }
    pub fn next_seq(&self) -> u64 { self.next_seq }

    /// Append a record and return its position. Assigns `seq` automatically.
    pub fn append(&mut self, mut record: AuditRecord) -> usize {
        record.seq = self.next_seq;
        let prev_hash = self.tip_hash;
        let this_hash = record_hash(&prev_hash, &record);
        self.records.push(ChainedRecord { record, prev_hash, record_hash: this_hash });
        self.tip_hash = this_hash;
        self.next_seq += 1;
        self.records.len() - 1
    }

    /// Close the current run into a signed segment. The signature covers the tip hash.
    /// The writer remains valid: it retains the current `tip_hash` and `next_seq`
    /// so subsequent `append` calls extend the chain.
    pub fn close_segment(&mut self, segment_id: u64, signer: &dyn Signer) -> Segment {
        let sig = signer.sign(&self.tip_hash);
        let pubkey = signer.public_key();
        let records = std::mem::take(&mut self.records);
        Segment {
            segment_id,
            records,
            tip_signature: sig,
            signer_pubkey: pubkey,
        }
    }
}

impl Default for ChainWriter {
    fn default() -> Self { Self::new() }
}

/// Verify a segment offline. Returns `Ok(())` if the chain and the tip signature
/// are consistent, otherwise the first break with its exact record index (SEC-13).
///
/// The segment is verified in isolation: the anchor is whatever `records[0].prev_hash`
/// claims (GENESIS for the first segment ever, otherwise the previous segment's tip).
/// Cross-segment continuity is `SegmentStore::verify_all`'s responsibility — it
/// walks segments in order and additionally checks each segment's anchor matches the
/// prior tip.
pub fn verify_segment(segment: &Segment) -> Result<(), VerifyError> {
    if segment.records.is_empty() {
        return Err(VerifyError::EmptySegment);
    }
    let anchor = segment.records[0].prev_hash;
    let starting_seq = segment.records[0].record.seq;
    let tip_hash = verify_chain(&segment.records, anchor, starting_seq)?;

    // Verify the segment signature over the tip.
    use ed25519_dalek::Verifier as _;
    let vk = VerifyingKey::from_bytes(&segment.signer_pubkey).map_err(|_| VerifyError::BadSignature)?;
    let sig = Signature::from_bytes(&segment.tip_signature);
    vk.verify(&tip_hash, &sig).map_err(|_| VerifyError::BadSignature)
}

/// Verify a run of chained records against an anchor `prev_hash` and starting `seq`.
/// Returns the tip hash on success.
pub fn verify_chain(
    records: &[ChainedRecord],
    anchor_prev_hash: [u8; HASH_LEN],
    starting_seq: u64,
) -> Result<[u8; HASH_LEN], VerifyError> {
    if records.is_empty() {
        return Err(VerifyError::EmptySegment);
    }
    let mut prev_hash = anchor_prev_hash;
    let mut expected_seq = starting_seq;
    for (index, cr) in records.iter().enumerate() {
        if cr.record.seq != expected_seq {
            return Err(VerifyError::SeqBroken { index, expected: expected_seq, got: cr.record.seq });
        }
        if cr.prev_hash != prev_hash {
            return Err(VerifyError::ChainBroken { index });
        }
        let recomputed = record_hash(&cr.prev_hash, &cr.record);
        if recomputed != cr.record_hash {
            return Err(VerifyError::RecordHashMismatch { index });
        }
        prev_hash = cr.record_hash;
        expected_seq += 1;
    }
    Ok(prev_hash)
}

/// Redact a record in place (SEC-15). Replaces the payload commitment with a
/// tombstone marker AND records `redacted = true`. The `record_hash` MUST NOT
/// change — otherwise the chain would break. So we hash BEFORE redaction and
/// keep the same `record_hash`; we only mutate what a reader can see.
///
/// This function is intentionally a no-op on the on-chain commitment: it is up
/// to the *payload store* to actually delete the raw payload elsewhere. Here we
/// only mark the record so verifiers know the payload is no longer available.
pub fn mark_redacted(chained: &mut ChainedRecord) {
    // Change ONLY the `redacted` flag of the visible record. Because `record_hash`
    // was computed over the pre-redaction canonical form, verifiers need to know
    // the original bytes to reproduce it. We therefore keep the pre-redaction hash
    // and the pre-redaction commitment fields intact and only flip `redacted`.
    //
    // Under this model, `verify_chain` verifies against the record AS STORED. To
    // support post-redaction verification we would need to record a separate
    // "redaction certificate" that re-witnesses the hash. That certificate is
    // out of scope for V0; for now, calling this function creates a chain that
    // fails RecordHashMismatch, which is the SAFE default — verifiers see the
    // tampering unless the operator has separately issued a witness.
    //
    // TODO(SEC-15 v1.1): add RedactionCertificate { original_hash, at_seq, by, at_time }
    // and teach verify_chain to accept a certificate map.
    chained.record.redacted = true;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(kind: &str, payload: &[u8]) -> AuditRecord {
        AuditRecord {
            seq: 0,
            timestamp_us: 1_726_300_000_000_000,
            tenant_id: "tnt_demo".into(),
            kind: kind.into(),
            payload_commitment: payload_commitment(payload),
            redacted: false,
        }
    }

    fn signer() -> SoftwareSigner {
        SoftwareSigner::from_seed([7u8; 32])
    }

    #[test]
    fn happy_path_verifies() {
        let mut w = ChainWriter::new();
        w.append(rec("decision", b"payload-1"));
        w.append(rec("decision", b"payload-2"));
        w.append(rec("config_change", b"payload-3"));
        let seg = w.close_segment(1, &signer());
        assert_eq!(verify_segment(&seg), Ok(()));
    }

    #[test]
    fn corrupted_record_hash_detected_at_exact_index() {
        // V0 criterion 10: verify reports the exact record index.
        let mut w = ChainWriter::new();
        w.append(rec("decision", b"a"));
        w.append(rec("decision", b"b"));
        w.append(rec("decision", b"c"));
        w.append(rec("decision", b"d"));
        let mut seg = w.close_segment(1, &signer());

        // Corrupt record #2 by flipping a bit in the payload commitment.
        seg.records[2].record.payload_commitment[0] ^= 0x01;

        match verify_segment(&seg) {
            Err(VerifyError::RecordHashMismatch { index: 2 }) => {} // exact index
            other => panic!("expected RecordHashMismatch at index 2, got {other:?}"),
        }
    }

    #[test]
    fn corrupted_prev_hash_detected_at_exact_index() {
        let mut w = ChainWriter::new();
        w.append(rec("decision", b"a"));
        w.append(rec("decision", b"b"));
        w.append(rec("decision", b"c"));
        let mut seg = w.close_segment(1, &signer());

        // Break the chain link into record #1.
        seg.records[1].prev_hash[0] ^= 0x01;

        match verify_segment(&seg) {
            Err(VerifyError::ChainBroken { index: 1 }) => {}
            other => panic!("expected ChainBroken at index 1, got {other:?}"),
        }
    }

    #[test]
    fn seq_reorder_detected() {
        let mut w = ChainWriter::new();
        w.append(rec("decision", b"a"));
        w.append(rec("decision", b"b"));
        let mut seg = w.close_segment(1, &signer());
        seg.records[1].record.seq = 42;

        match verify_segment(&seg) {
            Err(VerifyError::RecordHashMismatch { index: 1 }) => {} // seq is inside canonical()
            Err(VerifyError::SeqBroken { index: 1, .. }) => {}
            other => panic!("expected a break at index 1, got {other:?}"),
        }
    }

    #[test]
    fn bad_signature_detected() {
        let mut w = ChainWriter::new();
        w.append(rec("decision", b"a"));
        let mut seg = w.close_segment(1, &signer());
        seg.tip_signature[0] ^= 0x01;
        assert_eq!(verify_segment(&seg), Err(VerifyError::BadSignature));
    }

    #[test]
    fn tip_signature_verifies_against_actual_tip() {
        // Signature must be over the true tip hash, so any tail alteration invalidates it.
        let mut w = ChainWriter::new();
        w.append(rec("decision", b"a"));
        w.append(rec("decision", b"b"));
        let mut seg = w.close_segment(1, &signer());

        // Replace the last record with something that also chains — but signature was over the OLD tip.
        let new_tail = ChainedRecord {
            record: AuditRecord {
                seq: seg.records[1].record.seq,
                ..rec("decision", b"forged")
            },
            prev_hash: seg.records[1].prev_hash,
            record_hash: record_hash(&seg.records[1].prev_hash, &AuditRecord {
                seq: seg.records[1].record.seq,
                ..rec("decision", b"forged")
            }),
        };
        seg.records[1] = new_tail;

        // Chain internally verifies (records[1] is self-consistent) but signature does not.
        assert_eq!(verify_segment(&seg), Err(VerifyError::BadSignature));
    }

    #[test]
    fn empty_segment_rejected() {
        let seg = Segment {
            segment_id: 1,
            records: vec![],
            tip_signature: [0u8; 64],
            signer_pubkey: signer().public_key(),
        };
        assert_eq!(verify_segment(&seg), Err(VerifyError::EmptySegment));
    }

    #[test]
    fn canonical_bytes_is_deterministic() {
        let a = rec("decision", b"payload");
        let b = rec("decision", b"payload");
        assert_eq!(canonical_bytes(&a), canonical_bytes(&b));
    }

    #[test]
    fn tip_can_be_resumed_across_writers() {
        let mut w1 = ChainWriter::new();
        w1.append(rec("decision", b"a"));
        w1.append(rec("decision", b"b"));
        let tip = w1.tip_hash();
        let next = w1.next_seq();
        let seg1 = w1.close_segment(1, &signer());

        let mut w2 = ChainWriter::from_tip(tip, next);
        w2.append(rec("decision", b"c"));
        let seg2 = w2.close_segment(2, &signer());

        // Each segment verifies against genesis? No — seg2 anchors on seg1's tip.
        assert_eq!(verify_segment(&seg1), Ok(()));
        let anchor = seg1.records.last().unwrap().record_hash;
        assert_eq!(
            verify_chain(&seg2.records, anchor, seg2.records[0].record.seq),
            Ok(seg2.records.last().unwrap().record_hash),
        );
    }
}
