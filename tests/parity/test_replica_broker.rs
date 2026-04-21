//! Parity ports of `tests/simulation/test_virtual.py` ReplicaBroker subset
//! (PORT-PLAN §8 R7 — 10 items).

use std::collections::HashMap;
use std::sync::Arc;

use omsrs::replica_broker::{OrderHandle, ReplicaBroker};
use omsrs::simulation::{Instrument, Status};
use serde_json::{json, Value};

fn kwargs(pairs: &[(&str, Value)]) -> HashMap<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect()
}

/// Upstream random.seed(1000) + `generate_instrument(name="AAPL"|"XOM"|"DOW")`
/// produces these last_prices in Python. We hardcode them to sidestep the
/// Python `random` byte-semantics parity (§14A analogue for R7 fixtures).
/// The R7 trials assert specific avg_prices that are derived from these
/// values, so they must stay pinned.
fn instrument(name: &str, last_price: f64) -> Instrument {
    Instrument {
        name: name.into(),
        token: None,
        last_price,
        open: last_price,
        high: last_price + 5.0,
        low: last_price - 5.0,
        close: last_price,
        volume: None,
        open_interest: None,
        strike: None,
        expiry: None,
        orderbook: None,
        last_update_time: None,
    }
}

fn replica_with_instruments() -> ReplicaBroker {
    let mut b = ReplicaBroker::new();
    b.update(vec![
        instrument("AAPL", 125.0),
        instrument("XOM", 153.0),
        instrument("DOW", 136.0),
    ]);
    b
}

/// Upstream `replica_with_orders` fixture — 10 orders placed in order.
fn replica_with_orders() -> (ReplicaBroker, Vec<OrderHandle>) {
    let mut b = replica_with_instruments();
    let inputs: [(&str, i64, f64, i64, f64, &str); 10] = [
        ("AAPL", 1, 10.0, 1, 124.0, "user1"),
        ("AAPL", -1, 10.0, 2, 126.0, "default"),
        ("AAPL", 1, 20.0, 2, 124.0, "user2"),
        ("DOW", -1, 13.0, 1, 124.0, "user1"),
        ("DOW", 1, 18.0, 1, 124.0, "user2"),
        ("XOM", 1, 20.0, 2, 154.0, "user2"),
        ("XOM", 1, 30.0, 2, 135.0, "default"),
        ("XOM", -1, 30.0, 2, 140.0, "default"),
        ("AAPL", 1, 10.0, 2, 123.0, "default"),
        ("AAPL", 1, 10.0, 2, 122.0, "default"),
    ];
    let mut handles = Vec::new();
    for (sym, side, qty, ot, price, user) in inputs {
        let h = b.order_place(kwargs(&[
            ("symbol", json!(sym)),
            ("side", json!(side)),
            ("quantity", json!(qty)),
            ("order_type", json!(ot)),
            ("price", json!(price)),
            ("user", json!(user)),
        ]));
        handles.push(h);
    }
    (b, handles)
}

// ── R7 trials ───────────────────────────────────────────────────────────

pub fn test_replica_broker_defaults() {
    let b = ReplicaBroker::new();
    assert_eq!(b.name, "replica");
    assert!(b.instruments.is_empty());
    assert!(b.orders.is_empty());
    assert_eq!(b.users.len(), 1);
    assert!(b.users.contains("default"));
    assert!(b.user_orders.is_empty());
    assert!(b.pending.is_empty());
    assert!(b.completed.is_empty());
    assert!(b.fills.is_empty());
}

pub fn test_replica_broker_update() {
    let mut b = ReplicaBroker::new();
    let names = ["AAPL", "XOM", "DOW"];
    let instruments: Vec<Instrument> = names
        .iter()
        .map(|n| instrument(n, 120.0))
        .collect();
    b.update(instruments);
    for name in names {
        assert!(b.instruments.contains_key(name));
    }
    // Update existing instrument
    let mut updated = b.instruments["AAPL"].clone();
    updated.last_price = 144.0;
    b.update(vec![updated]);
    assert_eq!(b.instruments["AAPL"].last_price, 144.0);
}

pub fn test_replica_broker_order_place() {
    let mut b = replica_with_instruments();
    let order = b.order_place(kwargs(&[
        ("symbol", json!("AAPL")),
        ("side", json!(1)),
        ("quantity", json!(10)),
    ]));
    let order_id = order.lock().order_id.clone();
    assert!(b.orders.contains_key(&order_id));
    assert_eq!(b.user_orders["default"].len(), 1);
    assert!(!order.lock().is_done());
    assert_eq!(b.pending.len(), 1);
    assert_eq!(b.fills.len(), 1);
    assert_eq!(b.pending[0].lock().order_id, order_id);

    // Object identity across all 5 collections — upstream's `id(order) ==
    // id(broker.orders[...]) == ...` assertion becomes Arc::ptr_eq on
    // our shared-handle model.
    assert!(Arc::ptr_eq(&order, &b.orders[&order_id]));
    assert!(Arc::ptr_eq(&order, &b.user_orders["default"][0]));
    assert!(Arc::ptr_eq(&order, &b.pending[0]));
    assert!(Arc::ptr_eq(&order, &b.fills[0].order));
}

pub fn test_replica_broker_order_place_multiple_users() {
    let mut b = replica_with_instruments();
    b.order_place(kwargs(&[
        ("symbol", json!("AAPL")),
        ("side", json!(1)),
        ("quantity", json!(10)),
    ]));
    for user in ["user1", "user2", "default"] {
        b.order_place(kwargs(&[
            ("symbol", json!("AAPL")),
            ("side", json!(1)),
            ("quantity", json!(10)),
            ("user", json!(user)),
        ]));
    }
    b.order_place(kwargs(&[
        ("symbol", json!("AAPL")),
        ("side", json!(1)),
        ("quantity", json!(10)),
    ]));
    assert_eq!(b.orders.len(), 5);
    assert_eq!(b.user_orders.len(), 3);
    for (k, v) in &b.user_orders {
        if k == "default" {
            assert_eq!(v.len(), 3);
        } else {
            assert_eq!(v.len(), 1);
        }
    }
}

pub fn test_replica_order_fill() {
    let (mut b, handles) = replica_with_orders();
    assert_eq!(b.orders.len(), 10);
    assert_eq!(b.completed.len(), 0);
    assert_eq!(b.user_orders.len(), 3);

    // Upstream expected map: fills at indices (0, 3, 4, 5, 7) fill with
    // avg_prices (125, 136, 136, 153, 153). Indices 1,2,6,8,9 don't fill.
    let expected_avgs: HashMap<String, f64> = [
        (b.fills[0].order.lock().order_id.clone(), 125.0),
        (b.fills[3].order.lock().order_id.clone(), 136.0),
        (b.fills[4].order.lock().order_id.clone(), 136.0),
        (b.fills[5].order.lock().order_id.clone(), 153.0),
        (b.fills[7].order.lock().order_id.clone(), 153.0),
    ]
    .into_iter()
    .collect();

    b.run_fill();
    // After first run_fill, upstream: completed == fills == 5 (fills
    // filtered to drop done entries).
    assert_eq!(b.completed.len(), 5);
    assert_eq!(b.fills.len(), 5);
    for handle in &b.completed {
        let o = handle.lock();
        let expected = expected_avgs[&o.order_id];
        assert_eq!(o.average_price, Some(expected));
    }

    // AAPL SELL LIMIT 10 @ 126 (handles[1]) should fill at avg=126 after
    // AAPL last_price bumps to 127.
    b.instruments.get_mut("AAPL").unwrap().last_price = 127.0;
    b.run_fill();
    assert_eq!(b.fills.len(), 4);
    assert_eq!(b.completed.len(), 6);
    let o = handles[1].lock();
    assert_eq!(o.average_price, Some(126.0));
    assert_eq!(o.filled_quantity, 10.0);
    drop(o);

    // Idempotent — repeated run_fill without price changes doesn't alter.
    for _ in 0..10 {
        b.run_fill();
    }
    assert_eq!(b.fills.len(), 4);
    assert_eq!(b.completed.len(), 6);

    // AAPL last_price = 121.95 triggers AAPL BUY LIMIT @ 123 and 122.
    // Plus the AAPL BUY LIMIT 20 @ 124 (handles[2]). 3 more fills.
    b.instruments.get_mut("AAPL").unwrap().last_price = 121.95;
    b.run_fill();
    assert_eq!(b.fills.len(), 1);
    assert_eq!(b.completed.len(), 9);
    let id6 = handles[6].lock().order_id.clone();
    assert!(b.orders.contains_key(&id6));
}

pub fn test_replica_broker_order_modify() {
    let mut b = replica_with_instruments();
    let order = b.order_place(kwargs(&[
        ("symbol", json!("AAPL")),
        ("side", json!(1)),
        ("quantity", json!(10)),
        ("order_type", json!(2)),
        ("price", json!(124)),
    ]));
    let order_id = order.lock().order_id.clone();
    b.run_fill();
    assert!(!order.lock().is_done());
    b.order_modify(
        &order_id,
        kwargs(&[("quantity", json!(20)), ("price", json!(125.1))]),
    );
    assert_eq!(b.orders[&order_id].lock().quantity, 20.0);
    assert_eq!(b.orders[&order_id].lock().price, Some(125.1));
    b.run_fill();
    let o = order.lock();
    assert_eq!(o.filled_quantity, 20.0);
    assert_eq!(o.average_price, Some(125.1));
    assert!(o.is_done());
}

pub fn test_replica_broker_order_modify_market() {
    let mut b = replica_with_instruments();
    let order = b.order_place(kwargs(&[
        ("symbol", json!("AAPL")),
        ("side", json!(1)),
        ("quantity", json!(10)),
        ("order_type", json!(2)),
        ("price", json!(124)),
    ]));
    let order_id = order.lock().order_id.clone();
    b.run_fill();
    assert!(!order.lock().is_done());
    b.order_modify(&order_id, kwargs(&[("quantity", json!(20))]));
    b.run_fill();
    assert!(!order.lock().is_done());
    b.order_modify(&order_id, kwargs(&[("order_type", json!(1))]));
    b.run_fill();
    let o = order.lock();
    assert_eq!(o.filled_quantity, 20.0);
    assert_eq!(o.average_price, Some(125.0));
    assert!(o.is_done());
    drop(o);
    assert!(Arc::ptr_eq(&order, &b.orders[&order_id]));
    assert!(Arc::ptr_eq(&order, &b.user_orders["default"][0]));
    assert_eq!(b.completed.len(), 1);
}

pub fn test_replica_broker_order_cancel() {
    let mut b = replica_with_instruments();
    let order = b.order_place(kwargs(&[
        ("symbol", json!("AAPL")),
        ("side", json!(1)),
        ("quantity", json!(10)),
        ("order_type", json!(2)),
        ("price", json!(124)),
    ]));
    let order_id = order.lock().order_id.clone();
    b.run_fill();
    assert!(!order.lock().is_done());
    b.order_cancel(&order_id);
    assert_eq!(b.completed.len(), 1);
    assert!(order.lock().is_done());
    assert_eq!(b.fills.len(), 1);
    b.run_fill();
    assert_eq!(b.fills.len(), 0);
}

pub fn test_replica_broker_order_cancel_multiple_times() {
    let mut b = replica_with_instruments();
    let order = b.order_place(kwargs(&[
        ("symbol", json!("AAPL")),
        ("side", json!(1)),
        ("quantity", json!(10)),
        ("order_type", json!(2)),
        ("price", json!(124)),
    ]));
    let order_id = order.lock().order_id.clone();
    b.run_fill();
    assert!(!order.lock().is_done());
    for _ in 0..10 {
        b.order_cancel(&order_id);
    }
    assert_eq!(b.completed.len(), 1);
    assert!(order.lock().is_done());
    assert_eq!(b.fills.len(), 1);
    b.run_fill();
    assert_eq!(b.fills.len(), 0);
}

pub fn test_replica_broker_no_symbol() {
    let mut b = replica_with_instruments();
    let order1 = b.order_place(kwargs(&[
        ("symbol", json!("AAPL")),
        ("side", json!(1)),
        ("quantity", json!(10)),
    ]));
    assert_eq!(b.fills.len(), 1);
    let o1 = order1.lock();
    assert_eq!(o1.pending_quantity, 10.0);
    assert_eq!(o1.quantity, o1.pending_quantity);
    assert_eq!(o1.status(), Status::Open);
    assert!(!o1.is_done());
    drop(o1);

    let order2 = b.order_place(kwargs(&[
        ("symbol", json!("yinyang")),
        ("side", json!(1)),
        ("quantity", json!(10)),
    ]));
    assert_eq!(b.fills.len(), 1);
    assert_eq!(b.pending.len(), 1);
    assert_eq!(b.completed.len(), 1);
    let o2 = order2.lock();
    assert!(!o2.is_complete());
    assert!(o2.is_done());
    assert_eq!(o2.filled_quantity, 0.0);
    assert_eq!(o2.pending_quantity, 0.0);
    assert_eq!(o2.canceled_quantity, 10.0);
    assert_eq!(o2.status(), Status::Rejected);
}
