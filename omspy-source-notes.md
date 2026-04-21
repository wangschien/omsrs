# omspy source notes — symbol-level inventory

All paths relative to `~/refs/omspy/omspy/`. LOC = number of source lines in upstream file.
Every class + every public function catalogued. **MVP / defer / drop** decision at the end.

---

## 1. `__init__.py` (1 LOC)

```python
__version__ = "0.1.0"
```

Nothing else. No re-exports.

## 2. `base.py` (340 LOC) — broker abstraction

**Imports**: `yaml`, `logging`, `inspect`, `copy.deepcopy`, `omspy.utils` (`from … import *`), `omspy.models`.

### Decorators
- `pre(func)` — wraps a method to run override-based key rewriting on kwargs before the call. Used by `Paper.order_place / order_modify / order_cancel`.
- `post(func)` — same idea, but rewrites response keys after call. Used by `Paper.orders / trades / positions`.

### `class Broker` — metaclass-style abstract
**Fields**: `_override: Dict[str, Dict]` (6 keys: `orders`, `positions`, `trades`, `order_place`, `order_cancel`, `order_modify`).

**`__init__(**kwargs)`** loads override yaml from `{class_file_path}.yaml` if present (e.g. `zerodha.yaml`).

**Methods**:
- `get_override(key)` — getter from `_override` dict.
- `set_override(key, values)` — setter.
- `authenticate()` — abstract. Raises `NotImplementedError`.
- `orders` — `@property`, abstract. Raises `NotImplementedError`.
- `trades` — `@property`, abstract. Raises `NotImplementedError`.
- `positions` — `@property`, abstract. Raises `NotImplementedError`.
- `order_place(symbol, side, order_type='MARKET', quantity=1, **kwargs) -> str` — abstract.
- `order_modify(order_id, **kwargs) -> str` — abstract.
- `order_cancel(order_id) -> str` — abstract.
- `@staticmethod rename(dct, keys) -> dict` — renames dict keys using a mapping.
- `close_all_positions(positions=None, keys_to_copy=None, keys_to_add=None, symbol_transformer=None, **kwargs)` — iterates positions + emits opposite-side MARKET orders.
- `cancel_all_orders(keys_to_copy=None, keys_to_add=None, **kwargs)` — iterates `self.orders`, cancels any that are not COMPLETE/CANCELED/REJECTED.
- `get_positions_from_orders(**kwargs) -> Dict[str, BasicPosition]` — filters orders via `dict_filter`, then delegates to `utils.create_basic_positions_from_orders_dict`.
- `cover_orders(stop, order_args=None, **kwargs)` — computes stop-loss price per net position, emits SL-M orders. Calls `utils.tick` for tick-rounding.

**Dependencies on `utils.py`**: `dict_filter`, `create_basic_positions_from_orders_dict`, `tick` (3 functions).

## 3. `models.py` (482 LOC) — data + time models

**Imports**: `pydantic`, `pendulum`, `logging`, `copy.deepcopy`.

### `QuantityMatch(BaseModel)` (4 LOC) — `buy`/`sell` counts + `is_equal` + `not_matched`.

### `BasicPosition(BaseModel)` (22 LOC) — position tracking
- Fields: `symbol`, `buy_quantity`, `sell_quantity`, `buy_value`, `sell_value`.
- Props: `net_quantity`, `average_buy_value`, `average_sell_value`.
- **Used by `utils.create_basic_positions_from_orders_dict` + `base.Broker.get_positions_from_orders`.**

### `Quote(BaseModel)` (8 LOC)
- Fields: `price`, `quantity`, `orders_count (Optional)`.
- Prop: `value = price * quantity`.

### `OrderBook(BaseModel)` (44 LOC)
- Fields: `bid: List[Quote]`, `ask: List[Quote]`.
- Props: `is_bid_ask`, `spread`, `total_bid_quantity`, `total_ask_quantity`.
- **Used by `simulation/models.py` (re-imported).**

### `Tracker(BaseModel)` (15 LOC) — high/low last-price tracker
- Fields: `name`, `last_price`, `high` (-inf), `low` (inf).
- Method: `update(last_price)`.

### `Timer(BaseModel)` (50 LOC) — start/end scheduling
- Fields: `start_time: pendulum.DateTime`, `end_time: pendulum.DateTime`, `timezone: Optional[str]`.
- Validator: `end_time > start_time` and `start_time > now()`.
- Props: `has_started`, `has_completed`, `is_running`.

### `TimeTracker(Tracker, Timer)` (1 LOC) — multi-inherit, nothing added.

### `OrderLock(BaseModel)` (92 LOC) — **hard dependency of `Order`**
- Fields: `max_order_{creation,modification,cancellation}_lock_time` (each default 60), `timezone`.
- Private attrs: `_{creation,modification,cancellation}_lock_till: pendulum.DateTime`.
- Methods: `create(seconds)`, `modify(seconds)`, `cancel(seconds)` — set lock-till into the future.
- Props: `can_create`, `can_modify`, `can_cancel`, `creation_lock_till`, `modification_lock_till`, `cancellation_lock_till`.

### `Candle(BaseModel)` (13 LOC) — OHLCV candle
- Fields: `timestamp, open, high, low, close, volume, info`.

### `CandleStick(BaseModel)` (195 LOC) — candle-stream aggregator
- Fields: `symbol, candles, initial_price, interval, timer, timezone, ltp, high, low, bar_open, bar_high, bar_low, next_interval, periods`.
- Methods: `add_candle`, `update_candle`, `update(ltp)`, `get_next_interval`, `_update_prices`.
- Props: `bullish_bars`, `bearish_bars`, `last_bullish_bar_index`, `last_bearish_bar_index`, `last_bullish_bar`, `last_bearish_bar`.

## 4. `utils.py` (243 LOC)

### `UQty(NamedTuple)` — `(q, f, p, c)` update-quantity result.
### Functions
- `create_basic_positions_from_orders_dict(orders) -> Dict[str, BasicPosition]` — used by `base.py`.
- `dict_filter(lst, **kwargs) -> List[Dict]` — AND filter. Used by `base.py`.
- `tick(price, tick_size=0.05) -> float` — round to tick. Used by `base.py`.
- `stop_loss_step_decimal(price, side='B', dec=0.45, step=2) -> float` — step-aligned stop. **Not referenced by in-scope files.** Used only in tests + downstream brokers.
- `update_quantity(q, f, p, c) -> UQty` — quantity-state update. Used by `simulation/models.py`.
- `load_broker(credentials, index=0)` — dynamic broker loader that imports Indian brokers. **Out of scope** (imports `zerodha/finvasia/icici/neo/noren`).

## 5. `order.py` (1468 LOC) — **biggest file**

**Imports**: `pydantic`, `pendulum`, `uuid`, `sqlite3`, `collections.Counter/defaultdict`, `copy.deepcopy`, `sqlite_utils.Database`, `omspy.base.*`, `omspy.models.OrderLock`.

### Module-level
- `get_option(spot, num=0, step=100.0) -> float` — options strike calculator. **Not used internally.** Used by tests + downstream. **DROP** (instrument-specific helper, irrelevant outside Indian equity options).
- `create_db(dbname=":memory:") -> Union[Database, None]` — makes sqlite3 'orders' table. **NOT** called by `CompoundOrder.__init__` (verified `order.py:741-760`); it is invoked by test fixtures and by external callers before constructing an `Order`/`CompoundOrder` with `connection=<db>`. See §13 point 4.

### `class Order(BaseModel)` (~600 LOC)
**Fields (40)**: `symbol, side, quantity, id, parent_id, timestamp, order_type, broker_timestamp, exchange_timestamp, order_id, exchange_order_id, price, trigger_price, average_price, pending_quantity, filled_quantity, cancelled_quantity, disclosed_quantity, validity, status, expires_in, timezone, client_id, convert_to_market_after_expiry, cancel_after_expiry, retries, max_modifications, exchange, tag, connection (Database), can_peg, pseudo_id, strategy_id, portfolio_id, JSON, error, is_multi, last_updated_at`.

**Private**: `_num_modifications: int`, `_attrs: Tuple` (7 fields that can be updated from broker: `exchange_timestamp, exchange_order_id, status, filled_quantity, pending_quantity, disclosed_quantity, average_price`), `_exclude_fields: Set` = `{"connection"}`, `_lock: OrderLock`, `_frozen_attrs: Set` = `{"symbol", "side"}`.

**Validators**: `quantity_not_negative`, `json_string_to_dict` (DB → dict), `timestamp_string_to_datetime` (DB → pendulum).

**Init behavior**:
- Auto-generates `id = uuid4().hex` if unset.
- Auto-sets `timestamp = pendulum.now(tz=timezone)`.
- `pending_quantity = quantity` at init.
- `expires_in = seconds until end-of-day` if 0, else `abs(expires_in)`.
- Auto-creates `OrderLock(timezone)` if unset.

**Status props**:
- `is_complete` — `quantity == filled_quantity` or `status == 'COMPLETE'` or `filled + cancelled == quantity`.
- `is_pending` — not (COMPLETE/CANCELED/REJECTED) and `filled + cancelled < quantity`.
- `is_done` — `is_complete` OR status in (CANCELLED, CANCELED, REJECTED).

**Expiry props**:
- `time_to_expiry` — `max(0, expires_in - (now - timestamp).seconds)`.
- `time_after_expiry` — `max(0, (now - timestamp).seconds - expires_in)`.
- `has_expired` — `time_to_expiry == 0`.

**Other props**: `has_parent`, `lock`.

**Methods**:
- `_get_other_args_from_attribs(broker, attribute, attribs_to_copy=None)` — extracts broker-specified or user-specified extra kwargs to pass through.
- `update(data, save=True) -> bool` — writes `data` fields into `_attrs` ONLY if `not is_done`. Recomputes `pending_quantity` if missing.
- `execute(broker, **kwargs) -> str or None` — calls `broker.order_place(symbol, side, quantity, order_type, trigger_price, disclosed_quantity, validity, price, ...)`. Sets `order_id`. Respects `lock.can_create`.
- `modify(broker, **kwargs) -> str or None` — calls `broker.order_modify(order_id, quantity, price, trigger_price, order_type, disclosed_quantity, validity)`. Respects `_num_modifications < max_modifications` + `lock.can_modify`. Certain fields are frozen after creation.
- `cancel(broker, attribs_to_copy=None) -> None` — calls `broker.order_cancel(order_id)`. Respects `lock.can_cancel`.
- `save_to_db() -> bool` — writes self as dict into `connection.table('orders')`.
- `clone() -> Order` — deep copy with new id.
- `add_lock(code, seconds)` — delegates to `OrderLock` (code 1 → create, 2 → modify, 3 → cancel).

### `class CompoundOrder(BaseModel)` (~570 LOC)
**Fields**: `broker (Any)`, `id (Optional[str])`, `connection (Optional[Database])`, `ltp (defaultdict(float))`, `orders (List[Order])`, `_index (Dict[Hashable, int])`, `_broker_attrs`, others.

**Init**: auto-id, creates tables if `connection is None`.

**Methods / props** (61 members):
- `__len__`, `count`, `positions (Counter)` — aggregation.
- `_get_next_index`, `_get_by_key`, `_get_by_index`, `get(key)` — index management (can look up by key, by index, by pseudo_id).
- `add_order(**kwargs) -> Optional[str]` — build `Order(**kwargs)`, add to list, index by id/pseudo_id.
- `_average_price(side='buy') -> Dict[str, float]`, `average_buy_price`, `average_sell_price`.
- `update_orders(data: Dict[str, Dict]) -> Dict[str, bool]` — bulk update by order_id.
- `_total_quantity`, `buy_quantity`, `sell_quantity` — `Counter` by symbol.
- `update_ltp(last_price: Dict[str, float])` — writes into `self.ltp`.
- `net_value`, `mtm`, `total_mtm` — P&L calcs.
- `execute_all(**kwargs)` — place all contained orders.
- `check_flags()` — inspects expiry + retry logic; may cancel or convert to MARKET.
- `completed_orders`, `pending_orders` — list filters.
- `add(order: Order)` — append existing `Order` to list + index.
- `save()` — persist all orders.

### `class OrderStrategy(BaseModel)` (~188 LOC, lines 1280-1468)
**Fields**: `broker`, `orders: List[CompoundOrder]`, `ltp (defaultdict(float))`.

**Methods/props**:
- `positions (Counter)` — aggregates across compound orders.
- `update_ltp(last_price)` — fan out to each compound order.
- `update_orders(data)` — fan out.
- `mtm`, `total_mtm`.
- `run(ltp=None)` — invoke `check_flags` on every compound order.
- `add(compound_order: CompoundOrder)`.
- `save()`.

**Has independent test file `test_order_strategy.py` (7 tests, 128 LOC).** Upstream `__init__.py` does not re-export it, but external users import via `from omspy.order import Order, CompoundOrder, OrderStrategy, create_db`.

## 6. `simulation/__init__.py` (1 LOC)

Package marker.

## 7. `simulation/models.py` (594 LOC) — paper-exchange types

**Imports**: `pydantic`, `enum.Enum`, `random`, `uuid`, `pendulum`, `omspy.utils as utils` (only `update_quantity` used), `omspy.models.OrderBook`.

### Enums (verified against upstream source `simulation/models.py:16-43`)
- `Status(Enum)` — `COMPLETE=1, REJECTED=2, CANCELED=3, PARTIAL_FILL=4, OPEN=5, PENDING=6` (6 values, integer).
- `ResponseStatus(str, Enum)` — `SUCCESS="success", FAILURE="failure"` (2).
- `Side(Enum)` — `BUY=1, SELL=-1`.
- `TickerMode(Enum)` — `RANDOM=1, MANUAL=2`.
- `OrderType(Enum)` — `MARKET=1, LIMIT=2, STOP=3` (3 values, **no SL/SL-M**).

### Models (BaseModel)
- `OHLC`, `OHLCV(OHLC)`, `OHLCVI(OHLCV)` — candle variants.
- `Ticker(BaseModel)` — stream config (`~77 LOC`).
- `VQuote(OHLCV)` — quoted instrument snapshot.
- **`VTrade(BaseModel)`** (16 LOC) — paper trade. Fields: `trade_id, order_id, symbol, side, quantity, price, fee`.
- **`VOrder(BaseModel)`** (~207 LOC) — paper order. Fields: `order_id, symbol, side, quantity, price, trigger_price, order_type, status, filled_quantity, pending_quantity, cancelled_quantity, disclosed_quantity, validity, exchange, tag, average_price, timestamp, ...`. Methods: state setters + validators. Calls `utils.update_quantity`.
- **`VPosition(BaseModel)`** (52 LOC) — paper position. Fields + net calcs.
- `VUser(BaseModel)` (17 LOC) — multi-user support.
- Response models (8 classes): `Response, OrderResponse, AuthResponse, GenericResponse, LTPResponse, OHLCVResponse, QuoteResponse, OrderBookResponse, PositionResponse`.
- `Instrument(BaseModel)` (22 LOC) — instrument metadata.
- **`OrderFill(BaseModel)`** (~89 LOC) — fill-engine config. Controls the probability/schedule of fills for `ReplicaBroker.run_fill`. Fields: `probability, percentage, symbol_map, ...`.

## 8. `simulation/virtual.py` (836 LOC) — the actual paper engine

**Imports**: various simulation models, `random`, `numpy`, `pendulum`, etc.

### Module functions
- `user_response(f)` — decorator that wraps broker methods to attach user context.
- `_iterate_method(func, ...)` — helper to map method over iterable input.
- `generate_price(start=100, end=110) -> int` — random price.
- `generate_orderbook(...)` → synthetic `OrderBook`.
- `generate_ohlc(start=100, end=110, volume=10000) -> OHLCV`.

### `class FakeBroker(BaseModel)` (~343 LOC)
Dummy broker that returns random/synthetic market data + synthetic order responses. **No real matching.** Just:
- `_create_order_args`, `_get_random_symbols`.
- `_ltp`, `ltp`, `_orderbook`, `orderbook`, `_ohlc`, `ohlc`, `_quote`, `quote` — all call the `generate_*` helpers.
- `_avg_fill_price`, `order_place`, `order_modify`, `order_cancel` — generate `VOrder` with random fill price.
- `positions`, `orders`, `trades` — synthetic lists.

### `class VirtualBroker(BaseModel)` (~222 LOC)
Multi-user paper broker with actual state:
- `_users: Dict[str, VUser]`, `_orders: Dict[str, VOrder]`, `_tickers: Dict[str, Ticker]`.
- Methods: `add_user`, `clients`, `is_failure`, `get`, `order_place`, `order_modify`, `order_cancel`.
- Market-data methods: `update_tickers(last_price)`, `_ltp`, `ltp`, `_ohlc`, `ohlc`, `_quote`, `quote`.
- Orders stored in internal dict; no matching loop — orders transition state externally (e.g. when `update_tickers` moves price past a limit).

### `class ReplicaBroker(BaseModel)` (~99 LOC)
Matching/fill-engine broker:
- `update(instruments)` — register tradeable instruments.
- `order_place`, `order_modify`, `order_cancel` — validated placement.
- `run_fill()` — drives `OrderFill` config to generate synthetic fills on open orders.

## 9. `simulation/server.py` (174 LOC)
Flask HTTP server wrapping `VirtualBroker`. Provides REST routes so a Python client could talk to a paper exchange over HTTP. **DROP** — omsrs uses in-process channels.

## 10. `brokers/paper.py` (52 LOC) — dummy paper broker

`class Paper(Broker)` — trivial broker that echoes configured data:
- Init: `(orders=None, trades=None, positions=None)` — stored as private attributes; `authenticate()` returns True.
- `orders`, `trades`, `positions` — `@property @post` — return stored data or `[{}]`.
- `order_place(**kwargs) -> dict` — returns the kwargs.
- `order_modify(order_id, **kwargs)` — returns `order_id` updated dict.
- `order_cancel(order_id) -> str` — returns order_id.

**This is NOT the paper matching engine.** The matching engine is in `simulation/virtual.py` (`FakeBroker`/`VirtualBroker`/`ReplicaBroker`). `brokers/paper.py` is just an override-compatible dummy for testing the override mechanism in `base.Broker`.

---

## 11. Upstream test coverage (source of truth for parity) — pytest-item denominator

Upstream uses `@pytest.mark.parametrize` and occasional duplicate function names. The v4 audit showed "function-name counts" undercounted the true pytest collection by ignoring parametrize expansion and duplicate overwrites. Authoritative count = **pytest items** after collection.

### Parametrize expansion (portable tests only)

| Test | Function names | pytest items |
|---|---:|---:|
| `tests/test_models.py::test_order_lock_can_methods` | 1 | **3** (can_create / can_modify / can_cancel) |
| `tests/test_utils.py::test_update_quantity` | 1 | **6** |
| `tests/simulation/test_models.py::test_vorder_is_done` | 1 | **6** |

### Duplicate function-name overwrites (pytest collects only the last)

| Test file | Duplicate name | Function bodies → collected |
|---|---|---:|
| `tests/test_order.py` | `test_compound_order_update_orders` | 2 → **1** |
| `tests/simulation/test_models.py` | `test_vorder_modify_by_status_partial_fill` | 2 → **1** |
| `tests/simulation/test_virtual.py` | `test_virtual_broker_ltp` | 2 → **1** |

### Counts — portable MVP (pytest items)

| Upstream file | pytest items (total) | Portable (MVP) | Excluded | Reason |
|---|---:|---:|---:|---|
| `test_base.py` | 12 | **10** | 2 | `cover_orders` |
| `test_models.py` | 13 | **13** | 0 | — |
| `test_models_tracker.py` | 8 | 0 | 8 | `Tracker/Timer/TimeTracker` deferred |
| `test_models_candles.py` | 19 | 0 | 19 | `Candle/CandleStick` deferred |
| `test_utils.py` | **34** | **17** | 17 | 1 `tick` + 8 `stop_loss_step_decimal[...]` (parametrize) + 8 `test_load_broker_*` |
| `test_order.py` | **107** | **104** | 3 | `test_get_option` (parametrize 3) |
| `test_order_strategy.py` | 7 | **7** | 0 | — |
| `tests/simulation/test_models.py` | 55 | **55** | 0 | — |
| `tests/simulation/test_virtual.py` | 79 | **32** | 47 | FakeBroker 38 + `generate_*` 9 |
| **Totals** | **334** | **238 gross** | — | before Ticker exception |

`test_utils.py` item arithmetic: `22 function names − 1 (stop_loss body) + 8 (parametrize) − 1 (update_quantity body) + 6 (parametrize) = 34 items`.
`test_order.py` item arithmetic: `105 pytest-collected bodies (after dup collapse) + 2 extra parametrize from test_get_option = 107`.

### Parity denominator (v7 decision: 237, excludes Ticker exception)

`test_ticker_ltp` is replaced in Rust by a statistical version (`test_ticker_ltp_statistical`) that asserts mean / std over 1000 samples. Since the Rust test does **not** behaviorally match `test_ticker_ltp`, it is not counted as a parity pass.

**Denominator = 238 − 1 = 237**.

Test_ticker_ltp is **removed from the portable set**, not listed as "failure". The Rust statistical test lives in a separate module and is not a parity item.

### §11.1a Phase allocation (current, denominator = 237)

Every portable pytest item belongs to exactly one phase. Phase gates sum to 237.

| Phase | Source file | Items | Scope |
|---|---|---:|---|
| R1 | `test_utils.py` (portable) + `test_models.py` (3 × `BasicPosition`; no upstream `QuantityMatch` test exists in `tests/test_models.py`) | 17 + 3 = **20** | utils + basic position models |
| R2 | `test_models.py` (Quote, OrderBook, OrderLock) | **10** | 1 Quote + 3 OrderBook + 6 OrderLock (including parametrize 3 `can_*`) |
| R3 | `test_order.py` non-`test_compound_order_*` items (§11.1b) | **64** | Order + persistence + lifecycle |
| R4 | `test_base.py` | **10** | Broker trait + Paper |
| R5 | `test_simulation_models.py` minus `test_ticker_ltp` | **54** | simulation data types + Ticker exception |
| R6 | `test_simulation_virtual.py` VirtualBroker | **22** | multi-user VirtualBroker |
| R7 | `test_simulation_virtual.py` ReplicaBroker | **10** | ReplicaBroker + matching |
| R8 | `test_order.py` `test_compound_order_*` items (§11.1b) | **40** | CompoundOrder |
| R9 | `test_order_strategy.py` | **7** | OrderStrategy + persistence chain |
| **Total** | | **237** | matches denominator |

Verification: `20 + 10 + 64 + 10 + 54 + 22 + 10 + 40 + 7 = 237` ✓

### §11.1b In-module test split by symbol (for R3 vs R8) — deterministic rule

`test_order.py` 104 portable pytest items split across two MVP symbols by a single mechanical rule:

- **R8** = every collected pytest item whose function name matches `^test_compound_order_` (prefix match, Python-style). Upstream file defines 41 such `def`s, one of which (`test_compound_order_update_orders`) is overwritten by a later duplicate definition, so Python collects **40 unique** items.
- **R3** = all other portable items = `104 − 40 = 64`.

| Phase | Selector | pytest items |
|---|---|---:|
| R3 | collected name does **not** start with `test_compound_order_`; covers `Order` / `OrderLock` / persistence / db + `create_db` / `Order.update/execute/modify/cancel/save_to_db/clone/add_lock` / lifecycle + expiry + `test_get_order*` / `test_save_to_db*` | **64** |
| R8 | collected name matches `^test_compound_order_` exactly | **40** |

The rule is executed at R3 gate via `pytest --collect-only -q tests/test_order.py` followed by a name-prefix filter; the resulting two lists are frozen and committed to `rust-tests/parity-item-manifest.txt`.

### MVP parity gate (current)

**≥ 230 of 237 portable pytest items pass** (≥ 97.0%).

7-item slack covers tz/DST candidates (enumerated at phase gates) and any probabilistic test promoted to §14. No slack reserved for Ticker seed (already excluded from 237 denominator).

Excluded upstream files:
- `tests/test_multi.py` (261 LOC, multi-broker) — `multi.py` dropped.
- `tests/test_omspy.py` (5 LOC) — version smoke.
- `tests/brokers/*`, `tests/algos/*`, `tests/orders/*` — out-of-scope adapters.
- `tests/simulation/test_server.py` (173 LOC) — Flask server dropped.

---

## 12. Decision table — every in-scope symbol

| Module | Symbol | Classification | Reason |
|---|---|---|---|
| `base.py` | `Broker` | **MVP** | Core trait |
| `base.py` | `pre` decorator | **MVP** (simplified) | Needed for Paper echo behavior. Rust equivalent: trait method wrapper. |
| `base.py` | `post` decorator | **MVP** (simplified) | Same. |
| `base.py` | override yaml loading | **defer** | Venue-key renaming. Can be added later; not on Order lifecycle path. |
| `base.py` | `close_all_positions` | **MVP** | Safety primitive. |
| `base.py` | `cancel_all_orders` | **MVP** | Safety primitive. |
| `base.py` | `get_positions_from_orders` | **MVP** | Uses BasicPosition. |
| `base.py` | `cover_orders` | **defer** | SL-M specific. Not core. |
| `models.py` | `QuantityMatch` | **MVP** | Simple helper. |
| `models.py` | `BasicPosition` | **MVP** | `get_positions_from_orders` dep. |
| `models.py` | `Quote` | **MVP** | `OrderBook` dep. |
| `models.py` | `OrderBook` | **MVP** | Sim uses it. |
| `models.py` | `Tracker` | **defer** | Candle-path, not order-path. |
| `models.py` | `Timer` | **defer** | Candle-path. |
| `models.py` | `TimeTracker` | **defer** | Candle-path. |
| `models.py` | `OrderLock` | **MVP** | Hard dep of `Order`. |
| `models.py` | `Candle` | **drop from MVP** | Market-data candle aggregator, not OMS. Put in `omsrs::candles` sub-crate or later. |
| `models.py` | `CandleStick` | **drop from MVP** | Same. |
| `utils.py` | `UQty` | **MVP** | Sim uses `update_quantity`. |
| `utils.py` | `create_basic_positions_from_orders_dict` | **MVP** | `base.Broker.get_positions_from_orders` dep. |
| `utils.py` | `dict_filter` | **MVP** | Same path. |
| `utils.py` | `tick` | **defer** | `base.Broker.cover_orders` is deferred → `tick` has no MVP caller. Corresponding `test_tick` excluded from portable count. |
| `utils.py` | `stop_loss_step_decimal` | **defer** | Not referenced in core. |
| `utils.py` | `update_quantity` | **MVP** | Sim `VOrder` uses it. |
| `utils.py` | `load_broker` | **drop** | Imports Indian brokers. |
| `order.py` | `get_option` | **drop** | Indian equity options helper. |
| `order.py` | `create_db` | **MVP optional** | SQLite persistence; expose behind `persistence` feature flag. |
| `order.py` | `Order` | **MVP** | Core. |
| `order.py` | `CompoundOrder` | **MVP** | Core. |
| `order.py` | `OrderStrategy` | **MVP** | Has own tests; external API. |
| `simulation/models.py` | `Status` enum | **MVP** | |
| `simulation/models.py` | `ResponseStatus` enum | **MVP** | |
| `simulation/models.py` | `Side` enum | **MVP** | |
| `simulation/models.py` | `TickerMode` enum | **MVP** | |
| `simulation/models.py` | `OrderType` enum | **MVP** | |
| `simulation/models.py` | `OHLC` | **MVP** (reversed) | `OHLCV` inherits it. |
| `simulation/models.py` | `OHLCV` | **MVP** (reversed) | `VQuote` inherits it (`sim/models.py:139`). Without it, `VQuote` won't type-check. |
| `simulation/models.py` | `OHLCVI` | **MVP** | NOT required by `VQuote`/`VirtualBroker` inheritance (`VQuote : OHLCV`). Kept only so `test_ohlcvi` remains portable; alternative is to exclude both `OHLCVI` and that test. |
| `simulation/models.py` | `Ticker` | **MVP** (reversed) | `VirtualBroker.tickers: Dict[str, Ticker]` + `_ohlc/_quote` use it. |
| `simulation/models.py` | `VQuote`, `VTrade`, `VOrder`, `VPosition` | **MVP** | Paper sim core. |
| `simulation/models.py` | `VUser` | **MVP** | Multi-user path in VirtualBroker. |
| `simulation/models.py` | Response models (8) | **MVP** | Used by Virtual/Replica brokers. |
| `simulation/models.py` | `Instrument` | **MVP** | `ReplicaBroker.update` uses. |
| `simulation/models.py` | `OrderFill` | **MVP** | `ReplicaBroker.run_fill` config. |
| `simulation/virtual.py` | `user_response` decorator | **MVP** (simplified) | User-context wrapping. |
| `simulation/virtual.py` | `_iterate_method` | **MVP** | Internal helper. |
| `simulation/virtual.py` | `generate_price` | **defer** | Random market-data; only needed if we port FakeBroker fully. |
| `simulation/virtual.py` | `generate_orderbook` | **defer** | Same. |
| `simulation/virtual.py` | `generate_ohlc` | **defer** | Market-data. |
| `simulation/virtual.py` | `FakeBroker` | **defer from MVP** | Random-data dummy. Pbot doesn't need random prices; it feeds real book data. Can add later. |
| `simulation/virtual.py` | `VirtualBroker` | **MVP (multi-user per upstream)** | Upstream is multi-user via `VUser`. Portable `test_virtual_broker_add_user`/`_order_place_users`/`_order_place_same_memory` require it. |
| `simulation/virtual.py` | `ReplicaBroker` | **MVP** | Matching engine — fills drive off `OrderFill` config. Core of paper trading. |
| `simulation/server.py` | Flask server | **drop** | HTTP layer; Rust uses channels. |
| `brokers/paper.py` | `Paper` | **MVP** | Dummy broker for testing override mechanism. ~50 LOC Rust. |

### MVP totals (summary, revised)

In-scope MVP symbol set:
- `base.Broker` + `pre/post` + 6 methods + override (simplified) + yaml **drop** for MVP
- `models`: 5 MVP types (`QuantityMatch`, `BasicPosition`, `Quote`, `OrderBook`, `OrderLock`)
- `utils`: 4 MVP functions (`create_basic_positions_from_orders_dict`, `dict_filter`, `update_quantity`, `UQty`)
- `order`: `Order`, `CompoundOrder`, `OrderStrategy`, `create_db` (feature-flagged) + `PersistenceHandle` trait (unconditionally declared)
- `simulation/models`: 5 enums + **`OHLC/OHLCV`** (`VQuote : OHLCV` inheritance chain) + **`OHLCVI`** (kept only so `test_ohlcvi` stays in the portable set; NOT required by `VQuote`/`VirtualBroker`) + **`Ticker`** (used by `test_ticker_*` parity items; `test_ticker_ltp` excluded via §14(A)) + `VQuote/VTrade/VOrder/VPosition/VUser` + 8 responses + `Instrument` + `OrderFill`
- `simulation/virtual`: `VirtualBroker` (multi-user per upstream, includes `VUser` handling), `ReplicaBroker`

### Python LOC in MVP set (revised estimate)

| Module | In-scope LOC |
|---|---|
| `base.py` minus yaml+cover_orders | ~250 |
| `models.py` MVP subset (QuantityMatch + BasicPosition + Quote + OrderBook + OrderLock) | ~180 |
| `utils.py` MVP subset | ~80 |
| `order.py` minus `get_option` | ~1400 |
| `simulation/models.py` MVP subset (**now includes** OHLC/OHLCV/OHLCVI/Ticker ~90 LOC) | ~540 |
| `simulation/virtual.py` (VirtualBroker + ReplicaBroker, drop FakeBroker) | ~350 |
| `brokers/paper.py` | 52 |
| **Total** | **~2850 Python LOC in MVP** |

Rust multiplier **1.4-1.6×** → **4000-4560 Rust LOC** for core. Plan target **5000 Rust prod LOC** (1.75×, 9% safety margin).

### (Legacy) Behavior-parity test count — superseded by §11 above. Keep for history only.

#### (Legacy) Behavior-parity test count (ceiling, **pytest-collected**)

Python allows duplicate test-function names in the same module; pytest
collects only the last definition, silently dropping the earlier ones.
Counting function bodies (naive) vs pytest-collected (authoritative):

| Upstream test file | Function bodies | pytest-collected | Portable (MVP) | Reason for exclusion |
|---|---:|---:|---:|---|
| `test_base.py` | 12 | 12 | 10 | `cover_orders` deferred (2) |
| `test_models.py` | 11 | 11 | 11 | — |
| `test_models_tracker.py` | 8 | 8 | 0 | `Tracker/Timer/TimeTracker` deferred |
| `test_models_candles.py` | 19 | 19 | 0 | `Candle/CandleStick` deferred |
| `test_utils.py` | 22 | 22 | 12 | `tick` 1, `stop_loss_step_decimal` 1, `load_broker` 8 |
| `test_order.py` | 106 | **105** | **104** | duplicate `test_compound_order_update_orders` overwritten; `get_option` dropped |
| `test_order_strategy.py` | 7 | 7 | 7 | — |
| `tests/simulation/test_models.py` | 51 | **50** | **50** | duplicate `test_vorder_modify_by_status_partial_fill` overwritten |
| `tests/simulation/test_virtual.py` | 80 | **79** | **32** | duplicate `test_virtual_broker_ltp` + FakeBroker 38 + `generate_*` 9 dropped |
| **Total** | **316** | **313** | **226** | pytest-collected portable ceiling |

**MVP parity gate: ≥ 218 of 226 pass** (≥ 96.5%). Failures require codex-approved excuse entry in §14 (below).

**R6/R7 split** (corrected from v4):
- R6 `VirtualBroker` — **22 tests** (pytest-collected after overwrite elimination).
  Includes multi-user behavior (`test_virtual_broker_add_user`, `test_virtual_broker_order_place_users`, `test_virtual_broker_order_place_same_memory`). ⇒ drop the "single-user MVP" framing; `VirtualBroker` is multi-user via `VUser` per upstream.
- R7 `ReplicaBroker` — **10 tests** (includes `test_replica_broker_order_place_multiple_users`).

### §14 Parity-denominator exclusions + exception register (current)

Two categories:

**(A) Exclusions from the 238-item gross set → denominator becomes 237.**
These tests are replaced in Rust by non-parity alternatives because Python RNG byte-semantics cannot be matched. Not counted as "failure"; they're simply out of scope.

| Upstream test id | Type | Rust replacement |
|---|---|---|
| `tests/simulation/test_models.py::test_ticker_ltp` | seed-exact RNG (`random.seed(1000)` + `random.gauss(0,1)` rounded to 0.05 → exact `_ltp=120.5/_high=125.3/_low=120.5`) | `test_ticker_ltp_statistical` — 1000-sample mean ∈ [−0.1, +0.1], std ∈ [0.9, 1.1]. Lives in separate module. Not counted in parity pass rate. |

**(B) Excused failures inside the 237-item denominator.**
Tests that remain in the portable set but are allowed to fail up to 7 times (slack = 237 − 230). Each failure requires codex audit approval at its phase gate with a rationale row here.

No pre-authorized entries at R0; `tests/parity/excused.toml` starts empty and rows are added only after codex approval at the owning phase gate. Candidates (finalized only at phase gate after real Rust runs):

| Upstream test id | Candidate sensitivity | Phase gate |
|---|---|---|
| `tests/test_order.py::test_order_timezone` | `pendulum.now(tz)` vs `chrono::Local::now()` DST semantics | R3 |
| `tests/test_order.py::test_order_expiry_times[...]` | end-of-day crossing DST | R3 |
| `tests/test_models.py::test_order_lock_can_methods[can_*]` (3 items) | Clock tick granularity | R2 |
| `tests/simulation/test_models.py::test_ticker_ticker_mode` | asserts `ltp != 125`; Ticker rounds Normal perturbation to 0.05 → non-zero probability of `ltp == 125` after mode switch. **Not** pre-authorized; if the Rust test proves flaky under statistical seeds, it moves to (B) with codex approval at R5 gate (probabilistic-parity exception), NOT `#[ignore]`. The (B) acceptance criterion for this test (if promoted) is: passes ≥ 95/100 independent `SmallRng::seed_from_u64(seed)` runs with `seed ∈ 0..100`; that ratio is proven in `tests/parity/test_ticker_ticker_mode.rs` via a 100-seed loop asserting ≥ 95 successes, not via a single-seed parity assertion. | R5 |

**Rules**:
- No test enters (B) without a codex-approved rationale row at its phase gate.
- No `#[ignore]` attribute anywhere — every excused failure must be a tracked (B) entry.
- If all 7 slack items are consumed before R10, gate tightens (≥ 237 pass required) for remaining phases.

### Rust test LOC (current, denominator = 237 pytest items; see `PORT-PLAN.md` §3)

At **20 LOC/test** realistic (Rust parity tests with fixtures + golden values):
- **237** pytest items × 20 = **4740 LOC** base (Ticker `test_ticker_ltp` removed via §14(A))
- Shared fixtures / helpers ~500 LOC
- Proptest modules ~300 LOC (`update_quantity` conservation, `Order` lifecycle invariants, `BasicPosition` arithmetic)
- Clock test harness + MockClock fixtures ~200 LOC
- Ticker statistical replacement (`tests/statistical/test_ticker_ltp_statistical.rs`, separate module, not in 237): ~50 LOC
- **Total test LOC ~5790**

---

## 13. Key non-obvious dependencies

1. `Order` depends on `OrderLock`. `OrderLock` must be in MVP.
2. `base.Broker.get_positions_from_orders` depends on `utils.dict_filter` + `utils.create_basic_positions_from_orders_dict` + `models.BasicPosition`.
3. `simulation/models.VOrder` depends on `utils.update_quantity`.
4. **`CompoundOrder.__init__` does NOT call `create_db`** (prior note was wrong, verified `order.py:741-760`). It only `uuid.uuid4().hex`, defaults `order_args = {}`, and rebuilds `_index` for pre-existing orders.
5. However `connection` IS read/propagated at many sites inside `Order` / `CompoundOrder`, not only in `save_to_db`:
   - `Order.update()` at `order.py:454` — `if self.connection and save:` post-update save
   - `Order.execute()` at `order.py:521` — `if self.connection:` post-execute save
   - `Order.modify()` at `order.py:612` — `if self.connection:` post-modify save
   - `Order.save_to_db()` at `order.py:660` — the save method itself
   - `CompoundOrder.add_order()` at `order.py:863` — injects `kwargs["connection"] = self.connection` into new Orders
   - `CompoundOrder.add()` at `order.py:1239` — backfills `order.connection = self.connection`
   - `CompoundOrder.save()` at `order.py:1258` — iterates calling `order.save_to_db()`

**Decision for MVP**: keep a **`connection: Option<Box<dyn PersistenceHandle>>`** field on Order/CompoundOrder **unconditionally**. The trait `PersistenceHandle` has one method `save(&self, order: &Order)`; its SQLite impl and `create_db` go behind `#[cfg(feature = "persistence")]`. Without the feature, the trait + field still exist but no impl is usable, so callers always pass `None` and every save site becomes a no-op. No cfg-split in `Order.update/execute/modify` or `CompoundOrder.add_order/add/save`. Clean.

5. `Paper(Broker)` imports `pre/post` decorators from `base`. Those are used to test override mechanism but Paper never processes real fills. Rust equivalent: `#[derive]` macro or simple trait wrapper.
