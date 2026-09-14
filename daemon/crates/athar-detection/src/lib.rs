//! Detection, policy, and explainable decision records (SPEC §9, INV-16, SEC-16..SEC-25).
//!
//! V0 scope (deliberately narrow — SPEC §12.2):
//! - Two signal types: `new_beneficiary`, `high_amount`.
//! - One hardcoded policy in OBSERVE mode (D11's CEL evaluator is deferred).
//! - Full `DecisionRecord` per `INV-16`, persisted to the state store.
//!
//! The signal engine and decision writer are separate from the daemon so the
//! same code can be exercised in unit tests without spinning up the full stack.

#![deny(unsafe_code)]

pub mod config;
pub mod decision;
pub mod policy;
pub mod signal;
pub mod store;
pub mod trackers;

pub use config::{DetectionConfig, PolicyConfig, PolicyRule};

pub use signal::{Signal, SignalEngine, SignalKind, SignalRecord};
pub use policy::{Action, PolicyDecision, PolicyEngine, PolicyMode};
pub use decision::DecisionRecord;
pub use store::{DecisionStore, DecisionStoreError, SqliteDecisionStore};
