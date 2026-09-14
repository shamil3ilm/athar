# Conformance matrix

Per §14.4 / `OPS-29`: every requirement ID maps to at least one automated test. A release with any unmapped requirement is blocked.

Statuses: `NOT_IMPLEMENTED` · `SCAFFOLDED` · `GREEN` · `RED` · `DEFERRED_STAGE_n`.

All entries below are `NOT_IMPLEMENTED` at V0 start.

---

## INV — Architectural invariants

| ID    | Requirement (short) | Test(s) | Status |
|-------|---------------------|---------|--------|
| INV-1 | Application always primary | soak/perf-ab | NOT_IMPLEMENTED |
| INV-2 | No user-perceivable / >budget harm | soak/perf-ab | NOT_IMPLEMENTED |
| INV-3 | Enforcement is opt-in per policy | test/policy-modes | NOT_IMPLEMENTED |
| INV-4 | Critical-path deadline + fail-open on breach | test/deadline | NOT_IMPLEMENTED |
| INV-5 | Backlog resolved by self-shedding | fault/backlog | NOT_IMPLEMENTED |
| INV-6 | Coverage gaps recorded on shed | test/coverage-gap | NOT_IMPLEMENTED |
| INV-7 | No fabricated events | property/no-fabrication | NOT_IMPLEMENTED |
| INV-8 | No uncertainty-to-certainty across layers | property/confidence-preserved | NOT_IMPLEMENTED |
| INV-9 | Evidence is append-only | property/append-only | NOT_IMPLEMENTED |
| INV-10 | Decisions reconstructable from stored data | test/decision-reconstruction | NOT_IMPLEMENTED |
| INV-11 | Split-plane topology | test/topology | NOT_IMPLEMENTED |
| INV-12 | Shim works without daemon | fault/daemon-absent | NOT_IMPLEMENTED |
| INV-13 | Shim does not load models/parse policy/open network | test/shim-boundary | NOT_IMPLEMENTED |
| INV-14 | Dead-man's switch | fault/cpu-starve, `athar-governor::tests::deadman_*` | SCAFFOLDED |
| INV-15 | Shim panic containment | fault/panic-inject | NOT_IMPLEMENTED |
| INV-16 | Explainable decision record | test/decision-record-schema | NOT_IMPLEMENTED |
| INV-17 | inputs_missing / gaps / degradation in every decision | test/decision-fields | NOT_IMPLEMENTED |

## PRI — Privacy

| ID    | Requirement (short) | Test(s) | Status |
|-------|---------------------|---------|--------|
| PRI-1 through PRI-16 | (see SPEC §4) | test/privacy/* | NOT_IMPLEMENTED |
| PRI-7 | C5 dropped at capture, before any buffer/disk write | `Athar\Shim\Redact::stripSecrets`; `shim/bin/smoke-test.php` (adversarial payload → no leaks) | SCAFFOLDED |
| PRI-8 | Payload capture opt-in per field, unknown = drop | `Athar\Shim\Redact::apply` with allowlist; smoke test enforces C3 default = drop | SCAFFOLDED |
| PRI-9 | Default denylist applied unconditionally | `Athar\Shim\Classify::C5_SUBSTRINGS`; smoke test covers 8 denylist categories | SCAFFOLDED |

## SEC — Security

| ID    | Requirement (short) | Test(s) | Status |
|-------|---------------------|---------|--------|
| SEC-1 through SEC-25 | (see SPEC §5, §8, §9) | test/security/*, adversarial/* | NOT_IMPLEMENTED |
| SEC-16 | Detection produces signals as evidence, not final decisions | `athar-detection::signal::SignalEngine`, `SignalRecord` | SCAFFOLDED |
| SEC-17 | Signal references the evidence that produced it | `SignalRecord::event_id` + `lifecycle_id` | SCAFFOLDED |
| SEC-21 | Policies are declarative data, versioned | `athar-detection::policy::PolicyEngine`, `PolicyDecision::policy_version` | SCAFFOLDED (hardcoded V0; CEL D11 deferred) |
| SEC-22 | Policy starts in OBSERVE mode; promotion is explicit | Only OBSERVE mode implemented in V0; no promotion path exists | SCAFFOLDED |
| SEC-23 | Policy evaluation is deterministic, side-effect free | `athar-detection::policy::tests::deterministic_result` | SCAFFOLDED |
| SEC-25 | Reason codes from a stable enumeration | `TARGET_NEW_BENEFICIARY_HIGH_AMOUNT` (V0 seed; D9 taxonomy pending) | SCAFFOLDED |
| INV-16 | Explainable decision record | `athar-detection::decision::DecisionRecord`; `detection_produces_decision_with_matched_policy_for_high_amount_new_beneficiary` | SCAFFOLDED |
| INV-17 | inputs_missing / coverage_gaps / degradation_level mandatory | Fields present on every `DecisionRecord`; ingest fills `degradation_level` from governor | SCAFFOLDED |
| SEC-11 | Hash-chained audit log `H(prev_hash \|\| canonical(record))` | `athar-audit::record_hash`, `verify_chain`; `happy_path_verifies` | SCAFFOLDED |
| INV-9  | Evidence is append-only (structural, no update API) | `athar-storage::segment_log::SegmentLog` (no update/delete on prior records) | SCAFFOLDED |
| SEC-12 | Segments signed at close (Ed25519) | `athar-audit::ChainWriter::close_segment`, `Signer` trait; `bad_signature_detected` | SCAFFOLDED |
| SEC-13 | `verify` reports exact record index of any break | `athar-audit::verify_segment`, `verify_chain`; `corrupted_record_hash_detected_at_exact_index`, `corrupted_prev_hash_detected_at_exact_index` | SCAFFOLDED |
| SEC-14 | External anchoring of segment roots | pending | NOT_IMPLEMENTED |
| SEC-15 | Erasure preserves chain (payload-commitment model) | `athar-audit::mark_redacted` (partial, needs RedactionCertificate) | SCAFFOLDED (partial) |
| INV-10 | Decisions reconstructable from stored data | `athar-audit::persistence::SegmentStore::verify_all`, `athar-cli` `audit verify` (foundation only; decision records still pending in athar-daemon) | SCAFFOLDED (partial) |

## PERF — Performance

| ID    | Requirement (short) | Test(s) | Status |
|-------|---------------------|---------|--------|
| PERF-1  | +200µs p50 per instrumented request | bench/refapp | NOT_IMPLEMENTED |
| PERF-2  | +1ms p99 per instrumented request | bench/refapp | NOT_IMPLEMENTED |
| PERF-3  | +5ms p99 with enforcement | bench/refapp-enforce | DEFERRED_STAGE_2 |
| PERF-4  | ≤2% throughput loss | bench/refapp | NOT_IMPLEMENTED |
| PERF-5  | Shim ≤16MB RSS | bench/mem | NOT_IMPLEMENTED |
| PERF-6  | Daemon ≤512MB RSS (default cap) | bench/mem | NOT_IMPLEMENTED |
| PERF-7  | Daemon ≤1 core steady state | bench/cpu | NOT_IMPLEMENTED |
| PERF-8  | Startup ≤25ms added | bench/startup | NOT_IMPLEMENTED |
| PERF-9  | Write amplification ≤3x | bench/storage | NOT_IMPLEMENTED |
| PERF-10 | Continuous prod self-measurement | test/self-measure | NOT_IMPLEMENTED |
| PERF-11 | Sampled self-measurement | test/self-measure-sampling | NOT_IMPLEMENTED |
| PERF-12 | Budget breach detection needs a window | test/self-measure-window | NOT_IMPLEMENTED |

## MOD — Domain model

| ID    | Requirement (short) | Test(s) | Status |
|-------|---------------------|---------|--------|
| MOD-1 through MOD-42 | (see SPEC §5) | schema/*, golden-corpus/*, property/* | NOT_IMPLEMENTED |
| MOD-16 | Endpoint is not an operation; semantic operation identity | `athar-lifecycle::LifecycleType::from_event_type`; state machine over event_type | SCAFFOLDED |
| MOD-17 | Correlation tier + confidence recorded on every inference | `athar-lifecycle::InferenceTier::confidence`, stored per event on the lifecycle | SCAFFOLDED |
| MOD-24 | Lifecycle is first-class, independent of app/service/endpoint | `athar-lifecycle::Lifecycle` | SCAFFOLDED |
| OPS-9  | Split storage: evidence log + state store + audit chain | `athar-storage::segment_log` (evidence) + `athar-lifecycle::SqliteLifecycleStore` (state) + `athar-audit` (chain) | SCAFFOLDED |
| MOD-25 | No lifecycle silently open forever; staleness → close | `athar-lifecycle::StalenessScanner`; `stale_lifecycle_is_closed_with_uncertainty` test | SCAFFOLDED |
| MOD-26 | State transitions validated per lifecycle type | `athar-lifecycle::engine::transition_for` | SCAFFOLDED |
| MOD-27 | Late events after closure classified, no automatic reopen | `athar-lifecycle::engine::classify_late_event`; `late_event_after_closure_is_classified_not_applied`, `ingest_late_event_after_closure_is_classified_no_reopen` | SCAFFOLDED |
| MOD-28 | Missing events: gap recorded, no synthetic fill | State machine emits `Timeout`/`Abandoned` on staleness, does NOT insert fake events | SCAFFOLDED |
| MOD-6 | Canonical event schema | `schema/event.v1.0.json`, `athar-event` types | SCAFFOLDED |
| MOD-7 | Unknown is null, not empty string | `athar-event::tests::empty_string_actor_id_rejected`, `reserved_unknown_token_rejected`; fixture `invalid-01-empty-string-id.json` | SCAFFOLDED |
| MOD-8 | timestamp + received_at distinct | `athar-event::Event` struct | SCAFFOLDED |
| MOD-11 | Five provenance fields distinct | `athar-event::Provenance` | SCAFFOLDED |
| MOD-15 | Six actor roles distinct | `athar-event::Event` struct; fixture `valid-02-admin-on-behalf.json` | SCAFFOLDED |
| MOD-20 | Schema version negotiation | `athar-event::SCHEMA_VERSION`, `schema_version_mismatch_rejected` test | SCAFFOLDED |

## INT — Integration / DX

| ID    | Requirement (short) | Test(s) | Status |
|-------|---------------------|---------|--------|
| INT-1 through INT-13 | (see SPEC §10, §11) | test/dx, integration/* | NOT_IMPLEMENTED |
| INT-1  | Minimal integration: install + `Runtime::enable();` | `Athar\Adapter\Laravel\ServiceProvider` auto-discovered; no controller edits required for HTTP capture | SCAFFOLDED |
| INT-2  | No forced app redesign / no IDs everywhere | Middleware captures without controller changes; `route_template` uses framework-normalised form | SCAFFOLDED |
| INT-3  | Auto-instrumentation covers HTTP entry + queue + outbound HTTP + ORM | `HttpMiddleware` (router), `QueueSubscriber` (Laravel queue events), `OutboundHttpSubscriber` (Http client events), `Support\ObservesLifecycle` trait (Eloquent). Tests: `middleware-test.php` (14), `eventmapper-test.php` (27), `subscribers-test.php` (34) | SCAFFOLDED (auth events still pending) |
| INT-5  | Instrumentation failure degrades capability; never breaks app | Middleware `emit()` catches every `\Throwable`; `MIDDLEWARE ALL GREEN` test proves it | SCAFFOLDED |

## OPS — Operations

| ID    | Requirement (short) | Test(s) | Status |
|-------|---------------------|---------|--------|
| OPS-1 through OPS-34 | (see SPEC §1.5, §6, §7, §14, §15) | test/ops, fault/*, cli/* | NOT_IMPLEMENTED |
| OPS-12 | Storage enforces disk quota itself | `athar-storage::quota::Quota`, `segment_log::quota_rejects_oversized_total` | SCAFFOLDED |
| OPS-13 | Backup = file-copy of consistent segments | Segments are self-contained files under `root`; restorable by directory copy | SCAFFOLDED |
| OPS-16 | Corrupt segment quarantined, not silently skipped | `athar-storage::segment_log::crash_recovery_quarantines_wip` | SCAFFOLDED |
| OPS-17 | Governor publishes single `pressure_level` (0-4) | `athar-governor::Governor` | SCAFFOLDED |
| OPS-18 | Subscribers honour changes within 100 ms | `athar-governor::subscribe`, `subscribers_receive_updates` test; `athar-daemon::ingest::ingest_drops_and_records_coverage_gap_under_l4` | SCAFFOLDED |
| INV-6  | Coverage gaps recorded on shed | `athar-daemon::ingest::DropCounters`, `emit_coverage_gap_if_any`; `ingest_drops_and_records_coverage_gap_under_l4` test | SCAFFOLDED |
| MOD-29 | `coverage_gap` written when runtime sheds/drops/loses data | `athar-daemon::ingest::CoverageGapSummary`; audit chain records under kind `coverage_gap` | SCAFFOLDED |

---

## Legend for release gating

A release ships iff:
- Zero `RED`.
- Zero `NOT_IMPLEMENTED` for requirements in-scope for the current stage.
- Every `DEFERRED_STAGE_n` has `n > current_stage`.

This is scaffold. Each `<class>-n` row above must be broken out to a discrete row with a specific test path before Stage 1 exit.
