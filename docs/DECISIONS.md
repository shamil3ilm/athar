# Decisions — D1 through D14

Status legend: **RECOMMENDED** (awaiting sign-off) · **ACCEPTED** · **REJECTED** · **DEFERRED**.

**All decisions D1–D14 ACCEPTED on 2026-09-13.** Appendix A step 1 (resolve D2/D3/D4) satisfied. V0 code begins.

Every decision has: the question, the recommendation, the reasoning (including what was rejected and why), and what accepting it locks in.

---

## D1 — Daemon deployment unit

**Question.** Sidecar per pod vs. DaemonSet per node.

**Recommendation.** DaemonSet per node, with tenant isolation enforced inside the daemon (§4.6). Fall back to sidecar per pod for T2/T3 customers who require process-level isolation contractually.

**Reasoning.** A DaemonSet amortises the daemon's fixed memory cost (`PERF-6`, default 512 MB) across all replicas on a node; sidecar-per-pod pays it per replica and does not scale to high-density clusters. Cross-tenant isolation is not the container boundary in either case — it is the storage-layer and key-layer isolation required by `PRI-14`/`PRI-15`. If those are correct, DaemonSet is safe; if they are not, sidecar does not save you.

**Rejected.** Sidecar-per-pod as the default (too expensive at scale). Central cluster daemon (violates `INV-11` locality; a network hop is not a Unix socket).

**Locks in.** Kubernetes-first packaging; a documented "sidecar mode" build flag; documented isolation guarantees the customer's security review can read.

---

## D2 — Daemon implementation language

**Question.** Rust vs. Go vs. other.

**Recommendation.** **Rust**. Go acceptable as a second-choice fallback if the initial engineering team lacks Rust experience.

**Reasoning.** The daemon is a long-lived, security-critical, resource-bounded process handling payments-shaped data. Requirements that push toward Rust:

- Predictable latency without GC pauses in the hot path (`PERF-3`, hard 10 ms deadline).
- Memory safety without a runtime, which shrinks the supply-chain surface for T2/T3.
- Single static binary shippable to air-gapped hosts (T4) with no libc surprises.
- Mature, audited crypto ecosystem (`ring`, `RustCrypto`) for the audit chain (`SEC-11`).
- WASM sandboxing via `wasmtime` or CEL evaluators for policy (`SEC-6`) are first-class.
- Reproducible builds are easier (`SEC-7`).

Go trades GC pauses for developer velocity; for a system whose whole point is not to introduce latency, that trade is wrong here.

**Rejected.** C/C++ (memory-safety cost too high for a security product). JVM/CLR (runtime footprint, cold start, GC). Node/Python (unsuitable for a resource-bounded daemon).

**Locks in.** Cargo workspace, `cargo-deny` and `cargo-audit` in CI, `cargo-vet` for supply-chain review, SBOM via `cargo-sbom`, WASM policy engine or CEL-in-Rust.

---

## D3 — First shim / SDK target

**Question.** First host framework for the shim.

**Recommendation.** **PHP / Laravel** (Laravel adapter first, with a generic PSR-15 adapter alongside). Second: **Node/Express**. Third: **Python/Django**.

**Reasoning.** The spec's vocabulary (`Runtime::enable();`, controllers, jobs, workers, listeners, observers, middleware, ORM model hooks, scheduled commands) is Laravel-shaped. Confirmed by working directory context (masaar-erp-backend, live.onlinecheckwriter.com — both Laravel). PHP-FPM's worker-per-request model is also the hardest of the mainstream shims:

- No long-lived background thread — the shim must flush at request end.
- Startup budget (`PERF-8`, 25 ms) applies per request, not per boot.
- Auto-instrumentation must attach via Composer's autoloader without editing controllers.

Proving the invariants in PHP/FPM proves them everywhere. Doing Node first would let the shim rely on a background thread that PHP does not have — the design would then not port cleanly.

**Rejected.** Node-first (easier but not diagnostic). Python-first (async story is uneven across ASGI/WSGI). Java-first (bigger runtime, harder cold start, misaligned to spec vocabulary).

**Locks in.** Composer package `athar/shim-laravel` and `athar/shim-psr15`. Service provider auto-discovery. FFI or Unix-socket bridge to the daemon (recommend Unix socket for portability; FFI adds a build dependency).

---

## D4 — Local storage engine

**Question.** Storage backend for evidence + state + audit chain.

**Recommendation.** Split store per `OPS-9`:

- **State store:** SQLite in WAL mode. Tables for lifecycles, identities, endpoints, decisions, signals, config. Indexed for time and by lifecycle_id / tenant_id.
- **Evidence log:** append-only segmented log, one file per segment, segments closed on size or time boundary, hash-chained within segment and across segments (`SEC-11`). Compressed at rest (zstd), encrypted at rest with tenant key.
- **Audit chain:** logically part of the evidence log but with mandatory signing at each segment close (`SEC-12`) and retention floor (`PRI-13`).

**Reasoning.** SQLite is battle-tested, embedded, crash-safe with WAL, and its single-file backup story matches T4. It is inappropriate for the raw evidence stream (row-per-event write amplification, index overhead), which is why the evidence log is separate and append-only. Keeping them separate also makes the invariant "no code path may edit an evidence record" enforceable structurally.

Crash consistency between the two is handled by treating the evidence log as truth and replaying deltas into the state store on recovery (`V-3` in REVIEW.md).

**Rejected.** RocksDB (bigger dependency, harder to reason about crash recovery for this workload). LMDB (excellent but mmap semantics interact badly with quota enforcement). Customer's DB (violates `INV-2`). Any networked store (violates T4).

**Locks in.** `rusqlite` (or `sqlx` in offline mode). A custom segmented-log crate under `daemon/storage/`. zstd + AES-GCM for at-rest.

---

## D5 — Signing / key custody for the audit chain

**Question.** Where do audit-chain signing keys live?

**Recommendation.** Tiered:

- **T1 / T2:** OS keystore — macOS Keychain, Windows DPAPI, Linux kernel keyring or gnome-keyring. Keys never in a file readable by the daemon user.
- **T3 / T4:** PKCS#11 — HSM or software token (e.g. SoftHSM for T4 without an HSM). Keys never leave the module.
- **Development:** file-backed keys under `daemon/keys/`, but daemon MUST refuse to start with `env=production` and file-backed keys.

**Reasoning.** The audit chain's value is proportional to the difficulty of forging a signature. A key readable by root on the daemon host is only marginally better than no signing at all. OS keystore is a large improvement for low operational cost; PKCS#11 is the auditor-defensible option for regulated tiers.

**Rejected.** Vault-based (external service, violates T4). File-only (indefensible for T2+). Cloud KMS (violates `PRI-4`).

**Locks in.** A `KeyProvider` trait with three implementations. Signing algorithm: Ed25519 (fast, small, no parameter choices to get wrong).

---

## D6 — License / packaging model per tier

**Question.** How is licensing structured, and what happens on expiry?

**Recommendation.** Per-node license (matches DaemonSet unit from D1). Signed offline license file for T4. On expiry:

- Non-essential features (discovery, dashboard, deep analysis) disable immediately.
- **Enforcement continues for 30 days** past expiry, then requires an operator-acknowledged extension.
- Audit chain retention continues unconditionally until manually purged.
- The runtime never phones home to verify a license.

**Reasoning.** Silently disabling enforcement on license expiry would be a critical safety regression (a customer whose subscription lapses could suddenly stop blocking fraud). The 30-day grace balances vendor commercial reality against customer safety. Retention is untouched because destroying audit evidence on expiry is unacceptable regardless of contract state.

**Rejected.** License-checked-per-request (perf + reliability disaster). License-forces-egress-check (violates `PRI-1`).

**Locks in.** License file schema, signature key rotation policy, grace-period semantics documented in the customer contract.

---

## D7 — Confidence calibration thresholds per correlation tier

**Question.** What confidence value should each correlation tier produce?

**Recommendation.** In V0, ship the spec's stated bands as *nominal* values, and mark all confidence outputs as `calibration: NOMINAL_UNVALIDATED` until the golden corpus reaches a documented minimum size. Post-Stage-2, calibrate empirically: for a cohort labelled confidence `c`, the observed correct-resolution rate should be within ±0.05 of `c` (Brier score ≤ 0.1 per tier). Publish reliability curves per release.

**Reasoning.** Confidence values chosen by intuition and never measured are worse than no confidence values, because they look like data. Marking them as unvalidated in V0 keeps the discipline honest.

**Locks in.** A `calibration` field on every confidence-bearing record. A calibration test class (§14.1). A public calibration report per release.

---

## D8 — Default retention per record class per tier

**Question.** How long is each record class kept by default?

**Recommendation.**

| Class | T1 default | T2 default | T3 default | T4 default |
|---|---|---|---|---|
| Raw evidence log (correlated, uneventful) | 30 days | 90 days | 90 days | operator-set |
| Raw evidence log (lifecycle-linked, open) | until closure + 90 | until closure + 365 | until closure + 365 | operator-set |
| Decision records | 365 days | 7 years | 7 years | operator-set |
| Audit chain | 7 years | 7 years | 7 years | operator-set |
| Coverage gaps | same as evidence | same as evidence | same as evidence | operator-set |
| Signals (unlinked to enforced decision) | 30 days | 90 days | 90 days | operator-set |

All defaults are configurable. Retention MUST be logged in the audit chain on any change.

**Reasoning.** Numbers align to the strongest common regulatory floors (PCI DSS: 1 year online + 1 year retrievable; SOX: 7 years; GDPR: erasure supported via `PRI-16`). T4 defers to operator because environment-specific rules dominate.

**Locks in.** Default config templates in `daemon/config/defaults/`. Retention change requires operator role + audit event.

---

## D9 — Reason-code taxonomy

**Question.** What is the shape of the public reason-code enumeration?

**Recommendation.** Namespaced: `<CATEGORY>_<CONDITION>`. Categories: `IDENTITY`, `AUTHZ`, `VELOCITY`, `TARGET`, `SEQUENCE`, `SESSION`, `DEVICE`, `NETWORK`, `AMOUNT`, `RECONCILIATION`, `RUNTIME`. Each code has:

```
id                    IDENTITY_WEAK_RESOLUTION
version               1
category              IDENTITY
description           <one paragraph, human-readable>
when_emitted          <observable trigger>
policy_hint           <what an author typically does with it>
regulator_mapping     { pci: "...", psd2: "...", ... }
introduced            2026-01-01
deprecated            null
```

Published as a versioned JSON manifest (`athar-reason-codes-v1.json`) alongside human documentation. Codes are additive; deprecation is a two-stage flag (marked deprecated, then removed no earlier than 12 months later).

**Locks in.** A stability commitment — reason codes are part of the customer's integration surface (SIEMs, dashboards, support tooling).

---

## D10 — Control plane in first release

**Question.** Does the runtime ship with any control plane in the first release?

**Recommendation.** **No.** First release is pure-local. Updates and policy bundles are signed tarballs the customer fetches (or is handed via sneakernet in T4).

**Reasoning.** The `LOCAL_ONLY` invariant (`PRI-3`) is strongest when there is no code path to abuse. Every control-plane control (auth, ratelimit, replay resistance, tenant isolation of the plane itself) is a new attack surface. Deferring the control plane until Stage 3 costs a small amount of vendor operational friction and buys a much stronger security story for the wedge.

**Rejected.** Ship-control-plane-day-one (perceived vendor efficiency, real security cost). Ship-with-a-disabled-plane (`PRI-3` requires compile-gating; disabled-in-config is not the same posture).

**Locks in.** Update distribution model: signed tarball + `athar update apply <path>` CLI verb. No outbound sockets in the binary at all.

---

## D11 — Policy evaluation engine (NEW)

**Question.** What language/engine evaluates policies?

**Recommendation.** **CEL** (Common Expression Language) via a Rust CEL evaluator, with a step limit and no I/O. Custom deterministic operators only.

**Reasoning.** CEL is Turing-*incomplete* (guaranteed to terminate), sandboxed by construction, has a formal semantics, is used at scale (Envoy, Kubernetes admission), and has type checking. Rego (OPA) is more expressive but also more complex and has a heavier evaluator. A custom DSL is a maintenance burden and a supply-chain surface. WASM as a policy runtime is powerful but overkill — customer policies are predicates, not programs, and WASM invites Turing-complete policies with unbounded surprises.

**Rejected.** Rego (heavier, less predictable evaluation cost per rule). Lua (mutable state, non-deterministic). WASM (too general).

**Locks in.** `cel-rs` or equivalent. Policy schema defined as CEL over the canonical event + signal set. Policy compilation happens once; evaluation is bytecode.

---

## D12 — Secret scanner for privacy conformance (NEW)

**Question.** Which scanner enforces criterion 12 ("no `C5` on disk")?

**Recommendation.** Two-layer:

- **Layer 1** (deterministic): regex bank derived from the `PRI-9` denylist, run over every closed evidence segment. Matches block the release.
- **Layer 2** (entropy): trufflehog or gitleaks over the data directory, matches trigger an operator alert but do not fail the build (entropy detectors false-positive on IDs).

**Reasoning.** Deterministic must be gate-level; entropy is advisory. A single-scanner approach either gates on false positives (unshippable) or misses real leaks.

**Locks in.** `scripts/scan-secrets.sh` and a CI job. The regex bank is versioned in-repo.

---

## D13 — Reference application for benchmarking (NEW)

**Question.** What application is the performance and V0-criteria harness run against?

**Recommendation.** A synthetic payments-shaped Laravel application in-repo (`bench/refapp-laravel/`), scripted with `k6` or `wrk` and orchestrated by `bench/run.sh`. It MUST:

- Contain the operations named in the spec (payment create, retry, cancel).
- Emit a queue job and a worker step.
- Make one outbound HTTP call to a local fake processor.
- Have a webhook endpoint the fake processor calls back.
- Have a database with an accounts and payments table.

**Reasoning.** Benchmarks that are not reproducible are decoration. Two customers' apps produce two irreconcilable numbers. The reference app is the *only* place `PERF-1`–`PERF-4` are contractually measured.

**Locks in.** Reference app is versioned; performance numbers are always tagged with refapp version.

---

## D14 — Clock source (NEW)

**Question.** How does the runtime obtain time?

**Recommendation.**

- **`timestamp`** (event occurrence): wall clock at the point of observation.
- **`received_at`**: wall clock at the daemon on ingest.
- **`clock.monotonic_seq`**: process-monotonic counter per host, reset on daemon restart, so ordering within a host is stable.
- **`clock.skew_estimate_ms`**: rolling estimate of wall-clock skew relative to monotonic. On backward wall-clock jump > 100 ms, the shim MUST NOT emit events with the new wall clock until the skew is characterised (one-shot pause, ≤ 1 second) and MUST record a `clock_correction` event with before/after values.
- NTP is not assumed; behaviour must be correct without it.

**Locks in.** Clock module in the daemon with an injectable trait for tests (needed for `V-5`).

---

## Decisions summary

| ID | Status | Choice |
|---|---|---|
| D1  | ACCEPTED | DaemonSet per node, tenant isolation in-daemon |
| D2  | ACCEPTED | **Rust** |
| D3  | ACCEPTED | **PHP / Laravel** first |
| D4  | ACCEPTED | **SQLite WAL** (state) + append-only segmented log (evidence + audit) |
| D5  | ACCEPTED | OS keystore T1/T2, PKCS#11 T3/T4, Ed25519; dev mode may use file keys |
| D6  | ACCEPTED | Per-node license; 30-day enforcement grace on expiry; retention unaffected |
| D7  | ACCEPTED | V0 confidence marked `NOMINAL_UNVALIDATED`; empirical calibration post-Stage-2 |
| D8  | ACCEPTED | Tiered retention defaults per §D8 table |
| D9  | ACCEPTED | Namespaced `<CATEGORY>_<CONDITION>`, versioned JSON manifest, 12-month deprecation |
| D10 | ACCEPTED | **No control plane in first release**; signed tarball updates |
| D11 | ACCEPTED | **CEL** for policy evaluation |
| D12 | ACCEPTED | Two-layer scan: regex gates release, entropy scan advisory |
| D13 | ACCEPTED | Synthetic Laravel refapp in-repo (`bench/refapp-laravel/`) |
| D14 | ACCEPTED | Wall clock + monotonic seq + skew estimate; injectable clock trait |

Next steps per Appendix A:
- Step 2 (schema + validator + fixtures): **partially done** — schema and fixtures exist; validator binary is pending.
- Step 3 (shim skeleton): scaffolded — PHP/Laravel package layout in `shim/`.
- Step 4 (daemon skeleton): scaffolded — Rust workspace in `daemon/`.
- Step 5 (resource governor + dead-man's switch): pending, must precede feature build.
