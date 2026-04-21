//! Parity ports of `tests/simulation/test_virtual.py` VirtualBroker subset
//! (PORT-PLAN §8 R6 — 22 items, multi-user inclusive).

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use omsrs::clock::{Clock, MockClock};
use omsrs::simulation::{ResponseStatus, Side, Status, Ticker, VUser};
use omsrs::virtual_broker::VirtualBroker;
use serde_json::{json, Value};

fn kwargs(pairs: &[(&str, Value)]) -> HashMap<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect()
}

fn basic_tickers() -> HashMap<String, Ticker> {
    let mut m = HashMap::new();
    m.insert(
        "aapl".into(),
        Ticker::with_initial_price("aapl", 100.0).with_token(1111),
    );
    m.insert(
        "goog".into(),
        Ticker::with_initial_price("goog", 125.0).with_token(2222),
    );
    m.insert(
        "amzn".into(),
        Ticker::with_initial_price("amzn", 260.0).with_token(3333),
    );
    m
}

fn basic_broker() -> VirtualBroker {
    let clock: Arc<dyn Clock + Send + Sync> = Arc::new(MockClock::new(
        Utc.with_ymd_and_hms(2023, 2, 1, 10, 17, 0).unwrap(),
    ));
    VirtualBroker::with_clock(clock).with_tickers(basic_tickers())
}

fn basic_broker_with_users() -> VirtualBroker {
    let mut b = basic_broker();
    // Ensure order_place reliably succeeds by zeroing the failure rate up-front.
    b.set_failure_rate(0.0).unwrap();
    b.add_user(VUser::new("abcd1234"));
    b.add_user(VUser::new("xyz456"));
    b.add_user(VUser::new("bond007"));
    b
}

fn basic_broker_with_prices() -> VirtualBroker {
    let mut b = basic_broker();
    let prices = [
        ("aapl", 105.0),
        ("goog", 121.0),
        ("amzn", 264.0),
        ("aapl", 102.0),
        ("goog", 124.0),
        ("amzn", 258.0),
        ("aapl", 99.0),
        ("goog", 120.0),
        ("amzn", 260.0),
        ("aapl", 106.0),
        ("goog", 122.0),
        ("amzn", 259.0),
        ("aapl", 103.0),
        ("goog", 123.0),
        ("amzn", 261.0),
    ];
    for chunk in prices.chunks(3) {
        let map: HashMap<String, f64> = chunk.iter().map(|(k, v)| ((*k).to_string(), *v)).collect();
        b.update_tickers(&map);
    }
    b
}

// ── R6 trials ───────────────────────────────────────────────────────────

pub fn test_virtual_broker_defaults() {
    let b = basic_broker();
    assert_eq!(b.name, "VBroker");
    assert_eq!(b.tickers.len(), 3);
    assert_eq!(b.failure_rate(), 0.001);
}

pub fn test_virtual_broker_is_failure() {
    let mut b = basic_broker();
    assert!(!b.is_failure());
    b.set_failure_rate(1.0).unwrap();
    assert!(b.is_failure());
    assert!(b.set_failure_rate(-1.0).is_err());
    assert!(b.set_failure_rate(2.0).is_err());
}

pub fn test_virtual_broker_order_place_success() {
    let mut b = basic_broker();
    b.set_failure_rate(0.0).unwrap();
    let known = Utc.with_ymd_and_hms(2023, 2, 1, 10, 17, 0).unwrap();
    let reply = b.order_place(kwargs(&[
        ("symbol", json!("aapl")),
        ("quantity", json!(10)),
        ("side", json!(1)),
    ]));
    let resp = reply.as_order().unwrap();
    assert_eq!(resp.status, ResponseStatus::Success);
    assert_eq!(resp.timestamp, Some(known));
    assert!(!resp.data.as_ref().unwrap().order_id.is_empty());
    assert_eq!(b.orders().len(), 1);
}

pub fn test_virtual_broker_order_place_success_fields() {
    let mut b = basic_broker();
    b.set_failure_rate(0.0).unwrap();
    let known = Utc.with_ymd_and_hms(2023, 2, 1, 10, 17, 0).unwrap();
    let reply = b.order_place(kwargs(&[
        ("symbol", json!("aapl")),
        ("quantity", json!(10)),
        ("side", json!(1)),
        ("price", json!(100)),
        ("trigger_price", json!(99)),
    ]));
    let resp = reply.as_order().unwrap();
    assert_eq!(resp.status, ResponseStatus::Success);
    assert_eq!(resp.timestamp, Some(known));
    let d = resp.data.as_ref().unwrap();
    assert_eq!(d.price, Some(100.0));
    assert_eq!(d.trigger_price, Some(99.0));
    assert_eq!(d.symbol, "aapl");
    assert_eq!(d.quantity, 10.0);
    assert_eq!(d.side, Side::Buy);
    assert_eq!(d.filled_quantity, 0.0);
    assert_eq!(d.canceled_quantity, 0.0);
    assert_eq!(d.pending_quantity, 10.0);
    assert_eq!(d.status(), Status::Open);
}

pub fn test_virtual_broker_order_place_failure() {
    let mut b = basic_broker();
    b.set_failure_rate(1.0).unwrap();
    let known = Utc.with_ymd_and_hms(2023, 2, 1, 10, 17, 0).unwrap();
    let reply = b.order_place(kwargs(&[
        ("symbol", json!("aapl")),
        ("quantity", json!(10)),
        ("side", json!(1)),
        ("price", json!(100)),
    ]));
    let resp = reply.as_order().unwrap();
    assert_eq!(resp.status, ResponseStatus::Failure);
    assert_eq!(resp.timestamp, Some(known));
    assert!(resp.data.is_none());
}

pub fn test_virtual_broker_order_place_user_response() {
    let mut b = basic_broker();
    b.set_failure_rate(1.0).unwrap();
    let reply = b.order_place(kwargs(&[(
        "response",
        json!({"symbol": "aapl", "price": 100}),
    )]));
    let pass = reply.as_passthrough().unwrap();
    assert_eq!(pass, &json!({"symbol": "aapl", "price": 100}));
}

pub fn test_virtual_broker_order_place_validation_error() {
    let mut b = basic_broker();
    b.set_failure_rate(0.0).unwrap();
    // All 3 missing.
    let reply = b.order_place(HashMap::new());
    let resp = reply.as_order().unwrap();
    assert_eq!(resp.status, ResponseStatus::Failure);
    assert!(resp
        .error_msg
        .as_ref()
        .unwrap()
        .starts_with("Found 3 validation"));
    assert!(resp.data.is_none());

    // Only quantity missing.
    let reply = b.order_place(kwargs(&[("symbol", json!("aapl")), ("side", json!(-1))]));
    let resp = reply.as_order().unwrap();
    assert_eq!(resp.status, ResponseStatus::Failure);
    let msg = resp.error_msg.as_ref().unwrap();
    assert!(msg.starts_with("Found 1 validation"));
    assert!(msg.contains("quantity"));
    assert!(resp.data.is_none());
}

pub fn test_virtual_broker_get() {
    let mut b = basic_broker();
    b.set_failure_rate(0.0).unwrap();
    let mut ids: Vec<String> = Vec::new();
    for q in [50_i64, 100, 130] {
        let reply = b.order_place(kwargs(&[
            ("symbol", json!("dow")),
            ("side", json!(1)),
            ("quantity", json!(q)),
        ]));
        ids.push(
            reply
                .as_order()
                .unwrap()
                .data
                .as_ref()
                .unwrap()
                .order_id
                .clone(),
        );
    }
    assert_eq!(b.orders().len(), 3);
    let target = ids[1].clone();
    let order = b.get_default(&target).unwrap();
    assert_eq!(order.order_id, target);
}

pub fn test_virtual_broker_order_modify() {
    let mut b = basic_broker();
    b.set_failure_rate(0.0).unwrap();
    let reply = b.order_place(kwargs(&[
        ("symbol", json!("dow")),
        ("side", json!(1)),
        ("quantity", json!(50)),
    ]));
    let order_id = reply
        .as_order()
        .unwrap()
        .data
        .as_ref()
        .unwrap()
        .order_id
        .clone();

    let resp = b.order_modify(&order_id, kwargs(&[("quantity", json!(25))]));
    assert_eq!(resp.as_order().unwrap().status, ResponseStatus::Success);
    assert_eq!(
        resp.as_order().unwrap().data.as_ref().unwrap().quantity,
        25.0
    );

    let resp = b.order_modify(&order_id, kwargs(&[("price", json!(1000))]));
    assert_eq!(resp.as_order().unwrap().status, ResponseStatus::Success);
    assert_eq!(
        resp.as_order().unwrap().data.as_ref().unwrap().price,
        Some(1000.0)
    );
    assert_eq!(b.orders().get(&order_id).unwrap().price, Some(1000.0));
}

pub fn test_virtual_broker_order_modify_failure() {
    let mut b = basic_broker();
    b.set_failure_rate(0.0).unwrap();
    let reply = b.order_place(kwargs(&[
        ("symbol", json!("dow")),
        ("side", json!(1)),
        ("quantity", json!(50)),
    ]));
    let order_id = reply
        .as_order()
        .unwrap()
        .data
        .as_ref()
        .unwrap()
        .order_id
        .clone();

    let resp = b.order_modify("hexid", kwargs(&[("quantity", json!(25))]));
    let r = resp.as_order().unwrap();
    assert_eq!(r.status, ResponseStatus::Failure);
    assert!(r.data.is_none());

    b.set_failure_rate(1.0).unwrap();
    let resp = b.order_modify(&order_id, kwargs(&[("price", json!(100))]));
    let r = resp.as_order().unwrap();
    assert_eq!(r.status, ResponseStatus::Failure);
    assert!(r.data.is_none());
}

pub fn test_virtual_broker_order_modify_kwargs_response() {
    let mut b = basic_broker();
    let resp = b.order_modify(
        "hexid",
        kwargs(&[
            ("quantity", json!(25)),
            ("response", json!({"a": 10, "b": 15})),
        ]),
    );
    assert_eq!(resp.as_passthrough().unwrap(), &json!({"a": 10, "b": 15}));
}

pub fn test_virtual_broker_order_cancel() {
    let mut b = basic_broker();
    b.set_failure_rate(0.0).unwrap();
    let reply = b.order_place(kwargs(&[
        ("symbol", json!("dow")),
        ("side", json!(1)),
        ("quantity", json!(50)),
    ]));
    let order_id = reply
        .as_order()
        .unwrap()
        .data
        .as_ref()
        .unwrap()
        .order_id
        .clone();

    let resp = b.order_cancel(&order_id, HashMap::new());
    let r = resp.as_order().unwrap();
    assert_eq!(r.status, ResponseStatus::Success);
    let d = r.data.as_ref().unwrap();
    assert_eq!(d.canceled_quantity, 50.0);
    assert_eq!(d.filled_quantity, 0.0);
    assert_eq!(d.pending_quantity, 0.0);
    assert_eq!(d.status(), Status::Canceled);
}

pub fn test_virtual_broker_order_cancel_failure() {
    let mut b = basic_broker();
    b.set_failure_rate(0.0).unwrap();
    let reply = b.order_place(kwargs(&[
        ("symbol", json!("dow")),
        ("side", json!(1)),
        ("quantity", json!(50)),
    ]));
    let order_id = reply
        .as_order()
        .unwrap()
        .data
        .as_ref()
        .unwrap()
        .order_id
        .clone();

    let resp = b.order_modify("hexid", kwargs(&[("quantity", json!(25))]));
    let r = resp.as_order().unwrap();
    assert_eq!(r.status, ResponseStatus::Failure);
    assert!(r.data.is_none());

    // Upstream then mutates the fetched order to filled_quantity=50 and
    // re-asserts resp.status == "failure" — the resp variable still points
    // at the earlier failure, the mutation is a non-observable side
    // effect. We just smoke the same order-fetch path.
    if let Some(order) = b.orders_mut().get_mut(&order_id) {
        order.filled_quantity = 50.0;
    }
    assert_eq!(r.status, ResponseStatus::Failure);
}

pub fn test_virtual_broker_order_cancel_kwargs_response() {
    let mut b = basic_broker();
    let resp = b.order_cancel(
        "hexid",
        kwargs(&[
            ("quantity", json!(25)),
            ("response", json!({"a": 10, "b": 15})),
        ]),
    );
    assert_eq!(resp.as_passthrough().unwrap(), &json!({"a": 10, "b": 15}));
}

pub fn test_virtual_broker_add_user() {
    let clock: Arc<dyn Clock + Send + Sync> = Arc::new(MockClock::new(Utc::now()));
    let mut b = VirtualBroker::with_clock(clock);
    assert_eq!(b.users.len(), 0);
    assert_eq!(b.clients().len(), 0);
    assert!(b.add_user(VUser::new("abcd1234")));
    assert!(b.add_user(VUser::new("xyz456")));
    assert_eq!(b.users.len(), 2);
    assert_eq!(b.clients().len(), 2);
    assert!(!b.add_user(VUser::new("abcd1234")));
    assert_eq!(b.users.len(), 2);
    assert_eq!(b.clients().len(), 2);
}

pub fn test_virtual_broker_order_place_users() {
    let mut b = basic_broker_with_users();
    b.order_place(kwargs(&[
        ("symbol", json!("aapl")),
        ("quantity", json!(10)),
        ("side", json!(1)),
    ]));
    b.order_place(kwargs(&[
        ("symbol", json!("goog")),
        ("quantity", json!(10)),
        ("side", json!(1)),
    ]));
    let clients: Vec<String> = b.clients().iter().cloned().collect();
    for c in &clients {
        b.order_place(kwargs(&[
            ("symbol", json!("aapl")),
            ("quantity", json!(20)),
            ("side", json!(-1)),
            ("userid", json!(c.clone())),
        ]));
    }
    b.order_place(kwargs(&[
        ("symbol", json!("goog")),
        ("quantity", json!(10)),
        ("side", json!(1)),
        ("userid", json!("unknown")),
    ]));
    assert_eq!(b.orders().len(), 6);
    for u in &b.users {
        assert_eq!(u.orders.len(), 1);
    }
    assert_eq!(b.clients().len(), 3);
    assert_eq!(b.users.len(), 3);
}

pub fn test_virtual_broker_order_place_same_memory() {
    let mut b = basic_broker_with_users();
    b.order_place(kwargs(&[
        ("symbol", json!("aapl")),
        ("quantity", json!(10)),
        ("side", json!(1)),
    ]));
    b.order_place(kwargs(&[
        ("symbol", json!("goog")),
        ("quantity", json!(10)),
        ("side", json!(1)),
    ]));
    let clients: Vec<String> = b.clients().iter().cloned().collect();
    for c in &clients {
        b.order_place(kwargs(&[
            ("symbol", json!("aapl")),
            ("quantity", json!(20)),
            ("side", json!(-1)),
            ("userid", json!(c.clone())),
        ]));
    }
    assert_eq!(b.orders().len(), 5);
    // Upstream asserts Python object identity via `id()` — our "weak
    // clone" bridge returns a fresh VOrder with the same `order_id`,
    // so we assert id-string equality instead of pointer equality.
    // The VirtualBroker keeps the canonical copy in `orders`; the user
    // list mirrors by order_id.
    for i in 0..3 {
        let order = &b.users[i].orders[0];
        let canonical = b.orders().get(&order.order_id).unwrap();
        assert_eq!(canonical.order_id, order.order_id);
    }
}

pub fn test_virtual_broker_order_place_delay() {
    let mut b = basic_broker_with_users();
    b.order_place(kwargs(&[
        ("symbol", json!("aapl")),
        ("quantity", json!(10)),
        ("side", json!(1)),
    ]));
    b.order_place(kwargs(&[
        ("symbol", json!("goog")),
        ("quantity", json!(10)),
        ("side", json!(1)),
        ("delay", json!(5_000_000)),
    ]));
    // Find orders by symbol and check delays.
    let mut delays_by_symbol = HashMap::new();
    for order in b.orders().values() {
        delays_by_symbol.insert(
            order.symbol.clone(),
            order.delay.num_microseconds().unwrap(),
        );
    }
    assert_eq!(delays_by_symbol.get("aapl"), Some(&1_000_000));
    assert_eq!(delays_by_symbol.get("goog"), Some(&5_000_000));
}

pub fn test_virtual_broker_get_order_by_status() {
    let clock_handle = MockClock::new(Utc.with_ymd_and_hms(2023, 2, 1, 10, 17, 0).unwrap());
    let clock: Arc<dyn Clock + Send + Sync> = Arc::new(clock_handle.clone());
    let mut b = VirtualBroker::with_clock(clock).with_tickers(basic_tickers());
    b.set_failure_rate(0.0).unwrap();

    let known = Utc.with_ymd_and_hms(2023, 2, 1, 10, 17, 0).unwrap();
    clock_handle.set(known);
    let reply = b.order_place(kwargs(&[
        ("symbol", json!("aapl")),
        ("quantity", json!(10)),
        ("side", json!(1)),
    ]));
    let order_id = reply
        .as_order()
        .unwrap()
        .data
        .as_ref()
        .unwrap()
        .order_id
        .clone();
    {
        let order = b.get_default(&order_id).unwrap();
        assert_eq!(order.pending_quantity, 10.0);
    }

    clock_handle.set(known + Duration::seconds(2));
    {
        let order = b.get_default(&order_id).unwrap();
        assert_eq!(order.status(), Status::Complete);
        assert_eq!(order.filled_quantity, 10.0);
    }

    clock_handle.set(known + Duration::seconds(3));
    {
        let order = b.get(&order_id, Status::Canceled).unwrap();
        assert_eq!(order.status(), Status::Complete);
    }

    // Second order → Canceled path.
    clock_handle.set(known);
    let reply = b.order_place(kwargs(&[
        ("symbol", json!("goog")),
        ("quantity", json!(10)),
        ("side", json!(1)),
    ]));
    let order_id = reply
        .as_order()
        .unwrap()
        .data
        .as_ref()
        .unwrap()
        .order_id
        .clone();

    clock_handle.set(known + Duration::seconds(3));
    let order = b.get(&order_id, Status::Canceled).unwrap();
    assert_eq!(order.status(), Status::Canceled);
    assert_eq!(order.filled_quantity, 0.0);
    assert_eq!(order.canceled_quantity, 10.0);
}

pub fn test_virtual_broker_update_ticker() {
    let b = basic_broker_with_prices();
    assert_eq!(b.tickers["aapl"].ohlc().high, 106.0);
    assert_eq!(b.tickers["goog"].ohlc().low, 120.0);
    assert_eq!(b.tickers["amzn"].ohlc().close, 261.0);
    let ohlc = b.tickers["aapl"].ohlc();
    assert_eq!(ohlc.open, 100.0);
    assert_eq!(ohlc.high, 106.0);
    assert_eq!(ohlc.low, 99.0);
    assert_eq!(ohlc.close, 103.0);
    assert_eq!(ohlc.last_price, 103.0);
}

pub fn test_virtual_broker_ltp() {
    // Second (shadowing) definition — upstream pytest collects only this
    // one. Covers `b.ltp("dow") is None` + multi-symbol fallback.
    let b = basic_broker_with_prices();
    assert!(b.ltp("dow").is_none());
    let multi = b.ltp_many(&["goog", "amzn", "dow", "aa"]);
    assert_eq!(multi.len(), 2);
}

pub fn test_virtual_broker_ohlc() {
    let b = basic_broker_with_prices();
    let result = b.ohlc("aapl").unwrap();
    assert_eq!(result.len(), 1);
    let o = &result["aapl"];
    let direct = b.tickers["aapl"].ohlc();
    assert_eq!(o.open, direct.open);
    assert_eq!(o.high, direct.high);
    assert_eq!(o.low, direct.low);
    assert_eq!(o.close, direct.close);
}
