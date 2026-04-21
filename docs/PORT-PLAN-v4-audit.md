# Audit: omsrs Port Plan v4

Adversarial audit. Author (Claude) is NACK'd from self-auditing.

**Read first**:
- `~/omsrs/PORT-PLAN.md` (v4 — the plan under audit)
- `~/omsrs/omspy-source-notes.md` (source notes, now v4-corrected)
- `~/omsrs/PORT-PLAN-v3-audit-result.md` (prior NACK — v4 must close its findings)

## Context

Audit chain:
- **v1 NACK**: scope creep (Polymarket / barter mixed in).
- **v2 NACK**: file-level scope, invented `OrderRequest` type that doesn't exist, mis-mapped `simulation/virtual.py`.
- **v3 NACK**: 3 P0 + 5 P1 + 8 P2:
  - P0.1 `simulation/models` inheritance chain broken (`VQuote : OHLCV`, `VirtualBroker.tickers: Dict[str, Ticker]`, etc.) — `OHLC/V/VI/Ticker` deferred then used by MVP symbols.
  - P0.2 `OrderType` + `Status` enum values wrong (invented `SL/SL-M`, mis-named `PARTIAL`).
  - P0.3 Persistence feature flag only covered save methods, not the many call sites that read `self.connection` inside `Order.update/execute/modify` and `CompoundOrder.add/add_order/save`.
  - P1.1-1.5: inflated portable-test count, phase-test promises referenced deferred symbols, test LOC 2.5× undercounted, R10 too short, audit rework not budgeted.
  - P2.1-2.8: acceptance criteria vague, `OrderLifecycle` dual-source-of-truth, decimal comparison rules undefined, tz behavior underspecified, SQLite schema freeze, yaml extension point, proptest scope.

v4 claims to close all of the above.

## Audit stance

Stay in scope. Pure Rust library port of omspy core. Do NOT re-raise Polymarket / barter / `/poly/` / pbot concerns.

Be adversarial on **whether the v4 fixes actually hold**. Prior plans looked sensible but hid errors under "MVP / defer" framing. The task now: walk every v3 finding and check v4 closes it.

## Audit checklist

### A. v3 P0 closure verification

- [ ] A1 (P0.1 sim/models inheritance): v4 §1 says `OHLC/OHLCV/OHLCVI/Ticker` are now MVP. Verify:
  - `omspy-source-notes.md` §12 decision table reflects this (should say `MVP (reversed)`).
  - `~/omsrs/PORT-PLAN.md` §2 MVP table lists them.
  - `~/omsrs/PORT-PLAN.md` §3 LOC table includes them (~90 Python LOC added).
  - v4 R5 phase includes their parity tests.
  - Confirm all 4 types are really needed — or if any is transitively unused after reconsideration.
- [ ] A2 (P0.2 enum values): walk each enum:
  - `Status` in v4 notes should list `COMPLETE=1, REJECTED=2, CANCELED=3, PARTIAL_FILL=4, OPEN=5, PENDING=6`. Verify against `~/refs/omspy/omspy/simulation/models.py:16-23`.
  - `OrderType` in v4 notes should list `MARKET=1, LIMIT=2, STOP=3`. Verify against `sim/models.py:40-43`.
  - `Side`, `ResponseStatus`, `TickerMode` — also verify.
- [ ] A3 (P0.3 persistence unified): v4 §6 D8 declares `trait PersistenceHandle { fn save_order(&self, &Order) -> Result<(), OmsError>; }` unconditionally. Every call site in the upstream that reads `self.connection` becomes `if let Some(h) = self.connection.as_ref() { h.save_order(self); }` — no cfg-split needed in method bodies. Verify this design actually removes the cfg-split everywhere, and specifically:
  - `Order.update` (`order.py:454`)
  - `Order.execute` (`order.py:521`)
  - `Order.modify` (`order.py:612`)
  - `Order.save_to_db` (`order.py:660`)
  - `CompoundOrder.add_order` (`order.py:863`)
  - `CompoundOrder.add` (`order.py:1239`)
  - `CompoundOrder.save` (`order.py:1258`)
- [ ] A4 (P0.3 correction): v4 `omspy-source-notes.md` should no longer claim "`CompoundOrder.__init__` conditionally calls `create_db`" (that was false in v3). Verify corrected.

### B. v3 P1 closure verification

- [ ] B1 (P1.1 test ceiling): v4 cites 229 portable tests. Walk upstream and verify the per-file counts in v4 §3/§4 table. Especially tricky: `test_simulation_virtual.py` breakdown — how many are FakeBroker only? `generate_*` only? VirtualBroker? ReplicaBroker?
- [ ] B2 (P1.2 per-phase tests): v4 §7 names exact include/exclude per phase. Verify they're internally consistent:
  - R1 lists 12 `test_utils.rs` tests. Upstream has 22. Confirm the 10 excluded map to the 3 deferred/dropped helpers.
  - R4 lists 10 `test_base.rs` tests. Upstream has 12. Confirm the 2 excluded are `cover_orders`.
  - R3 lists 105 of 106. Confirm the 1 excluded is `test_get_option`.
- [ ] B3 (P1.3 test LOC): v4 budgets 5400 test LOC (229 × 20 + 500 fixture + 300 proptest). Is 20 LOC/test realistic for Rust parity testing? Sample upstream `test_order.py` test sizes — 30+ of the 106 are probably 5-10 lines; some are 40+. Weighted average of 20 might still be low.
- [ ] B4 (P1.4 R10 extended): 3 weeks. Reasonable for 220+ test parity pass + decimal/tz stabilisation?
- [ ] B5 (P1.5 audit rework): v4 §7 totals table gives clean-path 14 + expected 22-28. Is this honest? With 10 phases × avg 1.5 NACK rework = 15 weeks rework on top of 14 → 29. v4 says 22-28. Check the arithmetic.

### C. v3 P2 closure verification

- [ ] C1 (P2.1 binary acceptance): v4 §8 criterion 3 now says "≥ 220 pass; failure requires excuse entry". Is this truly binary? What if 219 pass?
- [ ] C2 (P2.2 vague "aggregate like upstream"): v4 §8 criterion 10 cites specific tests. Verify they exist in upstream.
- [ ] C3 (P2.3 `OrderLifecycle` dual-source): v4 §6 D2 makes `OrderLifecycle::from_order(&Order)` a pure function, not stored. Check that no codepath independently mutates lifecycle without going through `Order` fields.
- [ ] C4 (P2.4 decimal rules): v4 §6 D3 lists exact rules. Are epsilons right for MTM / average price / spread?
- [ ] C5 (P2.5 tz + Clock): v4 §6 D4 specifies `Clock` trait + `MockClock`. Does every tz-sensitive upstream location actually have the Clock threaded through? Audit `OrderLock.creation_lock_till`, `Order.time_to_expiry`, `Timer.has_started`, etc.
- [ ] C6 (P2.6 SQL schema freeze): v4 §6 D7 says schema frozen. Does it name the 36 columns or just promise they'll match?
- [ ] C7 (P2.7 yaml extension): v4 §6 D5 notes downstream crates can add yaml. OK.
- [ ] C8 (P2.8 proptest scope): v4 names 3 property test modules (`quantity.rs`, `lifecycle.rs`, `position.rs`) with specific invariants. Verify these invariants are meaningful.

### D. New issues v4 might have introduced

v4 added `OHLC/V/VI/Ticker` + `Clock` trait + `PersistenceHandle` trait. Fresh bugs possible.

- [ ] D1: Does adding `Ticker` as MVP pull in any new dependency? `Ticker` has a `TickerMode` enum + `last_price`, random generation — does it use `FakeBroker.generate_price` internally? If so, part of `FakeBroker` must come along.
- [ ] D2: `PersistenceHandle` with `save_order(&Order)` — upstream `save_to_db` writes the Order to a sqlite_utils table. Rust port's `SqliteHandle` impl would need `rusqlite`. Does the trait signature actually cover all save semantics (update / upsert, delete)?
- [ ] D3: `Clock` trait + `MockClock`. Ecosystem has `mock_instant`, `quartz_sched::Clock`, `tokio::time::Instant::pause`. Is there a reason not to use one? Flag if rolling our own is inventing a crate someone else maintains.
- [ ] D4: v4 expands simulation/models.py port size (~540 LOC) which now drags in all enums + all responses + models. R5 is 1.5 weeks for 540 LOC → ~360 LOC/week. Feasible but tight for struct-heavy serde derives + 51 parity tests.

### E. Source-notes consistency

- [ ] E1: The decision table in v4 `omspy-source-notes.md` §12 must list `OHLC/V/VI/Ticker` as MVP. Re-check.
- [ ] E2: Status enum values in notes must match upstream.
- [ ] E3: The wrong `CompoundOrder.__init__` claim has been removed. Re-check.
- [ ] E4: §13 "Key non-obvious dependencies" item 4 has been revised. Re-check.

### F. Things still likely to break

- [ ] F1: Paper test suite (`test_base.py`) partly depends on override yaml loading. Since MVP drops yaml, are those tests portable? List which.
- [ ] F2: `CompoundOrder.check_flags` involves time/expiry math and convert-to-market logic. Are any of its tests deferred, or all 106 count?
- [ ] F3: Does `Order.execute` calling `broker.order_place` require the broker to be a trait object in Rust? If `Broker` is an `Arc<dyn Broker>` on `Order`, check serialization — can't derive serde over trait objects. Plan § unclear.

## Deliverables

Write result to `~/omsrs/PORT-PLAN-v4-audit-result.md`.

- **ACK**: v4 sound, start R1.
- **NACK**: list P0 (plan-breaking) / P1 (estimate/scope) / P2 (hardening).

Be adversarial but stay in scope. Don't re-raise Polymarket / barter / `/poly/` — not this plan's problem.
