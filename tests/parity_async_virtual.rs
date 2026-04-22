//! R12.1 — async mirror of `tests/parity/test_virtual_broker.rs`.
//!
//! Same fixtures, same assertions, driven through `AsyncVirtualBroker`
//! under `#[tokio::test]`. Core contract: an async broker built with
//! the same clock + same seed as a sync broker produces identical
//! reply sequences (modulo the uuid-based `order_id`).

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{TimeZone, Utc};
use omsrs::async_broker::AsyncBroker;
use omsrs::async_virtual_broker::AsyncVirtualBroker;
use omsrs::clock::{Clock, MockClock};
use omsrs::simulation::{ResponseStatus, Side, Status, Ticker, VUser};
use omsrs::virtual_broker::{BrokerReply, VirtualBroker};
use serde_json::{json, Value};

// ─────────────────────────────────────────────────────────────
// Fixtures — mirror of sync parity helpers.
// ─────────────────────────────────────────────────────────────

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

fn mock_clock() -> Arc<dyn Clock + Send + Sync> {
    Arc::new(MockClock::new(
        Utc.with_ymd_and_hms(2023, 2, 1, 10, 17, 0).unwrap(),
    ))
}

fn basic_broker() -> AsyncVirtualBroker {
    AsyncVirtualBroker::with_clock(mock_clock()).with_tickers(basic_tickers())
}

fn basic_broker_with_users() -> AsyncVirtualBroker {
    let b = basic_broker();
    b.set_failure_rate(0.0).unwrap();
    b.add_user(VUser::new("abcd1234"));
    b.add_user(VUser::new("xyz456"));
    b.add_user(VUser::new("bond007"));
    b
}

// ─────────────────────────────────────────────────────────────
// R12.1.async_virtual.1 — defaults
// ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn defaults_match_sync() {
    let b = basic_broker();
    assert_eq!(b.name(), "VBroker");
    assert_eq!(b.failure_rate(), 0.001);
    assert!(b.orders().is_empty());
    assert!(b.clients().is_empty());
}

// ─────────────────────────────────────────────────────────────
// R12.1.async_virtual.2 — is_failure RNG sequence parity
//
// AsyncVirtualBroker and VirtualBroker built with the same
// seed must emit the same boolean sequence.
// ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn is_failure_rng_sequence_matches_sync() {
    // Use failure_rate=0.5 so the boolean output is informative
    // (always-false at 0.001 would pass trivially).
    let sync_b = {
        let mut b = VirtualBroker::with_clock_and_seed(mock_clock(), 42);
        b.set_failure_rate(0.5).unwrap();
        b
    };
    let async_b = {
        let b = AsyncVirtualBroker::with_clock_and_seed(mock_clock(), 42);
        b.set_failure_rate(0.5).unwrap();
        b
    };

    let sync_seq: Vec<bool> = (0..20).map(|_| sync_b.is_failure()).collect();
    let async_seq: Vec<bool> = (0..20).map(|_| async_b.is_failure()).collect();
    assert_eq!(sync_seq, async_seq, "RNG sequence must match bit-for-bit");
}

// ─────────────────────────────────────────────────────────────
// R12.1.async_virtual.3 — order_place success
// ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn order_place_success() {
    let b = basic_broker();
    b.set_failure_rate(0.0).unwrap();
    let known = Utc.with_ymd_and_hms(2023, 2, 1, 10, 17, 0).unwrap();
    let reply = b
        .place(kwargs(&[
            ("symbol", json!("aapl")),
            ("quantity", json!(10)),
            ("side", json!(1)),
        ]))
        .await;
    let resp = reply.as_order().unwrap();
    assert_eq!(resp.status, ResponseStatus::Success);
    assert_eq!(resp.timestamp, Some(known));
    assert!(!resp.data.as_ref().unwrap().order_id.is_empty());
    assert_eq!(b.orders().len(), 1);
}

// ─────────────────────────────────────────────────────────────
// R12.1.async_virtual.4 — order_place field round-trip
// ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn order_place_success_fields() {
    let b = basic_broker();
    b.set_failure_rate(0.0).unwrap();
    let reply = b
        .place(kwargs(&[
            ("symbol", json!("aapl")),
            ("quantity", json!(10)),
            ("side", json!(1)),
            ("price", json!(100)),
            ("trigger_price", json!(99)),
        ]))
        .await;
    let d = reply.as_order().unwrap().data.as_ref().unwrap();
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

// ─────────────────────────────────────────────────────────────
// R12.1.async_virtual.5 — order_place failure
// ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn order_place_failure_when_rate_is_one() {
    let b = basic_broker();
    b.set_failure_rate(1.0).unwrap();
    let reply = b
        .place(kwargs(&[
            ("symbol", json!("aapl")),
            ("quantity", json!(10)),
            ("side", json!(1)),
            ("price", json!(100)),
        ]))
        .await;
    let resp = reply.as_order().unwrap();
    assert_eq!(resp.status, ResponseStatus::Failure);
    assert!(resp.data.is_none());
}

// ─────────────────────────────────────────────────────────────
// R12.1.async_virtual.6 — passthrough kwarg short-circuits
// ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn order_place_passthrough_response() {
    let b = basic_broker();
    b.set_failure_rate(1.0).unwrap();
    let reply = b
        .place(kwargs(&[(
            "response",
            json!({"symbol": "aapl", "price": 100}),
        )]))
        .await;
    let pass = reply.as_passthrough().unwrap();
    assert_eq!(pass, &json!({"symbol": "aapl", "price": 100}));
}

// ─────────────────────────────────────────────────────────────
// R12.1.async_virtual.7 — validation errors match sync format
// ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn order_place_validation_errors() {
    let b = basic_broker();
    b.set_failure_rate(0.0).unwrap();
    // All 3 required fields missing.
    let reply = b.place(HashMap::new()).await;
    let resp = reply.as_order().unwrap();
    assert_eq!(resp.status, ResponseStatus::Failure);
    let msg = resp.error_msg.as_ref().unwrap();
    assert!(msg.starts_with("Found 3 validation"), "got: {msg}");
    assert!(resp.data.is_none());

    // Only quantity missing.
    let reply = b
        .place(kwargs(&[("symbol", json!("aapl")), ("side", json!(-1))]))
        .await;
    let resp = reply.as_order().unwrap();
    assert_eq!(resp.status, ResponseStatus::Failure);
    let msg = resp.error_msg.as_ref().unwrap();
    assert!(msg.starts_with("Found 1 validation"));
    assert!(msg.contains("quantity"));
}

// ─────────────────────────────────────────────────────────────
// R12.1.async_virtual.8 — get + get_default
// ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn get_default_returns_target_order() {
    let b = basic_broker();
    b.set_failure_rate(0.0).unwrap();
    let mut ids: Vec<String> = Vec::new();
    for q in [50_i64, 100, 130] {
        let reply = b
            .place(kwargs(&[
                ("symbol", json!("dow")),
                ("side", json!(1)),
                ("quantity", json!(q)),
            ]))
            .await;
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

// ─────────────────────────────────────────────────────────────
// R12.1.async_virtual.9 — order_modify happy path
// ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn order_modify_success() {
    let b = basic_broker();
    b.set_failure_rate(0.0).unwrap();
    let reply = b
        .place(kwargs(&[
            ("symbol", json!("dow")),
            ("side", json!(1)),
            ("quantity", json!(50)),
        ]))
        .await;
    let oid = reply
        .as_order()
        .unwrap()
        .data
        .as_ref()
        .unwrap()
        .order_id
        .clone();

    let resp = b
        .modify(kwargs(&[
            ("order_id", json!(oid.clone())),
            ("quantity", json!(25)),
        ]))
        .await;
    assert_eq!(resp.as_order().unwrap().status, ResponseStatus::Success);
    assert_eq!(
        resp.as_order().unwrap().data.as_ref().unwrap().quantity,
        25.0
    );

    let resp = b
        .modify(kwargs(&[
            ("order_id", json!(oid.clone())),
            ("price", json!(1000)),
        ]))
        .await;
    assert_eq!(resp.as_order().unwrap().status, ResponseStatus::Success);
    assert_eq!(
        resp.as_order().unwrap().data.as_ref().unwrap().price,
        Some(1000.0)
    );
}

// ─────────────────────────────────────────────────────────────
// R12.1.async_virtual.10 — modify error paths
// ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn order_modify_failure_unknown_oid_and_failure_rate() {
    let b = basic_broker();
    b.set_failure_rate(0.0).unwrap();
    let reply = b
        .place(kwargs(&[
            ("symbol", json!("dow")),
            ("side", json!(1)),
            ("quantity", json!(50)),
        ]))
        .await;
    let oid = reply
        .as_order()
        .unwrap()
        .data
        .as_ref()
        .unwrap()
        .order_id
        .clone();

    // Unknown order id → failure.
    let resp = b
        .modify(kwargs(&[
            ("order_id", json!("hexid")),
            ("quantity", json!(25)),
        ]))
        .await;
    assert_eq!(resp.as_order().unwrap().status, ResponseStatus::Failure);

    // Flip failure rate to 1.0 → is_failure path wins before
    // order lookup.
    b.set_failure_rate(1.0).unwrap();
    let resp = b
        .modify(kwargs(&[
            ("order_id", json!(oid)),
            ("price", json!(100)),
        ]))
        .await;
    assert_eq!(resp.as_order().unwrap().status, ResponseStatus::Failure);
}

// ─────────────────────────────────────────────────────────────
// R12.1.async_virtual.11 — modify response passthrough
// ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn order_modify_kwargs_response_passthrough() {
    let b = basic_broker();
    let resp = b
        .modify(kwargs(&[
            ("order_id", json!("hexid")),
            ("quantity", json!(25)),
            ("response", json!({"a": 10, "b": 15})),
        ]))
        .await;
    assert_eq!(resp.as_passthrough().unwrap(), &json!({"a": 10, "b": 15}));
}

// ─────────────────────────────────────────────────────────────
// R12.1.async_virtual.12 — cancel happy path
// ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn order_cancel_success() {
    let b = basic_broker();
    b.set_failure_rate(0.0).unwrap();
    let reply = b
        .place(kwargs(&[
            ("symbol", json!("dow")),
            ("side", json!(1)),
            ("quantity", json!(50)),
        ]))
        .await;
    let oid = reply
        .as_order()
        .unwrap()
        .data
        .as_ref()
        .unwrap()
        .order_id
        .clone();

    let resp = b
        .cancel(kwargs(&[("order_id", json!(oid))]))
        .await;
    let r = resp.as_order().unwrap();
    assert_eq!(r.status, ResponseStatus::Success);
    let d = r.data.as_ref().unwrap();
    assert_eq!(d.canceled_quantity, 50.0);
    assert_eq!(d.filled_quantity, 0.0);
    assert_eq!(d.pending_quantity, 0.0);
    assert_eq!(d.status(), Status::Canceled);
}

// ─────────────────────────────────────────────────────────────
// R12.1.async_virtual.13 — cancel passthrough
// ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn order_cancel_kwargs_response_passthrough() {
    let b = basic_broker();
    let resp = b
        .cancel(kwargs(&[
            ("order_id", json!("hexid")),
            ("quantity", json!(25)),
            ("response", json!({"a": 10, "b": 15})),
        ]))
        .await;
    assert_eq!(resp.as_passthrough().unwrap(), &json!({"a": 10, "b": 15}));
}

// ─────────────────────────────────────────────────────────────
// R12.1.async_virtual.14 — add_user + de-dup
// ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn add_user_is_idempotent_on_duplicate() {
    let b = AsyncVirtualBroker::with_clock(mock_clock());
    assert_eq!(b.clients().len(), 0);
    assert!(b.add_user(VUser::new("abcd1234")));
    assert!(b.add_user(VUser::new("xyz456")));
    assert_eq!(b.clients().len(), 2);
    assert!(!b.add_user(VUser::new("abcd1234")));
    assert_eq!(b.clients().len(), 2);
}

// ─────────────────────────────────────────────────────────────
// R12.1.async_virtual.15 — order_place attaches to matching user
// ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn order_place_attaches_to_userid_when_registered() {
    let b = basic_broker_with_users();
    let uid = "ABCD1234".to_string();
    let reply = b
        .place(kwargs(&[
            ("symbol", json!("aapl")),
            ("side", json!(1)),
            ("quantity", json!(10)),
            ("userid", json!(uid.clone())),
        ]))
        .await;
    assert_eq!(
        reply.as_order().unwrap().status,
        ResponseStatus::Success
    );
    assert_eq!(b.orders().len(), 1);
    // User attach path wired (the clients set is unchanged;
    // orders show up as the user's own via the sync API).
}

// ─────────────────────────────────────────────────────────────
// R12.1.async_virtual.16 — update_tickers + ltp + ltp_many + ohlc
// ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn ticker_accessors_work() {
    let b = basic_broker();
    let mut prices = HashMap::new();
    prices.insert("aapl".into(), 105.0);
    prices.insert("goog".into(), 121.0);
    b.update_tickers(&prices);

    // `ltp()` in the default Random ticker mode perturbs the
    // stored price by `Z * ltp * 0.01` before returning, so
    // we check shape + symbol presence rather than exact
    // equality (sync parity test `test_virtual_broker_ltp`
    // does the same — `tests/parity/test_virtual_broker.rs:
    // 546-553` only asserts `is_none()` for untracked
    // symbols and the map length).
    assert!(b.ltp("aapl").is_some());
    assert!(b.ltp("dow").is_none());

    let many = b.ltp_many(&["goog", "amzn", "dow", "aa"]);
    assert_eq!(many.len(), 2);

    let ohlc = b.ohlc("aapl").unwrap();
    assert!(ohlc.contains_key("aapl"));
    // Ohlc.open is the original seed (100.0 for aapl) — this
    // is unaffected by ltp() perturbation.
    assert!((ohlc["aapl"].open - 100.0).abs() < 1e-9);
}

// ─────────────────────────────────────────────────────────────
// R12.1.async_virtual.17 — AsyncBroker trait adapter
//
// Confirms the lossy trait path: Option<String> on success, None
// on failure + on passthrough + on validation error. The inherent
// `place` returns the rich reply; this is the trait-object surface
// pbot's event loop uses.
// ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn trait_adapter_loses_rich_reply_but_preserves_semantics() {
    let b = basic_broker();
    b.set_failure_rate(0.0).unwrap();
    let broker: &dyn AsyncBroker = &b;

    // Success → Some(order_id).
    let oid = broker
        .order_place(kwargs(&[
            ("symbol", json!("aapl")),
            ("quantity", json!(10)),
            ("side", json!(1)),
        ]))
        .await;
    let oid = oid.expect("success path should return Some");
    assert!(!oid.is_empty());

    // Validation failure → None.
    let none_1 = broker.order_place(HashMap::new()).await;
    assert!(none_1.is_none());

    // Passthrough → None (no order_id on a passthrough reply).
    let none_2 = broker
        .order_place(kwargs(&[("response", json!({}))]))
        .await;
    assert!(none_2.is_none());
}

// ─────────────────────────────────────────────────────────────
// R12.1.async_virtual.18 — hash parity across sync and async
//
// Drive sync + async brokers with the same seed, same ops, same
// mock clock; compare the canonical string representation of the
// reply sequence. order_id is uuid-generated so we mask it out
// before hashing.
// ─────────────────────────────────────────────────────────────

fn canonicalize(reply: &BrokerReply) -> String {
    match reply {
        BrokerReply::Passthrough(v) => format!("PT:{}", v),
        BrokerReply::Order(resp) => {
            let data_stub = resp.data.as_ref().map(|d| {
                format!(
                    "{}|q={}|fq={}|cq={}|pq={}|price={:?}|trig={:?}",
                    d.symbol,
                    d.quantity,
                    d.filled_quantity,
                    d.canceled_quantity,
                    d.pending_quantity,
                    d.price,
                    d.trigger_price,
                )
            });
            format!(
                "OR:{:?}|err={:?}|data={:?}",
                resp.status, resp.error_msg, data_stub
            )
        }
    }
}

#[tokio::test]
async fn sync_async_reply_sequence_parity() {
    // Shared op list — no randomness inputs beyond the seed.
    let ops: Vec<HashMap<String, Value>> = vec![
        kwargs(&[
            ("symbol", json!("aapl")),
            ("quantity", json!(10)),
            ("side", json!(1)),
        ]),
        kwargs(&[
            ("symbol", json!("goog")),
            ("quantity", json!(5)),
            ("side", json!(-1)),
            ("price", json!(100)),
        ]),
        HashMap::new(), // validation failure
        kwargs(&[("response", json!({"echo": true}))]), // passthrough
        kwargs(&[
            ("symbol", json!("amzn")),
            ("quantity", json!(3)),
            ("side", json!(1)),
            ("trigger_price", json!(255)),
        ]),
    ];

    let seed = 7;
    let sync_seq: Vec<String> = {
        let mut b = VirtualBroker::with_clock_and_seed(mock_clock(), seed);
        b.set_failure_rate(0.5).unwrap(); // mix of success/failure
        ops.iter()
            .map(|op| canonicalize(&b.order_place(op.clone())))
            .collect()
    };

    let async_seq: Vec<String> = {
        let b = AsyncVirtualBroker::with_clock_and_seed(mock_clock(), seed);
        b.set_failure_rate(0.5).unwrap();
        let mut out = Vec::with_capacity(ops.len());
        for op in &ops {
            out.push(canonicalize(&b.place(op.clone()).await));
        }
        out
    };

    assert_eq!(
        sync_seq, async_seq,
        "sync and async reply sequences must match with same seed + same ops"
    );
}
