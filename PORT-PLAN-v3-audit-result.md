# omsrs Port Plan v3 Audit Result

Result: **NACK**.

R1 should not start. v3 is much better scoped than v1/v2, but it still has plan-breaking inventory and dependency errors inside the pure omspy-core port. The biggest failure is not downstream venue integration: the MVP set defers symbols that upstream MVP symbols directly inherit from, store, or return.

## P0 Plan-Breaking

### P0.1 `simulation/models.py` MVP set omits required base types

The decision table marks `VQuote`, response models, `VirtualBroker`, and `ReplicaBroker` as MVP, but defers `OHLC`, `OHLCV`, `OHLCVI`, and `Ticker`.

That does not compile as a faithful port:

- `VQuote` directly inherits `OHLCV` in upstream `simulation/models.py`.
- `OHLCVResponse` stores `Dict[str, OHLCV]`.
- `VirtualBroker.tickers` is `Dict[str, Ticker]`.
- `VirtualBroker._ohlc()` returns `Dict[str, OHLC]`.
- `VirtualBroker._quote()` builds `VQuote(..., **ticker.ohlc().model_dump())`.
- Upstream `tests/simulation/test_virtual.py` fixtures construct `Ticker` for every `VirtualBroker` test.

Fix: either pull `OHLC`, `OHLCV`, `OHLCVI`, and `Ticker` into MVP, or explicitly cut `VirtualBroker` market-data methods and all affected tests from MVP. The current plan does neither.

### P0.2 `OrderType` and `Status` inventory is wrong

`omspy-source-notes.md` says `OrderType` has `MARKET, LIMIT, SL, SL-M`. Upstream has:

- `MARKET`
- `LIMIT`
- `STOP`

No `SL` or `SL-M` exists in `simulation/models.py`.

The notes also summarize `Status` as `PARTIAL`, but upstream uses `PARTIAL_FILL`. This matters because the port's enums and tests must match upstream names and status behavior.

Fix: correct the symbol-level notes and plan before implementation. Do not start R5 from the current enum inventory.

### P0.3 Persistence feature plan is internally inconsistent

The plan says:

- `default = ["persistence"]`
- without `persistence`, `Order.connection` does not exist
- `save_to_db`, `save`, and `create_db` do not compile

But upstream non-save paths still touch `connection`:

- `CompoundOrder.add_order()` assigns `kwargs["connection"] = self.connection`.
- `CompoundOrder.add()` reads `order.connection` and assigns `self.connection`.
- `Order.update()`, `Order.execute()`, and `Order.modify()` check `self.connection` before saving.

If the Rust field is removed with `#[cfg(feature = "persistence")]`, these methods also need cfg-split logic or a persistence-agnostic helper. The plan only mentions gating save methods, not the call sites that read the field.

Also, `omspy-source-notes.md` says `CompoundOrder.__init__` conditionally calls `create_db` when `connection is None`. The upstream source in `order.py` does **not** do this; it only sets `id`, `order_args`, and indexes existing orders. That source-note assertion is false.

Fix: define exact no-default behavior:

- no `connection` parameter accepted, and all connection propagation code compiled out; or
- keep `connection: Option<PersistenceHandle>` as a no-op type without the feature; or
- keep persistence always compiled in.

Then update phase ordering. v3 currently says both "R3 Order except save_to_db" and "R3 persistence.rs: save_to_db + create_db", while R9 also says save-to-db wiring.

## P1 Estimate / Scope

### P1.1 Parity-test target is based on inflated portable-test counts

The plan says ~220 tests pass out of 316, with ~57 exclusions for candle + FakeBroker. The actual portable ceiling is much tighter.

Observed upstream test counts:

| File | Upstream tests | Deferred/dropped within file | Realistic MVP portable |
|---|---:|---:|---:|
| `tests/test_base.py` | 12 | 2 `cover_orders` | 10 |
| `tests/test_models.py` | 11 | 0 | 11 |
| `tests/test_models_tracker.py` | 8 | 8 | 0 |
| `tests/test_models_candles.py` | 19 | 19 | 0 |
| `tests/test_utils.py` | 22 | 1 `tick`, 1 `stop_loss_step_decimal`, 8 `load_broker` | 12 |
| `tests/test_order.py` | 106 | 1 `get_option` | 105 |
| `tests/test_order_strategy.py` | 7 | 0 | 7 |
| `tests/simulation/test_models.py` | 51 | 9 only if `OHLC`/`Ticker` remain deferred | 42-51 |
| `tests/simulation/test_virtual.py` | 80 | 38 `FakeBroker`, 9 `generate_*` | 33 |

So the realistic ceiling is:

- **~220** if `OHLC`/`Ticker` remain deferred, but then `VirtualBroker` itself is not cleanly portable.
- **~229** if `OHLC`/`OHLCV`/`OHLCVI`/`Ticker` are pulled into MVP to make `VQuote`/`VirtualBroker` coherent.

The plan's `target >= 220` leaves almost no failure budget, not ~60 tests of slack. It must be a hard gate or be re-estimated.

### P1.2 R1/R4 test promises include deferred symbols

R1 promises `tests/parity/test_utils.rs` with all 22 upstream `test_utils.py` tests. That is impossible if `tick`, `stop_loss_step_decimal`, and `load_broker` are deferred/dropped. Only 12 of 22 are portable under the current MVP.

R4 promises all 12 `test_base.py` tests. Two are `cover_orders`, which the plan defers. Only 10 of 12 are portable unless `cover_orders` and `tick` return to MVP.

Fix: every phase gate must name the exact tests included and excused.

### P1.3 LOC estimate has a safety margin on prod but not tests

The plan says Python MVP LOC is 2760 and Rust multiplier is 1.4-1.6x, which implies 3900-4400 prod LOC. The phase table budgets 4900 prod LOC, or 1.78x. That is not an underestimate; it is already a production-code safety margin.

The test budget is the weak part:

- 2050 test LOC / 220 tests = ~9.3 LOC/test.
- Rust parity tests with fixtures are more commonly 15-25 LOC/test.
- At 20 LOC/test, 220 tests are ~4400 test LOC before helpers.

Fix: re-baseline tests to roughly 3500-4500 LOC, or reduce the parity count and state exactly what is excluded.

### P1.4 R10 is too short

R10 budgets 1.5 weeks and no new LOC for parity sweep + stabilization. For ~220 tests across order lifecycle, persistence, broker behavior, simulation fills, decimals, and timezone semantics, that is not credible.

Expected parity-pass/stabilization is closer to 3-4 weeks, especially because R3/R8/R9 touch high-state objects and because decimals/timezones will need tolerance and clock fixes.

### P1.5 Audit rework is acknowledged but not budgeted

§8.R.6 says codex audit rework adds 1-2 weeks and is not in the 12-week estimate. The phase protocol says every phase requires an audit ACK before the next phase.

With 10 phases, the 12-week MVP assumes near-perfect ACKs. Historical 1-2 NACKs per phase would add 10-20 weeks. The plan should present:

- clean-path estimate: ~12 weeks plus corrected R10/test budget
- expected estimate with audit rework: materially longer

## P2 Hardening

### P2.1 Acceptance criterion 3 is ambiguous

Criterion 3 says "`cargo test` -- all parity tests pass (target >= 220)". "Target" reads soft.

Fix: make it binary:

- ACCEPT if all included parity tests pass and pass count is >= the declared floor.
- NACK if pass count is below the floor or if any included parity test is excused without audit approval.

### P2.2 Acceptance criterion 9 is vague

"CompoundOrder + OrderStrategy can aggregate positions + MTM like upstream" is not a testable acceptance criterion.

Fix: cite the exact upstream tests that define parity, including:

- `test_compound_order_positions`
- `test_compound_order_average_buy_price`
- `test_compound_order_average_sell_price`
- `test_compound_order_net_value`
- `test_compound_order_mtm`
- `test_compound_order_total_mtm`
- `test_order_strategy_positions`
- `test_order_strategy_mtm`

### P2.3 `OrderLifecycle` is a dual-source-of-truth risk

Keeping `status: Option<String>` for parity and adding `OrderLifecycle` is defensible, but the plan only says both stay in sync through `Order.update()`.

That is not enough. `status` can also be set during construction/deserialization or by direct field mutation depending on API design.

Fix: define an invariant and tests:

- lifecycle is derived from current order fields/status, not independently stored; or
- all status mutation flows are private and go through one setter.

Add parity/property tests for `COMPLETE`, `CANCELED`, `CANCELLED`, `REJECTED`, partial fill, filled+cancelled equals quantity, and unknown status strings.

### P2.4 Decimal parity needs explicit comparison rules

The plan chooses `rust_decimal` while upstream uses Python floats. That is fine for production quality, but parity tests need a rule.

Fix: specify comparison policy per value type:

- exact integer equality for quantities
- exact string/enum equality for statuses
- Decimal-to-f64 with epsilon for Python float-derived expected values, or decimal literals copied from upstream when exact
- explicit epsilon values for MTM, average price, spread, and tick-derived values

### P2.5 Clock and timezone production path is underspecified

The plan names chrono/chrono-tz and MockClock, but it does not specify how production handles upstream's `timezone="local"` behavior. Upstream calls `pendulum.now(tz=self.timezone)` and `pendulum.today(tz=tz).end_of("day")`; `OrderLock` also compares lock-till values against `pendulum.now(tz=timezone)`.

Fix: define:

- how `"local"` maps in Rust
- how invalid timezone strings fail
- whether timestamps are stored as UTC with display timezone or as timezone-aware chrono values
- a Clock trait/MockClock as an actual MVP support type if tests depend on it

### P2.6 SQLite schema needs a freeze policy

The plan acknowledges the 36-column upstream schema and says `to_sql_row()` should mirror upstream names. Good. It should also state that the MVP schema is frozen for compatibility and that future column additions require migration/version handling.

### P2.7 YAML override loading defer is fine but should name extension point

Skipping YAML is fine for MVP. Add one sentence that downstream adapters may load YAML or other mapping config in their own crates and feed `override_keys` directly.

### P2.8 `proptest` needs concrete scope

R10 mentions property-based tests but no count or invariants. Ad hoc proptest is acceptable as supplemental coverage, but the plan should state a minimum set:

- `update_quantity`: conservation and bounds
- `Order` lifecycle: done/pending/complete invariants
- `BasicPosition`: buy/sell/net arithmetic

## Checklist Results

### A. Source-notes accuracy

- A1: MVP symbols exist at cited paths by AST/top-level inspection. Spot-checked: `Broker`, `pre`, `post`, `QuantityMatch`, `BasicPosition`, `OrderLock`, `UQty`, `update_quantity`, `Order`, `CompoundOrder`, `OrderStrategy`, `VOrder`, `OrderFill`, `VirtualBroker`, `ReplicaBroker`, `Paper`.
- A1 caveat: existence is not enough. The `simulation/models.py` enum values and inheritance/dependencies are wrong in notes.
- A2 `get_option`: confirmed not used by `Order`; in the upstream tree it is only referenced by `tests/test_order.py`.
- A2 `load_broker`: confirmed imports `Zerodha`, `Finvasia`, `Icici`, `Neo`, `Noren`.
- A2 `stop_loss_step_decimal`: confirmed not referenced by in-scope modules; only `utils.py`, `tests/test_utils.py`, and changelog.
- A2 `Tracker` / `Timer` / `TimeTracker` / `Candle` / `CandleStick`: confirmed not dependencies of `Order`, `CompoundOrder`, or `OrderStrategy`.
- A3 matching book: confirmed no book-level matching abstraction in `VirtualBroker` or `ReplicaBroker`. `VirtualBroker` stores orders and updates ticker state; `ReplicaBroker.run_fill()` iterates `OrderFill`.
- A4 `brokers/paper.py`: confirmed only imports `Broker`, `pre`, and `post` from `omspy.base`; no `simulation/` dependency.

### B. Missing MVP symbols / hidden deps

- B1: `Order.__init__` depends on `uuid`, `pendulum`, and `OrderLock`; no other init-time omspy symbol found.
- B2: source notes are wrong: `CompoundOrder.__init__` does not call `create_db`. However, `connection` is used outside save-only methods, so feature-gating still needs a more precise design.
- B3: `Broker.get_positions_from_orders` -> `dict_filter` + `create_basic_positions_from_orders_dict` + `BasicPosition`: OK.
- B4: `VOrder` -> `utils.update_quantity`: OK.
- B5: `OrderFill` references `VOrder`, `Side`, and `OrderType`; no deferred type, but `OrderType` inventory must be corrected to `STOP`.
- B6: deferring `cover_orders` removes the only `base.py` dependency on `utils.tick`. `tick` is otherwise used by out-of-scope modules/tests and as a local parameter name in random orderbook generation.
- B7: `base.py` star import effectively uses `dict_filter`, `create_basic_positions_from_orders_dict`, and `tick`. No silent use of `UQty`, `stop_loss_step_decimal`, `update_quantity`, or `load_broker`.

### C. Parity test coverage

- C1: `tests/test_order.py` has 106 tests. Only one directly tests dropped `get_option`. Many persistence tests remain if `persistence` is default. No `Candle` or `tick` references found in `test_order.py`.
- C2: `tests/simulation/test_virtual.py` breakdown: 23 `VirtualBroker`, 10 `ReplicaBroker`, 38 `FakeBroker`, 9 random `generate_*`.
- C2: plan's 220 target is not a loose target; it is near the realistic ceiling after proper exclusions.
- C3: property tests are fine as extra coverage, but the plan needs concrete invariant names/counts.

### D. LOC realism

- D1: 4900 prod LOC is above the stated 1.4-1.6x multiplier; it includes a safety margin.
- D2: 2050 test LOC is likely low by ~2x.
- D3: R3 at 1600 LOC over 2 weeks is feasible only with no slack.
- D4: R10 at 1.5 weeks is too optimistic; use 3-4 weeks.
- D5: audit rework is not in the 12-week estimate.

### E. Design decisions

- E1 sync `Broker` trait: acceptable and clearly says downstream async users wrap separately.
- E2 status string + lifecycle enum: needs stronger invariant/tests.
- E3 Decimal everywhere: acceptable, but parity tolerances unspecified.
- E4 chrono vs pendulum: drift risk around `"local"` and lock-till comparisons remains.
- E5 override map/no YAML: acceptable; add downstream extension-point note.
- E6 SQLite feature: schema drift acknowledged; add freeze/migration policy.

### F. Phase ordering

- F1 R1 utils/basic models before R4 broker: OK.
- F2 R4 `Paper` has no simulation hidden dep: OK.
- F3 R6 `VirtualBroker` after R5 simulation models is only OK if R5 includes `Ticker`/`OHLC` dependencies or R6 cuts market-data methods/tests.
- F4 R3/R9 persistence ordering is inconsistent in the plan text.
- F5 R9 bundles `OrderStrategy` and persistence wiring; they are not intrinsically tied. This is load-balancing, not a dependency.

### G. Hidden reinvention

- G1 I found no existing Rust crate exposing omspy's `create_basic_positions_from_orders_dict` equivalent by exact name/search. Reimplementing this small helper is fine.
- G2 `OrderLifecycle` does not require a state-machine crate. A full `rust-fsm` dependency would be overkill unless lifecycle transitions become independently stored and complex.
- G3 `barter-execution::client::ExecutionClient` overlaps `open_order`, `cancel_order`, fetch balances/open orders/trades, but it is async, has no `modify_order`, and does not model omspy's sync `orders`/`trades`/`positions` properties, override mapping, or `get_positions_from_orders`. Divergence is genuine; depending on `ExecutionClient` is not required for this pure port.

External check:

- `barter-execution` 0.7.0 exposes async `ExecutionClient` with `open_order`, `cancel_order`, `fetch_open_orders`, `fetch_balances`, and `fetch_trades`.
- `rust-fsm` and similar crates exist, but the plan's lifecycle enum does not justify pulling one in by default.

### H. Acceptance criteria

- H1 criterion 9 is vague; replace with exact parity tests and numeric tolerances.
- H2 criterion 3 must say whether `>= 220` is a hard gate. It should be a hard gate after the corrected portable ceiling is declared.

### I. Residual hidden scope

- I1 yes: current phases require `OHLC`, `OHLCV`, `OHLCVI`, `Ticker`, and corrected enum variants not listed correctly in §1 MVP.
- I2 R10 nominally covers all ported tests, but "ported" is underdefined. It needs an explicit manifest of included/excused upstream tests.
- I3 `MockClock` is mentioned as a risk mitigation but not listed as MVP or in the test plan. It should be a named support type if clock-dependent parity tests are expected.

## Required Changes Before ACK

1. Correct `omspy-source-notes.md` for `simulation/models.py`: enum values, `VQuote` inheritance, response-model dependencies, and `VirtualBroker` dependency on `Ticker`/`OHLC`.
2. Decide whether `OHLC`/`OHLCV`/`OHLCVI`/`Ticker` are MVP or whether `VirtualBroker` market-data methods/tests are out of MVP.
3. Rewrite the parity-test manifest with included/excused tests per upstream file and recalculate the hard pass floor.
4. Fix the persistence feature design for no-default builds, including non-save methods that read/write `connection`.
5. Re-baseline test LOC and R10 duration.
6. Turn vague acceptance criteria into hard gates with exact tests, tolerances, and excused-test policy.

## External Sources

- `barter-execution` crate metadata: https://crates.io/crates/barter-execution/0.7.0
- `barter-execution::client::ExecutionClient` docs: https://docs.rs/barter-execution/0.7.0/barter_execution/client/trait.ExecutionClient.html
- `rust-fsm` crate docs: https://docs.rs/rust-fsm/
