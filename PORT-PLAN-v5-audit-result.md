# omsrs Port Plan v5 Audit Result

Result: **NACK**.

v5 fixes some v4 findings: the SQLite DDL is now the upstream 37-column schema, the R6/R7 `test_simulation_virtual.py` split is correct at function-name level, and `OrderFill` itself does not hide a Python RNG. But v5 is still not sound enough to start R1. The largest issue is that the plan repeatedly says "pytest-collected" while still counting collected function names, not pytest items. A second v4 P0 remains only partially closed: the Ticker exception register is still not explicit.

## P0 Plan-Breaking

### P0.1 The 226 denominator is still not the pytest-collected portable ceiling

v5 says the parity ceiling is **226 pytest-collected portable tests**. That is factually wrong if "pytest-collected" means pytest items. It is only the de-duplicated test-function-name count.

Included parameterized tests add 12 portable pytest items:

- `tests/test_models.py::test_order_lock_can_methods` has 3 items, not 1 (`can_create`, `can_modify`, `can_cancel`) => +2.
- `tests/test_utils.py::test_update_quantity` has 6 items, not 1. `test_stop_loss_step_decimal` is excluded, so its 8 items do not count => +5.
- `tests/simulation/test_models.py::test_vorder_is_done` has 6 items, not 1 => +5.
- `tests/test_order.py::test_get_option` has 3 items but is excluded, so it does not change the portable count.

Correct portable pytest-item arithmetic is:

```text
test_base                  10
test_models                13
test_utils                 17
test_order                104
test_order_strategy         7
simulation/test_models     55
simulation/test_virtual    32
TOTAL                     238
```

Evidence: `python3 -m pytest --collect-only -q ...` expands the included parameterized item ids before collection aborts on out-of-scope `nodriver` imports; AST inspection confirms the same parametrization. v5's `10+11+12+104+7+50+32=226` is a function-name count, not a pytest-collected item count.

Impact: acceptance criterion `>=218 of 226` is invalid. If the intended gate remains 96.5%, the equivalent item gate is about **>=230 of 238**, not 218. If the plan intentionally wants one Rust parity test per Python function name, stop calling it pytest-collected and update every gate/LOC statement accordingly.

### P0.2 Ticker RNG exception is not actually enumerated

v5 claims `omspy-source-notes.md` §14 lists the specific excluded Ticker tests. It does not. The only explicit row is:

```text
tests/simulation/test_models.py::test_ticker_ltp (+ related `Ticker` seed-dependent tests)
```

and the section ends with:

```text
Pending audit: which specific `test_ticker_*` tests rely on seeded determinism vs structural behavior.
```

That is not an exception register; it is a placeholder.

Upstream `tests/simulation/test_models.py` has six collected Ticker functions:

- `test_ticker_defaults` at line 439: structural, no RNG.
- `test_ticker_is_random` at line 448: structural, no RNG.
- `test_ticker_ltp` at line 455: `random.seed(1000)` plus exact `_ltp/_high/_low` assertions. This is the real seed-exact exception.
- `test_ticker_ohlc` at line 465: calls random mode, but has no `assert` statements for the equality expressions.
- `test_ticker_ticker_mode` at line 473: checks manual mode and then asserts random-mode `ltp != 125`; this is probabilistic, not seed-exact.
- `test_ticker_update` at line 483: manual update behavior, no RNG.

v5 R5 says "minus seed-dependent `test_ticker_*`, expected ~3-4 excused." The source supports **one** seed-exact test, plus possibly one flaky/probabilistic random-mode test if the port chooses to excuse it. The 3-4 number is unsupported, and §14 does not list the exact IDs. This fails the v4 P0.1 closure requirement.

Fix: list each excused test id exactly, with exception type. Do not use "`+ related`". Decide whether `test_ticker_ltp` is excluded, rewritten as a Rust statistical test, or both; v5 currently says both "excluded" and "becomes a statistical assertion."

## P1 Estimate / Scope

### P1.1 R3/R8 still double-count `test_order.py`

v5 R3 says **104 of 105** `test_order` tests are in scope while also saying the CompoundOrder portion is deferred to R8. That is not an executable phase gate.

After duplicate elimination and excluding `test_get_option`, `test_order.py` has 104 portable function names. A simple source classification gives:

- **63 Order/db/order-lock-ish tests**
- **41 CompoundOrder-ish tests**

The R8 estimate of "~60 of 104" CompoundOrder tests is not supported by upstream names/source. R3 must either gate only the Order-only subset, or R3 must implement CompoundOrder too. As written, R3 and R8 both claim the same file-level count.

### P1.2 R5 remains optimistic and now has the wrong count shape

`simulation/models.py` is listed as 50 pytest-collected tests, but actual pytest items are 55 because `test_vorder_is_done` has six cases. The phase also includes:

- 5 enums.
- OHLC/OHLCV/OHLCVI/Ticker.
- `VTrade`, `VOrder`, `VPosition`, `VUser`.
- 9 response classes including timestamp behavior.
- `Instrument`.
- 12 `OrderFill` function-name tests.
- `VOrder._modify_order_by_status` random partial/pending branches.
- Ticker exception design and clock threading.

Two clean weeks is still tight. It may be possible, but it should not be represented as a reliable clean-path estimate.

### P1.3 Clock design is directionally right, but the details are inconsistent

The required clock use-sites are mostly identified: `Order`, `OrderLock`, `VOrder`, `Response`, and broker paths that construct those types. But v5 still has design gaps:

- §1 uses `#[serde(skip, default = "Clock::system")]`; that path is not a sound serde default unless an exact callable returning the field type exists. §6 later uses `clock_system_default`, which is the right shape. Use one explicit free function returning `Arc<dyn Clock + Send + Sync>`.
- `#[serde(skip, default = "clock_system_default")]` is semantically workable for a trait object field because skipped fields do not require `Serialize`/`Deserialize`, but the default function must return the exact owned field type. The plan should say that explicitly.
- The D4 table says `VirtualBroker.get` has a wall-time filter. Upstream `VirtualBroker.get` itself does not read time; it calls `VOrder.modify_by_status`, which gates on `VOrder.is_past_delay`. The real propagation requirement is `VirtualBroker.order_place` and `ReplicaBroker.order_place` passing `broker.clock` into constructed `VOrder`s, and response constructors receiving the same clock.
- `ReplicaBroker` is named as a clock container but has no D4 table row. It needs an explicit "constructs `VOrder` in `order_place`" row.

### P1.4 R6/R7 split is fixed, but source notes still contradict it

The v5 R6/R7 split checks out at function-name level:

- `test_simulation_virtual.py`: 22 collected `VirtualBroker` function names after duplicate `test_virtual_broker_ltp` elimination.
- 10 collected `ReplicaBroker` function names.

However, `omspy-source-notes.md` §12 still says `VirtualBroker` is "single-user MVP" and "users feature defers", and the MVP summary still says `VirtualBroker` (single-user MVP). That contradicts v5 §1/R6 and must be fixed before the notes can be used as implementation guidance.

### P1.5 The 8-test slack is not auditable

v5 says the 8-test slack covers ~3-4 Ticker tests and ~2-3 timezone/DST edge cases. §14 only names `test_ticker_ltp` vaguely and names no timezone/DST tests.

Time-sensitive portable candidates include, at minimum:

- `test_order_lock_defaults`, `test_order_lock_methods`, `test_order_lock_methods_max_duration`, `test_order_lock_can_methods[...]`.
- `test_order_update_timestamp`, `test_order_expires`, `test_order_expiry_times`, `test_order_has_expired`, `test_order_timezone`, and order-lock tests in `test_order.py`.
- `test_response`, `test_vorder_is_past_delay`, `test_vorder_custom_delay`, `test_vorder_modify_by_status*`.
- `test_virtual_broker_order_place_*`, `test_virtual_broker_get_order_by_status`, and `test_replica_broker_order_place`.

If any of these are expected to fail, they need exact §14 entries. Otherwise, the slack should not mention them.

## P2 Hardening / Consistency

### P2.1 SQLite schema closure is good

v5 §6 D7 matches upstream `order.py::create_db` exactly: 37 columns, same order, same type affinities, and uppercase `JSON`. This v4 P0.3 is closed.

### P2.2 `OrderFill` RNG concern is inverted

Upstream `OrderFill` and `ReplicaBroker.run_fill` do **not** use Python `random`. `OrderFill.update()` is deterministic from order state and last price. The random branches found in `simulation/models.py` are in `VOrder._modify_order_by_status` for `PARTIAL_FILL` and `PENDING`, and the portable tests assert inequalities/status behavior, not exact random draws.

Acceptance criterion 9 ("`ReplicaBroker.run_fill` byte-exact for seed `0xDEADBEEF`") is therefore source-inaccurate. If Rust adds `SmallRng` to `OrderFill`, that is extra behavior not present upstream. If `SmallRng` remains elsewhere, lock the `rand` major version; but `run_fill` itself should not need a seed.

### P2.3 Dependency plan is incomplete

v5 mentions `rand_distr::Normal`, `SmallRng`, and `parking_lot::Mutex`, but there is no Cargo manifest yet and no dependency table. Add explicit planned dependencies and versions/features, at least:

- `rand` with `small_rng` feature, major version locked.
- `rand_distr`.
- `parking_lot`.
- timezone support crate(s), if using `chrono_tz::Tz`.
- `serde` derives with skipped defaults for trait-object fields.

### P2.4 Source notes still contain stale v4/v5 leftovers

`omspy-source-notes.md` needs cleanup:

- §11's first table still labels body counts as "Tests" and says in-scope totals 316, while the revised table below says 313/226.
- §12 still marks `utils.tick` as **MVP** even though `cover_orders` is deferred and v5 excludes `test_tick`.
- §12 and MVP summary still call `VirtualBroker` single-user.
- The Rust test LOC section still uses `229 tests x 20 = 4580` and `~5400`, while v5 uses 226 and 5520.
- §14 says tests are "excluded from the 226 portable ceiling"; if exceptions are excluded from the denominator, the pass gate math changes. If they are included but excused failures, say that.

### P2.5 OHLCVI is still test-driven, not load-bearing

`OHLCVI` is not needed by `VQuote` or `VirtualBroker`; it is only needed for `tests/simulation/test_models.py::test_ohlcvi`. Keeping it in MVP is acceptable if the plan keeps the full simulation-model surface, but the cleaner cut is to exclude that one test and leave `OHLCVI` out.

### P2.6 Cross-machine determinism is underspecified

Several upstream tests use `tz="local"` and compare concrete timestamps. If Rust parity runs across CI machines, the plan needs an explicit local-timezone policy. "Byte-exact over 3 runs" on one machine is weaker than cross-machine determinism.

## Checklist Notes

- A1: **not closed**. Behavioral-not-seed parity is documented, but §14 is not explicit and the expected 3-4 Ticker excused tests is unsupported.
- A2: **partially closed**. Clock use-sites are mostly present, but serde default syntax and broker propagation need correction.
- A3: **closed**. The SQLite schema list matches upstream 37 columns.
- B1: **not closed**. 226 is de-duplicated function names, not pytest-collected portable items; actual portable pytest items are 238.
- B2: **closed at function-name level**. R6=22 and R7=10.
- B3: **mostly closed**. 14.5 clean-path + R0 ~= 15; 30 weeks rationale is plausible if rework averages 1.5 weeks across 10 implementation phases.
- B4: **not fully closed**. R5 two weeks remains optimistic.
- B5: **needs recalculation**. With actual pytest items, 238 x 20 = 4760 before fixtures, not 4520.
- C1: **acknowledged, optional cleanup**. OHLCVI can be cut if that test is excluded.
- C2: **risk remains**. Multi-user R6 is correctly in scope, but source notes still say single-user and 1 week is tight.
- C3: **not closed**. §14 only names `test_ticker_ltp` plus a vague placeholder.
- D1: **conditional**. `Arc<dyn Clock>` with serde skip/default is workable only with an exact free default function; `Clock::system` as written is not enough.
- D2: **needs budgeting**. CompoundOrder/OrderStrategy clock propagation adds R8/R9 work.
- D3: **missing manifest plan**. `rand_distr` and `parking_lot` are mentioned but not versioned.
- D4: **not closed**. Timezone/DST slack has no enumerated §14 failures.
- D5: **not closed**. R3/R8 split is ambiguous and double-counts.
- D6: **source-inaccurate**. `ReplicaBroker.run_fill` has no RNG; seed determinism criterion is misplaced.
- E1: **misleading**. The revised table has 226 function names, but not pytest items.
- E2: **not closed**. §14 is not explicit.
- E3: **yes for OHLC/OHLCV/OHLCVI/Ticker**, but VirtualBroker summary is stale.
- E4: **closed**. `Status` and `OrderType` enum values match upstream.
- F1/F2: **needs hardening**. Exceptions should be exact and cross-machine determinism should be defined.
- G1: **closed**. `OrderFill` does not use Python random.
- G2: **closed**. `Response` uses `pendulum.now(tz="local")` and needs Clock injection.
- G3: **closed**. `VUser` has no time-sensitive field.

## Required Fixes Before ACK

1. Decide whether the denominator is pytest items (**238**) or de-duplicated Python test function names (**226**). Rename and recalculate all gates, phase counts, and test LOC budgets accordingly.
2. Replace §14's Ticker placeholder with exact test ids and exact exception types. Remove the unsupported "~3-4 seed-dependent" claim unless exact tests justify it.
3. Split R3/R8 explicitly: list Order-only tests versus CompoundOrder tests, and give each phase its own gate.
4. Fix Clock serde/default syntax and explicitly document clock propagation through `VirtualBroker.order_place`, `ReplicaBroker.order_place`, response construction, `CompoundOrder.add_order/add`, and `OrderStrategy.add`.
5. Clean `omspy-source-notes.md` stale rows: single-user VirtualBroker, `tick` MVP, 229 test LOC, and body-count tables labelled as tests.
6. Replace the `ReplicaBroker.run_fill` seeded-RNG acceptance criterion with a source-faithful deterministic criterion, or justify the new RNG behavior.

No Rust code should start until these are corrected.
