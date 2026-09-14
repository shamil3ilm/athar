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
use athar_detection::{DecisionStore, DetectionConfig, SqliteDecisionStore};
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
        Some("policy") => match args.get(2).map(|s| s.as_str()) {
            Some("show") => cmd_policy_show(&args[3..]),
            Some("validate") => cmd_policy_validate(&args[3..]),
            _ => usage(2),
        },
        Some("doctor") => cmd_doctor(&args[2..]),
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

  policy show <path>
      Show the effective policy configuration. `<path>` is either a policies.json
      file directly or a data-dir (in which case config/policies.json inside it
      is read). Missing / malformed → defaults are shown, with a note.

  policy validate <policies.json>
      Parse and structurally validate a policies.json. Exits 0 on valid, 1 on
      any parse error (with the specific error message).

  doctor <data-dir> [--host 127.0.0.1] [--port 11223]
      End-to-end install health check. Verifies: data-dir exists, segment
      store readable, audit chain verifies, lifecycles + decisions DBs open,
      policies.json parseable, daemon TCP port reachable. Exits 0 if every
      check passes, 1 if any fails.

The state database is typically at <data-dir>/state/lifecycles.db.
The decisions database is typically at <data-dir>/state/decisions.db.
The policy config is typically at <data-dir>/config/policies.json.
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

fn cmd_policy_show(rest: &[String]) -> ExitCode {
    let Some(arg) = rest.first() else {
        eprintln!("usage: athar policy show <policies.json | data-dir>");
        return ExitCode::from(2);
    };
    // Auto-detect: file → use directly; dir → look for config/policies.json inside.
    let input = PathBuf::from(arg);
    let resolved = if input.is_dir() {
        input.join("config").join("policies.json")
    } else {
        input.clone()
    };

    let cfg = DetectionConfig::load_or_default(&resolved);
    let source = if resolved.exists() {
        format!("(from {})", resolved.display())
    } else {
        format!("(file not found at {}; showing defaults)", resolved.display())
    };
    println!("Policy configuration {source}\n");

    println!("POLICIES:");
    let rules = [
        ("high_amount_new_beneficiary", &cfg.policies.high_amount_new_beneficiary),
        ("high_velocity",               &cfg.policies.high_velocity),
        ("distinct_targets",            &cfg.policies.distinct_targets),
        ("credential_stuffing",         &cfg.policies.credential_stuffing),
    ];
    for (name, rule) in rules {
        let state = if rule.enabled { "ENABLED " } else { "disabled" };
        let mode_str = rule.mode.as_str();
        let fm_str = rule.fail_mode.as_str();
        println!("  {state}  {name:<34}  mode={mode_str:<10}  fail_mode={fm_str}");
    }
    println!();

    println!("SIGNAL THRESHOLDS:");
    println!("  high_amount_floor        : {}", cfg.signals.high_amount_floor);
    println!("  amount_field             : {}", cfg.signals.amount_field);
    println!(
        "  velocity                 : window={}ms  threshold>{}  max_subjects={}",
        cfg.signals.velocity.window_ms,
        cfg.signals.velocity.threshold,
        cfg.signals.velocity.max_subjects,
    );
    println!(
        "  targets                  : window={}ms  threshold>{}  max_subjects={}  max_targets_per_subject={}",
        cfg.signals.targets.window_ms,
        cfg.signals.targets.threshold,
        cfg.signals.targets.max_subjects,
        cfg.signals.targets.max_targets_per_subject,
    );
    println!(
        "  credential_stuffing      : window={}ms  threshold>{}  event_types={:?}  outcome_field={:?}  outcome_failed={:?}",
        cfg.signals.credential_stuffing.window_ms,
        cfg.signals.credential_stuffing.threshold,
        cfg.signals.credential_stuffing.event_types,
        cfg.signals.credential_stuffing.outcome_field,
        cfg.signals.credential_stuffing.outcome_failed_values,
    );

    // Warn on suspicious configurations that are usually mistakes.
    let mut warnings = Vec::<String>::new();
    if !cfg.policies.high_amount_new_beneficiary.enabled
        && !cfg.policies.high_velocity.enabled
        && !cfg.policies.distinct_targets.enabled
        && !cfg.policies.credential_stuffing.enabled
    {
        warnings.push(
            "every policy is DISABLED — the daemon will produce only ALLOW decisions"
                .into(),
        );
    }
    use athar_detection::PolicyMode;
    let has_enforce = matches!(cfg.policies.high_amount_new_beneficiary.mode, PolicyMode::Enforce)
        || matches!(cfg.policies.high_velocity.mode, PolicyMode::Enforce)
        || matches!(cfg.policies.distinct_targets.mode, PolicyMode::Enforce)
        || matches!(cfg.policies.credential_stuffing.mode, PolicyMode::Enforce);
    if has_enforce {
        warnings.push("at least one policy is in ENFORCE mode — the shim's Decision::isEnforced() will be true for matching events, and callers who respect that will block requests".into());
    }
    if !warnings.is_empty() {
        println!("\nNOTES:");
        for w in warnings {
            println!("  * {w}");
        }
    }

    ExitCode::from(0)
}

fn cmd_doctor(rest: &[String]) -> ExitCode {
    let Some(arg) = rest.first() else {
        eprintln!("usage: athar doctor <data-dir> [--host 127.0.0.1] [--port 11223]");
        return ExitCode::from(2);
    };
    let data_dir = PathBuf::from(arg);
    let host = flag_value(rest, "--host").unwrap_or_else(|| "127.0.0.1".into());
    let port: u16 = flag_value(rest, "--port")
        .and_then(|s| s.parse().ok())
        .unwrap_or(11223);

    let mut fails = 0_u32;
    let mut warns = 0_u32;
    println!("athar doctor — checking install at {}\n", data_dir.display());

    // 1. data-dir exists
    fails += !check(
        "data-dir exists",
        data_dir.exists(),
        &format!("path not found: {}", data_dir.display()),
    ) as u32;

    // 2. audit segment store — open + verify_all
    let audit_dir = data_dir.join("audit");
    if audit_dir.exists() {
        match SegmentStore::open(&audit_dir) {
            Ok(store) => {
                fails += !check("audit segment store opens", true, "") as u32;
                match store.verify_all() {
                    Ok(_) => {
                        check("audit chain verifies to genesis", true, "");
                    }
                    Err(e) => {
                        fails += !check(
                            "audit chain verifies to genesis",
                            false,
                            &format!("{e}"),
                        ) as u32;
                    }
                }
            }
            Err(e) => {
                fails += !check(
                    "audit segment store opens",
                    false,
                    &format!("{e}"),
                ) as u32;
            }
        }
    } else {
        warns += 1;
        println!(
            "  warn  audit directory not created yet ({}) — expected after first daemon boot",
            audit_dir.display()
        );
    }

    // 3. lifecycles DB opens; row count
    let life_db = data_dir.join("state").join("lifecycles.db");
    if life_db.exists() {
        match SqliteLifecycleStore::open(&life_db) {
            Ok(store) => {
                let total = store.count();
                let open = store.count_open();
                fails += !check(
                    "lifecycles DB opens",
                    true,
                    &format!("total={total} open={open}"),
                ) as u32;
            }
            Err(e) => {
                fails += !check(
                    "lifecycles DB opens",
                    false,
                    &format!("{e}"),
                ) as u32;
            }
        }
    } else {
        warns += 1;
        println!(
            "  warn  lifecycles DB not created yet ({}) — expected after first event",
            life_db.display()
        );
    }

    // 4. decisions DB opens; row count
    let dec_db = data_dir.join("state").join("decisions.db");
    if dec_db.exists() {
        match SqliteDecisionStore::open(&dec_db) {
            Ok(store) => {
                let count = store.count_decisions();
                let signals = store.count_signals();
                fails += !check(
                    "decisions DB opens",
                    true,
                    &format!("decisions={count} signals={signals}"),
                ) as u32;
            }
            Err(e) => {
                fails += !check(
                    "decisions DB opens",
                    false,
                    &format!("{e}"),
                ) as u32;
            }
        }
    } else {
        warns += 1;
        println!(
            "  warn  decisions DB not created yet ({}) — expected after first event",
            dec_db.display()
        );
    }

    // 5. policies.json parseable (if present)
    let policies_path = data_dir.join("config").join("policies.json");
    if policies_path.exists() {
        match std::fs::read_to_string(&policies_path) {
            Ok(text) => match serde_json::from_str::<DetectionConfig>(&text) {
                Ok(_) => {
                    check("policies.json is valid", true, "");
                }
                Err(e) => {
                    fails += !check(
                        "policies.json is valid",
                        false,
                        &format!("parse error: {e}"),
                    ) as u32;
                }
            },
            Err(e) => {
                fails += !check(
                    "policies.json is readable",
                    false,
                    &format!("{e}"),
                ) as u32;
            }
        }
    } else {
        println!(
            "  info  policies.json not present ({}) — daemon will use built-in defaults",
            policies_path.display()
        );
    }

    // 6. daemon TCP reachable
    let addr = format!("{host}:{port}");
    let reachable = match std::net::ToSocketAddrs::to_socket_addrs(&addr).ok().and_then(|mut it| it.next()) {
        Some(sock_addr) => std::net::TcpStream::connect_timeout(
            &sock_addr,
            std::time::Duration::from_millis(500),
        ).is_ok(),
        None => false,
    };
    fails += !check(
        &format!("daemon reachable at {addr}"),
        reachable,
        if reachable { "" } else { "TCP connect failed within 500ms" },
    ) as u32;

    println!();
    if fails == 0 {
        if warns > 0 {
            println!("DOCTOR: OK ({warns} advisory warning(s))");
        } else {
            println!("DOCTOR: OK");
        }
        ExitCode::from(0)
    } else {
        println!("DOCTOR: {fails} FAIL(S), {warns} warning(s)");
        ExitCode::from(1)
    }
}

/// Print one check line. Returns whether the check passed (so the caller can
/// increment a fail counter).
fn check(name: &str, ok: bool, detail: &str) -> bool {
    if ok {
        if detail.is_empty() {
            println!("  ok    {name}");
        } else {
            println!("  ok    {name}  ({detail})");
        }
    } else if detail.is_empty() {
        println!("  FAIL  {name}");
    } else {
        println!("  FAIL  {name}  -- {detail}");
    }
    ok
}

/// Parse `--flag value` out of the tail of args. Returns None if flag is absent.
fn flag_value(rest: &[String], flag: &str) -> Option<String> {
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        if a == flag {
            return it.next().cloned();
        }
    }
    None
}

fn cmd_policy_validate(rest: &[String]) -> ExitCode {
    let Some(arg) = rest.first() else {
        eprintln!("usage: athar policy validate <policies.json>");
        return ExitCode::from(2);
    };
    let path = PathBuf::from(arg);
    if !path.exists() {
        eprintln!("file does not exist: {}", path.display());
        return ExitCode::from(1);
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot read {}: {}", path.display(), e);
            return ExitCode::from(1);
        }
    };
    match serde_json::from_str::<DetectionConfig>(&text) {
        Ok(_) => {
            println!("OK: {} is a valid detection config", path.display());
            ExitCode::from(0)
        }
        Err(e) => {
            eprintln!("INVALID: {}", path.display());
            eprintln!("  {e}");
            ExitCode::from(1)
        }
    }
}
