//! R12.3a — async mirror of `tests/parity/test_order.rs` lifecycle subset.
//!
//! Mirrors the 9 sync tests that drive `Order::execute` / `modify` /
//! `cancel` through a `MockBroker`. Each sync test has an async
//! sibling that asserts identical behavior via `Order::execute_async`
//! / `modify_async` / `cancel_async` driven by `AsyncMockBroker`.
//!
//! The invariant: sync and async paths produce bit-for-bit identical
//! `order_args` (the HashMap passed to broker.order_place /
//! order_modify / order_cancel), so every sync-side assertion about
//! the broker's call history translates directly.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use omsrs::async_broker::AsyncBroker;
use omsrs::clock::{Clock, MockClock};
use omsrs::order::{Order, OrderInit};
use rust_decimal_macros::dec;
use serde_json::{json, Value};

// ── fixtures (mirror sync) ────────────────────────────────

fn kwargs(pairs: &[(&str, Value)]) -> HashMap<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect()
}

fn default_mock_clock() -> Arc<dyn Clock + Send + Sync> {
    Arc::new(MockClock::new(
        Utc.with_ymd_and_hms(2022, 1, 1, 10, 0, 0).unwrap(),
    ))
}

fn simple_order(clock: Arc<dyn Clock + Send + Sync>) -> Order {
    Order::from_init_with_clock(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 10,
            order_type: Some("LIMIT".into()),
            price: Some(dec!(650)),
            order_id: Some("abcdef".into()),
            ..Default::default()
        },
        clock,
    )
}

// ── AsyncMockBroker — mirror of sync MockBroker ──────────────

#[derive(Debug, Default)]
pub struct AsyncMockBroker {
    place_calls: Mutex<Vec<HashMap<String, Value>>>,
    modify_calls: Mutex<Vec<HashMap<String, Value>>>,
    cancel_calls: Mutex<Vec<HashMap<String, Value>>>,
    place_returns: Mutex<Vec<Option<String>>>,
    attrs_execute: Mutex<Option<Vec<String>>>,
    attrs_modify: Mutex<Option<Vec<String>>>,
    attrs_cancel: Mutex<Option<Vec<String>>>,
}

impl AsyncMockBroker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_place_return(&self, v: Option<String>) {
        self.place_returns.lock().unwrap().push(v);
    }

    pub fn set_attribs_to_copy_execute(&self, v: Option<Vec<String>>) {
        *self.attrs_execute.lock().unwrap() = v;
    }
    pub fn set_attribs_to_copy_modify(&self, v: Option<Vec<String>>) {
        *self.attrs_modify.lock().unwrap() = v;
    }
    pub fn set_attribs_to_copy_cancel(&self, v: Option<Vec<String>>) {
        *self.attrs_cancel.lock().unwrap() = v;
    }

    pub fn place_calls(&self) -> Vec<HashMap<String, Value>> {
        self.place_calls.lock().unwrap().clone()
    }
    pub fn modify_calls(&self) -> Vec<HashMap<String, Value>> {
        self.modify_calls.lock().unwrap().clone()
    }
    pub fn cancel_calls(&self) -> Vec<HashMap<String, Value>> {
        self.cancel_calls.lock().unwrap().clone()
    }
    pub fn place_call_count(&self) -> usize {
        self.place_calls.lock().unwrap().len()
    }
    pub fn modify_call_count(&self) -> usize {
        self.modify_calls.lock().unwrap().len()
    }
    pub fn cancel_call_count(&self) -> usize {
        self.cancel_calls.lock().unwrap().len()
    }
}

#[async_trait]
impl AsyncBroker for AsyncMockBroker {
    async fn order_place(&self, args: HashMap<String, Value>) -> Option<String> {
        self.place_calls.lock().unwrap().push(args);
        let mut guard = self.place_returns.lock().unwrap();
        if guard.is_empty() {
            Some(format!(
                "ASYNC-MOCK-{}",
                self.place_calls.lock().unwrap().len()
            ))
        } else {
            guard.remove(0)
        }
    }

    async fn order_modify(&self, args: HashMap<String, Value>) {
        self.modify_calls.lock().unwrap().push(args);
    }

    async fn order_cancel(&self, args: HashMap<String, Value>) {
        self.cancel_calls.lock().unwrap().push(args);
    }

    async fn attribs_to_copy_execute(&self) -> Option<Vec<String>> {
        self.attrs_execute.lock().unwrap().clone()
    }

    async fn attribs_to_copy_modify(&self) -> Option<Vec<String>> {
        self.attrs_modify.lock().unwrap().clone()
    }

    async fn attribs_to_copy_cancel(&self) -> Option<Vec<String>> {
        self.attrs_cancel.lock().unwrap().clone()
    }
}

// ── R12.3a.async_order.1 — execute builds broker args from
// Order state (mirror sync test_simple_order_execute)
#[tokio::test]
async fn execute_builds_broker_args_from_order_state() {
    let broker = AsyncMockBroker::new();
    let mut order = Order::from_init_with_clock(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 10,
            order_type: Some("LIMIT".into()),
            price: Some(dec!(650)),
            ..Default::default()
        },
        default_mock_clock(),
    );
    order.execute_async(&broker, None, HashMap::new()).await;
    assert_eq!(broker.place_call_count(), 1);
    let args = &broker.place_calls()[0];
    assert_eq!(args.get("symbol"), Some(&json!("AAPL")));
    assert_eq!(args.get("side"), Some(&json!("BUY")));
    assert_eq!(args.get("quantity"), Some(&json!(10)));
    assert_eq!(args.get("order_type"), Some(&json!("LIMIT")));
    assert_eq!(args.get("price"), Some(&json!("650")));
    assert_eq!(args.get("trigger_price"), Some(&json!("0")));
    assert_eq!(args.get("disclosed_quantity"), Some(&json!(0)));
}

// ── R12.3a.async_order.2 — extra kwargs pass through
#[tokio::test]
async fn execute_passes_through_extra_kwargs() {
    let broker = AsyncMockBroker::new();
    let mut order = Order::from_init_with_clock(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 10,
            order_type: Some("LIMIT".into()),
            price: Some(dec!(650)),
            ..Default::default()
        },
        default_mock_clock(),
    );
    order
        .execute_async(
            &broker,
            None,
            kwargs(&[("exchange", json!("NSE")), ("variety", json!("regular"))]),
        )
        .await;
    let args = &broker.place_calls()[0];
    assert_eq!(args.get("exchange"), Some(&json!("NSE")));
    assert_eq!(args.get("variety"), Some(&json!("regular")));
    assert_eq!(args.get("price"), Some(&json!("650")));
}

// ── R12.3a.async_order.3 — kwargs don't override Order's own
// default-keys (symbol / quantity / order_type / price / …)
#[tokio::test]
async fn execute_kwargs_do_not_override_default_keys() {
    let broker = AsyncMockBroker::new();
    let mut order = Order::from_init_with_clock(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 10,
            order_type: Some("LIMIT".into()),
            price: Some(dec!(650)),
            ..Default::default()
        },
        default_mock_clock(),
    );
    order
        .execute_async(
            &broker,
            None,
            kwargs(&[
                ("exchange", json!("NSE")),
                ("variety", json!("regular")),
                ("quantity", json!(20)),
                ("order_type", json!("MARKET")),
            ]),
        )
        .await;
    let args = &broker.place_calls()[0];
    assert_eq!(args.get("quantity"), Some(&json!(10)));
    assert_eq!(args.get("order_type"), Some(&json!("LIMIT")));
    assert_eq!(args.get("exchange"), Some(&json!("NSE")));
    assert_eq!(args.get("variety"), Some(&json!("regular")));
}

// ── R12.3a.async_order.4 — execute is idempotent once order_id
// is set (no duplicate placements on re-call)
#[tokio::test]
async fn execute_is_idempotent_once_order_id_set() {
    let broker = AsyncMockBroker::new();
    broker.set_place_return(Some("aaabbb".into()));
    let mut order = Order::from_init_with_clock(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 10,
            order_type: Some("LIMIT".into()),
            price: Some(dec!(650)),
            ..Default::default()
        },
        default_mock_clock(),
    );
    for _ in 0..10 {
        order
            .execute_async(
                &broker,
                None,
                kwargs(&[("exchange", json!("NSE")), ("variety", json!("regular"))]),
            )
            .await;
    }
    assert_eq!(broker.place_call_count(), 1);
}

// ── R12.3a.async_order.5 — completed order is never executed
#[tokio::test]
async fn execute_skips_completed_order() {
    let broker = AsyncMockBroker::new();
    let mut order = Order::from_init_with_clock(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 10,
            order_type: Some("LIMIT".into()),
            price: Some(dec!(650)),
            filled_quantity: Some(10),
            ..Default::default()
        },
        default_mock_clock(),
    );
    for _ in 0..10 {
        order
            .execute_async(
                &broker,
                None,
                kwargs(&[("exchange", json!("NSE")), ("variety", json!("regular"))]),
            )
            .await;
    }
    assert_eq!(broker.place_call_count(), 0);
}

// ── R12.3a.async_order.6 — modify builds broker args
#[tokio::test]
async fn modify_builds_broker_args_from_order_state() {
    let broker = AsyncMockBroker::new();
    let mut order = simple_order(default_mock_clock());
    order.price = Some(dec!(630));
    order.modify_async(&broker, None, HashMap::new()).await;
    assert_eq!(broker.modify_call_count(), 1);
    let args = &broker.modify_calls()[0];
    assert_eq!(args.get("order_id"), Some(&json!("abcdef")));
    assert_eq!(args.get("quantity"), Some(&json!(10)));
    assert_eq!(args.get("order_type"), Some(&json!("LIMIT")));
    assert_eq!(args.get("price"), Some(&json!("630")));
    assert_eq!(args.get("trigger_price"), Some(&json!("0")));
    assert_eq!(args.get("disclosed_quantity"), Some(&json!(0)));
}

// ── R12.3a.async_order.7 — cancel sends order_id
#[tokio::test]
async fn cancel_sends_order_id_to_broker() {
    let broker = AsyncMockBroker::new();
    let mut order = Order::from_init_with_clock(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 10,
            order_type: Some("LIMIT".into()),
            price: Some(dec!(650)),
            order_id: Some("abcdef".into()),
            ..Default::default()
        },
        default_mock_clock(),
    );
    order.cancel_async(&broker, None).await;
    assert_eq!(broker.cancel_call_count(), 1);
    assert_eq!(
        broker.cancel_calls()[0].get("order_id"),
        Some(&json!("abcdef"))
    );
}

// ── R12.3a.async_order.8 — cancel without order_id is a no-op
#[tokio::test]
async fn cancel_without_order_id_is_noop() {
    let broker = AsyncMockBroker::new();
    let mut order = Order::from_init_with_clock(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 10,
            order_type: Some("LIMIT".into()),
            price: Some(dec!(650)),
            ..Default::default()
        },
        default_mock_clock(),
    );
    order.cancel_async(&broker, None).await;
    assert_eq!(broker.cancel_call_count(), 0);
}

// ── R12.3a.async_order.9 — broker.attribs_to_copy_execute is
// awaited + merged into order_args. Exercises the async-specific
// path that sync tests can't reach (sync calls a sync hook).
#[tokio::test]
async fn execute_merges_broker_async_attribs_to_copy() {
    let broker = AsyncMockBroker::new();
    broker.set_attribs_to_copy_execute(Some(vec!["tag".into(), "client_id".into()]));
    let mut order = Order::from_init_with_clock(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 10,
            order_type: Some("LIMIT".into()),
            price: Some(dec!(650)),
            tag: Some("entry".into()),
            client_id: Some("CL-01".into()),
            ..Default::default()
        },
        default_mock_clock(),
    );
    order.execute_async(&broker, None, HashMap::new()).await;
    let args = &broker.place_calls()[0];
    assert_eq!(args.get("tag"), Some(&json!("entry")));
    assert_eq!(args.get("client_id"), Some(&json!("CL-01")));
    // Default-key fields still intact (attribs_to_copy can't
    // overwrite them).
    assert_eq!(args.get("quantity"), Some(&json!(10)));
    assert_eq!(args.get("order_type"), Some(&json!("LIMIT")));
}

// ── R12.3a.async_order.10 — R12 semver guard: the pre-existing
// sync `execute` / `modify` / `cancel` methods still compile
// and drive the sync Broker path (smoke). Ensures R12.3a is
// purely additive.
#[test]
fn sync_execute_still_works_after_r12_3a() {
    // Only a build-time check: calling sync methods through a
    // sync Broker still works. No broker object here; we just
    // rely on `cargo check` seeing both signatures.
    fn _type_check<B: omsrs::broker::Broker>(broker: &B, order: &mut Order) {
        order.execute(broker, None, HashMap::new());
        order.modify(broker, None, HashMap::new());
        order.cancel(broker, None);
    }
    let _ = _type_check::<omsrs::brokers::Paper> as fn(_, _);
}
