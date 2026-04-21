//! Parity ports of `tests/test_base.py`. R4 scope per PORT-PLAN §8:
//! 10 items = all 12 upstream trials minus the 2 `test_cover_orders*`
//! (deferred — `tick()` / `cover_orders()` not in MVP).

use std::collections::HashMap;
use std::sync::Mutex;

use omsrs::broker::{rename, Broker};
use omsrs::Paper;
use serde_json::{json, Value};

/// Upstream `base.yaml` override table used by the Dummy broker fixture.
/// We pre-apply the rename to the JSON fixtures at load time rather than
/// threading a live `pre/post` override system through the trait — yaml
/// override loading is deferred per source-notes §12.
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

/// Upstream `Dummy(Broker)`. Loads kiteconnect JSON fixtures + applies
/// the zerodha-yaml rename to each record; mutates every order's
/// `status` to `"pending"` (mirrors upstream test_base.py lines 17–18).
pub struct DummyBroker {
    orders: Vec<HashMap<String, Value>>,
    positions: Vec<HashMap<String, Value>>,
    trades: Vec<HashMap<String, Value>>,
    place_calls: Mutex<Vec<HashMap<String, Value>>>,
    modify_calls: Mutex<Vec<HashMap<String, Value>>>,
    cancel_calls: Mutex<Vec<HashMap<String, Value>>>,
}

impl DummyBroker {
    pub fn new() -> Self {
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

    pub fn place_calls(&self) -> Vec<HashMap<String, Value>> {
        self.place_calls.lock().unwrap().clone()
    }

    pub fn cancel_calls(&self) -> Vec<HashMap<String, Value>> {
        self.cancel_calls.lock().unwrap().clone()
    }
}

impl Broker for DummyBroker {
    fn order_place(&self, args: HashMap<String, Value>) -> Option<String> {
        self.place_calls.lock().unwrap().push(args);
        Some("DUMMY-PLACED".into())
    }

    fn order_modify(&self, args: HashMap<String, Value>) {
        self.modify_calls.lock().unwrap().push(args);
    }

    fn order_cancel(&self, args: HashMap<String, Value>) {
        self.cancel_calls.lock().unwrap().push(args);
    }

    fn orders(&self) -> Vec<HashMap<String, Value>> {
        self.orders.clone()
    }

    fn positions(&self) -> Vec<HashMap<String, Value>> {
        self.positions.clone()
    }

    fn trades(&self) -> Vec<HashMap<String, Value>> {
        self.trades.clone()
    }
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

// ── R4 trials ───────────────────────────────────────────────────────────

pub fn test_dummy_broker_values() {
    let broker = DummyBroker::new();
    // Upstream: `broker.order_place(symbol="aapl") == {"symbol": "aapl"}`.
    // Our return type is Option<String>, so assert on the recorded call
    // instead — semantic equivalence (dummy echoes kwargs).
    broker.order_place(kwargs(&[("symbol", json!("aapl"))]));
    assert_eq!(
        broker.place_calls()[0],
        kwargs(&[("symbol", json!("aapl"))])
    );
    broker.order_modify(kwargs(&[("order_id", json!(1234))]));
    broker.order_cancel(kwargs(&[("order_id", json!(1234))]));
}

pub fn test_close_all_positions() {
    let broker = DummyBroker::new();
    broker.close_all_positions(None, None, None, None);
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

pub fn test_cancel_all_orders() {
    let broker = DummyBroker::new();
    broker.cancel_all_orders(None, None);
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

pub fn test_close_all_positions_copy_keys() {
    let broker = DummyBroker::new();
    broker.close_all_positions(None, Some(&["exchange", "product"]), None, None);
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

pub fn test_close_all_positions_add_keys() {
    let broker = DummyBroker::new();
    let add: HashMap<String, Value> = kwargs(&[("variety", json!("regular"))]);
    broker.close_all_positions(None, None, Some(&add), None);
    let calls = broker.place_calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].get("variety"), Some(&json!("regular")));
    assert_eq!(calls[0].get("side"), Some(&json!("buy")));
    assert_eq!(calls[1].get("variety"), Some(&json!("regular")));
    assert_eq!(calls[1].get("side"), Some(&json!("sell")));
}

pub fn test_close_all_positions_copy_and_add_keys() {
    let broker = DummyBroker::new();
    let add: HashMap<String, Value> = kwargs(&[("validity", json!("day"))]);
    broker.close_all_positions(None, Some(&["exchange", "product"]), Some(&add), None);
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

pub fn test_close_all_positions_quantity_as_string() {
    let broker = Paper::new().with_positions(vec![
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
    broker.close_all_positions(None, None, Some(&add), None);
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

pub fn test_close_all_positions_quantity_as_error() {
    let broker = Paper::new().with_positions(vec![
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
    // Upstream passes `symbol_transformer="string"` — Rust's signature
    // requires `Option<&dyn Fn>`; we pass None because the non-callable
    // branch upstream just falls back to identity. Same observable
    // behaviour: the first two positions round-trip, the non-numeric
    // "O" quantity is skipped.
    let add: HashMap<String, Value> = kwargs(&[("variety", json!("regular"))]);
    broker.close_all_positions(None, None, Some(&add), None);
    let calls = broker.place_calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].get("symbol"), Some(&json!("aapl")));
    assert_eq!(calls[1].get("symbol"), Some(&json!("meta")));
}

pub fn test_close_all_positions_symbol_transfomer() {
    let broker = Paper::new().with_positions(vec![
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
    let transform = |s: &str| format!("nyse:{s}");
    broker.close_all_positions(None, None, Some(&add), Some(&transform));
    let calls = broker.place_calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].get("symbol"), Some(&json!("nyse:aapl")));
    assert_eq!(calls[1].get("symbol"), Some(&json!("nyse:meta")));
}

pub fn test_close_all_positions_given_positions() {
    let broker = Paper::new();
    let positions = vec![kwargs(&[
        ("symbol", json!("aapl")),
        ("quantity", json!(10)),
        ("tag", json!("reg")),
    ])];
    broker.close_all_positions(Some(positions), None, None, None);
    assert_eq!(broker.place_call_count(), 1);
    assert_eq!(
        broker.place_calls()[0].get("symbol"),
        Some(&json!("aapl"))
    );
    assert_eq!(
        broker.place_calls()[0].get("quantity"),
        Some(&json!(10))
    );
    assert_eq!(broker.place_calls()[0].get("side"), Some(&json!("sell")));
}
