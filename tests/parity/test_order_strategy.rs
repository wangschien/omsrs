//! Parity ports of `tests/test_order_strategy.py` (PORT-PLAN §8 R9 —
//! 7 items).

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{TimeZone, Utc};
use omsrs::broker::Broker;
use omsrs::clock::{Clock, MockClock};
use omsrs::compound_order::CompoundOrder;
use omsrs::order::{Order, OrderInit};
use omsrs::order_strategy::OrderStrategy;
use parking_lot::Mutex;
use rust_decimal_macros::dec;
use serde_json::{json, Value};

use crate::mock_broker::MockBroker;

fn default_clock() -> Arc<dyn Clock + Send + Sync> {
    Arc::new(MockClock::new(Utc.with_ymd_and_hms(2023, 1, 1, 10, 0, 0).unwrap()))
}

/// Build a fresh `MockBroker` whose `order_place` returns the sequence
/// `100000..` (upstream `broker.order_place.side_effect = range(100000, 100100)`).
fn sequential_mock_broker() -> (Arc<MockBroker>, Arc<dyn Broker>) {
    let m = Arc::new(MockBroker::new());
    m.set_place_side_effect((100000..100100).map(|i| Some(i.to_string())).collect());
    let as_broker: Arc<dyn Broker> = m.clone();
    (m, as_broker)
}

fn simple_strategy() -> OrderStrategy {
    let (_mock, broker) = sequential_mock_broker();
    OrderStrategy::new().with_broker(broker)
}

fn strategy_with_orders() -> OrderStrategy {
    let symbols = ["aapl", "goog", "dow", "amzn"];
    let prices = [dec!(100), dec!(102), dec!(105), dec!(110)];
    let quantities = [10_i64, 20, 30, 40];
    let clock = default_clock();
    let (_mock, broker) = sequential_mock_broker();

    let mut orders: Vec<Order> = Vec::new();
    for i in 0..4 {
        let mut o = Order::from_init_with_clock(
            OrderInit {
                symbol: symbols[i].into(),
                side: "buy".into(),
                quantity: quantities[i],
                price: Some(prices[i]),
                ..Default::default()
            },
            clock.clone(),
        );
        o.average_price = prices[i];
        o.filled_quantity = quantities[i] - 1;
        orders.push(o);
    }

    let mut com1 = CompoundOrder::with_clock(clock.clone());
    com1.broker = Some(broker.clone());
    com1.add(orders.remove(0), None, None).unwrap();
    com1.add(orders.remove(0), None, None).unwrap();
    com1.execute_all(HashMap::new());

    let mut com2 = CompoundOrder::with_clock(clock);
    com2.broker = Some(broker.clone());
    com2.add(orders.remove(0), None, None).unwrap();
    com2.add(orders.remove(0), None, None).unwrap();
    com2.execute_all(HashMap::new());

    OrderStrategy::new()
        .with_broker(broker)
        .with_orders(vec![com1, com2])
}

// ── R9 trials ───────────────────────────────────────────────────────────

pub fn test_order_strategy_defaults() {
    let s = simple_strategy();
    assert!(s.orders.is_empty());
}

pub fn test_order_strategy_positions() {
    let s = strategy_with_orders();
    let p = s.positions();
    assert_eq!(p.get("aapl"), Some(&9));
    assert_eq!(p.get("goog"), Some(&19));
    assert_eq!(p.get("dow"), Some(&29));
    assert_eq!(p.get("amzn"), Some(&39));
}

pub fn test_order_strategy_update_ltp() {
    let mut s = strategy_with_orders();
    assert!(s.ltp.is_empty());
    let mut m = HashMap::new();
    m.insert("aapl".to_string(), 120.0);
    s.update_ltp(&m);
    assert_eq!(s.ltp.get("aapl"), Some(&120.0));
    let mut m = HashMap::new();
    m.insert("goog".to_string(), 100.0);
    m.insert("amzn".to_string(), 110.0);
    s.update_ltp(&m);
    assert_eq!(s.ltp.get("aapl"), Some(&120.0));
    assert_eq!(s.ltp.get("goog"), Some(&100.0));
    assert_eq!(s.ltp.get("amzn"), Some(&110.0));
}

pub fn test_order_strategy_update_orders() {
    let mut s = strategy_with_orders();
    assert!(s.orders[0].orders[0].exchange_order_id.is_none());
    let mut data: HashMap<String, HashMap<String, Value>> = HashMap::new();
    let mut inner1 = HashMap::new();
    inner1.insert("exchange_order_id".to_string(), json!("11111"));
    data.insert("100000".into(), inner1);
    let mut inner2 = HashMap::new();
    inner2.insert("exchange_order_id".to_string(), json!("11112"));
    data.insert("100003".into(), inner2);
    s.update_orders(&data);
    assert_eq!(
        s.orders[0].orders[0].exchange_order_id.as_deref(),
        Some("11111")
    );
    assert_eq!(
        s.orders[1].orders[1].exchange_order_id.as_deref(),
        Some("11112")
    );
}

pub fn test_order_strategy_mtm() {
    let mut s = strategy_with_orders();
    let mut m = HashMap::new();
    m.insert("goog".to_string(), 100.0);
    m.insert("amzn".to_string(), 110.0);
    m.insert("dow".to_string(), 105.0);
    s.update_ltp(&m);
    let mtm = s.mtm();
    // Upstream: {goog: 19*(100-102) = -38, amzn: 29*(110-110)=0 (typo,
    // really 39*0=0), dow: 0, aapl: -900}.
    assert_eq!(mtm.get("goog"), Some(&dec!(-38)));
    assert_eq!(mtm.get("amzn"), Some(&dec!(0)));
    assert_eq!(mtm.get("dow"), Some(&dec!(0)));
    assert_eq!(mtm.get("aapl"), Some(&dec!(-900)));
}

pub fn test_order_strategy_run() {
    let mut s = strategy_with_orders();
    let (_mock, broker) = sequential_mock_broker();

    // `CompoundOrderRun` analogue — run_fn captures a shared cell so
    // the test can read `d` back after strategy.run.
    let d = Arc::new(Mutex::new(0_i64));
    let d_handle = d.clone();
    let mut com_run = CompoundOrder::with_clock(default_clock());
    com_run.broker = Some(broker.clone());
    com_run
        .add(
            Order::from_init_with_clock(
                OrderInit {
                    symbol: "xom".into(),
                    side: "buy".into(),
                    quantity: 100,
                    ..Default::default()
                },
                default_clock(),
            ),
            None,
            None,
        )
        .unwrap();
    com_run.run_fn = Some(Arc::new(move |_co, data| {
        let v = data.get("xom").copied().unwrap_or(0.0);
        *d_handle.lock() = v as i64;
    }));

    // `CompoundOrderNoRun` analogue — no run_fn set, strategy.run skips.
    let mut com_no_run = CompoundOrder::with_clock(default_clock());
    com_no_run.broker = Some(broker);
    com_no_run
        .add(
            Order::from_init_with_clock(
                OrderInit {
                    symbol: "xom".into(),
                    side: "buy".into(),
                    quantity: 100,
                    ..Default::default()
                },
                default_clock(),
            ),
            None,
            None,
        )
        .unwrap();

    s.orders.push(com_run);
    s.orders.push(com_no_run);

    let mut ltp = HashMap::new();
    ltp.insert("goog".to_string(), 100.0);
    ltp.insert("amzn".to_string(), 110.0);
    ltp.insert("xom".to_string(), 105.0);
    s.run(&ltp);
    assert_eq!(*d.lock(), 105);
}

pub fn test_order_strategy_add() {
    let mut s = strategy_with_orders();
    assert_eq!(s.orders.len(), 2);
    let (_mock, broker) = sequential_mock_broker();
    let mut com = CompoundOrder::with_clock(default_clock());
    com.broker = Some(broker);
    com.add(
        Order::from_init_with_clock(
            OrderInit {
                symbol: "xom".into(),
                side: "buy".into(),
                quantity: 100,
                ..Default::default()
            },
            default_clock(),
        ),
        None,
        None,
    )
    .unwrap();
    s.orders.push(com);
    assert_eq!(s.orders.len(), 3);
}
