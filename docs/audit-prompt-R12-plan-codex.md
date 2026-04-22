# codex audit — R12 plan review (no code yet)

## Context

This is a **plan audit**, not a code audit. No implementation
has started. We want codex to stress-test the plan before we
commit to 6-8 days of work in a sequence that's hard to undo
(published crate versions can't be unpublished).

Plan doc: `docs/R12-async-complete-plan.md` (the thing we want
you to review).

## Background

- R11.1-R11.3 already shipped `AsyncBroker` trait + `AsyncPaper`
  implementation (2026-04-21, PORT-PLAN v11).
- Current gap: only `AsyncPaper` implements `AsyncBroker`. The
  richer omspy-parity types (`VirtualBroker`, `ReplicaBroker`,
  `CompoundOrder`, `OrderStrategy`) are sync-only.
- Downstream: pbot uses `omsrs = { path = "../omsrs" }` and
  wants to switch to a crates.io semver dep. pbot currently has
  305 tests green against omsrs 0.2.0.

## What we need you to audit

Read `docs/R12-async-complete-plan.md` end-to-end, then assess:

### 1. Phase decomposition
- Is splitting into R12.1 (Virtual) → R12.2 (Replica) → R12.3
  (Compound + Strategy) → R12.4 (publish prep) → R12.5 (publish
  + pbot migration) the right order, or should some be
  parallelized / merged / split further?
- Does any phase depend on another's output in a way the plan
  doesn't call out?

### 2. Technical feasibility
- `AsyncVirtualBroker` approach: `tokio::sync::Mutex` over
  simulation state + `mpsc::UnboundedReceiver<BrokerReply>` for
  the event stream. Right primitives? Any concurrency hazards
  (deadlock, lost events, unbounded memory growth)?
- `AsyncCompoundOrder` / `AsyncOrderStrategy` as "just swap
  `&impl Broker` for `&(dyn AsyncBroker + Send + Sync)`" — is
  that actually enough? Are there lifetime / dyn-compatibility
  gotchas with `async_trait` object safety?
- `AsyncReplicaBroker` primary-success + replica-timeout test —
  is there a subtler async failure mode we should explicitly
  cover (primary succeeds, replica panics mid-await, etc.)?

### 3. Semver + publish risk
- `0.2.0 → 0.3.0` for "new public API, no breaks" — correct
  semver call? Or should adding async impls to types that
  already had sync impls be a 0.3 vs 1.0 vs 0.2.1?
- Is there a risk the crates.io name `omsrs` is already
  squatted? Plan says "check before publishing with fallback
  `omsrs-core`" — that manual enough?
- `polymarket-kernel` has a `build.rs` compiling C sources with
  AVX-512 runtime detection. Can that survive a stock
  crates.io build infrastructure? (docs.rs builds on a specific
  pool; if AVX-512 isn't detectable there, does the build
  fall back correctly?)

### 4. Non-goals sanity check
The plan excludes: async persistence, AsyncClock, N-replica
fan-out, pbot-itself-on-crates.io. Are any of these secretly
gating R12 in a way the plan doesn't see?

### 5. Open questions
The plan lists 3 open questions at the end (tokio vs runtime-
agnostic channel, PORT-PLAN.md §10 update, `&dyn` vs
`Arc<dyn>` ergonomics). Take a position on each. You don't
have to agree with our current lean.

## Output

Write to `docs/audit-R12-plan-codex-result.md`. Structure:

- **Phase decomposition** — PASS / CONCERN + rationale
- **Technical feasibility** — PASS / CONCERN + rationale
- **Semver + publish** — PASS / CONCERN + rationale
- **Non-goals** — PASS / CONCERN + rationale
- **Open questions** — your recommendation on each of the 3

Final verdict line:
- `R12 PLAN ACK — proceed to R12.1 implementation` if no
  material concerns, or
- `R12 PLAN NACK — revise before kickoff: <specific items>` if
  the plan needs changes.

## Meta

This plan author explicitly notes: **codex audit opinions are
valuable but not always correct**. The author intends to assess
every NACK item on technical merit before closing it out, and
may push back with a counter-argument in a re-audit rather than
blindly apply every suggestion. So: be specific, be technical,
cite concrete files or paths where relevant, and don't pad the
output with boilerplate. Short and sharp beats long and hedged.
