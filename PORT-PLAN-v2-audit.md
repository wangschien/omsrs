# Audit: omsrs Port Plan v2

Adversarial audit. Author (Claude) is NACK'd from self-auditing.

**Read first**: `~/omsrs/PORT-PLAN.md` (v2).

## Context

v1 plan (`~/poly/docs/omsrs-port-plan-v1.md`) was NACK'd — it mixed in Polymarket integration, barter-rs reuse discussion, and `/poly/src/bot/` replacement scope that user **did not ask for**.

v2 is deliberately **pure port of omspy (the Python library) into Rust**. Scope: core OMS abstraction, broker-agnostic. No venue integration, no downstream consumers mentioned, no `/poly/` or `~/bot/` touched.

**Audit this plan as a pure Rust port of omspy, nothing else.** Do not re-raise Polymarket / barter-execution concerns — those are downstream users' problems, out of scope here.

## Reference material

- `~/refs/omspy/` — upstream Python source
- Key files:
  - `omspy/base.py` (340) — `Broker` abstract base
  - `omspy/order.py` (1468) — `Order` + `CompoundOrder`
  - `omspy/models.py` (482) — pydantic models
  - `omspy/simulation/{models,virtual,server}.py` (1604) — paper exchange
  - `omspy/brokers/paper.py` (52) — paper broker binding
  - `omspy/utils.py` (243) — helpers
  - `omspy/tests/` — existing test patterns to mirror

## Audit checklist

### A. Scope correctness

- [ ] A1: Does the **in-scope** list (§1) actually cover omspy's core? Compare against `omspy/__init__.py` — are there critical imports/exports not mentioned in the plan?
- [ ] A2: Does the **out-of-scope** list correctly exclude only broker adapters that are venue-specific? Specifically: Zerodha, ICICI, Finvasia, Neo, Noren. Confirm these are Indian-market brokers and the user doesn't trade those.
- [ ] A3: `algos/` (TWAP/iceberg) and `orders/` (bracket/trailing/peg) excluded — verify these aren't depended on by omspy's core (`base.py` / `order.py`). If `order.py` imports from `orders/`, that's a gap.
- [ ] A4: `multi.py` (multi-broker) excluded — confirm not a hard dep of core.
- [ ] A5: `utils.py` "port only what's referenced" — which functions are actually referenced by the in-scope modules? List them.
- [ ] A6: Is there any omspy module NOT listed in §1 at all? (e.g. `omspy/tests/*.py` fixtures, `omspy/__init__.py` re-exports)

### B. Module layout completeness

- [ ] B1: Does the target layout in §2 actually map 1:1 to the in-scope list? Specifically:
  - `base.py` → `broker.rs` — is the whole file one trait, or does it have helper types/functions that should go elsewhere?
  - `order.py` → `order.rs + compound.rs + tif.rs` — is this split correct? omspy's `order.py` has `Order`, `CompoundOrder`, and TIF logic; verify TIF is a clean extract candidate.
  - `simulation/virtual.py` → `paper/engine.rs + paper/book.rs + paper/fills.rs` — is this split clean, or does omspy's virtual.py have tight coupling that forces these into one file?
- [ ] B2: Plan drops `simulation/server.py` (Flask HTTP server). Does any upstream test rely on the HTTP transport? If yes, plan needs a channel-based test shim.
- [ ] B3: `brokers/paper.py` (52 LOC) → `paper/broker.rs`. Is 52 LOC really enough to port, or is it a thin wrapper that pulls significantly from `simulation/`?

### C. LOC / time estimate

- [ ] C1: Plan estimates ~5800 LOC Rust for ~3950 LOC Python core (1.47× ratio). Is this realistic? Rust OMS ports typically land at 1.3-1.7×. Check against:
  - `barter-execution` Rust implementation size vs similar Python OMS
  - `nautilus_trader` has both Python and Rust — ratio?
  - any public crate porting a Python OMS
- [ ] C2: Plan's test LOC is included in the 5800 or not? Re-read §3 and §2. If tests aren't broken out, 5800 is probably prod-only and total with tests is 7500-8000.
- [ ] C3: 7 weeks solo full-time for MVP — is that calibrated? Rust OMS projects from scratch typically take:
  - `barter-execution` development history in git log
  - `rs-clob-client` development history
  Use these as calibration points.
- [ ] C4: R2 (order.rs) budgeted 1200 LOC in 1.5 weeks. `omspy/order.py` is 1468 Python LOC. 1.5 weeks for 1200 LOC Rust state-machine code is tight — state transitions + partial-fill accumulator + TIF expiry + comprehensive tests. Flag if under-scoped.
- [ ] C5: R5 (paper engine + fills) 1400 LOC in 1.5 weeks. The matching engine is algorithmically the densest part. Calibrate against similar implementations.

### D. Design decisions

- [ ] D1: **async-first Broker trait**. omspy's Broker is synchronous. Porting to async changes the semantic contract — e.g. omspy has some patterns like "place_order() returns the Order object directly, caller can immediately mutate it". Async requires awaiting. Does any omspy semantic break under async?
- [ ] D2: **Explicit OrderLifecycle enum**. omspy uses boolean flags. Enum is safer but changes the data model. Does any omspy test rely on flag-level manipulation (e.g. setting `cancelled = True` directly)?
- [ ] D3: **`rust_decimal`** vs Python `Decimal`. Scale handling on arithmetic differs. Plan mentions in risks but no test strategy. Add: golden-value tests for Decimal operations that appear in omspy's original tests.
- [ ] D4: **No SQLite persistence in v1**. omspy has SQLite as optional — is it truly optional or does any in-scope module import `sqlite3`?
- [ ] D5: **`thiserror` error enum**. omspy raises Python exceptions from varied sources. Mapping to Rust Result requires enumerating every exception type omspy raises. Has that enumeration been done? If not, plan is incomplete.

### E. Test strategy

- [ ] E1: Plan mentions "behavior parity spot-check: pick 5 scenarios from `omspy/tests/`". Does `omspy/tests/` have enough scenarios that 5 gives meaningful coverage? Count the test files + test functions.
- [ ] E2: Plan says each phase has its own tests (R2 has transition tests, R5 has paper integration). Is that enough, or does the plan need a dedicated R-phase for parity golden tests against omspy output?
- [ ] E3: `proptest` (property-based testing) is in `Cargo.toml`-to-be but not mentioned in plan. State machines benefit from proptest. Flag as gap.

### F. Phasing

- [ ] F1: R1-R7 order looks sensible (scaffolding → data → state → broker trait → paper engine → paper broker → compound). Can R7 (CompoundOrder) be swapped earlier if needed? No dependency from R6 back to R7, right?
- [ ] F2: Each phase claims "one commit + codex audit ACK before next". Given prior sessions averaged 1-2 NACKs per phase requiring rework, plan should budget audit-induced rework. Does 7 weeks include this or assume clean ACKs?

### G. Hidden creep detection

Prior v1 had hidden scope (venue integration) that looked innocent until audited. Check v2 for:

- [ ] G1: Does any in-scope file silently require porting an "out-of-scope" module? Walk the import graph of each in-scope file.
- [ ] G2: Does the plan leave acceptance open-ended? E.g. "spot-check 5 scenarios" is vague — which 5? Plan v2 should commit to specific scenarios.
- [ ] G3: Does "MVP" gate leave post-MVP work that is actually critical (e.g. observability / error propagation)? If yes, those should move into MVP.

## Deliverables

Write verdict to `~/omsrs/PORT-PLAN-v2-audit-result.md`.

- **ACK**: go write R1.
- **NACK**: list P0 (plan-breaking) / P1 (estimate or scope) / P2 (hardening) findings.

Be adversarial but **stay in scope**. Do not re-raise Polymarket / barter / `/poly/` concerns — those aren't this plan's problem. This plan is a pure library port.
