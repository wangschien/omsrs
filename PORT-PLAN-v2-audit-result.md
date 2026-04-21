# PORT-PLAN v2 Audit Result

Verdict: NACK.

Scope of this audit: pure Rust port of omspy core only. I did not evaluate downstream consumers, venue integration, or any non-omspy migration.

## P0 Findings

### P0.1 The in-scope inventory is not symbol-complete

The plan lists whole files as in scope, but the Rust layout and phase plan omit material types and functions inside those files.

Evidence:

- `omspy/order.py` is not just `Order` + `CompoundOrder` + TIF. It also defines `get_option`, `create_db`, and `OrderStrategy` (`/home/ubuntu/refs/omspy/omspy/order.py:28`, `:51`, `:1280`).
- `OrderStrategy` has first-class tests in `tests/test_order_strategy.py` and is imported as `from omspy.order import Order, CompoundOrder, OrderStrategy, create_db`.
- `omspy/models.py` does not match the plan's description of "Position, Trade, OrderBook, Quote, OrderRequest". It defines `QuantityMatch`, `BasicPosition`, `Quote`, `OrderBook`, `Tracker`, `Timer`, `TimeTracker`, `OrderLock`, `Candle`, and `CandleStick` (`/home/ubuntu/refs/omspy/omspy/models.py:12`, `:25`, `:49`, `:59`, `:121`, `:272`). `OrderLock` is a hard dependency of `Order` (`/home/ubuntu/refs/omspy/omspy/order.py:25`).
- There is no `OrderRequest` in `omspy/models.py`. `Trade`-like types are under `simulation/models.py` as `VTrade`, not the top-level models file.

Impact: R1/R2 cannot be ACK'd as written because the plan does not define whether these in-file core symbols are ported, postponed, or intentionally excluded. File-level scope and symbol-level implementation disagree.

Required fix: replace the file-level in-scope table with a symbol-level inventory. For each class/function in `base.py`, `order.py`, `models.py`, `simulation/models.py`, `simulation/virtual.py`, `brokers/paper.py`, and referenced `utils.py`, mark `MVP`, `defer`, or `drop`, with a test decision.

### P0.2 The simulation/paper architecture is mis-mapped

The target layout says:

- `simulation/virtual.py` -> `paper/engine.rs + paper/book.rs + paper/fills.rs`
- `brokers/paper.py` -> `paper/broker.rs` as `impl Broker for PaperBroker`

That is not a 1:1 port of upstream.

Evidence:

- `brokers/paper.py` is a 52-line dummy broker that returns configured lists and echoes order args. It does not bind to `simulation/` at all (`/home/ubuntu/refs/omspy/omspy/brokers/paper.py:4`, `:27`, `:42`).
- `simulation/virtual.py` contains `FakeBroker`, `VirtualBroker`, `ReplicaBroker`, random market-data helpers, response override behavior, user handling, and order placement/cancel/modify behavior (`/home/ubuntu/refs/omspy/omspy/simulation/virtual.py:33`, `:69`, `:88`, `:172`, `:515`, `:737`).
- The fill logic is not cleanly isolated in `virtual.py`; the concrete `OrderFill` model is in `simulation/models.py` (`/home/ubuntu/refs/omspy/omspy/simulation/models.py:505`), and `ReplicaBroker.run_fill` drives it (`/home/ubuntu/refs/omspy/omspy/simulation/virtual.py:813`).
- There is no explicit upstream "local order book" engine abstraction. `OrderBook`/`Quote` are data models and generated fake data, not a central matching-book implementation.

Impact: the proposed `paper/book.rs`, `paper/engine.rs`, and `paper/fills.rs` may be a valid Rust design, but it is not yet demonstrated as a faithful omspy port. The plan currently risks inventing a new paper engine while claiming a direct port.

Required fix: describe which upstream simulator is being ported:

- dummy `Paper`;
- `FakeBroker`;
- `VirtualBroker`;
- `ReplicaBroker` + `OrderFill`;
- or a deliberately consolidated Rust `PaperBroker`.

Then map each upstream method and response type to the Rust module that owns it.

### P0.3 Acceptance is too vague for a parity port

The plan's parity gate is "pick 5 scenarios from `omspy/tests/` (or hand-craft from omspy docs)" (`/home/ubuntu/omsrs/PORT-PLAN.md:93`). That is not enough for a pure port claim.

Counts:

- upstream has 23 test files and 538 test functions total;
- the in-scope/core test subset has 10 files, 326 test functions, and 4,812 LOC;
- `tests/test_order.py` alone has 106 test functions and 1,552 LOC;
- `tests/simulation/test_virtual.py` has 80 test functions and 1,097 LOC;
- `tests/simulation/test_models.py` has 51 test functions and 736 LOC.

Impact: "5 scenarios" leaves parity open-ended and unauditable. It also allows critical behavior to be missed: lock timing, direct status/quantity mutation, DB save/update paths, server transport behavior, generated quote/orderbook behavior, partial-fill quantity invariants, and response/error shapes.

Required fix: define a named parity matrix before R1. Minimum MVP parity set should include top-level model tests, order lifecycle/update/execute/modify/cancel tests, compound tests, simulator model tests, virtual broker tests, and a decision on server and persistence tests.

## P1 Findings

### P1.1 LOC estimate is under-specified and likely low

The plan says `~3950 LOC Python -> target ~5000-6000 LOC Rust` and totals `~5800 LOC` (`/home/ubuntu/omsrs/PORT-PLAN.md:9`, `:85`, `:108`).

Measured source:

- listed prod files including dropped server: 4,189 Python LOC;
- listed prod files excluding server: 4,015 Python LOC;
- core/in-scope tests: 4,812 Python LOC.

The 5,800 LOC number appears to be production-only, but §2 includes tests and §3 phase lines include "transition tests", "full integration tests", and "compound tests" without a separate test LOC budget. A realistic total including parity tests is not 5,800. Expect roughly 6,500-8,000 prod LOC after the missing symbols are accounted for, plus 3,000-5,000 focused test LOC for a real MVP parity suite.

Calibration:

- local `barter-execution` currently has 2,815 Rust source LOC, with history across `barter-execution/src` from 2024-07-08 to 2025-12-21;
- local `rs-clob-client` has 17,246 Rust source LOC and 12,933 Rust test LOC, with main source work visible from 2025-12-08 to 2026-03-23 before mostly dependency maintenance;
- current NautilusTrader `develop` snapshot has about 1,042,432 Rust LOC under `crates/` and 240,344 Python/Cython LOC under `nautilus_trader/`. This is not an omspy-sized port, but it is a warning that real Rust trading infrastructure can exceed simple 1.5x ratios.

Conclusion: keep the 1.3-1.7x rule only for prod code that is a narrow data/model port. For this OMS plus simulator plus tests, the plan should budget total code separately and likely raise MVP total to 9,500-13,000 LOC including tests.

### P1.2 Seven weeks assumes clean ACKs and leaves no audit rework budget

The plan says each phase is one commit plus one audit ACK (`/home/ubuntu/omsrs/PORT-PLAN.md:73`) and totals seven full-time weeks (`:85`). Prior audited phases reportedly averaged 1-2 NACKs requiring rework. The plan does not say whether seven weeks includes rework.

Required fix: split estimates into implementation time and audit/rework time. I would budget 9-12 full-time weeks for MVP if parity tests are real and each phase requires audit closure.

### P1.3 R2 and R7 are under-scoped relative to `order.py`

`order.py` has:

- `Order`, including update/execute/modify/cancel/save/clone/locking;
- `CompoundOrder`, including indexing, positions, averaging, MTM, execute/update/save;
- `OrderStrategy`;
- `create_db`;
- `get_option`;
- direct SQLite persistence support.

The plan budgets R2 as 1,200 LOC in 1.5 weeks for `order.rs + OrderLifecycle + transition tests` and R7 as 800 LOC in 1 week for compound. That leaves no explicit phase for `OrderStrategy`, `create_db`, persistence decision tests, `get_option`, or compatibility behavior around direct `status`/quantity mutation.

Required fix: either add `OrderStrategy` and persistence-related symbols to phases, or explicitly exclude them and remove their upstream tests from MVP parity.

### P1.4 SQLite is not merely external optional behavior in upstream

The plan says "No SQLite persistence in v1" (`/home/ubuntu/omsrs/PORT-PLAN.md:37`, `:101`). Upstream `order.py` imports `sqlite3` and `sqlite_utils.Database` at module load (`/home/ubuntu/refs/omspy/omspy/order.py:18`, `:24`), defines `create_db` (`:51`), carries `connection` on `Order` and `CompoundOrder` (`:138`, `:731`), and has tests that exercise DB save/update and integrity errors.

Dropping SQLite can be acceptable for v1, but it is not just an implementation detail. It is a feature cut from an in-scope file.

Required fix: state "persistence APIs and DB parity tests are excluded from MVP" or include a minimal in-memory/trait-backed persistence abstraction. Do not call the port behavior-complete for `order.py` while those tests are excluded.

### P1.5 `async-first Broker` needs a semantic compatibility design

The upstream `Broker` is synchronous, and `Order.execute` mutates `self.order_id` immediately after `broker.order_place` returns (`/home/ubuntu/refs/omspy/omspy/order.py:460`). `Order.modify` and `Order.cancel` also mutate or call synchronously and rely on lock checks.

Async can be the right Rust design, but the plan has not resolved:

- whether `Order::execute` owns `&mut self` across `.await`;
- whether broker methods return an ID, a response object, or a normalized order snapshot;
- how immediate post-call mutation semantics are represented;
- whether helper methods such as `close_all_positions`, `cancel_all_orders`, `get_positions_from_orders`, and `cover_orders` become async extension methods (`/home/ubuntu/refs/omspy/omspy/base.py:253`, `:264`, `:283`, `:290`).

Required fix: add an async semantic contract section with method signatures and one example port of `Order.execute`.

### P1.6 Error mapping has not been enumerated

The plan chooses `thiserror` but does not enumerate upstream error cases. In-scope sources raise or rely on:

- `NotImplementedError` in `Broker`;
- `ValueError` from validators and utility loading;
- pydantic `ValidationError` behavior in simulator paths;
- `IndexError`, `KeyError`, and `TypeError` in `CompoundOrder`/`OrderStrategy`;
- `sqlite3.IntegrityError` in persistence tests;
- broker response failure objects instead of exceptions in simulator code.

Required fix: add an `OmsError` inventory before R1. Otherwise each phase will invent incompatible errors locally.

### P1.7 The Decimal rationale is factually off

The plan says omspy uses Python `Decimal` (`/home/ubuntu/omsrs/PORT-PLAN.md:114`), but the inspected upstream core uses `float` for prices and values throughout `models.py`, `order.py`, and `simulation/models.py`. `rust_decimal` may still be a good Rust choice, but it is a deliberate semantic improvement, not Decimal parity.

Required fix: add golden tests for observed upstream numeric behavior, especially:

- `tick`;
- `get_option`;
- `OrderBook.spread`;
- average buy/sell prices;
- MTM/net value;
- simulator fill price and quantity normalization.

These should check values, rounding, and accepted input conversions.

## P2 Findings

### P2.1 `base.py` is more than a trait

`base.py` includes:

- `pre` and `post` decorators;
- override YAML loading;
- key rename behavior;
- base helper methods for closing positions, canceling orders, deriving positions from orders, and placing cover orders.

If `broker.rs` is only an async trait, parity with `test_base.py` will be lost. Use either a trait plus extension functions, or a `BrokerSupport` helper type for rename/override/position helpers.

### P2.2 Referenced `utils.py` functions are known

For the in-scope modules, the referenced utilities are:

- `create_basic_positions_from_orders_dict`: used by `Broker.get_positions_from_orders`;
- `dict_filter`: used by `Broker.get_positions_from_orders`;
- `tick`: used by `Broker.cover_orders`;
- `update_quantity`: used by `simulation.models.VOrder._make_right_quantity`;
- `UQty`: needed as the return type of `update_quantity`.

Not referenced by in-scope modules:

- `stop_loss_step_decimal`;
- `load_broker`.

`load_broker` imports venue-specific brokers and should be excluded or feature-gated with the skipped broker adapters.

### P2.3 Out-of-scope broker list is basically correct but incomplete

Zerodha, ICICI, Finvasia/Shoonya, Kotak Neo, and Noren are Indian-market broker adapters, and excluding them is consistent with the stated user scope. However, the plan should explicitly list `brokers/api_helper.py`, `brokers/__init__.py`, and broker YAML files as out of scope. `utils.load_broker` should also be excluded because it imports those adapters.

### P2.4 `orders/`, `algos/`, and `multi.py` are not hard dependencies of core

Import graph check:

- `base.py` does not import `orders/`, `algos/`, or `multi.py`;
- `order.py` does not import `orders/`, `algos/`, or `multi.py`;
- `multi.py` imports `Broker` and `Order`, not the reverse;
- `orders/` and `algos/` import core types, not the reverse.

So excluding those modules is acceptable for MVP as long as `OrderStrategy` is handled separately because it lives in `order.py`, not `algos/`.

### P2.5 Dropping `simulation/server.py` requires a test shim decision

`tests/simulation/test_server.py` has 15 HTTP transport tests and imports `omspy.simulation.server.app`. If the server is dropped, the plan needs to say whether those behaviors are:

- covered by direct in-process broker calls;
- covered by a channel/request enum shim;
- or explicitly excluded from MVP parity.

The current phrase "Flask HTTP server replaced by in-process channels" is not enough because no target module or tests are specified.

### P2.6 `proptest` should be in the phase plan

The plan mentions state transitions and includes `proptest` in the intended dependency set, but does not assign property-based tests to a phase. Good candidates:

- quantity invariant: `filled + pending + canceled == quantity`;
- lifecycle transition validity;
- cancel/modify idempotence after terminal states;
- generated orderbook monotonic bid/ask levels;
- fill behavior for market/limit/stop orders around trigger prices.

### P2.7 Phasing order mostly works, but compound can move earlier

R1-R7 is broadly sensible. There is no hard dependency from R6 back to R7 if `PaperBroker` only handles single orders. `CompoundOrder` can move earlier if order aggregation and MTM parity are needed before paper integration.

The critical issue is not ordering, but missing work items: `OrderLock`, timers/candles or their exclusion, `OrderStrategy`, error inventory, persistence decision, server shim decision, and parity matrix.

## Checklist Summary

- A1: `omspy/__init__.py` only exposes `__version__`; no critical re-exports missed there. But symbol omissions inside listed files are P0.
- A2: Indian broker adapters are correctly out of scope; add `api_helper.py` and `load_broker` to the exclusion.
- A3: `orders/` and `algos/` are not hard dependencies of `base.py` or `order.py`.
- A4: `multi.py` is not a hard dependency of core.
- A5: referenced utils are `create_basic_positions_from_orders_dict`, `dict_filter`, `tick`, `update_quantity`, and `UQty`.
- A6: unlisted trivial modules include `__init__.py` files; unlisted nontrivial broker helper is `brokers/api_helper.py`.
- B1: layout is not complete for `base.py`, `order.py`, `models.py`, or simulator files.
- B2: upstream HTTP tests exist; dropping server needs an explicit replacement or exclusion.
- B3: `brokers/paper.py` is a dummy broker, not the simulator binding.
- C1-C5: LOC and time are optimistic and test LOC is not budgeted separately.
- D1-D5: async, lifecycle enum, Decimal, SQLite, and errors all need stronger semantic specs.
- E1-E3: 5 parity scenarios is too weak; add a dedicated parity phase/matrix and property tests.
- F1-F2: phase order is mostly fine; audit rework is not budgeted.
- G1: no hidden dependency on skipped venue/algo/multi modules, except `utils.load_broker` if ported.
- G2: acceptance is open-ended.
- G3: error propagation is MVP-critical; observability can remain post-MVP.

## Required Changes Before ACK

1. Rewrite §1 as a symbol-level port inventory with `MVP/defer/drop` status.
2. Rewrite §2 simulator layout around actual upstream classes: `FakeBroker`, `VirtualBroker`, `ReplicaBroker`, `OrderFill`, response models, and the dummy `Paper` broker.
3. Add an explicit parity test matrix with named upstream tests or scenarios, not "pick 5".
4. Split LOC estimates into prod LOC and test LOC; revise schedule to include audit rework.
5. Add design specs for async broker semantics, lifecycle mutation compatibility, persistence exclusion or abstraction, numeric parity, and `OmsError`.

NACK until those are fixed.
