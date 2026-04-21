# omsrs Port Plan v4 Audit Result

Result: **NACK**.

v4 closes the headline v3 P0 inventory mistakes: `OHLC/OHLCV/OHLCVI/Ticker` are now in MVP, enum values match upstream, and the persistence field is no longer cfg-removed from methods that read it. But v4 is still not sound enough to start R1. The remaining problems are not Polymarket/barter scope issues; they are inside the pure omspy-core Rust port.

## P0 Plan-Breaking

### P0.1 `Ticker` adds deterministic Python RNG parity that v4 does not budget or design

Pulling `Ticker` into MVP is necessary for `VirtualBroker.tickers` and `VirtualBroker._ohlc/_quote`. It does **not** pull in `FakeBroker` or the `generate_*` helpers, but it does pull in `random.gauss` semantics:

- Upstream `Ticker.ltp` uses `random.gauss(0, 1)`, rounds to 0.05, and mutates `_ltp/_high/_low` (`omspy/simulation/models.py:112-117`).
- Upstream `tests/simulation/test_models.py::test_ticker_ltp` seeds Python RNG with `random.seed(1000)` and expects exact values (`_ltp == 120.5`, `_high == 125.3`, `_low == 120.5`).
- v4 R5 promises **all 51** `test_simulation_models.rs` tests, but the plan only discusses seeded `SmallRng` for `ReplicaBroker.run_fill`, not Python-compatible RNG for `Ticker`.

Fix: either implement a Python-compatible RNG/gauss path for `Ticker` tests, make `Ticker` random mode a documented parity exception, or change the R5 test promise. As written, R5 cannot honestly promise all simulation model tests.

### P0.2 `Clock` is declared, but not threaded through the MVP time-sensitive paths

v4 D4 declares `Clock`, `SystemClock`, and `MockClock`, but the plan does not say where the clock lives or how methods receive it. That is not enough for the promised parity tests.

MVP upstream locations that read wall-clock time:

- `Order.__init__`: timestamp default and end-of-day `expires_in` calculation (`order.py:221-228`).
- `Order.time_to_expiry`, `time_after_expiry`, `update.last_updated_at` (`order.py:330-347`, `order.py:451`).
- `OrderLock.__init__`, `create/modify/cancel`, `can_*` (`models.py:192-268`).
- `VOrder.__init__`, `is_past_delay`, `set_exchange_timestamp` (`simulation/models.py:226-271`, `simulation/models.py:361-363`).
- `Response.__init__` (`simulation/models.py:441-444`).
- `VirtualBroker.get` and `ReplicaBroker` tests depend on the `VOrder` clock behavior through `pendulum.travel_to`.

`Timer.has_started` is deferred with `Timer`, so it does not need MVP wiring. The paths above do.

Fix: define the clock injection strategy before R2/R3/R5. For example, add an explicit `Clock` handle to `Order`, `OrderLock`, `VOrder`, and response constructors with serde skips as needed, or make methods accept a clock parameter. Without this, `MockClock` exists as an unused support type and the time-based parity tests are not deterministic.

### P0.3 SQLite schema freeze is factually wrong and not actually frozen

v4 says `create_db` emits the “36-column upstream schema verbatim” (`PORT-PLAN.md` §6 D7), but upstream has **37** columns:

`symbol, side, quantity, id, parent_id, timestamp, order_type, broker_timestamp, exchange_timestamp, order_id, exchange_order_id, price, trigger_price, average_price, pending_quantity, filled_quantity, cancelled_quantity, disclosed_quantity, validity, status, expires_in, timezone, client_id, convert_to_market_after_expiry, cancel_after_expiry, retries, max_modifications, exchange, tag, can_peg, pseudo_id, strategy_id, portfolio_id, JSON, error, is_multi, last_updated_at`.

The plan also does not name the columns in §6 D7, so the “freeze” is only a promise. This matters because the default feature includes persistence and upstream `test_new_db_all_values` round-trips all model fields.

Fix: enumerate the exact 37-column schema in the plan/source notes and align the persistence tests with that list.

## P1 Estimate / Scope

### P1.1 The 229 portable-test ceiling is not the pytest-collected upstream ceiling

v4 still counts Python function bodies, not tests that pytest would collect. Upstream has duplicate test function names, so earlier definitions are overwritten:

- `tests/test_order.py`: two `test_compound_order_update_orders`; pytest sees 105 tests, not 106. Excluding `test_get_option` leaves 104, not 105.
- `tests/simulation/test_models.py`: two `test_vorder_modify_by_status_partial_fill`; pytest sees 50 tests, not 51.
- `tests/simulation/test_virtual.py`: two `test_virtual_broker_ltp`; pytest sees 79 tests, not 80. After excluding FakeBroker and `generate_*`, portable count is 32, not 33.

Portable pytest-collected ceiling is therefore **226**, not 229:

`10 + 11 + 12 + 104 + 7 + 50 + 32 = 226`.

If v4 intentionally wants to port overwritten test bodies too, it must say “229 upstream test bodies, including renamed duplicate definitions,” not “229 upstream tests.” Otherwise the acceptance gate and phase counts are wrong.

### P1.2 `test_simulation_virtual.py` R6/R7 split is wrong

AST/body breakdown is:

- 9 `generate_*` tests excluded.
- 38 `FakeBroker` tests excluded.
- 23 `VirtualBroker` bodies included.
- 10 `ReplicaBroker` bodies included.

Pytest-collected included count is 22 `VirtualBroker` + 10 `ReplicaBroker` because one `test_virtual_broker_ltp` is overwritten.

v4 says R6 has `~18` VirtualBroker tests and R7 has `~15` ReplicaBroker tests. That split is not supported by upstream.

There is also a scope contradiction: v4 says `VirtualBroker (single-user MVP)`, but the included portable set contains multi-user behavior (`test_virtual_broker_add_user`, `test_virtual_broker_order_place_users`, `test_virtual_broker_order_place_same_memory`, and `test_replica_broker_order_place_multiple_users`). Either include user support and stop calling it single-user, or exclude those tests and lower the portable ceiling.

### P1.3 Phase total arithmetic is wrong

The clean-path phase estimates sum to **15 weeks**, not 14:

`0.75 + 1 + 3 + 1 + 1.5 + 1 + 1.25 + 1.5 + 1 + 3 = 15`.

The rework column is also still underexplained. With 10 implementation phases and an average 1.5 weeks of audit rework per phase, expected duration is about **30 weeks total**: 15 clean-path weeks plus 15 rework weeks. v4’s **22-28 weeks** may be possible only if rework averages materially less than one week per phase, but the table does not state that assumption.

### P1.4 R5 is tighter than v4 admits

R5 is 1.5 weeks for `simulation/models.py` data types plus the full simulation-model parity surface. That phase includes enum parsing, pydantic-style validation, `Ticker` private mutable state, response timestamps, `OrderFill` behavior, and seeded/random test concerns. With the RNG/time gaps above, 1.5 weeks is optimistic and should not be treated as clean-path reliable.

### P1.5 Test LOC budget is plausible, but only after fixing the test definition

The 5400 test LOC budget is much more realistic than v3. Selected upstream test bodies average about 11.6 Python LOC; 20 Rust LOC/test plus fixtures is plausible for parity tests.

But the denominator must be fixed first: it is either 226 pytest-collected tests or 229 deliberately renamed upstream test bodies.

## P2 Hardening / Consistency

### P2.1 v3 P0 closures: mostly closed, with minor consistency gaps

- `OHLC/OHLCV/OHLCVI/Ticker`: now MVP in both `PORT-PLAN.md` §2 and `omspy-source-notes.md` §12. `OHLCVI` is not transitively required by `VQuote`/`VirtualBroker`, but it is required if R5 keeps all simulation-model parity bodies.
- Enums: `Status`, `ResponseStatus`, `Side`, `TickerMode`, and `OrderType` match upstream values.
- Persistence call-site shape: keeping `connection: Option<Box<dyn PersistenceHandle>>` unconditionally removes the v3 cfg-split bug in `Order.update/execute/modify/save_to_db` and `CompoundOrder.add_order/add/save`.
- The false `CompoundOrder.__init__` calls `create_db` claim has been corrected.

Remaining consistency gaps: §1 says `fn save(&self, order: &Order)`, §6 says `fn save_order(&self, order: &Order)`, source notes say `save`, and R3 places the trait in `persistence.rs` while §6 says `order.rs`.

### P2.2 `OrderLifecycle` fix is directionally correct but under-tested

Making `OrderLifecycle::from_order(&Order)` derived and unstored eliminates the dual-source-of-truth problem. The proposed property test is too weak by itself. Add cases for `COMPLETE`, `CANCELED`, `CANCELLED`, `REJECTED`, partial fill, filled+cancelled equals quantity, and unknown status strings.

### P2.3 Decimal rules are acceptable, but need per-test rounding notes

The epsilons for MTM (`0.01`), average price (`0.0001`), and spread (`0.0001`) are acceptable for parity. Add explicit notes for tests that round expected values before assertion, such as `test_compound_order_average_sell_price`.

### P2.4 Base parity tests rely on YAML-loaded overrides

The 10 portable `test_base.py` tests all use the `broker` fixture, which passes `override_file=ROOT / "zerodha.yaml"`. Since YAML loading is dropped, the Rust parity fixture must manually call `set_override` with the same mappings:

- `orders.tradingsymbol -> symbol`
- `positions.tradingsymbol -> symbol`
- `trades.tradingsymbol -> symbol`
- `order_place.symbol -> tradingsymbol`
- `order_place.side -> transaction_type`

This is compatible with v4 D5, but the phase test plan should say it explicitly.

### P2.5 Section 14 excuse log does not exist

Acceptance criterion 3 says failing tests require entries in `omspy-source-notes.md` section 14. The file currently ends at §13. Add the section before relying on the criterion.

### P2.6 Source-notes `tick` classification is stale

`omspy-source-notes.md` §12 marks `utils.tick` as **MVP** while the reason says it can defer if `cover_orders` defers, and the plan/source summary exclude `tick`. The intended classification is defer; fix the decision table.

### P2.7 `Broker` trait object plus serde needs an explicit representation

`Order.execute` receives a broker parameter, but `CompoundOrder` stores `broker`. If Rust uses `Arc<dyn Broker>`, serde derives cannot cover that field without `skip`/custom handling. v4 acceptance says `Arc<dyn Broker>` must compile, but the plan does not state how `CompoundOrder` serialization/debug/clone handle the trait object.

### P2.8 Property tests are meaningful but thin

The proposed property modules are useful:

- `quantity.rs`: conservation/bounds for `update_quantity`.
- `position.rs`: `net_quantity == buy - sell`.
- `lifecycle.rs`: derived lifecycle consistency.

But lifecycle needs the expanded status cases noted above, and `position.rs` should also cover average-price zero-division behavior.

## Checklist Notes

- A1: closed for inheritance. All four types are included. `OHLCVI` is test-driven, not transitively required by `VQuote` or `VirtualBroker`.
- A2: closed. Enum values match `simulation/models.py:16-43`.
- A3: mostly closed. The unconditional persistence trait/field design removes the no-feature cfg-split across the listed call sites, but naming/location inconsistencies remain.
- A4: closed. The false `CompoundOrder.__init__` persistence claim is removed and reversed in §13.
- B1/B2: not closed. Counts and R6/R7 split are wrong once duplicate names and actual categories are considered.
- B3: improved. 5400 test LOC is plausible after count correction.
- B4: improved but still tight. R10 at 3 weeks is plausible only if earlier phases do not defer parity debt.
- B5: not closed. Clean-path and rework arithmetic are still wrong.
- C1: criterion is binary for `< 220`, but the denominator/definition must be corrected.
- C2: criterion cites real CompoundOrder/OrderStrategy tests, but R8 uses “etc.” and should enumerate the exact `check_flags` tests too.
- C3: closed by deriving lifecycle.
- C4: mostly closed.
- C5: not closed; clock is declared but not wired.
- C6: not closed; schema count is wrong and columns are not enumerated.
- C7: OK, with explicit base-test fixture note needed.
- C8: partially closed; invariants are useful but too shallow.

## Required Fixes Before ACK

1. Decide whether the parity target is 226 pytest-collected tests or 229 renamed test bodies, then update all phase counts and gates.
2. Resolve `Ticker` RNG parity: Python-compatible RNG, explicit test exception, or revised R5 promise.
3. Specify clock injection/storage for every MVP time-sensitive type and method.
4. Replace the “36-column” persistence claim with an enumerated 37-column schema.
5. Fix phase arithmetic and R6/R7 breakdown.
6. Clean source-note inconsistencies: `tick`, `PersistenceHandle` method name/location, and missing §14.

No Rust code should start until those are corrected.
