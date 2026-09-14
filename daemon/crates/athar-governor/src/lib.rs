//! Resource governor (SPEC §7.3, §7.5, INV-14).
//!
//! Reads host headroom + runtime budget compliance + shim heartbeats and publishes a
//! single `PressureLevel` all components subscribe to. Every worker pool, queue, and
//! background task MUST honour changes within 100 ms (OPS-18).
//!
//! Also owns the dead-man's switch (`INV-14`): if the shim cannot confirm it is within
//! `PERF-1`..`PERF-3` for a sustained window, non-enforcement instrumentation self-disables
//! without waiting for the daemon or operator.
//!
//! Appendix A step 5: built BEFORE the features it protects.

#![deny(unsafe_code)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tracing::{debug, warn};

/// §7.5 degradation ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum PressureLevel {
    L0Normal,
    L1Reduce,
    L2Defer,
    L3Essential,
    L4SafeMode,
}

impl PressureLevel {
    pub fn allows(self, priority: Priority) -> bool {
        match (self, priority) {
            (PressureLevel::L0Normal, _) => true,
            (PressureLevel::L1Reduce, Priority::P4) => false,
            (PressureLevel::L1Reduce, _) => true,
            (PressureLevel::L2Defer, Priority::P3 | Priority::P4) => false,
            (PressureLevel::L2Defer, _) => true,
            (PressureLevel::L3Essential, Priority::P0 | Priority::P1) => true,
            (PressureLevel::L3Essential, _) => false,
            (PressureLevel::L4SafeMode, Priority::P0) => true,
            (PressureLevel::L4SafeMode, _) => false,
        }
    }
}

/// §2.2 priority classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Priority {
    /// Customer workflow, or explicitly-configured enforcement.
    P0,
    /// Security signals for enforced policies, audit chain, decision records, lifecycle integrity.
    P1,
    /// Correlation, reconciliation.
    P2,
    /// Deep analysis, discovery, enrichment.
    P3,
    /// Optimisation, compaction, model refresh.
    P4,
}

/// Host-level headroom snapshot. Values are percentages in `[0.0, 100.0]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HostMetrics {
    pub cpu_headroom_pct: f32,
    pub memory_free_pct: f32,
    pub disk_free_pct: f32,
    pub taken_at: Instant,
}

/// Runtime's own budget compliance (§7.2 self-measurement).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BudgetObservation {
    /// PERF-1: default 200 us.
    pub p50_latency_us: u64,
    /// PERF-2: default 1000 us.
    pub p99_latency_us: u64,
    /// Negative means throughput loss.
    pub throughput_delta_pct: f32,
    pub taken_at: Instant,
}

/// Dead-man's switch state (INV-14).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DeadmanSignal {
    Ok,
    Tripped { since: Instant, reason: DeadmanReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeadmanReason {
    /// PERF-1/PERF-2 breached beyond `sustained_breach_window`.
    BudgetBreach,
    /// Runtime self-check failed.
    SelfCheckFailure,
    /// No shim confirmation for `shim_heartbeat_timeout`.
    NoShimConfirmation,
}

/// Thresholds. Real deployments will load these from config; defaults match §7.1.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    // §7.1 supported operating conditions
    pub cpu_headroom_l1_pct: f32,   // < → L1
    pub cpu_headroom_l2_pct: f32,   // < → L2
    pub memory_free_l1_pct: f32,
    pub memory_free_l2_pct: f32,
    pub disk_free_l1_pct: f32,
    pub disk_free_l2_pct: f32,

    // PERF budgets
    pub p99_latency_budget_us: u64,     // PERF-2
    pub p99_latency_l2_us: u64,         // 2x budget
    pub p99_latency_l3_us: u64,         // 5x budget

    // §7.5 hysteresis
    pub good_news_window: Duration,       // require sustained recovery before demoting pressure

    // INV-14 dead-man's switch
    pub sustained_breach_window: Duration,  // PERF breach for this long → deadman trip
    pub shim_heartbeat_timeout: Duration,   // no heartbeat for this long → deadman trip
}

impl Default for Config {
    fn default() -> Self {
        Self {
            cpu_headroom_l1_pct: 20.0,
            cpu_headroom_l2_pct: 10.0,
            memory_free_l1_pct: 15.0,
            memory_free_l2_pct: 10.0,
            disk_free_l1_pct: 15.0,
            disk_free_l2_pct: 10.0,
            p99_latency_budget_us: 1000,     // PERF-2 default
            p99_latency_l2_us: 2000,
            p99_latency_l3_us: 5000,
            good_news_window: Duration::from_secs(30),
            sustained_breach_window: Duration::from_secs(10),
            shim_heartbeat_timeout: Duration::from_secs(15),
        }
    }
}

/// Governor: single source of truth for pressure and the dead-man's switch.
pub struct Governor {
    config: Config,
    pressure_tx: watch::Sender<PressureLevel>,
    deadman_tx: watch::Sender<DeadmanSignal>,
    state: Arc<Mutex<State>>,
}

struct State {
    host: Option<HostMetrics>,
    budget: Option<BudgetObservation>,
    last_heartbeat: Option<Instant>,
    /// Highest pressure observed since we last dipped below it — for hysteresis.
    breach_since: Option<Instant>,
    /// When the current pressure level was last set. Used for good-news window.
    at_current_since: Instant,
    current: PressureLevel,
    deadman: DeadmanSignal,
}

impl Governor {
    pub fn new(config: Config) -> Self {
        let (pressure_tx, _) = watch::channel(PressureLevel::L0Normal);
        let (deadman_tx, _) = watch::channel(DeadmanSignal::Ok);
        Self {
            config,
            pressure_tx,
            deadman_tx,
            state: Arc::new(Mutex::new(State {
                host: None,
                budget: None,
                last_heartbeat: None,
                breach_since: None,
                at_current_since: Instant::now(),
                current: PressureLevel::L0Normal,
                deadman: DeadmanSignal::Ok,
            })),
        }
    }

    /// Subscribers get every pressure change; OPS-18 requires reacting within 100 ms.
    pub fn subscribe(&self) -> watch::Receiver<PressureLevel> {
        self.pressure_tx.subscribe()
    }

    pub fn subscribe_deadman(&self) -> watch::Receiver<DeadmanSignal> {
        self.deadman_tx.subscribe()
    }

    pub fn current_level(&self) -> PressureLevel {
        *self.pressure_tx.borrow()
    }

    pub fn current_deadman(&self) -> DeadmanSignal {
        *self.deadman_tx.borrow()
    }

    pub fn report_host(&self, m: HostMetrics) {
        let mut s = self.state.lock().expect("governor state poisoned");
        s.host = Some(m);
        self.recompute(&mut s, m.taken_at);
    }

    pub fn report_budget(&self, o: BudgetObservation) {
        let mut s = self.state.lock().expect("governor state poisoned");
        s.budget = Some(o);
        self.recompute(&mut s, o.taken_at);
    }

    pub fn shim_heartbeat(&self, now: Instant) {
        let mut s = self.state.lock().expect("governor state poisoned");
        s.last_heartbeat = Some(now);
        // If deadman was tripped by NoShimConfirmation, do NOT auto-clear here;
        // an operator ack (or a good-news window with a fresh heartbeat) will.
        self.recompute(&mut s, now);
    }

    /// Force a re-check based on the current wall time; call periodically for
    /// timeout-based conditions (heartbeat, breach window).
    pub fn tick(&self, now: Instant) {
        let mut s = self.state.lock().expect("governor state poisoned");
        self.recompute(&mut s, now);
    }

    fn recompute(&self, s: &mut State, now: Instant) {
        // Step 1: compute the intrinsic level from live inputs.
        let intrinsic = self.intrinsic_level(s);

        // Step 2: check the dead-man's switch (INV-14).
        let deadman = self.evaluate_deadman(s, now, intrinsic);
        if deadman != s.deadman {
            if let DeadmanSignal::Tripped { reason, .. } = deadman {
                warn!(?reason, "dead-man's switch tripped");
            }
            s.deadman = deadman;
            // send_replace: unconditional update, even with no active receivers.
            self.deadman_tx.send_replace(deadman);
        }

        // Step 3: apply deadman override and hysteresis, produce the effective level.
        let target = match deadman {
            DeadmanSignal::Tripped { .. } => PressureLevel::L4SafeMode,
            DeadmanSignal::Ok => intrinsic,
        };

        let new_level = self.apply_hysteresis(s, target, now);
        if new_level != s.current {
            debug!(from = ?s.current, to = ?new_level, "pressure level changed");
            s.current = new_level;
            s.at_current_since = now;
            // send_replace updates the value unconditionally, even with no active
            // receivers (unit tests read via current_level() without subscribing).
            self.pressure_tx.send_replace(new_level);
        }
    }

    fn intrinsic_level(&self, s: &State) -> PressureLevel {
        let mut worst = PressureLevel::L0Normal;
        let c = &self.config;
        if let Some(h) = s.host {
            worst = worst.max(bucket3(h.cpu_headroom_pct, c.cpu_headroom_l1_pct, c.cpu_headroom_l2_pct));
            worst = worst.max(bucket3(h.memory_free_pct, c.memory_free_l1_pct, c.memory_free_l2_pct));
            worst = worst.max(bucket3(h.disk_free_pct, c.disk_free_l1_pct, c.disk_free_l2_pct));
        }
        if let Some(b) = s.budget {
            worst = worst.max(latency_bucket(
                b.p99_latency_us,
                c.p99_latency_budget_us,
                c.p99_latency_l2_us,
                c.p99_latency_l3_us,
            ));
        }
        worst
    }

    fn evaluate_deadman(&self, s: &mut State, now: Instant, intrinsic: PressureLevel) -> DeadmanSignal {
        // Trip on sustained PERF-2 breach.
        if let Some(b) = s.budget {
            if b.p99_latency_us > self.config.p99_latency_budget_us {
                let start = s.breach_since.get_or_insert(b.taken_at);
                if now.saturating_duration_since(*start) >= self.config.sustained_breach_window {
                    return DeadmanSignal::Tripped { since: *start, reason: DeadmanReason::BudgetBreach };
                }
            } else {
                s.breach_since = None;
            }
        }

        // Trip on missing shim heartbeats.
        if let Some(last) = s.last_heartbeat {
            if now.saturating_duration_since(last) >= self.config.shim_heartbeat_timeout {
                return DeadmanSignal::Tripped { since: last, reason: DeadmanReason::NoShimConfirmation };
            }
        }

        // Preserve an already-tripped state until an operator ack unless the underlying condition
        // has clearly recovered. For V0, keep it simple: recover automatically after the
        // good_news_window with the intrinsic level back at L0.
        if let DeadmanSignal::Tripped { since, .. } = s.deadman {
            let recovered = intrinsic == PressureLevel::L0Normal
                && now.saturating_duration_since(since) >= self.config.good_news_window;
            if recovered {
                return DeadmanSignal::Ok;
            }
            return s.deadman;
        }

        DeadmanSignal::Ok
    }

    fn apply_hysteresis(&self, s: &State, target: PressureLevel, now: Instant) -> PressureLevel {
        if target >= s.current {
            // Bad news: escalate immediately.
            return target;
        }
        // Good news: require the improved condition to hold for good_news_window.
        if now.saturating_duration_since(s.at_current_since) >= self.config.good_news_window {
            target
        } else {
            s.current
        }
    }
}

/// Maps a headroom percentage into a pressure level. `l1` and `l2` are thresholds below which
/// the level rises. `l2 < l1` is expected; violated inputs yield L0.
fn bucket3(observed: f32, l1: f32, l2: f32) -> PressureLevel {
    if observed < l2 {
        PressureLevel::L2Defer
    } else if observed < l1 {
        PressureLevel::L1Reduce
    } else {
        PressureLevel::L0Normal
    }
}

fn latency_bucket(observed_us: u64, budget: u64, l2: u64, l3: u64) -> PressureLevel {
    if observed_us >= l3 {
        PressureLevel::L3Essential
    } else if observed_us >= l2 {
        PressureLevel::L2Defer
    } else if observed_us > budget {
        PressureLevel::L1Reduce
    } else {
        PressureLevel::L0Normal
    }
}

pub const CRATE_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    fn healthy_host(at: Instant) -> HostMetrics {
        HostMetrics {
            cpu_headroom_pct: 80.0,
            memory_free_pct: 70.0,
            disk_free_pct: 60.0,
            taken_at: at,
        }
    }

    fn healthy_budget(at: Instant) -> BudgetObservation {
        BudgetObservation {
            p50_latency_us: 100,
            p99_latency_us: 400,
            throughput_delta_pct: -0.5,
            taken_at: at,
        }
    }

    #[test]
    fn starts_at_l0() {
        let g = Governor::new(Config::default());
        assert_eq!(g.current_level(), PressureLevel::L0Normal);
        assert_eq!(g.current_deadman(), DeadmanSignal::Ok);
    }

    #[test]
    fn healthy_metrics_stay_l0() {
        let g = Governor::new(Config::default());
        let at = t0();
        g.report_host(healthy_host(at));
        g.report_budget(healthy_budget(at));
        assert_eq!(g.current_level(), PressureLevel::L0Normal);
    }

    #[test]
    fn cpu_pressure_promotes_immediately() {
        let g = Governor::new(Config::default());
        let at = t0();
        g.report_host(HostMetrics {
            cpu_headroom_pct: 5.0, // below L2 threshold of 10
            memory_free_pct: 70.0,
            disk_free_pct: 60.0,
            taken_at: at,
        });
        assert_eq!(g.current_level(), PressureLevel::L2Defer);
    }

    #[test]
    fn latency_breach_promotes() {
        let g = Governor::new(Config::default());
        let at = t0();
        g.report_host(healthy_host(at));
        // 3000 us > 2000 us L2 threshold, < 5000 us L3
        g.report_budget(BudgetObservation {
            p50_latency_us: 500,
            p99_latency_us: 3000,
            throughput_delta_pct: -1.0,
            taken_at: at,
        });
        assert_eq!(g.current_level(), PressureLevel::L2Defer);
    }

    #[test]
    fn deadman_trips_on_sustained_breach_and_forces_l4() {
        let cfg = Config {
            sustained_breach_window: Duration::from_millis(100),
            ..Config::default()
        };
        let g = Governor::new(cfg);
        let start = t0();

        g.report_host(healthy_host(start));
        g.report_budget(BudgetObservation {
            p50_latency_us: 500,
            p99_latency_us: 1500, // over PERF-2
            throughput_delta_pct: -1.0,
            taken_at: start,
        });
        // Immediately after the first breach report: level rises to L1 but the
        // dead-man's switch requires the breach to be SUSTAINED — no trip yet.
        assert_eq!(g.current_level(), PressureLevel::L1Reduce);
        assert_eq!(g.current_deadman(), DeadmanSignal::Ok);
        // Wait past the sustained-breach window → deadman trips → L4.
        let later = start + Duration::from_millis(200);
        g.tick(later);
        assert_eq!(g.current_level(), PressureLevel::L4SafeMode);
        assert!(matches!(
            g.current_deadman(),
            DeadmanSignal::Tripped { reason: DeadmanReason::BudgetBreach, .. }
        ));
    }

    #[test]
    fn deadman_trips_on_missing_heartbeat() {
        let cfg = Config {
            shim_heartbeat_timeout: Duration::from_millis(50),
            ..Config::default()
        };
        let g = Governor::new(cfg);
        let start = t0();
        g.shim_heartbeat(start);
        g.tick(start + Duration::from_millis(100)); // past timeout
        assert!(matches!(
            g.current_deadman(),
            DeadmanSignal::Tripped { reason: DeadmanReason::NoShimConfirmation, .. }
        ));
        assert_eq!(g.current_level(), PressureLevel::L4SafeMode);
    }

    #[test]
    fn good_news_hysteresis_defers_demotion() {
        let cfg = Config {
            good_news_window: Duration::from_millis(200),
            ..Config::default()
        };
        let g = Governor::new(cfg);
        let t = t0();
        // Escalate to L2 via CPU pressure.
        g.report_host(HostMetrics {
            cpu_headroom_pct: 5.0,
            memory_free_pct: 70.0,
            disk_free_pct: 60.0,
            taken_at: t,
        });
        assert_eq!(g.current_level(), PressureLevel::L2Defer);
        // Immediately report healthy — should NOT drop yet.
        g.report_host(healthy_host(t + Duration::from_millis(10)));
        assert_eq!(g.current_level(), PressureLevel::L2Defer);
        // After good_news_window with continued healthy metrics, demote.
        g.report_host(healthy_host(t + Duration::from_millis(300)));
        assert_eq!(g.current_level(), PressureLevel::L0Normal);
    }

    #[test]
    fn priority_gating() {
        assert!(PressureLevel::L0Normal.allows(Priority::P4));
        assert!(!PressureLevel::L1Reduce.allows(Priority::P4));
        assert!(PressureLevel::L1Reduce.allows(Priority::P3));
        assert!(!PressureLevel::L2Defer.allows(Priority::P3));
        assert!(PressureLevel::L3Essential.allows(Priority::P1));
        assert!(!PressureLevel::L3Essential.allows(Priority::P2));
        assert!(PressureLevel::L4SafeMode.allows(Priority::P0));
        assert!(!PressureLevel::L4SafeMode.allows(Priority::P1));
    }

    #[tokio::test]
    async fn subscribers_receive_updates() {
        let g = Governor::new(Config::default());
        let mut rx = g.subscribe();
        let t = t0();
        g.report_host(HostMetrics {
            cpu_headroom_pct: 5.0,
            memory_free_pct: 70.0,
            disk_free_pct: 60.0,
            taken_at: t,
        });
        rx.changed().await.expect("watch changed");
        assert_eq!(*rx.borrow(), PressureLevel::L2Defer);
    }
}
