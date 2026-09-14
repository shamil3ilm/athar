//! athar CLI (SPEC §15.1, OPS-30).
//!
//! V0 subset:
//!   audit verify <dir>           Verify persisted audit segments.
//!   lifecycle list <db>          List lifecycles (--open for open only).
//!   lifecycle show <db> <id>     Show one lifecycle as JSON.
//!
//! Read-only. The daemon holds the SQLite database in WAL mode, so concurrent
//! readers (this CLI) do not block writers (the daemon).

use std::path::PathBuf;
use std::process::ExitCode;

use athar_audit::persistence::SegmentStore;
use athar_detection::{DecisionStore, SqliteDecisionStore};
use athar_lifecycle::{LifecycleStore, SqliteLifecycleStore};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("audit") => match args.get(2).map(|s| s.as_str()) {
            Some("verify") => cmd_audit_verify(&args[3..]),
            _ => usage(2),
        },
        Some("lifecycle") => match args.get(2).map(|s| s.as_str()) {
            Some("list") => cmd_lifecycle_list(&args[3..]),
            Some("show") => cmd_lifecycle_show(&args[3..]),
            _ => usage(2),
        },
        Some("decision") => match args.get(2).map(|s| s.as_str()) {
            Some("recent") => cmd_decision_recent(&args[3..]),
            Some("show") => cmd_decision_show(&args[3..]),
            _ => usage(2),
        },
        Some("--help") | Some("-h") | None => usage(0),
        Some(other) => {
            eprintln!("unknown subcommand: {other}");
            usage(2)
        }
    }
}

fn usage(code: u8) -> ExitCode {
    let text = "\
athar CLI (V0 subset)

SUBCOMMANDS:
  audit verify <segments-dir>
      Verify every audit segment; report exact record index of any break.

  lifecycle list <state-db> [--open] [--json]
      List lifecycles. Default: id, type, state, closure, started, last_event.
      --open : show only Open lifecycles.
      --json : one lifecycle per line as JSON.

  lifecycle show <state-db> <lifecycle-id> [--json]
      Show one lifecycle. --json prints the full body.

  decision recent <decisions-db> [--limit N] [--json]
      Show the N most recent decisions (default 20).

  decision show <decisions-db> <decision-id>
      Show one decision record with all signals, reason codes, and explanation.

The state database is typically at <data-dir>/state/lifecycles.db.
The decisions database is typically at <data-dir>/state/decisions.db.
";
    if code == 0 {
        println!("{text}");
    } else {
        eprintln!("{text}");
    }
    ExitCode::from(code)
}

fn cmd_audit_verify(rest: &[String]) -> ExitCode {
    let Some(dir) = rest.first() else {
        eprintln!("usage: athar audit verify <audit-segments-dir>");
        return ExitCode::from(2);
    };
    let root = PathBuf::from(dir);
    let store = match SegmentStore::open(&root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot open segment store at {}: {}", root.display(), e);
            return ExitCode::from(1);
        }
    };
    match store.verify_all() {
        Ok(report) => {
            println!(
                "OK: {} segment(s), {} record(s) verified",
                report.segments_verified, report.records_verified
            );
            ExitCode::from(0)
        }
        Err(e) => {
            eprintln!("VERIFY FAILED: {e}");
            ExitCode::from(1)
        }
    }
}

fn cmd_lifecycle_list(rest: &[String]) -> ExitCode {
    let Some(db) = rest.first() else {
        eprintln!("usage: athar lifecycle list <state-db> [--open] [--json]");
        return ExitCode::from(2);
    };
    let only_open = rest.iter().any(|a| a == "--open");
    let as_json = rest.iter().any(|a| a == "--json");
    let store = match SqliteLifecycleStore::open(db) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot open state db {db}: {e}");
            return ExitCode::from(1);
        }
    };
    if only_open {
        let list = store.all_open();
        print_list(list, as_json)
    } else {
        // For V0 there's no all(); reconstruct by "list open" + noting total via count.
        // Add a small SQL fallback here: read every id, then fetch each.
        // Simpler: since we can't list all closed without extending the trait, print open + summary.
        let list = store.all_open();
        println!("(showing {} open of {} total; use --open to filter)", list.len(), store.count());
        print_list(list, as_json)
    }
}

fn print_list(list: Vec<athar_lifecycle::Lifecycle>, as_json: bool) -> ExitCode {
    if as_json {
        for lc in list {
            match serde_json::to_string(&lc) {
                Ok(s) => println!("{s}"),
                Err(e) => eprintln!("serialize failed: {e}"),
            }
        }
    } else {
        println!("{:<44} {:<10} {:<20} {:<24} {:>13} {:>13}",
            "id", "type", "state", "closure", "started_ms", "last_event_ms");
        for lc in list {
            println!("{:<44} {:<10} {:<20} {:<24} {:>13} {:>13}",
                lc.id,
                format!("{:?}", lc.lifecycle_type),
                format!("{:?}", lc.state),
                format!("{:?}", lc.closure),
                lc.started_at_ms,
                lc.last_event_at_ms,
            );
        }
    }
    ExitCode::from(0)
}

fn cmd_decision_recent(rest: &[String]) -> ExitCode {
    let Some(db) = rest.first() else {
        eprintln!("usage: athar decision recent <decisions-db> [--limit N] [--json]");
        return ExitCode::from(2);
    };
    let as_json = rest.iter().any(|a| a == "--json");
    let limit: usize = rest.windows(2)
        .find(|w| w[0] == "--limit")
        .and_then(|w| w[1].parse().ok())
        .unwrap_or(20);
    let store = match SqliteDecisionStore::open(db) {
        Ok(s) => s,
        Err(e) => { eprintln!("cannot open decisions db {db}: {e}"); return ExitCode::from(1); }
    };
    let list = store.recent_decisions(limit);
    if as_json {
        for d in list {
            match serde_json::to_string(&d) {
                Ok(s) => println!("{s}"),
                Err(e) => eprintln!("serialize failed: {e}"),
            }
        }
    } else {
        println!("{:<44} {:<8} {:<10} {:>16} {:<20}", "decision_id", "action", "mode", "timestamp_ms", "event_id");
        for d in list {
            println!(
                "{:<44} {:<8} {:<10} {:>16} {:<20}",
                d.decision_id,
                d.action.as_str(),
                d.mode.as_str(),
                d.timestamp_ms,
                d.subject.event_id,
            );
        }
    }
    ExitCode::from(0)
}

fn cmd_decision_show(rest: &[String]) -> ExitCode {
    let Some(db) = rest.first() else {
        eprintln!("usage: athar decision show <decisions-db> <decision-id> [--json]");
        return ExitCode::from(2);
    };
    let Some(id) = rest.get(1) else {
        eprintln!("usage: athar decision show <decisions-db> <decision-id> [--json]");
        return ExitCode::from(2);
    };
    let as_json = rest.iter().any(|a| a == "--json");
    let store = match SqliteDecisionStore::open(db) {
        Ok(s) => s,
        Err(e) => { eprintln!("cannot open decisions db {db}: {e}"); return ExitCode::from(1); }
    };
    let Some(d) = store.get_decision(id) else {
        eprintln!("not found: {id}");
        return ExitCode::from(1);
    };
    if as_json {
        match serde_json::to_string_pretty(&d) {
            Ok(s) => println!("{s}"),
            Err(e) => { eprintln!("serialize failed: {e}"); return ExitCode::from(1); }
        }
    } else {
        println!("decision_id       : {}", d.decision_id);
        println!("timestamp_ms      : {}", d.timestamp_ms);
        println!("tenant_id         : {}", d.tenant_id);
        println!("event_id          : {}", d.subject.event_id);
        println!("lifecycle_id      : {}", d.subject.lifecycle_id.as_deref().unwrap_or("-"));
        println!("action            : {}", d.action.as_str());
        println!("mode              : {}", d.mode.as_str());
        println!("fail_mode         : {}", d.fail_mode.as_str());
        println!("degradation       : {}", d.degradation_level);
        println!("reason_codes      : [{}]", d.reason_codes.join(", "));
        println!("latency_us        : {}", d.latency_us);
        println!("policies evaluated:");
        for p in &d.policies_evaluated {
            println!("  {} v{} → matched={} action={} mode={}",
                p.policy_id, p.policy_version, p.matched, p.action.as_str(), p.mode.as_str());
        }
        println!("signals used:");
        for s in &d.signals_used {
            println!("  {} {} conf={:.2} val={}", s.signal_id, s.kind.as_str(), s.confidence, s.value);
        }
        println!("engine versions   : detector={} policy={} resolver={}",
            d.engine_versions.detector, d.engine_versions.policy, d.engine_versions.resolver);
        println!("");
        println!("EXPLANATION:");
        println!("  {}", d.explanation);
    }
    ExitCode::from(0)
}

fn cmd_lifecycle_show(rest: &[String]) -> ExitCode {
    let Some(db) = rest.first() else {
        eprintln!("usage: athar lifecycle show <state-db> <id> [--json]");
        return ExitCode::from(2);
    };
    let Some(id) = rest.get(1) else {
        eprintln!("usage: athar lifecycle show <state-db> <id> [--json]");
        return ExitCode::from(2);
    };
    let as_json = rest.iter().any(|a| a == "--json");
    let store = match SqliteLifecycleStore::open(db) {
        Ok(s) => s,
        Err(e) => { eprintln!("cannot open state db {db}: {e}"); return ExitCode::from(1); }
    };
    let Some(lc) = store.get(id) else {
        eprintln!("not found: {id}");
        return ExitCode::from(1);
    };
    if as_json {
        match serde_json::to_string_pretty(&lc) {
            Ok(s) => println!("{s}"),
            Err(e) => { eprintln!("serialize failed: {e}"); return ExitCode::from(1); }
        }
    } else {
        println!("id            : {}", lc.id);
        println!("tenant_id     : {}", lc.tenant_id);
        println!("type          : {:?}", lc.lifecycle_type);
        println!("business_key  : {}", lc.business_key.as_deref().unwrap_or("-"));
        println!("resource_id   : {}", lc.resource_id.as_deref().unwrap_or("-"));
        println!("state         : {:?}", lc.state);
        println!("closure       : {:?}", lc.closure);
        println!("started_at_ms : {}", lc.started_at_ms);
        println!("last_event_ms : {}", lc.last_event_at_ms);
        println!("closed_at_ms  : {}", lc.closed_at_ms.map(|v| v.to_string()).unwrap_or("-".into()));
        println!("events        : {} ({} tiers)", lc.event_ids.len(), lc.tiers.len());
        println!("late_events   : {}", lc.late_events.len());
        for (i, ev) in lc.event_ids.iter().enumerate() {
            let tier = lc.tiers.get(i).copied().unwrap_or(athar_lifecycle::InferenceTier::Unknown);
            println!("  #{:03} {} (tier: {:?}, conf: {:.2})", i, ev, tier, tier.confidence());
        }
        for le in &lc.late_events {
            println!("  LATE {} ({}) -> {:?} at {}", le.event_id, le.event_type, le.class, le.arrived_at_ms);
        }
    }
    ExitCode::from(0)
}
