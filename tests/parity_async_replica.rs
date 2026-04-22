//! R12.2 — async mirror of `tests/parity/test_replica_broker.rs`.
//!
//! Core contract: `Arc::ptr_eq` identity across `orders()` /
//! `pending()` / `completed()` / `fills()` / `user_orders(user)`
//! still compares handle identity on the async side.
//!
//! Pinned instrument prices match sync fixture (upstream Python
//! `random.seed(1000)` output replayed literally so R7 expected
//! avg_prices don't drift).

use std::collections::HashMap;
use std::sync::Arc;

use omsrs::async_broker::AsyncBroker;
use omsrs::async_replica_broker::AsyncReplicaBroker;
use omsrs::replica_broker::OrderHandle;
use omsrs::simulation::{Instrument, Status};
use serde_json::{json, Value};

// ── fixtures (mirror sync) ────────────────────────────────

fn kwargs(pairs: &[(&str, Value)]) -> HashMap<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect()
}

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

fn replica_with_instruments() -> AsyncReplicaBroker {
    let b = AsyncReplicaBroker::new();
    b.update(vec![
        instrument("AAPL", 125.0),
        instrument("XOM", 153.0),
        instrument("DOW", 136.0),
    ]);
    b
}

async fn replica_with_orders() -> (AsyncReplicaBroker, Vec<OrderHandle>) {
    let b = replica_with_instruments();
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
        let h = b
            .place(kwargs(&[
                ("symbol", json!(sym)),
                ("side", json!(side)),
                ("quantity", json!(qty)),
                ("order_type", json!(ot)),
                ("price", json!(price)),
                ("user", json!(user)),
            ]))
            .await;
        handles.push(h);
    }
    (b, handles)
}

// ── R12.2.async_replica.1 — defaults
#[tokio::test]
async fn defaults_match_sync() {
    let b = AsyncReplicaBroker::new();
    assert_eq!(b.name(), "replica");
    assert!(b.instruments().is_empty());
    assert!(b.orders().is_empty());
    assert_eq!(b.users().len(), 1);
    assert!(b.users().contains("default"));
    assert!(b.pending().is_empty());
    assert!(b.completed().is_empty());
    assert!(b.fills().is_empty());
}

// ── R12.2.async_replica.2 — update instruments
#[tokio::test]
async fn update_instruments_inserts_and_overwrites() {
    let b = AsyncReplicaBroker::new();
    let names = ["AAPL", "XOM", "DOW"];
    let instruments: Vec<Instrument> = names.iter().map(|n| instrument(n, 120.0)).collect();
    b.update(instruments);
    let snapshot = b.instruments();
    for name in names {
        assert!(snapshot.contains_key(name));
    }
    // Update existing instrument
    let mut updated = snapshot["AAPL"].clone();
    updated.last_price = 144.0;
    b.update(vec![updated]);
    assert_eq!(b.instruments()["AAPL"].last_price, 144.0);
}

// ── R12.2.async_replica.3 — order_place: shared identity across
// all 5 collections (the sync test's load-bearing assertion)
#[tokio::test]
async fn order_place_preserves_shared_identity() {
    let b = replica_with_instruments();
    let order = b
        .place(kwargs(&[
            ("symbol", json!("AAPL")),
            ("side", json!(1)),
            ("quantity", json!(10)),
        ]))
        .await;
    let order_id = order.lock().order_id.clone();

    let orders = b.orders();
    assert!(orders.contains_key(&order_id));
    let user_orders_default = b.user_orders("default").unwrap();
    assert_eq!(user_orders_default.len(), 1);
    assert!(!order.lock().is_done());
    let pending = b.pending();
    assert_eq!(pending.len(), 1);
    let fills = b.fills();
    assert_eq!(fills.len(), 1);
    assert_eq!(pending[0].lock().order_id, order_id);

    // Arc::ptr_eq across every collection — the single load-bearing
    // invariant sync `test_replica_broker_order_place` checks.
    assert!(Arc::ptr_eq(&order, &orders[&order_id]));
    assert!(Arc::ptr_eq(&order, &user_orders_default[0]));
    assert!(Arc::ptr_eq(&order, &pending[0]));
    assert!(Arc::ptr_eq(&order, &fills[0].order));
}

// ── R12.2.async_replica.4 — multi-user routing
#[tokio::test]
async fn order_place_routes_to_correct_user_bucket() {
    let b = replica_with_instruments();
    b.place(kwargs(&[
        ("symbol", json!("AAPL")),
        ("side", json!(1)),
        ("quantity", json!(10)),
    ]))
    .await;
    for user in ["user1", "user2", "default"] {
        b.place(kwargs(&[
            ("symbol", json!("AAPL")),
            ("side", json!(1)),
            ("quantity", json!(10)),
            ("user", json!(user)),
        ]))
        .await;
    }
    b.place(kwargs(&[
        ("symbol", json!("AAPL")),
        ("side", json!(1)),
        ("quantity", json!(10)),
    ]))
    .await;

    assert_eq!(b.orders().len(), 5);
    // 3 distinct users: default (3 orders), user1 (1), user2 (1).
    assert_eq!(b.user_orders("default").unwrap().len(), 3);
    assert_eq!(b.user_orders("user1").unwrap().len(), 1);
    assert_eq!(b.user_orders("user2").unwrap().len(), 1);
}

// ── R12.2.async_replica.5 — run_fill: expected avg_prices
#[tokio::test]
async fn run_fill_applies_orderfill_rules() {
    let (b, handles) = replica_with_orders().await;
    assert_eq!(b.orders().len(), 10);
    assert_eq!(b.completed().len(), 0);
    assert_eq!(b.user_orders("user1").unwrap().len(), 2);
    assert_eq!(b.user_orders("user2").unwrap().len(), 3);
    assert_eq!(b.user_orders("default").unwrap().len(), 5);

    // Upstream expected map: fills at indices (0, 3, 4, 5, 7) fill with
    // avg_prices (125, 136, 136, 153, 153). Indices 1,2,6,8,9 don't fill.
    let fills_before = b.fills();
    let expected_avgs: HashMap<String, f64> = [
        (fills_before[0].order.lock().order_id.clone(), 125.0),
        (fills_before[3].order.lock().order_id.clone(), 136.0),
        (fills_before[4].order.lock().order_id.clone(), 136.0),
        (fills_before[5].order.lock().order_id.clone(), 153.0),
        (fills_before[7].order.lock().order_id.clone(), 153.0),
    ]
    .into_iter()
    .collect();
    drop(fills_before);

    b.run_fill().await;
    assert_eq!(b.completed().len(), 5);
    assert_eq!(b.fills().len(), 5);
    for handle in &b.completed() {
        let o = handle.lock();
        let expected = expected_avgs[&o.order_id];
        assert_eq!(o.average_price, Some(expected));
    }

    // AAPL SELL LIMIT 10 @ 126 (handles[1]) should fill at avg=126
    // after AAPL last_price bumps to 127.
    let mut updated = b.instruments()["AAPL"].clone();
    updated.last_price = 127.0;
    b.update(vec![updated]);
    b.run_fill().await;
    assert_eq!(b.fills().len(), 4);
    assert_eq!(b.completed().len(), 6);
    {
        let o = handles[1].lock();
        assert_eq!(o.average_price, Some(126.0));
        assert_eq!(o.filled_quantity, 10.0);
    }

    // Idempotent — repeated run_fill without price changes doesn't alter.
    for _ in 0..10 {
        b.run_fill().await;
    }
    assert_eq!(b.fills().len(), 4);
    assert_eq!(b.completed().len(), 6);

    // AAPL last_price = 121.95 triggers AAPL BUY LIMIT @ 123 and 122
    // plus AAPL BUY LIMIT 20 @ 124 (handles[2]). 3 more fills.
    let mut updated = b.instruments()["AAPL"].clone();
    updated.last_price = 121.95;
    b.update(vec![updated]);
    b.run_fill().await;
    assert_eq!(b.fills().len(), 1);
    assert_eq!(b.completed().len(), 9);
    let id6 = handles[6].lock().order_id.clone();
    assert!(b.orders().contains_key(&id6));
}

// ── R12.2.async_replica.6 — order_modify reprices + requalifies
#[tokio::test]
async fn order_modify_reprices_and_refills() {
    let b = replica_with_instruments();
    let order = b
        .place(kwargs(&[
            ("symbol", json!("AAPL")),
            ("side", json!(1)),
            ("quantity", json!(10)),
            ("order_type", json!(2)),
            ("price", json!(124)),
        ]))
        .await;
    let order_id = order.lock().order_id.clone();
    b.run_fill().await;
    assert!(!order.lock().is_done());
    b.modify(kwargs(&[
        ("order_id", json!(order_id.clone())),
        ("quantity", json!(20)),
        ("price", json!(125.1)),
    ]))
    .await;
    let orders = b.orders();
    assert_eq!(orders[&order_id].lock().quantity, 20.0);
    assert_eq!(orders[&order_id].lock().price, Some(125.1));
    b.run_fill().await;
    let o = order.lock();
    assert_eq!(o.filled_quantity, 20.0);
    assert_eq!(o.average_price, Some(125.1));
    assert!(o.is_done());
}

// ── R12.2.async_replica.7 — modify to MARKET triggers fill at
// instrument last_price
#[tokio::test]
async fn order_modify_to_market_fills_at_last_price() {
    let b = replica_with_instruments();
    let order = b
        .place(kwargs(&[
            ("symbol", json!("AAPL")),
            ("side", json!(1)),
            ("quantity", json!(10)),
            ("order_type", json!(2)),
            ("price", json!(124)),
        ]))
        .await;
    let order_id = order.lock().order_id.clone();
    b.run_fill().await;
    assert!(!order.lock().is_done());
    b.modify(kwargs(&[
        ("order_id", json!(order_id.clone())),
        ("quantity", json!(20)),
    ]))
    .await;
    b.run_fill().await;
    assert!(!order.lock().is_done());
    b.modify(kwargs(&[
        ("order_id", json!(order_id.clone())),
        ("order_type", json!(1)),
    ]))
    .await;
    b.run_fill().await;
    let o = order.lock();
    assert_eq!(o.filled_quantity, 20.0);
    assert_eq!(o.average_price, Some(125.0));
    assert!(o.is_done());
    drop(o);

    assert!(Arc::ptr_eq(&order, &b.orders()[&order_id]));
    assert!(Arc::ptr_eq(&order, &b.user_orders("default").unwrap()[0]));
    assert_eq!(b.completed().len(), 1);
}

// ── R12.2.async_replica.8 — order_cancel
#[tokio::test]
async fn order_cancel_marks_done_and_trims_fills() {
    let b = replica_with_instruments();
    let order = b
        .place(kwargs(&[
            ("symbol", json!("AAPL")),
            ("side", json!(1)),
            ("quantity", json!(10)),
            ("order_type", json!(2)),
            ("price", json!(124)),
        ]))
        .await;
    let order_id = order.lock().order_id.clone();
    b.run_fill().await;
    assert!(!order.lock().is_done());
    b.cancel(kwargs(&[("order_id", json!(order_id))])).await;
    assert_eq!(b.completed().len(), 1);
    assert!(order.lock().is_done());
    assert_eq!(b.fills().len(), 1);
    b.run_fill().await;
    assert_eq!(b.fills().len(), 0);
}

// ── R12.2.async_replica.9 — cancel is idempotent
#[tokio::test]
async fn order_cancel_is_idempotent() {
    let b = replica_with_instruments();
    let order = b
        .place(kwargs(&[
            ("symbol", json!("AAPL")),
            ("side", json!(1)),
            ("quantity", json!(10)),
            ("order_type", json!(2)),
            ("price", json!(124)),
        ]))
        .await;
    let order_id = order.lock().order_id.clone();
    b.run_fill().await;
    assert!(!order.lock().is_done());
    for _ in 0..10 {
        b.cancel(kwargs(&[("order_id", json!(order_id.clone()))]))
            .await;
    }
    assert_eq!(b.completed().len(), 1);
    assert!(order.lock().is_done());
    assert_eq!(b.fills().len(), 1);
    b.run_fill().await;
    assert_eq!(b.fills().len(), 0);
}

// ── R12.2.async_replica.10 — unknown symbol → REJECTED
#[tokio::test]
async fn unknown_symbol_results_in_rejected_order() {
    let b = replica_with_instruments();
    let order1 = b
        .place(kwargs(&[
            ("symbol", json!("AAPL")),
            ("side", json!(1)),
            ("quantity", json!(10)),
        ]))
        .await;
    assert_eq!(b.fills().len(), 1);
    {
        let o = order1.lock();
        assert_eq!(o.pending_quantity, 10.0);
        assert_eq!(o.quantity, o.pending_quantity);
        assert_eq!(o.status(), Status::Open);
        assert!(!o.is_done());
    }

    let order2 = b
        .place(kwargs(&[
            ("symbol", json!("yinyang")),
            ("side", json!(1)),
            ("quantity", json!(10)),
        ]))
        .await;
    assert_eq!(b.fills().len(), 1);
    assert_eq!(b.pending().len(), 1);
    assert_eq!(b.completed().len(), 1);
    let o = order2.lock();
    assert!(!o.is_complete());
    assert!(o.is_done());
    assert_eq!(o.filled_quantity, 0.0);
    assert_eq!(o.pending_quantity, 0.0);
    assert_eq!(o.canceled_quantity, 10.0);
    assert_eq!(o.status(), Status::Rejected);
}

// ── R12.2.async_replica.12 — R12.2 audit closeout: lock-order
// discipline. Exercises the exact ABBA path codex flagged — an
// external caller holds `handle.lock()` while a concurrent task
// runs `cancel` / `run_fill` / accessors. The new lock discipline
// (never hold inner while taking handle lock) means this must
// NOT deadlock. Uses `timeout` so a regression (re-introducing
// inner-held-while-handle-locked) shows up as a test hang-then-
// panic rather than a silent stall.
#[tokio::test]
async fn external_handle_hold_does_not_deadlock_cancel_or_accessors() {
    use std::time::Duration;
    use tokio::time::timeout;

    let b = Arc::new(replica_with_instruments());

    let h = b
        .place(kwargs(&[
            ("symbol", json!("AAPL")),
            ("side", json!(1)),
            ("quantity", json!(10)),
            ("order_type", json!(2)),
            ("price", json!(124)),
        ]))
        .await;
    let order_id = h.lock().order_id.clone();

    // Spawn a task that holds `handle.lock()` for ~80ms.
    let h_for_task = h.clone();
    let hold_task = tokio::task::spawn_blocking(move || {
        let _g = h_for_task.lock();
        std::thread::sleep(Duration::from_millis(80));
    });

    // Let the spawn_blocking task get on-CPU and grab the handle
    // lock before we race into cancel / accessors.
    tokio::time::sleep(Duration::from_millis(10)).await;

    let b2 = b.clone();
    let order_id_clone = order_id.clone();
    // Both of these would deadlock under the pre-closeout impl:
    // cancel would hold inner → wait for handle lock, while
    // hold_task holds handle → `orders()` inside the assert
    // would wait for inner. Now: cancel takes inner briefly to
    // clone the handle, drops inner, then awaits handle; the
    // accessor never waits on a handle so both make progress.
    let cancel_fut = async move {
        b2.cancel(kwargs(&[("order_id", json!(order_id_clone))]))
            .await;
    };
    let b3 = b.clone();
    let accessor_fut = async move {
        // While hold_task has the handle locked, reading
        // `orders()` should succeed without blocking.
        let _orders = b3.orders();
        let _fills = b3.fills();
    };
    // 500ms is >>80ms handle-hold + 10ms spawn race; anything
    // that fails to complete is a genuine hang.
    timeout(Duration::from_millis(500), async {
        tokio::join!(cancel_fut, accessor_fut)
    })
    .await
    .expect("cancel + accessors must not deadlock while an external task holds the handle");

    hold_task.await.expect("hold task completes");

    // Post-condition: cancel did eventually mark the order done.
    assert!(h.lock().is_done());
}

// ── R12.2.async_replica.11 — AsyncBroker trait adapter smoke
#[tokio::test]
async fn trait_adapter_returns_some_on_place() {
    let b = replica_with_instruments();
    let broker: &dyn AsyncBroker = &b;
    let oid = broker
        .order_place(kwargs(&[
            ("symbol", json!("AAPL")),
            ("side", json!(1)),
            ("quantity", json!(10)),
        ]))
        .await;
    let oid = oid.expect("place should return Some");
    assert!(b.orders().contains_key(&oid));

    // Unknown symbol: sync `order_place` still returns a handle
    // (REJECTED state); the lossy trait adapter mirrors that by
    // returning Some(order_id) because the handle exists. This
    // matches what the sync adapter would do if it were wired
    // to the trait.
    let oid2 = broker
        .order_place(kwargs(&[
            ("symbol", json!("nope")),
            ("side", json!(1)),
            ("quantity", json!(10)),
        ]))
        .await;
    assert!(oid2.is_some());
}
