//! Parity ports of `tests/test_order.py`. R3.a covers the 55 non-DB items;
//! R3.b will add the 9 SQLite-backed tests (`test_order_create_db*`,
//! `test_order_save_to_db*` that need a real connection, `test_new_db*`).

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use omsrs::clock::{Clock, MockClock};
use omsrs::order::{Order, OrderInit};
use omsrs::Broker;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::{json, Value};

use crate::mock_broker::MockBroker;

/// Tiny helper — equivalent to Python's `dict(k=v, …)` at the call site.
fn kwargs(pairs: &[(&str, Value)]) -> HashMap<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect()
}

fn simple_order_arc(
    clock: Arc<dyn Clock + Send + Sync>,
) -> Order {
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

fn mock_clock(ts: chrono::DateTime<Utc>) -> Arc<dyn Clock + Send + Sync> {
    Arc::new(MockClock::new(ts))
}

fn default_mock_clock() -> Arc<dyn Clock + Send + Sync> {
    mock_clock(Utc.with_ymd_and_hms(2022, 1, 1, 10, 0, 0).unwrap())
}

fn new_order() -> Order {
    Order::from_init_with_clock(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 10,
            ..Default::default()
        },
        default_mock_clock(),
    )
}

// ── simple / property tests ─────────────────────────────────────────────

pub fn test_order_simple() {
    let order = Order::from_init_with_clock(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 10,
            timezone: Some("Europe/Paris".into()),
            ..Default::default()
        },
        default_mock_clock(),
    );
    assert_eq!(order.quantity, 10);
    assert_eq!(order.pending_quantity, Some(10));
    assert_eq!(order.filled_quantity, 0);
    assert!(order.timestamp.is_some());
    assert!(order.id.is_some());
    assert_eq!(order.timezone, "Europe/Paris");
    assert_eq!(order.lock().timezone.as_deref(), Some("Europe/Paris"));
    assert_eq!(
        Order::frozen_attrs(),
        ["symbol", "side"].iter().copied().collect()
    );
}

pub fn test_order_id_custom() {
    let order = Order::from_init_with_clock(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 10,
            id: Some("some_hex_digit".into()),
            ..Default::default()
        },
        default_mock_clock(),
    );
    assert_eq!(order.id.as_deref(), Some("some_hex_digit"));
}

pub fn test_order_is_complete() {
    let mut order = new_order();
    assert!(!order.is_complete());
    order.filled_quantity = 10;
    assert!(order.is_complete());
}

pub fn test_order_is_complete_other_cases() {
    let mut order = new_order();
    order.filled_quantity = 6;
    assert!(!order.is_complete());
    order.cancelled_quantity = 4;
    assert!(order.is_complete());
}

pub fn test_order_is_pending() {
    let mut order = new_order();
    assert!(order.is_pending());
    order.filled_quantity = 10;
    assert!(!order.is_pending());
    order.filled_quantity = 5;
    order.cancelled_quantity = 5;
    assert!(!order.is_pending());
    order.filled_quantity = 5;
    order.cancelled_quantity = 4;
    assert!(order.is_pending());
    order.status = Some("COMPLETE".into());
    assert!(!order.is_pending());
}

pub fn test_order_is_pending_canceled() {
    let mut order = new_order();
    assert!(order.is_pending());
    order.filled_quantity = 5;
    order.cancelled_quantity = 0;
    assert!(order.is_pending());
    order.status = Some("CANCELED".into());
    assert!(!order.is_pending());
}

pub fn test_order_is_pending_rejected() {
    let mut order = new_order();
    assert!(order.is_pending());
    order.status = Some("REJECTED".into());
    assert_eq!(order.filled_quantity, 0);
    assert_eq!(order.cancelled_quantity, 0);
    assert!(!order.is_pending());
}

pub fn test_order_is_done() {
    let mut order = Order::from_init_with_clock(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 10,
            filled_quantity: Some(10),
            ..Default::default()
        },
        default_mock_clock(),
    );
    assert!(order.is_complete());
    assert!(order.is_done());

    order = Order::from_init_with_clock(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 10,
            filled_quantity: Some(5),
            cancelled_quantity: Some(5),
            ..Default::default()
        },
        default_mock_clock(),
    );
    assert!(order.is_done());
}

pub fn test_order_is_done_not_complete() {
    let mut order = new_order();
    assert!(!order.is_done());
    order.status = Some("CANCELED".into());
    assert!(!order.is_complete());
    assert!(!order.is_pending());
    assert!(order.is_done());

    let mut order = new_order();
    order.status = Some("REJECTED".into());
    assert!(!order.is_complete());
    assert!(!order.is_pending());
    assert!(order.is_done());
}

pub fn test_order_has_parent() {
    let order = new_order();
    assert!(!order.has_parent());
    // CompoundOrder path lands at R8; smoke the direct parent_id set here
    // to lock in the predicate semantics upstream uses.
    let order2 = Order::from_init_with_clock(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 10,
            parent_id: Some("compound".into()),
            ..Default::default()
        },
        default_mock_clock(),
    );
    assert!(order2.has_parent());
}

// ── update() tests ──────────────────────────────────────────────────────

pub fn test_order_update_simple() {
    let mut order = new_order();
    order.update(&kwargs(&[
        ("filled_quantity", json!(7)),
        ("average_price", json!("912")),
        ("exchange_order_id", json!("abcd")),
    ]));
    assert_eq!(order.filled_quantity, 7);
    assert_eq!(order.average_price, dec!(912));
    assert_eq!(order.exchange_order_id.as_deref(), Some("abcd"));
}

pub fn test_order_update_timestamp() {
    let base = Utc.with_ymd_and_hms(2021, 1, 1, 12, 0, 0).unwrap();
    let clock = MockClock::new(base);
    let arc: Arc<dyn Clock + Send + Sync> = Arc::new(clock.clone());
    let mut order = Order::from_init_with_clock(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 10,
            timezone: Some("Europe/Paris".into()),
            ..Default::default()
        },
        arc,
    );
    assert_eq!(order.timestamp, Some(base));
    clock.set(base + Duration::minutes(5));
    order.update(&kwargs(&[
        ("filled_quantity", json!(7)),
        ("average_price", json!("912")),
        ("exchange_order_id", json!("abcd")),
    ]));
    assert_eq!(order.last_updated_at, Some(base + Duration::minutes(5)));
    let diff = order.last_updated_at.unwrap() - order.timestamp.unwrap();
    assert_eq!(diff.num_seconds(), 300);
    assert_eq!(order.timestamp, Some(base));
}

pub fn test_order_update_non_attribute() {
    let mut order = new_order();
    order.update(&kwargs(&[
        ("filled_quantity", json!(7)),
        ("average_price", json!("912")),
        ("message", json!("not in attributes")),
    ]));
    assert_eq!(order.filled_quantity, 7);
    // `message` is not an upstream `_attrs` member, so Order has no field
    // for it — effectively `hasattr(order, "message") is False`.
}

pub fn test_order_update_do_not_update_when_complete() {
    let mut order = new_order();
    order.filled_quantity = 7;
    assert_eq!(order.average_price, Decimal::ZERO);
    order.update(&kwargs(&[("average_price", json!("920"))]));
    assert_eq!(order.average_price, dec!(920));
    order.status = Some("COMPLETE".into());
    order.update(&kwargs(&[
        ("average_price", json!("912")),
        ("quantity", json!(10)),
    ]));
    assert_eq!(order.average_price, dec!(920));
    assert_eq!(order.filled_quantity, 7);
    order.filled_quantity = 10;
    assert_eq!(order.filled_quantity, 10);
}

pub fn test_order_update_do_not_update_rejected_order() {
    let mut order = new_order();
    order.filled_quantity = 7;
    order.average_price = dec!(912);
    order.status = Some("REJECTED".into());
    order.update(&kwargs(&[("average_price", json!("920"))]));
    assert_eq!(order.average_price, dec!(912));
}

pub fn test_order_update_do_not_update_cancelled_order() {
    let mut order = new_order();
    order.filled_quantity = 7;
    order.average_price = dec!(912);
    order.status = Some("CANCELED".into());
    order.update(&kwargs(&[("average_price", json!("920"))]));
    assert_eq!(order.average_price, dec!(912));
    order.status = Some("CANCELLED".into());
    order.update(&kwargs(&[("average_price", json!("920"))]));
    assert_eq!(order.average_price, dec!(912));
    order.status = Some("OPEN".into());
    order.update(&kwargs(&[("average_price", json!("920"))]));
    assert_eq!(order.average_price, dec!(920));
}

pub fn test_order_update_do_not_update_timestamp_for_completed_orders() {
    let base = Utc.with_ymd_and_hms(2022, 11, 5, 0, 0, 0).unwrap();
    let clock = MockClock::new(base);
    let arc: Arc<dyn Clock + Send + Sync> = Arc::new(clock.clone());
    let mut order = Order::from_init_with_clock(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 10,
            ..Default::default()
        },
        arc,
    );
    for i in [10_i64, 20, 30, 40] {
        clock.set(base + Duration::seconds(i));
        order.update(&kwargs(&[("filled_quantity", json!(10))]));
        // `filled_quantity==10==quantity` triggers is_complete → the second
        // update onward is rejected, so last_updated_at stays at base+10.
        assert_eq!(order.last_updated_at, Some(base + Duration::seconds(10)));
    }

    // Rejected path: status flipped after each update; subsequent updates
    // are blocked by is_done → last_updated_at stays at base+10.
    clock.set(base);
    let arc2: Arc<dyn Clock + Send + Sync> = Arc::new(clock.clone());
    let mut order = Order::from_init_with_clock(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 10,
            ..Default::default()
        },
        arc2,
    );
    for i in [10_i64, 20, 30, 40] {
        clock.set(base + Duration::seconds(i));
        order.update(&kwargs(&[("filled_quantity", json!(5))]));
        order.status = Some("REJECTED".into());
        assert_eq!(order.last_updated_at, Some(base + Duration::seconds(10)));
    }
}

pub fn test_order_update_pending_quantity() {
    let mut order = new_order();
    assert_eq!(order.pending_quantity, Some(10));
    assert_eq!(order.filled_quantity, 0);
    order.update(&kwargs(&[("filled_quantity", json!(5))]));
    assert_eq!(order.pending_quantity, Some(5));
    assert_eq!(order.filled_quantity, 5);
}

pub fn test_order_update_pending_quantity_in_data() {
    let mut order = new_order();
    assert_eq!(order.pending_quantity, Some(10));
    assert_eq!(order.filled_quantity, 0);
    order.update(&kwargs(&[
        ("filled_quantity", json!(5)),
        ("pending_quantity", json!(2)),
    ]));
    assert_eq!(order.pending_quantity, Some(2));
    assert_eq!(order.filled_quantity, 5);
}

// ── expiry ──────────────────────────────────────────────────────────────

pub fn test_order_expires() {
    // Upstream: known = 2021-01-01 12:00 local. expires_in=0 → seconds to
    // end_of_day (23:59:59) = 12h - 1s = 43199. UTC-normalised: same math.
    let base = Utc.with_ymd_and_hms(2021, 1, 1, 12, 0, 0).unwrap();
    let order = Order::from_init_with_clock(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 10,
            ..Default::default()
        },
        mock_clock(base),
    );
    assert_eq!(order.expires_in, 60 * 60 * 12 - 1);

    let order = Order::from_init_with_clock(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 10,
            expires_in: Some(600),
            ..Default::default()
        },
        default_mock_clock(),
    );
    assert_eq!(order.expires_in, 600);

    let order = Order::from_init_with_clock(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 10,
            expires_in: Some(-600),
            ..Default::default()
        },
        default_mock_clock(),
    );
    assert_eq!(order.expires_in, 600);
}

pub fn test_order_expiry_times() {
    let base = Utc.with_ymd_and_hms(2021, 1, 1, 9, 30, 0).unwrap();
    let clock = MockClock::new(base);
    let arc: Arc<dyn Clock + Send + Sync> = Arc::new(clock.clone());
    let order = Order::from_init_with_clock(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 10,
            expires_in: Some(60),
            ..Default::default()
        },
        arc,
    );
    assert_eq!(order.expires_in, 60);
    assert_eq!(order.time_to_expiry(), 60);
    assert_eq!(order.time_after_expiry(), 0);

    clock.set(base + Duration::seconds(40));
    assert_eq!(order.time_to_expiry(), 20);
    assert_eq!(order.time_after_expiry(), 0);

    clock.set(base + Duration::seconds(100));
    assert_eq!(order.time_to_expiry(), 0);
    assert_eq!(order.time_after_expiry(), 40);
}

pub fn test_order_has_expired() {
    let base = Utc.with_ymd_and_hms(2021, 1, 1, 10, 0, 0).unwrap();
    let clock = MockClock::new(base);
    let arc: Arc<dyn Clock + Send + Sync> = Arc::new(clock.clone());
    let order = Order::from_init_with_clock(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 10,
            expires_in: Some(60),
            ..Default::default()
        },
        arc,
    );
    assert!(!order.has_expired());
    clock.set(base + Duration::seconds(60));
    assert!(order.has_expired());
}

// ── execute() tests ─────────────────────────────────────────────────────

pub fn test_simple_order_execute() {
    let broker = MockBroker::new();
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
    order.execute(&broker, None, HashMap::new());
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

pub fn test_simple_order_execute_kwargs() {
    let broker = MockBroker::new();
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
    order.execute(
        &broker,
        None,
        kwargs(&[("exchange", json!("NSE")), ("variety", json!("regular"))]),
    );
    assert_eq!(broker.place_call_count(), 1);
    let args = &broker.place_calls()[0];
    assert_eq!(args.get("exchange"), Some(&json!("NSE")));
    assert_eq!(args.get("variety"), Some(&json!("regular")));
    assert_eq!(args.get("price"), Some(&json!("650")));
}

pub fn test_simple_order_execute_do_not_update_existing_kwargs() {
    let broker = MockBroker::new();
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
    order.execute(
        &broker,
        None,
        kwargs(&[
            ("exchange", json!("NSE")),
            ("variety", json!("regular")),
            ("quantity", json!(20)),
            ("order_type", json!("MARKET")),
        ]),
    );
    assert_eq!(broker.place_call_count(), 1);
    let args = &broker.place_calls()[0];
    // Upstream preserves the Order's own values for the default keys —
    // kwargs don't overwrite them.
    assert_eq!(args.get("quantity"), Some(&json!(10)));
    assert_eq!(args.get("order_type"), Some(&json!("LIMIT")));
    assert_eq!(args.get("exchange"), Some(&json!("NSE")));
    assert_eq!(args.get("variety"), Some(&json!("regular")));
}

pub fn test_simple_order_do_not_execute_more_than_once() {
    let broker = MockBroker::new();
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
        order.execute(
            &broker,
            None,
            kwargs(&[("exchange", json!("NSE")), ("variety", json!("regular"))]),
        );
    }
    assert_eq!(broker.place_call_count(), 1);
}

pub fn test_simple_order_do_not_execute_completed_order() {
    let broker = MockBroker::new();
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
        order.execute(
            &broker,
            None,
            kwargs(&[("exchange", json!("NSE")), ("variety", json!("regular"))]),
        );
    }
    assert_eq!(broker.place_call_count(), 0);
}

// ── modify() / cancel() ─────────────────────────────────────────────────

pub fn test_simple_order_modify() {
    let broker = MockBroker::new();
    let mut order = simple_order_arc(default_mock_clock());
    order.price = Some(dec!(630));
    order.modify(&broker, None, HashMap::new());
    assert_eq!(broker.modify_call_count(), 1);
    let args = &broker.modify_calls()[0];
    assert_eq!(args.get("order_id"), Some(&json!("abcdef")));
    assert_eq!(args.get("quantity"), Some(&json!(10)));
    assert_eq!(args.get("order_type"), Some(&json!("LIMIT")));
    assert_eq!(args.get("price"), Some(&json!("630")));
    assert_eq!(args.get("trigger_price"), Some(&json!("0")));
    assert_eq!(args.get("disclosed_quantity"), Some(&json!(0)));
}

pub fn test_simple_order_cancel() {
    let broker = MockBroker::new();
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
    order.cancel(&broker, None);
    assert_eq!(broker.cancel_call_count(), 1);
    assert_eq!(broker.cancel_calls()[0].get("order_id"), Some(&json!("abcdef")));
}

pub fn test_simple_order_cancel_none() {
    let broker = MockBroker::new();
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
    order.cancel(&broker, None);
    assert_eq!(broker.cancel_call_count(), 0);
}

// ── modify variants ─────────────────────────────────────────────────────

pub fn test_order_modify_quantity() {
    let broker = MockBroker::new();
    let mut order = simple_order_arc(default_mock_clock());
    order.exchange = Some("NSE".into());
    order.modify(
        &broker,
        None,
        kwargs(&[
            ("price", json!("630")),
            ("quantity", json!(20)),
            ("exchange", json!("NFO")),
        ]),
    );
    assert_eq!(broker.modify_call_count(), 1);
    assert_eq!(order.quantity, 20);
    assert_eq!(order.price, Some(dec!(630)));
}

pub fn test_order_modify_by_attribute() {
    let broker = MockBroker::new();
    let mut order = simple_order_arc(default_mock_clock());
    order.quantity = 100;
    order.price = Some(dec!(600));
    order.modify(&broker, None, kwargs(&[("exchange", json!("NSE"))]));
    assert_eq!(broker.modify_call_count(), 1);
    let args = &broker.modify_calls()[0];
    assert_eq!(args.get("quantity"), Some(&json!(100)));
    assert_eq!(args.get("price"), Some(&json!("600")));
    assert_eq!(args.get("exchange"), Some(&json!("NSE")));
}

pub fn test_order_modify_extra_attributes() {
    let broker = MockBroker::new();
    let mut order = simple_order_arc(default_mock_clock());
    order.modify(
        &broker,
        None,
        kwargs(&[
            ("price", json!("630")),
            ("quantity", json!(20)),
            ("exchange", json!("NFO")),
            ("validity", json!("GFD")),
        ]),
    );
    assert_eq!(broker.modify_call_count(), 1);
    let args = &broker.modify_calls()[0];
    assert_eq!(args.get("quantity"), Some(&json!(20)));
    assert_eq!(args.get("price"), Some(&json!("630")));
    assert_eq!(args.get("validity"), Some(&json!("GFD")));
}

pub fn test_order_modify_frozen() {
    let broker = MockBroker::new();
    let mut order = simple_order_arc(default_mock_clock());
    order.modify(
        &broker,
        None,
        kwargs(&[
            ("price", json!("630")),
            ("quantity", json!(20)),
            ("exchange", json!("NFO")),
            ("validity", json!("GFD")),
            ("symbol", json!("meta")),
            ("tsym", json!("meta")),
        ]),
    );
    let args = &broker.modify_calls()[0];
    assert!(!args.contains_key("symbol"));
    assert_eq!(args.get("tsym"), Some(&json!("meta")));
}

// ── max_modifications ───────────────────────────────────────────────────

pub fn test_order_max_modifications() {
    let broker = MockBroker::new();
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
    order.price = Some(dec!(630));
    order.modify(&broker, None, HashMap::new());
    assert_eq!(order.num_modifications, 1);
    for _ in 0..100 {
        order.modify(&broker, None, HashMap::new());
    }
    assert_eq!(order.num_modifications, 10);
    assert_eq!(order.max_modifications, order.num_modifications);
}

pub fn test_order_max_modifications_change_default() {
    let broker = MockBroker::new();
    let mut order = Order::from_init_with_clock(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 10,
            order_type: Some("LIMIT".into()),
            price: Some(dec!(650)),
            order_id: Some("abcdef".into()),
            max_modifications: Some(3),
            ..Default::default()
        },
        default_mock_clock(),
    );
    order.price = Some(dec!(630));
    for _ in 0..10 {
        order.modify(&broker, None, HashMap::new());
    }
    assert_eq!(order.num_modifications, 3);
    assert_eq!(order.max_modifications, 3);
}

// ── clone ───────────────────────────────────────────────────────────────

pub fn test_order_clone() {
    let order = Order::from_init_with_clock(
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
    let clone = order.clone_fresh();
    assert_ne!(order.id, clone.id);
    assert_eq!(clone.symbol, "aapl");
    assert_eq!(clone.quantity, 10);
    assert_eq!(clone.price, Some(dec!(650)));
    assert_eq!(clone.order_type, "LIMIT");
}

pub fn test_order_clone_new_timestamp() {
    let base = Utc.with_ymd_and_hms(2021, 1, 1, 12, 0, 0).unwrap();
    let clock = MockClock::new(base);
    let arc: Arc<dyn Clock + Send + Sync> = Arc::new(clock.clone());
    let order = Order::from_init_with_clock(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 10,
            ..Default::default()
        },
        arc,
    );
    clock.set(base + Duration::hours(2));
    let clone = order.clone_fresh();
    assert_ne!(order.timestamp, clone.timestamp);
    assert_eq!(clone.timestamp, Some(base + Duration::hours(2)));
}

// ── timezone ────────────────────────────────────────────────────────────

pub fn test_order_timezone() {
    let order = new_order();
    assert_eq!(order.timezone, "local");
    // Upstream additionally asserts
    //   order.timestamp.timezone.name == pendulum.now("local").timezone_name
    // Pendulum's `DateTime` carries a named tz object; our
    // `DateTime<Utc>` does not. Expressing the assertion faithfully
    // requires a tz-aware timestamp type — out of R3 scope.
    //
    // Registered as §14(B) in `tests/parity/excused.toml`; the panic
    // here is the intended signal to the parity gate.
    panic!("§14B: pendulum DateTime.timezone.name parity not portable to chrono DateTime<Utc>");
}

// ── order-lock interaction ──────────────────────────────────────────────

pub fn test_order_lock_no_lock() {
    let base = Utc.with_ymd_and_hms(2022, 1, 1, 10, 10, 0).unwrap();
    let clock = MockClock::new(base);
    let arc: Arc<dyn Clock + Send + Sync> = Arc::new(clock.clone());
    let broker = MockBroker::new();
    let mut order = Order::from_init_with_clock(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 10,
            ..Default::default()
        },
        arc,
    );
    order.execute(&broker, None, HashMap::new());
    for i in 1..=10 {
        clock.set(base + Duration::seconds(i));
        order.modify(&broker, None, HashMap::new());
    }
    assert_eq!(broker.place_call_count(), 1);
    assert_eq!(broker.modify_call_count(), 10);

    for i in 1..=6 {
        clock.set(base + Duration::seconds(i));
        order.cancel(&broker, None);
    }
    assert_eq!(broker.cancel_call_count(), 6);
}

pub fn test_order_lock_modify_and_cancel() {
    let base = Utc.with_ymd_and_hms(2022, 1, 1, 10, 10, 0).unwrap();
    let clock = MockClock::new(base);
    let arc: Arc<dyn Clock + Send + Sync> = Arc::new(clock.clone());
    let broker = MockBroker::new();
    let mut order = Order::from_init_with_clock(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 10,
            ..Default::default()
        },
        arc,
    );
    order.execute(&broker, None, HashMap::new());

    for i in 0..10 {
        clock.set(base + Duration::seconds(i + 1));
        if i == 5 {
            order.add_lock(1, 3.0);
        }
        order.modify(&broker, None, HashMap::new());
    }
    // Upstream expects 6 modify calls total — lock engaged at i==5 and
    // holds for 3s (blocks i==5, i==6, i==7 ⇒ modifies at i==0..4 (5) plus
    // i==8, i==9 (2) = 7? Actually upstream asserts 6. Let's trace:
    //   i=0..4 (5 modifies) before lock engaged at i==5.
    //   at i==5: add_lock(1, 3) sets modification_lock_till = now+3s = base+6+3=base+9. Then modify() → can_modify = (base+6 > base+9)? false → skip.
    //   i=6: now=base+7 < base+9 → blocked
    //   i=7: now=base+8 < base+9 → blocked
    //   i=8: now=base+9 == base+9 → can_modify = (now > lock_till)? false → blocked
    //   i=9: now=base+10 > base+9 → can_modify → modify.
    //   Total = 5 + 1 = 6. ✓
    assert_eq!(broker.modify_call_count(), 6);

    for i in 0..6 {
        clock.set(base);
        order.add_lock(2, 10.0);
        clock.set(base + Duration::seconds(i + 1));
        order.cancel(&broker, None);
    }
    assert_eq!(broker.cancel_call_count(), 0);
}

pub fn test_order_lock_cancel() {
    let base = Utc.with_ymd_and_hms(2022, 1, 1, 10, 10, 0).unwrap();
    let clock = MockClock::new(base);
    let arc: Arc<dyn Clock + Send + Sync> = Arc::new(clock.clone());
    let broker = MockBroker::new();
    let mut order = Order::from_init_with_clock(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 10,
            ..Default::default()
        },
        arc,
    );
    order.execute(&broker, None, HashMap::new());

    for i in 0..10 {
        clock.set(base + Duration::seconds(i + 1));
        if i % 2 == 0 {
            order.cancel(&broker, None);
        }
        if i % 6 == 0 {
            order.add_lock(2, 4.0);
        }
    }
    // Upstream expects 2 cancels. Trace:
    //   i=0: t=base+1, cancel (no prior lock — succeeds). then add_lock(2,4): cancel_lock_till=base+1+4=base+5.
    //   i=1: no cancel call in the loop.
    //   i=2: t=base+3, cancel. can_cancel=(base+3 > base+5)? false → blocked.
    //   i=3: no cancel.
    //   i=4: t=base+5, cancel. can_cancel=(base+5 > base+5)? false → blocked.
    //   i=5: no cancel.
    //   i=6: t=base+7, cancel. can_cancel=(base+7 > base+5)? true → cancel(#2). then add_lock(2,4): lock_till=base+7+4=base+11.
    //   i=7: no cancel.
    //   i=8: t=base+9, cancel. can_cancel=(base+9 > base+11)? false → blocked.
    //   i=9: no cancel.
    //   Total = 2 cancels. ✓
    assert_eq!(broker.cancel_call_count(), 2);
}

// ── attribs_to_copy coverage ────────────────────────────────────────────

pub fn test_order_modify_args_to_add() {
    let broker = MockBroker::new();
    let mut order = simple_order_arc(default_mock_clock());
    order.client_id = Some("abcd1234".into());
    order.exchange = Some("nyse".into());
    let attribs = ["client_id"];
    order.modify(
        &broker,
        Some(&attribs),
        kwargs(&[("price", json!("600"))]),
    );
    assert_eq!(broker.modify_call_count(), 1);
    assert_eq!(order.price, Some(dec!(600)));
    let args = &broker.modify_calls()[0];
    let expected: HashMap<String, Value> = [
        ("order_id", json!("abcdef")),
        ("quantity", json!(10)),
        ("price", json!("600")),
        ("trigger_price", json!("0")),
        ("order_type", json!("LIMIT")),
        ("disclosed_quantity", json!(0)),
        ("client_id", json!("abcd1234")),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_string(), v.clone()))
    .collect();
    assert_eq!(args, &expected);
}

pub fn test_order_modify_args_to_add_no_args() {
    let broker = MockBroker::new();
    let mut order = simple_order_arc(default_mock_clock());
    order.client_id = Some("abcd1234".into());
    order.exchange = Some("nyse".into());
    let attribs = ["transform", "segment"];
    order.modify(
        &broker,
        Some(&attribs),
        kwargs(&[("price", json!("600"))]),
    );
    let args = &broker.modify_calls()[0];
    let expected: HashMap<String, Value> = [
        ("order_id", json!("abcdef")),
        ("quantity", json!(10)),
        ("price", json!("600")),
        ("trigger_price", json!("0")),
        ("order_type", json!("LIMIT")),
        ("disclosed_quantity", json!(0)),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_string(), v.clone()))
    .collect();
    assert_eq!(args, &expected);
}

pub fn test_order_modify_args_to_add_override() {
    let broker = MockBroker::new();
    let mut order = simple_order_arc(default_mock_clock());
    let attribs = ["exchange"];
    order.modify(
        &broker,
        Some(&attribs),
        kwargs(&[("price", json!("600")), ("exchange", json!("nasdaq"))]),
    );
    let args = &broker.modify_calls()[0];
    let expected: HashMap<String, Value> = [
        ("order_id", json!("abcdef")),
        ("quantity", json!(10)),
        ("price", json!("600")),
        ("trigger_price", json!("0")),
        ("order_type", json!("LIMIT")),
        ("disclosed_quantity", json!(0)),
        ("exchange", json!("nasdaq")),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_string(), v.clone()))
    .collect();
    assert_eq!(args, &expected);
}

pub fn test_order_modify_args_dont_modify_frozen() {
    let broker = MockBroker::new();
    let mut order = simple_order_arc(default_mock_clock());
    let attribs = ["symbol", "side"];
    order.modify(
        &broker,
        Some(&attribs),
        kwargs(&[("price", json!("600"))]),
    );
    let args = &broker.modify_calls()[0];
    let expected: HashMap<String, Value> = [
        ("order_id", json!("abcdef")),
        ("quantity", json!(10)),
        ("price", json!("600")),
        ("trigger_price", json!("0")),
        ("order_type", json!("LIMIT")),
        ("disclosed_quantity", json!(0)),
        ("symbol", json!("aapl")),
        ("side", json!("buy")),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_string(), v.clone()))
    .collect();
    assert_eq!(args, &expected);

    order.modify(
        &broker,
        Some(&attribs),
        kwargs(&[
            ("price", json!("600")),
            ("symbol", json!("goog")),
            ("side", json!("sell")),
        ]),
    );
    let args = &broker.modify_calls()[1];
    assert_eq!(args, &expected);
}

pub fn test_order_execute_attribs_to_copy() {
    let broker = MockBroker::new();
    broker.set_place_side_effect((100000..100010).map(|v| Some(v.to_string())).collect());
    let mut order = Order::from_init_with_clock(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 10,
            order_type: Some("LIMIT".into()),
            price: Some(dec!(650)),
            exchange: Some("nyse".into()),
            ..Default::default()
        },
        default_mock_clock(),
    );
    let attribs = ["exchange"];
    order.execute(&broker, Some(&attribs), HashMap::new());
    assert_eq!(broker.place_call_count(), 1);
    let args = &broker.place_calls()[0];
    let expected: HashMap<String, Value> = [
        ("symbol", json!("AAPL")),
        ("side", json!("BUY")),
        ("quantity", json!(10)),
        ("order_type", json!("LIMIT")),
        ("price", json!("650")),
        ("trigger_price", json!("0")),
        ("disclosed_quantity", json!(0)),
        ("exchange", json!("nyse")),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_string(), v.clone()))
    .collect();
    assert_eq!(args, &expected);
}

pub fn test_order_execute_attribs_to_copy_broker() {
    let broker = MockBroker::new();
    broker.set_attribs_to_copy_execute(Some(vec!["exchange".into(), "client_id".into()]));
    let mut order = simple_order_arc(default_mock_clock());
    order.order_id = None;
    order.exchange = Some("nyse".into());
    order.execute(&broker, None, HashMap::new());
    let args = &broker.place_calls()[0];
    let expected: HashMap<String, Value> = [
        ("symbol", json!("AAPL")),
        ("side", json!("BUY")),
        ("quantity", json!(10)),
        ("order_type", json!("LIMIT")),
        ("price", json!("650")),
        ("trigger_price", json!("0")),
        ("disclosed_quantity", json!(0)),
        ("exchange", json!("nyse")),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_string(), v.clone()))
    .collect();
    assert_eq!(args, &expected);
}

pub fn test_order_execute_attribs_to_copy_broker2() {
    let broker = MockBroker::new();
    broker.set_attribs_to_copy_execute(Some(vec!["exchange".into(), "client_id".into()]));
    let mut order = simple_order_arc(default_mock_clock());
    order.order_id = None;
    order.exchange = Some("nyse".into());
    order.client_id = Some("abcd1234".into());
    order.execute(&broker, None, HashMap::new());
    let args = &broker.place_calls()[0];
    let expected: HashMap<String, Value> = [
        ("symbol", json!("AAPL")),
        ("side", json!("BUY")),
        ("quantity", json!(10)),
        ("order_type", json!("LIMIT")),
        ("price", json!("650")),
        ("trigger_price", json!("0")),
        ("disclosed_quantity", json!(0)),
        ("exchange", json!("nyse")),
        ("client_id", json!("abcd1234")),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_string(), v.clone()))
    .collect();
    assert_eq!(args, &expected);
}

pub fn test_order_execute_attribs_to_copy_override() {
    // Tests kwargs-override-other_args precedence: exchange + client_id
    // live on the Order AND the attribs_to_copy set (via broker's default
    // nothing — set explicitly here), AND in kwargs. Upstream kwargs win.
    let broker = MockBroker::new();
    broker.set_attribs_to_copy_execute(Some(vec!["exchange".into(), "client_id".into()]));
    let mut order = simple_order_arc(default_mock_clock());
    order.order_id = None;
    order.exchange = Some("nyse".into());
    order.client_id = Some("abcd1234".into());
    order.execute(
        &broker,
        None,
        kwargs(&[
            ("exchange", json!("nasdaq")),
            ("client_id", json!("xyz12345")),
        ]),
    );
    let args = &broker.place_calls()[0];
    let expected: HashMap<String, Value> = [
        ("symbol", json!("AAPL")),
        ("side", json!("BUY")),
        ("quantity", json!(10)),
        ("order_type", json!("LIMIT")),
        ("price", json!("650")),
        ("trigger_price", json!("0")),
        ("disclosed_quantity", json!(0)),
        ("exchange", json!("nasdaq")),
        ("client_id", json!("xyz12345")),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_string(), v.clone()))
    .collect();
    assert_eq!(args, &expected);
}


pub fn test_get_other_args_from_attribs() {
    let broker = MockBroker::new();
    broker.set_attribs_to_copy_execute(Some(vec!["exchange".into(), "client_id".into()]));
    let mut order = simple_order_arc(default_mock_clock());
    order.exchange = Some("nyse".into());
    order.client_id = Some("abcd1234".into());
    let args = order.get_other_args_from_attribs(broker.attribs_to_copy_execute(), None);
    let expected: HashMap<String, Value> = [
        ("exchange", json!("nyse")),
        ("client_id", json!("abcd1234")),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_string(), v.clone()))
    .collect();
    assert_eq!(args, expected);
}

pub fn test_order_modify_attribs_to_copy_broker() {
    let broker = MockBroker::new();
    broker.set_attribs_to_copy_modify(Some(vec!["exchange".into(), "client_id".into()]));
    let mut order = simple_order_arc(default_mock_clock());
    order.exchange = Some("nyse".into());
    order.client_id = Some("abcd1234".into());
    order.modify(&broker, None, kwargs(&[("price", json!("700"))]));
    let args = &broker.modify_calls()[0];
    let expected: HashMap<String, Value> = [
        ("order_id", json!("abcdef")),
        ("quantity", json!(10)),
        ("price", json!("700")),
        ("trigger_price", json!("0")),
        ("order_type", json!("LIMIT")),
        ("disclosed_quantity", json!(0)),
        ("exchange", json!("nyse")),
        ("client_id", json!("abcd1234")),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_string(), v.clone()))
    .collect();
    assert_eq!(args, &expected);
}

pub fn test_order_cancel_attribs_to_copy_broker() {
    let broker = MockBroker::new();
    broker.set_attribs_to_copy_cancel(Some(vec!["exchange".into(), "client_id".into()]));
    let mut order = simple_order_arc(default_mock_clock());
    order.exchange = Some("nyse".into());
    order.client_id = Some("abcd1234".into());
    order.cancel(&broker, None);
    let args = &broker.cancel_calls()[0];
    let expected: HashMap<String, Value> = [
        ("order_id", json!("abcdef")),
        ("exchange", json!("nyse")),
        ("client_id", json!("abcd1234")),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_string(), v.clone()))
    .collect();
    assert_eq!(args, &expected);
}

// ── misc persistence edge cases (no real connection) ────────────────────

pub fn test_order_do_not_save_to_db_if_no_connection() {
    let order = new_order();
    assert!(!order.save_to_db());
}

pub fn test_order_save_to_db_dont_update_order_no_connection() {
    let mut order = new_order();
    for _ in 0..3 {
        order.update(&kwargs(&[
            ("filled_quantity", json!(7)),
            ("average_price", json!("780")),
        ]));
    }
    assert!(!order.save_to_db());
}

// ── R3.b: SQLite-backed tests ───────────────────────────────────────────

use omsrs::persistence::SqlitePersistenceHandle;
use omsrs::PersistenceError;

fn new_db() -> Arc<SqlitePersistenceHandle> {
    Arc::new(SqlitePersistenceHandle::in_memory().expect("sqlite in-memory"))
}

fn order_with_conn(
    symbol: &str,
    quantity: i64,
    con: Arc<SqlitePersistenceHandle>,
) -> Order {
    let handle: Arc<dyn omsrs::PersistenceHandle> = con.clone();
    Order::from_init_with_clock(
        OrderInit {
            symbol: symbol.into(),
            side: "buy".into(),
            quantity,
            timezone: Some("Europe/Paris".into()),
            connection: Some(handle),
            ..Default::default()
        },
        default_mock_clock(),
    )
}

pub fn test_order_create_db() {
    let _order = Order::from_init_with_clock(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 10,
            timezone: Some("Europe/Paris".into()),
            ..Default::default()
        },
        default_mock_clock(),
    );
    let con = new_db();
    for i in 0..10 {
        con.insert_raw(kwargs(&[
            ("symbol", json!("aapl")),
            ("quantity", json!(i)),
            ("id", json!(format!("id-{i}"))),
        ]))
        .expect("insert");
    }
    assert_eq!(con.count().unwrap(), 10);
}

pub fn test_order_create_db_primary_key_duplicate_error() {
    let order = Order::from_init_with_clock(
        OrderInit {
            symbol: "aapl".into(),
            side: "buy".into(),
            quantity: 10,
            timezone: Some("Europe/Paris".into()),
            id: Some("primary_id".into()),
            ..Default::default()
        },
        default_mock_clock(),
    );
    let con = new_db();
    let id = order.id.clone().unwrap();
    let row = kwargs(&[
        ("symbol", json!("aapl")),
        ("quantity", json!(0)),
        ("id", json!(id.clone())),
    ]);
    con.insert_raw(row.clone()).expect("first insert ok");
    match con.insert_raw(row.clone()) {
        Err(PersistenceError::Unique(_)) => {} // expected
        other => panic!("expected Unique error, got {other:?}"),
    }
}

pub fn test_order_save_to_db() {
    let con = new_db();
    let order = order_with_conn("aapl", 10, con.clone());
    assert!(order.save_to_db());
    assert_eq!(con.count().unwrap(), 1);
    let rows = con.query_all().unwrap();
    assert_eq!(rows[0].get("symbol"), Some(&json!("aapl")));
}

pub fn test_order_save_to_db_update() {
    let con = new_db();
    let mut order = order_with_conn("aapl", 10, con.clone());
    order.save_to_db();
    for i in 1..8 {
        order.filled_quantity = i;
        order.save_to_db();
    }
    assert_eq!(con.count().unwrap(), 1);
    let rows = con.query_all().unwrap();
    assert_eq!(rows[0].get("filled_quantity"), Some(&json!(7)));
}

pub fn test_order_save_to_db_multiple_orders() {
    let con = new_db();
    let order1 = order_with_conn("aapl", 10, con.clone());
    let handle: Arc<dyn omsrs::PersistenceHandle> = con.clone();
    let order2 = Order::from_init_with_clock(
        OrderInit {
            symbol: "goog".into(),
            side: "sell".into(),
            quantity: 20,
            timezone: Some("Europe/Paris".into()),
            connection: Some(handle),
            tag: Some("short".into()),
            ..Default::default()
        },
        default_mock_clock(),
    );
    order1.save_to_db();
    order2.save_to_db();
    assert_eq!(con.count().unwrap(), 2);
    for _ in 0..10 {
        order1.save_to_db();
        order2.save_to_db();
    }
    // Still 2 rows — upsert by id.
    assert_eq!(con.count().unwrap(), 2);
    for row in con.query_all().unwrap() {
        match row.get("symbol").and_then(|v| v.as_str()) {
            Some("aapl") => {
                assert_eq!(row.get("quantity"), Some(&json!(10)));
                assert_eq!(row.get("tag"), Some(&json!(null)));
            }
            Some("goog") => {
                assert_eq!(row.get("tag"), Some(&json!("short")));
            }
            _ => panic!("unexpected symbol"),
        }
    }
}

pub fn test_order_save_to_db_update_order() {
    let con = new_db();
    let mut order = order_with_conn("aapl", 10, con.clone());
    for _ in 0..3 {
        order.update(&kwargs(&[
            ("filled_quantity", json!(7)),
            ("average_price", json!("780")),
        ]));
    }
    assert_eq!(con.count().unwrap(), 1);
    let rows = con.query_all().unwrap();
    assert_eq!(rows[0].get("filled_quantity"), Some(&json!(7)));
    assert_eq!(rows[0].get("average_price"), Some(&json!(780.0)));
}

pub fn test_new_db() {
    let con = new_db();
    let handle: Arc<dyn omsrs::PersistenceHandle> = con.clone();
    let order = Order::from_init_with_clock(
        OrderInit {
            symbol: "amzn".into(),
            side: "sell".into(),
            quantity: 10,
            connection: Some(handle),
            ..Default::default()
        },
        default_mock_clock(),
    );
    order.save_to_db();
    // New-column presence check, upstream keys:
    let keys = ["can_peg", "strategy_id", "portfolio_id", "pseudo_id", "JSON", "error"];
    for row in con.query_all().unwrap() {
        for k in keys {
            assert!(row.contains_key(k), "missing column {k}");
        }
    }
}

pub fn test_new_db_with_values() {
    let con = new_db();
    let handle: Arc<dyn omsrs::PersistenceHandle> = con.clone();
    let mut json_val: HashMap<String, Value> = HashMap::new();
    json_val.insert("a".into(), json!(10));
    json_val.insert("b".into(), json!([4, 5, 6]));
    let order = Order::from_init_with_clock(
        OrderInit {
            symbol: "amzn".into(),
            side: "sell".into(),
            quantity: 10,
            connection: Some(handle),
            json: Some(json_val.clone()),
            pseudo_id: Some("hex_pseudo_id".into()),
            error: Some("some_error_message".into()),
            tag: Some("this is a tag".into()),
            ..Default::default()
        },
        default_mock_clock(),
    );
    order.save_to_db();

    let expected_json =
        serde_json::to_string(&serde_json::json!({"a": 10, "b": [4, 5, 6]})).unwrap();

    for row in con.query_all().unwrap() {
        assert_eq!(row.get("can_peg"), Some(&json!(1)));
        assert_eq!(row.get("JSON"), Some(&json!(expected_json)));
        assert_eq!(row.get("tag"), Some(&json!("this is a tag")));
        assert_eq!(row.get("is_multi"), Some(&json!(0)));
        assert_eq!(row.get("last_updated_at"), Some(&json!(null)));

        let retrieved = Order::from_row(&row);
        assert!(retrieved.can_peg);
        assert_eq!(retrieved.json, Some(json_val.clone()));
        assert_eq!(retrieved.pseudo_id.as_deref(), Some("hex_pseudo_id"));
    }
}

pub fn test_new_db_all_values() {
    let con = new_db();
    let handle: Arc<dyn omsrs::PersistenceHandle> = con.clone();
    let mut json_val: HashMap<String, Value> = HashMap::new();
    json_val.insert("a".into(), json!(10));
    json_val.insert("b".into(), json!([4, 5, 6]));
    let order = Order::from_init_with_clock(
        OrderInit {
            symbol: "amzn".into(),
            side: "sell".into(),
            quantity: 10,
            connection: Some(handle),
            json: Some(json_val.clone()),
            pseudo_id: Some("hex_pseudo_id".into()),
            error: Some("some_error_message".into()),
            timezone: Some("Asia/Kolkata".into()),
            ..Default::default()
        },
        default_mock_clock(),
    );
    order.save_to_db();

    for row in con.query_all().unwrap() {
        let retrieved = Order::from_row(&row);
        // Field-by-field equality (upstream asserts model_dump, minus
        // `connection` — our `connection` is serde(skip) so both sides
        // have it `None` after reconstruction).
        assert_eq!(retrieved.symbol, order.symbol);
        assert_eq!(retrieved.side, order.side);
        assert_eq!(retrieved.quantity, order.quantity);
        assert_eq!(retrieved.id, order.id);
        assert_eq!(retrieved.order_type, order.order_type);
        assert_eq!(retrieved.price, order.price);
        assert_eq!(retrieved.trigger_price, order.trigger_price);
        assert_eq!(retrieved.average_price, order.average_price);
        assert_eq!(retrieved.filled_quantity, order.filled_quantity);
        assert_eq!(retrieved.cancelled_quantity, order.cancelled_quantity);
        assert_eq!(retrieved.disclosed_quantity, order.disclosed_quantity);
        assert_eq!(retrieved.validity, order.validity);
        assert_eq!(retrieved.expires_in, order.expires_in);
        assert_eq!(retrieved.timezone, order.timezone);
        assert_eq!(retrieved.max_modifications, order.max_modifications);
        assert_eq!(retrieved.retries, order.retries);
        assert_eq!(retrieved.can_peg, order.can_peg);
        assert_eq!(retrieved.is_multi, order.is_multi);
        assert_eq!(retrieved.pseudo_id, order.pseudo_id);
        assert_eq!(retrieved.error, order.error);
        assert_eq!(retrieved.json, order.json);
        assert_eq!(retrieved.cancel_after_expiry, order.cancel_after_expiry);
        assert_eq!(
            retrieved.convert_to_market_after_expiry,
            order.convert_to_market_after_expiry
        );
    }
}
