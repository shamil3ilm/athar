//! Signer implementations for D5.
//!
//! - `SoftwareSigner` (in `lib.rs`) — in-memory seed, for tests only.
//! - `FileKeySigner` — dev-mode signer backed by a 32-byte seed on disk.
//!   Emits a critical warning on generation.
//!
//! T1/T2 production: replace with an OS-keystore-backed implementation of the
//! `Signer` trait. T3/T4: PKCS#11.
//!
//! The daemon refuses to run with a `FileKeySigner` when the environment is
//! marked "production" — that check lives in the daemon binary, not here.

use std::path::Path;

use tracing::warn;

use crate::{Signer, SoftwareSigner};

pub struct FileKeySigner {
    inner: SoftwareSigner,
}

#[derive(Debug, thiserror::Error)]
pub enum FileKeyError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("key file must contain exactly 32 bytes")]
    BadLength,
}

impl FileKeySigner {
    /// Load a 32-byte seed from `path`, or if absent, generate a fresh one and
    /// write it (dev mode only). Logs a warning on generation.
    pub fn load_or_create_dev(path: &Path) -> Result<Self, FileKeyError> {
        let seed = if path.exists() {
            let bytes = std::fs::read(path)?;
            if bytes.len() != 32 {
                return Err(FileKeyError::BadLength);
            }
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes);
            arr
        } else {
            let s = fresh_seed();
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, s)?;
            warn!(
                path = %path.display(),
                "DEV MODE: generated a file-backed audit-chain signing key. \
                 D5 requires OS keystore / PKCS#11 for production; \
                 do NOT use this configuration outside of development."
            );
            s
        };
        Ok(Self { inner: SoftwareSigner::from_seed(seed) })
    }
}

impl Signer for FileKeySigner {
    fn public_key(&self) -> [u8; 32] { self.inner.public_key() }
    fn sign(&self, msg: &[u8]) -> [u8; 64] { self.inner.sign(msg) }
}

/// Best-effort entropy from time + PID + a stack address. Adequate for a dev-mode
/// throwaway key. Real deployments load from an OS keystore or HSM instead.
fn fresh_seed() -> [u8; 32] {
    use std::time::SystemTime;
    let mut hasher = blake3::Hasher::new();
    let t = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    hasher.update(&t.as_nanos().to_le_bytes());
    hasher.update(&std::process::id().to_le_bytes());
    let stack_addr: usize = &t as *const _ as usize;
    hasher.update(&stack_addr.to_le_bytes());
    // Extra entropy: hash the address of the hasher itself for a second stack address.
    let h_addr: usize = &hasher as *const _ as usize;
    hasher.update(&h_addr.to_le_bytes());
    *hasher.finalize().as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_key_when_absent_and_reads_it_back() {
        let tmp = std::env::temp_dir().join(format!(
            "athar-audit-signer-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let key_path = tmp.join("segment.key");
        let s1 = FileKeySigner::load_or_create_dev(&key_path).expect("create");
        let pk1 = s1.public_key();

        let s2 = FileKeySigner::load_or_create_dev(&key_path).expect("reload");
        assert_eq!(pk1, s2.public_key(), "reload MUST return the same key");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn rejects_wrong_size_key() {
        let tmp = std::env::temp_dir().join(format!(
            "athar-audit-signer-bad-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let key_path = tmp.join("segment.key");
        std::fs::write(&key_path, b"too short").unwrap();
        assert!(matches!(
            FileKeySigner::load_or_create_dev(&key_path),
            Err(FileKeyError::BadLength)
        ));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
