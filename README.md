# athar

Privacy-first local application security, trust, lifecycle & intelligence runtime.

Source of truth: [`SPEC.md`](./SPEC.md) (v1, normative).
How to run V0 locally: [`docs/RUN.md`](./docs/RUN.md).
Testing against a real Laravel payments app: [`docs/TESTING_LARAVEL.md`](./docs/TESTING_LARAVEL.md).

## Repo layout

```
SPEC.md                     Normative specification (v1)
docs/
  REVIEW.md                 Critical review of v1: contradictions, gaps, untestable requirements
  DECISIONS.md              Resolutions for D1-D14 (recommendations pending sign-off)
  conformance.md            Requirement ID -> test mapping (§14.4)
schema/
  event.v1.0.json           Canonical event JSON Schema (Appendix A step 2)
  fixtures/                 Conformance fixtures for the schema
shim/                       In-process shim (empty; blocked on D2/D3)
daemon/                     Out-of-band daemon (empty; blocked on D2)
```

## Status

- v1 spec captured.
- Review + decisions drafted; **D1-D14 all ACCEPTED 2026-09-13**.
- V0 code started. Appendix A progress:
  - Step 1 (resolve D2/D3/D4): done.
  - Step 2 (schema + validator + fixtures): schema and fixtures in `schema/`; Rust types in `daemon/crates/athar-event/` with structural validators and tests.
  - Step 3 (shim skeleton): scaffolded in `shim/`; Runtime API stub, Laravel ServiceProvider stub, safety-catch pattern in place. Capture/Classify/Redact/Buffer/Transport pending.
  - Step 4 (daemon skeleton): Rust workspace + placeholder crates. Storage, audit, governor, daemon, CLI to fill in.
  - Step 5 (resource governor + dead-man's switch): pending, must precede steps 7-10.
