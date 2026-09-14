# Critical review of SPEC.md v1

Purpose: identify unresolved contradictions, remaining gaps, requirements that cannot be tested as written, missing IDs, and V0 execution risks. This is a working review, not a rejection — the spec is buildable, but the items below should be resolved (or explicitly deferred with a note) before shipping V0.

Every finding has a suggested resolution. Findings are ordered by severity.

---

## 1. Contradictions / unresolved tensions

### R-1 (severity: high) — Fail-closed under deadline breach silently becomes fail-open

`INV-4` says any critical-path runtime operation exceeding its hard deadline MUST abandon the runtime operation, not delay the request. `SEC-10` says a policy MAY be configured `fail_mode: CLOSED`. When a fail-closed policy hits the deadline, `INV-4` forces abandonment — which means the request is allowed. A fail-closed policy under overload therefore silently degrades to fail-open, with no way for the customer to distinguish "policy said allow" from "runtime ran out of time".

**Resolution:** the decision record already captures `mode` and `latency_us` (INV-16), but it does not currently capture *deadline-abandonment* as a distinct outcome. Add a mandatory field `outcome_reason: {"POLICY_MATCH", "DEADLINE_EXCEEDED", "RUNTIME_FAILURE", "NO_POLICY_APPLIED"}` and require fail-closed policies to emit a `system` alert (§9.5) on each deadline abandonment. This preserves fail-open safety while making the compromise visible.

### R-2 (severity: high) — Authority override vs. "closed lifecycles do not reopen"

`MOD-23` says an authority's assertion overrides observation for state. `MOD-27` says reopening a closed lifecycle requires an explicit authority assertion *or* an operator action. Reading these together: an authority's assertion IS supposed to override, so does an authoritative `SETTLED` arriving after `CLOSED_WITH_UNCERTAINTY` reopen the lifecycle?

**Resolution:** clarify in §5.11 that authority overrides *update state without reopening*: the lifecycle stays closed, but `final_state` is corrected and a `state_correction` event is appended with a link to the authority. Reopening (returning `closure` from `CLOSED_*` to `OPEN`) requires an operator action regardless. Split `MOD-23` and `MOD-27` textually so the distinction is unambiguous.

### R-3 (severity: high) — Serverless / T4 tension in daemon topology

`INV-11` requires split-plane, and §3.2 supports serverless with "daemon must be remote-but-local-network, or degrade to buffered export". "Buffered export" is undefined and appears to violate `PRI-1` in T4 (no egress). If a serverless customer is also T4, there is no daemon location that satisfies both requirements.

**Resolution:** either (a) declare serverless + T4 unsupported, or (b) specify buffered-export destination as a customer-controlled local-network endpoint only, and forbid the vendor from ever being that endpoint. Prefer (a) for the first release; the mixed profile is small and adds significant complexity.

### R-4 (severity: medium) — L4 safe mode does not say whether enforcement itself can be shed

§7.5 L3 keeps "enforcement, signals feeding enforced policies, audit chain, lifecycle state". L4 says "shim detaches from all non-enforcement hooks". But `INV-14` (dead-man's switch) says the shim self-disables non-enforcement instrumentation when it cannot meet `PERF-1`–`PERF-3`. Nothing describes what happens if enforcement policies themselves exceed budget sustainably. Under a runaway enforcement rule, the runtime could hold the request path indefinitely while claiming fail-open per-request.

**Resolution:** add a cumulative circuit breaker: if enforcement policies exceed their aggregate CPU budget for a sustained window, the runtime MUST demote enforcement policies to observe mode and emit a critical system alert. This must be a runtime behaviour, not a per-policy setting, because the trigger is cross-policy.

### R-5 (severity: medium) — Schema evolution has no sunset

`MOD-20`: "daemon MUST read all event schema versions it has ever emitted." Over years this becomes a maintenance burden and a security surface (every historical decoder must remain fuzzed and patched). No deprecation path is defined.

**Resolution:** add a policy: the daemon MUST read all versions still within any tenant's active retention window. Once no live evidence exists at version N and no unclosed lifecycles reference it, N may be sunset with an operator-acknowledged migration.

### R-6 (severity: medium) — `PRI-3` "compiled/feature-gated off where the platform allows" weakens the invariant

Rust and Go daemons can enforce this at compile time. The PHP shim cannot. "Where the platform allows" is an escape hatch that will be used everywhere the enforcement is hardest.

**Resolution:** require, for platforms that cannot compile-gate egress, a startup self-test that fails hard if any egress code path is reachable (e.g. by attempting to bind an outbound test call to a blackhole and asserting the code path is unreachable). Making the invariant a runtime assertion is weaker than compile-time but stronger than "where the platform allows".

### R-7 (severity: low) — Concurrency group P1 signal under L3

`MOD-36` says COMPETING groups raise a signal regardless of outcome. L3 keeps "signals feeding enforced policies". A P1 concurrency signal not currently tied to an enforced policy is ambiguous: is it P1 evidence integrity or P2 correlation?

**Resolution:** classify concurrency-group emission as P1 lifecycle integrity (not P2 correlation), because it is a state-truth signal not merely a correlation heuristic. Update §2.2.

---

## 2. Gaps still present

### G-1 — Policy evaluation engine (missing D-item)

`SEC-6` requires policy to be "data, evaluated by a sandboxed evaluator with no I/O and a step limit", but the language/engine is not chosen. This is a first-class dependency and belongs in §16. Add **D11**.

### G-2 — Reference application for benchmarking

§7 mandates continuous performance A/B, criterion 2 in §12.3 requires benchmark under load, but no reference application is specified. Two customers running two apps will produce two irreconcilable numbers. Add **D13**.

### G-3 — Secret scanner for criterion 12

§12.3 criterion 12 says "verified by a secret-scanner over the data directory". Which scanner? Detection rules for `C4`/`C5`? Add **D12**.

### G-4 — Clock source model

`MOD-8` requires timestamp + received_at + monotonic_seq. NTP is not assumed. §5.4 shows microsecond precision. No spec of clock source, skew estimation method, or behaviour on backward wall-clock jumps. Add **D14**.

### G-5 — License model interaction with enforcement

License expiry (implied by §16 D6) must not silently disable enforcement. No text currently covers this. Under a lapsed license, does the runtime keep enforcing? Must be explicit in D6.

### G-6 — Anchor confirmation

`SEC-14` requires the runtime to support anchoring segment roots to a customer-controlled write-once destination, but does not specify how the runtime records that the anchor succeeded. Without a confirmation record, an operator cannot prove that an anchor was performed at any specific past time.

**Resolution:** the audit chain MUST include an `anchor_receipt` record after each anchoring event, containing destination identifier, digest anchored, and any customer-supplied receipt.

### G-7 — Reconciliation DB read locking

`MOD-39` says reads are read-only, rate-limited, dedicated connection. It does not say "snapshot isolation only, no lock hints". Some DBs and drivers acquire shared locks that block writers.

**Resolution:** require MVCC / snapshot isolation and forbid lock hints in the reconciliation read path.

### G-8 — Baseline poisoning test

`SEC-20` requires resistance to slow poisoning. `§14 · OPS-27` adversarial baseline list does not include a poisoning attack. This is one of the strongest attacks against behavioural detection and must be in the adversarial set from day one, even if V0 defers behavioural baselines.

### G-9 — `runtime doctor --json` schema

`OPS-31` requires `--json` for every command. `runtime doctor` returns per-capability state (`INT-6`), with no defined shape. Define it once, in-repo, and freeze it — it is a support-contract surface.

### G-10 — Reason-code taxonomy is a public contract (D9)

D9 is already flagged. Emphasising: this must be published *before* enforcement ships, because customer support and regulators will consume it. Late-binding this is not survivable.

---

## 3. Untestable requirements

### T-1 — `INV-2` "user-perceivable or materially harmful impact"

"Materially harmful" is not defined until §7.1 provides budgets. The invariant text should reference §7.1 explicitly, otherwise a reader can read INV-2 alone and be unable to test it. Textual fix.

### T-2 — `PRI-8` opt-in per field or per path

Capture is opt-in, but no config schema exists. Cannot write a test for "the default denies capture" without a config to point at. Define the config in §4.3.

### T-3 — `MOD-17` confidence calibration

Calibration is testable via §14 calibration class, but requires a target (Brier score, ECE, reliability diagram) and a threshold. Depends on D7. Cannot be conformance-graded until D7 is set. Mark deferred to Stage 2 exit in the conformance matrix.

### T-4 — Criterion 11 "reconstructable months later"

Currently subjective. Rewrite as: "given only the decision record and any evidence records it references, an operator not present at decision time can identify (a) each signal used with its value and confidence, (b) each policy version evaluated and its match state, (c) the resolution status of every actor role, (d) `inputs_missing`, (e) `coverage_gaps_overlapping`, (f) `mode` and `fail_mode`, (g) `degradation_level`, (h) engine versions. Test: automated harness reads the record and asserts each field."

### T-5 — Criterion 14 "injected CPU starvation"

Method affects reproducibility. Specify: cgroup CPU quota / `stress-ng --cpu N --cpu-load 100` at host level, with runtime pinned to a quota below the load. Include in `docs/test-methods.md`.

---

## 4. Missing from the conformance matrix

The matrix does not yet exist; §14.4 requires it. Counted requirement IDs:

| Class | Range | Count |
|---|---|---|
| INV | 1-17 | 17 |
| PRI | 1-16 | 16 |
| SEC | 1-25 | 25 |
| PERF | 1-12 | 12 |
| MOD | 1-42 | 42 |
| INT | 1-13 | 13 |
| OPS | 1-34 | 34 |

Total: 159 normative requirements. `docs/conformance.md` is initialised as a skeleton with every ID present, status `NOT_IMPLEMENTED`, test reference blank. `OPS-29` gates release on this matrix being green.

---

## 5. V0 execution risks

### V-1 — Enforcement path is not exercised in V0

§12.2 excludes enforcement mode. Consequence: `INV-4` (deadline + fail-open on critical path), `SEC-10` (fail-mode), and the entire "cost of being on the request path" story are unverified until Stage 2. This is deliberate but it means Stage 1 exit cannot claim the runtime is safe on the critical path — because it never is on the critical path in V0. Make this explicit in the Stage 1 exit criteria.

### V-2 — Governor is upstream of the features it protects

Appendix A step 5 correctly says build the governor before what it protects. In practice, the temptation will be to build correlation and lifecycle first because they are more intellectually interesting. Reinforce: build the governor + dead-man's switch + degradation ladder as its own testable milestone with dedicated fault-injection tests before shim capture is expanded beyond one hook.

### V-3 — SQLite + segmented log crash consistency

The evidence log is append-only and the state store is SQLite. On unclean shutdown these can end in mutually inconsistent states (evidence written but state not yet applied, or vice versa). Recovery must reconcile both, treating the evidence log as the source of truth and replaying deltas into the state store. Define this before writing storage code.

### V-4 — Idempotency requires strict `event_id` generation

`OPS-7` says ingest is idempotent on `event_id`. In V0's PHP-FPM shim, if the shim generates an event, sends it, and the process dies before the daemon acks, redelivery on next request will not happen (no queue). If the daemon dies and reboots, redelivery from the shim buffer *will* happen. `event_id` must be stable across shim retries — generate it deterministically per hook invocation (ULID with monotonic sequence in a shared memory counter), not per send attempt.

### V-5 — Lifecycle closure test needs a synthetic clock

Criterion 6 requires reaching `CLOSED_WITH_UNCERTAINTY` "within its configured ceiling". Waiting real time is not testable in CI. Provide an injectable clock in the daemon from day one so lifecycle timers can be advanced in tests.

---

## 6. Recommended text amendments

Small edits I would make to v1 before it circulates further:

- Rename `INV-2` "user-perceivable or materially harmful" to "user-perceivable or exceeding §7.1 budgets".
- Add `outcome_reason` to §9.4 decision record schema (per R-1).
- Split §5.11 "authority correction" from "reopening" (per R-2).
- Add §12.3 criterion 15: "runtime survives 24h with a fail-closed policy under sustained deadline pressure without silently degrading; system alert stream shows the demotions."
- Add §12.3 criterion 16: "runtime survives clock skew of ±5 minutes and a single backward wall-clock jump without emitting duplicates or losing lifecycles."
- Add §14 adversarial test: slow-poison a behavioural baseline (deferred to Stage 5 but scaffold now).

---

## 7. Overall assessment

The spec is *implementable*. It is more disciplined than most V1 documents in the category, in particular:

- The split-plane invariant (`INV-11`) is correctly the axis around which everything else pivots.
- The evidence-vs-inference discipline (`INV-7`–`INV-10`) is unusually strong and will pay off in audit defensibility.
- The degradation ladder (§7.5) plus dead-man's switch (`INV-14`) is the right shape.
- Reserving `UNKNOWN` as a first-class result (principle 9) is what separates a truthful evidence system from a plausible one.

The gaps above are almost all specification hygiene, not conceptual defects. The two exceptions — R-1 (silent degradation under fail-closed) and R-4 (no cumulative enforcement circuit breaker) — are real design gaps and must be resolved before enforcement mode ships in Stage 2. Neither blocks V0.
