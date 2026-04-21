//! R11.2 — async mirror of R4's 10-item Paper parity harness.
//!
//! Same assertions as `tests/parity/test_base.rs`, driven through
//! `omsrs::AsyncBroker` + `omsrs::AsyncPaper` under `#[tokio::test]`.
//! The sync suite verifies the sync trait; this suite verifies the
//! async trait matches semantically. Both must pass for v0.2 ACK.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use omsrs::async_broker::{AsyncBroker, AsyncSymbolTransformer};
use omsrs::broker::rename;
use omsrs::AsyncPaper;
use serde_json::{json, Value};

// ── fixtures + rename (copy of sync parity helpers) ─────────────────

fn positions_rename() -> HashMap<String, String> {
    [("tradingsymbol".to_string(), "symbol".to_string())]
        .into_iter()
        .collect()
}

fn orders_rename() -> HashMap<String, String> {
    [
        ("tradingsymbol".to_string(), "symbol".to_string()),
        ("transaction_type".to_string(), "side".to_string()),
    ]
    .into_iter()
    .collect()
}

fn value_to_map(v: &Value) -> HashMap<String, Value> {
    match v {
        Value::Object(m) => m.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        other => panic!("expected object, got {other:?}"),
    }
}

fn kwargs(pairs: &[(&str, Value)]) -> HashMap<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect()
}

/// Async sibling of `DummyBroker`. Same fixture loading, same echo
/// behaviour, methods are async.
struct AsyncDummyBroker {
    orders: Vec<HashMap<String, Value>>,
    positions: Vec<HashMap<String, Value>>,
    trades: Vec<HashMap<String, Value>>,
    place_calls: Mutex<Vec<HashMap<String, Value>>>,
    modify_calls: Mutex<Vec<HashMap<String, Value>>>,
    cancel_calls: Mutex<Vec<HashMap<String, Value>>>,
}

impl AsyncDummyBroker {
    fn new() -> Self {
        let orders_src = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/data/kiteconnect/orders.json"
        ));
        let positions_src = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/data/kiteconnect/positions.json"
        ));
        let trades_src = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/data/kiteconnect/trades.json"
        ));

        let orders_root: Value = serde_json::from_str(orders_src).unwrap();
        let positions_root: Value = serde_json::from_str(positions_src).unwrap();
        let trades_root: Value = serde_json::from_str(trades_src).unwrap();

        let orders = orders_root["data"]
            .as_array()
            .expect("orders.data array")
            .iter()
            .map(|v| {
                let mut m = value_to_map(v);
                m.insert("status".into(), json!("pending"));
                rename(&m, &orders_rename())
            })
            .collect();
        let positions = positions_root["data"]["day"]
            .as_array()
            .expect("positions.data.day array")
            .iter()
            .map(|v| rename(&value_to_map(v), &positions_rename()))
            .collect();
        let trades = trades_root["data"]
            .as_array()
            .expect("trades.data array")
            .iter()
            .map(|v| rename(&value_to_map(v), &orders_rename()))
            .collect();

        Self {
            orders,
            positions,
            trades,
            place_calls: Mutex::new(Vec::new()),
            modify_calls: Mutex::new(Vec::new()),
            cancel_calls: Mutex::new(Vec::new()),
        }
    }

    fn place_calls(&self) -> Vec<HashMap<String, Value>> {
        self.place_calls.lock().unwrap().clone()
    }

    fn cancel_calls(&self) -> Vec<HashMap<String, Value>> {
        self.cancel_calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl AsyncBroker for AsyncDummyBroker {
    async fn order_place(&self, args: HashMap<String, Value>) -> Option<String> {
        self.place_calls.lock().unwrap().push(args);
        Some("DUMMY-PLACED".into())
    }

    async fn order_modify(&self, args: HashMap<String, Value>) {
        self.modify_calls.lock().unwrap().push(args);
    }

    async fn order_cancel(&self, args: HashMap<String, Value>) {
        self.cancel_calls.lock().unwrap().push(args);
    }

    async fn orders(&self) -> Vec<HashMap<String, Value>> {
        self.orders.clone()
    }

    async fn positions(&self) -> Vec<HashMap<String, Value>> {
        self.positions.clone()
    }

    async fn trades(&self) -> Vec<HashMap<String, Value>> {
        self.trades.clone()
    }
}

// ── 10 parity items, mirroring tests/parity/test_base.rs ────────────

#[tokio::test]
async fn async_test_dummy_broker_values() {
    let broker = AsyncDummyBroker::new();
    broker
        .order_place(kwargs(&[("symbol", json!("aapl"))]))
        .await;
    assert_eq!(
        broker.place_calls()[0],
        kwargs(&[("symbol", json!("aapl"))])
    );
    broker
        .order_modify(kwargs(&[("order_id", json!(1234))]))
        .await;
    broker
        .order_cancel(kwargs(&[("order_id", json!(1234))]))
        .await;
}

#[tokio::test]
async fn async_test_close_all_positions() {
    let broker = AsyncDummyBroker::new();
    broker.close_all_positions(None, None, None, None).await;
    let calls = broker.place_calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[0],
        kwargs(&[
            ("symbol", json!("GOLDGUINEA17DECFUT")),
            ("order_type", json!("MARKET")),
            ("quantity", json!(3)),
            ("side", json!("buy")),
        ])
    );
    assert_eq!(
        calls[1],
        kwargs(&[
            ("symbol", json!("LEADMINI17DECFUT")),
            ("order_type", json!("MARKET")),
            ("quantity", json!(1)),
            ("side", json!("sell")),
        ])
    );
}

#[tokio::test]
async fn async_test_cancel_all_orders() {
    let broker = AsyncDummyBroker::new();
    broker.cancel_all_orders(None, None).await;
    let expected_ids = [
        "100000000000000",
        "300000000000000",
        "500000000000000",
        "700000000000000",
        "9000000000000000",
    ];
    let cancelled = broker.cancel_calls();
    assert_eq!(cancelled.len(), expected_ids.len());
    for (actual, expected) in cancelled.iter().zip(expected_ids) {
        assert_eq!(actual.get("order_id"), Some(&json!(expected)));
    }
}

#[tokio::test]
async fn async_test_close_all_positions_copy_keys() {
    let broker = AsyncDummyBroker::new();
    broker
        .close_all_positions(None, Some(&["exchange", "product"]), None, None)
        .await;
    let calls = broker.place_calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[0],
        kwargs(&[
            ("symbol", json!("GOLDGUINEA17DECFUT")),
            ("order_type", json!("MARKET")),
            ("quantity", json!(3)),
            ("side", json!("buy")),
            ("product", json!("NRML")),
            ("exchange", json!("MCX")),
        ])
    );
    assert_eq!(
        calls[1],
        kwargs(&[
            ("symbol", json!("LEADMINI17DECFUT")),
            ("order_type", json!("MARKET")),
            ("quantity", json!(1)),
            ("side", json!("sell")),
            ("product", json!("NRML")),
            ("exchange", json!("MCX")),
        ])
    );
}

#[tokio::test]
async fn async_test_close_all_positions_add_keys() {
    let broker = AsyncDummyBroker::new();
    let add: HashMap<String, Value> = kwargs(&[("variety", json!("regular"))]);
    broker
        .close_all_positions(None, None, Some(&add), None)
        .await;
    let calls = broker.place_calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].get("variety"), Some(&json!("regular")));
    assert_eq!(calls[0].get("side"), Some(&json!("buy")));
    assert_eq!(calls[1].get("variety"), Some(&json!("regular")));
    assert_eq!(calls[1].get("side"), Some(&json!("sell")));
}

#[tokio::test]
async fn async_test_close_all_positions_copy_and_add_keys() {
    let broker = AsyncDummyBroker::new();
    let add: HashMap<String, Value> = kwargs(&[("validity", json!("day"))]);
    broker
        .close_all_positions(None, Some(&["exchange", "product"]), Some(&add), None)
        .await;
    let calls = broker.place_calls();
    assert_eq!(calls.len(), 2);
    for (i, expected_side) in ["buy", "sell"].iter().enumerate() {
        let c = &calls[i];
        assert_eq!(c.get("side"), Some(&json!(expected_side)));
        assert_eq!(c.get("product"), Some(&json!("NRML")));
        assert_eq!(c.get("exchange"), Some(&json!("MCX")));
        assert_eq!(c.get("validity"), Some(&json!("day")));
    }
}

#[tokio::test]
async fn async_test_close_all_positions_quantity_as_string() {
    let broker = AsyncPaper::new().with_positions(vec![
        kwargs(&[
            ("symbol", json!("aapl")),
            ("quantity", json!("10")),
            ("tag", json!("reg")),
        ]),
        kwargs(&[
            ("symbol", json!("meta")),
            ("quantity", json!("-10")),
            ("tag", json!("reg")),
        ]),
        kwargs(&[
            ("symbol", json!("goog")),
            ("quantity", json!("0")),
            ("tag", json!("reg")),
        ]),
    ]);
    let add: HashMap<String, Value> = kwargs(&[("variety", json!("regular"))]);
    broker
        .close_all_positions(None, None, Some(&add), None)
        .await;
    let calls = broker.place_calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].get("symbol"), Some(&json!("aapl")));
    assert_eq!(calls[0].get("quantity"), Some(&json!(10)));
    assert_eq!(calls[0].get("side"), Some(&json!("sell")));
    assert_eq!(calls[0].get("variety"), Some(&json!("regular")));
    assert_eq!(calls[1].get("symbol"), Some(&json!("meta")));
    assert_eq!(calls[1].get("quantity"), Some(&json!(10)));
    assert_eq!(calls[1].get("side"), Some(&json!("buy")));
    assert_eq!(calls[1].get("variety"), Some(&json!("regular")));
}

#[tokio::test]
async fn async_test_close_all_positions_quantity_as_error() {
    let broker = AsyncPaper::new().with_positions(vec![
        kwargs(&[
            ("symbol", json!("aapl")),
            ("quantity", json!("10")),
            ("tag", json!("reg")),
        ]),
        kwargs(&[
            ("symbol", json!("meta")),
            ("quantity", json!("-10")),
            ("tag", json!("reg")),
        ]),
        kwargs(&[
            ("symbol", json!("goog")),
            ("quantity", json!("O")),
            ("tag", json!("reg")),
        ]),
    ]);
    let add: HashMap<String, Value> = kwargs(&[("variety", json!("regular"))]);
    broker
        .close_all_positions(None, None, Some(&add), None)
        .await;
    let calls = broker.place_calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].get("symbol"), Some(&json!("aapl")));
    assert_eq!(calls[1].get("symbol"), Some(&json!("meta")));
}

#[tokio::test]
async fn async_test_close_all_positions_symbol_transfomer() {
    let broker = AsyncPaper::new().with_positions(vec![
        kwargs(&[
            ("symbol", json!("aapl")),
            ("quantity", json!("10")),
            ("tag", json!("reg")),
        ]),
        kwargs(&[
            ("symbol", json!("meta")),
            ("quantity", json!("-10")),
            ("tag", json!("reg")),
        ]),
    ]);
    let add: HashMap<String, Value> = kwargs(&[("variety", json!("regular"))]);
    let transform: AsyncSymbolTransformer =
        Arc::new(|s: &str| format!("nyse:{s}"));
    broker
        .close_all_positions(None, None, Some(&add), Some(transform))
        .await;
    let calls = broker.place_calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].get("symbol"), Some(&json!("nyse:aapl")));
    assert_eq!(calls[1].get("symbol"), Some(&json!("nyse:meta")));
}

#[tokio::test]
async fn async_test_close_all_positions_given_positions() {
    let broker = AsyncPaper::new();
    let positions = vec![kwargs(&[
        ("symbol", json!("aapl")),
        ("quantity", json!(10)),
        ("tag", json!("reg")),
    ])];
    broker
        .close_all_positions(Some(positions), None, None, None)
        .await;
    assert_eq!(broker.place_call_count(), 1);
    assert_eq!(broker.place_calls()[0].get("symbol"), Some(&json!("aapl")));
    assert_eq!(broker.place_calls()[0].get("quantity"), Some(&json!(10)));
    assert_eq!(broker.place_calls()[0].get("side"), Some(&json!("sell")));
}
