//! Parity ports of `tests/test_models.py`. R1 shipped the 3
//! `test_basic_position*` items; R2 adds 10 more (1 Quote-ish + 3 OrderBook
//! + 6 OrderLock incl. 3-case parametrize).

use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use omsrs::clock::{Clock, MockClock};
use omsrs::models::{BasicPosition, OrderBook, OrderLock, Quote};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

pub fn test_basic_position() {
    let p = BasicPosition::new("AAPL");
    assert_eq!(p.symbol, "AAPL");
}

pub fn test_basic_position_calculations() {
    let p = BasicPosition {
        symbol: "AAPL".into(),
        buy_quantity: dec!(100),
        sell_quantity: dec!(120),
        buy_value: dec!(100) * dec!(131),
        sell_value: dec!(120) * dec!(118.5),
    };
    assert_eq!(p.net_quantity(), Decimal::from(-20));
    assert_eq!(p.average_buy_value(), dec!(131));
    assert_eq!(p.average_sell_value(), dec!(118.5));
}

pub fn test_basic_position_zero_quantity() {
    let mut p = BasicPosition::new("AAPL");
    assert_eq!(p.average_buy_value(), Decimal::ZERO);
    assert_eq!(p.average_sell_value(), Decimal::ZERO);
    p.buy_quantity = dec!(10);
    assert_eq!(p.average_buy_value(), Decimal::ZERO);
    p.buy_value = dec!(1315);
    assert_eq!(p.average_buy_value(), dec!(131.5));
    assert_eq!(p.average_sell_value(), Decimal::ZERO);
}

// ── R2: Quote / OrderBook ───────────────────────────────────────────────

pub fn test_order_book() {
    let bids = vec![
        Quote::new(dec!(120), 4),
        Quote::with_orders_count(dec!(121), 20, 2),
    ];
    let asks = vec![Quote::new(dec!(119), 7), Quote::new(dec!(118), 28)];
    let ob = OrderBook::new(bids, asks);
    assert_eq!(ob.bid[0].quantity, 4);
    assert_eq!(ob.bid[0].orders_count, None);
    assert_eq!(ob.bid.last().unwrap().orders_count, Some(2));
    assert_eq!(ob.ask[1].quantity, 28);
    assert_eq!(ob.ask.last().unwrap().value(), dec!(118) * Decimal::from(28));
}

fn sample_orderbook() -> OrderBook {
    let bids = vec![
        Quote::with_orders_count(dec!(6466), 3, 3),
        Quote::with_orders_count(dec!(6465), 29, 19),
        Quote::with_orders_count(dec!(6464), 43, 33),
        Quote::with_orders_count(dec!(6463), 19, 12),
        Quote::with_orders_count(dec!(6462), 11, 8),
    ];
    let asks = vec![
        Quote::with_orders_count(dec!(6468), 4, 4),
        Quote::with_orders_count(dec!(6469), 17, 3),
        Quote::with_orders_count(dec!(6470), 6, 3),
        Quote::with_orders_count(dec!(6471), 13, 11),
        Quote::with_orders_count(dec!(6472), 43, 20),
    ];
    OrderBook::new(bids, asks)
}

pub fn test_orderbook_is_bid_ask() {
    let mut ob = OrderBook::new(vec![], vec![]);
    assert!(!ob.is_bid_ask());
    ob.bid.push(Quote::with_orders_count(dec!(100), 10, 175));
    assert!(!ob.is_bid_ask());
    ob.ask.push(Quote::with_orders_count(dec!(100), 10, 175));
    assert!(ob.is_bid_ask());
}

pub fn test_orderbook_spread() {
    let ob = sample_orderbook();
    assert_eq!(ob.spread(), Decimal::from(2));
    let empty = OrderBook::new(vec![], vec![]);
    assert_eq!(empty.spread(), Decimal::ZERO);
}

pub fn test_orderbook_total_bid_ask_quantity() {
    let ob = sample_orderbook();
    assert_eq!(ob.total_bid_quantity(), 105);
    assert_eq!(ob.total_ask_quantity(), 83);
    let empty = OrderBook::new(vec![], vec![]);
    assert_eq!(empty.total_bid_quantity(), 0);
    assert_eq!(empty.total_ask_quantity(), 0);
}

// ── R2: OrderLock ───────────────────────────────────────────────────────

/// Upstream tests use `pendulum.travel_to(known, freeze=True)`. We inject a
/// `MockClock` at construction so `clock.now()` returns a deterministic UTC
/// instant. Timezone info on `OrderLock` is carried but doesn't affect
/// arithmetic — pendulum's equality is instant-based and so is chrono's.
fn known() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2022, 1, 1, 10, 10, 15).unwrap()
}

pub fn test_order_lock_defaults() {
    let clock = MockClock::new(known());
    let arc: Arc<dyn Clock + Send + Sync> = Arc::new(clock.clone());
    let lock = OrderLock::with_clock(arc);
    assert_eq!(lock.creation_lock_till(), known());
    assert_eq!(lock.modification_lock_till(), known());
    assert_eq!(lock.cancellation_lock_till(), known());
}

pub fn test_order_lock_methods() {
    let clock = MockClock::new(known());
    let arc: Arc<dyn Clock + Send + Sync> = Arc::new(clock.clone());
    let mut lock = OrderLock::with_clock(arc);

    // create(20): known + 20s
    lock.create(20.0);
    assert_eq!(lock.creation_lock_till(), known() + Duration::seconds(20));

    // modify(60): known + 60s = 10:11:15
    lock.modify(60.0);
    assert_eq!(
        lock.modification_lock_till(),
        Utc.with_ymd_and_hms(2022, 1, 1, 10, 11, 15).unwrap()
    );

    // cancel(15): known + 15s = 10:10:30
    lock.cancel(15.0);
    assert_eq!(
        lock.cancellation_lock_till(),
        Utc.with_ymd_and_hms(2022, 1, 1, 10, 10, 30).unwrap()
    );
}

pub fn test_order_lock_methods_max_duration() {
    let clock = MockClock::new(known());
    let arc: Arc<dyn Clock + Send + Sync> = Arc::new(clock.clone());
    let mut lock = OrderLock::with_clock(arc);

    // max = 60 (default). create(90) → capped at 60s → 10:11:15.
    lock.create(90.0);
    assert_eq!(
        lock.creation_lock_till(),
        Utc.with_ymd_and_hms(2022, 1, 1, 10, 11, 15).unwrap()
    );

    // Bump max to 120. create(90) → uncapped 90s → 10:11:45.
    lock.max_order_creation_lock_time = 120.0;
    lock.create(90.0);
    assert_eq!(
        lock.creation_lock_till(),
        Utc.with_ymd_and_hms(2022, 1, 1, 10, 11, 45).unwrap()
    );
}

/// Upstream parametrizes over `("can_create", "can_modify", "can_cancel")`
/// and uses `getattr(lock, method)` + `getattr(lock, method[4:])(10)` to
/// drive each lock method symmetrically. Rust can't do attribute indirection
/// at runtime; we encode the same flow via closures, one trial per method.
enum LockKind {
    Create,
    Modify,
    Cancel,
}

fn run_can_methods(kind: LockKind) {
    let clock = MockClock::new(known());
    let arc: Arc<dyn Clock + Send + Sync> = Arc::new(clock.clone());
    let mut lock = OrderLock::with_clock(arc);

    let can = |l: &OrderLock| match kind {
        LockKind::Create => l.can_create(),
        LockKind::Modify => l.can_modify(),
        LockKind::Cancel => l.can_cancel(),
    };
    let do_lock = |l: &mut OrderLock, s: f64| match kind {
        LockKind::Create => {
            l.create(s);
        }
        LockKind::Modify => {
            l.modify(s);
        }
        LockKind::Cancel => {
            l.cancel(s);
        }
    };

    // At `known`, now == lock_till → can_* is False (strict greater).
    assert!(!can(&lock), "at known, can_* must be False");

    // Advance 1s → now > lock_till → True. Then lock for 10s → False.
    clock.set(known() + Duration::seconds(1));
    assert!(can(&lock));
    do_lock(&mut lock, 10.0);
    assert!(!can(&lock));

    // Advance to known+12s → past the 10s lock (known+1+10 = known+11) → True.
    clock.set(known() + Duration::seconds(12));
    assert!(can(&lock));
}

pub fn test_order_lock_can_methods_can_create() {
    run_can_methods(LockKind::Create);
}

pub fn test_order_lock_can_methods_can_modify() {
    run_can_methods(LockKind::Modify);
}

pub fn test_order_lock_can_methods_can_cancel() {
    run_can_methods(LockKind::Cancel);
}
