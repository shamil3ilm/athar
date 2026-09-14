//! Signal trackers (SPEC §9.1, SEC-18).
//!
//! Bounded, in-memory state used by the signal engine to detect rate- and
//! cardinality-based patterns. Each tracker MUST have a documented capacity
//! bound and MUST prune / evict rather than growing without limit.
//!
//! For V0 we ship `VelocityTracker` (rate of events per subject in a rolling
//! window). `TargetTracker` (distinct beneficiaries per actor) is the natural
//! next addition — same shape, different aggregate.
//!
//! Real deployments will eventually swap the HashMap for an approximate
//! sketch (HyperLogLog for distincts, count-min for velocity) once cardinality
//! exceeds `max_subjects`. That's Stage 2 hardening; for V0 the exact tracker
//! with LRU eviction is enough.

use std::collections::{HashMap, VecDeque};

#[derive(Debug, Clone)]
pub struct VelocityConfig {
    /// Rolling window size in milliseconds. Default 60_000 (1 minute).
    pub window_ms: u64,
    /// Fire the signal when the count within window strictly exceeds this. Default 10.
    pub threshold: u32,
    /// Bound on tracked subjects. When full, the least-recently-active subject is
    /// evicted (SEC-18). Default 10_000.
    pub max_subjects: usize,
}

impl Default for VelocityConfig {
    fn default() -> Self {
        Self {
            window_ms: 60_000,
            threshold: 10,
            max_subjects: 10_000,
        }
    }
}

/// Rolling-window event-rate tracker. Given a stream of `(subject, timestamp)`
/// pairs, tells you when the count-in-window for any subject exceeds a threshold.
pub struct VelocityTracker {
    config: VelocityConfig,
    windows: HashMap<String, VecDeque<u64>>,
    /// LRU list. Most-recently-active at the back.
    lru: VecDeque<String>,
    at_capacity_reported: bool,
}

impl VelocityTracker {
    pub fn new(config: VelocityConfig) -> Self {
        Self {
            config,
            windows: HashMap::new(),
            lru: VecDeque::new(),
            at_capacity_reported: false,
        }
    }

    /// Record one observation for `subject` at `now_ms`. Returns the current
    /// count-in-window if it strictly exceeds `threshold`, else None.
    pub fn observe(&mut self, subject: &str, now_ms: u64) -> Option<u32> {
        let is_new_subject = !self.windows.contains_key(subject);

        // Evict oldest subject when adding a new one at capacity.
        if is_new_subject && self.windows.len() >= self.config.max_subjects {
            if let Some(evict) = self.lru.pop_front() {
                self.windows.remove(&evict);
            }
            if !self.at_capacity_reported {
                tracing::warn!(
                    max_subjects = self.config.max_subjects,
                    "VelocityTracker at capacity; evicting oldest subjects"
                );
                self.at_capacity_reported = true;
            }
        }

        let window = self.windows.entry(subject.to_string()).or_default();

        // Prune samples older than the rolling window.
        let cutoff = now_ms.saturating_sub(self.config.window_ms);
        while let Some(&front) = window.front() {
            if front < cutoff {
                window.pop_front();
            } else {
                break;
            }
        }

        window.push_back(now_ms);
        let count = window.len() as u32;

        // Bump this subject to the back of the LRU list.
        self.lru.retain(|s| s != subject);
        self.lru.push_back(subject.to_string());

        if count > self.config.threshold {
            Some(count)
        } else {
            None
        }
    }

    pub fn config(&self) -> &VelocityConfig { &self.config }
    pub fn subject_count(&self) -> usize { self.windows.len() }
    pub fn is_at_capacity(&self) -> bool { self.windows.len() >= self.config.max_subjects }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_threshold_returns_none() {
        let mut t = VelocityTracker::new(VelocityConfig { window_ms: 1000, threshold: 5, max_subjects: 100 });
        for i in 0..5 {
            assert_eq!(t.observe("actor_a", 100 + i), None);
        }
    }

    #[test]
    fn firing_when_count_exceeds_threshold() {
        let mut t = VelocityTracker::new(VelocityConfig { window_ms: 1000, threshold: 3, max_subjects: 100 });
        // Threshold is 3, so the 4th event within window should fire with count=4.
        assert_eq!(t.observe("a", 100), None);
        assert_eq!(t.observe("a", 200), None);
        assert_eq!(t.observe("a", 300), None);
        assert_eq!(t.observe("a", 400), Some(4));
    }

    #[test]
    fn old_samples_pruned_out_of_window() {
        let mut t = VelocityTracker::new(VelocityConfig { window_ms: 1000, threshold: 2, max_subjects: 100 });
        // At t=100, 200, 300: three samples. Count > 2 → fire at t=300 with count=3.
        assert_eq!(t.observe("a", 100), None);
        assert_eq!(t.observe("a", 200), None);
        assert_eq!(t.observe("a", 300), Some(3));
        // Now advance well past the window. Older samples get pruned; single new sample.
        assert_eq!(t.observe("a", 5_000), None);
    }

    #[test]
    fn subjects_are_independent() {
        let mut t = VelocityTracker::new(VelocityConfig { window_ms: 1000, threshold: 2, max_subjects: 100 });
        assert_eq!(t.observe("a", 100), None);
        assert_eq!(t.observe("b", 100), None);
        assert_eq!(t.observe("a", 200), None);
        assert_eq!(t.observe("b", 200), None);
        // Third observation for `a`: fires. `b` still below.
        assert_eq!(t.observe("a", 300), Some(3));
        assert_eq!(t.observe("b", 300), Some(3));
    }

    #[test]
    fn capacity_evicts_oldest_subject() {
        let mut t = VelocityTracker::new(VelocityConfig { window_ms: 60_000, threshold: 100, max_subjects: 3 });
        t.observe("a", 100);
        t.observe("b", 200);
        t.observe("c", 300);
        assert_eq!(t.subject_count(), 3);
        assert!(t.is_at_capacity());
        // Adding `d` should evict `a` (least-recently-touched).
        t.observe("d", 400);
        assert_eq!(t.subject_count(), 3);
        assert!(!t.windows.contains_key("a"));
        assert!(t.windows.contains_key("b"));
        assert!(t.windows.contains_key("c"));
        assert!(t.windows.contains_key("d"));
    }

    #[test]
    fn recent_activity_keeps_subject_alive() {
        let mut t = VelocityTracker::new(VelocityConfig { window_ms: 60_000, threshold: 100, max_subjects: 3 });
        t.observe("a", 100);
        t.observe("b", 200);
        t.observe("c", 300);
        // Touch `a` again — now `b` is the oldest.
        t.observe("a", 350);
        t.observe("d", 400);
        assert!(t.windows.contains_key("a"), "a should survive because it was touched most recently before d");
        assert!(!t.windows.contains_key("b"), "b should be evicted as it's now the LRU");
    }
}
