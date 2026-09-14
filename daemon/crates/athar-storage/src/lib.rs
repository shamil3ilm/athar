//! Storage layer (SPEC §6.5, D4).
//!
//! Two stores, deliberately split:
//! - `state_store` (SQLite in WAL mode): lifecycles, identities, endpoints, decisions, signals.
//!   Mutable-with-history, indexed. Not yet implemented; scaffolded in `state_store.rs`.
//! - `segment_log` (append-only, segmented): raw evidence + audit chain records.
//!   Immutable, hash-chained (via `athar-audit`), zstd-compressible, encryptable at rest.
//!
//! The segmented log has NO update or delete code path. INV-9 is structural, not policy:
//! the API simply does not expose a mutation on prior records. Retention deletes whole
//! *segments* and leaves a tombstone (see `retention.rs`, pending).
//!
//! OPS-10: no path in this crate connects to the customer's database.

#![deny(unsafe_code)]

pub mod segment_log;
pub mod quota;
pub mod error;

pub use error::{Result, StorageError};

pub const CRATE_VERSION: &str = env!("CARGO_PKG_VERSION");
