# codex v2 audit — R12 plan after round-1 NACK closeout

## Context

Round 1 (`docs/audit-R12-plan-codex-result.md`) NACKed the v1
plan with substantive concerns spread across phase
decomposition, technical feasibility, semver, and publish
sequencing. **Every** concern was accepted by the plan author;
nothing was pushed back as misreading.

This is the v2 audit — verify the closeout addressed each
round-1 item and that v2 doesn't introduce new problems.

## What changed (v1 → v2)

Plan doc: `docs/R12-async-complete-plan.md` (rewritten).
Revision history summary is in the doc's own §"Revision history"
block at the top. Quick cross-reference of round-1 NACK items to
v2 remediation:

| v1 NACK item | v2 remediation | Where |
| --- | --- | --- |
| R12.2 described as primary/replica wrapper — actually matching engine | Rewritten to "async port of the standalone matching engine"; API aligned to real `place` / `modify` / `cancel` / `run_fill` / `update` / shared-identity `Arc::ptr_eq` | §R12.2 |
| R12.3 missing async `Order` lifecycle prerequisite | Split into R12.3a (async Order methods) + R12.3b (AsyncCompoundOrder + AsyncOrderStrategy) | §R12.3a, §R12.3b |
| R12.1 `AsyncBroker::order_place` return drops `BrokerReply` surface | Inherent `async fn place/modify/cancel → BrokerReply` + lossy `impl AsyncBroker` adapter | §R12.1 |
| Invented method names (submit_with_fill_ratio etc) | Method list matches real sync API (`with_clock`, `with_tickers`, `order_place/modify/cancel`, `update_tickers`, `ltp`, `ohlc`, etc.) | §R12.1, §R12.2 |
| `tokio::sync::Mutex` → prod dep without justification | Use `parking_lot::Mutex`, no await-while-locked; `tokio` stays dev-only | §R12.1 implementation note + §"Resolved open questions" |
| Stream contract underspecified | Deleted. Sync returns one reply per call; async does the same | §R12.1 |
| AsyncCompoundOrder "mechanical trait swap" wrong | Now explicitly depends on R12.3a Order `_async` methods | §R12.3a/b |
| OrderStrategy run_fn async question | Accept sync closure (no I/O in callback) | §R12.3b |
| R12.5 "coordinated" kernel publish | polymarket-kernel is hard predecessor | §R12.5 |
| `cargo doc ... -D warnings` syntax wrong | `RUSTDOCFLAGS="-D warnings" cargo doc ...` | §R12.4 CI block |
| `lib.rs` export missing from acceptance | Added per sub-phase | §R12.1-R12.3b acceptance |
| 0.3.0 vs 0.2.1 semver | Documented as policy choice, not semver requirement | §R12.5 step 2 |
| crates.io name squat check | Gating pre-step of R12.4 | §R12.4 pre-step |
| polymarket-kernel non-AVX-512 build on docs.rs | Hard-gate pre-test in §R12.5 step 1 | §R12.5 |
| Test names (OCO/bracket/ladder/grid) invented | Replaced with real parity surface: `execute_all`, `check_flags`, aggregate views, `run_fn`, `add` | §R12.3b |
| Sync→async signature change = semver break | Explicit "Hard constraint" block at top of Scope | §"Hard constraint" |
| `save_to_db` sync inside async method | Documented caveat; pbot unaffected; R13 scope for async persistence | §R12.3a + §"Explicit non-goals" |
| BrokerReply not Clone | Inherent method returns reply directly (no stream send); sync call shape | §R12.1 |
| PORT-PLAN.md §10 still says "no tokio" | Update in R12.4 with supersession note | §R12.4 docs + §Resolved open questions |
| `&dyn` vs `Arc<dyn>` | Stored: `Arc<dyn>`, method params: `&dyn` | §"Resolved open questions" |

## What to audit (v2-specific)

### 1. Did v2 actually fix every round-1 concern?
Read `docs/audit-R12-plan-codex-result.md` end-to-end, then
walk each CONCERN/hypothesis item and check it against the new
plan. Any item that v1 raised that v2 did **not** address
should be called out.

### 2. Are there *new* problems introduced by the closeout?
The biggest restructures:
- R12.3 split into 3a + 3b. Does R12.3a's scope ("add async
  siblings to `Order::execute/modify/cancel`") itself have
  hidden gotchas? For example: `Order::execute` internally calls
  broker-copy hooks (`broker.attribs_to_copy_execute`) which are
  sync today. `AsyncBroker` has async versions. Does
  `Order::execute_async` call the async versions, and does that
  behavior parity with sync still hold?
- R12.1 inherent-method-plus-lossy-trait split. Is the lossy
  adaptation lossless enough for pbot's use? pbot's event loop
  cares only about `Option<String>` from `order_place`, but
  does `AsyncCompoundOrder` (which takes `Arc<dyn AsyncBroker>`)
  ever need the richer `BrokerReply` surface? If so, the lossy
  adapter path defeats the purpose.
- R12.5's hard-predecessor order (polymarket-kernel first, then
  omsrs, then pbot). Is there a scenario where the pbot
  migration hits an SDK version-pin mismatch that the plan
  doesn't anticipate?

### 3. Plan-level soundness
- Timing (7-9 days) — plausible with codex round-trips?
- Risk table — anything missing?
- Acceptance checklist at the bottom — covers every irreversible
  action (publishing is unreversible; signatures changes cross
  the semver boundary)?

## Output

Write to `docs/audit-R12-plan-v2-codex-result.md`. Structure:

- **Round-1 closeout coverage** — PASS (all remediated) or
  FAIL with list of items still open
- **New problems in v2** — list any
- **Plan-level soundness** — PASS / CONCERN + rationale

Final verdict line:
- `R12 PLAN v2 ACK — proceed to R12.1 implementation`, or
- `R12 PLAN v2 NACK — revise again: <specific items>`

## Meta

Per feedback_codex_audit_judgment in the plan author's notes:
audit items will be assessed on merit, not auto-applied. If
round-1 closeout over-rotated (e.g. closed a NACK that was
actually wrong), say so. If closeout missed a real item, say
so concretely with file/line.

Be specific, short, technical. No boilerplate.
