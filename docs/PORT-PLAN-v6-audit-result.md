# omsrs Port Plan v6 Audit Result

Result: **NACK**.

v6 closes important v5 issues: the portable pytest-item denominator can be made to equal 238, the Ticker exception register now names the real seed-exact id, `VirtualBroker` is no longer described as single-user in the main decision table, `Clock::system` residue is gone, and `ReplicaBroker.run_fill` is correctly described as non-random.

It is still not executable enough to start R1. The main blocker is that v6 has two incompatible meanings for the 238 gate: sometimes `test_ticker_ltp` is inside the 238 as an excused failure, and sometimes it is "excluded" or "not counted." The phase gates also do not sum to the same denominator and R1 undercounts portable `test_utils.py` items.

## P0 Plan-Breaking

### P0.1 The 238 parity gate is internally inconsistent

The portable denominator arithmetic itself is recoverable:

```text
test_base.py                  10
test_models.py                13
test_utils.py                 17
test_order.py                104
test_order_strategy.py         7
simulation/test_models.py     55
simulation/test_virtual.py    32
TOTAL                        238
```

I verified the key parameterized items:

- `tests/test_models.py::test_order_lock_can_methods` collects 3 items.
- `tests/test_utils.py::test_update_quantity` collects 6 items.
- `tests/simulation/test_models.py::test_vorder_is_done` collects 6 items.
- Duplicate names collapse in the three documented files: `test_order.py`, `simulation/test_models.py`, and `simulation/test_virtual.py`.

But v6 does not consistently use that denominator:

- `PORT-PLAN.md` §4 says the 8-item slack covers `test_ticker_ltp`, but also says it is "renamed statistical in Rust, not counted."
- `PORT-PLAN.md` R5 says `55 portable pytest items ... minus 1 test_ticker_ltp ... = 54 against parity gate`.
- `omspy-source-notes.md` §14 says the Ticker row is "Excluded from the 238 pytest-item ceiling."
- `omspy-source-notes.md` §11 includes all 55 `simulation/test_models.py` items as portable, which includes `test_ticker_ltp`.

Those cannot all be true. If `test_ticker_ltp` is excluded from the ceiling, the denominator is 237, not 238. If the denominator is 238, then `test_ticker_ltp` must remain inside the parity ceiling as an explicitly excused failure/replacement, not "excluded" or "not counted."

This is the same class of denominator/gate ambiguity that caused earlier NACKs. The fix is mechanical: choose one model and update every denominator, phase count, LOC calculation, and §14 sentence to match it.

### P0.2 Phase gates do not add up to the advertised pytest-item scope

The phase gates in §8 do not cover 238 pytest-equivalent items as written.

Stated phase counts:

```text
R1  17
R2  10
R3  63
R4  10
R5  54
R6  22
R7  10
R8  41
R9   7
TOTAL 234
```

R1 is the concrete error. It says "17 portable pytest items" but the bullets are:

- 12 portable `test_utils` items
- 3 portable `test_models` items
- 2 proptest modules

Proptest modules are not upstream pytest items. Also, `test_utils.py` has **17** portable pytest items by itself: 4 `create_basic_positions...`, 7 `dict_filter...`, and 6 `test_update_quantity[...]` items. R1 should be 20 upstream pytest items if it owns utils plus `QuantityMatch`/`BasicPosition`, before counting any proptest modules.

Until the phase gates sum cleanly to the chosen parity denominator, the plan cannot be used as a phase-by-phase ACK contract.

## P1 Estimate / Design

### P1.1 R5 remains an optimistic 2-week clean path

R5 still carries almost all of `simulation/models.py`: five enums, OHLC/OHLCV/OHLCVI, Ticker exception design, `VTrade`, `VOrder`, `VPosition`, `VUser`, the response hierarchy, `Instrument`, `OrderFill`, and Clock threading through `VOrder` and responses.

The count is 55 upstream items, or 54 only if the plan truly excludes `test_ticker_ltp`. Two clean weeks might be possible, but v6 still presents it too confidently for a phase that includes both broad model surface and clock injection.

### P1.2 Clock propagation is better, but `CompoundOrder::add` is under-specified

Upstream `CompoundOrder.add(order)` mutates an existing `Order`: it sets `parent_id`, backfills `connection` if missing, may generate an id, indexes the same object, then saves it. The Rust plan says:

> `CompoundOrder::add(order)` - if `order.clock` is default, backfill from `self.clock`.

With `Arc<dyn Clock + Send + Sync>` and a non-optional serde default, "is default" is not reliably detectable. `Order` always has some clock. The plan needs an implementation rule: for example, store `Option<Arc<dyn Clock>>` until finalization, track an explicit `clock_was_supplied` flag, or choose to overwrite on add. Without that, adding an existing `Order` to a `CompoundOrder` with a `MockClock` will be ambiguous.

This also affects `OrderStrategy::add`: v6 says it propagates to the `CompoundOrder`, but does not say whether already-contained orders are cascaded or left with their existing clocks.

### P1.3 Broker response clock propagation has small source inaccuracies

`VirtualBroker.order_place` constructs both `VOrder` and `OrderResponse`; v6 covers that. But `VirtualBroker.order_modify` and `order_cancel` also construct `OrderResponse` instances, and the method-level propagation list only says they use `self.clock` for timestamp updates. Response construction in those paths should be explicitly covered too.

`ReplicaBroker.order_place` constructs `VOrder` and `OrderFill`, but upstream does not construct an `OrderResponse`. v6 says "ReplicaBroker.order_place - same" and the response table says `Response::new` receives clocks from `VirtualBroker / ReplicaBroker`. That overstates the upstream surface.

### P1.4 The Ticker accountability process has a loophole

The exact confirmed exception is now correctly named as `tests/simulation/test_models.py::test_ticker_ltp`. However §14 says `test_ticker_ticker_mode` is portable while also saying "flaky-skip allowed via `#[ignore]` if needed."

That is not binding enough. `test_ticker_ticker_mode` asserts `ticker.ltp != 125` after switching back to random mode. Because `Ticker.ltp` rounds a normal perturbation to 0.05, there is a real non-zero chance the rounded price remains 125. If this test is allowed to become ignored, it must become an exact §14 exception at the R5 gate with codex approval. It should not have a pre-authorized ignore escape hatch.

## P2 Hardening / Source Notes

### P2.1 `omspy-source-notes.md` §11 has wrong total pytest-item rows

The portable sum can be 238, but the table's total pytest-item counts are wrong:

- `test_utils.py` is **34** pytest items, not 22. `test_stop_loss_step_decimal` has 8 items and `test_update_quantity` has 6.
- `test_order.py` is **107** pytest items, not 105, because excluded `test_get_option` has 3 parameterized items.
- Listed-file total is therefore **334**, not 320.
- The note under the table says `test_utils.py` has `22+5+8=35` items; the correct arithmetic is 34 because the base 22 function names already include one `test_stop_loss_step_decimal` body and one `test_update_quantity` body.

This does not change the portable denominator if exclusions are applied correctly:

```text
test_utils.py: 34 total - 17 excluded = 17 portable
test_order.py: 107 total - 3 excluded = 104 portable
```

But the table is still not a reliable source of truth as labeled.

### P2.2 `OHLCVI` necessity is still misstated in source notes

`PORT-PLAN.md` correctly says `OHLCVI` is kept for test parity, not because it is transitively required. But `omspy-source-notes.md` §12 says:

> `OHLCVI` - MVP (reversed) - Same inheritance chain.

Upstream `VQuote` inherits `OHLCV`, not `OHLCVI`. Keeping `OHLCVI` for `test_ohlcvi` is fine; saying it is required by the inheritance chain is residue.

### P2.3 `rand = "=0.8"` has a false rationale

The dependency table says:

```toml
rand = { version = "=0.8", features = ["small_rng"] } # major locked for OrderFill determinism
```

Upstream `OrderFill.update()` and `ReplicaBroker.run_fill()` have no RNG. The only confirmed RNG parity exception is Ticker. If `rand` is pinned, the rationale should be Ticker statistical reproducibility or test repeatability, not OrderFill. Also, exact `=0.8` is stricter than a major/minor policy and should be deliberate; a lockfile can freeze patch versions for builds without baking an exact patch into the public manifest.

### P2.4 `test_ticker_ohlc` is not a behavior assertion

§14 says `test_ticker_ohlc` is portable "behavior only." Upstream calls equality expressions but never asserts them:

```python
ticker.ohlc() == dict(open=125, high=125, low=125, close=125)
...
ticker.ohlc() == dict(open=125, high=125, low=116.95, close=120)
```

A Rust port should decide whether this is a no-op smoke test or whether to add real assertions as an intentional strengthening. Calling it behavior parity is misleading.

### P2.5 `rstest` is not listed

v6 plans to preserve pytest-item granularity, including parameterized cases, but the dependency table only lists `proptest` under dev-dependencies. This is not a blocker if the port duplicates test functions or writes explicit loops with distinct names, but if it expects pytest-style parameter IDs, add `rstest` or document the manual approach.

## Checklist Status

- A1: **partially closed**. The portable 238 arithmetic is right, and the requested parameterized items are verified. The table's total pytest-item rows and phase gates are wrong.
- A2: **mostly closed**. Exact `test_ticker_ltp` id is listed. The other Ticker tests are correctly identified at source level, except `test_ticker_ohlc` is no-assert and `test_ticker_ticker_mode` has a pre-authorized ignore loophole.
- A3: skipped per instructions.
- B1: **closed enough**. A simple keyword split gives 40 `test_compound_order_*` names and 64 other portable names; v6's 41/63 split is within the requested tolerance.
- B2: **not closed**. R5 remains tight and should be marked as high-risk.
- B3: **closed**. No `Clock::system` residue found in `PORT-PLAN.md`.
- B4: **partially closed**. Main construction sites are named, but `CompoundOrder::add` backfill mechanics and response clocks in modify/cancel paths need tightening.
- B5: **closed**. `VirtualBroker` is marked multi-user in §12 and MVP totals.
- B6: **partially closed**. The approval rule exists, but `test_ticker_ticker_mode` has a skip loophole.
- C1: **closed**. `OrderFill.update()` and `ReplicaBroker.run_fill()` have no RNG.
- C2: **closed with note**. Dependencies are listed, but the rand rationale is wrong.
- C3: **partially closed**. `tick`, multi-user VirtualBroker, and Rust test LOC are updated; §11 totals and `OHLCVI` rationale still have residue.
- C4: **OK in plan**, but source notes overstate necessity.
- C5: **acceptable for MVP** if MockClock/tolerance rules are enforced at phase gates.
- D1: **math OK**. 15.75 + 0.5 = 16.25, rounded to ~16; expected ~31 follows the stated rework model.
- D2: **math OK**. 238 × 20 + 500 + 300 + 200 = 5760.
- D3: **needs correction**. Rand lock rationale is false for OrderFill.
- D4: **closed**. Three proptest modules are planned.
- D5: **not closed**. Existing-order clock backfill needs an implementable Rust ownership/default-detection rule.
- E1: **partially closed**. Legacy section is labeled, but the new §11 table still has wrong total rows.
- E2: **closed**. `utils.tick` is defer and `VirtualBroker` is multi-user.
- E3: **closed for exact id**. No vague "+ related" remains.
- E4: **closed**. MVP totals mention multi-user VirtualBroker.
- F1: **no blocker found**. `Arc<dyn Clock + Send + Sync>` may be stricter than necessary, but it is a reasonable conservative bound.
- F2: **no blocker found**. `parking_lot::Mutex<DateTime<Utc>>` is usable for a `Send + Sync` `MockClock` under default features.
- F3: **P2 only**. Add `rstest` or document duplicated parameter-case tests.

## Required Fixes Before ACK

1. Decide whether `test_ticker_ltp` is inside the 238 denominator as an excused/replaced item, or excluded from the denominator. Update §4, §8 R5, §11, §14, LOC math, and acceptance criteria consistently.
2. Fix §8 phase gates so the upstream pytest-item counts sum to the chosen denominator. R1 must not count proptest modules as pytest items and must include all 17 portable `test_utils.py` items if it owns utils.
3. Specify an implementable Clock backfill rule for `CompoundOrder::add(existing_order)` and whether `OrderStrategy::add` cascades clocks into already-contained orders.
4. Remove the pre-authorized `#[ignore]` loophole for `test_ticker_ticker_mode`; any ignored/flaky Ticker case must become an exact §14 exception approved at the R5 gate.
5. Clean `omspy-source-notes.md` §11 total pytest-item rows and the stale `OHLCVI` inheritance rationale.
6. Correct the `rand = "=0.8"` rationale, and either justify the exact pin for Ticker repeatability or loosen it.

