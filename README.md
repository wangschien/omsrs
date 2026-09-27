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

## Release notes

- v0.5.1 — typed close submission on the transaction path.
- v0.5.0 — I/O-free execution transactions, market inventory, and venue observation.
- v0.4.1 — documentation only; no functional change.
- v0.4.0 — per-order lifecycle state machine, replayable journal, and binary-outcome inventory.
- v0.3.3 — Rust 1.78 build compatibility.
- v0.3.2 — typed asynchronous cancel outcomes and the cancel-versus-fill report.
- v0.3.1 — duplicate-completion guard, non-blocking persistence, and monotonic fills.
- v0.3.0 — asynchronous virtual and replica brokers, and asynchronous order lifecycle.
- v0.2.0 — asynchronous broker trait and asynchronous paper broker.
- v0.1.0 — initial order, broker, simulation, and persistence primitives.

See [CHANGELOG.md](CHANGELOG.md).

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

v0.2 ships `AsyncPaper` (async sibling of `Paper`, same echo semantics).

### v0.3 async matching engines + order lifecycle

The quickstart below uses `#[tokio::main]` so the async APIs can `.await`.
Broker implementations may use another executor; the library's Tokio
dependency is limited to its asynchronous persistence helper:

```toml
[dependencies]
omsrs = "0.5"
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
omsrs = "0.5"

# persistence = ["dep:rusqlite"]  — off by default
# statistical-tests — test-only, gates tests/statistical
```

### Feature flags

| Feature | Default | Enables |
| --- | --- | --- |
| `persistence` | off | SQLite-backed `PersistenceHandle` — pulls in `rusqlite` (bundled build). Off unless the feature is enabled. |
| `statistical-tests` | off | `tests/statistical/*` — Z-mean / Z-std bounds on the ticker RNG path. Test-only. |

## License

MIT. Matches upstream omspy.

## Upstream reference

- omspy (upstream): https://github.com/uberdeveloper/omspy
