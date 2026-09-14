//! Disk-quota accounting (OPS-12, §4.5).
//!
//! The runtime enforces its own disk quota. Uncontrolled storage growth is itself
//! a way to harm the host application, so quota is a first-class concern.
//!
//! The `Quota` type is intentionally a small pure-logic component: given the current
//! total bytes on disk and a candidate write size, it decides whether the write fits.
//! The eviction ladder (§4.5) is implemented as a separate function that returns a
//! plan; the caller carries out the plan.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quota {
    pub limit_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fits {
    Yes,
    No { over_by: u64 },
}

impl Quota {
    pub fn new(limit_bytes: u64) -> Self {
        Self { limit_bytes }
    }

    pub fn check(&self, current_total: u64, record_size: u64) -> Fits {
        match current_total.checked_add(record_size) {
            Some(t) if t <= self.limit_bytes => Fits::Yes,
            Some(t) => Fits::No { over_by: t - self.limit_bytes },
            None => Fits::No { over_by: u64::MAX },
        }
    }

    pub fn headroom(&self, current_total: u64) -> u64 {
        self.limit_bytes.saturating_sub(current_total)
    }

    pub fn fill_ratio(&self, current_total: u64) -> f32 {
        if self.limit_bytes == 0 {
            return 1.0;
        }
        (current_total as f64 / self.limit_bytes as f64) as f32
    }
}

/// One rung of the §4.5 eviction ladder. Caller executes; storage layer plans.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvictionAction {
    DropP4Artifacts,
    DropPayloadCapturesPastMinRetention,
    DropClosedLifecyclesPastRetention,
    DropCorrelatedUneventful,
    ForceCloseOpenLifecyclesPastCeiling,
    /// Terminal: stop capture, raise critical alert (§4.5 step 6).
    StopCaptureAlert,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_that_fits_returns_yes() {
        let q = Quota::new(1_000);
        assert_eq!(q.check(400, 500), Fits::Yes);
    }

    #[test]
    fn record_at_exact_limit_fits() {
        let q = Quota::new(1_000);
        assert_eq!(q.check(500, 500), Fits::Yes);
    }

    #[test]
    fn record_over_limit_reports_delta() {
        let q = Quota::new(1_000);
        assert_eq!(q.check(600, 500), Fits::No { over_by: 100 });
    }

    #[test]
    fn overflow_is_reported_as_over() {
        let q = Quota::new(1_000);
        assert!(matches!(q.check(u64::MAX, 1), Fits::No { .. }));
    }

    #[test]
    fn headroom_saturates_at_zero() {
        let q = Quota::new(1_000);
        assert_eq!(q.headroom(1_500), 0);
    }

    #[test]
    fn fill_ratio() {
        let q = Quota::new(1_000);
        assert!((q.fill_ratio(500) - 0.5).abs() < f32::EPSILON);
    }
}
