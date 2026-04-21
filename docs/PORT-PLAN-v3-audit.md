# Audit: omsrs Port Plan v3

Adversarial audit. Author (Claude) is NACK'd from self-auditing.

**Read first**:
- `~/omsrs/PORT-PLAN.md` (v3 — the plan under audit)
- `~/omsrs/omspy-source-notes.md` (symbol-level inventory the plan is built from)

## Context + audit stance

v1 NACK: plan silently mixed in Polymarket / barter-rs / `/poly/` integration scope.
v2 NACK: plan was file-level (not symbol-level), invented types that don't exist in omspy (`OrderRequest`), mis-mapped `simulation/virtual.py` to a fictional "matching book" abstraction, claimed `brokers/paper.py` binds to `simulation/` (it doesn't), underestimated LOC by 2-3×, gave vague acceptance criteria.

v3 was rewritten from scratch after reading every in-scope omspy source file. `omspy-source-notes.md` is the symbol-level source of truth the plan is built from.

**Audit stance**: stay in scope. This plan is a pure Rust library port of omspy's core. Do NOT re-raise Polymarket / barter / `/poly/` integration questions — those are downstream, not this plan's problem.

**Stance on author bias**: prior plans have smuggled in reinvention, under-estimated LOC, and given vague acceptance. This plan must survive checks for all three.

## Audit checklist

### A. Source-notes accuracy

- [ ] A1: Walk every entry in `omspy-source-notes.md` §12 (the decision table). For each symbol marked MVP, verify it actually exists in upstream source at the cited path. Spot-check 10.
- [ ] A2: For each symbol marked `defer` / `drop`, verify the rationale is accurate. In particular:
  - `get_option` — is it really only used by tests + downstream, not by `Order` itself?
  - `load_broker` — confirm it imports Indian brokers (zerodha, finvasia, icici, neo, noren).
  - `stop_loss_step_decimal` — confirm not referenced by in-scope modules.
  - `Tracker` / `Timer` / `TimeTracker` / `Candle` / `CandleStick` — confirm none are dependencies of `Order` / `CompoundOrder` / `OrderStrategy`.
- [ ] A3: The notes claim upstream has no "matching book" abstraction. Verify: is there any book-level matching in `VirtualBroker` or `ReplicaBroker` that the plan missed?
- [ ] A4: The notes claim `brokers/paper.py` doesn't bind to `simulation/`. Verify via imports in `brokers/paper.py`.

### B. Missing MVP symbols

Walk every MVP symbol's upstream source and check for hidden deps:

- [ ] B1: `Order.__init__` calls `OrderLock(timezone=...)`. `OrderLock` is MVP. Any other init-time dependency?
- [ ] B2: `CompoundOrder.__init__` conditionally calls `create_db` when `connection is None`. Plan puts `create_db` behind `persistence` feature. If `persistence` is off, what does `CompoundOrder::new()` do? Plan says fields don't exist — verify the field removal is consistent with all code paths that use `connection`.
- [ ] B3: `base.Broker.get_positions_from_orders` → `utils.create_basic_positions_from_orders_dict` + `dict_filter` + `models.BasicPosition`. All MVP. Plan OK.
- [ ] B4: `simulation/models.VOrder` calls `utils.update_quantity`. All MVP. Plan OK.
- [ ] B5: `simulation/models.OrderFill` referenced by `ReplicaBroker.run_fill`. Both MVP. But: does `OrderFill` reference any deferred type?
- [ ] B6: `base.Broker.cover_orders` is deferred. Plan says that removes the dependency on `utils.tick`. Verify `tick` isn't used by any other MVP symbol.
- [ ] B7: `base.Broker` uses `utils.*` (`from omspy.utils import *`). Check that ALL star-imported symbols are either MVP or unused. Specifically: `UQty`, `create_basic_positions_from_orders_dict`, `dict_filter`, `tick`, `stop_loss_step_decimal`, `update_quantity`, `load_broker`. Plan drops `tick` (with `cover_orders` deferred), `stop_loss_step_decimal`, `load_broker`. Verify `base.py` doesn't silently use `tick` elsewhere.

### C. Parity test coverage

- [ ] C1: Plan targets "~220 parity tests pass out of 316 in-scope upstream". Verify: upstream `tests/test_order.py` alone has 106 tests. How many reference deferred symbols (e.g. `tick` or `Candle`)? If many do, the 106 available for port may be much smaller.
- [ ] C2: Test counts claimed in `source-notes.md` §11:
  - `tests/test_base.py` 12
  - `tests/test_models.py` 11
  - `tests/test_models_tracker.py` 8 — **deferred, not ported**
  - `tests/test_models_candles.py` 19 — **deferred, not ported**
  - `tests/test_utils.py` 22
  - `tests/test_order.py` 106
  - `tests/test_order_strategy.py` 7
  - `tests/simulation/test_models.py` 51
  - `tests/simulation/test_virtual.py` 80 — `FakeBroker` tests not ported; how many of the 80 are for FakeBroker?
  - Subtotal of portable: 12 + 11 + 22 + 106 + 7 + 51 + (80 − FakeBroker tests) = **~280 ceiling**.
  
  Plan's 220 target leaves ~60 test budget for failures / deferred cases. Is that right? Grep the actual upstream test files to confirm the 80 `test_virtual.py` breakdown between FakeBroker / VirtualBroker / ReplicaBroker.
- [ ] C3: Property-based tests (`proptest`) are mentioned in R10 but no count. Is ad-hoc proptest fine, or does the plan need to specify property count?

### D. LOC estimate realism

Plan says ~4900 prod + ~2050 tests = ~6950 Rust LOC, 12 weeks solo FT.

- [ ] D1: Python MVP LOC in `source-notes.md` = 2760. Rust multiplier 1.4-1.6×. Plan's 4900 is 1.78× — **higher** than the stated multiplier. Is that overshooting or right? State clearly: this plan already built in a safety margin.
- [ ] D2: Test LOC budget 2050 for 220 tests = ~9.3 LOC/test. Typical Rust test LOC is 15-25 with fixtures. If upstream tests port at 20 LOC each, 220 × 20 = 4400 LOC, more than 2× budget. Flag.
- [ ] D3: Phase R3 (Order) budgeted 1100 prod + 500 test = 1600 LOC in 2 weeks. That's 800 LOC/week. Historical Rust velocity is 600-800 LOC/week. Feasible but no slack.
- [ ] D4: Phase R10 (parity sweep) budgeted 1.5 weeks with no new LOC. That's surprisingly low for running 220 tests + fixing gaps. Typical parity-pass + stabilisation phase is 3-4 weeks.
- [ ] D5: Audit rework not budgeted (§8.R.6 notes 1-2 weeks per cycle). With 10 phases at 1-2 NACKs each historical, that's 10-20 weeks of rework. The 12-week MVP estimate is implicitly assuming 100% clean ACKs per phase. Call this out.

### E. Design decisions

- [ ] E1: **Sync Broker trait** (D1). Codex's prior v2 audit recommended async. v3 reverses that for faithful port. Both are defensible but plan should explicitly say: downstream async users must wrap the sync trait in their own async layer. Does the plan say this clearly?
- [ ] E2: **Status as `Option<String>` + parallel `OrderLifecycle` enum** (D2). Dual-source-of-truth risk: what keeps them in sync? Plan says "both stay in sync via `Order.update()`". Is that tested? Enforced by invariant?
- [ ] E3: **`rust_decimal` everywhere** (D3). omspy uses `float` freely. Ratio tests that compare Python `float` to Rust `Decimal` will have epsilon issues. How does the plan handle this in parity tests? Tolerance values? Convert Decimal to f64 for comparison?
- [ ] E4: **chrono vs pendulum** (D4). pendulum has quirks chrono doesn't (e.g. `pendulum.now(tz="local")` silently uses system TZ; chrono requires explicit `Local::now()`). OrderLock's lock-til expects tz-aware comparison. Plan mentions MockClock injection for tests but not the production path. Is there a drift risk?
- [ ] E5: **Override via HashMap, no yaml** (D5). Fine for MVP. Does the plan note that downstream adapters may add yaml loading in their own crate?
- [ ] E6: **SQLite feature flag** (D7). Plan puts `connection` behind `#[cfg(feature = "persistence")]`. But the upstream `create_db` schema has 36 columns — if any new column is needed later, it's a breaking schema change. Does the plan acknowledge schema freezing?

### F. Phase ordering sanity

- [ ] F1: R1 starts with utils + QuantityMatch + BasicPosition. But `base.Broker.get_positions_from_orders` (which R4 adds) requires all of these. Any circular dep?
- [ ] F2: R4 adds `Broker` trait + `Paper`. But `Paper` is just a dummy — is there any hidden dep that pulls in simulation types? Upstream `Paper` import = `from omspy.base import Broker, pre, post`. Clean.
- [ ] F3: R6 adds `VirtualBroker` before R7 adds `ReplicaBroker`. But `VirtualBroker.clients` → `VUser` → in simulation models. Plan puts simulation/models in R5. Order OK.
- [ ] F4: R8 (CompoundOrder) after R3 (Order) — correct, CompoundOrder needs Order. But R3 already budgets Order's save_to_db (via persistence feature). CompoundOrder.save wires in at R9. Any dep reversed?
- [ ] F5: R9 (OrderStrategy + persistence wiring) — bundles two somewhat independent concerns. Is this load-balancing the plan's time-per-phase, or is OrderStrategy intrinsically tied to persistence?

### G. Hidden reinvention check

- [ ] G1: Plan's `src/utils.rs` includes `create_basic_positions_from_orders_dict`. Does this function already exist in any Rust OMS crate on crates.io or in `barter-execution`? If yes, flag — omsrs should depend on it, not reimplement.
- [ ] G2: `OrderLifecycle` enum (D2) — is this reinventing a state machine crate like `rust-fsm`? Flag if yes.
- [ ] G3: `Broker` trait with `place_order/cancel_order/modify_order` — does `barter-execution::client::ExecutionClient` cover this? If yes, the plan should at minimum depend on that trait (not re-implement). However, omspy's `Broker` is sync and includes `orders/trades/positions` getters that `ExecutionClient` doesn't — may be genuine divergence. Decide.

### H. Acceptance criteria robustness

- [ ] H1: §6 has 10 acceptance criteria. Number 8 (ReplicaBroker fills deterministic given RNG seed) is testable. Criterion 9 ("aggregate positions + MTM like upstream") is vague — what counts as "like upstream"?
- [ ] H2: §6 criterion 3 says "all parity tests pass (target ≥ 220)". "Target" is ambiguous — is it a hard gate or a soft target? Plan should say: if ≥ 220 pass, ACCEPT; if < 220, reject MVP.

### I. What the plan could still hide

Look for scope creep at phase-boundary:

- [ ] I1: Does any phase secretly require a symbol not listed in §1 MVP set?
- [ ] I2: Does R10 "parity sweep + stabilisation" cover all the phases, or only some?
- [ ] I3: Plan mentions "clock injection for tests" in §8.R.2 but not in test plan. Is a `MockClock` type part of MVP or deferred?

## Deliverables

Write result to `~/omsrs/PORT-PLAN-v3-audit-result.md`.

- **ACK**: MVP scope + LOC + phase plan all sound, R1 can start.
- **NACK**: enumerate P0 (plan-breaking), P1 (estimate / scope), P2 (hardening).

Be adversarial but stay in scope. Don't re-raise Polymarket / barter / `/poly/` — not this plan's problem.
