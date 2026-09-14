# MASTER IMPLEMENTATION SPECIFICATION — v1

**Privacy-First Local Application Security, Trust, Lifecycle & Intelligence Runtime**

| | |
|---|---|
| **Document type** | Normative implementation specification + build prompt |
| **Supersedes** | Initial implementation prompt (working draft) |
| **Status** | Draft for review — Sections marked `DECISION REQUIRED` block implementation |
| **Audience** | Implementing engineers, or an AI coding agent executing the build |
| **Independence** | New standalone product. Inherits **no** architecture, naming, schema, or dependency from Taxfly or any prior system. |

---

## 0. How to read this document

### 0.1 Normative language

| Term | Meaning |
|---|---|
| **MUST** / **MUST NOT** | Absolute requirement. A build that violates it is non-conforming. |
| **SHOULD** / **SHOULD NOT** | Strong recommendation. Deviation requires a recorded rationale in `docs/decisions/`. |
| **MAY** | Optional. |
| **DECISION REQUIRED** | An open choice that must be resolved by a human before the affected code is written. |

### 0.2 Requirement identifiers

Every normative requirement carries a stable ID:

- `INV-n` — architectural invariant (never violable)
- `PRI-n` — privacy requirement
- `SEC-n` — security requirement
- `PERF-n` — performance requirement
- `MOD-n` — domain model requirement
- `INT-n` — integration requirement
- `OPS-n` — operational requirement

IDs are permanent. If a requirement is withdrawn, mark it `WITHDRAWN` rather than reusing the number. Every requirement **MUST** map to at least one automated test, referenced in the conformance matrix (§14.4).

### 0.3 What changed from the working draft, and why

The draft was a strong conceptual charter but not yet buildable. The following gaps were closed in this, the first specification release:

| # | Gap in the draft | Resolution in v1 |
|---|---|---|
| 1 | Document truncated mid-sentence at §30 (target tracking). | Completed, plus the missing downstream sections (policy, decisions, alerts, storage, CLI, testing). |
| 2 | "Build the smallest complete vertical slice first" — the slice was never defined. | §12 defines V0 exactly, with binary acceptance criteria and an explicit *not in V0* list. |
| 3 | No threat model. The runtime is a privileged observer of payments and credentials, i.e. a high-value target, but the draft never treated it as one. | §8 adds an attacker-centric threat model with per-threat controls. |
| 4 | "Must not harm the application" stated as a principle with no measurable budget, so it cannot be tested or enforced. | §7 sets numeric budgets, measurement method, and an automatic self-disable circuit breaker. |
| 5 | No technology decisions; no statement of deployment topology. In-process-only designs cannot meet the stated performance invariants under PHP/CGI-style request models. | §6 specifies the split-plane topology (in-process shim + out-of-band daemon) and marks language/storage as `DECISION REQUIRED` with recommendations. |
| 6 | "Tamper-evident audit evidence" asserted without a mechanism. | §8.6 specifies a hash-chained, signed, externally-anchorable audit log. |
| 7 | Signals defined; policy evaluation, decision format, and alerting left undefined. | §9 adds the policy model, the explainable decision record, and alert lifecycle. |
| 8 | No storage model, retention model, or capacity bound. Unbounded local storage is itself a way to harm the host application. | §6.5 and §4.5 add the storage engine contract, quotas, and retention/eviction ladder. |
| 9 | No testing strategy for claims that are fundamentally about *uncertainty* (correlation confidence, reconciliation). | §14 adds golden-corpus replay, property-based tests, fault injection, and calibration tests. |
| 10 | Conceptual sections repeated the same idea in several places; ASCII diagrams were corrupted. | Deduplicated, diagrams rebuilt, cross-referenced instead of repeated. |
| 11 | No failure-mode catalogue: what the runtime does when *it* is broken. | §7.5 degradation ladder + §10.6 instrumentation failure semantics. |
| 12 | No versioning/compatibility contract for the runtime's own event schema and storage format. | §5.9 schema evolution rules. |

Everything conceptual in the draft is preserved. Nothing was removed on grounds of difficulty.

---

## 1. Product definition

### 1.1 Mission

Understand the application locally, then verify and protect it locally.

### 1.2 One-sentence scope

A runtime that installs inside a customer's application environment, observes what the application actually does, resolves who really did it, assembles those observations into business lifecycles that reach a known closure, detects abuse, enforces policy, and preserves tamper-evident evidence — without sending customer data anywhere and without the customer's users ever feeling that it is running.

### 1.3 Founding principles

1. Never blindly trust.
2. Never lose lifecycle or evidence truth.
3. Never make the customer's application pay the performance cost of our intelligence.
4. Technical context is not business identity.
5. A shared endpoint is not a shared identity.
6. One business lifecycle can cross many applications, platforms, modules, services, versions, repositories and endpoints.
7. One technical component can participate in many business lifecycles.
8. Customer data stays inside the customer's controlled environment by default.
9. **(new)** Uncertainty is a first-class result. The runtime must be able to say `UNKNOWN` and remain useful.
10. **(new)** The runtime is part of the attack surface it defends. It must be hardened as a target, not only as a sensor.
11. **(new)** Every automated decision must be reconstructable from stored evidence months later, by someone who was not present.

### 1.4 Product evolution

```
Stage 1  Local Event & Context Runtime      <- foundation
Stage 2  Application Security               <- commercial wedge
Stage 3  Lifecycle Intelligence
Stage 4  Application Intelligence
Stage 5  Advanced Security
Stage 6  AI / Agent Security
```

Stage *n+1* **MUST NOT** begin until stage *n* meets its exit criteria (§13).

### 1.5 Target environments

Ordered by increasing constraint. Earlier stages must not foreclose later ones.

| Tier | Environment | Added constraints |
|---|---|---|
| T1 | Normal commercial, internet-connected | Baseline |
| T2 | Regulated (finance, health) | Retention controls, audit export, role-separated access |
| T3 | Government / private cloud | No third-party egress at all, signed artifacts, reproducible builds |
| T4 | Offline / air-gapped | No control plane, sneakernet update path, offline license, no telemetry of any kind |

`OPS-1` — The runtime **MUST** be fully functional in T4 with no degradation of security enforcement. Only update freshness, not capability, may degrade.

### 1.6 Explicit non-goals

The following are **out of scope permanently** unless a written decision reverses it:

- Being a general-purpose APM, tracing, or log aggregation product.
- Being a WAF or network-layer appliance.
- Shipping customer data to a vendor-operated analytics service.
- Blocking traffic by default. Enforcement is opt-in, per-policy, and explicitly configured (§2.1).
- Requiring the customer to restructure their application, schema, or identifiers.

---

## 2. Non-negotiable architectural invariants

These are system invariants, not suggestions. Each has a stated verification method; if it cannot be tested, it is not yet a real invariant.

### 2.1 Primary application independence

`INV-1` — The customer's application workflow is always primary; the runtime is always secondary.

`INV-2` — Observation, capture, correlation, discovery, analysis, reconciliation, storage, recovery, and intelligence **MUST NOT** introduce user-perceivable or materially harmful impact on the primary application under supported operating conditions (defined in §7.1).

`INV-3` — Only explicitly configured **enforcement** policies may execute on the application's critical path. Everything else is out-of-band. A policy that has not been explicitly promoted to enforcement mode **MUST** run in observe-only mode.

`INV-4` — Any runtime operation on the critical path **MUST** have a hard deadline and a defined fail-open behaviour. Exceeding the deadline **MUST** abandon the runtime operation, not delay the request.

The runtime **MUST** actively protect against each of the following, and **MUST** have a named control and a load test for each:

| Pressure | Control | §ref |
|---|---|---|
| CPU contention | Bounded worker pool, nice/priority, CPU quota | 7.3 |
| Memory pressure | Fixed-size ring buffers, hard byte caps, spill-to-disk | 7.3 |
| Database contention | Runtime never shares the application's DB connection pool or transaction | 6.5 |
| I/O pressure | Batched appends, write coalescing, disk quota + eviction | 6.5 |
| Network overhead | No synchronous egress; local-only by default | 4.2 |
| Queue contention | Runtime uses its own transport, never the app's queue | 6.2 |
| Worker exhaustion | Dedicated worker pool with hard ceiling | 7.3 |
| Startup spikes | Lazy init, staged warm-up, no blocking discovery at boot | 7.4 |
| Discovery spikes | Rate-limited, off-peak-biased, cancellable | 11.4 |
| Recovery spikes | Throttled replay with backpressure from live traffic | 6.6 |
| Backlog catch-up spikes | Catch-up rate is a function of measured headroom | 7.3 |
| Reconciliation spikes | Batched, windowed, deferrable to P2 | 5.11 |
| Deployment spikes | Version negotiation without full re-discovery | 5.9 |
| Policy reload spikes | Atomic swap of precompiled policy; no per-request compilation | 9.3 |
| Model loading spikes | Models load in the daemon only, never in the app process | 6.2 |

**Verification:** a soak test that runs the reference application with the runtime disabled, then enabled, and compares p50/p95/p99 latency and throughput against the budgets in §7.1.

### 2.2 No performance debt transfer

`INV-5` — The runtime **MUST NOT** resolve its own backlog by consuming unbounded customer resources. Backlog is the runtime's problem, and the correct resolution is to shed its own work.

Priority ladder under resource constraint:

| Priority | Class | Behaviour when constrained |
|---|---|---|
| **P0** | Customer workflow | Always continues. Never blocked by runtime. |
| **P0** | Mandatory security enforcement (explicitly configured) | Continues, within its deadline (`INV-4`) |
| **P1** | Security integrity (signal capture for enforced policies) | Continues |
| **P1** | Critical evidence (audit chain, decision records) | Continues |
| **P1** | Lifecycle integrity (state transitions, closure) | Continues |
| **P2** | Correlation | Reduce fidelity / defer |
| **P2** | Reconciliation | Reduce frequency / defer |
| **P3** | Deep analysis, discovery, enrichment | Defer |
| **P4** | Optimisation, compaction, model refresh | Stop |

`INV-6` — Customer application correctness takes priority over secondary intelligence completeness. When the runtime sheds work, it **MUST** record that it did so (§5.12 `coverage_gap` record) rather than silently producing a thinner picture. Shedding is a fact about the evidence, and incomplete evidence that knows it is incomplete is safe; incomplete evidence that believes it is complete is dangerous.

### 2.3 Truthfulness invariants

`INV-7` — The runtime **MUST NOT** fabricate an event, an actor, a state transition, or a relationship that it did not observe or lawfully infer. Inference **MUST** be labelled as inference and carry a confidence value and the evidence it rests on.

`INV-8` — The runtime **MUST NOT** convert uncertainty into certainty at any layer boundary. A `PROBABLE` identity resolution that feeds a policy decision remains `PROBABLE` in the decision record.

`INV-9` — Original evidence is immutable. Corrections are appended as new records that supersede, never as edits. No code path may update or delete an evidence record except the retention process (§4.5), which deletes whole records and leaves a tombstone in the audit chain.

`INV-10` — The runtime **MUST** be able to answer "why did you decide that?" for any decision it made, from stored data alone, without access to the live application.

---

## 3. Deployment topology

### 3.1 The split-plane requirement

`INV-11` — The runtime **MUST** be split into two planes with a hard boundary. In brief: an in-process **shim** that captures, redacts, and enqueues; and an out-of-band **daemon** that normalizes, correlates, resolves identity, tracks lifecycles, evaluates policy, and stores. Communication is local (Unix socket or shared memory), non-blocking, bounded, and lossy-by-design under pressure.

**Rationale.** The stated invariants (§2.1, §2.2) cannot be met by a purely in-process design. Any meaningful correlation, lifecycle, and detection work performed inside the request path directly consumes the application's CPU, memory, and worker slots, and in worker-per-request models (PHP-FPM, CGI, short-lived serverless) there is no background thread in which to hide it. The shim must therefore do the least possible work — capture, redact, hand off — and everything expensive must live in a separate process with its own resource envelope that the OS can bound and the operator can starve without harming the application.

`INV-12` — The in-process shim **MUST** function correctly when the daemon is absent, stopped, or unreachable. Its failure mode is bounded local buffering, then drop-with-`coverage_gap`. It **MUST NOT** block, retry synchronously, or raise into application code.

`INV-13` — The shim **MUST NOT** load ML models, parse policy source, open network sockets, or perform disk I/O beyond appending to its bounded buffer.

### 3.2 Supported host models

The design **MUST** accommodate all of:

| Host model | Implication |
|---|---|
| Long-lived process (Node, Go, JVM, Python ASGI) | Shim may use a background thread + shared ring buffer |
| Worker-per-request (PHP-FPM, CGI) | Shim must flush at request end; no background thread available |
| Containerised, many replicas | Daemon per node (sidecar or DaemonSet), not per replica |
| Serverless / ephemeral | Shim must flush before freeze; daemon must be remote-but-local-network, or degrade to buffered export |
| CLI / queue worker processes | Same shim, different entry point adapters |

`DECISION REQUIRED — D1:` daemon deployment unit for containerised customers: sidecar per pod vs. DaemonSet per node. Recommendation: **DaemonSet per node** for resource efficiency, with tenant isolation enforced inside the daemon (§4.6), falling back to sidecar where the customer requires process-level isolation.

### 3.3 Technology decisions

`DECISION REQUIRED — D2: daemon implementation language.`
Recommendation: a compiled, memory-safe language with predictable latency and no GC pauses in the critical path — **Rust** preferred, **Go** acceptable.

`DECISION REQUIRED — D3: first SDK/shim target.`
Recommendation: **PHP / Laravel** (Laravel adapter + generic PSR-15 adapter). Second: Node/Express. Third: Python/Django.

`DECISION REQUIRED — D4: local storage engine.`
Recommendation: **SQLite in WAL mode** for indexed state plus an **append-only segmented log** for raw evidence. Explicitly rejected: the customer's own database, and any networked datastore.

`DECISION REQUIRED — D5:` signing and key custody model for the audit chain — software keys vs. OS keystore vs. HSM/PKCS#11 for T2/T3.

---

## 4. Privacy model

### 4.1 Default posture

`PRI-1` — Customer data **MUST NOT** leave the customer environment by default.

`PRI-2` — The deny-by-default egress rule applies to all of: request payloads, response payloads, logs, traces, metrics, crash reports, diagnostic bundles, security events, audit data, evidence, backups, analytics, identity information, and any derived artifact (embeddings, hashes of identifiers, aggregate counts, model gradients).

`PRI-3` — The runtime **MUST** ship with a `privacy.mode` setting whose default is `LOCAL_ONLY`. In `LOCAL_ONLY`, the egress layer is compiled/feature-gated off where the platform allows.

### 4.2 Control plane separation

If a control plane exists, it is a **one-way, pull-only** channel carrying non-customer data (updates, policies, schemas, model artifacts, license metadata).

`PRI-4` — The runtime **MUST** remain fully functional when the control plane is unreachable, indefinitely.
`PRI-5` — All control-plane artifacts **MUST** be signature-verified before use.
`PRI-6` — Control-plane fetches **MUST NOT** carry a payload beyond an opaque install ID and the current artifact versions.

### 4.3 Data classification

Every field in every stored record **MUST** carry a class:

| Class | Examples | Default handling |
|---|---|---|
| `C0-PUBLIC` | Route template, HTTP method, framework version | Stored in clear |
| `C1-TECHNICAL` | Endpoint ID, service ID, deployment ID, latency | Stored in clear |
| `C2-PSEUDONYMOUS` | Internal user ID, session ID, device ID | Stored in clear, access-controlled |
| `C3-PERSONAL` | Email, phone, name, IP address | Redacted or tokenised by default |
| `C4-SENSITIVE` | Payment instrument, government ID, health, biometric | Never stored raw; tokenised or dropped |
| `C5-SECRET` | Passwords, tokens, keys, session cookies, auth headers | **MUST** be dropped at capture time in the shim |

`PRI-7` — `C5` detection and removal **MUST** occur in the in-process shim, before the data crosses any process boundary or touches durable storage.
`PRI-8` — Payload capture is **opt-in per field or per path**, never "capture everything and filter later".
`PRI-9` — A default denylist (`authorization`, `cookie`, `set-cookie`, `x-api-key`, `password`, `secret`, `token`, `card`, `cvv`, `pan`, `iban`, `ssn`, and configurable additions) **MUST** be applied even when payload capture is enabled.

### 4.4 Tokenisation

`PRI-10` — Correlatable-but-unreadable identifiers **MUST** use a keyed, tenant-scoped, rotating token (`HMAC(tenant_key, namespace || value)`), not a bare hash.

### 4.5 Retention, quotas, and eviction

`PRI-11` — Every record class **MUST** have a configured retention period. No "keep forever" default.
`PRI-12` — The runtime **MUST** operate within a configured disk quota.

Eviction ladder when the quota is approached:

```
1. P4 optimisation artifacts, caches, compacted intermediates
2. Raw payload captures beyond their minimum retention
3. Closed lifecycles past retention
4. Correlated-but-uneventful technical spans
5. Open lifecycles older than the hard ceiling  -> closed as CLOSED_WITH_UNCERTAINTY first
6. STOP capture; emit coverage_gap; raise a critical operator alert
```

`PRI-13` — Audit-chain records and decision records for enforced policies **MUST NOT** be evicted before their retention period.

### 4.6 Multi-tenancy and isolation

`PRI-14` — `tenant_id` is mandatory on every record. Cross-tenant reads **MUST** be impossible at the storage layer.
`PRI-15` — Tenant encryption keys **MUST** be distinct.

### 4.7 Right to erasure

`PRI-16` — Subject erasure **MUST** be supported without breaking the audit chain: erase the subject's `C3`/`C4` attributes in place with a tombstone, retain the chain hash, and record the erasure itself as an auditable event.

---

## 5. Domain model

### 5.1 Security philosophy: evidence, not assertion

`SEC-1` — The runtime **MUST NOT** treat a value as true merely because it exists, came from a client, is an identifier, came from a known endpoint, was recently observed, is the latest value, or matches a name/email/phone.

Pipeline (each stage independently testable): Validation → Normalization → Identity → Authentication → Authorization → Context → Trust → Correlation → Detection → Policy → Decision → Action → Audit.

`SEC-2` — Every stage **MUST** be able to emit a partial result with `UNKNOWN` fields rather than failing the pipeline.

### 5.2 Identity is an evidence graph

`MOD-1` — An identifier is evidence, not proof.

`MOD-2` — Identity **MUST** be namespace-scoped (`tenant_id · issuer_id · identity_namespace · entity_type · entity_id · version · valid_from · valid_until`).

`MOD-3` — `123@Bank-A` and `123@Bank-B` **MUST NOT** resolve to the same entity without explicit, configured linkage evidence.

Resolution statuses: `VERIFIED · PROBABLE · POSSIBLE · UNKNOWN · CONFLICTED · REVOKED`.

`MOD-4` — Conflicting evidence **MUST** be retained, both sides, with their sources and timestamps.

`MOD-5` — Identity resolution **MUST** be reproducible.

### 5.3 Authentication vs. authorization

Separate concerns and separate records. Each authentication record captures: `method · strength · issuer · credential_id · authentication_time · reauthentication_time · session_id · context · result`.

`SEC-3` — Authentication strength and recency are inputs to policy.

`SEC-4` — Client-provided roles, scopes, and authorization claims **MUST NOT** be trusted without verification. An unverified claim is recorded as `claimed_*` and never as the effective value.

### 5.4 Canonical event model

`MOD-6` — Every integration **MUST** normalize into the canonical event (see `schema/event.v1.0.json`).

`MOD-7` — Unknown is expressed as `null` plus a resolution status; never as a plausible default, an empty string, or `"unknown"` cast as a real ID.

`MOD-8` — `timestamp` (when it happened, per the source) and `received_at` (when the runtime saw it) are **both** mandatory and **MUST NOT** be conflated.

### 5.5 Event truth model

`MOD-9` — The runtime **MUST NOT** conflate *event received* with *business action actually happened*.

Truth stages: `observed · executed · authorized · committed · approved · signed · audited · authoritative · inferred · uncertain`.

`MOD-10` — Promotion between truth stages **MUST** be evidence-backed and recorded as a transition with its justification.

### 5.6 Provenance

Origins: `HUMAN · SYSTEM · EXTERNAL · SCHEDULED · AUTOMATED · UNKNOWN`.
Triggers: `USER_ACTION · API_REQUEST · BACKGROUND_JOB · QUEUE · SCHEDULER · WEBHOOK · POLLING · DATABASE_CHANGE · SYSTEM_RULE · EXTERNAL_PROCESSOR · RECONCILIATION · MANUAL_ADMIN_ACTION · UNKNOWN`.

`MOD-11` — `actor`, `producer`, `authority`, `source`, and `trigger` are five distinct fields and **MUST NOT** be collapsed.

### 5.7 Technical context is not business identity

`MOD-12` — The system **MUST NOT** be modelled as `Application → Service → Endpoint → Lifecycle`. Business graph and execution graph are separate; joined only through evidence and correlation.

`MOD-13` — No execution-graph node inherently owns a business lifecycle.

### 5.8 Shared endpoints

`MOD-14` — Endpoint identity **MUST NOT** imply application identity, module identity, actor identity, or lifecycle identity.

`MOD-15` — Actor, caller, authenticated principal, on-behalf-of, beneficiary, and resource owner are six distinct roles and **MUST NOT** be collapsed into a single `user_id`.

### 5.9 Operation model and versioning

`MOD-16` — An endpoint is not an operation. Operation identity is semantic.

Correlation priority (first match wins, and the matched tier is recorded):

```
1. Explicit operation ID            confidence 1.00
2. Business / transaction ID        confidence 0.95+
3. Parent / causation ID            confidence 0.90+
4. Resource ID                      confidence 0.70–0.90
5. Actor + context                  confidence 0.50–0.75
6. Semantic operation match         confidence 0.40–0.70
7. Temporal proximity               confidence ≤ 0.40
8. UNKNOWN                          no binding
```

`MOD-17` — Every inference **MUST** record both the tier used and a confidence value. Confidence bands **MUST** be calibrated against the golden corpus.

`MOD-18` — Version is provenance, not identity.

`MOD-19` — Unresolved events are stored with `UNKNOWN` bindings and **MUST** be re-resolvable later.

`MOD-20` — Schema evolution: canonical event schema and storage format are versioned independently. The daemon **MUST** read all event schema versions it has ever emitted. The shim **MUST** negotiate its schema version at handshake and degrade gracefully.

### 5.10 Authority model

`MOD-21` — An **authority** is a configured declaration that a named source is definitive for a named attribute or state, within a scope.

`MOD-22` — Where no authority is configured, conflicting values **MUST** produce `CONFLICTED` status, never a silent resolution.

`MOD-23` — An authority's assertion overrides observation for *state*, but **MUST NOT** delete the contradicting observation.

### 5.11 Lifecycle model

`MOD-24` — Lifecycle is a first-class entity, independent of any application, service, or endpoint.

States: `STARTED · PROCESSING · PENDING · SUCCESS · FAILED · CANCELLED · REJECTED · TIMEOUT · EXPIRED · ABANDONED · REVERSED · COMPENSATED · PARTIALLY_COMPLETED · CONFLICTED · UNKNOWN`.

Closure: `OPEN · CLOSED · CLOSED_WITH_EXCEPTION · CLOSED_WITH_UNCERTAINTY`.

`MOD-25` — No lifecycle may remain silently open forever. Every lifecycle type **MUST** have a staleness threshold and a hard ceiling.

`MOD-26` — State transitions **MUST** be validated against a declared state machine per lifecycle type.

Late events: classified as `DUPLICATE · CORRECTION · AMENDMENT · CONFLICT · LATE_INFORMATION · UNKNOWN`.

`MOD-27` — Reopening requires either an explicit authority assertion or an operator action; is itself an audited event.

`MOD-28` — Missing events: recorded as gaps. No synthetic event is created to fill them.

### 5.12 Coverage gaps

`MOD-29` — Whenever the runtime sheds, drops, times out, fails to instrument, or loses data, it **MUST** write a `coverage_gap` record.

`MOD-30` — Any query, dashboard, report, or decision that overlaps a coverage gap **MUST** surface it.

### 5.13 Attempt model

`MOD-31` — A successful business result **MUST NOT** erase the failed attempts.

### 5.14 Transaction and compensation

`MOD-32` — Events **MUST NOT** be deleted because a transaction rolled back.

`MOD-33` — Distributed side effects **MUST** be modelled. Local DB rollback alongside an already-succeeded external charge is a **reconciliation problem**, raised as `UNCOMMITTED_OPERATION`.

`MOD-34` — Compensation is distinct from rollback. A compensating action **MUST** be linked to what it compensates, and both remain in the lifecycle.

### 5.15 Concurrency

`MOD-35` — Equal timestamps do not mean equal operations. Latest-wins is prohibited.

Relationships: `DUPLICATE · SAME_OPERATION · CONCURRENT · COMPETING · DEPENDENT · CAUSING · SUPERSEDING · CORRECTION · RETRY · REDELIVERY · UNKNOWN`.

`MOD-36` — `COMPETING` groups **MUST** raise a signal regardless of outcome.

### 5.16 Reconciliation

Three truth layers: OBSERVATION → EXECUTION → BUSINESS.

Findings: `ORPHAN_EVENT · PHANTOM_BUSINESS_OPERATION · UNOBSERVED_OPERATION · UNKNOWN_SOURCE · UNCOMMITTED_OPERATION · CONFLICTED_STATE`.

`MOD-37` — `PHANTOM_BUSINESS_OPERATION` and `UNKNOWN_SOURCE` are potential security findings.

`MOD-38` — Reconciliation output is a finding with confidence, not a correction applied to history.

`MOD-39` — Reconciliation reads of the customer database **MUST** be read-only, rate-limited, off-peak-biased, use a dedicated connection outside the application's pool, and be abortable within one second of a headroom alarm.

### 5.17 Collision safety and ID coexistence

`MOD-40` — Names are not globally unique. Every identity **MUST** be namespaced.

`MOD-41` — Existing customer identifiers **MUST NOT** be overwritten. Preserve them in `causality.customer_correlation_ids`.

`MOD-42` — No application code should ever be required to understand a runtime ID.

---

## 6. Runtime architecture

### 6.1 Component map

Shim (in-process): Hooks → Capture → Classify/Redact → Frame → Buffer → (optional) Enforcement gate.
Daemon (out-of-band): Ingest → Validate → Normalize → Enrich → Identity/Trust → Context → Correlation → Lifecycle → Detection → Policy → Decision → Action → Audit chain → Storage → Reconciler → Discovery → Scheduler → Query API/CLI/Dashboard/Export.

### 6.2 Transport

`OPS-2` — Shim→daemon transport **MUST** be local (Unix domain socket or shared memory ring buffer), non-blocking, bounded, and lossy-by-design under pressure.
`OPS-3` — The runtime **MUST NOT** use the customer's message queue, job system, database, or HTTP client for its own transport.
`OPS-4` — Framing **MUST** be length-prefixed and versioned, with a handshake that negotiates schema version and capabilities.

### 6.3 Processing model

`OPS-5` — The daemon **MUST** process in bounded stages with explicit queues between them.
`OPS-6` — All processing **MUST** be cancellable within 100 ms of a resource-governor signal.

### 6.4 Idempotency and ordering

`OPS-7` — Ingest **MUST** be idempotent on `event_id`.
`OPS-8` — The runtime **MUST NOT** assume in-order delivery.

### 6.5 Storage contract

`OPS-9` — Storage is split (evidence log / state store / audit chain).
`OPS-10` — The runtime **MUST NOT** write to the customer's database, ever.
`OPS-11` — Storage **MUST** be encrypted at rest with a tenant-scoped key.
`OPS-12` — Storage **MUST** enforce the configured disk quota itself.
`OPS-13` — Backup **MUST** be a file-copy of self-consistent segments, restorable on an air-gapped host.

### 6.6 Crash recovery

`OPS-14` — Recovery is automatic and records a `coverage_gap` for the loss window.
`OPS-15` — Recovery replay **MUST** be throttled by live-traffic headroom.
`OPS-16` — A corrupt segment **MUST** be quarantined, not silently skipped.

---

## 7. Performance requirements

### 7.1 Supported operating conditions and budgets

Supported operating conditions: host has ≥ 10% CPU headroom, ≥ 15% free memory, ≥ 10% free disk at the runtime's quota. Outside these, the runtime degrades per §7.5.

| ID | Budget | Default |
|---|---|---|
| `PERF-1` | Added latency per instrumented request, p50 | ≤ 200 µs |
| `PERF-2` | Added latency per instrumented request, p99 | ≤ 1 ms |
| `PERF-3` | Added latency, p99, with enforcement policy on critical path | ≤ 5 ms, hard deadline 10 ms |
| `PERF-4` | Application throughput reduction | ≤ 2% |
| `PERF-5` | Shim resident memory per process | ≤ 16 MB |
| `PERF-6` | Daemon resident memory | ≤ 512 MB default, hard-enforced |
| `PERF-7` | Daemon CPU, steady state | ≤ 1 core-equivalent or configured cap |
| `PERF-8` | Application startup time added | ≤ 25 ms |
| `PERF-9` | Disk write amplification vs. raw event volume | ≤ 3× |

`PERF-10` — Every budget **MUST** be measured continuously in production by the runtime itself.

### 7.2 Self-measurement

`PERF-11` — Low-cost sampling method whose own cost is within budget.
`PERF-12` — Distinguish runtime cost from host noise; sustained-breach determination requires a window.

### 7.3 Resource governor

`OPS-17` — A governor component **MUST** publish a `pressure_level` (0–4).
`OPS-18` — Every worker pool, queue, and background task **MUST** honour `pressure_level` within 100 ms.
`OPS-19` — Catch-up rates **MUST** be a function of measured headroom.

### 7.4 Startup

`OPS-20` — Shim initialisation **MUST** be lazy.
`OPS-21` — The daemon **MUST** reach "accepting events" before warm-up.

### 7.5 Degradation ladder

| Level | Trigger | Behaviour |
|---|---|---|
| **L0 Normal** | Within all budgets | Full capability |
| **L1 Reduce** | Budget breach, or headroom < 20% | Stop P4. Reduce sampling of P3. Widen batch windows. |
| **L2 Defer** | Sustained breach, or headroom < 10% | Stop P3. Defer P2. Capture continues. |
| **L3 Essential** | Severe pressure | P0/P1 only. Everything else drops with `coverage_gap`. |
| **L4 Safe mode** | `INV-2` cannot be met at L3, or self-check failure | Shim detaches non-enforcement hooks. Daemon idles. Critical alert. |

`INV-14` — Dead-man's switch. If the shim cannot confirm it is within `PERF-1`–`PERF-3` for a sustained window, it self-disables non-enforcement instrumentation without waiting for anyone.

`INV-15` — Any runtime panic in the shim **MUST** be contained and **MUST NOT** propagate into application code. Repeated panic in the same hook permanently disables that hook for the process lifetime.

---

## 8. Threat model and runtime self-security

### 8.1 Assets

Evidence log · audit chain · identity graph · policy set · tenant keys · signing keys · the enforcement decision path itself.

### 8.2 Adversaries

| ID | Adversary | Capability |
|---|---|---|
| A1 | External attacker via the application | Arbitrary requests |
| A2 | Malicious insider (app developer) | Can change application code |
| A3 | Malicious insider (runtime operator) | Can change runtime config and policy |
| A4 | Compromised host / root | Full local control |
| A5 | Compromised supply chain | Malicious update or policy |
| A6 | Curious vendor | Attempt to exfiltrate customer data |

### 8.3 Threats and required controls

Evidence tampering (A2/A3/A4): hash-chained, signed audit log; append-only; deletion leaves tombstones.
Detection evasion by flooding (A1): per-source rate limits; shedding preserves P1 signals; flood is itself a signal.
Identity poisoning (A1): source-weighted evidence; client-supplied evidence never `VERIFIED` alone (`SEC-4`).
Policy tampering (A3/A5): signed policy bundles; policy changes audited; two-person rule for T2/T3.
Malicious update (A5): signature verification; reproducible builds; SBOM.
Exfiltration through runtime (A6/A5): `LOCAL_ONLY` compile-gated (`PRI-3`).
Secrets in evidence (A2 accidental): `C5` drop at capture; denylist; CI secret-scan.
Runtime as foothold (A4): least-privilege daemon user, no shell-out, no dynamic code loading, no inbound listener by default.
DoS via runtime (A1/A3): §7 budgets, governor, dead-man's switch.
Cross-tenant leakage (A1/A3): storage-layer isolation (`PRI-14`), per-tenant keys (`PRI-15`).

`SEC-5` — No network listener by default. CLI/dashboard bind to loopback or Unix socket.
`SEC-6` — Policy is data, evaluated by a sandboxed evaluator with no I/O and a step limit.
`SEC-7` — Every release ships an SBOM and is reproducibly buildable.

### 8.4 Access control for the runtime itself

`SEC-8` — Separate roles: `viewer`, `analyst`, `policy_author`, `operator`, `auditor`. `auditor` cannot alter retention.
`SEC-9` — All privileged actions on the runtime are themselves audited events.

### 8.5 Fail-open vs. fail-closed

`SEC-10` — Default enforcement failure mode is **fail-open** (`INV-1`), explicitly configurable per policy to fail-closed. Chosen mode **MUST** be recorded on every decision.

### 8.6 Tamper-evident audit

`SEC-11` — Hash-chained: each record stores `H(prev_hash || canonical_record)`.
`SEC-12` — Segments **MUST** be signed periodically with a key held outside the runtime's writable storage.
`SEC-13` — `verify` command validates the full chain offline and reports any break's exact record index.
`SEC-14` — Anchoring segment roots to a customer-controlled write-once destination MUST be supported for T2/T3.
`SEC-15` — Erasure redacts leaf content while preserving per-record commitment; chain remains verifiable.

---

## 9. Detection, policy, decision, alerting

### 9.1 Signal engine

`SEC-16` — Detection produces **signals**; signals are evidence, never final decisions.

Trackers (min set): `VelocityTracker · TargetTracker · DeviceTracker · SessionTracker · CredentialTracker · BehaviorTracker`.

Signal types (min set): `new_device · new_beneficiary · high_amount · high_velocity · distinct_targets · unusual_sequence · credential_change · privilege_change · suspicious_ip · automation_detected · credential_stuffing_pattern · multi_account_probing`.

Signal record: `signal_id · signal_type · subject_ref · value · threshold · source · window · confidence · timestamp · evidence_refs[] · tracker_version`.

`SEC-17` — A signal **MUST** reference the evidence that produced it.

Target tracking: distinct-target counter separate from raw request counter (500 requests to 1 beneficiary = retry; 500 requests to 500 = enumeration). Approximate structure (HyperLogLog) + bounded exact recent set. Records which produced a given value.

`SEC-18` — Trackers **MUST** be bounded in memory by cardinality; report when in approximate mode.

### 9.2 Baselines

`SEC-19` — Baseline-relative signals **MUST** declare learning window, minimum sample size, and behaviour before minimum is met.
`SEC-20` — Baselines **MUST** be resistant to slow poisoning.

### 9.3 Policy

`SEC-21` — Policies are declarative data, versioned, signed, hot-swappable as a precompiled unit.
`SEC-22` — A policy **MUST** start in `OBSERVE` mode. Promotion to `ENFORCE` is explicit, audited, reversible.
`SEC-23` — Policy evaluation **MUST** be deterministic, side-effect free, bounded in steps and time.
`SEC-24` — Shadow evaluation: run version N+1 alongside N and report the delta before promotion.

### 9.4 Decision record

`INV-16` — Every decision **MUST** produce a record sufficient to reconstruct it without the live application (decision_id, timestamp, tenant, subject, policies_evaluated, signals_used, identity_state, inputs_missing, coverage_gaps_overlapping, action, reason_codes, mode, fail_mode, degradation_level, latency_us, explanation, engine_versions).

`INV-17` — `inputs_missing`, `coverage_gaps_overlapping`, and `degradation_level` are mandatory.

`SEC-25` — Reason codes come from a stable, documented enumeration; part of the public contract.

### 9.5 Alerts

`OPS-22` — Two classes: security alerts (about the application) and system alerts (about the runtime). Separate channels and lifecycles.
`OPS-23` — Alerts **MUST** support dedup, grouping, suppression, ack/resolve lifecycle with audit.
`OPS-24` — Chain-verification failure, `PHANTOM_BUSINESS_OPERATION` cluster, or L4 safe mode are **critical** and **MUST NOT** be suppressible.

---

## 10. Integration and developer experience

`INT-1` — Minimal integration is a core requirement.

Target: `install package; Runtime::enable(); done.`

`INT-2` — Developers **MUST NOT** be required to redesign application, add IDs everywhere, rewrite controllers, redesign schemas, or manually configure every endpoint.

`INT-3` — Automatic instrumentation covers: HTTP entry, framework routing/middleware, queue dispatch and consumption, worker execution, DB/ORM operations, outbound HTTP, authentication events.

`INT-4` — Hook selection follows fallback chain; the tier actually used **MUST** be recorded.

`INT-5` — Instrumentation failure degrades capability; it never breaks the application.

`INT-6` — `runtime doctor` reports per-capability: attached/degraded/unavailable, reason, remedy.

`INT-7` — Optional enrichment API: `Runtime::context(['business_id' => $x, 'operation' => 'INVOICE_ISSUE']);`.

`INT-8` — Uninstall **MUST** be clean.

---

## 11. Application discovery

`INT-9` — The runtime **SHOULD** discover routes, controllers, services, commands, jobs, workers, listeners, observers, middleware, ORM activity, model hooks, webhooks, external integrations, scheduled tasks, database activity.

`INT-10` — Discovery states are distinct: `KNOWN · POSSIBLE · OBSERVED · CORRELATED · UNRESOLVED · UNKNOWN`.

`INT-11` — Static discovery ≠ runtime observation. Do not conflate.

`INT-12` — Discovery is a P3 activity; rate-limited, cancellable, biased to low-traffic periods.

`INT-13` — Discovery output is a proposal, not a fact; carry confidence; reviewable and overridable.

---

## 12. V0 — the smallest complete vertical slice

### 12.1 Principle

Narrow but complete: one thin path through every layer, end to end.

### 12.2 V0 scope

**In scope**

| Area | V0 content |
|---|---|
| Host | One framework adapter (D3, recommended PHP/Laravel) + one reference app |
| Topology | Shim + daemon, local socket, versioned handshake |
| Capture | HTTP entry, queue dispatch/consume, outbound HTTP, auth events |
| Privacy | Full classification + `C5` drop + denylist + `LOCAL_ONLY`, no egress code path |
| Event model | Full canonical event, schema v1.0 |
| Identity | Six actor roles distinct; `VERIFIED`/`UNKNOWN` only |
| Correlation | Tiers 1–4 only (explicit op ID, business ID, causation, resource ID) |
| Lifecycle | One lifecycle type end-to-end with staleness → timeout → closure |
| Signals | Three only: `new_beneficiary`, `high_velocity`, `distinct_targets` |
| Policy | One policy, `OBSERVE` mode, with shadow evaluation |
| Decision | Full decision record incl. `inputs_missing` and `degradation_level` |
| Audit | Hash chain + signing + `verify` |
| Storage | Evidence log + state store + quota + retention + eviction |
| Governor | Full L0–L4 + dead-man's switch |
| Interfaces | CLI only |
| Reconciliation | One check: lifecycle vs. one customer DB table, read-only |

**Not in V0**: enforcement mode, challenge actions, multi-framework adapters, discovery, dashboard UI, ML/behavioural baselines, probabilistic identity matching, concurrency groups, compensation modelling, multi-version route continuity, control plane, remote access, multi-tenant beyond a single tenant ID on every record.

### 12.3 V0 acceptance criteria

Binary, testable, all required.

1. Reference app runs unmodified apart from `Runtime::enable();`.
2. Under load: p99 latency ≤ `PERF-2`, throughput loss ≤ `PERF-4`.
3. Killing the daemon mid-load → zero application errors, gap recorded.
4. Filling disk to quota → eviction per ladder, no crash or unbounded growth.
5. Payment lifecycle spanning HTTP→queue→worker→outbound HTTP→webhook assembled correctly and closes.
6. Lifecycle whose final event never arrives reaches `CLOSED_WITH_UNCERTAINTY` within its configured ceiling, automatically.
7. Late event after closure classified and does not reopen or mutate prior evidence.
8. Shared endpoint hit by two callers produces two correctly-distinguished contexts on same `endpoint_id`.
9. Admin-on-behalf-of-customer payment to third party populates all six actor roles distinctly.
10. `verify` detects a deliberately corrupted audit record and reports exact index.
11. Decision record readable months later by a non-author.
12. Every `C5` field in a crafted adversarial payload absent from disk, verified by secret scanner.
13. Runtime runs on air-gapped host with no functional loss.
14. Under injected CPU starvation, runtime traverses L1→L4 and self-disables non-enforcement hooks without operator action.

---

## 13. Stage exit criteria

| Stage | Exit criteria |
|---|---|
| 1 | V0 met; two framework adapters; schema stability commitment; performance verified on two real workloads |
| 2 | Enforcement mode with fail-open/closed per policy; ≥12 signal types with calibrated confidence; measured FP rate below agreed threshold; policy shadow-promotion workflow; security alert lifecycle |
| 3 | Correlation tiers 1–8 with calibration; concurrency groups; attempts; compensation; reconciliation across DB + one external system; closure rate above agreed threshold |
| 4 | Discovery; static-vs-observed delta; multi-version semantic continuity; dependency and blast-radius mapping |
| 5 | Behavioural baselines with poisoning resistance; cross-lifecycle campaign detection; insider-threat findings |
| 6 | Agent identity as first-class actor type; tool-call and autonomy-level provenance; delegation chains |

`OPS-25` — Stage 6 is anticipated now, not retrofitted: `provenance.origin` admits `AUTOMATED`; `on_behalf_of` expresses delegation. Agent actors slot into existing model without schema break.

---

## 14. Testing and verification

`OPS-26` — Every requirement ID **MUST** map to at least one automated test.

### 14.1 Test classes

Unit · Property-based · Golden-corpus replay · Adversarial · Fault injection · Performance A/B · Soak · Calibration · Conformance.

### 14.2 Adversarial baseline

`OPS-27` — In the test suite from V0: event flood; two lifecycles made to look like one; one split; client asserting `role: admin`; equal timestamps on competing operations; webhook replayed 1000×; late SETTLED after CLOSED_WITH_UNCERTAINTY; identifier reused across two tenants.

### 14.3 Golden corpus

`OPS-28` — Versioned alongside code, no real customer data, changes reviewed.

### 14.4 Conformance matrix

`OPS-29` — `docs/conformance.md` maps every ID to its tests and status. A release with an unmapped requirement is blocked.

---

## 15. Interfaces

### 15.1 CLI-first

`OPS-30` — Every capability exists in the CLI before any UI. Minimum surface:

```
runtime status                 health, degradation level, budgets, coverage
runtime doctor                 per-capability attach state + remedy      (INT-6)
runtime events tail|query
runtime lifecycle show <id>
runtime lifecycle open|stale
runtime identity show <id>
runtime decision show <id>
runtime signals list
runtime policy list|lint|shadow|promote|rollback
runtime reconcile run|findings
runtime audit verify [--from --to]
runtime audit anchor
runtime coverage gaps
runtime storage usage|evict|retention
runtime privacy scan
runtime export --redacted
runtime tenant erase-subject
```

`OPS-31` — Every command **MUST** support `--json`.
`OPS-32` — `runtime export` defaults to redacted; MUST print inclusions before writing.

### 15.2 Local dashboard

`OPS-33` — Deferred past V0. Served by daemon, loopback-bound, no external assets. Views over coverage gaps MUST display them.

### 15.3 Query API

`OPS-34` — Local, authenticated, read-only by default. Not enabled by default.

---

## 16. Open decisions

| ID | Decision | Blocks | Recommendation |
|---|---|---|---|
| D1 | Daemon unit | Deployment | DaemonSet |
| D2 | Daemon language | All daemon code | Rust |
| D3 | First shim target | All shim code | PHP/Laravel |
| D4 | Storage engine | Storage layer | SQLite WAL + segmented log |
| D5 | Signing key custody | Audit chain | OS keystore T1/T2, PKCS#11 T3/T4 |
| D6 | Licence/packaging | Distribution | — |
| D7 | Confidence calibration thresholds | Detection quality | Set empirically from golden corpus |
| D8 | Default retention per class per tier | Privacy/storage | Conservative, documented |
| D9 | Reason-code taxonomy | Support + regulators | Define before enforcement ships |
| D10 | Control plane in first release | Roadmap | No — pure-local first |

---

## 17. Glossary

**Actor** — the business entity whose action this is; not necessarily the one authenticated.
**Authenticated principal** — the identity that actually proved itself.
**Caller** — the application/client that made the call.
**Service identity** — workload identity of the service handling the call.
**Authority** — a configured source declared definitive for a named attribute or state.
**Attempt** — one execution try of an operation.
**Coverage gap** — a recorded interval where the runtime knows it was not fully observing.
**Evidence** — a recorded observation with source, time, trust weight. Not proof.
**Lifecycle** — a business process that must reach a known closure state.
**Operation** — a semantic business action; not an endpoint.
**Pressure level** — 0–4 signal published by the resource governor.
**Shim** — the thin in-process component.
**Daemon** — the out-of-band local processing component.
**Truth stage** — where a claim sits between observation and authoritative business state.

---

## Appendix A — Implementation order

1. Resolve `D2`, `D3`, `D4`.
2. Canonical event schema + validator + conformance fixtures.
3. Shim skeleton: one hook, classification/redaction, bounded buffer, handshake. Prove `PERF-1`, `PERF-5`, `PERF-8` before adding a second hook.
4. Daemon skeleton: ingest, idempotency, storage, quota, crash recovery.
5. Resource governor + degradation ladder + dead-man's switch. Build this before the features it protects.
6. Audit chain + `verify`.
7. Identity role separation + context resolution.
8. Correlation tiers 1–4 + one lifecycle type + closure.
9. Three signals + one observe-mode policy + decision record.
10. One reconciliation check.
11. CLI surface.
12. Full conformance matrix green, then Stage 1 exit review.

Steps 3–6 make or break the product.
