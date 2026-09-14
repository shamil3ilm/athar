# athar

[![CI](https://github.com/shamil3ilm/athar/actions/workflows/ci.yml/badge.svg)](https://github.com/shamil3ilm/athar/actions/workflows/ci.yml)

Privacy-first local application security, trust, lifecycle & intelligence
runtime. A thin PHP shim in your app + a fat Rust daemon on the same host.
No cloud dependency; the daemon binds loopback only and never phones home.

## What it does

- Captures every business event your app emits (payments, logins, model saves)
- Correlates them into lifecycles with a 4-tier resolution ladder
- Runs a signal + policy engine over them (new_beneficiary, high_amount,
  high_velocity, distinct_targets, credential_stuffing_pattern) that produces
  explainable decisions (`action`, `reason_codes`, `explanation`)
- Persists an append-only, hash-chained + signed audit log so decisions
  are reconstructable months later
- Never blocks your request unless you explicitly call the synchronous
  `Runtime::evaluate()` gate

## Quick links

| Doc | Purpose |
|---|---|
| [`SPEC.md`](./SPEC.md) | Normative specification (v1) |
| [`docs/INTEGRATE.md`](./docs/INTEGRATE.md) | 5-minute integration into an existing PHP / Laravel app |
| [`docs/RUN.md`](./docs/RUN.md) | Daemon operations: env vars, policy config, CLI |
| [`docs/TESTING_LARAVEL.md`](./docs/TESTING_LARAVEL.md) | Testing plan against a real Laravel payments app |
| [`docs/conformance.md`](./docs/conformance.md) | Requirement ID → test mapping |
| [`docs/DECISIONS.md`](./docs/DECISIONS.md) | D1–D14 resolutions (all ACCEPTED 2026-09-13) |
| [`refapp/README.md`](./refapp/README.md) | Pure-PHP end-to-end simulator + verifier |

## Repo layout

```
SPEC.md                       Normative specification (v1)
schema/event.v1.0.json        Canonical event JSON Schema
docs/                         Operator + integrator guides
shim/                         PHP shim (drops into Laravel + plain PHP)
  src/                        PSR-4 (namespace Athar\)
  src/Adapter/Laravel/        Auto-discovered service provider
daemon/                       Rust workspace (7 crates + binary)
  crates/athar-event          Canonical event schema + validators
  crates/athar-storage        Append-only segment log
  crates/athar-audit          Hash chain + Ed25519 signing
  crates/athar-governor       Pressure ladder + dead-man's switch
  crates/athar-lifecycle      SQLite state + 4-tier correlation
  crates/athar-detection      Signals + policies + decisions
  crates/athar-daemon         TCP ingest + background tasks
  crates/athar-cli            audit / lifecycle / decision / policy verbs
refapp/                       Pure-PHP simulator + verifier (10 scenarios)
```

## Status — 2026-09-14

**V0 vertical slice complete.** Every V0 acceptance criterion has a green test.

- **Detection surface**: 5 signals (new_beneficiary, high_amount, high_velocity,
  distinct_targets, credential_stuffing_pattern), 4 policies, all configurable
  per-rule via `policies.json`
- **Live policy reload**: edits picked up within 5s, no restart
- **Fully cross-platform**: Linux + Windows both green in CI
- **Air-gapped CI job**: iptables blocks non-loopback egress and the daemon
  still passes its full test suite — proves the D10 "no calls home" invariant

**Not yet built** (Stage 2): CEL policy evaluator, TLS on ingest port,
`/metrics` endpoint, live reload of signal thresholds, multi-tenant quotas,
Composer publishing.

## 30-second local demo

```powershell
# Terminal 1 — start the daemon
cd C:\athar\daemon
$env:ATHAR_DATA_DIR = "C:\athar\refapp-data"
cargo run --release --bin athar-daemon

# Terminal 2 — drive it with the reference application
cd C:\athar
php refapp\bin\simulator.php --scenario=all --verbose
php refapp\bin\evaluate-demo.php     # synchronous decision demo
php refapp\bin\verify.php --data-dir=C:\athar\refapp-data
```

Then poke at the state:

```
daemon\target\release\athar policy show C:\athar\refapp-data
daemon\target\release\athar decision recent C:\athar\refapp-data\state\decisions.db --limit 20
daemon\target\release\athar audit verify C:\athar\refapp-data\audit
```

For integrating into YOUR existing app, see [`docs/INTEGRATE.md`](./docs/INTEGRATE.md).
