# omsrs

[![crates.io](https://img.shields.io/crates/v/omsrs.svg)](https://crates.io/crates/omsrs)
[![docs.rs](https://img.shields.io/docsrs/omsrs)](https://docs.rs/omsrs)
[![CI](https://github.com/wangschien/omsrs/actions/workflows/ci.yml/badge.svg)](https://github.com/wangschien/omsrs/actions/workflows/ci.yml)
[![license](https://img.shields.io/crates/l/omsrs.svg)](https://github.com/wangschien/omsrs/blob/main/LICENSE)

**Rust port of [omspy](https://github.com/uberdeveloper/omspy)** - broker-agnostic order-management primitives for Rust.

`omsrs` provides sync and async broker traits, order lifecycle management,
paper simulation, virtual matching engines, order aggregation, persistence,
and a deterministic event-driven lifecycle core. Applications supply their
own venue adapter by implementing `Broker` or `AsyncBroker`.

## Status

**v0.1.0** (2026-04-21): initial order, broker, paper simulation,
virtual matching, persistence, compound-order, and strategy APIs.

**v0.2.0** (2026-04-21): additive `AsyncBroker` trait and `AsyncPaper`
reference implementation. Existing synchronous APIs remain unchanged.

**v0.3.0** (2026-04-22): completes the asynchronous API with
`AsyncVirtualBroker`, `AsyncReplicaBroker`, async order lifecycle methods,
`AsyncCompoundOrder`, and `AsyncOrderStrategy`. Existing v0.2 APIs remain
unchanged. The async additions use a consistent pattern:
- Inherent methods return the rich sync shape (`BrokerReply`, `OrderHandle`) for callers that want it
- `impl AsyncBroker` provides a lossy `Option<String>` adapter for trait-object use
- Broker operations do not require a particular executor; the narrow `tokio`
  dependency supports non-blocking persistence writes
- Shared-identity invariants (`Arc::ptr_eq` across matching-engine collections) preserved
- Order's async lifecycle methods are siblings, not replacements — sync `execute` / `modify` / `cancel` still compile and work

**v0.3.1-v0.3.3** (2026-04-27 to 2026-06-12): hardens asynchronous
cancellation, persistence, cumulative fill handling, and Rust 1.78 build
compatibility.

**v0.4.0** (2026-09-02): adds a deterministic per-order lifecycle state
machine, typed client/venue/fill/attempt identities, replayable journal
records, reconciliation and reservation-release effects, backward-compatible
client-order-aware journal variants, and a binary-outcome inventory helper.
It also hardens repeated cancellation, IOC misses, late fills, and terminal
reconciliation. See [CHANGELOG.md](CHANGELOG.md) for details.

**v0.4.1** (2026-09-02): documentation-only release with a generic crate
description, complete v0.4 release notes, and application-neutral examples.
There are no functional or public API changes from v0.4.0.

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

## Synchronous broker adapters

omsrs is **ready to embed**. Write your venue adapter as `impl Broker for YourBroker` and every `Order::execute / modify / cancel` path works:

```rust
use std::collections::HashMap;
use omsrs::{Broker, Order, OrderInit};
use serde_json::Value;

pub struct VenueBroker { /* client, credentials, ... */ }

impl Broker for VenueBroker {
    fn order_place(&self, args: HashMap<String, Value>) -> Option<String> {
        // Map the request to an external API and return its order ID.
    }
    fn order_modify(&self, args: HashMap<String, Value>) { /* ... */ }
    fn order_cancel(&self, args: HashMap<String, Value>) { /* ... */ }
    // attribs_to_copy_execute / _modify / _cancel if your venue wants extra kwargs
}

let mut order = Order::from_init(OrderInit {
    symbol: "EXAMPLE".into(),
    side: "buy".into(),
    quantity: 10,
    order_type: Some("LIMIT".into()),
    price: Some(rust_decimal_macros::dec!(0.42)),
    ..Default::default()
});
let broker = VenueBroker::new(/* ... */);
order.execute(&broker, None, HashMap::new());
```

`CompoundOrder` and `OrderStrategy` give you basket / strategy-level aggregates (positions, MTM, net_value, average_buy/sell_price). `OrderLock` throttles modify/cancel with an injected `Clock`. Persistence goes through `PersistenceHandle` (SQLite reference impl behind the `persistence` feature).

## Async venue adapters (v0.2)

For an asynchronous client, implement `AsyncBroker`. It provides the same
method surface as `Broker` with asynchronous operations throughout:

```rust
use async_trait::async_trait;
use std::collections::HashMap;
use omsrs::AsyncBroker;
use serde_json::Value;

pub struct VenueBroker { /* async client, credentials, ... */ }

#[async_trait]
impl AsyncBroker for VenueBroker {
    async fn order_place(&self, args: HashMap<String, Value>) -> Option<String> {
        // .await into your async venue SDK directly — no block_on bridge
    }
    async fn order_modify(&self, args: HashMap<String, Value>) { /* ... */ }
    async fn order_cancel(&self, args: HashMap<String, Value>) { /* ... */ }
}
```

v0.2 ships `AsyncPaper` (async sibling of `Paper`, same echo semantics) and a 10-item async parity harness at `tests/parity_async.rs` that mirrors the sync R4 base tests 1:1. If `AsyncPaper` passes an assertion, sync `Paper` passes the same assertion with the same inputs.

### v0.3 async matching engines + order lifecycle

The quickstart below uses `#[tokio::main]` so the async APIs can `.await`.
Broker implementations may use another executor; the library's Tokio
dependency is limited to its asynchronous persistence helper:

```toml
[dependencies]
omsrs = "0.4"
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

```rust
use std::sync::Arc;
use omsrs::{AsyncVirtualBroker, AsyncCompoundOrder, AsyncOrderStrategy};
use omsrs::clock::MockClock;
use omsrs::simulation::Ticker;
use omsrs::order::OrderInit;
use std::collections::HashMap;
use chrono::Utc;

#[tokio::main]
async fn main() {
    // Multi-user virtual matching engine. Inherent `place` / `modify` /
    // `cancel` return the full `BrokerReply` (rich surface); the
    // `impl AsyncBroker` adapter collapses to `Option<String>` for
    // dyn-dispatch consumers.
    let mut tickers = HashMap::new();
    tickers.insert(
        "aapl".into(),
        Ticker::with_initial_price("aapl", 100.0),
    );
    let broker = Arc::new(
        AsyncVirtualBroker::with_clock(Arc::new(MockClock::new(Utc::now())))
            .with_tickers(tickers),
    );
    broker.set_failure_rate(0.0).unwrap();

    // Compose into CompoundOrder → OrderStrategy (basket / strategy
    // aggregates, same as sync).
    let mut compound = AsyncCompoundOrder::new()
        .with_broker(broker.clone());
    compound.add_order(
        OrderInit { symbol: "aapl".into(), side: "buy".into(), quantity: 10, ..Default::default() },
        None,
        None,
    ).unwrap();
    compound.execute_all_async(HashMap::new()).await;

    let mut strategy = AsyncOrderStrategy::new();
    strategy.add(compound);
    let positions = strategy.positions();  // sync — no I/O
    println!("{positions:?}");
}
```

Order lifecycle siblings (`execute_async` / `modify_async` / `cancel_async`) let existing patterns reach an async broker without a `block_on` bridge.

## Features

```toml
[dependencies]
omsrs = "0.4"

# persistence = ["dep:rusqlite"]  — off by default (§7 "MSRV-minimum build")
# statistical-tests — test-only, gates tests/statistical
```

### Feature flags

| Feature | Default | Enables |
| --- | --- | --- |
| `persistence` | off | SQLite-backed `PersistenceHandle` — pulls in `rusqlite` (bundled build). Off at MSRV-minimum build. |
| `statistical-tests` | off | `tests/statistical/*` — Z-mean / Z-std bounds on Ticker RNG path. Test-only. |

## Verification

- `scripts/parity_gate.sh` — release-mode full sweep, exits 0 on 237-item manifest
- `cargo test --test parity --all-features` — 237 / 236 pass / 1 excused, exit 0
- `cargo test --test parity_async` — 10-item async parity (v0.2 AsyncPaper), all pass
- `cargo test --test parity_runner_smoke` — 13-row exit-code smoke matrix
- `cargo test --no-default-features` — 222-item effective manifest, all pass
- `cargo test --test statistical --features statistical-tests --release` — Z-mean / Z-std bounds on Ticker RNG path
- `cargo clippy --all-features --all-targets -- -D warnings` — clean
- `cargo fmt --check` — clean

## Scope

**In MVP** (10 phases complete):
- `Order` lifecycle (R3): ~40 fields, update / execute / modify / cancel / save / clone / add_lock
- `Broker` trait (R3/R4): object-safe, `Paper` reference impl
- `AsyncBroker` trait + `AsyncPaper` (v0.2 / R11): additive async surface for async venue SDKs
- `PersistenceHandle` trait (R3): SQLite backend behind `persistence` feature
- Simulation (R5-R7): `VirtualBroker`, `ReplicaBroker`, `Ticker`, fill engine
- Aggregates (R8/R9): `CompoundOrder` / `OrderStrategy`

**Out of MVP** (per PORT-PLAN §11 non-goals):
- Venue-specific HTTP and WebSocket adapters
- `omspy.algos` / `omspy.multi` / candle-tracker (`Candle`, `CandleStick`) — defer
- `yaml` override loading — defer, test fixtures pre-apply renames
- Network clients and credential management

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
