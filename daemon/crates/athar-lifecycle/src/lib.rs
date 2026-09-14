//! Lifecycle correlation, state, and closure (SPEC §5.11, MOD-24..MOD-28).
//!
//! - `LifecycleStore` is a trait; two implementations ship:
//!   - `InMemoryStore` for tests / ephemeral use.
//!   - `SqliteLifecycleStore` for production (WAL, indexed, restart-durable).
//! - `LifecycleEngine::apply` correlates incoming events (tiers 1-4) and drives
//!   the state machine for the matched lifecycle type.
//! - `StalenessScanner::sweep` closes stale lifecycles as `CLOSED_WITH_UNCERTAINTY`.

#![deny(unsafe_code)]

pub mod engine;
pub mod scanner;
pub mod sqlite_store;
pub mod store;
pub mod types;

pub use engine::{ApplyOutcome, LifecycleEngine};
pub use scanner::{StalenessResult, StalenessScanner};
pub use sqlite_store::{SqliteLifecycleStore, StoreError};
pub use store::{InMemoryStore, LifecycleStore};
pub use types::*;
