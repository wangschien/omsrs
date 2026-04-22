//! R12.3b — async mirror of `tests/parity/test_compound_order.rs`
//! focused on: a) pure-state spot checks, b) async execute_all_async
//! + check_flags_async behavior.
//!
//! Sync parity covers 40 items in `test_compound_order.rs`; the
//! aggregate-views / add / add_order / get / update_orders bodies
//! are line-for-line duplicates of sync in `AsyncCompoundOrder`,
//! so here we sample the surface rather than clone all 40 tests.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use omsrs::async_broker::AsyncBroker;
use omsrs::async_compound_order::AsyncCompoundOrder;
use omsrs::clock::{Clock, MockClock};
use omsrs::order::OrderInit;
use rust_decimal_macros::dec;
use serde_json::{json, Value};

// ── AsyncMockBroker (same shape as parity_async_order) ────────

#[derive(Debug, Default)]
struct AsyncMockBroker {
    place_calls: Mutex<Vec<HashMap<String, Value>>>,
    modify_calls: Mutex<Vec<HashMap<String, Value>>>,
    cancel_calls: Mutex<Vec<HashMap<String, Value>>>,
}

#[allow(dead_code)] // Some accessors only used by a subset of tests.
impl AsyncMockBroker {
    fn new() -> Self {
        Self::default()
    }
    fn place_calls(&self) -> Vec<HashMap<String, Value>> {
        self.place_calls.lock().unwrap().clone()
    }
    fn modify_calls(&self) -> Vec<HashMap<String, Value>> {
        self.modify_calls.lock().unwrap().clone()
    }
    fn cancel_calls(&self) -> Vec<HashMap<String, Value>> {
        self.cancel_calls.lock().unwrap().clone()
    }
    fn place_call_count(&self) -> usize {
        self.place_calls.lock().unwrap().len()
    }
    fn modify_call_count(&self) -> usize {
        self.modify_calls.lock().unwrap().len()
    }
    fn cancel_call_count(&self) -> usize {
        self.cancel_calls.lock().unwrap().len()
    }
}

#[async_trait]
impl AsyncBroker for AsyncMockBroker {
    async fn order_place(&self, args: HashMap<String, Value>) -> Option<String> {
        self.place_calls.lock().unwrap().push(args);
        Some(format!(
            "MOCK-{}",
            self.place_calls.lock().unwrap().len()
        ))
    }
    async fn order_modify(&self, args: HashMap<String, Value>) {
        self.modify_calls.lock().unwrap().push(args);
    }
    async fn order_cancel(&self, args: HashMap<String, Value>) {
        self.cancel_calls.lock().unwrap().push(args);
    }
}

// ── fixtures ────────────────────────────────────────────────

fn mock_clock(t: chrono::DateTime<Utc>) -> Arc<dyn Clock + Send + Sync> {
    Arc::new(MockClock::new(t))
}

fn default_clock() -> Arc<dyn Clock + Send + Sync> {
    mock_clock(Utc.with_ymd_and_hms(2023, 1, 1, 10, 0, 0).unwrap())
}

fn compound_with_broker() -> (AsyncCompoundOrder, Arc<AsyncMockBroker>) {
    let mock = Arc::new(AsyncMockBroker::new());
    let broker: Arc<dyn AsyncBroker + Send + Sync> = mock.clone();
    let com = AsyncCompoundOrder::with_clock(default_clock()).with_broker(broker);
    (com, mock)
}

fn simple_compound_with_three_orders() -> (AsyncCompoundOrder, Arc<AsyncMockBroker>) {
    let (mut com, mock) = compound_with_broker();
    for (sym, side, qty) in [
        ("aapl", "buy", 10),
        ("goog", "sell", 20),
        ("aapl", "buy", 5),
    ] {
        com.add_order(
            OrderInit {
                symbol: sym.into(),
                side: side.into(),
                quantity: qty,
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
    }
    (com, mock)
}

// ── R12.3b.async_compound.1 — defaults
#[tokio::test]
async fn defaults_match_sync() {
    let com = AsyncCompoundOrder::new();
    assert_eq!(com.count(), 0);
    assert!(com.is_empty());
    assert!(com.broker.is_none());
    assert!(com.connection.is_none());
    assert_eq!(com.get_next_index(), 0);
}

// ── R12.3b.async_compound.2 — add_order assigns parent_id + index
#[tokio::test]
async fn add_order_sets_parent_id_and_next_index() {
    let (mut com, _) = compound_with_broker();
    let parent = com.id.clone();
    com.add_order(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 10,
            ..Default::default()
        },
        None,
        None,
    )
    .unwrap();
    assert_eq!(com.count(), 1);
    assert_eq!(com.get_next_index(), 1);
    let o = com.get_by_index(0).unwrap();
    assert_eq!(o.parent_id.as_deref(), Some(parent.as_str()));
}

// ── R12.3b.async_compound.3 — index collision rejected
#[tokio::test]
async fn add_order_rejects_duplicate_index() {
    let (mut com, _) = compound_with_broker();
    com.add_order(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 1,
            ..Default::default()
        },
        Some(5),
        None,
    )
    .unwrap();
    let err = com
        .add_order(
            OrderInit {
                symbol: "goog".into(),
                side: "buy".into(),
                quantity: 1,
                ..Default::default()
            },
            Some(5),
            None,
        )
        .unwrap_err();
    assert!(matches!(err, omsrs::compound_order::CompoundError::IndexAlreadyUsed(5)));
}

// ── R12.3b.async_compound.4 — positions / buy_qty / sell_qty
#[tokio::test]
async fn position_aggregates_work() {
    let (mut com, _) = compound_with_broker();
    for (sym, side, qty, fill) in [
        ("aapl", "buy", 10, 10),
        ("goog", "sell", 20, 20),
        ("aapl", "buy", 5, 5),
    ] {
        com.add_order(
            OrderInit {
                symbol: sym.into(),
                side: side.into(),
                quantity: qty,
                filled_quantity: Some(fill),
                average_price: Some(dec!(100)),
                status: Some("COMPLETE".into()),
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
    }
    let pos = com.positions();
    assert_eq!(pos.get("aapl").copied(), Some(15));
    assert_eq!(pos.get("goog").copied(), Some(-20));
    assert_eq!(com.buy_quantity().get("aapl").copied(), Some(15));
    assert_eq!(com.sell_quantity().get("goog").copied(), Some(20));
}

// ── R12.3b.async_compound.5 — execute_all_async fans out to N
// order_place calls
#[tokio::test]
async fn execute_all_async_fans_out_to_every_order() {
    let (mut com, mock) = simple_compound_with_three_orders();
    com.execute_all_async(HashMap::new()).await;
    assert_eq!(mock.place_call_count(), 3);
}

// ── R12.3b.async_compound.6 — execute_all_async merges
// order_args + caller kwargs
#[tokio::test]
async fn execute_all_async_merges_order_args_and_caller_kwargs() {
    let (mut com, mock) = simple_compound_with_three_orders();
    // Class-level order_args:
    let mut class_args = HashMap::new();
    class_args.insert("variety".into(), json!("regular"));
    class_args.insert("exchange".into(), json!("NSE"));
    class_args.insert("product".into(), json!("MIS"));
    com.order_args = class_args;
    // Caller's kwargs override one of the class args:
    let mut kwargs = HashMap::new();
    kwargs.insert("product".into(), json!("CNC"));
    com.execute_all_async(kwargs).await;
    for call in mock.place_calls() {
        assert_eq!(call.get("variety"), Some(&json!("regular")));
        assert_eq!(call.get("exchange"), Some(&json!("NSE")));
        // Caller kwarg wins over class-level order_args.
        assert_eq!(call.get("product"), Some(&json!("CNC")));
    }
}

// ── R12.3b.async_compound.7 — execute_all_async is a no-op
// without broker
#[tokio::test]
async fn execute_all_async_noop_without_broker() {
    let mut com = AsyncCompoundOrder::with_clock(default_clock());
    com.add_order(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 10,
            ..Default::default()
        },
        None,
        None,
    )
    .unwrap();
    com.execute_all_async(HashMap::new()).await;
    // No broker attached; no panic and the order_id stays None.
    assert!(com.get_by_index(0).unwrap().order_id.is_none());
}

// ── R12.3b.async_compound.8 — check_flags_async converts pending
// expired to MARKET via modify_async. Follows sync parity pattern
// (`tests/parity/test_compound_order.rs:test_compound_order_
// check_flags_convert_to_market_after_expiry`): use a
// time-advancing MockClock rather than replacing the clock —
// order.timestamp is set at add_order time from the clock's
// current value, so later clock advances are what drive
// has_expired().
#[tokio::test]
async fn check_flags_async_converts_expired_to_market() {
    let base = Utc.with_ymd_and_hms(2021, 1, 1, 10, 0, 0).unwrap();
    let clock_handle = MockClock::new(base);
    let clock: Arc<dyn Clock + Send + Sync> = Arc::new(clock_handle.clone());
    let mock = Arc::new(AsyncMockBroker::new());
    let broker: Arc<dyn AsyncBroker + Send + Sync> = mock.clone();
    let mut com = AsyncCompoundOrder::with_clock(clock).with_broker(broker);

    com.add_order(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 10,
            order_type: Some("LIMIT".into()),
            price: Some(dec!(650)),
            order_id: Some("abcdef".into()),
            expires_in: Some(30),
            convert_to_market_after_expiry: Some(true),
            ..Default::default()
        },
        None,
        None,
    )
    .unwrap();
    // Pre-expiry: no modify.
    com.check_flags_async().await;
    assert_eq!(mock.modify_call_count(), 0);
    // Advance clock past the 30s window.
    clock_handle.set(base + chrono::Duration::seconds(30));
    com.check_flags_async().await;
    assert_eq!(mock.modify_call_count(), 1);
    assert_eq!(mock.cancel_call_count(), 0);
    assert_eq!(com.get_by_index(0).unwrap().order_type, "MARKET");
}

// ── R12.3b.async_compound.9 — check_flags_async cancels when
// cancel_after_expiry is set
#[tokio::test]
async fn check_flags_async_cancels_expired_when_flagged() {
    let base = Utc.with_ymd_and_hms(2021, 1, 1, 10, 0, 0).unwrap();
    let clock_handle = MockClock::new(base);
    let clock: Arc<dyn Clock + Send + Sync> = Arc::new(clock_handle.clone());
    let mock = Arc::new(AsyncMockBroker::new());
    let broker: Arc<dyn AsyncBroker + Send + Sync> = mock.clone();
    let mut com = AsyncCompoundOrder::with_clock(clock).with_broker(broker);

    com.add_order(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 10,
            order_type: Some("LIMIT".into()),
            price: Some(dec!(650)),
            order_id: Some("abcdef".into()),
            expires_in: Some(30),
            cancel_after_expiry: Some(true),
            ..Default::default()
        },
        None,
        None,
    )
    .unwrap();
    com.check_flags_async().await;
    assert_eq!(mock.cancel_call_count(), 0);
    clock_handle.set(base + chrono::Duration::seconds(30));
    com.check_flags_async().await;
    assert_eq!(mock.cancel_call_count(), 1);
    assert_eq!(mock.modify_call_count(), 0);
}

// ── R12.3b.async_compound.10 — save() stays sync
#[tokio::test]
async fn save_is_sync_and_returns_count() {
    let (com, _) = simple_compound_with_three_orders();
    // No connection attached → save_to_db returns false per
    // order; counted = 0.
    assert_eq!(com.save(), 0);
}
