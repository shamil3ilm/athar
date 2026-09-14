//! `LifecycleStore` trait plus an in-memory implementation.
//!
//! Two implementations ship: `InMemoryStore` (tests, ephemeral use) and
//! `SqliteLifecycleStore` (production; see `sqlite_store.rs`). Both satisfy the
//! same trait so the engine, scanner, and ingest path treat them identically.
//!
//! Concurrency note (V0): the engine's read-mutate-upsert path is not atomic
//! across the store boundary. For V0 with single-threaded ingest this is safe.
//! When we go to multi-connection concurrent ingest, wrap the sequence in a
//! transaction (see TODO in `SqliteLifecycleStore`).

use std::collections::HashMap;
use std::sync::RwLock;

use crate::types::*;

pub trait LifecycleStore: Send + Sync {
    fn get(&self, id: &str) -> Option<Lifecycle>;
    fn find_by_resource(&self, r: &str) -> Option<String>;
    fn find_by_business_key(&self, k: &str) -> Option<String>;
    fn find_by_event(&self, event_id: &str) -> Option<String>;
    /// Insert or replace. All indexes (resource_id, business_key, event bindings)
    /// are refreshed from the lifecycle's current contents.
    fn upsert(&self, lifecycle: &Lifecycle);
    /// Bind an event to a lifecycle for tier-3 (causation) lookups.
    fn record_event(&self, event_id: &str, lifecycle_id: &str);
    fn all_open(&self) -> Vec<Lifecycle>;
    fn count(&self) -> usize;
    fn count_open(&self) -> usize;
}

#[derive(Default)]
pub struct InMemoryStore {
    lifecycles: RwLock<HashMap<String, Lifecycle>>,
    by_resource: RwLock<HashMap<String, String>>,
    by_business_key: RwLock<HashMap<String, String>>,
    by_event: RwLock<HashMap<String, String>>,
}

impl InMemoryStore {
    pub fn new() -> Self { Self::default() }
}

impl LifecycleStore for InMemoryStore {
    fn get(&self, id: &str) -> Option<Lifecycle> {
        self.lifecycles.read().expect("lifecycle lock").get(id).cloned()
    }

    fn find_by_resource(&self, r: &str) -> Option<String> {
        self.by_resource.read().expect("resource lock").get(r).cloned()
    }

    fn find_by_business_key(&self, k: &str) -> Option<String> {
        self.by_business_key.read().expect("business lock").get(k).cloned()
    }

    fn find_by_event(&self, event_id: &str) -> Option<String> {
        self.by_event.read().expect("event lock").get(event_id).cloned()
    }

    fn upsert(&self, lc: &Lifecycle) {
        let id = lc.id.clone();
        if let Some(r) = &lc.resource_id {
            self.by_resource.write().expect("resource lock").insert(r.clone(), id.clone());
        }
        if let Some(k) = &lc.business_key {
            self.by_business_key.write().expect("business lock").insert(k.clone(), id.clone());
        }
        for ev in &lc.event_ids {
            self.by_event.write().expect("event lock").insert(ev.clone(), id.clone());
        }
        self.lifecycles.write().expect("lifecycle lock").insert(id, lc.clone());
    }

    fn record_event(&self, event_id: &str, lifecycle_id: &str) {
        self.by_event.write().expect("event lock")
            .insert(event_id.to_string(), lifecycle_id.to_string());
    }

    fn all_open(&self) -> Vec<Lifecycle> {
        self.lifecycles.read().expect("lifecycle lock").values()
            .filter(|l| l.closure == Closure::Open)
            .cloned()
            .collect()
    }

    fn count(&self) -> usize {
        self.lifecycles.read().expect("lifecycle lock").len()
    }

    fn count_open(&self) -> usize {
        self.lifecycles.read().expect("lifecycle lock").values()
            .filter(|l| l.closure == Closure::Open)
            .count()
    }
}
