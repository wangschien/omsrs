# omsrs

**Rust port of [omspy](https://github.com/uberdeveloper/omspy)** — a broker-agnostic OMS for trading.

Pure-Rust re-implementation of omspy's MVP order-management core: `Order` lifecycle, `Broker` trait, paper-simulation engine, and virtual-broker matching engine. Built to host venue adapters for prediction-market exchanges (Polymarket, Kalshi) and anything else that fits the `Broker` shape.

## Status

**All 10 implementation phases complete.** Parity gate: 237 upstream pytest items, 236 pass + 1 excused (`test_order_timezone` — pendulum tz-name not expressible in `chrono::DateTime<Utc>`, §14B). Every phase codex-audited individually; see `docs/PORT-PLAN-R{1..10}-audit-result.md`.

| phase | items | surface |
|---|---:|---|
| R1 | 20 | `utils` + `BasicPosition` + parity harness (libtest-mimic) + 13-row smoke matrix |
| R2 | 10 | `Quote` / `OrderBook` / `OrderLock` + `Clock` trait + `MockClock` |
| R3 | 64 | `Order` (~40 fields, full lifecycle), `Broker` trait, `PersistenceHandle` trait + SQLite backend |
| R4 | 10 | `Broker::close_all_positions` / `cancel_all_orders` / `get_positions_from_orders` + `Paper` broker |
| R5 | 54 | Simulation models (`VOrder`, `VTrade`, `VPosition`, `VUser`, `Ticker`, `OHLC*`, `OrderFill`, 8 response types) + statistical target for `test_ticker_ltp_statistical` |
| R6 | 22 | `VirtualBroker` (multi-user via `VUser`, seeded RNG, clock-driven `get(order_id, status)`) |
| R7 | 10 | `ReplicaBroker` + `ReplicaFill` (shared ownership via `Arc<Mutex<VOrder>>`) |
| R8 | 40 | `CompoundOrder` (indexes, keys, aggregate views, execute_all, check_flags, save) |
| R9 | 7 | `OrderStrategy` + clock-cascade on `add` (§6 D4) |
| R10 | — | parity sweep + stabilisation |

## For downstream consumers (e.g. pbot)

omsrs is **ready to embed**. Write your venue adapter as `impl Broker for YourBroker` and every `Order::execute / modify / cancel` path works:

```rust
use std::collections::HashMap;
use omsrs::{Broker, Order, OrderInit};
use serde_json::Value;

pub struct PolymarketBroker { /* client, credentials, ... */ }

impl Broker for PolymarketBroker {
    fn order_place(&self, args: HashMap<String, Value>) -> Option<String> {
        // map kwargs → Polymarket CLOB API call, return broker-assigned order_id
    }
    fn order_modify(&self, args: HashMap<String, Value>) { /* ... */ }
    fn order_cancel(&self, args: HashMap<String, Value>) { /* ... */ }
    // attribs_to_copy_execute / _modify / _cancel if your venue wants extra kwargs
}

let mut order = Order::from_init(OrderInit {
    symbol: "BTC-YES".into(),
    side: "buy".into(),
    quantity: 10,
    order_type: Some("LIMIT".into()),
    price: Some(rust_decimal_macros::dec!(0.42)),
    ..Default::default()
});
let broker = PolymarketBroker::new(/* ... */);
order.execute(&broker, None, HashMap::new());
```

`CompoundOrder` and `OrderStrategy` give you basket / strategy-level aggregates (positions, MTM, net_value, average_buy/sell_price). `OrderLock` throttles modify/cancel with an injected `Clock`. Persistence goes through `PersistenceHandle` (SQLite reference impl behind the `persistence` feature).

## Features

```toml
[dependencies]
omsrs = "0.1"

# persistence = ["dep:rusqlite"]  — off by default (§7 "MSRV-minimum build")
# statistical-tests — test-only, gates tests/statistical
```

## Verification

- `scripts/parity_gate.sh` — release-mode full sweep, exits 0 on 237-item manifest
- `cargo test --test parity --all-features` — 237 / 236 pass / 1 excused, exit 0
- `cargo test --test parity_runner_smoke` — 13-row exit-code smoke matrix
- `cargo test --no-default-features` — 222-item effective manifest, all pass
- `cargo test --test statistical --features statistical-tests --release` — Z-mean / Z-std bounds on Ticker RNG path
- `cargo clippy --all-features --all-targets -- -D warnings` — clean
- `cargo fmt --check` — clean

## Scope

**In MVP** (10 phases complete):
- `Order` lifecycle (R3): ~40 fields, update / execute / modify / cancel / save / clone / add_lock
- `Broker` trait (R3/R4): object-safe, `Paper` reference impl
- `PersistenceHandle` trait (R3): SQLite backend behind `persistence` feature
- Simulation (R5-R7): `VirtualBroker`, `ReplicaBroker`, `Ticker`, fill engine
- Aggregates (R8/R9): `CompoundOrder` / `OrderStrategy`

**Out of MVP** (per PORT-PLAN §11 non-goals):
- Indian-broker adapters (Zerodha / Finvasia / ICICI / Neo / Noren) — deliberately skipped
- `omspy.algos` / `omspy.multi` / candle-tracker (`Candle`, `CandleStick`) — defer
- `yaml` override loading — defer, test fixtures pre-apply renames
- HTTP / WebSocket client crates — broker adapters are downstream's job

## Documentation

- `docs/PORT-PLAN.md` — the v11 ACKed port plan (§1–§14)
- `docs/omspy-source-notes.md` — symbol-level inventory of upstream
- `docs/PORT-PLAN-R{1..10}-audit-result.md` — per-phase codex audit results
- `docs/PORT-PLAN-v{2..11}-audit-result.md` — plan evolution history

## License

MIT. Matches upstream omspy.

## Upstream reference

- omspy (upstream): https://github.com/uberdeveloper/omspy
- local path: `~/refs/omspy/`
